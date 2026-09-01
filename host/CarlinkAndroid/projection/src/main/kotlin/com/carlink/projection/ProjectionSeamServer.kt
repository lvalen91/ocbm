package com.carlink.projection

import android.content.Context
import android.media.AudioManager
import android.view.Surface
import com.carlink.av.AacPlayer
import com.carlink.av.HevcRenderer
import com.carlink.av.VoiceRouter
import com.carlink.logging.ProbeLog
import com.carlink.ocbm.Ocbm
import com.carlink.ocbm.seam.AudioSeam
import com.carlink.ocbm.seam.MetadataSeam
import com.carlink.ocbm.seam.SeamPipe
import com.carlink.ocbm.seam.VideoSeam
import org.json.JSONObject
import java.util.concurrent.atomic.AtomicBoolean

/**
 * The whole A/V data plane: three loopback listeners, the two forward-encrypted seam parsers, and
 * the three decoders.
 *
 * ```
 *   airplayd ──connect──► :9001 ─► VideoSeam ─► SeamPipe ─► HevcRenderer ─► Surface
 *            ──connect──► :9002 ─┐
 *            ──connect──► :9003 ─┴► AudioSeam ─┬► SeamPipe ─► AacPlayer   (media)
 *                                              └► SeamPipe ─► VoiceRouter (telephony/Siri/alerts)
 * ```
 *
 * Everything below the `SeamPipe`s is code already proven against real iPhone traffic on the CCPA;
 * this class supplies the transport those parsers were previously fed by OCBM-over-USB. That is the
 * entire Pi-specific difference, and it is why the seam sources are shared rather than reimplemented.
 *
 * ## Session scope vs surface scope — the distinction that matters
 *
 * The ChaCha20 key and the frame sequence counter belong to the **session** and live in [VideoSeam].
 * The decoder belongs to the **Surface** and dies with it. Rebuilding the seam when the user leaves
 * and returns to projection would discard the key, and `airplayd` only re-sends the key when *its*
 * seam socket reconnects — which a surface change on our side does not cause. The result would be
 * permanent decrypt failure after the first time the driver looked at another app.
 *
 * So the seams are created once in the constructor and live as long as this object; [attachSurface]
 * and [detachSurface] only swap the pipe underneath. `VideoSeam.attach` exists for exactly this.
 *
 * ## Audio does not have that problem, and is started eagerly
 *
 * Audio has no surface, so its pipes and players are created at [start] and torn down at [stop].
 * That is also what makes audio-only operation work while the driver is in another app — which is
 * the normal case for music, and the reason the session must not be tied to the activity.
 */
