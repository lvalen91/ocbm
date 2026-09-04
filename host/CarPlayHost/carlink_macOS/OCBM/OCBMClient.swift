// OCBMClient.swift — the host-side OCBM session controller (replaces the riddlebox AdapterProtocol).
//
// Ties the pieces together: it drives a raw USB bulk pipe (via `RawBulkTransport`, which the existing
// IOKit `USBTransport` adapts to), frames/reassembles OCBM (OCBMFraming), runs the docs/carplay/02_SESSION_LIFECYCLE.md host role
// (HELLO → SUBSCRIBE → ~1 Hz HEARTBEAT → STOP), and routes CH_VIDEO/CH_MEDIA_AUDIO to the decrypt layer
// (OCBMAVDecrypt) whose plaintext A/V feeds the VideoToolbox decoder + audio player. This is the host
// counterpart of the box's `ocbmd` + `ocbm-host avdec`, now inside a real macOS app.
//
// Being the "host app" in the committed model, SUBSCRIBE is what commands the box IDLE→projection→ARM;
// the heartbeat keeps the session alive; STOP (or app exit / heartbeat lapse) tears it down.

import Foundation
import os
import Synchronization

/// Minimal raw bulk pipe the client needs. `USBTransport` provides this over the claimed OCBM interface.
protocol RawBulkTransport: AnyObject {
    /// Write raw bytes to the OUT bulk endpoint (a full framed OCBM frame). Returns success.
    @discardableResult func writeBulk(_ bytes: [UInt8]) -> Bool
    /// Register a callback invoked on the read queue with each raw bulk IN chunk.
    func setReadHandler(_ handler: @escaping ([UInt8]) -> Void)
    func start()
    func stop()
}

/// Outcome of a UI-facing send, delivered once on the main queue (C3). Distinguishes the three ways a
/// control can end up: it reached the wire (`sent`); it was silently swallowed because the box has no
/// session yet, so it never left the host (`droppedNotSubscribed`); or the USB bulk write itself failed
/// (`writeFailed`). The Controls UI reports each differently instead of always claiming "sent".
enum SendOutcome: Sendable { case sent; case droppedNotSubscribed; case writeFailed }

protocol OCBMClientDelegate: AnyObject {
    func ocbmSessionEvent(present: Bool)          // box CT_SESSION_EVENT
    func ocbmDidUpdateStats(video: (ok: UInt64, fail: UInt64), audio: (ok: UInt64, fail: UInt64))
    /// HELLO went unanswered through the fast retry phase (~15 s) — the box is absent or still
    /// booting. Retries continue in the background; this is the UI's cue to say "box not ready"
    /// instead of the misleading "waiting for phone". (#34)
    func ocbmBoxNotReady()
    /// Truthful phone presence from the box (SEV_PHONE_*): the supervisor scans the adapter's
    /// phone-facing bus every second, so this arrives within ~1–2 s of a plug/unplug — the UI can
    /// say "waiting for phone" immediately instead of a 20 s no-A/V watchdog. (2026-07-12)
    func ocbmPhonePresence(present: Bool)
}

// Thread-safety: `subscribed`/heartbeat/helloLoop state is confined to `queue`; `seq` is guarded by
// `sendLock` (send() is callable from both `queue` and `aaWriteQueue`, see its doc comment); the read
// path (reasm, av) runs on the transport's read queue and shares no mutable state with the above.
final class OCBMClient: @unchecked Sendable {
    private let transport: RawBulkTransport
    private let reasm = OCBMReassembler()
    let av = OCBMAVDecrypt()
    weak var delegate: OCBMClientDelegate?

    private let log = Logger(subsystem: "com.carlink.ocbm", category: "client")
    private var seq: UInt32 = 0
    private var heartbeatTimer: DispatchSourceTimer?
    private let queue = DispatchQueue(label: "carplay-host.ocbm", qos: .userInteractive)
    /// Android Auto CH_IP writes run here, NOT on `queue`, so an AA backlog does not delay the 1 Hz
    /// heartbeat's DISPATCH.
    ///
    /// CORRECTION (2026-08-25): this does NOT decouple them on the wire, and the previous comment
    /// claiming AA writes "can never delay the heartbeat" was wrong. Both queues funnel into
    /// USBTransport's single serial `writeQueue.sync`, where one write can occupy up to
    /// `rawWriteCompletionTimeoutMs` (1500 ms). ~7 stalled AA frames therefore starve the heartbeat
    /// past ocbmd's 10 s `HEARTBEAT_GRACE`, and the box declares the host gone — which tears down the
    /// CH_IP relay and bounces the phone. AA is the first workload that writes at this rate (per-frame
    /// video + audio ACKs, 50+/s); CarPlay barely writes, which is why only AA exposes it.
    /// `writeStallMs` below exists to make that visible instead of silent.
    private let aaWriteQueue = DispatchQueue(label: "carplay-host.ocbm.aa", qos: .userInitiated)
    private var subscribed = false

    // Last-logged decrypt tallies, so the 1 Hz heartbeat only emits a line when they change.
    private var lastVideoOK: UInt64 = 0, lastVideoFail: UInt64 = 0
    private var lastAudioOK: UInt64 = 0, lastAudioFail: UInt64 = 0
    private var lastAltOK: UInt64 = 0, lastAltFail: UInt64 = 0

    /// The ephemeral session config (YAML) pushed on SUBSCRIBE. Sent verbatim; the box stores it.
    var sessionConfig: Data = Data()

    /// Raw CH_METADATA payload chunks (box-forwarded inbound /command plists) — the Metadata window's
    /// feed. Invoked on the transport read queue; the consumer (MetadataStore) is thread-safe.
    var onMetadata: (([UInt8]) -> Void)?

    /// Raw CH_RTSP payload chunks — the app-driven SETUP control relay's seam feed (plan P3). Invoked on
    /// the transport read queue; the consumer (OCBMControlRelay) reassembles + authors. Nil (default) when
    /// app-driven SETUP is off, so the box's local response is what the phone sees (box-driven fallback).
    var onControlRelay: (([UInt8]) -> Void)?

    /// CH_IP stream-mux inbound (used by the Android Auto transport, which rides CH_IP to reach the
    /// box's aa-bridge). Called per IP_DATA sub-frame with (conn id, bytes); an empty payload signals
    /// IP_CLOSE for that id. INVOKED on the transport read queue — but unlike the other `on*`
    /// callbacks (set once before `connect()`), this one and `onIpWriteFailed` are (re)ASSIGNED
    /// mid-session by `AAOCBMTransport.init` (`AASession.swift`), reached off the transport read queue
    /// via `onProjectionMode → Task { @MainActor … } → startAAOverOCBM` with no synchronization against
    /// a concurrent read-queue call — a genuine data race on the stored closure (func ptr + context
    /// word), caught the same way `projMode` was. Lock-guarded like `projMode` for the same reason.
    var onIpData: ((UInt16, [UInt8]) -> Void)? {
        get { ipCallbackLock.lock(); defer { ipCallbackLock.unlock() }; return _onIpData }
        set { ipCallbackLock.lock(); _onIpData = newValue; ipCallbackLock.unlock() }
    }
    private var _onIpData: ((UInt16, [UInt8]) -> Void)?
    private let ipCallbackLock = NSLock()

    /// Mic-uplink gate (CH_CTRL `ctUplink`): the box raises this when the iPhone opens a type-100
    /// `input=true` MainAudio SETUP (Siri/telephony) — or an HFP call opens SCO — and lowers it on
    /// TEARDOWN. `(on, sampleRate, channels, codec)`. The app captures the mic ONLY while on, at the
    /// box-negotiated format, and ships the result back over `sendMicPCM`. `codec` is 0 for S16LE PCM
    /// (every CarPlay uplink and HFP narrowband/CVSD) or `OCBM.seamCodecMsbc` for HFP WIDEBAND, where
    /// the box wants whole 60-byte mSBC eSCO packets rather than PCM. Invoked on the transport read queue.
    var onUplinkGate: ((Bool, UInt32, UInt8, UInt8) -> Void)?

    /// CH_MIC frames shipped since launch (one per `sendMicPCM` call — MicCapture emits exact 20 ms
    /// frames). Read at 1 Hz by StreamMetricsMonitor for the AVmon `mictx=` rate; an Atomic so the
    /// sampler never contends with the audio IO thread that produces them.
    static let micTxFrames = Atomic<UInt64>(0)

    /// Wireless SSP Numeric-Comparison pairing code (CH_CTRL `ctPairingCode`): a non-nil 6-digit string
    /// is the code to DISPLAY for the user to match against the iPhone; `nil` clears it (pairing done or
    /// Just-Works). Invoked on the transport read queue.
    var onPairingCode: ((String?) -> Void)?

