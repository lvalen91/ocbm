package zeno.gmccpa.logging

import android.os.SystemClock
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit
import zeno.gmccpa.ProbeLog

/**
 * The two things a capture of this app could not previously answer: **what is supposed to happen
 * next**, and **what is running right now**.
 *
 * ## Why this exists
 *
 * `05_SESSION_FLOW.md` §8 lists nine ordering rules that kill a session, and the property they
 * share is that **every one of them fails silently**. A stream type answered with silence, a
 * `streamID` of zero, an RCS frame with the wrong 4CC, a feature negotiated but not backed — none
 * of these produce an error on either side. The 2026-09-09 capture makes the same point from the
 * other direction: 641 app lines, of which 7 were `W` and 0 were `E`, in a session that contained a
 * 15 s MFi timeout, 42 dropped Siri access units and 6 media underruns. Everything that went wrong
 * was *printed*, and nothing was *flagged*.
 *
 * A log that narrates success is not a diagnostic. What an operator needs from
 * `adb logcat > logcat.log` on a truck with no laptop attached is:
 *
 *  - **the negative space** — "RECORD completed, so a screen stream was expected within 10 s, and
 *    it never arrived", not merely the absence of a `SETUP` line among six hundred others;
 *  - **the standing state** — which sinks, seams, adverts and listeners are up at this instant,
 *    without reconstructing it from forty lines of history.
 *
 * [expect] provides the first. [Board] provides the second.
 *
 * ## Why one shared primitive rather than a timer per subsystem
 *
 * There are already nineteen `*_MS` constants across `av/`, `ocbm/` and `CarPlayRx`, and almost all
 * of them are retry or backoff intervals — the app is full of code that waits, and nearly empty of
 * code that reports having waited in vain. Left to grow organically each subsystem would spell its
 * own absence differently and none would be greppable as a class. One vocabulary, one severity
 * rule, one string prefix.
 *
 * ## Cost, and why it is safe on the A/V path
 *
 * One shared daemon scheduler, no thread per expectation. [expect] and [met] are a
 * `ConcurrentHashMap` put/remove plus a `schedule`, so they are safe to call from the OCBM reader
 * thread, the `cp-*` seam threads and the main looper alike. Nothing here blocks, and nothing here
 * is on a per-frame path — expectations are per phase, per stream, per session. A missed
 * expectation costs one log line, and a met one costs none unless it was late.
 */
object SessionTrace {

    /** Everything this object emits carries one of these, so a capture can be sliced by class. */
    private const val TAG_MISSING = "!! EXPECTED-MISSING"
    private const val TAG_LATE = "~~ EXPECTED-LATE"
    private const val TAG_MET = "== EXPECTED-MET"
    private const val TAG_STATUS = "## STATUS"

    private val log = ProbeLog.sub("trace")

    private val sched = Executors.newSingleThreadScheduledExecutor { r ->
        Thread(r, "trace-watchdog").apply { isDaemon = true }
    }

    /**
     * An armed expectation. Held only so [met] and [cancel] can find it by name; the scheduler owns
     * the firing.
     */
    private class Pending(
        val what: String,
        val because: String,
        val armedAt: Long,
        val withinMs: Long,
        val future: ScheduledFuture<*>,
        val onMiss: (() -> Unit)? = null,
    )

    private val pending = ConcurrentHashMap<String, Pending>()

