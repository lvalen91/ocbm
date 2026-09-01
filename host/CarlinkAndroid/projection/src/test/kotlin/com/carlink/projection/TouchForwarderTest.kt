package com.carlink.projection

import android.view.MotionEvent
import com.carlink.ocbm.Ocbm
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * Touch mapping, which is untestable on this bench any other way — the Pi has no touchscreen at all
 * (`/proc/bus/input/devices` lists two HDMI audio jacks, a USB mouse and EarPods), so multi-touch
 * can be declared and plumbed but not exercised by hand.
 *
 * That makes these assertions the only thing standing between a wrong mapping and a silent failure
 * on hardware we do not have yet.
 */
@RunWith(RobolectricTestRunner::class)
class TouchForwarderTest {
    /** Captures what would go on the wire, without a socket. */
    private class Recorder : TouchSink {
        data class Touch(
            val phase: Byte,
            val nx: Int,
            val ny: Int,
            val finger: Int,
        )

        val touches = mutableListOf<Touch>()

        override fun touch(
            phase: Byte,
            nx: Int,
            ny: Int,
            finger: Int,
        ) {
            touches += Touch(phase, nx, ny, finger)
        }
    }

    private fun event(
        action: Int,
        pointers: List<Triple<Int, Float, Float>>,
        actionIndex: Int = 0,
    ): MotionEvent {
        val props =
            pointers
                .map { (id, _, _) ->
                    MotionEvent.PointerProperties().apply {
                        this.id = id
                        toolType = MotionEvent.TOOL_TYPE_FINGER
                    }
                }.toTypedArray()
        val coords =
            pointers
                .map { (_, x, y) ->
                    MotionEvent.PointerCoords().apply {
                        this.x = x
                        this.y = y
                    }
                }.toTypedArray()
        val masked = action or (actionIndex shl MotionEvent.ACTION_POINTER_INDEX_SHIFT)
        return MotionEvent.obtain(
            0L,
            0L,
            masked,
            pointers.size,
            props,
            coords,
            0,
            0,
            1f,
            1f,
            0,
            0,
            0,
            0,
        )
    }

    @Test
    fun `coordinates are normalised to 0-65535, not sent as pixels`() {
        val r = Recorder()
        // Dead centre of a 1000x500 view.
        TouchForwarder(r).onTouch(event(MotionEvent.ACTION_DOWN, listOf(Triple(0, 500f, 250f))), 1000, 500)
        assertEquals(1, r.touches.size)
        val t = r.touches[0]
        assertEquals(Ocbm.TOUCH_DOWN, t.phase)
        // The box scales these to the resolution it ADVERTISED; sending pixels would double-apply
        // the scale on any panel where advertised and actual differ.
        assertEquals(32767, t.nx)
        assertEquals(32767, t.ny)
    }

    @Test
    fun `a move emits every active pointer, not just actionIndex`() {
        val r = Recorder()
        val e =
            event(
                MotionEvent.ACTION_MOVE,
                listOf(Triple(7, 0f, 0f), Triple(9, 1000f, 500f)),
            )
        TouchForwarder(r).onTouch(e, 1000, 500)
        // actionIndex is meaningless for MOVE. Emitting only it drops the second finger mid-pinch
        // and the gesture reads as a jumping single touch.
        assertEquals(2, r.touches.size)
        assertEquals(listOf(7, 9), r.touches.map { it.finger })
    }

    @Test
    fun `pointer id is sent, not pointer index`() {
        val r = Recorder()
        // Two contacts; lift the FIRST one. The survivor keeps id 9 even though its index becomes 0.
        val e =
            event(
                MotionEvent.ACTION_POINTER_UP,
                listOf(Triple(7, 10f, 10f), Triple(9, 20f, 20f)),
                actionIndex = 0,
            )
        TouchForwarder(r).onTouch(e, 100, 100)
        assertEquals(1, r.touches.size)
        // airplayd keys its two contact slots on this value and holds the mapping while the contact
        // is down, so sending an index would swap fingers mid-gesture.
        assertEquals(7, r.touches[0].finger)
        assertEquals(Ocbm.TOUCH_UP, r.touches[0].phase)
    }

    @Test
    fun `cancel lifts every contact`() {
        val r = Recorder()
        val e =
            event(
                MotionEvent.ACTION_CANCEL,
                listOf(Triple(1, 0f, 0f), Triple(2, 5f, 5f)),
            )
        TouchForwarder(r).onTouch(e, 100, 100)
        // iOS has no cancel concept here. Leaving a contact down because the window lost the gesture
        // is how a phantom touch gets stuck on the phone's UI.
        assertEquals(2, r.touches.size)
        assertTrue(r.touches.all { it.phase == Ocbm.TOUCH_UP })
    }

    @Test
    fun `coordinates outside the view are clamped rather than wrapped`() {
        val r = Recorder()
        // A MOVE can legitimately report coordinates outside the view once the gesture leaves it.
        // A negative would wrap when packed into the u16 and land on the opposite edge.
        TouchForwarder(r).onTouch(
            event(MotionEvent.ACTION_MOVE, listOf(Triple(0, -50f, 9999f))),
            100,
            100,
        )
        assertEquals(0, r.touches[0].nx)
        assertEquals(65535, r.touches[0].ny)
    }

    @Test
    fun `a zero-sized view sends nothing instead of dividing by zero`() {
        val r = Recorder()
        val n = TouchForwarder(r).onTouch(event(MotionEvent.ACTION_DOWN, listOf(Triple(0, 1f, 1f))), 0, 0)
        assertEquals(0, n)
        assertTrue(r.touches.isEmpty())
    }
}
