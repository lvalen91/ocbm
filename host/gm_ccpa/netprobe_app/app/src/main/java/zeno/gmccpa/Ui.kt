package zeno.gmccpa

import android.app.Activity
import android.app.AlertDialog
import android.content.Context
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.text.InputType
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView

/**
 * The launcher screen's chrome. Kept out of [MainActivity] so the session logic there is not
 * interleaved with view construction.
 *
 * Built in code, with no resources and no AndroidX, because `app/build.gradle` declares zero
 * dependencies on purpose and `tools/build_apk.sh` has no dependency resolution at all — it runs
 * kotlinc over a source directory. A Material import here would not be a style change, it would be a
 * new build system.
 *
 * **Nothing here is a fixed pixel size and nothing assumes the screen size.** The AAOS emulator hands
 * this Activity a 2400x960dp panel; the real head unit gives it a materially smaller area, and GM's
 * own chrome can take more of it at any time. So every dimension is derived from the space the root
 * view is actually given, re-derived whenever that changes, and the two headline strings autosize
 * themselves to fit rather than clipping or wrapping.
 */
object Palette {
    const val BG          = 0xFF0E1113.toInt()  // near-black; the dash is dark and stays dark
    const val SURFACE     = 0xFF171B1E.toInt()
    const val LINE        = 0xFF2A3136.toInt()
    const val TEXT        = 0xFFF2F4F5.toInt()
    const val TEXT_DIM    = 0xFF9AA4AA.toInt()
    // State colours. These carry the meaning at a glance — the colour is read first, the word second.
    const val IDLE        = 0xFF8A9298.toInt()  // grey   — nothing is happening
    const val WORKING     = 0xFFE8B341.toInt()  // amber  — something is in flight
    const val PHONE       = 0xFF58A6FF.toInt()  // blue   — the phone is in the picture
    const val LIVE        = 0xFF4CC38A.toInt()  // green  — CarPlay is through
    const val FAILED      = 0xFFE5534B.toInt()  // red    — it stopped for a reason
}

/**
 * The states the launcher can show, in the order a session actually walks through them.
 *
 * The first four come from [MainActivity.autoStart]; [PHONE_DETECTED] and [PAIRING] come from the
 * box over `CH_CTRL` (`CT_SESSION_EVENT` / `CT_PAIRING_CODE`, see `OcbmClient.handleCtrl`); the last
 * two come from the receiver when the phone's control connection lands.
 *
 * Labels are short and upper-case because this is the one thing that has to be readable at arm's
 * length in a moving vehicle. The sentence goes in the detail line, and the per-packet detail stays
 * in logcat (`adb logcat -s NETPROBE`), which remains the real instrument.
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
    STOPPED       ("STOPPED",            Palette.IDLE),
    FAILED        ("FAILED",             Palette.FAILED),
}

/**
 * Things the app can ask the adapter to do, over `CH_MGMT` (0x0040).
 *
 * All four verbs are already implemented client-side in `OcbmClient.mgmtAction`; only `GET_INFO` is
 * device-verified (docs/06 §identity snapshot). The rest are wired here because the protocol offers
 * them, but treat a first run of REBOOT / RESTART_WIRELESS on hardware as an experiment.
 */
enum class BoxAction(val label: String) {
    INFO("Box Info"),
    RESTART_WIFI("Restart Wi-Fi"),
    REBOOT("Reboot Box"),
    /**
     * Clears the Bluetooth bond on the box AND this app's CarPlay pairing store, together.
     *
     * They must go together. The box's bond and the app's Ed25519 peer store are separate records of
     * the same relationship, and clearing one leaves the other claiming a pairing the phone no longer
     * has — a split brain that presents as pair-verify failing for reasons that look like broken
     * crypto. The old "Forget Phone" cleared only the box's half.
     */
    FORGET_PHONE("Forget Pairing"),
}