    /// Which projection transport the box armed (CH_CTRL `ctProjMode`, one of `OCBM.pm*`). The box owns
    /// arbitration — it sees the USB bus, runs the AOAP switch and claims `/tmp/projection_owner` — so
    /// this is the app's only signal for WHICH engine to run: `pmWiredAa` means drive the AA head-unit
    /// engine over CH_IP; a CarPlay mode (or `pmNone`) means the normal decode path. Delivered on change
    /// and once per fresh SUBSCRIBE. Invoked on the transport read queue.
    /// `(mode, seq)`. `seq` is stamped HERE, on the serial read queue, so it is the box's own
    /// emission order — the handler hops to the main actor and two hops can land out of order, so it
    /// must be able to tell it has been superseded (the `usbEventGen` hazard, same shape).
    var onProjectionMode: ((UInt8, UInt64) -> Void)?
    /// Monotonic counter for the above. Confined to the transport read queue.
    private var projModeSeq: UInt64 = 0
    /// Last mode the box announced. Read by `repushConfig` to refuse a live re-push while a CarPlay
    /// transport owns the box (the re-SUBSCRIBE dips box-side presence, which re-spawns airplayd).
    /// WRITTEN on the USB read loop (`handleCtrl`) and READ on `queue`, so it is lock-guarded —
    /// ThreadSanitizer caught exactly this as a data race the first time it shipped unguarded.
    private var lastProjMode: UInt8 = OCBM.pmNone
    private let modeLock = NSLock()
    private var projMode: UInt8 {
        get { modeLock.lock(); defer { modeLock.unlock() }; return lastProjMode }
        set { modeLock.lock(); lastProjMode = newValue; modeLock.unlock() }
    }

    /// CCPA-tab responses (CH_MGMT). `onBoxInfo` delivers a parsed GET_INFO snapshot (nil = parse failed);
    /// `onBoxAck` delivers an action result `(verb, status)` (status 0 = ok). Both on the read queue.
    var onBoxInfo: ((CCPAInfo?) -> Void)?
    var onBoxAck: ((UInt8, UInt8) -> Void)?

    /// Box readiness bitmask (CH_CTRL `ctBoxHealth`, `OCBM.bh*` bits). Sent only while subscribed, on
    /// change only, re-emitted after each fresh SUBSCRIBE — a reconnect always refreshes this rather
    /// than the app caching a stale value across sessions. Invoked on the transport read queue.
    var onBoxHealth: ((UInt8) -> Void)?
    /// Bluetooth/iAP2 handshake progress (CH_CTRL `ctBtPhase`, one of `OCBM.btp*`). Advisory — never
    /// gate on ordering. Same emission discipline as `onBoxHealth`. Invoked on the transport read queue.
    var onBtPhase: ((UInt8) -> Void)?
    /// Identity of the connected phone (CH_CTRL `ctPhoneIdent`); `nil` = no identity yet / cleared.
    /// Same emission discipline as `onBoxHealth`. Invoked on the transport read queue.
    var onPhoneIdent: ((PhoneIdent?) -> Void)?

    /// Decoded CH_LOG entries — the box's universal log stream (docs/carplay/01_OCBM_PROTOCOL.md
    /// CH_LOG). Invoked on the transport read queue, in wire order; `BoxLogStore` is the consumer. A
    /// `seq` jump is synthesized as an extra `source: logSourceTailer` marker entry ahead of the entry
    /// that revealed the gap.
    var onBoxLog: (([LogEntry]) -> Void)?

    /// Whether to auto-arm CH_LOG (`sendLogCtl`) right after each successful SUBSCRIBE — the box resets
    /// this to disabled on STOP/host-gone, so it must be re-sent every time (see `subscribe()`). Set
    /// from `BoxLogSettings` before `connect()`; a live toggle re-sends immediately via `sendLogCtl`.
    var logStreamEnabled: Bool = true
    /// KB cap for the box's log ring; 0 = box default (256 KB). Same source as `logStreamEnabled`.
    var logStreamCapKB: UInt16 = 256

    /// Truthful SUBSCRIBE state (C2): fires `true` when a SUBSCRIBE write lands (box now streaming-capable)
    /// and `false` the moment the box declares the host GONE, BEFORE the re-subscribe attempt. The UI gates
    /// its "did it reach the wire" logic on this — during the SUBSCRIBE→first-frame window commands DO reach
    /// the wire, so gating on first-frame/streaming would wrongly report those as dropped. Invoked on `queue`.
    var onSubscriptionState: (@Sendable (Bool) -> Void)?

    init(transport: RawBulkTransport) {
        self.transport = transport
        transport.setReadHandler { [weak self] chunk in self?.onRead(chunk) }
    }

    // MARK: - Lifecycle

    // #34 boot-race fix: HELLO is retransmitted until the box ACKs; SUBSCRIBE + heartbeat are GATED
    // on that ACK. Without this, an app that connects while the box gadget is still settling loses
    // its one-shot HELLO, SUBSCRIBEs into the void, and sits at a misleading "Waiting for phone…"
    // (bit repeatedly on 2026-07-12 after box reboots). All state below is confined to `queue`.
    private var helloAcked = false
    private var connectGeneration = 0

    /// Connect: start the pipe, then HELLO→(ACK)→SUBSCRIBE→heartbeat, retrying HELLO as needed.
    func connect() {
        transport.start()
        queue.async { [weak self] in
            guard let self else { return }
            self.helloAcked = false
            self.connectGeneration &+= 1
            self.helloLoop(generation: self.connectGeneration, attempt: 0)
        }
    }

    /// On `queue`. Fast phase: HELLO every 500 ms for ~15 s; then flag "box not ready" once and drop
    /// to a 2 s cadence forever (the box may legitimately still be booting — ~50 s cold).
    private func helloLoop(generation: Int, attempt: Int) {
        guard generation == connectGeneration else { return } // superseded by disconnect/reconnect
        if helloAcked {
            subscribe()
            startHeartbeat()
            return
        }
        if attempt == 30 {
            log.error("no HELLO_ACK after 15s — box absent or still booting; retrying every 2s")
            delegate?.ocbmBoxNotReady()
        }
        sendHello()
        let delay: TimeInterval = attempt < 30 ? 0.5 : 2.0
        queue.asyncAfter(deadline: .now() + delay) { [weak self] in
            self?.helloLoop(generation: generation, attempt: attempt + 1)
        }
    }

    func disconnect() {
        // Confine the timer cancel + generation bump + helloAcked reset to `queue` (cancels any
        // in-flight helloLoop and keeps those flags race-free; the heartbeat timer is CREATED on
        // `queue` by startHeartbeat, so it must be cancelled there too — cancelling from the caller
        // thread was a data race on `heartbeatTimer`). ASYNC, not sync (audit #7): a main-thread
        // `queue.sync` here re-introduces the exact UI freeze the STOP semaphore path below was
        // written to avoid — if `queue` is mid read/op, the caller (main) blocks until it drains.
        // `queue` never blocks back on main (its delegate hops are all async), so this is
        // deadlock-free, and the serial FIFO guarantees the cancel precedes any later timer tick.
        // These flags only gate the helloLoop chain (already guarded by the generation compare), and
        // this async is enqueued BEFORE the STOP async block below, so ordering is preserved without
        // blocking the caller.
        queue.async { [weak self] in
            guard let self else { return }
            self.stopHeartbeat()
            self.connectGeneration &+= 1 // cancels any in-flight helloLoop chain
            self.helloAcked = false
        }
        // Send STOP best-effort, but NEVER freeze the (main-thread) caller waiting for it. On a healthy
        // pipe WritePipeTO returns in ~1 ms, so the semaphore is signalled and we proceed immediately
        // with STOP delivered. On a wedged/unplugged pipe WritePipeTO can park for its full
        // 2 s + 5 s (× retry) budget — which previously froze the UI for up to ~14 s during teardown
        // (unplug / Reset / quit). Bound the wait to a short grace; the parked write sits on
        // `writeQueue` (NOT the caller), and `transport.stop()` below AbortPipes the endpoint so that
        // write returns kIOReturnAborted right after. `seq`/`subscribed` stay confined to `queue`.
        let sent = DispatchSemaphore(value: 0)
        let stopSent = Mutex<Bool?>(nil) // nil = wasn't subscribed (no STOP owed)
        queue.async { [weak self] in
            if let self, self.subscribed {
                self.subscribed = false
                // Best-effort — the box also resets CH_LOG to disabled on its own STOP/host-gone path,
                // but sending it explicitly here means an app-initiated STOP never races that.
                self.send(channel: OCBM.chCtrl, payload: OCBM.logCtl(enabled: false, capKB: 0))
                let ok = self.send(channel: OCBM.chCtrl, payload: [OCBM.ctStop])
                stopSent.withLock { $0 = ok }
            }
            sent.signal()
        }
        switch sent.wait(timeout: .now() + 0.3) {
        case .success:
            switch stopSent.withLock({ $0 }) {
            case true?:
                log.info("STOP delivered")
            case false?:
                log.error("STOP write FAILED — box will tear down via its heartbeat watchdog instead")
            case nil:
                log.info("disconnect: not subscribed — no STOP owed")
            }
        case .timedOut:
            log.warning("STOP abandoned after 300ms — write still in flight; transport.stop() aborts it")
        }
        transport.stop()
    }

