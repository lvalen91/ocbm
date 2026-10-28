package zeno.gmccpa.ocbm

import android.content.Context
import zeno.gmccpa.logging.SessionTrace

/**
 * The OCBM probe section — grows NetProbe from "what can this app see on the network" into the
 * permanent instrument for the CCPA link. Each capability lands here first, observable and
 * isolated, before it becomes product code.
 *
 * Two entry points:
 *   [selfTest]  — framing + client bring-up against a FakeTransport. No hardware, no adapter.
 *   [runAll]    — the real link: claim, HELLO, MGMT_INFO snapshot, SETTIME, MFi, MGMT_INFO, SUBSCRIBE, heartbeat.
 *
 * ## Severity, since 2026-09-10
 *
 * The bring-up ladder in [runAllLocked] used to narrate every failure at INFO: `ABORT: …` through
 * [sink], `cert: NO RESPONSE (timeout)` through `mfiLog.i`. The 2026-09-09 drive produced 634 `I`
 * lines, 7 `W` and 0 `E` across a 15 s MFi sign timeout and an `ocbmd` restart. Now: a step that does
 * not happen is a [SessionTrace] expectation armed with the budget the step already waits
 * ([OcbmClient.hello] 20 s, [OcbmClient.setTime] 2 s, [OcbmClient.MFI_CERT_TIMEOUT_MS] 12 s,
 * [OcbmClient.MFI_SIGN_TIMEOUT_MS] 15 s), an aborted bring-up goes through [abort] at ERROR and marks
 * the link FAILED on the Board, and a chip status that is not OK is an ERROR. `W` is reserved for
 * degraded-but-continuing (no MGMT_INFO, a re-raised permission dialog); `I` narrates success.
 */
class OcbmProbe(context: Context) {

    // Application context only: this object outlives any Activity that created it.
    private val ctx: Context = context.applicationContext

    private val log = zeno.gmccpa.ProbeLog.sub("ocbm")
    private val mfiLog = zeno.gmccpa.ProbeLog.sub("mfi")
    /** Box-side lines, tagged so they are greppable apart from our own. See [startBoxLogStream]. */
    private val boxLog = zeno.gmccpa.ProbeLog.sub("box")
    private val sink: (String) -> Unit = { zeno.gmccpa.ProbeLog.raw(it) }

    /**
     * A bring-up that stops short. ERROR, never INFO: the 2026-09-09 capture showed that an `ABORT`
     * at INFO among six hundred INFO lines is not a finding, it is a line. Also marks
     * [OcbmBoard.LINK] FAILED so every later `## STATUS` block carries the reason.
     */
    private fun abort(why: String, vararg more: String) {
        log.e("ABORT: $why")
        more.forEach { log.e("       $it") }
        SessionTrace.Board.failed(OcbmBoard.LINK, "bring-up aborted: $why")
    }

    // @Volatile: written on the `ops` thread, read by [hasLink] from NsdManager's callback thread
    // (the receiver's dial gate) and by the restart progress timer on the UI thread.
    @Volatile var client: OcbmClient? = null
        private set
    private var usb: UsbBulkTransport? = null

    /**
     * Is there an OCBM link the CH_MFI relay can answer on — i.e. has THIS client's CT_HELLO been
     * ACKed? `client != null` is not enough (it is set before HELLO, OcbmProbe.runAllLocked) and
     * `subscribed` is too much (the mid-session relink deliberately never subscribes, yet its relay
     * must serve /auth-setup). Read by the receiver before every self-provoked connect-out.
     */
    fun hasLink(): Boolean = client?.helloAcked == true

    /**
     * The OCBM accessory is on the bus right now (VID/PID, as [awaitClaimable] polls it). No logging,
     * no permission check. The box relink reads it after a claimed-but-no-HELLO run: still
     * enumerated = a box that is not answering (re-arm with backoff, v5.6 H1-4); gone = unplugged
     * (today's "press Start"). The current transport when there is one; a throwaway otherwise.
     */
    fun adapterEnumerated(): Boolean =
        runCatching { (usb ?: UsbBulkTransport(ctx, zeno.gmccpa.ProbeLog.silent())).findQuiet() != null }.getOrDefault(false)

    /**
     * The CT_HELLO nonce every client this probe builds identifies itself with. Minted ONCE per
     * probe: a USB re-attach reuses the probe (`SessionCore.ocbm()`), so the box sees the same host
     * come back — the "USB blip, warm reuse" contract — while Restart Session and Stop->Start drop
     * the probe and the next one presents a new nonce, which is what makes a lost CT_STOP
     * recoverable. See [OcbmClient.hello] for the box-side consequences of getting this wrong.
     */
    private val hostInstance: Int = OcbmClient.newHostInstance()

    /**
     * Set by [stop] when the USB read thread would not leave `bulkTransfer`, so the interface stayed
     * claimed (UsbBulkTransport.stop). The caller that then builds a replacement probe needs this to
     * report the re-claim failure honestly: "claimInterface refused — another process holds it" is
     * literally true and completely misleading when the other process is us. No box verb clears it;
     * only a replug or a process restart does.
     */
    @Volatile var usbReleaseDeferred = false; private set

    /** What [awaitClaimable] is currently waiting on ("absent" / "present-no-permission" / …), for a caller's progress line. */
    val claimWaitState: String get() = lastClaimState

    /**
     * End an in-flight [awaitClaimable] WITHOUT stopping the probe. For a caller that is about to
     * queue a stop-and-restart behind the very command that is sitting in the wait: the command
     * executor is single-threaded, so the queued stop cannot run until the wait ends, and the wait
     * only ends on this flag. Idempotent; harmless when nothing is waiting.
     */
    fun abortClaimWait() { claimAbort = true }

    /**
     * Cancels an in-flight [awaitClaimable] from OFF the `ops` thread.
     *
     * [runAll] is a blocking `ops.submit{}.get()`, and `awaitClaimable` inside it waits up to
     * [CLAIM_WAIT_MS] (ten minutes). Every UI command shares ONE command executor (SessionCore's `netprobe-cmd`),
     * and [stop] itself was `ops.submit { stopLocked() }.get()` -- i.e. it queued behind the very
     * task it needed to cancel. Start with no adapter attached and Start, Stop, the then-Recover button, the
     * credentials screen and every `--es run` verb were dead for ten minutes with the UI still
     * saying "looking for the adapter...". Nothing interrupted the wait either: `stop()` used
     * `shutdown()`, not `shutdownNow()`, and only after the `.get()` returned.
     *
     * A flag rather than an interrupt on purpose: interrupting mid-`runAllLocked` could leave the
     * USB interface half-claimed, whereas this unwinds through the normal ABORT path.
     */
    @Volatile private var claimAbort = false

    /** The last state [awaitClaimable] observed, so a ten-minute give-up can say what it was waiting on. */
    @Volatile private var lastClaimState = ""

    /**
     * Sticky Stop (2026-10-06): set by [stop] next to [claimAbort] and never cleared — a stopped
     * probe is discarded. [awaitClaimable] clears [claimAbort] on entry, so a relink queued on [ops]
     * behind a busy verb when Stop ran would otherwise reset the abort, wait up to ten minutes while
     * Stop's own `ops.submit{}.get()` hung behind it, or claim + SUBSCRIBE and then get CT_STOP.
     * [runAllLocked] returns at once and the wait loop exits when this is set.
     */
    @Volatile var stopped = false; private set

    /** True for the whole of [awaitClaimable] — the "claim wait active" of the adapter-state text. */
    @Volatile var claimWaitActive = false; private set
    /**
     * The caller's `mayRequest` policy refused the permission dialog this interval (logged once per
     * deferral). Cleared when a request goes out, and at the entry and the exit of [awaitClaimable].
     */
    @Volatile var requestDeferred = false; private set
    /**
     * The last [awaitClaimable] ended at its [CLAIM_WAIT_MS] deadline — not by Stop, an abort or an
     * interrupt. Read by the box relink, which re-arms once per expiry only while CarPlay is live.
     */
    @Volatile var waitExpired = false; private set

    /**
     * Permission requests made for the CURRENT device instance (the adapter's `deviceId`), across
     * waits: a replug is a new instance with a fresh count. `mayRequest(n)` receives this. A per-wait
     * local would hand instance B the n=1 of instance A and defer it for the whole session. The log's
     * `#n` and the [OcbmExpect.USB_PERMISSION] arm count per WAIT instead (a local in
     * [awaitClaimableLoop]): counted per instance, a Start after an expired wait logged "#2 after 0s"
     * and never armed the expectation (2026-10-06).
     */
    @Volatile private var requestsThisInstance = 0
    @Volatile private var requestDeviceId = -1
    /** When the dialog was last requested; [rerequestNow] may reset it. */
    @Volatile private var lastRequestAt = 0L
    /** The last request went out while no gm_ccpa activity was resumed — see [rerequestNow]. */
    @Volatile private var requestSentWhileBackground = false

    /**
     * Incremented on every HELLO-ACKed claim in [runAllLocked]. [runAll] captures it at submit, so a
     * call that was QUEUED behind a bring-up (the box relink holding [ops] in [awaitClaimable] while
     * the driver replugs and launches) can see that a link came up meanwhile and leave it alone
     * instead of CT_STOP + re-SUBSCRIBE — a host_present flap seconds after a good link.
     */
    @Volatile private var linkEpoch = 0
    /** What the link that bumped [linkEpoch] achieved, returned by a queued call that leaves it up. */
    @Volatile private var lastLink = LinkResult()

