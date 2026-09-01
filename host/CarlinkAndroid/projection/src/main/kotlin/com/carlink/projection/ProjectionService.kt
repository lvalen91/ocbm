package com.carlink.projection

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Binder
import android.os.IBinder
import android.view.Surface
import com.carlink.logging.ProbeLog
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.launchIn
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.onEach
import kotlinx.coroutines.launch

/**
 * The projection owner: a long-lived foreground service that holds the CarPlay session.
 *
 * ## Why the session lives here and not in the Activity
 *
 * This is the single most important structural decision in the app, and `pi/docs/01` §7.1 argues it
 * from the OEM references: GM's `GMCarPlay` is a *service* holding `MANAGE_ACTIVITY_TASKS`, and
 * "return to CarPlay" is `moveTaskToFront` on an existing task — never a relaunch.
 *
 * Concretely, tying the session to a Surface would mean:
 *
 *  - music stops the moment the driver opens another app;
 *  - returning to projection costs a full re-handshake (BT → MFi → Wi-Fi handoff → pair-verify →
 *    RECORD), which is tens of seconds on real hardware;
 *  - the ChaCha20 stream key is discarded and `airplayd` will not re-send it, because it re-sends
 *    only when *its* seam socket reconnects.
 *
 * So the service owns [ProjectionSeamServer] and the activity borrows a Surface from it.
 *
 * ## Foreground type
 *
 * `connectedDevice` plus `mediaPlayback`: the former is what this actually is, the latter is what
 * keeps audio alive under Android's background restrictions. Both are declared in the manifest and
 * both must be passed to `startForeground`, or Android 14+ throws on the type mismatch.
 */
class ProjectionService : Service() {
    private val log = ProbeLog.sub("svc")

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    val status = SessionStatus()
    private val input = InputSender()
    private lateinit var seams: ProjectionSeamServer
    private lateinit var carFw: CarProjectionBridge
    private lateinit var control: WirelessControlClient
    private lateinit var mediaSession: CarPlayMediaSession
    private lateinit var micUplink: MicUplink

    private var geometry: VehicleConfigGenerator.DisplayGeometry? = null

    inner class LocalBinder : Binder() {
        val service: ProjectionService get() = this@ProjectionService
    }

    private val binder = LocalBinder()

    override fun onBind(intent: Intent?): IBinder = binder

    override fun onCreate() {
        super.onCreate()
        log.i("creating projection service")
        // Assume the restricted set until told otherwise. onCreate runs BEFORE onStartCommand, so a
        // boot start cannot be distinguished here — and guessing "unrestricted" is the guess that
        // throws. A tile launch pays one extra startForeground() below; a boot launch pays nothing
        // and survives.
        bootRestricted = true

        // Generate the VehicleConfig BEFORE anything can arm. Arming is first-arm-wins per process,
        // so a config written after airplayd starts is ignored until the next session. §5.1.
        val vcfg = VehicleConfigGenerator(this)
        geometry = vcfg.generate()
        // Logged at INFO so a bring-up session can copy it straight into airplayd's environment.
        log.i("launch airplayd with CARPLAY_CFG_FILE=${vcfg.configPath().absolutePath}")

        seams = ProjectionSeamServer(this, status, input)
        mediaSession = CarPlayMediaSession(this, input)
        micUplink = MicUplink(this)
        control = WirelessControlClient(status)
        carFw =
            CarProjectionBridge(
                ctx = this,
                status = status,
                onSiriDown = { input.siriDown() },
                onSiriUp = { input.siriUp() },
                // The call key maps to the telephony HID's hook-switch, which is answer-or-end
                // depending on call state — iOS decides which, exactly as a real head unit's single
                // call button does.
                onCallKey = { input.telephony(com.carlink.ocbm.Ocbm.TEL_ANSWER) },
            )

        startForegroundWithNotification(status.state.value)

        // Metadata -> AAOS. Publishing is what makes CarPlay a media source the head unit
        // understands rather than an opaque video window: the now-playing card, the steering-wheel
        // transport keys and anything watching MediaSessionManager all read this.
        mediaSession.start()
        seams.onNowPlayingChanged = { snap -> mediaSession.publish(snap) }
        seams.onPlaybackTick = { snap -> mediaSession.publishPlaybackState(snap) }

        input.start()
        seams.start()
        // Gated by airplayd's back-channel, so this holds no microphone until iOS actually arms a
        // voice stream.
        micUplink.start()
        control.start()
        carFw.connect()

        // Seed from the current configuration: onConfigurationChanged only fires on CHANGE, so a
        // service started at night would otherwise never tell the phone until the next transition.
        applyNightMode(resources.configuration)

        observeState()
    }