    /// Per-process, non-zero instance nonce sent in HELLO bytes 2..6 — mirrors `ocbm-host`'s
    /// `host_instance_nonce()` (`host/ocbm-host/src/main.rs:146-156`). Fixed for the process lifetime so
    /// `ocbmd`'s `CT_HELLO` arm (`ccpa/ocbmd/src/main.rs:2446-2473`, `if inst != 0`) can detect a host
    /// RELAUNCH (new nonce) vs. a retransmitted HELLO from the same host instance (same nonce). A
    /// permanent zero — the previous behavior — was a no-op for that replacement detection.
    /// `.random(in: 1...UInt32.max)` already excludes 0, so no extra floor is needed.
    private static let instanceNonce: UInt32 = .random(in: 1...UInt32.max)

    private func sendHello() {
        // [CT_HELLO][version][instance nonce u32 LE] — the box replies HELLO_ACK (drained by the read
        // handler).
        let n = Self.instanceNonce
        let p: [UInt8] = [OCBM.ctHello, OCBM.version,
                           UInt8(n & 0xff), UInt8((n >> 8) & 0xff), UInt8((n >> 16) & 0xff), UInt8(n >> 24)]
        send(channel: OCBM.chCtrl, payload: p)
    }

    /// Push a NEW config to a box we are already connected to, by re-sending SUBSCRIBE.
    ///
    /// Without this, Settings ▸ Save changed only what the box would receive on the NEXT connection:
    /// `sessionConfig` was assigned once at connect and never again, so every lever the box reads out
    /// of the pushed YAML was inert for the life of a session. `android_auto` made that visible —
    /// turning Android Auto off did nothing until the app was relaunched, no matter what the box did
    /// with the key (docs/androidauto/02_ARBITRATION.md F3).
    ///
    /// REFUSED while a CarPlay transport owns the box. ocbmd's re-SUBSCRIBE deliberately dips
    /// `/tmp/host_present` for 2 s (`rearm_presence_silently`) so the shell supervisor sees a
    /// GONE→PRESENT edge and re-spawns airplayd — harmless when idle or on AA (aa-bridge confirms
    /// host absence over 5 s and rides it out), but on a live CarPlay session that is a session
    /// restart. A settings Save must not bounce the screen someone is driving by; those changes keep
    /// the old behaviour and apply on the next connection.
    /// Runs on `queue` (where every other `subscribe()` caller already runs) and never blocks the
    /// caller: this is invoked from the main thread on a Settings Save, and a `queue.sync` here would
    /// freeze the UI behind an in-flight USB write.
    func repushConfig(_ data: Data) {
        let mode = projMode
        queue.async { [weak self] in
            guard let self else { return }
            self.sessionConfig = data
            guard self.subscribed else { return }  // not connected — applies at the next connect
            if mode == OCBM.pmWiredCp || mode == OCBM.pmWirelessCp {
                self.log.info("config change held back — \(OCBM.projModeName(mode), privacy: .public) owns the box; applies on the next connection")
                return
            }
            self.log.info("config changed (\(data.count) B) — re-SUBSCRIBE to apply it to the live box")
            self.subscribe()
        }
    }

    private func subscribe() {
        var p: [UInt8] = [OCBM.ctSubscribe]
        p.append(contentsOf: [UInt8](sessionConfig))
        let ok = send(channel: OCBM.chCtrl, payload: p)
        // C2: only claim subscribed if the write actually landed. A failed SUBSCRIBE leaves the box
        // uncommanded, so telling the UI/senders we're subscribed would silently swallow every control.
        // When it fails we stay unsubscribed and the 1 Hz heartbeat tick re-sends SUBSCRIBE next second.
        subscribed = ok
        onSubscriptionState?(ok)
        if ok {
            log.info("SUBSCRIBE sent (\(self.sessionConfig.count) B config) — commanding box projection")
            // R2 (quick-relaunch grace): a within-grace session REUSE keeps the box's host_present flag
            // at 1, so airplayd's 0→1 keyframe edge never fires for this fresh decoder — it would sit on
            // undecodable P-frames. Proactively request an IDR on both lanes. Harmless on a cold launch
            // (airplayd forces one on the real 0→1 edge; requestKeyframe is ≤1/500ms throttled) and a
            // decode-failure fallback still backstops any race.
            requestKeyframe()
            requestAltKeyframe()
            // Clock sync FIRST (device-proven 2026-09-03: the box booted with its clock at 2020-01-02 and
            // every box log line — ocbmd's read-time stamps and the daemons' write-time stamps — carried
            // that date; the Android client has always sent CT_SETTIME here, the macOS client never did).
            sendSetTime()
            // Re-arm CH_LOG every fresh SUBSCRIBE — the box drops it to disabled on STOP/host-gone
            // (docs/carplay/01_OCBM_PROTOCOL.md CH_LOG), so caching "already sent" here would leave the
            // stream dark after any re-subscribe (SEV_HOST_GONE, Settings ▸ Save, etc).
            if logStreamEnabled {
                send(channel: OCBM.chCtrl, payload: OCBM.logCtl(enabled: true, capKB: logStreamCapKB))
            }
        } else {
            log.error("SUBSCRIBE write FAILED (\(self.sessionConfig.count) B config) — box was NOT commanded to project; heartbeat will retry")
        }
    }

    /// CT_SETTIME: hand the box the host's wall clock (unix seconds, u64 LE). Sent on the caller's queue
    /// right after SUBSCRIBE; the box acks `[0x05][secs][status]` (see docs/carplay/01_OCBM_PROTOCOL.md).
    private func sendSetTime() {
        let secs = UInt64(Date().timeIntervalSince1970)
        var p: [UInt8] = [OCBM.ctSetTime]
        for i in 0..<8 { p.append(UInt8((secs >> (8 * UInt64(i))) & 0xff)) }
        _ = send(channel: OCBM.chCtrl, payload: p)
        log.info("CT_SETTIME sent (\(secs, privacy: .public))")
    }

    /// Arm/disarm the box's CH_LOG stream mid-session (e.g. a live Settings toggle). Also sent
    /// automatically after every successful SUBSCRIBE when `logStreamEnabled` is true — see
    /// `subscribe()`. No-op when not subscribed (the box has no session to arm).
    func sendLogCtl(enabled: Bool, capKB: UInt16) {
        queue.async { [weak self] in
            guard let self else { return }
            guard self.subscribed else { return }
            self.send(channel: OCBM.chCtrl, payload: OCBM.logCtl(enabled: enabled, capKB: capKB))
        }
    }

    /// App-commanded mid-session radio switch (CH_CTRL `ctRadio`, docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating). `false`
    /// powers the box radios (BT + WiFi AP) off NOW without touching a live wired session; `true`
    /// clears the inhibit and the pushed config governs again. Radios-on is otherwise implied by
    /// SUBSCRIBE + config push, and the box powers radios off itself on app loss.
    /// NOTE: not yet called — wiring to the Settings wireless toggle was deferred (plan_A C4);
    /// the box-side surface is complete and this is the seam the toggle will call.
    @discardableResult
    func sendRadio(_ on: Bool) -> Bool {
        send(channel: OCBM.chCtrl, payload: [OCBM.ctRadio, on ? 1 : 0])
    }

