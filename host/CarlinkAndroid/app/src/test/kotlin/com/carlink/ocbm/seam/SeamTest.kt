package com.carlink.ocbm.seam

import com.carlink.logging.ProbeLog
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.ByteArrayOutputStream
import javax.crypto.Cipher
import javax.crypto.spec.IvParameterSpec
import javax.crypto.spec.SecretKeySpec

/**
 * Headless tests for the forward-encrypted A/V seam.
 *
 * These run on the JVM with no adapter, no emulator and no head unit — the point being that the
 * riskiest part of the port (exact nonce/AAD derivation, seam resync, the AVCC→Annex-B rewrite, and
 * the routing trap around `audioType` 5) is settled before any hardware time is spent. The sealing
 * helpers below deliberately re-derive the nonce and AAD from the wire spec rather than calling into
 * [SeamCrypto], so a mistake in the production derivation cannot cancel itself out in the test.
 */
class SeamTest {
    private val log = ProbeLog.silent()

    // ---- helpers ---------------------------------------------------------------------------------

    private fun seal(
        key: ByteArray,
        nonce: ByteArray,
        aad: ByteArray,
        plain: ByteArray,
    ): ByteArray {
        val c = Cipher.getInstance("ChaCha20-Poly1305")
        c.init(Cipher.ENCRYPT_MODE, SecretKeySpec(key, "ChaCha20"), IvParameterSpec(nonce))
        c.updateAAD(aad)
        return c.doFinal(plain) // ct || tag16
    }

    private fun be32(v: Int) =
        byteArrayOf(
            ((v ushr 24) and 0xFF).toByte(),
            ((v ushr 16) and 0xFF).toByte(),
            ((v ushr 8) and 0xFF).toByte(),
            (v and 0xFF).toByte(),
        )

    /**
     * The video nonce, re-derived HERE from the wire spec: 4 zero bytes then `seq` little-endian.
     *
     * Deliberately NOT `SeamCrypto.videoNonce`. Sealing a fixture with the same function the code
     * under test opens it with makes the test pass for any derivation, right or wrong — if the
     * production nonce put `seq` big-endian, or at offset 0, seal and open would share the error, the
     * suite would stay green, and every frame from the real box would fail to authenticate. That is
     * the single hardest failure to diagnose on hardware, so it is the one that must be pinned
     * independently.
     */
    private fun independentVideoNonce(seq: Long): ByteArray = ByteArray(4) + le64(seq)

    private fun le64(v: Long): ByteArray {
        val b = ByteArray(8)
        var x = v
        for (i in 0 until 8) {
            b[i] = (x and 0xFF).toByte()
            x = x ushr 8
        }
        return b
    }

    /** Wrap a video seam message body as `[u32 BE len][SEAV][marker][payload]`. */
    private fun videoMsg(
        marker: Int,
        payload: ByteArray,
    ): ByteArray {
        val body = byteArrayOf(marker.toByte()) + payload
        return be32(4 + body.size) + SeamCrypto.SEAM_MAGIC + body
    }

    /**
     * Wrap an audio seam message in the LEGACY (pre-2026-09-03) framing `[u32 BE len][marker][payload]`.
     * Kept as the default in these tests on purpose: it is the compatibility path, and a box build
     * older than the magic must keep working.
     */
    private fun audioMsg(
        marker: Int,
        payload: ByteArray,
    ): ByteArray {
        val body = byteArrayOf(marker.toByte()) + payload
        return be32(body.size) + body
    }

    /** The CURRENT audio wire: `[u32 BE len][SEAM_MAGIC][marker][payload]`, same as the video seam. */
    private fun audioMsgV3(
        marker: Int,
        payload: ByteArray,
    ): ByteArray {
        val body = byteArrayOf(marker.toByte()) + payload
        return be32(4 + body.size) + SeamCrypto.SEAM_MAGIC + body
    }

    private fun videoHeader(
        opcode: Int,
        keyframe: Boolean,
    ): ByteArray {
        val h = ByteArray(SeamCrypto.HDR_LEN)
        h[4] = opcode.toByte()
        if (keyframe) h[5] = SeamCrypto.KEYFRAME_BIT.toByte()
        return h
    }

