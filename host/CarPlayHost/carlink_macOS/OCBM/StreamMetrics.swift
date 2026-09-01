// StreamMetrics.swift — received-stream health accounting for the live A/V performance monitor.
//
// The Mac app receives ALL CarPlay A/V over the OCBM USB link and decodes it (VideoToolbox), so it is
// the right place to measure what actually arrives and renders. The weak single-core ARM adapter only
// forwards; these numbers describe the HOST's received stream, not the box.
//
// Two layers, both IOKit/AppKit-free and Sendable so `tests/run_tests.sh` can exercise the rate math
// with injected timestamps (no real clock, no sockets):
//
//   • StreamMetricsAccumulator — the mutable collector. It lives INSIDE OCBMAVDecrypt's existing stats
//     Mutex (no new lock on the A/V hot path) and is fed a few integer adds per parsed message via
//     `record(...)`. Cumulative counters only; `snapshot(at:)` freezes them into an immutable value and
//     rotates the per-interval frame-size min/max bucket.
//   • The pure derivation — `StreamMetricsReport.rates(from:to:dt:)` turns two cumulative snapshots and a
//     time delta into Mbps / fps (or pkt/s) / frame-size / loss / jitter. No state, no clock: fully unit
//     testable. min/avg come from cumulative diffs; min/max are read from the newer snapshot's rotated
//     interval bucket.
//
// "Frame" means a video access unit (main / alt lane); for audio it means an RTP packet — the derived
// `framesPerSec` reads as fps for video and packets/sec for audio.

import Foundation
import Synchronization

/// The four logical streams the monitor tracks independently. Media vs voice audio is routed by the
/// SEAM_FORMAT `audioType` (isVoice), exactly as the render bridge routes to the media vs voice sink.
enum StreamKind: Int, CaseIterable, Sendable {
    case mainVideo
    case altVideo
    case mediaAudio
    case voiceAudio

    var label: String {
        switch self {
        case .mainVideo:  return "Main video"
        case .altVideo:   return "Alt video"
        case .mediaAudio: return "Media audio"
        case .voiceAudio: return "Voice audio"
        }
    }

    var isVideo: Bool { self == .mainVideo || self == .altVideo }
}

/// One consistent, immutable copy of a single stream's cumulative counters at a point in time, plus the
/// per-interval frame-size bucket (min/max since the previous snapshot). Sendable: crosses from the USB
/// read queue (writer) to the monitor's timer (reader) as a value.
struct StreamStatSnapshot: Sendable, Equatable {
    /// Total received bytes for this stream (sum of parsed message lengths).
    var bytes: UInt64 = 0
    /// Parsed messages (frames + keys + config + format markers).
    var messages: UInt64 = 0
    /// Successfully processed units — decoded video AUs, or accepted audio packets. Drives fps / pkt-s.
    var frames: UInt64 = 0
    /// Seq-gap events (lost frames) detected on the video seam; 0 for audio.
    var gaps: UInt64 = 0
    /// Decrypt / validation failures.
    var decryptFails: UInt64 = 0
    /// Cumulative sum of per-frame size samples — `(Δ sizeSum) / (Δ frames)` is the interval average.
    var sizeSum: UInt64 = 0
    /// Smallest / largest frame size (bytes) seen SINCE THE PREVIOUS snapshot (rotated on snapshot).
    var intervalMinBytes: UInt32 = 0
    var intervalMaxBytes: UInt32 = 0
    /// Frame samples in the interval bucket (0 ⇒ min/max are not meaningful).
    var intervalFrames: UInt64 = 0
    /// Smoothed inter-arrival jitter (ms) — an RFC3550-style running estimate; reads as stutter. 0 for
    /// audio (only video frames feed it).
    var jitterMs: Double = 0
    /// Last observed audio format for this stream (nil for video / before the box sends SEAM_FORMAT).
    var format: OCBMAudioStreamFormat? = nil
    /// Video codec label ("HEVC" / "H.264") latched from the decoder's config parse; nil for audio /
    /// before the first VideoConfig. The video analogue of `format` (video has no OCBMAudioStreamFormat).
    var videoCodec: String? = nil
}

