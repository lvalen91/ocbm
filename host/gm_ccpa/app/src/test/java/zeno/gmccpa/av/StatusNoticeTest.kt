package zeno.gmccpa.av

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import zeno.gmccpa.LinkState
import zeno.gmccpa.StallCause

/** plan v5.9 D6 / OD-9E: the one notification's texts, and its no-PII rule. */
class StatusNoticeTest {
    @Test fun table() {
        assertEquals(Notice("GM CCPA — adapter connected", "Waiting for iPhone"), StatusNotice.of(LinkState.WAITING, null, false))
        assertEquals(Notice("GM CCPA — adapter connected", "Pairing with iPhone"), StatusNotice.of(LinkState.PAIRING, null, false))
        assertEquals(Notice("CarPlay active", "Wireless CarPlay session running"), StatusNotice.of(LinkState.LIVE, null, false))
        for (c in StallCause.values())
            assertEquals(Notice("CarPlay interrupted", "Recovering connection"), StatusNotice.of(LinkState.FAILED, c, false))
        assertEquals(Notice("GM CCPA — adapter reconnecting", "Reconnecting to the adapter"), StatusNotice.of(LinkState.WAITING, null, true))
        assertEquals(Notice("GM CCPA", "Connecting to the adapter"), StatusNotice.of(LinkState.CLAIMING, null, false))
        assertEquals(Notice("GM CCPA", "Connecting to the adapter"), StatusNotice.of(LinkState.SEARCHING, null, false))
        assertEquals(Notice("GM CCPA", "Adapter connected"), StatusNotice.of(LinkState.PHONE_DETECTED, null, false))
        assertEquals(Notice("GM CCPA", "Adapter connected"), StatusNotice.of(LinkState.STARTING, null, false))
        // D-F6 (2026-10-08): these four no longer read "Adapter connected".
        assertEquals(Notice("GM CCPA", "Starting"), StatusNotice.of(null, null, false))
        assertEquals(Notice("GM CCPA", "Starting"), StatusNotice.of(LinkState.IDLE, null, false))
        assertEquals(Notice("GM CCPA — CarPlay problem", "Open GM CCPA for details"), StatusNotice.of(LinkState.FAILED, null, false))
        assertEquals(Notice("GM CCPA", "Restarting the session"), StatusNotice.of(LinkState.RESTARTING, null, false))
        assertEquals(Notice("GM CCPA", "Stopped"), StatusNotice.of(LinkState.STOPPED, null, false))
        // A relink in progress outranks a plain FAILED (the relink text is the actionable one).
        assertEquals("GM CCPA — adapter reconnecting", StatusNotice.of(LinkState.FAILED, null, true).title)
    }

    @Test fun liveWinsOverARelinkAndStallWinsOverARelink() {
        assertEquals("CarPlay active", StatusNotice.of(LinkState.LIVE, null, true).title)
        assertEquals("CarPlay interrupted", StatusNotice.of(LinkState.FAILED, StallCause.ADAPTER, true).title)
    }

    /** No field can carry a name or an address: the inputs are a state, a cause and a boolean. */
    @Test fun noPii() {
        val all = (LinkState.values().toList() + listOf<LinkState?>(null)).flatMap { s ->
            (StallCause.values().toList() + listOf<StallCause?>(null)).flatMap { c -> listOf(true, false).map { StatusNotice.of(s, c, it) } }
        }
        for (n in all) {
            assertFalse(n.toString(), Regex("[0-9A-Fa-f]{2}(:[0-9A-Fa-f]{2}){5}").containsMatchIn(n.title + n.text))
            assertTrue(n.toString(), n.title.length <= 40 && n.text.length <= 40)
        }
        // 7 until 2026-10-08; +4 for D-F6 (problem, restarting, stopped, starting). Deliberate.
        assertEquals("a closed set of texts", 11, all.toSet().size)
    }
}
