package com.carlink.ocbm.seam

import com.carlink.logging.ProbeLog
import java.util.concurrent.atomic.AtomicLong

/**
 * `CH_MEDIA_AUDIO` + `CH_ALT_AUDIO` → the proven `AacPlayer` and `VoiceRouter`.
 *
 * Like [VideoSeam] this is a transcoder from the forward-encrypted v2 seam into the legacy framing the
 * existing players already speak, so neither player changes:
 *
 *  - **media** → ADTS-framed AAC, the byte stream `AacPlayer.consume` walks;
 *  - **voice** → `[u32 BE rate][u16 BE ch][u8 atype][u32 BE len][AU]`, the 11-byte tag
 *    `VoiceRouter.consume` reads (`forward.rs:101-109`).
 *
 * `VoiceRouter` even diagnoses the mismatch this class exists to remove — it rejects an implausible
 * rate with *"the seam is probably speaking the forward-encrypted v2 framing"*. It now never sees it.
 *
 * ## Both channels, one instance
 *
 * The two seams are physically separate but the `scid` key and format tables are shared, exactly as in
 * the macOS host: a stream's SEAM_KEY and SEAM_FORMAT may arrive on either, and every message is
 * scid-tagged precisely so concurrent streams (telephony and alert share the voice sink) can never
 * clobber each other. Route on the format's `audioType`, never on which channel a packet arrived on.
 *
 * ## Wire
 *
 * `[u32 BE len][marker][...]`, with:
 *  - `0x00 SEAM_KEY`    `[key 32][scid 8 LE]`
 *  - `0x01 SEAM_PKT`    `[scid 8 LE][raw encrypted RTP packet]`
 *  - `0x02 SEAM_FORMAT` `[scid 8 LE][codec][rate u32 LE][ch][bits][atype]`
 */
