package zeno.gmccpa.ocbm

import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import zeno.gmccpa.logging.SessionTrace

/**
 * The OCBM host client — the Kotlin equivalent of `ocbm-host`'s `Link` and the macOS
 * `OCBMClient.swift`, driving any [RawBulkTransport].
 *
 * Bring-up order, which is load-bearing:
 *
 *   1. CT_HELLO, retransmitted until CT_HELLO_ACK arrives. Every other frame is DISCARDED while
 *      waiting — a prior session can leave kernel-buffered A/V frames queued ahead of the ACK.
 *   2. CT_SETTIME immediately. The box has no RTC battery, so its clock is bogus at every boot and
 *      CarPlay's TLS pairing needs a real one.
 *   3. CT_SUBSCRIBE with the config blob. This is the presence latch: `/tmp/host_present` goes to 1
 *      and the box supervisor brings the radios up from that edge. THE APP IS THE IGNITION.
 *   4. CT_HEARTBEAT at 1 Hz. Miss the box's 10 s watchdog and it emits SEV_HOST_GONE, clears its
 *      own `subscribed` flag, and no heartbeat restores presence until a fresh SUBSCRIBE (it answers
 *      an unsubscribed host's heartbeat only with a HOST_GONE nudge every 30 s).
 *
 * ## What the box is expected to say back, and by when (2026-09-10)
 *
 * Every step above that waits for the box arms a [SessionTrace] expectation with the SAME budget the
 * waiting code already uses, so a step that never answers is an `!! EXPECTED-MISSING` line at ERROR
 * instead of a `return false` the caller may or may not narrate:
 *
 *  - [OcbmExpect.HELLO_ACK] in [hello] — the caller's `timeoutMs` (20 s default, 4 s on the
 *    post-sign-timeout re-HELLO).
 *  - [OcbmExpect.SETTIME_ACK] in [setTime] — its `timeoutMs` (2 s; 1 s inside [subscribe]).
 *  - [OcbmExpect.HOST_PRESENT] in [subscribe] — [BOX_REPLY_BUDGET_MS]: `ocbmd` raises presence and
 *    answers `SEV_HOST_PRESENT` synchronously (2 ms device-observed 2026-09-09).
 *  - [OcbmExpect.HCI_PRESENT] in [subscribe] — [BT_ATTACH_BUDGET_MS]: SUBSCRIBE is the radio-wake
 *    edge, and the first `CT_BOX_HEALTH` carrying `BH_HCI_PRESENT` is the proof that the box's BT
 *    controller actually attached. Its absence is "Bluetooth does nothing" (docs/wireless/01).
 *  - [OcbmExpect.LOG_FIRST_LINE] in [logCtl] — [BOX_REPLY_BUDGET_MS]: arming CH_LOG replays every
 *    source from offset 0, so the first `[box:*]` line follows within milliseconds. This is the
 *    absence [reArmAfterMgmt] was written for: "no box lines" is otherwise indistinguishable from
 *    "the box has nothing to say".
 *
 * The same transitions keep the [SessionTrace.Board] current under the [OcbmBoard] names, so a
 * `## STATUS` block at any offset shows the link, the heartbeat, the log stream, the radio inhibit,
 * the last decoded box health, the BT phase, the identified phone and the pairing code. Updates
 * are on TRANSITIONS only — never per frame in [handleLog], never per tick in the heartbeat.
 *
 * @param trace false disables every [SessionTrace] call. `OcbmProbe.selfTest` drives this class
 *   against a scripted `FakeTransport` that deliberately answers `SEV_HOST_GONE`; with tracing on,
 *   that would print a FAILED link and, ten seconds after the test ended, a missing HOST_PRESENT
 *   for a box that never existed. A silent logger already says the test wants no side effects.
 */
