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

    /// Called with PCM16LE (little-endian) data ready to ship to the box over CH_MIC.
    var onPCMData: ((Data) -> Void)?

    private let engine = AVAudioEngine()
    private struct CaptureState {
        var isCapturing = false
        /// The active capture format — the box-negotiated rate/channels from the uplink gate.
        var targetRate: Double = 16000
        var targetChannels: UInt32 = 1
        /// Latest caller intent: true after startCapture, false after
        /// stopCapture. The deferred permission-grant retry and the
        /// configuration-change restart both abort if intent flipped while
        /// they were waiting — otherwise a TCC dialog answered after the
        /// Siri session ended would leave a hot mic no stop call can reach.
        var wantsCapture = false
    }
    private let state = Mutex(CaptureState())

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
    /// kHz for wired LPCM); `channels` is ~always 1 for voice input.
    func startCapture(sampleRate: Double, channels: UInt32) {
        state.withLock { $0.wantsCapture = true }
        controlQueue.sync { [self] in
            startCaptureLocked(sampleRate: sampleRate, channels: max(1, channels))
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
            let (capturing, rate, channels, wanted) = state.withLock {
                ($0.isCapturing, $0.targetRate, $0.targetChannels, $0.wantsCapture)
            }
            guard capturing, wanted else { return }
            Self.logger.info("Audio configuration changed — rebuilding mic capture")
            stopCaptureLocked()
            startCaptureLocked(sampleRate: rate, channels: channels)
        }
    }

    /// Must run on controlQueue.
    private func startCaptureLocked(sampleRate: Double, channels: UInt32) {
        // Re-check intent HERE, inside the serialized section. The deferred
        // permission-grant task checks wantsCapture before hopping onto the
        // control queue, but a stopCapture can land in between — without this
        // re-check that interleaving still produced an unstoppable hot mic.
        guard state.withLock({ $0.wantsCapture }) else {
            Self.logger.info("Capture no longer wanted — not starting")
            return
        }
        let current = state.withLock { ($0.isCapturing, $0.targetRate, $0.targetChannels) }
        if current.0 {
            if current.1 == sampleRate && current.2 == channels { return }
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
                    self.startCaptureLocked(sampleRate: sampleRate, channels: channels)
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
            try inputNode.__installTap(onBus: 0, bufferSize: 1024, format: hwFormat, error: ()) {
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
        }

        do {
            try engine.start()
            Self.logger.info("Capture started: \(sampleRate, format: .fixed(precision: 0), privacy: .public)Hz \(channels, privacy: .public)ch")
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
        onPCMData?(pcmData)
    }
}
