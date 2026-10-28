package zeno.gmccpa.av

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Plan v5.8 V4a — the route latch from edges. The cp8 replay is every `route: routeGuidanceState=` and
 * `modes: … turns=` line of /tmp/ccpa_sess/logcat_cp8.txt up to 20:10 (both are change-only log
 * lines, so the replay carries no unchanged records), times in ms from 17:00:31.160.
 */
class RouteLatchTest {
    private data class Ev(val t: Long, val k: Char, val v: Int)

    private val cp8 = listOf(
        Ev(0L, 'T', 0),
        Ev(13235L, 'T', 1),
        Ev(13399L, 'S', 0),
        Ev(13406L, 'S', 3),
        Ev(13406L, 'S', 0),
        Ev(13503L, 'S', 3),
        Ev(14170L, 'S', 6),
        Ev(18041L, 'S', 1),
        Ev(228690L, 'T', 1),
        Ev(229198L, 'T', 1),
        Ev(235015L, 'T', 1),
        Ev(235694L, 'T', 1),
        Ev(273193L, 'T', 0),
        Ev(322135L, 'T', 0),
        Ev(322864L, 'T', 1),
        Ev(325918L, 'S', 0),
        Ev(325979L, 'S', 1),
        Ev(440361L, 'T', 0),
        Ev(440389L, 'T', 1),
        Ev(442672L, 'S', 0),
        Ev(442757L, 'S', 1),
        Ev(480695L, 'T', 0),
        Ev(480722L, 'T', 1),
        Ev(482798L, 'S', 0),
        Ev(482863L, 'S', 1),
        Ev(521287L, 'T', 0),
        Ev(521362L, 'T', 1),
        Ev(522831L, 'S', 0),
        Ev(522891L, 'S', 1),
        Ev(565052L, 'T', 0),
        Ev(565634L, 'T', 1),
        Ev(567882L, 'S', 0),
        Ev(568028L, 'S', 1),
        Ev(608569L, 'T', 0),
        Ev(609021L, 'T', 1),
        Ev(610611L, 'S', 0),
        Ev(610655L, 'S', 1),
        Ev(652414L, 'T', 0),
        Ev(652968L, 'T', 1),
        Ev(655182L, 'S', 0),
        Ev(655286L, 'S', 1),
        Ev(3699583L, 'S', 0),
        Ev(3699584L, 'T', 0),
        Ev(3706736L, 'T', 1),
        Ev(3706891L, 'S', 3),
        Ev(3706891L, 'S', 0),
        Ev(3706971L, 'S', 3),
        Ev(3707568L, 'S', 6),
        Ev(3711440L, 'S', 1),
        Ev(4360873L, 'S', 0),
        Ev(4360877L, 'T', 0),
        Ev(4367427L, 'T', 1),
        Ev(4367609L, 'S', 3),
        Ev(4367610L, 'S', 0),
        Ev(4367951L, 'S', 3),
        Ev(4368200L, 'S', 6),
        Ev(4371436L, 'S', 1),
        Ev(4669992L, 'T', 0),
        Ev(4670000L, 'S', 0),
        Ev(4771093L, 'T', 0),
        Ev(4797796L, 'T', 1),
        Ev(4797915L, 'S', 0),
        Ev(4797916L, 'S', 3),
        Ev(4797916L, 'S', 0),
        Ev(4798044L, 'S', 3),
        Ev(4798531L, 'S', 6),
        Ev(4802674L, 'S', 1),
        Ev(5234561L, 'T', 1),
        Ev(5235596L, 'T', 1),
        Ev(5241083L, 'T', 1),
        Ev(5241762L, 'T', 1),
        Ev(5249933L, 'T', 1),
        Ev(5251002L, 'T', 1),
        Ev(5256371L, 'T', 1),
        Ev(5257021L, 'T', 1),
        Ev(5265264L, 'T', 1),
        Ev(5266176L, 'T', 1),
        Ev(5268984L, 'T', 1),
        Ev(5269167L, 'T', 1),
        Ev(5272173L, 'T', 1),
        Ev(5272823L, 'T', 1),
        Ev(5278593L, 'T', 1),
        Ev(5279275L, 'T', 1),
        Ev(5328668L, 'T', 1),
        Ev(5330547L, 'T', 1),
        Ev(5332380L, 'T', 1),
        Ev(5332573L, 'T', 1),
        Ev(5408336L, 'T', 1),
        Ev(5414791L, 'T', 1),
        Ev(5415463L, 'T', 1),
        Ev(5430677L, 'T', 1),
        Ev(5431635L, 'T', 1),
        Ev(5437105L, 'T', 1),
        Ev(5437767L, 'T', 1),
        Ev(5451132L, 'T', 1),
        Ev(5461379L, 'T', 1),
        Ev(5466508L, 'T', 1),
        Ev(5467467L, 'T', 1),
        Ev(5472963L, 'T', 1),
        Ev(5473972L, 'T', 1),
        Ev(5478100L, 'T', 1),
        Ev(5478807L, 'T', 1),
        Ev(5481098L, 'T', 1),
        Ev(5499608L, 'T', 1),
        Ev(5505376L, 'T', 1),
        Ev(5506281L, 'T', 1),
        Ev(5508633L, 'T', 1),
        Ev(5532964L, 'T', 1),
        Ev(5787289L, 'T', 1),
        Ev(5827676L, 'T', 1),
        Ev(5936208L, 'T', 0),
        Ev(5936215L, 'S', 0),
        Ev(5956638L, 'T', 1),
        Ev(5956798L, 'S', 3),
        Ev(5956800L, 'S', 0),
        Ev(5957147L, 'S', 3),
        Ev(5957755L, 'S', 1),
        Ev(8301855L, 'T', 0),
        Ev(8301864L, 'S', 0),
        Ev(8308632L, 'T', 1),
        Ev(8308728L, 'S', 3),
        Ev(8308728L, 'S', 0),
        Ev(8308972L, 'S', 3),
        Ev(8309323L, 'S', 6),
        Ev(8313550L, 'S', 1),
    )

