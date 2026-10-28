package zeno.gmccpa.av

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import zeno.gmccpa.av.VoiceLatency.Purpose

// =====================================================================================================
// The host-JVM rig for the v5.8 voice engine: fake clock, codec, track, sink factory, focus and duck
// ports, driving the REAL VoiceStreams / PurposeEngine / VoiceJitterBuffer / FocusSlot code.
// NavEpochTest, FocusSlotTest and DuckRampTest reuse it, so this file is compiled with them
// (tools/jvm_tests.txt lists VoiceStreamsTest; the gate compiles every listed test source together).
// =====================================================================================================

internal class FakeClock(var us: Long = 10_000_000L) : VoiceClock { override fun nowUs() = us }

internal class CapLog : VoiceLog {
    val lines = ArrayList<String>()
    override fun i(msg: String) { lines.add("I $msg") }
    override fun w(msg: String) { lines.add("W $msg") }
    override fun e(msg: String) { lines.add("E $msg") }
    fun has(s: String) = lines.any { it.contains(s) }
    fun count(s: String) = lines.count { it.contains(s) }
}

/** AU byte 0 = 1 → a loud decoded frame (peak 2000), else digital silence. 480 frames per AU. */
internal class FakeCodec(val ch: Int) : CodecPort {
    val pending = ArrayDeque<Pair<Long, ByteArray>>()
    var flushes = 0
    var queued = 0
    /** Outputs held back (a decoder one AU behind its input). */
    var lag = 0
    /** Rewrite the output pts (a decoder that adjusts timestamps). */
    var ptsShift = 0L
    override fun queue(au: ByteArray, key: Long): Boolean { pending.addLast(key to au); queued++; return true }
    override fun drain(waitUs: Long): List<Decoded> {
        val out = ArrayList<Decoded>()
        while (pending.size > lag) {
            val (k0, au) = pending.removeFirst()
            val k = k0 + ptsShift
            val pcm = ByteArray(480 * 2 * ch)
            if (au.isNotEmpty() && au[0].toInt() == 1) {
                var i = 0
                while (i < pcm.size) { pcm[i] = 0xD0.toByte(); pcm[i + 1] = 0x07; i += 2 }   // 2000
            }
            out.add(Decoded(k, pcm))
        }
        return out
    }
    override fun flush() { pending.clear(); flushes++ }
}

/** A device: consumes at [rate] while playing, once the start threshold (the whole buffer) was filled. */
internal class FakeTrack(val clock: FakeClock, val rate: Int, val ch: Int, override val bufferFrames: Int) : TrackPort {
    var playing = false
    var started = false
    var written = 0L
    var consumed = 0L
    private var lastUs = clock.us
    var underruns = 0
    private var starving = false
    var plays = 0; var pauses = 0; var flushes = 0; var stops = 0
    /** Frames still queued (written − played) when stop() came; -1 until then. */
    var queuedAtStop = -1L
    /** The head stops moving (a HAL that stopped consuming). */
    var frozen = false
    /** fix2: after play(), the head does not move for this long (the HAL bus wake; cp12 139-408 ms). */
    var wakeDelayMs = 0L
    private var playAtUs = 0L
    /** Clock time of the first frame consumed after a play(), -1 = none yet. */
    var firstConsumeUs = -1L
    /** Frames that reached the speaker (non-silent and silent). */
    var playedFrames = 0L

    private fun advance() {
        val now = clock.us
        val dt = now - lastUs
        lastUs = now
        if (!playing || dt <= 0 || frozen) return
        if (now < playAtUs + wakeDelayMs * 1000) return
        if (!started) { if (written - consumed >= bufferFrames) started = true else return }
        val want = dt * rate / 1_000_000
        val can = minOf(want, written - consumed)
        if (can > 0 && firstConsumeUs < 0) firstConsumeUs = now
        consumed += can
        playedFrames += can
        if (can < want) { if (!starving) { starving = true; underruns++ } } else starving = false
    }

    override fun play() { advance(); playing = true; plays++; playAtUs = clock.us }
    override fun pause() { advance(); playing = false; started = false; starving = false; pauses++ }
    override fun flush() { advance(); if (!playing) written = consumed; flushes++ }
    override fun stop() { advance(); if (queuedAtStop < 0) queuedAtStop = written - consumed; playing = false; started = false; stops++ }
    override fun write(pcm: ByteArray, off: Int, len: Int): Int {
        advance()
        val fb = 2 * ch
        val room = bufferFrames - (written - consumed)
        val n = minOf((len / fb).toLong(), room)
        written += n
        return (n * fb).toInt()
    }
    override fun headFrames(): Long { advance(); return consumed }
    override fun underruns(): Int = underruns
}

