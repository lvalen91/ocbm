import Foundation
import OSLog
import os
import Synchronization

/// Persistent, file-backed mirror of the unified log for this process.
///
/// Unified logging keeps info/debug entries in an in-memory buffer that rotates under
/// pressure — a long CarPlay session can lose the first minute of history before the
/// user gets to export. This logger runs a background task that polls `OSLogStore`
/// every ~2 s and appends new `com.carlink.*` entries to a text file under
/// `~/Library/Logs/Carlink/`. The file:
///
///   • survives the app quitting (the export path later reads from here),
///   • survives an app crash up to the last fsynced batch,
///   • is visible in Console.app under "Log Reports" for the current user,
///   • can be retrieved after the fact for any session the user forgot to export.
///
/// The app uses `os.Logger` exactly as before — this class just listens.
final class FileLogger: Sendable {

    private static let logger = Logger(subsystem: "com.carlink.app", category: "FileLogger")

    static let shared = FileLogger()

    /// Directory holding all session log files.
    static let logsDirectory: URL = {
        let home = FileManager.default.homeDirectoryForCurrentUser
        return home.appendingPathComponent("Library/Logs/Carlink", isDirectory: true)
    }()

    private let state = Mutex<State>(State())

    /// Serializes drainOnce and the stop-time close. The poll task and
    /// flushSync (export, stop) can run concurrently; without this they both
    /// snapshot the same lastDate/recent, write the same batch twice, and
    /// last-writer-wins on the dedup state.
    private let drainLock = Mutex<Void>(())

    private struct State {
        var fileURL: URL?
        var fileHandle: FileHandle?
        var pollTask: Task<Void, Never>?
        var lastDate: Date = .distantPast
        var running: Bool = false
        /// Hashes of recently written entries, used to dedup in case OSLogStore
        /// returns an entry we've already seen between polls. Sliding-window bounded.
        var recent: Set<Int> = []
        /// Insertion order of `recent`, so the cap evicts the oldest hashes —
        /// Set iteration order is arbitrary and could evict the newest ones
        /// (the only ones the re-query overlap window needs).
        var recentOrder: [Int] = []
    }

    private init() {}

    // MARK: - Lifecycle

    /// Create today's log file, start the polling task.
    func start() {
        let shouldStart = state.withLock { s -> Bool in
            guard !s.running else { return false }
            s.running = true
            return true
        }
        guard shouldStart else { return }

        do {
            try FileManager.default.createDirectory(at: Self.logsDirectory, withIntermediateDirectories: true)
        } catch {
            Self.logger.error("Failed to create logs dir: \(error.localizedDescription, privacy: .public)")
            // Release the claim so a later start() can retry — otherwise one
            // transient failure disables file logging for the process lifetime.
            state.withLock { $0.running = false }
            return
        }

        pruneOldLogs()

        let url = Self.logsDirectory.appendingPathComponent(Self.fileName())
        guard FileManager.default.createFile(atPath: url.path, contents: nil),
              let handle = FileHandle(forWritingAtPath: url.path) else {
            Self.logger.error("Failed to open log file at \(url.path, privacy: .public)")
            state.withLock { $0.running = false }
            return
        }

        let header = Self.preamble()
        try? handle.write(contentsOf: Data(header.utf8))
        try? handle.synchronize()

        // Start from app launch (-15 min buffer — unified log may have entries from
        // immediately before OSLogStore opened that we still want to capture).
        let startDate = Date().addingTimeInterval(-900)

        let task = Task.detached(priority: .utility) { [weak self] in
            guard let self else { return }
            await self.pollLoop()
        }

        // Install the handle and task, but re-check `running` in the SAME locked step: a stop()
        // that landed during the long setup above (createDirectory/createFile/open) already
        // flipped running=false and saw a nil handle/task, so if we install unconditionally we
        // leak an open FileHandle and an orphaned poll task that writes for the process lifetime.
        let aborted = state.withLock { s -> Bool in
            guard s.running else { return true }
            s.fileURL = url
            s.fileHandle = handle
            s.lastDate = startDate
            s.pollTask = task
            return false
        }
        if aborted {
            task.cancel()
            try? handle.close()
            Self.logger.info("FileLogger start aborted — stop() raced setup; handle closed")
            return
        }

        Self.logger.info("FileLogger started: \(url.path, privacy: .public)")
    }

    /// Flush any pending entries, close the file, cancel the polling task.
    func stop() {
        let (task, handle) = state.withLock { s -> (Task<Void, Never>?, FileHandle?) in
            guard s.running else { return (nil, nil) }
            s.running = false
            let t = s.pollTask
            let h = s.fileHandle
            s.pollTask = nil
            return (t, h)
        }
        task?.cancel()
        // One more synchronous drain so final entries land on disk. Closing
        // under drainLock waits out any drain already in flight on the poll
        // task, so nothing writes to a closed handle.
        flushSync()
        drainLock.withLock { _ in
            try? handle?.synchronize()
            try? handle?.close()
            state.withLock { s in
                s.fileHandle = nil
                s.fileURL = nil
            }
        }
    }

