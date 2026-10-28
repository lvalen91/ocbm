package zeno.gmccpa.av

/**
 * Plan v5.8 V1/V2: the real-time voice constants and the NAV_DURING_VOICE switch. Pure (no Android).
 *
 * Owner direction (2026-10-07): voice audio (call, Siri, nav) behaves like video — real time, never a
 * growing buffer; stale audio is dropped, not queued. OD-8A: no late nav.
 *
 * The stock numbers are Apple R14G17 / Cinemo r14 (docs/carplay/06_AV_PIPELINE.md §10.2):
 *  - target = audioLatencyMs; absent or negative → 80 ms (WiFi); clamped to ≥ 32 ms
 *    (AirPlayReceiverSession.c:3340-3353, AirPlayCommon.h:154-156);
 *  - watermark W = max(target + 20, 200) ms (AirPlayUtils.c:657-660);
 *  - node pool = max(50, 2 × packets(target)), Discard Excess triggered at allocated − 3
 *    (AirPlayUtils.c:503-505, :755);
 *  - device buffers by usage: phone 32 ms, Siri 128, alert 48, alt 48, all with an 8 ms period
 *    (SD:resmanagement/CarPlayAudioManager.java:227-268, CarPlayConfiguration.java:25-51).
 */
object VoiceLatency {

    /** `NAV_DURING_VOICE` (plan v5.8 V2): nav prompts while a call or Siri is live. */
    enum class NavDuringVoice {
        /** Discard the stream's audio in real time while a call or Siri is live — a non-default safety net. */
        DROP,
        /** Play it live, mixed (stock) — the DEFAULT (owner OD-8D, 2026-10-07). */
        PLAY_LIVE,
    }

    /**
     * Read in exactly ONE place: [VoiceIdleRules.navSuppressed] (its default argument). Owner
     * 2026-10-07: OD-8D = PLAY_LIVE (stock) — whatever arrives on the nav/AltAudio stream is played
     * live, never late; during a call that is the iPhone's maneuver tone mixed over the call (T3),
     * and while Siri records the iPhone sends nothing (T2). DROP stays only as a safety net.
     *
     * NAV focus has no switch any more: OD-8C = YES (stock — NAV requests no focus,
     * SD:CarPlayAudioManager.java:748-750; T1/T3), and the PER_EPOCH code was deleted with it.
     */
    @JvmField val NAV_DURING_VOICE: NavDuringVoice = NavDuringVoice.PLAY_LIVE

    /** audioLatencyMs absent or negative → this (stock WiFi default). */
    const val DEFAULT_TARGET_MS = 80
    /** audioLatencyMs is clamped up to this. */
    const val MIN_TARGET_MS = 32

    fun targetMs(audioLatencyMs: Int): Int =
        if (audioLatencyMs < 0) DEFAULT_TARGET_MS else maxOf(audioLatencyMs, MIN_TARGET_MS)

    /** The Discard Excess watermark: buffered audio is trimmed DOWN TO this. */
    fun watermarkMs(targetMs: Int): Int = maxOf(targetMs + 20, 200)

    /** Node pool size for [targetMs] at [auMs] per AU. */
    fun poolNodes(targetMs: Int, auMs: Int): Int =
        maxOf(50, 2 * ((targetMs + auMs - 1) / maxOf(auMs, 1)))

    /** Discard Excess fires when the nodes in use reach allocated − this. */
    const val POOL_TRIGGER_SLACK = 3

    /** Device (AudioTrack) buffer per purpose, ms — stock's per-usage values. */
    fun deviceBufferMs(p: Purpose): Int = when (p) {
        Purpose.CALL -> 32
        Purpose.ASSISTANT -> 128
        Purpose.ALERT -> 48
        Purpose.NAV -> 48
    }

    /** The worker's write period. */
    const val PERIOD_MS = 8

    /** No AU and no END for this long → an inferred END (W). [UNK, U13] — B-V1-1 measures it. */
    const val STREAM_STALL_MS = 3_000L

    /** A :9003 connection ending while not closed holds every live stream this long. [UNK, U19]. */
    const val RECONNECT_GRACE_MS = 500L

    /**
     * fix2: the longest device start stall (play() → first head advance; the HAL bus wake, cp12 139-408 ms)
     * that is carried as delay instead of being trimmed. Beyond it the start trim resumes (still bounded).
     */
    const val START_STALL_CAP_MS = 500

    /**
     * Owner answer 2026-10-08 (in-prompt duck stacking, plan_v5.md:2694): like stock, once the GM platform
     * duck is active for the nav prompt, gm_ccpa's own software duck lifts. The platform duck engages
     * within 5-20 ms of each bus1 activation with media active (cp12 `updateMutexSink … ducking -15.80dB`,
     * rc1 review wf_d866e1dc-1ed), and bus1 activates when OUR nav track starts consuming — so the lift is
     * keyed on the nav track's first head advance. Read in exactly one place: [VoiceStreams.navDuckLift].
     */
    @JvmField val NAV_DUCK_LIFT_ON_BUS: Boolean = true

