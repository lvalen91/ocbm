package zeno.gmccpa

import java.util.concurrent.atomic.AtomicBoolean

/**
 * The OD-8B decision logic of plan v5.8 V5, pulled out of SessionCore so it runs on the host JVM
 * (RekeyFlowTest, tools/jvm_tests.txt). Pure: every side effect is an injected function.
 *
 * - [onLinkUp]: an OCBM link came up. If the persisted rekey state is PENDING and this process has
 *   not tried yet, queue ONE automatic Forget Pairing on the caller's command thread. Once per
 *   process: a partial clear (box bond not released) leaves the state PENDING for the driver's
 *   hand-pressed Forget Pairing — it must not tear the receiver down on every link.
 * - [run]: the queued body. Re-reads the state first: a pair-verify may have cleared it in between,
 *   and then the fresh pairing must NOT be thrown away. Whatever the forget achieved, the link it
 *   saw is still up, so the supervisor is handed the same BOX_LINKED report the start path gave
 *   it (the forget told it "stopped"; BOX_LINKED only moves forward, so this is harmless if ahead).
 * - [onForgetDone]: a forget that BEGAN under PENDING and cleared both halves re-asserts FORGOTTEN
 *   (force): a pair-verify that cleared the flag between the forget's first read and its receiver
 *   stop ran on the receiver just stopped, so its pairing went with the peer store and the hint must
 *   stay up. A partial clear leaves PENDING so the hint keeps naming the Forget Pairing control.
 * - [onPairVerified]: a pair-verify under the current key ends the migration.
 */
class RekeyFlow(
    private val store: Store,
    /** Forget Pairing, both halves; returns true when both were cleared. */
    private val forget: () -> Boolean,
    /** Is the OCBM link still subscribed right now? */
    private val linkSubscribed: () -> Boolean,
    /** `supervisor.onBoxLinked(mfiProven)` for the link that is still up. */
    private val reportLinked: () -> Unit,
    private val emit: (String) -> Unit
) {
    /** The persisted state ([AccessoryKey] on the device; a map in tests). */
    interface Store {
        fun state(): AccessoryKey.Rekey
        fun markForgotten(force: Boolean): Boolean
        fun clear(): Boolean
    }

    private val attempted = AtomicBoolean(false)

    /** True when the one automatic run was queued via [queue]. */
    fun onLinkUp(queue: (() -> Unit) -> Unit): Boolean {
        if (store.state() != AccessoryKey.Rekey.PENDING) return false
        if (!attempted.compareAndSet(false, true)) return false
        queue { run() }
        return true
    }

    /** The queued body; returns what the forget achieved, or null when it was skipped. */
    fun run(): Boolean? {
        if (store.state() != AccessoryKey.Rekey.PENDING) return null
        emit("accessory key changed since the stored pairings were made — forgetting the old pairing automatically (once, OD-8B)")
        val clean = forget()
        if (linkSubscribed()) reportLinked()
        return clean
    }

    /** Returns true when FORGOTTEN was (re-)asserted. */
    fun onForgetDone(rekeyWas: AccessoryKey.Rekey, boxOk: Boolean, appOk: Boolean): Boolean {
        if (rekeyWas != AccessoryKey.Rekey.PENDING || !boxOk || !appOk) return false
        store.markForgotten(force = true)
        return true
    }

    /** Returns true when a migration was in progress and is now cleared. */
    fun onPairVerified(): Boolean {
        if (store.state() == AccessoryKey.Rekey.NONE) return false
        store.clear()
        return true
    }
}
