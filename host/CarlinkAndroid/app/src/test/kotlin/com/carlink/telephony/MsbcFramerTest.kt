package com.carlink.telephony

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlin.math.log10
import kotlin.math.sin

/**
 * The H2/eSCO framer and the two adapters, over the failure modes the proto calls load-bearing:
 * a payload that is not a whole frame, resync across a split payload, loss detected from the
 * sequence number, and junk on the lane. Plus the uplink codec round trip: what
 * `MsbcUplinkEncoder` puts on `CH_MIC` must be exactly what `MsbcTelephonyDecoder` turns back
 * into speech.
 */
class MsbcFramerTest {
    private fun pcmLe(
        n: Int,
        hz: Double,
        amp: Double,
        phase0: Int = 0,
    ): ByteArray {
        val out = ByteArray(n * 2)
        for (i in 0 until n) {
            val s = (amp * sin(2.0 * Math.PI * hz * (i + phase0) / 16000.0)).toInt()
            out[2 * i] = (s and 0xFF).toByte()
            out[2 * i + 1] = ((s shr 8) and 0xFF).toByte()
        }
        return out
    }

    private fun fourPackets(): List<ByteArray> {
        val up = MsbcUplinkEncoder()
        val pcm = pcmLe(120, 800.0, 5000.0)
        return List(4) { up.packet(pcm)!! }
    }

    private fun frames(events: List<MsbcFramer.Event>) = events.count { it is MsbcFramer.Event.Frame }

    @Test
    fun `an eSCO packet is 60 bytes with 0x01, a cycling sequence byte, the syncword and a zero pad`() {
        val packets = fourPackets()
        assertTrue(packets.all { it.size == Msbc.PACKET_BYTES })
        assertArrayEquals(intArrayOf(0x08, 0x38, 0xC8, 0xF8), packets.map { it[1].toInt() and 0xFF }.toIntArray())
        assertTrue(packets.all { (it[0].toInt() and 0xFF) == 0x01 && (it[2].toInt() and 0xFF) == 0xAD && it[59].toInt() == 0 })
        // Fifth packet wraps the sequence.
        val up = MsbcUplinkEncoder()
        repeat(4) { up.packet(pcmLe(120, 800.0, 5000.0)) }
        assertEquals(0x08, up.packet(pcmLe(120, 800.0, 5000.0))!![1].toInt() and 0xFF)
    }

    @Test
    fun `four clean packets yield four frames, no loss, and the pad byte is not a resync`() {
        val f = MsbcFramer()
        var got = 0
        for (p in fourPackets()) got += frames(f.push(p))
        assertEquals(4, got)
        assertEquals(0, f.lostPackets)
        assertEquals(0, f.resyncs)
    }

    @Test
    fun `the framer reassembles across reads cut at packet-misaligned boundaries`() {
        // 37-byte chunks: every packet is split, several chunks straddle two packets, and no chunk
        // boundary coincides with a frame boundary. Splitting by message length would fail here.
        val flat = fourPackets().reduce { a, b -> a + b }
        val f = MsbcFramer()
        var got = 0
        var off = 0
        while (off < flat.size) {
            val n = minOf(37, flat.size - off)
            got += frames(f.push(flat, off, n))
            off += n
        }
        assertEquals(4, got)
        assertEquals(0, f.lostPackets)
    }

    @Test
    fun `a payload of one and a half packets yields one frame now and the rest later`() {
        val p = fourPackets()
        val f = MsbcFramer()
        assertEquals(1, frames(f.push(p[0] + p[1].copyOfRange(0, 30))))
        assertEquals(1, frames(f.push(p[1].copyOfRange(30, 60))))
        assertEquals(0, f.lostPackets)
    }

    @Test
    fun `a dropped packet is detected from the H2 sequence gap and counted, not silently skipped`() {
        val p = fourPackets()
        val f = MsbcFramer()
        val events = ArrayList<MsbcFramer.Event>()
        for ((i, pkt) in p.withIndex()) if (i != 2) events += f.push(pkt)
        assertEquals(3, frames(events))
        val lost = events.filterIsInstance<MsbcFramer.Event.Lost>().map { it.n }
        assertEquals(listOf(1), lost)
        assertEquals(1, f.lostPackets)
    }

    @Test
    fun `junk on the lane is skipped, the packet behind it decodes, and one resync is counted`() {
        val f = MsbcFramer()
        val got = frames(f.push(ByteArray(40) { 0x5A } + fourPackets()[0]))
        assertEquals(1, got)
        assertEquals(1, f.resyncs)
    }

