package zeno.gmccpa

import java.util.concurrent.atomic.AtomicBoolean

/**
 * OD-7A (owner, 2026-10-07): "a start was refused because no hotspot credentials are set, and is
 * waiting for them." Pure (no android.*), so the host JUnit tests drive the exact latch SessionCore uses.
 *
 * - [block]: autoStart or a USB attach refused for missing credentials.
 * - [clear]: a start got past the gate, or the operator pressed Stop. A Stop withdraws the pending
 *   start, so a later Save must not start a session the operator stopped.
 * - [resumeOnSave]: the dialog was saved. True exactly once, and only with usable credentials, so
 *   the caller runs the start that was waiting.
 * - [repromptOnAttach]: a view attached while a refused start is still waiting and the credentials
 *   are still missing, so the dialog opens again. This covers a refusal while no view was attached.
 */
internal class CredsLatch {
    private val blocked = AtomicBoolean(false)

    val isBlocked: Boolean get() = blocked.get()

    fun block() = blocked.set(true)

    fun clear() = blocked.set(false)

    fun resumeOnSave(credsMissing: Boolean): Boolean = !credsMissing && blocked.compareAndSet(true, false)

    fun repromptOnAttach(credsMissing: Boolean): Boolean = credsMissing && blocked.get()
}
