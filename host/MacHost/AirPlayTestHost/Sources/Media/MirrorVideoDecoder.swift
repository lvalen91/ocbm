// MirrorVideoDecoder.swift — H.264 and HEVC decode for the AirPlay screen stream through an explicit
// VTDecompressionSession, handing every decoded picture to `onFrame`.
//
// Why not the shipping `VideoDecoder`: that class decodes inside AVSampleBufferDisplayLayer's renderer,
// which reports no per-frame result, and it is built around a keyframe request: a full queue sheds a
// frame and asks for an IDR, a renderer that wants a flush drops every P-frame until one arrives. AirPlay
// mirroring sends ONE IDR per session and has no keyframe request (docs/ocbm/02_AIRPLAY_ONLY_MODE.md
// §4.6), so here nothing is ever shed and nothing waits for a keyframe: every frame is submitted in
// order, a frame VideoToolbox rejects is counted, and decoding goes on. The record parsing and the NAL
// length rewrite are the shipping app's (`AVCCFastPath`); which codec a config body carries is
// `MirrorVideoConfig.parse` (an avcC, or an 'hvc1' box wrapping an hvcC, §4.10).
//
// Codec changes: the phone chooses the codec per mirroring session. A config whose codec or parameter
// sets the running session cannot take tears that session down and builds a new one; a config that
// cannot be used at all leaves NO session, so the frames that follow are counted as `unconfigured`
// instead of being fed to a decoder for the other codec.
//
// Threading: `configure`, `decode`, `reset` and `finish` belong to ONE caller queue (the client's).
// VideoToolbox calls back on its own thread; the tallies are behind a mutex and `onFrame` is called
// from that thread.

import CoreMedia
import Foundation
import Synchronization
import VideoToolbox

final class MirrorVideoDecoder: @unchecked Sendable {
    struct Stats: Equatable, Sendable {
        /// Frames handed to VideoToolbox since the last `reset`.
        var submitted: UInt64 = 0
        /// Pictures VideoToolbox produced.
        var decoded: UInt64 = 0
        /// Frames it rejected (bad data, missing reference), or that could not be submitted at all.
        var errors: UInt64 = 0
        /// The OSStatus of the most recent error.
        var lastError: Int32 = 0
        /// Frames that arrived with no usable configuration in force.
        var unconfigured: UInt64 = 0
        var width = 0
        var height = 0
        var profile: UInt8 = 0
        var level: UInt8 = 0
        /// The codec of the configuration in force; nil before one (or after one that failed).
        var codec: MirrorVideoCodec?
        /// "HEVC Main L5.1" / "H.264 High L3.1"; empty with no configuration.
        var codecSummary = ""
        /// VideoToolbox reports a hardware decoder for the session in force; nil = no session.
        var hardware: Bool?
        /// Why the last configuration could not be used (shown and logged); nil once one works.
        var configProblem: String?
        var sessions: UInt64 = 0
        /// Configurations that changed the codec of a running decoder (no reset in between).
        var codecSwitches: UInt64 = 0
    }

    /// When the frame reached the decoder (host uptime, s) and its header timestamp (phone clock, s).
    struct FrameTime: Equatable, Sendable {
        var arrival: Double
        var stream: Double
    }

    /// Whether this Mac has a hardware HEVC decoder. Without one VideoToolbox may still decode HEVC in
    /// software (slowly at 4K); `configure` reports which one it got.
    static let hevcHardwareAvailable = VTIsHardwareDecodeSupported(kCMVideoCodecType_HEVC)

    /// Called on VideoToolbox's thread with each decoded picture (BGRA, IOSurface-backed).
    private let onFrame: @Sendable (CVPixelBuffer, FrameTime) -> Void
    private let stats = Mutex(Stats())

    // Caller-queue-confined.
    private var session: VTDecompressionSession?
    private var format: CMVideoFormatDescription?
    private var record: [UInt8] = []
    private var codec: MirrorVideoCodec?
    private var nalLengthSize = 4
    private var frameIndex: Int64 = 0

    init(onFrame: @escaping @Sendable (CVPixelBuffer, FrameTime) -> Void) {
        self.onFrame = onFrame
    }

    deinit {
        if let session { VTDecompressionSessionInvalidate(session) }
    }

    var snapshot: Stats { stats.withLock { $0 } }

    /// A new stream (a fresh key on the lane): the old parameter sets and reference pictures are void.
    func reset() {
        dropSession()
        frameIndex = 0
        stats.withLock { $0 = Stats(sessions: $0.sessions, codecSwitches: $0.codecSwitches) }
    }

