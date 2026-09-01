import Foundation
import AVFAudio
import os
import Synchronization

// MARK: - Audio Decode Types
//
// Relocated from the deleted Protocol/MessageTypes.swift: AudioPlayer is the only live user
// (it pre-warms one player node per rate×channel combo from `allCases`).
enum AudioDecodeType: UInt32, Sendable, CaseIterable {
    case media48kAlt = 1   // 48000Hz stereo (alternate media)
    case mediaCtrl   = 2   // 48000Hz stereo (stop/cleanup signals — carries commands, not PCM)
    case voice8k     = 3   // 8000Hz mono (AA phone calls, WebRTC AECM compatible)
    case media48k    = 4   // 48000Hz stereo (primary CarPlay media output)
    case voice16k    = 5   // 16000Hz mono (Siri/voice assistant)
    case voice24k    = 6   // 24000Hz mono (enhanced voice/alerts)
    case voice16kStereo = 7 // 16000Hz stereo

    var sampleRate: Double {
        switch self {
        case .media48kAlt: return 48000
        case .mediaCtrl:   return 48000
        case .voice8k:     return 8000
        case .media48k:    return 48000
        case .voice16k:    return 16000
        case .voice24k:    return 24000
        case .voice16kStereo: return 16000
        }
    }

    var channels: UInt32 {
        switch self {
        case .media48kAlt, .mediaCtrl, .media48k, .voice16kStereo: return 2
        case .voice8k, .voice16k, .voice24k:                       return 1
        }
    }
}

/// Dual-stream audio player (media + navigation) with volume ducking.
/// Uses AVAudioEngine with two AVAudioPlayerNode instances.
///
/// Threading: AVAudioEngine graph mutation (connect/disconnect/start/stop)
/// is not thread-safe, and this class is reached from three threads — the
/// audio decrypt queue (feedAudio→delegate, since the D1 decouple), the main actor (start/stop/commands), and
/// the NotificationCenter posting thread (configuration change). Every
/// engine-touching operation therefore runs on `engineQueue`; the Mutex
/// protects only the bookkeeping state.
final class AudioPlayer: @unchecked Sendable {

    private static let logger = Logger(subsystem: "com.carlink.audio", category: "Player")

    private let engine = AVAudioEngine()
    private let mediaPlayer = AVAudioPlayerNode()
    private let navPlayer = AVAudioPlayerNode()
    private let mediaMixer = AVAudioMixerNode()
    private let navMixer = AVAudioMixerNode()

    // MARK: All-rates pre-warm (user directive 2026-07-11)
    //
    // CarPlay wired streams are ALL PCM but at various rates (SDK catalog: 8/16/24/32/44.1/48 kHz,
    // 16-bit, mono/stereo; audioTypes media/telephony/speechRecognition/alert/default). Every known
    // rate×channel combo gets a DEDICATED player node, attached and connected at engine setup — so a
    // new stream (Siri chime at 16k mono, alert at 48k stereo…) starts with ZERO graph mutation:
    // pick the node, schedule the buffer. Media-role nodes (stereo 44.1/48k) hang off mediaMixer
    // (ducking applies); voice-role nodes hang off navMixer (they ARE the ducking trigger class).
    struct PCMKey: Hashable {
        let rate: Int
        let channels: Int
        let voice: Bool
    }
    static let wiredPCMRates = [8000, 16000, 24000, 32000, 44100, 48000]
    private var prewarmed: [PCMKey: AVAudioPlayerNode] = [:]
    /// One immutable AVAudioFormat per rate×ch (int16 interleaved — the wire layout after byteswap).
    private static let pcmFormatCache: [PCMKey: AVAudioFormat] = {
        var cache: [PCMKey: AVAudioFormat] = [:]
        for rate in wiredPCMRates {
            for ch in [1, 2] {
                for voice in [false, true] {
                    let k = PCMKey(rate: rate, channels: ch, voice: voice)
                    cache[k] = AVAudioFormat(
                        commonFormat: .pcmFormatInt16, sampleRate: Double(rate),
                        channels: AVAudioChannelCount(ch), interleaved: true)
                }
            }
        }
        return cache
    }()

    /// Serializes ALL engine/node/mixer access. Serial, so packet order from
    /// the single audio decrypt queue is preserved through the async hops.
    private let engineQueue = DispatchQueue(label: "audio.engine", qos: .userInitiated)

    // Thread safety for mutable state via Mutex
    private struct PlayerState {
        var currentMediaFormat: AVAudioFormat?
        var currentNavFormat: AVAudioFormat?
        var isStarted = false
        /// OCBM auto-duck: voice-stream playback ducks media; these track the
        /// quiet-period deadline and whether the restore watcher is alive.
        var isVoiceDucking = false
        var voiceDeadlineNs: UInt64 = 0
    }

    /// Nav/voice ducking level: media plays at this gain while voice audio is AUDIBLE. The SDK's
    /// `duckAudio` is "Delegating ducking of audio TO %f" — a target gain — and stock CarPlay ducks
    /// music to ~20% during nav prompts (the legacy app used 0.2 too), so 0.2 is the default here
    /// until task #19 wires the parameterized command through. (First cut used 0.8 — "duck by 20%" —
    /// which is a ~2 dB dip, empirically imperceptible: reported as "does not duck", 2026-07-12.)
    private static let voiceDuckLevel: Float = 0.2
    /// Peak-amplitude gate for the voice-activity duck: iOS keeps voice-class streams alive with
    /// CONTINUOUS SILENCE, so packet flow alone would duck media permanently (the 2026-07-12 bug —
    /// constant 0.8 from session start, no audible change on a real prompt). Only a buffer whose
    /// int16 peak clears this (≈ −32 dBFS; digital-silence streams sit ≲10) counts as voice activity.
    private static let voiceActivityPeak: Int32 = 800
    private let state = Mutex(PlayerState())

