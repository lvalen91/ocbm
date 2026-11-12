// AirPlayAudioPlayer.swift — raw AAC access units from the tunnel to the Mac's output.
//
//   access units -> backlog (compressed) -> CompressedAudioDecoder -> AudioPlayoutQueue (PCM)
//                -> AVAudioSourceNode render callback -> AVAudioEngine main mixer -> output device
//
// The decoder is the shipping app's `CompressedAudioDecoder` (AudioToolbox AAC-LC through
// AVAudioConverter). Its `AudioPlayer` is NOT used: it schedules on arrival and drops whatever exceeds
// a short backlog cap, which is right for CarPlay's realtime streams and wrong for AirPlay's buffered
// audio (type 103), where the phone delivers minutes ahead of real time. So the playout side is a pull:
// the render callback takes what it needs from `AudioPlayoutQueue`, and for a buffered stream the
// decoder only keeps that queue topped up while the rest waits, still compressed, in the backlog.
//
// One stream plays at a time: the one with the newest key that has sent a packet. A type 96 stream that
// ends and comes back mid-session arrives with a new scid and a new key (§4.6); the first packet of the
// newer stream switches playout to it, and stragglers of the older one are dropped.
//
// Not attempted (OPEN in docs/ocbm/02_AIRPLAY_ONLY_MODE.md §3): A/V sync and a playout clock tied to
// the phone's. Audio starts after a small cushion and runs on the Mac's output clock.
//
// Threading: every method hops onto `work`; the render callback touches only the active
// `AudioPlayoutQueue` (its own mutex) through `ActiveQueue`.

import AVFAudio
import Foundation
import Synchronization

final class AirPlayAudioPlayer: @unchecked Sendable {
    struct Stats: Equatable, Sendable {
        var scid: UInt64?
        var kind: AirPlayAudioKind?
        var format: AirPlayAudioFormat?
        /// Access units accepted for playout since the app started / for the current stream.
        var accepted: UInt64 = 0
        var acceptedThisStream: UInt64 = 0
        var decoded: UInt64 = 0
        var decodeErrors: UInt64 = 0
        /// Packets of a stream that is not (or no longer) the one playing.
        var stale: UInt64 = 0
        var streamSwitches: UInt64 = 0
        var backlogUnits = 0
        var backlogBytes = 0
        var backlogDropped: UInt64 = 0
        var flushedUnits: UInt64 = 0
        var queue = AudioPlayoutQueue.Stats()
        /// Underruns / trims of streams already replaced, so the totals never go backwards.
        var underrunsBefore: UInt64 = 0
        var trimsBefore: UInt64 = 0
        var outputRunning = false
        var outputError: String?
        var gain: Float = 1

        var underruns: UInt64 { underrunsBefore + queue.underruns }
        var trims: UInt64 { trimsBefore + queue.trims }
    }

    private final class ActiveQueue: Sendable {
        let queue = Mutex<AudioPlayoutQueue?>(nil)
    }

    private struct Unit {
        var sequence: UInt32
        var au: Data
    }

    private struct Current {
        var scid: UInt64
        var generation: UInt64
        var kind: AirPlayAudioKind
        var format: AirPlayAudioFormat
        var decoder: CompressedAudioDecoder?
        var queue: AudioPlayoutQueue
    }

    /// Seconds of PCM kept decoded ahead for a buffered stream; the rest stays compressed.
    private static let bufferedHighWater = 1.5
    private static let backlogCapBytes = 64 * 1024 * 1024

    private let work = DispatchQueue(label: "airplay-test.audio", qos: .userInitiated)
    private let active = ActiveQueue()
    private let shared = Mutex(Stats())
    private let enableOutput: Bool

    // `work`-confined.
    private var stats = Stats()
    private var current: Current?
    private var backlog: [Unit] = []
    private var backlogHead = 0
    /// scid -> the generation that was stopped. A scid that comes back under a NEWER key is a new
    /// stream (a sender may reuse its ids from one session to the next) and plays.
    private var closed: [UInt64: UInt64] = [:]
    private var engine: AVAudioEngine?
    private var engineRate = 0.0
    private var gain: Float = 1
    private var pumpTimer: DispatchSourceTimer?
    private var configObserver: NSObjectProtocol?