/// A full snapshot of all four streams at one timestamp. `timestamp` is caller-supplied seconds
/// (monotonic in the app; injected in tests) so the derivation is clock-free.
struct StreamMetricsSnapshot: Sendable {
    var timestamp: Double = 0
    var main = StreamStatSnapshot()
    var alt = StreamStatSnapshot()
    var media = StreamStatSnapshot()
    var voice = StreamStatSnapshot()

    subscript(_ kind: StreamKind) -> StreamStatSnapshot {
        switch kind {
        case .mainVideo:  return main
        case .altVideo:   return alt
        case .mediaAudio: return media
        case .voiceAudio: return voice
        }
    }
}

/// The derived, human-facing rates for one stream over an interval. Pure output of the math below.
struct StreamRates: Sendable, Equatable {
    var mbps: Double = 0
    /// Video: frames/sec. Audio: packets/sec (same field; the UI labels it per stream kind).
    var framesPerSec: Double = 0
    var avgFrameBytes: Double = 0
    var minFrameBytes: UInt32 = 0
    var maxFrameBytes: UInt32 = 0
    /// Gaps (lost frames) per second over the interval.
    var lossPerSec: Double = 0
    /// Absolute gap / fail counts accrued in the interval (for a running total the UI can show).
    var gapsDelta: UInt64 = 0
    var decryptFailDelta: UInt64 = 0
    var jitterMs: Double = 0
    var format: OCBMAudioStreamFormat? = nil
    var videoCodec: String? = nil
}

/// The report the monitor renders each tick: the interval it covers plus per-stream derived rates.
struct StreamMetricsReport: Sendable {
    var dt: Double = 0
    var rates: [StreamKind: StreamRates] = [:]

    subscript(_ kind: StreamKind) -> StreamRates { rates[kind] ?? StreamRates() }

    /// Derive per-stream rates from two cumulative snapshots. `dt` comes from the snapshot timestamps;
    /// a non-positive dt (clock stall / same instant) yields an all-zero report rather than dividing by
    /// zero. Pure — the single testable entry point for the whole rate math.
    static func between(_ a: StreamMetricsSnapshot, _ b: StreamMetricsSnapshot) -> StreamMetricsReport {
        let dt = b.timestamp - a.timestamp
        var report = StreamMetricsReport(dt: dt)
        for kind in StreamKind.allCases {
            report.rates[kind] = rates(from: a[kind], to: b[kind], dt: dt)
        }
        return report
    }

    /// The core arithmetic for one stream. `&-` throughout: counters are monotonic, so a diff never
    /// underflows in practice, and wrapping subtraction avoids a trap on the (impossible) reset case.
    static func rates(from a: StreamStatSnapshot, to b: StreamStatSnapshot, dt: Double) -> StreamRates {
        guard dt > 0 else {
            // No time base — surface only the point-in-time frame-size / format, no rates.
            return StreamRates(minFrameBytes: b.intervalMinBytes, maxFrameBytes: b.intervalMaxBytes,
                               format: b.format, videoCodec: b.videoCodec)
        }
        let dBytes  = b.bytes &- a.bytes
        let dFrames = b.frames &- a.frames
        let dGaps   = b.gaps &- a.gaps
        let dFails  = b.decryptFails &- a.decryptFails
        let dSize   = b.sizeSum &- a.sizeSum

        return StreamRates(
            mbps: Double(dBytes) * 8.0 / dt / 1_000_000.0,
            framesPerSec: Double(dFrames) / dt,
            avgFrameBytes: dFrames > 0 ? Double(dSize) / Double(dFrames) : 0,
            minFrameBytes: b.intervalFrames > 0 ? b.intervalMinBytes : 0,
            maxFrameBytes: b.intervalFrames > 0 ? b.intervalMaxBytes : 0,
            lossPerSec: Double(dGaps) / dt,
            gapsDelta: dGaps,
            decryptFailDelta: dFails,
            jitterMs: b.jitterMs,
            format: b.format,
            videoCodec: b.videoCodec)
    }
}

