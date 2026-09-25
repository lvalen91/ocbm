package com.carlink.av

import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import com.carlink.ocbm.seam.SeamCrypto
import java.nio.ByteBuffer

/**
 * One access unit in, S16LE PCM out — the single decode interface every CarPlay audio stream goes
 * through, whatever codec the box negotiated for it.
 *
 * The seam hands every stream to a player as `[VoiceTag][AU]`, where the tag's `codec` is a
 * `SeamCrypto.CODEC_*` value. This is the ONE place that byte is turned into a decoder:
 *
 *  - [SeamCrypto.CODEC_PCM]     → [PcmPassthrough]. Already S16LE (the seam byte-swapped the big-endian
 *                                 AirPlay downlink and decoded mSBC); written to the track as-is.
 *  - [SeamCrypto.CODEC_AAC_LC]  → [MediaCodecAacDecoder] with the LC AudioSpecificConfig. Wireless
 *                                 CarPlay media (type 102, 48 kHz stereo).
 *  - [SeamCrypto.CODEC_AAC_ELD] → [MediaCodecAacDecoder] with the ELD ASC. Wireless CarPlay voice
 *                                 (16 kHz mono), alert and alt-audio (48 kHz stereo).
 *  - anything else               → [AudioDecoders.open] throws; callers ask [AudioDecoders.canDecode]
 *                                 FIRST and drop the stream with a diagnostic naming the codec, so an
 *                                 undecodable stream never requests focus or opens a track.
 *
 * Opus (`CODEC_OPUS`) is deliberately absent: neither box preset advertises it (`info.rs`
 * `preset_wired_pcm` / `preset_wireless_8`), so no session can negotiate it today, and an untested
 * decode path is worse than a loud drop.
 *
 * The implementation is not thread-safe by design: a decoder is owned by exactly one consume thread
 * (the media player's or a voice sink's), which configures, feeds, drains and closes it. Closing a
 * MediaCodec from another thread while `decode` is inside a dequeue is a native crash, not an
 * exception — see `VoiceRouter.stop`.
 */
interface AudioDecoder : AutoCloseable {
    /** Human-readable name for the configure log line (`"direct PCM"`, `"c2.android.aac.decoder"`). */
    val name: String

    /**
     * Decode one access unit. [sink] is invoked zero or more times with S16LE PCM at the configured
     * rate/channels. A [MediaCodec.CodecException] or [IllegalStateException] propagates: the caller
     * releases the sink/player and arms its configure backoff, exactly as before this abstraction.
     */
    fun decode(
        au: ByteArray,
        off: Int,
        len: Int,
        sink: (ByteArray, Int, Int) -> Unit,
    )
}

/** PCM: the AU IS the PCM. */
class PcmPassthrough : AudioDecoder {
    override val name: String = "direct PCM"

    override fun decode(
        au: ByteArray,
        off: Int,
        len: Int,
        sink: (ByteArray, Int, Int) -> Unit,
    ) {
        if (len >= 2) sink(au, off, len)
    }

    override fun close() = Unit
}

/**
 * AAC-LC / AAC-ELD through [MediaCodec], fed RAW access units (no ADTS) with the codec configured
 * from an explicit `csd-0`. AAC decoders treat `csd-0` as authoritative over `KEY_SAMPLE_RATE` /
 * `KEY_CHANNEL_COUNT`, which is why the ASC is built per stream ([AacCsd]) and never hardcoded.
 */
