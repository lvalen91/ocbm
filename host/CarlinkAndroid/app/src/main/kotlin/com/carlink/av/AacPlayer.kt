package com.carlink.av

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.media.MediaCodec
import android.media.MediaFormat
import com.carlink.logging.ProbeLog
import java.io.InputStream
import java.nio.ByteBuffer
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * AAC-LC decoder + [AudioTrack] playback for the CarPlay media stream.
 *
 * The seam delivers ADTS (`forward.rs` wraps the AUs), device-confirmed as AAC-LC 48 kHz stereo, which
 * matches what the SETUP dict negotiates (`audioFormat=0x800000`). ADTS carries its own header, so the
 * decoder is configured from the first frame's header rather than from `/info` — if iOS ever picks a
 * different rate the stream stays self-describing.
 *
 * Playback uses `USAGE_MEDIA`, so it routes through the head unit's normal media path and obeys the
 * volume knob. `AudioTrack` is created after the decoder reports its output format, because the channel
 * mask must match what the decoder actually produces, not what ADTS advertised.
 *
 * ## Media must HOLD audio focus, or the volume knob gets stuck
 *
 * [am] is not optional in practice. AAOS points the hardware volume control at the current audio-focus
 * owner, and it returns focus to the **previous owner** when a transient holder abandons it. VoiceRouter
 * takes `AUDIOFOCUS_GAIN_TRANSIENT` for Siri and calls; if media never held focus there is nothing to
 * hand back to, so after a call or a Siri turn the knob stays pointed at Phone/Siri while media plays —
 * the head unit's volume UI then adjusts a group the driver cannot hear. Holding `AUDIOFOCUS_GAIN` here
 * gives the system a resting owner to return to. It also lets AAOS duck us properly for navigation
 * rather than relying solely on our own software ducking in [setDucked].
 */
