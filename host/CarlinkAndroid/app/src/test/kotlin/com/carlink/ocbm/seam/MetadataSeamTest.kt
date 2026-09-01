package com.carlink.ocbm.seam

import com.carlink.logging.ProbeLog
import com.carlink.ocbm.Ocbm
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class MetadataSeamTest {
    private val log = ProbeLog.silent()

    private fun be32(v: Int) =
        byteArrayOf(
            ((v ushr 24) and 0xFF).toByte(),
            ((v ushr 16) and 0xFF).toByte(),
            ((v ushr 8) and 0xFF).toByte(),
            (v and 0xFF).toByte(),
        )

    /** `[u32 BE "META"][u32 BE len][marker][payload]`, len = 1 + payload.size. */
    private fun msg(
        marker: Int,
        payload: ByteArray,
    ) = Ocbm.META_SEAM_MAGIC + be32(1 + payload.size) + byteArrayOf(marker.toByte()) + payload

    private class Sink {
        val got = ArrayList<Pair<Int, ByteArray>>()

        fun accept(
            m: Int,
            p: ByteArray,
        ) {
            got.add(m to p)
        }
    }

    @Test
    fun `delivers a JSON message and an artwork message in order`() {
        val sink = Sink()
        val seam = MetadataSeam(log, sink::accept)
        val json = """{"kind":"nowPlaying","title":"x"}""".toByteArray()
        val art = byteArrayOf(7) + ByteArray(32) { it.toByte() }

        seam.feed(msg(Ocbm.META_JSON, json) + msg(Ocbm.META_ARTWORK, art))

        assertEquals(2, sink.got.size)
        assertEquals(Ocbm.META_JSON, sink.got[0].first)
        assertArrayEquals(json, sink.got[0].second)
        assertEquals(Ocbm.META_ARTWORK, sink.got[1].first)
        assertArrayEquals(art, sink.got[1].second)
        assertEquals(2L, seam.messagesOut.get())
        assertEquals(0L, seam.resyncBytes.get())
    }

    @Test
    fun `a message split across three feeds is reassembled`() {
        val sink = Sink()
        val seam = MetadataSeam(log, sink::accept)
        val payload = ByteArray(300) { (it % 251).toByte() }
        val m = msg(Ocbm.META_CMD, payload)

        seam.feed(m.copyOfRange(0, 3)) // mid-magic
        seam.feed(m.copyOfRange(3, 40)) // mid-payload
        assertEquals(0, sink.got.size)
        seam.feed(m.copyOfRange(40, m.size))

        assertEquals(1, sink.got.size)
        assertArrayEquals(payload, sink.got[0].second)
    }

    @Test
    fun `junk before the magic is resynced past, not dropped as a message`() {
        val sink = Sink()
        val seam = MetadataSeam(log, sink::accept)
        val payload = "hello".toByteArray()

        seam.feed(byteArrayOf(0x00, 0x11, 0x22) + msg(Ocbm.META_JSON, payload))

        assertEquals(1, sink.got.size)
        assertArrayEquals(payload, sink.got[0].second)
        assertEquals(3L, seam.resyncBytes.get())
    }

    /**
     * A payload that happens to contain the bytes "META" must not be mistaken for a header. The length
     * that follows it is arbitrary data, so the guard is the implausible-length check.
     */
    @Test
    fun `a coincidental magic with an implausible length resyncs instead of hanging`() {
        val sink = Sink()
        val seam = MetadataSeam(log, sink::accept)
        // "META" followed by 0x7FFFFFFF — far over MAX_MESSAGE.
        seam.feed(Ocbm.META_SEAM_MAGIC + byteArrayOf(0x7F, 0xFF.toByte(), 0xFF.toByte(), 0xFF.toByte()))
        seam.feed(msg(Ocbm.META_JSON, "ok".toByteArray()))

        assertEquals(1, sink.got.size)
        assertArrayEquals("ok".toByteArray(), sink.got[0].second)
        assertTrue(seam.resyncBytes.get() > 0)
    }

    @Test
    fun `a marker-only message is consumed but not delivered`() {
        val sink = Sink()
        val seam = MetadataSeam(log, sink::accept)
        seam.feed(msg(Ocbm.META_JSON, ByteArray(0)) + msg(Ocbm.META_JSON, "after".toByteArray()))

        assertEquals(1, sink.got.size)
        assertArrayEquals("after".toByteArray(), sink.got[0].second)
    }

    /** A consumer fault must not propagate into the OCBM read thread and kill every other channel. */
    @Test
    fun `a throwing consumer does not stop the seam`() {
        var seen = 0
        val seam =
            MetadataSeam(log) { _, _ ->
                seen++
                if (seen == 1) throw IllegalStateException("boom")
            }
        seam.feed(msg(Ocbm.META_JSON, "a".toByteArray()) + msg(Ocbm.META_JSON, "b".toByteArray()))
        assertEquals(2, seen)
    }
}
