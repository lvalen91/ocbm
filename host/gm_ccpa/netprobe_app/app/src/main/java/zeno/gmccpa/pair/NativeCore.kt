package zeno.gmccpa.pair

/**
 * The JNI door to the proven CarPlay receiver core (`ccpa_custom`'s `receiver`/`pairing`/`rtsp`/`mfi`).
 *
 * Kotlin owns the control TCP connection and hands raw bytes through [feed]; everything on the other
 * side — the RTSP state machine, SRP-6a, the Ed25519/X25519 pairing API 32 cannot do, the
 * ChaCha20-Poly1305 channel, `/auth-setup`, SETUP/RECORD — is the ported implementation, not a
 * rewrite. There is no Kotlin fallback (the SRP-6a pair-setup stub was removed in v5.9, D016): when
 * the core cannot start, the control connection is closed.
 */
object NativeCore {

    val available: Boolean by lazy {
        try { System.loadLibrary("carplayjni"); true }
        catch (t: Throwable) {
            zeno.gmccpa.ProbeLog.sub("jni").e("libcarplayjni.so not loadable: ${t.message}")
            false
        }
    }

    /**
     * Create a native control server and return its **generation handle** (0 = failure). The handle
     * is a token, not a pointer — the Rust side keeps the single live session in a mutex-guarded slot
     * keyed by this generation, so a stale handle's [feed]/[destroy] is a safe no-op rather than a
     * use-after-free. The CALLER owns the returned handle and must pair it with [destroy]; concurrent
     * connections each hold their own handle, and only the generation that installed the core can
     * destroy it (this is what makes connection-hijack safe).
     *
     * @param pi          pairing identity — MUST equal the Bonjour TXT `pi` or pair-verify fails
     * @param edSeed      32-byte Ed25519 accessory seed; stable across runs or pairings break
     * @param infoPlist   the generated `/info` binary plist
     * @param peerFile    where controller-id → LTPK pairings persist
     * @param peerAddr    the phone's address:port — AvSession needs it to reach back for streams
     * @param mfiRelay    the OCBM `CH_MFI` bridge (already device-proven)
     */
    fun start(pi: String, edSeed: ByteArray, infoPlist: ByteArray, peerFile: String,
              peerAddr: String, mfiRelay: MfiRelay): Long {
        if (!available) return 0
        return nativeInit(pi, edSeed, infoPlist, peerFile, peerAddr, mfiRelay)
    }

    /**
     * [start] could not install because the INCUMBENT generation still held the native CORE mutex.
     *
     * Distinct from `0`, though the caller now does the same thing with both: close the connection.
     * `0` means no core at all — the connection is closed and there is no fallback. [BUSY] means
     * the core is fine and simply occupied, almost always by an incumbent inside
     * `ControlServer::feed` doing MFi (12-15 s on the control path); closing lets the phone redial
     * into a core that is free by then, which is what Apple's hijack reconnect does anyway.
     */
    const val BUSY: Long = -1L

    /**
     * Feed bytes read from the control socket for connection [handle]; returns bytes to write back.
     * An empty array is normal ("nothing to write this round"). **null means the connection is dead**
     * — a feed error, a panic, or a stale/superseded generation — and the caller must close it.
     */
    fun feed(handle: Long, input: ByteArray): ByteArray? {
        if (handle <= 0L) return null   // 0 = no core, BUSY (-1) = never installed: nothing to feed
        return nativeFeed(handle, input)
    }

    /**
     * Send a single-touch HID report to the iPhone. Normalized 0..1 in the ADVERTISED geometry.
     * phase: 0=DOWN 1=MOVE 2=UP. Returns false between sessions (no event channel) — not an error.
     *
     * Unlike video/audio this needs no handle: the receiver's event channel is process-global state
     * (`events::send_hid_report`), the same singleton carplayd's :9110 socket path ends at.
     */
    fun touch(phase: Int, nx: Float, ny: Float, w: Int, h: Int): Boolean =
        available && nativeTouch(phase, nx, ny, w, h)

    /** Ask iOS for a fresh IDR. Blocking (event channel) — never call from the UI thread. */
    fun forceKeyFrame(): Boolean = available && nativeForceKeyFrame()

    /**
     * Consumer-usage indices of the uid-2 media-buttons HID device (`receiver/src/hid.rs:11-18`).
     * Index 0 is the release and is sent by the native side after every tap — callers never send it.
     */
    object MediaBtn {
        const val PLAY = 1
        const val PAUSE = 2
        const val PLAY_PAUSE = 3
        const val NEXT = 4
        const val PREV = 5
    }

