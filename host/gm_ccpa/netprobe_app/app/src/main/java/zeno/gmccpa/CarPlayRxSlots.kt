package zeno.gmccpa

import java.net.Inet6Address
import java.net.InetAddress
import java.net.Socket
import java.util.concurrent.atomic.AtomicReference

/*
 * The two single-slot handoffs CarPlayRx shares between threads, kept free of android.* so the
 * host-JVM unit tests run the exact code the receiver runs (plan v5.7 B1-4 / F040, U5 / F039).
 */

/**
 * One accepted control connection. Built ONCE on the accept thread and handed to the pump, so every
 * party (accept, pump, stub pump, stop()) names the same object and identity is unambiguous.
 *
 * [peer] and [scopeId] are taken at accept time. [scopeId] is the IPv6 scope of a link-local peer
 * (0 for IPv4 and global IPv6); it is stored separately because `Inet6Address.equals` compares the
 * address bytes only. [encryptedAt] and [lastRxAt] are written by the native pump (pair-verify
 * milestone, every received chunk) and are volatile so another thread can read them without a lock.
 * acceptLoop reads [encryptedAt] across threads for the admission rule ([Admission], B1-4 F038,
 * owner decision OD-7F) before the liveness increment.
 */
internal class Incumbent(val sock: Socket, val peer: InetAddress?, val scopeId: Int, acceptedAt: Long) {
    /** elapsedRealtime of the ENCRYPTED milestone (pair-verify completed); 0 = not yet. */
    @Volatile var encryptedAt = 0L
    /** elapsedRealtime of the last received byte; starts at accept, like the pump's own clock. */
    @Volatile var lastRxAt = acceptedAt

    override fun toString() = "${peer?.hostAddress}:${sock.port}"

    companion object {
        fun of(c: Socket, acceptedAt: Long): Incumbent {
            val peer = c.inetAddress
            return Incumbent(c, peer, (peer as? Inet6Address)?.scopeId ?: 0, acceptedAt)
        }
    }
}

/**
 * The incumbent control connection slot (F040).
 *
 * It used to be a `@Volatile Socket?` cleared with `if (currentSocket === c) currentSocket = null`.
 * That check-then-write is two steps, and acceptLoop's install could land between them:
 *   pump A's finally reads `currentSocket === A` (true) → accept installs B → pump A writes null.
 * B is then live with an empty slot, so (1) B's own real link loss read "closed by us" (the slot no
 * longer named B) and was reported CLOSED instead of LINK_LOST, and (2) stop() found nothing to close
 * and B stayed open until the 30 s idle backstop. Every clear is now a compareAndSet on the record.
 */
internal class IncumbentSlot {
    private val ref = AtomicReference<Incumbent?>(null)

    fun get(): Incumbent? = ref.get()

    /** Accept: install [rec] and return the connection it supersedes (the hijack target), if any. */
    fun install(rec: Incumbent): Incumbent? = ref.getAndSet(rec)

    /** A connection's own exit: clear the slot only if it still names [rec]. Atomic, unlike `===` then `=`. */
    fun release(rec: Incumbent): Boolean = ref.compareAndSet(rec, null)

    /** stop(): empty the slot and hand back whatever was in it, to be closed. */
    fun clear(): Incumbent? = ref.getAndSet(null)

    /**
     * The pump's "did WE end this connection" test (52D), read in the read-error path before the pump
     * releases its own slot. Both of our closes write their state first: stop() sets running=false
     * before [clear], and a hijack [install]s the new record before closing the old socket.
     */
    fun endedByUs(running: Boolean, rec: Incumbent): Boolean = !running || ref.get() !== rec
}

/**
 * The browse listener slot (F039).
 *
 * The rediscover loop drops its listener and, 600 ms later, installs a fresh one. stop() used to do
 * read-stop-null with no lock, so this interleaving leaked a browse for the life of the process:
 *   loop nulls the slot → stop() sets running=false, reads null, stops nothing → loop installs and
 *   starts a new listener on a receiver that is already stopped.
 * [start] checks [live] and starts the listener under the same lock that [stop] holds for its
 * read-stop-null. Callers set the "not live" state (running=false) BEFORE calling [stop], so a start
 * either completes first (and is stopped by [stop]) or runs after and is refused.
 *
 * [begin] and [end] run under the lock. That is what makes "installed" and "started" one step as
 * seen by [stop]; both are asynchronous NsdManager requests and neither callback takes this lock.
 */
internal class BrowseSlot<L : Any>(private val live: () -> Boolean) {
    private val lock = Any()
    private var listener: L? = null

    /** Install [l] and start it with [begin] if still [live]; false = refused, nothing started. */
    fun start(l: L, begin: (L) -> Unit): Boolean = synchronized(lock) {
        if (!live()) return false
        listener = l
        begin(l)
        true
    }

    /** Remove the current listener, if any, and stop it with [end]. */
    fun stop(end: (L) -> Unit) {
        synchronized(lock) {
            val l = listener ?: return
            listener = null
            end(l)
        }
    }
}

/**
 * Who may open a control connection (B1-4 F038, owner decision OD-7F, 2026-10-07):
 * "no new connections are allowed if an active CarPlay session is active."
 *
 * Decided on the accept thread BEFORE `liveConnections` is incremented, first match wins:
 *  0. a LOCAL newcomer is refused, always, session or not ([isLocal]);
 *  1. no incumbent, or its socket is closed → allow;
 *  2. the incumbent's control channel is not ENCRYPTED yet (pair-verify not done) → allow; it is
 *     superseded as before. No session exists yet, and a stalled pre-verify connection must not lock
 *     the phone out;
 *  3. the incumbent is ENCRYPTED and open → refuse ANY newcomer. No quiet-time or third-try escape.
 *
 * A half-dead incumbent is not kept forever by row 3: keepalive + TCP_USER_TIMEOUT (≤ 12 s) or the
 * pump's idle backstop ends its read, its pump releases the slot, and the phone's next connect meets
 * row 1. The policy is pure so the host JUnit table runs exactly this code.
 */
