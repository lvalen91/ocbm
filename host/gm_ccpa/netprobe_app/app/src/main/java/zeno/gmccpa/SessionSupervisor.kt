package zeno.gmccpa

import com.carlink.ocbm.Ocbm
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
}

/**
 * Owns [RxPhase] and the recovery ladder.
 *
 * Everything runs on one scheduler thread, so transitions and timers cannot race each other and no
 * field here needs locking. Callers post events; the supervisor decides. It never blocks: the actions
 * it invokes are expected to hand off (they are dispatched onto the caller's own executor).
 */
class SessionSupervisor(private val act: Actions) {

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
        /** Surface phase + a human sentence. */
        fun report(phase: RxPhase, detail: String)
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
    }

    private val log = ProbeLog.sub("sup")
    private val sched: ScheduledExecutorService =
        Executors.newSingleThreadScheduledExecutor { r -> Thread(r, "rx-supervisor").apply { isDaemon = true } }

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

    // ---- event intake ---------------------------------------------------------------------------
    // All of these are safe to call from any thread; they only queue work onto the scheduler.

    fun onReceiverReady(ok: Boolean, detail: String) = post {
        if (ok) to(RxPhase.RX_READY, detail)
        else to(RxPhase.RX_UNHEALTHY, "receiver NOT ready: $detail")
    }

    /**
     * The box is linked. [mfiProven] must be the OBSERVED outcome of a real cert+signature exchange,
     * never an assumption: this label used to read "MFi proven" unconditionally, on a path the caller
     * reached even when no adapter was ever claimed.
     */
    fun onBoxLinked(mfiProven: Boolean) = post {
        to(RxPhase.BOX_LINKED, if (mfiProven) "box claimed, MFi proven" else "box claimed — MFi NOT proven")
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
    fun onBoxHealth(flags: Int) = post {
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
        if (was < 0) {
            log.i("first box health 0x%02x [%s] — baseline, not evaluated".format(flags, Ocbm.bhString(flags)))
            return@post
        }
        // Otherwise act only on a CHANGE into the unhealthy set, or a standing fault would re-trigger
        // the ladder every couple of seconds.
        val wasMissing = BH_REQUIRED_BRIDGE and was.inv()
        if (missing == wasMissing) return@post
        val detail = "adapter is missing ${Ocbm.bhString(missing)} (has ${Ocbm.bhString(flags)})"
        // Rung 1 takes the box's radios down and back up; the controller going missing across that
        // edge is the rung working, not a new fault. See [RADIO_SETTLE_MS].
        if (System.currentTimeMillis() < radioSettleUntil) {
            log.i("box health regressed inside the rung 1 radio cycle — self-inflicted — not acting ($detail)")
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

    fun onBoxSubscribed() = post { to(RxPhase.ARMED, "box radios waking — waiting for the phone") }

    /**
     * The OCBM link went away.
     *
     * If CarPlay is streaming this is explicitly NOT a session-ending event: the A/V path runs over
     * Wi-Fi and does not depend on Bluetooth or on the box once it is up. Tearing the session down
     * here would be the app destroying a working session because a side-channel blinked.
     */
    fun onBoxLost() = post {
        // BOTH live-session phases, not just SESSION_UP: two independent producers deliver
        // SEV_HOST_GONE -- the box over CH_CTRL, and the heartbeat write-failure path, which
        // synthesises one directly and deliberately bypasses handleCtrl (OcbmClient). An ocbmd
        // restart while USB stays enumerated fires both: the first correctly parks us in
        // BOX_LOST_SESSION_UP, and the second used to fall through to the else branch and drop a
        // LIVE session to IDLE. That resumes discovery mid-session AND re-qualifies the box-health
        // handler, whose guard list names only these same phases -- so the next health regression
        // climbed the ladder into CT_RADIO off/on against streaming CarPlay. Idempotent instead.
        if (phase == RxPhase.SESSION_UP || phase == RxPhase.BOX_LOST_SESSION_UP) {
            boxLostMidSession = true
            to(RxPhase.BOX_LOST_SESSION_UP, "box gone but CarPlay is streaming — holding the session")
        } else {
            to(RxPhase.IDLE, "adapter dropped the link — press Start to re-subscribe")
        }
    }

    /**
     * The box is back. If it went away mid-session, its radios must stay down: the phone is already
     * connected over Wi-Fi and a fresh BT bring-up would only disturb it.
     */
    fun onBoxRelinked() = post {
        if (boxLostMidSession) {
            log.w("box returned after a mid-session drop — holding its radios DOWN (session is live)")
            if (act.setBoxRadios(false)) radiosHeldForSession = true
            else log.w("CT_RADIO off failed — the box may bring BT up on its own")
            boxLostMidSession = false
            to(RxPhase.SESSION_UP, "box back; radios held down, session intact")
        } else {
            to(RxPhase.BOX_LINKED, "box back")
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
        when (p) {
            Ocbm.BTP_LINK_UP, Ocbm.BTP_AUTHENTICATING -> {
                if (!replay) { armHandoffWatchdog(); deferLadder("phone is pairing over Bluetooth") }
                to(RxPhase.BT_PAIRING, "phone connecting over Bluetooth")
            }
            Ocbm.BTP_IDENTIFIED -> {
                if (!replay) deferLadder("phone identified over iAP2")
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

    fun onSessionUp() = post {
        cancelAllTimers()   // includes the ladder's own retry chain: a real session ends recovery
        rung = 0                       // a real session is the only proof the ladder can stop
        boxLostMidSession = false
        to(RxPhase.SESSION_UP, "CarPlay session is up")   // to() suspends discovery for this state
    }

    /**
     * The control connection went away. Hold still for [GRACE_MS] with discovery PAUSED — the phone
     * usually comes back by itself, and the 12 s rediscover churn would otherwise redial into the gap
     * and provoke a hijack of the session that was about to resume.
     */
    fun onSessionDown() = post {
        releaseSessionRadioHold("the session it was protecting has ended")
        to(RxPhase.GRACE, "session ended — holding ${GRACE_MS / 1000}s for the phone to return")
        graceTimer = arm(graceTimer, GRACE_MS) {
            if (phase != RxPhase.GRACE) return@arm
            log.w("phone did not return within ${GRACE_MS / 1000}s")
            // Leave GRACE FIRST. escalate() does not move the phase unless the ladder runs out, so
            // staying here would strand the machine in a state that suspends discovery with no timer
            // left to fire — the receiver would go quiet permanently and look idle while doing
            // nothing. to() restores discovery on the way out.
            to(RxPhase.ARMED, "grace expired — resuming discovery and recovering")
            escalate("the session ended and the phone did not come back")
        }
    }

    /**
     * [CarPlayRx] has counted enough accepted-but-unanswered dials to call it. This is the terminal
     * rung: three reproductions on 2026-08-27 showed nothing on this side clears it — not restarting
     * the app, not power-cycling the adapter. iOS reads our mDNS TXT and never resolves SRV, so it
     * holds no address for us. Reconnecting Bluetooth from the phone did clear it, every time.
     */
    fun onStalled(dials: Int) = post {
        cancelAllTimers()
        to(RxPhase.STALLED, "iPhone sees this receiver but will not connect ($dials tries) — " +
            "reconnect Bluetooth to the car from the iPhone")
    }

    /**
     * Everything was torn down deliberately (the Stop button, or `stopEverything`). Reset to IDLE and
     * drop the ladder: without this the supervisor kept a stale phase — potentially SESSION_UP, whose
     * invariant suspends discovery — across a teardown, and the next start-up inherited it.
     */
    fun onStopped() = post {
        releaseSessionRadioHold("everything was stopped")
        cancelAllTimers()
        rung = 0
        boxLostMidSession = false
        boxHealth = -1
        to(RxPhase.IDLE, "stopped")
    }

    /**
     * Driver pressed Recover.
     *
     * Runs the same ladder the supervisor runs on its own, but from the bottom and ignoring cooldowns:
     * the cooldowns exist to stop the machine hammering the box unattended, and an explicit human
     * request is not that. The rungs themselves stay in cheapest-first order — a person pressing a
     * button still should not get their box rebooted when re-announcing mDNS would have done it.
     *
     * The one thing it will NOT do is pretend to fix what it cannot. If the ladder runs out, the
     * result is the same terminal report as the automatic path: the remaining action is on the phone.
     */
    fun userRecover() = post {
        log.w("=== driver-requested recovery ===")
        cancelAllTimers()
        rung = 0
        java.util.Arrays.fill(lastRungAt, 0L)   // an explicit request is not rate-limited
        escalate("the driver asked for a recovery")
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
     */
    fun stop() {
        post {
            releaseSessionRadioHold("the supervisor is going away")
            cancelAllTimers()
            // Leave discovery RUNNING. The supervisor going away (Activity destroyed) must not leave
            // the receiver permanently unable to dial: the receiver outlives the UI by design.
            runCatching { act.resumeDiscovery() }
            sched.shutdown()
        }
    }

    // ---- the ladder -----------------------------------------------------------------------------

    /**
     * Try the next rung, cheapest first, honouring per-rung cooldowns.
     *
     * Rungs never repeat automatically without a cooldown, and the ladder only resets on a real
     * session ([onSessionUp]). Rung 3 is not an action — the evidence says the remaining fix is on the
     * phone, and an app that keeps silently retrying something that cannot work is worse than one that
     * says so.
     */
    private fun escalate(why: String) {
        if (phase == RxPhase.SESSION_UP || phase == RxPhase.BOX_LOST_SESSION_UP) {
            log.i("recovery abandoned — a session came up on its own")
            rung = 0
            return
        }
        val now = System.currentTimeMillis()
        while (rung < RUNG_COOLDOWN_MS.size) {
            val since = now - lastRungAt[rung]
            if (lastRungAt[rung] != 0L && since < RUNG_COOLDOWN_MS[rung]) {
                // WAIT — do not promote. Cooling means "not yet", not "never": skipping forward
                // answers a cheap rung's cooldown with a MORE expensive action, which is exactly how
                // MGMT_RESTART_WIRELESS became a *first* response and dropped a pairing in progress.
                // It also made STALLED reachable with every rung cooling and no action taken at all.
                val left = RUNG_COOLDOWN_MS[rung] - since
                log.i("rung $rung still cooling down (${left / 1000}s left) — waiting for it, not promoting")
                retryTimer = arm(retryTimer, left) { escalate(why) }
                return
            }
            val done = runRung(rung, why)
            // Stamp only a rung that actually RAN. A refused rung (no responder, no box link) costs
            // nothing and must cost nothing: burning its cooldown here walked the ladder to
            // exhaustion on actions that never happened.
            if (done) lastRungAt[rung] = now
            rung++
            if (done) {
                // Keep the ladder moving under its own power. Nothing external is guaranteed to call
                // us back: a silent phone produces no events at all.
                retryTimer = arm(retryTimer, LADDER_RECHECK_MS) { escalate(why) }
                return
            }
        }
        log.e("recovery ladder exhausted — $why")
        to(RxPhase.STALLED, terminalDetail())
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
    private fun runRung(n: Int, why: String): Boolean = when (n) {
        0 -> {
            log.w("recovery rung 0 (re-announce mDNS): $why")
            val ok = act.reannounce()
            if (!ok) log.w("rung 0 refused — responder down")
            ok
        }
        1, 2 -> if (!runCatching { act.boxLinkAlive() }.getOrDefault(false)) {
            log.w("recovery rung $n skipped — no OCBM link to the adapter, so a box-side action cannot help")
            false
        } else runBoxRung(n, why)
        else -> false
    }

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
                radioSettleUntil = System.currentTimeMillis() + RADIO_SETTLE_MS
                armRadioOnEdge()
                true
            }
        }
        2 -> { log.w("recovery rung 2 (MGMT_RESTART_WIRELESS): $why"); act.restartBoxWireless() }
        else -> false
    }

    /** Name the fault the driver can actually act on, rather than always blaming the phone. */
    private fun terminalDetail(): String =
        if (!runCatching { act.boxLinkAlive() }.getOrDefault(false))
            "no link to the CarPlay adapter — check it is plugged in"
        else "cannot recover automatically — reconnect Bluetooth to the car from the iPhone"

    // ---- plumbing -------------------------------------------------------------------------------

    private fun post(block: () -> Unit) {
        try { sched.execute { runCatching(block).onFailure { log.e("supervisor: ${it.javaClass.simpleName}: ${it.message}") } } }
        catch (_: Throwable) { /* shut down */ }
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
    private fun to(next: RxPhase, detail: String) {
        if (next != phase) log.i("${phase.label} -> ${next.label}  ($detail)")
        phase = next
        val quiet = next == RxPhase.SESSION_UP || next == RxPhase.BOX_LOST_SESSION_UP || next == RxPhase.GRACE
        runCatching { if (quiet) act.pauseDiscovery() else act.resumeDiscovery() }
            .onFailure { log.w("discovery toggle failed: ${it.message}") }
        act.report(next, detail)
    }

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
        if (retryTimer == null) return
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
