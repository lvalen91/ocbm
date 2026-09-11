package zeno.gmccpa

import zeno.gmccpa.ocbm.Ocbm
import android.app.Activity
import android.content.Intent
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import android.os.Bundle
import zeno.gmccpa.logging.CapturePrefs
import zeno.gmccpa.logging.LogCapture
import zeno.gmccpa.logging.LogExport
import zeno.gmccpa.logging.SessionSummary
import zeno.gmccpa.logging.SessionTrace

/**
 * GM CCPA — the instrument shell for the wireless CarPlay receiver.
 *
 * Started life as a non-privileged capability prober named NetProbe (`com.carlink.netprobe`), renamed
 * 2026-08-14; the logcat tag stays `NETPROBE` because `native/carplay-jni/src/lib.rs` writes it from
 * Rust and `tools/tri_capture.sh` greps for it. The feasibility probes it was built around have
 * served their purpose and were retired 2026-08-12 (then docs/11 task T6.2, "Trim MainActivity feasibility probes"; the row was dropped from the ledger in the 2026-08-31 rewrite). What remains is the launcher UI, the `--es run`
 * command dispatcher, and the permanent diagnostic verbs: `mdns_self`, `ocbm_state`.
 *
 * Export goes through the Storage Access Framework (ACTION_CREATE_DOCUMENT) -> built-in file manager
 * -> USB, because the app's external files dir is NOT reachable by adb (`run-as` is blocked on this
 * user build, and so is `adb pull` of the app's private dir) or by other apps on this unit.
 */
class MainActivity : Activity() {

    /** The launcher screen. Owns the state readout and the hotspot credentials (see [Ui.kt]). */
    private lateinit var ui: LauncherUi

    /**
     * Vehicle-state levers (drive-restricted UI, day/night). Created eagerly so the Car API is bound
     * before the first session rather than on the first gear change — `SENSOR_RATE_ONCHANGE` would
     * otherwise leave us with no gear opinion until the driver happens to move the lever.
     */
    private val vehicle by lazy { zeno.gmccpa.av.VehicleStateWatcher(this) }

    /**
     * CarPlay-identity receiver probe (Car bit set + the _carplay-ctrl connect-out).
     *
     * Backed by [SessionHolder], NOT by an instance field — see that object for why. The accessor
     * keeps every existing call site unchanged while the object itself now outlives this Activity.
     */
    private var cpRx: CarPlayRx?
        get() = SessionHolder.cpRx
        set(v) { SessionHolder.cpRx = v }
    /** Set once the phone has a live session, so [onResume] can restore the CarPlay screen. */
    @Volatile private var sessionUp = false

    /**
     * Period of the `## STATUS` anchor while the receiver is up. Slower than SessionTrace's 30 s
     * default on purpose: the board has ~8 entries plus the phase, so a block is ~12 lines, and the
     * receiver waits for a phone for the whole drive — at 30 s that is ~1400 lines an hour on top of
     * the 641 the reference session produced in four minutes. One block a minute still bounds how
     * far a reader must scroll from any offset to learn what was up.
     */
    private val BOARD_TICK_MS = 60_000L

    /**
     * Owns the session phase and the recovery ladder. Everything that used to be inferred from which
     * objects were non-null is decided here instead; see [SessionSupervisor] for why that mattered.
     */
    private val supervisor: SessionSupervisor by lazy {
        SessionSupervisor(object : SessionSupervisor.Actions {
            override fun reannounce(): Boolean = cpRx?.reannounce() ?: false
            // Cheap and non-blocking: `subscribed` is the app's own view of the link, and the
            // heartbeat's write-failure path clears it, so a dead adapter reads false here.
            override fun boxLinkAlive(): Boolean = ocbmProbe?.client?.subscribed == true
            // Both box verbs are DISPATCHED, never awaited. OcbmProbe serialises every box command
            // on one thread that `awaitClaimable` can hold for up to ten minutes; awaiting one from
            // the supervisor's scheduler would block the thread that owns every timer in the state
            // machine, so a slow box would silently freeze grace windows and the whole ladder. The
            // ladder only needs to know the request was issued — whether the box acted on it is
            // observable from the BT_PHASE events that follow.
            override fun setBoxRadios(on: Boolean): Boolean {
                val p = ocbmProbe ?: return false
                runAsync { runCatching { p.setRadios(on) }.onFailure { emit("CT_RADIO failed: ${it.message}") } }
                return true
            }
            override fun restartBoxWireless(): Boolean {
                val p = ocbmProbe ?: return false
                runAsync { runCatching { p.disconnectPhone() }.onFailure { emit("restart-wireless failed: ${it.message}") } }
                return true
            }
            override fun pauseDiscovery() { cpRx?.pauseDiscovery() }
            override fun resumeDiscovery() { cpRx?.resumeDiscovery() }
            override fun report(phase: RxPhase, detail: String) {
                // The phase word is the supervisor's; LinkState stays the colour/label vocabulary the
                // launcher screen already speaks, so the two are mapped rather than merged.
                ui.setState(linkStateFor(phase), detail)
            }
        })
    }

    /** [RxPhase] is the truth; [LinkState] is how the launcher screen renders it. */
    private fun linkStateFor(p: RxPhase): LinkState = when (p) {
        // Restart Session tells the supervisor onStopped() first (deliberate teardown), and its
        // to(IDLE) report lands on the UI thread AFTER the RESTARTING status the command set — so
        // without this the word flipped to STOPPED for the whole settle while the detail line counted
        // down a re-claim. The flag is the truth about whether a restart is in flight; the phase is
        // honestly idle underneath it.
        RxPhase.IDLE -> if (restartInProgress.get()) LinkState.RESTARTING else LinkState.STOPPED
        RxPhase.RX_READY -> LinkState.SEARCHING
        RxPhase.RX_UNHEALTHY, RxPhase.BOX_UNHEALTHY -> LinkState.FAILED
        RxPhase.BOX_LINKED -> LinkState.CLAIMING
        RxPhase.ARMED, RxPhase.GRACE -> LinkState.WAITING
        RxPhase.BT_PAIRING -> LinkState.PAIRING
        RxPhase.BT_PAIRED, RxPhase.HANDOFF_SENT -> LinkState.PHONE_DETECTED
        // LinkState.STARTING was declared and never used by anything. This is precisely what it
        // describes: the phone has accepted the nudge and the session is coming up.
        RxPhase.INBOUND_EXPECTED -> LinkState.STARTING
        RxPhase.SESSION_UP, RxPhase.BOX_LOST_SESSION_UP -> LinkState.LIVE
        RxPhase.STALLED -> LinkState.FAILED
    }

    private fun toggleCarPlayRx() {
        val cur = cpRx
        if (cur == null) {
            emit(""); emit("==================== CARPLAY RX (Car bit + connect-out) ====================")
            // Give the receiver the proven MFi relay and a persistent peer store. Without the
            // store every session re-runs pair-setup; with a stale one, pair-verify fails and you
            // debug crypto that is fine.
            // Event-driven standby: the control connection is the signal to get the decoders and
            // seams listening, so they are ready before the first stream SETUP rather than
            // whenever an operator gets round to running carplay_ui. Callbacks are bound below via
            // [bindReceiver] — the one site shared with [rebindSessionCallbacks] — rather than passed
            // as constructor args, so the two paths cannot drift.
            val rx = CarPlayRx(
                this,
                mfiRelay = ocbmProbe?.mfiRelay(),
                peerFile = java.io.File(filesDir, "carplay_peers.bin").absolutePath
            )
            bindReceiver(rx)
            cpRx = rx; rx.start()
            // The receiver coming up is the start of the app's session, so the periodic status
            // anchor starts here — not on the phone's control connection, because the waiting phase
            // (advert up? browse up? peer seen?) is exactly where a capture opened at a random
            // offset most needs a `## STATUS` block. Idempotent across Activity generations; the
            // receiver is process-scoped and so is the ticker. Stopped by [stopEverything].
            SessionTrace.Board.startTicker(BOARD_TICK_MS)
            // Bind the Car API alongside the receiver. Safe if android.car is absent — the watcher
            // degrades to night-mode-only and says so once.
            vehicle.start()
            reportReceiverHealth(rx)
        } else { cur.stop(); cpRx = null }
    }

    /** OCBM link to the CCPA adapter; null until first use. Survives across Run-all invocations. */
    private var ocbmProbe: zeno.gmccpa.ocbm.OcbmProbe?
        get() = SessionHolder.ocbmProbe
        set(v) { SessionHolder.ocbmProbe = v }

