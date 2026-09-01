package com.carlink.ocbm

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Golden-byte tests for the OCBM envelope.
 *
 * These exist because round-tripping `frame()` through `parseHeader()` proves nothing: both sides would
 * share any porting error, every test would stay green, and the box would byte-resync through every
 * frame the app sends — a silent no-op rather than a failure. So the expected bytes below are written
 * out by hand from the wire spec in `crates/ocbm-proto/src/lib.rs`, not produced by the code under test.
 *
 * Header (16 bytes, little-endian except where noted):
 * ```
 *   off 0  magic   u32 = 0x4F43424D   -> wire bytes 4D 42 43 4F
 *   off 4  length  u32                 payload bytes only
 *   off 8  channel u16
 *   off 10 flags   u8                  bit0 SOM, bit1 EOM
 *   off 11 hcheck  u8                  XOR of header bytes 0..=10
 *   off 12 seq     u32                 debug only — OUTSIDE the checksum
 * ```
 */
class OcbmFramingTest {
    /** Hand-computed XOR of bytes 0..=10, independent of `Framing.hcheck`. */
    private fun expectedHcheck(vararg b: Int): Byte {
        var a = 0
        for (i in 0 until 11) a = a xor (b[i] and 0xFF)
        return a.toByte()
    }

    @Test
    fun `a heartbeat frame is byte-exact on the wire`() {
        // CT_HEARTBEAT on CH_CTRL, seq 0 — the most common frame in a session.
        val out = Framing.frame(Ocbm.CH_CTRL, Ocbm.F_BOTH, 0, byteArrayOf(Ocbm.CT_HEARTBEAT))

        val hdr =
            intArrayOf(
                0x4D,
                0x42,
                0x43,
                0x4F, // magic, little-endian
                0x01,
                0x00,
                0x00,
                0x00, // length = 1
                0x00,
                0x00, // channel = 0x0000
                0x03, // flags = SOM|EOM
            )
        val expected =
            hdr.map { it.toByte() }.toByteArray() +
                expectedHcheck(*hdr) +
                byteArrayOf(0, 0, 0, 0) + // seq
                byteArrayOf(Ocbm.CT_HEARTBEAT)

        assertEquals(17, out.size)
        assertArrayEquals(expected, out)
    }

    @Test
    fun `channel and seq are little-endian and seq is outside the checksum`() {
        val a = Framing.frame(Ocbm.CH_MGMT, Ocbm.F_BOTH, 0x11223344, ByteArray(0))
        // CH_MGMT = 0x0040 -> 40 00
        assertEquals(0x40.toByte(), a[8])
        assertEquals(0x00.toByte(), a[9])
        // seq 0x11223344 -> 44 33 22 11
        assertArrayEquals(byteArrayOf(0x44, 0x33, 0x22, 0x11), a.copyOfRange(12, 16))

        // Same frame, different seq: only bytes 12..16 may differ — hcheck must NOT move.
        val b = Framing.frame(Ocbm.CH_MGMT, Ocbm.F_BOTH, 0x55667788, ByteArray(0))
        assertArrayEquals(a.copyOfRange(0, 12), b.copyOfRange(0, 12))
        assertEquals("hcheck covers bytes 0..=10 only", a[11], b[11])
    }

    @Test
    fun `a corrupted header byte is rejected by hcheck`() {
        val f = Framing.frame(Ocbm.CH_INPUT, Ocbm.F_BOTH, 7, byteArrayOf(1, 2, 3))
        assertNotNull(Framing.parseHeader(f, 0, f.size))
        // Flip a bit in the channel field; the checksum must catch it.
        f[8] = (f[8].toInt() xor 0x01).toByte()
        assertNull("a header failing hcheck must not parse", Framing.parseHeader(f, 0, f.size))
    }

    @Test
    fun `MAX_PAYLOAD is inclusive and one byte more is refused`() {
        // 65536 is a LEGAL payload; the guard is `> MAX_PAYLOAD`. An off-by-one here would silently
        // drop full-size frames.
        val ok = Framing.frame(Ocbm.CH_MIC, Ocbm.F_BOTH, 0, ByteArray(Ocbm.MAX_PAYLOAD))
        assertEquals(Ocbm.HDR_LEN + Ocbm.MAX_PAYLOAD, ok.size)

        var threw = false
        try {
            Framing.frame(Ocbm.CH_MIC, Ocbm.F_BOTH, 0, ByteArray(Ocbm.MAX_PAYLOAD + 1))
        } catch (_: IllegalArgumentException) {
            threw = true
        }
        assertTrue("an oversize frame must be refused, not emitted", threw)
    }

