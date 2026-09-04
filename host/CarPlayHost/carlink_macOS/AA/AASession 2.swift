// AASession.swift — the Android Auto head-unit session state machine, in Swift.
//
// Direct port of the validated Rust reference (host/aa-headunit/main.rs). Runs the
// full flow over an abstract byte transport (TCP now via adb-forward; the box CH_AA
// channel later): VERSION -> encapsulated TLS -> AUTH_COMPLETE -> SERVICE_DISCOVERY
// (advertise the accepted 1.7 service set) -> answer channel-open / audio-focus / ping
// / nav-focus / sensor driving-status gate / video Setup->Config->Focus -> receive
// H.264 (feed the app's VideoDecoder via the Annex-B->AVCC shim) -> ACK each frame ->
// clean BYEBYE. Touch is sent up on the input channel.
//
// The session runs on its own thread (blocking transport). All TLS ops are serialized
// on that thread; touch is enqueued from the UI thread and drained each loop iteration.

import AVFoundation
import Foundation
import os

/// A blocking byte source/sink for the AA session (TCP now, OCBM CH_AA later).
protocol AAByteTransport: AnyObject {
    func readExact(_ n: Int) -> Data?   // nil on EOF/error
    func write(_ data: Data) -> Bool
    func close()
    /// Frames queued but not yet on the wire, for droppable traffic to back off. 0 = unknown/none.
    var writeBacklog: Int { get }
}

extension AAByteTransport {
    var writeBacklog: Int { 0 }
}

final class AASession: @unchecked Sendable {
    /// Shared os.Logger sink for every AA log line — subsystem "com.carlink.app" so
    /// `FileLogger` (App/FileLogger.swift, filters `log.subsystem.hasPrefix("com.carlink")`) actually
    /// captures it. Every AA log line used to go through `NSLog("[AA] ...")`, which carries the
    /// process's OWN subsystem, not ours — FileLogger's OSLogStore poll silently dropped every one,
    /// which is why a whole AA session showed as 4 lines in the combined log despite dozens of call
    /// sites here. NSLog is not removed elsewhere in the app; this just gives AA's lines a subsystem
    /// the aggregator watches, same as AppDelegate/StreamMetricsMonitor already do.
    static let osLog = Logger(subsystem: "com.carlink.app", category: "AA")
    /// Default `log` sink, used by both AppDelegate call sites that construct an `AASession` — replaces
    /// the old `{ NSLog("[AA] \($0)") }` literal. `@Sendable`: `AASession.init` stores this in a
    /// `let` closure property that outlives the (non-Sendable) capturing context, and it is invoked
    /// from the session thread as well as the mic-capture callback thread.
    static let defaultLog: @Sendable (String) -> Void = { msg in osLog.info("\(msg, privacy: .public)") }

    private let transport: AAByteTransport
    private let decoder: VideoDecoder
    private let tls: AATLS
    private let log: (String) -> Void
    /// Set by `shutdown()` — distinguishes a LOCAL, intentional close (app quit, box mode change) from
    /// the phone ending the session on its own, for the "session ended" reason line.
    private var shutdownRequested = false
    /// Set when the PHONE's own BYEBYE_REQUEST was received (as opposed to us sending the first one).
    private var remoteByebye = false
    /// Whether MEDIA_ACK is sent on the sink channels; see the VERSION_RESPONSE handler.
    private var mediaAcksEnabled = true
    /// Decoded metadata-service events (media / navigation / phone), delivered on the session thread.
    var onMetadata: ((AAMetadata.Event) -> Void)?
    /// One-shot: fires on the very first MEDIA_DATA frame, keyframe or not — distinct from the
    /// per-IDR log below, which is silent if the stream somehow never keyframes.
    private var firstVideoFrameLogged = false
    /// Cumulative wire-level byte counters for the 1 Hz "AA stats" line (sendMsg/recvMsg maintain
    /// these) — plaintext-payload bytes, not the 4-byte frame header, close enough for a stats line.
    private var rxBytes = 0
    private var txBytes = 0
    private var lastStatsLog: Date?

    // Per-channel multi-frame reassembly: channel -> (encrypted, accumulated payload).
    private var partial: [UInt8: (Bool, Data)] = [:]
    private var sessionId: Int64 = 0
    private var configured = false
    private var sawKeyframe = false
    private var dataCount = 0
    private var videoWarmShrunk = false

    // Decode-FIFO depths for the AA video lane (see the CODEC_CONFIG handler). The startup depth is
    // deep enough to swallow the display layer's one-time decoder warm-up burst without shedding a
    // single frame (a shed P-frame breaks the chain until AA's next sparse IDR); once `aaWarmupFrames`
    // have decoded the FIFO has drained and we drop to the shallow steady-state depth to stay live.
    private static let aaStartupDecodeDepth = 64
    // Deep, because SHEDDING A FRAME IS THE BUG. Google's own reference head unit (the DHU) queues
    // decoded-but-not-yet-decoded frames in an UNBOUNDED std::list with no cap, no shedding and no
    // back-pressure — its only reaction to depth is a log line. It can afford that because a shed
    // P-frame is unrecoverable: the protocol has no keyframe request, and the phone's I-frame
    // interval is set by the version we REQUEST — 2 s at the 6.1 default, but 60 s under
    // `AA_PROTO=1.7` (< 6.0), which is why shedding still must not happen. Latency from a deep
    // queue is paid only when we are already behind; corruption from a shed frame lasts up to a
    // minute. AA_LEGACY_VIDEO=1 restores the old shallow depth for A/B.
    private static let aaSteadyDecodeDepth = ProcessInfo.processInfo.environment["AA_LEGACY_VIDEO"] == "1" ? 2 : 64
    private static let aaWarmupFrames = 30
    private var byebyeSent = false
    /// Per-channel media session ids + frame counts for the AUDIO sinks (4/5/6). Audio is
    /// flow-controlled per channel exactly like video, so each one needs its OWN session id.
    private var audioSessions: [UInt8: Int64] = [:]
    private var audioFrames: [UInt8: Int] = [:]
    /// Mic SOURCE channel (id 9). We advertise it in the SD response — a service set without it is
    /// rejected outright (`CAR.SERVICE Critical error 2/24 "No audio/mic"`, docs/androidauto/01_SESSION_AND_AV.md) — but nothing
    /// serviced it: an Assistant tap opened the channel and then waited for audio that never came.
    /// `onMicStart`/`onMicStop` let the app drive its real capture; `sendMicPCM` puts frames on 9.
    var onMicStart: ((Double, UInt32) -> Void)?
    var onMicStop: (() -> Void)?
    private var micSessionId: Int64 = 0
    private var micActive = false
    private var micFrames = 0
    private var micDropped = 0
    private var droppedSinceIdr = 0
    private static let legacyVideo = ProcessInfo.processInfo.environment["AA_LEGACY_VIDEO"] == "1"
    /// Can we actually capture? Checked BEFORE answering MicrophoneRequest so a denied mic is declined
    /// with a status the phone understands rather than answered with silence.
    private func micAvailable() -> Bool {
        AVCaptureDevice.authorizationStatus(for: .audio) == .authorized
    }
    /// Diagnostic lever (see the audio-ack call site): 1 = do NOT ack the audio sinks.
    private static let skipAudioAck = ProcessInfo.processInfo.environment["AA_SKIP_AUDIO_ACK"] == "1"

    // Touch queue (UI thread -> session thread).
    private let touchLock = NSLock()
    private var touchQueue: [(UInt32, UInt32, UInt64)] = []
    /// Pending key events, drained on the session thread alongside touches. Same lock: they ride the
    /// same input channel and must keep their order relative to each other.
    private var keyQueue: [(AACapability.Key, Bool)] = []
    /// Pending rotary detents (signed, ±1 per click), drained with the keys.
    private var scrollQueue: [Int32] = []
    /// LIVE sensor state, which starts at the session-start snapshot and then follows the UI. The
    /// phone re-asks on its own sensor requests, and must be answered with the CURRENT value — not
    /// the value captured when the session opened, or a toggle would silently revert on the next ask.
    private let sensorLock = NSLock()
    private var liveNight: Bool?
    private var liveDriving: Bool?
    private let start = Date()

    // Keyframe-recovery flag: the decoder sets it (any thread) when a P-frame chain
    // breaks; the recv loop re-asserts VideoFocus to nudge a fresh IDR from the phone.
    private let kfLock = NSLock()
    private var needKeyframe = false