    private fun at(hms: String): Long {
        fun ms(s: String): Long { val p = s.split(":"); return Math.round((p[0].toInt() * 3600 + p[1].toInt() * 60 + p[2].toDouble()) * 1000) }
        return ms(hms) - ms("17:00:31.160")
    }

    /** Replays [evs] with a 100 ms sweeper tick; returns the verdict edges (time, active, cause). */
    private fun replay(evs: List<Ev>, until: Long = evs.last().t + 10_000): List<Triple<Long, Boolean, String>> {
        val l = RouteLatch()
        val edges = ArrayList<Triple<Long, Boolean, String>>()
        var was = false
        var i = 0
        var t = 0L
        while (t <= until) {
            while (i < evs.size && evs[i].t <= t) {
                val e = evs[i]
                if (e.k == 'T') l.onTurns(e.v != 0, e.t) else l.onState(e.v, e.t)
                i++
            }
            val v = l.evaluate(t)
            if (v.active != was) { was = v.active; edges.add(Triple(t, v.active, v.cause)) }
            t += 100
        }
        return edges
    }

    @Test fun turnsZeroAtSessionStartIsInactive() {
        val e = replay(cp8.filter { it.t < at("17:00:44.000") })
        assertTrue(e.isEmpty())
    }

    @Test fun cp8_pairedEndsEndWithinOneTick_andEveryStartIsActiveWithinOneTick() {
        val e = replay(cp8)
        val ends = e.filter { !it.second }
        for (pair in listOf("18:02:10.744", "18:13:12.037", "18:18:21.160", "18:39:27.375", "19:18:53.024")) {
            val t = at(pair)
            assertTrue("ended within one tick of $pair: $ends", ends.any { it.first in t..t + 100 && it.third.contains("and turns=0") })
        }
        val starts = e.filter { it.second }
        for (s in listOf("17:00:44.395", "18:02:17.896", "18:13:18.587", "18:20:28.956", "18:39:47.798", "19:18:59.792")) {
            val t = at(s)
            assertTrue("ACTIVE within one tick of $s: $starts", starts.any { it.first in t..t + 100 })
        }
        // The change-only log shows turns=0 alone from 17:05:04.353 until turns=1 at 17:05:54.024 with no
        // state change between: the replay ends the route at +5 s (one end signal lost) and restarts it.
        assertTrue(ends.any { it.first in at("17:05:09.353")..at("17:05:09.353") + 100 && it.third.contains("turns=0 for 5s") })
        assertEquals("5 paired ends + the 17:05:04 lone turns=0", 6, ends.size)
        assertEquals(7, starts.size)
    }