    // ---- Reassembler -----------------------------------------------------------------------------

    @Test
    fun `two frames in one push both pop, in order`() {
        val r = Reassembler()
        val a = Framing.frame(Ocbm.CH_CTRL, Ocbm.F_BOTH, 0, byteArrayOf(0x11))
        val b = Framing.frame(Ocbm.CH_VIDEO, Ocbm.F_BOTH, 1, byteArrayOf(0x22, 0x33))
        val both = a + b
        r.push(both, both.size)

        val f1 = r.next()!!
        assertEquals(Ocbm.CH_CTRL, f1.channel)
        assertArrayEquals(byteArrayOf(0x11), f1.payload)
        val f2 = r.next()!!
        assertEquals(Ocbm.CH_VIDEO, f2.channel)
        assertArrayEquals(byteArrayOf(0x22, 0x33), f2.payload)
        assertNull(r.next())
        assertEquals(0L, r.resyncBytes)
    }

    @Test
    fun `a frame split across three pushes is reassembled`() {
        val r = Reassembler()
        val f = Framing.frame(Ocbm.CH_METADATA, Ocbm.F_BOTH, 0, ByteArray(300) { it.toByte() })
        for (part in listOf(f.copyOfRange(0, 5), f.copyOfRange(5, 20), f.copyOfRange(20, f.size))) {
            r.push(part, part.size)
        }
        val got = r.next()!!
        assertEquals(Ocbm.CH_METADATA, got.channel)
        assertArrayEquals(ByteArray(300) { it.toByte() }, got.payload)
    }

    @Test
    fun `junk before a frame is resynced past and counted`() {
        val r = Reassembler()
        val junk = byteArrayOf(0x00, 0x4D, 0x42, 0x11, 0x22) // includes a partial magic, deliberately
        val f = Framing.frame(Ocbm.CH_CTRL, Ocbm.F_BOTH, 0, byteArrayOf(0x42))
        val buf = junk + f
        r.push(buf, buf.size)

        val got = r.next()!!
        assertArrayEquals(byteArrayOf(0x42), got.payload)
        assertEquals(junk.size.toLong(), r.resyncBytes)
    }

    /**
     * The trap the implementation calls out: a corrupt length with bit 31 set parses NEGATIVE in
     * Kotlin, so a naive `> MAX_PAYLOAD` test passes it, `total` goes negative, and `copyOfRange`
     * throws — killing the read thread on the first torn stream instead of resyncing. Rust is immune
     * because it widens to usize.
     */
    @Test
    fun `a length with bit 31 set resyncs instead of throwing`() {
        val r = Reassembler()
        val bad = Framing.frame(Ocbm.CH_CTRL, Ocbm.F_BOTH, 0, byteArrayOf(1))
        bad[4] = 0xFF.toByte()
        bad[5] = 0xFF.toByte()
        bad[6] = 0xFF.toByte()
        bad[7] = 0xFF.toByte() // length = 0xFFFFFFFF -> -1 as a signed Int
        bad[11] = Framing.hcheck(bad, 0) // keep the header self-consistent so only length is wrong

        val good = Framing.frame(Ocbm.CH_CTRL, Ocbm.F_BOTH, 1, byteArrayOf(0x7E))
        val buf = bad + good
        r.push(buf, buf.size)

        // Must not throw, and must recover the following good frame.
        val got = r.next()
        assertNotNull("the reassembler must resync past an implausible length", got)
        assertArrayEquals(byteArrayOf(0x7E), got!!.payload)
        assertTrue(r.resyncBytes > 0)
    }

    @Test
    fun `a full-size payload survives a round trip through the reassembler`() {
        val r = Reassembler()
        val payload = ByteArray(Ocbm.MAX_PAYLOAD) { (it and 0xFF).toByte() }
        val f = Framing.frame(Ocbm.CH_VIDEO, Ocbm.F_BOTH, 0, payload)
        r.push(f, f.size)
        val got = r.next()!!
        assertArrayEquals(payload, got.payload)
        assertEquals(0L, r.resyncBytes)
    }
}