    /**
     * Get the process-scoped probe, (RE)BINDING its observers to THIS Activity generation every time.
     *
     * The rebind is not optional. The probe outlives the Activity, so a probe built by a previous
     * generation still holds that generation's `supervisor` and `ui` in its observer lambdas; left
     * alone it would narrate the box's session into a dead UI while this one showed nothing.
     */
    private fun ocbm(): zeno.gmccpa.ocbm.OcbmProbe =
        (ocbmProbe ?: zeno.gmccpa.ocbm.OcbmProbe(applicationContext)).also {
            // The box narrates the phone's side of the session over CH_CTRL. All six observers live
            // on the probe, not the client, because the client is rebuilt on every re-claim — and
            // because the BT phases arrive DURING runAll(), before any caller could re-wire them.
            it.onSessionEvent = { sev -> onBoxSessionEvent(sev) }
            it.onPairingCode = { code -> onBoxPairingCode(code) }
            it.onBtPhase = { p -> onBoxBtPhase(p) }
            // The box's own readiness, pushed on change. This is the half of "both sides green" that
            // did not exist before: MGMT_INFO is a snapshot you have to ask for, and nothing asked
            // after bring-up.
            it.onBoxHealth = { f -> supervisor.onBoxHealth(f) }
            it.onPhoneIdent = { j -> onBoxPhoneIdent(j) }
            it.onProjMode = { m -> onBoxProjMode(m) }
            ocbmProbe = it
        }

    /**
     * `CT_SESSION_EVENT` from the adapter — the only source of truth for where the *phone* is on the
     * box's own USB bus. (Corrected 2026-08-26: this used to say the app "cannot see the
     * Bluetooth/iAP2 side at all" — false. CT_BT_PHASE, CT_PHONE_IDENT and CT_PROJ_MODE now mirror
     * that side too; see [onBoxBtPhase], [onBoxPhoneIdent], [onBoxProjMode] below.)
     */
    private fun onBoxSessionEvent(sev: Byte) {
        SessionSummary.current()?.onSessionEvent(sev)
        when (sev) {
            // These two refer to the BOX's OWN USB bus, not to Bluetooth — the label
            // "iPhone connected over Bluetooth" was simply wrong, and worse, it overwrote the state
            // word the BT_PHASE ladder had just set. Detail line only; BT progress is the
            // supervisor's to report.
            Ocbm.SEV_PHONE_PRESENT -> ui.setDetail("adapter reports a device on its USB bus")
            Ocbm.SEV_PHONE_ABSENT -> {
                ui.setPairingCode("")
                ui.setDetail("adapter reports its USB bus is idle")
            }
            Ocbm.SEV_HOST_PRESENT -> supervisor.onBoxSubscribed()
            // The box clears its own subscribed flag here and ignores further heartbeats until a
            // fresh SUBSCRIBE, so this is a real dead-end, not a blip (OcbmClient.kt:20).
            // A real dead-end, so it is a real session end. Snapshot the box's bonded list on the
            // way out FIRST: the start-vs-end diff is the bond-asymmetry signature behind the
            // "iOS lists the box but won't connect until you forget it" fault, and it is worthless
            // with only one end of the comparison. Best-effort and short — the link is already sick.
            Ocbm.SEV_HOST_GONE -> {
                // MUST be off this thread. A box-originated CT_SESSION_EVENT is dispatched inline on
                // the single `ocbm-read` thread (UsbBulkTransport.kt:161 -> OcbmClient.dispatch), and
                // mgmtGetInfo() blocks polling a queue that only that same thread ever fills — so
                // calling it here would starve itself, time out every time, and take the reader
                // offline for the timeout while it did. The end snapshot would then be silently
                // absent from exactly the sessions it exists to explain.
                SessionSummary.current()?.let { sess ->
                    runAsync {
                        runCatching { ocbmProbe?.client?.mgmtGetInfo(1500) }.getOrNull()
                            ?.let { sess.onMgmtInfoEnd(it) }
                        SessionSummary.end(sess, "host_gone")
                    }
                }
                // NOT a CarPlay session end. If A/V is streaming the supervisor deliberately holds
                // the session: the media path is Wi-Fi and does not depend on the box once it is up.
                supervisor.onBoxLost()
            }
            else -> Unit
        }
    }

    /** `CT_PAIRING_CODE` — the code the driver has to match against the prompt on the iPhone. */
    private fun onBoxPairingCode(code: String) {
        ui.setPairingCode(code)
        if (code.isNotEmpty()) setStatus(LinkState.PAIRING, "match this code on the iPhone")
    }

    /**
     * `CT_BT_PHASE` — Bluetooth/iAP2 handshake progress. Advisory/monotonic-ish (docs/lib.rs): an
     * unknown value still means "progress", so this only logs and updates the detail line — it must
     * never gate the [LinkState] machine on a particular phase arriving or arriving in order.
     */
    private fun onBoxBtPhase(phase: Byte) {
        SessionSummary.current()?.onBtPhase(phase)
        ui.setDetail("Bluetooth: ${Ocbm.btpName(phase)}")
        // BTP_WIFI_HANDOFF in particular is the only trustworthy "a CarPlay connect is coming" signal
        // we get; before the supervisor existed this method logged it and nothing acted on it. The
        // replay flag rides on the client rather than on the callback (see OcbmClient.lastBtPhaseReplay)
        // and says whether the box is reporting progress or just re-reading its latched mirror to a
        // fresh subscriber — the supervisor must not start a deadline on the latter.
        supervisor.onBtPhase(phase, ocbmProbe?.client?.lastBtPhaseReplay ?: false)
    }

    /** `CT_PHONE_IDENT` — who the connected phone is, once the box has an identity for it. */
    private fun onBoxPhoneIdent(json: String) {
        SessionSummary.current()?.onPhoneIdent(json)
        if (json.isNotEmpty()) emit("phone identity: $json")
    }

    /**
     * `CT_PROJ_MODE` — which projection transport currently owns the box. Advisory: an unknown
     * value means "some transport owns the box" — never gate on ordering.
     */
    private fun onBoxProjMode(mode: Byte) {
        SessionSummary.current()?.onProjMode(mode)
        ui.setDetail("projection: ${Ocbm.pmName(mode)}")
    }

