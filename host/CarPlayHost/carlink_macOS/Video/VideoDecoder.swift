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
/// per-frame work off the main thread. Only the final receiver enqueue hops to main. Both the
/// decode and enqueue hops are single-slot latest-wins (V4) — a stalled consumer drops frames rather
/// than buffering them (the live-UI "drop on backpressure, never buffer" rule).
final class VideoDecoder: @unchecked Sendable {

    private static let logger = Logger(subsystem: "com.carlink.video", category: "decoder")

    let displayLayer = AVSampleBufferDisplayLayer()

    /// Lane label for logs ("main" / "alt"), set by the owner. Lets the nav-freeze instrumentation
    /// below distinguish the cluster lane from the main screen.
    var label = "video"
    /// Edge-trigger for the renderer-wedged log (main-thread-confined, drainEnqueue only).
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
    /// shed by the depth-1 decode/enqueue slots (latest-wins collisions). AR (decrypted, in the decrypt
    /// metrics) − AD = decode-slot loss — together they localize whether the cluster lane starves upstream
    /// of the app or drops in-app. Atomics (not the decodeQueue-confined frameCount) so the ~1 Hz monitor
    /// can read them off the main queue without a data race.
    let decodedCount = Atomic<UInt64>(0)
    let slotDrops = Atomic<UInt64>(0)

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

    // MARK: - V4 single-slot latest-wins handoffs

    /// A frame awaiting decode (USB read queue → decodeQueue).
    private struct PendingAVCC { let avcc: Data; let keyframe: Bool }
    /// A decoded sample buffer awaiting enqueue (decodeQueue → main). CMSampleBuffer is not Sendable;
    /// this box is a freshly-built, single-owner hand-off (only ever stored/taken under the Mutex), so
    /// @unchecked Sendable is sound and lets the value flow through the `sending` withLock body.
    private final class PendingSB: @unchecked Sendable {
        let sb: CMSampleBuffer; let generation: UInt64; let keyframe: Bool
        init(sb: CMSampleBuffer, generation: UInt64, keyframe: Bool) {
            self.sb = sb; self.generation = generation; self.keyframe = keyframe
        }
    }

    /// Decode FIFO. Depth 1 (default) is the original latest-wins single slot: a stalled decodeQueue
    /// drops frames rather than accumulating (the CarPlay/OCBM path — byte-for-byte unchanged). AA
    /// raises `maxDecodeDepth` so the opening IDR plus the P-frames that land during VideoToolbox's
    /// one-time decode warm-up are queued instead of shed; a single shed P-frame there breaks the
    /// whole P-chain until AA's next (sparse) periodic IDR, which is the startup flash/pixelation.
    private let pendingDecode = Mutex<[PendingAVCC]>([])
    /// Max frames the decode FIFO holds before latest-wins dropping resumes. 1 = prior behaviour.
    /// Set >1 (AA lane only) to absorb the startup burst. Assign once at setup, before frames flow.
    var maxDecodeDepth = 1
    /// Depth-1 enqueue slot: a stalled main thread drops decoded frames rather than buffering full
    /// CMSampleBuffers without bound.
    private let pendingEnqueue = Mutex<PendingSB?>(nil)

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

    // MARK: - Main Entry Point (V1 AVCC fast path + V4 decode slot)

    /// Submit a length-prefixed (4-byte AVCC) access unit for decode+display. Safe to call from any
    /// thread. The frame lands in the depth-1 decode slot; a busy decodeQueue drops per the V4 table.
    func decodeAndDisplay(avcc: Data, keyframe: Bool) {
        let (dispatch, requestKF): (Bool, Bool) = pendingDecode.withLock { queue in
            let wasEmpty = queue.isEmpty
            if queue.count < maxDecodeDepth {
                queue.append(PendingAVCC(avcc: avcc, keyframe: keyframe))
                return (wasEmpty, false)               // room in the FIFO; nothing shed
            }
            // FIFO full → latest-wins against the newest queued frame (depth-1: the only frame).
            let old = queue[queue.count - 1]
            let d = AVCCFastPath.resolveSlot(oldIsKF: old.keyframe, newIsKF: keyframe)
            slotDrops.wrappingAdd(1, ordering: .relaxed) // one frame lost per collision
            if d.store { queue[queue.count - 1] = PendingAVCC(avcc: avcc, keyframe: keyframe) }
            return (false, d.requestKeyframe)          // dropped a frame; no new drain to dispatch
        }
        if requestKF { onFrameDropped?() }
        if dispatch { decodeQueue.async { [weak self] in self?.drainDecode() } }
    }