// MARK: - Collector

/// The mutable per-stream accumulator. One instance per StreamKind lives in `StreamMetricsAccumulator`,
/// which itself lives inside OCBMAVDecrypt's stats Mutex — so `record` runs under the SAME lock the six
/// decrypt tallies already take, adding no new lock to the A/V hot path and doing only integer adds.
struct PerStreamMetrics: Sendable {
    private var bytes: UInt64 = 0
    private var messages: UInt64 = 0
    private var frames: UInt64 = 0
    private var gaps: UInt64 = 0
    private var decryptFails: UInt64 = 0
    private var sizeSum: UInt64 = 0

    // Interval frame-size bucket — reset each snapshot so min/max describe only the last interval.
    private var intMin: UInt32 = 0
    private var intMax: UInt32 = 0
    private var intFrames: UInt64 = 0

    // Jitter (video only): RFC3550-style smoothed |Δinter-arrival|. All in nanoseconds until snapshot.
    private var lastArrivalNs: UInt64 = 0
    private var lastIntervalNs: Double = 0
    private var jitterNs: Double = 0

    private var format: OCBMAudioStreamFormat? = nil
    private var videoCodec: String? = nil

    /// Record one parsed message. `sizeSample` (nil for keys/config/format markers) feeds the frame-size
    /// stats and marks the message as a countable frame/packet. `nowNs` is a monotonic clock read for
    /// jitter (0 disables it — audio passes 0). Every argument is an Int/Bool/UInt: no allocation.
    mutating func record(bytes: Int, sizeSample: UInt32?, gap: Bool, fail: Bool, nowNs: UInt64) {
        self.bytes &+= UInt64(bytes)
        messages &+= 1
        if gap { gaps &+= 1 }
        if fail { decryptFails &+= 1 }
        guard let s = sizeSample else { return }   // key / config / format marker — byte-counted only
        frames &+= 1
        sizeSum &+= UInt64(s)
        if intFrames == 0 { intMin = s; intMax = s } else { intMin = min(intMin, s); intMax = max(intMax, s) }
        intFrames &+= 1
        if nowNs != 0 {
            if lastArrivalNs != 0 {
                let interval = Double(nowNs &- lastArrivalNs)
                if lastIntervalNs != 0 {
                    let d = abs(interval - lastIntervalNs)
                    jitterNs += (d - jitterNs) / 16.0   // RFC3550 gain
                }
                lastIntervalNs = interval
            }
            lastArrivalNs = nowNs
        }
    }

    /// Record a new/changed audio format for this stream (SEAM_FORMAT). Byte-counted separately by the
    /// message's own `record`; this just latches the codec/rate/channels for display.
    mutating func setFormat(_ f: OCBMAudioStreamFormat) { format = f }

    /// Latch the video codec label ("HEVC" / "H.264") for display — the video analogue of `setFormat`,
    /// fed from the decoder's authoritative avcC/hvcC parse (single source of truth, no re-sniff).
    mutating func setVideoCodec(_ label: String) { videoCodec = label }

    /// Freeze into an immutable snapshot and ROTATE the interval bucket (min/max) so the next interval
    /// starts clean. Cumulative counters carry forward; only the per-interval bucket resets.
    mutating func snapshot() -> StreamStatSnapshot {
        let s = StreamStatSnapshot(
            bytes: bytes, messages: messages, frames: frames, gaps: gaps, decryptFails: decryptFails,
            sizeSum: sizeSum,
            intervalMinBytes: intFrames > 0 ? intMin : 0,
            intervalMaxBytes: intFrames > 0 ? intMax : 0,
            intervalFrames: intFrames,
            jitterMs: jitterNs / 1_000_000.0,
            format: format,
            videoCodec: videoCodec)
        intMin = 0; intMax = 0; intFrames = 0   // rotate the interval frame-size bucket
        return s
    }
}

/// All four per-stream accumulators. Held inside OCBMAVDecrypt's stats Mutex.
struct StreamMetricsAccumulator: Sendable {
    private var main = PerStreamMetrics()
    private var alt = PerStreamMetrics()
    private var media = PerStreamMetrics()
    private var voice = PerStreamMetrics()

