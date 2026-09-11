package zeno.gmccpa.av

import android.annotation.SuppressLint
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import zeno.gmccpa.ProbeLog
import zeno.gmccpa.logging.SessionTrace
import java.io.BufferedReader
import java.io.InputStreamReader
import java.io.OutputStream
import java.net.InetSocketAddress
import java.net.Socket
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * Microphone uplink: vehicle mic → the Rust receiver → the iPhone.
 *
 * The receiver opens `127.0.0.1:9112` at library load. We connect to it and the socket carries BOTH
 * directions:
 *  - IN  (receiver → us): `uplink on <rate> <ch>\n` when iOS SETUPs type 100 with `input=true`, and
 *    `uplink off\n` at teardown. **This is the capture gate.**
 *  - OUT (us → receiver): `mic <len>\n` followed by `<len>` bytes of S16LE PCM.
 *
 * The Rust side does RTP framing, encryption with the stream INPUT key, and the byte-order swap.
 *
 * **Gate on the control line, never on downlink activity.** Siri wants the mic BEFORE any downlink
 * audio arrives, so an activity-based gate clips the onset of every request.
 *
 * **Connect eagerly, not on first data.** The same socket carries the gate, so a data-triggered
 * connect deadlocks: no connection → no gate → no capture → no data.
 *
 * ## This class IS live — both uplink encoders are compiled
 *
 * `carplay-jni/Cargo.toml` enables receiver `mic-uplink` (wired big-endian PCM) and defaults
 * `mic-uplink-eld` ON, so the truck x86_64 `.so` built by `tools/build_apk.sh` carries libfdk-aac
 * and the AAC-ELD entries in `/info` are real; the mic uplink is owner-confirmed on the truck
 * (`docs/13_AUDIO_ROUTING.md` §4). The ELD bail exists only for an aarch64/Pi build made with
 * `--no-default-features`. Do not assume this class is dormant.
 */
class MicUplink {

    private val log = ProbeLog.sub("mic")
    private val running = AtomicBoolean(false)
    private val capturing = AtomicBoolean(false)
    val framesSent = AtomicLong(0)

    @Volatile private var sock: Socket? = null
    @Volatile private var out: OutputStream? = null
    @Volatile private var activeRate = 0
    @Volatile private var activeChannels = 0
    private var gateThread: Thread? = null
    /** Written on `mic-gate`, read by [stopCapture] from the UI thread too — hence volatile. */
    @Volatile private var captureThread: Thread? = null
    /** Capture-thread latch for [EXPECT_FIRST_CHUNK]; [framesSent] is per session, this is per gate. */
    @Volatile private var firstChunkThisCapture = false
    /** Captures started this session, so each capture's first-chunk line is attributable. */
    private var captures = 0
    /** Consecutive failed connects to `:9112`, gate thread only — see [gateLoop] for the severity rule. */
    private var connectFailures = 0

    private companion object {
        const val PORT = 9112
        /** 20 ms at the negotiated rate. Chunk size and period must stay in lockstep or the uplink
         *  underruns — the receiver packetises on the same cadence. */
        const val CHUNK_MS = 20
        /** Reconnect cadence to the gate seam while the receiver's listener is absent. */
        const val RECONNECT_MS = 2_000L
        /** [SessionTrace] name for "the capture the gate just opened has sent a chunk". */
        const val EXPECT_FIRST_CHUNK = "av/mic-first-chunk"
        /**
         * Budget from `uplink on` to the first chunk on the wire. Measured 2026-09-09: `GATE ON`
         * 22:55:39.482 → `capturing VOICE_COMMUNICATION` .694 (212 ms of AudioRecord bring-up on a
         * cold HAL) → `FIRST MIC CHUNK SENT` .744 — **262 ms**. Every failure on that path already
         * logs an `E` or `W` except one: `AudioRecord.read` blocking forever with no data, which is
         * exactly the silent shape a mic route that was never granted takes. 2 s is ~8× measured
         * and still inside a Siri turn, so the miss lands while the driver is waiting on Siri.
         */
        const val FIRST_CHUNK_MS = 2_000L
        /** [SessionTrace.Board] entry. */
        const val BOARD = "mic-uplink"
    }

