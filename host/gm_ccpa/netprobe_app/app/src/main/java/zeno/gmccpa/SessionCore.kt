package zeno.gmccpa

import android.content.Context
import android.content.Intent
import android.hardware.usb.UsbManager
import android.os.Handler
import android.os.Looper
import zeno.gmccpa.logging.CapturePrefs
import zeno.gmccpa.logging.LogCapture
import zeno.gmccpa.logging.SessionSummary
import zeno.gmccpa.logging.SessionTrace
import zeno.gmccpa.ocbm.Ocbm
import zeno.gmccpa.ocbm.OcbmProbe
import zeno.gmccpa.ocbm.PhoneIdent
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.atomic.AtomicBoolean

/**
 * PROCESS-scoped session core (v5.7 U4: F021, F022, F023, F024, F096, F025, F027, F028, F031, [C2]).
 *
 * The receiver and the OCBM probe were already process-scoped ([SessionHolder]); everything that
 * DROVE them was not. The supervisor, the `netprobe-cmd` executor, `sessionUp` and the
 * VehicleStateWatcher (which held `this`) all lived in MainActivity, and its onDestroy stopped the
 * supervisor, shut the executor and stopped the watcher — while the receiver and probe callbacks
 * kept pointing at the dead instance, whose supervisor then dropped every post. A session that ended
 * with no MainActivity got no grace window, no ladder and no discovery pause (F021a); Drive left the
 * full UI because the stopped watcher ignored onSessionUp (F022); a recreated Activity's rebind
 * seeded only SESSION_UP (F021b); the old executor's queue overlapped the new one (F023); the dead
 * Activity stayed pinned (F024); and every rebind started another watcher (F096).
 *
 * Now there is ONE core per process, built from applicationContext and held by [SessionHolder]. It
 * owns the supervisor (never stopped while the process lives), the one `netprobe-cmd` executor,
 * `sessionUp`, and the current receiver's VehicleStateWatcher. Receiver and probe callbacks are bound
 * ONCE, to the core. MainActivity is a view plus the verb dispatcher: it [attach]es its [LauncherUi]
 * on create and [detach]es it (compare-and-clear) on destroy; every UI side effect goes through the
 * attached view and tolerates its absence, and the last state shown is stored so a new view renders
 * it at once. LauncherUi's setters marshal to the main thread themselves.
 */
class SessionCore internal constructor(private val app: Context) {

    // ---- view -------------------------------------------------------------------------------------

    /**
     * What the launcher screen shows, stored so a newly attached view renders it (plan v5.9 C6).
     * [state]/[failReason] drive the persistent driver line ("P"); [stateHint] is a per-state "P"
     * override cleared by the next state; [transient]/[transientAt] (elapsedRealtime) is a one-shot
     * result ("T") replayed onto a re-attached view only while younger than 15 s; [lastMessage] is the
     * full engineering sentence of the last status/detail ("L"), shown in the sheet's "Last message".
     */
    private data class Screen(val state: LinkState? = null, val failReason: FailReason? = null,
                              val stateHint: String? = null,
                              val transient: String? = null, val transientAt: Long = 0L,
                              val lastMessage: String = "",
                              val pairing: String = "", val btName: String = "",
                              /** V5 rekey hint; "" = hidden. Survives the per-phase detail rewrites. */
                              val hint: String = "")

    private val slot = ViewSlot<LauncherUi, Screen> { v, s ->
        if (s.state != null) v.setState(s.state, s.failReason, s.lastMessage) else v.setLastMessage(s.lastMessage)
        if (s.stateHint != null) v.setStateHint(s.stateHint, s.lastMessage)
        val now = android.os.SystemClock.elapsedRealtime()
        if (s.transient != null && TransientLine.replayable(s.transientAt, now))
            v.showTransient(s.transient, s.lastMessage, now - s.transientAt)
        v.setPairingCode(s.pairing)
        v.setBoxBtName(s.btName)
        v.setHint(s.hint)
    }

    /**
     * Hotspot credentials last read from a view. NEVER logged (no toString of the values): this
     * carries the vehicle hotspot passphrase. No channel: the app sends none (owner, 2026-10-07).
     */
    class Creds(val ssid: String, val pass: String) {
        override fun toString() = "Creds(<redacted>)"
        /** OD-7A: either value blank — autoStart will not hand off with these. */
        val missing: Boolean get() = ssid.isBlank() || pass.isBlank()
    }
    @Volatile private var lastCreds: Creds? = null

    /**
     * OD-7A: a start (autoStart or a USB attach) was refused for missing credentials. Resumed by the
     * dialog's Save ([onCredentialsSaved]), re-prompted by [attach], withdrawn by Stop (CredsLatch.kt).
     */
    private val credsLatch = CredsLatch()

    /** The credentials on [ui] right now. Call on the main thread (the dialog writes them there). */
    fun credsOf(ui: LauncherUi): Creds = Creds(ui.ssid, ui.pass).also { lastCreds = it }

    /**
     * Attach [ui] as the view and render the stored state onto it. Called from MainActivity.onCreate,
     * on the main thread. Replaces the old rebind seed (MA "re-attached … seed"): the core never lost
     * its phase, so there is nothing to reconstruct.
     */
    fun attach(ui: LauncherUi) {
        lastCreds = Creds(ui.ssid, ui.pass)
        slot.attach(ui)
        ui.setRestartBusy(restartInProgress.get())
        // A start refused for missing credentials while no view was attached (headless) asks here.
        if (credsLatch.repromptOnAttach(ui.credentialsMissing)) ui.promptForCredentials(CREDS_WHY)
        val s = slot.stored()
        emit("launcher view attached to the session core (stored: ${s?.state?.label ?: "nothing yet"})")
    }

    /** Compare-and-clear: a recreated MainActivity's onCreate can precede the old one's onDestroy. */
    fun detach(ui: LauncherUi) {
        if (slot.detach(ui)) emit("launcher view detached — the session core runs on headless")
    }

    /**
     * The state word + its FAILED reason ("P"; plan v5.9 C6). The screen shows [driverLine], never
     * [detail]: the engineering sentence goes to the log (`ui: <STATE> — <detail>`, unless the caller
     * already logged it — [logged], the supervisor's to() line) and to the sheet's "Last message". A
     * change of state or reason drops a showing transient and the state hint.
     */
    private fun setStatus(state: LinkState, detail: String, failReason: FailReason? = null, logged: Boolean = false) {
        val reason = if (state == LinkState.FAILED) failReason ?: FailReason.OTHER else null
        if (!logged) emit("ui: ${state.label} — $detail")
        slot.update({ prev ->
            val p = prev ?: Screen()
            val changed = p.state != state || p.failReason != reason
            p.copy(state = state, failReason = reason, lastMessage = detail,
                stateHint = if (changed) null else p.stateHint,
                transient = if (changed) null else p.transient)
        }) { it.setState(state, reason, detail) }
        lastState = state
        lastStallForNotice = when (reason) {
            FailReason.STALLED_PHONE -> StallCause.PHONE
            FailReason.STALLED_ADAPTER -> StallCause.ADAPTER
            else -> null
        }
        refreshNotice()
    }

    // ---- D6 / OD-9E: the one state-driven notification ---------------------------------------------

    @Volatile private var lastState: LinkState? = null
    @Volatile private var lastStallForNotice: StallCause? = null

    /**
     * SessionCore's state funnel -> the status notification (plan v5.9 D6, OD-9E). Posted only while
     * something holds CarPlaySessionService (CarPlaySessionService.update keeps it otherwise). No PII.
     */
    private fun refreshNotice() {
        runCatching {
            zeno.gmccpa.av.CarPlaySessionService.update(app,
                zeno.gmccpa.av.StatusNotice.of(lastState, lastStallForNotice, BoxRelink.owner.get() != null))
        }
    }

    /** "L": an engineering progress sentence (Bluetooth phase, projection, …) — log + "Last message" only. */
    private fun setDetail(detail: String) {
        emit("ui: — $detail")
        slot.update({ (it ?: Screen()).copy(lastMessage = detail) }) { it.setLastMessage(detail) }
    }

    /**
     * "T": a one-shot result (box actions, export, forget, a rejected press, no adapter) on the
     * driver line for 15 s; [detail] (default: the same text) is logged and kept as "Last message".
     */
    fun showResult(text: String, detail: String = text) {
        emit("ui: result — $detail")
        val at = android.os.SystemClock.elapsedRealtime()
        slot.update({ (it ?: Screen()).copy(transient = text, transientAt = at, lastMessage = detail) }) {
            it.showTransient(text, detail)
        }
    }

    /** "P" override for the CURRENT state (the 30 s claim hints); cleared by the next state change. */
    private fun setStateHint(hint: String, detail: String) {
        emit("ui: hint — $detail")
        slot.update({ (it ?: Screen()).copy(stateHint = hint, lastMessage = detail) }) { it.setStateHint(hint, detail) }
    }

    private fun setPairingCode(code: String) =
        slot.update({ (it ?: Screen()).copy(pairing = code) }) { it.setPairingCode(code) }

    private fun setBoxBtName(name: String) =
        slot.update({ (it ?: Screen()).copy(btName = name) }) { it.setBoxBtName(name) }

    /** The persistent V5 rekey hint line ("" hides it). Not touched by [setStatus]/[setDetail]. */
    private fun setHint(hint: String) =
        slot.update({ (it ?: Screen()).copy(hint = hint) }) { it.setHint(hint) }

    // ---- V5: per-install accessory key (Q001, OD-8B) ---------------------------------------------------

    private val keyLog = object : AccessoryKey.Log {
        private val l = ProbeLog.sub("akey")
        override fun i(msg: String) = l.i(msg)
        override fun e(msg: String) = l.e(msg)
    }
    private val keyDir: java.io.File get() = app.noBackupFilesDir
    private val peerStoreFile: java.io.File get() = java.io.File(app.filesDir, "carplay_peers.bin")
    /** The last link report handed to the supervisor, re-used when a forget has to re-report it. */
    @Volatile private var lastMfiProven = false

