import Foundation
import AVFoundation
import CoreMedia
import VideoToolbox
import Synchronization
import os

/// The video codec a CarPlay session negotiates (user directive 2026-07-11: support BOTH).
/// Wired stock sessions are H.264; HEVC arrives once the box publishes `hevcInfo` + accepts `hevc`
/// at SETUP (flag-gated box A/B — docs/carplay/03_SDK_GROUND_TRUTH.md §5). The decode path here is codec-complete either way.
enum VideoCodec: String {
    case h264 = "H.264"
    case hevc = "HEVC"
}

/// Decodes H.264 **and HEVC** length-prefixed (AVCC) NAL streams and displays them via
/// AVSampleBufferDisplayLayer. Handles parameter-set extraction (SPS/PPS; +VPS for HEVC), the
/// zero-copy CMBlockBuffer wrap, and CMSampleBuffer creation.
///
/// V1 FAST PATH (2026-08): the OCBM plaintext is already AVCC with 4-byte NAL length prefixes — the
/// exact CMBlockBuffer payload VideoToolbox needs. `decodeAndDisplay(avcc:keyframe:)` validates the
/// length prefixes in one cheap pass (no copy), then wraps the `Data` zero-copy. In-band parameter-set
/// frames (mid-stream format change) take the classify/diff path; steady-state frames copy nothing.
///
/// PRE-WARM: `configure(codec:parameterSets:)` builds the CMVideoFormatDescription the moment the
/// box forwards the session's codec config (avcC/hvcC) — BEFORE the first IDR — AND retains the
/// parameter sets (`configuredParameterSets`) so the flush-recovery path can re-seed the format
/// without an in-band SPS/PPS prepend (the AVCC fast path carries none).
///
/// Threading: `decodeAndDisplay` may be called from any thread (the USB read queue in practice); all
/// parsing/conversion runs on a private serial queue, which both guarantees frame ordering and keeps
/// per-frame work off the main thread. The final renderer hand-off runs on its OWN serial queue —
/// see `renderQueue`; NOTHING on the frame path touches the main thread any more.
///
/// V6 (2026-09-03, measured): the renderer hand-off left the main thread. The V5 FIFOs cut the drops to
/// a start-of-session burst — 30 `evict-oldest-P`, ALL on the enqueue queue, in the first 25 s and then
/// none for 5 minutes — because the main thread is busy with window/appearance/format-ready work exactly
/// while the first frames arrive. Deepening the queue would only have delayed those frames; the fix is
/// that the consumer is no longer the main thread. See `renderQueue` for why that is API-legal.
///
/// V5 (2026-09-03): both hops are bounded FIFOs (`AVCCFastPath.FrameFIFO`, depth 3 by default) instead
/// of the V4 depth-1 latest-wins slots. Neither hop ever blocks its producer — a full queue always
/// resolves to a drop, so a stalled consumer can still never back-pressure the USB read path. What
/// changed is WHICH frame is lost: an IDR (and the frame immediately after it) is never discarded, and
/// the oldest unprotected P goes first, so a two-frame consumer hiccup costs latency instead of
/// punching a hole in the reference chain (the measured cause of the 2026-09-03 stutter: 13 enqueue-slot
/// drops in the first 12 s, each followed by a keyframe request and ~1–2 s of corrupted video).
final class VideoDecoder: @unchecked Sendable {

    private static let logger = Logger(subsystem: "com.carlink.video", category: "decoder")

    let displayLayer = AVSampleBufferDisplayLayer()

    /// Lane label for logs ("main" / "alt"), set by the owner. Lets the nav-freeze instrumentation
    /// below distinguish the cluster lane from the main screen.
    var label = "video"
    /// Edge-trigger for the renderer-wedged log. renderQueue-confined (drainEnqueue only) — it was
    /// main-thread-confined until V6 moved the hand-off off main; the confinement discipline is the same
    /// one, re-homed on the queue that now owns the receiver.
    private var rendererWedgedLogged = false

    /// Invoked (on the decode queue) when the renderer requires a fresh IDR
    /// to resume after a flush. The owner should request a keyframe from the
    /// adapter — without it playback stays frozen until one happens to arrive.
    var onNeedsKeyFrame: (@Sendable () -> Void)?

    /// Invoked when a frame is dropped under backpressure (V4) or a P-frame decode chain is broken.
    /// The owner requests a keyframe (throttled ≤1/500 ms) so the decoder re-syncs on the next IDR.
    var onFrameDropped: (@Sendable () -> Void)?

    // macOS 26+ rendering model: the deprecated enqueue/flush/status on the renderer are
    // replaced by a render-synchronizer + Receiver. We attach the display layer's video
    // renderer to a synchronizer once and enqueue every frame through the receiver.
    private let synchronizer = AVSampleBufferRenderSynchronizer()
    private let receiver: AVSampleBufferVideoRenderer.Receiver

    // All of the state below is confined to decodeQueue.
    private var codec: VideoCodec = .h264
    private var currentVPS: Data? // HEVC only
    private var currentSPS: Data?
    private var currentPPS: Data?
    private var formatDescription: CMVideoFormatDescription?
    private var frameCount: Int64 = 0

    /// Cross-thread frame accounting (perf 2026-08-09), readable from the metrics monitor on any queue.
    /// `decodedCount` = sample buffers actually built (AD, bumped with frameCount); `slotDrops` = frames
    /// shed by the decode/enqueue FIFOs (queue-full evictions + rejected newcomers). AR (decrypted, in
    /// the decrypt metrics) − AD = decode-queue loss — together they localize whether the cluster lane
    /// starves upstream of the app or drops in-app. Atomics (not the decodeQueue-confined frameCount) so
    /// the ~1 Hz monitor can read them off the main queue without a data race.
    let decodedCount = Atomic<UInt64>(0)
    let slotDrops = Atomic<UInt64>(0)