    /**
     * `CH_MGMT` verbs. Only `MGMT_GET_INFO` is device-verified (docs/06 — identity snapshot returned
     * `CarLink-626a`); the rest are implemented in `OcbmClient.mgmtAction` and offered here, but a
     * first run on hardware is an experiment, which is why the destructive two confirm first.
     */
    private fun boxAction(a: BoxAction) {
        val c = ocbmProbe?.client
        if (c == null) {
            setStatus(LinkState.FAILED, "no adapter link — press Start first")
            return
        }
        when (a) {
            BoxAction.INFO -> {
                val json = c.mgmtGetInfo()
                emit(json ?: "MGMT_GET_INFO: no reply")
                ui.setDetail(if (json == null) "adapter did not answer MGMT_GET_INFO"
                             else "adapter info written to logcat")
            }
            // This is the ONLY lever that re-applies the hotspot credentials without a full
            // host_present cycle: ocbmd just writes /tmp/wireless_restart and ACKs immediately, then
            // session_supervisor.sh picks the flag up on its next tick (~1 s), does wireless_down,
            // waits 4 s, and calls wireless_up — and wireless_up is what runs apply_host_wifi_creds.
            // The BT controller then re-attaches over HCI ~8 s later, so BH_HCI_PRESENT comes back
            // ~12 s after the press. The ACK below means "request accepted", NOT "wireless is back";
            // the radios are genuinely down for several seconds and any live session dies with them.
            BoxAction.RESTART_WIFI -> {
                ui.setDetail("asking the adapter to bounce its wireless stack…")
                // BEFORE the verb goes out, as rung 1 does: the health regression this causes is ours,
                // and the ladder used to climb on it — an app restart on top of the box's restart.
                supervisor.onManualWirelessRestart()
                val st = c.mgmtAction(Ocbm.MGMT_RESTART_WIRELESS)
                if (st == 0) ui.setDetail("wireless restarting — radios down ~5 s, Bluetooth back in ~12 s; hotspot credentials re-applied on the way up")
                else reportMgmt("restart wireless", st)
            }
            BoxAction.REBOOT -> {
                ui.setDetail("rebooting the adapter — the session will drop")
                reportMgmt("reboot", c.mgmtAction(Ocbm.MGMT_REBOOT))
            }
            // BOTH halves, together. The box's BR/EDR bond and this app's Ed25519 CarPlay peer store
            // are separate records of the same relationship, and clearing one alone leaves the other
            // asserting a pairing the phone no longer has. That split brain surfaces as pair-verify
            // failing — which reads as broken crypto and sends you debugging code that is fine.
            BoxAction.FORGET_PHONE -> {
                ui.setPairingCode("")
                ui.setDetail("clearing the pairing on both sides — also forget the car on the iPhone")
                val boxOk = ocbmProbe?.forgetPhone(null) == true
                emit(if (boxOk) "forget: adapter bond cleared" else "forget: adapter bond NOT cleared")
                // The peer store is owned by the Rust core, which holds it open for the life of a
                // receiver. Stop the receiver first or the delete races a writer and the file comes
                // back on the next save_peer.
                val hadRx = cpRx != null
                // Tell the supervisor the teardown is deliberate BEFORE it happens. Otherwise the
                // session-down that stop() produces looks like a fault to it, and it would answer a
                // user-requested reset by opening a grace window and climbing the recovery ladder.
                // stop() now drains its pumps, so the session-down lands inside this call, not after
                // the replacement receiver has already reported itself ready.
                supervisor.onStopped()
                // stop() now REPORTS whether the pumps actually unwound. They do not when one is
                // parked inside NativeCore.feed — that is JNI, not read(), so the interrupt does not
                // land and /auth-setup can hold it for the full 12-15 s MFi budget. Deleting the peer
                // store while a native core is still live lets its save_peer resurrect the file
                // moments later, which is the exact split-brain this action exists to clear. So the
                // delete is sequenced BEHIND a confirmed drain, with a second bounded wait rather
                // than a hopeful one.
                val drained = (cpRx?.stop() ?: true) || waitForPeerStoreQuiet()
                cpRx = null
                sessionUp = false
                val peers = java.io.File(filesDir, "carplay_peers.bin")
                val appOk = if (!drained) {
                    emit("forget: a receiver pump is still in the native core — NOT deleting the peer store")
                    false
                } else !peers.exists() || peers.delete()
                emit(if (appOk) "forget: app CarPlay pairings cleared" else "forget: could NOT delete ${peers.name}")
                if (hadRx) toggleCarPlayRx()   // back up clean, advertising a receiver with no peers
                ui.setDetail(
                    if (boxOk && appOk) "pairing cleared on both sides — now forget this car on the iPhone"
                    else "pairing only PARTLY cleared — see the log; do not re-pair until it is clean"
                )
            }
        }
    }

    /**
     * Driver-triggered recovery, cheapest rung first.
     *
     * Re-runs the receiver's readiness probe before touching the box, because half of what "CarPlay
     * will not start" turns out to mean is that this side was never ready — and that is both the
     * cheapest thing to check and the one the driver has no other way to see.
     *
     * A live session or an open grace window is refused by the supervisor, not here: the probe's
     * verdict is still worth logging, and `sessionLive` read on this thread can be a step behind the
     * phase the supervisor owns. See `SessionSupervisor.onReceiverReady` / `userRecover`.
     */
    private fun userRecover() {
        emit("")
        emit("==================== DRIVER-REQUESTED RECOVERY ====================")
        val rx = cpRx
        if (rx == null) {
            emit("no receiver running — starting one")
            autoStart(manual = true)
            return
        }
        reportReceiverHealth(rx)
        supervisor.userRecover()
    }

    /** Confirmation for the three adapter verbs that cannot be walked back by pressing Start again. */
    private fun confirmBoxAction(a: BoxAction) {
        android.app.AlertDialog.Builder(this, android.R.style.Theme_DeviceDefault_Dialog_Alert)
            .setTitle(a.label)
            .setMessage(when (a) {
                BoxAction.REBOOT ->
                    "Reboot the adapter?\n\nAny live CarPlay session drops immediately and the box " +
                    "needs a fresh claim afterwards."
                BoxAction.RESTART_WIFI ->
                    "Restart the adapter's wireless stack?\n\nThe radios go down for about five " +
                    "seconds, Bluetooth takes ~12 s to come back, and a live session drops. This is " +
                    "also the only way to re-apply changed hotspot credentials without unplugging."
                BoxAction.FORGET_PHONE ->
                    (if (cpRx?.sessionLive == true)
                        "A CarPlay session is RUNNING and this will end it.\n\n" else "") +
                    "Clear the pairing on both the adapter and this app?\n\nYou must ALSO forget " +
                    "this car on the iPhone — a one-sided pairing makes the next attempt fail in a " +
                    "way that looks like broken encryption."
                else -> "Continue?"
            })
            .setPositiveButton(a.label) { _, _ -> runAsync { boxAction(a) } }
            .setNegativeButton("Cancel", null)
            .show()
    }

    /** MGMT_ACK status: 0 ok, non-zero error, null no reply at all. */
    private fun reportMgmt(what: String, status: Int?) = when (status) {
        0 -> ui.setDetail("$what: adapter acknowledged")
        null -> ui.setDetail("$what: no reply from the adapter")
        else -> ui.setDetail("$what: adapter returned error $status")
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // A launcher screen, not an instrument panel. The instrument is logcat (`-s NETPROBE`), which
        // is the only channel that survives the CarPlay screen taking over the display anyway — this
        // Activity is backgrounded for the entire session it is meant to report on. What is left is
        // the connection state, a manual override, and the credentials the 0x5703 handoff needs.
        //
        // The credentials sit behind a dialog rather than on the screen: they are set once per
        // vehicle and then never touched, whereas the state is the thing being read at a glance while
        // the truck moves. The passphrase cannot be read programmatically on this head unit
        // (SecurityException on getSoftApConfiguration, EACCES on hostapd.conf), so LauncherUi
        // prefills the known value and keeps it editable. --es ssid/--es pass still override.
        ui = LauncherUi(this).apply {
            onStart = { runAsync { autoStart(manual = true) } }
            // abortClaimWait first, on THIS thread: a Start with no adapter sits in awaitClaimable
            // for up to ten minutes ON the command executor, so the stop queued behind it could not
            // run until the wait ended — Stop was dead for exactly as long as it was needed. The
            // flag is the only thing that ends the wait early (OcbmProbe.claimAbort).
            onStop = {
                // Raised BEFORE queueing: a Start already queued ahead could otherwise begin on the
                // executor between the two statements and miss it. Withdrawn if the queue is closed,
                // or a process-wide flag would refuse every Start of the next Activity generation.
                SessionHolder.stopRequested = true
                ocbmProbe?.abortClaimWait()
                if (!runAsync { stopEverything() }) SessionHolder.stopRequested = false
            }
            onRecover = { runAsync { userRecover() } }
            onRestart = { requestRestart() }
            // Credentials confirmed in the dialog reach the probe immediately, so a Start that
            // follows cannot use the previous values.
            onCredentials = { _, _, _ -> runAsync { applyHotspotFields() } }
            // Reboot, restart-wireless and forget-bond are destructive to a live session and, unlike everything else on
            // this screen, are not undone by pressing Start again — so they ask first.
            onBoxAction = { a ->
                when (a) {
                    BoxAction.REBOOT, BoxAction.FORGET_PHONE, BoxAction.RESTART_WIFI ->
                        confirmBoxAction(a)
                    else -> runAsync { boxAction(a) }
                }
            }
            onLogAction = { a ->
                when (a) {
                    LogAction.EXPORT -> exportLogs(redact = true)
                    LogAction.SCOPE -> setCaptureScope(
                        if (CapturePrefs.scope(this@MainActivity) == LogCapture.Scope.WHOLE_OS)
                            LogCapture.Scope.OWN_PROCESS else LogCapture.Scope.WHOLE_OS
                    )
                    LogAction.STATUS -> {
                        val st = LogCapture.status()
                        emit(st.toString())
                        ui.setDetail(
                            "capture ${st.effectiveScope}: ${st.fileCount} files, " +
                                "${st.bytesOnDisk / 1024 / 1024} MB, ${st.linesDropped} dropped"
                        )
                    }
                }
            }
        }
        setContentView(ui.root)
        // BEFORE anything decides whether to start a session: the receiver and the OCBM probe are
        // process-scoped and may already be running from a previous Activity generation (see
        // SessionHolder). Re-attach them to THIS generation, or their callbacks keep driving a dead
        // supervisor and a dead Ui while this screen shows an idle launcher.
        rebindSessionCallbacks()

        // The PID anchor. Replaces a banner that carried uid and package but not the PID, which is
        // the only key that attributes this process's ~250 framework lines per session (CCodec,
        // MediaCodec, BufferQueueProducer, …) in a whole-OS capture. See ProbeLog.identity.
        ProbeLog.identity(this)
        // Capture was only ever started from BootReceiver, the USB trampoline and the scope menu —
        // never from here, so a launcher start recorded nothing at all and the drive-ready process
        // had no logcap-pump thread. start() is idempotent and self-reaping, so calling it on every
        // onCreate is free when capture is already running.
        runCatching { LogCapture.start(applicationContext, CapturePrefs.config(this)) }
        dispatchIntent(intent)
    }

