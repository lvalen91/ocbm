package com.carlink

import android.Manifest
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.KeyEvent
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.core.content.ContextCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.lifecycle.lifecycleScope
import com.carlink.logging.Logger
import com.carlink.logging.logInfo
import com.carlink.logging.logWarn
import com.carlink.media.MediaSessionManager
import com.carlink.protocol.AdapterConfig
import com.carlink.protocol.KnownDevices
import com.carlink.ui.MainScreen
import com.carlink.ui.settings.DisplayMode
import com.carlink.ui.settings.DisplayModeStore
import com.carlink.ui.theme.CarlinkTheme
import com.carlink.util.DisplayProfile
import com.carlink.util.LogCallback
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Main Activity — Entry Point for Carlink Native.
 *
 * The sole Activity in the app; owns every long-lived session object.
 *
 * Responsibilities:
 *  - Boot sequencing (logging → immersive → deferred CarlinkManager init) on the main thread.
 *  - Ownership of [CarlinkManager] (nullable to survive being destroyed before init completes).
 *  - Microphone runtime permission (denial is survivable).
 *  - The persisted [DisplayMode] applied to the window ([applyDisplayMode]); the content area it leaves is what the session declares.
 *  - In-place session rebuild via [reinitialize] (used by Reset Connection).
 *  - USB attach (onNewIntent, via manifest intent-filter) + detach (BroadcastReceiver)
 *    handling for faster disconnect detection than USB-transfer error paths provide.
 *  - Compose UI host: the top-level [CarlinkApp] composable keeps [MainScreen]
 *    continuously composed (to preserve the VideoSurface / HWC plane) and slides
 *    [SettingsScreen] on top via AnimatedVisibility rather than replacing it.
 */
class MainActivity : ComponentActivity() {
    // Nullable to prevent UninitializedPropertyAccessException if Activity
    // is destroyed before initialization completes (e.g., low memory kill)
    private var carlinkManager: CarlinkManager? = null

    /** The display the live manager was built for; a configuration change that moves its content area rebuilds. */
    private var activeProfile: DisplayProfile? = null

    /**
     * Which system bars the projection keeps — the user's persisted choice ([DisplayModeStore]),
     * read once here. It decides the content area, so it is applied to the window BEFORE the
     * display is detected and a change of it rebuilds the session like a panel change does.
     */
    private lateinit var displayModeStore: DisplayModeStore
    private var displayMode: DisplayMode = DisplayMode.DEFAULT

    /**
     * App-scope MediaSession reference. Owned and lifecycle-managed by
     * [CarlinkMediaBrowserService] — this Activity only reads the live singleton via
     * [MediaSessionManager.instance] / [MediaSessionManager.getOrCreate] to pass into
     * [CarlinkManager]. Never released from here; the MBS releases on its own
     * `onDestroy`. Survival across tier-2 CarlinkManager rebuilds is now a property of
     * the Service lifecycle (Service stays alive across Activity destruction), not a
     * property of this field.
     *
     * Boot-race fix (2026-05-03): previously this Activity created the manager, which
     * meant the session didn't exist until the user tapped the launcher icon — AAOS
     * auto-launches the MBS at boot and probes onGetSession within ~50ms, so the
     * Activity-scoped initialize never won the race. Ownership moved to MBS.onCreate.
     */
    private var mediaSessionManager: MediaSessionManager? = null

    /**
     * LogCallback routed to the app's [com.carlink.logging.Logger] + any active file log.
     * Passed to [MediaSessionManager] so its lifecycle events surface through the same
     * infrastructure that [CarlinkManager] uses. Stable singleton — one instance per
     * Activity lifetime, safe to share across rebuilds.
     */
    private val mediaSessionLogCallback =
        object : LogCallback {
            override fun log(message: String) {
                logInfo(message, tag = "MEDIA_SESSION")
            }

            override fun log(
                tag: String,
                message: String,
            ) {
                logInfo(message, tag = tag)
            }
        }

    // Observable state for Compose — replacement triggers recomposition of CarlinkApp
    private val carlinkManagerState = mutableStateOf<CarlinkManager?>(null)

    // Pending reinit handler — tracked for cancellation on rapid Reset Connection taps
    private var pendingReinitRunnable: Runnable? = null
    private val mainHandler = Handler(Looper.getMainLooper())