class AacPlayer(
    private val am: android.media.AudioManager? = null,
) {
    private val log = ProbeLog.sub("aac")
    private val running = AtomicBoolean(false)

    // @Volatile is load-bearing, not decoration: `track` is read cross-thread by stop() for the
    // pause/flush unblock below, and without it the UI thread may legally observe a stale null — the
    // consume thread's write has no happens-before edge to it — and silently pause nothing.
    @Volatile private var codec: MediaCodec? = null

    @Volatile private var track: AudioTrack? = null

    @Volatile private var configureFailedAt = 0L

    // Remembered so a dead AudioTrack can be rebuilt in place without waiting for the next configure.
    @Volatile private var cfgRate = 48000

    @Volatile private var cfgChannels = 2

    val framesDecoded = AtomicLong(0)
    val bytesIn = AtomicLong(0)

    private val rates = intArrayOf(96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350)

    private companion object {
        /** Never re-attempt configure per ADTS frame — that drains the codec pool in seconds. */
        const val CONFIGURE_RETRY_MS = 5_000L
    }

    @Volatile private var focus: android.media.AudioFocusRequest? = null

    @Volatile private var pausedForAssistant = false

    /**
     * Pause playback outright while Siri speaks, instead of only ducking.
     *
     * Ducking is not enough for the head unit's volume knob: AAOS chooses the knob's target from
     * *active players*, a ducked track is still active, and MUSIC outranks VOICE_COMMAND in this
     * unit's priority list — so the knob showed "Audio" even mid-Siri. Pausing removes MUSIC from the
     * active set and lets the voice group win. See VoiceRouter's class docs for the decompiled detail.
     *
     * `pause()` without `flush()`: the buffered media survives, so resume continues where it left off
     * rather than dropping a second of audio. The seam keeps delivering throughout; AudioTrack simply
     * stops consuming, and the consume loop's writes block harmlessly until we resume.
     */
    fun setAssistantSpeaking(speaking: Boolean) {
        if (pausedForAssistant == speaking) return
        pausedForAssistant = speaking
        val t = track ?: return
        runCatching {
            if (speaking) {
                t.pause()
                log.i("media paused for the assistant")
            } else {
                t.play()
                log.i("media resumed after the assistant")
            }
        }.onFailure { log.w("assistant pause/resume: ${it.javaClass.simpleName}: ${it.message}") }
    }

    fun start() {
        configureFailedAt = 0L
        running.set(true)
        requestFocus()
    }

    /**
     * Take permanent media focus. Idempotent — [start] may be called again on a re-launched screen.
     *
     * Deliberately NOT tied to whether audio is currently flowing: the point is to be the resting focus
     * owner that AAOS returns to when Siri or a call abandons its transient focus. Releasing it between
     * tracks would recreate the stuck-knob bug in the gaps.
     */
    private fun requestFocus() {
        val mgr =
            am ?: run {
                log.w("no AudioManager — media will not hold focus; the volume knob may stick on Phone/Siri")
                return
            }
        if (focus != null) return
        runCatching {
            val req =
                android.media.AudioFocusRequest
                    .Builder(android.media.AudioManager.AUDIOFOCUS_GAIN)
                    .setAudioAttributes(
                        AudioAttributes
                            .Builder()
                            .setUsage(AudioAttributes.USAGE_MEDIA)
                            .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                            .build(),
                    )
                    // We already duck in software from VoiceRouter, and CarPlay expects the phone to remain
                    // the mixing authority — so do not let the system pause us behind our back.
                    .setWillPauseWhenDucked(false)
                    .build()
            val r = mgr.requestAudioFocus(req)
            focus = req
            log.i("media audio focus: ${if (r == android.media.AudioManager.AUDIOFOCUS_REQUEST_GRANTED) "GRANTED" else "result=$r"}")
        }.onFailure { log.w("media focus request failed: ${it.javaClass.simpleName}: ${it.message}") }
    }

    private fun abandonFocus() {
        val mgr = am ?: return
        focus?.let { runCatching { mgr.abandonAudioFocusRequest(it) } }
        focus = null
    }

    /**
     * Build the [AudioTrack] before any audio arrives, so the first ADTS frame is decoded straight
     * into a track that is already playing.
     *
     * Safe to do speculatively because the format is not a guess: CarPlay's audio ceiling is a
     * WIRE-FORMAT limit of stereo AAC-LC 48 kHz (`kAirPlayAudioFormat_*` has no entry above 2
     * channels), the SETUP dict negotiates exactly that, and every output thread on this head unit
     * reports 48 kHz stereo. The decoder is deliberately NOT pre-configured — [configure] still
     * builds it from the first frame's own ADTS header, so a surprise rate still works; it simply
     * reuses this track instead of building one.
     *
     * Measured saving: ~134 ms of the 172 ms seam-connect -> first-audio path (2026-08-12).
     */
    fun prime() {
        if (track != null) return
        runCatching { buildTrack(cfgRate, cfgChannels) }
            .onSuccess {
                track = it
                log.i("primed AudioTrack ${cfgRate}Hz ${cfgChannels}ch — waiting for the stream")
            }.onFailure { log.w("prime failed (harmless; configure will build one): ${it.message}") }
    }

    /*
     * Flag-only teardown, plus the one cross-thread call audio genuinely needs.
     *
     * It releases NOTHING: [consume] owns both resources and releases them on its own thread. Releasing
     * a MediaCodec from here races a live `feed()` — best case a message-less IllegalStateException,
     * worst case a native crash, since `release()` unmaps the direct input ByteBuffer `feed()` may be
     * writing into. And nulling the fields here is exactly what let the still-running loop observe
     * `codec == null` and rebuild a codec + playing track that nothing would ever free.
     *
     * `pause()`+`flush()` ARE safe cross-thread (AudioTrack is thread-safe for these) and are the audio
     * analogue of the caller closing the socket: a socket close unblocks a consume thread parked in
     * `read()`, but never one parked in the blocking `write()`. `pause()` is documented to interrupt an
     * in-flight write; `flush()` then drops queued PCM so nothing plays out after stop and no write is
     * left waiting on buffer space that would never free.
     */

    /**
     * Duck the media track. Only MEDIA is ever ducked — every other purpose plays at unity, and the
     * effective gain is min(commandedDuck, focusDuck).
     *
     * 0.2, not 0.8: "duck by 20%" is ~2 dB and was reported by users as "does not duck at all".
     */
    fun setDucked(ducked: Boolean) {
        // synchronized, not a volatile compare-and-set: this is called from the voice decode thread
        // AND the UI thread (teardown). The read-compare-write-apply sequence being non-atomic let an
        // interleaving leave duckGain=1.0 with the hardware at 0.2 — after which every later
        // setDucked(false) early-returns and the duck is unrecoverable for the life of the player,
        // while the log claims it was restored.
        synchronized(duckLock) {
            val g = if (ducked) 0.2f else 1.0f
            if (g == duckGain) return
            duckGain = g
            runCatching { track?.setVolume(g) }
            log.i("media ${if (ducked) "ducked to 0.2" else "restored to 1.0"}")
        }
    }

    /** Re-assert the current gain onto a freshly built track. Without this a track created while a
     *  duck was in flight starts at unity and the duck is silently lost. */
    private fun applyGain(t: AudioTrack) {
        synchronized(duckLock) { runCatching { t.setVolume(duckGain) } }
    }

    private val duckLock = Any()

    @Volatile private var duckGain = 1.0f

    fun stop() {
        val hadConsumer = consumeStarted
        running.set(false)
        abandonFocus()
        runCatching { track?.pause() }
        runCatching { track?.flush() }
        // If consume() never ran, nothing else will EVER release the primed track: releaseAv() is
        // reachable only from the consume thread. That happens on every path where :9002 never
        // connected — a bind failure, or a session torn down before the producer dialled — and
        // AudioTrack instances come from a small global pool, so a retry loop exhausts it and every
        // later build() in the process throws.
        if (!hadConsumer) {
            val t = track
            track = null
            runCatching { t?.stop() }
            runCatching { t?.release() }
            log.i("released the primed track (the media seam never connected)")
        }
        log.i("stopping — ${framesDecoded.get()} frames, ${bytesIn.get()} bytes")
    }

    /** Consume ADTS off the seam until it closes. Blocking; call on its own thread. */
    @Volatile private var consumeStarted = false

    fun consume(ins: InputStream) {
        consumeStarted = true
        val buf = ByteArray(32 * 1024)
        val acc = java.io.ByteArrayOutputStream(64 * 1024)
        try {
            while (running.get()) {
                // A SocketException here during teardown is the caller's deliberate close, not a fault.
                val n =
                    try {
                        ins.read(buf)
                    } catch (e: Exception) {
                        if (running.get()) log.e("read: ${e.message}")
                        break
                    }
                if (n <= 0) break
                bytesIn.addAndGet(n.toLong())
                acc.write(buf, 0, n)
                val data = acc.toByteArray()
                val used = processAdts(data)
                acc.reset()
                if (used < data.size) acc.write(data, used, data.size - used)
            }
        } finally {
            releaseAv()
            log.i("seam ended — ${framesDecoded.get()} frames decoded")
        }
    }

    /**
     * The ONLY place codec/track are torn down, and only ever on the consume thread.
     *
     * Both fields are nulled: `CarPlayActivity.serve()` calls `consume()` again on this same instance
     * every time the producer re-dials the seam, and a non-null released codec would make the next
     * connection feed a corpse — an exception per frame and permanently dead audio after the first
     * transient reconnect. Each call is independently wrapped: with two resources, a throw releasing
     * the codec must not skip the track. Order per resource is stop-then-release — the authority never
     * bare-releases, and a bare release can click on some HALs.
     */
    private fun releaseAv() {
        val c = codec
        codec = null
        runCatching { c?.stop() }
        runCatching { c?.release() }
        val t = track
        track = null
        runCatching { t?.pause() }
        runCatching { t?.flush() }
        runCatching { t?.stop() }
        runCatching { t?.release() }
    }

    /** Walk complete ADTS frames; returns how many bytes were consumed. */
    private fun processAdts(data: ByteArray): Int {
        var i = 0
        while (i + 7 <= data.size) {
            // stop() may have run mid-buffer: a full 32 KB read holds many frames, and continuing to
            // decode+write them into a paused/flushed track is the write-after-stop deadlock. Bail.
            if (!running.get()) return i
            if ((data[i].toInt() and 0xFF) != 0xFF || (data[i + 1].toInt() and 0xF0) != 0xF0) {
                i++
                continue
            }
            val frameLen =
                ((data[i + 3].toInt() and 0x03) shl 11) or
                    ((data[i + 4].toInt() and 0xFF) shl 3) or
                    ((data[i + 5].toInt() and 0xE0) ushr 5)
            if (frameLen < 7) {
                i++
                continue
            }
            if (i + frameLen > data.size) return i // incomplete tail — keep it
            if (codec == null) {
                if (!running.get()) return i // tearing down — don't configure just to release
                val b2 = data[i + 2].toInt() and 0xFF
                val rateIdx = (b2 shr 2) and 0x0F
                val ch = ((b2 and 0x01) shl 2) or ((data[i + 3].toInt() and 0xC0) ushr 6)
                configure(if (rateIdx < rates.size) rates[rateIdx] else 48000, if (ch in 1..8) ch else 2)
            }
            // Strip the 7-byte ADTS header: the codec is configured with csd-0, so it wants raw AAC.
            feed(data, i + 7, frameLen - 7)
            i += frameLen
        }
        return i
    }

    /**
     * All-or-nothing. The fields are assigned only once BOTH resources are live, so no path exists
     * where a started codec has no track (the old code assigned `codec` first, so a throw from the
     * AudioTrack builder stranded a started decoder with no track, no retry and no release owner).
     * Failure releases both locals and backs off — without the backoff this retried on every ADTS
     * frame and drained the global codec pool in seconds.
     */
    private fun configure(
        sampleRate: Int,
        channels: Int,
    ) {
        val now = android.os.SystemClock.elapsedRealtime()
        if (configureFailedAt != 0L && now - configureFailedAt < CONFIGURE_RETRY_MS) return
        var c: MediaCodec? = null
        var t: AudioTrack? = null
        try {
            val fmt = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_AAC, sampleRate, channels)
            fmt.setInteger(MediaFormat.KEY_AAC_PROFILE, android.media.MediaCodecInfo.CodecProfileLevel.AACObjectLC)
            // csd-0 for raw AAC: 5 bits objectType(2=LC) | 4 bits rateIdx | 4 bits channelCfg.
            val rateIdx = rates.indexOf(sampleRate).let { if (it < 0) 3 else it }
            val csd =
                byteArrayOf(
                    (((2 shl 3) or (rateIdx shr 1)) and 0xFF).toByte(),
                    ((((rateIdx and 1) shl 7) or (channels shl 3)) and 0xFF).toByte(),
                )
            fmt.setByteBuffer("csd-0", ByteBuffer.wrap(csd))
            c = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_AUDIO_AAC)
            c.configure(fmt, null, null, 0)
            c.start()

            // Adopt the primed track when the stream matches what was primed (the normal case);
            // otherwise discard it and build for what actually arrived — a primed track must never
            // silently impose the wrong rate or channel count on the stream.
            val primed = track
            t =
                if (primed != null && sampleRate == cfgRate && channels == cfgChannels) {
                    log.i("adopting the primed AudioTrack")
                    primed
                } else {
                    if (primed != null) {
                        log.i(
                            "primed track was ${cfgRate}Hz ${cfgChannels}ch but the stream is " +
                                "${sampleRate}Hz ${channels}ch — rebuilding",
                        )
                        track = null
                        runCatching { primed.pause() }
                        runCatching { primed.flush() }
                        runCatching { primed.stop() }
                        runCatching { primed.release() }
                    }
                    buildTrack(sampleRate, channels)
                }

            codec = c
            track = t
            cfgRate = sampleRate
            cfgChannels = channels
            configureFailedAt = 0L
            log.i("configured AAC-LC ${sampleRate}Hz ${channels}ch → AudioTrack (USAGE_MEDIA), decoder=${c.name}")
        } catch (e: Exception) {
            // Release BOTH or the native side leaks — `it.play()` can throw after build() succeeded,
            // which would strand a live AudioTrack if only the codec were released.
            runCatching { c?.release() }
            runCatching { t?.release() }
            configureFailedAt = now
            log.e("configure failed (retry in ${CONFIGURE_RETRY_MS}ms): ${e.message}")
        }
    }

    /** Track construction, shared by [configure] and the ERROR_DEAD_OBJECT rebuild in [feed]. */
    private fun buildTrack(
        sampleRate: Int,
        channels: Int,
    ): AudioTrack {
        val chMask = if (channels >= 2) AudioFormat.CHANNEL_OUT_STEREO else AudioFormat.CHANNEL_OUT_MONO
        val minBuf = AudioTrack.getMinBufferSize(sampleRate, chMask, AudioFormat.ENCODING_PCM_16BIT)
        return AudioTrack
            .Builder()
            .setAudioAttributes(
                AudioAttributes
                    .Builder()
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                    .build(),
            ).setAudioFormat(
                AudioFormat
                    .Builder()
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .setSampleRate(sampleRate)
                    .setChannelMask(chMask)
                    .build(),
            ).setBufferSizeInBytes(maxOf(minBuf, 8192) * 2)
            .setTransferMode(AudioTrack.MODE_STREAM)
            .build()
            // `play()` can throw AFTER build() succeeded. `.also { it.play() }` would propagate before
            // the track is ever returned, so the caller's `t` is still null and its `t?.release()`
            // releases nothing — stranding a live native AudioTrack. Release it here, where it is
            // still in hand, then rethrow. (The caller's comment claimed this was handled; it was not.)
            .also { trk ->
                try {
                    trk.play()
                } catch (e: Throwable) {
                    runCatching { trk.release() }
                    throw e
                }
                applyGain(trk)
            }
    }

    private fun feed(
        data: ByteArray,
        off: Int,
        len: Int,
    ) {
        val c = codec ?: return
        if (len <= 0) return
        try {
            val inIdx = c.dequeueInputBuffer(5_000)
            if (inIdx >= 0) {
                // A dequeued index MUST be queued back on EVERY path, including the null-buffer
                // one. Input buffers are a fixed pool of 4-8; leaking them makes dequeueInputBuffer
                // return TRY_AGAIN_LATER forever — silent dead audio with a full timeout per frame.
                // Same fix as VoiceRouter; it had only been applied there.
                val ib = c.getInputBuffer(inIdx)
                if (ib == null) {
                    c.queueInputBuffer(inIdx, 0, 0, 0, 0)
                } else {
                    ib.clear()
                    if (ib.remaining() >= len) {
                        ib.put(data, off, len)
                        c.queueInputBuffer(inIdx, 0, len, System.nanoTime() / 1000, 0)
                    } else {
                        c.queueInputBuffer(inIdx, 0, 0, 0, 0)
                    }
                }
            }
            val info = MediaCodec.BufferInfo()
            while (true) {
                // Never issue the blocking AudioTrack.write below once stop() has paused the track —
                // that is the deadlock. Drain no further after teardown is requested.
                if (!running.get()) break
                val outIdx = c.dequeueOutputBuffer(info, 0)
                if (outIdx < 0) break
                c.getOutputBuffer(outIdx)?.let { ob ->
                    val pcm = ByteArray(info.size)
                    ob.position(info.offset)
                    ob.get(pcm)
                    track?.let { t ->
                        val w = t.write(pcm, 0, pcm.size)
                        if (w == AudioTrack.ERROR_DEAD_OBJECT) {
                            // An audioserver restart or a route teardown invalidates the track. Every
                            // later write then fails silently while the decoder keeps running — audio
                            // is gone for the rest of the session with a single log line. Rebuild in
                            // place; this runs on the consume thread, the only legal owner.
                            log.e("AudioTrack ERROR_DEAD_OBJECT — rebuilding")
                            track = null
                            runCatching { t.release() }
                            track =
                                runCatching { buildTrack(cfgRate, cfgChannels) }
                                    .onFailure { log.e("track rebuild failed: ${it.message}") }
                                    .getOrNull()
                        } else if (w < 0) {
                            log.e("AudioTrack.write returned $w")
                        }
                    }
                }
                c.releaseOutputBuffer(outIdx, false)
                val n = framesDecoded.incrementAndGet()
                if (n == 1L) log.i("FIRST AUDIO FRAME PLAYED")
                if (n % 500 == 0L) log.i("$n audio frames played")
            }
        } catch (e: Exception) {
            log.e("feed: ${e.javaClass.simpleName}: ${e.message}")
            // A mid-session codec error otherwise keeps the broken codec forever: processAdts never
            // reconfigures (codec != null), the seam stays open, and every later frame throws — dead
            // audio with per-frame spam. Release both (consume thread, the only legal owner) so the
            // next frame rebuilds via the codec == null path.
            if (e is MediaCodec.CodecException || e is IllegalStateException) {
                releaseAv()
                // Arm the backoff. releaseAv() nulls `codec`, so without this the very next ADTS
                // frame takes the codec == null path and reconfigures — and configureFailedAt is 0
                // there because the previous configure SUCCEEDED, so the guard never engages. A
                // persistent codec fault then rebuilt a decoder + AudioTrack ~47 times a second:
                // the codec-pool drain the backoff exists to prevent, plus audible clicking.
                configureFailedAt = android.os.SystemClock.elapsedRealtime()
            }
        }
    }
}
