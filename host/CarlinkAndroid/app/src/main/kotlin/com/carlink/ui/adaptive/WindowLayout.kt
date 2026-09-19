package com.carlink.ui.adaptive

import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalWindowInfo
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.window.core.layout.WindowSizeClass
import androidx.window.core.layout.computeWindowSizeClass
import com.carlink.util.EdgeInsets
import kotlin.math.max
import kotlin.math.min

/**
 * The window as the dashboard sees it: the `androidx.window` size class plus the raw dp size the
 * class was computed from (the class alone cannot tell portrait from landscape). Recomputes in
 * place on every window resize — `LocalWindowInfo.containerSize` tracks the Compose root view, so
 * a `wm size`, a split-screen drag or a rotation handled in-process (the manifest keeps them there)
 * all re-lay the dashboard without an Activity restart.
 */
@Immutable
data class WindowLayoutInfo(
    val sizeClass: WindowSizeClass,
    val widthDp: Int,
    val heightDp: Int,
) {
    val isLandscape: Boolean get() = widthDp > heightDp
}

@Composable
fun rememberWindowLayoutInfo(): WindowLayoutInfo {
    val size = LocalWindowInfo.current.containerSize
    val density = LocalDensity.current.density
    return remember(size, density) {
        val w = (size.width / density).toInt()
        val h = (size.height / density).toInt()
        WindowLayoutInfo(WindowSizeClass.BREAKPOINTS_V1.computeWindowSizeClass(w, h), w, h)
    }
}

/** Which of the two dashboard arrangements a window gets. */
enum class DashboardArrangement {
    /** Adapter status + controls in a fixed-width column beside the known-devices grid; the row scrolls when the window is short. */
    TWO_PANE,

    /** Adapter card above the known-devices card in one scrolling column. */
    STACKED,
}

/**
 * The breakpoint scheme, kept pure so every cell of the width x height matrix is a JVM test:
 *
 * | width \ height  | compact < 480 | medium 480..900 | expanded >= 900 |
 * |-----------------|---------------|-----------------|-----------------|
 * | compact < 600   | STACKED       | STACKED         | STACKED         |
 * | medium 600..840 | STACKED       | STACKED         | STACKED         |
 * | expanded >= 840 | TWO_PANE*     | TWO_PANE*       | TWO_PANE*       |
 *
 * `*` only while the window is wider than it is tall; a portrait window always stacks, however
 * wide (a 1067 x 1707 dp portrait tablet is "expanded" on both axes and still reads better as a
 * column). Height never picks the arrangement: a short window keeps its arrangement and SCROLLS —
 * the alternative, collapsing to a column at compact height, makes a 1920x720 @ 320 dpi panel
 * stack for no gain in room.
 */
object DashboardLayout {
    /** The content block keeps the classic 70 % of the width, but never narrower than this unless the window is. */
    const val CONTENT_SHARE = 0.7f
    val CONTENT_MIN_WIDTH = 1100.dp

    /** In [DashboardArrangement.TWO_PANE] the cards are at least this share of the window height (they grow with their content). */
    const val CONTENT_HEIGHT_SHARE = 0.6f

    /** Adapter column: a share of the content block, clamped to the range where its buttons stay legible on one line. */
    const val ADAPTER_SHARE = 0.26f
    val ADAPTER_MIN_WIDTH = 300.dp
    val ADAPTER_MAX_WIDTH = 420.dp

    fun arrangement(info: WindowLayoutInfo): DashboardArrangement {
        val expanded = info.sizeClass.isWidthAtLeastBreakpoint(WindowSizeClass.WIDTH_DP_EXPANDED_LOWER_BOUND)
        return if (expanded && info.isLandscape) DashboardArrangement.TWO_PANE else DashboardArrangement.STACKED
    }

    /** Width of the centred content block for a window [availableDp] wide. */
    fun contentWidthDp(availableDp: Float): Float = max(availableDp * CONTENT_SHARE, min(availableDp, CONTENT_MIN_WIDTH.value))

    /** Width of the adapter column inside a content block [contentDp] wide. */
    fun adapterWidthDp(contentDp: Float): Float = (contentDp * ADAPTER_SHARE).coerceIn(ADAPTER_MIN_WIDTH.value, ADAPTER_MAX_WIDTH.value)
}

/** Column count and cell width for an adaptive grid; pure, in pixels. */
object GridMath {
    /** How many cells of at least [minCell] fit across [available] with [gap] between them (never fewer than one). */
    fun columns(
        available: Int,
        minCell: Int,
        gap: Int,
    ): Int = max(1, (available + gap) / (minCell + gap))

    /** The cell width that fills [available] with [columns] cells, capped at [maxCell] so two cards on a wide panel do not balloon. */
    fun cellWidth(
        available: Int,
        columns: Int,
        gap: Int,
        maxCell: Int,
    ): Int = min(maxCell, (available - (columns - 1) * gap) / columns).coerceAtLeast(0)
}

/**
 * A non-lazy adaptive grid: as many equal cells per row as fit at [minCellWidth] (`GridCells.Adaptive`
 * semantics), every cell the same height (the tallest cell's intrinsic), rows centred. Non-lazy on
 * purpose — it lives inside a scrolling column, where a `LazyVerticalGrid` would demand a bounded
 * height; a known-devices list is a handful of cards, not a feed.
 */
@Composable
fun AdaptiveGrid(
    modifier: Modifier = Modifier,
    minCellWidth: Dp = 200.dp,
    maxCellWidth: Dp = 280.dp,
    gap: Dp = 16.dp,
    content: @Composable () -> Unit,
) {
    Layout(content = content, modifier = modifier) { measurables, constraints ->
        if (measurables.isEmpty()) return@Layout layout(constraints.minWidth, constraints.minHeight) {}
        val gapPx = gap.roundToPx()
        // Unbounded width (a horizontal scroller) degrades to one row of max-width cells.
        val available = if (constraints.hasBoundedWidth) constraints.maxWidth else maxCellWidth.roundToPx() * measurables.size
        val columns = min(measurables.size, GridMath.columns(available, minCellWidth.roundToPx(), gapPx))
        val cellW = GridMath.cellWidth(available, columns, gapPx, maxCellWidth.roundToPx())
        val cellH = measurables.maxOf { it.minIntrinsicHeight(cellW) }
        val placeables = measurables.map { it.measure(Constraints.fixed(cellW, cellH)) }
        val rows = (placeables.size + columns - 1) / columns
        val rowW = columns * cellW + (columns - 1) * gapPx
        val width = if (constraints.hasBoundedWidth) constraints.maxWidth else rowW
        val height = (rows * cellH + (rows - 1) * gapPx).coerceIn(constraints.minHeight, constraints.maxHeight)
        layout(width, height) {
            val startX = Alignment.CenterHorizontally.align(rowW, width, layoutDirection)
            placeables.forEachIndexed { i, p ->
                p.placeRelative(startX + (i % columns) * (cellW + gapPx), (i / columns) * (cellH + gapPx))
            }
        }
    }
}

/** Pixel insets as Compose window insets, so a detected safe area pads exactly (no dp round-trip). */
fun EdgeInsets.asWindowInsets(): WindowInsets = WindowInsets(left = left, top = top, right = right, bottom = bottom)
