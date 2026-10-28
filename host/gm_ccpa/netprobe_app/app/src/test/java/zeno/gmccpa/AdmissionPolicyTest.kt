package zeno.gmccpa

import java.net.Inet6Address
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Wave 3, OD-7F / F038: the admission table. Host JVM, no Android. Addresses are documentation /
 * synthetic values (RFC 5737 TEST-NET, fe80::1x), never anything from a capture.
 */
class AdmissionPolicyTest {
    private val loop4 = InetAddress.getByName("127.0.0.1")
    private val loop6 = InetAddress.getByName("::1")
    private val any4 = InetAddress.getByName("0.0.0.0")
    private val br0v4 = InetAddress.getByName("192.0.2.1")     // "this unit's br0 address"
    private val phone4 = InetAddress.getByName("192.0.2.77")
    private val otherIf = InetAddress.getByName("198.51.100.5") // "another of this unit's addresses"
    private fun ll(last: Int, scope: Int): InetAddress {
        val b = ByteArray(16); b[0] = 0xfe.toByte(); b[1] = 0x80.toByte(); b[15] = last.toByte()
        return Inet6Address.getByAddress(null, b, scope)
    }
    private val phoneA = ll(0x11, 7)
    private val phoneB = ll(0x22, 7)            // the phone after a Wi-Fi blip: NEW br0 address
    private val sameBytesOtherScope = ll(0x11, 9)
    private val notOwned: (InetAddress) -> Boolean = { false }
    private val ownsOtherIf: (InetAddress) -> Boolean = { it == otherIf }

    private fun local(peer: InetAddress?, la: InetAddress? = br0v4, owns: (InetAddress) -> Boolean = notOwned) =
        Admission.isLocal(peer, la, owns)

    // ---- row 0: local is always refused --------------------------------------------------------

    @Test fun loopbackV4_noIncumbent_refusedLocal() =
        assertEquals(Admission.Verdict.REFUSE_LOCAL, Admission.decide(local(loop4), false, false, false))

    @Test fun loopbackV6_encryptedIncumbent_refusedLocal_notSessionLive() =
        assertEquals(Admission.Verdict.REFUSE_LOCAL, Admission.decide(local(loop6), true, false, true))

    @Test fun peerEqualsLocalAddress_isLocal_withAndWithoutSession() {
        assertTrue(local(br0v4, la = br0v4))
        assertEquals(Admission.Verdict.REFUSE_LOCAL, Admission.decide(local(br0v4, la = br0v4), false, false, false))
        assertEquals(Admission.Verdict.REFUSE_LOCAL, Admission.decide(local(br0v4, la = br0v4), true, false, true))
    }

    @Test fun peerOwnedByAnotherLocalInterface_isLocal() {
        assertTrue(local(otherIf, la = br0v4, owns = ownsOtherIf))
        assertEquals(Admission.Verdict.REFUSE_LOCAL, Admission.decide(local(otherIf, owns = ownsOtherIf), false, false, false))
    }

    @Test fun lookupThrows_fallsBackToFirstTests() {
        val boom: (InetAddress) -> Boolean = { throw java.net.SocketException("enumeration denied") }
        assertFalse(local(phone4, owns = boom))   // not loopback, not == local -> not local
        assertTrue(local(loop4, owns = boom))     // loopback still caught before the lookup
    }

    @Test fun wildcardAndNullPeer_areLocal() {
        assertTrue(local(any4))
        assertTrue(local(null))
    }

    @Test fun phoneAddresses_areNotLocal() {
        assertFalse(local(phoneA))
        assertFalse(local(phone4))
    }

    // ---- rows 1-3 ------------------------------------------------------------------------------

    @Test fun noIncumbent_allowed() =
        assertEquals(Admission.Verdict.ALLOW_NO_INCUMBENT, Admission.decide(local(phoneA), false, false, false))

    @Test fun closedIncumbent_allowedEvenIfItWasEncrypted() =
        assertEquals(Admission.Verdict.ALLOW_INCUMBENT_CLOSED, Admission.decide(local(phoneB), true, true, true))

    @Test fun notEncryptedIncumbent_neverBlocks_anyNonLocalNewcomer() {
        for (p in listOf(phoneA, phoneB, sameBytesOtherScope, phone4))
            assertEquals(Admission.Verdict.ALLOW_NOT_ENCRYPTED, Admission.decide(local(p), true, false, false))
    }

    @Test fun encryptedIncumbent_refusesEveryNewcomer_noThirdTryEscape() {
        // Same address, a new br0 address, same bytes on another scope, IPv4 — and repeated: no count unlocks it.
        for (p in listOf(phoneA, phoneB, sameBytesOtherScope, phone4)) repeat(5) {
            assertEquals(Admission.Verdict.REFUSE_SESSION_LIVE, Admission.decide(local(p), true, false, true))
        }
    }

    @Test fun verdictFlags() {
        assertTrue(Admission.Verdict.ALLOW_NO_INCUMBENT.allowed)
        assertTrue(Admission.Verdict.ALLOW_INCUMBENT_CLOSED.allowed)
        assertTrue(Admission.Verdict.ALLOW_NOT_ENCRYPTED.allowed)
        assertFalse(Admission.Verdict.REFUSE_LOCAL.allowed)
        assertFalse(Admission.Verdict.REFUSE_SESSION_LIVE.allowed)
        assertEquals("local", Admission.Verdict.REFUSE_LOCAL.rule)
    }

    // ---- the record overload, with the real slot -----------------------------------------------

