package com.carlink.ocbm

import com.carlink.logging.ProbeLog
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Headless tests for the OCBM client over [FakeTransport] — no adapter, no emulator.
 *
 * The two properties worth protecting here are the ones that only bite on hardware: A/V must not reach
 * a decoder before the session is keyed, and an uplink frame's bytes must match the box's parser
 * exactly, because a wrong byte there is a silent no-op rather than an error.
 */
class OcbmClientTest {
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

    /** Deliver one framed OCBM frame as though the box had sent it. */
    private fun FakeTransport.deliver(
        channel: Int,
        payload: ByteArray,
    ) = feed(Framing.frame(channel, Ocbm.F_BOTH, 0, payload))

    /** `[CT_HELLO_ACK][ver][caps u32 LE][mode]` — enough for handleCtrl to latch helloAcked. */
    private fun helloAck(caps: Int = Ocbm.CAP_MFI): ByteArray =
        byteArrayOf(
            Ocbm.CT_HELLO_ACK,
            Ocbm.VERSION,
            (caps and 0xFF).toByte(),
            ((caps ushr 8) and 0xFF).toByte(),
            ((caps ushr 16) and 0xFF).toByte(),
            ((caps ushr 24) and 0xFF).toByte(),
            Ocbm.MODE_PROJECTION,
        )

    /** Pull frames off the fake wire until one is on [channel], or give up. */
    private fun FakeTransport.awaitFrameOn(
        channel: Int,
        ms: Long = 2000,
    ): ByteArray? {
        val deadline = System.currentTimeMillis() + ms
        while (System.currentTimeMillis() < deadline) {
            val raw = takeWritten(100) ?: continue
            val h = Framing.parseHeader(raw, 0, raw.size) ?: continue
            if (h.channel == channel) return raw.copyOfRange(Ocbm.HDR_LEN, Ocbm.HDR_LEN + h.length)
        }
        return null
    }

    private fun subscribedClient(t: FakeTransport): OcbmClient {
        val c = newClient(t)
        t.deliver(Ocbm.CH_CTRL, helloAck())
        assertTrue("helloAcked should latch from the ACK frame", c.helloAcked)
        assertTrue(c.subscribe("name: test\n".toByteArray()))
        return c
    }

    // ---- the arming guard ------------------------------------------------------------------------

    /**
     * The property that matters most: a decoder must never be fed before the session is keyed. A prior
     * session can leave kernel-buffered A/V queued ahead of the handshake, and feeding that to a
     * renderer means decrypting with a stale key or none at all.
     */
    @Test
    fun `A slash V arriving before SUBSCRIBE is dropped, not routed`() {
        val t = FakeTransport()
        val c = newClient(t)

        t.deliver(Ocbm.CH_VIDEO, ByteArray(64) { 1 })
        t.deliver(Ocbm.CH_MEDIA_AUDIO, ByteArray(32) { 2 })
        t.deliver(Ocbm.CH_METADATA, ByteArray(16) { 3 })

        assertNull("no generation may exist before SUBSCRIBE", c.lanes)
        // and nothing blew up: the frames were counted and discarded
        assertEquals(3L, c.framesIn)
    }

    @Test
    fun `subscribe is refused before CT_HELLO_ACK`() {
        val t = FakeTransport()
        val c = newClient(t)
        assertFalse(c.subscribe("x".toByteArray()))
        assertFalse(c.subscribed)
        assertNull(c.lanes)
    }

    @Test
    fun `lanes are armed on subscribe and retired on HOST_GONE`() {
        val t = FakeTransport()
        val c = subscribedClient(t)
        assertNotNull("SUBSCRIBE arms the generation", c.lanes)

        t.deliver(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_SESSION_EVENT, Ocbm.SEV_HOST_GONE))