    /**
     * OD-8B (owner, 2026-10-07): on the first OCBM link after an upgrade whose seed file did not
     * exist yet (or was corrupt), run Forget Pairing automatically, ONCE, then keep prompting the
     * driver to forget the car on the iPhone until a pair-verify succeeds under the new key. The
     * decisions live in [RekeyFlow] (host-tested); this is the wiring. The run is queued on the
     * command thread so it serialises behind the start/attach/relink that observed the link. A Stop
     * that runs first leaves the state PENDING with the per-process attempt spent: the next process
     * start retries. Window left open: a pair-verify that lands between the run's re-check and
     * `beginRetire()` inside [forgetPairing] (which spans the box's forgetPhone round trip) is wiped
     * with the old pairing — `onForgetDone` re-asserts FORGOTTEN so the hint stays up and the driver
     * re-pairs once more; the receiver is stopped before the peer store is deleted, so a half-saved
     * pairing is never raced.
     */
    private val rekey = RekeyFlow(
        store = object : RekeyFlow.Store {
            override fun state() = AccessoryKey.rekeyState(keyDir)
            override fun markForgotten(force: Boolean) = AccessoryKey.markForgotten(keyDir, force)
            override fun clear() = AccessoryKey.clearRekey(keyDir)
        },
        forget = { forgetPairing(auto = true) },
        linkSubscribed = { ocbmProbe?.client?.subscribed == true },
        reportLinked = { supervisor.onBoxLinked(lastMfiProven) },
        emit = { emit(it) }
    )

    /** Render the hint that matches the persisted rekey state (idempotent; called on every receiver build). */
    private fun showRekeyHint(state: AccessoryKey.Rekey) = setHint(
        when (state) {
            AccessoryKey.Rekey.NONE -> ""
            AccessoryKey.Rekey.PENDING -> REKEY_HINT_PENDING
            AccessoryKey.Rekey.FORGOTTEN -> REKEY_HINT_FORGOTTEN
        }
    )

    /** Every OCBM link-up site: remember the report, then let [RekeyFlow] queue its one run. */
    private fun rekeyAutoForgetIfDue(mfiProven: Boolean) {
        lastMfiProven = mfiProven
        rekey.onLinkUp { body -> runAsync(body) }
    }

    /**
     * Pair-verify completed: the session is UP for the supervisor (A-F1, 2026-10-08 — until then
     * the accept alone put the supervisor in SESSION_UP, so any hotspot client that opened `:7011`
     * paused discovery and cancelled every recovery clock; see [UnverifiedConnRule]). The screen
     * and the A/V consumers were stood up at accept ([bindReceiver]); only [sessionUp] and the
     * supervisor move here. BEFORE the rekey early return: that returns for every non-rekey session.
     * Then, under the current key, the rekey migration (if any) is over.
     */
    private fun onPairVerified(rx: CarPlayRx, gen: Long) {
        if (!isCurrentReceiver(rx)) return
        sessionUp = true
        // The post re-checks on the scheduler that rx is still the current receiver.
        supervisor.onSessionUp { isCurrentReceiver(rx) }
        if (!rekey.onPairVerified()) return
        setHint("")
        emit("accessory key: pair-verify succeeded under the new key (gen=$gen) — re-pair hint cleared")
    }

    /**
     * BOTH halves, together: the box's BR/EDR bond and this app's Ed25519 peer store. The body of the
     * driver's Forget Pairing (BoxAction.FORGET_PHONE), shared with the one automatic run
     * ([rekeyAutoForgetIfDue]). Returns true when both halves were cleared.
     */
    private fun forgetPairing(auto: Boolean, mac: String? = null): Boolean {
        val rekeyWas = AccessoryKey.rekeyState(keyDir)
        setPairingCode("")
        showResult("Clearing the pairing on both sides…", "clearing the pairing on both sides — also forget the car on the iPhone")
        // D020: with a MAC (the ocbm_forget verb) the box clears that one bond plus ALL of its AirPlay
        // peers, and this app clears its whole peer store: carplay_peers.bin is Rust-owned and has no
        // MAC -> peer mapping, so it cannot be filtered per device (plan v5.9; not attempted).
        if (!mac.isNullOrBlank()) ProbeLog.sub("forget").w("forget for one device: every OTHER iPhone bonded to this adapter must ALSO be forgotten (on the iPhone) and re-paired — the app's pairing store is cleared whole")
        val boxOk = ocbmProbe?.forgetPhone(mac) == true
        emit(if (boxOk) "forget: adapter bond cleared" else "forget: adapter bond NOT cleared")
        val hadRx = cpRx != null
        // Supervisor told the teardown is deliberate BEFORE it happens; fence + sessionUp first.
        sessionUp = false
        beginRetire()
        val drained = try {
            supervisor.onStopped()
            // Delete only behind a confirmed drain: a live core's save_peer resurrects the file.
            val d = (cpRx?.stop() ?: true) || waitForPeerStoreQuiet()
            cpRx = null
            d
        } finally { endRetire() }
        sessionUp = false
        tearDownSessionConsumers("pairing forgotten")
        retireVehicle("receiver stopped (forget pairing)")
        val peers = peerStoreFile
        val appOk = if (!drained) {
            emit("forget: a receiver pump is still in the native core — NOT deleting the peer store")
            false
        } else !peers.exists() || peers.delete()
        emit(if (appOk) "forget: app CarPlay pairings cleared" else "forget: could NOT delete ${peers.name}")
        // V5: a pending rekey is consumed by a COMPLETE forget — automatic or driver-pressed (force
        // re-assert against a racing clear: see RekeyFlow.onForgetDone). A partial clear leaves it
        // PENDING, so the hint keeps saying "press Forget Pairing" until it is clean.
        rekey.onForgetDone(rekeyWas, boxOk, appOk)
        if (hadRx) startCarPlayRx()   // back up clean, advertising a receiver with no peers
        else showRekeyHint(AccessoryKey.rekeyState(keyDir))   // startCarPlayRx renders it otherwise
        if (boxOk && appOk) showResult("Pairing cleared. Now forget this car on the iPhone.",
            if (auto) "accessory key changed — old pairing cleared on both sides; now forget this car on the iPhone, then pair again"
            else "pairing cleared on both sides — now forget this car on the iPhone")
        else showResult("Pairing only partly cleared. Do not re-pair yet.",
            "pairing only PARTLY cleared (adapter=${if (boxOk) "cleared" else "NOT cleared"}, app=${if (appOk) "cleared" else "NOT cleared"}) — see the log; do not re-pair until it is clean")
        return boxOk && appOk
    }

    /** Serialises name updates against the clears (BoxNameGate.kt). Writes the screen via [setBoxBtName]. */
    private val boxName = BoxNameGate { n -> setBoxBtName(n) }

    /**
     * The link a MGMT_INFO reply was read from is still the live one: the current probe, not stopped,
     * still holding [c], and [c] has not taken its transport-death edge (`hostGoneDelivered`, CASed by
     * onBoxTransportDead BEFORE it clears the name). A box-side HOST_GONE does not set that flag
     * (OcbmClient KDoc), so the same box's re-read after one still counts as live.
     */
    private fun linkStillLive(p: OcbmProbe?, c: zeno.gmccpa.ocbm.OcbmClient?): Boolean =
        p != null && c != null && SessionHolder.ocbmProbe === p && !p.stopped && p.client === c && !c.hostGoneDelivered.get()

    /**
     * MGMT_INFO from the bring-up snapshot (OcbmProbe, on its `ops` thread — never `ocbm-read`) or
     * the HOST_GONE re-read (command executor; the INFO button went in plan v5.9 D002). Only the box's Bluetooth `name` reaches
     * the screen; `bt_mac` and every other field stay out of it. [p]/[c] are the probe and client the
     * reply was read from, captured BEFORE the read: a reply from a link that has since died, been
     * stopped or been replaced is dropped (BoxNameGate). [linkUp] = this is the snapshot of a NEW
     * link: a missing reply or name then clears the line, so a swapped adapter that does not answer
     * can never show the previous unit's name. Otherwise a reply with no name changes nothing.
     */
    private fun onBoxMgmtInfo(p: OcbmProbe?, c: zeno.gmccpa.ocbm.OcbmClient?, json: String?, linkUp: Boolean) {
        val name = zeno.gmccpa.ocbm.MgmtInfo.btName(json)
        if (name == null) {
            if (linkUp && linkStillLive(p, c)) clearBoxBtName("the new link's MGMT_INFO carried no Bluetooth name")
            return
        }
        when (boxName.offer(name) { linkStillLive(p, c) }) {
            BoxNameGate.Offer.SHOWN -> emit("adapter Bluetooth name: $name (shown on the launcher screen for new iPhone pairings)")
            BoxNameGate.Offer.STALE -> emit("adapter Bluetooth name reply ignored — its link is gone (died, stopped or replaced)")
            BoxNameGate.Offer.UNCHANGED -> Unit
        }
    }

    /**
     * The adapter went away (or Stop): the name on screen no longer names a box we are linked to.
     * Callers make the link read as not live FIRST (see BoxNameGate), so a late reply cannot repaint.
     */
    private fun clearBoxBtName(why: String) {
        if (boxName.clear()) emit("adapter Bluetooth name cleared from the launcher screen — $why")
    }

    private fun setRestartBusy(busy: Boolean) { slot.current()?.setRestartBusy(busy) }

    // ---- process-scoped state -----------------------------------------------------------------------

    private var cpRx: CarPlayRx?
        get() = SessionHolder.cpRx
        set(v) { SessionHolder.cpRx = v }

    private val ocbmProbe: OcbmProbe? get() = SessionHolder.ocbmProbe

    /** Set once the phone has a live session, so MainActivity.onResume can restore the CarPlay screen. */
    @Volatile var sessionUp = false
        private set

    /**
     * The CURRENT receiver's vehicle-state watcher (drive-restricted UI, day/night), or null with no
     * receiver. A VehicleStateWatcher cannot restart (`io` is shut by stop() and `stopped` is never
     * cleared), so each receiver start builds a FRESH one in [startCarPlayRx] — the only build site —
     * and receiver-off stops and drops it ([retireVehicle]). Every reader reads it once into a local
     * and tolerates null.
     */
    @Volatile private var vehicle: zeno.gmccpa.av.VehicleStateWatcher? = null

    /** Period of the `## STATUS` anchor while the receiver is up. See SessionTrace.Board. */
    private val BOARD_TICK_MS = 60_000L

    private val main = Handler(Looper.getMainLooper())

    // Single-thread executor: a fresh Thread per command let two Starts delivered close together both
    // observe `cpRx == null` and each build a CarPlayRx (EADDRINUSE on :7011, RX_UNHEALTHY, two
    // adverts). ONE per process now (F023): a per-Activity executor let a dead generation's queued
    // tasks overlap the new generation's. Never shut down. (The separate `netprobe-recover` executor
    // went with the Recover button, plan v5.9 UI-1: Restart Session aborts a parked claim on the UI
    // thread before it queues, so it never waits behind one.)
    private val cmdExecutor: ExecutorService = Executors.newSingleThreadExecutor { r ->
        Thread(r, "netprobe-cmd").apply { isDaemon = true }
    }