    /** Read exactly one `[u32 BE len][payload]` message from a pipe, or null at EOF. */
    private fun readMsg(pipe: SeamPipe): ByteArray? {
        val hdr = ByteArray(4)
        if (!readFully(pipe, hdr)) return null
        val len =
            ((hdr[0].toInt() and 0xFF) shl 24) or ((hdr[1].toInt() and 0xFF) shl 16) or
                ((hdr[2].toInt() and 0xFF) shl 8) or (hdr[3].toInt() and 0xFF)
        val body = ByteArray(len)
        return if (readFully(pipe, body)) body else null
    }

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

    private fun avccAu(vararg nals: ByteArray): ByteArray {
        val o = ByteArrayOutputStream()
        for (n in nals) {
            o.write(be32(n.size))
            o.write(n)
        }
        return o.toByteArray()
    }

    // ---- video -----------------------------------------------------------------------------------

    @Test
    fun `encrypted video frame decrypts and converts AVCC to Annex-B`() {
        val pipe = SeamPipe(1 shl 20)
        var gaps = 0
        val seam = VideoSeam(pipe, log) { gaps++ }

        val key = ByteArray(32) { it.toByte() }
        seam.feed(videoMsg(SeamCrypto.MARK_KEY, key + le64(7L)))

        // Two NALs in one access unit, so the length-prefix rewrite is exercised more than once.
        val nal1 = byteArrayOf(0x26, 0x01, 0x02, 0x03)
        val nal2 = byteArrayOf(0x02, 0x01, 0x09)
        val au = avccAu(nal1, nal2)

        val seq = 0L
        val hdr = videoHeader(SeamCrypto.OPCODE_FRAME, keyframe = true)
        val body = seal(key, independentVideoNonce(seq), hdr, au)
        seam.feed(videoMsg(SeamCrypto.MARK_FRAME, le64(seq) + hdr + body))

        val out = readMsg(pipe)!!
        val expected = byteArrayOf(0, 0, 0, 1) + nal1 + byteArrayOf(0, 0, 0, 1) + nal2
        assertArrayEquals(expected, out)
        assertEquals(1L, seam.decryptOk.get())
        assertEquals(0L, seam.decryptFail.get())
        assertEquals(0, gaps)
    }

    @Test
    fun `sequence gap fires the keyframe request and the counter resyncs to the wire`() {
        val pipe = SeamPipe(1 shl 20)
        var gaps = 0
        val seam = VideoSeam(pipe, log) { gaps++ }
        val key = ByteArray(32) { 0x11 }
        seam.feed(videoMsg(SeamCrypto.MARK_KEY, key + le64(1L)))

        fun frameAt(seq: Long) {
            val hdr = videoHeader(SeamCrypto.OPCODE_FRAME, keyframe = false)
            val au = avccAu(byteArrayOf(0x02, 0x11))
            val body = seal(key, independentVideoNonce(seq), hdr, au)
            seam.feed(videoMsg(SeamCrypto.MARK_FRAME, le64(seq) + hdr + body))
        }

        frameAt(0)
        frameAt(1)
        assertEquals(0, gaps)
        frameAt(9) // seq jumped — frames were lost
        assertEquals(1, gaps)
        // Resynced to the wire's absolute seq, so the next in-order frame is NOT a second gap. If the
        // counter were incremented locally instead, every subsequent frame would look like a gap.
        frameAt(10)
        assertEquals(1, gaps)
        assertEquals(4L, seam.decryptOk.get())
    }

    @Test
    fun `decrypt failure still advances seq so the next good frame is not a phantom gap`() {
        val pipe = SeamPipe(1 shl 20)
        var gaps = 0
        val seam = VideoSeam(pipe, log) { gaps++ }
        val key = ByteArray(32) { 0x22 }
        seam.feed(videoMsg(SeamCrypto.MARK_KEY, key + le64(1L)))

        val hdr = videoHeader(SeamCrypto.OPCODE_FRAME, keyframe = false)
        // Ciphertext that will not authenticate.
        val junk = ByteArray(64) { 0x5A }
        seam.feed(videoMsg(SeamCrypto.MARK_FRAME, le64(0L) + hdr + junk))
        assertEquals(1L, seam.decryptFail.get())
        assertEquals(1, gaps) // the failure itself asked for a keyframe

        val au = avccAu(byteArrayOf(0x26, 0x01))
        val body = seal(key, independentVideoNonce(1L), hdr, au)
        seam.feed(videoMsg(SeamCrypto.MARK_FRAME, le64(1L) + hdr + body))
        assertEquals(1, gaps) // seq 0 -> 1 is contiguous: no second gap
        assertEquals(1L, seam.decryptOk.get())
    }

