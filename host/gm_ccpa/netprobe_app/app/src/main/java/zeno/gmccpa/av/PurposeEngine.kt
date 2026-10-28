package zeno.gmccpa.av

import zeno.gmccpa.av.VoiceLatency.Purpose

/**
 * One purpose's worker logic. Called ONLY on that purpose's worker thread ([onEvent], [service]);
 * every field here is that thread's.
 */
class PurposeEngine(val purpose: Purpose, private val core: VoiceStreams) {
    private val log = core.log
    private val clock = core.clock
    private val deviceBufferMs = VoiceLatency.deviceBufferMs(purpose)

    var current: VoiceStream? = null
        private set
    /** A BEGIN waiting for the current stream's END play-out (it does not cut the last word). */
    private var pendingBegin: VoiceStream? = null
    var sink: SinkHandle? = null
        private set
    private var trackPlaying = false
    private var written = 0L
    private var headBase = 0L
    private var wallMode = true
    private var lastWallPullUs = 0L
    private var expectDecodeTs = -1L
    private val inflight = ArrayList<VoiceJitterBuffer.Node>()
    /** FAILED focus request: muted for the stream's life (stock). */
    private var mutedForLife = false
    /** A focus LOSS / LOSS_TRANSIENT on the held slot: muted until GAIN. */
    private var lossMuted = false
    private var buildFailedLogged = false
    private var endPhase = 0          // 0 none, 1 play-out, 2 head drain
    private var drainDeadlineUs = 0L
    private var playoutDeadlineUs = 0L
    /** fix2: play() was called and the head has not moved yet (0 = not waiting). */
    private var awaitingHeadSinceUs = 0L
    /** The stream's first device start stall (play() → first head advance), ms; -1 = none yet. */
    private var deviceStartStallMs = -1L
    private var headEverMoved = false
    /** NAV: when the software duck was last lifted for an active bus1 (0 = never this stream). */
    private var liftedAt = 0L
    /** Head frames consumed by earlier track runs of this stream (before a pause/flush reset the base). */
    private var headTotal = 0L

    private fun accumulateHead(sk: SinkHandle) {
        headTotal += maxOf(0L, runCatching { sk.track.headFrames() }.getOrDefault(headBase) - headBase)
    }
    /** The track's underrun counter when this stream took the sink (the counter is per track, not per stream). */
    private var underrunBase = 0
    var retired = false
        private set
    /** Bounds instrumentation: played AUs whose rx→write exceeded W + one AU. */
    var boundExceeded = 0L
        private set
    var writes = 0L
        private set

    // ---- NAV (owner 2026-10-07: OD-8C YES, OD-8D PLAY_LIVE — stock) ----
    // NAV requests no audio focus (SD:CarPlayAudioManager.java:748-750; T1/T3: AltAudio unmuted with
    // no CarAudioFocus request) and its stream plays live from BEGIN to END, never late. No prompt
    // epochs, no park/re-acquire, no Siri guard.
    /** V3: iOS has the media ducked (duckAudio without its unduckAudio yet). */
    var ducked = false
        private set
    /** The scid a lost unduck is restored at (the live 101 at duck time, or the first NAV BEGIN after it). */
    var duckBoundScid: Long? = null
        private set
    private var duckPendingBind = false
    /**
     * unduckAudio arrived while its NAV stream was still live: media is HELD at
     * [VoiceLatency.NAV_HOLD_LEVEL] until that stream ENDs, then restored — stock T1 (Cinemo ramps its
     * sink to 1.0 but the GM platform duck, -13.6 dB, holds until AltAudio TEARDOWN; full volume
     * +1.87-1.90 s after the unduck, n=3).
     */
    var holdScid: Long? = null
        private set
    private var holdDurMs = DuckMath.DEFAULT_DURATION_MS
    /** DROP (non-default) cut requested by a playout-time fate; applied after the pull. */
    private var cutPending = false
    /** Loud decoded AUs of the current stream (stats only). */
    private var loudAus = 0L

    private val periodFrames: Int get() = (current?.rate ?: 16000) * VoiceLatency.PERIOD_MS / 1000

    // ---- events -----------------------------------------------------------------------------------

    fun onEvent(e: VoiceEvent) {
        val now = clock.nowUs()
        when (e) {
            is VoiceEvent.Begin -> onBegin(e, now)
            is VoiceEvent.End -> onEnd(e, now)
            is VoiceEvent.LifecycleEnd -> finishWithoutPlayout(e.s, e.why, e.stall, now)
            is VoiceEvent.Hold -> onHold(e.s)
            is VoiceEvent.Focus -> onFocus(e.change)
            is VoiceEvent.PhoneIdle -> onPhoneIdle()
            is VoiceEvent.Duck -> onDuck(now)
            is VoiceEvent.Unduck -> onUnduck(e.durationMs, e.stampUs)
            is VoiceEvent.Report -> report()
            is VoiceEvent.Retire -> retire()
        }
    }

