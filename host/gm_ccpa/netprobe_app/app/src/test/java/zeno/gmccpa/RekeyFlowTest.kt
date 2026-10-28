package zeno.gmccpa

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import zeno.gmccpa.AccessoryKey.Rekey

/** Plan v5.8 V5 / OD-8B: the SessionCore decision seam, driven without Android (review fix1-6). */
class RekeyFlowTest {
    private class FakeStore(var st: Rekey) : RekeyFlow.Store {
        val marks = mutableListOf<Boolean>()
        override fun state() = st
        override fun markForgotten(force: Boolean): Boolean { marks += force; val was = st; if (was == Rekey.PENDING || force) st = Rekey.FORGOTTEN; return was == Rekey.PENDING }
        override fun clear(): Boolean { val had = st != Rekey.NONE; st = Rekey.NONE; return had }
    }

    private class Rig(state: Rekey, var forgetResult: Boolean = true, var subscribed: Boolean = true) {
        val store = FakeStore(state)
        val queued = mutableListOf<() -> Unit>()
        var forgets = 0; var reports = 0
        val lines = mutableListOf<String>()
        /** what the forget does to the store while it runs (models a racing pair-verify clear) */
        var duringForget: () -> Unit = {}
        val flow = RekeyFlow(
            store = store,
            forget = { forgets++; duringForget(); forgetResult },
            linkSubscribed = { subscribed },
            reportLinked = { reports++ },
            emit = { lines += it }
        )
        fun linkUp() = flow.onLinkUp { queued += it }
        fun drain() { val q = queued.toList(); queued.clear(); q.forEach { it() } }
    }

    @Test fun pending_firstLink_queuesExactlyOneRun_thenForgetsAndReports() {
        val r = Rig(Rekey.PENDING)
        assertTrue(r.linkUp())
        assertFalse("second link-up in the same process: latched", r.linkUp())
        assertEquals(1, r.queued.size)
        r.drain()
        assertEquals(1, r.forgets); assertEquals(1, r.reports)
        assertEquals(1, r.lines.count { it.contains("OD-8B") })
        assertFalse("still latched after the run", r.linkUp())
    }

    @Test fun none_or_forgotten_neverQueues() {
        assertFalse(Rig(Rekey.NONE).linkUp())
        assertFalse(Rig(Rekey.FORGOTTEN).linkUp())
    }

    @Test fun clearedBetweenQueueAndRun_skipsTheForget_noReport() {
        val r = Rig(Rekey.PENDING)
        assertTrue(r.linkUp())
        r.store.st = Rekey.NONE            // a pair-verify landed on the cmd queue's way
        r.drain()
        assertEquals(0, r.forgets); assertEquals(0, r.reports)
        assertNull(r.flow.run())
    }

    @Test fun partialForget_stillReportsTheLinkThatIsUp() {
        val r = Rig(Rekey.PENDING, forgetResult = false)
        r.linkUp(); r.drain()
        assertEquals(1, r.forgets)
        assertEquals("review fix1-3: re-report whenever the link is still subscribed", 1, r.reports)
    }

    @Test fun linkGoneAfterForget_noReport() {
        val r = Rig(Rekey.PENDING, subscribed = false)
        r.linkUp(); r.drain()
        assertEquals(1, r.forgets); assertEquals(0, r.reports)
    }

    @Test fun onForgetDone_cleanFromPending_forcesForgotten_evenAfterRacingClear() {
        val r = Rig(Rekey.PENDING)
        val was = r.store.state()
        r.store.st = Rekey.NONE            // pair-verify cleared it while the forget was in flight
        assertTrue(r.flow.onForgetDone(was, boxOk = true, appOk = true))
        assertEquals(Rekey.FORGOTTEN, r.store.state())
        assertEquals(listOf(true), r.store.marks)
    }

    @Test fun onForgetDone_partial_leavesPending() {
        for ((box, app) in listOf(false to true, true to false, false to false)) {
            val r = Rig(Rekey.PENDING)
            assertFalse(r.flow.onForgetDone(Rekey.PENDING, box, app))
            assertEquals(Rekey.PENDING, r.store.state())
            assertTrue(r.store.marks.isEmpty())
        }
    }

    @Test fun onForgetDone_manualForgetInStateNone_staysNone() {
        val r = Rig(Rekey.NONE)
        assertFalse(r.flow.onForgetDone(Rekey.NONE, boxOk = true, appOk = true))
        assertEquals(Rekey.NONE, r.store.state())
        val f = Rig(Rekey.FORGOTTEN)
        assertFalse(f.flow.onForgetDone(Rekey.FORGOTTEN, boxOk = true, appOk = true))
        assertEquals(Rekey.FORGOTTEN, f.store.state())
    }

    @Test fun pairVerified_endsMigration_fromPendingOrForgotten_notFromNone() {
        val p = Rig(Rekey.PENDING); assertTrue(p.flow.onPairVerified()); assertEquals(Rekey.NONE, p.store.state())
        val f = Rig(Rekey.FORGOTTEN); assertTrue(f.flow.onPairVerified()); assertEquals(Rekey.NONE, f.store.state())
        val n = Rig(Rekey.NONE); assertFalse(n.flow.onPairVerified()); assertEquals(Rekey.NONE, n.store.state())
    }

    /** End to end on the fake: upgrade -> link -> auto forget -> hint stays -> re-pair -> cleared. */
    @Test fun wholeMigration_onTheFake() {
        val r = Rig(Rekey.PENDING)
        r.duringForget = { r.flow.onForgetDone(Rekey.PENDING, boxOk = true, appOk = true) }
        assertTrue(r.linkUp()); r.drain()
        assertEquals(Rekey.FORGOTTEN, r.store.state())
        assertFalse("a later link does not run again", r.linkUp())
        assertTrue(r.flow.onPairVerified())
        assertEquals(Rekey.NONE, r.store.state())
        assertEquals(1, r.forgets); assertEquals(1, r.reports)
    }
}
