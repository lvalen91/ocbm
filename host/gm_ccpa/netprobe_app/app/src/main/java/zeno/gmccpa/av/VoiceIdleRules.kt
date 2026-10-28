package zeno.gmccpa.av

/**
 * Pure voice-routing decisions that [VoiceStreams] and [VoiceRouter] call: the assistant edge, the
 * NAV_DURING_VOICE safety net, the focus stop rules, the CALL-slot backstop log and the head drain.
 * [RouteLatch] below this object is the one stateful piece (V4a).
 *
 * Kept free of Android types on purpose: every input is a clock reading or a flag the caller already
 * holds, so these rules run under host-JVM JUnit (`VoiceIdleRulesNavTest`) with the real numbers from
 * the bench captures, instead of only on the head unit. Nothing in this object locks, allocates or logs.
 *
 * v5.9 UC2 (D019): the v5.7 idle-window rules (the F065 end-of-turn shortcut, the F064 AU-clock
 * choice and the idle test) and their fixture test are gone — since v5.8 V1b a voice sink lives from
 * its stream's BEGIN to its END and nothing called them.
 */
object VoiceIdleRules {

    // ---- plan v5.8: the U10 NAV focus park / re-acquire / route rules (v5.7) are gone — owner
    // 2026-10-07, OD-8C = YES: NAV takes no focus (stock), so there is nothing to park or re-acquire,
    // and the U10-a Siri guard retires with it. What remains below: the assistant edge, navSuppressed
    // (the non-default DROP safety net), the focus stop rules (V2) and the route latch (V4a).

    /**
     * Siri is live for the media pause (onAssistant): the speechRecognition
     * stream is between BEGIN and END, OR iOS's speech START edge is younger than [leadMs] and no
     * ASSISTANT BEGIN has come since it — the modes edge as an EARLY start only (plan v5.8 V1b). A
     * START with no stream behind it expires at [leadMs]; an END edge is not needed (the stream's END
     * ends it).
     */
    fun assistantLive(
        nowMs: Long,
        streamLive: Boolean,
        ctrlSpeech: Boolean,
        ctrlSpeechAtMs: Long,
        leadMs: Long,
        lastAssistantBeginAtMs: Long,
    ): Boolean =
        streamLive || (ctrlSpeech && nowMs - ctrlSpeechAtMs < leadMs && lastAssistantBeginAtMs < ctrlSpeechAtMs)

    /**
     * NAV_DURING_VOICE (plan v5.8 V2), the ONLY read of [VoiceLatency.NAV_DURING_VOICE] (its default):
     * PLAY_LIVE (the default, owner OD-8D): never. DROP (safety net): suppress nav while a CALL or
     * ASSISTANT stream is between BEGIN and END, OR iOS's modes say a call is up (phone ≠ 0; covers
     * the Siri-placed-call handover 18:32:19.793 → 20.765 and a held call).
     */
    fun navSuppressed(
        callLive: Boolean,
        assistantLive: Boolean,
        ctrlCall: Boolean,
        mode: VoiceLatency.NavDuringVoice = VoiceLatency.NAV_DURING_VOICE,
    ): Boolean = mode == VoiceLatency.NavDuringVoice.DROP && (callLive || assistantLive || ctrlCall)

    /** Why nav is suppressed, for `nav: DROPPED (call|siri)`. */
    fun suppressedBy(callLive: Boolean, ctrlCall: Boolean): String = if (callLive || ctrlCall) "call" else "siri"

    /** The track has played what was written since the last flush, to within one period. */
    fun headDrained(headFrames: Long, writtenFrames: Long, periodFrames: Long): Boolean =
        headFrames >= writtenFrames - periodFrames

    /** What a voice slot does at its stream's END (plan v5.8 V2 stop rules). */
    enum class StopRule {
        RELEASE,
        /** CALL / ALERT while iOS says a call is up: kept until phone 1→0 (a held call may tear 100 down). */
        HOLD_UNTIL_PHONE_IDLE,
        /** ASSISTANT while a call is up (a Siri-placed call): kept, released at the CALL BEGIN's GRANTED or phone 1→0. */
        HANDOVER,
    }

