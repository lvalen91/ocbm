package zeno.gmccpa.av

import zeno.gmccpa.av.VoiceLatency.Purpose
import java.util.concurrent.ConcurrentLinkedQueue

// =====================================================================================================
// Plan v5.8 V1b/V2/V3 — the voice engine, Android-free.
//
// One stream per CarPlay voice stream (scid), BEGIN → END, each with its own jitter buffer; one worker
// per purpose (CALL, ASSISTANT, ALERT, NAV) that owns that purpose's codec + track; focus per V2 (NAV
// takes none: owner OD-8C); a pool of stopped codec + track pairs. Everything acts through injected ports, so
// the whole state machine runs under host-JVM JUnit with a fake clock (VoiceStreamsTest).
//
// Threads (VoiceRouter runs them):
//  - the READER (consume thread, [VoiceReader]): parses and routes by scid, registers a BEGIN's
//    stream + jitter buffer synchronously, puts AUs into the stream's buffer, and only ENQUEUES
//    events. It never blocks on a worker and never touches a codec, track or focus.
//  - a WORKER per purpose ([PurposeEngine]): all BEGIN/END processing (sink, focus, pool), decode-ahead
//    and writes. The only thread that touches its purpose's codec and track.
//  - the SWEEPER: stall backstop, RECONNECT_HOLD expiry, the assistant edge, the route latch, reports.
// Locks: `sinks` (this [VoiceStreams] object's monitor) → the jitter-buffer lock (a leaf). Focus slots
// have their own leaf monitors and are never called under `sinks`. The leaf rule is kept by the
// pull path too: `VoiceJitterBuffer.pull` calls the engine's `fateOf` under its monitor, so that
// lambda takes no lock — `navSuppressed()` (which takes `sinks`) is snapshotted before the pull
// (C-F2, 2026-10-08). The one remaining `sinks` → jb path, `identityLine` → `bucketSum` on an END,
// runs only for streams no worker pulls (dropped / superseded).
// =====================================================================================================

/** The receiver's voice clock (µs). Production: NativeCore.voiceClockUs(); tests: a fake. */
fun interface VoiceClock { fun nowUs(): Long }

interface VoiceLog {
    fun i(msg: String)
    fun w(msg: String)
    fun e(msg: String)
}

/** One decoded output: [key] is the node's RTP timestamp it was queued with. */
class Decoded(val key: Long, val pcm: ByteArray)

/** A decoder (production: MediaCodec AAC-ELD). Worker thread only. */
interface CodecPort {
    /** Queue one AU under [key]; false = no input buffer right now (try again next pass). */
    fun queue(au: ByteArray, key: Long): Boolean
    /** Whatever output is ready, waiting at most [waitUs]. */
    fun drain(waitUs: Long): List<Decoded>
    /** Drop everything queued (a discontinuity: skipped or missing AUs). */
    fun flush()
}

/** A PCM output (production: an AudioTrack in MODE_STREAM, start threshold = the device buffer). */
interface TrackPort {
    /** The device buffer, frames (floored at the platform minimum). */
    val bufferFrames: Int
    fun play()
    fun pause()
    fun flush()
    fun stop()
    /** Non-blocking write; bytes written, or < 0 on error (dead track). */
    fun write(pcm: ByteArray, off: Int, len: Int): Int
    /** Playback head, frames (32-bit counter widened). */
    fun headFrames(): Long
    /** Output latency estimate (getTimestamp) for [writtenFrames] written since the head base, or null. */
    fun outputLatencyMs(writtenFrames: Long): Long? = null
    /** The platform's underrun counter for this track (AudioTrack.getUnderrunCount), or -1. */
    fun underruns(): Int = -1
}

/** One purpose's codec + track, built for one format. Pooled stopped, flushed and focus-less. */
class SinkHandle(val purpose: Purpose, val rate: Int, val ch: Int, val codec: CodecPort, val track: TrackPort, val desc: String)

/** Builds and frees sinks (production: MediaCodec + AudioTrack with the purpose's usage). */
interface SinkFactory {
    fun build(p: Purpose, rate: Int, ch: Int): SinkHandle?
    fun release(s: SinkHandle)
}

