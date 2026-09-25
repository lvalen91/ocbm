package com.carlink.av

import android.media.MediaCodec
import android.media.MediaFormat
import android.os.SystemClock
import android.view.Surface
import com.carlink.logging.ProbeLog
import java.io.InputStream
import java.nio.ByteBuffer
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * H.264 / HEVC decoder for the CarPlay screen stream, rendering straight to a [Surface].
 *
 * Successor to the HEVC-only `HevcRenderer` (itself structured after `carlink_native`'s
 * `H264Renderer`: MediaCodec SYNC mode, dedicated decode thread, keyframe-request callback, drop
 * accounting). The codec is NOT chosen by this app — iOS picks H.264 or HEVC from what the pushed
 * config advertises (`enablesHEVC`, fed by [VideoCodecs.probe]) and the box forwards the elementary
 * stream byte-for-byte — so the renderer latches it from the first parameter set ([VideoCodecs.sniff])
 * and configures accordingly:
 *
 *  - **HEVC**: MIME `video/hevc`, `csd-0` = VPS+SPS+PPS concatenated as ONE buffer. Splitting them
 *    the way H.264 does fails to configure.
 *  - **H.264**: MIME `video/avc`, `csd-0` = SPS, `csd-1` = PPS.
 *
 * Parameter sets arrive as the first Annex-B payload on the seam (the receiver converts the
 * `hvcC`/`avcC` record), so we buffer until the set the codec needs is complete.
 *
 * **Access units, not NALs.** MediaCodec expects one AU per input buffer, and the seam already gives us
 * exactly that: it is message-framed `[u32 BE len][payload]` with one message per screen message.
 *
 * **Threading contract.** [consume] owns the codec for its whole life: it configures, feeds, drains and
 * releases on its own thread. [stop] only flips the flag — it never touches the codec, because
 * MediaCodec lifecycle calls racing a dequeue crash natively.
 */
