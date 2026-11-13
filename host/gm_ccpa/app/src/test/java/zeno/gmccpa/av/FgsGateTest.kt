package zeno.gmccpa.av

import java.util.ArrayDeque
import java.util.Random
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * Plan v5.7.1 B1-1 (audit12 F094/F095): the FGS start/stop gate against a model of the one AMS rule
 * that kills the process — a stop (stopService or stopSelf) while any startForegroundService has not
 * yet reached its startForeground (SERVICE_FOREGROUND_CRASH_MSG, android-32 ActiveServices.java:5279-5286).
 *
 * The model, [Ams]: every startForegroundService adds one outstanding obligation and queues one
 * onStartCommand (delivered FIFO on "main"); startForeground clears one; a stop with any obligation
 * outstanding is a crash. stopSelf(startId) is ignored unless startId is the newest issued (AOSP).
 */
class FgsGateTest {

    private class Ams {
        var outstanding = 0          // startForegroundService calls not yet through startForeground
        var running = false
        var crashed = false
        var stopServiceCalls = 0
        var stopSelfCalls = 0
        private var nextStartId = 0
        val queue = ArrayDeque<Int>() // startIds of onStartCommands not yet delivered

        fun startForegroundService() { outstanding++; running = true; queue.addLast(++nextStartId) }
        fun startForeground() { check(outstanding > 0); outstanding-- }
        fun stopService() { stopServiceCalls++; if (outstanding > 0) crashed = true; running = false }
        fun stopSelf(startId: Int) {
            stopSelfCalls++
            if (startId != nextStartId) return   // stale startId: AMS ignores it
            if (outstanding > 0) crashed = true
            running = false
        }
    }

    private val ams = Ams()
    private val gate = FgsGate()

    private fun start(t: Long) = gate.start(t) { ams.startForegroundService() }
    private fun stop(t: Long) = gate.stop(t) { ams.stopService() }

    /** onStartCommand as the service runs it: startForeground FIRST, then the gate. */
    private fun onStartCommand(): FgsGate.Started {
        val id = ams.queue.removeFirst()
        ams.startForeground()
        return gate.onStarted { ams.stopSelf(id) }
    }

    private fun assertAlive() = assertFalse("AMS would have killed the process", ams.crashed)

    @Test fun stopBeforeOnStartCommandIsDeferredAndOnStartCommandStops() {
        start(1)
        assertEquals(FgsGate.Stop.DEFERRED, stop(1))
        assertEquals(0, ams.stopServiceCalls)
        assertEquals(FgsGate.Started.STOPPED, onStartCommand())
        assertEquals(1, ams.stopSelfCalls)
        assertFalse(ams.running); assertAlive()
    }

    @Test fun stopAfterOnStartCommandStops() {
        start(1)
        assertEquals(FgsGate.Started.STAYS, onStartCommand())
        assertEquals(FgsGate.Stop.STOPPED, stop(1))
        assertEquals(1, ams.stopServiceCalls)
        assertFalse(ams.running); assertAlive()
    }

    @Test fun anotherHolderKeepsTheService() {
        start(1); start(2)
        assertEquals(FgsGate.Stop.HELD, stop(1))
        assertEquals(0, ams.stopServiceCalls)
        assertEquals(FgsGate.Started.STAYS, onStartCommand())
        assertEquals(FgsGate.Started.STAYS, onStartCommand())
        assertTrue(ams.running); assertAlive()
    }

    @Test fun deferredStopThenNewStartStays() {
        start(1)
        assertEquals(FgsGate.Stop.DEFERRED, stop(1))
        start(2)
        assertEquals(FgsGate.Started.STAYS, onStartCommand())   // A's
        assertEquals(FgsGate.Started.STAYS, onStartCommand())   // B's
        assertEquals(0, ams.stopServiceCalls + ams.stopSelfCalls)
        assertTrue(ams.running)
        assertEquals(FgsGate.Stop.STOPPED, stop(2))
        assertFalse(ams.running); assertAlive()
    }

    /** v5.7.1, the crash window. The gate has no onDestroy hook, so a late old-instance onDestroy
     *  anywhere in this sequence cannot change any decision (asserted by construction). */
    @Test fun crashWindowNewStartStopBeforeItsOnStartCommand() {
        start(1)
        assertEquals(FgsGate.Started.STAYS, onStartCommand())
        assertEquals(FgsGate.Stop.STOPPED, stop(1))
        assertEquals(1, ams.stopServiceCalls)
        start(2)
        assertEquals(FgsGate.Stop.DEFERRED, stop(2))   // the v5.7 flag design called stopService here
        assertEquals(1, ams.stopServiceCalls)
        assertAlive()
        assertEquals(FgsGate.Started.STOPPED, onStartCommand())
        assertFalse(ams.running); assertAlive()
    }

    @Test fun twoStartsNoStopUntilTheSecondOnStartCommand() {
        start(1); start(2)
        assertEquals(FgsGate.Started.STAYS, onStartCommand())
        assertEquals(FgsGate.Stop.HELD, stop(1))
        assertEquals(FgsGate.Stop.DEFERRED, stop(2))
        assertEquals(0, ams.stopServiceCalls + ams.stopSelfCalls)
        assertEquals(FgsGate.Started.STOPPED, onStartCommand())
        assertFalse(ams.running); assertAlive()
    }

