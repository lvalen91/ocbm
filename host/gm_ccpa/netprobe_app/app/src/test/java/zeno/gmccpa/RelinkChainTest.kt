package zeno.gmccpa

import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * P-H1-2 (plan v5.6 H1-4, reviews B1 and A3): the box relink's dead-run streak, cap and backoff,
 * on a fake probe, a fake clock and a fake sleeper. Runs on the real single-thread executor shape
 * BoxRelink uses, so the arm -> run -> finish -> arm recursion is the production one.
 */
class RelinkChainTest {

    private class FakeProbe {
        @Volatile var stopped = false
        @Volatile var link = false
        @Volatile var expired = false
        @Volatile var enumerated = true
    }

    private data class Res(val claimed: Boolean, val helloOk: Boolean, val helloAtMs: Long = 0L, val error: String? = null)

    private class Harness {
        val clock = AtomicLong(10_000_000L)
        val probe = FakeProbe()
        @Volatile var live = false
        val runs = AtomicInteger(0)
        /** Backoff slept before each runAll, in run order. */
        val sleptBeforeRun = CopyOnWriteArrayList<Long>()
        private val sleptSinceRun = AtomicLong(0)
        val reports = CopyOnWriteArrayList<Res>()
        /** The `expired` argument of every report, in order. */
        val reportsExpired = CopyOnWriteArrayList<Boolean>()
        val capped = CopyOnWriteArrayList<String>()
        val logs = CopyOnWriteArrayList<String>()
        @Volatile var onSleep: ((Long) -> Unit)? = null
        /** Called inside runAll with the 0-based run index; returns the result. */
        @Volatile var script: (Int) -> Res = { Res(false, false) }
        val exec = Executors.newSingleThreadExecutor { r -> Thread(r, "test-relink").apply { isDaemon = true } }
        val chain: RelinkChain<FakeProbe, Res> = RelinkChain(
            env = object : RelinkChain.Env<FakeProbe, Res> {
                override fun probe() = probe
                override fun stopped(p: FakeProbe) = p.stopped
                override fun hasLink(p: FakeProbe) = p.link
                override fun waitExpired(p: FakeProbe) = p.expired
                override fun adapterEnumerated(p: FakeProbe) = p.enumerated
                override fun stopRequested() = false
                override fun sessionLive() = live
                override fun runAll(p: FakeProbe, cancelled: () -> Boolean): Res {
                    sleptBeforeRun.add(sleptSinceRun.getAndSet(0))
                    val i = runs.getAndIncrement()
                    return script(i)
                }
                override fun outcome(r: Res) = RelinkChain.Outcome(r.claimed, r.helloOk, r.helloAtMs)
                override fun report(r: Res, expired: Boolean) { reports.add(r); reportsExpired.add(expired) }
                override fun capped(detail: String) { capped.add(detail) }
                override fun failed(detail: String) = Res(claimed = false, helloOk = false, error = detail)
            },
            exec = exec,
            log = object : RelinkChain.Log {
                override fun i(msg: String) { logs.add("I $msg") }
                override fun w(msg: String) { logs.add("W $msg") }
                override fun e(msg: String) { logs.add("E $msg") }
            },
            nowMs = { clock.get() },
            sleepMs = { ms ->
                clock.addAndGet(ms)
                val s = sleptSinceRun.addAndGet(ms)
                onSleep?.invoke(s)
            },
        )

        /** Wait until no run owns the chain and nothing is queued behind it. */
        fun awaitIdle() {
            val deadline = System.currentTimeMillis() + 10_000
            while (System.currentTimeMillis() < deadline) {
                exec.submit {}.get(5, TimeUnit.SECONDS)
                if (chain.owner.get() == null) { exec.submit {}.get(5, TimeUnit.SECONDS); if (chain.owner.get() == null) return }
            }
            throw AssertionError("relink chain never went idle; logs=$logs")
        }
    }

    private val h = Harness()

    @After fun tearDown() { h.exec.shutdownNow() }

    /** P-H1-2: claimed-no-HELLO x5 with the adapter enumerated -> backoff 5/10/20/30 s, then the cap. */
    @Test fun claimedNoHelloFiveTimesBacksOffThenCaps() {
        h.probe.enumerated = true
        h.script = { Res(claimed = true, helloOk = false) }
        h.chain.arm("heartbeat declared the link dead", RelinkChain.Cause.TRANSPORT_DIED)
        h.awaitIdle()
        assertEquals("runs; logs=${h.logs}", 5, h.runs.get())
        assertEquals(listOf(0L, 5_000L, 10_000L, 20_000L, 30_000L), h.sleptBeforeRun.toList())
        assertEquals(1, h.capped.size)
        assertTrue("no 'press Start' report under the cap", h.reports.isEmpty())
        assertEquals(0, h.chain.streak())
    }

    /** A claim with no HELLO and the adapter OFF the bus keeps today's behaviour: report, no re-arm. */
    @Test fun claimedNoHelloUnpluggedReportsOnce() {
        h.probe.enumerated = false
        h.script = { Res(claimed = true, helloOk = false) }
        h.chain.arm("transport died")
        h.awaitIdle()
        assertEquals(1, h.runs.get())
        assertEquals(1, h.reports.size)
        assertTrue(h.capped.isEmpty())
    }