    /**
     * Declare that [what] is expected within [withinMs], **because** [because] already happened.
     *
     * The `because` is not decoration and must not be dropped: an operator reading
     * `EXPECTED-MISSING screen-stream` learns nothing they could act on, whereas
     * `EXPECTED-MISSING screen-stream after 10000ms — RECORD completed at 22:55:22.072, so iOS
     * should have SETUP a type-110 stream` names the step that did happen, which is where the fault
     * actually is. Write it as the *precondition observed*, not as a restatement of the name.
     *
     * Re-arming the same [what] replaces the previous expectation rather than stacking a second —
     * iOS legitimately re-SETUPs streams throughout a session, and two live timers for one name
     * would fire a spurious miss for the older of two satisfied expectations.
     *
     * Idempotent to call from any thread. Never blocks.
     */
    fun expect(what: String, withinMs: Long, because: String, onMiss: (() -> Unit)? = null) {
        val armedAt = SystemClock.elapsedRealtime()
        val future = try {
            sched.schedule({
                // Removed by the timer itself, not by met(): if this fires, the expectation is
                // resolved (as a miss) and must not also be reported met by a very late arrival.
                val p = pending.remove(what) ?: return@schedule
                log.e("$TAG_MISSING $what after ${p.withinMs}ms — ${p.because}")
                // Let the owner reflect the miss in its own state — typically a Board entry, so a
                // later `## STATUS` dump agrees with the EXPECTED-MISSING line instead of still
                // showing the component as merely waiting. Runs on the watchdog thread: it must not
                // block, and it must not throw (a throw here would kill the shared scheduler for
                // every other expectation in the process, which is why it is contained).
                p.onMiss?.let { cb ->
                    runCatching(cb).onFailure {
                        log.w("onMiss for '$what' threw ${it.javaClass.simpleName}: ${it.message}")
                    }
                }
            }, withinMs, TimeUnit.MILLISECONDS)
        } catch (_: Throwable) {
            // Scheduler shut down during teardown. An expectation nobody can fire is not worth a
            // line, but silently dropping one IS the failure mode this class exists to end.
            log.w("could not arm expectation '$what' — trace scheduler is shut down")
            return
        }
        pending.put(what, Pending(what, because, armedAt, withinMs, future, onMiss))?.future?.cancel(false)
    }

    /**
     * [what] arrived. Cancels its watchdog and, if it was slower than [lateAboveMs] of its budget,
     * says so — a step that lands at 95 % of its deadline is the one that will miss it on the next
     * drive, and that is invisible if only misses are logged.
     *
     * Unknown names are silently ignored: a subsystem may report a step that was never armed
     * (a stream the phone SETUPs unprompted), and that is not an error.
     */
    fun met(what: String, lateAboveMs: Float = 0.5f) {
        val p = pending.remove(what) ?: return
        p.future.cancel(false)
        val took = SystemClock.elapsedRealtime() - p.armedAt
        if (took > p.withinMs * lateAboveMs) {
            log.w("$TAG_LATE $what arrived after ${took}ms of a ${p.withinMs}ms budget — ${p.because}")
        } else {
            log.i("$TAG_MET $what in ${took}ms")
        }
    }

    /**
     * [what] is no longer expected, and its absence is NOT a fault — the session ended, the phone
     * left, the operator stopped it. Distinct from [met] so a capture never shows a step as
     * satisfied when it was actually abandoned.
     */
    fun cancel(what: String, why: String) {
        val p = pending.remove(what) ?: return
        p.future.cancel(false)
        log.i("expectation '$what' dropped — $why")
    }

    /**
     * Drop every armed expectation whose name starts with [prefix].
     *
     * Exists because "the session ended" and "everything ended" are different events and a blanket
     * [cancelAll] conflates them. The phone leaving ends the AirPlay session; it does NOT end the
     * OCBM link, so a box-side expectation in flight at that moment is still meaningful and must
     * survive. Name expectations `scope/what` — `av/first-frame`, `ocbm/box-health`,
     * `rx/pair-verify gen=3` — and each owner cancels only its own scope.
     *
     * Prefix rather than a formal scope type on purpose: expectations are already named per
     * generation and per stream, the set is small, and a string prefix keeps the call sites
     * readable without a registry to keep in step.
     */
    fun cancelScope(prefix: String, why: String) {
        val hit = pending.keys.filter { it.startsWith(prefix) }
        if (hit.isEmpty()) return
        hit.forEach { n -> pending.remove(n)?.future?.cancel(false) }
        log.i("dropped ${hit.size} '$prefix' expectation(s) — $why: ${hit.joinToString(", ")}")
    }

    /** Drop every armed expectation. For a deliberate teardown, where all of them are moot. */
    fun cancelAll(why: String) {
        val names = pending.keys.toList()
        if (names.isEmpty()) return
        names.forEach { n -> pending.remove(n)?.future?.cancel(false) }
        log.i("dropped ${names.size} pending expectation(s) — $why: ${names.joinToString(", ")}")
    }

