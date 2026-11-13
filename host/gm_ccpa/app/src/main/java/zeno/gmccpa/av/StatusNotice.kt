package zeno.gmccpa.av

import zeno.gmccpa.LinkState
import zeno.gmccpa.StallCause

/**
 * plan v5.9 D6 / OD-9E (owner 2026-10-08: YES): the ONE ongoing notification, state-driven, shown
 * while anything holds [CarPlaySessionService] — the adapter LINK hold and/or a CarPlay session. It
 * replaces the old static session-only notification; there is never a
 * second notification. Pure (no android.*): StatusNoticeTest.
 *
 * **No PII.** No phone name, no adapter Bluetooth name, no SSID, no address: the drawer is reachable
 * by passengers and appears in screenshots, and the state word carries all this needs. The inputs are
 * therefore only the state, the stall cause and whether a relink owns the claim.
 */
data class Notice(val title: String, val text: String)

object StatusNotice {
    private const val APP = "GM CCPA"

    // D-F6 (2026-10-08): FAILED without a stall, RESTARTING, STOPPED and the initial null/IDLE used to
    // fall through to "Adapter connected", which was false for every one of them. Each now has its
    // own line; the set stays closed (StatusNoticeTest counts it) and carries no PII.
    fun of(state: LinkState?, stall: StallCause?, relinking: Boolean): Notice = when {
        state == LinkState.LIVE -> Notice("CarPlay active", "Wireless CarPlay session running")
        state == LinkState.FAILED && stall != null -> Notice("CarPlay interrupted", "Recovering connection")
        relinking -> Notice("$APP — adapter reconnecting", "Reconnecting to the adapter")
        state == LinkState.FAILED -> Notice("$APP — CarPlay problem", "Open GM CCPA for details")
        state == LinkState.RESTARTING -> Notice(APP, "Restarting the session")
        state == LinkState.STOPPED -> Notice(APP, "Stopped")
        state == null || state == LinkState.IDLE -> Notice(APP, "Starting")
        state == LinkState.WAITING -> Notice("$APP — adapter connected", "Waiting for iPhone")
        state == LinkState.PAIRING -> Notice("$APP — adapter connected", "Pairing with iPhone")
        state == LinkState.CLAIMING || state == LinkState.SEARCHING -> Notice(APP, "Connecting to the adapter")
        else -> Notice(APP, "Adapter connected")   // PHONE_DETECTED, STARTING
    }
}

/**
 * D6 rev c C3: at most one notification post per [minGapMs], and the LAST state always lands. Pure;
 * every call is made under the [FgsGate] lock by [NoticePoster].
 */
class NoticeThrottle(private val minGapMs: Long = 1_000L) {
    sealed class Act {
        object None : Act()
        object PostNow : Act()
        /** Schedule ONE trailing post [delayMs] from now. */
        data class Schedule(val delayMs: Long) : Act()
    }

    private var lastPostAt = Long.MIN_VALUE
    var lastPosted: Any? = null; private set
    private var trailing = false

    fun onChange(value: Any, nowMs: Long): Act = when {
        value == lastPosted -> Act.None
        lastPostAt == Long.MIN_VALUE || nowMs - lastPostAt >= minGapMs -> Act.PostNow
        trailing -> Act.None
        else -> { trailing = true; Act.Schedule(lastPostAt + minGapMs - nowMs) }
    }

    /** The trailing post fired: post [current] only if it differs from what was last posted. */
    fun onTrailing(current: Any): Boolean { trailing = false; return current != lastPosted }

    fun markPosted(value: Any, nowMs: Long) { lastPosted = value; lastPostAt = nowMs }
}

/**
 * The update path of the one notification (D6 rev c C3), pure so the orphan guard is testable
 * (NoticeThrottleTest). [post] is `NotificationManager.notify(NOTIF_ID, build(n))`; [schedule] a
 * main-looper postDelayed. Every [post] runs under the gate lock and only while [FgsGate] says the
 * service's notification is posted, so nothing can re-post an ongoing notification no service owns.
 */
class NoticePoster(
    private val gate: FgsGate,
    private val clock: () -> Long,
    private val schedule: (Long, () -> Unit) -> Unit,
    private val post: (Notice) -> Unit,
    initial: Notice,
) {
    /** The latest notice from SessionCore's state funnel; onStartCommand builds from it. */
    @Volatile var pending: Notice = initial
        private set
    private val throttle = NoticeThrottle()

    fun update(n: Notice) {
        pending = n
        gate.whenPosted {
            when (val a = throttle.onChange(n, clock())) {
                NoticeThrottle.Act.PostNow -> { post(n); throttle.markPosted(n, clock()) }
                is NoticeThrottle.Act.Schedule -> schedule(a.delayMs) { trailing() }
                NoticeThrottle.Act.None -> Unit
            }
        }
    }

    private fun trailing() {
        gate.locked { posted ->
            val cur = pending
            if (throttle.onTrailing(cur) && posted) { post(cur); throttle.markPosted(cur, clock()) }
        }
    }

    /**
     * Inside `FgsGate.onStarted`'s STAYS branch (under the lock), right after the flag is set:
     * startForeground showed [shown], built before the flag existed; re-post the latest at once if a
     * state landed in between (the build -> flag gap).
     */
    fun onStartedStays(shown: Notice) {
        throttle.markPosted(shown, clock())
        val cur = pending
        if (cur != shown) { post(cur); throttle.markPosted(cur, clock()) }
    }
}