    /// Bounded playback backlog (docs/carplay/01_OCBM_PROTOCOL.md): every scheduled buffer increments its node's queued-duration
    /// counter; the `.dataConsumed` completion decrements it. A node already holding more than its cap
    /// DROPS the incoming packet — without a cap the player is a lossless unbounded queue and clock drift
    /// (~50 ppm) grows it ~4 ms/min. The cap is per NODE; the DEPTH splits by stream class, because
    /// CarPlay's delivery philosophy is not uniform (device-proven + Apple R14G17 AirPlayReceiverSession.c):
    ///   • VOICE / telephony (Siri, phone, alerts = Apple `MainAudio` 100 / `AltAudio` 101) is LOW-latency,
    ///     delivered ~realtime through a shallow jitter buffer (Apple: 32 ms wired / 80 ms Wi-Fi). 150 ms
    ///     keeps prompts tight and never overflows — arrival-paced is CORRECT here (these stay clean).
    ///   • MEDIA (Apple Music = Apple `MainHighAudio` 102) is HIGH-latency BY DESIGN: the iPhone delivers
    ///     it in BURSTS ahead of realtime (~750 ms shipped fast, then a ~200 ms pause), expecting the
    ///     receiver to hold a deep, primed playout buffer (Apple nominal `kAirPlayAudioBufferMainHighMs`
    ///     = 1000 ms; structures sized to ~4 s). Forcing it through the old uniform 150 ms cap starved the
    ///     node ~200 ms every ~750 ms — the media-audio stutter (Siri/telephony were clean; only Apple
    ///     Music chopped; 1062 underruns / 10 min on hardware).
    private static let voiceMaxQueuedSeconds = 0.150
    private static let mediaMaxQueuedSeconds = 1.0
    /// MEDIA pre-roll. A deep cap ALONE does nothing: an AVAudioPlayerNode drains as fast as it fills, so
    /// under net-realtime delivery the queue settles at ~0 and starves every burst-pause regardless of the
    /// cap (matches the every-cycle underrun observed). Apple primes the buffer before the first pull
    /// (AirPlayUtils.c:800-804); we mirror that — HOLD media buffers until this much is staged, then release
    /// in order. The queue then oscillates around the pre-roll depth (never ~0 → no underrun; never ~cap →
    /// no drop). This sets media latency + post-pause overhang ≈ this value; 400 ms covers the observed
    /// ≤250 ms burst deficit with margin and is imperceptible for music (no lip-sync to the real-time UI
    /// video, which stays immediate-drop). Voice is NEVER pre-rolled.
    private static let mediaPrerollSeconds = 0.4
    private struct BacklogState {
        /// Seconds of scheduled-but-not-yet-consumed audio, per player node.
        var queuedSeconds: [ObjectIdentifier: Double] = [:]
        /// Drops accumulated since the last drop log (throttled to ~1 line/s).
        var dropCount = 0
        var lastDropLogNs: UInt64 = 0
        /// A5 underrun signal: uptime-ns stamp when a node's queue last drained to 0 (set in the
        /// .dataConsumed completion). A packet arriving to a node that sat empty 2 ms..300 ms counts
        /// as an underrun (below 2 ms is normal back-to-back scheduling; above 300 ms the stream is
        /// legitimately idle/stopped, not starved).
        var emptySinceNs: [ObjectIdentifier: UInt64] = [:]
        /// Underruns accumulated since the last underrun log (same ~1 line/s throttle as drops).
        var underrunCount = 0
        var lastUnderrunLogNs: UInt64 = 0
        /// MEDIA pre-roll (see `mediaPrerollSeconds`): `mediaPrimed` marks a media node past the pre-roll
        /// threshold (schedule directly). It is the ONLY cross-thread pre-roll field — the completion
        /// handler clears it when the node drains (re-prime the deep buffer on the next stream); enqueue
        /// inserts it on crossing. The staging buffers themselves live in engineQueue-only plain properties
        /// (`mediaStaging`/`mediaStagedSeconds`) so the non-Sendable AVAudioPCMBuffers never cross this mutex.
        var mediaPrimed: Set<ObjectIdentifier> = []
    }
    private let backlog = Mutex(BacklogState())

    /// MEDIA pre-roll staging — buffers held per media node until `mediaPrerollSeconds` is accumulated,
    /// then released in order. Touched ONLY on engineQueue (feedPCM/enqueue + stop/config-change, all
    /// serialized there), so these are plain properties, NOT mutex-guarded — keeping the non-Sendable
    /// AVAudioPCMBuffers off the mutex boundary (region isolation). The cross-thread `mediaPrimed` flag
    /// lives in BacklogState. Voice nodes never appear here.
    private var mediaStaging: [ObjectIdentifier: [AVAudioPCMBuffer]] = [:]
    private var mediaStagedSeconds: [ObjectIdentifier: Double] = [:]
    /// Wall-clock (uptime-ns) when the current priming batch started staging, per media node. Drives the
    /// AGE-OUT (audit A3): a batch releases when it reaches `mediaPrerollSeconds` of audio OR has been
    /// held `mediaPrerollSeconds` of wall-clock — whichever first. Without the age-out, a media stream (or
    /// the tail after a re-prime) that delivers < 0.4 s and then slows/stops would be held in silence and
    /// eventually dropped unplayed; the age-out bounds the hold and flushes a slow trickle instead of
    /// stranding it. engineQueue-only, like the staging maps.
    private var mediaStageStartNs: [ObjectIdentifier: UInt64] = [:]

