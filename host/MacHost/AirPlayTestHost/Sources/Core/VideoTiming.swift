// VideoTiming.swift — the screen stream's rates and arrival jitter, and the playout schedule of the
// optional "Smooth playback". Pure: times are plain seconds handed in by the caller (host uptime for
// arrivals and decodes, the 32.32 header timestamp for the phone's capture clock), so the tests drive
// them with a scripted clock.
//
// Why a window and not the poll interval: iOS encodes on screen change (docs/ocbm/02_AIRPLAY_ONLY_MODE.md
// §2, `fvdp_statsReport`: 0..31 frames a second in one session), so a rate taken over the 0.5 s between
// two status polls reads 0 whenever the screen holds still for half a second. The readout here is a
// 2 s moving window, plus the run's average, plus the rate the phone's own timestamps say it sent, plus
// how long ago the last picture came, so a 0 can be told apart from a stall.

import Foundation

struct VideoTimingMeter: Sendable {
    static let window = 2.0
    /// RFC 3550 §6.4.1 smoothing: J += (|D| - J) / 16.
    static let jitterGain = 1.0 / 16.0

    struct Readout: Equatable, Sendable {
        /// Decoded pictures per second over the last `window` seconds (0 when none came in it).
        var fps = 0.0
        /// Decoded pictures per second from the first to the last picture of this stream.
        var averageFPS = 0.0
        /// Frames per second by the phone's header timestamps, over the arrivals of the last window.
        var streamFPS = 0.0
        /// Smoothed |arrival interval - timestamp interval| (RFC 3550 interarrival jitter), in ms.
        var jitterMs = 0.0
        /// The largest |arrival interval - timestamp interval| in the last window, in ms.
        var jitterPeakMs = 0.0
        /// Seconds since the last decoded picture; nil before the first.
        var sinceLastFrame: Double?
        var decoded: UInt64 = 0
        var arrivals: UInt64 = 0
    }

    private var decodeTimes: [Double] = []
    private var firstDecode: Double?
    private var lastDecode: Double?
    private var decoded: UInt64 = 0
    /// (host arrival, stream timestamp, |D|) of the arrivals in the window.
    private var arrivalsInWindow: [(host: Double, stream: Double, d: Double)] = []
    private var lastArrival: (host: Double, stream: Double)?
    private var arrivals: UInt64 = 0
    private var jitter = 0.0

    /// A new stream: its rates and jitter start over.
    mutating func reset() { self = VideoTimingMeter() }

    /// A decrypted frame reached the decoder at host time `host`, stamped `stream` by the phone.
    mutating func arrival(host: Double, stream: Double) {
        arrivals += 1
        var d = 0.0
        if let last = lastArrival {
            d = abs((host - last.host) - (stream - last.stream))
            jitter += (d - jitter) * Self.jitterGain
        }
        lastArrival = (host, stream)
        arrivalsInWindow.append((host, stream, d))
        trim(now: host)
    }

    /// VideoToolbox produced a picture at host time `host`.
    mutating func decoded(host: Double) {
        decoded += 1
        if firstDecode == nil { firstDecode = host }
        lastDecode = host
        decodeTimes.append(host)
        trim(now: host)
    }

    private mutating func trim(now: Double) {
        let from = now - Self.window
        if let i = decodeTimes.firstIndex(where: { $0 > from }) {
            if i > 0 { decodeTimes.removeFirst(i) }
        } else {
            decodeTimes.removeAll()
        }
        if let i = arrivalsInWindow.firstIndex(where: { $0.host > from }) {
            if i > 0 { arrivalsInWindow.removeFirst(i) }
        } else {
            arrivalsInWindow.removeAll()
        }
    }

    func readout(now: Double) -> Readout {
        var r = Readout(decoded: decoded, arrivals: arrivals)
        r.jitterMs = jitter * 1000
        let from = now - Self.window
        let recent = decodeTimes.filter { $0 > from && $0 <= now }
        if let first = firstDecode, let last = lastDecode {
            r.sinceLastFrame = max(0, now - last)
            if first <= from {
                r.fps = Double(recent.count) / Self.window // a full window: count over its length
            } else if recent.count >= 2, let a = recent.first, let b = recent.last, b > a {
                r.fps = Double(recent.count - 1) / (b - a) // the stream is younger than the window
            }
            if decoded >= 2, last > first { r.averageFPS = Double(decoded - 1) / (last - first) }
        }
        let arr = arrivalsInWindow.filter { $0.host > from && $0.host <= now }
        if arr.count >= 2, let a = arr.first, let b = arr.last, b.stream > a.stream {
            r.streamFPS = Double(arr.count - 1) / (b.stream - a.stream)
        }
        // The first arrival of the window has no predecessor inside it; its D is still a real interval.
        r.jitterPeakMs = (arr.map(\.d).max() ?? 0) * 1000
        return r
    }
}

/// When to show a decoded picture under "Smooth playback": at its own timestamp, re-based onto the host
/// clock, plus a fixed jitter buffer (100 ms by default, the phone's `latencyMs` in the type 110 SETUP).
///
/// The offset between the two clocks is the SMALLEST (arrival - timestamp) seen over the last few
/// seconds: the fastest a frame has made it across. A frame delayed by up to `buffer` beyond that is
/// still shown on its slot; one delayed more is shown at once and counted late. A timestamp that jumps
/// (backwards, or more than `maxStep` forwards) starts a new base, so a new stream or a phone clock step
/// cannot hold pictures back for seconds.
struct PlayoutSchedule: Sendable {
    var buffer: Double
    /// How long an observed minimum offset is remembered.
    static let baseWindow = 4.0
    static let maxStep = 2.0

    struct Stats: Equatable, Sendable {
        var scheduled: UInt64 = 0
        /// Shown at once: it arrived (or finished decoding) after its slot.
        var late: UInt64 = 0
        var rebases: UInt64 = 0
    }

    private var offsets: [(host: Double, offset: Double)] = []
    private var lastStream: Double?
    private(set) var stats = Stats()

    init(buffer: Double = 0.1) { self.buffer = buffer }

    mutating func reset() {
        offsets = []
        lastStream = nil
    }

    /// The host time at which the picture stamped `stream`, which arrived at `arrival`, is due; `now` is
    /// when the picture is ready. A due time before `now` means "show it now" (and is counted late).
    mutating func due(stream: Double, arrival: Double, now: Double) -> Double {
        if let last = lastStream, stream < last || stream - last > Self.maxStep {
            offsets = []
            stats.rebases += 1
        }
        lastStream = stream
        let offset = arrival - stream
        offsets.removeAll { $0.host < arrival - Self.baseWindow || $0.offset >= offset }
        offsets.append((arrival, offset)) // ascending in offset: the first is the minimum
        let base = offsets[0].offset
        let at = stream + base + buffer
        stats.scheduled += 1
        if at < now { stats.late += 1 }
        return at
    }
}
