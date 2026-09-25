package com.carlink.av

import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
import android.os.Process
import android.os.SystemClock
import com.carlink.logging.ProbeLog
import com.carlink.ocbm.seam.VoiceTag
import java.io.InputStream
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * The CarPlay MEDIA stream (`audioType` media / compatibility) → one `USAGE_MEDIA` [AudioTrack].
 *
 * Replaces the ADTS-only `AacPlayer`. The seam now hands every media access unit over with a
 * [VoiceTag] (`[rate][ch][atype][codec][len][AU]`, the same framing the voice lane uses), so this
 * player honours the format THE BOX SENT — wired CarPlay's big-endian PCM 48 kHz stereo (byte-swapped
 * to S16LE by the seam) and wireless CarPlay's AAC-LC 48 kHz stereo both land here and are turned into
 * PCM by [AudioDecoders]. Nothing about 48 k stereo is assumed; the prime is only a head start.
 *
 * ## What was ported from `carlink_native` (`DualStreamAudioManager`), and what was not
 *
 * Ported, because it was measured on the GM head unit:
 *  - `AudioTrack` sizing: 4x the minimum buffer, `PERFORMANCE_MODE_NONE` — `AUDIO_OUTPUT_FLAG_FAST`
 *    is denied to third-party apps on GM AAOS, so LOW_LATENCY buys nothing and adds jitter.
 *  - **Pre-fill before `play()`** (80 ms): the track is built stopped, written non-blocking until the
 *    threshold is buffered, then started — so AudioFlinger's first pull finds data and the first
 *    second of a session is not a stutter of underruns. Re-armed after a stream gap (the phone paused).
 *  - Underrun accounting from `AudioTrack.underrunCount`, logged as deltas so a bad lane is visible.
 *  - `ERROR_DEAD_OBJECT` rebuild in place (audioserver restart / route change is routine on a head unit).
 *  - `THREAD_PRIORITY_URGENT_AUDIO` on the writer thread.
 *  - Focus-driven gain: the listener maps `LOSS_TRANSIENT_CAN_DUCK` → 0.2, `LOSS_TRANSIENT`/`LOSS` → 0,
 *    `GAIN` → 1.0, and the effective gain is `min(commandedDuck, focusGain)`.
 *
 * Deliberately NOT ported:
 *  - Its playback thread + `AudioRingBuffer`. There the USB ingest thread must never block, so a ring
 *    decoupled it from the track. Here the OCBM read thread already hands off through
 *    [com.carlink.ocbm.seam.SeamPipe] (bounded, blocking — the project's closed-loop backpressure), and
 *    this consumer runs on its own thread, so a blocking `AudioTrack.write` IS the pacing. A second
 *    ring would only add latency.
 *  - Its zero-packet filter and nav "end marker" heuristics: those decoded riddleBox's PCM command
 *    stream. The seam carries a codec and an `audioType` per stream, so nothing is inferred from samples.
 *  - Its bug where a `LOSS` muted media and nothing ever re-requested focus: here a permanent loss
 *    also asks the phone to pause ([onFocusLost]), and focus is re-requested when audible media
 *    resumes, so the driver pressing play on the phone is enough to come back.
 *
 * ## Media must HOLD audio focus, or the volume knob gets stuck
 *
 * AAOS points the hardware volume control at the current focus owner and returns focus to the
 * PREVIOUS owner when a transient holder abandons it. `VoiceRouter` takes transient focus for Siri and
 * calls; if media never held focus there is nothing to hand back to and the knob stays on Phone/Siri
 * while music plays. Holding `AUDIOFOCUS_GAIN` here — regardless of whether audio is flowing — gives
 * the system a resting owner to return to.
 *
 * ## Threading
 *
 * [consume] owns the decoder and the track and releases both in its own `finally`; [stop] is
 * flag-only plus the one cross-thread call that is safe (`pause`+`flush`, which unblocks a blocking
 * write). Releasing a MediaCodec from another thread while `decode` is inside a dequeue is a native
 * crash, not an exception.
 */
class MediaAudioPlayer(
    am: AudioManager?,
    /** A permanent focus loss (another AAOS source took over): ask the phone to pause. */
    onFocusLost: () -> Unit = {},
    /**
     * Audible media is arriving while AAOS focus still holds us below 1.0 — i.e. a transient holder
     * (normally this app's own Siri/call sink) has outlived the phone's session. The router releases
     * its quiet sinks in response; throttled to once per 500 ms.
     */
    onAudibleWhileSuppressed: () -> Unit = {},
) {
    private val log = ProbeLog.sub("media")
    private val running = AtomicBoolean(false)

    @Volatile private var track: MediaTrack? = null

    @Volatile private var decoder: AudioDecoder? = null

    @Volatile private var codec = -1

    @Volatile private var configureFailedAt = 0L

    @Volatile private var consumeStarted = false

    @Volatile private var pausedForAssistant = false

    val framesDecoded = AtomicLong(0)
    val bytesIn = AtomicLong(0)

    private val attrs =
        AudioAttributes
            .Builder()
            .setUsage(AudioAttributes.USAGE_MEDIA)
            .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
            .build()

    private val focus = MediaFocus(am, attrs, log, onChange = { applyGain() }, onLost = onFocusLost, onSuppressedAudible = onAudibleWhileSuppressed)

    @Volatile private var loggedGain = -1f

    private val gainLock = Any()

    /** Fade-in after Siri / a call — see [ResumeRamp]. Applied to the PCM, so `setGain` never sees it. */
    private val ramp = ResumeRamp { log.i(it) }

    /** Last time audible media was written; "media was active" for the ramp means within [ACTIVE_WINDOW_MS]. */
    @Volatile private var lastAudibleAt = 0L

    /** Commanded by VoiceRouter while a voice stream is audible. 0.2, not 0.8: ~2 dB was "does not duck". */
    @Volatile private var duckGain = 1.0f

    private val warned = HashSet<String>()

    private companion object {
        /** Never re-attempt configure per access unit — that drains the codec pool in seconds. */
        const val CONFIGURE_RETRY_MS = 5_000L
        const val PRIME_RATE = 48_000
        const val PRIME_CHANNELS = 2
        const val DUCK_GAIN = 0.2f
        const val LOG_EVERY_FRAMES = 1000L
        const val ACTIVE_WINDOW_MS = 2_000L
    }

    /**
     * Take resting focus and build the track before any audio arrives, so the first AU is written
     * into a track that already exists. The prime is only a head start: [configure] adopts it when the
     * stream matches and rebuilds otherwise. Measured saving on the AAC path: ~134 ms of the 172 ms
     * seam-connect → first-audio (2026-08-12).
     */
    fun start() {
        configureFailedAt = 0L
        running.set(true)
        focus.request()
        if (track != null) return
        runCatching { MediaTrack(PRIME_RATE, PRIME_CHANNELS, attrs, log) }
            .onSuccess {
                track = it
                applyGain()
                log.i("primed AudioTrack ${PRIME_RATE}Hz ${PRIME_CHANNELS}ch — waiting for the stream")
            }.onFailure { log.w("prime failed (harmless; configure will build one): ${it.message}") }
    }

    /**
     * Pause outright while Siri speaks, instead of only ducking: AAOS chooses the knob's target from
     * ACTIVE players and a ducked track is still active, so with MUSIC outranking VOICE_COMMAND the knob
     * showed "Audio" mid-Siri. `pause()` without `flush()`: the buffered media survives and resume
     * continues where it left off. The seam keeps delivering; writes block until we resume.
     */
    fun setAssistantSpeaking(speaking: Boolean) {
        if (pausedForAssistant == speaking) return
        pausedForAssistant = speaking
        if (speaking) ramp.interrupted(mediaWasActive = SystemClock.elapsedRealtime() - lastAudibleAt < ACTIVE_WINDOW_MS)
        val t = track ?: return
        runCatching { t.setPaused(speaking) }
            .onSuccess { log.i(if (speaking) "media paused for the assistant" else "media resumed after the assistant") }
            .onFailure { log.w("assistant pause/resume: ${it.javaClass.simpleName}: ${it.message}") }
    }

    /** Duck for a voice stream. Effective gain is min(this, focus-driven gain). */
    fun setDucked(ducked: Boolean) {
        synchronized(gainLock) {
            val g = if (ducked) DUCK_GAIN else 1.0f
            if (g == duckGain) return
            duckGain = g
        }
        applyGain()
    }

    /**
     * The ONE place the track's volume is set: [MediaGain.effective] of the focus gain and the duck,
     * logged together so neither path can mask the other silently.
     */
    private fun applyGain() {
        // synchronized: called from the voice decode thread, the focus callback and teardown; a torn
        // read-compare-write left the hardware at 0.2 with duckGain=1.0 once, unrecoverably.
        synchronized(gainLock) {
            val f = focus.gain
            val d = duckGain
            val g = MediaGain.effective(f, d)
            runCatching { track?.setGain(g) }
            if (g != loggedGain) {
                // Exclusive transient / permanent loss (focus 0) is a hard interruption for the
                // fade-in; a duck (0.2, nav) is not and restores by setVolume as before.
                if (f == 0f && loggedGain != 0f) ramp.interrupted(mediaWasActive = SystemClock.elapsedRealtime() - lastAudibleAt < ACTIVE_WINDOW_MS)
                loggedGain = g
                log.i("media gain $g (focus $f, duck $d, ramp ${if (ramp.active) "%.2f".format(ramp.level) else "-"})")
            }
        }
    }

    fun stop() {
        val hadConsumer = consumeStarted
        running.set(false)
        focus.abandon()
        runCatching { track?.pauseAndFlush() } // unblocks a blocking write; drops queued PCM
        // If consume() never ran, nothing else will EVER release the primed track — releaseAv() is
        // reachable only from the consume thread. AudioTrack instances come from a small global
        // pool, so a retry loop that primes and never consumes exhausts it.
        if (!hadConsumer) {
            val t = track
            track = null
            runCatching { t?.release() }
            log.i("released the primed track (the media seam never connected)")
        }
        log.i("stopping — ${framesDecoded.get()} frames, ${bytesIn.get()} bytes")
    }

    /** Consume tagged media frames off the seam until it closes. Blocking; call on its own thread. */
    fun consume(ins: InputStream) {
        consumeStarted = true
        runCatching { Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO) }
        val hdr = ByteArray(VoiceTag.LEN)
        try {
            while (running.get()) {
                val (fmt, au) = readTaggedFrame(ins, hdr, log) ?: break
                bytesIn.addAndGet((VoiceTag.LEN + au.size).toLong())
                if (au.isNotEmpty()) handle(fmt, au)
            }
        } finally {
            // No catch: OcbmAvLanes.runConsumer logs a dying consumer; this only has to release.
            releaseAv()
            log.i("seam ended — ${framesDecoded.get()} frames decoded")
        }
    }

    private fun handle(
        f: VoiceTag.Fmt,
        au: ByteArray,
    ) {
        if (!AudioDecoders.canDecode(f.codec)) {
            if (warned.add("codec-${f.codec}")) {
                log.w(
                    "media stream is ${AudioDecoders.codecName(f.codec)} ${f.rate}Hz ${f.channels}ch; this player decodes " +
                        "PCM, AAC-LC and AAC-ELD — dropping rather than feeding it to the wrong decoder",
                )
            }
            return
        }
        val needsConfigure = decoder == null || track?.matches(f.rate, f.channels) != true || codec != f.codec
        if (needsConfigure) configure(f.rate, f.channels, f.codec)
        val dec = decoder ?: return
        try {
            dec.decode(au, 0, au.size) { pcm, off, len -> writeDecoded(pcm, off, len) }
        } catch (e: IllegalStateException) {
            // MediaCodec.CodecException IS an IllegalStateException. A mid-session codec fault would
            // otherwise keep the broken decoder forever: release both on the consume thread (the only
            // legal owner) and ARM the backoff — without it the next AU reconfigures immediately and
            // a persistent fault rebuilds a decoder + track ~47 times a second.
            log.e("decode: ${e.javaClass.simpleName}: ${e.message}")
            releaseAv()
            configureFailedAt = SystemClock.elapsedRealtime()
        }
        val n = framesDecoded.incrementAndGet()
        if (n == 1L) log.i("FIRST AUDIO FRAME PLAYED")
        if (n % LOG_EVERY_FRAMES == 0L) log.i("$n media frames played")
    }

    /**
     * One decoded S16LE buffer → the track. The resume ramp is applied to the samples first (on the
     * frame that re-primes the track after a gap, when armed); a dead track is rebuilt in place.
     */
    private fun writeDecoded(
        pcm: ByteArray,
        off: Int,
        len: Int,
    ) {
        // Re-read `track` per callback: a rebuild inside this decode must not leave later callbacks
        // writing to (and re-rebuilding from) the dead one.
        val trk = track ?: return
        val now = SystemClock.elapsedRealtime()
        if (trk.gapSinceLastWrite(now) && ramp.start(now)) log.i("resume ramp triggered by the first media frame after a ${now - trk.lastWriteAt} ms gap")
        ramp.apply(pcm, off, len, ResumeRamp.Format(trk.channels, trk.rate), now)
        if (PcmLevel.audible(pcm, off, len)) lastAudibleAt = now
        if (!trk.write(pcm, off, len, running)) {
            log.e("AudioTrack ERROR_DEAD_OBJECT — rebuilding")
            track = null
            runCatching { trk.release() }
            track =
                runCatching { MediaTrack(trk.rate, trk.channels, attrs, log) }
                    .onSuccess { applyGain() }
                    .onFailure { log.e("track rebuild failed: ${it.message}") }
                    .getOrNull()
        }
        focus.noticeAudible(pcm, off, len)
    }

    /**
     * All-or-nothing: the fields are assigned only once BOTH resources are live, so no path exists
     * where a started decoder has no track. Failure releases both locals and backs off.
     */
    private fun configure(
        rate: Int,
        channels: Int,
        cod: Int,
    ) {
        val now = SystemClock.elapsedRealtime()
        if (configureFailedAt != 0L && now - configureFailedAt < CONFIGURE_RETRY_MS) return
        decoder?.let { runCatching { it.close() } }
        decoder = null
        var dec: AudioDecoder? = null
        var built: MediaTrack? = null
        runCatching {
            dec = AudioDecoders.open(cod, rate, channels)
            // Adopt the primed track when the stream matches (the normal case); otherwise discard it
            // and build for what actually arrived — a primed track must never silently impose the
            // wrong rate or channel count on the stream.
            val primed = track
            if (primed != null && !primed.matches(rate, channels)) {
                log.i("track was ${primed.rate}Hz ${primed.channels}ch but the stream is ${rate}Hz ${channels}ch — rebuilding")
                track = null
                runCatching { primed.release() }
            }
            if (track == null) built = MediaTrack(rate, channels, attrs, log)
            decoder = dec
            built?.let { track = it }
            codec = cod
            configureFailedAt = 0L
            applyGain()
            log.i("configured ${AudioDecoders.codecName(cod)} ${rate}Hz ${channels}ch -> AudioTrack(USAGE_MEDIA), decoder=${dec?.name}")
        }.onFailure { e ->
            // runCatching, not catch: an OutOfMemoryError here is an Error, and letting it escape
            // leaks the native codec AND leaves the backoff unarmed. Release BOTH or the native side leaks.
            runCatching { dec?.close() }
            runCatching { built?.release() }
            decoder = null
            configureFailedAt = now
            log.e("configure failed (retry in ${CONFIGURE_RETRY_MS}ms): ${e.message}")
        }
    }

    /** The ONLY place the decoder and track are torn down, and only ever on the consume thread. */
    private fun releaseAv() {
        val d = decoder
        decoder = null
        codec = -1
        runCatching { d?.close() }
        val t = track
        track = null
        runCatching { t?.release() }
    }
}

