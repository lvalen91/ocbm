package com.carlink.av

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AudioFocusPolicyTest {
    @Test
    fun `media gain is the minimum of focus and duck, never a product`() {
        assertEquals(1.0f, MediaGain.effective(1.0f, 1.0f))
        assertEquals(0.0f, MediaGain.effective(0.0f, 1.0f)) // transient loss holds media silent
        assertEquals(0.2f, MediaGain.effective(1.0f, 0.2f)) // software duck alone
        assertEquals(0.2f, MediaGain.effective(0.2f, 0.2f)) // nav: both duck the same event -> 0.2, not 0.04
        assertEquals(1.0f, MediaGain.effective(1.0f, 1.0f))
        assertEquals(1.0f, MediaGain.effective(2.0f, 5.0f)) // clamped
    }

    @Test
    fun `a duck restore while focus is still lost stays silent, and a focus regain makes it audible`() {
        // The 2026-09-25 bug shape: duck said 1.0, focus said 0.0 -> must be 0.0 and visible as such.
        assertEquals(0.0f, MediaGain.effective(0.0f, 1.0f))
        assertEquals(1.0f, MediaGain.effective(1.0f, 1.0f))
    }

    @Test
    fun `uplink off releases once after the grace and never before`() {
        val p = TransientFocusPolicy(uplinkGraceMs = 1_000L)
        assertFalse("nothing pending", p.uplinkReleaseDue(now = 5_000L))
        p.uplinkChanged(on = false, now = 10_000L)
        assertFalse(p.uplinkReleaseDue(now = 10_500L))
        assertFalse(p.uplinkReleaseDue(now = 10_999L))
        assertTrue(p.uplinkReleaseDue(now = 11_000L))
        assertFalse("fires exactly once", p.uplinkReleaseDue(now = 11_100L))
    }

    @Test
    fun `an uplink re-open inside the grace cancels the release so the held focus is reused`() {
        val p = TransientFocusPolicy(uplinkGraceMs = 1_000L)
        p.uplinkChanged(on = false, now = 10_000L)
        p.uplinkChanged(on = true, now = 10_400L) // Siri follow-up inside the window
        assertFalse(p.uplinkReleaseDue(now = 12_000L))
        p.uplinkChanged(on = false, now = 13_000L)
        assertTrue(p.uplinkReleaseDue(now = 14_000L))
    }

    @Test
    fun `uplinkOn mirrors the gate so a quiet assistant is not released while Siri still listens`() {
        val p = TransientFocusPolicy()
        assertFalse(p.uplinkOn)
        p.uplinkChanged(on = true, now = 1L)
        assertTrue(p.uplinkOn)
        p.uplinkChanged(on = false, now = 2L)
        assertFalse(p.uplinkOn)
    }

    @Test
    fun `the measured back-to-back turn gap is outside the grace, so it is a fresh session`() {
        // UPLINK OFF 02:17:34.08 -> next Siri turn 02:17:41: ~7 s. The grace must not span that.
        val p = TransientFocusPolicy()
        p.uplinkChanged(on = false, now = 34_080L)
        assertTrue(p.uplinkReleaseDue(now = 34_080L + TransientFocusPolicy.UPLINK_GRACE_MS))
        assertTrue(TransientFocusPolicy.UPLINK_GRACE_MS < 7_000L)
        // Media resumed 0.9 s after UPLINK OFF; the media-audible trigger covers that case regardless.
        assertTrue(TransientFocusPolicy.MEDIA_RESUME_MIN_QUIET_MS < 900L)
    }
}
