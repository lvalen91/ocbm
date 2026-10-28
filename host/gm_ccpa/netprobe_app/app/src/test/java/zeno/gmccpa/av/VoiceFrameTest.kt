package zeno.gmccpa.av

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Plan v5.8 V1a: the v2 voice wire. The byte arrays are the SAME golden vectors as
 * `crates/vendor/receiver/src/forward.rs` `voice_v2_golden_vectors`; change one, change both.
 */
class VoiceFrameTest {

    private fun b(vararg x: Int) = ByteArray(x.size) { x[it].toByte() }

    private val auGolden = b(
        0x00, 0x00, 0x3E, 0x80, 0x00, 0x01, 0x81, 0x00, 0x00, 0x00, 0x1A,
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
        0xBE, 0xEF, 0xCA, 0xFE, 0xF0, 0x0D, 100,
        0xA1, 0xA2, 0xA3,
    )
    private val beginGolden = b(
        0x00, 0x00, 0xBB, 0x80, 0x00, 0x02, 0xFF, 0x00, 0x00, 0x00, 0x23,
        0x01, 0, 0, 0, 0, 0, 0, 0, 0x10,
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
        101, 4, 2, 0x00, 0x00, 0xBB, 0x80, 0x00, 0x02,
        0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x03,
    )
    private val endGolden = b(
        0x00, 0x00, 0x3E, 0x80, 0x00, 0x01, 0xFF, 0x00, 0x00, 0x00, 0x22,
        0x02, 0, 0, 0, 0, 0, 0, 0, 0x20,
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
        0x03, 0x00, 0x00, 0x02, 0xF5, 0x00, 0x00, 0x00, 0x02,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x01, 0xE0,
    )

    private fun body(frame: ByteArray) = frame.copyOfRange(VoiceFrame.HEAD_LEN, frame.size)

    @Test fun auGoldenVector() {
        val h = VoiceFrame.parseHead(auGolden)
        assertEquals(VoiceFrame.Head(16000, 1, 0x81, 26), h)
        assertEquals(VoiceFrame.Kind.AU_V2, VoiceFrame.classify(h))
        assertEquals(1, h.atype and 0x7F)
        val ext = VoiceFrame.parseAuExt(body(auGolden))
        assertEquals(VoiceFrame.AuExt(0x0102030405060708L, 0x1122334455667788L, 0xBEEF, 0xCAFEF00DL, 100), ext)
        assertTrue(body(auGolden).copyOfRange(VoiceFrame.V2_EXT_LEN, 26).contentEquals(b(0xA1, 0xA2, 0xA3)))
    }

    @Test fun streamTypeByteSeparatesNavFromSiriDefault() {
        val nav = auGolden.copyOf().also { it[33] = 101; it[6] = (0x80 or 4).toByte() }
        val siri = auGolden.copyOf().also { it[33] = 100; it[6] = (0x80 or 4).toByte() }
        assertEquals(101, VoiceFrame.parseAuExt(body(nav)).streamType)
        assertEquals(100, VoiceFrame.parseAuExt(body(siri)).streamType)
    }

    @Test fun beginGoldenVector() {
        val h = VoiceFrame.parseHead(beginGolden)
        assertEquals(VoiceFrame.Kind.CONTROL, VoiceFrame.classify(h))
        assertEquals(VoiceFrame.BEGIN_LEN, h.len)
        assertEquals(
            VoiceFrame.Begin(0x10, 0x0102030405060708L, 101, 4, VoiceFrame.CODEC_AAC_ELD, 48000, 2, -1, false, 3),
            VoiceFrame.parseControl(body(beginGolden)),
        )
    }

    @Test fun endGoldenVector() {
        val h = VoiceFrame.parseHead(endGolden)
        assertEquals(VoiceFrame.Kind.CONTROL, VoiceFrame.classify(h))
        assertEquals(VoiceFrame.END_LEN, h.len)
        assertEquals(
            VoiceFrame.End(0x20, 0x0102030405060708L, VoiceFrame.END_SUPERSEDED, 757, 2, 1, 480),
            VoiceFrame.parseControl(body(endGolden)),
        )
    }

    /** VR:514-518 rejected any rate outside 8-48 kHz FIRST; a control frame must pass whatever its rate. */
    @Test fun controlIsDispatchedBeforeTheRateCheck() {
        assertEquals(VoiceFrame.Kind.CONTROL, VoiceFrame.classify(VoiceFrame.Head(0, 0, 0xFF, 34)))
        assertEquals(VoiceFrame.Kind.CONTROL, VoiceFrame.classify(VoiceFrame.Head(96000, 2, 0xFF, 35)))
        assertEquals(VoiceFrame.Kind.DESYNC, VoiceFrame.classify(VoiceFrame.Head(0, 0, 0x81, 30)))
        assertEquals(VoiceFrame.Kind.DESYNC, VoiceFrame.classify(VoiceFrame.Head(1469658167, 232, 2, 4)))
        assertEquals(VoiceFrame.Kind.DESYNC, VoiceFrame.classify(VoiceFrame.Head(16000, 1, 0xFF, -1)))
        assertEquals(VoiceFrame.Kind.DESYNC, VoiceFrame.classify(VoiceFrame.Head(16000, 1, 0x82, 22)))
        assertEquals(VoiceFrame.Kind.AU_V1, VoiceFrame.classify(VoiceFrame.Head(16000, 1, 2, 40)))
    }

    @Test fun shortOrUnknownControlIsNull() {
        assertNull(VoiceFrame.parseControl(ByteArray(0)))
        assertNull(VoiceFrame.parseControl(body(beginGolden).copyOf(34)))
        assertNull(VoiceFrame.parseControl(b(9, 0, 0)))
    }

    @Test fun histogramPercentiles() {
        val h = LatencyHist()
        assertEquals(-1, h.percentileMs(50.0))
        for (ms in 1..100) h.add(ms * 1000L - 500)
        assertEquals(49, h.percentileMs(50.0))
        assertEquals(94, h.percentileMs(95.0))
        assertEquals(98, h.percentileMs(99.0))
        h.add(12_345_000)
        assertEquals(12_345, h.maxUs / 1000)
        assertEquals(12_300, h.percentileMs(100.0))
    }

    @Test fun streamStatsCountsSeqGapsAndResetsTheWindow() {
        val s = VoiceStreamStats(0xAB, "call/100")
        s.onRead(1_000, 3_000, 65534)
        s.onRead(2_000, 4_000, 65535)
        s.onRead(3_000, 5_000, 2)        // wraps; 0 and 1 lost
        s.onWrite(1_000, 10_000)
        assertEquals(3, s.aus)
        assertEquals(2, s.seqGaps)
        val w = s.window()!!
        assertTrue(w, w.contains("3 AUs") && w.contains("rx→read p50/p95/p99/max 2/2/2/2ms"))
        assertNull("window resets", s.window())
        assertTrue(s.total().startsWith("3 AUs read"))
    }
}
