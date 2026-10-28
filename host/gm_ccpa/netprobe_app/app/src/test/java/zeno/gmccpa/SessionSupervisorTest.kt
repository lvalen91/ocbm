package zeno.gmccpa

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import zeno.gmccpa.ocbm.Ocbm
import java.util.concurrent.AbstractExecutorService
import java.util.concurrent.Callable
import java.util.concurrent.Delayed
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit

/**
 * v5.7 U3 (SessionSupervisor) host-JVM tests. Run by /tmp/ccpa_sess/unit_test_v57_U3.sh, which puts
 * stub `android.util.Log` / `android.os.SystemClock` classes on the RUNTIME classpath (android.jar is
 * compile-only) — the stub Log records every line into a static `LINES` list, read here by reflection
 * so this file compiles against the real android.jar.
 *
 * Time is virtual: the supervisor is built through its internal constructor with [VirtualScheduler],
 * whose `now` is also the supervisor's clock, so a 45 s watchdog costs no wall time and timer order is
 * deterministic. The scheduler mirrors the two FutureTask properties the code relies on: a future is
 * not `isDone` while its own body runs, and `cancel(false)` on a running future returns true and
 * makes it done.
 *
 * The phase is read from the last `Actions.report` (every `to()` reports, and every in-place report
 * carries the current phase), not from `SessionSupervisor.current`, which Batch 3 (F014) may delete.
 */
class SessionSupervisorTest {

    // ---- virtual-time scheduler ------------------------------------------------------------

    class VirtualScheduler : AbstractExecutorService(), ScheduledExecutorService {
        var now = 1_000_000L      // far from 0 (the "never ran" stamp) and from the wall clock
        private var seq = 0L
        private var shut = false
        private val queue = ArrayList<Task>()

        inner class Task(val due: Long, val seq: Long, private val r: Runnable) : ScheduledFuture<Any?> {
            var state = 0   // 0 new, 1 running, 2 done, 3 cancelled
            fun run() {
                if (state != 0) return
                state = 1
                try { r.run() } finally { if (state == 1) state = 2 }
            }
            override fun cancel(mayInterruptIfRunning: Boolean): Boolean =
                if (state == 0 || state == 1) { state = 3; true } else false
            override fun isCancelled(): Boolean = state == 3
            override fun isDone(): Boolean = state >= 2
            override fun get(): Any? = null
            override fun get(timeout: Long, unit: TimeUnit): Any? = null
            override fun getDelay(unit: TimeUnit): Long = unit.convert(due - now, TimeUnit.MILLISECONDS)
            override fun compareTo(other: Delayed): Int =
                getDelay(TimeUnit.MILLISECONDS).compareTo(other.getDelay(TimeUnit.MILLISECONDS))
        }

        private fun add(delayMs: Long, r: Runnable): Task {
            if (shut) throw RejectedExecutionException("shut down")
            return Task(now + delayMs, seq++, r).also { queue.add(it) }
        }

        /** Run every task due within [ms], in (due, submission) order, advancing `now` to each. */
        fun advance(ms: Long) {
            val target = now + ms
            while (true) {
                queue.removeAll { it.state != 0 }
                val t = queue.filter { it.due <= target }.minWithOrNull(compareBy<Task>({ it.due }, { it.seq })) ?: break
                queue.remove(t)
                if (t.due > now) now = t.due
                t.run()
            }
            now = target
        }

        override fun execute(command: Runnable) { add(0, command) }
        override fun schedule(command: Runnable, delay: Long, unit: TimeUnit): ScheduledFuture<*> =
            add(unit.toMillis(delay), command)
        override fun <V : Any?> schedule(callable: Callable<V>, delay: Long, unit: TimeUnit): ScheduledFuture<V> =
            throw UnsupportedOperationException()
        override fun scheduleAtFixedRate(command: Runnable, initialDelay: Long, period: Long, unit: TimeUnit): ScheduledFuture<*> =
            throw UnsupportedOperationException()
        override fun scheduleWithFixedDelay(command: Runnable, initialDelay: Long, delay: Long, unit: TimeUnit): ScheduledFuture<*> =
            throw UnsupportedOperationException()
        override fun shutdown() { shut = true }
        override fun shutdownNow(): MutableList<Runnable> { shut = true; return ArrayList() }
        override fun isShutdown(): Boolean = shut
        override fun isTerminated(): Boolean = shut && queue.none { it.state == 0 }
        override fun awaitTermination(timeout: Long, unit: TimeUnit): Boolean = true
    }