    /// A3: (rate, channels) combos outside the pre-warm matrix we have already complained about —
    /// the 48k/2ch fallback still plays (graceful degradation) but pitch-shifted, so say so once.
    private let offCatalogLogged = Mutex(Set<PCMKey>())

    /// One immutable AVAudioFormat per decode type — feedAudio runs per
    /// packet, and allocating a format (plus ObjC isEqual compares) per
    /// packet was measurable churn. AVAudioFormat is immutable after init.
    private static let formatCache: [UInt32: AVAudioFormat] = {
        var cache: [UInt32: AVAudioFormat] = [:]
        for decodeType in AudioDecodeType.allCases {
            cache[decodeType.rawValue] = AVAudioFormat(
                commonFormat: .pcmFormatInt16,
                sampleRate: decodeType.sampleRate,
                channels: AVAudioChannelCount(decodeType.channels),
                interleaved: true
            )!
        }
        return cache
    }()
    private static let fallbackFormat = AVAudioFormat(
        commonFormat: .pcmFormatInt16, sampleRate: 48000, channels: 2, interleaved: true
    )!

    private var configChangeObserver: NSObjectProtocol?

    /// Stream-performance monitor's shared anomaly log (set by AppDelegate at session setup, before any
    /// audio flows). AudioPlayer only computes the per-buffer numbers + underrun signal and hands them to
    /// this IOKit-free sink; all silence/click/clip decisions + debounce live there. nil = no monitor.
    var anomalyLog: AVAnomalyLog?

