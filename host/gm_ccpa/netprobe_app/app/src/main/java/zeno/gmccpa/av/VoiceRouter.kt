package zeno.gmccpa.av

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
import android.media.MediaCodec
import android.media.MediaFormat
import zeno.gmccpa.ProbeLog
import zeno.gmccpa.logging.SessionTrace
import zeno.gmccpa.pair.NativeCore
import java.io.InputStream
import java.nio.ByteBuffer
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.locks.ReentrantLock

/**
 * U10-c (route priming, F159) stays OFF: plan v5.8 V4b enables the NAV pool pre-warm only after V4a's
 * bench (B-V4-1, B-V4-2) has passed, and then under P-N5 (no focus at pre-warm; the pre-warmed track
 * must not be an active player). With this false a route edge only logs and drives V4a's NAV pool
 * retention.
 */
private const val NAV_ROUTE_PRIME_ENABLED = false

/**
 * The non-media half of CarPlay audio: phone calls, Siri, alerts and navigation — real time, per
 * stream (plan v5.8 V1b/V2/V3/V4a).
 *
 * `:9003` carries every non-media stream on one socket, in the v2 voice tag (`VoiceFrame`): a BEGIN
 * per stream at SETUP, every AU tagged with its stream (scid), rust's receive time and its RTP
 * sequence number and timestamp, and an END at TEARDOWN. So a sink lives exactly as long as its
 * stream, and every AU goes through that stream's own timestamp jitter buffer — target
 * audioLatencyMs (stock), trimmed down to W, never a growing buffer (owner, 2026-10-07; F163).
 *
 * This class is the ANDROID half only: MediaCodec/AudioTrack/AudioManager adapters behind the ports
 * of [VoiceStreams], the reader / worker / sweeper / duck-executor threads, and the control-plane
 * inputs (modes, duck commands, route records). Every decision lives in the pure classes
 * ([VoiceStreams], [PurposeEngine], [VoiceJitterBuffer], [FocusSlot], [VoiceIdleRules],
 * [DuckComposer]) and is host-tested there.
 *
 * Routing is by [AudioAttributes] usage, never by device address: GM's CarAudioService maps
 * usage → context → volume group → bus. See `docs/13_AUDIO_ROUTING.md`.
 *
 * @param onDuckRamp V3: the media duck, `AacPlayer.setVoiceDuckRamp(target, durationMs)` — reached
 *   only from the duck executor (the iPhone's duck/unduck), the lost-unduck restore and session end.
 * @param onAssistant true while Siri is live, false after. The media track must be **paused**, not
 *   merely ducked, for the duration: AAOS `CarVolume` V1 ranks VOICE_COMMAND below MUSIC and a
 *   ducked player is still an active player, so the knob would stay on media (decompiled
 *   2026-08-12; see AacPlayer.setAssistantSpeaking).
 */
