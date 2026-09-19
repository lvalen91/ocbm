package com.carlink.ui.adaptive

import androidx.window.core.layout.WindowSizeClass
import androidx.window.core.layout.computeWindowSizeClass
import org.junit.Assert.assertEquals
import org.junit.Test

/** The breakpoint matrix and the grid arithmetic, exercised for every panel the app is expected on. */
class WindowLayoutTest {
    private fun info(
        w: Int,
        h: Int,
    ) = WindowLayoutInfo(WindowSizeClass.BREAKPOINTS_V1.computeWindowSizeClass(w, h), w, h)

    private fun arrangement(
        pxW: Int,
        pxH: Int,
        dpi: Int,
    ) = DashboardLayout.arrangement(info(pxW * 160 / dpi, pxH * 160 / dpi))

    @Test
    fun `wide landscape panels get two panes at every density that keeps them expanded`() {
        assertEquals(DashboardArrangement.TWO_PANE, arrangement(2400, 960, 160)) // emulator
        assertEquals(DashboardArrangement.TWO_PANE, arrangement(2400, 960, 200)) // gminfo37: 1920x768 dp
        assertEquals(DashboardArrangement.TWO_PANE, arrangement(1920, 720, 160))
        assertEquals(DashboardArrangement.TWO_PANE, arrangement(1920, 720, 320)) // 960x360 dp: compact height, still two panes, scrolls
        assertEquals(DashboardArrangement.TWO_PANE, arrangement(1280, 720, 160))
        assertEquals(DashboardArrangement.TWO_PANE, arrangement(1024, 600, 160)) // 1024 dp: expanded
    }

    @Test
    fun `medium and compact widths stack`() {
        assertEquals(DashboardArrangement.STACKED, arrangement(1024, 600, 240)) // 682x400 dp
        assertEquals(DashboardArrangement.STACKED, arrangement(1280, 720, 320)) // 640x360 dp
        assertEquals(DashboardArrangement.STACKED, arrangement(800, 480, 160))
    }

    @Test
    fun `portrait always stacks, however wide`() {
        assertEquals(DashboardArrangement.STACKED, arrangement(800, 1280, 120)) // portrait AVD: 1066x1706 dp, expanded both axes
        assertEquals(DashboardArrangement.STACKED, arrangement(800, 1280, 160))
        assertEquals(DashboardArrangement.STACKED, arrangement(960, 2400, 160))
    }

    @Test
    fun `content block keeps the classic 70 percent on the target panels and widens below the floor`() {
        assertEquals(2368 * 0.7f, DashboardLayout.contentWidthDp(2368f), 0.01f) // 2400x960 @160 minus padding
        assertEquals(1888 * 0.7f, DashboardLayout.contentWidthDp(1888f), 0.01f) // gminfo37 dp width
        assertEquals(1100f, DashboardLayout.contentWidthDp(1248f), 0.01f) // 1280 wide: floor wins
        assertEquals(992f, DashboardLayout.contentWidthDp(992f), 0.01f) // 1024 wide: whole window
    }

    @Test
    fun `adapter column is clamped to the legible range`() {
        assertEquals(420f, DashboardLayout.adapterWidthDp(1658f), 0.01f)
        assertEquals(300f, DashboardLayout.adapterWidthDp(1100f), 0.01f) // 286 raw, floor wins
        assertEquals(1300 * 0.26f, DashboardLayout.adapterWidthDp(1300f), 0.01f)
        assertEquals(300f, DashboardLayout.adapterWidthDp(992f), 0.01f)
    }

    @Test
    fun `grid columns follow the available width`() {
        assertEquals(1, GridMath.columns(100, 200, 16))
        assertEquals(1, GridMath.columns(415, 200, 16))
        assertEquals(2, GridMath.columns(416, 200, 16))
        assertEquals(5, GridMath.columns(1230, 200, 16))
    }

    @Test
    fun `cells fill the row but are capped`() {
        assertEquals(200, GridMath.cellWidth(416, 2, 16, 280))
        assertEquals(280, GridMath.cellWidth(1230, 2, 16, 280))
        assertEquals(10, GridMath.cellWidth(10, 1, 16, 280))
    }
}