    /// The user's answer to the wireless SSP Numeric-Comparison prompt (CH_CTRL `ctPairConfirm`).
    /// `true` = the codes match, pair; `false` = cancel, which also makes the box raise its
    /// pair-rejected flag so the reconnect driver backs off instead of retrying into the same prompt.
    ///
    /// Off the calling thread (`send` does a BLOCKING bulk write) so a click cannot stall the UI, and
    /// unguarded by `subscribed` on purpose — the box only publishes a code to a subscribed host, so
    /// the guard could only ever refuse an answer the user really gave. The box waits ~55 s and then
    /// answers NO itself, so a dropped send costs the pairing attempt, never a hang.
    func sendPairConfirm(accept: Bool) {
        queue.async { [weak self] in
            guard let self else { return }
            let ok = self.send(channel: OCBM.chCtrl, payload: [OCBM.ctPairConfirm, accept ? 1 : 0])
            if ok {
                self.log.info("pair confirm sent: \(accept ? "PAIR" : "CANCEL", privacy: .public)")
            } else {
                self.log.error("pair confirm \(accept ? "PAIR" : "CANCEL", privacy: .public) FAILED to write — the box will time the prompt out")
            }
        }
    }

    private func startHeartbeat() {
        // Defensive: cancel any existing timer first so two callers of connect() without an
        // intervening disconnect() can't leave two live 1 Hz timers running (releasing an active
        // resumed dispatch source without cancel() is legal but leaks a second ticker).
        stopHeartbeat()
        let t = DispatchSource.makeTimerSource(queue: queue)
        t.schedule(deadline: .now() + 1.0, repeating: 1.0)
        t.setEventHandler { [weak self] in
            guard let self else { return }
            // Heartbeat STARVATION detector. The tick is 1 Hz; a materially larger gap means this
            // queue was blocked behind USB writes (see `aaWriteQueue`), which is how ocbmd's 10 s
            // grace gets missed and the whole AA session is torn down from the box side. Log the gap
            // and the AA backlog that caused it — silent starvation was undiagnosable before.
            let now = Date()
            let gap = now.timeIntervalSince(self.lastHeartbeatTick)
            self.lastHeartbeatTick = now
            let peak = self.aaBacklogPeak.withLock { p -> Int in let v = p; p = 0; return v }
            if gap > 3.0 {
                self.log.error("heartbeat gap \(String(format: "%.1f", gap), privacy: .public)s (AA backlog peak \(peak, privacy: .public)) — ocbmd cuts at 10s")
            } else if peak > 16 {
                self.log.info("AA write backlog peak \(peak, privacy: .public) in the last second")
            }
            // C2: bounded SUBSCRIBE retry that RIDES this 1 Hz timer (no new timer — cancelled on
            // disconnect, so the retry is session-bounded). If we're not subscribed — the first
            // SUBSCRIBE write failed, or a SEV_HOST_GONE dropped us — re-send SUBSCRIBE this tick
            // instead of a heartbeat (SUBSCRIBE also stamps liveness box-side) and try again next tick
            // until it lands.
            guard self.subscribed else { self.subscribe(); return }
            self.send(channel: OCBM.chCtrl, payload: [OCBM.ctHeartbeat])
            // Log the decrypt tally only when it moves — a fixed 1 Hz line would flood /tmp during idle.
            // One consistent snapshot under one lock (six unsynchronized reads of read-queue-mutated
            // counters were torn/racy).
            let s = self.av.statsSnapshot()
            if s.videoOK != self.lastVideoOK || s.videoFail != self.lastVideoFail
                || s.audioOK != self.lastAudioOK || s.audioFail != self.lastAudioFail
                || s.altOK != self.lastAltOK || s.altFail != self.lastAltFail {
                self.log.info("A/V decrypt — video ok=\(s.videoOK, privacy: .public) fail=\(s.videoFail, privacy: .public), audio ok=\(s.audioOK, privacy: .public) fail=\(s.audioFail, privacy: .public), alt ok=\(s.altOK, privacy: .public) fail=\(s.altFail, privacy: .public)")
                self.lastVideoOK = s.videoOK; self.lastVideoFail = s.videoFail
                self.lastAudioOK = s.audioOK; self.lastAudioFail = s.audioFail
                self.lastAltOK = s.altOK; self.lastAltFail = s.altFail
            }
            self.delegate?.ocbmDidUpdateStats(
                video: (s.videoOK, s.videoFail),
                audio: (s.audioOK, s.audioFail))
        }
        t.resume()
        heartbeatTimer = t
    }

    /// On `queue` ONLY — `heartbeatTimer` is created (startHeartbeat via helloLoop) and cancelled
    /// exclusively on `queue`; disconnect() hops here rather than calling from the caller thread.
    private func stopHeartbeat() { heartbeatTimer?.cancel(); heartbeatTimer = nil }

    // MARK: - HID input uplink (task #20)

    /// Send a single-touch event to the box (→ ocbmd → airplayd → iPhone `hidSendReport`). `nx`/`ny` are
    /// normalized 0..1 (letterbox-corrected by `CarPlayView`); the box scales them to the advertised
    /// resolution, so the host never needs to know it. Confined to `queue` for `seq` safety, and only
    /// sent while subscribed (no session ⇒ the box has no event channel and would drop it anyway).
    func sendTouch(phase: UInt8, nx: Float32, ny: Float32, finger: UInt8 = 0) {
        let xi = UInt16((max(0, min(1, nx)) * 65535).rounded())
        let yi = UInt16((max(0, min(1, ny)) * 65535).rounded())
        let payload: [UInt8] = [
            OCBM.inputTouch, phase,
            UInt8(xi & 0xFF), UInt8(xi >> 8),
            UInt8(yi & 0xFF), UInt8(yi >> 8),
            finger,
        ]
        queue.async { [weak self] in
            guard let self else { return }
            guard self.subscribed else { self.noteInputDrop("touch"); return }
            self.send(channel: OCBM.chInput, payload: payload)
        }
    }

    /// Send a media-key tap (task #35): the box turns the Consumer array `index` into a press+release
    /// pair on the advertised media-buttons HID device (uid 2). Same confinement/gating as sendTouch.
    func sendMediaButton(_ index: UInt8, completion: (@MainActor @Sendable (SendOutcome) -> Void)? = nil) {
        queue.async { [weak self] in
            guard let self else { Self.deliver(completion, .droppedNotSubscribed); return }
            guard self.subscribed else {
                self.noteInputDrop("media key index=\(index)")
                Self.deliver(completion, .droppedNotSubscribed)
                return
            }
            self.log.info("media key index=\(index, privacy: .public)")
            let ok = self.send(channel: OCBM.chInput, payload: [OCBM.inputMediaBtn, index])
            Self.deliver(completion, ok ? .sent : .writeFailed)
        }
    }

    /// Send a mapped /command (task #35) — Siri hold (cmdSiriDown/Up), cluster content (cmdNav*),
    /// limitedUI (cmdLimitedUI*). The box owns the encrypted event channel, so it does the actual
    /// `{type:...}` dispatch.
    func sendCommand(_ cmd: UInt8, completion: (@MainActor @Sendable (SendOutcome) -> Void)? = nil) {
        queue.async { [weak self] in
            guard let self else { Self.deliver(completion, .droppedNotSubscribed); return }
            guard self.subscribed else {
                self.noteInputDrop("command 0x\(String(cmd, radix: 16))")
                Self.deliver(completion, .droppedNotSubscribed)
                return
            }
            self.log.info("command 0x\(String(cmd, radix: 16), privacy: .public)")
            let ok = self.send(channel: OCBM.chInput, payload: [OCBM.inputCommand, cmd])
            Self.deliver(completion, ok ? .sent : .writeFailed)
        }
    }

    /// Cluster appearance toggles (speed limit / compass / ETA) — sends the 3-byte
    /// `[inputCommand, cmdNavAppearance, flags]`; the box rebuilds the current cluster surface's showUI
    /// URL from `flags` and re-showUIs it. `flags` is a bitmask of `OCBM.navAp*`.
    func sendNavAppearance(_ flags: UInt8, completion: (@MainActor @Sendable (SendOutcome) -> Void)? = nil) {
        queue.async { [weak self] in
            guard let self else { Self.deliver(completion, .droppedNotSubscribed); return }
            guard self.subscribed else {
                self.noteInputDrop("navAppearance")
                Self.deliver(completion, .droppedNotSubscribed)
                return
            }
            self.log.info("navAppearance flags=0x\(String(flags, radix: 16), privacy: .public)")
            let ok = self.send(channel: OCBM.chInput, payload: [OCBM.inputCommand, OCBM.cmdNavAppearance, flags])
            Self.deliver(completion, ok ? .sent : .writeFailed)
        }
    }

