// AudioPlayoutQueue.swift — decoded PCM between the AAC decoder and the output's render callback, with
// the two playout policies AirPlay-only mode needs (docs/ocbm/02_AIRPLAY_ONLY_MODE.md §4.6):
//
//   realtime (type 96)  packets arrive at playback rate. Start after a small cushion; if the queue ever
//                       grows past `maxDepth` (a burst, or a sender faster than the output clock), drop
//                       the oldest audio back down to `trimTo` so latency cannot build up.
//   buffered (type 103) the phone sends far ahead of real time. Never trimmed here; the decoder only
//                       tops this queue up (`AirPlayAudioPlayer`), and `flush` / `setPaused` follow the
//                       phone's FLUSHBUFFERED and SETRATEANCHORTIME.
//
// Pure (no AudioToolbox): the render side is a plain function over two float buffers, so the tests run
// it without an audio device. One mutex guards everything; the render callback holds it for one copy.

import Foundation
import Synchronization

/// Membership in a wrapping sequence-number range, as FLUSHBUFFERED names one.
enum SequenceRange {
    /// Is `seq` before `until` (and at or after `from`, when given) in a space of `modulus` values?
    /// "Before" means behind by less than half the space, the usual serial-number rule.
    static func contains(_ seq: UInt32, from: UInt32?, until: UInt32, modulus: UInt32) -> Bool {
        let m = UInt64(modulus)
        func dist(_ a: UInt32, _ b: UInt32) -> UInt64 { (UInt64(b % modulus) + m - UInt64(a % modulus)) % m } // a -> b
        if let from {
            return dist(from, seq) < dist(from, until)
        }
        let d = dist(seq, until)
        return d >= 1 && d <= m / 2
    }
}

final class AudioPlayoutQueue: Sendable {
    struct Policy: Equatable, Sendable {
        /// Seconds queued before playout starts (and restarts after the queue ran dry).
        var startThreshold: Double
        /// Seconds queued above which the oldest audio is dropped. nil = never.
        var maxDepth: Double?
        /// Seconds kept by such a drop.
        var trimTo: Double

        static let realtime = Policy(startThreshold: 0.12, maxDepth: 1.0, trimTo: 0.25)
        static let buffered = Policy(startThreshold: 0.25, maxDepth: nil, trimTo: 0)
    }

    struct Stats: Equatable, Sendable {
        var queuedFrames = 0
        var playedFrames: UInt64 = 0
        /// Times playout ran dry and audio for the same stream arrived shortly after: audible gaps.
        var underruns: UInt64 = 0
        var trims: UInt64 = 0
        var trimmedFrames: UInt64 = 0
        var flushedFrames: UInt64 = 0
        var playing = false
        var paused = false
    }

    private struct Chunk {
        var sequence: UInt32
        var samples: [Int16] // interleaved
        var offset = 0       // frames already played
    }

    private struct State {
        var policy: Policy
        var chunks: [Chunk] = []
        var head = 0
        var stats = Stats()
        var dryAtNs: UInt64?
        var lastPushNs: UInt64 = 0
    }

    /// A dry spell shorter than this is ordinary scheduling; one longer than `maxUnderrunNs` is a
    /// stream that stopped, not one that starved.
    private static let minUnderrunNs: UInt64 = 2_000_000
    private static let maxUnderrunNs: UInt64 = 2_000_000_000
    /// With nothing new for this long, play what is queued even below the start threshold (the tail of
    /// a stream would otherwise never be heard).
    private static let tailAfterNs: UInt64 = 300_000_000

    let sampleRate: Double
    let channels: Int
    private let state: Mutex<State>

    init(sampleRate: Double, channels: Int, policy: Policy) {
        self.sampleRate = sampleRate
        self.channels = max(1, channels)
        state = Mutex(State(policy: policy))
    }

    var stats: Stats { state.withLock { $0.stats } }
    var queuedSeconds: Double { Double(state.withLock { $0.stats.queuedFrames }) / sampleRate }

    func setPolicy(_ p: Policy) { state.withLock { $0.policy = p } }