    // MARK: - Pipeline-latency measurement (V5; RENAMED HONESTLY in V6, 2026-09-03)
    //
    // The FIFOs trade frame loss for QUEUEING DELAY, so the delay has to be observable or the trade is
    // unmeasurable (`AVmon` used to print a hard-coded `declat=n/a`). Two EWMAs per lane, both anchored
    // on the frame's ARRIVAL (the instant `decodeAndDisplay` was called on the USB read queue):
    //   • `wrapLatencyMs`    = arrival → CMSampleBuffer built (decode-FIFO wait + length-walk + wrap)
    //   • `handoffLatencyMs` = arrival → handed to the renderer (adds the enqueue-FIFO wait)
    //
    // THESE ARE NOT DECODE TIMES, and V5 was wrong to call the first one `decodeLatencyMs` / `declat`:
    // it measures a zero-copy CMBlockBuffer wrap, comes out around 0.1 ms, and reads like a spectacular
    // decode time when it is in fact a parse. Real decode happens inside the renderer, AFTER the
    // hand-off, and this render path cannot observe it: `Receiver` exposes no VTDecompressionSession and
    // no per-frame completion. Its only feedback channels are `EnqueueResult` (synchronous accept/
    // reject) and `renderingEventsAfterFinishedEnqueuing`, whose RenderingEvent cases are
    // didFailToDecode / requiresFlushToResumeDecoding / failed — FAILURES only. So the names now say
    // what is actually measured; a true decode latency would need an explicit VTDecompressionSession.
    //
    // Stored as the IEEE bit pattern of a Double in a relaxed atomic. The read-modify-write is NOT a CAS
    // loop and does not need to be: each cell has exactly ONE writer queue (wrap cell ← decodeQueue,
    // handoff cell ← renderQueue), and the ~1 Hz monitor only ever reads. Bit pattern 0 (== +0.0) is the
    // "no sample yet" sentinel; a real EWMA is never exactly 0 ms.
    private let wrapLatencyBits = Atomic<UInt64>(0)
    private let handoffLatencyBits = Atomic<UInt64>(0)
    private static let ewmaAlpha = 0.125

    /// EWMA of arrival → CMSampleBuffer built, in ms. NOT a decode time (see above). nil until the
    /// first frame has been wrapped.
    var wrapLatencyMs: Double? {
        let b = wrapLatencyBits.load(ordering: .relaxed)
        return b == 0 ? nil : Double(bitPattern: b)
    }
    /// EWMA of arrival → handed to the renderer, in ms. This is the number the FIFO depth actually
    /// buys or costs. nil until the first frame has been handed off.
    var handoffLatencyMs: Double? {
        let b = handoffLatencyBits.load(ordering: .relaxed)
        return b == 0 ? nil : Double(bitPattern: b)
    }

    /// Fold one arrival-relative sample into an EWMA cell. Single-writer per cell (see above).
    private static func updateEWMA(_ cell: borrowing Atomic<UInt64>, sinceNs arrivalNs: UInt64) {
        let now = DispatchTime.now().uptimeNanoseconds
        // Monotonic clock, but a nonsensical (negative → wrapped) delta must never poison the average.
        guard now >= arrivalNs else { return }
        let ms = Double(now &- arrivalNs) / 1_000_000.0
        let prev = Double(bitPattern: cell.load(ordering: .relaxed))
        let next = prev == 0 ? ms : prev + (ms - prev) * ewmaAlpha
        cell.store(next.bitPattern, ordering: .relaxed)
    }

    /// The session's codec config, retained from `configure(codec:parameterSets:)` — H.264 [SPS, PPS]
    /// or HEVC [VPS, SPS, PPS]. This is the re-seed source for flush-recovery: the flush path nils
    /// `formatDescription` + `current{VPS,SPS,PPS}`, and (unlike the old Annex-B path) the AVCC fast
    /// path prepends no in-band SPS/PPS, so the format is rebuilt from THIS on the next keyframe.
    /// decodeQueue-confined; survives flush-recovery.
    private var configuredParameterSets: [Data] = []
    private var configuredCodec: VideoCodec = .h264

    /// Fired on the MAIN queue when the coded video dimensions are first established or change.
    /// Drives window auto-sizing — the alt/cluster window locks its aspect to these actual decoded
    /// dimensions (iOS picks the cluster resolution itself, which need not match what we requested).
    /// Set once at construction, before any frames flow.
    var onDimensions: ((_ width: Int, _ height: Int) -> Void)?
    /// Last dimensions handed to `onDimensions` (decodeQueue-confined) — dedups repeat rebuilds.
    private var lastReportedDims: (Int32, Int32)?
    /// Bumped whenever formatDescription is (re)built or cleared. The
    /// flush-recovery path clears decode state only if the generation still
    /// matches the failed sample's — otherwise several stale frames queued
    /// on the main thread would wipe a format a newer frame just rebuilt
    /// (self-inflicted second freeze) and spam duplicate keyframe requests.
    private var formatGeneration: UInt64 = 0

    private let decodeQueue = DispatchQueue(label: "video.decode", qos: .userInitiated)

    /// The single queue that owns `receiver` after construction (V6, 2026-09-03).
    ///
    /// WHY THIS IS CORRECT, not merely convenient. `AVSampleBufferVideoRenderer.Receiver` is declared in
    /// the macOS 27 AVFoundation swiftinterface as a bare `public class Receiver` — it is NOT `Sendable`
    /// and it is NOT `@MainActor`. What it IS: `sampleBufferReceiver(adding:)` returns it as `sending`
    /// and `removeReceiver(_:at:)` takes it as `sending`, i.e. Apple models it as a single-owner object
    /// that is TRANSFERRED into one isolation domain. Any single serial queue is legal; two are not.
    /// Apple's own usage example (AVSampleBufferRenderSynchronizer.h, "Example use") enqueues from a
    /// client-chosen `requestMediaDataWhenReadyOnQueue:` queue, not from main.
    ///
    /// THE RULE THIS IMPOSES: `receiver` is constructed on the main actor in `init` (the transfer point,
    /// before any frame can flow) and from then on is touched ONLY here — `enqueueImmediately` and both
    /// `flush()` call sites. Adding a fourth touch from another queue reintroduces exactly the data race
    /// the `sending` annotations exist to prevent. `synchronizer` and `displayLayer` stay main-confined
    /// and are never read after init.
    ///
    /// `.userInteractive` because this is the frame-presentation deadline path and it does exactly one
    /// bounded hand-off per frame; the shared Mutex donates priority, so it cannot invert against
    /// decodeQueue.
    private let renderQueue = DispatchQueue(label: "video.render", qos: .userInteractive)

    // MARK: - V5 bounded-FIFO handoffs