    private fun onBegin(e: VoiceEvent.Begin, now: Long) {
        if (retired) return
        val s = e.s
        if (!core.eventCurrent(s, e.gen)) {
            log.w("${purpose.label}: BEGIN of ${s.label} read on connection ${e.gen}, but the stream is now on ${s.gen} — skipped")
            return
        }
        if (e.resume) {
            if (current === s) {
                log.i("${purpose.label}: ${s.label} resumed — output back on")
            }
            return
        }
        val old = current
        if (old != null && old !== s && old.state == VoiceStream.State.ENDING) {
            // The old stream's END was read and it is playing out its last ≤ W: let it finish (its last
            // word is real audio), and start this one right after. This stream's buffer fills
            // meanwhile and its transition trims it to W, so the delay stays bounded.
            pendingBegin?.let { p -> if (p !== s) dropPending(p, s) }
            pendingBegin = s
            log.i("${purpose.label}: ${s.label} waits for ${old.label} to finish its play-out")
            return
        }
        if (pendingBegin === s) pendingBegin = null
        if (old != null && old !== s) supersede(old, s)
        current = s
        core.armIfPending(s)
        headTotal = 0; deviceStartStallMs = -1; headEverMoved = false; liftedAt = 0; awaitingHeadSinceUs = 0
        expectDecodeTs = -1L
        inflight.clear()
        // Sink: pooled or built (a format change goes back to the pool and takes the other format).
        val have = sink
        if (have != null && (have.rate != s.rate || have.ch != s.ch)) { stopFlush(have); core.giveBack(have); sink = null }
        if (sink == null) sink = core.takeSink(purpose, s.rate, s.ch)
        underrunBase = sink?.track?.underruns()?.coerceAtLeast(0) ?: 0
        resetTrackCounters()
        if (sink == null && !buildFailedLogged) {
            buildFailedLogged = true
            log.e("${purpose.label}: no sink for ${s.label} (build failed) — its audio is discarded in real time (counted refused_lost)")
        }
        wallMode = true; lastWallPullUs = now
        mutedForLife = false
        if (purpose == Purpose.NAV) {
            onNavBegin(s)
        } else {
            beginFocus(s)
        }
    }

    private fun beginFocus(s: VoiceStream) {
        val slot = core.slots.getValue(purpose)
        if (purpose == Purpose.ASSISTANT) {
            // Stock :751-756: Siri abandons a held but non-streaming telephony focus first.
            val call = core.slots.getValue(Purpose.CALL)
            if (call.held && core.liveOf(Purpose.CALL) == null && call.release("ASSISTANT BEGIN: idle CALL focus abandoned (stock)")) {
                log.i("call: focus released — ASSISTANT BEGIN with no live CALL stream (stock :751-756)")
            }
        }
        when (slot.requestIfNotHeld()) {
            FocusSlot.Outcome.GRANTED -> {
                lossMuted = false
                log.i("${purpose.label}: focus GRANTED at BEGIN of ${s.label}")
                core.board.state("voice-sink/${purpose.label}", "focus GRANTED (${s.label})")
                handover()
            }
            FocusSlot.Outcome.ALREADY_HELD -> {
                lossMuted = FocusCodes.mutes(slot.state)
                log.i("${purpose.label}: focus already held at BEGIN of ${s.label} (no request; ${FocusCodes.name(slot.state)})")
                if (!lossMuted) handover()
            }
            FocusSlot.Outcome.FAILED -> {
                mutedForLife = true
                log.w("${purpose.label}: focus REFUSED at BEGIN of ${s.label} — muted for its life (stock; counted refused_lost)")
                core.board.state("voice-sink/${purpose.label}", "REFUSED (${s.label})")
            }
        }
    }

    /** CALL BEGIN with focus: release a Siri slot handed over by a Siri-placed call. */
    private fun handover() {
        if (purpose != Purpose.CALL) return
        val a = core.slots.getValue(Purpose.ASSISTANT)
        if (a.handoverPending && core.liveOf(Purpose.ASSISTANT) == null && a.release("handed over to CALL")) {
            log.i("siri: focus released — handed over to the CALL BEGIN (Siri-placed call; no media GAIN in between)")
        }
    }

    private fun supersede(old: VoiceStream, by: VoiceStream) {
        val n = old.jb.dropAll(VoiceJitterBuffer.Drop.SUPERSEDED)
        core.markSuperseded(old)
        sink?.let { pauseFlush(it); runCatching { it.codec.flush() } }
        inflight.clear(); expectDecodeTs = -1L
        endPhase = 0
        log.i("${purpose.label}: ${old.label} superseded by ${by.label} — $n buffered AUs → superseded; its END is ignored")
        if (purpose == Purpose.NAV) {
            if (duckBoundScid == old.scid) duckBoundScid = by.scid
            if (holdScid == old.scid) holdScid = by.scid
        }
        old.endFrame?.let { old.identityLogged = true; log.i("${purpose.label}: ${old.label}: ${old.identityLine(it)}") }
    }

