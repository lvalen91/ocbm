package zeno.gmccpa.av

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import zeno.gmccpa.av.VoiceJitterBuffer.Fate
import zeno.gmccpa.av.VoiceJitterBuffer.SilenceKind

/**
 * Plan v5.8 V1b — the per-stream jitter buffer. Pure; time is simulated.
 *
 * The simulation: 16 kHz mono, 480-frame AUs (30 ms, the Siri/call shape), target 80 ms → W 200 ms.
 * AU k is sent at k × 30 ms (ts = k × 480) and arrives at its send time, except inside a stall,
 * where it arrives at the stall's end (a WiFi burst). The device pulls 128 frames every 8 ms, from
 * the first start on — exactly as a track with a full device buffer asks for room.
 */
class VoiceJitterBufferTest {

    private val played: (VoiceJitterBuffer.Node) -> Fate = { Fate.PLAYED }

    private companion object {
        /** A node already decoded (480 frames of PCM), as the worker's decode-ahead leaves it. */
        fun nd(ts: Long, seq: Int, rx: Long, read: Long, au: ByteArray) =
            VoiceJitterBuffer.Node(ts, seq, rx, au).also { it.pcm = ByteArray(480 * 4) }
    }

    private class Sim(
        val stallAtMs: Long = -1, val stallMs: Long = 0, val auMs: Double = 30.0, val target: Int = 80,
    ) {
        val jb = VoiceJitterBuffer(16_000, 1, target)
        var nextAu = 0
        val pulls = ArrayList<Pair<Long, List<VoiceJitterBuffer.Segment>>>()
        var shortPulls = 0
        var maxBufferedMs = 0L
        var transitionBuffered = -1L

        fun arrivalMs(k: Int): Long {
            val send = Math.round(k * auMs)
            return if (stallAtMs >= 0 && send >= stallAtMs && send < stallAtMs + stallMs) stallAtMs + stallMs else send
        }

        fun run(untilMs: Long, onPull: (Long, List<VoiceJitterBuffer.Segment>) -> Unit = { _, _ -> }) {
            var t = 0L
            while (t <= untilMs) {
                while (arrivalMs(nextAu) <= t) {
                    val rx = arrivalMs(nextAu) * 1000
                    jb.put(nd(nextAu * 480L, nextAu and 0xFFFF, rx, rx, ByteArray(4)), rx)
                    nextAu++
                }
                if (t % 8 == 0L) {
                    val wasBuffering = jb.state == VoiceJitterBuffer.State.BUFFERING
                    val before = jb.bufferedMs()
                    val segs = jb.pull(t * 1000, 128) { Fate.PLAYED }
                    if (wasBuffering && jb.state == VoiceJitterBuffer.State.PLAYING && transitionBuffered < 0 && t > (stallAtMs + stallMs)) {
                        transitionBuffered = before
                    }
                    if (jb.everPlayed && segs.sumOf { it.frames } != 128) shortPulls++
                    if (segs.isNotEmpty()) pulls.add(t to segs)
                    onPull(t, segs)
                    if (jb.state == VoiceJitterBuffer.State.PLAYING) maxBufferedMs = maxOf(maxBufferedMs, jb.bufferedMs())
                }
                t++
            }
        }
    }

    @Test fun targetClampAndDefault() {
        assertEquals(80, VoiceLatency.targetMs(-1))
        assertEquals(32, VoiceLatency.targetMs(0))
        assertEquals(32, VoiceLatency.targetMs(10))
        assertEquals(80, VoiceLatency.targetMs(80))
        assertEquals(1000, VoiceLatency.targetMs(1000))
        assertEquals(200, VoiceLatency.watermarkMs(80))
        assertEquals(200, VoiceLatency.watermarkMs(32))
        assertEquals(1020, VoiceLatency.watermarkMs(1000))
        assertEquals(50, VoiceLatency.poolNodes(80, 30))
        assertEquals(50, VoiceLatency.poolNodes(80, 10))
        assertEquals(68, VoiceLatency.poolNodes(1000, 30))
    }

