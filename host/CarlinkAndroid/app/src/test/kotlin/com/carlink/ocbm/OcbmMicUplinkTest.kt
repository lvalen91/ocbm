package com.carlink.ocbm

import com.carlink.logging.ProbeLog
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/**
 * The mic uplink half of the OCBM client: the `CT_UPLINK` gate coming down, and `CH_MIC` PCM going
 * up. Headless over [FakeTransport] — no adapter, no emulator.
 *
 * Both directions fail silently on hardware, which is why they are pinned here. A misparsed gate
 * opens capture at the wrong rate and Siri hears a pitch-shifted stream; a torn or reordered PCM
 * buffer reaches Siri as noise. Neither produces an error on the box, on the phone, or in logcat.
 */
class OcbmMicUplinkTest {
    private val log = ProbeLog.silent()
    private var client: OcbmClient? = null

    @After
    fun tearDown() {
        client?.let { runCatching { it.stop() } }
    }

    private fun newClient(t: FakeTransport): OcbmClient =
        OcbmClient(t, log).also {
            client = it
            it.start()
        }

    private fun FakeTransport.deliver(
        channel: Int,
        payload: ByteArray,
    ) = feed(Framing.frame(channel, Ocbm.F_BOTH, 0, payload))

    private fun helloAck(): ByteArray = byteArrayOf(Ocbm.CT_HELLO_ACK, Ocbm.VERSION, 0, 0, 0, 0, Ocbm.MODE_PROJECTION)

    private fun subscribedClient(t: FakeTransport): OcbmClient {
        val c = newClient(t)
        t.deliver(Ocbm.CH_CTRL, helloAck())
        assertTrue(c.subscribe("name: test\n".toByteArray()))
        return c
    }

    /** `[CT_UPLINK][state u8][rate u32 LE][ch u8]` — the box's layout, built by hand. */
    private fun uplink(
        on: Boolean,
        rate: Int,
        ch: Int,
    ): ByteArray =
        byteArrayOf(
            Ocbm.CT_UPLINK,
            if (on) 1 else 0,
            (rate and 0xFF).toByte(),
            ((rate ushr 8) and 0xFF).toByte(),
            ((rate ushr 16) and 0xFF).toByte(),
            ((rate ushr 24) and 0xFF).toByte(),
            ch.toByte(),
        )

    /** Collect every payload the client wrote on [channel], in wire order. */
    private fun FakeTransport.payloadsOn(channel: Int): List<ByteArray> =
        generateSequence { takeWritten(150) }
            .mapNotNull { raw -> Framing.parseHeader(raw, 0, raw.size)?.let { it to raw } }
            .filter { (h, _) -> h.channel == channel }
            .map { (h, raw) -> raw.copyOfRange(Ocbm.HDR_LEN, Ocbm.HDR_LEN + h.length) }
            .toList()

    // ---- the gate coming down --------------------------------------------------------------------

    @Test
    fun `CT_UPLINK on surfaces the negotiated rate and channel count`() {
        val t = FakeTransport()
        val c = newClient(t)
        val seen = mutableListOf<Triple<Boolean, Int, Int>>()
        c.onUplinkGate = { on, rate, ch -> seen += Triple(on, rate, ch) }

        t.deliver(Ocbm.CH_CTRL, uplink(on = true, rate = 16000, ch = 1))

        assertEquals(1, seen.size)
        assertEquals(Triple(true, 16000, 1), seen[0])
        assertTrue(c.uplinkOn)
        assertEquals(16000, c.uplinkRate)
        assertEquals(1, c.uplinkChannels)
    }

    /**
     * Byte-order pin. The rate is little-endian; read big-endian, 16000 becomes 0x803E0000 and
     * capture opens at a rate no table maps — which then falls back to 16 kHz and looks correct
     * right up until it does not.
     */
    @Test
    fun `rate is parsed little-endian`() {
        val t = FakeTransport()
        val c = newClient(t)
        var rate = -1
        c.onUplinkGate = { _, r, _ -> rate = r }

        // 16000 == 0x00003E80 -> LE bytes 80 3E 00 00.
        t.feed(
            Framing.frame(
                Ocbm.CH_CTRL,
                Ocbm.F_BOTH,
                0,
                byteArrayOf(Ocbm.CT_UPLINK, 1, 0x80.toByte(), 0x3E, 0x00, 0x00, 1),
            ),
        )
        assertEquals(16000, rate)
    }

    @Test
    fun `every supported capture format survives the gate`() {
        val t = FakeTransport()
        val c = newClient(t)
        var got: Pair<Int, Int>? = null
        c.onUplinkGate = { _, r, ch -> got = r to ch }

        for ((rate, ch) in listOf(8000 to 1, 16000 to 1, 24000 to 1, 16000 to 2)) {
            t.deliver(Ocbm.CH_CTRL, uplink(on = true, rate = rate, ch = ch))
            assertEquals("${rate}Hz ${ch}ch", rate to ch, got)
        }
    }

