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
 * HEVC decoder for the CarPlay screen stream, rendering straight to a [Surface].
 *
 * Structured after `carlink_native_personal`'s `H264Renderer` (MediaCodec SYNC mode, dedicated decode
 * thread, keyframe-request callback, watchdog-style drop accounting), with the differences HEVC forces:
 *
 *  - MIME `video/hevc`, and **`csd-0` is VPS+SPS+PPS concatenated** as one buffer. H.264 splits SPS into
 *    `csd-0` and PPS into `csd-1`; HEVC does not — passing them separately fails to configure.
 *  - Parameter sets arrive as the first Annex-B payload on the seam (the receiver converts the `hvcC`
 *    record — `01_FINDINGS.md` §8b), so we buffer until VPS(32)+SPS(33)+PPS(34) are all present.
 *
 * **Access units, not NALs.** MediaCodec expects one AU per input buffer, and the seam already gives us
 * exactly that: it is message-framed `[u32 BE len][payload]` (`forward_screen`, session.rs:1768) with one
 * message per screen message. Treating it as a raw byte stream both loses those boundaries and appends
 * the next message's 4 length bytes to every trailing NAL.
 *
 * **Threading contract.** [consume] owns the codec for its whole life: it configures, feeds, drains and
 * releases on its own thread. [stop] only flips the flag and closes the stream — it never touches the
 * codec, because MediaCodec lifecycle calls racing a dequeue crash natively.
 */
