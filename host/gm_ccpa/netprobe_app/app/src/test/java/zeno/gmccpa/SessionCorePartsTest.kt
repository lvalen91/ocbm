package zeno.gmccpa

import java.util.Collections
import java.util.concurrent.CountDownLatch
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import zeno.gmccpa.pair.MfiRelay

/** Plan v5.7 U4: the view slot of SessionCore (attach/detach/stored state) and the F025 relay. Host JVM. */
class SessionCorePartsTest {

    /** A fake view: records every value rendered or forwarded to it, in order. */
    private class View(val name: String) {
        val seen: MutableList<Int> = Collections.synchronizedList(ArrayList())
        fun last(): Int? = synchronized(seen) { seen.lastOrNull() }
    }

    private fun slot() = ViewSlot<View, Int> { v, s -> v.seen.add(s) }
    private fun ViewSlot<View, Int>.publish(n: Int) = update({ n }) { it.seen.add(n) }

    // ---- ViewSlot ---------------------------------------------------------------------------------

    @Test fun attachWithNothingStoredRendersNothing() {
        val s = slot(); val a = View("a")
        s.attach(a)
        assertTrue(a.seen.isEmpty())
        assertSame(a, s.current())
    }

    @Test fun headlessUpdateIsStoredAndRenderedOnTheNextAttach() {
        val s = slot()
        s.publish(1); s.publish(2)              // no view: stored only (the headless session core)
        assertEquals(2, s.stored())
        val a = View("a")
        s.attach(a)
        assertEquals(listOf(2), a.seen)          // the stored phase, not a fresh idle
    }

    @Test fun updateForwardsToTheAttachedView() {
        val s = slot(); val a = View("a")
        s.attach(a); s.publish(7)
        assertEquals(listOf(7), a.seen)
    }

    /** v5.7.1 Review 2: a recreated singleTask MainActivity's onCreate can precede the old onDestroy. */
    @Test fun oldDetachAfterNewAttachKeepsTheNewView() {
        val s = slot(); val old = View("old"); val new = View("new")
        s.attach(old)
        s.publish(1)
        s.attach(new)                            // new onCreate first
        assertFalse(s.detach(old))               // then the old onDestroy: compare-and-clear misses
        assertSame(new, s.current())
        s.publish(2)
        assertEquals(listOf(1, 2), new.seen)     // rendered stored 1 on attach, then forwarded 2
        assertEquals(listOf(1), old.seen)        // the dead view gets nothing after replacement
    }

    @Test fun detachOfTheCurrentViewClearsIt() {
        val s = slot(); val a = View("a")
        s.attach(a)
        assertTrue(s.detach(a))
        assertNull(s.current())
        s.publish(3)
        assertTrue(a.seen.isEmpty())
        assertEquals(3, s.stored())
        assertFalse(s.detach(a))                 // idempotent
    }

    /**
     * An update racing an attach is either stored before it (the attach renders it) or forwarded
     * after it (to the new view): the view attached last always ends on the last published value.
     */
    @Test fun updateRacingAttachIsNeverLost() {
        repeat(200) {
            val s = slot()
            val n = 2_000
            val go = CountDownLatch(1)
            val views = ArrayList<View>()
            val publisher = Thread { go.await(); for (i in 1..n) s.publish(i) }
            val attacher = Thread {
                go.await()
                for (k in 0 until 50) { val v = View("v$k"); synchronized(views) { views += v }; s.attach(v) }
            }
            publisher.start(); attacher.start(); go.countDown()
            publisher.join(); attacher.join()
            val lastView = synchronized(views) { views.last() }
            assertSame(lastView, s.current())
            assertEquals(n, lastView.last())
        }
    }

    // ---- ProcessMfiRelay (F025) -----------------------------------------------------------------

    private class FakeRelay(val tag: Byte) : MfiRelay {
        val calls = AtomicInteger()
        val fast = AtomicInteger()
        override fun copyCertificate(): ByteArray { calls.incrementAndGet(); return byteArrayOf(tag) }
        override fun createSignature(digest: ByteArray): ByteArray { calls.incrementAndGet(); return byteArrayOf(tag, digest[0]) }
        override fun copyCertificateFast(): ByteArray { fast.incrementAndGet(); return byteArrayOf(tag, 1) }
        override fun createSignatureFast(digest: ByteArray): ByteArray { fast.incrementAndGet(); return byteArrayOf(tag, 2) }
    }