    // ---- fake world ------------------------------------------------------------------------

    class FakeActions : SessionSupervisor.Actions {
        var linkAlive = false
        var reannounceOk = true
        var radiosOk = true
        var restartOk = true
        var unsubscribed = false
        var subscribeOk = true
        var adapter: AdapterState = AdapterState.SUBSCRIBED
        var ssid: String? = "TestNet"
        val radios = ArrayList<Boolean>()
        var restarts = 0
        var reannounces = 0
        var subscribes = 0
        var backoff: Boolean? = null
        val reports = ArrayList<Pair<RxPhase, String>>()
        val phase: RxPhase get() = reports.lastOrNull()?.first ?: RxPhase.IDLE
        val detail: String get() = reports.lastOrNull()?.second ?: ""

        override fun boxLinkAlive() = linkAlive
        override fun reannounce(): Boolean { reannounces++; return reannounceOk }
        override fun setBoxRadios(on: Boolean): Boolean { radios.add(on); return radiosOk }
        override fun restartBoxWireless(): Boolean { restarts++; return restartOk }
        override fun pauseDiscovery() {}
        override fun resumeDiscovery() {}
        override fun setDiscoveryBackoff(enabled: Boolean) { backoff = enabled }
        override fun hotspotSsid(): String? = ssid
        override fun adapterState(): AdapterState = adapter
        override fun boxLinkedUnsubscribed(): Boolean = unsubscribed
        override fun subscribeBox(): Boolean { subscribes++; return subscribeOk }
        var lastStall: StallCause? = null
        override fun report(phase: RxPhase, detail: String, stall: StallCause?) { reports.add(phase to detail); lastStall = stall }
    }

    private lateinit var vs: VirtualScheduler
    private lateinit var a: FakeActions
    private lateinit var s: SessionSupervisor

    @Suppress("UNCHECKED_CAST")
    private val logLines: MutableList<String>
        get() = Class.forName("android.util.Log").getField("LINES").get(null) as MutableList<String>

    private fun logHas(frag: String) = synchronized(logLines) { logLines.any { it.contains(frag) } }
    private fun clearLog() = synchronized(logLines) { logLines.clear() }

    /** Let queued posts run without moving time. */
    private fun drain() = vs.advance(0)
    private fun sec(n: Double) = vs.advance((n * 1000).toLong())

    @Before fun setUp() {
        clearLog()
        vs = VirtualScheduler()
        a = FakeActions()
        s = SessionSupervisor(a, { vs.now }, vs)
    }

    /** IDLE -> ARMED via a fresh subscribe (no timers armed). */
    private fun armed() {
        s.onBoxSubscribed(); drain()
        assertEquals(RxPhase.ARMED, a.phase)
    }

    /**
     * plan v5.9 UI-1 (R1): the ladder entry that survives the Recover merge — a session that ends
     * and is not resumed. ARMED -> SESSION_UP -> GRACE, then GRACE_MS (10 s, SessionSupervisor's
     * private constant) to the expiry, whose escalate(not fast: CLOSED, not unsubscribed) runs rung 0
     * and stamps it at the expiry instant E. Every offset in the tests below is measured from E, the
     * instant the deleted userRecover() used to run rung 0 at; the rung timing after E is identical
     * (escalate from rung 0, retry LADDER_RECHECK_MS 20 s, rung 1 hold 35 s, rung 2 hold 45 s).
     */
    private fun enterLadderViaGrace() {
        s.onSessionUp(); s.onSessionDown(EndReason.CLOSED); drain()
        assertEquals(RxPhase.GRACE, a.phase)
        sec(10.0)
        assertTrue(a.reports.any { it.first == RxPhase.ARMED && it.second.startsWith("grace expired") })
    }

    // ---- F001 ------------------------------------------------------------------------------

    /** plan v5.9 UI-1 (R1): rewritten from f001_recoverFromStalledLeavesForArmed… (userRecover is gone). */
    @Test fun f001_operatorRestartFromStalledIdles_thenResubscribeArmsAndDialAcceptIsHonoured() {
        armed()
        s.onStalled(3); drain()
        assertEquals(RxPhase.STALLED, a.phase)
        assertEquals(true, a.backoff)
        s.onOperatorRestart(); drain()
        assertEquals(RxPhase.IDLE, a.phase)
        assertEquals("STALLED backoff must be dropped on leaving", false, a.backoff)
        s.onReceiverReady(true, "receiver ready to accept a session"); drain()
        s.onBoxSubscribed(); drain()
        assertEquals(RxPhase.ARMED, a.phase)
        s.onDialAccepted(); drain()
        assertEquals(RxPhase.INBOUND_EXPECTED, a.phase)
    }