    @Test
    fun `CT_UPLINK off clears the negotiated format`() {
        val t = FakeTransport()
        val c = newClient(t)
        t.deliver(Ocbm.CH_CTRL, uplink(on = true, rate = 16000, ch = 2))
        assertTrue(c.uplinkOn)

        var off: Triple<Boolean, Int, Int>? = null
        c.onUplinkGate = { on, r, ch -> off = Triple(on, r, ch) }
        // The box sends the format again on the off edge; it must NOT be retained as live state.
        t.deliver(Ocbm.CH_CTRL, uplink(on = false, rate = 16000, ch = 2))

        assertEquals(Triple(false, 0, 0), off)
        assertFalse(c.uplinkOn)
        assertEquals(0, c.uplinkRate)
        assertEquals(0, c.uplinkChannels)
    }

    /** A truncated gate must be ignored outright, not parsed from whatever bytes are present. */
    @Test
    fun `a short CT_UPLINK is ignored`() {
        val t = FakeTransport()
        val c = newClient(t)
        var fired = false
        c.onUplinkGate = { _, _, _ -> fired = true }

        t.deliver(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_UPLINK, 1, 0x80.toByte(), 0x3E, 0x00, 0x00))

        assertFalse("a 6-byte CT_UPLINK is malformed and must not fire the gate", fired)
        assertFalse(c.uplinkOn)
        assertEquals(0, c.uplinkRate)
    }

    // ---- PCM going up ---------------------------------------------------------------------------

    @Test
    fun `mic PCM is dropped before subscribe`() {
        val t = FakeTransport()
        val c = newClient(t)
        t.deliver(Ocbm.CH_CTRL, helloAck())

        c.sendMicPcm(ByteArray(640) { 0x5A })

        assertNull("no CH_MIC traffic may precede CT_SUBSCRIBE", t.payloadsOn(Ocbm.CH_MIC).firstOrNull())
    }

    @Test
    fun `one tick is one CH_MIC frame carrying the PCM verbatim`() {
        val t = FakeTransport()
        val c = subscribedClient(t)
        val pcm = ByteArray(640) { (it and 0xFF).toByte() }

        assertTrue(c.sendMicPcm(pcm))

        val frames = t.payloadsOn(Ocbm.CH_MIC)
        assertEquals("a 640 B tick must not be split", 1, frames.size)
        assertArrayEquals(pcm, frames[0])
    }

    @Test
    fun `an offset slice sends only that slice`() {
        val t = FakeTransport()
        val c = subscribedClient(t)
        val backing = ByteArray(1024) { (it and 0xFF).toByte() }

        assertTrue(c.sendMicPcm(backing, off = 100, len = 640))

        val frames = t.payloadsOn(Ocbm.CH_MIC)
        assertEquals(1, frames.size)
        assertArrayEquals(backing.copyOfRange(100, 740), frames[0])
    }

    @Test
    fun `an empty buffer sends nothing and still succeeds`() {
        val t = FakeTransport()
        val c = subscribedClient(t)

        assertTrue(c.sendMicPcm(ByteArray(0)))
        assertTrue(c.sendMicPcm(ByteArray(64), off = 0, len = 0))

        assertNull(t.payloadsOn(Ocbm.CH_MIC).firstOrNull())
    }

    // ---- oversize splitting ---------------------------------------------------------------------

    @Test
    fun `exactly MIC_CHUNK stays a single frame`() {
        val t = FakeTransport()
        val c = subscribedClient(t)
        val pcm = ByteArray(Ocbm.MIC_CHUNK) { (it and 0xFF).toByte() }

        assertTrue(c.sendMicPcm(pcm))

        val frames = t.payloadsOn(Ocbm.CH_MIC)
        assertEquals("MIC_CHUNK is the inclusive boundary", 1, frames.size)
        assertArrayEquals(pcm, frames[0])
    }

    @Test
    fun `one byte over MIC_CHUNK splits into two frames`() {
        val t = FakeTransport()
        val c = subscribedClient(t)
        val pcm = ByteArray(Ocbm.MIC_CHUNK + 1) { (it and 0xFF).toByte() }

        assertTrue(c.sendMicPcm(pcm))

        val frames = t.payloadsOn(Ocbm.CH_MIC)
        assertEquals(2, frames.size)
        assertEquals(Ocbm.MIC_CHUNK, frames[0].size)
        assertEquals(1, frames[1].size)
    }

    /**
     * The split must be lossless and ordered. CH_MIC is a boundary-less byte stream, so the box
     * reassembles by concatenation — a dropped or reordered chunk is not a recoverable framing
     * error there, it is permanently corrupt audio.
     */
    @Test
    fun `a large buffer reassembles byte-identically in order`() {
        val t = FakeTransport()
        val c = subscribedClient(t)
        val size = Ocbm.MIC_CHUNK * 3 + 777
        val pcm = ByteArray(size) { ((it * 31) and 0xFF).toByte() }

        assertTrue(c.sendMicPcm(pcm))

        val frames = t.payloadsOn(Ocbm.CH_MIC)
        assertEquals(4, frames.size)
        for (i in 0 until 3) {
            assertEquals("chunk $i must be a full MIC_CHUNK", Ocbm.MIC_CHUNK, frames[i].size)
        }
        assertEquals(777, frames[3].size)

        val rejoined = ByteArray(size)
        var at = 0
        for (f in frames) {
            f.copyInto(rejoined, at)
            at += f.size
        }
        assertArrayEquals("concatenated chunks must equal the original buffer", pcm, rejoined)
    }

    /**
     * Every chunk boundary must be 4-byte aligned, or a split lands mid-frame in 16-bit stereo and
     * every sample after it is channel-swapped for the rest of the stream.
     */
    @Test
    fun `chunk boundaries never bisect a stereo sample frame`() {
        val t = FakeTransport()
        val c = subscribedClient(t)
        // A whole number of stereo frames in, so any misalignment out is the splitter's doing.
        val pcm = ByteArray(Ocbm.MIC_CHUNK * 2 + 1024) { (it and 0xFF).toByte() }

        assertTrue(c.sendMicPcm(pcm))

        val frames = t.payloadsOn(Ocbm.CH_MIC)
        var offset = 0
        for (f in frames.dropLast(1)) {
            offset += f.size
            assertEquals("a chunk boundary at $offset is not 4-byte aligned", 0, offset % 4)
        }
    }

    /**
     * The atomicity claim behind `sendOversize`: one buffer is split under a SINGLE [txLock] hold,
     * so no other sender's frame can land between its chunks. CH_MIC is a boundary-less byte
     * stream, so an interleaved frame is not a recoverable framing error on the box — it is
     * concatenated straight into the audio.
     *
     * The contending sender has to be a BLOCKING one. `sendTouch` and `sendMicPcm` both enqueue
     * onto `txQ` and are drained by the single `ocbm-tx` thread, so they serialise on the queue no
     * matter what the lock does — a touch-based version of this test passes even with the lock
     * removed, which is why it is not the one written here. `setRadios` calls `sendSync` directly
     * from the caller's thread, so it genuinely races the split.
     *
     * [PacingTransport] makes the race deterministic: it stalls each write, so the split holds the
     * lock long enough that a correct implementation must make the contender wait.
     */
    @Test
    fun `a blocking sender cannot interleave into a split mic buffer`() {
        val t = PacingTransport(writeDelayMs = 25)
        val c =
            OcbmClient(t, log).also {
                client = it
                it.start()
            }
        t.deliver(Ocbm.CH_CTRL, helloAck())
        assertTrue(c.subscribe("name: test\n".toByteArray()))
        t.frames.clear()

        val contender =
            Thread {
                // Enter only once the split is demonstrably under way.
                t.firstMicWrite.await(2, TimeUnit.SECONDS)
                repeat(4) { c.setRadios(true) }
            }
        contender.start()
        assertTrue(c.sendMicPcm(ByteArray(Ocbm.MIC_CHUNK * 4) { (it and 0xFF).toByte() }))
        contender.join(5000)

        // Drain the tx thread's remaining work.
        val deadline = System.currentTimeMillis() + 3000
        while (t.channels().count { it == Ocbm.CH_MIC } < 4 && System.currentTimeMillis() < deadline) {
            Thread.sleep(25)
        }

        val channels = t.channels()
        val first = channels.indexOf(Ocbm.CH_MIC)
        assertTrue("the mic buffer should have reached the wire", first >= 0)
        assertEquals(
            "a frame landed between mic chunks — txLock did not span the split",
            List(4) { Ocbm.CH_MIC },
            channels.subList(first, first + 4),
        )
    }

    /**
     * A transport that stalls every write, turning the lock-contention race above into a
     * deterministic one, and records frames in wire order.
     */
    private class PacingTransport(
        private val writeDelayMs: Long,
    ) : RawBulkTransport {
        val frames = java.util.concurrent.ConcurrentLinkedQueue<ByteArray>()
        val firstMicWrite = CountDownLatch(1)
        private var handler: ((ByteArray, Int) -> Unit)? = null
        private var running = false

        override fun writeBulk(data: ByteArray): Boolean {
            frames.add(data.copyOf())
            if (Framing.parseHeader(data, 0, data.size)?.channel == Ocbm.CH_MIC) firstMicWrite.countDown()
            Thread.sleep(writeDelayMs)
            return true
        }

        override fun setReadHandler(handler: (ByteArray, Int) -> Unit) {
            this.handler = handler
        }

        override fun setDeadHandler(handler: () -> Unit) = Unit

        override fun start() {
            running = true
        }

        override fun stop() {
            running = false
        }

        fun deliver(
            channel: Int,
            payload: ByteArray,
        ) {
            val f = Framing.frame(channel, Ocbm.F_BOTH, 0, payload)
            if (running) handler?.invoke(f, f.size)
        }

        /** Channel of every frame written so far, in order. */
        fun channels(): List<Int> = frames.mapNotNull { Framing.parseHeader(it, 0, it.size)?.channel }
    }
}