/**
 * The capture row. Separate from [BoxAction] because these act on the *instrument*, not the adapter —
 * mixing them would put "Reboot Box" one thumb-width from "Export Logs" in a moving vehicle.
 *
 * This row is the whole reason the logger exists: the faults it is meant to catch only happen when no
 * laptop is attached, so retrieving a capture cannot require one.
 */
enum class LogAction(val label: String) {
    EXPORT("Export Logs"),
    SCOPE("Log Scope"),
    STATUS("Log Status"),
}

/** dp -> px, against the density the Activity is actually running at. */
fun Context.dp(v: Int): Int = (v * resources.displayMetrics.density + 0.5f).toInt()

/** Rounded-rectangle background, used for the pill buttons and the credential chip. */
private fun pillBg(ctx: Context, fill: Int, stroke: Int, radiusPx: Int, strokePx: Int) =
    GradientDrawable().apply {
        shape = GradientDrawable.RECTANGLE
        cornerRadius = radiusPx.toFloat()
        setColor(fill)
        if (stroke != Color.TRANSPARENT) setStroke(strokePx, stroke)
    }

/**
 * The launcher screen.
 *
 * Deliberately minimal: connection state, one supporting line, Start and Stop. The Wi-Fi credentials
 * and the adapter-control verbs are real inputs but are not glanceable information, so they sit
 * behind a chip and a secondary row rather than competing with the state readout.
 */
class LauncherUi(private val act: Activity) {

    /** Fired by the two primary buttons; wired up by [MainActivity]. */
    var onStart: () -> Unit = {}
    var onStop: () -> Unit = {}
    /**
     * Driver-triggered recovery. Deliberately next to Start/Stop rather than in the adapter row: it is
     * the thing to press when everything *looks* right and CarPlay still will not start, which is the
     * situation a driver actually finds themselves in.
     */
    var onRecover: () -> Unit = {}
    /** Fired when the credentials dialog is confirmed, so the caller can push them into the probe. */
    var onCredentials: (ssid: String, pass: String, chan: String) -> Unit = { _, _, _ -> }
    /** Fired by the secondary row; the caller decides whether there is a link to send it on. */
    var onBoxAction: (BoxAction) -> Unit = {}
    /** Fired by the capture row — export, scope toggle, status. */
    var onLogAction: (LogAction) -> Unit = {}

    // Credential state lives here as plain strings, NOT as EditTexts held across dialog lifetimes:
    // a dialog's views are torn down on dismiss, so keeping references to them means reading stale or
    // detached widgets on the next session start.
    var ssid: String = "myChevrolet 32D4"; private set
    var pass: String = "123456789000";     private set
    var chan: String = "36";               private set

    private val dot = View(act)
    private val stateText = TextView(act)
    private val detailText = TextView(act)
    private val pairText = TextView(act)
    private val credSummary = TextView(act)

    private lateinit var actions: LinearLayout
    private lateinit var boxRow: LinearLayout
    private lateinit var logRow: LinearLayout
    private lateinit var column: LinearLayout

    /** Every button, with the sp size it should have at scale 1.0. Re-sized in [applyMetrics]. */
    private val pills = mutableListOf<Pair<Button, Float>>()

    /** Last scale applied, so a layout pass that changes nothing does no work. */
    private var lastScale = -1f

    val root: View = buildRoot()

    // ---- construction ---------------------------------------------------------------------------

    private fun pill(labelText: String, accent: Int, filled: Boolean, baseSp: Float,
                     onClick: () -> Unit): Button =
        Button(act).apply {
            text = labelText
            isAllCaps = true
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            setTextColor(if (filled) Palette.BG else accent)
            stateListAnimator = null       // the default elevation lift fights a flat pill
            setOnClickListener { onClick() }
            setTag(if (filled) accent else Color.TRANSPARENT)   // fill colour, re-read on re-style
            pills += this to baseSp
        }

    private fun buildRoot(): View {
        // ScrollView so a short panel degrades to a scroll instead of clipping the buttons off the
        // bottom — the failure that would otherwise make Stop unreachable on the smallest screen.
        val scroller = ScrollView(act).apply {
            isFillViewport = true
            setBackgroundColor(Palette.BG)
            overScrollMode = View.OVER_SCROLL_NEVER
        }
        val centre = FrameLayout(act)

        column = LinearLayout(act).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
        }

