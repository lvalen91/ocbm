package zeno.gmccpa

import java.io.ByteArrayOutputStream
import java.net.DatagramPacket
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.MulticastSocket
import java.net.NetworkInterface
import java.net.SocketTimeoutException

/**
 * Ask the link what our OWN advert actually looks like.
 *
 * `rx-connect` — the proven implementation — pins the advertised address explicitly
 * (`RX_ADDR=192.168.43.1`) precisely because its `enable_addr_auto()` alternative publishes EVERY
 * interface address. `NsdManager` offers no equivalent control: it publishes the platform hostname
 * with whatever address set the system mdnsd chooses, over whatever interfaces it chooses. On a head
 * unit carrying `br0`, a Wi-Fi client interface and a cellular VLAN, iOS can therefore resolve our
 * SRV to an address it has no route to — it would still accept our connect-out (we dialed it) but
 * could never open the control connection back.
 *
 * That is a hypothesis, and this turns it into a measurement: query the link for our own SRV and
 * A/AAAA records and print exactly what a phone would receive.
 */
object MdnsInspect {

    private val log = ProbeLog.sub("mdns")

    fun inspect(instance: String, type: String = "_airplay._tcp") {
        val fqdn = "$instance.$type.local"
        log.i("resolving our own advert: $fqdn")
        val group = InetAddress.getByName("224.0.0.251")
        // No single-arg fallback if there is no br* interface: the one-arg join lets the kernel pick
        // the interface by routing 224.0.0.251, and on this head unit that route does not exist
        // (`Network is unreachable`), so it can only fail ENODEV. Without an interface there is
        // nothing to ask.
        val nif = brIface() ?: run { log.e("no br* interface — cannot query the link"); return }
        val sock = try {
            MulticastSocket(null as java.net.SocketAddress?).apply {
                reuseAddress = true
                bind(InetSocketAddress(5353))
                try { networkInterface = nif } catch (_: Throwable) {}
                joinGroup(InetSocketAddress(group, 5353), nif)
                soTimeout = 700
            }
        } catch (t: Throwable) { log.e("mdns bind failed: ${t.message}"); return }

        try {
            // QTYPE 33 = SRV, 1 = A, 28 = AAAA. Ask for all three by name.
            for (qtype in intArrayOf(33, 1, 28)) {
                val q = query(fqdn, qtype)
                sock.send(DatagramPacket(q, q.size, group, 5353))
            }
            val buf = ByteArray(9000)
            val deadline = System.currentTimeMillis() + 4000
            var srvTarget: String? = null
            var srvPort = -1
            // (owner-name, address) — the owner is load-bearing, see the filter below.
            val seen = LinkedHashSet<Pair<String, String>>()
            var sawAny = false
            while (System.currentTimeMillis() < deadline) {
                val pkt = DatagramPacket(buf, buf.size)
                try { sock.receive(pkt) } catch (e: SocketTimeoutException) { continue }
                val r = parse(pkt.data, pkt.length, fqdn) ?: continue
                sawAny = true
                r.srvTarget?.let { srvTarget = it; srvPort = r.srvPort }
                seen.addAll(r.addresses)
            }
            // KEEP ONLY RECORDS WE OWN. This socket is joined to 224.0.0.251 and receives every mDNS
            // packet on the link for the whole window, and parse() walks answer + authority +
            // additional sections — so `seen` contains the iPhone's own announcements and GM's
            // CarPlay._airplay._tcp records too. Attributing those to our advert produced a real false
            // alarm on 2026-08-12: the tool reported the PHONE's 192.168.5.57 as an address we were
            // "also advertising". Filter to records whose OWNER is our instance or our SRV target.
            val addrs = seen.filter { (owner, _) ->
                owner.equals(fqdn, true) || srvTarget?.let { owner.equals(it, true) } == true
            }.map { it.second }.toCollection(LinkedHashSet())
            if (!sawAny) {
                log.w("no answer for our own advert — the platform mdnsd may not answer its own queries")
                log.w("(that is not proof of a problem; it means this check is inconclusive)")
            }
            srvTarget?.let { log.i("SRV -> target=$it port=$srvPort") }

            // THE decisive check. rx-connect pins both the SRV target hostname AND an explicit
            // advertised address, because iOS resolves the target through mDNS and dials the A
            // record it gets back. If nobody answers an A query for our target on br0, or answers
            // with an address the phone cannot route to, iOS abandons SILENTLY — which is exactly
            // the "200 OK then never opens RTSP" signature. This resolves the target by NAME; the
            // summary set below is separately owner-filtered, because collecting A records from any
            // answer on the link (including the phone's own) proves nothing.
            srvTarget?.let { t ->
                log.i("resolving the SRV TARGET itself: $t")
                val ta = resolveHost(sock, group, t)
                if (ta.isEmpty()) log.e("VERDICT: NOBODY ANSWERS an A/AAAA query for '$t' — iOS cannot reach us")
                else {
                    ta.forEach { log.i("  $t -> $it") }
                    val br0v4 = br0Addrs().filter { !it.contains(':') }
                    if (ta.any { a -> br0v4.any { it == a } })
                        log.i("VERDICT: SRV target resolves to a br0 address — the path is intact")
                    else
                        log.e("VERDICT: SRV target does NOT resolve to a br0 address ($br0v4) — iOS dials somewhere unreachable")
                }
            }
            if (addrs.isEmpty()) log.w("no A/AAAA records observed for the SRV target")
            else addrs.forEach { log.i("address record: $it") }

            // The verdict that matters: does the published set include the hotspot bridge address,
            // and does it ALSO include addresses the phone cannot route to?
            val br0 = br0Addrs()
            val onBr0 = addrs.any { a -> br0.any { it == a } }
            val strays = addrs.filter { a -> br0.none { it == a } }
            log.i("br0 addresses: ${br0.joinToString(", ")}")
            if (onBr0) log.i("VERDICT: the advert includes a br0 address — reachable from the phone")
            else if (addrs.isNotEmpty()) log.e("VERDICT: the advert does NOT include any br0 address — the phone cannot reach us")
            if (strays.isNotEmpty()) log.w("also advertising non-br0 addresses (iOS may try these first): ${strays.joinToString(", ")}")
        } finally {
            try { sock.leaveGroup(InetSocketAddress(group, 5353), nif) } catch (_: Throwable) {}
            sock.close()
        }
    }

