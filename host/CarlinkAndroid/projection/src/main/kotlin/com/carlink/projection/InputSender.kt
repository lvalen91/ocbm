package com.carlink.projection

import com.carlink.logging.ProbeLog
import com.carlink.ocbm.Ocbm
import java.io.IOException
import java.io.OutputStream
import java.net.InetSocketAddress
import java.net.Socket
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * The input plane: touch, HID buttons and `/command` dispatch, sent to `airplayd` on :9110.
 *
 * Framing is `[len u16 LE][payload]` and the payload opcodes are [Ocbm]'s `INPUT_*` — the same wire
 * the CCPA's OCBM `CH_INPUT` carries, because `airplayd`'s `handle_input_frame` is transport-blind.
 * Sharing `OcbmProto.kt` rather than restating the opcodes here is deliberate: these values are a
 * three-way contract with `crates/ocbm-proto/src/lib.rs` and `:app`, and a drift between them is
 * silent (a wrong opcode is dropped by the box with a log line nobody is reading).
 *
 * ## Why a queue and a writer thread
 *
 * Touch arrives on the UI thread. A blocking `connect()` or a stalled write there would jank the
 * surface, and during a session teardown `airplayd` is gone entirely — so a synchronous send would
 * block the very thread that draws. Everything is therefore queued and drained by one writer, and
 * the queue **drops** rather than blocks when full: stale touch is worthless, and back-pressuring
 * the UI thread to deliver a MOVE from 300 ms ago is strictly worse than losing it.
 *
 * ## Connection is lazy and self-healing
 *
 * There is no session for most of the device's uptime, and `airplayd` is spawned per session, so a
 * refused connect is the normal idle state rather than an error. The writer reconnects on demand and
 * logs only on up↔down transitions.
 */
class InputSender : TouchSink {
    private val log = ProbeLog.sub("input")
    private val running = AtomicBoolean(false)
    private val q = ArrayBlockingQueue<ByteArray>(QUEUE_DEPTH)

    @Volatile private var sock: Socket? = null

    @Volatile private var out: OutputStream? = null

    @Volatile private var thread: Thread? = null

    @Volatile private var up = false

    val sent = AtomicLong(0)
    val dropped = AtomicLong(0)

    /** Throttle state for [requestKeyframe]. */
    private val lastKeyframeMs = AtomicLong(0)

    fun start() {
        if (!running.compareAndSet(false, true)) return
        thread =
            Thread({ run() }, "input-writer").apply {
                isDaemon = true
                start()
            }
    }

    fun stop() {
        running.set(false)
        // closeSocket(), not just sock.close(): leaving `out` set means a restart's first frame is
        // written to a closed stream and dropped — and the OLD writer's own closeSocket(), which
        // runs up to POLL_MS later, would then null the NEW writer's socket out from under it.
        closeSocket()
        up = false
        q.clear()
    }

    // ---- public input API ------------------------------------------------------------------------

    /**
     * One touch contact update. [nx]/[ny] are normalised 0..65535 — the box scales them to the
     * resolution it advertised, so this is resolution-independent by construction and stays correct
     * if the generated VehicleConfig changes.
     *
     * [finger] is a stable per-contact id. `airplayd` maps it to one of Apple's two transducer slots
     * and keeps that mapping for as long as the contact is down, so ids may be reused freely across
     * gestures but must not be reused *within* one.
     */
    override fun touch(
        phase: Byte,
        nx: Int,
        ny: Int,
        finger: Int,
    ) {
        val x = nx.coerceIn(0, U16_MAX)
        val y = ny.coerceIn(0, U16_MAX)
        offer(
            byteArrayOf(
                Ocbm.INPUT_TOUCH,
                phase,
                (x and 0xFF).toByte(),
                ((x shr 8) and 0xFF).toByte(),
                (y and 0xFF).toByte(),
                ((y shr 8) and 0xFF).toByte(),
                (finger and 0xFF).toByte(),
            ),
        )
    }

    /**
     * Ask iOS for a fresh IDR, throttled.
     *
     * `VideoSeam` calls this once per gap **and** once per decrypt failure, so under a wrong key or
     * a torn stream it fires at frame rate. Unthrottled that is a request storm on the encrypted
     * event channel for a condition an IDR cannot fix.
     */
    fun requestKeyframe() {
        val now = System.currentTimeMillis()
        val last = lastKeyframeMs.get()
        if (now - last < KEYFRAME_MIN_INTERVAL_MS) return
        if (!lastKeyframeMs.compareAndSet(last, now)) return
        offer(byteArrayOf(Ocbm.INPUT_KEYFRAME))
    }

    /** Steering-wheel / HID media transport key. Index is the uid-2 Consumer array index, 1..5. */
    fun mediaButton(index: Byte) = offer(byteArrayOf(Ocbm.INPUT_MEDIA_BTN, index))

    /** D-Pad / rotary navigation, uid 3. Only delivered if the box advertised the D-Pad. */
    fun nav(action: Byte) = offer(byteArrayOf(Ocbm.INPUT_NAV, action))

    /** Telephony HID, uid 5: answer / end / mute / DTMF. */
    fun telephony(index: Byte) = offer(byteArrayOf(Ocbm.INPUT_TELEPHONY, index))

    /** Rotary knob, uid 4. Relative rotation, ±1 per detent. */
    fun knob(
        flags: Byte,
        nudgeX: Byte,
        nudgeY: Byte,
        rotation: Byte,
    ) = offer(byteArrayOf(Ocbm.INPUT_KNOB, flags, nudgeX, nudgeY, rotation))

