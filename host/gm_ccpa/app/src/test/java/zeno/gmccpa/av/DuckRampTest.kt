package zeno.gmccpa.av

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Plan v5.8 V3 — the duck parameters, the one-shaper min(voice, focus) curve, and the executor. */
class DuckRampTest {

    @Test fun dbToLinearDefaultsAndClamp() {
        assertEquals(0.2f, DuckMath.duckTarget(null), 1e-6f)
        assertEquals(0.5012f, DuckMath.duckTarget(-6.0), 1e-3f)
        assertEquals(0.1f, DuckMath.duckTarget(-20.0), 1e-4f)
        assertEquals("clamped to 1", 1f, DuckMath.duckTarget(6.0), 0f)
        assertEquals(0f, DuckMath.duckTarget(-400.0), 1e-6f)
        assertEquals(500L, DuckMath.duration(null))
        assertEquals(500L, DuckMath.duration(-1.0))
        assertEquals(250L, DuckMath.duration(250.0))
    }

    @Test fun voiceRampAndIdempotence() {
        val d = DuckComposer()
        val c = d.setVoice(0.2f, 500, 1_000)!!
        assertEquals(1f, c.start, 0f); assertEquals(0.2f, c.end, 0f); assertEquals(500L, c.durationMs)
        assertNull("a repeated target is a no-op: no new curve", d.setVoice(0.2f, 500, 1_100))
        assertEquals(0.6f, d.gainAt(1_250), 1e-4f)
    }

    /** FOCUS duck on top of an ACTIVE voice ramp: the curve is the exact min of the two. */
    @Test fun focusDuckOnTopOfAnActiveVoiceRamp() {
        val d = DuckComposer(focusDuckGain = 0.2f, focusRampMs = 20)
        d.setVoice(0.5f, 500, 0)
        val c = d.setFocus(true, 200)!!
        // voice at 200 = 0.8; focus 1 → 0.2 over 20 ms; they cross ≈ 5 ms in; then focus rules.
        assertEquals(0.8f, c.start, 1e-4f)
        assertEquals(0.2f, c.end, 1e-6f)
        assertEquals(300L, c.durationMs)
        assertTrue("a crossing breakpoint: ${c}", c.timesMs.any { it in 4..7 })
        for (t in listOf(200L, 205L, 210L, 220L, 300L, 500L))
            assertEquals("min at $t", minOf(0.8f - 0.3f * (t - 200) / 300f, maxOf(0.2f, 1f - 0.8f * (t - 200) / 20f)), d.gainAt(t), 2e-3f)
        // Focus restored while the voice duck holds: back to the voice target, not unity.
        val back = d.setFocus(false, 600)!!
        assertEquals(0.5f, back.end, 1e-6f)
    }

    /** A track rebuilt mid-ramp: its new shaper starts at the current gain and runs the rest. */
    @Test fun rebuiltTrackResumesMidRamp() {
        val d = DuckComposer()
        d.setVoice(0.2f, 400, 1_000)
        val c = d.curve(1_100)
        assertEquals(0.8f, c.start, 1e-4f)
        assertEquals(0.2f, c.end, 1e-6f)
        assertEquals(300L, c.durationMs)
        val done = d.curve(2_000)
        assertEquals(0L, done.durationMs)
        assertEquals(0.2f, done.end, 0f)
    }

    /** T1 unduck hold: the voice target goes to the -13.6 dB hold level, then 1.0 at the nav END. */
    @Test fun holdLevelThenRestore() {
        val d = DuckComposer()
        d.setVoice(0.2f, 500, 0)
        val hold = d.setVoice(VoiceLatency.NAV_HOLD_LEVEL, 500, 4_000)!!
        assertEquals(0.2f, hold.start, 1e-6f)
        assertEquals(0.2089f, hold.end, 5e-4f)
        val end = d.setVoice(1f, 500, 5_900)!!
        assertEquals(1f, end.end, 0f)
    }

    /** The executor is signalled from the stamp and applies at once; it samples at start + duration + 20 ms. */
    @Test fun executorAppliesOnTheSignalAndSamples() {
        var now = 1_000_000L
        val applied = ArrayList<Pair<Float, Long>>()
        val lines = ArrayList<String>()
        val ex = DuckExecutor({ now }, { t, ms, _ -> applied.add(t to ms) }, { DuckSample(0.2f, true) }, { lines.add(it) })
        ex.signal(0.2f, 500, "duckAudio")
        now += 3_000
        ex.drainNow()
        assertEquals(listOf(0.2f to 500L), applied)
        assertTrue(lines.single().contains("stamp → shaper start 3ms"))
        // The sample is due at start + 520 ms.
        now += 520_000
        ex.runOnce(0)
        assertTrue(lines.last(), lines.last().contains("sampled VolumeShaper volume 0.2") && lines.last().contains("OK"))
        // An earlier stamp (the unduck's cp-meta arrival) is kept through the NAV worker hop.
        ex.signal(1f, 500, "unduckAudio", stampUs = now - 7_000)
        ex.drainNow()
        assertTrue(lines.last().contains("stamp → shaper start 7ms"))
    }

    /**
     * Review round 1: duckAudio reaches the executor directly, unduckAudio through the NAV worker. A
     * slow worker must not apply a stale unduck AFTER the next prompt's duck (T1: re-duck 165 ms after
     * an unduck): the newest cp-meta stamp wins.
     */
    @Test fun aStaleUnduckArrivingAfterTheNextDuckIsSkipped() {
        var now = 10_000_000L
        val applied = ArrayList<Float>()
        val ex = DuckExecutor({ now }, { t, _, _ -> applied.add(t) }, { DuckSample(null, true) }, { })
        ex.signal(0.2f, 500, "duckAudio", stampUs = 1_000_000)          // prompt 1
        ex.signal(0.2f, 500, "duckAudio", stampUs = 2_165_000)          // prompt 2, direct
        ex.signal(1f, 500, "unduckAudio", stampUs = 2_000_000)          // prompt 1's unduck, late via the worker
        ex.drainNow()
        assertEquals(listOf(0.2f, 0.2f), applied)
        assertEquals(1L, ex.skippedStale)
    }

    /**
     * Bench cp12 (2026-10-08): a restore applied while media was PAUSED for Siri sampled 0.209 — the
     * shaper is evaluated only while the track is mixed. The self-check defers while paused and samples
     * one ramp after the track plays again (AacPlayer re-asserts the curve on that resume).
     */
    @Test fun selfCheckDefersWhileMediaIsPaused() {
        var now = 1_000_000L
        var playing = false
        var vol = 0.209f
        val lines = ArrayList<String>()
        val ex = DuckExecutor({ now }, { _, _, _ -> }, { DuckSample(vol, playing) }, { lines.add(it) })
        ex.signal(1f, 500, "restore")
        ex.drainNow()
        now += 520_000; ex.runOnce(0)
        assertTrue(lines.last(), lines.last().contains("self-check deferred"))
        assertTrue("no OFF verdict on a paused track", lines.none { it.contains("(OFF") })
        now += 2_000_000; ex.runOnce(0)
        playing = true; vol = 1f                                  // resumed; the re-asserted curve ran
        now += 250_000; ex.runOnce(0)                             // sees it playing: samples one ramp later
        now += 520_000; ex.runOnce(0)
        assertTrue(lines.last(), lines.last().contains("sampled VolumeShaper volume 1.0") && lines.last().contains("(OK"))
    }
}