    /** The intent decides the path: a USB attach or an --es run verb is explicit; anything else
     *  (launcher icon, task switch) means "just bring the session up". Shared by [onCreate] and
     *  [onNewIntent] so the two paths cannot drift. */
    private fun dispatchIntent(i: Intent?) {
        if (handleAttachIntent(i)) return
        if (i?.getStringExtra("run") != null) handleRunExtra(i) else runAsync { autoStart(manual = false) }
    }

    /**
     * Run the receiver's readiness probe and hand the verdict to the supervisor and the log.
     *
     * Deliberately runs the moment the receiver starts and BEFORE the box is asked to wake its
     * radios, which is the ordering the whole design turns on: there is no point handing an iPhone
     * our SSID if we cannot accept the session it will then try to open.
     */
    private fun reportReceiverHealth(rx: CarPlayRx) {
        val checks = runCatching { rx.selfTest() }.getOrElse {
            emit("!! receiver self-test threw: ${it.javaClass.simpleName}: ${it.message}")
            supervisor.onReceiverReady(false, "self-test threw")
            return
        }
        emit("---- receiver readiness ----")
        checks.forEach { emit("  ${if (it.ok) "OK  " else "FAIL"}  ${it.name}: ${it.detail}") }
        val bad = checks.filter { !it.ok }
        supervisor.onReceiverReady(
            bad.isEmpty(),
            if (bad.isEmpty()) "receiver ready to accept a session"
            else bad.joinToString("; ") { it.name }
        )
    }

    /** State + one-line detail for the launcher screen. The per-packet detail lives in logcat. */
    private fun setStatus(state: LinkState, detail: String) = ui.setState(state, detail)

    /**
     * The whole product flow: adapter present -> claim + init the box -> the box does BT/iAP2 and the
     * handoff -> the phone dials in -> `onSessionUp` stands the A/V consumers up -> CarPlay.
     *
     * Idempotent. [manual] only changes the message when there is nothing to do, so the Start button
     * says something useful instead of appearing dead.
     *
     * Returns what the bring-up achieved, or null when a guard found nothing to do — Restart Session
     * needs the distinction to name its failure (no adapter / no HELLO / subscribed) rather than
     * reading it back off the status line.
     */
    private fun autoStart(manual: Boolean): zeno.gmccpa.ocbm.OcbmProbe.LinkResult? {
        // Never tear down a live CarPlay session to "start" one.
        //
        // The `subscribed` guard below is not sufficient on its own. After a mid-session USB re-attach
        // the link is deliberately re-established WITHOUT subscribing (see handleAttachIntent), so
        // `subscribed` reads false while CarPlay is streaming — and runAll() begins with stopLocked(),
        // which would kill the fresh client and the relay underneath the session. Check the thing that
        // actually matters first.
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
        // A Stop pressed while this Start was still queued: its press-time abortClaimWait found no
        // wait to abort (or a probe this run would replace), and awaitClaimable resets the abort flag
        // on entry — so without this the queued Start ran the full ten-minute wait with the Stop
        // parked behind it. The flag is consumed by stopEverything, which runs next.
        if (SessionHolder.stopRequested) {
            emit("Stop was pressed before this Start ran — not starting; the stop runs next")
            return null
        }
        setStatus(LinkState.SEARCHING, "looking for the adapter…")
        applyHotspotFields()
        // Make the ordering dependency explicit rather than incidental. CarPlayRx captures
        // `ocbmProbe.mfiRelay()` ONCE, in its constructor (CarPlayRx.kt:51); if the probe does not
        // exist yet the receiver is built with a null relay and stays pairing-incapable for the
        // whole process. Today applyHotspotFields() above happens to call ocbm() already, so the
        // probe exists by the time we get here — but that is a side effect of a function named for
        // something else, and moving or guarding it would silently break pairing. This call is
        // idempotent; it costs nothing and pins the requirement in place.
        ocbm()
        if (cpRx == null) toggleCarPlayRx()      // advertise before the box even wakes its radios
        setStatus(LinkState.CLAIMING, "claiming the adapter…")
        try {
            // GATE ON THE RESULT. runAll() reports failure by RETURNING, not by throwing (every
            // "ABORT:" path inside it is a bare return), so this catch never fired for "no adapter",
            // "claim failed" or "no CT_HELLO_ACK" -- and the app announced "box claimed, MFi proven"
            // with no box on the bus at all. Device-observed 2026-08-28.
            val r = ocbm().runAll()
            if (!r.helloOk) {
                setStatus(LinkState.FAILED, r.failureDetail())
                emit("link NOT established: ${r.failureDetail()}")
                return r
            }
            // The supervisor writes the state word from here on. A synchronous setStatus() next to an
            // async supervisor event raced it, and losing that race visibly reverted the status line.
            supervisor.onBoxLinked(r.mfiProven)
            return r
        } catch (t: Throwable) {
            setStatus(LinkState.FAILED, t.message ?: t.javaClass.simpleName)
            throw t
        }
    }

    private fun stopEverything() = try { stopEverythingLocked() } finally { SessionHolder.stopRequested = false }

    private fun stopEverythingLocked() {
        sessionUp = false   // else onResume would relaunch the CarPlay screen after an explicit Stop
        // FIRST, before anything is torn down. Every armed expectation is about a step this stop
        // makes impossible, and `CarPlayRx.stop` can spend up to 1.5 s draining a pump parked in
        // `NativeCore.feed` — long enough for a near-expiry watchdog to fire `EXPECTED-MISSING` on
        // a step the operator just cancelled. That line would be worse than none: it is the
        // fastest way to teach a reader to ignore the mechanism.
        SessionTrace.cancelAll("operator stop (stopEverything)")
        cpRx?.stop(); cpRx = null
        // Stamped AFTER stop() for Restart Session's settle (see restartSession for why after): Stop
        // followed by Restart inside five seconds is the same detached-teardown race as a restart
        // alone, and only the stamp lets it be caught.
        ocbmProbe?.let { it.stop(); emit(it.stats()); SessionHolder.lastProbeStopAt = android.os.SystemClock.elapsedRealtime() }
        ocbmProbe = null
        supervisor.onStopped()
        // Stops the ticker and forgets every entry; the supervisor's (posted, so later) `to(IDLE)`
        // re-registers `phase: idle` as the only thing on a clean board.
        SessionTrace.Board.reset("operator stop (stopEverything)")
        setStatus(LinkState.STOPPED, "idle — press Start or plug the adapter in")
    }

    // ---- Restart Session ----------------------------------------------------------------------------

    /**
     * Minimum gap between the box losing us (CT_STOP, or the heartbeat timeout if that write was lost)
     * and our next CT_SUBSCRIBE.
     *
     * Grounded in the box supervisor, not chosen. `wireless_down` is a DETACHED teardown and
     * `wireless_up` early-returns while it is still running, so a presence edge that lands inside the
     * teardown is swallowed: the bring-up simply does not happen and nothing says so. The supervisor's
     * own settle for that hazard is 4 s, used twice (the Restart-wireless deferral and the CT_RADIO
     * on-edge, tools/session_supervisor.sh `wireless_rebring_at`), with the comment that a quick
     * off->on "must not race the off-edge's detached teardown". 5 s clears it with margin, and one
     * restart is one presence edge against FLAP_N=5 / FLAP_WINDOW=20 s. Do not shorten.
     */
    private val RESTART_SETTLE_MS = 5_000L

    /**
     * True from the moment Restart Session is pressed until [restartSession] has finished.
     *
     * Taken on the UI thread AT THE PRESS, not inside the command: the command executor is
     * single-threaded, so a press during a bring-up queues behind it, and a second press during that
     * wait would queue a SECOND full restart that tears down the session the first had just rebuilt.
     * Taking the flag at the press rejects the repeat at once and lets the button read "Restarting…"
     * while the command is still waiting its turn. Also what [linkStateFor] reads to render the
     * supervisor's IDLE as RESTARTING.
     */
    private val restartInProgress = java.util.concurrent.atomic.AtomicBoolean(false)