    /// Multiplies the output gain (bench runs that must stay quiet). Set before audio flows.
    private let outputScale: Float

    /// `enableOutput: false` builds no AVAudioEngine: the tests pull the PCM with `renderOffline`.
    init(enableOutput: Bool = true, outputScale: Float = 1) {
        self.enableOutput = enableOutput
        self.outputScale = outputScale
    }

    var snapshot: Stats {
        var s = shared.withLock { $0 }
        if let q = active.queue.withLock({ $0 }) { s.queue = q.stats }
        return s
    }

    private static var nowNs: UInt64 { DispatchTime.now().uptimeNanoseconds }

    // MARK: - Inputs (any thread)

    /// One decrypted access unit. `declaredType` = the SETUP type from the stream's `start` event, when
    /// it has arrived (the lane alone is enough: the packet's own bytes decide until then).
    func accept(scid: UInt64, generation: UInt64, au: Data, rtpHead: UInt32, wireKind: AirPlayAudioKind,
                declaredType: Int?, format: AirPlayAudioFormat) {
        work.async { [self] in
            if let stopped = closed[scid] {
                if generation <= stopped {
                    stats.stale += 1
                    return publish()
                }
                closed[scid] = nil
            }
            let kind: AirPlayAudioKind = declaredType == 96 ? .realtime : declaredType == 103 ? .buffered : wireKind
            if let c = current, c.scid == scid {
                if c.kind != kind || c.format != format {
                    // The `start` event or the SEAM_FORMAT caught up with the lane.
                    if c.format != format { current?.decoder = Self.makeDecoder(format) }
                    current?.kind = kind
                    current?.format = format
                    c.queue.setPolicy(kind == .realtime ? .realtime : .buffered)
                    stats.kind = kind
                    stats.format = format
                }
            } else {
                if let c = current, generation < c.generation {
                    stats.stale += 1 // a straggler of the stream this one replaced
                    return publish()
                }
                begin(scid: scid, generation: generation, kind: kind, format: format)
            }
            guard let cur = current else { return }
            backlog.append(Unit(sequence: cur.kind.sequence(rtpHead: rtpHead), au: au))
            stats.accepted += 1
            stats.acceptedThisStream += 1
            stats.backlogBytes += au.count
            while stats.backlogBytes > Self.backlogCapBytes, backlogHead < backlog.count - 1 {
                stats.backlogBytes -= backlog[backlogHead].au.count
                backlog[backlogHead].au = Data()
                backlogHead += 1
                stats.backlogDropped += 1
            }
            pump()
            publish()
        }
    }

    /// The box reported the stream's TEARDOWN (or the session's end). Packets still in flight for it
    /// are dropped from here on; a buffered stream goes silent at once, a realtime one plays out the
    /// fraction of a second it has queued.
    func streamStopped(scid: UInt64, generation: UInt64) {
        work.async { [self] in
            if closed.count > 64 { closed = [:] } // stragglers arrive within moments, not sessions later
            closed[scid] = max(generation, current?.scid == scid ? current?.generation ?? 0 : 0)
            guard let c = current, c.scid == scid else { return }
            dropBacklog()
            if c.kind == .buffered { c.queue.clear() }
            retire(c)
            current = nil
            stats.scid = nil
            stats.kind = nil
            publish()
        }
    }

    /// The session is over, or the link is gone: silence, now.
    func silence() {
        work.async { [self] in
            dropBacklog()
            if let c = current {
                c.queue.clear()
                retire(c)
            }
            active.queue.withLock { $0?.clear() }
            current = nil
            stats.scid = nil
            stats.kind = nil
            stats.format = nil
            publish()
        }
    }