    // Serializes ALL SSLContext ops — handshake, encrypt and decrypt. The recv loop, the handshake
    // loop, and setNightMode/setDrivingRestricted/sendMicPCM/shutdown from other threads must not
    // touch the context concurrently: SSLContext is not thread-safe, and `AATLS.outbound` is ONE
    // buffer that `encrypt()` drains unconditionally, so an encrypt racing the handshake would ship
    // the handshake's own records out as an ENCRYPTED frame and kill the session.
    //
    // LOCK ORDER: sendLock -> tlsLock, and nothing else is ever acquired while tlsLock is held.
    // tlsLock is a LEAF: never held across a transport write, a recvMsg, or a `log`/callback. That is
    // what keeps the handshake loop below safe — it takes tlsLock for each individual `tls.*` call and
    // drops it before calling sendPlain (which takes sendLock), so the order is never inverted.
    private let tlsLock = NSLock()
    /// True once the handshake has completed and the context can encrypt application data. Guarded by
    /// `tlsLock` so the check and the encrypt are one atomic step. `aaSession` is published to the UI
    /// before `run()` starts, so a night-mode toggle or a quit can land mid-handshake; those sends are
    /// dropped rather than allowed into the context (there is no session to receive them yet).
    private var tlsReady = false
    /// Held across encrypt+write in `sendMsg` (order: sendLock -> tlsLock; the read path takes only
    /// tlsLock, so no cycle). Transport writes are fire-and-forget, so this is never held long.
    private let sendLock = NSLock()

    // Advertised video mode.
    /// What we tell the phone about this head unit. Built from the shared vehicle profile — see
    /// AACapability, which replaced the five constants that used to live here (800x480 / 30 fps /
    /// density 160 / night ALWAYS false / driving ALWAYS unrestricted) while the CarPlay half of the
    /// same app had every one of them app-driven.
    private static let traceUnhandled = ProcessInfo.processInfo.environment["AA_TRACE_UNHANDLED"] != nil
    private let cap: AACapability
    /// Where AA's audio sinks play. Optional so a headless/test session can run without an engine.
    private let audio: AudioPlayer?
    private var videoRes: UInt32 { cap.resolution.rawValue }
    private var videoFps: UInt32 { cap.frameRate.rawValue }
    private var videoW: UInt32 { cap.touchSize.w }
    private var videoH: UInt32 { cap.touchSize.h }

    /// "wired" / "wireless" — the box's projection mode, carried only so the metrics panel can label
    /// the transport. Nothing in the protocol depends on it.
    let transportLabel: String

    init?(transport: AAByteTransport, decoder: VideoDecoder, p12Path: String, p12Password: String,
          capability: AACapability, audio: AudioPlayer? = nil, transportLabel: String = "wired",
          log: @escaping (String) -> Void) {
        guard let tls = AATLS(p12Path: p12Path, password: p12Password) else { return nil }
        self.cap = capability
        self.audio = audio
        self.transport = transport
        self.decoder = decoder
        self.tls = tls
        self.transportLabel = transportLabel
        self.log = log
    }

    /// The touchscreen surface we ADVERTISED, and therefore the coordinate space every touch must
    /// be expressed in. Exposed because the view layer has to scale into it: it is not a constant
    /// any more, it follows the negotiated resolution.
    var touchSurface: (w: UInt32, h: UInt32) { cap.touchSize }

    /// Enqueue a key press or release on the input channel. Thread-safe.
    ///
    /// Only keys in `AACapability.supportedKeycodes` are sendable, because those are the ones the SD
    /// response declared — sending an undeclared keycode claims a button this head unit never said it
    /// had. The type makes that structural: `AACapability.Key` IS the declared set.
    func enqueueKey(_ key: AACapability.Key, down: Bool) {
        touchLock.lock(); keyQueue.append((key, down)); touchLock.unlock()
    }

    /// Push a night-mode change to the phone NOW.
    ///
    /// `SensorBatch` is an INDICATION, not a response — it may be sent at any time, not only when the
    /// phone asks. Until this existed, night and driving status were answered once, reactively, from
    /// a snapshot taken at session start, so flipping the in-app toggle mid-session changed nothing:
    /// AA has no light/dark control of its own, `night_mode` IS the lever gearhead derives its theme
    /// (and Maps' Day/Night "Auto") from, so the toggle was silently inert.
    func setNightMode(_ on: Bool) {
        sensorLock.lock(); liveNight = on; sensorLock.unlock()
        guard sendEnc(AAWire.chSensor, AAWire.sensorBatch, control: false,
                      AAWire.sensorBatchNight(on)) else { return }
        log("-> sensor night=\(on)")
    }

    /// Push a driving-restriction change to the phone NOW. AA gives this ONE bit where CarPlay has
    /// the whole limitedUI catalogue, so this is the nearest equivalent of that toggle.
    func setDrivingRestricted(_ restricted: Bool) {
        let mask: AACapability.DrivingRestrictions = restricted ? .drivingDefault : .none
        sensorLock.lock(); liveDriving = restricted; sensorLock.unlock()
        guard sendEnc(AAWire.chSensor, AAWire.sensorBatch, control: false,
                      AAWire.sensorBatchDriving(mask)) else { return }
        log("-> sensor drivingStatus=0x\(String(mask.rawValue, radix: 16))")
    }

    /// Enqueue a rotary detent. AA carries this as a RelativeEvent delta, not a button press, so it
    /// has no down/up pair. Thread-safe.
    func enqueueScroll(delta: Int32) {
        touchLock.lock(); scrollQueue.append(delta); touchLock.unlock()
    }

    /// Press and release, for a momentary button. AA expects both edges — a down with no up leaves
    /// the key stuck as far as the phone is concerned.
    ///
    /// The release is HELD BACK ~90 ms rather than queued immediately behind the press. Enqueuing
    /// both together drains them in the same pass with effectively the same timestamp, i.e. a
    /// zero-duration press, which Android's input plumbing is entitled to treat as noise. 90 ms is a
    /// short human tap and well under the long-press threshold.
    func tapKey(_ key: AACapability.Key) {
        enqueueKey(key, down: true)
        DispatchQueue.global().asyncAfter(deadline: .now() + 0.09) { [weak self] in
            self?.enqueueKey(key, down: false)
        }
    }

    /// Enqueue a touch, in the advertised touchscreen space (see `touchSurface`). Thread-safe.
    func enqueueTouch(x: UInt32, y: UInt32, action: UInt64) {
        touchLock.lock(); touchQueue.append((x, y, action)); touchLock.unlock()
    }

    /// Best-effort clean teardown (called at app quit): send BYEBYE, then drop the
    /// socket so the recv loop ends. Prevents the phone's "Already connected" stale state.
    func shutdown() {
        shutdownRequested = true
        // Called from ANY thread (app quit, stopAAOverOCBM) against a session thread that may still be
        // mid-handshake. Before the handshake completes there is no session to say goodbye to, so skip
        // straight to closing the transport — which is what actually ends the recv loop.
        tlsLock.lock(); let ready = tlsReady; tlsLock.unlock()
        if ready && claimByebye() {
            _ = sendEnc(AAWire.chControl, AAWire.msgByebyeRequest, control: false, AAWire.byebyeRequest())
        }
        transport.close()
        // Hide the AA rows in Settings immediately rather than leaving the last tick's rates frozen
        // on screen. The monitor also ages the snapshot out, this just makes it instant.
        StreamMetricsMonitor.publishAA(nil)
    }

    /// Test-and-set `byebyeSent`: true for the ONE caller that gets to send the BYEBYE. Written from
    /// `shutdown()` on an arbitrary thread and from the session thread, hence the lock.
    private func claimByebye() -> Bool {
        touchLock.lock(); defer { touchLock.unlock() }
        if byebyeSent { return false }
        byebyeSent = true
        return true
    }

    // MARK: - Send helpers

    /// Largest plaintext message we will hand to `sendMsg` (see the guard there).
    private static let maxMessagePayload = 60 * 1024