    @Test
    fun `a false H2 pair without the mSBC syncword behind it is not a lock`() {
        // 0x01 0x08 occurs inside compressed audio often enough to matter; 0x01 0x08 0xAD does not.
        // Locking on the pair alone would emit 57 bytes of junk as a "frame" and mis-sequence the
        // real packet behind it.
        val real = fourPackets()[0]
        val junk = byteArrayOf(0x01, 0x08, 0x77, 0x01, 0x38, 0x00) + ByteArray(20) { 0x22 }
        val f = MsbcFramer()
        val events = f.push(junk + real)
        val frames = events.filterIsInstance<MsbcFramer.Event.Frame>()
        assertEquals(1, frames.size)
        assertArrayEquals(real.copyOfRange(2, 59), frames[0].bytes)
        assertEquals(0, f.lostPackets)
        assertEquals(1, f.resyncs)
    }

    @Test
    fun `a header split across two pushes still locks`() {
        // 0x01 at the very end of one read, 0x08 0xAD... at the start of the next.
        val p = fourPackets()[0]
        val f = MsbcFramer()
        assertEquals(0, frames(f.push(p.copyOfRange(0, 1))))
        assertEquals(1, frames(f.push(p.copyOfRange(1, 60))))
        assertEquals(0, f.resyncs)
    }

    @Test
    fun `the reassembly buffer is capped so a non-mSBC lane cannot grow memory`() {
        val f = MsbcFramer()
        f.push(ByteArray(10_000) { 0x11 })
        assertTrue(f.resyncs >= 1)
        // Still functional afterwards.
        assertEquals(1, frames(f.push(fourPackets()[0])))
    }

    @Test
    fun `telephony adapter conceals a dropped packet and yields nothing for a fragment`() {
        val p = fourPackets()
        val tel = MsbcTelephonyDecoder()
        var out = ByteArray(0)
        for ((i, pkt) in p.withIndex()) if (i != 2) out += tel.decode(pkt)
        assertEquals(4 * Msbc.PCM_BYTES_PER_FRAME, out.size)
        assertEquals(3, tel.framesDecoded)
        assertEquals(1, tel.plcFrames)

        val frag = MsbcTelephonyDecoder()
        assertEquals(0, frag.decode(p[0].copyOfRange(0, 30)).size)
        assertEquals(Msbc.PCM_BYTES_PER_FRAME, frag.decode(p[0].copyOfRange(30, 60)).size)
    }

    @Test
    fun `a corrupt frame is concealed rather than rendered`() {
        val p = fourPackets()
        val bad = p[1].copyOf().also { it[2 + 3] = (it[5].toInt() xor 0xFF).toByte() } // CRC byte
        val tel = MsbcTelephonyDecoder()
        tel.decode(p[0])
        val out = tel.decode(bad)
        assertEquals(Msbc.PCM_BYTES_PER_FRAME, out.size)
        assertEquals(1, tel.decodeFailures)
        assertEquals(1, tel.plcFrames)
        assertEquals(MsbcDecoder.Failure.CRC_MISMATCH, tel.lastFailure)
    }

    /**
     * The uplink codec round trip, in the wire shape: PCM S16LE in 240-byte frames -> one 60-byte
     * packet each (as `MicrophoneCaptureManager.drainUplink` sends them, one per `CH_MIC` message)
     * -> the downlink adapter -> S16LE. The recovered speech must be the input at the filterbank
     * delay, and the byte order at both ends must be little-endian (a swap at either end shows up
     * here as an SNR of roughly 0 dB, not as an error).
     */
    @Test
    fun `uplink encode to downlink decode round trip preserves the signal and byte order`() {
        val frames = 20
        val n = frames * Msbc.SAMPLES_PER_FRAME
        val src = pcmLe(n, 1000.0, 8000.0)
        val up = MsbcUplinkEncoder()
        val down = MsbcTelephonyDecoder()
        var out = ByteArray(0)
        for (f in 0 until frames) {
            val pkt = up.packet(src, f * Msbc.PCM_BYTES_PER_FRAME, Msbc.PCM_BYTES_PER_FRAME)
            assertNotNull(pkt)
            assertEquals(Msbc.PACKET_BYTES, pkt!!.size)
            out += down.decode(pkt)
        }
        assertEquals(src.size, out.size)
        assertEquals(0, down.lostPackets)
        assertEquals(0, down.plcFrames)

        fun s16le(
            b: ByteArray,
            i: Int,
        ) = ((b[2 * i].toInt() and 0xFF) or (b[2 * i + 1].toInt() shl 8)).toShort().toDouble()
        val delay = Msbc.RECONSTRUCTION_DELAY
        var sig = 0.0
        var err = 0.0
        for (i in 240 until n - delay) {
            val a = s16le(src, i)
            val b = s16le(out, i + delay)
            sig += a * a
            err += (b - a) * (b - a)
        }
        val snr = 10.0 * log10(sig / maxOf(err, 1e-9))
        assertTrue("SNR $snr dB", snr > 20.0)
    }

    @Test
    fun `uplink encoder refuses a wrong-sized frame rather than emitting a malformed packet`() {
        val up = MsbcUplinkEncoder()
        assertEquals(null, up.packet(ByteArray(239)))
        assertEquals(null, up.packet(ByteArray(241)))
        assertEquals(0, up.packetsOut)
    }
}