    mutating func record(_ kind: StreamKind, bytes: Int, sizeSample: UInt32?,
                         gap: Bool, fail: Bool, nowNs: UInt64) {
        switch kind {
        case .mainVideo:  main.record(bytes: bytes, sizeSample: sizeSample, gap: gap, fail: fail, nowNs: nowNs)
        case .altVideo:   alt.record(bytes: bytes, sizeSample: sizeSample, gap: gap, fail: fail, nowNs: nowNs)
        case .mediaAudio: media.record(bytes: bytes, sizeSample: sizeSample, gap: gap, fail: fail, nowNs: nowNs)
        case .voiceAudio: voice.record(bytes: bytes, sizeSample: sizeSample, gap: gap, fail: fail, nowNs: nowNs)
        }
    }

    mutating func setFormat(_ kind: StreamKind, _ f: OCBMAudioStreamFormat) {
        switch kind {
        case .mainVideo:  main.setFormat(f)
        case .altVideo:   alt.setFormat(f)
        case .mediaAudio: media.setFormat(f)
        case .voiceAudio: voice.setFormat(f)
        }
    }

    mutating func setVideoCodec(_ kind: StreamKind, _ label: String) {
        switch kind {
        case .mainVideo:  main.setVideoCodec(label)
        case .altVideo:   alt.setVideoCodec(label)
        case .mediaAudio, .voiceAudio: break   // audio uses setFormat
        }
    }

    /// Freeze all four streams at `timestamp` (caller-supplied seconds) and rotate each interval bucket.
    mutating func snapshot(at timestamp: Double) -> StreamMetricsSnapshot {
        StreamMetricsSnapshot(timestamp: timestamp,
                              main: main.snapshot(), alt: alt.snapshot(),
                              media: media.snapshot(), voice: voice.snapshot())
    }
}

// MARK: - Anomaly events (the timestamped "what the user hears/sees" layer)

/// One kind of A/V anomaly. Raw values are the STABLE strings written to JSON / the log — do not rename.
enum AnomalyKind: String, Sendable, CaseIterable {
    // Video
    case freeze                       // no decoded frame for > freezeMs on a stream that had been live
    case fpsDrop      = "fps_drop"    // decoded fps fell below fpsDropBelow while frames were flowing
    case decodeFail   = "decode_fail" // a video/audio decrypt/decode failure
    case gap                          // a video seq gap (lost frame(s))
    // Audio
    case underrun                     // the player node ran dry mid-stream (audio "dropped frame")
    case audioGap     = "audio_gap"   // reserved: an audio-transport gap (no audio seq on the wire yet)
    case seqLoss      = "seq_loss"    // reserved: audio sequence loss
    case silence                      // RMS below floor for > silenceMinMs while streaming (dropout)
    case click                        // inter/intra-buffer discontinuity (click/pop)
    case clip                         // sustained full-scale samples (distortion)
}

/// One ring entry. `tWall` is unix seconds (for correlating with logs); `tMonoMs` is monotonic ms
/// (ordering + inter-event deltas). `stream` is the label of the stream it happened on (nil = global).
struct AnomalyEvent: Sendable, Equatable {
    var tWall: Double
    var tMonoMs: Double
    var kind: AnomalyKind
    var stream: String?
    var detail: String
}

/// Raw per-buffer audio numbers computed ONCE by AudioPlayer (the only AVFoundation-coupled part) and
/// handed to the pure detector below. All amplitudes are normalized to full scale: int16 by /32767,
/// float PCM used as-is. Channels are NOT separated for energy/clip (the interleaved stream is analyzed
/// as one — sufficient for silence/clip); discontinuity uses channel 0 with a per-frame stride so a
/// stereo L/R interleave is never mistaken for a click.
struct AudioBufferDSP: Sendable, Equatable {
    var frames: Int            // frames analyzed (per channel)
    var channels: Int
    var sampleRate: Double
    var rms: Float             // 0…1
    var peak: Float            // 0…1
    var clipFraction: Float    // 0…1 — fraction of samples at/above clipLevel
    var firstSample: Float     // −1…1 (channel 0, first frame)
    var lastSample: Float      // −1…1 (channel 0, last frame)
    var maxAbsDelta: Float     // 0…2 — max |x[i] − x[i−1]| over channel 0
    var voice: Bool
    var durationMs: Double { sampleRate > 0 ? Double(frames) / sampleRate * 1000.0 : 0 }
}