    @Test fun recordOverload_followsEncryptedAtAndClose_andDeadIncumbentPathReopens() {
        val slot = IncumbentSlot()
        assertEquals(Admission.Verdict.ALLOW_NO_INCUMBENT, Admission.decide(false, slot.get()))
        val inc = Incumbent(Socket(), phoneA, 7, 0L)
        assertNull(slot.install(inc))
        // pre-verify: a newcomer may supersede
        assertEquals(Admission.Verdict.ALLOW_NOT_ENCRYPTED, Admission.decide(false, slot.get()))
        inc.encryptedAt = 1234L
        assertEquals(Admission.Verdict.REFUSE_SESSION_LIVE, Admission.decide(false, slot.get()))
        // a local dial is still `local`, never `session live`
        assertEquals(Admission.Verdict.REFUSE_LOCAL, Admission.decide(true, slot.get()))
        // keepalive / USER_TIMEOUT kill: the pump's finally closes the socket, then releases its own record.
        inc.sock.close()
        assertEquals(Admission.Verdict.ALLOW_INCUMBENT_CLOSED, Admission.decide(false, slot.get()))
        assertTrue(slot.release(inc))
        assertEquals(Admission.Verdict.ALLOW_NO_INCUMBENT, Admission.decide(false, slot.get()))
    }

    // ---- refusal-line scope: never the address -------------------------------------------------

    @Test fun scopeOf_namesKindNotAddress() {
        val s6 = Admission.scopeOf(phoneA)
        assertEquals("IPv6 link-local %scope 7", s6)
        assertFalse(s6.contains("fe80"))
        assertEquals("IPv4 loopback", Admission.scopeOf(loop4))
        assertEquals("IPv6 loopback", Admission.scopeOf(loop6))
        assertEquals("unknown", Admission.scopeOf(null))
        assertFalse(Admission.scopeOf(phone4).contains("192.0.2"))
    }

    // ---- AcceptGate: the accept-side step, on real loopback sockets ------------------------------

    /** (server-side accepted socket, client socket). The listener is closed at once. */
    private fun accepted(): Pair<Socket, Socket> {
        val ss = ServerSocket(0, 5, InetAddress.getLoopbackAddress())
        val cl = Socket(ss.inetAddress, ss.localPort)
        val sv = ss.accept()
        ss.close()
        return sv to cl
    }

    private val lines = mutableListOf<String>()
    private fun gate(slot: IncumbentSlot, local: (Socket) -> Boolean) = AcceptGate(slot, { lines += it }, local)
    /** The production local test, with interface ownership answered "no". */
    private val prodLocal: (Socket) -> Boolean = { c -> Admission.isLocal(c.inetAddress, c.localAddress) { false } }

    @Test fun gate_loopbackNewcomer_refusedClosed_slotUntouched() {
        val slot = IncumbentSlot()
        val (sv, cl) = accepted()
        assertNull(gate(slot, prodLocal).admit(sv, 1_000L))
        assertTrue(sv.isClosed)
        assertNull(slot.get())
        assertEquals(1, lines.size)
        assertTrue(lines[0], lines[0].startsWith("refusing control connection from "))
        assertTrue(lines[0], lines[0].contains("(local): no session live"))
        cl.close()
    }

    @Test fun gate_emptySlot_admits_recordNamesTheSocket() {
        val slot = IncumbentSlot()
        val (sv, cl) = accepted()
        val a = gate(slot, { false }).admit(sv, 5_000L)
        assertNotNull(a)
        assertEquals(Admission.Verdict.ALLOW_NO_INCUMBENT, a!!.verdict)
        assertSame(sv, a.rec.sock)
        assertEquals(5_000L, a.rec.lastRxAt)
        assertFalse(sv.isClosed)
        assertNull(slot.get())   // the gate never installs; the accept loop does
        assertTrue(lines.isEmpty())
        sv.close(); cl.close()
    }

    @Test fun gate_encryptedIncumbent_refusesEveryTry_closesNewcomer_keepsIncumbent() {
        val slot = IncumbentSlot()
        val (isv, icl) = accepted()
        val inc = Incumbent.of(isv, 0L).also { it.encryptedAt = 100L; it.lastRxAt = 900L }
        slot.install(inc)
        repeat(3) { k ->
            val (sv, cl) = accepted()
            assertNull(gate(slot, { false }).admit(sv, 1_000L))
            assertTrue(sv.isClosed)
            assertSame(inc, slot.get())
            assertFalse(isv.isClosed)
            assertTrue(lines[k], lines[k].contains("(session live): incumbent "))
            assertTrue(lines[k], lines[k].contains("encrypted 900 ms ago, quiet 100 ms"))
            cl.close()
        }
        isv.close(); icl.close()
    }

    @Test fun gate_notEncryptedIncumbent_admits_andAfterIncumbentCloses_admits() {
        val slot = IncumbentSlot()
        val (isv, icl) = accepted()
        val inc = Incumbent.of(isv, 0L)
        slot.install(inc)
        val (sv, cl) = accepted()
        assertEquals(Admission.Verdict.ALLOW_NOT_ENCRYPTED, gate(slot, { false }).admit(sv, 1L)!!.verdict)
        inc.encryptedAt = 2L
        isv.close()   // the incumbent's pump ended (keepalive kill): its socket is closed
        val (sv2, cl2) = accepted()
        assertEquals(Admission.Verdict.ALLOW_INCUMBENT_CLOSED, gate(slot, { false }).admit(sv2, 3L)!!.verdict)
        listOf(sv, cl, sv2, cl2, icl).forEach { it.close() }
    }

    @Test fun gate_localTestThrows_refuses() {
        val (sv, cl) = accepted()
        assertNull(gate(IncumbentSlot(), { throw IllegalStateException("boom") }).admit(sv, 0L))
        assertTrue(sv.isClosed)
        cl.close()
    }
}