    /// Serializes ENCRYPT+WRITE as one step. `tlsLock` alone is not enough: two senders could each
    /// encrypt under it and then write in the other order, putting TLS records on the wire out of
    /// sequence — the phone's decrypt fails and the session dies. Needed now that the mic channel
    /// sends from the capture callback rather than only from the session thread.
    private func sendMsg(_ channel: UInt8, _ msgid: UInt16, encrypted: Bool, control: Bool, _ body: Data) -> Bool {
        sendLock.lock(); defer { sendLock.unlock() }
        var payload = Data([UInt8(msgid >> 8), UInt8(msgid & 0xff)])
        payload.append(body)
        // A frame length is u16 and we only ever emit BULK frames (no fragmentation), so an oversize
        // message is a caller bug — it must be an error, not a trap in `UInt16(...)` inside
        // encodeFrame. Checked on the PLAINTEXT, BEFORE encrypting: dropping a message after
        // `tls.encrypt` has already advanced the cipher state would leave a HOLE in the TLS stream
        // the phone can never recover from. 60 KiB leaves room for TLS record overhead (at most four
        // 16 KiB records, ~64 B each) to stay inside 65535. Nothing we send is near it — the SD
        // response is a few hundred bytes and a mic buffer a few KB.
        guard payload.count <= Self.maxMessagePayload else {
            log("send DROPPED: ch=\(channel) id=\(msgid) — \(payload.count) B exceeds the frame limit")
            return false
        }
        let framePayload: Data
        if encrypted {
            // The ready-check and the encrypt are ONE critical section: checking outside the lock
            // leaves a window where the handshake finishes (or hasn't) between the two.
            tlsLock.lock()
            let ct = tlsReady ? tls.encrypt(payload) : nil
            tlsLock.unlock()
            guard let ct else { return false }
            framePayload = ct
        } else {
            framePayload = payload
        }
        guard framePayload.count <= 0xFFFF else {   // belt and braces: see the plaintext guard above
            log("send DROPPED: ch=\(channel) id=\(msgid) — \(framePayload.count) B ciphertext exceeds u16")
            return false
        }
        txBytes += framePayload.count
        return transport.write(AAWire.encodeFrame(channel: channel, encrypted: encrypted, control: control, payload: framePayload))
    }

    @discardableResult private func sendEnc(_ ch: UInt8, _ id: UInt16, control: Bool, _ body: Data) -> Bool {
        sendMsg(ch, id, encrypted: true, control: control, body)
    }
    @discardableResult private func sendPlain(_ ch: UInt8, _ id: UInt16, control: Bool, _ body: Data) -> Bool {
        sendMsg(ch, id, encrypted: false, control: control, body)
    }

    /// Rolling window of the last frame headers seen, for the decrypt-failure dump in `recvMsg`.
    private var frameTrace: [String] = []

    /// Upper bound on one reassembled multi-frame message — same 64 MiB as the Rust reference
    /// (host/aa-headunit/src/main.rs `MAX_REASSEMBLED`).
    private static let maxReassembled = 64 * 1024 * 1024

    // MARK: - Receive (framing + reassembly + decrypt)

    /// Receive one full message; decrypt if the frame was ENCRYPTED. Returns
    /// (channel, wasEncrypted, msgid, body) or nil on EOF/error.
    private func recvMsg() -> (UInt8, Bool, UInt16, Data)? {
        while true {
            // Keep the last few frame headers. When decrypt fails there is nothing in any log saying
            // WHAT arrived — the failure is reported by SSLRead, several layers below the framing —
            // so an intermittent errSSLDecryptionFail (device-observed at 57 s / 62 s / 94 s into a
            // session, on a byte stream the transport counters prove arrived complete and in order)
            // cannot be attributed to a channel, a fragmentation event, or a frame size. This is the
            // cheapest thing that can: dump the trace at the moment the context dies.
            guard let hdr = transport.readExact(2) else { return nil }
            let channel = hdr[hdr.startIndex]
            let flags = hdr[hdr.startIndex + 1]
            let frameType = flags & AAWire.ftBulk
            let encrypted = (flags & AAWire.encEncrypted) != 0
            defer { if frameTrace.count > 8 { frameTrace.removeFirst(frameTrace.count - 8) } }

            guard let sz = transport.readExact(2) else { return nil }
            let frameLen = Int(sz[sz.startIndex]) << 8 | Int(sz[sz.startIndex + 1])
            if frameType == AAWire.ftFirst {
                guard transport.readExact(4) != nil else { return nil } // u32 total (ignored)
            }
            guard let payload = transport.readExact(frameLen) else { return nil }
            rxBytes += frameLen
            frameTrace.append("ch=\(channel) flags=0x\(String(flags, radix: 16)) len=\(frameLen)"
                              + (frameType == AAWire.ftFirst ? " FIRST" : "")
                              + (frameType == AAWire.ftBulk ? " BULK" : "")
                              + ((flags & AAWire.ftLast) != 0 && frameType != AAWire.ftBulk ? " LAST" : ""))

            // DECRYPT IN ARRIVAL ORDER, ACCUMULATE PLAINTEXT — never the other way round.
            //
            // There is ONE TLS stream shared by every channel, so its ciphertext must be fed in the
            // order it arrived. Reassembling a fragmented message's CIPHERTEXT first (what this code
            // and host/aa-headunit both used to do, following aasdk) withholds the FIRST fragment's
            // bytes while feeding the next frame's — and the phone DOES interleave: device trace,
            // 2026-08-27, `ch=3 flags=0x9 len=16149 FIRST` immediately followed by
            // `ch=4 flags=0xb len=8231 BULK`, which then failed to decrypt. Out-of-order ciphertext
            // is an errSSLDecryptionFail the context never recovers from.
            //
            // That the phone interleaves at all proves encryption is PER FRAME, not per message: no
            // receiver could decrypt a per-message record run split around another channel's frame.
            // So decrypt each frame as it lands and let the per-channel buffer hold PLAINTEXT. A
            // frame whose ciphertext ends mid-record simply yields nothing here — AATLS keeps the
            // remainder buffered and the next frame completes it — which is why this is also strictly
            // more robust than the old path, not just correctly ordered.
            //
            // Symptom this fixes: AA died at an unpredictable 57–94 s into every session, exactly
            // when a >16 KB message fragmented and another channel interleaved before its LAST.
            let clearPayload: Data
            if encrypted {
                tlsLock.lock(); let d = tls.decrypt(payload); tlsLock.unlock()
                guard let d else {                // real TLS failure -> end the session
                    log("decrypt FAILED on ch=\(channel) (\(payload.count) B frame). " +
                        "Last frames: \(frameTrace.joined(separator: " | "))")
                    return nil
                }
                clearPayload = d
            } else {
                clearPayload = payload
            }

            let clear: Data
            if frameType == AAWire.ftBulk {
                if clearPayload.isEmpty { continue }  // partial record -> read on
                clear = clearPayload
            } else if frameType == AAWire.ftFirst {
                partial[channel] = (encrypted, clearPayload)
                continue
            } else {
                guard var entry = partial[channel] else { continue } // stray continuation
                entry.1.append(clearPayload)
                // Bound the reassembly, matching the Rust reference (main.rs MAX_REASSEMBLED). A peer
                // that streams FIRST..MIDDLE and never sends LAST grows this without limit, and the
                // read-stall timer never fires because data IS still arriving. End the session, as the
                // reference does — there is no way back to a coherent message stream from here.
                if entry.1.count > Self.maxReassembled {
                    partial.removeValue(forKey: channel)
                    log("reassembled message on ch=\(channel) exceeded \(Self.maxReassembled) B — ending the session")
                    return nil
                }
                partial[channel] = entry
                if flags & AAWire.ftLast != 0 {
                    clear = entry.1; partial.removeValue(forKey: channel)
                    if clear.isEmpty { continue }
                } else {
                    continue
                }
            }
            let (id, body) = AAWire.splitMessageId(clear)
            return (channel, encrypted, id, body)
        }
    }

    // MARK: - Run

    private func flagKeyframe() { kfLock.lock(); needKeyframe = true; kfLock.unlock() }