    /// FLUSHBUFFERED: drop buffered audio before `until` (from `from`, when the phone names a start).
    func flush(from: UInt32?, until: UInt32) {
        work.async { [self] in
            guard let c = current, c.kind == .buffered else { return }
            let m = c.kind.sequenceModulus
            var kept: [Unit] = []
            for u in backlog[backlogHead...] {
                if SequenceRange.contains(u.sequence, from: from, until: until, modulus: m) {
                    stats.flushedUnits += 1
                    stats.backlogBytes -= u.au.count
                } else {
                    kept.append(u)
                }
            }
            backlog = kept
            backlogHead = 0
            c.queue.flush(from: from, until: until, modulus: m)
            pump()
            publish()
        }
    }

    /// SETRATEANCHORTIME: rate 0 pauses a buffered stream, anything else resumes it.
    func setRate(_ rate: Double) {
        work.async { [self] in
            guard let c = current, c.kind == .buffered else { return }
            c.queue.setPaused(rate == 0)
            publish()
        }
    }

    /// The phone's `volume` (dB: 0 full, -30 slider at 0, -144 mute) as the output gain.
    func setVolume(db: Double) {
        work.async { [self] in
            gain = Float(AirPlayVolume.sliderPosition(db: db))
            engine?.mainMixerNode.outputVolume = gain * outputScale
            stats.gain = gain
            publish()
        }
    }

    func shutdown() {
        work.sync {
            pumpTimer?.cancel()
            pumpTimer = nil
            if let configObserver { NotificationCenter.default.removeObserver(configObserver) }
            configObserver = nil
            engine?.stop()
            engine = nil
            active.queue.withLock { $0 = nil }
            current = nil
            backlog = []
            backlogHead = 0
        }
    }

    // MARK: - Tests

    /// Everything accepted so far has been through the decoder (as far as the policy lets it).
    func settle() { work.sync {} }

    /// Pull `frames` frames the way the render callback would. Returns (left, right, frames of real audio).
    func renderOffline(frames: Int) -> (left: [Float], right: [Float], produced: Int) {
        let nowNs = Self.nowNs
        var l = [Float](repeating: 0, count: frames)
        var r = [Float](repeating: 0, count: frames)
        guard let q = active.queue.withLock({ $0 }) else { return (l, r, 0) }
        let n = l.withUnsafeMutableBufferPointer { lp in
            r.withUnsafeMutableBufferPointer { rp in
                q.render(frames: frames, left: lp.baseAddress!, right: rp.baseAddress!, nowNs: nowNs)
            }
        }
        work.sync {
            pump()
            publish()
        }
        return (l, r, n)
    }

    // MARK: - `work`-confined

    private func publish() {
        stats.backlogUnits = backlog.count - backlogHead
        let copy = stats
        shared.withLock { $0 = copy }
    }

    private static func makeDecoder(_ f: AirPlayAudioFormat) -> CompressedAudioDecoder? {
        CompressedAudioDecoder(codec: f.codec, sampleRate: Double(f.sampleRate), channels: UInt32(f.channels))
    }

    private func retire(_ c: Current) {
        let q = c.queue.stats
        stats.underrunsBefore += q.underruns
        stats.trimsBefore += q.trims
    }

    private func dropBacklog() {
        backlog = []
        backlogHead = 0
        stats.backlogBytes = 0
    }

    private func begin(scid: UInt64, generation: UInt64, kind: AirPlayAudioKind, format: AirPlayAudioFormat) {
        if let old = current {
            old.queue.clear()
            retire(old)
        }
        dropBacklog()
        let rate = Double(format.sampleRate == 0 ? 44100 : format.sampleRate)
        let queue = AudioPlayoutQueue(sampleRate: rate, channels: Int(max(1, format.channels)),
                                      policy: kind == .realtime ? .realtime : .buffered)
        current = Current(scid: scid, generation: generation, kind: kind, format: format,
                          decoder: Self.makeDecoder(format), queue: queue)
        active.queue.withLock { $0 = queue }
        stats.scid = scid
        stats.kind = kind
        stats.format = format
        stats.acceptedThisStream = 0
        stats.streamSwitches += 1
        if enableOutput { ensureEngine(rate: rate) }
    }