    private fun onEnd(e: VoiceEvent.End, now: Long) {
        val s = e.s
        if (s === pendingBegin) {
            // It ended before the stream ahead of it finished: start it now (cutting nothing that is
            // still playing is no longer possible), then play it out.
            current?.let { if (it !== s) supersede(it, s) }
            current = null
            pendingBegin = null
            onBegin(VoiceEvent.Begin(s, false, s.gen), now)
        }
        if (current !== s) {
            // Already superseded or finished: nothing to play out.
            s.endFrame?.let { if (!s.identityLogged) { s.identityLogged = true
                log.i("${purpose.label}: END ${s.label} for a stream no longer current — ${s.identityLine(it)}") } }
            core.finished(s)
            return
        }
        if (!core.eventCurrent(s, e.gen)) {
            log.w("${purpose.label}: END of ${s.label} read on connection ${e.gen}, but the stream is now on ${s.gen} — skipped")
            return
        }
        s.state = VoiceStream.State.ENDING
        endPhase = 1
        // Bound the play-out too (review round 1): what is buffered, plus any start wait, plus the device
        // buffer and slack. A HAL that stopped consuming must not hold the END (and its focus) forever.
        val startWait = maxOf(0L, s.jb.startAtUs() - now)
        playoutDeadlineUs = now + startWait + (s.jb.bufferedMs() + deviceBufferMs + 2 * VoiceLatency.HEAD_DRAIN_SLACK_MS) * 1000L
        val end = s.endFrame
        log.i("${purpose.label}: END ${s.label} reason=${end?.let { VoiceFrame.reasonName(it.reason) }} — playing out " +
              "${s.jb.bufferedMs()}ms (rust: ${end?.frames} frames, ${end?.drops} drops, ${end?.seqGaps} seq gaps, " +
              "max inter-arrival ${end?.maxInterArrivalMs}ms)")
    }

    private fun onHold(s: VoiceStream) {
        if (current !== s) return
        sink?.let { pauseFlush(it) }
        wallMode = true
        log.i("${purpose.label}: ${s.label} on hold — track paused + flushed, jitter buffer kept")
    }

    private fun onFocus(change: Int) {
        if (purpose == Purpose.NAV) { log.w("nav: focus ${FocusCodes.name(change)} — NAV holds no focus; ignored"); return }
        val s = current
        when {
            FocusCodes.mutes(change) -> {
                lossMuted = true
                if (s != null) { sink?.let { pauseFlush(it) }; wallMode = true }
                log.w("${purpose.label}: focus ${FocusCodes.name(change)} — ${s?.label ?: "no live stream"} muted (refused_lost)")
            }
            change == FocusCodes.GAIN -> {
                if (lossMuted) log.i("${purpose.label}: focus GAIN — ${if (s != null) "${s.label} output back on" else "no live stream: nothing resumed"}")
                lossMuted = false
            }
        }
    }

    private fun onPhoneIdle() {
        if (purpose == Purpose.NAV) return
        val slot = core.slots.getValue(purpose)
        // A stream still current (live, or playing out its END) applies the stop rule at its finish,
        // with ctrlCall false by then — never cut its tail's focus here.
        if (current != null) return
        if (purpose == Purpose.ASSISTANT) {
            if (slot.handoverPending && slot.release("phone 1→0")) log.i("siri: focus released (phone 1→0; the handed-over slot)")
            return
        }
        if (slot.held && slot.release("phone 1→0")) log.i("${purpose.label}: focus released (phone 1→0)")
    }

    // ---- service ----------------------------------------------------------------------------------

    /** One worker pass. Returns the next wake time (µs), or Long.MAX_VALUE when idle. */
    fun service(): Long {
        val now = clock.nowUs()
        if (retired) return Long.MAX_VALUE
        if (purpose == Purpose.NAV) navTimers(now)
        val s = current ?: return Long.MAX_VALUE
        if (s.state == VoiceStream.State.HOLD) return now + 50_000
        decodeAhead(s)
        output(s, now)
        if (s.state == VoiceStream.State.ENDING) endService(s, now)
        if (purpose == Purpose.NAV && cutPending) dropCut(s, now)
        if (current == null) return Long.MAX_VALUE
        val start = s.jb.startAtUs()
        val period = now + VoiceLatency.PERIOD_MS * 1000L
        return if (start > now) minOf(start, period) else period
    }