    /// Display appearance (Light/Dark). `stream` = `OCBM.appearanceStreamMain`/`Alt`; `dark` picks
    /// AppearanceMode. `isMap` sends `mapAppearanceUpdate` (map content) vs `uiAppearanceUpdate` (UI).
    /// The box does the actual `{type:...}` dispatch on the encrypted event channel.
    func sendAppearance(stream: UInt8, dark: Bool, isMap: Bool,
                        completion: (@MainActor @Sendable (SendOutcome) -> Void)? = nil) {
        let cmd = isMap ? OCBM.cmdMapAppearance : OCBM.cmdUIAppearance
        let mode = dark ? OCBM.appearanceModeDark : OCBM.appearanceModeLight
        queue.async { [weak self] in
            guard let self else { Self.deliver(completion, .droppedNotSubscribed); return }
            guard self.subscribed else {
                self.noteInputDrop("appearance")
                Self.deliver(completion, .droppedNotSubscribed)
                return
            }
            self.log.info("appearance cmd=0x\(String(cmd, radix: 16), privacy: .public) stream=\(stream, privacy: .public) dark=\(dark, privacy: .public)")
            let ok = self.send(channel: OCBM.chInput, payload: [OCBM.inputCommand, cmd, stream, mode])
            Self.deliver(completion, ok ? .sent : .writeFailed)
        }
    }

    /// Global night mode — `setNightMode{nightMode:on}`. One input (with iOS's own logic) into whether
    /// the CarPlay UI goes dark; distinct from the explicit per-display appearance above.
    func sendNightMode(_ on: Bool, completion: (@MainActor @Sendable (SendOutcome) -> Void)? = nil) {
        queue.async { [weak self] in
            guard let self else { Self.deliver(completion, .droppedNotSubscribed); return }
            guard self.subscribed else {
                self.noteInputDrop("nightMode")
                Self.deliver(completion, .droppedNotSubscribed)
                return
            }
            self.log.info("nightMode \(on, privacy: .public)")
            let ok = self.send(channel: OCBM.chInput,
                               payload: [OCBM.inputCommand, OCBM.cmdNightMode, on ? 1 : 0])
            Self.deliver(completion, ok ? .sent : .writeFailed)
        }
    }

    /// Send a Telephony HID button (uid 5): Answer/End/Flash/Mute or a DTMF digit. The box emits the
    /// 1-byte HID report `[index]` then a release `[0]`. Requires "Telephony buttons" armed in Settings.
    func sendTelephony(_ index: UInt8, completion: (@MainActor @Sendable (SendOutcome) -> Void)? = nil) {
        queue.async { [weak self] in
            guard let self else { Self.deliver(completion, .droppedNotSubscribed); return }
            guard self.subscribed else {
                self.noteInputDrop("telephony")
                Self.deliver(completion, .droppedNotSubscribed)
                return
            }
            self.log.info("telephony idx=\(index, privacy: .public)")
            let ok = self.send(channel: OCBM.chInput, payload: [OCBM.inputTelephony, index])
            Self.deliver(completion, ok ? .sent : .writeFailed)
        }
    }

    /// Send a Knob report (uid 4): the CarPlay Simulator's real navigation device. `flags` bit0 Select /
    /// bit1 Home / bit2 Back; `nudgeX`/`nudgeY` are signed (±127 = a 4-way arrow); `rotation` is a signed
    /// relative delta (±1 per detent). The box sends this report then an all-zero release.
    func sendKnob(flags: UInt8, nudgeX: Int8, nudgeY: Int8, rotation: Int8,
                  completion: (@MainActor @Sendable (SendOutcome) -> Void)? = nil) {
        queue.async { [weak self] in
            guard let self else { Self.deliver(completion, .droppedNotSubscribed); return }
            guard self.subscribed else {
                self.noteInputDrop("knob")
                Self.deliver(completion, .droppedNotSubscribed)
                return
            }
            let ok = self.send(channel: OCBM.chInput,
                               payload: [OCBM.inputKnob, flags, UInt8(bitPattern: nudgeX),
                                         UInt8(bitPattern: nudgeY), UInt8(bitPattern: rotation)])
            Self.deliver(completion, ok ? .sent : .writeFailed)
        }
    }

    /// Send a D-Pad nav action (uid 3): the box taps Apple's exact HIDDPad report. Distinct from the
    /// media buttons (uid 2) and from a rotary knob (a separate wheel device we don't advertise).
    func sendNav(_ nav: UInt8, completion: (@MainActor @Sendable (SendOutcome) -> Void)? = nil) {
        queue.async { [weak self] in
            guard let self else { Self.deliver(completion, .droppedNotSubscribed); return }
            guard self.subscribed else {
                self.noteInputDrop("nav \(nav)")
                Self.deliver(completion, .droppedNotSubscribed)
                return
            }
            self.log.info("nav \(nav, privacy: .public)")
            let ok = self.send(channel: OCBM.chInput, payload: [OCBM.inputNav, nav])
            Self.deliver(completion, ok ? .sent : .writeFailed)
        }
    }

    /// Video-loss recovery (task #33): ask the box to request an iOS keyframe after the host detects a
    /// frame gap. Throttled to ≤1/500 ms so a burst of gaps can't spam ForceKeyFrame (a single IDR
    /// repaints the whole screen anyway). Confined to `queue`; no-op when not subscribed.
    private var lastKeyframeReqNs: UInt64 = 0
    func requestKeyframe() {
        queue.async { [weak self] in
            guard let self else { return }
            guard self.subscribed else { self.noteInputDrop("keyframe"); return }
            let now = DispatchTime.now().uptimeNanoseconds
            if now &- self.lastKeyframeReqNs < 500_000_000 { return }
            self.lastKeyframeReqNs = now
            self.send(channel: OCBM.chInput, payload: [OCBM.inputKeyframe])
        }
    }

    /// Same as `requestKeyframe`, but for the ALT/cluster (nav) lane. The bare `inputKeyframe` only
    /// re-IDRs the main console box-side (VideoStream.Main); the cluster (VideoStream.Alt1) needs its
    /// own stream addressed or it stays frozen after a nav view switch. Its OWN throttle, so a main
    /// gap and an alt gap firing in the same 500 ms window don't suppress each other.
    private var lastAltKeyframeReqNs: UInt64 = 0
    func requestAltKeyframe() {
        queue.async { [weak self] in
            guard let self else { return }
            guard self.subscribed else { self.noteInputDrop("keyframe-alt"); return }
            let now = DispatchTime.now().uptimeNanoseconds
            if now &- self.lastAltKeyframeReqNs < 500_000_000 { return }
            self.lastAltKeyframeReqNs = now
            self.send(channel: OCBM.chInput, payload: [OCBM.inputKeyframeAlt])
        }
    }

    // MARK: - Mic uplink

    /// Ship one captured mic chunk to the box over CH_MIC — S16LE PCM at the gate's negotiated
    /// rate/channels, or, when the gate asked for `OCBM.seamCodecMsbc`, one whole 60-byte mSBC eSCO
    /// packet (H2 header + 57-byte frame + pad) that the box writes to the SCO socket verbatim. ocbmd relays it to airplayd, which RTP-uplinks it to the iPhone on the active
    /// type-100 `input` SETUP. Confined to `queue` for `seq` safety, like the HID uplink; only sent while
    /// subscribed (no session ⇒ the box has no uplink armed and drops it). The capture is itself gated by
    /// `onUplinkGate`, so this is a no-op stream between Siri/call turns.
    func sendMicPCM(_ pcm: Data) {
        guard !pcm.isEmpty else { return }
        // Counted HERE, not in MicCapture: this is the CH_MIC lane specifically. An AA session drives a
        // SECOND MicCapture whose frames go to the phone over AA channel 9 and must not be folded in.
        Self.micTxFrames.wrappingAdd(1, ordering: .relaxed)
        let bytes = [UInt8](pcm)
        queue.async { [weak self] in
            guard let self else { return }
            guard self.subscribed else { self.noteInputDrop("mic pcm"); return }
            // CH_MIC is a boundary-less PCM byte stream, so an oversize capture chunk can be split
            // across frames safely — never hand frame() a payload over maxPayload (C8).
            var i = 0
            while i < bytes.count {
                let end = min(i + OCBM.maxPayload, bytes.count)
                self.send(channel: OCBM.chMic, payload: Array(bytes[i..<end]))
                i = end
            }
        }
    }

    // MARK: - App-driven SETUP control relay (plan P3)

    /// Ship one seam-framed control-relay message (RS_RESP / RS_ERR) to the box over CH_RTSP. The relay
    /// (OCBMControlRelay) has already framed it as `[u32 BE "RTSP"][u32 BE len][msg]`; ocbmd is a dumb
    /// byte pipe, so an over-maxPayload frame is split across OCBM frames here (like CH_MIC) — the box's
    /// seam reassembler stitches them back. Confined to `queue` for `seq` safety. NOT gated on
    /// `subscribed`: the relay is only ever active mid-session, and the box needs the answer within its
    /// rpc timeout or it falls back to local — either way the phone never sees a relay-caused error.
    func sendControlRelay(_ framed: [UInt8]) {
        guard !framed.isEmpty else { return }
        queue.async { [weak self] in
            guard let self else { return }
            var i = 0
            while i < framed.count {
                let end = min(i + OCBM.maxPayload, framed.count)
                self.send(channel: OCBM.chRtsp, payload: Array(framed[i..<end]))
                i = end
            }
        }
    }

