package zeno.gmccpa

import java.net.DatagramPacket
import java.net.Inet4Address
import java.net.InetAddress
import java.net.MulticastSocket
import java.net.SocketAddress
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Plan v5.7 U8 / audit12 F126: once stop() has begun, the only packets the responder sends are its
 * TTL-0 goodbyes. A query answered after the goodbye (TTL 120/4500) re-caches a receiver that is gone.
 *
 * No network: the responder's private socket is replaced by reflection with a [MulticastSocket]
 * that records what it is asked to send, `running` is set by hand (start() needs br0), and the
 * advertised address is TEST-NET-1. handleQuery is driven directly with a PTR query.
 */
class MdnsResponderStopTest {

    private class CapturingSocket : MulticastSocket(null as SocketAddress?) {
        val sent = CopyOnWriteArrayList<ByteArray>()
        @Volatile var gate: CountDownLatch? = null
        val entered = CountDownLatch(1)
        private val calls = java.util.concurrent.atomic.AtomicInteger()
        /** Only the FIRST send is held at [gate]; later ones go straight through. */
        override fun send(p: DatagramPacket) {
            if (calls.getAndIncrement() == 0) { entered.countDown(); gate?.await(5, TimeUnit.SECONDS) }
            sent.add(p.data.copyOfRange(p.offset, p.offset + p.length))
        }
    }

    private val r = MdnsResponder("gmccpa-test", "_airplay._tcp", 7000, mapOf("srcvers" to "1"), ProbeLog.silent())
    private val sock = CapturingSocket()

    private fun field(name: String) = MdnsResponder::class.java.getDeclaredField(name).apply { isAccessible = true }
    private fun up() {
        field("sock").set(r, sock)
        field("advertised").set(r, InetAddress.getByName("192.0.2.1") as Inet4Address)
        (field("running").get(r) as AtomicBoolean).set(true)
    }
    private fun handleQuery(pkt: ByteArray) {
        MdnsResponder::class.java.getDeclaredMethod("handleQuery", ByteArray::class.java, Int::class.javaPrimitiveType)
            .apply { isAccessible = true }.invoke(r, pkt, pkt.size)
    }
    private fun send(pkt: ByteArray, goodbye: Boolean): Boolean =
        MdnsResponder::class.java.getDeclaredMethod("send", ByteArray::class.java, Boolean::class.javaPrimitiveType)
            .apply { isAccessible = true }.invoke(r, pkt, goodbye) as Boolean

    /** A standard PTR query for `_airplay._tcp.local`. */
    private val ptrQuery: ByteArray = run {
        val b = java.io.ByteArrayOutputStream()
        b.write(byteArrayOf(0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0))
        for (l in listOf("_airplay", "_tcp", "local")) { b.write(l.length); b.write(l.toByteArray()) }
        b.write(0)
        b.write(byteArrayOf(0, 12, 0, 1))
        b.toByteArray()
    }

    /** TTL of the first answer record (names are written uncompressed). */
    private fun firstTtl(p: ByteArray): Long {
        var i = 12
        while (p[i].toInt() != 0) i += 1 + (p[i].toInt() and 0xFF)
        i += 1 + 4
        return ((p[i].toLong() and 0xFF) shl 24) or ((p[i + 1].toLong() and 0xFF) shl 16) or
            ((p[i + 2].toLong() and 0xFF) shl 8) or (p[i + 3].toLong() and 0xFF)
    }

    @Test fun answersWhileRunning() {
        up()
        handleQuery(ptrQuery)
        assertEquals(1, sock.sent.size)
        assertEquals(4500L, firstTtl(sock.sent[0]))
    }

    @Test fun queryAfterStopIsNotAnsweredGoodbyesStillGoOut() {
        up()
        r.stop()
        assertEquals("both goodbyes sent", 2, sock.sent.size)
        assertTrue(sock.sent.all { firstTtl(it) == 0L })
        handleQuery(ptrQuery)
        r.reannounce()
        assertEquals("nothing after the goodbyes", 2, sock.sent.size)
    }

    /**
     * The F126 window: stop() has cleared `running` and is in its goodbye/join (the socket is still
     * open), and the loop's last receive hands handleQuery a query. It must not be answered.
     */
    @Test fun queryInsideTheStopWindowIsNotAnswered() {
        up()
        (field("running").get(r) as AtomicBoolean).set(false)
        handleQuery(ptrQuery)
        assertEquals(0, sock.sent.size)
    }

    /** The send gate itself (the check-then-send window inside handleQuery/announce). */
    @Test fun sendGateRefusesNonGoodbyeOnceStopped() {
        up()
        (field("running").get(r) as AtomicBoolean).set(false)
        assertFalse(send(byteArrayOf(1, 2, 3), goodbye = false))
        assertTrue(send(byteArrayOf(1, 2, 3), goodbye = true))
        assertEquals(1, sock.sent.size)
    }

