// SyntheticBox.swift — DEBUG ONLY: a stand-in for the box at the raw USB pipe (`RawBulkTransport`), so
// the whole app can be run and measured with no hardware and no USB access at all. It answers the
// control plane as ocbmd does (HELLO_ACK with the mode tail, CT_MODE_ACK, CT_AIRPLAY_STATE, the 10 s
// heartbeat watchdog, SEV_HOST_GONE to an unexpected beat) and plays a scripted pair of sessions built
// from real H.264 / AAC files, encrypted and framed by `TunnelFixture` exactly as the tunnel carries
// them:
//
//   session A  mirroring: type 110 (one IDR, opcode-5 statistics every 30 frames), H.264 or HEVC chosen
//              as the phone chooses (HEVC only when the request allows it AND asks for a canvas above
//              1080p; docs/ocbm/02_AIRPLAY_ONLY_MODE.md §4.10), or alternating per pair; a type 96 stream
//              that starts after the video, is torn down half way and comes back with a NEW scid and
//              key (its lane ahead of its `start` event); volume
//   session B  music: type 103 delivered far ahead of real time; track, artwork, volume; a pause
//              (anchor rate 0 then 1) and a flush that skips ahead
//
// and then the pair again, with new scids and keys. Compiled out of release builds.

#if DEBUG

import Foundation

final class SyntheticBox: RawBulkTransport, @unchecked Sendable {
    struct Media {
        var video: TunnelFixture.Video
        var audio: TunnelFixture.Audio
        var fps = 30.0
        /// The HEVC stream sent when the request allows it (see the header); nil = always H.264.
        var hevc: TunnelFixture.Video?
        var hevcFPS = 60.0
    }

    struct Options {
        /// Drop this video frame of every mirroring session (a seq jump the host must survive).
        var dropVideoFrame: Int?
        /// Run the script this many times faster than real time (tests).
        var speed = 1.0
        /// Stop after this many session pairs (0 = loop for ever).
        var pairs = 0
        /// Seconds without a heartbeat before the mode is left, as ocbmd's HEARTBEAT_GRACE.
        var heartbeatGrace = 10.0
        /// Ignore the request and alternate per session pair: H.264, HEVC, H.264, ... (needs `Media.hevc`).
        var alternateCodecs = false
    }

    /// What the host did, for the tests.
    struct Observed: Sendable {
        var hellos = 0
        var modeRequests: [AirPlayModeRequest] = []
        var badModeRequests = 0
        var heartbeats = 0
        var heartbeatsOutsideMode = 0
        var subscribes = 0
        var stops = 0
        var projectionSelects = 0
        var logArmed = false
        var sessionsPlayed = 0
        var videoFramesSent = 0
        var audioPacketsSent = 0
        /// The codec of each mirroring session sent, in order.
        var videoCodecs: [MirrorVideoCodec] = []
        var leftByWatchdog = false
    }

    private struct Step {
        var at: Double
        var channel: UInt16
        var bytes: [UInt8]
        var state: AirPlayBoxState?
    }

    private let media: Media
    private let options: Options
    private let q = DispatchQueue(label: "airplay-test.synthetic-box", qos: .userInitiated)
    private let lock = NSLock()
    private var observed = Observed()
    private var handler: (([UInt8]) -> Void)?
    private let reasm = OCBMReassembler()
    private var seq: UInt32 = 0
    private var running = false
    private var modeActive = false
    private var lastBeat = 0.0
    private var lastNudge = -100.0
    private var state = AirPlayBoxState()
    private var timer: DispatchSourceTimer?
    private var steps: [Step] = []
    private var cursor = 0
    private var scriptStart = 0.0
    private var pair = 0
    private var dead = false
    private var request: AirPlayModeRequest?

    init(media: Media, options: Options = Options()) {
        self.media = media
        self.options = options
    }

    var seen: Observed { lock.withLock { observed } }
    private func note(_ f: (inout Observed) -> Void) { lock.withLock { f(&observed) } }

    private static var now: Double { Double(DispatchTime.now().uptimeNanoseconds) / 1e9 }

    // MARK: - RawBulkTransport

    func setReadHandler(_ handler: @escaping ([UInt8]) -> Void) { q.sync { self.handler = handler } }

    func start() {
        q.async { [self] in
            guard !running else { return }
            running = true
            let t = DispatchSource.makeTimerSource(queue: q)
            t.schedule(deadline: .now() + 0.005, repeating: 0.005, leeway: .milliseconds(1))
            t.setEventHandler { [weak self] in self?.tick() }
            timer = t
            t.resume()
        }
    }

