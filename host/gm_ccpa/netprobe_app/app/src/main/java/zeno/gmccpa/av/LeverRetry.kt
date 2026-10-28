package zeno.gmccpa.av

/**
 * The vehicle-lever retry schedule (VehicleStateWatcher.reassert), kept free of android.* so the host
 * JUnit tests run the exact decisions the watcher makes.
 *
 * A refused assert is retried every [RETRY_DELAY_MS] up to [RETRY_MAX] times (~8 s; the gap between
 * "session up" and "event channel usable" measured 54 ms on 2026-09-08). F100 / OD-7D (owner,
 * 2026-10-07): past that the E line is written ONCE and the levers keep being re-asserted every
 * [PERSIST_DELAY_MS] while the session is up and the watcher is not stopped, until delivered.
 * Evidence for the change: logcat_cp4 20:04:57.080 gave up after 20 tries on a live session with
 * night=true never delivered.
 *
 * At most one chain per session generation persists: a second chain that exhausts its retries while
 * a persister is already running ends silently, because the persister re-reads the desired state on
 * every tick and so covers whatever the second chain wanted to send.
 */
internal object LeverRetry {
    /** ~8 s of cover at 400 ms — comfortably past the observed 54 ms gap without spinning. */
    const val RETRY_MAX = 20
    const val RETRY_DELAY_MS = 400L
    /** F100: the re-assert cadence once the fast retries are spent. */
    const val PERSIST_DELAY_MS = 2_000L

    enum class Step {
        /** Fast retry: schedule attempt+1 after [RETRY_DELAY_MS]. */
        RETRY,
        /** Retries spent, no persister yet: write the E line once, become the persister. */
        GIVE_UP_AND_PERSIST,
        /** Retries spent, but a persister for this session already runs: end without a line. */
        COVERED,
        /** This chain is the persister: schedule the next tick after [PERSIST_DELAY_MS]. */
        PERSIST,
    }

    /**
     * What to do after attempt number [attempt] (0-based) was refused. [isPersister] = this chain
     * already holds the persist role; [persisterLive] = some chain of the CURRENT session does.
     */
    fun afterRefusal(attempt: Int, isPersister: Boolean, persisterLive: Boolean): Step = when {
        isPersister -> Step.PERSIST
        attempt < RETRY_MAX -> Step.RETRY
        persisterLive -> Step.COVERED
        else -> Step.GIVE_UP_AND_PERSIST
    }

    fun delayFor(step: Step): Long? = when (step) {
        Step.RETRY -> RETRY_DELAY_MS
        Step.GIVE_UP_AND_PERSIST, Step.PERSIST -> PERSIST_DELAY_MS
        Step.COVERED -> null
    }

    /** Whole seconds from [sinceMs] (the persisting chain's first attempt) to [nowMs], for the I line. */
    fun deliveredAfterS(sinceMs: Long, nowMs: Long): Long = ((nowMs - sinceMs).coerceAtLeast(0L)) / 1000L
}

/**
 * The F100 persister bookkeeping VehicleStateWatcher.reassert runs on its single `cp-vehicle` thread
 * (so no lock here: confine every call to that thread). Pure, so the host JUnit tests drive the same
 * state machine the watcher does.
 *
 * A chain is one reassert sequence: it carries the session generation it started in ([gen]), the
 * elapsedRealtime of its first attempt, and a token that is 0 until the chain becomes the persister.
 */
internal class LeverChains {
    private var persistToken = 0L
    private var persistSeq = 0L
    private var persistGen = -1
    private var persistSince = 0L

    /**
     * Top of every tick. False ends the chain: the session is down or a newer one began, the watcher
     * is stopped, or this chain was the persister and its role was cleared (delivered) or taken over.
     */
    fun shouldRun(sessionUp: Boolean, stopped: Boolean, gen: Int, currentGen: Int, token: Long): Boolean =
        sessionUp && !stopped && gen == currentGen && (token == 0L || token == persistToken)

    /** What a refused attempt does next; [token] is the chain's token AFTER this call. */
    class Next(val step: LeverRetry.Step, val delayMs: Long?, val token: Long) {
        /** Write the E line: only the chain that just became the persister does. */
        val logGiveUp: Boolean get() = step == LeverRetry.Step.GIVE_UP_AND_PERSIST
    }

    fun onRefused(attempt: Int, gen: Int, token: Long, startedAt: Long): Next {
        val live = persistToken != 0L && persistGen == gen
        val step = LeverRetry.afterRefusal(attempt, isPersister = token != 0L, persisterLive = live)
        var t = token
        if (step == LeverRetry.Step.GIVE_UP_AND_PERSIST) {
            t = ++persistSeq
            persistToken = t
            persistGen = gen
            persistSince = startedAt
        }
        return Next(step, LeverRetry.delayFor(step), t)
    }

    /**
     * Nothing was left undelivered on this tick. If a persister of [gen] was running, its role ends
     * and the whole seconds since its chain started are returned for the I line; otherwise null.
     */
    fun onDelivered(gen: Int, now: Long): Long? {
        if (persistToken == 0L || persistGen != gen) return null
        persistToken = 0L
        return LeverRetry.deliveredAfterS(persistSince, now)
    }
}
