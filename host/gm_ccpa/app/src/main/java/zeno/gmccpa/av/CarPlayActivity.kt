package zeno.gmccpa.av

import android.app.Activity
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.WindowManager
import zeno.gmccpa.ProbeLog
import zeno.gmccpa.logging.SessionTrace
import zeno.gmccpa.pair.NativeCore
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

/**
 * The CarPlay screen: fullscreen immersive HEVC video, AAC audio, and touch back to the iPhone.
 *
 * **Ownership:** the seam listeners and the audio player are ACTIVITY-scoped; only the video renderer
 * is SURFACE-scoped. Backgrounding therefore costs video frames and nothing else — the sockets stay
 * bound, audio keeps playing, and the producer re-dials the video seam with a ForceKeyFrame when a
 * Surface comes back. (A foreground service is still required for the session to survive this Activity
 * being destroyed; that is a separate, larger gap.)
 *
 * Owns the consumer end of the receiver's localhost seam. `forward.rs` dials OUT to `127.0.0.1:9001`
 * (video) and `:9002` (media audio) with a 2 s connect timeout, drops frames while the consumer is
 * down, and reconnects — issuing ForceKeyFrame — on the next frame. So a gap costs dropped AUs but is
 * recoverable; a failed *bind* is a permanent black screen, which is why binding is synchronous and
 * retried rather than logged and forgotten.
 *
 * Geometry is fixed at the advertised 2400x960: iOS renders into that space and expects touch
 * normalized against it. The Surface is stretched to fill the view, so normalizing by the view's own
 * dimensions is equivalent — but only while the layout is full-bleed. Do not introduce letterboxing
 * without revisiting [onTouch].
 */
class CarPlayActivity : Activity() {

    private val log = ProbeLog.sub("cpui")

    companion object {
        /**
         * The live screen, so a session ending elsewhere can tear it down.
         *
         * A WEAK reference on purpose: this is a static field holding an Activity, and a strong one
         * would pin a finished Activity (and its Surface, decoder and window) for the life of the
         * process. Cleared in [onDestroy] too — the weak ref is the backstop, not the plan.
         */
        @Volatile private var live: java.lang.ref.WeakReference<CarPlayActivity>? = null

        /**
         * The CarPlay session ended — take the screen down with it.
         *
         * # Why this exists
         *
         * Nothing used to tell this Activity a session had ended. `CarPlayRx.fireSessionDown` reached
         * `MainActivity.onCarPlaySessionDown`, which set `sessionUp = false`, poked the supervisor and
         * cleared the pairing code — and stopped there. The only `finish()` in this file was on a bind
         * failure. So when the phone went out of Wi-Fi range the pump timed out, the session was
         * correctly declared down, and **the last decoded frame stayed on the Surface indefinitely**:
         * a frozen CarPlay screen over a dead session, swallowing touches. Device-reported 2026-08-28.
         *
         * Finishing, rather than clearing the Surface and staying up, is deliberate — it is also what
         * fixes RESUME. `startSession` returns early on "session already started", so a stale live
         * Activity absorbs the returning phone's `onSessionUp` and the screen never rebuilds. Taking
         * it down means `SessionCore.showCarPlayScreen` starts a clean one, which is exactly what a
         * reconnect needs (a fresh generation, a fresh Surface, and a producer that re-dials with an
         * IDR).
         *
         * Safe to call from any thread and when no screen is up.
         */
        fun onSessionEnded(why: String) {
            live?.get()?.endSession(why)
        }

        // ---- link-interrupted overlay (2026-10-06) ----------------------------------------------
        // Generation-tagged, so a superseded pump cannot leave an overlay over a live session: the
        // old pump T1 can pass its own check, the accept thread then installs gen h2 and fires up(h2),
        // and only THEN does T1 fire quiet(h1). Both orders end correct: up(h2) then quiet(h1) is
        // ignored; quiet(h1) then up(h2) is shown, then removed. The three fields are main-thread
        // only; the callers post through a main-looper Handler, so state updates with no screen up
        // and a screen created later shows the current state.
        private val main = android.os.Handler(android.os.Looper.getMainLooper())
        private var lastUpGen = 0L   // main thread only
        private var overlayGen = 0L  // main thread only; 0 = no overlay

        /** A native generation [g] came up (the receiver's onSessionUp). */
        fun linkUp(g: Long) {
            main.post {
                lastUpGen = maxOf(lastUpGen, g)
                if (overlayGen in 1 until g) { overlayGen = 0L; live?.get()?.showLinkOverlay(false) }
            }
        }

        /** Generation [g]'s control channel went silent with A/V not fresh — show the overlay. */
        fun linkQuiet(g: Long) {
            main.post {
                if (g < lastUpGen) return@post   // a superseded generation's late report
                overlayGen = g
                live?.get()?.showLinkOverlay(true)
            }
        }

        /** Generation [g] carried bytes again, or its pump ended. Only [g]'s own overlay is removed. */
        fun linkResumed(g: Long) {
            main.post {
                if (g != overlayGen) return@post
                overlayGen = 0L
                live?.get()?.showLinkOverlay(false)
            }
        }

        /**
         * The merged now-playing picture. PROCESS-wide, not per-Activity: the MediaSession that
         * publishes it lives in [CarPlayMediaBrowserService], whose lifetime AAOS controls (it is
         * bound when Media Center asks, not when we start), so an Activity-instance field would be
         * unreachable from it. Session scope is restored by CLEARING it in
         * [CarPlayMediaBrowserService.onSessionDown] — on the SESSION's end, not this screen's (plan
         * v5.9 MED M4): a destroy with the session still up must not blank the merge base.
         */
        val nowPlaying = NowPlayingState().apply {
            onMetadataChanged = { CarPlayMediaBrowserService.publish(it) }
            onPlaybackTick = { CarPlayMediaBrowserService.publishPlaybackState(it) }
            // Plan v5.9 MED M5: the phone-side status edge for the media player's focus-reclaim rule.
            // Resolved at call time through the live screen, never captured (same reason as below).
            onPlaybackStatus = { st -> live?.get()?.player?.onPhonePlaybackStatus(st) }
            // U10 (F159): route-active signal for the voice router. Resolved at CALL time through
            // the live screen's CURRENT router — never captured, because this object is process-wide
            // and a captured Activity or VoiceRouter would pin a finished screen (the F082 class).
            // Runs on cp-meta; the router only stamps volatiles. A router between stop() and its
            // field being nulled is latched closed, so a prime from here is refused.
            onRouteGuidance = { st -> live?.get()?.voiceRouter?.onRouteGuidance(st) }
        }

        init {
            // Plan v5.9 MED M5: a MediaSession play / play-pause is a resume intent for the player's
            // focus-reclaim rule. Process-wide like [nowPlaying]; resolved through the live screen.
            CarPlayMediaBrowserService.onResumeIntent = { live?.get()?.player?.onResumeIntent() }
        }

        /**
         * The declared view areas, in `/info` order. MUST stay in step with
         * `tools/info_plist_viewareas.py` and the priming block in `native/carplay-jni/src/lib.rs` —
         * three places, one geometry, because this app serves a STATIC /info and nothing derives one
         * from the others.
         *
         * [0] the full panel. [1] the box AAOS actually gives an ordinary app on this head unit
         * (`mAppBounds` 1416x842, origin moved from x=189 to 188 because iOS's validator requires all
         * four values even and an odd one is a teardown, not a warning).
         */
        val VIEW_AREAS = listOf(
            android.graphics.Rect(0, 0, 2400, 960),
            android.graphics.Rect(188, 118, 188 + 1416, 118 + 842),
        )
        /** The duration the receiver tells iOS the transition takes (`events::send_update_view_area`). */
        const val VIEW_AREA_ANIM_MS = 3000L

        /** Must match `/info` displays[] — the space iOS renders into and expects touch in. */
        const val DISPLAY_W = 2400
        const val DISPLAY_H = 960
        // Binds run on the `cp-setup` thread (see [startSession]), so they no longer sit on the ANR
        // clock — but the worst case (4 ports x RETRIES x BACKOFF = 3.6 s) is still how long the
        // seams can take to come up after the screen appears, so it stays small. Normal binds are instant.
        private const val BIND_RETRIES = 6
        private const val BIND_BACKOFF_MS = 150L
        /** Q010 (v5.8 V7): accept() retry while the generation is live — CarPlayRx.acceptLoop's 40 x 250 ms. */
        private const val SEAM_ACCEPT_RETRY_MS = 250L
        private const val SEAM_ACCEPT_FAILURES_FATAL = 40
        /** Desync guard on the seam length prefix; matches HevcRenderer and session.rs MAX_FRAME_BODY. */
        private const val MAX_MESSAGE = 8 * 1024 * 1024
        /** Displacement sent before a cancelled gesture's UP, as a fraction of the view width. ~2% is
         *  comfortably past iOS's tap allowable-movement and still an imperceptible drag. */
        private const val CANCEL_SLOP_N = 0.02f

        /**
         * [SessionTrace] name for "the producer dialled :9001". ONE name, armed from two edges that
         * both mean "a video connection is now owed":
         *
         *  - the first `/command` plist of a generation ([onCommandPlist]) with no video connection
         *    live — iOS sends it on the event channel accepted at RECORD, and SETUPs the screen
         *    stream right after (2026-09-09: RECORD 22:55:22.072, first `modesChanged` .100, SETUP
         *    type-110 .299, `video seam connected` .802 — **730 ms**). Budget [VIDEO_CONNECT_MS].
         *  - a live :9001 connection bounced by [attachRenderer], after which forward.rs re-dials on
         *    its next frame and the ForceKeyFrame just sent produces one. Budget [VIDEO_REDIAL_MS].
         *
         * NOT armed from the Activity launch itself: `SessionCore.showCarPlayScreen` also runs from the bench
         * `carplay_ui` command and from `onResume`, and the reference session shows this screen up
         * from 22:51:14 with no phone until 22:55:20 — a launch-anchored expectation would fire on
         * every bench start. Nor from the metadata seam connect, whose producer is not proven to be
         * post-RECORD on every transport. The plist is byte-proven post-RECORD.
         *
         * `av/`-scoped: [stopSession] drops it with the rest when the AirPlay session ends.
         */
        private const val EXPECT_VIDEO_CONNECT = "av/video-seam-connect"
        /** 730 ms measured; the screen SETUP is iOS-initiated immediately after RECORD, unlike the
         *  type-102 media stream (8.5 s later in the same session, on demand — deliberately NOT armed). */
        private const val VIDEO_CONNECT_MS = 10_000L
        /** One `connect_timeout(2 s)` ceiling plus a frame from the requested IDR, with margin. */
        private const val VIDEO_REDIAL_MS = 5_000L
        /** [SessionTrace.Board] names for the four seams and the Surface-scoped renderer. */
        private const val BOARD_VIDEO = "seam-video"
        private const val BOARD_MEDIA = "seam-media"
        private const val BOARD_VOICE = "seam-voice"
        private const val BOARD_META = "seam-meta"
        private const val BOARD_RENDERER = "video-renderer"
        /** Touch counters — published on DOWN/UP/refusal and at session end, never per MOVE. */
        private const val BOARD_TOUCH = "touch"

        private fun boardFor(label: String): String = when (label) {
            "video" -> BOARD_VIDEO; "audio" -> BOARD_MEDIA; "voice" -> BOARD_VOICE; else -> BOARD_META
        }
    }