/// Tunable DSP thresholds (named + commented so they can be dialed on hardware without hunting).
enum AudioDSP {
    /// RMS at/below this is "silence". ≈ −48 dBFS (0.004 × 32767 ≈ 131 counts) — comfortably below any
    /// real program material, above the dither/noise floor of a live decoded stream.
    static let silenceRMS: Float = 0.004
    /// A silence run must persist this long before it counts as an audible dropout (per the brief).
    static let silenceMinMs: Double = 120
    /// A sample whose |amplitude| reaches this fraction of full scale is treated as clipped.
    static let clipLevel: Float = 0.985
    /// A buffer with at least this fraction of its samples clipped is a sustained-distortion event.
    static let clipMinFraction: Float = 0.02
    /// Adjacent-sample amplitude jump (full-scale fraction) that reads as a click/pop.
    static let clickDelta: Float = 0.6
    /// Ignore click-sized jumps when the buffer is near-silent — those are onset transients, not clicks.
    static let clickMinRMS: Float = 0.01
    /// A gap between decoded audio buffers longer than this means the stream went IDLE (nothing playing,
    /// or a track boundary) — the silence run is reset on resume so a cross-idle gap never reads as a
    /// mid-playback dropout. Only the flow of buffers (an active stream) can trip a silence event.
    static let idleGapMs: Double = 200
}

/// Whether a stream is delivering motion/program content (ACTIVE) or iOS is deliberately conserving
/// (IDLE — a static homescreen/map throttles video to a few small frames/sec; a paused/absent track
/// sends no audio). IDLE is a FIRST-CLASS, NON-ANOMALOUS state — anomalies are judged relative to a
/// stream's OWN recent baseline, never a fixed absolute rate.
enum ActivityState: String, Sendable { case active, idle }

/// Video anomaly thresholds — all RELATIVE to the stream's own rolling baseline (tunable constants).
enum VideoActivity {
    /// EMA smoothing for the rolling inter-frame interval baseline.
    static let emaAlpha: Double = 0.3
    /// Instantaneous fps at/above this (or a large frame) pushes toward ACTIVE.
    static let activeFpsEnter: Double = 12
    /// Steady fps at/below this WITH small frames pulls toward IDLE (hysteresis gap vs enter).
    static let idleFpsExit: Double = 8
    /// Consecutive agreeing frames required before the state flips — stops flapping at the boundary.
    static let hysteresisFrames = 4
    /// A frame smaller than this is an idle/static-content hint (iOS shrinks frames when nothing moves).
    static let smallFrameBytes = 8000
    /// FREEZE trips only when the gap since the last frame exceeds K × the stream's baseline interval…
    static let freezeK: Double = 4.0
    /// …but never below this absolute floor (so a fast baseline can't make trivial jitter a "freeze").
    static let freezeFloorMs: Double = 350
    /// An ACTIVE stream with no frame for at least this long (checked by the monitor's poll) is a stall.
    static let activeStallMs: Double = 250
    /// fps_drop is meaningful only as a drop FROM an active rate: smoothed fps below this WHILE ACTIVE
    /// (motion-sized frames still arriving) is a stutter — a low fps during IDLE is normal, not a drop.
    static let fpsDropBelow: Double = 15
}

/// Kept for JSON key stability (the monitor reads `VideoActivity.fpsDropBelow`).
enum VideoAnomaly { static let fpsDropBelow = VideoActivity.fpsDropBelow }