    @Test fun startIsFirstRxPlusTarget() {
        val jb = VoiceJitterBuffer(16_000, 1, 80)
        jb.put(nd(0, 0, 1_000_000, 1_050_000, ByteArray(1)), 1_050_000)
        assertEquals("start = first RX + target, not read time", 1_080_000, jb.startAtUs())
        assertTrue(jb.pull(1_079_999, 128, played).isEmpty())
        assertEquals(VoiceJitterBuffer.State.BUFFERING, jb.state)
        val s = jb.pull(1_080_000, 128, played)
        assertEquals(VoiceJitterBuffer.State.PLAYING, jb.state)
        assertEquals(128, s.sumOf { it.frames })
        assertTrue(s.all { it.silence == null })
    }

    @Test fun lateIsSkippedAndGapIsConcealed() {
        val jb = VoiceJitterBuffer(16_000, 1, 80)
        for (k in 0..2) jb.put(nd(k * 480L, k, 0, 0, ByteArray(1)), 0)
        jb.pull(80_000, 480, played)                    // playhead at 480
        assertEquals("entirely before the playhead", VoiceJitterBuffer.Put.LATE,
            jb.put(nd(0, 9, 0, 0, ByteArray(1)), 90_000))
        // AU 3 lost; AU 4 arrives: pulling across the hole conceals exactly its 480 frames.
        jb.put(nd(4 * 480L, 4, 0, 0, ByteArray(1)), 100_000)
        val segs = jb.pull(110_000, 480 * 4, played)
        val concealed = segs.filter { it.silence == SilenceKind.CONCEALED }.sumOf { it.frames }
        assertEquals(480, concealed)
        assertEquals(480L, jb.concealedFrames)
        assertEquals(1L, jb.late)
        // AU 4 was consumed: a re-delivery is before the playhead → late, not a duplicate.
        assertEquals(VoiceJitterBuffer.Put.LATE, jb.put(nd(4 * 480L, 4, 0, 0, ByteArray(1)), 0))
    }

    @Test fun duplicateInTheBufferIsCountedLate() {
        val jb = VoiceJitterBuffer(16_000, 1, 80)
        jb.put(nd(0, 0, 0, 0, ByteArray(1)), 0)
        jb.put(nd(480, 1, 0, 0, ByteArray(1)), 0)
        assertEquals(VoiceJitterBuffer.Put.DUPLICATE, jb.put(nd(480, 1, 0, 0, ByteArray(1)), 0))
        assertEquals(1L, jb.duplicates)
        assertEquals(1L, jb.late)
    }

    /** The WiFi burst numbers of plan v5.8 V1b (rev d, re-derived for trim-to-W at target 80). */
    @Test fun wifiBurst100msStallMakesNoDiscard() {
        val s = Sim(stallAtMs = 1_000, stallMs = 100); s.run(3_000)
        assertTrue("≈180 ms buffered at the transition, got ${s.transitionBuffered}", s.transitionBuffered in 150..200)
        assertEquals(0L, s.jb.excess)
        assertEquals(0L, s.jb.transitionTrims)
        assertEquals(0L, s.jb.wTriggers)
    }

    @Test fun wifiBurst200msStallMakesExactlyOneDiscardOfAbout80ms() {
        val s = Sim(stallAtMs = 1_000, stallMs = 200); s.run(4_000)
        assertTrue("≈280 ms at the transition, got ${s.transitionBuffered}", s.transitionBuffered in 250..300)
        assertEquals("exactly one trim", 1L, s.jb.transitionTrims)
        val cutMs = s.jb.excessFramesLastTrim * 1000 / 16_000
        assertTrue("≈80 ms discarded (30 ms AUs), got $cutMs", cutMs in 60..120)
        assertEquals("never repeated", 0L, s.jb.wTriggers)
        assertEquals(0L, s.jb.poolTriggers)
    }

