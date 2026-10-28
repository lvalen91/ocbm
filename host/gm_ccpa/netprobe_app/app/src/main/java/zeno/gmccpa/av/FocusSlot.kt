package zeno.gmccpa.av

import zeno.gmccpa.av.VoiceLatency.Purpose

/**
 * Plan v5.8 V2: audio focus as pure state machines over an injected [FocusPort]. No Android here;
 * VoiceRouter's port wraps AudioManager (a fresh, epoch-stamped listener per request, Main looper).
 *
 * Stock (SD:resmanagement/CarPlayAudioManager.java, [STOCK+R]):
 *  - Phone, Siri and Alert request GAIN_TRANSIENT at startAudio ≈ Prepare ≈ SETUP, only if
 *    `!hasFocus()` (:953-959, :997-998). gm_ccpa's BEGIN is written on the same edge.
 *  - A FAILED request → that stream is muted for its life, no retry, nothing to abandon
 *    (:793-795, :998-1001).
 *  - The listener mutes on loss and resumes on gain (:713-736). Abandon-on-loss is NOT adopted
 *    ([UNK, U16]).
 *  - Siri abandons a held but non-streaming telephony focus first (:751-756).
 *  - Alt (nav) never requests focus (:748-750). Owner 2026-10-07: OD-8C = YES, so NAV has no slot
 *    at all (the v5.8 PER_EPOCH NavFocusSlot was deleted with that answer, as the plan's NONE path
 *    says); runtime confirmation T1/T3 (`hasFocus(Alt):3`, Alt unmuted with no CarAudioFocus request).
 * Delayed gain is never asked for (OD-8A: no late nav; stock asks only for Media).
 */
interface FocusPort {
    /**
     * Request transient focus for [purpose] (GAIN_TRANSIENT, or GAIN_TRANSIENT_MAY_DUCK when
     * [mayDuck]). [listener] receives the focus-change codes ([FocusCodes]) for THIS request only.
     */
    fun request(purpose: Purpose, mayDuck: Boolean, listener: (Int) -> Unit): FocusGrant
    fun abandon(handle: Any)
}

/** A request's result ([FocusCodes].REQUEST_*) and the handle to abandon it with (null: nothing to abandon). */
class FocusGrant(val result: Int, val handle: Any?)

/** AudioManager's values, restated so the pure code needs no Android import. */
object FocusCodes {
    const val REQUEST_FAILED = 0
    const val REQUEST_GRANTED = 1
    const val REQUEST_DELAYED = 2
    const val GAIN = 1
    const val LOSS = -1
    const val LOSS_TRANSIENT = -2
    const val LOSS_TRANSIENT_CAN_DUCK = -3

    fun name(c: Int) = when (c) {
        GAIN -> "GAIN"; LOSS -> "LOSS"; LOSS_TRANSIENT -> "LOSS_TRANSIENT"
        LOSS_TRANSIENT_CAN_DUCK -> "LOSS_TRANSIENT_CAN_DUCK"; else -> "focus=$c"
    }

    /** The changes that mute (CAN_DUCK keeps playing, as it always has here). */
    fun mutes(c: Int) = c == LOSS || c == LOSS_TRANSIENT
}

/**
 * One CALL / ASSISTANT / ALERT focus holder. Thread-safe (its own monitor, a leaf: the port is
 * called inside it, and the port never calls back synchronously into the slot).
 *
 * [onChange] is told, outside the monitor, about every change for the CURRENT request (a stale
 * request's late callback is dropped by its epoch) — VoiceRouter turns it into a worker event, so
 * the track act happens on the purpose worker, never on the listener thread.
 */
class FocusSlot(
    val purpose: Purpose,
    private val port: FocusPort,
    private val onChange: (Purpose, Int) -> Unit,
) {
    enum class Outcome { GRANTED, FAILED, ALREADY_HELD }

    private var epoch = 0
    private var handle: Any? = null
    var held = false
        @Synchronized get
        private set
    /** Last focus state of the held request (GAIN until a change says otherwise). */
    var state = FocusCodes.GAIN
        @Synchronized get
        private set
    /**
     * ASSISTANT only: its stream ENDed while iOS said a call is up (a Siri-placed call). The slot is
     * kept and handed over: released at the CALL BEGIN's GRANTED or at phone 1→0 — so media gets no
     * GAIN between Siri's END and the call's BEGIN (fixture 18:32:19.793 / .931 / 20.765).
     */
    var handoverPending = false
        @Synchronized get
        @Synchronized set
    /** Why it was last released, for the logged release cause (B-V1-1: `phone 1→0` or `END`). */
    var lastReleaseCause: String? = null
        @Synchronized get
        private set

    /** Stock `!hasFocus()` guard: request only when not held. */
    fun requestIfNotHeld(): Outcome {
        val ep: Int
        synchronized(this) {
            if (held) return Outcome.ALREADY_HELD
            ep = ++epoch
        }
        val g = port.request(purpose, false) { change -> deliver(ep, change) }
        synchronized(this) {
            if (g.result == FocusCodes.REQUEST_GRANTED) {
                if (ep == epoch) {
                    held = true; handle = g.handle; handoverPending = false
                    // A change the listener delivered before this point (same epoch) already set state.
                    if (!FocusCodes.mutes(state)) state = FocusCodes.GAIN
                    return Outcome.GRANTED
                }
            }
            // FAILED (or DELAYED, which is never asked for): nothing is held. A handle that exists
            // anyway is abandoned below, outside the monitor.
            epoch++
            state = FocusCodes.GAIN
        }
        g.handle?.let { runCatching { port.abandon(it) } }
        return Outcome.FAILED
    }

    /** Abandon if held. Returns true when it was held. */
    fun release(cause: String): Boolean {
        val h: Any?
        synchronized(this) {
            if (!held) return false
            held = false; h = handle; handle = null; epoch++
            handoverPending = false
            state = FocusCodes.GAIN
            lastReleaseCause = cause
        }
        h?.let { runCatching { port.abandon(it) } }
        return true
    }

    private fun deliver(ep: Int, change: Int) {
        synchronized(this) {
            if (ep != epoch) return
            state = change
        }
        onChange(purpose, change)
    }
}
