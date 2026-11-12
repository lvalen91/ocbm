// MediaPipeline.swift — what the app does with the client's events: keys and frames to the video
// decoder (and its pictures through the presenter to the surface), access units to the audio player,
// metadata into a snapshot the UI polls; the screen stream's rates and jitter are measured on the way.
// No AppKit, so the tests run the same object the app does.
//
// Threading: `handle` is called on the client's queue, one event at a time, in lane order. The lanes and
// CH_METADATA are queued separately on the box, so a stream's `start` event may arrive before or after
// its key and its first packets (docs/ocbm/00_PROTOCOL.md "AirPlay-only mode", Events): everything here
// is keyed by scid and works with either order, and a lane alone is enough to decode.

import CoreVideo
import Foundation
import Synchronization

final class MediaPipeline: @unchecked Sendable {
    struct StreamInfo: Equatable, Sendable, Identifiable {
        var scid: UInt64
        var lane: String
        /// 110 / 96 / 103. nil until the `start` event or a packet says which.
        var type: Int?
        var id: UInt64 { scid }
    }

    struct Snapshot: Equatable, Sendable {
        var streams: [StreamInfo] = []
        var sender: AirPlayEvent.Sender?
        /// From the opcode-1 header floats; nil when the sender leaves them zero.
        var headerSourceSize: String?
        var headerOutputSize: String?
        var lastVideoSeq: UInt64?
        var track: AirPlayEvent.Track?
        var volumeDB: Double?
        var artwork: Data?
        /// Bumped whenever `artwork` changes, so the UI decodes the JPEG once per change.
        var artworkVersion = 0
    }

    let decoder: MirrorVideoDecoder
    let presenter: FramePresenter
    let player: AirPlayAudioPlayer
    /// Arrivals on the session queue, decodes on VideoToolbox's thread, reads on the main queue.
    private let timing: TimingCell
    private let onLog: @Sendable (String) -> Void
    private let onVideoCleared: @Sendable () -> Void
    private let shared = Mutex(Snapshot())

    // Client-queue-confined.
    private var snap = Snapshot()
    private var declaredType: [UInt64: Int] = [:]
    private var formats: [UInt64: AirPlayAudioFormat] = [:]
    private var audioGeneration: [UInt64: UInt64] = [:]
    private var undecryptable: UInt64 = 0
    private var sessionLive = false
    private var videoSCID: UInt64?
    /// The current video stream's own `stop` event arrived: its remaining frames are not shown.
    private var videoStopped = false
    /// The codec of the last usable config, kept across streams so a change between sessions is logged.
    private var lastCodec: MirrorVideoCodec?

    init(audioOutput: Bool, outputScale: Float = 1,
         onFrame: @escaping @Sendable (CVPixelBuffer) -> Void,
         onVideoCleared: @escaping @Sendable () -> Void,
         onLog: @escaping @Sendable (String) -> Void) {
        let presenter = FramePresenter(present: onFrame)
        let timing = TimingCell()
        decoder = MirrorVideoDecoder { pb, time in
            let now = FramePresenter.now
            timing.meter.withLock { $0.decoded(host: now) }
            presenter.submit(pb, time: time)
        }
        self.presenter = presenter
        self.timing = timing
        player = AirPlayAudioPlayer(enableOutput: audioOutput, outputScale: outputScale)
        self.onLog = onLog
        self.onVideoCleared = onVideoCleared
    }

    var snapshot: Snapshot { shared.withLock { $0 } }

    /// Any thread: the current stream's rates and arrival jitter.
    func timingReadout(now: Double = FramePresenter.now) -> VideoTimingMeter.Readout {
        timing.meter.withLock { $0.readout(now: now) }
    }

    /// Every picture already submitted is out, nothing held is shown later, then the surface is cleared.
    private func clearPicture() {
        decoder.finish()
        presenter.cancelPending()
        onVideoCleared()
    }