    /**
     * Fired from the transport-death path ([UsbBulkTransport.onTransportDead]) with the client whose
     * transport died and the reason it was ended, on the `ocbm-read` thread. Two causes: the read
     * loop ended on its own (ENODEV, read errors; reason null), or — since v5.6 H1-1 — the
     * heartbeat declared the link dead after 5 failed OUT writes with reads still timing out
     * normally ([UsbBulkTransport.declareDead]; reason non-null). That second case was the known gap
     * before 2026-10-06: a half-dead gadget never ended the read loop, so this never fired.
     * SessionCore binds it in `ocbm()`'s rebind block (`SessionCore.onBoxTransportDead`) to deliver
     * the box-lost edge and arm the box relink; the reason decides whether that relink counts toward
     * its dead-run cap.
     */
    var onLinkDead: ((OcbmClient, String?) -> Unit)? = null

    /**
     * The app's resumed-activity count says gm_ccpa is in front, for [rerequestNow]. Set by
     * SessionCore.ocbm() (`AppForeground.any()`); true when unknown.
     */
    var appInForeground: () -> Boolean = { true }

    /**
     * Re-raise the permission dialog on the next poll, because a gm_ccpa activity just came to the
     * foreground (onResume) and a dialog requested from the background may never have been shown
     * (unknown on this unit). No dialog storm: dismissing the dialog resumes the activity under it,
     * so this resets only when the last request is at least [REQUEST_INTERVAL_MS] old or was sent
     * while no gm_ccpa activity was resumed. Still subject to the caller's `mayRequest`. No-op when
     * no claim wait is running.
     */
    fun rerequestNow() {
        if (!claimWaitActive) return
        val age = android.os.SystemClock.elapsedRealtime() - lastRequestAt
        if (lastRequestAt != 0L && (age >= REQUEST_INTERVAL_MS || requestSentWhileBackground)) {
            requestSentWhileBackground = false
            lastRequestAt = 0L
            sink("  app in front — the USB permission dialog will be re-raised on the next poll")
        }
    }

    /**
     * UI observers, held on the probe rather than the client because the client is destroyed and
     * rebuilt on every [runAll] (a re-claim, a USB re-attach). A caller that wired the client
     * directly would silently stop receiving events after the first re-claim; these are re-attached
     * to each new client as it is created.
     */
    var onSessionEvent: ((Byte) -> Unit)? = null
    var onPairingCode: ((String) -> Unit)? = null
    var onBtPhase: ((Byte) -> Unit)? = null
    /** `CT_BOX_HEALTH` — the box's readiness bitmask, pushed on change (not polled). */
    var onBoxHealth: ((Int) -> Unit)? = null
    var onPhoneIdent: ((String) -> Unit)? = null
    var onProjMode: ((Byte) -> Unit)? = null
    /**
     * The MGMT_INFO JSON taken right after each link's HELLO (the bring-up snapshot below), or null
     * when the box did not answer — called either way, once per link-up, so the screen never keeps a
     * previous adapter's Bluetooth name for a new link. Carries the client the reply came from, so
     * the receiver can drop a reply from a link that died or was stopped meanwhile. Invoked on the
     * probe's `ops` thread — never `ocbm-read`, which mgmtGetInfo itself depends on.
     */
    var onMgmtInfo: ((OcbmClient, String?) -> Unit)? = null

    /**
     * The vehicle hotspot the iPhone should be sent to by the 0x5703 handoff. The passphrase cannot
     * be read programmatically on the head unit (SecurityException on getSoftApConfiguration,
     * EACCES on hostapd.conf), so the user types it in once from the OS Hotspot GUI.
     */
    var wifiSsid: String? = null
    var wifiPass: String? = null

    /**
     * The CT_SUBSCRIBE config blob.
     *
     * `wireless`/`pairing`/`wifi_ap`/`wifi_*` are read by the box supervisor's raw greps;
     * `accessoryConfig` would be read by carplayd's serde. Unknown keys are silently ignored by
     * both, so sending a key the deployed box doesn't understand yet is always safe.
     */
    fun btOnlyConfig(): ByteArray = SubscribeConfig.build(wifiSsid, wifiPass)

    // ---- headless self-test ---------------------------------------------------------------------

    /**
     * Proves the framing and the client state machine with no adapter attached, by playing the box
     * side against a FakeTransport. This is the seam paying for itself: every byte layout below is
     * checked before we ever blame the hardware.
     */
    fun selfTest() {
        sink("")
        sink("==================== OCBM SELF-TEST (no hardware) ====================")
        var pass = 0
        var fail = 0
        fun check(name: String, cond: Boolean, detail: String = "") {
            if (cond) { pass++; sink("  PASS  $name") }
            else { fail++; sink("  FAIL  $name  $detail") }
        }

        // 1. Header layout, byte for byte.
        val f = Framing.frame(Ocbm.CH_CTRL, Ocbm.F_BOTH, 0, byteArrayOf(Ocbm.CT_HEARTBEAT))
        check("frame size = 16 + payload", f.size == 17, "got ${f.size}")
        check("magic on the wire is 4D 42 43 4F",
            f[0] == 0x4D.toByte() && f[1] == 0x42.toByte() && f[2] == 0x43.toByte() && f[3] == 0x4F.toByte(),
            "got %02x %02x %02x %02x".format(f[0], f[1], f[2], f[3]))
        check("length counts payload only, LE", f[4] == 1.toByte() && f[5] == 0.toByte())
        check("channel LE u16 at off 8", f[8] == 0.toByte() && f[9] == 0.toByte())
        check("flags = SOM|EOM", f[10] == 0x03.toByte())
        check("hcheck = XOR of bytes 0..10", f[11] == Framing.hcheck(f, 0))

        // 2. Reassembly and byte-wise resync.
        val r = Reassembler()
        r.push(f, f.size)
        val got = r.next()
        check("round-trips one frame", got != null && got.channel == Ocbm.CH_CTRL &&
            got.payload.size == 1 && got.payload[0] == Ocbm.CT_HEARTBEAT)
        check("drains to empty", r.next() == null)

        // A frame split across two reads must survive — USB boundaries are not frame boundaries.
        val r2 = Reassembler()
        r2.push(f.copyOfRange(0, 9), 9)
        check("partial header yields nothing yet", r2.next() == null)
        r2.push(f.copyOfRange(9, f.size), f.size - 9)
        check("frame reassembles across reads", r2.next() != null)

        // Junk ahead of a good frame must be resynced through, one byte at a time.
        val r3 = Reassembler()
        val junk = byteArrayOf(0x11, 0x22, 0x33)
        r3.push(junk, junk.size); r3.push(f, f.size)
        check("resyncs past leading junk", r3.next() != null)
        check("counted exactly 3 resync bytes", r3.resyncBytes == 3L, "got ${r3.resyncBytes}")

        // Two frames in one read is the normal case, not an edge case.
        val r4 = Reassembler()
        val two = f + f
        r4.push(two, two.size)
        check("two frames in one read", r4.next() != null && r4.next() != null && r4.next() == null)

        // 3. MFi sub-framing — the one big-endian field family in the protocol.
        val certReq = Mfi.certRequest(0x7A)
        check("cert request is 01 00 00 + tag",
            certReq.size == 4 && certReq[0] == 0x01.toByte() && certReq[1] == 0.toByte() &&
                certReq[2] == 0.toByte() && certReq[3] == 0x7A.toByte())
        val signReq = Mfi.signRequest(ByteArray(20), 0x7B)
        check("sign request is 02 00 14 + 20B + tag (length BIG-endian)",
            signReq.size == 24 && signReq[0] == 0x02.toByte() && signReq[1] == 0x00.toByte() &&
                signReq[2] == 0x14.toByte() && signReq[23] == 0x7B.toByte())
        val mfiResp = byteArrayOf(0x00, 0x03, 0xB1.toByte()) + ByteArray(945)
        val parsed = Mfi.parse(mfiResp, mfiResp.size)
        check("945-byte UNTAGGED cert response parses (older box)",
            parsed != null && parsed.ok && parsed.payload.size == 945 && parsed.tag == null,
            "got ${parsed?.payload?.size} tag=${parsed?.tag}")
        // The correlation the length check structurally cannot do: two 128-byte signatures.
        val tagged = byteArrayOf(0x00, 0x00, 0x80.toByte()) + ByteArray(128) + byteArrayOf(0x5C)
        val pTagged = Mfi.parse(tagged, tagged.size)
        check("tagged 128-B signature keeps payload and tag separate",
            pTagged != null && pTagged.payload.size == 128 && pTagged.tag == 0x5C.toByte(),
            "payload=${pTagged?.payload?.size} tag=${pTagged?.tag}")
        // A trailing byte is a TAG, never payload — the length field is authoritative.
        check("declared length wins over trailing bytes",
            Mfi.parse(byteArrayOf(0x00, 0x00, 0x02, 0x11, 0x22, 0x33), 6)?.payload?.size == 2)

        // 4. Client bring-up against a scripted box.
        val fake = FakeTransport()
        // trace = false: the scripted box below deliberately answers SEV_HOST_GONE and never answers
        // HOST_PRESENT — with tracing on, a self-test would print a FAILED link and a missing
        // expectation for hardware that does not exist. See the OcbmClient constructor KDoc.
        val c = OcbmClient(fake, zeno.gmccpa.ProbeLog.silent(), hostInstance, trace = false)
        c.start()
        val helloThread = Thread { c.hello(4000) }
        helloThread.start()
        val wrote = fake.takeWritten(2000)
        val helloLen = Ocbm.HDR_LEN + 6 + OcbmClient.HOST_LABEL.toByteArray(Charsets.UTF_8).size
        check("client emits CT_HELLO first (hdr + nonce + label)",
            wrote != null && wrote.size == helloLen &&
                wrote[Ocbm.HDR_LEN] == Ocbm.CT_HELLO && wrote[Ocbm.HDR_LEN + 1] == Ocbm.VERSION,
            "got ${wrote?.size} expected $helloLen")
        check("CT_HELLO instance nonce is non-zero (0 = 'not supplied' to the box)",
            wrote != null && wrote.size >= Ocbm.HDR_LEN + 6 &&
                (wrote[Ocbm.HDR_LEN + 2].toInt() or wrote[Ocbm.HDR_LEN + 3].toInt() or
                 wrote[Ocbm.HDR_LEN + 4].toInt() or wrote[Ocbm.HDR_LEN + 5].toInt()) != 0)
        // Answer with a HELLO_ACK carrying the full cap set including MFI.
        val ackPayload = byteArrayOf(Ocbm.CT_HELLO_ACK, 1, 0x3F, 0, 0, 0, Ocbm.MODE_PROJECTION)
        fake.feed(Framing.frame(Ocbm.CH_CTRL, Ocbm.F_BOTH, 0, ackPayload))
        helloThread.join(4000)
        check("HELLO_ACK accepted", c.helloAcked)
        check("caps decoded LE from payload[2..6]", c.caps == 0x3F, "got 0x%08x".format(c.caps))
        check("CAP_MFI detected", c.hasMfi)

        // HOST_GONE must clear subscribed, or the client heartbeats into the void forever.
        c.subscribe(btOnlyConfig())
        check("subscribe sets the latch", c.subscribed)
        fake.feed(Framing.frame(Ocbm.CH_CTRL, Ocbm.F_BOTH, 0,
            byteArrayOf(Ocbm.CT_SESSION_EVENT, Ocbm.SEV_HOST_GONE)))
        Thread.sleep(150)
        check("SEV_HOST_GONE clears subscribed (re-arm path)", !c.subscribed)
        c.stop()

        sink("")
        sink("SELF-TEST: $pass passed, $fail failed")
        if (fail == 0) sink("=> framing + client state machine are correct with no hardware in the loop.")
    }