    @Test
    fun `bare avcC config emits Annex-B parameter sets`() {
        val pipe = SeamPipe(1 shl 20)
        val seam = VideoSeam(pipe, log) { }
        val sps = byteArrayOf(0x67, 0x42, 0x00, 0x1E)
        val pps = byteArrayOf(0x68, 0xCE.toByte(), 0x3C, 0x80.toByte())
        val rec =
            ByteArrayOutputStream()
                .apply {
                    write(byteArrayOf(1, 0x42, 0x00, 0x1E, 0xFF.toByte())) // ver, profile, compat, level, nls=4
                    write(0xE1) // numSPS = 1
                    write(byteArrayOf(0, sps.size.toByte()))
                    write(sps)
                    write(1) // numPPS = 1
                    write(byteArrayOf(0, pps.size.toByte()))
                    write(pps)
                }.toByteArray()

        val hdr = videoHeader(SeamCrypto.OPCODE_CONFIG, keyframe = false)
        seam.feed(videoMsg(SeamCrypto.MARK_FRAME, le64(0L) + hdr + rec))

        val out = readMsg(pipe)!!
        assertArrayEquals(byteArrayOf(0, 0, 0, 1) + sps + byteArrayOf(0, 0, 0, 1) + pps, out)
    }

    @Test
    fun `hvcC wrapped in an hvc1 sample entry is unwrapped`() {
        val pipe = SeamPipe(1 shl 20)
        val seam = VideoSeam(pipe, log) { }
        val vps = byteArrayOf(0x40, 0x01, 0x0C)
        val sps = byteArrayOf(0x42, 0x01, 0x01)
        val pps = byteArrayOf(0x44, 0x01, 0xC0.toByte())
        val hvcc = buildHvcC(vps, sps, pps)
        // A minimal `hvc1` VisualSampleEntry carrying the hvcC atom — the shape iOS actually sends.
        val atom = be32(8 + hvcc.size) + "hvcC".toByteArray() + hvcc
        val entry = be32(8 + 70 + atom.size) + "hvc1".toByteArray() + ByteArray(70) + atom

        val hdr = videoHeader(SeamCrypto.OPCODE_CONFIG, keyframe = false)
        seam.feed(videoMsg(SeamCrypto.MARK_FRAME, le64(0L) + hdr + entry))

        val out = readMsg(pipe)!!
        val expected =
            byteArrayOf(0, 0, 0, 1) + vps + byteArrayOf(0, 0, 0, 1) + sps +
                byteArrayOf(0, 0, 0, 1) + pps
        assertArrayEquals(expected, out)
    }

    /**
     * The case `OCBMAVBridge.swift:80` misses: an `avc1` sample entry. On a wireless session that fell
     * back to H.264 the macOS bridge would find neither `hvc1` nor `hev1`, drop the config, and never
     * decode a frame.
     */
    @Test
    fun `avcC wrapped in an avc1 sample entry is unwrapped`() {
        val pipe = SeamPipe(1 shl 20)
        val seam = VideoSeam(pipe, log) { }
        val sps = byteArrayOf(0x67, 0x42, 0x00, 0x1E)
        val pps = byteArrayOf(0x68, 0xCE.toByte())
        val avcc =
            ByteArrayOutputStream()
                .apply {
                    write(byteArrayOf(1, 0x42, 0x00, 0x1E, 0xFF.toByte()))
                    write(0xE1)
                    write(byteArrayOf(0, sps.size.toByte()))
                    write(sps)
                    write(1)
                    write(byteArrayOf(0, pps.size.toByte()))
                    write(pps)
                }.toByteArray()
        val atom = be32(8 + avcc.size) + "avcC".toByteArray() + avcc
        val entry = be32(8 + 70 + atom.size) + "avc1".toByteArray() + ByteArray(70) + atom

        val hdr = videoHeader(SeamCrypto.OPCODE_CONFIG, keyframe = false)
        seam.feed(videoMsg(SeamCrypto.MARK_FRAME, le64(0L) + hdr + entry))

        val out = readMsg(pipe)!!
        assertArrayEquals(byteArrayOf(0, 0, 0, 1) + sps + byteArrayOf(0, 0, 0, 1) + pps, out)
    }

