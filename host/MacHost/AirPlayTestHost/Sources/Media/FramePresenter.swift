// FramePresenter.swift — between the decoder and the picture. By default a picture is shown the moment
// VideoToolbox hands it over (the behaviour the app has always had). With "Smooth playback" on, it is held
// until its own header timestamp, re-based onto the host clock, plus a fixed jitter buffer
// (`PlayoutSchedule`; 100 ms, the phone's `latencyMs` for type 110), so uneven arrival over Wi-Fi + USB is
// evened out at the cost of that much latency. A picture that is already past its slot is shown at once.
//
// Threading: `submit` is called on VideoToolbox's thread; held pictures are released in order by one
// timer on a private serial queue; `present` may be called from either. `cancelPending` (a stream ended,
// a new key) and switching the mode drop what is held, so nothing from an old stream can appear after the
// picture was cleared and nothing is shown out of order.

import CoreVideo
import Foundation
import Synchronization

final class FramePresenter: @unchecked Sendable {
    struct Snapshot: Equatable, Sendable {
        var smooth = false
        var bufferMs = 100
        var presented: UInt64 = 0
        /// Held for their slot (smooth mode).
        var held: UInt64 = 0
        /// Past their slot when ready: shown at once (smooth mode).
        var late: UInt64 = 0
        /// Held, then dropped: the stream ended or the mode was switched before their slot.
        var dropped: UInt64 = 0
        var rebases: UInt64 = 0
    }

    private struct Held {
        var buffer: CVPixelBuffer
        var due: Double
    }

    private struct State: @unchecked Sendable {
        var smooth = false
        var schedule: PlayoutSchedule
        var fifo: [Held] = []
        var lastDue = 0.0
        var snap = Snapshot()
    }

    private let present: @Sendable (CVPixelBuffer) -> Void
    private let queue = DispatchQueue(label: "airplay-test.presenter", qos: .userInteractive)
    private let state: Mutex<State>
    /// Queue-confined.
    private var timer: DispatchSourceTimer?

    init(buffer: Double = 0.1, present: @escaping @Sendable (CVPixelBuffer) -> Void) {
        self.present = present
        var s = State(schedule: PlayoutSchedule(buffer: buffer))
        s.snap.bufferMs = Int((buffer * 1000).rounded())
        state = Mutex(s)
    }

    static var now: Double { Double(DispatchTime.now().uptimeNanoseconds) / 1e9 }

    var snapshot: Snapshot {
        state.withLock { s in
            var snap = s.snap
            snap.rebases = s.schedule.stats.rebases
            return snap
        }
    }

    /// Any thread. Switching drops whatever is held (so no picture is shown twice or out of order).
    var smooth: Bool {
        get { state.withLock { $0.smooth } }
        set {
            state.withLock { s in
                guard s.smooth != newValue else { return }
                s.smooth = newValue
                s.snap.smooth = newValue
                Self.drop(&s)
            }
        }
    }

    /// Any thread: the picture is no longer wanted (stream stopped, new key). Held pictures are dropped and
    /// the clock base forgotten.
    func cancelPending() {
        state.withLock { Self.drop(&$0) }
    }

    private static func drop(_ s: inout State) {
        s.snap.dropped += UInt64(s.fifo.count)
        s.fifo = []
        s.lastDue = 0
        s.schedule.reset()
    }

    /// VideoToolbox's thread: a decoded picture with its arrival and stream times.
    func submit(_ buffer: CVPixelBuffer, time: MirrorVideoDecoder.FrameTime) {
        nonisolated(unsafe) let b = buffer
        let now = Self.now
        let showNow = state.withLock { s -> Bool in
            guard s.smooth else {
                s.snap.presented += 1
                return true
            }
            var due = s.schedule.due(stream: time.stream, arrival: time.arrival, now: now)
            s.snap.late = s.schedule.stats.late
            // Never ahead of a picture already held: order is kept even across a re-base.
            due = max(due, s.lastDue)
            if due <= now && s.fifo.isEmpty {
                s.snap.presented += 1
                return true
            }
            s.lastDue = due
            s.fifo.append(Held(buffer: b, due: due))
            s.snap.held += 1
            return false
        }
        if showNow {
            present(b)
        } else {
            queue.async { self.drain() }
        }
    }

    /// Queue: show every held picture whose slot has come, then sleep until the next one.
    private func drain() {
        let now = Self.now
        // `present` runs under the lock: a `cancelPending` (followed by the surface being cleared) can
        // then never fall between taking a picture off the queue and showing it.
        let next = state.withLock { s -> Double? in
            var newest: CVPixelBuffer?
            while let h = s.fifo.first, h.due <= now + 0.001 {
                newest = h.buffer
                s.fifo.removeFirst()
                s.snap.presented += 1
            }
            // Several due at once (the queue ran late): the surface keeps only the newest anyway.
            if let newest { present(newest) }
            return s.fifo.first?.due
        }
        guard let next else { return }
        if timer == nil {
            let t = DispatchSource.makeTimerSource(flags: .strict, queue: queue)
            t.setEventHandler { [weak self] in self?.drain() }
            t.resume()
            timer = t
        }
        timer?.schedule(deadline: DispatchTime(uptimeNanoseconds: UInt64(max(0, next) * 1e9)), leeway: .milliseconds(1))
    }

    deinit {
        timer?.cancel()
    }
}
