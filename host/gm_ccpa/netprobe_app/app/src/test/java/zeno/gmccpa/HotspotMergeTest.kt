package zeno.gmccpa

import org.junit.Assert.assertEquals
import org.junit.Test

/** Plan v5.8 V7 / Q019: a blank field in the credentials dialog keeps the running value. */
class HotspotMergeTest {

    @Test fun blankKeepsPrevious() {
        assertEquals("OldNet", HotspotMerge.merge("OldNet", ""))
        assertEquals("OldNet", HotspotMerge.merge("OldNet", "   "))
        assertEquals("OldNet", HotspotMerge.merge("OldNet", "\t\n"))
        assertEquals("OldNet", HotspotMerge.merge("OldNet", null))
    }

    @Test fun typedReplacesPreviousTrimmed() {
        assertEquals("NewNet", HotspotMerge.merge("OldNet", "NewNet"))
        assertEquals("New Net", HotspotMerge.merge("OldNet", "  New Net  "))
    }

    @Test fun blankPreviousStaysBlankOnBlankTyped() {
        // OD-7A: a never-saved field stays "missing" (autoStart keeps prompting) rather than turning into whitespace.
        assertEquals("", HotspotMerge.merge("", "   "))
    }

    @Test fun interiorWhitespaceIsContent() {
        // An SSID with inner spaces is a value, not a blank; only the ends are trimmed.
        assertEquals("a b", HotspotMerge.merge("x", " a b "))
    }
}
