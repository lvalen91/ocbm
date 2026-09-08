// OCBMSessionCoordinator.swift — app-level reactions to OCBMClient + USBTransport delegate events.
//
// Extracted verbatim from OCBMClient.swift so the client half (OCBMClient + OCBMFraming + framing) can
// compile in a hardware-free CLI test harness without dragging in IOKit / USBTransport, which only this
// coordinator references (via USBTransportDelegate). OCBMClient itself talks to the pipe
// through the RawBulkTransport protocol and has no USBTransport dependency.

import Foundation
import os

/// Bridges OCBMClient + USBTransport delegate events into app-level reactions (task #29). Revives the
/// previously-unwired safety nets — the box's `SEV_HOST_GONE`, USBTransport's 5-read-error disconnect,
/// and write-failure — and adds a host-side A/V-progress watchdog so a "connected but not streaming"
/// session becomes visible (UI status) and actionable instead of silently sitting at "waiting for
/// adapter" (the docs/carplay/02_SESSION_LIFECYCLE.md app-side symptom). Callbacks are hopped to the main actor for UI/session work.
///
/// STATUS TEXT HOME (2026-09-05). Every status string the overlay shows is emitted from here, and
/// since 2026-09-05 every one of them passes through `SessionStatusComposer` on its way out, so the
/// last session failure (SessionFailureTracker — fed from CH_LOG by AppDelegate, and from the
/// phone-presence / streaming transitions below) decorates the headline and fills the second
/// overlay line. Before that, a retry loop where the iPhone tore every session down read as
/// "iPhone connected — starting CarPlay…" / "Waiting for phone…" forever. This is the ONE place the
/// composition happens; AppDelegate only forwards the two strings to the view.
// @unchecked Sendable invariant: all mutable watchdog state (streaming/lastTotal/idleTicks/
// announcedWaiting/announcedIdle/phonePresent/lastBase) is main-queue-confined — mutated ONLY inside
// `hop` blocks; the onStatus/onStatusDetail/onStreaming/onTransportLost callbacks are set once during
// setup and invoked only from those main-queue blocks.
final class OCBMSessionCoordinator: OCBMClientDelegate, USBTransportDelegate, @unchecked Sendable {
    private let log = Logger(subsystem: "com.carlink.ocbm", category: "coordinator")

    /// UI status text. /// First A/V (true) or A/V stopped (false). /// Hard USB transport loss (reason).
    var onStatus: ((String) -> Void)?
    /// Second overlay line: the last session failure in the box's words, nil when there is none.
    var onStatusDetail: ((String?) -> Void)?
    var onStreaming: ((Bool) -> Void)?
    var onTransportLost: ((String) -> Void)?

    // `ocbmDidUpdateStats` fires at ~1 Hz from the heartbeat, so ticks ≈ seconds.
    private let establishTicks = 20   // subscribed but no A/V within ~20s -> "waiting for phone"
    private let stallTicks = 8        // was streaming, A/V frozen ~8s -> "reconnecting"
    private var lastTotal: UInt64 = 0
    private var idleTicks = 0
    private var streaming = false
    private var announcedWaiting = false
    // Whether the iPhone is on the bus (from ocbmPhonePresence). CarPlay's screen is CHANGE-DRIVEN, so a
    // STATIC screen (settled home screen / paused map) sends ~no frames while the control plane stays
    // fully alive — the heartbeat that CALLS ocbmDidUpdateStats at ~1Hz is proof of that. A frame gap is
    // therefore NOT a disconnect while the phone is present; it's an idle-but-connected session. Used to
    // stop the watchdog from flapping the status to "Reconnecting…" on a still screen.
    private var phonePresent = false
    private var announcedIdle = false
    /// The last undecorated status this coordinator chose. Re-composed when the failure tracker
    /// changes (a teardown line arrives on the transport read queue, asynchronously to any of the
    /// events below), so the overlay updates the moment the box says why.
    private var lastBase = "Waiting for phone…"

    private let failures: SessionFailureTracker

    init(failures: SessionFailureTracker = .shared) {
        self.failures = failures
        failures.onChange = { [weak self] _ in
            guard let self else { return }
            self.hop { self.publish() }
        }
    }

    // MARK: OCBMClientDelegate
    func ocbmBoxNotReady() {
        log.error("box not ready (no HELLO_ACK) — surfacing to UI; HELLO retries continue")
        hop { self.status("Adapter not responding — check the box…") }
    }

    // All watchdog state (streaming/lastTotal/idleTicks/announcedWaiting) is mutated ONLY inside
    // `hop` (i.e. on the main queue). The delegate callbacks arrive on two different executors —
    // ocbmDidUpdateStats on the control `queue` (heartbeat), ocbmPhonePresence/ocbmSessionEvent on
    // the transport read queue — so confining every read AND write to the main queue is what keeps
    // this state race-free (the previous code mutated it on whichever queue called in).
    func ocbmPhonePresence(present: Bool) {
        if present {
            log.info("box: iPhone on the bus — session starting")
            hop {
                self.phonePresent = true
                self.failures.notePhonePresent(true)
                guard !self.streaming else { return }
                self.status("iPhone connected — starting CarPlay…")
            }
        } else {
            // Truthful + immediate (replaces waiting on the 20 s no-A/V watchdog for this case).
            log.info("box: no iPhone on the bus — waiting for plug")
            hop {
                self.phonePresent = false
                self.setStreaming(false)
                // After the streaming flag: the tracker must see "A/V had stopped" before it judges
                // whether this ABSENT closed an attempt that never streamed.
                self.failures.notePhonePresent(false)
                self.status("Waiting for phone…")
            }
        }
    }

