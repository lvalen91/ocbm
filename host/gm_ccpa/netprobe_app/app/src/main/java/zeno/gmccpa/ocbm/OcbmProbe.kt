package zeno.gmccpa.ocbm

import com.carlink.ocbm.Mfi
import com.carlink.ocbm.Ocbm
import android.content.Context

/**
 * The OCBM probe section — grows NetProbe from "what can this app see on the network" into the
 * permanent instrument for the CCPA link. Each capability lands here first, observable and
 * isolated, before it becomes product code.
 *
 * Two entry points:
 *   [selfTest]  — framing + client bring-up against a FakeTransport. No hardware, no adapter.
 *   [runAll]    — the real link: claim, HELLO, SETTIME, SUBSCRIBE, heartbeat, MFi, MGMT.
 */
class OcbmProbe(context: Context) {

    // Application context only: this object outlives any Activity that created it.
    private val ctx: Context = context.applicationContext

    private val log = zeno.gmccpa.ProbeLog.sub("ocbm")
    private val mfiLog = zeno.gmccpa.ProbeLog.sub("mfi")
    /** Box-side lines, tagged so they are greppable apart from our own. See [startBoxLogStream]. */
    private val boxLog = zeno.gmccpa.ProbeLog.sub("box")
    private val sink: (String) -> Unit = { zeno.gmccpa.ProbeLog.raw(it) }

    var client: OcbmClient? = null
        private set
    private var usb: UsbBulkTransport? = null

    /**
     * Cancels an in-flight [awaitClaimable] from OFF the `ops` thread.
     *
     * [runAll] is a blocking `ops.submit{}.get()`, and `awaitClaimable` inside it waits up to
     * [CLAIM_WAIT_MS] (ten minutes). Every UI command in MainActivity shares ONE command executor,
     * and [stop] itself was `ops.submit { stopLocked() }.get()` -- i.e. it queued behind the very
     * task it needed to cancel. Start with no adapter attached and Start, Stop, Recover, the
     * credentials screen and every `--es run` verb were dead for ten minutes with the UI still
     * saying "looking for the adapter...". Nothing interrupted the wait either: `stop()` used
     * `shutdown()`, not `shutdownNow()`, and only after the `.get()` returned.
     *
     * A flag rather than an interrupt on purpose: interrupting mid-`runAllLocked` could leave the
     * USB interface half-claimed, whereas this unwinds through the normal ABORT path.
     */
    @Volatile private var claimAbort = false

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
     * The vehicle hotspot the iPhone should be sent to by the 0x5703 handoff. The passphrase cannot
     * be read programmatically on the head unit (SecurityException on getSoftApConfiguration,
     * EACCES on hostapd.conf), so the user types it in once from the OS Hotspot GUI.
     */
    var wifiSsid: String? = null
    var wifiPass: String? = null
    var wifiChannel: String? = null

