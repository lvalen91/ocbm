package com.carlink.av

import android.media.MediaCodecList
import android.media.MediaFormat

/** The two codecs a CarPlay screen stream can be negotiated in. iOS picks; the box forwards byte-for-byte. */
enum class VideoCodec(
    val mime: String,
    val label: String,
) {
    H264(MediaFormat.MIMETYPE_VIDEO_AVC, "h264"),
    HEVC(MediaFormat.MIMETYPE_VIDEO_HEVC, "hevc"),
}

/**
 * Codec-neutral Annex-B access-unit inspection plus the decoder capability probe.
 *
 * Pure functions (no MediaCodec) so the classification the renderer keys everything on — which
 * codec, is this a parameter set, is this a keyframe — is unit-tested off-device. The NAL header
 * differs between the two codecs: H.264 is `forbidden(1) ref_idc(2) type(5)`, HEVC is
 * `forbidden(1) type(6) layer(6) tid(3)`. A parameter-set message's first NAL identifies the codec
 * unambiguously ([sniff]), because no HEVC VPS/SPS/PPS header byte (`0x40`/`0x42`/`0x44`) reads as an
 * H.264 SPS/PPS (`x7`/`x8` in the low five bits) and vice versa.
 */
object VideoCodecs {
    private const val H264_SPS = 7
    private const val H264_PPS = 8
    private const val H264_IDR = 5
    private const val H264_VCL_MAX = 5
    private const val HEVC_VPS = 32
    private const val HEVC_SPS = 33
    private const val HEVC_PPS = 34
    private const val HEVC_VCL_MAX = 31
    private const val HEVC_IRAP_MIN = 16
    private const val HEVC_IRAP_MAX = 21
    const val PS_NONE: Int = 0
    const val PS_VPS: Int = 1
    const val PS_SPS: Int = 2
    const val PS_PPS: Int = 3

    /** What one seam message carries, after a NAL walk. Parameter sets keep their start codes. */
    class Scan {
        var codec: VideoCodec? = null
        var vps: ByteArray? = null
        var sps: ByteArray? = null
        var pps: ByteArray? = null
        var sawParamSet = false
        var sawVcl = false
        var keyframe = false

        fun put(
            kind: Int,
            nal: ByteArray,
        ) {
            sawParamSet = true
            when (kind) {
                PS_VPS -> vps = nal
                PS_SPS -> sps = nal
                PS_PPS -> pps = nal
            }
        }
    }

    /**
     * The renderer's parameter-set cache across messages, and the `csd` it configures with: HEVC needs
     * VPS+SPS+PPS as ONE `csd-0`; H.264 needs SPS (`csd-0`) and PPS (`csd-1`).
     */
    class ParamSets {
        var vps: ByteArray? = null
        var sps: ByteArray? = null
        var pps: ByteArray? = null

        fun absorb(scan: Scan) {
            scan.vps?.let { vps = it }
            scan.sps?.let { sps = it }
            scan.pps?.let { pps = it }
        }

        fun clear() {
            vps = null
            sps = null
            pps = null
        }

        fun describe(): String = "vps=${vps?.size ?: 0} sps=${sps?.size ?: 0} pps=${pps?.size ?: 0} B"

        /** The complete csd for [c], or null while a parameter set the codec needs is still missing. */
        fun csd(c: VideoCodec): ByteArray? {
            val s = sps ?: return null
            val p = pps ?: return null
            return if (c == VideoCodec.HEVC) vps?.let { it + s + p } else s + p
        }
    }

    fun nalType(
        codec: VideoCodec,
        hdr: Int,
    ): Int = if (codec == VideoCodec.HEVC) (hdr shr 1) and 0x3F else hdr and 0x1F

    /** Identify the codec from the header byte of a PARAMETER-SET NAL; null for anything else. */
    fun sniff(hdr: Int): VideoCodec? =
        when {
            nalType(VideoCodec.HEVC, hdr) in HEVC_VPS..HEVC_PPS -> VideoCodec.HEVC
            nalType(VideoCodec.H264, hdr) in H264_SPS..H264_PPS -> VideoCodec.H264
            else -> null
        }