class HevcRenderer(
    private val width: Int,
    private val height: Int,
    private val surface: Surface,
    private val onKeyframeNeeded: () -> Unit = {},
) {
    private val log = ProbeLog.sub("hevc")
    private val running = AtomicBoolean(false)

    @Volatile private var codec: MediaCodec? = null

    @Volatile private var configured = false

    @Volatile private var configureFailedAt = 0L

    val framesRendered = AtomicLong(0)
    val bytesIn = AtomicLong(0)
    val ausDropped = AtomicLong(0)

    private var vps: ByteArray? = null
    private var sps: ByteArray? = null
    private var pps: ByteArray? = null
    private var sawKeyframe = false
    private var lastKeyframeReq = 0L

    /** The VPS+SPS+PPS actually baked into the live codec's csd-0, to detect a mid-session change. */
    private var configuredCsd: ByteArray? = null

    private companion object {
        const val CONFIGURE_RETRY_MS = 5_000L // don't re-attempt configure per NAL (codec-pool leak)
        const val KEYFRAME_COOLDOWN_MS = 500L // H264Renderer's REACTIVE_KEYFRAME_COOLDOWN
        const val MAX_MESSAGE = 8 * 1024 * 1024 // desync guard on the seam length prefix

        // Match the seam's max message: a seam-legal AU in (old 2 MB, 8 MB] — most dangerously the IDR
        // itself — was silently dropped, and dropping the replacement keyframe stalls video entirely.
        const val MAX_INPUT_SIZE = MAX_MESSAGE
    }

    fun start() {
        // A restart must not carry parameter sets or keyframe state across streams: mixing a new VPS
        // with a stale SPS/PPS configures a codec that cannot decode what follows.
        vps = null
        sps = null
        pps = null
        sawKeyframe = false
        configured = false
        configureFailedAt = 0L
        running.set(true)
    }

    /**
     * Flag-only. The codec is released by [consume] on its own thread, and this does NOT close the
     * stream — a [consume] parked in `read` unblocks only when the caller closes the socket, which
     * [CarPlayActivity.detachRenderer] does.
     */
    fun stop() {
        running.set(false)
        log.i("stopping — ${framesRendered.get()} frames, ${bytesIn.get()} bytes, ${ausDropped.get()} AUs dropped")
    }

    /**
     * Consume the seam until it closes. Blocking; call on its own thread.
     *
     * The seam is **message-framed**: `[u32 BE len][payload]`, one message per screen message
     * (`forward_screen`, session.rs:1768). Each payload is either the converted parameter sets
     * (VPS+SPS+PPS from the opcode-1 VideoConfig) or exactly one Annex-B access unit. That framing is
     * the whole point — TCP hides write boundaries, and the prefix hands us clean AU boundaries for
     * free, so no start-code heuristics or first_slice parsing are needed to know where a frame ends.
     */
    fun consume(ins: InputStream) {
        // Every producer re-dial is a NEW decode session on the SAME instance, and re-dial is now the
        // designed heal path. The reconnect issues a ForceKeyFrame, but the frames already in flight are
        // P-frames from the old GOP and this codec has no reference frames — so the IRAP gate must be
        // re-armed or it is dead on every reconnect. VPS/SPS/PPS are deliberately KEPT: a mid-stream
        // re-dial re-sends no VideoConfig, so the cache is the only way to configure at all.
        sawKeyframe = false
        val hdr = ByteArray(4)
        try {
            while (running.get()) {
                if (!readFully(ins, hdr, 4)) break
                val len =
                    ((hdr[0].toInt() and 0xFF) shl 24) or ((hdr[1].toInt() and 0xFF) shl 16) or
                        ((hdr[2].toInt() and 0xFF) shl 8) or (hdr[3].toInt() and 0xFF)
                if (len < 0 || len > MAX_MESSAGE) {
                    log.e("implausible seam message length $len — desync, dropping connection")
                    break
                }
                // A 0-length message is NOT a desync: forward_screen is called unconditionally with
                // whatever the conversion produced (session.rs:2074), and an empty VideoConfig is
                // exactly what this head unit emitted before the sample-description fix. Treating it
                // as desync drops the connection and churns ForceKeyFrame on every reconnect.
                if (len == 0) continue
                val msg = ByteArray(len)
                if (!readFully(ins, msg, len)) break
                bytesIn.addAndGet((len + 4).toLong())
                handleMessage(msg)
            }
        } finally {
            releaseCodec()
            log.i("seam ended — ${framesRendered.get()} frames rendered, ${ausDropped.get()} AUs dropped")
        }
    }

    private fun readFully(
        ins: InputStream,
        dst: ByteArray,
        n: Int,
    ): Boolean {
        var off = 0
        while (off < n) {
            val r =
                try {
                    ins.read(dst, off, n - off)
                } catch (e: Exception) {
                    if (running.get()) log.e("read: ${e.message}")
                    return false
                }
            if (r <= 0) return false
            off += r
        }
        return true
    }

    /** One message = the parameter sets, or one complete access unit. */
    private fun handleMessage(msg: ByteArray) {
        // Classify by scanning the NALs this message carries. Parameter-set messages carry VPS/SPS/PPS
        // and no VCL NAL; frame messages are fed whole, exactly one queueInputBuffer per AU.
        var sawParamSet = false
        var sawVcl = false
        var keyframe = false
        var i = 0
        while (i < msg.size - 4) {
            val long4 =
                msg[i].toInt() == 0 &&
                    msg[i + 1].toInt() == 0 &&
                    msg[i + 2].toInt() == 0 &&
                    msg[i + 3].toInt() == 1
            val short3 = !long4 && msg[i].toInt() == 0 && msg[i + 1].toInt() == 0 && msg[i + 2].toInt() == 1
            if (!long4 && !short3) {
                i++
                continue
            }
            val off = i + (if (long4) 4 else 3)
            if (off >= msg.size) break
            val type = (msg[off].toInt() shr 1) and 0x3F
            val to = nextStart(msg, off)
            when (type) {
                32 -> {
                    vps = msg.copyOfRange(i, to)
                    sawParamSet = true
                    log.i("VPS (${to - i} B)")
                }
                33 -> {
                    sps = msg.copyOfRange(i, to)
                    sawParamSet = true
                    log.i("SPS (${to - i} B)")
                }
                34 -> {
                    pps = msg.copyOfRange(i, to)
                    sawParamSet = true
                    log.i("PPS (${to - i} B)")
                }
                in 0..31 -> {
                    sawVcl = true
                    if (type in 16..21) keyframe = true
                } // 16..21 = IRAP
            }
            i = off
        }

        // Reconfigure if the parameter sets CHANGED mid-session (iOS re-SETUP / resolution change):
        // feeding new-stream AUs into a codec configured for the old VPS/SPS/PPS decodes garbage
        // indefinitely with no self-heal.
        if (vps != null && sps != null && pps != null) {
            val csd = vps!! + sps!! + pps!!
            if (configured && !csd.contentEquals(configuredCsd)) {
                log.i("parameter sets changed — reconfiguring decoder")
                releaseCodec() // sets configured = false
                sawKeyframe = false
            }
            if (!configured) maybeConfigure()
        }
        if (!configured) return
        // Skip only a PURE parameter-set message — csd-0 already carries those. A mixed message
        // (parameter sets + VCL slices, which HEVC IDR access units routinely are) must be fed whole:
        // MediaCodec accepts in-band parameter sets inside an Annex-B AU, and dropping it would throw
        // away the keyframe. ccpa_custom's proven consumer does the same (H264Decoder.swift:236-241).
        if (sawParamSet && !sawVcl) return

        if (!sawKeyframe) {
            if (!keyframe) {
                requestKeyframe()
                return
            }
            sawKeyframe = true
            log.i("keyframe — decoding starts")
        }
        feed(msg)
    }

    private fun nextStart(
        msg: ByteArray,
        from: Int,
    ): Int {
        var i = from
        while (i < msg.size - 4) {
            if (msg[i].toInt() == 0 && msg[i + 1].toInt() == 0) {
                if (msg[i + 2].toInt() == 1) return i
                if (msg[i + 2].toInt() == 0 && msg[i + 3].toInt() == 1) return i
            }
            i++
        }
        return msg.size
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

    private fun maybeConfigure() {
        val now = SystemClock.elapsedRealtime()
        if (configureFailedAt != 0L && now - configureFailedAt < CONFIGURE_RETRY_MS) return
        var c: MediaCodec? = null
        try {
            val fmt = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_HEVC, width, height)
            // HEVC: ONE csd-0 holding VPS+SPS+PPS. Splitting them the way H.264 does fails here.
            val csd = vps!! + sps!! + pps!!
            fmt.setByteBuffer("csd-0", ByteBuffer.wrap(csd))
            // Without this the input buffer is a vendor default; the NAL most likely to overflow it is
            // the IDR, and losing that costs every frame until the next one.
            fmt.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, MAX_INPUT_SIZE)
            c = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_HEVC)
            c.configure(fmt, surface, null, 0)
            c.start()
            codec = c
            configuredCsd = csd
            configured = true
            configureFailedAt = 0L
            log.i("MediaCodec configured: video/hevc ${width}x$height csd-0=${csd.size} B (VPS+SPS+PPS), decoder=${c.name}")
        } catch (e: Throwable) {
            // Throwable, not Exception: an OutOfMemoryError while the framework allocates the input
            // buffers is an Error, and catching only Exception let it escape with the native codec
            // never released AND configureFailedAt never armed — so the next producer re-dial leaked
            // another one, walking the global codec pool down to a permanent black screen. Bare
            // release is correct HERE: stop() is invalid from the Configured state.
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
            while (running.get()) {
                idx = c.dequeueInputBuffer(100_000)
                if (idx >= 0) break
                drain(c)
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
            log.e("codec in error state: ${e.message} — resetting")
            resetCodec()
        } catch (e: Exception) {
            log.e("feed: ${e.message}")
        }
    }

    /**
     * Recover from a codec that threw mid-stream. Reached only from the ISE/CodecException handler,
     * i.e. with the codec in the **Error** state.
     *
     * It deliberately does NOT clear [configureFailedAt]. Clearing it meant a codec that threw on
     * every access unit rebuilt at frame rate — release + createDecoderByType + configure + start,
     * tens of ms of native work per frame — which is exactly the codec-pool drain the backoff exists
     * to prevent, just entered through the feed path instead of the configure path.
     */
    private fun resetCodec() {
        releaseCodec() // single release path; it already does stop-then-release correctly
        sawKeyframe = false
        configureFailedAt = android.os.SystemClock.elapsedRealtime()
        requestKeyframe()
    }

    private fun drain(c: MediaCodec) {
        val info = MediaCodec.BufferInfo()
        while (true) {
            val outIdx =
                try {
                    c.dequeueOutputBuffer(info, 0)
                } catch (e: IllegalStateException) {
                    return
                }
            when {
                outIdx >= 0 -> {
                    // render only a real frame; render=true hands it to the Surface with no CPU copy
                    c.releaseOutputBuffer(outIdx, info.size != 0)
                    if (info.size != 0) {
                        val n = framesRendered.incrementAndGet()
                        if (n == 1L) log.i("FIRST FRAME RENDERED")
                        if (n % 300 == 0L) log.i("$n frames rendered (${bytesIn.get()} B in)")
                    }
                }
                outIdx == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED -> log.i("output format: ${c.outputFormat}")
                outIdx == MediaCodec.INFO_OUTPUT_BUFFERS_CHANGED -> {}
                else -> return // INFO_TRY_AGAIN_LATER
            }
        }
    }
}
