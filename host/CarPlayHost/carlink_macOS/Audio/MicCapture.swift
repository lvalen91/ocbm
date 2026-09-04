import Foundation
@preconcurrency import AVFAudio
import os
import Synchronization

/// Captures microphone audio, converts to the CarPlay-negotiated format (sample rate + channels the box
/// reports on the type-100 `input` SETUP), and provides raw PCM16LE (little-endian) data via callback.
/// The box RTP-uplinks it to the iPhone. Capture is gated by the box's mic-uplink signal, so the engine
/// only runs during a live Siri/telephony turn — never a hot mic between turns.
final class MicCapture: @unchecked Sendable {

    private static let logger = Logger(subsystem: "com.carlink.audio", category: "Mic")

    /// Called with one CH_MIC chunk ready to ship to the box. For the PCM codec that is PCM16LE
    /// (little-endian) in EXACTLY 20 ms frames (see `uplinkFrameBytes`); the tap delivers 100 ms, so
    /// each callback fans out to five sends and the fractional remainder is carried, never dropped.
    /// For `OCBM.seamCodecMsbc` it is one 60-byte mSBC eSCO packet per 7.5 ms — the frame cadence is
    /// the codec's, not a choice, because the box writes each chunk to the SCO socket verbatim.
    var onPCMData: ((Data) -> Void)?

    private let engine = AVAudioEngine()
    private struct CaptureState {
        var isCapturing = false
        /// The active capture format — the box-negotiated rate/channels from the uplink gate.
        var targetRate: Double = 16000
        var targetChannels: UInt32 = 1
        /// The box-negotiated uplink codec: 0 = S16LE PCM, `OCBM.seamCodecMsbc` = HFP wideband.
        var targetCodec: UInt8 = 0
        /// Latest caller intent: true after startCapture, false after
        /// stopCapture. The deferred permission-grant retry and the
        /// configuration-change restart both abort if intent flipped while
        /// they were waiting — otherwise a TCC dialog answered after the
        /// Siri session ended would leave a hot mic no stop call can reach.
        var wantsCapture = false
    }
    private let state = Mutex(CaptureState())

    /// 20 ms uplink pacing + the per-second telemetry, shared between the audio IO thread (`processInput`)
    /// and controlQueue (start/stop reset it), hence the Mutex — touched once per tap callback (100 ms)
    /// plus once per emitted frame, never per sample.
    private struct UplinkTx {
        /// Samples left over from the previous callback: the converter returns whatever the resampler
        /// produced, which is not a multiple of 20 ms, so the tail must be carried into the next frame
        /// rather than padded (padding injects a click every 100 ms) or dropped (a slow drift).
        var residual = Data()
        var frames = 0
        var sumSq = 0.0
        var samples = 0
        var lastLog = 0.0
        /// The mSBC uplink encoder, non-nil only while the gate asked for codec 4. It lives HERE, in
        /// the same Mutex as the pacing state, because it is written on controlQueue (start/stop) and
        /// read on the audio IO thread: `stopCaptureLocked` removes the tap first, but a tap callback
        /// already in flight would otherwise race the store — an unsynchronised class-reference write
        /// under ARC, not merely a stale read.
        var msbc: MSBCUplinkEncoder?
    }
    private let tx = Mutex(UplinkTx())

    /// Bytes in one 20 ms uplink frame at `rate` × `channels` × 16-bit — 320 B for the AA telephony
    /// lane's 8 kHz mono, 640 B for CarPlay's 16 kHz mono Siri turn.
    static func uplinkFrameBytes(rate: Double, channels: UInt32) -> Int {
        max(2, Int(rate * 0.020) * Int(max(1, channels)) * 2)
    }

    /// Bytes of captured PCM one uplink chunk consumes. mSBC has no say in the matter: an eSCO
    /// packet is exactly one 120-sample frame (7.5 ms at 16 kHz), so the capture is cut there and
    /// encoded, and `onPCMData` receives 60 bytes instead of 240.
    static func uplinkChunkBytes(rate: Double, channels: UInt32, codec: UInt8) -> Int {
        codec == OCBM.seamCodecMsbc
            ? MSBCUplinkEncoder.pcmBytesPerPacket
            : uplinkFrameBytes(rate: rate, channels: channels)
    }