class OcbmClient(
    private val transport: RawBulkTransport,
    private val log: zeno.gmccpa.ProbeLog.Logger,
    /** The CT_HELLO nonce this client identifies itself with. Owned by the probe — see [newHostInstance]. */
    private val hostInstance: Int,
    private val trace: Boolean = true,
) {
    private val reasm = Reassembler()
    private val seq = AtomicInteger(0)
    private val running = AtomicBoolean(false)

    private val ctrlQ = LinkedBlockingQueue<ByteArray>()
    private val mfiQ = LinkedBlockingQueue<ByteArray>()
    private val mgmtQ = LinkedBlockingQueue<ByteArray>()
    private val fileQ = LinkedBlockingQueue<ByteArray>()

    @Volatile var helloAcked = false; private set
    @Volatile var caps = 0; private set
    @Volatile var activeMode: Byte = Ocbm.MODE_PROJECTION; private set
    @Volatile var subscribed = false; private set
    @Volatile var lastSessionEvent: Byte = 0; private set
    /** Last CT_BT_PHASE seen this session. Advisory/monotonic-ish — see [Ocbm.btpName]. */
    @Volatile var lastBtPhase: Byte = Ocbm.BTP_IDLE; private set
    /**
     * Whether [lastBtPhase] arrived with envelope bit2 ([Ocbm.F_REPLAY]) — i.e. the box replayed a
     * latched mirror value to a fresh subscriber rather than reporting a change.
     *
     * Carried as state rather than as a second [onBtPhase] parameter because the observers are held
     * one hop away on [OcbmProbe] (the client is rebuilt on every re-claim), and widening the lambda
     * would widen that hop too. Read it from inside an `onBtPhase` handler: dispatch is inline on the
     * single `ocbm-read` thread, so it is the flag for the phase being delivered.
     */
    @Volatile var lastBtPhaseReplay = false; private set
    /** Last `CT_BOX_HEALTH` bitmask the box pushed. -1 until it has said anything. */
    @Volatile var lastBoxHealth: Int = -1; private set
    /** Last CT_PROJ_MODE seen this session. Advisory — see [Ocbm.pmName]. */
    @Volatile var lastProjMode: Byte = Ocbm.PM_NONE; private set
    /** Last CT_PHONE_IDENT JSON seen this session, or "" when cleared / not yet identified. */
    @Volatile var lastPhoneIdent: String = ""; private set
    @Volatile var framesIn = 0L; private set
    @Volatile var bytesIn = 0L; private set

    val hasMfi: Boolean get() = (caps and Ocbm.CAP_MFI) != 0

    /** Config blob last sent with CT_SUBSCRIBE; re-sent verbatim if the box drops us. */
    @Volatile private var configBlob: ByteArray = ByteArray(0)

    private var heartbeatThread: Thread? = null

    /** Optional observer for session events, so the UI can react to HOST_GONE / PHONE_* live. */
    var onSessionEvent: ((Byte) -> Unit)? = null

    /**
     * Optional observer for the box's pairing code. Empty string means "cleared". This is the one
     * value that MUST reach a human — it is matched against a prompt on the iPhone, and until it is
     * on screen the only place it exists is a logcat line nobody is watching from the driver's seat.
     */
    var onPairingCode: ((String) -> Unit)? = null

    /**
     * Bluetooth/iAP2 handshake progress (CT_BT_PHASE). The only signal the host has for the whole
     * BT phase — the box owns the radio, so without this the app can only say "waiting" from
     * subscribe until video appears. Advisory: treat an unknown value as progress, never gate on
     * ordering.
     */
    var onBtPhase: ((Byte) -> Unit)? = null
    /** `CT_BOX_HEALTH` — the box's own readiness bitmask, pushed on change. */
    var onBoxHealth: ((Int) -> Unit)? = null

    /** The connected phone's identity as a raw JSON string, or "" when cleared (CT_PHONE_IDENT). */
    var onPhoneIdent: ((String) -> Unit)? = null

    /**
     * Which projection transport owns the box (CT_PROJ_MODE). Advisory: an unknown value means
     * "some transport owns the box" — never gate on ordering.
     */
    var onProjMode: ((Byte) -> Unit)? = null

    /**
     * One decoded box log line off `CH_LOG`. [source] indexes [logSourceName]; [backfill] marks
     * a line that was already on disk when the box's tailer opened the file, so it is history, not
     * evidence of anything happening now. [unixMs] is the box's own stamp and is bogus until our
     * CT_SETTIME has landed.
     */
    var onBoxLog: ((source: Int, text: String, backfill: Boolean, truncated: Boolean, unixMs: Long) -> Unit)? = null
    /** Box reported it discarded [lines] log lines from [source] — its 64 KiB queue overflowed. */
    var onBoxLogDropped: ((source: Int, lines: Long) -> Unit)? = null

    /**
     * Desired CH_LOG state, re-applied after every SUBSCRIBE.
     *
     * The box resets the log stream to OFF on teardown and on host loss, so "armed once" is not a
     * state that survives — [subscribe] re-sends it, which is why this is remembered here rather
     * than left to the caller.
     */
    @Volatile private var logStreamWanted = false
    @Volatile private var logCapKb = Ocbm.LOG_CAP_DEFAULT_KB

    /**
     * Desired `CT_RADIO` inhibit, re-applied after every SUBSCRIBE.
     *
     * CT_SUBSCRIBE deletes the box's `/tmp/radio_off` unconditionally (`ocbmd` main.rs, the
     * `raise_presence` path), so an inhibit asserted before a re-subscribe is silently lost. That
     * matters for exactly one case and it is the important one: holding the box's radios down under
     * a live Wi-Fi session after the box came back.
     */
    @Volatile private var radioInhibited = false

    /**
     * CH_LOG entry counter, channel-wide.
     *
     * NOT per source: device-observed 2026-09-08 against box HEAD, one ascending run spans
     * bt -> radio_bt_attach -> wl -> box. A per-source counter reports a false gap on every source
     * switch. It wraps; a real jump means the box dropped entries we will never see.
     */
    @Volatile private var lastLogSeq = -1

    /**
     * Whether a `CH_LOG` entry has arrived since the stream was last armed by [logCtl]. The
     * false->true edge is the one place [handleLog] touches the trace — per-entry work there is
     * exactly what the "no per-frame logging" rule forbids.
     */
    @Volatile private var logFlowing = false

    /**
     * Where the box's BT controller stands since the last SUBSCRIBE: [HCI_PENDING] until the first
     * `CT_BOX_HEALTH` with `BH_HCI_PRESENT`, [HCI_SEEN] after it, [HCI_MISSED] once
     * [BT_ATTACH_BUDGET_MS] elapsed without one. The heartbeat tick promotes PENDING to MISSED at
     * [hciDeadlineMs] so the Board shows the failure even if the box never pushes another health
     * report — health is pushed on CHANGE, and a controller that never attaches never changes.
     */
    @Volatile private var hciState = HCI_PENDING
    @Volatile private var hciDeadlineMs = 0L

    /**
     * Where decoded `CH_LOG` lines go. Defaults to the client's own logger; [OcbmProbe] points it at
     * the `box` sub-tag so box-side lines stay greppable apart from ours, as they were under the old
     * CH_FILE poller.
     */
    var boxLogger: zeno.gmccpa.ProbeLog.Logger? = null
    private val blog: zeno.gmccpa.ProbeLog.Logger get() = boxLogger ?: log

    // ---- trace helpers (all no-ops when `trace` is false) ------------------------------------------

    /** Board names this client has written, so teardown marks DOWN only what it ever marked. */
    private val boardTouched = java.util.concurrent.ConcurrentHashMap.newKeySet<String>()

    private fun expect(what: String, withinMs: Long, because: String) {
        if (trace) SessionTrace.expect(what, withinMs, because)
    }
    private fun met(what: String) { if (trace) SessionTrace.met(what) }
    private fun cancelExpect(what: String, why: String) { if (trace) SessionTrace.cancel(what, why) }
    private fun boardUp(name: String, detail: String) {
        if (!trace) return
        boardTouched.add(name); SessionTrace.Board.up(name, detail)
    }
    private fun boardDown(name: String, why: String) {
        if (!trace) return
        boardTouched.add(name); SessionTrace.Board.down(name, why)
    }
    private fun boardFailed(name: String, why: String) {
        if (!trace) return
        boardTouched.add(name); SessionTrace.Board.failed(name, why)
    }

    /** The link's standing detail: what the box advertised, plus the phase [state] we are in. */
    private fun linkDetail(state: String): String =
        "$state caps=[${Ocbm.capsString(caps)}] mode=${if (activeMode == Ocbm.MODE_CONSOLE) "CONSOLE" else "PROJECTION"}"

    // ---- wire helpers ---------------------------------------------------------------------------

    private fun send(channel: Int, payload: ByteArray): Boolean =
        transport.writeBulk(Framing.frame(channel, Ocbm.F_BOTH, seq.getAndIncrement(), payload))

    fun start() {
        if (!running.compareAndSet(false, true)) return
        transport.setReadHandler { data, len ->
            // A stopped client speaks to nobody — the same fence CarPlayRx.stopped provides. The
            // transport's read loop checks its own flag only BEFORE bulkTransfer, so a transfer that
            // was parked when stop() ran still delivers one buffer when it finally returns. On a
            // stalled gadget that return can come seconds later, after Restart Session has built a
            // replacement client, and the OLD client would then dispatch a stale SEV_HOST_GONE or
            // HOST_PRESENT through observers that now describe the new link.
            if (!running.get()) return@setReadHandler
            bytesIn += len
            reasm.push(data, len)
            while (true) {
                val f = reasm.next() ?: break
                framesIn++
                dispatch(f)
            }
        }
        transport.start()
    }

    private fun dispatch(f: OcbmFrame) {
        when (f.channel) {
            Ocbm.CH_CTRL -> handleCtrl(f.payload, (f.flags.toInt() and Ocbm.F_REPLAY.toInt()) != 0)
            Ocbm.CH_MFI -> mfiQ.offer(f.payload)
            Ocbm.CH_MGMT -> mgmtQ.offer(f.payload)
            Ocbm.CH_FILE -> fileQ.offer(f.payload)
            Ocbm.CH_LOG -> handleLog(f.payload)
            else -> {
                // In this design the A/V channels must stay silent — the box never spawns its A/V
                // layer in the bridge role. Anything here is a finding worth reporting.
                log.w("<< unexpected frame on ${Ocbm.channelName(f.channel)} (${f.payload.size}B)")
            }
        }
    }

    /** [replay] is envelope bit2: this is a mirror value re-emitted on SUBSCRIBE, not a change. */
    private fun handleCtrl(pl: ByteArray, replay: Boolean) {
        if (pl.isEmpty()) return
        when (pl[0]) {
            Ocbm.CT_HELLO_ACK -> {
                if (pl.size >= 6) {
                    caps = (pl[2].toInt() and 0xFF) or ((pl[3].toInt() and 0xFF) shl 8) or
                        ((pl[4].toInt() and 0xFF) shl 16) or ((pl[5].toInt() and 0xFF) shl 24)
                    if (pl.size >= 7) activeMode = pl[6]
                    helloAcked = true
                    ctrlQ.offer(pl)
                }
            }
            Ocbm.CT_SETTIME -> ctrlQ.offer(pl)
            Ocbm.CT_SESSION_EVENT -> {
                if (pl.size >= 2) {
                    val sev = pl[1]
                    lastSessionEvent = sev
                    log.i("<< SESSION_EVENT ${Ocbm.sevName(sev)}")
                    if (sev == Ocbm.SEV_HOST_PRESENT) {
                        met(OcbmExpect.HOST_PRESENT)
                        boardUp(OcbmBoard.LINK, linkDetail("subscribed, box confirms HOST_PRESENT"))
                    }
                    // The box has cleared its own subscribed flag and will ignore our heartbeats
                    // until a fresh SUBSCRIBE. Re-arm or the session hangs forever.
                    if (sev == Ocbm.SEV_HOST_GONE) {
                        subscribed = false
                        // Since 2026-09-05 the box also answers a heartbeat from an UNSUBSCRIBED
                        // host with HOST_GONE once every 30 s, as a "re-SUBSCRIBE now" nudge. That
                        // is not a drop, and on the deliberate no-subscribe relink path (a box that
                        // came back under a live Wi-Fi session) re-subscribing is exactly the wrong
                        // answer — it would clear the radio inhibit and re-latch presence.
                        if (configBlob.isEmpty()) {
                            log.i("<< HOST_GONE nudge (we hold no subscription by choice) — ignoring")
                        } else {
                            // A fault, not a warning: the box's 10 s watchdog expired on us, or
                            // ocbmd restarted underneath the session. Board.failed carries the E
                            // line, and the entry stays FAILED in every status block until the
                            // re-subscribe below is answered with HOST_PRESENT.
                            boardFailed(OcbmBoard.LINK, "box reported HOST_GONE while we hold a subscription (its ${Ocbm.HEARTBEAT_GRACE_MS}ms watchdog expired, or ocbmd restarted) — re-subscribing on the next heartbeat tick")
                        }
                    }
                    onSessionEvent?.invoke(sev)
                }
            }
            Ocbm.CT_UPLINK -> {
                if (pl.size >= 7) {
                    val state = pl[1].toInt() and 0xFF
                    val rate = (pl[2].toInt() and 0xFF) or ((pl[3].toInt() and 0xFF) shl 8) or
                        ((pl[4].toInt() and 0xFF) shl 16) or ((pl[5].toInt() and 0xFF) shl 24)
                    val ch = pl[6].toInt() and 0xFF
                    log.i("<< UPLINK state=$state rate=$rate ch=$ch (mic gate)")
                }
            }
            Ocbm.CT_PAIRING_CODE -> {
                val code = if (pl.size > 1) String(pl, 1, pl.size - 1, Charsets.US_ASCII).trim() else ""
                log.i(if (code.isEmpty()) "<< PAIRING_CODE cleared" else "<< PAIRING_CODE $code  <-- match this on the iPhone")
                if (code.isEmpty()) boardDown(OcbmBoard.PAIRING_CODE, "cleared by box")
                else boardUp(OcbmBoard.PAIRING_CODE, "$code — must match the iPhone's prompt")
                onPairingCode?.invoke(code)
            }
            Ocbm.CT_BOX_HEALTH -> {
                if (pl.size >= 2) {
                    val f = pl[1].toInt() and 0xFF
                    val prev = lastBoxHealth
                    lastBoxHealth = f
                    log.i("<< BOX_HEALTH 0x%02x [%s]".format(f, Ocbm.bhString(f)))
                    noteBoxHealth(prev, f)
                    onBoxHealth?.invoke(f)
                }
            }
            Ocbm.CT_BT_PHASE -> {
                if (pl.size >= 2) {
                    val p = pl[1]
                    val prev = lastBtPhase
                    lastBtPhase = p
                    lastBtPhaseReplay = replay
                    log.i("<< BT_PHASE ${Ocbm.btpName(p)}${if (replay) " (replay)" else ""}")
                    noteBtPhase(prev, p, replay)
                    onBtPhase?.invoke(p)
                }
            }
            Ocbm.CT_PHONE_IDENT -> {
                val json = if (pl.size > 1) String(pl, 1, pl.size - 1, Charsets.UTF_8).trim() else ""
                lastPhoneIdent = json
                log.i(if (json.isEmpty()) "<< PHONE_IDENT cleared" else "<< PHONE_IDENT $json")
                if (json.isEmpty()) boardDown(OcbmBoard.PHONE, "cleared by box — no phone identified")
                else boardUp(OcbmBoard.PHONE, phoneSummary(json))
                onPhoneIdent?.invoke(json)
            }
            Ocbm.CT_PROJ_MODE -> {
                if (pl.size >= 2) {
                    val m = pl[1]
                    lastProjMode = m
                    log.i("<< PROJ_MODE ${Ocbm.pmName(m)}")
                    boardUp(OcbmBoard.PROJ_MODE, Ocbm.pmName(m))
                    onProjMode?.invoke(m)
                }
            }
            else -> log.i("<< CTRL unknown type 0x%02x (${pl.size}B)".format(pl[0]))
        }
    }

    /**
     * Keep the Board's box-health entry honest about `BH_HCI_PRESENT`, the one bit that means
     * "Bluetooth can work at all". Three outcomes, on transitions only:
     *
     *  - HCI set: UP with the decoded mask; the first such report also meets [OcbmExpect.HCI_PRESENT].
     *  - HCI clear after it WAS set: FAILED — the controller went away (a radio cycle, or a crash).
     *  - HCI clear and never set: UP-but-pending while [BT_ATTACH_BUDGET_MS] is still running (the
     *    box's first health tick reliably lands before the controller attaches — SessionSupervisor
     *    treats that first sample as a baseline for the same reason), FAILED once the budget is gone.
     *    `0x50` with bit 0 clear is the exact signature of a missing radio HAL script or a failed
     *    `hciattach` (docs/wireless/01_BT_AND_RADIO.md), and the box's `[box:bt]` lines say which.
     */
    private fun noteBoxHealth(prev: Int, f: Int) {
        val detail = "0x%02x [%s]".format(f, Ocbm.bhString(f))
        val hci = (f and Ocbm.BH_HCI_PRESENT) != 0
        when {
            hci -> {
                if (hciState != HCI_SEEN) { hciState = HCI_SEEN; hciDeadlineMs = 0L; met(OcbmExpect.HCI_PRESENT) }
                boardUp(OcbmBoard.HEALTH, detail)
            }
            prev >= 0 && (prev and Ocbm.BH_HCI_PRESENT) != 0 ->
                boardFailed(OcbmBoard.HEALTH, "$detail — hci0 was present and is now GONE (controller dropped, or the radios were taken down)")
            hciState == HCI_MISSED ->
                boardFailed(OcbmBoard.HEALTH, "$detail — still no hci0: the box's BT controller never attached (radio HAL / hciattach — read the [box:bt] lines)")
            else ->
                boardUp(OcbmBoard.HEALTH, "$detail — hci0 not yet attached (BT bring-up in progress, ${((hciDeadlineMs - System.currentTimeMillis()).coerceAtLeast(0)) / 1000}s of budget left)")
        }
    }

    /**
     * The pairing state, readable at any offset. `PAIR_REJECTED` is a FAILED entry; anything else is
     * UP with the phase name. A phase moving BACKWARDS while the box reports no projection session
     * (`CT_PROJ_MODE` NONE) is logged at WARN: the BT/iAP2 handshake reset before a session formed,
     * which used to be silent — the phase lines all read as progress in isolation. Once a projection
     * session exists (the box sends `PROJ_MODE WIRELESS_CP` together with `WIFI_HANDOFF`,
     * device-observed 2026-09-09), a later drop to IDLE is the phone leaving Bluetooth behind, not a
     * regression, and SessionSupervisor already ignores it for the same reason. Replayed mirror
     * values are excluded — they are the box re-reading its latch to a fresh subscriber.
     */
    private fun noteBtPhase(prev: Byte, p: Byte, replay: Boolean) {
        val name = Ocbm.btpName(p)
        if (p == Ocbm.BTP_PAIR_REJECTED) {
            boardFailed(OcbmBoard.BT_PHASE, "PAIR_REJECTED — the pairing was refused; the bond may need forgetting on the box (MGMT_FORGET) AND on the iPhone")
            return
        }
        boardUp(OcbmBoard.BT_PHASE, if (replay) "$name (replayed mirror, not a change)" else name)
        val v = p.toInt() and 0xFF
        val pv = prev.toInt() and 0xFF
        if (!replay && prev != Ocbm.BTP_PAIR_REJECTED && v < pv && lastProjMode == Ocbm.PM_NONE) {
            log.w("~~ BT phase went BACKWARDS ${Ocbm.btpName(prev)} -> $name with no projection session (PROJ_MODE NONE) — " +
                "the BT/iAP2 handshake reset before a session formed; if PROJ_MODE just went NONE above, this is the tail of a session that ended")
        }
    }

    /**
     * One Board line for the identified phone: model, OS and the BR/EDR `deviceID` that ties it to a
     * `MGMT_INFO` bond, plus the user-given name. Hand-scanned, honouring the box's `\"` escaping,
     * because the field set is fixed (receiver `publish_phone_identity`) and the app carries no JSON
     * parser for this.
     */
    private fun phoneSummary(json: String): String {
        fun field(k: String): String {
            val key = "\"$k\":\""
            val i = json.indexOf(key)
            if (i < 0) return ""
            val sb = StringBuilder()
            var j = i + key.length
            while (j < json.length) {
                val c = json[j]
                if (c == '\\' && j + 1 < json.length) { sb.append(json[j + 1]); j += 2; continue }
                if (c == '"') break
                sb.append(c); j++
            }
            return sb.toString()
        }
        val name = field("name"); val model = field("model")
        val os = "${field("osName")} ${field("osVersion")}".trim()
        val id = field("deviceID")
        return buildString {
            if (name.isNotEmpty()) append('"').append(name).append("\" ")
            if (model.isNotEmpty()) append(model).append(' ')
            if (os.isNotEmpty()) append(os).append(' ')
            if (id.isNotEmpty()) append("deviceID=").append(id)
            if (isEmpty()) append("identified (${json.length}B, no recognised fields)")
        }.trim()
    }

    // ---- CH_LOG ---------------------------------------------------------------------------------

    /**
     * Decode one `CH_LOG` frame: back-to-back entries, each
     * `[source u8][flags u8][seq u16 LE][unix_ms u64 LE][len u16 LE][text len B]`.
     *
     * This is a deliberate port of `ocbm-proto`'s `decode_log_entry`, INCLUDING its three
     * rejections — a `len` above [Ocbm.LOG_MAX_LINE], a `len` running past the end of the frame, and
     * a [Ocbm.LOG_F_DROPPED] entry whose `len` is not exactly 4. Entries are self-delimiting, so a
     * tolerant decoder lets one corrupt length walk the reader off the end of every entry behind it,
     * and the drop count is the one field a host acts on. On a bad entry we abandon the REST of the
     * frame rather than trying to resynchronise inside it: the next frame starts clean.
     */
    private fun handleLog(pl: ByteArray) {
        var off = 0
        while (off < pl.size) {
            if (pl.size - off < Ocbm.LOG_ENTRY_HDR) {
                log.w("<< CH_LOG truncated entry header (${pl.size - off}B left)")
                return
            }
            val source = pl[off].toInt() and 0xFF
            val flags = pl[off + 1].toInt() and 0xFF
            val seq = (pl[off + 2].toInt() and 0xFF) or ((pl[off + 3].toInt() and 0xFF) shl 8)
            var unixMs = 0L
            for (i in 0 until 8) unixMs = unixMs or ((pl[off + 4 + i].toLong() and 0xFF) shl (8 * i))
            val len = (pl[off + 12].toInt() and 0xFF) or ((pl[off + 13].toInt() and 0xFF) shl 8)

            if (len > Ocbm.LOG_MAX_LINE || pl.size - off < Ocbm.LOG_ENTRY_HDR + len) {
                log.w("<< CH_LOG bad entry len=$len at +$off (${pl.size}B frame) — dropping rest of frame")
                return
            }
            if ((flags and Ocbm.LOG_F_DROPPED) != 0 && len != 4) {
                log.w("<< CH_LOG malformed drop report len=$len — dropping rest of frame")
                return
            }
            val body = off + Ocbm.LOG_ENTRY_HDR
            off = body + len

            // Channel-wide, not per source. See [lastLogSeq].
            if (lastLogSeq >= 0) {
                val expect = (lastLogSeq + 1) and 0xFFFF
                if (seq != expect) log.w("<< CH_LOG gap: seq $lastLogSeq -> $seq (box queue overflowed)")
            }
            lastLogSeq = seq

            // The only trace work on this path, and only on the first entry after arming: the
            // stream is proven alive. A drop report counts — the tailer is running, it just lost
            // lines — which is why this sits above the DROPPED branch.
            if (!logFlowing) {
                logFlowing = true
                met(OcbmExpect.LOG_FIRST_LINE)
                boardUp(OcbmBoard.CH_LOG, "flowing — first entry from ${logSourceName(source)}" +
                    (if ((flags and Ocbm.LOG_F_BACKFILL) != 0) " (backfill)" else ""))
            }

            if ((flags and Ocbm.LOG_F_DROPPED) != 0) {
                var n = 0L
                for (i in 0 until 4) n = n or ((pl[body + i].toLong() and 0xFF) shl (8 * i))
                blog.w("!! $n lines dropped by ${logSourceName(source)}")
                onBoxLogDropped?.invoke(source, n)
                continue
            }

            val text = String(pl, body, len, Charsets.UTF_8)
            val backfill = (flags and Ocbm.LOG_F_BACKFILL) != 0
            val truncated = (flags and Ocbm.LOG_F_TRUNCATED) != 0
            // Backfill is history the box already had on disk. Kept, but marked, so a stale line can
            // never be read as live evidence in a capture.
            val tag = buildString {
                append("[box:").append(logSourceName(source)).append(']')
                if (backfill) append("[old]")
                if (truncated) append("[cut]")
            }
            blog.i("$tag $text")
            onBoxLog?.invoke(source, text, backfill, truncated, unixMs)
        }
    }

    /**
     * Arm or disarm the box's `CH_LOG` stream: `[CT_LOG_CTL][enabled u8][cap_kb u16 LE]`.
     *
     * The intent is remembered and re-applied by [subscribe], because the box resets the stream to
     * OFF on every teardown. Enabling always restarts every source from offset 0 — that replay is
     * the backfill, and it is why [Ocbm.LOG_F_BACKFILL] exists.
     *
     * Every `on` arms [OcbmExpect.LOG_FIRST_LINE] for [BOX_REPLY_BUDGET_MS] and resets [logFlowing];
     * [handleLog] meets it on the first entry. That is the absence check for "the box went silent on
     * CH_LOG" — the 2026-09-08 `MGMT_FORGET_ALL` failure, see [reArmAfterMgmt].
     */
    fun logCtl(enabled: Boolean, capKb: Int = Ocbm.LOG_CAP_DEFAULT_KB): Boolean {
        logStreamWanted = enabled
        logCapKb = capKb
        lastLogSeq = -1
        logFlowing = false
        val pl = byteArrayOf(
            Ocbm.CT_LOG_CTL,
            if (enabled) 1 else 0,
            (capKb and 0xFF).toByte(),
            ((capKb ushr 8) and 0xFF).toByte()
        )
        val ok = send(Ocbm.CH_CTRL, pl)
        if (ok) {
            log.i(">> CT_LOG_CTL ${if (enabled) "on, cap ${capKb}KiB" else "off"}")
            if (enabled) {
                boardUp(OcbmBoard.CH_LOG, "armed cap=${capKb}KiB — no entry yet")
                // Arming replays every source from offset 0, so a live tailer answers at once
                // (same millisecond, device-observed 2026-09-09). Re-arming (subscribe, a MGMT
                // wireless restart) replaces the previous expectation rather than stacking one.
                expect(OcbmExpect.LOG_FIRST_LINE, BOX_REPLY_BUDGET_MS,
                    "CT_LOG_CTL on was written; the box replays each log source from offset 0 on arming, so a live tailer produces the first [box:*] entry within milliseconds")
            } else {
                cancelExpect(OcbmExpect.LOG_FIRST_LINE, "CH_LOG disarmed by host")
                boardDown(OcbmBoard.CH_LOG, "disarmed by host")
            }
        } else {
            log.e(">> CT_LOG_CTL write FAILED")
            if (enabled) boardFailed(OcbmBoard.CH_LOG, "CT_LOG_CTL write failed — box narration will NOT reach this log")
        }
        return ok
    }

    /**
     * Answer the box's SSP numeric-comparison prompt: `[CT_PAIR_CONFIRM][accept u8]`.
     *
     * Only reachable when the subscribe config asks for `pairing: numeric_comparison`; under this
     * app's `just_works` the box auto-accepts in-kernel and never prompts. Implemented anyway
     * because since 2026-09-03 the box no longer auto-accepts in numeric mode — it waits 55 s for
     * this and then gives up, so a host without it cannot ever switch pairing modes.
     */
    fun pairConfirm(accept: Boolean): Boolean {
        val ok = send(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_PAIR_CONFIRM, if (accept) 1 else 0))
        log.i(">> CT_PAIR_CONFIRM ${if (accept) "ACCEPT" else "REJECT"}${if (ok) "" else " (write FAILED)"}")
        return ok
    }

    // ---- bring-up -------------------------------------------------------------------------------

    /**
     * Send CT_HELLO and wait for CT_HELLO_ACK, retransmitting on the reference cadence (500 ms for
     * the first 15 s, then 2 s). Every non-ACK frame arriving meanwhile is discarded by [dispatch].
     */
    fun hello(timeoutMs: Long = 20_000): Boolean {
        ctrlQ.clear()
        // Nonce + label. We used to send four zero bytes here, which the box reads as "not supplied"
        // — so its host-replacement detection, which exists precisely to notice an app that died
        // without CT_STOP and re-arm projection for its successor, has never once fired for this app.
        // The value is the PROBE's (one per OcbmProbe generation, see [newHostInstance]), not the
        // process's: a USB blip re-attaches through the same probe and so presents the same nonce —
        // "same host, warm reuse" — while Restart Session and Stop->Start build a new probe and so
        // present a new one. That second half is load-bearing. CT_STOP is fire-and-forget ([stop]
        // ignores the write result); if it is lost and the successor HELLOs inside the box's 10 s
        // HEARTBEAT_GRACE with the OLD nonce, ocbmd takes its "presence never dropped" arm on the
        // following SUBSCRIBE: no GONE->PRESENT edge, no wireless_up, stale radio state under a
        // bring-up that believes it is clean. A changed nonce takes the host_replaced arm instead and
        // rearm_presence_silently forces the edge (ccpa/ocbmd/src/main.rs, CT_HELLO / SUBSCRIBE).
        val inst = hostInstance
        val label = HOST_LABEL.toByteArray(Charsets.UTF_8)
        val hi = ByteArray(6 + label.size)
        hi[0] = Ocbm.CT_HELLO; hi[1] = Ocbm.VERSION
        hi[2] = (inst and 0xFF).toByte()
        hi[3] = ((inst ushr 8) and 0xFF).toByte()
        hi[4] = ((inst ushr 16) and 0xFF).toByte()
        hi[5] = ((inst ushr 24) and 0xFF).toByte()
        System.arraycopy(label, 0, hi, 6, label.size)
        val deadline = System.currentTimeMillis() + timeoutMs
        val fastUntil = System.currentTimeMillis() + 15_000
        var attempt = 0
        // Same budget as the loop below, so the watchdog and the loop agree to the poll granularity
        // on when "never" is. On a timeout both speak: the loop's line says how many attempts were
        // made, the EXPECTED-MISSING line puts it in the class every other missing step shares.
        expect(OcbmExpect.HELLO_ACK, timeoutMs,
            "CT_HELLO is being written on the claimed bulk OUT (retransmit every 500 ms, then 2 s) — a running ocbmd ACKs in milliseconds, so silence means no daemon, a wedged daemon, or wrong framing")
        while (System.currentTimeMillis() < deadline) {
            attempt++
            // A single failed write must NOT abort bring-up: right after a box reboot the gadget is
            // re-enumerating and the first write(s) can fail while a retry 500 ms later succeeds.
            if (!send(Ocbm.CH_CTRL, hi)) { log.w(">> CT_HELLO write failed (attempt $attempt) — retrying") }
            else if (attempt == 1) log.i(">> CT_HELLO (v${Ocbm.VERSION})")
            val waitMs = if (System.currentTimeMillis() < fastUntil) 500L else 2000L
            val end = System.currentTimeMillis() + waitMs
            while (System.currentTimeMillis() < end) {
                val pl = ctrlQ.poll(100, TimeUnit.MILLISECONDS) ?: continue
                if (pl.isNotEmpty() && pl[0] == Ocbm.CT_HELLO_ACK) {
                    log.i("<< CT_HELLO_ACK v${pl[1]} caps=0x%08x [%s] mode=%s"
                        .format(caps, Ocbm.capsString(caps),
                            if (activeMode == Ocbm.MODE_CONSOLE) "CONSOLE" else "PROJECTION"))
                    met(OcbmExpect.HELLO_ACK)
                    boardUp(OcbmBoard.LINK, linkDetail("HELLO_ACK v${pl[1]}"))
                    // A fault, not a warning: without CAP_MFI the CH_MFI relay cannot exist, and
                    // every /auth-setup this session will fail. The box's own log says why
                    // (/dev/i2c-1 open or the chip's first read).
                    if (!hasMfi) log.e("!! CAP_MFI is NOT set — the box has no MFi chip access (/dev/i2c-1 failed); CarPlay authentication cannot work this session")
                    return true
                }
            }
        }
        log.e("!! no CT_HELLO_ACK within ${timeoutMs}ms after $attempt attempts")
        boardFailed(OcbmBoard.LINK, "no CT_HELLO_ACK within ${timeoutMs}ms after $attempt attempts — ocbmd not running, wedged, or framing mismatch")
        return false
    }

    /** Push the wall clock. The box has no RTC battery; do this on every connect. */
    fun setTime(timeoutMs: Long = 2000): Boolean {
        val secs = System.currentTimeMillis() / 1000
        val pl = ByteArray(9)
        pl[0] = Ocbm.CT_SETTIME
        for (i in 0 until 8) pl[1 + i] = ((secs shr (8 * i)) and 0xFF).toByte()
        ctrlQ.clear()
        if (!send(Ocbm.CH_CTRL, pl)) {
            // Used to return false silently; a control write failing right after HELLO_ACK is the
            // transport going away and must be visible as such.
            log.e(">> CT_SETTIME write FAILED")
            return false
        }
        log.i(">> CT_SETTIME $secs (${java.util.Date(secs * 1000)})")
        expect(OcbmExpect.SETTIME_ACK, timeoutMs,
            "CT_SETTIME was written on the same control channel that just carried CT_HELLO_ACK — ocbmd answers it inline")
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            val r = ctrlQ.poll(100, TimeUnit.MILLISECONDS) ?: continue
            if (r.isNotEmpty() && r[0] == Ocbm.CT_SETTIME && r.size >= 10) {
                val ok = r[9] == 0.toByte()
                met(OcbmExpect.SETTIME_ACK)
                if (ok) log.i("<< CT_SETTIME ack status=${r[9]} (applied)")
                // The box answered but could not set its clock: every CH_LOG stamp and the TLS
                // pairing's certificate validity run on that clock. A fault on the box, not here.
                else log.e("<< CT_SETTIME ack status=${r[9]} (settimeofday FAILED on the box — its clock stays bogus)")
                return ok
            }
        }
        // Narration only: the EXPECTED-MISSING line armed above is the fault record.
        log.w("!! no CT_SETTIME ack — continuing anyway")
        return false
    }

    /**
     * The presence latch. The payload is `[0x10]` followed by the config blob VERBATIM — no length
     * prefix, no terminator; the frame length is the only delimiter. An empty blob is legal and
     * means "subscribe, box defaults".
     */
    fun subscribe(config: ByteArray): Boolean {
        if (!running.get()) { log.w("subscribe after stop — refusing (would re-latch the box)"); return false }
        configBlob = config
        val pl = ByteArray(1 + config.size)
        pl[0] = Ocbm.CT_SUBSCRIBE
        System.arraycopy(config, 0, pl, 1, config.size)
        if (!send(Ocbm.CH_CTRL, pl)) {
            log.e(">> CT_SUBSCRIBE write FAILED")
            boardFailed(OcbmBoard.LINK, "CT_SUBSCRIBE write failed — the radio-wake edge was never taken")
            return false
        }
        subscribed = true
        log.i(">> CT_SUBSCRIBE (${config.size}B config) — this is the radio-wake edge")
        if (config.isNotEmpty()) config.toString(Charsets.UTF_8).trim().lines().forEach { log.i("     | $it") }
        boardUp(OcbmBoard.LINK, linkDetail("subscribed, awaiting HOST_PRESENT"))

        // What the box owes us for this edge. Both are re-armed on every subscribe, including the
        // heartbeat's recovery re-subscribe, because the box's state resets with the edge: the
        // supervisor tears wireless down on host loss and brings it up again from here.
        expect(OcbmExpect.HOST_PRESENT, BOX_REPLY_BUDGET_MS,
            "CT_SUBSCRIBE (${config.size}B) was written — ocbmd raises /tmp/host_present and answers SEV_HOST_PRESENT synchronously (2 ms device-observed 2026-09-09)")
        hciState = HCI_PENDING
        hciDeadlineMs = System.currentTimeMillis() + BT_ATTACH_BUDGET_MS
        expect(OcbmExpect.HCI_PRESENT, BT_ATTACH_BUDGET_MS,
            "CT_SUBSCRIBE is the radio-wake edge: the box supervisor runs wireless_up and attaches the BT controller, and CT_BOX_HEALTH must then carry BH_HCI_PRESENT (8 s device-observed 2026-09-09; 0x50 with bit 0 clear = no hci0, docs/wireless/01_BT_AND_RADIO.md)")

        // Everything below is per-subscription state on the box and does NOT survive this edge.
        // Re-assert it here rather than at the call sites, so the heartbeat's own recovery
        // re-subscribe cannot silently drop it.
        //
        // Clock first: the box has no RTC, and CH_LOG stamps every line with the box's idea of now.
        // Arming the log stream before the clock lands buys a screenful of 1970. Bounded low: this
        // also runs on the heartbeat thread during recovery, and the box's grace is 10 s.
        setTime(1000)
        if (radioInhibited) {
            log.i("re-asserting CT_RADIO off — CT_SUBSCRIBE clears the box's radio inhibit")
            radio(false)
        } else {
            boardUp(OcbmBoard.RADIOS, "allowed — CT_SUBSCRIBE clears the box's radio inhibit")
        }
        if (logStreamWanted) logCtl(true, logCapKb)
        return true
    }

    /**
     * `CT_RADIO` — host->box radio inhibit. `0` writes the box's radio-off flag, which its supervisor
     * checks at the top of `wireless_up()`; `1` clears it.
     *
     * This is the one lever that resets the box's radios WITHOUT touching `host_present`, so it is the
     * only strong recovery action that carries no exposure to the supervisor's flap detector (a
     * host_present cycle inside ~20 s escalates to an ocbmd restart and then a full box reboot). It is
     * also how a mid-session box reconnect is told to keep Bluetooth down: CarPlay does not need BT
     * once it is streaming.
     *
     * Implemented on the box since before this app existed and, until now, called by no host at all.
     */
    fun radio(on: Boolean): Boolean {
        radioInhibited = !on
        val ok = send(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_RADIO, if (on) 1 else 0))
        if (ok) {
            log.i(">> CT_RADIO ${if (on) "ON (allow radios)" else "OFF (inhibit radios)"}")
            boardUp(OcbmBoard.RADIOS, if (on) "allowed (CT_RADIO on)" else "INHIBITED by host (CT_RADIO off) — the box's wireless_up will not run")
        } else {
            log.e(">> CT_RADIO ${if (on) "ON" else "OFF"} write FAILED")
            boardFailed(OcbmBoard.RADIOS, "CT_RADIO ${if (on) "on" else "off"} write failed — box radio state unknown")
        }
        return ok
    }

    /** Start the 1 Hz beat. If we are un-subscribed, the tick re-SUBSCRIBEs instead (it also stamps liveness). */
    fun startHeartbeat() {
        // isAlive, not != null: the loop nulls the field on exit (below), so a heartbeat that ended on
        // a spurious interrupt can be restarted instead of refusing forever.
        if (heartbeatThread?.isAlive == true) return
        val t = Thread({
            var writeFailures = 0
            var resubBackoff = 0L
            var nextResubAt = 0L
            // Consecutive healthy ticks while subscribed. The backoff clears only after the link
            // has held for this long — see the comment at the reset site.
            var stableTicks = 0
            val STABLE_TICKS_TO_CLEAR = 30
            // Set on the presumed-dead exit so `finally` does not overwrite FAILED with DOWN.
            var died = false
            try {
                while (running.get()) {
                    try { Thread.sleep(Ocbm.HEARTBEAT_MS) } catch (_: InterruptedException) { break }
                    if (!running.get()) break
                    // One-shot, on this thread because it already ticks: promote a pending HCI wait
                    // to MISSED at its deadline so the Board says so even if the box never pushes
                    // another health report. Logs once per subscribe at most — see [hciState].
                    val hciDue = hciDeadlineMs
                    if (hciDue != 0L && hciState == HCI_PENDING && System.currentTimeMillis() >= hciDue) {
                        hciDeadlineMs = 0L
                        hciState = HCI_MISSED
                        val h = lastBoxHealth
                        boardFailed(OcbmBoard.HEALTH,
                            if (h < 0) "no CT_BOX_HEALTH at all within ${BT_ATTACH_BUDGET_MS}ms of CT_SUBSCRIBE — the box never reported its health"
                            else "0x%02x [%s] — no hci0 within ${BT_ATTACH_BUDGET_MS}ms of CT_SUBSCRIBE: the box's BT controller never attached (radio HAL / hciattach — read the [box:bt] lines)".format(h, Ocbm.bhString(h)))
                    }
                    if (subscribed) {
                        // Check the write result — it is the liveness signal. On a box reboot / gadget
                        // stall every write fails; declare the link dead rather than looking healthy
                        // forever with a hung session.
                        if (send(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_HEARTBEAT))) {
                            writeFailures = 0
                            // Do NOT clear the backoff just because a write succeeded. A successful
                            // USB write says nothing about whether the box ACCEPTED the subscription,
                            // and clearing it here made the escalation unreachable: re-subscribe ->
                            // one good heartbeat -> backoff reset -> HOST_GONE -> immediate re-latch,
                            // steady state ~2 s forever. That is precisely the rapid down/up edge
                            // pattern that trips the supervisor's flap detector and escalates to an
                            // ocbmd restart and then a full box reboot. Only a SUSTAINED subscription
                            // is evidence the link is healthy.
                            if (resubBackoff != 0L && ++stableTicks >= STABLE_TICKS_TO_CLEAR) {
                                log.i("link stable for ${STABLE_TICKS_TO_CLEAR}s — clearing re-subscribe backoff")
                                resubBackoff = 0L; nextResubAt = 0L; stableTicks = 0
                            }
                        } else if (++writeFailures >= 5) {
                            log.e("!! heartbeat write failed ${writeFailures}x — link presumed dead")
                            died = true
                            boardFailed(OcbmBoard.HEARTBEAT, "write failed ${writeFailures}x — link presumed dead")
                            boardFailed(OcbmBoard.LINK, "heartbeat writes failing — box rebooted, adapter unplugged, or gadget stalled; HOST_GONE synthesised")
                            // Clear the latch HERE. This path synthesises HOST_GONE by calling the
                            // observer directly, which bypasses handleCtrl() — the only other place
                            // `subscribed` is cleared. Leaving it true stranded the client in a state
                            // where the link was dead but every readiness check said "up": the UI told
                            // the driver to press Start, and autoStart() then short-circuited on
                            // `subscribed == true` with "already subscribed; nothing to do". The one
                            // action offered was the one action that could not work; only Stop-then-
                            // Start escaped. Device-observed 2026-08-27.
                            subscribed = false
                            onSessionEvent?.invoke(Ocbm.SEV_HOST_GONE)
                            break
                        }
                    } else {
                        // HOST_GONE recovery WITH backoff: re-latching at 1 Hz produces rapid down/up
                        // edges that trip the box supervisor's flap detector (-> ocbmd restart -> full
                        // box reboot). Space attempts out: 2s, 4s, 8s, capped at 15s.
                        stableTicks = 0
                        // No config means this client was brought up with subscribe=false on
                        // purpose. Keep beating (it stamps liveness and keeps CH_MFI usable) but
                        // never latch presence behind the caller's back.
                        if (configBlob.isEmpty()) continue
                        val now = System.currentTimeMillis()
                        if (now >= nextResubAt) {
                            log.i(">> re-CT_SUBSCRIBE (recovering from HOST_GONE, backoff=${resubBackoff}ms)")
                            subscribe(configBlob)
                            resubBackoff = if (resubBackoff == 0L) 2000L else (resubBackoff * 2).coerceAtMost(15_000L)
                            nextResubAt = now + resubBackoff
                        }
                    }
                }
            } catch (_: InterruptedException) {
                // stop() interrupts this thread. If the interrupt lands while a recovery
                // re-SUBSCRIBE is inside setTime()'s ctrlQ.poll(), the exception surfaces here
                // rather than at the sleep above. Deliberate teardown, not a fault — it must not
                // escape run(): no UncaughtExceptionHandler is installed, so Android would kill
                // the process.
            } finally {
                heartbeatThread = null
                if (!died) boardDown(OcbmBoard.HEARTBEAT, if (running.get()) "thread exited on interrupt — restartable" else "client stopped")
            }
        }, "ocbm-heartbeat")
        t.isDaemon = true
        heartbeatThread = t
        t.start()
        log.i(">> heartbeat started at ${Ocbm.HEARTBEAT_MS}ms (box watchdog is ${Ocbm.HEARTBEAT_GRACE_MS}ms)")
        boardUp(OcbmBoard.HEARTBEAT, "${Ocbm.HEARTBEAT_MS}ms cadence, box watchdog ${Ocbm.HEARTBEAT_GRACE_MS}ms")
    }

    // ---- CH_MFI relay ---------------------------------------------------------------------------

    companion object {
        /** Counts [newHostInstance] calls, so two probes built inside one millisecond still differ. */
        private val HOST_GENERATION = java.util.concurrent.atomic.AtomicInteger(0)

        /**
         * Mint a host instance nonce. One per [zeno.gmccpa.ocbm.OcbmProbe], minted when the probe is
         * built and re-sent on every HELLO that probe makes — see the note in [hello] for why the
         * lifetime is the probe's and not the process's.
         *
         * Derived from the pid, uptime and a generation counter so two successive launches, or two
         * probes in one process, cannot collide; and never 0 — the box reads 0 as "not supplied" and
         * falls back to its old blind behaviour.
         */
        fun newHostInstance(): Int {
            val h = (android.os.Process.myPid().toLong() shl 20) xor
                android.os.SystemClock.elapsedRealtime() xor
                (HOST_GENERATION.incrementAndGet().toLong() shl 44)
            val v = (h xor (h ushr 32)).toInt()
            return if (v == 0) 1 else v
        }
        /**
         * What kind of host this is. Diagnostic only — the box logs it and reports it in MGMT_INFO so
         * "what is talking to me" is answerable. Deliberately carries no serial, address or user data.
         */
        internal const val HOST_LABEL = "gm-ccpa head unit (bridge role)"

        /**
         * Default budget for the MFi certificate. Generous because the box takes a bounded flock on
         * /tmp/carplay_mfi.lock with a 10 s deadline, and MFi blocks its single-threaded dispatch.
         * Named so `OcbmProbe.runAllLocked` arms its expectation with the SAME number it waits.
         */
        const val MFI_CERT_TIMEOUT_MS = 12_000L
        /**
         * Default budget for a signature. The chip sequence alone runs up to ~2.1 s, and with lock
         * contention the worst case approaches 12 s. Same sharing rule as [MFI_CERT_TIMEOUT_MS].
         */
        const val MFI_SIGN_TIMEOUT_MS = 15_000L
        /**
         * How long the box gets to answer a control-plane request that `ocbmd` handles inline
         * (`SEV_HOST_PRESENT` after SUBSCRIBE, the first `CH_LOG` entry after LOG_CTL). Both
         * answer in milliseconds on a live daemon; the budget is the box's OWN liveness window,
         * [Ocbm.HEARTBEAT_GRACE_MS] — the interval after which the box itself declares a silent
         * peer gone. Not a new number.
         */
        const val BOX_REPLY_BUDGET_MS = Ocbm.HEARTBEAT_GRACE_MS
        /**
         * From CT_SUBSCRIBE to the first `CT_BOX_HEALTH` carrying `BH_HCI_PRESENT`.
         *
         * Deliberately EQUAL to `SessionSupervisor.HANDOFF_TIMEOUT_MS` (45 s) rather than referenced
         * from it: that constant is the app's existing deadline for "the box hung in Bluetooth
         * bring-up" and the ladder acts on it; this expectation fires at the same instant and names
         * the cause (no hci0) rather than the symptom (no handoff). Held here as a literal because
         * the client must not depend on the supervisor's class. A healthy attach lands at ~8 s
         * (2026-09-09); a cold box with one `hciattach` retry lands at ~20 s; the box's own attach
         * loop gives up far later, so anything past 45 s is the box failing, not the budget.
         */
        const val BT_ATTACH_BUDGET_MS = 45_000L

        private const val HCI_PENDING = 0
        private const val HCI_SEEN = 1
        private const val HCI_MISSED = 2
    }

    /**
     * Serializes ALL CH_MFI traffic. The channel has TWO concurrent consumers — the native receiver
     * core's MFi relay (on control-connection threads) and any probe/bring-up cert/sign — and the
     * wire response carries no opcode echo or request id, so without a lock a certificate can be
     * returned as the answer to a signature request (auth-setup then fails and the session drops).
     *
     * A real lock, not a monitor, so acquisition can be BOUNDED.
     *
     * `synchronized` has no timed acquire, so the wait to get in was unbounded and ADDITIVE to the
     * caller's own timeout: worst case was another caller's full 15 s plus this caller's 15 s. That
     * matters because the iAP2 tunnel's chip ops run inside `ControlServer::feed` with the native
     * CORE and SESSION guards held, so the phone's `POST /command` gets no HTTP reply until they
     * return — and the phone's request timeout is 10 s. The budget below is now TOTAL: lock
     * acquisition and the reply wait come out of the same clock.
     */
    private val mfiLock = java.util.concurrent.locks.ReentrantLock(true)

    /** Serialises CH_FILE: the channel carries no request id, so two pulls would interleave. */
    private val fileLock = java.util.concurrent.locks.ReentrantLock(true)

    /**
     * Per-request correlation tag. Rolls 1..255, never 0, so a zero byte from a torn frame is never
     * mistaken for a valid tag. See [Mfi.TAG_LEN] for why length correlation was not enough.
     */
    private var mfiTag: Int = 0
    private fun nextMfiTag(): Byte {
        mfiTag = (mfiTag % 255) + 1
        return mfiTag.toByte()
    }

    private fun mfiRequest(
        build: (Byte) -> ByteArray,
        timeoutMs: Long,
        expectedOk: (ByteArray) -> Boolean,
    ): Mfi.Response? {
        val deadline = System.currentTimeMillis() + timeoutMs
        // Bounded acquire out of the SAME budget. Failing fast here is correct: the caller is either
        // the tunnel (a late frame is fine, a late HTTP reply is not) or /auth-setup (which surfaces
        // the failure), and blocking past the phone's timeout helps neither.
        if (!mfiLock.tryLock(timeoutMs, TimeUnit.MILLISECONDS)) {
            log.w("!! CH_MFI busy — could not acquire the chip lock within ${timeoutMs}ms")
            return null
        }
        try {
            val tag = nextMfiTag()
            // Drain stale/late replies from a previous request UNDER the lock, so this request's poll
            // cannot pick up an earlier answer that arrived after that caller's timeout.
            while (mfiQ.poll() != null) { /* discard */ }
            if (!send(Ocbm.CH_MFI, build(tag))) return null
            while (true) {
                val remaining = deadline - System.currentTimeMillis()
                if (remaining <= 0) return null
                val pl = mfiQ.poll(remaining, TimeUnit.MILLISECONDS) ?: return null
                val resp = Mfi.parse(pl, pl.size) ?: continue
                // Tag first, when the box echoes one: this is the only check that can tell two
                // same-length SIGNATURE replies apart, which length correlation structurally cannot.
                // It also correctly discards a misattributed error status, which the old code
                // accepted as-is on the assumption the caller would retry — the tunnel does not.
                if (resp.tag != null) {
                    if (resp.tag != tag) {
                        log.w("!! CH_MFI reply tag 0x%02x != request 0x%02x — stale, discarding"
                            .format(resp.tag, tag))
                        continue
                    }
                    return resp
                }
                // Untagged peer (older box): fall back to the old weak length correlation.
                if (resp.ok && !expectedOk(resp.payload)) {
                    log.w("!! CH_MFI stale/mismatched OK reply (${resp.payload.size}B) discarded")
                    continue
                }
                return resp
            }
        } finally {
            mfiLock.unlock()
        }
    }

    /**
     * Fetch the MFi certificate. Generous timeout: the box takes a bounded flock on
     * /tmp/carplay_mfi.lock with a 10 s deadline, and MFi blocks its single-threaded dispatch.
     */
    fun mfiCertificate(timeoutMs: Long = MFI_CERT_TIMEOUT_MS): Mfi.Response? {
        log.i(">> CH_MFI copy_certificate")
        // A certificate is ~945 B; a 128-B OK reply is the signature answer to another request.
        return mfiRequest({ t -> Mfi.certRequest(t) }, timeoutMs) { it.size > Mfi.SIG_LEN }
    }

    /**
     * Sign a 20-byte SHA-1 digest. The chip sequence alone runs up to ~2.1 s, and with lock
     * contention the worst case approaches 12 s.
     */
    fun mfiSign(digest: ByteArray, timeoutMs: Long = MFI_SIGN_TIMEOUT_MS): Mfi.Response? {
        log.i(">> CH_MFI create_signature (${digest.size}B digest)")
        return mfiRequest({ t -> Mfi.signRequest(digest, t) }, timeoutMs) { it.size == Mfi.SIG_LEN }
    }

    // ---- CH_FILE --------------------------------------------------------------------------------

    /**
     * Read a file off the BOX over `CH_FILE`, end-to-end CRC-checked.
     *
     * # Why the app needs this
     *
     * Until now the box's own logs were reachable only over the UART or an OCBM root console, so
     * every box-side fault had to be diagnosed on a different machine from the one holding the app's
     * logs -- and in a vehicle that usually meant not at all. Both halves of a failure now land in
     * one place: this feeds [zeno.gmccpa.ProbeLog], which goes to logcat, which the always-on
     * capture already bundles.
     *
     * Serialised on [fileLock] because the channel carries no request id: two concurrent pulls would
     * interleave their FILE_DATA frames into each other's buffers with nothing to tell them apart.
     *
     * Returns null on timeout, a non-OK status, or a CRC/size mismatch -- never a partial file, so a
     * caller can treat a non-null result as trustworthy.
     */
    fun filePull(path: String, timeoutMs: Long = 8_000): ByteArray? {
        if (!fileLock.tryLock(timeoutMs, TimeUnit.MILLISECONDS)) {
            log.w("!! CH_FILE busy — could not start a pull of $path within ${timeoutMs}ms")
            return null
        }
        try {
            val deadline = System.currentTimeMillis() + timeoutMs
            while (fileQ.poll() != null) { /* drop anything left by a timed-out pull */ }
            val pathBytes = path.toByteArray(Charsets.UTF_8)
            val req = ByteArray(1 + pathBytes.size)
            req[0] = Ocbm.FILE_PULL
            System.arraycopy(pathBytes, 0, req, 1, pathBytes.size)
            if (!send(Ocbm.CH_FILE, req)) return null

            val body = java.io.ByteArrayOutputStream()
            while (true) {
                val remaining = deadline - System.currentTimeMillis()
                if (remaining <= 0) { log.w("!! CH_FILE pull of $path timed out"); return null }
                val pl = fileQ.poll(remaining, TimeUnit.MILLISECONDS) ?: continue
                if (pl.isEmpty()) continue
                when (pl[0]) {
                    Ocbm.FILE_DATA -> body.write(pl, 1, pl.size - 1)
                    Ocbm.FILE_ACK -> {
                        if (pl.size < 10) { log.w("!! CH_FILE ACK truncated (${pl.size}B)"); return null }
                        val status = pl[1]
                        if (status != Ocbm.FILE_OK) {
                            // NO_SUCH_FILE is ordinary: a box that never ran wireless has no wl.log.
                            if (status != Ocbm.FILE_ERR_NOFILE)
                                log.w("!! CH_FILE pull $path: ${Ocbm.fileStatusName(status)}")
                            return null
                        }
                        val wantCrc = (pl[2].toLong() and 0xFF) or ((pl[3].toLong() and 0xFF) shl 8) or
                            ((pl[4].toLong() and 0xFF) shl 16) or ((pl[5].toLong() and 0xFF) shl 24)
                        val wantSize = (pl[6].toLong() and 0xFF) or ((pl[7].toLong() and 0xFF) shl 8) or
                            ((pl[8].toLong() and 0xFF) shl 16) or ((pl[9].toLong() and 0xFF) shl 24)
                        val got = body.toByteArray()
                        if (got.size.toLong() != wantSize) {
                            log.w("!! CH_FILE $path: got ${got.size}B, box said ${wantSize}B — discarding")
                            return null
                        }
                        val crc = java.util.zip.CRC32().apply { update(got) }.value
                        if (crc != wantCrc) {
                            log.w("!! CH_FILE $path: crc 0x%08x != box 0x%08x — discarding".format(crc, wantCrc))
                            return null
                        }
                        return got
                    }
                    else -> { /* not ours */ }
                }
            }
        } finally {
            fileLock.unlock()
        }
    }

    // ---- CH_MGMT --------------------------------------------------------------------------------

    /** MGMT_GET_INFO → MGMT_INFO, whose payload is UTF-8 JSON running to the end of the frame. */
    fun mgmtGetInfo(timeoutMs: Long = 5000): String? {
        mgmtQ.clear()
        if (!send(Ocbm.CH_MGMT, byteArrayOf(Ocbm.MGMT_GET_INFO))) return null
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            val pl = mgmtQ.poll(200, TimeUnit.MILLISECONDS) ?: continue
            if (pl.isNotEmpty() && pl[0] == Ocbm.MGMT_INFO) {
                return String(pl, 1, pl.size - 1, Charsets.UTF_8)
            }
        }
        return null
    }

    /** Any of the four action verbs; returns the status byte from MGMT_ACK (0 ok, 1 error). */
    fun mgmtAction(verb: Byte, arg: ByteArray = ByteArray(0), timeoutMs: Long = 5000): Int? {
        mgmtQ.clear()
        val pl = ByteArray(1 + arg.size)
        pl[0] = verb
        System.arraycopy(arg, 0, pl, 1, arg.size)
        if (!send(Ocbm.CH_MGMT, pl)) return null
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            val r = mgmtQ.poll(200, TimeUnit.MILLISECONDS) ?: continue
            if (r.size >= 3 && r[0] == Ocbm.MGMT_ACK && r[1] == verb) {
                if (r[2].toInt() == 0) reArmAfterMgmt(verb)
                return r[2].toInt()
            }
        }
        return null
    }

    /**
     * Re-apply per-session box state that a MGMT verb has just reset.
     *
     * [subscribe] already re-arms the log stream and the radio inhibit, which covers teardown and
     * heartbeat recovery — but a MGMT verb that restarts the box's wireless stack resets box-side
     * state WITHOUT any re-subscribe, so nothing re-sends CT_LOG_CTL. Device-observed 2026-09-08:
     * after `MGMT_FORGET_ALL` the box went permanently silent on CH_LOG while the link stayed
     * healthy and the client still reported `subscribed=true` — the failure WAS invisible, because
     * "no box lines" is indistinguishable from "box has nothing to say". The old CH_FILE poller
     * could not fail this way; it re-polled unconditionally. Since 2026-09-10 the re-arm below (and
     * every other [logCtl] on) arms [OcbmExpect.LOG_FIRST_LINE], so a re-arm the box does not
     * answer with an entry inside [BOX_REPLY_BUDGET_MS] is an `EXPECTED-MISSING` line, not silence.
     *
     * Only the verbs that bounce the wireless stack qualify. `MGMT_GET_INFO` changes nothing, and
     * `MGMT_REBOOT` / `MGMT_ENTER_NCM` take the box away entirely — there is nothing to re-arm on
     * the far side of those, and the next bring-up subscribes from scratch.
     */
    private fun reArmAfterMgmt(verb: Byte) {
        val restarts = verb == Ocbm.MGMT_RESTART_WIRELESS ||
            verb == Ocbm.MGMT_FORGET_ALL ||
            verb == Ocbm.MGMT_FORGET_DEVICE
        if (!restarts) return
        if (radioInhibited) radio(false)
        if (logStreamWanted) {
            log.i("re-arming CH_LOG — the box's wireless restart reset the log stream")
            logCtl(true, logCapKb)
        }
    }

    // ---- teardown -------------------------------------------------------------------------------

    /** Clean shutdown: CT_STOP ends the box session immediately (no warm-reuse grace since 2026-09-03) so a relaunch gets a clean session. */
    fun stop() {
        if (!running.compareAndSet(true, false)) return
        // JOIN, do not just interrupt: bulkTransfer is not interruptible, so a tick already inside
        // send() keeps writing for up to the write timeout. Closing the transport under it is the
        // gadget-stall case. Joining also stops a straggling tick from re-SUBSCRIBEing after CT_STOP.
        heartbeatThread?.let {
            it.interrupt()
            try { it.join(3000) } catch (_: InterruptedException) { Thread.currentThread().interrupt() }
        }
        heartbeatThread = null
        // Nothing armed by this client can be met after this point, and none of it is a fault: the
        // teardown is deliberate. Named, not cancelAll(): other subsystems' expectations are theirs.
        for (e in OcbmExpect.CLIENT_OWNED) cancelExpect(e, "OCBM client stopped")
        hciDeadlineMs = 0L
        // Disarm the log stream before CT_STOP so the box is not still packing frames for a host
        // that has gone. The box would reset it on teardown anyway; sending it makes the shutdown
        // legible in the box's own log, which is the first place anyone looks after a bad session.
        if (logStreamWanted) logCtl(false)
        val wasSubscribed = subscribed
        if (subscribed) {
            log.i(">> CT_STOP")
            // Immediate teardown since 2026-09-03 — the old 5 s warm-reuse grace is gone, so a
            // relaunch after this gets a full clean session, not a resumed one.
            send(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_STOP))
            subscribed = false
        }
        transport.stop()
        // Box-side facts do not outlive the link. Only entries this client actually wrote go DOWN,
        // so a session that never reached pairing does not print five "(new) -> DOWN" lines.
        boardDown(OcbmBoard.LINK, if (wasSubscribed) "CT_STOP sent, client stopped" else "client stopped (never subscribed)")
        // CH_LOG is not in this list: logCtl(false) above already marked it DOWN with the reason.
        for (n in listOf(OcbmBoard.HEALTH, OcbmBoard.BT_PHASE, OcbmBoard.PHONE, OcbmBoard.PAIRING_CODE, OcbmBoard.PROJ_MODE, OcbmBoard.RADIOS)) {
            if (boardTouched.contains(n)) boardDown(n, "link stopped")
        }
    }

    fun statsLine(): String =
        "frames=$framesIn bytes=$bytesIn resyncBytes=${reasm.resyncBytes} helloAck=$helloAcked " +
            "subscribed=$subscribed caps=[${Ocbm.capsString(caps)}]"
}