    private fun decodeAhead(s: VoiceStream) {
        val sk = sink ?: return
        // Review round 1: decode at least one device buffer + one AU ahead — a room-driven pull can take a
        // whole device buffer (128 ms for ASSISTANT) at once, longer than the 80 ms target.
        val horizon = maxOf(s.targetMs.toLong(), deviceBufferMs.toLong()) * s.rate / 1000 + s.jb.framesPerPacket
        for (n in s.jb.decodeCandidates(horizon)) {
            if (expectDecodeTs >= 0 && n.ts != expectDecodeTs) {
                // A discontinuity in the decode input (skipped, discarded or missing AUs): flush first.
                runCatching { sk.codec.flush() }
                inflight.forEach { s.jb.decodeLost(it) }
                inflight.clear()
            }
            if (!runCatching { sk.codec.queue(n.au, n.ts) }.getOrDefault(false)) break
            n.submitted = true
            inflight.add(n)
            expectDecodeTs = (n.ts + s.jb.framesPerPacket) and 0xFFFFFFFFL
        }
        if (inflight.isEmpty()) return
        val outs = runCatching { sk.codec.drain(2_000) }.getOrDefault(emptyList())
        for (d in outs) {
            // FIFO: one output per queued AU, in order (AAC). The presentation time is only checked —
            // a decoder may adjust it — never used to match, or a pts rewrite would silence the sink.
            if (inflight.isEmpty()) break
            val n = inflight.removeAt(0)
            if (d.key != n.ts) {
                ptsMismatch++
                if (ptsMismatch == 1L) log.w("${s.label}: decoder output pts ${d.key} ≠ queued ${n.ts} — matched in order (FIFO)")
            }
            val loud = VoiceStreams.peakExceeds(d.pcm, VoiceLatency.LOUD_PEAK)
            s.jb.decoded(n, d.pcm, loud)
            if (loud) loudAus++
        }
        s.jb.tsUnitMismatch?.let { if (!tsMismatchLogged) { tsMismatchLogged = true; log.e("${s.label}: $it (U3: RTP units vs output frames) — the timeline keeps the RTP step") } }
    }
    private var tsMismatchLogged = false
    private var ptsMismatch = 0L

    private fun outputOn(s: VoiceStream): Boolean {
        if (sink == null) return false
        if (purpose == Purpose.NAV) return !s.latchedSuppressed && !s.cutSuppressed
        return !mutedForLife && !lossMuted
    }

    /**
     * Runs INSIDE `VoiceJitterBuffer.pull`, i.e. under the jitter-buffer monitor (the leaf lock), so
     * it must take no other lock: [navSuppressedNow] is the caller's snapshot of
     * `core.navSuppressed()` — which takes `sinks` — made once per pull pass BEFORE the pull (C-F2,
     * 2026-10-08; it used to be called per node from in here, inverting the documented order).
     */
    private fun fateOf(s: VoiceStream, navSuppressedNow: Boolean): (VoiceJitterBuffer.Node) -> VoiceJitterBuffer.Fate = { _ ->
        if (purpose != Purpose.NAV) {
            when {
                sink == null || mutedForLife || lossMuted -> VoiceJitterBuffer.Fate.REFUSED_LOST
                else -> VoiceJitterBuffer.Fate.PLAYED
            }
        } else {
            when {
                s.latchedSuppressed || s.cutSuppressed -> VoiceJitterBuffer.Fate.SUPPRESSED
                navSuppressedNow -> { cutPending = true; s.cutSuppressed = true; VoiceJitterBuffer.Fate.SUPPRESSED }
                sink == null -> VoiceJitterBuffer.Fate.REFUSED_LOST
                else -> VoiceJitterBuffer.Fate.PLAYED
            }
        }
    }

    private fun output(s: VoiceStream, now: Long) {
        if (endPhase == 2) return            // head drain: nothing more is pulled or written
        val sk = sink
        val on = outputOn(s)
        if (on && sk != null) {
            if (!trackPlaying) {
                // Track start, only once the stream's buffer is due to play (first RX + target).
                // The device buffer is PRIMED with silence first, so the device then pulls the
                // jitter buffer at its own pace (8 ms periods), as stock's audio unit does — end to
                // end = target + device buffer. Without the prime, the first room-driven pull would
                // take a whole device buffer (up to 128 ms) out of an 80 ms jitter buffer and force a
                // re-buffer right after the first word.
                if (!s.jb.dueToPlay(now)) return
                prime(s, sk)
                // fix2: until the head first moves (HAL bus wake), the stall is not charged against W.
                awaitingHeadSinceUs = now
                s.jb.holdTrims(true)
            }
            if (wallMode) { wallMode = false }
            val head = runCatching { sk.track.headFrames() }.getOrDefault(headBase) - headBase
            if (awaitingHeadSinceUs > 0) deviceStartCheck(s, head, now)
            val queued = written - head
            val room = sk.track.bufferFrames - queued
            if (room <= 0) return
            // Snapshot OUTSIDE the jitter-buffer lock: see [fateOf].
            val ns = purpose == Purpose.NAV && core.navSuppressed()
            val segs = s.jb.pull(now, room.toInt(), fateOf(s, ns))
            writeSegments(s, sk, segs, now)
        } else {
            if (!wallMode) { wallMode = true; lastWallPullUs = now }
            if (lastWallPullUs == 0L) lastWallPullUs = now
            val frames = (now - lastWallPullUs) * s.rate / 1_000_000
            if (frames <= 0) return
            val ns = purpose == Purpose.NAV && core.navSuppressed()
            s.jb.pull(now, frames.toInt(), fateOf(s, ns))
            lastWallPullUs += frames * 1_000_000 / s.rate
        }
    }