    /** The Restart Session button. UI thread. Safe in any state, any number of times. */
    private fun requestRestart() {
        if (!restartInProgress.compareAndSet(false, true)) {
            emit("restart already in progress — ignoring the repeat press")
            ui.setDetail("restart already in progress…")
            return
        }
        ui.setRestartBusy(true)
        // Same reason as the Stop button above: a Start parked in awaitClaimable holds the command
        // executor, and only this flag ends the wait early. A restart pressed during that wait
        // restarts the wait, which is the right reading of the press.
        ocbmProbe?.abortClaimWait()
        if (!runAsync { restartSession() }) { restartInProgress.set(false); ui.setRestartBusy(false) }
    }

    /**
     * The one button that fixed things in the old carlink_native app: drop the phone, release the box,
     * wait, re-claim. For the driver who cannot force-stop the app or reach the adapter.
     *
     * What it deliberately does NOT do:
     *  - Send a second disconnect after the re-claim. `MGMT_RESTART_WIRELESS` into a fresh bring-up is
     *    the 2026-09-08 collapse (BT phases collapse, PROJ_MODE NONE, `EAAccessoryLeft`, and the phone
     *    then 200-OKs every nudge and never dials back). The clean slate already comes from CT_STOP ->
     *    `go_idle` plus `wireless_up`'s carplayd reap and its fresh `apply_host_wifi_creds`.
     *  - Touch the pairing. No `forgetPhone`, no `MGMT_FORGET_*`, and the peer store is not deleted:
     *    `go_idle` and `wireless_down` never touch the box's key store, and `carplay_peers.bin` is
     *    deleted only by Forget Pairing. A restart must never cost the driver a re-pair.
     *  - Retry on its own. Each failure is named so the driver knows whether pressing again can help.
     *
     * Ordering that is load-bearing: the supervisor is told first (the teardown is deliberate, not a
     * fault); the probe is dropped and rebuilt so its HELLO carries a NEW nonce (OcbmProbe.hostInstance
     * — a lost CT_STOP is otherwise read by the box as "presence never dropped" and no wireless_up
     * runs); and [autoStart] rebuilds the probe BEFORE the receiver, because CarPlayRx captures
     * `mfiRelay()` in its constructor.
     */
    private fun restartSession() {
        try {
            emit(""); emit("==================== DRIVER-REQUESTED SESSION RESTART ====================")
            setStatus(LinkState.RESTARTING, "closing the session…")
            // The supervisor FIRST, then the expectations — same order and reason as Forget Pairing:
            // told afterwards, it would read the session-down as a fault, open a grace window and climb
            // the ladder on a receiver we are about to replace.
            supervisor.onStopped()
            SessionTrace.cancelAll("operator restart (restartSession)")
            sessionUp = false
            val rx = cpRx
            if (rx != null) {
                // A pump parked in NativeCore.feed is recorded, not waited for. Only Forget needs the
                // drain, because only Forget deletes a file a still-live core could resurrect; here the
                // fresh receiver IS the fix and the old core is fenced from every callback.
                if (!rx.stop()) emit("restart: a receiver pump did not unwind within 1500 ms (parked in NativeCore.feed) — proceeding; the fresh receiver replaces it")
                cpRx = null
                tearDownSessionConsumers("driver restarted the session")
            }
            val old = ocbmProbe
            var usbStuck = false
            if (old != null) {
                old.stop(quick = true)
                // Stamped AFTER stop(), because the settle is measured from the box's GONE edge and
                // that edge is `go_idle` on CT_STOP — which is the LAST thing OcbmClient.stop() writes,
                // behind the bounded log pull (up to 4 x 1 s), CT_LOG_CTL off and the <=3 s heartbeat
                // join. Stamped before, a box that is alive but slow on CH_FILE ate the whole settle
                // inside stop(): `left` was already <= 0, the loop was skipped, and CT_SUBSCRIBE landed
                // 2-3 s after go_idle — inside the detached wireless_down the 5 s exists to clear.
                // Silent failure: ARMED, radios never up, and the deadline blames the phone. (If the
                // CT_STOP write is lost the edge is the heartbeat timeout instead; the fresh nonce is
                // what makes that case recoverable — see OcbmProbe.hostInstance.)
                SessionHolder.lastProbeStopAt = android.os.SystemClock.elapsedRealtime()
                emit(old.stats())
                usbStuck = old.usbReleaseDeferred
                ocbmProbe = null
            }
            SessionTrace.Board.reset("operator restart (restartSession)")
            // Settle from the LAST probe stop, whoever made it: Stop followed by Restart inside five
            // seconds is the same edge race as a restart alone, and this is where it can be caught.
            // A restart with nothing running and no recent stop skips straight to the bring-up.
            val since = SessionHolder.lastProbeStopAt
            if (since != 0L) {
                var left = RESTART_SETTLE_MS - (android.os.SystemClock.elapsedRealtime() - since)
                while (left > 0) {
                    ui.setDetail("adapter released — re-claiming in ${(left + 999) / 1000} s")
                    Thread.sleep(minOf(left, 1_000L))
                    left = RESTART_SETTLE_MS - (android.os.SystemClock.elapsedRealtime() - since)
                }
            }
            if (usbStuck) emit("restart: the old link's USB read thread is still parked in bulkTransfer — this process still holds the interface, so the re-claim below is expected to be refused")
            // awaitClaimable waits up to ten minutes and says nothing to the screen meanwhile. Tell the
            // driver at 30 s what it is waiting on and leave the wait running — an adapter appearing
            // late is the common case, not a fault. A UI-thread timer because the command thread is
            // inside autoStart for the duration; `client == null` is what "still waiting to claim"
            // looks like from outside, and the flag drops the message if the restart has moved on.
            ui.root.postDelayed({
                val p = ocbmProbe
                if (restartInProgress.get() && p != null && p.client == null) ui.setDetail(
                    if (p.claimWaitState == "present-no-permission")
                        "adapter is on the bus but USB permission is not held — accept the dialog (still waiting)"
                    else "no adapter on the USB bus after 30 s — still waiting (up to 10 min); check the cable"
                )
            }, 30_000L)
            // A Stop pressed during the settle has nothing to abort (the probe is already gone) and is
            // queued BEHIND the bring-up below — whose awaitClaimable can hold the executor for ten
            // minutes with no adapter attached. The later press wins: leave the bring-up to Stop.
            if (SessionHolder.stopRequested) {
                emit("restart: Stop was pressed during the settle — not re-claiming; the stop runs next")
                return
            }
            val r = autoStart(manual = true)   // narrates "looking for…" / "claiming the adapter…" itself
            when {
                // Both guards passed everything-null moments ago; something else brought a session up
                // during the settle. It is up — leave it alone.
                r == null -> emit("restart: a session came up on its own during the settle — leaving it")
                // autoStart already set FAILED with the generic "no adapter claimed". Say more only when
                // we KNOW the reason is this process.
                !r.claimed && usbStuck -> setStatus(LinkState.FAILED,
                    "adapter re-claim refused: this app still holds the USB interface (the old link's read is stuck in the kernel) — no adapter command can fix that; unplug and replug the adapter, or restart the app")
                !r.claimed -> Unit
                !r.helloOk -> setStatus(LinkState.FAILED,
                    "adapter claimed but ocbmd did not answer HELLO in 20 s — the box's own supervisor restarts a wedged ocbmd within ~2 min; press Restart Session again after that")
                !r.subscribed -> Unit   // a credential refusal throws before this; runAll narrates anything else
                else -> {
                    // ARMED has no clock of its own; give this one a deadline, and a named verdict.
                    supervisor.onRestartSubscribed()
                    ui.setDetail("adapter up — waiting for the iPhone to reconnect Bluetooth")
                }
            }
        } finally {
            restartInProgress.set(false)
            ui.setRestartBusy(false)
        }
    }

    override fun onNewIntent(intent: Intent?) {
        super.onNewIntent(intent)
        dispatchIntent(intent)
    }

