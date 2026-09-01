package com.carlink.audio

import com.carlink.ocbm.Ocbm
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Mic uplink format selection.
 *
 * The defect class this exists for is silent: [MicrophoneCaptureManager] keys capture off
 * riddleBox's `decodeType` table, so a negotiated format that does not survive a round trip
 * through that table opens the microphone at the wrong rate while the box believes otherwise.
 * Siri then hears a pitch-shifted stream and nothing anywhere reports an error — not the box, not
 * logcat, not the phone. Nothing but a test can catch it.
 *
 * These assert against literal expected values rather than re-deriving them from [MicProfile], so
 * a sign/order/table error in the production code cannot cancel itself out in the assertion.
 */
class MicProfileTest {
    /** The four formats the capture table actually implements, and their decodeTypes. */
    private val supported =
        listOf(
            Triple(8000, 1, 3),
            Triple(16000, 1, 5),
            Triple(24000, 1, 6),
            Triple(16000, 2, 7),
        )

    // ---- the table ------------------------------------------------------------------------------

    @Test
    fun `each supported format maps to its decodeType`() {
        for ((rate, ch, expected) in supported) {
            assertEquals("${rate}Hz ${ch}ch", expected, MicProfile.decodeTypeFor(rate, ch))
        }
    }

    /**
     * THE pitch-shift guard, and the reason this file exists.
     *
     * A decodeType is only meaningful if the capture manager reopens the exact format the box
     * negotiated. Asserting the mapping alone would not catch a table that is self-consistently
     * wrong — this closes the loop through [MicFormats], the table capture actually reads.
     */
    @Test
    fun `every supported format round-trips through MicFormats unchanged`() {
        for ((rate, ch, _) in supported) {
            val decodeType = MicProfile.decodeTypeFor(rate, ch)
            assertNotNull("${rate}Hz ${ch}ch must be mappable", decodeType)

            val reopened = MicFormats.fromDecodeType(decodeType!!)
            assertEquals(
                "decodeType $decodeType must reopen at ${rate}Hz, not ${reopened.sampleRate}Hz — " +
                    "a mismatch here reaches Siri pitch-shifted with no error anywhere",
                rate,
                reopened.sampleRate,
            )
            assertEquals(
                "decodeType $decodeType must reopen with $ch channel(s)",
                ch,
                reopened.channelCount,
            )
        }
    }

    @Test
    fun `unmapped formats return null rather than a silent fallback`() {
        // 48 kHz stereo is the DOWNLINK media format; it is not a capture profile. Returning a
        // fallback for it silently would be exactly the bug this signature is shaped to prevent.
        assertNull(MicProfile.decodeTypeFor(48000, 2))
        assertNull(MicProfile.decodeTypeFor(44100, 1))
        assertNull(MicProfile.decodeTypeFor(8000, 2))
        assertNull(MicProfile.decodeTypeFor(24000, 2))
        assertNull(MicProfile.decodeTypeFor(0, 0))
        assertNull(MicProfile.decodeTypeFor(16000, 0))
    }

    @Test
    fun `the fallback is 16 kHz mono`() {
        assertEquals(5, MicProfile.FALLBACK_DECODE_TYPE)
        val f = MicFormats.fromDecodeType(MicProfile.FALLBACK_DECODE_TYPE)
        assertEquals(16000, f.sampleRate)
        assertEquals(1, f.channelCount)
    }

    // ---- tick arithmetic ------------------------------------------------------------------------

    @Test
    fun `chunk size is 20 ms of PCM at each supported format`() {
        assertEquals(20, MicProfile.TICK_MS)
        assertEquals(50, MicProfile.TICKS_PER_SECOND)

        // Hand-computed: rate x seconds x bytes-per-sample x channels.
        assertEquals(320, MicProfile.chunkBytes(8000, 1)) //  8000 * 0.02 * 2 * 1
        assertEquals(640, MicProfile.chunkBytes(16000, 1)) // 16000 * 0.02 * 2 * 1
        assertEquals(960, MicProfile.chunkBytes(24000, 1)) // 24000 * 0.02 * 2 * 1
        assertEquals(1280, MicProfile.chunkBytes(16000, 2)) // 16000 * 0.02 * 2 * 2
    }

    /**
     * A chunk that ends mid-frame desyncs every sample after it — the left channel of one frame
     * pairs with the right channel of the next, for the rest of the session.
     */
    @Test
    fun `chunk size is a whole number of sample frames`() {
        for ((rate, ch, _) in supported) {
            val frameBytes = MicProfile.BYTES_PER_SAMPLE * ch
            val chunk = MicProfile.chunkBytes(rate, ch)
            assertEquals(
                "${rate}Hz ${ch}ch: $chunk B is not a whole number of ${frameBytes}B frames",
                0,
                chunk % frameBytes,
            )
            assertTrue("${rate}Hz ${ch}ch must produce a non-empty tick", chunk > 0)
        }
    }

    /**
     * One tick must fit in one OCBM frame, or the common path takes the oversize splitter and the
     * "one buffer, one queue slot" atomicity argument in `sendMicPcm` stops describing reality.
     */
    @Test
    fun `one tick always fits inside a single MIC_CHUNK`() {
        for ((rate, ch, _) in supported) {
            assertTrue(
                "${rate}Hz ${ch}ch tick (${MicProfile.chunkBytes(rate, ch)}B) must not exceed MIC_CHUNK",
                MicProfile.chunkBytes(rate, ch) <= Ocbm.MIC_CHUNK,
            )
        }
    }

    /**
     * Pins the stated rationale for MIC_CHUNK's value: it is 4-byte aligned so a split boundary
     * cannot bisect a 16-bit stereo sample frame, which is the widest frame the uplink carries.
     */
    @Test
    fun `MIC_CHUNK is a whole number of stereo sample frames`() {
        assertEquals(16384, Ocbm.MIC_CHUNK)
        assertEquals(0, Ocbm.MIC_CHUNK % (MicProfile.BYTES_PER_SAMPLE * 2))
    }
}
