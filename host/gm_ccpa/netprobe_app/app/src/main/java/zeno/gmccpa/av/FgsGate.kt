package zeno.gmccpa.av

/**
 * Decides when [CarPlaySessionService] may be stopped. Pure Kotlin, no Android types, so it is
 * unit-tested on the host JVM (FgsGateTest). The service calls themselves are passed in as lambdas
 * and are made UNDER [lock], so no start or stop can be interleaved between a decision and its call.
 *
 * # The crash this prevents (audit12 F094, F095; plan v5.7.1 B1-1)
 *
 * `startForegroundService` obliges the service to reach `startForeground` (in `onStartCommand`, on
 * main). If `stopService` lands on the record before that, AMS posts SERVICE_FOREGROUND_CRASH_MSG
 * (android-32 ActiveServices.java:5279-5286) and kills the whole process: OCBM link, receiver and
 * session. A session end reaches [CarPlayActivity] on the receiver pump or `netprobe-cmd` thread, so it
 * can easily land while main has not yet run the `onStartCommand` for the start it is undoing.
 *
 * # The rule
 *
 * `stopService` is never called while any start has not reached its `startForeground`.
 *  - [holders]: the session tokens that want the service up (one per CarPlayActivity instance). Keying
 *    by token means an old instance's late stop cannot take down a replacement's service ([C1]).
 *  - [pendingStarts]: starts issued but not yet through `startForeground`. Incremented only after
 *    `startForegroundService` returned; decremented by [onStarted] once `startForeground` has run.
 *  - A stop that finds no holders but a pending start is DEFERRED: the `onStartCommand` of that start
 *    sees the empty holder set and stops the service itself, after its own `startForeground`.
 *
 * A counter, not a "foreground" flag (v5.7.1): a flag cleared in `onDestroy` reads stale when a new
 * start/stop pair runs on a background thread before main has run the old instance's `onDestroy`, and
 * the stop then hits the new record before its `startForeground`. The counter needs no AMS internals.
 * Nothing here is touched by `onDestroy`.
 *
 * Lock order: CarPlayActivity.sessionLock -> [lock]. `onStartCommand` takes only [lock]. The service
 * calls made under it are non-blocking binder calls that never wait on main, so holding it across
 * them cannot deadlock; main may wait a few ms in `onStartCommand`.
 */
class FgsGate {

    enum class Stop {
        /** Holders empty and no start pending: the stop call was made. */
        STOPPED,
        /** Holders empty but [pendingStarts] > 0: no call; the pending `onStartCommand` stops it. */
        DEFERRED,
        /** Another token still holds the service: no call. */
        HELD,
    }

    enum class Started {
        /** A holder remains (or a start is still pending): the service stays. */
        STAYS,
        /** No holder and nothing pending: stopForeground + stopSelf were called. */
        STOPPED,
        /** An `onStartCommand` this gate never issued a start for; clamped at 0. */
        UNREQUESTED_STAYS,
        /** Unrequested, and nothing holds the service: stopped. */
        UNREQUESTED_STOPPED,
    }

    private val lock = Any()
    private val holders = HashSet<Long>()
    private var pendingStarts = 0
    /**
     * plan v5.9 D6 rev c C3: the service's ongoing notification is up and owned. SET in [onStarted]'s
     * staying branches (after `startForeground`), CLEARED under the same lock immediately before
     * `stopService` ([stop]'s STOPPED branch) and before `stopForeground` ([onStarted]'s stop). Every
     * later notify checks it under the lock ([whenPosted]), so a late update cannot resurrect an
     * ongoing notification after the last holder released.
     */
    private var posted = false

    /**
     * Register [token] and issue the start. [startFgs] runs under the lock; [pendingStarts] counts it
     * only once it returned. If it throws, [token] is removed again and the throwable propagates.
     */
    fun start(token: Long, startFgs: () -> Unit) {
        synchronized(lock) {
            holders.add(token)
            try {
                startFgs()
            } catch (t: Throwable) {
                holders.remove(token)
                throw t
            }
            pendingStarts++
        }
    }

    /** Drop [token]. Calls [stopSvc] (under the lock) only when nothing holds the service and no start is pending. */
    fun stop(token: Long, stopSvc: () -> Unit): Stop = synchronized(lock) {
        holders.remove(token)
        when {
            holders.isNotEmpty() -> Stop.HELD
            pendingStarts > 0 -> Stop.DEFERRED
            else -> { posted = false; stopSvc(); Stop.STOPPED }
        }
    }

    /**
     * Call from `onStartCommand` AFTER `startForeground` returned. Consumes one pending start and, if
     * nothing holds the service any more, calls [stopSelf] (stopForeground + stopSelf(startId)) under the lock.
     */
    fun onStarted(onStays: () -> Unit = {}, stopSelf: () -> Unit): Started = synchronized(lock) {
        val requested = pendingStarts > 0
        if (requested) pendingStarts--
        val stop = holders.isEmpty() && pendingStarts == 0
        if (stop) { posted = false; stopSelf() } else { posted = true; onStays() }
        when {
            requested && stop -> Started.STOPPED
            requested -> Started.STAYS
            stop -> Started.UNREQUESTED_STOPPED
            else -> Started.UNREQUESTED_STAYS
        }
    }

    /** Starts issued but not yet through `startForeground` (log text and tests). */
    fun pending(): Int = synchronized(lock) { pendingStarts }

    /** Tokens currently holding the service (log text and tests). */
    fun holderCount(): Int = synchronized(lock) { holders.size }

    /** D6: whether [token] holds the service now (read under the lock). */
    fun holds(token: Long): Boolean = synchronized(lock) { token in holders }

    /** D6 rev c C3: run [block] under the lock only while the notification is posted; false if it is not. */
    fun whenPosted(block: () -> Unit): Boolean = synchronized(lock) { if (posted) { block(); true } else false }

    /** D6 rev c C3: run [block] under the lock with the current posted flag. */
    fun <T> locked(block: (posted: Boolean) -> T): T = synchronized(lock) { block(posted) }
}

/**
 * The session-holder tokens (pure, so FgsGateTest can pin them). [newToken] hands each CarPlayActivity
 * instance a token starting at 1; [LINK_TOKEN] (plan v5.9 D6 rev b R5) is the adapter-link hold — a
 * negative constant, so it can never collide with a session's token.
 */
object FgsTokens {
    const val LINK_TOKEN = -1L
    private val tokens = java.util.concurrent.atomic.AtomicLong(0)
    fun newToken(): Long = tokens.incrementAndGet()
}