    /** A claim failure keeps today's behaviour too. */
    @Test fun claimFailureReportsOnce() {
        h.script = { Res(claimed = false, helloOk = false) }
        h.chain.arm("transport died")
        h.awaitIdle()
        assertEquals(1, h.runs.get())
        assertEquals(1, h.reports.size)
    }

    /**
     * v5.8 V7 / Q018: a runAll that THROWS is reported as a failed run carrying the exception text
     * (before: r == null, nothing reported, the IDLE detail stuck on "re-claiming"). No re-arm, so a
     * throw-every-time bring-up cannot spin; the streak is zeroed like any other chain end.
     */
    @Test fun runAllThrowReportsSyntheticFailure() {
        h.script = { throw IllegalStateException("refusing credential-less CT_SUBSCRIBE (SSID missing)") }
        h.chain.arm("transport died")
        h.awaitIdle()
        assertEquals("runs; logs=${h.logs}", 1, h.runs.get())
        assertEquals("reports; logs=${h.logs}", 1, h.reports.size)
        val r = h.reports[0]
        assertEquals(false, r.claimed)
        assertEquals(false, r.helloOk)
        assertEquals("IllegalStateException: refusing credential-less CT_SUBSCRIBE (SSID missing)", r.error)
        assertEquals(listOf(false), h.reportsExpired.toList())
        assertTrue(h.capped.isEmpty())
        assertNull(h.chain.owner.get())
        assertEquals(0, h.chain.streak())
        assertTrue("the throw is still logged; logs=${h.logs}", h.logs.any { it.startsWith("E relink failed: IllegalStateException: refusing") })
    }

    /** Q018: a throw with the claim wait expired and CarPlay live is REPORTED, not re-armed (no spin). */
    @Test fun runAllThrowWithExpiredLiveWaitDoesNotRearm() {
        h.live = true
        h.probe.expired = true
        h.script = { throw java.io.IOException("USB claim failed") }
        h.chain.arm("transport died")
        h.awaitIdle()
        assertEquals("runs; logs=${h.logs}", 1, h.runs.get())
        assertEquals(1, h.reports.size)
        assertEquals("IOException: USB claim failed", h.reports[0].error)
        // fix1: never reported as the ten-minute expiry — the stale probe flag must not hide the exception text.
        assertEquals(listOf(false), h.reportsExpired.toList())
        assertTrue(h.logs.none { it.contains("waiting again") })
    }

    /** Q018: a transport death flagged DURING a run that then throws still wins — counted re-arm, no report. */
    @Test fun runAllThrowAfterTransportDeathStillRearms() {
        h.probe.link = false
        h.script = { i ->
            if (i == 0) { h.chain.arm("a transport died during the relink"); throw IllegalStateException("boom") }
            Res(claimed = false, helloOk = false)
        }
        h.chain.arm("transport died")
        h.awaitIdle()
        assertEquals("runs; logs=${h.logs}", 2, h.runs.get())
        assertEquals(listOf(0L, 5_000L), h.sleptBeforeRun.toList())
        // Only the second (non-throwing) run reports; the thrown first run was superseded by its re-arm.
        assertEquals(1, h.reports.size)
        assertNull(h.reports[0].error)
    }

    /**
     * Review B1: helloOk relinks, each followed by a heartbeat death 30 s after its HELLO, five in a
     * row -> the cap, and no 6th claim. Before v5.6 the successful finish zeroed the streak and this
     * relinked forever.
     */
    @Test fun repeatedQuickHeartbeatDeathsAfterSuccessfulRelinksCap() {
        h.script = { h.probe.link = true; Res(claimed = true, helloOk = true, helloAtMs = h.clock.get()) }
        // The first death: no relink HELLO before it, so not counted.
        h.chain.arm("heartbeat declared the link dead", RelinkChain.Cause.HEARTBEAT_DIED)
        h.awaitIdle()
        for (k in 1..5) {
            h.clock.addAndGet(30_000L)
            h.probe.link = false
            h.chain.arm("heartbeat declared the link dead", RelinkChain.Cause.HEARTBEAT_DIED)
            h.awaitIdle()
        }
        assertEquals("claims; logs=${h.logs}", 5, h.runs.get())
        assertEquals(listOf(0L, 5_000L, 10_000L, 20_000L, 30_000L), h.sleptBeforeRun.toList())
        assertEquals(1, h.capped.size)
        // Every successful relink reported (box linked / restored); the cap itself reports nothing else.
        assertEquals(5, h.reports.size)
        // A further death after the cap would start a new episode — but nothing re-claimed meanwhile.
        assertEquals(5, h.runs.get())
    }