    private func dropSession() {
        if let session {
            VTDecompressionSessionWaitForAsynchronousFrames(session)
            VTDecompressionSessionInvalidate(session)
        }
        session = nil
        format = nil
        record = []
        codec = nil
    }

    /// The opcode-1 body. A byte-identical body (the box re-sends the last one after a seam reconnect)
    /// changes nothing: rebuilding would discard the reference pictures of a stream that has no second
    /// IDR. Returns false when the body carries no usable avcC / hvcC, or no decoder takes it; the
    /// reason is in `snapshot.configProblem`.
    @discardableResult
    func configure(_ body: [UInt8]) -> Bool {
        if body == record, session != nil { return true }
        guard let cfg = MirrorVideoConfig.parse(body) else {
            // Not counted in `errors` (no frame was rejected); `configProblem` says it.
            return refuse(nil, "the config frame (\(body.count) B) is neither an avcC nor an hvc1/hvcC")
        }
        var fmt: CMVideoFormatDescription?
        let status = Self.formatDescription(cfg, &fmt)
        guard status == noErr, let fmt else {
            return refuse(status, "\(cfg.codec.rawValue) parameter sets refused by CoreMedia (OSStatus \(status))")
        }
        let previous = codec
        if let session, previous == cfg.codec, VTDecompressionSessionCanAcceptFormatDescription(session, formatDescription: fmt) {
            // Same stream, compatible parameter sets: keep the session and its reference pictures.
        } else {
            dropSession()
            var created: OSStatus = noErr
            session = Self.makeSession(fmt, status: &created)
            guard session != nil else {
                let why = cfg.codec == .hevc && !Self.hevcHardwareAvailable
                    ? "HEVC cannot be decoded on this Mac: no hardware HEVC decoder, and VideoToolbox refused a software one (OSStatus \(created)). Turn HEVC off and start again"
                    : "VideoToolbox could not create a \(cfg.codec.rawValue) decoder (OSStatus \(created))"
                return refuse(created == noErr ? kVTCouldNotCreateInstanceErr : created, why)
            }
            stats.withLock {
                $0.sessions += 1
                if let previous, previous != cfg.codec { $0.codecSwitches += 1 }
            }
        }
        format = fmt
        record = body
        codec = cfg.codec
        nalLengthSize = cfg.nalLengthSize
        let dims = CMVideoFormatDescriptionGetDimensions(fmt)
        let hw = session.map(Self.usesHardware)
        stats.withLock {
            $0.width = Int(dims.width)
            $0.height = Int(dims.height)
            $0.profile = cfg.profile
            $0.level = cfg.level
            $0.codec = cfg.codec
            $0.codecSummary = cfg.summary
            $0.hardware = hw
            $0.configProblem = nil
        }
        return true
    }

    /// No decoder after this: the frames that follow are `unconfigured`, never fed to the old codec's.
    private func refuse(_ status: OSStatus?, _ why: String) -> Bool {
        dropSession()
        stats.withLock {
            if let status {
                $0.errors += 1
                $0.lastError = status
            }
            $0.codec = nil
            $0.codecSummary = ""
            $0.hardware = nil
            $0.width = 0
            $0.height = 0
            $0.configProblem = why
        }
        return false
    }