    // ---- the real link ---------------------------------------------------------------------------

    /** Serializes every session-control operation, so no two can interleave on one device. */
    private val ops = java.util.concurrent.Executors.newSingleThreadExecutor { r ->
        Thread(r, "ocbm-session").apply { isDaemon = true }
    }

    /**
     * What a bring-up attempt actually achieved.
     *
     * [runAll] used to return `Unit`, and every failure inside [runAllLocked] is reported with an
     * [abort] (ERROR + Board FAILED; until 2026-09-10 an INFO `sink("ABORT: ...")`) and a bare
     * `return` -- it throws only for a credential-less subscribe. So
     * "no adapter attached", "claim failed" and "no CT_HELLO_ACK" all returned NORMALLY, the caller's
     * `catch (t: Throwable)` never fired, and the caller (then `MainActivity`, now `SessionCore.autoStart`) went straight on to
     * `supervisor.onBoxLinked()`. The app announced "box claimed, MFi proven" with no box on the bus
     * at all -- device-observed 2026-08-28. A status word that lies about the thing it exists to
     * report is worse than no status word, so the outcome is now a RETURN VALUE, not an exception,
     * and the caller cannot forget to look at it.
     */
    data class LinkResult(
        /** The USB interface was claimed. False means no adapter, no permission, or a claim failure. */
        val claimed: Boolean = false,
        /** CT_HELLO -> CT_HELLO_ACK completed. This is what "box linked" actually means. */
        val helloOk: Boolean = false,
        /** A real 945-byte certificate AND a real 128-byte signature came back over CH_MFI. */
        val mfiProven: Boolean = false,
        /** CT_SUBSCRIBE was sent (the radio-wake edge) and the heartbeat is running. */
        val subscribed: Boolean = false,
        /**
         * Monotonic (System.nanoTime) ms at CT_HELLO_ACK; 0 without one. The box relink measures
         * "a heartbeat death < 60 s after the previous relink's HELLO" from it (v5.6 H1-4).
         */
        val helloAtMs: Long = 0L,
        /**
         * v5.8 V7 / Q018: the bring-up THREW (`ExceptionName: message`), so none of the flags above
         * describe what happened. Set only by the box relink's synthetic result ([zeno.gmccpa.RelinkChain.Env.failed]).
         */
        val error: String? = null,
    ) {
        /** One line for the status word, naming what actually happened rather than what was hoped. */
        fun failureDetail(): String = when {
            error != null -> "the relink threw: $error"
            !claimed -> "no adapter claimed — is it plugged into this head unit?"
            !helloOk -> "adapter claimed but no CT_HELLO_ACK — is ocbmd running on the box?"
            else -> "linked"
        }
    }

    /**
     * Full bring-up: claim, HELLO, SETTIME, MFi, then CT_SUBSCRIBE + heartbeat.
     *
     * [subscribe] = false stops one step short, restoring the link and the CH_MFI relay WITHOUT
     * sending CT_SUBSCRIBE. That matters because SUBSCRIBE is the radio-wake edge: it flips
     * host_present 0->1 and the box's supervisor brings its BT stack up off that edge. A box that
     * re-enumerates mid-session (USB bumped, box power-cycled) must not drag its radios up underneath
     * a CarPlay session that is still streaming — but the relay DOES have to come back, because
     * /auth-setup is per control connection, not per pairing, so the next reconnect or hijack needs
     * the MFi coprocessor or it fails to authenticate.
     */
    fun runAll(subscribe: Boolean = true): LinkResult = runAll({ subscribe })

    /**
     * The box relink's form (2026-10-06). [subscribe] is evaluated at the subscribe decision, AFTER
     * the claim, HELLO and MFi — so a grant that lands after the session ended still subscribes.
     * [mayRequest] is evaluated each time the dialog's request interval elapses, with the requests
     * made for this device instance; false defers the dialog (logged once, with [deferredWhy]) and
     * re-asks on every poll. [quietExpiry] is evaluated at the ten-minute deadline: true logs it at
     * I instead of an ABORT — for the box relink, an automatic background wait that re-arms while
     * CarPlay is live, where an E every 10 min would be false. The epoch is captured HERE, at submit
     * — see [linkEpoch]. [deferredWhy] is read when a deferral is logged, right after the refusing
     * `mayRequest`, so the logged reason is the one for that refusal. [cancelled] is polled by the
     * claim wait next to [claimAbort]: the relink's own Stop/Restart signal, because a Stop that
     * lands before the wait starts has its [abortClaimWait] cleared at the wait's entry and its
     * sticky [stopped] may still be queued behind a busy verb (2026-10-06).
     */
    fun runAll(
        subscribe: () -> Boolean,
        mayRequest: ((Int) -> Boolean)? = null,
        deferredWhy: () -> String = { "" },
        quietExpiry: () -> Boolean = { false },
        cancelled: () -> Boolean = { false },
    ): LinkResult {
        val e = linkEpoch
        return ops.submit<LinkResult> { runAllLocked(subscribe, mayRequest, deferredWhy, quietExpiry, cancelled, e) }.get()
    }

    /**
     * A CT_SUBSCRIBE on the link that is already up — no stopLocked, no re-claim (2026-10-06). The
     * ladder's rung 1 uses it for a link restored under the last session WITHOUT a subscribe:
     * [runAll] would stopLocked first, which pulls box logs over the live link and drops the claim
     * and the MFi relay exactly when a returning phone needs /auth-setup. A SUBSCRIBE on an
     * established HELLO'd link is the heartbeat's own recovery path, so this is not a new protocol
     * state. Refuses unless the link is up, unsubscribed AND never subscribed: a link that did
     * subscribe and took a box HOST_GONE is in the heartbeat's re-SUBSCRIBE backoff, which an extra
     * immediate SUBSCRIBE would defeat. [stillWanted] is re-checked when this finally runs on
     * [ops] (a box-log pull can hold it for 24 s): CarPlay going live meanwhile cancels it.
     */
    fun subscribeExisting(stillWanted: () -> Boolean): Boolean = try { ops.submit<Boolean> {
        if (stopped) return@submit false
        val c = client
        when {
            c == null || !c.helloAcked -> { log.i("subscribe skipped — no OCBM link (it went away while queued)"); false }
            c.subscribed -> { log.i("subscribe skipped — the link is already subscribed"); false }
            c.everSubscribed -> { log.i("subscribe skipped — this link subscribed before and is in HOST_GONE recovery; its heartbeat re-subscribes with backoff"); false }
            !stillWanted() -> { log.i("subscribe skipped — CarPlay went live while queued"); false }
            wifiSsid.isNullOrBlank() || wifiPass.isNullOrBlank() -> {
                val missing = if (wifiSsid.isNullOrBlank()) "SSID" else "passphrase"
                // The same refusal as runAllLocked, and for the same reason: the box applies 0x5703
                // credentials only at the host_present edge this subscribe takes.
                log.e("REFUSING to subscribe: no hotspot $missing supplied — the deferred CT_SUBSCRIBE is not sent")
                SessionTrace.Board.failed(OcbmBoard.LINK, "CT_SUBSCRIBE refused: no hotspot $missing — supply --es ssid/--es pass or fill the hotspot fields")
                false
            }
            else -> {
                sink(">> deferred CT_SUBSCRIBE on the existing link (no re-claim, MFi relay kept)")
                c.subscribe(btOnlyConfig())
                c.startHeartbeat()
                startBoxLogStream()
                true
            }
        }
    }.get() } catch (_: java.util.concurrent.RejectedExecutionException) {
        log.i("subscribe skipped — this probe was stopped"); false
    }