    // MARK: - I/O

    // Consecutive writeBulk failures (confined to `queue`, like `seq`). Every host→box message
    // funnels through `send`, so this is the single choke point where a dying OUT pipe shows up
    // host-side; the transport escalates its own 5-failure streak to a disconnect.
    private var consecutiveSendFailures = 0

    /// Serializes ONLY the seq counter + failure tally, so `send()` is safe to call from both the
    /// control `queue` (heartbeat/subscribe) and the separate `aaWriteQueue` (Android Auto CH_IP). The
    /// blocking `writeBulk` runs OUTSIDE this lock — the whole point is that a stalled/backlogged AA
    /// write must never hold up the heartbeat. seq being briefly non-monotonic across the two writers
    /// is harmless: the wire seq is "global across channels; debug only" (reassembly uses SOM/EOM +
    /// per-channel buffers, never seq).
    private let sendLock = NSLock()

    /// Frame + write one OCBM message. Returns whether the bulk write succeeded. Thread-safe.
    @discardableResult
    private func send(channel: UInt16, payload: [UInt8]) -> Bool {
        // Belt-and-braces alongside frame()'s precondition (C8): an oversize header would make the
        // box's reassembler treat the frame as junk and byte-resync through the whole payload.
        guard payload.count <= OCBM.maxPayload else {
            log.error("send REFUSED: payload \(payload.count, privacy: .public) B > maxPayload \(OCBM.maxPayload, privacy: .public) ch=0x\(String(channel, radix: 16), privacy: .public)")
            return false
        }
        sendLock.lock()
        let s = seq
        seq &+= 1
        sendLock.unlock()
        let f = OCBM.frame(channel: channel, flags: OCBM.fSom | OCBM.fEom, seq: s, payload: payload)
        let ok = transport.writeBulk(f)   // blocking USB write — deliberately NOT under sendLock
        sendLock.lock()
        if ok {
            consecutiveSendFailures = 0
        } else {
            consecutiveSendFailures += 1
            log.error("writeBulk FAILED ch=0x\(String(channel, radix: 16), privacy: .public) op=0x\(String(payload.first ?? 0, radix: 16), privacy: .public) seq=\(s, privacy: .public) len=\(payload.count, privacy: .public) consecutive=\(self.consecutiveSendFailures, privacy: .public)")
        }
        sendLock.unlock()
        return ok
    }

    // C9: throttled visibility for frames that silently fell through the switch defaults — a
    // truncated SEV_HOST_GONE (short ctSessionEvent) or an unknown op/channel used to vanish
    // without a trace. Confined to the transport read queue (onRead/handleCtrl/handleMgmt all
    // run there), ≤1 line/s.
    private var lastUnhandledLogNs: UInt64 = 0
    private func logUnhandled(channel: UInt16, _ payload: [UInt8]) {
        let now = DispatchTime.now().uptimeNanoseconds
        guard now &- lastUnhandledLogNs >= 1_000_000_000 else { return }
        lastUnhandledLogNs = now
        log.warning("unhandled frame ch=0x\(String(channel, radix: 16), privacy: .public) op=0x\(String(payload.first ?? 0, radix: 16), privacy: .public) len=\(payload.count, privacy: .public)")
    }

    // C3: an input the client swallowed because the box has no session yet used to vanish silently.
    // Count every drop and log ≤1 line/s (same throttle shape as logUnhandled). Confined to `queue`
    // (every input sender hops here before touching it), a SEPARATE counter from logUnhandled's so the
    // two never race across the control `queue` / transport read queue boundary.
    private var lastInputDropLogNs: UInt64 = 0
    private var inputDropCount: UInt64 = 0
    private func noteInputDrop(_ what: String) {
        inputDropCount &+= 1
        let now = DispatchTime.now().uptimeNanoseconds
        guard now &- lastInputDropLogNs >= 1_000_000_000 else { return }
        lastInputDropLogNs = now
        log.warning("input dropped — no session: \(what, privacy: .public) (total dropped=\(self.inputDropCount, privacy: .public))")
    }

    // C3: deliver a UI-facing send outcome EXACTLY ONCE on the main queue. Static so every return path —
    // including the `guard let self` failure when the client deallocated mid-send — can call it without a
    // live `self`. Main queue is the MainActor executor, so assumeIsolated is sound and keeps the callback
    // FIFO-ordered with the rest of the UI's main-queue work.
    private static func deliver(_ completion: (@MainActor @Sendable (SendOutcome) -> Void)?, _ outcome: SendOutcome) {
        guard let completion else { return }
        DispatchQueue.main.async { MainActor.assumeIsolated { completion(outcome) } }
    }

    private func onRead(_ chunk: [UInt8]) {
        reasm.push(chunk)
        while let frame = reasm.next() {
            switch frame.channel {
            // fNewSource: the box accepted a NEW producer on that seam and dropped the previous one
            // without draining it, so the decrypt layer's reassembly buffer for this channel may hold a
            // partial message that will never be completed. Surface the bit so it resets the buffer
            // BEFORE appending; otherwise the new producer's SEAM_KEY lands mid-message and the lane
            // desyncs for the rest of the session (2026-09-02, media audio on a re-SETUP).
            case OCBM.chVideo:
                av.feedVideo(frame.payload, newSource: frame.flags & OCBM.fNewSource != 0)
            case OCBM.chAltVideo:
                av.feedAltVideo(frame.payload, newSource: frame.flags & OCBM.fNewSource != 0)
            case OCBM.chMediaAudio:
                av.feedAudio(frame.payload, newSource: frame.flags & OCBM.fNewSource != 0)
            case OCBM.chAltAudio:
                av.feedVoice(frame.payload, newSource: frame.flags & OCBM.fNewSource != 0)
            case OCBM.chMetadata:
                onMetadata?(frame.payload)
            case OCBM.chRtsp:
                onControlRelay?(frame.payload)
            case OCBM.chIp:
                handleIp(frame.payload)
            case OCBM.chCtrl:
                handleCtrl(frame.payload)
            case OCBM.chMgmt:
                handleMgmt(frame.payload)
            case OCBM.chLog:
                handleLog(frame.payload)
            default:
                // ECHO/CONSOLE/etc — not used by the A/V receiver, but say so (throttled).
                logUnhandled(channel: frame.channel, frame.payload)
            }
        }
    }

    // MARK: - CH_IP stream-mux relay (Android Auto transport)
    // Sub-frame = [type u8][conn_id u16 LE][data]; mirrors ocbmd's handle_ip (IP_OPEN/IP_DATA/IP_CLOSE).
    // The box connect()s IP_OPEN's "host:port" target and relays the byte stream both ways over CH_IP.

    /// Open a relayed stream to `target` ("host:port") tagged with `id`. AA uses this to reach the
    /// box's aa-bridge (e.g. "127.0.0.1:5277").
    func ipOpen(id: UInt16, target: String) {
        aaWriteQueue.async { [weak self] in
            guard let self else { return }
            var p: [UInt8] = [OCBM.ipOpen, UInt8(id & 0xff), UInt8(id >> 8)]
            p.append(contentsOf: Array(target.utf8))
            _ = self.send(channel: OCBM.chIp, payload: p)
        }
    }

    /// How many CH_IP writes are queued but not yet on the wire. `aaWriteQueue` keeps AA traffic off
    /// the heartbeat's dispatch, but every frame still funnels into USBTransport's ONE serial write
    /// queue, where a stalled pipe can block ~1.5 s. Droppable AA traffic (mic PCM) checks this so it
    /// cannot pile up behind a stall and starve the writes that are NOT droppable — video/audio ACKs
    /// and the 1 Hz heartbeat, whose loss makes the box declare the host gone.
    private let aaPending = Mutex<Int>(0)
    var aaWriteBacklog: Int { aaPending.withLock { $0 } }

