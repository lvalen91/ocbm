package zeno.gmccpa

import android.app.Activity
import android.app.AlertDialog
import android.content.Context
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.text.InputType
import android.text.TextUtils
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView

/*
 * The launcher screen's chrome. Kept out of [MainActivity] so the session logic there is not
 * interleaved with view construction.
 *
 * Built in code, with no resources and no AndroidX, because `app/build.gradle` declares zero
 * dependencies on purpose and `tools/build_apk.sh` has no dependency resolution at all — it runs
 * kotlinc over a source directory. A Material import here would not be a style change, it would be a
 * new build system.
 *
 * Since plan v5.9 UI-3 every rule — which actions a state offers, the driver's sentence, every size —
 * lives in the pure LauncherModel.kt and is JUnit-tested at the head unit's real 1132.8 x 673.6 dp
 * (1416 x 842 px below GM's status bar at 200 dpi). This file only renders what those return, and
 * re-renders whenever the space it is given changes.
 */

/** dp -> px, against the density the Activity is actually running at. */
fun Context.dp(v: Int): Int = (v * resources.displayMetrics.density + 0.5f).toInt()

/** Rounded-rectangle background, used for the buttons and the credentials chip. */
private fun pillBg(fill: Int, stroke: Int, radiusPx: Int, strokePx: Int) =
    GradientDrawable().apply {
        shape = GradientDrawable.RECTANGLE
        cornerRadius = radiusPx.toFloat()
        setColor(fill)
        if (stroke != Color.TRANSPARENT) setStroke(strokePx, stroke)
    }

/**
 * The launcher screen (plan v5.9 UI-3).
 *
 * Two panes at the head unit's size ([metrics]): the STATUS on the left — state dot, state word, the
 * driver line, the pairing code while one is shown, the adapter's Bluetooth name, the V5 rekey hint,
 * and the hotspot chip (C2: always on screen) — and the ACTIONS on the right, chosen by the state
 * ([actionsFor]): at most one filled primary, neutral secondaries, and "Adapter & logs" last.
 * Narrower or portrait panels get one scrolling column with the same floors.
 */
class LauncherUi(private val act: Activity) {

    /** Fired by the action buttons; wired up by [MainActivity]. */
    var onStart: () -> Unit = {}
    var onStop: () -> Unit = {}
    /**
     * Restart Session: drop the phone, release the box, wait, re-claim — the one action that fixed
     * things in the old app, and the one a driver needs when they cannot force-stop the app or reach
     * the adapter. Since plan v5.9 UI-1 (D001) it is also what the old Recover button did: the
     * restart clears the recovery ladder's cooldowns (SessionSupervisor.onOperatorRestart), so the
     * fresh link's ladder runs without waiting out stamps earned against the dropped one. One
     * action for "it looks right and CarPlay still will not start".
     */
    var onRestart: () -> Unit = {}
    /** Fired when the credentials dialog is confirmed, so the caller can push them into the probe. */
    var onCredentials: (ssid: String, pass: String) -> Unit = { _, _ -> }
    /** Fired by the "Adapter & logs" sheet's adapter items, after the sheet is dismissed (MA confirms). */
    var onBoxAction: (BoxAction) -> Unit = {}
    /** Fired by the sheet's "Export logs" (D003: the scope/status pills went; their verbs stay). */
    var onExportLogs: () -> Unit = {}

    // Credential state lives here as plain strings, NOT as EditTexts held across dialog lifetimes:
    // a dialog's views are torn down on dismiss, so keeping references to them means reading stale or
    // detached widgets on the next session start.
    // OD-7A (owner, 2026-10-07): no compiled-in credentials. First run starts blank, and autoStart
    // refuses to hand off until the driver saves them in the dialog (SessionCore.autoStart).
    var ssid: String = ""; private set
    var pass: String = ""; private set

    // Persisted values win over the blank first-run state. Loaded HERE — before `root` is built and
    // therefore before MainActivity's autoStart can read these — so a relaunch hands the box the
    // credentials the driver last saved. Written only by the dialog's Save (see HotspotPrefs for why
    // the --es path does not persist).
    //
    // There is no channel field (owner, 2026-10-07): the app sends NO wifi_channel on purpose and never
    // asks for one, so the box keeps its stock hostapd channel. A `chan` an older build saved is never
    // loaded and is purged by the next Save (HotspotPrefs.save, plan v5.9 D015).
    init {
        val app = act.applicationContext
        HotspotPrefs.ssid(app)?.let { ssid = it }
        HotspotPrefs.pass(app)?.let { pass = it }
    }