/** V3: the media duck. Production: AacPlayer.setVoiceDuckRamp through the duck executor. */
interface DuckPort {
    /** Ramp the media duck to [target] over [durationMs]; [stampUs] = when the cause arrived (executor latency). */
    fun ramp(target: Float, durationMs: Long, why: String, stampUs: Long)
}

/** SessionTrace.Board in production. */
fun interface BoardPort { fun state(name: String, detail: String) }

/** A voice stream: one CarPlay audio stream (scid) from BEGIN to END. */
class VoiceStream(
    val scid: Long,
    val streamType: Int,
    val audioType: Int,
    val codec: Int,
    val rate: Int,
    val ch: Int,
    val audioLatencyMs: Int,
    val purpose: Purpose?,
    val unsupported: Boolean,
    val implicit: Boolean,
    val beginReadUs: Long,
) {
    enum class State { PENDING_WORKER, ARMED, HOLD, ENDING, SUPERSEDED, RETIRED }

    val targetMs = VoiceLatency.targetMs(audioLatencyMs)
    val jb = VoiceJitterBuffer(rate, ch, targetMs)
    val label = "${purpose?.label ?: "dropped"}/$streamType scid=${java.lang.Long.toHexString(scid)}"
    val stats = VoiceStreamStats(scid, label)

    @Volatile var gen = 0
    @Volatile var state = State.PENDING_WORKER
    /** AUs the reader read for this scid — every one, whatever happened to it. Reader thread only. */
    @Volatile var read = 0L
    @Volatile var staleGen = 0L
    @Volatile var unsupportedAus = 0L
    /** AUs read after the stream was superseded (the old thread runs on until it sees its flag). */
    @Volatile var supersededLate = 0L
    /** AUs read after a lifecycle END retired it (stall / grace) and before it left the registry. */
    @Volatile var retiredLate = 0L
    @Volatile var lastAuUs = beginReadUs
    @Volatile var endFrame: VoiceFrame.End? = null
    /** The END was read (reader edge) — `between BEGIN and END` for navSuppressed / siriLive. */
    @Volatile var endRead = false
    @Volatile var holdUntilUs = 0L
    @Volatile var superseded = false
    /** Its identity line was logged (once per stream). Worker thread. */
    var identityLogged = false

    // NAV, worker thread only
    /** Began while navSuppressed: suppressed until its END or a duckAudio while NOT suppressed (rule a). */
    var latchedSuppressed = false
    /** DROP (non-default): cut by a suppression onset; suppressed until navSuppressed turns false (rule b). */
    var cutSuppressed = false

    /** The node buckets plus the stream-level ones; equals [read] when every AU is accounted for. */
    fun bucketTotal(): Long = jb.bucketSum() + unsupportedAus + staleGen + supersededLate + retiredLate

    /**
     * The END line's accounting identity: END.frames = played + late + excess + epoch_idle +
     * suppressed + refused_lost + superseded + transport_lost + unsupported + retired_unplayed +
     * stale_gen, where transport_lost = END.frames − AUs read.
     */
    fun identityLine(end: VoiceFrame.End): String {
        val transportLost = end.frames - read
        val sum = bucketTotal() + transportLost
        val ok = sum == end.frames
        return "END.frames ${end.frames} ${if (ok) "=" else "≠"} played ${jb.played} + decode_miss ${jb.decodeMiss} + late ${jb.late} + excess ${jb.excess} + " +
            "epoch_idle ${jb.epochIdle} + suppressed ${jb.suppressed} + refused_lost ${jb.refusedLost} + " +
            "superseded ${jb.superseded + supersededLate} + transport_lost $transportLost + unsupported $unsupportedAus + " +
            "retired_unplayed ${jb.retiredUnplayed + retiredLate} + stale_gen $staleGen" +
            (if (ok) " — identity OK" else " — IDENTITY MISMATCH (sum $sum, read $read)")
    }

    fun identityHolds(end: VoiceFrame.End): Boolean = bucketTotal() + (end.frames - read) == end.frames
}