    /** A peer's live SRV endpoint: the port it publishes, and every address answering for its target. */
    class Endpoint(val target: String, val port: Int, val addresses: List<InetAddress>)

    /**
     * Resolve a peer's service **from the wire**, bypassing `NsdManager`'s cache.
     *
     * This exists because of a device-observed failure (2026-08-12): `NsdManager` kept answering
     * resolves for `_carplay-ctrl._tcp` with an iPhone IPv6 link-local that no longer existed — the
     * neighbour table showed it `INCOMPLETE` and `ping6` said "Address unreachable", while the phone
     * was live on the same bridge at a different link-local and at `192.168.5.57`. Every connect-out
     * then timed out, so the `GET /ctrl-int/1/connect` never landed, so iOS never set a pending
     * autoconnect (`carManager_handlePendingAutoconnect: no pending autoconnections`) and never
     * dialled us back. Restarting discovery does NOT fix it: the platform answers the new resolve
     * from the same stale cache.
     *
     * iOS rotates its IPv6 privacy address, so this is expected to recur — treat `NsdManager`'s
     * address as a hint and this as the authority.
     */
    fun liveEndpoint(instance: String, type: String): Endpoint? {
        val fqdn = "$instance.$type.local"
        val group = InetAddress.getByName("224.0.0.251")
        val nif = brIface() ?: run { log.w("live lookup: no br* interface — cannot query the link"); return null }
        // This shares :5353 with MdnsResponder and with `inspect` (SO_REUSEADDR, three-way). Left as
        // is on purpose. If this lookup ever proves flaky, the right fix is NOT to fight over the
        // shared bind: bind the QUERY sockets to an ephemeral port and read the legacy-unicast
        // replies (RFC 6762 §6.7), which sidesteps both the shared bind and the group join entirely.
        // Only the responder genuinely needs to be on 5353.
        val sock = try {
            MulticastSocket(null as java.net.SocketAddress?).apply {
                reuseAddress = true
                bind(InetSocketAddress(5353))
                try { networkInterface = nif } catch (_: Throwable) {}
                joinGroup(InetSocketAddress(group, 5353), nif)
                soTimeout = 500
            }
        } catch (t: Throwable) { log.w("live lookup: mdns bind failed: ${t.message}"); return null }
        try {
            val q = query(fqdn, 33)                       // 33 = SRV
            try { sock.send(DatagramPacket(q, q.size, group, 5353)) } catch (_: Throwable) {}
            var target: String? = null
            var port = -1
            val buf = ByteArray(9000)
            val deadline = System.currentTimeMillis() + 2500
            while (System.currentTimeMillis() < deadline && target == null) {
                val pkt = DatagramPacket(buf, buf.size)
                try { sock.receive(pkt) } catch (e: SocketTimeoutException) { continue }
                val r = parse(pkt.data, pkt.length, fqdn) ?: continue
                r.srvTarget?.let { target = it; port = r.srvPort }
            }
            val t = target ?: run { log.w("live lookup: no SRV answer for $fqdn"); return null }
            // Ask for the TARGET's own A/AAAA — not "any address in the packet", which would happily
            // return the stale record riding along in an unrelated answer section.
            val addrs = resolveHost(sock, group, t).mapNotNull {
                try { InetAddress.getByName(it.substringBefore('%')) } catch (_: Throwable) { null }
            }
            log.i("live lookup: $fqdn -> target=$t port=$port addrs=${addrs.joinToString { a -> a.hostAddress ?: "?" }}")
            return Endpoint(t, port, addrs)
        } finally {
            try { sock.leaveGroup(InetSocketAddress(group, 5353), nif) } catch (_: Throwable) {}
            sock.close()
        }
    }

