package zeno.gmccpa

import android.app.Activity
import android.content.Intent
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import android.os.Bundle
import zeno.gmccpa.logging.CapturePrefs
import zeno.gmccpa.logging.LogCapture
import zeno.gmccpa.logging.LogExport

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

    /** The launcher screen. Owns the hotspot credentials (see [Ui.kt]); attached to the [core] as its view. */
    private lateinit var ui: LauncherUi

    /**
     * The PROCESS-scoped session core (v5.7 U4). Everything that drives the receiver and the probe —
     * the supervisor, the command executor, `sessionUp`, the vehicle-state watcher — lives there and
     * survives this Activity. This class is a view plus the verb dispatcher: it attaches its
     * [LauncherUi] on create and detaches it (compare-and-clear) on destroy, and nothing else.
     */
    private lateinit var core: SessionCore

    /**
     * Confirmation for the three adapter actions, none of which is walked back by pressing Start
     * again. Title, body and floors come from the pure model ([BoxAction.confirmTitle],
     * [confirmBodyFor]) and [LauncherUi.confirm] (R16 floors, no stacking). Every [BoxAction] is
     * confirmed: there is no unconfirmed branch (plan v5.9 D021).
     */
    private fun confirmBoxAction(a: BoxAction) =
        ui.confirm(a.confirmTitle, confirmBodyFor(a, SessionHolder.cpRx?.sessionLive == true), a.label) {
            core.requestBoxAction(a)
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
        // (SecurityException on getSoftApConfiguration, EACCES on hostapd.conf), so the driver types
        // it once. OD-7A (2026-10-07): there are no compiled-in values; LauncherUi starts blank unless
        // HotspotPrefs has saved ones, and a start or USB attach with none opens the dialog instead of
        // linking. --es ssid/--es pass still override for that run.
        core = SessionHolder.core(applicationContext)
        ui = LauncherUi(this).apply {
            onStart = { core.requestStart(manual = true) }
            // Stop: abortClaimWait on THIS thread first, then the stop is queued (see SessionCore.requestStop).
            onStop = { core.requestStop() }
            onRestart = { core.requestRestart() }
            // Credentials confirmed in the dialog reach the probe immediately, so a Start that
            // follows cannot use the previous values. Snapshot on main, applied on the executor (F027).
            // OD-7A: a start refused for missing credentials runs once they are saved (onCredentialsSaved).
            onCredentials = { _, _ -> val c = core.credsOf(this); core.runAsync { core.applyHotspotFields(c); core.onCredentialsSaved(c) } }
            // Restart adapter Bluetooth, reboot and forget are destructive to a live session and, unlike
            // everything else on this screen, are not undone by pressing Start again — so they ask
            // first. All three: the `when` is exhaustive with no else, so a new BoxAction must choose.
            onBoxAction = { a ->
                val confirmed: Unit = when (a) {
                    BoxAction.RESTART_BT, BoxAction.REBOOT, BoxAction.FORGET_PHONE -> confirmBoxAction(a)
                }
                confirmed
            }
            // D003: Export is the sheet's one log control; capture scope/status are the
            // capture_whole_os / capture_own / capture_status verbs only.
            onExportLogs = { exportLogs(redact = true) }
            // OD-9A: changed credentials while the box is subscribed -> offer Restart Session.
            credentialsApplyOffer = { core.credentialsNeedResubscribe() }
            onApplyCredentialsNow = { core.requestRestart() }
        }
        setContentView(ui.root)
        // BEFORE anything decides whether to start a session: the core renders the phase it already
        // holds onto this view (a recreated Activity shows the live state, not a fresh idle launcher).
        // The receiver and probe callbacks are bound to the core, never to an Activity, so there is
        // nothing to re-bind here.
        core.attach(ui)

        // The PID anchor. Replaces a banner that carried uid and package but not the PID, which is
        // the only key that attributes this process's ~250 framework lines per session (CCodec,
        // MediaCodec, BufferQueueProducer, …) in a whole-OS capture. See ProbeLog.identity.
        ProbeLog.identity(this)
        // Capture was only ever started from BootReceiver, the USB trampoline and the scope menu —
        // never from here, so a launcher start recorded nothing at all and the drive-ready process
        // had no logcap-pump thread. start() is idempotent and self-reaping, so calling it on every
        // onCreate is free when capture is already running.
        runCatching { LogCapture.start(applicationContext, CapturePrefs.config(this)) }
        // D-F2: a recreate re-delivers the original launch intent; its one-shot `run` verb must not
        // run again (IntentReplay). The default auto-start below still does.
        dispatchIntent(intent, verbAllowed = IntentReplay.verbRuns(savedInstanceState != null, intent?.flags ?: 0))
    }

    /** The intent decides the path: a USB attach or an --es run verb is explicit; anything else
     *  (launcher icon, task switch) means "just bring the session up". Shared by [onCreate] and
     *  [onNewIntent] so the two paths cannot drift. [verbAllowed] false (a recreate, D-F2) takes the
     *  default path instead of the verb, and the verb is stripped from the retained intent either
     *  way so a later recreate finds none. */
    private fun dispatchIntent(i: Intent?, verbAllowed: Boolean = true) {
        if (handleAttachIntent(i)) return
        // v5.7w1 (R3): a media-card tap brings this screen forward (onResume restores the CarPlay
        // screen while a session is live) — it must never start a bring-up or a runAll over a live
        // receiver or box link, nor undo an explicit Stop.
        // v5.7w1 fix2: but with no receiver, no box client and no Stop, nothing is up and nothing
        // else will bring it up (this tap may be the launch) — auto-start exactly as the launcher.
        if (i?.getBooleanExtra(EXTRA_FROM_MEDIA_CARD, false) == true) {
            val rxUp = SessionHolder.cpRx != null
            val boxUp = SessionHolder.ocbmProbe?.client != null
            // D6 / OD-9D: a Stop of this head-unit boot counts too, even after stopEverything cleared
            // stopRequested (a tap on the media card is not a Start).
            val stopped = SessionHolder.stopRequested || AutoClaimPrefs.suppressed(applicationContext)
            if (!rxUp && !boxUp && !stopped) {
                emit("media card tapped — no receiver, no box client, no Stop: auto-starting")
                core.requestStart(manual = false)
            } else {
                emit("media card tapped — brought forward, no auto-start (rx=$rxUp box=$boxUp stopRequested=$stopped)")
            }
            return
        }
        val verb = i?.getStringExtra("run")
        if (verb != null) {
            if (verbAllowed) handleRunExtra(i) else emit("run verb '$verb' not replayed on recreate — auto-starting instead")
            // Consumed: a recreate re-reads getIntent(), which must no longer carry the verb (or the
            // credentials that rode with it). The retained intent is a neutral launcher intent.
            i.removeExtra("run"); i.removeExtra("ssid"); i.removeExtra("pass"); i.removeExtra("mac")
            setIntent(Intent(this, MainActivity::class.java))
            if (!verbAllowed) core.requestStart(manual = false)
        } else core.requestStart(manual = false)
    }

    override fun onNewIntent(intent: Intent?) {
        super.onNewIntent(intent)
        // D-F2: getIntent() must name the delivery that is current, so a recreate replays nothing older.
        setIntent(intent)
        dispatchIntent(intent)
    }

    /**
     * Entered on adapter attach — but NOT directly from the platform anymore. [UsbAttachActivity], a
     * NoDisplay trampoline, filters to the OCBM adapter and forwards a matching
     * ACTION_USB_DEVICE_ATTACHED intent here. **Corrected 2026-10-06: on this unit the platform never
     * launches it** — GM routes attach to an absent fixed handler (`Default USB handling package
     * (android.car.usb.handler) not found`, every attach in logcat2; 0 trampoline lines) — so this
     * path is reachable only from an explicit intent. **Corrected again 2026-10-08 (plan v5.9 D6):**
     * the same attach IS broadcast to the current user before that failed lookup, and
     * [AdapterAttachReceiver] claims a (re)plugged adapter from it with the app closed — no dialog,
     * because the USB grant persists once "always open" was accepted. [BoxRelink] still re-claims
     * after a transport death while the process lives. This activity route passes through the same
     * gate (SessionCore.onAdapterAttached) with its foreground semantics. The rest of this note is
     * the design as written. Permission comes from the ordinary attach
     * resolver: the framework shows the standard USB dialog once per device, "always open" caches
     * it, and the grant to our UID lands before the trampoline is launched — so by the time this
     * runs `hasPermission(dev)` is normally true. Normally, not always: on a cold start the grant
     * can commit after we sample it (see UsbAttachActivity's "process age" note), which is why
     * UsbBulkTransport polls hasPermission() instead of trusting this. (Until 2026-09-08 the
     * app installed as `android.car.usb.handler`, the GM USB fixed-handler squat — see the manifest
     * header + `ccpa_custom/docs/host/01_ANDROID_AND_AAOS.md` §"GM AAOS USB permission handler". That
     * squat was SUPPOSED to make the framework grant our UID USB permission silently on every attach;
     * corrected 2026-09-10, it did not — the app still needed the user to grant adapter permission,
     * which is why it was reverted. The squat was reverted; the install package was `zeno.gmccpa`
     * 2026-09-08..10-15. Since 2026-10-15 it is `android.car.usb.handler` again, owner request, and
     * the fixed handler starts this trampoline through the manifest's activity-alias.)
     * In a wireless-only design this attach is also the only physical trigger there is. Device-proven
     * 2026-08-17. The bring-up itself (credentials, receiver first, then the link; no CT_SUBSCRIBE
     * under a live session) is [SessionCore.onAdapterAttached], on the command executor.
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
        core.onAdapterAttached()
        return true
    }

    /**
     * Scriptable entry point, so bring-up can be driven from a shell instead of by tapping:
     *   adb shell am start -n android.car.usb.handler/zeno.gmccpa.MainActivity --es run ocbm_selftest
     *   (the am component is <install-pkg>/<class>. Install package and class package DIFFER again
     *    since 2026-10-15 (android.car.usb.handler vs zeno.gmccpa, owner request), so the short
     *    `/.MainActivity` form does not work; this example said zeno.gmccpa/… 2026-09-09..10-15.
     *    Never re-hard-code it in a
     *    printed string — see LogCapture.grantCmd, which derives it from ctx.packageName.
     *    See the manifest header.)
     * Accepts: ocbm_selftest | ocbm_link | ocbm_state | ocbm_disconnect | ocbm_forget | ocbm_stop
     *          | carplay_rx | carplay_stop | carplay_ui | full | mdns_self
     *          | export_log | export_log_raw | capture_status | capture_whole_os | capture_own
     *          | mic_inject — BENCH builds only (GMCCPA_BENCH=1; see zeno.gmccpa.bench.BenchHooks). This
     *            activity is exported, so in a bench build any installed app can send it while
     *            `debug.gmccpa.mic_inject` is 1; a release build compiles a stub that only logs.
     *          | finish_launcher — BENCH builds only (plan v5.9 D6): finish() this activity, for the
     *            headless auto-claim checks; a release build only logs.
     */
    private fun handleRunExtra(intent: Intent?) {
        val i = intent ?: return
        val what = i.getStringExtra("run") ?: return
        // Hotspot credentials for the 0x5703 handoff, from the shell or the on-screen field.
        // v5.7 U4 (F027): the screen update stays on main (setCredentials marshals there, and runs
        // inline here), the values are snapshotted on main AFTER it, and the probe write (which may
        // create the probe) runs on the command executor — never a get-or-create on main. The
        // snapshot is what makes the order safe: an executor read of the view could otherwise race
        // the main-thread write.
        // No `--es chan`: the app sends no wifi_channel (owner, 2026-10-07; D015 removed the read).
        ui.setCredentials(i.getStringExtra("ssid"), i.getStringExtra("pass"))
        val creds = core.credsOf(ui)
        core.runAsync { core.applyHotspotFields(creds) }
        emit("")
        emit(">>> scripted run: $what")
        when (what) {
            "export_log" -> exportLogs(redact = true)
            // Deliberately a separate verb, never a flag on the redacted one: an unredacted export
            // carries the vehicle hotspot passphrase and the phone's BR/EDR MAC off the vehicle on a
            // removable stick, so it has to be asked for in full by someone who meant it.
            "export_log_raw" -> exportLogs(redact = false)
            "capture_status" -> emit(LogCapture.status().toString())
            "capture_whole_os" -> core.setCaptureScope(LogCapture.Scope.WHOLE_OS)
            "capture_own" -> core.setCaptureScope(LogCapture.Scope.OWN_PROCESS)
            "ocbm_selftest" -> core.verbOcbmSelftest()
            // Same policy as autoStart: runAll() begins with stopLocked() on the live client and then
            // re-SUBSCRIBEs, which cycles the box's radios under a streaming session (04:20:46,
            // 2026-10-06). Bench verbs get the refusal, not the exception.
            "ocbm_link" -> core.verbOcbmLink()
            "carplay_rx" -> core.verbCarplayRx()
            // Both planes at once: the OCBM/BT link that drives the handoff, and the Wi-Fi endpoint
            // the phone is supposed to find afterwards. This is the real end-to-end sequence.
            "full" -> core.verbFull()
            // The real UI: fullscreen HEVC + AAC + touch. Screen only, and only over a live session.
            "carplay_ui" -> core.verbCarplayUi()
            "mdns_self" -> core.runAsync { MdnsInspect.inspect(CarPlayRx.ACCESSORY_NAME) }
            // The receiver half of restartSession. sessionUp is cleared HERE, on the main thread,
            // before queueing: `am start` delivers this (onNewIntent) and then onResume on the same
            // thread, and onResume restoring a session this verb is about to stop relaunched the
            // CarPlay screen over it.
            "carplay_stop" -> core.verbCarplayStop()
            "ocbm_state" -> core.verbOcbmState()
            // Session teardown. disconnect = drop BT, keep the bond (test hygiene between runs).
            // forget = clear the bond entirely; also forget the car on the iPhone.
            "ocbm_disconnect" -> core.verbOcbmDisconnect()
            "ocbm_forget" -> core.verbOcbmForget(i.getStringExtra("mac"))
            "ocbm_stop" -> core.verbOcbmStop()
            "mic_inject" -> zeno.gmccpa.bench.BenchHooks.micInject(applicationContext, i)
            // plan v5.9 D6 (B-D6-5): bench builds finish this activity (after the creds above).
            "finish_launcher" -> zeno.gmccpa.bench.BenchHooks.finishLauncher(this)
            else -> emit("unknown run extra '$what'")
        }
    }

    /**
     * Bring CarPlay back when the user returns to the app mid-session.
     *
     * Leaving the app (to check a permission, take a call, glance at settings) DESTROYS
     * CarPlayActivity — `dumpsys activity` shows the task back at `sz=1` with only MainActivity in
     * history. Audio keeps playing because the seams and AacPlayer deliberately outlive the Surface,
     * so the session looks alive while the screen stays black. Nothing recovers on its own:
     * `onSessionUp` fires only when a NEW native core is installed. Re-launching is safe and cheap:
     * CarPlayActivity guards its own start. A stop delivered by the same intent (onNewIntent ->
     * handleRunExtra) clears the core's sessionUp on this thread before we get here.
     */
    override fun onResume() {
        super.onResume()
        AppForeground.resumed()
        core.onViewResumed()
    }

    override fun onPause() {
        AppForeground.paused()
        super.onPause()
    }

    /** All probe output goes through ProbeLog, which owns the single NETPROBE logcat tag. */
    private fun emit(line: String) = ProbeLog.raw(line)

    companion object {
        /** Set by CarPlayMediaBrowserService's session-activity PendingIntent (the media-card tap). */
        const val EXTRA_FROM_MEDIA_CARD = "from_media_card"

        /**
         * The relink's dialog policy for the CURRENT state, evaluated on every request interval
         * (52B). Gear comes from the process-scoped core's watcher (v5.7 U4; it used to need a live
         * MainActivity generation); no watcher = gear unknown = not parked. See [UsbRequestPolicy].
         */
        internal fun mayRequestUsb(requestsThisInstance: Int): Boolean {
            val live = SessionHolder.cpRx?.sessionLive == true
            val parked = if (!live) null else SessionHolder.core?.vehicleParked()
            val why = UsbRequestPolicy.deferredWhy(live, parked, requestsThisInstance) ?: return true
            lastUsbDeferral = why
            return false
        }

        /**
         * The reason for the last refusal by [mayRequestUsb], from the SAME gear/session read as the
         * refusal (a second read could see the gear change in between). The relink's `deferredWhy`.
         */
        @Volatile private var lastUsbDeferral = UsbRequestPolicy.DEFERRED_WHY
        internal fun usbDeferredWhy(): String = lastUsbDeferral
    }

    /**
     * `uiMode` is in this activity's `configChanges`, so a day/night toggle arrives here rather than
     * recreating the activity — the same precondition `carlink_native` relies on for its Compose
     * `isSystemInDarkTheme()` path. Do not remove `uiMode` from the manifest entry.
     */
    override fun onConfigurationChanged(newConfig: android.content.res.Configuration) {
        super.onConfigurationChanged(newConfig)
        core.onConfigurationChanged(newConfig)
    }

    /**
     * Detach this view and nothing else (v5.7 U4). The supervisor, the command executor and the
     * vehicle-state watcher belong to the process-scoped core and outlive this Activity: stopping
     * them here is what left a session that ended with no MainActivity without grace, ladder or
     * limitedUI (F021/F022). Compare-and-clear: a recreated instance's onCreate may already have
     * attached its own view.
     */
    override fun onDestroy() {
        if (::core.isInitialized && ::ui.isInitialized) { core.detach(ui); ui.release() }
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
            // routed through the process-scoped core (plan v5.9 C6): LogExport keys its pending
            // callback by request code alone, so if AAOS recreated this Activity while the picker was
            // in front, the closure that runs belongs to the destroyed instance — the core renders
            // the result on whichever view is attached now.
            r.onSuccess {
                emit("export OK via ${it.rung}: ${it.destination} (${it.bytesWritten} B, ${it.filesIncluded} files)")
                if (it.redactionCounts.isNotEmpty()) emit("redactions: ${it.redactionCounts}")
                // C6 "T" through the core, so a re-attached view replays it while it is young.
                core.showResult("Logs exported", "logs exported to ${it.destination}")
            }.onFailure {
                emit("export FAILED: ${it.message}")
                core.showResult("Log export failed", "log export failed: ${it.message}")
            }
        }
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
 * always was. Since v5.7 U4 the code that drives them is process-scoped too: [SessionCore], built
 * once from applicationContext and held here, binds their callbacks once; an Activity only attaches
 * its view to it. Nothing here holds an Activity context: `OcbmProbe`, `CarPlayRx` and the core
 * keep only `applicationContext`.
 */