    private companion object {
        /** Poll cadence. hasPermission() is a cheap binder call; 2 s is responsive without churn. */
        const val POLL_MS = 2_000L
        /** How often the permission dialog may be re-raised. Rarely, so this is never a dialog storm. */
        const val REQUEST_INTERVAL_MS = 30_000L
        /** Overall patience. Long enough to cover walking to the vehicle and replugging. */
        const val CLAIM_WAIT_MS = 10 * 60_000L
        /** How finely [awaitClaimable] slices its sleep so a stop is observed promptly. */
        const val ABORT_SLICE_MS = 250L
        /**
         * How long the post-sign-timeout re-HELLO in [runAllLocked] waits for CT_HELLO_ACK.
         *
         * A restarted `ocbmd` ACKs in milliseconds (2 ms device-observed 2026-09-09); [OcbmClient.hello]
         * retransmits every 500 ms, so 4 s is eight attempts against a daemon that is either back or
         * genuinely blocked in its single dispatch thread. Long enough that a busy-but-alive daemon
         * cannot be mistaken for a wedged one, short enough that a wedged one costs seconds, not the
         * default 20 s HELLO budget, on top of the 15 s sign timeout that got us here.
         */
        const val REHELLO_TIMEOUT_MS = 4_000L
        /**
         * `CH_LOG` backfill cap, in KiB.
         *
         * Enabling the stream replays each source from offset 0 — that replay IS the backfill, and
         * it is bounded by this. 256 KiB (the box's default) is roughly a session's worth of
         * narration; larger buys older history at the cost of a burst on every subscribe, and every
         * one of those lines is tagged so it can never be mistaken for live evidence.
         */
        const val BOX_LOG_CAP_KB = 256
        /**
         * Per-file budget for the final box-log pull on the QUICK stop path ([stop] with
         * `quick = true`). The default 6 s x four files is 24 s against a box that has stopped
         * answering — and a box that has stopped answering is exactly the state a driver presses
         * Restart Session in. A live box serves each file in milliseconds, so 1 s loses nothing from
         * a healthy pull and caps the dead one at 4 s, inside the restart's own 5 s settle.
         */
        const val QUICK_PULL_MS = 1_000L
        /**
         * Box logs the push stream does NOT carry, pulled once at session end.
         *
         * `CH_LOG` follows eleven sources (box, airplayd, iap2d, aa-bridge, rx-connect, bt, wl,
         * radio_*) and these are not among them — `ocbmd` and the supervisor now narrate into
         * `/tmp/box.log`, which IS streamed, but the Wi-Fi and boot legs still only exist on disk.
         * Verified against box HEAD 2026-09-08.
         */
        val BOX_LOG_SNAPSHOT_FILES = listOf(
            "/tmp/wlan.log", "/tmp/wlan_off.log", "/tmp/supervisor.log", "/tmp/ocbm_boot.log",
        )
    }

    /**
     * Keep looking until the adapter is present AND we hold permission for it — do not give up.
     *
     * The old path was one-shot: `find()` once, `ensurePermission()` once (blocking 20 s on a
     * broadcast), and on failure `ABORT` with nothing ever trying again. On this head unit the
     * permission dialog frequently never appears and the result broadcast is often lost, so a single
     * attempt is a coin flip — and losing it left the app idle with the adapter plugged in.
     *
     * The loop polls [UsbBulkTransport.hasPermission], which is authoritative whether or not any
     * broadcast arrives, and only re-raises the dialog every [REQUEST_INTERVAL_MS] so it cannot
     * become a dialog storm. (Corrected 2026-10-06: this said the reliable grant path was an
     * ACTION_USB_DEVICE_ATTACHED launch granting implicitly with no dialog. On this unit the attach
     * route never runs — GM routes attach to an absent fixed handler — permission is per device
     * instance, and every replug needed the dialog plus a user tap. This loop finding the replugged
     * adapter by polling, then raising the dialog, IS the path; the box relink re-arms it on
     * transport death so it also runs with no launch.)
     *
     * Logging is edge-triggered: one line per state change, not per poll, so a long wait cannot
     * flood a 16 MiB ring buffer. Two additions (2026-09-10) keep the wait from LOOKING dead
     * without breaking that rule — the reference failure is an adapter that is present but never
     * granted, which produced exactly two lines in ten minutes:
     *  - the first dialog request arms [OcbmExpect.USB_PERMISSION] with [REQUEST_INTERVAL_MS], so a
     *    grant that does not arrive before the dialog would be re-raised is an `EXPECTED-MISSING`
     *    at ERROR (once — it is not re-armed on later requests);
     *  - each re-raise after that is one WARN line with the elapsed time, i.e. one line per 30 s
     *    for as long as the state persists, never per poll;
     *  - the waiting states are written under [OcbmBoard.USB_CLAIM] so a status block says
     *    "adapter present, USB permission not held" rather than nothing.
     */
    private fun awaitClaimable(
        t: UsbBulkTransport,
        mayRequest: ((Int) -> Boolean)?,
        deferredWhy: () -> String,
        cancelled: () -> Boolean,
    ): android.hardware.usb.UsbDevice? {
        claimWaitActive = true
        requestDeferred = false
        waitExpired = false
        try {
            val dev = awaitClaimableLoop(t, mayRequest, deferredWhy, cancelled)
            if (dev == null && !claimAbort && !stopped && !Thread.currentThread().isInterrupted) waitExpired = true
            return dev
        } finally {
            requestDeferred = false
            claimWaitActive = false
        }
    }

    private fun awaitClaimableLoop(
        t: UsbBulkTransport,
        mayRequest: ((Int) -> Boolean)?,
        deferredWhy: () -> String,
        cancelled: () -> Boolean,
    ): android.hardware.usb.UsbDevice? {
        val deadline = android.os.SystemClock.elapsedRealtime() + CLAIM_WAIT_MS
        val startedAt = android.os.SystemClock.elapsedRealtime()
        claimAbort = false
        lastRequestAt = 0L
        var lastState = ""
        var polls = 0
        // Per wait: the `#n` in the log and the expectation arm (see [requestsThisInstance]).
        var requestsThisWait = 0
        var deferredLogged: String? = null
        lastClaimState = "not started"
        while (android.os.SystemClock.elapsedRealtime() < deadline) {
            // The caller's cancel becomes claimAbort, so every "was it cancelled" check after the
            // wait (waitExpired, runAllLocked's narration) reads it as the Stop it is.
            if (!claimAbort && cancelled()) claimAbort = true
            if (claimAbort || stopped || Thread.currentThread().isInterrupted) {
                sink("giving up the wait for the adapter (stop requested)")
                SessionTrace.cancel(OcbmExpect.USB_PERMISSION, "stop requested while waiting for the adapter")
                return null
            }
            val dev = t.findQuiet()
            val state = when {
                dev == null -> "absent"
                t.hasPermission(dev) -> "claimable"
                else -> "present-no-permission"
            }
            if (state != lastState) {
                lastState = state
                lastClaimState = state
                when (state) {
                    "absent" -> {
                        sink("waiting for the OCBM accessory 0x1314:0x2d00 to appear ...")
                        SessionTrace.Board.down(OcbmBoard.USB_CLAIM, "waiting for the adapter to appear on the bus")
                    }
                    "present-no-permission" -> {
                        sink("adapter present; waiting for USB permission ...")
                        SessionTrace.Board.down(OcbmBoard.USB_CLAIM, "adapter ${dev?.deviceName} present — USB permission NOT held")
                    }
                    else -> sink("adapter present and permission held")
                }
            }
            if (dev != null && state == "claimable") {
                if (polls > 0) sink("  (claimable after ${polls * POLL_MS / 1000}s of waiting)")
                SessionTrace.met(OcbmExpect.USB_PERMISSION)
                return dev
            }
            if (dev != null) {
                val now = android.os.SystemClock.elapsedRealtime()
                // Per device instance: a replug is a new instance, and every new instance needs its
                // own dialog plus a tap on this unit (2026-10-06).
                if (dev.deviceId != requestDeviceId) { requestDeviceId = dev.deviceId; requestsThisInstance = 0 }
                if (now - lastRequestAt >= REQUEST_INTERVAL_MS && mayRequest != null && !mayRequest(requestsThisInstance)) {
                    // Deferred, not skipped: lastRequestAt is NOT advanced, so the request goes out
                    // on the first poll (<= POLL_MS) where the policy turns true. Logged once per
                    // deferral, and again only if its reason changes (e.g. the gear reaches Park on
                    // an instance already asked).
                    val why = deferredWhy()
                    if (!requestDeferred || why != deferredLogged) {
                        requestDeferred = true
                        deferredLogged = why
                        sink("  USB permission needed — request deferred ($why)")
                        SessionTrace.Board.down(OcbmBoard.USB_CLAIM, "adapter ${dev.deviceName} present — USB permission NOT held; request deferred ($why)")
                    }
                } else if (now - lastRequestAt >= REQUEST_INTERVAL_MS) {
                    requestDeferred = false
                    deferredLogged = null
                    lastRequestAt = now
                    requestsThisInstance++
                    val requests = ++requestsThisWait
                    // Fallback for a dialog requested from the background, which may never have been
                    // shown (unknown on this unit): onResume re-raises it via rerequestNow().
                    requestSentWhileBackground = !appInForeground()
                    if (requests == 1) {
                        sink("  requesting USB permission (accept on the head unit)")
                        SessionTrace.expect(OcbmExpect.USB_PERMISSION, REQUEST_INTERVAL_MS,
                            "adapter 0x1314:0x2d00 is on the bus with no grant held and the permission dialog was requested — until it is accepted nothing in the app can proceed")
                    } else {
                        log.w("still no USB permission after ${(now - startedAt) / 1000}s — re-raising the dialog (#$requests); the app is idle until it is accepted")
                        SessionTrace.Board.down(OcbmBoard.USB_CLAIM,
                            if (requestSentWhileBackground) "USB access needed — open gm_ccpa to see the prompt"
                            else "adapter ${dev.deviceName} present — USB permission NOT held; dialog re-raised (#$requests) — accept it on screen")
                    }
                    // We only reach here because no grant is held — neither the attach-time grant
                    // the ordinary resolver gives a launched handler nor a remembered "always open"
                    // — so this is the moment the driver gets a dialog. Recording it here rather than
                    // scraping SystemUI out of the log stream makes `prompted=` a first-hand fact.
                    // (Until 2026-09-10 this said "the silent fixed-handler grant did NOT land"; that
                    // grant path went with the package squat, reverted 2026-09-08 — UsbAttachActivity.)
                    // Object-level, not `current()?.`: with no session open yet (launched with the
                    // adapter absent, box plugged in later) this poll can raise the dialog before the
                    // trampoline's begin() runs, and `current()?.` dropped the observation on the floor.
                    zeno.gmccpa.logging.SessionSummary.permissionDialogObserved()
                    t.requestPermissionAsync(dev)
                }
            }
            polls++
            // Sliced, so [stop] is observed within ABORT_SLICE_MS rather than up to a full POLL_MS.
            var slept = 0L
            while (slept < POLL_MS && !claimAbort && !stopped && !cancelled()) {
                try { Thread.sleep(ABORT_SLICE_MS) }
                catch (_: InterruptedException) { Thread.currentThread().interrupt(); return null }
                slept += ABORT_SLICE_MS
            }
        }
        return null
    }