    /// A frame awaiting decode (USB read queue → decodeQueue). `arrivalNs` is the uptime the frame was
    /// submitted, and is the anchor for BOTH latency EWMAs.
    private struct PendingAVCC { let avcc: Data; let keyframe: Bool; let arrivalNs: UInt64 }
    /// A decoded sample buffer awaiting enqueue (decodeQueue → main). CMSampleBuffer is not Sendable;
    /// this box is a freshly-built, single-owner hand-off (only ever stored/taken under the Mutex), so
    /// @unchecked Sendable is sound and lets the value flow through the `sending` withLock body.
    private final class PendingSB: @unchecked Sendable {
        let sb: CMSampleBuffer; let generation: UInt64; let keyframe: Bool
        /// `AVCCFastPath.Walk.nalTypeMask` of the AU this sample buffer was built from — carried so the
        /// decode-failure log can name the NAL types of the frame VideoToolbox rejected.
        let nalMask: UInt64
        /// Arrival uptime of the source AU, carried through so the main thread can close the
        /// arrival → presented measurement.
        let arrivalNs: UInt64
        init(sb: CMSampleBuffer, generation: UInt64, keyframe: Bool, nalMask: UInt64, arrivalNs: UInt64) {
            self.sb = sb; self.generation = generation; self.keyframe = keyframe
            self.nalMask = nalMask; self.arrivalNs = arrivalNs
        }
    }

    /// Default depth for both hand-off FIFOs. 3 = the incoming frame plus a 2-frame consumer cushion:
    /// deep enough to ride out the main-thread/decodeQueue hiccups that were shedding frames every
    /// ~1 s on device, shallow enough that a genuinely wedged consumer still sheds (never buffers
    /// unboundedly) and that the added presentation delay stays inside one frame interval at 30–60 fps.
    static let defaultQueueDepth = 3

    /// Decode FIFO (USB read queue → decodeQueue). See `AVCCFastPath.FrameFIFO` for the drop policy.
    private let pendingDecode = Mutex(AVCCFastPath.FrameFIFO<PendingAVCC>(depth: VideoDecoder.defaultQueueDepth))
    /// Enqueue FIFO (decodeQueue → main). Holds built CMSampleBuffers, so it stays shallow.
    private let pendingEnqueue = Mutex(AVCCFastPath.FrameFIFO<PendingSB>(depth: VideoDecoder.defaultQueueDepth))

    /// Max frames the DECODE FIFO holds before the drop policy engages. Kept as a knob: AA raises it
    /// hard (64) so the opening IDR plus every P-frame that lands during VideoToolbox's one-time decode
    /// warm-up is queued rather than shed, and `1` reproduces the V4 single-slot latest-wins table
    /// exactly (the harness asserts that against `AVCCFastPath.resolveSlot`). Assign before frames flow.
    var maxDecodeDepth: Int {
        get { pendingDecode.withLock { $0.depth } }
        set { pendingDecode.withLock { $0.depth = newValue } }
    }
    /// Max frames the ENQUEUE FIFO holds. Same knob for the decodeQueue → main hop.
    var maxEnqueueDepth: Int {
        get { pendingEnqueue.withLock { $0.depth } }
        set { pendingEnqueue.withLock { $0.depth = newValue } }
    }

    // MARK: - Throttled diagnostics (2026-09-03)
    //
    // WHY: the 2026-09-03 alt-lane investigation could not attribute a single
    // `Frame enqueued with decode failures` line to a lane, name the VideoToolbox status behind it, or
    // see the slot drops that punch the P-chain holes those failures report — the line carried no lane
    // tag, discarded `EnqueueResult.enqueuedWithDecodeFailures`'s `[Error]` payload, and `slotDrops` was
    // a counter that never reached the log. It also fired UNTHROTTLED at up to ~50/s from the MAIN
    // THREAD, i.e. straight into the queue whose lateness causes the drops. Every log below is
    // lane-tagged, carries the evidence, and is rate-limited with a suppressed-since count.
    /// ns of the last emitted line, per diagnostic kind. Atomics because the producers sit on three
    /// different queues (submit thread / decodeQueue / main).
    private let lastDropLogNs = Atomic<UInt64>(0)
    private let lastFailLogNs = Atomic<UInt64>(0)
    private let lastNeedsKFLogNs = Atomic<UInt64>(0)
    private let suppressedFailures = Atomic<UInt64>(0)
    private static let logThrottleNs: UInt64 = 1_000_000_000

    /// True at most once per `logThrottleNs` per kind. Lock-free: the CAS loser stays silent.
    private static func throttlePasses(_ gate: borrowing Atomic<UInt64>) -> Bool {
        let now = DispatchTime.now().uptimeNanoseconds
        let last = gate.load(ordering: .relaxed)
        guard now &- last >= logThrottleNs else { return false }
        return gate.compareExchange(expected: last, desired: now,
                                    ordering: .relaxed).exchanged
    }

    /// Account for (and, throttled, log) one FIFO push. THIS is the event that breaks the P-frame chain
    /// and makes every later frame decode with errors until an IDR lands, so it is the first thing to
    /// look for when the `decode FAILURES` lines start. The line names WHICH RULE fired, so a live log
    /// distinguishes "the cushion overflowed and we shed a stale P" (expected, cheap) from
    /// "everything queued was protected and we had to refuse the newcomer" (the expensive case).
    ///
    /// Called on the PRODUCER's thread for each hop (USB read queue for `decode`, decodeQueue for
    /// `enqueue`) — same as V4, so `onFrameDropped` keeps its existing caller context and its
    /// client-side ≤1/500 ms request throttle.
    private func noteAdmission(_ a: AVCCFastPath.FrameAdmission, queue: String) {
        if a.outcome.shedAFrame {
            let total = slotDrops.wrappingAdd(1, ordering: .relaxed).newValue
            if Self.throttlePasses(lastDropLogNs) {
                let lost = a.droppedWasKeyframe.map { $0 ? "IDR" : "P" } ?? "-"
                let cost = a.requestKeyframe
                    ? "P-chain hole; every frame after it decodes with errors until the next IDR"
                    : "chain repaired by an IDR already in the queue"
                Self.logger.warning("""
                [\(self.label, privacy: .public)] \(queue, privacy: .public)-queue DROP \
                rule=\(a.outcome.rule, privacy: .public) lost=\(lost, privacy: .public) \
                total=\(total, privacy: .public) — \(cost, privacy: .public)
                """)
            }
        }
        if a.requestKeyframe { onFrameDropped?() }
    }