    // Microphone permission launcher. Denial is survivable — CarlinkManager still constructs;
    // mic features (Siri, phone-call capture) just fail silently.
    private val micPermissionLauncher =
        registerForActivityResult(
            ActivityResultContracts.RequestPermission(),
        ) { isGranted ->
            logInfo("Microphone permission ${if (isGranted) "granted" else "denied"}", tag = "MAIN")
        }

    /**
     * BroadcastReceiver for USB device detachment events.
     *
     * Provides immediate detection when the Carlinkit adapter is physically
     * disconnected, enabling faster recovery than waiting for USB transfer errors
     * to surface. Filters to known Carlinkit VID/PID pairs before signaling
     * [CarlinkManager.onUsbDeviceDetached] — other USB device events are ignored.
     */
    private val usbDetachReceiver =
        object : BroadcastReceiver() {
            override fun onReceive(
                context: Context,
                intent: Intent,
            ) {
                if (UsbManager.ACTION_USB_DEVICE_DETACHED == intent.action) {
                    val device =
                        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                            intent.getParcelableExtra(UsbManager.EXTRA_DEVICE, UsbDevice::class.java)
                        } else {
                            @Suppress("DEPRECATION")
                            intent.getParcelableExtra(UsbManager.EXTRA_DEVICE)
                        }

                    device?.let {
                        // Only handle if it's a known Carlinkit device
                        if (KnownDevices.isKnownDevice(it.vendorId, it.productId)) {
                            logWarn(
                                "[USB_DETACH] Carlinkit device detached: VID=0x${it.vendorId.toString(16)} " +
                                    "PID=0x${it.productId.toString(16)} path=${it.deviceName}",
                                tag = "MAIN",
                            )
                            // Notify CarlinkManager of the detachment (null-safe)
                            carlinkManager?.onUsbDeviceDetached()
                        }
                    }
                }
            }
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        // Enable edge-to-edge display
        enableEdgeToEdge()