    /**
     * fix2: the device start. The first head advance after play() ends the start stall: the stall is
     * recorded (B-V1-W: the busActivated proxy), what is buffered above W becomes the jitter buffer's
     * allowance (delayed, not cut), and — NAV, owner 2026-10-08 — the software duck lifts because the GM
     * platform duck now carries the prompt. A head that does not move within the start cap releases the
     * hold anyway (the buffer stays bounded).
     */
    private fun deviceStartCheck(s: VoiceStream, head: Long, now: Long) {
        val waited = now - awaitingHeadSinceUs
        if (head <= 0 && waited < VoiceLatency.START_STALL_CAP_MS * 1000L) return
        awaitingHeadSinceUs = 0
        val slack = (sink?.track?.bufferFrames ?: 0).toLong() + s.jb.framesPerPacket
        val allowance = s.jb.deviceStarted(slack) * 1000 / maxOf(s.rate, 1)
        headEverMoved = headEverMoved || head > 0
        if (deviceStartStallMs < 0) deviceStartStallMs = waited / 1000
        if (head > 0) {
            log.i("${purpose.label}: ${s.label} device started ${waited / 1000}ms after play() (HAL bus wake) — " +
                  "${allowance}ms carried as delay, paid back from silence")
        } else {
            log.w("${purpose.label}: ${s.label} head did not move within ${VoiceLatency.START_STALL_CAP_MS}ms of play() — start hold released")
        }
        if (purpose == Purpose.NAV && head > 0) liftForBus(now)
    }

    /** NAV: the platform duck carries the prompt once bus1 is active (owner 2026-10-08): lift ours. */
    private fun liftForBus(now: Long) {
        if (!core.navDuckLift()) return
        liftedAt = now
        log.i("nav: duck lifted — the nav track is playing (bus1 active; the GM platform duck carries the prompt, " +
              "owner 2026-10-08). Bench B-V3-3: VAC/gaincontrol `ducking` ≈ -14..-16 dB with our shaper at 1.0")
        core.duck.ramp(1f, VoiceLatency.NAV_DUCK_LIFT_RAMP_MS, "lift: nav track playing (platform duck active)", now)
    }

    /** Silence for the whole free device buffer, then play() — the start of output. */
    private fun prime(s: VoiceStream, sk: SinkHandle) {
        val fb = 2 * maxOf(s.ch, 1)
        val head = runCatching { sk.track.headFrames() }.getOrDefault(headBase) - headBase
        val free = (sk.track.bufferFrames - (written - head)).toInt()
        if (free > 0) {
            val w = runCatching { sk.track.write(ByteArray(free * fb), 0, free * fb) }.getOrDefault(-1)
            if (w > 0) { written += w / fb; primeSilenceFrames += w / fb }
        }
        runCatching { sk.track.play() }
        trackPlaying = true
    }
    /** Device-buffer primes (silence at each track start), frames — reported apart, not in the identity. */
    var primeSilenceFrames = 0L
        private set

    private fun writeSegments(s: VoiceStream, sk: SinkHandle, segs: List<VoiceJitterBuffer.Segment>, now: Long) {
        if (segs.isEmpty()) return
        val fb = 2 * maxOf(s.ch, 1)
        var total = 0
        for (g in segs) if (g.silence != null || g.fate == VoiceJitterBuffer.Fate.PLAYED) total += g.frames
        if (total == 0) return
        val buf = ByteArray(total * fb)
        var off = 0
        // The bound: W + one AU, plus a start-stall allowance still being paid back (fix2).
        val wAgeLimitUs = (s.jb.wMs + s.jb.allowanceMs() + s.jb.framesPerPacket * 1000L / maxOf(s.rate, 1)) * 1000L
        for (g in segs) {
            if (g.silence == null && g.fate != VoiceJitterBuffer.Fate.PLAYED) continue
            val n = g.node
            val pcm = n?.pcm
            if (pcm != null) {
                val srcOff = g.offsetFrames * fb
                val len = minOf(g.frames * fb, maxOf(0, pcm.size - srcOff))
                if (len > 0) System.arraycopy(pcm, srcOff, buf, off, len)
            }
            if (n != null && g.offsetFrames == 0) {
                s.stats.onWrite(n.rxUs, now)
                if (n.rxUs >= 0 && now - n.rxUs > wAgeLimitUs && s.jb.state == VoiceJitterBuffer.State.PLAYING) boundExceeded++
            }
            off += g.frames * fb
        }
        val w = runCatching { sk.track.write(buf, 0, buf.size) }.getOrDefault(-1)
        if (w < 0) {
            log.w("${purpose.label}: track write → $w — the sink is rebuilt at the next BEGIN")
            return
        }
        written += w / fb
        writes++
        if (!trackPlaying) {
            runCatching { sk.track.play() }
            trackPlaying = true
        }
    }

