package zeno.gmccpa.av

import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/** plan v5.9 MIC (rev b R24, rev c C7-C9): the pure arm slot read by MicUplink's capture thread. */
class MicInjectTest {

    private val logs = mutableListOf<String>()
    private val C = MicInject.CHUNK_BYTES
    private val A = Any()
    private val B = Any()

    @Before fun setUp() {
        MicInject.clear(); logs.clear()
        MicInject.log = { logs += it }
        MicInject.clock = { throw AssertionError("clock read") }
    }
    @After fun tearDown() { MicInject.clear(); MicInject.log = {} ; MicInject.clock = { System.nanoTime() / 1_000_000L } }

    private fun pcm(n: Int) = ByteArray(n) { (it % 120 + 1).toByte() }   // never zero
    private fun next(o: Any, now: Long = 0, rate: Int = 16000, ch: Int = 1, len: Int = C, tag: Int = 1) =
        MicInject.nextChunk(o, tag, rate, ch, len, now)
    private fun drain(o: Any, tag: Int = 1): List<ByteArray> {
        val out = mutableListOf<ByteArray>()
        while (true) { out += next(o, tag = tag) ?: break; check(out.size < 100_000) }
        return out
    }
    private fun zero(b: ByteArray) = b.all { it.toInt() == 0 }

    @Test fun idleIsOneAtomicReadNoClock() {
        assertNull(MicInject.nextChunk(A, 1, 16000, 1, C))   // clock throws if read
        assertFalse(MicInject.isArmed())
    }

    @Test fun chunkCountIsCeilOfLeadClipTail() {
        MicInject.arm(pcm(32_000), 700, 1500, 60, now = 0)        // 1000 + 700 + 1500 = 3200 ms
        assertEquals(160, drain(A).size)
        MicInject.arm(pcm(6_410), 30, 10, 60, now = 0)            // 960 + 6410 + 320 = 7690 B
        assertEquals((7690 + C - 1) / C, drain(A).size)
    }

    @Test fun leadAndTailAreZeroLastClipChunkPadded() {
        val p = pcm(6_410)                                         // 10 chunks + 10 bytes
        MicInject.arm(p, 40, 0, 60, now = 0)                       // lead = 2 chunks exactly
        val ch = drain(A)
        assertEquals(2 + 11, ch.size)
        assertTrue(zero(ch[0]) && zero(ch[1]))
        for (k in 0 until 10) assertArrayEquals(p.copyOfRange(k * C, (k + 1) * C), ch[2 + k])
        val last = ch[12]
        assertArrayEquals(p.copyOfRange(6400, 6410), last.copyOfRange(0, 10))
        assertTrue(zero(last.copyOfRange(10, C)))
        MicInject.arm(p, 0, 100, 60, now = 0)                      // tail 3200 B
        val t = drain(A)
        assertEquals((6410 + 3200 + C - 1) / C, t.size)
        assertTrue(zero(t.last()))
        assertTrue(t.drop(11).all { zero(it) })
    }

    @Test fun startsOnlyOn16000Mono() {
        MicInject.arm(pcm(6_400), 0, 0, 60, now = 0)
        assertNull(next(A, rate = 8000, len = 320))
        assertNull(next(A, rate = 8000, len = 320))
        assertNull(next(B, ch = 2, len = 1280, tag = 2))
        assertNull(next(B, len = 1280, tag = 2))                   // right rate/ch, wrong chunk size
        assertEquals(listOf(
            "mic_inject: gate is 8000 1, clip is 16000 1 — not consumed (capture #1)",
            "mic_inject: gate is 16000 2, clip is 16000 1 — not consumed (capture #2)"), logs)   // once per capture
        assertTrue(MicInject.isArmed())
        assertNotNull(next(Any(), tag = 3))
        assertTrue(logs.last().contains("clip 1 started on capture #3"))
    }

