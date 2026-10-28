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
 *   1. CT_HELLO, retransmitted until CT_HELLO_ACK arrives. While waiting, CH_CTRL frames ARE
 *      decoded by [dispatch]/handleCtrl; [hello] clears ctrlQ and ignores non-ACK entries.
 *      CH_MFI/CH_MGMT/CH_FILE frames are QUEUED (nothing clears them at HELLO; each queue is
 *      drained only by its own verb before a request), CH_LOG is replayed (handleLog), and only unknown/A-V channels are
 *      logged and dropped — a prior session can leave kernel-buffered A/V frames queued ahead of
 *      the ACK. (Corrected 2026-10-06: this said every other frame was discarded.)
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
    /**
     * Monotonic ms for the client's own deadlines (v5.8 V7 / Q015): the HCI-attach budget armed by
     * [subscribe] and promoted to MISSED by the heartbeat. NOT the wall clock — the bench's clock is
     * stepped by the adb time-sync hook, and a forward step promoted a pending wait to MISSED while
     * a backward one stretched the budget. Since fix1 every deadline in this class (HELLO retry,
     * SETTIME ack, the re-SUBSCRIBE backoff, the transport-edge wait, the CH_MFI / CH_FILE / CH_MGMT
     * reply budgets) runs on it too; the one wall-clock read left is the CT_SETTIME value itself.
     * Injected so the JVM tests run with no android.os.
     */
    private val monoMs: () -> Long = { android.os.SystemClock.elapsedRealtime() },
) {
    private val reasm = Reassembler()
    private val seq = AtomicInteger(0)
    private val running = AtomicBoolean(false)

    private val ctrlQ = LinkedBlockingQueue<ByteArray>()
    private val mfiQ = LinkedBlockingQueue<ByteArray>()
    private val mgmtQ = LinkedBlockingQueue<ByteArray>()
    private val fileQ = LinkedBlockingQueue<ByteArray>()

    @Volatile var helloAcked = false; private set
    /**
     * The transport is gone (read loop died): no CT_HELLO_ACK stands any more. [helloAcked] is
     * otherwise only ever set, and OcbmProbe.hasLink() gates every self-provoked connect-out on it.
     */
    fun markLinkDead() { helloAcked = false }
    @Volatile var caps = 0; private set
    @Volatile var activeMode: Byte = Ocbm.MODE_PROJECTION; private set
    @Volatile var subscribed = false; private set
    @Volatile var lastSessionEvent: Byte = 0; private set
    /**
     * HOST_GONE nudges ignored because this client holds no subscription after its HELLO_ACK. The
     * no-subscribe relink's liveness beats draw one every ~30 s, so only the first is logged per
     * client and the rest are counted here ([statsLine]; review A2). Written on `ocbm-read` only.
     */
    @Volatile var hostGoneNudgesIgnored = 0; private set
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
    /**
     * Whether the last `CT_BOX_HEALTH` carried envelope bit2 ([Ocbm.F_REPLAY]): the box's latched
     * mirror re-sent after a (re)SUBSCRIBE, not a change. Read inline from an `onBoxHealth`
     * handler, exactly like [lastBtPhaseReplay].
     */
    @Volatile var lastBoxHealthReplay = false; private set
    /**
     * Latch for the "box lost" edge of THIS client. CASed by SessionCore's transport-death edge
     * (OcbmProbe.onLinkDead — since v5.6 also reached when the heartbeat declares the link dead) and
     * by the heartbeat's fallback HOST_GONE synthesis (only when that edge cannot run), so the edge
     * is delivered once per client whichever lands first. The box's own CH_CTRL HOST_GONE is deliberately NOT
     * under it: that one is recoverable (heartbeat re-SUBSCRIBE), and CASing it would swallow the
     * later, real transport-death edge of the same client. Its consumers are idempotent
     * (SessionSummary.end, SessionSupervisor.onBoxLost). 2026-10-06.
     */
    val hostGoneDelivered = java.util.concurrent.atomic.AtomicBoolean(false)
    /** Last `CT_BOX_HEALTH` bitmask the box pushed. -1 until it has said anything. */
    @Volatile var lastBoxHealth: Int = -1; private set
    /** Last CT_PROJ_MODE seen this session. Advisory — see [Ocbm.pmName]. */
    @Volatile var lastProjMode: Byte = Ocbm.PM_NONE; private set
    @Volatile var framesIn = 0L; private set
    @Volatile var bytesIn = 0L; private set

    val hasMfi: Boolean get() = (caps and Ocbm.CAP_MFI) != 0

    /** Config blob last sent with CT_SUBSCRIBE; re-sent verbatim if the box drops us. */
    @Volatile private var configBlob: ByteArray = ByteArray(0)

    /**
     * True once this client has sent a CT_SUBSCRIBE ([configBlob] is written only by [subscribe]
     * and never cleared). `!subscribed && everSubscribed` is a link in HOST_GONE recovery, whose
     * own re-SUBSCRIBE has a backoff that must not be defeated (2026-10-06).
     */
    val everSubscribed: Boolean get() = configBlob.isNotEmpty()

    // Volatile: handleCtrl reads it on ocbm-read to say whether anything will re-subscribe.
    @Volatile private var heartbeatThread: Thread? = null

    /**
     * Raised by [subscribe] BEFORE [configBlob] is set (a count, so two overlapping calls cannot lower
     * it early), lowered right after `subscribed = true` — or when [subscribe] returns, on the
     * failure paths (v5.6, reviews A2/B-N2; v5.6 fix B-M1).
     * Two readers: the heartbeat's recovery branch, which must not send a second CT_SUBSCRIBE while
     * this one is between setting the blob and setting [subscribed] (once `subscribed` is true the
     * heartbeat takes its subscribed branch first); and [handleCtrl], which drops a box HOST_GONE that
     * arrives meanwhile — it predates this SUBSCRIBE (typically the box's 30 s nudge answering a
     * liveness-probe beat written just before). It is NOT held across the setTime/radio/logCtl writes
     * after the latch: a HOST_GONE there is a real drop and must be forwarded (B-M1).
     */
    private val subscribesInFlight = AtomicInteger(0)
    private val subscribeInFlight: Boolean get() = subscribesInFlight.get() > 0

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
    /** Test view (v5.8 V7 fix1): the heartbeat promoted the HCI wait to MISSED on the injected clock. */
    internal val hciMissed: Boolean get() = hciState == HCI_MISSED

    /**
     * Where decoded `CH_LOG` lines go. Defaults to the client's own logger; [OcbmProbe] points it at
     * the `box` sub-tag so box-side lines stay greppable apart from ours, as they were under the old
     * CH_FILE poller.
     */
    var boxLogger: zeno.gmccpa.ProbeLog.Logger? = null
    private val blog: zeno.gmccpa.ProbeLog.Logger get() = boxLogger ?: log

    /**
     * Backfill de-duplication. Every arm replays each source's WHOLE file from offset 0 (ocbmd
     * main.rs `log_reset` / the offset-0 reopen; lines read in that pass carry [Ocbm.LOG_F_BACKFILL]),
     * so a re-subscribe or a MGMT re-arm pushes the same history again: 860 of 1300 box lines in one
     * capture were `[old]` repeats (2026-10-05). Keyed on (source, the line text) — NOT on the box
     * stamp, because an unstamped line is stamped at the tailer's READ time and so differs on every
     * replay (ocbmd main.rs `decode`/tailer). The ring records LIVE lines too, so the replay of a
     * line first seen live is caught, but only BACKFILL lines are ever suppressed: a live line that
     * repeats is evidence. One summary line per re-arm.
     *
     * One ring per source, bounded in BYTES — the wire UTF-8 length of each line, not `text.length`
     * — to the same size the replay itself is bounded to, so the whole replay is covered: box.log
     * is held to the CT_LOG_CTL cap (`log_enforce_cap`), and the five CH_LOG tail sources that
     * tools/session_supervisor.sh `bound_logs` covers (carplayd, carplayd_wl, iap2d, aa-bridge, wl)
     * are cut to their last 65536 B once past 262144 B, every 30 supervisor ticks — so a file can
     * exceed 256 KiB between passes and a replay can exceed the ring. bt.log, radio_ap_dhcp.log and
     * radio_bt_attach.log are small `>`-truncated status files bound_logs does not touch. Budget per
     * source = max(cap, 256 KiB), taken from [logCapKb] at record time so a later re-arm with a
     * larger cap grows the ring. Residuals: box.log is only rotated once it passes the cap AND
     * nothing is left unstreamed, or at 4x the cap (`log_enforce_cap`), so a replay between cap and
     * 4x cap can exceed the budget; worst-case memory is roughly sources x 256 KiB of text.
     * A line genuinely repeated within one box log file is shown once, even on the first replay:
     * its later copies are BACKFILL and already in the ring (counted as suppressed in the per-arm
     * summary).
     * Touched from the read thread ([handleLog]) and the `ops` thread ([logCtl], [stop]).
     */
    private class BoxLogRing {
        private val order = ArrayDeque<String>()
        /** Wire byte length of each entry in [order], same position. Kept rather than re-encoded:
         *  a [Ocbm.LOG_F_TRUNCATED] or malformed line does not re-encode to its wire length. */
        private val lens = ArrayDeque<Int>()
        private val count = HashMap<String, Int>()
        private var bytes = 0L
        /** Record [line] of [len] wire bytes; true if it was already in the ring. Evicts oldest past [budget]. */
        fun record(line: String, len: Int, budget: Long): Boolean {
            val seen = (count[line] ?: 0) > 0
            count[line] = (count[line] ?: 0) + 1
            order.addLast(line)
            lens.addLast(len)
            bytes += len
            while (bytes > budget && order.isNotEmpty()) {
                val old = order.removeFirst()
                bytes -= lens.removeFirst()
                val n = (count[old] ?: 1) - 1
                if (n <= 0) count.remove(old) else count[old] = n
            }
            return seen
        }
    }
    private val logDedup = Any()
    private val logRings = HashMap<Int, BoxLogRing>()
    private var backfillLines = 0L
    private var backfillSuppressed = 0L

    /** The per-arm summary. Called on each re-arm and at stop; silent when the arm saw no backfill. */
    private fun backfillSummary(why: String) {
        val (total, dropped) = synchronized(logDedup) {
            val r = backfillLines to backfillSuppressed
            backfillLines = 0; backfillSuppressed = 0
            r
        }
        if (total > 0) blog.i("[box] backfill: $total [old] lines this arm, $dropped suppressed as replays of lines already logged — $why")
    }

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
                    log.i("<< SESSION_EVENT ${Ocbm.sevName(sev)}${if (replay) " (replay)" else ""}")
                    if (sev == Ocbm.SEV_HOST_PRESENT) {
                        met(OcbmExpect.HOST_PRESENT)
                        boardUp(OcbmBoard.LINK, linkDetail("subscribed, box confirms HOST_PRESENT"))
                    }
                    // The box has cleared its own subscribed flag and will ignore our heartbeats
                    // until a fresh SUBSCRIBE. Re-arm or the session hangs forever.
                    if (sev == Ocbm.SEV_HOST_GONE) {
                        // Read the blob BEFORE the in-flight flag: subscribe() raises the flag before
                        // it sets the blob, so a non-empty blob read here with the flag down means
                        // that subscribe() had already returned. `subscribed` is cleared only on the
                        // real-drop path below — clearing it in the ignore paths could overwrite a
                        // concurrent subscribe()'s `subscribed = true` (v5.6).
                        val blob = configBlob
                        // Since 2026-09-05 the box also answers a heartbeat from an UNSUBSCRIBED
                        // host with HOST_GONE once every 30 s, as a "re-SUBSCRIBE now" nudge. That
                        // is not a drop, and on the deliberate no-subscribe relink path (a box that
                        // came back under a live Wi-Fi session) re-subscribing is exactly the wrong
                        // answer — it would clear the radio inhibit and re-latch presence. Since
                        // v5.6 that relink's heartbeat beats as a liveness probe, so this nudge is
                        // expected every ~30 s on such a link.
                        if (blob.isEmpty()) {
                            // Before the ACK it is about the previous host's subscription, not this
                            // link (2026-10-06: all four "by choice" lines on record preceded the ACK).
                            if (!helloAcked) log.i("<< HOST_GONE before CT_HELLO_ACK — sent before this link was acknowledged, about the previous host's subscription (its teardown / the box's 10 s watchdog), not this link — ignoring")
                            else if (hostGoneNudgesIgnored++ == 0) log.i("<< HOST_GONE — we hold no subscription (not yet subscribed on this link, or the deliberate no-subscribe relink, whose liveness beats the box nudges every 30 s) — ignoring; further ones on this link are counted, not logged (statsLine nudgesIgnored)")
                            // Ignoring means not forwarding either: the observer chain ends in
                            // onBoxLost ("press Start") and SessionSummary.end("host_gone"), and
                            // three relaunches on 2026-10-06 closed a 2 s "session" that way.
                            return
                        }
                        if (subscribeInFlight) {
                            // v5.6 (A2/B-N2): a beat went out just before subscribe() set the blob and
                            // the box's nudge for it landed just after. Forwarded, it would end the
                            // SessionSummary, report the box lost and drive a second (edgeless)
                            // SUBSCRIBE from the heartbeat. The SUBSCRIBE in flight answers it.
                            log.i("<< HOST_GONE while our CT_SUBSCRIBE is in flight — the box handles frames in order, so it predates that SUBSCRIBE — ignoring")
                            return
                        }
                        subscribed = false
                        // A fault, not a warning: the box's 10 s watchdog expired on us, or
                        // ocbmd restarted underneath the session. Board.failed carries the E
                        // line, and the entry stays FAILED in every status block until the
                        // re-subscribe is answered with HOST_PRESENT. Whether anything re-subscribes
                        // depends on the heartbeat (v5.6 H1-5): once it has declared the link dead
                        // it is gone, and the relink re-claims the link instead.
                        boardFailed(OcbmBoard.LINK, "box reported HOST_GONE while we hold a subscription (its ${Ocbm.HEARTBEAT_GRACE_MS}ms watchdog expired, or ocbmd restarted) — " +
                            if (heartbeatThread?.isAlive == true) "re-subscribing from the heartbeat (2 s backoff, doubling)"
                            else "no heartbeat is running on this link (it already declared the link dead, or the client is stopping) — the relink re-claims it")
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
                val rp = if (replay) " (replay)" else ""
                log.i(if (code.isEmpty()) "<< PAIRING_CODE cleared$rp" else "<< PAIRING_CODE $code$rp  <-- match this on the iPhone")
                if (code.isEmpty()) boardDown(OcbmBoard.PAIRING_CODE, "cleared by box")
                else boardUp(OcbmBoard.PAIRING_CODE, "$code — must match the iPhone's prompt")
                onPairingCode?.invoke(code)
            }
            Ocbm.CT_BOX_HEALTH -> {
                if (pl.size >= 2) {
                    val f = pl[1].toInt() and 0xFF
                    val prev = lastBoxHealth
                    lastBoxHealth = f
                    lastBoxHealthReplay = replay
                    log.i("<< BOX_HEALTH 0x%02x [%s]%s".format(f, Ocbm.bhString(f), if (replay) " (replay)" else ""))
                    noteBoxHealth(prev, f, replay)
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
                log.i((if (json.isEmpty()) "<< PHONE_IDENT cleared" else "<< PHONE_IDENT ${PhoneIdent.redactName(json)}") + (if (replay) " (replay)" else ""))
                // In this role the box only ever clears it (F-C8): CT_PHONE_IDENT is filled from the
                // box's own SETUP, which the app — the AirPlay endpoint here — never sends it. The
                // text says so, so a capture does not read "no phone" as a fault under live CarPlay.
                if (json.isEmpty()) boardDown(OcbmBoard.PHONE, "not identified by the box — expected: the app is the AirPlay endpoint, the box never sees SETUP")
                else boardUp(OcbmBoard.PHONE, phoneSummary(json))
                onPhoneIdent?.invoke(json)
            }
            Ocbm.CT_PROJ_MODE -> {
                if (pl.size >= 2) {
                    val m = pl[1]
                    lastProjMode = m
                    log.i("<< PROJ_MODE ${Ocbm.pmName(m)}${if (replay) " (replay)" else ""}")
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
    private fun noteBoxHealth(prev: Int, f: Int, replay: Boolean) {
        val detail = "0x%02x [%s]".format(f, Ocbm.bhString(f))
        val hci = (f and Ocbm.BH_HCI_PRESENT) != 0
        when {
            hci -> {
                if (hciState != HCI_SEEN) { hciState = HCI_SEEN; hciDeadlineMs = 0L; met(OcbmExpect.HCI_PRESENT) }
                boardUp(OcbmBoard.HEALTH, detail)
            }
            // A replay is the box's mirror after a (re)SUBSCRIBE, not a transition (2026-10-06): it
            // falls through to the budget branches, and subscribe() re-arms the HCI_PRESENT expectation.
            !replay && prev >= 0 && (prev and Ocbm.BH_HCI_PRESENT) != 0 ->
                boardFailed(OcbmBoard.HEALTH, "$detail — hci0 was present and is now GONE (controller dropped, the radios were taken down, or the box's BT probe sample went stale >10 s)")
            hciState == HCI_MISSED ->
                boardFailed(OcbmBoard.HEALTH, "$detail — still no hci0: the box's BT controller never attached, or its BT probe sample is stale >10 s (radio HAL / hciattach — read the [box:bt] lines)")
            else ->
                boardUp(OcbmBoard.HEALTH, "$detail — hci0 not yet attached (BT bring-up in progress, ${((hciDeadlineMs - monoMs()).coerceAtLeast(0)) / 1000}s of budget left)")
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
            log.w("~~ BT phase went BACKWARDS ${Ocbm.btpName(prev)} -> $name while the box reports PROJ_MODE NONE (the box's view only — says nothing about the Wi-Fi CarPlay session this app hosts) — " +
                "the BT/iAP2 handshake reset before a session formed; if PROJ_MODE just went NONE above, this is the tail of a session that ended")
        }
    }

    /**
     * One Board line for the identified phone: model, OS and the BR/EDR `deviceID` that ties it to a
     * `MGMT_INFO` bond, plus the user-given name's LENGTH only (F119: the name itself is personal and
     * the export redactor does not catch it). Hand-scanned, honouring the box's `\"` escaping,
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
            if (name.isNotEmpty()) append(PhoneIdent.placeholder(name)).append(' ')
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
            // Replay suppression — see [BoxLogRing]. Live lines are recorded and always emitted.
            val budget = maxOf(logCapKb.toLong() * 1024, LOG_RING_MIN_BYTES)
            val suppress = synchronized(logDedup) {
                val seen = logRings.getOrPut(source) { BoxLogRing() }.record(text, len, budget)
                if (backfill) { backfillLines++; if (seen) backfillSuppressed++ }
                backfill && seen
            }
            if (suppress) continue
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
        // Close the previous arm's books before the replay this arm triggers starts a new count.
        if (enabled) backfillSummary("re-arming CH_LOG")
        logStreamWanted = enabled
        logCapKb = capKb
        lastLogSeq = -1
        logFlowing = false
        if (enabled) {
            boardUp(OcbmBoard.CH_LOG, "armed cap=${capKb}KiB — no entry yet")
            // Arming replays every source from offset 0, so a live tailer answers at once
            // (same millisecond, device-observed 2026-09-09). Re-arming (subscribe, a MGMT
            // wireless restart) replaces the previous expectation rather than stacking one.
            // Armed BEFORE the write: a stale in-flight line that lands between the write and
            // the arm took the !logFlowing edge and met nothing, and the fresh replay's first
            // line was then a false EXPECTED-MISSING (03:56:06.629/.630 -> 03:56:16.631,
            // 2026-10-06). Now such a line is an early MET instead.
            expect(OcbmExpect.LOG_FIRST_LINE, BOX_REPLY_BUDGET_MS,
                "CT_LOG_CTL on was written; the box replays each log source from offset 0 on arming, so a live tailer produces the first [box:*] entry within milliseconds")
        }
        val pl = byteArrayOf(
            Ocbm.CT_LOG_CTL,
            if (enabled) 1 else 0,
            (capKb and 0xFF).toByte(),
            ((capKb ushr 8) and 0xFF).toByte()
        )
        val ok = send(Ocbm.CH_CTRL, pl)
        if (ok) {
            log.i(">> CT_LOG_CTL ${if (enabled) "on, cap ${capKb}KiB" else "off"}")
            if (!enabled) {
                cancelExpect(OcbmExpect.LOG_FIRST_LINE, "CH_LOG disarmed by host")
                boardDown(OcbmBoard.CH_LOG, "disarmed by host")
            }
        } else {
            log.e(">> CT_LOG_CTL write FAILED")
            if (enabled) {
                cancelExpect(OcbmExpect.LOG_FIRST_LINE, "CT_LOG_CTL write failed")
                boardFailed(OcbmBoard.CH_LOG, "CT_LOG_CTL write failed — box narration will NOT reach this log")
            }
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
     * the first 15 s, then 2 s). Meanwhile [dispatch] still decodes CH_CTRL (this waits only for
     * the ACK entry in ctrlQ), queues CH_MFI/CH_MGMT/CH_FILE, replays CH_LOG and drops only
     * unknown/A-V channels (corrected 2026-10-06; it said every non-ACK frame was discarded).
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
        val deadline = monoMs() + timeoutMs
        val fastUntil = monoMs() + 15_000
        var attempt = 0
        // Same budget as the loop below, so the watchdog and the loop agree to the poll granularity
        // on when "never" is. On a timeout both speak: the loop's line says how many attempts were
        // made, the EXPECTED-MISSING line puts it in the class every other missing step shares.
        expect(OcbmExpect.HELLO_ACK, timeoutMs,
            "CT_HELLO is being written on the claimed bulk OUT (retransmit every 500 ms, then 2 s) — a running ocbmd ACKs in milliseconds, so silence means no daemon, a wedged daemon, or wrong framing")
        while (monoMs() < deadline) {
            attempt++
            // A single failed write must NOT abort bring-up: right after a box reboot the gadget is
            // re-enumerating and the first write(s) can fail while a retry 500 ms later succeeds.
            if (!send(Ocbm.CH_CTRL, hi)) { log.w(">> CT_HELLO write failed (attempt $attempt) — retrying") }
            else if (attempt == 1) log.i(">> CT_HELLO (v${Ocbm.VERSION})")
            val waitMs = if (monoMs() < fastUntil) 500L else 2000L
            val end = monoMs() + waitMs
            while (monoMs() < end) {
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
        val deadline = monoMs() + timeoutMs
        while (monoMs() < deadline) {
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
        // Raised BEFORE the blob is set; lowered once, by the body right after `subscribed = true`,
        // or by the finally on the failure paths — see [subscribeInFlight].
        subscribesInFlight.incrementAndGet()
        var held = true
        val lower = { if (held) { held = false; subscribesInFlight.decrementAndGet() } }
        try { return subscribeInFlightBody(config, lower) } finally { lower() }
    }

    private fun subscribeInFlightBody(config: ByteArray, lowerInFlight: () -> Unit): Boolean {
        configBlob = config
        val pl = ByteArray(1 + config.size)
        pl[0] = Ocbm.CT_SUBSCRIBE
        System.arraycopy(config, 0, pl, 1, config.size)
        // Armed BEFORE the write: the box answers synchronously and the arm won by only >=6 ms
        // in every capture (2026-10-06), so an arm after the write could meet nothing. HCI_PRESENT
        // stays after the write on purpose — armed early, a stale health report could meet it.
        expect(OcbmExpect.HOST_PRESENT, BOX_REPLY_BUDGET_MS,
            "CT_SUBSCRIBE (${config.size}B) was written — ocbmd raises /tmp/host_present and answers SEV_HOST_PRESENT synchronously (2 ms device-observed 2026-09-09)")
        if (!send(Ocbm.CH_CTRL, pl)) {
            log.e(">> CT_SUBSCRIBE write FAILED")
            cancelExpect(OcbmExpect.HOST_PRESENT, "CT_SUBSCRIBE write failed")
            boardFailed(OcbmBoard.LINK, "CT_SUBSCRIBE write failed — the radio-wake edge was never taken")
            return false
        }
        subscribed = true
        // Latch first, then lower (B-M1): the heartbeat now sees `subscribed`, and a HOST_GONE read
        // from here on is taken as about this subscription — forwarded, not held behind the
        // setTime/radio/logCtl writes below until the box's next 30 s nudge. Residual, accepted: a
        // nudge for a beat written just before the SUBSCRIBE that ocbmd only emits after this write
        // completed now reads as a real drop; the heartbeat's recovery re-SUBSCRIBE answers it.
        lowerInFlight()
        log.i(">> CT_SUBSCRIBE (${config.size}B config) — this is the radio-wake edge")
        // The passphrase is echoed by shape only. The value went to logcat in plaintext until
        // 2026-10-06, and logcat leaves the vehicle (export_log_raw, whole-OS capture); the length
        // is enough to spot a blank or truncated entry, which is all the echo was ever for. The SSID
        // likewise since v5.8 V7 (Q014): a Wi-Fi identifier is not for durable output either.
        if (config.isNotEmpty()) config.toString(Charsets.UTF_8).trim().lines().forEach {
            log.i("     | " + when {
                it.startsWith("wifi_pass:") -> "wifi_pass: set (${it.removePrefix("wifi_pass:").trim().length} chars)"
                it.startsWith("wifi_ssid:") -> "wifi_ssid: set (${it.removePrefix("wifi_ssid:").trim().length} chars)"
                else -> it
            })
        }
        boardUp(OcbmBoard.LINK, linkDetail("subscribed, awaiting HOST_PRESENT"))

        // What the box owes us for this edge (HOST_PRESENT is armed above, before the write).
        // Both are re-armed on every subscribe, including the heartbeat's recovery re-subscribe,
        // because the box's state resets with the edge: the supervisor tears wireless down on
        // host loss and brings it up again from here.
        hciState = HCI_PENDING
        hciDeadlineMs = monoMs() + BT_ATTACH_BUDGET_MS
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

    /**
     * Start the 1 Hz beat. Three modes, chosen per tick:
     *  - subscribed: CT_HEARTBEAT, the box watchdog's input and our liveness signal;
     *  - HOST_GONE recovery (a subscription was sent, the box dropped it): re-CT_SUBSCRIBE with
     *    backoff instead of beating;
     *  - never subscribed (the deliberate no-subscribe relink, v5.6 H1-3): CT_HEARTBEAT as a
     *    liveness probe ONLY. It never SUBSCRIBEs, so it never takes a presence edge; the box
     *    answers an unsubscribed beat with a HOST_GONE nudge at most every 30 s, which
     *    [handleCtrl] ignores while no subscription was ever sent.
     * In every mode, 5 consecutive failed OUT writes (beats, or re-SUBSCRIBE writes) declare the
     * link dead — see [heartbeatDeclaresDead].
     */
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
            // v5.6 H1-3: whether this thread has already seen a non-empty configBlob. A heartbeat
            // started in no-subscribe mode can see the blob appear when another thread's subscribe()
            // (rung 1's OcbmProbe.subscribeExisting, or runAllLocked's queued-link path) sets it
            // BEFORE its write and `subscribed` only AFTER. [subscribeInFlight] closes that window
            // exactly; this first-sight deferral is the plan's own guard on top of it, so a first
            // subscribe whose write failed is retried on the backoff schedule, not on the next tick.
            var sawConfig = configBlob.isNotEmpty()
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
                    if (hciDue != 0L && hciState == HCI_PENDING && monoMs() >= hciDue) {
                        hciDeadlineMs = 0L
                        hciState = HCI_MISSED
                        val h = lastBoxHealth
                        boardFailed(OcbmBoard.HEALTH,
                            if (h < 0) "no CT_BOX_HEALTH at all within ${BT_ATTACH_BUDGET_MS}ms of CT_SUBSCRIBE — the box never reported its health"
                            else "0x%02x [%s] — no hci0 within ${BT_ATTACH_BUDGET_MS}ms of CT_SUBSCRIBE: the box's BT controller never attached (radio HAL / hciattach — read the [box:bt] lines)".format(h, Ocbm.bhString(h)))
                    }
                    if (subscribed) {
                        if (configBlob.isNotEmpty()) sawConfig = true
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
                            died = true
                            heartbeatDeclaresDead(writeFailures, "heartbeat")
                            break
                        }
                    } else {
                        // HOST_GONE recovery WITH backoff: re-latching at 1 Hz produces rapid down/up
                        // edges that trip the box supervisor's flap detector (-> ocbmd restart -> full
                        // box reboot). Space attempts out: 2s, 4s, 8s, capped at 15s.
                        stableTicks = 0
                        // Another thread's subscribe() is between setting configBlob and setting
                        // `subscribed` (v5.6 H1-3, the double-SUBSCRIBE race): not our edge to take.
                        if (subscribeInFlight) continue
                        if (configBlob.isEmpty()) {
                            // No config means this client was brought up with subscribe=false on
                            // purpose (the no-subscribe relink under a live session). Beat anyway as a
                            // liveness probe ONLY (v5.6 H1-3): until 2026-10-06 this branch wrote
                            // nothing, so a second box stall after such a relink was undetectable and
                            // a dead CH_MFI surfaced only at the next /auth-setup. A beat from an
                            // unsubscribed host changes no box state (it stamps last_hb and earns a
                            // HOST_GONE nudge at most every 30 s, which handleCtrl ignores here).
                            // Never SUBSCRIBE from here: that is presence behind the caller's back.
                            if (send(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_HEARTBEAT))) writeFailures = 0
                            else if (++writeFailures >= 5) {
                                died = true
                                heartbeatDeclaresDead(writeFailures, "liveness-probe heartbeat")
                                break
                            }
                            continue
                        }
                        val now = monoMs()
                        if (!sawConfig) {
                            sawConfig = true
                            nextResubAt = now + 2000L
                            log.i("a subscription appeared on this link (configBlob set elsewhere) — any recovery re-CT_SUBSCRIBE waits 2 s")
                            continue
                        }
                        if (now >= nextResubAt) {
                            log.i(">> re-CT_SUBSCRIBE (recovering from HOST_GONE, backoff=${resubBackoff}ms)")
                            // The write result counts (v5.6 H1-2): a link that dies in recovery used
                            // to retry forever, because subscribe() returns false on a failed write
                            // without touching any failure count.
                            if (subscribe(configBlob)) writeFailures = 0
                            else if (!running.get()) break
                            else if (++writeFailures >= 5) {
                                died = true
                                heartbeatDeclaresDead(writeFailures, "re-CT_SUBSCRIBE")
                                break
                            }
                            resubBackoff = if (resubBackoff == 0L) 2000L else (resubBackoff * 2).coerceAtMost(15_000L)
                            nextResubAt = now + resubBackoff
                        }
                    }
                }
            } catch (_: InterruptedException) {
                // stop() interrupts this thread. If the interrupt lands while a recovery
                // re-SUBSCRIBE is inside setTime()'s ctrlQ.poll(), or while the dead-link handoff
                // below waits for the transport edge, the exception surfaces here rather than at the
                // sleep above. Deliberate teardown, not a fault — it must not escape run(): no
                // UncaughtExceptionHandler is installed, so Android would kill the process.
            } finally {
                heartbeatThread = null
                if (!died) boardDown(OcbmBoard.HEARTBEAT, if (running.get()) "thread exited on interrupt — restartable" else "client stopped")
            }
        }, "ocbm-heartbeat")
        t.isDaemon = true
        heartbeatThread = t
        t.start()
        log.i(">> heartbeat started at ${Ocbm.HEARTBEAT_MS}ms (box watchdog is ${Ocbm.HEARTBEAT_GRACE_MS}ms)" +
            if (configBlob.isEmpty()) " — liveness probe only, no CT_SUBSCRIBE on this link" else "")
        boardUp(OcbmBoard.HEARTBEAT, "${Ocbm.HEARTBEAT_MS}ms cadence, box watchdog ${Ocbm.HEARTBEAT_GRACE_MS}ms")
    }

    /**
     * The link is presumed dead: [failures] consecutive OUT writes failed ([what] = which writes).
     * Runs on the heartbeat thread, which exits right after. May throw InterruptedException
     * (stop() during the handoff wait); the caller's catch treats that as teardown.
     *
     * v5.6 H1-2. Until 2026-10-06 this synthesised HOST_GONE and nothing else, and on a half-dead
     * link (OUT NAKing, IN timing out normally — logcat_cp4 18:42:52-18:44:07) the read loop never
     * ended, so the transport-death edge and the box relink never ran: the app sat in
     * BOX_LOST_SESSION_UP / "press Start" with no heartbeat and no relink. Now it hands the link to
     * the transport-death path ([RawBulkTransport.declareDead]), whose edge carries
     * `transportDied = true` and arms the relink (SessionCore.onBoxTransportDead CASes
     * [hostGoneDelivered], then arms). HOST_GONE is synthesised here only when that edge does not
     * run: a transport with no read loop to end, or a read thread wedged inside a native transfer.
     */
    private fun heartbeatDeclaresDead(failures: Int, what: String) {
        log.e("!! heartbeat write failed ${failures}x ($what writes) — link presumed dead")
        boardFailed(OcbmBoard.HEARTBEAT, "write failed ${failures}x ($what) — link presumed dead")
        boardFailed(OcbmBoard.LINK, "heartbeat writes failing — box stalled or rebooted, adapter unplugged, or gadget stalled; declaring the link dead")
        // Clear the latch HERE. The edge below bypasses handleCtrl() — the only other place
        // `subscribed` is cleared. Leaving it true stranded the client in a state where the link was
        // dead but every readiness check said "up": the UI told the driver to press Start, and
        // autoStart() then short-circuited on `subscribed == true` with "already subscribed; nothing
        // to do". The one action offered was the one action that could not work; only Stop-then-
        // Start escaped. Device-observed 2026-08-27. It also keeps stop() from sending CT_STOP into
        // a box that is waking up.
        subscribed = false
        // Same reason for the HELLO latch: a dead link must not answer OcbmProbe.hasLink() with "yes".
        helloAcked = false
        val handed = transport.declareDead("heartbeat write failed ${failures}x ($what), read loop alive")
        if (handed) {
            // The read loop exits within one read timeout and its edge CASes the latch. Wait for it
            // (bounded; stop() interrupts this) so the synthetic edge below cannot pre-empt the real
            // one, whose `transportDied = true` is what tells the supervisor a relink is coming.
            val until = monoMs() + 2000L
            while (!hostGoneDelivered.get() && running.get() && monoMs() < until) Thread.sleep(50)
        }
        // A stopped client delivers nothing (B-N3): its owner tore it down on purpose.
        if (!running.get()) return
        if (hostGoneDelivered.compareAndSet(false, true)) {
            // `handed`: the read loop was told to end and has not yet. If it does end later (the
            // wedged transfer returns), its edge still arms the box relink and re-reports the loss
            // as relinking (SessionCore.onBoxTransportDead, reviews A1/B-M2) — so "none yet", not "none".
            log.w(if (handed) "transport-death edge did not arrive within 2 s (read thread wedged in a native transfer?) — HOST_GONE synthesised; no relink is armed unless the read loop ends later"
                  else "no read loop to end on this transport — HOST_GONE synthesised; no relink is armed")
            onSessionEvent?.invoke(Ocbm.SEV_HOST_GONE)
        } else if (handed) {
            log.i("link handed to the transport-death path — its edge delivered HOST_GONE and arms the relink")
        }
    }

    // ---- CH_MFI relay ---------------------------------------------------------------------------

    companion object {
        /** Counts [newHostInstance] calls, so two probes built inside one millisecond still differ. */
        private val HOST_GENERATION = java.util.concurrent.atomic.AtomicInteger(0)

        /**
         * [BoxLogRing] byte floor per source: the 262144 B threshold at which tools/session_supervisor.sh
         * `bound_logs` cuts its five tail sources back to 65536 B (every 30 ticks, so a file can run
         * past it between passes). The CT_LOG_CTL cap raises it when larger — see [BoxLogRing].
         */
        private const val LOG_RING_MIN_BYTES = 262144L

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
     * Serialises CH_MGMT, as [fileLock] does CH_FILE. Both [mgmtGetInfo] and [mgmtAction] clear the
     * one shared [mgmtQ] before they send, and nothing in a reply names its request. Unlocked, a
     * getInfo that started while a FORGET was waiting for its ACK cleared that ACK: the FORGET then
     * reported failure and [reArmAfterMgmt] never ran, so CH_LOG stayed silent (audit12 F057;
     * SessionCore's HOST_GONE end snapshot runs getInfo on a pool thread, concurrent with a forget).
     */
    private val mgmtLock = java.util.concurrent.locks.ReentrantLock(true)

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
        val deadline = monoMs() + timeoutMs
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
                val remaining = deadline - monoMs()
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
        val r = mfiRequest({ t -> Mfi.certRequest(t) }, timeoutMs) { it.size > Mfi.SIG_LEN }
        // Remember a GOOD read of the expected length — and nothing else. A failure, a timeout or
        // an odd-length payload must never be served back as the certificate.
        if (r != null && r.ok && r.payload.size == Mfi.EXPECTED_CERT_LEN) certCache = r.payload
        return r
    }

    /**
     * The certificate this link has already read, if any. Per client on purpose: a re-claim builds
     * a new client, so a swapped adapter can never be served the old chip's certificate. Populated
     * by every OK read of [Mfi.EXPECTED_CERT_LEN] bytes (bring-up included); consumed only by
     * `OcbmProbe.mfiRelay`, which is why a caller that wants the CHIP's answer calls
     * [mfiCertificate] directly. A signature can never be cached — it is over a fresh digest.
     */
    @Volatile private var certCache: ByteArray? = null
    fun cachedCertificate(): ByteArray? = certCache

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
            val deadline = monoMs() + timeoutMs
            while (fileQ.poll() != null) { /* drop anything left by a timed-out pull */ }
            val pathBytes = path.toByteArray(Charsets.UTF_8)
            val req = ByteArray(1 + pathBytes.size)
            req[0] = Ocbm.FILE_PULL
            System.arraycopy(pathBytes, 0, req, 1, pathBytes.size)
            if (!send(Ocbm.CH_FILE, req)) return null

            val body = java.io.ByteArrayOutputStream()
            while (true) {
                val remaining = deadline - monoMs()
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
        if (!mgmtLock.tryLock(timeoutMs, TimeUnit.MILLISECONDS)) {
            log.w("!! CH_MGMT busy — MGMT_GET_INFO could not start within ${timeoutMs}ms")
            return null
        }
        try {
            mgmtQ.clear()
            if (!send(Ocbm.CH_MGMT, byteArrayOf(Ocbm.MGMT_GET_INFO))) return null
            val deadline = monoMs() + timeoutMs
            while (monoMs() < deadline) {
                val pl = mgmtQ.poll(200, TimeUnit.MILLISECONDS) ?: continue
                if (pl.isNotEmpty() && pl[0] == Ocbm.MGMT_INFO) {
                    return String(pl, 1, pl.size - 1, Charsets.UTF_8)
                }
            }
            return null
        } finally {
            mgmtLock.unlock()
        }
    }

    /** Any of the four action verbs; returns the status byte from MGMT_ACK (0 ok, 1 error). */
    fun mgmtAction(verb: Byte, arg: ByteArray = ByteArray(0), timeoutMs: Long = 5000): Int? {
        if (!mgmtLock.tryLock(timeoutMs, TimeUnit.MILLISECONDS)) {
            log.w("!! CH_MGMT busy — verb 0x%02x could not start within ${timeoutMs}ms".format(verb.toInt() and 0xFF))
            return null
        }
        val status: Int? = try {
            mgmtQ.clear()
            val pl = ByteArray(1 + arg.size)
            pl[0] = verb
            System.arraycopy(arg, 0, pl, 1, arg.size)
            if (!send(Ocbm.CH_MGMT, pl)) return null
            val deadline = monoMs() + timeoutMs
            var got: Int? = null
            while (monoMs() < deadline) {
                val r = mgmtQ.poll(200, TimeUnit.MILLISECONDS) ?: continue
                if (r.size >= 3 && r[0] == Ocbm.MGMT_ACK && r[1] == verb) { got = r[2].toInt(); break }
            }
            got
        } finally {
            mgmtLock.unlock()
        }
        // Outside the lock: the re-arm writes CH_CTRL only, and a waiting getInfo need not sit behind it.
        if (status == 0) reArmAfterMgmt(verb)
        return status
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
        backfillSummary("client stopping")
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
            "subscribed=$subscribed nudgesIgnored=$hostGoneNudgesIgnored caps=[${Ocbm.capsString(caps)}]"
}
