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

    var client: OcbmClient? = null
        private set
    private var usb: UsbBulkTransport? = null

    /**
     * The CT_HELLO nonce every client this probe builds identifies itself with. Minted ONCE per
     * probe: a USB re-attach reuses the probe (`MainActivity.ocbm()`), so the box sees the same host
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

    /** The last state [awaitClaimable] observed, so a ten-minute give-up can say what it was waiting on. */
    @Volatile private var lastClaimState = ""

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
     * `accessoryConfig` would be read by carplayd's serde. Unknown keys are silently ignored by
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
     * become a dialog storm. The reliable grant path remains an ACTION_USB_DEVICE_ATTACHED launch,
     * which grants implicitly with no dialog — this loop is what lets the app pick that up whenever
     * it happens, including a replug minutes later, instead of having to be restarted for it.
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
    private fun awaitClaimable(t: UsbBulkTransport): android.hardware.usb.UsbDevice? {
        val deadline = android.os.SystemClock.elapsedRealtime() + CLAIM_WAIT_MS
        val startedAt = android.os.SystemClock.elapsedRealtime()
        claimAbort = false
        var lastRequestAt = 0L
        var lastState = ""
        var polls = 0
        var requests = 0
        lastClaimState = "not started"
        while (android.os.SystemClock.elapsedRealtime() < deadline) {
            if (claimAbort || Thread.currentThread().isInterrupted) {
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
                if (now - lastRequestAt >= REQUEST_INTERVAL_MS) {
                    lastRequestAt = now
                    requests++
                    if (requests == 1) {
                        sink("  requesting USB permission (accept on the head unit; a replug also grants it)")
                        SessionTrace.expect(OcbmExpect.USB_PERMISSION, REQUEST_INTERVAL_MS,
                            "adapter 0x1314:0x2d00 is on the bus with no grant held and the permission dialog was requested — until it is accepted (or a replug grants it) nothing in the app can proceed")
                    } else {
                        log.w("still no USB permission after ${(now - startedAt) / 1000}s — re-raising the dialog (#$requests); the app is idle until it is accepted or the adapter is replugged")
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
        // Armed by UsbAttachActivity when it forwards an attach; a launcher-tap or scripted run
        // never armed it and met() ignores unknown names.
        SessionTrace.met(OcbmExpect.LINK_ATTEMPT)
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
        t.onTransportDead = {
            log.e("!! transport died — the adapter is gone or the gadget stalled")
            SessionTrace.Board.failed(OcbmBoard.LINK, "transport died — adapter gone or gadget stalled; the heartbeat will declare the link dead within 5 s")
        }

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

        val dev = awaitClaimable(t)
        if (dev == null) {
            // awaitClaimable re-sets the interrupt flag on its own InterruptedException path, so
            // both ways of cancelling the wait are visible here; only the deadline is a fault.
            if (claimAbort || Thread.currentThread().isInterrupted) {
                // Deliberate: the operator stopped the wait. Narration, not a fault.
                sink("link attempt ended: wait for the adapter cancelled")
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
        c.start()

        if (!c.hello()) {
            abort("no CT_HELLO_ACK. The box may not be running ocbmd, or the framing is wrong.",
                "Check the box's own log: it is pullable over OCBM (captureBoxLogsNow) once a link exists, or over the console.")
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
        if (info != null) { sink("  MGMT_INFO:"); info.chunked(110).forEach { sink("     $it") } }
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
            log.i(">> MGMT_FORGET_DEVICE $mac")
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
            val r = c.mfiCertificate(4_000) ?: run {
                zeno.gmccpa.logging.SessionSummary.current()?.onMfiFailure("relay cert (fast): timeout")
                relayFail("cert timeout (fast, 4000ms)")
            }
            if (!r.ok) {
                zeno.gmccpa.logging.SessionSummary.current()?.onMfiFailure("relay cert (fast): ${r.statusName()}")
                relayFail("cert (fast): ${r.statusName()}")
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
     * Both callers (`MainActivity.stopEverything`, the `ocbm_stop` command) drop the probe and the
     * next `ocbm()` builds a fresh one; without the shutdown the `ocbm-session` thread survives every
     * Stop -> Start cycle. Shut down HERE and not in `stopLocked()`: that runs on `ops` and is also
     * reached from `runAllLocked` mid-run, which would then find the executor closed under it.
     * `shutdown()` rather than `shutdownNow()` so the teardown task is never interrupted part-way.
     */
    fun stop(quick: Boolean = false) {
        if (ops.isShutdown) return
        // Set the abort FIRST, from this thread. See [claimAbort]: the submit below queues behind
        // whatever `ops` is running, so if that is a ten-minute `awaitClaimable` this call has to be
        // what ends it -- it cannot wait its turn to ask.
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