/**
 * Resting `AUDIOFOCUS_GAIN` for media plus the focus-driven gain (ported from
 * `DualStreamAudioManager.getOrCreateFocusListener`, MEDIA arm). A DISTINCT listener instance is
 * load-bearing: AAOS CarAudioFocus keys on listener identity.
 */
private class MediaFocus(
    private val am: AudioManager?,
    private val attrs: AudioAttributes,
    private val log: ProbeLog.Logger,
    private val onChange: () -> Unit,
    private val onLost: () -> Unit,
    private val onSuppressedAudible: () -> Unit,
) {
    @Volatile var gain = 1.0f
        private set

    /** True after a permanent `AUDIOFOCUS_LOSS` until a later request is granted. */
    @Volatile var lost = false
        private set

    @Volatile private var request: AudioFocusRequest? = null

    @Volatile private var lastRegainAt = 0L

    @Volatile private var lastSuppressedAt = 0L

    private companion object {
        const val DUCKED = 0.2f

        /** After a permanent focus loss, re-request no more often than this once audible media resumes. */
        const val REGAIN_RETRY_MS = 2_000L

        /** Audible-while-suppressed notifications are throttled to this. */
        const val SUPPRESSED_NOTIFY_MS = 500L
    }

    private val listener =
        AudioManager.OnAudioFocusChangeListener { change ->
            gain =
                when (change) {
                    AudioManager.AUDIOFOCUS_GAIN -> 1.0f
                    AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK -> DUCKED
                    AudioManager.AUDIOFOCUS_LOSS_TRANSIENT, AudioManager.AUDIOFOCUS_LOSS -> 0.0f
                    else -> gain
                }
            log.i("media focus change $change -> gain $gain")
            if (change == AudioManager.AUDIOFOCUS_LOSS) {
                // The system has revoked this request; a later request() must build a fresh one.
                request = null
                lost = true
                runCatching { onLost() }
            }
            onChange()
        }

    /** Idempotent. NOT tied to whether audio flows: the point is to be the owner AAOS returns to. */
    fun request() {
        val mgr =
            am ?: run {
                log.w("no AudioManager — media will not hold focus; the volume knob may stick on Phone/Siri")
                return
            }
        if (request != null) return
        runCatching {
            val req =
                AudioFocusRequest
                    .Builder(AudioManager.AUDIOFOCUS_GAIN)
                    .setAudioAttributes(attrs)
                    .setOnAudioFocusChangeListener(listener)
                    // VoiceRouter ducks us in software and CarPlay expects the phone to remain the
                    // mixing authority — never let the system pause us behind our back.
                    .setWillPauseWhenDucked(false)
                    .build()
            val r = mgr.requestAudioFocus(req)
            val granted = r == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
            if (granted) {
                request = req
                lost = false
                gain = 1.0f
                onChange()
            }
            log.i("media audio focus: ${if (granted) "GRANTED" else "result=$r"}")
        }.onFailure { log.w("media focus request failed: ${it.javaClass.simpleName}: ${it.message}") }
    }

    /**
     * Audible media arrived. After a PERMANENT loss: re-request (throttled). While a TRANSIENT loss
     * or duck still holds the gain below 1.0: tell the owner, so a voice sink of our own that has
     * outlived Siri gives its focus back — iOS never resumes audible media inside a Siri turn.
     */
    fun noticeAudible(
        pcm: ByteArray,
        off: Int,
        len: Int,
    ) {
        if (gain >= 1.0f && !lost) return
        if (!PcmLevel.audible(pcm, off, len)) return
        val now = SystemClock.elapsedRealtime()
        if (lost) {
            if (now - lastRegainAt < REGAIN_RETRY_MS) return
            lastRegainAt = now
            log.i("audible media after a focus loss — re-requesting media focus")
            request()
            return
        }
        if (now - lastSuppressedAt < SUPPRESSED_NOTIFY_MS) return
        lastSuppressedAt = now
        runCatching { onSuppressedAudible() }
    }

    fun abandon() {
        val mgr = am ?: return
        request?.let { runCatching { mgr.abandonAudioFocusRequest(it) } }
        request = null
    }
}

