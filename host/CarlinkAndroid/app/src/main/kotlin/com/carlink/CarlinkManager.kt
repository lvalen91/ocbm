package com.carlink

import android.content.Context
import android.os.Looper
import android.os.PowerManager
import android.os.SystemClock
import android.view.Surface
import com.carlink.audio.MicProfile
import com.carlink.audio.MicrophoneCaptureManager
import com.carlink.av.AacPlayer
import com.carlink.av.HevcRenderer
import com.carlink.av.VoiceRouter
import com.carlink.car.DriveStateMonitor
import com.carlink.device.KnownDevice
import com.carlink.device.KnownDeviceSnapshot
import com.carlink.device.KnownDeviceStore
import com.carlink.device.mergeDeviceList
import com.carlink.logging.Logger
import com.carlink.logging.ProbeLog
import com.carlink.logging.logDebug
import com.carlink.logging.logError
import com.carlink.logging.logInfo
import com.carlink.logging.logWarn
import com.carlink.media.CarlinkMediaBrowserService
import com.carlink.media.MediaSessionManager
import com.carlink.ocbm.BinaryPlist
import com.carlink.ocbm.Ocbm
import com.carlink.ocbm.OcbmAvLanes
import com.carlink.ocbm.OcbmClient
import com.carlink.ocbm.OemIcon
import com.carlink.ocbm.UsbBulkTransport
import com.carlink.ocbm.VehicleConfigSpec
import com.carlink.ocbm.VehicleConfigYaml
import com.carlink.ocbm.seam.SeamPipe
import com.carlink.protocol.AdapterConfig
import com.carlink.protocol.MessageSerializer
import com.carlink.protocol.MultiTouchAction
import com.carlink.protocol.PhoneType
import com.carlink.util.LogCallback
import kotlinx.coroutines.CoroutineExceptionHandler
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.util.concurrent.atomic.AtomicReference

/**
 * Main Carlink Manager — OCBM.
 *
 * Central orchestrator for the CarPlay-only app against the **open OCBM userspace** on a
 * CPC200-CCPA:
 * - USB device lifecycle ([UsbBulkTransport]) and the OCBM session ([OcbmClient])
 * - Video via [HevcRenderer], fed from `CH_VIDEO` through the forward-encrypted seam
 * - Audio via [AacPlayer] (media) and [VoiceRouter] (telephony/Siri/alert/nav)
 * - Microphone capture gated by the box's own `CT_UPLINK`, shipped over `CH_MIC`
 * - MediaSession integration for AAOS (metadata + album art) from `CH_METADATA`
 * - Auto-reconnect with exponential backoff and Pattern A / Pattern C escalation
 *
 * ## What changed from the riddleBox era, and why the shape is different
 *
 * The old manager was a **message pump**: one `handleMessage` switch over decoded protocol
 * messages, from which every piece of session state was *inferred*. OCBM inverts that — the box
 * states things directly (`CT_BT_PHASE`, `SEV_PHONE_*`, `CT_UPLINK`, per-stream `audioType`), so
 * the pump is gone and this class is a set of callbacks wired to [OcbmClient].
 *
 * Two whole categories of bug disappear with it:
 * - **Voice-mode arbitration.** riddleBox required a `VoiceMode` state machine because
 *   `PHONECALL_START` arrived ~130 ms *before* `SIRI_STOP`, and a naive reading killed the call's
 *   microphone. OCBM routes per stream by `audioType`, so there is no command ordering to reason
 *   about, and the mic is gated by the box.
 * - **Format guessing.** Audio formats came from a `decodeType` byte mapped through a fixed table;
 *   now each stream carries its own `SEAM_FORMAT`.
 *
 * ## The public API is deliberately unchanged
 *
 * Every member the UI and media layers call keeps its exact signature, so `MainActivity`,
 * `MainScreen`, `PhonesTab` and everything under `media/` compile untouched. That is why
 * [sendMultiTouch] still takes riddleBox's `MessageSerializer.TouchPoint`: it is the app's
 * host-side input value type now, and rewriting `MainScreen.handleTouchEvent` (110 lines of
 * hard-won deadband / ACTION_CANCEL / DOWN-demotion logic) while also swapping protocols would
 * have been two risky changes at once.
 */
