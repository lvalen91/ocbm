package com.carlink.audio

/**
 * The pure part of mic uplink format selection: box-negotiated `(rate, channels)` in, capture
 * profile and tick size out.
 *
 * Split out of `CarlinkManager.captureProfileFor` for one reason — the invariant that matters here
 * is not testable while it is welded to a `Context`. [MicrophoneCaptureManager] still keys its
 * capture format off riddleBox's `decodeType` table, so every negotiated format has to survive a
 * round trip through that table unchanged. When it does not, capture silently opens at 16 kHz mono
 * while the box believes it negotiated something else, and the only symptom is Siri hearing a
 * pitch-shifted stream. There is no error anywhere in that path — not on the box, not in logcat,
 * not on the phone — so a unit test is the only place it can be caught.
 *
 * Nothing here touches the Android framework, deliberately: it must run in a plain JVM test.
 */
object MicProfile {
    /** Uplink tick, matching the box's own RTP packetization. */
    const val TICK_MS = 20

    /** Ticks per second — [TICK_MS] expressed the way the chunk arithmetic needs it. */
    const val TICKS_PER_SECOND = 1000 / TICK_MS

    /** Bytes per sample per channel. The uplink is S16LE throughout. */
    const val BYTES_PER_SAMPLE = 2

    /**
     * What an unmapped format falls back to: 16 kHz mono, the CarPlay/Siri default.
     *
     * Callers are expected to complain loudly before using it. It is a floor that keeps the uplink
     * running, NOT an answer — if it is ever reached the audio reaching Siri is wrong.
     */
    const val FALLBACK_DECODE_TYPE = 5

    /**
     * riddleBox `decodeType` for a box-negotiated format, or null if the table has no entry.
     *
     * Null rather than a silent fallback so the caller has to decide, and can log, rather than
     * inheriting a wrong format by omission.
     */
    fun decodeTypeFor(
        rate: Int,
        channels: Int,
    ): Int? =
        when {
            rate == 8000 && channels == 1 -> 3
            rate == 16000 && channels == 1 -> 5
            rate == 24000 && channels == 1 -> 6
            rate == 16000 && channels == 2 -> 7
            else -> null
        }

    /**
     * Bytes of S16LE PCM in one [TICK_MS] tick.
     *
     * The cadence and this size must change together or the stream underruns. The result is always
     * a whole number of sample frames, since it is built from a frame count rather than rounded
     * down from a byte count — a half frame here would desync every subsequent sample.
     */
    fun chunkBytes(
        rate: Int,
        channels: Int,
    ): Int = rate / TICKS_PER_SECOND * BYTES_PER_SAMPLE * channels
}