    private fun runAllLocked(
        subscribe: () -> Boolean,
        mayRequest: ((Int) -> Boolean)?,
        deferredWhy: () -> String,
        quietExpiry: () -> Boolean,
        cancelled: () -> Boolean,
        epochAtSubmit: Int,
    ): LinkResult {
        var r = LinkResult()
        // Sticky Stop: a call queued behind a busy verb when Stop ran must not claim anything.
        if (stopped) { sink("link attempt skipped — this OCBM probe was stopped"); return r }
        // A link came up while this call sat in the queue (2026-10-06): leave it up rather than
        // CT_STOP + re-claim + re-SUBSCRIBE it — that presence flap is what this check exists for.
        if (linkEpoch != epochAtSubmit && hasLink()) {
            val c = client
            val cur = lastLink
            return when {
                c?.subscribed == true -> {
                    log.i("link came up while this call was queued (epoch $epochAtSubmit→$linkEpoch) — leaving it up")
                    cur
                }
                c != null && !c.everSubscribed && subscribe() -> {
                    log.i("link came up unsubscribed while this call was queued (epoch $epochAtSubmit→$linkEpoch) — subscribing it in place")
                    if (!c.subscribed && (wifiSsid.isNullOrBlank() || wifiPass.isNullOrBlank())) {
                        val missing = if (wifiSsid.isNullOrBlank()) "SSID" else "passphrase"
                        log.e("REFUSING to subscribe: no hotspot $missing supplied — bring-up stops before the radio-wake edge")
                        SessionTrace.Board.failed(OcbmBoard.LINK, "CT_SUBSCRIBE refused: no hotspot $missing — supply --es ssid/--es pass or fill the hotspot fields")
                        throw IllegalStateException("refusing credential-less CT_SUBSCRIBE ($missing missing) " +
                            "— would pin 0x5703 to the box's stock credentials for the whole session")
                    }
                    c.subscribe(btOnlyConfig())
                    c.startHeartbeat()
                    startBoxLogStream()
                    sink("STEP 3 — subscribed the link that came up while this call was queued.")
                    cur.copy(subscribed = true).also { lastLink = it }
                }
                else -> {
                    log.i("link came up while this call was queued (epoch $epochAtSubmit→$linkEpoch) — leaving it as it is")
                    cur
                }
            }
        }
        sink("")
        sink("==================== OCBM LINK (real adapter) ====================")
        // Armed by UsbAttachActivity when it forwards an attach; a launcher-tap or scripted run
        // never armed it and met() ignores unknown names.
        SessionTrace.met(OcbmExpect.LINK_ATTEMPT)
        // Tear down any live session FIRST. Without this, a second run (a button press, or a USB
        // re-attach firing the intent) leaves the old read thread and heartbeat running: two readers
        // race on bulk IN and two writers interleave frames with independent seq counters on bulk
        // OUT. The box sees a corrupt stream, and on this gadget that can mean a power cycle.
        if (client != null || usb != null) {
            log.w("a session is already live — stopping it before re-claiming the device")
            // Quick on a dead link (transport gone, or no HELLO): there is nothing to pull box logs
            // over, and the full pull is 4 x 6 s of timeouts (2026-10-06, the box relink).
            stopLocked(quick = !hasLink())
        }
        val t = UsbBulkTransport(ctx, zeno.gmccpa.ProbeLog.sub("usb"))
        usb = t

        // A session with no attach behind it begins HERE. The trampoline (UsbAttachActivity) owns
        // begin() for a real attach; a launcher tap, `am start` or a task switch with the adapter
        // already plugged in reaches this point with no session open, and until 2026-09-10 such a
        // session — the whole 2026-09-09 drive, for one — ended with no SESSION line at all.
        //
        // Placement is load-bearing twice over. BEFORE awaitClaimable, so a dialog raised in the
        // wait lands on this session's `prompted=` rather than on nothing. And ONLY when the adapter
        // is already present: with it absent, the plug-in that ends the wait fires the trampoline,
        // whose begin() is the one with real attach facts — beginning here as well would only
        // manufacture a second session to be superseded. This is the single choke point for all
        // four runAll() callers, for the same reason the MGMT_INFO start snapshot below is taken
        // here and not at any of them. It never fabricates an attach fact: see beginLaunch.
        if (zeno.gmccpa.logging.SessionSummary.current() == null) {
            t.findQuiet()?.let { present ->
                zeno.gmccpa.logging.SessionSummary.beginLaunch(present)
                sink("session begun at link attempt (origin=launch): no USB attach preceded this run")
            }
        }

        val dev = awaitClaimable(t, mayRequest, deferredWhy, cancelled)
        if (dev == null) {
            // awaitClaimable re-sets the interrupt flag on its own InterruptedException path, so
            // both ways of cancelling the wait are visible here; only the deadline is a fault.
            if (claimAbort || stopped || Thread.currentThread().isInterrupted) {
                // Deliberate: the operator stopped the wait. Narration, not a fault.
                sink("link attempt ended: wait for the adapter cancelled")
            } else if (quietExpiry()) {
                // The box relink: an automatic wait, re-armed once per expiry while CarPlay is live,
                // so an ABORT here would be a false E every ten minutes.
                log.i("no claimable adapter in ${CLAIM_WAIT_MS / 60_000} min (last state: $lastClaimState) — the relink decides whether to wait again")
                SessionTrace.Board.down(OcbmBoard.USB_CLAIM, "no claimable adapter in ${CLAIM_WAIT_MS / 60_000} min — last state: $lastClaimState")
            } else {
                abort("gave up waiting for a claimable OCBM accessory after ${CLAIM_WAIT_MS / 60_000} min (last state: $lastClaimState)")
                SessionTrace.Board.failed(OcbmBoard.USB_CLAIM, "no claimable adapter in ${CLAIM_WAIT_MS / 60_000} min — last state: $lastClaimState")
            }
            return r
        }
        sink("found ${dev.deviceName} vid=0x%04x pid=0x%04x".format(dev.vendorId, dev.productId))
        if (!t.open(dev)) { abort("claim failed (${t.lastError})"); return r }
        r = r.copy(claimed = true)

        val c = OcbmClient(t, log, hostInstance)
        // Re-attach the UI observers to every new client (see their declaration above).
        c.onSessionEvent = { sev -> onSessionEvent?.invoke(sev) }
        c.onPairingCode = { code -> onPairingCode?.invoke(code) }
        // These three must be attached HERE, before c.start(), not by the caller after runAll()
        // returns: the BT/iAP2 handshake the box reports over CT_BT_PHASE runs *during* runAll, so
        // a caller wiring them afterwards would miss every phase up to WIFI_HANDOFF — precisely the
        // window the events exist to explain.
        c.onBtPhase = { p -> onBtPhase?.invoke(p) }
        c.onBoxHealth = { f -> onBoxHealth?.invoke(f) }
        c.onPhoneIdent = { j -> onPhoneIdent?.invoke(j) }
        c.onProjMode = { m -> onProjMode?.invoke(m) }
        // Box-side lines get their own sub-tag, so `logcat -s NETPROBE | grep '\[box'` still
        // separates the box's narration from ours exactly as it did under the CH_FILE poller.
        c.boxLogger = boxLog
        c.onBoxLogDropped = { src, n -> log.w("box dropped $n log lines from ${logSourceName(src)}") }
        client = c
        // If the read loop dies while we still think we are running, the device is gone. Bound to
        // this client: the loop only starts in c.start() below and the transport fires the callback
        // only while running, so no cross-generation check is needed. The HELLO latch is cleared
        // here because nothing else does — the heartbeat's own death path clears it, but a dead
        // transport that never reaches five write failures left hasLink() true (2026-10-06).
        t.onTransportDead = {
            // Null: the read loop ended on its own. Non-null: the heartbeat ended it (v5.6 H1-1).
            val why = t.deadReason
            log.e(if (why == null) "!! transport died — the read loop ended (adapter gone, or the gadget stalled)"
                  else "!! transport declared dead — $why")
            c.markLinkDead()
            SessionTrace.Board.failed(OcbmBoard.LINK,
                if (why == null) "transport died — read loop ended (adapter gone or gadget stalled); handed to the box relink"
                else "transport declared dead — $why; handed to the box relink")
            // The box-lost edge and the relink (SessionCore.onBoxTransportDead), with the dying client. Guarded: this
            // runs at the end of the read thread, and an escaping throw there kills the process.
            onLinkDead?.let { cb -> runCatching { cb(c, why) }.onFailure { log.e("onLinkDead threw: ${it.javaClass.simpleName}: ${it.message}") } }
        }
        c.start()

        if (!c.hello()) {
            abort("no CT_HELLO_ACK. The box may not be running ocbmd, or the framing is wrong.",
                "Check the box's own log: it is pullable over OCBM (captureBoxLogsNow) once a link exists, or over the console.")
            return r
        }
        r = r.copy(helloOk = true, helloAtMs = System.nanoTime() / 1_000_000L)
        linkEpoch++
        lastLink = r
        sink("STEP 1 OK — claimed the accessory and completed the OCBM handshake.")

        // Bonded-device snapshot at the top of the session. Taken HERE rather than at any of the
        // four runAll() call sites so every path that brings a link up gets one — the start-vs-end
        // diff is the whole point, and a session missing its start snapshot cannot be diffed at all.
        // The same reply carries the adapter's Bluetooth name for the launcher screen (wave 3), so it
        // is now read even when no session summary is open — one MGMT_GET_INFO, both consumers.
        val startSess = zeno.gmccpa.logging.SessionSummary.current()
        val startInfo = runCatching { c.mgmtGetInfo() }.getOrNull()
        startInfo?.let { startSess?.onMgmtInfoStart(it) }
        onMgmtInfo?.let { cb -> runCatching { cb(c, startInfo) } }

        c.setTime()

        // MFi BEFORE any BT work: it needs zero box-side changes and proves three things at once —
        // an ordinary app can claim the accessory, the framing is right, and the relay works.
        if (c.hasMfi) {
            // Every outcome below — including a TIMEOUT — reaches the session summary. Until
            // 2026-09-10 only the `!ok` branches called onMfiFailure and the null (timeout) branches
            // merely logged, so the 2026-09-09 session summarised as `mfi=none` across a 15 s sign
            // timeout: the one-line verdict said "every request succeeded" about a session whose
            // keystone step never answered.
            // Expectations carry the client's OWN budgets (the same numbers mfiCertificate/mfiSign
            // wait), so the watchdog and the poll agree on "never". A chip reply with a non-OK
            // status still MEETS the expectation — the chip answered — and is then an ERROR of its
            // own below; the expectation is for silence, the status line is for refusal.
            SessionTrace.expect(OcbmExpect.MFI_CERT, OcbmClient.MFI_CERT_TIMEOUT_MS,
                "CT_HELLO_ACK advertised CAP_MFI and copy_certificate was sent over CH_MFI — the coprocessor answers in ~160 ms (2026-09-09) when ocbmd holds the chip")
            val cert = c.mfiCertificate()
            if (cert != null) SessionTrace.met(OcbmExpect.MFI_CERT)
            if (cert == null) {
                zeno.gmccpa.logging.SessionSummary.current()?.onMfiFailure("cert: timeout")
                mfiLog.e("cert: NO RESPONSE within ${OcbmClient.MFI_CERT_TIMEOUT_MS}ms — ocbmd never relayed the coprocessor's answer")
            }
            else if (!cert.ok) {
                zeno.gmccpa.logging.SessionSummary.current()?.onMfiFailure("cert: ${cert.statusName()}")
                mfiLog.e("cert: ${cert.statusName()} — the coprocessor refused the certificate read")
            }
            else {
                mfiLog.i("cert: ${cert.payload.size} bytes  ${if (cert.payload.size == Mfi.EXPECTED_CERT_LEN) "(matches the expected 945)" else "(expected 945)"}")
                mfiLog.i("   first 16: ${hex(cert.payload, 16)}")
            }
            // A SHA-1-shaped digest; the chip signs whatever 20 bytes we hand it.
            val digest = ByteArray(Mfi.DIGEST_LEN) { (it * 7 + 3).toByte() }
            SessionTrace.expect(OcbmExpect.MFI_SIGN, OcbmClient.MFI_SIGN_TIMEOUT_MS,
                "create_signature was sent over CH_MFI" + (if (cert?.ok == true) " right after a good certificate, so chip and daemon were both alive" else " (the certificate step already failed)"))
            var sig = c.mfiSign(digest)
            if (sig != null) SessionTrace.met(OcbmExpect.MFI_SIGN)
            // What the summary's `mfi=` token says about the sign, beyond its final status. Empty
            // unless the first attempt timed out; then it records whether the re-HELLO below recovered
            // it, so a recovered session is still countable as an ocbmd-restart instance.
            var signNote = ""
            if (sig == null && cert != null && cert.ok) {
                // A sign timeout right after a GOOD cert is a link discontinuity, not a chip verdict.
                //
                // Device-observed 2026-09-09: the cert came back in 160 ms from ocbmd pid 127; the box
                // supervisor then declared that daemon wedged ("alive mtime stale >=1min" — a false
                // positive against a healthy idle daemon) and restarted it as pid 71, and our sign
                // timed out at exactly 15 s. The replacement daemon then served CT_SUBSCRIBE and every
                // later CH_MFI op without ever seeing a HELLO — `ocbmd`'s `handle()` gates neither on it
                // (verified against ccpa/ocbmd/src/main.rs HEAD 2026-09-10) — so its `host_instance`
                // stayed `None` for the whole session, which silently disables the host-replacement
                // detection the nonce exists for. The chip was fine; the daemon that had our request
                // was gone. The gadget stays CONFIGURED across an L2 restart, so the claim and the read
                // thread are still valid and a fresh CT_HELLO over the same transport is the repair.
                //
                // The re-HELLO is also the discriminator: `ocbmd` is single-threaded, so a daemon that
                // is GENUINELY wedged (blocked in the chip's I2C sequence, say) cannot ACK within
                // REHELLO_TIMEOUT_MS, and that non-answer is itself the finding — no sign retry then.
                //
                // Cost on a real chip fault (chip dead, daemon healthy): the ACK arrives in
                // milliseconds and the retry burns another full sign budget, so bring-up is at worst
                // REHELLO_TIMEOUT_MS + one mfiSign timeout (~19 s) longer than before. The retry
                // deliberately keeps the default sign budget: a shorter one would turn a slow-but-
                // working chip into a false fault, which is the exact lie this branch exists to stop.
                mfiLog.w("sign: NO RESPONSE (timeout) after a good cert — treating it as a link " +
                    "discontinuity (ocbmd restarted under us?), not a chip verdict; re-HELLO")
                // hello() arms its own HELLO_ACK expectation with REHELLO_TIMEOUT_MS.
                if (c.hello(REHELLO_TIMEOUT_MS)) {
                    mfiLog.i("re-HELLO acked — ocbmd is answering (replacement daemon now holds our nonce); retrying the sign once")
                    SessionTrace.expect(OcbmExpect.MFI_SIGN_RETRY, OcbmClient.MFI_SIGN_TIMEOUT_MS,
                        "the re-HELLO was ACKed after a sign timeout, so a live ocbmd holds the chip again and the retried create_signature should answer")
                    sig = c.mfiSign(digest)
                    if (sig != null) SessionTrace.met(OcbmExpect.MFI_SIGN_RETRY)
                    signNote = if (sig != null) ", recovered by re-HELLO" else " after re-HELLO"
                } else {
                    mfiLog.e("re-HELLO: no CT_HELLO_ACK within ${REHELLO_TIMEOUT_MS}ms — ocbmd is wedged, not restarted; no sign retry")
                    signNote = "; re-HELLO no ACK"
                }
            }
            if (sig == null) {
                zeno.gmccpa.logging.SessionSummary.current()?.onMfiFailure("sign: timeout$signNote")
                mfiLog.e("sign: NO RESPONSE (timeout$signNote) — the MFi relay is unproven; /auth-setup will fail the same way")
            }
            else if (!sig.ok) {
                zeno.gmccpa.logging.SessionSummary.current()?.onMfiFailure("sign: ${sig.statusName()}$signNote")
                mfiLog.e("sign: ${sig.statusName()}$signNote — the coprocessor refused to sign")
            }
            else {
                // A recovered timeout is still recorded: `mfi=` is the summary's only trace of an
                // ocbmd restart mid-bring-up, and the value says "recovered" in so many words.
                if (signNote.isNotEmpty()) zeno.gmccpa.logging.SessionSummary.current()?.onMfiFailure("sign: timeout$signNote")
                mfiLog.i("sign: ${sig.payload.size} bytes  ${if (sig.payload.size == Mfi.SIG_LEN) "(matches RSA-1024)" else "(expected 128)"}")
                mfiLog.i("   first 16: ${hex(sig.payload, 16)}")
                r = r.copy(mfiProven = sig.payload.size == Mfi.SIG_LEN)
                sink("STEP 2 OK — the MFi relay works. This is the keystone of the whole architecture.")
            }
        } else {
            // A fault, not a skip: the relay is the keystone, and a box without CAP_MFI cannot
            // authenticate a phone this session. hello() already said so at ERROR; this line puts
            // it in the MFi tag where the cert/sign lines are looked for.
            mfiLog.e("CH_MFI unavailable — the box did not advertise CAP_MFI; no certificate, no signature, no CarPlay auth this session")
        }

        val info = c.mgmtGetInfo()
        // Chunked on JSON element boundaries, never mid-value (E-F2): a MAC split across two lines
        // escapes the line-wise export redactor. Logcat itself stays unredacted by policy (docs/06 §8.2).
        if (info != null) { sink("  MGMT_INFO:"); MgmtInfo.logChunks(info).forEach { sink("     $it") } }
        // Degraded, not fatal: bring-up continues, but the session summary loses its bonded-device
        // diff and the box was slow on CH_MGMT for 5 s — worth a WARN, not a silent INFO.
        else log.w("MGMT_INFO: no response within 5 s — box busy on its dispatch loop? continuing without the bonded-device snapshot")

        // The radio-wake edge. Everything above is passive; this is what starts the box's radios.
        //
        // REFUSE to take this edge without credentials. This used to be a NOTE, and the note was not
        // enough: the box applies host credentials only inside `wireless_up()`
        // (session_supervisor.sh -> apply_host_wifi_creds), which runs on the host_present 0->1 edge.
        // So subscribing without them brings the wireless stack up against the box's STOCK
        // /etc/hostapd.conf, and a LATER subscribe carrying credentials does not re-apply them —
        // wireless_up has already run. The 0x5703 handoff then hands the iPhone `ccpa-b0df`, an SSID
        // that is never raised (wifi_ap:false); the phone joins nothing, and per 06 §5.4 the dead
        // network poisons the next attempt until it is forgotten on the phone. The only recovery is a
        // full host_present cycle. Device-observed 2026-08-12 — one credential-less click cost the
        // whole session. Failing here is cheap and obvious; succeeding into that state is neither.
        if (!subscribe()) {
            // CT_LOG_CTL does not require a subscription, so the box narrates its side of a
            // mid-session recovery even though we deliberately hold no presence latch.
            startBoxLogStream()
            sink("STEP 3 SKIPPED — link and CH_MFI relay restored WITHOUT CT_SUBSCRIBE.")
            sink("   A CarPlay session is live. Subscribing would take the radio-wake edge and bring")
            sink("   the box's BT stack up mid-session; CarPlay does not need BT once it is streaming.")
            sink("   Heartbeat runs as a liveness probe only (no CT_SUBSCRIBE); the box answers it with a")
            sink("   HOST_GONE nudge at most every 30 s, which this client ignores.")
            // v5.6 H1-3: until 2026-10-06 this path started no heartbeat, so nothing wrote OUT after
            // it and a second box stall went undetected. The probe never subscribes; a later
            // subscribeExisting (rung 1) finds it already running.
            c.startHeartbeat()
            lastLink = r
            return r
        }
        if (wifiSsid.isNullOrBlank() || wifiPass.isNullOrBlank()) {
            val missing = if (wifiSsid.isNullOrBlank()) "SSID" else "passphrase"
            // The headline at ERROR (the bring-up ends here and the phone will never be handed a
            // network); the explanation stays narration.
            log.e("REFUSING to subscribe: no hotspot $missing supplied — bring-up stops before the radio-wake edge")
            SessionTrace.Board.failed(OcbmBoard.LINK, "CT_SUBSCRIBE refused: no hotspot $missing — supply --es ssid/--es pass or fill the hotspot fields")
            sink("     CT_SUBSCRIBE is the radio-wake edge, and the box applies 0x5703 credentials")
            sink("     ONLY at that edge. Subscribing now would lock this session to the box's stock")
            sink("     SSID — which is never raised — and no later subscribe can undo it without a")
            sink("     full host_present cycle (app stop/start, minding the 20s flap detector).")
            sink("     Supply --es ssid/--es pass, or fill the hotspot fields, and run again.")
            throw IllegalStateException("refusing credential-less CT_SUBSCRIBE ($missing missing) " +
                "— would pin 0x5703 to the box's stock credentials for the whole session")
        }
        c.subscribe(btOnlyConfig())
        c.startHeartbeat()
        r = r.copy(subscribed = true)
        lastLink = r
        // From here the box narrates its own bring-up. Follow it, so both halves of any failure land
        // in the same logcat (and therefore the same capture bundle) instead of on separate machines.
        startBoxLogStream()
        sink("STEP 3 — subscribed. The box supervisor brings radios up from this edge.")
        sink("   Watch it live:  the box pushes its own logs over CH_LOG as [box:<source>] lines.")
        sink("   BT progress arrives as CT_BT_PHASE (SEV_PHONE_* refer to the box's own USB bus, not")
        sink("   Bluetooth). Box-side lines arrive over CH_LOG as [box:<source>]; the four /tmp files")
        sink("   CH_LOG does not carry are pulled once over CH_FILE at session end. No UART needed.")
        sink("   Leave this running; heartbeats hold the session at 1 Hz.")
        return r
    }

