package zeno.gmccpa

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import java.io.BufferedInputStream
import java.io.OutputStream
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.Executors
import zeno.gmccpa.pair.MfiRelay
import zeno.gmccpa.pair.NativeCore
import zeno.gmccpa.pair.SrpServer
import zeno.gmccpa.pair.Tlv8
import java.security.SecureRandom
import java.util.concurrent.atomic.AtomicBoolean

/**
 * A **CarPlay-identity** AirPlay receiver — the successor to the feasibility-era `AirPlayRx` probe
 * (now removed).
 *
 * That probe advertised `model=AppleTV3,2` with `features=0x5A7FFFF7,0x1E`, whose high word lacks
 * **bit 32 (Car)**, so iOS engaged it as an Apple TV doing Screen Mirroring. This one presents the
 * production CCPA identity — `features=0x44440B80,0x61` = **Car(32) | CarPlayControl(37) |
 * HKPairing(38)** — so iOS engages the CarPlay path instead.
 *
 * Two things are load-bearing and easy to get wrong:
 *
 *  1. **Discovery is bidirectional.** Advertising `_airplay._tcp` is not enough. The accessory also
 *     browses `_carplay-ctrl._tcp`, finds the phone, and dials it with `GET /ctrl-int/1/connect`.
 *     Only then does the phone open the control connection *inbound*. Without that nudge, nothing
 *     happens no matter how correct the advert is.
 *  2. **TXT `features` must equal `/info` `features`**, and TXT `deviceid` / `pi` must equal the
 *     receiver's, or pair-verify fails later.
 *
 * This probe stops where the crypto begins. `pair-setup` (SRP-6a) and `pair-verify`
 * (Curve25519/Ed25519) need primitives the API-32 platform does not expose, which is what the JNI'd
 * Rust core exists for. Everything up to that point — discovery, the connect-out, the inbound
 * control connection, `/info`, and which requests iOS actually issues on the CarPlay path — is
 * observable here, and none of it has been observed before.
 */
