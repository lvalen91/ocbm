package zeno.gmccpa

/**
 * The launcher screen's pure model (plan v5.9 UI-2/UI-3): colours, states, adapter actions, the
 * "Adapter & logs" sheet, and — from UI-3 — the state-driven actions, the driver wording, the layout
 * metrics and the transient result line. No android.* here, so every rule is a tier-0 JUnit test
 * (LauncherModelTest). [LauncherUi] renders only what this file returns.
 */
object Palette {
    const val BG          = 0xFF0E1113.toInt()  // near-black; the dash is dark and stays dark
    const val LINE        = 0xFF2A3136.toInt()  // 1.44:1 on BG — decoration only, never a control edge
    /** Control outline (plan v5.9 C8, R21): 4.07:1 on BG, above the 3:1 non-text floor LINE fails. */
    const val STROKE      = 0xFF6B767C.toInt()
    const val TEXT        = 0xFFF2F4F5.toInt()
    const val TEXT_DIM    = 0xFF9AA4AA.toInt()
    // State colours. These carry the meaning at a glance — the colour is read first, the word second.
    // Since C8 they are used on the state DOT only; buttons are neutral.
    const val IDLE        = 0xFF8A9298.toInt()  // grey   — nothing is happening
    const val WORKING     = 0xFFE8B341.toInt()  // amber  — something is in flight (also: creds missing chip)
    const val PHONE       = 0xFF58A6FF.toInt()  // blue   — the phone is in the picture
    const val LIVE        = 0xFF4CC38A.toInt()  // green  — CarPlay is through
    const val FAILED      = 0xFFE5534B.toInt()  // red    — it stopped for a reason
}

/**
 * The states the launcher can show, in the order a session actually walks through them.
 *
 * The first four come from `SessionCore.autoStart`; [PHONE_DETECTED] and [PAIRING] come from the
 * box over `CH_CTRL` (`CT_SESSION_EVENT` / `CT_PAIRING_CODE`, see `OcbmClient.handleCtrl`); the rest
 * from the supervisor's phase (`SessionCore.linkStateFor`). Labels are short and upper-case because
 * this is the one thing that has to be readable at arm's length in a moving vehicle; the sentence
 * under it is [driverLine], and the engineering detail stays in logcat (`adb logcat -s NETPROBE`)
 * and the sheet's "Last message".
 */
enum class LinkState(val label: String, val color: Int) {
    IDLE          ("READY",              Palette.IDLE),
    SEARCHING     ("LOOKING FOR ADAPTER", Palette.WORKING),
    CLAIMING      ("CLAIMING ADAPTER",   Palette.WORKING),
    WAITING       ("WAITING FOR PHONE",  Palette.WORKING),
    PHONE_DETECTED("PHONE DETECTED",     Palette.PHONE),
    PAIRING       ("PAIRING",            Palette.PHONE),
    STARTING      ("CARPLAY STARTING",   Palette.PHONE),
    LIVE          ("CARPLAY RUNNING",    Palette.LIVE),
    /** Restart Session is tearing down and re-claiming. Amber: in flight, not failed, not idle. */
    RESTARTING    ("RESTARTING SESSION", Palette.WORKING),
    STOPPED       ("STOPPED",            Palette.IDLE),
    FAILED        ("FAILED",             Palette.FAILED),
}

/**
 * Things the app asks the adapter to do over `CH_MGMT` (0x0040) — `OcbmClient.mgmtAction`. Every one
 * is confirmed first (MainActivity, [confirmBodyFor]); none is undone by pressing Start again.
 *
 * Traced (plan v5.9 D005): [RESTART_BT] sends MGMT_RESTART_WIRELESS; ocbmd only touches a flag and
 * ACKs at once ("accepted", not "done"), and the box supervisor then takes the adapter's Bluetooth
 * (btd + the BT controller) down and, ~4 s later, back up. It does NOT re-apply changed hotspot
 * details — those reach the box only on a CT_SUBSCRIBE, i.e. Restart session. [REBOOT] is
 * MGMT_REBOOT. [FORGET_PHONE] is MGMT_FORGET_ALL plus this app's peer store (below).
 * (MGMT_GET_INFO stays automatic on every link-up; the "Box Info" button went in D002.)
 */
enum class BoxAction(val label: String, val confirmTitle: String) {
    RESTART_BT("Restart adapter Bluetooth", "Restart adapter Bluetooth?"),
    REBOOT("Reboot adapter", "Reboot adapter?"),
    /**
     * Clears the Bluetooth bond on the box AND this app's CarPlay pairing store, together.
     *
     * They must go together. The box's bond and the app's Ed25519 peer store are separate records of
     * the same relationship, and clearing one leaves the other claiming a pairing the phone no longer
     * has — a split brain that presents as pair-verify failing for reasons that look like broken
     * crypto. The old "Forget Phone" cleared only the box's half; so did the `ocbm_forget` verb until
     * plan v5.9 D020.
     */
    FORGET_PHONE("Forget pairing", "Forget pairing?"),
}