internal class FakeFactory(val clock: FakeClock) : SinkFactory {
    val built = ArrayList<SinkHandle>()
    /** HAL bus wake applied to every track built (fix2 start-stall tests). */
    var wakeDelayMs = 0L
    var codecLag = 0
    var codecPtsShift = 0L
    val released = ArrayList<SinkHandle>()
    var failNext = false
    override fun build(p: Purpose, rate: Int, ch: Int): SinkHandle? {
        if (failNext) { failNext = false; return null }
        val frames = (rate.toLong() * VoiceLatency.deviceBufferMs(p) / 1000).toInt()
        val s = SinkHandle(p, rate, ch, FakeCodec(ch).also { it.lag = codecLag; it.ptsShift = codecPtsShift }, FakeTrack(clock, rate, ch, frames).also { it.wakeDelayMs = wakeDelayMs }, "fake")
        built.add(s)
        return s
    }
    override fun release(s: SinkHandle) { released.add(s) }
    fun track(p: Purpose) = built.last { it.purpose == p }.track as FakeTrack
}

/**
 * AudioManager stand-in. Results per purpose (default GRANTED). Keeps the holder stack so a test can
 * see whether MEDIA would have been handed GAIN (the stack emptied) between two moments.
 */
internal class FakeFocus(val clock: FakeClock) : FocusPort {
    data class Req(val p: Purpose, val mayDuck: Boolean, val atUs: Long, val listener: (Int) -> Unit, val handle: Any)
    val requests = ArrayList<Req>()
    val abandons = ArrayList<Pair<Purpose, Long>>()
    val results = HashMap<Purpose, ArrayDeque<Int>>()
    private val holders = LinkedHashMap<Any, Purpose>()
    /** Moments the holder set went empty (media would get GAIN). */
    val mediaGains = ArrayList<Long>()

    fun next(p: Purpose, r: Int) { results.getOrPut(p) { ArrayDeque() }.addLast(r) }
    /** Runs once inside the next request, before it returns (a concurrent request / release). */
    var duringRequest: (() -> Unit)? = null
    fun count(p: Purpose) = requests.count { it.p == p }
    fun holderCount() = holders.size
    fun held(p: Purpose) = holders.values.contains(p)
    fun last(p: Purpose) = requests.last { it.p == p }

    override fun request(purpose: Purpose, mayDuck: Boolean, listener: (Int) -> Unit): FocusGrant {
        val h = Any()
        requests.add(Req(purpose, mayDuck, clock.us, listener, h))
        duringRequest?.let { duringRequest = null; it() }
        val r = results[purpose]?.removeFirstOrNull() ?: FocusCodes.REQUEST_GRANTED
        if (r == FocusCodes.REQUEST_GRANTED) { holders[h] = purpose; return FocusGrant(r, h) }
        return FocusGrant(r, null)
    }

    override fun abandon(handle: Any) {
        val p = holders.remove(handle) ?: return
        abandons.add(p to clock.us)
        if (holders.isEmpty()) mediaGains.add(clock.us)
    }
}

internal class FakeDuck : DuckPort {
    data class Ramp(val target: Float, val durationMs: Long, val why: String, val stampUs: Long)
    val ramps = ArrayList<Ramp>()
    /** Ramps back to unity (an unduck with nothing live, a hold released at END, a lost unduck). */
    val restores: List<Ramp> get() = ramps.filter { it.target == 1f }
    override fun ramp(target: Float, durationMs: Long, why: String, stampUs: Long) { ramps.add(Ramp(target, durationMs, why, stampUs)) }
}

