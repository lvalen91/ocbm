package com.carlink.projection

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.Process
import com.carlink.logging.ProbeLog
import java.io.BufferedReader
import java.io.IOException
import java.io.InputStreamReader
import java.io.OutputStream
import java.net.InetSocketAddress
import java.net.Socket
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * The mic uplink: captures head-unit microphone audio and streams it to `airplayd`, which encodes
 * AAC-ELD and sends it to the phone.
 *
 * Without this, iOS negotiates the uplink and receives nothing: **Siri hears silence and a phone
 * call is one-way**, while the downlink plays perfectly and every log line looks healthy. That
 * asymmetry is what makes this failure hard to notice — everything about the voice path *except*
 * the direction that matters appears to work.
 *
 * ## The chain, and where this sits in it
 *
 * ```
 *   AudioRecord ──► MicUplink ──:9112──► airplayd ──AAC-ELD──► iPhone
 * ```
 *
 * `airplayd`'s encoder is only present when it was built with `mic-uplink-eld` (see
 * `pi/tools/build_fdk_aac_arm64.sh`); without it the box logs *"negotiated but `mic-uplink-eld` not
 * built"* and drops what we send.
 *
 * ## Capture is gated in principle, CONTINUOUS in practice
 *
 * `airplayd` arms at stream SETUP and — until the box-side fix in `session.rs` is deployed — never
 * sends `uplink off`, so the microphone stays open for the whole session rather than for the turn.
 * Measured: 95 minutes of continuous capture in one session. The gate below is correct and does
 * what it says; the counterpart signal simply does not arrive yet. Do not read the next paragraph
 * as a promise that currently holds.
 *
 * ## Capture is GATED, not continuous
 *
 * `airplayd` opens a back-channel on the same socket: `uplink on <rate> <channels>` when iOS arms a
 * voice stream, `uplink off` when it tears down. The microphone is opened only between those, which
 * is the right behaviour for both privacy (no open mic while music plays) and power, and it is also
 * the only way to know the sample rate — iOS picks it, typically 16 kHz mono for Siri and calls.
 *
 * ## Level metering, and a retracted claim
 *
 * [measureLevel] reports peak and RMS once per second of audio. That exists because "is the
 * microphone actually picking anything up" is answerable from nowhere else in the chain: the box
 * happily encodes and transmits silence, the packet counters climb, and the phone simply hears
 * nothing.
 *
 * An earlier version of this doc asserted that `ro.boot.audio.tinyalsa.simulate_input=true` made
 * `AudioRecord` return generated silence on this build, and that the uplink therefore could not
 * work. **That was wrong** — measured on device, capture reaches -4 dBFS peak on speech. The
 * retraction is `pi/docs/02` §1. Do not reintroduce that diagnosis; if the mic is genuinely dead
 * here, the level line will say so and the cause is something else.
 */