    /// A new stream on the lane, or none: decoder, held pictures and measurements start over.
    private func resetVideo() {
        decoder.reset()
        presenter.cancelPending()
        timing.meter.withLock { $0.reset() }
    }

    private func publish() {
        let copy = snap
        shared.withLock { $0 = copy }
    }

    private func upsert(_ scid: UInt64, lane: String, type: Int?) {
        if let i = snap.streams.firstIndex(where: { $0.scid == scid }) {
            if let type { snap.streams[i].type = type }
            snap.streams[i].lane = lane
        } else {
            snap.streams.append(StreamInfo(scid: scid, lane: lane, type: type))
        }
    }

    /// The session is over (CT_AIRPLAY_STATE back to waiting, or the META `session` event): silence,
    /// no stream rows, no picture. The last metadata stays on show.
    ///
    /// Deliberately does NOT reset the decoder or gate the video lane. This signal carries no scid and
    /// travels on another lane than the media, so it can arrive after the NEXT session's key; a reset
    /// here would then void a live stream that has only one IDR. A picture is only gated by its own
    /// stream's `stop` event (`video stopped`), which names the scid.
    private func sessionEnded() {
        player.silence()
        if !videoStopped {
            clearPicture()
        }
        snap.streams = []
        declaredType = [:]
        formats = [:]
        audioGeneration = [:]
        sessionLive = false
        publish()
    }

    /// Every key is void: the box left the mode, the link dropped, or the mode was requested again.
    func reset() {
        sessionEnded()
        resetVideo()
        onVideoCleared()
        videoSCID = nil
        videoStopped = false
        undecryptable = 0
        snap.headerSourceSize = nil
        snap.headerOutputSize = nil
        snap.lastVideoSeq = nil
        publish()
    }

    func handle(_ event: AirPlayModeClient.Event) {
        switch event {
        case .video(let v): video(v)
        case .audio(let a): audio(a)
        case .meta(let m): meta(m)
        case .artwork(let jpeg):
            snap.artwork = jpeg
            snap.artworkVersion += 1
            publish()
        case .state(let st):
            // CT_AIRPLAY_STATE and the META `session` event say the same thing on two lanes; either
            // may come first, and ending twice is harmless.
            if st.session >= OCBM.airplaySessionConnected {
                sessionLive = true
            } else if sessionLive {
                sessionEnded()
            }
        case .reRequested, .closed:
            reset()
        case .phase, .helloAck, .boxNotReady, .note, .boxLog:
            break
        }
    }

    private func video(_ v: AirPlayVideoEvent) {
        switch v {
        case .key(let scid, let fresh):
            if fresh {
                // A new data connection: the old parameter sets and reference pictures mean nothing.
                resetVideo()
                videoSCID = scid
                videoStopped = false
                snap.streams.removeAll { $0.lane == "video" }
                upsert(scid, lane: "video", type: declaredType[scid] ?? 110)
                snap.lastVideoSeq = nil
                undecryptable = 0
                onLog("video: key for a new stream, scid \(scid)")
            } else {
                onLog("video: the same key again (the box's seam reconnected); the stream continues")
            }
            publish()
        case .config(let record, let header):
            let ok = decoder.configure(record)
            snap.headerSourceSize = header.sourceSizeText
            snap.headerOutputSize = header.outputSizeText
            let s = decoder.snapshot
            if ok, let codec = s.codec {
                if let last = lastCodec, last != codec {
                    onLog("video: codec changed \(last.rawValue) -> \(codec.rawValue) (the phone chose another codec for this stream)")
                }
                lastCodec = codec
                let container = MirrorVideoConfig.parse(record)?.container ?? "?"
                let engine = s.hardware == true ? "hardware decoder" : "SOFTWARE decoder"
                onLog("video: \(s.codecSummary) (\(container), \(record.count) B), coded \(s.width)x\(s.height), \(engine); "
                    + "header source \(header.sourceSizeText ?? "n/a") output \(header.outputSizeText ?? "n/a")")
            } else {
                onLog("video: \(s.configProblem ?? "config frame refused"); frames cannot be decoded")
            }
            publish()
        case .frame(let seq, let data, let header):
            guard !videoStopped else { return }
            let now = FramePresenter.now
            let stream = header.timestampSeconds
            timing.meter.withLock { $0.arrival(host: now, stream: stream) }
            decoder.decode(data, time: .init(arrival: now, stream: stream))
            snap.lastVideoSeq = seq
        case .gap(let expected, let got):
            onLog("video: seq jump, expected \(expected) got \(got): frames were lost. This mode has no keyframe "
                + "request and one IDR per session, so decoding continues and the picture may stay damaged")
        case .undecryptable(let seq, let haveKey):
            undecryptable += 1
            if undecryptable == 1 || undecryptable % 100 == 0 {
                onLog("video: frame seq \(seq) " + (haveKey ? "failed to authenticate" : "arrived before any key")
                    + " (\(undecryptable) so far)")
            }
        }
    }