    /** True while either credential is blank — the state in which autoStart will not hand off. */
    val credentialsMissing: Boolean get() = ssid.isBlank() || pass.isBlank()

    // ---- view state --------------------------------------------------------------------------------

    private var state: LinkState = LinkState.IDLE
    private var failReason: FailReason? = null
    /** C6 "P": a per-state hint that replaces the driver line until the state changes (e.g. the 30 s claim hints). */
    private var stateHint: String? = null
    /** C6 "T": one-shot results, 15 s. */
    private val transient = TransientLine()
    /** Full engineering sentence of the last status/detail, for the sheet's "Last message" (C6). */
    private var lastMessage: String = ""
    /** R14: every recompute goes through this one tracker ([refreshActions]). */
    private val tracker = ActionsTracker(state, credentialsMissing, false)
    private var shownActions: Actions? = null

    private val handler = Handler(Looper.getMainLooper())
    private val expireTransient = Runnable { renderLine() }

    // ---- views -------------------------------------------------------------------------------------

    private val dot = View(act)
    private val stateText = TextView(act)
    /** The driver line: transient result, else the state hint, else [driverLine]. */
    private val lineText = TextView(act)
    private val pairText = TextView(act)
    /** The adapter's Bluetooth name (MGMT_INFO `name`) — what to pick on the iPhone for a new pairing. */
    private val btNameText = TextView(act)
    /** V5: the persistent rekey hint (SessionCore.setHint); GONE while empty. */
    private val hintText = TextView(act)
    /** C2: the hotspot chip, under the "pairs with" line, always on screen. */
    private val credChip = TextView(act)
    /** The credentials dialog while it is showing, so a repeated prompt does not stack a second one. */
    private var credDialog: AlertDialog? = null
    /** The "Adapter & logs" sheet while it is showing — guarded like [credDialog], so nothing stacks. */
    private var sheetDialog: AlertDialog? = null

    private val panes = LinearLayout(act)
    private val left = LinearLayout(act)
    private val right = LinearLayout(act)
    private lateinit var scroller: ScrollView

    /** The metrics last applied, so a layout pass that changes nothing does no work. */
    private var m: Metrics? = null

    val root: View = buildRoot()

    // ---- construction ------------------------------------------------------------------------------

