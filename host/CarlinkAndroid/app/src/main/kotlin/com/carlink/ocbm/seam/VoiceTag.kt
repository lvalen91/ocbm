package com.carlink.ocbm.seam

/**
 * The per-access-unit tag `AudioSeam` writes into the voice pipe and `VoiceRouter` reads back:
 * ```
 *   [rate u32 BE][ch u16 BE][atype u8][codec u8][len u32 BE][AU]
 * ```
 * 12 bytes. This is the box's legacy `forward.rs` 11-byte voice tag with ONE byte added — the
 * codec — so the consumer can branch on it instead of assuming AAC-ELD.
 *
 * `codec` is a `SeamCrypto.CODEC_*` value with one seam-side normalisation: [SeamCrypto.CODEC_PCM]
 * in this tag ALWAYS means host-order S16LE. `AudioSeam` byte-swaps the big-endian AirPlay PCM
 * downlink, passes the little-endian HFP `SEAM_PKT_PLAIN` payload through untouched, and decodes
 * mSBC to PCM before tagging — so `VoiceRouter` never sees an endianness question or a compressed
 * bitstream under codec 0.
 *
 * The change is deliberately NOT byte-compatible with the box's own legacy `:9003` tag: nothing in
 * `:app` consumes that socket any more (the seam is the only producer), and a silent 11-vs-12
 * mismatch would be far worse than a loud one — `VoiceRouter.consume` rejects an implausible rate
 * on the first message.
 */
object VoiceTag {
    const val LEN: Int = 12

    /** The stream description half of the tag. */
    class Fmt(
        val rate: Int,
        val channels: Int,
        val atype: Int,
        val codec: Int,
    )

    class Header(
        val fmt: Fmt,
        val len: Int,
    ) {
        val rate: Int get() = fmt.rate
        val channels: Int get() = fmt.channels
        val atype: Int get() = fmt.atype
        val codec: Int get() = fmt.codec
    }

    fun wrap(
        au: ByteArray,
        off: Int,
        len: Int,
        f: Fmt,
    ): ByteArray {
        val out = ByteArray(LEN + len)
        out[0] = ((f.rate ushr 24) and 0xFF).toByte()
        out[1] = ((f.rate ushr 16) and 0xFF).toByte()
        out[2] = ((f.rate ushr 8) and 0xFF).toByte()
        out[3] = (f.rate and 0xFF).toByte()
        out[4] = ((f.channels ushr 8) and 0xFF).toByte()
        out[5] = (f.channels and 0xFF).toByte()
        out[6] = (f.atype and 0xFF).toByte()
        out[7] = (f.codec and 0xFF).toByte()
        out[8] = ((len ushr 24) and 0xFF).toByte()
        out[9] = ((len ushr 16) and 0xFF).toByte()
        out[10] = ((len ushr 8) and 0xFF).toByte()
        out[11] = (len and 0xFF).toByte()
        System.arraycopy(au, off, out, LEN, len)
        return out
    }

    fun wrap(
        au: ByteArray,
        f: Fmt,
    ): ByteArray = wrap(au, 0, au.size, f)

    fun parse(hdr: ByteArray): Header =
        Header(
            Fmt(
                rate = be32(hdr, 0),
                channels = ((hdr[4].toInt() and 0xFF) shl 8) or (hdr[5].toInt() and 0xFF),
                atype = hdr[6].toInt() and 0xFF,
                codec = hdr[7].toInt() and 0xFF,
            ),
            len = be32(hdr, 8),
        )

    private fun be32(
        b: ByteArray,
        off: Int,
    ): Int =
        ((b[off].toInt() and 0xFF) shl 24) or ((b[off + 1].toInt() and 0xFF) shl 16) or
            ((b[off + 2].toInt() and 0xFF) shl 8) or (b[off + 3].toInt() and 0xFF)
}
