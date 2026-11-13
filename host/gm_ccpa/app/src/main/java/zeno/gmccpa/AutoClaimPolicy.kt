package zeno.gmccpa

/**
 * plan v5.9 D6 (D006, owner 2026-10-08 "fix D006"): the pure decisions behind the auto-claim — claim
 * the adapter on attach, box reboot and boot with the app closed. No android.* here, so every rule is
 * a tier-0 JUnit test (AutoClaimPolicyTest). [AutoClaim] and `SessionCore.autoClaim` are the wiring.
 */
object AutoClaimPolicy {

    /** What the adapter looks like at the decision (from `SessionCore.readAdapterState`). */
    enum class Adapter { ABSENT, WRONG_MODE, NO_PERMISSION, PRESENT }

    enum class Decision(val why: String) {
        SKIP_STOPPED("operator Stop this boot (or a Stop is queued)"),
        YIELD_RESTART("Restart session is running; it re-claims"),
        SKIP_ABSENT("no adapter on USB"),
        SKIP_WRONG_MODE("the adapter is not in CarPlay mode (0x2d00)"),
        SKIP_NO_GRANT("no USB permission for the adapter — no dialog from the background; Start asks"),
        YIELD_CLAIM("a relink or a claim wait already owns the claim"),
        NOOP("the adapter link is already up"),
        RELINK_LIVE("CarPlay is live — restoring the link without CT_SUBSCRIBE"),
        BLOCK_CREDS("no hotspot credentials — the dialog opens with the launcher"),
        SKIP_FLAP("the adapter failed its HELLO on the last background claims — waiting for Start or a good link"),
        CLAIM("claiming the adapter"),
    }

    data class Inputs(
        val suppressed: Boolean,
        val stopRequested: Boolean,
        val restartInProgress: Boolean,
        val adapter: Adapter,
        val relinkOwner: Boolean,
        val claimWaitActive: Boolean,
        val hasLink: Boolean,
        val sessionLive: Boolean,
        val credsMissing: Boolean,
        val flapTripped: Boolean,
    )

    /**
     * The D6 table, first match wins. [foreground] = the dormant UsbAttachActivity route
     * (`SessionCore.onAdapterAttached`): it keeps its foreground semantics — the USB dialog is allowed
     * and the wait is ten minutes — so the adapter rows (3-5) and the flap guard (10) do not apply to it.
     */
    fun decide(i: Inputs, foreground: Boolean = false): Decision = when {
        i.suppressed || i.stopRequested -> Decision.SKIP_STOPPED
        i.restartInProgress -> Decision.YIELD_RESTART
        !foreground && i.adapter == Adapter.ABSENT -> Decision.SKIP_ABSENT
        !foreground && i.adapter == Adapter.WRONG_MODE -> Decision.SKIP_WRONG_MODE
        !foreground && i.adapter == Adapter.NO_PERMISSION -> Decision.SKIP_NO_GRANT
        i.relinkOwner || i.claimWaitActive -> Decision.YIELD_CLAIM
        i.hasLink -> Decision.NOOP
        i.sessionLive -> Decision.RELINK_LIVE
        i.credsMissing -> Decision.BLOCK_CREDS
        !foreground && i.flapTripped -> Decision.SKIP_FLAP
        else -> Decision.CLAIM
    }

    /**
     * D6 rev c C1: the LINK hold a trigger acquired is released at the end of its task whenever there
     * is no link and no relink owns the claim — WHATEVER the decision (a BLOCK_CREDS or SKIP_FLAP kept
     * a microphone-type FGS and its notification up with nothing linked under rev b's rule). The one
     * stated keep is a relink in progress: it keeps the process out of the freezer through its wait,
     * and its own ends release or re-acquire. A hold this trigger did not add is never released here.
     */
    fun shouldRelease(acquired: Boolean, hasLinkAtEnd: Boolean, relinkOwnerAtEnd: Boolean): Boolean =
        acquired && !hasLinkAtEnd && !relinkOwnerAtEnd

    /**
     * R10: the credentials a headless process uses — the attached view's, else the last a view or the
     * `--es ssid/pass` path held (process lifetime; a blank pair there is a deliberate test input and
     * is NOT replaced by the prefs), else the saved ones (HotspotPrefs), else none.
     */
    fun <C : Any> resolveCreds(view: C?, last: C?, prefs: C?): C? = view ?: last ?: prefs

    /**
     * OD-9D (owner 2026-10-08: a Stop does NOT survive a head-unit reboot), D6 rev c C2. A boot
     * identity read at Stop and now; -1 / "" = unreadable. The first identity readable at BOTH times
     * decides: `Settings.Global.BOOT_COUNT`, else `/proc/sys/kernel/random/boot_id`. Neither readable
     * at both -> NOT the same boot (fails toward OD-9D: an unprovable Stop does not suppress).
     */
    fun sameBoot(stopCount: Int, stopBootId: String, nowCount: Int, nowBootId: String): Boolean = when {
        stopCount >= 0 && nowCount >= 0 -> stopCount == nowCount
        stopBootId.isNotEmpty() && nowBootId.isNotEmpty() -> stopBootId == nowBootId
        else -> false
    }

    /** Which identity [sameBoot] would use for a Stop taken now — for the `stop flag: boot identity` line. */
    fun identityName(count: Int, bootId: String): String = when {
        count >= 0 -> "boot_count"
        bootId.isNotEmpty() -> "boot_id"
        else -> "none"
    }

    /**
     * The flap guard (process-scoped): background CLAIMs that end claimed but without HELLO, with the
     * adapter still enumerated, [max] times in a row (RelinkChain.MAX_DEAD_RELINKS) stop further
     * background claims until an explicit start or any successful HELLO.
     */
    class FlapGuard(private val max: Int = RelinkChain.MAX_DEAD_RELINKS) {
        @Volatile var streak = 0; private set
        val tripped: Boolean get() = streak >= max
        /** A background CLAIM finished. */
        @Synchronized fun onClaimEnd(claimed: Boolean, helloOk: Boolean, enumerated: Boolean) {
            when {
                helloOk -> streak = 0
                claimed && enumerated -> streak++
                // Not claimed, or the adapter left: not a flap of THIS adapter; the count stands.
            }
        }
        /** Any HELLO (autoStart, relink) or an explicit Start/Restart. */
        @Synchronized fun reset() { streak = 0 }
    }
}
