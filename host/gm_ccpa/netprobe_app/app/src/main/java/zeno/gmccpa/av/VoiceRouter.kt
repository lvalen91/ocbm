package zeno.gmccpa.av

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
import android.media.MediaCodec
import android.media.MediaFormat
import zeno.gmccpa.ProbeLog
import zeno.gmccpa.logging.SessionTrace
import java.io.InputStream
import java.nio.ByteBuffer
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * The non-media half of CarPlay audio: phone calls, Siri, alerts and navigation.
 *
 * `:9003` multiplexes every non-media stream onto one socket. Each access unit is tagged
 * `[rate u32 BE][ch u16 BE][atype u8][len u32 BE][AU]`, where `atype` is the CarPlay purpose —
 * 0 media, 1 telephony, 2 speechRecognition, 3 alert, 4 default, 5 compatibility. That byte is the
 * whole basis for
 * routing: telephony, speechRecognition and the Siri `default` downlink are ALL negotiated as
 * AAC-ELD 16 kHz mono, so without it they are byte-for-byte indistinguishable and call audio would
 * land on the assistant output — wrong volume group, wrong ducking, and a call whose volume the
 * user cannot adjust. (`ccpa_custom` carries the byte as of the matching `tag_voice` change.)
 *
 * Routing is by [AudioAttributes] usage, never by device address: GM's CarAudioService maps
 * usage → context → volume group → bus. See `docs/13_AUDIO_ROUTING.md`.
 */
/**
 * @param onAssistant true while Siri is speaking, false once its sink is released.
 *
 * The media track must be **paused**, not merely ducked, for the duration. Decompiled from this head
 * unit (2026-08-12): AAOS `CarVolume` V1 ranks VOICE_COMMAND *below* MUSIC, so the volume knob targets
 * whichever of the two is active — and a ducked player is still an active player. GM layers its own
 * `GMAudioService.VolumeSynchronizer` on top, which retargets a knob-MUSIC event to the active route's
 * group, but only for GM-native sources (`BUS_TCP_PROMPT` and friends). Our Siri route reports
 * `source BUS_VOICE_COMMAND, extSource BUS_EXT_NONE`, which matches no retarget rule. Net effect: every
 * knob tick landed on `groupId 5` (MUSIC) and never once on `groupId 2` (VOICE_COMMAND).
 *
 * Pausing media removes MUSIC from the active set, leaving VOICE_COMMAND as the highest-priority active
 * context — at which point GM's own popup renders it, since its `gm_car_volume_items.xml` already maps
 * usage 16 to the title "voice volume".
 */
