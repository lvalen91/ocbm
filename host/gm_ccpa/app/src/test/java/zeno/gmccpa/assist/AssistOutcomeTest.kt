package zeno.gmccpa.assist

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import zeno.gmccpa.pair.NativeCore

/**
 * Steering-wheel Voice -> Siri log wording (2026-10-26). The bench reads these lines to tell "not
 * connected" from "sent" from a half-sent tap, so each native result must map to its own wording and
 * only a full buttondown+buttonup may read as sent.
 */
class AssistOutcomeTest {

    @Test fun onlyBothHalvesReadAsSent() {
        assertTrue(AssistOutcome.describeTap(NativeCore.SiriTap.BOTH).startsWith("sent "))
        for (c in listOf(-3, -2, -1, 0, 1, 2, 7)) {
            assertTrue("code $c", !AssistOutcome.describeTap(c).startsWith("sent"))
        }
    }

    @Test fun notConnectedIsDistinctFromFailure() {
        assertTrue(AssistOutcome.describeTap(NativeCore.SiriTap.NO_CORE).contains("not connected"))
        assertTrue(AssistOutcome.describeTap(NativeCore.SiriTap.NOT_STARTED).contains("not connected"))
        assertTrue(AssistOutcome.describeTap(0).startsWith("FAILED"))
        assertTrue(AssistOutcome.describeTap(1).startsWith("PARTIAL"))
    }

    @Test fun wheelFlagsDecode() {
        // 0x20 from GMCarInputService | 0x1|0x2 OR-ed in by showSessionForActiveService.
        assertEquals("0x23 (WITH_ASSIST|WITH_SCREENSHOT|SOURCE_PUSH_TO_TALK)", AssistOutcome.describeFlags(0x23))
        assertEquals("0x0", AssistOutcome.describeFlags(0))
        assertEquals("0x120 (SOURCE_PUSH_TO_TALK|0x100)", AssistOutcome.describeFlags(0x120))
    }
}
