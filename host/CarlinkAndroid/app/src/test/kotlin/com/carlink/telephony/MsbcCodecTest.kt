package com.carlink.telephony

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlin.math.abs
import kotlin.math.log10
import kotlin.math.sin

/**
 * The mSBC codec, pinned the same way the macOS original is (`host/MacHost/tests/main.swift`,
 * "mSBC" sections): from the filterbank tables outwards, ending on a reference bitstream this
 * encoder must reproduce BYTE-FOR-BYTE and a PCM vector produced by an INDEPENDENT decoder (a
 * transcription of ffmpeg's fixed-point `sbc_synthesize_eight`) that this decoder must match to a
 * few LSB. Get a coefficient sign wrong and the only symptom on hardware is "the call sounds like
 * noise", with nothing to compare against — hence the vectors.
 */
class MsbcCodecTest {
    /** Swift's `.rounded()` (half away from zero), so the source signal matches the reference generator exactly. */
    private fun roundAway(x: Double): Int = if (x >= 0) Math.floor(x + 0.5).toInt() else -Math.floor(-x + 0.5).toInt()

    private fun tone(
        n: Int,
        hz: Double,
        amp: Double,
    ): ShortArray = ShortArray(n) { (amp * sin(2.0 * Math.PI * hz * it / 16000.0)).toInt().toShort() }

    // ---- tables ---------------------------------------------------------------------------------

    @Test
    fun `prototype is 80 taps, exactly symmetric about tap 40, with the spec centre tap`() {
        assertEquals(80, MsbcTables.proto.size)
        for (n in 1 until 80) {
            assertEquals("tap $n", MsbcTables.proto[n], MsbcTables.proto[80 - n], 0.0)
        }
        assertTrue(abs(MsbcTables.proto[40] - 1.46955073e-01) < 1e-9)
    }

    @Test
    fun `analysis window carries the block-index sign fold and synthesis is -8x`() {
        for (n in 0 until 80) {
            val want = if ((n / 16) % 2 == 0) MsbcTables.proto[n] else -MsbcTables.proto[n]
            assertEquals("fold $n", want, MsbcTables.analysisWindow[n], 0.0)
        }
        assertEquals(-8.0 * MsbcTables.analysisWindow[40], MsbcTables.synthesisWindow[40], 0.0)
    }

    @Test
    fun `LOUDNESS allocation terminates on all-zero and matches the spec loop on a voiced frame`() {
        assertTrue(MsbcTables.calculateBits(IntArray(8)).sum() <= Msbc.BITPOOL)
        assertArrayEquals(
            intArrayOf(8, 7, 5, 6, 0, 0, 0, 0),
            MsbcTables.calculateBits(intArrayOf(11, 12, 9, 10, 0, 0, 0, 0)),
        )
    }

    // ---- round trip -----------------------------------------------------------------------------

    @Test
    fun `1 kHz round trip reconstructs at the 73-sample delay with SNR above 20 dB`() {
        val frames = 20
        val n = frames * Msbc.SAMPLES_PER_FRAME
        val src = tone(n, 1000.0, 8000.0)
        val enc = MsbcEncoder()
        val dec = MsbcDecoder()
        val decoded = ShortArray(n)
        for (f in 0 until frames) {
            val frame = enc.encode(src, f * 120, 120)
            assertNotNull("encoder accepted a 120-sample frame", frame)
            assertEquals(Msbc.FRAME_BYTES, frame!!.size)
            assertEquals(Msbc.SYNCWORD, frame[0].toInt() and 0xFF)
            assertEquals(0, frame[1].toInt())
            assertEquals(0, frame[2].toInt())
            val pcm = dec.decode(frame)
            assertNotNull("decode failed: ${dec.lastFailure}", pcm)
            System.arraycopy(pcm!!, 0, decoded, f * 120, 120)
        }
        val delay = Msbc.RECONSTRUCTION_DELAY
        var sig = 0.0
        var err = 0.0
        for (i in 240 until n - delay) {
            val a = src[i].toDouble()
            val b = decoded[i + delay].toDouble()
            sig += a * a
            err += (b - a) * (b - a)
        }
        val snr = 10.0 * log10(sig / maxOf(err, 1e-9))
        assertTrue("SNR $snr dB", snr > 20.0)
    }

    @Test
    fun `an all-zero frame decodes to digital silence, not a DC click`() {
        val enc = MsbcEncoder()
        val dec = MsbcDecoder()
        repeat(4) {
            val f = enc.encode(ShortArray(120))!!
            val pcm = dec.decode(f)!!
            assertTrue(pcm.all { it.toInt() == 0 })
        }
    }

    // ---- validation -----------------------------------------------------------------------------

