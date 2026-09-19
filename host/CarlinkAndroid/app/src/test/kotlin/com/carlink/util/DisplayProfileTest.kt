package com.carlink.util

import com.carlink.ocbm.PanelRule
import com.carlink.ocbm.PixelRect
import com.carlink.ocbm.VehicleConfigSpec
import com.carlink.ocbm.ViewAreaRule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Launch-time display detection reduced to the config it produces. The emulator is 2400x960 with no
 * cutouts, so the cutout / waterfall / rounded-corner arms can only be proven here.
 */
class DisplayProfileTest {
    private fun panel(
        w: Int,
        h: Int,
        hz: Float = 60f,
    ) = DisplayProfile(w, h, hz, 160, 160f, 160f)

    @Test
    fun `the emulator panel is declared as-is with a full safe area`() {
        val p = panel(2400, 960, 60.000004f)
        assertEquals(2400, p.panelWidth)
        assertEquals(960, p.panelHeight)
        assertEquals(60, p.maxFps)
        assertFalse(p.panelClamped)
        assertEquals(PixelRect(0, 0, 2400, 960), p.safeArea)
        assertEquals(16.2, p.diagonalInches, 0.05)
    }

    @Test
    fun `a visible system bar comes off the window to give the content area, and the panel stays the panel`() {
        // Portrait AVD, SYSTEM_UI_VISIBLE: 800x1280 window on an 800x1280 panel, 48 px status bar
        // on top and a 96 px navigation bar at the bottom.
        val p =
            DisplayProfile(800, 1280, 60f, 120, 120f, 120f, physicalWidthPx = 800, physicalHeightPx = 1280)
                .contentAreaUnder(EdgeInsets(top = 48, bottom = 96))
        assertEquals(800, p.widthPx)
        assertEquals(1136, p.heightPx)
        assertEquals(800, p.windowWidthPx)
        assertEquals(1280, p.physicalHeightPx)
        assertEquals(EdgeInsets(top = 48, bottom = 96), p.barInsets)
        // What goes on the wire is the content area, legal as a portrait panel, full safe area.
        assertEquals(800, p.geometry.width)
        assertEquals(1136, p.geometry.height)
        assertEquals(PixelRect(0, 0, 800, 1136), p.safeArea)
        assertEquals(EdgeInsets.NONE, p.safeAreaInsets)
        assertEquals(p.barInsets, p.surfaceInsets)
        // The diagonal is the PANEL's, not the content area's.
        assertEquals(12.6, p.diagonalInches, 0.05)
    }

    @Test
    fun `an odd content area declares the even panel and the surface drops the parity pixel`() {
        // The portrait AVD's real bars: 57 px status, 72 px navigation -> 1151, declared 1150.
        val p =
            DisplayProfile(800, 1280, 60f, 120, 120f, 120f, physicalWidthPx = 800, physicalHeightPx = 1280)
                .contentAreaUnder(EdgeInsets(top = 57, bottom = 72))
        assertEquals(1151, p.heightPx)
        assertEquals(1150, p.panelHeight)
        assertEquals(EdgeInsets(top = 57, bottom = 73), p.surfaceInsets)
    }

    @Test
    fun `immersive keeps the whole window and a split-screen window is not the panel`() {
        val full = panel(2400, 960).contentAreaUnder(EdgeInsets.NONE)
        assertEquals(2400 to 960, full.widthPx to full.heightPx)
        val split = DisplayProfile(1200, 960, 60f, 160, 160f, 160f, physicalWidthPx = 2400, physicalHeightPx = 960).contentAreaUnder(EdgeInsets.NONE)
        assertEquals(1200, split.widthPx)
        assertEquals(1200, split.windowWidthPx)
        assertEquals(2400, split.physicalWidthPx)
        assertEquals(1200, split.geometry.width)
    }

    @Test
    fun `a cutout under a visible status bar is already outside the content area`() {
        val p =
            panel(2400, 960)
                .copy(cutout = EdgeInsets(top = 40), waterfall = EdgeInsets(left = 20))
                .contentAreaUnder(EdgeInsets(top = 60))
        assertEquals(EdgeInsets.NONE, p.cutout)
        assertEquals(EdgeInsets(left = 20), p.waterfall)
        assertEquals(900, p.heightPx)
        assertEquals(PixelRect(20, 0, 2380, 900), p.safeArea)
        assertEquals(EdgeInsets(left = 20), p.safeAreaInsets)
    }