class VideoRenderer(
    private val width: Int,
    private val height: Int,
    private val surface: Surface,
    private val onKeyframeNeeded: () -> Unit = {},
) {
    private val log = ProbeLog.sub("video")
    private val running = AtomicBoolean(false)

    @Volatile private var codec: MediaCodec? = null

    @Volatile private var configured = false

    @Volatile private var configureFailedAt = 0L

    /** Latched from the first parameter set of the stream; reset by [start]. */
    @Volatile var videoCodec: VideoCodec? = null
        private set

    val framesRendered = AtomicLong(0)
    val bytesIn = AtomicLong(0)
    val ausDropped = AtomicLong(0)

    private val paramSets = VideoCodecs.ParamSets()
    private var sawKeyframe = false
    private var lastKeyframeReq = 0L

    /** The parameter sets actually baked into the live codec, to detect a mid-session change. */
    private var configuredCsd: ByteArray? = null

    private companion object {
        const val CONFIGURE_RETRY_MS = 5_000L // don't re-attempt configure per NAL (codec-pool leak)
        const val KEYFRAME_COOLDOWN_MS = 500L // H264Renderer's REACTIVE_KEYFRAME_COOLDOWN

        // Match the seam's max message: a seam-legal AU in (old 2 MB, 8 MB] — most dangerously the IDR
        // itself — was silently dropped, and dropping the replacement keyframe stalls video entirely.
        const val MAX_INPUT_SIZE = MAX_SEAM_MESSAGE
        const val INPUT_DEQUEUE_US = 100_000L
        const val LOG_EVERY_FRAMES = 300L
    }

    fun start() {
        // A restart must not carry parameter sets, codec or keyframe state across streams: mixing a
        // new VPS with a stale SPS/PPS configures a codec that cannot decode what follows.
        paramSets.clear()
        videoCodec = null
        sawKeyframe = false
        configured = false
        configureFailedAt = 0L
        running.set(true)
    }

    /** Flag-only. The codec is released by [consume] on its own thread; the caller closes the pipe. */
    fun stop() {
        running.set(false)
        log.i("stopping — ${framesRendered.get()} frames, ${bytesIn.get()} bytes, ${ausDropped.get()} AUs dropped")
    }

    /**
     * Consume the seam until it closes. Blocking; call on its own thread.
     *
     * The seam is **message-framed**: `[u32 BE len][payload]`, one message per screen message. Each
     * payload is either the converted parameter sets or exactly one Annex-B access unit.
     */
    fun consume(ins: InputStream) {
        // Every producer re-dial is a NEW decode session on the SAME instance. The reconnect issues a
        // ForceKeyFrame, but the frames already in flight are P-frames from the old GOP and this codec
        // has no reference frames — so the IRAP gate must be re-armed. Parameter sets are KEPT: a
        // mid-stream re-dial re-sends no VideoConfig, so the cache is the only way to configure.
        sawKeyframe = false
        val hdr = ByteArray(4)
        try {
            while (running.get()) {
                val msg = readSeamMessage(ins, hdr, log) ?: break
                bytesIn.addAndGet((msg.size + 4).toLong())
                // A 0-length message is NOT a desync: forward_screen is called unconditionally with
                // whatever the conversion produced, and an empty VideoConfig is exactly what this head
                // unit emitted before the sample-description fix.
                if (msg.isNotEmpty()) handleMessage(msg)
            }
        } finally {
            releaseCodec()
            log.i("seam ended — ${framesRendered.get()} frames rendered, ${ausDropped.get()} AUs dropped")
        }
    }

    /** One message = the parameter sets, or one complete access unit. */
    private fun handleMessage(msg: ByteArray) {
        val scan = VideoCodecs.scan(videoCodec, msg)
        val c = scan.codec
        if (c == null) {
            // VCL before any parameter set: nothing to configure with. The epoch already asked for a
            // keyframe; iOS sends the parameter sets with it.
            requestKeyframe()
            return
        }
        absorb(c, scan)
        if (!configured) return
        // Skip only a PURE parameter-set message — csd already carries those. A mixed message
        // (parameter sets + VCL slices, which IDR access units routinely are) must be fed whole:
        // MediaCodec accepts in-band parameter sets inside an Annex-B AU, and dropping it would throw
        // away the keyframe.
        if (scan.sawParamSet && !scan.sawVcl) return

        if (!sawKeyframe) {
            if (!scan.keyframe) {
                requestKeyframe()
                return
            }
            sawKeyframe = true
            log.i("keyframe — decoding starts")
        }
        feed(msg)
    }

    /**
     * Latch the codec, merge any parameter sets the message carried, and (re)configure once the set
     * the codec needs is complete. Reconfigures if the parameter sets CHANGED mid-session (iOS
     * re-SETUP / resolution change): feeding new-stream AUs into a codec configured for the old ones
     * decodes garbage indefinitely with no self-heal.
     */
    private fun absorb(
        c: VideoCodec,
        scan: VideoCodecs.Scan,
    ) {
        if (videoCodec == null) {
            videoCodec = c
            log.i("stream codec: ${c.label}")
        }
        paramSets.absorb(scan)
        if (scan.sawParamSet) log.i("parameter sets: ${paramSets.describe()}")
        val csd = paramSets.csd(c) ?: return
        if (configured && !csd.contentEquals(configuredCsd)) {
            log.i("parameter sets changed — reconfiguring decoder")
            releaseCodec() // sets configured = false
            sawKeyframe = false
        }
        if (!configured) maybeConfigure(c, csd)
    }

    private fun requestKeyframe() {
        val now = SystemClock.elapsedRealtime()
        if (now - lastKeyframeReq < KEYFRAME_COOLDOWN_MS) return
        lastKeyframeReq = now
        onKeyframeNeeded()
    }

    /** Owned by the consume thread — never call from the UI thread while a dequeue may be in flight. */
    private fun releaseCodec() {
        val c = codec ?: return
        codec = null
        configured = false
        runCatching { c.stop() }
        runCatching { c.release() }
    }

    private fun maybeConfigure(
        vc: VideoCodec,
        csd: ByteArray,
    ) {
        val now = SystemClock.elapsedRealtime()
        if (configureFailedAt != 0L && now - configureFailedAt < CONFIGURE_RETRY_MS) return
        var c: MediaCodec? = null
        runCatching {
            val fmt = MediaFormat.createVideoFormat(vc.mime, width, height)
            if (vc == VideoCodec.HEVC) {
                // HEVC: ONE csd-0 holding VPS+SPS+PPS. Splitting them the way H.264 does fails here.
                fmt.setByteBuffer("csd-0", ByteBuffer.wrap(csd))
            } else {
                fmt.setByteBuffer("csd-0", ByteBuffer.wrap(paramSets.sps!!))
                fmt.setByteBuffer("csd-1", ByteBuffer.wrap(paramSets.pps!!))
            }
            // Without this the input buffer is a vendor default; the NAL most likely to overflow it is
            // the IDR, and losing that costs every frame until the next one.
            fmt.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, MAX_INPUT_SIZE)
            val mc = MediaCodec.createDecoderByType(vc.mime)
            c = mc
            mc.configure(fmt, surface, null, 0)
            mc.start()
            codec = mc
            configuredCsd = csd
            configured = true
            configureFailedAt = 0L
            log.i("MediaCodec configured: ${vc.mime} ${width}x$height csd=${csd.size} B, decoder=${mc.name}")
        }.onFailure { e ->
            // runCatching, not catch Exception: an OutOfMemoryError while the framework allocates the
            // input buffers is an Error, and catching only Exception let it escape with the native
            // codec never released AND configureFailedAt never armed — so the next producer re-dial
            // leaked another one, walking the global codec pool down to a permanent black screen.
            // Bare release is correct HERE: stop() is invalid from the Configured state.
            runCatching { c?.release() }
            configureFailedAt = now
            log.e("configure failed (retry in ${CONFIGURE_RETRY_MS}ms): ${e.message}")
        }
    }

    private fun feed(auBytes: ByteArray) {
        val c = codec ?: return
        try {
            var idx = -1
            // Retry rather than drop: a dropped slice corrupts every frame until the next IDR, which
            // CarPlay may not send for a long time.
            while (running.get() && idx < 0) {
                idx = c.dequeueInputBuffer(INPUT_DEQUEUE_US)
                if (idx < 0) drain(c)
            }
            if (idx < 0) return
            val ib = c.getInputBuffer(idx)
            if (ib == null) {
                c.queueInputBuffer(idx, 0, 0, 0, 0)
                return
            }
            ib.clear()
            if (ib.remaining() < auBytes.size) {
                c.queueInputBuffer(idx, 0, 0, 0, 0)
                ausDropped.incrementAndGet()
                log.e("AU ${auBytes.size} B exceeds input buffer ${ib.remaining()} B — dropped, requesting keyframe")
                requestKeyframe()
                return
            }
            ib.put(auBytes)
            c.queueInputBuffer(idx, 0, auBytes.size, SystemClock.uptimeMillis() * 1000, 0)
            drain(c)
        } catch (e: IllegalStateException) {
            // Recover from a codec that threw mid-stream (CodecException is an ISE). Deliberately
            // does NOT clear configureFailedAt: clearing it meant a codec that threw on every access
            // unit rebuilt at frame rate — the codec-pool drain the backoff exists to prevent.
            log.e("codec in error state: ${e.message} — resetting")
            releaseCodec()
            sawKeyframe = false
            configureFailedAt = SystemClock.elapsedRealtime()
            requestKeyframe()
        }
    }

    private fun drain(c: MediaCodec) {
        val info = MediaCodec.BufferInfo()
        var more = true
        while (more) {
            val outIdx =
                try {
                    c.dequeueOutputBuffer(info, 0)
                } catch (_: IllegalStateException) {
                    return
                }
            when {
                outIdx >= 0 -> {
                    // render only a real frame; render=true hands it to the Surface with no CPU copy
                    val real = info.size != 0
                    c.releaseOutputBuffer(outIdx, real)
                    val n = if (real) framesRendered.incrementAndGet() else 0L
                    if (n == 1L) log.i("FIRST FRAME RENDERED")
                    if (n > 0L && n % LOG_EVERY_FRAMES == 0L) log.i("$n frames rendered (${bytesIn.get()} B in)")
                }
                outIdx == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED -> log.i("output format: ${c.outputFormat}")
                outIdx == MediaCodec.INFO_OUTPUT_BUFFERS_CHANGED -> Unit
                else -> more = false // INFO_TRY_AGAIN_LATER
            }
        }
    }
}

/**
 * One `[u32 BE len][payload]` seam message, or null on EOF / an implausible length (the caller drops
 * the connection; the box re-dials). An empty payload is returned as an empty array, not null.
 */
private fun readSeamMessage(
    ins: InputStream,
    hdr: ByteArray,
    log: ProbeLog.Logger,
): ByteArray? {
    if (!StreamIo.readFully(ins, hdr, 4)) return null
    val len =
        ((hdr[0].toInt() and 0xFF) shl 24) or ((hdr[1].toInt() and 0xFF) shl 16) or
            ((hdr[2].toInt() and 0xFF) shl 8) or (hdr[3].toInt() and 0xFF)
    if (len < 0 || len > MAX_SEAM_MESSAGE) {
        log.e("implausible seam message length $len — desync, dropping connection")
        return null
    }
    val msg = ByteArray(len)
    return if (len == 0 || StreamIo.readFully(ins, msg, len)) msg else null
}

private const val MAX_SEAM_MESSAGE = 8 * 1024 * 1024