class AudioSeam(
    private val mediaPipe: SeamPipe,
    private val voicePipe: SeamPipe,
    private val log: ProbeLog.Logger,
) {
    companion object {
        /** Generous vs any real RTP packet; a larger declared length is a desync, not a jumbo packet. */
        private const val MAX_MESSAGE = 1 shl 20
        private val SF_INDEX =
            intArrayOf(
                96000,
                88200,
                64000,
                48000,
                44100,
                32000,
                24000,
                22050,
                16000,
                12000,
                11025,
                8000,
                7350,
            )
    }

    class Format(
        val codec: Int,
        val rate: Int,
        val channels: Int,
        val bits: Int,
        val atype: Int,
    )

    val decryptOk = AtomicLong(0)
    val decryptFail = AtomicLong(0)
    val unkeyed = AtomicLong(0)

    private val keys = HashMap<Long, ByteArray>()
    private val formats = HashMap<Long, Format>()

    private val media = Buf()
    private val voice = Buf()

    private class Buf {
        var b = ByteArray(1 shl 16)
        var end = 0
        var start = 0
    }

    fun feedMedia(payload: ByteArray) = feed(media, payload)

    fun feedVoice(payload: ByteArray) = feed(voice, payload)

    @Synchronized
    private fun feed(
        s: Buf,
        payload: ByteArray,
    ) {
        compact(s)
        if (s.end + payload.size > s.b.size) {
            if (s.start > 0) {
                System.arraycopy(s.b, s.start, s.b, 0, s.end - s.start)
                s.end -= s.start
                s.start = 0
            }
            var cap = s.b.size
            while (cap < s.end + payload.size) cap = cap shl 1
            if (cap != s.b.size) s.b = s.b.copyOf(cap)
        }
        System.arraycopy(payload, 0, s.b, s.end, payload.size)
        s.end += payload.size
        drain(s)
    }

    private fun compact(s: Buf) {
        if (s.start >= s.end) {
            s.start = 0
            s.end = 0
            return
        }
        if (s.start > (1 shl 16)) {
            System.arraycopy(s.b, s.start, s.b, 0, s.end - s.start)
            s.end -= s.start
            s.start = 0
        }
    }

    private fun be32(
        b: ByteArray,
        off: Int,
    ): Int =
        ((b[off].toInt() and 0xFF) shl 24) or ((b[off + 1].toInt() and 0xFF) shl 16) or
            ((b[off + 2].toInt() and 0xFF) shl 8) or (b[off + 3].toInt() and 0xFF)

    private fun le32(
        b: ByteArray,
        off: Int,
    ): Int =
        (b[off].toInt() and 0xFF) or ((b[off + 1].toInt() and 0xFF) shl 8) or
            ((b[off + 2].toInt() and 0xFF) shl 16) or ((b[off + 3].toInt() and 0xFF) shl 24)

    private fun le64(
        b: ByteArray,
        off: Int,
    ): Long {
        var v = 0L
        for (k in 0 until 8) v = v or ((b[off + k].toLong() and 0xFF) shl (8 * k))
        return v
    }

    private fun drain(s: Buf) {
        while (s.end - s.start >= 4) {
            val mlen = be32(s.b, s.start)
            // No magic on this seam, so an implausible length can only be resynced byte-wise.
            if (mlen <= 0 || mlen > MAX_MESSAGE) {
                s.start += 1
                continue
            }
            if (s.end - s.start < 4 + mlen) return
            handle(s.b, s.start + 4, mlen)
            s.start += 4 + mlen
            compact(s)
        }
    }

    private fun handle(
        b: ByteArray,
        off: Int,
        len: Int,
    ) {
        if (len < 1) return
        when (b[off].toInt() and 0xFF) {
            SeamCrypto.MARK_KEY -> {
                if (len < 41) return
                val scid = le64(b, off + 33)
                keys[scid] = b.copyOfRange(off + 1, off + 33)
                log.i("received audio key (scid=$scid)")
            }
            SeamCrypto.MARK_FORMAT -> {
                if (len < 17) return
                val scid = le64(b, off + 1)
                val f =
                    Format(
                        codec = b[off + 9].toInt() and 0xFF,
                        rate = le32(b, off + 10),
                        channels = b[off + 14].toInt() and 0xFF,
                        bits = b[off + 15].toInt() and 0xFF,
                        atype = b[off + 16].toInt() and 0xFF,
                    )
                val prev = formats[scid]
                formats[scid] = f
                if (prev == null ||
                    prev.codec != f.codec ||
                    prev.rate != f.rate ||
                    prev.channels != f.channels ||
                    prev.atype != f.atype
                ) {
                    log.i(
                        "audio format scid=$scid: codec=${codecName(f.codec)} ${f.rate}Hz " +
                            "${f.channels}ch atype=${f.atype} -> ${if (SeamCrypto.isMediaAudioType(f.atype)) "media" else "voice"}",
                    )
                }
            }
            SeamCrypto.MARK_PKT -> {
                if (len < 9) return
                val scid = le64(b, off + 1)
                val key = keys[scid]
                if (key == null) {
                    unkeyed.incrementAndGet()
                    return
                }
                val pkt = b.copyOfRange(off + 9, off + len)
                val au = SeamCrypto.openAudio(key, pkt)
                if (au == null) {
                    val n = decryptFail.incrementAndGet()
                    if (n == 1L || n % 500 == 0L) log.e("audio decrypt FAILED (ok=${decryptOk.get()} fail=$n)")
                    return
                }
                decryptOk.incrementAndGet()
                route(scid, au)
            }
        }
    }

    private fun codecName(c: Int) =
        when (c) {
            SeamCrypto.CODEC_PCM -> "PCM"
            SeamCrypto.CODEC_AAC_LC -> "AAC-LC"
            SeamCrypto.CODEC_AAC_ELD -> "AAC-ELD"
            SeamCrypto.CODEC_OPUS -> "OPUS"
            else -> "codec$c"
        }

    /**
     * Route one decrypted access unit by its stream's `audioType`.
     *
     * atype 5 (`compatibility`) goes to **media**, not voice. It is a media-carrying PCM fallback
     * (`session.rs:889-895`); the macOS host's `isVoice { audioType != 0 }` shortcut sends it to the
     * voice player, which on AAOS would put music on the assistant volume group and permanently duck it.
     */
    private fun route(
        scid: Long,
        au: ByteArray,
    ) {
        val f = formats[scid]
        if (f == null) {
            // Without SEAM_FORMAT there is no rate/channel/atype to route or configure with, and
            // guessing is how streams end up on the wrong AAOS volume group.
            log.w("audio AU for scid=$scid with no SEAM_FORMAT yet — dropped")
            return
        }
        if (SeamCrypto.isMediaAudioType(f.atype)) {
            when (f.codec) {
                SeamCrypto.CODEC_AAC_LC -> mediaPipe.write(adts(au, f.rate, f.channels))
                else ->
                    warnOnce(
                        "media-${f.codec}",
                        "media stream scid=$scid is ${codecName(f.codec)}; AacPlayer consumes ADTS AAC-LC " +
                            "only — dropping. Wireless CarPlay negotiates AAC-LC, so this means the pushed " +
                            "audio config selected something else.",
                    )
            }
        } else {
            voicePipe.write(voiceTagged(au, f))
        }
    }

    private val warned = HashSet<String>()

    private fun warnOnce(
        key: String,
        msg: String,
    ) {
        if (warned.add(key)) log.w(msg)
    }

    /** `[u32 BE rate][u16 BE ch][u8 atype][u32 BE len][AU]` — VoiceRouter's 11-byte tag. */
    private fun voiceTagged(
        au: ByteArray,
        f: Format,
    ): ByteArray {
        val out = ByteArray(11 + au.size)
        out[0] = ((f.rate ushr 24) and 0xFF).toByte()
        out[1] = ((f.rate ushr 16) and 0xFF).toByte()
        out[2] = ((f.rate ushr 8) and 0xFF).toByte()
        out[3] = (f.rate and 0xFF).toByte()
        out[4] = ((f.channels ushr 8) and 0xFF).toByte()
        out[5] = (f.channels and 0xFF).toByte()
        out[6] = (f.atype and 0xFF).toByte()
        out[7] = ((au.size ushr 24) and 0xFF).toByte()
        out[8] = ((au.size ushr 16) and 0xFF).toByte()
        out[9] = ((au.size ushr 8) and 0xFF).toByte()
        out[10] = (au.size and 0xFF).toByte()
        System.arraycopy(au, 0, out, 11, au.size)
        return out
    }

    /**
     * Prepend a 7-byte ADTS header (no CRC) to a raw AAC-LC access unit.
     *
     * `AacPlayer` parses ADTS to find frame boundaries and then strips the header again before feeding
     * MediaCodec, because it configures the codec from csd-0. Wrapping here rather than reworking the
     * player keeps that proven, on-hardware path byte-identical.
     */
    private fun adts(
        au: ByteArray,
        rate: Int,
        channels: Int,
    ): ByteArray {
        val fi = SF_INDEX.indexOf(rate).let { if (it < 0) 3 else it } // default 48 kHz
        val ch = channels.coerceIn(1, 7)
        val total = au.size + 7
        val out = ByteArray(total)
        out[0] = 0xFF.toByte()
        out[1] = 0xF1.toByte() // MPEG-4, layer 0, no CRC
        out[2] = (((1 and 0x03) shl 6) or ((fi and 0x0F) shl 2) or ((ch shr 2) and 0x01)).toByte()
        out[3] = (((ch and 0x03) shl 6) or ((total shr 11) and 0x03)).toByte()
        out[4] = ((total shr 3) and 0xFF).toByte()
        out[5] = (((total and 0x07) shl 5) or 0x1F).toByte()
        out[6] = 0xFC.toByte()
        System.arraycopy(au, 0, out, 7, au.size)
        return out
    }
}