    /** Send a targeted A+AAAA query for one hostname and collect the answers. */
    private fun resolveHost(sock: MulticastSocket, group: InetAddress, host: String): List<String> {
        val out = LinkedHashSet<String>()
        for (qtype in intArrayOf(1, 28)) {
            val q = query(host, qtype)
            try { sock.send(DatagramPacket(q, q.size, group, 5353)) } catch (_: Throwable) {}
        }
        val buf = ByteArray(9000)
        val deadline = System.currentTimeMillis() + 3000
        // Stop as soon as the target has answered. This runs on the connect-out path (dial attempts
        // 1, 3 and 6), so burning the full 3 s window after the answer is already in hand added
        // ~15 s of dead time per dial cycle. A responder that answers at all answers immediately.
        var quietUntil = 0L
        while (System.currentTimeMillis() < deadline) {
            if (out.isNotEmpty() && System.currentTimeMillis() >= quietUntil) break
            val pkt = DatagramPacket(buf, buf.size)
            try { sock.receive(pkt) } catch (e: SocketTimeoutException) { continue }
            val before = out.size
            // Only accept answers whose NAME is the host we asked about.
            out.addAll(addressesFor(pkt.data, pkt.length, host))
            // Give a short grace after the first answer so an A and a AAAA in separate packets are
            // both collected rather than racing.
            if (out.size > before && quietUntil == 0L) quietUntil = System.currentTimeMillis() + 250
        }
        return out.toList()
    }

