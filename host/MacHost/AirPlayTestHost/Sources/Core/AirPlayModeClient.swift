// AirPlayModeClient.swift — AirPlay-only mode over one raw OCBM pipe: framing, the CH_CTRL state machine
// (`AirPlayModeSession`), and the three lanes (`AirPlayVideoLane`, `AirPlayAudioLane`, `MetaReader`).
//
// Deliberately not `OCBMClient`: that class is the projection host (HELLO -> SUBSCRIBE), and a SUBSCRIBE
// makes the box leave this mode. The pieces under it are the shipping app's own, unchanged:
// `RawBulkTransport`, `OCBM.frame`, `OCBMReassembler`, `parseLogEntries`.
//
// Threading: ALL state lives on `queue`. The transport's read handler hops onto it (in order: the queue
// is serial), the 100 ms tick runs on it, writes are made from it, and `onEvent` is called on it. The
// only cross-thread surface is `counters`, a snapshot behind a mutex for the UI's poll.
//
// The queue is the CALLER'S, and successive clients must be given the same one. A client that replaces
// another after a lost link then starts strictly after the old one's last block, so whatever consumes
// `onEvent` (the media pipeline) is never entered by two clients at once.

import Foundation
import Synchronization

struct AirPlayClientCounters: Sendable, Equatable {
    var bytesReceived: UInt64 = 0
    var framesReceived: UInt64 = 0
    var writeFailures: UInt64 = 0
    var video = AirPlayVideoStats()
    var audio: [AirPlayAudioStreamStats] = []
    var audioLaneBytes: UInt64 = 0
    var audioResyncs: UInt64 = 0
    var metaResyncs: UInt64 = 0
    var events: UInt64 = 0
}

final class AirPlayModeClient: @unchecked Sendable {
    enum Event {
        case phase(AirPlayModeSession.Phase)
        case helloAck(HelloAckInfo)
        case boxNotReady
        case state(AirPlayBoxState)
        case note(String)
        /// The mode was requested again (restarted ocbmd): every stream key is void.
        case reRequested
        case video(AirPlayVideoEvent)
        case audio(AirPlayAudioEvent)
        case meta(AirPlayEvent)
        case artwork(Data)
        case boxLog([LogEntry])
        /// The session ended and the pipe was released. Always the last event of a client.
        case closed(AirPlayModeSession.End)
    }

    /// Fixed for the process lifetime and non-zero, as `OCBMClient` and `ocbm-host` do it: ocbmd tells
    /// a relaunched host from a retransmitted HELLO by this value.
    static let instanceNonce: UInt32 = .random(in: 1...UInt32.max)

    /// Called on the client's queue.
    var onEvent: ((Event) -> Void)?

    private let transport: RawBulkTransport
    private let queue: DispatchQueue
    private var session: AirPlayModeSession
    private let reasm = OCBMReassembler()
    private var videoLane = AirPlayVideoLane()
    private var audioLane = AirPlayAudioLane()
    private var meta = MetaReader()
    private var seq: UInt32 = 0
    private var timer: DispatchSourceTimer?
    private var closed = false
    private var local = AirPlayClientCounters()
    private let shared = Mutex(AirPlayClientCounters())

    init(transport: RawBulkTransport, wantBoxLog: Bool, queue: DispatchQueue) {
        self.transport = transport
        self.queue = queue
        session = AirPlayModeSession(nonce: Self.instanceNonce, wantBoxLog: wantBoxLog)
        transport.setReadHandler { [weak self] chunk in
            guard let self else { return }
            self.queue.async { self.onRead(chunk) }
        }
    }

    /// A consistent copy of the tallies, for any thread.
    var counters: AirPlayClientCounters { shared.withLock { $0 } }

    private static var now: Double { Double(DispatchTime.now().uptimeNanoseconds) / 1e9 }

    /// Open the pipe, say HELLO (retried until the box answers), then request the mode.
    func start(_ request: AirPlayModeRequest) {
        queue.async { [self] in
            guard !closed, session.phase == .idle else { return }
            transport.start()
            let t = DispatchSource.makeTimerSource(queue: queue)
            t.schedule(deadline: .now() + 0.1, repeating: 0.1, leeway: .milliseconds(20))
            t.setEventHandler { [weak self] in
                guard let self, !self.closed else { return }
                self.apply(self.session.tick(now: Self.now))
            }
            timer = t
            t.resume()
            apply(session.start(request, now: Self.now))
        }
    }

