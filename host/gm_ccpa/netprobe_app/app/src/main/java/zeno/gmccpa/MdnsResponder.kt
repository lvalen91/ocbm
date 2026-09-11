package zeno.gmccpa

import java.io.ByteArrayOutputStream
import java.net.DatagramPacket
import java.net.Inet4Address
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.MulticastSocket
import java.net.NetworkInterface
import java.net.SocketTimeoutException
import java.util.concurrent.atomic.AtomicBoolean

/**
 * A minimal mDNS responder that we control completely.
 *
 * **Why this exists.** `NsdManager` published PTR and TXT for our service but iOS never received an
 * SRV or address record, so it had a service it could read and no `host:port` to dial — the exact
 * observed failure. That is not fixable through the platform API: verified at AOSP source,
 * `NsdService.registerService()` forwards only `regId, name, type, port, record`,
 * `NsdServiceInfo.setHost()` is *ignored* on registration, netd's `MDnsSdListener` hardcodes the
 * interface index to 0, and resolution then does a `getaddrinfo` across every interface and takes the
 * first address — a coin flip on this 8-interface head unit.
 *
 * `rx-connect`, the proven implementation, sidesteps all of it by pinning both the SRV target
 * hostname and an explicit advertised address (`RX_ADDR`). This class does the same thing: it owns
 * the hostname, and it answers with **one** address — the vehicle bridge `br0`, which is the only
 * address the iPhone can reach us on.
 *
 * Deliberately small. It answers PTR / SRV / TXT / A / NSEC (and ANY) for exactly one service and
 * announces unsolicited on start.
 *
 * **RFC 6762 §8.1 probing is deliberately skipped**, even though we set the cache-flush bit on the
 * records we own. Probing exists to detect a name already claimed by another responder; here the
 * instance name and the hostname are ours alone by construction — chosen to collide with neither GM's
 * `CarPlay` nor the other adapter's `carlink`, on a closed AP whose only other members are the phone
 * and the box. There is no third responder that could hold them, so a probe phase would only add
 * 750 ms of dead time to every start on the one path that is already latency-critical.
 */