    @Test fun f001_boxRelinkedFromStalledLeavesForArmed() {
        armed()
        s.onStalled(3); drain()
        a.unsubscribed = true
        s.onBoxRelinked(); drain()
        assertTrue(a.reports.any { it.first == RxPhase.ARMED && it.second == "box relinked — recovering" })
        assertEquals("rung 1 = the deferred CT_SUBSCRIBE", 1, a.subscribes)
        assertEquals(RxPhase.ARMED, a.phase)
    }

    // f001_boxUnhealthyKeepsItsFaultOnRecover: DELETED (plan v5.9 UI-1 R1, authorized there). The
    // behaviour it pinned — Recover keeping BOX_UNHEALTHY — no longer exists by design: Restart
    // goes through IDLE. operatorRestart_clearsCooldownStamps_andIdles covers the replacement.

    // ---- F002 ------------------------------------------------------------------------------

    @Test fun f002_boxLostInGraceHoldsGraceAndExpiryRunsTheLadder() {
        armed()
        s.onSessionUp(); s.onSessionDown(EndReason.CLOSED); drain()
        assertEquals(RxPhase.GRACE, a.phase)
        s.onBoxLost(relinking = false); drain()
        assertEquals(RxPhase.GRACE, a.phase)
        assertEquals("adapter link lost during the grace window — still holding for the iPhone", a.detail)
        s.onBoxSubscribed(); drain()          // heartbeat re-subscribe inside GRACE
        assertEquals(RxPhase.GRACE, a.phase)
        clearLog()
        sec(10.0)
        assertTrue(a.reports.any { it.first == RxPhase.ARMED && it.second.startsWith("grace expired") })
        assertTrue("a rung ran on expiry", logHas("recovery rung 0"))
    }

    /** v5.7w1 (R3): a transport death in GRACE keeps GRACE and its clock, and keeps the USB-prompt hint. */
    @Test fun f002_relinkingBoxLostInGraceKeepsGraceAndTheUsbPromptHint() {
        armed()
        s.onSessionUp(); s.onSessionDown(EndReason.CLOSED); drain()
        assertEquals(RxPhase.GRACE, a.phase)
        s.onBoxLost(relinking = true); drain()
        assertEquals(RxPhase.GRACE, a.phase)
        assertTrue(a.detail, a.detail.contains("accept the USB prompt if the adapter was replugged"))
        assertTrue(a.detail, a.detail.contains("still holding for the iPhone"))
        clearLog()
        sec(10.0)
        assertTrue(a.reports.any { it.first == RxPhase.ARMED && it.second.startsWith("grace expired") })
    }

    @Test fun f002_resubscribeWhileAwaitingDialBackKeepsTheDialBackClock() {
        armed()
        s.onDialAccepted(); drain()
        assertEquals(RxPhase.INBOUND_EXPECTED, a.phase)
        s.onBoxSubscribed(); drain()
        assertEquals(RxPhase.INBOUND_EXPECTED, a.phase)
        sec(15.0)
        assertTrue(a.reports.any { it.first == RxPhase.ARMED && it.second == "no dial-back — recovering" })
    }

    // ---- F004 ------------------------------------------------------------------------------

    @Test fun f004_firedRetryTimerIsNotPendingForDeferLadder() {
        armed()
        a.linkAlive = false
        enterLadderViaGrace()                  // E: rung 0 runs, retry in 20 s
        sec(20.0)                              // E+20: rung 1 + 2 skip (no link) -> exhausted
        assertTrue(logHas("recovery ladder exhausted"))
        assertEquals(RxPhase.STALLED, a.phase)
        clearLog()
        s.onBtPhase(Ocbm.BTP_LINK_UP); drain()
        assertFalse("nothing is armed, so nothing stands down", logHas("standing the recovery ladder down"))
    }

    @Test fun f004_firedHandoffTimerDoesNotBlockTheRestartDeadline() {
        armed()
        s.onBtPhase(Ocbm.BTP_LINK_UP); drain()  // arms the 45 s watchdog
        sec(45.0)                               // fires -> ARMED + ladder
        s.onBtPhase(Ocbm.BTP_LINK_UP, replay = true); drain()   // label only, arms nothing
        assertEquals(RxPhase.BT_PAIRING, a.phase)
        clearLog()
        s.onRestartSubscribed(); drain()
        assertFalse(logHas("restart: Bluetooth already progressed"))
    }