class MicUplink(
    private val ctx: Context,
) {
    private val log = ProbeLog.sub("mic")
    private val running = AtomicBoolean(false)

    @Volatile private var thread: Thread? = null

    @Volatile private var sock: Socket? = null

    @Volatile private var record: AudioRecord? = null

    /** True while iOS has an uplink armed. */
    @Volatile
    var armed: Boolean = false
        private set

    val chunksSent = AtomicLong(0)
    val bytesSent = AtomicLong(0)

    /**
     * Set once we have captured a meaningful amount of audio that was entirely zero.
     */
    @Volatile
    var silenceDetected: Boolean = false
        private set

    /**
     * Set once the capture contains runs of exactly-zero samples long enough to be fabricated.
     *
     * **This is the detector that matters on this hardware.** Peak and RMS both read healthy while
     * roughly three quarters of what we ship is HAL-inserted silence, because the real audio
     * *between* the gaps is fine — so "signal present" is a true statement about a stream a speech
     * recogniser cannot use. Measured on the bench: `AHAL_StreamAlsa: transfer: incomplete data
     * received, inserting 240 frames of silence` about 35 times a second, and `hw_ptr` advancing at
     * ~13,000 frames/s against a nominal 48,000.
     */
    @Volatile
    var dropoutDetected: Boolean = false
        private set

    fun hasPermission(): Boolean =
        ctx.checkPermission(Manifest.permission.RECORD_AUDIO, Process.myPid(), Process.myUid()) ==
            PackageManager.PERMISSION_GRANTED

    fun start() {
        if (!running.compareAndSet(false, true)) return
        thread =
            Thread({ run() }, "mic-uplink").apply {
                isDaemon = true
                start()
            }
    }

    fun stop() {
        running.set(false)
        runCatching { sock?.close() }
        stopCapture()
    }

    // ---- the control connection ------------------------------------------------------------------

    private fun run() {
        while (running.get()) {
            val s = connect()
            if (s == null) {
                // airplayd exists only during a session, so a refused connect is the normal idle
                // state rather than an error.
                sleepQuietly(RECONNECT_MS)
                continue
            }
            try {
                serve(s)
            } catch (e: IOException) {
                if (running.get()) log.i("uplink control connection ended — ${e.message}")
            } finally {
                stopCapture()
                runCatching { s.close() }
                sock = null
                armed = false
            }
            if (running.get()) sleepQuietly(RECONNECT_MS)
        }
        stopCapture()
    }

    private fun connect(): Socket? =
        try {
            Socket().apply {
                tcpNoDelay = true
                connect(InetSocketAddress(SeamContract.LOOPBACK, SeamContract.PORT_MIC_INGEST), CONNECT_TIMEOUT_MS)
                sock = this
                log.i("connected to the uplink ingest (:${SeamContract.PORT_MIC_INGEST})")
            }
        } catch (_: IOException) {
            null
        }

    /** Read the back-channel until it closes, arming and disarming capture as told. */
    private fun serve(s: Socket) {
        val out = s.getOutputStream()
        val reader = BufferedReader(InputStreamReader(s.getInputStream()))
        while (running.get()) {
            val line = reader.readLine() ?: break
            when {
                line.startsWith("uplink on") -> {
                    // `uplink on <rate> <channels>` — iOS chooses these, so they are read from the
                    // wire rather than assumed. 16 kHz mono is the usual Siri/call case.
                    val parts = line.trim().split(Regex("\\s+"))
                    val rate = parts.getOrNull(2)?.toIntOrNull() ?: DEFAULT_RATE
                    val channels = parts.getOrNull(3)?.toIntOrNull() ?: 1
                    log.i("uplink ARMED — ${rate}Hz ${channels}ch")
                    armed = true
                    startCapture(rate, channels, out)
                }
                line.startsWith("uplink off") -> {
                    log.i("uplink disarmed")
                    armed = false
                    stopCapture()
                }
                else -> Unit
            }
        }
    }

    // ---- capture ---------------------------------------------------------------------------------

    @Synchronized
    private fun startCapture(
        rate: Int,
        channels: Int,
        out: OutputStream,
    ) {
        stopCapture()
        // stop() may have won the lock first. Without this check its stopCapture() finds no recorder,
        // returns, and then this call creates a NEW one that nothing will ever stop — the pump exits
        // on its first `running` check and the AudioRecord stays RECORDING for the process lifetime.
        if (!running.get()) {
            log.i("shutting down — not starting capture")
            return
        }
        if (!hasPermission()) {
            // Loud, because the symptom (silent Siri) is identical to every other failure in this
            // chain and this one is fixed by one adb flag.
            log.e("RECORD_AUDIO not granted — uplink will be SILENT (adb install -g, or grant it)")
            return
        }
        val channelMask = if (channels >= 2) AudioFormat.CHANNEL_IN_STEREO else AudioFormat.CHANNEL_IN_MONO
        val minBuf = AudioRecord.getMinBufferSize(rate, channelMask, AudioFormat.ENCODING_PCM_16BIT)
        if (minBuf <= 0) {
            log.e("AudioRecord rejects ${rate}Hz ${channels}ch (getMinBufferSize=$minBuf) — no uplink")
            return
        }
        val r =
            try {
                AudioRecord(
                    // VOICE_COMMUNICATION rather than MIC: it is the source that carries AEC and
                    // noise suppression where the platform provides them, and on a head unit the
                    // speaker is feeding the same cabin as the mic — without echo cancellation the
                    // phone hears its own downlink.
                    MediaRecorder.AudioSource.VOICE_COMMUNICATION,
                    rate,
                    channelMask,
                    AudioFormat.ENCODING_PCM_16BIT,
                    // Several periods of slack so a scheduling hiccup does not drop frames.
                    minBuf * BUFFER_PERIODS,
                )
            } catch (e: Exception) {
                log.e("AudioRecord construction failed: $e")
                return
            }
        if (r.state != AudioRecord.STATE_INITIALIZED) {
            log.e("AudioRecord did not initialise (state=${r.state}) — no uplink")
            runCatching { r.release() }
            return
        }
        record = r
        runCatching { r.startRecording() }
            .onFailure {
                log.e("startRecording failed: $it")
                runCatching { r.release() }
                record = null
                return
            }
        Thread({ pump(r, rate, channels, out) }, "mic-pump").apply {
            isDaemon = true
            start()
        }
    }

    @Synchronized
    private fun stopCapture() {
        val r = record ?: return
        record = null
        runCatching { r.stop() }
        runCatching { r.release() }
        log.i("capture stopped (${chunksSent.get()} chunks, ${bytesSent.get()} B)")
    }

    /**
     * Read PCM and frame it as `mic <len>\n<len bytes>`.
     *
     * Runs until capture stops. Chunked at [CHUNK_MS] rather than by the driver's buffer size so the
     * cadence is predictable regardless of what the HAL picks — the encoder on the other side wants
     * a steady feed, and a jittery one shows up as choppy uplink rather than as an error.
     */
    private fun pump(
        r: AudioRecord,
        rate: Int,
        channels: Int,
        out: OutputStream,
    ) {
        // EVERY exit from this function must release the microphone.
        //
        // Without the finally below, a read error or a write failure returned straight out and left
        // `record` non-null and the AudioRecord in RECORDING state with nobody reading it: a hot mic
        // for the life of the process, with the class doc still promising "no open mic while music
        // plays". Confirmed on the bench — `appops get RECORD_AUDIO` read "running" ten minutes
        // after the last Siri turn.
        try {
            pumpLoop(r, rate, channels, out)
        } finally {
            // Only if we are still the active capture; a concurrent startCapture may already have
            // replaced us, and stopping then would kill ITS recorder.
            synchronized(this) {
                if (record === r) stopCapture()
            }
        }
    }

    private fun pumpLoop(
        r: AudioRecord,
        rate: Int,
        channels: Int,
        out: OutputStream,
    ) {
        Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)
        val bytesPerFrame = 2 * channels
        val chunk = (rate * CHUNK_MS / 1000) * bytesPerFrame
        val buf = ByteArray(chunk)
        try {
            while (running.get() && record === r) {
                var filled = 0
                while (filled < buf.size && record === r) {
                    val n = r.read(buf, filled, buf.size - filled)
                    if (n <= 0) {
                        if (n < 0) log.e("AudioRecord.read returned $n — stopping capture")
                        return
                    }
                    filled += n
                }
                if (filled == 0) continue

                // LEVEL, not a zero test. An all-zero check is too crude to be useful: a HAL that
                // synthesises input can emit low-level noise or a fixed pattern that is not zero
                // and is still inaudible, which reads as "signal present" and proves nothing. Peak
                // and RMS say what is actually there.
                measureLevel(buf, filled)

                // Header and payload in one write: two writes on a NODELAY socket are two packets,
                // and the reader does a line read followed by an exact-length read.
                val header = "mic $filled\n".toByteArray()
                val msg = ByteArray(header.size + filled)
                System.arraycopy(header, 0, msg, 0, header.size)
                System.arraycopy(buf, 0, msg, header.size, filled)
                out.write(msg)
                out.flush()
                chunksSent.incrementAndGet()
                bytesSent.addAndGet(filled.toLong())
            }
        } catch (e: IOException) {
            if (running.get()) log.i("uplink write ended — ${e.message}")
        } catch (e: Exception) {
            log.e("mic pump died: $e")
        }
    }

    /**
     * Report the captured signal level periodically while armed.
     *
     * This exists because "is the microphone actually picking anything up" is not answerable from
     * anywhere else in the chain: the box happily encodes and transmits silence, the packet counts
     * climb, and the phone simply hears nothing. Peak and RMS in dBFS turn that into a number.
     *
     * Rough guide for 16-bit PCM:
     *   peak 0            — dead, nothing is being captured at all
     *   peak < ~100 (-50 dBFS) — inaudible; either a synthesised source or a mic that hears nothing
     *   peak 1000-20000   — normal speech
     */
    private fun measureLevel(
        buf: ByteArray,
        len: Int,
    ) {
        var peak = 0
        var sumSq = 0.0
        var zeroRun = 0
        var maxZeroRun = 0
        var i = 0
        while (i + 1 < len) {
            // Little-endian i16, as sent on the wire.
            val v = ((buf[i + 1].toInt() shl 8) or (buf[i].toInt() and 0xFF)).toShort().toInt()
            val a = if (v < 0) -v else v
            if (a > peak) peak = a
            sumSq += (v.toDouble() * v)
            // A live analogue microphone has a noise floor; it does not emit long runs of BIT-EXACT
            // zeros. The audio HAL does, when the USB endpoint under-delivers and it memsets the
            // missing frames.
            if (v == 0) {
                zeroRun++
                if (zeroRun > maxZeroRun) maxZeroRun = zeroRun
            } else {
                zeroRun = 0
            }
            i += 2
        }
        val samples = len / 2
        if (samples == 0) return
        levelSamples += samples
        levelSumSq += sumSq
        if (peak > levelPeak) levelPeak = peak
        if (maxZeroRun > levelMaxZeroRun) levelMaxZeroRun = maxZeroRun

        // The uplink is armed at stream SETUP and (per pi/docs/02 §1a) never disarmed, so "once per
        // second" would be one INFO line per second for the entire session. Report the first few
        // windows — which is what a bring-up session actually needs — then only on a state change.
        if (levelSamples < LEVEL_WINDOW_SAMPLES) return
        val rms = kotlin.math.sqrt(levelSumSq / levelSamples)
        val peakDb = if (levelPeak > 0) 20 * kotlin.math.log10(levelPeak / 32768.0) else -999.0
        val rmsDb = if (rms > 0) 20 * kotlin.math.log10(rms / 32768.0) else -999.0
        val verdict =
            when {
                levelPeak == 0 -> "DEAD (exactly zero — nothing captured)"
                levelPeak < INAUDIBLE_PEAK -> "INAUDIBLE — the phone will hear nothing"
                // Ordered AFTER the level checks on purpose: a dropping stream still has a healthy
                // peak, so this is the only verdict that can be true while everything else looks
                // fine. That combination is exactly what makes it hard to find.
                levelMaxZeroRun >= ZERO_RUN_ALARM -> "CHOPPED — level is fine, but audio is missing"
                else -> "signal present"
            }
        // Log the first few windows (what a bring-up session needs), then only when the verdict
        // changes. Unthrottled this is one INFO line per second for the whole session, because the
        // uplink is armed at SETUP and never disarmed.
        levelWindows++
        if (levelWindows <= LEVEL_VERBOSE_WINDOWS || verdict != lastVerdict) {
            val fmt = "peak=%d (%.1f dBFS) rms=%.0f (%.1f dBFS) — %s"
            log.i(String.format(fmt, levelPeak, peakDb, rms, rmsDb, verdict))
        }
        lastVerdict = verdict
        if (levelMaxZeroRun >= ZERO_RUN_ALARM && !dropoutDetected) {
            dropoutDetected = true
            log.e(
                "capture contains $levelMaxZeroRun-sample runs of EXACT silence — the audio HAL is " +
                    "padding for frames the device never delivered, so the phone receives a chopped " +
                    "stream that a recogniser cannot use. Peak and RMS stay healthy throughout, " +
                    "which is why this needs its own detector. Confirm with " +
                    "`logcat -s AHAL_StreamAlsa` (\"inserting N frames of silence\") and " +
                    "/proc/asound/card*/pcm*c/sub0/status. THIS IS A HAL/USB FAULT — AudioRecord " +
                    "buffer sizing and read cadence cannot affect it; see pi/docs/02.",
            )
        }
        if (levelPeak < INAUDIBLE_PEAK && !silenceDetected) {
            silenceDetected = true
            log.e(
                "microphone level is inaudible — the phone will hear nothing. Check which input " +
                    "device AudioRecord actually opened (dumpsys audio, /proc/asound/card*/pcm*c). " +
                    "NOTE: do NOT assume ro.boot.audio.tinyalsa.simulate_input is the cause — that " +
                    "diagnosis was tested and retracted, see pi/docs/02 §1.",
            )
        }
        levelSamples = 0
        levelSumSq = 0.0
        levelPeak = 0
        levelMaxZeroRun = 0
    }

    private var levelSamples = 0L
    private var levelSumSq = 0.0
    private var levelPeak = 0
    private var levelWindows = 0L
    private var lastVerdict: String? = null
    private var levelMaxZeroRun = 0

    private fun sleepQuietly(ms: Long) {
        try {
            Thread.sleep(ms)
        } catch (_: InterruptedException) {
            Thread.currentThread().interrupt()
        }
    }

    fun diagnostics(): String =
        buildString {
            append("armed=$armed chunks=${chunksSent.get()} bytes=${bytesSent.get()}")
            if (silenceDetected) append(" SILENT(platform)")
            if (dropoutDetected) append(" DROPOUTS(platform)")
        }

    private companion object {
        const val CONNECT_TIMEOUT_MS = 500
        const val RECONNECT_MS = 2000L

        /** 20 ms, which is one AAC-ELD frame at 16 kHz and a natural cadence for the encoder. */
        const val CHUNK_MS = 20

        const val BUFFER_PERIODS = 4
        const val DEFAULT_RATE = 16000

        /** One second at 16 kHz — the measurement window. */
        const val LEVEL_WINDOW_SAMPLES = 16000L

        /** Report this many windows in full, then only on a change of verdict. */
        const val LEVEL_VERBOSE_WINDOWS = 5L

        /**
         * ~-50 dBFS. Below this nothing is audible to a speech recogniser; a real cabin mic with a
         * person speaking peaks orders of magnitude above it.
         */
        const val INAUDIBLE_PEAK = 100

        /**
         * Consecutive bit-exact zero samples that mean the audio was fabricated rather than quiet.
         *
         * A real microphone's noise floor makes 32 exact zeros in a row vanishingly unlikely. The
         * HAL's padding unit is 240 frames at 48 kHz, which after the resample to 16 kHz is an ~80
         * sample run — comfortably above this, while ordinary quiet passages are not.
         */
        const val ZERO_RUN_ALARM = 32
    }
}