    @Test fun cp8_turnsBlipsAndTransientStateZeroRunsNeverEndTheRoute() {
        val e = replay(cp8.filter { it.t in at("17:05:50.000")..at("17:12:00.000") }.let { listOf(Ev(at("17:05:50.000"), 'T', 1)) + it })
        assertEquals("one ACTIVE, no end, across 6 turns blips (25-582 ms) and 7 state-0 runs (≤ 347 ms): $e", 1, e.size)
        assertTrue(e.single().second)
    }

    @Test fun aGapOf28sStaysActive() {
        val e = replay(listOf(Ev(0, 'T', 1), Ev(100, 'S', 1), Ev(28_100, 'S', 1)), until = 60_000)
        assertEquals(1, e.size)
        assertTrue(e.single().second)
    }

    @Test fun oneEndSignalLostEndsAt5s_andAContradictionCancelsIt() {
        val lost = replay(listOf(Ev(0, 'T', 1), Ev(1_000, 'S', 1), Ev(10_000, 'S', 0)), until = 20_000)
        assertEquals(15_000L, lost.last().first)
        assertFalse(lost.last().second)
        // state 0 alone, then a LATER state > 0 record: contradicted, still active.
        val back = replay(listOf(Ev(0, 'T', 1), Ev(1_000, 'S', 1), Ev(10_000, 'S', 0), Ev(10_300, 'S', 1)), until = 30_000)
        assertEquals(1, back.size)
        // turns 0 alone, with route records still arriving at state > 0 afterwards: still active.
        val rec = replay(listOf(Ev(0, 'T', 1), Ev(500, 'S', 1), Ev(10_000, 'T', 0), Ev(11_000, 'S', 1), Ev(12_000, 'S', 1)), until = 30_000)
        assertEquals(1, rec.size)
    }

    /** V4a drives the NAV pool: kept while a route is ACTIVE, freed at ended; without a route a NAV sink is freed at its END. */
    @Test fun navPoolRetentionFollowsTheLatch() {
        val r = VoiceRig()
        r.core.navRetain = false
        fun prompt(scid: Long) {
            r.begin(scid, 101, VoiceLatency.AT_DEFAULT, 48000, 2)
            val f = Feeder(r, scid, 101, 4, 48000, 2, 10)
            r.run(500) { f.tick(it) }
            f.on = false; r.end(scid, f.k.toLong()); r.run(300)
        }
        prompt(0x1)
        assertEquals("no route: freed at END", 1, r.factory.released.size)
        assertEquals(0, r.core.pooledCount())
        r.core.navRetain = true
        prompt(0x2)
        assertEquals("route ACTIVE: pooled", 1, r.core.pooledCount())
        prompt(0x3)
        assertEquals("reused from the pool, not rebuilt", 2, r.factory.built.size)
        r.core.navRetain = false
        assertEquals(1, r.core.freeNavPool())
        assertEquals(0, r.core.pooledCount())
        assertEquals(r.factory.built.size, r.factory.released.size)
    }
}