    /** A heartbeat death >= 60 s after the last relink HELLO is a new episode: not counted, streak zeroed. */
    @Test fun heartbeatDeathAfterTheWindowStartsANewEpisode() {
        h.script = { h.probe.link = true; Res(claimed = true, helloOk = true, helloAtMs = h.clock.get()) }
        h.chain.arm("hb", RelinkChain.Cause.HEARTBEAT_DIED)
        h.awaitIdle()
        h.clock.addAndGet(30_000L); h.probe.link = false
        h.chain.arm("hb", RelinkChain.Cause.HEARTBEAT_DIED)
        h.awaitIdle()
        assertEquals(1, h.chain.streak())
        h.clock.addAndGet(70_000L); h.probe.link = false
        h.chain.arm("hb", RelinkChain.Cause.HEARTBEAT_DIED)
        h.awaitIdle()
        assertEquals(listOf(0L, 5_000L, 0L), h.sleptBeforeRun.toList())
        assertEquals(0, h.chain.streak())
        assertTrue(h.capped.isEmpty())
    }

    /**
     * Review A3: an unplug (`again`, the run's transport died) and a failed HELLO in the SAME run
     * count once — the next run backs off 5 s (streak 1), not 10 s.
     */
    @Test fun unplugAndFailedHelloInOneRunCountOnce() {
        h.script = { i ->
            if (i == 0) {
                // The run's own transport dies while it holds the token: arm() loses the CAS -> again.
                h.chain.arm("transport died")
                h.probe.link = false
                Res(claimed = true, helloOk = false)
            } else { h.probe.link = true; Res(claimed = true, helloOk = true, helloAtMs = h.clock.get()) }
        }
        h.chain.arm("transport died")
        h.awaitIdle()
        assertEquals(2, h.runs.get())
        assertEquals(listOf(0L, 5_000L), h.sleptBeforeRun.toList())
        // The counted run brought the link up: B1 keeps the streak.
        assertEquals(1, h.chain.streak())
        assertEquals(1, h.reports.size)
    }

    /** The pre-v5.6 path stays: the transport dies during each of 5 relinks -> the cap. */
    @Test fun transportDyingDuringEveryRelinkCaps() {
        h.script = {
            h.chain.arm("transport died")
            h.probe.link = false
            Res(claimed = true, helloOk = true, helloAtMs = h.clock.get())
        }
        h.chain.arm("transport died")
        h.awaitIdle()
        assertEquals(5, h.runs.get())
        assertEquals(1, h.capped.size)
        assertTrue(h.reports.isEmpty())
    }

    /** P-H1-2: Stop during a backoff sleep -> no runAll, nothing reported, token released. */
    @Test fun stopDuringBackoffSleepRunsNothing() {
        h.probe.enumerated = true
        h.script = { Res(claimed = true, helloOk = false) }
        h.onSleep = { slept -> if (slept >= 1_000L) h.chain.cancel("operator stop") }
        h.chain.arm("transport died")
        h.awaitIdle()
        assertEquals(1, h.runs.get())
        assertTrue(h.reports.isEmpty())
        assertTrue(h.capped.isEmpty())
        assertNull(h.chain.owner.get())
        assertTrue("abandon logged; logs=${h.logs}", h.logs.any { it.contains("abandoned during its 5 s backoff") })
    }

    /** awaitBackoff itself: slices of 250 ms, false as soon as abandoned. */
    @Test fun awaitBackoffSlicesAndAbandons() {
        val slices = CopyOnWriteArrayList<Long>()
        val c = RelinkChain(
            env = object : RelinkChain.Env<FakeProbe, Res> {
                override fun probe(): FakeProbe? = null
                override fun stopped(p: FakeProbe) = false
                override fun hasLink(p: FakeProbe) = false
                override fun waitExpired(p: FakeProbe) = false
                override fun adapterEnumerated(p: FakeProbe) = false
                override fun stopRequested() = false
                override fun sessionLive() = false
                override fun runAll(p: FakeProbe, cancelled: () -> Boolean) = Res(false, false)
                override fun outcome(r: Res) = RelinkChain.Outcome(false, false, 0L)
                override fun report(r: Res, expired: Boolean) {}
                override fun capped(detail: String) {}
                override fun failed(detail: String) = Res(false, false, error = detail)
            },
            exec = { it.run() },
            log = object : RelinkChain.Log { override fun i(msg: String) {}; override fun w(msg: String) {}; override fun e(msg: String) {} },
            sleepMs = { slices.add(it) },
        )
        assertTrue(c.awaitBackoff(1_000L) { false })
        assertEquals(listOf(250L, 250L, 250L, 250L), slices.toList())
        slices.clear()
        var n = 0
        assertTrue(!c.awaitBackoff(5_000L) { ++n > 3 })
        assertEquals(3, slices.size)
    }

    /** Live-session claim-wait expiry re-arms uncounted, with no backoff. */
    @Test fun expiredWaitWithCarPlayLiveReArmsWithoutBackoff() {
        h.live = true
        h.script = { i -> h.probe.expired = (i == 0); Res(claimed = false, helloOk = false) }
        h.chain.arm("transport died")
        h.awaitIdle()
        assertEquals(2, h.runs.get())
        assertEquals(listOf(0L, 0L), h.sleptBeforeRun.toList())
        assertEquals(1, h.reports.size)
    }
}