    @Test
    fun `corrupt frames are rejected with the right reason and never read past`() {
        val src = tone(120, 400.0, 6000.0)
        val good = MsbcEncoder().encode(src)!!
        assertNotNull(MsbcDecoder().decode(good))

        val badCrc = good.copyOf().also { it[3] = (it[3].toInt() xor 0xFF).toByte() }
        MsbcDecoder().let {
            assertNull(it.decode(badCrc))
            assertEquals(MsbcDecoder.Failure.CRC_MISMATCH, it.lastFailure)
        }
        // A flipped scale-factor bit changes the CRC input, so it is caught rather than decoded
        // with the wrong bit allocation — which is the whole point of the CRC.
        val badSf = good.copyOf().also { it[4] = (it[4].toInt() xor 0x10).toByte() }
        MsbcDecoder().let {
            assertNull(it.decode(badSf))
            assertEquals(MsbcDecoder.Failure.CRC_MISMATCH, it.lastFailure)
        }
        val badSync = good.copyOf().also { it[0] = 0x9C.toByte() } // plain SBC syncword
        MsbcDecoder().let {
            assertNull(it.decode(badSync))
            assertEquals(MsbcDecoder.Failure.BAD_SYNC, it.lastFailure)
        }
        MsbcDecoder().let {
            assertNull(it.decode(ByteArray(20) { 0xAD.toByte() }))
            assertEquals(MsbcDecoder.Failure.SHORT_FRAME, it.lastFailure)
        }
        val reserved = good.copyOf().also { it[1] = 0x01 }
        MsbcDecoder().let {
            assertNull(it.decode(reserved))
            assertEquals(MsbcDecoder.Failure.RESERVED_HEADER, it.lastFailure)
        }
    }

    // ---- reference vectors ----------------------------------------------------------------------

    private val refFrames =
        listOf(
            "ad0000f8cc9a88777deddb5f7b76d860ddb5e7478cc6b1c95233c2ea314db5729c6d9f349b5729c6d9f349b5729c6d9f349b5729c6d9f349b4",
            "ad00009abc9a00003c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cc",
            "ad00009abc9a0000c2d172cf0b1f3c2d172cf0b1f3c2d172cf0b1f3c2d172cf0b1f3c2d172cf0b1f3c2d172cf0b1f3c2d172cf0b1f3c2d172c",
            "ad00009abc9a00003c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cc",
        )

    /** The 4th frame as decoded by the independent fixed-point reference, 8 samples per line. */
    private val refPcm4th: ShortArray =
        (
            "-4968 -36 4885 7072 6670 6092 6734 7162 4969 38 -4884 -7071 " +
                "-6671 -6091 -6733 -7161 -4968 -36 4885 7072 6670 6092 6734 7162 " +
                "4969 38 -4884 -7071 -6671 -6091 -6733 -7161 -4968 -36 4885 7072 " +
                "6670 6092 6734 7162 4969 38 -4884 -7071 -6671 -6091 -6733 -7161 " +
                "-4968 -36 4885 7072 6670 6092 6734 7162 4969 38 -4884 -7071 " +
                "-6671 -6091 -6733 -7161 -4968 -36 4885 7072 6670 6092 6734 7162 " +
                "4969 38 -4884 -7071 -6671 -6091 -6733 -7161 -4968 -36 4885 7072 " +
                "6670 6092 6734 7162 4969 38 -4884 -7071 -6671 -6091 -6733 -7161 " +
                "-4968 -36 4885 7072 6670 6092 6734 7162 4969 38 -4884 -7071 " +
                "-6671 -6091 -6733 -7161 -4968 -36 4885 7072 6670 6092 6734 7162"
        ).split(' ').map { it.toShort() }.toShortArray()

    private fun unhex(s: String): ByteArray = ByteArray(s.length / 2) { s.substring(2 * it, 2 * it + 2).toInt(16).toByte() }

    private fun hex(b: ByteArray): String = b.joinToString("") { "%02x".format(it.toInt() and 0xFF) }

    @Test
    fun `encoder reproduces the recorded reference bitstream byte-for-byte`() {
        // A change in scale-factor selection or quantiser rounding is a WIRE change and must not
        // pass silently: the far end is a phone's HFP stack, not this decoder.
        val src =
            ShortArray(480) { i ->
                val t = i / 16000.0
                roundAway(8000.0 * sin(2.0 * Math.PI * 1000.0 * t) + 2000.0 * sin(2.0 * Math.PI * 3000.0 * t)).toShort()
            }
        val enc = MsbcEncoder()
        for (f in 0 until 4) {
            assertEquals("frame $f", refFrames[f], hex(enc.encode(src, f * 120, 120)!!))
        }
    }

    @Test
    fun `decoder matches the independent fixed-point reference within 8 LSB`() {
        val dec = MsbcDecoder()
        var last: ShortArray? = null
        for (h in refFrames) {
            last = dec.decode(unhex(h))
            assertNotNull("reference frame decode failed: ${dec.lastFailure}", last)
        }
        var maxDiff = 0
        for (i in 0 until 120) maxDiff = maxOf(maxDiff, abs(last!![i] - refPcm4th[i]))
        assertTrue("max diff $maxDiff LSB", maxDiff <= 8)
    }
}
