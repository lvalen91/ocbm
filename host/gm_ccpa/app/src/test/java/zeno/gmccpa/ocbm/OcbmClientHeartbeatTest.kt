package zeno.gmccpa.ocbm

import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicInteger
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * P-H1-1 (plan v5.6 H1-1..H1-3, reviews A2/B-N2): the heartbeat's dead-link handoff, the beat-only
 * probe and the subscribe-in-flight races, on [FakeTransport] with real 1 s ticks. trace = false and
 * a silent logger, so no Android class is touched at runtime.
 */
class OcbmClientHeartbeatTest {

    private val fake = FakeTransport()
    // v5.8 V7 / Q015: the client's deadlines run on an injected monotonic clock; the tier-0 harness
    // stubs android.os.SystemClock to THROW, so the default would fail the first subscribe().
    private val c = OcbmClient(fake, zeno.gmccpa.ProbeLog.silent(), 0x5A5A1234, trace = false, monoMs = { System.nanoTime() / 1_000_000L })
    /** HOST_GONE the client forwarded itself (box frame, or its fallback synthesis). */
    private val events = CopyOnWriteArrayList<Byte>()
    /** Deliveries by the simulated transport-death edge (MainActivity.onBoxTransportDead's CAS). */
    private val edgeDeliveries = AtomicInteger(0)

    private fun ctrl(vararg b: Byte) = Framing.frame(Ocbm.CH_CTRL, Ocbm.F_BOTH, 0, b)
    private fun isCtrl(f: ByteArray, type: Byte) =
        f.size > Ocbm.HDR_LEN && f[8] == 0.toByte() && f[9] == 0.toByte() && f[Ocbm.HDR_LEN] == type
    private fun attemptsOf(type: Byte, from: Int = 0) = fake.attempts.toList().drop(from).count { isCtrl(it, type) }
    private fun mark() = fake.attempts.size

    private fun waitFor(ms: Long, cond: () -> Boolean): Boolean {
        val end = System.currentTimeMillis() + ms
        while (System.currentTimeMillis() < end) { if (cond()) return true; Thread.sleep(20) }
        return cond()
    }

    /** Return right after a fresh heartbeat tick wrote, so the next tick is ~1 s away. */
    private fun afterTick() {
        val n = attemptsOf(Ocbm.CT_HEARTBEAT)
        assertTrue("a heartbeat tick", waitFor(2500) { attemptsOf(Ocbm.CT_HEARTBEAT) > n })
    }

    private fun bringUp(helloAck: Boolean = true) {
        c.onSessionEvent = { sev -> events.add(sev) }
        fake.start()
        c.start()
        if (helloAck) {
            fake.feed(ctrl(Ocbm.CT_HELLO_ACK, 1, 0x3F, 0, 0, 0, Ocbm.MODE_PROJECTION))
            assertTrue(c.helloAcked)
        }
    }

    /** Simulate the USB transport: declareDead -> read loop ends -> MainActivity CASes the latch. */
    private fun transportEdgeRuns() {
        fake.declareDeadResult = true
        fake.onDeclareDead = {
            Thread {
                Thread.sleep(150)
                if (c.hostGoneDelivered.compareAndSet(false, true)) edgeDeliveries.incrementAndGet()
            }.start()
        }
    }

    private val cfg = "ssid: x\n".toByteArray()

    @After fun tearDown() { c.stop() }

    /** (a) subscribed, 5 failures -> one declareDead, no CT_SUBSCRIBE / CT_HELLO, the latch set once. */
    @Test fun subscribedFiveFailuresHandsTheLinkToTheTransportEdge() {
        bringUp()
        transportEdgeRuns()
        assertTrue(c.subscribe(cfg))
        c.startHeartbeat()
        afterTick()
        val m = mark()
        fake.failWrites = true
        assertTrue("declareDead within 8 s", waitFor(8000) { fake.declareDeadCalls.get() >= 1 })
        Thread.sleep(3500)
        assertEquals(1, fake.declareDeadCalls.get())
        assertEquals(5, attemptsOf(Ocbm.CT_HEARTBEAT, m))
        assertEquals(0, attemptsOf(Ocbm.CT_SUBSCRIBE, m))
        assertEquals(0, attemptsOf(Ocbm.CT_HELLO, m))
        assertEquals("the transport edge delivered it", 1, edgeDeliveries.get())
        assertTrue(c.hostGoneDelivered.get())
        assertTrue("no synthetic HOST_GONE when the edge ran", events.isEmpty())
        assertFalse(c.subscribed)
        assertFalse(c.helloAcked)
    }

