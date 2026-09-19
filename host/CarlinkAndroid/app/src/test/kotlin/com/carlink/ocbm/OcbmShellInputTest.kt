package com.carlink.ocbm

import com.carlink.logging.ProbeLog
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The WP1 "native shell" additions to the client, headless over [FakeTransport]:
 * CT_PROJ_MODE / CT_BOX_HEALTH are parsed and surfaced, the Siri hold pair goes out in order, and
 * INPUT_TELEPHONY has the exact two-byte shape the box's parser expects.
 */
class OcbmShellInputTest {
    private val log = ProbeLog.silent()
    private var client: OcbmClient? = null

    @After
    fun tearDown() {
        client?.let { runCatching { it.stop() } }
    }

    private fun FakeTransport.deliver(
        channel: Int,
        payload: ByteArray,
    ) = feed(Framing.frame(channel, Ocbm.F_BOTH, 0, payload))

    private fun helloAck(): ByteArray = byteArrayOf(Ocbm.CT_HELLO_ACK, Ocbm.VERSION, Ocbm.CAP_MFI.toByte(), 0, 0, 0, Ocbm.MODE_PROJECTION)

    private fun FakeTransport.awaitFrameOn(
        channel: Int,
        ms: Long = 2000,
    ): ByteArray? {
        val deadline = System.currentTimeMillis() + ms
        while (System.currentTimeMillis() < deadline) {
            val raw = takeWritten(100)
            val h = raw?.let { Framing.parseHeader(it, 0, it.size) }
            if (raw != null && h != null && h.channel == channel) {
                return raw.copyOfRange(Ocbm.HDR_LEN, Ocbm.HDR_LEN + h.length)
            }
        }
        return null
    }

    private fun subscribed(t: FakeTransport): OcbmClient {
        val c = OcbmClient(t, log).also { client = it }
        c.start()
        t.deliver(Ocbm.CH_CTRL, helloAck())
        assertTrue(c.helloAcked)
        assertTrue(c.subscribe("name: test\n".toByteArray()))
        return c
    }

    @Test
    fun projModeAndBoxHealthAreParsedAndSurfaced() {
        val t = FakeTransport()
        val c = subscribed(t)
        var mode: Byte? = null
        var health: Int? = null
        c.onProjMode = { mode = it }
        c.onBoxHealth = { health = it }

        assertEquals(Ocbm.PM_NONE, c.lastProjMode)
        assertFalse("all-zero before the first HEALTH is unknown, not dead", c.boxHealthKnown)

        t.deliver(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_PROJ_MODE, Ocbm.PM_WIRELESS_CP))
        assertEquals(Ocbm.PM_WIRELESS_CP, mode)
        assertEquals(Ocbm.PM_WIRELESS_CP, c.lastProjMode)

        val bits = Ocbm.BH_HCI_PRESENT or Ocbm.BH_IAP2D or Ocbm.BH_AIRPLAYD or 0x80 // an unknown high bit must survive
        t.deliver(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_BOX_HEALTH, bits.toByte()))
        assertEquals(bits, health)
        assertEquals(bits, c.lastBoxHealth)
        assertTrue(c.boxHealthKnown)
        assertEquals("HCI|iap2d|airplayd", Ocbm.bhString(bits))

        // A truncated frame is ignored, not misread as PM_NONE / health 0.
        t.deliver(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_PROJ_MODE))
        t.deliver(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_BOX_HEALTH))
        assertEquals(Ocbm.PM_WIRELESS_CP, c.lastProjMode)
        assertEquals(bits, c.lastBoxHealth)
    }

    @Test
    fun siriPressIsTheHoldPairInWireOrder() {
        val t = FakeTransport()
        val c = subscribed(t)
        assertTrue(c.sendSiriPress())
        assertArrayEquals(byteArrayOf(Ocbm.INPUT_COMMAND, Ocbm.CMD_SIRI_DOWN), t.awaitFrameOn(Ocbm.CH_INPUT)!!)
        assertArrayEquals(byteArrayOf(Ocbm.INPUT_COMMAND, Ocbm.CMD_SIRI_UP), t.awaitFrameOn(Ocbm.CH_INPUT)!!)
        assertNull("exactly two frames — never the deprecated bare requestSiri", t.awaitFrameOn(Ocbm.CH_INPUT, 200))
    }

    @Test
    fun telephonyIsOpcodePlusIndex() {
        val t = FakeTransport()
        val c = subscribed(t)
        assertTrue(c.sendTelephony(Ocbm.TEL_ANSWER))
        assertArrayEquals(byteArrayOf(Ocbm.INPUT_TELEPHONY, Ocbm.TEL_ANSWER), t.awaitFrameOn(Ocbm.CH_INPUT)!!)
        assertTrue(c.sendTelephony((Ocbm.TEL_DIGIT0 + 7).toByte()))
        assertArrayEquals(byteArrayOf(0x08, 12), t.awaitFrameOn(Ocbm.CH_INPUT)!!)
    }

    @Test
    fun inputsAreDroppedNotQueuedBeforeSubscribe() {
        val t = FakeTransport()
        val c = OcbmClient(t, log).also { client = it }
        c.start()
        assertFalse(c.sendSiriPress())
        assertFalse(c.sendTelephony(Ocbm.TEL_END))
        assertNull(t.awaitFrameOn(Ocbm.CH_INPUT, 200))
    }
}
