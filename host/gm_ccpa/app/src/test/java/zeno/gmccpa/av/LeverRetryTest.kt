package zeno.gmccpa.av

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Wave 3, OD-7D / F100: the vehicle-lever retry schedule ([LeverRetry]) and the persister state
 * machine ([LeverChains]) that VehicleStateWatcher.reassert runs. Host JVM, no Android. Every test
 * below drives the production classes; nothing re-implements the watcher's bookkeeping.
 */
class LeverRetryTest {
    // ---- LeverRetry: the pure schedule --------------------------------------------------------------

    @Test fun schedule_fastRetries_thenGiveUp_thenPersist() {
        for (a in 0 until LeverRetry.RETRY_MAX) {
            val s = LeverRetry.afterRefusal(a, isPersister = false, persisterLive = false)
            assertEquals(LeverRetry.Step.RETRY, s)
            assertEquals(400L, LeverRetry.delayFor(s))
        }
        val g = LeverRetry.afterRefusal(LeverRetry.RETRY_MAX, isPersister = false, persisterLive = false)
        assertEquals(LeverRetry.Step.GIVE_UP_AND_PERSIST, g)
        assertEquals(2_000L, LeverRetry.delayFor(g))
        assertEquals(LeverRetry.Step.PERSIST, LeverRetry.afterRefusal(99, isPersister = true, persisterLive = true))
        assertEquals(LeverRetry.Step.COVERED, LeverRetry.afterRefusal(LeverRetry.RETRY_MAX, isPersister = false, persisterLive = true))
        assertNull(LeverRetry.delayFor(LeverRetry.Step.COVERED))
    }

    @Test fun deliveredAfterSeconds() {
        assertEquals(0L, LeverRetry.deliveredAfterS(1_000L, 1_999L))
        assertEquals(30L, LeverRetry.deliveredAfterS(0L, 30_999L))
        assertEquals(0L, LeverRetry.deliveredAfterS(5_000L, 4_000L))
    }

    // ---- LeverChains: the persister state machine -----------------------------------------------------

    /** Refuse attempts 0..RETRY_MAX of one chain; returns the token it holds afterwards. */
    private fun exhaust(ch: LeverChains, gen: Int, startedAt: Long): LeverChains.Next {
        var token = 0L
        for (a in 0 until LeverRetry.RETRY_MAX) {
            val n = ch.onRefused(a, gen, token, startedAt)
            assertEquals(LeverRetry.Step.RETRY, n.step)
            assertFalse(n.logGiveUp)
            assertEquals(0L, n.token)
            token = n.token
        }
        return ch.onRefused(LeverRetry.RETRY_MAX, gen, token, startedAt)
    }

    @Test fun giveUpOnce_becomesPersister_ticksEvery2s_noSecondELine() {
        val ch = LeverChains()
        val g = exhaust(ch, gen = 1, startedAt = 0L)
        assertTrue(g.logGiveUp)
        assertNotEquals(0L, g.token)
        assertEquals(2_000L, g.delayMs)
        assertTrue(ch.shouldRun(true, false, 1, 1, g.token))
        for (a in LeverRetry.RETRY_MAX + 1..LeverRetry.RETRY_MAX + 30) {
            val n = ch.onRefused(a, 1, g.token, 0L)
            assertEquals(LeverRetry.Step.PERSIST, n.step)
            assertFalse(n.logGiveUp)
            assertEquals(g.token, n.token)
            assertEquals(2_000L, n.delayMs)
        }
    }

    @Test fun secondChain_exhausted_whilePersisterLive_isCovered() {
        val ch = LeverChains()
        exhaust(ch, 1, 0L)
        val b = exhaust(ch, 1, 3_000L)
        assertEquals(LeverRetry.Step.COVERED, b.step)
        assertFalse(b.logGiveUp)
        assertNull(b.delayMs)
        assertEquals(0L, b.token)
    }

    @Test fun delivery_endsPersister_onceWithSeconds_andItsNextTickStops() {
        val ch = LeverChains()
        val g = exhaust(ch, 1, startedAt = 10_000L)
        assertEquals(30L, ch.onDelivered(1, 40_500L))
        assertNull(ch.onDelivered(1, 41_000L))             // I line once
        assertFalse(ch.shouldRun(true, false, 1, 1, g.token)) // the old persister ends at its next tick
        assertTrue(ch.shouldRun(true, false, 1, 1, 0L))      // ordinary chains still run
    }

    @Test fun deliveryWithoutPersister_logsNothing() = assertNull(LeverChains().onDelivered(1, 5_000L))

