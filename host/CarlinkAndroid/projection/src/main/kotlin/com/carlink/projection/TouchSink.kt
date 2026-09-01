package com.carlink.projection

/**
 * The one method [TouchForwarder] needs from [InputSender].
 *
 * Exists so the `MotionEvent` mapping is testable without a socket. That matters more than usual
 * here: the Pi has no touchscreen at all, so these tests are the only exercise the multi-touch path
 * gets until real hardware is attached (see `pi/docs/01_PROJECTION_APP_DESIGN.md` §5.3).
 */
fun interface TouchSink {
    /**
     * One contact update. [nx]/[ny] are normalised 0..65535; [finger] is a stable per-contact id
     * that `airplayd` maps to one of Apple's two transducer slots.
     */
    fun touch(
        phase: Byte,
        nx: Int,
        ny: Int,
        finger: Int,
    )
}