    // ---- F005 ------------------------------------------------------------------------------

    @Test fun f005_identifiedArmsADeadline() {
        armed()
        s.onBtPhase(Ocbm.BTP_IDENTIFIED); drain()
        assertEquals(RxPhase.BT_PAIRED, a.phase)
        sec(44.9)
        assertEquals(RxPhase.BT_PAIRED, a.phase)
        sec(0.1)
        assertTrue(a.reports.any { it.first == RxPhase.ARMED && it.second == "no CarPlay connect — recovering" })
    }

    @Test fun f005_replayedIdentifiedArmsNothing() {
        armed()
        s.onBtPhase(Ocbm.BTP_IDENTIFIED, replay = true); drain()
        sec(60.0)
        assertEquals(RxPhase.BT_PAIRED, a.phase)
    }

    // ---- F006 ------------------------------------------------------------------------------

    @Test fun f006_noRung2InsideRung1WindowAfterAPreSessionClose() {
        armed()
        a.linkAlive = true
        val t0 = vs.now
        fun at(t: Double) = vs.advance(t0 + (t * 1000).toLong() - vs.now)
        s.onBtPhase(Ocbm.BTP_WIFI_HANDOFF); drain()           // t=0: watchdog due at t=45
        at(1.0)
        s.onSessionDown(EndReason.CLOSED); drain()             // closed before a session formed
        assertEquals(RxPhase.GRACE, a.phase)
        at(11.0)                                               // grace expiry -> rung 0
        assertTrue(logHas("recovery rung 0"))
        at(31.0)                                               // rung 1 (CT_RADIO off)
        assertEquals(listOf(false), a.radios)
        // Up to just before rung 1's 35 s hold ends: no rung 2 (the old watchdog fired it at t=45).
        at(65.9)
        assertEquals(0, a.restarts)
        at(66.1)
        assertEquals(1, a.restarts)
    }

    // ---- F008 ------------------------------------------------------------------------------

    @Test fun f008_cooldownsRunOnTheInjectedClock() {
        armed()
        enterLadderViaGrace()                  // rung 0 stamped at E (the first grace expiry)
        sec(1.0)
        s.onSessionUp(); s.onSessionDown(EndReason.CLOSED); drain()
        clearLog()
        // Second grace expiry at E+11: rung 0 cooling, 30 - 11 = 19 s left. The number is unchanged
        // from the userRecover version because the stamp moved from T to E together with the entry.
        sec(10.0)
        assertTrue(synchronized(logLines) { logLines.joinToString("\n") }, logHas("rung 0 still cooling down (19s left)"))
    }

    @Test fun f008_settleWindowRunsOnTheInjectedClock() {
        // The supervisor reads no other time source, so a wall-clock jump cannot reach it (also
        // checked structurally by the harness: no currentTimeMillis in SessionSupervisor.kt). The
        // virtual clock moves while the wall clock barely does, so a wall-clock window would still
        // be open at +31 s here.
        armed()
        a.linkAlive = true
        val healthy = Ocbm.BH_HCI_PRESENT or Ocbm.BH_ROOTFS_OK
        s.onBoxHealth(healthy); drain()        // baseline
        enterLadderViaGrace(); sec(20.0)       // rung 0 at E, rung 1 at E+20 s: settle until E+30 s
        sec(9.0)                               // E+29 s
        clearLog()
        s.onBoxHealth(Ocbm.BH_ROOTFS_OK); drain()
        assertTrue(logHas("self-inflicted"))
        s.onBoxHealth(healthy); drain()
        sec(2.0)                               // E+31 s: the window is closed
        clearLog()
        s.onBoxHealth(Ocbm.BH_ROOTFS_OK); drain()
        assertFalse(logHas("self-inflicted"))
        assertTrue(logHas("box health regressed:"))
        assertEquals(RxPhase.BOX_UNHEALTHY, a.phase)
    }

    // ---- F010 ------------------------------------------------------------------------------

