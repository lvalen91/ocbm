package zeno.gmccpa

import android.os.SystemClock
import zeno.gmccpa.ocbm.Ocbm
import zeno.gmccpa.logging.SessionTrace
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit

private const val BH_REQUIRED_BRIDGE: Int = Ocbm.BH_HCI_PRESENT or Ocbm.BH_ROOTFS_OK
// The health bits THIS app requires before asking the box to wake its radios. Deployment policy, not
// protocol, which is why it lives here and not in the shared OcbmProto.kt: `hostapd` is deliberately
// not required because in the GM bridge role the head unit owns Wi-Fi and the box raises no AP —
// BH_WLAN_AP being clear is correct here and would be a fault elsewhere. `iap2d` is wired-CarPlay
// only. `rootfs-ok` is included because a full rootfs fails the session later, in ways that look
// like anything but a full disk. A deliberately LOW bar: BH_HCI_PRESENT cannot see a controller that
// was taken down, only one that was never there.


/**
 * Where a wireless CarPlay session actually is, as an explicit value.
 *
 * Before this existed, state was implicit in which objects happened to be non-null and which threads
 * happened to be running — `OcbmProbe.client`, `CarPlayRx.running`, `liveConnections`,
 * `MainActivity.sessionUp`, `CarPlayActivity.generation`. `LinkState` looked like a state machine but
 * is a display label: written from thirteen sites, read by none. The cost of that was not tidiness.
 * It was that the app could not tell apart three situations that need opposite responses:
 *
 *  - the phone has not dialled back YET (wait),
 *  - the phone dialled back and hung up (wait ~10 s, it usually returns),
 *  - the phone can see us, answers our nudge, and will not connect (waiting is useless — escalate,
 *    and eventually tell the driver, because the only fix is on their phone).
 *
 * All three looked identical from inside `rediscoverLoop`, which is why the app's response to every
 * one of them was to redial silently forever.
 */
enum class RxPhase(val label: String) {
    IDLE("idle"),
    /** The readiness probe failed: the receiver cannot accept a session even if a phone arrives. */
    RX_UNHEALTHY("receiver unhealthy"),
    /** The box reported it is missing something it needs. Our side may be perfectly fine. */
    BOX_UNHEALTHY("adapter unhealthy"),
    /** Receiver proven: listening, advertised, native core loadable. Box not up yet. */
    RX_READY("receiver ready"),
    /** OCBM claimed, HELLO + MFi proven. Radios not woken yet. */
    BOX_LINKED("box linked"),
    /** Both sides green and the box told to bring its radios up. Also the recovery hub: inbound/grace expiry and adapter-recovered paths re-enter here and re-arm the ladder. */
    ARMED("armed"),
    BT_PAIRING("pairing"),
    BT_PAIRED("paired"),
    /** `BTP_WIFI_HANDOFF` — the box handed the phone our SSID. A CarPlay connect is now expected. */
    HANDOFF_SENT("handoff sent"),
    /** The phone accepted `GET /ctrl-int/1/connect`. It should now dial us back. */
    INBOUND_EXPECTED("awaiting dial-back"),
    SESSION_UP("CarPlay live"),
    /** The session ended and we are holding still to let the phone come back on its own. */
    GRACE("session ended — waiting"),
    /** The box went away while CarPlay was streaming. Deliberately do nothing. */
    BOX_LOST_SESSION_UP("box gone, session live"),
    /** Discovered, answering, refusing to connect. Needs the driver. */
    STALLED("stalled");

    /**
     * The phases that mean "something is wrong", as opposed to "not there yet". A transition INTO one
     * of these is logged at E with its detail and put on the Board as FAILED, so `grep ' E NETPROBE'`
     * finds it; every other phase is narration. [STALLED] is the terminal "the driver must act"
     * state and used to read like any other line.
     */
    val isFailure: Boolean get() = this == RX_UNHEALTHY || this == BOX_UNHEALTHY || this == STALLED
}

/**
 * What the CarPlay adapter is doing, for stall/skip text that is true in every state (2026-10-06).
 * Before this, one sentence ("no link to the CarPlay adapter — check it is plugged in") covered
 * every unlinked state, including an adapter on the bus waiting for USB access. Read via
 * [SessionSupervisor.Actions.adapterState]; true when evaluated, and it can change before display.
 */
class AdapterState private constructor(val name: String, val text: String) {
    override fun toString() = name
    companion object {
        val ABSENT = AdapterState("ABSENT", "no CarPlay adapter on USB — check it is plugged in")
        fun wrongMode(pid: Int) = AdapterState("WRONG_MODE(0x%04x)".format(pid),
            "the Carlinkit adapter is on USB in mode 0x%04x, not the CarPlay mode (0x2d00) — it needs ocbm_boot.sh".format(pid))
        /** No grant, a claim wait is running and its dialog request is not deferred (incl. the <= 2 s before the first). */
        val NO_PERMISSION_PROMPTED = AdapterState("NO_PERMISSION_PROMPTED",
            "the CarPlay adapter is plugged in and waiting for USB access — accept the USB prompt on screen")
        val NO_PERMISSION_DEFERRED = AdapterState("NO_PERMISSION_DEFERRED",
            "the CarPlay adapter is plugged in; USB access will be requested when CarPlay ends (or in Park, if not yet asked since it was plugged in)")
        /** No grant and no claim wait running (it gave up after 10 min, or never started). */
        val NO_PERMISSION_IDLE = AdapterState("NO_PERMISSION_IDLE",
            "the CarPlay adapter is plugged in but this app has no USB access — press Start to request it")
        val LINKING = AdapterState("LINKING", "the CarPlay adapter is plugged in and connecting — wait a few seconds")
        val NOT_LINKED = AdapterState("NOT_LINKED", "the CarPlay adapter is plugged in but not connected — press Start")
        /** True as advice: Start -> autoStart -> runAll subscribes. */
        val LINKED_UNSUBSCRIBED = AdapterState("LINKED_UNSUBSCRIBED", "the CarPlay adapter is connected but not armed — press Start")
        val SUBSCRIBED = AdapterState("SUBSCRIBED", "the CarPlay adapter is connected and armed")
    }
}

/**
 * Which side did not come back when the supervisor gave up into [RxPhase.STALLED] (plan v5.9 rev c
 * C4). The phase alone cannot tell them apart; each `toStalled` caller knows.
 */
enum class StallCause { PHONE, ADAPTER }

/**
 * Owns [RxPhase] and the recovery ladder.
 *
 * Everything runs on one scheduler thread, so transitions and timers cannot race each other and no
 * field here needs locking. Callers post events; the supervisor decides. It never blocks: the actions
 * it invokes are expected to hand off (they are dispatched onto the caller's own executor).
 *
 * [clock] times every cooldown, settle window and STALLED age (v5.7 F008). It is monotonic
 * (`SystemClock.elapsedRealtime`), not the wall clock: the bench time-sync hook sets the radio's clock
 * with `cmd alarm set-time` whenever it drifts > 90 s, and a backwards jump of an hour on the wall
 * clock read as "rung N ran an hour in the future" and stretched its cooldown by the jump. The stamps
 * it produces (lastRungAt, radioSettleUntil, stalledAt) never leave this class.
 *
 * The internal constructor's [sched] exists for the host-JVM tests only (a virtual-time scheduler);
 * production always goes through the public constructor and gets the single daemon thread.
 */