    /// Fires when a CH_IP write FAILS to reach the wire. The AA stream is TLS: a dropped frame is a
    /// hole in the record sequence that nothing can repair, so the peer's decrypt fails and it tears
    /// the session down with no protocol teardown — a freeze with no diagnosis. USBTransport
    /// deliberately does not retry a timed-out write (a retry could duplicate partially-sent bytes),
    /// so the only sound response is to fail the stream FAST and let it be rebuilt. Lock-guarded like
    /// `onIpData` above — same mid-session (re)assignment hazard.
    var onIpWriteFailed: ((UInt16) -> Void)? {
        get { ipCallbackLock.lock(); defer { ipCallbackLock.unlock() }; return _onIpWriteFailed }
        set { ipCallbackLock.lock(); _onIpWriteFailed = newValue; ipCallbackLock.unlock() }
    }
    private var _onIpWriteFailed: ((UInt16) -> Void)?

    /// Write stream bytes to conn `id`. Split into ≤maxPayload IP_DATA sub-frames (header = 3 B).
    func ipWrite(id: UInt16, _ bytes: [UInt8]) {
        let depth = aaPending.withLock { $0 += 1; return $0 }
        aaBacklogPeak.withLock { if depth > $0 { $0 = depth } }
        // Pre-emptive fail-fast. A backlog this deep means the USB pipe is stalling (each write can
        // take up to 1.5 s), and the consequence downstream is worse than stopping: gearhead's ACK
        // watchdog kills the session after ~400 blocked sends (~7-13 s of starved ACKs) via a path
        // that sends NO ByeBye and leaves the phone bounced out of accessory mode. Failing the stream
        // here costs a ~2 s clean rebuild instead.
        if depth > Self.aaBacklogFailAt {
            log.error("CH_IP backlog \(depth, privacy: .public) — pipe stalling, failing the AA stream before the phone's ACK watchdog does")
            onIpWriteFailed?(id)
            aaPending.withLock { $0 -= 1 }
            return
        }
        aaWriteQueue.async { [weak self] in
            guard let self else { return }
            defer { self.aaPending.withLock { $0 -= 1 } }
            let cap = OCBM.maxPayload - 3
            var off = 0
            while off < bytes.count {
                let end = min(off + cap, bytes.count)
                var p: [UInt8] = [OCBM.ipData, UInt8(id & 0xff), UInt8(id >> 8)]
                p.append(contentsOf: bytes[off..<end])
                if !self.send(channel: OCBM.chIp, payload: p) {
                    self.log.error("CH_IP write FAILED (conn \(id, privacy: .public)) — stream corrupt, failing it")
                    self.onIpWriteFailed?(id)
                    return
                }
                off = end
            }
        }
    }

    /// Close conn `id`.
    func ipClose(id: UInt16) {
        aaWriteQueue.async { [weak self] in
            guard let self else { return }
            _ = self.send(channel: OCBM.chIp, payload: [OCBM.ipClose, UInt8(id & 0xff), UInt8(id >> 8)])
        }
    }

    /// High-water backlog since the last report, logged by the heartbeat tick.
    private let aaBacklogPeak = Mutex<Int>(0)
    /// Backlog at which we declare the pipe stalled. ~2 s of AA ACK traffic (50+/s): well past any
    /// healthy burst, well before gearhead's 400-frame watchdog.
    private static let aaBacklogFailAt = 96
    /// Wall clock of the last heartbeat tick (confined to `queue`), for the starvation detector.
    private var lastHeartbeatTick = Date()

    /// Wait, up to `timeout`, for the CH_IP writes queued so far to reach the wire.
    ///
    /// `ipWrite`/`ipClose` are fire-and-forget onto `aaWriteQueue`, so a teardown that shuts the AA
    /// session down and then immediately stops the USB transport can abort the pipe with the AA
    /// BYEBYE still queued — leaving the phone holding a stale session, which breaks its NEXT attach
    /// (docs/androidauto/02_ARBITRATION.md Phase 1). Draining first fixes that.
    ///
    /// It is TIME-BOUNDED, not a plain `aaWriteQueue.sync {}` barrier, because the caller is the main
    /// actor and the queue's work is blocking USB I/O: each queued write can sit in `WritePipeTO` for
    /// up to ~1.5 s, and a failed write is dropped without closing the pipe, so the NEXT one waits the
    /// same again. Exactly when this runs — the box hung or the cable pulled mid-AA — there can be a
    /// backlog of them, which as an unbounded barrier froze the UI (and beat `applicationWillTerminate`'s
    /// own ~2 s bound) for as long as the pipe took to give up. A missed BYEBYE costs one stale phone
    /// session; a frozen main thread costs the whole app.
    func drainIpWrites(timeout: TimeInterval = 0.5) {
        let drained = DispatchSemaphore(value: 0)
        aaWriteQueue.async { drained.signal() }
        _ = drained.wait(timeout: .now() + timeout)
    }

    private func handleIp(_ payload: [UInt8]) {
        guard payload.count >= 3 else { return }
        let type = payload[0]
        let id = UInt16(payload[1]) | (UInt16(payload[2]) << 8)
        switch type {
        case OCBM.ipData:
            onIpData?(id, Array(payload[3...]))
        case OCBM.ipClose:
            onIpData?(id, []) // empty payload == EOF for that stream
        default:
            break
        }
    }

    private func handleCtrl(_ payload: [UInt8]) {
        guard let op = payload.first else { return }
        switch op {
        case OCBM.ctHelloAck:
            log.info("HELLO_ACK from box")
            // Runs on the transport read queue; hop to `queue` where helloAcked is confined. The
            // in-flight helloLoop tick (≤500 ms away) observes it and proceeds to SUBSCRIBE.
            queue.async { [weak self] in self?.helloAcked = true }
        case OCBM.ctUplink where payload.count >= 7:
            // [ctUplink][state u8][rate u32 LE][ch u8]([codec u8]) — the box's mic-uplink gate.
            // The codec byte was appended 2026-09-04 for HFP wideband and is OPTIONAL: a 7-byte
            // payload is the pre-existing PCM form and must keep meaning exactly that, which is also
            // why the box still sends OFF as the 7-byte all-zero shape. Never index payload[7]
            // without the count check — a truncated frame would otherwise crash the read queue.
            let on = payload[1] != 0
            let rate = UInt32(payload[2]) | (UInt32(payload[3]) << 8)
                | (UInt32(payload[4]) << 16) | (UInt32(payload[5]) << 24)
            let ch = payload[6]
            let codec: UInt8 = payload.count >= 8 ? payload[7] : 0
            let codecName = codec == OCBM.seamCodecMsbc ? "mSBC" : (codec == 0 ? "PCM" : "codec \(codec)")
            log.info("mic uplink gate: \(on ? "ON" : "OFF", privacy: .public) \(rate, privacy: .public)Hz \(ch, privacy: .public)ch \(codecName, privacy: .public)")
            onUplinkGate?(on, on ? rate : 0, on ? ch : 0, on ? codec : 0)
        case OCBM.ctPairingCode:
            // [ctPairingCode][6 ascii digits | empty] — the wireless SSP Numeric-Comparison code.
            let digits = String(bytes: payload.dropFirst(), encoding: .ascii)?
                .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            log.info("pairing code: \(digits.isEmpty ? "cleared" : digits, privacy: .public)")
            onPairingCode?(digits.isEmpty ? nil : digits)
        case OCBM.ctProjMode where payload.count >= 2:
            // [ctProjMode][pm*] — the box telling us which transport owns it (docs/androidauto/02_ARBITRATION.md).
            let mode = payload[1]
            log.info("projection mode: \(OCBM.projModeName(mode), privacy: .public)")
            projMode = mode
            projModeSeq &+= 1
            onProjectionMode?(mode, projModeSeq)
        case OCBM.ctBtPhase where payload.count >= 2:
            // [ctBtPhase][BTP_*] — Bluetooth/iAP2 handshake progress; advisory (never gate on
            // ordering — docs/carplay/01_OCBM_PROTOCOL.md).
            let phase = payload[1]
            log.info("[box] bt phase: \(OCBM.btPhaseName(phase), privacy: .public)")
            onBtPhase?(phase)
        case OCBM.ctPhoneIdent:
            // [ctPhoneIdent][utf8 JSON | empty] — who the connected phone is; empty payload = cleared.
            let body = Array(payload.dropFirst())
            if body.isEmpty {
                log.info("[box] phone: cleared")
                onPhoneIdent?(nil)
            } else if let ident = try? JSONDecoder().decode(PhoneIdent.self, from: Data(body)) {
                log.info("[box] phone: \(ident.model, privacy: .public) \(ident.osName, privacy: .public) \(ident.osVersion, privacy: .public)")
                onPhoneIdent?(ident)
            } else {
                // Malformed JSON — never trap; throttled visibility like every other unhandled case.
                logUnhandled(channel: OCBM.chCtrl, payload)
            }
        case OCBM.ctBoxHealth where payload.count >= 2:
            // [ctBoxHealth][BH_* bitmask] — the box's own readiness; re-emitted on change / after a
            // fresh SUBSCRIBE, so the app never needs to cache this across a reconnect.
            let bits = payload[1]
            log.info("[box] health: \(OCBM.boxHealthNames(bits), privacy: .public)")
            onBoxHealth?(bits)
        case OCBM.ctSessionEvent where payload.count >= 2:
            switch payload[1] {
            case OCBM.sevPhonePresent, OCBM.sevPhoneAbsent:
                let phone = payload[1] == OCBM.sevPhonePresent
                log.info("box session event: phone \(phone ? "PRESENT" : "ABSENT", privacy: .public)")
                delegate?.ocbmPhonePresence(present: phone)
            case OCBM.sevHostPresent, OCBM.sevHostGone:
                let present = payload[1] == OCBM.sevHostPresent
                log.info("box session event: \(present ? "PRESENT" : "GONE", privacy: .public)")
                delegate?.ocbmSessionEvent(present: present)
                if !present {
                    // Box declared the host GONE (its heartbeat watchdog expired while we were
                    // stalled, e.g. laptop sleep/App-Nap) and set ITS side subscribed=false. Our
                    // heartbeat timer keeps running, but the box now ignores heartbeats until a
                    // fresh SUBSCRIBE, so without this the session sits at "Waiting for phone…"
                    // forever. Re-project by re-sending SUBSCRIBE. Confined to `queue`, where
                    // subscribed/helloAcked live, so `seq` and those flags stay race-free.
                    queue.async { [weak self] in
                        guard let self else { return }
                        // C2: the box set ITS subscribed=false, so ours is now a lie. Drop it and tell
                        // the UI BEFORE re-subscribing — during the re-project window controls would be
                        // silently swallowed, and the UI must report that truthfully. The heartbeat tick
                        // also re-sends SUBSCRIBE while unsubscribed, so this is belt-and-braces.
                        self.subscribed = false
                        self.onSubscriptionState?(false)
                        guard self.helloAcked else { return }
                        self.log.info("host GONE — re-subscribing to re-establish projection")
                        self.subscribe()
                    }
                }
            default:
                // Undefined SEV_* byte (payload has no checksum beyond the frame's `hcheck`) — log and
                // ignore rather than falling into "not present"/HOST GONE. Previously the `default:` arm
                // computed `present = (payload[1] == sevHostPresent)`, so this and a real SEV_HOST_GONE
                // were indistinguishable and both dropped `subscribed` + re-SUBSCRIBEd.
                logUnhandled(channel: OCBM.chCtrl, payload)
            }
        default:
            // Unknown op, or a KNOWN op whose payload was too short for its `where` guard (a
            // truncated ctUplink/ctSessionEvent lands here) — surface it, throttled.
            logUnhandled(channel: OCBM.chCtrl, payload)
        }
    }