/**
 * One `USAGE_MEDIA` S16 [AudioTrack] with the `carlink_native` playback discipline: built STOPPED,
 * pre-filled, then started; blocking writes after that; underruns counted; dead objects reported.
 */
private class MediaTrack(
    val rate: Int,
    val channels: Int,
    attrs: AudioAttributes,
    private val log: ProbeLog.Logger,
) {
    private companion object {
        /** `carlink_native` `AudioConfig.bufferMultiplier` — 4x minimum absorbs the measured P99 jitter. */
        const val BUFFER_MULTIPLIER = 4
        const val MIN_BUFFER_BYTES = 4096

        /** `carlink_native` `prefillThresholdMs`. */
        const val PREFILL_MS = 80

        /** A gap this long means the phone stopped the stream; the next data re-arms the pre-fill. */
        const val REPRIME_GAP_MS = 1_000L

        /** How often to poll `underrunCount`. */
        const val UNDERRUN_POLL_EVERY = 50

        /** When a blocking write is interrupted by pause(), poll rather than spin. */
        const val PAUSED_POLL_MS = 20L
        const val UNDERRUN_LOG_INTERVAL_MS = 10_000L
        const val MS_PER_S = 1000L
    }

    private val bytesPerSecond = rate * channels * 2
    private val bufferBytes: Int
    private val prefillBytes: Int
    private val track: AudioTrack

    private var started = false
    private var prefilled = 0

    @Volatile var lastWriteAt = 0L
        private set
    private var writes = 0
    private var underruns = 0
    private var lastUnderrunLogAt = 0L

    @Volatile private var paused = false

    init {
        val mask = if (channels >= 2) AudioFormat.CHANNEL_OUT_STEREO else AudioFormat.CHANNEL_OUT_MONO
        val minBuf = AudioTrack.getMinBufferSize(rate, mask, AudioFormat.ENCODING_PCM_16BIT)
        require(minBuf > 0) { "AudioTrack.getMinBufferSize(${rate}Hz) = $minBuf" }
        bufferBytes = maxOf(minBuf, MIN_BUFFER_BYTES) * BUFFER_MULTIPLIER
        prefillBytes = minOf((bytesPerSecond * PREFILL_MS / MS_PER_S).toInt(), bufferBytes / 2)
        track =
            AudioTrack
                .Builder()
                .setAudioAttributes(attrs)
                .setAudioFormat(
                    AudioFormat
                        .Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setSampleRate(rate)
                        .setChannelMask(mask)
                        .build(),
                ).setBufferSizeInBytes(bufferBytes)
                .setTransferMode(AudioTrack.MODE_STREAM)
                // FAST is denied to third-party apps on GM AAOS; LOW_LATENCY buys nothing and can add jitter.
                .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_NONE)
                .build()
        if (track.state != AudioTrack.STATE_INITIALIZED) {
            runCatching { track.release() }
            error("AudioTrack failed to initialise (${rate}Hz ${channels}ch)")
        }
        log.i("AudioTrack ${rate}Hz ${channels}ch buffer=${bufferBytes}B (min $minBuf) prefill=${prefillBytes}B id=${track.audioSessionId}")
    }

    fun matches(
        r: Int,
        c: Int,
    ): Boolean = rate == r && channels == c

    fun setGain(g: Float) {
        track.setVolume(g.coerceIn(0f, 1f))
    }

    /** Assistant pause/resume. A resume before the pre-fill completed leaves `play()` to the pre-fill. */
    fun setPaused(p: Boolean) {
        paused = p
        if (p) {
            track.pause()
        } else if (started) {
            track.play()
        }
    }

    /** Cross-thread teardown unblock: pause interrupts an in-flight blocking write, flush drops the queue. */
    fun pauseAndFlush() {
        paused = true
        runCatching { track.pause() }
        runCatching { track.flush() }
    }

    fun release() {
        runCatching { track.pause() }
        runCatching { track.flush() }
        runCatching { track.stop() }
        runCatching { track.release() }
    }

    /**
     * Write S16LE PCM. Returns false only when the track is dead and must be rebuilt by the caller.
     *
     * Before the pre-fill threshold, writes are non-blocking into the stopped track; at the threshold
     * `play()` starts it. After that, writes block — that IS the lane's pacing (see the class doc). A
     * write interrupted by `pause()` returns 0; while paused for the assistant we poll rather than
     * spin, and [running] going false (stop) ends the wait.
     */
    fun write(
        pcm: ByteArray,
        off: Int,
        len: Int,
        running: AtomicBoolean,
    ): Boolean {
        val now = SystemClock.elapsedRealtime()
        if (gapSinceLastWrite(now)) reprime(now - lastWriteAt)
        var o = off
        var remaining = len
        while (remaining > 0 && running.get()) {
            val w = if (started) track.write(pcm, o, remaining) else prefillWrite(pcm, o, remaining)
            if (w < 0) {
                log.w("AudioTrack.write -> $w")
                // ERROR_DEAD_OBJECT (audioserver restart / route change) and ERROR_INVALID_OPERATION
                // (track uninitialised) both mean this track will never accept data again.
                return w != AudioTrack.ERROR_DEAD_OBJECT && w != AudioTrack.ERROR_INVALID_OPERATION
            }
            if (w == 0 && started) SystemClock.sleep(PAUSED_POLL_MS)
            o += w
            remaining -= w
        }
        // Stamped AFTER the write: a write that blocked through a Siri turn must not read as a gap.
        lastWriteAt = SystemClock.elapsedRealtime()
        if (started && ++writes % UNDERRUN_POLL_EVERY == 0) pollUnderruns()
        return true
    }

    /** Non-blocking until the threshold, then `play()`. Anything the stopped buffer would not take is written after. */
    private fun prefillWrite(
        pcm: ByteArray,
        off: Int,
        len: Int,
    ): Int {
        val w = track.write(pcm, off, len, AudioTrack.WRITE_NON_BLOCKING)
        if (w < 0) return w
        prefilled += w
        if (prefilled >= prefillBytes || w < len) {
            if (!paused) track.play()
            started = true
            underruns = track.underrunCount
            log.i("pre-fill complete: ${prefilled}B (~${prefilled * MS_PER_S / bytesPerSecond} ms) buffered, playback started")
        }
        return w
    }

    /** The stream stopped long enough to drain the track; go back to the pre-fill state so it restarts clean. */
    private fun reprime(gapMs: Long) {
        pollUnderruns()
        runCatching { track.pause() }
        started = false
        prefilled = 0
        log.i("stream resumed after a $gapMs ms gap — re-arming pre-fill")
    }

    /** The stream stopped for longer than [REPRIME_GAP_MS] since the last write: the next frame is a resume. */
    fun gapSinceLastWrite(now: Long): Boolean = started && lastWriteAt != 0L && now - lastWriteAt > REPRIME_GAP_MS

    /**
     * Underruns are expected while the phone is paused: iOS keeps the stream up but sparse, the track
     * stays PLAYING and starves at the mixer period (~20/s). Count them all; log at most once per
     * [UNDERRUN_LOG_INTERVAL_MS] so a paused phone does not fill the log at 400 ms cadence.
     */
    private fun pollUnderruns() {
        val n = runCatching { track.underrunCount }.getOrDefault(underruns)
        if (n <= underruns) return
        val now = SystemClock.elapsedRealtime()
        if (now - lastUnderrunLogAt >= UNDERRUN_LOG_INTERVAL_MS) {
            log.w("media underrun +${n - underruns} (total $n)")
            lastUnderrunLogAt = now
        }
        underruns = n
    }
}

/**
 * One `[VoiceTag][AU]` frame off the media pipe, or null on EOF / a torn tag (the caller ends the
 * connection; ocbmd re-dials). `au` is empty for a zero-length frame.
 */
private fun readTaggedFrame(
    ins: InputStream,
    hdr: ByteArray,
    log: ProbeLog.Logger,
): Pair<VoiceTag.Fmt, ByteArray>? {
    if (!StreamIo.readFully(ins, hdr, VoiceTag.LEN)) return null
    val h = VoiceTag.parse(hdr)
    if (h.rate !in 8000..96000 || h.len < 0 || h.len > MAX_MEDIA_AU) {
        log.e("media desync: ${h.rate}Hz len=${h.len} — dropping the connection")
        return null
    }
    val au = ByteArray(h.len)
    if (h.len > 0 && !StreamIo.readFully(ins, au, h.len)) return null
    return h.fmt to au
}

private const val MAX_MEDIA_AU = 1 shl 20
