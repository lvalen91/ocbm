package com.carlink.projection

import android.view.MotionEvent
import com.carlink.ocbm.Ocbm

/**
 * `MotionEvent` → CarPlay touch frames.
 *
 * Split out of the view purely so it is testable: the mapping has three details that are each their
 * own bug, and none of them is observable without an iPhone unless this is exercised directly.
 *
 *  1. **Normalise to 0..65535, not to pixels.** The box scales to the resolution it advertised, so
 *     sending pixels would double-apply the scale on any panel where the advertised and actual
 *     sizes differ.
 *  2. **`ACTION_MOVE` carries every pointer at once.** A move must emit one frame per active
 *     pointer; emitting only `actionIndex` (which is meaningless for MOVE) drops the second finger
 *     mid-pinch and the gesture reads as a jumping single touch.
 *  3. **Pointer *id*, not pointer *index*.** Indices renumber when a finger lifts; ids do not.
 *     `airplayd` keys its two contact slots on the id we send and holds the mapping while the
 *     contact is down, so sending indices would swap fingers mid-gesture.
 *
 * `ACTION_CANCEL` is deliberately treated as UP for every pointer: iOS has no cancel concept here,
 * and leaving a contact down because the window lost the gesture is how a phantom touch gets stuck.
 */
class TouchForwarder(
    private val input: TouchSink,
) {
    /**
     * Convert and send one event.
     *
     * [viewWidth] and [viewHeight] are the *view's* pixel size, used only to normalise. Returns the
     * number of frames sent, which is what the tests assert on.
     */
    fun onTouch(
        e: MotionEvent,
        viewWidth: Int,
        viewHeight: Int,
    ): Int {
        if (viewWidth <= 0 || viewHeight <= 0) return 0
        var sent = 0
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> {
                sent += emit(e, e.actionIndex, Ocbm.TOUCH_DOWN, viewWidth, viewHeight)
            }
            MotionEvent.ACTION_MOVE -> {
                // Every pointer, not just actionIndex — see note 2.
                for (i in 0 until e.pointerCount) {
                    sent += emit(e, i, Ocbm.TOUCH_MOVE, viewWidth, viewHeight)
                }
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP -> {
                sent += emit(e, e.actionIndex, Ocbm.TOUCH_UP, viewWidth, viewHeight)
            }
            MotionEvent.ACTION_CANCEL -> {
                for (i in 0 until e.pointerCount) {
                    sent += emit(e, i, Ocbm.TOUCH_UP, viewWidth, viewHeight)
                }
            }
            else -> Unit
        }
        return sent
    }

    private fun emit(
        e: MotionEvent,
        index: Int,
        phase: Byte,
        w: Int,
        h: Int,
    ): Int {
        if (index < 0 || index >= e.pointerCount) return 0
        // Clamped because a MOVE can legitimately report coordinates outside the view once the
        // gesture has left it, and a negative would wrap when packed into the u16.
        val nx = ((e.getX(index) / w) * U16_MAX).toInt().coerceIn(0, U16_MAX)
        val ny = ((e.getY(index) / h) * U16_MAX).toInt().coerceIn(0, U16_MAX)
        input.touch(phase, nx, ny, e.getPointerId(index))
        return 1
    }

    private companion object {
        const val U16_MAX = 65535
    }
}
