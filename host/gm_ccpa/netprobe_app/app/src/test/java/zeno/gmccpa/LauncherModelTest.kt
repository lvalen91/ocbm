package zeno.gmccpa

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * plan v5.9 UI-2/UI-3: the launcher's pure model (LauncherModel.kt) at the head unit's real size,
 * 1132.8 x 673.6 dp (1416 x 842 px below GM's 118 px status bar at 200 dpi, font_scale 1.0).
 */
class LauncherModelTest {

    private val W = 1132.8f
    private val H = 673.6f

    // ---- UI-2: the "Adapter & logs" sheet ---------------------------------------------------------

    @Test fun sheetItemsAreExactlyTheFourInOrder() {
        assertEquals(listOf("RESTART_BT", "REBOOT", "FORGET_PHONE", "EXPORT"), SheetItem.values().map { it.name })
        assertEquals(listOf("Restart adapter Bluetooth", "Reboot adapter", "Forget pairing", "Export logs"),
            SheetItem.values().map { it.label })
    }

    @Test fun everySheetItemIsAConfirmedAdapterActionOrExport() {
        for (i in SheetItem.values()) {
            if (i == SheetItem.EXPORT) assertNull(i.box)
            else {
                val a = assertNotNullAnd(i.box)
                assertTrue(confirmBodyFor(a, false).isNotBlank())
                assertTrue(confirmBodyFor(a, true).isNotBlank())
            }
        }
        // Every BoxAction is reachable from the sheet (nothing orphaned by the fold).
        assertEquals(BoxAction.values().toSet(), SheetItem.values().mapNotNull { it.box }.toSet())
    }

    @Test fun noInfoScopeOrStatusAnywhere() {
        val names = BoxAction.values().map { it.name } + SheetItem.values().map { it.name } + SheetItem.values().map { it.label }
        for (bad in listOf("INFO", "SCOPE", "STATUS", "Box Info", "Log Scope", "Log Status", "Restart Wi-Fi", "RECOVER"))
            assertFalse(bad, names.any { it.equals(bad, ignoreCase = true) })
    }

    @Test fun everyBoxActionHasAConfirmTitle() {
        for (a in BoxAction.values()) assertTrue(a.name, a.confirmTitle.isNotBlank())
        assertEquals("Restart adapter Bluetooth?", BoxAction.RESTART_BT.confirmTitle)
    }

    @Test fun restartBtConfirmTextIsTheTracedOne() {
        val idle = confirmBodyFor(BoxAction.RESTART_BT, sessionLive = false)
        assertTrue(idle.startsWith("The adapter restarts its Bluetooth."))
        assertTrue(idle.contains("use Restart session instead"))
        assertFalse("D005: it does NOT re-apply credentials", idle.contains("only way to re-apply"))
        assertFalse("D005: no untraced figures", Regex("~\\d").containsMatchIn(idle))
        assertTrue(confirmBodyFor(BoxAction.RESTART_BT, sessionLive = true).startsWith("CarPlay is running and may stop.\n\n"))
    }

    private fun <T> assertNotNullAnd(v: T?): T { assertNotNull(v); return v!! }

    // ---- UI-3 C4: actionsFor ------------------------------------------------------------------------

    private val inFlight = listOf(LinkState.SEARCHING, LinkState.CLAIMING, LinkState.WAITING,
        LinkState.PHONE_DETECTED, LinkState.PAIRING, LinkState.STARTING)

    @Test fun actionsInvariantsForEveryInput() {
        for (s in LinkState.values()) for (missing in listOf(true, false)) for (busy in listOf(true, false)) {
            val a = actionsFor(s, missing, busy)
            val tag = "$s missing=$missing busy=$busy"
            assertEquals(tag, ActionId.ADAPTER_SHEET, a.all.last())
            assertEquals(tag, 1, a.all.count { it == ActionId.ADAPTER_SHEET })
            assertTrue("$tag: at most one filled primary", a.all.count { it == a.primary } <= 1)
            assertTrue("$tag: <= 2 action buttons + the entry", a.actionCount <= 2)
            if (s == LinkState.LIVE && !busy) assertNull("$tag: LIVE has no primary (R11)", a.primary)
            val restingNotBusy = (s == LinkState.IDLE || s == LinkState.STOPPED) && !busy
            assertEquals("$tag: Stop everywhere but READY/STOPPED", !restingNotBusy, a.all.contains(ActionId.STOP))
            assertEquals(tag, a.primary == ActionId.RESTARTING, a.primaryDimmed)
        }
    }

    @Test fun actionsTableRows() {
        for (s in listOf(LinkState.IDLE, LinkState.STOPPED)) {
            assertEquals(Actions(ActionId.SET_HOTSPOT, false, listOf(ActionId.ADAPTER_SHEET)), actionsFor(s, true, false))
            assertEquals(Actions(ActionId.START, false, listOf(ActionId.ADAPTER_SHEET)), actionsFor(s, false, false))
        }
        for (s in inFlight + LinkState.LIVE) for (m in listOf(true, false))
            assertEquals("$s", Actions(null, false, listOf(ActionId.RESTART, ActionId.STOP, ActionId.ADAPTER_SHEET)), actionsFor(s, m, false))
        assertEquals(Actions(ActionId.RESTART, false, listOf(ActionId.STOP, ActionId.ADAPTER_SHEET)), actionsFor(LinkState.FAILED, false, false))
        val restarting = Actions(ActionId.RESTARTING, true, listOf(ActionId.STOP, ActionId.ADAPTER_SHEET))
        assertEquals(restarting, actionsFor(LinkState.RESTARTING, false, false))
        for (s in LinkState.values()) assertEquals("$s busy", restarting, actionsFor(s, true, true))
    }

    /** R14: a credentials change alone (no state change) recomputes the primary. */
    @Test fun trackerRecomputesOnCredentialsAlone() {
        val t = ActionsTracker()
        t.onState(LinkState.STOPPED)
        assertEquals(ActionId.SET_HOTSPOT, t.onCredentials(missing = true).primary)
        assertEquals(ActionId.START, t.onCredentials(missing = false).primary)
        assertEquals(ActionId.RESTARTING, t.onRestartBusy(true).primary)
        assertEquals(ActionId.START, t.onRestartBusy(false).primary)
        assertEquals(ActionId.START, t.current().primary)
    }

    // ---- UI-3 C6: driverLine + the token regex ----------------------------------------------------------

    @Test fun everyDriverLineIsShortAndClean() {
        for (s in LinkState.values()) for (m in listOf(true, false))
            for (r in FailReason.values().toList() + listOf<FailReason?>(null)) {
                val line = driverLine(s, m, r)
                assertTrue("$s/$m/$r: '$line' (${line.length})", line.isNotBlank() && line.length <= DRIVER_LINE_MAX)
                assertFalse("$s/$m/$r: '$line' has an engineering token", ENGINEERING_TOKENS.containsMatchIn(line))
            }
    }

    @Test fun driverLineTable() {
        assertEquals("Press Start to connect the iPhone.", driverLine(LinkState.IDLE, false, null))
        assertEquals("Set the vehicle hotspot to start.", driverLine(LinkState.IDLE, true, null))
        assertEquals("Set the vehicle hotspot to start.", driverLine(LinkState.STOPPED, true, null))
        assertEquals("Stopped. Press Start to reconnect.", driverLine(LinkState.STOPPED, false, null))
        assertEquals("Connection failed. Press Restart session.", driverLine(LinkState.FAILED, false, null))
        assertEquals("The iPhone did not reconnect. Reconnect from the iPhone, or Restart session.",
            driverLine(LinkState.FAILED, false, FailReason.STALLED_PHONE))
        assertEquals("The adapter did not come back. Press Restart session, or unplug and replug it.",
            driverLine(LinkState.FAILED, false, FailReason.STALLED_ADAPTER))
        // Every FAILED reason has its own sentence.
        assertEquals(FailReason.values().size, FailReason.values().map { driverLine(LinkState.FAILED, false, it) }.toSet().size)
    }

    @Test fun tokenRegexVectors() {
        for (p in listOf("CT_SUBSCRIBE sent", "MGMT_REBOOT", "reply 0x5703", "wait 20ms", "took 5 ms",
                "did not answer HELLO", "MGMT_GET_INFO", "ocbmd restarted", "hostapd.conf", "btd died", "new nonce",
                // after `_` or a digit, where `\b` finds no boundary (why the look-behind, R9)
                "box_ocbmd died", "id_0x5703", "rc=1MGMT_REBOOT"))
            assertTrue("must match: $p", ENGINEERING_TOKENS.containsMatchIn(p))
        for (n in listOf("Press Start to connect the iPhone.", "Wait two minutes, then Restart session.",
                "Keep its Bluetooth and Wi-Fi on.", "announce", "Adapter Bluetooth restarting",
                // a token inside a word is not a token (the look-behind's other half)
                "OTHELLO", "subtdx"))
            assertFalse("must not match: $n", ENGINEERING_TOKENS.containsMatchIn(n))
    }

    // ---- UI-3 C3/C9: metrics --------------------------------------------------------------------------

    private fun floors(m: Metrics) {
        assertTrue("targets >= 76 dp", m.minTargetDp >= 76f)
        assertEquals(94f, m.actionH)
        assertTrue("gaps >= 25 dp", m.actionGap >= 25f && m.pairsWithGap >= 25f)
        assertTrue("text >= 24 sp (${m.minTextSp})", m.minTextSp >= 24f)
    }

    @Test fun headUnitIsTwoPaneAndNothingScrolls() {
        val m = metrics(W, H)
        assertEquals(Layout.TWO_PANE, m.layout)
        floors(m)
        assertEquals(623.6f, m.available, 0.01f)
        assertEquals(600.8f, m.leftPane, 0.5f)
        assertEquals(400f, m.rightPane)
        // The spec's bound (3 actions) and the real worst case (2, after R11).
        assertTrue("right sum ${m.rightSum(3)}", m.rightSum(3) <= m.available)
        assertEquals(433f, m.rightSum(3), 0.01f)
        assertEquals(314f, m.rightSum(2), 0.01f)
        for (s in LinkState.values()) for (mi in listOf(true, false)) for (b in listOf(true, false))
            assertTrue(m.rightSum(actionsFor(s, mi, b).actionCount) <= m.available)
        // R13: the left sum with the pairing code (and the V5 rekey hint) visible — the worst case.
        assertEquals(352f, m.leftSum(pairing = false, hint = false), 0.01f)
        assertEquals(446f, m.leftSum(pairing = true, hint = false), 0.01f)
        assertTrue("left worst ${m.leftSum(true, true)}", m.leftSum(pairing = true, hint = true) <= m.available)
        // Two-pane widths add up to the panel.
        assertEquals(W, m.padH * 2 + m.leftPane + m.gutter + m.rightPane, 0.01f)
    }

    /** C8 width estimate: "RESTART SESSION" at 30 sp + 2 x 48 dp padding fits the 400 dp pane. */
    @Test fun widestActionLabelFitsTheRightPane() {
        val m = metrics(W, H)
        val longest = ActionId.values().filter { it != ActionId.ADAPTER_SHEET }.maxOf { it.label.length }
        // ~0.56 em per upper-case glyph (the plan's 252 dp for 15 glyphs at 30 sp).
        assertTrue(longest * 0.56f * m.actionSp + 2 * m.actionPadH <= m.rightPane)
    }

    @Test fun smallPanelIsSingleColumnWithTheFloors() {
        val m = metrics(900f, 600f)
        assertEquals(Layout.SINGLE, m.layout)
        floors(m)
        assertEquals(900f - 2 * m.padH, m.leftPane, 0.01f)
    }

    @Test fun largePanelIsTwoPaneWithTheFloors() {
        val m = metrics(1920f, 768f)
        assertEquals(Layout.TWO_PANE, m.layout)
        floors(m)
        assertTrue(m.leftSum(true, true) <= m.available)
        assertTrue(m.rightSum(3) <= m.available)
    }

    @Test fun portraitIsSingleEvenWhenWide() {
        assertEquals(Layout.SINGLE, metrics(1100f, 1400f).layout)
        assertEquals(Layout.SINGLE, metrics(999f, 500f).layout)
    }

    // ---- UI-3 C6: TransientLine ---------------------------------------------------------------------

    @Test fun transientVisibleFor15sThenGone() {
        val t = TransientLine()
        t.show("Adapter Bluetooth restarting", 1_000L)
        assertEquals("Adapter Bluetooth restarting", t.current(1_000L + 14_900L))
        assertNull(t.current(1_000L + 15_000L))
        assertEquals(100L, t.remainingMs(1_000L + 14_900L))
        assertEquals(0L, t.remainingMs(1_000L + 15_000L))
    }

    @Test fun transientCancelledByAStateChange() {
        val t = TransientLine()
        t.show("x", 0L)
        t.cancel()
        assertNull(t.current(1L))
    }

    @Test fun transientReplayedOnAttachOnlyWhileYoung() {
        assertTrue(TransientLine.replayable(shownAt = 10_000L, nowMs = 24_999L))
        assertFalse(TransientLine.replayable(shownAt = 10_000L, nowMs = 25_000L))
        assertFalse("a clock that went backwards is not young", TransientLine.replayable(shownAt = 10_000L, nowMs = 9_999L))
    }

    // ---- C8 contrast (WCAG relative luminance), recomputed --------------------------------------------

    private fun lum(c: Int): Double {
        fun ch(v: Int): Double { val s = v / 255.0; return if (s <= 0.03928) s / 12.92 else Math.pow((s + 0.055) / 1.055, 2.4) }
        return 0.2126 * ch((c shr 16) and 0xFF) + 0.7152 * ch((c shr 8) and 0xFF) + 0.0722 * ch(c and 0xFF)
    }
    private fun contrast(a: Int, b: Int): Double { val x = lum(a); val y = lum(b); return (maxOf(x, y) + 0.05) / (minOf(x, y) + 0.05) }

    @Test fun strokePassesTheNonTextFloorAndLineDoesNot() {
        assertTrue(contrast(Palette.STROKE, Palette.BG) >= 3.0)
        assertFalse(contrast(Palette.LINE, Palette.BG) >= 3.0)
        assertEquals(4.07, contrast(Palette.STROKE, Palette.BG), 0.01)
        assertEquals(17.17, contrast(Palette.TEXT, Palette.BG), 0.01)
    }
}