    private fun observeState() {
        // Publish to AAOS and refresh the notification on every meaningful change. distinctUntilChanged
        // on the whole state is not enough — the state carries counters-adjacent fields — so each
        // consumer narrows to what it actually cares about.
        // Promote out of the boot-restricted type set as soon as a seam producer exists.
        //
        // The earliest point the restricted types are NEEDED and the latest they are still free:
        // `microphone` must be held before iOS arms the voice uplink or capture returns zeroed
        // frames once the driver navigates away, and `airplayd` connecting is strictly earlier.
        status.state
            .map { it.phase != SessionStatus.Phase.IDLE }
            .distinctUntilChanged()
            .onEach { live ->
                if (live && bootRestricted) {
                    bootRestricted = false
                    log.i("session starting — promoting to the full foreground service type set")
                    startForegroundWithNotification(status.state.value)
                }
            }.launchIn(scope)

        status.state
            .onEach { s -> carFw.publish(s) }
            .launchIn(scope)

        status.state
            .map { it.phase to it.deviceName }
            .distinctUntilChanged()
            .onEach { startForegroundWithNotification(status.state.value) }
            .launchIn(scope)

        // Drive state → iOS limitedUI. Only on transitions: setLimitedUI is a command on the
        // encrypted event channel, not a poll, and re-sending it per restriction update would put
        // traffic on that channel every time any unrelated restriction bit changed.
        status.state
            .map { it.driveRestricted }
            .distinctUntilChanged()
            .onEach { restricted ->
                log.i("drive state → limitedUI=$restricted")
                input.setLimitedUi(restricted)
            }.launchIn(scope)

        // A session that ends should not leave the platform believing projection is live.
        status.state
            .map { it.sessionActive }
            .distinctUntilChanged()
            .onEach { active -> log.i(if (active) "session ACTIVE" else "session ended") }
            .launchIn(scope)

        // NOTHING here derives playback state from the audio seam.
        //
        // It used to, and the comment claimed the audio lane "going quiet is the honest signal" —
        // but `audioSeamUp` is a TCP connect/disconnect and `airplayd` holds that socket for the
        // whole session. It never goes quiet, so the card read STATE_PLAYING through every pause.
        // The real signal is iAP2 PlaybackStatus, merged in NowPlayingState and published from the
        // metadata path via onPlaybackTick / onNowPlayingChanged above.
    }

    /**
     * Day/night, following the AAOS UI mode.
     *
     * CarPlay renders its own UI on the phone, so it cannot see the head unit's theme — it has to be
     * told. Without this the projected UI stays in day colours through a night drive, which is the
     * single most visible way a projection integration looks unfinished.
     */
    override fun onConfigurationChanged(newConfig: android.content.res.Configuration) {
        super.onConfigurationChanged(newConfig)
        applyNightMode(newConfig)
    }

    private fun applyNightMode(cfg: android.content.res.Configuration) {
        val night =
            (cfg.uiMode and android.content.res.Configuration.UI_MODE_NIGHT_MASK) ==
                android.content.res.Configuration.UI_MODE_NIGHT_YES
        if (night == lastNightMode) return
        lastNightMode = night
        log.i("UI mode -> night=$night")
        input.setNightMode(night)
    }