/** The engine under test, plus a reader-side API shaped like the wire. */
internal class VoiceRig(
    navDuringVoice: VoiceLatency.NavDuringVoice? = null,
    navUnduckHold: Boolean? = null,
    navDuckLift: Boolean? = null,
) {
    val clock = FakeClock()
    val log = CapLog()
    val factory = FakeFactory(clock)
    val focus = FakeFocus(clock)
    val duck = FakeDuck()
    val boards = ArrayList<String>()
    val core = VoiceStreams(clock, log, factory, focus, duck, { n, d -> boards.add("$n: $d") }) { }
    val engines = Purpose.values().associateWith { PurposeEngine(it, core) }
    var gen = core.onConnect()

    init {
        core.navDuringVoiceOverride = navDuringVoice
        core.navUnduckHoldOverride = navUnduckHold
        core.navDuckLiftOverride = navDuckLift
    }

    fun ms(t: Long) = t * 1000

    fun pump() {
        for (p in Purpose.values()) {
            val q = core.events.getValue(p)
            while (true) { val e = q.poll() ?: break; engines.getValue(p).onEvent(e) }
            engines.getValue(p).service()
        }
    }

    /** Advance [ms] in 1 ms steps; [each] runs before every pump (feeders, scripted events). */
    fun run(ms: Long, each: (Long) -> Unit = {}) {
        repeat(ms.toInt()) { clock.us += 1000; each(clock.us); pump(); core.sweep(clock.us) }
    }

    fun begin(scid: Long, streamType: Int, audioType: Int, rate: Int = 16000, ch: Int = 1, latency: Int = 80,
              beginSeq: Long = 0, codec: Int = VoiceFrame.CODEC_AAC_ELD) {
        core.onBegin(VoiceFrame.Begin(clock.us, scid, streamType, audioType, codec, rate, ch, latency, false, beginSeq), gen, clock.us)
    }

    fun au(scid: Long, seq: Int, ts: Long, loud: Boolean = false, streamType: Int = 100, atype: Int = 1,
           rate: Int = 16000, ch: Int = 1, rxUs: Long = clock.us) {
        core.onAu(VoiceFrame.AuExt(scid, rxUs, seq, ts, streamType), atype, rate, ch,
            byteArrayOf(if (loud) 1 else 0, 0, 0, 0), gen, clock.us)
    }

    fun end(scid: Long, frames: Long, reason: Int = VoiceFrame.END_TEARDOWN) {
        core.onEnd(VoiceFrame.End(clock.us, scid, reason, frames, 0, 0, 30), gen, clock.us)
    }

    fun modes(phone: Boolean? = null, speech: Boolean? = null) {
        core.ctrlSeen = true
        if (speech != null && speech != core.ctrlSpeech) { core.ctrlSpeechAtUs = clock.us; core.ctrlSpeech = speech }
        if (phone != null && phone != core.ctrlCall) {
            core.ctrlCall = phone
            if (!phone) listOf(Purpose.CALL, Purpose.ALERT, Purpose.ASSISTANT).forEach { core.post(it, VoiceEvent.PhoneIdle) }
        }
    }

    fun duckCmd(on: Boolean) {
        if (on) core.post(Purpose.NAV, VoiceEvent.Duck(0.2f, 500)) else core.post(Purpose.NAV, VoiceEvent.Unduck(500, clock.us))
    }

    fun reconnect() {
        core.onDisconnect(gen, clock.us)
        gen = core.onConnect()
    }

    fun stream(scid: Long): VoiceStream? = core.liveStreams().firstOrNull { it.scid == scid }
}

/** Sends a stream's AUs at real time (one per [auMs]), loud by [loudAt] (AU index → loud). */
internal class Feeder(
    val rig: VoiceRig, val scid: Long, val streamType: Int, val atype: Int, val rate: Int, val ch: Int,
    val auMs: Long, var loudAt: (Int) -> Boolean = { false },
) {
    var k = 0
    private var nextUs = rig.clock.us
    var on = true
    fun tick(now: Long) {
        while (on && now >= nextUs) {
            rig.au(scid, k and 0xFFFF, k * 480L, loudAt(k), streamType, atype, rate, ch, rxUs = nextUs)
            k++
            nextUs += auMs * 1000
        }
    }
}

class VoiceStreamsTest {

    @Test fun mappingRows() {
        fun m(st: Int, at: Int, c: Int = VoiceFrame.CODEC_AAC_ELD) = VoiceLatency.mapping(st, at, c)
        assertEquals(Purpose.CALL, m(100, VoiceLatency.AT_TELEPHONY).purpose)
        assertEquals(Purpose.ASSISTANT, m(100, VoiceLatency.AT_SPEECH).purpose)
        assertEquals(Purpose.ALERT, m(100, VoiceLatency.AT_ALERT).purpose)
        assertEquals("100 + default: deviation, ASSISTANT", Purpose.ASSISTANT, m(100, VoiceLatency.AT_DEFAULT).purpose)
        assertTrue("100 + compatibility: dropped", m(100, VoiceLatency.AT_COMPATIBILITY).unsupported)
        assertEquals(Purpose.NAV, m(101, VoiceLatency.AT_DEFAULT).purpose)
        assertTrue("101 + compatibility: dropped", m(101, VoiceLatency.AT_COMPATIBILITY).unsupported)
        assertTrue("media never on :9003", m(102, VoiceLatency.AT_MEDIA).unsupported)
        assertTrue("non-media AAC-LC: dropped", m(100, VoiceLatency.AT_TELEPHONY, VoiceFrame.CODEC_AAC_LC).unsupported)
    }

