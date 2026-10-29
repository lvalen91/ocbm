package zeno.gmccpa

import java.net.DatagramPacket
import java.net.Inet4Address
import java.net.Inet6Address
import java.net.InetAddress
import java.net.MulticastSocket
import java.net.NetworkInterface
import java.net.SocketAddress
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicBoolean
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test

/**
 * C2 (2026-10-29): the self-hosted advert follows br0 instead of snapshotting it at start().
 * [BridgeWatch.decide] is the per-tick policy; [MdnsResponder.refreshIfChanged] / [MdnsResponder.boundTo]
 * are the responder's half. No network for the responder, as in [MdnsResponderStopTest]: its socket
 * is a recording [MulticastSocket], `running` and the advertised pair are set by reflection, and the
 * address re-read is pointed at a real host interface carrying an IPv4 and an IPv6 link-local
 * (skipped where the host has none). NOT a radio or phone test: nothing here touches br0 or iOS.
 */
class MdnsResponderBridgeTest {

    private class CapturingSocket : MulticastSocket(null as SocketAddress?) {
        val sent = CopyOnWriteArrayList<ByteArray>()
        override fun send(p: DatagramPacket) { sent.add(p.data.copyOfRange(p.offset, p.offset + p.length)) }
    }

    private val r = MdnsResponder("gmccpa-test", "_airplay._tcp", 7000, mapOf("srcvers" to "1"), ProbeLog.silent())
    private val sock = CapturingSocket()

    private fun field(name: String) = MdnsResponder::class.java.getDeclaredField(name).apply { isAccessible = true }
    private fun running(): AtomicBoolean = field("running").get(r) as AtomicBoolean
    private fun up(v4: Inet4Address = a4, v6: Inet6Address? = null, nif: NetworkInterface? = null) {
        field("sock").set(r, sock)
        field("advertised").set(r, v4)
        field("advertised6").set(r, v6)
        if (nif != null) { field("nifName").set(r, nif.name); field("nifIndex").set(r, nif.index) }
        running().set(true)
    }
    /** ANCOUNT of a response: PTR+SRV+TXT+A = 4, +1 with an AAAA. */
    private fun answers(p: ByteArray) = ((p[6].toInt() and 0xFF) shl 8) or (p[7].toInt() and 0xFF)
    private fun awaitSent(n: Int) {
        val t0 = System.nanoTime()
        while (sock.sent.size < n && System.nanoTime() - t0 < 5_000_000_000L) Thread.sleep(20)
    }

    /** A host interface with an IPv4 and an IPv6 link-local — what br0 looks like once GM's hotspot is up. */
    private fun dualStackNif(): NetworkInterface? = NetworkInterface.getNetworkInterfaces()?.toList()?.firstOrNull { n ->
        runCatching { n.isUp }.getOrDefault(false) &&
            n.inetAddresses.toList().let { a -> a.any { it is Inet4Address } && a.any { it is Inet6Address && it.isLinkLocalAddress } }
    }
    private fun v4Of(n: NetworkInterface) = n.inetAddresses.toList().filterIsInstance<Inet4Address>().first()
    private fun ll6Of(n: NetworkInterface) = n.inetAddresses.toList().filterIsInstance<Inet6Address>().first { it.isLinkLocalAddress }

    private val a4 = InetAddress.getByName("192.0.2.1") as Inet4Address
    private val ll = InetAddress.getByName("fe80::1") as Inet6Address
    private val br7 = MdnsResponder.Bridge("br0", 7, a4, null)      // A-only, as at a cold start
    private val br7ll = MdnsResponder.Bridge("br0", 7, a4, ll)      // the link-local landed
    private val br9ll = MdnsResponder.Bridge("br0", 9, a4, ll)      // br0 re-created: new ifindex

    // ---- the per-tick policy ----------------------------------------------------------------------