    func ocbmSessionEvent(present: Bool) {
        guard !present else { return }
        log.info("box SESSION_EVENT: host GONE — box tore down; awaiting re-projection")
        hop {
            // lastTotal is NOT reset here: it's a monotonic decrypt tally (never decreases across the
            // process), and ocbmDidUpdateStats compares delta-since-last-tick (`total > lastTotal`).
            // Zeroing it made the very next stats tick after a legit HOST_GONE read `total > 0` and
            // flip back to "CarPlay streaming" one tick after declaring the session torn down.
            self.announcedWaiting = false; self.idleTicks = 0
            self.setStreaming(false)
            self.status("Waiting for phone…")
        }
    }

    func ocbmDidUpdateStats(video: (ok: UInt64, fail: UInt64), audio: (ok: UInt64, fail: UInt64)) {
        let total = video.ok &+ audio.ok
        hop {
            if total > self.lastTotal {
                self.lastTotal = total; self.idleTicks = 0
                // Every progressing tick, not only the status transition below: the tracker clears
                // its own streaming flag at each box teardown, and a phone that re-projects without a
                // presence flap inside the 8 s stall window never re-enters the branch below — its
                // later teardown would then be miscounted as a failure of a session that streamed.
                self.failures.noteStreaming(true)
                if !self.streaming || self.announcedIdle {
                    self.announcedWaiting = false; self.announcedIdle = false
                    self.log.info("A/V flowing — streaming")
                    self.setStreaming(true)
                    self.status("CarPlay streaming")
                }
                return
            }
            self.idleTicks += 1
            if !self.streaming {
                if self.idleTicks >= self.establishTicks && !self.announcedWaiting {
                    self.announcedWaiting = true
                    self.log.info("subscribed \(self.idleTicks, privacy: .public)s, no A/V — waiting for phone")
                    self.status("Waiting for phone…")
                }
            } else if self.idleTicks >= self.stallTicks {
                // A/V frozen after streaming. This is NOT a disconnect while the phone is present: the
                // heartbeat still calling us proves the link is alive, so the screen is simply STATIC
                // (change-driven). Keep the session shown (do NOT blank the last frame or churn to
                // "Reconnecting…"); just report a calm "connected". Only when the phone has actually left
                // (control plane down) do we fall back to waiting.
                if self.phonePresent {
                    if !self.announcedIdle {
                        self.announcedIdle = true
                        self.log.info("A/V idle \(self.idleTicks, privacy: .public)s but link alive — static screen, staying connected")
                        self.status("CarPlay connected")
                    }
                } else {
                    self.log.info("A/V stopped \(self.idleTicks, privacy: .public)s and phone absent — waiting")
                    self.setStreaming(false)
                    self.status("Waiting for phone…")
                }
            }
        }
    }

    /// App-driven SETUP relay state (plan P3). The RTSP/SETUP negotiation is invisible to the user, so
    /// this only logs (diagnostics) and never churns the UI status text — a relay hiccup falls back to
    /// the box's own local response and the session is unaffected.
    func noteSetupRelay(_ detail: String) {
        log.info("app-driven SETUP: \(detail, privacy: .public)")
    }

    // MARK: USBTransportDelegate
    func transportDidEncounterError(_ transport: USBTransport, error: Error) {
        log.error("USB transport error: \(error.localizedDescription, privacy: .public)")
    }

    func transportDidDisconnect(_ transport: USBTransport) {
        log.error("USB transport disconnect (read errors) — session lost")
        hop { self.onTransportLost?("USB transport disconnect") }
    }

    // MARK: - Status emission (main queue)

    /// Flip the streaming flag, tell the view AND the failure tracker (a transition to true is what
    /// resets its consecutive-failure count; the one signal that proves an attempt succeeded).
    private func setStreaming(_ on: Bool) {
        streaming = on
        failures.noteStreaming(on)
        onStreaming?(on)   // unconditional, exactly as every call site fired it before 2026-09-05
    }

    /// Record the base text and publish the composed pair.
    private func status(_ base: String) {
        lastBase = base
        publish()
    }

    /// Compose from the last base + the tracker snapshot + ConfigIntegrity's verdict and push both
    /// lines. Also the re-entry point when the tracker changes underneath us.
    private func publish() {
        let t = SessionStatusComposer.compose(base: lastBase, snapshot: failures.snapshot,
                                              verdict: ConfigIntegrity.shared.snapshot.verdict)
        onStatus?(t.headline)
        onStatusDetail?(t.detail)
    }

    private func hop(_ work: @escaping @Sendable () -> Void) { DispatchQueue.main.async(execute: work) }
}