    /// Run the whole session on the calling thread (caller runs it off-main).
    func run() {
        defer { transport.close() }

        // Video-loss recovery: when the decoder drops a frame / needs an IDR, ask the
        // phone for a fresh keyframe (re-assert VideoFocus) to clear pixelation fast.
        decoder.onNeedsKeyFrame = { [weak self] in self?.flagKeyframe() }
        decoder.onFrameDropped = { [weak self] in self?.flagKeyframe() }

        // 1. VERSION_REQUEST (plain): [major u16 BE][minor u16 BE].
        var ver = Data()
        ver.append(UInt8(AAWire.versionMajor >> 8)); ver.append(UInt8(AAWire.versionMajor & 0xff))
        ver.append(UInt8(AAWire.versionMinor >> 8)); ver.append(UInt8(AAWire.versionMinor & 0xff))
        log("-> VERSION_REQUEST \(AAWire.versionMajor).\(AAWire.versionMinor)")
        guard sendPlain(AAWire.chControl, AAWire.msgVersionRequest, control: false, ver) else { return }

        guard let (_, _, vid, vbody) = recvMsg(), vid == AAWire.msgVersionResponse else {
            log("expected VERSION_RESPONSE"); return
        }
        if vbody.count >= 6 {
            let major = Int(vbody[vbody.startIndex]) << 8 | Int(vbody[vbody.startIndex + 1])
            let minor = Int(vbody[vbody.startIndex + 2]) << 8 | Int(vbody[vbody.startIndex + 3])
            let status = Int(vbody[vbody.startIndex + 4]) << 8 | Int(vbody[vbody.startIndex + 5])
            log("<- VERSION_RESPONSE \(major).\(minor) status=\(status)")
            // gearhead 17.5 constructs its video/audio endpoints with acks DISABLED when the negotiated
            // protocol is >= 6.0 (`ivc.c`: `new jbq(carInfo.e, carInfo.f).t(6, 0)` -> `jdk.o`); every
            // MEDIA_ACK we then send falls through its dispatch and is logged as "Received message
            // with invalid type header: 32772" — 180k lines in one morning on this Pixel, for nothing.
            // We ask for 6.1 (the 2 s IDR interval), so acks are off unless the phone answers < 6.0.
            mediaAcksEnabled = major < 6
            if !mediaAcksEnabled { log("media acks disabled — protocol \(major).\(minor) >= 6.0, gearhead ignores them") }
            if status == 0xFFFF { log("version mismatch — abort"); return }
        }

        // 2. Encapsulated TLS handshake.
        log("starting encapsulated TLS handshake")
        // Every `tls.*` call here takes tlsLock INDIVIDUALLY and drops it before sendPlain/recvMsg.
        // Holding it across sendPlain would invert the sendLock -> tlsLock order established in
        // sendMsg and deadlock; holding it across recvMsg would deadlock on tlsLock itself (recvMsg
        // takes it to decrypt). Application sends can't interleave here regardless — `tlsReady` is
        // still false, so sendMsg drops them — but the lock is what makes that true for the
        // handshake's own records rather than merely likely.
        while true {
            tlsLock.lock(); let st = tls.handshakeStep(); tlsLock.unlock()
            guard let st else { log("TLS handshake failed"); return }
            while true {
                tlsLock.lock(); let out = tls.takeOutbound(); tlsLock.unlock()
                guard let out else { break }
                guard sendPlain(AAWire.chControl, AAWire.msgEncapsulatedSSL, control: false, out) else { return }
            }
            if st == .done { break }
            guard let (_, _, mid, sslBody) = recvMsg(), mid == AAWire.msgEncapsulatedSSL else {
                log("expected ENCAPSULATED_SSL"); return
            }
            tlsLock.lock(); tls.feedInbound(sslBody); tlsLock.unlock()
        }
        // describe() reads the context too, so take it under the same lock — and BEFORE tlsReady goes
        // true, after which another thread's encrypt may be running.
        tlsLock.lock(); let tlsDesc = tls.describe(); tls.logPeerCertificateIssuer(); tlsReady = true; tlsLock.unlock()
        log("TLS handshake COMPLETE: \(tlsDesc)")

        // 3. AUTH_COMPLETE (plain).
        log("-> AUTH_COMPLETE")
        guard sendPlain(AAWire.chControl, AAWire.msgAuthComplete, control: false, AAWire.authResponseSuccess()) else { return }

        // 4. SERVICE_DISCOVERY_REQUEST (encrypted) then our response.
        guard let (_, _, sdid, _) = recvMsg(), sdid == AAWire.msgServiceDiscoveryRequest else {
            log("expected SERVICE_DISCOVERY_REQUEST"); return
        }
        var declared: [UInt8] = [AAWire.chSensor, AAWire.chVideo, AAWire.chMediaAudio,
                                 AAWire.chGuidanceAudio, AAWire.chSystemAudio]
        if AACapability.telephonySinkExperiment { declared.append(AACapability.telephonySinkChannel) }
        declared.append(contentsOf: [AAWire.chInput, AAWire.chMicrophone])
        if AACapability.metadataServices {
            declared.append(contentsOf: [AAWire.chMediaPlayback, AAWire.chNavigationStatus, AAWire.chPhoneStatus])
        }
        log("<- SERVICE_DISCOVERY_REQUEST; -> SERVICE_DISCOVERY_RESPONSE: "
            + declared.map { "\($0)(\(AAWire.channelName($0)))" }.joined(separator: " "))
        if AACapability.telephonySinkExperiment {
            log("telephony sink DECLARED (experiment AA_TELEPHONY_SINK)")
        }
        log("declaring \(videoW)x\(videoH)@\(cap.frameRate == .fps60 ? 60 : 30), density \(cap.density), "
            + "night=\(cap.nightMode), drivingRestricted=\(cap.drivingRestricted) as \"\(cap.name)\"")
        log("declaring driver_position=\(cap.driverPosition) (rightHandDrive=\(cap.rightHandDrive))")
        if cap.hasMargins {
            log("declaring margins \(cap.margins.w)x\(cap.margins.h) — visible \(cap.touchSize.w)x\(cap.touchSize.h) inside \(cap.resolution.size.w)x\(cap.resolution.size.h); touch in visible space")
        }
        let sd = AAWire.serviceDiscoveryResponseFull(resolution: videoRes, fps: videoFps,
                                                     density: cap.density,
                                                     widthMargin: cap.margins.w, heightMargin: cap.margins.h,
                                                     tsW: videoW, tsH: videoH,
                                                     name: cap.name, driverPosition: cap.driverPosition,
                                                     metadataServices: AACapability.metadataServices,
                                                     hevc: cap.videoCodecHEVC)
        if cap.videoCodecHEVC { log("declaring video codec H.265 (tier \(cap.resolution.rawValue)\(ProcessInfo.processInfo.environment["AA_HEVC"] == "1" ? ", AA_HEVC forced" : ""))") }
        if AACapability.metadataServices { log("metadata services DECLARED (AA_METADATA): media_playback ch=10, navigation_status ch=11 (IMAGE 128x128), phone_status ch=12") }
        guard sendEnc(AAWire.chControl, AAWire.msgServiceDiscoveryResponse, control: false, sd) else { return }

        // 5. Event loop.
        eventLoop()

        // Clean teardown.
        if claimByebye() {
            _ = sendEnc(AAWire.chControl, AAWire.msgByebyeRequest, control: false, AAWire.byebyeRequest())
        }
        let endReason = remoteByebye ? "remote BYEBYE"
            : shutdownRequested ? "local shutdown"
            : "transport closed (stream ended / stall)"
        log("session ended: \(endReason)")
    }

