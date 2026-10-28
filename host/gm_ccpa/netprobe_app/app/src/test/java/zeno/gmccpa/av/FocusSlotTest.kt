package zeno.gmccpa.av

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import zeno.gmccpa.av.VoiceLatency.Purpose

/**
 * Plan v5.8 V2 — CALL / ASSISTANT / ALERT focus (stock SD:CarPlayAudioManager.java). NAV has no
 * slot (owner OD-8C = YES). Pure slot tests plus engine scenarios on the rig in VoiceStreamsTest.kt.
 */
class FocusSlotTest {

    @Test fun grantedThenReleased_andTheGuardMakesOneRequest() {
        val clock = FakeClock()
        val port = FakeFocus(clock)
        val slot = FocusSlot(Purpose.CALL, port) { _, _ -> }
        assertEquals(FocusSlot.Outcome.GRANTED, slot.requestIfNotHeld())
        assertEquals(FocusSlot.Outcome.ALREADY_HELD, slot.requestIfNotHeld())
        assertEquals(1, port.count(Purpose.CALL))
        assertTrue(slot.release("END"))
        assertFalse(slot.release("again"))
        assertEquals("END", slot.lastReleaseCause)
        assertEquals(1, port.abandons.size)
    }

    @Test fun failedHoldsNothingAndAStaleListenerIsIgnored() {
        val clock = FakeClock()
        val port = FakeFocus(clock)
        val seen = ArrayList<Int>()
        val slot = FocusSlot(Purpose.ALERT, port) { _, c -> seen.add(c) }
        port.next(Purpose.ALERT, FocusCodes.REQUEST_FAILED)
        assertEquals(FocusSlot.Outcome.FAILED, slot.requestIfNotHeld())
        assertFalse(slot.held)
        assertTrue("nothing to abandon", port.abandons.isEmpty())
        val stale = port.last(Purpose.ALERT).listener
        assertEquals(FocusSlot.Outcome.GRANTED, slot.requestIfNotHeld())
        stale(FocusCodes.LOSS)
        assertTrue("a refused request's late callback is dropped", seen.isEmpty())
        port.last(Purpose.ALERT).listener(FocusCodes.LOSS_TRANSIENT)
        assertEquals(listOf(FocusCodes.LOSS_TRANSIENT), seen)
        assertEquals(FocusCodes.LOSS_TRANSIENT, slot.state)
    }

    @Test fun stopRules() {
        assertEquals(VoiceIdleRules.StopRule.RELEASE, VoiceIdleRules.focusStopRule(Purpose.CALL, false))
        assertEquals(VoiceIdleRules.StopRule.HOLD_UNTIL_PHONE_IDLE, VoiceIdleRules.focusStopRule(Purpose.CALL, true))
        assertEquals(VoiceIdleRules.StopRule.HOLD_UNTIL_PHONE_IDLE, VoiceIdleRules.focusStopRule(Purpose.ALERT, true))
        assertEquals(VoiceIdleRules.StopRule.HANDOVER, VoiceIdleRules.focusStopRule(Purpose.ASSISTANT, true))
        assertEquals(VoiceIdleRules.StopRule.RELEASE, VoiceIdleRules.focusStopRule(Purpose.ASSISTANT, false))
        assertTrue(VoiceIdleRules.focusOutcome(FocusCodes.REQUEST_GRANTED))
        assertFalse(VoiceIdleRules.focusOutcome(FocusCodes.REQUEST_FAILED))
        assertFalse(VoiceIdleRules.focusOutcome(FocusCodes.REQUEST_DELAYED))
    }

    /** Stock :793-795 / :998-1001: a refused stream is muted for its life — counted, never played, no retry. */
    @Test fun refusedStreamIsMutedForItsLife() {
        val r = VoiceRig()
        r.focus.next(Purpose.CALL, FocusCodes.REQUEST_FAILED)
        r.begin(0x10, 100, VoiceLatency.AT_TELEPHONY)
        val s = r.stream(0x10)!!
        val f = Feeder(r, 0x10, 100, 1, 16000, 1, 30)
        r.run(1_000) { f.tick(it) }
        f.on = false; r.end(0x10, f.k.toLong()); r.run(400)
        assertEquals(0L, s.jb.played)
        assertTrue(s.jb.refusedLost >= f.k - 1)
        assertEquals("no retry", 1, r.focus.count(Purpose.CALL))
        assertEquals(0L, r.factory.track(Purpose.CALL).written)
        assertTrue(r.boards.any { it.contains("REFUSED") })
        assertTrue(r.log.has("identity OK"))
    }

    @Test fun lossMutesAndGainResumesTheLiveStreamOnly() {
        val r = VoiceRig()
        r.begin(0x20, 100, VoiceLatency.AT_SPEECH)
        val s = r.stream(0x20)!!
        val f = Feeder(r, 0x20, 100, 2, 16000, 1, 30)
        r.run(500) { f.tick(it) }
        val listener = r.focus.last(Purpose.ASSISTANT).listener
        listener(FocusCodes.LOSS_TRANSIENT)
        r.run(300) { f.tick(it) }
        val t = r.factory.track(Purpose.ASSISTANT)
        assertFalse("muted: paused", t.playing)
        assertTrue(s.jb.refusedLost >= 8)
        listener(FocusCodes.GAIN)
        r.run(300) { f.tick(it) }
        assertTrue("resumed", t.playing)
        // After the stream: a GAIN with no live stream plays nothing.
        f.on = false; r.modes(phone = true); r.end(0x20, f.k.toLong()); r.run(500)
        val plays = t.plays
        r.modes(phone = true)
        listener(FocusCodes.GAIN); r.run(50)
        assertEquals(plays, t.plays)
    }

