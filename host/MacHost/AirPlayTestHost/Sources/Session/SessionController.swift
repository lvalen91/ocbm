// SessionController.swift — the app's one piece of orchestration: the user's intent (Start / Stop), the
// link to the box, the AirPlay-mode client on it, the media pipeline behind that, and the status models
// in front. No views; the tests run it against the synthetic link.
//
// Lifetimes and the interleavings that matter:
//   - The pipe is held only between Start and the end of the leave. Opening it is exclusive (another
//     OCBM client on the same box loses the interface), so nothing is opened before Start.
//   - `epoch` names the current client. Every event a client sends to the main queue carries the epoch
//     it was created under and is dropped if a newer client exists: a block queued by a client whose
//     link died cannot repaint the status of the one that replaced it.
//   - All clients and the pipeline share `sessionQueue` (serial). A replacement client's first block
//     runs after the old client's last, so the pipeline never sees two clients at once.
//   - The passphrase exists in `request` while the intent is "running" (it is needed again when the
//     box comes back after an unplug) and is dropped the moment the session is over.

import CoreGraphics
import CoreVideo
import Foundation
import ImageIO
import Synchronization

@MainActor
final class SessionController {
    let form: FormModel
    let link = LinkStatus()
    let streams = StreamsStatus()
    let video = VideoStatus()
    let audio = AudioStatus()
    let meta = MetadataStatus()
    let log = LogModel()

    let pipeline: MediaPipeline
    private let boxLink: BoxLink
    private let sessionQueue = DispatchQueue(label: "airplay-test.session", qos: .userInitiated)
    private let showing = Flag()
    private var client: AirPlayModeClient?
    private var epoch = 0
    private var request: AirPlayModeRequest?
    private var reopen: DispatchWorkItem?
    private var reopenAttempts = 0
    private var pollTimer: Timer?
    private var lastCounters = AirPlayClientCounters()
    private var artworkVersion = 0
    /// DEBUG runs only: where to write the numeric status once a poll (never identities or metadata).
    var statusDumpPath: String?
    var showBoxLog = true

