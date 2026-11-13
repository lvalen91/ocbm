package zeno.gmccpa.av

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Plan v5.7 U1 / audit12 F081: once the primary finger has lifted, a surviving finger's MOVE (a tip-DOWN
 * on the wire) or CANCEL must send nothing until the next ACTION_DOWN.
 *
 * Action values are android.view.MotionEvent's (compile-time constants, inlined; no Android at runtime).
 */
class PrimaryPointerFilterTest {

    private companion object {
        const val DOWN = 0; const val UP = 1; const val MOVE = 2; const val CANCEL = 3
        const val POINTER_DOWN = 5; const val POINTER_UP = 6; const val HOVER_MOVE = 7
        const val A = 10; const val B = 20
        const val CONSUMED = PrimaryPointerFilter.CONSUMED
    }

    private val f = PrimaryPointerFilter()

    /** [ids] are the pointer ids present in the event, in index order. */
    private fun ev(action: Int, actionIndex: Int, vararg ids: Int): Int =
        f.decide(action, actionIndex, ids[actionIndex]) { id -> ids.indexOf(id) }

    @Test fun primaryLiftsFirstThenSurvivorMoveSendsNothing() {
        assertEquals(PrimaryPointerFilter.PHASE_DOWN, ev(DOWN, 0, A))
        assertEquals(CONSUMED, ev(POINTER_DOWN, 1, A, B))
        assertEquals(PrimaryPointerFilter.PHASE_UP, ev(POINTER_UP, 0, A, B))
        assertEquals(0, f.index)   // UP at A's own position
        assertEquals(CONSUMED, ev(MOVE, 0, B))
        assertEquals(CONSUMED, ev(MOVE, 0, B))
        assertEquals(CONSUMED, ev(CANCEL, 0, B))
    }

    @Test fun nextDownStartsANewGesture() {
        ev(DOWN, 0, A); ev(POINTER_DOWN, 1, A, B); ev(POINTER_UP, 0, A, B)
        assertEquals(CONSUMED, ev(MOVE, 0, B))
        assertEquals(PrimaryPointerFilter.PHASE_UP, ev(UP, 0, B))   // redundant tip-up: fails toward release
        assertEquals(PrimaryPointerFilter.PHASE_DOWN, ev(DOWN, 0, B))
        assertEquals(PrimaryPointerFilter.PHASE_MOVE, ev(MOVE, 0, B))
        assertEquals(B, f.primaryId)
    }

    @Test fun secondFingerLiftingFirstKeepsThePrimary() {
        ev(DOWN, 0, A); ev(POINTER_DOWN, 1, A, B)
        assertEquals(CONSUMED, ev(POINTER_UP, 1, A, B))
        assertEquals(PrimaryPointerFilter.PHASE_MOVE, ev(MOVE, 0, A))
        assertEquals(PrimaryPointerFilter.PHASE_UP, ev(UP, 0, A))
    }

    @Test fun moveTracksThePrimaryIndexNotIndexZero() {
        // B is the primary (its ACTION_DOWN); A arrives second but lands at index 0.
        ev(DOWN, 0, B); ev(POINTER_DOWN, 0, A, B)
        assertEquals(PrimaryPointerFilter.PHASE_MOVE, ev(MOVE, 0, A, B))
        assertEquals(1, f.index)                     // B's index
    }

    @Test fun cancelWhilePrimaryDownIsReportedOnce() {
        ev(DOWN, 0, A)
        assertEquals(PrimaryPointerFilter.PHASE_CANCEL, ev(CANCEL, 0, A))
        assertEquals(CONSUMED, ev(CANCEL, 0, A))
    }

    @Test fun nonTouchActionIsUnhandled() {
        assertEquals(PrimaryPointerFilter.UNHANDLED, ev(HOVER_MOVE, 0, A))
    }
}
