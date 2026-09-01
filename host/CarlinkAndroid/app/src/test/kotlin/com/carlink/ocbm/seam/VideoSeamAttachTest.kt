package com.carlink.ocbm.seam

import com.carlink.logging.ProbeLog
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Test
import javax.crypto.Cipher
import javax.crypto.spec.IvParameterSpec
import javax.crypto.spec.SecretKeySpec

/**
 * The pipe under a [VideoSeam] is swappable, and that is load-bearing rather than convenience.
 *
 * `HevcRenderer` binds its Surface at construction, so its pipe and consumer thread live and die with
 * the Surface. The ChaCha20 key and the frame sequence counter, however, belong to the OCBM **session**.
 * If a surface change rebuilt the seam, the key would go with it — and the box only re-sends a key when
 * its *own* seam reconnects, which a host-side surface change does not cause. Every frame after the
 * first surface swap would then fail to decrypt, permanently.
 *
 * These tests pin that: the key survives a detach/attach cycle, and frames produced while detached are
 * counted rather than silently lost.
 */
class VideoSeamAttachTest {
    private val log = ProbeLog.silent()

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

    private fun be32(v: Int) =
        byteArrayOf(
            ((v ushr 24) and 0xFF).toByte(),
            ((v ushr 16) and 0xFF).toByte(),
            ((v ushr 8) and 0xFF).toByte(),
            (v and 0xFF).toByte(),
        )

    /** Re-derived from the wire spec, not `SeamCrypto.videoNonce` — see the note in `SeamTest`. */
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

    private fun videoMsg(
        marker: Int,
        payload: ByteArray,
    ): ByteArray {
        val body = byteArrayOf(marker.toByte()) + payload
        return be32(4 + body.size) + SeamCrypto.SEAM_MAGIC + body
    }

    private fun readMsg(pipe: SeamPipe): ByteArray? {
        val hdr = ByteArray(4)
        var off = 0
        while (off < 4) {
            val r = pipe.read(hdr, off, 4 - off)
            if (r <= 0) return null
            off += r
        }
        val len =
            ((hdr[0].toInt() and 0xFF) shl 24) or ((hdr[1].toInt() and 0xFF) shl 16) or
                ((hdr[2].toInt() and 0xFF) shl 8) or (hdr[3].toInt() and 0xFF)
        val body = ByteArray(len)
        off = 0
        while (off < len) {
            val r = pipe.read(body, off, len - off)
            if (r <= 0) return null
            off += r
        }
        return body
    }

    @Test
    fun `the session key and sequence survive a surface swap`() {
        val first = SeamPipe(1 shl 20)
        val seam = VideoSeam(first, log) { }
        val key = ByteArray(32) { 0x5A }
        seam.feed(videoMsg(SeamCrypto.MARK_KEY, key + le64(1L)))

        fun frame(
            seq: Long,
            nal: Byte,
        ): ByteArray {
            val hdr = ByteArray(SeamCrypto.HDR_LEN)
            hdr[4] = SeamCrypto.OPCODE_FRAME.toByte()
            hdr[5] = SeamCrypto.KEYFRAME_BIT.toByte()
            val au = be32(2) + byteArrayOf(0x26, nal)
            return videoMsg(SeamCrypto.MARK_FRAME, le64(seq) + hdr + seal(key, independentVideoNonce(seq), hdr, au))
        }

        seam.feed(frame(0L, 1))
        assertArrayEquals(byteArrayOf(0, 0, 0, 1, 0x26, 1), readMsg(first)!!)

        // Surface goes away: detach and close the old pipe, exactly as a retiring video epoch does.
        seam.attach(null)
        first.close()
        seam.feed(frame(1L, 2))
        assertEquals(1L, seam.framesDroppedNoPipe.get())

        // New surface, new pipe — the SAME seam, so the key and the seq are still live.
        val second = SeamPipe(1 shl 20)
        seam.attach(second)
        seam.feed(frame(2L, 3))

        assertArrayEquals(byteArrayOf(0, 0, 0, 1, 0x26, 3), readMsg(second)!!)
        assertEquals(3L, seam.decryptOk.get()) // all three decrypted; only one had nowhere to go
        assertEquals(0L, seam.decryptFail.get())
        assertEquals(0L, seam.gaps.get()) // and the sequence never broke
    }
}
