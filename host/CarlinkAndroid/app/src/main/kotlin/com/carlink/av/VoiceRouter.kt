package com.carlink.av

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
import android.media.MediaCodec
import android.media.MediaFormat
import com.carlink.logging.ProbeLog
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
 * usage → context → volume group → bus. See `docs/carplay/06_AV_PIPELINE.md`.
 *
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

    val framesDecoded = AtomicLong(0)
    val bytesIn = AtomicLong(0)

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
        val usage: Int,
        val content: Int,
        val label: String,
        val idleMs: Long,
        /** Fill gaps with silence to stay an "active player" — see [Sink.keepAlive]. */
        val keepAlive: Boolean = false,
    ) {
        // bus4_call_out — shortest: a lingering CALL context is the most disruptive to the knob.
        // No keep-alive: call audio is continuous, and filling silence would hold the telephony
        // context active after the call ends, which is exactly the stuck-knob bug.
        CALL(AudioAttributes.USAGE_VOICE_COMMUNICATION, AudioAttributes.CONTENT_TYPE_SPEECH, "call", 3_000L),

        // bus2_voice_command_out — longest window AND keep-alive: Siri goes quiet between prompt and
        // answer, and a drained track stops being an active player, which is what makes the head
        // unit's volume knob ignore Siri and adjust media instead.
        ASSISTANT(
            AudioAttributes.USAGE_ASSISTANT,
            AudioAttributes.CONTENT_TYPE_SPEECH,
            "siri",
            15_000L,
            keepAlive = true,
        ),

        // bus3_call_ring_out — UNPROVEN on this head unit, see docs/carplay/03_SDK_GROUND_TRUTH.md §1
        ALERT(
            AudioAttributes.USAGE_VOICE_COMMUNICATION_SIGNALLING,
            AudioAttributes.CONTENT_TYPE_SONIFICATION,
            "alert",
            3_000L,
        ),

        // bus1_navigation_out — continuous while speaking, so no keep-alive needed; this is why nav
        // volume was already adjustable and Siri was not.
        NAV(
            AudioAttributes.USAGE_ASSISTANCE_NAVIGATION_GUIDANCE,
            AudioAttributes.CONTENT_TYPE_SPEECH,
            "nav",
            8_000L,
        ),
    }

    private companion object {
        /**
         * AAC-ELD AudioSpecificConfig as the shipping fdk-aac encoder actually emits it
         * (`ccpa_custom/docs/50`): SBR enabled by fdk auto-mode, frameLength 480. NOT the
         * `f8f03000` the older docs claim. `:9003` carries RAW access units with no ADTS header, so
         * MediaCodec has to be handed this or it cannot configure at all.
         */
        val ELD_CSD_16K_MONO =
            byteArrayOf(
                0xF8.toByte(),
                0xF0.toByte(),
                0x31,
                0x2C,
                0x00,
                0xBC.toByte(),
                0x00,
            )

        /** samplingFrequencyIndex per ISO/IEC 14496-3 Table 1.16. */
        val SF_INDEX =
            intArrayOf(
                96000,
                88200,
                64000,
                48000,
                44100,
                32000,
                24000,
                22050,
                16000,
                12000,
                11025,
                8000,
                7350,
            )

        /** Never re-attempt configure per access unit — that drains the codec pool in seconds. */
        const val CONFIGURE_RETRY_MS = 5_000L

        /** Duck trigger. iOS streams CONTINUOUS DIGITAL SILENCE on idle voice streams, so a
         *  packet-flow trigger ducks media permanently from session start. Gate on energy. */
        const val DUCK_PEAK_THRESHOLD = 800

        // A single global "stop a sink that has gone quiet" window used to live here. Superseded by
        // Purpose.idleMs: one window across all purposes let CALL/MEDIA/ASSISTANT contexts stay active
        // simultaneously and confused the head unit's volume knob.

        /** Sweeper cadence. Must be well under the AudioTrack buffer so keep-alive never underruns. */
        const val KEEPALIVE_TICK_MS = 100L

        /** Silence written per keep-alive write. Two ticks of headroom against a late thread. */
        const val KEEPALIVE_PERIOD_MS = 200L

        /** Only fill once the real stream has actually paused — not between consecutive AUs. */
        const val KEEPALIVE_AFTER_MS = 250L

        /**
         * How long after Siri's last audio we keep media paused. Must exceed AAOS's ~3 s
         * `audioVolumeKeyEventTimeoutMs`, or MUSIC re-enters the active set (and re-latches the knob)
         * while the driver is still reaching for the dial.
         */
        const val ASSISTANT_HOLD_MS = 4_000L

        /** Restore media this long after the last ENERGETIC frame on any voice sink. */
        const val DUCK_RELEASE_MS = 1_500L
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
    private fun eldCsd(
        rate: Int,
        channels: Int,
    ): ByteArray {
        if (rate == 16000 && channels == 1) return ELD_CSD_16K_MONO
        val fi = SF_INDEX.indexOf(rate).let { if (it < 0) 3 else it } // default 48 kHz
        val ch = channels.coerceIn(1, 2)
        // 11111 (esc) 000111 (39-32) | fi(4) | ch(4) | ELDSpecificConfig: frameLengthFlag=0,
        // aacSectionDataResilience=0, aacScalefactorDataResilience=0, aacSpectralDataResilience=0,
        // ldSbrPresentFlag=0, then a 4-bit ELDEXT_TERM(0).
        var acc = 0L
        var bits = 0

        fun put(
            v: Int,
            n: Int,
        ) {
            acc = (acc shl n) or (v.toLong() and ((1L shl n) - 1))
            bits += n
        }
        put(31, 5)
        put(39 - 32, 6)
        put(fi, 4)
        put(ch, 4)
        // frameLengthFlag = 1 => 480 samples. NOT 0/512: every proven ELD config in this project
        // is 480 (the macOS decoder hardcodes it at every rate, and the device-confirmed 16k ASC
        // sets this bit). A frameLength mismatch is a configure failure or garbage output.
        put(1, 1)
        put(0, 1)
        put(0, 1)
        put(0, 1)
        put(0, 1)
        put(0, 4)
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
        sweeper =
            Thread({
                while (running.get()) {
                    try {
                        Thread.sleep(KEEPALIVE_TICK_MS)
                    } catch (_: InterruptedException) {
                        return@Thread
                    }
                    val now = android.os.SystemClock.elapsedRealtime()
                    runCatching { synchronized(sinks) { sinks.values.forEach { it.keepAlive(now) } } }
                    runCatching { assistantTick(now) }
                    runCatching { sweepIdle() }
                }
            }, "cp-voice-sweep").apply {
                isDaemon = true
                start()
            }
    }

    private var sweeper: Thread? = null

    /**
     * FLAG-ONLY teardown, exactly like [AacPlayer.stop].
     *
     * It releases NOTHING. [consume] owns every codec and track and releases them on its own thread
     * in its `finally`. Releasing here — from the UI thread, via onDestroy — while the consume
     * thread is inside `feed()` unmaps the direct input/output ByteBuffers it is writing into. That
     * is a SIGSEGV in libmedia, not a catchable exception, so no runCatching helps: the app dies and
     * takes the session with it, precisely when a driver taps away mid-Siri.
     *
     * Unducking here IS safe and necessary — setDucked is synchronized and idempotent, and the
     * consume thread may never run again to do it.
     */
    fun stop() {
        running.set(false)
        onDuck(false)
        log.i("stopping — ${framesDecoded.get()} AUs, ${bytesIn.get()} bytes")
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
                    log.e(
                        "voice desync: implausible rate ${rate}Hz — the seam is probably speaking " +
                            "the forward-encrypted v2 framing (check OCBM_FWD_ENC on the box)",
                    )
                    break
                }
                if (len < 0 || len > 1 shl 20) {
                    log.e("voice desync: len=$len")
                    break
                }
                if (len == 0) continue
                val au = ByteArray(len)
                if (!readFully(ins, au, len)) break
                bytesIn.addAndGet((11 + len).toLong())
                route(atype, rate, ch, au)
            }
        } catch (t: Throwable) {
            if (running.get()) log.e("consume: ${t.javaClass.simpleName}: ${t.message}")
        } finally {
            synchronized(sinks) {
                sinks.values.forEach { it.release() }
                sinks.clear()
            }
            onDuck(false)
        }
    }

    /*
     * Release sinks whose stream has gone quiet, and drop the duck when none is speaking.
     *
     * This is load-bearing, not housekeeping. iOS streams CONTINUOUS DIGITAL SILENCE on idle voice
     * streams and `:9003` is a persistent socket held across re-SETUPs, so without a sweep:
     *   - the first Siri word or nav prompt of a drive ducks media to 0.2 FOREVER, and
     *   - the first idle telephony packet takes AUDIOFOCUS_GAIN_TRANSIENT with
     *     USAGE_VOICE_COMMUNICATION and never abandons it, which on AAOS pins the hardware volume
     *     keys to the call group and suppresses the head unit's own sources for the rest of the
     *     session.
     * Runs on the consume thread, so it is also the only thread allowed to release.
     */

    /** Edge-detected so the media track is paused/resumed once per Siri turn, not once per tick. */
    private var assistantWasActive = false

    /**
     * Signal Siri's speaking window on its own cadence, independent of [sweepIdle]'s 1 Hz throttle.
     *
     * Runs from the fast sweeper tick: the media track has to be paused BEFORE the driver reaches for
     * the knob, and a second of latency is enough to miss the whole utterance.
     */
    private fun assistantTick(now: Long) {
        val live =
            synchronized(sinks) {
                sinks[Purpose.ASSISTANT]?.let { it.isConfigured && now - it.lastAudioAt < ASSISTANT_HOLD_MS } ?: false
            }
        if (live == assistantWasActive) return
        assistantWasActive = live
        log.i("assistant ${if (live) "SPEAKING — pausing media so the knob can reach the voice group" else "done — media resumes"}")
        runCatching { onAssistant(live) }
    }

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
                if (quiet < DUCK_RELEASE_MS) anyLoud = true
                if (quiet > p.idleMs) dead.add(p) // per-purpose, not one global window — see [Purpose]
            }
            dead.forEach { sinks.remove(it)?.release() }
        }
        dead.forEach { log.i("${it.label}: idle ${it.idleMs / 1000}s — released (focus + volume group freed)") }
        if (!anyLoud) onDuck(false)
    }

    @Volatile private var lastSweep = 0L

    /** Warn once per unknown atype rather than per access unit. */
    private val unroutedLogged = java.util.Collections.synchronizedSet(HashSet<Int>())

    /**
     * `atype` 4 (`default`) is the one value that needs the format to disambiguate: 16 kHz mono is
     * the Siri downlink on type 100, 48 kHz stereo is alt-audio/navigation on type 101. Those two
     * genuinely differ in format, so this split is safe — unlike guessing between 1/2/4.
     */
    private fun purposeFor(
        atype: Int,
        rate: Int,
        ch: Int,
    ): Purpose? =
        when (atype) {
            1 -> Purpose.CALL
            2 -> Purpose.ASSISTANT
            3 -> Purpose.ALERT
            4 -> if (rate >= 44100 && ch >= 2) Purpose.NAV else Purpose.ASSISTANT
            // 0 = media (should never reach :9003) and 5 = compatibility (a PCM media fallback this
            // receiver cannot play). Both were previously swept into the `else` and mis-routed to NAV
            // purely because they happen to be 48 kHz stereo.
            else -> null
        }

    private fun route(
        atype: Int,
        rate: Int,
        ch: Int,
        au: ByteArray,
    ) {
        val p =
            purposeFor(atype, rate, ch) ?: run {
                if (unroutedLogged.add(atype)) log.w("atype $atype (${rate}Hz ${ch}ch) has no sink — dropping")
                return
            }
        val sink =
            synchronized(sinks) {
                sinks.getOrPut(p) { Sink(p).also { it.configure(rate, ch) } }
            }
        if (sink.isConfigured && (sink.rate != rate || sink.channels != ch)) {
            log.i("${p.label}: format changed ${sink.rate}Hz${sink.channels}ch -> ${rate}Hz${ch}ch")
            sink.release()
            sink.configure(rate, ch)
        } else if (!sink.isConfigured) {
            // DIVERGENCE from the gm_ccpa original, deliberate. feed()'s two error paths call
            // release() and promise a rebuild ("rebuilding the track", "the next AU reconfigures
            // immediately"), but neither could deliver it: configure() was reachable only from
            // the getOrPut lambda above, which runs solely for a purpose ABSENT from the map, or
            // from the format-change branch, which requires isConfigured. So a released sink sat
            // in the map with isConfigured=false, feed() early-returned on `codec ?: return`
            // forever, and sweepIdle (which skips unconfigured sinks) never evicted it to let
            // getOrPut rebuild. One ERROR_DEAD_OBJECT — which this file calls routine on a head
            // unit — killed that purpose, e.g. Siri, until the lanes generation retired.
            //
            // Reviving in place rather than evicting from the map is what keeps the two error
            // paths' differing intent intact: configure() gates itself on the sink-local
            // CONFIGURE_RETRY_MS backoff, so the dead-track path (which zeroes it) rebuilds on
            // the next AU while the codec-exception path (which arms it) still waits out its 5 s.
            // Evicting would construct a fresh Sink with a zero backoff and rebuild a codec plus
            // track per frame under a persistent fault — exactly what that backoff exists to stop.
            sink.configure(rate, ch)
        }
        sink.feed(au)
        framesDecoded.incrementAndGet()
    }

    // ---- one sink per purpose ----------------------------------------------------------------

    private inner class Sink(
        val p: Purpose,
    ) {
        @Volatile var rate = 0

        @Volatile var channels = 0

        /** True only after a configure() that fully succeeded; guards the format-change branch. */
        @Volatile var isConfigured = false

        @Volatile var lastAudioAt = 0L

        @Volatile private var codec: MediaCodec? = null

        @Volatile private var track: AudioTrack? = null

        @Volatile private var focus: AudioFocusRequest? = null

        @Volatile private var configureFailedAt = 0L

        /** Absolute deadline for the next keep-alive write; see [keepAlive]. */
        @Volatile private var nextSilenceAt = 0L

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
         */
        fun keepAlive(now: Long) {
            if (!p.keepAlive || !isConfigured) return
            val tk = track ?: return
            // Only between bursts: while audio is flowing the stream is its own keep-alive, and once
            // the idle window expires the sweeper releases us and hands the volume context back.
            val quiet = now - lastAudioAt
            if (quiet < KEEPALIVE_AFTER_MS || quiet > p.idleMs) return
            if (now < nextSilenceAt) return
            nextSilenceAt =
                if (nextSilenceAt == 0L || now - nextSilenceAt > KEEPALIVE_PERIOD_MS) {
                    now + KEEPALIVE_PERIOD_MS
                } else {
                    nextSilenceAt + KEEPALIVE_PERIOD_MS
                }
            runCatching {
                if (tk.playState != AudioTrack.PLAYSTATE_PLAYING) tk.play()
                val frames = rate * KEEPALIVE_PERIOD_MS / 1000
                val buf = ByteArray(frames.toInt() * 2 * maxOf(channels, 1))
                tk.write(buf, 0, buf.size, AudioTrack.WRITE_NON_BLOCKING)
            }.onFailure { nextSilenceAt = 0L }
        }

        /**
         * A DISTINCT listener instance per purpose is load-bearing, not tidiness: AAOS
         * CarAudioFocus keys on listener IDENTITY, so one shared object cannot hold focus for two
         * usages at once.
         */
        private val focusListener =
            AudioManager.OnAudioFocusChangeListener { change ->
                log.i("${p.label}: focus change $change")
            }

        fun configure(
            r: Int,
            c: Int,
        ) {
            val now = android.os.SystemClock.elapsedRealtime()
            if (configureFailedAt != 0L && now - configureFailedAt < CONFIGURE_RETRY_MS) return
            var mc: MediaCodec? = null
            var tk: AudioTrack? = null
            try {
                val attrs =
                    AudioAttributes
                        .Builder()
                        .setUsage(p.usage)
                        .setContentType(p.content)
                        .build()

                // Focus BEFORE the track exists: AAOS picks the volume group from active players, so
                // a track that starts without focus can claim the group before the request lands.
                val gain =
                    if (p == Purpose.NAV) {
                        AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_MAY_DUCK
                    } else {
                        AudioManager.AUDIOFOCUS_GAIN_TRANSIENT
                    }
                val req =
                    AudioFocusRequest
                        .Builder(gain)
                        .setAudioAttributes(attrs)
                        .setOnAudioFocusChangeListener(focusListener)
                        .build()
                am.requestAudioFocus(req)
                focus = req

                val mask = if (c >= 2) AudioFormat.CHANNEL_OUT_STEREO else AudioFormat.CHANNEL_OUT_MONO
                val minBuf = AudioTrack.getMinBufferSize(r, mask, AudioFormat.ENCODING_PCM_16BIT)
                tk =
                    AudioTrack
                        .Builder()
                        .setAudioAttributes(attrs)
                        .setAudioFormat(
                            AudioFormat
                                .Builder()
                                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                                .setSampleRate(r)
                                .setChannelMask(mask)
                                .build(),
                        )
                        // 4x minimum. AUDIO_OUTPUT_FLAG_FAST is denied to third-party apps on this head
                        // unit, so PERFORMANCE_MODE_LOW_LATENCY buys nothing and can add jitter.
                        .setBufferSizeInBytes(maxOf(minBuf, 4096) * 4)
                        .setTransferMode(AudioTrack.MODE_STREAM)
                        .build()
                        .also { t ->
                            try {
                                t.play()
                            } catch (e: Throwable) {
                                runCatching { t.release() }
                                throw e
                            }
                        }

                val fmt = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_AAC, r, c)
                fmt.setInteger(
                    MediaFormat.KEY_AAC_PROFILE,
                    android.media.MediaCodecInfo.CodecProfileLevel.AACObjectELD,
                )
                fmt.setByteBuffer("csd-0", ByteBuffer.wrap(eldCsd(r, c)))
                mc = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_AUDIO_AAC)
                mc.configure(fmt, null, null, 0)
                mc.start()

                codec = mc
                track = tk
                rate = r
                channels = c
                isConfigured = true
                configureFailedAt = 0L
                lastAudioAt = now
                log.i("${p.label}: AAC-ELD ${r}Hz ${c}ch -> AudioTrack(usage=${p.usage}), decoder=${mc.name}")
            } catch (e: Throwable) {
                // Throwable, not Exception: an OutOfMemoryError here is an Error, and letting it
                // escape leaks the native codec AND leaves the backoff unarmed.
                runCatching { mc?.release() }
                runCatching { tk?.release() }
                runCatching { focus?.let { am.abandonAudioFocusRequest(it) } }
                focus = null
                configureFailedAt = now
                log.e("${p.label}: configure failed (retry in ${CONFIGURE_RETRY_MS}ms): ${e.message}")
            }
        }

        fun feed(au: ByteArray) {
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
                            if (au.size <= ib.remaining()) {
                                ib.put(au)
                                size = au.size
                            } else {
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
                        ob.position(info.offset)
                        ob.get(pcm)
                        if (peakExceeds(pcm)) {
                            lastAudioAt = android.os.SystemClock.elapsedRealtime()
                            onDuck(true)
                        } else if (lastAudioAt == 0L) {
                            lastAudioAt = android.os.SystemClock.elapsedRealtime()
                        }
                        var off = 0
                        while (off < pcm.size) {
                            val w = t.write(pcm, off, pcm.size - off, AudioTrack.WRITE_NON_BLOCKING)
                            if (w < 0) {
                                // ERROR_DEAD_OBJECT (audioserver restart / route change) is routine on
                                // a head unit, and a long-idle voice track is what provokes HAL standby.
                                // Rebuild rather than writing into the void silently forever.
                                log.w("${p.label}: write -> $w; rebuilding the track")
                                release()
                                configureFailedAt = 0L
                                return
                            }
                            if (w == 0) break // buffer full: drop the remainder, do not spin
                            off += w
                        }
                    }
                    c.releaseOutputBuffer(o, false)
                }
            } catch (e: Exception) {
                log.e("${p.label}: feed ${e.javaClass.simpleName}: ${e.message}")
                if (e is MediaCodec.CodecException || e is IllegalStateException) {
                    release()
                    // Arm the backoff, or the next AU reconfigures immediately and a persistent
                    // fault rebuilds a codec + track per frame.
                    configureFailedAt = android.os.SystemClock.elapsedRealtime()
                }
            }
        }

        /** pause + flush BEFORE abandoning focus, so AAOS sees no active player of this usage. */
        fun release() {
            val t = track
            track = null
            runCatching { t?.pause() }
            runCatching { t?.flush() }
            runCatching { t?.stop() }
            runCatching { t?.release() }
            val c = codec
            codec = null
            runCatching { c?.stop() }
            runCatching { c?.release() }
            runCatching { focus?.let { am.abandonAudioFocusRequest(it) } }
            focus = null
            rate = 0
            channels = 0
            isConfigured = false
        }
    }

    // ---- helpers -----------------------------------------------------------------------------

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

    private fun be32(
        b: ByteArray,
        off: Int,
    ): Int =
        ((b[off].toInt() and 0xFF) shl 24) or ((b[off + 1].toInt() and 0xFF) shl 16) or
            ((b[off + 2].toInt() and 0xFF) shl 8) or (b[off + 3].toInt() and 0xFF)

    private fun readFully(
        ins: InputStream,
        buf: ByteArray,
        n: Int,
    ): Boolean {
        var off = 0
        while (off < n) {
            val r =
                try {
                    ins.read(buf, off, n - off)
                } catch (e: Exception) {
                    if (running.get()) log.e("read: ${e.message}")
                    return false
                }
            if (r <= 0) return false
            off += r
        }
        return true
    }
}