    /** BEGIN → real-time AUs → END: played, identity OK, focus once, released at END, sink pooled, track stopped. */
    @Test fun beginAusEnd() {
        val r = VoiceRig()
        r.begin(0xA1, 100, VoiceLatency.AT_TELEPHONY)
        val f = Feeder(r, 0xA1, 100, 1, 16000, 1, 30)
        r.run(2_000) { f.tick(it) }
        f.on = false
        r.end(0xA1, f.k.toLong())
        r.run(600)
        assertTrue(r.log.lines.joinToString("\n"), r.log.has("finished (END)"))
        assertTrue(r.log.has("identity OK"))
        assertFalse(r.log.has("MISMATCH"))
        assertEquals(1, r.focus.count(Purpose.CALL))
        assertFalse("released at END (no call up)", r.focus.held(Purpose.CALL))
        assertEquals(1, r.core.pooledCount())
        val t = r.factory.track(Purpose.CALL)
        assertTrue(t.stops >= 1)
        assertEquals("the track never starved mid-stream", 0, t.underruns)
        assertNull(r.core.liveOf(Purpose.CALL))
    }

    /** First audio ≈ first RX + target; rx→write age ≤ W + one AU while PLAYING. */
    @Test fun boundsWhilePlaying() {
        val r = VoiceRig()
        r.begin(0xB1, 100, VoiceLatency.AT_SPEECH)
        val f = Feeder(r, 0xB1, 100, 2, 16000, 1, 30)
        val t0 = r.clock.us
        var firstWriteUs = 0L
        r.run(1_500) { f.tick(it); if (firstWriteUs == 0L && r.factory.built.isNotEmpty() && r.factory.track(Purpose.ASSISTANT).written > 0) firstWriteUs = it }
        assertTrue("first write ≈ first rx + 80 ms (got ${(firstWriteUs - t0) / 1000} ms)", (firstWriteUs - t0) / 1000 in 78..90)
        assertEquals(0L, r.engines.getValue(Purpose.ASSISTANT).boundExceeded)
        val s = r.stream(0xB1)!!
        assertEquals("no re-buffer after the first word: the device buffer is primed at start (${s.jb.summary()})", 0L, s.jb.rebuffers)
        assertEquals(0, r.factory.track(Purpose.ASSISTANT).underruns)
        assertTrue("steady delay ≈ target: ${s.jb.bufferedMs()} ms", s.jb.bufferedMs() in 40..130)
    }

    @Test fun resentBeginResumesWithoutANewFocusRequest() {
        val r = VoiceRig()
        r.begin(0xC1, 100, VoiceLatency.AT_SPEECH)
        val f = Feeder(r, 0xC1, 100, 2, 16000, 1, 30)
        r.run(500) { f.tick(it) }
        r.begin(0xC1, 100, VoiceLatency.AT_SPEECH, beginSeq = 1)
        r.run(500) { f.tick(it) }
        assertTrue(r.log.has("RESUMED on its re-sent BEGIN"))
        assertEquals(1, r.focus.count(Purpose.ASSISTANT))
        assertEquals(1, r.core.liveStreams().size)
    }

    @Test fun unknownScidAuIsAnImplicitBeginFromTheStreamType() {
        val r = VoiceRig()
        r.au(0xD1, 0, 0, streamType = 101, atype = 4, rate = 48000, ch = 2)
        r.au(0xD2, 0, 0, streamType = 100, atype = 4)
        assertEquals(2L, r.core.implicitBegins)
        assertEquals(Purpose.NAV, r.stream(0xD1)!!.purpose)
        assertEquals(Purpose.ASSISTANT, r.stream(0xD2)!!.purpose)
        assertEquals("audioLatencyMs absent → 80", 80, r.stream(0xD1)!!.targetMs)
        assertEquals(2, r.log.count("implicit BEGIN"))
        assertTrue(r.log.lines.filter { it.contains("implicit BEGIN") }.all { it.startsWith("W ") })
    }

    @Test fun unknownScidEndIsIgnoredAndCounted() {
        val r = VoiceRig()
        r.end(0xE1, 10)
        assertEquals(1L, r.core.unknownEnds)
        assertTrue(r.log.has("unknown stream — ignored"))
    }

