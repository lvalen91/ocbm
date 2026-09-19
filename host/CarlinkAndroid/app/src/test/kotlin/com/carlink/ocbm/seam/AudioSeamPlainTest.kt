package com.carlink.ocbm.seam

import com.carlink.logging.ProbeLog
import com.carlink.telephony.Msbc
import com.carlink.telephony.MsbcUplinkEncoder
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import javax.crypto.Cipher
import javax.crypto.spec.IvParameterSpec
import javax.crypto.spec.SecretKeySpec
import kotlin.math.sin

/**
 * `SEAM_PKT_PLAIN` (0x03) on the voice seam: the HFP call-audio path (`ocbm-proto` lib.rs
 * `SEAM_PKT_PLAIN` / `SEAM_CODEC_MSBC`). Every rule the proto calls load-bearing is pinned here
 * because each fails silently on hardware:
 *  - narrowband is 320 B of 8 kHz mono S16 **little-endian**, passed through untouched;
 *  - an unknown marker is skipped by its length prefix, never treated as a desync;
 *  - mSBC is decoded to 16 kHz PCM, resynchronising on the H2 header — a payload that is not a
 *    whole frame yields nothing now and completes later;
 *  - a stream this host cannot decode, or an mSBC payload that never frames, writes NOTHING to the
 *    voice pipe — the bitstream is never rendered as PCM;
 *  - the AirPlay PCM downlink (`SEAM_PKT`) is big-endian and is swapped, so `VoiceRouter` only
 *    ever sees S16LE under codec 0.
 */
class AudioSeamPlainTest {
    private val log = ProbeLog.silent()

    private fun be32(v: Int) = byteArrayOf(((v ushr 24) and 0xFF).toByte(), ((v ushr 16) and 0xFF).toByte(), ((v ushr 8) and 0xFF).toByte(), (v and 0xFF).toByte())

    private fun le32(v: Int) = byteArrayOf((v and 0xFF).toByte(), ((v ushr 8) and 0xFF).toByte(), ((v ushr 16) and 0xFF).toByte(), ((v ushr 24) and 0xFF).toByte())

    private fun le64(v: Long): ByteArray {
        val b = ByteArray(8)
        var x = v
        for (i in 0 until 8) {
            b[i] = (x and 0xFF).toByte()
            x = x ushr 8
        }
        return b
    }

    /** The current audio wire: `[u32 BE len][SEAV][marker][payload]`, `len` counting the magic. */
    private fun msg(
        marker: Int,
        payload: ByteArray,
    ): ByteArray {
        val body = byteArrayOf(marker.toByte()) + payload
        return be32(4 + body.size) + SeamCrypto.SEAM_MAGIC + body
    }

    /** `[scid 8 LE][codec u8][rate u32 LE][ch u8][bits u8][audio_type u8]` — built by hand from the proto; bits is always 16 here. */
    private fun format(
        scid: Long,
        codec: Int,
        rate: Int,
        ch: Int,
        atype: Int,
    ) = msg(SeamCrypto.MARK_FORMAT, le64(scid) + byteArrayOf(codec.toByte()) + le32(rate) + byteArrayOf(ch.toByte(), 16, atype.toByte()))

    private fun plain(
        scid: Long,
        payload: ByteArray,
    ) = msg(SeamCrypto.MARK_PLAIN, le64(scid) + payload)

    /** The telephony narrowband SEAM_FORMAT exactly as `btd` declares it. */
    private fun nbFormat(scid: Long) = format(scid, SeamCrypto.CODEC_PCM, 8000, 1, SeamCrypto.ATYPE_TELEPHONY)

    /** The wideband one: codec 4, rate/bits describe the DECODED audio. */
    private fun wbFormat(scid: Long) = format(scid, SeamCrypto.CODEC_MSBC, 16000, 1, SeamCrypto.ATYPE_TELEPHONY)

    private fun readFully(
        pipe: SeamPipe,
        dst: ByteArray,
    ): Boolean {
        var off = 0
        while (off < dst.size) {
            val r = pipe.read(dst, off, dst.size - off)
            if (r <= 0) return false
            off += r
        }
        return true
    }