object SessionHolder {
    @Volatile var cpRx: CarPlayRx? = null
    /**
     * The receiver a deliberate stop is retiring, raised BEFORE that stop posts `supervisor.onStopped()`
     * and cleared once `cpRx` no longer points at it. `cpRx` is nulled only AFTER `stop()`, so identity
     * alone left a gap in which that receiver's session-up could still pass as current. See
     * `SessionCore.isCurrentReceiver`.
     */
    @Volatile var retiringRx: CarPlayRx? = null
    /** Written only by [SessionCore] (get-or-create and null-outs under its probe lock, v5.7 F027). */
    @Volatile var ocbmProbe: zeno.gmccpa.ocbm.OcbmProbe? = null
    /**
     * The process-scoped session core (v5.7 U4), or null until the first MainActivity builds it. It
     * replaces the old weak `activity` reference: BoxRelink reports to the core, which is alive
     * whether or not any MainActivity is ([C2]).
     */
    @Volatile var core: SessionCore? = null
        private set

    /** The one core, built on first use from the application context. */
    fun core(ctx: android.content.Context): SessionCore =
        core ?: synchronized(this) { core ?: SessionCore(ctx.applicationContext).also { core = it } }
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

/**
 * How many gm_ccpa activities are resumed (MainActivity, CarPlayActivity). Main thread writes, any
 * thread reads. Used to know whether a USB permission request was sent from the background, where
 * the dialog may never have been shown (unknown on this unit) — see `OcbmProbe.rerequestNow`.
 */
object AppForeground {
    private val count = java.util.concurrent.atomic.AtomicInteger(0)
    fun resumed() { count.incrementAndGet() }
    fun paused() { if (count.decrementAndGet() < 0) count.set(0) }
    fun any(): Boolean = count.get() > 0
}

/**
 * Re-claim the box after its link dies, including under a live CarPlay session (2026-10-06, 52B;
 * v5.6 H1 adds the heartbeat-declared death and the bounded HELLO retry).
 *
 * Within a live process this is the re-claim (the attach ACTIVITY route never runs: GM routes attach
 * to an absent fixed handler; under a live session autoStart refuses). Since plan v5.9 D6 the attach
 * BROADCAST also reaches AdapterAttachReceiver, whose auto-claim YIELDs to a relink that owns the
 * claim and claims by itself when none does (after the cap, or in a fresh process). What the relink
 * uses is OcbmProbe's claim loop
 * polling the bus — logcat_cp4 15:44:28.900 home-card launch -> autoStart -> runAll ->
 * `waiting for the OCBM accessory` .040 -> UsbHostManager adds the device 15:45:02.261 ->
 * `adapter present; waiting for USB permission` 15:45:03.114 -> the dialog, tapped -> `permission
 * held` 15:45:05.188 -> `CT_HELLO_ACK` .220. It was started by the launch, not by the attach. So
 * this arms that same loop on TRANSPORT death (`OcbmProbe.onLinkDead`): the read loop ending on its
 * own (ENODEV), or — since v5.6 — the heartbeat declaring the link dead after 5 failed OUT writes
 * with the adapter still enumerated (the half-dead link of logcat_cp4 18:42:52-18:44:19; a
 * same-enumeration reopen needs no new USB prompt, awaitClaimable polls hasPermission). A box-side
 * HOST_GONE with writes still succeeding does not arm it: while the heartbeat lives, its own
 * re-SUBSCRIBE recovers that, and a runAll there would add a CT_STOP + SUBSCRIBE presence cycle.
 * When the heartbeat's writes fail too, it now declares the link dead and that arms this.
 *
 * The token/`again` single-flight, the dead-run streak, its cap and the backoff live in
 * [RelinkChain] (pure Kotlin, unit-tested on the JVM); this object binds it to the probe, the
 * `ocbm-relink` executor and the process-scoped [SessionCore]. A claim failure, an unplugged
 * adapter after a failed HELLO, and a no-session claim-wait expiry still end the chain with a report.
 *
 * Runs on its own process-scoped `ocbm-relink` thread, never on a generation's command executor:
 * the wait can last ten minutes. OcbmProbe verbs that enter `ops` while it waits (U-K) queue behind
 * it and run late against the NEW link: runAll (autoStart, Start/Restart, the ocbm_link/full verbs
 * and the attach intent) — the link epoch leaves a link that came up meanwhile alone; setRadios
 * (rung 1 and the session radio hold, both of which need a subscribed link, so not dispatched with
 * none; a late release ON reaches a fresh link that the subscribe or onBoxRelinked re-asserts
 * anyway); disconnectPhone (rung 2 needs a subscribed link; the operator verb runs as asked);
 * forgetPhone (Forget Pairing runs as asked on the new link); sessionState (read-only);
 * subscribeExisting (needs a link, so it is not dispatched while one is being waited for). The
 * command thread that dispatched such a verb is blocked until it runs; Stop ends the wait. A
 * backoff sleep (5-30 s) holds the token too, so autoStart refuses during it as during the wait.
 */
internal object BoxRelink {
    private val log = ProbeLog.sub("relink")
    private val exec by lazy {
        java.util.concurrent.Executors.newSingleThreadExecutor { r -> Thread(r, "ocbm-relink").apply { isDaemon = true } }
    }