    fun focusStopRule(p: VoiceLatency.Purpose, ctrlCall: Boolean): StopRule = when (p) {
        VoiceLatency.Purpose.CALL, VoiceLatency.Purpose.ALERT ->
            if (ctrlCall) StopRule.HOLD_UNTIL_PHONE_IDLE else StopRule.RELEASE
        VoiceLatency.Purpose.ASSISTANT -> if (ctrlCall) StopRule.HANDOVER else StopRule.RELEASE
        VoiceLatency.Purpose.NAV -> StopRule.RELEASE
    }

    /** A focus request result as the stream sees it: GRANTED plays; anything else mutes it for life (stock). */
    fun focusOutcome(requestResult: Int): Boolean = requestResult == FocusCodes.REQUEST_GRANTED

    /**
     * The lost-phone-1→0 backstop is LOG-ONLY until U18 is measured (a call on hold may tear 100
     * down): a held CALL slot with no live CALL stream for [logMs] after its END logs a W, once.
     */
    fun callSlotBackstopLog(slotHeld: Boolean, callLive: Boolean, nowMs: Long, callEndAtMs: Long, logMs: Long): Boolean =
        slotHeld && !callLive && callEndAtMs > 0 && nowMs - callEndAtMs >= logMs
}

/**
 * Plan v5.8 V4a: the route latch, from EDGES (F160/Q024). Replaces VIR.routeActive/turnsFresh and
 * VR's ROUTE_FRESH_MS (v5.7): no recency expiry — a 0x5201 gap of 28 s inside a live route (cp8) must
 * not end it.
 *  - **active**: a routeGuidanceState > 0 record (2/4/5 never observed; treated as active), or
 *    TurnByTurn turns ≠ 0;
 *  - **ended**: at once when the LATEST state is 0 AND the latest turns is 0 (cp8: both within ≤ 9 ms
 *    at each of its 5 ends); or when ONE of them has said "end" for [singleMs] with no contradiction
 *    since (a later state > 0 record or turns ≠ 0 frame) — the other end signal was lost;
 *  - turns = 0 at session start → inactive.
 * Pure; [onTurns]/[onState] are called on cp-meta, [evaluate] on the sweeper — callers synchronize
 * (VoiceRouter does: all three are @Synchronized here).
 */
class RouteLatch(private val singleMs: Long = ROUTE_END_SINGLE_MS) {
    data class Verdict(val active: Boolean, val cause: String)

    private var active = false
    private var cause = "no route"
    private var turns: Boolean? = null
    private var turnsAt = 0L
    private var state = -1
    private var stateAt = 0L
    /** Last frame/record that said "route on" — the contradiction stamps. */
    private var turnsOnAt = 0L
    private var stateOnAt = 0L
    /** When the latch last ended (-1 = never). */
    private var endedAt = -1L

    @Synchronized fun onTurns(nonZero: Boolean, atMs: Long) {
        if (turns != nonZero) { turns = nonZero; turnsAt = atMs }
        if (nonZero) turnsOnAt = atMs
    }

    @Synchronized fun onState(st: Int, atMs: Long) {
        if (st < 0) return
        if (st != state) { state = st; stateAt = atMs }
        if (st > 0) stateOnAt = atMs
    }

    @Synchronized fun evaluate(nowMs: Long): Verdict {
        if (!active) {
            // Only an "on" NEWER than the last end re-activates: after a lone-signal end the other
            // signal's last value is stale (its "on" predates the end) and must not flap the latch.
            if (turns == true && turnsOnAt > endedAt) { active = true; cause = "turns ≠ 0" }
            else if (state > 0 && stateOnAt > endedAt) { active = true; cause = "routeGuidanceState=$state" }
            return Verdict(active, cause)
        }
        val turnsEnd = turns == false
        val stateEnd = state == 0
        when {
            turnsEnd && stateEnd -> { active = false; endedAt = nowMs; cause = "routeGuidanceState=0 and turns=0" }
            turnsEnd && nowMs - turnsAt >= singleMs && stateOnAt < turnsAt ->
                { active = false; endedAt = nowMs; cause = "turns=0 for ${singleMs / 1000}s, uncontradicted (the state-0 record was lost)" }
            stateEnd && nowMs - stateAt >= singleMs && turnsOnAt < stateAt ->
                { active = false; endedAt = nowMs; cause = "routeGuidanceState=0 for ${singleMs / 1000}s, uncontradicted (turns=0 was lost)" }
        }
        return Verdict(active, cause)
    }

    companion object {
        const val ROUTE_END_SINGLE_MS = 5_000L
    }
}

