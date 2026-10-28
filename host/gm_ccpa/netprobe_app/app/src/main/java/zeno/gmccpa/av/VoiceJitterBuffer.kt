package zeno.gmccpa.av

/**
 * One voice stream's jitter buffer, keyed on the RTP timestamp (plan v5.8 V1b). Pure: no Android,
 * no I/O, no clock of its own — every time is passed in (µs on the receiver's voice clock).
 *
 * Stock model (Apple R14G17 AirPlayUtils.c, Cinemo r14; docs/carplay/06_AV_PIPELINE.md §10.2):
 *  - **Target** = audioLatencyMs (absent → 80 ms, clamped ≥ 32; [VoiceLatency.targetMs]).
 *  - **Start**: the first packet's rx time + target (:800-803). After a dry buffer it re-buffers
 *    (:1203-1204) and the timer restarts on the next put (:794-799).
 *  - **Discard Excess** trims DOWN TO W = max(target + 20, 200) ms (:657-673), at the
 *    buffering → playing transition (:1063-1068) and at node-pool exhaustion (:755-757; pool =
 *    max(50, 2 × packets(target)), trigger at allocated − 3).
 *  - Late → skip; gap → conceal with silence; dry → re-buffer; while BUFFERING after a dry buffer
 *    the output is silence (:1052-1056, :1199-1206) — `rebuffer_silence` here, apart from
 *    `concealed`.
 *
 * Two departures, both [OWNER-DIR] (real time, never a growing buffer):
 *  - **Re-anchor on the first SURVIVING node** after every Discard Excess. Stock takes the head node
 *    BEFORE the discard (:1058) and anchors on it (:1065), so the survivors read as "too early" and
 *    the discarded span comes out as silence: the delay stays stall + target. Here the playhead jumps
 *    to the first survivor, so a trim cuts the delay to ≤ W and writes no silence for what it cut.
 *  - **Continuous W**: while PLAYING, buffered > W → Discard Excess down to W on every put/pull,
 *    not only at stock's two points.
 *
 * Output is PULLED ([pull]) in frames, the way a device callback reads (RTPJitterBufferRead):
 * room-driven while the worker's track is playing, wall-clock-driven while it is not (a NAV stream
 * between prompt epochs, a muted stream) — so the playhead always advances in real time and stale
 * audio is dropped at its playout time, never queued.
 *
 * Every node ends in exactly ONE bucket: `late`, `excess`, an at-playout fate
 * ([Fate]: played / epoch_idle / suppressed / refused_lost — assigned once, when its first frame is
 * pulled), or a [dropAll] bucket (superseded / retired_unplayed). That is what makes the per-stream
 * accounting identity checkable.
 *
 * Thread-safety: every public method is synchronized on this object — the leaf lock of plan v5.8's
 * order `sinks → jitter-buffer lock` (the `Sink.lock` of the retired VoiceRouter sink is gone). The
 * reader thread [put]s; the purpose worker decodes and [pull]s. Being the leaf means the [pull]
 * callback `fateOf` must take no lock of its own: `PurposeEngine` snapshots `navSuppressed()` before
 * the pull (C-F2, 2026-10-08).
 */