    // ---- session teardown -------------------------------------------------------------------------

    /**
     * Bonded phone MACs, from the `devices` array in the MGMT_INFO JSON snapshot.
     * Hand-parsed — the payload is a small hand-rolled JSON document, not worth a parser.
     */
    fun bondedDevices(): List<String> {
        val json = client?.mgmtGetInfo() ?: return emptyList()
        val i = json.indexOf("\"devices\"")
        if (i < 0) return emptyList()
        val open = json.indexOf('[', i)
        val close = json.indexOf(']', open)
        if (open < 0 || close < 0) return emptyList()
        return json.substring(open + 1, close)
            .split(',')
            .map { it.trim().trim('"') }
            .filter { it.isNotEmpty() }
    }

    /**
     * `CT_RADIO`. Serialized on [ops] like every other box verb so it cannot interleave with a
     * bring-up or a teardown mid-frame.
     */
    fun setRadios(on: Boolean): Boolean = ops.submit<Boolean> {
        val c = client ?: run { log.w("CT_RADIO with no OCBM link"); return@submit false }
        c.radio(on)
    }.get()

    /**
     * Command the phone off Bluetooth **without** destroying the bond, by bouncing the box's wireless
     * stack (`MGMT_RESTART_WIRELESS`). `btd` restarts, which closes the RFCOMM link and
     * takes the controller non-discoverable before coming back up. The phone can reconnect afterwards
     * with no re-pairing.
     *
     * This is the right lever for test hygiene between runs: it clears a half-live session without the
     * heavy-handedness of forgetting the bond.
     */
    fun disconnectPhone(): Boolean = ops.submit<Boolean> { disconnectPhoneLocked() }.get()