    private fun buildRoot(): View {
        // ScrollView so a short panel degrades to a scroll instead of clipping the buttons off the
        // bottom (SINGLE). At the head unit's size nothing scrolls (TWO_PANE, LauncherModelTest).
        scroller = ScrollView(act).apply {
            isFillViewport = true
            setBackgroundColor(Palette.BG)
            overScrollMode = View.OVER_SCROLL_NEVER
            isVerticalScrollBarEnabled = true
        }
        left.orientation = LinearLayout.VERTICAL
        left.gravity = Gravity.CENTER
        right.orientation = LinearLayout.VERTICAL
        right.gravity = Gravity.CENTER

        dot.background = GradientDrawable().apply { shape = GradientDrawable.OVAL; setColor(Palette.IDLE) }
        left.addView(dot, LinearLayout.LayoutParams(0, 0))
        stateText.apply {
            text = LinkState.IDLE.label
            setTextColor(Palette.TEXT)
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            gravity = Gravity.CENTER
            // maxLines, not setSingleLine(): single-line scrolls horizontally, and autosize then
            // measures against an unbounded width, so only the row height limited the size.
            maxLines = 1
            letterSpacing = 0.02f
        }
        left.addView(stateText, LinearLayout.LayoutParams(-1, 0))
        lineText.apply {
            setTextColor(Palette.TEXT_DIM)
            gravity = Gravity.CENTER_HORIZONTAL
            maxLines = 2
            ellipsize = TextUtils.TruncateAt.END   // C6: today it set maxLines only
        }
        left.addView(lineText, LinearLayout.LayoutParams(-1, -2))
        // Pairing code: hidden until the box sends one. When it does appear it is the most important
        // thing on the screen: it has to be read off this display and matched on the iPhone.
        pairText.apply {
            setTextColor(Palette.PHONE)
            typeface = Typeface.create("monospace", Typeface.BOLD)
            gravity = Gravity.CENTER
            letterSpacing = 0.28f
            setSingleLine()
            visibility = View.GONE
        }
        left.addView(pairText, LinearLayout.LayoutParams(-1, 0))
        btNameText.apply {
            setTextColor(Palette.TEXT_DIM)
            gravity = Gravity.CENTER_HORIZONTAL
            setSingleLine()
            ellipsize = TextUtils.TruncateAt.END
            visibility = View.GONE
        }
        left.addView(btNameText, LinearLayout.LayoutParams(-1, -2))
        // V5 rekey hint: hidden unless the accessory key changed under stored pairings. Persistent
        // (SessionCore.setHint), unlike the driver line.
        hintText.apply {
            setTextColor(Palette.TEXT_DIM)
            gravity = Gravity.CENTER_HORIZONTAL
            maxLines = 3
            ellipsize = TextUtils.TruncateAt.END
            visibility = View.GONE
        }
        left.addView(hintText, LinearLayout.LayoutParams(-1, -2))
        credChip.apply {
            setTextColor(Palette.TEXT)
            gravity = Gravity.CENTER
            setSingleLine()
            ellipsize = TextUtils.TruncateAt.END
            isClickable = true
            setOnClickListener { showCredentialsDialog() }
        }
        left.addView(credChip, LinearLayout.LayoutParams(-2, 0))

        panes.addView(left)
        panes.addView(right)
        val frame = FrameLayout(act)
        frame.addView(panes, FrameLayout.LayoutParams(-1, -1))
        scroller.addView(frame, FrameLayout.LayoutParams(-1, -1))

        refreshCredSummary()
        renderLine()
        // Re-derive every dimension whenever the space we are given changes: first layout, a GM
        // chrome change, a multi-window resize, a different head unit.
        // F-C3 (2026-10-08): metrics are applied from a pre-draw listener, not a layout-change one.
        // applyMetrics rebuilds the buttons (removeAllViews/addView) and calls requestLayout(), which
        // inside a layout pass is "requestLayout() improperly called" plus a second full pass before
        // the first frame. OnPreDraw runs after layout and before the draw: on a size change it
        // applies the metrics, asks for a new layout and returns false (this draw is skipped), and
        // on the next pass the metrics are unchanged and it returns true. The scroller's size is read
        // here, as the layout listener read it; the first pass at 2400x960 is the only one expected.
        scroller.viewTreeObserver.addOnPreDrawListener {
            if (applyMetrics(scroller.width, scroller.height)) false else true
        }
        return scroller
    }

    // ---- responsive sizing -------------------------------------------------------------------------

    /** Apply [metrics] for the area actually granted; true if they changed (and a layout was requested).
     *  Every size is dp/sp from the pure model. */
    private fun applyMetrics(wPx: Int, hPx: Int): Boolean {
        if (wPx <= 0 || hPx <= 0) return false
        val d = act.resources.displayMetrics.density
        val nm = metrics(wPx / d, hPx / d)
        if (nm == m) return false
        m = nm
        fun px(v: Float) = (v * d + 0.5f).toInt()
        val two = nm.layout == Layout.TWO_PANE

        panes.orientation = if (two) LinearLayout.HORIZONTAL else LinearLayout.VERTICAL
        panes.gravity = Gravity.CENTER
        panes.setPadding(px(nm.padH), px(nm.padV), px(nm.padH), px(nm.padV))
        left.layoutParams = LinearLayout.LayoutParams(if (two) px(nm.leftPane) else -1, if (two) -1 else -2)
        right.layoutParams = LinearLayout.LayoutParams(if (two) px(nm.rightPane) else -1, if (two) -1 else -2).apply {
            if (two) leftMargin = px(nm.gutter) else topMargin = px(nm.stackGap)
        }

        (dot.layoutParams as LinearLayout.LayoutParams).apply { width = px(nm.dot); height = px(nm.dot); bottomMargin = px(nm.dotGap) }
        // Headline: autosized to the row height AND the pane width (maxLines = 1 above), so
        // "LOOKING FOR ADAPTER" fits rather than clipping.
        (stateText.layoutParams as LinearLayout.LayoutParams).apply { height = px(nm.headlineH); bottomMargin = px(nm.headlineGap) }
        stateText.setAutoSizeTextTypeUniformWithConfiguration(nm.headlineMinSp.toInt(), nm.headlineMaxSp.toInt(), 1, TypedValue.COMPLEX_UNIT_SP)
        lineText.setTextSize(TypedValue.COMPLEX_UNIT_SP, nm.driverSp)
        (lineText.layoutParams as LinearLayout.LayoutParams).bottomMargin = px(nm.driverGap)
        (pairText.layoutParams as LinearLayout.LayoutParams).apply { height = px(nm.pairingH); bottomMargin = px(nm.pairingGap) }
        pairText.setAutoSizeTextTypeUniformWithConfiguration(nm.pairingMinSp.toInt(), nm.pairingMaxSp.toInt(), 1, TypedValue.COMPLEX_UNIT_SP)
        hintText.setTextSize(TypedValue.COMPLEX_UNIT_SP, nm.hintSp)
        (hintText.layoutParams as LinearLayout.LayoutParams).bottomMargin = px(nm.hintGap)
        btNameText.setTextSize(TypedValue.COMPLEX_UNIT_SP, nm.pairsWithSp)
        (btNameText.layoutParams as LinearLayout.LayoutParams).bottomMargin = px(nm.pairsWithGap)
        credChip.setTextSize(TypedValue.COMPLEX_UNIT_SP, nm.chipSp)
        (credChip.layoutParams as LinearLayout.LayoutParams).height = px(nm.chipH)
        credChip.setPadding(px(nm.chipPadH), 0, px(nm.chipPadH), 0)
        credChip.maxWidth = if (two) px(nm.leftPane) else Int.MAX_VALUE
        styleChip()
        shownActions = null          // sizes changed: rebuild the buttons
        refreshActions()
        panes.requestLayout()
        return true
    }