    @Test fun wifiBurst300msStallMakesExactlyOneDiscardOfAbout180ms() {
        val s = Sim(stallAtMs = 1_000, stallMs = 300); s.run(4_000)
        assertTrue("≈380 ms at the transition, got ${s.transitionBuffered}", s.transitionBuffered in 350..410)
        assertEquals(1L, s.jb.transitionTrims)
        val cutMs = s.jb.excessFramesLastTrim * 1000 / 16_000
        assertTrue("≈180 ms discarded, got $cutMs", cutMs in 150..240)
        assertEquals("never repeated", 0L, s.jb.wTriggers)
    }

    /**
     * Rev e, R3 MAJOR: re-anchor on the first SURVIVING node. After a 200 ms stall the playout delay
     * is ≤ W at once, and NO silence is written for the discarded span. A stock-literal port (anchor
     * on the pre-discard head) would emit the discarded span as silence and keep the delay.
     */
    @Test fun reanchorOnTheSurvivorWritesNoSilenceForTheCut() {
        var transitionSegs: List<VoiceJitterBuffer.Segment>? = null
        var delayAfter = -1L
        val s = Sim(stallAtMs = 1_000, stallMs = 200)
        var prevState = VoiceJitterBuffer.State.PLAYING
        s.run(2_000) { t, segs ->
            if (t > 1_200 && prevState == VoiceJitterBuffer.State.BUFFERING && s.jb.state == VoiceJitterBuffer.State.PLAYING && transitionSegs == null) {
                transitionSegs = segs
                delayAfter = s.jb.bufferedMs()
            }
            prevState = s.jb.state
        }
        val segs = transitionSegs!!
        assertTrue("a trim happened", s.jb.transitionTrims == 1L)
        assertTrue("the transition pull is all real audio: $segs", segs.all { it.silence == null && it.node != null })
        assertTrue("delay ≤ W right after the transition, got $delayAfter", delayAfter <= 200)
        // The first output after the transition is the first SURVIVOR, not the pre-discard head.
        val firstTs = segs.first().node!!.ts
        assertTrue("first output after the trim must be past the cut AUs", firstTs > 1_020 / 30 * 480)
    }

    /** Rev e, R3: a 1 s stall mid-Siri — the track never starves; the gap is rebuffer silence, not concealment. */
    @Test fun oneSecondStallRebuffersWithSilenceAndNeverStarves() {
        val s = Sim(stallAtMs = 2_000, stallMs = 1_000); s.run(5_000)
        assertEquals("every pull after the first start returned the full period (0 underruns)", 0, s.shortPulls)
        val rebufMs = s.jb.rebufferSilenceFrames * 1000 / 16_000
        // Stock: silence from the dry point until the burst + target. ≈ 1 s − the drained buffer + target.
        assertTrue("rebuffer_silence ≈ 1 s − drained + target, got $rebufMs", rebufMs in 900..1_100)
        assertEquals("no seq gap → nothing concealed", 0L, s.jb.concealedFrames)
        assertEquals(1L, s.jb.rebuffers)
    }

    @Test fun continuousWTrimsWhilePlaying() {
        val jb = VoiceJitterBuffer(16_000, 1, 80)
        for (k in 0..2) jb.put(nd(k * 480L, k, 0, 0, ByteArray(1)), 0)
        jb.pull(80_000, 128, played)
        assertEquals(VoiceJitterBuffer.State.PLAYING, jb.state)
        // A burst of 10 AUs lands at once: buffered 13 × 30 − 8 ms > W → trimmed DOWN TO W on the put.
        for (k in 3..12) jb.put(nd(k * 480L, k, 0, 0, ByteArray(1)), 90_000)
        assertTrue("trimmed to ≤ W, got ${jb.bufferedMs()}", jb.bufferedMs() <= 200)
        assertTrue("and not to the target", jb.bufferedMs() > 80)
        assertTrue(jb.wTriggers >= 1)
        assertTrue(jb.excess >= 6)
    }