    /**
     * The race: an answer is already inside the socket send when stop() starts. The goodbyes wait for
     * it, so the TTL-0 packets are the last ones out and the departed receiver is not re-cached.
     */
    @Test fun answerInFlightWhenStopStartsLeavesBeforeTheGoodbyes() {
        up()
        val release = CountDownLatch(1)
        sock.gate = release
        val q = Thread { handleQuery(ptrQuery) }.apply { start() }
        assertTrue(sock.entered.await(2, TimeUnit.SECONDS))
        val stopper = Thread { r.stop() }.apply { start() }
        Thread.sleep(400)     // stop() has cleared running and is waiting to send its goodbye
        assertEquals("nothing sent while the answer is held", 0, sock.sent.size)
        release.countDown()
        q.join(3000); stopper.join(5000)
        assertEquals(3, sock.sent.size)
        assertEquals(listOf(4500L, 0L, 0L), sock.sent.map { firstTtl(it) })
    }

    // ---- D-F7 (2026-10-08): announcements run on their own thread and the caller returns at once --

    /**
     * STRUCTURAL, not a wall-clock budget: reannounce() returns before its first packet is sent, the
     * sends happen on a thread that is neither the caller nor the query-answer loop, and there is no
     * trailing sleep after the third packet. start() shares the same announceAsync helper (it cannot
     * run here: it needs br0).
     */
    @Test fun reannounceIsAsyncOffTheLoopThread_andEndsAfterTheLastSend() {
        up()
        val gate = CountDownLatch(1)
        sock.gate = gate
        val senders = CopyOnWriteArrayList<Thread>()
        val recording = object : MulticastSocket(null as SocketAddress?) {
            override fun send(p: DatagramPacket) { senders.add(Thread.currentThread()); sock.send(p) }
        }
        field("sock").set(r, recording)
        val caller = Thread.currentThread()
        r.reannounce()                                   // must return while the first send is held at the gate
        assertTrue("first send entered while the caller already returned", sock.entered.await(2, TimeUnit.SECONDS))
        assertEquals(0, sock.sent.size)                  // held: the caller did not wait for it
        gate.countDown()
        val t0 = System.nanoTime()
        while (sock.sent.size < 3 && System.nanoTime() - t0 < 5_000_000_000L) Thread.sleep(20)
        assertEquals(3, sock.sent.size)
        val sender = senders.first()
        assertTrue(senders.all { it === sender })
        assertFalse("not the caller", sender === caller)
        assertFalse("not the query-answer loop", sender.name == "mdns-responder")
        assertTrue(sender.name, sender.name.startsWith("mdns-announce"))
        // No sleep after the last packet: the announce thread is gone well inside the old 1 s tail.
        sender.join(500)
        assertFalse("announce thread still alive 500 ms after its last send", sender.isAlive)
    }

    /**
     * C3 (2026-10-08), ORDERING only: an async announce whose first packet is in flight under
     * send()'s [sendLock] when stop() runs sees stop() wait for that lock (0 packets while held),
     * then the goodbyes follow the announce packet, and no later announce packet appears — in this
     * harness because the announce loop's own `running` pre-check ends it before its second send
     * (and stop() has nulled `sock` by then). This test does NOT isolate send()'s under-lock
     * `running` re-check: that guard — the one that matters for an announce already past its
     * pre-check — is held by [sendGateRefusesNonGoodbyeOnceStopped], and isolating it from here would
     * need a seam between the pre-check and the lock acquisition that production code does not have.
     */
    @Test fun asyncAnnounceInFlightAtStop_goodbyesFollowIt_andNoLaterAnnouncePacket() {
        up()
        val gate = CountDownLatch(1)
        sock.gate = gate
        r.reannounce()
        assertTrue(sock.entered.await(2, TimeUnit.SECONDS))     // announce holds sendLock mid-send
        val stopper = Thread({ r.stop() }, "test-stopper").apply { start() }
        Thread.sleep(150)                                        // let stop() flip `running` and queue at the lock
        assertEquals("goodbye cannot leave while the announce holds the lock", 0, sock.sent.size)
        gate.countDown()
        stopper.join(5_000)
        assertFalse(stopper.isAlive)
        Thread.sleep(2_500)                                      // past the announce's two 1 s gaps
        val ttl0 = sock.sent.map { isGoodbye(it) }
        assertEquals("one announce packet, then exactly two goodbyes, nothing after (ordering; see KDoc)", listOf(false, true, true), ttl0)
    }

    /** A goodbye carries TTL 0 on every record; an announce carries TTL 120/4500. Looks at the first answer's TTL. */
    private fun isGoodbye(pkt: ByteArray): Boolean {
        // DNS header 12 B, then the first answer: name (compressed or labels), type(2), class(2), ttl(4).
        var i = 12
        while (i < pkt.size) {
            val l = pkt[i].toInt() and 0xFF
            if (l == 0) { i++; break }
            if (l and 0xC0 == 0xC0) { i += 2; break }
            i += 1 + l
        }
        i += 4
        val ttl = ((pkt[i].toInt() and 0xFF) shl 24) or ((pkt[i + 1].toInt() and 0xFF) shl 16) or
            ((pkt[i + 2].toInt() and 0xFF) shl 8) or (pkt[i + 3].toInt() and 0xFF)
        return ttl == 0
    }
}