    /// Decode from the backlog into the playout queue: everything for a realtime stream, up to the high
    /// water mark for a buffered one.
    private func pump() {
        guard let c = current else { return }
        let limit = c.kind == .buffered ? Self.bufferedHighWater : Double.infinity
        while backlogHead < backlog.count, c.queue.queuedSeconds < limit {
            let u = backlog[backlogHead]
            backlog[backlogHead].au = Data()
            backlogHead += 1
            stats.backlogBytes -= u.au.count
            guard let dec = c.decoder, let pcm = dec.decode(u.au), !pcm.isEmpty else {
                stats.decodeErrors += 1
                continue
            }
            let samples = pcm.withUnsafeBytes { Array($0.bindMemory(to: Int16.self)) }
            c.queue.push(sequence: u.sequence, samples: samples, nowNs: Self.nowNs)
            stats.decoded += 1
        }
        if backlogHead > 512 {
            backlog.removeFirst(backlogHead)
            backlogHead = 0
        }
        // A buffered stream is decoded as playout makes room, not as packets arrive: keep ticking
        // while anything is waiting.
        let waiting = backlogHead < backlog.count
        if waiting && pumpTimer == nil {
            let t = DispatchSource.makeTimerSource(queue: work)
            t.schedule(deadline: .now() + 0.05, repeating: 0.05, leeway: .milliseconds(10))
            t.setEventHandler { [weak self] in
                guard let self else { return }
                self.pump()
                self.publish()
            }
            pumpTimer = t
            t.resume()
        } else if !waiting, let t = pumpTimer {
            t.cancel()
            pumpTimer = nil
        }
    }

    private func ensureEngine(rate: Double) {
        if engine != nil && engineRate == rate { return }
        engine?.stop()
        engine = nil
        guard let fmt = AVAudioFormat(standardFormatWithSampleRate: rate, channels: 2) else {
            stats.outputError = "no output format for \(Int(rate)) Hz"
            return
        }
        let e = AVAudioEngine()
        let active = self.active
        let node = AVAudioSourceNode(format: fmt) { _, _, frameCount, bufferList -> OSStatus in
            let bufs = UnsafeMutableAudioBufferListPointer(bufferList)
            let n = Int(frameCount)
            guard bufs.count > 0, let l = bufs[0].mData?.assumingMemoryBound(to: Float.self) else { return noErr }
            let r = bufs.count > 1 ? bufs[1].mData?.assumingMemoryBound(to: Float.self) : nil
            if let q = active.queue.withLock({ $0 }) {
                q.render(frames: n, left: l, right: r, nowNs: DispatchTime.now().uptimeNanoseconds)
            } else {
                for i in 0..<n {
                    l[i] = 0
                    r?[i] = 0
                }
            }
            return noErr
        }
        e.attach(node)
        do {
            try e.connectNode(node, to: e.mainMixerNode, format: fmt)
            e.mainMixerNode.outputVolume = gain * outputScale
            try e.start()
            engine = e
            engineRate = rate
            stats.outputRunning = true
            stats.outputError = nil
        } catch {
            stats.outputRunning = false
            stats.outputError = error.localizedDescription
        }
        if configObserver == nil {
            // The output device changed (headphones, sample rate): the engine stops itself and has to
            // be rebuilt against the new hardware format.
            configObserver = NotificationCenter.default.addObserver(
                forName: .AVAudioEngineConfigurationChange, object: nil, queue: nil
            ) { [weak self] note in
                guard let self else { return }
                let changed = (note.object as AnyObject?).map { ObjectIdentifier($0) }
                self.work.async {
                    guard let cur = self.engine, changed == ObjectIdentifier(cur) else { return }
                    let rate = self.engineRate
                    self.engine = nil
                    self.stats.outputRunning = false
                    self.ensureEngine(rate: rate)
                    self.publish()
                }
            }
        }
    }
}
