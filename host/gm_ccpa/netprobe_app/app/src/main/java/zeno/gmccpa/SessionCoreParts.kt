package zeno.gmccpa

import zeno.gmccpa.pair.MfiRelay

/**
 * The pure-Kotlin halves of [SessionCore] (v5.7 U4), kept free of Android types so the host-JVM unit
 * tests can exercise them (unit_test_v57_U4.sh).
 */

/**
 * Two start/stop rules of [SessionCore] (B-F2, 2026-10-08), pure so `SessionCorePartsTest` holds them.
 *
 * [subscribeAllowed]: the `subscribe` lambda `autoStart` hands `OcbmProbe.runAll`, evaluated by the
 * probe right before the CT_SUBSCRIBE radio-wake edge. A Stop pressed while the bring-up was past
 * its claim wait used to be unable to stop that edge: the box's radios came up for a session the
 * queued stop then tore down (one presence 0->1->0 cycle, and the box's flap detector counts
 * those). Precedent: `claimInBackground`'s lambda. Known cosmetic cost, documented rather than
 * fixed: the probe's STEP-3-SKIPPED narration says "a CarPlay session is live", which is the only
 * reason it knows, and `autoStart` still reports the link up before the stop runs.
 *
 * [probeStopQuick]: the operator Stop's `OcbmProbe.stop(quick)`. The full stop pulls the box logs
 * (4 x 6 s of timeouts on a dead link); with no link there is nothing to pull over — the same rule
 * `OcbmProbe.runAllLocked` applies before re-claiming.
 */
object StartPolicy {
    fun subscribeAllowed(stopRequested: Boolean): Boolean = !stopRequested
    fun probeStopQuick(hasLink: Boolean): Boolean = !hasLink
}

/**
 * Whether a MainActivity launch intent's `--es run` verb is replayed (D-F2, 2026-10-08). Android
 * re-delivers the ORIGINAL launch intent on every recreate (configuration change, process death
 * restored from recents, the task re-selected from history), so a one-shot verb such as
 * `ocbm_forget`, `carplay_stop` or `export_log_raw` ran again each time. A verb runs only on a fresh
 * delivery: no saved instance state, and the intent not re-launched from history. The default
 * auto-start is NOT gated on this — a launcher restored from recents must still bring the session
 * up. The flag literal is `Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY` (0x00100000), spelled out so
 * this stays host-testable.
 */
object IntentReplay {
    const val FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY = 0x00100000
    fun verbRuns(hasSavedState: Boolean, intentFlags: Int): Boolean =
        !hasSavedState && (intentFlags and FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY) == 0
}

/**
 * The one attached view of the process-scoped session core, plus the last thing shown on it.
 *
 * [attach] replaces the view and renders the stored state onto it at once, so a newly created
 * MainActivity shows the phase the core already holds instead of a fresh "idle" (this replaces the
 * old rebind seed). [detach] is COMPARE-AND-CLEAR (v5.7.1, Review 2): a recreated singleTask
 * MainActivity can run onCreate (attach) BEFORE the old instance's onDestroy (detach) — CarPlayActivity's
 * KDoc documents the same ordering for its own recreation — and an unconditional clear there would
 * detach the live view.
 *
 * [update] stores and forwards under the same lock as [attach], so an update racing an attach is
 * either stored before the attach (the attach renders it) or forwarded after it (to the new view):
 * no update can fall between the two and be lost to the screen.
 */
class ViewSlot<V : Any, S : Any>(private val render: (V, S) -> Unit) {
    private val lock = Any()
    @Volatile private var view: V? = null
    private var last: S? = null

    /** The attached view, or null. A read, not a reservation: it can be detached right after. */
    fun current(): V? = view

    /** The last stored state, or null if nothing was ever published. */
    fun stored(): S? = synchronized(lock) { last }

    fun attach(v: V) {
        synchronized(lock) {
            view = v
            last?.let { render(v, it) }
        }
    }

    /** Clears only if [v] is still the attached view. Returns whether it was. */
    fun detach(v: V): Boolean = synchronized(lock) {
        if (view === v) { view = null; true } else false
    }

    /** Store `transform(last)` and hand the attached view (if any) to [forward]. */
    fun update(transform: (S?) -> S, forward: (V) -> Unit) {
        synchronized(lock) {
            last = transform(last)
            view?.let(forward)
        }
    }
}

/**
 * The MFi relay the receiver is built with (v5.7 F025): it holds NO probe. Every call resolves the
 * CURRENT one through [resolve] (production: `SessionHolder.ocbmProbe?.mfiRelay()`), so a receiver
 * outlives `ocbm_stop`, Restart Session and every probe rebuild without staying bound to a dead link.
 * Before this the receiver captured `ocbmProbe.mfiRelay()` once, in its constructor, and auth-setup
 * then failed over a healthy link after an ocbm_stop -> ocbm_link, because the captured relay read
 * the stopped probe's null client.
 *
 * With no probe it raises the same IOException("no OCBM link") the probe's own relay raises with no
 * client (OcbmProbe.mfiRelay), so the native caller sees one failure shape either way.
 */
class ProcessMfiRelay(
    private val resolve: () -> MfiRelay?,
    private val linked: () -> Boolean,
    private val onNoProbe: (String) -> Unit = {},
) : MfiRelay {
    private fun target(what: String): MfiRelay = resolve() ?: run {
        onNoProbe("relay $what requested with no OCBM probe")
        throw java.io.IOException("no OCBM link")
    }

    override fun copyCertificate(): ByteArray = target("cert").copyCertificate()
    override fun createSignature(digest: ByteArray): ByteArray = target("sign").createSignature(digest)
    override fun copyCertificateFast(): ByteArray = target("cert").copyCertificateFast()
    override fun createSignatureFast(digest: ByteArray): ByteArray = target("sign").createSignatureFast(digest)

    /**
     * The receiver self-test's detail line (RX "MFi relay"). The check's `ok` stays true — the
     * delegate always exists — because autoStart builds the receiver (and runs the self-test) BEFORE
     * runAll makes the link, and SessionSupervisor turns any `ok = false` into RX_UNHEALTHY with no
     * guard: a link-gated `ok` would mark every cold start unhealthy (v5.7.1, Review 1).
     */
    fun describe(): String =
        if (runCatching(linked).getOrDefault(false)) "delegating to the OCBM probe — link up"
        else "delegating to the OCBM probe — no link yet (auth-setup fails until the adapter is linked)"
}