    /** Null until the first evaluation, so the initial state is always sent once. */
    private var lastNightMode: Boolean? = null

    /** So the missing-permission warning is logged once, not on every notification refresh. */
    private var micWarned = false

    /**
     * True until the service has earned the restricted foreground service types.
     *
     * Android 15+ forbids a `BOOT_COMPLETED` receiver from starting an FGS of type `mediaPlayback`,
     * `microphone`, `dataSync`, `camera`, `phoneCall` or `mediaProjection`; the platform throws
     * `ForegroundServiceStartNotAllowedException`. `connectedDevice` is NOT on that list, and it is
     * what this service actually IS, so it is enough to hold the process up until a phone appears.
     *
     * The gate is `mAllowStartForeground == REASON_BOOT_COMPLETED`, so on a bench where the app is
     * already foreground the restricted types ARE granted and everything looks fine — measured on
     * this Pi as `types=0x92` from a secondary-user boot broadcast. That masking is exactly why this
     * has to be fixed by construction: the failing path is the cold boot, which is the only path
     * the receiver exists for.
     */
    @Volatile private var bootRestricted = false

    override fun onStartCommand(
        intent: Intent?,
        flags: Int,
        startId: Int,
    ): Int {
        if (bootRestricted && intent?.getBooleanExtra(EXTRA_FROM_BOOT, false) == false) {
            bootRestricted = false
            startForegroundWithNotification(status.state.value)
        }
        when (intent?.action) {
            ACTION_CONNECT -> {
                val addr = intent.getStringExtra(EXTRA_ADDRESS)
                scope.launch { control.connect(addr) }
            }
            ACTION_DISCONNECT -> scope.launch { control.disconnect() }
            ACTION_REQUEST_UI -> input.requestUi()
        }
        // STICKY: this service is the head unit's projection owner and should come back if the
        // system kills it under memory pressure. It is cheap to restart — the accessory stack keeps
        // running and will reconnect its seams.
        return START_STICKY
    }

    // ---- surface handover, called by ProjectionActivity -------------------------------------------

    /**
     * The activity has a Surface. Attach a decoder to it.
     *
     * Dimensions come from the generated config rather than the Surface's own size so the decoder is
     * configured for the stream iOS was told to send. They agree on this hardware; keeping one
     * source of truth means they still agree if the panel changes.
     */
    fun attachSurface(surface: Surface) {
        val g = geometry
        val w = g?.widthPx ?: FALLBACK_WIDTH
        val h = g?.heightPx ?: FALLBACK_HEIGHT
        seams.attachSurface(surface, w, h)
    }

    fun detachSurface() = seams.detachSurface()

    /**
     * The projection surface became visible or went away.
     *
     * **Not** named `setForeground`: `android.app.Service` has a hidden, FINAL
     * `setForeground(boolean)` left over from pre-API-5 days. Overriding it compiles cleanly and
     * then throws `LinkageError` the first time the class is loaded on device —
     * "overrides final method in class Landroid/app/Service" — killing the process before any of
     * our code runs. Nothing in the toolchain warns about it.
     */
    fun setUiForeground(foreground: Boolean) = status.setForeground(foreground)

    /** Touch and HID input from the activity. */
    val inputSender: InputSender get() = input

    fun diagnostics(): String = "${seams.diagnostics()} | in=${input.sent.get()}/${input.dropped.get()} drop | mic ${micUplink.diagnostics()}"

    // ---- notification ----------------------------------------------------------------------------

