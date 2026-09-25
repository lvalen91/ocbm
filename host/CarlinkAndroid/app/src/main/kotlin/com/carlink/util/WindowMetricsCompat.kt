package com.carlink.util

import android.app.Activity
import android.graphics.Rect
import android.hardware.display.DisplayManager
import android.os.Build
import android.view.Display
import android.view.RoundedCorner
import android.view.WindowManager
import androidx.core.view.WindowInsetsCompat
import com.carlink.logging.logInfo
import com.carlink.ocbm.PanelRule
import com.carlink.ocbm.PixelRect
import com.carlink.ocbm.ViewAreaRule
import com.carlink.protocol.AdapterConfig
import kotlin.math.ceil
import kotlin.math.roundToInt
import kotlin.math.sqrt

/**
 * Thin window-geometry helper.
 *
 * minSdk is 32 (Android 12L), so `currentWindowMetrics` and `getInsetsIgnoringVisibility`
 * (both API 30) are always available — there is no pre-30 fallback. Kept as a small wrapper
 * so call sites read uniformly.
 */
object WindowMetricsCompat {
    /** Full display bounds in pixels. */
    fun displayBounds(windowManager: WindowManager): Rect = windowManager.currentWindowMetrics.bounds

    /**
     * Stable window insets (ignoring visibility) as a [WindowInsetsCompat]. Callers extract
     * specific inset types via [WindowInsetsCompat.getInsetsIgnoringVisibility] with a
     * [WindowInsetsCompat.Type] mask.
     */
    fun stableWindowInsets(windowManager: WindowManager): WindowInsetsCompat = WindowInsetsCompat.toWindowInsetsCompat(windowManager.currentWindowMetrics.windowInsets)
}

/** Left/top/right/bottom, pixels. Immutable, unlike `android.graphics.Insets`, and JVM-testable. */
data class EdgeInsets(
    val left: Int = 0,
    val top: Int = 0,
    val right: Int = 0,
    val bottom: Int = 0,
) {
    val isZero: Boolean get() = left == 0 && top == 0 && right == 0 && bottom == 0

    companion object {
        val NONE = EdgeInsets()
    }
}

/**
 * The geometry the CarPlay config declares, from whichever source produced it: a detected
 * [DisplayProfile] (the norm) or the stored [AdapterConfig] (no window — tests, headless).
 */
data class PanelGeometry(
    val width: Int,
    val height: Int,
    val maxFps: Int,
    val safe: PixelRect,
    val dpi: Int,
    val diagonalInches: Double,
    /** Where the numbers came from, for the subscribe log line. */
    val source: String,
    /**
     * `drawUIOutsideSafeArea` for the declared view area. True whenever [safe] is a real inset:
     * with it OFF iOS paints the band between the safe rectangle and the panel edge BLACK; with it ON
     * the wallpaper extends into it while interactive UI stays inside the safe rectangle
     * (hardware-confirmed 2026-09-09, `docs/carplay/06_AV_PIPELINE.md`). A full-panel safe area sends
     * `false`, which keeps that document byte-identical to before this flag existed.
     */
    val drawUiOutsideSafeArea: Boolean = false,
) {
    fun describe(): String =
        "panel ${width}x$height@$maxFps safe ${safe.width}x${safe.height}@${safe.x},${safe.y} " +
            "drawUIOutsideSafeArea=$drawUiOutsideSafeArea dpi=$dpi diag=${"%.1f".format(diagonalInches)}\" ($source)"

    companion object {
        fun fromConfig(c: AdapterConfig): PanelGeometry =
            PanelGeometry(
                width = c.width,
                height = c.height,
                maxFps = if (c.fps >= 60) 60 else 30,
                safe = PixelRect(0, 0, c.width, c.height),
                dpi = c.dpi,
                diagonalInches = 0.0,
                source = "no display profile — stored config",
            )
    }
}

/**
 * Safe-area derivation, kept pure so a cutout/waterfall/rounded-corner panel can be exercised on the
 * JVM — the emulator this ships against has none of the three.
 */
object SafeAreaMath {
    /** `1 - 1/sqrt(2)`: inset each edge by this fraction of a corner radius and the rect's corner sits inside the arc. */
    private const val CORNER_INSET_FRACTION = 0.2929