    @Test fun f010_skippedRung1SurvivesARung2Cooldown() {
        armed()
        a.linkAlive = true
        enterLadderViaGrace()                  // t=0 (E)  rung 0
        sec(20.0)                              // t=20  rung 1 (CT_RADIO)
        sec(35.0)                              // t=55  rung 2 (restart) -> stamp
        assertEquals(1, a.restarts)
        sec(45.0)                              // t=100 exhausted -> STALLED
        assertEquals(RxPhase.STALLED, a.phase)
        sec(1.0)
        s.onSessionUp(); drain()
        sec(1.0)
        s.onSessionDown(EndReason.CLOSED); drain()   // rung = 0, stamps kept
        sec(10.0)                              // t=112 grace expiry -> rung 0 runs (stamp 0 is >30 s old)
        a.linkAlive = false
        sec(20.0)                              // t=132 rung 1 skipped, rung 2 cooling (stamp t=55)
        assertTrue(logHas("rung 2 still cooling down"))
        assertEquals(RxPhase.ARMED, a.phase)
        sec(104.0)                             // t=236 rung 2 skipped -> exhausted
        assertEquals(RxPhase.STALLED, a.phase)
        clearLog()
        s.onBoxSubscribed(); drain()
        assertTrue("STALLED kept skippedAtRung=1, so the subscribe is honoured as rung 1",
            logHas("box subscribed after the ladder skipped rung 1 for no link"))
    }

    // ---- F012 ------------------------------------------------------------------------------

    @Test fun f012_terminalTextAfterBtClaimsNoHotspotJoin() {
        armed()
        a.linkAlive = true
        s.onBtPhase(Ocbm.BTP_WIFI_HANDOFF); drain()
        sec(45.0)                              // watchdog -> ARMED, rung 0
        sec(20.0); sec(35.0); sec(45.0)        // rung 1, rung 2, exhausted
        assertEquals(RxPhase.STALLED, a.phase)
        assertEquals("Bluetooth completed but no CarPlay connection followed — check the hotspot SSID and passphrase " +
            "on this screen match the vehicle hotspot, then reconnect Bluetooth from the iPhone", a.detail)
        // v5.8 V7 fix1 / Q014: the SSID is never formatted into this (logged, exported) text.
        assertFalse(a.detail.contains("TestNet"))
    }

    // ---- F013 ------------------------------------------------------------------------------

    @Test fun f013_radiosHeldTextOnlyWhenDispatched() {
        armed()
        s.onSessionUp(); drain()
        a.radiosOk = false
        s.onBoxRelinked(); drain()
        assertEquals(RxPhase.SESSION_UP, a.phase)
        assertEquals("box back; could not hold its radios down — session intact", a.detail)
        a.radiosOk = true
        s.onBoxRelinked(); drain()
        assertEquals("box back; radios held down, session intact", a.detail)
    }

    @Test fun f013_boxLostTextSaysTheLinkResubscribes() {
        armed()
        s.onBoxLost(relinking = false); drain()
        assertEquals(RxPhase.IDLE, a.phase)
        assertEquals("the adapter dropped the host — the link re-subscribes on its own; press Start if it stays here", a.detail)
    }

    // ---- F029 ------------------------------------------------------------------------------

    @Test fun f029_receiverReadyWithSubscribedBoxGoesArmed() {
        a.linkAlive = true
        s.onReceiverReady(true, "ok"); drain()
        assertEquals(RxPhase.ARMED, a.phase)
        assertEquals("receiver ready; the adapter is already subscribed — waiting for the iPhone", a.detail)
    }

    @Test fun f029_receiverReadyWithoutLinkStaysRxReady() {
        s.onReceiverReady(true, "ok"); drain()
        assertEquals(RxPhase.RX_READY, a.phase)
    }

    // ---- v5.7.1 seams (U4) -----------------------------------------------------------------

    @Test fun seam_receiverReadyFromAStoppedReceiverIsIgnored() {
        s.onReceiverReady(false, "listening FAIL", stillCurrent = { false }); drain()
        s.onReceiverReady(true, "ok", stillCurrent = { false }); drain()
        assertTrue(a.reports.isEmpty())
        assertTrue(logHas("ignoring readiness from a receiver that is being stopped or was replaced"))
    }

    @Test fun seam_receiverNotReadyStillUnguardedByPhase() {
        armed()
        s.onSessionUp(); drain()
        s.onReceiverReady(false, "listening FAIL"); drain()
        assertEquals(RxPhase.RX_UNHEALTHY, a.phase)
    }

    /** plan v5.9 UI-1 (R1): rewritten from seam_userRecoverInIdleAnswersAndDoesNothing. */
    @Test fun seam_operatorRestartInIdleIsHarmless() {
        s.onOperatorRestart(); drain()
        assertEquals(listOf(RxPhase.IDLE to "operator restart"), a.reports)
        sec(300.0)
        assertFalse(logHas("recovery rung"))
        assertEquals(0, a.reannounces)
        assertTrue(a.radios.isEmpty())
        assertEquals(0, a.subscribes)
    }