class SessionSupervisor internal constructor(
    private val act: Actions,
    private val clock: () -> Long,
    private val sched: ScheduledExecutorService,
) {

    constructor(act: Actions, clock: () -> Long = { SystemClock.elapsedRealtime() }) :
        this(act, clock, Executors.newSingleThreadScheduledExecutor { r -> Thread(r, "rx-supervisor").apply { isDaemon = true } })

    /**
     * Everything the supervisor can do to the world. An interface rather than direct calls so the
     * decision logic has no dependency on the Activity, the receiver, or the OCBM client.
     */
    interface Actions {
        /** Is the OCBM link to the box actually usable right now? See [runRung]. */
        fun boxLinkAlive(): Boolean
        /**
         * Rung 0: re-assert our mDNS records. Cheap, no box involvement, no session disturbance.
         *
         * Returns false when the responder was not actually up, so nothing was announced. A rung that
         * did nothing must not be reported as an action taken — see [runRung].
         */
        fun reannounce(): Boolean
        /** Rung 1: `CT_RADIO`. Cycles the box's radios WITHOUT a host_present edge. */
        fun setBoxRadios(on: Boolean): Boolean
        /** Rung 2: `MGMT_RESTART_WIRELESS`. Drops the BT link, keeps the bond. ~4 s. */
        fun restartBoxWireless(): Boolean
        /** Stop the 12 s rediscover churn so it cannot preempt a deliberate wait. */
        fun pauseDiscovery()
        fun resumeDiscovery()
        /**
         * The phase lever for the receiver's rediscover backoff: true only in the phases where
         * nothing more is expected from the phone (STALLED, BOX_UNHEALTHY). Every call also resets
         * the cadence, so relaying it on BT progress restarts the count. See `CarPlayRx.setDiscoveryBackoff`.
         */
        fun setDiscoveryBackoff(enabled: Boolean)
        /** The hotspot SSID the box was handed for the 0x5703 handoff, for the terminal text. Never the passphrase. */
        fun hotspotSsid(): String?
        /** What the adapter is doing now (52C), for the terminal and rung-skip text. Non-blocking. */
        fun adapterState(): AdapterState
        /**
         * The box link was restored under the last session WITHOUT a subscribe and has never
         * subscribed (52B): rung 1 is then a deferred CT_SUBSCRIBE instead of CT_RADIO.
         */
        fun boxLinkedUnsubscribed(): Boolean
        /** Rung 1's deferred CT_SUBSCRIBE on the existing link. Dispatched; the return is the dispatch verdict. */
        fun subscribeBox(): Boolean
        /**
         * Surface phase + a human sentence. [stall] is set on every report INTO [RxPhase.STALLED]
         * (plan v5.9 rev c C4): which side failed to come back, for the driver line and the status
         * notification. Null for every other phase.
         */
        fun report(phase: RxPhase, detail: String, stall: StallCause? = null)
    }

    private companion object {
        /**
         * How long to hold still after a session ends before treating it as a failure.
         *
         * The phone very often comes straight back (an app switch, a brief radio glitch). Reacting
         * instantly would tear down a session that was about to resume. `rediscoverLoop` fires every
         * 12 s, so this window is deliberately shorter than that AND discovery is paused across it —
         * otherwise the churn preempts the wait and redials into the gap.
         */
        const val GRACE_MS = 10_000L
        /**
         * From the phone accepting our nudge to giving up on the dial-back.
         *
         * Distinct from [GRACE_MS] on purpose. "Never dialled back" and "dialled back then left" are
         * different faults with different fixes, and collapsing them into one timer is what made the
         * app blind to the stall.
         */
        const val INBOUND_TIMEOUT_MS = 15_000L

        /**
         * From the box saying it handed the phone our SSID to giving up on hearing from the phone.
         *
         * Without this, ARMED/BT_PAIRING/BT_PAIRED/HANDOFF_SENT carried no deadline at all: if the
         * box hung in Bluetooth bring-up, or sent the handoff and the phone then did nothing, no
         * clock was ever started and the ladder could not begin. Generous, because a healthy handoff
         * to a first inbound connection was measured at 6-9 s and a slow one should not be punished.
         */
        const val HANDOFF_TIMEOUT_MS = 45_000L

        /**
         * How long after trying a rung before trying the next one.
         *
         * The ladder MUST drive itself. It used to run exactly one rung per external event, and the
         * only events that could re-enter it were "the phone accepted a dial" and "a session ended" —
         * neither of which happens when the phone simply goes silent, which is the commonest failure
         * of all. So rung 0 ran, `rung` advanced to 1, and nothing ever called escalate() again:
         * CT_RADIO, MGMT_RESTART_WIRELESS and the terminal STALLED report were all unreachable, and
         * the UI sat on "session ended — waiting" forever. Exactly the silent-forever behaviour this
         * class exists to end.
         */
        const val LADDER_RECHECK_MS = 20_000L

        /**
         * How long after a rung-1 action (CT_RADIO off/on, or the deferred CT_SUBSCRIBE) before
         * rung 2 (2026-10-06). Measured on P1: rung 1 -> iPhone BT 19.6 s, -> BT_PHASE at the app
         * 20.0 s, 6 s of it the box paging a stale bond — so a 20 s recheck lands INSIDE rung 1's own
         * effect window, and rung 2 (a btd bounce) then broke the handshake already in progress (R1).
         * Any BT_PHASE progress stands the ladder down earlier ([deferLadder]). [LADDER_RECHECK_MS]
         * still holds after rung 0.
         */
        const val RUNG1_RECHECK_MS = 35_000L

        /**
         * How long after a rung-2 action (MGMT_RESTART_WIRELESS) before the ladder is declared
         * exhausted (v5.6 H2, 2026-10-06). After [LADDER_RECHECK_MS] (20 s) STALLED landed before the
         * reconnect rung 2 itself sets off: the restarted wireless stack pages the bonded phones on
         * its own, after a 5 s page timeout on any stale bond. Measured rung 2 -> BT AUTH: P1 17.1 s,
         * P4b 30.6 s (STALLED at +20.0 s, AUTH at +30.6 s). 45 s = the 30.6 s maximum + ~14 s (one
         * more page timeout, and the 12.5 s spread in rung 2 -> reconnect start between the two
         * runs). BT progress still stands the ladder down earlier ([deferLadder]). The rung-2 health
         * settle window is stretched to match (see [runBoxRung]), or a regression 20-45 s in would
         * reach STALLED by the health route instead (review B-N4).
         */
        const val RUNG2_RECHECK_MS = 45_000L

        /**
         * Minimum spacing per rung. These are not politeness — they are correctness.
         *
         * There is no rung 3 action: `runRung` returns false past rung 2 and the ladder falls to
         * STALLED, so this array has exactly one entry per rung that exists (`lastRungAt` and the
         * `escalate` loop bound are both sized from it; a 4th entry was dead — 2026-09-10). The spacing matters because rungs 1–2 touch the box: a `host_present` cycle inside
         * ~20 s reads as flapping to the box supervisor, which escalates to an `ocbmd` restart and then
         * a full box reboot, so a ladder that retried freely would brick the session it was trying to
         * rescue. Rung 1 exists precisely because `CT_RADIO` resets the box's radios WITHOUT touching
         * host_present, so it is the strongest lever that carries no flap risk at all.
         */
        val RUNG_COOLDOWN_MS = longArrayOf(30_000L, 90_000L, 180_000L)

        /**
         * How long after rung 1 a box-health regression is presumed to be our own doing.
         *
         * Covers the whole cycle the rung sets in motion: the off-edge, the 2 s gap before the
         * on-edge, and the box's controller re-attaching over HCI afterwards. Health necessarily
         * regresses across that — the rung's entire purpose is to take the radios down — so treating
         * it as a fresh fault fed rung 1 straight into rung 2, which is the feedback loop that made
         * `MGMT_RESTART_WIRELESS` look inevitable.
         */
        const val RADIO_SETTLE_MS = 10_000L

        /**
         * Same idea as [RADIO_SETTLE_MS], for the driver's Restart adapter Bluetooth action (`MGMT_RESTART_WIRELESS`).
         *
         * Longer because the chain is longer: ocbmd ACKs at once and only writes a flag; the box
         * supervisor picks it up on its next tick (~1 s), runs `wireless_down`, waits 4 s, then
         * `wireless_up`, and the BT controller re-attaches over HCI ~8 s after that — so
         * `BH_HCI_PRESENT` returns ~12 s after the press (tools/session_supervisor.sh, the
         * `WIRELESS_RESTART_FLAG` / `wireless_rebring_at` pair). The regression lands inside that
         * window and belongs to the button, not to a fault. Without this claim the ladder climbed on
         * top of the box's own restart — the app answering a restart with a second restart.
         */
        const val WIRELESS_RESTART_SETTLE_MS = 20_000L
    }

    private val log = ProbeLog.sub("sup")

    @Volatile private var phase = RxPhase.IDLE
    /** Public read for callers that need the current phase for a log line or the UI. */
    val current: RxPhase get() = phase

    /**
     * Five independent deadlines, not one slot.
     *
     * A single `timer` field was wrong in a way that only shows up under load: rung 1 schedules the
     * `CT_RADIO` on-edge 2 s after the off-edge, and with one slot that schedule silently cancelled
     * whatever deadline was already pending — typically the very grace or dial-back timer whose
     * expiry had just triggered the escalation. The box's radios would then be left INHIBITED with
     * nothing left to turn them back on, which is a worse state than the one being recovered from.
     */
    private var graceTimer: ScheduledFuture<*>? = null
    private var inboundTimer: ScheduledFuture<*>? = null
    private var ladderTimer: ScheduledFuture<*>? = null
    /** Drives the ladder forward on its own; see [LADDER_RECHECK_MS]. */
    private var retryTimer: ScheduledFuture<*>? = null
    /** Deadline on the box-side phases, which otherwise carry no clock. See [HANDOFF_TIMEOUT_MS]. */
    private var handoffTimer: ScheduledFuture<*>? = null
    private var rung = 0
    private val lastRungAt = LongArray(RUNG_COOLDOWN_MS.size)
    /** Set while the box is absent and a CarPlay session is up, so the relink knows to hold BT down. */
    private var boxLostMidSession = false
    /** Last `CT_BOX_HEALTH` bitmask the box pushed; -1 = it has not said anything yet. */
    private var boxHealth = -1
    /** True between rung 1's radios-OFF and the paired radios-ON. See [armRadioOnEdge]. */
    private var radioOnPending = false
    /** Health regressions before this instant are rung 1's own doing. See [RADIO_SETTLE_MS]. */
    private var radioSettleUntil = 0L
    /**
     * True while radios are held down *because a session is live* (set by [onBoxRelinked]).
     *
     * Distinct from [radioOnPending], which is the 2 s ladder edge. This inhibit is deliberately
     * open-ended — it lasts as long as the session it is protecting — so the release has to be tied to
     * the session ending rather than to a timer, or the box would be left with Bluetooth off and the
     * next connection attempt could never begin.
     */
    private var radiosHeldForSession = false
    /**
     * The 45 s handoff watchdog expired while the phone was past iAP2 identification (BT_PAIRED or
     * HANDOFF_SENT) and no CarPlay connect followed. That shape has one likely cause the driver can
     * act on — the phone was handed an SSID/passphrase it could not join — and the terminal text
     * names it only when this latch is set. Cleared by a real session, a stop or a Restart Session.
     */
    private var handoffExpiredAfterBt = false
    /**
     * A session actually formed since the last stop ([onSessionUp]); read and cleared by
     * [onSessionDown] so its GRACE text can tell "the session ended" from "the control connection
     * closed before one formed". Keyed on this latch and NOT on the phase, because
     * [onReceiverReady] (ok=false) is deliberately unguarded and can leave SESSION_UP first. Also
     * cleared by [onStopped], which fences the receiver's session-down report on every teardown.
     */
    private var sessionSeen = false

    /**
     * STALLED-by-skip (2026-10-06). Every transition INTO STALLED assigns these three; every `to()`
     * into any other phase clears them, as do onSessionDown/onSessionUp/cancelRecovery. [skippedAtRung]
     * is 1 only when the pass that stalled skipped rung 1 for want of a usable link (no link, a link
     * in HOST_GONE recovery, or a refused subscribe dispatch). [onBoxSubscribed] honours it ONCE, and
     * only while the STALLED is younger than RUNG_COOLDOWN_MS[2]: a later heartbeat re-SUBSCRIBE must
     * not re-enter the ladder at rung 2 minutes later with a stale reason against a phone that left.
     * A skip at rung 2 alone is deliberately not generalised: R1 shows rung 2 can break a handshake.
     */
    private var skippedAtRung = 0
    private var stalledWhy: String? = null
    private var stalledAt = 0L
    /** The [StallCause] of the current STALLED (null outside it); read by [onRelinkEnded]. */
    private var stalledCause: StallCause? = null
    /** Set by [runRung] when rung 1 was skipped for want of a usable link; read by [escalate]. */
    private var rung1Skipped = false
    /**
     * The outcome of this ladder's latest rung-1 attempt: true when it was skipped for want of a
     * usable link. A field, not an [escalate] local (v5.7 F010): a pass that skips rung 1 and then
     * finds rung 2 cooling returns to wait, and the pass that later exhausts the ladder starts at
     * rung 2 — a local was false there, so [toStalled] lost the skip and the honour path in
     * [onBoxSubscribed] was never taken. Cleared at every `rung = 0` site and when that honour path
     * consumes the record.
     */
    private var skipped1 = false

    // ---- event intake ---------------------------------------------------------------------------
    // All of these are safe to call from any thread; they only queue work onto the scheduler.

    /**
     * The receiver's readiness probe reported.
     *
     * `ok=true` may only move the machine FORWARD into [RxPhase.RX_READY]. That phase means "receiver
     * proven, box not up yet", and every later phase already has the receiver proven — so from any of
     * them a good report is confirmation, not a transition. Re-entering RX_READY from a later phase
     * asserted that the box was down when it was not, and `to()` acts on that assertion: it RESUMES
     * discovery, so the 12 s rediscover loop redialled a live control connection (the hijack
     * [onBtPhase] documents), and [escalate]'s SESSION_UP guard no longer saw SESSION_UP, so rung 0
     * ran and rung 1 cycled the box's radios 20 s later under streaming CarPlay. In GRACE the same
     * drop threw away the hold the grace window exists to provide. The caller that used to reach here
     * with a live receiver was the Recover button, which re-ran the probe first — so pressing Recover
     * during a live session knocked the session over. Recover was merged into Restart Session (plan
     * v5.9 UI-1), which builds a fresh receiver after [onOperatorRestart]'s IDLE instead.
     *
     * `ok=false` is deliberately NOT guarded by phase: a listener that retired under a live session is
     * a real fault the phase must reflect, whatever it was. It IS guarded by [stillCurrent] (v5.7.1),
     * evaluated here on the scheduler exactly as [onSessionUp] does: a receiver's listener-retired
     * edge (SessionCore.bindReceiver) can land while carplay_stop is retiring it, and that receiver's
     * `listening` FAIL would otherwise land as RX_UNHEALTHY after onStopped's IDLE.
     *
     * v5.7 F029: the box link outlives a receiver restart (FORGET, carplay_stop -> carplay_rx), and the
     * box sends HOST_PRESENT only on a fresh subscribe, so nothing would move RX_READY on. With the
     * adapter already subscribed the receiver being ready completes "both sides green": ARMED, which
     * carries no clock of its own by design.
     */
    fun onReceiverReady(ok: Boolean, detail: String, stillCurrent: () -> Boolean = { true }) = post {
        if (!stillCurrent()) {
            log.i("ignoring readiness from a receiver that is being stopped or was replaced")
            return@post
        }
        if (!ok) { to(RxPhase.RX_UNHEALTHY, "receiver NOT ready: $detail"); return@post }
        when (phase) {
            RxPhase.IDLE, RxPhase.RX_UNHEALTHY, RxPhase.RX_READY -> {
                to(RxPhase.RX_READY, detail)
                if (runCatching { act.boxLinkAlive() }.getOrDefault(false))
                    to(RxPhase.ARMED, "receiver ready; the adapter is already subscribed — waiting for the iPhone")
            }
            else -> log.i("receiver re-checked OK while ${phase.label} — the phase stands ($detail)")
        }
    }

    /**
     * The box is linked. [mfiProven] must be the OBSERVED outcome of a real cert+signature exchange,
     * never an assumption: this label used to read "MFi proven" unconditionally, on a path the caller
     * reached even when no adapter was ever claimed.
     */
    fun onBoxLinked(mfiProven: Boolean) = post {
        linkForward(if (mfiProven) "box claimed, MFi proven" else "box claimed — MFi NOT proven")
    }

    /**
     * A box link report may only move the machine FORWARD into [RxPhase.BOX_LINKED], the same rule
     * [onReceiverReady] applies to RX_READY. Every later phase already has the box linked — on an
     * ordinary bring-up this is posted from the command thread AFTER `runAll` returns, by which time
     * SEV_HOST_PRESENT has already moved us to ARMED, so relabelling walked ARMED back to BOX_LINKED
     * on every start (5 instances on 2026-10-06) and [onRestartSubscribed]'s 45 s expiry then read
     * "the adapter never confirmed the host present" when it had. Under a live session the same
     * relabel armed a watchdog and ran the ladder's box-side rungs against streaming CarPlay.
     * [RxPhase.BOX_UNHEALTHY] is deliberately excluded: health owns that exit.
     */
    private fun linkForward(detail: String) {
        when (phase) {
            RxPhase.IDLE, RxPhase.RX_UNHEALTHY, RxPhase.RX_READY, RxPhase.BOX_LINKED -> to(RxPhase.BOX_LINKED, detail)
            else -> log.i("box link confirmed while ${phase.label} — the phase stands ($detail)")
        }
    }

    /**
     * The box's own readiness, pushed rather than polled.
     *
     * This is the second half of "both sides green": [onReceiverReady] proves we can accept a session,
     * this proves the box can produce one. Neither was previously knowable without asking, and nothing
     * asked after bring-up — so a box whose Bluetooth controller went away mid-session was
     * indistinguishable from a healthy one.
     *
     * Only reported, never used to refuse a session outright: the bits are advisory by protocol
     * contract, and an app that declines to try because it disliked a bitmask is worse than one that
     * tries and reports the failure.
     */
    fun onBoxHealth(flags: Int, replay: Boolean = false) = post {
        val was = boxHealth
        boxHealth = flags
        val missing = BH_REQUIRED_BRIDGE and flags.inv()
        if (missing == 0) {
            // Green proves only that the HEALTH fault is gone. It says nothing whatever about a phone
            // that will not dial back, so it may only cancel a recovery that health itself started.
            // Cancelling unconditionally killed retryTimer while the real fault stood, and left the
            // machine with no deadline anywhere: an unbounded dead window, not the 48.5 s once
            // measured — that number is only how long the phone happened to take to rescue us.
            if (phase == RxPhase.BOX_UNHEALTHY) {
                cancelRecovery()
                to(RxPhase.ARMED, "adapter healthy again — waiting for the phone")
                // ARMED carries no clock of its own; entering it without one is how the machine went
                // quiet with nothing pending.
                armHandoffWatchdog()
            }
            return@post
        }
        // The FIRST report is a baseline, never a regression.
        //
        // The box's health tick fires within 2 s of SUBSCRIBE, and SUBSCRIBE is itself the edge that
        // makes the box's supervisor run wireless_up and attach the BT controller. So on a cold box
        // the first sample reliably lands with the controller not yet present. Treating an absent
        // baseline as "everything was fine before" turned that ordinary bring-up race into a fault,
        // and the ladder then climbed on it 20 s into a perfectly healthy pairing.
        //
        // A REPLAY is a baseline too (2026-10-06): after a heartbeat re-SUBSCRIBE the box re-sends its
        // latched health mirror (envelope F_REPLAY), and an HCI-clear there read as "regressed" ->
        // BOX_UNHEALTHY E + ladder. Two consequences, accepted: (1) an HCI that never returns after a
        // re-subscribe now gets no supervisor ladder (ARMED has no clock; onBoxSubscribed arms none) —
        // before this it escalated to rung 1 at +20 s; only OcbmClient's 45 s HCI_PRESENT expectation
        // and its Board FAILED remain, the same gap as the cold baseline above. (2) The box sends
        // health only on change, so a replayed standing fault is never re-evaluated by a later report.
        if (was < 0 || replay) {
            log.i(if (was < 0) "first box health 0x%02x [%s] — baseline, not evaluated".format(flags, Ocbm.bhString(flags))
                else "box health 0x%02x [%s] is the box's replayed mirror after a (re)SUBSCRIBE — baseline, not evaluated".format(flags, Ocbm.bhString(flags)))
            return@post
        }
        // Otherwise act only on a CHANGE into the unhealthy set, or a standing fault would re-trigger
        // the ladder every couple of seconds.
        val wasMissing = BH_REQUIRED_BRIDGE and was.inv()
        if (missing == wasMissing) return@post
        val detail = "adapter is missing ${Ocbm.bhString(missing)} (has ${Ocbm.bhString(flags)})"
        // Rung 1 and the driver's Restart adapter Bluetooth both take the box's radios down and back up; the
        // controller going missing across that edge is the action working, not a new fault. See
        // [RADIO_SETTLE_MS] and [WIRELESS_RESTART_SETTLE_MS].
        if (clock() < radioSettleUntil) {
            log.i("box health regressed inside a radio cycle we started (rung 1 or Restart adapter Bluetooth) — self-inflicted — not acting ($detail)")
            return@post
        }
        log.w("box health regressed: $detail")
        when (phase) {
            // Do NOT disturb a live session over this. The A/V path is Wi-Fi and does not care that
            // the box's Bluetooth went away. GRACE is here for the same reason inverted: acting there
            // destroys the hold the grace window exists to protect, and rung 1 demonstrably CAUSES
            // health regressions, so the two together are a loop with a session in the middle.
            RxPhase.SESSION_UP, RxPhase.BOX_LOST_SESSION_UP, RxPhase.GRACE -> log.i("session is live or in grace — noting the box fault, not acting on it")
            else -> {
                to(RxPhase.BOX_UNHEALTHY, detail)
                escalate(detail)
            }
        }
    }

    fun onBoxSubscribed() = post {
        // SEV_HOST_PRESENT can land under a session that went live during bring-up (the live
        // check in autoStart precedes runAll by the whole bring-up) — `CarPlay live -> armed` at
        // 03:58:21 and 04:20:46 on 2026-10-06, after which the handoff watchdog and rungs 0-2 ran
        // under streaming CarPlay. The subscribe itself stands; only the phase must not move.
        if (phase == RxPhase.SESSION_UP || phase == RxPhase.BOX_LOST_SESSION_UP) {
            log.i("box confirmed HOST_PRESENT while CarPlay is live — the phase stands, nothing armed")
            return@post
        }
        // v5.7 F002: GRACE and INBOUND_EXPECTED each own a deadline (the grace window, the dial-back
        // clock) whose expiry escalates. A heartbeat re-SUBSCRIBE landing inside them used to move the
        // phase to ARMED, where neither timer's body acts, leaving ARMED with no deadline at all.
        if (phase == RxPhase.GRACE || phase == RxPhase.INBOUND_EXPECTED) {
            log.i("box confirmed HOST_PRESENT while ${phase.label} — that phase's own timer owns the clock; the phase stands")
            return@post
        }
        if (phase == RxPhase.STALLED) {
            // Consumed before acting, whatever the outcome: honoured once.
            val now = clock()
            val honour = skippedAtRung == 1 && now - stalledAt < RUNG_COOLDOWN_MS[2]
            val why = stalledWhy
            skippedAtRung = 0; stalledWhy = null; stalledAt = 0L
            if (honour && why != null) {
                // The subscribe that rung 1 could not send has now happened: count it as rung 1.
                lastRungAt[1] = now
                rung = 2
                // The record is consumed: rung 1 now counts as having run. Left set, a rung-2 skip in
                // this pass would re-stall with skippedAtRung=1 and every later re-SUBSCRIBE would
                // honour it again.
                skipped1 = false
                log.i("box subscribed after the ladder skipped rung 1 for no link — counting it as rung 1; rung 2 in ${RUNG1_RECHECK_MS / 1000}s unless the iPhone makes progress")
                retryTimer = arm(retryTimer, RUNG1_RECHECK_MS) { escalate(why) }
            }
        }
        // A fresh subscription from a pre-link phase (Start, or a re-subscribe after the box was
        // lost: IDLE is reached only by box loss or Stop) begins a new box state — the box tore its
        // wireless down with the old one and replays its health as a baseline. An earlier rung-2 /
        // Restart adapter Bluetooth settle claim owns nothing here; drop it (B-M3). Not from STALLED/ARMED/
        // BOX_UNHEALTHY, where a ladder that may have just run rung 2 is still in charge.
        if (phase == RxPhase.IDLE || phase == RxPhase.RX_UNHEALTHY || phase == RxPhase.RX_READY || phase == RxPhase.BOX_LINKED) radioSettleUntil = 0L
        // to() does not touch retryTimer, so a timer armed above survives the transition.
        to(RxPhase.ARMED, "box radios waking — waiting for the phone")
    }

    /**
     * The OCBM link went away.
     *
     * If CarPlay is streaming this is explicitly NOT a session-ending event: the A/V path runs over
     * Wi-Fi and does not depend on Bluetooth or on the box once it is up. Tearing the session down
     * here would be the app destroying a working session because a side-channel blinked.
     */
    fun onBoxLost(relinking: Boolean = false) = post {
        // BOTH live-session phases, not just SESSION_UP: two independent producers deliver
        // SEV_HOST_GONE -- the box over CH_CTRL, and the link-death path (SessionCore's transport-
        // death edge, onBoxTransportDead, which since v5.6 the heartbeat's write failures also reach, or the heartbeat's
        // fallback synthesis), which bypasses handleCtrl (OcbmClient). An ocbmd
        // restart while USB stays enumerated fires both: the first correctly parks us in
        // BOX_LOST_SESSION_UP, and the second used to fall through to the else branch and drop a
        // LIVE session to IDLE. That resumes discovery mid-session AND re-qualifies the box-health
        // handler, whose guard list names only these same phases -- so the next health regression
        // climbed the ladder into CT_RADIO off/on against streaming CarPlay. Idempotent instead.
        if (phase == RxPhase.SESSION_UP || phase == RxPhase.BOX_LOST_SESSION_UP) {
            boxLostMidSession = true
            to(RxPhase.BOX_LOST_SESSION_UP, "box gone but CarPlay is streaming — holding the session")
        } else if (phase == RxPhase.GRACE) {
            // v5.7 F002: the grace window still owns the clock. Dropping to IDLE made its expiry a
            // no-op (it acts only in GRACE) after onSessionDown had already cancelled the ladder's
            // retry, so a later re-subscribe parked the machine in ARMED with no deadline anywhere.
            // On expiry the ladder runs; rungs 1 and 2 skip themselves without a subscribed link and
            // the terminal text names the adapter's state.
            // v5.7w1 (R3): GRACE is tested before `relinking`, so a transport death in grace must
            // carry the relink's USB-prompt hint itself — the phase and its clock stay as above.
            to(RxPhase.GRACE, if (relinking)
                "the adapter link died during the grace window — re-claiming it (accept the USB prompt if the adapter was replugged); still holding for the iPhone"
            else "adapter link lost during the grace window — still holding for the iPhone")
        } else {
            // B-F1 (2026-10-08): a ladder retry armed before the transport died used to survive this
            // transition — to() does not touch retryTimer — and fired in IDLE while the relink was
            // still re-claiming: rungs 1 and 2 skipped themselves (no link), the ladder exhausted into
            // STALLED with the adapter's "connecting — wait a few seconds" text, and a retry that
            // outlived a SUCCESSFUL relink fired rung 2 (MGMT_RESTART_WIRELESS) 27 s into the fresh
            // link. The ladder restarts from the bottom for the box's return, as [onSessionDown] does
            // for the phone's; the per-rung cooldown stamps are kept. GRACE above is untouched: its
            // expiry runs the ladder on purpose.
            retryTimer?.cancel(false); retryTimer = null
            rung = 0
            skipped1 = false
            if (relinking) {
                // The transport died — the adapter left the bus, or (v5.6) the heartbeat declared a
                // half-dead link dead with the adapter still enumerated — and the box relink is
                // re-claiming it (BoxRelink, armed by SessionCore.onBoxTransportDead): no Start needed;
                // a replug needs the USB prompt.
                to(RxPhase.IDLE, "the adapter link died — re-claiming it (accept the USB prompt if the adapter was replugged)")
            } else {
                // v5.7 F013: the heartbeat re-subscribes by itself (OcbmClient), so "press Start" is only
                // the fallback, not the instruction.
                to(RxPhase.IDLE, "the adapter dropped the host — the link re-subscribes on its own; press Start if it stays here")
            }
        }
    }

    /**
     * The box relink ended without a link (2026-10-06): the wait expired, or the claim or HELLO
     * failed. Only the detail changes, and only where the standing text claimed the adapter would be
     * re-claimed on return, which is no longer true: IDLE, and (B-F1, 2026-10-08) a STALLED whose
     * cause is the ADAPTER — the grace-expiry ladder exhausting under a relink names the adapter's
     * state ("plugged in and connecting — wait a few seconds"), and a capped or expired relink left
     * that standing until Start. A STALLED blamed on the phone, and every other phase, owns its own
     * text and stands.
     */
    fun onRelinkEnded(detail: String) = post {
        if (phase == RxPhase.IDLE) to(RxPhase.IDLE, detail)
        else if (phase == RxPhase.STALLED && stalledCause == StallCause.ADAPTER) to(RxPhase.STALLED, detail, StallCause.ADAPTER)
    }

    /**
     * The box is back. If it went away mid-session, its radios must stay down: the phone is already
     * connected over Wi-Fi and a fresh BT bring-up would only disturb it.
     */
    fun onBoxRelinked() = post {
        // Keyed on the phase, not on [boxLostMidSession] alone: that flag is only cleared here, so
        // a session that ended (GRACE) between the caller's attach check and this post would
        // otherwise be reported live again with the radios held under nothing.
        if (phase == RxPhase.SESSION_UP || phase == RxPhase.BOX_LOST_SESSION_UP) {
            log.w("box returned under a live session — holding its radios DOWN")
            val held = act.setBoxRadios(false)
            if (held) radiosHeldForSession = true
            else log.w("CT_RADIO off failed — the box may bring BT up on its own")
            boxLostMidSession = false
            // v5.7 F013: "radios held down" only when the CT_RADIO off was actually dispatched.
            to(RxPhase.SESSION_UP, if (held) "box back; radios held down, session intact"
                else "box back; could not hold its radios down — session intact")
        } else {
            if (boxLostMidSession) log.i("box returned, but the session it was lost under has ended — radios NOT held")
            boxLostMidSession = false
            linkForward("box back")
            // The session ended between the relink's subscribe decision and this post (2026-10-06):
            // the link is up but unsubscribed, so nothing would wake the box's radios for the phone's
            // return. In GRACE the grace expiry's ladder owns it; with a ladder running, the subscribe
            // goes out as rung 1 (cooldown honoured); otherwise it is dispatched here.
            if (phase != RxPhase.GRACE && runCatching { act.boxLinkedUnsubscribed() }.getOrDefault(false)) {
                if (pending(retryTimer) || phase == RxPhase.STALLED) {
                    // Re-enter AT rung 1 (its cooldown still applies): from STALLED the ladder is
                    // exhausted, and with a timer pending the next rung may be 2, which needs the
                    // subscribe this provides. `pending`, not `!= null` (v5.7 F004): a fired retry
                    // is not a running ladder.
                    rung = 1
                    // v5.7 F001: leave STALLED before the ladder runs, or its rungs fire under a
                    // terminal phase that ignores dial-accepts and keeps the discovery backoff on.
                    if (phase == RxPhase.STALLED) to(RxPhase.ARMED, "box relinked — recovering")
                    escalate("box relinked unsubscribed after the session ended")
                } else {
                    log.i("box relinked unsubscribed after the session ended — sending the deferred CT_SUBSCRIBE")
                    if (!act.subscribeBox()) log.w("deferred CT_SUBSCRIBE not dispatched — the command executor is gone")
                }
            }
        }
    }

    /**
     * [replay] means the box re-emitted a latched mirror value to a fresh subscriber (envelope bit2,
     * [Ocbm.F_REPLAY]) rather than reporting something the phone just did.
     *
     * A replay updates the label — it is a truthful statement about where Bluetooth got to — but it
     * must not arm a deadline. The box replays on every SUBSCRIBE, so a watchdog started here ran
     * against a phone that was never part of this session and escalated 45 s into every single
     * session, guaranteed. The rare attach-mid-handshake case loses nothing that matters: the phone's
     * own events (dial-accept, inbound) arrive on their own and carry their own clocks.
     */
    fun onBtPhase(p: Byte, replay: Boolean = false) = post {
        // A live session outranks every BT phase. Bluetooth is the anchor that BUILDS a session; once
        // the phone is streaming over Wi-Fi the A/V path no longer depends on it, so a late or
        // repeated BT mirror says nothing about session health.
        //
        // Without this guard the machine walks backwards out of SESSION_UP under a working session,
        // and the damage is not cosmetic: leaving SESSION_UP RESUMES mDNS discovery (see `to()`),
        // which lets the 12 s rediscover loop redial and hijack the live control connection, and
        // BTP_LINK_UP / BTP_WIFI_HANDOFF each arm the 45 s handoff watchdog, whose expiry drops to
        // ARMED and escalates the recovery ladder — rung 1 cycles the box's radios and rung 2 fires
        // MGMT_RESTART_WIRELESS, underneath a session that was streaming fine.
        //
        // Device-observed 2026-09-08: `CarPlay live -> pairing -> handoff sent` at 21:42:04 while
        // HEVC was at 900 rendered frames. `onBoxHealth` and `onDialAccepted` already guard this way;
        // this handler was the one that did not.
        if (phase == RxPhase.SESSION_UP || phase == RxPhase.BOX_LOST_SESSION_UP) {
            log.i("session is live — noting BT phase ${Ocbm.btpName(p)}, not acting on it")
            return@post
        }
        // Any genuine BT progress restarts the rediscover backoff count, whether or not it moves the
        // phase (the `else` branch below moves nothing). A replay is the box re-reading its mirror
        // to a fresh subscriber, not progress, and must not reset it.
        if (!replay) runCatching { act.setDiscoveryBackoff(phase == RxPhase.STALLED || phase == RxPhase.BOX_UNHEALTHY) }
        when (p) {
            Ocbm.BTP_LINK_UP, Ocbm.BTP_AUTHENTICATING -> {
                // A genuine new BT link starts a new handoff attempt; the previous expiry's latch must
                // not colour its terminal text. A replay is the mirror, not a new attempt.
                if (!replay) { handoffExpiredAfterBt = false; armHandoffWatchdog(); deferLadder("phone is pairing over Bluetooth") }
                to(RxPhase.BT_PAIRING, "phone connecting over Bluetooth")
            }
            Ocbm.BTP_IDENTIFIED -> {
                // v5.7 F005: deferLadder yields the clock to "the deadline the new phase just armed",
                // so this phase must arm one, as LINK_UP and WIFI_HANDOFF do ([armHandoffWatchdog]
                // is re-armed on each BT phase by design). Without it, BT_PAIRED had no deadline.
                if (!replay) { armHandoffWatchdog(); deferLadder("phone identified over iAP2") }
                to(RxPhase.BT_PAIRED, "phone identified over iAP2")
            }
            Ocbm.BTP_WIFI_HANDOFF -> {
                if (!replay) { armHandoffWatchdog(); deferLadder("box sent the Wi-Fi handoff") }
                // The only trustworthy signal that a CarPlay connect is actually coming. The phone
                // appearing on the SoftAP is NOT one: the vehicle hotspot is an ordinary saved network
                // on the phone, so it joins whenever it is in range whether or not CarPlay is
                // involved. Observed 2026-08-27 sitting on br0 with isCarPlayWiFi:0 for minutes.
                to(RxPhase.HANDOFF_SENT, "Wi-Fi handoff sent — expecting the phone to connect")
            }
            else -> Unit   // BTP_IDLE and friends carry no transition of their own
        }
    }

    /** The phone answered `GET /ctrl-int/1/connect`. Start the dial-back clock. */
    fun onDialAccepted() = post {
        if (phase == RxPhase.SESSION_UP || phase == RxPhase.BOX_LOST_SESSION_UP) return@post
        // STALLED is terminal on this side: the phone kept 200-OKing nudges every ~12.6 s without
        // dialling, and each accept re-entered INBOUND_EXPECTED ("CARPLAY STARTING") only to fall
        // back 15 s later — `stalled -> awaiting dial-back` at 04:13:52 on 2026-10-06.
        if (phase == RxPhase.STALLED) { log.i("nudge accepted again — still stalled"); return@post }
        // The box-side clock's job is done the moment the phone answers: from here the dial-back
        // deadline is the only one that applies. Leaving the handoff watchdog armed let it survive
        // inbound-expiry — which moves the phase back to ARMED and re-qualifies it — and the two
        // deadlines then burned two rungs in four seconds, promoting the ladder to the destructive
        // rung 2 (observed 17:31:17.732 and 17:31:21.243).
        handoffTimer?.cancel(false); handoffTimer = null
        // Same reasoning, applied to the ladder's blind 20 s retry: the dial-back clock armed just
        // below is now the only deadline that applies. Observed firing rung 2 into a healthy session
        // 1.4 s after this transition on 2026-09-08. See [deferLadder].
        deferLadder("phone accepted the nudge")
        to(RxPhase.INBOUND_EXPECTED, "phone accepted the nudge — waiting for it to dial back")
        inboundTimer = arm(inboundTimer, INBOUND_TIMEOUT_MS) {
            if (phase == RxPhase.INBOUND_EXPECTED) {
                log.w("no dial-back within ${INBOUND_TIMEOUT_MS / 1000}s of an accepted nudge")
                // Same reason as the grace timer: escalate() alone leaves the phase where it was, and
                // a stale INBOUND_EXPECTED would never re-arm its own deadline.
                to(RxPhase.ARMED, "no dial-back — recovering")
                escalate("the phone accepted the connect and did not dial back")
            }
        }
    }

    /**
     * [stillCurrent] is evaluated HERE, on the scheduler, not at the call site. A receiver callback
     * can pass its own fence and then be overtaken by a deliberate stop that posts [onStopped] ahead
     * of this post; the FIFO then ran IDLE -> SESSION_UP for a stopped receiver (false "CarPlay
     * live", which blocks every recovery path because SESSION_UP refuses them all). The caller's
     * fence is raised BEFORE it posts onStopped, so any onSessionUp that runs after onStopped sees it.
     */
    fun onSessionUp(stillCurrent: () -> Boolean = { true }) = post {
        if (!stillCurrent()) {
            log.i("ignoring session-up from a receiver that is being stopped or was replaced")
            return@post
        }
        cancelAllTimers()   // includes the ladder's own retry chain: a real session ends recovery
        rung = 0                       // a real session is the only proof the ladder can stop
        skipped1 = false
        boxLostMidSession = false
        handoffExpiredAfterBt = false
        sessionSeen = true
        to(RxPhase.SESSION_UP, "CarPlay session is up")   // to() suspends discovery for this state
    }

    /**
     * The control connection went away. Hold still for [GRACE_MS] with discovery PAUSED — the phone
     * usually comes back by itself, and the 12 s rediscover churn would otherwise redial into the gap
     * and provoke a hijack of the session that was about to resume.
     */
    fun onSessionDown(reason: EndReason = EndReason.CLOSED) = post {
        releaseSessionRadioHold("the session it was protecting has ended")
        val wasLive = sessionSeen
        sessionSeen = false
        // The ladder restarts from the bottom for the phone's return, unconditionally. A rung and
        // latch left over from an excursion under the session (see [onBoxSubscribed]) made the
        // post-session ladder run zero rungs and print the hotspot text (04:53:48 on 2026-10-06);
        // a retryTimer left armed could fire a rung inside GRACE with discovery paused. The
        // per-rung cooldown stamps are kept, so a reset rung still waits its cooldown out.
        rung = 0
        skipped1 = false
        handoffExpiredAfterBt = false
        retryTimer?.cancel(false); retryTimer = null
        // v5.7 F006: the box-side watchdog too. A control connection that closed before a session
        // formed never passed through onSessionUp's cancelAllTimers, so a watchdog armed by BT
        // progress survived GRACE, fired in the post-grace ARMED and escalated a second time —
        // rung 2 inside rung 1's 35 s window. The grace expiry's own ladder owns the clock now.
        handoffTimer?.cancel(false); handoffTimer = null
        to(RxPhase.GRACE, if (wasLive) "session ended — holding ${GRACE_MS / 1000}s for the phone to return"
            else "control connection closed before a session formed — holding ${GRACE_MS / 1000}s")
        graceTimer = arm(graceTimer, GRACE_MS) {
            if (phase != RxPhase.GRACE) return@arm
            log.w("phone did not return within ${GRACE_MS / 1000}s")
            // Leave GRACE FIRST. escalate() does not move the phase unless the ladder runs out, so
            // staying here would strand the machine in a state that suspends discovery with no timer
            // left to fire — the receiver would go quiet permanently and look idle while doing
            // nothing. to() restores discovery on the way out.
            to(RxPhase.ARMED, "grace expired — resuming discovery and recovering")
            // Fast (no rung-0 hold) after a dead link, or when rung 1 is a deferred subscribe: the
            // phone cannot come back over a link that died, and R1 shows rung 1 started the P1
            // recovery. `reason` is this closure's own copy of the ending connection's verdict.
            val fast = reason == EndReason.LINK_LOST || runCatching { act.boxLinkedUnsubscribed() }.getOrDefault(false)
            escalate("the session ended (${reason.name}) and the phone did not come back", fast)
        }
    }

    /**
     * [CarPlayRx] has counted enough accepted-but-unanswered dials to call it. This is the terminal
     * rung: three reproductions on 2026-08-27 showed power-cycling the adapter does not clear it,
     * and reconnecting Bluetooth from the iPhone cleared it every time. 2026-10-06: after an
     * in-process receiver restart the phone dialled the IPv4-only self-hosted advert instead
     * (9/9 TCP timeouts) and a process restart did clear it. Those timeouts were the record set —
     * the radio drops IPv4 :7011 — and the responder now publishes br0's IPv6 link-local as well.
     */
    fun onStalled(dials: Int) = post {
        // One thread hop after the receiver's own live check; a session that formed in between
        // outranks the report, as it does in [onDialAccepted].
        if (phase == RxPhase.SESSION_UP || phase == RxPhase.BOX_LOST_SESSION_UP) {
            log.i("stall reported with a live session — the phase stands")
            return@post
        }
        cancelAllTimers()
        toStalled("iPhone sees this receiver but will not connect ($dials tries) — " +
            "reconnect Bluetooth to the car from the iPhone", "stall after $dials accepted dials", skipped1 = false, cause = StallCause.PHONE)
    }

    /**
     * Everything was torn down deliberately (the Stop button, or `stopEverything`). Reset to IDLE and
     * drop the ladder: without this the supervisor kept a stale phase — potentially SESSION_UP, whose
     * invariant suspends discovery — across a teardown, and the next start-up inherited it.
     */
    fun onStopped() = post { resetToIdle("stopped") }

    /**
     * The merged Restart Session (plan v5.9 UI-1, D001: Recover was folded into it). The same reset to
     * IDLE as [onStopped], and ALSO the per-rung cooldown stamps cleared: an explicit driver request
     * is not rate-limited, and stamps earned against the link being dropped would otherwise hold the
     * fresh link's ladder back by up to [RUNG_COOLDOWN_MS] (30/90/180 s). Stop and Forget keep using
     * [onStopped], which keeps the stamps (the unattended rule, see [onSessionDown]).
     * [onRestartSubscribed] clears them once more, for a rung that ran during the settle.
     */
    fun onOperatorRestart() = post {
        resetToIdle("operator restart")
        java.util.Arrays.fill(lastRungAt, 0L)
    }

    /** The body of [onStopped] and [onOperatorRestart]: rung 0, timers cancelled, IDLE ([why] is the to() detail). */
    private fun resetToIdle(why: String) {
        releaseSessionRadioHold("everything was stopped ($why)")
        cancelAllTimers()
        rung = 0
        skipped1 = false
        boxLostMidSession = false
        boxHealth = -1
        // A rung-2 / Restart adapter Bluetooth claim must not outlive the teardown and swallow the next start's
        // first health regression (box health is change-only, so that edge never comes back; B-M3).
        radioSettleUntil = 0L
        handoffExpiredAfterBt = false
        sessionSeen = false
        to(RxPhase.IDLE, why)
    }

    /**
     * Restart Session has just re-SUBSCRIBEd the box. Give ARMED the deadline it otherwise lacks.
     *
     * ARMED carries no clock of its own on purpose: on an ordinary bring-up the receiver waits for a
     * phone for the whole drive, and a phone that is simply not there is not a fault. After a driver
     * restart the phone IS there — it was in or near a session moments ago — and its silence means it
     * never re-initiated Bluetooth. Left alone the machine would sit in ARMED forever saying "waiting
     * for the phone". This is a REPORT, not the ladder: the ladder's rungs (re-announce, CT_RADIO,
     * MGMT_RESTART_WIRELESS) cannot make a phone dial Bluetooth, and a second restart on top of a
     * fresh bring-up is the 2026-09-08 collapse. Rides the `handoffTimer` slot so genuine BT progress
     * ([onBtPhase] -> [armHandoffWatchdog]) replaces it and any teardown cancels it.
     *
     * Ordering hazard the guard below exists for: this is posted from the command thread AFTER
     * `runAll` returns, while BT phases are posted from the read thread as they arrive — so a real
     * `BTP_LINK_UP` can land first, arm the genuine watchdog, and would then be REPLACED by this timer,
     * whose body does nothing outside ARMED/BOX_LINKED. Verifier-measured 2026-09-11: BT_PAIRING with
     * no deadline anywhere. So the slot is taken only while the phase still warrants it, or when it is
     * empty (a replayed phase moves the label but arms nothing).
     *
     * Also fires in BOX_LINKED: `subscribed` is our write, ARMED is the box's SEV_HOST_PRESENT. A box
     * that never sends it leaves the restart on CLAIMING forever, and this button's contract is a
     * definite outcome either way.
     */
    fun onRestartSubscribed() = post {
        // FIRST, before the early return (plan v5.9 UI-1, R2): a box-health regression that landed
        // during the restart's settle can still have run a rung (onBoxHealth escalates in IDLE) and
        // re-stamped it; the fresh link starts with no cooldown whatever ran in between.
        java.util.Arrays.fill(lastRungAt, 0L)
        if (phase != RxPhase.ARMED && phase != RxPhase.BOX_LINKED && pending(handoffTimer)) {
            log.i("restart: Bluetooth already progressed to ${phase.label} with its own deadline — not arming the restart deadline")
            return@post
        }
        handoffTimer = arm(handoffTimer, HANDOFF_TIMEOUT_MS) {
            when (phase) {
                RxPhase.ARMED -> {
                    log.w("restart: still ARMED ${HANDOFF_TIMEOUT_MS / 1000}s after re-subscribing — the iPhone never re-initiated Bluetooth")
                    toStalled("restarted, but the iPhone did not reconnect Bluetooth — reconnect from the iPhone", "restart deadline", skipped1 = false, cause = StallCause.PHONE)
                }
                RxPhase.BOX_LINKED -> {
                    log.e("restart: still BOX_LINKED ${HANDOFF_TIMEOUT_MS / 1000}s after CT_SUBSCRIBE — the adapter never confirmed the host present, so its radios were never told to come up")
                    toStalled("restarted, but the adapter never confirmed the host present — no radio bring-up; press Restart Session again, or replug the adapter", "restart deadline", skipped1 = false, cause = StallCause.ADAPTER)
                }
                else -> Unit
            }
        }
    }

    /**
     * The driver is about to press Restart adapter Bluetooth (`MGMT_RESTART_WIRELESS`). Own the health regression
     * it will cause — see [WIRELESS_RESTART_SETTLE_MS]. Called BEFORE the verb is sent, as rung 1
     * does, so the first regression cannot arrive unowned; if the verb is then refused the cost is
     * 20 s of not reacting to a health regression, which is the lesser error.
     */
    fun onManualWirelessRestart() = post {
        radioSettleUntil = maxOf(radioSettleUntil, clock() + WIRELESS_RESTART_SETTLE_MS)
        log.i("driver-requested wireless restart — box health regressions are self-inflicted for the next ${WIRELESS_RESTART_SETTLE_MS / 1000}s")
    }

    /**
     * Tear down, ON the scheduler thread.
     *
     * This was the one method that bypassed [post] and touched scheduler-owned fields
     * (`radiosHeldForSession`, `radioOnPending`, all five timers) from the caller's thread --
     * `MainActivity.onDestroy`, i.e. the main thread. The class contract above says those need no
     * locking precisely BECAUSE only one thread touches them, so that was a straight violation, and
     * it had a concrete cost: with rung 1 mid-flight between its `CT_RADIO off` and
     * `armRadioOnEdge()`, `cancelAllTimers` would read `radioOnPending == false`, skip its rescue,
     * and `shutdownNow()` would then reject the `arm()` the scheduler thread was about to make.
     * `radioOnPending` was left true with no timer anywhere and the box kept a radio inhibit that we
     * own -- Bluetooth stays down, no BT phase is ever emitted, so no watchdog arms and nothing
     * notices. Racing the other way (main thread wins on a genuinely pending edge) ends identically.
     *
     * `shutdown()` from INSIDE the block, not `shutdownNow()` from the caller, for two reasons:
     * `cancelAllTimers` may itself arm a 2 s CT_RADIO-on retry, and calling `sched.shutdown()` from
     * the caller would race that `arm()` into a RejectedExecutionException; and a
     * ScheduledThreadPoolExecutor still runs delayed tasks registered before `shutdown()`
     * (`ExecuteExistingDelayedTasksAfterShutdownPolicy` defaults to true), whereas `shutdownNow()`
     * discards exactly the edge that must not be lost. Non-blocking, so it is safe in `onDestroy`.
     *
     * [then] runs on this thread once the block is done (in a `finally`, so a throw cannot skip
     * it), or inline right here if the scheduler already refused the post. It exists so the caller
     * can shut its own command executor down AFTER the CT_RADIO ON that `releaseSessionRadioHold`
     * / `cancelAllTimers` may dispatch onto it: `onDestroy` used to shut that executor on the main
     * thread with no ordering against this block, so the write could be dropped while logged as sent.
     */
    fun stop(then: () -> Unit = {}) {
        postOrInline(then) {
            try {
                releaseSessionRadioHold("the supervisor is going away")
                cancelAllTimers()
                // Leave discovery RUNNING. The supervisor going away (Activity destroyed) must not leave
                // the receiver permanently unable to dial: the receiver outlives the UI by design.
                runCatching { act.resumeDiscovery() }
            } finally {
                runCatching { sched.shutdown() }
                then()
            }
        }
    }

    // ---- the ladder -----------------------------------------------------------------------------

    /**
     * Try the next rung, cheapest first, honouring per-rung cooldowns.
     *
     * Rungs never repeat automatically without a cooldown. The ladder's POSITION resets at rung 0 on
     * a real session ([onSessionUp], which also cancels the retry), on a session ending
     * ([onSessionDown]) and on the box link going away outside GRACE ([onBoxLost]); the per-rung
     * cooldown stamps survive all three, so a reset rung still waits its cooldown out. Rung 3 is not
     * an action — the evidence says the remaining fix is on the phone, and an app that keeps silently
     * retrying something that cannot work is worse than one that says so.
     */
    private fun escalate(why: String, fast: Boolean = false) {
        if (phase == RxPhase.SESSION_UP || phase == RxPhase.BOX_LOST_SESSION_UP) {
            log.i("recovery abandoned — a session came up on its own")
            rung = 0
            skipped1 = false
            return
        }
        val now = clock()
        while (rung < RUNG_COOLDOWN_MS.size) {
            val since = now - lastRungAt[rung]
            if (lastRungAt[rung] != 0L && since < RUNG_COOLDOWN_MS[rung]) {
                // WAIT — do not promote. Cooling means "not yet", not "never": skipping forward
                // answers a cheap rung's cooldown with a MORE expensive action, which is exactly how
                // MGMT_RESTART_WIRELESS became a *first* response and dropped a pairing in progress.
                // It also made STALLED reachable with every rung cooling and no action taken at all.
                val left = RUNG_COOLDOWN_MS[rung] - since
                log.i("rung $rung still cooling down (${left / 1000}s left) — waiting for it, not promoting")
                retryTimer = arm(retryTimer, left) { escalate(why, fast) }
                return
            }
            rung1Skipped = false
            val done = runRung(rung, why, fast)
            // The latest rung-1 attempt decides (v5.7 F010): a re-entry at rung 1 ([onBoxRelinked])
            // that now runs replaces an earlier skip.
            if (rung == 1) skipped1 = rung1Skipped
            // Stamp only a rung that actually RAN. A refused rung (no responder, no box link) costs
            // nothing and must cost nothing: burning its cooldown here walked the ladder to
            // exhaustion on actions that never happened.
            if (done) lastRungAt[rung] = now
            // Rung 0 is refused when the self-hosted responder is not up (it did not start), and
            // falling straight through to rung 1 would make
            // CT_RADIO off/on the FIRST response, with no wait — also on the box-health entry,
            // where a dip that self-healed in 0.5 s (01:22:58.723 -> 01:22:59.231, 2026-10-06) would have
            // been answered with a radio cycle. So an idle rung 0 still holds the 20 s it holds when
            // it does re-announce, without being stamped as an action taken.
            val holdFirst = !done && rung == 0
            val ran = rung
            rung++
            // `fast` (the grace expiry after a dead link, or with a deferred subscribe pending): a
            // rung 0 that ran or was refused does NOT hold — the loop goes on to rung 1 in this pass.
            if (ran == 0 && fast) continue
            if (done || holdFirst) {
                // Keep the ladder moving under its own power. Nothing external is guaranteed to call
                // us back: a silent phone produces no events at all. After a rung-1 or rung-2 action
                // the hold outlasts that rung's own measured effect window (RUNG1_/RUNG2_RECHECK_MS);
                // LADDER_RECHECK_MS is now only rung 0's hold.
                retryTimer = arm(retryTimer, when (ran) {
                    1 -> RUNG1_RECHECK_MS
                    2 -> RUNG2_RECHECK_MS
                    else -> LADDER_RECHECK_MS
                }) { escalate(why) }
                return
            }
        }
        log.e("recovery ladder exhausted — $why")
        // C4: the phone is the side that did not come back while the box link is alive; otherwise the adapter.
        val cause = if (runCatching { act.boxLinkAlive() }.getOrDefault(false)) StallCause.PHONE else StallCause.ADAPTER
        toStalled(terminalDetail(), why, skipped1, cause)
    }

    /** Every transition INTO STALLED goes through here, so [skippedAtRung] is always assigned. */
    private fun toStalled(detail: String, why: String, skipped1: Boolean, cause: StallCause) {
        to(RxPhase.STALLED, detail, cause)
        stalledCause = cause
        skippedAtRung = if (skipped1) 1 else 0
        stalledWhy = why
        stalledAt = clock()
    }

    /**
     * Rungs 1 and 2 are box operations. Attempting them with no box is not merely futile — it burns
     * their cooldowns, walks the ladder to exhaustion, and ends by telling the driver to reconnect
     * Bluetooth when the real problem is that the adapter is unplugged.
     *
     * Device-observed 2026-08-27: the adapter was removed mid-session, the supervisor correctly
     * entered BOX_LOST_SESSION_UP and held the CarPlay session — and then, ninety seconds later, the
     * ladder escalated through `CT_RADIO` and `MGMT_RESTART_WIRELESS` anyway. Both writes failed, both
     * rungs were consumed, and the terminal advice named the wrong fault. The information was already
     * in the phase; it just was not used.
     */
    private fun runRung(n: Int, why: String, fast: Boolean = false): Boolean = when (n) {
        0 -> {
            log.w("recovery rung 0 (re-announce mDNS): $why")
            val ok = act.reannounce()
            if (!ok) log.i("rung 0 has nothing to do — self-hosted responder not up; " +
                if (fast) "fast entry, going straight on to rung 1" else "holding ${LADDER_RECHECK_MS / 1000}s before the first box-side action")
            ok
        }
        // Rung 1 from EVERY entry (2026-10-06): a link restored under the last session without a
        // subscribe gets its deferred CT_SUBSCRIBE here — never CT_RADIO, which an unsubscribed box
        // may not honour. A true dispatch is a rung-1 action that ran. Rung 2 still needs `subscribed`.
        1 -> if (runCatching { act.boxLinkedUnsubscribed() }.getOrDefault(false)) {
            log.w("recovery rung 1 = deferred CT_SUBSCRIBE (link restored under the last session, never subscribed): $why")
            act.subscribeBox().also { if (!it) { rung1Skipped = true; log.w("recovery rung 1: the deferred CT_SUBSCRIBE was not dispatched") } }
        } else boxRungOrSkip(1, why)
        2 -> boxRungOrSkip(2, why)
        else -> false
    }

    private fun boxRungOrSkip(n: Int, why: String): Boolean =
        if (!runCatching { act.boxLinkAlive() }.getOrDefault(false)) {
            if (n == 1) rung1Skipped = true
            val st = runCatching { act.adapterState().name }.getOrDefault("unknown")
            log.w("recovery rung $n skipped — no subscribed OCBM link to the adapter (adapter: $st), so a box-side action cannot help")
            false
        } else runBoxRung(n, why)

    /** The box-side rungs proper, reached only when the link is alive. */
    private fun runBoxRung(n: Int, why: String): Boolean = when (n) {
        1 -> {
            log.w("recovery rung 1 (CT_RADIO off/on — no host_present edge, so no flap risk): $why")
            // Off then on, spaced. The box clears its radio inhibit on the off->on edge; doing both
            // back to back would collapse into no edge at all.
            if (!act.setBoxRadios(false)) { log.w("CT_RADIO off failed"); false }
            else {
                // Everything that happens to box health for the next few seconds is ours. Claim it
                // BEFORE the on-edge is armed, or the first regression arrives unowned.
                radioSettleUntil = clock() + RADIO_SETTLE_MS
                armRadioOnEdge()
                true
            }
        }
        2 -> {
            log.w("recovery rung 2 (MGMT_RESTART_WIRELESS): $why")
            // Same claim rung 1 makes and the driver's Restart adapter Bluetooth makes ([onManualWirelessRestart]):
            // without it the HCI drop this restart causes read as a fresh fault and the ladder was
            // exhausted 6 s after the rung (04:00:02 -> 04:00:08, 2026-10-06). The window is the
            // whole post-rung-2 hold (v5.6, review B-N4), not WIRELESS_RESTART_SETTLE_MS: a regression
            // 20-45 s after the rung would otherwise escalate at rung 3 straight into STALLED, the same
            // premature STALLED by another route. The driver's own Restart adapter Bluetooth keeps the 20 s window.
            // Claimed only when the verb was actually dispatched (v5.6 fix, review B-M3): a refused
            // rung 2 must not blind the health handler for 45 s. Set after the call is still before
            // any regression it causes is handled — onBoxHealth posts behind this task.
            val ran = act.restartBoxWireless()
            if (ran) radioSettleUntil = maxOf(radioSettleUntil, clock() + maxOf(WIRELESS_RESTART_SETTLE_MS, RUNG2_RECHECK_MS))
            ran
        }
        else -> false
    }

    /** Name the fault the driver can actually act on, rather than always blaming the phone. */
    private fun terminalDetail(): String {
        // 52C (2026-10-06): one sentence per adapter state instead of "check it is plugged in" for
        // every unlinked state — that was false for an adapter on the bus waiting for USB access.
        val a = runCatching { act.adapterState() }.getOrNull()
        if (a != null && a !== AdapterState.SUBSCRIBED) return a.text
        if (a == null && !runCatching { act.boxLinkAlive() }.getOrDefault(false)) return "no link to the CarPlay adapter — check it is plugged in"
        return terminalDetailLinked()
    }

    /** The adapter is linked and subscribed: name the remaining fault the driver can act on. */
    private fun terminalDetailLinked(): String = when {
        // Bluetooth got as far as iAP2 identification / the Wi-Fi handoff and the phone never
        // opened a CarPlay connection: the one fault on this side the driver can check is the
        // hotspot the phone was handed. Neither the SSID nor the passphrase is named (v5.8 V7 fix1 /
        // Q014: this detail is logged at E, put on the Board and exported — until then the SSID was
        // quoted here in clear; the screen's own credential summary already shows it). v5.7 F012: the
        // text states only what was observed — no CarPlay connection followed. Whether the iPhone
        // joined the hotspot is not visible here (a join is no CarPlay signal; see [onBtPhase]).
        handoffExpiredAfterBt ->
            "Bluetooth completed but no CarPlay connection followed — check the hotspot SSID and passphrase " +
                "on this screen match the vehicle hotspot, then reconnect Bluetooth from the iPhone"
        // v5.6 fix (review A3): the box-health route (BOX_UNHEALTHY -> rungs 1, 2 -> here) with hci0
        // absent. The adapter cannot page any iPhone without its controller, so the text below would
        // be false; the one driver action left that re-runs the box's bring-up is a replug.
        boxHealth >= 0 && (boxHealth and Ocbm.BH_HCI_PRESENT) == 0 ->
            "the app's recovery steps are used up — the adapter's Bluetooth controller is not up; replug the adapter, then reconnect Bluetooth to the car from the iPhone"
        // v5.6 H2 (reviews A4, B-N6): "cannot recover automatically" was false while the adapter
        // keeps paging on its own — its reconnect driver tries every bonded phone with 10 -> 60 s
        // backoff until shutdown (crates/vendor/wireless/src/reconnect.rs:51-52, :754-757). With NO
        // bonds it idles (reconnect.rs:800-811), and the app holds no current bond count here (only
        // the session-start MGMT_INFO snapshot), so the text is hedged to be true either way.
        else -> "the app's recovery steps are used up — the adapter keeps trying to reconnect any iPhone it is paired with over Bluetooth; if CarPlay does not return, reconnect Bluetooth to the car from the iPhone"
    }

    // ---- plumbing -------------------------------------------------------------------------------

    private fun post(block: () -> Unit) {
        try { sched.execute { runCatching(block).onFailure { log.e("supervisor: ${it.javaClass.simpleName}: ${it.message}") } } }
        catch (_: Throwable) { /* shut down */ }
    }

    /** [post], except a rejected post (scheduler already shut down) runs [rejected] inline. Only [stop] needs it. */
    private fun postOrInline(rejected: () -> Unit, block: () -> Unit) {
        try { sched.execute { runCatching(block).onFailure { log.e("supervisor: ${it.javaClass.simpleName}: ${it.message}") } } }
        catch (_: Throwable) { rejected() }
    }

    /**
     * The single place phase changes, so the discovery invariant is stated once instead of being
     * maintained by hand at every call site — which is how a paused-and-never-resumed leak gets in.
     *
     * Discovery is suspended in exactly the states where a dial would be actively harmful: while a
     * session is up (a redial provokes a hijack of the session we already have) and across the grace
     * window (a redial preempts the wait it exists to protect). Everywhere else it must be running,
     * including STALLED — the phone may come back on its own at any moment and the receiver has to be
     * able to answer when it does.
     */
    private fun to(next: RxPhase, detail: String, stall: StallCause? = null) {
        // Any exit from STALLED ends the STALLED-by-skip record; toStalled() re-assigns it on entry.
        if (next != RxPhase.STALLED) { skippedAtRung = 0; stalledWhy = null; stalledAt = 0L; stalledCause = null }
        if (next != phase) {
            val line = "${phase.label} -> ${next.label}  ($detail)"
            if (next.isFailure) log.e(line) else log.i(line)
            // The phase is the one piece of standing state a capture most needs at a random
            // offset, so it lives on the Board alongside the listener, adverts and core. FAILED for
            // the failure phases (Board.failed is its own E line and leaves the dump reading
            // "FAILED", not "UP stalled"); the four anchoring phases also dump the whole board so
            // "what was up when it went live / when it gave up" is one block, not a reconstruction.
            if (next.isFailure) SessionTrace.Board.failed("phase", "${next.label}: $detail")
            else SessionTrace.Board.up("phase", next.label)
            if (next.isFailure || next == RxPhase.SESSION_UP) SessionTrace.Board.dump("phase ${next.label}")
        }
        phase = next
        val quiet = next == RxPhase.SESSION_UP || next == RxPhase.BOX_LOST_SESSION_UP || next == RxPhase.GRACE
        runCatching { if (quiet) act.pauseDiscovery() else act.resumeDiscovery() }
            .onFailure { log.w("discovery toggle failed: ${it.message}") }
        // The second standing invariant on discovery: its cadence may lengthen only where nothing
        // more is expected from the phone. Stated here, once, for the same reason as the pause.
        runCatching { act.setDiscoveryBackoff(next == RxPhase.STALLED || next == RxPhase.BOX_UNHEALTHY) }
            .onFailure { log.w("discovery backoff toggle failed: ${it.message}") }
        act.report(next, detail, if (next == RxPhase.STALLED) stall else null)
    }

    /**
     * A timer slot holds a deadline that has not happened yet (v5.7 F004). The slots are not nulled
     * when their timer fires, so `!= null` read a spent future as pending. The scheduler is
     * single-threaded, so a timer body never sees its OWN future as done while it runs.
     */
    private fun pending(f: ScheduledFuture<*>?): Boolean = f != null && !f.isDone

    /** Arm one named deadline, replacing only its own previous value. */
    private fun arm(prev: ScheduledFuture<*>?, delayMs: Long, block: () -> Unit): ScheduledFuture<*>? {
        prev?.cancel(false)
        return try {
            sched.schedule({ runCatching(block).onFailure { log.e("timer: ${it.message}") } }, delayMs, TimeUnit.MILLISECONDS)
        } catch (_: Throwable) { null }
    }

    /**
     * Give the box-side phases a deadline. Re-armed on each BT phase change so steady progress keeps
     * pushing the deadline out, and only a genuine stall reaches it.
     */
    private fun armHandoffWatchdog() {
        handoffTimer = arm(handoffTimer, HANDOFF_TIMEOUT_MS) {
            when (phase) {
                RxPhase.ARMED, RxPhase.BT_PAIRING, RxPhase.BT_PAIRED, RxPhase.HANDOFF_SENT -> {
                    log.w("no CarPlay connect within ${HANDOFF_TIMEOUT_MS / 1000}s of Bluetooth progress")
                    // Past identification with no connect is the hotspot-credentials shape; remember
                    // it for the terminal text (see [handoffExpiredAfterBt]).
                    if (phase == RxPhase.BT_PAIRED || phase == RxPhase.HANDOFF_SENT) handoffExpiredAfterBt = true
                    // Same reason as the grace and inbound timers: escalate() does not move the phase,
                    // so without this the label stays HANDOFF_SENT for the whole of recovery and every
                    // line the driver and the logs see names a step that finished long ago.
                    to(RxPhase.ARMED, "no CarPlay connect — recovering")
                    escalate("Bluetooth completed but the phone never opened a CarPlay session")
                }
                else -> Unit
            }
        }
    }

    /**
     * Schedule rung 1's on-edge, and make it un-losable.
     *
     * The off-edge inhibits the box's radios; only this timer clears the inhibit. Cancelling it — a
     * session coming up, a Stop press, or a stall report inside the 2 s window — would leave the box
     * with radios off and nothing pending to turn them back on. The box's own SUBSCRIBE handler used
     * to be the safety net for that and no longer needs to be, so the guarantee has to live here:
     * [cancelAllTimers] fires the on-edge inline rather than dropping it.
     */
    private fun armRadioOnEdge() {
        radioOnPending = true
        ladderTimer = arm(ladderTimer, 2_000L) {
            radioOnPending = false
            // A false is "not dispatched", not "done". Swallowing it silently strands the box's
            // radios off with nothing pending — precisely the state this method exists to prevent.
            if (!act.setBoxRadios(true)) {
                log.e("CT_RADIO on was not dispatched — retrying once in 2s")
                radioOnPending = true
                ladderTimer = arm(ladderTimer, 2_000L) {
                    radioOnPending = false
                    if (!act.setBoxRadios(true)) log.e("CT_RADIO on refused twice — the box's radios are left DOWN")
                }
            }
        }
    }

    /**
     * Give the box its radios back if we were holding them down for a live session.
     *
     * Every exit from that hold routes through here. The box no longer clears a stale inhibit for us
     * on the next SUBSCRIBE in the reattach case, so an inhibit we set is an inhibit we own, and
     * forgetting one leaves Bluetooth off with no BT phase ever emitted — which also means no
     * watchdog arms and the app never notices.
     */
    private fun releaseSessionRadioHold(why: String) {
        if (!radiosHeldForSession) return
        radiosHeldForSession = false
        log.i("releasing the mid-session radio hold — $why")
        runCatching { act.setBoxRadios(true) }.onFailure { log.e("CT_RADIO on failed: ${it.message}") }
    }

    /** Abandon recovery: the condition that started it is gone. */
    private fun cancelRecovery() {
        retryTimer?.cancel(false); retryTimer = null
        rung = 0
        skipped1 = false
        skippedAtRung = 0; stalledWhy = null; stalledAt = 0L
    }

    /**
     * Stand the ladder's self-driving retry down while something demonstrably IS happening.
     *
     * [LADDER_RECHECK_MS] exists so a silent phone still escalates — nothing external calls us back
     * when nothing is going on. But the retry it arms is blind: it fires on a fixed 20 s schedule
     * carrying the ORIGINAL `why`, regardless of what the box and phone have achieved since. A
     * handshake in flight is not silence, and rung 2 is `MGMT_RESTART_WIRELESS` — the one rung that
     * tears down the box's whole wireless stack, `carplayd` included.
     *
     * Device-observed twice on 2026-09-08, the second time with the ladder shooting a handshake in
     * the back mid-authentication:
     *
     *     12:07:31.778  rung 1 (CT_RADIO off/on)
     *     12:07:50.160  BT_PHASE LINK_UP        -> pairing
     *     12:07:51.150  BT_PHASE AUTHENTICATING
     *     12:07:51.779  rung 2 (MGMT_RESTART_WIRELESS)   <-- 20.001 s after rung 1
     *     12:07:52.296  BT_PHASE WIFI_HANDOFF / PROJ_MODE WIRELESS_CP
     *     12:07:53.284  BT_PHASE IDLE / PROJ_MODE NONE   <-- collapsed
     *     12:07:55.659  BOX_HEALTH 0x43                  <-- airplayd gone
     *
     * The phone then 200-OKs every nudge forever and never dials back, because the receiver it is
     * being pointed at no longer exists. iOS records it as `EAAccessoryLeft`.
     *
     * This deliberately does NOT reset [rung]. The ladder's position and cooldowns are earned state
     * and a phone that stalls again must resume where it left off — see [escalate]'s contract that
     * only a real session ([onSessionUp]) resets it. All this does is yield the CLOCK to the
     * narrower deadline the new phase just armed ([armHandoffWatchdog] at 45 s, or
     * [INBOUND_TIMEOUT_MS] at 15 s), each of which re-enters [escalate] on expiry. So a genuinely
     * stuck phone still escalates, only never on top of live progress.
     *
     * Exactly the rule [onDialAccepted] already applies to `handoffTimer`: once a narrower clock
     * owns the deadline, the broader one must let go.
     */
    private fun deferLadder(progress: String) {
        // `pending`, not `!= null` (v5.7 F004): after the ladder exhausts, retryTimer still holds the
        // future that just fired, and LINK_UP then logged "standing down at rung 3" with nothing armed.
        if (!pending(retryTimer)) return
        retryTimer?.cancel(false); retryTimer = null
        log.i("standing the recovery ladder down at rung $rung — $progress (its own deadline now owns the clock)")
    }

    private fun cancelAllTimers() {
        graceTimer?.cancel(false); graceTimer = null
        inboundTimer?.cancel(false); inboundTimer = null
        ladderTimer?.cancel(false); ladderTimer = null
        // Never drop a pending radios-ON. Losing it strands the box with its radios inhibited.
        if (radioOnPending) {
            radioOnPending = false
            log.w("cancelling timers with a radios-ON edge pending — sending it now instead of dropping it")
            val sent = runCatching { act.setBoxRadios(true) }
                .onFailure { log.e("CT_RADIO on failed: ${it.message}") }
                .getOrDefault(false)
            if (!sent) {
                // Same rule as [armRadioOnEdge]: false means the write never left, so the inhibit we
                // own is still standing and something has to stay pending to clear it.
                log.e("CT_RADIO on was not dispatched — retrying once in 2s")
                radioOnPending = true
                ladderTimer = arm(ladderTimer, 2_000L) {
                    radioOnPending = false
                    if (!act.setBoxRadios(true)) log.e("CT_RADIO on refused twice — the box's radios are left DOWN")
                }
            }
        }
        retryTimer?.cancel(false); retryTimer = null
        handoffTimer?.cancel(false); handoffTimer = null
    }
}