class VoiceRouter(
    private val ctx: Context,
    private val onDuck: (Boolean) -> Unit,
    private val onAssistant: (Boolean) -> Unit = {},
) {

    private val log = ProbeLog.sub("voice")
    private val running = AtomicBoolean(false)
    private val am = ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager

    /** One sink per PURPOSE, created lazily — most sessions never see a call or an alert. */
    private val sinks = HashMap<Purpose, Sink>()

    /** AUs handed to a sink. NOT "played": [Sink.feed] can still drop one, which is [ausDropped]. */
    val framesDecoded = AtomicLong(0)
    val bytesIn = AtomicLong(0)
    /** AUs a sink silently discarded while its track was PLAYING — oversized for the codec input
     *  buffer, or a short write on a live track. Both used to be counted as routed audio. This is the
     *  A6 defect signal and must read 0 on a spoken Siri turn; PCM thrown away because the track was
     *  PAUSED by a focus loss is NOT counted here (that is [ausDiscardedPausedTotal]), or an incoming
     *  call mid-turn would masquerade as a buffer-full drop. */
    val ausDropped = AtomicLong(0)
    /** AU remainders discarded because the track was paused/stopped by focus loss when written. A
     *  blocking `AudioTrack.write` turns non-blocking on a paused track and returns short; discarding
     *  there is correct, so this is a health counter, not a defect counter. */
    val ausDiscardedPausedTotal = AtomicLong(0)
    /** Keep-alive silence writes across all sinks — [Sink.keepAlive] writes that landed (w > 0).
     *  Expected ≈ (ASSISTANT_HOLD_MS - KEEPALIVE_AFTER_MS) / KEEPALIVE_PERIOD_MS ≈ 19 per Siri turn
     *  after the stream ends; ~30 was the double-feed signature (see [Sink.keepAlive]). */
    val silenceWritesTotal = AtomicLong(0)

    /**
     * `idleMs` is per-purpose, and that is load-bearing for the head unit's volume knob.
     *
     * AAOS points the hardware volume control at the highest-priority **active** audio context, and an
     * open AudioTrack keeps its context active whether or not sound is coming out. With one global 30 s
     * window, `dumpsys audio` showed MEDIA, ASSISTANT and CALL tracks all held open simultaneously —
     * so the knob adjusted whichever AAOS ranked highest, not what the driver was listening to. That is
     * the "stuck on Phone while media plays / stuck on Audio during a call" symptom.
     *
     * So: release call and alert promptly (the knob must follow reality the moment a call ends), keep
     * the assistant longer because Siri pauses mid-turn and rebuilding its track between turns clips
     * the response, and keep nav mid-range since guidance comes in bursts.
     */
    enum class Purpose(
        val usage: Int, val content: Int, val label: String, val idleMs: Long,
        /** Fill gaps with silence to stay an "active player" — see [Sink.keepAlive]. */
        val keepAlive: Boolean = false,
        /**
         * How long after this sink's last LOUD frame it keeps the media duck asserted. The duck is
         * ONE boolean shared by every sink ([sweepIdle]'s `anyLoud`), so this is a per-purpose input
         * to that OR, not a per-purpose duck: a quiet ASSISTANT inside its hold still keeps media at
         * 0.2 under a NAV prompt, and a loud NAV keeps it there after ASSISTANT's hold lapses.
         *
         * ASSISTANT holds for [ASSISTANT_HOLD_MS], the same clock as [assistantTick] — with the
         * shared 1.5 s default, Siri's own prompt→answer pause (~3 s in the 2026-09-09 capture)
         * un-ducked and re-ducked inside one turn (four `setVolume` edges, all on a paused and
         * therefore inaudible track — A7). One duck and one restore per turn is the verifiable shape.
         */
        val duckHoldMs: Long = DUCK_RELEASE_MS,
    ) {
        // bus4_call_out — shortest: a lingering CALL context is the most disruptive to the knob.
        // No keep-alive: call audio is continuous, and filling silence would hold the telephony
        // context active after the call ends, which is exactly the stuck-knob bug.
        CALL(AudioAttributes.USAGE_VOICE_COMMUNICATION, AudioAttributes.CONTENT_TYPE_SPEECH, "call", 3_000L),
        // bus2_voice_command_out — keep-alive: Siri goes quiet between prompt and answer, and a
        // drained track stops being an active player, which is what makes the head unit's volume knob
        // ignore Siri and adjust media instead.
        //
        // The window was 15 s, and that number was the whole of the "media silent for 10-20 s after
        // Siri" bug. Audio focus is abandoned ONLY by [release], which the sweeper calls at idleMs —
        // so the sink went on holding GAIN_TRANSIENT for 11 s after [assistantTick] had already
        // declared Siri done at ASSISTANT_HOLD_MS, AacPlayer stayed in LOSS_TRANSIENT, and media
        // could not resume until the sweep fired. Device-measured 2026-09-08: assistant done at
        // 21:25:55.940, media back at 21:26:07.932 — 12.0 s of dead radio.
        //
        // Tying it to ASSISTANT_HOLD_MS + one sweep period makes the focus hold end just after the
        // edge that says Siri is finished. The cost is a codec rebuild if Siri thinks for longer than
        // this between query and answer; that is ~130 ms of added latency and drops nothing, because
        // configure() runs synchronously ahead of the feed for the same AU.
        //
        // duckHoldMs = ASSISTANT_HOLD_MS puts the un-duck edge ONE sweep before the release edge, and
        // sweeps run 1000-1100 ms apart — so a late sweep lands both in the same pass. That is why
        // [sweepIdle] sends the un-duck BEFORE it releases (abandoning focus hands AAOS the GAIN that
        // makes AacPlayer play(), and it must not play at 0.2).
        ASSISTANT(AudioAttributes.USAGE_ASSISTANT, AudioAttributes.CONTENT_TYPE_SPEECH, "siri",
                  ASSISTANT_HOLD_MS + 1_000L, keepAlive = true, duckHoldMs = ASSISTANT_HOLD_MS),
        // bus3_call_ring_out — UNPROVEN on this head unit, see docs/13 §1
        ALERT(AudioAttributes.USAGE_VOICE_COMMUNICATION_SIGNALLING,
              AudioAttributes.CONTENT_TYPE_SONIFICATION, "alert", 3_000L),
        // bus1_navigation_out — continuous while speaking, so no keep-alive needed; this is why nav
        // volume was already adjustable and Siri was not.
        NAV(AudioAttributes.USAGE_ASSISTANCE_NAVIGATION_GUIDANCE,
            AudioAttributes.CONTENT_TYPE_SPEECH, "nav", 8_000L),
    }

    private companion object {
        /**
         * AAC-ELD AudioSpecificConfig as the shipping fdk-aac encoder actually emits it
         * (`ccpa_custom/docs/50`): SBR enabled by fdk auto-mode, frameLength 480. NOT the
         * `f8f03000` the older docs claim. `:9003` carries RAW access units with no ADTS header, so
         * MediaCodec has to be handed this or it cannot configure at all.
         */
        val ELD_CSD_16K_MONO = byteArrayOf(
            0xF8.toByte(), 0xF0.toByte(), 0x31, 0x2C, 0x00, 0xBC.toByte(), 0x00
        )
        /** samplingFrequencyIndex per ISO/IEC 14496-3 Table 1.16. */
        val SF_INDEX = intArrayOf(96000, 88200, 64000, 48000, 44100, 32000,
                                  24000, 22050, 16000, 12000, 11025, 8000, 7350)
        /** Never re-attempt configure per access unit — that drains the codec pool in seconds. */
        const val CONFIGURE_RETRY_MS = 5_000L
        /** Duck trigger. iOS streams CONTINUOUS DIGITAL SILENCE on idle voice streams, so a
         *  packet-flow trigger ducks media permanently from session start. Gate on energy. */
        const val DUCK_PEAK_THRESHOLD = 800
        /** `compatibility` — a PCM fallback iOS may pick for MEDIA if it declines the AAC-LC stream. */
        const val ATYPE_COMPATIBILITY = 5
        /** Sweeper cadence. Must be well under the AudioTrack buffer so keep-alive never underruns. */
        const val KEEPALIVE_TICK_MS = 100L
        /** Silence written per keep-alive write. Two ticks of headroom against a late thread. */
        const val KEEPALIVE_PERIOD_MS = 200L
        /**
         * AudioTrack buffer floor in MILLISECONDS rather than bytes, so it scales with the negotiated
         * rate. Load-bearing since the voice entries began advertising 48 kHz (bit 32) alongside 16 kHz.
         *
         * This is also Siri's first-word latency (A10): AudioFlinger does not mix a fresh MODE_STREAM
         * track until its buffer has filled once, and iOS delivers voice at realtime, so nothing is
         * audible until this much PCM has been written — 13 AUs at 30 ms. Measured on the truck by
         * the `FIRST AUDIO OUT +N ms after configure` line [Sink.feed] prints.
         *
         * NOT lowered yet, because the floor is coupled to [KEEPALIVE_AFTER_MS], not just
         * [KEEPALIVE_PERIOD_MS] as the earlier rationale ("two keep-alive periods, so a silence write
         * can never outrun the buffer") had it: with blocking writes the track sits near FULL in
         * steady state, and when the stream stops the first silence write cannot land until
         * KEEPALIVE_AFTER_MS later — so the buffer drains by that much first, then oscillates between
         * (floor - KEEPALIVE_AFTER_MS) and (floor - KEEPALIVE_AFTER_MS + KEEPALIVE_PERIOD_MS). The
         * margin against underrun is therefore floor - KEEPALIVE_AFTER_MS = 150 ms, against a sweeper
         * that can be a 100 ms tick late plus a HAL period. Cutting the floor to ~250 ms leaves ~0
         * margin; the correct A10 change is to lower KEEPALIVE_AFTER_MS and the floor together, once
         * the FIRST AUDIO OUT number says how much there is to win.
         */
        const val TRACK_BUFFER_MIN_MS = 2 * KEEPALIVE_PERIOD_MS
        /**
         * Only fill once the stream has actually stopped DELIVERING — measured on [Sink.lastAuAt],
         * the last AU of any loudness, NOT on [Sink.lastAudioAt] (last loud frame). iOS streams
         * silent AUs at realtime straight through Siri's prompt→answer pause, so on the loud clock
         * this gate opened while the stream was still feeding the track: 200 ms of silence per
         * 200 ms on top of 30 ms per 30 ms, 2× realtime into a 400-640 ms buffer, full in ~0.5 s,
         * and every AU write after that came back short. That was A6 — 42 of 249 AUs (17 %) of one
         * Siri turn dropped in the 2026-09-09 capture.
         */
        const val KEEPALIVE_AFTER_MS = 250L
        /**
         * How long after Siri's last audio we keep media paused. Must exceed AAOS's ~3 s
         * `audioVolumeKeyEventTimeoutMs`, or MUSIC re-enters the active set (and re-latches the knob)
         * while the driver is still reaching for the dial.
         */
        const val ASSISTANT_HOLD_MS = 4_000L
        /** Restore media this long after the last ENERGETIC frame on any voice sink — the default
         *  [Purpose.duckHoldMs]; ASSISTANT overrides it with [ASSISTANT_HOLD_MS]. */
        const val DUCK_RELEASE_MS = 1_500L
        /**
         * A11 control-plane Siri edges, from `modesChanged` (see [onModes]). Three windows, all
         * bounding how far the control plane may override the energy gate:
         *
         * A START is trusted this long with no AU yet on the ASSISTANT sink. iOS SETUPs the type-100
         * stream ~300 ms after the edge (2026-09-09: edge 39.165, SETUP 39.472, first AU 39.549); a
         * stream that never comes is not Siri, and past this the energy gate is the truth again.
         */
        const val CTRL_LEAD_MS = 3_000L
        /**
         * Past the lead window a START stays trusted only while AUs — loud OR silent — are still
         * arriving ([Sink.lastAuAt]). Stream 100 is continuous at 30 ms/AU straight through Siri's
         * prompt→answer pause (249 AUs in 7.46 s on 2026-09-09), so a gap this long means iOS tore
         * the stream down and the END edge was lost on the seam. Bounded, so a lost END can never
         * pin media paused.
         */
        const val CTRL_AU_LIVENESS_MS = 1_000L
        /**
         * After an END edge, how long the energy gate may still hold before the ASSISTANT sink is
         * treated as done: un-ducked, released, focus abandoned, media resumed. Must exceed the
         * ASSISTANT track's buffer depth (≥ [TRACK_BUFFER_MIN_MS]; the `buffer Nms` line in
         * [Sink.configure] reports the real figure) plus one sweep tick, or `release()` cuts Siri's
         * last word out of the track.
         *
         * WHAT THIS GIVES UP: [ASSISTANT_HOLD_MS]'s second rationale. That 4 s exceeds AAOS's ~3 s
         * `audioVolumeKeyEventTimeoutMs` so MUSIC does not re-enter the active set — and re-latch
         * the knob — while the driver is still reaching for the dial after Siri stops. With 750 ms
         * the knob follows media ~0.8 s after Siri's last word instead of ~5 s. iOS itself re-SETUPs
         * the media stream 93 ms after this edge (47.078 → 47.171), so a native CarPlay unit resumes
         * at once and the knob rationale was only ever covering our own inference latency. If the
         * knob turns out to matter more than 4.4 s of dead radio per turn, raise this to 3_000L and
         * nothing else changes.
         */
        const val CTRL_END_GRACE_MS = 750L
        /** [SessionTrace.Board] entry for the router itself (the sweeper thread) and the per-sink prefix. */
        const val BOARD_ROUTER = "voice-router"
        const val BOARD_SINK_PREFIX = "voice-sink/"
        /** [SessionTrace] name prefix, completed with [Purpose.label]; `av/`-scoped for `stopSession`. */
        const val EXPECT_FIRST_OUT_PREFIX = "av/voice-first-out/"
        /**
         * Budget from [Sink.configure] to AudioFlinger consuming the first frame of that track
         * (`playbackHeadPosition > 0` — the A10 `FIRST AUDIO OUT` line). Not yet measured on the
         * truck; the arithmetic is: [TRACK_BUFFER_MIN_MS] (400 ms) of pre-fill at realtime delivery,
         * plus this unit's ~450 ms fill grace and one `restartIfDisabled` if the fill loses that
         * race — under 1 s for any continuous stream. A head position still at 0 after 3 s is a track
         * the HAL is not mixing: a refused focus muting it, or a burst too short to ever reach the
         * pre-fill mark (which is what an ALERT shorter than 400 ms would do — a real finding, not a
         * false alarm). Cancelled, not missed, when focus loss pauses the track first.
         */
        const val VOICE_FIRST_OUT_MS = 3_000L
        /**
         * Track underruns a released sink may show before its summary is a `W`. One, because the
         * pre-fill is 400 ms against a ~450 ms fill grace and a late sweep tick can lose that race
         * once per stream start (docs/13 §3 rule 2); the keep-alive's own margin is 150 ms, so a
         * second episode is a silence-fill that arrived late — inaudible in itself, but the shape
         * that becomes audible under load.
         */
        const val UNDERRUN_TOLERANCE_PER_SINK = 1
    }

    /**
     * Sweeper-stage fault counters, `cp-voice-sweep` only. The sweeper wraps each stage in
     * `runCatching` so one bad tick cannot kill the thread — but until 2026-09-10 the failure was
     * swallowed outright, and a stage that throws on EVERY tick (a sink whose codec is in a bad
     * state, a callback into a torn-down AacPlayer) meant sinks never released, the duck never
     * cleared or media never resumed, with zero lines to say so. Logged on the first fault and
     * every 50th (5 s at the 100 ms tick), never per tick.
     */
    private val sweepFaults = HashMap<String, Int>()

    private fun sweepFault(stage: String, t: Throwable) {
        val n = (sweepFaults[stage] ?: 0) + 1
        sweepFaults[stage] = n
        if (n == 1 || n % 50 == 0) {
            log.e("$stage threw ${t.javaClass.simpleName}: ${t.message} (x$n) — sweeper skipped it; " +
                  "sinks may not release, the duck may stick, media may stay paused")
        }
    }

    init {
        // 48 kHz mono is now advertised (audioFormat bit 32) alongside 16 kHz, and it is served by the
        // SAME synthesised path as the truck-proven 48 kHz STEREO ASC used for nav/alert — only the
        // channelConfiguration nibble differs (f8 e6 30 00 vs f8 e7 10 00). No ASC travels on the wire:
        // iOS builds its encoder from the audioFormat constant alone, so a drift here surfaces as
        // silent dropped frames, not a configure error. Assert the layout instead of trusting it.
        check(eldCsd(48000, 1).contentEquals(byteArrayOf(0xF8.toByte(), 0xE6.toByte(), 0x30, 0x00))) {
            "eldCsd(48k mono) drifted from the ISO 14496-3 1.6.2.1 layout"
        }
    }

    /**
     * Build an AAC-ELD AudioSpecificConfig for [rate]/[channels].
     *
     * A single hardcoded ASC was wrong: it encodes AOT 39, samplingFrequencyIndex 8 (16 kHz),
     * channelConfiguration 1 — and AAC decoders treat csd-0 as AUTHORITATIVE over KEY_SAMPLE_RATE
     * and KEY_CHANNEL_COUNT. The producer puts MIXED formats on this one socket (telephony/Siri
     * 16 k mono, alert/nav 48 k stereo), so handing every sink the 16 k mono ASC made nav and alert
     * either fail to configure or decode ~3x fast with the channels garbled.
     *
     * Layout: 5 bits AOT (39 = ER AAC ELD, so escape 31 + 6-bit (39-32)), 4 bits freq index,
     * 4 bits channel config, then the ELDSpecificConfig. The 16 k mono case is returned verbatim
     * from the capture in `ccpa_custom/docs/50` rather than synthesised, because that one is
     * device-confirmed against the shipping fdk encoder (SBR on, frameLength 480).
     */
    private fun eldCsd(rate: Int, channels: Int): ByteArray {
        if (rate == 16000 && channels == 1) return ELD_CSD_16K_MONO
        val fi = SF_INDEX.indexOf(rate).let { if (it < 0) 3 else it }   // default 48 kHz
        val ch = channels.coerceIn(1, 2)
        // 11111 (esc) 000111 (39-32) | fi(4) | ch(4) | ELDSpecificConfig: frameLengthFlag=0,
        // aacSectionDataResilience=0, aacScalefactorDataResilience=0, aacSpectralDataResilience=0,
        // ldSbrPresentFlag=0, then a 4-bit ELDEXT_TERM(0).
        var acc = 0L; var bits = 0
        fun put(v: Int, n: Int) { acc = (acc shl n) or (v.toLong() and ((1L shl n) - 1)); bits += n }
        put(31, 5); put(39 - 32, 6); put(fi, 4); put(ch, 4)
        // frameLengthFlag = 1 => 480 samples. NOT 0/512: every proven ELD config in this project
        // is 480 (the macOS decoder hardcodes it at every rate, and the device-confirmed 16k ASC
        // sets this bit). A frameLength mismatch is a configure failure or garbage output.
        put(1, 1); put(0, 1); put(0, 1); put(0, 1); put(0, 1); put(0, 4)
        val pad = (8 - (bits % 8)) % 8
        put(0, pad)
        val out = ByteArray((bits) / 8)
        for (i in out.indices) out[i] = ((acc shr ((out.size - 1 - i) * 8)) and 0xFF).toByte()
        return out
    }

    fun start() {
        running.set(true)
        // A TIMER thread, not a call site in the read loop. Driving the sweep from ingest meant it
        // could only run while :9003 was delivering — so on a normal TEARDOWN, or the dropped-stop
        // case it was written for, no AU arrives, no sweep runs, and the sink keeps its focus and a
        // PLAYING track forever. That is the session-long volume-group wedge, and it is exactly what
        // the reference implementation runs on its own playback clock.
        // Ticks at KEEPALIVE_TICK_MS, not 1 s: the keep-alive silence-fill has to stay ahead of the
        // AudioTrack's buffer, and a 1 s cadence would underrun exactly the track it exists to hold
        // open. sweepIdle() self-throttles to 1 Hz internally (lastSweep), so the faster tick costs
        // nothing there.
        sweeper = Thread({
            while (running.get()) {
                try { Thread.sleep(KEEPALIVE_TICK_MS) } catch (_: InterruptedException) { return@Thread }
                val now = android.os.SystemClock.elapsedRealtime()
                // pollFirstOut rides the same tick and the same tryLock as keepAlive: it is what lets
                // a sink whose LAST AU arrived before the pre-fill mark still report FIRST AUDIO OUT.
                runCatching { synchronized(sinks) { sinks.values.forEach { it.keepAlive(now); it.pollFirstOut() } } }
                    .onFailure { sweepFault("keepAlive", it) }
                runCatching { assistantTick(now) }.onFailure { sweepFault("assistantTick", it) }
                runCatching { sweepIdle() }.onFailure { sweepFault("sweepIdle", it) }
            }
        }, "cp-voice-sweep").apply { isDaemon = true; start() }
        SessionTrace.Board.up(BOARD_ROUTER, "cp-voice-sweep ticking at ${KEEPALIVE_TICK_MS}ms; sinks build on first AU")
    }

    private var sweeper: Thread? = null

    /**
     * FLAG-ONLY teardown, exactly like [AacPlayer.stop].
     *
     * It releases NOTHING. Two places release sinks, and only two: [consume]'s `finally` on the
     * consume thread, and [sweepIdle] on `cp-voice-sweep` — both serialized against `feed()` by
     * `Sink.lock` (this KDoc used to say the consume thread was the sole releaser; the sweeper has
     * released from its own timer thread since `start()` moved it there). Releasing here — from the
     * UI thread, via onDestroy, without that lock — while the consume thread is inside `feed()`
     * unmaps the direct input/output ByteBuffers it is writing into. That is a SIGSEGV in libmedia,
     * not a catchable exception, so no runCatching helps: the app dies and takes the session with
     * it, precisely when a driver taps away mid-Siri.
     *
     * Unducking here IS safe and necessary — `onDuck` lands in AacPlayer.setDucked, which is
     * synchronized, idempotent per source, and only one input of a min(voice, focus) so it cannot
     * cancel a focus duck — and the consume thread may never run again to do it.
     *
     * The summary is JUDGED: [ausDropped] > 0 is an `E` (the A6 signal — audio iOS sent and a
     * PLAYING track refused), everything else `I`. [ausDiscardedPausedTotal] never lifts the
     * severity: that is PCM discarded because focus loss paused the track, which is correct. The
     * per-sink underrun figure is judged where it is latched, on the `released` line in [sweepIdle].
     */
    fun stop(why: String) {
        running.set(false)
        onDuck(false)
        val dropped = ausDropped.get()
        val msg = "stopping ($why) — ${framesDecoded.get()} AUs routed, $dropped dropped, " +
                  "${ausDiscardedPausedTotal.get()} discarded-paused, ${silenceWritesTotal.get()} silence writes, " +
                  "${bytesIn.get()} bytes"
        if (dropped > 0) log.e("$msg — AUs dropped on a PLAYING track: audio iOS sent that was never heard") else log.i(msg)
        SessionTrace.Board.down(BOARD_ROUTER, "stopped — $why")
    }

    /** Consume the tagged voice seam until it closes. Blocking; call on its own thread. */
    fun consume(ins: InputStream) {
        val hdr = ByteArray(11)
        try {
            while (running.get()) {
                if (!readFully(ins, hdr, 11)) break
                val rate = be32(hdr, 0)
                val ch = ((hdr[4].toInt() and 0xFF) shl 8) or (hdr[5].toInt() and 0xFF)
                val atype = hdr[6].toInt() and 0xFF
                val len = be32(hdr, 7)
                if (rate !in 8000..48000) {
                    log.e("voice desync: implausible rate ${rate}Hz — the seam is probably speaking " +
                          "the forward-encrypted v2 framing (check OCBM_FWD_ENC on the box)")
                    break
                }
                if (len < 0 || len > 1 shl 20) { log.e("voice desync: len=$len"); break }
                if (len == 0) continue
                val au = ByteArray(len)
                if (!readFully(ins, au, len)) break
                bytesIn.addAndGet((11 + len).toLong())
                route(atype, rate, ch, au)
            }
        } catch (t: Throwable) {
            if (running.get()) log.e("consume: ${t.javaClass.simpleName}: ${t.message}")
        } finally {
            val why = if (running.get()) "voice seam connection ended — rebuilt on the next AU" else "router stopped"
            synchronized(sinks) { sinks.values.forEach { it.release(why) }; sinks.clear() }
            onDuck(false)
        }
    }

    /** Edge-detected so the media track is paused/resumed once per Siri turn, not once per tick.
     *  Written only by [assistantTick] on `cp-voice-sweep`. */
    private var assistantWasActive = false

    /**
     * Control-plane Speech state from iOS's `modesChanged` (A11). Written on `cp-meta` by [onModes],
     * read on `cp-voice-sweep` by [assistantTick] and [sweepIdle]. Volatiles only: [onModes] runs on
     * the metadata seam thread, which must never block (see `MetadataSeam`), so no lock is taken and
     * no edge is resolved there.
     *
     * NEVER the sole truth. The `:9004` seam it arrives on is best-effort by design —
     * `iap2_core::metadata::emit_command_plist` try_locks the seam and DROPS the plist if a
     * now-playing or artwork write holds it — so either edge of a turn can go missing, and a phone
     * may not send the state at all. The energy gate ([Sink.lastAudioAt] / [ASSISTANT_HOLD_MS])
     * remains the fallback in every case; the control plane only moves the edges earlier:
     *   - never sent: `ctrlSeen` stays false and every predicate below is false — byte-identical
     *     to the energy gate alone.
     *   - START dropped: the energy gate starts the turn as before; END still cuts the hold to
     *     [CTRL_END_GRACE_MS].
     *   - END dropped: `ctrlSpeech` stops counting once AUs stop for [CTRL_AU_LIVENESS_MS] and the
     *     [CTRL_LEAD_MS] window is over, and the energy gate's 4 s hold ends the turn as before.
     *   - START with no stream ever: expires at [CTRL_LEAD_MS] — a ≤3 s media pause, once.
     *   - a later turn whose START was dropped: its first loud frame is newer than the stale END
     *     stamp, so [ctrlEnded] is false and the energy rule applies.
     */
    @Volatile private var ctrlSpeech = false
    /** `elapsedRealtime` of the last START/END edge; 0 until one has arrived. */
    @Volatile private var ctrlSpeechAt = 0L
    @Volatile private var ctrlSeen = false
    /** Last logged tuple, so the `modes:` line prints per change rather than per frame (iOS sends
     *  3-5 identical frames per transition). `cp-meta` only. */
    private var lastModesLogged = Long.MIN_VALUE

    /**
     * One decoded `modesChanged` from `CarPlayActivity.onCommandPlist`, on `cp-meta`.
     *
     * `speechMode == -1` is `kAirPlaySpeechMode_NotApplicable`; anything else means the Speech app
     * state is live — Siri is up. That is exactly, and only, what the 2026-09-09 capture proved:
     * the 287 B → 277 B bracket around the turn is the uniqued 8-byte `-1` int leaving the plist.
     * Which of none/speaking/recognizing the turn carries, and who the Speech entity is, is NOT yet
     * known — the `modes:` line below is what settles it on the next capture, and it is also the
     * falsifier: a 277 B frame logging `mode=-1` means the byte argument was wrong and this must be
     * reverted.
     *
     * `phone` (appStateID 2) and `turns` (3) are logged and deliberately NOT acted on: no call has
     * been captured, so PhoneCall's entity behaviour is unverified, and a second call-state machine
     * beside the `atype` byte would be two inferences that can disagree. When a call is captured,
     * the right shape is to shorten CALL's idle window from here, not to add a parallel path.
     */
    fun onModes(speechMode: Long, speechEntity: Long, phoneEntity: Long, turnsEntity: Long, size: Int) {
        val active = speechMode != -1L
        val key = (speechMode shl 12) or (speechEntity shl 8) or (phoneEntity shl 4) or turnsEntity
        if (key != lastModesLogged) {
            lastModesLogged = key
            log.i("modes: speech mode=$speechMode entity=$speechEntity phone=$phoneEntity turns=$turnsEntity ($size B)")
        }
        ctrlSeen = true
        if (active != ctrlSpeech) {
            // Stamp BEFORE flag. Both readers ([assistantTick], [ctrlEnded]) read the volatile flag
            // first and the stamp second, so this order guarantees a reader that sees the new flag
            // also sees the new stamp. The other order let one sweep tick pair a fresh END with the
            // old START stamp — "ended long ago" — and release the sink inside the grace, cutting
            // Siri's last word out of the track.
            ctrlSpeechAt = android.os.SystemClock.elapsedRealtime()
            ctrlSpeech = active
            // Let the next 100 ms tick sweep instead of waiting out the 1 Hz throttle: this edge is
            // what turns the 5 s post-Siri silence into <1 s, and the sweep is where the release is.
            lastSweep = 0L
        }
    }

    /**
     * An END edge that is newer than the sink's last loud frame and past its grace. `s` may be null
     * (edge arrived with no sink yet) — still "ended", so a later energy-only start is not blocked
     * by a stale stamp: its first loud frame is newer than `ctrlSpeechAt`. Call under `sinks`.
     */
    private fun ctrlEnded(now: Long, s: Sink?): Boolean =
        ctrlSeen && !ctrlSpeech && now - ctrlSpeechAt > CTRL_END_GRACE_MS &&
            (s == null || ctrlSpeechAt >= s.lastAudioAt)

    /**
     * Signal Siri's speaking window on its own cadence, independent of [sweepIdle]'s 1 Hz throttle.
     *
     * Runs from the fast sweeper tick: the media track has to be paused BEFORE the driver reaches for
     * the knob, and a second of latency is enough to miss the whole utterance.
     *
     * `live` is control-plane OR energy, with the control plane bounded on both sides — see
     * [ctrlSpeech] for the six failure cases. With a START edge the pause lands ~630 ms before the
     * energy gate's (39.165 vs 39.795 on 2026-09-09); with an END edge, [CTRL_END_GRACE_MS] after
     * it instead of [ASSISTANT_HOLD_MS] after the last loud frame.
     */
    private fun assistantTick(now: Long) {
        val live = synchronized(sinks) {
            val s = sinks[Purpose.ASSISTANT]
            val energyLive = s != null && s.isConfigured && now - s.lastAudioAt < ASSISTANT_HOLD_MS
            val ctrlLive = ctrlSpeech && (now - ctrlSpeechAt < CTRL_LEAD_MS ||
                (s != null && s.isConfigured && now - s.lastAuAt < CTRL_AU_LIVENESS_MS))
            ctrlLive || (energyLive && !ctrlEnded(now, s))
        }
        if (live == assistantWasActive) return
        assistantWasActive = live
        log.i("assistant ${if (live) "SPEAKING — pausing media so the knob can reach the voice group" else "done — media resumes"}")
        // A throw here is a lost pause/resume edge — the post-Siri dead-radio class — and it was
        // swallowed. Per edge, so a W is cheap.
        runCatching { onAssistant(live) }
            .onFailure { log.w("onAssistant($live) threw ${it.javaClass.simpleName}: ${it.message} — media was NOT ${if (live) "paused" else "resumed"}") }
    }

    /**
     * Release sinks whose stream has gone quiet, and drop the duck when none is speaking.
     *
     * This is load-bearing, not housekeeping. iOS streams CONTINUOUS DIGITAL SILENCE on idle voice
     * streams and `:9003` is a persistent socket held across re-SETUPs, so without a sweep:
     *   - the first Siri word or nav prompt of a drive ducks media to 0.2 FOREVER, and
     *   - the first idle telephony packet takes AUDIOFOCUS_GAIN_TRANSIENT with
     *     USAGE_VOICE_COMMUNICATION and never abandons it, which on AAOS pins the hardware volume
     *     keys to the call group and suppresses the head unit's own sources for the rest of the
     *     session.
     *
     * Both clocks here are the LOUD clock ([Sink.lastAudioAt]), deliberately: idle on the AU clock
     * would never release a sink iOS keeps streaming silence into, which is the wedge above.
     *
     * Runs on `cp-voice-sweep`, NOT the consume thread — this KDoc used to claim the consume thread
     * and that it was "the only thread allowed to release", which stopped being true when [start]
     * moved the sweep onto its own timer so a dropped stop message could not starve it. A release
     * from here races `feed()` on the consume thread (the codec's mapped ByteBuffers, and the track);
     * `Sink.lock` is what makes that safe — see [Sink.release].
     *
     * ORDER IS LOAD-BEARING (A7): the un-duck is sent BEFORE dead sinks are released. [Sink.release]
     * abandons focus, AAOS answers with AUDIOFOCUS_GAIN, and AacPlayer's listener calls `play()` on
     * the main thread — if the duck were still asserted at that moment media would resume at 0.2
     * until this method got round to `onDuck(false)`. With ASSISTANT's `duckHoldMs` one sweep
     * short of its `idleMs` and sweeps 1000-1100 ms apart, a late sweep puts both edges in one pass,
     * so the old order (release loop, then un-duck) was a live bug, not a theoretical one.
     */
    private fun sweepIdle() {
        val now = android.os.SystemClock.elapsedRealtime()
        if (now - lastSweep < 1000) return
        lastSweep = now
        var anyLoud = false
        val dead = ArrayList<Purpose>()
        synchronized(sinks) {
            for ((p, s) in sinks) {
                // Never sweep a sink that failed to configure: its lastAudioAt is 0, so it would be
                // removed on the next tick and rebuilt on the next AU, at 1 Hz forever.
                if (!s.isConfigured) continue
                val quiet = now - s.lastAudioAt
                // A11: an END edge from iOS past its grace ends ASSISTANT's duck hold AND its idle
                // window at once — same pass, same order as the timer path, so the un-duck below
                // still lands before the release (A7). Other purposes have no control-plane edge.
                val ended = p == Purpose.ASSISTANT && ctrlEnded(now, s)
                // Per-purpose hold folded into ONE boolean — the duck is shared, so a sink inside its
                // hold keeps media ducked for everyone; see [Purpose.duckHoldMs].
                if (quiet < p.duckHoldMs && !ended) anyLoud = true
                if (quiet > p.idleMs || ended) dead.add(p)   // per-purpose, not one global window — see [Purpose]
            }
        }
        // Un-duck FIRST, outside the sinks lock (it is a call out to AacPlayer.setVoiceDucked, which
        // takes its own lock). Idempotent per source, so re-asserting at 1 Hz is harmless.
        if (!anyLoud) onDuck(false)
        // Stats are latched by release() and copied here, because the track is gone by the time we
        // log outside the lock. Severity travels with the line: dropped AUs on a playing track are
        // the A6 signal (E); underruns past the per-stream-start tolerance are audible (W).
        val released = ArrayList<Pair<Int, String>>()
        synchronized(sinks) {
            for (p in dead) {
                val s = sinks[p] ?: continue
                val ended = p == Purpose.ASSISTANT && ctrlEnded(now, s)
                if (now - s.lastAudioAt <= p.idleMs && !ended) continue   // went loud between the two blocks
                sinks.remove(p)
                val why = if (ended) "iOS says Siri ended ${now - ctrlSpeechAt}ms ago" else "idle ${p.idleMs / 1000}s"
                s.release(why)
                val level = when {
                    s.ausDroppedFull > 0 -> 2
                    s.underruns > UNDERRUN_TOLERANCE_PER_SINK -> 1
                    else -> 0
                }
                released.add(level to "${p.label}: $why — released (${s.stats()}; focus + volume group freed)")
            }
        }
        released.forEach { (level, m) ->
            when (level) {
                2 -> log.e("$m — AUs dropped on a PLAYING track: the A6 signal")
                1 -> log.w("$m — more than the one fill-timeout underrun a stream start may show")
                else -> log.i(m)
            }
        }
    }

    @Volatile private var lastSweep = 0L
    /** Warn once per unknown atype rather than per access unit. */
    private val unroutedLogged = java.util.Collections.synchronizedSet(HashSet<Int>())

    /**
     * `atype` 4 (`default`) is the one value that needs the format to disambiguate: 16 kHz mono is
     * the Siri downlink on type 100, 48 kHz stereo is alt-audio/navigation on type 101. Those two
     * genuinely differ in format, so this split is safe — unlike guessing between 1/2/4.
     */
    private fun purposeFor(atype: Int, rate: Int, ch: Int): Purpose? = when (atype) {
        1 -> Purpose.CALL
        2 -> Purpose.ASSISTANT
        3 -> Purpose.ALERT
        4 -> if (rate >= 44100 && ch >= 2) Purpose.NAV else Purpose.ASSISTANT
        // 0 = media (should never reach :9003) and 5 = compatibility (a PCM media fallback this
        // receiver cannot play). Both were previously swept into the `else` and mis-routed to NAV
        // purely because they happen to be 48 kHz stereo.
        else -> null
    }

    private fun route(atype: Int, rate: Int, ch: Int, au: ByteArray) {
        val p = purposeFor(atype, rate, ch) ?: run {
            if (unroutedLogged.add(atype)) {
                if (atype == ATYPE_COMPATIBILITY) {
                    // Say what this COSTS, not just that it happened. A dropped compatibility stream
                    // is silent media with an otherwise healthy session — the same presentation as a
                    // stuck audio-focus hold, and the two are only distinguishable from this line.
                    log.e("atype 5 (compatibility, ${rate}Hz ${ch}ch) arrived on :9003 and was DROPPED.")
                    log.e("  iOS fell back to PCM for media instead of the AAC-LC type-102 stream.")
                    log.e("  CONSEQUENCE: media is silent while calls and Siri still play.")
                    log.e("  This receiver has no PCM media sink; see docs/13 §compatibility.")
                } else {
                    log.w("atype $atype (${rate}Hz ${ch}ch) has no sink — dropping")
                }
            }
            return
        }
        val sink = synchronized(sinks) {
            sinks.getOrPut(p) { Sink(p).also { it.configure(rate, ch) } }
        }
        if (sink.isConfigured && (sink.rate != rate || sink.channels != ch)) {
            log.i("${p.label}: format changed ${sink.rate}Hz${sink.channels}ch -> ${rate}Hz${ch}ch")
            sink.release("format changed ${sink.rate}Hz${sink.channels}ch -> ${rate}Hz${ch}ch — rebuilding")
            sink.configure(rate, ch)
        }
        // A sink that failed configure(), or that feed() released on a dead track / codec fault, stays
        // in the map unconfigured and nothing else ever rebuilds it (sweepIdle skips unconfigured
        // sinks on purpose). Retry here; configure() self-throttles on configureFailedAt.
        if (!sink.isConfigured) sink.configure(rate, ch)
        if (!sink.isConfigured) return   // still in backoff — do not count an AU nothing decoded
        sink.feed(au)
        framesDecoded.incrementAndGet()
    }

    // ---- one sink per purpose ----------------------------------------------------------------

    private inner class Sink(val p: Purpose) {
        @Volatile var rate = 0
        @Volatile var channels = 0
        /** True only after a configure() that fully succeeded; guards the format-change branch. */
        @Volatile var isConfigured = false
        /**
         * The LOUD clock: last frame whose peak cleared [DUCK_PEAK_THRESHOLD], or configure time.
         * Gates the duck ([sweepIdle]), the idle window ([Purpose.idleMs]), the assistant edge
         * ([assistantTick]) and [keepAlive]'s UPPER bound — every consumer that must ignore the
         * digital silence iOS streams on an idle voice channel.
         */
        @Volatile var lastAudioAt = 0L
        /**
         * The AU clock: last access unit DELIVERED to [feed], loud or silent, or configure time.
         * Gates only [keepAlive]'s LOWER bound — "has the stream stopped feeding the track?" — which
         * is a question about delivery, not loudness. Gating it on [lastAudioAt] was A6.
         */
        @Volatile var lastAuAt = 0L
        /** Track underruns as of the last [release]; -1 until one has happened. Latched there because
         *  the sweeper logs after the track has already been dropped. */
        @Volatile var underruns = -1
        // Per-sink counters for the `idle … released` line. Each has ONE writer thread (the consume
        // thread for the AU counters, cp-voice-sweep for silenceWrites), so a volatile ++ is safe.
        /** AUs handed to [feed] since this Sink object was created (survives a format-change rebuild). */
        @Volatile var ausFed = 0
        /** AU remainders lost on a PLAYING track — the A6 signal; must be 0 on a spoken Siri turn. */
        @Volatile var ausDroppedFull = 0
        /** AU remainders discarded because the track was paused by focus loss when written. */
        @Volatile var ausDiscardedPaused = 0
        /** [keepAlive] writes that landed. ~19 per Siri turn is healthy; ~30 was the double-feed. */
        @Volatile var silenceWrites = 0
        @Volatile private var codec: MediaCodec? = null
        @Volatile private var track: AudioTrack? = null
        @Volatile private var focus: AudioFocusRequest? = null
        @Volatile private var configureFailedAt = 0L
        /** Absolute deadline for the next keep-alive write; see [keepAlive]. */
        @Volatile private var nextSilenceAt = 0L
        /** A10 instrumentation: when [configure] built the current track, its depth in ms, and
         *  whether the first-audio-out line has been printed for it. */
        @Volatile private var configuredAt = 0L
        @Volatile private var trackBufferMs = 0
        @Volatile private var firstOutSeen = false
        /** Logged once per sink on the first failed keep-alive write, reset on the next success. */
        private var keepAliveFailLogged = false

        private val boardName = BOARD_SINK_PREFIX + p.label
        private val expectName = EXPECT_FIRST_OUT_PREFIX + p.label

        /** Standing state for the [SessionTrace.Board]: format, usage, real buffer, focus, first-out. */
        private fun boardDetail(): String =
            "AAC-ELD ${rate}Hz ${channels}ch usage=${p.usage} buffer ${trackBufferMs}ms " +
            "focus=${focusName(focusState)}" + (if (firstOutSeen) "" else " (no audio out yet)")

        /**
         * The first time AudioFlinger has consumed anything from this track — the A10 measurement,
         * and the [expectName] met() edge. Reached from [feedLocked] (per AU) and [pollFirstOut] (per
         * sweeper tick), both under [lock], so a sink whose last AU landed before the pre-fill mark
         * is still observed when the HAL finally starts.
         */
        private fun markFirstOut() {
            firstOutSeen = true
            val now = android.os.SystemClock.elapsedRealtime()
            log.i("${p.label}: FIRST AUDIO OUT +${now - configuredAt}ms after configure " +
                  "(track buffer ${trackBufferMs}ms, $ausFed AUs written)")
            SessionTrace.met(expectName)
            SessionTrace.Board.up(boardName, boardDetail())
        }

        /** `cp-voice-sweep`, under `sinks`: same tryLock discipline as [keepAlive], one head-position read. */
        fun pollFirstOut() {
            if (firstOutSeen || !isConfigured) return
            if (!lock.tryLock()) return
            try {
                val t = track ?: return
                if ((runCatching { t.playbackHeadPosition }.getOrNull() ?: 0) > 0) markFirstOut()
            } finally {
                lock.unlock()
            }
        }

        /**
         * Serializes every caller that touches this sink's codec or track: [feed] (consume thread),
         * [keepAlive] and [release] (`cp-voice-sweep`), and [release] again from the consume thread
         * itself. `AudioTrack.write` is not safe for concurrent callers, and [keepAlive] and [feed]
         * overlapped in exactly the window A6 fixed (a silent stream plus a keep-alive that thought
         * the stream had stopped); `MediaCodec.release` under a live `getOutputBuffer` is the SIGSEGV
         * described on [stop]. A ReentrantLock, not `synchronized`, so [keepAlive] can `tryLock` and
         * skip a tick instead of parking the sweeper behind a blocking write.
         */
        val lock = java.util.concurrent.locks.ReentrantLock()

        fun stats(): String =
            "$ausFed AUs, $ausDroppedFull dropped, $ausDiscardedPaused discarded-paused, " +
            "$silenceWrites silence writes, $underruns track underruns"

        /**
         * Keep the track in PLAYSTATE_PLAYING through gaps in the stream, by writing paced silence.
         *
         * **This is what makes the head unit's volume knob work for Siri.** AAOS targets volume keys
         * at the volume group that owns an *active player* — a track actually in PLAYSTATE_PLAYING —
         * not at whoever holds focus or declares a usage. Siri is bursty: there is a gap of a second
         * or more between your query and the spoken answer. With nothing written, the track drains,
         * stops being an active player, and the knob silently falls back to MEDIA — so the driver
         * turns the knob during the gap and adjusts the wrong group. Navigation, media and telephony
         * do not show this because their audio is continuous while the driver is reaching for the dial.
         *
         * Pacing is against an ABSOLUTE deadline rather than "now + period": drift from a sleeping
         * sweeper would otherwise open exactly the underrun this exists to prevent. Catch-up is capped
         * at one period so a long stall cannot dump a burst of silence into the track.
         *
         * Only for purposes whose audio is genuinely bursty ([Purpose.keepAlive]) — filling silence on
         * a continuous stream would just waste writes, and filling it on CALL would hold the telephony
         * context active after the call, which is the bug the short idle windows exist to fix.
         *
         * TWO clocks, and which bound each gates is the whole of A6:
         *   - LOWER bound on [lastAuAt] (last DELIVERED AU): fill only once the stream has stopped
         *     feeding the track. iOS streams silent AUs at realtime through Siri's prompt→answer
         *     pause, so gating this on the loud clock — as it was — double-fed the track (stream
         *     30 ms per 30 ms + this 200 ms per 200 ms) and dropped 17 % of the turn.
         *   - UPPER bound on [lastAudioAt] (last LOUD frame): past ASSISTANT_HOLD_MS [assistantTick]
         *     has told AacPlayer that Siri is done and media is coming back, and MUSIC outranks
         *     VOICE_COMMAND in CarVolume's priority list — so silence-filling beyond it cannot win
         *     the knob and only keeps a would-be-idle player active while the sink waits to be swept.
         *     That edge must ignore streamed silence, so it stays on the loud clock.
         */
        fun keepAlive(now: Long) {
            if (!p.keepAlive || !isConfigured) return
            // A focus-paused track must STAY paused: the play() below would otherwise undo the pause
            // within one tick and make the focus handling a silent no-op.
            if (focusState != AudioManager.AUDIOFOCUS_GAIN) return
            if (now - lastAuAt < KEEPALIVE_AFTER_MS || now - lastAudioAt > ASSISTANT_HOLD_MS) return
            if (now < nextSilenceAt) return
            // tryLock, never lock: if the consume thread holds it, it is inside feed() writing the
            // stream's own PCM to this track right now — the keep-alive's job, done for it — and the
            // sweeper must not park behind a blocking write. nextSilenceAt is advanced only once we
            // hold the lock, so a skipped tick retries next tick rather than a period later.
            if (!lock.tryLock()) return
            try {
                val tk = track ?: return
                nextSilenceAt = if (nextSilenceAt == 0L || now - nextSilenceAt > KEEPALIVE_PERIOD_MS)
                    now + KEEPALIVE_PERIOD_MS else nextSilenceAt + KEEPALIVE_PERIOD_MS
                runCatching {
                    if (tk.playState != AudioTrack.PLAYSTATE_PLAYING) tk.play()
                    val frames = rate * KEEPALIVE_PERIOD_MS / 1000
                    val buf = ByteArray(frames.toInt() * 2 * maxOf(channels, 1))
                    // NON-blocking is right here even though feed() blocks: a short silence write is
                    // harmless (the next one tops up), and the sweeper serves every sink.
                    val w = tk.write(buf, 0, buf.size, AudioTrack.WRITE_NON_BLOCKING)
                    if (w > 0) { silenceWrites++; silenceWritesTotal.incrementAndGet() }
                    keepAliveFailLogged = false
                }.onFailure {
                    nextSilenceAt = 0L
                    // A keep-alive that throws every tick is the volume knob falling back to media
                    // mid-Siri — the bug this exists to prevent — and it was invisible. Once per run
                    // of failures, never per tick.
                    if (!keepAliveFailLogged) {
                        keepAliveFailLogged = true
                        log.w("${p.label}: keep-alive write threw ${it.javaClass.simpleName}: ${it.message} — the track may drain and lose the knob")
                    }
                }
            } finally {
                lock.unlock()
            }
        }

        /**
         * A DISTINCT listener instance per purpose is load-bearing, not tidiness: AAOS
         * CarAudioFocus keys on listener IDENTITY, so one shared object cannot hold focus for two
         * usages at once.
         */
        private val focusListener = AudioManager.OnAudioFocusChangeListener { change ->
            val from = focusState
            focusState = change
            log.i("${p.label}: focus ${focusName(from)} -> ${focusName(change)}")
            // Pause, never release: the per-purpose idle window still owns teardown, and releasing from
            // the focus callback would race the consume thread that owns this codec and track.
            val t = track
            if (t != null) runCatching {
                when (change) {
                    AudioManager.AUDIOFOCUS_LOSS, AudioManager.AUDIOFOCUS_LOSS_TRANSIENT -> t.pause()
                    AudioManager.AUDIOFOCUS_GAIN -> t.play()
                    else -> Unit
                }
            }.onFailure { log.w("${p.label}: focus pause/resume: ${it.javaClass.simpleName}: ${it.message}") }
            // A paused track cannot reach FIRST AUDIO OUT, and that is not a fault of ours: drop the
            // expectation rather than let it fire, and re-arm it if focus comes back first.
            if (!firstOutSeen && isConfigured) when (change) {
                AudioManager.AUDIOFOCUS_LOSS, AudioManager.AUDIOFOCUS_LOSS_TRANSIENT ->
                    SessionTrace.cancel(expectName, "${p.label} paused by ${focusName(change)} before its first audio out — not a fault")
                AudioManager.AUDIOFOCUS_GAIN -> {
                    SessionTrace.expect(expectName, VOICE_FIRST_OUT_MS,
                        "${p.label} regained focus with no audio out yet, so the resumed track should start consuming")
                    // This thread reads firstOutSeen without the lock. If markFirstOut() (consume
                    // thread or sweeper, under it) landed between the check above and the arm, the
                    // met() it issued found nothing and the arm just made would fire a false miss.
                    // Re-check after arming: an expectation for a step that has happened is met now.
                    if (firstOutSeen) SessionTrace.met(expectName)
                }
                else -> Unit
            }
            if (isConfigured) SessionTrace.Board.up(boardName, boardDetail())
        }

        /** Last focus state seen, so every transition logs old -> new and gates [keepAlive]. */
        @Volatile private var focusState = AudioManager.AUDIOFOCUS_GAIN

        fun configure(r: Int, c: Int) {
            val now = android.os.SystemClock.elapsedRealtime()
            if (configureFailedAt != 0L && now - configureFailedAt < CONFIGURE_RETRY_MS) return
            var mc: MediaCodec? = null
            var tk: AudioTrack? = null
            try {
                val attrs = AudioAttributes.Builder()
                    .setUsage(p.usage).setContentType(p.content).build()

                // Focus BEFORE the track exists: AAOS picks the volume group from active players, so
                // a track that starts without focus can claim the group before the request lands.
                val gain = if (p == Purpose.NAV) AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_MAY_DUCK
                           else AudioManager.AUDIOFOCUS_GAIN_TRANSIENT
                val req = AudioFocusRequest.Builder(gain)
                    .setAudioAttributes(attrs)
                    .setOnAudioFocusChangeListener(focusListener)
                    .build()
                val fr = am.requestAudioFocus(req)
                // The result was discarded until 2026-09-10. This head unit enforces focus at the
                // HAL (`Use hal ducking signals true`), so a refused request is a sink AAOS may keep
                // muted — Siri "playing" into silence with every other line healthy.
                if (fr != AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
                    log.w("${p.label}: focus request (usage=${p.usage}) result=$fr, not GRANTED — AAOS may mute this sink")
                }
                focus = req
                focusState = AudioManager.AUDIOFOCUS_GAIN   // a rebuilt sink must not inherit a stale loss

                val mask = if (c >= 2) AudioFormat.CHANNEL_OUT_STEREO else AudioFormat.CHANNEL_OUT_MONO
                val minBuf = AudioTrack.getMinBufferSize(r, mask, AudioFormat.ENCODING_PCM_16BIT)
                val frameBytes = 2 * (if (c >= 2) 2 else 1)
                val bufBytes = maxOf(minBuf * 4, (r * TRACK_BUFFER_MIN_MS / 1000).toInt() * frameBytes)
                tk = AudioTrack.Builder()
                    .setAudioAttributes(attrs)
                    .setAudioFormat(AudioFormat.Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setSampleRate(r).setChannelMask(mask).build())
                    // 4x minimum OR two keep-alive periods, whichever is larger.
                    //
                    // The old floor was `maxOf(minBuf, 4096) * 4`, which is RATE-BLIND. At 16 kHz mono
                    // the 4096 constant dominated and bought ~512 ms of buffer. At 48 kHz mono
                    // getMinBufferSize dominates instead (~40 ms HAL period), and 4x of that can be
                    // SHORTER than the 200 ms of silence each keep-alive tick writes — so the
                    // non-blocking write truncates every tick and the track underruns between them,
                    // which is precisely the drained-track state the keep-alive exists to prevent.
                    // Sizing in milliseconds makes the floor hold at any negotiated rate.
                    //
                    // AUDIO_OUTPUT_FLAG_FAST is denied to third-party apps on this head unit, so
                    // PERFORMANCE_MODE_LOW_LATENCY buys nothing and can add jitter.
                    .setBufferSizeInBytes(bufBytes)
                    .setTransferMode(AudioTrack.MODE_STREAM)
                    .build()
                    .also { t -> try { t.play() } catch (e: Throwable) { runCatching { t.release() }; throw e } }

                val fmt = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_AAC, r, c)
                fmt.setInteger(MediaFormat.KEY_AAC_PROFILE,
                    android.media.MediaCodecInfo.CodecProfileLevel.AACObjectELD)
                fmt.setByteBuffer("csd-0", ByteBuffer.wrap(eldCsd(r, c)))
                mc = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_AUDIO_AAC)
                mc.configure(fmt, null, null, 0)
                mc.start()

                codec = mc; track = tk; rate = r; channels = c
                isConfigured = true
                configureFailedAt = 0L
                lastAudioAt = now
                lastAuAt = now
                configuredAt = now
                trackBufferMs = (bufBytes.toLong() * 1000 / (r.toLong() * frameBytes)).toInt()
                firstOutSeen = false
                log.i("${p.label}: AAC-ELD ${r}Hz ${c}ch -> AudioTrack(usage=${p.usage}, buffer ${trackBufferMs}ms), decoder=${mc.name}")
                SessionTrace.Board.up(boardName, boardDetail())
                // The caller feeds the AU that triggered this configure immediately after, so the
                // track has PCM from now on; what can still fail silently is the HAL never starting
                // to consume it. Met by [markFirstOut]; cancelled by [release] and by a focus loss.
                SessionTrace.expect(expectName, VOICE_FIRST_OUT_MS,
                    "the ${p.label} sink configured (${r}Hz ${c}ch, ${trackBufferMs}ms pre-fill) with an AU " +
                    "in hand, so AudioFlinger should start consuming once the pre-fill is written")
            } catch (e: Throwable) {
                // Throwable, not Exception: an OutOfMemoryError here is an Error, and letting it
                // escape leaks the native codec AND leaves the backoff unarmed.
                // deliberate, all three: discarding half-built resources.
                runCatching { mc?.release() }
                runCatching { tk?.release() }
                runCatching { focus?.let { am.abandonAudioFocusRequest(it) } }
                focus = null
                configureFailedAt = now
                log.e("${p.label}: configure failed (retry in ${CONFIGURE_RETRY_MS}ms): ${e.message}")
                // note(), not failed(): the E above is the severity.
                SessionTrace.Board.note(boardName, "FAILED (configure threw ${e.javaClass.simpleName} — ${p.label} audio dropped until the ${CONFIGURE_RETRY_MS}ms retry)")
            }
        }

        /**
         * Decode one AU and write its PCM to the track. Consume thread only.
         *
         * The write is BLOCKING. With [keepAlive] gated on [lastAuAt] the track is fed at exactly
         * its drain rate, so a blocking write parks for at most one HAL period (~20 ms) and loses
         * nothing; the non-blocking write it replaced discarded the remainder of every AU that
         * arrived while the buffer was full, which under the A6 double-feed was every other AU.
         * A blocking write on a PAUSED or stopped track turns non-blocking and returns short
         * (`AudioTrack::obtainBuffer`: "Non-blocking if track is stopped or paused"), so `w == 0`
         * here means the focus listener paused us and discarding IS correct — counted apart from
         * real drops so a focus loss mid-turn cannot masquerade as the A6 defect. `w < 0` is
         * ERROR_DEAD_OBJECT and rebuilds, unchanged.
         *
         * Whole body under [lock]: the codec's mapped buffers and the track are both touched, and
         * [release] can arrive from `cp-voice-sweep` at any AU boundary.
         */
        fun feed(au: ByteArray) {
            // The AU clock advances for EVERY delivered AU, before the decode, so a decode fault
            // cannot leave the keep-alive believing the stream has stopped.
            lastAuAt = android.os.SystemClock.elapsedRealtime()
            ausFed++
            lock.lock()
            try { feedLocked(au) } finally { lock.unlock() }
        }

        private fun feedLocked(au: ByteArray) {
            val c = codec ?: return
            val t = track ?: return
            try {
                val inIdx = c.dequeueInputBuffer(10_000)
                if (inIdx >= 0) {
                    // Once dequeued the index MUST be queued back on every path. Input buffers are a
                    // fixed pool (4-8); leaking them makes dequeueInputBuffer return TRY_AGAIN_LATER
                    // forever — the sink goes silent with no error and every later AU pays the full
                    // 10 ms timeout.
                    var size = 0
                    try {
                        val ib = c.getInputBuffer(inIdx)
                        if (ib != null) {
                            ib.clear()
                            if (au.size <= ib.remaining()) { ib.put(au); size = au.size }
                            else {
                                ausDroppedFull++; ausDropped.incrementAndGet()
                                log.w("${p.label}: AU ${au.size} > input buffer ${ib.remaining()} — dropped")
                            }
                        }
                    } finally {
                        c.queueInputBuffer(inIdx, 0, size, 0, 0)
                    }
                }
                val info = MediaCodec.BufferInfo()
                while (true) {
                    val o = c.dequeueOutputBuffer(info, 0)
                    if (o < 0) break
                    val ob = c.getOutputBuffer(o)
                    if (ob != null && info.size > 0) {
                        val pcm = ByteArray(info.size)
                        ob.position(info.offset); ob.get(pcm)
                        if (peakExceeds(pcm)) { lastAudioAt = android.os.SystemClock.elapsedRealtime(); onDuck(true) }
                        else if (lastAudioAt == 0L) lastAudioAt = android.os.SystemClock.elapsedRealtime()
                        var off = 0
                        while (off < pcm.size) {
                            val w = t.write(pcm, off, pcm.size - off, AudioTrack.WRITE_BLOCKING)
                            if (w < 0) {
                                // ERROR_DEAD_OBJECT (audioserver restart / route change) is routine on
                                // a head unit, and a long-idle voice track is what provokes HAL standby.
                                // Rebuild rather than writing into the void silently forever.
                                log.w("${p.label}: write -> $w; rebuilding the track")
                                release("AudioTrack.write returned $w — rebuilding on the next AU"); configureFailedAt = 0L
                                return
                            }
                            if (w == 0) {
                                // A blocking write returns 0 only when the track is not PLAYING —
                                // the focus listener paused it (it sets focusState BEFORE pause(),
                                // and the Java playState flips after the native one, so test both).
                                // Discarding is right; count it apart from a real drop.
                                if (focusState != AudioManager.AUDIOFOCUS_GAIN ||
                                    t.playState != AudioTrack.PLAYSTATE_PLAYING) {
                                    ausDiscardedPaused++; ausDiscardedPausedTotal.incrementAndGet()
                                } else {
                                    // Should now be unreachable on a playing track. Counted, because a
                                    // sink that drops half of every AU is otherwise indistinguishable
                                    // from a healthy one — this is the A6 signal.
                                    ausDroppedFull++; ausDropped.incrementAndGet()
                                }
                                break
                            }
                            off += w
                        }
                    }
                    c.releaseOutputBuffer(o, false)
                }
                // A10 instrumentation: the first time AudioFlinger has consumed anything from this
                // track. The distance from configure is the pre-fill cost the buffer floor imposes.
                // One volatile read per AU until it fires, then a constant-false branch.
                if (!firstOutSeen && t.playbackHeadPosition > 0) markFirstOut()
            } catch (e: Exception) {
                log.e("${p.label}: feed ${e.javaClass.simpleName}: ${e.message}")
                if (e is MediaCodec.CodecException || e is IllegalStateException) {
                    release("feed threw ${e.javaClass.simpleName} — rebuilt after ${CONFIGURE_RETRY_MS}ms backoff")
                    // Arm the backoff, or the next AU reconfigures immediately and a persistent
                    // fault rebuilds a codec + track per frame.
                    configureFailedAt = android.os.SystemClock.elapsedRealtime()
                }
            }
        }

        /**
         * pause + flush BEFORE abandoning focus, so AAOS sees no active player of this usage.
         *
         * Callable from `cp-voice-sweep` ([sweepIdle]) or the consume thread ([consume]'s finally,
         * the format-change branch of [route], and [feed]'s own dead-object path, where [lock] is
         * already held and re-enters). The `pause()` BEFORE taking the lock is deliberate: it
         * interrupts a consume thread parked in a blocking write (`AudioTrack::pause` wakes the
         * proxy futex and the write returns short), so this never waits longer than one AU's decode.
         *
         * [why] is recorded on the [SessionTrace.Board] and on the first-audio-out expectation if
         * that is still armed — a sink released on its idle timer is a state, not a failure.
         */
        fun release(why: String) {
            runCatching { track?.pause() }   // deliberate: the interrupt described above; nothing to report
            lock.lock()
            try {
                val wasConfigured = isConfigured
                val t = track; track = null
                underruns = runCatching { t?.underrunCount }.getOrNull() ?: underruns
                // deliberate, all six: pause → flush → stop → release, then abandon — each wrapped on
                // its own so a throw never skips the focus abandon that frees the volume group.
                runCatching { t?.pause() }; runCatching { t?.flush() }
                runCatching { t?.stop() }; runCatching { t?.release() }
                val c = codec; codec = null
                runCatching { c?.stop() }; runCatching { c?.release() }
                runCatching { focus?.let { am.abandonAudioFocusRequest(it) } }
                focus = null
                rate = 0; channels = 0; isConfigured = false
                if (wasConfigured) {
                    if (!firstOutSeen) SessionTrace.cancel(expectName, "${p.label} released first — $why")
                    SessionTrace.Board.down(boardName, why)
                }
            } finally {
                lock.unlock()
            }
        }
    }

    // ---- helpers -----------------------------------------------------------------------------

    private fun focusName(c: Int) = when (c) {
        AudioManager.AUDIOFOCUS_GAIN -> "GAIN"
        AudioManager.AUDIOFOCUS_LOSS -> "LOSS"
        AudioManager.AUDIOFOCUS_LOSS_TRANSIENT -> "LOSS_TRANSIENT"
        AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK -> "LOSS_TRANSIENT_CAN_DUCK"
        else -> "state=$c"
    }

    /** int16 peak over the frame; ~-32 dBFS separates real speech from iOS's idle silence. */
    private fun peakExceeds(pcm: ByteArray): Boolean {
        var i = 0
        while (i + 1 < pcm.size) {
            val s = ((pcm[i + 1].toInt() shl 8) or (pcm[i].toInt() and 0xFF)).toShort().toInt()
            if (s > DUCK_PEAK_THRESHOLD || s < -DUCK_PEAK_THRESHOLD) return true
            i += 2
        }
        return false
    }

    private fun be32(b: ByteArray, off: Int): Int =
        ((b[off].toInt() and 0xFF) shl 24) or ((b[off + 1].toInt() and 0xFF) shl 16) or
        ((b[off + 2].toInt() and 0xFF) shl 8) or (b[off + 3].toInt() and 0xFF)

    private fun readFully(ins: InputStream, buf: ByteArray, n: Int): Boolean {
        var off = 0
        while (off < n) {
            val r = try { ins.read(buf, off, n - off) } catch (e: Exception) {
                if (running.get()) log.e("read: ${e.message}"); return false
            }
            if (r <= 0) return false
            off += r
        }
        return true
    }
}
