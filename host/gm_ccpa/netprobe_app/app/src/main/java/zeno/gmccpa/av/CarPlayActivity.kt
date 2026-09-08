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
         * it down means `MainActivity.launchCarPlayUi` starts a clean one, which is exactly what a
         * reconnect needs (a fresh generation, a fresh Surface, and a producer that re-dials with an
         * IDR).
         *
         * Safe to call from any thread and when no screen is up.
         */
        fun onSessionEnded(why: String) {
            live?.get()?.endSession(why)
        }

        /**
         * The merged now-playing picture. PROCESS-wide, not per-Activity: the MediaSession that
         * publishes it lives in [CarPlayMediaBrowserService], whose lifetime AAOS controls (it is
         * bound when Media Center asks, not when we start), so an Activity-instance field would be
         * unreachable from it. Session scope is restored by CLEARING it in [stopSession] instead.
         */
        val nowPlaying = NowPlayingState().apply {
            onMetadataChanged = { CarPlayMediaBrowserService.publish(it) }
            onPlaybackTick = { CarPlayMediaBrowserService.publishPlaybackState(it) }
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
        // Bind runs on the UI thread in onCreate; keep the worst case (4 ports x RETRIES x BACKOFF)
        // safely under the ~5 s ANR threshold. 4 x 6 x 150 ms = 3.6 s max, and normal binds are instant.
        private const val BIND_RETRIES = 6
        private const val BIND_BACKOFF_MS = 150L
        /** Desync guard on the seam length prefix; matches HevcRenderer and session.rs MAX_FRAME_BODY. */
        private const val MAX_MESSAGE = 8 * 1024 * 1024
        /** Internal phase for ACTION_CANCEL. The wire only knows 0/1/2 — see [onTouch]. */
        private const val PHASE_CANCEL = 3
        /** Displacement sent before a cancelled gesture's UP, as a fraction of the view width. ~2% is
         *  comfortably past iOS's tap allowable-movement and still an imperceptible drag. */
        private const val CANCEL_SLOP_N = 0.02f
    }

    private lateinit var surfaceView: SurfaceView
    private lateinit var root: android.widget.FrameLayout
    /** Index into [VIEW_AREAS] the surface is currently laid out for. */
    @Volatile private var viewAreaIndex = 0
    /** Cancels a pending shrink if another transition arrives first. */
    private var viewAreaGen = 0
    // @Volatile is load-bearing: written on the UI thread (attach/detachRenderer, stopSession) and
    // read on the cp-video thread. Without it the serve thread may legally observe a stale null after a
    // Surface returns — a permanent black screen plus the discard path below — or a stale stopped
    // renderer after a detach.
    @Volatile private var renderer: HevcRenderer? = null
    private var player: AacPlayer? = null
    @Volatile private var voiceRouter: VoiceRouter? = null
    @Volatile private var micUplink: MicUplink? = null

    private val servers = CopyOnWriteArrayList<ServerSocket>()
    private val liveSockets = CopyOnWriteArrayList<Socket>()

    /**
     * Per-generation liveness. A single shared flag cannot distinguish "my generation was stopped"
     * from "a new generation started", which lets a stale listener thread bind the port and feed a
     * released decoder while the new generation gets EADDRINUSE — listeners alive, zero video.
     */
    private var generation: AtomicBoolean? = null

    /** Touch must not run on the UI thread: a send can block on the event-channel lock for seconds. */
    private var touchThread: HandlerThread? = null
    private var touchHandler: Handler? = null
    // AtomicReference, not a plain @Volatile: the check-set-post sequence on a plain field has a
    // lost-wakeup race (reader nulls it between the UI thread's read and post) that freezes the rest
    // of a drag. getAndSet makes the drain atomic and self-coalescing.
    private val pendingMove = AtomicReference<Triple<Int, Float, Float>?>(null)
    private var primaryPointerId = -1

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        live = java.lang.ref.WeakReference(this)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        surfaceView = SurfaceView(this)
        // The surface is no longer the content view: it is a POSITIONED child of a panel-sized root,
        // so it can occupy a view area smaller than the panel and let GM's own chrome show around it.
        // The root stays full-bleed (2400x960) and is the coordinate space touch is normalised in —
        // see [onTouch]. Its background is what shows outside the active view area.
        root = android.widget.FrameLayout(this)
        root.addView(surfaceView, android.widget.FrameLayout.LayoutParams(DISPLAY_W, DISPLAY_H))
        setContentView(root)
        goImmersive()

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

    private fun goImmersive() = applySystemUi()

    private fun startSession() {
        if (generation != null) { log.i("session already started"); return }
        val gen = AtomicBoolean(true)
        generation = gen

        // Pin the process to foreground priority so backgrounding (GM dialog / reverse gear / app
        // switch) can't let LMK reclaim the live session. See CarPlaySessionService for the scope limit.
        CarPlaySessionService.start(this)

        // Prime, don't just construct: the AudioTrack is built and playing before the seam even
        // connects, so the first ADTS frame decodes straight into a live track.
        // Pass the AudioManager so media can HOLD AUDIOFOCUS_GAIN — without a resting focus owner the
        // head unit's volume knob stays stuck on Phone/Siri after a transient holder abandons focus.
        val p = AacPlayer(getSystemService(android.content.Context.AUDIO_SERVICE) as? android.media.AudioManager)
        p.start(); p.prime(); player = p
        // The non-media half. Ducking is routed straight at the media track: every other purpose
        // plays at unity, so min(commandedDuck, focusDuck) collapses to this one call.
        voiceRouter = VoiceRouter(
            this,
            onDuck = { ducked -> player?.setDucked(ducked) },
            // Pausing (not ducking) media is what lets the volume knob reach the voice group while
            // Siri speaks — MUSIC outranks VOICE_COMMAND, and a ducked track still counts as active.
            onAssistant = { speaking -> player?.setAssistantSpeaking(speaking) },
        ).also { it.start() }
        // Connect eagerly: the same socket carries the capture gate, so a data-triggered connect
        // would deadlock (no connection -> no gate -> no capture -> no data).
        micUplink = MicUplink().also { it.start() }

        // Bind BEFORE returning so a failure is visible and retryable, not swallowed on a thread.
        val vs = bindOrNull(9001) ?: run { failStart("video"); return }
        val aus = bindOrNull(9002) ?: run { vs.close(); failStart("audio"); return }
        servers.add(vs); servers.add(aus)

        // The video seam is served whether or not a Surface exists, and the connection is HELD either
        // way. With no renderer we drain and DISCARD.
        //
        // Closing instead produced a ~30-60 Hz accept/close storm: `forward_screen` re-dials on the very
        // next frame with NO backoff (session.rs:1790-1821) — the 2 s connect_timeout is a ceiling on
        // failure, not a delay — and fires a ForceKeyFrame on EVERY successful connect
        // (session.rs:1808-1811). That is one connect plus one encrypted event-channel command per frame,
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
        // (session.rs routes them there, tagged `[rate u32 BE][ch u16 BE][atype u8][len u32 BE][AU]`). We do not
        // decode it yet, but it MUST be bound: `forward_to_sink` logs its failed connect once per RTP
        // packet with no transition gate, so the first nav prompt of a drive produces ~50 log lines a
        // second, permanently, burying every other diagnostic in the ring buffer.
        //
        // Draining is deliberate — accepting without reading fills the socket buffer and stalls the
        // producer's write for its full 2 s timeout. Best-effort by design: voice is optional, so a
        // bind failure must NOT take video and media audio down with it.
        val vos = bindOrNull(9003)
        if (vos != null) {
            servers.add(vos)
            serve(gen, vos, "voice") { ins -> voiceRouter?.consume(ins) }
        } else {
            log.e("voice seam :9003 could not be bound — Siri/telephony/nav audio is discarded AND the "
                + "producer will log a failed connect per packet; continuing without it")
        }
        // :9004 carries now-playing metadata and album art as `[u32 BE "META"][u32 BE len][marker]
        // [payload]`. Bound here rather than in CarPlaySessionService because the service is
        // priority-only by its own KDoc — session ownership still lives in this Activity (T2.2 open),
        // and a lone seam with a different owner and a different generation guard is exactly the
        // stale-listener / EADDRINUSE shape the per-generation AtomicBoolean exists to prevent.
        //
        // Best-effort like :9003, but for the opposite reason. An UNBOUND port here is cheap: the
        // producer connects lazily per record and warns once (metadata.rs:44-46), unlike the voice
        // seam's per-packet log storm. An accepted-but-unread one is the expensive case — the producer
        // writes under a SINK mutex it SHARES with the iAP2 reader, and since the core is in-process
        // that reader is our own thread, so a stalled consumer here stalls iAP2 ingest for the whole
        // session. Bind, and always drain.
        val mts = bindOrNull(9004)
        if (mts != null) {
            servers.add(mts)
            val seam = MetadataSeam(ProbeLog.sub("meta")) { m, pl ->
                // /command plists are a different plane from now-playing; keep NowPlayingState purely
                // about media rather than teaching it about view areas.
                if (m == MetadataSeam.META_CMD) onCommandPlist(pl) else nowPlaying.dispatch(m, pl)
            }
            serve(gen, mts, "meta") { ins -> seam.consume(gen, ins) }
        } else {
            log.e("metadata seam :9004 could not be bound — the now-playing card stays empty; continuing")
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
        zeno.gmccpa.logging.SessionSummary.current()?.avCountersSource =
            { longArrayOf(r.framesRendered.get(), r.ausDropped.get(), r.bytesIn.get()) }
        log.i("renderer attached to Surface")
        // Any seam connection already open belongs to the previous renderer; drop it so the producer
        // re-dials into this one and sends a fresh IDR.
        liveSockets.filter { it.localPort == 9001 }.forEach { runCatching { it.close() } }
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
        r.stop()
        // Unblock the consume thread parked in read(); it releases the codec on its own thread.
        liveSockets.filter { it.localPort == 9001 }.forEach { runCatching { it.close() } }
        log.i("renderer detached — audio and seams stay up")
    }

    /**
     * No Surface: keep the seam connected and throw the frames away.
     *
     * Draining is not optional. Accepting but not reading fills the socket buffer, stalls the producer's
     * `write_all` for the full 2 s SO_SNDTIMEO (session.rs:1799), then drops the connection anyway —
     * back-pressuring the screen thread and, through it, the iPhone's screen socket. Reading and
     * discarding costs one read plus a memcpy into a reused buffer.
     *
     * Framing is the seam's own `[u32 BE len][payload]`, read exactly as [HevcRenderer.consume] reads
     * it, so a desync cannot make this allocate — the payload is skipped, never allocated. A 0-length
     * message is legal (session.rs:2074) and is not a desync.
     *
     * Returns as soon as a renderer appears; [attachRenderer] then closes the socket so the producer
     * re-dials into the new renderer with ONE fresh ForceKeyFrame — the designed heal path. If no frames
     * are arriving we are parked in read(), and that same close is what unblocks us.
     */
    private fun discardUntilRenderer(gen: AtomicBoolean, ins: java.io.InputStream) {
        log.i("video seam connected with no Surface — holding open, discarding until one returns")
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

    /**
     * Consume and discard the voice seam.
     *
     * A pure byte drain, deliberately NOT a framing parse: the point is to keep the producer's socket
     * healthy and its per-packet connect-failure log silent, and a byte drain cannot desync. Real
     * consumption (AAC-ELD decode into a USAGE_ASSISTANT / USAGE_VOICE_COMMUNICATION track) is the
     * follow-up; until then Siri and call audio are silently dropped, which is a known gap rather than
     * a surprise.
     */

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

    private fun failStart(which: String) {
        log.e("$which seam could not be bound after $BIND_RETRIES attempts — no A/V; finishing")
        stopSession()
        runOnUiThread { finish() }
    }

    /**
     * Bind loopback with SO_REUSEADDR and a bounded retry. We perform the active close on the accepted
     * sockets, so the listener tuple can sit in TIME_WAIT across a quick stop/start; and AvSink or a
     * stale generation may still hold the port for a moment.
     */
    private fun bindOrNull(port: Int): ServerSocket? {
        repeat(BIND_RETRIES) { attempt ->
            try {
                return ServerSocket().apply {
                    reuseAddress = true
                    bind(InetSocketAddress(InetAddress.getByName("127.0.0.1"), port), 4)
                }
            } catch (e: Exception) {
                log.e("bind :$port attempt ${attempt + 1}/$BIND_RETRIES failed: ${e.message}")
                try { Thread.sleep(BIND_BACKOFF_MS) } catch (_: InterruptedException) { return null }
            }
        }
        return null
    }

    private fun serve(gen: AtomicBoolean, srv: ServerSocket, label: String, body: (java.io.InputStream) -> Unit) {
        Thread({
            try {
                while (gen.get()) {
                    val s = try { srv.accept() } catch (e: Exception) {
                        if (gen.get()) log.e("$label accept: ${e.message}"); break
                    }
                    liveSockets.add(s)
                    log.i("$label seam connected")
                    // Catch Throwable, not Exception: an 8 MB per-message allocation can throw
                    // OutOfMemoryError (an Error), which would otherwise escape to the outer finally
                    // and permanently close this listener — a black screen with a live session. Only
                    // this one connection should die; the listener must keep accepting.
                    try { body(s.getInputStream()) } catch (e: Throwable) { log.e("$label: ${e.javaClass.simpleName}: ${e.message}") }
                    finally { liveSockets.remove(s); runCatching { s.close() } }
                }
            } finally {
                runCatching { srv.close() }   // never leave a bound socket behind
            }
        }, "cp-$label").apply { isDaemon = true }.start()
    }

    private fun stopSession() {
        val gen = generation ?: return
        generation = null
        gen.set(false)
        renderer?.stop(); renderer = null
        player?.stop()
        // Flag-only, like the player: VoiceRouter releases its codecs/tracks on the consume thread.
        // Skipping this strands up to four AudioTracks AND their focus requests, and an unabandoned
        // USAGE_VOICE_COMMUNICATION request keeps the hardware volume keys pinned to the call group
        // for the rest of the session.
        voiceRouter?.stop()
        micUplink?.stop()
        // Closing the accepted socket is the only reliable way to unblock a consumer parked in read().
        liveSockets.forEach { runCatching { it.close() } }; liveSockets.clear()
        servers.forEach { runCatching { it.close() } }; servers.clear()
        player = null
        voiceRouter = null
        micUplink = null
        CarPlaySessionService.stop(this)
        // Clear AND publish the cleared picture: a card still showing the last track over a dead
        // session is the metadata twin of the frozen-frame bug onSessionEnded exists to prevent.
        nowPlaying.clear()
        log.i("session stopped")
    }

    /** Ask iOS for a fresh IDR. Off the UI thread — it rides the same blocking event channel. */
    private fun requestKeyframe() {
        touchHandler?.post { NativeCore.forceKeyFrame() }
    }

    /**
     * Touch → HID report to the iPhone, dispatched off the UI thread.
     *
     * A send takes the event-channel lock and can block on a stalled socket write for seconds; doing
     * that inline from `onTouch` risks an ANR. MOVEs are coalesced latest-wins (a stale MOVE has no
     * value once a newer one exists) while DOWN and UP are never dropped and keep their order.
     *
     * Single-touch: only the primary pointer is tracked. Without that, a second finger lifting sends
     * UP while the first is still down, and iOS sees the gesture end mid-drag.
     */
    /**
     * An inbound `/command` plist off the `:9004` seam.
     *
     * Only `requestViewArea` is acted on. Everything else (`modesChanged`, `setNightMode`,
     * `duckAudio`, …) is ignored rather than half-handled — the receiver already acts on the ones
     * that matter to the session, and a partially-implemented command plane is worse than none.
     *
     * Runs on the `cp-meta` seam thread; the layout change is posted to the UI thread.
     */
    private fun onCommandPlist(payload: ByteArray) {
        val root = BPlist.parse(payload) ?: return
        if (BPlist.str(root, "type") != "requestViewArea") return
        val idx = BPlist.int(root, "params", "viewAreaIndex")?.toInt() ?: return
        if (idx !in VIEW_AREAS.indices) {
            log.w("requestViewArea index=$idx outside the ${VIEW_AREAS.size} declared areas — ignoring")
            return
        }
        log.i("requestViewArea index=$idx -> laying the surface out at ${VIEW_AREAS[idx]}")
        runOnUiThread { applyViewArea(idx) }
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

    private fun onTouch(v: View, ev: MotionEvent): Boolean {
        if (v.width <= 0 || v.height <= 0) return false
        val action = ev.actionMasked
        val pid = ev.getPointerId(ev.actionIndex)

        val phase = when (action) {
            MotionEvent.ACTION_DOWN -> { primaryPointerId = pid; 0 }
            MotionEvent.ACTION_MOVE -> 1
            MotionEvent.ACTION_UP -> { primaryPointerId = -1; 2 }
            MotionEvent.ACTION_CANCEL -> { primaryPointerId = -1; PHASE_CANCEL }
            MotionEvent.ACTION_POINTER_DOWN -> return true               // a non-primary finger arrived
            // If the PRIMARY finger lifts first, send UP (don't let MOVEs teleport to the survivor);
            // a non-primary finger lifting is ignored.
            MotionEvent.ACTION_POINTER_UP -> if (pid == primaryPointerId) { primaryPointerId = -1; 2 } else return true
            else -> return false
        }
        // For a POINTER_UP the lifting pointer's own position is at actionIndex; otherwise track the
        // primary pointer (falling back to index 0 once it has been cleared).
        val idx = if (action == MotionEvent.ACTION_POINTER_UP) ev.actionIndex
                  else if (primaryPointerId >= 0) ev.findPointerIndex(primaryPointerId).takeIf { it >= 0 } ?: 0
                  else 0
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
        } else if (phase == PHASE_CANCEL) {
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
        // Log every DOWN/UP and every failure — a dying event channel is otherwise invisible mid-drag.
        if (phase != 1 || !sent) {
            log.i("touch ${phaseName(phase)} n=(%.3f, %.3f) sent=$sent".format(nx, ny))
        }
    }

    private fun phaseName(p: Int) = when (p) { 0 -> "down"; 1 -> "move"; else -> "up" }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) goImmersive()
    }

    override fun onDestroy() {
        // Only clear the shared handle if it still points at US. A newer generation may already have
        // published itself (launch of the replacement can precede this teardown), and clearing it
        // unconditionally would leave the LIVE screen unreachable from [onSessionEnded].
        if (live?.get() === this) live = null
        stopSession()
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
        stopSession()
        runOnUiThread { if (!isFinishing) finish() }
    }
}