    private fun startForegroundWithNotification(s: SessionStatus.State) {
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL_ID, "CarPlay projection", NotificationManager.IMPORTANCE_LOW).apply {
                description = "Shown while a CarPlay session is running"
                setShowBadge(false)
            },
        )
        val text =
            when (s.phase) {
                SessionStatus.Phase.IDLE -> "Waiting for a phone"
                SessionStatus.Phase.LINKING -> "Connecting…"
                SessionStatus.Phase.KEYED -> s.deviceName?.let { "Connected to $it" } ?: "Connected"
                SessionStatus.Phase.STREAMING -> s.deviceName?.let { "CarPlay — $it" } ?: "CarPlay"
            }
        val open =
            PendingIntent.getActivity(
                this,
                0,
                Intent(this, ProjectionActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
        val n =
            Notification
                .Builder(this, CHANNEL_ID)
                .setContentTitle("CarPlay")
                .setContentText(text)
                .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth)
                .setContentIntent(open)
                .setOngoing(true)
                .build()
        // The microphone type is CONDITIONAL on actually holding RECORD_AUDIO.
        //
        // Declaring an FGS type the app lacks the permission for makes startForeground throw
        // SecurityException — from inside onCreate, taking video, audio and input down with it. That
        // is exactly the plain `adb install` case (no -g) the manifest comment promises degrades
        // gracefully, and it would have failed hard instead.
        //
        // Without the type, capture from a backgrounded app returns ZEROED frames on Android 11+
        // with no error, so a call goes one-way once the driver opens another app. Hence: request it
        // when we can use it, and say plainly when we cannot.
        val micGranted =
            checkSelfPermission(android.Manifest.permission.RECORD_AUDIO) ==
                android.content.pm.PackageManager.PERMISSION_GRANTED
        var types =
            ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE or
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK
        if (micGranted) {
            types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
        } else if (!micWarned) {
            micWarned = true
            log.w(
                "RECORD_AUDIO not granted — running without the microphone FGS type. The mic uplink " +
                    "will be silent, and capture would return zeroed frames once backgrounded. " +
                    "Install with `adb install -g` or grant the permission.",
            )
        }
        if (bootRestricted) {
            // mediaPlayback and microphone are both denied from a BOOT_COMPLETED start. Take the one
            // type that is allowed; the promotion above adds the rest when a session appears.
            types = ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE
        }
        try {
            startForeground(NOTIFICATION_ID, n, types)
        } catch (e: Exception) {
            // Never let the notification take the session down — but say exactly what was lost, or
            // this degrades into "CarPlay does not come up after a reboot but works from the tile"
            // with no cause recorded anywhere.
            log.e(
                "startForeground(types=0x${types.toString(16)}) REFUSED: $e — this service is now an " +
                    "ordinary background service and the system will kill it shortly. If that is a " +
                    "ForegroundServiceStartNotAllowedException naming a type, the boot-restricted " +
                    "set above is wrong for this platform build.",
            )
        }
    }

    override fun onDestroy() {
        log.i("destroying projection service")
        // Order: tell the platform first (while the binder is still alive), then tear down.
        carFw.disconnect()
        control.stop()
        seams.stop()
        micUplink.stop()
        mediaSession.stop()
        input.stop()
        scope.cancel()
        super.onDestroy()
    }

    companion object {
        const val ACTION_CONNECT = "com.carlink.projection.action.CONNECT"
        const val ACTION_DISCONNECT = "com.carlink.projection.action.DISCONNECT"
        const val ACTION_REQUEST_UI = "com.carlink.projection.action.REQUEST_UI"
        const val EXTRA_ADDRESS = "address"

        private const val CHANNEL_ID = "carplay_projection"
        private const val NOTIFICATION_ID = 1

        /**
         * Only used if the display could not be read at all, which would already have been logged
         * as an error. 1920x1080 is what this bench reports.
         */
        private const val FALLBACK_WIDTH = 1920
        private const val FALLBACK_HEIGHT = 1080

        const val EXTRA_FROM_BOOT = "fromBoot"

        fun start(
            ctx: Context,
            fromBoot: Boolean = false,
        ) {
            ctx.startForegroundService(
                Intent(ctx, ProjectionService::class.java).putExtra(EXTRA_FROM_BOOT, fromBoot),
            )
        }
    }
}