    private lateinit var surfaceView: SurfaceView
    private lateinit var root: android.widget.FrameLayout
    /** "Connection to iPhone interrupted" — see [linkQuiet]. Not clickable, so touches pass through to the Surface. */
    private var linkOverlay: android.widget.TextView? = null
    /** Index into [VIEW_AREAS] the surface is currently laid out for. */
    @Volatile private var viewAreaIndex = 0
    /** Cancels a pending shrink if another transition arrives first. */
    private var viewAreaGen = 0
    // @Volatile is load-bearing: written on the UI thread (attach/detachRenderer, stopSession) and
    // read on the cp-video thread. Without it the serve thread may legally observe a stale null after a
    // Surface returns — a permanent black screen plus the discard path below — or a stale stopped
    // renderer after a detach.
    @Volatile private var renderer: HevcRenderer? = null
    // @Volatile: published on `cp-setup`, read by the VoiceRouter duck callbacks on its consume
    // thread and nulled by stopSession on whichever thread ends the session.
    @Volatile private var player: AacPlayer? = null
    @Volatile private var voiceRouter: VoiceRouter? = null
    @Volatile private var micUplink: MicUplink? = null

    private val servers = CopyOnWriteArrayList<ServerSocket>()
    private val liveSockets = CopyOnWriteArrayList<Socket>()

    /**
     * Per-generation liveness. A single shared flag cannot distinguish "my generation was stopped"
     * from "a new generation started", which lets a stale listener thread bind the port and feed a
     * released decoder while the new generation gets EADDRINUSE — listeners alive, zero video.
     *
     * Written only under [sessionLock]; @Volatile for the lock-free readers ([onCommandPlist]).
     */
    @Volatile private var generation: AtomicBoolean? = null

    /**
     * Makes [stopSession] and the `cp-setup` thread's publication of each component mutually
     * exclusive. Setup builds a component OUTSIDE the lock, then takes the lock to check the
     * generation and publish it; stop takes the lock, kills the generation and stops everything
     * published so far. So a component is either published (and stop stops it) or built after the
     * stop (and setup tears it down itself) — never both unstopped. See [publish].
     */
    private val sessionLock = Any()

    /** Touch counters for the Board — see [send]. One writer thread (`cp-touch`); atomics for the readers. */
    private val touchDown = java.util.concurrent.atomic.AtomicInteger(0)
    private val touchMove = java.util.concurrent.atomic.AtomicInteger(0)
    private val touchUp = java.util.concurrent.atomic.AtomicInteger(0)
    private val touchRefused = java.util.concurrent.atomic.AtomicInteger(0)

    /** Per generation: [EXPECT_VIDEO_CONNECT] is armed from the first `/command` plist at most once.
     *  Written on `cp-meta` and the UI thread (reset in [startSession]); volatile is enough. */
    @Volatile private var videoConnectArmed = false
    /** Latched so a dead event channel logs one W per run of refused ForceKeyFrames, not one per request. */
    @Volatile private var keyframeReqFailed = false

    /** Touch must not run on the UI thread: a send can block on the event-channel lock for seconds. */
    private var touchThread: HandlerThread? = null
    private var touchHandler: Handler? = null
    // AtomicReference, not a plain @Volatile: the check-set-post sequence on a plain field has a
    // lost-wakeup race (reader nulls it between the UI thread's read and post) that freezes the rest
    // of a drag. getAndSet makes the drain atomic and self-coalescing.
    private val pendingMove = AtomicReference<Triple<Int, Float, Float>?>(null)
    /** Primary-pointer tracking for [onTouch] (UI thread only); pure, so it is unit-tested. */
    private val pointerFilter = PrimaryPointerFilter()

    /**
     * This instance's holder token for [CarPlaySessionService] (audit12 F094 / [C1]). A replacement
     * CarPlayActivity can launch before this one's onDestroy; keyed by token, this instance's late stop
     * cannot take down the service the replacement started.
     */
    private val fgsToken = CarPlaySessionService.newToken()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        live = java.lang.ref.WeakReference(this)
        // audit12 F030: MainActivity checks the receiver on the pump thread and starts this Activity
        // later; a stop landing in between found no screen to end (live was still null), and nothing
        // else takes this one down. So: publish `live` FIRST, then check. A stop landing after the
        // check finds `live` and ends this screen (its finish is posted behind this onCreate). A stop
        // landing between the publish and the check is caught here, or undone by onDestroy's
        // stopSession. Read order as MainActivity.isCurrentReceiver: the retire fence FIRST, then cpRx.
        val refusal = when {
            zeno.gmccpa.SessionHolder.retiringRx != null -> "the receiver is being stopped"
            zeno.gmccpa.SessionHolder.cpRx == null -> "no receiver"
            zeno.gmccpa.SessionHolder.cpRx?.sessionLive != true -> "no live CarPlay session on the current receiver"
            else -> null
        }
        if (refusal != null) {
            log.i("CarPlay screen not started: $refusal — finishing before the session starts")
            finish()
            return
        }
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        surfaceView = SurfaceView(this)
        // The surface is no longer the content view: it is a POSITIONED child of a panel-sized root,
        // so it can occupy a view area smaller than the panel and let GM's own chrome show around it.
        // The root stays full-bleed (2400x960) and is the coordinate space touch is normalised in —
        // see [onTouch]. Its background is what shows outside the active view area.
        root = android.widget.FrameLayout(this)
        root.addView(surfaceView, android.widget.FrameLayout.LayoutParams(DISPLAY_W, DISPLAY_H))
        setContentView(root)
        applySystemUi()
        // A quiet episode announced while no screen was up (backgrounded, recreated) still shows.
        if (overlayGen != 0L) showLinkOverlay(true)

