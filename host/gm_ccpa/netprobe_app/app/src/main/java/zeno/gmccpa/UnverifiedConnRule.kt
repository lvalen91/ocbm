package zeno.gmccpa

/**
 * The unverified-connection deadline (A-F1, 2026-10-08). Pure, so `UnverifiedConnRuleTest` runs it
 * on the host JVM; `CarPlayRx.handleNative` and `SessionCore` consult it.
 *
 * ## The defect
 * Anything on the hotspot can open a TCP connection to `:7011`. Until this rule the accept alone
 * stood the whole session up — the consumers (right: streams are SETUP within ~300 ms of RECORD),
 * but also the supervisor's SESSION_UP, which pauses discovery and cancels every recovery clock —
 * and a peer that kept sending plaintext requests was never cut off: the pump's idle backstop
 * needs 30 s of silence. A rogue (or a stalled real phone) that never completes pair-verify
 * therefore held the receiver out of recovery for as long as it liked, and a silent one ended as
 * LINK_LOST, the grace ladder's fast entry (rung-1 CT_RADIO off/on, for a connection that was
 * never a phone).
 *
 * ## The rule
 *  - [overdue]: an accepted control connection that has not completed pair-verify within
 *    [deadlineMs] of its accept is closed by the pump, whether it is sending or silent. The budget
 *    is `CarPlayRx.PAIR_VERIFY_MS` (10 s; measured verify-only 57 ms, and a first pairing's
 *    pair-setup is sub-second with no human in the loop — see that constant's KDoc).
 *  - The close is reported as [EndReason.UNVERIFIED]: never LINK_LOST (so the grace expiry's
 *    ladder is not `fast`), and distinct from the peer's own CLOSED so that the SessionCore gate
 *    ([reachesSupervisor]) can keep a deadline-closed, never-encrypted generation away from
 *    `SessionSupervisor.onSessionDown` — no GRACE and no ladder reset for a connection that was never
 *    a session. A real phone that connects and closes EARLY on its own still arrives as CLOSED and
 *    keeps today's GRACE (docs/04 §4b case 2): after the V5 rekey a phone failing pair-verify needs
 *    the ladder to reach the rung-3 forget-and-re-pair text.
 *  - `SessionCore` moves `sessionUp` and `supervisor.onSessionUp` from the accept to the
 *    pair-verified callback (option (a): the screen and the A/V consumers still stand up at accept).
 */
internal object UnverifiedConnRule {

    /** True once [now] is [deadlineMs] or more past [acceptedAt] with the channel still plaintext. */
    fun overdue(encrypted: Boolean, acceptedAt: Long, now: Long, deadlineMs: Long): Boolean =
        !encrypted && now - acceptedAt >= deadlineMs

    /** The end reason for a deadline close. Not LINK_LOST: see the class KDoc. */
    fun reason(): EndReason = EndReason.UNVERIFIED

    /**
     * Whether a session-down with [reason] is handed to the supervisor. A deadline close of a
     * never-encrypted generation is not: nothing it could tell the supervisor is about a phone —
     * UNLESS a verified session was up ([verifiedSessionWasUp]: SessionCore's `sessionUp` at the
     * moment of the call). That case is real (C1, 2026-10-08): gen1 verified; its pump releases the
     * incumbent slot BEFORE it decrements `liveConnections`, a newcomer admitted in that gap makes
     * gen1 end at count 1 (no session-down), and if the newcomer never verifies it ends UNVERIFIED at
     * count 0 — the only session-down the supervisor will ever get for gen1's SESSION_UP. Skipping it
     * would leave the phase SESSION_UP, with discovery paused, forever.
     */
    fun reachesSupervisor(reason: EndReason, verifiedSessionWasUp: Boolean): Boolean =
        verifiedSessionWasUp || reason != EndReason.UNVERIFIED
}