    private fun buildHvcC(
        vps: ByteArray,
        sps: ByteArray,
        pps: ByteArray,
    ): ByteArray {
        val o = ByteArrayOutputStream()
        o.write(ByteArray(21).also { it[0] = 1 }) // through general_level_idc
        o.write(0xFF) // byte 21: lengthSizeMinusOne = 3 -> 4
        o.write(3) // numOfArrays
        for ((type, nal) in listOf(32 to vps, 33 to sps, 34 to pps)) {
            o.write(type) // array_completeness | NAL type
            o.write(byteArrayOf(0, 1)) // numNalus = 1
            o.write(byteArrayOf(0, nal.size.toByte()))
            o.write(nal)
        }
        return o.toByteArray()
    }

    @Test
    fun `seam resyncs past junk and survives a message split across feeds`() {
        val pipe = SeamPipe(1 shl 20)
        val seam = VideoSeam(pipe, log) { }
        val key = ByteArray(32) { 0x33 }

        // Junk that contains neither a valid length nor a magic, then a real key message split in two.
        seam.feed(byteArrayOf(0x11, 0x22, 0x33, 0x44, 0x55))
        val msg = videoMsg(SeamCrypto.MARK_KEY, key + le64(1L))
        seam.feed(msg.copyOfRange(0, 20))
        seam.feed(msg.copyOfRange(20, msg.size))

        val hdr = videoHeader(SeamCrypto.OPCODE_FRAME, keyframe = true)
        val au = avccAu(byteArrayOf(0x26, 0x01))
        val body = seal(key, independentVideoNonce(0L), hdr, au)
        seam.feed(videoMsg(SeamCrypto.MARK_FRAME, le64(0L) + hdr + body))

        assertEquals(1L, seam.decryptOk.get())
        assertArrayEquals(byteArrayOf(0, 0, 0, 1, 0x26, 0x01), readMsg(pipe)!!)
    }

    @Test
    fun `an implausible declared length resyncs instead of throwing`() {
        val pipe = SeamPipe(1 shl 20)
        val seam = VideoSeam(pipe, log) { }
        val key = ByteArray(32) { 0x44 }
        // 0x7FFFFFFF exceeds MAX_MESSAGE; the reader must byte-resync to the next magic, not blow up
        // trying to allocate it.
        seam.feed(byteArrayOf(0x7F, 0xFF.toByte(), 0xFF.toByte(), 0xFF.toByte()) + SeamCrypto.SEAM_MAGIC)
        seam.feed(videoMsg(SeamCrypto.MARK_KEY, key + le64(1L)))

        val hdr = videoHeader(SeamCrypto.OPCODE_FRAME, keyframe = true)
        val au = avccAu(byteArrayOf(0x26, 0x02))
        seam.feed(
            videoMsg(SeamCrypto.MARK_FRAME, le64(0L) + hdr + seal(key, independentVideoNonce(0L), hdr, au)),
        )
        assertEquals(1L, seam.decryptOk.get())
    }

    // ---- audio -----------------------------------------------------------------------------------

    /** Build an encrypted RTP packet: `[12B hdr][ct][tag16][nonce8]`. */
    private fun rtp(
        key: ByteArray,
        plain: ByteArray,
        nonceTail: ByteArray,
    ): ByteArray {
        val hdr = ByteArray(12) { (0x80 + it).toByte() }
        val nonce = ByteArray(12)
        System.arraycopy(nonceTail, 0, nonce, 4, 8)
        val ctAndTag = seal(key, nonce, hdr.copyOfRange(4, 12), plain)
        return hdr + ctAndTag + nonceTail
    }

    @Test
    fun `media AAC-LC is decrypted and ADTS-wrapped for AacPlayer`() {
        val mediaPipe = SeamPipe(1 shl 20)
        val voicePipe = SeamPipe(1 shl 20)
        val seam = AudioSeam(mediaPipe, voicePipe, log)

        val key = ByteArray(32) { 0x55 }
        val scid = 42L
        seam.feedMedia(audioMsg(SeamCrypto.MARK_KEY, key + le64(scid)))
        seam.feedMedia(
            audioMsg(
                SeamCrypto.MARK_FORMAT,
                le64(scid) + byteArrayOf(SeamCrypto.CODEC_AAC_LC.toByte()) +
                    byteArrayOf(0x80.toByte(), 0xBB.toByte(), 0, 0) + // 48000 LE
                    byteArrayOf(2, 0, SeamCrypto.ATYPE_MEDIA.toByte()),
            ),
        )

        val au = ByteArray(37) { (it * 3).toByte() }
        seam.feedMedia(audioMsg(SeamCrypto.MARK_PKT, le64(scid) + rtp(key, au, ByteArray(8) { 9 })))

        assertEquals(1L, seam.decryptOk.get())
        val out = ByteArray(7 + au.size)
        assertTrue(readFully(mediaPipe, out))
        // ADTS: syncword, then the frame length must include the 7-byte header.
        assertEquals(0xFF, out[0].toInt() and 0xFF)
        assertEquals(0xF1, out[1].toInt() and 0xFF)
        val fi = (out[2].toInt() shr 2) and 0x0F
        assertEquals(3, fi) // 48 kHz
        val ch = ((out[2].toInt() and 0x01) shl 2) or ((out[3].toInt() shr 6) and 0x03)
        assertEquals(2, ch)
        val frameLen =
            ((out[3].toInt() and 0x03) shl 11) or ((out[4].toInt() and 0xFF) shl 3) or
                ((out[5].toInt() shr 5) and 0x07)
        assertEquals(7 + au.size, frameLen)
        assertArrayEquals(au, out.copyOfRange(7, out.size))
    }

