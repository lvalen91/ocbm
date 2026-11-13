package zeno.gmccpa.av

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import zeno.gmccpa.av.VoiceLatency.NavDuringVoice
import zeno.gmccpa.av.VoiceLatency.Purpose

/**
 * Plan v5.8 V2/V3 for NAV, as the owner decided on 2026-10-07 (OD-8C = YES, OD-8D = PLAY_LIVE —
 * stock) and as stock does it on this bench (/tmp/ccpa_sess/stock_runtime T1/T2/T3):
 *  - NAV requests no focus, and its stream plays live for its whole life (no epochs);
 *  - PLAY_LIVE is the default; DROP survives as a non-default safety net and keeps its fixtures;
 *  - duckAudio ramps media at once; unduckAudio with the nav stream still live HOLDS media at
 *    -13.6 dB until that stream ENDs (T1), and a lost unduck is restored at the END.
 * Uses the rig in VoiceStreamsTest.kt.
 */
class NavTest {

    private fun nav(r: VoiceRig, scid: Long, loud: (Int) -> Boolean = { true }) =
        Feeder(r, scid, 101, 4, 48000, 2, 10, loud)

    @Test fun switchDefaultsAreTheOwnersAnswers() {
        assertEquals(NavDuringVoice.PLAY_LIVE, VoiceLatency.NAV_DURING_VOICE)
        assertFalse("PLAY_LIVE: never suppressed", VoiceIdleRules.navSuppressed(true, true, true))
        assertTrue(VoiceIdleRules.navSuppressed(true, false, false, NavDuringVoice.DROP))
        assertTrue(VoiceIdleRules.navSuppressed(false, true, false, NavDuringVoice.DROP))
        assertTrue(VoiceIdleRules.navSuppressed(false, false, true, NavDuringVoice.DROP))
        assertFalse(VoiceIdleRules.navSuppressed(false, false, false, NavDuringVoice.DROP))
        for (a in listOf(true, false)) for (b in listOf(true, false)) for (c in listOf(true, false))
            assertFalse(VoiceIdleRules.navSuppressed(a, b, c, NavDuringVoice.PLAY_LIVE))
        assertEquals(0.2089f, VoiceLatency.NAV_HOLD_LEVEL, 0.0005f)
    }

    /** OD-8C / T1: no focus request, the whole stream (quiet and loud) played live. */
    @Test fun navTakesNoFocusAndPlaysTheWholeStreamLive() {
        val r = VoiceRig()
        r.begin(0x101, 101, VoiceLatency.AT_DEFAULT, 48000, 2)
        val s = r.stream(0x101)!!
        val f = nav(r, 0x101) { it in 100..250 }          // 1 s quiet, 1.5 s loud, then quiet
        r.run(4_000) { f.tick(it) }
        f.on = false
        r.end(0x101, f.k.toLong())
        r.run(600)
        assertEquals("no NAV focus request (stock SD:748-750)", 0, r.focus.count(Purpose.NAV))
        assertEquals(0L, s.jb.epochIdle)
        assertEquals(0L, s.jb.suppressed)
        assertTrue("all of it played: ${s.jb.summary()}", s.jb.played >= f.k - 1)
        assertTrue(r.log.has("identity OK"))
        assertEquals(0, r.factory.track(Purpose.NAV).underruns)
    }

    /** T3: during a call the iPhone's tone on AltAudio is mixed live; the call's focus is untouched. */
    @Test fun callToneIsMixedLiveOverTheCall() {
        val r = VoiceRig()
        r.modes(phone = true)
        r.begin(0x10, 100, VoiceLatency.AT_TELEPHONY)
        val call = Feeder(r, 0x10, 100, 1, 16000, 1, 30, loudAt = { true })
        r.run(1_000) { call.tick(it) }
        r.begin(0x11, 101, VoiceLatency.AT_DEFAULT, 48000, 2)
        val s = r.stream(0x11)!!
        val tone = nav(r, 0x11)
        r.run(1_740) { call.tick(it); tone.tick(it) }      // "sound effect announcement in 1.74 seconds"
        tone.on = false
        r.end(0x11, tone.k.toLong())
        r.run(500) { call.tick(it) }
        assertTrue("the tone played: ${s.jb.summary()}", s.jb.played >= tone.k - 1)
        assertEquals(0L, s.jb.suppressed)
        assertEquals(1, r.focus.count(Purpose.CALL))
        assertTrue("the call keeps its focus", r.focus.held(Purpose.CALL))
        assertEquals(0, r.focus.count(Purpose.NAV))
    }

