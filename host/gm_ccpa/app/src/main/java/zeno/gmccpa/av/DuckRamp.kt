package zeno.gmccpa.av

/**
 * Plan v5.8 V3 — the media duck follows the iPhone's duckAudio / unduckAudio with their parameters,
 * as pure functions and state (no Android). AacPlayer drives ONE VolumeShaper per track from
 * [DuckComposer]; VoiceRouter runs [DuckExecutor] on its own thread.
 *
 * Stock parameters (Apple R14G17 AirPlayReceiverPOSIX.c:387-418, [STOCK+R]):
 *  - duckAudio: `volume` in dB → linear (default 0.2 linear when absent), clamped 0..1;
 *    `durationMs` (default 500 when absent or negative).
 *  - unduckAudio: `durationMs` (default 500); target 1.0.
 */
object DuckMath {
    const val DEFAULT_DUCK_LINEAR = 0.2f
    const val DEFAULT_DURATION_MS = 500L

    /** dB → linear (DBtoLinear: 10^(dB/20)); absent → 0.2; clamped to 0..1. */
    fun duckTarget(volumeDb: Double?): Float {
        val lin = if (volumeDb == null || volumeDb.isNaN()) DEFAULT_DUCK_LINEAR.toDouble() else Math.pow(10.0, volumeDb / 20.0)
        return lin.coerceIn(0.0, 1.0).toFloat()
    }

    /** absent or negative → 500 ms. */
    fun duration(durationMs: Double?): Long =
        if (durationMs == null || durationMs.isNaN() || durationMs < 0) DEFAULT_DURATION_MS else durationMs.toLong()
}

/**
 * One shaper curve, starting NOW: [timesMs] relative (first 0, ascending), [volumes] the gain at each,
 * linear in between. Duration = the last time; 0 = a step to the last volume.
 */
class Curve(val timesMs: LongArray, val volumes: FloatArray) {
    val durationMs: Long get() = timesMs.last()
    val start: Float get() = volumes.first()
    val end: Float get() = volumes.last()
    override fun toString() = timesMs.indices.joinToString(prefix = "[", postfix = "]") { "${timesMs[it]}ms:${"%.3f".format(volumes[it])}" }
}

/**
 * The two duck sources folded into ONE gain (rev d, R3): gain(t) = min(voice(t), focus(t)), kept as
 * min() semantics and driven through ONE VolumeShaper per track; setVolume stays 1.0.
 *  - VOICE: the iPhone's duck/unduck ramps ([setVoice]).
 *  - FOCUS: AAOS's LOSS_TRANSIENT_CAN_DUCK on the media focus ([setFocus]), a [focusRampMs] ramp to
 *    [focusDuckGain]. cp8 has 0 CAN_DUCK lines, so this source is untested live.
 * Each source is its own linear ramp; the shaper curve is their exact min (breakpoints at each ramp's
 * end and where they cross — VolumeShaper takes a multi-point curve). A repeated target is a no-op
 * (idempotent: no new curve). Pure; the caller synchronizes.
 */
class DuckComposer(
    private val focusDuckGain: Float = DuckMath.DEFAULT_DUCK_LINEAR,
    private val focusRampMs: Long = FOCUS_RAMP_MS,
) {
    private class Seg(val from: Float, val to: Float, val startMs: Long, val durMs: Long) {
        fun at(t: Long): Float = when {
            durMs <= 0 || t >= startMs + durMs -> to
            t <= startMs -> from
            else -> from + (to - from) * (t - startMs).toFloat() / durMs
        }
        val endMs: Long get() = startMs + maxOf(0L, durMs)
    }

    private var voice = Seg(1f, 1f, 0, 0)
    private var focus = Seg(1f, 1f, 0, 0)

    val voiceTarget: Float get() = voice.to
    val focusTarget: Float get() = focus.to
    val target: Float get() = minOf(voice.to, focus.to)

    fun gainAt(nowMs: Long): Float = minOf(voice.at(nowMs), focus.at(nowMs))

    /** The iPhone's command. Returns the curve to apply, or null when the voice target is unchanged. */
    fun setVoice(t: Float, durationMs: Long, nowMs: Long): Curve? {
        val tt = t.coerceIn(0f, 1f)
        if (tt == voice.to) return null
        voice = Seg(voice.at(nowMs), tt, nowMs, maxOf(0L, durationMs))
        return curve(nowMs)
    }

    /** AAOS CAN_DUCK on / off. Null when unchanged. */
    fun setFocus(ducked: Boolean, nowMs: Long): Curve? {
        val tt = if (ducked) focusDuckGain else 1f
        if (tt == focus.to) return null
        focus = Seg(focus.at(nowMs), tt, nowMs, focusRampMs)
        return curve(nowMs)
    }

    /** The curve from [nowMs] on: the current gain, then what remains of both ramps (a rebuilt track). */
    fun curve(nowMs: Long): Curve {
        val ts = sortedSetOf(nowMs)
        if (voice.endMs > nowMs) ts.add(voice.endMs)
        if (focus.endMs > nowMs) ts.add(focus.endMs)
        val base = ts.toList()
        for (i in 0 until base.size - 1) {
            val a = base[i]; val b = base[i + 1]
            val da = voice.at(a) - focus.at(a); val db = voice.at(b) - focus.at(b)
            if ((da < 0 && db > 0) || (da > 0 && db < 0)) {
                // Both are linear on [a, b]: the crossing is where the difference is zero.
                val x = a + Math.round((b - a) * (da / (da - db)).toDouble())
                if (x in (a + 1) until b) ts.add(x)
            }
        }
        val all = ts.toList()
        return Curve(LongArray(all.size) { all[it] - nowMs }, FloatArray(all.size) { gainAt(all[it]) })
    }

    companion object {
        /** The FOCUS source's ramp (it was an instant setVolume before v5.8). */
        const val FOCUS_RAMP_MS = 20L
    }
}