    /** Read one tagged voice message from the pipe. */
    private fun readVoice(pipe: SeamPipe): Pair<VoiceTag.Header, ByteArray> {
        val hdr = ByteArray(VoiceTag.LEN)
        assertTrue(readFully(pipe, hdr))
        val h = VoiceTag.parse(hdr)
        val body = ByteArray(h.len)
        assertTrue(readFully(pipe, body))
        return h to body
    }

    private fun pcmLe8k(n: Int): ByteArray {
        val out = ByteArray(n * 2)
        for (i in 0 until n) {
            val s = (6000.0 * sin(2.0 * Math.PI * 440.0 * i / 8000.0)).toInt()
            out[2 * i] = (s and 0xFF).toByte()
            out[2 * i + 1] = ((s shr 8) and 0xFF).toByte()
        }
        return out
    }

    private fun seam(): Triple<AudioSeam, SeamPipe, SeamPipe> {
        val media = SeamPipe(1 shl 20)
        val voice = SeamPipe(1 shl 20)
        return Triple(AudioSeam(media, voice, log), media, voice)
    }

    // ---- narrowband -----------------------------------------------------------------------------

    @Test
    fun `narrowband PLAIN is tagged PCM 8 kHz mono telephony with the 320 LE bytes untouched`() {
        val (s, media, voice) = seam()
        val scid = 0x1122334455667788L
        s.feedVoice(nbFormat(scid))
        val frame = pcmLe8k(160) // 20 ms
        assertEquals(320, frame.size)
        s.feedVoice(plain(scid, frame))

        val (h, body) = readVoice(voice)
        assertEquals(8000, h.rate)
        assertEquals(1, h.channels)
        assertEquals(SeamCrypto.ATYPE_TELEPHONY, h.atype)
        assertEquals(SeamCrypto.CODEC_PCM, h.codec)
        assertEquals(320, h.len)
        // Byte-identical: no swap. The AirPlay downlink is big-endian, THIS is not.
        assertArrayEquals(frame, body)
        assertEquals(0L, media.residentBytes())
        assertEquals(1L, s.plainIn.get())
    }

    @Test
    fun `a PLAIN message split across feeds is reassembled, not resynced`() {
        val (s, _, voice) = seam()
        val scid = 9L
        s.feedVoice(nbFormat(scid))
        val wire = plain(scid, pcmLe8k(160))
        s.feedVoice(wire.copyOfRange(0, 7)) // inside [len][magic]
        s.feedVoice(wire.copyOfRange(7, 100))
        s.feedVoice(wire.copyOfRange(100, wire.size))
        val (h, body) = readVoice(voice)
        assertEquals(320, h.len)
        assertEquals(320, body.size)
    }

    @Test
    fun `PLAIN before any SEAM_FORMAT is dropped, never guessed`() {
        val (s, media, voice) = seam()
        s.feedVoice(plain(5L, pcmLe8k(160)))
        assertEquals(0L, voice.residentBytes())
        assertEquals(0L, media.residentBytes())
        assertEquals(0L, s.plainIn.get())
    }

    // ---- unknown marker -------------------------------------------------------------------------

    @Test
    fun `an unknown marker is skipped by its length prefix and the next message still parses`() {
        val (s, _, voice) = seam()
        val scid = 3L
        s.feedVoice(nbFormat(scid))
        val junk = ByteArray(77) { 0x53 } // full of 'S' — a magic-lookalike inside, to tempt a resync
        s.feedVoice(msg(0x07, junk) + plain(scid, pcmLe8k(160)))
        val (h, _) = readVoice(voice)
        assertEquals(320, h.len)
        assertEquals(1L, s.skippedUnknown.get())
        assertEquals(1L, s.plainIn.get())
    }

    @Test
    fun `an unknown marker with an absurd length byte-resyncs instead of swallowing the lane`() {
        val (s, _, voice) = seam()
        val scid = 3L
        s.feedVoice(nbFormat(scid))
        // Declares 1 MiB behind marker 0x07 — a false magic in ciphertext looks like this.
        s.feedVoice(be32(1 shl 20) + SeamCrypto.SEAM_MAGIC + byteArrayOf(0x07) + ByteArray(10))
        s.feedVoice(plain(scid, pcmLe8k(160)))
        val (h, _) = readVoice(voice)
        assertEquals(320, h.len)
    }

    // ---- wideband (mSBC) ------------------------------------------------------------------------