    /**
     * An AirPlay `/command`, dispatched by the box which owns the encrypted event channel.
     *
     * **Two-byte form only.** `airplayd` reads a FIXED operand count per opcode, so an opcode that
     * carries operands cannot be expressed here: a bare `command(CMD_UI_APPEARANCE)` compiles, goes
     * out two bytes short, and is discarded by the box with a log line nobody is reading. Refuse it
     * by name instead of emitting it.
     */
    fun command(cmd: Byte) {
        val operands = COMMAND_OPERAND_BYTES[cmd]
        if (operands != null) {
            log.e(
                "command 0x${(cmd.toInt() and 0xFF).toString(16)} carries $operands operand byte(s) " +
                    "and cannot be sent bare — use a typed helper. The box would drop it silently.",
            )
            dropped.incrementAndGet()
            return
        }
        offer(byteArrayOf(Ocbm.INPUT_COMMAND, cmd))
    }

    /** Bring the CarPlay UI to the foreground on the phone's side. */
    fun requestUi() = command(Ocbm.CMD_REQUEST_UI)

    /**
     * Siri press and release.
     *
     * These are a **hold pair**, not two independent events: iOS keeps the Siri turn open until the
     * buttonup arrives. The bare `requestSiri` is ignored by iOS entirely, which is why there is no
     * one-shot variant here.
     */
    fun siriDown() = command(Ocbm.CMD_SIRI_DOWN)

    fun siriUp() = command(Ocbm.CMD_SIRI_UP)

    /** Drive-state restriction, driven from `CarUxRestrictions`. */
    fun setLimitedUi(limited: Boolean) = command(if (limited) Ocbm.CMD_LIMITED_UI_ON else Ocbm.CMD_LIMITED_UI_OFF)

    /** Day/night, driven from the AAOS UI mode. */
    fun setNightMode(on: Boolean) = offer(byteArrayOf(Ocbm.INPUT_COMMAND, Ocbm.CMD_NIGHT_MODE, if (on) 1 else 0))

    // ---- plumbing --------------------------------------------------------------------------------

    private fun offer(frame: ByteArray) {
        if (frame.size > SeamContract.MAX_INPUT_FRAME) {
            // The box drops the whole CONNECTION on an implausible length, taking every subsequent
            // touch with it. Refusing here keeps one bad caller from killing the input plane.
            log.e("frame of ${frame.size} B exceeds the ${SeamContract.MAX_INPUT_FRAME} B box limit — dropped")
            dropped.incrementAndGet()
            return
        }
        if (!q.offer(frame)) dropped.incrementAndGet()
    }

    private fun run() {
        while (running.get()) {
            val frame =
                try {
                    q.poll(POLL_MS, TimeUnit.MILLISECONDS)
                } catch (_: InterruptedException) {
                    continue
                } ?: continue
            val o = connectIfNeeded()
            if (o == null) {
                // No session, or airplayd is restarting. Dropping is correct: there is nothing on
                // the other side that could act on this frame, and holding it would deliver a stale
                // touch whenever the next session happens to start.
                dropped.incrementAndGet()
                continue
            }
            try {
                // One write for header+payload: two writes on a NODELAY socket are two packets, and
                // the box reads the header with read_exact — a split it handles, but needlessly.
                val msg = ByteArray(2 + frame.size)
                msg[0] = (frame.size and 0xFF).toByte()
                msg[1] = ((frame.size shr 8) and 0xFF).toByte()
                System.arraycopy(frame, 0, msg, 2, frame.size)
                o.write(msg)
                o.flush()
                sent.incrementAndGet()
            } catch (e: IOException) {
                if (up) log.i("input seam down — ${e.message}")
                up = false
                closeSocket()
                dropped.incrementAndGet()
            }
        }
        closeSocket()
    }

    private fun connectIfNeeded(): OutputStream? {
        out?.let { return it }
        return try {
            val s = Socket()
            s.tcpNoDelay = true
            s.connect(InetSocketAddress(SeamContract.LOOPBACK, SeamContract.PORT_INPUT), CONNECT_TIMEOUT_MS)
            sock = s
            out = s.getOutputStream()
            if (!up) log.i("input seam up (:${SeamContract.PORT_INPUT})")
            up = true
            out
        } catch (e: IOException) {
            // Normal while idle — airplayd only exists during a session. Logged once per transition.
            if (up) log.i("input seam connect failed — ${e.message}")
            up = false
            null
        }
    }

    private fun closeSocket() {
        runCatching { sock?.close() }
        sock = null
        out = null
    }

    private companion object {
        /**
         * ~2 s of 60 Hz touch. Deep enough that a brief writer stall costs nothing, shallow enough
         * that a long one drops stale events instead of replaying a gesture the driver has finished.
         */
        const val QUEUE_DEPTH = 128

        const val POLL_MS = 200L
        const val CONNECT_TIMEOUT_MS = 500
        const val KEYFRAME_MIN_INTERVAL_MS = 500L
        const val U16_MAX = 65535

        /**
         * Opcodes that carry operands, keyed by opcode so a NEW one added to `OcbmProto` without a
         * typed helper here fails loudly the first time it is used rather than going out truncated.
         */
        val COMMAND_OPERAND_BYTES =
            mapOf(
                Ocbm.CMD_NAV_APPEARANCE to 1,
                Ocbm.CMD_UI_APPEARANCE to 2,
                Ocbm.CMD_MAP_APPEARANCE to 2,
                Ocbm.CMD_NIGHT_MODE to 1,
            )
    }
}