    /**
     * Entered on adapter attach — but NOT directly from the platform anymore. The platform launches
     * [UsbAttachActivity]; that NoDisplay trampoline filters to the OCBM adapter and forwards a
     * matching ACTION_USB_DEVICE_ATTACHED intent here. Permission comes from the ordinary attach
     * resolver: the framework shows the standard USB dialog once per device, "always open" caches
     * it, and the grant to our UID lands before the trampoline is launched — so by the time this
     * runs `hasPermission(dev)` is normally true. Normally, not always: on a cold start the grant
     * can commit after we sample it (see UsbAttachActivity's "process age" note), which is why
     * UsbBulkTransport polls hasPermission() instead of trusting this. (Until 2026-09-08 the
     * app installed as `android.car.usb.handler`, the GM USB fixed-handler squat — see the manifest
     * header + `ccpa_custom/docs/host/01_ANDROID_AND_AAOS.md` §"GM AAOS USB permission handler". That
     * squat was SUPPOSED to make the framework grant our UID USB permission silently on every attach;
     * corrected 2026-09-10, it did not — the app still needed the user to grant adapter permission,
     * which is why it was reverted. The squat was reverted; the install package is `zeno.gmccpa` again.)
     * In a wireless-only design this attach is also the only physical trigger there is. Device-proven
     * 2026-08-17.
     */
    private fun handleAttachIntent(i: Intent?): Boolean {
        if (i?.action != UsbManager.ACTION_USB_DEVICE_ATTACHED) return false
        @Suppress("DEPRECATION")
        val dev = i.getParcelableExtra<UsbDevice>(UsbManager.EXTRA_DEVICE) ?: return false
        ProbeLog.banner("USB ATTACH 0x%04x:0x%04x forwarded from the trampoline — permission is polled by the claim loop, not assumed"
            .format(dev.vendorId, dev.productId))
        if (dev.vendorId != zeno.gmccpa.ocbm.UsbBulkTransport.VID_CARLINKIT) return false
        if (dev.productId != zeno.gmccpa.ocbm.UsbBulkTransport.PID_OCBM) {
            emit("attached 0x%04x is not the OCBM PID — box needs ocbm_boot.sh".format(dev.productId))
            return false
        }
        // Carry over whatever hotspot credentials are already on screen.
        applyHotspotFields()
        runAsync {
            // Order matters, and matching autoStart() here is the whole point: the receiver must be
            // listening on :7011 and advertised BEFORE the box wakes its radios.
            //
            // This path used to call ocbm().runAll() alone. The box then ran its entire ladder —
            // HELLO, SETTIME, MFi, SUBSCRIBE, BT pair, 0x5703 Wi-Fi handoff — into an app with no
            // listener and no mDNS advert, and reported no error anywhere: the log showed a textbook
            // bring-up and the phone had nothing to discover. Since the attach is the RECOMMENDED
            // launch path (it is the one carrying the implicit USB grant), that was the DEFAULT
            // cold-start behaviour. Device-observed 2026-08-27.
            ocbm()
            if (cpRx == null) toggleCarPlayRx()
            // A re-attach while CarPlay is streaming must not re-take the radio-wake edge. runAll()
            // sends CT_STOP then a fresh CT_SUBSCRIBE, flipping host_present 0->1, which makes the
            // box bring its BT stack up underneath a live session. CarPlay does not depend on BT once
            // streaming, so leave both alone.
            //
            // The link itself IS re-established, only the subscribe is withheld. Skipping the whole
            // bring-up would leave the CH_MFI relay dead, and /auth-setup runs per CONTROL CONNECTION
            // rather than per pairing — so the next reconnect or hijack would fail to authenticate,
            // not merely lose Bluetooth. runAll(subscribe = false) restores claim + HELLO + SETTIME +
            // MFi and stops short of the radio-wake edge.
            if (cpRx?.sessionLive == true) {
                emit("USB re-attach with a live CarPlay session — restoring the link WITHOUT " +
                     "CT_SUBSCRIBE (a fresh subscribe would re-wake the box radios mid-session)")
                ocbm().runAll(subscribe = false)
                // Tells the supervisor this is a mid-session return, so it holds the box's radios
                // down (CT_RADIO) instead of letting BT come up underneath a streaming session.
                supervisor.onBoxRelinked()
            } else {
                val r = ocbm().runAll()
                if (r.helloOk) supervisor.onBoxLinked(r.mfiProven)
                else emit("USB attach: link NOT established — ${r.failureDetail()}")
            }
        }
        return true
    }

    /**
     * Push the on-screen hotspot fields into the OCBM probe, so `CT_SUBSCRIBE` carries the VEHICLE's
     * credentials for the `0x5703` handoff.
     *
     * **Every path that starts the OCBM link must call this first.** The box applies credentials only
     * inside `wireless_up()` (`session_supervisor.sh` -> `apply_host_wifi_creds`), which runs on the
     * `host_present` 0->1 edge. A credential-less SUBSCRIBE therefore brings the wireless stack up with
     * the box's STOCK `/etc/hostapd.conf`, and a later SUBSCRIBE that does carry credentials will NOT
     * re-apply them — `wireless_up` has already run. The handoff then hands the iPhone an SSID that is
     * never raised (`wifi_ap:false`), the phone joins nothing, and per 06 §5.4 the dead network also
     * poisons the next attempt until it is forgotten on the phone. Recovering needs a full
     * `host_present` cycle. Device-observed 2026-08-12.
     */
    private fun applyHotspotFields() {
        ocbm().apply {
            wifiSsid = ui.ssid
            wifiPass = ui.pass
            wifiChannel = ui.chan
        }
    }

    /**
     * Scriptable entry point, so bring-up can be driven from a shell instead of by tapping:
     *   adb shell am start -n zeno.gmccpa/zeno.gmccpa.MainActivity --es run ocbm_selftest
     *   (the am component is <install-pkg>/<class>. Install package and class package are the same
     *    again since the android.car.usb.handler USB fixed-handler squat was reverted 2026-09-08;
     *    this example said android.car.usb.handler/… until 2026-09-09. Never re-hard-code it in a
     *    printed string — see LogCapture.grantCmd, which derives it from ctx.packageName.
     *    See the manifest header.)
     * Accepts: ocbm_selftest | ocbm_link | ocbm_state | ocbm_disconnect | ocbm_forget | ocbm_stop
     *          | carplay_rx | carplay_stop | carplay_ui | full | mdns_self
     *          | export_log | export_log_raw | capture_status | capture_whole_os | capture_own
     */
    private fun handleRunExtra(intent: Intent?) {
        val i = intent ?: return
        val what = i.getStringExtra("run") ?: return
        // Hotspot credentials for the 0x5703 handoff, from the shell or the on-screen field.
        ui.setCredentials(i.getStringExtra("ssid"), i.getStringExtra("pass"), i.getStringExtra("chan"))
        applyHotspotFields()
        emit("")
        emit(">>> scripted run: $what")
        when (what) {
            "export_log" -> exportLogs(redact = true)
            // Deliberately a separate verb, never a flag on the redacted one: an unredacted export
            // carries the vehicle hotspot passphrase and the phone's BR/EDR MAC off the vehicle on a
            // removable stick, so it has to be asked for in full by someone who meant it.
            "export_log_raw" -> exportLogs(redact = false)
            "capture_status" -> emit(LogCapture.status().toString())
            "capture_whole_os" -> setCaptureScope(LogCapture.Scope.WHOLE_OS)
            "capture_own" -> setCaptureScope(LogCapture.Scope.OWN_PROCESS)
            "ocbm_selftest" -> runAsync { ocbm().selfTest() }
            "ocbm_link" -> runAsync { ocbm().runAll() }
            // ocbm() first for the same reason as "full" below: CarPlayRx captures the MFi relay in
            // its constructor, and a null relay silently disables the native core.
            "carplay_rx" -> runAsync { ocbm(); if (cpRx == null) toggleCarPlayRx() }
            // Both planes at once: the OCBM/BT link that drives the handoff, and the Wi-Fi endpoint
            // the phone is supposed to find afterwards. This is the real end-to-end sequence.
            "full" -> runAsync {
                // Create the OCBM probe FIRST so CarPlayRx can capture its MFi relay. The relay
                // resolves `client` lazily at call time, so the link does not have to be up yet —
                // but the probe instance must exist or the receiver starts without a signer.
                ocbm()
                if (cpRx == null) toggleCarPlayRx()
                ocbm().runAll()
            }
            // The real UI: fullscreen HEVC + AAC + touch.
            "carplay_ui" -> runAsync { launchCarPlayUi() }
            "mdns_self" -> runAsync { MdnsInspect.inspect(CarPlayRx.ACCESSORY_NAME) }
            "carplay_stop" -> runAsync { cpRx?.stop(); cpRx = null }
            "ocbm_state" -> runAsync { ocbmProbe?.sessionState() ?: emit("no OCBM link") }
            // Session teardown. disconnect = drop BT, keep the bond (test hygiene between runs).
            // forget = clear the bond entirely; also forget the car on the iPhone.
            "ocbm_disconnect" -> runAsync { ocbmProbe?.disconnectPhone() ?: emit("no OCBM link") }
            "ocbm_forget" -> runAsync { ocbmProbe?.forgetPhone(i.getStringExtra("mac")) ?: emit("no OCBM link") }
            "ocbm_stop" -> runAsync {
                ocbmProbe?.let { it.stop(); emit(it.stats()); SessionHolder.lastProbeStopAt = android.os.SystemClock.elapsedRealtime() }
                ocbmProbe = null
            }
            else -> emit("unknown run extra '$what'")
        }
    }