    fun start() {
        if (!running.compareAndSet(false, true)) return
        SessionTrace.Board.up(BOARD, "connecting to the gate seam :$PORT")
        gateThread = Thread({ gateLoop() }, "mic-gate").apply { isDaemon = true; start() }
    }

    /** [why] is the caller's reason, so the summary and the board both say whether this was expected. */
    fun stop(why: String) {
        running.set(false)
        stopCapture("uplink stopped — $why")
        runCatching { sock?.close() }   // deliberate: teardown of a socket we are discarding
        sock = null; out = null
        log.i("stopped ($why) — $framesSent chunks sent over $captures capture(s)")
        SessionTrace.Board.down(BOARD, "stopped — $why")
    }

    /**
     * Connect and follow the gate. Reconnects while running: the receiver may restart the listener.
     *
     * Severity of a failed connect is by COUNT, not by kind. The first miss is normal — the
     * receiver's `:9112` listener comes up at native-library load, which on the 2026-09-09 truck
     * session was 208 ms AFTER this connect was attempted (attempt at 22:51:14.467, `control-in
     * listening` at .675, connected on the retry at 22:51:16.471). Logging that at `W` on every app
     * start is the false alarm that gets a mechanism ignored. The SECOND consecutive miss means the
     * listener has been absent for over [RECONNECT_MS] + the 2 s connect timeout, which no ordering
     * of our own explains — that one is a `W`, and the board shows the uplink as FAILED until it lands.
     */
    private fun gateLoop() {
        while (running.get()) {
            try {
                val s = Socket().apply { connect(InetSocketAddress("127.0.0.1", PORT), 2000) }
                sock = s; out = s.getOutputStream()
                connectFailures = 0
                log.i("connected to the uplink control seam :$PORT — waiting for the gate")
                SessionTrace.Board.up(BOARD, "gate seam :$PORT connected — gate CLOSED")
                val r = BufferedReader(InputStreamReader(s.getInputStream()))
                while (running.get()) {
                    val line = r.readLine() ?: break
                    handleGate(line.trim())
                }
                if (running.get()) log.w("gate seam :$PORT closed by the receiver — reconnecting in ${RECONNECT_MS}ms")
            } catch (t: Throwable) {
                if (running.get()) {
                    connectFailures++
                    val what = "uplink seam :$PORT: ${t.javaClass.simpleName}: ${t.message}"
                    if (connectFailures == 1) {
                        log.i("$what — the receiver core may not be loaded yet; retrying in ${RECONNECT_MS}ms")
                        SessionTrace.Board.down(BOARD, "gate seam :$PORT not up yet — retrying every ${RECONNECT_MS}ms")
                    } else {
                        log.w("$what — attempt $connectFailures; no mic for Siri until this connects")
                        SessionTrace.Board.note(BOARD, "FAILED (gate seam :$PORT unreachable, $connectFailures attempts — no mic for Siri)")
                    }
                }
            } finally {
                stopCapture("gate seam connection ended")
                runCatching { sock?.close() }; sock = null; out = null   // deliberate: teardown
            }
            if (running.get()) try { Thread.sleep(RECONNECT_MS) } catch (_: InterruptedException) { return }
        }
    }