    private fun headDrained(sk: SinkHandle): Boolean {
        val head = runCatching { sk.track.headFrames() }.getOrDefault(Long.MAX_VALUE) - headBase
        return VoiceIdleRules.headDrained(head, written, periodFrames.toLong())
    }

    private fun endService(s: VoiceStream, now: Long) {
        if (endPhase == 1) {
            // Play-out: the buffer drains through normal pulls (BUFFERING with nodes waits for its start).
            if (!s.jb.isEmpty()) {
                // A device still waking (start stall) does not use up the play-out bound (review MINOR).
                if (awaitingHeadSinceUs > 0) {
                    playoutDeadlineUs = maxOf(playoutDeadlineUs,
                        now + (s.jb.bufferedMs() + deviceBufferMs + 2 * VoiceLatency.HEAD_DRAIN_SLACK_MS) * 1000L)
                    return
                }
                if (now < playoutDeadlineUs) return
                val n = s.jb.dropAll(VoiceJitterBuffer.Drop.RETIRED_UNPLAYED)
                log.w("${purpose.label}: ${s.label} END play-out overran its bound (the track stopped consuming?) — $n AUs retired unplayed " +
                      "(head ${if (headEverMoved) "had moved" else "NEVER moved"})")
                finish(s, now, "END, play-out bound")
                return
            }
            endPhase = 2
            drainDeadlineUs = now + (deviceBufferMs + VoiceLatency.HEAD_DRAIN_SLACK_MS) * 1000L
        }
        if (endPhase == 2) {
            val sk = sink
            if (sk != null && trackPlaying && !headDrained(sk) && now < drainDeadlineUs) return
            finish(s, now, "END")
        }
    }

    /** END after play-out and head drain: stop + flush, the stop rules, back to the pool. */
    private fun finish(s: VoiceStream, now: Long, why: String) {
        val urNow = sink?.track?.underruns() ?: -1
        val underruns = if (urNow >= 0) urNow - underrunBase else -1
        // B-V1-W: what the device actually consumed for this stream against what iOS sent.
        val headNow = sink?.let { runCatching { it.track.headFrames() }.getOrDefault(headBase) - headBase } ?: 0L
        val deliveredMs = (headTotal + headNow) * 1000 / maxOf(s.rate, 1)
        val audioMs = (s.endFrame?.frames ?: s.read) * s.jb.framesPerPacket * 1000L / maxOf(s.rate, 1)
        val silenceMs = (primeSilenceFrames + s.jb.rebufferSilenceFrames + s.jb.concealedFrames) * 1000 / maxOf(s.rate, 1)
        val bvw = "B-V1-W: stream audio ${audioMs}ms, device delivered ${deliveredMs}ms (incl. ${silenceMs}ms prime/re-buffer/concealed " +
            "silence), deficit ${audioMs - (deliveredMs - silenceMs)}ms, device start stall ${deviceStartStallMs}ms, " +
            "excess loud ${s.jb.excessLoud} of ${s.jb.excess} (silent catch-up ${s.jb.excessSilentCatchUp})"
        sink?.let { stopFlush(it); core.giveBack(it) }
        sink = null
        endPhase = 0
        current = null
        core.finished(s)
        s.identityLogged = true
        val end = s.endFrame
        val idLine = if (end != null) s.identityLine(end) else "no END received (exempt from the identity)"
        val msg = "${purpose.label}: ${s.label} finished ($why) — ${s.jb.summary()}; $idLine; ${s.stats.total()}" +
            (if (boundExceeded > 0) "; $boundExceeded AUs written past W + one AU" else "") +
            "; device-buffer prime ${primeSilenceFrames * 1000 / maxOf(s.rate, 1)}ms, pts mismatches $ptsMismatch, " +
            "track underruns $underruns; $bvw"
        if (end != null && !s.identityHolds(end)) log.w(msg) else log.i(msg)
        boundExceeded = 0
        primeSilenceFrames = 0
        val next = pendingBegin
        if (next != null) {
            pendingBegin = null
            // The purpose continues with the waiting stream: the stop rules do not run between them
            // (no focus churn); the duck binding moves on as for a supersede.
            if (purpose == Purpose.NAV) {
                if (duckBoundScid == s.scid) duckBoundScid = next.scid
                if (holdScid == s.scid) holdScid = next.scid
            }
            onBegin(VoiceEvent.Begin(next, false, next.gen), now)
            val cut = next.jb.catchUpToTarget()
            if (cut > 0) log.i("${purpose.label}: ${next.label} started after waiting — ${cut * 1000 / maxOf(next.rate, 1)}ms older than the target dropped as late (real time)")
            return
        }
        if (purpose == Purpose.NAV) navStreamEnded(s, now) else stopRule(s)
    }

    private fun dropPending(p: VoiceStream, by: VoiceStream) {
        val n = p.jb.dropAll(VoiceJitterBuffer.Drop.SUPERSEDED)
        core.markSuperseded(p)
        log.i("${purpose.label}: ${p.label} (waiting to start) superseded by ${by.label} — $n AUs → superseded")
    }