    // Single-thread executor: a fresh Thread per command let two Starts delivered close together both
    // observe `cpRx == null` and each build a CarPlayRx. The second `bind(:7011)` fails EADDRINUSE
    // (SO_REUSEADDR does not permit a second LISTEN), so its self-test reports "listening" FAIL and
    // the supervisor declares RX_UNHEALTHY — while both instances advertise `gmccpa-rx.local`, the
    // double-answer hazard CarPlayRx warns about. Serializing commands makes the check-and-start
    // atomic w.r.t. other commands.
    private val cmdExecutor = java.util.concurrent.Executors.newSingleThreadExecutor { r ->
        Thread(r, "netprobe-cmd").apply { isDaemon = true }
    }
    /**
     * Bring the CarPlay screen up. Idempotent: `CarPlayActivity` is `singleTop` on its own task
     * affinity, so a repeat brings the existing instance forward instead of creating a second one
     * that would fight for :9001/:9002/:9003.
     *
     * FLAG_ACTIVITY_NEW_TASK is what makes that affinity take effect — affinity alone does nothing
     * without it, and both activities would stay in one task where MainActivity's launch clears
     * everything above it and kills the CarPlay screen.
     */
    private fun launchCarPlayUi() {
        sessionUp = true
        // Tell the AAOS media card a session exists. Deliberately NOT a flip to PLAYING: playback
        // state is whatever iOS reports in the first nowPlaying record, never inferred from a session
        // existing or from bytes on the audio seam — the phone may well hand us a paused player.
        zeno.gmccpa.av.CarPlayMediaBrowserService.onSessionUp()
        // The supervisor sets the state word (via report -> Ui.setState). Setting it here too made
        // two writers race for one line, so the label flickered between whichever landed last.
        supervisor.onSessionUp()
        ui.setPairingCode("")
        startActivity(
            android.content.Intent(this, zeno.gmccpa.av.CarPlayActivity::class.java)
                .addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK)
        )
    }

    /**
     * The phone's control connection went away — clean teardown or abrupt link loss, indistinguishable
     * from here.
     *
     * Without this the launcher screen kept claiming LIVE and [sessionUp] stayed true forever, so
     * [onResume] re-launched CarPlayActivity over a dead session: black screen, audio consumers bound
     * to a producer that had gone, and touches logged `sent=false`. Device-observed 2026-08-27.
     */
    private fun onCarPlaySessionDown() {
        sessionUp = false
        // Deliberately NOT SessionTrace.cancelAll here. The receiver's own per-generation
        // expectations (pair-verify, auth-setup) are dropped by the pump whose finally fired this;
        // the box-side expectations are about the OCBM link, which the phone leaving does not end,
        // and a blanket cancel would silence a genuine box fault in flight. A/V expectations that
        // are session-scoped belong to CarPlayActivity.onSessionEnded below, which owns that seam.
        // TAKE THE SCREEN DOWN WITH THE SESSION. This used to stop at the three lines below, none of
        // which the CarPlay screen can see — so a phone that went out of Wi-Fi range left its last
        // decoded frame frozen on the display over a dead session, swallowing touches, and the
        // returning phone could not rebuild it because startSession() guards on "already started".
        // Device-reported 2026-08-28. See CarPlayActivity.onSessionEnded.
        tearDownSessionConsumers("control connection went away")
        supervisor.onSessionDown()   // owns the state word and the grace window
    }

    /**
     * Everything that must go down WITH a CarPlay session, minus the supervisor. Shared by the
     * receiver's session-down above and by [restartSession], whose deliberate `stop()` fences that
     * callback (CarPlayRx.stopped) — so the restart has to do this itself or a mid-session restart
     * leaves the CarPlay screen frozen on its last frame over a dead session.
     */
    private fun tearDownSessionConsumers(why: String) {
        zeno.gmccpa.av.CarPlayActivity.onSessionEnded(why)
        // The card must go idle with the session. This IS an inference we are entitled to make: the
        // phone never sends a final "stopped" record, it simply stops sending.
        zeno.gmccpa.av.CarPlayMediaBrowserService.onSessionDown()
        // Both vehicle levers are session-scoped — the receiver refuses them with no event channel —
        // so drop the sent-state here and let onSessionUp push them fresh.
        vehicle.onSessionDown()
        ui.setPairingCode("")
    }

    /**
     * The phone can reach us and is answering the connect-out, but will not open the control
     * connection. See `CarPlayRx.noteDialAccepted` for the evidence behind the wording; the action
     * named here is the only one observed to clear it.
     */
    private fun onCarPlayStalled(dials: Int) {
        // The supervisor renders the sentence; it holds the terminal phase this belongs to.
        supervisor.onStalled(dials)
    }

    /**
     * The receiver's accept loop gave up and closed :7011 while the receiver was otherwise up. This
     * is the same verdict [reportReceiverHealth] would reach if anything re-ran it — `listening`
     * now reads false — delivered on the edge instead, because nothing re-runs it. Goes through the
     * existing onReceiverReady(false) entry so the phase word and the launcher render (FAILED) are
     * the ones a failed readiness check already produces.
     */
    private fun onCarPlayListenerLost(why: String) {
        emit("!! receiver listener retired: $why")
        supervisor.onReceiverReady(false, "listener retired: $why")
    }

    /**
     * Bring CarPlay back when the user returns to the app mid-session.
     *
     * Leaving the app (to check a permission, take a call, glance at settings) DESTROYS
     * CarPlayActivity — `dumpsys activity` shows the task back at `sz=1` with only MainActivity in
     * history. Audio keeps playing because the seams and AacPlayer deliberately outlive the Surface,
     * so the session looks alive while the screen stays black. Nothing recovers on its own:
     * `onSessionUp` fires only when a NEW native core is installed, and the existing session is still
     * perfectly healthy, so it never fires again. The video seam just sits there logging
     * "connected with no Surface — holding open, discarding until one returns" forever.
     *
     * Re-launching is safe and cheap: CarPlayActivity guards its own start ("session already
     * started"), and `attachRenderer` drops any open :9001 socket so the producer re-dials and sends
     * a fresh IDR — which is exactly what a resumed screen needs.
     */
    override fun onResume() {
        super.onResume()
        // Gate on the receiver's own liveness, not just the flag. `sessionUp` is set optimistically
        // when the control connection lands; `sessionLive` is the truth about whether it is still
        // there. Checking only the flag re-launched the CarPlay screen over a dead session.
        if (sessionUp && cpRx?.sessionLive == true) {
            emit("session still live — restoring the CarPlay screen")
            launchCarPlayUi()
        } else if (sessionUp) {
            emit("stale sessionUp with no live control connection — clearing instead of restoring")
            onCarPlaySessionDown()
        }
    }

    /** Returns false when the command was dropped, so a caller holding state for it can release it. */
    private fun runAsync(block: () -> Unit): Boolean {
        try {
            cmdExecutor.execute {
                try { block() } catch (t: Throwable) { emit("!! run aborted: ${t.javaClass.simpleName}: ${t.message}") }
            }
            return true
        } catch (_: java.util.concurrent.RejectedExecutionException) {
            // onDestroy shut the executor down. Anything submitted after that comes from a callback
            // still holding this dead Activity, and dropping it is the point — but it must not throw
            // back into whichever box or receiver thread made the call.
            emit("command dropped — the Activity that owned it is gone")
            return false
        }
    }

    /** All probe output goes through ProbeLog, which owns the single NETPROBE logcat tag. */
    private fun emit(line: String) = ProbeLog.raw(line)

    /**
     * Second chance for a pump that was still inside `NativeCore.feed` when [CarPlayRx.stop] gave up.
     *
     * Bounded by the longest chip op the control path can be waiting on (`mfiSign`'s 15 s) plus a
     * little slack, because that is the actual thing being waited for. Called only from Forget
     * Pairing, which is an explicit driver action already showing "clearing the pairing…", so a few
     * seconds here is honest rather than a hang.
     */
    private fun waitForPeerStoreQuiet(): Boolean {
        emit("forget: a pump is still in the native core — waiting for it to unwind before deleting")
        val deadline = android.os.SystemClock.elapsedRealtime() + 17_000L
        while (android.os.SystemClock.elapsedRealtime() < deadline) {
            if ((cpRx?.liveConnectionCount() ?: 0) == 0) return true
            try { Thread.sleep(250) } catch (_: InterruptedException) { Thread.currentThread().interrupt(); return false }
        }
        return false
    }

    /** One binding site so toggleCarPlayRx() and rebindSessionCallbacks() cannot drift. */
    private fun bindReceiver(rx: CarPlayRx) {
        rx.onSessionUp = { launchCarPlayUi(); vehicle.onSessionUp() }
        rx.onSessionDown = { onCarPlaySessionDown() }
        rx.onStalled = { n -> onCarPlayStalled(n) }
        rx.onListenerLost = { why -> onCarPlayListenerLost(why) }
        rx.onDialAccepted = { supervisor.onDialAccepted() }
    }

    /**
     * Re-point the process-scoped session objects at THIS Activity generation.
     *
     * AAOS destroys this Activity while the process lives — the note in [onDestroy] records three
     * generations observed in one process — and the session objects deliberately survive that. Their
     * callbacks capture `supervisor` and `ui`, so without this a resurrected Activity would show an
     * idle launcher while the previous generation's dead supervisor was still being driven.
     */
    private fun rebindSessionCallbacks() {
        SessionHolder.cpRx?.let { rx ->
            bindReceiver(rx)
            // The previous generation's onDestroy disconnected its Car API; this generation's
            // watcher is a fresh lazy instance and has to be bound exactly as toggleCarPlayRx does.
            vehicle.start()
            emit("re-attached the surviving CarPlay receiver to this Activity generation")
        }
        if (SessionHolder.ocbmProbe != null) ocbm()   // ocbm() rebinds the probe's six observers
    }

    /**
     * `uiMode` is in this activity's `configChanges`, so a day/night toggle arrives here rather than
     * recreating the activity — the same precondition `carlink_native` relies on for its Compose
     * `isSystemInDarkTheme()` path. Do not remove `uiMode` from the manifest entry.
     */
    override fun onConfigurationChanged(newConfig: android.content.res.Configuration) {
        super.onConfigurationChanged(newConfig)
        vehicle.onConfigurationChanged(newConfig)
    }

    override fun onDestroy() {
        runCatching { vehicle.stop() }
        // The supervisor owns a scheduler thread and the pending timers on it; both must go with the
        // Activity or a destroyed instance keeps driving phase transitions into a dead Ui.
        supervisor.stop()
        // Same argument, one executor down: this is created per Activity and was never shut down, so
        // each generation left a live `netprobe-cmd` thread pinning its dead Activity through the
        // `emit` closures still queued on it. Three were observed alive in one process.
        cmdExecutor.shutdown()
        super.onDestroy()
    }
    // ---- capture control -------------------------------------------------------------------------

    /**
     * Export the rotated capture files as one artifact. Redacted by default — an export lands on a
     * removable stick and leaves the vehicle, and a whole-OS capture from this unit contains the
     * hotspot passphrase, the phone's BR/EDR MAC and the VIN. [LogExport] walks its own fallback
     * ladder (SAF, then a mounted USB volume, then MediaStore, then a no-op that reports the path),
     * so this does not care whether AAOS ships a document picker.
     */
    private fun exportLogs(redact: Boolean) {
        emit(if (redact) ">>> exporting logs (redacted)" else ">>> exporting logs (RAW - unredacted)")
        LogExport.export(this, this, LogExport.Options(redact = redact)) { r ->
            // emit() is ProbeLog only, so the result always reaches the log. The UI update is
            // best-effort: LogExport keys its pending callback by request code alone, so if AAOS
            // recreated this Activity while the picker was in front, the closure that runs belongs
            // to the destroyed instance. Writing to its views would be a silent no-op at best.
            r.onSuccess {
                emit("export OK via ${it.rung}: ${it.destination} (${it.bytesWritten} B, ${it.filesIncluded} files)")
                if (it.redactionCounts.isNotEmpty()) emit("redactions: ${it.redactionCounts}")
                if (uiAlive()) ui.setDetail("logs exported to ${it.destination}")
            }.onFailure {
                emit("export FAILED: ${it.message}")
                if (uiAlive()) ui.setDetail("log export failed: ${it.message}")
            }
        }
    }

    /** False once this Activity instance is gone, so a late async callback cannot write to its views. */
    private fun uiAlive(): Boolean = !isFinishing && !isDestroyed

    /** Persist the scope and restart capture so it takes effect now rather than at the next boot. */
    private fun setCaptureScope(scope: LogCapture.Scope) {
        CapturePrefs.setScope(this, scope)
        LogCapture.stop()
        val ok = LogCapture.start(this, CapturePrefs.config(this))
        // Print ONLY what is known synchronously. `effectiveScope` and `readLogsGranted` are both
        // resolved later on the pump thread, so sampling status() here read the pre-resolution
        // defaults and printed them as fact - on hardware, `effective=WHOLE_OS read_logs=false` when
        // the truth was OWN_PROCESS/true, both inverted, which then misled an audit. The authoritative
        // line and the degraded-path hints come from the pump itself.
        emit("capture scope=$scope start=$ok (effective scope + read_logs resolve on the pump thread; see [logcap])")
        ui.setDetail("capture: $scope")
    }

    @Suppress("DEPRECATION")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        if (LogExport.onActivityResult(requestCode, resultCode, data)) return
        super.onActivityResult(requestCode, resultCode, data)
    }

}