    /** cp8 18:32:43.529 / .681 / .785 on 100 (trailing telephony): each re-SETUP supersedes; no new focus request. */
    @Test fun supersedeTripleOnTelephony() {
        val r = VoiceRig()
        r.modes(phone = true)
        r.begin(0x10, 100, VoiceLatency.AT_TELEPHONY)
        val call = Feeder(r, 0x10, 100, 1, 16000, 1, 30)
        r.run(3_000) { call.tick(it) }
        // 43.529: TEARDOWN of the call stream and the first trailing SETUP.
        call.on = false
        r.end(0x10, call.k.toLong())
        r.begin(0x11, 100, VoiceLatency.AT_TELEPHONY)
        val a = Feeder(r, 0x11, 100, 1, 16000, 1, 30)
        r.run(152) { a.tick(it) }
        r.begin(0x12, 100, VoiceLatency.AT_TELEPHONY)            // .681
        a.on = false
        r.end(0x11, a.k.toLong(), VoiceFrame.END_SUPERSEDED)
        val b = Feeder(r, 0x12, 100, 1, 16000, 1, 30)
        r.run(104) { b.tick(it) }
        r.begin(0x13, 100, VoiceLatency.AT_TELEPHONY)            // .785
        b.on = false
        r.end(0x12, b.k.toLong(), VoiceFrame.END_SUPERSEDED)
        r.au(0x13, 0, 0); r.run(30); r.au(0x13, 1, 480)
        r.end(0x13, 2)                                           // "ended after 2 frames"
        r.run(600)
        assertEquals("the trailing re-SETUPs issue no request", 1, r.focus.count(Purpose.CALL))
        assertTrue(r.log.has("superseded by"))
        assertFalse(r.log.lines.joinToString("\n"), r.log.has("MISMATCH"))
        assertEquals("one identity line per stream", 4, r.log.count("identity OK"))
        assertTrue("slot kept while phone=1", r.focus.held(Purpose.CALL))
        r.modes(phone = false)
        r.run(10)
        assertFalse(r.focus.held(Purpose.CALL))
    }

    /** Reconnect mid-Siri: RECONNECT_HOLD, then the re-sent BEGIN resumes — no focus change. */
    @Test fun reconnectMidSiriResumesWithNoFocusChange() {
        val r = VoiceRig()
        r.begin(0x20, 100, VoiceLatency.AT_SPEECH)
        val f = Feeder(r, 0x20, 100, 2, 16000, 1, 30, loudAt = { true })
        r.run(800) { f.tick(it) }
        val t = r.factory.track(Purpose.ASSISTANT)
        val flushesBefore = t.flushes
        r.reconnect()
        r.run(5)
        assertEquals(VoiceStream.State.HOLD, r.stream(0x20)!!.state)
        assertTrue("held: track paused AND flushed", t.flushes > flushesBefore && !t.playing)
        r.run(200)
        r.begin(0x20, 100, VoiceLatency.AT_SPEECH, beginSeq = 1)
        r.run(500) { f.tick(it) }
        assertEquals(VoiceStream.State.ARMED, r.stream(0x20)!!.state)
        assertEquals(1, r.focus.count(Purpose.ASSISTANT))
        assertTrue(r.focus.abandons.isEmpty())
        assertTrue("output back on", t.playing)
    }

    @Test fun graceExpiryEndsWithoutPlayout() {
        val r = VoiceRig()
        r.begin(0x30, 100, VoiceLatency.AT_SPEECH)
        val f = Feeder(r, 0x30, 100, 2, 16000, 1, 30)
        r.run(500) { f.tick(it) }
        r.reconnect()
        r.run(VoiceLatency.RECONNECT_GRACE_MS + 50)
        assertTrue(r.log.has("grace expired"))
        assertNull(r.stream(0x30))
        assertFalse("ASSISTANT released at its (lifecycle) END", r.focus.held(Purpose.ASSISTANT))
        assertTrue(r.log.has("no END received (exempt from the identity)"))
    }

    @Test fun stallBackstopInfersAnEnd() {
        val r = VoiceRig()
        r.begin(0x40, 100, VoiceLatency.AT_TELEPHONY)
        val f = Feeder(r, 0x40, 100, 1, 16000, 1, 30)
        r.run(300) { f.tick(it) }
        f.on = false
        r.run(VoiceLatency.STREAM_STALL_MS + 100)
        assertTrue(r.log.lines.any { it.startsWith("W ") && it.contains("inferred END") })
        assertNull(r.stream(0x40))
    }

