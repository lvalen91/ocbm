package zeno.gmccpa

import com.carlink.ocbm.Ocbm
import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import android.os.Build
import android.os.Bundle
import android.os.Process
import zeno.gmccpa.logging.CapturePrefs
import zeno.gmccpa.logging.LogCapture
import zeno.gmccpa.logging.LogExport
import zeno.gmccpa.logging.SessionSummary
import java.util.concurrent.Executors

/**
 * GM CCPA — the instrument shell for the wireless CarPlay receiver.
 *
 * Started life as a non-privileged capability prober named NetProbe (`com.carlink.netprobe`), renamed
 * 2026-08-14; the logcat tag stays `NETPROBE` because `native/carplay-jni/src/lib.rs` writes it from
 * Rust and `tools/tri_capture.sh` greps for it. The feasibility probes it was built around have
 * served their purpose and were retired 2026-08-12 (then docs/11 task T6.2, "Trim MainActivity feasibility probes"; the row was dropped from the ledger in the 2026-08-31 rewrite). What remains is the launcher UI, the `--es run`
 * command dispatcher, and the permanent diagnostic verbs: `display`, `dump_setup`, `mdns_self`,
 * `av_stats`, `ocbm_state`.
 *
 * Export goes through the Storage Access Framework (ACTION_CREATE_DOCUMENT) -> built-in file manager
 * -> USB, because the app's external files dir is NOT reachable by adb or other apps on this unit.
 */
class MainActivity : Activity() {

    /** The launcher screen. Owns the state readout and the hotspot credentials (see [Ui.kt]). */
    private lateinit var ui: LauncherUi