class VoiceJitterBuffer(
    val rate: Int,
    val channels: Int,
    val targetMs: Int,
    auFrames: Int = DEFAULT_AU_FRAMES,
) {
    val wMs: Int = VoiceLatency.watermarkMs(targetMs)
    private val frameBytes = 2 * maxOf(channels, 1)
    private val targetTs: Long = targetMs.toLong() * rate / 1000
    private val wTs: Long = wMs.toLong() * rate / 1000

    /** Frames per AU: learned from consecutive (seq + 1) timestamp deltas; 480 until then. [UNK, U3]. */
    var framesPerPacket: Int = auFrames
        private set
    /** Set when a seq+1 delta disagreed with the decoded frame count — U3 evidence, logged by the caller. */
    var tsUnitMismatch: String? = null
        private set
    val poolNodes: Int = VoiceLatency.poolNodes(targetMs, maxOf(1, auFrames * 1000 / maxOf(rate, 1)))

    enum class State { BUFFERING, PLAYING }
    /** At-playout fates — the four buckets assigned when a node's first frame is pulled. */
    enum class Fate { PLAYED, EPOCH_IDLE, SUPPRESSED, REFUSED_LOST }
    enum class Put { OK, LATE, DUPLICATE }
    enum class Trim { TRANSITION, POOL, CONTINUOUS }
    enum class SilenceKind { CONCEALED, REBUFFER }

    class Node(val ts: Long, val seq: Int, val rxUs: Long, val au: ByteArray) {
        /** Decoded PCM, or null (not decoded yet, or the decode produced nothing). */
        var pcm: ByteArray? = null
        /** Queued to the codec (decode-ahead). */
        var submitted = false
        /** The decode is over and gave no PCM (codec flush or fault): played as silence. */
        var decodeFailed = false
        /** Peak above the loud threshold — set at decode time (the NAV onset is detected there). */
        var loud = false
        /** Frames of this node already pulled. */
        internal var consumed = 0
        /** Fate, assigned when the first frame is pulled. */
        var fate: Fate? = null
            internal set
        /** Voice-clock time of its first pulled frame. */
        var outAtUs = 0L
            internal set
        /** Removed from the buffer: [decoded] / [decodeLost] ignore a node once this is set (a worker
         *  holding a reference finishes its decode after the trim or drop that removed it). */
        var removed = false
            internal set
    }

    /** One output piece: a node's PCM (pcm == null → silence of its length: decode missing) or silence. */
    class Segment(
        val node: Node?, val offsetFrames: Int, val frames: Int, val fate: Fate?, val silence: SilenceKind?,
    )

    var state = State.BUFFERING
        private set
    var everPlayed = false
        private set
    private var startUs = 0L
    private var playheadTs = 0L
    /** End ts of the last node removed after output (or as late/excess) — the late test while BUFFERING. */
    private var lastOutEndTs = -1L
    private val nodes = ArrayList<Node>()
    private var lastPutSeq = -1
    private var lastPutTs = -1L

    // ---- counters (nodes unless "frames") ----
    var late = 0L; private set
    var duplicates = 0L; private set
    var excess = 0L; private set
    var played = 0L; private set
    var epochIdle = 0L; private set
    var suppressed = 0L; private set
    var refusedLost = 0L; private set
    var superseded = 0L; private set
    var retiredUnplayed = 0L; private set
    var concealedFrames = 0L; private set
    var rebufferSilenceFrames = 0L; private set
    var rebuffers = 0L; private set
    var decodeMiss = 0L; private set
    /** Continuous-W trims while PLAYING — the B-V1-2 "W-trigger count" (U2). */
    var wTriggers = 0L; private set
    var transitionTrims = 0L; private set
    var poolTriggers = 0L; private set
    var excessFramesLastTrim = 0L; private set
    var putCount = 0L; private set

    private fun d32(a: Long, b: Long): Long = (a - b).toInt().toLong()   // signed mod-2^32 distance a - b
    private fun add32(a: Long, n: Long): Long = (a + n) and 0xFFFFFFFFL
    /**
     * A node's length on the timeline is the RTP timestamp step, NOT its PCM length: the timeline is
     * kept in RTP ticks, assumed equal to output frames ([UNK, U3]; V1a measures it). A decode whose
     * frame count differs is reported once ([tsUnitMismatch]) and its PCM is cut or padded to the step
     * by the writer, so the timeline never drifts.
     */
    @Suppress("UNUSED_PARAMETER")
    private fun nodeFrames(n: Node): Int = framesPerPacket
    private fun endTs(n: Node): Long = add32(n.ts, nodeFrames(n).toLong())

    /** Frames from the playhead (PLAYING) or the first node (BUFFERING) to the end of the last node. */
    @Synchronized fun bufferedFrames(): Long {
        if (nodes.isEmpty()) return 0
        val first = nodes[0]
        val start = if (state == State.PLAYING) {
            val s = add32(first.ts, first.consumed.toLong())
            if (d32(playheadTs, s) > 0) playheadTs else s
        } else first.ts
        return maxOf(0L, d32(endTs(nodes[nodes.size - 1]), start))
    }

    @Synchronized fun bufferedMs(): Long = bufferedFrames() * 1000 / maxOf(rate, 1)
    @Synchronized fun size(): Int = nodes.size
    @Synchronized fun isEmpty(): Boolean = nodes.isEmpty()

    /**
     * Insert one AU (reader thread). [nowUs] is the read time, used for the start timer only when the
     * AU carries no rx stamp.
     */
    @Synchronized fun put(n: Node, nowUs: Long): Put {
        putCount++
        // U3: learn frames-per-packet from a seq+1 neighbour.
        if (lastPutSeq >= 0 && ((n.seq - lastPutSeq) and 0xFFFF) == 1) {
            val dts = d32(n.ts, lastPutTs)
            if (dts in 64..8192 && dts.toInt() != framesPerPacket) framesPerPacket = dts.toInt()
        }
        lastPutSeq = n.seq; lastPutTs = n.ts
        // Late: before the playhead (PLAYING), or before what was already output (BUFFERING).
        val endN = add32(n.ts, framesPerPacket.toLong())
        if (state == State.PLAYING && d32(endN, playheadTs) <= 0) { late++; return Put.LATE }
        if (lastOutEndTs >= 0 && d32(endN, lastOutEndTs) <= 0) { late++; return Put.LATE }
        // Stock: Discard Excess at node-pool exhaustion, BEFORE the insert (AirPlayUtils.c:755-768).
        if (trimHold) {
            // Device start stall (fix2): no trim on the pool count while the HAL bus wakes; the
            // start cap (hardWatermark) bounds the buffer instead.
        } else if (nodes.size >= poolNodes - VoiceLatency.POOL_TRIGGER_SLACK) {
            poolTriggers++
            discardExcess(Trim.POOL)
        }
        // Timestamp order, scanning from the end (the usual case is a later timestamp).
        var i = nodes.size
        while (i > 0) {
            val dd = d32(nodes[i - 1].ts, n.ts)
            if (dd == 0L) { duplicates++; late++; return Put.DUPLICATE }
            if (dd < 0) break
            i--
        }
        nodes.add(i, n)
        if (state == State.BUFFERING && startUs == 0L) {
            startUs = (if (n.rxUs >= 0) n.rxUs else nowUs) + targetMs * 1000L
        }
        if (state == State.PLAYING && bufferedFrames() > hardWatermark()) {
            wTriggers++
            discardExcess(Trim.CONTINUOUS, hardWatermark())
        }
        return Put.OK
    }

    /**
     * Discard Excess DOWN TO W, oldest first, then re-anchor on the first SURVIVING node
     * ([OWNER-DIR]; stock re-anchors on the pre-discard head).
     */
    private fun discardExcess(why: Trim, downTo: Long = wTs) {
        var cut = 0L
        while (nodes.isNotEmpty() && bufferedFrames() > downTo) {
            val n = nodes.removeAt(0)
            n.removed = true
            cut += nodeFrames(n) - n.consumed
            if (n.fate == null) { excess++; if (n.loud && n.pcm != null) excessLoud++ }
            lastOutEndTs = endTs(n)
        }
        if (cut > 0) {
            excessFramesLastTrim = cut
            if (why == Trim.TRANSITION) transitionTrims++
        }
        if (state == State.PLAYING && nodes.isNotEmpty()) {
            val f = nodes[0]
            val s = add32(f.ts, f.consumed.toLong())
            if (d32(s, playheadTs) > 0) playheadTs = s
        }
    }

    // ---- fix2 (rc1 review, owner real-time direction): the device start stall ----------------------
    // After play() the HAL bus may take 140-400 ms to wake (cp12: first AU → `busActivated` 139-408 ms,
    // the early excess tracking it monotonically, n=7; 0 track underruns on rc1). Nothing is consumed
    // then, so the buffer grows by the stall and the continuous W trim used to cut the FIRST WORDS. Now:
    //  - while the device has not started ([holdTrims]), no Discard Excess below the hard start cap
    //    (W + [VoiceLatency.START_STALL_CAP_MS]) — the buffer stays bounded;
    //  - at the first head advance ([deviceStarted]) what is buffered above W becomes an ALLOWANCE (the
    //    hard watermark is W + allowance): the first words are delayed by the stall, not cut;
    //  - the allowance is paid back by dropping only NON-LOUD decoded AUs at the playhead
    //    ([silenceCatchUp]), so the delay returns to ≤ W in the first pause without cutting speech.
    private var trimHold = false
    private var allowanceTs = 0L
    private val startCapTs: Long = VoiceLatency.START_STALL_CAP_MS.toLong() * rate / 1000
    /** Loud decoded AUs that a trim discarded — audible skips (B-V1-W). */
    var excessLoud = 0L; private set
    /** Silent AUs dropped to pay the start allowance back. Counted in [excess] too. */
    var excessSilentCatchUp = 0L; private set

    private fun hardWatermark(): Long = if (trimHold) wTs + startCapTs else wTs + allowanceTs

    /** The device has been told to play and its head has not moved yet: hold the trims (bounded). */
    @Synchronized fun holdTrims(hold: Boolean) { trimHold = hold }

    /** The head advanced for the first time: what is buffered above W becomes the allowance. Returns it, frames. */
    @Synchronized fun deviceStarted(slackFrames: Long = 0): Long {
        trimHold = false
        // + slack (one device buffer + one AU): the device pulls in device-buffer bursts, so the level
        // swings by that much around the carried delay; without it every swing would trim a loud AU.
        val over = bufferedFrames() - wTs
        allowanceTs = if (over > 0) minOf(startCapTs, over + slackFrames) else 0L
        return allowanceTs
    }

    @Synchronized fun allowanceMs(): Long = allowanceTs * 1000 / maxOf(rate, 1)

    /** Pay the allowance back with silence only: drop the playhead's next AUs while they are decoded and not loud. */
    private fun silenceCatchUp() {
        // The whole allowance (the carried stall + the swing slack) is paid back, so the level ends below W
        // by a device-buffer swing; never below the target.
        while (allowanceTs > 0 && nodes.isNotEmpty() && bufferedFrames() > targetTs) {
            val f = nodes[0]
            if (f.consumed != 0 || f.pcm == null || f.loud) break
            nodes.removeAt(0); f.removed = true
            if (f.fate == null) { excess++; excessSilentCatchUp++; if (f.loud) excessLoud++ }
            lastOutEndTs = endTs(f)
            allowanceTs = maxOf(0L, allowanceTs - nodeFrames(f))      // paid back only by what was dropped
            if (state == State.PLAYING && nodes.isNotEmpty()) {
                val s = add32(nodes[0].ts, nodes[0].consumed.toLong())
                if (d32(s, playheadTs) > 0) playheadTs = s
            }
        }
        // A buffer that drained to the target by itself (the sender paused) owes nothing any more.
        if (bufferedFrames() <= targetTs) allowanceTs = 0L
    }

    /** Output may start: PLAYING, or BUFFERING with its start time reached (or after a first start). */
    @Synchronized fun dueToPlay(nowUs: Long): Boolean =
        state == State.PLAYING || everPlayed || (startUs != 0L && nowUs >= startUs && nodes.isNotEmpty())

    /** Voice-clock time the BUFFERING state ends (0 = not scheduled); for the worker's wait. */
    @Synchronized fun startAtUs(): Long = if (state == State.BUFFERING) startUs else 0L

    /**
     * Nodes due for decode-ahead: in order, not yet submitted, at most [targetMs] ahead of the
     * playhead (PLAYING) or of the first node (BUFFERING). The worker decodes them OUTSIDE this lock
     * and reports with [decoded] / [decodeLost].
     */
    @Synchronized fun decodeCandidates(horizonFrames: Long = targetTs): List<Node> {
        if (nodes.isEmpty()) return emptyList()
        val ref = if (state == State.PLAYING) playheadTs else nodes[0].ts
        val out = ArrayList<Node>()
        for (n in nodes) {
            if (d32(n.ts, ref) > maxOf(targetTs, horizonFrames)) break
            if (!n.submitted) out.add(n)
        }
        return out
    }

    /**
     * Review round 1: a stream that waited for its predecessor's END play-out starts as if it had
     * started on time — what is older than [targetMs] from the newest audio is dropped as `late`
     * (its due time passed while it waited), so it does not run at up to W for its whole life.
     * Returns the frames cut.
     */
    @Synchronized fun catchUpToTarget(): Long {
        var cut = 0L
        while (nodes.size > 1 && bufferedFrames() > targetTs) {
            val n = nodes.removeAt(0)
            n.removed = true
            cut += nodeFrames(n) - n.consumed
            if (n.fate == null) late++
            lastOutEndTs = endTs(n)
        }
        if (state == State.PLAYING && nodes.isNotEmpty()) {
            val f = nodes[0]
            val s = add32(f.ts, f.consumed.toLong())
            if (d32(s, playheadTs) > 0) playheadTs = s
        }
        return cut
    }

    /** The decode of [n] produced [pcm] (null = nothing); [loud] is its peak test. */
    @Synchronized fun decoded(n: Node, pcm: ByteArray?, loud: Boolean) {
        if (n.removed) return
        n.pcm = pcm
        n.loud = loud
        if (pcm == null) n.decodeFailed = true
        val f = if (pcm != null) pcm.size / frameBytes else 0
        if (f > 0 && f != framesPerPacket && tsUnitMismatch == null) {
            tsUnitMismatch = "decoded $f frames per AU, the RTP timestamp step says $framesPerPacket"
        }
    }

    /** A submitted node's decode was lost (codec flushed before it came out). */
    @Synchronized fun decodeLost(n: Node) { if (!n.removed && n.pcm == null) n.decodeFailed = true }

    /**
     * Pull [frames] of output at [nowUs]. [fateOf] is called once per node, when its first frame is
     * pulled (its playout time), and decides the node's at-playout bucket. Returns the segments in
     * order; their frames sum to [frames], except before the first start, when nothing is output.
     */
    @Synchronized fun pull(nowUs: Long, frames: Int, fateOf: (Node) -> Fate): List<Segment> {
        val out = ArrayList<Segment>()
        if (frames <= 0) return out
        if (state == State.BUFFERING) {
            if (startUs == 0L || nowUs < startUs || nodes.isEmpty()) {
                if (everPlayed) {
                    rebufferSilenceFrames += frames
                    out.add(Segment(null, 0, frames, null, SilenceKind.REBUFFER))
                }
                return out
            }
            // Buffering → playing: Discard Excess down to W, then anchor on the first SURVIVOR.
            state = State.PLAYING
            playheadTs = nodes[0].ts
            discardExcess(Trim.TRANSITION, hardWatermark())   // fix2: W + the start allowance (W when none)
            if (nodes.isNotEmpty()) playheadTs = add32(nodes[0].ts, nodes[0].consumed.toLong())
            everPlayed = true
        }
        val lim = add32(playheadTs, frames.toLong())
        while (d32(lim, playheadTs) > 0) {
            if (nodes.isEmpty()) break
            val n = nodes[0]
            val nFrames = nodeFrames(n)
            val nStart = add32(n.ts, n.consumed.toLong())
            val nEnd = add32(n.ts, nFrames.toLong())
            if (d32(nStart, lim) >= 0) {
                // Too early: a hole before the next node — conceal the rest of the window.
                val gap = d32(lim, playheadTs)
                concealedFrames += gap
                out.add(Segment(null, 0, gap.toInt(), null, SilenceKind.CONCEALED))
                playheadTs = lim
                break
            }
            if (d32(nEnd, playheadTs) <= 0) {
                // Entirely before the playhead: late.
                nodes.removeAt(0); n.removed = true
                if (n.fate == null) late++
                lastOutEndTs = nEnd
                continue
            }
            if (d32(nStart, playheadTs) < 0) {
                n.consumed += d32(playheadTs, nStart).toInt()   // skip the late part
            } else if (d32(nStart, playheadTs) > 0) {
                val gap = d32(nStart, playheadTs)
                concealedFrames += gap
                out.add(Segment(null, 0, gap.toInt(), null, SilenceKind.CONCEALED))
                playheadTs = nStart
            }
            if (n.fate == null) {
                val f = fateOf(n)
                n.fate = f; n.outAtUs = nowUs
                when (f) {
                    // Review round 1: a node due to play with no PCM yet is written as silence; it is its
                    // own bucket, decode_miss, so `played` counts only audio that was really written.
                    Fate.PLAYED -> if (n.pcm == null) decodeMiss++ else played++
                    Fate.EPOCH_IDLE -> epochIdle++
                    Fate.SUPPRESSED -> suppressed++
                    Fate.REFUSED_LOST -> refusedLost++
                }
            }
            val take = minOf(d32(nEnd, playheadTs), d32(lim, playheadTs)).toInt()
            out.add(Segment(n, n.consumed, take, n.fate, null))
            n.consumed += take
            playheadTs = add32(playheadTs, take.toLong())
            if (n.consumed >= nFrames) {
                nodes.removeAt(0); n.removed = true
                lastOutEndTs = nEnd
            }
        }
        if (d32(lim, playheadTs) > 0) {
            // Dry: re-buffer (stock :1199-1206). Silence for the rest, BUFFERING until the next put.
            val miss = d32(lim, playheadTs)
            rebufferSilenceFrames += miss
            out.add(Segment(null, 0, miss.toInt(), null, SilenceKind.REBUFFER))
            playheadTs = lim
            state = State.BUFFERING
            startUs = 0L
            rebuffers++
        } else if (bufferedFrames() > hardWatermark()) {
            wTriggers++
            discardExcess(Trim.CONTINUOUS, hardWatermark())
        } else if (allowanceTs > 0) {
            silenceCatchUp()
        }
        return out
    }

    /** Every node still in the buffer goes to [bucket] (superseded / retired_unplayed). Returns the count. */
    @Synchronized fun dropAll(bucket: Drop): Int {
        var n = 0
        for (x in nodes) {
            x.removed = true
            if (x.fate == null) {
                n++
                when (bucket) {
                    Drop.SUPERSEDED -> superseded++
                    Drop.RETIRED_UNPLAYED -> retiredUnplayed++
                }
            }
        }
        nodes.clear()
        return n
    }

    enum class Drop { SUPERSEDED, RETIRED_UNPLAYED }

    /** Sum of every node bucket — what the stream's identity checks against AUs read. */
    @Synchronized fun bucketSum(): Long =
        late + excess + played + decodeMiss + epochIdle + suppressed + refusedLost + superseded + retiredUnplayed

    /** Delay of the newest queued audio relative to the playhead, ms (PLAYING), for the bounds. */
    @Synchronized fun delayMs(): Long = bufferedMs()

    @Synchronized fun summary(): String =
        "played $played, decode_miss $decodeMiss, late $late (dup $duplicates), excess $excess, epoch_idle $epochIdle, suppressed $suppressed, " +
        "refused_lost $refusedLost, superseded $superseded, retired_unplayed $retiredUnplayed; " +
        "concealed ${concealedFrames * 1000 / maxOf(rate, 1)}ms, rebuffer_silence ${rebufferSilenceFrames * 1000 / maxOf(rate, 1)}ms " +
        "($rebuffers re-buffers), W-triggers $wTriggers, transition trims $transitionTrims, pool triggers $poolTriggers, " +
        "excess loud $excessLoud, silent catch-up $excessSilentCatchUp, " +
        "target ${targetMs}ms W ${wMs}ms, ${framesPerPacket} frames/AU"

    companion object {
        /** AAC-ELD frameLength 480 (the device-confirmed ASC): 30 ms at 16 kHz, 10 ms at 48 kHz. */
        const val DEFAULT_AU_FRAMES = 480
    }
}