    func stop() {
        q.async { [self] in
            running = false
            timer?.cancel()
            timer = nil
            handler = nil
        }
    }

    /// Go silent without a goodbye, as an unplugged box does.
    func kill() {
        q.async { [self] in dead = true }
    }

    @discardableResult
    func writeBulk(_ bytes: [UInt8]) -> Bool {
        q.async { [self] in
            guard running, !dead else { return }
            reasm.push(bytes)
            while let f = reasm.next() {
                if f.channel == OCBM.chCtrl { control(f.payload) }
            }
        }
        return true
    }

    // MARK: - Control plane, as ocbmd answers it

    private func emit(_ channel: UInt16, _ payload: [UInt8]) {
        guard running, !dead, let handler else { return }
        let framed = TunnelFixture.ocbmFrames(channel: channel, payload: payload, seq: &seq)
        // A bulk read returns whatever is in the pipe, not a frame: hand it over in uneven pieces.
        var at = 0
        while at < framed.count {
            let n = min(16_384 + (at % 3) * 777, framed.count - at)
            handler(Array(framed[at..<(at + n)]))
            at += n
        }
    }

    private func setState(_ s: AirPlayBoxState) {
        guard s != state else { return }
        state = s
        emit(OCBM.chCtrl, [OCBM.ctAirplayState, s.ap, s.session, s.lastEnd])
    }

    private func leaveMode() {
        guard modeActive else { return }
        modeActive = false
        steps = []
        cursor = 0
        state = AirPlayBoxState()
        emit(OCBM.chCtrl, [OCBM.ctAirplayState, 0, 0, 0])
    }

    private func control(_ pl: [UInt8]) {
        guard let op = pl.first else { return }
        switch op {
        case OCBM.ctHello:
            note { $0.hellos += 1 }
            var ack = [UInt8](repeating: 0, count: OCBM.helloAckModeLen)
            ack[0] = OCBM.ctHelloAck
            ack[1] = OCBM.version
            let caps = OCBM.capConsole | OCBM.capEcho | OCBM.capMfi | OCBM.capIp | OCBM.capFile | OCBM.capEth | OCBM.capAirplay
            ack.replaceSubrange(2..<6, with: TunnelFixture.le32(caps))
            ack[6] = modeActive ? OCBM.modeAirplay : OCBM.modeProjection
            for i in 0..<OCBM.bundleIdLen { ack[OCBM.helloAckBundleIdOff + i] = UInt8(0xA0 &+ UInt8(i)) }
            ack[OCBM.helloAckBundleStateOff] = OCBM.bundleOk
            ack[OCBM.helloAckApStateOff] = state.ap
            ack[OCBM.helloAckSessionStateOff] = state.session
            if modeActive { lastBeat = Self.now }
            emit(OCBM.chCtrl, ack)
        case OCBM.ctModeSelect where pl.count >= 2 && pl[1] == OCBM.modeAirplay:
            guard let req = AirPlayModeRequest.decode(pl) else {
                note { $0.badModeRequests += 1 }
                emit(OCBM.chCtrl, [OCBM.ctModeAck, OCBM.modeAirplay, OCBM.modeAckBadParams])
                return
            }
            note { $0.modeRequests.append(req) }
            request = req
            modeActive = true
            lastBeat = Self.now
            emit(OCBM.chCtrl, [OCBM.ctModeAck, OCBM.modeAirplay, OCBM.modeAckOk])
            state = AirPlayBoxState()
            setState(AirPlayBoxState(ap: OCBM.airplayApStarting, session: 0, lastEnd: 0))
            pair = 0
            // The measured AP bring-up is about 6 s; the script does not make the tester wait for it.
            schedule(apUpAfter: 0.8, lastEnd: OCBM.airplayEndNone)
        case OCBM.ctModeSelect where pl.count >= 2 && pl[1] == OCBM.modeProjection:
            note { $0.projectionSelects += 1 }
            leaveMode()
            emit(OCBM.chCtrl, [OCBM.ctModeAck, OCBM.modeProjection, OCBM.modeAckOk])
        case OCBM.ctHeartbeat:
            if modeActive {
                note { $0.heartbeats += 1 }
                lastBeat = Self.now
            } else {
                note { $0.heartbeatsOutsideMode += 1 }
                if Self.now - lastNudge >= 5 {
                    lastNudge = Self.now
                    emit(OCBM.chCtrl, [OCBM.ctSessionEvent, OCBM.sevHostGone])
                }
            }
        case OCBM.ctSubscribe:
            note { $0.subscribes += 1 }
            leaveMode() // projection wins
        case OCBM.ctStop:
            note { $0.stops += 1 }
            leaveMode()
        case OCBM.ctLogCtl:
            note { $0.logArmed = pl.count >= 2 && pl[1] == 1 }
            if pl.count >= 2 && pl[1] == 1 {
                emitLog("airplayd: synthetic box log armed")
                if let r = request {
                    // What MGMT_INFO's `airplay` object reports: the display and whether bit 42 is set.
                    emitLog("airplayd: display \(r.display.text), hevc \(r.hevc) (feature bit 42 \(r.hevc ? "set" : "clear"))")
                }
            }
        default:
            break
        }
    }