    /**
     * CarPlay-identity receiver probe (Car bit set + the _carplay-ctrl connect-out).
     *
     * Backed by [SessionHolder], NOT by an instance field — see that object for why. The accessor
     * keeps every existing call site unchanged while the object itself now outlives this Activity.
     */
    /**
     * Vehicle-state levers (drive-restricted UI, day/night). Created eagerly so the Car API is bound
     * before the first session rather than on the first gear change — `SENSOR_RATE_ONCHANGE` would
     * otherwise leave us with no gear opinion until the driver happens to move the lever.
     */
    private val vehicle by lazy { zeno.gmccpa.av.VehicleStateWatcher(this) }
    private var cpRx: CarPlayRx?
        get() = SessionHolder.cpRx
        set(v) { SessionHolder.cpRx = v }
    /** Set once the phone has a live session, so [onResume] can restore the CarPlay screen. */
    @Volatile private var sessionUp = false

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
        RxPhase.IDLE -> LinkState.STOPPED
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
            val rx = CarPlayRx(
                this,
                mfiRelay = ocbmProbe?.mfiRelay(),
                // Event-driven standby: the control connection is the signal to get the decoders and
                // seams listening, so they are ready before the first stream SETUP rather than
                // whenever an operator gets round to running carplay_ui.
                onSessionUp = { launchCarPlayUi(); vehicle.onSessionUp() },
                // Nothing observed session END before this. See onCarPlaySessionDown.
                onSessionDown = { onCarPlaySessionDown() },
                onStalled = { n -> onCarPlayStalled(n) },
                onDialAccepted = { supervisor.onDialAccepted() },
                peerFile = java.io.File(filesDir, "carplay_peers.bin").absolutePath
            )
            // AvSink is NOT started here. It binds the same :9001/:9002 the real decoders need, so
            // starting it on every receiver path made the collision structural — CarPlayActivity only
            // survived because `carplay_ui` stops it one statement before launching, and bindOrNull's
            // retry papers over the race. Lose that race and the Activity finishes: black screen with a
            // live session. It remains available as an explicit opt-in via `--es run av_sink`.
            cpRx = rx; rx.start()
            // Bind the Car API alongside the receiver. Safe if android.car is absent — the watcher
            // degrades to night-mode-only and says so once.
            vehicle.start()
            reportReceiverHealth(rx)
        } else { cur.stop(); cpRx = null }
    }

    /**
     * Consumer for the receiver's localhost A/V seam. Must be listening BEFORE the streams are set
     * up — `forward.rs` dials out per access unit and drops the AU if nobody answers.
     */
    private var avSink: AvSink?
        get() = SessionHolder.avSink
        set(v) { SessionHolder.avSink = v }

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
            // The box narrates the phone's side of the session over CH_CTRL. All five observers live
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
            // session_supervisor.sh does wireless_down, waits ~4 s, and calls wireless_up — and
            // wireless_up is what runs apply_host_wifi_creds. So the ACK below means "request
            // accepted", NOT "wireless is back"; the radios are genuinely down for a few seconds and
            // any live session dies with them.
            BoxAction.RESTART_WIFI -> {
                ui.setDetail("asking the adapter to bounce its wireless stack…")
                val st = c.mgmtAction(Ocbm.MGMT_RESTART_WIRELESS)
                if (st == 0) ui.setDetail("wireless restarting — hotspot credentials re-applied in ~5 s")
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
                    "seconds and a live session drops. This is also the only way to re-apply " +
                    "changed hotspot credentials without unplugging."
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

    companion object {
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
            onStop = { runAsync { stopEverything() } }
            onRecover = { runAsync { userRecover() } }
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
        // BEFORE anything decides whether to start a session: the receiver, the OCBM probe and the
        // A/V sink are process-scoped and may already be running from a previous Activity generation
        // (see SessionHolder). Re-attach them to THIS generation, or their callbacks keep driving a
        // dead supervisor and a dead Ui while this screen shows an idle launcher.
        rebindSessionCallbacks()

        ProbeLog.banner("GM CCPA v4  uid=${Process.myUid()}  pkg=$packageName  (adb logcat -s ${ProbeLog.TAG})")
        // Capture was only ever started from BootReceiver, the USB trampoline and the scope menu —
        // never from here, so a launcher start recorded nothing at all and the drive-ready process
        // had no logcap-pump thread. start() is idempotent and self-reaping, so calling it on every
        // onCreate is free when capture is already running.
        runCatching { LogCapture.start(applicationContext, CapturePrefs.config(this, "4.0")) }
        // The intent that launched us decides the path: a USB attach or an --es run verb is explicit,
        // anything else (launcher icon, task switch) means "just bring the session up".
        if (!handleAttachIntent(intent)) {
            if (intent?.getStringExtra("run") != null) handleRunExtra(intent)
            else runAsync { autoStart(manual = false) }
        }
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
     */
    private fun autoStart(manual: Boolean) {
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
            return
        }
        if (ocbmProbe?.client?.subscribed == true) {
            setStatus(LinkState.WAITING, "link already up — waiting for the phone")
            if (manual) emit("already subscribed; nothing to do")
            return
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
                return
            }
            // The supervisor writes the state word from here on. A synchronous setStatus() next to an
            // async supervisor event raced it, and losing that race visibly reverted the status line.
            supervisor.onBoxLinked(r.mfiProven)
        } catch (t: Throwable) {
            setStatus(LinkState.FAILED, t.message ?: t.javaClass.simpleName)
            throw t
        }
    }

    private fun stopEverything() {
        sessionUp = false   // else onResume would relaunch the CarPlay screen after an explicit Stop
        cpRx?.stop(); cpRx = null
        avSink?.stop(); avSink = null
        ocbmProbe?.let { it.stop(); emit(it.stats()) }; ocbmProbe = null
        supervisor.onStopped()
        setStatus(LinkState.STOPPED, "idle — press Start or plug the adapter in")
    }

    override fun onNewIntent(intent: Intent?) {
        super.onNewIntent(intent)
        if (!handleAttachIntent(intent)) {
            if (intent?.getStringExtra("run") != null) handleRunExtra(intent)
            else runAsync { autoStart(manual = false) }
        }
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
     * header + `ccpa_custom/docs/host/01_ANDROID_AND_AAOS.md` §"GM AAOS USB permission handler" — and under that squat the framework granted our UID USB permission SILENTLY
     * on every attach. The squat was reverted; the install package is `zeno.gmccpa` again.)
     * In a wireless-only design this attach is also the only physical trigger there is. Device-proven
     * 2026-08-17.
     */
    private fun handleAttachIntent(i: Intent?): Boolean {
        if (i?.action != UsbManager.ACTION_USB_DEVICE_ATTACHED) return false
        @Suppress("DEPRECATION")
        val dev = i.getParcelableExtra<UsbDevice>(UsbManager.EXTRA_DEVICE) ?: return false
        ProbeLog.banner("USB ATTACH 0x%04x:0x%04x — permission implicitly granted on this path"
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
     *          | carplay_rx | carplay_stop | carplay_ui | full | av_sink | av_stats
     *          | display | dump_setup | mdns_self
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
            // The real UI: fullscreen HEVC + AAC + touch. Starts its OWN seam consumers, so the
            // diagnostic AvSink must not also be running or they contend for :9001/:9002.
            "carplay_ui" -> runAsync { launchCarPlayUi() }
            "av_sink" -> runAsync { if (avSink == null) avSink = AvSink(filesDir).also { it.start() } else emit("sink already up") }
            "av_stats" -> runAsync { emit(avSink?.stats() ?: "no A/V sink running") }
            "display" -> runAsync { displayProbe() }
            "dump_setup" -> runAsync { dumpSetupRequests() }
            "mdns_self" -> runAsync { MdnsInspect.inspect(CarPlayRx.ACCESSORY_NAME) }
            "carplay_stop" -> runAsync { cpRx?.stop(); cpRx = null; avSink?.stop(); avSink = null }
            "ocbm_state" -> runAsync { ocbmProbe?.sessionState() ?: emit("no OCBM link") }
            // Session teardown. disconnect = drop BT, keep the bond (test hygiene between runs).
            // forget = clear the bond entirely; also forget the car on the iPhone.
            "ocbm_disconnect" -> runAsync { ocbmProbe?.disconnectPhone() ?: emit("no OCBM link") }
            "ocbm_forget" -> runAsync { ocbmProbe?.forgetPhone(i.getStringExtra("mac")) ?: emit("no OCBM link") }
            "ocbm_stop" -> runAsync { ocbmProbe?.let { it.stop(); emit(it.stats()) }; ocbmProbe = null }
            else -> emit("unknown run extra '$what'")
        }
    }

    // Single-thread executor: a fresh Thread per command let two toggles delivered close together
    // both observe a null cpRx/avSink and each start one, double-binding :9001/:9002 — the
    // session-fatal seam collision. Serializing commands makes the check-and-start atomic w.r.t. other
    // commands.
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
        avSink?.stop(); avSink = null   // it binds the same seams; the two cannot coexist
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
        // TAKE THE SCREEN DOWN WITH THE SESSION. This used to stop at the three lines below, none of
        // which the CarPlay screen can see — so a phone that went out of Wi-Fi range left its last
        // decoded frame frozen on the display over a dead session, swallowing touches, and the
        // returning phone could not rebuild it because startSession() guards on "already started".
        // Device-reported 2026-08-28. See CarPlayActivity.onSessionEnded.
        zeno.gmccpa.av.CarPlayActivity.onSessionEnded("control connection went away")
        // The card must go idle with the session. This IS an inference we are entitled to make: the
        // phone never sends a final "stopped" record, it simply stops sending.
        zeno.gmccpa.av.CarPlayMediaBrowserService.onSessionDown()
        // Both vehicle levers are session-scoped — the receiver refuses them with no event channel —
        // so drop the sent-state here and let onSessionUp push them fresh.
        vehicle.onSessionDown()
        supervisor.onSessionDown()   // owns the state word and the grace window
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

    private fun runAsync(block: () -> Unit) {
        try {
            cmdExecutor.execute {
                try { block() } catch (t: Throwable) { emit("!! run aborted: ${t.javaClass.simpleName}: ${t.message}") }
            }
        } catch (_: java.util.concurrent.RejectedExecutionException) {
            // onDestroy shut the executor down. Anything submitted after that comes from a callback
            // still holding this dead Activity, and dropping it is the point — but it must not throw
            // back into whichever box or receiver thread made the call.
            emit("command dropped — the Activity that owned it is gone")
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
            if (cpRx?.liveConnectionCount() ?: 0 == 0) return true
            try { Thread.sleep(250) } catch (_: InterruptedException) { Thread.currentThread().interrupt(); return false }
        }
        return false
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
            rx.onSessionUp = { launchCarPlayUi() }
            rx.onSessionDown = { onCarPlaySessionDown() }
            rx.onStalled = { n -> onCarPlayStalled(n) }
            rx.onDialAccepted = { supervisor.onDialAccepted() }
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
    private inline fun section(title: String, body: () -> Unit) {
        emit(""); emit("==================== $title ====================")
        try { body() } catch (t: Throwable) { emit("!! section failed: ${t.javaClass.simpleName}: ${t.message}") }
    }

    private fun displayProbe() {
        section("DISPLAY GEOMETRY (drives /info)") {
            val dm = resources.displayMetrics
            emit("resources.displayMetrics: ${dm.widthPixels}x${dm.heightPixels} density=${dm.density} dpi=${dm.xdpi}x${dm.ydpi}")
            if (Build.VERSION.SDK_INT >= 30) {
                val wm = windowManager
                val maxB = wm.maximumWindowMetrics.bounds
                val curB = wm.currentWindowMetrics.bounds
                emit("maximumWindowMetrics (the panel): ${maxB.width()}x${maxB.height()}")
                emit("currentWindowMetrics  (we get)  : ${curB.width()}x${curB.height()}")
                val ins = wm.currentWindowMetrics.windowInsets
                val bars = ins.getInsets(android.view.WindowInsets.Type.systemBars())
                emit("system bar insets: left=${bars.left} top=${bars.top} right=${bars.right} bottom=${bars.bottom}")
            }
            val d = windowManager.defaultDisplay
            @Suppress("DEPRECATION") emit("refreshRate=${d.refreshRate}")

            // Ask for true immersive and re-measure — this is the 2400x960-vs-1416x960 question.
            runOnUiThread {
                @Suppress("DEPRECATION")
                window.decorView.systemUiVisibility =
                    android.view.View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY or
                    android.view.View.SYSTEM_UI_FLAG_FULLSCREEN or
                    android.view.View.SYSTEM_UI_FLAG_HIDE_NAVIGATION or
                    android.view.View.SYSTEM_UI_FLAG_LAYOUT_STABLE or
                    android.view.View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN or
                    android.view.View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
            }
            Thread.sleep(1500)
            if (Build.VERSION.SDK_INT >= 30) {
                val after = windowManager.currentWindowMetrics
                val b = after.bounds
                val ins = after.windowInsets.getInsets(android.view.WindowInsets.Type.systemBars())
                emit("AFTER immersive: bounds=${b.width()}x${b.height()} insets l=${ins.left} t=${ins.top} r=${ins.right} b=${ins.bottom}")
                // The window was already full-panel; the question was only ever whether the bars
                // still OVERLAY it. Insets are the answer, not bounds.
                if (ins.left == 0 && ins.top == 0 && ins.right == 0 && ins.bottom == 0) {
                    emit("=> TRUE fullscreen: advertise ${b.width()}x${b.height()} with NO safeArea")
                } else {
                    val sx = ins.left; val sy = ins.top
                    val sw = b.width() - ins.left - ins.right
                    val sh = b.height() - ins.top - ins.bottom
                    emit("=> GM chrome still overlays the panel.")
                    emit("   Advertise ${b.width()}x${b.height()} WITH safeArea origin=($sx,$sy) size=${sw}x${sh}")
                    emit("   (that is what safeArea exists for: iOS keeps interactive UI inside it,")
                    emit("    wallpaper may still bleed to the full rect. Needs the viewAreas lever on.)")
                }
            }
        }
    }

    /**
     * Print the feature tokens the iPhone PROPOSES in its SETUP request.
     *
     * `enabledFeatures` in our response must be a SUBSET of this (05_SESSION_FLOW §5 E2). The
     * receiver asserts its lever set instead of intersecting, so echoing something the phone did not
     * propose is an InvalidParameter — which is what
     * `carEndpoint_validateEnabledFeaturesWithAccessory` rejected us with. The binary plist stores
     * these as plain ASCII, so scanning for printable runs is enough to read them.
     */
    private fun dumpSetupRequests() {
        section("SETUP REQUEST (what the phone proposes)") {
            val files = filesDir.listFiles { f -> f.name.startsWith("setup_req") }?.sortedBy { it.name }
            if (files.isNullOrEmpty()) { emit("no setup_req dumps — run a session first"); return@section }
            for (f in files) {
                val b = f.readBytes()
                emit("${f.name}: ${b.size} bytes")
                val toks = StringBuilder()
                val out = ArrayList<String>()
                for (byte in b) {
                    val c = byte.toInt().toChar()
                    if (c.isLetterOrDigit() || c == '.' || c == '_') toks.append(c)
                    else { if (toks.length >= 4) out.add(toks.toString()); toks.setLength(0) }
                }
                if (toks.length >= 4) out.add(toks.toString())
                emit("  tokens: ${out.joinToString(", ")}")
            }
        }
    }


    // ---- helpers ----------------------------------------------------------------------------------

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
        val ok = LogCapture.start(this, CapturePrefs.config(this, "4.0"))
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
 * PROCESS-scoped owner of the three objects that must outlive any single Activity.
 *
 * They used to be MainActivity instance fields. AAOS destroys the backgrounded launcher while the
 * process lives (CarPlayActivity runs on its own `taskAffinity`, so the launcher spends the whole
 * session in the background), and `onDestroy` stopped only the supervisor and the command executor
 * — so the receiver, the OCBM probe and the A/V sink were left running and UNREACHABLE. The next
 * `onCreate` then saw three nulls and built duplicates that cannot work: `ServerSocket.bind(:7011)`
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
    @Volatile var avSink: AvSink? = null
    @Volatile var ocbmProbe: zeno.gmccpa.ocbm.OcbmProbe? = null
}