    /// Serializes all engine/tap mutation. startCapture and stopCapture can
    /// be invoked from the main actor, the detached permission task, and the
    /// configuration-change observer; AVAudioEngine is not thread-safe and
    /// an interleaved stop-between-claim-and-installTap previously left the
    /// tap running with isCapturing == false (unstoppable capture).
    private let controlQueue = DispatchQueue(label: "mic.control", qos: .userInitiated)

    private var configChangeObserver: NSObjectProtocol?

    init() {
        // Input device changes (AirPods connect, USB mic unplugged) stop the
        // engine and invalidate the tap's hardware format. Rebuild the whole
        // capture path if we are supposed to be capturing.
        configChangeObserver = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange,
            object: engine,
            queue: nil
        ) { [weak self] _ in
            self?.handleConfigurationChange()
        }
    }

    deinit {
        if let observer = configChangeObserver {
            NotificationCenter.default.removeObserver(observer)
        }
    }

    // MARK: - Permission

    /// Proactively surface the microphone TCC prompt (once) at session start, so permission is already
    /// granted by the time iOS opens the first Siri/telephony input stream. Requesting it lazily on the
    /// uplink-gate edge would pop a blocking dialog mid-Siri and clip the mic onset — and if the user is
    /// mid-drive, they may never see it. No-op if already granted/denied.
    func requestPermission() {
        guard AVAudioApplication.shared.recordPermission == .undetermined else { return }
        Self.logger.info("Pre-requesting microphone permission at session start")
        Task.detached { _ = await AVAudioApplication.requestRecordPermission() }
    }

    // MARK: - Start/Stop

    /// Start (or reconfigure) capture at the box-negotiated format. `sampleRate` is Hz (8/16/24/32/44.1/48
    /// kHz for wired LPCM); `channels` is ~always 1 for voice input. `codec` is the uplink encoding the
    /// box asked for: 0 = raw S16LE PCM, `OCBM.seamCodecMsbc` = mSBC air packets (HFP wideband).
    func startCapture(sampleRate: Double, channels: UInt32, codec: UInt8 = 0) {
        // mSBC is DEFINED at 16 kHz mono — the rate is not a parameter of the codec, it is part of
        // it. If the box ever asked for codec 4 at another rate, honouring the rate would encode
        // pitch-shifted speech into a frame the far end decodes as 16 kHz; honour the codec instead
        // and say so, because that mismatch is invisible at every later layer.
        var rate = sampleRate
        var ch = max(1, channels)
        if codec == OCBM.seamCodecMsbc && (rate != Double(MSBC.sampleRate) || ch != 1) {
            Self.logger.error("uplink gate asked for mSBC at \(rate, format: .fixed(precision: 0), privacy: .public) Hz/\(ch, privacy: .public)ch — mSBC is 16 kHz mono; capturing at 16 kHz mono")
            rate = Double(MSBC.sampleRate)
            ch = 1
        }
        state.withLock { $0.wantsCapture = true }
        controlQueue.sync { [self] in
            startCaptureLocked(sampleRate: rate, channels: ch, codec: codec)
        }
    }

    func stopCapture() {
        state.withLock { $0.wantsCapture = false }
        controlQueue.sync { [self] in
            stopCaptureLocked()
        }
    }

    private func handleConfigurationChange() {
        controlQueue.async { [self] in
            let (capturing, rate, channels, codec, wanted) = state.withLock {
                ($0.isCapturing, $0.targetRate, $0.targetChannels, $0.targetCodec, $0.wantsCapture)
            }
            guard capturing, wanted else { return }
            Self.logger.info("Audio configuration changed — rebuilding mic capture")
            stopCaptureLocked()
            startCaptureLocked(sampleRate: rate, channels: channels, codec: codec)
        }
    }

    /// Must run on controlQueue.
    private func startCaptureLocked(sampleRate: Double, channels: UInt32, codec: UInt8) {
        // Re-check intent HERE, inside the serialized section. The deferred
        // permission-grant task checks wantsCapture before hopping onto the
        // control queue, but a stopCapture can land in between — without this
        // re-check that interleaving still produced an unstoppable hot mic.
        guard state.withLock({ $0.wantsCapture }) else {
            Self.logger.info("Capture no longer wanted — not starting")
            return
        }
        let current = state.withLock { ($0.isCapturing, $0.targetRate, $0.targetChannels, $0.targetCodec) }
        if current.0 {
            // The codec is part of the format: a CVSD→mSBC re-negotiation keeps the rate at 16 kHz
            // on the CarPlay side, so comparing rate/channels alone would leave a PCM uplink running
            // against a box that is now writing what it gets straight into a wideband SCO socket.
            if current.1 == sampleRate && current.2 == channels && current.3 == codec { return }
            stopCaptureLocked()
        }

        // Check/request mic permission via AVAudioApplication (modern AVFAudio API)
        switch AVAudioApplication.shared.recordPermission {
        case .granted:
            break
        case .undetermined:
            Self.logger.info("Requesting microphone permission...")
            Task.detached { [weak self] in
                let granted = await AVAudioApplication.requestRecordPermission()
                guard let self else { return }
                guard granted else {
                    Self.logger.warning("Permission denied")
                    return
                }
                // The voice session may have ended while the dialog was up —
                // only proceed if capture is still wanted.
                guard self.state.withLock({ $0.wantsCapture }) else {
                    Self.logger.info("Permission granted, but capture no longer wanted — not starting")
                    return
                }
                Self.logger.info("Permission granted, starting capture")
                self.controlQueue.sync {
                    self.startCaptureLocked(sampleRate: sampleRate, channels: channels, codec: codec)
                }
            }
            return
        case .denied:
            Self.logger.warning("Microphone permission denied — cannot capture")
            return
        @unknown default:
            break
        }

        let inputNode = engine.inputNode
        let hwFormat = inputNode.inputFormat(forBus: 0)

        // 0 Hz / 0-channel means no input device (or one that vanished mid-configuration-change).
        // AVAudioConverter/installTap have no documented contract for that case — guard here instead
        // of relying on undocumented behavior (installTap with an invalid format is an uncatchable
        // ObjC exception, not a Swift error).
        guard hwFormat.sampleRate > 0, hwFormat.channelCount > 0 else {
            Self.logger.error("No usable input device (0 Hz / 0-channel format) — not starting capture")
            return
        }

        guard let targetFormat = AVAudioFormat(
            commonFormat: .pcmFormatInt16,
            sampleRate: sampleRate,
            channels: AVAudioChannelCount(channels),
            interleaved: true
        ) else {
            Self.logger.error("Failed to create target format")
            return
        }

        guard let conv = AVAudioConverter(from: hwFormat, to: targetFormat) else {
            Self.logger.error("Failed to create converter: hw=\(hwFormat.sampleRate, format: .fixed(precision: 0), privacy: .public)Hz/\(hwFormat.channelCount, privacy: .public)ch → target=\(sampleRate, format: .fixed(precision: 0), privacy: .public)Hz/\(channels, privacy: .public)ch")
            return
        }

        // Install tap at hardware format. macOS 27 deprecates the non-throwing installTap in
        // favor of the throwing installTapOnBus:bufferSize:format:error:block:. That method is
        // NS_REFINED_FOR_SWIFT and ships without a Swift overlay in this SDK, so the importer
        // surfaces it as the throwing `__installTap` — the NSError out-param becomes `throws`,
        // leaving a vestigial `error: ()` placeholder argument.
        do {
            // A2: capture the CONVERTER in the tap closure, freezing the (converter, format) pair.
            // Fetching the converter from mutable state inside processInput let a reconfigure pair
            // a NEW converter with an OLD-format buffer already in flight.
            // 4800 frames = 100 ms at 48 kHz, the floor of AVAudioNode's documented [100, 400] ms tap
            // range. A smaller request (the previous 1024) is silently clamped to the same 100 ms
            // delivery — measured on this hardware — so ask for what is actually delivered.
            try inputNode.__installTap(onBus: 0, bufferSize: 4800, format: hwFormat, error: ()) {
                [weak self] buffer, _ in
                self?.processInput(buffer: buffer, converter: conv)
            }
        } catch {
            Self.logger.error("Failed to install mic tap: \(error.localizedDescription, privacy: .public)")
            return
        }

        state.withLock { s in
            s.isCapturing = true
            s.targetRate = sampleRate
            s.targetChannels = channels
            s.targetCodec = codec
        }
        // Fresh pacing state per capture: a residual left from a 16 kHz Siri turn would be emitted as
        // the first bytes of an 8 kHz call frame, at the wrong rate and half a frame out of phase.
        // The encoder is fresh for the same reason twice over: the H2 sequence must restart at 0x08
        // for a new SCO channel, and a carried-over delay line would ring into its first 10 blocks.
        tx.withLock {
            $0 = UplinkTx()
            $0.msbc = codec == OCBM.seamCodecMsbc ? MSBCUplinkEncoder() : nil
        }

        do {
            try engine.start()
            Self.logger.info("Capture started: \(sampleRate, format: .fixed(precision: 0), privacy: .public)Hz \(channels, privacy: .public)ch")
            // The uplink side of the same event, in the vocabulary of the box's gate: what the CH_MIC
            // byte stream will now carry. 8000 Hz is the AA telephony (HFP/SCO) lane, 16000 the
            // CarPlay Siri/telephony one; the hardware may be running at 48 kHz either way — the
            // converter installed above resamples, so the input node is never asked for 8 kHz.
            if codec == OCBM.seamCodecMsbc {
                Self.logger.info("mic uplink armed \(sampleRate, format: .fixed(precision: 0), privacy: .public) Hz mSBC (\(channels, privacy: .public)ch, 60 B/7.5 ms eSCO packets, hw \(hwFormat.sampleRate, format: .fixed(precision: 0), privacy: .public) Hz)")
            } else {
                let fb = Self.uplinkFrameBytes(rate: sampleRate, channels: channels)
                Self.logger.info("mic uplink armed \(sampleRate, format: .fixed(precision: 0), privacy: .public) Hz (\(channels, privacy: .public)ch, \(fb, privacy: .public) B/20 ms frames, hw \(hwFormat.sampleRate, format: .fixed(precision: 0), privacy: .public) Hz)")
            }
        } catch {
            Self.logger.error("Failed to start capture: \(error.localizedDescription, privacy: .public)")
            inputNode.removeTap(onBus: 0)
            state.withLock { s in
                s.isCapturing = false
            }
        }
    }

    /// Must run on controlQueue.
    private func stopCaptureLocked() {
        let wasCapturing = state.withLock { s -> Bool in
            guard s.isCapturing else { return false }
            s.isCapturing = false
            return true
        }
        guard wasCapturing else { return }
        engine.inputNode.removeTap(onBus: 0)
        tx.withLock { $0 = UplinkTx() } // also drops the mSBC encoder, if one was armed
        engine.stop()
        Self.logger.info("Capture stopped")
    }

    // MARK: - Process

    private func processInput(buffer: AVAudioPCMBuffer, converter conv: AVAudioConverter) {
        // Stop-gate: a tap callback already in flight when stopCapture ran must not emit PCM.
        guard state.withLock({ $0.isCapturing }) else { return }
        // A2: the (converter, format) pair is frozen at tap install; a reconfigure tears the tap
        // down and installs a new pair. Drop a mismatched in-flight callback (old buffer format
        // against a stale converter captured by a not-yet-removed tap) rather than convert garbage.
        guard conv.inputFormat == buffer.format else { return }
        let targetFormat = conv.outputFormat

        let ratio = targetFormat.sampleRate / buffer.format.sampleRate
        // Headroom beyond the truncated estimate lets the converter drain the
        // fractional-frame remainder it buffers between callbacks; without it
        // the remainder accumulates and mic audio lags progressively.
        let outputFrameCount = AVAudioFrameCount(Double(buffer.frameLength) * ratio) + 64
        guard outputFrameCount > 0 else { return }

        guard let outputBuffer = AVAudioPCMBuffer(
            pcmFormat: targetFormat,
            frameCapacity: outputFrameCount
        ) else { return }

        var error: NSError?
        // Converter callback is synchronous — safe to capture mutable state
        nonisolated(unsafe) var inputConsumed = false
        nonisolated(unsafe) let inputBuffer = buffer

        conv.convert(to: outputBuffer, error: &error) { _, outStatus in
            if !inputConsumed {
                inputConsumed = true
                outStatus.pointee = .haveData
                return inputBuffer
            }
            outStatus.pointee = .noDataNow
            return nil
        }

        if let err = error {
            Self.logger.error("Conversion error: \(err.localizedDescription, privacy: .public)")
            return
        }

        guard outputBuffer.frameLength > 0 else { return }

        // Extract PCM16LE bytes
        let bytesPerFrame = Int(targetFormat.streamDescription.pointee.mBytesPerFrame)
        let byteCount = Int(outputBuffer.frameLength) * bytesPerFrame
        guard let channelData = outputBuffer.int16ChannelData?[0] else { return }

        let pcmData = Data(bytes: channelData, count: byteCount)
        emitUplink(pcmData, rate: targetFormat.sampleRate, channels: targetFormat.channelCount)
    }

    /// Cut the converted PCM into exact CH_MIC chunks, carrying the remainder, and account for the
    /// per-second telemetry. The box's HFP/SCO sink consumes 20 ms of CVSD at a time; handing it a
    /// 100 ms lump means the first frame of every lump is 80 ms late relative to a steady feed, which
    /// on a call is audible as choppiness rather than latency. Under mSBC the chunk is 7.5 ms because
    /// that is one air packet — the same argument, decided by the codec instead of by us.
    private func emitUplink(_ pcm: Data, rate: Double, channels: AVAudioChannelCount) {
        var out: [Data] = []
        var line: (n: Int, rms: Double)? = nil
        tx.withLock { t in
            // Frame size and encoder are read as one under the lock: pairing a 20 ms cut with the
            // mSBC encoder (or a 7.5 ms cut with none) would put malformed packets on the SCO socket.
            let frameBytes = Self.uplinkChunkBytes(rate: rate, channels: UInt32(channels),
                                                   codec: t.msbc == nil ? 0 : OCBM.seamCodecMsbc)
            t.residual.append(pcm)
            // RMS over the whole callback (not per frame): a silent uplink — muted input device, wrong
            // device selected, TCC granted but the mic capturing digital zero — is otherwise invisible.
            pcm.withUnsafeBytes { raw in
                let n = raw.count / 2
                for i in 0..<n { // loadUnaligned: Data gives no 2-byte alignment guarantee
                    let v = raw.loadUnaligned(fromByteOffset: i * 2, as: Int16.self)
                    t.sumSq += Double(v) * Double(v)
                }
                t.samples += n
            }
            while t.residual.count >= frameBytes {
                let frame = Data(t.residual.prefix(frameBytes))
                t.residual.removeFirst(frameBytes)
                t.frames += 1
                if let enc = t.msbc {
                    // 120 samples in, one 60-byte eSCO packet out. A nil return can only mean a
                    // wrong-sized frame, which `frameBytes` rules out — drop rather than hand the box
                    // a malformed packet it would write to the SCO socket verbatim.
                    if let pkt = enc.packet(from: frame) { out.append(pkt) }
                } else {
                    out.append(frame)
                }
            }
            let now = ProcessInfo.processInfo.systemUptime
            if t.lastLog == 0 {
                t.lastLog = now
            } else if now - t.lastLog >= 1.0 {
                t.lastLog = now
                let rms = t.samples > 0 ? (t.sumSq / Double(t.samples)).squareRoot() / 32768.0 : 0
                line = (t.frames, rms)
                t.frames = 0; t.sumSq = 0; t.samples = 0
            }
        }
        for chunk in out { onPCMData?(chunk) }
        if let l = line {
            Self.logger.info("mic tx=\(l.n, privacy: .public) frames rms=\(l.rms, format: .fixed(precision: 4), privacy: .public)")
        }
    }
}