    // The display layer's renderer and videoGravity are main-actor-isolated in the macOS 26
    // SDK. Both call sites construct the decoder on the main actor, so isolate init here.
    @MainActor
    init() {
        receiver = synchronizer.sampleBufferReceiver(adding: displayLayer.sampleBufferRenderer)
        displayLayer.videoGravity = .resizeAspect
        // Run the timebase so enqueued frames present. We use enqueueImmediately for every
        // frame (real-time mirror: always show the newest frame), so absolute timestamps are
        // not used for scheduling, but a non-zero rate keeps the renderer active.
        synchronizer.setRate(1.0, time: .zero)
    }

    // MARK: - Configuration (pre-warm + flush-recovery re-seed source)

    /// PRE-WARM (user directive 2026-07-11): configure the decoder from the session's codec config
    /// the moment it arrives — before the first IDR. Builds the CMVideoFormatDescription eagerly and
    /// pins the codec, so the first keyframe goes straight to a ready renderer. Also RETAINS the sets
    /// as `configuredParameterSets` so flush-recovery can rebuild the format from them (V1). Raw NAL
    /// bytes: H.264 [SPS, PPS]; HEVC [VPS, SPS, PPS].
    func configure(codec: VideoCodec, parameterSets: [Data]) {
        decodeQueue.async { [weak self] in
            guard let self else { return }
            // DEDUP (item-3 root-cause fix): the box RESENDS the video-config periodically. Rebuilding
            // the format for byte-identical parameter sets mints a NEW CMVideoFormatDescription object,
            // which makes VideoToolbox RESTART its decompression session — the restarted session then
            // needs an IDR, but the next frame off the wire is a NotSync P-frame → enqueuedWithDecode-
            // Failures, and (with no recovery) the picture FREEZES. OLD avoided this by rebuilding from
            // the in-band IDR so every swap coincided with a keyframe. Skip the rebuild when nothing
            // changed and a format already exists; a genuine mid-stream param change still rebuilds.
            if codec == self.configuredCodec,
               parameterSets == self.configuredParameterSets,
               self.formatDescription != nil {
                return
            }
            self.codec = codec
            switch codec {
            case .h264 where parameterSets.count >= 2:
                self.currentSPS = parameterSets[0]
                self.currentPPS = parameterSets[1]
                self.currentVPS = nil
            case .hevc where parameterSets.count >= 3:
                self.currentVPS = parameterSets[0]
                self.currentSPS = parameterSets[1]
                self.currentPPS = parameterSets[2]
            default:
                Self.logger.error("configure(\(codec.rawValue, privacy: .public)): wrong parameter-set count \(parameterSets.count) — ignored")
                return
            }
            self.configuredCodec = codec
            self.configuredParameterSets = parameterSets
            self.rebuildFormat(reason: "config (pre-warmed before first IDR)")
        }
    }

    // MARK: - Main Entry Point (V1 AVCC fast path + V5 decode FIFO)

    /// Submit a length-prefixed (4-byte AVCC) access unit for decode+display. Safe to call from any
    /// thread, and NEVER blocks: a full decode FIFO resolves to a drop (per `FrameFIFO`'s policy), so a
    /// stalled decodeQueue can never stall the USB read path.
    func decodeAndDisplay(avcc: Data, keyframe: Bool) {
        let arrivalNs = DispatchTime.now().uptimeNanoseconds
        let admission = pendingDecode.withLock { fifo in
            fifo.push(PendingAVCC(avcc: avcc, keyframe: keyframe, arrivalNs: arrivalNs), keyframe: keyframe)
        }
        noteAdmission(admission, queue: "decode")
        // Empty → non-empty is the only transition that needs a fresh drain; every other push is
        // covered by the in-flight drain's tail re-dispatch below (no lost wakeup: a push that sees a
        // non-empty FIFO happens-before that drain's own under-lock `isEmpty` check, or before its pop).
        if admission.wasEmpty { decodeQueue.async { [weak self] in self?.drainDecode() } }
    }

    /// decodeQueue: take the OLDEST queued frame and process it. One item per drain; the drain
    /// re-dispatches itself while the FIFO still has work, so a burst never strands a frame.
    private func drainDecode() {
        guard let pending = pendingDecode.withLock({ $0.pop() }) else { return }
        performDecodeAVCC(avcc: pending.avcc, keyframe: pending.keyframe, arrivalNs: pending.arrivalNs)
        // decodeQueue is serial, so this chains cleanly and never runs two drains at once.
        let more = pendingDecode.withLock { !$0.isEmpty }
        if more { decodeQueue.async { [weak self] in self?.drainDecode() } }
    }

    /// On decodeQueue: (re)build formatDescription from the current parameter sets.
    private func rebuildFormat(reason: String) {
        let fmt: CMVideoFormatDescription?
        switch codec {
        case .h264:
            guard let sps = currentSPS, let pps = currentPPS else { return }
            fmt = createFormatDescription(sps: sps, pps: pps)
        case .hevc:
            guard let vps = currentVPS, let sps = currentSPS, let pps = currentPPS else { return }
            fmt = createHEVCFormatDescription(vps: vps, sps: sps, pps: pps)
        }
        formatDescription = fmt
        formatGeneration &+= 1
        if let fmt {
            // Log the actual coded resolution the iPhone streams, so the negotiated CarPlay video
            // size is visible without guessing from the window scaler.
            let dims = CMVideoFormatDescriptionGetDimensions(fmt)
            Self.logger.info("\(self.codec.rawValue, privacy: .public) format ready — \(dims.width, privacy: .public)×\(dims.height, privacy: .public) (\(reason, privacy: .public))")
            // Report the real coded size so the hosting window can lock its aspect to it. Deduped so a
            // format rebuild with unchanged dimensions doesn't churn the window.
            if lastReportedDims?.0 != dims.width || lastReportedDims?.1 != dims.height {
                lastReportedDims = (dims.width, dims.height)
                if let cb = onDimensions, dims.width > 0, dims.height > 0 {
                    DispatchQueue.main.async { cb(Int(dims.width), Int(dims.height)) }
                }
            }
        }
    }

    // MARK: - AVCC decode