    /**
     * The largest EVEN-aligned rectangle that avoids the cutouts, the waterfall edges and the rounded
     * corners. Origin rounds UP to even and the far edges DOWN, so the result can only shrink, never
     * spill over an inset (containment and parity are both session-teardown rules on iOS). Null when
     * nothing usable is left.
     */
    fun derive(
        panelW: Int,
        panelH: Int,
        cutout: EdgeInsets,
        waterfall: EdgeInsets,
        cornerRadiusPx: Int,
    ): PixelRect? {
        val corner = if (cornerRadiusPx > 0) ceil(cornerRadiusPx * CORNER_INSET_FRACTION).toInt() else 0
        val l = maxOf(cutout.left, waterfall.left, corner)
        val t = maxOf(cutout.top, waterfall.top, corner)
        val r = maxOf(cutout.right, waterfall.right, corner)
        val b = maxOf(cutout.bottom, waterfall.bottom, corner)
        val x = roundUpEven(l)
        val y = roundUpEven(t)
        val right = roundDownEven(panelW - r)
        val bottom = roundDownEven(panelH - b)
        val w = right - x
        val h = bottom - y
        if (w <= 0 || h <= 0) return null
        return PixelRect(x, y, w, h)
    }

    private fun roundUpEven(v: Int) = (v + 1) and 1.inv()

    private fun roundDownEven(v: Int) = v and 1.inv()
}

/**
 * What the display this app is running on actually is, read from the platform at launch — the source
 * of truth for the CarPlay config. Stored settings, where they exist, are an override on top of this,
 * never the other way round.
 *
 * Three rectangles, and they are NOT the same number:
 *  - the PHYSICAL panel (`maximumWindowMetrics.bounds`): [physicalWidthPx] x [physicalHeightPx];
 *  - the APP WINDOW (`currentWindowMetrics.bounds`): [windowWidthPx] x [windowHeightPx] — equal to the
 *    panel only when the window is fullscreen (split-screen, embedding and letterboxing shrink it);
 *  - the CONTENT AREA: the window minus the system bars the active display mode leaves VISIBLE
 *    ([barInsets]) — [widthPx] x [heightPx]. This is the rectangle the video surface occupies, the
 *    resolution iOS encodes at, and the basis every touch is normalised over. It is what the config
 *    declares; declaring the panel while a bar eats part of the surface mis-scales the frame and
 *    offsets every tap.
 *
 * [cutout] / [waterfall] are RELATIVE TO THE CONTENT AREA (a cutout under a visible status bar is
 * already outside it). The corner radius is kept as-is: a content-area corner can only sit further
 * inside the panel's arc than the panel corner does, so the safe area it produces is conservative.
 */
