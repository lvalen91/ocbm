package zeno.gmccpa

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import zeno.gmccpa.AutoClaimPolicy.Adapter
import zeno.gmccpa.AutoClaimPolicy.Decision

/** plan v5.9 D6: the auto-claim's pure rules (AutoClaimPolicy.kt). */
class AutoClaimPolicyTest {

    /** An input that reaches the last row (CLAIM). */
    private val base = AutoClaimPolicy.Inputs(
        suppressed = false, stopRequested = false, restartInProgress = false, adapter = Adapter.PRESENT,
        relinkOwner = false, claimWaitActive = false, hasLink = false, sessionLive = false,
        credsMissing = false, flapTripped = false)

    private fun d(i: AutoClaimPolicy.Inputs, fg: Boolean = false) = AutoClaimPolicy.decide(i, fg)

    @Test fun everyRowAlone() {
        assertEquals(Decision.CLAIM, d(base))
        assertEquals(Decision.SKIP_STOPPED, d(base.copy(suppressed = true)))
        assertEquals(Decision.SKIP_STOPPED, d(base.copy(stopRequested = true)))
        assertEquals(Decision.YIELD_RESTART, d(base.copy(restartInProgress = true)))
        assertEquals(Decision.SKIP_ABSENT, d(base.copy(adapter = Adapter.ABSENT)))
        assertEquals(Decision.SKIP_WRONG_MODE, d(base.copy(adapter = Adapter.WRONG_MODE)))
        assertEquals(Decision.SKIP_NO_GRANT, d(base.copy(adapter = Adapter.NO_PERMISSION)))
        assertEquals(Decision.YIELD_CLAIM, d(base.copy(relinkOwner = true)))
        assertEquals(Decision.YIELD_CLAIM, d(base.copy(claimWaitActive = true)))
        assertEquals(Decision.NOOP, d(base.copy(hasLink = true)))
        assertEquals(Decision.RELINK_LIVE, d(base.copy(sessionLive = true)))
        assertEquals(Decision.BLOCK_CREDS, d(base.copy(credsMissing = true)))
        assertEquals(Decision.SKIP_FLAP, d(base.copy(flapTripped = true)))
    }

    /** First match wins: each row beats every row below it. */
    @Test fun precedence() {
        val all = base.copy(suppressed = true, stopRequested = true, restartInProgress = true, adapter = Adapter.ABSENT,
            relinkOwner = true, claimWaitActive = true, hasLink = true, sessionLive = true, credsMissing = true, flapTripped = true)
        var i = all
        val order = listOf(
            Decision.SKIP_STOPPED to { x: AutoClaimPolicy.Inputs -> x.copy(suppressed = false, stopRequested = false) },
            Decision.YIELD_RESTART to { x: AutoClaimPolicy.Inputs -> x.copy(restartInProgress = false) },
            Decision.SKIP_ABSENT to { x: AutoClaimPolicy.Inputs -> x.copy(adapter = Adapter.WRONG_MODE) },
            Decision.SKIP_WRONG_MODE to { x: AutoClaimPolicy.Inputs -> x.copy(adapter = Adapter.NO_PERMISSION) },
            Decision.SKIP_NO_GRANT to { x: AutoClaimPolicy.Inputs -> x.copy(adapter = Adapter.PRESENT) },
            Decision.YIELD_CLAIM to { x: AutoClaimPolicy.Inputs -> x.copy(relinkOwner = false, claimWaitActive = false) },
            Decision.NOOP to { x: AutoClaimPolicy.Inputs -> x.copy(hasLink = false) },
            Decision.RELINK_LIVE to { x: AutoClaimPolicy.Inputs -> x.copy(sessionLive = false) },
            Decision.BLOCK_CREDS to { x: AutoClaimPolicy.Inputs -> x.copy(credsMissing = false) },
            Decision.SKIP_FLAP to { x: AutoClaimPolicy.Inputs -> x.copy(flapTripped = false) },
        )
        for ((want, peel) in order) { assertEquals(want, d(i)); i = peel(i) }
        assertEquals(Decision.CLAIM, d(i))
    }

    /** The foreground (activity) route keeps its dialog and its ten-minute wait: no adapter/flap rows. */
    @Test fun foregroundRouteIgnoresAdapterAndFlapRows() {
        for (a in Adapter.values()) assertEquals("$a", Decision.CLAIM, d(base.copy(adapter = a), fg = true))
        assertEquals(Decision.CLAIM, d(base.copy(flapTripped = true), fg = true))
        assertEquals(Decision.SKIP_STOPPED, d(base.copy(suppressed = true), fg = true))
        assertEquals(Decision.NOOP, d(base.copy(hasLink = true), fg = true))
    }

