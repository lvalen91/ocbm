package com.carlink.audio

import com.carlink.telephony.Msbc
import com.carlink.telephony.MsbcH2
import com.carlink.telephony.MsbcTelephonyDecoder
import com.carlink.telephony.MsbcUplinkEncoder
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The pure half of the codec-aware mic uplink: what the `CT_UPLINK` codec byte does to the capture
 * format and the per-tick drain, and the shape of what goes on `CH_MIC` under mSBC. The
 * `AudioRecord`-bound half (`MicrophoneCaptureManager.drainUplink`) is not JVM-testable; its
 * contract is exercised here through the same encoder it uses, in the same 240-in / 60-out cut.
 */
class MicUplinkCodecTest {
    @Test
    fun `only PCM and mSBC are encodable uplink codecs`() {
        assertTrue(MicProfile.isUplinkCodecSupported(MicProfile.CODEC_PCM))
        assertTrue(MicProfile.isUplinkCodecSupported(MicProfile.CODEC_MSBC))
        assertFalse(MicProfile.isUplinkCodecSupported(1)) // AAC-LC
        assertFalse(MicProfile.isUplinkCodecSupported(3)) // OPUS
    }

    @Test
    fun `mSBC forces 16 kHz mono capture whatever the gate's rate says`() {
        assertEquals(16000 to 1, MicProfile.captureFormatFor(16000, 1, MicProfile.CODEC_MSBC))
        assertEquals(16000 to 1, MicProfile.captureFormatFor(8000, 1, MicProfile.CODEC_MSBC))
        assertEquals(16000 to 1, MicProfile.captureFormatFor(48000, 2, MicProfile.CODEC_MSBC))
        // PCM is the negotiated pair, untouched.
        assertEquals(8000 to 1, MicProfile.captureFormatFor(8000, 1, MicProfile.CODEC_PCM))
        assertEquals(16000 to 2, MicProfile.captureFormatFor(16000, 2, MicProfile.CODEC_PCM))
    }

    @Test
    fun `the mSBC capture format survives the decodeType table round trip`() {
        // start() maps codec 4 onto decodeType 5 (SIRI_VOICE); that entry must be 16 kHz mono or
        // the encoder is fed the wrong rate with no error anywhere.
        val (r, c) = MicProfile.captureFormatFor(16000, 1, MicProfile.CODEC_MSBC)
        val dt = MicProfile.decodeTypeFor(r, c)
        assertEquals(5, dt)
        val f = MicFormats.fromDecodeType(dt!!)
        assertEquals(Msbc.SAMPLE_RATE, f.sampleRate)
        assertEquals(Msbc.CHANNELS, f.channelCount)
    }

    @Test
    fun `per-tick drain is one 20 ms chunk for PCM and a whole number of mSBC packets otherwise`() {
        assertEquals(640, MicProfile.uplinkDrainBytes(16000, 1, MicProfile.CODEC_PCM))
        assertEquals(320, MicProfile.uplinkDrainBytes(8000, 1, MicProfile.CODEC_PCM))
        val d = MicProfile.uplinkDrainBytes(16000, 1, MicProfile.CODEC_MSBC)
        assertEquals(0, d % MicProfile.MSBC_PCM_BYTES_PER_PACKET)
        // Must exceed one tick of 16 kHz mono (640 B = 2.67 packets) or the ring buffer fills.
        assertTrue(d >= 3 * MicProfile.MSBC_PCM_BYTES_PER_PACKET)
        assertEquals(Msbc.PCM_BYTES_PER_FRAME, MicProfile.MSBC_PCM_BYTES_PER_PACKET)
        assertEquals(Msbc.PACKET_BYTES, MicProfile.MSBC_PACKET_BYTES)
    }

    /**
     * The 20 ms tick over a 7.5 ms codec: simulate what `drainUplink` does — pull whole 240-byte
     * frames, carry the remainder — over 100 ticks of steady 640 B/tick capture, and check the
     * carry never grows and the far end sees every sample once, in order.
     */
    @Test
    fun `mSBC packets ride the 20 ms tick without drift, one message per packet`() {
        val enc = MsbcUplinkEncoder()
        val dec = MsbcTelephonyDecoder()
        var ring = ByteArray(0)
        var sent = 0
        var decodedBytes = 0
        var maxCarry = 0
        repeat(100) {
            ring += ByteArray(640) // one tick of captured 16 kHz mono
            val whole = minOf(ring.size, MicProfile.uplinkDrainBytes(16000, 1, MicProfile.CODEC_MSBC)) / 240 * 240
            var off = 0
            while (off < whole) {
                val pkt = enc.packet(ring, off, 240)!!
                assertEquals(Msbc.PACKET_BYTES, pkt.size) // one CH_MIC message each
                decodedBytes += dec.decode(pkt).size
                sent += 1
                off += 240
            }
            ring = ring.copyOfRange(whole, ring.size)
            maxCarry = maxOf(maxCarry, ring.size)
        }
        assertEquals(100 * 640 / 240, sent) // 266 packets from 64000 B, 160 B carried
        assertEquals(sent * Msbc.PCM_BYTES_PER_FRAME, decodedBytes)
        assertTrue("carry $maxCarry", maxCarry < 240)
        assertEquals(0, dec.lostPackets)
        // Sequence cycles continuously across ticks.
        val next = enc.packet(ByteArray(240))!!
        assertEquals(MsbcH2.SEQUENCE_BYTES[sent % 4], next[1].toInt() and 0xFF)
    }
}