    @Test fun afterDelivery_aLaterRefusalRound_getsAFreshPersisterAndOneMoreELine() {
        val ch = LeverChains()
        val g1 = exhaust(ch, 1, 0L)
        ch.onDelivered(1, 1L)
        val g2 = exhaust(ch, 1, 50_000L)
        assertTrue(g2.logGiveUp)
        assertNotEquals(g1.token, g2.token)
        assertFalse(ch.shouldRun(true, false, 1, 1, g1.token))
        assertTrue(ch.shouldRun(true, false, 1, 1, g2.token))
    }

    @Test fun sessionChange_endsChains_andANewSessionGetsItsOwnPersister() {
        val ch = LeverChains()
        val g1 = exhaust(ch, 1, 0L)
        // onSessionDown/onSessionUp bumped the generation to 3: the old persister and old chains end.
        assertFalse(ch.shouldRun(true, false, 1, 3, g1.token))
        assertFalse(ch.shouldRun(true, false, 1, 3, 0L))
        // The stale persister of gen 1 does not cover gen 3, and gen 3's delivery says nothing about it.
        val g3 = exhaust(ch, 3, 100_000L)
        assertTrue(g3.logGiveUp)
        assertNotEquals(g1.token, g3.token)
        assertEquals(1L, ch.onDelivered(3, 101_000L))
    }

    @Test fun staleGenPersister_deliveryInNewGen_isNotReported() {
        val ch = LeverChains()
        exhaust(ch, 1, 0L)
        assertNull(ch.onDelivered(2, 9_000L))
    }

    @Test fun stoppedOrSessionDown_endsEveryChain() {
        val ch = LeverChains()
        val g = exhaust(ch, 1, 0L)
        assertFalse(ch.shouldRun(true, true, 1, 1, g.token))
        assertFalse(ch.shouldRun(false, false, 1, 1, g.token))
        assertFalse(ch.shouldRun(true, true, 1, 1, 0L))
        assertFalse(ch.shouldRun(false, false, 1, 1, 0L))
    }

    /**
     * Two chains driven tick by tick through the production LeverChains on a fake clock, the way the
     * single cp-vehicle thread interleaves them: session-up at t=0, a gear change at t=3 s, an event
     * channel that refuses until [openAt]. Returns (E lines, I-line seconds or null, delivery time).
     */
    private fun drive(openAt: Long, sessionEndsAt: Long = Long.MAX_VALUE): Triple<Int, Long?, Long?> {
        data class Chain(var at: Long, var attempt: Int, var token: Long, val startedAt: Long)
        val ch = LeverChains()
        val chains = mutableListOf(Chain(0L, 0, 0L, 0L), Chain(3_000L, 0, 0L, 3_000L))
        var e = 0; var iSeconds: Long? = null; var deliveredAt: Long? = null; var guard = 0
        while (chains.isNotEmpty() && guard++ < 100_000) {
            val c = chains.minByOrNull { it.at }!!
            val now = c.at
            val up = now < sessionEndsAt
            if (!ch.shouldRun(up, false, 1, 1, c.token)) { chains.remove(c); continue }
            if (now >= openAt || deliveredAt != null) {
                if (deliveredAt == null) deliveredAt = now
                ch.onDelivered(1, now)?.let { iSeconds = it }
                chains.remove(c); continue
            }
            val n = ch.onRefused(c.attempt, 1, c.token, c.startedAt)
            if (n.logGiveUp) e++
            val d = n.delayMs
            if (d == null) { chains.remove(c); continue }
            c.at = now + d; c.attempt++; c.token = n.token
        }
        return Triple(e, iSeconds, deliveredAt)
    }

    @Test fun twoChains_channelOpensDuringFastRetries_noELine_noILine() {
        val (e, i, at) = drive(openAt = 1_000L)
        assertEquals(0, e); assertNull(i); assertEquals(1_200L, at)
    }

    @Test fun twoChains_channelOpensAt30s_oneELine_deliveredAfter30s() {
        val (e, i, at) = drive(openAt = 30_000L)
        assertEquals(1, e)
        assertEquals(30_000L, at)   // chain 1 persists from 8000 ms on 2 s ticks; chain 2 is covered at 11 s
        assertEquals(30L, i)
    }

    @Test fun twoChains_sessionEndsFirst_chainsStop_noDelivery() {
        val (e, i, at) = drive(openAt = Long.MAX_VALUE, sessionEndsAt = 60_000L)
        assertEquals(1, e); assertNull(i); assertNull(at)
    }
}