    /** Stock :751-756: an ASSISTANT BEGIN abandons a held CALL focus that has no live CALL stream. */
    @Test fun assistantBeginAbandonsAnIdleCall() {
        val r = VoiceRig()
        r.modes(phone = true)
        r.begin(0x30, 100, VoiceLatency.AT_TELEPHONY)
        val c = Feeder(r, 0x30, 100, 1, 16000, 1, 30)
        r.run(300) { c.tick(it) }
        c.on = false; r.end(0x30, c.k.toLong()); r.run(400)
        assertTrue("held (phone=1)", r.focus.held(Purpose.CALL))
        r.begin(0x31, 100, VoiceLatency.AT_SPEECH)
        r.run(10)
        assertFalse(r.focus.held(Purpose.CALL))
        assertTrue(r.focus.held(Purpose.ASSISTANT))
    }

    /** Siri-placed call (cp8 18:32:19.793 phone=1 / .931 Siri END / 20.765 CALL BEGIN): no media GAIN between. */
    @Test fun siriPlacedCallHandsTheSlotOverWithNoMediaGain() {
        val r = VoiceRig()
        r.begin(0x40, 100, VoiceLatency.AT_SPEECH)
        val siri = Feeder(r, 0x40, 100, 2, 16000, 1, 30)
        r.run(2_000) { siri.tick(it) }
        r.modes(phone = true)                                   // .793
        r.run(138) { siri.tick(it) }
        siri.on = false; r.end(0x40, siri.k.toLong())           // .931
        r.run(834)                                              // → 20.765
        assertTrue("kept for the handover", r.focus.held(Purpose.ASSISTANT))
        r.begin(0x41, 100, VoiceLatency.AT_TELEPHONY)
        r.run(10)
        assertTrue(r.focus.held(Purpose.CALL))
        assertFalse("released at the CALL BEGIN's GRANTED", r.focus.held(Purpose.ASSISTANT))
        assertTrue("media never got GAIN between Siri's END and the call", r.focus.mediaGains.isEmpty())
    }

    @Test fun phone1to0ReleasesAndTheBackstopOnlyLogs() {
        val r = VoiceRig()
        r.modes(phone = true)
        r.begin(0x50, 100, VoiceLatency.AT_TELEPHONY)
        val c = Feeder(r, 0x50, 100, 1, 16000, 1, 30)
        r.run(300) { c.tick(it) }
        c.on = false; r.end(0x50, c.k.toLong())
        r.run(6_000)
        assertTrue("log-only until U18: still held 6 s after END", r.focus.held(Purpose.CALL))
        assertTrue(VoiceIdleRules.callSlotBackstopLog(true, false, 10_000, 4_000, 5_000))
        assertFalse(VoiceIdleRules.callSlotBackstopLog(true, true, 10_000, 4_000, 5_000))
        assertFalse(VoiceIdleRules.callSlotBackstopLog(true, false, 8_000, 4_000, 5_000))
        r.modes(phone = false)
        r.run(10)
        assertFalse(r.focus.held(Purpose.CALL))
        assertEquals("phone 1→0", r.core.slots.getValue(Purpose.CALL).lastReleaseCause)
    }

    /** A CALL END after phone=0 releases at the END itself (the clean call: TEARDOWN .560 before phone=0 .836). */
    @Test fun callEndAfterPhone0ReleasesAtEnd() {
        val r = VoiceRig()
        r.modes(phone = true)
        r.begin(0x60, 100, VoiceLatency.AT_TELEPHONY)
        val c = Feeder(r, 0x60, 100, 1, 16000, 1, 30)
        r.run(1_000) { c.tick(it) }
        c.on = false; r.end(0x60, c.k.toLong())
        r.run(276)
        r.modes(phone = false)
        r.run(500)
        assertFalse(r.focus.held(Purpose.CALL))
        val cause = r.core.slots.getValue(Purpose.CALL).lastReleaseCause
        assertTrue("released by END or phone 1→0, never the stall backstop: $cause", cause == "END" || cause == "phone 1→0")
        assertEquals(1, r.focus.mediaGains.size)
    }

    /**
     * Review round 1: a GRANTED answer for a request that is no longer the current one (a second
     * request was made while it was in flight) must not be kept — its handle is abandoned, exactly one
     * request stays held.
     */
    @Test fun aGrantForASupersededRequestIsAbandoned() {
        val clock = FakeClock()
        val port = FakeFocus(clock)
        val slot = FocusSlot(Purpose.CALL, port) { _, _ -> }
        port.duringRequest = { assertEquals(FocusSlot.Outcome.GRANTED, slot.requestIfNotHeld()) }
        assertEquals(FocusSlot.Outcome.FAILED, slot.requestIfNotHeld())
        assertTrue(slot.held)
        assertEquals("the stale grant was abandoned", 1, port.abandons.size)
        assertEquals("one holder, not two (no leaked request)", 1, port.holderCount())
    }
}