class MdnsResponder(
    private val instance: String,
    private val serviceType: String,   // "_airplay._tcp"
    private val port: Int,
    private val txt: Map<String, String>,
    private val log: ProbeLog.Logger
) {
    private val running = AtomicBoolean(false)
    private var sock: MulticastSocket? = null
    private var thread: Thread? = null

    /** Our own hostname. NOT the platform's `Android.local` — we must own the A record for it. */
    private val hostname = "$instance.local"
    private val serviceFqdn = "$instance.$serviceType.local"
    private val typeFqdn = "$serviceType.local"

    /** The single address we advertise: the hotspot bridge the phone is actually on. */
    @Volatile
    private var advertised: Inet4Address? = null

    private companion object {
        const val PORT = 5353
        const val GROUP = "224.0.0.251"
        const val TTL_HOST = 120
        const val TTL_PTR = 4500
        /** Class IN with the cache-flush bit — correct for records we are authoritative for. */
        const val CLASS_FLUSH = 0x8001
        const val CLASS_IN = 0x0001
        const val T_A = 1; const val T_PTR = 12; const val T_TXT = 16; const val T_SRV = 33
        const val T_AAAA = 28
        const val T_NSEC = 47
        const val T_ANY = 255
    }

    fun start(): Boolean {
        if (!running.compareAndSet(false, true)) return true
        val nif = br0() ?: run { log.e("no br0 interface — cannot advertise where the phone is"); running.set(false); return false }
        advertised = nif.inetAddresses.toList().filterIsInstance<Inet4Address>().firstOrNull()
        if (advertised == null) { log.e("br0 has no IPv4 address yet"); running.set(false); return false }

        sock = try {
            MulticastSocket(null as java.net.SocketAddress?).apply {
                reuseAddress = true
                bind(InetSocketAddress(PORT))
                // Send-side egress only. The join below must NOT rely on it: `IP_ADD_MEMBERSHIP`
                // with `imr_ifindex=0` does its own route lookup regardless of `IP_MULTICAST_IF`,
                // and on this head unit `ip route get 224.0.0.251` is "Network is unreachable" —
                // which is why the single-arg join failed ENODEV here despite this line already
                // being present. The two-arg form passes the ifindex explicitly and skips the
                // lookup; /proc/net/igmp shows the system daemon already joined on br0 that way.
                networkInterface = nif
                joinGroup(InetSocketAddress(InetAddress.getByName(GROUP), PORT), nif)
                soTimeout = 500
            }
        } catch (t: Throwable) {
            log.e("mDNS bind failed: ${t.javaClass.simpleName}: ${t.message}"); running.set(false); return false
        }

        log.i("responder up: $serviceFqdn -> $hostname:$port @ ${advertised?.hostAddress} (${nif.name})")
        thread = Thread({ loop() }, "mdns-responder").apply { isDaemon = true; start() }
        announce()
        return true
    }

    fun stop() {
        if (!running.compareAndSet(true, false)) return
        // Retract before the socket closes. Without a goodbye our SRV/TXT/A linger in iOS's cache for
        // TTL_HOST (120 s) and the PTR for TTL_PTR (4500 s) — so a receiver that restarts precisely to
        // clear a wedged endpoint is re-discovered at its stale records and the restart accomplishes
        // nothing. This is a prerequisite for any "reset the stack to a clean state" recovery.
        goodbye()
        try { thread?.join(1200) } catch (_: InterruptedException) {}
        try { sock?.close() } catch (_: Throwable) {}
        sock = null; thread = null
        log.i("responder stopped")
    }

    /**
     * Re-assert every record without tearing the responder down — the cheapest recovery rung there
     * is, and the only one that addresses a peer holding a stale endpoint for us.
     */
    fun reannounce() {
        if (!running.get()) { log.w("reannounce with the responder down — ignored"); return }
        log.i("re-announcing (recovery)")
        announce()
    }

    /**
     * Withdraw every record we own: same set as [announce], TTL 0. Sent twice — mDNS goodbyes are
     * unacknowledged, and one lost packet leaves the stale record in place for its whole TTL.
     */
    private fun goodbye() {
        val pkt = buildResponse(ptr = true, srv = true, txt = true, a = true, ttl0 = true)
        var sent = 0
        repeat(2) {
            if (send(pkt)) sent++
            try { Thread.sleep(120) } catch (_: InterruptedException) { return }
        }
        if (sent == 0) log.w("goodbye FAILED — our records linger in iOS's cache until they expire")
        else log.i("goodbye sent ($sent/2) — PTR/SRV/TXT/A withdrawn at TTL 0")
    }

    /** Unsolicited announcements, so iOS learns SRV+A without having to ask. */
    private fun announce() {
        val pkt = buildResponse(ptr = true, srv = true, txt = true, a = true)
        var sent = 0
        repeat(3) {
            if (send(pkt)) sent++
            // RFC 6762 §8.3: announcements are sent at least one second apart. At 250 ms all three
            // arrived inside one iOS coalescing window, so the repetition bought no loss margin.
            try { Thread.sleep(1000) } catch (_: InterruptedException) { return }
        }
        // Report what actually happened. Claiming success unconditionally hid a real failure mode:
        // called from the UI thread, every send throws NetworkOnMainThreadException and is swallowed.
        if (sent == 0) log.e("announce FAILED — no packet left the socket; iOS will never index us")
        else log.i("announced PTR + SRV + TXT + A ($sent/3 sent)")
    }

    private fun loop() {
        val buf = ByteArray(9000)
        while (running.get()) {
            val p = DatagramPacket(buf, buf.size)
            try { sock?.receive(p) ?: break } catch (e: SocketTimeoutException) { continue }
            catch (t: Throwable) { if (running.get()) log.w("recv: ${t.message}"); continue }
            try { handleQuery(p.data, p.length) } catch (t: Throwable) {
                log.w("query handling: ${t.javaClass.simpleName}: ${t.message}")
            }
        }
    }

    private fun handleQuery(data: ByteArray, len: Int) {
        if (len < 12) return
        fun u16(o: Int) = ((data[o].toInt() and 0xFF) shl 8) or (data[o + 1].toInt() and 0xFF)
        if (u16(2) and 0x8000 != 0) return          // a response, not a query
        val qd = u16(4)
        if (qd == 0) return
        var pos = 12
        var wantPtr = false; var wantSrv = false; var wantTxt = false; var wantA = false
        var wantNsec = false
        for (i in 0 until qd) {
            val (name, after) = dnsReadName(data, pos, len)
            if (after + 4 > len) return
            val qtype = u16(after)
            pos = after + 4
            val any = qtype == T_ANY
            when {
                name.equals(typeFqdn, true) && (qtype == T_PTR || any) -> { wantPtr = true; wantSrv = true; wantTxt = true; wantA = true }
                name.equals(serviceFqdn, true) -> {
                    if (qtype == T_SRV || any) { wantSrv = true; wantA = true }
                    if (qtype == T_TXT || any) wantTxt = true
                }
                // AAAA gets our A record AND an NSEC saying "this name has A and nothing else"
                // (RFC 6762 §6.1). The A alone does not answer the question that was asked, so iOS
                // sat out its own AAAA timeout instead of proceeding on IPv4 — dead time sitting
                // directly in the endpoint-creation path. The NSEC is the authoritative "no AAAA
                // here", which lets it move on immediately.
                name.equals(hostname, true) && (qtype == T_A || qtype == T_AAAA || any) -> {
                    wantA = true
                    if (qtype == T_AAAA || any) wantNsec = true
                }
            }
        }
        if (!(wantPtr || wantSrv || wantTxt || wantA)) return
        log.i("query -> answering${if (wantPtr) " PTR" else ""}${if (wantSrv) " SRV" else ""}" +
              "${if (wantTxt) " TXT" else ""}${if (wantA) " A" else ""}${if (wantNsec) " NSEC" else ""}")
        send(buildResponse(wantPtr, wantSrv, wantTxt, wantA, nsec = wantNsec))
    }

    private fun send(pkt: ByteArray): Boolean = try {
        sock?.send(DatagramPacket(pkt, pkt.size, InetAddress.getByName(GROUP), PORT))
        true
    } catch (t: Throwable) { log.w("send: ${t.javaClass.simpleName}: ${t.message}"); false }

    // ---- record construction ----------------------------------------------------------------------

    /**
     * [ttl0] builds the same record set with TTL 0 — an mDNS goodbye. See [goodbye].
     * [nsec] adds the "this name has A and nothing else" denial to the ADDITIONAL section, which is
     * where RFC 6762 §6.1 puts it.
     */
    private fun buildResponse(ptr: Boolean, srv: Boolean, txt: Boolean, a: Boolean,
                              nsec: Boolean = false, ttl0: Boolean = false): ByteArray {
        val answers = ByteArrayOutputStream()
        var n = 0
        if (ptr) { answers.write(recPtr(ttl0)); n++ }
        if (srv) { answers.write(recSrv(ttl0)); n++ }
        if (txt) { answers.write(recTxt(ttl0)); n++ }
        if (a)   { recA(ttl0)?.let { answers.write(it); n++ } }
        val additional = ByteArrayOutputStream()
        var ar = 0
        if (nsec) { additional.write(recNsec(ttl0)); ar++ }
        val out = ByteArrayOutputStream()
        // id=0, flags=0x8400 (response + authoritative), qd=0, an=n, ns=0, ar=ar
        out.write(byteArrayOf(0, 0, 0x84.toByte(), 0, 0, 0, (n shr 8).toByte(), (n and 0xFF).toByte(),
                              0, 0, (ar shr 8).toByte(), (ar and 0xFF).toByte()))
        out.write(answers.toByteArray())
        out.write(additional.toByteArray())
        return out.toByteArray()
    }

    private fun record(name: String, type: Int, cls: Int, ttl: Int, rdata: ByteArray): ByteArray {
        val b = ByteArrayOutputStream()
        b.write(encodeName(name))
        b.write(byteArrayOf((type shr 8).toByte(), (type and 0xFF).toByte()))
        b.write(byteArrayOf((cls shr 8).toByte(), (cls and 0xFF).toByte()))
        b.write(byteArrayOf((ttl ushr 24).toByte(), (ttl ushr 16).toByte(), (ttl ushr 8).toByte(), ttl.toByte()))
        b.write(byteArrayOf((rdata.size shr 8).toByte(), (rdata.size and 0xFF).toByte()))
        b.write(rdata)
        return b.toByteArray()
    }

    // PTR is shared (many instances of a type may exist) — no cache-flush bit.
    private fun recPtr(ttl0: Boolean = false) =
        record(typeFqdn, T_PTR, CLASS_IN, if (ttl0) 0 else TTL_PTR, encodeName(serviceFqdn))

    private fun recSrv(ttl0: Boolean = false): ByteArray {
        val rd = ByteArrayOutputStream()
        rd.write(byteArrayOf(0, 0, 0, 0))                                   // priority 0, weight 0
        rd.write(byteArrayOf((port shr 8).toByte(), (port and 0xFF).toByte()))
        rd.write(encodeName(hostname))
        return record(serviceFqdn, T_SRV, CLASS_FLUSH, if (ttl0) 0 else TTL_HOST, rd.toByteArray())
    }

    private fun recTxt(ttl0: Boolean = false): ByteArray {
        val rd = ByteArrayOutputStream()
        for ((k, v) in txt) {
            val kv = "$k=$v".toByteArray(Charsets.UTF_8)
            rd.write(kv.size); rd.write(kv)
        }
        if (txt.isEmpty()) rd.write(0)
        return record(serviceFqdn, T_TXT, CLASS_FLUSH, if (ttl0) 0 else TTL_HOST, rd.toByteArray())
    }

    private fun recA(ttl0: Boolean = false): ByteArray? =
        advertised?.let { record(hostname, T_A, CLASS_FLUSH, if (ttl0) 0 else TTL_HOST, it.address) }

    /**
     * NSEC for our hostname: "the only record type that exists at this name is A".
     *
     * RDATA is next-domain-name followed by a type bitmap (RFC 4034 §4.1.2). In mDNS the
     * next-domain field carries the owner name itself and is written UNCOMPRESSED. The bitmap is one
     * window: block 0, one byte long, with the bit for type A (1) set — bit 0 is the high bit of the
     * first byte, so type 1 is 0x40. Nothing else is set, which is exactly the assertion "no AAAA".
     *
     * Keep this minimal and exact: a malformed NSEC makes iOS discard the WHOLE response, taking the
     * A record with it.
     */
    private fun recNsec(ttl0: Boolean = false): ByteArray {
        val rd = ByteArrayOutputStream()
        rd.write(encodeName(hostname))                  // next domain name = the owner
        rd.write(byteArrayOf(0, 1, 0x40))               // window 0, 1 byte, { A }
        return record(hostname, T_NSEC, CLASS_FLUSH, if (ttl0) 0 else TTL_HOST, rd.toByteArray())
    }

    /** Uncompressed name encoding. Compression pointers are legal but pointless at this size. */
    private fun encodeName(name: String): ByteArray {
        val b = ByteArrayOutputStream()
        for (label in name.trimEnd('.').split(".")) {
            val by = label.toByteArray(Charsets.UTF_8)
            b.write(by.size); b.write(by)
        }
        b.write(0)
        return b.toByteArray()
    }

    private fun br0(): NetworkInterface? = try {
        NetworkInterface.getNetworkInterfaces()?.toList()?.firstOrNull {
            it.isUp && it.name.startsWith("br") && it.supportsMulticast() &&
                it.inetAddresses.toList().any { a -> a is Inet4Address }
        }
    } catch (t: Throwable) { null }
}

/**
 * Decode a DNS name at [start] in [data] (bounded by [len]), following at most one compression
 * pointer chain. Shared by [MdnsResponder] and [MdnsInspect] so the wire-format edge cases —
 * notably the bounds check on a truncated compression pointer's second byte — are not duplicated
 * and cannot diverge between the two copies again.
 */
internal fun dnsReadName(data: ByteArray, start: Int, len: Int): Pair<String, Int> {
    val sb = StringBuilder(); var pos = start; var jumped = false; var after = start; var guard = 0
    while (pos < len && guard++ < 128) {
        val b = data[pos].toInt() and 0xFF
        if (b == 0) { if (!jumped) after = pos + 1; break }
        if (b and 0xC0 == 0xC0) {
            if (pos + 1 >= len) break
            val ptr = ((b and 0x3F) shl 8) or (data[pos + 1].toInt() and 0xFF)
            if (!jumped) after = pos + 2
            pos = ptr; jumped = true; continue
        }
        pos += 1
        if (pos + b > len) break
        if (sb.isNotEmpty()) sb.append('.')
        sb.append(String(data, pos, b, Charsets.UTF_8)); pos += b
    }
    return Pair(sb.toString(), after)
}
