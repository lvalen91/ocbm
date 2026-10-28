package zeno.gmccpa

/**
 * The adapter Bluetooth name on the launcher screen, serialised against the paths that clear it
 * (wave 3, fix round 2). Pure (no android.*), so the host JUnit tests drive the exact gate SessionCore
 * uses.
 *
 * A MGMT_INFO reply is read on one thread (the probe's `ops` thread for the link-up snapshot, the
 * command executor for the INFO button and the HOST_GONE re-read) while the link can die or be
 * stopped on another (`ocbm-read`'s transport-death edge, Stop on the command executor). Without this
 * gate a reply read just before the box went away could paint its name AFTER the clear.
 *
 * The rule: every clearing path FIRST makes the link read as not live (the transport-death CAS on
 * `hostGoneDelivered`, `OcbmProbe.stop()` setting `stopped`, the probe being dropped), THEN calls
 * [clear]. [offer] re-checks liveness with [stillLive] INSIDE the same lock and drops the reply when
 * the link it came from is gone. Whichever order the two take the lock in, the screen ends cleared:
 * an offer before the clear is overwritten by it, and an offer after it sees the dead link.
 */
internal class BoxNameGate(private val show: (String) -> Unit) {
    enum class Offer { SHOWN, UNCHANGED, STALE }

    private val lock = Any()
    private var shown = ""

    /** The name on screen now ("" = none). */
    fun current(): String = synchronized(lock) { shown }

    /** Show [name] if the link it was read from is still live; never shows a blank. */
    fun offer(name: String, stillLive: () -> Boolean): Offer = synchronized(lock) {
        if (!stillLive()) return Offer.STALE
        if (name == shown) return Offer.UNCHANGED
        shown = name
        show(name)
        Offer.SHOWN
    }

    /** Remove the name. True when one was on screen. Call AFTER the link reads as not live. */
    fun clear(): Boolean = synchronized(lock) {
        if (shown.isEmpty()) return false
        shown = ""
        show("")
        true
    }
}
