// ──────────────────────────────────────────────────────────────────────────────
// SessionFailureTracker.swift — "the session died; say WHY, in the box's own words" (2026-09-05)
// ──────────────────────────────────────────────────────────────────────────────
//
// WHY THIS EXISTS. On the night of 2026-09-05 a wireless CarPlay session sat in a retry loop. The
// box log (streamed to this app over CH_LOG the whole time) said, per attempt:
//
//   [session] RECORD: session-focus handshake sent (requestUI=true, takeScreen=true)
//   [session] RECORD done
//   [events] command response NOT OK: 'RTSP/1.0 400 Bad Request'
//   [receiver] TEARDOWN /... (enc, 42 B body)
//   [session] full TEARDOWN reason=host-request — stopping stream threads + resetting session state
//   [events] reader: EOF — iPhone closed the event channel
//
// What the app showed the owner: "iPhone connected — starting CarPlay…" then "Waiting for phone…",
// cycling on every attempt. Nothing said the iPhone was ENDING each session, nothing said how many
// times, nothing repeated the 400 the box had already printed. Diagnosing it meant reading raw box
// lines one by one. Every one of those lines was already in this process (BoxLogStore.ingest); the
// information was present and unsurfaced. This type is the missing step: it watches the same CH_LOG
// entries, recognises the receiver crate's teardown/rejection lines, and keeps ONE small piece of
// state — the last failure, the consecutive count, when — for the status overlay, the app log and the
// control socket to read.
//
// WHAT IS AND IS NOT CLASSIFIED — the box is the only source of truth, and it reports exactly this
// much (crates/vendor/receiver/src/{session,events,net,server}.rs):
//
//   * `[session] full TEARDOWN reason=host-request …`  — the iPhone sent an RTSP TEARDOWN
//     (server.rs `Route::Teardown` → `AvSession::teardown`). The token names the INITIATOR, not iOS's
//     motive; iOS never states one. So the summary says "iPhone sent TEARDOWN", never "iOS rejected
//     X because Y".
//   * `[session] TEARDOWN reason=link-loss (Drop with no prior host TEARDOWN) …` — the control
//     connection went away without a TEARDOWN. Two box lines can PRECEDE it and are its reported
//     cause when present: `[receiver] idle backstop tearing down session (…)` (the box's own quiet-
//     link timer) and `[receiver] control error → closing connection: …` (a feed/decrypt error).
//   * `[events] command response NOT OK: '<status line>'` — iOS answered a command WE sent with a
//     non-2xx. The box does not log WHICH command (events.rs has no correlation), so this is kept as
//     a precursor fact, and the last command the box logged SENDING (`[session] RECORD: session-focus
//     handshake sent (…)`) is recorded beside it as a separate fact — the reader draws the line, the
//     app does not.
//   * `[receiver] pair-setup Mn FAIL: …` / `pair-verify Mn FAIL: …` / `auth-setup FAILED: …` — a
//     handshake the box refused or that failed, before RECORD. Verbatim.
//   * `[events] reader: EOF — iPhone closed the event channel` — an epilogue fact folded into the
//     failure it follows.
//   * `[session] partial TEARDOWN …` — deliberately NOT a failure: the session is kept (that is a
//     per-stream stop, e.g. Siri's per-turn speechRecognition stream).
//   * A phone that went ABSENT after RECORD with no teardown line at all — recorded with
//     `boxReason == nil` and rendered as "no reason reported". Never guessed.
//
// A teardown counts as a FAILURE only when the attempt never streamed A/V — a session that ran and
// was then ended by the phone (user hit disconnect, phone left the car) is an ending, not a failure,
// and is logged at info. `consecutiveFailures` is the number of such failures since the last attempt
// that reached streaming; streaming resets it to 0. This is the number that makes "3rd failure in a
// row" look different from a first attempt and from a healthy idle box with no phone.
//
// LAYERING. Foundation-only (like ConfigIntegrity.swift) so tests/run_tests.sh compiles it. Inputs
// come from two places: `ingest(_ entries:)` on OCBMClient's transport read queue (wired in
// AppDelegate next to BoxLogStore.ingest), and `notePhonePresent`/`noteStreaming` from
// OCBMSessionCoordinator on the main queue. Everything is under one NSLock; callbacks fire outside
// it. The composition of the status text lives here too (`SessionStatusComposer`) so the wording is
// pinned by the harness rather than living in the IOKit-coupled coordinator.
//
// LIMITS TO KNOW. Backfill entries (`LogEntry.isBackfill`, replayed history on every reconnect) are
// ignored — counting them would re-report last week's failures on every connect. When the CH_LOG
// stream is OFF (Settings ▸ Diagnostics) the tracker sees no lines and can only report presence
// flaps with "no reason reported"; the snapshot carries `boxLogStreamEnabled` so the UI can say so.
// The `[receiver] <METHOD> <path>` request lines are gated on the receiver's `verbose` flag and are
// NOT relied on; RECORD done, the teardown lines and the events lines are unconditional.
// ──────────────────────────────────────────────────────────────────────────────