    @Test fun startThatThrowsUnregistersAndCountsNothing() {
        try {
            gate.start(1) { throw IllegalStateException("not allowed") }
            fail("expected the throw to propagate")
        } catch (e: IllegalStateException) { /* expected */ }
        assertEquals(0, gate.holderCount())
        assertEquals(0, gate.pending())
        assertEquals(FgsGate.Stop.STOPPED, stop(1))   // harmless: nothing is pending
        assertAlive()
    }

    @Test fun unrequestedOnStartCommandIsClampedAndStopsWhenUnheld() {
        assertEquals(FgsGate.Started.UNREQUESTED_STOPPED, gate.onStarted { })
        assertEquals(0, gate.pending())
        start(1)
        assertEquals(FgsGate.Started.STAYS, onStartCommand())
        assertEquals(FgsGate.Started.UNREQUESTED_STAYS, gate.onStarted { fail("held: must not stop") })
    }

    /**
     * Random interleavings of N sessions (each: start, then stop, from any thread) with main delivering
     * onStartCommands in order, its startForeground and its gate step as SEPARATE steps (only the gate
     * step is under the lock in production). Invariants: AMS never crashes; once every session has
     * stopped and every onStartCommand ran, the service is not running (no orphan).
     */
    @Test fun randomInterleavingsNeverCrashAndNeverOrphan() {
        repeat(20_000) { seed ->
            val rnd = Random(seed.toLong())
            val ams = Ams()
            val gate = FgsGate()
            val n = 1 + rnd.nextInt(4)
            val state = IntArray(n)           // 0 = not started, 1 = started, 2 = stopped
            var midOnStart: Int? = null       // startId whose startForeground ran but whose gate step has not
            val trace = StringBuilder()
            while (true) {
                val moves = ArrayList<Int>()
                for (i in 0 until n) if (state[i] < 2) moves.add(i)
                if (midOnStart != null) moves.add(-2) else if (ams.queue.isNotEmpty()) moves.add(-1)
                if (moves.isEmpty()) break
                when (val m = moves[rnd.nextInt(moves.size)]) {
                    -1 -> { val id = ams.queue.removeFirst(); ams.startForeground(); midOnStart = id; trace.append(" fg$id") }
                    -2 -> { val id = midOnStart!!; midOnStart = null; gate.onStarted { ams.stopSelf(id) }; trace.append(" osc$id") }
                    else -> {
                        val t = m.toLong() + 1
                        if (state[m] == 0) { gate.start(t) { ams.startForegroundService() }; trace.append(" start$t") }
                        else { gate.stop(t) { ams.stopService() }; trace.append(" stop$t") }
                        state[m]++
                    }
                }
                if (ams.crashed) fail("seed $seed: AMS crash after$trace")
            }
            if (ams.running) fail("seed $seed: orphan service after$trace")
            assertEquals(0, gate.pending())
            assertEquals(0, gate.holderCount())
        }
    }

    // ---- plan v5.9 D6: the LINK token, holds(), and the notification orphan guard ---------------------

    @Test fun d6_linkTokenIsNegativeAndSessionTokensArePositive() {
        assertEquals(-1L, FgsTokens.LINK_TOKEN)
        repeat(3) { assertTrue(FgsTokens.newToken() > 0) }
    }

    @Test fun d6_linkPlusSessionThenSessionStopIsHeld() {
        start(FgsTokens.LINK_TOKEN); onStartCommand()
        start(7L); onStartCommand()
        assertEquals(FgsGate.Stop.HELD, stop(7L))
        assertTrue(gate.holds(FgsTokens.LINK_TOKEN))
        assertFalse(gate.holds(7L))
        assertEquals(FgsGate.Stop.STOPPED, stop(FgsTokens.LINK_TOKEN))
        assertFalse(ams.running); assertAlive()
    }

    @Test fun d6_doubleLinkStartIsOneHolder() {
        start(FgsTokens.LINK_TOKEN); start(FgsTokens.LINK_TOKEN)
        assertEquals(1, gate.holderCount())
        onStartCommand(); onStartCommand()
        assertEquals(FgsGate.Stop.STOPPED, stop(FgsTokens.LINK_TOKEN))
        assertAlive()
    }

    @Test fun d6_postedFlagFollowsTheService() {
        assertFalse(gate.whenPosted { fail("not started: nothing is posted") })
        start(FgsTokens.LINK_TOKEN)
        assertFalse("between start and startForeground the flag is not set", gate.whenPosted { })
        onStartCommand()
        assertTrue(gate.whenPosted { })
        stop(FgsTokens.LINK_TOKEN)
        assertFalse("cleared before stopService", gate.whenPosted { fail("posted after STOPPED") })
    }

    @Test fun d6_deferredStopClearsTheFlagOnTheStopSelfPath() {
        start(FgsTokens.LINK_TOKEN)
        assertEquals(FgsGate.Stop.DEFERRED, stop(FgsTokens.LINK_TOKEN))
        var stays = 0
        val id = ams.queue.removeFirst(); ams.startForeground()
        assertEquals(FgsGate.Started.STOPPED, gate.onStarted(onStays = { stays++ }) { ams.stopSelf(id) })
        assertEquals(0, stays)
        assertFalse(gate.whenPosted { fail("posted after the deferred stop") })
    }
}