internal object Admission {
    enum class Verdict(val allowed: Boolean, val rule: String) {
        ALLOW_NO_INCUMBENT(true, "no incumbent"),
        ALLOW_INCUMBENT_CLOSED(true, "incumbent closed"),
        ALLOW_NOT_ENCRYPTED(true, "incumbent not yet encrypted"),
        REFUSE_LOCAL(false, "local"),
        REFUSE_SESSION_LIVE(false, "session live"),
    }

    /**
     * [incumbentPresent]/[incumbentClosed]/[incumbentEncrypted] describe the slot's record at decision
     * time (`encryptedAt != 0` is "encrypted").
     */
    fun decide(
        newcomerLocal: Boolean,
        incumbentPresent: Boolean,
        incumbentClosed: Boolean,
        incumbentEncrypted: Boolean,
    ): Verdict = when {
        newcomerLocal -> Verdict.REFUSE_LOCAL
        !incumbentPresent -> Verdict.ALLOW_NO_INCUMBENT
        incumbentClosed -> Verdict.ALLOW_INCUMBENT_CLOSED
        !incumbentEncrypted -> Verdict.ALLOW_NOT_ENCRYPTED
        else -> Verdict.REFUSE_SESSION_LIVE
    }

    fun decide(newcomerLocal: Boolean, incumbent: Incumbent?): Verdict = decide(
        newcomerLocal,
        incumbentPresent = incumbent != null,
        incumbentClosed = incumbent?.sock?.isClosed ?: false,
        incumbentEncrypted = (incumbent?.encryptedAt ?: 0L) != 0L,
    )

    /**
     * Is the peer this host itself? Any of: loopback; the wildcard address; the socket's own local
     * address (a local app dialling the head unit's br0 address is looped back with peer == local;
     * `Inet6Address.equals` compares bytes only, so the scope does not matter); or an address assigned
     * to ANY local interface ([ownsAddress], production: `NetworkInterface.getByInetAddress(a) != null`),
     * which closes the bypass of binding the source to another of the unit's own addresses. If
     * [ownsAddress] throws, only the first three tests decide. An unknown (null) peer counts as local:
     * a connection whose source cannot be named is not admitted.
     */
    fun isLocal(peer: InetAddress?, localAddr: InetAddress?, ownsAddress: (InetAddress) -> Boolean): Boolean {
        if (peer == null) return true
        if (peer.isLoopbackAddress || peer.isAnyLocalAddress) return true
        if (localAddr != null && peer == localAddr) return true
        return try { ownsAddress(peer) } catch (_: Throwable) { false }
    }

    /**
     * The peer's scope for the refusal line: kind and interface only, never the address. Matches the
     * link-local/loopback split the rest of the receiver's logging reasons in.
     */
    fun scopeOf(peer: InetAddress?): String {
        if (peer == null) return "unknown"
        val kind = when {
            peer.isLoopbackAddress -> "loopback"
            peer.isAnyLocalAddress -> "wildcard"
            peer.isLinkLocalAddress -> "link-local"
            peer.isSiteLocalAddress -> "site-local"
            else -> "global"
        }
        val fam = if (peer is Inet6Address) "IPv6" else "IPv4"
        val iface = (peer as? Inet6Address)?.scopedInterface?.name
            ?: (peer as? Inet6Address)?.scopeId?.takeIf { it > 0 }?.let { "scope $it" }
        return if (iface != null) "$fam $kind %$iface" else "$fam $kind"
    }
}

/**
 * The accept-side step of [Admission], pure so the host JUnit tests run it on real sockets.
 *
 * [admit] judges the newcomer [c] against the record in [slot] right now. A refused newcomer is
 * closed here and reported through [onRefused] (the receiver's W line), and the caller gets null: it
 * must `continue` without touching the liveness count, the dial counter or the slot. An admitted one
 * comes back as the record to install, with the verdict that admitted it.
 *
 * [isLocal] is injectable only so the tests can reach rows 1-3 from a loopback socket; production
 * uses [Admission.isLocal] with `NetworkInterface.getByInetAddress(a) != null` as the ownership test.
 */
internal class AcceptGate(
    private val slot: IncumbentSlot,
    private val onRefused: (String) -> Unit,
    private val isLocal: (Socket) -> Boolean,
) {
    class Admitted(val rec: Incumbent, val verdict: Admission.Verdict)

    fun admit(c: Socket, now: Long): Admitted? {
        // The interface lookup first: it is the slow part, and the incumbent is read after it so the
        // record judged is as fresh as possible when the caller installs.
        val local = try { isLocal(c) } catch (_: Throwable) { true }
        val cur = slot.get()
        val verdict = Admission.decide(local, cur)
        if (verdict.allowed) return Admitted(Incumbent.of(c, now), verdict)
        val inc = if (cur == null) "no session live"
                  else "incumbent $cur encrypted ${if (cur.encryptedAt != 0L) "${now - cur.encryptedAt} ms ago" else "no"}, " +
                       "quiet ${now - cur.lastRxAt} ms"
        // The same address form as the receiver's INBOUND line (export redaction applies to both).
        onRefused("refusing control connection from ${c.inetAddress?.hostAddress}:${c.port} " +
                  "[${Admission.scopeOf(c.inetAddress)}] (${verdict.rule}): $inc")
        try { c.close() } catch (_: Throwable) {}
        return null
    }
}