    private static func formatDescription(_ cfg: MirrorVideoConfig, _ out: inout CMVideoFormatDescription?) -> OSStatus {
        let sets = cfg.parameterSets
        let sizes = sets.map(\.count)
        // Contiguous copies whose addresses stay valid for the duration of the call.
        let joined = sets.flatMap { $0 }
        return joined.withUnsafeBufferPointer { all -> OSStatus in
            guard let base = all.baseAddress else { return kCMFormatDescriptionError_InvalidParameter }
            var pointers: [UnsafePointer<UInt8>] = []
            var at = 0
            for n in sizes {
                pointers.append(base + at)
                at += n
            }
            switch cfg.codec {
            case .h264:
                return CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    allocator: kCFAllocatorDefault, parameterSetCount: sets.count, parameterSetPointers: pointers,
                    parameterSetSizes: sizes, nalUnitHeaderLength: 4, formatDescriptionOut: &out)
            case .hevc:
                return CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    allocator: kCFAllocatorDefault, parameterSetCount: sets.count, parameterSetPointers: pointers,
                    parameterSetSizes: sizes, nalUnitHeaderLength: 4, extensions: nil, formatDescriptionOut: &out)
            }
        }
    }

    private static func makeSession(_ fmt: CMVideoFormatDescription, status: inout OSStatus) -> VTDecompressionSession? {
        // BGRA and IOSurface-backed: a CALayer takes the surface as its contents with no conversion.
        let attrs: [CFString: Any] = [
            kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA,
            kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
        ]
        // Hardware when there is one; VideoToolbox falls back to software by itself when there is not.
        let spec: [CFString: Any] = [kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder: true]
        var s: VTDecompressionSession?
        status = VTDecompressionSessionCreate(
            allocator: kCFAllocatorDefault, formatDescription: fmt, decoderSpecification: spec as CFDictionary,
            imageBufferAttributes: attrs as CFDictionary, outputCallback: nil, decompressionSessionOut: &s)
        guard status == noErr, let s else { return nil }
        VTSessionSetProperty(s, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        return s
    }

    private static func usesHardware(_ s: VTDecompressionSession) -> Bool {
        // A Copy-rule (+1) CFBoolean written into a Swift optional, which then owns it.
        var value: CFTypeRef?
        let status = withUnsafeMutablePointer(to: &value) {
            VTSessionCopyProperty(s, key: kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
                                  allocator: kCFAllocatorDefault, valueOut: UnsafeMutableRawPointer($0))
        }
        return status == noErr && (value as? Bool) == true
    }

    /// One access unit of length-prefixed NAL units (the decrypted opcode-0 body). Never dropped here.
    func decode(_ frame: Data, time: FrameTime = FrameTime(arrival: 0, stream: 0)) {
        guard let session, let format else {
            stats.withLock { $0.unconfigured += 1 }
            return
        }
        let au: Data
        if nalLengthSize == 4 {
            au = frame
        } else if let fixed = AVCCFastPath.rewriteToFourByteLengths(frame, lenSize: nalLengthSize) {
            au = fixed
        } else {
            fail(kVTVideoDecoderBadDataErr)
            return
        }
        guard !au.isEmpty, let sample = Self.sampleBuffer(au, format: format, index: frameIndex) else {
            fail(kVTVideoDecoderBadDataErr)
            return
        }
        frameIndex += 1
        stats.withLock { $0.submitted += 1 }
        let status = VTDecompressionSessionDecodeFrame(
            session, sampleBuffer: sample, flags: [._EnableAsynchronousDecompression], infoFlagsOut: nil
        ) { [weak self] status, _, image, _, _ in
            guard let self else { return }
            guard status == noErr, let image else {
                self.fail(status == noErr ? kVTVideoDecoderBadDataErr : status)
                return
            }
            self.stats.withLock { $0.decoded += 1 }
            self.onFrame(image, time)
        }
        if status != noErr {
            fail(status)
            if status == kVTInvalidSessionErr {
                // The decoder was reclaimed (GPU reset, sleep). Rebuild it; with no second IDR in the
                // stream the frames that follow will be counted as errors, and that is the truth.
                VTDecompressionSessionInvalidate(session)
                var created: OSStatus = noErr
                self.session = Self.makeSession(format, status: &created)
                if self.session != nil { stats.withLock { $0.sessions += 1 } }
            }
        }
    }

    /// Block until every submitted frame has been answered (tests; teardown).
    func finish() {
        if let session { VTDecompressionSessionWaitForAsynchronousFrames(session) }
    }

    private func fail(_ status: OSStatus) {
        stats.withLock {
            $0.errors += 1
            $0.lastError = status
        }
    }

    private static func sampleBuffer(_ au: Data, format: CMVideoFormatDescription, index: Int64) -> CMSampleBuffer? {
        var block: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: au.count,
            blockAllocator: kCFAllocatorDefault, customBlockSource: nil, offsetToData: 0,
            dataLength: au.count, flags: kCMBlockBufferAssureMemoryNowFlag, blockBufferOut: &block) == noErr,
            let block else { return nil }
        let copied = au.withUnsafeBytes { raw -> OSStatus in
            guard let base = raw.baseAddress else { return kCMBlockBufferBadPointerParameterErr }
            return CMBlockBufferReplaceDataBytes(with: base, blockBuffer: block, offsetIntoDestination: 0, dataLength: au.count)
        }
        guard copied == noErr else { return nil }
        // The picture is shown the moment it is decoded; the timestamps only have to be monotonic.
        var timing = CMSampleTimingInfo(duration: CMTime(value: 1, timescale: 30),
                                        presentationTimeStamp: CMTime(value: index, timescale: 30),
                                        decodeTimeStamp: .invalid)
        var size = au.count
        var sample: CMSampleBuffer?
        guard CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: format, sampleCount: 1,
            sampleTimingEntryCount: 1, sampleTimingArray: &timing, sampleSizeEntryCount: 1,
            sampleSizeArray: &size, sampleBufferOut: &sample) == noErr else { return nil }
        return sample
    }
}
