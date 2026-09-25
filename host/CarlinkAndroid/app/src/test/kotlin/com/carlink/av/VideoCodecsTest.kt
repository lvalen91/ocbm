package com.carlink.av

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The codec-neutral NAL walk the renderer keys configure/keyframe gating on.
 *
 * Synthetic parameter sets: the bytes after the NAL header are opaque to the scan, so short filler is
 * enough. What matters is the header byte — `0x67`/`0x68`/`0x65` for H.264 SPS/PPS/IDR (ref_idc 3),
 * `0x40`/`0x42`/`0x44` for HEVC VPS/SPS/PPS and `0x26` for an IDR_W_RADL (type 19).
 */
class VideoCodecsTest {
    private val sc4 = byteArrayOf(0, 0, 0, 1)
    private val sc3 = byteArrayOf(0, 0, 1)

    private fun nal(
        hdr: Int,
        vararg body: Int,
    ): ByteArray = byteArrayOf(hdr.toByte()) + body.map { it.toByte() }.toByteArray()

    @Test
    fun `sniff tells the codecs apart from one parameter-set header byte`() {
        assertEquals(VideoCodec.H264, VideoCodecs.sniff(0x67)) // SPS
        assertEquals(VideoCodec.H264, VideoCodecs.sniff(0x68)) // PPS
        assertEquals(VideoCodec.H264, VideoCodecs.sniff(0x27)) // SPS, ref_idc 1
        assertEquals(VideoCodec.HEVC, VideoCodecs.sniff(0x40)) // VPS
        assertEquals(VideoCodec.HEVC, VideoCodecs.sniff(0x42)) // SPS
        assertEquals(VideoCodec.HEVC, VideoCodecs.sniff(0x44)) // PPS
        assertNull("an IDR slice is not a parameter set", VideoCodecs.sniff(0x65))
        assertNull("an HEVC IRAP is not a parameter set", VideoCodecs.sniff(0x26))
    }

    @Test
    fun `an H264 parameter-set message latches the codec and captures SPS and PPS with start codes`() {
        val sps = sc4 + nal(0x67, 0x42, 0x00, 0x1E, 0xAB)
        val pps = sc4 + nal(0x68, 0xCE, 0x38, 0x80)
        val s = VideoCodecs.scan(null, sps + pps)
        assertEquals(VideoCodec.H264, s.codec)
        assertArrayEquals(sps, s.sps)
        assertArrayEquals(pps, s.pps)
        assertNull(s.vps)
        assertTrue(s.sawParamSet)
        assertFalse(s.sawVcl)
        assertFalse(s.keyframe)
    }

    @Test
    fun `an H264 IDR is a keyframe and a P slice is not`() {
        val idr = VideoCodecs.scan(VideoCodec.H264, sc4 + nal(0x65, 0x88, 0x84, 0x00, 0x33))
        assertTrue(idr.sawVcl)
        assertTrue(idr.keyframe)
        assertFalse(idr.sawParamSet)
        val p = VideoCodecs.scan(VideoCodec.H264, sc3 + nal(0x41, 0x9A, 0x02, 0x11, 0x22))
        assertTrue(p.sawVcl)
        assertFalse(p.keyframe)
    }

    @Test
    fun `an HEVC config message captures VPS SPS PPS and an IRAP is a keyframe`() {
        val vps = sc4 + nal(0x40, 0x01, 0x0C, 0x01)
        val sps = sc4 + nal(0x42, 0x01, 0x01, 0x01)
        val pps = sc4 + nal(0x44, 0x01, 0xC0, 0x2C)
        val s = VideoCodecs.scan(null, vps + sps + pps)
        assertEquals(VideoCodec.HEVC, s.codec)
        assertArrayEquals(vps, s.vps)
        assertArrayEquals(sps, s.sps)
        assertArrayEquals(pps, s.pps)
        assertTrue(s.sawParamSet && !s.sawVcl)
        // A mixed IDR access unit (in-band parameter sets + slice) reports both, so it is fed whole.
        val mixed = VideoCodecs.scan(VideoCodec.HEVC, vps + sps + pps + sc4 + nal(0x26, 0x01, 0xAF, 0x00))
        assertTrue(mixed.sawParamSet && mixed.sawVcl && mixed.keyframe)
        // TRAIL_R (type 1) is VCL but not a keyframe.
        val trail = VideoCodecs.scan(VideoCodec.HEVC, sc4 + nal(0x02, 0x01, 0xE0, 0x11))
        assertTrue(trail.sawVcl && !trail.keyframe)
    }

    @Test
    fun `a VCL message before any parameter set yields no codec and is reported as VCL`() {
        val s = VideoCodecs.scan(null, sc4 + nal(0x65, 0x88, 0x84, 0x00))
        assertNull(s.codec)
        assertTrue(s.sawVcl)
        assertFalse(s.keyframe)
    }

    @Test
    fun `a latched codec is never re-sniffed from a later message`() {
        // 0x67 under HEVC is type 51 (reserved): neither a parameter set nor VCL. The codec stays HEVC.
        val s = VideoCodecs.scan(VideoCodec.HEVC, sc4 + nal(0x67, 0x42, 0x00, 0x1E))
        assertEquals(VideoCodec.HEVC, s.codec)
        assertFalse(s.sawParamSet)
        assertFalse(s.sawVcl)
    }

    @Test
    fun `the parameter-set classifier is per codec`() {
        assertEquals(VideoCodecs.PS_SPS, VideoCodecs.paramSetKind(VideoCodec.H264, 7))
        assertEquals(VideoCodecs.PS_PPS, VideoCodecs.paramSetKind(VideoCodec.H264, 8))
        assertEquals(VideoCodecs.PS_NONE, VideoCodecs.paramSetKind(VideoCodec.H264, 32))
        assertEquals(VideoCodecs.PS_VPS, VideoCodecs.paramSetKind(VideoCodec.HEVC, 32))
        assertEquals(VideoCodecs.PS_SPS, VideoCodecs.paramSetKind(VideoCodec.HEVC, 33))
        assertEquals(VideoCodecs.PS_PPS, VideoCodecs.paramSetKind(VideoCodec.HEVC, 34))
        assertEquals(VideoCodecs.PS_NONE, VideoCodecs.paramSetKind(VideoCodec.HEVC, 7))
    }
}
