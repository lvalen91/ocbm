package com.carlink

import com.carlink.ocbm.HostInstanceNonce
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.yield
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The clean-slate reset's sequencing, headless: the ORDER of stop / nonce rotation / wait /
 * reconnect, the wait value, the stage reports, single-flight, and that a failed reconnect still
 * closes the sequence. The wait's derivation is in [CleanSlateReset]'s KDoc; this pins the number
 * so a casual edit cannot silently shrink it below the box's own teardown settle.
 */
class CleanSlateResetTest {
    private class RecordingSteps(
        private val reconnectFails: Boolean = false,
    ) : CleanSlateReset.Steps {
        val events = mutableListOf<String>()
        private var nonce = 0x1234

        override fun stopSession() {
            events += "stop"
        }

        override fun rotateInstanceNonce(): Int {
            nonce = HostInstanceNonce.fresh(previous = nonce)
            events += "rotate"
            return nonce
        }

        override suspend fun reconnect() {
            events += "reconnect"
            if (reconnectFails) error("adapter not found")
        }

        override fun onStage(stage: CleanSlateReset.Stage) {
            events += "stage:$stage"
        }
    }

    @Test
    fun `stop, rotate, wait, reconnect - in that order, with the stages announced around them`() =
        runBlocking {
            val steps = RecordingSteps()
            val waits = mutableListOf<Long>()
            val reset = CleanSlateReset(steps, wait = { waits += it })

            assertTrue(reset.run())

            assertEquals(
                listOf(
                    "stage:STOPPING",
                    "stop",
                    "rotate",
                    "stage:WAITING_FOR_ADAPTER",
                    "stage:RECONNECTING",
                    "reconnect",
                    "stage:DONE",
                ),
                steps.events,
            )
            assertEquals(listOf(CleanSlateReset.BOX_TEARDOWN_SETTLE_MS), waits)
            assertFalse(reset.isInFlight)
        }

    @Test
    fun `the wait covers the box's own teardown settle`() {
        // 1 s supervisor poll + the supervisor's 4 s post-teardown deferral + 1 s margin
        // (tools/session_supervisor.sh:1461, :1190). Anything shorter re-SUBSCRIBEs into a
        // detached wireless_down still running.
        assertEquals(6_000L, CleanSlateReset.BOX_TEARDOWN_SETTLE_MS)
        assertTrue(CleanSlateReset.BOX_TEARDOWN_SETTLE_MS >= 1_000L + 4_000L)
    }

    @Test
    fun `the nonce is rotated exactly once, after stop and before reconnect`() =
        runBlocking {
            val steps = RecordingSteps()
            CleanSlateReset(steps, wait = {}).run()
            assertEquals(1, steps.events.count { it == "rotate" })
            assertTrue(steps.events.indexOf("stop") < steps.events.indexOf("rotate"))
            assertTrue(steps.events.indexOf("rotate") < steps.events.indexOf("reconnect"))
        }

    @Test
    fun `a failed reconnect still reports DONE and clears the in-flight latch`() =
        runBlocking {
            val steps = RecordingSteps(reconnectFails = true)
            val reset = CleanSlateReset(steps, wait = {})
            val thrown = runCatching { reset.run() }.exceptionOrNull()
            assertNotEquals(null, thrown)
            assertEquals("stage:DONE", steps.events.last())
            assertFalse(reset.isInFlight)
        }

    @Test
    fun `a second run while one is waiting is refused and does nothing`() =
        runBlocking {
            val steps = RecordingSteps()
            val gate = CompletableDeferred<Unit>()
            val reset = CleanSlateReset(steps, wait = { gate.await() })

            val first = async { reset.run() }
            while (!reset.isInFlight) yield()
            assertFalse(reset.run())
            assertEquals(1, steps.events.count { it == "stop" })

            gate.complete(Unit)
            assertTrue(first.await())
            assertEquals(1, steps.events.count { it == "reconnect" })
        }

    @Test
    fun `a fresh nonce is never zero and never the value it replaces`() {
        repeat(1_000) {
            val prev = HostInstanceNonce.fresh()
            val next = HostInstanceNonce.fresh(previous = prev)
            assertNotEquals(0, prev)
            assertNotEquals(0, next)
            assertNotEquals(prev, next)
        }
    }
}