    // ---- plan v5.9 UI-1: the merged Restart Session ------------------------------------------------

    /** Rung 2 has run; the restart idles, the re-subscribe arms, and the next ladder is not cooldown-blocked. */
    @Test fun operatorRestart_clearsCooldownStamps_andIdles() {
        armed()
        a.linkAlive = true
        enterLadderViaGrace()                  // t=0 (E) rung 0
        sec(20.0)                              // t=20 rung 1
        sec(35.0)                              // t=55 rung 2 (stamped)
        assertEquals(1, a.restarts)
        s.onOperatorRestart(); drain()
        assertEquals("IDLE between the restart and the re-subscribe", RxPhase.IDLE, a.phase)
        s.onRestartSubscribed(); drain()
        assertEquals(RxPhase.IDLE, a.phase)
        armed()
        clearLog()
        enterLadderViaGrace()                  // t=65: rung 0 (would run anyway: 65 s > 30 s)
        sec(20.0)                              // t=85: rung 1 — blocked by a kept stamp (20 < 90 s)
        sec(35.0)                              // t=120: rung 2 — blocked by a kept stamp (65 < 180 s)
        assertFalse(synchronized(logLines) { logLines.joinToString("\n") }, logHas("still cooling down"))
        assertEquals("rung 2 ran again", 2, a.restarts)
        assertEquals(listOf(false, true, false, true), a.radios)
    }

    /** The restart's OWN clear, without the re-subscribe's (mutation guard for onOperatorRestart). */
    @Test fun operatorRestart_aloneClearsCooldownStamps() {
        armed()
        a.linkAlive = true
        enterLadderViaGrace()                  // t=0 rung 0 stamped
        s.onOperatorRestart(); drain()
        armed()
        clearLog()
        enterLadderViaGrace()                  // t=10: rung 0 again — 10 s < 30 s if the stamp were kept
        assertFalse(logHas("rung 0 still cooling down"))
        assertEquals(2, a.reannounces)
    }

    /** R2 ordering: a rung run (and stamped) during the settle is cleared by onRestartSubscribed. */
    @Test fun operatorRestart_healthRegressionDuringSettle_isClearedByResubscribe() {
        s.onOperatorRestart(); drain()
        assertEquals(RxPhase.IDLE, a.phase)
        s.onBoxHealth(Ocbm.BH_HCI_PRESENT or Ocbm.BH_ROOTFS_OK); drain()   // baseline
        s.onBoxHealth(Ocbm.BH_ROOTFS_OK); drain()                          // regression in IDLE
        assertEquals(RxPhase.BOX_UNHEALTHY, a.phase)
        assertTrue("rung 0 ran (and stamped) during the settle", logHas("recovery rung 0"))
        assertEquals(1, a.reannounces)
        s.onRestartSubscribed(); drain()
        armed()
        clearLog()
        enterLadderViaGrace()                  // 10 s after the settle's rung 0: cooling if not cleared
        assertFalse(logHas("rung 0 still cooling down"))
        assertTrue(logHas("recovery rung 0"))
        assertEquals(2, a.reannounces)
    }

    /** Stop (and Forget) keep the stamps: onStopped is unchanged by the merge. */
    @Test fun stop_keepsCooldownStamps() {
        armed()
        enterLadderViaGrace()                  // t=0 rung 0 stamped
        s.onStopped(); drain()
        assertEquals(RxPhase.IDLE, a.phase)
        armed()
        clearLog()
        enterLadderViaGrace()                  // t=10: rung 0 cooling, 30 - 10 = 20 s left
        assertTrue(synchronized(logLines) { logLines.joinToString("\n") }, logHas("rung 0 still cooling down (20s left)"))
        assertEquals(1, a.reannounces)
    }

    // ---- plan v5.9 rev c C4: STALLED carries its cause ----------------------------------------------

    @Test fun stallCause_restartDeadlineInBoxLinkedIsAdapter() {
        s.onBoxLinked(true); drain()
        assertEquals(RxPhase.BOX_LINKED, a.phase)
        s.onRestartSubscribed(); drain()
        sec(45.0)
        assertEquals(RxPhase.STALLED, a.phase)
        assertEquals(StallCause.ADAPTER, a.lastStall)
    }