    /// Append one decoded access unit. `samples` is interleaved, `channels` per frame.
    func push(sequence: UInt32, samples: [Int16], nowNs: UInt64) {
        let frames = samples.count / channels
        guard frames > 0 else { return }
        state.withLock { s in
            s.chunks.append(Chunk(sequence: sequence, samples: samples))
            s.stats.queuedFrames += frames
            s.lastPushNs = nowNs
            if let dry = s.dryAtNs {
                s.dryAtNs = nil
                let gap = nowNs &- dry
                if gap >= Self.minUnderrunNs && gap <= Self.maxUnderrunNs { s.stats.underruns += 1 }
            }
            if let maxDepth = s.policy.maxDepth, Double(s.stats.queuedFrames) > maxDepth * sampleRate {
                let keep = Int(s.policy.trimTo * sampleRate)
                var dropped = 0
                while s.head < s.chunks.count - 1, s.stats.queuedFrames - dropped > keep {
                    let c = s.chunks[s.head]
                    dropped += c.samples.count / channels - c.offset
                    s.chunks[s.head].samples = []
                    s.head += 1
                }
                s.stats.queuedFrames -= dropped
                s.stats.trimmedFrames += UInt64(dropped)
                s.stats.trims += 1
            }
            if !s.stats.playing && Double(s.stats.queuedFrames) >= s.policy.startThreshold * sampleRate {
                s.stats.playing = true
            }
            Self.compact(&s)
        }
    }

    /// Fill `frames` frames of output (silence where there is no audio). Returns the frames that were
    /// real audio. `right` may equal `left` for a mono output.
    @discardableResult
    func render(frames: Int, left: UnsafeMutablePointer<Float>, right: UnsafeMutablePointer<Float>?, nowNs: UInt64) -> Int {
        let ch = channels
        let produced = state.withLock { s -> Int in
            if s.stats.paused { return 0 }
            if !s.stats.playing {
                // The tail rule: nothing new for a while, so what is queued is all there will be.
                guard s.stats.queuedFrames > 0, nowNs &- s.lastPushNs >= Self.tailAfterNs else { return 0 }
                s.stats.playing = true
            }
            var done = 0
            while done < frames, s.head < s.chunks.count {
                let total = s.chunks[s.head].samples.count / ch
                let off = s.chunks[s.head].offset
                let n = min(frames - done, total - off)
                s.chunks[s.head].samples.withUnsafeBufferPointer { p in
                    for i in 0..<n {
                        let base = (off + i) * ch
                        let l = Float(p[base]) / 32768
                        left[done + i] = l
                        if let right { right[done + i] = ch > 1 ? Float(p[base + 1]) / 32768 : l }
                    }
                }
                done += n
                if off + n == total {
                    s.chunks[s.head].samples = []
                    s.head += 1
                } else {
                    s.chunks[s.head].offset = off + n
                }
            }
            s.stats.queuedFrames -= done
            s.stats.playedFrames += UInt64(done)
            if done < frames {
                s.stats.playing = false // ran dry: build the cushion again before resuming
                s.dryAtNs = nowNs
            }
            return done
        }
        if produced < frames {
            for i in produced..<frames {
                left[i] = 0
                right?[i] = 0
            }
        }
        return produced
    }

    /// Drop queued audio whose sequence number is before `until` (and at or after `from`, when given).
    /// Returns the frames removed.
    @discardableResult
    func flush(from: UInt32?, until: UInt32, modulus: UInt32) -> Int {
        state.withLock { s in
            var removed = 0
            var kept: [Chunk] = []
            for c in s.chunks[s.head...] {
                if SequenceRange.contains(c.sequence, from: from, until: until, modulus: modulus) {
                    removed += c.samples.count / channels - c.offset
                } else {
                    kept.append(c)
                }
            }
            s.chunks = kept
            s.head = 0
            s.stats.queuedFrames -= removed
            s.stats.flushedFrames += UInt64(removed)
            if kept.isEmpty {
                s.stats.playing = false
                s.dryAtNs = nil // emptied on request: not an underrun
            }
            return removed
        }
    }

    /// Drop everything (stream replaced, stream stopped, session over). Tallies survive.
    func clear() {
        state.withLock { s in
            s.chunks = []
            s.head = 0
            s.stats.queuedFrames = 0
            s.stats.playing = false
            s.dryAtNs = nil
        }
    }

    /// SETRATEANCHORTIME rate 0 / 1. Paused audio stays queued.
    func setPaused(_ paused: Bool) {
        state.withLock { s in
            s.stats.paused = paused
            if paused { s.dryAtNs = nil }
        }
    }

    private static func compact(_ s: inout State) {
        if s.head > 256 {
            s.chunks.removeFirst(s.head)
            s.head = 0
        }
    }
}