    init() {
        engineQueue.sync { setupEngine() }
        // The engine stops itself when the output device changes (headphones
        // plugged in, AirPods connect). Nobody restarts it otherwise: every
        // subsequent feedAudio would queue buffers onto a stopped node —
        // permanent silence plus unbounded memory growth.
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

    private func handleConfigurationChange() {
        engineQueue.async { [self] in
            let started = state.withLock { $0.isStarted }
            guard started else { return }
            Self.logger.info("Audio configuration changed — restarting engine")
            // The config change implicitly flushed every scheduled buffer (same as stop()); their
            // .dataConsumed handlers may or may not have run yet, so clear the backlog counters
            // here — a node left >150 ms "deep" would otherwise be permanently silenced (every new
            // packet dropped against a phantom backlog). Late decrements clamp at 0 in
            // scheduleBounded's handler instead of going negative.
            backlog.withLock {
                $0.queuedSeconds.removeAll()
                $0.emptySinceNs.removeAll()
                $0.mediaPrimed.removeAll()
            }
            mediaStaging.removeAll(); mediaStagedSeconds.removeAll(); mediaStageStartNs.removeAll()  // engineQueue-only
            // audit A3: a stop/restart landing mid-duck must not leave media at voiceDuckLevel until the
            // 1.5 s watcher expires — reset the duck state + mixer here so a fresh session starts at unity.
            state.withLock { $0.isVoiceDucking = false; $0.voiceDeadlineNs = 0 }
            mediaMixer.outputVolume = 1.0
            do {
                try engine.start()
                try mediaPlayer.playAudio()
                try navPlayer.playAudio()
                playPrewarmed()
            } catch {
                Self.logger.error("Engine restart after configuration change failed: \(error.localizedDescription, privacy: .public)")
            }
        }
    }

    // MARK: - Setup

    /// Must run on engineQueue.
    private func setupEngine() {
        engine.attach(mediaPlayer)
        engine.attach(navPlayer)
        engine.attach(mediaMixer)
        engine.attach(navMixer)

        // Default formats — will reconnect on first audio if different
        let defaultMedia = Self.formatCache[AudioDecodeType.media48k.rawValue] ?? Self.fallbackFormat
        let defaultNav = Self.formatCache[AudioDecodeType.voice16k.rawValue] ?? Self.fallbackFormat

        do {
            try engine.connectNode(mediaPlayer, to: mediaMixer, format: defaultMedia)
            try engine.connectNode(navPlayer, to: navMixer, format: defaultNav)
            try engine.connectNode(mediaMixer, to: engine.mainMixerNode, format: nil)
            try engine.connectNode(navMixer, to: engine.mainMixerNode, format: nil)
        } catch {
            Self.logger.error("Engine node connection failed: \(error.localizedDescription, privacy: .public)")
        }

        state.withLock {
            $0.currentMediaFormat = defaultMedia
            $0.currentNavFormat = defaultNav
        }

        // Pre-warm the all-rates PCM matrix: voice nodes for every rate×ch (Siri/telephony/alert can
        // negotiate any of them), media nodes for the stereo music rates. Attached + connected NOW so
        // stream start never mutates the graph. The engine mixes heterogeneous input rates natively.
        var keys: [PCMKey] = [
            PCMKey(rate: 44100, channels: 2, voice: false),
            PCMKey(rate: 48000, channels: 2, voice: false),
        ]
        for rate in Self.wiredPCMRates {
            for ch in [1, 2] {
                keys.append(PCMKey(rate: rate, channels: ch, voice: true))
            }
        }
        for k in keys {
            guard let fmt = Self.pcmFormatCache[k] else { continue }
            let node = AVAudioPlayerNode()
            engine.attach(node)
            do {
                try engine.connectNode(node, to: k.voice ? navMixer : mediaMixer, format: fmt)
                prewarmed[k] = node
            } catch {
                engine.detach(node)
                Self.logger.error("pre-warm \(k.rate)Hz/\(k.channels)ch voice=\(k.voice) connect failed: \(error.localizedDescription, privacy: .public)")
            }
        }
        Self.logger.info("pre-warmed \(self.prewarmed.count) PCM player nodes (all wired rates)")
    }

    /// Start every pre-warmed node (engine must be running). On engineQueue.
    private func playPrewarmed() {
        for node in prewarmed.values where !node.isPlaying {
            try? node.playAudio()
        }
    }

    func start() {
        let alreadyStarted = state.withLock { s -> Bool in
            if s.isStarted { return true }
            s.isStarted = true
            return false
        }
        guard !alreadyStarted else { return }
        engineQueue.sync { [self] in
            do {
                try engine.start()
                try mediaPlayer.playAudio()
                try navPlayer.playAudio()
                playPrewarmed()
                Self.logger.info("Engine started")
            } catch {
                state.withLock { $0.isStarted = false }
                Self.logger.error("Failed to start engine: \(error.localizedDescription, privacy: .public)")
            }
        }
    }

    func stop() {
        // Clear the flag first so packets and the config-change handler
        // already queued behind this block become no-ops.
        state.withLock { $0.isStarted = false }
        engineQueue.sync { [self] in
            mediaPlayer.stop()
            navPlayer.stop()
            for node in prewarmed.values { node.stop() }
            engine.stop()
            // stop() flushed every scheduled buffer; their .dataConsumed handlers may or may not
            // have run yet, so clear the backlog counters here — any late decrement clamps at 0
            // in scheduleBounded's handler instead of going negative.
            backlog.withLock {
                $0.queuedSeconds.removeAll()
                $0.emptySinceNs.removeAll()
                $0.mediaPrimed.removeAll()
            }
            mediaStaging.removeAll(); mediaStagedSeconds.removeAll(); mediaStageStartNs.removeAll()  // engineQueue-only
            // audit A3: a stop/restart landing mid-duck must not leave media at voiceDuckLevel until the
            // 1.5 s watcher expires — reset the duck state + mixer here so a fresh session starts at unity.
            state.withLock { $0.isVoiceDucking = false; $0.voiceDeadlineNs = 0 }
            mediaMixer.outputVolume = 1.0
        }
    }

    // MARK: - Feed Audio

    /// OCBM path: play decrypted MEDIA PCM (48 kHz / 16-bit / stereo interleaved LPCM). Legacy shim —
    /// streams with a box SEAM_FORMAT go through `feedPCM` at their actual rate.
    func feedMediaPCM(_ samples: Data) {
        feedPCM(samples, rate: 48000, channels: 2, voice: false, bigEndian: true)
    }

    /// All-rates OCBM path: schedule 16-bit interleaved PCM on the PRE-WARMED node for
    /// (rate, channels, voice) — zero graph mutation at stream start. Wired CarPlay PCM arrives
    /// BIG-ENDIAN (network order; playing it verbatim is white noise — see WIRED_AUDIO_ROOT_CAUSE);
    /// decoded wireless output is host-endian, so `bigEndian: false` skips the swap. A combo outside
    /// the pre-warm matrix (shouldn't happen — it spans the SDK's full wired catalog) falls back to
    /// the dynamic media/nav player with a one-time reconnect.
    func feedPCM(_ samples: Data, rate: Int, channels: Int, voice: Bool, bigEndian: Bool) {
        guard !samples.isEmpty else { return }
        engineQueue.async { [self] in
            guard state.withLock({ $0.isStarted }), engine.isRunning else { return }
            let key = PCMKey(rate: rate, channels: channels, voice: voice)
            if Self.pcmFormatCache[key] == nil {
                // A3: off-catalog combo — still play via the 48k/2ch fallback (graceful
                // degradation), but it will be pitch-shifted, so log it once per distinct combo.
                if offCatalogLogged.withLock({ $0.insert(key).inserted }) {
                    Self.logger.error("off-catalog PCM format \(rate)Hz/\(channels)ch voice=\(voice) — falling back to 48000Hz/2ch (audio will be pitch-shifted)")
                }
            }
            guard let format = Self.pcmFormatCache[key] ?? Self.pcmFormatCache[
                PCMKey(rate: 48000, channels: 2, voice: voice)] else { return }
            let bpf = Int(format.streamDescription.pointee.mBytesPerFrame)
            guard bpf > 0 else { return }
            let frameCount = AVAudioFrameCount(samples.count / bpf)
            guard frameCount > 0,
                  let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frameCount) else { return }
            buffer.frameLength = frameCount
            var peak: Int32 = 0 // computed in the same pass as the copy (voice activity gate)
            samples.withUnsafeBytes { src in
                guard let base = src.baseAddress, let d = buffer.int16ChannelData?[0] else { return }
                let count = min(samples.count, Int(frameCount) * bpf)
                let n16 = count / 2
                if bigEndian {
                    let s = base.assumingMemoryBound(to: UInt8.self)
                    for i in 0..<n16 {
                        let v = Int16(bitPattern: (UInt16(s[2 * i]) << 8) | UInt16(s[2 * i + 1]))
                        d[i] = v
                        let a = Int32(v).magnitude
                        if Int32(a) > peak { peak = Int32(a) }
                    }
                } else {
                    memcpy(d, base, count)
                    if voice {
                        for i in stride(from: 0, to: n16, by: 4) { // strided scan is plenty for a gate
                            let a = Int32(d[i]).magnitude
                            if Int32(a) > peak { peak = Int32(a) }
                        }
                    }
                }
            }
            // ── DSP tap (Option B): one extra single pass over the FINAL host-endian int16 samples this
            // buffer, feeding raw per-buffer numbers to the IOKit-free anomaly layer (silence/click/clip
            // decisions + debounce live there; here we only reduce the buffer to scalars — no per-sample
            // allocation). The app's PCM is uniformly int16 interleaved (wired byteswaps to int16; the
            // compressed decoder outputs int16), so this path is int16-only; AudioBufferDSP itself is
            // normalized-float so a future float path can feed the same detector. Energy/clip scan every
            // interleaved sample (channel-agnostic); the discontinuity scan walks CHANNEL 0 with a
            // per-frame stride so a stereo L/R interleave is never mistaken for a click.
            if let log = anomalyLog, let d = buffer.int16ChannelData?[0] {
                // Scan width from the ACTUAL allocated buffer, NOT the raw wire `channels` param
                // (gate 2 fix): an off-catalog `channels` falls back to the 48k/2ch format above, so the
                // buffer holds `frameCount * format.channelCount` int16s, not `frameCount * channels`.
                // For an interleaved int16 format bytes-per-frame = channelCount * 2, so realChannels = bpf/2.
                let realChannels = bpf / 2
                let n16 = Int(frameCount) * realChannels
                if n16 > 0 && realChannels > 0 {
                    let fs = Float(Int16.max)
                    let clipInt = Int32(fs * AudioDSP.clipLevel)
                    var sumSq = 0.0
                    var pk: Int32 = 0
                    var clipped = 0
                    var maxDelta: Int32 = 0
                    var prevCh0: Int32 = 0
                    var haveCh0 = false
                    var i = 0
                    while i < n16 {
                        let v = Int32(d[i])
                        let a = Int32(v.magnitude)
                        sumSq += Double(v) * Double(v)
                        if a >= clipInt { clipped += 1 }
                        if a > pk { pk = a }
                        if i % realChannels == 0 {   // channel-0 frame
                            if haveCh0 { let dd = Int32((v - prevCh0).magnitude); if dd > maxDelta { maxDelta = dd } }
                            prevCh0 = v; haveCh0 = true
                        }
                        i += 1
                    }
                    let lastIdx = max(0, (Int(frameCount) - 1) * realChannels)
                    let dsp = AudioBufferDSP(
                        frames: Int(frameCount), channels: realChannels, sampleRate: format.sampleRate,
                        rms: Float((sumSq / Double(n16)).squareRoot()) / fs,
                        peak: Float(pk) / fs,
                        clipFraction: Float(clipped) / Float(n16),
                        firstSample: Float(Int32(d[0])) / fs,
                        lastSample: Float(Int32(d[lastIdx])) / fs,
                        maxAbsDelta: Float(maxDelta) / fs,
                        voice: voice)
                    log.recordAudioBuffer(voice ? .voiceAudio : .mediaAudio, dsp,
                                          active: true, tMonoMs: AVAnomalyLog.monoMs(),
                                          tWall: Date().timeIntervalSince1970)
                }
            }
            // Voice ACTIVITY (not mere packet flow) ducks media: iOS streams silence on idle
            // voice-class streams, so gating on energy is what makes the duck actually modulate.
            if voice && peak >= Self.voiceActivityPeak { duckMediaForVoice() }
            let streamKind: StreamKind = voice ? .voiceAudio : .mediaAudio
            if let node = prewarmed[key] {
                if !node.isPlaying { try? node.playAudio() }
                enqueue(buffer, on: node, kind: streamKind)
            } else if voice {
                guard ensureFormat(format, for: navPlayer, mixer: navMixer,
                                   stored: \.currentNavFormat, label: "Voice") else { return }
                enqueue(buffer, on: navPlayer, kind: streamKind)
            } else {
                guard ensureFormat(format, for: mediaPlayer, mixer: mediaMixer,
                                   stored: \.currentMediaFormat, label: "Media") else { return }
                enqueue(buffer, on: mediaPlayer, kind: streamKind)
            }
        }
    }