    private func emitLog(_ text: String) {
        let t = Array(text.utf8)
        let ms = UInt64(Date().timeIntervalSince1970 * 1000)
        let entry: [UInt8] = [OCBM.logSourceAirplayd, 0, 0, 0] + TunnelFixture.le64(ms) + [UInt8(t.count & 0xff), UInt8(t.count >> 8)] + t
        emit(OCBM.chLog, entry)
    }

    private func tick() {
        guard running, !dead else { return }
        let now = Self.now
        if modeActive && now - lastBeat >= options.heartbeatGrace / options.speed {
            note { $0.leftByWatchdog = true }
            leaveMode()
            return
        }
        guard modeActive else { return }
        let t = (now - scriptStart) * options.speed
        while cursor < steps.count, steps[cursor].at <= t {
            let s = steps[cursor]
            cursor += 1
            if let st = s.state { setState(st) }
            if !s.bytes.isEmpty { emit(s.channel, s.bytes) }
        }
        if cursor >= steps.count && !steps.isEmpty {
            steps = []
            cursor = 0
            pair += 1
            if options.pairs == 0 || pair < options.pairs {
                schedule(apUpAfter: 2.0, lastEnd: OCBM.airplayEndTeardown)
            }
        }
    }

    // MARK: - The script

    private func key(_ seed: UInt8) -> [UInt8] { (0..<32).map { UInt8(truncatingIfNeeded: Int(seed) &* 31 &+ $0 &* 7 &+ pair &* 13) } }