    /** A/AAAA records whose owner name matches [want] exactly — not "any address in the packet". */
    private fun addressesFor(data: ByteArray, len: Int, want: String): List<String> {
        if (len < 12) return emptyList()
        fun u16(o: Int) = ((data[o].toInt() and 0xFF) shl 8) or (data[o + 1].toInt() and 0xFF)
        val qd = u16(4); val an = u16(6) + u16(8) + u16(10)
        var pos = 12
        for (i in 0 until qd) { pos = skipName(data, pos); pos += 4 }
        val out = ArrayList<String>()
        for (i in 0 until an) {
            if (pos >= len) break
            val (nm, after) = dnsReadName(data, pos, len)
            val type = u16(after); val rdlen = u16(after + 8); val rd = after + 10
            if (nm.equals(want, true)) {
                if (type == 1 && rdlen == 4) out.add("%d.%d.%d.%d".format(
                    data[rd].toInt() and 0xFF, data[rd + 1].toInt() and 0xFF,
                    data[rd + 2].toInt() and 0xFF, data[rd + 3].toInt() and 0xFF))
                if (type == 28 && rdlen == 16) try {
                    InetAddress.getByAddress(data.copyOfRange(rd, rd + 16)).hostAddress?.let { out.add(it) }
                } catch (_: Throwable) {}
            }
            pos = rd + rdlen
        }
        return out
    }

    private fun br0Addrs(): List<String> =
        brIface()?.inetAddresses?.toList()?.mapNotNull { it.hostAddress?.substringBefore('%') } ?: emptyList()

    private fun brIface(): NetworkInterface? =
        try {
            NetworkInterface.getNetworkInterfaces()?.toList()
                ?.firstOrNull { it.isUp && it.name.startsWith("br") && it.supportsMulticast() }
        } catch (t: Throwable) { null }

    private fun query(name: String, qtype: Int): ByteArray {
        val b = ByteArrayOutputStream()
        b.write(byteArrayOf(0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0))
        for (label in name.trimEnd('.').split(".")) {
            val by = label.toByteArray(Charsets.UTF_8); b.write(by.size); b.write(by)
        }
        b.write(0)
        b.write(byteArrayOf((qtype shr 8).toByte(), (qtype and 0xFF).toByte()))
        b.write(byteArrayOf(0, 1))
        return b.toByteArray()
    }

    /** [addresses] is (owner-name, address) — the owner is required to attribute a record. */
    private class Rec(val srvTarget: String?, val srvPort: Int, val addresses: List<Pair<String, String>>)

    private fun parse(data: ByteArray, len: Int, want: String): Rec? {
        if (len < 12) return null
        fun u16(o: Int) = ((data[o].toInt() and 0xFF) shl 8) or (data[o + 1].toInt() and 0xFF)
        val qd = u16(4); val an = u16(6) + u16(8) + u16(10)
        var pos = 12
        for (i in 0 until qd) { pos = skipName(data, pos); pos += 4 }
        var srvTarget: String? = null
        var srvPort = -1
        // Carry the OWNER name with each address. The caller cannot attribute an address correctly
        // without it, and this socket sees every mDNS packet on the link, not just replies to us.
        val addrs = ArrayList<Pair<String, String>>()
        for (i in 0 until an) {
            if (pos >= len) break
            val (nm, after) = dnsReadName(data, pos, len)
            val type = u16(after); val rdlen = u16(after + 8); val rd = after + 10
            when (type) {
                33 -> if (nm.equals(want, true)) { srvPort = u16(rd + 4); srvTarget = dnsReadName(data, rd + 6, len).first }
                1 -> if (rdlen == 4) addrs.add(nm to "%d.%d.%d.%d".format(
                    data[rd].toInt() and 0xFF, data[rd + 1].toInt() and 0xFF,
                    data[rd + 2].toInt() and 0xFF, data[rd + 3].toInt() and 0xFF))
                28 -> if (rdlen == 16) addrs.add(nm to
                    (try { InetAddress.getByAddress(data.copyOfRange(rd, rd + 16)).hostAddress ?: "" }
                     catch (t: Throwable) { "" }))
            }
            pos = rd + rdlen
        }
        return if (srvTarget == null && addrs.isEmpty()) null
               else Rec(srvTarget, srvPort, addrs.filter { it.second.isNotEmpty() })
    }

    private fun skipName(data: ByteArray, start: Int): Int {
        var pos = start
        while (pos < data.size) {
            val b = data[pos].toInt() and 0xFF
            if (b == 0) return pos + 1
            if (b and 0xC0 == 0xC0) return pos + 2
            pos += 1 + b
        }
        return pos
    }

}