    /// Route a decoded buffer to its node with the correct delivery discipline. VOICE/telephony schedules
    /// immediately (low-latency, arrival-paced — correct for that class). MEDIA goes through pre-roll
    /// staging: buffers are HELD until `mediaPrerollSeconds` is accumulated, then released in order, so the
    /// deep buffer is primed before the first packet plays (mirrors Apple's prime-before-pull; without it a
    /// deep cap still starves — the node drains as fast as it fills). Once a media node is primed, buffers
    /// schedule straight through until it drains (which re-primes). On engineQueue (feedPCM).
    private func enqueue(_ buffer: AVAudioPCMBuffer, on node: AVAudioPlayerNode, kind: StreamKind) {
        guard kind == .mediaAudio else { scheduleBounded(buffer, on: node, kind: kind); return }
        let id = ObjectIdentifier(node)
        if backlog.withLock({ $0.mediaPrimed.contains(id) }) {     // steady state → schedule directly
            scheduleBounded(buffer, on: node, kind: kind)
            return
        }
        // Priming (engineQueue-serialized): accumulate until the pre-roll DEPTH or the pre-roll wall-clock
        // AGE, whichever first (audit A3), then release in order.
        let now = DispatchTime.now().uptimeNanoseconds
        let seconds = Double(buffer.frameLength) / buffer.format.sampleRate
        if mediaStaging[id]?.isEmpty ?? true { mediaStageStartNs[id] = now } // first buffer of this batch
        mediaStaging[id, default: []].append(buffer)
        let stagedSec = (mediaStagedSeconds[id] ?? 0) + seconds
        mediaStagedSeconds[id] = stagedSec
        // Release when the batch holds enough audio (normal cold prime / re-prime) OR has been HELD for the
        // pre-roll wall-clock (age-out): the latter flushes a slow trickle or a sub-pre-roll tail instead of
        // stranding it in silence until the next stop()/config-change clear. The inherent one-time cushion-
        // rebuild after a genuine drain still costs up to a pre-roll of latency — a deep buffer cannot be
        // primed without it — but a partial/slow batch no longer hangs. Follow-up: flush on META_CMD pause.
        let heldNs = now &- (mediaStageStartNs[id] ?? now)
        let ageOut = heldNs >= UInt64(Self.mediaPrerollSeconds * 1_000_000_000)
        guard stagedSec >= Self.mediaPrerollSeconds || ageOut else { return } // still priming → held
        backlog.withLock { _ = $0.mediaPrimed.insert(id) }
        let batch = mediaStaging.removeValue(forKey: id) ?? []     // this buffer included, in arrival order
        mediaStagedSeconds[id] = nil
        mediaStageStartNs[id] = nil
        for buf in batch { scheduleBounded(buf, on: node, kind: kind) }
    }

