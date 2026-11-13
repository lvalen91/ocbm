package zeno.gmccpa.av

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Plan v5.9 MED M5 — after a permanent focus loss, focus is re-requested only on an explicit resume
 * intent (user play / play-pause / a screen launch or return) or a phone-side resume after the loss
 * (not PLAYING at some point after it, PLAYING now); media merely flowing never re-requests.
 */
class FocusReclaimRuleTest {

    @Test fun truthTable() {
        // not a permanent loss: today's retry-while-flowing stands
        assertTrue(FocusReclaimRule.shouldReclaim(false, false, false, false))
        // permanent loss, nothing else: media flowing alone never reclaims
        assertFalse(FocusReclaimRule.shouldReclaim(true, false, false, false))
        assertFalse("still PLAYING, never paused: FM was picked", FocusReclaimRule.shouldReclaim(true, false, true, false))
        assertFalse("paused, not resumed", FocusReclaimRule.shouldReclaim(true, true, false, false))
        assertTrue("phone resumed", FocusReclaimRule.shouldReclaim(true, true, true, false))
        assertTrue("user intent", FocusReclaimRule.shouldReclaim(true, false, false, true))
    }

    @Test fun latch_fmPickedWhilePlaying_staysLost() {
        val l = FocusReclaimRule.Latch()
        l.onPhoneStatus(NowPlayingState.PLAY)
        l.onPermanentLoss()
        l.onPhoneStatus(NowPlayingState.PLAY)   // a repeat PLAY is not a resume
        assertFalse(l.shouldReclaim())
    }

    @Test fun latch_phonePausedThenPlayedAfterLoss_reclaims() {
        val l = FocusReclaimRule.Latch()
        l.onPhoneStatus(NowPlayingState.PLAY)
        l.onPermanentLoss()
        l.onPhoneStatus(NowPlayingState.PAUSE)
        assertFalse(l.shouldReclaim())
        l.onPhoneStatus(NowPlayingState.PLAY)
        assertTrue(l.shouldReclaim())
    }

    /**
     * rc2 review MINOR: a phone ALREADY paused when the loss lands sends no PAUSE edge afterwards
     * (NowPlayingState forwards status only on a change), so before the fix its later PLAY could never
     * reclaim and media stayed silent.
     */
    @Test fun latch_alreadyPausedAtLoss_thenPhonePlays_reclaims() {
        val l = FocusReclaimRule.Latch()
        l.onPhoneStatus(NowPlayingState.PAUSE)
        l.onPermanentLoss()
        assertFalse("still paused: nothing to reclaim for", l.shouldReclaim())
        l.onPhoneStatus(NowPlayingState.PLAY)
        assertTrue(l.shouldReclaim())
    }

    @Test fun latch_unknownStatusAtLoss_thenPhonePlays_reclaims() {
        val l = FocusReclaimRule.Latch()
        l.onPermanentLoss()   // no status ever seen: not PLAYING
        l.onPhoneStatus(NowPlayingState.PLAY)
        assertTrue(l.shouldReclaim())
    }

    @Test fun latch_playingAtLoss_thenStopped_thenPlays_reclaims() {
        val l = FocusReclaimRule.Latch()
        l.onPhoneStatus(NowPlayingState.PLAY)
        l.onPermanentLoss()
        l.onPhoneStatus(NowPlayingState.STOP)
        l.onPhoneStatus(NowPlayingState.PLAY)
        assertTrue(l.shouldReclaim())
    }

    /** The screen-return intent (CarPlayActivity.onResume -> AacPlayer.onScreenReturned) is a userIntent. */
    @Test fun latch_screenReturnAfterLoss_reclaims_untilTheNextLoss() {
        val l = FocusReclaimRule.Latch()
        l.onPhoneStatus(NowPlayingState.PLAY)
        l.onPermanentLoss()
        assertFalse(l.shouldReclaim())
        l.onUserIntent()
        assertTrue(l.shouldReclaim())
        l.onPermanentLoss()
        assertFalse("a fresh loss forgets the return", l.shouldReclaim())
    }

    @Test fun latch_userIntent_reclaims_andAFreshLossForgetsIt() {
        val l = FocusReclaimRule.Latch()
        l.onPermanentLoss()
        l.onUserIntent()
        assertTrue(l.shouldReclaim())
        l.onPermanentLoss()
        assertFalse(l.shouldReclaim())
    }

    @Test fun latch_focusHeld_resetsToNotLost() {
        val l = FocusReclaimRule.Latch()
        l.onPermanentLoss()
        assertFalse(l.shouldReclaim())
        l.onFocusHeld()
        assertTrue(l.shouldReclaim())
    }

    /** C-F1 (2026-10-08, variant B): a request REFUSED at start sets the hold; the feed gate retries it. */
    @Test fun feedGate_refusedAtStartRetriesWhileFlowing() {
        val l = FocusReclaimRule.Latch()
        // Refusal: pausedForFocus true, nothing held, no permanent loss.
        assertTrue(FocusReclaimRule.shouldRequestFromFeed(pausedForFocus = true, focusHeld = false, latch = l))
        // Granted later: the hold is lifted and a request is cached — no more retries.
        l.onFocusHeld()
        assertFalse(FocusReclaimRule.shouldRequestFromFeed(pausedForFocus = false, focusHeld = true, latch = l))
        // Not paused for focus (playing): never asks from feed.
        assertFalse(FocusReclaimRule.shouldRequestFromFeed(pausedForFocus = false, focusHeld = false, latch = l))
        // A permanent LOSS without a resume intent: held, nothing cached, but the M5 rule says no.
        l.onPhoneStatus(NowPlayingState.PLAY)
        l.onPermanentLoss()
        assertFalse(FocusReclaimRule.shouldRequestFromFeed(pausedForFocus = true, focusHeld = false, latch = l))
        l.onUserIntent()
        assertTrue(FocusReclaimRule.shouldRequestFromFeed(pausedForFocus = true, focusHeld = false, latch = l))
    }
}