class VoiceRouter(
    private val ctx: Context,
    private val onDuckRamp: (Float, Long) -> Unit,
    private val onAssistant: (Boolean) -> Unit = {},
    /** B-V3-1: AacPlayer.sampleDuck — the duck executor samples the shaper at start + duration + 20 ms. */
    private val duckSampler: () -> DuckSample = { DuckSample(null, true) },
) {

    private val log = ProbeLog.sub("voice")
    private val running = AtomicBoolean(false)
    private val am = ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager

    val bytesIn = AtomicLong(0)

    /**
     * The voice usages. CALL = bus4_call_out, ASSISTANT = bus2_voice_command_out, NAV =
     * bus1_navigation_out. ALERT (OD-7C, owner 2026-10-07): USAGE_NOTIFICATION_RINGTONE is the
     * CALL_RING context, which this radio routes to bus3_call_ring_out (dumpsys car_service,
     * 2026-10-07), so alert volume is ring volume; USAGE_VOICE_COMMUNICATION_SIGNALLING mapped to CALL.
     */
    private enum class Usage(val usage: Int, val content: Int) {
        CALL(AudioAttributes.USAGE_VOICE_COMMUNICATION, AudioAttributes.CONTENT_TYPE_SPEECH),
        ASSISTANT(AudioAttributes.USAGE_ASSISTANT, AudioAttributes.CONTENT_TYPE_SPEECH),
        ALERT(AudioAttributes.USAGE_NOTIFICATION_RINGTONE, AudioAttributes.CONTENT_TYPE_SONIFICATION),
        NAV(AudioAttributes.USAGE_ASSISTANCE_NAVIGATION_GUIDANCE, AudioAttributes.CONTENT_TYPE_SPEECH),
    }

    private fun usageOf(p: VoiceLatency.Purpose): Usage = when (p) {
        VoiceLatency.Purpose.CALL -> Usage.CALL
        VoiceLatency.Purpose.ASSISTANT -> Usage.ASSISTANT
        VoiceLatency.Purpose.ALERT -> Usage.ALERT
        VoiceLatency.Purpose.NAV -> Usage.NAV
    }

    private companion object {
        /**
         * AAC-ELD AudioSpecificConfig as the shipping fdk-aac encoder actually emits it
         * (`ccpa_custom/docs/50`): SBR enabled by fdk auto-mode, frameLength 480. `:9003` carries RAW
         * access units with no ADTS header, so MediaCodec has to be handed this.
         */
        val ELD_CSD_16K_MONO = byteArrayOf(
            0xF8.toByte(), 0xF0.toByte(), 0x31, 0x2C, 0x00, 0xBC.toByte(), 0x00
        )
        /** samplingFrequencyIndex per ISO/IEC 14496-3 Table 1.16. */
        val SF_INDEX = intArrayOf(96000, 88200, 64000, 48000, 44100, 32000,
                                  24000, 22050, 16000, 12000, 11025, 8000, 7350)
        /** Sweeper cadence. */
        const val SWEEP_TICK_MS = 100L
        const val BOARD_ROUTER = "voice-router"
        const val BOARD_SINK_PREFIX = "voice-sink/"
        /** A worker with nothing live waits this long at most between passes. */
        const val IDLE_WAIT_MS = 1_000L
        /** How long a codec.queue may wait for an input buffer. */
        const val QUEUE_WAIT_US = 2_000L
    }

    init {
        // 48 kHz mono is advertised (audioFormat bit 32) alongside 16 kHz, served by the SAME
        // synthesised path as the truck-proven 48 kHz STEREO ASC; only the channel nibble differs.
        check(eldCsd(48000, 1).contentEquals(byteArrayOf(0xF8.toByte(), 0xE6.toByte(), 0x30, 0x00))) {
            "eldCsd(48k mono) drifted from the ISO 14496-3 1.6.2.1 layout"
        }
    }

    /**
     * Build an AAC-ELD AudioSpecificConfig for [rate]/[channels]: 5 bits AOT (escape 31 + 6-bit
     * (39-32)), 4 bits freq index, 4 bits channel config, then the ELDSpecificConfig with
     * frameLengthFlag=1 (480 — every proven ELD config in this project). The 16 k mono case is the
     * device-confirmed capture verbatim (SBR on).
     */
    private fun eldCsd(rate: Int, channels: Int): ByteArray {
        if (rate == 16000 && channels == 1) return ELD_CSD_16K_MONO
        val fi = SF_INDEX.indexOf(rate).let { if (it < 0) 3 else it }
        val ch = channels.coerceIn(1, 2)
        var acc = 0L; var bits = 0
        fun put(v: Int, n: Int) { acc = (acc shl n) or (v.toLong() and ((1L shl n) - 1)); bits += n }
        put(31, 5); put(39 - 32, 6); put(fi, 4); put(ch, 4)
        put(1, 1); put(0, 1); put(0, 1); put(0, 1); put(0, 1); put(0, 4)
        val pad = (8 - (bits % 8)) % 8
        put(0, pad)
        val out = ByteArray((bits) / 8)
        for (i in out.indices) out[i] = ((acc shr ((out.size - 1 - i) * 8)) and 0xFF).toByte()
        return out
    }

    // ---- ports ------------------------------------------------------------------------------------

    private val clock = VoiceClock { NativeCore.voiceClockUs() }

    private val vlog = object : VoiceLog {
        override fun i(msg: String) = log.i(msg)
        override fun w(msg: String) = log.w(msg)
        override fun e(msg: String) = log.e(msg)
    }

    /** MediaCodec AAC-ELD behind [CodecPort]. Its owner worker is the only caller. */
    private inner class AndroidCodec(val mc: MediaCodec) : CodecPort {
        private val info = MediaCodec.BufferInfo()
        override fun queue(au: ByteArray, key: Long): Boolean {
            val idx = mc.dequeueInputBuffer(QUEUE_WAIT_US)
            if (idx < 0) return false
            // Once dequeued the index MUST be queued back on every path (a leaked input buffer
            // silences the sink for good).
            var size = 0
            try {
                val ib = mc.getInputBuffer(idx)
                if (ib != null) {
                    ib.clear()
                    if (au.size <= ib.remaining()) { ib.put(au); size = au.size }
                    else log.w("decoder: AU ${au.size} B > input buffer ${ib.remaining()} B — dropped")
                }
            } finally {
                mc.queueInputBuffer(idx, 0, size, key, 0)
            }
            return true
        }

        override fun drain(waitUs: Long): List<Decoded> {
            val out = ArrayList<Decoded>(2)
            var wait = waitUs
            while (true) {
                val o = mc.dequeueOutputBuffer(info, wait)
                if (o < 0) break
                wait = 0
                val ob = mc.getOutputBuffer(o)
                if (ob != null && info.size > 0) {
                    val pcm = ByteArray(info.size)
                    ob.position(info.offset); ob.get(pcm)
                    out.add(Decoded(info.presentationTimeUs, pcm))
                }
                mc.releaseOutputBuffer(o, false)
            }
            return out
        }

        override fun flush() { mc.flush() }
    }

    /** AudioTrack behind [TrackPort]. Non-blocking writes; its owner worker is the only writer. */
    private class AndroidTrack(val t: AudioTrack, override val bufferFrames: Int, val rate: Int) : TrackPort {
        override fun play() = t.play()
        override fun pause() = t.pause()
        override fun flush() = t.flush()
        override fun stop() = t.stop()
        override fun write(pcm: ByteArray, off: Int, len: Int): Int = t.write(pcm, off, len, AudioTrack.WRITE_NON_BLOCKING)
        override fun headFrames(): Long = t.playbackHeadPosition.toLong() and 0xFFFFFFFFL
        override fun underruns(): Int = runCatching { t.underrunCount }.getOrDefault(-1)
        override fun outputLatencyMs(writtenFrames: Long): Long? {
            val ts = android.media.AudioTimestamp()
            if (!t.getTimestamp(ts)) return null
            val pendingUs = (writtenFrames - ts.framePosition) * 1_000_000 / maxOf(rate, 1)
            val sinceUs = (System.nanoTime() - ts.nanoTime) / 1000
            return (pendingUs - sinceUs) / 1000
        }
    }

    /**
     * Builds a purpose's codec + track: the track at the purpose's device buffer (stock: phone 32 ms,
     * Siri 128, alert 48, alt 48; floored at getMinBufferSize, logged when the floor wins — U1), start
     * threshold = the device buffer, NOT started (a pooled or idle track is never an active player).
     */
    private val factory = object : SinkFactory {
        override fun build(p: VoiceLatency.Purpose, rate: Int, ch: Int): SinkHandle? {
            val u = usageOf(p)
            var mc: MediaCodec? = null
            var tk: AudioTrack? = null
            val t0 = System.nanoTime()
            return try {
                val attrs = AudioAttributes.Builder().setUsage(u.usage).setContentType(u.content).build()
                val mask = if (ch >= 2) AudioFormat.CHANNEL_OUT_STEREO else AudioFormat.CHANNEL_OUT_MONO
                val fb = 2 * (if (ch >= 2) 2 else 1)
                val minBuf = AudioTrack.getMinBufferSize(rate, mask, AudioFormat.ENCODING_PCM_16BIT)
                val devMs = VoiceLatency.deviceBufferMs(p)
                val want = (rate.toLong() * devMs / 1000).toInt() * fb
                val bytes = maxOf(minBuf, want)
                val track = AudioTrack.Builder()
                    .setAudioAttributes(attrs)
                    .setAudioFormat(AudioFormat.Builder().setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setSampleRate(rate).setChannelMask(mask).build())
                    .setBufferSizeInBytes(bytes)
                    .setTransferMode(AudioTrack.MODE_STREAM)
                    .build()
                tk = track
                val frames = track.bufferSizeInFrames
                val thr = runCatching { track.setStartThresholdInFrames(frames) }.getOrNull()
                val fmt = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_AAC, rate, ch)
                fmt.setInteger(MediaFormat.KEY_AAC_PROFILE, android.media.MediaCodecInfo.CodecProfileLevel.AACObjectELD)
                fmt.setByteBuffer("csd-0", ByteBuffer.wrap(eldCsd(rate, ch)))
                val codec = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_AUDIO_AAC)
                mc = codec
                codec.configure(fmt, null, null, 0)
                codec.start()
                val bufMs = frames * 1000L / rate
                val floor = if (minBuf > want) " — FLOORED at getMinBufferSize ${minBuf / fb * 1000L / rate}ms (stock wants ${devMs}ms; U1)" else ""
                val desc = "AAC-ELD ${rate}Hz ${ch}ch → AudioTrack(usage=${u.usage}, buffer ${bufMs}ms$floor, " +
                    "start threshold ${thr?.let { "${it * 1000L / rate}ms" } ?: "n/a"}), decoder=${codec.name}, built in ${(System.nanoTime() - t0) / 1_000_000}ms"
                log.i("${p.label}: sink built: $desc")
                SessionTrace.Board.up(BOARD_SINK_PREFIX + p.label, "AAC-ELD ${rate}Hz ${ch}ch usage=${u.usage} buffer ${bufMs}ms")
                SinkHandle(p, rate, ch, AndroidCodec(codec), AndroidTrack(track, frames, rate), desc)
            } catch (e: Throwable) {
                runCatching { mc?.release() }
                runCatching { tk?.release() }
                log.e("${p.label}: sink build failed (${rate}Hz ${ch}ch): ${e.javaClass.simpleName}: ${e.message}")
                SessionTrace.Board.note(BOARD_SINK_PREFIX + p.label, "FAILED (build threw ${e.javaClass.simpleName})")
                null
            }
        }

        override fun release(s: SinkHandle) {
            val c = (s.codec as AndroidCodec).mc
            val t = (s.track as AndroidTrack).t
            runCatching { t.pause() }; runCatching { t.flush() }; runCatching { t.stop() }; runCatching { t.release() }
            runCatching { c.stop() }; runCatching { c.release() }
            log.i("${s.purpose.label}: sink freed (${s.rate}Hz ${s.ch}ch)")
            SessionTrace.Board.down(BOARD_SINK_PREFIX + s.purpose.label, "sink freed")
        }
    }

    /**
     * AudioManager behind [FocusPort]: a FRESH listener object per request (AAOS keys focus on
     * listener identity, and AudioManager re-resolves a queued callback by the listener's id — a
     * reused listener would hand an old request's LOSS to a new one), callbacks on the main looper,
     * and never a delayed gain (OD-8A).
     */
    private val focusPort = object : FocusPort {
        override fun request(purpose: VoiceLatency.Purpose, mayDuck: Boolean, listener: (Int) -> Unit): FocusGrant {
            val u = usageOf(purpose)
            val attrs = AudioAttributes.Builder().setUsage(u.usage).setContentType(u.content).build()
            val l = object : AudioManager.OnAudioFocusChangeListener {
                override fun onAudioFocusChange(change: Int) = listener(change)
            }
            val req = AudioFocusRequest.Builder(
                if (mayDuck) AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_MAY_DUCK else AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
                .setAudioAttributes(attrs)
                .setOnAudioFocusChangeListener(l, android.os.Handler(android.os.Looper.getMainLooper()))
                .build()
            val t0 = System.nanoTime()
            val r = am.requestAudioFocus(req)
            log.i("${purpose.label}: requestAudioFocus(usage=${u.usage}, ${if (mayDuck) "GAIN_TRANSIENT_MAY_DUCK" else "GAIN_TRANSIENT"}) " +
                  "→ $r in ${(System.nanoTime() - t0) / 1_000_000}ms")
            return FocusGrant(r, req)
        }

        override fun abandon(handle: Any) {
            runCatching { am.abandonAudioFocusRequest(handle as AudioFocusRequest) }
        }
    }

    private val duckExec = DuckExecutor(
        clockUs = { clock.nowUs() },
        apply = { target, ms, _ -> onDuckRamp(target, ms) },
        sample = { duckSampler() },
        log = { log.i(it) },
    )

    private val duckPort = object : DuckPort {
        override fun ramp(target: Float, durationMs: Long, why: String, stampUs: Long) =
            duckExec.signal(target, durationMs, why, stampUs)
    }

    private val workers = HashMap<VoiceLatency.Purpose, Worker>()

    private val core = VoiceStreams(
        clock, vlog, factory, focusPort, duckPort,
        board = { name, detail -> SessionTrace.Board.note(name, detail) },
        wake = { p -> workers[p]?.wake() },
    )
    private val engines = VoiceLatency.Purpose.values().associateWith { PurposeEngine(it, core) }
    private val reader = VoiceReader(core, clock, vlog, { running.get() }, bytesIn)

    /** One worker per purpose: drains its events, then one engine pass; waits for the next deadline or a wake. */
    private inner class Worker(val p: VoiceLatency.Purpose) {
        private val lock = ReentrantLock()
        private val cond = lock.newCondition()
        @Volatile private var signaled = false
        private val faults = AtomicLong(0)
        val thread = Thread({ loop() }, "cp-voice-${p.label}").apply { isDaemon = true }

        fun wake() {
            lock.lock()
            try { signaled = true; cond.signal() } finally { lock.unlock() }
        }

        private fun fault(stage: String, t: Throwable) {
            val n = faults.incrementAndGet()
            if (n == 1L || n % 100 == 0L) log.e("${p.label} worker: $stage threw ${t.javaClass.simpleName}: ${t.message} (x$n)")
        }

        private fun loop() {
            raisePriority("cp-voice-${p.label}")
            val q = core.events.getValue(p)
            val eng = engines.getValue(p)
            while (true) {
                while (true) {
                    val e = q.poll() ?: break
                    runCatching { eng.onEvent(e) }.onFailure { fault("event ${e.javaClass.simpleName}", it) }
                }
                if (eng.retired) break
                val next = runCatching { eng.service() }.getOrElse { fault("service", it); clock.nowUs() + VoiceLatency.PERIOD_MS * 1000L }
                val now = clock.nowUs()
                val waitUs = if (next == Long.MAX_VALUE) IDLE_WAIT_MS * 1000 else (next - now).coerceIn(0, IDLE_WAIT_MS * 1000)
                lock.lock()
                try {
                    if (!signaled && q.isEmpty() && waitUs > 0) cond.awaitNanos(waitUs * 1000)
                    signaled = false
                } catch (_: InterruptedException) {
                    // stop() never interrupts workers; a stray interrupt just ends this wait.
                } finally {
                    lock.unlock()
                }
            }
            log.i("${p.label} worker exited")
        }
    }

    // ---- lifecycle --------------------------------------------------------------------------------

    private var sweeper: Thread? = null
    private var duckThread: Thread? = null

    /**
     * One-way teardown latch on this single-use router. Written by [stop] WITHOUT a lock, before
     * `running` goes false; READ under `sinks` (the [VoiceStreams] monitor) by [consume]'s gate and
     * finally, and by [sweeperExitRelease].
     */
    @Volatile private var closed = false

    /** [consume] calls between their entry gate and their `finally`, under `sinks`. */
    private var consumeActive = 0

    fun start() {
        running.set(true)
        core.navRetain = false        // V4a: NAV sinks are pooled only while a route is ACTIVE
        for (p in VoiceLatency.Purpose.values()) workers[p] = Worker(p)
        workers.values.forEach { it.thread.start() }
        duckThread = Thread({
            while (running.get()) { if (!duckExec.runOnce()) break }
        }, "cp-voice-duck").apply { isDaemon = true; start() }
        sweeper = Thread({
            try {
                while (running.get()) {
                    try { Thread.sleep(SWEEP_TICK_MS) } catch (_: InterruptedException) { return@Thread }
                    val now = clock.nowUs()
                    runCatching { core.sweep(now) }.onFailure { sweepFault("sweep", it) }
                    runCatching { assistantTick(now) }.onFailure { sweepFault("assistantTick", it) }
                    runCatching { routeTick(now) }.onFailure { sweepFault("routeTick", it) }
                    runCatching { backstopTick(now) }.onFailure { sweepFault("backstop", it) }
                    if (now - reportAtUs >= VoiceLatency.REPORT_MS * 1000) {
                        reportAtUs = now
                        VoiceLatency.Purpose.values().forEach { core.post(it, VoiceEvent.Report) }
                    }
                }
            } finally {
                sweeperExitRelease()
            }
        }, "cp-voice-sweep").apply { isDaemon = true; start() }
        SessionTrace.Board.up(BOARD_ROUTER, "v2 voice seam: per-stream jitter buffers, NAV focus none (stock), " +
            "NAV_DURING_VOICE=${VoiceLatency.NAV_DURING_VOICE}")
        log.i("voice router up — NAV focus none (stock, OD-8C), NAV_DURING_VOICE=${VoiceLatency.NAV_DURING_VOICE}, " +
              "stall ${VoiceLatency.STREAM_STALL_MS}ms, reconnect grace ${VoiceLatency.RECONNECT_GRACE_MS}ms")
    }

    @Volatile private var reportAtUs = 0L

    /**
     * fix2 (Fable perf F2: cp-voice-nav runnable-not-running 97.5 ms mid-prompt at nice 0, against a 48 ms
     * device buffer; rc1 review): the voice workers and the :9003 reader run at URGENT_AUDIO (-19), like
     * MicUplink's capture. The only read of [VoiceLatency.VOICE_THREADS_URGENT_AUDIO]. Logged per thread.
     */
    private fun raisePriority(what: String) {
        if (!VoiceLatency.VOICE_THREADS_URGENT_AUDIO) return
        runCatching { android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_URGENT_AUDIO) }
            .onSuccess { log.i("$what at THREAD_PRIORITY_URGENT_AUDIO (tid ${android.os.Process.myTid()})") }
            .onFailure { log.w("$what: setThreadPriority(URGENT_AUDIO) refused: ${it.message} — stays at default") }
    }

    private val sweepFaults = HashMap<String, Int>()
    private fun sweepFault(stage: String, t: Throwable) {
        val n = (sweepFaults[stage] ?: 0) + 1
        sweepFaults[stage] = n
        if (n == 1 || n % 50 == 0) log.e("$stage threw ${t.javaClass.simpleName}: ${t.message} (x$n) — sweeper skipped it")
    }

    /**
     * The sweeper's exit-path release: with no consume live, nothing else would retire the engines
     * and free the pool. [VoiceStreams.close] is idempotent and frees the pool exactly once, whichever
     * of this and [consume]'s finally takes `sinks` first. Session-end duck restore.
     */
    private fun sweeperExitRelease() {
        val go = synchronized(core) { consumeActive == 0 }
        if (go) runCatching { core.close() }.onFailure { sweepFault("exitRelease", it) }
        runCatching { onDuckRamp(1f, 0L) }.onFailure { sweepFault("exitUnduck", it) }
        if (assistantWasActive) { assistantWasActive = false; runCatching { onAssistant(false) } }
    }

    /**
     * Plan v5.9 MED M5 / OD-9C: does a CarPlay CALL / ASSISTANT / ALERT focus slot hold audio focus?
     * Stock's `!sm.hasFocus()` guard on the media-loss MainAudio take (AacPlayer.voiceFocusHeld).
     * [FocusSlot.held] is read under the slot's monitor; nothing else is touched.
     */
    fun holdsVoiceFocus(): Boolean = core.slots.values.any { it.held }

    /**
     * FLAG-ONLY teardown: latches [closed], clears `running`, restores the duck (session end). It
     * releases nothing and takes no voice lock — the workers release their own codecs and tracks (a
     * MediaCodec released under a live getOutputBuffer is a SIGSEGV, not an exception).
     */
    fun stop(why: String) {
        closed = true
        running.set(false)
        onDuckRamp(1f, 0L)
        duckThread?.interrupt()
        log.i("stopping ($why) — ${bytesIn.get()} bytes; implicit BEGINs ${core.implicitBegins}, unknown ENDs ${core.unknownEnds}, " +
              "stale-gen events ${core.staleGenEvents}, v1 AUs dropped ${core.v1Dropped}")
        SessionTrace.Board.down(BOARD_ROUTER, "stopped — $why")
    }

    /** Consume the voice seam until it closes. Blocking; call on its own thread. */
    fun consume(ins: InputStream) {
        val gen: Int
        synchronized(core) {
            if (closed) { log.i("consume refused — router stopped"); return }
            consumeActive++
            gen = core.onConnect()
        }
        var why = "?"
        val prevPrio = runCatching { android.os.Process.getThreadPriority(android.os.Process.myTid()) }.getOrDefault(0)
        try {
            raisePriority("the :9003 reader")
            log.i("voice: :9003 connection $gen up")
            why = reader.run(ins, gen)
        } catch (t: Throwable) {
            why = "${t.javaClass.simpleName}: ${t.message}"
            if (running.get()) log.e("consume: $why")
        } finally {
            val retire = synchronized(core) { consumeActive--; closed }
            if (retire) {
                core.close()
                runCatching { onDuckRamp(1f, 0L) }
            } else {
                core.onDisconnect(gen, clock.nowUs())
            }
            log.i("voice: :9003 connection $gen ended ($why) — ${if (retire) "router stopped: streams retired" else "live streams held for ${VoiceLatency.RECONNECT_GRACE_MS}ms"}")
            runCatching { android.os.Process.setThreadPriority(prevPrio) }   // the serve() thread outlives this connection
        }
    }

    // ---- control plane ----------------------------------------------------------------------------

    /** `cp-meta` only: the change-only `modes:` line. */
    private var lastModesLogged = Long.MIN_VALUE

    /**
     * One decoded `modesChanged` (CarPlayActivity.onCommandPlist, `cp-meta`; must not block).
     * `speechMode != -1` = Siri up (the early START for onAssistant; the stream's END ends it).
     * `phone` ≠ 0 = a call is up: navSuppressed (DROP) and the CALL/ALERT/ASSISTANT stop rules; its
     * 1→0 edge releases the held slots. `turns` feeds V4a's route latch.
     */
    fun onModes(speechMode: Long, speechEntity: Long, phoneEntity: Long, turnsEntity: Long, size: Int) {
        val key = (speechMode shl 12) or (speechEntity shl 8) or (phoneEntity shl 4) or turnsEntity
        if (key != lastModesLogged) {
            lastModesLogged = key
            log.i("modes: speech mode=$speechMode entity=$speechEntity phone=$phoneEntity turns=$turnsEntity ($size B)")
        }
        val now = clock.nowUs()
        core.ctrlSeen = true
        val active = speechMode != -1L
        if (active != core.ctrlSpeech) {
            core.ctrlSpeechAtUs = now      // stamp before flag
            core.ctrlSpeech = active
        }
        val call = phoneEntity != 0L
        if (call != core.ctrlCall) {
            core.ctrlCall = call
            if (!call) {
                log.i("modes: phone 1→0 — releasing held CALL/ALERT slots and a handed-over Siri slot")
                listOf(VoiceLatency.Purpose.CALL, VoiceLatency.Purpose.ALERT, VoiceLatency.Purpose.ASSISTANT)
                    .forEach { core.post(it, VoiceEvent.PhoneIdle) }
            }
        }
        routeLatch.onTurns(turnsEntity != 0L, now / 1000)
    }

    /**
     * An iPhone `duckAudio` / `unduckAudio` (CarPlayActivity:816-817, `cp-meta`). [root] is the
     * parsed command plist; its `params.volume` (dB) and `params.durationMs` are read here (stock
     * defaults 0.2 linear / 500 ms) and logged per command (U9). duckAudio stamps and signals the duck
     * executor at once (no tick wait); both go to the NAV worker, which binds the duck to its stream,
     * lifts our duck once the nav track plays (the GM platform duck carries it; owner 2026-10-08) and
     * restores at the unduck (NAV_UNDUCK_HOLD, a non-default switch, would hold -13.6 dB to the END).
     */
    fun onDuckCommand(duck: Boolean, root: Any?) {
        @Suppress("UNCHECKED_CAST")
        val params = (root as? Map<String, Any?>)?.get("params") as? Map<String, Any?>
        fun num(k: String): Double? = when (val v = params?.get(k)) { is Double -> v; is Long -> v.toDouble(); is Int -> v.toDouble(); else -> null }
        val volDb = num("volume")
        val durRaw = num("durationMs")
        val dur = DuckMath.duration(durRaw)
        if (duck) {
            val target = DuckMath.duckTarget(volDb)
            log.i("nav: iPhone duckAudio volume=${volDb ?: "absent"}dB durationMs=${durRaw ?: "absent"} → ${"%.3f".format(target)} over ${dur}ms (U9)")
            duckExec.signal(target, dur, "duckAudio")
            core.post(VoiceLatency.Purpose.NAV, VoiceEvent.Duck(target, dur))
        } else {
            // The NAV worker restores (default), or holds to the stream's END if NAV_UNDUCK_HOLD is set.
            log.i("nav: iPhone unduckAudio durationMs=${durRaw ?: "absent"} → ${dur}ms (U9)")
            core.post(VoiceLatency.Purpose.NAV, VoiceEvent.Unduck(dur, clock.nowUs()))
        }
    }

    /** `cp-meta` only: the change-only `route:` line. */
    private var routeStateLogged = -1

    /**
     * One `routeGuidance` record (0x5201), `cp-meta`; [state] is its routeGuidanceState or -1. Logs
     * the state on change; every record feeds V4a's latch (a later state > 0 contradicts a lone end
     * signal), evaluated on the sweeper ([routeTick]).
     */
    fun onRouteGuidance(state: Int) {
        if (state >= 0 && state != routeStateLogged) {
            log.i("route: routeGuidanceState=$state (was ${if (routeStateLogged < 0) "none" else routeStateLogged.toString()})")
            routeStateLogged = state
        }
        if (state >= 0) routeLatch.onState(state, clock.nowUs() / 1000)
    }

    // ---- sweeper stages ---------------------------------------------------------------------------

    private var assistantWasActive = false

    /** Media pause while Siri is live: the ASSISTANT stream's BEGIN..END, with the speech START edge as an early start. */
    private fun assistantTick(now: Long) {
        val live = core.siriLive(now)
        if (live == assistantWasActive) return
        assistantWasActive = live
        log.i("assistant ${if (live) "SPEAKING — pausing media so the knob can reach the voice group" else "done — media resumes"}")
        runCatching { onAssistant(live) }
            .onFailure { log.w("onAssistant($live) threw ${it.javaClass.simpleName}: ${it.message}") }
    }

    private val routeLatch = RouteLatch()
    private var routeWasActive = false

    /**
     * V4a, every sweeper tick: the route latch drives the NAV pool retention — kept while a route is
     * ACTIVE, freed at `ended`. NAV takes no focus (OD-8C), so the latch never touches focus. V4b's
     * pre-warm stays off until B-V4-1/2 pass.
     */
    private fun routeTick(now: Long) {
        val r = routeLatch.evaluate(now / 1000)
        if (r.active == routeWasActive) return
        routeWasActive = r.active
        core.navRetain = r.active
        if (r.active) {
            log.i("route: ACTIVE (${r.cause})${if (NAV_ROUTE_PRIME_ENABLED) "" else " — NAV sinks kept in the pool; pre-warm off (V4b gated on B-V4-1/2)"}")
        } else {
            val n = core.freeNavPool()
            log.i("route: ended (${r.cause}) — $n pooled NAV sink(s) freed")
        }
    }

    private var backstopLogged = 0L

    /** The lost-phone-1→0 backstop: LOG ONLY until U18 (a call on hold may tear 100 down). */
    private fun backstopTick(now: Long) {
        val slot = core.slots.getValue(VoiceLatency.Purpose.CALL)
        val endAt = core.callEndAtUs
        if (endAt == backstopLogged) return
        if (VoiceIdleRules.callSlotBackstopLog(slot.held, core.liveOf(VoiceLatency.Purpose.CALL) != null,
                now / 1000, endAt / 1000, VoiceLatency.CALL_SLOT_BACKSTOP_LOG_MS)) {
            backstopLogged = endAt
            log.w("call: focus still held ${(now - endAt) / 1000}ms after the CALL END with no CALL BEGIN — NOT released " +
                  "(log-only until U18: phone ${if (core.ctrlCall) "still 1" else "0"})")
        }
    }
}
