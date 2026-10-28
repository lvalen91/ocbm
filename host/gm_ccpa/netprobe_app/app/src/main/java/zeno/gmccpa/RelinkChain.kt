package zeno.gmccpa

import java.util.concurrent.Executor
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.atomic.AtomicReference

/**
 * The box relink's state machine (2026-10-06, 52B; bounded retry + backoff v5.6 H1-4), with no
 * Android types so it runs under plain-JVM unit tests. [MainActivity]'s `BoxRelink` binds it to the
 * real probe, executor and activity; read that object's KDoc for what a relink is and why it exists.
 *
 * Single-flight by an OWNER TOKEN, not a boolean: Stop nulls [owner] while the old task may still be
 * unwinding (up to 250 ms), and a later owner's flag must never be cleared by that task's finally.
 * The token guards (i) the NEXT transport dying during the relink's own bring-up and (ii)
 * concurrent arms. A trigger that loses the CAS sets [again] FIRST, then retries the CAS once
 * (R-LW: otherwise a finally that released between the two would never see the flag, and the new
 * death would be dropped — P3 again). The finally releases its own token, then EITHER re-arms OR
 * reports, never both.
 *
 * ## Dead runs, the streak and the cap (v5.6 H1-4, review B1, A3)
 *
 * A DEAD run is one whose own transport died while it ran ([again], gated on the link being gone),
 * OR one that claimed the adapter but got no CT_HELLO_ACK while the adapter is still enumerated (a
 * box that is on the bus and not answering — before v5.6 that ended the chain with "press Start").
 * Both at once count ONCE (A3). A dead run re-arms with [Cause.DEAD_RUN].
 *
 * A heartbeat-declared death less than [QUICK_DEATH_WINDOW_MS] after the previous relink's
 * CT_HELLO_ACK also counts ([Cause.HEARTBEAT_DIED]): a stall that recurs every ~30 s would otherwise
 * relink forever — and with no session each relink is one CT_SUBSCRIBE presence edge, which is what
 * the box's flap detector punishes. Such a run's successful finish does NOT zero the streak (B1:
 * before v5.6 every run without a death during it zeroed it, so a counted arm could never reach
 * the cap).
 *
 * Counting and the cap happen when the relink is ARMED, never in the finish of a successful run:
 * a finish-time cap would put "press Start" over a link that had just come up. The [MAX_DEAD_RELINKS]th
 * counted arm is refused and reported through [Env.capped]. Counted arms wait [BACKOFF_MS] first
 * (5, 10, 20, 30 s), in [BACKOFF_SLICE_MS] slices that end at once when Stop/Restart drops the token.
 * Worst case from the first dead run to the cap: 5 x (<= 4 s quick stop + 20 s HELLO) + 65 s ≈ 185 s.
 *
 * Any other arm starts a new episode and zeroes the streak: a transport that died on its own
 * (ENODEV: the adapter left the bus — today's behaviour), a heartbeat death >= 60 s after the last
 * relink's HELLO (the link held), and the live-session claim-wait expiry. A run that ends the chain
 * without a link (claim failure, unplugged adapter, exception) zeroes it too. A run whose [Env.runAll]
 * THREW is reported through [Env.failed] (v5.8 V7 / Q018): until then it ended in silence.
 */