class CarlinkManager(
    private val context: Context,
    initialConfig: AdapterConfig = AdapterConfig.DEFAULT,
    /**
     * MediaSession manager owned by [MainActivity] (app-scope lifetime). Injected so the
     * underlying Media3 session survives CarlinkManager rebuilds — without it, each rebuild
     * would fire `onSessionDestroyed` on CarLauncher's controller, which does not rebind to a
     * new token, leaving the homescreen Media card blank until a reinstall.
     */
    injectedMediaSessionManager: MediaSessionManager? = null,
) {
    private var config: AdapterConfig = initialConfig

    /** Configured WiFi/Bluetooth name the adapter advertises (shown on the dashboard). */
    val adapterName: String get() = config.boxName

    companion object {
        private const val USB_WAIT_PERIOD_MS = 3000L

        private const val MAX_RECONNECT_ATTEMPTS = 5

        // Wake lock: safety timeout per acquire, and how often the refresh job renews it.
        // Refresh at half the timeout so a missed tick still leaves a full margin.
        private const val WAKE_LOCK_TIMEOUT_MS = 2 * 60 * 60 * 1000L // 2h
        private const val WAKE_LOCK_REFRESH_INTERVAL_MS = WAKE_LOCK_TIMEOUT_MS / 2

        // How long a forgotten device's MAC is suppressed from list merges while the box
        // processes the forget (it restarts wireless, which is not instant).
        private const val FORGET_SUPPRESS_MS = 25_000L

        private const val INITIAL_RECONNECT_DELAY_MS = 2000L
        private const val MAX_RECONNECT_DELAY_MS = 30000L

        // Surface debouncing — wait for size to stabilize before rebuilding the decoder.
        private const val SURFACE_DEBOUNCE_MS = 150L

        // ---------------------------------------------------------------------------------
        // Reconnect-escalation patterns (in-source conventions)
        //
        //   Pattern A — "no initial response" errors in a row (consecutiveNoResponse >= 2).
        //               Under OCBM this means CT_HELLO went unanswered: the box is absent,
        //               still booting, or its OUT path is dead. Retrying won't help.
        //               Surfaced as "Adapter not responding — reboot adapter".
        //
        //   Pattern B — DELETED. It keyed off riddleBox's SCANNING_DEVICE command, which has
        //               no OCBM analogue; CT_BT_PHASE now reports the handshake truthfully
        //               instead of it being inferred. `hadPriorSession` survives only to
        //               select the give-up message.
        //
        //   Pattern C — STREAMING sessions that die within SHORT_SESSION_THRESHOLD_MS,
        //               SHORT_SESSION_ESCALATION_COUNT times in a row → "connection unstable".
        //
        // Keep these labels/thresholds stable; remote logs filter on the "[ESCALATION]" tag.
        // ---------------------------------------------------------------------------------

        private const val SHORT_SESSION_THRESHOLD_MS = 10_000L
        private const val SHORT_SESSION_ESCALATION_COUNT = 2

        /**
         * Contacts the box can carry. Apple's `HIDTouchScreenMultiCreateDescriptor` declares
         * exactly two `Finger` collections, so this is the descriptor's capacity, not a policy
         * choice — raising it would emit contacts the HID report has no field for.
         */
        private const val MAX_CONTACTS = 2

        /** Must hold one whole 2400x960 HEVC IDR; see [SeamPipe]'s capacity contract. */
        private const val VIDEO_PIPE_BYTES = 8 * 1024 * 1024
        private const val VIDEO_PIPE_DEPTH = 64

        /** After an overlay closes, how long to wait for frames before assuming a stuck decoder. */
        private const val OVERLAY_RECOVERY_WATCHDOG_MS = 750L
    }

    /**
     * Connection state enum.
     */
    enum class State {
        DISCONNECTED,
        CONNECTING,
        DEVICE_CONNECTED,
        STREAMING,
    }

    /**
     * Information about a known device.
     *
     * `type` is always "CarPlay" and `rfcomm` is always null under OCBM: `MGMT_INFO.devices` is a
     * list of bare MAC strings read out of the box's 25-byte link-key store, which holds no name,
     * no type and no timestamp. `name` and `lastConnected` are supplied by the app's own history
     * (see [com.carlink.device.KnownDeviceStore]), fed by `CT_PHONE_IDENT` — the box genuinely
     * cannot provide either.
     *
     * [bonded] distinguishes the two halves of the merged list: true = the box currently holds a
     * link key for this phone and can page it; false = the app remembers it but the box does not,
     * so it cannot be connected until it is paired again. Defaulted so existing construction sites
     * are unaffected.
     */
    data class DeviceInfo(
        val btMac: String,
        val name: String,
        val type: String,
        val lastConnected: String? = null,
        val rfcomm: String? = null,
        val bonded: Boolean = true,
    )

    /**
     * Callback interface for Carlink events.
     */
    interface Callback {
        fun onStateChanged(state: State)

        fun onStatusTextChanged(text: String)

        fun onHostUIPressed()

        /** Called when phone type becomes known or cleared. */
        fun onPhoneTypeChanged(phoneType: PhoneType) {}

        /** Called when the adapter's paired device list changes. */
        fun onDeviceListChanged(devices: List<DeviceInfo>) {}
    }

    /**
     * Listener for device management events. Unlike [Callback], multiple can be registered.
     */
    fun interface DeviceListener {
        fun onDeviceListChanged(devices: List<DeviceInfo>)
    }

    private val deviceListeners = mutableListOf<DeviceListener>()

    fun addDeviceListener(listener: DeviceListener) {
        synchronized(deviceListeners) { deviceListeners.add(listener) }
    }

    fun removeDeviceListener(listener: DeviceListener) {
        synchronized(deviceListeners) { deviceListeners.remove(listener) }
    }

    private fun notifyDeviceListeners() {
        val snapshot = synchronized(deviceListeners) { deviceListeners.toList() }
        snapshot.forEach { it.onDeviceListChanged(_pairedDevices) }
    }

    // Coroutine scope for async operations. SupervisorJob so one failed child cannot cancel the
    // whole scope — without it, every later launch silently no-ops and the session becomes a
    // permanent zombie. The handler logs instead of crashing.
    private val scopeExceptionHandler =
        CoroutineExceptionHandler { _, e ->
            logError("[SCOPE] Uncaught exception in session coroutine", tag = Logger.Tags.USB, throwable = e)
        }
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main + scopeExceptionHandler)

    // Serializes session lifecycle transitions (start/stop/restart/handleError). NOT reentrant —
    // internal callers already holding it must use the *Impl variants.
    private val lifecycleMutex = Mutex()

    // Set by release(); gates startImpl. Closes the zombie-start hole: an ATTACHED-intent start()
    // launched on the Activity's lifecycleScope could otherwise survive release() and drive the
    // released manager through a full connect, claiming USB from its replacement.
    @Volatile private var released = false

    private val currentState = AtomicReference(State.DISCONNECTED)
    val state: State get() = currentState.get()

    private var callback: Callback? = null

    // ---- OCBM session -----------------------------------------------------------------------
    // One transport + one client PER SESSION, by contract: OcbmClient.stop() is terminal and
    // UsbBulkTransport has no reopen, so reconnecting means constructing a fresh pair. Both are
    // cheap — neither holds a Context or native resources beyond the claimed interface.
    private var transport: UsbBulkTransport? = null
    private var client: OcbmClient? = null

    private val ocbmLog = ProbeLog.sub("ocbm")
    private val usbLog = ProbeLog.sub("usb")

    // Wake lock to prevent CPU sleep during streaming. PARTIAL keeps the CPU running but lets
    // the screen turn off.
    private val powerManager = context.getSystemService(Context.POWER_SERVICE) as PowerManager
    private val wakeLock: PowerManager.WakeLock =
        powerManager
            .newWakeLock(
                PowerManager.PARTIAL_WAKE_LOCK,
                "Carlink::UsbStreamingWakeLock",
            ).apply {
                // Non-reference-counted so the periodic refresh (a repeat acquire that restarts
                // the safety timeout) still fully releases on one release().
                setReferenceCounted(false)
            }

    // Renews the wake lock's safety timeout while a session is active; without it a continuous
    // session longer than the timeout silently lost the lock mid-drive.
    @Volatile private var wakeLockRefreshJob: Job? = null

    // ---- video ------------------------------------------------------------------------------

    /**
     * One video generation: a pipe, the renderer bound to a Surface, and the thread draining it.
     *
     * These three live and die together because [HevcRenderer] binds its `Surface` at
     * construction and its `consume` owns the codec for the life of the call. A new Surface
     * therefore means a new epoch, never a mutation of the old one.
     *
     * Note the asymmetry with audio, which is session-scoped: only video is surface-scoped, which
     * is exactly why `VideoSeam`'s pipe is swappable while the seam itself (holding the session's
     * decrypt key) is not.
     */
    private class VideoEpoch(
        val seq: Int,
        val pipe: SeamPipe,
        val renderer: HevcRenderer,
        val thread: Thread,
    )

    @Volatile private var videoEpoch: VideoEpoch? = null
    private val videoEpochSeq =
        java.util.concurrent.atomic
            .AtomicInteger(0)
    private var videoSurface: Surface? = null

    /** True when video start is deferred until a session exists and a valid Surface is present. */
    private var codecDeferred = true

    /** One-time [initialize] work (mic manager, MediaSession attach) has run. */
    private var initializedOnce = false

    /**
     * Who the connected phone is, from `CT_PHONE_IDENT` — the phone's own AirPlay SETUP plist.
     *
     * Keyed by lower-cased `deviceID` (the BR/EDR MAC), which is the join key against MGMT_INFO's
     * bonded list. Retained across the session so a device list rebuilt later still gets the name.
     */
    private val phoneNames = java.util.Collections.synchronizedMap(mutableMapOf<String, String>())

    /** `deviceID` of the phone currently in session, lower-cased, or null. */
    @Volatile private var connectedPhoneMac: String? = null

    /**
     * Durable known-device history. Survives process death; cleared only by the user or by clearing
     * app storage. This is what lets the list render before any adapter session exists.
     */
    private var knownStore: KnownDeviceStore? = null

    /**
     * The device the box should prefer when it initiates a reconnect, or null for
     * "follow most-recently-connected".
     *
     * Persisted here today; the box-side verb that acts on it is Phase 3. Storing it now is not
     * speculative — the ordering it drives is already visible in the list.
     */
    val preferredBtMac: String? get() = knownStore?.snapshot?.preferredMac

    /** Command verbs already logged as unhandled, so each is named once rather than per arrival. */
    private val unhandledCommands = java.util.Collections.synchronizedSet(mutableSetOf<String>())

    /** AAOS drive-state source. Null off automotive; see [DriveStateMonitor]. */
    private var driveMonitor: DriveStateMonitor? = null

    /**
     * This host session's identity on the wire, sent in every CT_HELLO this manager's clients make.
     *
     * Random per manager instance, never zero (0 is the wire's "not supplied"). It exists so the box
     * can tell a REPLACEMENT host from a reattaching one: a process killed without CT_STOP leaves the
     * box believing a host is present, and a relaunch inside the heartbeat grace keeps that belief
     * alive from the new process — so projection is never re-armed and the session comes up with no
     * A/V. Verified on hardware before the fix; see OCBMANDROID.md.
     */
    private val instanceNonce: Int =
        java.util.concurrent.ThreadLocalRandom
            .current()
            .nextInt()
            .let { if (it == 0) 1 else it }

    private var actualSurfaceWidth = 0
    private var actualSurfaceHeight = 0

    // ---- audio ------------------------------------------------------------------------------
    // Session-scoped (as in the reference implementation, where only the video renderer is
    // surface-scoped). AacPlayer holds a resting AUDIOFOCUS_GAIN so AAOS has an owner to return
    // to; VoiceRouter takes per-purpose transient focus with distinct listener identities,
    // because AAOS CarAudioFocus keys on the listener object.
    private var aacPlayer: AacPlayer? = null
    private var voiceRouter: VoiceRouter? = null

    // ---- microphone -------------------------------------------------------------------------
    private var microphoneManager: MicrophoneCaptureManager? = null

    @Volatile private var isMicrophoneCapturing = false

    // Negotiated by the box and delivered on CT_UPLINK. Never guessed: a wrong rate here is a
    // pitch-shifted uplink that Siri mishears with no error anywhere.
    @Volatile private var micRate = 16000

    @Volatile private var micChannels = 1

    // Dedicated single-thread scheduler for the 20 ms mic send cadence. Deliberately NOT
    // shared-pool coroutines (broke mic timing — revisions [19]/[21]). A single-thread executor
    // also serializes old/new tasks across format switches, and the generation token skips stale
    // queued runs entirely.
    private val micSendExecutor =
        java.util.concurrent
            .ScheduledThreadPoolExecutor(1) { r ->
                Thread({
                    android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_URGENT_AUDIO)
                    r.run()
                }, "MicSend")
            }.apply { removeOnCancelPolicy = true }
    private var micSendFuture: java.util.concurrent.ScheduledFuture<*>? = null

    private val micSendGeneration =
        java.util.concurrent.atomic
            .AtomicInteger(0)

    // ---- MediaSession -----------------------------------------------------------------------
    // Service-scope-owned. This manager only borrows access — [initialize] attaches the transport
    // callback, [release] detaches it. Resolved DYNAMICALLY: if the system restarts the MBS
    // mid-run, a cached reference dangles and every update silently early-returns.
    private val mediaSessionEnabled = injectedMediaSessionManager != null
    private val mediaSessionManager: MediaSessionManager?
        get() = if (mediaSessionEnabled && !released) MediaSessionManager.instance() else null

    private var attachedMediaControlCallback: MediaSessionManager.MediaControlCallback? = null

    private var frameIntervalJob: Job? = null

    /** Current phone type. Null when no phone is connected. */
    @Volatile var currentPhoneType: PhoneType? = null
        private set

    // ---- reconnect / escalation -------------------------------------------------------------
    @Volatile private var reconnectJob: Job? = null
    private var reconnectAttempts: Int = 0

    private var hadPriorSession: Boolean = false
    private var consecutiveNoResponse: Int = 0
    private var shortLivedStreamingCount: Int = 0
    private var lastStreamingStartMs: Long = 0L

    // ---- surface debounce -------------------------------------------------------------------
    private var surfaceUpdateJob: Job? = null
    private var pendingSurface: Surface? = null
    private var pendingSurfaceWidth: Int = 0
    private var pendingSurfaceHeight: Int = 0
    private var pendingCallback: Callback? = null

    // ---- media metadata ---------------------------------------------------------------------
    private var lastMediaSongName: String? = null
    private var lastMediaArtistName: String? = null
    private var lastMediaAlbumName: String? = null
    private var lastMediaAppName: String? = null
    private var lastAlbumCover: ByteArray? = null
    private var lastDuration: Long = 0L
    private var lastPosition: Long = 0L
    private var lastIsPlaying: Boolean = true

    /**
     * Album art arrives out of band from the track that references it, so it is cached by id
     * until the two meet.
     *
     * Match on `artworkId and 0xFF`: the wire id is ONE byte while `nowPlaying.artworkId` is the
     * full iAP2 integer. A full-int comparison silently dropped every artwork with an id ≥ 256 —
     * a bug already paid for once on the macOS host.
     */
    private val artworkById = HashMap<Int, ByteArray>()

    @Volatile private var pendingArtworkId: Int? = null

    // ---- device list ------------------------------------------------------------------------
    @Volatile private var _pairedDevices: List<DeviceInfo> = emptyList()

    private val deviceListLock = Any()

    // MAC -> elapsedRealtime expiry. Forgetting a device restarts the box's wireless stack, and
    // its device list during that window still contains the device; suppress the MAC from merges
    // until expiry or the card resurrects seconds after the user removed it.
    private val recentlyForgotten = mutableMapOf<String, Long>()

    /**
     * Atomically mutate the device list under [deviceListLock], filtering out recently-forgotten
     * MACs. Returns the committed list.
     */
    private fun mutateDeviceList(mutation: (List<DeviceInfo>) -> List<DeviceInfo>): List<DeviceInfo> {
        synchronized(deviceListLock) {
            val now = SystemClock.elapsedRealtime()
            recentlyForgotten.entries.removeIf { it.value <= now }
            var next = mutation(_pairedDevices)
            if (recentlyForgotten.isNotEmpty()) {
                next = next.filter { it.btMac !in recentlyForgotten }
            }
            _pairedDevices = next
            return next
        }
    }

    /**
     * MACs the box currently holds a bond for, from the last `MGMT_INFO`.
     *
     * Kept SEPARATE from [_pairedDevices] on purpose. That list now also carries history-only
     * entries, so any membership question asked of it would be wrong — most sharply the
     * single-bonded-device heuristic below, which would see two entries for a user with one phone
     * and silently stop marking it Connected.
     */
    @Volatile private var bondedMacs: Set<String> = emptySet()

    @Volatile private var _connectedBtMac: String? = null

    /** The adapter's list of paired wireless devices. */
    val pairedDevices: List<DeviceInfo> get() = _pairedDevices

    /** BT MAC of the currently connected phone (null if none). */
    val connectedBtMac: String? get() = _connectedBtMac

    /** 0 = wired, 1 = wireless, null = unknown. */
    @Volatile var currentWifi: Int? = null
        private set

    /** Clears cached media metadata to prevent stale data on reconnect. */
    private fun clearCachedMediaMetadata() {
        lastMediaSongName = null
        lastMediaArtistName = null
        lastMediaAlbumName = null
        lastMediaAppName = null
        lastAlbumCover = null
        lastDuration = 0L
        lastPosition = 0L
        lastIsPlaying = true
        synchronized(artworkById) { artworkById.clear() }
        pendingArtworkId = null
        _connectedBtMac = null
        currentWifi = null
    }

    // LogCallback for Java/legacy components — routes to Logger with proper tags
    private val logCallback =
        object : LogCallback {
            override fun log(message: String) {
                this@CarlinkManager.log(message)
            }

            override fun log(
                tag: String,
                message: String,
            ) {
                Logger.d(message, tag)
            }

            override fun logPerf(
                tag: String,
                message: String,
            ) {
                if (Logger.isDebugLoggingEnabled() && Logger.isTagEnabled(tag)) {
                    Logger.d(message, tag)
                }
            }
        }

    /**
     * Initialize the manager with a Surface and actual surface dimensions.
     *
     * Preconditions and call contract:
     * - Must be called from the main thread.
     * - Safe to call repeatedly (size changes go through a 150 ms debounce).
     * - The callback reference is retained until the next initialize() call or [release].
     * - The FIRST call bypasses the debounce; only subsequent calls coalesce.
     */
    fun initialize(
        surface: Surface,
        surfaceWidth: Int,
        surfaceHeight: Int,
        callback: Callback,
    ) {
        // Round to even — 4:2:0 chroma subsampling cannot express an odd dimension.
        val evenWidth = surfaceWidth and 1.inv()
        val evenHeight = surfaceHeight and 1.inv()

        actualSurfaceWidth = evenWidth
        actualSurfaceHeight = evenHeight

        // Config resolution was pre-computed from stable WindowMetrics in MainActivity. Do NOT
        // override with surface dimensions — SurfaceView size oscillates during Compose layout
        // because systemBars insets apply asynchronously on AAOS.
        logInfo(
            "[RES] Using resolution ${config.width}x${config.height} (surface: ${evenWidth}x$evenHeight)",
            tag = Logger.Tags.VIDEO,
        )

        // Do NOT use reference equality to decide the Surface is "the same". After the app goes
        // to background the Java object may be identical while the native BufferQueue has been
        // destroyed and recreated — rendering into it yields "BufferQueue has been abandoned".
        // Keyed on one-time setup having run, NOT on a live decoder. Under riddleBox the renderer
        // existed from the first call so this branch was equivalent — but the epoch is now only
        // created once a SESSION exists, and MainScreen re-calls initialize() on every surface or
        // container-size change. Without this flag every pre-session call would redo the one-time
        // work below, rebuilding the mic manager and re-attaching the MediaSession callback.
        if (initializedOnce) {
            pendingSurface = surface
            pendingSurfaceWidth = evenWidth
            pendingSurfaceHeight = evenHeight
            pendingCallback = callback

            surfaceUpdateJob?.cancel()
            surfaceUpdateJob =
                scope.launch {
                    delay(SURFACE_DEBOUNCE_MS)

                    val finalSurface = pendingSurface ?: return@launch
                    val finalCallback = pendingCallback ?: return@launch

                    logInfo(
                        "[LIFECYCLE] Surface stabilized at ${pendingSurfaceWidth}x$pendingSurfaceHeight",
                        tag = Logger.Tags.VIDEO,
                    )

                    // Persistent-divergence detector: the decoder runs at the pushed config while
                    // these are the live UI dimensions. A mismatch at first init is an expected
                    // startup transient the debounce coalesces away — but one that SURVIVES the
                    // debounce means the on-screen area genuinely disagrees with the projection
                    // area, i.e. a visible scale/crop mismatch.
                    if (pendingSurfaceWidth != config.width || pendingSurfaceHeight != config.height) {
                        logWarn(
                            "[RES] Surface stabilized at ${pendingSurfaceWidth}x$pendingSurfaceHeight " +
                                "but the decoder is ${config.width}x${config.height} — persistent divergence " +
                                "(Δ=${config.width - pendingSurfaceWidth}x${config.height - pendingSurfaceHeight})",
                            tag = Logger.Tags.VIDEO,
                        )
                    }

                    this@CarlinkManager.callback = finalCallback
                    this@CarlinkManager.videoSurface = finalSurface

                    // A new Surface is a new epoch: retire the old one first so its codec is
                    // released before a second is configured. This VPU's codec pool is small.
                    retireVideoEpoch("surface changed")
                    if (client?.lanes != null) openVideoEpoch(finalSurface)
                }
            return
        }

        this.callback = callback
        this.videoSurface = surface
        initializedOnce = true

        logInfo(
            "[RES] Initializing with surface ${evenWidth}x$evenHeight @ ${config.fps}fps, ${config.dpi}dpi",
            tag = Logger.Tags.VIDEO,
        )

        // Video starts when a session exists AND a Surface is valid; both orders happen.
        codecDeferred = true

        // Load the remembered devices. On IO because it reads a file, and NOT under lifecycleMutex
        // so it cannot serialise against start(). This is what makes the list appear before any
        // adapter session exists — the requirement's "shouldn't wait to populate until a device
        // connects".
        knownStore =
            KnownDeviceStore(context).also { store ->
                scope.launch(Dispatchers.IO) {
                    val snap = store.load()
                    // Seed the in-memory name cache so a session that starts before the first
                    // MGMT_INFO still labels its phone.
                    snap.devices.values.forEach { d -> d.name?.let { phoneNames[d.mac] = it } }
                    withContext(Dispatchers.Main) { rebuildDeviceList() }
                }
            }

        // Drive state is vehicle state, not UI state, so it is owned here rather than in Compose
        // (unlike night mode, which rides isSystemInDarkTheme). The monitor primes itself on
        // connect, and setLimitedUI no-ops until a client exists, so an early edge is not lost —
        // onSessionKeyed re-pushes the current value.
        driveMonitor =
            DriveStateMonitor(context).also { m ->
                m.onDriveStateChanged = { driving -> setLimitedUI(driving) }
                m.start()
            }

        microphoneManager =
            MicrophoneCaptureManager(context, logCallback).also { mgr ->
                // Wiring this is NOT optional. MicrophoneCaptureManager invokes it and nothing
                // else clears our flag, so without it a fatal AudioRecord error leaves the
                // pipeline dead-but-"capturing": isMicrophoneCapturing stays true, the 20 ms
                // timer keeps pushing, readChunk() returns null forever, and the next gate-on
                // early-returns — Siri or a whole phone call silent until the box cycles the
                // gate. Its contract forbids a synchronous stop() (that would join the calling
                // thread), hence the scope hop.
                mgr.onCaptureError = { why ->
                    scope.launch {
                        logWarn("[MIC] capture died: $why — clearing state", tag = Logger.Tags.AUDIO)
                        stopMicrophoneCapture()
                    }
                }
            }

        // MediaSession lifetime is owned by MainActivity. We only attach our transport-control
        // callback so routed Play/Pause/Skip reach the box. Never construct a MediaSessionManager
        // here — that bypasses the app-scope ownership invariant and resurrects the
        // blank-homescreen-card bug.
        val session = mediaSessionManager
        if (session != null) {
            val transportCallback =
                object : MediaSessionManager.MediaControlCallback {
                    override fun onPlay() {
                        sendMediaButton(Ocbm.MEDIA_BTN_PLAY)
                    }

                    override fun onPause() {
                        sendMediaButton(Ocbm.MEDIA_BTN_PAUSE)
                    }

                    override fun onStop() {
                        sendMediaButton(Ocbm.MEDIA_BTN_PAUSE)
                    }

                    override fun onSkipToNext() {
                        sendMediaButton(Ocbm.MEDIA_BTN_NEXT)
                    }

                    override fun onSkipToPrevious() {
                        sendMediaButton(Ocbm.MEDIA_BTN_PREV)
                    }
                }
            attachedMediaControlCallback = transportCallback
            session.setMediaControlCallback(transportCallback)
            logInfo("MediaSession transport callback attached", tag = Logger.Tags.ADAPTR)
        } else {
            logInfo("MediaSession not provided (owner skipped)", tag = Logger.Tags.ADAPTR)
        }

        logInfo("CarlinkManager initialized (OCBM)", tag = Logger.Tags.ADAPTR)
    }

    /**
     * Start the OCBM session.
     *
     * Runs under [lifecycleMutex] on Dispatchers.IO: the body performs blocking USB opens and
     * writes. Main-safe to call from anywhere.
     */
    suspend fun start() =
        lifecycleMutex.withLock {
            withContext(Dispatchers.IO) { startImpl() }
        }

    /**
     * Find the adapter and claim its USB interface, or null if we should abandon the start.
     *
     * Split out of [startImpl] so the release re-checks below do not push it over detekt's
     * return-count limit, and because "get me a claimed transport" is a coherent step on its own.
     *
     * The [released] re-checks are the point of this function. [findDevice] can burn ~80 s worst
     * case (10 x 3 s scans plus 20 x 250 ms permission polls), and `release()` runs OUTSIDE
     * [lifecycleMutex], so the flag can flip under an in-flight start. Without them a released
     * manager goes on to claim the interface and bring up a full session — a zombie holding the
     * claim against its own replacement, whose start then fails with a claim error and, at
     * attempt 0, schedules no retry. `release()` also cancels [scope], so the zombie's state
     * posts silently no-op and nothing on screen explains it.
     */
    private suspend fun acquireTransport(): UsbBulkTransport? {
        val t = UsbBulkTransport(context, usbLog)

        val dev = findDevice(t)
        if (dev == null) {
            logError("Failed to find the OCBM adapter", tag = Logger.Tags.USB)
            handleFailedStart("Adapter not found")
            return null
        }
        if (released) {
            logWarn("[LIFECYCLE] released during device discovery — abandoning start", tag = Logger.Tags.ADAPTR)
            return null
        }

        setStatusText("Adapter found — opening...")
        if (!t.open(dev)) {
            logError("Failed to claim the adapter's USB interface", tag = Logger.Tags.USB)
            handleFailedStart("USB permission denied")
            return null
        }
        if (released) {
            logWarn("[LIFECYCLE] released during USB open — closing and abandoning", tag = Logger.Tags.ADAPTR)
            t.stop()
            return null
        }

        return t
    }

    private suspend fun startImpl() {
        if (released) {
            logWarn("[LIFECYCLE] start() on a released manager — ignoring", tag = Logger.Tags.ADAPTR)
            return
        }

        // Tear down any existing session BEFORE entering CONNECTING, so the FGS started at
        // CONNECTING isn't immediately stopped by the teardown's DISCONNECTED transition.
        if (client != null) stopImpl()

        setState(State.CONNECTING)
        setStatusText("Searching for adapter...")

        val t = acquireTransport() ?: return

        transport = t
        val c = OcbmClient(t, ocbmLog)
        // Stable for this manager's whole life, NOT per client: a reattach after a USB blip is the same
        // host and should warm-reuse the box's live session, whereas a new process must force a re-arm.
        c.instanceNonce = instanceNonce
        client = c
        wireClientCallbacks(c)
        c.start()

        // CT_HELLO, retransmitted until ACK. Failure here is Pattern A's trigger — keep the
        // "no initial response" substring, which remote log filters match on.
        setStatusText("Connecting to adapter...")
        if (!c.hello()) {
            handleError("no initial response — no CT_HELLO_ACK from the adapter")
            return
        }

        // The box has no RTC battery, so its clock is bogus at every boot and CarPlay's TLS
        // pairing needs a real one. Non-fatal: a wrong clock degrades pairing, a missing
        // SETTIME ack does not mean the link is bad.
        c.setTime()

        // THE APP IS THE IGNITION: this is the presence latch that brings the box's radios up.
        setStatusText("Starting projection...")
        if (!c.subscribe(VehicleConfigYaml.renderBytes(vehicleConfigSpec()))) {
            handleError("usb — CT_SUBSCRIBE write failed")
            return
        }
        c.startHeartbeat()

        setStatusText("Waiting for phone...")
    }

    /**
     * The config document pushed on SUBSCRIBE.
     *
     * Derived from [config] so the decoder, the box's `/info` advertisement and the coordinate
     * space the box scales touch into all come from ONE number and cannot drift.
     */
    private fun vehicleConfigSpec(): VehicleConfigSpec =
        VehicleConfigSpec(
            name = config.boxName,
            width = config.width,
            height = config.height,
            maxFps = if (config.fps >= 60) 60 else 30,
            // The OEM icon is the CarPlay home-screen tile that returns the driver here; its tap
            // comes back as an inbound `requestUI` (see onCommandPlist). Rendered once and cached:
            // it is three PNG encodes, and this runs on the connect path.
            oemIconImages = oemIconImages,
            oemIconLabel = context.getString(R.string.app_name),
        )

    /** Lazily rendered once per manager; the launcher icon cannot change under a running process. */
    private val oemIconImages: List<OemIcon.Image> by lazy { OemIcon.render(context) }

    /**
     * Stop and disconnect.
     *
     * @param reboot when true, also asks the box to reboot. Expect ~50 s of device absence and a
     *   fresh Android USB permission grant afterwards; grants are per-attachment.
     */
    fun stop(reboot: Boolean = false) {
        // Synchronous by contract (release()/onDestroy needs teardown before returning).
        stopImpl(reboot)
    }

    /**
     * @param preserveEscalation keep Pattern A/C context across the stop. Adapter-initiated
     *   restarts are exactly the "phone left and may not return" scenario the escalation exists
     *   to diagnose; user-initiated stops keep the full reset.
     */
    private fun stopImpl(
        reboot: Boolean = false,
        preserveEscalation: Boolean = false,
    ) {
        logDebug("[LIFECYCLE] stop()", tag = Logger.Tags.VIDEO)
        cancelDelayedKeyframe()
        cancelReconnect()
        // Always clear the streaming-start timestamp: with preserveEscalation a stale value
        // would let the NEXT error measure "session duration" from the PREVIOUS session's start,
        // producing phantom Pattern C increments for attempts that never reached STREAMING.
        lastStreamingStartMs = 0L
        if (!preserveEscalation) {
            hadPriorSession = false
            consecutiveNoResponse = 0
            shortLivedStreamingCount = 0
        }
        currentPhoneType = null
        currentWifi = null
        codecDeferred = true
        callback?.onPhoneTypeChanged(PhoneType.UNKNOWN)
        clearCachedMediaMetadata()
        stopMicrophoneCapture()

        if (reboot) {
            // Manual verb only, never automated: the device disappears for ~50 s and
            // re-enumerates, and Android's USB permission grant does not survive that.
            logWarn("[LIFECYCLE] MGMT_REBOOT requested", tag = Logger.Tags.ADAPTR)
            runCatching { client?.mgmtAction(Ocbm.MGMT_REBOOT) }
        }

        retireVideoEpoch("session stopping")
        releaseAudio()

        // CT_STOP gives the box a 5 s warm grace, so a quick relaunch reuses the session rather
        // than re-running the whole handshake.
        client?.stop()
        client = null
        transport?.stop()
        transport = null

        setState(State.DISCONNECTED)
    }

    /**
     * End the phone's session without stopping the adapter.
     *
     * DEGRADED: OCBM has no per-phone disconnect verb, so this bounces the box's wireless stack,
     * which drops whatever phone is attached. The status text says so rather than pretending.
     */
    fun disconnectPhone() {
        logInfo("[LIFECYCLE] disconnectPhone() — restarting wireless (no per-phone verb exists)")
        setStatusText("Restarting wireless...")
        scope.launch(Dispatchers.IO) { client?.mgmtAction(Ocbm.MGMT_RESTART_WIRELESS) }
    }

    /**
     * Forward the AAOS day/night state to CarPlay mid-session, so CarPlay's own UI follows the
     * head unit's theme.
     *
     * `setNightMode` is global and advisory — one input alongside iOS's own appearance logic. The
     * explicit per-display commands are sent too, so the flip is deterministic rather than
     * depending on the user's iPhone-side "Automatic" preference.
     */
    fun setNightMode(night: Boolean) {
        logInfo("[NIGHT] AAOS night=$night", tag = Logger.Tags.ADAPTR)
        val c = client ?: return
        scope.launch(Dispatchers.IO) {
            c.sendNightMode(night)
            c.sendAppearance(dark = night, isMap = false)
            c.sendAppearance(dark = night, isMap = true)
        }
    }

    /**
     * Restrict the CarPlay UI as if the vehicle were in Drive, or release it.
     *
     * The box turns this into `/command setLimitedUI {limitedUI: <bool>}`
     * (`airplayd/src/main.rs:1065-1066`), which is a runtime command — no reconnect, no `/info`
     * change. iOS hides the on-screen keyboard, the phone keypad and long scrollable lists.
     *
     * WHICH elements get limited is the pushed config's `limitedUIConfig`; this is only the
     * on/off. We push no `limitedUIConfig` today, so iOS applies its own defaults.
     */
    fun setLimitedUI(limited: Boolean) {
        logInfo("[DRIVE] limitedUI=$limited", tag = Logger.Tags.ADAPTR)
        val c = client ?: return
        scope.launch(Dispatchers.IO) {
            c.sendCommand(if (limited) Ocbm.CMD_LIMITED_UI_ON else Ocbm.CMD_LIMITED_UI_OFF)
        }
    }

    // ==================== Device Management ====================

    /**
     * Ask the box for a fresh device snapshot.
     *
     * Blocking with a 5 s timeout box-side, so never called from a property getter — `PhonesTab`
     * polls those at 1 Hz.
     */
    fun refreshDeviceList() {
        logInfo("[DEVICE_MGMT] Requesting device snapshot", tag = Logger.Tags.ADAPTR)
        scope.launch(Dispatchers.IO) { pollBoxInfo() }
    }

    private fun pollBoxInfo() {
        val json = client?.mgmtGetInfo() ?: return
        val info = runCatching { JSONObject(json) }.getOrNull() ?: return

        val macs = info.optJSONArray("devices")
        if (macs != null) {
            val bonded =
                (0 until macs.length())
                    .mapNotNull { i -> macs.optString(i).takeIf { it.isNotEmpty() }?.lowercase() }
                    .toSet()
            bondedMacs = bonded

            // Learn any bond we have never seen — a fresh install over existing bonds, or app data
            // cleared while the box kept its link keys. It shows as a bare MAC until the phone next
            // connects and CT_PHONE_IDENT names it. Suppressed MACs are deliberately NOT learned:
            // the box's snapshot lags its own forget, and persisting one here would defeat the
            // suppression window permanently.
            val suppressed = synchronized(deviceListLock) { recentlyForgotten.keys.toSet() }
            val unknown = bonded - (knownStore?.snapshot?.devices?.keys ?: emptySet()) - suppressed
            if (unknown.isNotEmpty()) {
                val now = System.currentTimeMillis()
                knownStore?.update { snap ->
                    snap.copy(devices = snap.devices + unknown.associateWith { KnownDevice(mac = it, firstSeenMs = now) })
                }
            }
            rebuildDeviceList()
        }

        val wireless = info.optString("transport") == "wireless"
        val phonePresent = info.optBoolean("phone_present", false)
        currentWifi =
            if (wireless) {
                1
            } else if (phonePresent) {
                0
            } else {
                null
            }

        // `CT_PHONE_IDENT` names the live phone outright, so prefer it. The heuristic below is
        // the fallback for a box that has not sent one yet.
        connectedPhoneMac?.let { _connectedBtMac = it }

        // Single-bonded-device heuristic: MGMT_INFO reports no connected MAC, so with two or more
        // BONDED phones the "Connected" card will be wrong. Asked of `bondedMacs`, never of
        // `_pairedDevices` — the latter now includes history-only entries, which would make
        // `singleOrNull()` null for a one-phone user and silently kill the highlight.
        if (_connectedBtMac == null && (wireless || phonePresent)) {
            bondedMacs.singleOrNull()?.let { _connectedBtMac = it }
        }
    }

    /**
     * Connect to a specific paired device.
     *
     * DEGRADED: there is no targeted-connect verb in OCBM. The best available action is to bounce
     * wireless and let the box's own reconnect loop pick up whichever bonded phone is in range —
     * which is the right answer with one paired phone and wrong with several.
     */
    fun connectToDevice(btMac: String) {
        logInfo("[DEVICE_MGMT] Connect requested for $btMac — no targeted-connect verb; restarting wireless", tag = Logger.Tags.ADAPTR)
        setStatusText("Restarting wireless...")
        scope.launch(Dispatchers.IO) { client?.mgmtAction(Ocbm.MGMT_RESTART_WIRELESS) }
    }

    /**
     * Remove a device from the box's bond store.
     *
     * The box restarts wireless as part of the forget, which drops a live session.
     */
    fun forgetDevice(btMac: String) {
        logInfo("[DEVICE_MGMT] Forget device: $btMac", tag = Logger.Tags.ADAPTR)

        // Optimistically remove for a responsive UI, and suppress the MAC from merges while the
        // box processes the forget — its snapshot still contains the device for a while.
        val mac = btMac.lowercase()
        synchronized(deviceListLock) {
            recentlyForgotten[mac] = SystemClock.elapsedRealtime() + FORGET_SUPPRESS_MS
        }
        // Delete the remembered record in the same breath. `recentlyForgotten` is in-memory and
        // elapsedRealtime-based, so it cannot cover storage: without this the device returns when
        // the window expires, or on the next app launch.
        knownStore?.update { snap ->
            snap.copy(
                devices = snap.devices - mac,
                preferredMac = snap.preferredMac?.takeIf { it != mac },
            )
        }
        bondedMacs = bondedMacs - mac
        mutateDeviceList { list -> list.filter { it.btMac.lowercase() != mac } }
        callback?.onDeviceListChanged(_pairedDevices)
        notifyDeviceListeners()

        scope.launch(Dispatchers.IO) {
            val status = client?.mgmtAction(Ocbm.MGMT_FORGET_DEVICE, btMac.toByteArray(Charsets.US_ASCII))
            logInfo("[DEVICE_MGMT] FORGET_DEVICE($btMac) status=$status", tag = Logger.Tags.ADAPTR)
            delay(1000)
            pollBoxInfo()
        }
    }

    /**
     * Restart the connection: STOP, settle, then a fresh transport + client.
     *
     * A re-subscribe inside the box's 5 s grace REUSES the session, so this is usually seconds to
     * pixels rather than a full re-handshake.
     */
    suspend fun restart() {
        // Called only by the UI's Reset Connection. There is deliberately no INTERNAL trigger any
        // more: riddleBox restarted the session from UNPLUGGED / Phase-0, whereas the box now owns
        // phone departure (it holds the session and waits) and a dead link goes through
        // handleError -> scheduleReconnect. A single-flight guard is therefore unnecessary.
        setStatusText("Restarting...")
        lifecycleMutex.withLock {
            withContext(Dispatchers.IO) { stopImpl(preserveEscalation = true) }
        }
        delay(2000)
        start()
    }

    private fun sendMediaButton(index: Byte) {
        val c = client ?: return
        scope.launch(Dispatchers.IO) { c.sendMediaButton(index) }
    }

    /**
     * Send a multi-touch event.
     *
     * Signature preserved so `MainScreen.handleTouchEvent` is untouched, including its deadband,
     * ACTION_CANCEL UP-synthesis and DOWN→MOVE demotion.
     *
     * **Two contacts, because that is exactly what Apple's descriptor holds.** The box now
     * advertises `HIDTouchScreenMultiCreateDescriptor`, which declares two `Finger` collections —
     * enough for pinch, zoom and rotate. Pointers beyond the first two are dropped rather than
     * remapped: `airplayd` would have to evict a live contact to make room, which breaks the
     * gesture already in progress.
     *
     * Ordering by [MessageSerializer.TouchPoint.id] keeps a given finger on a stable slot for the
     * life of the gesture. That matters because the box routes by the id we send here, and a
     * contact that changed slot mid-pinch would read as a jump.
     *
     * One `INPUT_TOUCH` frame goes out per pointer; the box coalesces them into the single
     * two-finger HID report the descriptor requires.
     */
    fun sendMultiTouch(touches: List<MessageSerializer.TouchPoint>) {
        val c = client ?: return
        if (touches.isEmpty()) return
        for (t in touches.sortedBy { it.id }.take(MAX_CONTACTS)) {
            val phase =
                when (t.action) {
                    MultiTouchAction.DOWN -> Ocbm.TOUCH_DOWN
                    MultiTouchAction.MOVE -> Ocbm.TOUCH_MOVE
                    MultiTouchAction.UP -> Ocbm.TOUCH_UP
                    else -> continue
                }
            // Non-blocking by construction (the client queues), so no dispatch hop is needed and the
            // touches keep their ordering with respect to each other and to earlier frames.
            c.sendTouch(phase, t.x, t.y, finger = t.id)
        }
    }

    suspend fun rebootAdapter() =
        lifecycleMutex.withLock {
            withContext(Dispatchers.IO) { rebootAdapterImpl() }
        }

    private fun rebootAdapterImpl() {
        // Escalation counters are deliberately preserved: reboot is user-initiated recovery, not
        // a clean-slate reset, so a user who reboots after several short sessions still gets the
        // right escalation messaging on the next attempt.
        logWarn("[LIFECYCLE] Reboot adapter requested (~50 s absence expected)", tag = Logger.Tags.ADAPTR)
        cancelReconnect()
        cancelDelayedKeyframe()
        stopMicrophoneCapture()

        runCatching { client?.mgmtAction(Ocbm.MGMT_REBOOT) }

        retireVideoEpoch("adapter rebooting")
        releaseAudio()
        client?.stop()
        client = null
        transport?.stop()
        transport = null

        codecDeferred = true
        currentPhoneType = null
        currentWifi = null
        callback?.onPhoneTypeChanged(PhoneType.UNKNOWN)
        clearCachedMediaMetadata()
        setState(State.DISCONNECTED)
        setStatusText("Adapter rebooting (~50 s)...")
    }

    /**
     * Release all resources. IRREVERSIBLE — a new instance is required afterwards.
     */
    fun release() {
        // Gate future starts FIRST (covers callers on foreign scopes this manager cannot cancel),
        // then cancel the scope so in-flight coroutines die at their next suspension point rather
        // than racing the synchronous teardown below.
        released = true
        scope.cancel()

        stop()

        releaseAudio()

        driveMonitor?.stop()
        driveMonitor = null

        // Every mutation is written through, so this only drains a queued last delta — the history
        // itself is already on disk.
        knownStore?.close()
        knownStore = null

        microphoneManager?.stop()
        microphoneManager = null
        micSendExecutor.shutdownNow()

        // Unconditional, because the only other callers of these two sit inside setState's
        // DISCONNECTED branch and BOTH ways in can be swallowed: setState dispatches through
        // `scope` when off-main and scope.cancel() above kills that post, while a teardown that
        // already reached DISCONNECTED makes the later setState a no-op (oldState == newState).
        // Losing the race leaked a partial wake lock until its 2 h timeout plus an FGS
        // notification outliving the manager.
        releaseWakeLock()
        CarlinkMediaBrowserService.stopConnectionForeground(context)

        // MediaSession is service-scope owned — do NOT release it here. Detaching our callback is
        // sufficient and necessary: releasing the session fires onSessionDestroyed on every bound
        // controller, and CarLauncher does not rebind to a new token.
        //
        // Direct instance() call, not the gated getter: `released` is already true, so the getter
        // returns null and the detach would silently no-op. Compare-and-clear so we never clobber
        // a callback a newer manager already attached.
        attachedMediaControlCallback?.let { mine ->
            MediaSessionManager.clearMediaControlCallbackIf(mine)
        }
        attachedMediaControlCallback = null

        logInfo("CarlinkManager released", tag = Logger.Tags.ADAPTR)
    }

    /**
     * Handle USB device detachment, for immediate detection of a physical unplug rather than
     * waiting for transfer errors.
     */
    fun onUsbDeviceDetached() {
        logWarn("[USB] Device detached broadcast received", tag = Logger.Tags.USB)
        if (state == State.DISCONNECTED) {
            logInfo("[USB] Already disconnected, ignoring detach", tag = Logger.Tags.USB)
            return
        }
        handleError("USB device physically disconnected")
    }

    // ==================== Video lifecycle ====================

    /**
     * Open a video epoch on [surface].
     *
     * Ordering is load-bearing: the renderer must be constructed and started before the seam is
     * pointed at its pipe, or the first frames land in a pipe nobody is draining.
     */
    @Synchronized
    private fun openVideoEpoch(surface: Surface) {
        val lanes = client?.lanes ?: return
        if (videoEpoch != null) return
        if (!surface.isValid) {
            logWarn("[LIFECYCLE] Video epoch skipped — surface invalid", tag = Logger.Tags.VIDEO)
            return
        }

        val seq = videoEpochSeq.incrementAndGet()
        val pipe = SeamPipe(VIDEO_PIPE_BYTES, VIDEO_PIPE_DEPTH)
        val renderer =
            HevcRenderer(config.width, config.height, surface) {
                client?.requestKeyframe()
            }
        renderer.start()
        val thread = lanes.runConsumer("cp-video", pipe) { renderer.consume(it) }
        lanes.videoSeam.attach(pipe)

        videoEpoch = VideoEpoch(seq, pipe, renderer, thread)
        codecDeferred = false

        // Every epoch MUST ask for one, and it has to happen here rather than at the call sites.
        // A fresh HevcRenderer has an empty parameter-set cache, and VideoSeam emits the
        // opcode-1 VideoConfig exactly once per session — attach() re-emits nothing. So a
        // mid-session epoch sees only frame AUs, and it cannot ask for a keyframe itself:
        // handleMessage returns at `if (!configured)` BEFORE reaching its own !sawKeyframe
        // request. VideoSeam.onGap cannot cover it either, because frames keep decrypting and
        // seq keeps advancing whether or not a pipe is attached, so there is no gap to detect.
        // Under gm_ccpa this was free — attaching a renderer dropped the seam socket and the box
        // force-keyframed on re-dial. A virtual pipe is invisible to the box, so we ask.
        // Throttled at 500 ms client-side, so the paths that already request are unharmed.
        client?.requestKeyframe()

        logInfo("[LIFECYCLE] Video epoch #$seq open at ${config.width}x${config.height}", tag = Logger.Tags.VIDEO)
    }

    /**
     * Retire the current video epoch.
     *
     * The join is NOT optional: without it a second decoder is configured while the first is
     * still draining, and this Intel VPU's codec pool is small enough that exhausting it leaves a
     * permanently black screen.
     */
    @Synchronized
    private fun retireVideoEpoch(why: String) {
        val e = videoEpoch ?: return
        videoEpoch = null
        codecDeferred = true
        logInfo("[LIFECYCLE] Retiring video epoch #${e.seq} — $why", tag = Logger.Tags.VIDEO)

        // Detach first so no further frames enter a pipe we are closing, then close to give the
        // consumer EOF, then flag the renderer, then wait for the codec to actually be released.
        client?.lanes?.videoSeam?.attach(null)
        e.pipe.close()
        e.renderer.stop()
        runCatching { e.thread.join(1500) }
        if (e.thread.isAlive) {
            logWarn("[LIFECYCLE] Video consumer #${e.seq} did not exit within 1500 ms", tag = Logger.Tags.VIDEO)
        }
    }

    /**
     * Rebuild the decoder without dropping the session.
     */
    fun resetVideoDecoder() {
        logInfo("[DEVICE_OPS] Resetting the video decoder", tag = Logger.Tags.VIDEO)
        val surface = videoSurface ?: return
        retireVideoEpoch("decoder reset requested")
        openVideoEpoch(surface)
    }

    /**
     * Handle Surface destruction — retire the epoch IMMEDIATELY.
     *
     * Called when SurfaceView's Surface is destroyed, which happens BEFORE onStop(). Waiting
     * would leave the codec rendering into a dead surface.
     *
     * Idempotent, and it must stay that way: `VideoSurface` can fire this twice per teardown
     * (DisposableEffect onDispose plus the SurfaceHolder callback) and designates this method as
     * the sole de-dup point.
     */
    fun onSurfaceDestroyed() {
        logInfo("[LIFECYCLE] Surface destroyed — retiring video epoch", tag = Logger.Tags.VIDEO)
        surfaceUpdateJob?.cancel()
        surfaceUpdateJob = null
        pendingSurface = null
        videoSurface = null
        retireVideoEpoch("surface destroyed")
    }

    /**
     * Start video if it was deferred — i.e. a session exists and a valid Surface has arrived.
     * Both orders happen, so both sides call this.
     */
    fun startCodecIfDeferred() {
        if (!codecDeferred) return
        val surface = videoSurface
        if (surface == null || !surface.isValid) {
            logWarn("[LIFECYCLE] Deferred video start skipped — no valid surface", tag = Logger.Tags.VIDEO)
            return // stays deferred; the debounce or resumeVideo() will retry
        }
        if (client?.lanes == null) {
            logDebug("[LIFECYCLE] Deferred video start skipped — no session yet", tag = Logger.Tags.VIDEO)
            return
        }
        openVideoEpoch(surface)
    }

    /**
     * Pause video when the app goes to background.
     *
     * Retiring the epoch is also the DROP POLICY, and that is the point: `SeamPipe.write` blocks
     * by design, so a paused-but-open pipe would backpressure the OCBM read thread and stall
     * audio and control traffic too. A closed pipe returns false immediately and counts.
     */
    fun pauseVideo() {
        logInfo("[LIFECYCLE] Pausing video for background", tag = Logger.Tags.VIDEO)
        cancelDelayedKeyframe()
        retireVideoEpoch("app backgrounded")
    }

    /**
     * Resume video when the app returns to foreground.
     */
    fun resumeVideo() {
        logInfo("[LIFECYCLE] Resuming video for foreground", tag = Logger.Tags.VIDEO)
        val surface = videoSurface
        if (surface == null || !surface.isValid) {
            logInfo("[LIFECYCLE] Surface not ready — resume will happen via initialize()", tag = Logger.Tags.VIDEO)
            return
        }
        openVideoEpoch(surface)
        if (state == State.STREAMING || state == State.DEVICE_CONNECTED) {
            scheduleDelayedKeyframe()
        }
    }

    /**
     * Recover video after the settings overlay closes.
     *
     * A keyframe request FIRST, not an epoch bump. The problem this solves is a stale frame while
     * the next natural IDR is up to a minute away — a keyframe fixes that for the price of one
     * frame, whereas an epoch bump costs a codec teardown, a configure and an IDR wait anyway.
     * The watchdog bumps the epoch only if the frame counter proves the keyframe did not help.
     */
    fun recoverVideoFromOverlay() {
        if (state != State.STREAMING) return
        logInfo("[LIFECYCLE] Recovering video after overlay close", tag = Logger.Tags.VIDEO)
        val e = videoEpoch ?: return
        val before = e.renderer.framesRendered.get()
        client?.requestKeyframe()

        scope.launch {
            delay(OVERLAY_RECOVERY_WATCHDOG_MS)
            val still = videoEpoch ?: return@launch
            if (still.seq != e.seq) return@launch // already replaced
            if (still.renderer.framesRendered.get() == before) {
                logWarn(
                    "[LIFECYCLE] No frames ${OVERLAY_RECOVERY_WATCHDOG_MS}ms after overlay keyframe — rebuilding decoder",
                    tag = Logger.Tags.VIDEO,
                )
                val surface = videoSurface ?: return@launch
                retireVideoEpoch("overlay recovery watchdog")
                openVideoEpoch(surface)
            }
        }
    }

    // ==================== OCBM session callbacks ====================

    /**
     * Status text for a CT_BT_PHASE value, or null for phases with nothing to say.
     *
     * Advisory and only roughly monotonic: never gate on ordering, and treat an unknown value as
     * progress. This is the only visibility the host has into the entire Bluetooth handshake,
     * which was otherwise a silent multi-second gap.
     */
    private fun btPhaseText(phase: Byte): String? =
        when (phase) {
            Ocbm.BTP_LINK_UP -> "Phone found — linking..."
            Ocbm.BTP_AUTHENTICATING -> "Authenticating..."
            Ocbm.BTP_AUTHENTICATED -> "Authenticated"
            Ocbm.BTP_IDENTIFYING -> "Identifying..."
            Ocbm.BTP_IDENTIFIED -> "Phone identified"
            Ocbm.BTP_WIFI_HANDOFF -> "Handing off to Wi-Fi..."
            else -> null
        }

    /**
     * Wire [c]'s callbacks. Every one of them fires on the OCBM read thread or a client-owned
     * thread, never main — so anything touching Android state hops through [scope].
     */
    private fun wireClientCallbacks(c: OcbmClient) {
        c.onLanesArmed = { lanes -> onLanesArmed(lanes) }
        c.onLanesRetired = { onLanesRetired() }

        // The first per-stream key is the earliest truthful "a paired, keyed session exists"
        // signal — earlier than the first decoded frame, and unlike phone presence it proves the
        // crypto handshake completed.
        c.onSessionKeyed = {
            hadPriorSession = true
            scope.launch {
                // The key event and a link death can coincide (a connector jiggle at handshake).
                // Without this the stale post lands DEVICE_CONNECTED after the error path already
                // set DISCONNECTED, and both the reconnect backoff and the USB-attach intent are
                // gated on DISCONNECTED — so the app wedges at "Phone connected" with no session.
                if (client !== c) return@launch
                setState(State.DEVICE_CONNECTED)
                setStatusText("Phone connected")
                currentPhoneType = PhoneType.CARPLAY
                callback?.onPhoneTypeChanged(PhoneType.CARPLAY)
                startCodecIfDeferred()
                scheduleDelayedKeyframe()
                // Initial sync. A session that comes up while already in Drive gets no edge from
                // the monitor, so push the current value once the box can actually receive it.
                driveMonitor?.let { if (it.connected) setLimitedUI(it.driving) }
            }
        }

        c.onPhonePresence = { present ->
            scope.launch {
                if (present || state == State.DISCONNECTED) return@launch
                // Drop out of STREAMING. MainScreen keys both the loading overlay and touch
                // forwarding on STREAMING, so staying there leaves the last decoded frame frozen
                // on screen with touch still live and this very status text invisible behind it.
                // The OCBM session itself stays up by design; only the UI state reflects reality.
                if (state == State.STREAMING) setState(State.DEVICE_CONNECTED)
                setStatusText("Waiting for phone...")
            }
        }

        // Advisory only — the client's own backoff re-subscribes and tearing down here would
        // fight that recovery. Surfacing it just keeps the UI honest during the gap.
        c.onHostPresence = { present ->
            scope.launch {
                if (!present && state != State.DISCONNECTED) setStatusText("Re-establishing session...")
            }
        }

        c.onBtPhase = { phase ->
            val text = btPhaseText(phase)
            if (text != null) scope.launch { if (state != State.STREAMING) setStatusText(text) }
        }

        c.onPairingCode = { code ->
            // The one value that MUST reach a human: it is matched against a prompt on the
            // iPhone, and until it is on screen the only place it exists is a logcat line nobody
            // is watching from the driver's seat.
            scope.launch {
                if (code.isNotEmpty()) setStatusText("Pairing code: $code") else setStatusText("Pairing...")
            }
        }

        // Hops through `scope` like every other callback here. It previously ran inline on the
        // read thread, which let a CT_UPLINK(on) arriving mid-teardown start an AudioRecord that
        // stopImpl had already passed — a hot mic surviving into the next session, where
        // onMicGate would early-return on the stale flag and uplink at the WRONG rate. The
        // identity check closes the same window from the other side.
        c.onUplinkGate = { on, rate, ch ->
            scope.launch { if (client === c) onMicGate(on, rate, ch) }
        }

        c.onPhoneIdentity = { json -> scope.launch { onPhoneIdentity(json) } }

        c.onMetadata = { marker, payload -> onMetadata(marker, payload) }

        // FATAL only. A box-side SEV_HOST_GONE is recoverable and the client's own backoff
        // re-subscribes; tearing down on it would fight that recovery.
        c.onLinkDead = { reason -> handleError("usb — link dead: $reason") }
    }

    private fun onLanesArmed(lanes: OcbmAvLanes) {
        logInfo("[SESSION] A/V lanes armed", tag = Logger.Tags.ADAPTR)

        // Audio consumers must start here or the pipes fill and eventually backpressure the read
        // thread. Both are flag-only on stop(); their consume threads are the sole releasers.
        val am = context.getSystemService(Context.AUDIO_SERVICE) as? android.media.AudioManager
        val player =
            AacPlayer(am).also {
                it.start()
                it.prime()
            }
        aacPlayer = player
        val router =
            VoiceRouter(
                context,
                onDuck = { ducked -> player.setDucked(ducked) },
                onAssistant = { speaking -> player.setAssistantSpeaking(speaking) },
            ).also { it.start() }
        voiceRouter = router

        lanes.runConsumer("cp-audio", lanes.mediaPipe) { player.consume(it) }
        lanes.runConsumer("cp-voice", lanes.voicePipe) { router.consume(it) }

        // The Surface may already be waiting; if not, initialize()'s debounce opens the epoch.
        scope.launch { startCodecIfDeferred() }
    }

    private fun onLanesRetired() {
        // Contract with OcbmAvLanes: the video pipe is OURS, so we must close it here. Lanes
        // close only the two audio pipes; a video consumer left parked on an open pipe would
        // hold its codec and burn the join timeout.
        retireVideoEpoch("lanes retired")
        releaseAudio()
    }

    private fun releaseAudio() {
        // Flag-only on both: each consume thread releases its own codec and track in its finally,
        // and it is the only thread allowed to. AacPlayer.stop() additionally releases a primed
        // but never-consumed track, which matters on every path where the media seam never
        // delivered.
        aacPlayer?.stop()
        aacPlayer = null
        voiceRouter?.stop()
        voiceRouter = null
    }

    // ==================== Microphone ====================

    /**
     * The box's mic gate.
     *
     * This replaces the whole riddleBox `VoiceMode` state machine. There, mic-on had to be
     * inferred from downlink audio commands, and the inference had a genuine hazard —
     * `PHONECALL_START` arrives ~130 ms BEFORE `SIRI_STOP`, so a naive reading killed the call's
     * microphone. Here the box states it outright and carries the negotiated format with it.
     */
    private fun onMicGate(
        on: Boolean,
        rate: Int,
        channels: Int,
    ) {
        if (on) {
            micRate = rate
            micChannels = channels
            startMicrophoneCapture(rate, channels)
        } else {
            stopMicrophoneCapture()
        }
    }

    private fun startMicrophoneCapture(
        rate: Int,
        channels: Int,
    ) {
        if (isMicrophoneCapturing) return
        val mgr = microphoneManager ?: return

        if (!mgr.start(captureProfileFor(rate, channels))) {
            logError("[MIC] Capture failed to start", tag = Logger.Tags.MIC)
            return
        }
        isMicrophoneCapturing = true

        // 20 ms of PCM per tick, matching the box's own RTP packetization. The cadence and the
        // chunk size must change together or the stream underruns.
        val chunkSize = MicProfile.chunkBytes(rate, channels)
        val generation = micSendGeneration.incrementAndGet()
        micSendFuture?.cancel(false)
        var failures = 0
        micSendFuture =
            micSendExecutor.scheduleAtFixedRate({
                if (generation != micSendGeneration.get()) return@scheduleAtFixedRate
                try {
                    sendMicrophoneData(chunkSize)
                    failures = 0
                } catch (e: Exception) {
                    // Rate-limited: at 20 ms a persistent failure would log 50x/sec.
                    if (failures++ % 250 == 0) {
                        logError("[MIC] send threw (cadence kept alive, failures=$failures)", tag = Logger.Tags.MIC, throwable = e)
                    }
                }
            }, 0, MicProfile.TICK_MS.toLong(), java.util.concurrent.TimeUnit.MILLISECONDS)

        logInfo("[MIC] Capture started at ${rate}Hz ${channels}ch (chunk ${chunkSize}B)", tag = Logger.Tags.MIC)
    }

    /**
     * Map the box-negotiated format onto [MicrophoneCaptureManager]'s capture profile.
     *
     * That manager still keys its format off riddleBox's `decodeType` table. An unmapped rate
     * would silently fall back to 16 kHz mono and reach Siri pitch-shifted with no error
     * anywhere, so an unknown format is called out rather than quietly accepted.
     */
    private fun captureProfileFor(
        rate: Int,
        channels: Int,
    ): Int =
        MicProfile.decodeTypeFor(rate, channels) ?: run {
            logWarn(
                "[MIC] Box negotiated ${rate}Hz ${channels}ch, which has no capture profile — " +
                    "falling back to 16 kHz mono; the uplink will be WRONG",
                tag = Logger.Tags.MIC,
            )
            MicProfile.FALLBACK_DECODE_TYPE
        }

    private fun stopMicrophoneCapture() {
        if (!isMicrophoneCapturing) return
        micSendGeneration.incrementAndGet() // invalidate any queued run before cancelling
        micSendFuture?.cancel(false)
        micSendFuture = null
        microphoneManager?.stop()
        isMicrophoneCapturing = false
        logInfo("[MIC] Capture stopped", tag = Logger.Tags.MIC)
    }

    private fun sendMicrophoneData(chunkSize: Int) {
        if (!isMicrophoneCapturing) return
        val data = microphoneManager?.readChunk(maxBytes = chunkSize) ?: return
        // S16LE straight out to CH_MIC — the box converts to the big-endian the iPhone wants.
        // Do NOT "symmetry-fix" this against the big-endian DOWNLINK: a swapped uplink reaches
        // Siri as noise with no local symptom.
        if (data.isNotEmpty()) client?.sendMicPcm(data)
    }

    // ==================== Metadata ====================

    /**
     * `CT_PHONE_IDENT` — the connected phone's own description of itself.
     *
     * Both fields earn their place: `name` is what the user set in Settings > General > About > Name,
     * and `deviceID` is the BR/EDR MAC, which is what makes this joinable against the bonded MACs in
     * `MGMT_INFO`. Until this existed the device list could only show bare hex, and nothing said
     * which bonded phone was the live one.
     */
    private fun onPhoneIdentity(json: String) {
        if (json.isEmpty()) {
            connectedPhoneMac = null
            return
        }
        val obj =
            runCatching { JSONObject(json) }.getOrNull() ?: run {
                logWarn("[PHONE] CT_PHONE_IDENT is not JSON: $json", tag = Logger.Tags.PHONE)
                return
            }
        val name = obj.optString("name")
        val mac = obj.optString("deviceID").lowercase()
        if (mac.isEmpty()) return
        connectedPhoneMac = mac
        _connectedBtMac = mac
        if (name.isNotEmpty()) phoneNames[mac] = name

        // Remember it. This is the ONLY source of a device name anywhere in the system — the box's
        // link-key record has no room for one — so without this the list reverts to bare hex after
        // every app restart. Also lifts any forget-suppression: a user who forgets and re-pairs
        // inside the window must not stare at an empty list.
        val now = System.currentTimeMillis()
        synchronized(deviceListLock) { recentlyForgotten.remove(mac) }
        knownStore?.update { snap ->
            val prev = snap.devices[mac]
            snap.copy(
                devices =
                    snap.devices +
                        (
                            mac to
                                KnownDevice(
                                    mac = mac,
                                    name = name.ifEmpty { prev?.name },
                                    model = obj.optString("model").ifEmpty { prev?.model },
                                    osName = obj.optString("osName").ifEmpty { prev?.osName },
                                    osVersion = obj.optString("osVersion").ifEmpty { prev?.osVersion },
                                    firstSeenMs = prev?.firstSeenMs ?: now,
                                    lastConnectedMs = now,
                                )
                        ),
            )
        }
        logInfo(
            "[PHONE] ${name.ifEmpty { mac }} (${obj.optString("model")}, " +
                "${obj.optString("osName")} ${obj.optString("osVersion")})",
            tag = Logger.Tags.PHONE,
        )
        // The list may already be on screen with a MAC for a label; rebuild it with the real name.
        rebuildDeviceList()
    }

    /**
     * Rebuild the merged view from history + the box's bond list, and publish it.
     *
     * The single place the list is produced, so every input change funnels through the same rules.
     * It goes through [mutateDeviceList] specifically so the recently-forgotten filter applies to
     * PERSISTED entries too — merging outside that lock would let the box's stale snapshot, which
     * still lists a device for ~4-5 s while it restarts wireless, resurrect a just-forgotten phone
     * into permanent storage. That is strictly worse than the transient resurrection the filter was
     * originally written for.
     */
    private fun rebuildDeviceList() {
        val snap = knownStore?.snapshot ?: KnownDeviceSnapshot()
        val merged =
            synchronized(deviceListLock) {
                mergeDeviceList(
                    known = snap.devices,
                    bondedMacs = bondedMacs,
                    suppressed = recentlyForgotten.keys.toSet(),
                    preferredMac = snap.preferredMac,
                    formatLastSeen = ::formatLastSeen,
                )
            }
        if (merged == _pairedDevices) return
        mutateDeviceList { merged }
        callback?.onDeviceListChanged(_pairedDevices)
        notifyDeviceListeners()
    }

    /** Human-readable "last seen" for a wall-clock timestamp, in the head unit's locale. */
    private fun formatLastSeen(ms: Long): String =
        android.text.format.DateUtils
            .getRelativeTimeSpanString(ms, System.currentTimeMillis(), android.text.format.DateUtils.MINUTE_IN_MILLIS)
            .toString()

    /**
     * Choose the phone the adapter should prefer, or null to follow most-recently-connected.
     *
     * Persisted immediately so it survives a restart, and reflected in list order at once.
     */
    fun setPreferredDevice(btMac: String?) {
        val mac = btMac?.lowercase()
        logInfo("[DEVICE_MGMT] Preferred device -> ${mac ?: "(most recent)"}", tag = Logger.Tags.ADAPTR)
        knownStore?.update { it.copy(preferredMac = mac) }
        rebuildDeviceList()
    }

    private fun onMetadata(
        marker: Int,
        payload: ByteArray,
    ) {
        when (marker) {
            Ocbm.META_JSON -> {
                val obj = runCatching { JSONObject(String(payload, Charsets.UTF_8)) }.getOrNull() ?: return
                when (obj.optString("kind")) {
                    "nowPlaying" -> scope.launch { processNowPlaying(obj) }
                    "callState" -> logInfo("[CALL_STATE] ${obj.optString("label")} ${obj.optString("service")}", tag = Logger.Tags.PHONE)
                    else -> Unit
                }
            }
            Ocbm.META_ARTWORK -> {
                if (payload.size < 2) return
                val id = payload[0].toInt() and 0xFF
                val jpeg = payload.copyOfRange(1, payload.size)
                synchronized(artworkById) { artworkById[id] = jpeg }
                // Art can arrive before OR after the track that references it, so attach on
                // whichever side lands second.
                if (pendingArtworkId == id) scope.launch { attachArtwork(id) }
            }
            Ocbm.META_CMD -> onCommandPlist(payload)
            else -> Unit // META_CORNERMASK is not consumed by the media surface
        }
    }

    /**
     * An inbound iPhone `POST /command`, forwarded verbatim as a binary plist.
     *
     * The one we act on is `requestUI` — Apple's own words for it are "the function to call when the
     * controller requests accessory UI" (`AirPlayReceiverSession.h:189`). That is what the user taps
     * when they pick our OEM icon on the CarPlay home screen to get back to this app, so it is the
     * return path for the icon advertised in `oemIconConfig`.
     *
     * Runs on the OCBM read thread, so it parses and hands off — [BinaryPlist] returns null rather
     * than throwing on anything malformed, because an exception here would take the dispatch loop
     * down with it. Unrecognised verbs are logged once each: the box forwards every command iOS
     * sends, and a silent drop is how the previous one of these went unnoticed.
     */
    private fun onCommandPlist(payload: ByteArray) {
        val d = BinaryPlist.parseDict(payload)
        if (d == null) {
            logWarn("[META] META_CMD payload is not a readable plist (${payload.size} B)", tag = Logger.Tags.ADAPTR)
            return
        }
        when (val type = d["type"] as? String) {
            "requestUI" -> {
                val url = (d["params"] as? Map<*, *>)?.get("url") as? String
                logInfo("[META] requestUI${url?.let { " ($it)" } ?: ""} — bringing the app forward", tag = Logger.Tags.ADAPTR)
                scope.launch { callback?.onHostUIPressed() }
            }
            null -> logWarn("[META] META_CMD has no string 'type'", tag = Logger.Tags.ADAPTR)
            else -> if (unhandledCommands.add(type)) logInfo("[META] command '$type' (not handled)", tag = Logger.Tags.ADAPTR)
        }
    }

    private fun attachArtwork(id: Int) {
        val jpeg = synchronized(artworkById) { artworkById[id] } ?: return
        lastAlbumCover = jpeg
        mediaSessionManager?.updateMetadata(
            title = lastMediaSongName,
            artist = lastMediaArtistName,
            album = lastMediaAlbumName,
            appName = lastMediaAppName,
            albumArt = jpeg,
            duration = lastDuration,
        )
    }

    /**
     * Consume one `nowPlaying` record.
     *
     * iAP2 NowPlaying is a DELTA stream: an elapsed-only frame must not blank the title, and the
     * full attribute set is pushed on track change. That is the same shape the riddleBox path had
     * to handle, so the merge and change-detection logic below is carried over intact — only the
     * field names changed.
     */
    private fun processNowPlaying(obj: JSONObject) {
        val newSongName = obj.optString("title").takeIf { it.isNotEmpty() }

        // Snapshot before the song-change reset so incremental updates are still detectable after
        // the caches are overwritten. Without this, an artist-only frame was silently dropped and
        // the cluster showed a title with no artist until the next track.
        val previousSongName = lastMediaSongName
        val previousArtist = lastMediaArtistName
        val previousAlbum = lastMediaAlbumName
        val previousAppName = lastMediaAppName
        val previousDuration = lastDuration

        if (newSongName != null && newSongName != previousSongName) {
            lastMediaSongName = null
            lastMediaArtistName = null
            lastMediaAlbumName = null
            lastAlbumCover = null
            lastDuration = 0L
            lastPosition = 0L
            // Keep appName — it does not usually change mid-session.
        }

        newSongName?.let { lastMediaSongName = it }
        val textChanged = mergeTextFields(obj, previousArtist, previousAlbum, previousAppName)
        val albumCover = mergeArtwork(obj)
        val duration = if (obj.has("durationMs")) obj.optLong("durationMs") else lastDuration
        val position = if (obj.has("elapsedMs")) obj.optLong("elapsedMs") else lastPosition
        // playbackStatus: 0 stop, 1 play, 2 pause (3/4 seek). 0 is the clear-snapshot signal.
        val playStatus = if (obj.has("playbackStatus")) obj.optInt("playbackStatus") else null
        val isPlaying = if (playStatus != null) playStatus == 1 else lastIsPlaying

        lastDuration = duration
        lastPosition = position
        lastIsPlaying = isPlaying

        // Broad predicate so incremental artist/album/appName/duration-only updates are not
        // suppressed. Position-only ticks still short-circuit because none of these change.
        val metadataChanged =
            (newSongName != null && newSongName != previousSongName) ||
                textChanged ||
                albumCover != null ||
                (duration > 0 && duration != previousDuration)

        if (metadataChanged) {
            mediaSessionManager?.updateMetadata(
                title = lastMediaSongName,
                artist = lastMediaArtistName,
                album = lastMediaAlbumName,
                appName = lastMediaAppName,
                albumArt = lastAlbumCover,
                duration = duration,
            )
            CarlinkMediaBrowserService.updateNowPlaying(lastMediaSongName, lastMediaArtistName)
        }

        mediaSessionManager?.updatePlaybackState(playing = isPlaying, position = position)
        markStreamingIfFirstMedia()
    }

    /**
     * First real media is the STREAMING edge.
     *
     * riddleBox used the first video DATA frame; the seam's first decoded frame would be
     * equivalent, but metadata is the signal that actually means "there is something to show".
     * Stamping [lastStreamingStartMs] here is also what makes Pattern C measurable — it is the
     * start of the window a short-lived session is measured against.
     */
    private fun markStreamingIfFirstMedia() {
        if (state != State.DEVICE_CONNECTED) return
        lastStreamingStartMs = SystemClock.elapsedRealtime()
        setState(State.STREAMING)
    }

    /**
     * Merge artist / album / appName from a delta record into the cache.
     *
     * Returns whether any of them actually CHANGED — which is the whole reason the caller passes
     * the pre-merge values in: iAP2 sends these incrementally, so an artist-only frame must still
     * trigger a publish even though the title did not move.
     */
    private fun mergeTextFields(
        obj: JSONObject,
        previousArtist: String?,
        previousAlbum: String?,
        previousAppName: String?,
    ): Boolean {
        var changed = false
        obj.optString("artist").takeIf { it.isNotEmpty() }?.let {
            lastMediaArtistName = it
            if (it != previousArtist) changed = true
        }
        obj.optString("album").takeIf { it.isNotEmpty() }?.let {
            lastMediaAlbumName = it
            if (it != previousAlbum) changed = true
        }
        obj.optString("appName").takeIf { it.isNotEmpty() }?.let {
            lastMediaAppName = it
            if (it != previousAppName) changed = true
        }
        return changed
    }

    /**
     * Note the track's artwork id and return the bytes if they have already arrived.
     *
     * Match on the LOW BYTE: the wire id in `META_ARTWORK` is one byte while `nowPlaying`'s
     * `artworkId` is the full iAP2 integer, so a full-int comparison silently drops every artwork
     * with an id >= 256.
     */
    private fun mergeArtwork(obj: JSONObject): ByteArray? {
        if (!obj.has("artworkId")) return null
        val id = obj.optInt("artworkId") and 0xFF
        pendingArtworkId = id
        val bytes = synchronized(artworkById) { artworkById[id] } ?: return null
        lastAlbumCover = bytes
        return bytes
    }

    // ==================== Private Methods ====================

    private fun setState(newState: State) {
        // HAZARD: the callback dispatch and updateMediaSessionState fire OUTSIDE the CAS, so two
        // concurrent setState calls could dispatch in unspecified order even though each
        // getAndSet is atomic. Safe today because callers are the OCBM read thread and main at
        // non-overlapping phases — but that is a property of the callers, not of the
        // AtomicReference. A third caller on another thread needs a lock around the whole body.
        val oldState = currentState.getAndSet(newState)
        if (oldState != newState) {
            callback?.onStateChanged(newState)
            if (Looper.myLooper() == Looper.getMainLooper()) {
                updateMediaSessionState(newState)
            } else {
                // Binder work (FGS start/stop, wake-lock acquire) must not run on the OCBM read
                // thread. Main executes posts in order, so transitions keep their sequence.
                scope.launch { updateMediaSessionState(newState) }
            }
        }
    }

    private fun setStatusText(text: String) {
        callback?.onStatusTextChanged(text)
        // Mirror status to the MediaSession placeholder so cluster, cardview and CarMediaApp all
        // show the same state the app UI shows. Gated on != STREAMING: once real track metadata
        // is flowing, status text would clobber the live title/artist.
        if (state != State.STREAMING) {
            mediaSessionManager?.updatePlaceholderArtist(text)
        }
    }

    /**
     * Wire the projection state machine to the MediaSession active-flag.
     *
     * The session is INACTIVE by default and ACTIVE only once a phone is actually keyed. This
     * keeps Carlink out of AAOS's playback-primary slot when nothing is projecting.
     *
     * KNOWN OS DEFICIENCY — a stale homescreen Media card after force-stop is an AAOS platform
     * bug reproduced identically on first-party apps: CarLauncher caches the pre-force-stop
     * MediaController and never rebinds to the new session token. This wiring is kept because it
     * is semantically correct, not because it resolves that.
     */
    private fun updateMediaSessionState(state: State) {
        when (state) {
            State.CONNECTING -> {
                // Activate NOW with a "preparing to play" arbitration signal, at USB-attach time.
                // Without it the session stays idle through CONNECTING and only goes BUFFERING
                // later — by which time an already-PLAYING source (e.g. FM) is undisplaceable,
                // because CarMediaService only lets a newly-PLAYING source displace a
                // non-PLAYING one.
                mediaSessionManager?.setProjectionActive(connectingPhase = true)
                CarlinkMediaBrowserService.startConnectionForeground(context)
                acquireWakeLock()
            }

            State.DISCONNECTED -> {
                mediaSessionManager?.setInactive()
                CarlinkMediaBrowserService.clearNowPlaying()
                // Keep the FGS through an active reconnect backoff. This branch runs POSTED to
                // main, and scheduleReconnect has usually already issued its own start by the
                // time it executes — stopping here would land AFTER that start and poison the
                // queued intent, leaving no LMK protection for the whole backoff window.
                if (reconnectJob?.isActive != true) {
                    CarlinkMediaBrowserService.stopConnectionForeground(context)
                }
                releaseWakeLock()
            }

            State.DEVICE_CONNECTED -> {
                // Re-publish with phase-specific text now that the handshake is behind us and we
                // are only waiting for the first media.
                mediaSessionManager?.setProjectionActive(connectingPhase = false)
                CarlinkMediaBrowserService.startConnectionForeground(context)
            }

            State.STREAMING -> {
                CarlinkMediaBrowserService.startConnectionForeground(context)
                acquireWakeLock()
                // Session is already ACTIVE from DEVICE_CONNECTED; metadata drives the rest.
            }
        }
    }

    /**
     * Acquire (or refresh) a partial wake lock so USB transfers and heartbeats continue when the
     * app is backgrounded.
     *
     * The lock is non-reference-counted, so a repeat acquire simply restarts the safety timeout
     * and one release fully releases it. The periodic refresh exists because a session longer
     * than the timeout silently lost the lock mid-drive with no re-acquisition path.
     */
    @Synchronized
    private fun acquireWakeLock() {
        val wasHeld = wakeLock.isHeld
        wakeLock.acquire(WAKE_LOCK_TIMEOUT_MS)
        if (!wasHeld) {
            logInfo("[WAKE_LOCK] Acquired partial wake lock", tag = Logger.Tags.USB)
        } else {
            logDebug("[WAKE_LOCK] Refreshed wake lock timeout", tag = Logger.Tags.USB)
        }
        wakeLockRefreshJob?.cancel()
        wakeLockRefreshJob =
            scope.launch {
                while (isActive) {
                    delay(WAKE_LOCK_REFRESH_INTERVAL_MS)
                    if (wakeLock.isHeld) {
                        wakeLock.acquire(WAKE_LOCK_TIMEOUT_MS)
                        logDebug("[WAKE_LOCK] Periodic refresh", tag = Logger.Tags.USB)
                    }
                }
            }
    }

    /**
     * Release the wake lock and stop the periodic refresh. Synchronized with [acquireWakeLock]:
     * both run from setState on either the read thread or main, and an interleaved cancel/launch
     * pair could orphan a refresh job.
     */
    @Synchronized
    private fun releaseWakeLock() {
        wakeLockRefreshJob?.cancel()
        wakeLockRefreshJob = null
        if (wakeLock.isHeld) {
            wakeLock.release()
            logInfo("[WAKE_LOCK] Released wake lock", tag = Logger.Tags.USB)
        }
    }

    /**
     * Find the adapter, polling for a USB permission grant rather than awaiting the broadcast —
     * on this head unit the permission dialog frequently never appears and the broadcast is
     * often lost, so a blocking wait deadlocks bring-up.
     */
    private suspend fun findDevice(t: UsbBulkTransport): android.hardware.usb.UsbDevice? {
        var attempts = 0
        while (attempts < 10) {
            val dev = t.findQuiet()
            if (dev != null) {
                if (t.hasPermission(dev)) {
                    log("OCBM adapter found")
                    return dev
                }
                t.requestPermissionAsync(dev)
                repeat(20) {
                    if (t.hasPermission(dev)) return dev
                    delay(250)
                }
            }
            attempts++
            delay(USB_WAIT_PERIOD_MS)
        }
        return null
    }

    /**
     * Error handler for session failures.
     *
     * Callable from the OCBM read thread, the detach broadcast (main), and client-owned threads —
     * a physical unplug fires it from two at once. The public entry posts under [lifecycleMutex]
     * so executions serialize and the blocking teardown runs on IO.
     */
    private fun handleError(error: String) {
        // Capture session identity AT POST TIME: the queued impl may run after a new session has
        // come up (fast unplug/replug where a new start() wins the mutex first). Without this, a
        // stale error would tear down a healthy session.
        val erroredClient = client
        scope.launch {
            lifecycleMutex.withLock {
                withContext(Dispatchers.IO) { handleErrorImpl(error, erroredClient) }
            }
        }
    }

    private fun handleErrorImpl(
        error: String,
        erroredClient: OcbmClient?,
    ) {
        if (erroredClient != null && erroredClient !== client) {
            logDebug("[ERROR] Stale error from a replaced session, ignoring: $error", tag = Logger.Tags.ADAPTR)
            return
        }
        // Dedup: a second report of the same physical failure arrives with everything already
        // torn down. Skip — but only when there is genuinely nothing to do.
        if (client == null && transport == null && state == State.DISCONNECTED) {
            logDebug("[ERROR] Duplicate error report after teardown, ignoring: $error", tag = Logger.Tags.ADAPTR)
            return
        }
        // hadPriorSession is intentionally NOT reset here (only in stop()), so escalation can
        // distinguish "broken after a prior session" from "never connected".

        logError("Adapter error: $error", tag = Logger.Tags.ADAPTR)

        cancelDelayedKeyframe()
        currentPhoneType = null
        currentWifi = null
        codecDeferred = true
        callback?.onPhoneTypeChanged(PhoneType.UNKNOWN)
        clearCachedMediaMetadata()
        stopMicrophoneCapture()

        retireVideoEpoch("session error")
        releaseAudio()

        // Skip the graceful CT_STOP — the pipe is likely already dead.
        client?.stop()
        client = null
        transport?.stop()
        transport = null

        val isNoResponse = error.contains("no initial response")
        if (isNoResponse) consecutiveNoResponse++ else consecutiveNoResponse = 0

        if (lastStreamingStartMs > 0) {
            val sessionDuration = SystemClock.elapsedRealtime() - lastStreamingStartMs
            if (sessionDuration < SHORT_SESSION_THRESHOLD_MS) {
                shortLivedStreamingCount++
            } else {
                // A session survived past the threshold — the link is fundamentally stable, so
                // clear the instability history. This, not connect, is the correct reset point.
                shortLivedStreamingCount = 0
            }
            lastStreamingStartMs = 0L
        }

        setState(State.DISCONNECTED)

        // Escalation status is set AFTER scheduleReconnect: it unconditionally writes
        // "Reconnecting (n/5)...", which would otherwise instantly clobber the advice.
        if (isUsbDisconnectError(error)) {
            scheduleReconnect()
            if (consecutiveNoResponse >= 2) {
                setStatusText("Adapter not responding — reboot adapter")
                logWarn("[ESCALATION] Pattern A: $consecutiveNoResponse consecutive no-response errors", tag = Logger.Tags.USB)
            } else if (shortLivedStreamingCount >= SHORT_SESSION_ESCALATION_COUNT) {
                setStatusText("Connection unstable — reboot adapter")
                logWarn("[ESCALATION] Pattern C: $shortLivedStreamingCount short-lived sessions", tag = Logger.Tags.USB)
            } else if (isNoResponse) {
                setStatusText("Adapter not responding — reconnecting...")
            }
        }
    }

    /**
     * Failure exit for [startImpl] before a client exists. These paths used to just return, which
     * silently killed an in-progress reconnect chain: after an adapter reboot (~50 s to
     * re-enumerate) attempt 1 fired at +2 s, findDevice timed out, and the remaining attempts
     * never ran despite MAX_RECONNECT_ATTEMPTS.
     */
    private fun handleFailedStart(statusMsg: String) {
        transport?.stop()
        transport = null
        setState(State.DISCONNECTED)
        setStatusText(statusMsg)
        if (reconnectAttempts > 0) {
            logInfo("[RECONNECT] start() failed pre-session ($statusMsg) — continuing chain", tag = Logger.Tags.USB)
            scheduleReconnect()
        }
    }

    private fun isUsbDisconnectError(error: String): Boolean {
        val lowerError = error.lowercase()
        return lowerError.contains("disconnect") ||
            lowerError.contains("detach") ||
            lowerError.contains("transfer") ||
            lowerError.contains("usb") ||
            lowerError.contains("no initial response")
    }

    /**
     * Schedule an auto-reconnect with exponential backoff: 2s, 4s, 8s, 16s, 30s (capped), then
     * give up so a dead adapter cannot spin forever.
     *
     * The delays also matter to the BOX: it escalates on five presence edges in twenty seconds,
     * climbing to a reboot that spends from a persistent budget. Backing off keeps us clear of it.
     */
    private fun scheduleReconnect() {
        reconnectJob?.cancel()

        if (reconnectAttempts >= MAX_RECONNECT_ATTEMPTS) {
            logWarn(
                "[RECONNECT] Max attempts ($MAX_RECONNECT_ATTEMPTS) reached, giving up. " +
                    "noResponse=$consecutiveNoResponse shortSessions=$shortLivedStreamingCount hadPrior=$hadPriorSession",
                tag = Logger.Tags.USB,
            )
            val giveUpMessage =
                when {
                    consecutiveNoResponse >= 2 -> "Adapter not responding — reboot adapter"
                    shortLivedStreamingCount >= SHORT_SESSION_ESCALATION_COUNT -> "Connection unstable — reboot adapter"
                    hadPriorSession -> "Phone not reconnecting — reboot adapter"
                    else -> "Adapter not responding — unplug and replug adapter"
                }
            // reconnectAttempts resets but the escalation counters do NOT: a user who taps
            // reconnect after give-up should still see the right advice on the first failure.
            reconnectAttempts = 0
            setStatusText(giveUpMessage)
            CarlinkMediaBrowserService.stopConnectionForeground(context)
            return
        }

        CarlinkMediaBrowserService.startConnectionForeground(context)

        val backoff =
            minOf(
                INITIAL_RECONNECT_DELAY_MS * (1L shl reconnectAttempts),
                MAX_RECONNECT_DELAY_MS,
            )
        reconnectAttempts++

        logInfo("[RECONNECT] Scheduling attempt $reconnectAttempts/$MAX_RECONNECT_ATTEMPTS in ${backoff}ms", tag = Logger.Tags.USB)
        setStatusText("Reconnecting ($reconnectAttempts/$MAX_RECONNECT_ATTEMPTS)...")

        reconnectJob =
            scope.launch {
                delay(backoff)
                if (state == State.DISCONNECTED) {
                    logInfo("[RECONNECT] Attempting reconnection...", tag = Logger.Tags.USB)
                    try {
                        withContext(Dispatchers.IO) { start() }
                    } catch (e: kotlinx.coroutines.CancellationException) {
                        // Expected: a pre-session failure continues the chain via
                        // handleFailedStart→scheduleReconnect, which cancels THIS job.
                        throw e
                    } catch (e: Exception) {
                        logError("[RECONNECT] Reconnection failed: ${e.message}", tag = Logger.Tags.USB)
                    }
                } else {
                    logInfo("[RECONNECT] Already connected, cancelling reconnect", tag = Logger.Tags.USB)
                    reconnectAttempts = 0
                }
            }
    }

    private fun cancelReconnect() {
        reconnectJob?.cancel()
        reconnectJob = null
        reconnectAttempts = 0
    }

    /**
     * Schedule keyframe requests.
     *
     * 1. Initial (2.5 s): a cold-start safety net. The session's natural IDR normally decodes
     *    fine, but on this Intel VPU a first-session decoder can come up poisoned, and a fresh
     *    IDR on a now-warm codec clears it.
     * 2. Periodic (30 s): passive self-healing against silent mid-session decoder corruption.
     *    The gap watchdog only catches complete decode failure, not progressive degradation from
     *    corrupted reference frames, and a periodic IDR is the only fix for that.
     *
     * Both are platform mitigations, independent of protocol — they survived the migration
     * unchanged apart from the verb they send. The request is throttled client-side, so this
     * cannot collide with the seam's own gap-driven requests.
     */
    @Synchronized
    private fun scheduleDelayedKeyframe() {
        frameIntervalJob?.cancel()
        frameIntervalJob =
            scope.launch(Dispatchers.IO) {
                logInfo("[FRAME_INTERVAL] Keyframe schedule armed (2.5s initial, 30s periodic)", tag = Logger.Tags.VIDEO)
                delay(2500)
                client?.requestKeyframe()
                var requestCount = 0
                while (isActive) {
                    delay(30000)
                    requestCount++
                    val sent = client?.requestKeyframe() ?: false
                    logDebug("[FRAME_INTERVAL] Periodic keyframe #$requestCount sent=$sent", tag = Logger.Tags.VIDEO)
                }
            }
    }

    @Synchronized
    private fun cancelDelayedKeyframe() {
        if (frameIntervalJob?.isActive == true) {
            logDebug("[FRAME_INTERVAL] Cancelling pending keyframe schedule", tag = Logger.Tags.VIDEO)
            frameIntervalJob?.cancel()
        }
        frameIntervalJob = null
    }

    private fun log(message: String) {
        logDebug(message, tag = Logger.Tags.ADAPTR)
    }
}