    // ---- actions (C4/C8) ---------------------------------------------------------------------------

    /** R14: the ONE recompute. Called on state, credentials (dialog and --es), and restart busy. */
    private fun refreshActions() {
        val a = tracker.current()
        val nm = m ?: return
        if (a == shownActions) return
        shownActions = a
        right.removeAllViews()
        val d = act.resources.displayMetrics.density
        fun px(v: Float) = (v * d + 0.5f).toInt()
        a.all.forEachIndexed { i, id ->
            val entry = id == ActionId.ADAPTER_SHEET
            val filled = id == a.primary
            val h = px(if (entry) nm.entryH else nm.actionH)
            val b = Button(act).apply {
                text = id.label
                isAllCaps = true                                   // R21: all-caps stays
                typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
                setTextSize(TypedValue.COMPLEX_UNIT_SP, if (entry) nm.entrySp else nm.actionSp)
                setSingleLine()
                stateListAnimator = null                           // the default elevation lift fights a flat pill
                minHeight = h; minimumHeight = h
                setPadding(px(nm.actionPadH), 0, px(nm.actionPadH), 0)
                // C8: the state colour stays on the dot. Neutral = transparent + STROKE + TEXT label;
                // the one filled primary = TEXT fill + BG label.
                setTextColor(if (filled) Palette.BG else Palette.TEXT)
                background = pillBg(if (filled) Palette.TEXT else Color.TRANSPARENT,
                    if (filled) Color.TRANSPARENT else Palette.STROKE, h / 2, px(2f))
                // "Restarting…": dimmed but still clickable, so a press shows the rejection line.
                alpha = if (filled && a.primaryDimmed) 0.5f else 1f
                setOnClickListener { onAction(id) }
            }
            right.addView(b, LinearLayout.LayoutParams(-1, h).apply { if (i > 0) topMargin = px(nm.actionGap) })
        }
    }

    private fun onAction(id: ActionId) = when (id) {
        ActionId.SET_HOTSPOT -> showCredentialsDialog()
        ActionId.START -> onStart()
        ActionId.RESTART, ActionId.RESTARTING -> onRestart()
        ActionId.STOP -> onStop()
        ActionId.ADAPTER_SHEET -> showAdapterSheet()
    }

    // ---- the driver line (C6) ----------------------------------------------------------------------

    /** Transient result while young, else the state hint, else the state's driver line. */
    private fun renderLine() {
        val now = SystemClock.elapsedRealtime()
        lineText.text = transient.current(now) ?: stateHint ?: driverLine(state, credentialsMissing, failReason)
        handler.removeCallbacks(expireTransient)
        val rem = transient.remainingMs(now)
        if (rem > 0) handler.postDelayed(expireTransient, rem)
    }

    // ---- credentials -------------------------------------------------------------------------------