    /** cp8 18:32:42.836: a 101 inside a call (T100 43.529, phone=0 44.124, T101 44.212). */
    private fun inCall(mode: NavDuringVoice?): VoiceStream {
        val r = VoiceRig(mode)
        r.modes(phone = true)
        r.begin(0x20, 100, VoiceLatency.AT_TELEPHONY)
        val call = Feeder(r, 0x20, 100, 1, 16000, 1, 30)
        r.run(500) { call.tick(it) }
        r.begin(0x21, 101, VoiceLatency.AT_DEFAULT, 48000, 2)       // 42.836
        val s = r.stream(0x21)!!
        val n = nav(r, 0x21)
        r.run(693) { call.tick(it); n.tick(it) }                    // → 43.529
        call.on = false; r.end(0x20, call.k.toLong())
        r.run(595) { n.tick(it) }                                   // → 44.124
        r.modes(phone = false)
        r.run(88) { n.tick(it) }                                    // → 44.212
        n.on = false; r.end(0x21, n.k.toLong())
        r.run(600)
        assertTrue(r.log.has("identity OK"))
        return s
    }

    @Test fun fixture183242_inCall_dropSuppressesAllOfItEvenAfterPhone0() {
        val s = inCall(NavDuringVoice.DROP)
        assertEquals("DROP: all of it suppressed (${s.jb.summary()})", 0L, s.jb.played)
        assertTrue(s.jb.suppressed > 100)
    }

    @Test fun fixture183242_inCall_playLiveIsTheDefaultAndPlaysIt() {
        val s = inCall(null)
        assertEquals(0L, s.jb.suppressed)
        assertTrue(s.jb.played > 100)
    }

    /** cp8 18:31:20.349: a 101 between streams, no duck; phone=1 at 22.292; T101 at 23.388. */
    private fun betweenThenCall(mode: NavDuringVoice?): Pair<VoiceStream, Long> {
        val r = VoiceRig(mode)
        r.begin(0x30, 101, VoiceLatency.AT_DEFAULT, 48000, 2)
        val s = r.stream(0x30)!!
        val n = nav(r, 0x30)
        r.run(1_943) { n.tick(it) }                                 // → 22.292
        val playedBefore = s.jb.played
        r.modes(phone = true)
        r.run(1_096) { n.tick(it) }                                 // → 23.388
        n.on = false; r.end(0x30, n.k.toLong())
        r.run(400)
        return s to playedBefore
    }

    @Test fun fixture183120_dropCutsAtPhone1() {
        val (s, before) = betweenThenCall(NavDuringVoice.DROP)
        assertTrue(before > 150)
        assertTrue("played only up to the onset (+ the AUs already pulled): ${s.jb.summary()}", s.jb.played - before <= 12)
        assertTrue("then suppressed to T101", s.jb.suppressed >= 95)
    }

    @Test fun fixture183120_playLivePlaysToT101() {
        val (s, _) = betweenThenCall(null)
        assertEquals(0L, s.jb.suppressed)
        assertTrue(s.jb.played > 290)
    }

    /** cp8 18:32:12.015: no duck; Siri BEGIN 17.144; T101 19.449 (≈ 2.3 s). */
    private fun thenSiri(mode: NavDuringVoice?): Pair<VoiceStream, Long> {
        val r = VoiceRig(mode)
        r.begin(0x40, 101, VoiceLatency.AT_DEFAULT, 48000, 2)
        val s = r.stream(0x40)!!
        val n = nav(r, 0x40)
        r.run(5_129) { n.tick(it) }                                 // → 17.144
        val before = s.jb.played
        r.begin(0x41, 100, VoiceLatency.AT_SPEECH)
        val siri = Feeder(r, 0x41, 100, 2, 16000, 1, 30)
        r.run(2_305) { n.tick(it); siri.tick(it) }                  // → 19.449
        n.on = false; r.end(0x40, n.k.toLong())
        r.run(400) { siri.tick(it) }
        return s to before
    }

    @Test fun fixture183212_dropCutsAtTheSiriBegin() {
        val (s, before) = thenSiri(NavDuringVoice.DROP)
        assertTrue(s.jb.played - before <= 12)
        assertTrue("≈ 2.3 s suppressed (+ the ≤ target buffered at the onset, whose playout is after it): ${s.jb.suppressed}", s.jb.suppressed in 215..250)
    }