    /// Current log file (nil before start or after stop).
    var currentLogURL: URL? {
        state.withLock { $0.fileURL }
    }

    /// Force a synchronous poll-and-write so callers (e.g. exportLog) get
    /// up-to-date content. drainOnce fsyncs after writing under drainLock;
    /// an extra synchronize out here would race stop()'s close.
    func flushSync() {
        drainOnce()
    }

    // MARK: - Polling

    private func pollLoop() async {
        while !Task.isCancelled {
            drainOnce()
            try? await Task.sleep(for: .seconds(2))
        }
    }

    private func drainOnce() {
        // The whole read-format-write-update sequence is one critical section;
        // see drainLock. (A fresh OSLogStore per drain is required — the store
        // is a snapshot at creation time and never sees newer entries.)
        drainLock.withLock { _ in
            guard let store = try? OSLogStore(scope: .currentProcessIdentifier) else { return }

            let snapshot = state.withLock { s -> (Date, FileHandle?, Set<Int>, [Int]) in
                (s.lastDate, s.fileHandle, s.recent, s.recentOrder)
            }
            let (since, handle, seenSnapshot, orderSnapshot) = snapshot
            guard let handle else { return }

            // Re-query from a short window before the last seen entry. OSLogStore.position(date:)
            // has sub-millisecond quirks between calls; we rely on the `seen` hash set to dedup.
            let queryStart = since.addingTimeInterval(-0.5)
            let position = store.position(date: queryStart)
            guard let entries = try? store.getEntries(at: position) else { return }

            var newest = since
            var seen = seenSnapshot
            var order = orderSnapshot
            var buffer = ""
            buffer.reserveCapacity(8192)

            for entry in entries {
                guard let log = entry as? OSLogEntryLog else { continue }
                guard log.subsystem.hasPrefix("com.carlink") else { continue }

                // Hash = date (ns) ⊕ subsystem ⊕ category ⊕ message. Collisions are fine;
                // a false positive drops one duplicate, which is what we want anyway.
                var hasher = Hasher()
                hasher.combine(log.date.timeIntervalSinceReferenceDate)
                hasher.combine(log.subsystem)
                hasher.combine(log.category)
                hasher.combine(log.composedMessage)
                let h = hasher.finalize()

                if seen.contains(h) { continue }
                seen.insert(h)
                order.append(h)

                if log.date > newest { newest = log.date }
                buffer.append(Self.format(log))
                buffer.append("\n")
            }

            if !buffer.isEmpty {
                try? handle.write(contentsOf: Data(buffer.utf8))
                try? handle.synchronize()
            }

            // Cap the seen-set size to keep memory bounded, evicting oldest-first
            // so the hashes covering the re-query overlap window survive.
            if order.count > 4096 {
                let evicted = order.prefix(order.count - 2048)
                for h in evicted { seen.remove(h) }
                order.removeFirst(order.count - 2048)
            }

            state.withLock { s in
                s.lastDate = newest
                s.recent = seen
                s.recentOrder = order
            }
        }
    }

    // MARK: - Formatting

    private static let iso = Date.ISO8601FormatStyle(includingFractionalSeconds: true)

    private static func format(_ log: OSLogEntryLog) -> String {
        let ts = log.date.formatted(iso)
        let level: String
        switch log.level {
        case .undefined: level = "----"
        case .debug:     level = "DBG "
        case .info:      level = "INFO"
        case .notice:    level = "NOTE"
        case .error:     level = "ERR "
        case .fault:     level = "FLT "
        @unknown default: level = "????"
        }
        return "\(ts) \(level) [\(log.subsystem):\(log.category)] \(log.composedMessage)"
    }

    private static func fileName() -> String {
        let fmt = DateFormatter()
        fmt.dateFormat = "yyyy-MM-dd_HHmmss"
        fmt.locale = Locale(identifier: "en_US_POSIX")
        let ts = fmt.string(from: Date())
        let pid = ProcessInfo.processInfo.processIdentifier
        return "carlink_\(ts)_\(pid).log"
    }

    private static func preamble() -> String {
        var lines: [String] = []
        lines.append("=== CarLink Session Log ===")
        lines.append("Started:   \(Date().formatted(iso))")
        lines.append("PID:       \(ProcessInfo.processInfo.processIdentifier)")
        lines.append("Bundle ID: \(Bundle.main.bundleIdentifier ?? "?")")
        if let build = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String {
            lines.append("Version:   \(build)")
        }
        lines.append("")
        return lines.joined(separator: "\n")
    }

    // MARK: - Pruning

    /// Delete log files older than 14 days to keep ~/Library/Logs/Carlink tidy.
    private func pruneOldLogs() {
        let cutoff = Date().addingTimeInterval(-14 * 86_400)
        guard let entries = try? FileManager.default.contentsOfDirectory(
            at: Self.logsDirectory,
            includingPropertiesForKeys: [.contentModificationDateKey]
        ) else { return }
        for url in entries where url.pathExtension == "log" {
            let mtime = (try? url.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate
            if let mtime, mtime < cutoff {
                try? FileManager.default.removeItem(at: url)
            }
        }
    }
}