    /// `onFrame` is called on VideoToolbox's thread, `onVideoCleared` on the session queue.
    init(boxLink: BoxLink, form: FormModel, audioOutput: Bool, outputScale: Float = 1,
         onFrame: @escaping @Sendable (CVPixelBuffer) -> Void,
         onVideoCleared: @escaping @Sendable () -> Void) {
        self.boxLink = boxLink
        self.form = form
        // Built in two steps so the closures can reach the log through a weak reference to `self`.
        let relay = LogRelay()
        let showing = self.showing
        pipeline = MediaPipeline(
            audioOutput: audioOutput, outputScale: outputScale,
            onFrame: { pb in
                showing.value.withLock { $0 = true }
                onFrame(pb)
            },
            onVideoCleared: {
                showing.value.withLock { $0 = false }
                onVideoCleared()
            },
            onLog: { text in relay.post(text) })
        relay.sink = { [weak self] text in self?.log.add(text) }

        boxLink.onPresence = { [weak self] present in self?.presenceChanged(present) }
        boxLink.onLost = { [weak self] reason in self?.linkLost(reason) }
        boxLink.startWatching()
        presenceChanged(boxLink.isPresent)

        let t = Timer(timeInterval: 0.5, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.poll() }
        }
        RunLoop.main.add(t, forMode: .common)
        pollTimer = t
    }

    // MARK: - Intent

    /// Start pressed. Returns false (with `link.lastError` set) when the fields make no valid request.
    @discardableResult
    func start(passphrase: String) -> Bool {
        guard !link.running, !link.stopping else { return false }
        switch form.request(passphrase: passphrase) {
        case .failure(let bad):
            link.lastError = bad.message
            return false
        case .success(let req):
            form.remember()
            request = req
            link.requested = req.display.text + (req.hevc ? ", HEVC allowed" : ", H.264 only")
            link.running = true
            link.lastError = ""
            reopenAttempts = 0
            log.add("start: AirPlay-only mode with \(req.summary)")
            connect()
            return true
        }
    }

    /// "Smooth playback": pace pictures by their timestamps behind a 100 ms buffer. Any time, also mid-stream.
    func setSmoothPlayback(_ on: Bool) {
        pipeline.presenter.smooth = on
        video.smooth = on
        log.add(on ? "smooth playback on: pictures shown at their timestamps plus a \(pipeline.presenter.snapshot.bufferMs) ms buffer"
                   : "smooth playback off: pictures shown as soon as decoded")
    }

    /// Stop pressed: ask the box to leave the mode, stop the heartbeats, release the pipe.
    func stop() {
        guard link.running, !link.stopping else { return }
        reopen?.cancel()
        reopen = nil
        if let client {
            link.stopping = true
            link.phase = "leaving the mode"
            log.add("stop: CT_MODE_SELECT [PROJECTION], heartbeats stopped")
            client.stop()
        } else {
            log.add("stop")
            finish(error: nil)
        }
    }

    /// The app is quitting: get the leave request onto the wire, then let go. The box would leave by
    /// itself 10 s after the last heartbeat; this only saves it the wait.
    func shutdown() {
        pollTimer?.invalidate()
        if let client {
            client.stop()
            Thread.sleep(forTimeInterval: 0.3)
        }
        pipeline.player.shutdown()
    }

    // MARK: - Link

    private func presenceChanged(_ present: Bool) {
        link.boxPresent = present
        if client == nil {
            link.link = present ? "\(boxLink.label): attached, not opened" : "no box on USB"
        }
        guard present, link.running, !link.stopping, client == nil else { return }
        log.add("box attached: opening it")
        scheduleConnect(after: 0.5)
    }

    private func scheduleConnect(after delay: Double) {
        reopen?.cancel()
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated { self?.connect() }
        }
        reopen = work
        DispatchQueue.main.asyncAfter(deadline: .now() + delay, execute: work)
    }

    private func connect() {
        guard link.running, !link.stopping, client == nil, let request else { return }
        guard boxLink.isPresent else {
            link.phase = "waiting for the box on USB"
            return
        }
        guard let transport = boxLink.open() else {
            reopenAttempts += 1
            if reopenAttempts > 5 {
                log.add("could not open the box's OCBM interface after 5 attempts")
                finish(error: "could not open the box's OCBM interface (is another OCBM client holding it?)")
            } else {
                log.add("could not open the box's OCBM interface (attempt \(reopenAttempts)/5); retrying in 2 s")
                scheduleConnect(after: 2)
            }
            return
        }
        epoch += 1
        let mine = epoch
        let pipeline = self.pipeline
        let c = AirPlayModeClient(transport: transport, wantBoxLog: showBoxLog, queue: sessionQueue)
        c.onEvent = { [weak self] event in
            pipeline.handle(event)
            switch event {
            case .video, .audio, .meta, .artwork:
                break // the pipeline's business; the poll shows the result
            case .phase, .helloAck, .boxNotReady, .state, .note, .reRequested, .boxLog, .closed:
                DispatchQueue.main.async {
                    MainActor.assumeIsolated { self?.clientEvent(event, epoch: mine) }
                }
            }
        }
        client = c
        link.link = "\(boxLink.label): open"
        link.phase = "saying HELLO"
        sessionQueue.async { pipeline.reset() }
        c.start(request)
    }

    private func linkLost(_ reason: String) {
        guard let dead = client else { return }
        epoch += 1 // whatever it already queued for the main queue is now stale
        dead.abandon()
        client = nil
        boxLink.closed()
        let pipeline = self.pipeline
        sessionQueue.async { pipeline.reset() }
        clearBoxState()
        log.add("link lost: \(reason)")
        link.link = boxLink.isPresent ? "\(boxLink.label): attached, not opened" : "no box on USB"
        if link.stopping {
            finish(error: nil) // the user wanted out; the box leaves by its heartbeat watchdog
            return
        }
        link.phase = "box disconnected: waiting for it to come back"
        if boxLink.isPresent {
            // The device is still there, only the pipe died: try it again.
            reopenAttempts += 1
            if reopenAttempts > 5 {
                finish(error: "the box's OCBM pipe keeps failing: \(reason)")
            } else {
                scheduleConnect(after: 2)
            }
        }
    }

    private func clearBoxState() {
        link.ap = "-"
        link.session = "-"
        link.lastEnd = "-"
    }

    /// The session is over, for whatever reason: forget the passphrase and give the button back.
    private func finish(error: String?) {
        reopen?.cancel()
        reopen = nil
        request = nil
        client = nil
        link.running = false
        link.stopping = false
        link.phase = "idle"
        link.link = boxLink.isPresent ? "\(boxLink.label): attached, not opened" : "no box on USB"
        clearBoxState()
        link.requested = "-"
        if let error {
            link.lastError = error
            log.add("ended: \(error)")
        }
    }

    // MARK: - Client events (main queue)

    private func clientEvent(_ event: AirPlayModeClient.Event, epoch e: Int) {
        guard e == epoch else { return }
        switch event {
        case .phase(let p):
            switch p {
            case .idle: link.phase = "idle"
            case .hello: link.phase = "saying HELLO"
            case .awaitingAck: link.phase = "mode requested, waiting for the box"
            case .active: link.phase = "AirPlay-only mode active"
            case .leaving: link.phase = "leaving the mode"
            case .ended: break // `.closed` follows
            }
        case .helloAck(let info):
            reopenAttempts = 0
            link.caps = "0x" + String(info.caps, radix: 16) + (info.supportsAirPlay ? " (AirPlay)" : " (no AirPlay bit)")
            link.activeMode = AirPlayWire.modeName(info.activeMode)
            if let b = info.bundle {
                let id = b.contentIDHex
                link.bundle = id.isEmpty ? "none" : String(id.prefix(12))
                link.bundleState = OCBM.bundleStateName(b.state)
            } else {
                link.bundle = "not reported"
                link.bundleState = "-"
            }
            log.add("HELLO_ACK: version \(info.version), caps \(link.caps), mode \(link.activeMode), bundle \(link.bundle) (\(link.bundleState))")
        case .boxNotReady:
            link.phase = "box not answering HELLO (absent or still booting); retrying"
            log.add("no HELLO_ACK after 15 s; still retrying every 2 s")
        case .state(let st):
            link.ap = st.apName
            link.session = st.sessionName
            link.lastEnd = st.lastEndName
            log.add("box state: ap=\(st.apName) session=\(st.sessionName) last-end=\(st.lastEndName)")
            if st.ap == OCBM.airplayApFailed {
                log.add("the box could not raise the AP (it retries by itself); see the box log lines")
            }
        case .note(let text):
            log.add(text)
        case .reRequested:
            log.add("mode requested again; all stream keys are void")
        case .boxLog(let entries):
            guard showBoxLog else { break }
            for entry in entries {
                if let n = entry.droppedCount {
                    log.add("[\(OCBM.logSourceName(entry.source))] (\(n) lines dropped by the box)", source: .box)
                } else {
                    log.add("[\(OCBM.logSourceName(entry.source))] \(entry.text)", source: .box)
                }
            }
        case .closed(let end):
            boxLink.closed()
            log.add("pipe released (\(end.message))")
            finish(error: end == .stopped ? nil : end.message)
        case .video, .audio, .meta, .artwork:
            break
        }
    }

    // MARK: - Poll (main queue, 2 Hz)

    private func poll() {
        if let client { lastCounters = client.counters }
        let c = lastCounters
        let d = pipeline.decoder.snapshot
        let a = pipeline.player.snapshot
        let s = pipeline.snapshot
        let t = pipeline.timingReadout()
        let p = pipeline.presenter.snapshot

        link.bytesReceived = c.bytesReceived
        link.frames = c.framesReceived
        link.resyncs = c.video.resyncs + c.audioResyncs + c.metaResyncs
        if let sender = s.sender {
            let kind = sender.mirroring ? "mirroring" : "audio"
            link.sender = "\(sender.name) (\(sender.model), \(sender.osName) \(sender.osVersion), \(kind), \(sender.timing))"
        }
        streams.rows = s.streams

        // A 2 s moving window, not the 0.5 s between two polls: the phone encodes on screen change, and a
        // still half second made the old per-poll rate read 0 (VideoTiming.swift).
        func tenth(_ x: Double) -> Double { (x * 10).rounded() / 10 }
        video.fps = tenth(t.fps)
        video.averageFPS = tenth(t.averageFPS)
        video.streamFPS = tenth(t.streamFPS)
        video.jitterMs = tenth(t.jitterMs)
        video.jitterPeakMs = tenth(t.jitterPeakMs)
        video.lastFrame = t.sinceLastFrame.map { String(format: "%.1f s ago", $0) } ?? "-"
        video.decoded = d.decoded
        video.decodeErrors = d.errors
        video.lastError = d.lastError
        video.unconfigured = d.unconfigured
        if d.codec != nil {
            video.codec = d.codecSummary + (d.hardware == true ? ", hardware decoder" : ", software decoder")
        } else {
            video.codec = "-"
        }
        video.codecProblem = d.configProblem ?? ""
        video.codedSize = d.width > 0 ? "\(d.width)x\(d.height)" : "-"
        video.smooth = p.smooth
        video.playback = p.smooth ? "\(p.bufferMs) ms buffer: \(p.late) late, \(p.dropped) dropped" : "on decode"
        video.headerSize = s.headerSourceSize.map { "source \($0), output \(s.headerOutputSize ?? "n/a")" } ?? "-"
        video.decrypted = c.video.decrypted
        video.undecryptable = c.video.failed + c.video.noKey
        video.gaps = c.video.gaps
        video.framesLost = c.video.framesLost
        video.keyframes = c.video.flaggedIDR
        video.lastSeq = s.lastVideoSeq.map(String.init) ?? "-"
        video.showing = showing.value.withLock { $0 }

        if let scid = a.scid, let kind = a.kind {
            let f = a.format ?? .measuredDefault
            audio.stream = "type \(kind.streamType) scid \(scid), \(f.codecName) \(f.sampleRate) Hz \(f.channels) ch"
        } else {
            audio.stream = "-"
        }
        audio.packets = c.audio.reduce(0) { $0 + $1.decrypted }
        audio.undecryptable = c.audio.reduce(0) { $0 + $1.failed + $1.noKey }
        audio.decoded = a.decoded
        audio.decodeErrors = a.decodeErrors
        audio.underruns = a.underruns
        audio.trims = a.trims
        audio.stale = a.stale
        audio.backlog = a.backlogUnits
        let rate = Double(a.format?.sampleRate ?? 44100)
        audio.queuedMs = Int(Double(a.queue.queuedFrames) / rate * 1000)
        audio.playedSeconds = (Double(a.queue.playedFrames) / rate * 10).rounded() / 10
        if let err = a.outputError {
            audio.output = "error: \(err)"
        } else if a.outputRunning {
            let state = a.queue.paused ? "paused" : a.queue.playing ? "playing" : "idle"
            audio.output = "\(state), gain \(Int((a.gain * 100).rounded()))%"
        } else {
            audio.output = "not started"
        }

        meta.title = s.track?.title ?? "-"
        meta.artist = s.track?.artist ?? "-"
        meta.album = s.track?.album ?? "-"
        meta.volume = s.volumeDB.map { db in
            "\(String(format: "%.1f", db)) dB (\(Int((AirPlayVolume.sliderPosition(db: db) * 100).rounded()))%)"
        } ?? "-"
        if s.artworkVersion != artworkVersion {
            artworkVersion = s.artworkVersion
            meta.artwork = s.artwork.flatMap { data in
                CGImageSourceCreateWithData(data as CFData, nil).flatMap { CGImageSourceCreateImageAtIndex($0, 0, nil) }
            }
        }
        #if DEBUG
        if let statusDumpPath { dumpStatus(to: statusDumpPath, counters: c, decoder: d, player: a, timing: t, presenter: p) }
        #endif
    }

    #if DEBUG
    /// Numbers and state names only, for a bench run that has nobody watching the window.
    private func dumpStatus(to path: String, counters c: AirPlayClientCounters, decoder d: MirrorVideoDecoder.Stats,
                            player a: AirPlayAudioPlayer.Stats, timing t: VideoTimingMeter.Readout,
                            presenter p: FramePresenter.Snapshot) {
        // `request` carries the passphrase: only its display and HEVC fields are written.
        let status: [String: Any] = [
            "running": link.running, "phase": link.phase, "link": link.link, "caps": link.caps,
            "bundleState": link.bundleState, "ap": link.ap, "session": link.session, "lastEnd": link.lastEnd,
            "lastError": link.lastError, "bytesReceived": c.bytesReceived, "ocbmFrames": c.framesReceived,
            "streams": streams.rows.map { ["lane": $0.lane, "type": $0.type ?? 0] },
            "request": ["display": request?.display.text ?? "", "hevc": request?.hevc ?? false] as [String: Any],
            "video": [
                "decoded": d.decoded, "errors": d.errors, "lastError": d.lastError, "unconfigured": d.unconfigured,
                "fps": video.fps, "fpsAverage": video.averageFPS, "streamFps": video.streamFPS,
                "jitterMs": video.jitterMs, "jitterPeakMs": video.jitterPeakMs,
                "lastFrameAgeMs": t.sinceLastFrame.map { Int(($0 * 1000).rounded()) } ?? -1,
                "codec": d.codec?.rawValue ?? "", "codecSummary": d.codecSummary,
                "hardwareDecode": d.hardware ?? false, "codecSwitches": d.codecSwitches, "decoderSessions": d.sessions,
                "configProblem": d.configProblem ?? "",
                "smooth": p.smooth, "pacedLate": p.late, "pacedDropped": p.dropped, "presented": p.presented,
                "width": d.width, "height": d.height, "decrypted": c.video.decrypted,
                "undecryptable": c.video.failed + c.video.noKey, "gaps": c.video.gaps, "framesLost": c.video.framesLost,
                "flaggedIDR": c.video.flaggedIDR, "streams": c.video.streams, "showing": video.showing,
                "headerSize": video.headerSize,
            ] as [String: Any],
            "audio": [
                "packets": audio.packets, "undecryptable": audio.undecryptable, "decoded": a.decoded,
                "decodeErrors": a.decodeErrors, "underruns": a.underruns, "trims": a.trims, "stale": a.stale,
                "switches": a.streamSwitches, "flushedUnits": a.flushedUnits, "queuedMs": audio.queuedMs,
                "backlog": a.backlogUnits, "playedSeconds": audio.playedSeconds, "output": audio.output,
                "type": a.kind?.streamType ?? 0,
            ] as [String: Any],
            "meta": ["haveTitle": meta.title != "-", "haveArtwork": meta.artwork != nil, "volume": meta.volume] as [String: Any],
            "logLines": log.lines.count,
        ]
        if let data = try? JSONSerialization.data(withJSONObject: status, options: [.prettyPrinted, .sortedKeys]) {
            try? data.write(to: URL(fileURLWithPath: path), options: .atomic)
        }
    }
    #endif
}

/// "A picture is on screen", set from VideoToolbox's thread and the session queue, read by the poll.
private final class Flag: Sendable {
    let value = Mutex(false)
}

/// Carries the pipeline's log lines (session queue) to the log model (main queue), in order.
private final class LogRelay: @unchecked Sendable {
    /// Set once on the main queue before any line can be posted; called on the main queue.
    var sink: (@MainActor (String) -> Void)?

    func post(_ text: String) {
        DispatchQueue.main.async {
            MainActor.assumeIsolated { self.sink?(text) }
        }
    }
}