    @Test fun fixture183212_playLivePlaysThroughSiri() {
        val (s, _) = thenSiri(null)
        assertEquals(0L, s.jb.suppressed)
    }

    /**
     * Review round 1 (MAJOR), the default: on unduck media goes back to 1.0 at once, as Cinemo ramps
     * its own sink (T1 L82-84); the GM platform duck carries the tail until the nav stream ends, so a
     * software hold would stack on it. Nothing more is ramped at the END.
     */
    @Test fun unduckRestoresAtOnceByDefault() {
        assertFalse("captures support no software hold", VoiceLatency.NAV_UNDUCK_HOLD)
        val r = VoiceRig(navDuckLift = false)
        r.duckCmd(true); r.run(60)
        r.begin(0x51, 101, VoiceLatency.AT_DEFAULT, 48000, 2)
        val n = nav(r, 0x51)
        r.run(4_300) { n.tick(it) }
        r.duckCmd(false)
        r.run(10) { n.tick(it) }
        assertEquals(listOf(1f), r.duck.ramps.map { it.target })
        r.run(1_880) { n.tick(it) }
        n.on = false; r.end(0x51, n.k.toLong()); r.run(500)
        assertEquals("nothing at the END", listOf(1f), r.duck.ramps.map { it.target })
    }

    /** NAV_UNDUCK_HOLD = true (bench B-V3-2 only): unduck with the stream live HOLDS media until its END. */
    @Test fun unduckHoldsUntilTheNavStreamEnds() {
        val r = VoiceRig(navUnduckHold = true, navDuckLift = false)
        r.duckCmd(true)
        r.run(60)
        r.begin(0x50, 101, VoiceLatency.AT_DEFAULT, 48000, 2)
        val n = nav(r, 0x50)
        r.run(4_300) { n.tick(it) }
        val unduckAt = r.clock.us
        r.duckCmd(false)
        r.run(1_880) { n.tick(it) }                                  // T1: TEARDOWN +1.87-1.90 s after unduck
        assertEquals("held, not restored, while the stream lives", listOf(VoiceLatency.NAV_HOLD_LEVEL), r.duck.ramps.map { it.target })
        assertEquals("the executor stamp is the unduck's arrival", unduckAt, r.duck.ramps[0].stampUs)
        n.on = false; r.end(0x50, n.k.toLong())
        r.run(500)
        assertEquals(listOf(VoiceLatency.NAV_HOLD_LEVEL, 1f), r.duck.ramps.map { it.target })
        assertTrue(r.duck.ramps[1].why.contains("ended after its unduck"))
        assertEquals(0, r.focus.count(Purpose.NAV))
    }

    @Test fun unduckWithNoLiveNavStreamRestoresAtOnce() {
        val r = VoiceRig(navDuckLift = false)
        r.duckCmd(true); r.run(10)
        r.duckCmd(false); r.run(10)
        assertEquals(listOf(1f), r.duck.ramps.map { it.target })
    }

    /** duck → SETUP → TEARDOWN with no unduck: restored at that END (a lost unduck, W). */
    @Test fun lostUnduckIsRestoredAtTheBoundEnd() {
        val r = VoiceRig(navDuckLift = false)
        r.duckCmd(true); r.run(60)
        r.begin(0x60, 101, VoiceLatency.AT_DEFAULT, 48000, 2)
        val n = nav(r, 0x60)
        r.run(2_000) { n.tick(it) }
        n.on = false; r.end(0x60, n.k.toLong())
        r.run(500)
        assertEquals(1, r.duck.restores.size)
        assertTrue(r.log.lines.any { it.startsWith("W ") && it.contains("unduckAudio was lost") })
    }

    /** Back-to-back on one 101 (unduck 18:20:32.469, duck 33.808): bound to it, nothing restored in between. */
    @Test fun backToBackDuckOnALive101IsBoundToIt() {
        val r = VoiceRig(navUnduckHold = true, navDuckLift = false)
        r.duckCmd(true); r.run(60)
        r.begin(0x70, 101, VoiceLatency.AT_DEFAULT, 48000, 2)
        val n = nav(r, 0x70)
        r.run(3_000) { n.tick(it) }
        r.duckCmd(false)
        r.run(1_339) { n.tick(it) }
        r.duckCmd(true)
        r.run(10) { n.tick(it) }
        assertEquals(0x70L, r.engines.getValue(Purpose.NAV).duckBoundScid)
        assertTrue("no restore between the unduck and the next duck", r.duck.restores.isEmpty())
        r.run(2_000) { n.tick(it) }
        r.duckCmd(false)
        r.run(500) { n.tick(it) }
        n.on = false; r.end(0x70, n.k.toLong()); r.run(400)
        assertEquals(1, r.duck.restores.size)
    }