    private fun stopRule(s: VoiceStream) {
        val slot = core.slots.getValue(purpose)
        when (VoiceIdleRules.focusStopRule(purpose, core.ctrlSeen && core.ctrlCall)) {
            VoiceIdleRules.StopRule.RELEASE ->
                if (slot.release("END")) log.i("${purpose.label}: focus released at END of ${s.label}")
            VoiceIdleRules.StopRule.HOLD_UNTIL_PHONE_IDLE ->
                if (slot.held) log.i("${purpose.label}: focus kept after END of ${s.label} — iOS says a call is up; released at phone 1→0")
            VoiceIdleRules.StopRule.HANDOVER ->
                if (slot.held) { slot.handoverPending = true
                    log.i("siri: focus kept after END of ${s.label} — a call is up (Siri-placed call); released at the CALL BEGIN's GRANTED or phone 1→0") }
        }
    }

    /** Grace expiry / stall backstop: END with no play-out and no head-drain wait. */
    private fun finishWithoutPlayout(s: VoiceStream, why: String, stall: Boolean, now: Long) {
        if (s === pendingBegin) {
            pendingBegin = null
            val n = s.jb.dropAll(VoiceJitterBuffer.Drop.RETIRED_UNPLAYED)
            log.w("${purpose.label}: ${s.label} (waiting to start) END — $why; $n AUs retired unplayed")
            core.finished(s); return
        }
        if (current !== s) { core.finished(s); return }
        val n = s.jb.dropAll(VoiceJitterBuffer.Drop.RETIRED_UNPLAYED)
        sink?.let { pauseFlush(it) }
        if (stall) log.w("${purpose.label}: ${s.label} inferred END — $why; $n AUs retired unplayed")
        else log.w("${purpose.label}: ${s.label} END — $why; $n AUs retired unplayed (transport_lost unknown: no END)")
        finish(s, now, if (stall) "stall backstop" else "grace expired")
    }

    /** closed: everything released, nothing pooled; the worker exits after this. */
    private fun retire() {
        if (retired) return
        retired = true
        val s = current
        if (s != null) {
            val n = s.jb.dropAll(VoiceJitterBuffer.Drop.RETIRED_UNPLAYED)
            log.i("${purpose.label}: ${s.label} retired (router stopped) — $n AUs retired unplayed")
            core.finished(s)
        }
        current = null
        sink?.let { stopFlush(it); core.giveBack(it) }   // closed → released
        sink = null
        if (purpose != Purpose.NAV) core.slots.getValue(purpose).release("router stopped")
    }

    private fun pauseFlush(sk: SinkHandle) {
        accumulateHead(sk)
        runCatching { sk.track.pause() }
        runCatching { sk.track.flush() }
        trackPlaying = false
        endStartHold()
        resetTrackCounters()
    }

    private fun endStartHold() {
        if (awaitingHeadSinceUs > 0) { awaitingHeadSinceUs = 0; current?.jb?.holdTrims(false) }
    }

    private fun stopFlush(sk: SinkHandle) {
        endStartHold()
        accumulateHead(sk)
        runCatching { sk.track.pause() }
        runCatching { sk.track.stop() }
        runCatching { sk.track.flush() }
        runCatching { sk.codec.flush() }
        trackPlaying = false
        resetTrackCounters()
        inflight.clear()
        expectDecodeTs = -1L
    }

    private fun resetTrackCounters() {
        written = 0
        headBase = sink?.let { runCatching { it.track.headFrames() }.getOrDefault(0L) } ?: 0L
    }

    private fun report() {
        val s = current ?: return
        val line = s.stats.window() ?: return
        val sk = sink
        val head = if (sk != null) runCatching { sk.track.headFrames() }.getOrDefault(headBase) - headBase else 0L
        val level = if (sk != null && s.rate > 0) "${(written - head) * 1000 / s.rate}ms" else "-"
        val lat = sk?.track?.outputLatencyMs(written)?.let { "${it}ms" } ?: "-"
        val ur = sk?.track?.underruns() ?: -1
        log.i("voice stats: $line, jb ${s.jb.bufferedMs()}ms (${s.jb.state}), track level $level, output latency $lat, " +
              "track underruns ${if (ur >= 0) ur - underrunBase else -1}, " +
              "device buffer ${sk?.track?.bufferFrames?.let { it * 1000L / maxOf(s.rate, 1) } ?: -1}ms, " +
              "W-triggers ${s.jb.wTriggers}, late ${s.jb.late}, excess ${s.jb.excess}, concealed " +
              "${s.jb.concealedFrames * 1000 / maxOf(s.rate, 1)}ms, rebuffer_silence ${s.jb.rebufferSilenceFrames * 1000 / maxOf(s.rate, 1)}ms" +
              ", loud AUs $loudAus" + (if (purpose == Purpose.NAV) ", media ${if (ducked) "ducked" else if (holdScid != null) "held until END" else "unducked"}" else ""))
    }