    private fun handleGate(line: String) {
        when {
            line.startsWith("uplink on") -> {
                // `uplink on <rate> <ch>`
                val p = line.split(" ")
                val rate = p.getOrNull(2)?.toIntOrNull() ?: 16000
                val ch = p.getOrNull(3)?.toIntOrNull() ?: 1
                log.i("GATE ON — ${rate}Hz ${ch}ch")
                startCapture(rate, ch)
            }
            line.startsWith("uplink off") -> {
                log.i("GATE OFF")
                stopCapture("gate closed by the receiver (`uplink off`)")
                SessionTrace.Board.up(BOARD, "gate seam :$PORT connected — gate CLOSED")
            }
            line.isNotEmpty() -> log.i("uplink seam says: $line")
        }
    }

    @SuppressLint("MissingPermission")   // RECORD_AUDIO is declared and granted at install (-g)
    private fun startCapture(rate: Int, channels: Int) {
        // A re-SETUP can change the format, and the peer broadcasts a fresh `uplink on` for each.
        // Without this the CAS below swallows it and we keep capturing at the old rate while the
        // receiver frames and RTP-clocks at the new one.
        if (capturing.get() && (rate != activeRate || channels != activeChannels)) {
            log.i("format changed ${activeRate}Hz${activeChannels}ch -> ${rate}Hz${channels}ch — restarting")
            stopCapture("format changed ${activeRate}Hz${activeChannels}ch -> ${rate}Hz${channels}ch")
        }
        if (!capturing.compareAndSet(false, true)) return
        activeRate = rate; activeChannels = channels
        val mask = if (channels >= 2) AudioFormat.CHANNEL_IN_STEREO else AudioFormat.CHANNEL_IN_MONO
        val minBuf = AudioRecord.getMinBufferSize(rate, mask, AudioFormat.ENCODING_PCM_16BIT)
        // Each failure below already logs its own E; the board gets a note(), not a second E.
        if (minBuf <= 0) {
            log.e("getMinBufferSize failed ($rate/$channels) — no mic for this gate"); capturing.set(false)
            SessionTrace.Board.note(BOARD, "FAILED (getMinBufferSize($rate/$channels) refused — no mic this gate)"); return
        }
        val rec = try {
            AudioRecord(MediaRecorder.AudioSource.VOICE_COMMUNICATION, rate, mask,
                AudioFormat.ENCODING_PCM_16BIT, minBuf * 3)
        } catch (t: Throwable) {
            log.e("AudioRecord: ${t.javaClass.simpleName}: ${t.message} — no mic for this gate"); capturing.set(false)
            SessionTrace.Board.note(BOARD, "FAILED (AudioRecord threw ${t.javaClass.simpleName} — no mic this gate)"); return
        }
        if (rec.state != AudioRecord.STATE_INITIALIZED) {
            log.e("AudioRecord uninitialised — releasing; no mic for this gate"); runCatching { rec.release() }   // deliberate: discard
            capturing.set(false)
            SessionTrace.Board.note(BOARD, "FAILED (AudioRecord uninitialised — no mic this gate)"); return
        }
        runCatching { rec.startRecording() }.onFailure {
            log.e("startRecording: ${it.message} — no mic for this gate"); runCatching { rec.release() }   // deliberate: discard
            capturing.set(false)
            SessionTrace.Board.note(BOARD, "FAILED (startRecording threw — no mic this gate)"); return
        }
        val bytesPerChunk = rate / 1000 * CHUNK_MS * 2 * channels
        firstChunkThisCapture = false
        captures++
        // Armed here, after every synchronous failure above has had its own line: what remains
        // silent is a read() that never returns data. Met in [send] on this capture's first chunk.
        SessionTrace.expect(EXPECT_FIRST_CHUNK, FIRST_CHUNK_MS,
            "the receiver said `uplink on ${rate} ${channels}` and AudioRecord is recording, so PCM " +
            "should reach the seam within one bring-up (262ms on 2026-09-09)")
        captureThread = Thread({ captureLoop(rec, bytesPerChunk) }, "mic-capture").apply {
            isDaemon = true; start()
        }
        log.i("capturing VOICE_COMMUNICATION ${rate}Hz ${channels}ch, ${bytesPerChunk}B/chunk (capture #$captures)")
        SessionTrace.Board.up(BOARD, "gate OPEN — capturing VOICE_COMMUNICATION ${rate}Hz ${channels}ch, ${bytesPerChunk}B/${CHUNK_MS}ms")
    }