    /// Schedule `buffer` on `node` only if the node's queued-but-unplayed audio is under its per-class cap
    /// (`voiceMaxQueuedSeconds` / `mediaMaxQueuedSeconds`); otherwise drop the packet (with a throttled log). The `.dataConsumed`
    /// completion handler fires on an internal AVFAudio thread — it does NOTHING but the
    /// mutex-guarded counter decrement (no engine access, no logging). The decrement clamps at 0
    /// because `stop()` clears the counters while the handlers of stop-flushed buffers may still
    /// be in flight.
    private func scheduleBounded(_ buffer: AVAudioPCMBuffer, on node: AVAudioPlayerNode,
                                 kind: StreamKind) {
        let seconds = Double(buffer.frameLength) / buffer.format.sampleRate
        let id = ObjectIdentifier(node)
        // Cap depth splits by class: media holds a deep primed buffer, voice/telephony stays shallow.
        let cap = kind == .voiceAudio ? Self.voiceMaxQueuedSeconds : Self.mediaMaxQueuedSeconds
        var dropsToLog: Int?
        var underrunsToLog: Int?
        var underrunDryMs: Double?   // set when THIS packet arrived to a node that had run dry (underrun)
        let admitted = backlog.withLock { b -> Bool in
            if (b.queuedSeconds[id] ?? 0) >= cap {
                b.dropCount += 1
                let now = DispatchTime.now().uptimeNanoseconds
                if now &- b.lastDropLogNs >= 1_000_000_000 {
                    b.lastDropLogNs = now
                    dropsToLog = b.dropCount
                    b.dropCount = 0
                }
                return false
            }
            // A5: the node's queue was empty when this packet arrived — if it sat dry for
            // 2 ms..300 ms, the output rendered silence mid-stream (an underrun). Cheap: one
            // dict removal per packet, throttled log like drops.
            let now = DispatchTime.now().uptimeNanoseconds
            if let emptyAt = b.emptySinceNs.removeValue(forKey: id) {
                let dryNs = now &- emptyAt
                if dryNs >= 2_000_000 && dryNs <= 300_000_000 {
                    b.underrunCount += 1
                    underrunDryMs = Double(dryNs) / 1_000_000.0   // emit an anomaly event (debounced below)
                    if now &- b.lastUnderrunLogNs >= 1_000_000_000 {
                        b.lastUnderrunLogNs = now
                        underrunsToLog = b.underrunCount
                        b.underrunCount = 0
                    }
                }
            }
            b.queuedSeconds[id, default: 0] += seconds
            return true
        }
        if let n = dropsToLog { // logged OUTSIDE the lock
            Self.logger.warning("playback backlog over \(Int(cap * 1000))ms — dropped \(n) packet(s)")
        }
        if let n = underrunsToLog { // logged OUTSIDE the lock
            Self.logger.warning("playback underrun — node ran dry between packets \(n) time(s)")
        }
        if let dryMs = underrunDryMs { // anomaly event (its own debounce), OUTSIDE the lock
            anomalyLog?.record(.underrun, stream: kind, detail: "\(Int(dryMs)) ms dry",
                               tMonoMs: AVAnomalyLog.monoMs(), tWall: Date().timeIntervalSince1970)
        }
        guard admitted else { return }
        node.scheduleBuffer(buffer, completionCallbackType: .dataConsumed) { [weak self] _ in
            self?.backlog.withLock { b in
                let remaining = max(0, (b.queuedSeconds[id] ?? 0) - seconds)
                b.queuedSeconds[id] = remaining
                // < 1 ms ≈ drained (FP add/sub of the same values can leave a tiny residue).
                if remaining < 0.001 {
                    b.emptySinceNs[id] = DispatchTime.now().uptimeNanoseconds
                    // Media drained → drop its primed flag so the next stream re-primes the deep buffer.
                    // If the re-fill takes > 300 ms the gap reads as idle (no underrun event); if the burst
                    // re-primes faster it may log a TRUE-POSITIVE underrun — the node really did run dry.
                    b.mediaPrimed.remove(id)
                }
            }
        }
    }