    /** 1 = VPS, 2 = SPS, 3 = PPS, 0 = not a parameter set, for [codec]. */
    fun paramSetKind(
        codec: VideoCodec,
        type: Int,
    ): Int =
        if (codec == VideoCodec.HEVC) {
            when (type) {
                HEVC_VPS -> PS_VPS
                HEVC_SPS -> PS_SPS
                HEVC_PPS -> PS_PPS
                else -> PS_NONE
            }
        } else {
            when (type) {
                H264_SPS -> PS_SPS
                H264_PPS -> PS_PPS
                else -> PS_NONE
            }
        }

    /**
     * Walk the NALs of one Annex-B message. [known] is the codec already latched for the stream, or
     * null to sniff it from this message's first parameter set — a VCL-only message under an unknown
     * codec yields `codec == null` and the caller waits for the parameter sets.
     */
    fun scan(
        known: VideoCodec?,
        msg: ByteArray,
    ): Scan {
        val r = Scan()
        r.codec = known
        var i = 0
        while (i < msg.size - 4) {
            val scLen = startCodeAt(msg, i)
            if (scLen == 0) {
                i++
                continue
            }
            val off = i + scLen // < msg.size, since i < size - 4 and scLen <= 4
            val hdr = msg[off].toInt() and 0xFF
            if (r.codec == null) r.codec = sniff(hdr)
            val c = r.codec ?: return Scan().also { it.sawVcl = true }
            val type = nalType(c, hdr)
            val kind = paramSetKind(c, type)
            if (kind != PS_NONE) {
                r.put(kind, msg.copyOfRange(i, nextStart(msg, off)))
            } else if (isVcl(c, type)) {
                r.sawVcl = true
                r.keyframe = r.keyframe || isKeyframe(c, type)
            }
            i = off
        }
        return r
    }

    fun isVcl(
        codec: VideoCodec,
        type: Int,
    ): Boolean = if (codec == VideoCodec.HEVC) type in 0..HEVC_VCL_MAX else type in 1..H264_VCL_MAX

    /** HEVC: any IRAP (BLA/IDR/CRA, 16..21). H.264: IDR (5). */
    fun isKeyframe(
        codec: VideoCodec,
        type: Int,
    ): Boolean = if (codec == VideoCodec.HEVC) type in HEVC_IRAP_MIN..HEVC_IRAP_MAX else type == H264_IDR

    /** 4 for `00 00 00 01`, 3 for `00 00 01`, else 0. */
    private fun startCodeAt(
        b: ByteArray,
        i: Int,
    ): Int {
        if (b[i].toInt() != 0 || b[i + 1].toInt() != 0) return 0
        if (b[i + 2].toInt() == 1) return 3
        return if (b[i + 2].toInt() == 0 && b[i + 3].toInt() == 1) 4 else 0
    }

    private fun nextStart(
        msg: ByteArray,
        from: Int,
    ): Int {
        var i = from
        while (i < msg.size - 4) {
            if (startCodeAt(msg, i) != 0) return i
            i++
        }
        return msg.size
    }

    /** Which decoders this device has for the negotiated geometry; null = none. */
    class Capabilities(
        val h264Decoder: String?,
        val hevcDecoder: String?,
    ) {
        val hevc: Boolean get() = hevcDecoder != null
    }

    /**
     * Ask [MediaCodecList] for a decoder that accepts [width]x[height]@[fps] per codec. This feeds the
     * `enablesHEVC` the app pushes in `VehicleConfigYaml`, so the box never advertises `hevcInfo` to
     * iOS on a head unit whose decoder cannot take the stream — which is a black screen with every
     * frame decrypting perfectly and no error anywhere.
     */
    fun probe(
        width: Int,
        height: Int,
        fps: Int,
    ): Capabilities {
        fun find(mime: String): String? =
            runCatching {
                val fmt = MediaFormat.createVideoFormat(mime, width, height)
                fmt.setInteger(MediaFormat.KEY_FRAME_RATE, fps)
                MediaCodecList(MediaCodecList.REGULAR_CODECS).findDecoderForFormat(fmt)
            }.getOrNull()
        return Capabilities(find(VideoCodec.H264.mime), find(VideoCodec.HEVC.mime))
    }
}