/**
 * The confirmation body for [a] (MainActivity shows it with [BoxAction.confirmTitle]). [sessionLive]
 * = `SessionHolder.cpRx?.sessionLive == true` at the press.
 */
fun confirmBodyFor(a: BoxAction, sessionLive: Boolean): String = when (a) {
    // D005 (owner-approved wording): "may stop" until B-UI-5 measures whether a live Wi-Fi session
    // survives MGMT_RESTART_WIRELESS (U59-3).
    BoxAction.RESTART_BT ->
        (if (sessionLive) "CarPlay is running and may stop.\n\n" else "") +
        "The adapter restarts its Bluetooth. The iPhone's Bluetooth link drops and the adapter is " +
        "back after a few seconds. Pairings are kept.\n\nUse this when the iPhone does not see the " +
        "adapter. To apply changed hotspot details, use Restart session instead."
    BoxAction.REBOOT ->
        "Reboot the adapter?\n\nAny live CarPlay session drops immediately and the box " +
        "needs a fresh claim afterwards."
    BoxAction.FORGET_PHONE ->
        (if (sessionLive) "A CarPlay session is RUNNING and this will end it.\n\n" else "") +
        "Clear the pairing on both the adapter and this app?\n\nYou must ALSO forget " +
        "this car on the iPhone — a one-sided pairing makes the next attempt fail in a " +
        "way that looks like broken encryption."
}

/**
 * The "Adapter & logs" sheet (plan v5.9 D004/C1), in display order. Each adapter item opens its
 * confirmation ([BoxAction]); [EXPORT] runs the export flow. This is the ONLY place the four
 * secondary controls exist — the launcher's old box and log rows went with the fold.
 */
enum class SheetItem(val label: String, val box: BoxAction?) {
    RESTART_BT(BoxAction.RESTART_BT.label, BoxAction.RESTART_BT),
    REBOOT(BoxAction.REBOOT.label, BoxAction.REBOOT),
    FORGET_PHONE(BoxAction.FORGET_PHONE.label, BoxAction.FORGET_PHONE),
    EXPORT("Export logs", null),
}

const val ADAPTER_SHEET_TITLE = "Adapter & logs"

// ---- UI-3 C4: state-driven actions -----------------------------------------------------------------

/** Every control the launcher's action column can show. Labels are rendered all-caps (R21). */
enum class ActionId(val label: String) {
    SET_HOTSPOT("Set hotspot"),
    START("Start"),
    RESTART("Restart session"),
    /** The restart in flight: dimmed to alpha 0.5 but still clickable, so a press shows the rejection line. */
    RESTARTING("Restarting…"),
    STOP("Stop"),
    ADAPTER_SHEET(ADAPTER_SHEET_TITLE),
}

/**
 * What the action column shows: at most one filled [primary], then the neutral [secondaries] (the
 * "Adapter & logs" entry always last). [primaryDimmed] = the RESTARTING primary.
 */
data class Actions(val primary: ActionId?, val primaryDimmed: Boolean, val secondaries: List<ActionId>) {
    /** Top-to-bottom order. */
    val all: List<ActionId> get() = listOfNotNull(primary) + secondaries
    /** Action buttons (94 dp) — everything but the sheet entry (76 dp). */
    val actionCount: Int get() = all.count { it != ActionId.ADAPTER_SHEET }
}

/**
 * C4 (plan v5.9; rev b R11: LIVE has no "Open CarPlay" — MainActivity.onResume already relaunches
 * the CarPlay screen whenever a session is live). [restartBusy] = SessionCore's restartInProgress.
 */
fun actionsFor(state: LinkState, credsMissing: Boolean, restartBusy: Boolean): Actions {
    val tail = ActionId.ADAPTER_SHEET
    if (restartBusy || state == LinkState.RESTARTING)
        return Actions(ActionId.RESTARTING, true, listOf(ActionId.STOP, tail))
    return when (state) {
        LinkState.IDLE, LinkState.STOPPED ->
            Actions(if (credsMissing) ActionId.SET_HOTSPOT else ActionId.START, false, listOf(tail))
        LinkState.FAILED -> Actions(ActionId.RESTART, false, listOf(ActionId.STOP, tail))
        // In flight, and LIVE (on the launcher only transiently): nothing invites a press.
        LinkState.SEARCHING, LinkState.CLAIMING, LinkState.WAITING, LinkState.PHONE_DETECTED,
        LinkState.PAIRING, LinkState.STARTING, LinkState.LIVE ->
            Actions(null, false, listOf(ActionId.RESTART, ActionId.STOP, tail))
        LinkState.RESTARTING -> Actions(ActionId.RESTARTING, true, listOf(ActionId.STOP, tail))
    }
}

