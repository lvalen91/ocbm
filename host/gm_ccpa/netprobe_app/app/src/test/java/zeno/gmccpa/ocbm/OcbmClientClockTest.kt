package zeno.gmccpa.ocbm

import java.util.concurrent.atomic.AtomicLong
import org.junit.After
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Plan v5.8 V7 / Q015 (fix1): the client's deadlines run on the INJECTED monotonic clock, not the
 * wall clock. The HCI-attach budget is the observable one: CT_SUBSCRIBE arms it, the heartbeat tick
 * promotes PENDING to MISSED when the injected clock passes it — and not before. The injected clock
 * is real elapsed time plus a settable OFFSET (a frozen clock would hang subscribe(): its CT_SETTIME
 * ack wait runs on the same clock since fix1), so a "clock step" is one `offset.addAndGet`.
 */
class OcbmClientClockTest {

    private val fake = FakeTransport()
    private val offset = AtomicLong(0L)
    private val c = OcbmClient(fake, zeno.gmccpa.ProbeLog.silent(), 0x5A5A1234, trace = false,
        monoMs = { System.nanoTime() / 1_000_000L + offset.get() })

    private fun ctrl(vararg b: Byte) = Framing.frame(Ocbm.CH_CTRL, Ocbm.F_BOTH, 0, b)
    private fun isCtrl(f: ByteArray, type: Byte) =
        f.size > Ocbm.HDR_LEN && f[8] == 0.toByte() && f[9] == 0.toByte() && f[Ocbm.HDR_LEN] == type
    private fun beats() = fake.attempts.toList().count { isCtrl(it, Ocbm.CT_HEARTBEAT) }
    private fun waitFor(ms: Long, cond: () -> Boolean): Boolean {
        val end = System.nanoTime() + ms * 1_000_000L
        while (System.nanoTime() < end) { if (cond()) return true; Thread.sleep(20) }
        return cond()
    }

    @After fun tearDown() { c.stop() }

    @Test fun hciWaitIsPromotedToMissedOnTheInjectedClockOnly() {
        fake.start(); c.start()
        fake.feed(ctrl(Ocbm.CT_HELLO_ACK, 1, 0x3F, 0, 0, 0, Ocbm.MODE_PROJECTION))
        assertTrue(c.helloAcked)
        assertTrue(c.subscribe("ssid: x\n".toByteArray()))
        c.startHeartbeat()
        // Three heartbeat ticks (>= 3 s, far inside the 45 s budget): still pending.
        val b0 = beats()
        assertTrue("three ticks", waitFor(6_000) { beats() >= b0 + 3 })
        assertFalse("promoted inside the budget", c.hciMissed)
        // Step the injected clock past the budget: the next tick promotes it.
        offset.addAndGet(OcbmClient.BT_ATTACH_BUDGET_MS + 1)
        assertTrue("MISSED within two ticks of the clock step", waitFor(3_000) { c.hciMissed })
    }

    @Test fun aStepShortOfTheBudgetDoesNotPromote() {
        fake.start(); c.start()
        fake.feed(ctrl(Ocbm.CT_HELLO_ACK, 1, 0x3F, 0, 0, 0, Ocbm.MODE_PROJECTION))
        assertTrue(c.subscribe("ssid: x\n".toByteArray()))
        c.startHeartbeat()
        // 10 s short of the budget, then two ticks (~2-3 s): still inside it, so no promotion.
        offset.addAndGet(OcbmClient.BT_ATTACH_BUDGET_MS - 10_000L)
        val b0 = beats()
        assertTrue(waitFor(5_000) { beats() >= b0 + 2 })
        assertFalse(c.hciMissed)
    }
}