    @Test fun stallCause_restartDeadlineInArmedIsPhone() {
        armed()
        s.onRestartSubscribed(); drain()
        sec(45.0)
        assertEquals(RxPhase.STALLED, a.phase)
        assertEquals(StallCause.PHONE, a.lastStall)
    }

    @Test fun stallCause_dialStallIsPhone() {
        armed()
        s.onStalled(3); drain()
        assertEquals(StallCause.PHONE, a.lastStall)
    }

    @Test fun stallCause_ladderExhaustionWithoutTheBoxLinkIsAdapter() {
        armed()
        a.linkAlive = false
        enterLadderViaGrace(); sec(20.0)       // rungs 1 + 2 skip (no link) -> exhausted
        assertEquals(RxPhase.STALLED, a.phase)
        assertEquals(StallCause.ADAPTER, a.lastStall)
        s.onSessionUp(); drain()
        assertNull("a non-STALLED report carries no cause", a.lastStall)
    }

    @Test fun stallCause_ladderExhaustionWithTheBoxLinkIsPhone() {
        armed()
        a.linkAlive = true
        enterLadderViaGrace()                  // t=0 rung 0
        sec(20.0); sec(35.0); sec(45.0)        // rung 1, rung 2, exhausted at t=100 (as f010)
        assertEquals(RxPhase.STALLED, a.phase)
        assertEquals(StallCause.PHONE, a.lastStall)
    }

    // ---- B-F1 (2026-10-08): onBoxLost cancels the ladder's retry; onRelinkEnded rewrites STALLED/ADAPTER --

    /** (a) A rung-0 hold's retry must not fire into IDLE while the relink is re-claiming the box. */
    @Test fun bf1_boxLostDuringRung0HoldDoesNotStall() {
        armed()
        enterLadderViaGrace()                  // rung 0 at E, blind retry due at E+20
        sec(5.0)
        s.onBoxLost(relinking = true); drain()
        assertEquals(RxPhase.IDLE, a.phase)
        a.adapter = AdapterState.LINKING
        clearLog()
        sec(30.0)                              // past E+20: the old retry would have run rungs 1-2 skipped -> STALLED
        assertEquals(RxPhase.IDLE, a.phase)
        assertFalse(logHas("recovery ladder exhausted"))
        assertFalse(logHas("recovery rung"))
        assertEquals(0, a.restarts)
        assertEquals(emptyList<Boolean>(), a.radios)
        assertEquals(false, a.backoff)
    }

    /** (b) Same after rung 1 ran (retry due 35 s later), and the non-relinking branch too. */
    @Test fun bf1_boxLostAfterRung1DoesNotStall_bothBranches() {
        for (relinking in listOf(true, false)) {
            setUp()
            armed()
            a.linkAlive = true
            enterLadderViaGrace()
            sec(20.0)                          // rung 1 (CT_RADIO off), retry due at +35
            assertEquals(listOf(false), a.radios)
            sec(5.0)
            a.linkAlive = false
            s.onBoxLost(relinking = relinking); drain()
            assertEquals(RxPhase.IDLE, a.phase)
            clearLog()
            sec(60.0)
            assertEquals("relinking=$relinking", RxPhase.IDLE, a.phase)
            assertFalse("relinking=$relinking", logHas("recovery ladder exhausted"))
            assertEquals("relinking=$relinking", 0, a.restarts)
        }
    }

    /**
     * (c) The spurious rung 2: a retry armed by rung 1 outlived a SUCCESSFUL relink and fired
     * MGMT_RESTART_WIRELESS ~27 s into the fresh link. Not the STALLED-by-skip honour path
     * (onBoxSubscribed from STALLED), which schedules rung 2 on purpose.
     */
    @Test fun bf1_noRung2AfterASuccessfulRelink() {
        armed()
        a.linkAlive = true
        enterLadderViaGrace()
        sec(20.0)                              // rung 1 ran; retry due at +35
        sec(8.0)
        a.linkAlive = false
        s.onBoxLost(relinking = true); drain()
        assertEquals(RxPhase.IDLE, a.phase)
        a.linkAlive = true
        s.onBoxSubscribed(); drain()           // the relink re-subscribed
        assertEquals(RxPhase.ARMED, a.phase)
        clearLog()
        sec(60.0)                              // far past the old +35 retry
        assertEquals(0, a.restarts)
        assertEquals(RxPhase.ARMED, a.phase)
        assertFalse(logHas("recovery rung 2"))
    }