    /**
     * The CT_SUBSCRIBE config blob.
     *
     * `wireless`/`pairing`/`wifi_ap`/`wifi_*` are read by the box supervisor's raw greps;
     * `accessoryConfig` would be read by airplayd's serde. Unknown keys are silently ignored by
     * both, so sending a key the deployed box doesn't understand yet is always safe.
     */
    fun btOnlyConfig(): ByteArray {
        val sb = StringBuilder()
        sb.append("name: gm_ccpa netprobe\n")
        sb.append("version: 1\n")
        sb.append("wireless: true\n")
        sb.append("pairing: just_works\n")
        // The box is the Bluetooth radio and the MFi coprocessor only — it raises no AP of its own.
        sb.append("wifi_ap: false\n")
        wifiSsid?.takeIf { it.isNotBlank() }?.let { sb.append("wifi_ssid: ").append(it).append('\n') }
        wifiPass?.takeIf { it.isNotBlank() }?.let { sb.append("wifi_pass: ").append(it).append('\n') }
        wifiChannel?.takeIf { it.isNotBlank() }?.let { sb.append("wifi_channel: ").append(it).append('\n') }
        return sb.toString().toByteArray(Charsets.UTF_8)
    }

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
        val c = OcbmClient(fake, zeno.gmccpa.ProbeLog.silent())
        c.start()
        val helloThread = Thread { c.hello(4000) }
        helloThread.start()
        val wrote = fake.takeWritten(2000)
        check("client emits CT_HELLO first", wrote != null && wrote.size == 22 &&
            wrote[Ocbm.HDR_LEN] == Ocbm.CT_HELLO && wrote[Ocbm.HDR_LEN + 1] == Ocbm.VERSION)
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
    /**
     * What a bring-up attempt actually achieved.
     *
     * [runAll] used to return `Unit`, and every failure inside [runAllLocked] is reported with a
     * `sink("ABORT: ...")` and a bare `return` -- it throws only for a credential-less subscribe. So
     * "no adapter attached", "claim failed" and "no CT_HELLO_ACK" all returned NORMALLY, the caller's
     * `catch (t: Throwable)` never fired, and `MainActivity` went straight on to
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
    ) {
        /** One line for the status word, naming what actually happened rather than what was hoped. */
        fun failureDetail(): String = when {
            !claimed -> "no adapter claimed — is it plugged into this head unit?"
            !helloOk -> "adapter claimed but no CT_HELLO_ACK — is ocbmd running on the box?"
            else -> "linked"
        }
    }

    fun runAll(subscribe: Boolean = true): LinkResult =
        ops.submit<LinkResult> { runAllLocked(subscribe) }.get()

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
         * `CH_LOG` backfill cap, in KiB.
         *
         * Enabling the stream replays each source from offset 0 — that replay IS the backfill, and
         * it is bounded by this. 256 KiB (the box's default) is roughly a session's worth of
         * narration; larger buys older history at the cost of a burst on every subscribe, and every
         * one of those lines is tagged so it can never be mistaken for live evidence.
         */
        const val BOX_LOG_CAP_KB = 256
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
     * become a dialog storm. The reliable grant path remains an ACTION_USB_DEVICE_ATTACHED launch,
     * which grants implicitly with no dialog — this loop is what lets the app pick that up whenever
     * it happens, including a replug minutes later, instead of having to be restarted for it.
     *
     * Logging is edge-triggered: one line per state change, not per poll, so a long wait cannot
     * flood a 16 MiB ring buffer.
     */
    private fun awaitClaimable(t: UsbBulkTransport): android.hardware.usb.UsbDevice? {
        val deadline = android.os.SystemClock.elapsedRealtime() + CLAIM_WAIT_MS
        claimAbort = false
        var lastRequestAt = 0L
        var lastState = ""
        var polls = 0
        while (android.os.SystemClock.elapsedRealtime() < deadline) {
            if (claimAbort || Thread.currentThread().isInterrupted) {
                sink("giving up the wait for the adapter (stop requested)")
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
                when (state) {
                    "absent" -> sink("waiting for the OCBM accessory 0x1314:0x2d00 to appear ...")
                    "present-no-permission" -> sink("adapter present; waiting for USB permission ...")
                    else -> sink("adapter present and permission held")
                }
            }
            if (dev != null && state == "claimable") {
                if (polls > 0) sink("  (claimable after ${polls * POLL_MS / 1000}s of waiting)")
                return dev
            }
            if (dev != null) {
                val now = android.os.SystemClock.elapsedRealtime()
                if (now - lastRequestAt >= REQUEST_INTERVAL_MS) {
                    lastRequestAt = now
                    sink("  requesting USB permission (accept on the head unit; a replug also grants it)")
                    // We only reach here because the silent fixed-handler grant did NOT land, so
                    // this is the moment the driver gets a dialog. Recording it here rather than
                    // scraping SystemUI out of the log stream makes `prompted=` a first-hand fact.
                    zeno.gmccpa.logging.SessionSummary.current()?.permissionDialogObserved()
                    t.requestPermissionAsync(dev)
                }
            }
            polls++
            // Sliced, so [stop] is observed within ABORT_SLICE_MS rather than up to a full POLL_MS.
            var slept = 0L
            while (slept < POLL_MS && !claimAbort) {
                try { Thread.sleep(ABORT_SLICE_MS) }
                catch (_: InterruptedException) { Thread.currentThread().interrupt(); return null }
                slept += ABORT_SLICE_MS
            }
        }
        return null
    }

    private fun runAllLocked(subscribe: Boolean = true): LinkResult {
        var r = LinkResult()
        sink("")
        sink("==================== OCBM LINK (real adapter) ====================")
        // Tear down any live session FIRST. Without this, a second run (a button press, or a USB
        // re-attach firing the intent) leaves the old read thread and heartbeat running: two readers
        // race on bulk IN and two writers interleave frames with independent seq counters on bulk
        // OUT. The box sees a corrupt stream, and on this gadget that can mean a power cycle.
        if (client != null || usb != null) {
            log.w("a session is already live — stopping it before re-claiming the device")
            stopLocked()
        }
        val t = UsbBulkTransport(ctx, zeno.gmccpa.ProbeLog.sub("usb"))
        usb = t
        // If the read loop dies while we still think we are running, the device is gone.
        t.onTransportDead = { log.e("!! transport died — the adapter is gone or the gadget stalled") }

        val dev = awaitClaimable(t)
        if (dev == null) { sink("ABORT: gave up waiting for a claimable OCBM accessory"); return r }
        sink("found ${dev.deviceName} vid=0x%04x pid=0x%04x".format(dev.vendorId, dev.productId))
        if (!t.open(dev)) { sink("ABORT: claim failed (${t.lastError})"); return r }
        r = r.copy(claimed = true)

        val c = OcbmClient(t, log)
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
        c.start()

        if (!c.hello()) {
            sink("ABORT: no CT_HELLO_ACK. The box may not be running ocbmd, or the framing is wrong.")
            sink("       Check the box's own log: it is pullable over OCBM (see pullBoxLogs).")
            return r
        }
        r = r.copy(helloOk = true)
        sink("STEP 1 OK — claimed the accessory and completed the OCBM handshake.")

        // Bonded-device snapshot at the top of the session. Taken HERE rather than at any of the
        // four runAll() call sites so every path that brings a link up gets one — the start-vs-end
        // diff is the whole point, and a session missing its start snapshot cannot be diffed at all.
        zeno.gmccpa.logging.SessionSummary.current()?.let { sess ->
            runCatching { c.mgmtGetInfo() }.getOrNull()?.let { sess.onMgmtInfoStart(it) }
        }

        c.setTime()

        // MFi BEFORE any BT work: it needs zero box-side changes and proves three things at once —
        // an ordinary app can claim the accessory, the framing is right, and the relay works.
        if (c.hasMfi) {
            val cert = c.mfiCertificate()
            if (cert == null) mfiLog.i("cert: NO RESPONSE (timeout)")
            else if (!cert.ok) {
                zeno.gmccpa.logging.SessionSummary.current()?.onMfiFailure("cert: ${cert.statusName()}")
                mfiLog.i("cert: ${cert.statusName()}")
            }
            else {
                mfiLog.i("cert: ${cert.payload.size} bytes  ${if (cert.payload.size == Mfi.EXPECTED_CERT_LEN) "(matches the expected 945)" else "(expected 945)"}")
                mfiLog.i("   first 16: ${hex(cert.payload, 16)}")
            }
            // A SHA-1-shaped digest; the chip signs whatever 20 bytes we hand it.
            val digest = ByteArray(Mfi.DIGEST_LEN) { (it * 7 + 3).toByte() }
            val sig = c.mfiSign(digest)
            if (sig == null) mfiLog.i("sign: NO RESPONSE (timeout)")
            else if (!sig.ok) {
                zeno.gmccpa.logging.SessionSummary.current()?.onMfiFailure("sign: ${sig.statusName()}")
                mfiLog.i("sign: ${sig.statusName()}")
            }
            else {
                mfiLog.i("sign: ${sig.payload.size} bytes  ${if (sig.payload.size == Mfi.SIG_LEN) "(matches RSA-1024)" else "(expected 128)"}")
                mfiLog.i("   first 16: ${hex(sig.payload, 16)}")
                r = r.copy(mfiProven = sig.payload.size == Mfi.SIG_LEN)
                sink("STEP 2 OK — the MFi relay works. This is the keystone of the whole architecture.")
            }
        } else {
            mfiLog.w("skipping CH_MFI — the box did not advertise CAP_MFI")
        }

        val info = c.mgmtGetInfo()
        if (info != null) { sink("  MGMT_INFO:"); info.chunked(110).forEach { sink("     $it") } }
        else sink("  MGMT_INFO: no response")

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
        if (!subscribe) {
            // CT_LOG_CTL does not require a subscription, so the box narrates its side of a
            // mid-session recovery even though we deliberately hold no presence latch.
            startBoxLogStream()
            sink("STEP 3 SKIPPED — link and CH_MFI relay restored WITHOUT CT_SUBSCRIBE.")
            sink("   A CarPlay session is live. Subscribing would take the radio-wake edge and bring")
            sink("   the box's BT stack up mid-session; CarPlay does not need BT once it is streaming.")
            sink("   No heartbeat either: we are not subscribed, so the box has nothing to time out.")
            return r
        }
        if (wifiSsid.isNullOrBlank() || wifiPass.isNullOrBlank()) {
            val missing = if (wifiSsid.isNullOrBlank()) "SSID" else "passphrase"
            sink("  REFUSING to subscribe: no hotspot $missing supplied.")
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
        // From here the box narrates its own bring-up. Follow it, so both halves of any failure land
        // in the same logcat (and therefore the same capture bundle) instead of on separate machines.
        startBoxLogStream()
        sink("STEP 3 — subscribed. The box supervisor brings radios up from this edge.")
        sink("   Watch it live:  the box pushes its own logs over CH_LOG as [box:<source>] lines.")
        sink("   BT progress arrives as CT_BT_PHASE (SEV_PHONE_* refer to the box's own USB bus, not")
        sink("   Bluetooth), and the box's own /tmp logs are followed over CH_FILE — box-side lines")
        sink("   appear in this same log as [box:<file>]. Nothing here needs a UART any more.")
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
     * Command the phone off Bluetooth **without** destroying the bond, by bouncing the box's wireless
     * stack (`MGMT_RESTART_WIRELESS`). `carplay-wireless` restarts, which closes the RFCOMM link and
     * takes the controller non-discoverable before coming back up. The phone can reconnect afterwards
     * with no re-pairing.
     *
     * This is the right lever for test hygiene between runs: it clears a half-live session without the
     * heavy-handedness of forgetting the bond.
     */
    /**
     * `CT_RADIO`. Serialized on [ops] like every other box verb so it cannot interleave with a
     * bring-up or a teardown mid-frame.
     */
    fun setRadios(on: Boolean): Boolean = ops.submit<Boolean> {
        val c = client ?: run { log.w("CT_RADIO with no OCBM link"); return@submit false }
        c.radio(on)
    }.get()

    fun disconnectPhone(): Boolean = ops.submit<Boolean> { disconnectPhoneLocked() }.get()

    private fun disconnectPhoneLocked(): Boolean {
        val c = client ?: run { log.w("no OCBM link — cannot disconnect"); return false }
        log.i(">> MGMT_RESTART_WIRELESS (drop the BT link, keep the bond)")
        val st = c.mgmtAction(Ocbm.MGMT_RESTART_WIRELESS)
        when (st) {
            null -> { log.w("   no MGMT_ACK — the box may be busy"); return false }
            0 -> log.i("   ack ok — box bounces carplay-wireless (~4s), phone's BT link drops")
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
            log.i(">> MGMT_FORGET_DEVICE $mac")
            c.mgmtAction(Ocbm.MGMT_FORGET_DEVICE, mac.toByteArray(Charsets.US_ASCII))
        }
        return when (st) {
            null -> { log.w("   no MGMT_ACK"); false }
            0 -> { log.i("   ack ok — bond cleared, wireless restarting");
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
     */
    fun mfiRelay(): zeno.gmccpa.pair.MfiRelay = object : zeno.gmccpa.pair.MfiRelay {
        override fun copyCertificate(): ByteArray {
            val c = client ?: throw java.io.IOException("no OCBM link")
            val r = c.mfiCertificate() ?: throw java.io.IOException("CH_MFI cert timeout")
            if (!r.ok) {
                zeno.gmccpa.logging.SessionSummary.current()?.onMfiFailure("cert: ${r.statusName()}")
                throw java.io.IOException("CH_MFI cert: ${r.statusName()}")
            }
            return r.payload
        }
        override fun createSignature(digest: ByteArray): ByteArray {
            val c = client ?: throw java.io.IOException("no OCBM link")
            val r = c.mfiSign(digest) ?: throw java.io.IOException("CH_MFI sign timeout")
            if (!r.ok) {
                zeno.gmccpa.logging.SessionSummary.current()?.onMfiFailure("sign: ${r.statusName()}")
                throw java.io.IOException("CH_MFI sign: ${r.statusName()}")
            }
            return r.payload
        }
        // Short-budget variants for the iAP2 tunnel — see MfiRelay's KDoc. 4 s is empirical, not
        // derived: cert measures ~1.1 s and sign ~1.7 s, so it clears both even when one op is queued
        // behind another. The phone-side request timeout it has to fit inside is unmeasured (nearest
        // sourced figure is CarKit's disassembly-confirmed 30 s — see audit 4.8 verdict).
        override fun copyCertificateFast(): ByteArray {
            val c = client ?: throw java.io.IOException("no OCBM link")
            val r = c.mfiCertificate(4_000) ?: throw java.io.IOException("CH_MFI cert timeout (fast)")
            if (!r.ok) throw java.io.IOException("CH_MFI cert: ${r.statusName()}")
            return r.payload
        }
        override fun createSignatureFast(digest: ByteArray): ByteArray {
            val c = client ?: throw java.io.IOException("no OCBM link")
            val r = c.mfiSign(digest, 4_000) ?: throw java.io.IOException("CH_MFI sign timeout (fast)")
            if (!r.ok) throw java.io.IOException("CH_MFI sign: ${r.statusName()}")
            return r.payload
        }
    }

    fun stats(): String = client?.statsLine() ?: "no client"

    /**
     * Terminal for this instance: `ops` is shut down, so no box command may be submitted afterwards.
     * Both callers (`MainActivity.stopEverything`, the `ocbm_stop` command) drop the probe and the
     * next `ocbm()` builds a fresh one; without the shutdown the `ocbm-session` thread survives every
     * Stop -> Start cycle. Shut down HERE and not in `stopLocked()`: that runs on `ops` and is also
     * reached from `runAllLocked` mid-run, which would then find the executor closed under it.
     * `shutdown()` rather than `shutdownNow()` so the teardown task is never interrupted part-way.
     */
    fun stop() {
        if (ops.isShutdown) return
        // Set the abort FIRST, from this thread. See [claimAbort]: the submit below queues behind
        // whatever `ops` is running, so if that is a ten-minute `awaitClaimable` this call has to be
        // what ends it -- it cannot wait its turn to ask.
        claimAbort = true
        ops.submit { stopLocked() }.get()
        ops.shutdown()
    }

    // ---- box log streaming ------------------------------------------------------------------------

    /**
     * The box's own logs, followed and re-emitted into [zeno.gmccpa.ProbeLog].
     *
     * # Why
     *
     * The box narrates its whole Bluetooth and Wi-Fi bring-up to files in its `/tmp`, and until now
     * NOTHING carried them to the head unit: `OcbmProbe` told you to read them "over UART", which in
     * a vehicle means not at all. So the app's capture held one half of every failure and the half
     * that explained it was on a machine nobody had. On 2026-08-28 that cost hours -- Bluetooth was
     * dead because a line discipline was never loaded, and the only place that was visible was a box
     * log the app could already have fetched over a cable it was already holding.
     *
     * Everything emitted here goes through ProbeLog, so it lands in logcat and is therefore picked up
     * by the always-on capture with no extra wiring: one bundle, both sides of the story.
     *
     * # Shape
     *
     * A poller, not a true stream -- CH_FILE has no follow mode, so this pulls each file and emits
     * only the bytes past what it has already seen. That is enough: these are low-rate event logs,
     * not a data plane. Files that do not exist are skipped silently (a box that never ran wireless
     * has no `wl.log`), and a file that SHRANK is treated as rotated/truncated and re-read from zero.
     *
     * Deliberately conservative about the link: pulls stop while unsubscribed, and the interval is
     * seconds rather than milliseconds, because `ocbmd` is single-threaded and a file pull shares
     * that loop with the MFi relay and the heartbeat.
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
    fun captureBoxLogsNow() {
        val c = client ?: return
        for (path in BOX_LOG_SNAPSHOT_FILES) {
            val body = runCatching { c.filePull(path, 6_000) }
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

    private fun stopLocked() {
        // Take a final pass FIRST: the most interesting box lines are usually the last ones, and
        // after client.stop() there is no link left to fetch them over.
        captureBoxLogsNow()
        stopBoxLogStream()
        client?.stop()
        usb?.stop()
        client = null
        usb = null
        sink("OCBM link stopped")
    }

    private fun hex(b: ByteArray, n: Int): String =
        b.take(n).joinToString(" ") { "%02x".format(it) } + if (b.size > n) " …" else ""
}