import Foundation
import os

/// Where the current attempt got to. Drives the (a)/(b)/(c) distinction in the status text.
enum SessionAttemptPhase: String, Sendable {
    /// No phone, no attempt.
    case idle
    /// A phone is present (or the box has begun a control connection) but RECORD has not been logged.
    case connecting
    /// `[session] RECORD done` was logged: the session was established.
    case recorded
    /// A/V is flowing (from the coordinator's watchdog).
    case streaming
}

/// One recorded teardown. `boxReason` is the box's own token, verbatim, or nil when the box printed
/// no reason line at all.
struct SessionFailure: Equatable, Sendable {
    enum Kind: String, Sendable {
        /// `full TEARDOWN reason=host-request`: the iPhone sent an RTSP TEARDOWN.
        case iphoneTeardown
        /// `TEARDOWN reason=link-loss`: the connection dropped with no TEARDOWN.
        case linkLoss
        /// A `reason=` token this app does not know. Rendered verbatim.
        case otherTeardown
        /// `pair-setup/pair-verify … FAIL` or `auth-setup FAILED` before RECORD.
        case handshakeFailed
        /// Phone went ABSENT after a session milestone with no teardown line seen.
        case unreported
    }

    let date: Date
    let kind: Kind
    /// The box's `reason=` token (`host-request`, `link-loss`), or the handshake FAIL text. nil ⇒ the
    /// box did not say.
    let boxReason: String?
    /// The last precursor line seen in this attempt before the teardown, verbatim minus its
    /// `[events] `/`[receiver] ` tag: `command response NOT OK: '…'`, `idle backstop tearing down
    /// session (…)`, `control error → closing connection: …`.
    let precursor: String?
    /// The last command the box logged SENDING in this attempt (a fact placed beside `precursor`, not
    /// joined to it — the box does not correlate the two).
    let lastCommandSent: String?
    /// RECORD done had been logged for this attempt.
    let reachedRecord: Bool
    /// A/V flowed in this attempt. True ⇒ this is an ENDING, not a failure, and it is not counted.
    let streamed: Bool
    /// `[events] reader: EOF` arrived after the teardown and before the next attempt.
    var iphoneClosedEventChannel: Bool = false

    /// The one line: what happened, in the box's words, plus the facts around it. Same text in the
    /// overlay, the app log and the control socket.
    var summary: String {
        var s: String
        switch kind {
        case .iphoneTeardown:
            s = "iPhone sent TEARDOWN \(reachedRecord ? "after RECORD" : "before RECORD") (reason=host-request)"
        case .linkLoss:
            s = "connection dropped with no TEARDOWN (reason=link-loss)"
        case .otherTeardown:
            s = "box tore the session down (reason=\(boxReason ?? "?"))"
        case .handshakeFailed:
            s = "handshake failed before RECORD: \(boxReason ?? "?")"
        case .unreported:
            s = "session ended \(reachedRecord ? "after RECORD" : "before RECORD") — no reason reported by the box"
        }
        if let p = precursor { s += "; before it the box logged: \(p)" }
        if let c = lastCommandSent { s += "; last command the box logged sending: \(c)" }
        if iphoneClosedEventChannel { s += "; then the iPhone closed the event channel" }
        return s
    }
}

