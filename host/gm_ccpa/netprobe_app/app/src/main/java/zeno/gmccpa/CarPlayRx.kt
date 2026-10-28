package zeno.gmccpa

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import java.io.BufferedInputStream
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.Executors
import zeno.gmccpa.pair.MfiRelay
import zeno.gmccpa.pair.NativeCore
import zeno.gmccpa.logging.SessionTrace
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Why a control connection ended, as far as this side can tell (2026-10-06). Decided by the native
 * pump from existing state and never by parsing a message: LINK_LOST is the 30 s idle backstop, or a
 * read exception we did not cause with a dead-link errno in its cause chain or after >= 10 s of
 * silence. CLOSED is everything else — EOF, a feed close, the hijack, stop(), BUSY, no native core,
 * a dispatch failure. The supervisor enters recovery faster after LINK_LOST.
 */
/**
 * Why a control connection ended. [LINK_LOST] = dead-link evidence (the grace ladder's fast entry);
 * [UNVERIFIED] = the pump's own unverified-connection deadline closed a generation that never
 * completed pair-verify ([UnverifiedConnRule]; SessionCore does not hand it to the supervisor);
 * [CLOSED] = everything else, including the peer closing early on its own.
 */
enum class EndReason { CLOSED, LINK_LOST, UNVERIFIED }

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
    /** Bridge to the device-proven OCBM CH_MFI relay; null means the native core cannot start and every control connection is closed. */
    private val mfiRelay: MfiRelay? = null,
    /**
     * Fired the instant the phone opens the control connection — BEFORE pair-verify, and long
     * before the phone SETUPs any stream.
     *
     * This is the trigger that lets the A/V consumers stand by. Without it the seams did not exist
     * until an operator ran `carplay_ui`, so from RECORD onward the phone streamed into a closed
     * port and `forward.rs` dropped every access unit until something answered — measured as
     * multiple seconds of black screen on a session that was otherwise healthy (2026-08-12).
     * Runs on the accept thread: hand off, do not block. The argument is the native generation that
     * came up (2026-10-06), so the link-interrupted overlay can drop a stale generation's state.
     */
    var onSessionUp: ((Long) -> Unit)? = null,
    /**
     * Fired when the last live control connection goes away — clean TEARDOWN and abrupt link loss
     * alike, because the two are indistinguishable here (the phone simply stops reading).
     *
     * Nothing observed session END before this existed. The A/V consumers, the foreground service and
     * `MainActivity.sessionUp` were all stood up on [onSessionUp] and never torn back down, so after
     * the phone left, the CarPlay screen stayed foreground over a dead session, swallowing touches
     * (`touch move … sent=false`) and re-launching itself from `onResume`. Device-observed 2026-08-27.
     * Runs on the dying pump thread: hand off, do not block. The argument is why the connection
     * whose end brought the count to zero ended ([EndReason], 2026-10-06): the recovery ladder enters
     * faster after a dead link than after a phone that hung up.
     */
    var onSessionDown: ((EndReason) -> Unit)? = null,
    /**
     * Fired once when [STALL_DIALS] consecutive connect-outs are accepted by the phone without it
     * ever dialling back. Argument is the dial count. See [noteDialAccepted] for what this means.
     */
    var onStalled: ((Int) -> Unit)? = null,
    /**
     * Fired once when [acceptLoop] gives up on the listener while the receiver is still running —
     * the accept thread has retired and closed the ServerSocket, so :7011 now refuses instead of
     * filling the kernel backlog. Argument is the reason. This is the ONLY way the supervisor learns
     * about it: [selfTest] does read false afterwards, but nothing polls it — reportReceiverHealth
     * runs once after start(), and again on every Restart Session (a new receiver) — so without this edge
     * the receiver would sit in RX_READY with a closed listener until someone happened to look.
     * Runs on the accept thread: hand off, do not block.
     */
    var onListenerLost: ((String) -> Unit)? = null,
    /** Fired on every connect-out the phone accepts, so the supervisor can start its dial-back clock. */
    var onDialAccepted: (() -> Unit)? = null,
    /**
     * The live control channel has carried nothing for [QUIET_MS] while A/V is not fresh either: the
     * link is probably interrupted (the overlay's cue). Argument = the generation. Fired at most once
     * per silent episode, on the pump thread; hand off, do not block. 2026-10-06.
     */
    var onLinkQuiet: ((Long) -> Unit)? = null,
    /** Bytes arrived again after [onLinkQuiet], or that pump ended (clear-on-exit). Argument = the generation. */
    var onLinkResumed: ((Long) -> Unit)? = null,
    /**
     * May the self-provoked connect-out go out right now? Evaluated at every dispatch, never
     * captured: the answer is "is there an OCBM link whose CH_MFI relay can answer /auth-setup",
     * and that link is rebuilt on every re-claim. A nudge sent before CT_HELLO_ACK made the phone
     * dial back into a receiver whose relay had no client — `relay sign requested with no OCBM
     * link`, first auth failed, device-observed 2026-10-05. A refused dispatch burns no cooldown;
     * [wakeDiscovery] (called on link-up) re-resolves so the held nudge goes out promptly. Only
     * the outbound nudge is gated — an unsolicited inbound control connection is never refused here.
     */
    private val nudgeAllowed: () -> Boolean = { true },
    /** Where controller-id -> Ed25519 LTPK pairings persist across runs. */
    private val peerFile: String? = null,
    /**
     * Stable 32-byte Ed25519 accessory seed. Changing it invalidates every existing pairing.
     * REQUIRED, no default (plan v5.8 V5 / Q001): the only source is [AccessoryKey.loadOrCreate],
     * per install, so no code path can fall back to a compiled-in constant.
     */
    private val edSeed: ByteArray,
    /**
     * Pair-verify completed on a control connection (the ENCRYPTED milestone): the phone holds a
     * pairing that is valid under THIS install's accessory key. Argument = the generation. Fired on
     * the pump thread, once per connection; hand off, do not block. V5 uses it to end the rekey
     * migration ([AccessoryKey.clearRekey]).
     */
    var onPairVerified: ((Long) -> Unit)? = null
) {
    init { require(edSeed.size == 32) { "edSeed must be 32 bytes" } }
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
    /** The browse listener. Started and stopped only under the slot's lock, gated on [running] (F039). */
    private val browse = BrowseSlot<NsdManager.DiscoveryListener> { running.get() }
    private val pool = Executors.newCachedThreadPool()
    private val lastDial = java.util.concurrent.ConcurrentHashMap<String, Long>()

    /** The incumbent control connection. A new inbound supersedes and closes it only while it is not
     *  yet ENCRYPTED; once it is, every newcomer is refused ([Admission], OD-7F).
     *  Installed by acceptLoop, cleared by compareAndSet on each connection's own exit (F040). */
    private val incumbent = IncumbentSlot()
    /** OD-7F admission, judged on the accept thread against [incumbent] (CarPlayRxSlots.kt). */
    private val acceptGate = AcceptGate(incumbent, onRefused = { line -> log.w(line) }, isLocal = { c ->
        Admission.isLocal(c.inetAddress, c.localAddress) { a -> java.net.NetworkInterface.getByInetAddress(a) != null }
    })
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

    /**
     * Wakes [rediscoverLoop] out of its sleep so it restarts discovery NOW rather than at the next
     * tick. A semaphore, not `Thread.interrupt`: every sleep site in the loop returns on
     * InterruptedException because that is how `stop()`'s `shutdownNow()` ends it, so an interrupt
     * would kill the loop instead of hurrying it. The loop thread stays the only owner of
     * [browse]'s listener (stop() aside); a waker never touches NsdManager itself. See [wakeDiscovery].
     */
    private val rediscoverWake = java.util.concurrent.Semaphore(0)

    /**
     * Rediscover backoff — the supervisor's lever, not the receiver's own idea.
     *
     * `backoffEligible` is set by the supervisor for exactly the phases where the 12 s churn has
     * nothing left to find (STALLED, BOX_UNHEALTHY) and cleared on every other phase. While it is
     * set, [REDISCOVER_EMPTY_CYCLES] consecutive EMPTY cycles — a restart that produced no
     * `onServiceResolved` before the next tick — lengthen the interval towards
     * [REDISCOVER_MAX_MS]. Any resolve, any resume, any BT progress the supervisor relays, and the
     * link-up wake all reset it to [REDISCOVER_MS]. The interval is read once per sleep, so a reset
     * takes effect on the next tick at the latest; the wake makes it immediate.
     */
    private val backoffEligible = AtomicBoolean(false)
    /** Guards the two below: the loop's read-modify-write races the resets from other threads otherwise. */
    private val backoffLock = Any()
    private var emptyCycles = 0                       // under backoffLock
    private var rediscoverIntervalMs = REDISCOVER_MS  // under backoffLock
    /** Set by [resolve] on every resolved record; the loop reads-and-clears it per cycle. */
    private val resolvedThisCycle = AtomicBoolean(false)

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

    /**
     * When the advert went live (elapsedRealtime ms at onServiceRegistered; 0 = not yet). The
     * connect-out must not run until iOS has had time to index it. Monotonic since v5.8 V7 (Q015):
     * the bench clock is stepped by the adb time-sync hook.
     */
    @Volatile private var advertisedAt = 0L

    /** `onRegistrationFailed` fired — [advertisedAt] will never be stamped, so stop waiting on it. */
    @Volatile private var advertFailed = false

    /** One connect-out at a time. See the guard in [connectOutWithRetry]. */
    private val dialInFlight = AtomicBoolean(false)

    /** One resolved phone endpoint to nudge. [key] is the `addr:port` the cooldown map uses. */
    private class Target(val serviceName: String, val host: InetAddress, val port: Int) {
        val key: String get() = "${host.hostAddress}:$port"
    }

    /**
     * The NEWEST endpoint a resolve wants nudged, handed to the in-flight connect-out instead of
     * being dropped (2026-10-06, P1 R3: a fresh resolve to the phone's NEW port was dropped as "a
     * connect-out is already in flight" while the worker spent 11.6 s on the stale cached port). Set
     * by the resolver BEFORE it tries [dialInFlight]; consumed by the worker loop and at the top of
     * each attempt in [dialOut].
     */
    private val pendingTarget = java.util.concurrent.atomic.AtomicReference<Target?>(null)

    /** The endpoint a dispatch chain just had accepted, and when. Owned by that chain only. */
    private class Accepted(val key: String, val at: Long)

    /**
     * The newest native generation started, so a superseded pump's `finally` does not mark the
     * Board's `native-core`/`ctrl-conn` entries down underneath the hijacking connection that
     * replaced it. Only the pump whose handle equals this may take those entries down.
     */
    @Volatile private var latestGen = 0L

    /**
     * `addr:port` of the last peer announced as DETECTED. NsdManager re-resolves on every 12 s
     * discovery restart and the "resolved" line already narrates each one; the DETECTED line and the
     * Board's `peer-iphone` entry are for the CHANGE — a new phone, or the same phone on a new port
     * (which iOS rotates every session). Cleared by `onServiceLost` so a phone that leaves and
     * returns is announced again.
     */
    @Volatile private var lastDetectedKey: String? = null

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
        // stock derivation collapses to ONE identity shared by every adapter (<DEGENERATE_DEVICE_ID>) —
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
        /**
         * The proven CarPlay source version. 980.71.1 is an AirPlay-2 / Apple-TV-era string and can
         * steer iOS down a different negotiation path on a service claiming the Car bit; every
         * working CarPlay reference uses the 320.x family.
         */
        const val SRCVERS = "320.17"
        /** Car(32) | CarPlayControl(37) | HKPairing(38) — the production CCPA value. */
        const val FEATURES = "0x44440B80,0x61"
        /** TXT `model`; must equal `/info` `model` (InfoTxtEqualityTest). */
        const val MODEL = "CarLink-mac-1.0"
        /** TXT `flags`; `/info` `statusFlags` is the same value as an integer. */
        const val FLAGS = "0x4"

        /**
         * The TXT record both adverts publish (NsdManager's `setAttribute` loop and the self-hosted
         * responder). Pure and companion-level so `InfoTxtEqualityTest` can hold it against
         * `assets/info.bplist` on the host: `deviceid`/`features`/`pi` (and model/srcvers/flags)
         * must equal what `/info` returns, or pair-verify fails with `-16720` on the unit (docs/11
         * item 5.12).
         */
        internal fun txtRecordFor(deviceId: String, pi: String): Map<String, String> = linkedMapOf(
            "deviceid" to deviceId,
            // The Car bit (high word bit 32) is what makes iOS open RTSP back to the advertised port.
            "features" to FEATURES,
            "flags" to FLAGS,
            "model" to MODEL,
            "protovers" to "1.0",
            "pi" to pi,
            "srcvers" to SRCVERS
            // No `pk`: the Ed25519 long-term public key is exchanged inside pairing, not advertised.
        )
        /**
         * Re-nudge cooldown. Was 20 s, when a wasted dial was expensive because the only address we
         * had came from NsdManager's cache and a miss cost the whole window. Dials are now aimed by
         * a live wire lookup, so a miss is informative and cheap — and a long cooldown was pure
         * added latency on the critical path (measured: 48.5 s advert -> connect-out, 2026-08-12).
         */
        private const val REDIAL_COOLDOWN_MS = 5_000L
        /** The rediscover cadence while anything is still expected to happen. See [rediscoverLoop]. */
        private const val REDISCOVER_MS = 12_000L
        /** Ceiling the backed-off cadence may reach. Short enough that a phone returning on its own is still found within one tick. */
        private const val REDISCOVER_MAX_MS = 30_000L
        /** Consecutive empty cycles (no resolve) before the cadence starts to lengthen — only while the supervisor says it may. */
        private const val REDISCOVER_EMPTY_CYCLES = 5
        /**
         * Accepted-but-unanswered dials before we call it stalled. Six is ~75 s at the rediscover
         * cadence — long enough that a phone merely being slow is not misreported, short enough that
         * the driver is told something before they give up.
         */
        private const val STALL_DIALS = 6
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

        // ---- SessionTrace budgets. Each is derived from a measurement, not chosen. ----------------
        /**
         * Inbound control connection accepted -> `NativeCore.isEncrypted` (pair-verify M4 sent).
         *
         * Measured 57 ms on the 2026-09-09 reference session (already-paired phone, pair-verify
         * only). A first-time pairing adds pair-setup M1..M6 in front: two SRP-3072 modexps and the
         * Ed25519 exchange, with NO human in the loop — the CarPlay setup code is fixed (3939) and
         * MFi stands in for user confirmation — so it is sub-second on this SoC, not tens of seconds.
         * 10 s is >100x the observed verify-only path and comfortably above any pair-setup, and it is
         * a third of the pump's 30 s `soTimeout`: a connection that is still open but unencrypted
         * after 10 s is a phone that is talking and not pairing, which is the fault worth a line.
         * Since 2026-10-08 (A-F1) it is also the DEADLINE: [handleNative] closes a still-plaintext
         * connection at this age, sending or silent, as [EndReason.UNVERIFIED] ([UnverifiedConnRule]).
         */
        private const val PAIR_VERIFY_MS = 10_000L
        /**
         * Encrypted -> `POST /auth-setup` (observed here as the FIRST MFi chip op the native core asks
         * the relay for). Measured 6 ms after the ENCRYPTED milestone on 2026-09-09. iOS issues it
         * unconditionally on the CarPlay path once the channel is up, so anything beyond a few
         * hundred ms is iOS having changed its mind about us; 10 s leaves no room for a false alarm.
         */
        private const val AUTH_SETUP_REQUEST_MS = 10_000L
        /**
         * First chip op requested -> both `createSignature` and `copyCertificate` returned, which is
         * the instant the native core can answer `/auth-setup` (reference: "auth-setup (MFi-SAP) OK"
         * lands in the same millisecond the certificate returns). Measured 1.63 s on 2026-09-09.
         * The bound is the relay's own: each op may legitimately take ~12 s on the box and times out
         * at 15 s (`mfiSign`), so two slow-but-successful ops are ~30 s. 35 s is that plus margin —
         * tighter would flag a session the relay itself still considers healthy.
         */
        private const val AUTH_SETUP_CHIP_MS = 35_000L
        /**
         * accept() failure budget for [acceptLoop]. A throw from accept() while the receiver is still
         * running and the socket still open is not a shutdown — it is the process failing to hand us
         * a socket, and the case that actually happens in a long-lived process is EMFILE ("Too many
         * open files") from an fd leak somewhere else, or an OOM allocating the Socket. That is
         * transient when a burst of closes lands a moment later and permanent when the leak is real;
         * 40 x 250 ms = 10 s of retrying separates the two without letting a hard failure spin the
         * accept thread hot. Same shape as UsbBulkTransport's read loop: count consecutive failures
         * with a pause between them, and only after a sustained run declare the component dead.
         */
        private const val ACCEPT_RETRY_MS = 250L
        private const val ACCEPT_FAILURES_FATAL = 40

        /** [dialOut]: at most this many in-flight retargets (each resets the attempt budget) per run. */
        private const val MAX_RETARGETS = 3

        // ---- control-channel liveness (52A, 2026-10-06) -------------------------------------------
        /**
         * The pump's read timeout. Was 30 s; now a 1 s poll, so the silence clocks below are checked
         * every second and `running` is observed within 1 s. A SocketTimeoutException leaves the
         * socket valid (java.net.Socket contract), and partial ciphertext is already buffered by
         * NativeCore.feed.
         */
        private const val POLL_MS = 1_000
        /**
         * Stage 1: the link-interrupted overlay. The live channel carries `POST /feedback` every
         * 2.0 s; the largest healthy gap measured is 2.178 s (logcat2, 2821 intervals, locked phone
         * and a 46 s call included), so 5 s is 2.3x that.
         */
        private const val QUIET_MS = 5_000L
        /**
         * Stage 1 arms at the first byte received this long after ENCRYPTED. Setup has legitimate
         * gaps (ENCRYPTED -> first /feedback 4.0-8.5 s, longest pre-feedback gap 6.2 s), so a silent
         * setup stays unarmed and never shows the overlay; the backstop still applies.
         */
        private const val ARM_AFTER_MS = 15_000L
        /**
         * The idle backstop (replaces the unconditional 30 s read-timeout break): after 30 s of
         * control silence, continue only while the receiver core reports A/V or keepAlive beacons
         * fresher than 30 s — the reference `serve_connection` rule (receiver net.rs, AV_IDLE_TEARDOWN_MS).
         */
        private const val IDLE_BACKSTOP_MS = 30_000L
        /**
         * A read exception after this much silence is LINK_LOST, whatever the exception says. Both
         * kernel kills (keepalive: idle 3 s + 3 x 3 s probes; USER_TIMEOUT: 12 s of unacked data, and
         * the reply follows the last receive) fire >= 12 s after the last received byte, and a live
         * channel is never quiet past 2.2 s — so a peer RST mid-exchange stays CLOSED.
         */
        private const val LOST_MIN_SILENCE_MS = 10_000L
        /**
         * TCP_KEEPIDLE / TCP_KEEPINTVL / TCP_KEEPCNT. android.system.OsConstants has none of the
         * three at compileSdk 32 (only SO_KEEPALIVE, IPPROTO_TCP, TCP_NODELAY, TCP_USER_TIMEOUT), so
         * these are the Linux values: NDK 30 sysroot linux/tcp.h:36-38.
         */
        private const val TCP_KEEPIDLE_LINUX = 4
        private const val TCP_KEEPINTVL_LINUX = 5
        private const val TCP_KEEPCNT_LINUX = 6
        /** Apple's receiver values (carplayd `arm_keepalive`): 3 s idle, 3 s interval, 3 probes. */
        private const val KEEPALIVE_S = 3
        /** Abort once sent data has been unacked this long; with keepalive on it also bounds the probes. */
        private const val USER_TIMEOUT_MS = 12_000

        // ---- SessionTrace.Board entry names, so a capture is greppable per component ---------------
        private const val B_LISTENER = "rx-listener"
        private const val B_ADVERT_NSD = "advert-nsd"
        private const val B_ADVERT_SELF = "advert-self"
        private const val B_BROWSE = "browse-ctrl"
        private const val B_PEER = "peer-iphone"
        private const val B_CORE = "native-core"
        private const val B_CTRL = "ctrl-conn"
        private const val X_ADVERT = "advert-registered"
    }

    fun start() {
        if (stopped) { log.e("start() on a stopped receiver — construct a new CarPlayRx instead"); return }
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
            log.e("listen bind FAILED on :$port — ${t.javaClass.simpleName}: ${t.message}")
            SessionTrace.Board.failed(B_LISTENER, "bind :$port ${t.javaClass.simpleName}")
            null
        }
        server = srv
        if (srv != null) {
            log.i("listening on :$port")
            SessionTrace.Board.up(B_LISTENER, ":$port")
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
        // Nothing this receiver was waiting for can arrive now, and its absence is not a fault.
        SessionTrace.cancel(X_ADVERT, "receiver stopped")
        try { server?.close() } catch (_: Throwable) {}
        SessionTrace.Board.down(B_LISTENER, "receiver stopped")
        // Close the live control socket so its pump thread unblocks, breaks, and destroys its own
        // native generation in the finally. Without this, stop() left the native core running and the
        // pump blocked in read() for up to 30 s — a zombie session that collides with the next instance.
        incumbent.clear()?.let { try { it.sock.close() } catch (_: Throwable) {} }
        regListener?.let { try { nsd?.unregisterService(it) } catch (_: Throwable) {} }
        regListener = null
        selfMdns?.let { runCatching { it.stop() }.onFailure { t -> log.w("self-mdns stop: ${t.message}") } }
        selfMdns = null
        // Under the browse lock, after running=false above: a rediscover restart that is mid-flight
        // either finished first (stopped here) or now sees !running and starts nothing (F039).
        browse.stop { try { nsd?.stopServiceDiscovery(it) } catch (_: Throwable) {} }
        SessionTrace.Board.down(B_ADVERT_NSD, "receiver stopped")
        SessionTrace.Board.down(B_ADVERT_SELF, "receiver stopped")
        SessionTrace.Board.down(B_BROWSE, "receiver stopped")
        // Only if a phone was ever announced — otherwise this would register the entry just to mark it down.
        if (lastDetectedKey != null) SessionTrace.Board.down(B_PEER, "receiver stopped — no longer browsing")
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
     * The custom responder now runs ALONGSIDE this, not instead of it: 2026-10-06 the phone was shown
     * activating its `gmccpa-rx` endpoint for every session that formed — see the second-advert block below.
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
                advertisedAt = android.os.SystemClock.elapsedRealtime()
                SessionTrace.met(X_ADVERT)
                SessionTrace.Board.up(B_ADVERT_NSD, "'${s.serviceName}' _airplay._tcp:$listenPort")
            }
            override fun onRegistrationFailed(s: NsdServiceInfo, c: Int) {
                // Latch it. advertisedAt is stamped only by onServiceRegistered, so without this the
                // connect-out waits on an event that is never coming.
                advertFailed = true
                log.e("advertise FAILED code=$c")
                // Resolved, not missing: the failure is the answer, and Board.failed is its E line.
                SessionTrace.cancel(X_ADVERT, "NsdManager reported onRegistrationFailed code=$c")
                SessionTrace.Board.failed(B_ADVERT_NSD, "onRegistrationFailed code=$c")
            }
            override fun onServiceUnregistered(s: NsdServiceInfo) {
                SessionTrace.Board.down(B_ADVERT_NSD, "unregistered")
            }
            override fun onUnregistrationFailed(s: NsdServiceInfo, c: Int) {
                log.w("advert unregister FAILED code=$c — NsdManager may keep '${s.serviceName}' published")
            }
        }
        regListener = l
        // Requested is not registered. The record is live only on onServiceRegistered (55 ms after
        // the request on 2026-09-09); ADVERT_WAIT_MS is already the app's own definition of "too
        // long" — a connect-out abandons itself after that — so a registration that has not arrived
        // by then is missing by the receiver's own rules, whether or not a dial happened to be waiting.
        // Both are set BEFORE the request: a fast onServiceRegistered used to land between the
        // request and the arm, so its met() found nothing and its Board `up` was overwritten by the
        // `down` below (false EXPECTED-MISSING advert-registered at 03:58:25, 2026-10-06).
        SessionTrace.Board.down(B_ADVERT_NSD, "registerService requested — awaiting onServiceRegistered")
        SessionTrace.expect(X_ADVERT, ADVERT_WAIT_MS,
            "registerService('$ACCESSORY_NAME' _airplay._tcp:$listenPort) was requested and no " +
            "onServiceRegistered/onRegistrationFailed callback has arrived — iOS cannot index an " +
            "advert that is not live, so no connect-out can succeed")
        nsd?.registerService(info, NsdManager.PROTOCOL_DNS_SD, l)

        // ---- second advert, on a hostname WE own -------------------------------------------------
        // iOS keys its Wi-Fi CarPlay endpoint index by HOST, one endpoint per host, and whichever
        // Bonjour record lands first owns it. Captured from the phone on 2026-08-12:
        //
        //   Bonjour TXT Add CarPlay._airplay._tcp.local.        <- GM's
        //   Bonjour TXT Add gm-ccpa._airplay._tcp.local.        <- ours, 27 MICROSECONDS later
        //   Created APEndpointCarPlay [0xE051] ... id '<GM_RECEIVER_ID>'
        //
        // One endpoint, stamped with GM's identity; ours was discarded. Every later autoconnect then
        // failed with "No matching endpoint found for deviceID <ACCESSORY_DEVICE_ID>" — iOS was looking
        // for us and we were not in the index. Both SRV records target the platform's `Android.local`,
        // so we lose that race whenever GM registers first (it starts at boot, we start ~51 s later).
        //
        // NsdManager cannot set the SRV target — AOSP drops setHost() on registration — so the only
        // way to stop sharing GM's host is to answer mDNS ourselves. MdnsResponder owns
        // `$instance.local` and publishes only br0 addresses — its IPv6 link-local (AAAA) and IPv4
        // (A) — which also sidesteps `Android.local` resolving to three addresses of which two are
        // unroutable from the phone's subnet.
        //
        // Run it ALONGSIDE NsdManager rather than instead of it. The two do different jobs: this one
        // is what makes iOS create the SECOND, separately-keyed endpoint carrying our deviceID, and
        // the NsdManager advert is the proven dial path (SRV `gm-ccpa` -> `Android.local` -> br0
        // IPv6) that every formed session actually connected through. A distinct instance name keeps
        // the two from colliding.
        //
        // ACCEPTANCE TEST, on the phone: a second `Created APEndpointCarPlay ... id
        // '<ACCESSORY_DEVICE_ID>'`. That, not the absence of errors, is what proves this worked.
        //
        // Always on (v5.9 D018 removed its on/off switch, a constant that was always `true`):
        // the phone needs it. 2026-10-06: with it off, every nudge ended in airplayd's
        // `handlePendingAutoconnect ... No matching endpoint found for deviceID <ACCESSORY_DEVICE_ID>`
        // and no dial. Every session that formed went `index = WiFi, for deviceID: <ACCESSORY_DEVICE_ID>`
        // -> `Activating endpoint 'gmccpa-rx'` -> SRV `gm-ccpa` -> `Android.local:7011` over br0's IPv6
        // link-local, within ~15 s of "self-hosted advert up".
        //
        // It was switched off earlier that day on the wrong diagnosis. After an in-process restart
        // the phone resolved `gmccpa-rx.local` itself, and MdnsResponder then published only br0's
        // IPv4 (A, with an NSEC denying AAAA): 9/9 `Connecting to 192.168.5.1:7011` ended in
        // kTimeoutErr because the radio drops IPv4 :7011. The defect was that IPv4-only record set,
        // not the advert; the responder now publishes AAAA (br0 link-local) + A. That is a fix in
        // code only — acceptance is pending on-device: the phone must resolve `gmccpa-rx.local` to
        // `fe80::…%en0` and connect `[fe80::…]:7011`, and never dial `192.168.5.1`.
        // Stop any previous responder before replacing it. Overwriting the field leaks the old one,
        // which keeps its 5353 socket and answers every query a second time — visible in logcat as
        // paired "query -> answering" lines from two threads.
        selfMdns?.let { runCatching { it.stop() } }
        selfMdns = null
        runCatching {
            val r = MdnsResponder(SELF_HOSTED_NAME, "_airplay._tcp", listenPort, txtRecord(), log)
            if (r.start()) {
                selfMdns = r
                // A-only is the record set the phone could not dial (see the rationale above); the
                // Board line says which one is up so that state is visible without logcat.
                val fam = if (r.hasAaaa) "AAAA+A" else "A only — no IPv6 link-local on br0"
                log.i("self-hosted advert up: $SELF_HOSTED_NAME.local -> br0:$listenPort ($fam)")
                SessionTrace.Board.up(B_ADVERT_SELF, "$SELF_HOSTED_NAME.local -> br0:$listenPort ($fam)")
            } else {
                // Failed, not degraded: 2026-10-06 iPhone logs show no <ACCESSORY_DEVICE_ID>
                // endpoint without gmccpa-rx — the NsdManager advert alone gets "No matching
                // endpoint found".
                log.e("self-hosted advert did not start — no '$SELF_HOSTED_NAME' endpoint expected; autoconnect will find no match")
                SessionTrace.Board.down(B_ADVERT_SELF, "did not start — no '$SELF_HOSTED_NAME' endpoint for iOS")
            }
        }.onFailure {
            log.w("self-hosted advert threw: ${it.javaClass.simpleName}: ${it.message}")
            SessionTrace.Board.down(B_ADVERT_SELF, "threw ${it.javaClass.simpleName}")
        }
    }

    /** The TXT set, byte-identical to carplayd's (`fn run` in `ccpa/carplayd/src/discovery.rs`, the `("deviceid", …)…("srcvers", …)` tuple list). */
    private fun txtRecord(): Map<String, String> = txtRecordFor(deviceId, pi)

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
            // The sleep is a timed wait on the wake semaphore, so [wakeDiscovery] can end it early.
            // InterruptedException is still the stop path (shutdownNow). Permits are drained after
            // a wake so a burst of link-up calls collapses into one restart.
            val waitMs = synchronized(backoffLock) { rediscoverIntervalMs }
            val woken = try { rediscoverWake.tryAcquire(waitMs, java.util.concurrent.TimeUnit.MILLISECONDS) }
                        catch (_: InterruptedException) { return }
            if (woken) rediscoverWake.drainPermits()
            if (!running.get()) return
            if (liveConnections.get() > 0) continue
            if (discoveryPaused.get()) continue   // a deliberate wait is in progress; do not preempt it
            // Backoff bookkeeping, only on the cadence — a wake is progress by definition and has
            // already reset it. An empty cycle is one whose restart produced no resolve at all.
            if (!woken) {
                val empty = !resolvedThisCycle.getAndSet(false)
                var backedOffTo = 0L
                synchronized(backoffLock) {
                    if (!empty) {
                        emptyCycles = 0
                        rediscoverIntervalMs = REDISCOVER_MS
                    } else if (backoffEligible.get()) {
                        emptyCycles++
                        if (emptyCycles >= REDISCOVER_EMPTY_CYCLES) {
                            val next = minOf(rediscoverIntervalMs * 2, REDISCOVER_MAX_MS)
                            if (next != rediscoverIntervalMs) { rediscoverIntervalMs = next; backedOffTo = next }
                        }
                    }
                }
                if (backedOffTo != 0L) log.i("rediscover: $REDISCOVER_EMPTY_CYCLES+ empty cycles with nothing left to find — backing the cadence off to ${backedOffTo / 1000}s")
            } else {
                resolvedThisCycle.set(false)
            }
            log.i(if (woken) "woken — restarting discovery now so a held nudge can go out"
                  else "no live control connection — restarting discovery to force a FRESH resolve")
            // Do NOT clear lastDial here. Doing so cancelled the cooldown entirely, so every 12s
            // restart spawned another retry loop on top of the ones still running — up to a dozen
            // concurrent dialers hammering one phone port. carplayd's discovery loop dedups (the
            // `dialed` set in `ccpa/carplayd/src/discovery.rs`) and only clears on ServiceRemoved.
            browse.stop { try { nsd?.stopServiceDiscovery(it) } catch (_: Throwable) {} }
            try { Thread.sleep(600) } catch (_: InterruptedException) { return }
            browseForPhone()
        }
    }

    private fun browseForPhone() {
        val l = object : NsdManager.DiscoveryListener {
            override fun onDiscoveryStarted(t: String) {
                log.i("browsing _carplay-ctrl._tcp ...")
                // Value is constant across the 12 s rediscover restarts, so the Board only logs the
                // first time — the restart itself is already narrated by rediscoverLoop.
                SessionTrace.Board.up(B_BROWSE, "_carplay-ctrl._tcp")
            }
            override fun onServiceFound(s: NsdServiceInfo) {
                log.i("found _carplay-ctrl peer '${s.serviceName}' — resolving")
                resolve(s)
            }
            override fun onServiceLost(s: NsdServiceInfo) {
                log.w("lost _carplay-ctrl peer '${s.serviceName}'")
                lastDial.clear()
                lastDetectedKey = null   // so its return is announced as a fresh DETECTED
                SessionTrace.Board.down(B_PEER, "'${s.serviceName}' left the link (mDNS goodbye or TTL expiry)")
            }
            override fun onDiscoveryStopped(t: String) {}
            override fun onStartDiscoveryFailed(t: String, c: Int) {
                log.e("browse FAILED code=$c")
                SessionTrace.Board.failed(B_BROWSE, "onStartDiscoveryFailed code=$c — the phone cannot be found, so it will never be nudged")
            }
            // W, not silent: a failed stop is the one line that would show a browse outliving its
            // receiver (F052, the evidence line for F039).
            override fun onStopDiscoveryFailed(t: String, c: Int) {
                log.w("stop browse FAILED code=$c — this _carplay-ctrl listener may still be browsing")
            }
        }
        // Gated on running under the lock stop() takes for its read-stop-null, so a browse can never
        // start on a receiver that stop() has already passed (F039). Hence no re-check after the
        // rediscover loop's sleep.
        if (!browse.start(l) { nsd?.discoverServices("_carplay-ctrl._tcp", NsdManager.PROTOCOL_DNS_SD, it) })
            log.i("receiver stopped — not starting a browse")
    }

    private fun resolve(s: NsdServiceInfo) {
        @Suppress("DEPRECATION")
        nsd?.resolveService(s, object : NsdManager.ResolveListener {
            override fun onResolveFailed(si: NsdServiceInfo, code: Int) = log.w("resolve FAILED '${si.serviceName}' code=$code")
            override fun onServiceResolved(si: NsdServiceInfo) {
                // Every exit from here is logged. A resolve that produces no dial used to leave no
                // line at all (22:22:08 on 2026-10-05: a resolve, then silence, cause unknown), so
                // each return now says which guard took it.
                resolvedThisCycle.set(true)
                val raw = si.host ?: run { log.w("resolved '${si.serviceName}' with NO host address — nothing to dial"); return }
                // NsdManager hands back a link-local IPv6 with NO scope id, and connecting to one
                // without a scope fails EINVAL — there is no way for the stack to know which
                // interface to use. Re-bind it to the interface the hotspot bridge lives on.
                val host = withScope(raw)
                val key = "${host.hostAddress}:${si.port}"
                log.i("resolved '${si.serviceName}' -> $key")
                // The one unambiguous "an iPhone is here" line. A phone on br0 is NOT enough on its
                // own (the hotspot is an ordinary saved network — see SessionSupervisor.onBtPhase);
                // a resolvable _carplay-ctrl record is the phone saying it is ready for CarPlay.
                if (key != lastDetectedKey) {
                    lastDetectedKey = key
                    val iface = host.hostAddress?.substringAfter('%', "")?.takeIf { it.isNotEmpty() } ?: "the hotspot link"
                    log.i("*** iPhone DETECTED on $iface: '${si.serviceName}' at $key (_carplay-ctrl._tcp resolved)")
                    SessionTrace.Board.up(B_PEER, "'${si.serviceName}' $key")
                }
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
                val now = android.os.SystemClock.elapsedRealtime()   // Q015: a stepped wall clock must not stretch or void the cooldown
                val last = lastDial[key] ?: 0L
                if (now - last < REDIAL_COOLDOWN_MS) {
                    log.i("   (dialed ${(now - last) / 1000}s ago — within the ${REDIAL_COOLDOWN_MS / 1000}s cooldown, skipping)")
                    return
                }
                // The OCBM-link gate, BEFORE the cooldown key is burned: a refused dispatch must
                // leave the key clean so the re-resolve the link-up wake triggers can dial at once.
                // Evaluated now, not at construction — the link is rebuilt on every re-claim.
                if (!nudgeAllowed()) {
                    log.i("   (no OCBM link yet — holding the connect-out until CT_HELLO_ACK; the link-up wake re-resolves)")
                    return
                }
                lastDial[key] = now
                if (!running.get()) { log.i("   (receiver stopped — not dispatching the connect-out)"); return }   // stop() has run; the pool is shut down
                connectOutWithRetry(Target(si.serviceName, host, si.port))
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
    private fun connectOutWithRetry(t: Target) {
        // SINGLE-FLIGHT. rediscoverLoop restarts discovery every 12 s and each restart re-resolves
        // under a fresh addr:port key, so every cycle used to dispatch its own dialer on top of the
        // ones still running — four concurrent `live lookup` threads were observed in one process,
        // all racing the same phone. One dial at a time — but a stacked cycle is now HANDED to the
        // worker in flight (2026-10-06), not dropped: the target FIRST, then the CAS, so the worker
        // either sees it or has not yet cleared the flag and the post-finally check re-dispatches.
        pendingTarget.set(t)
        if (!dialInFlight.compareAndSet(false, true)) {
            log.i("   connect-out in flight — handing it :${t.port}")
            return
        }
        log.i("   dispatching connect-out to '${t.serviceName}' at ${t.key}")
        dispatchDialChain(null)
    }

    /** Runs a dial chain on the pool; [dialInFlight] is already held. Releases it if the pool refuses. */
    private fun dispatchDialChain(accepted: Accepted?) {
        try {
            pool.execute { dialChain(accepted) }
        } catch (t: java.util.concurrent.RejectedExecutionException) {
            // stop() raced this resolve. Same guard acceptLoop and reannounce() carry: the resolver
            // runs on NsdManager's own HandlerThread, where an uncaught throw is fatal.
            dialInFlight.set(false)
            log.i("   (receiver stopped — not dispatching the connect-out)")
        }
    }

    /**
     * One dispatch chain: drain [pendingTarget] until empty or a dial is accepted, then release
     * [dialInFlight] and re-dispatch if a target landed after the last check (the window between that
     * check and the release). [accepted] is the chain's own record of the endpoint it just had
     * accepted: a target EQUAL to it inside [REDIAL_COOLDOWN_MS] is not re-nudged — a second nudge
     * while the phone is dialling back is the harm — and the record dies with the chain, so a later
     * resolve (~12 s) starts fresh and dials the same endpoint, which is what lets the stall counter
     * ([STALL_DIALS]) advance exactly as before.
     */
    private fun dialChain(accepted: Accepted?) {
        var last = accepted
        try {
            while (true) {
                val t = pendingTarget.getAndSet(null) ?: break
                val a = last
                if (a != null && a.key == t.key && android.os.SystemClock.elapsedRealtime() - a.at < REDIAL_COOLDOWN_MS) {
                    log.i("connect-out to :${t.port} just accepted — not re-nudging")
                    continue
                }
                val ok = dialOut(t)
                if (ok != null) {
                    // Accepted: drop anything a re-resolve set meanwhile and stop — nudging again
                    // while the phone dials back is the harm. A target set AFTER this clear comes
                    // back through the post-finally check below.
                    pendingTarget.set(null)
                    last = Accepted(ok, android.os.SystemClock.elapsedRealtime())
                    break
                }
            }
        } finally {
            dialInFlight.set(false)
        }
        if (pendingTarget.get() != null && running.get() && dialInFlight.compareAndSet(false, true)) dispatchDialChain(last)
    }

    /** Returns the accepted endpoint's `addr:port`, or null if no dial was accepted. */
    private fun dialOut(target: Target): String? {
        val serviceName = target.serviceName
        var host = target.host
        var peerPort = target.port
        log.i("connect-out worker started for '$serviceName' at ${host.hostAddress}:$peerPort")
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
        val holdUntil = android.os.SystemClock.elapsedRealtime() + ADVERT_WAIT_MS
        var held = false
        while (running.get() && advertisedAt == 0L && !advertFailed) {
            if (android.os.SystemClock.elapsedRealtime() >= holdUntil) {
                log.w("advert never REGISTERED within ${ADVERT_WAIT_MS / 1000}s — abandoning this connect-out")
                return null
            }
            if (!held) { log.i("advert not yet REGISTERED — holding the connect-out until it is"); held = true }
            try { Thread.sleep(250) } catch (_: InterruptedException) { stoppedMidDial("advert wait"); return null }
        }
        if (!running.get()) { stoppedMidDial("advert wait"); return null }
        if (advertisedAt == 0L) {
            // Registration failed outright. The self-hosted responder is a real advert in its own
            // right — it carries the same TXT, on a hostname we own — so if it is up there IS
            // something for iOS to have indexed and the dial is worth making. If it is not, nothing
            // is published and the nudge can only be resolved against an index we are absent from.
            if (selfMdns == null) {
                log.w("advert registration FAILED and no self-hosted advert — abandoning this connect-out")
                return null
            }
            log.w("advert registration FAILED — dialling against the self-hosted advert instead")
        }
        var settled = advertisedAt + ADVERT_SETTLE_MS
        while (running.get() && android.os.SystemClock.elapsedRealtime() < settled) {
            settled = advertisedAt + ADVERT_SETTLE_MS   // re-read: a re-register moves the mark
            val left = settled - android.os.SystemClock.elapsedRealtime()
            if (left > 0) {
                log.i("holding the connect-out ${left / 1000}s longer so iOS can index our advert first")
                try { Thread.sleep(minOf(left, 2000L)) } catch (_: InterruptedException) { stoppedMidDial("advert settle"); return null }
            }
        }
        if (!running.get()) { stoppedMidDial("advert settle"); return null }
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
        if (discoveryPaused.get()) { log.i("connect-out suppressed — discovery is paused"); return null }
        var candidates = dialCandidates(serviceName, host, peerPort)
        // A resettable budget (2026-10-06): a newer endpoint handed over mid-run gets a full 10
        // attempts. At most MAX_RETARGETS resets per run, so a phone whose port keeps rotating cannot
        // pin the worker; rediscovery re-resolves every ~12 s anyway.
        var attempt = 0
        var retargets = 0
        while (++attempt <= 10) {
            if (!running.get()) { stoppedMidDial("attempt $attempt"); return null }
            if (discoveryPaused.get()) { log.i("connect-out abandoned — discovery was paused"); return null }
            // Same reason as the guard in onServiceResolved: once the phone has dialed in, further
            // nudges only provoke a hijack of the session we just won. A straggler retry thread that
            // re-queries at attempt 3 or 6 can pick up the phone's NEW ctrl port and nudge mid-session.
            if (liveConnections.get() > 0) { log.i("connect-out: session is live — stopping retries"); return null }
            // CONSUMED, not peeked: a peek would leave it set and the chain would re-dial it after this run.
            pendingTarget.getAndSet(null)?.let { nt ->
                if (nt.key != "${host.hostAddress}:$peerPort" && retargets < MAX_RETARGETS) {
                    log.i("in-flight connect-out retargeted :$peerPort → :${nt.port}")
                    retargets++
                    host = nt.host; peerPort = nt.port
                    candidates = (listOf(nt.host to nt.port) + candidates).distinct()
                    attempt = 1
                } else if (nt.key != "${host.hostAddress}:$peerPort") {
                    // Budget of resets spent: give it back to the chain, which dials it after this run.
                    pendingTarget.compareAndSet(null, nt)
                }
                // An equal endpoint is dropped silently: it is the one being dialled.
            }
            for ((h, p) in candidates) {
                if (!running.get()) { stoppedMidDial("attempt $attempt, candidate ${h.hostAddress}:$p"); return null }
                if (connectOut(h, p)) {
                    log.i("connect-out accepted on attempt $attempt via ${h.hostAddress}:$p")
                    noteDialAccepted()
                    return "${h.hostAddress}:$p"
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
            try { Thread.sleep(1000) } catch (_: InterruptedException) { stoppedMidDial("retry sleep after attempt $attempt"); return null }
        }
        log.w("connect-out: 10 attempts with no usable response from the phone " +
              "(tried ${candidates.joinToString { "${it.first.hostAddress}:${it.second}" }})")
        // Do NOT hold the cooldown after a failure — the next resolve may carry a new (live) port.
        lastDial.clear()
        return null
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
        out.add(cached)
        return out.toList()
    }

    private fun connectOut(host: InetAddress, peerPort: Int): Boolean {
        var connected = false
        try {
            Socket().use { s ->
                s.connect(InetSocketAddress(host, peerPort), 3000)   // carplayd `connect_out`: connect_timeout 3 s
                connected = true
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
            if (!connected) {
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
        var acceptFailures = 0
        // Set only when the loop is leaving with the receiver still running — the listener retired by
        // a fault rather than by stop(). The finally turns it into the B_LISTENER FAILED line.
        var fault: String? = null
        try {
            while (running.get()) {
                val c = try {
                    s.accept()
                } catch (t: Throwable) {
                    // Ordinary teardown: stop() flips `running` BEFORE it closes the socket, so a throw
                    // that finds `running` false is the close we asked for. Exit silently.
                    if (!running.get()) break
                    // A throw with `running` still true is NOT a reason to leave. This used to `break`
                    // unconditionally, and the outer handler closed nothing — so one throw (in a
                    // process this long-lived, realistically EMFILE from an fd leak elsewhere, or an
                    // OOM allocating the Socket) retired the accept thread while the ServerSocket
                    // stayed bound and open. The kernel kept completing handshakes on :7011 into a
                    // backlog nobody drained: the phone's control connection "connected", then hung
                    // with no RTSP answer, and iOS had no failure to react to. Worse, selfTest()'s
                    // `listening` check is exactly `running && bound && !closed`, which stayed TRUE,
                    // so the supervisor kept reporting a healthy receiver. Retry with a pause instead,
                    // and if the failure is sustained give up THROUGH the finally, so the socket really
                    // closes and the health check goes red. Do not simplify this back to a `break`.
                    if (s.isClosed) {
                        // Only stop() and the finally below close `s`, so this should be unreachable —
                        // but accept() on a closed socket can never succeed, so do not spend the
                        // retry budget finding that out.
                        fault = "socket closed while running — ${t.javaClass.simpleName}: ${t.message}"
                        break
                    }
                    if (++acceptFailures >= ACCEPT_FAILURES_FATAL) {
                        fault = "$acceptFailures consecutive accept failures — last: ${t.javaClass.simpleName}: ${t.message}"
                        break
                    }
                    log.w("accept failed ($acceptFailures/$ACCEPT_FAILURES_FATAL) — ${t.javaClass.simpleName}: ${t.message}; retrying in $ACCEPT_RETRY_MS ms")
                    try { Thread.sleep(ACCEPT_RETRY_MS) } catch (_: InterruptedException) { return }
                    continue
                }
                acceptFailures = 0
                // Admission (B1-4 F038, owner decision OD-7F, 2026-10-07): no new connection while an
                // ENCRYPTED session is live, and never one from this host. Decided HERE, before the
                // liveness increment, so a refused dial takes no liveness slot, builds no native core,
                // resets no dial counter and fires no session-down. Only this thread installs into the
                // slot, so the incumbent the gate read is still the one `install` replaces below unless
                // its own pump released it meanwhile (then the newcomer simply meets an empty slot).
                // A refused newcomer is closed and logged inside the gate.
                val admitted = acceptGate.admit(c, android.os.SystemClock.elapsedRealtime()) ?: continue
                val verdict = admitted.verdict
                log.i(">>> INBOUND CONTROL CONNECTION from ${c.inetAddress?.hostAddress}:${c.port}")
                dialsSinceInbound.set(0)
                // The record every party names this connection by (F040). Built by the gate, before the
                // liveness increment.
                val rec = admitted.rec
                // Claim the liveness slot HERE, before anything else — specifically before the
                // incumbent is closed below.
                //
                // The count used to be taken at the top of handleNative, i.e. on the pool
                // thread AFTER startNativeFor()'s JNI init. Closing the incumbent unblocks its read()
                // essentially instantly, so on every hijack the old pump's decrement landed while the
                // new connection had not yet incremented: the counter dipped 1 -> 0 -> 1 and
                // fireSessionDown() reported "session ended" in the middle of a perfectly healthy
                // reconnect — and, depending on which side won, either double-launched the CarPlay
                // screen or left the launcher stuck showing WAITING over a live session. (That comment
                // once called the hijack the phone's normal reconnect path; no capture shows one, and
                // since OD-7F, 2026-10-07, only a not-yet-encrypted or closed incumbent can be replaced
                // at all. A newcomer against an ENCRYPTED incumbent is refused above.)
                //
                // Incrementing on the accept thread makes the count a property of "a socket has been
                // accepted and not yet finished", which is what liveness actually means, and pairs it
                // 1:1 with the single decrement in handle()'s finally.
                liveConnections.incrementAndGet()
                // Connection hijack (Apple's _HijackHTTPServerConnections), limited by OD-7F: the gate
                // above admitted this newcomer only if there is no incumbent, it is closed, or it has not
                // finished pair-verify. Such an incumbent is superseded: close its socket so its pump
                // unblocks, breaks, and destroys its own native generation. An ENCRYPTED incumbent is
                // never superseded; it ends only by its own pump (EOF, keepalive / USER_TIMEOUT, idle
                // backstop), and the phone's next connect then meets an empty slot.
                // One atomic swap: an exiting pump can only clear its OWN record (compareAndSet), so it
                // can no longer blank the slot this install just filled (F040).
                val prev = incumbent.install(rec)
                if (prev != null) {
                    log.i("hijack: closing incumbent control connection (encrypted=${prev.encryptedAt != 0L})")
                    // Residual race, not prevented: the incumbent's pump stamps encryptedAt just after it
                    // writes the pair-verify reply, so a newcomer decided in the microseconds between the
                    // admission read and this install can still supersede a just-encrypted session.
                    if (prev.encryptedAt != 0L && verdict == Admission.Verdict.ALLOW_NOT_ENCRYPTED)
                        log.w("hijack raced the incumbent's ENCRYPTED milestone (admitted as not yet encrypted)")
                    try { prev.sock.close() } catch (_: Throwable) {}
                }
                try {
                    pool.execute { handle(rec) }
                } catch (t: Throwable) {
                    // The pool is shut down (stop() raced us). Release the slot we just took or the
                    // count never returns to zero and no session-end is ever reported again.
                    log.w("could not dispatch control connection: ${t.javaClass.simpleName}")
                    try { c.close() } catch (_: Throwable) {}
                    incumbent.release(rec)
                    if (liveConnections.decrementAndGet() == 0) fireSessionDown(EndReason.CLOSED)
                }
            }
        } catch (t: Throwable) {
            // Not logged here: Board.failed below emits the E line, and two lines per fault would
            // break the count (see its KDoc).
            fault = "acceptLoop threw ${t.javaClass.simpleName}: ${t.message}"
        } finally {
            // However the loop ends, the listener must not outlive it. After stop() this is a no-op
            // (already closed). After a fault it is the line that makes selfTest()'s `listening` read
            // false — `srv.isClosed` — so the next readiness check says RX_UNHEALTHY instead of
            // vouching for a bound socket nobody is accepting on.
            try { s.close() } catch (_: Throwable) {}
            fault?.let {
                // Gate on `running`: if stop() raced the fault it has already marked the slot DOWN, and
                // a FAILED on top would be a spurious E line on a teardown the user asked for.
                if (running.get()) {
                    SessionTrace.Board.failed(B_LISTENER, "retired: $it")
                    // The Board line is for the capture; this is for the supervisor, which otherwise
                    // never re-reads selfTest() and would keep showing SEARCHING over a closed port.
                    onListenerLost?.let { cb -> fenced("onListenerLost") { cb(it) } }
                } else log.w("acceptLoop ended during stop: $it")
            }
        }
    }

    private fun handle(rec: Incumbent) {
        val c = rec.sock
        // The matching decrement for acceptLoop's increment lives here and NOWHERE else, so it pairs
        // 1:1 with the accept even when startNativeFor() throws before the native pump is entered.
        // Only the native pump can classify its end; every other path is CLOSED. No shared field:
        // the reason travels by return value, and the connection whose decrement reaches 0 wins.
        var reason = EndReason.CLOSED
        try {
            // The ported receiver core is the only one. It owns the RTSP state machine, pairing
            // (including the Ed25519/X25519 that API 32 cannot do), the ChaCha20-Poly1305 channel,
            // /auth-setup and SETUP/RECORD. Kotlin only shuttles bytes.
            val h = startNativeFor(c)
            if (h == NativeCore.BUSY) {
                // The incumbent still owns the native core (it is inside ControlServer::feed, most
                // likely MFi). Close and say so: there is no fallback, and a busy core frees up, so the
                // phone's redial is the recovery. See [NativeCore.BUSY].
                log.w("native core BUSY — closing so the phone redials into a free generation")
                try { c.close() } catch (_: Throwable) {}
                incumbent.release(rec)
                return
            }
            if (h != 0L) { reason = handleNative(rec, h); return }
            // No core at all (h == 0; startNativeFor logged which bail). Same shape as BUSY above:
            // there is no fallback, so close, release the incumbent slot and let finally decrement.
            log.e("native core unavailable — closing; there is no fallback (see the startNativeFor error above)")
            try { c.close() } catch (_: Throwable) {}
            incumbent.release(rec)
        } finally {
            if (liveConnections.decrementAndGet() == 0) fireSessionDown(reason)
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
        // The relay is the only place Kotlin can SEE /auth-setup: the request itself is inside the
        // encrypted channel, but the native core must come back out to the relay for the two chip
        // ops, and those calls are the step. See [TracedRelay].
        val traced = TracedRelay(relay)
        val h = NativeCore.start(pi, edSeed, info, peers, addr, traced)
        if (h == NativeCore.BUSY) return h   // the caller closes; do NOT report a session up
        if (h != 0L) {
            traced.gen = h
            latestGen = h
            log.i("native receiver core started for $addr gen=$h (/info ${info.size}B)")
            SessionTrace.Board.up(B_CORE, "gen=$h peer=$addr")
            // Stand the A/V consumers up NOW, not when the phone asks. Streams are SETUP within
            // ~300 ms of RECORD and the producer dials the seam per access unit, so anything not
            // already listening loses frames outright.
            onSessionUp?.let { cb -> fenced("onSessionUp") { cb(h) } }
        } else {
            log.e("native core failed to start")
            SessionTrace.Board.failed(B_CORE, "NativeCore.start returned 0 for $addr")
        }
        return h
    }

    /**
     * The MFi relay with the two `/auth-setup` chip ops made visible to [SessionTrace].
     *
     * `handleNative` is a byte pump and, past pair-verify, every byte is ChaCha20 — so Kotlin cannot
     * see `POST /auth-setup` on the wire. What it CAN see is the native core calling back into this
     * relay for the certificate and the signature, which it does only while answering that request
     * (chip calls #3 and #4). The first call is therefore "auth-setup arrived", and both having
     * returned is "auth-setup can be answered" — on 2026-09-09 `auth-setup (MFi-SAP) OK` landed in
     * the same millisecond the certificate came back. Order is not assumed: the reference shows
     * signature first, certificate second, and nothing here depends on that.
     *
     * The `Fast` variants are the iAP2 tunnel's, not `/auth-setup`'s, and pass straight through.
     * [gen] is stamped after `NativeCore.start` returns; no chip op can be requested before the core
     * has been fed its first bytes, so it is always set by the time it is read.
     */
    private inner class TracedRelay(private val inner: MfiRelay) : MfiRelay {
        @Volatile var gen = 0L
        private val seen = AtomicBoolean(false)
        @Volatile private var sigDone = false
        @Volatile private var certDone = false

        private fun xRequest() = "auth-setup gen=$gen"
        private fun xChipOps() = "auth-setup chip ops gen=$gen"

        private fun <T> traced(op: String, body: () -> T): T {
            if (seen.compareAndSet(false, true)) {
                SessionTrace.met(xRequest())
                log.i("/auth-setup in progress on gen=$gen — MFi $op requested via the relay")
                SessionTrace.expect(xChipOps(), AUTH_SETUP_CHIP_MS,
                    "POST /auth-setup arrived on gen=$gen (the native core asked the relay for the MFi " +
                    "$op); the box's chip has ~15 s per op and iOS is holding the request open")
            }
            val r = try { body() } catch (t: Throwable) {
                log.e("MFi $op via the relay threw on gen=$gen: ${t.javaClass.simpleName}: ${t.message} — /auth-setup will fail")
                SessionTrace.cancel(xChipOps(), "relay $op threw — reported above")
                throw t
            }
            if (op == "signature") sigDone = true else certDone = true
            if (sigDone && certDone) {
                SessionTrace.met(xChipOps())
                log.i("*** MILESTONE: /auth-setup MFi chip ops complete on gen=$gen — RECORD is next")
            }
            return r
        }

        override fun copyCertificate(): ByteArray = traced("certificate") { inner.copyCertificate() }
        override fun createSignature(digest: ByteArray): ByteArray = traced("signature") { inner.createSignature(digest) }
        override fun copyCertificateFast(): ByteArray = inner.copyCertificateFast()
        override fun createSignatureFast(digest: ByteArray): ByteArray = inner.createSignatureFast(digest)
    }

    /**
     * Raw byte pump into ControlServer::feed() — the sans-IO seam. [handle] is this connection's gen.
     * Returns why it ended (2026-10-06): [EndReason.LINK_LOST] on dead-link evidence, else CLOSED.
     *
     * Link loss (52A, 2026-10-06). Three parts, one per failure it covers:
     *  1. Kernel TCP keepalive 3/3/3 + TCP_USER_TIMEOUT 12 s ([armKeepalive]) is the dead-link
     *     detector: ~12 s, no gate. It detects that the TRANSPORT died, not that the app went quiet —
     *     the phone's kernel ACKs the probes whether or not iOS sends `/feedback` — so it cannot tear
     *     down a quiet-but-live phone. USER_TIMEOUT closes keepalive's one gap: Linux probes only
     *     while nothing sent is unacked, and this pump writes a reply to every request.
     *  2. Stage 1: [QUIET_MS] of control silence once armed ([ARM_AFTER_MS]) and with A/V not fresh
     *     -> one W line and [onLinkQuiet] (the overlay); bytes again -> [onLinkResumed].
     *  3. The [IDLE_BACKSTOP_MS] backstop replaces the old unconditional 30 s read-timeout break: it
     *     continues only while the receiver core reports A/V or keepAlive beacons fresher than 30 s
     *     ([NativeCore.avIdleMs]), which is what the reference does (receiver net.rs) and what keeps a
     *     sleeping phone that stops `/feedback` but keeps beaconing alive. Beacons are stamped only on
     *     RECEIVED data, so a dead link cannot hold it open; with keepalive unarmed it ends a dead link.
     */
    private fun handleNative(rec: Incumbent, handle: Long): EndReason {
        val c = rec.sock
        // No liveConnections bookkeeping here — acceptLoop increments, handle()'s finally decrements.
        val peer = "${c.inetAddress?.hostAddress}:${c.port}"
        val xVerify = "pair-verify gen=$handle"
        var wasEncrypted = false
        var reason = EndReason.CLOSED
        // From accept, not 0: a 0 would read as "silent forever" on the first poll tick and turn a
        // peer RST before the first byte into LINK_LOST.
        var lastRxAt = android.os.SystemClock.elapsedRealtime()
        val acceptedAt = lastRxAt
        var encryptedAt = 0L
        // A-F1: the unverified-connection deadline, checked on BOTH read outcomes below — a peer that
        // keeps sending plaintext never hits the poll tick, and a silent one never hits the data path.
        fun unverifiedOverdue(now: Long): Boolean {
            if (!UnverifiedConnRule.overdue(wasEncrypted, acceptedAt, now, PAIR_VERIFY_MS)) return false
            log.w("control connection gen=$handle from $peer still not encrypted ${(now - acceptedAt) / 1000}s after accept — closing it (unverified-connection deadline)")
            return true
        }
        var armed = false
        var quiet = false          // an announced silent episode is open
        var quietSkipNoted = false // silence seen but A/V fresh — said once per episode
        // Per-generation names, so a superseded pump's cancel in its finally cannot drop the
        // expectation the hijacking connection just armed under the same step name.
        SessionTrace.Board.up(B_CTRL, "$peer gen=$handle plaintext — pair-verify pending")
        SessionTrace.expect(xVerify, PAIR_VERIFY_MS,
            "inbound control connection from $peer was accepted and native core gen=$handle started; " +
            "iOS opens with pair-verify (or pair-setup then pair-verify) and the channel should be " +
            "encrypted within ~60 ms")
        try {
            c.soTimeout = POLL_MS
            c.tcpNoDelay = true
            armKeepalive(c, handle)
            val ins = c.getInputStream()
            val out = c.getOutputStream()
            val buf = ByteArray(16384)
            while (running.get()) {
                val n = try { ins.read(buf) } catch (t: java.net.SocketTimeoutException) {
                    // The 1 s poll tick — not an end. (If the platform ever surfaced a kernel
                    // ETIMEDOUT as this class, the next read would see EOF and end CLOSED; the
                    // proof's "read ended <= 13 s, LINK_LOST" requirement exists to catch that.)
                    val now = android.os.SystemClock.elapsedRealtime()
                    if (unverifiedOverdue(now)) { reason = UnverifiedConnRule.reason(); break }
                    val silent = now - lastRxAt
                    if (armed && !quiet && silent >= QUIET_MS) {
                        val av = NativeCore.avIdleMs(handle)
                        if (av in 0 until QUIET_MS) {
                            if (!quietSkipNoted) {
                                quietSkipNoted = true
                                log.i("control channel silent ${silent / 1000}s but A/V idle only $av ms — not an interruption (gen=$handle)")
                            }
                        } else {
                            quiet = true
                            log.w("control channel silent ${silent / 1000}s (feedback cadence 2 s), A/V idle ${avIdleText(av)} — link interrupted? (gen=$handle)")
                            // Only the newest generation owns the Board entries. See [latestGen].
                            if (latestGen == handle) SessionTrace.Board.note(B_CTRL, "$peer gen=$handle ENCRYPTED — control silent ${silent / 1000}s, link interrupted?")
                            onLinkQuiet?.let { cb -> fenced("onLinkQuiet") { cb(handle) } }
                        }
                    }
                    if (silent >= IDLE_BACKSTOP_MS) {
                        val av = NativeCore.avIdleMs(handle)
                        // -1 (no A/V ever), -2 (core busy/superseded: this pump is being replaced)
                        // and >= 30 s all end it — the old unconditional rule; it cannot leak a
                        // connection on a stuck -2.
                        if (av in 0 until IDLE_BACKSTOP_MS) continue
                        log.w("control channel silent ${silent / 1000}s, A/V idle ${avIdleText(av)} — idle backstop, ending gen=$handle")
                        reason = EndReason.LINK_LOST
                        break
                    }
                    continue
                } catch (t: Throwable) {
                    // Taken HERE, before the finally releases the slot: both of our own closes write
                    // their state first (stop(): running=false before the close; the hijack: the new
                    // record installed before prev.close()), so this needs no flag. Exact since F040:
                    // only this pump can clear its own record, so a live slot naming us means a real loss.
                    val closedByUs = incumbent.endedByUs(running.get(), rec)
                    val silent = android.os.SystemClock.elapsedRealtime() - lastRxAt
                    // The FULL cause chain: how Android surfaces a kernel keepalive/USER_TIMEOUT kill
                    // on a blocking read is not shown in this tree (U-R); this line is the record.
                    log.i("read ended (gen=$handle, silent ${silent}ms${if (closedByUs) ", closed by us" else ""}): ${causeChain(t)}")
                    if (!closedByUs && (linkErrno(t) || silent >= LOST_MIN_SILENCE_MS)) reason = EndReason.LINK_LOST
                    -1
                }
                if (n <= 0) break
                val now = android.os.SystemClock.elapsedRealtime()
                if (quiet) {
                    quiet = false
                    log.i("control channel resumed after ${(now - lastRxAt) / 1000}s (gen=$handle)")
                    if (latestGen == handle) SessionTrace.Board.up(B_CTRL, "$peer gen=$handle ENCRYPTED")
                    onLinkResumed?.let { cb -> fenced("onLinkResumed") { cb(handle) } }
                }
                quietSkipNoted = false
                lastRxAt = now
                rec.lastRxAt = now
                // Armed on DATA, not on a clock: the first byte >= ARM_AFTER_MS after ENCRYPTED.
                if (!armed && wasEncrypted && now - encryptedAt >= ARM_AFTER_MS) armed = true
                // null reply = the core signalled close (feed error, panic, or this generation was
                // superseded by a hijacking reconnect). Either way, end this connection.
                val reply = NativeCore.feed(handle, buf.copyOf(n))
                if (reply == null) { log.w("native feed signalled close (gen=$handle) — ending connection"); break }
                if (reply.isNotEmpty()) { out.write(reply); out.flush() }
                // The flip is the milestone worth calling out: pair-verify completed.
                if (!wasEncrypted && NativeCore.isEncrypted(handle)) {
                    wasEncrypted = true
                    encryptedAt = android.os.SystemClock.elapsedRealtime()
                    rec.encryptedAt = encryptedAt
                    // A success marker is narration, not a warning. Three of these were W and made
                    // `NETPROBE:W` useless as a problem filter (2026-09-09 capture).
                    log.i("*** MILESTONE: control channel is ENCRYPTED — pair-verify completed (gen=$handle)")
                    log.i("*** next on the wire: POST /auth-setup (MFi-SAP, chip calls #3 and #4)")
                    SessionTrace.met(xVerify)
                    SessionTrace.Board.up(B_CTRL, "$peer gen=$handle ENCRYPTED")
                    SessionTrace.expect("auth-setup gen=$handle", AUTH_SETUP_REQUEST_MS,
                        "control channel gen=$handle is encrypted (pair-verify completed) and iOS sends " +
                        "POST /auth-setup next — 6 ms later on 2026-09-09; observed as the first MFi chip " +
                        "op the native core requests from the relay")
                    onPairVerified?.let { cb -> fenced("onPairVerified") { cb(handle) } }
                }
                if (unverifiedOverdue(now)) { reason = UnverifiedConnRule.reason(); break }
            }
        } catch (t: Throwable) {
            log.w("native control connection: ${causeChain(t)}")
        } finally {
            try { c.close() } catch (_: Throwable) {}
            // Destroy only THIS generation. If a hijacking reconnect already installed a newer core,
            // the Rust slot's gen no longer matches and this is a safe no-op — it will not tear down
            // the fresh session.
            NativeCore.destroy(handle)
            incumbent.release(rec)
            // Clear-on-exit: a pump that announced a silent episode never leaves the overlay up.
            if (quiet) onLinkResumed?.let { cb -> fenced("onLinkResumed") { cb(handle) } }
            // Ending unencrypted is a fault in its own right — the phone connected and never got
            // through pair-verify — and the line that says so is here, not the watchdog's. Whatever
            // was still armed for this generation is then moot: dropped, not missing.
            if (!wasEncrypted) log.w("control connection gen=$handle from $peer ended BEFORE pair-verify completed")
            SessionTrace.cancel(xVerify, if (wasEncrypted) "connection ended" else "connection ended unencrypted — reported above")
            SessionTrace.cancel("auth-setup gen=$handle", "connection gen=$handle ended")
            SessionTrace.cancel("auth-setup chip ops gen=$handle", "connection gen=$handle ended")
            // Only the newest generation owns the Board entries. See [latestGen].
            if (latestGen == handle) {
                SessionTrace.Board.down(B_CTRL, "gen=$handle from $peer ended" + (if (wasEncrypted) "" else " unencrypted") +
                    when (reason) { EndReason.LINK_LOST -> " — link lost"; EndReason.UNVERIFIED -> " — unverified deadline"; EndReason.CLOSED -> "" })
                SessionTrace.Board.down(B_CORE, "gen=$handle destroyed with its connection")
            }
        }
        return reason
    }

    /**
     * Arm kernel TCP keepalive 3 s/3 s/3 probes plus TCP_USER_TIMEOUT 12 s on the accepted control
     * socket — Apple's receiver values (carplayd `arm_keepalive`, armed on every control connection).
     * SO_KEEPALIVE alone is useless here: the kernel default idle is 7200 s.
     *
     * `java.net.Socket` cannot express the three TCP-level options, so they go through
     * [android.system.Os.setsockoptInt] (public since API 26) on a descriptor from
     * [android.os.ParcelFileDescriptor.fromSocket]. At this app's API level (minSdk 32, OD-4) that is
     * a DUP of the socket's fd (android-32 ParcelFileDescriptor.fromSocket -> dup -> fcntl
     * F_DUPFD_CLOEXEC): the options belong to the socket, so closing the dup leaves the connection
     * and its options in place. (Below API 29 fromSocket wrapped the socket's OWN fd and `use{}`
     * would have closed the connection — the reason minSdk had to move off 26 before this shipped.)
     * There is no public getsockoptInt at compileSdk 32, so the only readback is `c.keepAlive`;
     * whether this unit's kernel (4.19.283) honours the four options as set and whether SELinux
     * allows them is shown by this line and by the link-loss timing, not asserted. Never fails the
     * connection: a refusal is one W line, and a dead link then ends at the 30 s idle backstop.
     */
    private fun armKeepalive(c: Socket, handle: Long) {
        runCatching {
            c.keepAlive = true   // SO_KEEPALIVE through java.net.Socket
            val pfd = android.os.ParcelFileDescriptor.fromSocket(c) ?: error("fromSocket returned null")
            pfd.use {
                val fd = it.fileDescriptor
                val tcp = android.system.OsConstants.IPPROTO_TCP
                android.system.Os.setsockoptInt(fd, tcp, TCP_KEEPIDLE_LINUX, KEEPALIVE_S)
                android.system.Os.setsockoptInt(fd, tcp, TCP_KEEPINTVL_LINUX, KEEPALIVE_S)
                android.system.Os.setsockoptInt(fd, tcp, TCP_KEEPCNT_LINUX, KEEPALIVE_S)
                android.system.Os.setsockoptInt(fd, tcp, android.system.OsConstants.TCP_USER_TIMEOUT, USER_TIMEOUT_MS)
            }
        }.onSuccess {
            log.i("control keepalive armed (${KEEPALIVE_S}s/${KEEPALIVE_S}s/$KEEPALIVE_S + user-timeout ${USER_TIMEOUT_MS / 1000}s → ~12s dead-link detect; SO_KEEPALIVE=${runCatching { c.keepAlive }.getOrNull()}) gen=$handle")
        }.onFailure {
            log.w("keepalive arm failed (gen=$handle): ${it.javaClass.simpleName}: ${it.message} — a dead link now ends at the 30 s backstop")
        }
    }

    /** [NativeCore.avIdleMs] for a log line: -1 = no A/V ever, -2 = core busy / superseded. */
    private fun avIdleText(av: Long): String = when (av) {
        -1L -> "n/a (no A/V yet)"
        -2L -> "unknown (core busy or superseded)"
        else -> "$av ms"
    }

    /** Every cause, outermost first: class, message, and the errno name of an ErrnoException. */
    private fun causeChain(t: Throwable): String = buildString {
        var e: Throwable? = t
        var depth = 0
        while (e != null && depth < 8) {
            if (depth > 0) append(" <- caused by ")
            append(e.javaClass.name).append(": ").append(e.message)
            if (e is android.system.ErrnoException) append(" [errno=").append(android.system.OsConstants.errnoName(e.errno)).append(']')
            val next = e.cause
            e = if (next === e) null else next
            depth++
        }
    }

    /** An ErrnoException anywhere in the chain carrying a dead-link errno. Message text is never parsed. */
    private fun linkErrno(t: Throwable): Boolean {
        var e: Throwable? = t
        var depth = 0
        while (e != null && depth < 8) {
            if (e is android.system.ErrnoException && e.errno in LINK_ERRNOS) return true
            val next = e.cause
            e = if (next === e) null else next
            depth++
        }
        return false
    }

    private val LINK_ERRNOS = setOf(
        android.system.OsConstants.ETIMEDOUT, android.system.OsConstants.EHOSTUNREACH, android.system.OsConstants.ENETUNREACH,
    )

    // ---- session-end + stall reporting ------------------------------------------------------------

    /**
     * The last control connection went away. [reason] is that connection's [EndReason]: LINK_LOST
     * only on the native pump's dead-link evidence, CLOSED for everything else.
     */
    private fun fireSessionDown(reason: EndReason) {
        if (stopped) { log.i("suppressing session-down from a stopped receiver"); return }
        log.w("*** SESSION DOWN — no live control connection (reason=$reason)")
        onSessionDown?.let { cb -> fenced("onSessionDown") { cb(reason) } }
    }

    /**
     * A dial the phone accepted. If a session is already up this is a straggler retry and means
     * nothing; otherwise it is evidence that the phone can reach us, is answering us, and is still
     * declining to connect.
     */
    private fun noteDialAccepted() {
        // The accepted-dial -> inbound-connection deadline is deliberately NOT armed here. The
        // supervisor already owns it (`INBOUND_TIMEOUT_MS` in `SessionSupervisor.onDialAccepted`,
        // fed by this callback) and logs its expiry; a second clock on the same step would report
        // one fault twice with two different budgets.
        onDialAccepted?.let { cb -> fenced("onDialAccepted") { cb() } }
        if (liveConnections.get() > 0) { dialsSinceInbound.set(0); return }
        val n = dialsSinceInbound.incrementAndGet()
        if (n != STALL_DIALS) return   // report the crossing once, not every dial after it
        log.e("!! STALLED: $n consecutive connect-outs accepted with no inbound control connection on :$port")
        log.e("!! The phone can reach this receiver and is answering the nudge — it is choosing not to")
        log.e("!! dial back. Cause not determined from this side. Reconnecting Bluetooth to the car")
        log.e("!! FROM THE IPHONE cleared it on 2026-08-27.")
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
     * Deliberately does NOT dial :7011 on loopback. Since OD-7F (2026-10-07) [acceptLoop] refuses any
     * local newcomer before the liveness count, so such a dial would only log a refusal and prove
     * nothing about a phone's path. (Before OD-7F it took a liveness slot, spun up a native core and
     * fired a spurious session-down.) Holding the bound, open ServerSocket is the
     * capability that matters and it is directly observable — PROVIDED the socket is closed whenever
     * nobody is accepting on it, which is what [acceptLoop]'s finally guarantees; without it a dead
     * accept thread left this check green over a listener that only ever filled the kernel backlog.
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

        // v5.7 U4 (F025): the relay is the process-delegating one, which resolves the CURRENT probe on
        // every call. `ok` stays "a relay exists" — link state goes into the detail only, because the
        // receiver is built (and this runs) BEFORE runAll makes the link, and any `ok = false` here
        // becomes RX_UNHEALTHY with no guard (SessionSupervisor.onReceiverReady).
        out += Check("MFi relay", mfiRelay != null,
            (mfiRelay as? zeno.gmccpa.ProcessMfiRelay)?.describe()
                ?: if (mfiRelay != null) "OCBM CH_MFI relay attached"
                else "no relay — /auth-setup will fail on every connection")

        val br = runCatching {
            java.net.NetworkInterface.getNetworkInterfaces()?.toList()?.firstOrNull {
                it.isUp && it.name.startsWith("br") && it.inetAddresses.toList().any { a -> a is java.net.Inet4Address }
            }
        }.getOrNull()
        val brAddr = br?.inetAddresses?.toList()?.filterIsInstance<java.net.Inet4Address>()?.firstOrNull()
        out += Check("hotspot bridge", brAddr != null,
            brAddr?.let { "${br.name} ${it.hostAddress}" } ?: "no br* interface with an IPv4 — nothing to advertise on")

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

    fun resumeDiscovery() {
        if (discoveryPaused.compareAndSet(true, false)) log.i("discovery RESUMED")
        // Outside the CAS branch on purpose: the supervisor calls this on EVERY non-quiet phase
        // transition, and most of those find discovery already running — the reset must land anyway.
        resetRediscoverBackoff()
    }

    /**
     * The supervisor's phase lever for [backoffEligible]. Enabling says "the phases where nothing
     * more is expected from the phone"; disabling (or re-enabling) resets the cadence, so any BT
     * progress the supervisor relays through here starts the count over.
     */
    fun setDiscoveryBackoff(enabled: Boolean) {
        val was = backoffEligible.getAndSet(enabled)
        if (was != enabled) log.i("rediscover backoff ${if (enabled) "ELIGIBLE (nothing left to find — cadence may lengthen to ${REDISCOVER_MAX_MS / 1000}s)" else "off"}")
        resetRediscoverBackoff()
    }

    private fun resetRediscoverBackoff() {
        val changed = synchronized(backoffLock) {
            emptyCycles = 0
            val was = rediscoverIntervalMs
            rediscoverIntervalMs = REDISCOVER_MS
            was != REDISCOVER_MS
        }
        if (changed) log.i("rediscover cadence back to ${REDISCOVER_MS / 1000}s")
    }

    /**
     * The OCBM link just came up (or came back): restart discovery NOW so a nudge that
     * [nudgeAllowed] held goes out within the 600 ms restart rather than at the next tick. Safe
     * from any thread and in any state — the loop re-checks `running`, a live session and the
     * pause before acting, so a wake under a streaming session is a no-op. Also resets the backoff:
     * a link-up is progress.
     */
    fun wakeDiscovery(why: String) {
        if (!running.get()) return
        log.i("waking the rediscover loop — $why")
        resetRediscoverBackoff()
        rediscoverWake.release()
    }

    /** One line for a connect-out that found the receiver stopped under it, instead of a bare return. */
    private fun stoppedMidDial(where: String) = log.i("connect-out abandoned at $where — receiver stopped")

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
        val r = selfMdns ?: run {
            log.w("reannounce: self-hosted responder is not up (it did not start — see advert-self)")
            return false
        }
        return try { pool.execute { r.reannounce() }; true }
        catch (t: Throwable) { log.w("reannounce: not dispatched — ${t.javaClass.simpleName}"); false }
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