    /** (b) 4 failures then a success -> no declareDead (no flap). */
    @Test fun fourFailuresThenSuccessDoesNotDeclareDead() {
        bringUp()
        transportEdgeRuns()
        assertTrue(c.subscribe(cfg))
        c.startHeartbeat()
        afterTick()
        val m = mark()
        fake.failNext.set(4)
        assertTrue(waitFor(9000) { attemptsOf(Ocbm.CT_HEARTBEAT, m) >= 7 })
        assertEquals(0, fake.declareDeadCalls.get())
        assertTrue(c.subscribed)
        assertTrue(events.isEmpty())
    }

    /** (c) never subscribed -> a CT_HEARTBEAT every tick, the box nudge ignored, 5 failures -> declareDead, never a CT_SUBSCRIBE. */
    @Test fun noSubscribeProbeBeatsAndDeclaresDead() {
        bringUp()
        transportEdgeRuns()
        c.startHeartbeat()
        assertTrue(waitFor(3500) { attemptsOf(Ocbm.CT_HEARTBEAT) >= 3 })
        // The box's 30 s nudge to an unsubscribed beat: ignored, not forwarded.
        fake.feed(ctrl(Ocbm.CT_SESSION_EVENT, Ocbm.SEV_HOST_GONE))
        assertTrue(events.isEmpty())
        assertTrue(c.helloAcked)
        fake.failWrites = true
        assertTrue(waitFor(8000) { fake.declareDeadCalls.get() >= 1 })
        Thread.sleep(2500)
        assertEquals(1, fake.declareDeadCalls.get())
        assertEquals(0, attemptsOf(Ocbm.CT_SUBSCRIBE))
        assertEquals(1, edgeDeliveries.get())
        assertTrue(events.isEmpty())
        assertFalse(c.helloAcked)
    }

    /** (d) HOST_GONE recovery with failing re-SUBSCRIBE writes -> declareDead after 5. */
    @Test fun failingRecoveryResubscribesDeclareDead() {
        bringUp()
        transportEdgeRuns()
        assertTrue(c.subscribe(cfg))
        c.startHeartbeat()
        afterTick()
        // A real box HOST_GONE (we hold a subscription): forwarded once, recovery starts.
        fake.feed(ctrl(Ocbm.CT_SESSION_EVENT, Ocbm.SEV_HOST_GONE))
        assertEquals(listOf(Ocbm.SEV_HOST_GONE), events.toList())
        assertFalse(c.subscribed)
        val m = mark()
        fake.failWrites = true
        // Backoff 0, 2, 4, 8, 15 s between the five attempts (~30 s).
        assertTrue("declareDead within 45 s", waitFor(45_000) { fake.declareDeadCalls.get() >= 1 })
        Thread.sleep(500)
        assertEquals(1, fake.declareDeadCalls.get())
        assertEquals(5, attemptsOf(Ocbm.CT_SUBSCRIBE, m))
        assertEquals("recovery beats nothing", 0, attemptsOf(Ocbm.CT_HEARTBEAT, m))
        assertEquals(1, edgeDeliveries.get())
        assertEquals("only the box's own HOST_GONE", 1, events.size)
    }

    /** (e) declareDead returns false -> exactly one synthetic HOST_GONE. */
    @Test fun noTransportEdgeSynthesisesOneHostGone() {
        bringUp()
        fake.declareDeadResult = false
        assertTrue(c.subscribe(cfg))
        c.startHeartbeat()
        afterTick()
        fake.failWrites = true
        assertTrue(waitFor(8000) { fake.declareDeadCalls.get() >= 1 })
        assertTrue(waitFor(1000) { events.isNotEmpty() })
        Thread.sleep(2000)
        assertEquals(listOf(Ocbm.SEV_HOST_GONE), events.toList())
        assertTrue(c.hostGoneDelivered.get())
        assertEquals(1, fake.declareDeadCalls.get())
    }