    private func eventLoop() {
        log("waiting for the phone to open channels...")
        while true {
            drainTouch()
            drainKeyframe()
            maybeLogStats()
            guard let (ch, enc, id, body) = recvMsg() else { log("stream ended"); return }

            // Control-channel handshakes handled first.
            if ch == AAWire.chControl && id == AAWire.msgPingRequest {
                let ts = AAWire.getFieldVarint(body, 1) ?? 0
                _ = sendMsg(AAWire.chControl, AAWire.msgPingResponse, encrypted: enc, control: false, AAWire.pingResponse(ts))
                continue
            }
            if ch == AAWire.chControl && id == AAWire.msgNavFocusRequest {
                log("<- NAV_FOCUS_REQUEST -> PROJECTED")
                _ = sendEnc(AAWire.chControl, AAWire.msgNavFocusNotification, control: false, AAWire.navFocusProjected())
                continue
            }
            if ch == AAWire.chControl && id == AAWire.msgAudioFocusRequest {
                let t = AAWire.getFieldVarint(body, 1) ?? AAWire.audioFocusTypeRelease + 1
                let state = (t == AAWire.audioFocusTypeRelease) ? AAWire.audioFocusStateLoss : AAWire.audioFocusStateGain
                _ = sendEnc(AAWire.chControl, AAWire.msgAudioFocusNotification, control: false, AAWire.audioFocusNotification(state))
                continue
            }
            if ch == AAWire.chControl && id == AAWire.msgByebyeRequest {
                remoteByebye = true
                _ = sendEnc(AAWire.chControl, AAWire.msgByebyeResponse, control: false, Data())
                _ = claimByebye()   // the phone said goodbye first; don't send one back on teardown
                return
            }
            // Channel-open on any advertised channel.
            if id == AAWire.msgChannelOpenRequest {
                if ch == AACapability.telephonySinkChannel, AACapability.telephonySinkExperiment,
                   let sink = AACapability.audioSink(forChannel: ch) {
                    // The whole point of the experiment. Nobody has seen gearhead do this; if it ever
                    // happens, say so loudly and print what we declared for the channel — the setup
                    // request that follows carries the config the phone actually picked.
                    log("<- CHANNEL_OPEN ch=\(ch) (telephony) — the phone DOES route call audio over "
                        + "the projection link; declared \(sink.rate)Hz \(sink.channels)ch 16-bit "
                        + "streamType=\(sink.streamType)")
                } else {
                    log("<- CHANNEL_OPEN ch=\(ch) (\(AAWire.channelName(ch)))")
                }
                _ = sendEnc(ch, AAWire.msgChannelOpenResponse, control: true, AAWire.channelOpenResponseOK())
                continue
            }
            // Media setup on a sink: reply Config(READY); video also needs VideoFocus.
            if id == AAWire.mediaSetup {
                _ = sendEnc(ch, AAWire.mediaConfig, control: false, AAWire.mediaConfigReady())
                if ch == AAWire.chVideo {
                    log("-> VIDEO CONFIG ready \(videoW)x\(videoH)@\(cap.frameRate == .fps60 ? 60 : 30) (codec confirmed at CODEC_CONFIG)")
                    // Spontaneous grant after SETUP — flagged unsolicited, as the reference does.
                    _ = sendEnc(AAWire.chVideo, AAWire.mediaVideoFocusNotification, control: false,
                                AAWire.videoFocus(AAWire.videoFocusModeProjected, unsolicited: true))
                    log("-> VIDEO_FOCUS (unsolicited) PROJECTED")
                } else if let sink = AACapability.audioSink(forChannel: ch) {
                    log("-> AUDIO CONFIG ready ch=\(ch) (\(sink.label)) \(sink.rate)Hz \(sink.channels)ch")
                }
                continue
            }

            // Mic SOURCE (9): the phone drives it with Start/Stop and expects US to produce audio.
            if ch == AAWire.chMicrophone {
                switch id {
                case AAWire.micRequest:
                    // {1: open bool}. NOT MediaStart/Stop — a mic SOURCE is opened by its own
                    // MicrophoneRequest, and the phone waits on our MicrophoneResponse.
                    let open = (AAWire.getFieldVarint(body, 1) ?? 0) != 0
                    if open {
                        micSessionId = 1
                        let ok = micAvailable()
                        _ = sendEnc(ch, AAWire.micResponse, control: false,
                                    AAWire.micResponseBody(status: ok ? 0 : 1,
                                                           sessionId: UInt64(micSessionId)))
                        if ok {
                            touchLock.lock(); micActive = true; micFrames = 0; touchLock.unlock()
                            let mic = AACapability.micSource
                            log("<- MIC OPEN — capturing \(mic.rate / 1000) kHz "
                                + "\(mic.channels == 1 ? "mono" : "stereo") (session \(micSessionId))")
                            onMicStart?(Double(mic.rate), UInt32(mic.channels))
                        } else {
                            // Declining explicitly beats streaming silence: the phone would otherwise
                            // sit on an open mic it believes succeeded (jaz.java:17 checks this status).
                            log("<- MIC OPEN but the mic is unavailable — DECLINED (status=1)")
                        }
                    } else {
                        touchLock.lock(); micActive = false; let sent = micFrames; touchLock.unlock()
                        log("<- MIC CLOSE (sent \(sent) frames)")
                        onMicStop?()
                    }
                case AAWire.mediaStop:
                    touchLock.lock(); micActive = false; touchLock.unlock()
                    onMicStop?()
                default:
                    break
                }
                continue
            }

            // Audio sinks: MEDIA(4) / GUIDANCE(5) / SYSTEM(6).
            //
            // These are flow-controlled EXACTLY like video — one `gal.Ack{session_id, ack}` per media
            // frame, against THAT channel's own session id (Google's DHU carries a single shared
            // `gal.Ack` plus a `gal.AudioUnderflowNotification`, i.e. the audio sinks are first-class,
            // not best-effort chatter). We answer their Setup with the same `Config` we send video, so
            // we ADVERTISE a `max_unacked` window on each of them; never acking means the window fills
            // and the channel stalls. Gearhead then tears the whole session down — which the box sees
            // as the phone dropping off the USB bus with no BYEBYE, and the user sees as a freeze a
            // second or two after the UI made a sound. Previously every one of these fell through to
            // `default: break` as "unmodeled chatter".
            // Membership in the DECLARED sink table, not three hardcoded ids — the AA_TELEPHONY_SINK
            // experiment adds a fourth, and a sink we declared but never routed would be acked into
            // silence. With the lever off the table is exactly 4/5/6, so this is unchanged.
            if AACapability.audioSink(forChannel: ch) != nil {
                switch id {
                case AAWire.mediaStart:
                    let sid = Int64(bitPattern: AAWire.getFieldVarint(body, 1) ?? 0)
                    audioSessions[ch] = sid
                    log("<- AUDIO START ch=\(ch) session_id=\(sid)")
                case AAWire.mediaData, AAWire.mediaDataNoTs:
                    // Ack first, like video: our ack RTT is the phone's window.
                    // `AA_SKIP_AUDIO_ACK=1` reproduces the pre-fix behaviour on purpose — it exists so
                    // the root cause can be RE-PROVEN by experiment (drop only the audio acks, change
                    // nothing else, and see whether the session dies) rather than inferred from a
                    // run that changed several things at once.
                    if !Self.skipAudioAck {
                        if mediaAcksEnabled {
                            _ = sendEnc(ch, AAWire.mediaAck, control: false,
                                        AAWire.mediaAckBody(audioSessions[ch] ?? 0))
                        }
                    }
                    // PLAY it. Until now every one of these frames was counted, acked and then
                    // DROPPED — AA video worked while AA audio went nowhere, which is the whole of
                    // phase 2. `mediaData` carries [timestamp u64 BE][PCM] exactly like video, so
                    // strip 8; `mediaDataNoTs` is bare PCM.
                    let pcm: Data = (id == AAWire.mediaData)
                        ? (body.count > 8 ? body.dropFirst(8) : Data())
                        : body
                    if let sink = AACapability.audioSink(forChannel: ch), !pcm.isEmpty {
                        audio?.feedPCM(Data(pcm), rate: sink.rate, channels: sink.channels,
                                       voice: sink.voice, bigEndian: AACapability.pcmIsBigEndian)
                    }
                    let n = (audioFrames[ch] ?? 0) + 1
                    audioFrames[ch] = n
                    if n == 1 || n % 200 == 0 {
                        let label = AACapability.audioSink(forChannel: ch)?.label ?? "?"
                        log("audio ch=\(ch) (\(label)) frames=\(n) \(pcm.count)B/frame")
                    }
                case AAWire.mediaStop:
                    log("<- AUDIO STOP ch=\(ch)")
                    audioSessions[ch] = nil
                default:
                    break
                }
                continue
            }

            switch (ch, id) {
            case (AAWire.chSensor, AAWire.sensorRequest):
                let stype = AAWire.getFieldVarint(body, 1) ?? 0
                _ = sendEnc(AAWire.chSensor, AAWire.sensorResponse, control: false, AAWire.sensorStartResponseOK())
                sensorLock.lock()
                let night = liveNight ?? cap.nightMode
                let driving = liveDriving ?? cap.drivingRestricted
                sensorLock.unlock()
                if stype == AAWire.sensorTypeDrivingStatus {
                    _ = sendEnc(AAWire.chSensor, AAWire.sensorBatch, control: false,
                                AAWire.sensorBatchDriving(driving ? .drivingDefault : .none))
                } else if stype == AAWire.sensorTypeNightMode {
                    _ = sendEnc(AAWire.chSensor, AAWire.sensorBatch, control: false,
                                AAWire.sensorBatchNight(night))
                }
            case (AAWire.chInput, AAWire.inputKeyBindingRequest):
                // Log what the phone actually ASKED FOR before answering OK. We have always replied
                // status=0 without reading the request, so nothing in any log says whether gearhead
                // binds specific keycodes — which is exactly the question when some declared keys work
                // (media, search) and others are ignored (home, back).
                log("<- KEY BINDING REQUEST (\(body.count) B): \(body.map { String(format: "%02x", $0) }.joined())")
                _ = sendEnc(AAWire.chInput, AAWire.inputKeyBindingResponse, control: false, AAWire.keyBindingResponseOK())
            case (AAWire.chVideo, AAWire.mediaVideoFocusRequest):
                log("<- VIDEO_FOCUS_REQUEST -> PROJECTED")
                _ = sendEnc(AAWire.chVideo, AAWire.mediaVideoFocusNotification, control: false, AAWire.videoFocusProjected())
            case (AAWire.chVideo, AAWire.mediaStart):
                sessionId = Int64(bitPattern: AAWire.getFieldVarint(body, 1) ?? 0)
                log("<- MEDIA START (session_id=\(sessionId))")
                // START begins a NEW encoder session: the phone tears its encoder down and builds a
                // fresh one, so a CODEC_CONFIG and an IDR follow. Carrying stale state across that
                // boundary decodes the new stream against the old one's references.
                resetVideoSession()
            case (AAWire.chVideo, AAWire.mediaCodecConfig):
                // Annex-B parameter sets -> configure the decoder (H.264 SPS/PPS, or HEVC VPS/SPS/PPS
                // when we declared H.265 — tiers above 1080p, see AACapability.Resolution.needsHEVC).
                if cap.videoCodecHEVC, let (v, sp, pp) = AVCCFastPath.hevcParameterSetsFromAnnexB(body) {
                    decoder.maxDecodeDepth = Self.aaStartupDecodeDepth
                    decoder.configure(codec: .hevc, parameterSets: [v, sp, pp])
                    configured = true
                    log("<- CODEC_CONFIG (\(body.count) B) — HEVC decoder configured")
                } else if let (s, p) = AVCCFastPath.h264ParameterSetsFromAnnexB(body) {
                    // AA lane, startup: AVSampleBufferDisplayLayer lazily inits its decoder on the
                    // first enqueue (~100 ms). We ACK each video frame on ENQUEUE, not on decode, so
                    // the phone keeps streaming and frames pile up during that one-time warm-up. A
                    // shallow FIFO sheds the overflow — and one shed P-frame breaks the whole P-chain
                    // until AA's next (sparse) IDR, which is the startup flash/pixelation. Open with a
                    // deep FIFO to absorb the entire warm-up burst losslessly (frames drain in order,
                    // none dropped), then shrink to a live steady-state depth once warm (below).
                    // CarPlay never touches maxDecodeDepth; its lane keeps VideoDecoder.defaultQueueDepth (3).
                    decoder.maxDecodeDepth = Self.aaStartupDecodeDepth
                    decoder.configure(codec: .h264, parameterSets: [s, p])
                    configured = true
                    log("<- CODEC_CONFIG (\(body.count) B) — decoder configured")
                }
                if mediaAcksEnabled { _ = sendEnc(AAWire.chVideo, AAWire.mediaAck, control: false, AAWire.mediaAckBody(sessionId)) }
            case (AAWire.chVideo, AAWire.mediaData):
                // ACK the frame FIRST, before any decode work — the video channel is ack-windowed, so
                // minimizing our contribution to the ack RTT keeps the phone's pipeline full (paired
                // with the raised max_unacked in mediaConfigReady).
                if mediaAcksEnabled { _ = sendEnc(AAWire.chVideo, AAWire.mediaAck, control: false, AAWire.mediaAckBody(sessionId)) }
                // [timestamp u64 BE][Annex-B] -> strip 8, convert, decode.
                if body.count > 8 {
                    let au = body.subdata(in: (body.startIndex + 8)..<body.endIndex)
                    if configured, let avcc = AVCCFastPath.annexBToAVCC(au) {
                        let isKF = au.withUnsafeBytes { containsIDR($0) }
                        dataCount += 1
                        if !firstVideoFrameLogged {
                            firstVideoFrameLogged = true
                            log("first video frame received (\(au.count) B, keyframe=\(isKF))")
                        }
                        if isKF {
                            log("IDR frame #\(dataCount) (\(au.count) B)")
                            sawKeyframe = true
                        }
                        // Don't decode leading P-frames before the first IDR — they have
                        // no reference and render as garbage until the stream settles.
                        if sawKeyframe { decoder.decodeAndDisplay(avcc: avcc, keyframe: isKF) }
                        // Warm-up over: once the display layer's decoder has produced enough frames
                        // the deep startup FIFO has drained, so shrink to a live steady-state depth
                        // (keeps latency low while still absorbing minor jitter). One-shot.
                        if !videoWarmShrunk,
                           decoder.decodedCount.load(ordering: .relaxed) >= UInt64(Self.aaWarmupFrames) {
                            decoder.maxDecodeDepth = Self.aaSteadyDecodeDepth
                            videoWarmShrunk = true
                            log("video warm — decode FIFO depth -> \(Self.aaSteadyDecodeDepth)")
                        }
                        if dataCount % 90 == 0 {
                            log("frames=\(dataCount) decoded=\(decoder.decodedCount.load(ordering: .relaxed)) slotDrops=\(decoder.slotDrops.load(ordering: .relaxed))")
                        }
                    }
                }
            case (AAWire.chMediaPlayback, _), (AAWire.chNavigationStatus, _), (AAWire.chPhoneStatus, _):
                let ev = AAMetadata.decode(channel: ch, id: id, body: body)
                switch ev {
                case .raw(let c, let i, let b):
                    let hex = b.prefix(64).map { String(format: "%02x", $0) }.joined()
                    log("<- METADATA ch=\(c) (\(AAWire.channelName(c))) id=\(i) len=\(b.count) UNMODELED \(hex)")
                case .mediaMetadata(let m):
                    log("<- MEDIA METADATA \(m.song ?? "?") — \(m.artist ?? "?") [\(m.album ?? "?")] art=\(m.albumArt?.count ?? 0)B dur=\(m.durationSeconds ?? 0)s")
                case .mediaStatus(let st):
                    log("<- MEDIA STATUS state=\(st.state ?? 0) src=\(st.source ?? "?") pos=\(st.playbackSeconds ?? 0)s")
                case .navStatus(let v):
                    log("<- NAV STATUS \(v)")
                case .navTurn(let t):
                    log("<- NAV TURN \(t.description) onto \(t.road ?? "?") image=\(t.image?.count ?? 0)B n=\(t.turnNumber ?? 0) angle=\(t.turnAngle ?? 0)")
                case .navDistance(let d):
                    log("<- NAV DISTANCE \(d.meters ?? 0) m, \(d.secondsToTurn ?? 0) s, \(d.displayText ?? "-")")
                case .navState(let st):
                    log("<- NAV STATE \(AAMetadata.maneuverName(st.maneuverType ?? 0)) onto \(st.road ?? "?") cue=\(st.cue.first ?? "-") lanes=\(st.lanes.count) steps=\(st.stepCount)")
                case .navPosition(let p):
                    log("<- NAV POSITION step \(p.stepText ?? "?") in \(p.secondsToStep ?? 0) s; dest \(p.destText ?? "?") ETA \(p.eta ?? "?") (\(p.secondsToArrival ?? 0) s) road=\(p.currentRoad ?? "-")")
                case .phone(let calls, let sig):
                    log("<- PHONE STATUS calls=\(calls.count) signal=\(sig ?? -1) \(calls.map { "\($0.state ?? 0):\($0.callerId ?? $0.number ?? "?")" }.joined(separator: " "))")
                }
                onMetadata?(ev)
            case (AAWire.chVideo, AAWire.mediaStop):
                // STOP ends the VIDEO SESSION, not the connection. The phone sends it on EVERY
                // VideoFocus loss (screen off, a native-transient takeover), and the reference head
                // unit treats it as a playback stop and keeps the channel open for the next START.
                // Returning here tore the whole AA session down — a latent session-killer.
                log("<- MEDIA STOP — stopping playback, waiting for the next START")
                resetVideoSession()
            default:
                // AA_TRACE_UNHANDLED=1 dumps everything we ignore: channel, message id and the raw
                // protobuf body. Protobuf is self-describing enough to reverse a message from this
                // (each field is tag<<3|wiretype), which is how a service with no published schema —
                // media playback status, navigation status — can be decoded from what the phone
                // actually sends rather than guessed at. Off by default: this is per-message logging
                // on a hot path.
                if Self.traceUnhandled {
                    let hex = body.prefix(96).map { String(format: "%02x", $0) }.joined()
                    log("<- UNHANDLED ch=\(ch) id=\(id) len=\(body.count) \(hex)")
                }
                break // audio media, unmodeled chatter — safe to ignore
            }
        }
    }