    /** closed fence: nothing is pooled after close, and every sink is freed exactly once. */
    @Test fun closedFenceAndPoolFreedExactlyOnce() {
        val r = VoiceRig()
        r.begin(0x50, 100, VoiceLatency.AT_TELEPHONY)
        val a = Feeder(r, 0x50, 100, 1, 16000, 1, 30)
        r.run(300) { a.tick(it) }
        a.on = false; r.end(0x50, a.k.toLong()); r.run(400)
        assertEquals(1, r.core.pooledCount())
        r.begin(0x51, 100, VoiceLatency.AT_SPEECH)
        val b = Feeder(r, 0x51, 100, 2, 16000, 1, 30)
        r.run(300) { b.tick(it) }
        assertTrue(r.core.close())
        assertFalse("close is idempotent", r.core.close())
        r.pump()
        assertEquals(0, r.core.pooledCount())
        assertEquals(r.factory.built.size, r.factory.released.size)
        assertEquals("each sink freed exactly once", r.factory.released.size, r.factory.released.toSet().size)
        assertTrue(r.engines.values.all { it.retired })
        assertNull("no build after close", r.core.takeSink(Purpose.CALL, 16000, 1))
    }

    /** A cold worker (the first build late, e.g. 1.5 s): the buffer is cut to W, not played late. */
    @Test fun lateWorkerNeverPlaysAGrowingBuffer() {
        val r = VoiceRig()
        r.begin(0x60, 100, VoiceLatency.AT_SPEECH)
        val f = Feeder(r, 0x60, 100, 2, 16000, 1, 30)
        // The reader keeps reading; the worker does not run for 1.5 s.
        repeat(1_500) { r.clock.us += 1000; f.tick(r.clock.us) }
        r.run(500) { f.tick(it) }
        val s = r.stream(0x60)!!
        assertTrue("trimmed (pool trigger and/or transition): excess ${s.jb.excess}", s.jb.excess >= 40)
        assertTrue("delay ≤ W after: ${s.jb.bufferedMs()}", s.jb.bufferedMs() <= 200)
        assertEquals(0L, r.engines.getValue(Purpose.ASSISTANT).boundExceeded)
    }

    /** A dropped-format stream never builds a sink and counts every AU as unsupported. */
    @Test fun unsupportedRowsAreCounted() {
        val r = VoiceRig()
        r.begin(0x70, 101, VoiceLatency.AT_COMPATIBILITY)
        repeat(5) { r.au(0x70, it, it * 480L, streamType = 101, atype = 5) }
        r.end(0x70, 5)
        assertTrue(r.log.lines.any { it.startsWith("E ") && it.contains("no PCM path") })
        assertTrue(r.log.has("unsupported 5"))
        assertTrue(r.log.has("identity OK"))
        assertTrue(r.factory.built.isEmpty())
    }

    /** Burst + duplicate + gap in one stream: END.frames = Σ buckets. */
    @Test fun accountingIdentityAcrossBurstsAndGaps() {
        val r = VoiceRig()
        r.begin(0x80, 100, VoiceLatency.AT_TELEPHONY)
        var k = 0
        r.run(300) { if (it % 30_000 == 0L) { r.au(0x80, k, k * 480L, rxUs = it); k++ } }
        // a 400 ms stall, then the burst (+ a duplicate)
        r.run(400)
        val burstAt = r.clock.us
        repeat(13) { r.au(0x80, k, k * 480L, rxUs = burstAt); k++ }
        r.au(0x80, k - 1, (k - 1) * 480L, rxUs = burstAt)
        k += 2                                                  // two lost
        r.run(500) { if (it % 30_000 == 0L) { r.au(0x80, k, k * 480L, rxUs = it); k++ } }
        val sent = r.stream(0x80)!!.read
        r.end(0x80, sent)
        r.run(500)
        assertTrue(r.log.lines.joinToString("\n"), r.log.has("identity OK"))
    }

    /** Siri re-SETUPs mid-turn: a BEGIN right after an END lets the ended stream play its last word out. */
    @Test fun anEndedStreamsPlayoutIsNotCutByTheNextBegin() {
        val r = VoiceRig()
        r.begin(0x90, 100, VoiceLatency.AT_SPEECH)
        val a = Feeder(r, 0x90, 100, 2, 16000, 1, 30, loudAt = { true })
        r.run(1_000) { a.tick(it) }
        a.on = false
        val sa = r.stream(0x90)!!
        r.end(0x90, a.k.toLong())
        r.begin(0x91, 100, VoiceLatency.AT_SPEECH)           // the next SETUP, same millisecond
        val b = Feeder(r, 0x91, 100, 2, 16000, 1, 30)
        r.run(1_000) { b.tick(it) }
        assertEquals("nothing of the ended stream superseded: ${sa.jb.summary()}", 0L, sa.jb.superseded)
        assertEquals(a.k.toLong(), sa.jb.played)
        val sb = r.stream(0x91)!!
        assertTrue("the next stream plays after it, trimmed to W: ${sb.jb.summary()}", sb.jb.played > 20 && sb.jb.bufferedMs() <= 200)
        assertEquals(1, r.focus.count(Purpose.ASSISTANT))
        assertTrue(r.log.has("waits for"))
    }