/// Everything the status overlay, `get session` and the log line read.
struct SessionFailureSnapshot: Sendable {
    var phase: SessionAttemptPhase
    /// Failures since the last attempt that streamed (0 on a healthy box).
    var consecutiveFailures: Int
    /// The most recent teardown of any kind (failure or ending), or nil.
    var last: SessionFailure?
    /// Whether CH_LOG is armed — false means the tracker is mostly blind and the UI must say so.
    var boxLogStreamEnabled: Bool
    /// The consecutive failures all carry the same `kind` + `boxReason` + `precursor` (the retry loop
    /// tell of 2026-09-05).
    var repeating: Bool
}

final class SessionFailureTracker: @unchecked Sendable {
    static let shared = SessionFailureTracker()

    private let lock = NSLock()
    private let log = Logger(subsystem: "com.carlink.ocbm", category: "sessionfail")

    // Attempt in progress (nil ⇒ idle).
    private struct Attempt {
        var reachedRecord = false
        var streamed = false
        var precursor: String?
        var lastCommandSent: String?
        var milestone = false   // any box session line seen (RECORD / pair-verify OK / SETUP)
    }
    private var attempt: Attempt?
    private var phonePresent = false
    private var streamingNow = false
    private var consecutive = 0
    private var last: SessionFailure?
    /// `last` was closed by phone-ABSENT with no teardown line ("no reason reported") and may still be
    /// UPGRADED in place by the teardown line for the same session. SEV_PHONE_ABSENT (CH_CTRL) and
    /// the receiver's `full TEARDOWN` line (CH_LOG, via the tailer) race each other in either order;
    /// without this the ABSENT-first order counted one teardown twice — once unexplained, once
    /// explained. Cleared when the next attempt opens.
    private var lastIsProvisional = false
    /// The distinct (kind, reason, precursor) signatures of the current run of failures.
    private var runSignatures: Set<String> = []
    private var logStreamEnabled = true

    /// Fired (outside the lock, on the caller's queue) after every recorded teardown or ending, so
    /// the coordinator can re-render the status text without waiting for its next watchdog tick.
    /// Set from the main queue (a new coordinator per OCBM session), read on the transport read
    /// queue — so both go through the lock.
    var onChange: (@Sendable (SessionFailureSnapshot) -> Void)? {
        get { lock.lock(); defer { lock.unlock() }; return _onChange }
        set { lock.lock(); _onChange = newValue; lock.unlock() }
    }
    private var _onChange: (@Sendable (SessionFailureSnapshot) -> Void)?
    /// Test seam: every line this instance would send to os_log, with its level.
    var onLog: (@Sendable (_ isError: Bool, _ line: String) -> Void)? {
        get { lock.lock(); defer { lock.unlock() }; return _onLog }
        set { lock.lock(); _onLog = newValue; lock.unlock() }
    }
    private var _onLog: (@Sendable (_ isError: Bool, _ line: String) -> Void)?

    init() {}

    // MARK: - Inputs from the coordinator (main queue)

    func notePhonePresent(_ present: Bool) {
        var fire: SessionFailureSnapshot?
        lock.lock()
        phonePresent = present
        if present {
            if attempt == nil { attempt = Attempt() }
            lastIsProvisional = false
        } else {
            // ABSENT with an attempt still open: if the box had reported a session milestone and no
            // teardown line closed it, that is a teardown the box did not explain. Record it as such
            // — never with a guessed reason. An attempt with no milestone (phone came and went
            // without a session) is dropped silently: nothing was established, nothing failed.
            if let a = attempt, a.milestone {
                fire = closeAttemptLocked(kind: .unreported, boxReason: nil)
                lastIsProvisional = true
            } else {
                attempt = nil
            }
        }
        lock.unlock()
        if let fire { onChange?(fire) }
    }