/**
 * PROCESS-scoped owner of the two objects that must outlive any single Activity.
 *
 * They used to be MainActivity instance fields. AAOS destroys the backgrounded launcher while the
 * process lives (CarPlayActivity runs on its own `taskAffinity`, so the launcher spends the whole
 * session in the background), and `onDestroy` stopped only the supervisor and the command executor
 * — so the receiver and the OCBM probe were left running and UNREACHABLE. The next `onCreate` then
 * saw two nulls and built duplicates that cannot work: `ServerSocket.bind(:7011)`
 * fails EADDRINUSE against the orphan's listener (SO_REUSEADDR does not permit a second LISTEN), so
 * the self-test reports "listening" FAIL and the supervisor declares RX_UNHEALTHY, while the ORPHAN
 * keeps serving the phone into a dead supervisor. The probe fought the orphan for the USB interface
 * claim, and a second MdnsResponder answered for the same `gmccpa-rx.local` — the double-answer
 * hazard CarPlayRx warns about, now across instances rather than within one.
 *
 * Holding them here makes "is a session already up?" a question about the PROCESS, which is what it
 * always was. The Activity re-attaches its callbacks on create (see `rebindSessionCallbacks`)
 * instead of constructing rivals. Nothing here holds an Activity context: `OcbmProbe` and
 * `CarPlayRx` both keep only `applicationContext`.
 */
object SessionHolder {
    @Volatile var cpRx: CarPlayRx? = null
    @Volatile var ocbmProbe: zeno.gmccpa.ocbm.OcbmProbe? = null
    /**
     * `elapsedRealtime` of the last `OcbmProbe.stop()` this process made, or 0. Process-scoped like the
     * probe it describes, because the settle it feeds (`MainActivity.restartSession`) is about the
     * BOX's teardown clock, which does not restart when the Activity does.
     */
    @Volatile var lastProbeStopAt = 0L
    /**
     * The Stop button was pressed and its `stopEverything` has not yet run. Read by `autoStart` and by
     * the restart's settle so a bring-up queued AHEAD of the stop yields to it instead of parking the
     * command executor in a ten-minute `awaitClaimable` the stop cannot reach (`claimAbort` is reset
     * on entry to that wait, and during the restart settle there is no probe to abort at all).
     * Consumed — cleared — by `stopEverything`, whatever the outcome of the teardown.
     */
    @Volatile var stopRequested = false
}