    /// decodeQueue: take whatever frame currently occupies the decode slot (latest-wins) and process
    /// it. One item per drain; the next empty→occupied submit dispatches the next drain.
    private func drainDecode() {
        guard let pending = pendingDecode.withLock({ (queue: inout [PendingAVCC]) -> PendingAVCC? in
            queue.isEmpty ? nil : queue.removeFirst()
        }) else { return }
        performDecodeAVCC(avcc: pending.avcc, keyframe: pending.keyframe)
        // Depth >1: a drain handles one frame, so re-dispatch while the FIFO still has work.
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
    private func performDecodeAVCC(avcc: Data, keyframe: Bool) {
        let isHEVC = codec == .hevc
        let walk = avcc.withUnsafeBytes { AVCCFastPath.walkAVCC($0, isHEVC: isHEVC) }
        guard walk.valid else {
            Self.logger.error("AVCC frame failed length-walk validation (\(avcc.count, privacy: .public) B) — dropped")
            return
        }

        if walk.hasParamSets {
            // Mid-stream in-band SPS/PPS (format change): strip/diff/rebuild exactly like the legacy
            // path. One extra classification copy — on format-change frames only, zero in steady state.
            performDecodeAVCCWithParamSets(avcc: avcc, isHEVC: isHEVC)
            return
        }

        // Steady state: no in-band parameter sets in this AU.
        if formatDescription == nil {
            // Post-flush: flush-recovery nilled the format + parameter sets and RELIES on this re-seed
            // (the AVCC fast path prepends no in-band SPS/PPS to rebuild from). Only a keyframe can
            // re-seed usefully; a P-frame with no format is undecodable → drop + request an IDR.
            guard walk.containsIDR else {
                onNeedsKeyFrame?()
                return
            }
            guard reseedFormatFromConfigured() else {
                onNeedsKeyFrame?()
                return
            }
        }

        guard let fmt = formatDescription else { return }
        if let sb = createSampleBufferZeroCopy(avcc: avcc, containsIDR: walk.containsIDR, formatDescription: fmt) {
            enqueue(sb, generation: formatGeneration, keyframe: walk.containsIDR)
        }
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
    private func performDecodeAVCCWithParamSets(avcc: Data, isHEVC: Bool) {
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
            enqueue(sb, generation: formatGeneration, keyframe: containsIDR)
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

    // MARK: - Layer Enqueue (V4 enqueue slot → Main Thread)

    /// Submit a decoded sample buffer to the depth-1 enqueue slot. A stalled main thread drops
    /// decoded frames (latest-wins) rather than accumulating full CMSampleBuffers without bound.
    private func enqueue(_ sampleBuffer: CMSampleBuffer, generation: UInt64, keyframe: Bool) {
        // CMSampleBuffer is not Sendable but is freshly created per frame and handed off exclusively
        // through the Mutex, so parking it in the slot is safe.
        nonisolated(unsafe) let sb = sampleBuffer
        let (dispatch, requestKF): (Bool, Bool) = pendingEnqueue.withLock { slot in
            let d = AVCCFastPath.resolveSlot(oldIsKF: slot?.keyframe, newIsKF: keyframe)
            let wasEmpty = slot == nil
            if !wasEmpty { slotDrops.wrappingAdd(1, ordering: .relaxed) } // decoded-but-not-displayed loss
            if d.store {
                slot = PendingSB(sb: sb, generation: generation, keyframe: keyframe)
                return (wasEmpty, d.requestKeyframe)
            }
            return (false, d.requestKeyframe)
        }
        if requestKF { onFrameDropped?() }
        if dispatch { DispatchQueue.main.async { [weak self] in self?.drainEnqueue() } }
    }

    /// Main thread: take the latest decoded frame from the enqueue slot and present it through the
    /// generation-guarded receiver switch.
    private func drainEnqueue() {
        guard let pending = pendingEnqueue.withLock({ (slot: inout PendingSB?) -> PendingSB? in
            let p = slot; slot = nil; return p
        }) else { return }
        let generation = pending.generation
        // Wrap the freshly built CMSampleBuffer in the typed CMReadySampleBuffer the macOS 26
        // receiver API requires, then present it immediately.
        let ready = CMReadySampleBuffer(unsafeBuffer: pending.sb)
        // DECODE-LATENCY HOOK (deferred — StreamMetricsMonitor `decodeLatencyMs`): this render path uses
        // the macOS-26 AVSampleBufferRenderSynchronizer receiver, which exposes no VTDecompressionSession
        // decode start/finish callback, so a true submit→decoded time is not cleanly observable here. If a
        // future build routes frames through an explicit VTDecompressionSession, time the decompression
        // callback and feed the rolling stat out via a `@Sendable (Double) -> Void` hook installed by the
        // owner. Until then the received-frame `jitterMs` at the decrypt layer is the stutter proxy.
        switch self.receiver.enqueueImmediately(ready) {
        case .enqueued:
            rendererWedgedLogged = false   // clean frame — re-arm the wedged edge log
        case .enqueuedWithDecodeFailures:
            // Accepted but decoded WITH ERRORS — typically a P-frame that landed on a freshly restarted
            // decompression session (after a format-description swap) with no reference IDR yet. VT does
            // NOT return .cancelledDueToFlushRequiredToResume for this, so the recovery branch below is
            // never taken and, without action here, the stream stays FROZEN (the item-3 root cause).
            // Ask the owner for a keyframe so iOS emits a fresh IDR to re-sync; requestKeyframe is
            // throttled client-side (≤1/500 ms), so a burst of failures yields at most ~2 requests/sec.
            // No flush/format-reset — the format is correct; the session just needs an IDR.
            Self.logger.warning("Frame enqueued with decode failures — requesting keyframe to re-sync")
            self.onNeedsKeyFrame?()
        case .cancelledDueToFlushRequiredToResume:
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
        // the main queue — otherwise stale frames from the old stream would
        // be enqueued into the freshly flushed renderer.
        decodeQueue.async { [weak self] in
            guard let self else { return }
            self.frameCount = 0
            self.pendingDecode.withLock { $0 = [] }     // drop any queued-but-undecoded frames
            DispatchQueue.main.async {
                self.pendingEnqueue.withLock { $0 = nil } // drop any decoded-but-unenqueued frame
                self.receiver.flush()
            }
        }
    }
}