        assertNull("HOST_GONE retires the generation", c.lanes)
        assertFalse(c.subscribed)
    }

    @Test
    fun `the subscribe payload is the verb followed by the config verbatim`() {
        val t = FakeTransport()
        val c = newClient(t)
        t.deliver(Ocbm.CH_CTRL, helloAck())
        val cfg = "name: \"x\"\nversion: 1\n".toByteArray()
        assertTrue(c.subscribe(cfg))

        // Skip CT_HELLO if the harness wrote one; find the SUBSCRIBE.
        var payload: ByteArray? = null
        val deadline = System.currentTimeMillis() + 2000
        while (System.currentTimeMillis() < deadline && payload == null) {
            val p = t.awaitFrameOn(Ocbm.CH_CTRL, 200) ?: continue
            if (p.isNotEmpty() && p[0] == Ocbm.CT_SUBSCRIBE) payload = p
        }
        assertNotNull("a CT_SUBSCRIBE must reach the wire", payload)
        assertArrayEquals(byteArrayOf(Ocbm.CT_SUBSCRIBE) + cfg, payload)
    }

    // ---- uplink byte layouts ---------------------------------------------------------------------

    @Test
    fun `sendTouch encodes normalized coordinates little-endian`() {
        val t = FakeTransport()
        val c = subscribedClient(t)
        assertTrue(c.sendTouch(Ocbm.TOUCH_DOWN, 0f, 1f, finger = 2))

        val p = t.awaitFrameOn(Ocbm.CH_INPUT)!!
        // [INPUT_TOUCH][phase][nx lo][nx hi][ny lo][ny hi][finger]; ny = 1.0 -> 65535 = 0xFFFF
        assertArrayEquals(
            byteArrayOf(
                Ocbm.INPUT_TOUCH,
                Ocbm.TOUCH_DOWN,
                0,
                0,
                0xFF.toByte(),
                0xFF.toByte(),
                2,
            ),
            p,
        )
    }

    @Test
    fun `touch coordinates are clamped rather than wrapping`() {
        val t = FakeTransport()
        val c = subscribedClient(t)
        // Out-of-range input must not wrap into a valid-looking coordinate on the far edge.
        c.sendTouch(Ocbm.TOUCH_MOVE, -0.5f, 2.0f)
        val p = t.awaitFrameOn(Ocbm.CH_INPUT)!!
        assertEquals(0, ((p[3].toInt() and 0xFF) shl 8) or (p[2].toInt() and 0xFF))
        assertEquals(65535, ((p[5].toInt() and 0xFF) shl 8) or (p[4].toInt() and 0xFF))
    }

    @Test
    fun `night mode is an INPUT_COMMAND, not a bare command`() {
        val t = FakeTransport()
        val c = subscribedClient(t)
        assertTrue(c.sendNightMode(true))
        assertArrayEquals(
            byteArrayOf(Ocbm.INPUT_COMMAND, Ocbm.CMD_NIGHT_MODE, 1),
            t.awaitFrameOn(Ocbm.CH_INPUT)!!,
        )
    }

    @Test
    fun `media buttons carry the consumer array index`() {
        val t = FakeTransport()
        val c = subscribedClient(t)
        assertTrue(c.sendMediaButton(Ocbm.MEDIA_BTN_NEXT))
        assertArrayEquals(
            byteArrayOf(Ocbm.INPUT_MEDIA_BTN, Ocbm.MEDIA_BTN_NEXT),
            t.awaitFrameOn(Ocbm.CH_INPUT)!!,
        )
    }

    /**
     * The throttle is load-bearing, not decoration: the video seam asks for a keyframe on every gap AND
     * every decrypt failure, so a wrong key would otherwise produce one request per frame.
     */
    @Test
    fun `keyframe requests are throttled`() {
        val t = FakeTransport()
        val c = subscribedClient(t)
        assertTrue("the first request goes out", c.requestKeyframe())
        assertFalse("an immediate second is suppressed", c.requestKeyframe())
        assertFalse(c.requestKeyframe())
        // The alt lane keeps its own counter, so a main gap cannot suppress a cluster gap.
        assertTrue("the alt lane is throttled independently", c.requestAltKeyframe())
    }

    @Test
    fun `mic PCM is split at the chunk size with nothing lost`() {
        val t = FakeTransport()
        val c = subscribedClient(t)
        val pcm = ByteArray(Ocbm.MIC_CHUNK + 100) { (it % 256).toByte() }
        assertTrue(c.sendMicPcm(pcm))

        val first = t.awaitFrameOn(Ocbm.CH_MIC)!!
        val second = t.awaitFrameOn(Ocbm.CH_MIC)!!
        assertEquals(Ocbm.MIC_CHUNK, first.size)
        assertEquals(100, second.size)
        assertArrayEquals(pcm, first + second)
    }

    @Test
    fun `uplink frames are dropped and counted when there is no session`() {
        val t = FakeTransport()
        val c = newClient(t) // never subscribed
        assertFalse(c.sendTouch(Ocbm.TOUCH_DOWN, 0.5f, 0.5f))
        assertFalse(c.sendMediaButton(Ocbm.MEDIA_BTN_PLAY))
        assertNull("nothing may reach the wire", t.awaitFrameOn(Ocbm.CH_INPUT, 300))
    }

    // ---- CTRL surfacing --------------------------------------------------------------------------

    @Test
    fun `CT_UPLINK surfaces the mic gate with its negotiated format`() {
        val t = FakeTransport()
        val c = newClient(t)
        var got: Triple<Boolean, Int, Int>? = null
        c.onUplinkGate = { on, rate, ch -> got = Triple(on, rate, ch) }

        // [CT_UPLINK][state=1][rate u32 LE = 16000][ch=1]
        t.deliver(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_UPLINK, 1, 0x80.toByte(), 0x3E, 0, 0, 1))
        assertEquals(Triple(true, 16000, 1), got)
        assertTrue(c.uplinkOn)
        assertEquals(16000, c.uplinkRate)

        t.deliver(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_UPLINK, 0, 0, 0, 0, 0, 0))
        assertEquals(Triple(false, 0, 0), got)
        assertFalse(c.uplinkOn)
    }

    @Test
    fun `CT_BT_PHASE surfaces handshake progress`() {
        val t = FakeTransport()
        val c = newClient(t)
        val seen = ArrayList<Byte>()
        c.onBtPhase = { seen.add(it) }

        for (p in listOf(Ocbm.BTP_LINK_UP, Ocbm.BTP_AUTHENTICATED, Ocbm.BTP_WIFI_HANDOFF)) {
            t.deliver(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_BT_PHASE, p))
        }
        assertEquals(listOf(Ocbm.BTP_LINK_UP, Ocbm.BTP_AUTHENTICATED, Ocbm.BTP_WIFI_HANDOFF), seen)
        assertEquals(Ocbm.BTP_WIFI_HANDOFF, c.lastBtPhase)
    }

    /** An unknown phase must count as progress, never as an error — the box may add values. */
    @Test
    fun `an unknown BT phase is still delivered`() {
        val t = FakeTransport()
        val c = newClient(t)
        var got: Byte? = null
        c.onBtPhase = { got = it }
        t.deliver(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_BT_PHASE, 0x42))
        assertEquals(0x42.toByte(), got)
    }

    @Test
    fun `phone presence is reported separately from host presence`() {
        val t = FakeTransport()
        val c = newClient(t)
        val phone = ArrayList<Boolean>()
        val host = ArrayList<Boolean>()
        c.onPhonePresence = { phone.add(it) }
        c.onHostPresence = { host.add(it) }

        t.deliver(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_SESSION_EVENT, Ocbm.SEV_PHONE_PRESENT))
        t.deliver(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_SESSION_EVENT, Ocbm.SEV_PHONE_ABSENT))
        t.deliver(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_SESSION_EVENT, Ocbm.SEV_HOST_PRESENT))

        assertEquals(listOf(true, false), phone)
        assertEquals(listOf(true), host)
        assertFalse(c.phonePresent)
    }

    // ---- lifecycle -------------------------------------------------------------------------------

    /**
     * A dead link and a box-side drop must be distinguishable: SEV_HOST_GONE is recoverable and the
     * heartbeat re-subscribes, while a dead transport is not and the session layer has to rebuild.
     *
     * `onLinkDead` is delivered on its own thread, not the detector's — the natural reaction is
     * `stop()`, which joins the heartbeat and tx threads, and the detector is often one of them, so an
     * inline call would join itself. Hence the latch rather than a bare read.
     */
    @Test
    fun `link death fires once and is distinct from HOST_GONE`() {
        val t = FakeTransport()
        val c = subscribedClient(t)
        val deaths =
            java.util.concurrent.atomic
                .AtomicInteger(0)
        val fired = java.util.concurrent.CountDownLatch(1)
        c.onLinkDead = {
            deaths.incrementAndGet()
            fired.countDown()
        }

        t.deliver(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_SESSION_EVENT, Ocbm.SEV_HOST_GONE))
        assertEquals("a box-side drop is not a link death", 0, deaths.get())

        t.killLink()
        t.killLink()
        assertTrue(
            "a real death must be announced",
            fired.await(2, java.util.concurrent.TimeUnit.SECONDS),
        )
        // Give any second (incorrect) announcement time to land before asserting exactly-once.
        Thread.sleep(150)
        assertEquals("and exactly once, however many detectors fire", 1, deaths.get())
    }

    /** The callback must not run on the thread that detected the death — see the note above. */
    @Test
    fun `link death is not announced on the detecting thread`() {
        val t = FakeTransport()
        val c = subscribedClient(t)
        val callbackThread =
            java.util.concurrent.atomic
                .AtomicReference<String>()
        val fired = java.util.concurrent.CountDownLatch(1)
        c.onLinkDead = {
            callbackThread.set(Thread.currentThread().name)
            fired.countDown()
        }

        val killer = Thread.currentThread().name
        t.killLink()

        assertTrue(fired.await(2, java.util.concurrent.TimeUnit.SECONDS))
        assertNotEquals(
            "onLinkDead must not run on the detector's thread — stop() would join itself",
            killer,
            callbackThread.get(),
        )
    }

    @Test
    fun `a stopped client refuses to restart rather than failing silently`() {
        val t = FakeTransport()
        val c = subscribedClient(t)
        c.stop()
        assertTrue(c.stopped)
        assertFalse(c.isRunning)

        c.start() // must be a refusal, not a half-alive client
        assertFalse(c.isRunning)
        assertFalse(c.subscribe("x".toByteArray()))
    }
}