/**
 * [SessionTrace.Board] entry names for the OCBM side, in one place so `OcbmProbe`,
 * `UsbBulkTransport` and this client cannot spell the same component two ways.
 */
internal object OcbmBoard {
    /** The claimed USB interface — written by `UsbBulkTransport.open/stop` and, for the waiting states, by `OcbmProbe.awaitClaimable`. */
    const val USB_CLAIM = "usb-claim"
    /** The `ocbm-read` thread. */
    const val USB_READ = "usb-read-loop"
    /** The OCBM link: HELLO_ACK, subscribed, HOST_PRESENT confirmed — or FAILED with why. */
    const val LINK = "ocbm-link"
    /** The 1 Hz heartbeat thread. */
    const val HEARTBEAT = "ocbm-heartbeat"
    /** The box's `CH_LOG` push stream: armed, flowing, disarmed. */
    const val CH_LOG = "box-log-stream"
    /** Host-side radio inhibit (`CT_RADIO`), as last commanded. */
    const val RADIOS = "box-radios"
    /** The box's last `CT_BOX_HEALTH`, decoded via [Ocbm.bhString]; FAILED when hci0 is missing. */
    const val HEALTH = "box-health"
    /** The BT/iAP2 handshake phase (`CT_BT_PHASE`). */
    const val BT_PHASE = "bt-phase"
    /** The identified phone (`CT_PHONE_IDENT`). */
    const val PHONE = "phone"
    /** The SSP numeric-comparison code on display (`CT_PAIRING_CODE`). */
    const val PAIRING_CODE = "pairing-code"
    /** Which projection transport owns the box (`CT_PROJ_MODE`). */
    const val PROJ_MODE = "proj-mode"
}