    private func audio(_ a: AirPlayAudioEvent) {
        switch a {
        case .key(let scid, let fresh, let generation):
            audioGeneration[scid] = generation
            upsert(scid, lane: "audio", type: declaredType[scid])
            if fresh { onLog("audio: key for a new stream, scid \(scid)") }
            publish()
        case .format(let scid, let f):
            formats[scid] = f
            onLog("audio: scid \(scid) is \(f.codecName) \(f.sampleRate) Hz, \(f.channels) ch")
        case .packet(let scid, let generation, let au, let rtpHead, _, let wireKind):
            let declared = declaredType[scid]
            if declared == nil, let i = snap.streams.firstIndex(where: { $0.scid == scid }), snap.streams[i].type == nil {
                snap.streams[i].type = wireKind.streamType // the packet's own bytes, until the event arrives
                publish()
            }
            player.accept(scid: scid, generation: generation, au: au, rtpHead: rtpHead, wireKind: wireKind,
                          declaredType: declared, format: formats[scid] ?? .measuredDefault)
        }
    }

    private func meta(_ m: AirPlayEvent) {
        if let line = m.logLine { onLog("event: " + line) }
        switch m {
        case .session(let state, _):
            if state == "waiting" || state == "none" {
                if sessionLive { sessionEnded() }
            } else {
                sessionLive = true
            }
        case .sender(let s):
            snap.sender = s
            sessionLive = true
        case .streamStart(let lane, let scid, let type, _):
            if let type { declaredType[scid] = type }
            upsert(scid, lane: lane, type: type)
            sessionLive = true
        case .streamStop(let type, let scid, _, _, _, _):
            snap.streams.removeAll { $0.scid == scid }
            declaredType[scid] = nil
            formats[scid] = nil
            if scid == videoSCID {
                // This stream's frames end here, whatever is still queued on the lane behind the event.
                videoStopped = true
                clearPicture()
            } else if type != 110 {
                player.streamStopped(scid: scid, generation: audioGeneration[scid] ?? 0)
                audioGeneration[scid] = nil
            }
        case .volume(let db):
            snap.volumeDB = db
            player.setVolume(db: db)
        case .track(let t):
            snap.track = t
        case .artwork(let bytes):
            if bytes == 0 {
                snap.artwork = nil
                snap.artworkVersion += 1
            }
        case .anchor(let rate, _):
            player.setRate(rate)
        case .flush(let f):
            if let until = f.untilSeq { player.flush(from: f.fromSeq, until: until) }
        case .streamUnsupported, .progress, .sync, .other:
            break
        }
        publish()
    }
}

/// The meter behind a lock, shared by reference with the decoder's callback.
private final class TimingCell: Sendable {
    let meter = Mutex(VideoTimingMeter())
}