    /**
     * (f) + A2/B-N2: a subscribe() on another thread racing the probe's ticks, its write held for
     * 4.5 s (longer than the 2 s first-sight deferral), with the box's HOST_GONE nudge for an earlier
     * probe beat landing mid-write -> exactly one CT_SUBSCRIBE, the HOST_GONE not forwarded,
     * `subscribed` true afterwards.
     */
    @Test fun subscribeRacingTheProbeSendsOneSubscribeAndIgnoresTheNudge() {
        bringUp()
        transportEdgeRuns()
        c.startHeartbeat()
        assertTrue(waitFor(3500) { attemptsOf(Ocbm.CT_HEARTBEAT) >= 2 })
        val nudgeFed = AtomicInteger(0)
        fake.writeHook = { f ->
            if (isCtrl(f, Ocbm.CT_SUBSCRIBE)) {
                Thread {
                    Thread.sleep(1500)
                    fake.feed(ctrl(Ocbm.CT_SESSION_EVENT, Ocbm.SEV_HOST_GONE))
                    nudgeFed.incrementAndGet()
                }.start()
                Thread.sleep(4500)
            }
        }
        val ok = AtomicInteger(-1)
        val t = Thread { ok.set(if (c.subscribe(cfg)) 1 else 0) }
        t.start()
        t.join(10_000)
        assertEquals(1, ok.get())
        assertEquals(1, nudgeFed.get())
        Thread.sleep(3500)
        assertEquals(1, attemptsOf(Ocbm.CT_SUBSCRIBE))
        assertTrue("nudge not forwarded: $events", events.isEmpty())
        assertTrue(c.subscribed)
        assertEquals(0, fake.declareDeadCalls.get())
        // The heartbeat is in subscribed mode now.
        val m = mark()
        assertTrue(waitFor(2500) { attemptsOf(Ocbm.CT_HEARTBEAT, m) >= 1 })
    }

    /** The same nudge AFTER subscribe() returned is a real drop: forwarded, `subscribed` cleared. */
    @Test fun hostGoneAfterSubscribeReturnedIsReal() {
        bringUp()
        assertTrue(c.subscribe(cfg))
        fake.feed(ctrl(Ocbm.CT_SESSION_EVENT, Ocbm.SEV_HOST_GONE))
        assertEquals(listOf(Ocbm.SEV_HOST_GONE), events.toList())
        assertFalse(c.subscribed)
    }

    /**
     * B-M1: a HOST_GONE after the CT_SUBSCRIBE write, while subscribe() is still inside its CT_SETTIME
     * write (`subscribed` already true), is a real drop: forwarded and the latch cleared — not dropped
     * as "in flight". A HOST_GONE during the CT_SUBSCRIBE write itself is still ignored.
     */
    @Test fun hostGoneAfterSubscribeWriteButBeforeReturnIsReal() {
        bringUp()
        val before = AtomicInteger(0)
        val after = AtomicInteger(0)
        val latchedAtSetTime = AtomicInteger(0)
        fake.writeHook = { f ->
            if (isCtrl(f, Ocbm.CT_SUBSCRIBE)) {
                fake.feed(ctrl(Ocbm.CT_SESSION_EVENT, Ocbm.SEV_HOST_GONE)); before.incrementAndGet()
            } else if (isCtrl(f, Ocbm.CT_SETTIME) && after.get() == 0) {
                if (c.subscribed) latchedAtSetTime.set(1)
                fake.feed(ctrl(Ocbm.CT_SESSION_EVENT, Ocbm.SEV_HOST_GONE)); after.incrementAndGet()
            }
        }
        assertTrue(c.subscribe(cfg))
        assertEquals(1, before.get())
        assertEquals(1, after.get())
        assertEquals("latch set before the post-subscribe writes", 1, latchedAtSetTime.get())
        assertEquals("only the post-write HOST_GONE forwarded", listOf(Ocbm.SEV_HOST_GONE), events.toList())
        assertFalse(c.subscribed)
    }
}