    /// True if ANY Annex-B NAL in the access unit is an IDR slice (type 5). Scanning
    /// all NALs (not just the first) matters: IDR AUs often lead with SEI/AUD NALs.
    private func containsIDR(_ raw: UnsafeRawBufferPointer) -> Bool {
        let hevc = cap.videoCodecHEVC
        for r in AVCCFastPath.annexBNALRanges(raw) where r.lowerBound < raw.count {
            if hevc {
                // HEVC IRAP: BLA/IDR/CRA are NAL types 16...21.
                let t = (raw[r.lowerBound] >> 1) & 0x3F
                if t >= 16 && t <= 21 { return true }
            } else if (raw[r.lowerBound] & 0x1F) == 5 { return true }
        }
        return false
    }

    /// If the decoder flagged a broken P-chain, re-assert video focus so the phone
    /// emits a fresh IDR (throttled to at most one nudge per drain). Encrypted+SPECIFIC.
    /// Deliberately does NOT re-assert VideoFocus any more.
    ///
    /// Re-asserting PROJECTED while the phone already holds PROJECTED is a STATE-MACHINE NO-OP in
    /// gearhead: `jem.L(old,new,unsolicited)` branches only on an old->new transition, so
    /// PROJECTED->PROJECTED matches nothing and the `unsolicited` flag is only ever logged. It never
    /// prompted the IDR we were hoping for. (And the premise holds: the DHU's whole media message set
    /// contains no keyframe/IDR request — a head unit genuinely cannot ask for one at 1.7.)
    ///
    /// The only way to force one would be to relinquish to NATIVE and re-take PROJECTED, which drives
    /// a real transition through `jem.i()` and restarts projection — a visible glitch, and far worse
    /// than the drop it would be papering over. So the answer to a shed frame is to NOT SHED IT
    /// (steady FIFO depth 4), and to wait out AA's own periodic IDR when one slips through.
    private func drainKeyframe() {
        kfLock.lock(); let need = needKeyframe; needKeyframe = false; kfLock.unlock()
        guard need else { return }
        droppedSinceIdr += 1
        if droppedSinceIdr == 1 || droppedSinceIdr % 20 == 0 {
            log("decoder dropped a frame (\(droppedSinceIdr) total) — waiting for AA's next IDR")
        }
    }