        // Keep screen on during projection
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)

        // Initialize logging
        initializeLogging()

        // The persisted display mode is applied to the window before the display is detected, so
        // the content area DisplayProfile.detect() derives matches the bars actually on screen.
        displayModeStore = DisplayModeStore(this)
        displayMode = displayModeStore.get()
        applyDisplayMode()

        // Defer CarlinkManager creation to the next main-looper tick so the decorView is
        // attached before DisplayProfile.detect() reads insets; before attachment
        // currentWindowMetrics.windowInsets comes back CONSUMED (all-zero) and the content
        // area silently equals the whole window.
        // NOTE: currentWindowMetrics.bounds is the FULL window (2400x960 on the emulator)
        // regardless of decorFitsSystemWindows — verified via dumpsys (mBounds/mAppBounds =
        // 2400x960 in SYSTEM_UI_VISIBLE). The content area comes from subtracting the stable
        // insets of the bars the mode leaves visible exactly ONCE, inside detect().
        // Compose renders `if (manager != null)` while we wait, so the UI boots with
        // the loading overlay and swaps in as soon as the manager is ready.
        window.decorView.post {
            if (!isDestroyed && !isFinishing) {
                initializeCarlinkManager()
            }
        }

        // Request microphone permission (for Siri / phone-call capture)
        requestMicrophonePermission()

        // Register USB detachment receiver for immediate disconnect detection
        registerUsbDetachReceiver()

        // Set up Compose UI
        // CarlinkManager is observed via mutableStateOf — replacement during display mode
        // reinit triggers full recomposition without Activity restart.
        setContent {
            val manager = carlinkManagerState.value
            CarlinkTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    if (manager != null) {
                        CarlinkApp(
                            carlinkManager = manager,
                            // Reset Connection rebuilds the full manager: new SurfaceView,
                            // fresh HWC plane, fresh WindowMetrics, renegotiated Open().
                            onResetConnection = { reinitialize() },
                            displayMode = displayMode,
                            onDisplayModeSelected = { onDisplayModeSelected(it) },
                        )
                    }
                }
            }
        }
    }

    override fun onResume() {
        super.onResume()
        // Re-assert the display mode — the system may have shown bars while backgrounded.
        applyDisplayMode()
    }

    override fun onStart() {
        super.onStart()
        // Resume video decoding when app returns to foreground
        // On AAOS, Surface may remain valid while app is in background, but
        // BufferQueue can stall. Resume codec and request keyframe for immediate video.
        logInfo("[LIFECYCLE] onStart - resuming video", tag = "MAIN")
        carlinkManager?.resumeVideo()
    }

    override fun onStop() {
        super.onStop()
        // Pause video decoding when app goes to background
        // On AAOS, when another app covers this app (Maps, Phone, etc.), the Surface
        // may remain valid but SurfaceFlinger stops consuming frames. This causes
        // BufferQueue to fill up, stalling the decoder. Flushing prevents this.
        // USB connection and audio continue unaffected.
        logInfo("[LIFECYCLE] onStop - pausing video", tag = "MAIN")
        carlinkManager?.pauseVideo()
    }

    /**
     * Voice keys that reach the foreground activity go to Siri. On AAOS `KEYCODE_VOICE_ASSIST` is
     * normally intercepted by `CarInputService` (see `voice/`), and `ASSIST`/`SEARCH` by the window
     * manager — so this mostly matters on head units that let the key through. Cheap and harmless
     * where it never fires.
     */
    override fun onKeyDown(
        keyCode: Int,
        event: KeyEvent,
    ): Boolean {
        val voice =
            keyCode == KeyEvent.KEYCODE_VOICE_ASSIST ||
                keyCode == KeyEvent.KEYCODE_ASSIST ||
                keyCode == KeyEvent.KEYCODE_SEARCH
        if (voice && event.repeatCount == 0) {
            val sent = carlinkManager?.requestSiri() ?: false
            logInfo("[KEY] voice key $keyCode -> Siri ${if (sent) "sent" else "dropped"}", tag = "MAIN")
            return true
        }
        return super.onKeyDown(keyCode, event)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        // Only re-launch cluster binding for actual USB re-attach events.
        // The "bring back" REORDER_TO_FRONT intent from launchCarAppActivity()
        // also arrives here (singleTop) — must NOT re-trigger the launch cycle.
        if (intent.action == UsbManager.ACTION_USB_DEVICE_ATTACHED) {
            // Auto-connect when adapter re-enumerates (e.g., after reboot or replug)
            val manager = carlinkManager
            if (manager != null && manager.state == CarlinkManager.State.DISCONNECTED) {
                logInfo("[LIFECYCLE] Manager disconnected — auto-starting connection", tag = "MAIN")
                // lifecycleScope (not an unowned CoroutineScope): an unowned launch outlived
                // reinitialize()/onDestroy and could drive a released manager through a full
                // start() against torn-down state. Activity destruction now cancels it.
                lifecycleScope.launch(Dispatchers.IO) {
                    manager.start()
                }
            }
        }
    }

    override fun onDestroy() {
        super.onDestroy()

        // Unregister USB detachment receiver
        unregisterUsbDetachReceiver()

        // Release the CarlinkManager (it detaches its transport callback from the
        // MediaSession but does NOT release it — see CarlinkManager.release KDoc).
        // The MediaSession itself is owned by CarlinkMediaBrowserService; it releases
        // on its own onDestroy. Just drop the local reference here.
        carlinkManager?.release()
        mediaSessionManager = null

        logInfo("MainActivity destroyed", tag = "MAIN")
    }

    private fun initializeLogging() {
        val packageInfo = packageManager.getPackageInfo(packageName, 0)
        val appVersion = "${packageInfo.versionName}+${packageInfo.longVersionCode}"

        // DEBUG builds: full logcat output. RELEASE builds: WARN/ERROR only (enforced in
        // Logger.log) and verbose pipeline tags disabled. The log-to-file feature was removed
        // in this build — `adb logcat` is the only sink.
        val isDebugBuild = BuildConfig.DEBUG
        Logger.setDebugLoggingEnabled(isDebugBuild)

        logInfo("Carlink Native starting - version $appVersion", tag = "MAIN")
        logInfo("[LOGGING] Debug logging: ${if (isDebugBuild) "ENABLED" else "DISABLED (release build)"}", tag = "MAIN")
    }

    private fun initializeCarlinkManager() {
        // Launch-time display detection: the content area under the active display mode (window
        // minus the bars it leaves visible), refresh rate, density, cutouts/waterfall/rounded
        // corners. This — not a stored setting — is what the CT_SUBSCRIBE config declares, what
        // the decoder is sized to, and what the video surface is laid out to (MainScreen pads by
        // the same barInsets), so touch is normalised over the rectangle iOS encodes for.
        val profile = DisplayProfile.detect(this, displayMode.visibleBarTypes)
        activeProfile = profile
        val g = profile.geometry
        val cutout = profile.cutout

        // ViewArea/SafeArea binary blobs for the adapter (safeArea ⊆ viewArea ⊆ content area).
        val viewAreaData = buildViewAreaData(g.width, g.height)
        val safeAreaData =
            buildSafeAreaData(
                g.width,
                g.height,
                cutout.top,
                cutout.bottom,
                cutout.left,
                cutout.right,
            )

        // Hardcoded adapter config (cp-stripped): adapter audio, 48kHz, app mic, 5GHz, 60fps,
        // 1000ms media delay, LHD. AdapterConfig defaults bake the rest. width/height are the
        // declared panel (= the content area, even, inside PanelRule's envelope) so the decoder
        // and the pushed config are one number.
        val config =
            AdapterConfig(
                width = g.width,
                height = g.height,
                fps = 60,
                dpi = g.dpi,
                // Show the CarPlay OEM "Exit" host-UI icon (airplay.conf oemIconVisible=1).
                oemIconVisible = true,
                viewAreaData = viewAreaData,
                safeAreaData = safeAreaData,
            )

        logInfo(
            "[WINDOW] mode=${displayMode.name} physical ${profile.physicalWidthPx}x${profile.physicalHeightPx}, " +
                "window ${profile.windowWidthPx}x${profile.windowHeightPx}, bars ${profile.barInsets}, " +
                "content ${profile.widthPx}x${profile.heightPx} -> video ${g.width}x${g.height}, " +
                "cutout T:${cutout.top} B:${cutout.bottom} L:${cutout.left} R:${cutout.right}",
            tag = "MAIN",
        )
        logInfo("Display config: ${config.width}x${config.height}@${config.fps}fps, ${config.dpi}dpi", tag = "MAIN")

        // Adapter audio mode (hardcoded false) — acquire the app-scope MediaSession singleton.
        applyAudioTransferModeToMediaSession(false)

        carlinkManager = CarlinkManager(this, config, mediaSessionManager, profile)
        carlinkManagerState.value = carlinkManager
    }

    /**
     * Re-detect on a configuration change (the manifest keeps screenSize/density/orientation changes
     * in-process; Compose re-lays the dashboard on its own from the window size — see
     * `ui/adaptive/WindowLayout.kt`). A content area that actually moved — the panel, the window
     * (split-screen), the bars around it or the safe area — needs a new CT_SUBSCRIBE: the box only
     * knows the geometry it was pushed. The manager is then rebuilt exactly as Reset Connection does.
     * A live mid-session resize (`CMD_VIEW_AREA`) is NOT used: this app declares a single view area.
     */
    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        val before = activeProfile ?: return
        if (carlinkManager == null || pendingReinitRunnable != null) return
        val now = DisplayProfile.detect(this, displayMode.visibleBarTypes)
        if (now.geometry == before.geometry && now.barInsets == before.barInsets) return
        logInfo(
            "[DISPLAY] content area changed ${before.widthPx}x${before.heightPx} -> ${now.widthPx}x${now.heightPx} " +
                "(window ${now.windowWidthPx}x${now.windowHeightPx}, bars ${now.barInsets}) — rebuilding session",
            tag = "MAIN",
        )
        reinitialize()
    }

    /**
     * The user picked a display mode on the dashboard: persist it, then rebuild the session — the
     * bars the mode leaves visible change the content area, which is a new CT_SUBSCRIBE, a new
     * decoder size and a new surface rect, exactly like a panel change. [reinitialize] applies the
     * mode to the window before it re-detects.
     */
    private fun onDisplayModeSelected(mode: DisplayMode) {
        if (mode == displayMode) return
        logInfo("[DISPLAY] mode ${displayMode.name} -> ${mode.name}", tag = "MAIN")
        displayModeStore.set(mode)
        displayMode = mode
        reinitialize()
    }

    /**
     * Map the new config's `audioTransferMode` onto the app-scope [MediaSessionManager]
     * singleton (owned by [CarlinkMediaBrowserService]).
     *
     * - ADAPTER mode: take a reference to the singleton so [CarlinkManager] can publish
     *   metadata. The MBS already initialized it at boot; we never call `initialize()`
     *   from the Activity anymore.
     * - BLUETOOTH mode: drop our local reference and flip the session to placeholder
     *   via [MediaSessionManager.setInactive]. Crucially we do NOT call `release()` —
     *   destroying the session token here would re-trigger the boot-race-equivalent
     *   stale-card bug if the user toggles back to ADAPTER.
     *
     * Defensive bootstrap: if the singleton somehow isn't present yet (MBS not started
     * for any reason), we call [MediaSessionManager.getOrCreate] + `initialize()` to
     * cover the gap. In normal flow this branch is never taken.
     *
     * @param audioTransferMode `false` = ADAPTER (audio routed via USB, media session
     *   actively published), `true` = BLUETOOTH (audio via phone BT, session stays
     *   registered but inactive).
     */
    private fun applyAudioTransferModeToMediaSession(audioTransferMode: Boolean) {
        if (!audioTransferMode) {
            if (mediaSessionManager == null) {
                mediaSessionManager = MediaSessionManager.instance()
                    ?: try {
                        // Defensive — should not be reached in normal flow because MBS.onCreate
                        // already bootstrapped the singleton. Pass applicationContext for the
                        // same StaticFieldLeak-free lifetime guarantees as the MBS path.
                        MediaSessionManager
                            .getOrCreate(applicationContext, mediaSessionLogCallback)
                            .apply {
                                initialize()
                            }.also {
                                logWarn(
                                    "[MEDIA_SESSION] singleton missing — bootstrapped from Activity (unexpected)",
                                    tag = "MAIN",
                                )
                            }
                    } catch (e: Exception) {
                        logWarn(
                            "[MEDIA_SESSION] bootstrap failed — running without AAOS media integration: ${e.message}",
                            tag = "MAIN",
                        )
                        null
                    }
                if (mediaSessionManager != null) {
                    logInfo("[MEDIA_SESSION] Singleton acquired (ADAPTER mode)", tag = "MAIN")
                }
            }
        } else {
            mediaSessionManager?.let {
                logInfo("[MEDIA_SESSION] Audio mode switched to BLUETOOTH — flipping session inactive", tag = "MAIN")
                it.setInactive()
            }
            mediaSessionManager = null
        }
    }

    /**
     * Rebuild the CarlinkManager in place (used by Reset Connection, a display-mode change and a
     * configuration change that moved the content area). Tears down the current adapter session,
     * re-asserts the display mode, and reconstructs the manager against fresh WindowMetrics once
     * the system-UI transition settles. End state equals kill+relaunch: new SurfaceView, fresh HWC
     * plane, renegotiated Open().
     */
    fun reinitialize() {
        // Cancel any pending reinit from a rapid double-tap.
        pendingReinitRunnable?.let { mainHandler.removeCallbacks(it) }
        pendingReinitRunnable = null

        logInfo("[REINIT] Begin — rebuilding CarlinkManager", tag = "MAIN")

        // 1. Tear down current adapter session.
        val oldManager = carlinkManager
        if (oldManager != null) {
            // Clear Compose reference FIRST — stops surface callbacks into the old manager.
            carlinkManagerState.value = null
            oldManager.release()
            carlinkManager = null
            logInfo("[REINIT] Old CarlinkManager released", tag = "MAIN")
        }

        // 2. Re-assert the display mode (a mode change lands here with the new mode already set).
        applyDisplayMode()

        // 3. Rebuild after the system-bar/WindowMetrics transition settles (200ms).
        val reinitRunnable =
            Runnable {
                if (!isDestroyed && !isFinishing) {
                    logInfo("[REINIT] Rebuilding CarlinkManager with fresh metrics", tag = "MAIN")
                    initializeCarlinkManager()
                    logInfo("[REINIT] Complete", tag = "MAIN")
                }
                pendingReinitRunnable = null
            }
        pendingReinitRunnable = reinitRunnable
        mainHandler.postDelayed(reinitRunnable, 200)
    }

    /** Build HU_VIEWAREA_INFO (24 bytes): [screen_w, screen_h, view_w, view_h, originX, originY] */
    private fun buildViewAreaData(
        width: Int,
        height: Int,
    ): ByteArray =
        ByteBuffer
            .allocate(24)
            .order(ByteOrder.LITTLE_ENDIAN)
            .putInt(width)
            .putInt(height) // screen dims
            .putInt(width)
            .putInt(height) // viewarea dims (same)
            .putInt(0)
            .putInt(0) // origin
            .array()

    /** Build HU_SAFEAREA_INFO (20 bytes): [safe_w, safe_h, originX, originY, drawOutside] */
    private fun buildSafeAreaData(
        videoW: Int,
        videoH: Int,
        insetTop: Int,
        insetBottom: Int,
        insetLeft: Int,
        insetRight: Int,
    ): ByteArray {
        val safeW = videoW - insetLeft - insetRight
        val safeH = videoH - insetTop - insetBottom
        val hasInsets = (insetTop or insetBottom or insetLeft or insetRight) != 0
        return ByteBuffer
            .allocate(20)
            .order(ByteOrder.LITTLE_ENDIAN)
            .putInt(safeW)
            .putInt(safeH)
            .putInt(insetLeft)
            .putInt(insetTop)
            .putInt(if (hasInsets) 1 else 0) // wallpaper outside safe area only when cutouts exist
            .array()
    }

    private fun requestMicrophonePermission() {
        if (ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO)
            == PackageManager.PERMISSION_GRANTED
        ) {
            logInfo("Microphone permission already granted", tag = "MAIN")
        } else {
            logInfo("Requesting microphone permission", tag = "MAIN")
            micPermissionLauncher.launch(Manifest.permission.RECORD_AUDIO)
        }
    }

    /**
     * Applies the active [DisplayMode] to the window: hide the bars it hides, show the ones it
     * keeps, draw edge-to-edge into the cutout, and reveal hidden bars transiently on swipe. The
     * window stays edge-to-edge in every mode; a visible bar is kept OUT of the video by the
     * content area the profile derives (`barInsets`), not by letting the decor fit it — that keeps
     * the surface, the pushed config and the touch basis one rectangle.
     */
    private fun applyDisplayMode() {
        val windowInsetsController = WindowCompat.getInsetsController(window, window.decorView)
        val lp = window.attributes
        // Draw edge-to-edge into the cutout (gminfo3.7 has none, so this is a no-op there).
        lp.layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
        window.attributes = lp
        WindowCompat.setDecorFitsSystemWindows(window, false)
        val hidden = displayMode.hiddenBarTypes
        val shown = displayMode.visibleBarTypes
        if (shown != 0) windowInsetsController.show(shown)
        if (hidden != 0) windowInsetsController.hide(hidden)
        windowInsetsController.systemBarsBehavior =
            WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        logInfo("[DISPLAY] Applied display mode ${displayMode.name}", tag = "MAIN")
    }

    /**
     * Registers the USB detachment BroadcastReceiver.
     *
     * This enables immediate detection of physical adapter removal,
     * providing faster recovery than waiting for USB transfer errors.
     */
    private fun registerUsbDetachReceiver() {
        val filter = IntentFilter(UsbManager.ACTION_USB_DEVICE_DETACHED)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            registerReceiver(usbDetachReceiver, filter, RECEIVER_NOT_EXPORTED)
        } else {
            registerReceiver(usbDetachReceiver, filter)
        }
        logInfo("[USB_DETACH] Registered USB detachment receiver", tag = "MAIN")
    }

    /**
     * Unregisters the USB detachment BroadcastReceiver.
     */
    private fun unregisterUsbDetachReceiver() {
        try {
            unregisterReceiver(usbDetachReceiver)
            logInfo("[USB_DETACH] Unregistered USB detachment receiver", tag = "MAIN")
        } catch (e: IllegalArgumentException) {
            // Receiver was not registered or already unregistered
            logWarn("[USB_DETACH] Receiver already unregistered: ${e.message}", tag = "MAIN")
        }
    }
}

/**
 * Root composable: the single [MainScreen]. When projecting it's the CarPlay video surface;
 * otherwise it shows the in-screen dashboard (adapter status / controls / known devices). The
 * cp-stripped build collapsed the old Settings overlay into that dashboard — there is no
 * overlay/stack anymore.
 */
@Composable
fun CarlinkApp(
    carlinkManager: CarlinkManager,
    onResetConnection: () -> Unit = {},
    displayMode: DisplayMode = DisplayMode.DEFAULT,
    onDisplayModeSelected: (DisplayMode) -> Unit = {},
) {
    MainScreen(
        carlinkManager = carlinkManager,
        onResetConnection = onResetConnection,
        displayMode = displayMode,
        onDisplayModeSelected = onDisplayModeSelected,
    )
}
