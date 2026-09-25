package com.carlink.av

import com.carlink.ocbm.seam.SeamCrypto
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Format → decoder dispatch and the AudioSpecificConfig bytes, off-device.
 *
 * The values are literal, not re-derived from [AacCsd]: an ASC is authoritative over the MediaFormat
 * rate/channels for every AAC decoder, so a bit-order slip here decodes 3x fast with the channels
 * garbled and nothing anywhere reports an error.
 */
class AudioDecodersTest {
    @Test
    fun `every codec the box can put in a SEAM_FORMAT has exactly one path`() {
        assertEquals(AudioDecoders.Path.PCM, AudioDecoders.pathFor(SeamCrypto.CODEC_PCM))
        assertEquals(AudioDecoders.Path.AAC_LC, AudioDecoders.pathFor(SeamCrypto.CODEC_AAC_LC))
        assertEquals(AudioDecoders.Path.AAC_ELD, AudioDecoders.pathFor(SeamCrypto.CODEC_AAC_ELD))
        // Opus is advertised by neither preset; mSBC is decoded by the seam and never reaches a player.
        assertEquals(AudioDecoders.Path.UNSUPPORTED, AudioDecoders.pathFor(SeamCrypto.CODEC_OPUS))
        assertEquals(AudioDecoders.Path.UNSUPPORTED, AudioDecoders.pathFor(SeamCrypto.CODEC_MSBC))
        assertEquals(AudioDecoders.Path.UNSUPPORTED, AudioDecoders.pathFor(200))
    }

    @Test
    fun `canDecode gates focus and track creation on the same table`() {
        assertTrue(AudioDecoders.canDecode(SeamCrypto.CODEC_PCM))
        assertTrue(AudioDecoders.canDecode(SeamCrypto.CODEC_AAC_LC))
        assertTrue(AudioDecoders.canDecode(SeamCrypto.CODEC_AAC_ELD))
        assertFalse(AudioDecoders.canDecode(SeamCrypto.CODEC_OPUS))
        assertFalse(AudioDecoders.canDecode(SeamCrypto.CODEC_MSBC))
    }

    @Test
    fun `AAC-LC csd-0 for the wireless media stream is 0x1190`() {
        // objectType 2 (5 bits) | rateIdx 3 = 48 kHz (4 bits) | channelConfig 2 (4 bits) | 000
        assertArrayEquals(byteArrayOf(0x11, 0x90.toByte()), AacCsd.lc(48000, 2))
        // 44.1 kHz stereo: rateIdx 4.
        assertArrayEquals(byteArrayOf(0x12, 0x10), AacCsd.lc(44100, 2))
        // An unknown rate falls back to the 48 kHz index rather than an out-of-range one.
        assertArrayEquals(AacCsd.lc(48000, 2), AacCsd.lc(47000, 2))
    }

    @Test
    fun `AAC-ELD 16k mono csd-0 is the device-confirmed fdk capture, verbatim`() {
        assertArrayEquals(
            byteArrayOf(0xF8.toByte(), 0xF0.toByte(), 0x31, 0x2C, 0x00, 0xBC.toByte(), 0x00),
            AacCsd.eld(16000, 1),
        )
    }

    @Test
    fun `AAC-ELD 48k stereo csd-0 is synthesised with frameLength 480 and no SBR`() {
        // 11111 000111 | 0011 (48 kHz) | 0010 (stereo) | 1 (480) | 0 0 0 | 0 (no SBR) | 0000 | pad
        assertArrayEquals(byteArrayOf(0xF8.toByte(), 0xE6.toByte(), 0x50, 0x00), AacCsd.eld(48000, 2))
        // 24 kHz mono (the box's aac_eld_24k_mono token): rateIdx 6, ch 1.
        assertArrayEquals(byteArrayOf(0xF8.toByte(), 0xEC.toByte(), 0x30, 0x00), AacCsd.eld(24000, 1))
    }

    @Test
    fun `sample-rate index follows ISO 14496-3 table 1_16`() {
        assertEquals(3, AacCsd.sfIndex(48000))
        assertEquals(4, AacCsd.sfIndex(44100))
        assertEquals(8, AacCsd.sfIndex(16000))
        assertEquals(11, AacCsd.sfIndex(8000))
    }

    @Test
    fun `PCM passthrough hands the access unit through untouched and skips a lone byte`() {
        val dec = PcmPassthrough()
        var got: ByteArray? = null
        dec.decode(byteArrayOf(9, 1, 2, 3, 4), 1, 4) { b, off, len -> got = b.copyOfRange(off, off + len) }
        assertArrayEquals(byteArrayOf(1, 2, 3, 4), got)
        var calls = 0
        dec.decode(byteArrayOf(1), 0, 1) { _, _, _ -> calls++ }
        assertEquals(0, calls)
    }

    @Test
    fun `digital silence is not audible and a -32 dBFS peak is`() {
        val silence = ByteArray(64)
        assertFalse(PcmLevel.audible(silence, 0, silence.size))
        // One S16LE sample of 801 at offset 10.
        val loud = ByteArray(64)
        loud[10] = 0x21
        loud[11] = 0x03
        assertTrue(PcmLevel.audible(loud, 0, loud.size))
        assertFalse("a window that excludes the sample stays silent", PcmLevel.audible(loud, 12, 20))
        // Negative peak counts too: -801 = 0xFCDF.
        val neg = ByteArray(8)
        neg[2] = 0xDF.toByte()
        neg[3] = 0xFC.toByte()
        assertTrue(PcmLevel.audible(neg, 0, neg.size))
    }
}
