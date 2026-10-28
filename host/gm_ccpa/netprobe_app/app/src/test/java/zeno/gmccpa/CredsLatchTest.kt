package zeno.gmccpa

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Wave 3 fix 1, OD-7A: the refused-start latch SessionCore drives. Host JVM, no Android. */
class CredsLatchTest {
    @Test fun refusedStart_saveWithCreds_resumesExactlyOnce() {
        val l = CredsLatch()
        l.block()
        assertTrue(l.resumeOnSave(credsMissing = false))
        assertFalse(l.resumeOnSave(credsMissing = false))
        assertFalse(l.isBlocked)
    }

    @Test fun saveWithBlankCreds_doesNotResume_andStaysBlocked() {
        val l = CredsLatch()
        l.block()
        assertFalse(l.resumeOnSave(credsMissing = true))
        assertTrue(l.isBlocked)
        assertTrue(l.resumeOnSave(credsMissing = false))
    }

    @Test fun saveWithNothingPending_doesNotStart() = assertFalse(CredsLatch().resumeOnSave(credsMissing = false))

    @Test fun stopWithdrawsThePendingStart() {
        val l = CredsLatch()
        l.block()
        l.clear()   // requestStop / stopEverythingLocked
        assertFalse(l.resumeOnSave(credsMissing = false))
        assertFalse(l.repromptOnAttach(credsMissing = true))
    }

    @Test fun attach_repromptsOnlyWhilePendingAndStillMissing() {
        val l = CredsLatch()
        assertFalse(l.repromptOnAttach(credsMissing = true))
        l.block()
        assertTrue(l.repromptOnAttach(credsMissing = true))
        assertFalse(l.repromptOnAttach(credsMissing = false))
        assertTrue(l.isBlocked)   // a re-prompt does not consume the pending start
    }

    @Test fun startPastTheGate_clears() {
        val l = CredsLatch()
        l.block()
        l.clear()   // autoStart got past credsMissing()
        assertFalse(l.isBlocked)
    }
}
