package zeno.gmccpa.av

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.DataOutputStream
import java.util.concurrent.atomic.AtomicLong

/**
 * Review round 1: the production :9003 reader, bytes → [VoiceStreams], on the rig in
 * VoiceStreamsTest.kt. The frames are built with the same layout as forward.rs `tag_voice_v2` /
 * `voice_begin_v2` / `voice_end_v2` (VoiceFrameTest pins those bytes against the rust golden vectors).
 */
class VoiceReaderTest {

    private fun head(o: DataOutputStream, rate: Int, ch: Int, atype: Int, len: Int) {
        o.writeInt(rate); o.writeShort(ch); o.writeByte(atype); o.writeInt(len)
    }

    private fun begin(o: DataOutputStream, scid: Long, st: Int, at: Int, rate: Int, ch: Int, lat: Int, seq: Int = 0) {
        head(o, rate, ch, 0xFF, VoiceFrame.BEGIN_LEN)
        o.writeByte(VoiceFrame.KIND_BEGIN); o.writeLong(1); o.writeLong(scid); o.writeByte(st); o.writeByte(at)
        o.writeByte(VoiceFrame.CODEC_AAC_ELD); o.writeInt(rate); o.writeShort(ch); o.writeInt(lat); o.writeByte(0); o.writeInt(seq)
    }

    private fun au(o: DataOutputStream, scid: Long, rx: Long, seq: Int, ts: Long, st: Int, at: Int, rate: Int, ch: Int, payload: ByteArray) {
        head(o, rate, ch, at or 0x80, VoiceFrame.V2_EXT_LEN + payload.size)
        o.writeLong(scid); o.writeLong(rx); o.writeShort(seq); o.writeInt(ts.toInt()); o.writeByte(st); o.write(payload)
    }

    private fun end(o: DataOutputStream, scid: Long, rate: Int, ch: Int, frames: Int, reason: Int) {
        head(o, rate, ch, 0xFF, VoiceFrame.END_LEN)
        o.writeByte(VoiceFrame.KIND_END); o.writeLong(2); o.writeLong(scid); o.writeByte(reason)
        o.writeInt(frames); o.writeInt(0); o.writeInt(0); o.writeInt(30)
    }

    private fun run(r: VoiceRig, bytes: ByteArray, bytesIn: AtomicLong = AtomicLong()): String =
        VoiceReader(r.core, r.clock, r.log, { true }, bytesIn).run(ByteArrayInputStream(bytes), r.gen)

    @Test fun beginAusEndReachTheEngine() {
        val r = VoiceRig()
        val b = ByteArrayOutputStream(); val o = DataOutputStream(b)
        begin(o, 0x42, 100, VoiceLatency.AT_SPEECH, 16000, 1, 80)
        for (k in 0..2) au(o, 0x42, r.clock.us, k, k * 480L, 100, 2, 16000, 1, byteArrayOf(1, 2, 3))
        val bytesIn = AtomicLong()
        val why = run(r, b.toByteArray(), bytesIn)
        assertEquals("voice seam connection ended", why)
        val s = r.stream(0x42)!!
        assertEquals(VoiceLatency.Purpose.ASSISTANT, s.purpose)
        assertEquals(3L, s.read)
        assertEquals(80, s.targetMs)
        assertEquals("every byte counted", b.size().toLong(), bytesIn.get())
        r.run(300)
        assertEquals("the 23-byte extension stripped: the codec saw 3 AUs", 3, (r.factory.built.single().codec as FakeCodec).queued)
        val e = ByteArrayOutputStream(); end(DataOutputStream(e), 0x42, 16000, 1, 3, VoiceFrame.END_TEARDOWN)
        run(r, e.toByteArray())
        r.run(600)
        assertTrue(r.log.has("identity OK"))
    }

    @Test fun controlFramesPassWhateverTheirRateAndDesyncsReturnTheirReason() {
        val r = VoiceRig()
        val b = ByteArrayOutputStream(); val o = DataOutputStream(b)
        begin(o, 0x43, 101, VoiceLatency.AT_DEFAULT, 0, 0, -1)          // rate 0: still a control frame
        head(o, 1469658167, 232, 2, 4); o.writeInt(0)                  // the fwd-enc misread
        val why = run(r, b.toByteArray())
        assertTrue(why, why.contains("implausible rate 1469658167Hz"))
        assertTrue("the BEGIN before it was registered", r.stream(0x43) != null)
        val b2 = ByteArrayOutputStream(); head(DataOutputStream(b2), 16000, 1, 2, (1 shl 20) + 1)
        assertTrue(run(r, b2.toByteArray()).startsWith("voice desync: len="))
    }

    @Test fun v1AusAreDroppedAndCountedOnce() {
        val r = VoiceRig()
        val b = ByteArrayOutputStream(); val o = DataOutputStream(b)
        repeat(3) { head(o, 16000, 1, 2, 4); o.writeInt(7) }
        run(r, b.toByteArray())
        assertEquals(3L, r.core.v1Dropped)
        assertEquals(1, r.log.lines.count { it.startsWith("E ") && it.contains("v1-tagged AU") })
    }

    @Test fun aTruncatedFrameEndsTheConnection() {
        val r = VoiceRig()
        val b = ByteArrayOutputStream(); val o = DataOutputStream(b)
        head(o, 16000, 1, 0x82, 40); o.write(ByteArray(10))
        assertEquals("voice seam connection ended mid-frame", run(r, b.toByteArray()))
    }
}