        // --- state: colour dot above the one word that matters --------------------------------
        // The dot sits ABOVE rather than beside the headline on purpose. Beside it, the row has to be
        // full-width for the headline's autosizer to have a bound to shrink against, which drags the
        // pair to the left edge while everything below stays centred. Stacked, both are centred and
        // the headline still gets a full-width slot to autosize within.
        dot.background = GradientDrawable().apply {
            shape = GradientDrawable.OVAL
            setColor(Palette.IDLE)
        }
        column.addView(dot, LinearLayout.LayoutParams(0, 0))       // sized in applyMetrics
        stateText.apply {
            text = LinkState.IDLE.label
            setTextColor(Palette.TEXT)
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            gravity = Gravity.CENTER_HORIZONTAL
            setSingleLine()
            letterSpacing = 0.02f
        }
        column.addView(stateText, LinearLayout.LayoutParams(-1, -2))

        // --- detail line: the sentence the old screen used as its entire UI -------------------
        detailText.apply {
            text = "starting…"
            setTextColor(Palette.TEXT_DIM)
            gravity = Gravity.CENTER_HORIZONTAL
            maxLines = 2
        }
        column.addView(detailText, LinearLayout.LayoutParams(-1, -2))

        // --- pairing code: hidden until the box sends one -------------------------------------
        // When it does appear it is the most important thing on the screen: it has to be read off
        // this display and matched against a prompt on the iPhone.
        pairText.apply {
            setTextColor(Palette.PHONE)
            typeface = Typeface.create("monospace", Typeface.BOLD)
            gravity = Gravity.CENTER_HORIZONTAL
            letterSpacing = 0.28f
            setSingleLine()
            visibility = View.GONE
        }
        column.addView(pairText, LinearLayout.LayoutParams(-1, -2))

        // --- primary actions -------------------------------------------------------------------
        actions = LinearLayout(act).apply { orientation = LinearLayout.HORIZONTAL }
        actions.addView(pill("Start", Palette.LIVE, filled = true, baseSp = 26f) { onStart() })
        actions.addView(pill("Stop", Palette.TEXT_DIM, filled = false, baseSp = 26f) { onStop() })
        actions.addView(pill("Recover", Palette.PHONE, filled = false, baseSp = 26f) { onRecover() })
        column.addView(actions, LinearLayout.LayoutParams(-2, -2))

        // --- adapter control, secondary ---------------------------------------------------------
        boxRow = LinearLayout(act).apply { orientation = LinearLayout.HORIZONTAL }
        for (a in BoxAction.values()) {
            boxRow.addView(pill(a.label, Palette.LINE, filled = false, baseSp = 17f) { onBoxAction(a) }
                .also { it.setTextColor(Palette.TEXT_DIM) })
        }
        column.addView(boxRow, LinearLayout.LayoutParams(-2, -2))

        // --- capture control, secondary -----------------------------------------------------------
        logRow = LinearLayout(act).apply { orientation = LinearLayout.HORIZONTAL }
        for (a in LogAction.values()) {
            logRow.addView(pill(a.label, Palette.LINE, filled = false, baseSp = 17f) { onLogAction(a) }
                .also { it.setTextColor(Palette.TEXT_DIM) })
        }
        column.addView(logRow, LinearLayout.LayoutParams(-2, -2))

        // --- credentials, demoted to a chip that opens the dialog --------------------------------
        credSummary.apply {
            setTextColor(Palette.TEXT_DIM)
            gravity = Gravity.CENTER
            setSingleLine()
            setOnClickListener { showCredentialsDialog() }
        }
        column.addView(credSummary, LinearLayout.LayoutParams(-2, -2))
        refreshCredSummary()

        centre.addView(column, FrameLayout.LayoutParams(-1, -2, Gravity.CENTER))
        scroller.addView(centre, FrameLayout.LayoutParams(-1, -1))