    private val chain = RelinkChain(
        env = object : RelinkChain.Env<zeno.gmccpa.ocbm.OcbmProbe, zeno.gmccpa.ocbm.OcbmProbe.LinkResult> {
            override fun probe() = SessionHolder.ocbmProbe
            override fun stopped(p: zeno.gmccpa.ocbm.OcbmProbe) = p.stopped
            override fun hasLink(p: zeno.gmccpa.ocbm.OcbmProbe) = p.hasLink()
            override fun waitExpired(p: zeno.gmccpa.ocbm.OcbmProbe) = p.waitExpired
            override fun adapterEnumerated(p: zeno.gmccpa.ocbm.OcbmProbe) = p.adapterEnumerated()
            override fun stopRequested() = SessionHolder.stopRequested
            override fun sessionLive() = SessionHolder.cpRx?.sessionLive == true
            override fun runAll(p: zeno.gmccpa.ocbm.OcbmProbe, cancelled: () -> Boolean) = p.runAll(
                // Evaluated AFTER the claim, HELLO and MFi: a session that ended during a long wait
                // gets the subscribe; a live one keeps the radios down (no radio-wake edge mid-session).
                subscribe = { SessionHolder.cpRx?.sessionLive != true },
                mayRequest = { n -> MainActivity.mayRequestUsb(n) },
                deferredWhy = { MainActivity.usbDeferredWhy() },
                quietExpiry = { true },
                cancelled = cancelled,
            )
            override fun outcome(r: zeno.gmccpa.ocbm.OcbmProbe.LinkResult) =
                RelinkChain.Outcome(claimed = r.claimed, helloOk = r.helloOk, helloAtMs = r.helloAtMs)
            // To the process-scoped core, alive with or without a MainActivity (v5.7 U4, [C2]).
            override fun report(r: zeno.gmccpa.ocbm.OcbmProbe.LinkResult, expired: Boolean) {
                val core = SessionHolder.core
                if (core == null) {
                    log.i("relink done (hello=${r.helloOk}, subscribed=${r.subscribed}) — no session core to tell")
                    return
                }
                core.onRelinkDone(r, expired)
            }
            override fun capped(detail: String) {
                val core = SessionHolder.core
                if (core != null) core.onRelinkCapped(detail)
                else log.i("relink cap — no session core to tell")
            }
            // Q018: a bring-up that threw is reported as a failed run carrying the exception text.
            override fun failed(detail: String) = zeno.gmccpa.ocbm.OcbmProbe.LinkResult(error = detail)
        },
        // Lazy: the thread is created on the first arm, as before.
        exec = java.util.concurrent.Executor { r -> exec.execute(r) },
        log = object : RelinkChain.Log {
            override fun i(msg: String) = log.i(msg)
            override fun w(msg: String) = log.w(msg)
            override fun e(msg: String) = log.e(msg)
        },
    )

    /** Non-null while a relink owns the claim (a wait, a backoff sleep or a bring-up). */
    val owner: java.util.concurrent.atomic.AtomicReference<Any?> get() = chain.owner

    /**
     * [heartbeatWhy] is the reason the heartbeat declared the link dead (OcbmProbe.onLinkDead's
     * second argument, from UsbBulkTransport.deadReason), or null when the read loop ended on its
     * own. It decides whether this arm counts toward the dead-run cap (v5.6 H1-4, review B1).
     */
    fun arm(why: String, heartbeatWhy: String? = null) =
        chain.arm(if (heartbeatWhy != null) "$why: $heartbeatWhy" else why,
            if (heartbeatWhy != null) RelinkChain.Cause.HEARTBEAT_DIED else RelinkChain.Cause.TRANSPORT_DIED)

    /** Stop / Restart: drop the token so the unwinding task's finally fails its CAS and does nothing. */
    fun cancel(why: String) = chain.cancel(why)
}
