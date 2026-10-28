package zeno.gmccpa.av

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Plan v5.9 MED M5 / OD-9C — the stock-style MainAudio take on a permanent media focus loss and the
 * give-back. The wire bodies and the take/untake state live in the receiver (events.rs tests); this
 * pins the app-side decision and that every send goes through the shared queue, in order.
 */
class MainAudioHandoverTest {

    private class Rec : MainAudioHandover.Port {
        val calls = mutableListOf<String>()
        var releaseResult = 0
        override fun take(): Boolean { calls += "take"; return true }
        override fun release(): Int { calls += "release"; return releaseResult }
    }

    /** A queue that runs nothing until drained — proves nothing is sent on the caller's thread. */
    private class Q { val q = ArrayDeque<Runnable>(); fun post(r: Runnable) { q.addLast(r) }; fun drain() { while (q.isNotEmpty()) q.removeFirst().run() } }

    private fun handover(port: Rec, q: Q, logs: MutableList<String> = mutableListOf()) =
        MainAudioHandover(port, { q.post(it) }, { logs += it })

    @Test fun shouldTake_truthTable() {
        assertTrue(MainAudioHandover.shouldTake(current = true, running = true, voiceFocusHeld = false))
        assertFalse("replacement player's GAIN handed us the loss", MainAudioHandover.shouldTake(false, true, false))
        assertFalse("stopped player", MainAudioHandover.shouldTake(true, false, false))
        assertFalse("stock !hasFocus(): a CarPlay voice slot holds focus", MainAudioHandover.shouldTake(true, true, true))
    }

    @Test fun permanentLoss_queuesOneTake_offTheCallerThread() {
        val p = Rec(); val q = Q()
        assertTrue(handover(p, q).onPermanentLoss(current = true, running = true, voiceFocusHeld = false))
        assertEquals("nothing sent before the queue runs", emptyList<String>(), p.calls)
        q.drain()
        assertEquals(listOf("take"), p.calls)
    }

    @Test fun permanentLoss_refused_sendsNothing() {
        val p = Rec(); val q = Q()
        val h = handover(p, q)
        assertFalse(h.onPermanentLoss(current = false, running = true, voiceFocusHeld = false))
        assertFalse(h.onPermanentLoss(current = true, running = true, voiceFocusHeld = true))
        q.drain()
        assertEquals(emptyList<String>(), p.calls)
    }

    /** CMBS.onPlay: the give-back is queued before the PLAY key on the same queue, so it goes first. */
    @Test fun giveBack_precedesAPlayKeyQueuedAfterIt() {
        val p = Rec(); val q = Q()
        val h = handover(p, q)
        h.onPermanentLoss(current = true, running = true, voiceFocusHeld = false)
        h.giveBack("media play")
        q.post(Runnable { p.calls += "PLAY key" })
        q.drain()
        assertEquals(listOf("take", "release", "PLAY key"), p.calls)
    }

    @Test fun giveBack_logsOnlyWhenSomethingWasOwed() {
        val p = Rec(); val q = Q(); val logs = mutableListOf<String>()
        val h = handover(p, q, logs)
        p.releaseResult = 0; h.giveBack("focus granted"); q.drain()
        assertEquals("nothing owed is silent", 0, logs.size)
        p.releaseResult = 1; h.giveBack("focus gain"); q.drain()
        assertTrue(logs.single().contains("Untake sent"))
    }

    @Test fun aThrowingPort_neverEscapesTheQueue() {
        val q = Q()
        val h = MainAudioHandover(object : MainAudioHandover.Port {
            override fun take(): Boolean = throw IllegalStateException("jni")
            override fun release(): Int = throw IllegalStateException("jni")
        }, { q.post(it) }, { })
        h.onPermanentLoss(current = true, running = true, voiceFocusHeld = false)
        h.giveBack("x")
        q.drain()   // must not throw
    }
}