        // Re-derive every dimension whenever the space we are given changes: first layout, a GM
        // chrome change, a multi-window resize, a different head unit.
        scroller.addOnLayoutChangeListener { v, l, t, r, b, ol, ot, or_, ob ->
            if (r - l != or_ - ol || b - t != ob - ot) applyMetrics(r - l, b - t)
        }
        return scroller
    }

    // ---- responsive sizing ----------------------------------------------------------------------

    /**
     * Derive every size from the area actually granted.
     *
     * The reference panel is 1100x620dp — comfortably smaller than the emulator's 2400x960 and
     * around what a head unit leaves an app after its own chrome. Bigger panels scale up (capped, so
     * a huge dash does not turn into a billboard), smaller ones scale down to a floor that keeps the
     * secondary row tappable.
     */
    private fun applyMetrics(wPx: Int, hPx: Int) {
        if (wPx <= 0 || hPx <= 0) return
        val d = act.resources.displayMetrics.density
        val wDp = wPx / d
        val hDp = hPx / d
        val s = minOf(wDp / 1100f, hDp / 620f).coerceIn(0.5f, 1.35f)
        if (kotlin.math.abs(s - lastScale) < 0.01f) return
        lastScale = s

        fun px(dpValue: Float) = (dpValue * s * d + 0.5f).toInt()
        fun sp(v: Float) = v * s

        column.setPadding(px(48f), px(32f), px(48f), px(32f))

        // Headline. Autosizing is the part that makes this survive an unknown panel: the text shrinks
        // itself down to the floor rather than clipping, so "LOOKING FOR ADAPTER" fits where it must.
        stateText.setAutoSizeTextTypeUniformWithConfiguration(
            maxOf(14, (sp(22f)).toInt()), maxOf(16, sp(64f).toInt()), 1,
            TypedValue.COMPLEX_UNIT_SP)

        val dotPx = px(26f)
        (dot.layoutParams as LinearLayout.LayoutParams).apply {
            width = dotPx; height = dotPx; bottomMargin = px(22f)
        }
        dot.requestLayout()

        detailText.setTextSize(TypedValue.COMPLEX_UNIT_SP, sp(24f))
        (detailText.layoutParams as LinearLayout.LayoutParams).topMargin = px(16f)

        pairText.setAutoSizeTextTypeUniformWithConfiguration(
            maxOf(14, sp(24f).toInt()), maxOf(16, sp(56f).toInt()), 1,
            TypedValue.COMPLEX_UNIT_SP)
        (pairText.layoutParams as LinearLayout.LayoutParams).topMargin = px(24f)

        (actions.layoutParams as LinearLayout.LayoutParams).topMargin = px(56f)
        (boxRow.layoutParams as LinearLayout.LayoutParams).topMargin = px(28f)
        (credSummary.layoutParams as LinearLayout.LayoutParams).topMargin = px(40f)

        for ((i, entry) in pills.withIndex()) {
            val (b, baseSp) = entry
            val primary = baseSp >= 20f
            b.setTextSize(TypedValue.COMPLEX_UNIT_SP, sp(baseSp))
            // The primary pair is sized for a glance-and-stab while moving; this Activity is declared
            // distractionOptimized, so it is genuinely on screen in motion. The secondary row is
            // smaller on purpose — it is a parked-and-deliberate action, not a driving one.
            val h = px(if (primary) 88f else 60f)
            b.minimumHeight = h
            b.minHeight = h
            val padH = px(if (primary) 48f else 26f)
            b.setPadding(padH, 0, padH, 0)
            val fill = b.tag as Int
            val accent = if (fill != Color.TRANSPARENT) fill else
                if (primary) Palette.TEXT_DIM else Palette.LINE
            b.background = pillBg(act, fill, accent, h / 2, px(2f))
            // pills[0..1] are Start/Stop, pills[2..] the adapter row; the first button of each row
            // carries no left margin so both rows stay centred.
            (b.layoutParams as? ViewGroup.MarginLayoutParams)?.leftMargin =
                if (i == 0 || i == 2) 0 else px(if (primary) 24f else 12f)
        }

        credSummary.setTextSize(TypedValue.COMPLEX_UNIT_SP, sp(19f))
        credSummary.background = pillBg(act, Color.TRANSPARENT, Palette.LINE, px(32f), px(2f))
        credSummary.setPadding(px(32f), px(16f), px(32f), px(16f))

        column.requestLayout()
    }

    // ---- credentials ----------------------------------------------------------------------------

    private fun refreshCredSummary() {
        credSummary.text = "Wi-Fi:  $ssid   ·   ch $chan   ·   tap to edit"
    }

    /**
     * The credentials popup. Platform [AlertDialog] with the DeviceDefault dark alert theme — the
     * light default would flash a white sheet on a dark dash at night.
     */
    fun showCredentialsDialog() {
        val pad = act.dp(40)
        val body = LinearLayout(act).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, act.dp(24), pad, act.dp(8))
        }

        fun field(labelText: String, value: String, numeric: Boolean): EditText {
            body.addView(TextView(act).apply {
                text = labelText
                setTextColor(Palette.TEXT_DIM)
                setTextSize(TypedValue.COMPLEX_UNIT_SP, 16f)
                isAllCaps = true
                letterSpacing = 0.08f
            }, LinearLayout.LayoutParams(-1, -2).apply { topMargin = act.dp(20) })
            val e = EditText(act).apply {
                setText(value)
                setTextSize(TypedValue.COMPLEX_UNIT_SP, 26f)
                setTextColor(Palette.TEXT)
                inputType = if (numeric) InputType.TYPE_CLASS_NUMBER
                            else InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD
                setSingleLine()
            }
            body.addView(e, LinearLayout.LayoutParams(-1, -2))
            return e
        }

        // The passphrase is shown, not masked. It is a fixed vehicle-hotspot credential that has to be
        // read off this screen and typed into a phone during bring-up; masking it would help nobody
        // and would hide typos in the one value whose mistyping is silent — the phone simply never
        // joins, and per docs/06 §5.4 the dead network then poisons the next attempt.
        val eSsid = field("Hotspot SSID", ssid, numeric = false)
        val ePass = field("Passphrase", pass, numeric = false)
        val eChan = field("Channel", chan, numeric = true)

        AlertDialog.Builder(act, android.R.style.Theme_DeviceDefault_Dialog_Alert)
            .setTitle("Vehicle hotspot")
            .setView(body)
            .setPositiveButton("Save") { _, _ ->
                ssid = eSsid.text.toString().trim()
                pass = ePass.text.toString().trim()
                chan = eChan.text.toString().trim()
                refreshCredSummary()
                onCredentials(ssid, pass, chan)
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    /** Set the credentials from outside the dialog — the `--es ssid/pass/chan` scripted path. */
    fun setCredentials(newSsid: String?, newPass: String?, newChan: String?) = act.runOnUiThread {
        newSsid?.let { ssid = it }
        newPass?.let { pass = it }
        newChan?.let { chan = it }
        refreshCredSummary()
    }

    // ---- state ----------------------------------------------------------------------------------

    /**
     * The single UI update channel. [detail] is the free-form sentence the session code already
     * produces; [state] is what makes it glanceable.
     */
    fun setState(state: LinkState, detail: String) = act.runOnUiThread {
        if (act.isFinishing) return@runOnUiThread
        stateText.text = state.label
        (dot.background as GradientDrawable).setColor(state.color)
        detailText.text = detail
    }

    /** Show (or, on an empty code, hide) the box's pairing code. */
    fun setPairingCode(code: String) = act.runOnUiThread {
        if (act.isFinishing) return@runOnUiThread
        pairText.text = code
        pairText.visibility = if (code.isEmpty()) View.GONE else View.VISIBLE
    }

    /** Free-form message shown in the detail line without disturbing the state word. */
    fun setDetail(detail: String) = act.runOnUiThread {
        if (!act.isFinishing) detailText.text = detail
    }
}