    /** rev c C7: arm while capture A is already delivering → starts at A's next chunk, lead first. */
    @Test fun armMidCaptureStartsAtNextChunkAndBindsToIt() {
        repeat(5) { assertNull(next(A, tag = 7)) }
        val p = pcm(6_400)
        MicInject.arm(p, 20, 0, 60, now = 0)                      // one lead chunk
        val first = next(A, tag = 7)!!
        assertTrue(zero(first))
        assertEquals("mic_inject: clip 1 started on capture #7", logs.single())
        assertArrayEquals(p.copyOfRange(0, C), next(A, tag = 7))
        assertNull(next(B, tag = 8))                              // bound to A: never B
        assertFalse(MicInject.isArmed())
        assertTrue(logs.last().contains("clip 1 discarded (capture #7 ended at chunk 2)"))
    }

    /** rev c C8: identity, not the per-instance counter — a new object with the SAME number is not A. */
    @Test fun boundClipNeverResumesOnAnotherObjectEvenWithSameNumber() {
        MicInject.arm(pcm(32_000), 0, 0, 60, now = 0)
        repeat(3) { assertNotNull(next(A, tag = 1)) }
        val bNewInstance = Any()                                   // next CPA generation: capture #1 again
        assertNull(next(bNewInstance, tag = 1))
        assertNull(next(A, tag = 1))                               // the remainder is gone for A too
        assertFalse(MicInject.isArmed())
    }

    /** rev c C9: one clip only — a second arm replaces a pending one and a running one. */
    @Test fun secondArmReplacesPendingAndRunning() {
        val p1 = pcm(6_400); val p2 = ByteArray(6_400) { 9 }
        MicInject.arm(p1, 0, 0, 60, now = 0)
        MicInject.arm(p2, 0, 0, 60, now = 0)                      // pending replaced
        assertArrayEquals(p2.copyOfRange(0, C), next(A))
        val p3 = ByteArray(6_400) { 33 }
        MicInject.arm(p3, 0, 0, 60, now = 0)                      // running replaced
        assertArrayEquals(p3.copyOfRange(0, C), next(A, tag = 1))
        assertEquals(9, drain(A).size)                             // 10 chunks total for p3
        assertNull(next(A))
    }

    @Test fun expiryHonouredForUnstartedOnly() {
        MicInject.arm(pcm(6_400), 0, 0, 5, now = 1_000)            // expires at 6000
        assertNull(next(A, now = 6_000))
        assertEquals("mic_inject: expired", logs.single())
        assertFalse(MicInject.isArmed())
        MicInject.arm(pcm(6_400), 0, 0, 5, now = 1_000)
        assertNotNull(next(A, now = 5_999))                        // started before expiry
        assertNotNull(next(A, now = 60_000))                       // bound: not cut by the expiry
    }

    @Test fun rearmReplacesQueueAndClearEmptiesIt() {
        MicInject.arm(pcm(6_400), 0, 0, 60, now = 0)
        assertTrue(MicInject.clear())
        assertNull(next(A))
        assertFalse(MicInject.clear())
    }

    @Test fun exhaustionRevertsToRealMic() {
        MicInject.arm(pcm(6_400), 0, 0, 60, now = 0)
        assertEquals(10, drain(A).size)
        assertTrue(logs.last().contains("clip 1 done (10 chunks) — real mic from the next chunk"))
        assertFalse(MicInject.isArmed())
        assertNull(next(A)); assertNull(next(B))
    }

    @Test fun wrapperReadsClockOnlyWhenArmed() {
        var reads = 0
        MicInject.clock = { reads++; 0L }
        assertNull(MicInject.nextChunk(A, 1, 16000, 1, C))
        assertEquals(0, reads)
        MicInject.arm(pcm(6_400), 0, 0, 60)
        assertNotNull(MicInject.nextChunk(A, 1, 16000, 1, C))
        assertEquals(2, reads)                                     // arm + one chunk
    }
}