/**
 * R14: the last inputs to [actionsFor], so EVERY trigger (state, credentials from the dialog or the
 * `--es ssid/pass` path, restart busy) recomputes from the same three values — no path can be missed.
 */
class ActionsTracker(
    private var state: LinkState = LinkState.IDLE,
    private var credsMissing: Boolean = true,
    private var restartBusy: Boolean = false,
) {
    fun onState(s: LinkState): Actions { state = s; return current() }
    fun onCredentials(missing: Boolean): Actions { credsMissing = missing; return current() }
    fun onRestartBusy(busy: Boolean): Actions { restartBusy = busy; return current() }
    fun current(): Actions = actionsFor(state, credsMissing, restartBusy)
}

// ---- UI-3 C6: driver wording -------------------------------------------------------------------------

/**
 * Why a FAILED state failed (rev b R10, rev c C4), set at every FAILED source so the driver line can
 * say what to do. SessionCore.linkStateFor's phase mapping derives the phase ones.
 */
enum class FailReason { LINK_FAILED, EXCEPTION, USB_STUCK, NO_HELLO, STALLED_PHONE, STALLED_ADAPTER, RX_UNHEALTHY, BOX_UNHEALTHY, OTHER }

/** The persistent line under the state word. OD-9B wording (owner-approved 2026-10-08); <= 80 chars, no engineering tokens. */
fun driverLine(state: LinkState, credsMissing: Boolean, failReason: FailReason?): String = when (state) {
    LinkState.IDLE -> if (credsMissing) "Set the vehicle hotspot to start." else "Press Start to connect the iPhone."
    LinkState.STOPPED -> if (credsMissing) "Set the vehicle hotspot to start." else "Stopped. Press Start to reconnect."
    LinkState.SEARCHING -> "Check that the CarPlay adapter is plugged in."
    LinkState.CLAIMING -> "Connecting to the adapter…"
    LinkState.WAITING -> "Waiting for the iPhone. Keep its Bluetooth and Wi-Fi on."
    LinkState.PHONE_DETECTED -> "iPhone found. Connecting…"
    LinkState.PAIRING -> "Confirm the code on the iPhone."
    LinkState.STARTING -> "Starting CarPlay…"
    LinkState.LIVE -> "CarPlay is running."
    LinkState.RESTARTING -> "Restarting the session…"
    LinkState.FAILED -> when (failReason ?: FailReason.OTHER) {
        FailReason.LINK_FAILED -> "Could not connect to the adapter. Press Restart session."
        FailReason.EXCEPTION -> "Something went wrong. Press Restart session."
        FailReason.USB_STUCK -> "The adapter is stuck. Unplug it and plug it back in."
        FailReason.NO_HELLO -> "The adapter is not answering. Wait two minutes, then Restart session."
        FailReason.STALLED_PHONE -> "The iPhone did not reconnect. Reconnect from the iPhone, or Restart session."
        FailReason.STALLED_ADAPTER -> "The adapter did not come back. Press Restart session, or unplug and replug it."
        FailReason.RX_UNHEALTHY -> "CarPlay receiver problem. Press Restart session."
        FailReason.BOX_UNHEALTHY -> "Adapter problem. Press Restart session."
        FailReason.OTHER -> "Connection failed. Press Restart session."
    }
}

/**
 * R9: engineering tokens that must never reach the driver line. A letter look-behind instead of `\b`,
 * because `_` and hex digits are word characters (`\b…\b` misses CT_SUBSCRIBE, MGMT_REBOOT, 0x5703, 20ms).
 */
val ENGINEERING_TOKENS = Regex("(?<![A-Za-z])(CT_\\w+|MGMT\\w*|0x[0-9A-Fa-f]+|\\d+\\s?ms\\b|hostapd|ocbmd|btd|nonce|HELLO)")

/** C6: the driver line's length budget — 2 lines of 24 sp in the 601 dp left pane. */
const val DRIVER_LINE_MAX = 80

/**
 * C6 one-shot results (box actions, export, forget, a rejected repeat press, "no adapter connected"):
 * shown for [ttlMs] (15 s, N-S7) in place of the state line, then the state line returns. A state
 * change cancels it ([cancel]), so a stale result cannot cover a new FAILED reason. The clock is the
 * caller's (elapsedRealtime on the device, a plain number in the test).
 */
