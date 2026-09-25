package com.carlink

import kotlinx.coroutines.delay
import java.util.concurrent.atomic.AtomicBoolean

/**
 * The clean-slate reset — ONE sequence shared by Settings ▸ Reset Connection and the loading
 * overlay's Reset Device (owner intent, 2026-09-25: both are deliberate old-protocol-style resets,
 * not warm re-subscribes).
 *
 * Order, and why each step is where it is:
 *
 *  1. [Steps.stopSession] — `CT_STOP`, close the OCBM client, release the USB interface. The box
 *     answers CT_STOP with an IMMEDIATE `go_idle` (`ccpa/ocbmd/src/main.rs`, `fn go_idle`): it
 *     drops `present` + `subscribed` and writes `/tmp/host_present=0`. Nothing more reaches the
 *     host after that — every box→host mirror (`proj_mode_tick`, `phone_tick`, `bt_phase_tick`)
 *     is gated on `subscribed`, and the transport is closed anyway. So there is NO box-reported
 *     "teardown finished" signal for the app to wait on; the wait below is a timer by necessity.
 *  2. [Steps.rotateInstanceNonce] — a NEW host instance nonce for the next `CT_HELLO`, so the box
 *     treats what follows as a replacement host, never as a reattach of the one that just left.
 *  3. Wait [BOX_TEARDOWN_SETTLE_MS] for the box's own session-end lifecycle (derivation below).
 *  4. [Steps.reconnect] — reclaim USB, HELLO with the new nonce, SUBSCRIBE.
 *
 * **Where the wait comes from** (`tools/session_supervisor.sh`, the box's session supervisor):
 *  - The supervisor polls `/tmp/host_present` at 1 Hz (`sleep 1`, `:1461`), so the GONE edge is
 *    consumed up to 1 s after CT_STOP.
 *  - On that edge it runs `wireless_down` (`:1201-1203` → `:1014`): a DETACHED `setsid` block —
 *    SIGTERM btd, `sleep 1` (`:1092`), SIGKILL, then `radio_hal.sh wifi_ap_off` (bounded by a
 *    25 × 0.2 s = 5 s hostapd-exit wait, `ccpa/rootfs/script/radio_hal.sh:470`) and `bt_off`.
 *  - Then `teardown` (`:1386` → `:479`) → `kill_session` (`:291`): SIGTERM carplayd, `sleep 1`
 *    (`:302`), SIGKILL (`:303`), `release_carplay_owner` (`:304`). Synchronous inside the tick, so
 *    projection is released ≤ 2 s after CT_STOP.
 *  - The supervisor's OWN rule for how long to stay clear of a detached teardown before bringing
 *    wireless back up is 4 s (`wireless_rebring_at = now + 4`, `:1190`; the same deferral the
 *    Restart-wireless and CT_RADIO paths ride, `:905`, `:1178-1183`).
 *  - ocbmd holds the flag at 0 for `REARM_HOLD` = 2 s (`main.rs:643`) so even a fast SUBSCRIBE
 *    cannot hide the edge from the 1 Hz poll.
 *
 * So: 1 s (poll) + 4 s (the box's own settle) + 1 s margin = **6 s**. The theoretical worst case
 * of the detached radio teardown is 1 + 1 + 5 = 7 s, reached only when hostapd ignores SIGTERM
 * for the full bound — and then `wifi_ap_off` reports failure anyway.
 *
 * Single-flight: a second [run] while one is in progress returns false and does nothing.
 */
class CleanSlateReset(
    private val steps: Steps,
    private val wait: suspend (Long) -> Unit = { delay(it) },
) {
    enum class Stage {
        /** CT_STOP, close OCBM, release USB. */
        STOPPING,

        /** The box is running its own session-end lifecycle; nothing to observe, so a timer. */
        WAITING_FOR_ADAPTER,

        /** Reclaim USB, HELLO with the rotated nonce, SUBSCRIBE. */
        RECONNECTING,

        /** Sequence finished (success or failure — the reconnect step reports its own failure). */
        DONE,
    }

    interface Steps {
        /** Blocking teardown: CT_STOP + close the client + release the USB interface. */
        fun stopSession()

        /** Replace the host instance nonce; returns the new value (never 0, never the old one). */
        fun rotateInstanceNonce(): Int

        /** Reclaim the adapter and bring a fresh session up. */
        suspend fun reconnect()

        /** Stage transitions, in order, for the status line. */
        fun onStage(stage: Stage)
    }

    private val inFlight = AtomicBoolean(false)

    val isInFlight: Boolean get() = inFlight.get()

    /** Run the sequence. False = another run is in flight; nothing was done. */
    suspend fun run(): Boolean {
        if (!inFlight.compareAndSet(false, true)) return false
        try {
            steps.onStage(Stage.STOPPING)
            steps.stopSession()
            steps.rotateInstanceNonce()
            steps.onStage(Stage.WAITING_FOR_ADAPTER)
            wait(BOX_TEARDOWN_SETTLE_MS)
            steps.onStage(Stage.RECONNECTING)
            steps.reconnect()
        } finally {
            steps.onStage(Stage.DONE)
            inFlight.set(false)
        }
        return true
    }

    companion object {
        /** See the class KDoc for the line-by-line derivation. */
        const val BOX_TEARDOWN_SETTLE_MS = 6_000L
    }
}
