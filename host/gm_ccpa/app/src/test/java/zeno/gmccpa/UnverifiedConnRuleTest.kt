package zeno.gmccpa

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** A-F1 (2026-10-08): the unverified-connection deadline and the reason it closes with. Host JVM. */
class UnverifiedConnRuleTest {

    @Test fun overdueOnlyWhenPlaintextAndPastTheDeadline() {
        val t0 = 1_000_000L
        assertFalse("just accepted", UnverifiedConnRule.overdue(false, t0, t0, 10_000L))
        assertFalse("inside the budget", UnverifiedConnRule.overdue(false, t0, t0 + 9_999L, 10_000L))
        assertTrue("at the deadline", UnverifiedConnRule.overdue(false, t0, t0 + 10_000L, 10_000L))
        assertTrue("well past it (a peer that keeps sending plaintext)", UnverifiedConnRule.overdue(false, t0, t0 + 60_000L, 10_000L))
        assertFalse("encrypted: the deadline no longer applies, however old", UnverifiedConnRule.overdue(true, t0, t0 + 60_000L, 10_000L))
    }

    @Test fun deadlineCloseIsNeitherClosedNorLinkLost() {
        val r = UnverifiedConnRule.reason()
        assertEquals(EndReason.UNVERIFIED, r)
        // LINK_LOST is the grace ladder's fast entry (rung-1 CT_RADIO off/on): never for a rogue.
        assertFalse(r == EndReason.LINK_LOST)
        assertFalse(r == EndReason.CLOSED)
    }

    @Test fun onlyADeadlineCloseWithNoVerifiedSessionIsKeptFromTheSupervisor() {
        // Never verified, closed at the deadline: not notified (no GRACE, no ladder reset).
        assertFalse(UnverifiedConnRule.reachesSupervisor(EndReason.UNVERIFIED, verifiedSessionWasUp = false))
        // C1: a verified gen1 superseded in the release/decrement gap by a gen2 that then timed out —
        // gen2's UNVERIFIED end is the only session-down gen1's SESSION_UP will ever get: notified.
        assertTrue(UnverifiedConnRule.reachesSupervisor(EndReason.UNVERIFIED, verifiedSessionWasUp = true))
        // A real phone that closes early on its own keeps today's GRACE (docs/04 §4b case 2).
        assertTrue(UnverifiedConnRule.reachesSupervisor(EndReason.CLOSED, verifiedSessionWasUp = false))
        assertTrue(UnverifiedConnRule.reachesSupervisor(EndReason.CLOSED, verifiedSessionWasUp = true))
        assertTrue(UnverifiedConnRule.reachesSupervisor(EndReason.LINK_LOST, verifiedSessionWasUp = false))
    }
}