    private fun disconnectPhoneLocked(): Boolean {
        val c = client ?: run { log.w("no OCBM link — cannot disconnect"); return false }
        log.i(">> MGMT_RESTART_WIRELESS (drop the BT link, keep the bond)")
        val st = c.mgmtAction(Ocbm.MGMT_RESTART_WIRELESS)
        when (st) {
            null -> { log.w("   no MGMT_ACK — the box may be busy"); return false }
            0 -> log.i("   ack ok — box bounces btd (~4s), phone's BT link drops")
            else -> { log.w("   ack status=$st (error)"); return false }
        }
        // Being explicit about the half we cannot drive, so it isn't mistaken for a bug later.
        log.w("   NOTE: this drops BLUETOOTH only. The phone stays associated to the vehicle SoftAP —")
        log.w("   an unprivileged app cannot deauthenticate a hotspot client, and there is no iAP2")
        log.w("   message that tells a phone to leave a Wi-Fi network. It leaves when the CarPlay")
        log.w("   session ends or when the hotspot itself is cycled.")
        return true
    }

    /**
     * Forget the bond so the phone must pair again — `MGMT_FORGET_DEVICE` for one MAC, or
     * `MGMT_FORGET_ALL`. Definitive, and the only way to clear a phone-side record that has gone
     * stale (e.g. an identity that changed between the AP-on and AP-off roles).
     *
     * The phone keeps its own pairing record, so also remove the car under
     * Settings ▸ General ▸ CarPlay on the iPhone or the two sides disagree.
     */
    fun forgetPhone(mac: String? = null): Boolean = ops.submit<Boolean> { forgetPhoneLocked(mac) }.get()

    private fun forgetPhoneLocked(mac: String?): Boolean {
        val c = client ?: run { log.w("no OCBM link — cannot forget"); return false }
        val st = if (mac.isNullOrBlank()) {
            log.i(">> MGMT_FORGET_ALL")
            c.mgmtAction(Ocbm.MGMT_FORGET_ALL)
        } else {
            // The phone's BR/EDR MAC is never written to the log (plan v5.9 D020): its length only.
            log.i(">> MGMT_FORGET_DEVICE <mac ${mac.length} chars>")
            c.mgmtAction(Ocbm.MGMT_FORGET_DEVICE, mac.toByteArray(Charsets.US_ASCII))
        }
        return when (st) {
            null -> { log.w("   no MGMT_ACK"); false }
            0 -> { log.i("   ack ok — bond cleared, wireless restarting")
                   log.w("   ALSO forget the car on the iPhone (Settings > General > CarPlay)"); true }
            else -> { log.w("   ack status=$st (error)"); false }
        }
    }

    /** Report the current session so a run can start from a known state. */
    fun sessionState() { ops.submit { sessionStateLocked() }.get() }

    private fun sessionStateLocked() {
        val c = client ?: run { log.w("no OCBM link"); return }
        log.i("link: ${c.statsLine()}")
        log.i("last session event: ${Ocbm.sevName(c.lastSessionEvent)}")
        val bonded = bondedDevices()
        if (bonded.isEmpty()) log.i("bonded phones: none")
        else bonded.forEach { log.i("bonded phone: $it") }
    }