    @Test fun noProbeRaisesTheProbesOwnNoLinkIOException() {
        val noted = AtomicReference<String?>(null)
        val r = ProcessMfiRelay(resolve = { null }, linked = { false }, onNoProbe = { noted.set(it) })
        try { r.copyCertificate(); fail("expected IOException") }
        catch (e: java.io.IOException) { assertEquals("no OCBM link", e.message) }
        assertTrue(noted.get()!!.contains("cert"))
        try { r.createSignatureFast(byteArrayOf(9)); fail("expected IOException") }
        catch (e: java.io.IOException) { assertEquals("no OCBM link", e.message) }
        assertTrue(noted.get()!!.contains("sign"))
    }

    /** The receiver outlives probe rebuilds: each call reaches the CURRENT probe, never a captured one. */
    @Test fun resolvesTheCurrentProbeOnEveryCall() {
        val cur = AtomicReference<MfiRelay?>(FakeRelay(1))
        val r = ProcessMfiRelay(resolve = { cur.get() }, linked = { cur.get() != null })
        assertEquals(1, r.copyCertificate()[0].toInt())
        cur.set(null)                            // ocbm_stop
        try { r.copyCertificate(); fail("expected IOException") } catch (_: java.io.IOException) {}
        val p2 = FakeRelay(2); cur.set(p2)       // ocbm_link builds a new probe
        assertEquals(2, r.createSignature(byteArrayOf(5))[0].toInt())
        assertEquals(1, p2.calls.get())
    }

    @Test fun fastVariantsDelegateToFast() {
        val p = FakeRelay(3)
        val r = ProcessMfiRelay(resolve = { p }, linked = { true })
        assertEquals(1, r.copyCertificateFast()[1].toInt())
        assertEquals(2, r.createSignatureFast(byteArrayOf(0))[1].toInt())
        assertEquals(2, p.fast.get()); assertEquals(0, p.calls.get())
    }

    /** v5.7.1 Review 1: the self-test's ok stays true; link state is in the detail only. */
    @Test fun describeNamesTheLinkStateAndToleratesAThrow() {
        val up = AtomicBoolean(false)
        val r = ProcessMfiRelay(resolve = { null }, linked = { up.get() })
        assertEquals("delegating to the OCBM probe — no link yet (auth-setup fails until the adapter is linked)", r.describe())
        up.set(true)
        assertEquals("delegating to the OCBM probe — link up", r.describe())
        val bad = ProcessMfiRelay(resolve = { null }, linked = { throw IllegalStateException("x") })
        assertTrue(bad.describe().contains("no link yet"))
    }

    // ---- B-F2 / D-F2 (2026-10-08) -------------------------------------------------------------

    @Test fun startPolicy_subscribeFollowsStopRequested() {
        assertTrue(StartPolicy.subscribeAllowed(stopRequested = false))
        assertFalse("a Stop queued behind the bring-up must not take the radio-wake edge", StartPolicy.subscribeAllowed(stopRequested = true))
        // The lambda autoStart hands runAll reads the flag at CALL time, not at capture time.
        var stop = false
        val gate: () -> Boolean = { StartPolicy.subscribeAllowed(stop) }
        assertTrue(gate())
        stop = true
        assertFalse(gate())
    }

    @Test fun startPolicy_operatorStopIsQuickOnlyWithoutALink() {
        assertTrue(StartPolicy.probeStopQuick(hasLink = false))
        assertFalse("a live link still gets the box-log pull", StartPolicy.probeStopQuick(hasLink = true))
    }

    @Test fun intentReplay_verbRunsOnlyOnAFreshDelivery() {
        assertTrue(IntentReplay.verbRuns(hasSavedState = false, intentFlags = 0))
        assertFalse("recreate (saved state)", IntentReplay.verbRuns(hasSavedState = true, intentFlags = 0))
        assertFalse("re-launched from recents", IntentReplay.verbRuns(hasSavedState = false, intentFlags = IntentReplay.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY))
        assertFalse(IntentReplay.verbRuns(hasSavedState = true, intentFlags = IntentReplay.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY))
        assertTrue("other flags do not count", IntentReplay.verbRuns(hasSavedState = false, intentFlags = 0x10000000))
        assertEquals(android.content.Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY, IntentReplay.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY)
    }
}