class ProjectionSeamServer(
    private val ctx: Context,
    private val status: SessionStatus,
    private val input: InputSender,
) {
    private val log = ProbeLog.sub("avsrv")
    private val started = AtomicBoolean(false)

    // ---- session-scoped seam state ---------------------------------------------------------------

    private val videoSeam =
        VideoSeam(initialPipe = null, log = ProbeLog.sub("video")) {
            // A gap or a decrypt failure. Ask iOS for a fresh IDR through the input seam; the box
            // owns the encrypted event channel, so this is the only way to reach it. Throttled
            // inside InputSender because VideoSeam calls this per bad frame and a wrong key would
            // otherwise mean 60 requests a second.
            input.requestKeyframe()
        }

    /**
     * Now-playing state, merged across deltas. Public so the service can publish it to AAOS.
     *
     * Lives here rather than in the service because it is seam-scoped: it must be cleared when the
     * session ends, and the seam is what knows that.
     */
    val nowPlaying = NowPlayingState()

    /** Set by the service; invoked when the published picture actually changed. */
    @Volatile
    var onNowPlayingChanged: ((NowPlayingState.Snapshot) -> Unit)? = null

    /**
     * Fires on every nowPlaying record the merge decided was NOT worth a full metadata publish —
     * the ~2 Hz elapsed-only deltas.
     *
     * Separate from [onNowPlayingChanged] because the costs differ by orders of magnitude:
     * rebuilding a `MediaMetadata` at 2 Hz churns every AAOS consumer, while a `PlaybackState`
     * carries position and rate and is what a card interpolates from. `CarPlayMediaSession`
     * documented that split from the start; nothing was calling the cheap half, so the playback
     * position only ever refreshed when the track changed.
     */
    @Volatile
    var onPlaybackTick: ((NowPlayingState.Snapshot) -> Unit)? = null

    private val metadataSeam =
        MetadataSeam(ProbeLog.sub("meta")) { marker, payload ->
            // Fires on the seam read thread. Parse inline, hand off — anything slow here stalls the
            // metadata lane, and a JSON parse of a one-line record is cheap.
            handleMetadata(marker, payload)
        }

    private val audioLog = ProbeLog.sub("audio")

    /**
     * Rebuilt by [startAudioPlayers] on every run — NOT constructor-scoped, and that is the whole
     * difference between a restartable server and one that swallows audio in silence.
     *
     * `SeamPipe.close()` is TERMINAL: its `closed` flag never resets. A reused pipe accepts nothing,
     * both consumer threads return on their first read, and every byte lands in `SeamPipe.dropped` —
     * a counter [diagnostics] did not print. The `started` CAS pair advertises restartability, so
     * the state it guards has to actually be re-creatable.
     *
     * [audioSeam] is rebuilt with them, and that is correct rather than collateral: the audio stream
     * keys live in it and are per-CONNECTION, and [stop] drops the listeners so `airplayd`
     * reconnects and re-sends them. The VIDEO seam is deliberately NOT rebuilt — its key must
     * survive a surface change, which is a different lifetime entirely.
     */
    @Volatile private var mediaPipe = SeamPipe(capacityBytes = AUDIO_PIPE_BYTES)

    @Volatile private var voicePipe = SeamPipe(capacityBytes = AUDIO_PIPE_BYTES)

    @Volatile private var audioSeam = AudioSeam(mediaPipe, voicePipe, audioLog)

    // ---- surface-scoped decoder ------------------------------------------------------------------

    @Volatile private var renderer: HevcRenderer? = null

    @Volatile private var videoPipe: SeamPipe? = null

    @Volatile private var videoThread: Thread? = null

    /** The Surface the current decoder is bound to, for the idempotence check in [attachSurface]. */
    @Volatile private var attached: Surface? = null

    // ---- audio players ---------------------------------------------------------------------------

    @Volatile private var aac: AacPlayer? = null

    @Volatile private var voice: VoiceRouter? = null

    private val threads = mutableListOf<Thread>()

    // ---- listeners -------------------------------------------------------------------------------

    private val videoListener =
        SeamListener(
            port = SeamContract.PORT_VIDEO,
            name = "video",
            onConnect = { status.setVideoSeamUp(true) },
            onDisconnect = {
                status.setVideoSeamUp(false)
                // The per-stream key belongs to THIS connection and airplayd re-sends on reconnect.
                // Without clearing it the phase stays KEYED after the producer dies, and the HMI
                // keeps offering "return to CarPlay" for a session that no longer exists.
                status.setKeyed(false)
            },
        ) { b, n -> videoSeam.feed(b.copyOf(n)) }

    private val mediaListener =
        SeamListener(
            port = SeamContract.PORT_MEDIA_AUDIO,
            name = "media",
            onConnect = { status.setAudioSeamUp(true) },
            onDisconnect = { status.setAudioSeamUp(false) },
        ) { b, n -> audioSeam.feedMedia(b.copyOf(n)) }

    private val voiceListener =
        SeamListener(
            port = SeamContract.PORT_VOICE_AUDIO,
            name = "voice",
        ) { b, n -> audioSeam.feedVoice(b.copyOf(n)) }

    private val metadataListener =
        SeamListener(
            port = SeamContract.PORT_METADATA,
            name = "meta",
        ) { b, n -> metadataSeam.feed(b.copyOf(n)) }

    private fun handleMetadata(
        marker: Int,
        payload: ByteArray,
    ) {
        when (marker) {
            Ocbm.META_JSON -> {
                val json = runCatching { JSONObject(String(payload, Charsets.UTF_8)) }.getOrNull() ?: return
                // The seam carries far more than now-playing (route guidance, call state, power, app
                // discovery...). Only the media picture is consumed today; the rest is deliberately
                // ignored rather than half-handled.
                if (json.optString("kind") != "nowPlaying") return
                val changed = nowPlaying.onNowPlaying(json)
                if (changed != null) {
                    emitNowPlaying(changed)
                } else {
                    // Metadata unchanged, position moved: the cheap publish.
                    runCatching { onPlaybackTick?.invoke(nowPlaying.snapshot) }
                        .onFailure { log.e("playback tick consumer threw: $it") }
                }
            }
            Ocbm.META_ARTWORK -> {
                // `[id u8][JPEG bytes]`.
                if (payload.size < 2) return
                val id = payload[0].toInt() and 0xFF
                nowPlaying.onArtwork(id, payload.copyOfRange(1, payload.size))?.let { emitNowPlaying(it) }
            }
            // META_CMD (inbound /command plists) and META_CORNERMASK are not consumed here.
            else -> Unit
        }
    }

    private fun emitNowPlaying(s: NowPlayingState.Snapshot) {
        runCatching { onNowPlayingChanged?.invoke(s) }
            .onFailure { log.e("now-playing consumer threw: $it") }
    }

    init {
        // The first per-stream key is the earliest proof on the wire that a paired, keyed session
        // exists — earlier than the first decoded frame, and far earlier than anything the UI could
        // otherwise observe. The session layer raises "connected" on it.
        videoSeam.onKey = { status.setKeyed(true) }

        // Name a codec mismatch loudly. HevcRenderer configures video/hevc unconditionally, so an
        // H.264 stream decrypts perfectly and renders nothing — a black screen with healthy
        // counters everywhere, which is the hardest kind of failure to chase.
        //
        // In practice this means one thing: the box was launched WITHOUT the app-pushed config, so
        // it advertised its compiled default instead of `accessoryConfig.enablesHEVC: true`.
        videoSeam.onCodec = { isHevc ->
            hevcNegotiated = isHevc
            if (!isHevc) {
                log.e(
                    "stream is H.264 but the decoder is HEVC — NOTHING WILL RENDER. " +
                        "The box did not receive the pushed config: relaunch carplay-wireless with " +
                        "CARPLAY_CFG_FILE set (see pi/tools/start_stack.sh).",
                )
            }
            status.setHevcNegotiated(isHevc)
        }
    }

    /** Whether the live stream is HEVC. Null until the first codec config arrives. */
    @Volatile
    var hevcNegotiated: Boolean? = null
        private set

    fun start() {
        if (!started.compareAndSet(false, true)) return
        log.i("starting A/V data plane")
        startAudioPlayers()
        videoListener.start()
        mediaListener.start()
        voiceListener.start()
        metadataListener.start()
    }

    fun stop() {
        if (!started.compareAndSet(true, false)) return
        log.i("stopping A/V data plane")
        videoListener.stop()
        mediaListener.stop()
        voiceListener.stop()
        metadataListener.stop()
        detachSurface()
        stopAudioPlayers()
        videoListener.join(SHUTDOWN_JOIN_MS)
        mediaListener.join(SHUTDOWN_JOIN_MS)
        voiceListener.join(SHUTDOWN_JOIN_MS)
        metadataListener.join(SHUTDOWN_JOIN_MS)
        // A stale now-playing card must not outlive the phone.
        nowPlaying.clear()
        emitNowPlaying(nowPlaying.snapshot)
        // ONE atomic transition, not three. The open-coded version emitted three states — three
        // notification rebuilds and three CarProjectionBridge publishes — and left `hevcNegotiated`
        // and the device identity behind, so the notification could still read "CarPlay — iPhone"
        // after the phone was gone. sessionEnded() deliberately keeps `foreground`, which is a fact
        // about our own activity and is unaffected by the session ending.
        status.sessionEnded()
    }

    // ---- surface lifecycle -----------------------------------------------------------------------

    /**
     * Point the video seam at a decoder rendering to [surface].
     *
     * [width] and [height] are the *decoder's* configured dimensions. They come from the display
     * (see [VehicleConfigGenerator]) rather than from the stream, because MediaCodec needs them at
     * configure time and the SPS has not necessarily arrived yet. A mismatch is not fatal — the
     * decoder reconfigures from the SPS — but starting close avoids a reconfigure on the first IDR.
     */
    @Synchronized
    fun attachSurface(
        surface: Surface,
        width: Int,
        height: Int,
    ) {
        // Idempotent. SurfaceHolder delivers surfaceCreated AND surfaceChanged for one surface
        // coming up, and both legitimately want to (re)attach — but acting on both tears down a
        // decoder we built microseconds earlier, throws away its parameter sets, and costs another
        // ForceKeyFrame round trip before anything can render.
        if (attached === surface && renderer != null) {
            log.i("surface already attached — ignoring duplicate attach")
            return
        }
        detachSurface()
        val pipe = SeamPipe(capacityBytes = VIDEO_PIPE_BYTES)
        val r =
            HevcRenderer(width, height, surface) {
                input.requestKeyframe()
            }
        videoPipe = pipe
        renderer = r
        attached = surface
        videoSeam.attach(pipe)
        // MUST precede consume(): consume() loops on the same `running` flag that start() sets, so
        // a consumer started without it returns immediately and silently — a black screen with a
        // healthy-looking seam and climbing decrypt counters.
        r.start()
        val t =
            Thread({
                try {
                    // Blocks for the life of the surface; owns the codec on this thread, which is
                    // MediaCodec's requirement and why stop() closes the pipe rather than touching
                    // the codec.
                    r.consume(pipe)
                } catch (e: Exception) {
                    log.e("video consumer died: $e")
                } finally {
                    status.setRendering(false)
                }
            }, "hevc-consume").apply {
                isDaemon = true
                start()
            }
        videoThread = t
        status.setRendering(true)
        log.i("surface attached — decoder ${width}x$height")

        // Nothing on the wire prompts iOS to send an IDR just because we grew a surface, and the
        // stream is mid-GOP: without this the first thing the fresh decoder sees is a P-frame
        // referencing frames it never had, and it stays black until the phone's next scheduled IDR
        // (which can be tens of seconds).
        input.requestKeyframe()
    }

    /** Release the decoder and leave the seam keyed and running. Safe to call when not attached. */
    @Synchronized
    fun detachSurface() {
        videoSeam.attach(null)
        // Flag-only by contract — it does NOT touch the codec. The pipe close below is what actually
        // unblocks the consumer; this just stops it re-entering the loop.
        renderer?.stop()
        // Closing the pipe is the complete and correct way to stop the consumer: it surfaces EOF,
        // the renderer returns from consume() on its own thread and releases the codec there. Never
        // release the codec from here — a lifecycle call racing a dequeue crashes natively.
        videoPipe?.close()
        val t = videoThread
        if (t != null) {
            // JOIN BEFORE CLEARING THE REFERENCES.
            //
            // ProjectionActivity calls this from surfaceDestroyed, where returning while the decoder
            // still holds the Surface is a NATIVE ABORT, not an exception. The previous order nulled
            // `renderer`/`videoPipe` first and joined afterwards with a 2 s cap, so a slow consumer
            // left a MediaCodec bound to a destroyed Surface with no reference anywhere — orphaned
            // and never released.
            //
            // MediaCodec's contract also forbids releasing it from here: HevcRenderer.releaseCodec
            // is documented "owned by the consume thread". So waiting is the only correct action.
            // Wait in bounded steps and keep saying so rather than silently proceeding.
            var waited = 0L
            while (t.isAlive && waited < DETACH_MAX_WAIT_MS) {
                runCatching { t.join(SHUTDOWN_JOIN_MS) }
                waited += SHUTDOWN_JOIN_MS
                if (t.isAlive) {
                    log.w("video consumer still holding the surface after ${waited}ms — waiting")
                }
            }
            if (t.isAlive) {
                // Out of options that are safe. Say it as loudly as possible: continuing risks a
                // native abort in SurfaceFlinger, and the codec is now unreachable.
                log.e(
                    "video consumer did NOT release the surface after ${waited}ms — the MediaCodec " +
                        "is orphaned and a native abort is possible. This should never happen.",
                )
            }
        }
        videoPipe = null
        renderer = null
        videoThread = null
        attached = null
        if (t != null) log.i("surface detached")
        status.setRendering(false)
    }

    // ---- audio -----------------------------------------------------------------------------------

    private fun startAudioPlayers() {
        // Fresh pipes and parser per run. See the field docs: a closed SeamPipe never reopens, so
        // carrying them across a stop/start turns every audio byte into a silent drop.
        mediaPipe = SeamPipe(capacityBytes = AUDIO_PIPE_BYTES)
        voicePipe = SeamPipe(capacityBytes = AUDIO_PIPE_BYTES)
        audioSeam = AudioSeam(mediaPipe, voicePipe, audioLog)
        val am = ctx.getSystemService(Context.AUDIO_SERVICE) as? AudioManager
        val player = AacPlayer(am)
        val router =
            VoiceRouter(
                ctx = ctx,
                onDuck = { ducked -> player.setDucked(ducked) },
                onAssistant = { active -> status.setAssistantActive(active) },
            )
        aac = player
        voice = router
        // Same trap as the video path: consume() runs `while (running.get())`, and only start()
        // sets it. AacPlayer.start() also takes the resting AUDIOFOCUS_GAIN, which is what gives
        // AAOS an owner to hand focus back to after Siri or a call — without it the volume knob
        // stays pointed at the assistant's group once a turn ends.
        player.start()
        router.start()
        // Captured as locals: a consumer owns the pipe it was STARTED with, never whatever the
        // field happens to point at by the time the thread is scheduled.
        val mp = mediaPipe
        val vp = voicePipe
        threads += consumerThread("aac-consume") { player.consume(mp) }
        threads += consumerThread("voice-consume") { router.consume(vp) }
    }

    private fun stopAudioPlayers() {
        // Same contract as video: close the pipe, let the consumer unwind on its own thread.
        mediaPipe.close()
        voicePipe.close()
        aac?.stop()
        voice?.stop()
        threads.forEach { t ->
            runCatching { t.join(SHUTDOWN_JOIN_MS) }
        }
        threads.clear()
        aac = null
        voice = null
    }

    private fun consumerThread(
        name: String,
        body: () -> Unit,
    ): Thread =
        Thread({
            try {
                body()
            } catch (e: Exception) {
                log.e("$name died: $e")
            }
        }, name).apply {
            isDaemon = true
            start()
        }

    // ---- diagnostics -----------------------------------------------------------------------------

    /**
     * A one-line health summary. Deliberately reports the counters that distinguish the failure
     * modes from each other rather than a single "ok": `unkeyed` means the key never arrived
     * (plumbing), `fail` means it arrived and is wrong (crypto/framing), `noPipe` means frames are
     * being produced with no surface to draw on (normal while backgrounded, a bug while foreground).
     */
    fun diagnostics(): String =
        buildString {
            append("video seam=${videoListener.connected} ")
            append("keys=${videoSeam.keysReceived.get()} ")
            append("ok=${videoSeam.decryptOk.get()} fail=${videoSeam.decryptFail.get()} ")
            append("unkeyed=${videoSeam.unkeyed.get()} gaps=${videoSeam.gaps.get()} ")
            append("noPipe=${videoSeam.framesDroppedNoPipe.get()} ")
            append("out=${videoSeam.framesOut.get()} | ")
            append("audio seam=${mediaListener.connected} ")
            append("ok=${audioSeam.decryptOk.get()} fail=${audioSeam.decryptFail.get()} ")
            append("unkeyed=${audioSeam.unkeyed.get()} ")
            // Nonzero while a session is LIVE means a lane is writing into a closed pipe — the
            // restart failure this counter exists to make visible. Nonzero after stop() is normal.
            append("pipeDrop=${mediaPipe.dropped.get()}/${voicePipe.dropped.get()} | ")
            append("rendered=${renderer?.framesRendered?.get() ?: 0} ")
            append("codec=${hevcNegotiated?.let { if (it) "hevc" else "h264" } ?: "?"} ")
            append("meta=${metadataSeam.messagesOut.get()}/${metadataSeam.resyncBytes.get()}resync")
        }

    private companion object {
        /**
         * Must hold at least one whole keyframe, and a 1920x1080 HEVC IDR runs to a few hundred KB.
         * 8 MB gives headroom for a burst without letting a stalled decoder grow unboundedly — the
         * cap is backpressure, and backpressure is the design (`docs/carplay/06_AV_PIPELINE.md`).
         */
        const val VIDEO_PIPE_BYTES = 8 * 1024 * 1024

        /** Audio access units are ~1 KB; this is roughly a second of slack. */
        const val AUDIO_PIPE_BYTES = 512 * 1024

        const val SHUTDOWN_JOIN_MS = 2000L

        /**
         * Total time detachSurface will wait for the decoder to let go of the Surface. Generous on
         * purpose: the alternative to waiting is a native abort, so a stall is strictly better than
         * proceeding.
         */
        const val DETACH_MAX_WAIT_MS = 10_000L
    }
}