    func noteStreaming(_ on: Bool) {
        lock.lock()
        streamingNow = on
        if on {
            if attempt == nil { attempt = Attempt() }
            attempt?.streamed = true
            attempt?.milestone = true
            consecutive = 0
            runSignatures.removeAll()
        }
        lock.unlock()
    }

    func noteLogStream(enabled: Bool) {
        lock.lock(); logStreamEnabled = enabled; lock.unlock()
    }

    /// App-session teardown (AppDelegate.endSession). Drops the open attempt without recording it —
    /// the APP ended this, not the phone — and keeps the history so the owner can still read it.
    func sessionEnded() {
        lock.lock()
        attempt = nil
        phonePresent = false
        streamingNow = false
        lock.unlock()
    }

    // MARK: - Input from CH_LOG (transport read queue)

    /// Feed the same decoded entries BoxLogStore gets. Backfill, gap markers and drop markers are
    /// skipped (see the header). Cheap: a handful of prefix checks per line.
    func ingest(_ entries: [LogEntry]) {
        for e in entries where !e.isBackfill && !e.isGapMarker && e.droppedCount == nil {
            noteBoxLine(e.text, at: Date(timeIntervalSince1970: TimeInterval(e.unixMs) / 1000))
        }
    }

    /// One box log line (body only — no `[box/<source>]` prefix). Exposed for the harness.
    func noteBoxLine(_ text: String, at date: Date = Date()) {
        var fire: SessionFailureSnapshot?
        lock.lock()
        defer { lock.unlock(); if let fire { onChange?(fire) } }

        // ── session milestones ────────────────────────────────────────────────────────────────
        if text.hasPrefix("[session] RECORD done") {
            if attempt == nil { attempt = Attempt(); lastIsProvisional = false }
            attempt?.reachedRecord = true
            attempt?.milestone = true
            return
        }
        if text.hasPrefix("[receiver] pair-verify OK") || text.hasPrefix("[session] SETUP phase2") {
            if attempt == nil { attempt = Attempt() }
            attempt?.milestone = true
            return
        }
        if text.hasPrefix("[session] RECORD: session-focus handshake sent") {
            if attempt == nil { attempt = Attempt() }
            attempt?.milestone = true
            attempt?.lastCommandSent = String(text.dropFirst("[session] RECORD: ".count))
            return
        }

        // ── precursors: facts the box prints BEFORE the teardown that names them ──────────────
        if let p = Self.precursor(text) {
            if attempt == nil, lastIsProvisional, let l = last {
                // Belongs to the session ABSENT already closed — attach, do not open a new attempt.
                last = Self.upgraded(l, kind: l.kind, boxReason: l.boxReason, precursor: p)
                fire = snapshotLocked()
                return
            }
            if attempt == nil { attempt = Attempt() }
            attempt?.precursor = p
            return
        }

        // ── teardowns ─────────────────────────────────────────────────────────────────────────
        if text.hasPrefix("[session] partial TEARDOWN") { return }   // session kept — not a failure
        if let reason = Self.teardownReason(text) {
            switch reason {
            case "host-request": fire = closeAttemptLocked(kind: .iphoneTeardown, boxReason: reason, at: date)
            case "link-loss":    fire = closeAttemptLocked(kind: .linkLoss, boxReason: reason, at: date)
            default:             fire = closeAttemptLocked(kind: .otherTeardown, boxReason: reason, at: date)
            }
            return
        }
        if let why = Self.handshakeFailure(text) {
            fire = closeAttemptLocked(kind: .handshakeFailed, boxReason: why, at: date)
            return
        }

        // ── epilogue ──────────────────────────────────────────────────────────────────────────
        if text.hasPrefix("[events] reader: EOF"), attempt == nil, var l = last, !l.iphoneClosedEventChannel {
            l.iphoneClosedEventChannel = true
            last = l
            fire = snapshotLocked()
            return
        }
    }