        touchThread = HandlerThread("cp-touch").also { it.start(); touchHandler = Handler(it.looper) }

        // Seams + audio start HERE, not on surfaceCreated: they must outlive the Surface. Anything
        // that backgrounds this Activity (a GM dialog, reverse gear, a stray `am start`) destroys the
        // Surface, and if the sockets went with it the CarPlay session would be gone for good.
        startSession()

        surfaceView.setOnTouchListener { v, ev -> onTouch(v, ev) }
        surfaceView.holder.addCallback(object : SurfaceHolder.Callback {
            override fun surfaceCreated(h: SurfaceHolder) { attachRenderer(h) }
            override fun surfaceChanged(h: SurfaceHolder, f: Int, w: Int, ht: Int) {
                log.i("surface ${w}x$ht (advertised ${DISPLAY_W}x$DISPLAY_H)")
                // The view system owns this SurfaceControl and resets its geometry on relayout, so a
                // non-default view area has to be re-asserted here or the picture silently goes
                // full-bleed again after any layout pass.
                if (viewAreaIndex != 0) applyGeometry(VIEW_AREAS[viewAreaIndex])
            }
            override fun surfaceDestroyed(h: SurfaceHolder) { detachRenderer() }
        })
    }

    /**
     * System-bar VISIBILITY follows the view area. The LAYOUT is never touched.
     *
     * ## The distinction that matters
     * On this head unit AAOS insets the CONTENT, never the window: `mBounds` is the full 2400x960
     * panel in both states and `mWindowingMode=fullscreen` throughout
     * (`evidence/drive_20260818-132220/headunit.log:5762`). GM's LeftBar and TopCarSystemBar are
     * separate system-UI windows at a higher Z that OVERLAY our window — that is the default, not an
     * exception, and `docs/06_BRINGUP_RUNBOOK.md:352-354` measured it: "App window 2400x960 — full
     * panel", "System bar insets, normal left=189 top=118 — GM chrome overlays the panel".
     *
     * So the only thing that has to change per area is whether the bars are HIDDEN. The three LAYOUT_*
     * flags stay set permanently: they are what keep the decor from padding our content. Clearing
     * them alongside the hide bits moved the root to (189,118) and displaced the fixed 2400x960
     * surface — device-observed 2026-09-08 as the picture landing at 377,236. One line was needed and
     * two were changed.
     *
     * `FLAG_FULLSCREEN` has to move too. It comes from the theme (`Theme.NoTitleBar.Fullscreen`) and
     * suppresses the status bar INDEPENDENTLY of `systemUiVisibility`: clearing the sysui bits alone
     * brought the LeftBar back (`isReadyForDisplay=true`) while TopCarSystemBar stayed hidden behind
     * this flag.
     *
     * ## Geometry stays static
     * Both rects are known constants for this panel ([VIEW_AREAS]) — this app targets one radio and
     * one display, so nothing is measured, awaited or renegotiated at runtime.
     */
    private fun applySystemUi() {
        val immersive = viewAreaIndex == 0
        // Statement-scoped, as with systemUiVisibility below: the insets API would change layout.
        @Suppress("DEPRECATION")
        if (immersive) window.addFlags(WindowManager.LayoutParams.FLAG_FULLSCREEN)
        else window.clearFlags(WindowManager.LayoutParams.FLAG_FULLSCREEN)
        @Suppress("DEPRECATION")
        window.decorView.systemUiVisibility =
            // ALWAYS: keeps the root at (0,0) 2400x960 so the surface and the crop stay in panel space.
            View.SYSTEM_UI_FLAG_LAYOUT_STABLE or
            View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN or
            View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION or
            // Only these three toggle.
            if (immersive)
                View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY or
                View.SYSTEM_UI_FLAG_FULLSCREEN or
                View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
            else 0
    }

    private fun startSession() {
        val gen = AtomicBoolean(true)
        synchronized(sessionLock) {
            if (generation != null) { log.i("session already started"); return }
            generation = gen
            // Pin the process to foreground priority so backgrounding (GM dialog / reverse gear / app
            // switch) can't let LMK reclaim the live session. See CarPlaySessionService for the scope
            // limit. INSIDE sessionLock (audit12 F095): published-but-not-yet-started was a window in
            // which a stopSession on another thread stopped nothing and the start then orphaned the
            // service. Lock order sessionLock -> FgsGate lock, as in stopSession.
            // D-F11 (2026-10-08): a refused/failed foreground start must not crash the screen; the
            // session runs unheld, as AutoClaim.acquireLink does for the LINK hold. The service's own
            // E line names the cause; stopSession's stop(fgsToken) is harmless for a token never held.
            runCatching { CarPlaySessionService.start(this, fgsToken) }
                .onFailure { log.w("session FGS hold refused (${it.javaClass.simpleName}) — running the session unheld") }
        }
        videoConnectArmed = false

        // Everything else is built OFF the UI thread. The player, the voice router and four binds
        // (up to 3.6 s of retry — see BIND_RETRIES) used to run inline here, in onCreate, which is
        // the one place a stall is an ANR and a lost frame is visible. The Surface callbacks are
        // independent of this: attachRenderer needs only the holder, and the video seam reads
        // `renderer` at connect time, whichever side lands first.
        Thread({ setupSession(gen) }, "cp-setup").apply { isDaemon = true }.start()
    }

    /**
     * Publish one built component under [sessionLock], or tear it down if [gen] died meanwhile.
     * Returns false (component already torn down) when setup must stop.
     */
    private inline fun publish(gen: AtomicBoolean, what: String, teardown: () -> Unit, assign: () -> Unit): Boolean {
        synchronized(sessionLock) {
            if (gen.get()) { assign(); return true }
        }
        log.i("$what came up after the session was stopped — tearing it down, not publishing")
        runCatching(teardown).onFailure { log.w("$what teardown: ${it.javaClass.simpleName}: ${it.message}") }
        return false
    }

    /** The session's construction, on `cp-setup`. Every step publishes through [publish]. */
    private fun setupSession(gen: AtomicBoolean) {
        // Prime, don't just construct: the AudioTrack is built and playing before the seam even
        // connects, so the first ADTS frame decodes straight into a live track.
        // Pass the AudioManager so media can HOLD AUDIOFOCUS_GAIN — without a resting focus owner the
        // head unit's volume knob stays stuck on Phone/Siri after a transient holder abandons focus.
        // (Its focus callbacks are delivered on the main looper explicitly — see AacPlayer.requestFocus —
        // so building it here, on a thread with no Looper, changes nothing about where they run.)
        val p = AacPlayer(getSystemService(android.content.Context.AUDIO_SERVICE) as? android.media.AudioManager)
        // OD-9C: stock's `!hasFocus()` guard on the media-loss MainAudio take. Resolved at call time
        // through this screen's CURRENT router (built below), never captured.
        p.voiceFocusHeld = { voiceRouter?.holdsVoiceFocus() == true }
        p.start(); p.prime()
        if (!publish(gen, "media player", { p.stop("superseded before publish") }) { player = p }) return
        // The non-media half. Ducking is routed straight at the media track: every other purpose
        // plays at unity. This is the VOICE duck source only (plan v5.8 V3: the iPhone's duckAudio /
        // unduckAudio ramp); AacPlayer combines it with its own focus-driven duck (min of the two, one
        // VolumeShaper), so an AUDIOFOCUS_GAIN cannot cancel a live prompt's duck.
        val vr = VoiceRouter(
            this,
            onDuckRamp = { target, ms -> player?.setVoiceDuckRamp(target, ms) },
            // Pausing (not ducking) media is what lets the volume knob reach the voice group while
            // Siri speaks — MUSIC outranks VOICE_COMMAND, and a ducked track still counts as active.
            onAssistant = { speaking -> player?.setAssistantSpeaking(speaking) },
            // B-V3-1: the duck executor samples the shaper at start + durationMs + 20 ms.
            duckSampler = { player?.sampleDuck() ?: DuckSample(null, false) },
        ).also { it.start() }
        if (!publish(gen, "voice router", { vr.stop("superseded before publish") }) { voiceRouter = vr }) return
        // Connect eagerly: the same socket carries the capture gate, so a data-triggered connect
        // would deadlock (no connection -> no gate -> no capture -> no data).
        val mu = MicUplink().also { it.start() }
        if (!publish(gen, "mic uplink", { mu.stop("superseded before publish") }) { micUplink = mu }) return

        // A bind failure is still visible and still ends the screen (failStart) — it is just no
        // longer the UI thread that waits for it. Each listener is published the moment it is bound,
        // so a stop that lands mid-sequence closes exactly the ones that exist.
        val vs = bindOrNull(9001) ?: run { failStart(gen, "video"); return }
        // Each Board.up sits INSIDE the assign lambda: under [sessionLock] and only for a live
        // generation, so a stop racing setup can never see a seam posted UP after it was torn down.
        if (!publish(gen, "video seam", { vs.close() }) { servers.add(vs); SessionTrace.Board.up(BOARD_VIDEO, "bound :9001, listening") }) return
        val aus = bindOrNull(9002) ?: run { failStart(gen, "audio"); return }
        if (!publish(gen, "audio seam", { aus.close() }) { servers.add(aus); SessionTrace.Board.up(BOARD_MEDIA, "bound :9002, listening") }) return

        // The video seam is served whether or not a Surface exists, and the connection is HELD either
        // way. With no renderer we drain and DISCARD.
        //
        // Closing instead produced a ~30-60 Hz accept/close storm: `forward_screen2`
        // (`crates/vendor/receiver/src/session.rs`) re-dials on the very next frame with NO backoff —
        // `connect_timeout(2 s)` per call, `Err` returns and the next frame retries; the 2 s is a ceiling
        // on failure, not a delay — and fires a ForceKeyFrame on EVERY successful connect, from the
        // spawned `send_force_key_frame_stream` thread in the same fn. That is one connect plus one
        // encrypted event-channel command per frame,
        // taken on the GLOBAL event mutex that also carries touch, and it makes iOS emit all-IDR at
        // roughly 10x bitrate, degrading audio too. "Don't accept yet" would not have helped: on Linux
        // connect() to a bound listener completes in the kernel without accept().
        //
        // ocbmd — the other consumer of this exact seam — accepts unconditionally and back-pressures or
        // drops FRAMES, never the connection; so does the audio seam three lines below.
        serve(gen, vs, "video") { ins ->
            val r = renderer
            if (r != null) r.consume(ins) else discardUntilRenderer(gen, ins)
        }
        serve(gen, aus, "audio") { ins -> p.consume(ins) }

        // :9003 carries every NON-media audioType — Siri, telephony, alerts, navigation prompts
        // (session.rs routes them there, tagged `[rate u32 BE][ch u16 BE][atype u8][len u32 BE][AU]`).
        // `VoiceRouter.consume` decodes it — a real AAC-ELD decode into USAGE_VOICE_COMMUNICATION /
        // USAGE_ASSISTANT tracks, with the media duck driven from it. Independently of that, the seam
        // MUST be bound: `forward_to_sink` logs its failed connect once per RTP packet with no
        // transition gate, so the first nav prompt of a drive produces ~50 log lines a second,
        // permanently, burying every other diagnostic in the ring buffer.
        //
        // Reading continuously is required, not just decoding — accepting without reading fills the
        // socket buffer and stalls the producer's write for its full 2 s timeout. Best-effort by
        // design: voice is optional, so a bind failure must NOT take video and media audio down with it.
        val vos = bindOrNull(9003)
        if (vos != null) {
            if (!publish(gen, "voice seam", { vos.close() }) { servers.add(vos); SessionTrace.Board.up(BOARD_VOICE, "bound :9003, listening") }) return
            serve(gen, vos, "voice") { ins -> voiceRouter?.consume(ins) }
        } else {
            log.e("voice seam :9003 could not be bound — Siri/telephony/nav audio is discarded AND the "
                + "producer will log a failed connect per packet; continuing without it")
            // note(), not failed(): the E above is the severity.
            SessionTrace.Board.note(BOARD_VOICE, "FAILED (bind :9003 refused — Siri/call/nav audio discarded this session)")
        }
        // :9004 carries now-playing metadata and album art as `[u32 BE "META"][u32 BE len][marker]
        // [payload]`. Bound here rather than in CarPlaySessionService because the service is
        // priority-only by its own KDoc — session ownership still lives in this Activity (T2.2 open),
        // and a lone seam with a different owner and a different generation guard is exactly the
        // stale-listener / EADDRINUSE shape the per-generation AtomicBoolean exists to prevent.
        //
        // Best-effort like :9003, but for the opposite reason. An UNBOUND port here is cheap: the
        // producer connects lazily per record and warns once (the `WARNED.swap` block in
        // `crates/vendor/iap2-core/src/metadata.rs`), unlike the voice
        // seam's per-packet log storm. An accepted-but-unread one is the expensive case — the producer
        // writes under a SINK mutex it SHARES with the iAP2 reader, and since the core is in-process
        // that reader is our own thread, so a stalled consumer here stalls iAP2 ingest for the whole
        // session. Bind, and always drain.
        val mts = bindOrNull(9004)
        if (mts != null) {
            if (!publish(gen, "metadata seam", { mts.close() }) { servers.add(mts); SessionTrace.Board.up(BOARD_META, "bound :9004, listening") }) return
            val seam = MetadataSeam(ProbeLog.sub("meta")) { m, pl ->
                // /command plists are a different plane from now-playing; keep NowPlayingState purely
                // about media rather than teaching it about view areas.
                if (m == MetadataSeam.META_CMD) onCommandPlist(pl) else nowPlaying.dispatch(m, pl)
            }
            serve(gen, mts, "meta") { ins -> seam.consume(gen, ins) }
        } else {
            log.e("metadata seam :9004 could not be bound — the now-playing card stays empty; continuing")
            SessionTrace.Board.note(BOARD_META, "FAILED (bind :9004 refused — now-playing card empty, view-area and modesChanged commands lost this session)")
        }

        log.i("session up: seams listening on :9001 (HEVC), :9002 (AAC-LC), :9003 (voice, routed), "
            + ":9004 (metadata); audio is surface-independent")
    }

    /** Surface-scoped. The codec cannot outlive the Surface it renders into. */
    private fun attachRenderer(holder: SurfaceHolder) {
        if (renderer != null) { log.i("renderer already attached"); return }
        val r = HevcRenderer(DISPLAY_W, DISPLAY_H, holder.surface) { requestKeyframe() }
        r.start()
        renderer = r
        // Let the SESSION line SAMPLE the counters at emit rather than depending on HevcRenderer.stop()
        // having pushed them: stop() never runs on `superseded`/`process_death` and races `host_gone`,
        // which is how a session that observed a first frame still reported frames=0. Three AtomicLong
        // reads, once per session end — not a hook on the per-frame path.
        //
        // The closure captures the three AtomicLongs ONLY (audit12 F082). Capturing `r` pinned the
        // renderer and, through its keyframe callback, this Activity: stopSession nulls `renderer`
        // first, so detachRenderer returns early and never clears the source, and a finished
        // CarPlayActivity stayed reachable from SessionSummary. It is not cleared in stopSession
        // either, because the SESSION line samples it at emit, which can come after the stop.
        val framesRendered = r.framesRendered
        val ausDropped = r.ausDropped
        val bytesIn = r.bytesIn
        zeno.gmccpa.logging.SessionSummary.current()?.avCountersSource =
            { longArrayOf(framesRendered.get(), ausDropped.get(), bytesIn.get()) }
        log.i("renderer attached to Surface")
        SessionTrace.Board.up(BOARD_RENDERER, "Surface attached — decoding ${DISPLAY_W}x$DISPLAY_H into it")
        // Any seam connection already open belongs to the previous renderer; drop it so the producer
        // re-dials into this one and sends a fresh IDR.
        val bounced = liveSockets.filter { it.localPort == 9001 }
        bounced.forEach { runCatching { it.close() } }   // deliberate: the bounce IS the close
        if (bounced.isNotEmpty()) {
            // A connection WAS live, so the producer is demonstrably dialling and owes us a re-dial —
            // this is the only in-file precondition strong enough to arm on (see EXPECT_VIDEO_CONNECT).
            SessionTrace.expect(EXPECT_VIDEO_CONNECT, VIDEO_REDIAL_MS,
                "the Surface came back and the live :9001 connection was bounced, so forward.rs " +
                "should re-dial on its next frame (a ForceKeyFrame is going out now to produce one)")
        }
        // ASK iOS for the IDR rather than waiting to be given one.
        //
        // A fresh codec cannot render anything until an IDR arrives, so every re-attach — the driver
        // going Home and back, a GM dialog, reverse gear — shows a black screen until then. Owner
        // reported this 2026-09-08 on return from the homescreen. The socket bounce above only makes
        // the PRODUCER re-dial; how soon a keyframe follows is iOS's choice, and in practice it is
        // long enough to look broken.
        //
        // Ordered strictly AFTER the close: the request goes out on the event channel and the IDR
        // comes back down the seam, so the connection that will carry it must be the new one. Request
        // first and the answer can land on a socket we are about to drop.
        //
        // This does NOT keep the decoder alive across the gap — a SurfaceView's Surface dies with the
        // Activity and the codec dies with it. It only shortens the black window to about one round
        // trip. Keeping the decoder running across a background (MediaCodec.setOutputSurface onto a
        // parking Surface) was built and tested on this rig 2026-09-08: the Intel decoder ACCEPTED the
        // swap, but the ImageReader drain was hung on the touch thread, which makes blocking
        // event-channel calls — it stalled the drain, back-pressured the decoder and produced visible
        // video anomalies. Pulled. If it is retried, the drain needs its own thread.
        requestKeyframe()
    }

    private fun detachRenderer() {
        val r = renderer ?: return
        renderer = null
        // Drop the sampling closure before stop(); stop()'s own push stays the clean-path value.
        zeno.gmccpa.logging.SessionSummary.current()?.avCountersSource = null
        r.stop("Surface destroyed")
        // Unblock the consume thread parked in read(); it releases the codec on its own thread.
        liveSockets.filter { it.localPort == 9001 }.forEach { runCatching { it.close() } }   // deliberate: the unblock IS the close
        log.i("renderer detached — audio and seams stay up")
        SessionTrace.Board.down(BOARD_RENDERER, "Surface destroyed — audio and seams stay up; frames are discarded until a Surface returns")
    }

    /**
     * No Surface: keep the seam connected and throw the frames away.
     *
     * Draining is not optional. Accepting but not reading fills the socket buffer, stalls the producer's
     * `write_two` for the full 2 s SO_SNDTIMEO (`forward_screen2`'s `set_write_timeout`,
     * `crates/vendor/receiver/src/session.rs`), then drops the connection anyway —
     * back-pressuring the screen thread and, through it, the iPhone's screen socket. Reading and
     * discarding costs one read plus a memcpy into a reused buffer.
     *
     * Framing is the seam's own `[u32 BE len][payload]`, read exactly as [HevcRenderer.consume] reads
     * it, so a desync cannot make this allocate — the payload is skipped, never allocated. A 0-length
     * message is legal (`spawn_screen` calls `forward_screen` unconditionally with whatever the
     * conversion produced, `crates/vendor/receiver/src/session.rs`) and is not a desync.
     *
     * Returns as soon as a renderer appears; [attachRenderer] then closes the socket so the producer
     * re-dials into the new renderer with ONE fresh ForceKeyFrame — the designed heal path. If no frames
     * are arriving we are parked in read(), and that same close is what unblocks us.
     */
    private fun discardUntilRenderer(gen: AtomicBoolean, ins: java.io.InputStream) {
        log.i("video seam connected with no Surface — holding open, discarding until one returns")
        SessionTrace.Board.up(BOARD_VIDEO, "connected :9001, no Surface — discarding frames")
        val hdr = ByteArray(4)
        val sink = ByteArray(32 * 1024)
        var msgs = 0L
        var bytes = 0L
        while (gen.get() && renderer == null) {
            if (!readFully(ins, hdr, 4)) break
            val len = ((hdr[0].toInt() and 0xFF) shl 24) or ((hdr[1].toInt() and 0xFF) shl 16) or
                      ((hdr[2].toInt() and 0xFF) shl 8) or (hdr[3].toInt() and 0xFF)
            if (len < 0 || len > MAX_MESSAGE) {
                log.e("implausible seam message length $len — desync, dropping connection")
                break
            }
            if (len > 0 && !skipFully(ins, len, sink)) break
            msgs++; bytes += (len + 4).toLong()
        }
        log.i("discarded $msgs frames ($bytes B) with no Surface")
    }

    // deliberate (both readers): on the discard path a read exception is the close that
    // [attachRenderer] issues to make the producer re-dial — the designed heal, not a fault; the
    // discard summary line and the serve loop's next accept say what happened.
    private fun readFully(ins: java.io.InputStream, dst: ByteArray, n: Int): Boolean {
        var off = 0
        while (off < n) {
            val r = try { ins.read(dst, off, n - off) } catch (e: Exception) { return false }
            if (r <= 0) return false
            off += r
        }
        return true
    }

    /** Skip `n` bytes without allocating them. Socket skip() may short-count or block; read instead. */
    private fun skipFully(ins: java.io.InputStream, n: Int, sink: ByteArray): Boolean {
        var left = n
        while (left > 0) {
            val want = if (left < sink.size) left else sink.size
            val r = try { ins.read(sink, 0, want) } catch (e: Exception) { return false }
            if (r <= 0) return false
            left -= r
        }
        return true
    }

    private fun failStart(gen: AtomicBoolean, which: String) {
        // Never after stopSession: a bind that lost because the session was torn down under it
        // (the port is being closed, or was never ours to keep) is not a fault and must not
        // finish() a screen that is already being replaced.
        if (!gen.get()) { log.i("$which seam bind abandoned — the session was stopped during setup"); return }
        log.e("$which seam could not be bound after $BIND_RETRIES attempts — no A/V; finishing")
        // note(), not failed(): the E above is the severity.
        SessionTrace.Board.note(boardFor(which), "FAILED (bind refused after $BIND_RETRIES attempts — no A/V, screen finishing)")
        stopSession("$which seam bind failed")
        runOnUiThread { finish() }
    }

    /**
     * Bind loopback with SO_REUSEADDR and a bounded retry. We perform the active close on the accepted
     * sockets, so the listener tuple can sit in TIME_WAIT across a quick stop/start; and a stale
     * generation may still hold the port for a moment.
     */
    private fun bindOrNull(port: Int): ServerSocket? {
        repeat(BIND_RETRIES) { attempt ->
            var ss: ServerSocket? = null
            try {
                ss = ServerSocket()
                ss.reuseAddress = true
                ss.bind(InetSocketAddress(InetAddress.getByName("127.0.0.1"), port), 4)
                return ss
            } catch (e: Exception) {
                runCatching { ss?.close() }   // deliberate: discarding a socket whose bind failed
                log.e("bind :$port attempt ${attempt + 1}/$BIND_RETRIES failed: ${e.message}")
                try { Thread.sleep(BIND_BACKOFF_MS) } catch (_: InterruptedException) { return null }
            }
        }
        return null
    }

    private fun serve(gen: AtomicBoolean, srv: ServerSocket, label: String, body: (java.io.InputStream) -> Unit) {
        val port = srv.localPort
        val board = boardFor(label)
        Thread({
            var acceptFailures = 0
            try {
                while (gen.get()) {
                    // Throwable, as for body() below: an OOM allocating the Socket is an Error.
                    val s = try { srv.accept() } catch (t: Throwable) {
                        // Ordinary teardown: stopSession flips `gen` BEFORE it closes this server
                        // socket, so a throw that finds `gen` false is the close we asked for.
                        if (!gen.get()) break
                        // Q010 (v5.8 V7): this used to `break` on any throw, which retired the
                        // listener for the rest of the session (the finally closes srv) — one
                        // transient EMFILE/ENOBUFS/OOM on accept() meant a black screen or silent
                        // media with the session still up. CarPlayRx.acceptLoop's rule instead:
                        // retry with a pause while live, give up only when sustained or when the
                        // socket is really gone — and say so on the Board either way.
                        if (srv.isClosed) {
                            log.e("$label accept: socket closed while live — ${t.javaClass.simpleName}: ${t.message}")
                            SessionTrace.Board.failed(board, "server socket closed with the session live — listener retired (${t.javaClass.simpleName})")
                            break
                        }
                        if (++acceptFailures >= SEAM_ACCEPT_FAILURES_FATAL) {
                            log.e("$label accept: $acceptFailures consecutive failures — retiring the listener: ${t.javaClass.simpleName}: ${t.message}")
                            SessionTrace.Board.failed(board, "accept failed $acceptFailures times in a row — listener retired (${t.javaClass.simpleName})")
                            break
                        }
                        log.w("$label accept failed ($acceptFailures/$SEAM_ACCEPT_FAILURES_FATAL) — ${t.javaClass.simpleName}: ${t.message}; retrying in $SEAM_ACCEPT_RETRY_MS ms")
                        try { Thread.sleep(SEAM_ACCEPT_RETRY_MS) } catch (_: InterruptedException) { break }
                        continue
                    }
                    acceptFailures = 0
                    liveSockets.add(s)
                    log.i("$label seam connected")
                    SessionTrace.Board.up(board, "connected :$port")
                    if (label == "video") SessionTrace.met(EXPECT_VIDEO_CONNECT)
                    // Catch Throwable, not Exception: an 8 MB per-message allocation can throw
                    // OutOfMemoryError (an Error), which would otherwise escape to the outer finally
                    // and permanently close this listener — a black screen with a live session. Only
                    // this one connection should die; the listener must keep accepting.
                    try { body(s.getInputStream()) } catch (e: Throwable) { log.e("$label: ${e.javaClass.simpleName}: ${e.message}") }
                    finally {
                        liveSockets.remove(s); runCatching { s.close() }   // deliberate: teardown
                        if (gen.get()) SessionTrace.Board.up(board, "bound :$port, listening (producer disconnected)")
                    }
                }
            } finally {
                runCatching { srv.close() }   // deliberate: never leave a bound socket behind
            }
        }, "cp-$label").apply { isDaemon = true }.start()
    }

    /**
     * The single teardown choke point — [failStart], [onDestroy] and [endSession] all come through
     * here — and therefore the owner of the `av/` expectation scope: every expectation the A/V
     * components armed is moot once the AirPlay session is gone, and left armed each would fire as an
     * `EXPECTED-MISSING` a few seconds after a phone that simply walked away. Scoped, not
     * [SessionTrace.cancelAll]: the phone leaving does not end the OCBM link, and a box-side
     * expectation in flight at that moment must survive.
     *
     * [why] is threaded into every component's `stop` and every board entry, so a capture answers
     * "was this teardown expected" from the lines themselves.
     */
    private fun stopSession(why: String) = synchronized(sessionLock) {
        val gen = generation ?: return@synchronized
        generation = null
        gen.set(false)
        renderer?.stop(why); renderer = null
        player?.stop(why)
        // Flag-only, like the player: VoiceRouter releases its codecs/tracks on the consume thread.
        // Skipping this strands up to four AudioTracks AND their focus requests, and an unabandoned
        // USAGE_VOICE_COMMUNICATION request keeps the hardware volume keys pinned to the call group
        // for the rest of the session.
        voiceRouter?.stop(why)
        micUplink?.stop(why)
        // Closing the accepted socket is the only reliable way to unblock a consumer parked in read().
        liveSockets.forEach { runCatching { it.close() } }; liveSockets.clear()   // deliberate: teardown
        servers.forEach { runCatching { it.close() } }; servers.clear()           // deliberate: teardown
        player = null
        voiceRouter = null
        micUplink = null
        // Token-keyed and gated: deferred while this instance's start has not reached startForeground,
        // and never stops a replacement instance's service (audit12 F094, [C1]).
        CarPlaySessionService.stop(this, fgsToken)
        // No nowPlaying.clear() here (plan v5.9 MED M4): this also runs on an Activity destroy with
        // the session still up, where clearing blanked the merge base the next delta builds on. The
        // clear (and the idle card) happen on the session's end, in CarPlayMediaBrowserService.onSessionDown.
        for (b in listOf(BOARD_VIDEO, BOARD_MEDIA, BOARD_VOICE, BOARD_META)) SessionTrace.Board.down(b, "session stopped — $why")
        SessionTrace.Board.down(BOARD_RENDERER, "session stopped — $why")
        // Final touch tally, only if the screen was ever touched — otherwise the entry would be
        // registered just to be marked down.
        if (touchDown.get() + touchMove.get() + touchUp.get() + touchRefused.get() > 0)
            SessionTrace.Board.down(BOARD_TOUCH, "session stopped — $why; final ${touchTally()}")
        // After the components' own stop(): those that could cancel synchronously have, with a more
        // specific reason; what remains is on threads still unwinding, and the session reason is
        // the honest one for it. A later per-name cancel from those threads is then a silent no-op.
        SessionTrace.cancelScope("av/", "A/V session stopped — $why")
        log.i("session stopped — $why")
    }

    /**
     * Ask iOS for a fresh IDR. Off the UI thread — it rides the same blocking event channel.
     *
     * The result used to be discarded. A refused ForceKeyFrame is a dead event channel under a
     * black screen — the one symptom that has no other line. Once per run of refusals, not per
     * request: [HevcRenderer.requestKeyframe] re-issues every 500 ms while non-IRAP AUs arrive.
     */
    private fun requestKeyframe() {
        touchHandler?.post {
            if (NativeCore.forceKeyFrame()) {
                keyframeReqFailed = false
            } else if (!keyframeReqFailed) {
                keyframeReqFailed = true
                log.w("ForceKeyFrame refused by the event channel — no IDR is coming; the screen stays black until it recovers")
            }
        }
    }

    /**
     * An inbound `/command` plist off the `:9004` seam.
     *
     * Four commands are acted on. `requestViewArea` lays the surface out. `modesChanged` is decoded
     * for its Speech app state and handed to [VoiceRouter.onModes] (A11): iOS states "Siri is up /
     * Siri is done" on this channel ~400 ms before the first voice AU and ~3.3 s before the energy
     * gate can infer the end. `duckAudio` / `unduckAudio` bracket each nav prompt and go to
     * [VoiceRouter.onDuckCommand] with the whole plist: the router reads `params.volume` (dB) and
     * `params.durationMs` and ramps the media duck to them at once (plan v5.8 V3, stock
     * AirPlayReceiverPOSIX.c:387-418), and the NAV worker opens / binds its prompt epoch on them.
     * (Corrected 2026-10-07: the old "duckAudio did not arrive once" held for the 2026-09-09 session
     * only; cp7 has it at every nav prompt.) This seam is best-effort — `emit_command_plist` try_locks
     * and drops under a now-playing or artwork write — so either edge of a pair can be lost; a lost
     * unduck is restored at the END of the prompt's stream. Everything else
     * (`setNightMode`, `flushAudio`, …) is ignored rather than half-handled.
     *
     * Runs on the `cp-meta` seam thread and must not block: volatile writes into the router, and the
     * layout change posted to the UI thread.
     */
    private fun onCommandPlist(payload: ByteArray) {
        val root = BPlist.parse(payload) ?: return
        val type = BPlist.str(root, "type")
        if (!videoConnectArmed && generation?.get() == true) {
            // First /command of this generation: iOS is past RECORD, so the screen stream is owed.
            // Armed only if no video connection is already live (a re-attach mid-session may have
            // beaten this plist); once per generation either way. See EXPECT_VIDEO_CONNECT.
            videoConnectArmed = true
            if (liveSockets.none { it.localPort == 9001 }) {
                SessionTrace.expect(EXPECT_VIDEO_CONNECT, VIDEO_CONNECT_MS,
                    "iOS sent its first /command plist (type=$type) on the event channel accepted at " +
                    "RECORD, so it should SETUP the screen stream and forward.rs should dial :9001 " +
                    "(730ms after RECORD on 2026-09-09)")
            }
        }
        when (type) {
            "modesChanged" -> onModesChanged(root, payload.size)
            "duckAudio" -> voiceRouter?.onDuckCommand(true, root)
            "unduckAudio" -> voiceRouter?.onDuckCommand(false, root)
            "requestViewArea" -> {
                val idx = BPlist.int(root, "params", "viewAreaIndex")?.toInt() ?: return
                if (idx !in VIEW_AREAS.indices) {
                    log.w("requestViewArea index=$idx outside the ${VIEW_AREAS.size} declared areas — ignoring")
                    return
                }
                log.i("requestViewArea index=$idx -> laying the surface out at ${VIEW_AREAS[idx]}")
                runOnUiThread { applyViewArea(idx) }
            }
        }
    }

    /**
     * Decode `params.appStates[]{appStateID, entity, speechMode}` out of a `modesChanged`.
     *
     * The shape is byte-proven, not documented: every frame in
     * `docs/ops/captures/2026-07-24_carplay_cmd_capture.bin` carries exactly this plus
     * `resources[]{resourceID, entity, permanentEntity}`, and appStateID 1 is the one entry that
     * carries `speechMode` — Apple's Speech app state. `-1` is NotApplicable; in Apple's writer it is
     * the frame's only 8-byte integer, and its departure is the whole of the 287 B → 277 B bracket
     * around the Siri turn in the 2026-09-09 capture. appStateID 2/3 are believed to be
     * PhoneCall/TurnByTurn (AirPlayCommon.h order, unverified in this checkout) and are passed for
     * logging only. `BPlist` decodes arrays to `List` and dicts to `Map<String, Any?>` with `Long`
     * integers, which is what this destructures; an unexpected shape yields `-1`, i.e. "not
     * applicable", i.e. today's energy-gate behaviour.
     */
    private fun onModesChanged(root: Any?, size: Int) {
        val m = ModesChanged.decode(root) ?: return   // the pure decode; ModesChangedTest holds it
        voiceRouter?.onModes(m.speechMode, m.speechEntity, m.phoneEntity, m.turnsEntity, size)
    }

    /**
     * Move the visible picture to a declared view area by CROPPING on the hardware composer.
     *
     * ## Why a crop, and why not a layout change
     * iOS keeps encoding the FULL panel and draws its UI into the view-area sub-rect, filling the
     * rest of the frame with black — device-observed 2026-09-08 (130 parameter-set bursts, identical
     * CSD, zero decoder reconfigurations), matching upstream's "255 rect updates, coded size
     * constant". So the buffer is always 2400x960 with content at, say, 1416x842@(188,118).
     *
     * Shrinking the SurfaceView's LAYOUT would make SurfaceFlinger scale that whole frame into the
     * smaller window — the UI would appear at 59% and mispositioned. The correct operation is a crop:
     * show only the sub-rect, 1:1, at its own origin.
     *
     * ## Why this stays on the overlay
     * A SurfaceView cannot crop its buffer through the View API, but the hardware composer is already
     * doing exactly this per layer — the baseline dump shows `sourceCrop` and `displayFrame` as
     * separate rects with `composition=DEVICE (2)` and `usesClientComposition=false`, i.e. the whole
     * display scans out with no GPU. [SurfaceControl.Transaction.setGeometry] exposes those two rects
     * to the app (API 31; this unit is 32), so the crop costs nothing: no GPU composition, no extra
     * copy, no added latency. A TextureView would achieve the same picture by moving 2400x960 60 fps
     * HEVC onto GPU composition, which is the cost this avoids.
     *
     * If HWC cannot satisfy the crop it silently falls back, and that is directly observable in the
     * same dump as `forceClientComposition=true` — so this verifies itself rather than needing trust.
     *
     * ## Re-applying
     * The view system owns this SurfaceControl and resets its geometry on relayout, so the current
     * area is re-applied from `surfaceChanged` as well as from here.
     */
    private fun applyViewArea(index: Int) {
        if (index !in VIEW_AREAS.indices) return
        val from = VIEW_AREAS[viewAreaIndex]
        val to = VIEW_AREAS[index]
        viewAreaIndex = index
        log.i("view area [$index] $to — cropping on the compositor over ${VIEW_AREA_ANIM_MS}ms")
        if (from == to) { applyGeometry(to); return }

        // ASYMMETRIC, and not interpolated. We cannot see iOS's per-step rect — the screen header
        // that carries it is consumed in-process before the seam — so any ramp of ours runs on a
        // different clock from its animation. Device-observed 2026-09-08: an interpolated ramp looked
        // right growing and wrong shrinking, because on the way down our crop LEADS iOS and its
        // animation then plays out inside an already-clipped window.
        //
        // Both directions are correct if the crop is never smaller than iOS's current content:
        //   GROW   apply the target at once. The extra frame area we reveal is iOS's own black until
        //          its UI expands into it — which is exactly what was on screen before.
        //   SHRINK wait out the animation, then crop. For those 3 s the surround is iOS's black
        //          rather than GM's chrome, and it snaps at the end; clipping the animation is worse.
        //
        // Generation-guarded so a second request during the delay cancels the pending one rather
        // than cropping to a stale area after it.
        // Bars move WITH the crop, same asymmetry and for the same reason.
        //   GROW   hide the bars and reveal the frame together; the picture must never expand under
        //          a bar that is still on screen.
        //   SHRINK stay immersive for the whole animation (iOS's own black surrounds its shrinking
        //          UI), then crop and reveal GM's chrome as one event at the end.
        // Safe to schedule blind because the duration is OURS: the receiver sends iOS
        // animationDurationMillis=3000 in updateViewArea.
        val gen = ++viewAreaGen
        val growing = to.width() * to.height() >= from.width() * from.height()
        if (growing) {
            applySystemUi()
            applyGeometry(to)
        } else {
            surfaceView.postDelayed({
                if (gen != viewAreaGen) return@postDelayed
                applyGeometry(to)
                applySystemUi()
            }, VIEW_AREA_ANIM_MS)
        }
    }

    /**
     * Crop the layer to [r] and place it at the same rect — 1:1, no scaling, since the view area is
     * expressed in the same panel coordinates as the buffer.
     *
     * Guarded on API 31 for [SurfaceControl.Transaction.setGeometry]; below that the picture simply
     * stays full-bleed, which is the behaviour this app shipped with.
     */
    private fun applyGeometry(r: android.graphics.Rect) {
        if (android.os.Build.VERSION.SDK_INT < 31) return
        val sc = surfaceView.surfaceControl
        if (sc == null || !sc.isValid) return
        runCatching {
            android.view.SurfaceControl.Transaction().use { t ->
                t.setGeometry(sc, r, r, android.view.Surface.ROTATION_0)
                t.apply()
            }
        }.onFailure { log.w("setGeometry $r failed: ${it.javaClass.simpleName}: ${it.message}") }
    }

    /**
     * Touch → HID report to the iPhone, dispatched off the UI thread.
     *
     * A send takes the event-channel lock and can block on a stalled socket write for seconds; doing
     * that inline from `onTouch` risks an ANR. MOVEs are coalesced latest-wins (a stale MOVE has no
     * value once a newer one exists) while DOWN and UP are never dropped and keep their order.
     *
     * Single-touch: only the primary pointer is tracked. Without that, a second finger lifting sends
     * UP while the first is still down, and iOS sees the gesture end mid-drag. Once the primary has
     * lifted, MOVE and CANCEL are dropped until the next ACTION_DOWN (audit12 F081): phase 1 is a
     * tip-DOWN on the wire, so a survivor's MOVE was a phantom touch. See [PrimaryPointerFilter].
     */
    private fun onTouch(v: View, ev: MotionEvent): Boolean {
        if (v.width <= 0 || v.height <= 0) return false
        val phase = pointerFilter.decide(ev.actionMasked, ev.actionIndex, ev.getPointerId(ev.actionIndex)) {
            ev.findPointerIndex(it)
        }
        if (phase == PrimaryPointerFilter.UNHANDLED) return false
        if (phase == PrimaryPointerFilter.CONSUMED) return true
        // The lifting pointer's own position for an UP; the primary's for DOWN/MOVE/CANCEL.
        val idx = pointerFilter.index
        // Normalised against the PANEL, not the view. `nativeTouch` scales by DISPLAY_W/H to produce
        // an ABSOLUTE report in the advertised /info displays[] geometry, and that geometry is the
        // panel — a view area is a sub-rect INSIDE that space, never a new coordinate system. Once the
        // surface can be smaller than the panel, `ev.getX / v.width` would make the video's left edge
        // report x=0 instead of the view area's origin, offsetting and stretching every tap.
        val nx = ((v.left + ev.getX(idx)) / DISPLAY_W.toFloat()).coerceIn(0f, 1f)
        val ny = ((v.top + ev.getY(idx)) / DISPLAY_H.toFloat()).coerceIn(0f, 1f)

        if (phase == 1) {
            // Latest-wins, lost-wakeup-free: set the pending MOVE and post a drain that atomically
            // takes it. Extra posts are cheap and self-coalesce (a later drain finds null).
            pendingMove.set(Triple(1, nx, ny))
            touchHandler?.post { pendingMove.getAndSet(null)?.let { (p, x, y) -> send(p, x, y) } }
        } else if (phase == PrimaryPointerFilter.PHASE_CANCEL) {
            // The HID report is [buttons][x][y] and the JNI maps phase 2 to a tip-up: there is no cancel
            // semantic on the wire, so folding CANCEL into the UP arm delivered an ABORTED gesture to
            // iOS as a completed lift — a phantom tap at the last coordinates. iOS's tap recogniser
            // instead FAILS once the touch moves past its allowable movement, so displace beyond tap
            // slop and lift there. Worst case is a few pixels of scroll, which is strictly better.
            // Both go through touchHandler so they keep their order behind any in-flight MOVE.
            pendingMove.set(null)
            val cx = (if (nx + CANCEL_SLOP_N <= 1f) nx + CANCEL_SLOP_N else nx - CANCEL_SLOP_N)
                .coerceIn(0f, 1f)
            touchHandler?.post {
                log.i("touch CANCEL→displaced UP")
                send(1, cx, ny)
                send(2, cx, ny)
            }
        } else {
            pendingMove.set(null)
            touchHandler?.post { send(phase, nx, ny) }
        }
        if (phase == 0) v.performClick()
        return true
    }

    private fun send(phase: Int, nx: Float, ny: Float) {
        val sent = NativeCore.touch(phase, nx, ny, DISPLAY_W, DISPLAY_H)
        (when (phase) { 0 -> touchDown; 1 -> touchMove; else -> touchUp }).incrementAndGet()
        if (!sent) touchRefused.incrementAndGet()
        // Log every DOWN/UP and every failure — a dying event channel is otherwise invisible mid-drag.
        // A refused send is a W: it IS the dying event channel, and at I it was indistinguishable
        // from a healthy tap in a grep for problems.
        // The Board tally rides the same cadence: a refusal, a DOWN or an UP — never a MOVE, which
        // would be a STATUS line per sample of every drag. The periodic dump carries it between.
        // The Board post is gated on a live generation under [sessionLock] so a touch draining off
        // `cp-touch` after stopSession cannot post UP over the DOWN it wrote; the send itself is not.
        if (!sent) {
            log.w("touch ${phaseName(phase)} n=(%.3f, %.3f) sent=false — the event channel refused it".format(nx, ny))
            synchronized(sessionLock) { if (generation?.get() == true) SessionTrace.Board.up(BOARD_TOUCH, touchTally()) }
        } else if (phase != 1) {
            log.i("touch ${phaseName(phase)} n=(%.3f, %.3f) sent=true".format(nx, ny))
            synchronized(sessionLock) { if (generation?.get() == true) SessionTrace.Board.up(BOARD_TOUCH, touchTally()) }
        }
    }

    private fun touchTally(): String =
        "down=${touchDown.get()} move=${touchMove.get()} up=${touchUp.get()} refused=${touchRefused.get()}"

    private fun phaseName(p: Int) = when (p) { 0 -> "down"; 1 -> "move"; else -> "up" }

    /**
     * Main thread only. One TextView on [root], above the Surface; touches are still forwarded
     * (`send()` already refuses between sessions). Goes away with the Activity on [endSession].
     */
    private fun showLinkOverlay(show: Boolean) {
        if (!::root.isInitialized || isFinishing) return
        if (!show) { linkOverlay?.let { root.removeView(it) }; linkOverlay = null; return }
        if (linkOverlay != null) return
        val tv = android.widget.TextView(this).apply {
            text = "Connection to iPhone interrupted — reconnecting…"
            textSize = 28f
            setTextColor(android.graphics.Color.WHITE)
            setBackgroundColor(0xB0000000.toInt())
            setPadding(48, 24, 48, 24)
            isClickable = false
            isFocusable = false
        }
        root.addView(tv, android.widget.FrameLayout.LayoutParams(
            android.widget.FrameLayout.LayoutParams.WRAP_CONTENT, android.widget.FrameLayout.LayoutParams.WRAP_CONTENT,
            android.view.Gravity.CENTER))
        linkOverlay = tv
    }

    override fun onResume() {
        super.onResume()
        zeno.gmccpa.AppForeground.resumed()
        // Plan v5.9 MED M5: the surviving screen brought back to the front (singleTop: onNewIntent is
        // followed by this) is a resume intent for the focus-reclaim latch, like a fresh start().
        // cp12 (rc1): the screen survives backgrounding and SessionCore.showCarPlayScreen brings it
        // forward without a new setupSession, so start() alone never re-armed it.
        player?.onScreenReturned()
        // A USB permission dialog requested while gm_ccpa was in the background may never have been
        // shown; coming to the front re-raises it (rate-limited by the probe — no dialog storm).
        zeno.gmccpa.SessionHolder.ocbmProbe?.rerequestNow()
    }

    override fun onPause() {
        zeno.gmccpa.AppForeground.paused()
        super.onPause()
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) applySystemUi()
    }

    override fun onDestroy() {
        // Only clear the shared handle if it still points at US. A newer generation may already have
        // published itself (launch of the replacement can precede this teardown), and clearing it
        // unconditionally would leave the LIVE screen unreachable from [onSessionEnded].
        if (live?.get() === this) live = null
        stopSession("activity destroyed")
        // The screen went, the session may not have (plan v5.9 MED M4 (b)): the AAC track is stopped,
        // so the card must not say PLAYING. Whether the session survives this is U59-9 (B-MED-3).
        if (zeno.gmccpa.SessionHolder.cpRx?.sessionLive == true) CarPlayMediaBrowserService.onScreenGone()
        touchThread?.quitSafely(); touchThread = null; touchHandler = null
        super.onDestroy()
    }

    /**
     * Stop the A/V session and take the screen down. See [onSessionEnded] for why this finishes.
     *
     * Idempotent: `stopSession` returns immediately once `generation` is null, and `finish()` on an
     * already-finishing Activity is a no-op.
     */
    private fun endSession(why: String) {
        log.i("CarPlay session ended ($why) — stopping A/V and closing the screen")
        stopSession(why)
        runOnUiThread { if (!isFinishing) finish() }
    }
}
