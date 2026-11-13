package zeno.gmccpa.av

import android.view.MotionEvent

/**
 * Single-touch filter for [CarPlayActivity.onTouch]: which wire phase, if any, a MotionEvent becomes.
 * UI thread only. No Android calls (the MotionEvent action values are compile-time constants), so it
 * is unit-tested on the host JVM (PrimaryPointerFilterTest).
 *
 * Only the PRIMARY pointer (the one that went down with ACTION_DOWN) is reported. A second finger
 * arriving is ignored; the primary lifting first sends UP at the primary's own position.
 *
 * **After the primary has lifted, MOVE and CANCEL are dropped until the next ACTION_DOWN (audit12
 * F081).** The wire has no "move while up": the JNI maps phase 1 to tip-DOWN
 * (native/carplay-jni/src/lib.rs `nativeTouch`), so a MOVE of the surviving finger was a phantom touch
 * at the survivor, and a CANCEL (displaced MOVE + UP) was the same plus a lift. The survivor's own final
 * ACTION_UP still sends phase 2: a redundant tip-UP fails toward release, never toward a touch.
 */
class PrimaryPointerFilter {

    /** Pointer id of the tracked primary, or [NONE] once it has lifted. */
    var primaryId = NONE
        private set

    /** For a result >= 0: the pointer index whose coordinates the caller reports. */
    var index = 0
        private set

    /**
     * Returns a wire phase ([PHASE_DOWN], [PHASE_MOVE], [PHASE_UP], [PHASE_CANCEL]), or [CONSUMED]
     * (handled, send nothing), or [UNHANDLED] (not a touch action; the caller returns false).
     * [findPointerIndex] is `MotionEvent.findPointerIndex`.
     */
    fun decide(action: Int, actionIndex: Int, actionPointerId: Int, findPointerIndex: (Int) -> Int): Int =
        when (action) {
            MotionEvent.ACTION_DOWN -> { primaryId = actionPointerId; index = actionIndex; PHASE_DOWN }
            MotionEvent.ACTION_MOVE ->
                if (primaryId == NONE) CONSUMED
                else { index = primaryIndex(findPointerIndex); PHASE_MOVE }
            MotionEvent.ACTION_UP -> { index = actionIndex; primaryId = NONE; PHASE_UP }
            MotionEvent.ACTION_CANCEL ->
                if (primaryId == NONE) CONSUMED
                else { index = primaryIndex(findPointerIndex); primaryId = NONE; PHASE_CANCEL }
            MotionEvent.ACTION_POINTER_DOWN -> CONSUMED   // a non-primary finger arrived
            MotionEvent.ACTION_POINTER_UP ->
                if (actionPointerId == primaryId) { index = actionIndex; primaryId = NONE; PHASE_UP }
                else CONSUMED                              // a non-primary finger lifting
            else -> UNHANDLED
        }

    /** The primary's index in this event; 0 if it is somehow absent (the pre-F081 fallback). */
    private fun primaryIndex(findPointerIndex: (Int) -> Int): Int =
        findPointerIndex(primaryId).takeIf { it >= 0 } ?: 0

    companion object {
        const val NONE = -1
        const val PHASE_DOWN = 0
        const val PHASE_MOVE = 1
        const val PHASE_UP = 2
        /** Internal only; the wire knows 0/1/2. See CarPlayActivity.onTouch. */
        const val PHASE_CANCEL = 3
        const val CONSUMED = -1
        const val UNHANDLED = -2
    }
}