class MediaCodecAacDecoder(
    private val codec: MediaCodec,
    override val name: String,
) : AudioDecoder {
    private val info = MediaCodec.BufferInfo()

    private companion object {
        /** Long enough for a healthy decoder to free an input buffer; short enough not to stall the lane. */
        const val INPUT_TIMEOUT_US = 10_000L
    }

    override fun decode(
        au: ByteArray,
        off: Int,
        len: Int,
        sink: (ByteArray, Int, Int) -> Unit,
    ) {
        queueInput(au, off, len)
        drainOutput(sink)
    }

    private fun queueInput(
        au: ByteArray,
        off: Int,
        len: Int,
    ) {
        val inIdx = codec.dequeueInputBuffer(INPUT_TIMEOUT_US)
        if (inIdx < 0) return
        // A dequeued index MUST be queued back on EVERY path, including the null-buffer and
        // too-large ones. Input buffers are a fixed pool of 4-8; leaking them makes
        // dequeueInputBuffer return TRY_AGAIN_LATER forever — silent dead audio with a full
        // timeout per frame.
        var size = 0
        try {
            val ib = codec.getInputBuffer(inIdx)
            if (ib != null && len <= ib.remaining()) {
                ib.clear()
                ib.put(au, off, len)
                size = len
            }
        } finally {
            codec.queueInputBuffer(inIdx, 0, size, System.nanoTime() / 1000, 0)
        }
    }

    private fun drainOutput(sink: (ByteArray, Int, Int) -> Unit) {
        var outIdx = codec.dequeueOutputBuffer(info, 0)
        while (outIdx >= 0) {
            val ob = codec.getOutputBuffer(outIdx)
            if (ob != null && info.size > 0) {
                val pcm = ByteArray(info.size)
                ob.position(info.offset)
                ob.get(pcm)
                sink(pcm, 0, pcm.size)
            }
            codec.releaseOutputBuffer(outIdx, false)
            outIdx = codec.dequeueOutputBuffer(info, 0)
        }
    }

    /** stop-then-release: a bare release can click on some HALs; each call independently guarded. */
    override fun close() {
        runCatching { codec.stop() }
        runCatching { codec.release() }
    }
}

/**
 * AudioSpecificConfig builders — pure functions, unit-tested, shared by every AAC decoder.
 */
object AacCsd {
    /** samplingFrequencyIndex per ISO/IEC 14496-3 Table 1.16. */
    val SF_INDEX: IntArray =
        intArrayOf(96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350)

    /** AOT for ER AAC ELD; above 31 it is coded as the 5-bit escape 31 then 6 bits of (aot − 32). */
    private const val AOT_ELD = 39
    private const val AOT_LC = 2
    private const val AOT_ESCAPE = 31
    private const val DEFAULT_SF_INDEX = 3 // 48 kHz

    /**
     * AAC-ELD ASC as the shipping fdk-aac encoder actually emits it for the 16 kHz mono Siri/telephony
     * stream (`ccpa_custom/docs/50`): SBR enabled by fdk auto-mode, frameLength 480. NOT the
     * `f8f03000` the older docs claim. Device-confirmed, so it is returned verbatim rather than
     * synthesised.
     */
    val ELD_CSD_16K_MONO: ByteArray =
        byteArrayOf(0xF8.toByte(), 0xF0.toByte(), 0x31, 0x2C, 0x00, 0xBC.toByte(), 0x00)

    fun sfIndex(rate: Int): Int = SF_INDEX.indexOf(rate).let { if (it < 0) DEFAULT_SF_INDEX else it }

    /** csd-0 for raw AAC-LC: 5 bits objectType | 4 bits rateIdx | 4 bits channelConfig | 3 bits GASpecificConfig(0). */
    fun lc(
        rate: Int,
        channels: Int,
    ): ByteArray {
        val fi = sfIndex(rate)
        val ch = channels.coerceIn(1, 7)
        return byteArrayOf(
            (((AOT_LC shl 3) or (fi shr 1)) and 0xFF).toByte(),
            ((((fi and 1) shl 7) or (ch shl 3)) and 0xFF).toByte(),
        )
    }

    /**
     * csd-0 for AAC-ELD at [rate]/[channels].
     *
     * A single hardcoded ASC was wrong: it encodes samplingFrequencyIndex 8 (16 kHz), channel
     * config 1, and AAC decoders treat csd-0 as AUTHORITATIVE over the MediaFormat rate/channels. The
     * voice lane carries MIXED formats (telephony/Siri 16 k mono, alert/nav 48 k stereo), so handing
     * every sink the 16 k mono ASC made nav and alert either fail to configure or decode ~3x fast
     * with the channels garbled.
     *
     * Layout: 5 bits AOT (escape 31 + 6-bit (39-32)), 4 bits freq index, 4 bits channel config, then
     * the ELDSpecificConfig: frameLengthFlag=1 (480 samples — every proven ELD config in this project
     * is 480; a mismatch is a configure failure or garbage), the three resilience flags 0,
     * ldSbrPresentFlag 0, and a 4-bit ELDEXT_TERM.
     */
    fun eld(
        rate: Int,
        channels: Int,
    ): ByteArray {
        if (rate == 16000 && channels == 1) return ELD_CSD_16K_MONO
        val w = BitWriter()
        w.put(AOT_ESCAPE, 5)
        w.put(AOT_ELD - 32, 6)
        w.put(sfIndex(rate), 4)
        w.put(channels.coerceIn(1, 2), 4)
        w.put(1, 1) // frameLengthFlag = 1 => 480
        w.put(0, 1) // aacSectionDataResilienceFlag
        w.put(0, 1) // aacScalefactorDataResilienceFlag
        w.put(0, 1) // aacSpectralDataResilienceFlag
        w.put(0, 1) // ldSbrPresentFlag
        w.put(0, 4) // ELDEXT_TERM
        return w.bytes()
    }

