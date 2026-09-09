import Foundation
import AVFAudio
import os
import Synchronization

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
        /// L1: was a bare unsynchronized `var`, written from the main actor (AppDelegate at session
        /// setup) and read from `engineQueue` (`feedPCM`, `scheduleBounded`). Moved into the Mutex
        /// alongside the rest of the bookkeeping state instead of an injected `let`, so the existing
        /// `audio.anomalyLog = ...` call site in AppDelegate.swift (outside this file's ownership)
        /// keeps compiling unchanged.
        var anomalyLog: AVAnomalyLog?
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
    /// 2026-09-04: 150 → 250 ms. Android Auto guidance/Assistant at 48 kHz arrives as 4096 B packets
    /// of 42.7 ms (128 ms at the old 16 kHz declaration), and the phone opens each prompt with a
    /// burst of three; at 150 ms that burst dropped the two newest packets on every AUDIO START
    /// (measured: "backlog over 150ms — dropped 2 packet(s)" at each prompt). CarPlay Siri and
    /// telephony never queue past ~100 ms, so they keep their arrival-paced behaviour.
    private static let voiceMaxQueuedSeconds = 0.250
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
    /// TELEPHONY pre-roll (AA HFP calls over the plain seam, 2026-09-04): the box forwards each 20 ms SCO
    /// frame as it lands, so arrival-paced scheduling ran the 8 kHz voice node dry once every ~3 s
    /// (measured: one-packet "playback underrun" at that cadence on the first live call). Three frames
    /// of cushion absorbs the SCO/USB jitter and is still well under the HFP round-trip budget. Voice
    /// audio from CarPlay keeps its zero-cushion, arrival-paced delivery (that class was clean).
    static let telephonyPrerollSeconds = 0.06
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
        /// L2: bumped by `stop()`/`handleConfigurationChange()`. A `.dataConsumed` handler captures the
        /// generation at schedule time; if it differs when the handler fires, every buffer flushed by
        /// that stop/restart is stale and must not re-arm `emptySinceNs` — otherwise a start() + first
        /// packet within 300 ms logs a false "playback underrun" against a node that never actually ran.
        var generation: UInt64 = 0
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
    /// L3: age-out timer per media node, armed when its priming batch starts, so a batch that never
    /// reaches `mediaPrerollSeconds` of staged audio (packets slow down or stop mid-prime) still
    /// releases at the wall-clock age-out instead of waiting for the next packet arrival to check it.
    /// engineQueue-only, like the staging maps.
    private var mediaAgeOutTimers: [ObjectIdentifier: DispatchSourceTimer] = [:]

    /// A3: (rate, channels) combos outside the pre-warm matrix we have already complained about —
    /// the 48k/2ch fallback still plays (graceful degradation) but pitch-shifted, so say so once.
    private let offCatalogLogged = Mutex(Set<PCMKey>())

    private var configChangeObserver: NSObjectProtocol?

    /// Stream-performance monitor's shared anomaly log (set by AppDelegate at session setup, before any
    /// audio flows). AudioPlayer only computes the per-buffer numbers + underrun signal and hands them to
    /// this IOKit-free sink; all silence/click/clip decisions + debounce live there. nil = no monitor.
    var anomalyLog: AVAnomalyLog? {
        get { state.withLock { $0.anomalyLog } }
        set { state.withLock { $0.anomalyLog = newValue } }
    }

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
                $0.generation &+= 1   // L2: invalidate in-flight .dataConsumed handlers from before this restart
            }
            mediaStaging.removeAll(); mediaStagedSeconds.removeAll(); mediaStageStartNs.removeAll()  // engineQueue-only
            for t in mediaAgeOutTimers.values { t.cancel() }; mediaAgeOutTimers.removeAll()  // L3
            // audit A3: a stop/restart landing mid-duck must not leave media at voiceDuckLevel until the
            // 1.5 s watcher expires — reset the duck state + mixer here so a fresh session starts at unity.
            state.withLock { $0.isVoiceDucking = false; $0.voiceDeadlineNs = 0 }
            mediaMixer.outputVolume = 1.0
            do {
                try engine.start()
                try mediaPlayer.playAudio()
                try navPlayer.playAudio()
                playPrewarmed(force: true)
            } catch {
                Self.logger.error("Engine restart after configuration change failed: \(error.localizedDescription, privacy: .public)")
                // L4: leave the door open for recovery instead of a terminal state — one bounded retry
                // after 500 ms (transient output-device absence). A further failure just logs again on
                // the next configuration-change notification, same as today.
                self.engineQueue.asyncAfter(deadline: .now() + 0.5) { [weak self] in
                    guard let self, self.state.withLock({ $0.isStarted }) else { return }
                    do {
                        try self.engine.start()
                        try self.mediaPlayer.playAudio()
                        try self.navPlayer.playAudio()
                        self.playPrewarmed(force: true)
                        Self.logger.info("Engine restart retry succeeded")
                    } catch {
                        Self.logger.error("Engine restart retry failed: \(error.localizedDescription, privacy: .public)")
                    }
                }
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
        // L6: defaults come straight from pcmFormatCache (48k/2ch media, 16k/1ch voice) — both keys are
        // guaranteed present (rates/channels drawn from the same catalog that builds the cache below),
        // so the dead separate formatCache/fallbackFormat pair is gone.
        let defaultMedia = Self.pcmFormatCache[PCMKey(rate: 48000, channels: 2, voice: false)]!
        let defaultNav = Self.pcmFormatCache[PCMKey(rate: 16000, channels: 1, voice: true)]!

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
    private func playPrewarmed(force: Bool = false) {
        // `force` (engine restart after a configuration change, 2026-09-04): a player node keeps
        // reporting isPlaying == true across an AVAudioEngineConfigurationChange although it no
        // longer renders, so the `!isPlaying` filter skipped every pre-warmed node and the AA media
        // and guidance lanes went silent — each packet then hit the backlog cap and was dropped
        // for the rest of the session. Stop and re-play unconditionally, and log a play failure
        // instead of swallowing it.
        for (key, node) in prewarmed where force || !node.isPlaying {
            if force { node.stop() }
            do { try node.playAudio() } catch {
                Self.logger.error("pre-warmed node \(key.rate)Hz/\(key.channels)ch voice=\(key.voice) play failed: \(error.localizedDescription, privacy: .public)")
            }
        }
    }

    /// Watchdog (2026-09-04): a node whose queue sits at its cap while nothing is consumed is dead —
    /// its `.dataConsumed` handlers will never run, so every later packet is dropped. Called from
    /// `scheduleBounded` on each drop; after `stalledDropsBeforeRestart` consecutive drops on the
    /// same node with no decrement in between, stop and re-play that node and clear its counter.
    private static let stalledDropsBeforeRestart = 48   // ~2 s of media packets at 24/s
    private var stalledDrops: [ObjectIdentifier: Int] = [:]
    private func noteDropForWatchdog(node: AVAudioPlayerNode, id: ObjectIdentifier) {
        let n = (stalledDrops[id] ?? 0) + 1
        stalledDrops[id] = n
        guard n >= Self.stalledDropsBeforeRestart else { return }
        stalledDrops[id] = 0
        backlog.withLock {
            $0.queuedSeconds[id] = 0
            $0.emptySinceNs.removeValue(forKey: id)
            $0.mediaPrimed.remove(id)
            $0.generation &+= 1
        }
        node.stop()
        do {
            if !engine.isRunning { try engine.start() }
            try node.playAudio()
            Self.logger.warning("audio node stalled at its backlog cap with nothing consumed — restarted the node")
        } catch {
            Self.logger.error("audio node stall recovery failed: \(error.localizedDescription, privacy: .public)")
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
                $0.generation &+= 1   // L2: invalidate in-flight .dataConsumed handlers from before stop()
            }
            mediaStaging.removeAll(); mediaStagedSeconds.removeAll(); mediaStageStartNs.removeAll()  // engineQueue-only
            for t in mediaAgeOutTimers.values { t.cancel() }; mediaAgeOutTimers.removeAll()  // L3
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
    func feedPCM(_ samples: Data, rate: Int, channels: Int, voice: Bool, bigEndian: Bool,
                 preroll: Double = 0) {
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
                                          tMonoMs: AVAnomalyLog.monoMs(),
                                          tWall: Date().timeIntervalSince1970)
                }
            }
            // Voice ACTIVITY (not mere packet flow) ducks media: iOS streams silence on idle
            // voice-class streams, so gating on energy is what makes the duck actually modulate.
            if voice && peak >= Self.voiceActivityPeak { duckMediaForVoice() }
            let streamKind: StreamKind = voice ? .voiceAudio : .mediaAudio
            if let node = prewarmed[key] {
                if !node.isPlaying { try? node.playAudio() }
                enqueue(buffer, on: node, kind: streamKind, preroll: preroll)
            } else if voice {
                guard ensureFormat(format, for: navPlayer, mixer: navMixer,
                                   stored: \.currentNavFormat, label: "Voice") else { return }
                enqueue(buffer, on: navPlayer, kind: streamKind, preroll: preroll)
            } else {
                guard ensureFormat(format, for: mediaPlayer, mixer: mediaMixer,
                                   stored: \.currentMediaFormat, label: "Media") else { return }
                enqueue(buffer, on: mediaPlayer, kind: streamKind, preroll: preroll)
            }
        }
    }

    /// Route a decoded buffer to its node with the correct delivery discipline. VOICE/telephony schedules
    /// immediately (low-latency, arrival-paced — correct for that class). MEDIA goes through pre-roll
    /// staging: buffers are HELD until `mediaPrerollSeconds` is accumulated, then released in order, so the
    /// deep buffer is primed before the first packet plays (mirrors Apple's prime-before-pull; without it a
    /// deep cap still starves — the node drains as fast as it fills). Once a media node is primed, buffers
    /// schedule straight through until it drains (which re-primes). On engineQueue (feedPCM).
    /// `preroll` is the caller-requested cushion for a VOICE-class stream (0 = arrival-paced; the
    /// telephony lane passes `telephonyPrerollSeconds`); media always uses `mediaPrerollSeconds`.
    private func enqueue(_ buffer: AVAudioPCMBuffer, on node: AVAudioPlayerNode, kind: StreamKind,
                         preroll: Double = 0) {
        let preroll = kind == .mediaAudio ? Self.mediaPrerollSeconds : preroll
        guard preroll > 0 else { scheduleBounded(buffer, on: node, kind: kind); return }
        let id = ObjectIdentifier(node)
        if backlog.withLock({ $0.mediaPrimed.contains(id) }) {     // steady state → schedule directly
            scheduleBounded(buffer, on: node, kind: kind)
            return
        }
        // Priming (engineQueue-serialized): accumulate until the pre-roll DEPTH or the pre-roll wall-clock
        // AGE, whichever first (audit A3), then release in order.
        let now = DispatchTime.now().uptimeNanoseconds
        let seconds = Double(buffer.frameLength) / buffer.format.sampleRate
        if mediaStaging[id]?.isEmpty ?? true {
            mediaStageStartNs[id] = now // first buffer of this batch
            armAgeOutTimer(id: id, node: node, kind: kind, preroll: preroll)
        }
        mediaStaging[id, default: []].append(buffer)
        let stagedSec = (mediaStagedSeconds[id] ?? 0) + seconds
        mediaStagedSeconds[id] = stagedSec
        // Release when the batch holds enough audio (normal cold prime / re-prime) OR has been HELD for the
        // pre-roll wall-clock (age-out): the latter flushes a slow trickle or a sub-pre-roll tail instead of
        // stranding it in silence until the next stop()/config-change clear. The inherent one-time cushion-
        // rebuild after a genuine drain still costs up to a pre-roll of latency — a deep buffer cannot be
        // primed without it — but a partial/slow batch no longer hangs. L3: the timer armed above covers
        // the case where packets stop arriving mid-batch (this arrival-time check alone never fires then).
        let heldNs = now &- (mediaStageStartNs[id] ?? now)
        let ageOut = heldNs >= UInt64(preroll * 1_000_000_000)
        guard stagedSec >= preroll || ageOut else { return } // still priming → held
        releaseMediaBatch(id: id, node: node, kind: kind)
    }

    /// Release a media node's staged priming batch (this buffer included, arrival order) and mark it
    /// primed. Shared by the packet-arrival path and the age-out timer (L3). engineQueue-only.
    private func releaseMediaBatch(id: ObjectIdentifier, node: AVAudioPlayerNode, kind: StreamKind) {
        mediaAgeOutTimers.removeValue(forKey: id)?.cancel()
        backlog.withLock { _ = $0.mediaPrimed.insert(id) }
        let batch = mediaStaging.removeValue(forKey: id) ?? []
        mediaStagedSeconds[id] = nil
        mediaStageStartNs[id] = nil
        for buf in batch { scheduleBounded(buf, on: node, kind: kind) }
    }

    /// Arm (replacing any prior timer for this node) a one-shot age-out on `engineQueue` for a fresh
    /// priming batch. Fires only if the batch is still pending when it goes off — a normal depth-based
    /// release, `stop()`, or a config-change restart cancels/clears it first.
    private func armAgeOutTimer(id: ObjectIdentifier, node: AVAudioPlayerNode, kind: StreamKind,
                                preroll: Double) {
        mediaAgeOutTimers.removeValue(forKey: id)?.cancel()
        let timer = DispatchSource.makeTimerSource(queue: engineQueue)
        timer.schedule(deadline: .now() + preroll)
        timer.setEventHandler { [weak self] in
            guard let self, self.mediaStaging[id]?.isEmpty == false else { return }
            self.releaseMediaBatch(id: id, node: node, kind: kind)
        }
        mediaAgeOutTimers[id] = timer
        timer.resume()
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
        var scheduleGeneration: UInt64 = 0
        let admitted = backlog.withLock { b -> Bool in
            scheduleGeneration = b.generation
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
        if admitted { stalledDrops[id] = 0 } else { noteDropForWatchdog(node: node, id: id) }
        if let n = underrunsToLog { // logged OUTSIDE the lock
            Self.logger.warning("playback underrun — node ran dry between packets \(n) time(s)")
        }
        if let dryMs = underrunDryMs { // anomaly event (its own debounce), OUTSIDE the lock
            anomalyLog?.record(.underrun, stream: kind, detail: "\(Int(dryMs)) ms dry",
                               tMonoMs: AVAnomalyLog.monoMs(), tWall: Date().timeIntervalSince1970)
        }
        guard admitted else { return }
        node.scheduleBuffer(buffer, completionCallbackType: .dataConsumed) { [weak self, scheduleGeneration] _ in
            self?.backlog.withLock { b in
                // L2: a stop()/config-change bumped `generation` after this buffer was scheduled but
                // before its completion fired — it was flushed, not actually consumed, so its decrement
                // and (especially) its emptySinceNs re-arm would be against a node that never ran; the
                // counters were already cleared wholesale by that stop/restart. Drop it.
                guard b.generation == scheduleGeneration else { return }
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
        // L7: converter callback is synchronous — safe to capture mutable state (mirrors
        // MicCapture.processInput's identical pattern).
        nonisolated(unsafe) var fed = false
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