    @Test fun noBridgeDoesNothing_whateverWasTriedOrBound() {
        assertEquals(BridgeWatch.Action.NONE, BridgeWatch.decide(null, false, null))
        assertEquals(BridgeWatch.Action.NONE, BridgeWatch.decide(null, false, br7))
        assertEquals(BridgeWatch.Action.NONE, BridgeWatch.decide(null, true, null))   // responder up: keeps its last-known set
    }

    /** br0 absent at receiver start, up now: start a responder. */
    @Test fun bridgeAppearingAfterStart_starts() {
        assertEquals(BridgeWatch.Action.START, BridgeWatch.decide(br7, false, null))
    }

    /** A start that failed on this exact br0 is not retried until br0 changes (here: its link-local lands) — or the timer says so. */
    @Test fun failedStartIsRetriedOnlyWhenTheBridgeChanges() {
        assertEquals(BridgeWatch.Action.NONE, BridgeWatch.decide(br7, false, br7))
        val sameByValue = MdnsResponder.Bridge("br0", 7, InetAddress.getByName("192.0.2.1") as Inet4Address, null)
        assertEquals("equal by value, not identity", BridgeWatch.Action.NONE, BridgeWatch.decide(sameByValue, false, br7))
        assertEquals(BridgeWatch.Action.START, BridgeWatch.decide(br7ll, false, br7))
    }

    /**
     * Follow-up (2026-10-29): the re-arm timer. On an UNCHANGED br0 a due timer re-attempts a start
     * only while no responder is bound to it; a bound responder is refreshed first, so the timer can
     * never restart a healthy one; no br0 is still nothing; a changed br0 starts with or without it.
     */
    @Test fun retryTimerRestartsOnlyAnUnboundResponder() {
        assertEquals(BridgeWatch.Action.START, BridgeWatch.decide(br7, false, br7, retryDue = true))
        assertEquals(BridgeWatch.Action.START, BridgeWatch.decide(br9ll, false, br9ll, retryDue = true))   // same for a dead responder on a re-created br0
        assertEquals(BridgeWatch.Action.REFRESH, BridgeWatch.decide(br7, true, br7, retryDue = true))
        assertEquals(BridgeWatch.Action.REFRESH, BridgeWatch.decide(br7ll, true, br7, retryDue = true))
        assertEquals(BridgeWatch.Action.NONE, BridgeWatch.decide(null, false, br7, retryDue = true))
        assertEquals(BridgeWatch.Action.START, BridgeWatch.decide(br7ll, false, br7, retryDue = true))
        assertEquals(BridgeWatch.Action.START, BridgeWatch.decide(br7ll, false, br7, retryDue = false))
    }

    /** The timer itself, on a monotonic ms clock: due exactly at the period, not before; a fresh boot never holds the FIRST start. */
    @Test fun retryDueTiming_andTheFirstStartIsNeverHeld() {
        val period = 30_000L
        assertFalse(BridgeWatch.retryDue(100_000L + period - 1, 100_000L, period))
        assertTrue(BridgeWatch.retryDue(100_000L + period, 100_000L, period))
        // Freshly booted device (elapsedRealtime < period), no attempt yet (triedAt = 0): timer not due,
        // but lastTried == null lets the first START through regardless.
        assertFalse(BridgeWatch.retryDue(5_000L, 0L, period))
        assertEquals(BridgeWatch.Action.START, BridgeWatch.decide(br7, false, null, retryDue = false))
        // Long-up device, no attempt yet: timer reads due; still exactly one START, as above.
        assertTrue(BridgeWatch.retryDue(3_600_000L, 0L, period))
        assertEquals(BridgeWatch.Action.START, BridgeWatch.decide(br7, false, null, retryDue = true))
    }