    // MARK: - Voice auto-duck (OCBM path)

    /// Duck media to `voiceDuckLevel` while voice-class audio (nav prompts, alerts…) is playing, and
    /// restore ~1.5 s after the last voice buffer. Called per voice buffer from feedPCM (engineQueue):
    /// each call pushes the quiet-period deadline out; a single watcher task restores when it passes.
    /// The legacy `.naviStart`/`.naviStop` command duck never fires in OCBM mode — stream activity IS
    /// the signal here.
    private func duckMediaForVoice() {
        let deadline = DispatchTime.now().uptimeNanoseconds + 1_500_000_000
        let spawnWatcher = state.withLock { s -> Bool in
            s.voiceDeadlineNs = deadline
            if s.isVoiceDucking { return false }
            s.isVoiceDucking = true
            return true
        }
        // Already on engineQueue (feedPCM) — set the mixer directly. UNCONDITIONALLY on every
        // gated voice buffer, not only when spawning the watcher: the write is idempotent, and
        // re-asserting the duck at buffer cadence (~20 ms) means a restore that slips past the
        // watcher's on-queue re-check (below) self-heals within one buffer. This is the second
        // half of the duck/unduck race fix; the first half is in the watcher's expired branch.
        mediaMixer.outputVolume = Self.voiceDuckLevel
        guard spawnWatcher else { return }
        Self.logger.info("voice active — ducking media to \(Self.voiceDuckLevel, format: .fixed(precision: 2))")
        Task { [weak self] in
            while true {
                guard let self else { return }
                // Re-check the deadline and clear the ducking flag ATOMICALLY. Reading the
                // deadline and clearing isVoiceDucking in two separate locked steps let a fresh
                // voice buffer push the deadline out (and skip spawning a watcher because
                // isVoiceDucking was still true) in between — the watcher would then un-duck
                // while voice was still active, and nothing would re-duck until the next buffer.
                // (This closes the FLAG race only; the volume-WRITE race is closed below.)
                let expired = self.state.withLock { s -> Bool in
                    if DispatchTime.now().uptimeNanoseconds < s.voiceDeadlineNs { return false }
                    s.isVoiceDucking = false
                    return true
                }
                if expired {
                    // The volume WRITE must be guarded too, not just the flag: between the atomic
                    // expiry above and the async block below actually running, a fresh voice
                    // buffer can start a NEW duck (set isVoiceDucking, write 0.2 on engineQueue).
                    // An unguarded 1.0 would then land AFTER that duck — media snaps loud under an
                    // active prompt, and the new watcher never re-writes 0.2 on its own. So hop to
                    // engineQueue and RE-CHECK the flag there before restoring. engineQueue is
                    // serial: either the new duck ran first (flag set → this restore is a no-op)
                    // or it runs after (its 0.2 overwrites our 1.0). Either way the duck wins;
                    // the per-buffer re-assert in duckMediaForVoice covers the remainder.
                    self.engineQueue.async {
                        guard self.state.withLock({ !$0.isVoiceDucking }) else { return }
                        self.mediaMixer.outputVolume = 1.0
                        Self.logger.info("voice quiet 1.5s — media volume restored")
                    }
                    return
                }
                // Sleep until the (possibly newly-extended) deadline. Guard the subtraction:
                // these are UInt64, so if `now` has advanced past the deadline in this window we
                // must not wrap — just loop and let the next atomic check expire it.
                let now = DispatchTime.now().uptimeNanoseconds
                let target = self.state.withLock { $0.voiceDeadlineNs }
                if target > now {
                    try? await Task.sleep(nanoseconds: target - now)
                }
            }
        }
    }

    // MARK: - Format Switching