    /** The no-grant row never requests: it is a SKIP, and nothing below it (CLAIM) is reached. */
    @Test fun noGrantNeverClaims() {
        for (s in listOf(true, false)) for (c in listOf(true, false))
            assertEquals(Decision.SKIP_NO_GRANT, d(base.copy(adapter = Adapter.NO_PERMISSION, sessionLive = s, credsMissing = c)))
    }

    /** rev c C1: release iff (acquired && !link && !relinkOwner), whatever the decision. */
    @Test fun releaseRuleForEveryDecision() {
        for (dec in Decision.values()) for (acq in listOf(true, false)) for (link in listOf(true, false)) for (rel in listOf(true, false)) {
            val want = acq && !link && !rel
            assertEquals("$dec acquired=$acq link=$link relink=$rel", want, AutoClaimPolicy.shouldRelease(acq, link, rel))
        }
        assertFalse("never release a hold this trigger did not add", AutoClaimPolicy.shouldRelease(false, false, false))
        assertFalse("keep while a relink owns the claim", AutoClaimPolicy.shouldRelease(true, false, true))
        assertTrue("BLOCK_CREDS / SKIP_FLAP / YIELD / stale NOOP with no link: released", AutoClaimPolicy.shouldRelease(true, false, false))
    }

    @Test fun flapGuardTripsAfterFiveDeadClaimsAndResets() {
        val g = AutoClaimPolicy.FlapGuard()
        repeat(4) { g.onClaimEnd(claimed = true, helloOk = false, enumerated = true) }
        assertFalse(g.tripped)
        g.onClaimEnd(claimed = false, helloOk = false, enumerated = true)      // not claimed: not a flap
        g.onClaimEnd(claimed = true, helloOk = false, enumerated = false)      // adapter left: not a flap
        assertFalse(g.tripped)
        g.onClaimEnd(claimed = true, helloOk = false, enumerated = true)
        assertTrue(g.tripped)
        assertEquals(Decision.SKIP_FLAP, d(base.copy(flapTripped = g.tripped)))
        g.reset()
        assertFalse(g.tripped)
        repeat(4) { g.onClaimEnd(true, false, true) }
        g.onClaimEnd(true, true, true)                                         // a good HELLO resets
        g.onClaimEnd(true, false, true)
        assertFalse(g.tripped)
        assertEquals(1, g.streak)
    }

    /** OD-9D same-boot rule (rev c C2). */
    @Test fun sameBootRule() {
        assertTrue("BOOT_COUNT equal", AutoClaimPolicy.sameBoot(12, "a", 12, "b"))
        assertFalse("BOOT_COUNT differs (boot_id ignored)", AutoClaimPolicy.sameBoot(12, "a", 13, "a"))
        assertTrue("BOOT_COUNT unreadable, boot_id equal", AutoClaimPolicy.sameBoot(-1, "a", 12, "a"))
        assertTrue("BOOT_COUNT unreadable now, boot_id equal", AutoClaimPolicy.sameBoot(12, "a", -1, "a"))
        assertFalse("boot_id differs", AutoClaimPolicy.sameBoot(-1, "a", -1, "b"))
        assertFalse("both unreadable at Stop", AutoClaimPolicy.sameBoot(-1, "", 5, "x"))
        assertFalse("both unreadable now", AutoClaimPolicy.sameBoot(5, "x", -1, ""))
        assertFalse("nothing readable", AutoClaimPolicy.sameBoot(-1, "", -1, ""))
        assertEquals("boot_count", AutoClaimPolicy.identityName(0, ""))
        assertEquals("boot_id", AutoClaimPolicy.identityName(-1, "x"))
        assertEquals("none", AutoClaimPolicy.identityName(-1, ""))
    }

    /** R10: view -> lastCreds -> prefs -> null; a blank non-null lastCreds shadows the prefs. */
    @Test fun resolveCreds() {
        val view = "view"; val last = "last"; val prefs = "prefs"
        assertEquals(view, AutoClaimPolicy.resolveCreds(view, last, prefs))
        assertEquals(last, AutoClaimPolicy.resolveCreds(null, last, prefs))
        assertEquals(prefs, AutoClaimPolicy.resolveCreds<String>(null, null, prefs))
        assertNull(AutoClaimPolicy.resolveCreds<String>(null, null, null))
        assertEquals("", AutoClaimPolicy.resolveCreds(null, "", prefs))
    }

    /** B-F3 (2026-10-08): the relink end uses the same rule as every other trigger end (C1). */
    @Test fun releaseRuleAtRelinkEnd() {
        // Wait expired, adapter still enumerated but ungranted/unlinked, no relink owns the claim: release.
        assertTrue("expired, enumerated, unlinked", AutoClaimPolicy.shouldRelease(true, false, false))
        // Link came back: keep.
        assertFalse("relinked", AutoClaimPolicy.shouldRelease(true, true, false))
        // A relink re-armed (a transport died again under the wait): keep for its wait.
        assertFalse("re-armed relink owns the claim", AutoClaimPolicy.shouldRelease(true, false, true))
    }
}