    private fun wbPackets(n: Int): List<ByteArray> {
        val up = MsbcUplinkEncoder()
        val pcm = ByteArray(Msbc.PCM_BYTES_PER_FRAME)
        for (i in 0 until Msbc.SAMPLES_PER_FRAME) {
            val v = (5000.0 * sin(2.0 * Math.PI * 800.0 * i / 16000.0)).toInt()
            pcm[2 * i] = (v and 0xFF).toByte()
            pcm[2 * i + 1] = ((v shr 8) and 0xFF).toByte()
        }
        return List(n) { up.packet(pcm)!! }
    }

    @Test
    fun `mSBC PLAIN is decoded to 16 kHz mono S16LE and tagged PCM telephony`() {
        val (s, _, voice) = seam()
        val scid = 21L
        s.feedVoice(wbFormat(scid))
        for (p in wbPackets(4)) s.feedVoice(plain(scid, p))
        repeat(4) {
            val (h, body) = readVoice(voice)
            assertEquals(16000, h.rate)
            assertEquals(1, h.channels)
            assertEquals(SeamCrypto.ATYPE_TELEPHONY, h.atype)
            assertEquals(SeamCrypto.CODEC_PCM, h.codec)
            assertEquals(Msbc.PCM_BYTES_PER_FRAME, body.size)
        }
        assertEquals(0L, voice.residentBytes())
    }

    @Test
    fun `mSBC payloads that are not whole frames yield nothing until the H2 header completes a frame`() {
        val (s, _, voice) = seam()
        val scid = 22L
        s.feedVoice(wbFormat(scid))
        val p = wbPackets(2)
        // A short SCO read, then the remainder coalesced with the next packet — message lengths
        // 30 and 90, neither a frame boundary.
        s.feedVoice(plain(scid, p[0].copyOfRange(0, 30)))
        assertEquals(0L, voice.residentBytes())
        s.feedVoice(plain(scid, p[0].copyOfRange(30, 60) + p[1]))
        val (_, body) = readVoice(voice)
        assertEquals(2 * Msbc.PCM_BYTES_PER_FRAME, body.size)
        assertEquals(0L, voice.residentBytes())
    }

    @Test
    fun `an mSBC-format payload that never frames is dropped, not rendered as PCM`() {
        val (s, media, voice) = seam()
        val scid = 23L
        s.feedVoice(wbFormat(scid))
        // A perfectly plausible-looking 320-byte "PCM" sine. Under a PCM format this would play;
        // under mSBC it is a bitstream with no H2 header and MUST NOT reach the track.
        s.feedVoice(plain(scid, pcmLe8k(160)))
        s.feedVoice(plain(scid, ByteArray(60) { 0x5A }))
        assertEquals(0L, voice.residentBytes())
        assertEquals(0L, media.residentBytes())
    }

    @Test
    fun `the tag carries the DECODED rate even if the SEAM_FORMAT rate lies`() {
        val (s, _, voice) = seam()
        val scid = 24L
        s.feedVoice(format(scid, SeamCrypto.CODEC_MSBC, 8000, 1, SeamCrypto.ATYPE_TELEPHONY))
        s.feedVoice(plain(scid, wbPackets(1)[0]))
        val (h, _) = readVoice(voice)
        assertEquals(16000, h.rate)
    }

    @Test
    fun `a codec change on the same scid drops the mSBC lane and the PCM path takes over cleanly`() {
        val (s, _, voice) = seam()
        val scid = 25L
        s.feedVoice(wbFormat(scid))
        s.feedVoice(plain(scid, wbPackets(1)[0]))
        readVoice(voice)
        s.feedVoice(nbFormat(scid))
        s.feedVoice(plain(scid, pcmLe8k(160)))
        val (h, body) = readVoice(voice)
        assertEquals(8000, h.rate)
        assertEquals(320, body.size)
    }

    // ---- unsupported codecs ---------------------------------------------------------------------

    @Test
    fun `a voice stream in a codec this host cannot decode writes nothing`() {
        val (s, media, voice) = seam()
        val scid = 30L
        s.feedVoice(format(scid, SeamCrypto.CODEC_OPUS, 16000, 1, SeamCrypto.ATYPE_TELEPHONY))
        s.feedVoice(plain(scid, ByteArray(100) { 1 }))
        assertEquals(0L, voice.residentBytes())
        assertEquals(0L, media.residentBytes())
    }