/// Per-video-stream activity + freeze/fps-drop detector. Pure state machine; lives inside AVAnomalyLog.
struct VideoActivityDetector: Sendable {
    var haveLast = false
    var lastArrivalMs: Double = 0
    var emaIntervalMs: Double = 0
    var haveEma = false
    var state: ActivityState = .idle
    var activeStreak = 0
    var idleStreak = 0
    var stalled = false        // freeze already emitted for the CURRENT stall; re-armed when a frame lands
    var lastFrameBytes = 0
}

/// Fixed-size, thread-safe ring of anomaly events + the audio DSP detector state. IOKit/AppKit-free, so
/// the detection + debounce logic is unit-testable; AudioPlayer, OCBMAVDecrypt and the monitor all share
/// one instance per session and only feed it raw numbers. Internally Mutex-guarded (it is written from
/// the USB read queue, the audio engine queue, and read from the monitor's main-queue timer).
final class AVAnomalyLog: @unchecked Sendable {
    static let capacity = 48

    private struct DSPState {
        var prevLast: Float = 0
        var havePrev = false
        var silenceStartMs: Double? = nil
        var silenceEmitted = false
        var lastBufferMs: Double = 0
        var haveLastBuffer = false
    }
    private struct Key: Hashable { let kind: AnomalyKind; let stream: String? }
    private struct State {
        var ring: [AnomalyEvent] = []
        var total: UInt64 = 0                       // events ever appended (for "new since" deltas)
        var counts: [String: Int] = [:]             // per-kind cumulative counts (kind.rawValue → n)
        var lastEmitMs: [Key: Double] = [:]         // debounce: last emit per (kind, stream)
        var audio: [String: DSPState] = [:]         // per-audio-stream DSP detector state
        var video: [String: VideoActivityDetector] = [:] // per-video-stream activity detector
    }
    private let s = Mutex(State())

    /// Per-kind minimum spacing (ms) between emitted events — debounce so a sustained condition emits
    /// once on first trip and re-arms after it clears (silence/clip also gate on their own run state).
    static func cooldownMs(_ k: AnomalyKind) -> Double {
        switch k {
        case .freeze:     return 1500
        case .fpsDrop:    return 2000
        case .decodeFail: return 500
        case .gap:        return 300
        case .underrun:   return 500
        case .audioGap:   return 500
        case .seqLoss:    return 500
        case .silence:    return 1000
        case .click:      return 150
        case .clip:       return 600
        }
    }

    /// New session — drop all events + detector state.
    func reset() { s.withLock { $0 = State() } }

    /// Emit one event unless an identical (kind, stream) fired within its cooldown. Returns whether it
    /// was appended. Safe to call freely from a hot path — the debounce keeps the ring meaningful.
    @discardableResult
    func record(_ kind: AnomalyKind, stream: StreamKind?, detail: String,
                tMonoMs: Double, tWall: Double) -> Bool {
        s.withLock { st in
            let key = Key(kind: kind, stream: stream?.label)
            if let last = st.lastEmitMs[key], tMonoMs - last < Self.cooldownMs(kind) { return false }
            st.lastEmitMs[key] = tMonoMs
            st.ring.append(AnomalyEvent(tWall: tWall, tMonoMs: tMonoMs, kind: kind,
                                        stream: stream?.label, detail: detail))
            if st.ring.count > Self.capacity { st.ring.removeFirst(st.ring.count - Self.capacity) }
            st.total &+= 1
            st.counts[kind.rawValue, default: 0] += 1
            return true
        }
    }