/** Worker events. Produced by the reader, the sweeper, focus listeners and cp-meta; consumed by one engine. */
sealed class VoiceEvent {
    class Begin(val s: VoiceStream, val resume: Boolean, val gen: Int) : VoiceEvent()
    class End(val s: VoiceStream, val gen: Int) : VoiceEvent()
    /** Lifecycle ENDs (exempt from the gen fence): grace expiry, the stall backstop. */
    class LifecycleEnd(val s: VoiceStream, val why: String, val stall: Boolean) : VoiceEvent()
    class Hold(val s: VoiceStream) : VoiceEvent()
    class Focus(val change: Int) : VoiceEvent()
    object PhoneIdle : VoiceEvent()
    class Duck(val volume: Float, val durationMs: Long) : VoiceEvent()
    class Unduck(val durationMs: Long, val stampUs: Long) : VoiceEvent()
    object Report : VoiceEvent()
    object Retire : VoiceEvent()
}

/**
 * The registry, pool and shared state (`sinks` = this object's monitor). VoiceRouter owns one per
 * session.
 */
class VoiceStreams(
    val clock: VoiceClock,
    val log: VoiceLog,
    private val factory: SinkFactory,
    focusPort: FocusPort,
    val duck: DuckPort,
    val board: BoardPort,
    /** Wakes the purpose worker (VoiceRouter: its condition). */
    private val wake: (Purpose) -> Unit,
) {
    // ---- under `sinks` (this) ----
    var connGen = 0
        private set
    var closed = false
        private set
    private val streams = HashMap<Long, VoiceStream>()
    /** The purpose's stream between BEGIN and END (reader edges). */
    private val live = HashMap<Purpose, VoiceStream>()
    private val pool = HashMap<String, ArrayDeque<SinkHandle>>()
    private var poolFreed = false
    var implicitBegins = 0L; private set
    var unknownEnds = 0L; private set
    var staleGenEvents = 0L; private set
    var v1Dropped = 0L; private set
    /** V4a: NAV pool entries are kept only while a route is active (set by the sweeper's latch). */
    @Volatile var navRetain = true

    // ---- control plane (volatiles, written on cp-meta) ----
    @Volatile var ctrlSeen = false
    @Volatile var ctrlCall = false
    @Volatile var ctrlSpeech = false
    @Volatile var ctrlSpeechAtUs = 0L
    @Volatile var lastAssistantBeginUs = 0L
    @Volatile var callEndAtUs = 0L

    val events: Map<Purpose, ConcurrentLinkedQueue<VoiceEvent>> =
        Purpose.values().associateWith { ConcurrentLinkedQueue<VoiceEvent>() }

    val slots: Map<Purpose, FocusSlot> = listOf(Purpose.CALL, Purpose.ASSISTANT, Purpose.ALERT)
        .associateWith { FocusSlot(it, focusPort) { p, c -> post(p, VoiceEvent.Focus(c)) } }

    fun post(p: Purpose, e: VoiceEvent) { events.getValue(p).add(e); wake(p) }

    /** callLive / assistantLive: a stream of that purpose between BEGIN and END (reader edges). */
    fun liveOf(p: Purpose): VoiceStream? = synchronized(this) { live[p] }

    fun siriLive(nowUs: Long): Boolean = VoiceIdleRules.assistantLive(
        nowUs / 1000, liveOf(Purpose.ASSISTANT) != null, ctrlSeen && ctrlSpeech, ctrlSpeechAtUs / 1000,
        CTRL_LEAD_MS, lastAssistantBeginUs / 1000)

    /**
     * Test-only override of NAV_DURING_VOICE (JUnit drives both modes through the real engine). Null
     * in production, so the switch is still read in exactly one place: [VoiceIdleRules.navSuppressed]
     * (its default argument).
     */
    @Volatile var navDuringVoiceOverride: VoiceLatency.NavDuringVoice? = null
    /** Test-only override of NAV_DUCK_LIFT_ON_BUS (null in production). */
    @Volatile var navDuckLiftOverride: Boolean? = null

    /** The only read of [VoiceLatency.NAV_DUCK_LIFT_ON_BUS]. */
    fun navDuckLift(): Boolean = navDuckLiftOverride ?: VoiceLatency.NAV_DUCK_LIFT_ON_BUS

    /** Test-only override of NAV_UNDUCK_HOLD (null in production). */
    @Volatile var navUnduckHoldOverride: Boolean? = null

    /** The only read of [VoiceLatency.NAV_UNDUCK_HOLD]. */
    fun navUnduckHold(): Boolean = navUnduckHoldOverride ?: VoiceLatency.NAV_UNDUCK_HOLD

    fun navSuppressed(): Boolean {
        val call = liveOf(Purpose.CALL) != null
        val siri = liveOf(Purpose.ASSISTANT) != null
        val phone = ctrlSeen && ctrlCall
        val o = navDuringVoiceOverride
        return if (o != null) VoiceIdleRules.navSuppressed(call, siri, phone, o) else VoiceIdleRules.navSuppressed(call, siri, phone)
    }

    fun suppressedBy(): String = VoiceIdleRules.suppressedBy(liveOf(Purpose.CALL) != null, ctrlSeen && ctrlCall)

    // ---- reader side ------------------------------------------------------------------------------

    /** consume() entry: a new :9003 connection. Returns its gen, or -1 when closed. */
    fun onConnect(): Int = synchronized(this) { if (closed) -1 else ++connGen }

    /** A BEGIN read on connection [gen]. Registration is synchronous (rev f, C3). */
    fun onBegin(b: VoiceFrame.Begin, gen: Int, readUs: Long) {
        var post: Pair<Purpose, VoiceEvent>? = null
        var line: String
        synchronized(this) {
            if (gen != connGen) { staleGenEvents++; log.w("voice: BEGIN scid=${hex(b.scid)} on a stale connection (gen $gen, now $connGen) — dropped"); return }
            val known = streams[b.scid]
            if (known != null && known.state != VoiceStream.State.RETIRED && !known.superseded &&
                (b.beginSeq > 0 || known.state == VoiceStream.State.HOLD)) {
                // Resume: the re-sent BEGIN after a reconnect. Rebound to this connection's gen.
                known.gen = gen
                val wasHold = known.state == VoiceStream.State.HOLD
                if (wasHold) known.state = VoiceStream.State.ARMED
                line = "voice: ${known.label} RESUMED on its re-sent BEGIN (begin_seq ${b.beginSeq}${if (wasHold) ", out of RECONNECT_HOLD" else ""})"
                if (known.purpose != null) post = known.purpose to VoiceEvent.Begin(known, true, gen)
            } else {
                val m = VoiceLatency.mapping(b.streamType, b.audioType, b.codec)
                val s = VoiceStream(b.scid, b.streamType, b.audioType, b.codec, b.rate, b.ch, b.audioLatencyMs,
                    m.purpose, m.unsupported, false, readUs)
                s.gen = gen
                streams[b.scid] = s
                line = "voice: BEGIN ${s.label} audioType=${b.audioType} ${VoiceFrame.codecName(b.codec)} ${b.rate}Hz ${b.ch}ch " +
                    "audioLatencyMs=${b.audioLatencyMs} → target ${s.targetMs}ms W ${s.jb.wMs}ms (${m.why})" +
                    (if (b.beginSeq > 0) ", begin_seq ${b.beginSeq}" else "") +
                    "; BEGIN written ${b.writeUs}us, read ${readUs}us"
                if (m.unsupported) {
                    log.e("voice: ${s.label}: ${m.why} — its AUs are DROPPED (counted as unsupported)")
                } else {
                    val p = m.purpose!!
                    val old = live[p]
                    if (old != null && old !== s) {
                        old.superseded = true
                        line += "; supersedes ${old.label}"
                    }
                    live[p] = s
                    if (p == Purpose.ASSISTANT) lastAssistantBeginUs = readUs
                    post = p to VoiceEvent.Begin(s, false, gen)
                }
            }
        }
        log.i(line)
        post?.let { post(it.first, it.second) }
    }

    fun onEnd(e: VoiceFrame.End, gen: Int, readUs: Long) {
        var post: Pair<Purpose, VoiceEvent>? = null
        var msg: String? = null
        synchronized(this) {
            if (gen != connGen) { staleGenEvents++; return }
            val s = streams[e.scid]
            if (s == null) {
                unknownEnds++
                msg = "voice: END scid=${hex(e.scid)} for an unknown stream — ignored ($unknownEnds so far)"
                return@synchronized
            }
            s.endFrame = e
            s.endRead = true
            s.purpose?.let { if (live[it] === s) live.remove(it) }
            if (s.purpose == Purpose.CALL) callEndAtUs = readUs
            when {
                s.unsupported || s.purpose == null -> {
                    streams.remove(e.scid); s.state = VoiceStream.State.RETIRED
                    msg = "voice: END ${s.label} reason=${VoiceFrame.reasonName(e.reason)} (dropped stream): ${s.identityLine(e)}"
                }
                s.superseded && s.state == VoiceStream.State.SUPERSEDED -> {
                    streams.remove(e.scid); s.state = VoiceStream.State.RETIRED
                    msg = "voice: END ${s.label} reason=${VoiceFrame.reasonName(e.reason)} — superseded, END ignored: ${s.identityLine(e)}"
                }
                else -> post = s.purpose to VoiceEvent.End(s, gen)
            }
        }
        msg?.let { if (it.contains("MISMATCH") || it.contains("unknown stream")) log.w(it) else log.i(it) }
        post?.let { post(it.first, it.second) }
    }

    /**
     * One v2 AU. Puts it into its stream's buffer (implicit BEGIN for an unknown scid) and wakes the
     * worker. Returns the stream (null when dropped as stale).
     */
    fun onAu(ext: VoiceFrame.AuExt, atype: Int, rate: Int, ch: Int, au: ByteArray, gen: Int, readUs: Long): VoiceStream? {
        var implicitLine: String? = null
        var postBegin: Pair<Purpose, VoiceEvent>? = null
        val s: VoiceStream
        synchronized(this) {
            var st = streams[ext.scid]
            if (st == null) {
                // Implicit BEGIN from the v2 head: stream_type + atype, audioLatencyMs → 80 (default).
                val m = VoiceLatency.mapping(ext.streamType, atype, VoiceFrame.CODEC_AAC_ELD)
                st = VoiceStream(ext.scid, ext.streamType, atype, VoiceFrame.CODEC_AAC_ELD, rate, ch, -1,
                    m.purpose, m.unsupported, true, readUs)
                st.gen = gen
                streams[ext.scid] = st
                implicitBegins++
                implicitLine = "voice: AU for unknown scid=${hex(ext.scid)} — implicit BEGIN ${st.label} (${m.why}; " +
                    "audioLatencyMs absent → ${st.targetMs}ms; $implicitBegins so far)"
                if (!m.unsupported) {
                    val p = m.purpose!!
                    live[p]?.let { if (it !== st) it.superseded = true }
                    live[p] = st
                    postBegin = p to VoiceEvent.Begin(st, false, gen)
                }
            }
            s = st
        }
        implicitLine?.let { log.w(it) }
        postBegin?.let { post(it.first, it.second) }
        s.read++
        s.lastAuUs = readUs
        s.stats.onRead(ext.rxUs, readUs, ext.seq)
        when {
            s.gen != gen -> { s.staleGen++; return null }
            s.unsupported || s.purpose == null -> { s.unsupportedAus++; return s }
            s.superseded -> { s.supersededLate++; return s }
            s.state == VoiceStream.State.RETIRED -> { s.retiredLate++; return s }
        }
        s.jb.put(VoiceJitterBuffer.Node(ext.rtpTs, ext.seq, ext.rxUs, au), readUs)
        wake(s.purpose)
        return s
    }

    /** A v1 AU on a v2 seam (a stale native build): dropped, counted, one E. */
    fun onV1Au(atype: Int, rate: Int, ch: Int) {
        val first = synchronized(this) { ++v1Dropped == 1L }
        if (first) log.e("voice: a v1-tagged AU (atype $atype, ${rate}Hz ${ch}ch) on the v2 seam — the native build is stale; v1 AUs are DROPPED")
    }

    /**
     * consume()'s finally. Closed → RETIRE everything (exactly once with the sweeper's exit path);
     * otherwise RECONNECT_HOLD every live stream for [VoiceLatency.RECONNECT_GRACE_MS].
     */
    fun onDisconnect(gen: Int, nowUs: Long) {
        val holds = ArrayList<VoiceStream>()
        synchronized(this) {
            if (closed) return
            if (gen != connGen) return
            for (s in streams.values) {
                if (s.purpose == null || s.unsupported) continue
                // A stream whose END was already read plays out; only open streams are held.
                if (s.endRead) continue
                if (s.state == VoiceStream.State.ARMED || s.state == VoiceStream.State.PENDING_WORKER) {
                    s.state = VoiceStream.State.HOLD
                    s.holdUntilUs = nowUs + VoiceLatency.RECONNECT_GRACE_MS * 1000
                    holds.add(s)
                }
            }
        }
        for (s in holds) {
            log.i("voice: ${s.label} RECONNECT_HOLD for ${VoiceLatency.RECONNECT_GRACE_MS}ms (the :9003 connection ended; focus and duck kept)")
            post(s.purpose!!, VoiceEvent.Hold(s))
        }
    }

    /**
     * The stop path: closed is latched, every engine retires its stream (released, not pooled) and
     * the pool is freed — exactly once, by whichever caller takes `sinks` first.
     */
    fun close(): Boolean {
        val free = ArrayList<SinkHandle>()
        val first: Boolean
        synchronized(this) {
            first = !closed
            closed = true
            if (!poolFreed) {
                poolFreed = true
                pool.values.forEach { free.addAll(it) }
                pool.clear()
            }
        }
        if (first) Purpose.values().forEach { post(it, VoiceEvent.Retire) }
        free.forEach { runCatching { factory.release(it) } }
        return first
    }

    // ---- sweeper ----------------------------------------------------------------------------------

    /** Grace expiry and the stall backstop; both are lifecycle ENDs, exempt from the gen fence. */
    fun sweep(nowUs: Long) {
        val out = ArrayList<Pair<Purpose, VoiceEvent>>()
        synchronized(this) {
            for (s in streams.values) {
                val p = s.purpose ?: continue
                if (s.state == VoiceStream.State.RETIRED || s.state == VoiceStream.State.SUPERSEDED) continue
                if (s.state == VoiceStream.State.HOLD && nowUs >= s.holdUntilUs) {
                    s.state = VoiceStream.State.RETIRED
                    if (live[p] === s) live.remove(p)
                    out.add(p to VoiceEvent.LifecycleEnd(s, "no re-sent BEGIN within ${VoiceLatency.RECONNECT_GRACE_MS}ms (grace expired)", false))
                } else if (s.state != VoiceStream.State.HOLD && s.endFrame == null &&
                    nowUs - s.lastAuUs >= VoiceLatency.STREAM_STALL_MS * 1000) {
                    s.state = VoiceStream.State.RETIRED
                    if (live[p] === s) live.remove(p)
                    out.add(p to VoiceEvent.LifecycleEnd(s, "no AU for ${VoiceLatency.STREAM_STALL_MS}ms and no END (stall backstop)", true))
                }
            }
            // Superseded streams whose trailing END never came: forget them after the stall window.
            val it = streams.values.iterator()
            while (it.hasNext()) {
                val s = it.next()
                if (s.state == VoiceStream.State.SUPERSEDED && nowUs - s.lastAuUs >= VoiceLatency.STREAM_STALL_MS * 1000) {
                    it.remove(); s.state = VoiceStream.State.RETIRED
                }
            }
        }
        out.forEach { post(it.first, it.second) }
    }

    /** Remove a stream from the registry once its worker has finished it. */
    fun finished(s: VoiceStream) {
        synchronized(this) {
            if (streams[s.scid] === s) streams.remove(s.scid)
            s.purpose?.let { if (live[it] === s) live.remove(it) }
            if (s.state != VoiceStream.State.SUPERSEDED) s.state = VoiceStream.State.RETIRED
        }
    }

    /** PENDING_WORKER → ARMED under `sinks`, so it cannot overwrite a concurrent RECONNECT_HOLD (review NIT). */
    fun armIfPending(s: VoiceStream) {
        synchronized(this) { if (s.state == VoiceStream.State.PENDING_WORKER) s.state = VoiceStream.State.ARMED }
    }

    fun markSuperseded(s: VoiceStream) {
        synchronized(this) {
            s.superseded = true
            s.state = VoiceStream.State.SUPERSEDED
            s.purpose?.let { if (live[it] === s) live.remove(it) }
            // A superseded stream whose END was already read is finished now.
            if (s.endRead && streams[s.scid] === s) { streams.remove(s.scid); s.state = VoiceStream.State.RETIRED }
        }
    }

    /**
     * The connGen fence (rev e, R3; rev f, C3), checked under `sinks` before a worker's per-event
     * mutation: an event read on connection [eventGen] still applies to [s] only if [s] has not been
     * REBOUND to a newer connection by a re-sent BEGIN since. (An END read on the connection that
     * carried the stream's BEGIN is honoured even after that connection closed.)
     */
    fun eventCurrent(s: VoiceStream, eventGen: Int): Boolean = synchronized(this) {
        if (s.gen != eventGen) { staleGenEvents++; false } else true
    }

    fun liveStreams(): List<VoiceStream> = synchronized(this) { streams.values.toList() }

    // ---- pool ------------------------------------------------------------------------------------

    private fun key(p: Purpose, rate: Int, ch: Int) = "${p.name}/$rate/$ch"

    /** A pooled sink for the format, or a fresh build (outside `sinks`). Null when closed or the build failed. */
    fun takeSink(p: Purpose, rate: Int, ch: Int): SinkHandle? {
        synchronized(this) {
            if (closed) return null
            pool[key(p, rate, ch)]?.removeFirstOrNull()?.let { return it }
        }
        val s = factory.build(p, rate, ch) ?: return null
        val keep = synchronized(this) { !closed }
        if (!keep) { runCatching { factory.release(s) }; return null }
        return s
    }

    /** Back to the pool (stopped, flushed, no focus) — or released when closed, or NAV with no route. */
    fun giveBack(s: SinkHandle) {
        val release = synchronized(this) {
            if (closed || (s.purpose == Purpose.NAV && !navRetain)) true
            else { pool.getOrPut(key(s.purpose, s.rate, s.ch)) { ArrayDeque() }.addLast(s); false }
        }
        if (release) runCatching { factory.release(s) }
    }

    /** V4a: the route ended — free the pooled NAV sinks (an in-use one is freed at its END). */
    fun freeNavPool(): Int {
        val free = ArrayList<SinkHandle>()
        synchronized(this) {
            val it = pool.entries.iterator()
            while (it.hasNext()) {
                val e = it.next()
                if (e.key.startsWith(Purpose.NAV.name + "/")) { free.addAll(e.value); it.remove() }
            }
        }
        free.forEach { runCatching { factory.release(it) } }
        return free.size
    }

    fun pooledCount(): Int = synchronized(this) { pool.values.sumOf { it.size } }

    companion object {
        /** A speech START edge with no ASSISTANT BEGIN behind it pauses media for at most this. */
        const val CTRL_LEAD_MS = 3_000L
        fun hex(v: Long): String = java.lang.Long.toHexString(v)

        /** int16 LE peak over [pcm] above [threshold]. */
        fun peakExceeds(pcm: ByteArray, threshold: Int): Boolean {
            var i = 0
            while (i + 1 < pcm.size) {
                val s = ((pcm[i + 1].toInt() shl 8) or (pcm[i].toInt() and 0xFF)).toShort().toInt()
                if (s > threshold || s < -threshold) return true
                i += 2
            }
            return false
        }
    }
}