    /** A decoder one AU behind, rewriting its pts: outputs are matched in order, every AU is played. */
    @Test fun laggingDecoderWithRewrittenPtsStillPlaysEveryAu() {
        val r = VoiceRig()
        r.factory.codecLag = 1
        r.factory.codecPtsShift = 2_000
        r.begin(0xA0, 100, VoiceLatency.AT_SPEECH)
        val s = r.stream(0xA0)!!
        val f = Feeder(r, 0xA0, 100, 2, 16000, 1, 30, loudAt = { true })
        r.run(2_000) { f.tick(it) }
        f.on = false; r.end(0xA0, f.k.toLong()); r.run(500)
        assertEquals(f.k.toLong(), s.jb.played + s.jb.decodeMiss)
        assertTrue("at most the last AU (still inside the decoder) is a decode miss: ${s.jb.decodeMiss}", s.jb.decodeMiss <= 1)
        assertTrue(r.log.has("matched in order (FIFO)"))
    }

    /**
     * Review round 1 (surviving mutant): END plays out, then WAITS for the playback head to reach what
     * was written before stop + flush — a stop with a device buffer still queued cuts the last word.
     */
    @Test fun endWaitsForTheHeadBeforeStopFlush() {
        val r = VoiceRig()
        r.begin(0xB0, 100, VoiceLatency.AT_SPEECH)
        val f = Feeder(r, 0xB0, 100, 2, 16000, 1, 30, loudAt = { true })
        r.run(1_000) { f.tick(it) }
        f.on = false; r.end(0xB0, f.k.toLong())
        r.run(600)
        val t = r.factory.track(Purpose.ASSISTANT)
        assertTrue(r.log.has("finished (END)"))
        val period = 16000L * VoiceLatency.PERIOD_MS / 1000
        assertTrue("stopped with ${t.queuedAtStop} frames still queued (≤ one period allowed)", t.queuedAtStop in 0..period)
    }

    /** ... and a head that never moves cannot hold the END forever: the bounded wait (device buffer + 50 ms) ends it. */
    @Test fun endHeadDrainIsBounded() {
        val r = VoiceRig()
        r.begin(0xB1, 100, VoiceLatency.AT_SPEECH)
        val f = Feeder(r, 0xB1, 100, 2, 16000, 1, 30)
        r.run(1_000) { f.tick(it) }
        f.on = false
        r.factory.track(Purpose.ASSISTANT).frozen = true
        r.end(0xB1, f.k.toLong())
        r.run(600)
        assertTrue(r.log.lines.joinToString("\n"), r.log.has("finished (END"))
        assertFalse("the slot is released, not pinned by a stuck HAL", r.focus.held(Purpose.ASSISTANT))
        assertTrue(r.factory.track(Purpose.ASSISTANT).queuedAtStop > 0)
    }

    /** Review round 1: a stream that waited behind an END runs at the target afterwards, not at up to W. */
    @Test fun aWaitedStreamRunsAtTheTargetAfterItStarts() {
        val r = VoiceRig()
        r.begin(0xC0, 100, VoiceLatency.AT_SPEECH)
        val a = Feeder(r, 0xC0, 100, 2, 16000, 1, 30, loudAt = { true })
        r.run(1_000) { a.tick(it) }
        a.on = false; r.end(0xC0, a.k.toLong())
        r.begin(0xC1, 100, VoiceLatency.AT_SPEECH)
        val b = Feeder(r, 0xC1, 100, 2, 16000, 1, 30)
        var maxAfter = 0L
        r.run(3_000) { b.tick(it); val sb = r.stream(0xC1); if (it > r.clock.us - 2_500_000 && sb != null && sb.jb.state == VoiceJitterBuffer.State.PLAYING) maxAfter = maxOf(maxAfter, sb.jb.bufferedMs()) }
        assertTrue("delay after the waited start ≤ target + one AU: $maxAfter ms", maxAfter <= 110)
        assertTrue(r.log.has("started after waiting"))
    }

