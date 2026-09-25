package com.carlink.av

import kotlin.math.PI
import kotlin.math.asin
import kotlin.math.sin

/**
 * Fade-in for media resuming after a HARD interruption (Siri, or a call holding exclusive transient
 * focus). CarPlay pauses media itself during those, so "resume" is the first media frame that
 * arrives after the interruption — it would otherwise jump from silence straight to full level.
 *
 * ## Where the gain is applied, and why
 * In the PCM itself, per sample, in the write path (`apply`) — not `AudioTrack.setVolume` on a timer.
 * The PCM is ours on both media paths (wired PCM passthrough; wireless AAC after decode), the scaling is
 * sample-accurate and independent of the track's volume granularity and of the writer's pacing, and
 * `setVolume` stays the single authority for the focus/duck gain — so the effective gain is exactly
 * `focusGain × duckGain × rampGain` with the ramp factor living in the samples.
 *
 * ## Curve
 * Equal-power: `gain(p) = sin(π/2 · p)` for progress `p ∈ [0,1]` — 0 / 0.383 / 0.707 / 0.924 / 1.0 at
 * 0 / 25 / 50 / 75 / 100 %. Perceived loudness rises evenly, unlike linear, which sounds like nothing
 * happens for the first half. Duration [DURATION_MS].
 *
 * ## Trigger
 * Armed by [interrupted] when a hard interruption begins AND media was active just before it.
 * Started by [start] at the first media frame written after a stream gap (the track's re-prime point).
 * Never armed by a nav prompt (soft duck), the initial session start, an underrun/network gap, or a
 * user pause/play — none of those call [interrupted]. A new interruption mid-ramp cancels it and
 * keeps the level reached; the next start ramps from that level, not from 0, so there is no dip.
 */
class ResumeRamp(
    private val durationMs: Long = DURATION_MS,
    private val log: (String) -> Unit = {},
) {
    /** A hard interruption happened while media was active; the next post-gap frame starts the ramp. */
    @Volatile var armed = false
        private set

    @Volatile var active = false
        private set

    /** The ramp gain reached so far — the floor a restart begins from. 0 when idle. */
    @Volatile var level = 0f
        private set

    private var startProgress = 0.0
    private var startAt = 0L
    private var rampMs = durationMs
    private var quartersLogged = 0

    companion object {
        const val DURATION_MS: Long = 1_000L

        /** Equal-power curve. */
        fun curve(progress: Double): Float = sin(PI / 2 * progress.coerceIn(0.0, 1.0)).toFloat()

        private const val QUARTERS = 4
    }

    /** The PCM layout [apply] scales: S16LE interleaved. */
    class Format(
        val channels: Int,
        val rate: Int,
    )

    /**
     * A hard interruption began. [mediaWasActive]: audible media within the last couple of seconds.
     * An arm already set is KEPT: a chain of interruptions (Siri's within-turn focus flap, a call
     * during Siri) has silenced media by definition, so only the FIRST one can judge activity.
     */
    fun interrupted(mediaWasActive: Boolean) {
        if (active) {
            active = false
            log("resume ramp cancelled at gain ${"%.2f".format(level)} — new interruption; will restart from there")
        } else if (!armed) {
            level = 0f
        }
        armed = armed || mediaWasActive || level > 0f
    }

    /** First media frame after the gap: start (or restart from [level]) if armed. Returns true when a ramp began. */
    fun start(now: Long): Boolean {
        if (!armed) return false
        armed = false
        startProgress = asin(level.toDouble().coerceIn(0.0, 1.0)) * 2 / PI
        rampMs = ((1 - startProgress) * durationMs).toLong().coerceAtLeast(1L)
        startAt = now
        quartersLogged = 0
        active = true
        log("resume ramp START from gain ${"%.2f".format(level)}, $rampMs ms to 1.0 (equal-power)")
        return true
    }

    fun gainAt(now: Long): Float {
        if (!active) return 1f
        val p = ((now - startAt).toDouble() / rampMs).coerceIn(0.0, 1.0)
        return curve(startProgress + (1 - startProgress) * p)
    }

    /**
     * Scale S16LE [pcm] in place for the buffer that starts at [now] and spans `len / (2·channels)`
     * frames at [rate]. The gain moves linearly across the buffer between the curve values at its two
     * ends (a 20 ms buffer is far below any audible step), and the ramp ends when the buffer's end
     * passes the ramp's end.
     */
    fun apply(
        pcm: ByteArray,
        off: Int,
        len: Int,
        fmt: Format,
        now: Long,
    ) {
        if (!active) return
        val ch = fmt.channels.coerceAtLeast(1)
        val frames = len / (2 * ch)
        if (frames <= 0) return
        val bufMs = frames * 1000L / fmt.rate.coerceAtLeast(1)
        val g0 = gainAt(now)
        val g1 = gainAt(now + bufMs)
        var i = off
        for (f in 0 until frames) {
            val g = g0 + (g1 - g0) * f / frames
            repeat(ch) {
                val s = ((pcm[i + 1].toInt() shl 8) or (pcm[i].toInt() and 0xFF)).toShort().toInt()
                val v = (s * g).toInt().coerceIn(-32768, 32767)
                pcm[i] = (v and 0xFF).toByte()
                pcm[i + 1] = ((v shr 8) and 0xFF).toByte()
                i += 2
            }
        }
        level = g1
        val quarter = (((now + bufMs - startAt) * QUARTERS) / rampMs).toInt()
        if (quarter in (quartersLogged + 1) until QUARTERS) {
            quartersLogged = quarter
            log("resume ramp ${quarter * 100 / QUARTERS}% gain ${"%.2f".format(g1)}")
        }
        if (now + bufMs >= startAt + rampMs) {
            active = false
            level = 0f
            log("resume ramp END after ${now + bufMs - startAt} ms — gain 1.0")
        }
    }
}
