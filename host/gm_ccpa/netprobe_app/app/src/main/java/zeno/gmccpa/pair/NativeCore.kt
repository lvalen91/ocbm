package zeno.gmccpa.pair

/**
 * The JNI door to the proven CarPlay receiver core (`ccpa_custom`'s `receiver`/`pairing`/`rtsp`/`mfi`).
 *
 * Kotlin owns the control TCP connection and hands raw bytes through [feed]; everything on the other
 * side — the RTSP state machine, SRP-6a, the Ed25519/X25519 pairing API 32 cannot do, the
 * ChaCha20-Poly1305 channel, `/auth-setup`, SETUP/RECORD — is the ported implementation, not a
 * rewrite.
 *
 * The Kotlin SRP-6a in [SrpServer] stays in the tree: it independently proved pair-setup wire
 * compatibility against a real iPhone, which the reference itself flags as unprovable offline. It is
 * no longer on the session path.
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
     * Distinct from `0`, and the difference decides what the caller must do. `0` means no core at
     * all — the Kotlin stub path is the only option. [BUSY] means the core is fine and simply
     * occupied, almost always by an incumbent inside `ControlServer::feed` doing MFi (12-15 s on the
     * control path). The stub cannot pair, so answering BUSY with it guarantees a failed session;
     * closing the connection instead lets the phone redial into a core that is free by then, which
     * is what Apple's hijack reconnect does anyway.
     */
    const val BUSY: Long = -1L

    /**
     * Feed bytes read from the control socket for connection [handle]; returns bytes to write back.
     * An empty array is normal ("nothing to write this round"). **null means the connection is dead**
     * — a feed error, a panic, or a stale/superseded generation — and the caller must close it.
     */
    fun feed(handle: Long, input: ByteArray): ByteArray? {
        if (handle == 0L) return null
        return nativeFeed(handle, input)
    }

    /**
     * Send a single-touch HID report to the iPhone. Normalized 0..1 in the ADVERTISED geometry.
     * phase: 0=DOWN 1=MOVE 2=UP. Returns false between sessions (no event channel) — not an error.
     *
     * Unlike video/audio this needs no handle: the receiver's event channel is process-global state
     * (`events::send_hid_report`), the same singleton airplayd's :9110 socket path ends at.
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
     * or UI thread — `CarPlayMediaBrowserService` hands these to its own `cp-media-btn` executor.
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

    /** True once pair-verify has completed and the channel flipped to ChaCha20-Poly1305. */
    fun isEncrypted(handle: Long): Boolean = handle != 0L && nativeIsEncrypted(handle)

    /** Destroy connection [handle]'s core. No-ops if [handle] is a superseded generation. */
    fun destroy(handle: Long) {
        if (handle != 0L) nativeDestroy(handle)
    }

    private external fun nativeInit(
        pi: String, edSeed: ByteArray, infoPlist: ByteArray, peerFile: String,
        peerAddr: String, mfiRelay: MfiRelay
    ): Long
    private external fun nativeFeed(handle: Long, input: ByteArray): ByteArray?
    private external fun nativeIsEncrypted(handle: Long): Boolean
    private external fun nativeTouch(phase: Int, nx: Float, ny: Float, w: Int, h: Int): Boolean
    private external fun nativeMediaButton(index: Int): Boolean
    private external fun nativeSetLimitedUI(limit: Boolean): Boolean
    private external fun nativeSetNightMode(night: Boolean): Boolean
    private external fun nativeForceKeyFrame(): Boolean
    private external fun nativeDestroy(handle: Long)
}

/**
 * Called FROM Rust worker threads. Both methods must be blocking and synchronous — `createSignature`
 * runs inside MFi-SAP on the control path, where the phone is waiting on an HTTP reply, so there must
 * be no coroutine boundary here.
 *
 * This build runs d8 with no shrinking (minifyEnabled=false), and proguard-rules.pro already carries
     * the matching keep rules for when that changes, so the method names survive. If R8/ProGuard is ever
 * introduced these need a keep rule — `call_method` resolves them by name and would fail at runtime.
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