/**
 * The duck executor (plan v5.8 V3): cp-meta stamps a command and signals; this applies it at once
 * (no sweep-tick wait) through [apply], and samples the result at start + duration + 20 ms for the
 * bench (B-V3-1). VoiceRouter runs [runOnce] in a loop on `cp-voice-duck`. Pure: the clock and the
 * apply/sample ports are injected.
 */
/** A self-check sample of the media shaper: its volume, and whether the media track is playing. */
data class DuckSample(val volume: Float?, val playing: Boolean)

class DuckExecutor(
    private val clockUs: () -> Long,
    private val apply: (target: Float, durationMs: Long, why: String) -> Unit,
    private val sample: () -> DuckSample,
    private val log: (String) -> Unit,
) {
    data class Cmd(val target: Float, val durationMs: Long, val stampUs: Long, val why: String)

    private val q = java.util.concurrent.LinkedBlockingQueue<Cmd>()
    private var sampleAtUs = 0L
    private var sampleWant = 1f
    private var sampleWhy = ""
    var applied = 0L; private set
    private var sampleDurMs = 0L
    private var sampleDeferred = false

    private var deferSinceUs = 0L

    private companion object {
        const val DEFER_POLL_US = 250_000L
        const val DEFER_MAX_US = 60_000_000L
    }

    /** cp-meta (or a restore): stamp and signal. Never blocks. [stampUs] = when the cause arrived. */
    fun signal(target: Float, durationMs: Long, why: String, stampUs: Long = clockUs()) {
        q.offer(Cmd(target, durationMs, stampUs, why))
    }

    /** One step: wait for a command (or the pending sample), act. Returns false when interrupted. */
    fun runOnce(maxWaitMs: Long = 1_000): Boolean {
        val now = clockUs()
        val waitMs = if (sampleAtUs > 0) maxOf(0L, (sampleAtUs - now) / 1000) else maxWaitMs
        val c = try { q.poll(waitMs, java.util.concurrent.TimeUnit.MILLISECONDS) } catch (_: InterruptedException) { return false }
        if (c != null) { execute(c); return true }
        if (sampleAtUs > 0 && clockUs() >= sampleAtUs) {
            val s = sample()
            if (!s.playing) {
                // The shaper is only evaluated while the media track is mixed (bench cp12): a paused
                // track's volume is stale. Re-check until it plays, then sample one ramp later.
                if (!sampleDeferred) {
                    log("duck: self-check deferred — the media track is paused, its shaper is not evaluated ($sampleWhy)")
                    deferSinceUs = clockUs()
                } else if (clockUs() - deferSinceUs > DEFER_MAX_US) {
                    log("duck: self-check dropped — media did not resume within ${DEFER_MAX_US / 1_000_000}s ($sampleWhy)")
                    sampleAtUs = 0; sampleDeferred = false
                    return true
                }
                sampleDeferred = true
                sampleAtUs = clockUs() + DEFER_POLL_US
                return true
            }
            if (sampleDeferred) {
                sampleDeferred = false
                sampleAtUs = clockUs() + (sampleDurMs + 20) * 1000
                return true
            }
            sampleAtUs = 0
            val v = s.volume
            val ok = v != null && Math.abs(v - sampleWant) <= 0.01f
            log("duck: sampled VolumeShaper volume ${v ?: "n/a"} at start + duration + 20 ms — want $sampleWant ± 0.01 " +
                "(${if (ok) "OK" else "OFF"}; $sampleWhy)")
        }
        return true
    }

    /** Drain without waiting (tests). */
    fun drainNow() { while (true) { execute(q.poll() ?: return) } }

    /** cp-meta stamp of the last command applied: the newest command always wins (review round 1). */
    private var lastStampUs = Long.MIN_VALUE
    var skippedStale = 0L; private set

    private fun execute(c: Cmd) {
        // duckAudio is signalled straight from cp-meta; unduckAudio and the restores come through the NAV
        // worker. A command stamped BEFORE one already applied is stale (a slow NAV worker delivering an
        // unduck after the next prompt's duck, T1's 165 ms back-to-back) and is skipped.
        if (c.stampUs < lastStampUs) {
            skippedStale++
            log("duck: ${c.why} (stamped ${(lastStampUs - c.stampUs) / 1000}ms before the command in force) — stale, skipped")
            return
        }
        lastStampUs = c.stampUs
        val start = clockUs()
        apply(c.target, c.durationMs, c.why)
        applied++
        log("duck: ${c.why} → target ${"%.3f".format(c.target)} over ${c.durationMs}ms; stamp → shaper start ${(start - c.stampUs) / 1000}ms")
        sampleAtUs = start + (c.durationMs + 20) * 1000
        sampleWant = c.target
        sampleWhy = c.why
        sampleDurMs = c.durationMs
        sampleDeferred = false
    }
}