    /// One cheap validating pass over the AVCC length prefixes, then either the classify/diff path
    /// (in-band parameter sets) or the zero-copy steady-state path.
    private func performDecodeAVCC(avcc: Data, keyframe: Bool, arrivalNs: UInt64) {
        let isHEVC = codec == .hevc
        let walk = avcc.withUnsafeBytes { AVCCFastPath.walkAVCC($0, isHEVC: isHEVC) }
        guard walk.valid else {
            Self.logger.error("AVCC frame failed length-walk validation (\(avcc.count, privacy: .public) B) — dropped")
            return
        }

        if walk.hasParamSets {
            // Mid-stream in-band SPS/PPS (format change): strip/diff/rebuild exactly like the legacy
            // path. One extra classification copy — on format-change frames only, zero in steady state.
            performDecodeAVCCWithParamSets(avcc: avcc, isHEVC: isHEVC, nalMask: walk.nalTypeMask,
                                          arrivalNs: arrivalNs)
            return
        }

        // Steady state: no in-band parameter sets in this AU.
        if formatDescription == nil {
            // Post-flush: flush-recovery nilled the format + parameter sets and RELIES on this re-seed
            // (the AVCC fast path prepends no in-band SPS/PPS to rebuild from). Only a keyframe can
            // re-seed usefully; a P-frame with no format is undecodable → drop + request an IDR.
            guard walk.containsIDR else {
                noteNeedsKeyFrame("no format (post-flush) — P-frame dropped, nal=\(AVCCFastPath.nalTypeSummary(walk.nalTypeMask))")
                onNeedsKeyFrame?()
                return
            }
            guard reseedFormatFromConfigured() else {
                noteNeedsKeyFrame("no format and no usable session parameter sets — IDR dropped")
                onNeedsKeyFrame?()
                return
            }
        }

        guard let fmt = formatDescription else { return }
        if let sb = createSampleBufferZeroCopy(avcc: avcc, containsIDR: walk.containsIDR, formatDescription: fmt) {
            enqueue(sb, generation: formatGeneration, keyframe: walk.containsIDR,
                    nalMask: walk.nalTypeMask, arrivalNs: arrivalNs)
        }
    }

    /// Throttled, lane-tagged record of WHY the decoder asked for an IDR. `onNeedsKeyFrame` itself stays
    /// unthrottled — the client-side ≤1/500 ms throttle owns the request rate and the recovery latency
    /// must not change; only the logging is rate-limited.
    private func noteNeedsKeyFrame(_ reason: String) {
        guard Self.throttlePasses(lastNeedsKFLogNs) else { return }
        Self.logger.warning("[\(self.label, privacy: .public)] requesting IDR — \(reason, privacy: .public)")
    }

    /// Rebuild the format from the retained session parameter sets (the flush-recovery re-seed).
    /// Returns false if no usable configured sets exist (config never arrived).
    private func reseedFormatFromConfigured() -> Bool {
        switch configuredCodec {
        case .h264 where configuredParameterSets.count >= 2:
            currentSPS = configuredParameterSets[0]
            currentPPS = configuredParameterSets[1]
            currentVPS = nil
        case .hevc where configuredParameterSets.count >= 3:
            currentVPS = configuredParameterSets[0]
            currentSPS = configuredParameterSets[1]
            currentPPS = configuredParameterSets[2]
        default:
            return false
        }
        codec = configuredCodec
        rebuildFormat(reason: "flush-recovery re-seed from session parameter sets")
        return formatDescription != nil
    }

    /// Format-change path: an AU carrying in-band parameter sets. Diff SPS/PPS(/VPS) against the
    /// current sets, rebuild the format if they changed, and copy the displayable NALs into a
    /// CM-allocated block (one copy — format-change frames only).
    private func performDecodeAVCCWithParamSets(avcc: Data, isHEVC: Bool, nalMask: UInt64,
                                               arrivalNs: UInt64) {
        var needsNewFormat = false
        var displayRanges: [Range<Int>] = []
        var avccLength = 0
        var containsIDR = false
        avcc.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
            let ranges = AVCCFastPath.nalPayloadRanges(raw)
            let r = classifyNALs(ranges: ranges, raw: raw, isHEVC: isHEVC)
            displayRanges = r.display
            avccLength = r.avccLength
            containsIDR = r.containsIDR
            needsNewFormat = r.needsNewFormat
        }

        if needsNewFormat { rebuildFormat(reason: "in-band parameter sets") }

