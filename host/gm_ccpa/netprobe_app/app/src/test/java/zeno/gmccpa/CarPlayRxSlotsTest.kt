package zeno.gmccpa

import java.net.InetAddress
import java.net.Socket
import java.util.concurrent.CountDownLatch
import java.util.concurrent.CyclicBarrier
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/** Plan v5.7 U5: F040 (incumbent slot) and F039 (browse slot). Host JVM, no Android. */
class CarPlayRxSlotsTest {
    private fun rec(t: Long = 0L) = Incumbent(Socket(), null, 0, t)

    // ---- F040 ------------------------------------------------------------------------------------

    /** The audit interleaving, replayed step by step: A's exit lands after B's install. */
    @Test fun f040_exitAfterInstall_leavesNewcomerInstalled() {
        val slot = IncumbentSlot()
        val a = rec(); val b = rec()
        assertNull(slot.install(a))
        assertSame(a, slot.install(b))       // accept B (the hijack target is A)
        assertFalse(slot.release(a))         // A's finally: must NOT blank B
        assertSame(b, slot.get())
        // B's later real loss: running, slot still names B -> not ours -> LINK_LOST eligible.
        assertFalse(slot.endedByUs(true, b))
        // stop() finds B to close.
        assertSame(b, slot.clear())
        assertNull(slot.get())
    }

    /** What the old `if (cur === c) cur = null` did in the same interleaving (documents the bug). */
    @Test fun f040_oldCheckThenWrite_blanksNewcomer() {
        var cur: Any? = null
        val a = Any(); val b = Any()
        cur = a
        val sawA = cur === a                  // A's finally: check ...
        cur = b                               // ... accept installs B in between ...
        if (sawA) cur = null                  // ... A's write
        assertNull(cur)                       // B live, slot empty: the F040 defect
    }

    @Test fun f040_endedByUs_rows() {
        val slot = IncumbentSlot()
        val a = rec(); val b = rec()
        slot.install(a)
        assertFalse(slot.endedByUs(true, a))  // own slot, running: real loss
        assertTrue(slot.endedByUs(false, a))  // stop(): running=false first
        slot.install(b)
        assertTrue(slot.endedByUs(true, a))   // hijacked
        assertTrue(slot.release(b))           // own exit clears own record
        assertNull(slot.get())
        assertFalse(slot.release(b))          // idempotent
    }

    /** Racing exits and installs: the last installer that has not exited is always the one installed. */
    @Test fun f040_stress_exitNeverBlanksLiveRecord() {
        repeat(20000) {
            val slot = IncumbentSlot()
            val a = rec(); val b = rec()
            slot.install(a)
            val go = CyclicBarrier(2)
            val t1 = Thread { go.await(); slot.release(a) }
            val t2 = Thread { go.await(); slot.install(b) }
            t1.start(); t2.start(); t1.join(); t2.join()
            assertSame(b, slot.get())
        }
    }

    @Test fun f040_recordFields() {
        val r = Incumbent(Socket(), InetAddress.getByName("fe80::1%1"), 1, 42L)
        assertEquals(42L, r.lastRxAt)
        assertEquals(0L, r.encryptedAt)
        assertEquals(1, r.scopeId)
        val v4 = Incumbent.of(Socket(), 7L)   // unconnected: no peer
        assertNull(v4.peer)
        assertEquals(0, v4.scopeId)
    }

    // ---- F039 ------------------------------------------------------------------------------------

    @Test fun f039_startAfterStop_isRefused() {
        val running = AtomicBoolean(true)
        val active = AtomicInteger(0)
        val slot = BrowseSlot<Any> { running.get() }
        assertTrue(slot.start(Any()) { active.incrementAndGet() })
        slot.stop { active.decrementAndGet() }       // rediscover drop
        running.set(false)                            // stop(): running=false ...
        slot.stop { active.decrementAndGet() }       // ... then read-stop-null (nothing installed)
        assertFalse(slot.start(Any()) { active.incrementAndGet() })   // the loop's late restart
        assertEquals(0, active.get())
    }

    /** stop() racing the rediscover loop's drop-then-restart: no listener may outlive stop(). */
    @Test fun f039_stress_noBrowseOutlivesStop() {
        repeat(5000) {
            val running = AtomicBoolean(true)
            val active = AtomicInteger(0)
            val slot = BrowseSlot<Any> { running.get() }
            slot.start(Any()) { active.incrementAndGet() }
            val go = CountDownLatch(1)
            val loop = Thread {
                go.await()
                repeat(4) {
                    slot.stop { active.decrementAndGet() }
                    slot.start(Any()) { active.incrementAndGet() }
                }
            }
            val stopper = Thread {
                go.await()
                running.set(false)
                slot.stop { active.decrementAndGet() }
            }
            loop.start(); stopper.start(); go.countDown(); loop.join(); stopper.join()
            assertEquals("browse leaked after stop()", 0, active.get())
        }
    }
}