    /// Emit one throttled "AA stats" line per second while the session is up. Checked once per
    /// `eventLoop` iteration rather than on a second timer — reusing `StreamMetricsMonitor`'s 1 Hz timer
    /// would mean handing it a cross-thread reference to a session that runs its own blocking loop on a
    /// background thread, for no real benefit: AA's own loop iterates on every inbound frame, and a live
    /// AA session is never silent (ping/video/sensor traffic keeps arriving), so this fires at
    /// essentially the same 1 Hz cadence without a second timer or a second file logger.
    private func maybeLogStats() {
        let now = Date()
        if let last = lastStatsLog, now.timeIntervalSince(last) < 1.0 { return }
        lastStatsLog = now
        let decoded = decoder.decodedCount.load(ordering: .relaxed)
        let dropped = decoder.slotDrops.load(ordering: .relaxed)
        let media = audioFrames[AAWire.chMediaAudio] ?? 0
        let guidance = audioFrames[AAWire.chGuidanceAudio] ?? 0
        let system = audioFrames[AAWire.chSystemAudio] ?? 0
        let telephony = audioFrames[AAWire.chTelephonyAudio] ?? 0
        log("AA stats video rx=\(dataCount) decoded=\(decoded) dropped=\(dropped) | "
            + "audio media=\(media) guidance=\(guidance) system=\(system) mic=\(micFrames) | "
            + "bytes rx=\(rxBytes) tx=\(txBytes) | backlog=\(transport.writeBacklog)")
        // Same counters, as a value, to the metrics monitor: Settings ▸ stream performance reads the
        // CarPlay decrypt layer, which AA never touches, so an AA session showed four empty rows.
        // `micFrames` is written under touchLock on the capture thread — read it the same way.
        touchLock.lock(); let micN = micFrames; touchLock.unlock()
        StreamMetricsMonitor.publishAA(AAStatsSnapshot(
            t: ProcessInfo.processInfo.systemUptime,
            videoRx: UInt64(dataCount), videoDecoded: decoded, videoDropped: dropped,
            audioMedia: UInt64(media), audioGuidance: UInt64(guidance), audioSystem: UInt64(system),
            audioTelephony: UInt64(telephony), micFrames: UInt64(micN),
            bytesRx: UInt64(rxBytes), bytesTx: UInt64(txBytes),
            backlog: transport.writeBacklog, transport: transportLabel))
    }

    /// Push one captured mic buffer to the phone (16-bit LE PCM at the advertised rate). Called from
    /// the capture callback — safe against the session thread because `sendMsg` is atomic.
    func sendMicPCM(_ pcm: Data) {
        // micActive/micFrames are written on the SESSION thread (MicrophoneRequest open/close) and
        // read/incremented here on the CAPTURE thread, so they need the same lock the other
        // UI-thread <-> session-thread state uses. Never held across the send (see the lock-order note
        // on tlsLock): touchLock is a leaf too.
        touchLock.lock(); let active = micActive; touchLock.unlock()
        guard active, !pcm.isEmpty else { return }
        // Mic PCM is ~32 KB/s and DROPPABLE; video/audio ACKs and the OCBM heartbeat are not. If the
        // shared USB write path is backed up, skip this buffer rather than deepen the queue in front
        // of them (a delayed heartbeat is what makes the box declare the host gone).
        if transport.writeBacklog > 8 {
            micDropped += 1
            if micDropped % 50 == 1 { log("mic backpressure — dropped \(micDropped) buffers") }
            return
        }
        let ts = UInt64(Date().timeIntervalSince(start) * 1_000_000_000)
        var frame = Data(capacity: 8 + pcm.count)
        for i in (0..<8).reversed() { frame.append(UInt8((ts >> (8 * UInt64(i))) & 0xff)) }
        frame.append(pcm)
        _ = sendEnc(AAWire.chMicrophone, AAWire.mediaData, control: false, frame)
        touchLock.lock(); micFrames += 1; let n = micFrames; touchLock.unlock()
        if n == 1 || n % 200 == 0 { log("mic frames=\(n)") }
    }

    /// Return the video lane to its pre-stream state: no keyframe seen yet, deep startup FIFO, and a
    /// flushed decoder. Used at both MEDIA_START and MEDIA_STOP, which bracket an encoder lifetime.
    private func resetVideoSession() {
        sawKeyframe = false
        videoWarmShrunk = false
        decoder.maxDecodeDepth = Self.aaStartupDecodeDepth
        decoder.flush()
    }

    private func drainTouch() {
        touchLock.lock()
        let pending = touchQueue; touchQueue.removeAll(keepingCapacity: true)
        let keys = keyQueue; keyQueue.removeAll(keepingCapacity: true)
        let scrolls = scrollQueue; scrollQueue.removeAll(keepingCapacity: true)
        touchLock.unlock()
        for (x, y, action) in pending {
            let ts = UInt64(Date().timeIntervalSince(start) * 1_000_000_000)
            _ = sendEnc(AAWire.chInput, AAWire.inputReport, control: false,
                        AAWire.inputReportTouch(timestamp: ts, x: x, y: y, action: action))
        }
        for (key, down) in keys {
            // MICROSECONDS SINCE EPOCH, matching openauto's working head unit
            // (src/autoapp/Service/InputService.cpp:140) — NOT nanoseconds since session start,
            // which is what we sent and which is both the wrong unit and a near-zero value.
            // Android's input dispatcher uses event timestamps for ordering and staleness; global
            // media/mic keys bypass that pipeline while navigation keys go through it, which is
            // exactly the split observed (media + mic work, HOME/BACK/D-Pad do not).
            // Touch deliberately left on the old clock for now: it WORKS, and changing two things
            // at once would make the result unattributable.
            let ts = UInt64(Date().timeIntervalSince1970 * 1_000_000)
            _ = sendEnc(AAWire.chInput, AAWire.inputReport, control: false,
                        AAWire.inputReportKey(timestamp: ts, keycode: key.rawValue, down: down))
            log("-> key \(key) \(down ? "DOWN" : "UP")")
        }
        for delta in scrolls {
            let ts = UInt64(Date().timeIntervalSince1970 * 1_000_000)
            _ = sendEnc(AAWire.chInput, AAWire.inputReport, control: false,
                        AAWire.inputReportScroll(timestamp: ts, delta: delta))
            log("-> scroll \(delta > 0 ? "+" : "")\(delta)")
        }
    }
}

// MARK: - TCP transport (test-now path via `adb forward`; the box CH_AA channel later)

import Darwin

final class AATCPTransport: AAByteTransport {
    private var fd: Int32 = -1