    /// Reconnects the player at the new format (on engineQueue). The stored
    /// format is updated only after the reconnect succeeds; on failure it is
    /// CLEARED so the next packet retries the connect even if it carries the
    /// previous format — otherwise a failed A→B switch followed by format A
    /// would schedule onto a disconnected node (Objective-C exception).
    /// Also re-plays a node the engine left stopped, so buffers can't
    /// accumulate unboundedly on a non-playing node.
    private func ensureFormat(_ format: AVAudioFormat,
                              for player: AVAudioPlayerNode,
                              mixer: AVAudioMixerNode,
                              stored: WritableKeyPath<PlayerState, AVAudioFormat?>,
                              label: String) -> Bool {
        let changed = state.withLock { $0[keyPath: stored] !== format && $0[keyPath: stored] != format }
        if changed {
            engine.disconnectNodeOutput(player)
            do {
                try engine.connectNode(player, to: mixer, format: format)
            } catch {
                Self.logger.error("\(label, privacy: .public) format reconnect failed: \(error.localizedDescription, privacy: .public)")
                state.withLock { $0[keyPath: stored] = nil }
                return false
            }
            state.withLock { $0[keyPath: stored] = format }
            Self.logger.info("\(label, privacy: .public) format changed: \(format.sampleRate, format: .fixed(precision: 0), privacy: .public)Hz \(format.channelCount, privacy: .public)ch")
        }
        if !player.isPlaying {
            try? player.playAudio()
            guard player.isPlaying else { return false }
        }
        return true
    }

}

// MARK: - Wireless-format prestage (user directive 2026-07-11)

/// Decoder for the NON-PCM formats wireless CarPlay negotiates (AAC-LC / AAC-ELD / OPUS — the SDK
/// audioFormat catalog; wired is always PCM so this class is dormant on the wired transport). One
/// instance per stream, created the moment the box's SEAM_FORMAT announces a compressed codec — so
/// the converter is warmed BEFORE the first access unit arrives. Output is host-endian 16-bit
/// interleaved PCM at the stream's rate/channels, fed to the pre-warmed node via
/// `AudioPlayer.feedPCM(..., bigEndian: false)`.
///
/// PRESTAGED, NOT YET LIVE-VALIDATED: wireless isn't testable on this bench yet; decode errors log
/// and drop the AU rather than crash. AAC-LC/ELD/Opus are all native AudioToolbox codecs on macOS.
final class CompressedAudioDecoder {
    private static let logger = Logger(subsystem: "com.carlink.audio", category: "CompressedDecoder")
    private let converter: AVAudioConverter
    private let inFormat: AVAudioFormat
    let outFormat: AVAudioFormat
    let sampleRate: Double
    let channels: UInt32
    private var maxPacketSize: Int

    /// codec: 1 = AAC-LC, 2 = AAC-ELD, 3 = OPUS (ocbm-proto ACODEC_*). Returns nil for PCM/unknown.
    init?(codec: UInt8, sampleRate: Double, channels: UInt32) {
        var desc = AudioStreamBasicDescription()
        desc.mSampleRate = sampleRate
        desc.mChannelsPerFrame = channels
        switch codec {
        case 1:
            desc.mFormatID = kAudioFormatMPEG4AAC
            desc.mFramesPerPacket = 1024
        case 2:
            desc.mFormatID = kAudioFormatMPEG4AAC_ELD
            desc.mFramesPerPacket = 480
        case 3:
            desc.mFormatID = kAudioFormatOpus
            desc.mFramesPerPacket = 960
        default:
            return nil
        }
        guard let inF = AVAudioFormat(streamDescription: &desc),
              let outF = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: sampleRate,
                                       channels: AVAudioChannelCount(channels), interleaved: true),
              let conv = AVAudioConverter(from: inF, to: outF) else {
            Self.logger.error("converter init failed (codec=\(codec) \(sampleRate, format: .fixed(precision: 0))Hz \(channels)ch)")
            return nil
        }
        inFormat = inF
        outFormat = outF
        converter = conv
        self.sampleRate = sampleRate
        self.channels = channels
        maxPacketSize = 8192
        Self.logger.info("prestaged decoder codec=\(codec) \(sampleRate, format: .fixed(precision: 0))Hz \(channels)ch")
    }

    /// Decode one compressed access unit → host-endian interleaved Int16 PCM bytes (nil on failure).
    func decode(_ au: Data) -> Data? {
        guard !au.isEmpty else { return nil }
        if au.count > maxPacketSize { maxPacketSize = au.count }
        let inBuf = AVAudioCompressedBuffer(
            format: inFormat, packetCapacity: 1, maximumPacketSize: maxPacketSize)
        au.withUnsafeBytes { src in
            if let base = src.baseAddress {
                memcpy(inBuf.data, base, au.count)
            }
        }
        inBuf.byteLength = UInt32(au.count)
        inBuf.packetCount = 1
        inBuf.packetDescriptions?.pointee = AudioStreamPacketDescription(
            mStartOffset: 0, mVariableFramesInPacket: 0, mDataByteSize: UInt32(au.count))

        let capacity = AVAudioFrameCount(inFormat.streamDescription.pointee.mFramesPerPacket * 4)
        guard let outBuf = AVAudioPCMBuffer(pcmFormat: outFormat, frameCapacity: max(capacity, 4096)) else {
            return nil
        }
        var fed = false
        var err: NSError?
        let status = converter.convert(to: outBuf, error: &err) { _, outStatus in
            if fed {
                outStatus.pointee = .noDataNow
                return nil
            }
            fed = true
            outStatus.pointee = .haveData
            return inBuf
        }
        guard status != .error, err == nil, outBuf.frameLength > 0,
              let ch0 = outBuf.int16ChannelData?[0] else {
            Self.logger.error("decode failed: \(err?.localizedDescription ?? "no output", privacy: .public)")
            return nil
        }
        let byteCount = Int(outBuf.frameLength) * Int(outFormat.streamDescription.pointee.mBytesPerFrame)
        return Data(bytes: ch0, count: byteCount)
    }
}