    /** C2: "Hotspot: <ssid> · Edit" or, amber, "Hotspot not set · Set". Also the R14 credentials trigger. */
    private fun refreshCredSummary() {
        credChip.text = if (credentialsMissing) "Hotspot not set · Set" else "Hotspot: $ssid · Edit"
        styleChip()
        tracker.onCredentials(credentialsMissing)
        refreshActions()
        renderLine()
    }

    private fun styleChip() {
        val nm = m ?: return
        val d = act.resources.displayMetrics.density
        val h = (nm.chipH * d + 0.5f).toInt()
        credChip.background = pillBg(Color.TRANSPARENT, if (credentialsMissing) Palette.WORKING else Palette.STROKE,
            h / 2, (2f * d + 0.5f).toInt())
    }

    // ---- dialogs: credentials, the "Adapter & logs" sheet, confirmations -------------------------
    //
    // Every dialog here is a platform AlertDialog with the DeviceDefault dark alert theme (the light
    // default would flash a white sheet on a dark dash at night), and every one carries the plan v5.9
    // R16 floors: body text >= 24 sp, buttons >= 76 dp tall with a 24 sp label. At most ONE is up at
    // a time: each opener returns while another is showing, and a dialog that leads to a second one
    // (sheet -> confirmation, credentials Save -> "Apply now") opens it from its OWN dismiss listener,
    // after the first is gone — never stacked.

    /** The confirmation dialog while it is showing (stack guard; see the section note). */
    private var confirmDialog: AlertDialog? = null

    private fun anyDialogShowing(): Boolean =
        credDialog?.isShowing == true || sheetDialog?.isShowing == true || confirmDialog?.isShowing == true

    /** R16: 24 sp body text, padded like the platform's own message area. */
    private fun dialogText(t: String, color: Int = Palette.TEXT) = TextView(act).apply {
        text = t
        setTextColor(color)
        setTextSize(TypedValue.COMPLEX_UNIT_SP, DIALOG_TEXT_SP)
    }

    /** R16: the platform buttons, after show() (they exist only then): >= 76 dp tall, 24 sp label. */
    private fun AlertDialog.applyButtonFloors() {
        for (w in intArrayOf(AlertDialog.BUTTON_POSITIVE, AlertDialog.BUTTON_NEGATIVE, AlertDialog.BUTTON_NEUTRAL)) {
            getButton(w)?.let { b ->
                b.minHeight = act.dp(DIALOG_BUTTON_DP)
                b.minimumHeight = act.dp(DIALOG_BUTTON_DP)
                b.setTextSize(TypedValue.COMPLEX_UNIT_SP, DIALOG_TEXT_SP)
            }
        }
    }

    /** A body that scrolls (a short panel, the sheet's long "Last message", or the IME taking half the screen). */
    private fun scrolling(body: View) = ScrollView(act).apply {
        isVerticalScrollBarEnabled = true
        isFillViewport = false
        addView(body, FrameLayout.LayoutParams(-1, -2))
    }

    /**
     * A confirmation (MainActivity's four adapter actions, and the OD-9A "Apply now" offer): [title],
     * a 24 sp scrolling [body], [positive] and Cancel. Returns without showing while another dialog is
     * up — the caller's action then simply does not run, which is the safe direction for these.
     */
    fun confirm(title: String, body: String, positive: String, negative: String = "Cancel", onPositive: () -> Unit) {
        if (act.isFinishing || act.isDestroyed || anyDialogShowing()) return
        val pad = act.dp(32)
        val text = dialogText(body).apply { setPadding(pad, act.dp(16), pad, act.dp(8)) }
        confirmDialog = AlertDialog.Builder(act, android.R.style.Theme_DeviceDefault_Dialog_Alert)
            .setTitle(title)
            .setView(scrolling(text))
            .setPositiveButton(positive) { _, _ -> onPositive() }
            .setNegativeButton(negative, null)
            .setOnDismissListener { confirmDialog = null }
            .show()
            .also { it.applyButtonFloors() }
    }