    /**
     * Tap a media-transport key on the phone.
     *
     * BLOCKING: takes the same global event-channel mutex as [touch], so never call it from a binder
     * or UI thread — `CarPlayMediaBrowserService` hands these to [zeno.gmccpa.av.CarPlayEventTx].
     * Returns false between sessions, which is normal.
     */
    fun mediaButton(index: Int): Boolean = available && nativeMediaButton(index)

    /**
     * Restrict ([limit] = true) or release the CarPlay UI for driving.
     *
     * BLOCKING on the same global event-channel mutex as [touch] and [mediaButton] — never call from
     * a binder or UI thread. `DriveStateWatcher` hands these to its own single-thread executor.
     *
     * Returns false between sessions, which is normal: the command needs a live event channel, and
     * the state is re-asserted on session-up rather than persisted across one.
     */
    fun setLimitedUI(limit: Boolean): Boolean = available && nativeSetLimitedUI(limit)

    /**
     * Mirror the head unit's day/night state into CarPlay's UI.
     *
     * Same threading contract as [setLimitedUI]: BLOCKING on the event-channel mutex, driven from
     * `VehicleStateWatcher`'s executor, false between sessions.
     */
    fun setNightMode(night: Boolean): Boolean = available && nativeSetNightMode(night)

    /**
     * Plan v5.9 MED M5 / OD-9C: take MainAudio from the phone after a PERMANENT media focus loss, the
     * way stock does, so the phone pauses (receiver `events::send_take_main_audio`). False when a take
     * is already outstanding or between sessions.
     *
     * BLOCKING on the event-channel mutex — only from [zeno.gmccpa.av.CarPlayEventTx], which also
     * carries the transport keys, so a give-back queued before a PLAY key reaches the phone first.
     */
    fun takeMainAudio(): Boolean = available && nativeTakeMainAudio()

    /**
     * Give MainAudio back after an OD-9C take: 0 = nothing owed (the normal case), 1 = Untake sent,
     * -1 = owed but the write failed; 0 also when no native core is loaded. Same threading contract
     * as [takeMainAudio].
     */
    fun releaseMainAudio(): Int = if (available) nativeReleaseMainAudio() else 0

    /** True once pair-verify has completed and the channel flipped to ChaCha20-Poly1305. */
    fun isEncrypted(handle: Long): Boolean = handle > 0L && nativeIsEncrypted(handle)

    /**
     * Milliseconds since generation [handle]'s session last RECEIVED A/V or a keepAlive beacon; -1 =
     * none ever, -2 = unknown (core contended by another generation's feed, generation superseded,
     * panic — or a .so without the export). NON-BLOCKING: the native side uses `try_lock`, so the
     * control pump can never stall behind another generation here. Gates the pump's 30 s backstop.
     */
    fun avIdleMs(handle: Long): Long {
        if (handle <= 0L || !available) return -2L
        return try { nativeAvIdleMs(handle) } catch (e: UnsatisfiedLinkError) {
            // A stale libcarplayjni.so: -2 keeps the backstop at the old unconditional 30 s rule.
            if (!avIdleMissingLogged) { avIdleMissingLogged = true; zeno.gmccpa.ProbeLog.sub("jni").w("nativeAvIdleMs missing from libcarplayjni.so (stale native build) — A/V idle reads as unknown") }
            -2L
        }
    }
    @Volatile private var avIdleMissingLogged = false

    /**
     * The per-process auth token for the receiver's `127.0.0.1:9112` mic-ingest seam (F136): 32 hex
     * chars that [zeno.gmccpa.av.MicUplink] sends as its first line, `auth <hex>`. The receiver
     * closes any connection that does not present it in time, so no other app on the head unit can
     * inject mic PCM or learn when the uplink arms. NEVER log it.
     *
     * [localPort] is the port the caller's socket is ALREADY BOUND to (127.0.0.1) and is about to
     * connect from: the receiver reserves it, so that connection gets a pre-auth slot a flood of
     * other apps' connections cannot take (F136 fix round 1).
     *
     * Null when the library is not loadable, when the receiver's listener is not ours (the bind
     * failed — another process holds :9112 — or getrandom failed; then the token is never handed out,
     * so it can never reach a squatter), or for a stale `.so` without the export. Touching
     * [available] loads the library, which is what starts the listener this token unlocks.
     */
    fun micAuth(localPort: Int): String? {
        if (!available) return null
        return try { nativeMicAuth(localPort) } catch (e: UnsatisfiedLinkError) {
            // Once per process: MicUplink asks on every 2 s attempt.
            if (!micAuthMissingLogged) {
                micAuthMissingLogged = true
                zeno.gmccpa.ProbeLog.sub("jni").w("nativeMicAuth missing from libcarplayjni.so (stale native build) — no mic uplink")
            }
            null
        }
    }
    @Volatile private var micAuthMissingLogged = false