    // MARK: - CH_LOG (the box's universal log stream)

    // Confined to the transport read queue (onRead runs there); the box's own per-channel u16 counter,
    // not shared with `seq` above.
    private var lastLogSeq: UInt16?
    private var lastLogParseWarnNs: UInt64 = 0

    /// Decode one CH_LOG frame and hand it to `onBoxLog`. `parseLogEntries` is pure/bounds-checked
    /// (OCBMFraming.swift); a malformed remainder is logged here (throttled) rather than in the
    /// decoder, and never traps. A `seq` gap is loss — synthesize a `logSourceTailer` marker entry
    /// immediately ahead of the entry that revealed it, so the combined log shows exactly where.
    private func handleLog(_ payload: [UInt8]) {
        let entries = parseLogEntries(payload)
        let consumed = entries.reduce(0) { $0 + 14 + $1.rawLen }
        if consumed < payload.count {
            let now = DispatchTime.now().uptimeNanoseconds
            if now &- lastLogParseWarnNs >= 1_000_000_000 {
                lastLogParseWarnNs = now
                log.warning("CH_LOG malformed remainder (\(payload.count - consumed, privacy: .public) B) dropped")
            }
        }
        guard !entries.isEmpty else { return }
        var out: [LogEntry] = []
        out.reserveCapacity(entries.count)
        for e in entries {
            if let last = lastLogSeq, e.seq != last &+ 1 {
                let nowMs = UInt64(Date().timeIntervalSince1970 * 1000)
                // Stamped with `e.source` (NOT logSourceTailer) — the gap is per-source, and the box
                // line that revealed it tells us which per-daemon log lost the range.
                out.append(LogEntry(source: e.source, flags: 0, seq: e.seq, unixMs: nowMs,
                                     text: "seq gap \(last) -> \(e.seq)", droppedCount: nil, rawLen: 0,
                                     isGapMarker: true))
            }
            lastLogSeq = e.seq
            out.append(e)
        }
        onBoxLog?(out)
    }

    // MARK: - CH_MGMT (the "CCPA" tab)

    // NB: these hop to `queue` before `send` (whose `seq` mutation is actually guarded by `sendLock`,
    // not `queue`), exactly like sendTouch/sendCommand/etc — the hop is for ordering with the rest of
    // the control plane on `queue`, not for `seq` safety.
    /// Ask the box for a fresh info/health snapshot (reply arrives on `onBoxInfo`).
    func requestBoxInfo() { mgmtSend([OCBM.mgmtGetInfo]) }
    /// Reboot the adapter (box ACKs, then reboots). Reply on `onBoxAck`.
    func boxReboot() { mgmtSend([OCBM.mgmtReboot]) }
    /// Clear ALL paired BT devices + restart wireless. Reply on `onBoxAck`.
    func boxForgetAll() { mgmtSend([OCBM.mgmtForgetAll]) }
    /// Forget one paired device by MAC ("AA:BB:.."). Reply on `onBoxAck`.
    func boxForgetDevice(_ mac: String) { mgmtSend([OCBM.mgmtForgetDevice] + Array(mac.utf8)) }
    /// Restart the wireless stack (re-advertise CarLink) without a full reboot. Reply on `onBoxAck`.
    func boxRestartWireless() { mgmtSend([OCBM.mgmtRestartWireless]) }
    /// Put the adapter into NCM maintenance mode: the box arms `/script/ncm_only` and reboots. Sticky —
    /// the operator returns it over ssh (`rm /script/ncm_only; reboot`). Reply on `onBoxAck`, then the
    /// OCBM link drops.
    func boxEnterNCM() { mgmtSend([OCBM.mgmtEnterNCM]) }

    private func mgmtSend(_ payload: [UInt8]) {
        queue.async { [weak self] in self?.send(channel: OCBM.chMgmt, payload: payload) }
    }

    private func handleMgmt(_ payload: [UInt8]) {
        guard let op = payload.first else { return }
        switch op {
        case OCBM.mgmtInfo:
            let json = Data(payload.dropFirst())
            let info = try? JSONDecoder().decode(CCPAInfo.self, from: json)
            if info == nil { log.error("CCPA info: JSON decode failed") }
            onBoxInfo?(info)
        case OCBM.mgmtAck where payload.count >= 3:
            log.info("CCPA action ack: verb \(payload[1], privacy: .public) status \(payload[2], privacy: .public)")
            onBoxAck?(payload[1], payload[2])
        default:
            // Unknown mgmt op or a truncated mgmtAck — surface it, throttled.
            logUnhandled(channel: OCBM.chMgmt, payload)
        }
    }
}

/// Identity of the connected phone (CH_CTRL `CT_PHONE_IDENT`, `{name,deviceID,model,osName,osVersion}`
/// lifted from its own AirPlay phase-1 SETUP plist). Property names match the box's JSON keys exactly.
struct PhoneIdent: Codable, Equatable {
    let name: String
    let deviceID: String
    let model: String
    let osName: String
    let osVersion: String
}

/// A snapshot of CCPA adapter identity + health + bonded devices (the box's `MGMT_INFO` JSON). Property
/// names match the box's JSON keys exactly (no CodingKeys needed).
struct CCPAInfo: Codable, Equatable {
    struct Daemons: Codable, Equatable {
        let ocbmd: Bool
        let iap2d: Bool
        let airplayd: Bool
        let carplay_wireless: Bool
    }
    let bt_mac: String
    let wifi_mac: String
    let serial: String
    let name: String
    let uptime_s: Int
    let rootfs_pct: Int
    let rootfs_free_kb: Int
    let ssp: Bool
    let hci_up: Bool
    let wlan_ap: Bool
    let transport: String
    let host_present: Bool
    let phone_present: Bool
    let daemons: Daemons
    let devices: [String]
}