    /** Capture-scope changes (F028): LogCapture.stop joins the pump for up to 5 s — never on main. */
    private val captureExecutor: ExecutorService = Executors.newSingleThreadExecutor { r ->
        Thread(r, "netprobe-capture").apply { isDaemon = true }
    }

    /** Guards probe get-or-create and every probe null-out (F027). */
    private val probeLock = Any()

    /** The relay every receiver is built with (F025): resolves the current probe on each call. */
    private val relay = ProcessMfiRelay(
        resolve = { SessionHolder.ocbmProbe?.mfiRelay() },
        linked = { SessionHolder.ocbmProbe?.hasLink() == true },
        onNoProbe = { msg -> ProbeLog.sub("mfi").e("$msg — the phone's auth-setup / iAP2 auth will fail on this") },
    )

    // ---- supervisor ---------------------------------------------------------------------------------

    /**
     * Owns the session phase and the recovery ladder. Process-scoped and never stopped (v5.7 U4): the
     * receiver outlives every Activity, and so must whatever answers its session-down.
     */
    val supervisor: SessionSupervisor = SessionSupervisor(object : SessionSupervisor.Actions {
        override fun reannounce(): Boolean = cpRx?.reannounce() ?: false
        // Cheap and non-blocking: `subscribed` is the app's own view of the link, and the
        // heartbeat's write-failure path clears it, so a dead adapter reads false here.
        override fun boxLinkAlive(): Boolean = ocbmProbe?.client?.subscribed == true
        // Both box verbs are DISPATCHED, never awaited. OcbmProbe serialises every box command on one
        // thread that `awaitClaimable` can hold for up to ten minutes; awaiting one from the
        // supervisor's scheduler would freeze every timer in the state machine. The dispatch verdict
        // IS the return.
        override fun setBoxRadios(on: Boolean): Boolean {
            val p = ocbmProbe ?: return false
            return runAsync { runCatching { p.setRadios(on) }.onFailure { emit("CT_RADIO failed: ${it.message}") } }
        }
        override fun restartBoxWireless(): Boolean {
            val p = ocbmProbe ?: return false
            return runAsync { runCatching { p.disconnectPhone() }.onFailure { emit("restart-wireless failed: ${it.message}") } }
        }
        override fun pauseDiscovery() { cpRx?.pauseDiscovery() }
        override fun resumeDiscovery() { cpRx?.resumeDiscovery() }
        override fun setDiscoveryBackoff(enabled: Boolean) { cpRx?.setDiscoveryBackoff(enabled) }
        // What the box was actually handed (the probe's copy), not the screen's field.
        override fun hotspotSsid(): String? = ocbmProbe?.wifiSsid
        override fun adapterState(): AdapterState = readAdapterState()
        // The three-part predicate (2026-10-06): HELLO'd link, not subscribed, and NEVER subscribed.
        override fun boxLinkedUnsubscribed(): Boolean {
            val p = ocbmProbe ?: return false
            val c = p.client ?: return false
            return p.hasLink() && !c.subscribed && !c.everSubscribed
        }
        // Dispatched, never awaited — the same rule and the same verdict as setBoxRadios.
        override fun subscribeBox(): Boolean {
            val p = ocbmProbe ?: return false
            return runAsync {
                runCatching { p.subscribeExisting { SessionHolder.cpRx?.sessionLive != true } }
                    .onFailure { emit("deferred CT_SUBSCRIBE failed: ${it.message}") }
            }
        }
        // Stored by the core and forwarded to the attached view, if any; a view attached later
        // renders the stored value. LinkState stays the launcher's colour/label vocabulary.
        // The supervisor's to() already logged the transition, so setStatus does not log it again.
        override fun report(phase: RxPhase, detail: String, stall: StallCause?) {
            lastStall = stall
            setStatus(linkStateFor(phase), detail, failReasonFor(phase, stall), logged = true)
        }
    })

    /** The cause of the last report into STALLED (null otherwise); plan v5.9 rev c C4. */
    @Volatile private var lastStall: StallCause? = null

    /** C6/R10: the FAILED phases' reasons; null for every non-failure phase. */
    private fun failReasonFor(p: RxPhase, stall: StallCause?): FailReason? = when (p) {
        RxPhase.RX_UNHEALTHY -> FailReason.RX_UNHEALTHY
        RxPhase.BOX_UNHEALTHY -> FailReason.BOX_UNHEALTHY
        RxPhase.STALLED -> when (stall) {
            StallCause.PHONE -> FailReason.STALLED_PHONE
            StallCause.ADAPTER -> FailReason.STALLED_ADAPTER
            null -> FailReason.OTHER
        }
        else -> null
    }

    /** [RxPhase] is the truth; [LinkState] is how the launcher screen renders it. */
    private fun linkStateFor(p: RxPhase): LinkState = when (p) {
        // Restart Session tells the supervisor onStopped() first, and its to(IDLE) report lands
        // AFTER the RESTARTING status the command set; the flag is the truth about a restart in flight.
        RxPhase.IDLE -> if (restartInProgress.get()) LinkState.RESTARTING else LinkState.STOPPED
        RxPhase.RX_READY -> LinkState.SEARCHING
        RxPhase.RX_UNHEALTHY, RxPhase.BOX_UNHEALTHY -> LinkState.FAILED
        RxPhase.BOX_LINKED -> LinkState.CLAIMING
        RxPhase.ARMED, RxPhase.GRACE -> LinkState.WAITING
        RxPhase.BT_PAIRING -> LinkState.PAIRING
        RxPhase.BT_PAIRED, RxPhase.HANDOFF_SENT -> LinkState.PHONE_DETECTED
        RxPhase.INBOUND_EXPECTED -> LinkState.STARTING
        RxPhase.SESSION_UP, RxPhase.BOX_LOST_SESSION_UP -> LinkState.LIVE
        RxPhase.STALLED -> LinkState.FAILED
    }

    // ---- executors ----------------------------------------------------------------------------------

    /** Returns false when the command was dropped, so a caller holding state for it can release it. */
    fun runAsync(block: () -> Unit): Boolean {
        try {
            cmdExecutor.execute {
                try { block() } catch (t: Throwable) { emit("!! run aborted: ${t.javaClass.simpleName}: ${t.message}") }
            }
            return true
        } catch (_: RejectedExecutionException) {
            // The process-scoped executor is never shut down; kept so a caller's state is released
            // rather than thrown back into whichever box or receiver thread made the call.
            emit("command dropped — the command executor refused it")
            return false
        }
    }

    /**
     * F031: carplay_stop stays serialized on `netprobe-cmd`, so it can wait behind a parked claim.
     * Say so at once instead of waiting silently for up to ten minutes.
     */
    fun noteIfQueuedBehindClaim(what: String) {
        if (SessionHolder.ocbmProbe?.claimWaitActive == true) {
            val msg = "$what queued behind the adapter claim (up to 10 min) — press Stop to abort it"
            showResult("Waiting for the adapter claim. Press Stop to cancel it.", msg)
        }
    }

    // ---- the receiver ---------------------------------------------------------------------------------

    /** The only place a receiver — and therefore a VehicleStateWatcher — is built. */
    private fun startCarPlayRx() {
        val cur = cpRx
        if (cur == null) {
            emit(""); emit("==================== CARPLAY RX (Car bit + connect-out) ====================")
            // The relay is the process-delegating one (F025), so the receiver never binds to a probe
            // that a later ocbm_stop / restart replaces. Callbacks are bound once, to this core.
            // V5 (Q001): the per-install Ed25519 seed, created on first use. A seed created while a
            // peer store already exists is an upgrade — its pairings are now invalid; the persisted
            // rekey state drives the one automatic Forget Pairing and the hint line.
            val peers = peerStoreFile
            val key = AccessoryKey.loadOrCreate(keyDir, peersPresent = peers.exists(), log = keyLog)
            showRekeyHint(key.rekey)
            val rx = CarPlayRx(
                app,
                mfiRelay = relay,
                // Read through SessionHolder at call time, not captured: the probe is rebuilt on every
                // Stop->Start and Restart, and the gate must see the CURRENT one.
                nudgeAllowed = { SessionHolder.ocbmProbe?.hasLink() == true },
                peerFile = peers.absolutePath,
                edSeed = key.seed
            )
            bindReceiver(rx)
            // A FRESH watcher, built, assigned and started BEFORE rx.start() (v5.7.2, Reviewers 1 and
            // 3): rx.start() opens the accept loop at once, and a phone already re-dialling can deliver
            // onSessionUp on the accept/pump thread inside the gap. With the watcher assigned first,
            // that first callback always finds it (F022). Bind the Car API alongside the receiver;
            // safe if android.car is absent — the watcher degrades to night-mode-only.
            retireVehicle("replaced by a fresh watcher for a new receiver")   // F096: one per receiver start
            // D-F3: neither the watcher's construction nor its start may stop the receiver from
            // starting (the Car callbacks are lazy now; this is the belt to that braces).
            val w = runCatching { zeno.gmccpa.av.VehicleStateWatcher(app) }
                .onFailure { emit("vehicle-state watcher unavailable (${it.javaClass.simpleName}: ${it.message}) — levers not driven") }
                .getOrNull()
            vehicle = w
            w?.start()
            cpRx = rx; rx.start()
            // The receiver coming up is the start of the app's session, so the periodic status anchor
            // starts here. Idempotent; stopped by [stopEverything].
            SessionTrace.Board.startTicker(BOARD_TICK_MS)
            reportReceiverHealth(rx)
        }
    }

    /** Receiver-off: stop and drop the current watcher, if any. The core is its only stopper. */
    private fun retireVehicle(why: String) {
        val w = vehicle ?: return
        vehicle = null
        emit("vehicle-state watcher stopped — $why")
        runCatching { w.stop() }
    }

    /**
     * Run the receiver's readiness probe and hand the verdict to the supervisor and the log.
     *
     * Deliberately runs the moment the receiver starts and BEFORE the box is asked to wake its
     * radios. Gated on [isCurrentReceiver] INSIDE the supervisor's post (v5.7.1): a verdict for a
     * receiver that carplay_stop or a restart has since retired — whose `listening` FAIL would
     * otherwise land as RX_UNHEALTHY after onStopped's IDLE — is dropped there. Called only from
     * [startCarPlayRx] since the Recover button was merged into Restart Session (plan v5.9 UI-1).
     */
    private fun reportReceiverHealth(rx: CarPlayRx) {
        val current = { isCurrentReceiver(rx) }
        val checks = runCatching { rx.selfTest() }.getOrElse {
            emit("!! receiver self-test threw: ${it.javaClass.simpleName}: ${it.message}")
            supervisor.onReceiverReady(false, "self-test threw", stillCurrent = current)
            return
        }
        emit("---- receiver readiness ----")
        checks.forEach { emit("  ${if (it.ok) "OK  " else "FAIL"}  ${it.name}: ${it.detail}") }
        val bad = checks.filter { !it.ok }
        supervisor.onReceiverReady(
            bad.isEmpty(),
            if (bad.isEmpty()) "receiver ready to accept a session"
            else bad.joinToString("; ") { it.name },
            stillCurrent = current,
        )
    }