    /**
     * The magic-framed audio wire, and the desync it exists to survive.
     *
     * ocbmd replaces a seam producer on a re-SETUP WITHOUT draining the old one, so the seam can be
     * holding a partial message when the new producer's bytes arrive. Here the dead producer left a
     * `SEAM_PKT` whose length prefix promises far more than arrived; the parser must reject it on the
     * length-plausibility check, scan forward to the next `SEAM_MAGIC`, and pick the new stream up at
     * the key. Without the magic the new `SEAM_KEY` is swallowed as that message's tail and the lane
     * never decrypts again (device-proven 2026-09-02: bogus keys, a 1469658167Hz format, silence).
     */
    @Test
    fun `magic-framed audio resyncs past a dead producer's partial message`() {
        val mediaPipe = SeamPipe(1 shl 20)
        val voicePipe = SeamPipe(1 shl 20)
        val seam = AudioSeam(mediaPipe, voicePipe, log)

        val key = ByteArray(32) { 0x55 }
        val scid = 42L
        val au = ByteArray(37) { (it * 3).toByte() }

        // A magic-framed SEAM_KEY latches the framing, then a torn message: valid magic, impossible
        // length (SEAM_KEY is always 45), followed by the real stream.
        seam.feedMedia(audioMsgV3(SeamCrypto.MARK_KEY, key + le64(scid)))
        val torn = be32(100) + SeamCrypto.SEAM_MAGIC + byteArrayOf(SeamCrypto.MARK_KEY.toByte()) + ByteArray(20)
        seam.feedMedia(
            torn +
                audioMsgV3(
                    SeamCrypto.MARK_FORMAT,
                    le64(scid) + byteArrayOf(SeamCrypto.CODEC_AAC_LC.toByte()) +
                        byteArrayOf(0x80.toByte(), 0xBB.toByte(), 0, 0) + // 48000 LE
                        byteArrayOf(2, 0, SeamCrypto.ATYPE_MEDIA.toByte()),
                ) +
                audioMsgV3(SeamCrypto.MARK_PKT, le64(scid) + rtp(key, au, ByteArray(8) { 9 })),
        )

        assertEquals(1L, seam.decryptOk.get())
        assertEquals(0L, seam.decryptFail.get())
        assertEquals(0L, seam.unkeyed.get())
        val out = ByteArray(7 + au.size)
        assertTrue(readFully(mediaPipe, out))
        assertArrayEquals(au, out.copyOfRange(7, out.size))
    }