    /** Names currently armed, for the status block and for tests. */
    fun outstanding(): List<String> = pending.keys.sorted()

    // ---------------------------------------------------------------------------------------

    /**
     * The standing state of every long-lived thing the app runs, so "what is actually up right now"
     * is one grep instead of a reconstruction.
     *
     * A component reports itself [up] or [down] as it starts and stops. Transitions log; the
     * periodic [dump] prints the whole board. Both go through [ProbeLog] like everything else, so
     * they land in the same ordered stream as the box's `[box:*]` lines and the Rust core's
     * `[rust ]` lines.
     */
    object Board {
        private val state = ConcurrentHashMap<String, String>()
        private val order = java.util.Collections.synchronizedList(ArrayList<String>())
        @Volatile private var ticker: ScheduledFuture<*>? = null

        /** Register a component as running. [detail] is the useful specifics — a port, a format. */
        fun up(name: String, detail: String = "") = set(name, if (detail.isEmpty()) "UP" else "UP $detail")

        /** Register a component as stopped. [why] must say whether that is expected. */
        fun down(name: String, why: String) = set(name, "DOWN ($why)")

        /**
         * Register a component as absent when it was supposed to be present. Separate from [down]
         * because "the mic uplink is not running because no session is up" and "the mic uplink is
         * not running and one should be" are the same string to a reader otherwise.
         *
         * Emits its own ERROR line, so a caller that ALREADY logged the fault should use [note]
         * instead and let its own line carry the severity — otherwise one failure produces two `E`
         * lines and the count stops meaning anything.
         */
        fun failed(name: String, why: String) {
            set(name, "FAILED ($why)")
            log.e("$TAG_STATUS $name FAILED — $why")
        }

        /**
         * Record arbitrary standing state that is neither up nor down, without a severity of its
         * own.
         *
         * Added 2026-09-10 for the session phase. `RxPhase` has fourteen values and only three are
         * failures, so forcing it through [up]/[down] renders `UP stalled` — actively misleading in
         * a dump — while forcing it through [failed] emits a second `E` for a transition the
         * supervisor has already logged at `E` itself. This is the third case: the value IS the
         * state, the owner owns the severity.
         */
        fun note(name: String, value: String) = set(name, value)

        private fun set(name: String, value: String) {
            val prev = state.put(name, value)
            if (prev == null) synchronized(order) { if (!order.contains(name)) order.add(name) }
            if (prev != value) log.i("$TAG_STATUS $name: ${prev ?: "(new)"} -> $value")
        }

        /**
         * Print the whole board plus any outstanding expectations.
         *
         * Emitted on a slow tick during a session and at every phase change worth anchoring. The
         * point is that a capture opened at a random offset can answer "what was up at this moment"
         * from the nearest block, rather than only from the beginning of the file.
         */
        fun dump(reason: String) {
            val names = synchronized(order) { order.toList() }
            log.i("$TAG_STATUS ---- $reason ----")
            if (names.isEmpty()) log.i("$TAG_STATUS (nothing registered)")
            names.forEach { n -> log.i("$TAG_STATUS   ${n.padEnd(22)} ${state[n]}") }
            val waiting = outstanding()
            if (waiting.isNotEmpty()) log.i("$TAG_STATUS   awaiting: ${waiting.joinToString(", ")}")
            log.i("$TAG_STATUS ---- end ----")
        }

        /** Start the periodic dump. Idempotent; [every] is deliberately slow — this is an anchor, not a trace. */
        fun startTicker(every: Long = 30_000L) {
            if (ticker != null) return
            ticker = try {
                sched.scheduleWithFixedDelay({
                    runCatching { dump("periodic") }
                        .onFailure { log.w("status dump threw: ${it.javaClass.simpleName}") }
                }, every, every, TimeUnit.MILLISECONDS)
            } catch (_: Throwable) { null }
        }

        fun stopTicker() {
            ticker?.cancel(false)
            ticker = null
        }

        /** Forget everything. Called on a deliberate full teardown so the next session starts clean. */
        fun reset(why: String) {
            stopTicker()
            state.clear()
            synchronized(order) { order.clear() }
            log.i("$TAG_STATUS board reset — $why")
        }
    }
}
