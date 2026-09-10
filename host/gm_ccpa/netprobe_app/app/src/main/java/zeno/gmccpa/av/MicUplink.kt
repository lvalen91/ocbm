package zeno.gmccpa.av

import android.annotation.SuppressLint
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import zeno.gmccpa.ProbeLog
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
    @Volatile private var recorder: AudioRecord? = null
    @Volatile private var captureThreadRef: Thread? = null
    @Volatile private var activeRate = 0
    @Volatile private var activeChannels = 0
    private var gateThread: Thread? = null
    private var captureThread: Thread? = null

    private companion object {
        const val PORT = 9112
        /** 20 ms at the negotiated rate. Chunk size and period must stay in lockstep or the uplink
         *  underruns — the receiver packetises on the same cadence. */
        const val CHUNK_MS = 20
    }

    fun start() {
        if (!running.compareAndSet(false, true)) return
        gateThread = Thread({ gateLoop() }, "mic-gate").apply { isDaemon = true; start() }
    }

    fun stop() {
        running.set(false)
        stopCapture()
        runCatching { sock?.close() }
        sock = null; out = null
        log.i("stopped — $framesSent chunks sent")
    }

    /** Connect and follow the gate. Reconnects while running: the receiver may restart the listener. */
    private fun gateLoop() {
        while (running.get()) {
            try {
                val s = Socket().apply { connect(InetSocketAddress("127.0.0.1", PORT), 2000) }
                sock = s; out = s.getOutputStream()
                log.i("connected to the uplink control seam :$PORT — waiting for the gate")
                val r = BufferedReader(InputStreamReader(s.getInputStream()))
                while (running.get()) {
                    val line = r.readLine() ?: break
                    handleGate(line.trim())
                }
            } catch (t: Throwable) {
                if (running.get()) log.w("uplink seam: ${t.javaClass.simpleName}: ${t.message}")
            } finally {
                stopCapture()
                runCatching { sock?.close() }; sock = null; out = null
            }
            if (running.get()) try { Thread.sleep(2000) } catch (_: InterruptedException) { return }
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
            line.startsWith("uplink off") -> { log.i("GATE OFF"); stopCapture() }
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
            stopCapture()
        }
        if (!capturing.compareAndSet(false, true)) return
        activeRate = rate; activeChannels = channels
        val mask = if (channels >= 2) AudioFormat.CHANNEL_IN_STEREO else AudioFormat.CHANNEL_IN_MONO
        val minBuf = AudioRecord.getMinBufferSize(rate, mask, AudioFormat.ENCODING_PCM_16BIT)
        if (minBuf <= 0) { log.e("getMinBufferSize failed ($rate/$channels)"); capturing.set(false); return }
        val rec = try {
            AudioRecord(MediaRecorder.AudioSource.VOICE_COMMUNICATION, rate, mask,
                AudioFormat.ENCODING_PCM_16BIT, minBuf * 3)
        } catch (t: Throwable) {
            log.e("AudioRecord: ${t.javaClass.simpleName}: ${t.message}"); capturing.set(false); return
        }
        if (rec.state != AudioRecord.STATE_INITIALIZED) {
            log.e("AudioRecord uninitialised — releasing"); runCatching { rec.release() }
            capturing.set(false); return
        }
        recorder = rec
        runCatching { rec.startRecording() }.onFailure {
            log.e("startRecording: ${it.message}"); runCatching { rec.release() }
            recorder = null; capturing.set(false); return
        }
        val bytesPerChunk = rate / 1000 * CHUNK_MS * 2 * channels
        captureThread = Thread({ captureLoop(rec, bytesPerChunk) }, "mic-capture").apply {
            isDaemon = true; start()
        }
        captureThreadRef = captureThread
        log.i("capturing VOICE_COMMUNICATION ${rate}Hz ${channels}ch, ${bytesPerChunk}B/chunk")
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
                        if (capturing.get()) log.w("AudioRecord.read -> $n; ending capture (recoverable)")
                        capturing.set(false)
                        return
                    }
                    off += n
                }
                if (off == chunk) send(buf, chunk)
            }
        } finally {
            runCatching { rec.stop() }; runCatching { rec.release() }
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
            if (n == 1L) log.i("FIRST MIC CHUNK SENT")
            if (n % 250 == 0L) log.i("$n mic chunks sent")
        } catch (t: Throwable) {
            log.w("mic write failed: ${t.message}")
            capturing.set(false)
        }
    }

    private fun stopCapture() {
        if (!capturing.compareAndSet(true, false)) return
        (captureThread ?: captureThreadRef)?.let { runCatching { it.join(500) } }
        captureThread = null
        recorder = null   // the capture thread owns stop()/release() in its finally
        log.i("capture stopped")
    }
}