    @Test
    fun `safe-area insets are the padding the app's own UI applies`() {
        val p = DisplayProfile(2400, 960, 60f, 160, 160f, 160f, cutout = EdgeInsets(top = 30, right = 10), cornerRadiusPx = 0)
        assertEquals(EdgeInsets(left = 0, top = 30, right = 10, bottom = 0), p.safeAreaInsets)
        val clamped = DisplayProfile(700, 400, 60f, 160, 160f, 160f, cutout = EdgeInsets(top = 30))
        assertTrue(clamped.panelClamped)
        assertEquals(EdgeInsets.NONE, clamped.safeAreaInsets)
    }

    @Test
    fun `fps snaps to the two rates the box honours`() {
        assertEquals(60, panel(2400, 960, 59.94f).maxFps)
        assertEquals(30, panel(2400, 960, 50f).maxFps)
        assertEquals(30, panel(2400, 960, 30f).maxFps)
        assertEquals(60, panel(2400, 960, 90f).maxFps)
    }

    @Test
    fun `odd panels are evened and the portrait AVD is a legal portrait panel`() {
        val p = panel(801, 1281)
        assertEquals(800, p.panelWidth)
        assertEquals(1280, p.panelHeight)
        assertNull(PanelRule.verdict(p.panelWidth, p.panelHeight))
        // 800x1280 portrait: the floor that applies is 480x800, not 800x480.
        assertEquals(480 to 800, ViewAreaRule.minimumSize(800, 1280))
    }

    @Test
    fun `a panel below the floor or above the ceiling is clamped, by its own orientation`() {
        val small = panel(720, 480)
        assertEquals(800, small.panelWidth)
        assertEquals(480, small.panelHeight)
        assertTrue(small.panelClamped)
        val tallSmall = panel(400, 700)
        assertEquals(480 to 800, tallSmall.panelWidth to tallSmall.panelHeight)
        val huge = panel(5120, 1440)
        assertEquals(3840 to 1440, huge.panelWidth to huge.panelHeight)
        // The clamp never flips an aspect and its result is always constructible.
        VehicleConfigSpec(width = small.panelWidth, height = small.panelHeight)
        VehicleConfigSpec(width = huge.panelWidth, height = huge.panelHeight)
    }

    @Test
    fun `cutouts and waterfall edges inset the safe area to even, contained bounds`() {
        val p = panel(2400, 960).copy(cutout = EdgeInsets(top = 33), waterfall = EdgeInsets(left = 21, right = 20))
        // origin rounds UP to even, far edges DOWN: 22..2380 x 34..960.
        assertEquals(PixelRect(22, 34, 2358, 926), p.safeArea)
        assertNull(ViewAreaRule.teardownVerdict(PixelRect(22, 34, 2358, 926), 2400, 960))
        VehicleConfigSpec(width = 2400, height = 960, safeOriginX = 22, safeOriginY = 34, safeWidth = 2358, safeHeight = 926)
    }

    @Test
    fun `rounded corners inset every edge by the arc, not the full radius`() {
        val p = panel(2400, 960).copy(cornerRadiusPx = 100)
        // ceil(100 * 0.2929) = 30 -> origin 30, far edge 2370 / 930.
        assertEquals(PixelRect(30, 30, 2340, 900), p.safeArea)
    }

    @Test
    fun `insets that leave less than the floor fall back to the full panel`() {
        val p = panel(900, 600).copy(cutout = EdgeInsets(left = 60, right = 60))
        // 780 wide < 800 landscape floor: a black lockout is worse than UI under a bezel.
        assertEquals(PixelRect(0, 0, 900, 600), p.safeArea)
        assertNotNull(ViewAreaRule.floorVerdict(780, 600))
    }

    @Test
    fun `the teardown rules refuse what iOS refuses`() {
        assertNotNull("containment", ViewAreaRule.teardownVerdict(PixelRect(492, 59, 1416, 842), 1416, 842))
        assertNotNull("odd width", ViewAreaRule.teardownVerdict(PixelRect(240, 760, 357, 400), 1080, 1920))
        assertNotNull("odd origin", ViewAreaRule.teardownVerdict(PixelRect(240, 761, 600, 400), 1080, 1920))
        assertNotNull("zero", ViewAreaRule.teardownVerdict(PixelRect(0, 0, 0, 400), 1080, 1920))
        assertNull("floating portrait area touching no edge is fine", ViewAreaRule.teardownVerdict(PixelRect(0, 160, 1080, 1600), 1080, 1920))
        assertNull(ViewAreaRule.floorVerdict(1080, 1600))
        var threw = false
        try {
            VehicleConfigSpec(width = 2400, height = 960, safeOriginX = 1, safeOriginY = 0)
        } catch (_: IllegalArgumentException) {
            threw = true
        }
        assertTrue("an odd safe-area origin is unconstructible", threw)
    }
}