    /** No storm: the loop as the watch runs it (3 s ticks, every START stamps triedAt) makes at most one attempt per period. */
    @Test fun retryCannotStorm_oneAttemptPerPeriod() {
        val period = 30_000L; val tick = 3_000L
        var lastTried: MdnsResponder.Bridge? = null; var triedAt = 0L
        val starts = ArrayList<Long>()
        var t = 7_000L                                   // receiver started at boot +7 s, br7 up but unbindable throughout
        while (t <= 7_000L + 90_000L) {
            val due = BridgeWatch.retryDue(t, triedAt, period)
            if (BridgeWatch.decide(br7, false, lastTried, due) == BridgeWatch.Action.START) { lastTried = br7; triedAt = t; starts.add(t) }
            t += tick
        }
        assertEquals(listOf(7_000L, 37_000L, 67_000L, 97_000L), starts)
    }

    /** Responder up on this very interface: refresh in place — the path for the link-local-later case, and for removal. */
    @Test fun boundBridgeRefreshesInPlace() {
        assertEquals(BridgeWatch.Action.REFRESH, BridgeWatch.decide(br7ll, true, null))
        assertEquals(BridgeWatch.Action.REFRESH, BridgeWatch.decide(br7ll, true, br7ll))
        assertEquals(BridgeWatch.Action.REFRESH, BridgeWatch.decide(br7, true, br7ll))    // link-local removed: still in place
    }

    /** br0 re-created (new ifindex) under a responder: not bound any more → a new responder, once. */
    @Test fun recreatedBridgeStartsANewResponder_once() {
        assertEquals(BridgeWatch.Action.START, BridgeWatch.decide(br9ll, false, br7ll))
        assertEquals(BridgeWatch.Action.NONE, BridgeWatch.decide(br9ll, false, br9ll))  // that start failed: wait for a change
    }

    // ---- the responder's half ---------------------------------------------------------------------

    @Test fun boundToIsByIfindexAndOnlyWhileRunning() {
        val nif = dualStackNif(); assumeTrue("host has no up interface with IPv4 + IPv6 link-local", nif != null)
        up(nif = nif)
        val same = MdnsResponder.Bridge(nif!!.name, nif.index, v4Of(nif), ll6Of(nif))
        assertTrue(r.boundTo(same))
        assertFalse("same name, other ifindex", r.boundTo(same.copy(index = nif.index + 1000)))
        assertFalse(r.boundTo(null))
        running().set(false)
        assertFalse("stopped", r.boundTo(same))
    }

    /** A-only at start (no link-local then), the link-local there now: one announce carrying the AAAA, then nothing until the next change. */
    @Test fun refreshIfChangedAnnouncesTheNewPairOnce() {
        val nif = dualStackNif(); assumeTrue("host has no up interface with IPv4 + IPv6 link-local", nif != null)
        up(v4 = v4Of(nif!!), v6 = null, nif = nif)
        assertFalse(r.hasAaaa)
        assertTrue("changed: the link-local landed", r.refreshIfChanged())
        assertTrue(r.hasAaaa)
        awaitSent(3)
        assertEquals("one announce = three packets", 3, sock.sent.size)
        assertTrue("every packet carries PTR+SRV+TXT+AAAA+A", sock.sent.all { answers(it) == 5 })
        assertFalse("unchanged: nothing to say", r.refreshIfChanged())
        Thread.sleep(300)
        assertEquals(3, sock.sent.size)
    }

    @Test fun refreshIfChangedIsRefusedOnceStopped() {
        val nif = dualStackNif(); assumeTrue("host has no up interface with IPv4 + IPv6 link-local", nif != null)
        up(v4 = a4, v6 = null, nif = nif)
        running().set(false)
        assertFalse(r.refreshIfChanged())
        Thread.sleep(200)
        assertEquals(0, sock.sent.size)
        assertFalse("addresses untouched", r.hasAaaa)
    }

    /** The static look never throws; where a `br*` bridge with an IPv4 exists it is what is returned. */
    @Test fun bridgeLookupIsTotal() {
        val b = MdnsResponder.bridge()
        if (b != null) { assertTrue(b.name, b.name.startsWith("br")); assertTrue(b.index > 0) }
    }
}