    /** Stock pool exhaustion: 50 nodes, trigger at 47, trim DOWN TO W — while BUFFERING (a cold build). */
    @Test fun poolExhaustionTrimsToWWhileBuffering() {
        val jb = VoiceJitterBuffer(16_000, 1, 80)
        assertEquals(50, jb.poolNodes)
        for (k in 0 until 47) jb.put(nd(k * 480L, k, 0, 0, ByteArray(1)), 0)
        assertEquals(0L, jb.poolTriggers)
        assertEquals(47, jb.size())
        jb.put(nd(47 * 480L, 47, 0, 0, ByteArray(1)), 0)
        assertEquals("the 48th put finds 47 in use: allocated − 3", 1L, jb.poolTriggers)
        // Stock trims BEFORE the insert (AirPlayUtils.c:755-768), so W + the new AU remains.
        assertTrue("down to W (+ the AU inserted after): ${jb.bufferedMs()}", jb.bufferedMs() <= 200 + 30)
        assertTrue("not to the target", jb.bufferedMs() > 80)
        assertEquals(VoiceJitterBuffer.State.BUFFERING, jb.state)
        // The transition then anchors on the first survivor: no silence for the cut.
        val segs = jb.pull(5_000_000, 128, played)
        assertTrue(segs.all { it.silence == null })
    }

    /** ±100 ppm drift for 10 min: the delay never grows past W and no trim fires on the drift alone. */
    @Test fun driftOf100ppmFor10Minutes() {
        for (ppm in listOf(+100.0, -100.0)) {
            val s = Sim(auMs = 30.0 * (1 - ppm / 1e6)); s.run(600_000)
            assertTrue("10 min of AUs were fed (${s.nextAu})", s.nextAu in 19_990..20_010)
            assertTrue("and played (${s.jb.played})", s.jb.played >= 19_980)
            assertTrue("$ppm ppm: max buffered ${s.maxBufferedMs} ms ≤ W", s.maxBufferedMs <= 200)
            assertEquals("$ppm ppm: no W trigger", 0L, s.jb.wTriggers)
            assertTrue("$ppm ppm: at most one re-buffer (${s.jb.rebuffers})", s.jb.rebuffers <= 1)
            assertEquals(0, s.shortPulls)
        }
    }

    /** Every node lands in exactly one bucket. */
    @Test fun accountingIdentityCoversEveryNode() {
        val jb = VoiceJitterBuffer(16_000, 1, 80)
        var puts = 0L
        fun put(k: Int, at: Long) { puts++; jb.put(nd(k * 480L, k, at, at, ByteArray(1)), at) }
        for (k in 0..2) put(k, 0)
        jb.pull(80_000, 480, played)
        put(0, 80_000)                                   // late
        put(4, 80_000); put(4, 80_000)                   // a duplicate
        var i = 0
        jb.pull(100_000, 480) { i++; if (i % 2 == 0) Fate.EPOCH_IDLE else Fate.SUPPRESSED }
        for (k in 5..20) put(k, 120_000)                 // a burst → continuous W excess
        jb.pull(130_000, 480) { Fate.REFUSED_LOST }
        jb.dropAll(VoiceJitterBuffer.Drop.SUPERSEDED)
        assertEquals(puts, jb.bucketSum())
        assertTrue(jb.excess > 0 && jb.late >= 2 && jb.superseded > 0 && jb.played > 0)
    }

    /** Review round 1: a node due to play with no PCM is decode_miss, not played (it was written as silence). */
    @Test fun undecodedNodeAtPlayoutIsADecodeMissNotPlayed() {
        val jb = VoiceJitterBuffer(16_000, 1, 80)
        jb.put(VoiceJitterBuffer.Node(0, 0, 0, ByteArray(1)), 0)
        jb.put(nd(480, 1, 0, 0, ByteArray(1)), 0)
        jb.pull(80_000, 960, played)
        assertEquals(1L, jb.decodeMiss)
        assertEquals(1L, jb.played)
        assertEquals(2L, jb.bucketSum())
    }