    /// Leave the mode (CT_MODE_SELECT [MODE_PROJECTION], acknowledged; CT_STOP if not) and release the
    /// pipe. Asynchronous: `.closed(.stopped)` reports the end. Never blocks the caller.
    func stop() {
        queue.async { [self] in
            guard !closed else { return }
            apply(session.stop(now: Self.now))
        }
    }

    /// The link is gone (unplug, pipe errors): release everything without writing to it. Reports no
    /// `.closed`: the caller already knows, and a late event from a dead client would only race the
    /// one that replaces it.
    func abandon() {
        queue.async { [self] in
            guard !closed else { return }
            closed = true
            onEvent = nil
            timer?.cancel()
            timer = nil
            transport.stop()
        }
    }

    // MARK: - queue-confined

    private func send(_ payload: [UInt8]) {
        guard payload.count <= OCBM.maxPayload else { return }
        let frame = OCBM.frame(channel: OCBM.chCtrl, flags: OCBM.fSom | OCBM.fEom, seq: seq, payload: payload)
        seq &+= 1
        if !transport.writeBulk(frame) {
            local.writeFailures += 1
            onEvent?(.note("USB write failed (CH_CTRL op 0x\(String(payload.first ?? 0, radix: 16)))"))
        }
    }

    private func apply(_ outputs: [AirPlayModeSession.Output]) {
        for o in outputs {
            switch o {
            case .send(let payload):
                send(payload)
            case .phase(let p):
                onEvent?(.phase(p))
                if case .ended(let end) = p { close(end) }
            case .helloAck(let info):
                onEvent?(.helloAck(info))
            case .boxNotReady:
                onEvent?(.boxNotReady)
            case .state(let st):
                onEvent?(.state(st))
            case .reRequested:
                resetLanes()
                onEvent?(.reRequested)
            case .note(let text):
                onEvent?(.note(text))
            }
        }
        publish()
    }

    private func resetLanes() {
        videoLane.reset()
        audioLane.reset()
        meta = MetaReader()
    }

    private func close(_ end: AirPlayModeSession.End) {
        guard !closed else { return }
        closed = true
        timer?.cancel()
        timer = nil
        // Every write was synchronous on this queue, so the leave request is already on the wire.
        transport.stop()
        publish()
        onEvent?(.closed(end))
        onEvent = nil
    }

    private func publish() {
        local.video = videoLane.stats
        local.audio = audioLane.stats
        local.audioLaneBytes = audioLane.laneBytes
        local.audioResyncs = audioLane.resyncs
        local.metaResyncs = meta.resyncs
        let copy = local
        shared.withLock { $0 = copy }
    }

    private func onRead(_ chunk: [UInt8]) {
        guard !closed else { return }
        local.bytesReceived += UInt64(chunk.count)
        reasm.push(chunk)
        while !closed, let f = reasm.next() {
            local.framesReceived += 1
            route(f)
        }
        publish()
    }

    /// The lanes are taken only while this host holds (or is being given) the mode. After the host asked
    /// to leave they are dropped unread: nothing may reach a decoder that is being torn down.
    private var acceptsMedia: Bool { session.phase == .active || session.phase == .awaitingAck }

    private func route(_ f: OCBMFrame) {
        let fresh = f.flags & OCBM.fNewSource != 0
        switch f.channel {
        case OCBM.chCtrl:
            apply(session.control(f.payload, now: Self.now))
        case OCBM.chVideo where acceptsMedia:
            videoLane.feed(f.payload, newSource: fresh) { [onEvent] in onEvent?(.video($0)) }
        case OCBM.chMediaAudio where acceptsMedia:
            audioLane.feed(f.payload, newSource: fresh) { [onEvent] in onEvent?(.audio($0)) }
        case OCBM.chMetadata where acceptsMedia:
            meta.push(f.payload, newSource: fresh)
            while let m = meta.next() {
                switch m.marker {
                case OCBM.metaJson:
                    if let ev = AirPlayEvent.parse(m.body) {
                        local.events += 1
                        onEvent?(.meta(ev))
                    }
                case OCBM.metaArtwork where m.body.count > 1:
                    onEvent?(.artwork(Data(m.body[1...]))) // [artwork id][JPEG]
                default:
                    break
                }
            }
        case OCBM.chLog:
            let entries = parseLogEntries(f.payload)
            if !entries.isEmpty { onEvent?(.boxLog(entries)) }
        default:
            break
        }
    }
}