    private fun captureLoop(rec: AudioRecord, chunk: Int) {
        android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_URGENT_AUDIO)
        val buf = ByteArray(chunk)
        try {
            while (capturing.get() && running.get()) {
                var off = 0
                while (off < chunk && capturing.get()) {
                    val n = rec.read(buf, off, chunk - off)
                    if (n <= 0) {
                        // Any non-positive read (0, ERROR, ERROR_BAD_VALUE, ERROR_INVALID_OPERATION,
                        // ERROR_DEAD_OBJECT) ends this capture; all are treated alike and none is
                        // retried here — the next `uplink on` re-arms. The
                        // flag MUST be cleared here: the peer only sends `uplink off` on full control
                        // teardown, not per-SETUP, so the gate edge that would have reset it may never
                        // arrive — and iOS re-SETUPs MainAudio several times per Siri turn. Leaving it
                        // set made every later `uplink on` fail its CAS silently and killed the mic for
                        // the rest of the session.
                        if (capturing.get()) {
                            // A mid-gate read failure is the mic dying under Siri: E, and the board
                            // says so until the next `uplink on` re-arms it.
                            log.e("AudioRecord.read -> $n mid-capture; ending this capture — no mic until the next `uplink on`")
                            SessionTrace.Board.note(BOARD, "FAILED (AudioRecord.read -> $n mid-capture — no mic until the next gate)")
                            if (!firstChunkThisCapture) SessionTrace.cancel(EXPECT_FIRST_CHUNK, "AudioRecord.read failed ($n) first — already reported")
                        }
                        capturing.set(false)
                        return
                    }
                    off += n
                }
                if (off == chunk) send(buf, chunk)
            }
        } finally {
            runCatching { rec.stop() }; runCatching { rec.release() }   // deliberate: teardown
        }
    }

    private fun send(pcm: ByteArray, len: Int) {
        val o = out ?: return
        try {
            synchronized(o) {
                o.write("mic $len\n".toByteArray(Charsets.US_ASCII))
                o.write(pcm, 0, len)
                o.flush()
            }
            val n = framesSent.incrementAndGet()
            if (!firstChunkThisCapture) {
                // Per-capture latch (one branch per 20 ms chunk): each gate's first chunk is the
                // event that clears the expectation, and the line names the capture so a session
                // with several Siri turns stays attributable.
                firstChunkThisCapture = true
                log.i("FIRST MIC CHUNK SENT (capture #$captures, session chunk $n)")
                SessionTrace.met(EXPECT_FIRST_CHUNK)
            }
            if (n % 250 == 0L) log.i("$n mic chunks sent")
        } catch (t: Throwable) {
            log.w("mic write failed: ${t.message}")
            capturing.set(false)
        }
    }

    /** [why] is what the board and the cancelled expectation (if any) record. */
    private fun stopCapture(why: String) {
        if (!capturing.compareAndSet(true, false)) return
        if (!firstChunkThisCapture) SessionTrace.cancel(EXPECT_FIRST_CHUNK, why)
        captureThread?.let { t ->
            runCatching { t.join(500) }   // deliberate: an interrupt here is the caller's, not a fault
            // A join that TIMES OUT is a fault, and it was invisible: the thread is still inside
            // AudioRecord.read while `captureThread` is nulled below, so the next `uplink on` builds a
            // second recorder beside the stuck one.
            if (t.isAlive) log.w("capture thread still alive 500ms after stop ($why) — AudioRecord.read is stuck; the next gate starts a second recorder")
        }
        captureThread = null   // the capture thread owns AudioRecord stop()/release() in its finally
        log.i("capture stopped — $why")
    }
}