/**
 * [SessionTrace] expectation names for the bring-up ladder. Each is armed exactly where the wait
 * begins and met exactly where the answer is decoded; see the class KDoc of [OcbmClient] for the
 * budget behind each.
 */
internal object OcbmExpect {
    /** Armed by `UsbAttachActivity` when it forwards an attach; met at the top of `OcbmProbe.runAllLocked`. */
    const val LINK_ATTEMPT = "link-attempt-after-attach"
    /** Armed by `OcbmProbe.awaitClaimable` when it raises the permission dialog; met when the grant appears. */
    const val USB_PERMISSION = "usb-permission"
    const val HELLO_ACK = "ocbm-hello-ack"
    const val SETTIME_ACK = "ocbm-settime-ack"
    /** Bring-up MFi steps, armed by `OcbmProbe.runAllLocked` with the client's own budgets. */
    const val MFI_CERT = "mfi-cert"
    const val MFI_SIGN = "mfi-sign"
    const val MFI_SIGN_RETRY = "mfi-sign-retry"
    const val HOST_PRESENT = "sev-host-present"
    const val HCI_PRESENT = "box-health-hci"
    const val LOG_FIRST_LINE = "box-log-first-entry"

    /** The names [OcbmClient] arms itself, cancelled together on [OcbmClient.stop]. */
    val CLIENT_OWNED = listOf(HELLO_ACK, SETTIME_ACK, HOST_PRESENT, HCI_PRESENT, LOG_FIRST_LINE)
    /** The names `OcbmProbe` arms, cancelled together when it stops the link. */
    val PROBE_OWNED = listOf(USB_PERMISSION, MFI_CERT, MFI_SIGN, MFI_SIGN_RETRY)
}