    @Test fun timingConstantsArePinned() {
        assertEquals(500L, VoiceLatency.RECONNECT_GRACE_MS)
        assertEquals(3_000L, VoiceLatency.STREAM_STALL_MS)
        assertEquals(50L, VoiceLatency.HEAD_DRAIN_SLACK_MS)
        assertEquals(8, VoiceLatency.PERIOD_MS)
        assertEquals(listOf(32, 128, 48, 48), listOf(Purpose.CALL, Purpose.ASSISTANT, Purpose.ALERT, Purpose.NAV).map { VoiceLatency.deviceBufferMs(it) })
    }

    /**
     * fix2 (rc1 cp12: first AU → busActivated 139-408 ms, excess tracking it): a 300 ms HAL bus wake is
     * carried as delay, never trimmed — no loud AU is cut at the start — and the delay returns to ≤ W
     * from the stream's silence.
     */
    @Test fun aDeviceStartStallDelaysTheFirstWordsAndNeverCutsThem() {
        val r = VoiceRig()
        r.factory.wakeDelayMs = 300
        r.begin(0xD0, 101, VoiceLatency.AT_DEFAULT, 48000, 2)
        val s = r.stream(0xD0)!!
        // 1.5 s of speech, then 1 s of silence (a pause), then speech again.
        val f = Feeder(r, 0xD0, 101, 4, 48000, 2, 10, loudAt = { it < 150 || it >= 250 })
        r.run(3_500) { f.tick(it) }
        assertEquals("no loud AU trimmed: ${s.jb.summary()}", 0L, s.jb.excessLoud)
        assertTrue(r.log.lines.joinToString("\n"), r.log.has("device started 30"))
        assertTrue("the allowance was paid back from silence: ${s.jb.summary()}", s.jb.excessSilentCatchUp > 0)
        assertTrue("back to ≤ W after the pause: ${s.jb.bufferedMs()}", s.jb.bufferedMs() <= 200)
    }

    /** ...and a device that never wakes keeps the buffer bounded (start cap), the stream still ENDs. */
    @Test fun aDeviceThatNeverStartsIsBounded() {
        val r = VoiceRig()
        r.factory.wakeDelayMs = 60_000
        r.begin(0xD1, 101, VoiceLatency.AT_DEFAULT, 48000, 2)
        val s = r.stream(0xD1)!!
        val f = Feeder(r, 0xD1, 101, 4, 48000, 2, 10, loudAt = { true })
        var maxBuf = 0L
        r.run(2_000) { f.tick(it); maxBuf = maxOf(maxBuf, s.jb.bufferedMs()) }
        assertTrue("bounded by W + the start cap: $maxBuf", maxBuf <= 200 + VoiceLatency.START_STALL_CAP_MS + 10)
        assertTrue(r.log.has("head did not move"))
        f.on = false; r.end(0xD1, f.k.toLong()); r.run(3_000)
        assertTrue(r.log.has("finished (END"))
    }

    /** Review MINOR: a short stream that ENDs while the device is still waking is played, not retired unplayed. */
    @Test fun aShortStreamEndingDuringTheBusWakeIsPlayedOut() {
        val r = VoiceRig()
        r.factory.wakeDelayMs = 350
        r.begin(0xD2, 101, VoiceLatency.AT_DEFAULT, 48000, 2)
        val s = r.stream(0xD2)!!
        val f = Feeder(r, 0xD2, 101, 4, 48000, 2, 10, loudAt = { true })
        r.run(150) { f.tick(it) }                                   // a 150 ms tone
        f.on = false; r.end(0xD2, f.k.toLong()); r.run(1_500)
        assertEquals(0L, s.jb.retiredUnplayed)
        assertEquals("all of it played: ${s.jb.summary()}", f.k.toLong(), s.jb.played + s.jb.decodeMiss)
    }

    /** Review MINOR: the decode horizon covers a whole device buffer — a refill after a worker stall finds every AU decoded. */
    @Test fun aDeviceBufferRefillFindsEveryAuDecoded() {
        val r = VoiceRig()
        r.begin(0xD3, 100, VoiceLatency.AT_SPEECH)               // ASSISTANT: 128 ms device buffer > 80 ms target
        val s = r.stream(0xD3)!!
        val f = Feeder(r, 0xD3, 100, 2, 16000, 1, 30, loudAt = { true })
        r.run(1_000) { f.tick(it) }
        // The worker stalls 150 ms (the reader keeps reading; the device keeps playing and drains).
        repeat(150) { r.clock.us += 1000; f.tick(r.clock.us) }
        r.run(1_000) { f.tick(it) }
        assertEquals("no decode miss after the refill: ${s.jb.summary()}", 0L, s.jb.decodeMiss)
    }
}