    /**
     * The "Adapter & logs" sheet (plan v5.9 D004/C1): Restart adapter Bluetooth, Reboot adapter,
     * Forget pairing, Export logs ([SheetItem], in that order), the full "Last message", and Close.
     * A pressed item dismisses the sheet and runs from the sheet's dismiss listener, so MainActivity's
     * confirmation (a second dialog) opens only after the sheet is gone.
     */
    fun showAdapterSheet() {
        if (act.isFinishing || act.isDestroyed || anyDialogShowing()) return
        val pad = act.dp(32)
        val body = LinearLayout(act).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, act.dp(8), pad, act.dp(8))
        }
        var afterDismiss: (() -> Unit)? = null
        SheetItem.values().forEachIndexed { i, item ->
            val b = Button(act).apply {
                text = item.label
                isAllCaps = false
                setTextSize(TypedValue.COMPLEX_UNIT_SP, DIALOG_TEXT_SP)
                setTextColor(Palette.TEXT)
                stateListAnimator = null
                minHeight = act.dp(DIALOG_BUTTON_DP); minimumHeight = act.dp(DIALOG_BUTTON_DP)
                background = pillBg(Color.TRANSPARENT, Palette.STROKE, act.dp(DIALOG_BUTTON_DP) / 2, act.dp(2))
                setOnClickListener {
                    afterDismiss = { item.box?.let { a -> onBoxAction(a) } ?: onExportLogs() }
                    sheetDialog?.dismiss()
                }
            }
            body.addView(b, LinearLayout.LayoutParams(-1, act.dp(DIALOG_BUTTON_DP)).apply {
                topMargin = if (i == 0) 0 else act.dp(SHEET_GAP_DP)
            })
        }
        body.addView(dialogText("Last message", Palette.TEXT_DIM),
            LinearLayout.LayoutParams(-1, -2).apply { topMargin = act.dp(SHEET_GAP_DP) })
        // R15: the full engineering sentence, capped at 3 lines so the sheet stays one screen.
        body.addView(dialogText(lastMessage.ifEmpty { "—" }, Palette.TEXT).apply {
            maxLines = 3
            ellipsize = android.text.TextUtils.TruncateAt.END
        }, LinearLayout.LayoutParams(-1, -2))
        // Reserved slot: the v5.9 logging-reduction item's in-app option (plan_v5.md, "v5.9 logging-
        // reduction item") goes HERE, after bench testing. Deliberately no control yet.
        sheetDialog = AlertDialog.Builder(act, android.R.style.Theme_DeviceDefault_Dialog_Alert)
            .setTitle(ADAPTER_SHEET_TITLE)
            .setView(scrolling(body))
            .setNegativeButton("Close", null)
            .setOnDismissListener {
                sheetDialog = null
                val next = afterDismiss
                afterDismiss = null
                next?.invoke()
            }
            .show()
            .also { it.applyButtonFloors() }
    }

    /**
     * OD-9A (owner, 2026-10-08): saved credentials that differ from the running ones while the box is
     * subscribed reach the iPhone only on the next CT_SUBSCRIBE, so the Save offers "Apply now
     * (restarts the session)". [credentialsApplyOffer] says whether the box is subscribed now;
     * [onApplyCredentialsNow] is Restart Session.
     */
    var credentialsApplyOffer: () -> Boolean = { false }
    var onApplyCredentialsNow: () -> Unit = {}

    /**
     * The credentials popup.
     *
     * Owner-reported (2026-10-08, ux/combined_launch.png): with the on-screen keyboard up, the dialog
     * kept its full height behind the keyboard, so the passphrase field and Save were hidden while
     * typing. Now: the window resizes for the IME (SOFT_INPUT_ADJUST_RESIZE), everything except the
     * button bar — title, reason, both fields — sits in ONE ScrollView that shrinks and keeps the
     * focused field in view, the IME never goes fullscreen/extract, SSID's action key moves to the
     * passphrase, and the passphrase's action key IS Save. The button bar stays at the bottom of the
     * resized window.
     */
    fun showCredentialsDialog(why: String? = null) {
        if (act.isFinishing || act.isDestroyed || anyDialogShowing()) return
        val pad = act.dp(32)
        val body = LinearLayout(act).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, act.dp(16), pad, act.dp(8))
        }
        // The title is in the scroll body, not the dialog's fixed header: under the keyboard every
        // fixed dp is taken from the field area.
        body.addView(dialogText("Vehicle hotspot").apply {
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 28f)
        }, LinearLayout.LayoutParams(-1, -2))
        // Set only when the dialog opened by itself (OD-7A prompt), to say why it appeared.
        if (why != null) body.addView(dialogText(why), LinearLayout.LayoutParams(-1, -2).apply { topMargin = act.dp(12) })

        fun field(labelText: String, value: String, ime: Int): EditText {
            body.addView(TextView(act).apply {
                text = labelText
                setTextColor(Palette.TEXT_DIM)
                setTextSize(TypedValue.COMPLEX_UNIT_SP, DIALOG_TEXT_SP)   // R16: was 16 sp
                isAllCaps = true
                letterSpacing = 0.08f
            }, LinearLayout.LayoutParams(-1, -2).apply { topMargin = act.dp(20) })
            val e = EditText(act).apply {
                setText(value)
                setTextSize(TypedValue.COMPLEX_UNIT_SP, 26f)
                setTextColor(Palette.TEXT)
                inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD
                setSingleLine()
                imeOptions = ime or android.view.inputmethod.EditorInfo.IME_FLAG_NO_EXTRACT_UI or
                    android.view.inputmethod.EditorInfo.IME_FLAG_NO_FULLSCREEN
            }
            body.addView(e, LinearLayout.LayoutParams(-1, -2))
            return e
        }

        // The passphrase is shown, not masked. It is a fixed vehicle-hotspot credential that has to be
        // read off this screen and typed into a phone during bring-up; masking it would help nobody
        // and would hide typos in the one value whose mistyping is silent — the phone simply never
        // joins, and per docs/06 §5.4 the dead network then poisons the next attempt.
        // No channel field: the app always sends a blank channel (owner, 2026-10-07; see init above).
        val eSsid = field("Hotspot SSID", ssid, android.view.inputmethod.EditorInfo.IME_ACTION_NEXT)
        val ePass = field("Passphrase", pass, android.view.inputmethod.EditorInfo.IME_ACTION_DONE)

        var afterDismiss: (() -> Unit)? = null
        val dlg = AlertDialog.Builder(act, android.R.style.Theme_DeviceDefault_Dialog_Alert)
            .setView(scrolling(body))
            .setPositiveButton("Save") { _, _ ->
                val before = ssid to pass
                // Q019 (v5.8 V7): a field left blank keeps the running value — the same rule
                // HotspotPrefs.save applies to the stored one, so the two can never disagree.
                ssid = HotspotMerge.merge(ssid, eSsid.text)
                pass = HotspotMerge.merge(pass, ePass.text)
                // Persist in the same (UI-thread) step that applies them, so a saved value and a
                // used value can never differ. Blanks are not written — see HotspotPrefs.
                HotspotPrefs.save(act.applicationContext, ssid, pass)
                refreshCredSummary()
                onCredentials(ssid, pass)
                // OD-9A: read the link at the Save, offer after this dialog is gone.
                if ((ssid to pass) != before && !credentialsMissing && credentialsApplyOffer()) afterDismiss = {
                    confirm("Apply the new hotspot details now?",
                        "The adapter hands the iPhone the hotspot details when a session starts, so the " +
                        "new ones are used from the next session.\n\nRestart the session now to apply " +
                        "them? CarPlay stops for a moment if it is running.",
                        positive = "Apply now (restarts the session)", negative = "Later") { onApplyCredentialsNow() }
                }
            }
            .setNegativeButton("Cancel", null)
            .setOnDismissListener {
                credDialog = null
                val next = afterDismiss
                afterDismiss = null
                next?.invoke()
            }
            .create()
        dlg.window?.setSoftInputMode(android.view.WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE)
        ePass.setOnEditorActionListener { _, actionId, _ ->
            if (actionId == android.view.inputmethod.EditorInfo.IME_ACTION_DONE) {
                dlg.getButton(AlertDialog.BUTTON_POSITIVE)?.performClick(); true
            } else false
        }
        credDialog = dlg
        dlg.show()
        dlg.applyButtonFloors()
    }

    /**
     * OD-7A: a start found no hotspot credentials. Open the dialog with [why] as its message (the
     * driver line already reads "Set the vehicle hotspot to start."). Safe from any thread; marshals
     * to main, and does nothing if a dialog is already up.
     */
    fun promptForCredentials(why: String) = act.runOnUiThread {
        if (act.isFinishing || act.isDestroyed) return@runOnUiThread
        showCredentialsDialog(why)
    }

    /** Set the credentials from outside the dialog — the `--es ssid/pass` scripted path (R14 trigger). */
    fun setCredentials(newSsid: String?, newPass: String?) = act.runOnUiThread {
        newSsid?.let { ssid = it }
        newPass?.let { pass = it }
        refreshCredSummary()
    }

    // ---- state (from SessionCore; every setter marshals to main) -------------------------------------

    /**
     * The state word, its dot, and [failReason] for the FAILED line. A change of state or reason
     * cancels a showing transient and the state hint (C6), so a stale result cannot cover a new
     * FAILED reason. [message] is the full engineering sentence for the sheet's "Last message".
     */
    fun setState(newState: LinkState, newReason: FailReason?, message: String) = act.runOnUiThread {
        if (act.isFinishing || act.isDestroyed) return@runOnUiThread
        if (newState != state || newReason != failReason) { transient.cancel(); stateHint = null }
        state = newState
        failReason = newReason
        lastMessage = message
        stateText.text = newState.label
        (dot.background as GradientDrawable).setColor(newState.color)
        tracker.onState(newState)
        refreshActions()
        renderLine()
    }

    /** C6 "P": replaces the driver line until the next state change; null clears it. */
    fun setStateHint(hint: String?, message: String) = act.runOnUiThread {
        if (act.isFinishing || act.isDestroyed) return@runOnUiThread
        stateHint = hint
        lastMessage = message
        renderLine()
    }

    /** C6 "T": a one-shot result, shown until it is [ageMs] + 15 s old. [message] goes to "Last message". */
    fun showTransient(text: String, message: String, ageMs: Long = 0L) = act.runOnUiThread {
        if (act.isFinishing || act.isDestroyed) return@runOnUiThread
        transient.show(text, SystemClock.elapsedRealtime() - ageMs)
        lastMessage = message
        renderLine()
    }

    /** C6 "L": the engineering sentence only (log + the sheet's "Last message"); the line is untouched. */
    fun setLastMessage(message: String) = act.runOnUiThread { lastMessage = message }

    /** Show (or, on an empty code, hide) the box's pairing code. */
    fun setPairingCode(code: String) = act.runOnUiThread {
        if (act.isFinishing || act.isDestroyed) return@runOnUiThread
        pairText.text = code
        pairText.visibility = if (code.isEmpty()) View.GONE else View.VISIBLE
    }

    /**
     * The adapter's Bluetooth name, shown as the one to pick on the iPhone for a new pairing. An empty
     * [name] hides the line. Never given the adapter's BT MAC (SessionCore passes only `name`).
     */
    fun setBoxBtName(name: String) = act.runOnUiThread {
        if (act.isFinishing || act.isDestroyed) return@runOnUiThread
        btNameText.text = if (name.isEmpty()) "" else "iPhone pairs with:  $name"
        btNameText.visibility = if (name.isEmpty()) View.GONE else View.VISIBLE
    }

    /** V5: the persistent rekey hint under the adapter name; "" hides it. Never carries key material. */
    fun setHint(hint: String) = act.runOnUiThread {
        if (act.isFinishing || act.isDestroyed) return@runOnUiThread
        hintText.text = hint
        hintText.visibility = if (hint.isEmpty()) View.GONE else View.VISIBLE
    }

    /**
     * A restart is in flight: the primary becomes the dimmed "Restarting…" (still clickable — a repeat
     * press is rejected by `SessionCore.requestRestart` with its own transient line; a disabled button
     * on a dark dash reads as missing rather than busy).
     */
    fun setRestartBusy(busy: Boolean) = act.runOnUiThread {
        if (act.isFinishing || act.isDestroyed) return@runOnUiThread
        tracker.onRestartBusy(busy)
        refreshActions()
    }

    /** MainActivity.onDestroy: drop the transient timer (C6) so nothing posts into a dead view, and
     *  dismiss any dialog still up (D-F9, 2026-10-08) — a dialog outliving its Activity's window is a
     *  leaked window, and a late show against it a BadTokenException; the post guards above also
     *  test `isDestroyed`, which `isFinishing` alone does not cover for a system-driven destroy. */
    fun release() {
        handler.removeCallbacks(expireTransient)
        for (d in listOf(credDialog, sheetDialog, confirmDialog)) runCatching { if (d?.isShowing == true) d.dismiss() }
        credDialog = null; sheetDialog = null; confirmDialog = null
    }

    private companion object {
        /** R16 dialog floors: body/label text and button height. */
        const val DIALOG_TEXT_SP = 24f
        const val DIALOG_BUTTON_DP = 76
        /** C3: gap between the sheet's buttons. */
        const val SHEET_GAP_DP = 25
    }
}