/**
 * Name a `CH_LOG` source id. Mirrors `ocbm-proto`'s `log_source_name` exactly.
 *
 * Lives here rather than in the shared `OcbmProto.kt` because that file is ccpa_custom's, consumed
 * through a symlink — this repo restates the protocol, it does not extend it. An unknown id renders
 * rather than drops: the source table grows box-side, and a host that discards lines it cannot label
 * loses exactly the evidence about the thing that is new.
 */
internal fun logSourceName(id: Int): String = when (id) {
    Ocbm.LOG_SRC_BOX -> "box"
    Ocbm.LOG_SRC_AIRPLAYD -> "airplayd"
    Ocbm.LOG_SRC_AIRPLAYD_WL -> "airplayd-wl"
    Ocbm.LOG_SRC_IAP2D -> "iap2d"
    Ocbm.LOG_SRC_AA_BRIDGE -> "aa-bridge"
    Ocbm.LOG_SRC_RX_CONNECT -> "rx-connect"
    Ocbm.LOG_SRC_BT -> "bt"
    Ocbm.LOG_SRC_RADIO_AP_DHCP -> "radio_ap_dhcp"
    Ocbm.LOG_SRC_RADIO_BT_ATTACH -> "radio_bt_attach"
    Ocbm.LOG_SRC_RX_CONNECT_WL -> "rx-connect_wl"
    Ocbm.LOG_SRC_CARPLAY_WIRELESS -> "wl"
    Ocbm.LOG_SRC_INTERNAL -> "internal"
    else -> "src$id"
}