    /** cp8 18:33:19.640 / .683 / .931: a 27 s 101 superseded twice — no restore from a superseded scid. */
    @Test fun supersededLong101RestoresNothingAndTheHoldMovesOn() {
        val r = VoiceRig(navUnduckHold = true, navDuckLift = false)
        r.duckCmd(true); r.run(60)
        r.begin(0x80, 101, VoiceLatency.AT_DEFAULT, 48000, 2)
        val a = nav(r, 0x80) { it < 200 }
        r.run(2_000) { a.tick(it) }
        r.duckCmd(false)                                             // the hold binds to 0x80
        r.run(25_000) { a.tick(it) }                                 // the 27 s silent tail
        r.begin(0x81, 101, VoiceLatency.AT_DEFAULT, 48000, 2)        // .640
        a.on = false; r.end(0x80, a.k.toLong(), VoiceFrame.END_SUPERSEDED)
        val b = nav(r, 0x81) { false }
        r.run(43) { b.tick(it) }
        r.begin(0x82, 101, VoiceLatency.AT_DEFAULT, 48000, 2)        // .683
        b.on = false; r.end(0x81, b.k.toLong(), VoiceFrame.END_SUPERSEDED)
        val c = nav(r, 0x82) { false }
        r.run(248) { c.tick(it) }                                    // .931
        assertTrue("no restore from a superseded scid", r.duck.restores.isEmpty())
        c.on = false; r.end(0x82, c.k.toLong())
        r.run(400)
        assertEquals("restored once, at the last END", 1, r.duck.restores.size)
        assertEquals(0, r.focus.count(Purpose.NAV))
    }

    /** DROP rule (a): a 101 that began while a call was live stays suppressed until a duckAudio with nothing live. */
    @Test fun dropLatchIsLiftedOnlyByADuckWhenNotSuppressed() {
        val r = VoiceRig(NavDuringVoice.DROP)
        r.modes(phone = true)
        r.begin(0x90, 101, VoiceLatency.AT_DEFAULT, 48000, 2)
        val s = r.stream(0x90)!!
        val n = nav(r, 0x90)
        r.run(500) { n.tick(it) }
        r.duckCmd(true); r.run(300) { n.tick(it) }                  // while suppressed: no lift
        assertEquals(0L, s.jb.played)
        r.modes(phone = false)
        r.run(300) { n.tick(it) }
        assertEquals("still latched after phone=0", 0L, s.jb.played)
        r.duckCmd(true); r.run(500) { n.tick(it) }
        assertTrue("a duck with nothing live lifts it", s.jb.played > 30)
    }

    /**
     * Owner 2026-10-08 (in-prompt duck stacking): once the nav track plays (bus1 active, the GM platform
     * duck engaged — cp12 5-20 ms after each activation), our software duck lifts, so media sits at the
     * platform level (~-14..-16 dB) like stock, not ~-30 dB. The duck itself still lands BEFORE the bus
     * wakes (duckAudio ~60 ms before the AltAudio SETUP, T1), and a back-to-back duck on the playing 101
     * is lifted again.
     */
    @Test fun ourDuckLiftsOnceTheNavTrackPlays() {
        assertTrue(VoiceLatency.NAV_DUCK_LIFT_ON_BUS)
        val r = VoiceRig()
        r.factory.wakeDelayMs = 200
        r.duckCmd(true); r.run(60)                                // VR applied 0.2 directly (not in FakeDuck)
        r.begin(0x60, 101, VoiceLatency.AT_DEFAULT, 48000, 2)
        val n = nav(r, 0x60)
        r.run(150) { n.tick(it) }
        assertTrue("no lift while the bus is still waking", r.duck.ramps.isEmpty())
        r.run(400) { n.tick(it) }
        assertEquals(listOf(1f), r.duck.ramps.map { it.target })
        assertTrue(r.duck.ramps[0].why.startsWith("lift"))
        assertTrue(r.log.has("duck lifted"))
        r.duckCmd(true); r.run(20) { n.tick(it) }                 // back-to-back on the playing 101
        assertEquals(2, r.duck.ramps.count { it.why.startsWith("lift") })
        assertEquals(0, r.focus.count(VoiceLatency.Purpose.NAV))
    }
}
