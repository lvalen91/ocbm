package zeno.gmccpa

import java.io.ByteArrayOutputStream
import java.net.DatagramPacket
import java.net.Inet4Address
import java.net.Inet6Address
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
 * the hostname, and it answers only with addresses on the vehicle bridge `br0` — the one interface the
 * iPhone can reach us on: br0's IPv4 (A) and its IPv6 link-local (AAAA).
 * 2026-10-06: the AAAA is load-bearing. IPv4-only, the phone's 9/9 dials to the br0 IPv4 address on
 * :7011 timed out (the radio's firewall drops it), while every session that formed dialled br0's IPv6
 * link-local — resolved from `Android.local`, which carries AAAA + A, and the phone took the AAAA.
 * This record set now has the same shape, AAAA emitted first.
 *
 * Deliberately small. It answers PTR / SRV / TXT / A / AAAA / NSEC (and ANY) for exactly one service
 * and announces unsolicited on start.
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
    /**
     * Held for every socket send, so the `running` check in [send] and the send itself are one step.
     * stop() clears `running` before its [goodbye] takes this lock, so any non-goodbye packet either
     * left the socket before the goodbye or sees `running == false` and is refused (audit12 F126).
     */
    private val sendLock = Any()
    private var sock: MulticastSocket? = null
    private var thread: Thread? = null

    /** Our own hostname. NOT the platform's `Android.local` — we must own the address records for it. */
    private val hostname = "$instance.local"
    private val serviceFqdn = "$instance.$serviceType.local"
    private val typeFqdn = "$serviceType.local"

    /** The bridge we bound to; [reannounce] re-reads its addresses by this name. */
    private var nifName: String? = null
    /** …and its ifindex, which [boundTo] compares: the name survives a re-created bridge, the socket's join does not. */
    private var nifIndex = -1

    /** The addresses we advertise: the hotspot bridge the phone is actually on. */
    @Volatile
    private var advertised: Inet4Address? = null
    /** br0's IPv6 link-local, or null if it has none — then we are IPv4-only, which the phone cannot dial. */
    @Volatile
    private var advertised6: Inet6Address? = null

    /** True when the AAAA is being published. Read for the Board line; see [advertised6]. */
    val hasAaaa: Boolean get() = advertised6 != null

    /**
     * One look at br0 (C2, 2026-10-29): the interface — by ifindex, because a re-created bridge keeps
     * its name while the old socket's membership and `IP_MULTICAST_IF` die with the old index — and
     * the A/AAAA pair it would be advertised at. A value, so the receiver's bridge watch can compare
     * two looks and act only on a change.
     */
    data class Bridge(val name: String, val index: Int, val v4: Inet4Address, val v6: Inet6Address?)

    companion object {
        private const val PORT = 5353
        private const val GROUP = "224.0.0.251"
        private const val TTL_HOST = 120
        private const val TTL_PTR = 4500
        /** Class IN with the cache-flush bit — correct for records we are authoritative for. */
        private const val CLASS_FLUSH = 0x8001
        private const val CLASS_IN = 0x0001
        private const val T_A = 1; private const val T_PTR = 12; private const val T_TXT = 16; private const val T_SRV = 33
        private const val T_AAAA = 28
        private const val T_NSEC = 47
        private const val T_ANY = 255

        /** br0 as it is right now, or null when no usable bridge is up — the same test [start] applies. */
        fun bridge(): Bridge? {
            val nif = br0() ?: return null
            val v4 = nif.inetAddresses.toList().filterIsInstance<Inet4Address>().firstOrNull() ?: return null
            return Bridge(nif.name, nif.index, v4, linkLocal6(nif))
        }

        /** br0's IPv6 link-local — the address every formed session was dialled on. Null if it has none. */
        private fun linkLocal6(nif: NetworkInterface): Inet6Address? = runCatching {
            nif.inetAddresses.toList().filterIsInstance<Inet6Address>().firstOrNull { it.isLinkLocalAddress }
        }.getOrNull()

        private fun br0(): NetworkInterface? = try {
            NetworkInterface.getNetworkInterfaces()?.toList()?.firstOrNull {
                it.isUp && it.name.startsWith("br") && it.supportsMulticast() &&
                    it.inetAddresses.toList().any { a -> a is Inet4Address }
            }
        } catch (t: Throwable) { null }
    }

    fun start(): Boolean {
        if (!running.compareAndSet(false, true)) return true
        val nif = br0() ?: run { log.e("no br0 interface — cannot advertise where the phone is"); running.set(false); return false }
        advertised = nif.inetAddresses.toList().filterIsInstance<Inet4Address>().firstOrNull()
        if (advertised == null) { log.e("br0 has no IPv4 address yet"); running.set(false); return false }
        nifName = nif.name; nifIndex = nif.index
        advertised6 = linkLocal6(nif)
        if (advertised6 == null) log.w("${nif.name} has no IPv6 link-local — advertising A only; the phone's IPv4 dial to :$port is known to time out")

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

        log.i("responder up: $serviceFqdn -> $hostname:$port @ ${advertised?.hostAddress}" +
              "${advertised6?.let { " + ${it.hostAddress}" } ?: ""} (${nif.name})")
        thread = Thread({ loop() }, "mdns-responder").apply { isDaemon = true; start() }
        announceAsync("start")
        return true
    }

    /**
     * The three announcements, on their own daemon thread (D-F7, 2026-10-08). Inline, [announce]
     * held its caller for the two inter-packet seconds: [start] blocked `CarPlayRx.start()` — and
     * with it the OCBM claim queued behind the receiver — and [reannounce] blocked the supervisor's
     * single scheduler thread (rung 0 runs there). NOT on [loop]'s thread either: that is the
     * query-answer thread, and a 2 s pause there leaves iOS's queries unanswered. Two announces in
     * flight (a start and a rung 0) only repeat packets; each checks `running` before every send.
     */
    private fun announceAsync(why: String) {
        Thread({ announce() }, "mdns-announce-$why").apply { isDaemon = true; start() }
    }

    fun stop() {
        if (!running.compareAndSet(true, false)) return
        // Retract before the socket closes. Without a goodbye our SRV/TXT/A/AAAA linger in iOS's cache for
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
        refreshAddresses()
        log.i("re-announcing (recovery)")
        announceAsync("reannounce")
    }

    /**
     * True while [b] is the interface this responder is joined on. By ifindex, not name: a re-created
     * br0 keeps its name, but the socket's membership and `IP_MULTICAST_IF` died with the old index,
     * so only a new responder (a fresh [start]) can serve it.
     */
    fun boundTo(b: Bridge?): Boolean = b != null && running.get() && b.index == nifIndex

    /**
     * The bridge watch's step (C2, 2026-10-29): re-read br0 and, only if its A/AAAA pair differs from
     * what we advertise, re-announce with cache-flush — the IPv6 link-local that landed after [start]
     * replaces an A-only set in the phone's cache. Unchanged: nothing is sent. Same path as
     * [reannounce]; a rung 0 racing this repeats packets, nothing more.
     */
    fun refreshIfChanged(): Boolean {
        if (!running.get()) return false
        val before = advertised to advertised6
        refreshAddresses()
        if (before == (advertised to advertised6)) return false
        announceAsync("addr-change")
        return true
    }

    /**
     * Re-read br0's addresses before a re-announce, so a bridge that was re-addressed since [start]
     * is advertised at its current addresses. The cache-flush bit on A/AAAA replaces the old ones in
     * iOS's cache. A br0 that has vanished or lost its IPv4 keeps the last-known set: the socket is
     * bound to it, so only a receiver restart (a new responder, a fresh [start]) can follow that.
     */
    private fun refreshAddresses() {
        val nif = runCatching { nifName?.let { NetworkInterface.getByName(it) } }.getOrNull()
        val v4 = nif?.inetAddresses?.toList()?.filterIsInstance<Inet4Address>()?.firstOrNull()
        if (nif == null || v4 == null) { log.w("reannounce: ${nifName ?: "br0"} gone or without IPv4 — keeping the last-known addresses"); return }
        val v6 = linkLocal6(nif)
        if (v4 != advertised || v6 != advertised6) {
            log.i("br0 re-addressed: ${advertised?.hostAddress}/${advertised6?.hostAddress} -> ${v4.hostAddress}/${v6?.hostAddress}")
            if (v6 == null) log.w("${nif.name} has no IPv6 link-local now — advertising A only")
        }
        advertised = v4; advertised6 = v6
    }

    /**
     * Withdraw every record we own: same set as [announce], TTL 0. Sent twice — mDNS goodbyes are
     * unacknowledged, and one lost packet leaves the stale record in place for its whole TTL.
     */
    private fun goodbye() {
        val pkt = buildResponse(ptr = true, srv = true, txt = true, addr = true, ttl0 = true)
        var sent = 0
        repeat(2) {
            if (send(pkt, goodbye = true)) sent++
            try { Thread.sleep(120) } catch (_: InterruptedException) { return }
        }
        if (sent == 0) log.w("goodbye FAILED — our records linger in iOS's cache until they expire")
        else log.i("goodbye sent ($sent/2) — PTR/SRV/TXT/A${if (advertised6 != null) "/AAAA" else ""} withdrawn at TTL 0")
    }

    /** Unsolicited announcements, so iOS learns SRV + AAAA + A without having to ask. */
    private fun announce() {
        val pkt = buildResponse(ptr = true, srv = true, txt = true, addr = true)
        var sent = 0
        repeat(3) { i ->
            // A reannounce in flight when stop() runs must not follow its goodbye: one late TTL-120
            // packet re-caches us in iOS for a receiver that is gone. This check only ends the loop
            // early; the guarantee is [send]'s own `running` re-check UNDER [sendLock] (F126): stop()
            // flips `running` before its goodbye takes that lock, so a packet of ours either leaves
            // before the goodbye or is refused — on this thread (announceAsync) as on any other.
            if (!running.get()) { log.i("announce cut short by stop() ($sent/3 sent)"); return }
            if (send(pkt)) sent++
            // RFC 6762 §8.3: announcements are sent at least one second apart. At 250 ms all three
            // arrived inside one iOS coalescing window, so the repetition bought no loss margin.
            // No sleep after the last one (D-F7): nothing follows it.
            if (i < 2) try { Thread.sleep(1000) } catch (_: InterruptedException) { return }
        }
        // Report what actually happened. Claiming success unconditionally hid a real failure mode:
        // called from the UI thread, every send throws NetworkOnMainThreadException and is swallowed.
        if (sent == 0) log.e("announce FAILED — no packet left the socket; iOS will never index us")
        else log.i("announced PTR + SRV + TXT + A${if (advertised6 != null) " + AAAA" else ""} ($sent/3 sent)")
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
        var wantPtr = false; var wantSrv = false; var wantTxt = false; var wantAddr = false
        var wantNsec = false
        for (i in 0 until qd) {
            val (name, after) = dnsReadName(data, pos, len)
            if (after + 4 > len) return
            val qtype = u16(after)
            pos = after + 4
            val any = qtype == T_ANY
            when {
                name.equals(typeFqdn, true) && (qtype == T_PTR || any) -> { wantPtr = true; wantSrv = true; wantTxt = true; wantAddr = true }
                name.equals(serviceFqdn, true) -> {
                    if (qtype == T_SRV || any) { wantSrv = true; wantAddr = true }
                    if (qtype == T_TXT || any) wantTxt = true
                }
                // A or AAAA gets BOTH address records (RFC 6762 §6.2), so the phone never caches one
                // family without the other. AAAA/ANY also gets an NSEC naming exactly the types this
                // name has (RFC 6762 §6.1). It used to name A alone — an authoritative "no AAAA" that
                // left the phone the IPv4 address, which it cannot dial :7011 on. Without IPv6 on br0
                // it still says A-only, which is then true, and spares iOS its own AAAA timeout.
                name.equals(hostname, true) && (qtype == T_A || qtype == T_AAAA || any) -> {
                    wantAddr = true
                    if (qtype == T_AAAA || any) wantNsec = true
                }
            }
        }
        if (!(wantPtr || wantSrv || wantTxt || wantAddr)) return
        // F126: a query received while stop() runs (the loop finishes its last receive after `running`
        // drops) must not be answered: a TTL 120/4500 answer after the goodbye re-caches a receiver
        // that is gone. [send] makes the refusal race-free; this only keeps the log line honest.
        if (!running.get()) { log.i("query after stop — not answered (the goodbye stands)"); return }
        log.i("query -> answering${if (wantPtr) " PTR" else ""}${if (wantSrv) " SRV" else ""}" +
              "${if (wantTxt) " TXT" else ""}${if (wantAddr && advertised6 != null) " AAAA" else ""}" +
              "${if (wantAddr) " A" else ""}${if (wantNsec) " NSEC" else ""}")
        send(buildResponse(wantPtr, wantSrv, wantTxt, wantAddr, nsec = wantNsec))
    }

    /**
     * Once `running` is false only [goodbye] packets go out (F126): announce() and handleQuery() are
     * refused here, under [sendLock], so neither can follow the goodbye.
     */
    private fun send(pkt: ByteArray, goodbye: Boolean = false): Boolean = try {
        synchronized(sendLock) {
            if (!goodbye && !running.get()) {
                log.i("send refused — responder stopped; only goodbyes go out")
                return false
            }
            // No socket is not a send: returning true here made "N/N sent" claim packets that never left.
            val s = sock ?: return false
            s.send(DatagramPacket(pkt, pkt.size, InetAddress.getByName(GROUP), PORT))
            true
        }
    } catch (t: Throwable) { log.w("send: ${t.javaClass.simpleName}: ${t.message}"); false }

    // ---- record construction ----------------------------------------------------------------------

    /**
     * [ttl0] builds the same record set with TTL 0 — an mDNS goodbye. See [goodbye].
     * [addr] emits AAAA (when br0 has an IPv6 link-local) and then A — the order the phone's resolver
     * handed back `Android.local`'s pair on 2026-10-06, the resolve every formed session dialled.
     * [nsec] adds the "these are the only types at this name" assertion to the ADDITIONAL section,
     * which is where RFC 6762 §6.1 puts it.
     */
    private fun buildResponse(ptr: Boolean, srv: Boolean, txt: Boolean, addr: Boolean,
                              nsec: Boolean = false, ttl0: Boolean = false): ByteArray {
        // One read of each: a [refreshAddresses] on the announce thread must not leave this packet
        // with an AAAA that its own NSEC denies, or the reverse.
        val v4 = advertised; val v6 = advertised6
        val answers = ByteArrayOutputStream()
        var n = 0
        if (ptr) { answers.write(recPtr(ttl0)); n++ }
        if (srv) { answers.write(recSrv(ttl0)); n++ }
        if (txt) { answers.write(recTxt(ttl0)); n++ }
        if (addr) {
            v6?.let { answers.write(recAddr(T_AAAA, it, ttl0)); n++ }
            v4?.let { answers.write(recAddr(T_A, it, ttl0)); n++ }
        }
        val additional = ByteArrayOutputStream()
        var ar = 0
        if (nsec) { additional.write(recNsec(v6 != null, ttl0)); ar++ }
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

    /** A (4-byte RDATA) or AAAA (16-byte RDATA, no scope — the scope is the link it arrives on). */
    private fun recAddr(type: Int, a: InetAddress, ttl0: Boolean = false): ByteArray =
        record(hostname, type, CLASS_FLUSH, if (ttl0) 0 else TTL_HOST, a.address)

    /**
     * NSEC for our hostname: "the only record types that exist at this name are A and AAAA" — or A
     * alone when br0 has no IPv6 link-local and we publish no AAAA.
     *
     * RDATA is next-domain-name followed by a type bitmap (RFC 4034 §4.1.2). In mDNS the
     * next-domain field carries the owner name itself and is written UNCOMPRESSED. The bitmap is one
     * window, block 0, and bit 0 is the high bit of the first byte: type A (1) is 0x40 in byte 0,
     * type AAAA (28) is 0x08 in byte 3, so the window is 4 bytes long with both and 1 byte with A only.
     *
     * Keep this minimal and exact: a malformed NSEC makes iOS discard the WHOLE response, taking the
     * address records with it.
     */
    private fun recNsec(aaaa: Boolean, ttl0: Boolean = false): ByteArray {
        val rd = ByteArrayOutputStream()
        rd.write(encodeName(hostname))                  // next domain name = the owner
        if (aaaa) rd.write(byteArrayOf(0, 4, 0x40, 0, 0, 0x08))  // window 0, 4 bytes, { A, AAAA }
        else rd.write(byteArrayOf(0, 1, 0x40))                    // window 0, 1 byte, { A }
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
