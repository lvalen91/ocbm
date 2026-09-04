// BoxLogStore.swift — the app's durable, combined record of box activity (CH_LOG).
//
// Receives decoded `LogEntry` batches from `OCBMClient.onBoxLog` and:
//   1. keeps an in-memory ring (last `ringCapacity` lines) for the Box Log window's live tail,
//   2. appends every line to a per-session file `box-<yyyyMMdd-HHmmss>.log` under FileLogger's own
//      log directory (same directory, rotation/prune conventions and off-main serial-queue thread
//      model as `FileLogger.swift` — mirror those patterns, don't duplicate them),
//   3. ALSO emits every line through `os.Logger` on a `com.carlink.*` subsystem, so FileLogger's own
//      poller (which captures every `com.carlink.*` entry) picks it up — the app's existing session
//      log therefore already IS the combined app+box timeline; no second export path is needed.
//
// Thread model: `ingest` is called on OCBMClient's transport read queue. The ring update and the
// `onAppend` UI callback are synchronous (cheap — array append + a callback), but the actual file
// write is dispatched to a private serial `writeQueue`, exactly like FileLogger keeps OSLogStore
// polling off the caller. `endSession()` is a HARD synchronous flush+close (drains `writeQueue`) —
// it must run BEFORE the OCBM session-end path (HOST_GONE/disconnect/quit) tears the pipe down, or a
// box line already in flight can be lost. See the call site in `AppDelegate.endSession()`.

import Foundation
import OSLog
import os
import Synchronization

final class BoxLogStore: Sendable {
    static let shared = BoxLogStore()

    /// Cap on the in-memory tail ring — bounds the Box Log window's live view regardless of session
    /// length; the session FILE (and the FileLogger-mirrored combined log) is unbounded.
    static let ringCapacity = 20_000

    /// One rendered, ready-to-display combined-log line (no trailing newline).
    struct Line: Sendable {
        let date: Date
        let text: String
        /// Replayed from `/tmp/box.log`'s existing content at enable time, not observed live — see
        /// `LogEntry.isBackfill`. The Box Log window renders these dimmed and can hide them.
        let isBackfill: Bool
    }

    private static let bridgeLogger = Logger(subsystem: "com.carlink.app", category: "boxlog")
    private static let iso = Date.ISO8601FormatStyle(includingFractionalSeconds: true)

    private let log = Logger(subsystem: "com.carlink.app", category: "BoxLogStore")
    private let writeQueue = DispatchQueue(label: "carplay-host.boxlog.write", qos: .utility)
    private let state = Mutex<State>(State())

    private struct State {
        var ring: [Line] = []
        var fileURL: URL?
        var fileHandle: FileHandle?
        var running = false
        var onAppend: (@Sendable ([Line]) -> Void)?
    }

    private init() {}

    // MARK: - Lifecycle

    /// Open a fresh per-session file. Call once per OCBM session (client creation) — a reconnect gets
    /// a brand-new `box-<timestamp>.log` rather than silently appending to a stale one across a box
    /// reboot/reattach. No-op if a session is already open (guards a double `connect()`).
    func startSession() {
        let shouldStart = state.withLock { s -> Bool in
            guard !s.running else { return false }
            s.running = true
            return true
        }
        guard shouldStart else { return }

        writeQueue.async { [self] in
            do {
                try FileManager.default.createDirectory(at: FileLogger.logsDirectory, withIntermediateDirectories: true)
            } catch {
                log.error("failed to create logs dir: \(error.localizedDescription, privacy: .public)")
                state.withLock { $0.running = false }
                return
            }
            pruneOldSessionFiles()

            let url = FileLogger.logsDirectory.appendingPathComponent(Self.fileName())
            guard FileManager.default.createFile(atPath: url.path, contents: nil),
                  let handle = FileHandle(forWritingAtPath: url.path) else {
                log.error("failed to open \(url.path, privacy: .public)")
                state.withLock { $0.running = false }
                return
            }
            let aborted = state.withLock { s -> Bool in
                guard s.running else { return true } // endSession() raced this setup
                s.fileURL = url
                s.fileHandle = handle
                return false
            }
            if aborted {
                try? handle.close()
                return
            }
            log.info("session file: \(url.path, privacy: .public)")
        }
    }

    /// Synchronous flush + close. MUST be called BEFORE the OCBM session-end path
    /// (HOST_GONE/disconnect/quit) proceeds — see `AppDelegate.endSession()`. `writeQueue.sync`
    /// drains every write already queued by `ingest`, so nothing enqueued before this call is lost.
    func endSession() {
        let (handle, wasRunning) = state.withLock { s -> (FileHandle?, Bool) in
            let h = s.fileHandle
            let r = s.running
            s.running = false
            s.fileHandle = nil
            s.fileURL = nil
            return (h, r)
        }
        guard wasRunning else { return }
        writeQueue.sync {
            try? handle?.synchronize()
            try? handle?.close()
        }
    }