    /** The lift's ramp (the platform duck takes over within ~20 ms). */
    const val NAV_DUCK_LIFT_RAMP_MS = 100L

    /**
     * fix2 (Fable perf F2, rc1 review): run the cp-voice-* workers and the :9003 reader at
     * THREAD_PRIORITY_URGENT_AUDIO, as MicUplink's capture already does. Read once in VoiceRouter.
     */
    @JvmField val VOICE_THREADS_URGENT_AUDIO: Boolean = true

    /** The head-drain wait at END is bounded by the device buffer plus this. */
    const val HEAD_DRAIN_SLACK_MS = 50L

    /**
     * The level of the NON-DEFAULT unduck hold ([NAV_UNDUCK_HOLD]). Stock T1 (radio 21:15:38.511): on
     * unduckAudio Cinemo ramps its own sink to 1.0 while the GM platform duck (`updateMutexSink() …
     * ducking -13.60dB`) stays until the AltAudio TEARDOWN; T5 shows -15.80 dB for an SMS alert.
     */
    const val NAV_HOLD_DB = -13.6

    /**
     * Whether gm_ccpa holds media at [NAV_HOLD_DB] after an unduck until the nav stream ends. DEFAULT
     * false, settled by data: in stock T1 the tail is the GM PLATFORM duck alone (Cinemo's sink at 1.0),
     * and the same platform duck runs for gm_ccpa — rc1-era cp12 shows `updateMutexSink … ducking -15.80dB`
     * (also -16.20 / -18.70) within 5-20 ms of EVERY bus1 activation with media active and 0.00 dB at
     * de-activation, on a build whose NAV requests NO focus (rc1 review wf_d866e1dc-1ed). A software
     * hold would therefore stack to ~-30 dB. Read in exactly one place: [VoiceStreams.navUnduckHold].
     */
    @JvmField val NAV_UNDUCK_HOLD: Boolean = false
    val NAV_HOLD_LEVEL: Float = Math.pow(10.0, NAV_HOLD_DB / 20.0).toFloat()

    /** Per-stream statistics window. */
    const val REPORT_MS = 5_000L

    /** A held CALL slot with no CALL BEGIN this long after a CALL END logs a W (log-only until U18). */
    const val CALL_SLOT_BACKSTOP_LOG_MS = 5_000L

    /** int16 peak above which a decoded frame is "loud" (~-32 dBFS; iOS streams digital silence). Stats only. */
    const val LOUD_PEAK = 800

    /** The purposes a voice stream maps to (plan v5.8 V1b mapping table). */
    enum class Purpose(val label: String) { CALL("call"), ASSISTANT("siri"), ALERT("alert"), NAV("nav") }

    /** What a BEGIN's (stream type, audioType) maps to. [unsupported] rows have no PCM path. */
    data class Mapping(val purpose: Purpose?, val unsupported: Boolean, val why: String)

    /** audioType codes on the wire (session.rs SETUP: media 0, telephony 1, speechRecognition 2, alert 3, default/absent 4, compatibility 5). */
    const val AT_MEDIA = 0
    const val AT_TELEPHONY = 1
    const val AT_SPEECH = 2
    const val AT_ALERT = 3
    const val AT_DEFAULT = 4
    const val AT_COMPATIBILITY = 5

    /**
     * The plan v5.8 V1b purpose mapping (stock getAudioType is SD:…:208-225):
     *  - 100 + telephony → CALL; 100 + speechRecognition → ASSISTANT; 100 + alert → ALERT;
     *  - 100 + default/absent → ASSISTANT (deviation [T1]: stock says Media; never seen in cp8);
     *  - 101 + default/absent → NAV (stock: AltAudio is Alt whatever its audioType);
     *  - any + compatibility → dropped (no PCM sink);
     *  - non-ELD codecs (non-media AAC-LC, PCM, Opus) → dropped (no voice decoder);
     *  - media never reaches :9003 (session.rs routes it to :9002).
     */
    fun mapping(streamType: Int, audioType: Int, codec: Int): Mapping = when {
        audioType == AT_COMPATIBILITY ->
            Mapping(null, true, "$streamType + compatibility: no PCM path (explicit decision)")
        codec != VoiceFrame.CODEC_AAC_ELD ->
            Mapping(null, true, "${VoiceFrame.codecName(codec)} on :9003: no voice decoder for it")
        audioType == AT_MEDIA -> Mapping(null, true, "media on :9003 (session.rs routes media to :9002)")
        // Stock: AltAudio is Alt whatever its audioType (SD:CarPlayAudioManager.java:220-222).
        streamType == 101 -> Mapping(Purpose.NAV, false, "101 + ${if (audioType == AT_DEFAULT) "default" else "audioType $audioType"}")
        audioType == AT_TELEPHONY -> Mapping(Purpose.CALL, false, "$streamType + telephony")
        audioType == AT_SPEECH -> Mapping(Purpose.ASSISTANT, false, "$streamType + speechRecognition")
        audioType == AT_ALERT -> Mapping(Purpose.ALERT, false, "$streamType + alert")
        else -> Mapping(Purpose.ASSISTANT, false, "$streamType + default (deviation: stock maps this to Media)")
    }
}