    /**
     * The MFi bridge handed to the native receiver core. Blocking and synchronous by contract —
     * `createSignature` is called inside MFi-SAP on the control path, with the phone waiting on an
     * HTTP reply, so no coroutine boundary may be introduced here.
     *
     * Every failure here — timeout or status — is also pushed to the session summary, prefixed
     * `relay` so it reads apart from the bring-up probe's own cert/sign. A relay timeout is the one
     * that drops the PHONE (auth-setup fails, session gone), so a summary that omitted it would say
     * `mfi=none` about exactly the session it exists to explain.
     */
    fun mfiRelay(): zeno.gmccpa.pair.MfiRelay = object : zeno.gmccpa.pair.MfiRelay {
        // Each failure is ALSO an ERROR line here, under the mfi tag. Until 2026-09-10 a relay
        // timeout reached the summary and the native caller's exception and nothing else — the one
        // MFi failure that drops the phone was the one with no line of its own in the capture.
        private fun relayFail(what: String): Nothing {
            mfiLog.e("relay $what — the phone's auth-setup / iAP2 auth will fail on this")
            throw java.io.IOException("CH_MFI $what")
        }
        override fun copyCertificate(): ByteArray {
            val c = client ?: run { mfiLog.e("relay cert requested with no OCBM link"); throw java.io.IOException("no OCBM link") }
            // The certificate is a constant of the chip. Serve the copy this client already read
            // (bring-up or an earlier relay call) instead of asking the box again: the re-read is
            // where the fast FAILED-in-2-ms verdict landed (2026-10-05, each time right after a slow
            // read had succeeded), and the phone then waited out the whole 120 s tunnel budget.
            // The relay is the ONLY consumer of the cache; bring-up and self-test always read the chip.
            c.cachedCertificate()?.let { mfiLog.i("relay cert: served from this link's cache (${it.size} B)"); return it }
            val r = c.mfiCertificate() ?: run {
                zeno.gmccpa.logging.SessionSummary.current()?.onMfiFailure("relay cert: timeout")
                relayFail("cert timeout (${OcbmClient.MFI_CERT_TIMEOUT_MS}ms)")
            }
            if (!r.ok) {
                zeno.gmccpa.logging.SessionSummary.current()?.onMfiFailure("cert: ${r.statusName()}")
                relayFail("cert: ${r.statusName()}")
            }
            return r.payload
        }
        override fun createSignature(digest: ByteArray): ByteArray {
            val c = client ?: run { mfiLog.e("relay sign requested with no OCBM link"); throw java.io.IOException("no OCBM link") }
            val r = c.mfiSign(digest) ?: run {
                zeno.gmccpa.logging.SessionSummary.current()?.onMfiFailure("relay sign: timeout")
                relayFail("sign timeout (${OcbmClient.MFI_SIGN_TIMEOUT_MS}ms)")
            }
            if (!r.ok) {
                zeno.gmccpa.logging.SessionSummary.current()?.onMfiFailure("sign: ${r.statusName()}")
                relayFail("sign: ${r.statusName()}")
            }
            return r.payload
        }
        // Short-budget variants for the iAP2 tunnel — see MfiRelay's KDoc. 4 s is empirical, not
        // derived: cert measures ~1.1 s and sign ~1.7 s, so it clears both even when one op is queued
        // behind another. The phone-side request timeout it has to fit inside is unmeasured (nearest
        // sourced figure is CarKit's disassembly-confirmed 30 s — see audit 4.8 verdict).
        override fun copyCertificateFast(): ByteArray {
            val c = client ?: run { mfiLog.e("relay cert (fast) requested with no OCBM link"); throw java.io.IOException("no OCBM link") }
            // Same cache as copyCertificate(); see there. This is the path the iAP2 tunnel takes.
            c.cachedCertificate()?.let { mfiLog.i("relay cert (fast): served from this link's cache (${it.size} B)"); return it }
            val r = c.mfiCertificate(4_000) ?: run {
                zeno.gmccpa.logging.SessionSummary.current()?.onMfiFailure("relay cert (fast): timeout")
                relayFail("cert timeout (fast, 4000ms)")
            }
            if (!r.ok) {
                zeno.gmccpa.logging.SessionSummary.current()?.onMfiFailure("relay cert (fast): ${r.statusName()}")
                // W, not relayFail's E: a refused fast read is ONE tunnel handshake attempt, and the
                // tunnel retries it — the E-level "auth will fail on this" wording overstated a
                // single refusal. The summary line above still records it.
                mfiLog.w("relay cert (fast): ${r.statusName()} — tunnel handshake attempt failed; the tunnel retries")
                throw java.io.IOException("CH_MFI cert (fast): ${r.statusName()}")
            }
            return r.payload
        }
        override fun createSignatureFast(digest: ByteArray): ByteArray {
            val c = client ?: run { mfiLog.e("relay sign (fast) requested with no OCBM link"); throw java.io.IOException("no OCBM link") }
            val r = c.mfiSign(digest, 4_000) ?: run {
                zeno.gmccpa.logging.SessionSummary.current()?.onMfiFailure("relay sign (fast): timeout")
                relayFail("sign timeout (fast, 4000ms)")
            }
            if (!r.ok) {
                zeno.gmccpa.logging.SessionSummary.current()?.onMfiFailure("relay sign (fast): ${r.statusName()}")
                relayFail("sign (fast): ${r.statusName()}")
            }
            return r.payload
        }
    }

    fun stats(): String = client?.statsLine() ?: "no client"

    /**
     * Terminal for this instance: `ops` is shut down, so no box command may be submitted afterwards.
     * Both callers (`SessionCore.stopEverything`, the `ocbm_stop` command) drop the probe and the
     * next `ocbm()` builds a fresh one; without the shutdown the `ocbm-session` thread survives every
     * Stop -> Start cycle. Shut down HERE and not in `stopLocked()`: that runs on `ops` and is also
     * reached from `runAllLocked` mid-run, which would then find the executor closed under it.
     * `shutdown()` rather than `shutdownNow()` so the teardown task is never interrupted part-way.
     */
    fun stop(quick: Boolean = false) {
        if (ops.isShutdown) return
        // Set the abort FIRST, from this thread. See [claimAbort]: the submit below queues behind
        // whatever `ops` is running, so if that is a ten-minute `awaitClaimable` this call has to be
        // what ends it -- it cannot wait its turn to ask. [stopped] is the sticky half: a wait that
        // only STARTS after this (queued behind a busy verb) clears claimAbort on entry.
        stopped = true
        claimAbort = true
        ops.submit { stopLocked(quick) }.get()
        ops.shutdown()
    }

    // ---- box log streaming ------------------------------------------------------------------------

    /**
     * Bytes of each [BOX_LOG_SNAPSHOT_FILES] entry already emitted by [captureBoxLogsNow], so a
     * second capture in the same probe lifetime (a re-claim in [runAllLocked] calls [stopLocked]
     * before the final [stop]) emits only the delta. A file that SHRANK is re-read from zero.
     *
     * # Why box logs are captured at all
     *
     * The box narrates its whole Bluetooth and Wi-Fi bring-up to files in its `/tmp`, and until now
     * NOTHING carried them to the head unit: `OcbmProbe` told you to read them "over UART", which in
     * a vehicle means not at all. So the app's capture held one half of every failure and the half
     * that explained it was on a machine nobody had. On 2026-08-28 that cost hours -- Bluetooth was
     * dead because a line discipline was never loaded, and the only place that was visible was a box
     * log the app could already have fetched over a cable it was already holding.
     *
     * `captureBoxLogsNow` (which this offsets map backs) is a one-shot snapshot pull, emitted through
     * ProbeLog so it lands in logcat with no extra wiring. Live, continuous box narration no longer
     * goes through here — see [startBoxLogStream] (CH_LOG) instead.
     */
    private val boxLogOffsets = java.util.concurrent.ConcurrentHashMap<String, Int>()

    /**
     * Arm the box's `CH_LOG` push stream.
     *
     * Replaces the CH_FILE poller this used to run (a thread issuing `FILE_PULL` over six files
     * every 5 s, tracking offsets host-side). The box now tails eleven sources itself and pushes
     * deltas, so this is one control frame instead of a thread: no polling cadence to miss events
     * between, no host-side offset bookkeeping to get wrong on rotation, per-line box timestamps,
     * explicit drop accounting, and a backfill flag that separates history from what is happening
     * now. It also stops competing with the MFi relay for `ocbmd`'s single-threaded dispatch loop.
     *
     * Idempotent. [OcbmClient.subscribe] re-arms it by itself, because the box resets the stream to
     * OFF on every teardown.
     */
    fun startBoxLogStream() {
        val c = client ?: return
        c.logCtl(true, BOX_LOG_CAP_KB)
    }

    fun stopBoxLogStream() {
        client?.logCtl(false)
    }

    /**
     * Pull the box's logs ONCE, for a session bundle.
     *
     * Called at session end so the exported capture carries the box's final state even if the
     * follower missed the last few seconds.
     */
    fun captureBoxLogsNow(perFileTimeoutMs: Long = 6_000) {
        val c = client ?: return
        for (path in BOX_LOG_SNAPSHOT_FILES) {
            val body = runCatching { c.filePull(path, perFileTimeoutMs) }
                .onFailure { boxLog.w("final box log capture $path: ${it.message}") }
                .getOrNull() ?: continue
            val seen = boxLogOffsets[path] ?: 0
            // A shorter file than last time means it rotated or was truncated: start over rather
            // than slicing from a stale offset, which would emit garbage or drop the tail silently.
            val from = if (body.size < seen) 0 else seen
            boxLogOffsets[path] = body.size
            if (body.size <= from) continue
            val name = path.substringAfterLast('/')
            String(body, from, body.size - from, Charsets.UTF_8)
                .split('\n')
                .filter { it.isNotBlank() }
                .forEach { boxLog.i("[box:$name] $it") }
        }
    }

    private fun stopLocked(quick: Boolean = false) {
        // Take a final pass FIRST: the most interesting box lines are usually the last ones, and
        // after client.stop() there is no link left to fetch them over. On the quick path the pull is
        // bounded — and skipped outright when the link is not subscribed, which is also how the
        // heartbeat reports the box dead (it clears `subscribed` on the write-failure path): four
        // file pulls against a silent box would each run to their timeout for nothing.
        when {
            !quick -> captureBoxLogsNow()
            client?.subscribed == true -> captureBoxLogsNow(QUICK_PULL_MS)
            else -> log.i("quick stop with no subscription — skipping the final box-log pull")
        }
        stopBoxLogStream()
        // Deliberate teardown: nothing this probe armed is a fault any more. Named, not cancelAll()
        // — the receiver's and the supervisor's expectations are theirs to drop.
        for (e in OcbmExpect.PROBE_OWNED) SessionTrace.cancel(e, "OCBM link stopped")
        client?.stop()
        usb?.stop()
        // Read AFTER both stops: client.stop() already stopped the transport once, and the flag is
        // sticky, so the second stop() cannot un-set what the first one learned.
        if (usb?.releaseDeferred == true) usbReleaseDeferred = true
        client = null
        usb = null
        sink("OCBM link stopped")
    }

    private fun hex(b: ByteArray, n: Int): String =
        b.take(n).joinToString(" ") { "%02x".format(it) } + if (b.size > n) " …" else ""
}