    // MARK: - Output

    var snapshot: SessionFailureSnapshot {
        lock.lock(); defer { lock.unlock() }
        return snapshotLocked()
    }

    // MARK: - Internals

    private func snapshotLocked() -> SessionFailureSnapshot {
        let phase: SessionAttemptPhase
        if streamingNow { phase = .streaming }
        else if let a = attempt { phase = a.reachedRecord ? .recorded : .connecting }
        else if phonePresent { phase = .connecting }
        else { phase = .idle }
        return SessionFailureSnapshot(phase: phase, consecutiveFailures: consecutive, last: last,
                                      boxLogStreamEnabled: logStreamEnabled,
                                      repeating: consecutive >= 2 && runSignatures.count == 1)
    }

    /// Record the teardown, count it if the attempt never streamed, log one line, and clear the
    /// attempt. Returns the snapshot to hand to `onChange`.
    private func closeAttemptLocked(kind: SessionFailure.Kind, boxReason: String?,
                                    at date: Date = Date()) -> SessionFailureSnapshot {
        if attempt == nil, lastIsProvisional, let l = last, kind != .unreported {
            // The teardown line for the session ABSENT already closed: upgrade that record in place —
            // same count, the box's reason filled in, one log line saying so.
            lastIsProvisional = false
            let up = Self.upgraded(l, kind: kind, boxReason: boxReason, precursor: l.precursor)
            last = up
            runSignatures.remove("\(l.kind.rawValue)|\(l.boxReason ?? "")|\(l.precursor ?? "")")
            runSignatures.insert("\(kind.rawValue)|\(boxReason ?? "")|\(up.precursor ?? "")")
            emit(isError: true, "CARPLAY SESSION FAILED (\(consecutive) in a row) — box reason arrived: \(up.summary)")
            return snapshotLocked()
        }
        let a = attempt ?? Attempt()
        let f = SessionFailure(date: date, kind: kind, boxReason: boxReason, precursor: a.precursor,
                               lastCommandSent: a.lastCommandSent, reachedRecord: a.reachedRecord,
                               streamed: a.streamed || streamingNow)
        attempt = nil
        // The coordinator's streaming(false) arrives up to ~8 s after A/V stops; the attempt is over
        // now, so the next stats tick must not credit this dead session.
        streamingNow = false
        last = f
        lastIsProvisional = false   // notePhonePresent(false) re-arms it for its own record
        let line: String
        if f.streamed {
            line = "CARPLAY SESSION ENDED after streaming: \(f.summary)"
            emit(isError: false, line)
        } else {
            consecutive += 1
            runSignatures.insert("\(kind.rawValue)|\(boxReason ?? "")|\(a.precursor ?? "")")
            line = "CARPLAY SESSION FAILED (\(consecutive) in a row): \(f.summary)"
            emit(isError: true, line)
        }
        return snapshotLocked()
    }

    /// The same record with the box's reason (and precursor) filled in; date/RECORD/streamed kept.
    private static func upgraded(_ l: SessionFailure, kind: SessionFailure.Kind, boxReason: String?,
                                 precursor: String?) -> SessionFailure {
        var f = SessionFailure(date: l.date, kind: kind, boxReason: boxReason, precursor: precursor,
                               lastCommandSent: l.lastCommandSent, reachedRecord: l.reachedRecord,
                               streamed: l.streamed)
        f.iphoneClosedEventChannel = l.iphoneClosedEventChannel
        return f
    }

    /// Called with the lock HELD (from closeAttemptLocked): reads the raw seam, never the locked
    /// getter (NSLock is not recursive).
    private func emit(isError: Bool, _ line: String) {
        if isError { log.error("\(line, privacy: .public)") } else { log.info("\(line, privacy: .public)") }
        _onLog?(isError, line)
    }

