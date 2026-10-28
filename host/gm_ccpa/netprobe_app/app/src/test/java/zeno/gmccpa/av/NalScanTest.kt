package zeno.gmccpa.av

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** F-C2 (2026-10-08): the NAL scan is bounded by `len`, so a reused buffer's stale tail is never read. */
class NalScanTest {
    private fun nal(type: Int, body: Int, long: Boolean = true): ByteArray =
        (if (long) byteArrayOf(0, 0, 0, 1) else byteArrayOf(0, 0, 1)) + byteArrayOf((type shl 1).toByte()) + ByteArray(body) { 0x5A }

    private val vps = nal(32, 6); private val sps = nal(33, 20, long = false); private val pps = nal(34, 4)
    private val idr = nal(19, 50); private val p = nal(1, 20)

    @Test fun parameterSetsAreCopiedOutAndClassified() {
        val msg = vps + sps + pps
        val r = NalScan.scan(msg, msg.size)
        assertArrayEquals(vps, r.vps); assertArrayEquals(sps, r.sps); assertArrayEquals(pps, r.pps)
        assertTrue(r.sawParamSet); assertFalse(r.sawVcl); assertFalse(r.keyframe)
        // The copies are the caller's: mutating the source buffer afterwards changes nothing.
        msg.fill(0)
        assertArrayEquals(vps, r.vps)
    }

    @Test fun vclAndKeyframeClassification() {
        val r1 = NalScan.scan(p, p.size)
        assertTrue(r1.sawVcl); assertFalse(r1.keyframe); assertFalse(r1.sawParamSet); assertNull(r1.vps)
        val mixed = vps + sps + pps + idr        // an HEVC IDR AU with in-band parameter sets
        val r2 = NalScan.scan(mixed, mixed.size)
        assertTrue(r2.sawParamSet); assertTrue(r2.sawVcl); assertTrue(r2.keyframe)
    }

    /** THE case the reuse introduces: a stale `00 00 01 <VPS>` past len must not become a phantom VPS. */
    @Test fun staleTailBeyondLenIsNeverScanned() {
        val previous = vps + sps + pps                         // last message: parameter sets
        val current = p                                        // this message: one P-slice, shorter
        val buf = ByteArray(previous.size)
        System.arraycopy(previous, 0, buf, 0, previous.size)   // the buffer still holds the old tail
        System.arraycopy(current, 0, buf, 0, current.size)     // the new message overwrites its head
        val bounded = NalScan.scan(buf, current.size)
        val fresh = NalScan.scan(current, current.size)
        assertNull("phantom VPS from the stale tail", bounded.vps); assertNull(bounded.sps); assertNull(bounded.pps)
        assertEquals(fresh.sawParamSet, bounded.sawParamSet)
        assertEquals(fresh.sawVcl, bounded.sawVcl)
        assertEquals(fresh.keyframe, bounded.keyframe)
        // The unbounded scan of the same buffer DOES see the phantom: that is the bug being excluded.
        assertTrue(NalScan.scan(buf, buf.size).sawParamSet)
    }

    @Test fun lastNalEndsAtLenNotBufferSize() {
        val msg = vps + pps
        val buf = msg + byteArrayOf(0, 0, 1, 0x42, 0x7F, 0x7F)   // stale start code + bytes beyond len
        val r = NalScan.scan(buf, msg.size)
        assertArrayEquals("PPS copied up to len, not into the tail", pps, r.pps)
        assertEquals("from inside the VPS: the PPS start", vps.size, NalScan.nextStart(buf, msg.size, 4))
        assertEquals("from inside the PPS: len, not the stale start code past it", msg.size, NalScan.nextStart(buf, msg.size, vps.size + 4))
    }

    @Test fun emptyAndTinyInputs() {
        val r = NalScan.scan(ByteArray(0), 0)
        assertFalse(r.sawParamSet || r.sawVcl || r.keyframe)
        assertFalse(NalScan.scan(byteArrayOf(0, 0, 0, 1), 4).sawVcl)   // start code with no NAL byte
    }
}