    init?(host: String, port: UInt16) {
        fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { return nil }
        var addr = sockaddr_in()
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = port.bigEndian
        // Unchecked, a non-numeric host left sin_addr zeroed and we silently connected to 0.0.0.0.
        guard inet_pton(AF_INET, host, &addr.sin_addr) == 1 else {
            Darwin.close(fd); fd = -1; return nil
        }
        let r = withUnsafePointer(to: &addr) { p in
            p.withMemoryRebound(to: sockaddr.self, capacity: 1) { sp in
                Darwin.connect(fd, sp, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        if r != 0 { Darwin.close(fd); fd = -1; return nil }
        var one: Int32 = 1
        setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, socklen_t(MemoryLayout<Int32>.size))
    }

    func readExact(_ n: Int) -> Data? {
        guard fd >= 0, n > 0 else { return nil }
        var buf = [UInt8](repeating: 0, count: n)
        var got = 0
        while got < n {
            let r = buf.withUnsafeMutableBytes { p in
                recv(fd, p.baseAddress!.advanced(by: got), n - got, 0)
            }
            if r <= 0 { return nil }
            got += r
        }
        return Data(buf)
    }

    func write(_ data: Data) -> Bool {
        guard fd >= 0 else { return false }
        let n = data.count
        return data.withUnsafeBytes { p -> Bool in
            var sent = 0
            while sent < n {
                let r = send(fd, p.baseAddress!.advanced(by: sent), n - sent, 0)
                if r <= 0 { return false }
                sent += r
            }
            return true
        }
    }

    func close() {
        guard fd >= 0 else { return }
        // shutdown() BEFORE close(). `close()` alone does not reliably wake a thread already blocked in
        // recv() on this descriptor — and the fd number can be handed to the next socket the process
        // opens while that thread is still parked on it, so it would then be reading someone else's
        // connection. shutdown() makes the blocked recv return 0, which readExact turns into EOF.
        Darwin.shutdown(fd, SHUT_RDWR)
        Darwin.close(fd)
        fd = -1
    }
}

/// AA byte transport over the OCBM link's CH_IP stream-mux — the box's aa-bridge is reached via the
/// box's CH_IP relay (`IP_OPEN target` → `IP_DATA`), so Android Auto rides the SAME USB OCBM channel
/// as CarPlay instead of a separate NCM/TCP socket. Swap-in for AATCPTransport: same AAByteTransport
/// contract, so AASession is unchanged.
final class AAOCBMTransport: AAByteTransport {
    private let client: OCBMClient
    private let connId: UInt16
    private let cond = NSCondition()
    private var buffer = Data()
    private var eof = false
    /// Total bytes the box has relayed to us on this stream, logged once a second. Exists to be
    /// compared DIRECTLY against aa-bridge's `t=<n>s IN phone->host total=<n>` line: the bridge
    /// counts what it read off the phone's bulk-IN endpoint, this counts what came out the other end
    /// of the CH_IP relay. Equal totals at the moment TLS fails means no bytes were lost in
    /// transport and the corruption is ours; diverging totals means the relay dropped a gap, which a
    /// TLS stream cannot survive. Without this the two halves cannot be told apart from the logs.
    private var rxTotal = 0
    private var rxLastLog = Date()
    /// Cap on box bytes buffered but not yet read by the session thread. ~60 s of AA video at the
    /// bitrates we see, so it can only be reached by a reader that has genuinely stopped — well clear
    /// of normal jitter, which the 15 s read-stall timeout below already bounds from the other side.
    private static let maxUnreadBuffer = 32 * 1024 * 1024

    /// Opens a CH_IP stream to `target` (the box aa-bridge, e.g. "127.0.0.1:5277") over the app's
    /// existing OCBM link and buffers inbound AA bytes for the blocking AASession reader.
    init(client: OCBMClient, connId: UInt16 = 0x00AA, target: String) {
        self.client = client
        self.connId = connId
        // A CH_IP write that never reaches the wire is a HOLE in the TLS record stream (the encrypt
        // already advanced the cipher state and nothing retries). The phone then stops receiving
        // decryptable data — and gearhead answers "no data received" by RESETTING ITS OWN USB GADGET
        // (kqq.java resetUsbGadget/setCurrentFunctions, telemetry NO_DATA_RECEIVED_RESET_REPAIR_
        // ATTEMPTED), which drops it out of accessory mode with no ByeBye and loops. Failing the
        // stream here turns that into a clean, fast session restart instead.
        client.onIpWriteFailed = { [weak self] id in
            guard let self, id == self.connId else { return }
            AASession.osLog.info("CH_IP transport closed conn=\(self.connId, privacy: .public): write failed")
            self.cond.lock(); self.eof = true; self.cond.signal(); self.cond.unlock()
        }
        client.onIpData = { [weak self] id, bytes in
            guard let self, id == self.connId else { return }
            self.cond.lock()
            if bytes.isEmpty {
                AASession.osLog.info("CH_IP transport closed conn=\(self.connId, privacy: .public): box sent IP_CLOSE (empty read)")
                self.eof = true
            } else {
                self.buffer.append(contentsOf: bytes)
                self.rxTotal += bytes.count
                // The box pushes on ITS schedule; nothing here applies back-pressure, so a session
                // thread stalled in the decoder would let this grow until the app is killed. The phone's
                // max_unacked window bounds video but not sensor/audio/control traffic. Fail the stream
                // instead — same shape as onIpWriteFailed, and readExact still drains what's buffered
                // before returning EOF, so the session ends cleanly rather than being cut mid-record.
                if self.buffer.count > Self.maxUnreadBuffer {
                    AASession.osLog.info("CH_IP inbound backlog \(self.buffer.count, privacy: .public) B over the \(Self.maxUnreadBuffer, privacy: .public) B cap — the session reader is not keeping up; failing the stream")
                    AASession.osLog.info("CH_IP transport closed conn=\(self.connId, privacy: .public): inbound backlog cap exceeded")
                    self.eof = true
                }
                if self.rxLastLog.timeIntervalSinceNow <= -1 {
                    AASession.osLog.info("CH_IP box->host total=\(self.rxTotal, privacy: .public) (+\(bytes.count, privacy: .public)B last, \(self.buffer.count, privacy: .public)B unread)")
                    self.rxLastLog = Date()
                }
            }
            self.cond.signal()
            self.cond.unlock()
        }
        client.ipOpen(id: connId, target: target)
        // "Connected" here means the IP_OPEN went out, not a confirmed box-side accept — CH_IP has no
        // open-ack; the first `onIpData` byte is the real confirmation, and the box->host total line
        // below fires on it.
        AASession.osLog.info("CH_IP transport connecting conn=\(connId, privacy: .public) -> \(target, privacy: .public)")
    }

    /// Give up if the box goes quiet for this long. A live AA session is never silent — the phone
    /// pings, sends video and drives the sensor channel — so silence means the stream is dead, and an
    /// untimed `cond.wait()` here would park the session thread FOREVER: the box can drop the relay
    /// without an IP_CLOSE ever reaching us (aa-bridge resetting the accessory at its announce-window
    /// deadline, the bridge being SIGKILLed, a box reboot). Returning nil ends `AASession.run`, which
    /// closes the transport, which lets the box observe EOF and re-announce — i.e. the session
    /// RECOVERS instead of hanging with a dark window.
    /// 6 s, not 20: a real AA stream is never quiet for even one second (ping, video, sensors), and
    /// this timeout is the app's half of recovering from a phone that vanished off the box's USB bus —
    /// every second here is a second of frozen screen before the session can restart.
    /// 15 s, matching this project's own proven Rust reference client (host/aa-headunit uses a 15 s
    /// socket read timeout). 6 s was picked to "recover faster", but faster teardown is the wrong
    /// trade when the failure may be a RECOVERABLE transient: ending the session bounces the phone out
    /// of accessory mode via the bridge's reset, manufacturing the very outage it was meant to shorten.
    /// Override with AA_READ_STALL_S for experiments.
    private static let readStallTimeout: TimeInterval =
        ProcessInfo.processInfo.environment["AA_READ_STALL_S"].flatMap(Double.init) ?? 15

    func readExact(_ n: Int) -> Data? {
        guard n > 0 else { return nil }
        cond.lock(); defer { cond.unlock() }
        let deadline = Date().addingTimeInterval(Self.readStallTimeout)
        while buffer.count < n && !eof {
            if !cond.wait(until: deadline) {
                AASession.osLog.info("CH_IP transport closed conn=\(self.connId, privacy: .public): read stall — no box data for \(Int(Self.readStallTimeout), privacy: .public)s, giving up")
                eof = true // don't let a later caller re-arm the wait on a stream we've given up on
                return nil
            }
        }
        guard buffer.count >= n else { return nil } // EOF before n bytes
        let out = buffer.prefix(n)
        buffer.removeFirst(n)
        return Data(out)
    }

    func write(_ data: Data) -> Bool {
        // Still fire-and-forget (the write completes on the OCBM queue), but a FAILED write now marks
        // the stream dead via `onIpWriteFailed` above rather than being swallowed.
        cond.lock(); let dead = eof; cond.unlock()
        if dead { return false }
        client.ipWrite(id: connId, [UInt8](data))
        return true
    }

    func close() {
        AASession.osLog.info("CH_IP transport closed conn=\(self.connId, privacy: .public): local close (session teardown)")
        client.ipClose(id: connId)
        cond.lock(); eof = true; cond.signal(); cond.unlock()
    }

    var writeBacklog: Int { client.aaWriteBacklog }
}