data class DisplayProfile(
    /** Content-area width in pixels: the rectangle the CarPlay surface occupies (see the class doc). */
    val widthPx: Int,
    /** Content-area height in pixels. */
    val heightPx: Int,
    /** Active `Display.Mode` refresh rate as reported, e.g. 59.94 or 60.000004. */
    val refreshHz: Float,
    val densityDpi: Int,
    val xdpi: Float,
    val ydpi: Float,
    val cutout: EdgeInsets = EdgeInsets.NONE,
    val waterfall: EdgeInsets = EdgeInsets.NONE,
    /** Largest of the four rounded-corner radii, 0 when the panel has square corners. */
    val cornerRadiusPx: Int = 0,
    /** App window bounds (`currentWindowMetrics`); defaults to the content area for callers without bars. */
    val windowWidthPx: Int = widthPx,
    val windowHeightPx: Int = heightPx,
    /** Physical panel (`maximumWindowMetrics`); defaults to the window for a fullscreen app. */
    val physicalWidthPx: Int = windowWidthPx,
    val physicalHeightPx: Int = windowHeightPx,
    /** The system bars the display mode leaves visible, in window pixels — what the content area is inset by. */
    val barInsets: EdgeInsets = EdgeInsets.NONE,
) {
    /**
     * The only two rates the box honours. Rounded first (a 59.94 panel IS a 60 Hz panel), then 60
     * only for a genuine 60: advertising 60 to a 50 Hz panel makes iOS encode frames we drop.
     */
    val maxFps: Int get() = if (refreshHz.roundToInt() >= 60) 60 else 30

    /** Physical diagonal of the PANEL from the axis densities; 0 when the platform reports no real dpi. */
    val diagonalInches: Double
        get() {
            if (xdpi <= 0f || ydpi <= 0f) return 0.0
            val wi = physicalWidthPx / xdpi
            val hi = physicalHeightPx / ydpi
            return sqrt((wi * wi + hi * hi).toDouble())
        }

    /** The panel to DECLARE: the content area, even, then inside [PanelRule]'s envelope (floor by orientation, 3840 max). */
    val panelWidth: Int get() = PanelRule.clamped(widthPx and 1.inv(), heightPx and 1.inv()).first
    val panelHeight: Int get() = PanelRule.clamped(widthPx and 1.inv(), heightPx and 1.inv()).second

    /** True when the declared panel differs from the content area — the app then scales the stream. */
    val panelClamped: Boolean get() = panelWidth != (widthPx and 1.inv()) || panelHeight != (heightPx and 1.inv())

    /**
     * The safe area to declare, always legal against [panelWidth] x [panelHeight]: derived from the
     * insets, then dropped back to the full panel when the insets leave less than the product floor
     * (a black lockout is worse than UI under a corner). Full-panel = the spec's default.
     */
    val safeArea: PixelRect
        get() {
            val full = PixelRect(0, 0, panelWidth, panelHeight)
            if (cutout.isZero && waterfall.isZero && cornerRadiusPx == 0) return full
            val s = SafeAreaMath.derive(panelWidth, panelHeight, cutout, waterfall, cornerRadiusPx) ?: return full
            if (ViewAreaRule.floorVerdict(s.width, s.height) != null) return full
            if (ViewAreaRule.teardownVerdict(s, panelWidth, panelHeight) != null) return full
            return s
        }

    /**
     * How far the declared safe area sits inside each edge of the declared panel, in content-area
     * pixels — the padding the app's own UI applies so nothing of its own renders under a cutout,
     * waterfall edge or corner arc. Zero when the stream is scaled ([panelClamped]: the insets would
     * be in the wrong pixel space) or when the safe area is the full panel.
     */
    val safeAreaInsets: EdgeInsets
        get() {
            if (panelClamped) return EdgeInsets.NONE
            val s = safeArea
            return EdgeInsets(
                left = s.x,
                top = s.y,
                right = panelWidth - (s.x + s.width),
                bottom = panelHeight - (s.y + s.height),
            )
        }

    /**
     * What the video surface insets the window by: the visible bars, plus the odd pixel the parity
     * rule drops from the content area's right/bottom edge — so the surface IS the declared panel
     * and touch is normalised over exactly the rectangle iOS encodes for (an 800x1151 content area
     * declares 800x1150; without this the surface would be one pixel taller than the stream).
     * Zero adjustment when the stream is scaled ([panelClamped]): the surface then stays the content area.
     */
    val surfaceInsets: EdgeInsets
        get() {
            if (panelClamped) return barInsets
            return EdgeInsets(
                left = barInsets.left,
                top = barInsets.top,
                right = barInsets.right + (widthPx - panelWidth),
                bottom = barInsets.bottom + (heightPx - panelHeight),
            )
        }

    /** Everything the config needs, as one legal value. */
    val geometry: PanelGeometry
        get() {
            val safe = safeArea
            return PanelGeometry(
                width = panelWidth,
                height = panelHeight,
                maxFps = maxFps,
                safe = safe,
                dpi = densityDpi,
                diagonalInches = diagonalInches,
                source = if (panelClamped) "detected, CLAMPED from ${widthPx}x$heightPx" else "detected",
                // A cutout/waterfall/corner inset: let the wallpaper fill the band instead of black.
                drawUiOutsideSafeArea = safe != PixelRect(0, 0, panelWidth, panelHeight),
            )
        }

    /** One line for the log and the report: content area first (that is what goes on the wire), then the window and the panel it sits in. */
    fun describe(): String =
        "content ${widthPx}x$heightPx@${"%.2f".format(refreshHz)}Hz ${densityDpi}dpi (${"%.1f".format(diagonalInches)}\")" +
            " window ${windowWidthPx}x$windowHeightPx physical ${physicalWidthPx}x$physicalHeightPx bars=$barInsets" +
            " -> panel ${panelWidth}x$panelHeight@$maxFps" + (if (panelClamped) " CLAMPED" else "") +
            " safe ${safeArea.let { "${it.width}x${it.height}@${it.x},${it.y}" }}" +
            " cutout=$cutout waterfall=$waterfall corner=${cornerRadiusPx}px"

    /**
     * The pure half of [detect]: this profile, measured on the WINDOW basis ([widthPx] x [heightPx]
     * = the window, cutout/waterfall from the window edges), re-based onto the content area left by
     * the system bars in [bars]. JVM-testable.
     */
    fun contentAreaUnder(bars: EdgeInsets): DisplayProfile =
        copy(
            widthPx = (widthPx - bars.left - bars.right).coerceAtLeast(0),
            heightPx = (heightPx - bars.top - bars.bottom).coerceAtLeast(0),
            cutout = cutout.relativeTo(bars),
            waterfall = waterfall.relativeTo(bars),
            windowWidthPx = widthPx,
            windowHeightPx = heightPx,
            barInsets = bars,
        )

    /** An inset measured from the window edge, re-expressed from the edge of the content area (never negative). */
    private fun EdgeInsets.relativeTo(bars: EdgeInsets): EdgeInsets =
        EdgeInsets(
            left = (left - bars.left).coerceAtLeast(0),
            top = (top - bars.top).coerceAtLeast(0),
            right = (right - bars.right).coerceAtLeast(0),
            bottom = (bottom - bars.bottom).coerceAtLeast(0),
        )

    companion object {
        /**
         * Read the live display. Call after the decor view is attached: before that
         * `currentWindowMetrics` insets come back consumed (all zero).
         *
         * Uses the display this activity is on (its default display on AAOS — the occupant-zone
         * screen; a cluster is a separate display we must not target).
         *
         * @param visibleBarTypes `WindowInsetsCompat.Type` mask of the system bars the active display
         *   mode leaves VISIBLE; their stable (visibility-ignoring) insets come off the window to give
         *   the content area. 0 = fullscreen-immersive, the content area is the whole window.
         */
        fun detect(
            activity: Activity,
            visibleBarTypes: Int = 0,
        ): DisplayProfile {
            val wm = activity.windowManager
            val metrics = wm.currentWindowMetrics
            val bounds = metrics.bounds
            val physical = wm.maximumWindowMetrics.bounds
            val raw = metrics.windowInsets
            val compat = WindowInsetsCompat.toWindowInsetsCompat(raw)
            val bars =
                if (visibleBarTypes != 0) {
                    compat.getInsetsIgnoringVisibility(visibleBarTypes).let { EdgeInsets(it.left, it.top, it.right, it.bottom) }
                } else {
                    EdgeInsets.NONE
                }
            val cut = compat.getInsetsIgnoringVisibility(WindowInsetsCompat.Type.displayCutout())
            val wf = raw.displayCutout?.waterfallInsets
            val corner =
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                    listOf(
                        RoundedCorner.POSITION_TOP_LEFT,
                        RoundedCorner.POSITION_TOP_RIGHT,
                        RoundedCorner.POSITION_BOTTOM_RIGHT,
                        RoundedCorner.POSITION_BOTTOM_LEFT,
                    ).maxOf { raw.getRoundedCorner(it)?.radius ?: 0 }
                } else {
                    0
                }
            val display: Display? =
                runCatching { activity.display }.getOrNull()
                    ?: activity.getSystemService(DisplayManager::class.java)?.getDisplay(Display.DEFAULT_DISPLAY)
            val refresh = display?.mode?.refreshRate ?: display?.refreshRate ?: 60f
            val dm = activity.resources.displayMetrics
            val p =
                DisplayProfile(
                    widthPx = bounds.width(),
                    heightPx = bounds.height(),
                    refreshHz = refresh,
                    densityDpi = dm.densityDpi,
                    xdpi = dm.xdpi,
                    ydpi = dm.ydpi,
                    cutout = EdgeInsets(cut.left, cut.top, cut.right, cut.bottom),
                    waterfall = wf?.let { EdgeInsets(it.left, it.top, it.right, it.bottom) } ?: EdgeInsets.NONE,
                    cornerRadiusPx = corner,
                    physicalWidthPx = physical.width(),
                    physicalHeightPx = physical.height(),
                ).contentAreaUnder(bars)
            logInfo("[DISPLAY] detected ${p.describe()}", tag = "MAIN")
            return p
        }
    }
}