    private class BitWriter {
        private var acc = 0L
        private var bits = 0

        fun put(
            v: Int,
            n: Int,
        ) {
            acc = (acc shl n) or (v.toLong() and ((1L shl n) - 1))
            bits += n
        }

        fun bytes(): ByteArray {
            val pad = (8 - (bits % 8)) % 8
            put(0, pad)
            val out = ByteArray(bits / 8)
            for (i in out.indices) out[i] = ((acc shr ((out.size - 1 - i) * 8)) and 0xFF).toByte()
            return out
        }
    }
}

/** The codec → decoder dispatch table. */
object AudioDecoders {
    /** How a `SeamCrypto.CODEC_*` byte will be handled; pure, so the dispatch is unit-testable off-device. */
    enum class Path { PCM, AAC_LC, AAC_ELD, UNSUPPORTED }

    fun pathFor(codec: Int): Path =
        when (codec) {
            SeamCrypto.CODEC_PCM -> Path.PCM
            SeamCrypto.CODEC_AAC_LC -> Path.AAC_LC
            SeamCrypto.CODEC_AAC_ELD -> Path.AAC_ELD
            else -> Path.UNSUPPORTED
        }

    fun canDecode(codec: Int): Boolean = pathFor(codec) != Path.UNSUPPORTED

    fun codecName(c: Int): String =
        when (c) {
            SeamCrypto.CODEC_PCM -> "PCM"
            SeamCrypto.CODEC_AAC_LC -> "AAC-LC"
            SeamCrypto.CODEC_AAC_ELD -> "AAC-ELD"
            SeamCrypto.CODEC_OPUS -> "OPUS"
            SeamCrypto.CODEC_MSBC -> "mSBC"
            else -> "codec$c"
        }

    /**
     * Open a started decoder for [codec] at [rate]/[channels]. Throws for an unsupported codec (ask
     * [canDecode] first) and for any MediaCodec failure — the caller owns the retry backoff.
     */
    fun open(
        codec: Int,
        rate: Int,
        channels: Int,
    ): AudioDecoder =
        when (pathFor(codec)) {
            Path.PCM -> PcmPassthrough()
            Path.AAC_LC -> aac(rate, channels, MediaCodecInfo.CodecProfileLevel.AACObjectLC, AacCsd.lc(rate, channels))
            Path.AAC_ELD -> aac(rate, channels, MediaCodecInfo.CodecProfileLevel.AACObjectELD, AacCsd.eld(rate, channels))
            Path.UNSUPPORTED -> throw IllegalArgumentException("codec ${codecName(codec)} is not decodable here")
        }

    private fun aac(
        rate: Int,
        channels: Int,
        profile: Int,
        csd: ByteArray,
    ): AudioDecoder {
        val fmt = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_AAC, rate, channels)
        fmt.setInteger(MediaFormat.KEY_AAC_PROFILE, profile)
        fmt.setByteBuffer("csd-0", ByteBuffer.wrap(csd))
        val c = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_AUDIO_AAC)
        runCatching {
            c.configure(fmt, null, null, 0)
            c.start()
        }.onFailure {
            // Bare release is correct here: stop() is invalid from the Configured/Error state.
            runCatching { c.release() }
        }.getOrThrow()
        return MediaCodecAacDecoder(c, c.name)
    }
}

/** S16LE level test shared by the media player and the voice router's duck trigger. */
object PcmLevel {
    /** ~-32 dBFS: separates real speech/music from the digital silence iOS streams on idle lanes. */
    const val PEAK_THRESHOLD: Int = 800

    fun audible(
        pcm: ByteArray,
        off: Int,
        len: Int,
        threshold: Int = PEAK_THRESHOLD,
    ): Boolean {
        var i = off
        val end = off + len - 1
        while (i < end) {
            val s = ((pcm[i + 1].toInt() shl 8) or (pcm[i].toInt() and 0xFF)).toShort().toInt()
            if (s > threshold || s < -threshold) return true
            i += 2
        }
        return false
    }
}
