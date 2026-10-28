package zeno.gmccpa.av

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.ByteArrayOutputStream

/** plan v5.9 MIC: WavPcm accepts the macOS `say` 16 kHz mono file and rejects everything else. */
class WavPcmTest {

    /**
     * The first 64 bytes of a REAL `say -v Samantha -o say_probe.wav --file-format=WAVE
     * --data-format=LEI16@16000 --channels=1 "Call six one one"` file, generated 2026-10-08 on macOS
     * 27.2 (`xxd -l 64`; kept in /tmp/ccpa_sess/mic_inject/fixture/). RIFF size 0xbca2 = 48290;
     * `fmt ` 16: format 1, 1 ch, 16000 Hz, byteRate 32000, blockAlign 2, 16 bit; then `FLLR` size
     * 0x0fcc = 4044 whose body is all zero in that file; `data` (0xacaa = 44202) starts at 4088.
     */
    private val sayHdr64 = hex(
        "52494646a2bc000057415645666d74201000000001000100803e0000007d0000" +
        "02001000464c4c52cc0f00000000000000000000000000000000000000000000")

    private fun hex(s: String) = ByteArray(s.length / 2) { s.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
    private fun le16(o: ByteArrayOutputStream, v: Int) { o.write(v and 0xFF); o.write((v ushr 8) and 0xFF) }
    private fun le32(o: ByteArrayOutputStream, v: Long) { for (i in 0 until 4) o.write(((v ushr (8 * i)) and 0xFF).toInt()) }
    private fun pcm(n: Int) = ByteArray(n) { (it * 7 + 3).toByte() }

    /** The real say file: its own 64 header bytes, FLLR's zero body, the real data header, PCM. */
    private fun sayFile(): ByteArray {
        val o = ByteArrayOutputStream()
        o.write(sayHdr64)
        o.write(ByteArray(4088 - 64))
        o.write("data".toByteArray()); le32(o, 44202)
        o.write(pcm(44202))
        return o.toByteArray()
    }

    private fun wav(
        format: Int = 1, ch: Int = 1, rate: Long = 16000, bits: Int = 16, blockAlign: Int = 2,
        dataLen: Int = 32000, declared: Long = dataLen.toLong(), before: List<Pair<String, Int>> = emptyList(),
        withData: Boolean = true, fmtSize: Int = 16,
    ): ByteArray {
        val body = ByteArrayOutputStream()
        body.write("WAVE".toByteArray())
        body.write("fmt ".toByteArray()); le32(body, fmtSize.toLong())
        le16(body, format); le16(body, ch); le32(body, rate); le32(body, rate * blockAlign); le16(body, blockAlign); le16(body, bits)
        repeat(fmtSize - 16) { body.write(0) }
        for ((id, size) in before) {
            body.write(id.toByteArray()); le32(body, size.toLong())
            body.write(ByteArray(size) { 0x5A }); if (size and 1 == 1) body.write(0)
        }
        if (withData) { body.write("data".toByteArray()); le32(body, declared); body.write(pcm(dataLen)) }
        val o = ByteArrayOutputStream()
        o.write("RIFF".toByteArray()); le32(o, body.size().toLong()); o.write(body.toByteArray())
        return o.toByteArray()
    }

    private fun bad(b: ByteArray, contains: String) {
        val r = WavPcm.parse(b)
        assertTrue("expected Bad, got $r", r is WavPcm.Result.Bad)
        val why = (r as WavPcm.Result.Bad).reason
        assertTrue("reason '$why' should mention '$contains'", why.contains(contains))
    }

    @Test fun realSayHeaderIsAccepted() {
        val f = sayFile()
        assertEquals(48298, f.size)
        val r = WavPcm.parse(f)
        assertTrue("$r", r is WavPcm.Result.Ok)
        r as WavPcm.Result.Ok
        assertEquals(1, r.formatTag)
        assertEquals(44202, r.pcm.size)
        assertArrayEquals(pcm(44202), r.pcm)
        assertEquals(1381, r.durationMs)
    }

    @Test fun plainPcmAccepted() { assertTrue(WavPcm.parse(wav()) is WavPcm.Result.Ok) }
    @Test fun rejectsStereo() = bad(wav(ch = 2, blockAlign = 4), "2 channels")
    @Test fun rejects44k() = bad(wav(rate = 44100), "44100 Hz")
    @Test fun rejects8bit() = bad(wav(bits = 8, blockAlign = 1), "8 bits")
    @Test fun rejectsFloat() = bad(wav(format = 3, bits = 32, blockAlign = 4), "audioFormat 3")
    @Test fun rejectsExtensible() = bad(wav(format = 0xFFFE, fmtSize = 40), "EXTENSIBLE")
    @Test fun rejectsMissingData() = bad(wav(withData = false), "no data chunk")
    @Test fun rejectsTruncatedData() = bad(wav(dataLen = 32000, declared = 32002), "exceeds the file")
    @Test fun rejectsOddData() = bad(wav(dataLen = 32001), "odd")
    @Test fun rejectsOver30s() = bad(wav(dataLen = 960_002), "longer than 30 s")
    @Test fun rejectsUnder200ms() = bad(wav(dataLen = 6_398), "shorter than 200 ms")
    @Test fun boundsInclusive() {
        assertTrue(WavPcm.parse(wav(dataLen = 6_400)) is WavPcm.Result.Ok)
        assertTrue(WavPcm.parse(wav(dataLen = 960_000)) is WavPcm.Result.Ok)
    }
    @Test fun rejectsNotRiff() = bad(ByteArray(64), "not a RIFF/WAVE")

    /** rev b R30: an odd-size chunk is followed by a pad byte; the walk must skip it to find `data`. */
    @Test fun oddListChunkWithPadIsAccepted() {
        val r = WavPcm.parse(wav(before = listOf("LIST" to 25, "FLLR" to 4044)))
        assertTrue("$r", r is WavPcm.Result.Ok)
        assertArrayEquals(pcm(32000), (r as WavPcm.Result.Ok).pcm)
    }
}