class CarPlayRx(
    ctx: Context,
    private val port: Int = 7011,
    /** Must ultimately equal the identity the CCPA presents over Bluetooth (the box_identity problem). */
    private val deviceId: String = BOX_DEVICE_ID,
    private val pi: String = BOX_PI,
    /** Bridge to the device-proven OCBM CH_MFI relay; null falls back to the Kotlin stub path. */
    private val mfiRelay: MfiRelay? = null,
    /**
     * Fired the instant the phone opens the control connection — BEFORE pair-verify, and long
     * before the phone SETUPs any stream.
     *
     * This is the trigger that lets the A/V consumers stand by. Without it the seams did not exist
     * until an operator ran `carplay_ui`, so from RECORD onward the phone streamed into a closed
     * port and `forward.rs` dropped every access unit until something answered — measured as
     * multiple seconds of black screen on a session that was otherwise healthy (2026-08-12).
     * Runs on the accept thread: hand off, do not block.
     */
    var onSessionUp: (() -> Unit)? = null,
    /**
     * Fired when the last live control connection goes away — clean TEARDOWN and abrupt link loss
     * alike, because the two are indistinguishable here (the phone simply stops reading).
     *
     * Nothing observed session END before this existed. The A/V consumers, the foreground service and
     * `MainActivity.sessionUp` were all stood up on [onSessionUp] and never torn back down, so after
     * the phone left, the CarPlay screen stayed foreground over a dead session, swallowing touches
     * (`touch move … sent=false`) and re-launching itself from `onResume`. Device-observed 2026-08-27.
     * Runs on the dying pump thread: hand off, do not block.
     */
    var onSessionDown: (() -> Unit)? = null,
    /**
     * Fired once when [STALL_DIALS] consecutive connect-outs are accepted by the phone without it
     * ever dialling back. Argument is the dial count. See [noteDialAccepted] for what this means.
     */
    var onStalled: ((Int) -> Unit)? = null,
    /** Fired on every connect-out the phone accepts, so the supervisor can start its dial-back clock. */
    var onDialAccepted: (() -> Unit)? = null,
    /** Where controller-id -> Ed25519 LTPK pairings persist across runs. */
    private val peerFile: String? = null,
    /** Stable 32-byte Ed25519 accessory seed. Changing it invalidates every existing pairing. */
    private val edSeed: ByteArray = ByteArray(32) { (it * 31 + 7).toByte() }
) {
    /**
     * The SAME device id, decimal. This asymmetry is deliberate and is the thing that was broken:
     * TXT `deviceid` and /info `deviceID` carry the colon MAC, but the
     * `AirPlay-Receiver-Device-ID` HTTP header carries a DECIMAL uint64 (`mac_to_dec` in
     * `ccpa/carplayd/src/discovery.rs`).
     * Sending the MAC string makes iOS parse it base-10 as 0, look up "receiver 0" among the peers
     * it has browsed, find nothing, and have nothing to dial back to — which is exactly the
     * accepted-connection / no-reply / no-inbound symptom.
     */
    private val deviceIdDec: Long = deviceId.split(":").fold(0L) { v, p -> (v shl 8) or p.toLong(16) }

    /**
     * APPLICATION context, never the Activity's.
     *
     * This receiver is process-scoped and deliberately outlives the UI (see [stop]'s note and
     * `SessionHolder` in MainActivity), so holding an Activity context here would pin a destroyed
     * Activity for the life of the session. The four callbacks above are `var` for the same reason:
     * a new Activity generation rebinds them rather than constructing a second receiver, which
     * cannot bind :$port anyway.
     */
    private val ctx: Context = ctx.applicationContext

    /**
     * Set by [stop], read by every callback fire. Fences LATE callbacks from a stopped instance.
     *
     * `stop()` interrupts the pumps but cannot unblock one that is inside `NativeCore.feed` -- that
     * is JNI, not `read()`, and `/auth-setup` can sit in the synchronous MFi relay for 12-15 s. So
     * the bounded 1500 ms drain below legitimately times out, and the old pump's `finally` then
     * fired `onSessionDown` seconds later, into whatever receiver had replaced this one. Forget
     * Pairing does exactly stop-then-replace, so a brand-new receiver that never had a fault was
     * driven straight into GRACE -- which suspends discovery -- and the peer file it had just
     * deleted could be resurrected by the still-live native core's `save_peer`.
     */
    @Volatile private var stopped = false

    /** Fire an outward callback unless this instance has been stopped. See [stopped]. */
    private inline fun fenced(what: String, body: () -> Unit) {
        if (stopped) { log.i("suppressing $what from a stopped receiver"); return }
        runCatching(body).onFailure { log.w("$what threw: ${it.message}") }
    }

    private val log = ProbeLog.sub("cprx")
    private val running = AtomicBoolean(false)
    /** Written in [start], read by [selfTest] and [stop] on other threads — publication must be safe. */
    @Volatile private var server: ServerSocket? = null
    private var nsd: NsdManager? = null
    private var regListener: NsdManager.RegistrationListener? = null
    /** Our own mDNS responder, publishing a hostname NsdManager will not let us set. See [advertise]. */
    private var selfMdns: MdnsResponder? = null
    private var discListener: NsdManager.DiscoveryListener? = null
    private val pool = Executors.newCachedThreadPool()
    private val lastDial = java.util.concurrent.ConcurrentHashMap<String, Long>()

    /** The incumbent control connection, for hijack: a new inbound supersedes and closes it. */
    @Volatile private var currentSocket: Socket? = null
    /** Number of live control connections. Drives rediscovery: >0 = a session is up, pause the churn;
     *  0 = the phone left, resume forcing fresh resolves so the NEXT session can start. */
    private val liveConnections = java.util.concurrent.atomic.AtomicInteger(0)

    /**
     * Connect-outs the phone has ACCEPTED since the last inbound control connection.
     *
     * The stall signature this counts is real and was reproduced three times on 2026-08-27: the phone
     * is on br0, advertises `_carplay-ctrl._tcp`, answers `GET /ctrl-int/1/connect` with `200 OK` —
     * and never opens the control connection. iOS-side the cause is visible as a browse that reads our
     * `_airplay._tcp` TXT and never follows up with an SRV query, so it holds no address to dial.
     * Every successful session in that capture was preceded by an SRV query; every failure was not.
     */
    private val dialsSinceInbound = java.util.concurrent.atomic.AtomicInteger(0)

    /**
     * Suspends the rediscover churn and the connect-out without stopping the receiver.
     *
     * `rediscoverLoop` restarts discovery every 12 s whenever nothing is connected, and the 5 s dial
     * cooldown is keyed by `addr:port` — which the phone rotates every session, so a re-announce
     * arrives under a fresh key with no cooldown at all. Any deliberate wait shorter than 12 s was
     * therefore unimplementable: the loop would redial into the middle of it. The supervisor's grace
     * window needs this lever, and so does any future "restart the stack cleanly" rung.
     */
    private val discoveryPaused = AtomicBoolean(false)

    /** Is a control connection up right now? The box/attach paths need this to avoid disturbing a
     *  live session; nothing outside could see it before. */
    val sessionLive: Boolean get() = liveConnections.get() > 0

    /**
     * How many accepted control connections have not yet finished.
     *
     * Exposed for callers that must sequence destructive work behind a stop that [stop] reported as
     * un-drained — a pump parked in `NativeCore.feed` still owns a live native core. Zero here means
     * every pump has run its `finally`, so no core can write shared state any more.
     */
    fun liveConnectionCount(): Int = liveConnections.get()

    /** When the advert went live. The connect-out must not run until iOS has had time to index it. */
    @Volatile private var advertisedAt = 0L

    /** `onRegistrationFailed` fired — [advertisedAt] will never be stamped, so stop waiting on it. */
    @Volatile private var advertFailed = false

    /** One connect-out at a time. See the guard in [connectOutWithRetry]. */
    private val dialInFlight = AtomicBoolean(false)

    /** One SRP-6a exchange per control connection. */
    private var srp: SrpServer? = null

    companion object {
        /**
         * The identity the CCPA derives for itself, which the app's Bonjour advert MUST match — iOS
         * ties the Wi-Fi accessory to the one it authenticated over Bluetooth (the box_identity
         * problem). `box_identity::derive()` is SHA-256 over a hardware id with a domain tag.
         *
         * In the BT-only bridge role the hardware id is NOT per-box: `/sys/class/net/wlan0/address`
         * is absent (no Wi-Fi driver is ever loaded) and `/sys/devices/soc0/serial_number` does not
         * exist on this SoC, so the derivation falls through to `/proc/cpuinfo` Serial — which reads
         * `0000000000000000`. Every adapter in this role therefore derives the SAME identity. That is
         * the degenerate case box_identity.rs was written to avoid; it is harmless with one adapter
         * and must be fixed before a second one exists.
         *
         * Also note the identity CHANGES between AP-on and AP-off roles, because wlan0 exists in one
         * and not the other. A phone paired in one role holds a stale record for the other.
         */
        // Derived from the box's REAL /etc/serial_number, not the degenerate fallback.
        //
        // box_identity::derive() prefers wlan0's MAC, then the SoC serial, then /proc/cpuinfo Serial.
        // In this bridge role the first two are absent and the third reads "0000000000000000", so the
        // stock derivation collapses to ONE identity shared by every adapter (DA:BF:58:F4:7F:18) —
        // exactly the fleet-collapse box_identity.rs was written to prevent, and exactly the observed
        // behaviour where an iPhone registers several Carlinkit adapters as a single vehicle no matter
        // what their Bluetooth or Wi-Fi names are. Same SHA-256 derivation, real hardware id.
        // Per-box identity, derived by box_identity::derive()'s own SHA-256 scheme over the box's
        // real /etc/serial_number rather than the degenerate all-zeros fallback.
        //
        // A diagnostic swap to the box's own (degenerate) value was tried and is NOT the answer: it
        // only produced a mismatch against iOS's CACHED endpoint, because iOS indexes the endpoint by
        // deviceid and holds it for the record TTL. Lesson worth keeping — changing the advertised
        // identity mid-session guarantees a spurious "No matching endpoint found" until the cache ages
        // out, so identity must be pinned before a test run, not varied inside one.
        const val BOX_DEVICE_ID = "B2:D6:2A:9F:C9:30"
        const val BOX_PI = "a732fdef-5f99-78a2-e43b-bd10c9df7506"

        /**
         * Bonjour instance name. NOT "CarPlay": GM's own receiver already advertises
         * "CarPlay._airplay._tcp" from this same host (192.168.5.1) on :7000, and that component
         * cannot be disabled from shell on this user build. The upstream reference hardcodes
         * "CarPlay" only because nothing competes on the box. "carlink" matches the accessory family
         * the box already names itself after (CarLink-626a).
         */
        // Must collide with NOTHING on this link or in the phone's bonded-device list. "CarPlay" is
        // GM's own instance name on this very host; "carlink" is the Bluetooth name of a DIFFERENT
        // Carlinkit adapter this phone has been bonded to.
        const val ACCESSORY_NAME = "gm-ccpa"
        /** Instance name for the self-hosted advert. MUST differ from [ACCESSORY_NAME] so the two
         *  adverts do not collide, and it becomes our SRV target `<name>.local` — the whole point. */
        const val SELF_HOSTED_NAME = "gmccpa-rx"
        const val BPLIST = "application/x-apple-binary-plist"
        /**
         * The proven CarPlay source version. 980.71.1 is an AirPlay-2 / Apple-TV-era string and can
         * steer iOS down a different negotiation path on a service claiming the Car bit; every
         * working CarPlay reference uses the 320.x family.
         */
        const val SRCVERS = "320.17"
        /** Car(32) | CarPlayControl(37) | HKPairing(38) — the production CCPA value. */
        const val FEATURES = "0x44440B80,0x61"
        /** How long before the same peer is nudged again. NsdManager re-resolves often. */
        /**
         * Re-nudge cooldown. Was 20 s, when a wasted dial was expensive because the only address we
         * had came from NsdManager's cache and a miss cost the whole window. Dials are now aimed by
         * a live wire lookup, so a miss is informative and cheap — and a long cooldown was pure
         * added latency on the critical path (measured: 48.5 s advert -> connect-out, 2026-08-12).
         */
        private const val REDIAL_COOLDOWN_MS = 5_000L
        /**
         * Accepted-but-unanswered dials before we call it stalled. Six is ~75 s at the rediscover
         * cadence — long enough that a phone merely being slow is not misreported, short enough that
         * the driver is told something before they give up.
         */
        private const val STALL_DIALS = 6
        /** How long the advert must be live before the first dial. iOS indexes asynchronously. */
        /**
         * Settle after the advert is CONFIRMED registered — not after we asked for it.
         *
         * The old 15 s was a fixed guess standing in for "iOS has indexed us", which we cannot
         * observe directly. What we CAN observe is the real precondition pair: NsdManager firing
         * onServiceRegistered, and the phone publishing _carplay-ctrl._tcp (it only does that once
         * it is on br0 and ready). Gate on both events, then allow a short margin for iOS to commit
         * the record — and rely on the fast retry above rather than one long pre-emptive wait.
         */
        private const val ADVERT_SETTLE_MS = 2_500L
        /** How long a connect-out waits for `onServiceRegistered` before giving this cycle up. */
        private const val ADVERT_WAIT_MS = 30_000L
        private val INTERESTING = setOf(
            "content-type", "content-length", "user-agent", "cseq", "x-apple-device-id",
            "active-remote", "dacp-id", "airplay-receiver-device-id", "connection"
        )
    }

    fun start() {
        if (!running.compareAndSet(false, true)) { log.i("already running"); return }
        // Every caller is already off the main thread, and the bind below must stay that way. Say so
        // if that ever stops being true rather than letting it surface as a NetworkOnMainThread.
        if (android.os.Looper.myLooper() == android.os.Looper.getMainLooper())
            log.w("start() is running on the main thread — the :$port bind does not belong here")
        nsd = ctx.getSystemService(Context.NSD_SERVICE) as NsdManager
        // ORDER IS LOAD-BEARING. `GET /ctrl-int/1/connect` does not make iOS dial us; it sets a
        // PENDING AUTOCONNECT which iOS resolves immediately against its ALREADY-POPULATED Wi-Fi
        // endpoint index. Observed on device: the lookup ran and logged
        //   carManager_handlePendingAutoconnect: No matching endpoint found for deviceID ...
        // and our own record landed 16 ms later. Advertising and dialing together is a race we lose.
        //
        // carplayd never hits this because its discovery thread registers the advert (`mdns.register`,
        // `fn run` in `ccpa/carplayd/src/discovery.rs`) BEFORE it browses (`mdns.browse`, three lines
        // later), and on the box the advert has been live for the whole AP lifetime before the phone
        // even associates. So: advertise first, let iOS index it, only then dial.
        //
        // BIND HERE, NOT ON THE POOL THREAD. `server` used to be assigned inside acceptLoop, and
        // MainActivity calls reportReceiverHealth(rx) synchronously right after start() — so the
        // health check routinely read a null `server`, reported "FAIL listening", and drove the
        // supervisor straight into RX_UNHEALTHY on a receiver that was about to be perfectly fine.
        // Binding before the dispatch makes the first health reading true, and makes a genuine bind
        // failure say so in its own line instead of surfacing as a mysterious health FAIL.
        val srv = try {
            ServerSocket().apply { reuseAddress = true; bind(InetSocketAddress(port)) }
        } catch (t: Throwable) {
            log.e("listen bind FAILED on :$port — ${t.javaClass.simpleName}: ${t.message}"); null
        }
        server = srv
        if (srv != null) {
            log.i("listening on :$port")
            pool.execute { acceptLoop(srv) }
        }
        // NOT set here. This is the request, not the confirmation — starting the settle clock now
        // charges NsdManager's own registration latency against the margin that exists to cover
        // iOS's indexing. onServiceRegistered is the event that means the record is really live.
        advertisedAt = 0L
        advertFailed = false
        advertise()
        browseForPhone()
        pool.execute { rediscoverLoop() }

        log.i("CarPlay RX up on :$port deviceid=$deviceId features=$FEATURES")
        log.i("=> advertising _airplay._tcp AND browsing _carplay-ctrl._tcp (discovery is bidirectional)")
    }

    /**
     * Stop, and report whether the pumps actually unwound.
     *
     * The boolean is load-bearing for callers doing destructive work behind the stop: Forget Pairing
     * deletes the peer file, and a pump still inside `NativeCore.feed` owns a live core that can
     * `save_peer` afterwards and resurrect it. A `false` here means "a native generation may still
     * be running" -- late CALLBACKS are fenced by [stopped], but shared on-disk state is not.
     */
    fun stop(): Boolean {
        // BEFORE anything else: from here on this instance speaks to nobody. See [stopped].
        stopped = true
        running.set(false)
        try { server?.close() } catch (_: Throwable) {}
        // Close the live control socket so its pump thread unblocks, breaks, and destroys its own
        // native generation in the finally. Without this, stop() left the native core running and the
        // pump blocked in read() for up to 30 s — a zombie session that collides with the next instance.
        try { currentSocket?.close() } catch (_: Throwable) {}
        currentSocket = null
        regListener?.let { try { nsd?.unregisterService(it) } catch (_: Throwable) {} }
        regListener = null
        selfMdns?.let { runCatching { it.stop() }.onFailure { t -> log.w("self-mdns stop: ${t.message}") } }
        selfMdns = null
        discListener?.let { try { nsd?.stopServiceDiscovery(it) } catch (_: Throwable) {} }
        discListener = null
        pool.shutdownNow()   // every sleep site returns on InterruptedException
        // WAIT for the pumps to unwind before returning.
        //
        // shutdownNow() interrupts but does not join, so stop() used to return while a pump was still
        // in its finally — and that finally is what fires onSessionDown. A caller that stops this
        // receiver and immediately constructs a replacement (Forget Pairing does exactly that) would
        // therefore get the OLD instance's session-down delivered AFTER the new one had already
        // reported itself ready, driving the supervisor into GRACE — which suspends discovery — on a
        // brand-new receiver that never had a fault. Bounded, because a wedged pump must not make Stop
        // unresponsive; if the wait times out the caller is told rather than left to guess.
        val drained = try { pool.awaitTermination(1500, java.util.concurrent.TimeUnit.MILLISECONDS) }
        catch (_: InterruptedException) { Thread.currentThread().interrupt(); false }
        if (!drained) log.w("pumps did not unwind within 1500ms — a pump is parked in NativeCore.feed")
        log.i("CarPlay RX stopped")
        return drained
    }

    // ---- our advert -------------------------------------------------------------------------------

    /**
     * Advertise via NsdManager.
     *
     * I replaced this with a hand-written responder earlier today on the inference that iOS was
     * never receiving our SRV record. That inference was WRONG: `Bonjour SRV Add` / `Bonjour A Add`
     * never appear in iOS's log for ANY device — including Apple TVs — so their absence proved
     * nothing. The decisive counter-evidence is positive: `[WiFi] Bonjour device added/updated`
     * fires for our service under NsdManager and fires ZERO times under the custom responder. Getting
     * into iOS's Wi-Fi endpoint index is the whole game — the pending autoconnect is resolved against
     * it — so we use the thing that demonstrably achieves it.
     *
     * The custom responder is kept (it verifies byte-clean) for the case where we need to pin the
     * advertised address, but it is not the default until it is shown to produce an endpoint.
     */
    private fun advertise() {
        val listenPort = port
        val info = NsdServiceInfo().apply {
            serviceName = ACCESSORY_NAME
            serviceType = "_airplay._tcp"
            setPort(listenPort)
            for ((k, v) in txtRecord()) setAttribute(k, v)
        }
        val l = object : NsdManager.RegistrationListener {
            override fun onServiceRegistered(s: NsdServiceInfo) {
                log.i("advertise _airplay._tcp:$listenPort REGISTERED as '${s.serviceName}'")
                advertisedAt = System.currentTimeMillis()
            }
            override fun onRegistrationFailed(s: NsdServiceInfo, c: Int) {
                // Latch it. advertisedAt is stamped only by onServiceRegistered, so without this the
                // connect-out waits on an event that is never coming.
                advertFailed = true
                log.e("advertise FAILED code=$c")
            }
            override fun onServiceUnregistered(s: NsdServiceInfo) {}
            override fun onUnregistrationFailed(s: NsdServiceInfo, c: Int) {}
        }
        regListener = l
        nsd?.registerService(info, NsdManager.PROTOCOL_DNS_SD, l)

        // ---- second advert, on a hostname WE own -------------------------------------------------
        // iOS keys its Wi-Fi CarPlay endpoint index by HOST, one endpoint per host, and whichever
        // Bonjour record lands first owns it. Captured from the phone on 2026-08-12:
        //
        //   Bonjour TXT Add CarPlay._airplay._tcp.local.        <- GM's
        //   Bonjour TXT Add gm-ccpa._airplay._tcp.local.        <- ours, 27 MICROSECONDS later
        //   Created APEndpointCarPlay [0xE051] ... id 'F8:6D:CC:DC:32:D6'
        //
        // One endpoint, stamped with GM's identity; ours was discarded. Every later autoconnect then
        // failed with "No matching endpoint found for deviceID B2:D6:2A:9F:C9:30" — iOS was looking
        // for us and we were not in the index. Both SRV records target the platform's `Android.local`,
        // so we lose that race whenever GM registers first (it starts at boot, we start ~51 s later).
        //
        // NsdManager cannot set the SRV target — AOSP drops setHost() on registration — so the only
        // way to stop sharing GM's host is to answer mDNS ourselves. MdnsResponder owns
        // `$instance.local` and publishes exactly ONE address, the br0 bridge the phone is on, which
        // also sidesteps `Android.local` resolving to three addresses of which two are unroutable
        // from the phone's subnet.
        //
        // Run it ALONGSIDE NsdManager rather than instead of it: the NsdManager advert is the one iOS
        // has demonstrably indexed in the past, so keep it as the fallback and let this one add a
        // SECOND, separately-keyed endpoint carrying our deviceID. A distinct instance name keeps the
        // two from colliding.
        //
        // ACCEPTANCE TEST, on the phone: a second `Created APEndpointCarPlay ... id
        // 'B2:D6:2A:9F:C9:30'`. That, not the absence of errors, is what proves this worked.
        // Stop any previous responder before replacing it. Overwriting the field leaks the old one,
        // which keeps its 5353 socket and answers every query a second time — visible in logcat as
        // paired "query -> answering" lines from two threads.
        selfMdns?.let { runCatching { it.stop() } }
        selfMdns = null
        runCatching {
            val r = MdnsResponder(SELF_HOSTED_NAME, "_airplay._tcp", listenPort, txtRecord(), log)
            if (r.start()) { selfMdns = r; log.i("self-hosted advert up: $SELF_HOSTED_NAME.local -> br0:$listenPort") }
            else log.w("self-hosted advert did not start — NsdManager advert stands alone")
        }.onFailure { log.w("self-hosted advert threw: ${it.javaClass.simpleName}: ${it.message}") }
    }

    /** The TXT set, byte-identical to carplayd's (`fn run` in `ccpa/carplayd/src/discovery.rs`, the `("deviceid", …)…("srcvers", …)` tuple list). */
    private fun txtRecord(): Map<String, String> = linkedMapOf(
        "deviceid" to deviceId,
        // The Car bit (high word bit 32) is what makes iOS open RTSP back to the advertised port.
        "features" to FEATURES,
        "flags" to "0x4",
        "model" to "CarLink-mac-1.0",
        "protovers" to "1.0",
        "pi" to pi,
        "srcvers" to SRCVERS
        // No `pk`: the Ed25519 long-term public key is exchanged inside pairing, not advertised.
    )

    // ---- the outbound nudge -----------------------------------------------------------------------

    /**
     * The iPhone's `_carplay-ctrl` port CHANGES every session (58028 -> 58095 -> 58241 observed), and
     * NsdManager happily answers a resolve from its cache — so a single resolve at startup dials a
     * dead port forever (5s timeouts while the phone is off-network, then ECONNREFUSED once it is
     * back). carplayd's discovery loop (`ccpa/carplayd/src/discovery.rs`) never hits this because
     * mdns-sd delivers a fresh ServiceResolved on every
     * re-announce. Restarting discovery is the only way to force NsdManager to re-resolve, so do it
     * on a cadence until the phone actually dials us back.
     */
    private fun rediscoverLoop() {
        // Drive on liveConnections, not a one-shot inboundSeen latch: the phone's ctrl port changes
        // every session, so when it leaves (liveConnections -> 0) we must resume forcing fresh
        // resolves or the NEXT session never starts. Pause the churn while a session is up.
        while (running.get()) {
            try { Thread.sleep(12_000) } catch (_: InterruptedException) { return }
            if (!running.get()) return
            if (liveConnections.get() > 0) continue
            if (discoveryPaused.get()) continue   // a deliberate wait is in progress; do not preempt it
            log.i("no live control connection — restarting discovery to force a FRESH resolve")
            // Do NOT clear lastDial here. Doing so cancelled the cooldown entirely, so every 12s
            // restart spawned another retry loop on top of the ones still running — up to a dozen
            // concurrent dialers hammering one phone port. carplayd's discovery loop dedups (the
            // `dialed` set in `ccpa/carplayd/src/discovery.rs`) and only clears on ServiceRemoved.
            discListener?.let { try { nsd?.stopServiceDiscovery(it) } catch (_: Throwable) {} }
            discListener = null
            try { Thread.sleep(600) } catch (_: InterruptedException) { return }
            browseForPhone()
        }
    }

    private fun browseForPhone() {
        val l = object : NsdManager.DiscoveryListener {
            override fun onDiscoveryStarted(t: String) = log.i("browsing _carplay-ctrl._tcp ...")
            override fun onServiceFound(s: NsdServiceInfo) {
                log.i("found _carplay-ctrl peer '${s.serviceName}' — resolving")
                resolve(s)
            }
            override fun onServiceLost(s: NsdServiceInfo) { log.w("lost _carplay-ctrl peer '${s.serviceName}'"); lastDial.clear() }
            override fun onDiscoveryStopped(t: String) {}
            override fun onStartDiscoveryFailed(t: String, c: Int) = log.e("browse FAILED code=$c")
            override fun onStopDiscoveryFailed(t: String, c: Int) {}
        }
        discListener = l
        nsd?.discoverServices("_carplay-ctrl._tcp", NsdManager.PROTOCOL_DNS_SD, l)
    }

    private fun resolve(s: NsdServiceInfo) {
        @Suppress("DEPRECATION")
        nsd?.resolveService(s, object : NsdManager.ResolveListener {
            override fun onResolveFailed(si: NsdServiceInfo, code: Int) = log.w("resolve FAILED '${si.serviceName}' code=$code")
            override fun onServiceResolved(si: NsdServiceInfo) {
                val raw = si.host ?: return
                // NsdManager hands back a link-local IPv6 with NO scope id, and connecting to one
                // without a scope fails EINVAL — there is no way for the stack to know which
                // interface to use. Re-bind it to the interface the hotspot bridge lives on.
                val host = withScope(raw)
                val key = "${host.hostAddress}:${si.port}"
                log.i("resolved '${si.serviceName}' -> $key")
                // Re-dial on a cooldown rather than once ever: the phone needs a fresh nudge after
                // each Bluetooth handoff, and dialing exactly once meant a second handoff in the same
                // app session was never followed up.
                // NEVER nudge a phone that is already talking to us. iOS treats /ctrl-int/1/connect as
                // a new session request and opens a FRESH inbound connection, which acceptLoop's hijack
                // then uses to kill the incumbent — mid-handshake. That is self-inflicted: the phone is
                // not retrying, we are provoking it. Observed as a ~6 s cycle (the cooldown plus dial
                // latency) with a new generation each time, gen=5,6,7..., none surviving long enough to
                // finish activation. The cooldown alone cannot cover it: the phone's _carplay-ctrl port
                // changes every session, so a re-announce arrives under a new key with no cooldown at all.
                if (liveConnections.get() > 0) {
                    log.i("   (control connection already live — not nudging)")
                    return
                }
                val now = System.currentTimeMillis()
                val last = lastDial[key] ?: 0L
                if (now - last < REDIAL_COOLDOWN_MS) {
                    log.i("   (dialed ${(now - last) / 1000}s ago — within the ${REDIAL_COOLDOWN_MS / 1000}s cooldown, skipping)")
                    return
                }
                lastDial[key] = now
                pool.execute { connectOutWithRetry(si.serviceName, host, si.port) }
            }
        })
    }

    /**
     * Attach a scope id to a bare link-local IPv6 address. The vehicle hotspot bridge (`br0`) is
     * preferred; any other multicast-capable interface carrying a link-local address is the
     * fallback. Non-link-local and already-scoped addresses pass through untouched.
     */
    private fun withScope(addr: InetAddress): InetAddress {
        if (addr !is java.net.Inet6Address) return addr
        if (!addr.isLinkLocalAddress || addr.scopeId != 0) return addr
        val nifs = try { java.net.NetworkInterface.getNetworkInterfaces()?.toList() ?: emptyList() }
                   catch (t: Throwable) { emptyList() }
        val candidates = nifs.filter { nif ->
            try {
                nif.isUp && !nif.isLoopback &&
                    nif.inetAddresses.toList().any { it is java.net.Inet6Address && it.isLinkLocalAddress }
            } catch (t: Throwable) { false }
        }
        val chosen = candidates.firstOrNull { it.name.startsWith("br") } ?: candidates.firstOrNull()
        if (chosen == null) { log.w("no interface with a link-local IPv6 — cannot scope ${addr.hostAddress}"); return addr }
        return try {
            java.net.Inet6Address.getByAddress(null, addr.address, chosen).also {
                log.i("scoped ${addr.hostAddress} to %${chosen.name}")
            }
        } catch (t: Throwable) { log.w("scoping failed: ${t.message}"); addr }
    }

    /**
     * `GET /ctrl-int/1/connect` — the accessory→phone nudge. Surfaces phone-side as
     * `HandleControlServerEvent command 'connect'`. We never dial the phone for the control channel
     * itself; the phone opens that inbound afterwards.
     */
    private fun connectOutWithRetry(serviceName: String, host: InetAddress, peerPort: Int) {
        // SINGLE-FLIGHT. rediscoverLoop restarts discovery every 12 s and each restart re-resolves
        // under a fresh addr:port key, so every cycle used to dispatch its own dialer on top of the
        // ones still running — four concurrent `live lookup` threads were observed in one process,
        // all racing the same phone. One dial at a time; a stacked cycle is dropped, not queued.
        if (!dialInFlight.compareAndSet(false, true)) {
            log.i("   (a connect-out is already in flight — not stacking another)")
            return
        }
        try { dialOut(serviceName, host, peerPort) } finally { dialInFlight.set(false) }
    }

    private fun dialOut(serviceName: String, host: InetAddress, peerPort: Int) {
        // Hold off until the advert has been up long enough for iOS to have committed it to the
        // endpoint index it is about to search. Dialing early is not merely wasted — the pending
        // autoconnect is consumed and resolved against an index that does not contain us yet.
        // Event-gated, then a short margin. advertisedAt == 0 means onServiceRegistered has not
        // fired yet, so there is nothing for iOS to have indexed and dialling cannot succeed —
        // waiting on the event is strictly better than waiting on a clock.
        //
        // BOUNDED, and logged once. This used to spin at 4 Hz with a log line per tick and no exit
        // but stop(), so a dropped onServiceRegistered callback parked a pool thread for the life of
        // the process while filling the capture.
        val holdUntil = System.currentTimeMillis() + ADVERT_WAIT_MS
        var held = false
        while (running.get() && advertisedAt == 0L && !advertFailed) {
            if (System.currentTimeMillis() >= holdUntil) {
                log.w("advert never REGISTERED within ${ADVERT_WAIT_MS / 1000}s — abandoning this connect-out")
                return
            }
            if (!held) { log.i("advert not yet REGISTERED — holding the connect-out until it is"); held = true }
            try { Thread.sleep(250) } catch (_: InterruptedException) { return }
        }
        if (!running.get()) return
        if (advertisedAt == 0L) {
            // Registration failed outright. The self-hosted responder is a real advert in its own
            // right — it carries the same TXT, on a hostname we own — so if it is up there IS
            // something for iOS to have indexed and the dial is worth making. If it is not, nothing
            // is published and the nudge can only be resolved against an index we are absent from.
            if (selfMdns == null) {
                log.w("advert registration FAILED and no self-hosted advert — abandoning this connect-out")
                return
            }
            log.w("advert registration FAILED — dialling against the self-hosted advert instead")
        }
        var settled = advertisedAt + ADVERT_SETTLE_MS
        while (running.get() && System.currentTimeMillis() < settled) {
            settled = advertisedAt + ADVERT_SETTLE_MS   // re-read: a re-register moves the mark
            val left = settled - System.currentTimeMillis()
            if (left > 0) {
                log.i("holding the connect-out ${left / 1000}s longer so iOS can index our advert first")
                try { Thread.sleep(minOf(left, 2000L)) } catch (_: InterruptedException) { return }
            }
        }
        if (!running.get()) return
        // carplayd's connect-out (`ccpa/carplayd/src/discovery.rs`) retries up to 10x at 1s and only
        // marks a peer dialed on success. The phone
        // holds a 30s assertion on the connect command, so an early dial is simply wasted.
        // THE WIRE IS THE AUTHORITY; NsdManager is only the discovery trigger.
        //
        // Device-observed 2026-08-12: NsdManager answered every resolve from a cache that was stale in
        // BOTH fields. The address was an iPhone link-local already rotated away (`ip neigh` INCOMPLETE,
        // ping6 "Address unreachable") while the phone was live on br0 at a different link-local and at
        // 192.168.5.57 — AND the port was stale too (cached 57236 vs live 57292), so even the correct
        // address with the cached port drew ECONNREFUSED. Every dial timed out, `GET /ctrl-int/1/connect`
        // never landed, and iOS logged `no pending autoconnections`. Restarting discovery does NOT help:
        // the platform re-answers the new resolve from the same cache. iOS rotates its IPv6 privacy
        // address, so this recurs by design.
        //
        // Therefore: query the link directly FIRST and dial what actually answers. NsdManager's pair is
        // kept as the last candidate — it is occasionally the only answer, because the wire query can
        // miss a reply (observed once here, succeeding 12 s later) — but it is never dialled first.
        // Re-query periodically rather than once: the peer can move mid-retry.
        if (discoveryPaused.get()) { log.i("connect-out suppressed — discovery is paused"); return }
        var candidates = dialCandidates(serviceName, host, peerPort)
        for (attempt in 1..10) {
            if (!running.get()) return
            if (discoveryPaused.get()) { log.i("connect-out abandoned — discovery was paused"); return }
            // Same reason as the guard in onServiceResolved: once the phone has dialed in, further
            // nudges only provoke a hijack of the session we just won. A straggler retry thread that
            // re-queries at attempt 3 or 6 can pick up the phone's NEW ctrl port and nudge mid-session.
            if (liveConnections.get() > 0) { log.i("connect-out: session is live — stopping retries"); return }
            for ((h, p) in candidates) {
                if (!running.get()) return
                if (connectOut(h, p)) {
                    log.i("connect-out accepted on attempt $attempt via ${h.hostAddress}:$p")
                    noteDialAccepted()
                    return
                }
            }
            // Refresh on a cadence — cheap relative to a 3 s connect timeout, and it is the only way to
            // notice the peer moving while we are still retrying.
            if (attempt == 3 || attempt == 6) {
                // MERGE, never replace. The wire query can miss a reply (observed: a miss, then the
                // same query succeeding 12 s later), and replacing on a miss threw away live
                // addresses found at attempt 1 in favour of the cached pair for the rest of the run.
                val refreshed = dialCandidates(serviceName, host, peerPort)
                candidates = (refreshed + candidates).distinct()
            }
            try { Thread.sleep(1000) } catch (_: InterruptedException) { return }
        }
        log.w("connect-out: 10 attempts with no usable response from the phone " +
              "(tried ${candidates.joinToString { "${it.first.hostAddress}:${it.second}" }})")
        // Do NOT hold the cooldown after a failure — the next resolve may carry a new (live) port.
        lastDial.clear()
    }

    /**
     * Where to dial the peer, **live addresses first**.
     *
     * Order is the whole point: whatever answered an mDNS query on the link just now, then — only as a
     * last resort — whatever `NsdManager` had cached. See [connectOutWithRetry] for why the cached pair
     * cannot be trusted for either the address or the port.
     */
    private fun dialCandidates(serviceName: String, nsdHost: InetAddress, nsdPort: Int)
            : List<Pair<InetAddress, Int>> {
        val out = LinkedHashSet<Pair<InetAddress, Int>>()
        val live = try { MdnsInspect.liveEndpoint(serviceName, "_carplay-ctrl._tcp") }
                   catch (t: Throwable) { log.w("live lookup threw: ${t.message}"); null }
        if (live != null && live.addresses.isNotEmpty()) {
            val port = if (live.port > 0) live.port else nsdPort
            live.addresses.forEach { out.add(withScope(it) to port) }
            if (live.port > 0 && live.port != nsdPort)
                log.w("live SRV port $port differs from NsdManager's $nsdPort — the cache is stale")
        } else {
            log.w("no live mDNS answer for '$serviceName' — falling back to the cached address")
        }
        // Prefer the LIVE port even on the fallback: the port is the more frequently stale of the
        // two cached fields (observed rotating 57236 -> 57292 -> 57347 -> 57383 within minutes), so
        // pairing the cached address with a known-good live port beats using both cached values.
        val cachedPort = live?.port?.takeIf { it > 0 } ?: nsdPort
        val cached = withScope(nsdHost) to cachedPort
        if (out.none { it == cached }) out.add(cached)
        return out.toList()
    }

    private fun connectOut(host: InetAddress, peerPort: Int): Boolean {
        try {
            Socket().use { s ->
                s.connect(InetSocketAddress(host, peerPort), 3000)   // carplayd `connect_out`: connect_timeout 3 s
                s.soTimeout = 2000                                    // carplayd `connect_out`: set_read_timeout 2 s
                // A bare IPv6 literal with a zone id and a port glued on is not a parseable Host
                // header. Brackets, and the zone belongs to the socket, not the header.
                val hostHdr = if (host is java.net.Inet6Address)
                    // hostAddress is a PLATFORM type: InetAddress.getHostAddress() is documented to
                    // return null for an unresolved address, and kotlinc only warns. A null here
                    // would NPE on the advertise path rather than degrade. Fall back to the literal
                    // form rather than inventing an address.
                    "[${host.hostAddress?.substringBefore('%') ?: host.hostName}]:$peerPort"
                else "${host.hostAddress}:$peerPort"
                // Header set and order mirror carplayd's connect-out (`ccpa/carplayd/src/discovery.rs`)
                // exactly. No CSeq: this is plain HTTP, not
                // RTSP, and no reference sends one.
                val req = buildString {
                    append("GET /ctrl-int/1/connect HTTP/1.1\r\n")
                    append("Host: $hostHdr\r\n")
                    append("User-Agent: AirPlay/$SRCVERS\r\n")
                    append("AirPlay-Receiver-Device-ID: $deviceIdDec\r\n")
                    append("Connection: keep-alive\r\n\r\n")
                }
                s.getOutputStream().write(req.toByteArray()); s.getOutputStream().flush()
                log.i(">> GET /ctrl-int/1/connect -> ${host.hostAddress}:$peerPort  (device-id=$deviceIdDec decimal)")
                val ins = BufferedInputStream(s.getInputStream())
                val status = readLine(ins)
                // Match carplayd's `connect_out` response handling (`ccpa/carplayd/src/discovery.rs`) exactly: an empty response, a read
                // timeout, or an unparseable status line is TENTATIVELY ACCEPTED — the phone may open
                // RTSP asynchronously without answering. Only an explicit non-2xx is a refusal.
                // Scoring silence as failure turned the most likely real outcome into nine more dials.
                if (status == null) {
                    log.i("<< connect-out: no response body — tentatively accepted (phone may dial async)")
                    return true
                }
                log.i("<< connect-out response: $status")
                while (true) { val h = readLine(ins) ?: break; if (h.isBlank()) break; log.i("     $h") }
                log.i("=> now waiting for the phone to open the control connection INBOUND on :$port")
                val code = status.split(" ").getOrNull(1)
                return when {
                    code == null -> true
                    code.startsWith("2") -> true
                    else -> { log.w("connect-out refused (status $code) — will retry"); false }
                }
            }
        } catch (t: java.net.SocketTimeoutException) {
            // A READ timeout is the documented tentatively-accepted case (carplayd `connect_out`: `WouldBlock`/`TimedOut` → true):
            // the phone frequently accepts the nudge and opens RTSP asynchronously without answering.
            // This used to fall through to the generic catch and be scored a failure — the exact
            // behaviour the comment above says turned the most likely real outcome into nine more
            // dials. A CONNECT timeout is different and is still a failure; distinguish by whether
            // the socket ever connected.
            if (t.message?.contains("failed to connect", true) == true) {
                log.w("connect-out to ${host.hostAddress}:$peerPort — connect timed out")
                return false
            }
            log.i("<< connect-out: read timed out after connecting — tentatively accepted")
            return true
        } catch (t: Throwable) {
            log.w("connect-out to ${host.hostAddress}:$peerPort failed: ${t.javaClass.simpleName}: ${t.message}")
            return false
        }
    }

    // ---- inbound control --------------------------------------------------------------------------

    /** [s] is bound by [start] before this is dispatched — see the note there. */
    private fun acceptLoop(s: ServerSocket) {
        try {
            while (running.get()) {
                val c = try { s.accept() } catch (t: Throwable) { if (running.get()) log.w("accept ended: ${t.message}"); break }
                log.i(">>> INBOUND CONTROL CONNECTION from ${c.inetAddress?.hostAddress}:${c.port}")
                dialsSinceInbound.set(0)
                // Claim the liveness slot HERE, before anything else — specifically before the
                // incumbent is closed below.
                //
                // The count used to be taken at the top of handleNative/handleStub, i.e. on the pool
                // thread AFTER startNativeFor()'s JNI init. Closing the incumbent unblocks its read()
                // essentially instantly, so on every hijack the old pump's decrement landed while the
                // new connection had not yet incremented: the counter dipped 1 -> 0 -> 1 and
                // fireSessionDown() reported "session ended" in the middle of a perfectly healthy
                // reconnect — and, depending on which side won, either double-launched the CarPlay
                // screen or left the launcher stuck showing WAITING over a live session. Hijack is the
                // phone's NORMAL reconnect path, not an edge case, so this fired routinely.
                //
                // Incrementing on the accept thread makes the count a property of "a socket has been
                // accepted and not yet finished", which is what liveness actually means, and pairs it
                // 1:1 with the single decrement in handle()'s finally.
                liveConnections.incrementAndGet()
                // Connection hijack (Apple's _HijackHTTPServerConnections): a new control connection
                // supersedes the incumbent. Close the previous socket so its pump unblocks, breaks,
                // and destroys its own native generation. Without this a reconnecting phone starves
                // behind the incumbent and its fresh session is torn down when the old one cleans up.
                val prev = currentSocket
                currentSocket = c
                if (prev != null) {
                    log.i("hijack: closing incumbent control connection")
                    try { prev.close() } catch (_: Throwable) {}
                }
                try {
                    pool.execute { handle(c) }
                } catch (t: Throwable) {
                    // The pool is shut down (stop() raced us). Release the slot we just took or the
                    // count never returns to zero and no session-end is ever reported again.
                    log.w("could not dispatch control connection: ${t.javaClass.simpleName}")
                    try { c.close() } catch (_: Throwable) {}
                    if (currentSocket === c) currentSocket = null
                    if (liveConnections.decrementAndGet() == 0) fireSessionDown()
                }
            }
        } catch (t: Throwable) { log.e("acceptLoop: ${t.javaClass.simpleName}: ${t.message}") }
    }

    private fun handle(c: Socket) {
        // The matching decrement for acceptLoop's increment lives here and NOWHERE else, so it pairs
        // 1:1 with the accept even when startNativeFor() throws before either pump is entered.
        try {
            // Prefer the ported receiver core. It owns the RTSP state machine, pairing (including the
            // Ed25519/X25519 that API 32 cannot do), the ChaCha20-Poly1305 channel, /auth-setup and
            // SETUP/RECORD. Kotlin only shuttles bytes.
            val h = startNativeFor(c)
            if (h == NativeCore.BUSY) {
                // The incumbent still owns the native core (it is inside ControlServer::feed, most
                // likely MFi). Close and say so: the stub path cannot pair, so running it here would
                // turn a recoverable redial into a guaranteed failed session. See [NativeCore.BUSY].
                log.w("native core BUSY — closing so the phone redials into a free generation")
                try { c.close() } catch (_: Throwable) {}
                return
            }
            if (h != 0L) { handleNative(c, h); return }
            log.w("native core unavailable — Kotlin stub path (pair-setup M1..M4 only)")
            handleStub(c)
        } finally {
            if (liveConnections.decrementAndGet() == 0) fireSessionDown()
        }
    }

    /**
     * Start the ported core for THIS connection and return its generation handle (0 on failure).
     * AvSession needs the peer address to reach back for the stream sockets, so the server is built
     * per control connection. Installing a new core supersedes the previous generation in the Rust
     * slot, so the incumbent's pump then feeds a stale handle and gets null (see [handleNative]).
     */
    private fun startNativeFor(c: Socket): Long {
        // These three bails all end in the same "native core unavailable" line at the call site, but
        // for very different reasons — so each says which one it was. An unlogged bail here cost a
        // truck session: a null relay stopped the native core loading, and because the lazy loader in
        // NativeCore was never touched, System.loadLibrary was never called and logcat held no
        // UnsatisfiedLinkError to point at. Absence of evidence looked like a healthy library.
        val relay = mfiRelay ?: run {
            log.e("no MFi relay — the OCBM probe must be created BEFORE CarPlayRx (MainActivity.autoStart)")
            return 0
        }
        val peers = peerFile ?: run { log.e("no peer store file — cannot persist pairings"); return 0 }
        if (!NativeCore.available) { log.e("libcarplayjni.so did not load — see the [jni] line above"); return 0 }
        val info = infoBlob() ?: run { log.e("no assets/info.bplist"); return 0 }
        // BRACKET IPv6. Rust parses this with SocketAddr::from_str, which requires "[v6]:port" — an
        // unbracketed "fe80::c0b:7143:d356:a90:59419" is ambiguous and fails. The failure is silent and
        // costs the MICROPHONE: peer_addr stays None, so session.rs logs "type-100 input requested but
        // no peer addr — uplink skipped" on every Siri and telephony stream. Video and downlink audio
        // are unaffected, which is why this looks like a mic bug rather than an addressing bug.
        // The phone is ALWAYS IPv6 link-local on this rig (fe80::…%br0), so this is the normal path,
        // not an edge case. connectOut() already brackets correctly (the `hostHdr` construction) — these must stay in step.
        val raw = c.inetAddress?.hostAddress?.substringBefore('%') ?: ""
        val addr = if (raw.contains(':')) "[$raw]:${c.port}" else "$raw:${c.port}"
        val h = NativeCore.start(pi, edSeed, info, peers, addr, relay)
        if (h == NativeCore.BUSY) return h   // the caller closes; do NOT report a session up
        if (h != 0L) {
            log.i("native receiver core started for $addr gen=$h (/info ${info.size}B)")
            // Stand the A/V consumers up NOW, not when the phone asks. Streams are SETUP within
            // ~300 ms of RECORD and the producer dials the seam per access unit, so anything not
            // already listening loses frames outright.
            onSessionUp?.let { cb -> fenced("onSessionUp") { cb() } }
        } else log.e("native core failed to start")
        return h
    }

    /** Raw byte pump into ControlServer::feed() — the sans-IO seam. [handle] is this connection's gen. */
    private fun handleNative(c: Socket, handle: Long) {
        // No liveConnections bookkeeping here — acceptLoop increments, handle()'s finally decrements.
        try {
            c.soTimeout = 30000
            c.tcpNoDelay = true
            val ins = c.getInputStream()
            val out = c.getOutputStream()
            val buf = ByteArray(16384)
            var wasEncrypted = false
            while (running.get()) {
                // 30 s SO_RCVTIMEO matches the reference (`set_read_timeout(30 s)` just before `arm_keepalive` in
                // `run_pairing_server`'s per-connection path, `ccpa/carplayd/src/main.rs`). Breaking on timeout
                // is reference-correct for the pre-A/V phase: the `WouldBlock` arm of `serve_connection`
                // (`crates/vendor/receiver/src/net.rs`) closes the connection when
                // `av_idle_ms()` is None (no A/V ever flowed) or A/V has been idle >= 30 s.
                //
                // NOT YET IMPLEMENTED (both are real gaps, see docs/11_HARDENING_PLAN.md status ledger rows N13–N16):
                //   • the A/V-flowing case — the reference CONTINUES on timeout while A/V is live,
                //     which needs `ControlServer::av_idle_ms()` (`crates/vendor/receiver/src/server.rs`) exported over JNI;
                //   • `arm_keepalive` 3s/3s/3 (`fn arm_keepalive`, `ccpa/carplayd/src/main.rs`, armed per control connection) for ~12 s dead-link detect, which
                //     java.net.Socket cannot express but android.system.Os.setsockoptInt can.
                // Until av_idle_ms is exported, an unconditional `continue` here would match no
                // reference state at all and would leak sessions on silent link loss.
                val n = try { ins.read(buf) } catch (t: Throwable) { -1 }
                if (n <= 0) break
                // null reply = the core signalled close (feed error, panic, or this generation was
                // superseded by a hijacking reconnect). Either way, end this connection.
                val reply = NativeCore.feed(handle, buf.copyOf(n))
                if (reply == null) { log.w("native feed signalled close (gen=$handle) — ending connection"); break }
                if (reply.isNotEmpty()) { out.write(reply); out.flush() }
                // The flip is the milestone worth calling out: pair-verify completed.
                if (!wasEncrypted && NativeCore.isEncrypted(handle)) {
                    wasEncrypted = true
                    log.w("*** MILESTONE: control channel is ENCRYPTED — pair-verify completed")
                    log.w("*** next on the wire: POST /auth-setup (MFi-SAP, chip calls #3 and #4)")
                }
            }
        } catch (t: Throwable) {
            log.w("native control connection: ${t.javaClass.simpleName}: ${t.message}")
        } finally {
            try { c.close() } catch (_: Throwable) {}
            // Destroy only THIS generation. If a hijacking reconnect already installed a newer core,
            // the Rust slot's gen no longer matches and this is a safe no-op — it will not tear down
            // the fresh session.
            NativeCore.destroy(handle)
            if (currentSocket === c) currentSocket = null
        }
    }

    private fun handleStub(c: Socket) {
        // Mirror handleNative's session bookkeeping. Without it liveConnections stays 0 on this path,
        // so rediscoverLoop believes nothing is connected, tears discovery down every 12 s, re-dials
        // connect-out, and the resulting inbound connection hijacks the one already in progress. A
        // multi-message handshake can never finish — the churn is self-inflicted, not the phone
        // retrying. The count itself is taken by acceptLoop and released by handle()'s finally.
        try {
            c.soTimeout = 30000
            val ins = BufferedInputStream(c.getInputStream())
            val out = c.getOutputStream()
            while (running.get()) {
                val req = readRequest(ins) ?: break
                log.i("--- ${req.method} ${req.path}")
                req.headers.forEach { (k, v) -> if (k.lowercase() in INTERESTING) log.i("      $k: $v") }
                if (req.body.isNotEmpty()) {
                    log.i("      body(${req.body.size}B): ${req.body.take(48).joinToString("") { "%02x".format(it) }}${if (req.body.size > 48) "…" else ""}")
                }
                milestone(req)
                respond(req, out)
            }
        } catch (t: Throwable) {
            log.i("control connection closed: ${t.javaClass.simpleName}: ${t.message}")
        } finally {
            try { c.close() } catch (_: Throwable) {}
            // Identity guard, not a null check: during a hijack acceptLoop has ALREADY installed the
            // new connection's socket here, so an unconditional clear would blank the live one — and
            // the next hijack would then see prev == null and leave two pumps fighting over one phone.
            if (currentSocket === c) currentSocket = null
        }
    }

    // ---- session-end + stall reporting ------------------------------------------------------------

    /** The last control connection went away. Clean and abrupt are indistinguishable here. */
    private fun fireSessionDown() {
        if (stopped) { log.i("suppressing session-down from a stopped receiver"); return }
        log.w("*** SESSION DOWN — no live control connection")
        onSessionDown?.let { cb -> fenced("onSessionDown") { cb() } }
    }

    /**
     * A dial the phone accepted. If a session is already up this is a straggler retry and means
     * nothing; otherwise it is evidence that the phone can reach us, is answering us, and is still
     * declining to connect.
     */
    private fun noteDialAccepted() {
        onDialAccepted?.let { cb -> fenced("onDialAccepted") { cb() } }
        if (liveConnections.get() > 0) { dialsSinceInbound.set(0); return }
        val n = dialsSinceInbound.incrementAndGet()
        if (n != STALL_DIALS) return   // report the crossing once, not every dial after it
        log.e("!! STALLED: $n consecutive connect-outs accepted with no inbound control connection on :$port")
        log.e("!! The phone can reach this receiver and is answering the nudge — it is choosing not to")
        log.e("!! dial back. Observed cause: iOS reads our _airplay TXT and never resolves SRV, so it")
        log.e("!! holds no address for us. Reconnecting Bluetooth to the car FROM THE IPHONE clears it;")
        log.e("!! restarting this app or power-cycling the adapter does not. Device-observed 2026-08-27.")
        onStalled?.let { cb -> fenced("onStalled") { cb(n) } }
    }

    /** One readiness probe: what was checked, and what was found. */
    data class Check(val name: String, val ok: Boolean, val detail: String)

    /**
     * Prove this receiver can actually accept a CarPlay session, BEFORE a phone turns up.
     *
     * Every one of these used to be discovered only at the moment a phone arrived — and three of them
     * only inside `startNativeFor`, per connection, far too late to do anything about. The cost was
     * demonstrated on 2026-08-27: a cold start via USB attach brought the OCBM link up perfectly and
     * never started the receiver at all. The box then ran its whole ladder — HELLO, MFi, SUBSCRIBE, BT
     * pair, Wi-Fi handoff — into an app with no listener and no advert, and reported no error anywhere.
     * The log looked like a textbook bring-up. `listening` below is the check that catches exactly that.
     *
     * Deliberately does NOT dial :7011 on loopback. That would land in [acceptLoop], take a liveness
     * slot, spin up a native core and then fire a spurious session-down when it closed — a readiness
     * check must not perturb the thing it is measuring. Holding the bound, open ServerSocket is the
     * capability that matters and it is directly observable.
     */
    fun selfTest(): List<Check> {
        val out = ArrayList<Check>(6)

        val srv = server
        out += Check(
            "listening",
            running.get() && srv != null && srv.isBound && !srv.isClosed,
            if (srv == null) "no ServerSocket — the receiver was never started"
            else "bound=${srv.isBound} closed=${srv.isClosed} on :$port"
        )

        out += Check("native core", NativeCore.available,
            if (NativeCore.available) "libcarplayjni.so loaded" else "libcarplayjni.so did NOT load — pairing is impossible")

        val info = infoBlob()
        out += Check("/info", info != null,
            info?.let { "${it.size} B from assets/info.bplist" } ?: "assets/info.bplist missing — cannot answer GET /info")

        out += Check("MFi relay", mfiRelay != null,
            if (mfiRelay != null) "OCBM CH_MFI relay attached"
            else "no relay — /auth-setup will fail on every connection")

        val br = runCatching {
            java.net.NetworkInterface.getNetworkInterfaces()?.toList()?.firstOrNull {
                it.isUp && it.name.startsWith("br") && it.inetAddresses.toList().any { a -> a is java.net.Inet4Address }
            }
        }.getOrNull()
        val brAddr = br?.inetAddresses?.toList()?.filterIsInstance<java.net.Inet4Address>()?.firstOrNull()
        out += Check("hotspot bridge", brAddr != null,
            brAddr?.let { "${br?.name} ${it.hostAddress}" } ?: "no br* interface with an IPv4 — nothing to advertise on")

        // Advisory, not gating. This asks the link for our OWN record, and a multicast query that has
        // to come back through our own responder can miss for reasons that are not faults. It is
        // reported because "can anything actually resolve me end to end" is the single most useful
        // thing to know here — but it should only become a hard gate once it has been shown stable on
        // the truck. iOS was observed reading our TXT and never resolving SRV on 2026-08-27, which is
        // precisely the condition this check exists to make visible.
        val live = runCatching { MdnsInspect.liveEndpoint(ACCESSORY_NAME, "_airplay._tcp") }.getOrNull()
        out += Check("advert resolvable (advisory)", true,
            live?.let { "$ACCESSORY_NAME resolves on the wire" } ?: "no wire answer for $ACCESSORY_NAME (advisory — not treated as a failure)")

        return out
    }

    /** Hold discovery and dialling still without stopping the receiver. See [discoveryPaused]. */
    fun pauseDiscovery() { if (discoveryPaused.compareAndSet(false, true)) log.i("discovery PAUSED") }

    fun resumeDiscovery() { if (discoveryPaused.compareAndSet(true, false)) log.i("discovery RESUMED") }

    /**
     * Re-assert our mDNS records without tearing the receiver down — cheapest recovery rung.
     *
     * Returns **true only if an announce was actually dispatched**. It used to return Unit and the
     * caller assumed success, so on a head unit where the responder never came up rung 0 was a
     * silent no-op that still burned its 30 s cooldown and its 20 s retry slot — the ladder's
     * cheapest rung was fictitious in exactly the situation the ladder exists for. A false lets the
     * supervisor advance immediately instead.
     *
     * The announce itself runs off the caller's thread: it is ~3 s of RFC 6762 §8.3 spacing and the
     * caller is the supervisor thread that owns every timer.
     */
    fun reannounce(): Boolean {
        val r = selfMdns ?: run { log.w("reannounce: self-hosted responder is not up"); return false }
        return try { pool.execute { r.reannounce() }; true }
        catch (t: Throwable) { log.w("reannounce: not dispatched — ${t.javaClass.simpleName}"); false }
    }

    /** Call out the requests that mark real progress, so they are greppable in a long capture. */
    private fun milestone(r: Req) {
        val p = r.path
        when {
            p.startsWith("/info") ->
                log.i("*** MILESTONE: iOS is querying /info on the CarPlay path")
            p.startsWith("/pair-setup") ->
                log.i("*** /pair-setup — SRP-6a is implemented; see the M1..M4 lines that follow")
            p.startsWith("/pair-verify") ->
                log.w("*** WALL: /pair-verify reached — needs Curve25519/Ed25519. Not implemented.")
            p.startsWith("/auth-setup") ->
                log.w("*** /auth-setup reached — this is where the proven CH_MFI relay plugs in.")
            r.method == "SETUP" ->
                log.w("*** MILESTONE: SETUP — this is A/V stream negotiation. Body above is the stream dict.")
            r.method == "RECORD" ->
                log.w("*** MILESTONE: RECORD — the session is starting.")
            r.method == "ANNOUNCE" || r.method == "FLUSH" || r.method == "TEARDOWN" ->
                log.i("*** RTSP ${r.method}")
        }
    }

    private fun respond(r: Req, out: OutputStream) {
        val cseq = r.headers.entries.firstOrNull { it.key.equals("CSeq", true) }?.value
        when {
            r.method == "OPTIONS" -> write(out, 200, cseq, mapOf(
                "Public" to "ANNOUNCE, SETUP, RECORD, PAUSE, FLUSH, TEARDOWN, OPTIONS, GET_PARAMETER, SET_PARAMETER, POST, GET"),
                ByteArray(0), r.version)
            r.path.startsWith("/pair-setup") -> {
                val body = handlePairSetup(r.body)
                write(out, 200, cseq, mapOf("Content-Type" to "application/octet-stream"), body, r.version)
            }
            // /info is served for GET *and* POST, matched by suffix — iOS sends absolute request
            // URIs (`rtsp://host/info`) and may POST with a bplist qualifier. rtsp/src/route.rs:54-81.
            r.path.contains("/info") -> {
                val blob = infoBlob()
                if (blob == null) {
                    log.e("/info requested but assets/info.bplist is missing — regenerate it")
                    write(out, 500, cseq, emptyMap(), ByteArray(0), r.version)
                } else {
                    log.i("*** MILESTONE: serving /info (${blob.size}B binary plist)")
                    write(out, 200, cseq, mapOf("Content-Type" to BPLIST), blob, r.version)
                }
            }
            // Anything we do not implement must NOT be answered with a bodyless 200. iOS's first
            // request on the control connection is pair-verify M1, and a false success looks like a
            // valid empty M2 — strictly worse than an error. 05_SESSION_FLOW §8 "Ordering rules that
            // kill sessions", rule 9, makes the same point for SETUP stream types: never answer with
            // silence; omit and log loudly. Its HTTP-level counterpart is a 501, not a fake 200.
            else -> {
                log.w("UNIMPLEMENTED ${r.method} ${r.path} -> 501 (deliberately not a fake 200)")
                write(out, 501, cseq, emptyMap(), ByteArray(0), r.version)
            }
        }
    }

    /**
     * `features` here MUST equal the TXT value or pair-verify fails. 0x44440B80,0x61 as one integer
     * is (0x61 << 32) | 0x44440B80.
     */
    /**
     * pair-setup, states M1..M4 (SRP-6a). Ported from `pairing/src/setup.rs`.
     *
     * M1{State=1,Method}            -> M2{State=2,Salt,PublicKey=B}
     * M3{State=3,PublicKey=A,Proof} -> M4{State=4,Proof=M2}
     *
     * M5/M6 exchange Ed25519 long-term keys inside ChaCha20 and are NOT implemented here: Ed25519 is
     * API 33+, so that step needs either a bundled provider or the JNI'd Rust core. Reaching a
     * correct M4 is still worth it on its own — it proves SRP wire-compatibility with a real iPhone,
     * which `setup.rs` itself flags as "the single thing not provable offline".
     */
    private fun handlePairSetup(body: ByteArray): ByteArray {
        val tlv = Tlv8.decode(body)
        return when (val state = Tlv8.state(tlv)) {
            1 -> {
                val salt = ByteArray(16).also { SecureRandom().nextBytes(it) }
                val s = SrpServer(SrpServer.USERNAME, SrpServer.SETUP_CODE, salt)
                srp = s
                log.i("pair-setup M1 -> M2 (salt 16B, B ${s.bPub().size}B, setup code 3939)")
                Tlv8.encode(listOf(
                    Tlv8.STATE to byteArrayOf(2),
                    Tlv8.SALT to s.salt,
                    Tlv8.PUBLIC_KEY to s.bPub()
                ))
            }
            3 -> {
                val s = srp
                val a = tlv[Tlv8.PUBLIC_KEY]
                val proof = tlv[Tlv8.PROOF]
                if (s == null || a == null || proof == null) {
                    log.w("pair-setup M3 malformed (srp=${s != null} A=${a?.size} proof=${proof?.size})")
                    return Tlv8.error(4, Tlv8.ERR_UNKNOWN)
                }
                log.i("pair-setup M3: A=${a.size}B proof=${proof.size}B — verifying SRP")
                val m2 = s.verify(a, proof)
                if (m2 == null) {
                    // Either the setup code differs or our SRP is wire-incompatible. Both look the same.
                    log.e("*** pair-setup M3 PROOF MISMATCH — setup code wrong, or SRP not wire-compatible")
                    return Tlv8.error(4, Tlv8.ERR_AUTHENTICATION)
                }
                log.w("*** MILESTONE: SRP-6a PROOF VERIFIED against a real iPhone — M4 going out")
                log.w("*** This closes the gap pairing/src/setup.rs calls unprovable offline.")
                Tlv8.encode(listOf(Tlv8.STATE to byteArrayOf(4), Tlv8.PROOF to m2))
            }
            5 -> {
                log.w("*** pair-setup M5 reached — the Ed25519 LTPK exchange. Needs API 33+ curve")
                log.w("*** primitives (or the JNI'd Rust core). SRP is DONE; this is the next gate.")
                Tlv8.error(6, Tlv8.ERR_UNKNOWN)
            }
            else -> { log.w("pair-setup: unexpected state $state"); Tlv8.error(2, Tlv8.ERR_UNKNOWN) }
        }
    }

    /**
     * The `/info` capability description, GENERATED by the reference builder
     * (`receiver::info::build_info`) rather than hand-written, and shipped as an asset.
     *
     * iOS validates this capability set at RECORD and tears the session down if it is incomplete —
     * missing `displays` / `audioFormats` / `hidDevices` / `audioLatencies` / `modes`, or a display
     * whose `uuid` does not match the HID `displayUUID`. It is a binary plist served as
     * `application/x-apple-binary-plist`; the XML stub this replaces would never have survived.
     *
     * Built for this head unit: HEVC, 2400x960 @ 60 fps, fullscreen (no viewAreas, no cornerMasks,
     * no alt/cluster display), touch panel + media buttons only (no D-pad, no knob, no telephony
     * buttons), and the wireless CarPlay audio set including the AAC-ELD mic uplink.
     * Regenerate with the `infogen` tool if the identity or geometry changes — `deviceid`/`pi`/
     * `features` MUST stay equal to the Bonjour TXT or pair-verify fails.
     */
    private fun infoBlob(): ByteArray? = infoCache ?: try {
        ctx.assets.open("info.bplist").use { it.readBytes() }.also { infoCache = it }
    } catch (t: Throwable) { log.e("assets/info.bplist: ${t.message}"); null }

    private var infoCache: ByteArray? = null

    private fun write(out: OutputStream, code: Int, cseq: String?, headers: Map<String, String>,
                      body: ByteArray, version: String = "HTTP/1.1") {
        // Echo the REQUEST's protocol version. iOS sends RTSP verbs on this same socket, and
        // answering an RTSP/1.0 request with an HTTP/1.1 status line ends the session.
        val reason = if (code == 200) "OK" else "Not Implemented"
        val sb = StringBuilder("$version $code $reason\r\n")
        sb.append("Server: AirTunes/$SRCVERS\r\n")
        if (cseq != null) sb.append("CSeq: $cseq\r\n")
        headers.forEach { (k, v) -> sb.append("$k: $v\r\n") }
        sb.append("Content-Length: ${body.size}\r\n\r\n")
        out.write(sb.toString().toByteArray())
        if (body.isNotEmpty()) out.write(body)
        out.flush()
    }

    // ---- minimal HTTP/RTSP parsing ------------------------------------------------------------------

    private class Req(val method: String, val path: String, val version: String,
                      val headers: Map<String, String>, val body: ByteArray)

    private fun readRequest(ins: BufferedInputStream): Req? {
        var line = readLine(ins) ?: return null
        while (line.isBlank()) line = readLine(ins) ?: return null
        val parts = line.split(" ")
        val headers = LinkedHashMap<String, String>()
        while (true) {
            val h = readLine(ins) ?: break
            if (h.isBlank()) break
            val i = h.indexOf(':')
            if (i > 0) headers[h.substring(0, i).trim()] = h.substring(i + 1).trim()
        }
        val len = headers.entries.firstOrNull { it.key.equals("Content-Length", true) }?.value?.toIntOrNull() ?: 0
        val body = ByteArray(len)
        var read = 0
        while (read < len) { val n = ins.read(body, read, len - read); if (n < 0) break; read += n }
        return Req(parts.getOrElse(0) { "?" }, parts.getOrElse(1) { "?" },
            parts.getOrElse(2) { "HTTP/1.1" }, headers, body)
    }

    private fun readLine(ins: BufferedInputStream): String? {
        val sb = StringBuilder()
        var prev = -1
        while (true) {
            val b = ins.read()
            if (b < 0) return if (sb.isEmpty()) null else sb.toString()
            if (b == '\n'.code) { if (prev == '\r'.code && sb.isNotEmpty()) sb.setLength(sb.length - 1); return sb.toString() }
            sb.append(b.toChar()); prev = b
        }
    }
}