    @Test
    fun `voice audio gets VoiceRouter's 11-byte tag`() {
        val mediaPipe = SeamPipe(1 shl 20)
        val voicePipe = SeamPipe(1 shl 20)
        val seam = AudioSeam(mediaPipe, voicePipe, log)

        val key = ByteArray(32) { 0x66 }
        val scid = 7L
        seam.feedVoice(audioMsg(SeamCrypto.MARK_KEY, key + le64(scid)))
        seam.feedVoice(
            audioMsg(
                SeamCrypto.MARK_FORMAT,
                le64(scid) + byteArrayOf(SeamCrypto.CODEC_AAC_ELD.toByte()) +
                    byteArrayOf(0x80.toByte(), 0x3E, 0, 0) + // 16000 LE
                    byteArrayOf(1, 0, SeamCrypto.ATYPE_TELEPHONY.toByte()),
            ),
        )
        val au = ByteArray(20) { it.toByte() }
        seam.feedVoice(audioMsg(SeamCrypto.MARK_PKT, le64(scid) + rtp(key, au, ByteArray(8) { 3 })))

        val out = ByteArray(11 + au.size)
        assertTrue(readFully(voicePipe, out))
        val rate =
            ((out[0].toInt() and 0xFF) shl 24) or ((out[1].toInt() and 0xFF) shl 16) or
                ((out[2].toInt() and 0xFF) shl 8) or (out[3].toInt() and 0xFF)
        assertEquals(16000, rate)
        assertEquals(1, ((out[4].toInt() and 0xFF) shl 8) or (out[5].toInt() and 0xFF))
        assertEquals(SeamCrypto.ATYPE_TELEPHONY, out[6].toInt() and 0xFF)
        val len =
            ((out[7].toInt() and 0xFF) shl 24) or ((out[8].toInt() and 0xFF) shl 16) or
                ((out[9].toInt() and 0xFF) shl 8) or (out[10].toInt() and 0xFF)
        assertEquals(au.size, len)
        assertArrayEquals(au, out.copyOfRange(11, out.size))
    }

    /**
     * `audioType` 5 is `compatibility` — a **media**-carrying PCM fallback. The macOS host's
     * `isVoice { audioType != 0 }` shortcut would route it to the voice player, which on AAOS puts
     * music on the assistant volume group and self-ducks it for the session.
     */
    @Test
    fun `audioType 5 compatibility routes to media, not voice`() {
        val mediaPipe = SeamPipe(1 shl 20)
        val voicePipe = SeamPipe(1 shl 20)
        val seam = AudioSeam(mediaPipe, voicePipe, log)
        val key = ByteArray(32) { 0x77 }
        val scid = 3L
        seam.feedVoice(audioMsg(SeamCrypto.MARK_KEY, key + le64(scid)))
        seam.feedVoice(
            audioMsg(
                SeamCrypto.MARK_FORMAT,
                le64(scid) + byteArrayOf(SeamCrypto.CODEC_AAC_LC.toByte()) +
                    byteArrayOf(0x80.toByte(), 0xBB.toByte(), 0, 0) +
                    byteArrayOf(2, 0, SeamCrypto.ATYPE_COMPATIBILITY.toByte()),
            ),
        )
        val au = ByteArray(16) { 1 }
        seam.feedVoice(audioMsg(SeamCrypto.MARK_PKT, le64(scid) + rtp(key, au, ByteArray(8) { 5 })))

        assertTrue(mediaPipe.residentBytes() > 0)
        assertEquals(0L, voicePipe.residentBytes())
    }

    @Test
    fun `an access unit with no SEAM_FORMAT yet is dropped rather than guessed`() {
        val mediaPipe = SeamPipe(1 shl 20)
        val voicePipe = SeamPipe(1 shl 20)
        val seam = AudioSeam(mediaPipe, voicePipe, log)
        val key = ByteArray(32) { 0x11 }
        val scid = 1L
        seam.feedMedia(audioMsg(SeamCrypto.MARK_KEY, key + le64(scid)))
        seam.feedMedia(audioMsg(SeamCrypto.MARK_PKT, le64(scid) + rtp(key, ByteArray(8), ByteArray(8))))

        assertEquals(1L, seam.decryptOk.get())
        assertEquals(0L, mediaPipe.residentBytes())
        assertEquals(0L, voicePipe.residentBytes())
    }

    @Test
    fun `a packet whose key has not arrived is counted, not decrypted`() {
        val mediaPipe = SeamPipe(1 shl 20)
        val voicePipe = SeamPipe(1 shl 20)
        val seam = AudioSeam(mediaPipe, voicePipe, log)
        seam.feedMedia(audioMsg(SeamCrypto.MARK_PKT, le64(99L) + ByteArray(40)))
        assertEquals(1L, seam.unkeyed.get())
        assertEquals(0L, seam.decryptFail.get())
    }

    // ---- pipe ------------------------------------------------------------------------------------

    @Test
    fun `pipe delivers messages in order and reports EOF on close`() {
        val pipe = SeamPipe(1 shl 16)
        pipe.write(byteArrayOf(1, 2, 3))
        pipe.write(byteArrayOf(4, 5))
        val got = ByteArray(5)
        assertTrue(readFully(pipe, got))
        assertArrayEquals(byteArrayOf(1, 2, 3, 4, 5), got)
        pipe.close()
        assertNull(readMsg(pipe))
    }
}