        guard avccLength > 0, let fmt = formatDescription else { return }
        if let sb = createSampleBuffer(nalData: avcc,
                                       displayRanges: displayRanges,
                                       avccLength: avccLength,
                                       containsIDR: containsIDR,
                                       formatDescription: fmt) {
            enqueue(sb, generation: formatGeneration, keyframe: containsIDR,
                    nalMask: nalMask, arrivalNs: arrivalNs)
        }
    }

    /// Classify NAL payload ranges: diff parameter sets into `current{VPS,SPS,PPS}` (setting
    /// `needsNewFormat`) and collect the displayable slices (slices/SEI) with their 4-byte-prefixed
    /// output length. Shared by the in-band-parameter-set path. `raw` is the AVCC buffer; ranges are
    /// NAL PAYLOAD offsets (length prefixes already stripped).
    private func classifyNALs(ranges: [Range<Int>], raw: UnsafeRawBufferPointer, isHEVC: Bool)
        -> (display: [Range<Int>], avccLength: Int, containsIDR: Bool, needsNewFormat: Bool) {
        var needsNewFormat = false
        var displayRanges: [Range<Int>] = []
        var avccLength = 0
        var containsIDR = false
        for range in ranges {
            guard !range.isEmpty else { continue }
            if !isHEVC {
                let nalType = raw[range.lowerBound] & 0x1F
                switch nalType {
                case 7: // SPS
                    let sps = Data(bytes: raw.baseAddress! + range.lowerBound, count: range.count)
                    if sps != currentSPS { currentSPS = sps; needsNewFormat = true }
                case 8: // PPS
                    let pps = Data(bytes: raw.baseAddress! + range.lowerBound, count: range.count)
                    if pps != currentPPS { currentPPS = pps; needsNewFormat = true }
                case 1, 5, 6: // Non-IDR, IDR, SEI
                    if nalType == 5 { containsIDR = true }
                    displayRanges.append(range)
                    avccLength += 4 + range.count
                default:
                    break
                }
            } else {
                // HEVC: type = (byte0 >> 1) & 0x3F. VPS 32 / SPS 33 / PPS 34; slices 0–9 (trailing)
                // + 16–21 (IRAP: BLA/IDR/CRA — all keyframe-class); SEI 39/40.
                let nalType = (raw[range.lowerBound] >> 1) & 0x3F
                switch nalType {
                case 32: // VPS
                    let vps = Data(bytes: raw.baseAddress! + range.lowerBound, count: range.count)
                    if vps != currentVPS { currentVPS = vps; needsNewFormat = true }
                case 33: // SPS
                    let sps = Data(bytes: raw.baseAddress! + range.lowerBound, count: range.count)
                    if sps != currentSPS { currentSPS = sps; needsNewFormat = true }
                case 34: // PPS
                    let pps = Data(bytes: raw.baseAddress! + range.lowerBound, count: range.count)
                    if pps != currentPPS { currentPPS = pps; needsNewFormat = true }
                case 0...9, 16...21, 39, 40: // slices, IRAP, SEI
                    if (16...21).contains(nalType) { containsIDR = true }
                    displayRanges.append(range)
                    avccLength += 4 + range.count
                default:
                    break
                }
            }
        }
        return (displayRanges, avccLength, containsIDR, needsNewFormat)
    }

    // MARK: - Format Description

    private func createFormatDescription(sps: Data, pps: Data) -> CMVideoFormatDescription? {
        do {
            return try CMVideoFormatDescription(h264ParameterSets: [sps, pps], nalUnitHeaderLength: 4)
        } catch {
            Self.logger.error("Failed to create format description: \(error.localizedDescription, privacy: .public)")
            return nil
        }
    }

    private func createHEVCFormatDescription(vps: Data, sps: Data, pps: Data) -> CMVideoFormatDescription? {
        do {
            return try CMVideoFormatDescription(hevcParameterSets: [vps, sps, pps], nalUnitHeaderLength: 4)
        } catch {
            Self.logger.error("Failed to create HEVC format description: \(error.localizedDescription, privacy: .public)")
            return nil
        }
    }

    // MARK: - Sample Buffer Creation

    /// V1 ZERO-COPY: wrap the already-AVCC `Data` directly as the CMBlockBuffer payload — no copy,
    /// no parse. The `Data` is bridged to an `NSData` (whose `bytes` pointer is stable for its
    /// lifetime); we retain that NSData and hand the CMBlockBuffer a custom block source whose
    /// FreeBlock releases it when the last CMSampleBuffer reference drops.
    ///
    /// Lifetime: `passRetained(ns)` takes +1. On SUCCESS, CoreMedia owns that +1 and releases it via
    /// FreeBlock. On CMBlockBuffer FAILURE (FreeBlock never called) we release the +1 here. If
    /// CMSampleBufferCreateReady later fails, ARC releases the CMBlockBuffer, which calls FreeBlock —
    /// so no manual release is needed there.
    private func createSampleBufferZeroCopy(avcc: Data,
                                            containsIDR: Bool,
                                            formatDescription: CMVideoFormatDescription) -> CMSampleBuffer? {
        let ns = avcc as NSData
        let length = ns.length
        // NSData.bytes is always contiguous; this guard is defensive (empty / bridge anomaly) and
        // routes to the single-copy fallback rather than wrapping a bad pointer.
        guard length > 0, length == avcc.count else {
            return createSampleBufferCopyFallback(avcc: avcc, containsIDR: containsIDR, formatDescription: formatDescription)
        }
        let bytes = ns.bytes // valid for the retained NSData's lifetime
        let refCon = Unmanaged.passRetained(ns).toOpaque()

        var source = CMBlockBufferCustomBlockSource(
            version: kCMBlockBufferCustomBlockSourceVersion,
            AllocateBlock: nil, // memory already exists (ns.bytes) — CM must not allocate
            FreeBlock: { refCon, _, _ in
                if let refCon { Unmanaged<NSData>.fromOpaque(refCon).release() }
            },
            refCon: refCon
        )

        var blockBuffer: CMBlockBuffer?
        let status = CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault,
            memoryBlock: UnsafeMutableRawPointer(mutating: bytes),
            blockLength: length,
            blockAllocator: kCFAllocatorNull,  // never free memoryBlock via allocator; FreeBlock owns it
            customBlockSource: &source,
            offsetToData: 0,
            dataLength: length,
            flags: 0,
            blockBufferOut: &blockBuffer
        )

        guard status == kCMBlockBufferNoErr, let blockBuffer else {
            // Creation failed ⇒ FreeBlock will NOT be called ⇒ release the retain we took.
            Unmanaged<NSData>.fromOpaque(refCon).release()
            Self.logger.error("zero-copy block buffer failed: \(status, privacy: .public) — falling back to copy")
            return createSampleBufferCopyFallback(avcc: avcc, containsIDR: containsIDR, formatDescription: formatDescription)
        }

        return finishSampleBuffer(blockBuffer: blockBuffer, length: length,
                                  containsIDR: containsIDR, formatDescription: formatDescription)
    }

    /// Fallback: a single whole-payload copy into a CM-allocated block (still 1 copy vs the old
    /// path's 2). Used only if the zero-copy wrap is refused.
    private func createSampleBufferCopyFallback(avcc: Data,
                                                containsIDR: Bool,
                                                formatDescription: CMVideoFormatDescription) -> CMSampleBuffer? {
        let length = avcc.count
        guard length > 0 else { return nil }
        var blockBuffer: CMBlockBuffer?
        let allocStatus = CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: length,
            blockAllocator: kCFAllocatorDefault, customBlockSource: nil,
            offsetToData: 0, dataLength: length, flags: 0, blockBufferOut: &blockBuffer)
        guard allocStatus == kCMBlockBufferNoErr, let blockBuffer else {
            Self.logger.error("fallback block buffer alloc failed: \(allocStatus, privacy: .public)")
            return nil
        }
        let copyStatus = avcc.withUnsafeBytes { (raw: UnsafeRawBufferPointer) -> OSStatus in
            guard let base = raw.baseAddress else { return OSStatus(kCMBlockBufferBadPointerParameterErr) }
            return CMBlockBufferReplaceDataBytes(with: base, blockBuffer: blockBuffer,
                                                 offsetIntoDestination: 0, dataLength: length)
        }
        guard copyStatus == kCMBlockBufferNoErr else {
            Self.logger.error("fallback copy into block buffer failed: \(copyStatus, privacy: .public)")
            return nil
        }
        return finishSampleBuffer(blockBuffer: blockBuffer, length: length,
                                  containsIDR: containsIDR, formatDescription: formatDescription)
    }

    /// Format-change path: builds the AVCC sample (4-byte BE length prefix + NAL bytes, per NAL) by
    /// writing straight from the source buffer into a CM-allocated block — one copy of the frame data.
    private func createSampleBuffer(nalData: Data,
                                    displayRanges: [Range<Int>],
                                    avccLength: Int,
                                    containsIDR: Bool,
                                    formatDescription: CMVideoFormatDescription) -> CMSampleBuffer? {
        var finalBlock: CMBlockBuffer?

        let allocStatus = CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault,
            memoryBlock: nil,              // let CoreMedia allocate
            blockLength: avccLength,
            blockAllocator: kCFAllocatorDefault,
            customBlockSource: nil,
            offsetToData: 0,
            dataLength: avccLength,
            flags: 0,
            blockBufferOut: &finalBlock
        )

        guard allocStatus == kCMBlockBufferNoErr, let finalBlock else {
            Self.logger.error("Failed to create block buffer: \(allocStatus, privacy: .public)")
            return nil
        }

        let replaceStatus = nalData.withUnsafeBytes { (raw: UnsafeRawBufferPointer) -> OSStatus in
            guard let base = raw.baseAddress else { return OSStatus(kCMBlockBufferBadPointerParameterErr) }
            var offset = 0
            for range in displayRanges {
                var lengthBE = UInt32(range.count).bigEndian
                var status = withUnsafeBytes(of: &lengthBE) { lenPtr in
                    CMBlockBufferReplaceDataBytes(
                        with: lenPtr.baseAddress!,
                        blockBuffer: finalBlock,
                        offsetIntoDestination: offset,
                        dataLength: 4
                    )
                }
                guard status == kCMBlockBufferNoErr else { return status }
                offset += 4
                status = CMBlockBufferReplaceDataBytes(
                    with: base + range.lowerBound,
                    blockBuffer: finalBlock,
                    offsetIntoDestination: offset,
                    dataLength: range.count
                )
                guard status == kCMBlockBufferNoErr else { return status }
                offset += range.count
            }
            return kCMBlockBufferNoErr
        }

        guard replaceStatus == kCMBlockBufferNoErr else {
            Self.logger.error("Failed to copy data into block buffer: \(replaceStatus, privacy: .public)")
            return nil
        }

        return finishSampleBuffer(blockBuffer: finalBlock, length: avccLength,
                                  containsIDR: containsIDR, formatDescription: formatDescription)
    }

    /// Shared tail: timing (frameCount scheme), CMSampleBufferCreateReady, and the NotSync attachment
    /// for non-keyframes. Used by every sample-buffer path.
    private func finishSampleBuffer(blockBuffer: CMBlockBuffer,
                                    length: Int,
                                    containsIDR: Bool,
                                    formatDescription: CMVideoFormatDescription) -> CMSampleBuffer? {
        // Timing
        frameCount += 1
        decodedCount.wrappingAdd(1, ordering: .relaxed) // AD: a sample buffer was actually built
        var timing = CMSampleTimingInfo(
            duration: CMTime(value: 1, timescale: 60),
            presentationTimeStamp: CMTime(value: frameCount, timescale: 60),
            decodeTimeStamp: .invalid
        )

        var sampleBuffer: CMSampleBuffer?
        var sampleSize = length

        let sampleStatus = CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault,
            dataBuffer: blockBuffer,
            formatDescription: formatDescription,
            sampleCount: 1,
            sampleTimingEntryCount: 1,
            sampleTimingArray: &timing,
            sampleSizeEntryCount: 1,
            sampleSizeArray: &sampleSize,
            sampleBufferOut: &sampleBuffer
        )

        guard sampleStatus == noErr, let sb = sampleBuffer else {
            Self.logger.error("Failed to create sample buffer: \(sampleStatus, privacy: .public)")
            return nil
        }

        // Mark non-IDR samples as not-sync. Without this every P-frame
        // claims to be a keyframe, so after a flush the renderer resumes
        // decode on a stale P-frame (gray smear) instead of waiting for the
        // real IDR the recovery path requested.
        if !containsIDR,
           let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: true),
           CFArrayGetCount(attachments) > 0 {
            let dict = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(
                dict,
                Unmanaged.passUnretained(kCMSampleAttachmentKey_NotSync).toOpaque(),
                Unmanaged.passUnretained(kCFBooleanTrue).toOpaque()
            )
        }
        // Low-latency display is handled by Receiver.enqueueImmediately(_:), which presents this frame
        // as soon as possible and supersedes any pending frames.
        return sb
    }

    // MARK: - Layer Enqueue (V5 enqueue FIFO → Main Thread)

    /// Submit a decoded sample buffer to the bounded enqueue FIFO. Never blocks the decodeQueue: a full
    /// FIFO resolves to a drop, but (unlike the V4 latest-wins slot) the drop policy protects IDRs and
    /// the frame after them and sheds the STALEST unprotected P instead of whatever collided.
    private func enqueue(_ sampleBuffer: CMSampleBuffer, generation: UInt64, keyframe: Bool,
                         nalMask: UInt64, arrivalNs: UInt64) {
        // The sample buffer now exists; close the arrival → wrapped EWMA here (decodeQueue — the single
        // writer of that cell) before the frame joins the enqueue FIFO. This is a WRAP time, not a decode
        // time: nothing has been decoded yet — the renderer does that after the hand-off.
        Self.updateEWMA(wrapLatencyBits, sinceNs: arrivalNs)
        // CMSampleBuffer is not Sendable but is freshly created per frame and handed off exclusively
        // through the Mutex, so parking it in the FIFO is safe.
        nonisolated(unsafe) let sb = sampleBuffer
        let admission = pendingEnqueue.withLock { fifo in
            fifo.push(PendingSB(sb: sb, generation: generation, keyframe: keyframe,
                                nalMask: nalMask, arrivalNs: arrivalNs),
                      keyframe: keyframe)
        }
        noteAdmission(admission, queue: "enqueue")
        if admission.wasEmpty { renderQueue.async { [weak self] in self?.drainEnqueue() } }
    }

    /// renderQueue: take the OLDEST decoded frame from the enqueue FIFO and present it through the
    /// generation-guarded receiver switch. Re-dispatches itself while the FIFO still has work — without
    /// that tail dispatch a depth >1 FIFO would strand every frame pushed onto a non-empty queue.
    private func drainEnqueue() {
        guard let pending = pendingEnqueue.withLock({ $0.pop() }) else { return }
        defer {
            if pendingEnqueue.withLock({ !$0.isEmpty }) {
                renderQueue.async { [weak self] in self?.drainEnqueue() }
            }
        }
        let generation = pending.generation
        // Closes the arrival → hand-off EWMA (renderQueue — the single writer of that cell). Measured
        // here rather than after `enqueueImmediately` so the number is the queueing delay we control,
        // not the renderer's own call cost.
        Self.updateEWMA(handoffLatencyBits, sinceNs: pending.arrivalNs)
        // Wrap the freshly built CMSampleBuffer in the typed CMReadySampleBuffer the macOS 26
        // receiver API requires, then present it immediately.
        let ready = CMReadySampleBuffer(unsafeBuffer: pending.sb)
        switch self.receiver.enqueueImmediately(ready) {
        case .enqueued:
            rendererWedgedLogged = false   // clean frame — re-arm the wedged edge log
        case .enqueuedWithDecodeFailures(let errors):
            // Accepted but decoded WITH ERRORS — typically a P-frame that landed on a freshly restarted
            // decompression session (after a format-description swap) with no reference IDR yet. VT does
            // NOT return .cancelledDueToFlushRequiredToResume for this, so the recovery branch below is
            // never taken and, without action here, the stream stays FROZEN (the item-3 root cause).
            // Ask the owner for a keyframe so iOS emits a fresh IDR to re-sync; requestKeyframe is
            // throttled client-side (≤1/500 ms), so a burst of failures yields at most ~2 requests/sec.
            // No flush/format-reset — the format is correct; the session just needs an IDR.
            //
            // The LOG is throttled (2026-09-03) and carries the evidence the untagged one-liner lacked:
            // which lane, VideoToolbox's own status (`NSOSStatusErrorDomain` code == the OSStatus), the
            // NAL types of the frame it rejected, and how many failures were suppressed since the last
            // line. It fired ~50/s from the main thread — the same thread whose lateness sheds the
            // frames that cause these failures — so rate-limiting it also stops the log feeding the loop.
            // `onNeedsKeyFrame` stays UNTHROTTLED: recovery latency is the client's ≤1/500 ms to own.
            let suppressed = suppressedFailures.wrappingAdd(1, ordering: .relaxed).newValue
            if Self.throttlePasses(lastFailLogNs) {
                suppressedFailures.store(0, ordering: .relaxed)
                let detail = errors.map { e -> String in
                    let ns = e as NSError
                    return "\(ns.domain):\(ns.code)"
                }.joined(separator: ",")
                Self.logger.warning("""
                [\(self.label, privacy: .public)] decode FAILURES on \(pending.keyframe ? "IDR" : "P", privacy: .public) \
                frame — vt=[\(detail.isEmpty ? "none" : detail, privacy: .public)] \
                nal={\(AVCCFastPath.nalTypeSummary(pending.nalMask), privacy: .public)} \
                gen=\(generation, privacy: .public) (+\(suppressed &- 1, privacy: .public) suppressed) \
                — requesting keyframe to re-sync
                """)
            }
            self.onNeedsKeyFrame?()
        case .cancelledDueToFlushRequiredToResume(let error):
            // This branch used to log NOTHING, so a session that took it was indistinguishable in the
            // log from one that only ever hit .enqueuedWithDecodeFailures — the two have completely
            // different recoveries (format reset vs none). Edge it against the same throttle.
            if Self.throttlePasses(lastFailLogNs) {
                let ns = error as NSError
                Self.logger.error("""
                [\(self.label, privacy: .public)] renderer requires FLUSH to resume — vt=\(ns.domain, privacy: .public):\(ns.code, privacy: .public) \
                nal={\(AVCCFastPath.nalTypeSummary(pending.nalMask), privacy: .public)} gen=\(generation, privacy: .public) \
                — flushing + re-seeding the format on the next IDR
                """)
            }
            // Decoder needs an IDR after a flush. Flush the receiver, drop the cached format so the
            // next keyframe re-seeds it (from configuredParameterSets — the AVCC fast path carries no
            // in-band SPS/PPS), and ask the owner to request a keyframe. Generation-guarded: several
            // stale frames may hit this branch back-to-back, and a late one must not wipe a format a
            // newer frame just rebuilt (nor spam duplicate keyframe requests).
            self.receiver.flush()
            self.decodeQueue.async {
                guard self.formatGeneration == generation else { return }
                self.formatDescription = nil
                self.currentVPS = nil
                self.currentSPS = nil
                self.currentPPS = nil
                self.formatGeneration &+= 1
                self.onNeedsKeyFrame?()
            }
        case .cancelledDueToFlush:
            break
        case .cancelledDueToError(let error):
            // Nav-freeze instrumentation (Agent-3 hypothesis, 2026-08-01): on macOS 27 the renderer's
            // failed/requires-flush state arrives HERE via the EnqueueResult (the old
            // `requiresFlushToResumeDecoding`/`status` properties are deprecated for exactly this).
            // This case currently only logs — no recovery — so if the alt/cluster lane wedges after a
            // view switch, every later enqueue silently no-ops (altOK keeps climbing) = frozen picture.
            // Edge-log per lane so a repro pins this vs the configValid latch (logged in OCBMAVBridge)
            // vs PTS drift (no error at all). Logging only — confirm before writing recovery.
            if !rendererWedgedLogged {
                rendererWedgedLogged = true
                Self.logger.error("[\(self.label, privacy: .public)] renderer WEDGED — enqueueImmediately .cancelledDueToError: \(error.localizedDescription, privacy: .public) (no recovery yet; frames will silently drop until flush)")
            }
        @unknown default:
            break
        }
    }

    // MARK: - Reset

    func flush() {
        // Route through the decode queue first so decodes already in flight
        // (or queued) post their enqueue blocks BEFORE the receiver flush on
        // renderQueue — otherwise stale frames from the old stream would
        // be enqueued into the freshly flushed renderer. Both queues are serial and the hop is
        // decodeQueue → renderQueue, so that ordering is now a queue guarantee rather than a race
        // against unrelated main-thread work.
        decodeQueue.async { [weak self] in
            guard let self else { return }
            self.frameCount = 0
            self.pendingDecode.withLock { $0.removeAll() }   // drop any queued-but-undecoded frames
            self.renderQueue.async {
                self.pendingEnqueue.withLock { $0.removeAll() } // drop decoded-but-unenqueued frames
                self.receiver.flush()
            }
        }
    }
}