    /**
     * (d) GRACE is untouched (its expiry runs the ladder), so a box lost IN grace still exhausts into
     * STALLED blaming the ADAPTER with its "connecting — wait a few seconds" text. A capped/expired
     * relink must then rewrite that text; a STALLED blamed on the PHONE keeps its own.
     */
    @Test fun bf1_relinkEndedRewritesAnAdapterStall_notAPhoneStall() {
        armed()
        a.linkAlive = true
        s.onSessionUp(); s.onSessionDown(EndReason.CLOSED); drain()
        s.onBoxLost(relinking = true); drain()
        assertEquals(RxPhase.GRACE, a.phase)
        a.linkAlive = false
        a.adapter = AdapterState.LINKING
        sec(10.0)                              // grace expiry: rung 0 holds 20 s
        sec(20.0)                              // rungs 1 and 2 skipped (no link) -> exhausted
        assertEquals(RxPhase.STALLED, a.phase)
        assertEquals(StallCause.ADAPTER, a.lastStall)
        assertEquals(AdapterState.LINKING.text, a.detail)
        val ended = "the adapter did not come back within ten minutes — plug it in and press Start"
        s.onRelinkEnded(ended); drain()
        assertEquals(RxPhase.STALLED, a.phase)
        assertEquals(ended, a.detail)
        assertEquals(StallCause.ADAPTER, a.lastStall)
        // A phone-blamed stall stands.
        setUp(); armed()
        s.onStalled(9); drain()
        assertEquals(StallCause.PHONE, a.lastStall)
        val before = a.detail
        s.onRelinkEnded(ended); drain()
        assertEquals(before, a.detail)
        assertEquals(RxPhase.STALLED, a.phase)
        // And IDLE is rewritten as before.
        setUp()
        s.onBoxLost(relinking = true); drain()
        s.onRelinkEnded(ended); drain()
        assertEquals(RxPhase.IDLE, a.phase)
        assertEquals(ended, a.detail)
    }

    /** (e) POSITIVE: after the cancel a relinked box with a silent phone still escalates on its own clocks. */
    @Test fun bf1_silentPhoneAfterRelinkStillEscalates() {
        armed()
        enterLadderViaGrace()
        sec(5.0)
        s.onBoxLost(relinking = true); drain()
        a.linkAlive = true
        s.onBoxSubscribed(); drain()
        assertEquals(RxPhase.ARMED, a.phase)
        s.onBtPhase(Ocbm.BTP_LINK_UP); drain()  // 45 s handoff watchdog
        assertEquals(RxPhase.BT_PAIRING, a.phase)
        clearLog()
        sec(45.0)
        assertTrue(logHas("no CarPlay connect within 45s"))
        assertTrue(logHas("recovery rung 0"))   // the ladder restarted from rung 0 (cooldown from E is over)
        assertEquals(RxPhase.ARMED, a.phase)
        // and the dial-count stall path is unaffected
        s.onStalled(9); drain()
        assertEquals(RxPhase.STALLED, a.phase)
    }

    // ---- A-F1 (2026-10-08): session-down reasons after the SESSION_UP move to pair-verify -------

    /** A real phone that connects and closes before pair-verify still gets GRACE, and not the fast ladder. */
    @Test fun af1_earlyCloseOfARealPhoneKeepsGrace_notFast() {
        armed()
        a.linkAlive = true
        s.onSessionDown(EndReason.CLOSED); drain()       // no onSessionUp came first (verify never completed)
        assertEquals(RxPhase.GRACE, a.phase)
        assertTrue(a.detail, a.detail.startsWith("control connection closed before a session formed"))
        sec(10.0)                                        // grace expiry: not fast -> rung 0 holds, no CT_RADIO yet
        assertEquals(RxPhase.ARMED, a.phase)
        assertEquals(emptyList<Boolean>(), a.radios)
        sec(20.0)
        assertEquals("rung 1 after rung 0's hold, as for any CLOSED", listOf(false), a.radios)
    }

    /** SessionCore never hands UNVERIFIED over; if it ever did, it must read as CLOSED (not fast). */
    @Test fun af1_unverifiedIfEverHandedOverIsNotFast() {
        armed()
        a.linkAlive = true
        s.onSessionDown(EndReason.UNVERIFIED); drain()
        assertEquals(RxPhase.GRACE, a.phase)
        sec(10.0)
        assertEquals(emptyList<Boolean>(), a.radios)     // LINK_LOST would have run rung 1 in this pass
    }
}
