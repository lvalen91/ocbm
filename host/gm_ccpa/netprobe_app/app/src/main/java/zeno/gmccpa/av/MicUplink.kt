package zeno.gmccpa.av

import android.annotation.SuppressLint
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import zeno.gmccpa.ProbeLog
import zeno.gmccpa.logging.SessionTrace
import zeno.gmccpa.pair.NativeCore
import java.io.BufferedReader
import java.io.IOException
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
 *  - OUT (us → receiver): first `auth <hex>\n` (F136 — the per-process token from
 *    [NativeCore.micAuth]; the receiver closes a connection that does not open with it in time and
 *    tells it nothing until it does), then `mic <len>\n` followed by `<len>` bytes of S16LE PCM.
 *  - The receiver answers a valid `auth` with `auth ok\n` BEFORE any gate line. The seam counts as
 *    up only after that ack (F136 fix round 1): a wrong token, a full authenticated cap or a stale
 *    listener close the socket without it, and that is a failed attempt, not a connection.
 *  - We bind our socket to 127.0.0.1 BEFORE connecting and hand that port to [NativeCore.micAuth],
 *    which reserves it: other apps flooding :9112 fill only the receiver's unreserved pool, never
 *    the slot our connection gets.
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
    /** Captures started this session, so each capture's first-chunk line is attributable. */
    private var captures = 0

    /** Where a capture thread is, for the stop-timeout line: the code cannot otherwise tell these apart. */
    private enum class Phase { READ, SEND, TEARDOWN }

    /**
     * One capture's own state (2026-10-06, P5). Every capture used to share [capturing], so a STALE
     * capture thread that unblocked after the next one had started re-tested the NEW capture's
     * `true`: its failed read then logged a false E and cleared the flag, ending the new capture
     * (no mic until the next gate), or its data was interleaved into the new uplink. A capture thread
     * now tests only its own [live]; it may clear [capturing] only while it is still [current], under
     * [capLock]; and only the owning thread's finally releases its own [rec].
     */
    private class Capture(val rec: AudioRecord, val n: Int) {
        val live = AtomicBoolean(true)
        @Volatile var phase = Phase.READ
        @Volatile var thread: Thread? = null
        /** Per-capture latch for [EXPECT_FIRST_CHUNK]; [framesSent] is per session. */
        @Volatile var firstChunk = false
        /** elapsedRealtime of [stopCapture] on this capture; 0 = it ended on its own. */
        @Volatile var stoppedAt = 0L
        @Volatile var exit = "gate"
    }

    /** Guards the start CAS + publish, the stop CAS + live clear, and a capture's own-failure clear. */
    private val capLock = Any()
    /** The capture [capturing] belongs to; published BEFORE `startRecording()`. Null between captures. */
    @Volatile private var current: Capture? = null
    /** A stopped capture whose thread outlived the 500 ms join; re-stopped and re-joined by the next start. */
    @Volatile private var staleCapture: Capture? = null
    /** Consecutive failed connects to `:9112`, gate thread only — see [gateLoop] for the severity rule. */
    private var connectFailures = 0
    /** The no-token condition was logged (once per instance, not once per 2 s attempt). Gate thread only. */
    private var tokenMissingLogged = false
    /** This attempt's bound source port, reserved via [NativeCore.micAuth]; 0 = none. Gate thread only. */
    private var reservedPort = 0

    private companion object {
        const val PORT = 9112
        /** 20 ms at the negotiated rate. Chunk size and period must stay in lockstep or the uplink
         *  underruns — the receiver packetises on the same cadence. */
        const val CHUNK_MS = 20
        /** Reconnect cadence to the gate seam while the receiver's listener is absent. */
        const val RECONNECT_MS = 2_000L
        /** Wait for the receiver's `auth ok`. It answers within its own 1 s auth window for our
         *  reserved port, so 1.5 s never times out a receiver that is still deciding. */
        const val AUTH_ACK_MS = 1_500
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
     * An attempt fails on a connect error, on the receiver closing without `auth ok` (wrong token,
     * or both live slots held — e.g. a superseded session's socket not yet closed), or on no ack
     * within [AUTH_ACK_MS]. A missing token is NOT an attempt failure: it is logged once, below.
     *
     * Severity is by COUNT, not by kind. One isolated miss has an innocent explanation (the
     * superseded-session overlap above clears within a reconnect), so the first is `I`. The SECOND
     * consecutive miss is a `W`, and the board shows the uplink as FAILED until it lands.
     * (CORRECTED 2026-10-07, F136 fix round 2: this used to call the first miss the native-library-
     * load race — the 2026-09-09 truck session connected 208 ms before `control-in listening`. That
     * race is gone: [NativeCore.micAuth] loads the library, `JNI_OnLoad` binds the listener
     * synchronously, and the token exists only once it is bound.)
     */
    private fun gateLoop() {
        while (running.get()) {
            try {
                // Bound BEFORE the connect, so the port is known (and held — no SO_REUSEADDR) when
                // the core reserves it. NEVER log the token.
                val s = Socket()
                sock = s
                s.reuseAddress = false
                s.bind(InetSocketAddress("127.0.0.1", 0))
                reservedPort = s.localPort
                val token = NativeCore.micAuth(reservedPort)
                if (token == null) {
                    // Not a connect failure, and it does not change between attempts: the core is not
                    // loadable, the .so is stale, or the receiver's listener never bound. Say so ONCE.
                    if (!tokenMissingLogged) {
                        tokenMissingLogged = true
                        log.w("no mic token from the native core (library not loaded, stale .so, or the :$PORT listener " +
                            "did not start — e.g. another process holds the port) — no mic for Siri; retrying silently every ${RECONNECT_MS}ms")
                        SessionTrace.Board.note(BOARD, "FAILED (no mic token: the receiver's :$PORT listener is not ours or not up — no mic for Siri)")
                    }
                } else {
                    if (tokenMissingLogged) {
                        tokenMissingLogged = false
                        log.i("mic token now available — connecting to :$PORT")
                    }
                    s.connect(InetSocketAddress("127.0.0.1", PORT), 2000)
                    // The auth line goes out BEFORE [out] is published, so no `mic` chunk can precede it.
                    val o = s.getOutputStream()
                    o.write("auth $token\n".toByteArray(Charsets.US_ASCII)); o.flush()
                    // Up only once the receiver ACCEPTED the auth: `auth ok` precedes any gate line.
                    s.soTimeout = AUTH_ACK_MS
                    val r = BufferedReader(InputStreamReader(s.getInputStream(), Charsets.US_ASCII))
                    val ack = r.readLine()
                    if (ack?.trim() != "auth ok") {
                        throw IOException(if (ack == null) "receiver closed without `auth ok` (wrong token, connection cap, or not our listener)"
                            else "receiver answered the auth with something other than `auth ok`")
                    }
                    s.soTimeout = 0
                    out = o
                    connectFailures = 0
                    log.i("connected to the uplink control seam :$PORT (auth accepted) — waiting for the gate")
                    SessionTrace.Board.up(BOARD, "gate seam :$PORT connected — gate CLOSED")
                    while (running.get()) {
                        val line = r.readLine() ?: break
                        handleGate(line.trim())
                    }
                    if (running.get()) log.w("gate seam :$PORT closed by the receiver — reconnecting in ${RECONNECT_MS}ms")
                }
            } catch (t: Throwable) {
                if (running.get()) {
                    connectFailures++
                    val what = "uplink seam :$PORT: ${t.javaClass.simpleName}: ${t.message}"
                    if (connectFailures == 1) {
                        log.i("$what — attempt failed (connect error, auth refused, or no `auth ok`); retrying in ${RECONNECT_MS}ms")
                        SessionTrace.Board.down(BOARD, "gate seam :$PORT attempt failed — retrying every ${RECONNECT_MS}ms")
                    } else {
                        log.w("$what — attempt $connectFailures; no mic for Siri until this connects")
                        SessionTrace.Board.note(BOARD, "FAILED (gate seam :$PORT unreachable, $connectFailures attempts — no mic for Siri)")
                    }
                }
            } finally {
                stopCapture("gate seam connection ended")
                // Withdraw the source-port reservation WHILE the socket still holds the port: after a
                // failed connect it would otherwise stay claimable by whatever socket gets the port
                // next. A no-op when the receiver already consumed it on accept.
                if (reservedPort > 0) { NativeCore.micRelease(reservedPort); reservedPort = 0 }
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
        if (!synchronized(capLock) { capturing.compareAndSet(false, true) }) return
        // A previous capture thread that outlived its stop: re-issue the stop on its own recorder and
        // join again. Still alive -> say so and proceed: a mic for this turn beats a silent uplink, and
        // the stale thread can no longer stop this capture or feed into it (it tests only its own live).
        staleCapture?.let { st ->
            val t = st.thread
            if (t != null && t.isAlive) {
                runCatching { st.rec.stop() }   // deliberate: best effort on a recorder we are abandoning
                runCatching { t.join(500) }
            }
            if (t != null && t.isAlive) {
                log.e("previous capture thread still alive (capture #${st.n}, phase=${st.phase}) — starting a second AudioRecord")
                SessionTrace.Board.failed(BOARD, "capture #${st.n}'s thread is stuck (phase=${st.phase}) — a second AudioRecord is being started beside it")
            } else staleCapture = null
        }
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
        captures++
        val cap = Capture(rec, captures)
        val bytesPerChunk = rate / 1000 * CHUNK_MS * 2 * channels
        // Armed BEFORE the publish, so a stop that finds this capture always finds the expectation
        // armed and cancels it (armed after, a stop in between left a false EXPECTED-MISSING). What
        // remains silent is a read() that never returns data. Met in [send] on the first chunk.
        SessionTrace.expect(EXPECT_FIRST_CHUNK, FIRST_CHUNK_MS,
            "the receiver said `uplink on ${rate} ${channels}` and AudioRecord is recording, so PCM " +
            "should reach the seam within one bring-up (262ms on 2026-09-09)")
        // Published BEFORE startRecording, so a stop from here on finds it and stops its recorder.
        synchronized(capLock) { current = cap }
        runCatching { rec.startRecording() }.onFailure {
            log.e("startRecording: ${it.message} — no mic for this gate"); runCatching { rec.release() }   // deliberate: discard
            synchronized(capLock) { cap.live.set(false); if (current === cap) { current = null; capturing.set(false) } }
            SessionTrace.cancel(EXPECT_FIRST_CHUNK, "startRecording threw — already reported")
            SessionTrace.Board.note(BOARD, "FAILED (startRecording threw — no mic this gate)"); return
        }
        // A stop that landed between the publish and here: no capture thread ever owned this
        // recorder, so it is released on this thread.
        if (!(cap.live.get() && capturing.get())) {
            runCatching { rec.stop() }; runCatching { rec.release() }   // deliberate: discard
            synchronized(capLock) { if (current === cap) current = null }
            SessionTrace.cancel(EXPECT_FIRST_CHUNK, "capture #${cap.n} stopped before it started")
            log.i("capture #${cap.n} stopped before its thread started — recorder released")
            return
        }
        val t = Thread({ captureLoop(cap, bytesPerChunk) }, "mic-capture").apply { isDaemon = true }
        cap.thread = t
        t.start()
        log.i("capturing VOICE_COMMUNICATION ${rate}Hz ${channels}ch, ${bytesPerChunk}B/chunk (capture #$captures)")
        SessionTrace.Board.up(BOARD, "gate OPEN — capturing VOICE_COMMUNICATION ${rate}Hz ${channels}ch, ${bytesPerChunk}B/${CHUNK_MS}ms")
    }

    private fun captureLoop(cap: Capture, chunk: Int) {
        android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_URGENT_AUDIO)
        val rec = cap.rec
        val buf = ByteArray(chunk)
        try {
            while (cap.live.get() && running.get()) {
                var off = 0
                while (off < chunk && cap.live.get()) {
                    cap.phase = Phase.READ
                    val n = rec.read(buf, off, chunk - off)
                    if (n <= 0) {
                        // Any non-positive read (0, ERROR, ERROR_BAD_VALUE, ERROR_INVALID_OPERATION,
                        // ERROR_DEAD_OBJECT) ends this capture; all are treated alike and none is
                        // retried here — the next `uplink on` re-arms. The
                        // flag MUST still be cleared here, defensively. (Corrected 2026-10-08, C-F3:
                        // the receiver now sends `uplink off` per type-100 partial TEARDOWN as well —
                        // `uplink.rs` `clear()` -> `notify_control("uplink off")`, called from
                        // `session.rs`'s teardown arm, which cites the "424 armed / 0 off" bench
                        // measurement this comment used to describe as current — so the gate edge
                        // normally does arrive once per Siri turn. A read that dies before it, or a
                        // lost line, would otherwise leave the flag set and make every later
                        // `uplink on` fail its CAS silently, killing the mic for the session.) Only
                        // THIS capture's: [endOwn] clears [capturing] only while this capture is still
                        // [current].
                        if (endOwn(cap, "read=$n")) {
                            // A mid-gate read failure is the mic dying under Siri: E, and the board
                            // says so until the next `uplink on` re-arms it. A read ended by our own
                            // stop (live already cleared) is not one.
                            log.e("AudioRecord.read -> $n mid-capture; ending this capture — no mic until the next `uplink on`")
                            SessionTrace.Board.note(BOARD, "FAILED (AudioRecord.read -> $n mid-capture — no mic until the next gate)")
                            if (!cap.firstChunk) SessionTrace.cancel(EXPECT_FIRST_CHUNK, "AudioRecord.read failed ($n) first — already reported")
                        }
                        return
                    }
                    off += n
                }
                // A capture stopped mid-read must not put its last chunk into the uplink.
                // v5.9 MIC: a bench-armed clip (MicInject; armable only in a bench build) replaces this
                // chunk in place — same thread, same send(), paced by this read. Idle: one atomic read.
                if (off == chunk && cap.live.get()) { cap.phase = Phase.SEND; MicInject.nextChunk(cap, cap.n, rec.sampleRate, rec.channelCount, chunk)?.copyInto(buf); send(cap, buf, chunk) }
            }
        } finally {
            cap.phase = Phase.TEARDOWN
            runCatching { rec.stop() }; runCatching { rec.release() }   // deliberate: teardown
            val since = cap.stoppedAt
            log.i("capture loop exited (capture #${cap.n}, reason=${cap.exit}, " +
                (if (since > 0L) "${android.os.SystemClock.elapsedRealtime() - since} ms after stop)" else "not stopped from outside)"))
        }
    }

    /**
     * A capture ending on its own failure: clears its own [Capture.live], and [capturing] only while
     * it is still [current] — atomically with [stopCapture]/[startCapture] under [capLock], so a
     * stale thread can never clear a newer capture's flag. True if it was still live (a real fault,
     * not our own stop).
     */
    private fun endOwn(cap: Capture, reason: String): Boolean = synchronized(capLock) {
        val wasLive = cap.live.getAndSet(false)
        if (wasLive) cap.exit = reason
        if (current === cap) { current = null; capturing.set(false) }
        wasLive
    }

    private fun send(cap: Capture, pcm: ByteArray, len: Int) {
        val o = out ?: return
        try {
            synchronized(o) {
                if (!cap.live.get()) return
                o.write("mic $len\n".toByteArray(Charsets.US_ASCII))
                o.write(pcm, 0, len)
                o.flush()
            }
            val n = framesSent.incrementAndGet()
            if (!cap.firstChunk) {
                // Per-capture latch (one branch per 20 ms chunk): each gate's first chunk is the
                // event that clears the expectation, and the line names the capture so a session
                // with several Siri turns stays attributable.
                cap.firstChunk = true
                log.i("FIRST MIC CHUNK SENT (capture #${cap.n}, session chunk $n)")
                SessionTrace.met(EXPECT_FIRST_CHUNK)
            }
            if (n % 250 == 0L) log.i("$n mic chunks sent")
        } catch (t: Throwable) {
            if (endOwn(cap, "write failed")) log.w("mic write failed: ${t.message}")
        }
    }

    /** [why] is what the board and the cancelled expectation (if any) record. */
    private fun stopCapture(why: String) {
        val cap = synchronized(capLock) {
            if (!capturing.compareAndSet(true, false)) return
            current.also { c -> current = null; c?.live?.set(false) }
        }
        // Null: the start had not published its capture yet; its own re-check releases the recorder.
        if (cap != null) {
            cap.stoppedAt = android.os.SystemClock.elapsedRealtime()
            if (!cap.firstChunk) SessionTrace.cancel(EXPECT_FIRST_CHUNK, why)
            // The cross-thread unblock for a read parked in the recorder. Whether stop() returns a
            // blocked read() on this unit is AOSP behaviour not verified here (52I proof (2)).
            // release() stays in the capture thread's finally — never from a foreign thread.
            runCatching { cap.rec.stop() }   // deliberate: the capture thread reports its own exit
            cap.thread?.let { t ->
                runCatching { t.join(500) }   // deliberate: an interrupt here is the caller's, not a fault
                // A join that TIMES OUT is a fault: kept as [staleCapture] so the next `uplink on`
                // re-stops and re-joins it before building a second recorder. The phase names where it
                // is; the code cannot tell a stuck read from a stuck socket write or teardown.
                if (t.isAlive) {
                    staleCapture = cap
                    log.w("capture thread #${cap.n} still alive 500ms after stop ($why), phase=${cap.phase} — the next gate re-stops it before starting a second recorder")
                }
            }
        }
        log.i("capture stopped — $why")
    }
}
