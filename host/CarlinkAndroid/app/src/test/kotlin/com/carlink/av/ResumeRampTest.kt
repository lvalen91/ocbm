package com.carlink.av

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ResumeRampTest {
    private val logs = ArrayList<String>()

    /** Mono at 100 Hz: 100 frames = 1000 ms, so one buffer spans the whole default ramp. */
    private val mono100 = ResumeRamp.Format(channels = 1, rate = 100)

    private fun ramp(durationMs: Long = 1_000L) = ResumeRamp(durationMs) { logs.add(it) }

    /** One mono buffer of [frames] samples at value 10000, [rate] Hz. */
    private fun buffer(frames: Int): ByteArray {
        val b = ByteArray(frames * 2)
        for (i in 0 until frames) {
            b[2 * i] = (10000 and 0xFF).toByte()
            b[2 * i + 1] = (10000 shr 8).toByte()
        }
        return b
    }

    private fun sample(
        b: ByteArray,
        i: Int,
    ): Int = ((b[2 * i + 1].toInt() shl 8) or (b[2 * i].toInt() and 0xFF)).toShort().toInt()

    @Test
    fun `equal-power curve at 0 25 50 75 100 percent`() {
        assertEquals(0.000f, ResumeRamp.curve(0.0), 0.001f)
        assertEquals(0.383f, ResumeRamp.curve(0.25), 0.001f)
        assertEquals(0.707f, ResumeRamp.curve(0.5), 0.001f)
        assertEquals(0.924f, ResumeRamp.curve(0.75), 0.001f)
        assertEquals(1.000f, ResumeRamp.curve(1.0), 0.001f)
        assertEquals(1_000L, ResumeRamp.DURATION_MS)
    }

    @Test
    fun `positive case - hard interruption with active media arms, the post-gap frame starts, samples ramp up`() {
        val r = ramp()
        r.interrupted(mediaWasActive = true)
        assertTrue(r.armed)
        assertTrue(r.start(now = 0L))
        assertTrue(r.active)
        // One buffer spanning the whole ramp: 100 frames at 100 Hz = 1000 ms.
        val b = buffer(100)
        r.apply(b, 0, b.size, mono100, now = 0L)
        assertEquals("first sample starts at silence", 0, sample(b, 0))
        var prev = -1
        for (i in 0 until 100) {
            val s = sample(b, i)
            assertTrue("monotonic at $i", s >= prev)
            prev = s
        }
        assertTrue("last sample near full scale: $prev", prev > 9_800)
        assertFalse("ramp ended with the buffer", r.active)
        assertTrue(logs.any { it.startsWith("resume ramp START from gain 0.00") })
        assertTrue(logs.any { it.startsWith("resume ramp END") })
    }

    @Test
    fun `excluded cases never start a ramp`() {
        // Initial session start / ordinary gap / user pause-play: nothing called interrupted().
        val fresh = ramp()
        assertFalse(fresh.start(now = 5L))
        assertFalse(fresh.active)
        val b = buffer(10)
        fresh.apply(b, 0, b.size, mono100, 5L)
        assertEquals("untouched", 10000, sample(b, 0))
        // Nav prompt: a soft duck is not a hard interruption, so interrupted() is not called either.
        // A hard interruption while media was NOT active (phone already paused): armed stays false.
        val idle = ramp()
        idle.interrupted(mediaWasActive = false)
        assertFalse(idle.armed)
        assertFalse(idle.start(now = 1L))
    }

    @Test
    fun `an interruption mid-ramp cancels it and the restart continues from the level reached`() {
        val r = ramp()
        r.interrupted(mediaWasActive = true)
        r.start(now = 0L)
        // Half the ramp: 50 frames at 100 Hz = 500 ms -> gain ~0.707.
        val half = buffer(50)
        r.apply(half, 0, half.size, mono100, 0L)
        assertEquals(0.707f, r.level, 0.01f)
        r.interrupted(mediaWasActive = true)
        assertFalse(r.active)
        assertTrue(r.armed)
        assertEquals("level kept for the restart", 0.707f, r.level, 0.01f)
        assertTrue(r.start(now = 10_000L))
        assertEquals("restarts from the reached level, not 0", 0.707f, r.gainAt(10_000L), 0.01f)
        // Only the remaining half of the duration is left.
        assertEquals(1.0f, r.gainAt(10_500L), 0.01f)
        assertTrue(logs.any { it.startsWith("resume ramp cancelled at gain 0.71") })
    }

    @Test
    fun `a second interruption while media is already silenced keeps the arm`() {
        // Measured 2026-09-25: Siri's within-turn focus flap re-entered interrupted() 4.5 s after the
        // first one, with no audible media in the window — the ramp must still fire on resume.
        val r = ramp()
        r.interrupted(mediaWasActive = true)
        r.interrupted(mediaWasActive = false)
        assertTrue(r.armed)
        assertTrue(r.start(now = 0L))
    }

    @Test
    fun `start is one-shot per interruption`() {
        val r = ramp()
        r.interrupted(mediaWasActive = true)
        assertTrue(r.start(0L))
        val b = buffer(100)
        r.apply(b, 0, b.size, mono100, 0L)
        assertFalse("a later gap with no new interruption does not fade again", r.start(5_000L))
    }
}