    private func schedule(apUpAfter: Double, lastEnd: UInt8) {
        var out: [Step] = []
        func ctl(_ at: Double, _ ap: UInt8, _ session: UInt8, _ end: UInt8) {
            out.append(Step(at: at, channel: OCBM.chCtrl, bytes: [], state: AirPlayBoxState(ap: ap, session: session, lastEnd: end)))
        }
        func lane(_ at: Double, _ channel: UInt16, _ bytes: [UInt8]) {
            out.append(Step(at: at, channel: channel, bytes: bytes, state: nil))
        }
        func ev(_ at: Double, _ name: String, _ fields: String = "") { lane(at, OCBM.chMetadata, TunnelFixture.event(name, fields)) }
        let up = OCBM.airplayApUp
        ctl(apUpAfter, up, OCBM.airplaySessionWaiting, lastEnd)
        ev(apUpAfter, "session", "\"state\":\"waiting\",\"reason\":\"\(AirPlayWire.endName(lastEnd))\"")

        // ---- session A: mirroring
        let a0 = apUpAfter + 1.5
        let sender = "\"name\":\"Synthetic Phone\",\"model\":\"Bench1,1\",\"osName\":\"iPhone OS\",\"osVersion\":\"0.0\","
            + "\"deviceID\":\"02:00:00:00:00:01\",\"address\":\"192.0.2.1\""
        ctl(a0, up, OCBM.airplaySessionConnected, lastEnd)
        ev(a0, "session", "\"state\":\"connected\",\"reason\":\"\(AirPlayWire.endName(lastEnd))\"")
        ev(a0, "sender", sender + ",\"timing\":\"NTP\",\"mirroring\":true")
        // A scid with the top bit set: it travels as a negative plist integer and exceeds 2^53.
        let vscid = 0xF000_0000_0000_0000 | UInt64(pair + 1) * 0x1_0001
        let vkey = key(1)
        // The phone's choice: HEVC with bit 42 and a canvas above 1080p, else H.264.
        let canvas = request?.display ?? .measuredDefault
        let wantsHevc = options.alternateCodecs ? pair % 2 == 1 : (request?.hevc == true && canvas.isAbove1080p)
        let useHevc = wantsHevc && media.hevc != nil
        let video = useHevc ? media.hevc! : media.video
        var geometry = TunnelFixture.Geometry()
        if video.width > 0 {
            let (w, h) = (Float(video.width), Float(video.height))
            geometry = .init(sourceWidth: w, sourceHeight: h, offsetX: (Float(canvas.width) - w) / 2, outputWidth: w, outputHeight: h)
        }
        ev(a0 + 0.1, "stream", "\"state\":\"start\",\"lane\":\"video\",\"scid\":\(vscid),\"setup\":{\"type\":110,\"latencyMs\":100,"
            + "\"streamConnectionID\":\(Int64(bitPattern: vscid))}")
        ctl(a0 + 0.1, up, OCBM.airplaySessionStreaming, lastEnd)
        ev(a0 + 0.1, "volume", "\"db\":-10")
        lane(a0 + 0.12, OCBM.chVideo, TunnelFixture.keyMessage(key: vkey, scid: vscid)
            + TunnelFixture.videoFrame(key: vkey, seq: 0, opcode: 1, plain: video.config, timestamp: 1000 << 32,
                                       geometry: geometry))
        let frame = 1.0 / (useHevc ? media.hevcFPS : media.fps)
        var sent = 0
        for (n, f) in video.frames.enumerated() {
            let at = a0 + 0.15 + Double(n) * frame
            let ts = (1000 << 32) + UInt64(Double(n) * frame * 4_294_967_296.0)
            var bytes: [UInt8] = []
            if n % 30 == 29 {
                bytes += TunnelFixture.videoFrame(key: vkey, seq: UInt64(n), opcode: 5, plain: Array("bplist00stat".utf8), timestamp: ts)
            }
            if n != options.dropVideoFrame {
                bytes += TunnelFixture.videoFrame(key: vkey, seq: UInt64(n), opcode: 0, flags: f.idr ? 0x10 : 0, plain: f.au, timestamp: ts)
                sent += 1
            }
            lane(at, OCBM.chVideo, bytes)
        }
        let videoEnd = a0 + 0.15 + Double(video.frames.count) * frame
        let videoSent = sent

        // type 96, twice: the stream ends and a new one (new scid, new key) takes over
        let units = media.audio.units
        let unit = 1024.0 / Double(media.audio.sampleRate)
        let half = max(20, min(units.count / 2, Int((videoEnd - a0 - 2.8) / 2 / unit)))
        var audioSent = 0
        var rtSeq: UInt32 = 65_500 // crosses the 16-bit wrap
        let setup96 = "\"setup\":{\"type\":96,\"ct\":4,\"audioFormat\":4194304,\"sr\":\(media.audio.sampleRate),\"spf\":1024,"
            + "\"latencyMin\":0,\"latencyMax\":7497,\"redundantAudio\":2}"
        for part in 0..<2 {
            let scid = UInt64(9600 + pair * 10 + part)
            let k = key(UInt8(10 + part))
            let t0 = a0 + 1.5 + Double(part) * (Double(half) * unit + 1.0)
            let start = "\"state\":\"start\",\"lane\":\"audio\",\"scid\":\(scid)," + setup96
            // The lanes and the events are queued apart: the second stream's lane leads its event.
            ev(part == 0 ? t0 : t0 + 0.2, "stream", start)
            lane(t0 + 0.01, OCBM.chMediaAudio, TunnelFixture.keyMessage(key: k, scid: scid)
                + TunnelFixture.audioFormat(scid: scid, rate: media.audio.sampleRate, channels: media.audio.channels))
            for n in 0..<half {
                let au = units[(part * half + n) % units.count]
                lane(t0 + 0.02 + Double(n) * unit, OCBM.chMediaAudio,
                     TunnelFixture.audioPacket(key: k, scid: scid, kind: .realtime, sequence: rtSeq & 0xFFFF,
                                               rtpTime: UInt32(n) * 1024, nonce: UInt64(n) + 1, au: au))
                if n % 43 == 0 {
                    ev(t0 + 0.02 + Double(n) * unit, "sync", "\"type\":96,\"scid\":\(scid),\"first\":\(n == 0),\"rtpPlay\":0,\"netSecs\":0,\"netFrac\":0,\"rtpNow\":\(n * 1024)")
                }
                rtSeq &+= 1
                audioSent += 1
            }
            let end = t0 + 0.02 + Double(half) * unit + 0.3
            ev(end, "stream", "\"state\":\"stop\",\"type\":96,\"scid\":\(scid),\"reason\":\"teardown\",\"messages\":\(half),\"bytes\":0,\"dropped\":0")
        }
        let aEnd = max(videoEnd, a0 + 1.5 + 2 * (Double(half) * unit + 1.0)) + 0.5
        ev(aEnd, "stream", "\"state\":\"stop\",\"type\":110,\"scid\":\(vscid),\"reason\":\"session teardown\",\"messages\":\(videoSent),\"bytes\":0,\"dropped\":0")
        ev(aEnd, "session", "\"state\":\"waiting\",\"reason\":\"teardown\"")
        ctl(aEnd + 0.05, up, OCBM.airplaySessionWaiting, OCBM.airplayEndTeardown)

        // ---- session B: music, buffered
        let b0 = aEnd + 2.0
        let td = OCBM.airplayEndTeardown
        ctl(b0, up, OCBM.airplaySessionConnected, td)
        ev(b0, "session", "\"state\":\"connected\",\"reason\":\"teardown\"")
        ev(b0, "sender", sender + ",\"timing\":\"PTP\",\"mirroring\":false")
        let bscid = UInt64(10300 + pair)
        let shk = key(20)
        ev(b0 + 0.1, "stream", "\"state\":\"start\",\"lane\":\"audio\",\"scid\":\(bscid),\"setup\":{\"type\":103,\"ct\":4,"
            + "\"audioFormat\":4194304,\"audioFormatIndex\":22,\"spf\":1024}")
        ctl(b0 + 0.1, up, OCBM.airplaySessionStreaming, td)
        lane(b0 + 0.11, OCBM.chMediaAudio, TunnelFixture.keyMessage(key: shk, scid: bscid)
            + TunnelFixture.audioFormat(scid: bscid, rate: media.audio.sampleRate, channels: media.audio.channels))
        let firstSeq: UInt32 = 70_000
        for (n, au) in units.enumerated() {
            // Eight times real time: the phone fills the receiver's buffer and tops it up.
            lane(b0 + 0.12 + Double(n) * unit / 8, OCBM.chMediaAudio,
                 TunnelFixture.audioPacket(key: shk, scid: bscid, kind: .buffered, sequence: firstSeq + UInt32(n),
                                           rtpTime: UInt32(n) * 1024, nonce: UInt64(n), au: au))
            audioSent += 1
        }
        let total = Double(units.count) * unit
        ev(b0 + 0.2, "anchor", "\"rate\":1,\"rtpTime\":0,\"networkTimeSecs\":0,\"networkTimeFrac\":0")
        ev(b0 + 0.3, "volume", "\"db\":-17.5")
        ev(b0 + 0.3, "track", "\"title\":\"Test Track\",\"artist\":\"Nobody!\",\"album\":\"Bench\",\"durationMs\":\(Int(total * 1000))")
        let jpeg = Self.artwork(seed: pair)
        ev(b0 + 0.4, "artwork", "\"bytes\":\(jpeg.count)")
        lane(b0 + 0.4, OCBM.chMetadata, TunnelFixture.meta(OCBM.metaArtwork, [0] + jpeg))
        ev(b0 + 0.5, "progress", "\"start\":0,\"current\":0,\"end\":\(units.count * 1024)")
        // a pause, then a seek: flush everything before the unit two thirds of the way in
        let play = min(total, 9.0)
        ev(b0 + play * 0.3, "anchor", "\"rate\":0,\"rtpTime\":0")
        ev(b0 + play * 0.3 + 1.0, "anchor", "\"rate\":1,\"rtpTime\":0")
        let skipTo = firstSeq + UInt32(units.count * 2 / 3)
        ev(b0 + play * 0.6, "flush", "\"flushUntilSeq\":\(skipTo),\"flushUntilTS\":\((units.count * 2 / 3) * 1024)")
        let bEnd = b0 + play + 0.5
        ev(bEnd, "stream", "\"state\":\"stop\",\"type\":103,\"scid\":\(bscid),\"reason\":\"teardown\",\"messages\":\(units.count),\"bytes\":0,\"dropped\":0")
        ev(bEnd, "session", "\"state\":\"waiting\",\"reason\":\"teardown\"")
        ctl(bEnd + 0.05, up, OCBM.airplaySessionWaiting, td)

        // Stable: steps at the same instant keep the order they were written in.
        steps = out.enumerated().sorted { ($0.element.at, $0.offset) < ($1.element.at, $1.offset) }.map(\.element)
        cursor = 0
        scriptStart = Self.now
        note {
            $0.sessionsPlayed += 2
            $0.videoFramesSent += videoSent
            $0.videoCodecs.append(useHevc ? .hevc : .h264)
            $0.audioPacketsSent += audioSent
        }
    }

    private static func artwork(seed: Int) -> [UInt8] { TunnelFixture.jpeg(seed: seed) }
}

#endif