    /// Pure per-buffer audio anomaly detection (silence / click / clip) with per-stream run state.
    /// `active` gates silence to a live session. Called from AudioPlayer's decoded-PCM tap.
    func recordAudioBuffer(_ stream: StreamKind, _ dsp: AudioBufferDSP,
                           active: Bool, tMonoMs: Double, tWall: Double) {
        // Read + update the per-stream detector state under the lock, deciding which events to emit;
        // emit via `record` (its own lock section) after — record() re-locks, which is fine (no nesting).
        enum Fire { case silence(Double); case click(String); case clip(String) }
        var fires: [Fire] = []
        s.withLock { st in
            var d = st.audio[stream.label] ?? DSPState()
            // Cross-idle reset: if the stream sat idle (no buffers) longer than idleGapMs, this buffer
            // is a fresh active start — a gap while nothing was playing must not read as a dropout.
            if d.haveLastBuffer && tMonoMs - d.lastBufferMs > AudioDSP.idleGapMs {
                d.silenceStartMs = nil
                d.silenceEmitted = false
            }
            // CLIP — sustained full-scale content.
            if dsp.clipFraction >= AudioDSP.clipMinFraction {
                fires.append(.clip("\(Int(dsp.clipFraction * 100))% samples @ full-scale"))
            }
            // CLICK — a discontinuity within the buffer OR across the buffer boundary, but not while
            // near-silent (onset transients aren't clicks). Scaled by requiring real signal (rms floor).
            let interJump = d.havePrev ? abs(dsp.firstSample - d.prevLast) : 0
            let jump = max(dsp.maxAbsDelta, interJump)
            if jump >= AudioDSP.clickDelta && dsp.rms >= AudioDSP.clickMinRMS {
                fires.append(.click(String(format: "Δ%.2f fs", jump)))
            }
            // SILENCE — RMS floor sustained for silenceMinMs while the session is streaming.
            if active && dsp.rms < AudioDSP.silenceRMS {
                let start = d.silenceStartMs ?? tMonoMs
                d.silenceStartMs = start
                let dur = tMonoMs - start + dsp.durationMs   // include this buffer's own span
                if dur >= AudioDSP.silenceMinMs && !d.silenceEmitted {
                    d.silenceEmitted = true
                    fires.append(.silence(dur))
                }
            } else {
                d.silenceStartMs = nil
                d.silenceEmitted = false
            }
            d.prevLast = dsp.lastSample
            d.havePrev = true
            d.lastBufferMs = tMonoMs
            d.haveLastBuffer = true
            st.audio[stream.label] = d
        }
        for f in fires {
            switch f {
            case .silence(let ms): record(.silence, stream: stream, detail: "\(Int(ms)) ms", tMonoMs: tMonoMs, tWall: tWall)
            case .click(let d):    record(.click, stream: stream, detail: d, tMonoMs: tMonoMs, tWall: tWall)
            case .clip(let d):     record(.clip, stream: stream, detail: d, tMonoMs: tMonoMs, tWall: tWall)
            }
        }
    }

    /// Per decoded video frame (from OCBMAVDecrypt). Maintains the stream's rolling baseline + ACTIVE/
    /// IDLE state (hysteresis) and fires FREEZE only when the just-observed gap is a large OUTLIER vs the
    /// stream's OWN baseline while it was ACTIVE — so normal idle throttling never trips. Also detects an
    /// fps_drop (a low smoothed fps while still delivering motion-sized frames).
    func recordVideoFrame(_ stream: StreamKind, bytes: Int, tMonoMs: Double, tWall: Double) {
        enum Fire { case freeze(Double); case fpsDrop(Double) }
        var fires: [Fire] = []
        s.withLock { st in
            var v = st.video[stream.label] ?? VideoActivityDetector()
            if v.haveLast {
                let interval = tMonoMs - v.lastArrivalMs
                let baseline = v.haveEma ? v.emaIntervalMs : interval
                let instantFps = interval > 0 ? 1000.0 / interval : 999
                let freezeThresh = max(VideoActivity.freezeFloorMs, VideoActivity.freezeK * baseline)
                // FREEZE-on-resume: only while ACTIVE, only a genuine outlier gap. Flip to idle (stalled).
                if v.state == .active && !v.stalled && interval > freezeThresh {
                    fires.append(.freeze(interval))
                    v.state = .idle
                }
                // Update the baseline EMA — but never with an outlier gap (would poison the baseline).
                if interval <= freezeThresh {
                    v.emaIntervalMs = v.haveEma
                        ? VideoActivity.emaAlpha * interval + (1 - VideoActivity.emaAlpha) * v.emaIntervalMs
                        : interval
                    v.haveEma = true
                }
                // State classification with hysteresis (motion = fast OR large frames; calm = slow AND small).
                let motion = instantFps >= VideoActivity.activeFpsEnter || bytes >= VideoActivity.smallFrameBytes
                let calm = instantFps <= VideoActivity.idleFpsExit && bytes < VideoActivity.smallFrameBytes
                if v.state == .idle {
                    if motion { v.activeStreak += 1; v.idleStreak = 0
                        if v.activeStreak >= VideoActivity.hysteresisFrames { v.state = .active }
                    } else { v.activeStreak = 0 }
                } else {
                    if calm { v.idleStreak += 1; v.activeStreak = 0
                        if v.idleStreak >= VideoActivity.hysteresisFrames { v.state = .idle }
                    } else { v.idleStreak = 0 }
                    // fps_drop: a drop FROM an active rate — motion-sized frames arriving too slowly.
                    if v.state == .active, v.haveEma {
                        let smoothedFps = v.emaIntervalMs > 0 ? 1000.0 / v.emaIntervalMs : 999
                        if smoothedFps < VideoActivity.fpsDropBelow && bytes >= VideoActivity.smallFrameBytes {
                            fires.append(.fpsDrop(smoothedFps))
                        }
                    }
                }
            }
            v.stalled = false          // a frame landed — re-arm the freeze detector
            v.lastArrivalMs = tMonoMs
            v.haveLast = true
            v.lastFrameBytes = bytes
            st.video[stream.label] = v
        }
        for f in fires {
            switch f {
            case .freeze(let ms):  record(.freeze, stream: stream, detail: "gap \(Int(ms)) ms (was active)", tMonoMs: tMonoMs, tWall: tWall)
            case .fpsDrop(let fps): record(.fpsDrop, stream: stream, detail: String(format: "%.0f fps (active)", fps), tMonoMs: tMonoMs, tWall: tWall)
            }
        }
    }

