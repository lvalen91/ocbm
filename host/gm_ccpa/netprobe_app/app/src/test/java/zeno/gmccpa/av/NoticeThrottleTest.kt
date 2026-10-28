package zeno.gmccpa.av

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** plan v5.9 D6 rev c C3: the notification's throttle, trailing post and orphan guard (fake notifier/clock). */
class NoticeThrottleTest {
    private var now = 0L
    private val posts = ArrayList<Notice>()
    private val timers = ArrayList<Pair<Long, () -> Unit>>()
    private val gate = FgsGate()
    private val poster = NoticePoster(gate, { now }, { d, r -> timers.add((now + d) to r) }, { posts.add(it) }, n("init"))

    private fun n(t: String) = Notice("GM CCPA", t)
    private fun advance(ms: Long) {
        now += ms
        while (true) {
            val due = timers.filter { it.first <= now }.minByOrNull { it.first } ?: break
            timers.remove(due); due.second()
        }
    }
    /** The service: start (LINK) then onStartCommand's startForeground with the pending notice. */
    private fun startService(): Notice {
        gate.start(FgsTokens.LINK_TOKEN) {}
        val shown = poster.pending
        gate.onStarted(onStays = { poster.onStartedStays(shown) }) {}
        return shown
    }

    @Test fun updateBeforeTheServiceStartsPostsNothingButIsBuiltFrom() {
        poster.update(n("Waiting for iPhone"))
        assertTrue(posts.isEmpty())
        assertEquals(n("Waiting for iPhone"), startService())
        assertTrue("startForeground showed it; no extra post", posts.isEmpty())
    }

    @Test fun burstThenQuietPostsTheLastValueExactlyOnce() {
        startService()
        now = 10_000
        poster.update(n("a"))                         // posted now
        advance(100); poster.update(n("b"))           // inside 1 s: one trailing scheduled
        advance(100); poster.update(n("c"))           // still inside: no second timer
        assertEquals(1, timers.size)
        advance(2_000)                                // quiet
        assertEquals(listOf(n("a"), n("c")), posts)
    }

    @Test fun waitingPairingLiveInsideOneSecondEndsOnLive() {
        startService(); now = 5_000
        poster.update(n("Waiting")); advance(300); poster.update(n("Pairing")); advance(300); poster.update(Notice("CarPlay active", "x"))
        advance(1_000)
        assertEquals(Notice("CarPlay active", "x"), posts.last())
    }

    @Test fun burstThenStopPostsNothingAfterTheStop() {
        startService(); now = 1_000
        poster.update(n("a")); advance(100); poster.update(n("b"))
        gate.stop(FgsTokens.LINK_TOKEN) {}            // STOPPED: flag cleared before stopService
        val before = posts.size
        advance(5_000)
        poster.update(n("late"))
        assertEquals("nothing posted after the stop", before, posts.size)
    }

    @Test fun updateBetweenTheBuildAndOnStartedIsRepostedUnderTheLock() {
        gate.start(FgsTokens.LINK_TOKEN) {}
        val shown = poster.pending                    // startForeground builds this
        poster.update(n("arrived in the gap"))        // flag not yet set: kept as pending, not posted
        assertTrue(posts.isEmpty())
        gate.onStarted(onStays = { poster.onStartedStays(shown) }) {}
        assertEquals(listOf(n("arrived in the gap")), posts)
    }

    @Test fun sameValueIsNotReposted() {
        startService(); now = 3_000
        poster.update(n("a")); advance(5_000); poster.update(n("a"))
        assertEquals(1, posts.size)
    }
}