    /// `[session] full TEARDOWN reason=host-request — …` / `[session] TEARDOWN reason=link-loss (…`
    /// → the token after `reason=`. nil for any other line (including partial teardowns, handled
    /// before this is called).
    static func teardownReason(_ text: String) -> String? {
        guard text.hasPrefix("[session] "), text.contains("TEARDOWN reason="),
              let r = text.range(of: "reason=") else { return nil }
        let rest = text[r.upperBound...]
        let token = rest.prefix { !$0.isWhitespace }
        return token.isEmpty ? nil : String(token)
    }

    /// The three precursor lines, returned as their body (tag stripped, otherwise verbatim):
    /// `command response NOT OK: '…'` (events.rs), `idle backstop tearing down session (…)` and
    /// `control error → closing connection: …` (net.rs). Anything else ⇒ nil.
    static func precursor(_ text: String) -> String? {
        for tag in ["[events] ", "[receiver] "] where text.hasPrefix(tag) {
            let body = String(text.dropFirst(tag.count))
            if body.hasPrefix("command response NOT OK: ")
                || body.hasPrefix("idle backstop tearing down session")
                || body.hasPrefix("control error → closing connection: ") {
                return body
            }
        }
        return nil
    }

    /// `pair-setup Mn FAIL: …` / `pair-verify Mn FAIL: …` / `auth-setup FAILED: …` (server.rs), as
    /// their body. The `… Mn ok` siblings do not match.
    static func handshakeFailure(_ text: String) -> String? {
        guard text.hasPrefix("[receiver] ") else { return nil }
        let body = String(text.dropFirst("[receiver] ".count))
        if (body.hasPrefix("pair-setup ") || body.hasPrefix("pair-verify ")), body.contains(" FAIL: ") { return body }
        if body.hasPrefix("auth-setup FAILED: ") { return body }
        return nil
    }
}

// MARK: - Status composition (pure — pinned by tests/main.swift)

/// Turns the coordinator's base status + the tracker's state + ConfigIntegrity's verdict into the
/// two lines the overlay shows: a headline that differs between "no phone", "phone trying", and
/// "retrying after N failures", and a detail line that repeats the failure in the box's words with a
/// hint only where one is knowable. All wording lives here so the harness pins it.
enum SessionStatusComposer {
    struct Text: Equatable {
        let headline: String
        let detail: String?
    }

    /// `base` is what the coordinator would have shown before this existed ("Waiting for phone…",
    /// "iPhone connected — starting CarPlay…", "CarPlay streaming", …). `verdict` is
    /// ConfigIntegrity's — `.notInForce` on the last stream is the one hint that IS knowable.
    static func compose(base: String, snapshot s: SessionFailureSnapshot,
                        verdict: ConfigVerdict) -> Text {
        // Streaming, or nothing has ever failed: the historical text, unchanged. A healthy idle box
        // with no phone must look exactly as it always did.
        guard s.phase != .streaming, s.consecutiveFailures > 0, let f = s.last, !f.streamed else {
            return Text(headline: base, detail: nil)
        }
        let n = s.consecutiveFailures
        let times = n == 1 ? "once" : "\(n)× in a row"
        let headline: String
        switch s.phase {
        case .idle:
            headline = "CarPlay session failed \(times) — waiting for phone to retry…"
        case .connecting, .recorded:
            headline = "iPhone connected — retrying CarPlay (failed \(times))…"
        case .streaming:
            headline = base   // unreachable (guarded above); keeps the switch exhaustive
        }
        var detail = "Last: \(f.summary)"
        if f.kind == .unreported, !s.boxLogStreamEnabled {
            detail += " — box log stream is off (Settings ▸ Diagnostics); the reason cannot be seen"
        }
        if case .notInForce = verdict {
            detail += " — the last stream ran with the pushed config NOT in force (box on built-in defaults?)"
        } else if s.repeating, n >= 3 {
            detail += " — same failure each time; the iPhone's own log (idevicesyslog) names what it objects to"
        }
        return Text(headline: headline, detail: detail)
    }
}