    /// Poll from the monitor's ~1 Hz timer: catches an ACTIVE stream that STOPPED (no frame arrives to
    /// trigger the per-frame path). Fires freeze once, marks it stalled + idle. Never fires while idle.
    func pollVideo(tMonoMs: Double, tWall: Double) {
        var fires: [(StreamKind, Double)] = []
        s.withLock { st in
            for stream in [StreamKind.mainVideo, .altVideo] {
                guard var v = st.video[stream.label], v.haveLast, v.state == .active, !v.stalled else { continue }
                let gap = tMonoMs - v.lastArrivalMs
                let baseline = v.haveEma ? v.emaIntervalMs : gap
                let thresh = max(VideoActivity.freezeFloorMs, VideoActivity.activeStallMs, VideoActivity.freezeK * baseline)
                if gap > thresh {
                    v.stalled = true
                    v.state = .idle
                    st.video[stream.label] = v
                    fires.append((stream, gap))
                }
            }
        }
        for (stream, gap) in fires {
            record(.freeze, stream: stream, detail: "no frame \(Int(gap)) ms (active stall)", tMonoMs: tMonoMs, tWall: tWall)
        }
    }

    /// The stream's current ACTIVE/IDLE label (idle when unseen — no frames yet is normal, not a fault).
    func videoState(_ stream: StreamKind) -> ActivityState {
        s.withLock { st in
            guard let v = st.video[stream.label], v.haveLast else { return .idle }
            return v.state
        }
    }

    /// Ring snapshot, oldest → newest.
    func events() -> [AnomalyEvent] { s.withLock { $0.ring } }
    /// Per-kind cumulative counts (kind.rawValue → count).
    func counts() -> [String: Int] { s.withLock { $0.counts } }
    /// For the log summary: how many events appended since `total` was last read, the newest event, and
    /// the current total to remember for next time.
    func delta(sinceTotal prior: UInt64) -> (new: Int, latest: AnomalyEvent?, total: UInt64) {
        s.withLock { st in (Int(st.total - prior), st.ring.last, st.total) }
    }

    /// Monotonic clock in ms (mach uptime) — one source used by every emitter so `tMonoMs` is comparable.
    static func monoMs() -> Double { Double(DispatchTime.now().uptimeNanoseconds) / 1_000_000.0 }
}
