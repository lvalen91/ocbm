package zeno.gmccpa.av

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Ported for plan v5.8 (it_v57_U10's JUnit half). The U10 NAV park / re-acquire / Siri-guard / idle
 * exemption rules are gone: owner 2026-10-07, OD-8C = YES — NAV takes no focus (stock), so the plan's
 * NONE path deletes every U10 focus check. Their route rules moved to V4a's latch (RouteLatchTest).
 * What remains here: the head-drain rule (END play-out) and the assistant edge that replaced the
 * bounded control-plane Siri signal.
 */
class VoiceIdleRulesNavTest {

    @Test fun headDrained_withinOnePeriod() {
        val period = 960L
        assertTrue(VoiceIdleRules.headDrained(19_200, 19_200, period))
        assertTrue(VoiceIdleRules.headDrained(19_200 - period, 19_200, period))
        assertFalse(VoiceIdleRules.headDrained(19_200 - period - 1, 19_200, period))
        assertFalse(VoiceIdleRules.headDrained(0, 19_200, period))
        assertTrue("nothing written since the flush", VoiceIdleRules.headDrained(0, 0, period))
    }

    /** onAssistant follows the speechRecognition stream's BEGIN..END, with the modes START as an early start. */
    @Test fun assistantLive_streamOrAnEarlyStartWithNoBeginYet() {
        val now = 2_000_000L
        val lead = 3_000L
        assertTrue("stream live", VoiceIdleRules.assistantLive(now, true, false, 0L, lead, now - 10))
        assertTrue("START 300 ms before the SETUP, no BEGIN yet",
            VoiceIdleRules.assistantLive(now, false, true, now - 300, lead, now - 60_000))
        assertFalse("START with no stream behind it expires at the lead",
            VoiceIdleRules.assistantLive(now, false, true, now - lead, lead, now - 60_000))
        assertFalse("after the stream's END the START no longer counts (the END ends it)",
            VoiceIdleRules.assistantLive(now, false, true, now - 1_000, lead, now - 900))
        assertFalse("speech mode -1", VoiceIdleRules.assistantLive(now, false, false, now - 10, lead, 0L))
    }
}