class TransientLine(val ttlMs: Long = TTL_MS) {
    var text: String? = null; private set
    var shownAt: Long = 0L; private set
    fun show(t: String, nowMs: Long) { text = t; shownAt = nowMs }
    /** The text while it is younger than [ttlMs], else null. */
    fun current(nowMs: Long): String? = text?.takeIf { replayable(shownAt, nowMs, ttlMs) }
    /** Milliseconds until it expires (0 when gone). */
    fun remainingMs(nowMs: Long): Long = if (current(nowMs) == null) 0L else ttlMs - (nowMs - shownAt)
    fun cancel() { text = null }
    companion object {
        const val TTL_MS = 15_000L
        /** A stored result is replayed onto a re-attached view only while younger than [ttlMs]. */
        fun replayable(shownAt: Long, nowMs: Long, ttlMs: Long = TTL_MS): Boolean = nowMs - shownAt in 0 until ttlMs
    }
}

// ---- UI-3 C3/C9: layout metrics ------------------------------------------------------------------------

enum class Layout { TWO_PANE, SINGLE }

/**
 * Every size the launcher uses, in dp (heights, widths, gaps) and sp (text), for the space it is
 * given. Floors (C3): touch targets >= 76 dp, action buttons 94 dp, gaps >= 25 dp, text >= 24 sp.
 * Heights of text rows are budgets at font_scale 1.0 (bounds.txt, R18), used for the column sums.
 */
data class Metrics(
    val layout: Layout,
    val wDp: Float, val hDp: Float,
    val padH: Float, val padV: Float,
    val leftPane: Float, val rightPane: Float, val gutter: Float,
    val dot: Float, val dotGap: Float,
    val headlineMinSp: Float, val headlineMaxSp: Float, val headlineH: Float, val headlineGap: Float,
    val driverSp: Float, val driverH: Float, val driverGap: Float,
    val pairingMinSp: Float, val pairingMaxSp: Float, val pairingH: Float, val pairingGap: Float,
    val pairsWithSp: Float, val pairsWithH: Float, val pairsWithGap: Float,
    val hintSp: Float, val hintH: Float, val hintGap: Float,
    val chipH: Float, val chipSp: Float, val chipPadH: Float,
    val actionH: Float, val actionSp: Float, val actionPadH: Float,
    val entryH: Float, val entrySp: Float,
    val actionGap: Float,
    /** Gap between the status block and the actions in [Layout.SINGLE]. */
    val stackGap: Float,
) {
    /** Height available to a column inside the padding. */
    val available: Float get() = hDp - 2 * padV

    /** The left (status) column. Worst case = pairing code AND the V5 rekey hint both visible. */
    fun leftSum(pairing: Boolean, hint: Boolean): Float =
        dot + dotGap + headlineH + headlineGap + driverH + driverGap +
            (if (pairing) pairingH + pairingGap else 0f) +
            (if (hint) hintH + hintGap else 0f) +
            pairsWithH + pairsWithGap + chipH

    /** The right (actions) column with [actions] 94 dp buttons plus the 76 dp sheet entry. */
    fun rightSum(actions: Int): Float = actions * actionH + entryH + actions * actionGap

    /** Smallest text size anywhere on the screen (the floor is 24 sp). */
    val minTextSp: Float get() = minOf(headlineMinSp, driverSp, pairingMinSp, pairsWithSp, hintSp, chipSp, actionSp, entrySp)
    /** Smallest touch target. */
    val minTargetDp: Float get() = minOf(chipH, actionH, entryH)
}

/** C9: two panes when the panel is wide and landscape (N's gate), else one scrolling column. */
fun metrics(wDp: Float, hDp: Float): Metrics {
    val two = wDp >= 1000f && wDp > hDp
    val padH = if (two) 44f else 32f
    val padV = 25f
    val gutter = 44f
    val right = if (two) 400f else wDp - 2 * padH
    val left = if (two) wDp - 2 * padH - gutter - right else wDp - 2 * padH
    return Metrics(
        layout = if (two) Layout.TWO_PANE else Layout.SINGLE,
        wDp = wDp, hDp = hDp, padH = padH, padV = padV,
        leftPane = left, rightPane = right, gutter = gutter,
        dot = 26f, dotGap = 22f,
        headlineMinSp = 32f, headlineMaxSp = 56f, headlineH = 75f, headlineGap = 16f,
        driverSp = 26f, driverH = 64f, driverGap = 16f,
        pairingMinSp = 32f, pairingMaxSp = 56f, pairingH = 70f, pairingGap = 24f,
        pairsWithSp = 24f, pairsWithH = 32f, pairsWithGap = 25f,
        hintSp = 24f, hintH = 96f, hintGap = 16f,
        chipH = 76f, chipSp = 24f, chipPadH = 32f,
        actionH = 94f, actionSp = 30f, actionPadH = 48f,
        entryH = 76f, entrySp = 24f,
        actionGap = 25f,
        stackGap = 40f,
    )
}