internal class RelinkChain<P : Any, R : Any>(
    private val env: Env<P, R>,
    private val exec: Executor,
    private val log: Log,
    private val nowMs: () -> Long = { System.nanoTime() / 1_000_000L },
    private val sleepMs: (Long) -> Unit = { Thread.sleep(it) },
) {
    interface Log { fun i(msg: String); fun w(msg: String); fun e(msg: String) }

    /** What a run achieved, as far as the chain cares. [helloAtMs] is on the [nowMs] clock. */
    data class Outcome(val claimed: Boolean, val helloOk: Boolean, val helloAtMs: Long)

    /** The world, injected. P is the probe (identity matters: see [again]); R the run's result. */
    interface Env<P : Any, R : Any> {
        fun probe(): P?
        fun stopped(p: P): Boolean
        fun hasLink(p: P): Boolean
        fun waitExpired(p: P): Boolean
        fun adapterEnumerated(p: P): Boolean
        fun stopRequested(): Boolean
        fun sessionLive(): Boolean
        /** The claim + HELLO (+ MFi, subscribe) run. May throw; RejectedExecutionException = probe stopped. */
        fun runAll(p: P, cancelled: () -> Boolean): R
        fun outcome(r: R): Outcome
        fun report(r: R, expired: Boolean)
        fun capped(detail: String)
        /**
         * v5.8 V7 / Q018: [runAll] threw (anything but a RejectedExecutionException). Build the
         * result the chain reports in its place: a FAILED run (not claimed, no HELLO) that carries
         * [detail] (`ExceptionName: message`), so the status line says what happened instead of the
         * relink ending in silence with the UI still saying it is re-claiming.
         */
        fun failed(detail: String): R
    }

    enum class Cause {
        /** The read loop ended on its own (ENODEV, read errors) — not counted. */
        TRANSPORT_DIED,
        /** The heartbeat declared the link dead — counted when < [QUICK_DEATH_WINDOW_MS] after the last relink HELLO. */
        HEARTBEAT_DIED,
        /** Re-arm from [finish] after a dead run — always counted. */
        DEAD_RUN,
        /** Re-arm from [finish] after a claim-wait expiry with CarPlay live — not counted. */
        EXPIRED_LIVE,
    }

    companion object {
        /** After this many counted arms in a row the chain stops instead of re-claiming forever. */
        const val MAX_DEAD_RELINKS = 5
        const val QUICK_DEATH_WINDOW_MS = 60_000L
        /** Backoff before the Nth counted run (N = 1..4); the 5th counted arm is the cap. */
        val BACKOFF_MS = longArrayOf(5_000L, 10_000L, 20_000L, 30_000L)
        const val BACKOFF_SLICE_MS = 250L
    }

    /** Non-null while a relink owns the claim; the value is that run's token. */
    val owner = AtomicReference<Any?>(null)
    /**
     * A transport died while a relink owned the claim: re-check on that run's exit. The value is the
     * probe that was current at the death, so a run whose own probe was stopped drops only its own
     * flag and still relinks a NEWER probe's death (`ocbm_stop` + Start while it held the token).
     */
    val again = AtomicReference<P?>(null)

    private val lock = Any()
    /** Counted arms in the current episode. Guarded by [lock]. */
    private var deadStreak = 0
    /** [nowMs] at the last relink's CT_HELLO_ACK; 0 = none since the last [cancel]. Guarded by [lock]. */
    private var lastHelloAtMs = 0L

    private class Grant(val counted: Boolean, val delayMs: Long, val streak: Int)

    /** Test/diagnostic view of the streak. */
    fun streak(): Int = synchronized(lock) { deadStreak }

    fun arm(why: String, cause: Cause = Cause.TRANSPORT_DIED) {
        val p = env.probe()
        if (p == null || env.stopped(p) || env.stopRequested()) {
            log.i("relink not armed ($why) — no OCBM probe, or Stop was pressed")
            return
        }
        val tok = Any()
        if (!owner.compareAndSet(null, tok)) {
            again.set(p)
            // R-LW: the owner may have released between the CAS above and the set — retry once.
            if (!owner.compareAndSet(null, tok)) {
                // Not counted here: the owning run's finish sees [again] and counts it once (A3).
                log.i("relink wait already armed — will re-check on exit ($why)")
                return
            }
            again.set(null)
            if (env.stopped(p) || env.stopRequested()) { owner.compareAndSet(tok, null); return }
        }
        // This arm owns the claim: count it, and apply the cap, HERE (B1).
        val g = grant(cause)
        if (g == null) {
            owner.compareAndSet(tok, null)
            again.set(null)
            // The chain is not re-armed. A replug fires the attach broadcast, whose auto-claim
            // (plan v5.9 D6) claims again unless an operator Stop or its own flap guard (the same
            // MAX_DEAD_RELINKS streak of failed HELLOs) holds it back; Start always claims.
            val detail = "the adapter's link failed on each of the last $MAX_DEAD_RELINKS relinks (died during the relink, " +
                "no CT_HELLO_ACK with the adapter on the bus, or died again within ${QUICK_DEATH_WINDOW_MS / 1000} s of its HELLO) " +
                "— replug the adapter if needed, then press Start (or open gm_ccpa)"
            log.w("relink NOT re-armed ($why): $detail")
            // ALWAYS, HELLO or not: the IDLE detail still says the adapter is being re-claimed.
            env.capped(detail)
            return
        }
        try {
            exec.execute { run(tok, g) }
            log.i("relink wait armed ($why)" +
                if (g.counted) " — dead run ${g.streak} of $MAX_DEAD_RELINKS, retrying in ${g.delayMs / 1000} s" else "")
        } catch (e: RejectedExecutionException) {
            owner.compareAndSet(tok, null)
            log.w("relink not armed ($why): ${e.message}")
        }
    }

    /** null = capped. */
    private fun grant(cause: Cause): Grant? = synchronized(lock) {
        val counted = when (cause) {
            Cause.DEAD_RUN -> true
            Cause.HEARTBEAT_DIED -> lastHelloAtMs != 0L && nowMs() - lastHelloAtMs < QUICK_DEATH_WINDOW_MS
            Cause.TRANSPORT_DIED, Cause.EXPIRED_LIVE -> false
        }
        if (!counted) {
            deadStreak = 0
            return@synchronized Grant(false, 0L, 0)
        }
        val n = ++deadStreak
        if (n >= MAX_DEAD_RELINKS) {
            deadStreak = 0
            return@synchronized null
        }
        Grant(true, BACKOFF_MS[minOf(n, BACKOFF_MS.size) - 1], n)
    }

    /** Stop / Restart: drop the token so the unwinding task's finally fails its CAS and does nothing. */
    fun cancel(why: String) {
        if (owner.getAndSet(null) != null) log.i("relink cancelled — $why")
        again.set(null)
        synchronized(lock) { deadStreak = 0; lastHelloAtMs = 0L }
    }

    /**
     * Sleep [delayMs] in [BACKOFF_SLICE_MS] slices. False as soon as [abandoned] is true (checked
     * before every slice) or the thread is interrupted; true when the whole delay elapsed.
     */
    fun awaitBackoff(delayMs: Long, abandoned: () -> Boolean): Boolean {
        var left = delayMs
        while (left > 0) {
            if (abandoned()) return false
            val step = minOf(BACKOFF_SLICE_MS, left)
            try { sleepMs(step) } catch (_: InterruptedException) { Thread.currentThread().interrupt(); return false }
            left -= step
        }
        return !abandoned()
    }

    private fun run(tok: Any, g: Grant) {
        // NOT MainActivity.ocbm(): that BUILDS a probe when there is none, and Stop nulls it, so a
        // relink starting after Stop would re-claim the box.
        val p = env.probe()
        var r: R? = null
        // Q018: set when `r` is the synthetic result of a throw, so finish() reports it and never
        // re-arms on it (a runAll that throws every time would otherwise spin with no backoff).
        var threw = false
        try {
            if (p == null || env.stopped(p) || env.stopRequested()) return
            if (g.delayMs > 0 && !awaitBackoff(g.delayMs) { owner.get() !== tok || env.stopped(p) || env.stopRequested() }) {
                log.i("relink abandoned during its ${g.delayMs / 1000} s backoff — Stop/Restart, or a newer owner")
                return
            }
            // Stop/Restart drop the token: the claim wait ends on it even when Stop's own abort
            // flag was cleared at the wait's entry and its sticky stop is still queued.
            r = env.runAll(p) { owner.get() !== tok }
        } catch (e: RejectedExecutionException) {
            log.i("relink skipped — the OCBM probe was stopped")
        } catch (t: Throwable) {
            val detail = "${t.javaClass.simpleName}: ${t.message}"
            log.e("relink failed: $detail")
            // Q018: until v5.8 this left r == null and finish() ended the chain silently — the
            // IDLE detail kept saying the adapter was being re-claimed. Report a failed result instead.
            r = env.failed(detail)
            threw = true
        } finally {
            finish(tok, p, r, g.counted, threw)
        }
    }

    /**
     * Release the token, then EITHER re-arm OR report — never both. A run that no longer owns the
     * claim (Stop/Restart dropped it) does neither: the supervisor already says "stopped". A run
     * that re-arms does not report: its result is about a link that already died again (or a wait
     * that is starting over), and reporting it would put "box linked" or "press Start" over a wait
     * that is still running.
     */
    private fun finish(tok: Any, p: P?, r: R?, counted: Boolean, threw: Boolean = false) {
        // Stop (or a newer owner) holds it: do nothing, and leave `again` to its owner.
        if (!owner.compareAndSet(tok, null)) return
        if (p == null || env.stopped(p) || env.stopRequested()) {
            // Drop only this run's own flag. One set for a newer probe is a death nobody else will
            // relink: arm() re-checks Stop and the current probe; a link already back is left alone.
            val flagged = again.getAndSet(null)
            if (flagged != null && flagged !== p && !env.stopped(flagged) && !env.hasLink(flagged))
                arm("a transport died on the new OCBM probe during the old probe's relink")
            return
        }
        val o = r?.let { env.outcome(it) }
        // Gated on the link: a real death of this run's transport has already cleared HELLO
        // (markLinkDead runs before onLinkDead), while a flag left by the arm/finish race on a
        // healthy link would re-claim it — CT_STOP, log pull, re-SUBSCRIBE: the presence flap.
        val newDeath = again.getAndSet(null) != null && !env.hasLink(p)
        // v5.6 H1-4: claimed, no HELLO, adapter still on the bus = a box not answering. An adapter
        // that left the bus keeps today's "press Start" (and its ENODEV usually set `again` anyway).
        val helloFailed = !newDeath && o != null && o.claimed && !o.helloOk && env.adapterEnumerated(p)
        // A3: an unplug (`again`) and a failed HELLO in the same run are ONE dead run.
        val dead = newDeath || helloFailed
        synchronized(lock) {
            if (o != null && o.helloOk && o.helloAtMs != 0L) lastHelloAtMs = o.helloAtMs
            // B1: a counted run that brought the link up keeps the streak — the cap must stay
            // reachable for a stall that recurs shortly after every successful relink.
            if (!dead && !(counted && o?.helloOk == true)) deadStreak = 0
        }
        val expiredLive = o != null && !o.claimed && env.waitExpired(p) && env.sessionLive()
        when {
            dead -> arm(if (newDeath) "a transport died during the relink"
                        else "the relink claimed the adapter but got no CT_HELLO_ACK (adapter still on the bus)", Cause.DEAD_RUN)
            // Q018: a run that threw is reported, never re-armed from here (a dead run above still
            // wins: the death edge re-arms with its backoff and the throw is logged already).
            // expired = false ALWAYS (fix1): waitExpired is a probe flag cleared only when the next
            // claim wait starts, so a throw before that point would read a stale expiry and the
            // report's expiry wording would replace the exception text.
            threw -> env.report(r!!, false)
            expiredLive -> arm("claim wait expired with CarPlay live — waiting again", Cause.EXPIRED_LIVE)
            // A link that died between runAll's return and here: its death edge already went out
            // and re-armed (or will, via `again`) — a success report now would be false.
            r == null || (o!!.helloOk && !env.hasLink(p)) -> Unit
            else -> env.report(r, env.waitExpired(p))
        }
    }
}