    @Test
    fun `PLAIN on a media-typed stream is dropped, never ADTS-wrapped`() {
        val (s, media, voice) = seam()
        val scid = 31L
        s.feedVoice(format(scid, SeamCrypto.CODEC_PCM, 48000, 2, SeamCrypto.ATYPE_MEDIA))
        s.feedVoice(plain(scid, ByteArray(320)))
        assertEquals(0L, media.residentBytes())
        assertEquals(0L, voice.residentBytes())
    }

    // ---- AirPlay PCM (big-endian) via SEAM_PKT -------------------------------------------------

    private fun seal(
        key: ByteArray,
        nonce: ByteArray,
        aad: ByteArray,
        plain: ByteArray,
    ): ByteArray {
        val c = Cipher.getInstance("ChaCha20-Poly1305")
        c.init(Cipher.ENCRYPT_MODE, SecretKeySpec(key, "ChaCha20"), IvParameterSpec(nonce))
        c.updateAAD(aad)
        return c.doFinal(plain)
    }

    private fun rtp(
        key: ByteArray,
        plain: ByteArray,
        nonceTail: ByteArray,
    ): ByteArray {
        val hdr = ByteArray(12) { (0x80 + it).toByte() }
        val nonce = ByteArray(12)
        System.arraycopy(nonceTail, 0, nonce, 4, 8)
        return hdr + seal(key, nonce, hdr.copyOfRange(4, 12), plain) + nonceTail
    }

    @Test
    fun `AirPlay PCM voice via SEAM_PKT is byte-swapped to little-endian before tagging`() {
        val (s, _, voice) = seam()
        val key = ByteArray(32) { 0x42 }
        val scid = 40L
        s.feedVoice(msg(SeamCrypto.MARK_KEY, key + le64(scid)))
        s.feedVoice(format(scid, SeamCrypto.CODEC_PCM, 16000, 1, SeamCrypto.ATYPE_TELEPHONY))
        val be = byteArrayOf(0x12, 0x34, 0x7F, 0xFF.toByte(), 0x80.toByte(), 0x00)
        s.feedVoice(msg(SeamCrypto.MARK_PKT, le64(scid) + rtp(key, be, ByteArray(8) { 1 })))
        val (h, body) = readVoice(voice)
        assertEquals(SeamCrypto.CODEC_PCM, h.codec)
        assertArrayEquals(byteArrayOf(0x34, 0x12, 0xFF.toByte(), 0x7F, 0x00, 0x80.toByte()), body)
        assertEquals(1L, s.decryptOk.get())
    }

    @Test
    fun `AAC-ELD voice via SEAM_PKT keeps its codec in the tag`() {
        val (s, _, voice) = seam()
        val key = ByteArray(32) { 0x43 }
        val scid = 41L
        s.feedVoice(msg(SeamCrypto.MARK_KEY, key + le64(scid)))
        s.feedVoice(format(scid, SeamCrypto.CODEC_AAC_ELD, 16000, 1, SeamCrypto.ATYPE_SPEECH_RECOGNITION))
        val au = ByteArray(20) { it.toByte() }
        s.feedVoice(msg(SeamCrypto.MARK_PKT, le64(scid) + rtp(key, au, ByteArray(8) { 2 })))
        val (h, body) = readVoice(voice)
        assertEquals(SeamCrypto.CODEC_AAC_ELD, h.codec)
        assertEquals(SeamCrypto.ATYPE_SPEECH_RECOGNITION, h.atype)
        assertArrayEquals(au, body) // compressed: untouched
    }

    // ---- the tag itself -------------------------------------------------------------------------

    @Test
    fun `VoiceTag round-trips every field`() {
        val au = ByteArray(5) { (it + 1).toByte() }
        val w = VoiceTag.wrap(au, VoiceTag.Fmt(48000, 2, SeamCrypto.ATYPE_ALERT, SeamCrypto.CODEC_AAC_ELD))
        assertEquals(VoiceTag.LEN + 5, w.size)
        val h = VoiceTag.parse(w)
        assertEquals(48000, h.rate)
        assertEquals(2, h.channels)
        assertEquals(SeamCrypto.ATYPE_ALERT, h.atype)
        assertEquals(SeamCrypto.CODEC_AAC_ELD, h.codec)
        assertEquals(5, h.len)
        assertArrayEquals(au, w.copyOfRange(VoiceTag.LEN, w.size))
    }
}
