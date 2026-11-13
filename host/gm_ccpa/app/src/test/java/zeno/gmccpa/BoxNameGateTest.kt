package zeno.gmccpa

import java.util.concurrent.CyclicBarrier
import java.util.concurrent.atomic.AtomicBoolean
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Wave 3 fix round 2: the adapter-name gate SessionCore uses. Host JVM, no Android. */
class BoxNameGateTest {
    @get:org.junit.Rule val timeout: org.junit.rules.Timeout = org.junit.rules.Timeout.seconds(30)

    private val painted = mutableListOf<String>()
    private val gate = BoxNameGate { synchronized(painted) { painted += it } }

    @Test fun liveOffer_shows_thenUnchanged() {
        assertEquals(BoxNameGate.Offer.SHOWN, gate.offer("CarLink-0001") { true })
        assertEquals(BoxNameGate.Offer.UNCHANGED, gate.offer("CarLink-0001") { true })
        assertEquals(listOf("CarLink-0001"), painted)
        assertEquals("CarLink-0001", gate.current())
    }

    @Test fun staleOffer_paintsNothing() {
        assertEquals(BoxNameGate.Offer.STALE, gate.offer("CarLink-0001") { false })
        assertTrue(painted.isEmpty())
        assertEquals("", gate.current())
    }

    @Test fun clear_onlyWhenShown() {
        assertFalse(gate.clear())
        gate.offer("CarLink-0001") { true }
        assertTrue(gate.clear())
        assertFalse(gate.clear())
        assertEquals(listOf("CarLink-0001", ""), painted)
    }

    /** Clearing path order: the link reads dead FIRST, then clear. A late reply after that is dropped. */
    @Test fun lateReplyAfterDeathAndClear_isDropped() {
        val live = AtomicBoolean(true)
        gate.offer("CarLink-0001") { live.get() }
        live.set(false); gate.clear()                        // transport death: CAS, then clear
        assertEquals(BoxNameGate.Offer.STALE, gate.offer("CarLink-0001") { live.get() })
        assertEquals("", gate.current())
    }

    /** A reply offered just BEFORE the death is overwritten by the clear that follows it. */
    @Test fun replyBeforeDeath_isClearedAfter() {
        val live = AtomicBoolean(true)
        gate.offer("CarLink-0002") { live.get() }
        live.set(false); gate.clear()
        assertEquals("", gate.current())
        assertEquals("", painted.last())
    }

    /** The liveness check runs INSIDE the gate's lock: a clear cannot slip between check and paint. */
    @Test fun livenessCheckedUnderTheLock() {
        val inCheck = java.util.concurrent.CountDownLatch(1)
        val release = java.util.concurrent.CountDownLatch(1)
        val live = AtomicBoolean(true)
        val offerer = Thread { gate.offer("CarLink-0003") { inCheck.countDown(); release.await(); live.get() } }
        offerer.isDaemon = true
        offerer.start()
        // Bounded: a gate that never calls stillLive must FAIL here, not hang the suite.
        assertTrue("offer never evaluated stillLive", inCheck.await(2, java.util.concurrent.TimeUnit.SECONDS))
        val clearer = Thread { live.set(false); gate.clear() }
        clearer.isDaemon = true
        clearer.start()
        Thread.sleep(100)
        assertTrue("clear must wait for the in-progress offer", clearer.isAlive)
        release.countDown()
        offerer.join(2_000); clearer.join(2_000)
        assertEquals("", gate.current())
        assertEquals("", painted.lastOrNull() ?: "")
    }

    /** Racing reply vs death, many times: the screen always ends blank. */
    @Test fun race_alwaysEndsCleared() {
        repeat(2_000) {
            val g = BoxNameGate { }
            val live = AtomicBoolean(true)
            val b = CyclicBarrier(2)
            val a = Thread { b.await(); g.offer("CarLink-0004") { live.get() } }.apply { isDaemon = true }
            val d = Thread { b.await(); live.set(false); g.clear() }.apply { isDaemon = true }
            a.start(); d.start(); a.join(2_000); d.join(2_000)
            assertEquals("", g.current())
        }
    }
}