    /**
     * Withdraw the `:9112` source-port reservation [micAuth] made for [localPort] (F136 fix round 2).
     * Call it BEFORE closing the socket bound to that port: after a failed connect the reservation
     * would otherwise outlive our hold on the port. A no-op when the receiver already consumed it,
     * when the library is not loadable, or for a stale `.so` without the export (the receiver's 5 s
     * reservation TTL is then the backstop) — so it never throws and never logs.
     */
    fun micRelease(localPort: Int) {
        if (!available) return
        try { nativeMicRelease(localPort) } catch (_: UnsatisfiedLinkError) { }
    }

    /**
     * The receiver's voice clock, in microseconds (plan v5.8 V1a): `receiver::session::voice_clock_us()`,
     * the clock rust stamps every voice AU's `rx_us` and every BEGIN/END `write_us` with. The voice
     * path stamps each read and each write with THIS, so every age it reports (rx→read, rx→write) is
     * on rust's own clock; System.nanoTime is never used for an age. An in-process JNI call, about
     * 133 per second at most. -1 when the library is not loadable or predates the export (no voice
     * audio can arrive then anyway); the caller reports such an age as unknown.
     */
    fun voiceClockUs(): Long {
        if (!available) return -1L
        return try { nativeVoiceClockUs() } catch (_: UnsatisfiedLinkError) { -1L }
    }

    /** Destroy connection [handle]'s core. No-ops if [handle] is a superseded generation. */
    fun destroy(handle: Long) {
        if (handle > 0L) nativeDestroy(handle)
    }

    private external fun nativeInit(
        pi: String, edSeed: ByteArray, infoPlist: ByteArray, peerFile: String,
        peerAddr: String, mfiRelay: MfiRelay
    ): Long
    private external fun nativeFeed(handle: Long, input: ByteArray): ByteArray?
    private external fun nativeIsEncrypted(handle: Long): Boolean
    private external fun nativeAvIdleMs(handle: Long): Long
    private external fun nativeTouch(phase: Int, nx: Float, ny: Float, w: Int, h: Int): Boolean
    private external fun nativeMediaButton(index: Int): Boolean
    private external fun nativeSetLimitedUI(limit: Boolean): Boolean
    private external fun nativeSetNightMode(night: Boolean): Boolean
    private external fun nativeTakeMainAudio(): Boolean
    private external fun nativeReleaseMainAudio(): Int
    private external fun nativeForceKeyFrame(): Boolean
    private external fun nativeDestroy(handle: Long)
    private external fun nativeMicAuth(localPort: Int): String?
    private external fun nativeMicRelease(localPort: Int)
    private external fun nativeVoiceClockUs(): Long
}

/**
 * Called FROM Rust worker threads. Both methods must be blocking and synchronous — `createSignature`
 * runs inside MFi-SAP on the control path, where the phone is waiting on an HTTP reply, so there must
 * be no coroutine boundary here.
 *
 * This build runs d8 with no shrinking (minifyEnabled=false), so nothing renames or strips these
 * today; proguard-rules.pro already carries the matching keep rules (`-keep interface
 * zeno.gmccpa.**MfiRelay*`, `-keep class * implements zeno.gmccpa.**MfiRelay*`) for the day that
 * changes — `call_method` resolves them by name and would fail at runtime without them.
 */
interface MfiRelay {
    fun copyCertificate(): ByteArray
    fun createSignature(digest: ByteArray): ByteArray

    /**
     * Same two operations on a SHORT budget, for callers that must not block the control channel.
     *
     * The iAP2 metadata tunnel's chip ops run inside `ControlServer::feed`, which holds the native
     * CORE and SESSION guards — so the phone's `POST /command` receives no HTTP reply until they
     * return, against a phone-side request timeout we have never measured (no capture, no spec; the
     * nearest sourced figure is CarKit's disassembly-confirmed 30 s `timeoutInterval` — see audit
     * 4.8 verdict). The generous default budgets exist for `/auth-setup`, where the box may
     * legitimately take ~12 s and the phone waits differently.
     *
     * The tunnel is Zero-Ack with no link-layer retransmit timer, so a chip op that gives up early
     * simply fails that handshake attempt and the 120 s budget retries it. A late HTTP reply, by
     * contrast, costs the whole session. Failing fast is the cheaper error.
     */
    fun copyCertificateFast(): ByteArray = copyCertificate()
    fun createSignatureFast(digest: ByteArray): ByteArray = createSignature(digest)
}