    /**
     * Whether [rx] may still declare a session: it is the current receiver and no deliberate stop is
     * retiring it. Read order matters — [SessionHolder.retiringRx] FIRST, then `cpRx`: [endRetire]
     * clears the fence only after `cpRx` was nulled.
     */
    private fun isCurrentReceiver(rx: CarPlayRx): Boolean =
        SessionHolder.retiringRx !== rx && SessionHolder.cpRx === rx

    /** Raise the retire fence on the current receiver, BEFORE `supervisor.onStopped()`. */
    private fun beginRetire(): CarPlayRx? = cpRx.also { SessionHolder.retiringRx = it }

    /** Drop the fence once `cpRx` no longer points at the retired receiver. */
    private fun endRetire() { SessionHolder.retiringRx = null }

    /** The receiver's callbacks, bound ONCE per receiver, to this core (never to an Activity). */
    private fun bindReceiver(rx: CarPlayRx) {
        // On the receiver's pool thread, per its "stand consumers up NOW" rule; only the overlay state
        // hops to the main looper. Identity-checked here AND inside the supervisor's post.
        rx.onSessionUp = { g ->
            if (!isCurrentReceiver(rx)) emit("ignoring session-up (gen=$g) from a receiver that is being stopped or was replaced")
            else {
                zeno.gmccpa.av.CarPlayActivity.linkUp(g)
                // Screen + consumers only (A-F1): [sessionUp] and the supervisor wait for
                // [onPairVerified], so an unverified connection stands nothing up but the screen,
                // which the pump's deadline takes down again ([onCarPlaySessionDown]).
                showCarPlayScreen()
                val w = vehicle
                if (w != null) w.onSessionUp() else emit("session-up with no vehicle-state watcher — levers not asserted")
            }
        }
        rx.onSessionDown = { reason -> onCarPlaySessionDown(reason) }
        rx.onPairVerified = { g -> onPairVerified(rx, g) }
        rx.onLinkQuiet = { g -> zeno.gmccpa.av.CarPlayActivity.linkQuiet(g) }
        rx.onLinkResumed = { g -> zeno.gmccpa.av.CarPlayActivity.linkResumed(g) }
        rx.onStalled = { n -> supervisor.onStalled(n) }
        rx.onListenerLost = { why ->
            // Same verdict reportReceiverHealth would reach (`listening` false), delivered on the edge.
            emit("!! receiver listener retired: $why")
            supervisor.onReceiverReady(false, "listener retired: $why", stillCurrent = { isCurrentReceiver(rx) })
        }
        rx.onDialAccepted = { supervisor.onDialAccepted() }
    }