    /** Review round 1: a stream that waited behind an END starts at the target, not at up to W. */
    @Test fun catchUpToTargetDropsWhatWaitedAsLate() {
        val jb = VoiceJitterBuffer(16_000, 1, 80)
        for (k in 0..9) jb.put(nd(k * 480L, k, 0, 0, ByteArray(1)), 0)       // 300 ms queued while waiting
        val cut = jb.catchUpToTarget()
        assertTrue("≤ target left: ${jb.bufferedMs()}", jb.bufferedMs() <= 80)
        assertEquals(8L, jb.late)
        assertEquals(8 * 480L, cut)
    }

    @Test fun decodeAheadIsLimitedToTheTarget() {
        val jb = VoiceJitterBuffer(48_000, 2, 80)
        for (k in 0..19) jb.put(nd(k * 480L, k, 0, 0, ByteArray(1)), 0)
        // BUFFERING: ≤ 80 ms from the first node = AUs 0..8 (10 ms each).
        assertEquals("the horizon widens to a device buffer + one AU (128 ms + 10 ms)", 14, jb.decodeCandidates(48L * 138).size)
        val c = jb.decodeCandidates()
        assertEquals(9, c.size)
        c.forEach { it.submitted = true }
        assertTrue(jb.decodeCandidates().isEmpty())
        assertNull(jb.tsUnitMismatch)
        jb.decoded(c[0], ByteArray(480 * 4), loud = false)
        assertNull("480 frames per AU matches the RTP step", jb.tsUnitMismatch)
        jb.decoded(c[1], ByteArray(960 * 4), loud = false)
        assertFalse(jb.tsUnitMismatch == null)
    }

    /** fix2: while the device start stall holds the trims, the buffer is still bounded by W + the start cap. */
    @Test fun theStartHoldIsBoundedByTheCap() {
        val jb = VoiceJitterBuffer(48_000, 2, 80)
        jb.put(nd(0, 0, 0, 0, ByteArray(1)), 0)
        jb.pull(80_000, 480, played)                              // PLAYING
        jb.holdTrims(true)
        for (k in 1..150) jb.put(nd(k * 480L, k, 0, 0, ByteArray(1)), 0)   // 1.5 s, no device pulls
        assertTrue("≤ W + cap: ${jb.bufferedMs()}", jb.bufferedMs() <= 200 + VoiceLatency.START_STALL_CAP_MS + 10)
        assertTrue(jb.bufferedMs() > 200)
        assertEquals("nothing on the pool count during the hold", 0L, jb.poolTriggers)
    }

    /** fix2: the allowance is paid back from SILENT AUs only — a loud AU at the playhead stops the catch-up. */
    @Test fun theCatchUpNeverDropsALoudAu() {
        val jb = VoiceJitterBuffer(48_000, 2, 80)
        jb.holdTrims(true)                                        // play() issued; the bus is waking
        for (k in 0..39) jb.put(nd(k * 480L, k, 0, 0, ByteArray(1)).also { it.loud = k < 20 }, 0)
        val allowance = jb.deviceStarted(0)                       // 400 ms buffered: 200 ms over W carried
        jb.pull(80_000, 480, played)
        assertTrue(allowance > 0)
        jb.pull(90_000, 480, played)                              // the playhead sits on loud AUs: no catch-up
        assertEquals(0L, jb.excessSilentCatchUp)
        repeat(19) { jb.pull(100_000L + it * 10_000, 480, played) }  // play through the loud part
        jb.pull(300_000, 480, played)
        assertTrue("silence dropped once the playhead reached it: ${jb.summary()}", jb.excessSilentCatchUp > 0)
        assertEquals(0L, jb.excessLoud)
    }
}