    // ---- NAV: live AltAudio, the DROP safety net, the duck binding (V1b/V2/V3) -------------------------

    private fun onNavBegin(s: VoiceStream) {
        loudAus = 0
        s.latchedSuppressed = core.navSuppressed()
        if (s.latchedSuppressed) {
            val by = core.suppressedBy()
            log.i("nav: ${s.label} began while a $by is live — suppressed until its END or a duckAudio after the $by (NAV_DURING_VOICE=DROP)")
            core.board.state("voice-sink/nav", "DROPPED ($by)")
        } else {
            log.i("nav: ${s.label} plays live, no focus request (stock: OD-8C)")
        }
        if (ducked && duckPendingBind) { duckBoundScid = s.scid; duckPendingBind = false }
        trackPlaying = false
    }

    /** DROP (non-default): the suppression onset cut a live NAV stream — silence it now; never replayed (OD-8A). */
    private fun dropCut(s: VoiceStream, now: Long) {
        cutPending = false
        sink?.let { pauseFlush(it) }
        wallMode = true; lastWallPullUs = now
        val by = core.suppressedBy()
        log.i("nav: ${s.label} cut — a $by began (NAV_DURING_VOICE=DROP); suppressed while it lasts")
        core.board.state("voice-sink/nav", "DROPPED ($by)")
    }

    private fun navTimers(now: Long) {
        val s = current ?: return
        val ns = core.navSuppressed()
        if (s.cutSuppressed && !ns) {
            s.cutSuppressed = false
            log.i("nav: call/Siri over — ${s.label} plays live again")
            core.board.state("voice-sink/nav", "live")
        }
        if (ns && !s.cutSuppressed && !s.latchedSuppressed) { s.cutSuppressed = true; dropCut(s, now) }
    }

    private fun onDuck(now: Long) {
        if (purpose != Purpose.NAV) return
        ducked = true
        holdScid = null
        val live = current?.takeIf { it.state == VoiceStream.State.ARMED || it.state == VoiceStream.State.PENDING_WORKER }
        if (live != null) { duckBoundScid = live.scid; duckPendingBind = false } else { duckBoundScid = null; duckPendingBind = true }
        val ns = core.navSuppressed()
        if (!ns && live != null && live.latchedSuppressed) {
            live.latchedSuppressed = false
            log.i("nav: duckAudio with no call/Siri live — ${live.label} is no longer suppressed")
        }
        log.i("nav: duckAudio bound to ${live?.label ?: "the next NAV BEGIN"}")
        // bus1 still active (a back-to-back prompt on a playing 101): the platform duck is on — lift again.
        if (live != null && trackPlaying && awaitingHeadSinceUs == 0L && headEverMoved) liftForBus(now)
    }

    /**
     * unduckAudio: restore media now (default: the GM platform duck carries the tail, stock T1). Only
     * with the NON-default NAV_UNDUCK_HOLD, and its NAV stream still live, media is held at
     * [VoiceLatency.NAV_HOLD_LEVEL] until that stream ENDs. The NAV worker is the single decider; the
     * executor stamp is the cp-meta arrival ([stampUs]).
     */
    private fun onUnduck(durationMs: Long, stampUs: Long) {
        if (purpose != Purpose.NAV) return
        ducked = false
        val bound = duckBoundScid
        duckBoundScid = null
        duckPendingBind = false
        val live = current?.takeIf { it.scid == bound && it.state != VoiceStream.State.RETIRED && it.state != VoiceStream.State.SUPERSEDED }
        if (live != null && core.navUnduckHold()) {
            holdScid = live.scid
            holdDurMs = durationMs
            log.i("nav: unduckAudio while ${live.label} is live — media held at ${VoiceLatency.NAV_HOLD_DB} dB until it ENDs (stock T1)")
            core.duck.ramp(VoiceLatency.NAV_HOLD_LEVEL, durationMs, "unduckAudio — hold until the nav stream ends", stampUs)
        } else {
            // Default (NAV_UNDUCK_HOLD = false, stock T1): media back to 1.0 now; the GM platform duck
            // (VAC gaincontrol, -13.6 dB while bus1 is active) carries the tail until the nav stream ends.
            holdScid = null
            core.duck.ramp(1f, durationMs, "unduckAudio", stampUs)
        }
    }

    /** The NAV stream ENDed: a lost unduck → restore (W); an unduck hold on it → restore (stock T1). */
    private fun navStreamEnded(s: VoiceStream, now: Long) {
        if (ducked && duckBoundScid == s.scid) {
            ducked = false
            duckBoundScid = null
            log.w("nav: ${s.label} ended with the media still ducked — the unduckAudio was lost; restoring")
            core.duck.ramp(1f, DuckMath.DEFAULT_DURATION_MS, "restore (lost unduck: ${s.label} ended)", now)
        } else if (holdScid == s.scid) {
            holdScid = null
            core.duck.ramp(1f, holdDurMs, "restore (${s.label} ended after its unduck — stock T1)", now)
        }
    }

}
