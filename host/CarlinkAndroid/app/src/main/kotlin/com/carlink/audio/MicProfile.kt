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

    /** Bytes per sample per channel. Captured PCM is S16LE throughout; what goes on the wire depends on [CODEC_MSBC]. */
    const val BYTES_PER_SAMPLE = 2

    // ---- CT_UPLINK codec byte (`[CT_UPLINK][state][rate u32 LE][ch][codec]`, lib.rs:132) ---------

    /** Raw S16LE PCM in 20 ms ticks — every CarPlay uplink and HFP narrowband (CVSD). */
    const val CODEC_PCM = 0

    /**
     * HFP wideband: the app must hand back whole 60-byte mSBC eSCO packets (H2 + 57 B frame + 1 pad),
     * one per `CH_MIC` message, because the box writes each chunk to the SCO socket verbatim.
     * `ocbm-proto::SEAM_CODEC_MSBC`.
     */
    const val CODEC_MSBC = 4

    /** mSBC is DEFINED at 16 kHz mono — the rate is part of the codec, not a parameter of it. */
    const val MSBC_RATE = 16000
    const val MSBC_CHANNELS = 1

    /** Bytes of captured PCM one eSCO packet consumes: 120 samples x 2 = 7.5 ms. */
    const val MSBC_PCM_BYTES_PER_PACKET = 240

    /** One eSCO air packet. */
    const val MSBC_PACKET_BYTES = 60

    /**
     * Ceiling on packets drained per 20 ms tick. Steady state is 2.67 (20 / 7.5); the headroom lets
     * a late tick catch up without letting a long stall dump a burst of stale audio at the far end.
     */
    const val MSBC_MAX_PACKETS_PER_TICK = 8

    fun isUplinkCodecSupported(codec: Int): Boolean = codec == CODEC_PCM || codec == CODEC_MSBC

    /**
     * The format capture must actually open at for a negotiated `(rate, channels, codec)`.
     *
     * For PCM it is the negotiated pair. For mSBC it is 16 kHz mono regardless of what the gate said:
     * honouring another rate would encode pitch-shifted speech into a frame the far end decodes as
     * 16 kHz, a mismatch invisible at every later layer. The caller should log when they differ.
     */
    fun captureFormatFor(
        rate: Int,
        channels: Int,
        codec: Int,
    ): Pair<Int, Int> = if (codec == CODEC_MSBC) MSBC_RATE to MSBC_CHANNELS else rate to channels

    /**
     * Max bytes of captured PCM to pull from the ring buffer on one [TICK_MS] tick.
     *
     * PCM: exactly one tick's worth ([chunkBytes]) — the cadence and the chunk are one unit. mSBC:
     * up to [MSBC_MAX_PACKETS_PER_TICK] whole packets; the packetiser only ever consumes multiples of
     * [MSBC_PCM_BYTES_PER_PACKET] and the ring buffer carries the remainder, so the 7.5 ms frame
     * cadence rides on the 20 ms timer without drift.
     */
    fun uplinkDrainBytes(
        rate: Int,
        channels: Int,
        codec: Int,
    ): Int =
        if (codec == CODEC_MSBC) {
            MSBC_PCM_BYTES_PER_PACKET * MSBC_MAX_PACKETS_PER_TICK
        } else {
            chunkBytes(rate, channels)
        }

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