    /**
     * The screen ONLY: the media card and CarPlayActivity. Never touches [sessionUp] or the
     * supervisor (those move in [onPairVerified]). Idempotent: CarPlayActivity is `singleTop` on its
     * own task affinity; FLAG_ACTIVITY_NEW_TASK makes the affinity take effect. Launched from
     * applicationContext, so it works with no MainActivity alive; background activity launch is
     * judged on uid state, as before.
     */
    fun showCarPlayScreen() {
        // Tell the AAOS media card a session exists. Deliberately NOT a flip to PLAYING.
        zeno.gmccpa.av.CarPlayMediaBrowserService.onSessionUp()
        setPairingCode("")
        app.startActivity(
            Intent(app, zeno.gmccpa.av.CarPlayActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        )
    }

    /**
     * The phone's control connection went away. Takes the screen down with the session and hands the
     * reason to the supervisor — which now runs whether or not a MainActivity exists (F021a).
     */
    fun onCarPlaySessionDown(reason: EndReason = EndReason.CLOSED) {
        val hadVerified = sessionUp
        sessionUp = false
        tearDownSessionConsumers("control connection went away")
        // A-F1: a generation the pump closed at the unverified deadline was never a session — no
        // GRACE, no ladder reset; the supervisor's phase stands. A phone that closed early on its
        // own arrives as CLOSED and keeps its grace window. But if a VERIFIED session was up (gen1
        // superseded in the release/decrement gap by a newcomer that then timed out), this is the
        // supervisor's only session-down for it and must go through ([UnverifiedConnRule]).
        if (!UnverifiedConnRule.reachesSupervisor(reason, hadVerified)) {
            emit("unverified control connection closed at the deadline — screen taken down, supervisor untouched")
            return
        }
        supervisor.onSessionDown(reason)   // owns the state word and the grace window
    }

    /** Everything that must go down WITH a CarPlay session, minus the supervisor. */
    private fun tearDownSessionConsumers(why: String) {
        zeno.gmccpa.av.CarPlayActivity.onSessionEnded(why)
        zeno.gmccpa.av.CarPlayMediaBrowserService.onSessionDown()
        // Both vehicle levers are session-scoped, so drop the sent-state and let onSessionUp push fresh.
        vehicle?.onSessionDown()
        setPairingCode("")
    }

    /**
     * MainActivity resumed. A USB permission dialog requested from the background is re-raised, and a
     * live session gets its screen back (screen only — see [showCarPlayScreen]).
     */
    fun onViewResumed() {
        ocbmProbe?.rerequestNow()
        if (sessionUp && cpRx?.sessionLive == true) {
            emit("session still live — restoring the CarPlay screen")
            showCarPlayScreen()
        } else if (sessionUp) {
            emit("stale sessionUp with no live control connection — clearing instead of restoring")
            onCarPlaySessionDown()
        }
    }

    /** MainActivity.onConfigurationChanged (uiMode is in its configChanges). */
    fun onConfigurationChanged(cfg: android.content.res.Configuration) { vehicle?.onConfigurationChanged(cfg) }

    /** The current watcher's gear-only Park answer; null = not parked (also with no watcher). */
    internal fun vehicleParked(): Boolean? = vehicle?.parked()

    // ---- the OCBM probe -----------------------------------------------------------------------------

    /**
     * Get the process-scoped probe, creating it if there is none (or only a stopped one). Under
     * [probeLock] together with every null-out (F027): an unsynchronised get-or-create could restore
     * a probe a stop had just discarded. Observers are bound ONCE, at creation, to this core.
     */
    fun ocbm(): OcbmProbe = synchronized(probeLock) {
        SessionHolder.ocbmProbe?.takeUnless { it.stopped } ?: OcbmProbe(app).also {
            // The box narrates the phone's side of the session over CH_CTRL. All observers live on
            // the probe, not the client, because the client is rebuilt on every re-claim.
            it.onSessionEvent = { sev -> onBoxSessionEvent(sev) }
            it.onPairingCode = { code -> onBoxPairingCode(code) }
            it.onBtPhase = { p -> onBoxBtPhase(p) }
            // Replay flag read inline on ocbm-read: the flag for the frame being delivered.
            it.onBoxHealth = { f -> supervisor.onBoxHealth(f, SessionHolder.ocbmProbe?.client?.lastBoxHealthReplay ?: false) }
            it.onPhoneIdent = { j -> onBoxPhoneIdent(j) }
            it.onProjMode = { m -> onBoxProjMode(m) }
            it.onMgmtInfo = { c, j -> onBoxMgmtInfo(it, c, j, linkUp = true) }
            // Transport death (ENODEV, or the heartbeat declaring the link dead — v5.6 H1).
            it.onLinkDead = { dead, why -> onBoxTransportDead(dead, why) }
            it.appInForeground = { AppForeground.any() }
            SessionHolder.ocbmProbe = it
        }
    }

    /** Null the holder only if it still holds [p] (F027: never discard a probe built since). */
    private fun dropProbe(p: OcbmProbe) = synchronized(probeLock) {
        if (SessionHolder.ocbmProbe === p) SessionHolder.ocbmProbe = null
    }

    /**
     * Push the hotspot credentials into the OCBM probe, so `CT_SUBSCRIBE` carries the VEHICLE's
     * credentials for the `0x5703` handoff. **Every path that starts the OCBM link must call this
     * first** — the box applies credentials only on the host_present 0->1 edge (device-observed
     * 2026-08-12). Runs on the command executor (F027), never on main. [creds] defaults to the
     * attached view's values (main-thread writes, read here after the executor hand-off), falling
     * back to the last values a view held.
     */
    fun applyHotspotFields(creds: Creds? = null) {
        val c = creds ?: currentCreds()
        ocbm().apply {
            if (c != null) {
                wifiSsid = c.ssid
                wifiPass = c.pass
            }
        }
    }

    /**
     * The attached view's credentials (refreshing [lastCreds]), else the last a view (or the
     * `--es ssid/pass` path) held, else the SAVED ones (plan v5.9 D6 R10: a process that never built a
     * MainActivity — an auto-claim — reads HotspotPrefs), else null. See AutoClaimPolicy.resolveCreds.
     */
    private fun currentCreds(): Creds? {
        val view = slot.current()?.let { Creds(it.ssid, it.pass) }?.also { lastCreds = it }
        val saved = { val ss = HotspotPrefs.ssid(app); val pp = HotspotPrefs.pass(app)
            if (ss == null && pp == null) null else Creds(ss ?: "", pp ?: "") }
        return AutoClaimPolicy.resolveCreds(view, lastCreds, if (view == null && lastCreds == null) saved() else null)
    }

    /**
     * OD-9A: the box is subscribed now, so credentials saved now reach the iPhone only on the next
     * CT_SUBSCRIBE (the box writes /tmp/carplay_cfg.yaml only then; plan v5.9 D005 step 5).
     */
    fun credentialsNeedResubscribe(): Boolean = ocbmProbe?.client?.subscribed == true

    /** OD-7A: true when no usable hotspot SSID + passphrase is available to hand to the box. */
    private fun credsMissing(): Boolean = currentCreds()?.missing ?: true

    /**
     * OD-7A: refuse a start ([origin] = "start" or "USB attach") for missing credentials. Latches it so
     * the dialog's Save runs it, says so on screen, and opens the dialog with the reason. Command
     * executor only (autoStart, onAdapterAttached); the dialog itself is marshalled to main.
     */
    private fun blockStartForCreds(origin: String) {
        credsLatch.block()
        setStatus(LinkState.IDLE, CREDS_PROMPT)
        emit("not starting ($origin): no hotspot credentials saved — set the hotspot credentials (dialog opened)")
        slot.current()?.promptForCredentials(CREDS_WHY)
    }

    /**
     * The credentials dialog was saved (command executor, after [applyHotspotFields]). If the last
     * autoStart was refused for missing credentials and they are now present, run that start.
     */
    fun onCredentialsSaved(c: Creds) {
        if (credsLatch.resumeOnSave(c.missing)) {
            emit("hotspot credentials saved — running the start that was waiting for them")
            autoStart(manual = false)
        }
    }

    /** The adapter's USB transport died with [dead] as its client (2026-10-06, 52B). Runs on `ocbm-read`. */
    private fun onBoxTransportDead(dead: zeno.gmccpa.ocbm.OcbmClient, heartbeatWhy: String? = null) {
        // CAS first, then arm: the heartbeat waits (<= 2 s) on this latch before it would synthesise
        // a HOST_GONE of its own, whose transportDied = false would say "press Start".
        if (dead.hostGoneDelivered.compareAndSet(false, true)) onBoxSessionEvent(Ocbm.SEV_HOST_GONE, transportDied = true)
        // Latch already set by the heartbeat's own fallback HOST_GONE: re-report the loss as relinking.
        else if (heartbeatWhy != null) supervisor.onBoxLost(relinking = true)
        // The adapter is gone (unplug = ENODEV, or a dead link): its name must not stay on screen.
        // AFTER the CAS above, so a MGMT reply still in flight reads this client as dead (BoxNameGate).
        clearBoxBtName("adapter link lost")
        BoxRelink.arm(if (heartbeatWhy != null) "heartbeat declared the link dead" else "transport died", heartbeatWhy)
        refreshNotice()   // "Reconnecting to the adapter" while the relink owns the claim
    }

    /** `CT_SESSION_EVENT` from the adapter — where the *phone* is on the box's own USB bus. */
    private fun onBoxSessionEvent(sev: Byte, transportDied: Boolean = false) {
        SessionSummary.current()?.onSessionEvent(sev)
        when (sev) {
            Ocbm.SEV_PHONE_PRESENT -> setDetail("adapter reports a device on its USB bus")
            Ocbm.SEV_PHONE_ABSENT -> {
                setPairingCode("")
                setDetail("adapter reports its USB bus is idle")
            }
            Ocbm.SEV_HOST_PRESENT -> supervisor.onBoxSubscribed()
            Ocbm.SEV_HOST_GONE -> {
                // The box dropped us: the adapter name shown may no longer be true. Cleared now; if the
                // link is still up (a box-side watchdog drop, same box) the re-read below restores it.
                clearBoxBtName(if (transportDied) "adapter link lost" else "adapter reported HOST_GONE")
                // MUST be off this thread: mgmtGetInfo() blocks polling a queue that only `ocbm-read`
                // fills. Snapshot the bonded list on the way out (the start-vs-end diff).
                val sess = SessionSummary.current()
                if (sess != null || !transportDied) runAsync {
                    // Captured BEFORE the read, so a reply that lands after this link died, was stopped
                    // or was replaced is dropped instead of repainting the name (BoxNameGate).
                    val p = SessionHolder.ocbmProbe
                    val c = p?.client
                    val info = runCatching { c?.mgmtGetInfo(1500) }.getOrNull()
                    if (sess != null) {
                        info?.let { sess.onMgmtInfoEnd(it) }
                        SessionSummary.end(sess, "host_gone")
                    }
                    if (!transportDied) info?.let { onBoxMgmtInfo(p, c, it, linkUp = false) }
                }
                // NOT a CarPlay session end: the supervisor holds a streaming session.
                supervisor.onBoxLost(relinking = transportDied)
            }
            else -> Unit
        }
    }

    /** `CT_PAIRING_CODE` — the code the driver has to match against the prompt on the iPhone. */
    private fun onBoxPairingCode(code: String) {
        setPairingCode(code)
        if (code.isNotEmpty()) setStatus(LinkState.PAIRING, "match this code on the iPhone")
    }

    /** `CT_BT_PHASE` — advisory BT/iAP2 progress; never gates the LinkState machine on ordering. */
    private fun onBoxBtPhase(phase: Byte) {
        SessionSummary.current()?.onBtPhase(phase)
        setDetail("Bluetooth: ${Ocbm.btpName(phase)}")
        supervisor.onBtPhase(phase, SessionHolder.ocbmProbe?.client?.lastBtPhaseReplay ?: false)
    }

    /**
     * `CT_PHONE_IDENT` — who the connected phone is. The user-given device name is redacted before
     * the line is written (audit12 F119; U8's PhoneIdent.redactName): the export redactor does not
     * recognise it.
     */
    private fun onBoxPhoneIdent(json: String) {
        SessionSummary.current()?.onPhoneIdent(json)
        if (json.isNotEmpty()) emit("phone identity: ${PhoneIdent.redactName(json)}")
    }

    /** `CT_PROJ_MODE` — which projection transport currently owns the box. Advisory. */
    private fun onBoxProjMode(mode: Byte) {
        SessionSummary.current()?.onProjMode(mode)
        setDetail("projection: ${Ocbm.pmName(mode)}")
    }

    // ---- verbs ----------------------------------------------------------------------------------------

    /** Start button / launcher launch. */
    fun requestStart(manual: Boolean) {
        // D6 / OD-9D: the operator wants the adapter again — the Stop no longer suppresses the
        // auto-claim, and an explicit start re-opens a tripped flap guard.
        AutoClaimPrefs.clear(app)
        flapGuard.reset()
        runAsync { autoStart(manual) }
    }

    /**
     * The Stop button. Main thread. abortClaimWait first, on THIS thread: a Start with no adapter sits
     * in awaitClaimable for up to ten minutes ON the command executor.
     */
    fun requestStop() {
        // Raised BEFORE queueing; withdrawn if the queue refused it.
        SessionHolder.stopRequested = true
        // D6 / OD-9D: the Stop also suppresses the auto-claim for the rest of this head-unit boot.
        // Written here, on main, before the stop is queued; withdrawn with stopRequested below.
        emit("stop flag: boot identity = ${AutoClaimPrefs.markStopped(app)}")
        // OD-7A: Stop withdraws a start that was waiting for credentials, so a later Save does not
        // start what the operator stopped. Cleared again in stopEverythingLocked, after any refusal
        // that was already queued ahead of the stop.
        credsLatch.clear()
        BoxRelink.cancel("operator stop")
        ocbmProbe?.abortClaimWait()
        // Cleared here so an onResume while the stop waits its turn cannot relaunch the screen.
        sessionUp = false
        if (!runAsync { stopEverything() }) { SessionHolder.stopRequested = false; AutoClaimPrefs.clear(app) }
    }

    /**
     * The whole product flow: adapter present -> claim + init the box -> the box does BT/iAP2 and the
     * handoff -> the phone dials in -> `onSessionUp` stands the A/V consumers up -> CarPlay.
     *
     * Idempotent. Returns what the bring-up achieved, or null when a guard found nothing to do.
     */
    fun autoStart(manual: Boolean): OcbmProbe.LinkResult? {
        // Never tear down a live CarPlay session to "start" one (runAll begins with stopLocked()).
        if (cpRx?.sessionLive == true) {
            setStatus(LinkState.LIVE, "CarPlay session is live")
            if (manual) emit("a CarPlay session is live — Start would tear it down; refusing")
            return null
        }
        if (ocbmProbe?.client?.subscribed == true) {
            setStatus(LinkState.WAITING, "link already up — waiting for the phone")
            if (manual) emit("already subscribed; nothing to do")
            return null
        }
        // The box relink already owns the claim (2026-10-06).
        if (BoxRelink.owner.get() != null || ocbmProbe?.claimWaitActive == true) {
            ocbmProbe?.rerequestNow()
            val st = readAdapterState()
            setStatus(LinkState.CLAIMING, st.text)
            if (manual) emit("a re-claim of the adapter is already in progress (${st.name}) — not queuing another")
            return null
        }
        // A Stop pressed while this Start was still queued: consumed by stopEverything, which runs next.
        if (SessionHolder.stopRequested) {
            emit("Stop was pressed before this Start ran — not starting; the stop runs next")
            return null
        }
        // OD-7A (owner, 2026-10-07): there are no compiled-in credentials. With nothing saved (and no
        // --es override) the box would be handed nothing to send the phone, so do not start: say so and
        // open the dialog. Its Save runs this start (onCredentialsSaved). The probe refuses a
        // credential-less CT_SUBSCRIBE on its own too (runAllLocked); this stops the bring-up earlier.
        if (credsMissing()) { blockStartForCreds("start"); return null }
        credsLatch.clear()
        setStatus(LinkState.SEARCHING, "looking for the adapter…")
        applyHotspotFields()
        // The probe exists before the receiver and before runAll. The receiver's MFi relay no longer
        // depends on this ordering (it resolves the current probe per call, F025); the bring-up does.
        ocbm()
        if (cpRx == null) startCarPlayRx()      // advertise before the box even wakes its radios
        setStatus(LinkState.CLAIMING, "claiming the adapter…")
        try {
            // GATE ON THE RESULT: runAll() reports failure by RETURNING (device-observed 2026-08-28).
            // B-F2: a Stop pressed during the bring-up must not take the CT_SUBSCRIBE radio-wake
            // edge for a session the queued stop then tears down ([StartPolicy.subscribeAllowed]).
            val r = ocbm().runAll(subscribe = { StartPolicy.subscribeAllowed(SessionHolder.stopRequested) })
            if (!r.helloOk) {
                setStatus(LinkState.FAILED, r.failureDetail(), FailReason.LINK_FAILED)
                emit("link NOT established: ${r.failureDetail()}")
                releaseLinkIfUnlinked("start: no link")
                return r
            }
            cpRx?.wakeDiscovery("OCBM link up — CT_HELLO_ACK received")
            // The supervisor writes the state word from here on.
            supervisor.onBoxLinked(r.mfiProven)
            rekeyAutoForgetIfDue(r.mfiProven)   // V5/OD-8B: queued behind this start
            flapGuard.reset()
            // D6 R5: the LINK hold, AFTER the bring-up tail (a refusal cannot skip it); wrapped inside.
            AutoClaim.acquireLink(app, "start: link up")
            return r
        } catch (t: Throwable) {
            setStatus(LinkState.FAILED, t.message ?: t.javaClass.simpleName, FailReason.EXCEPTION)
            releaseLinkIfUnlinked("start: exception")
            throw t
        }
    }

    private fun stopEverything() = try { stopEverythingLocked() } finally { SessionHolder.stopRequested = false }

    private fun stopEverythingLocked() {
        credsLatch.clear()   // OD-7A: see requestStop
        clearBoxBtName("operator stop")
        sessionUp = false   // else onResume would relaunch the CarPlay screen after an explicit Stop
        // FIRST, before anything is torn down (see SessionTrace.cancelAll).
        SessionTrace.cancelAll("operator stop (stopEverything)")
        val rx = cpRx
        rx?.stop(); cpRx = null
        // Re-clear after stop(): a session-up that passed its identity check before cpRx was nulled.
        sessionUp = false
        // stop() fences the receiver's session-down callback, so the consumers come down here.
        if (rx != null) tearDownSessionConsumers("operator stop")
        retireVehicle("receiver stopped (operator stop)")
        // Stamped AFTER stop() for Restart Session's settle. Quick on a dead link (B-F2): no box
        // logs can be pulled over a link that is gone, and the full pull is 4 x 6 s of timeouts.
        ocbmProbe?.let { it.stop(quick = StartPolicy.probeStopQuick(it.hasLink())); emit(it.stats()); SessionHolder.lastProbeStopAt = android.os.SystemClock.elapsedRealtime(); dropProbe(it) }
        // Again, now that the probe is stopped and dropped: a relink's link-up snapshot runs on the
        // probe's own thread, not this one, and could have painted the name after the first clear.
        // Any reply after this point reads its probe as stopped/replaced and is dropped (BoxNameGate).
        clearBoxBtName("operator stop (probe stopped)")
        // BEFORE onStopped(), whose posted `to(IDLE)` re-registers `phase: idle` on a clean board.
        SessionTrace.Board.reset("operator stop (stopEverything)")
        supervisor.onStopped()
        // D6: the adapter is released, so is its hold (and with it the notification, unless a
        // CarPlayActivity session token still holds the service).
        AutoClaim.releaseLink(app, "operator stop")
        setStatus(LinkState.STOPPED, "stopped by the operator — press Start to reconnect")
    }

    // ---- Restart Session ----------------------------------------------------------------------------

    /**
     * Minimum gap between the box losing us and our next CT_SUBSCRIBE. Grounded in the box
     * supervisor's own 4 s settle for its detached wireless_down (tools/session_supervisor.sh). Do not
     * shorten.
     */
    private val RESTART_SETTLE_MS = 5_000L

    /**
     * True from the moment Restart Session is pressed until [restartSession] has finished. Taken on the
     * UI thread AT THE PRESS, so a repeat press is rejected at once. Also what [linkStateFor] reads.
     */
    private val restartInProgress = AtomicBoolean(false)

    /** The Restart Session button. UI thread. Safe in any state, any number of times. */
    fun requestRestart() {
        if (!restartInProgress.compareAndSet(false, true)) {
            showResult("Restart already in progress…", "restart already in progress — ignoring the repeat press")
            return
        }
        setRestartBusy(true)
        // D6 / OD-9D (R9: was Recover-as-Start): Restart wants the adapter — clear a standing Stop.
        AutoClaimPrefs.clear(app)
        flapGuard.reset()
        // A Start parked in awaitClaimable holds the command executor; only this ends the wait early.
        ocbmProbe?.abortClaimWait()
        sessionUp = false
        if (!runAsync { restartSession() }) { restartInProgress.set(false); setRestartBusy(false) }
    }

    /**
     * Drop the phone, release the box, wait, re-claim. Never a second disconnect after the re-claim,
     * never touches the pairing, never retries on its own. The supervisor is told first; the probe is
     * dropped and rebuilt so its HELLO carries a NEW nonce.
     */
    private fun restartSession() {
        try {
            emit(""); emit("==================== DRIVER-REQUESTED SESSION RESTART ====================")
            setStatus(LinkState.RESTARTING, "closing the session…")
            sessionUp = false
            val rx = beginRetire()
            try {
                // Not onStopped(): the merged Restart Session (plan v5.9 UI-1) also clears the ladder's
                // cooldown stamps — the one thing the old Recover button did that this lacked.
                supervisor.onOperatorRestart()
                SessionTrace.cancelAll("operator restart (restartSession)")
                if (rx != null) {
                    if (!rx.stop()) emit("restart: a receiver pump did not unwind within 1500 ms (parked in NativeCore.feed) — proceeding; the fresh receiver replaces it")
                    cpRx = null
                    sessionUp = false
                    tearDownSessionConsumers("driver restarted the session")
                    retireVehicle("receiver stopped (restart)")
                }
            } finally { endRetire() }
            val old = ocbmProbe
            var usbStuck = false
            // Deliberate teardown, like Stop: a relink of the probe being replaced must not re-arm.
            BoxRelink.cancel("operator restart")
            if (old != null) {
                old.stop(quick = true)
                // Stamped AFTER stop(): the settle is measured from the box's GONE edge, the LAST thing
                // OcbmClient.stop() writes.
                SessionHolder.lastProbeStopAt = android.os.SystemClock.elapsedRealtime()
                emit(old.stats())
                usbStuck = old.usbReleaseDeferred
                dropProbe(old)
            }
            SessionTrace.Board.reset("operator restart (restartSession)")
            val since = SessionHolder.lastProbeStopAt
            if (since != 0L) {
                var left = RESTART_SETTLE_MS - (android.os.SystemClock.elapsedRealtime() - since)
                while (left > 0) {
                    setDetail("adapter released — re-claiming in ${(left + 999) / 1000} s")
                    Thread.sleep(minOf(left, 1_000L))
                    left = RESTART_SETTLE_MS - (android.os.SystemClock.elapsedRealtime() - since)
                }
            }
            if (usbStuck) emit("restart: the old link's USB read thread is still parked in bulkTransfer — this process still holds the interface, so the re-claim below is expected to be refused")
            // awaitClaimable says nothing for up to ten minutes. Tell the driver at 30 s what it is
            // waiting on; a main-looper timer because the command thread is inside autoStart.
            main.postDelayed({
                val p = ocbmProbe
                // C6 "P": these replace the driver line until the state changes.
                if (restartInProgress.get() && p != null && p.client == null) {
                    if (p.claimWaitState == "present-no-permission") setStateHint("Accept the USB permission prompt for the adapter.",
                        "adapter is on the bus but USB permission is not held — accept the dialog (still waiting)")
                    else setStateHint("No adapter found yet. Check the cable; still waiting.",
                        "no adapter on the USB bus after 30 s — still waiting (up to 10 min); check the cable")
                }
            }, 30_000L)
            if (SessionHolder.stopRequested) {
                emit("restart: Stop was pressed during the settle — not re-claiming; the stop runs next")
                return
            }
            val r = autoStart(manual = true)
            when {
                r == null && credsLatch.isBlocked -> emit("restart: no hotspot credentials — not re-claiming until they are set")
                r == null -> emit("restart: a session came up on its own during the settle — leaving it")
                !r.claimed && usbStuck -> setStatus(LinkState.FAILED,
                    "adapter re-claim refused: this app still holds the USB interface (the old link's read is stuck in the kernel) — no adapter command can fix that; unplug and replug the adapter, or restart the app",
                    FailReason.USB_STUCK)
                !r.claimed -> Unit
                !r.helloOk -> setStatus(LinkState.FAILED,
                    "adapter claimed but ocbmd did not answer HELLO in 20 s — the box's own supervisor restarts a wedged ocbmd within ~2 min; press Restart Session again after that",
                    FailReason.NO_HELLO)
                !r.subscribed -> Unit
                else -> {
                    supervisor.onRestartSubscribed()
                    setDetail("adapter up — waiting for the iPhone to reconnect Bluetooth")
                }
            }
            releaseLinkIfUnlinked("restart: no link at the end")
        } finally {
            restartInProgress.set(false)
            setRestartBusy(false)
        }
    }

    // ---- box actions ----------------------------------------------------------------------------------

    /**
     * A confirmed (or non-destructive) box action from the launcher. Forget stops the receiver behind
     * a BT forget that can take a while, so sessionUp is cleared at the press, as carplay_stop does.
     */
    fun requestBoxAction(a: BoxAction) {
        if (a == BoxAction.FORGET_PHONE) { requestForget(null); return }
        runAsync { boxAction(a) }
    }

    /**
     * Forget pairing, BOTH halves (plan v5.9 D020): the sheet's button ([mac] null) and the
     * `ocbm_forget` verb (optional `--es mac`) take this one path — same executor, same order, same
     * drain fence. sessionUp is cleared at the press (main thread) when a box client exists, as
     * carplay_stop does, because the forget stops the receiver behind a BT forget that can take a while.
     */
    fun requestForget(mac: String?) {
        if (ocbmProbe?.client != null) sessionUp = false
        runAsync { forgetFromOperator(mac) }
    }

    /** Command executor. The no-link refusal is the box actions' own (no state change). */
    private fun forgetFromOperator(mac: String?) {
        if (ocbmProbe?.client == null) { refuseNoAdapter(); return }
        forgetPairing(auto = false, mac = mac)
        // The forget told the supervisor "stopped" while the link it saw is still up: hand it
        // the last link report again (BOX_LINKED only moves forward; review fix1-3).
        if (ocbmProbe?.client?.subscribed == true) supervisor.onBoxLinked(lastMfiProven)
    }

    /**
     * A box action or forget with no adapter link (plan v5.9 R12): a result, NOT a failure — the state
     * word is left as it is (nothing is running, so FAILED + "Restart session" was wrong advice).
     */
    private fun refuseNoAdapter() = showResult(NO_ADAPTER)

    /** The confirmed `CH_MGMT` actions from the "Adapter & logs" sheet (MGMT_GET_INFO stays automatic). */
    private fun boxAction(a: BoxAction) {
        val p = ocbmProbe
        val c = p?.client
        if (c == null) { refuseNoAdapter(); return }
        when (a) {
            // The ACK means "request accepted", NOT "Bluetooth is back" (plan v5.9 D005).
            BoxAction.RESTART_BT -> {
                showResult("Asking the adapter to restart its Bluetooth…")
                supervisor.onManualWirelessRestart()
                val st = c.mgmtAction(Ocbm.MGMT_RESTART_WIRELESS)
                // No "~5 s / ~12 s" figures and no "credentials re-applied" (D005: untraced / false —
                // the box re-applies the LAST subscribe's credentials; only Restart session sends new
                // ones). The one traced number is the box supervisor's 4 s re-bring-up delay.
                if (st == 0) showResult("Adapter Bluetooth restarting",
                    "MGMT_RESTART_WIRELESS accepted — the adapter restarts its Bluetooth (box supervisor: down, then up ~4 s later)")
                else reportMgmt("Restart adapter Bluetooth", st)
            }
            BoxAction.REBOOT -> {
                showResult("Rebooting the adapter…")
                reportMgmt("Reboot adapter", c.mgmtAction(Ocbm.MGMT_REBOOT))
            }
            // BOTH halves, together: the box's BR/EDR bond and this app's Ed25519 peer store (body in
            // [forgetPairing], shared with V5's one automatic run).
            BoxAction.FORGET_PHONE -> forgetFromOperator(null)
        }
    }

    /** MGMT_ACK status: 0 ok, non-zero error, null no reply at all. */
    private fun reportMgmt(what: String, status: Int?) = when (status) {
        0 -> showResult("$what: the adapter accepted it")
        null -> showResult("$what: the adapter did not answer")
        else -> showResult("$what: the adapter refused it", "$what: adapter returned error $status")
    }

    /** Second chance for a pump still inside `NativeCore.feed` when [CarPlayRx.stop] gave up (Forget only). */
    private fun waitForPeerStoreQuiet(): Boolean {
        emit("forget: a pump is still in the native core — waiting for it to unwind before deleting")
        val deadline = android.os.SystemClock.elapsedRealtime() + 17_000L
        while (android.os.SystemClock.elapsedRealtime() < deadline) {
            if ((cpRx?.liveConnectionCount() ?: 0) == 0) return true
            try { Thread.sleep(250) } catch (_: InterruptedException) { Thread.currentThread().interrupt(); return false }
        }
        return false
    }

    // ---- scripted verbs (MainActivity.handleRunExtra dispatches; bodies live here) ----------------------

    /** The adapter was attached (MainActivity.handleAttachIntent validated the device). */
    fun onAdapterAttached() {
        runAsync {
            // plan v5.9 D6: the same gate as the auto-claim, with this route's foreground semantics
            // (dialog allowed, ten-minute wait — so no adapter/permission/flap rows). A Stop of this
            // boot, a restart, a relink/claim wait or a link already up end it here.
            val d = AutoClaimPolicy.decide(autoClaimInputs(), foreground = true)
            if (d != AutoClaimPolicy.Decision.CLAIM && d != AutoClaimPolicy.Decision.RELINK_LIVE &&
                d != AutoClaimPolicy.Decision.BLOCK_CREDS) {
                emit("USB attach (activity route): ${d.name} — ${d.why}")
                return@runAsync
            }
            // OD-7A: the attach path gets the same credential gate as autoStart, before anything is
            // claimed. Without it a never-saved install plugged in the box, ran the link up to the
            // probe's credential-less CT_SUBSCRIBE refusal (runAllLocked) and showed nothing but a
            // developer-worded log line. The link is NOT brought up here: an unsubscribed link would
            // later be torn down and re-claimed by the resumed start (runAll begins with stopLocked),
            // so the Save simply runs the whole start on the adapter that is already present.
            // A live session needs no credentials (its re-attach never subscribes), so it is exempt.
            if (cpRx?.sessionLive != true && credsMissing()) { blockStartForCreds("USB attach"); return@runAsync }
            // Past the gate: a start that was waiting for credentials is now running (as in autoStart).
            credsLatch.clear()
            // Carry over whatever hotspot credentials are on screen — on this thread (F027).
            applyHotspotFields()
            // The receiver must be listening on :7011 and advertised BEFORE the box wakes its radios.
            ocbm()
            if (cpRx == null) startCarPlayRx()
            // A re-attach while CarPlay is streaming restores the link WITHOUT CT_SUBSCRIBE.
            if (cpRx?.sessionLive == true) {
                emit("USB re-attach with a live CarPlay session — restoring the link WITHOUT " +
                     "CT_SUBSCRIBE (a fresh subscribe would re-wake the box radios mid-session)")
                val r = ocbm().runAll(subscribe = false)
                if (r.helloOk) cpRx?.wakeDiscovery("OCBM link restored on USB re-attach")
                if (r.helloOk) { supervisor.onBoxRelinked(); AutoClaim.acquireLink(app, "attach (activity route): link restored") }
                else emit("USB re-attach: link NOT restored — ${r.failureDetail()}")
            } else {
                val r = ocbm().runAll()
                if (r.helloOk) {
                    cpRx?.wakeDiscovery("OCBM link up on USB attach — CT_HELLO_ACK received")
                    supervisor.onBoxLinked(r.mfiProven)
                    rekeyAutoForgetIfDue(r.mfiProven)   // V5/OD-8B
                    flapGuard.reset()
                    AutoClaim.acquireLink(app, "attach (activity route): link up")   // D6: after the bring-up tail
                }
                else emit("USB attach: link NOT established — ${r.failureDetail()}")
            }
        }
    }

    // ---- plan v5.9 D6: the auto-claim (AutoClaim.trigger -> here, on netprobe-cmd) -------------------

    /** Process-scoped: background CLAIMs that keep failing HELLO stop until a Start or a good link. */
    private val flapGuard = AutoClaimPolicy.FlapGuard()

    /** The claim wait of a background claim: measured from the claim task's start, not the trigger. */
    private val AUTO_CLAIM_WAIT_MS = 20_000L

    /** The policy's inputs, read now (netprobe-cmd). */
    private fun autoClaimInputs(): AutoClaimPolicy.Inputs {
        val p = ocbmProbe
        return AutoClaimPolicy.Inputs(
            suppressed = AutoClaimPrefs.suppressed(app),
            stopRequested = SessionHolder.stopRequested,
            restartInProgress = restartInProgress.get(),
            adapter = adapterKind(readAdapterState()),
            relinkOwner = BoxRelink.owner.get() != null,
            claimWaitActive = p?.claimWaitActive == true,
            hasLink = p?.hasLink() == true,
            sessionLive = cpRx?.sessionLive == true,
            credsMissing = credsMissing(),
            flapTripped = flapGuard.tripped,
        )
    }

    /** Main thread (AutoClaim.trigger step 6). A refused queue gives back a hold it just took. */
    fun requestAutoClaim(origin: AutoClaim.Origin, acquired: Boolean) {
        if (!runAsync { autoClaim(origin, acquired) } && acquired) AutoClaim.releaseLink(app, "auto-claim (${origin.tag}): command refused")
    }

    /**
     * The D6 decision and its action, on netprobe-cmd. Every decision is logged as
     * `auto-claim (<origin>): <DECISION> — <why>`. Whatever the decision, the LINK hold THIS trigger
     * added is released at the end when there is no link and no relink owns the claim (rev c C1).
     */
    private fun autoClaim(origin: AutoClaim.Origin, acquired: Boolean) {
        val tag = "auto-claim (${origin.tag})"
        try {
            val d = AutoClaimPolicy.decide(autoClaimInputs())
            emit("$tag: ${d.name} — ${d.why}")
            when (d) {
                AutoClaimPolicy.Decision.CLAIM -> claimInBackground(origin, tag)
                AutoClaimPolicy.Decision.RELINK_LIVE -> {
                    // As the attach route under a live session: restore the link WITHOUT CT_SUBSCRIBE.
                    applyHotspotFields()
                    ocbm()
                    if (cpRx == null) startCarPlayRx()
                    val r = ocbm().runAll(subscribe = { false }, mayRequest = { false },
                        deferredWhy = { "auto-claim: no dialog from the background" },
                        cancelled = { SessionHolder.stopRequested || AutoClaimPrefs.suppressed(app) })
                    if (r.helloOk) { cpRx?.wakeDiscovery("OCBM link restored by $tag"); supervisor.onBoxRelinked(); flapGuard.reset() }
                    else emit("$tag: link NOT restored — ${r.failureDetail()}")
                }
                // Latch + status; the dialog opens on the next MainActivity attach (credsLatch).
                AutoClaimPolicy.Decision.BLOCK_CREDS -> blockStartForCreds(tag)
                else -> Unit
            }
        } finally {
            val link = ocbmProbe?.hasLink() == true
            val relink = BoxRelink.owner.get() != null
            if (AutoClaimPolicy.shouldRelease(acquired, link, relink)) AutoClaim.releaseLink(app, "$tag: no link at the task's end")
            else if (acquired && relink && !link) emit("$tag: link hold kept — a relink owns the claim")
        }
    }

    /** D6 CLAIM (rev b R8): no dialog, a Stop-aware wait with a 20 s deadline, no subscribe after a Stop. */
    private fun claimInBackground(origin: AutoClaim.Origin, tag: String) {
        credsLatch.clear()
        applyHotspotFields()
        ocbm()
        if (cpRx == null) startCarPlayRx()      // advertise before the box wakes its radios
        setStatus(LinkState.CLAIMING, "$tag: claiming the adapter…")
        val claimStart = android.os.SystemClock.elapsedRealtime()
        val deadlineLogged = AtomicBoolean(false)
        val deadlineHit = {
            val hit = android.os.SystemClock.elapsedRealtime() - claimStart > AUTO_CLAIM_WAIT_MS
            if (hit && deadlineLogged.compareAndSet(false, true))
                emit("$tag: claim wait deadline ${AUTO_CLAIM_WAIT_MS / 1000} s — not a Stop")
            hit
        }
        val r = zeno.gmccpa.logging.SessionSummary.withLaunchOrigin(origin.summary) {
            ocbm().runAll(
                subscribe = { cpRx?.sessionLive != true && !SessionHolder.stopRequested && !AutoClaimPrefs.suppressed(app) },
                mayRequest = { false },
                deferredWhy = { "auto-claim: no dialog from the background" },
                cancelled = { SessionHolder.stopRequested || AutoClaimPrefs.suppressed(app) || deadlineHit() },
            )
        }
        flapGuard.onClaimEnd(r.claimed, r.helloOk, ocbmProbe?.adapterEnumerated() == true)
        if (!r.helloOk) {
            // A Stop or a Restart queued behind this task owns the state word (a Restart's press
            // aborted this wait on purpose).
            if (SessionHolder.stopRequested || restartInProgress.get()) return
            setStatus(LinkState.FAILED, "$tag: ${r.failureDetail()}", FailReason.LINK_FAILED)
            if (flapGuard.tripped) emit("$tag: ${flapGuard.streak} background claims in a row ended without HELLO — flap guard: no more until Start or a good link")
            return
        }
        cpRx?.wakeDiscovery("OCBM link up ($tag) — CT_HELLO_ACK received")
        supervisor.onBoxLinked(r.mfiProven)
        rekeyAutoForgetIfDue(r.mfiProven)
        flapGuard.reset()
        // After the bring-up tail: a no-op when the trigger's hold stands; re-places it if a failed
        // foreground start released it while this task waited in the queue.
        AutoClaim.acquireLink(app, "$tag: link up")
    }

    /** The LINK hold goes when nothing is linked and no relink owns the claim (FAILED never shows). */
    private fun releaseLinkIfUnlinked(why: String) {
        if (ocbmProbe?.hasLink() != true && BoxRelink.owner.get() == null) AutoClaim.releaseLink(app, why)
    }

    fun verbOcbmSelftest() { runAsync { ocbm().selfTest() } }

    /** Same policy as autoStart: runAll() would CT_STOP and re-SUBSCRIBE under a live session. */
    fun verbOcbmLink() {
        runAsync {
            if (cpRx?.sessionLive == true) emit("ocbm_link refused: a CarPlay session is live — runAll() would CT_STOP and re-SUBSCRIBE under it")
            else ocbm().runAll()
        }
    }

    fun verbCarplayRx() { runAsync { ocbm(); if (cpRx == null) startCarPlayRx() } }

    /** Both planes at once: the OCBM/BT link and the Wi-Fi endpoint. */
    fun verbFull() {
        runAsync {
            ocbm()
            if (cpRx == null) startCarPlayRx()
            if (cpRx?.sessionLive == true) emit("full refused: a CarPlay session is live — runAll() would CT_STOP and re-SUBSCRIBE under it")
            else ocbm().runAll()
        }
    }

    /** Screen only, and only over a live session: never declares a session to the supervisor. */
    fun verbCarplayUi() {
        runAsync {
            if (cpRx?.sessionLive == true) showCarPlayScreen()
            else emit("carplay_ui refused: no live CarPlay session on the current receiver")
        }
    }

    /**
     * The receiver half of restartSession. sessionUp is cleared on the calling (main) thread before
     * queueing: `am start` delivers onNewIntent and then onResume on the same thread.
     */
    fun verbCarplayStop() {
        sessionUp = false
        noteIfQueuedBehindClaim("carplay_stop")
        runAsync {
            sessionUp = false
            val rx = beginRetire()
            try {
                supervisor.onStopped()
                rx?.stop(); cpRx = null
            } finally { endRetire() }
            sessionUp = false
            if (rx != null) tearDownSessionConsumers("carplay_stop verb")
            retireVehicle("receiver stopped (carplay_stop)")
        }
    }

    fun verbOcbmState() { runAsync { ocbmProbe?.sessionState() ?: emit("no OCBM link") } }
    fun verbOcbmDisconnect() { runAsync { ocbmProbe?.disconnectPhone() ?: emit("no OCBM link") } }
    /** D020: the same both-halves forget as the sheet's button (it used to clear the box bond only). */
    fun verbOcbmForget(mac: String?) = requestForget(mac)

    fun verbOcbmStop() {
        runAsync {
            ocbmProbe?.let { it.stop(); emit(it.stats()); SessionHolder.lastProbeStopAt = android.os.SystemClock.elapsedRealtime(); dropProbe(it) }
            // No probe, no link: the adapter name goes with it (after the stop, see BoxNameGate).
            clearBoxBtName("ocbm_stop")
        }
    }

    // ---- capture control -------------------------------------------------------------------------

    /**
     * Persist the scope and restart capture so it takes effect now (F028: off the main thread —
     * LogCapture.stop joins the pump for up to 5 s). The scope is persisted on the CALLER's thread so
     * a second toggle reads the first one's value; the restart runs on [captureExecutor], in order.
     */
    fun setCaptureScope(scope: LogCapture.Scope) {
        CapturePrefs.setScope(app, scope)
        try {
            captureExecutor.execute {
                runCatching {
                    LogCapture.stop()
                    val ok = LogCapture.start(app, CapturePrefs.config(app))
                    // Print ONLY what is known synchronously; the pump resolves the effective scope.
                    emit("capture scope=$scope start=$ok (effective scope + read_logs resolve on the pump thread; see [logcap])")
                    showResult("Log capture: ${scope.name.lowercase().replace('_', ' ')}")
                }.onFailure { emit("capture scope change failed: ${it.javaClass.simpleName}: ${it.message}") }
            }
        } catch (_: RejectedExecutionException) { emit("capture scope change dropped") }
    }

    // ---- box relink seams (2026-10-06, 52B/52C) ---------------------------------------------------

    /** What the adapter is doing right now, for text that must be true (52C). Non-blocking. */
    internal fun readAdapterState(): AdapterState {
        val um = app.getSystemService(UsbManager::class.java) ?: return AdapterState.ABSENT
        val ours = runCatching { um.deviceList.values.filter { it.vendorId == zeno.gmccpa.ocbm.UsbBulkTransport.VID_CARLINKIT } }
            .getOrDefault(emptyList())
        if (ours.isEmpty()) return AdapterState.ABSENT
        val dev = ours.firstOrNull { it.productId == zeno.gmccpa.ocbm.UsbBulkTransport.PID_OCBM }
            ?: return AdapterState.wrongMode(ours.first().productId)
        val p = ocbmProbe
        if (p?.hasLink() == true) return if (p.client?.subscribed == true) AdapterState.SUBSCRIBED else AdapterState.LINKED_UNSUBSCRIBED
        val inFlight = p?.claimWaitActive == true || BoxRelink.owner.get() != null
        if (!runCatching { um.hasPermission(dev) }.getOrDefault(false)) return when {
            inFlight && p?.requestDeferred == true -> AdapterState.NO_PERMISSION_DEFERRED
            inFlight -> AdapterState.NO_PERMISSION_PROMPTED
            else -> AdapterState.NO_PERMISSION_IDLE
        }
        return if (inFlight) AdapterState.LINKING else AdapterState.NOT_LINKED
    }

    /**
     * [BoxRelink] stopped re-arming (RelinkChain.MAX_DEAD_RELINKS). Delivered to the core, so it is
     * heard with no MainActivity alive ([C2]). onRelinkEnded only rewrites IDLE.
     */
    internal fun onRelinkCapped(detail: String) {
        emit("relink: stopped — $detail")
        supervisor.onRelinkEnded(detail)
        AutoClaim.releaseLink(app, "relink capped")
        refreshNotice()
    }

    /** The box relink finished (52B). Called on `ocbm-relink`, to the core ([C2]). */
    internal fun onRelinkDone(r: OcbmProbe.LinkResult, expired: Boolean) {
        when {
            r.subscribed -> {
                cpRx?.wakeDiscovery("OCBM link re-claimed")
                supervisor.onBoxLinked(r.mfiProven)
                rekeyAutoForgetIfDue(r.mfiProven)   // V5/OD-8B (relink thread; the run is queued on netprobe-cmd)
            }
            r.helloOk -> {
                // Held without CT_SUBSCRIBE because CarPlay was live at the decision.
                cpRx?.wakeDiscovery("OCBM link re-claimed without CT_SUBSCRIBE")
                supervisor.onBoxRelinked()
            }
            else -> {
                emit("relink: link NOT restored — ${if (expired) "the adapter did not come back within ten minutes" else r.failureDetail()}")
                supervisor.onRelinkEnded(
                    if (expired) "the adapter did not come back within ten minutes — plug it in and press Start"
                    else "the adapter link was not restored (${r.failureDetail()}) — press Start")
            }
        }
        // D6 rev c C4: ONE wrapped acquire after the whole `when`, for BOTH success branches (the
        // subscribed onBoxLinked one and the held-without-subscribe onBoxRelinked one).
        if (r.helloOk) {
            flapGuard.reset()
            AutoClaim.acquireLink(app, "relink: link up")
        } else {
            // B-F3: the same rule as every other trigger end (AutoClaimPolicy.shouldRelease, C1): no
            // link and no relink owning the claim = release, whether or not the adapter is still
            // enumerated. The `adapterEnumerated() != true` guard this replaced kept the LINK hold and
            // its "Adapter connected" notification up indefinitely after the ten-minute wait expired
            // with the adapter on the bus but ungranted. Safe: RelinkChain.finish clears `owner`
            // before it reports, so the guard in releaseLinkIfUnlinked sees no owner here.
            releaseLinkIfUnlinked("relink: no link")
        }
        refreshNotice()
    }

    /** All probe output goes through ProbeLog, which owns the single NETPROBE logcat tag. */
    private fun emit(line: String) = ProbeLog.raw(line)

    companion object {
        /** D6: the policy's view of [readAdapterState] (AutoClaim's pre-check and the decision). */
        internal fun adapterKind(st: AdapterState): AutoClaimPolicy.Adapter = when {
            st === AdapterState.ABSENT -> AutoClaimPolicy.Adapter.ABSENT
            st.name.startsWith("WRONG_MODE") -> AutoClaimPolicy.Adapter.WRONG_MODE
            st === AdapterState.NO_PERMISSION_IDLE || st === AdapterState.NO_PERMISSION_PROMPTED ||
                st === AdapterState.NO_PERMISSION_DEFERRED -> AutoClaimPolicy.Adapter.NO_PERMISSION
            else -> AutoClaimPolicy.Adapter.PRESENT
        }

        /** OD-7A: the "Last message"/log sentence when no hotspot credentials are saved (C2 wording). */
        private const val CREDS_PROMPT = "Set the vehicle hotspot (button at left)"
        /** OD-7A: the message on a dialog that opened by itself, saying why it opened. */
        private const val CREDS_WHY = "No hotspot credentials are set. CarPlay cannot start until the vehicle hotspot's SSID and passphrase are saved here."
        /**
         * V5 / v5.9 UI-2 SEAM: how the rekey hint names the Forget Pairing control. Re-pointed by the
         * v5.9 UI unit (2026-10-08) after the "Adapter & logs" fold moved the button into that sheet.
         */
        private const val FORGET_PAIRING_CONTROL = "Adapter & logs ▸ Forget pairing"
        /** A box action / forget with no adapter link (plan v5.9 R12): transient, state word unchanged. */
        private const val NO_ADAPTER = "No adapter connected — press Start first"
        /** V5: the hint while the old pairing has NOT been cleared yet (before the first OCBM link, or after a partial clear). */
        private const val REKEY_HINT_PENDING = "Accessory key changed — press $FORGET_PAIRING_CONTROL, then forget this car on the iPhone, then pair again"
        /** V5: the hint after the one automatic (or hand-pressed) Forget Pairing cleared both halves. */
        private const val REKEY_HINT_FORGOTTEN = "Accessory key changed — the old pairing was cleared on the adapter and in this app; forget this car on the iPhone (Settings ▸ General ▸ CarPlay), then pair again"
    }
}