    var currentSessionURL: URL? { state.withLock { $0.fileURL } }

    /// Registered by the Box Log window while it's open (nil when closed). Delivered on the SAME
    /// thread `ingest` was called on (the OCBM transport read queue) — the window hops to main
    /// itself, matching every other `on*` callback's contract in this codebase.
    var onAppend: (@Sendable ([Line]) -> Void)? {
        get { state.withLock { $0.onAppend } }
        set { state.withLock { $0.onAppend = newValue } }
    }

    /// Current ring contents, oldest first — for a freshly-opened Box Log window to seed its tail.
    func snapshot() -> [Line] { state.withLock { $0.ring } }

    // MARK: - Ingest

    /// Feed decoded CH_LOG entries (`OCBMClient.onBoxLog`), in wire order. Renders each into the ring +
    /// session file + FileLogger's own stream.
    func ingest(_ entries: [LogEntry]) {
        guard !entries.isEmpty else { return }
        var lines: [Line] = []
        lines.reserveCapacity(entries.count)
        for e in entries {
            let date = Date(timeIntervalSince1970: TimeInterval(e.unixMs) / 1000)
            let name = OCBM.logSourceName(e.source)
            let body: String
            if let n = e.droppedCount {
                body = "!! \(n) lines dropped by \(name)"
            } else if e.isGapMarker {
                body = "!! \(e.text)"
            } else if e.isTruncated {
                body = "\(e.text) !! truncated"
            } else {
                body = e.text
            }
            // ` [backfill]` on the combined-log text so the app's own session file (and the
            // FileLogger-mirrored stream) let a grep separate replayed history from live lines,
            // without a second export path.
            let suffix = e.isBackfill ? " [backfill]" : ""
            lines.append(Line(
                date: date,
                text: "\(date.formatted(Self.iso)) [box/\(name)] \(body)\(suffix)",
                isBackfill: e.isBackfill
            ))
        }
        deliver(lines)
    }

    private func deliver(_ lines: [Line]) {
        let callback = state.withLock { s -> (@Sendable ([Line]) -> Void)? in
            s.ring.append(contentsOf: lines)
            if s.ring.count > Self.ringCapacity {
                s.ring.removeFirst(s.ring.count - Self.ringCapacity)
            }
            return s.onAppend
        }
        callback?(lines)

        // Mirror every line into the app's OWN combined-timeline log, via os.Logger so FileLogger's
        // existing `com.carlink.*` poller captures it — no second write path, no drift between "the
        // session log" and "the combined log". The box's wall clock is host-set at HELLO
        // (SETTIME, docs/carplay/01_OCBM_PROTOCOL.md), so `[box/<sourceName>]`-prefixed lines interleave sanely
        // with the app's own entries even though FileLogger stamps its OWN call-time, not `unix_ms`.
        for l in lines {
            Self.bridgeLogger.info("\(l.text, privacy: .public)")
        }

        writeQueue.async { [self] in
            guard let handle = state.withLock({ $0.fileHandle }) else { return }
            var buf = ""
            buf.reserveCapacity(lines.count * 96)
            for l in lines { buf.append(l.text); buf.append("\n") }
            try? handle.write(contentsOf: Data(buf.utf8))
            try? handle.synchronize()
        }
    }

    // MARK: - Naming / pruning

    private static func fileName() -> String {
        let fmt = DateFormatter()
        fmt.dateFormat = "yyyyMMdd-HHmmss"
        fmt.locale = Locale(identifier: "en_US_POSIX")
        return "box-\(fmt.string(from: Date())).log"
    }

    /// Same retention window as `FileLogger.pruneOldLogs` (14 days) — one policy, restated for the
    /// `box-*.log` filename family rather than shared code, since the two prune different directories'
    /// worth of a shared directory by a different prefix.
    private func pruneOldSessionFiles() {
        let cutoff = Date().addingTimeInterval(-14 * 86_400)
        guard let entries = try? FileManager.default.contentsOfDirectory(
            at: FileLogger.logsDirectory,
            includingPropertiesForKeys: [.contentModificationDateKey]
        ) else { return }
        for url in entries where url.lastPathComponent.hasPrefix("box-") && url.pathExtension == "log" {
            let mtime = (try? url.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate
            if let mtime, mtime < cutoff {
                try? FileManager.default.removeItem(at: url)
            }
        }
    }
}
