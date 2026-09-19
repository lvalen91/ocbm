package com.carlink.telephony

import kotlin.math.PI
import kotlin.math.abs
import kotlin.math.cos
import kotlin.math.floor

/**
 * mSBC (HFP 1.6 wideband speech) encoder + decoder, pure Kotlin, no dependencies.
 *
 * A line-for-line port of the proven macOS implementation
 * (`host/MacHost/carlink_macOS/Audio/MSBCCodec.swift`), which is pinned there against an
 * independent fixed-point decoder (ffmpeg `sbc_synthesize_eight`). The same reference bitstream
 * and PCM vector are asserted by `MsbcCodecTest` here, so the port cannot drift from the original
 * without failing.
 *
 * WHY THIS EXISTS: Android ships no SBC decoder a third-party app can reach for a raw bitstream
 * (the Bluetooth stack's is behind the HFP profile, not an API), and the box deliberately does not
 * grow one — when the AG negotiates wideband, the SCO socket carries AIR FRAMES and `btd` forwards
 * each read verbatim as `SEAM_PKT_PLAIN` under a `SEAM_FORMAT` of codec 4
 * (`ocbm-proto::SEAM_CODEC_MSBC`). Decoding is the host's job in both directions: downlink
 * (57-byte frames -> 16 kHz PCM) and uplink (mic PCM -> 57-byte frames the box writes straight to
 * the SCO socket).
 *
 * WHAT mSBC IS: plain SBC (A2DP 1.3 §12.4 / §12.6) with every parameter frozen by HFP 1.6 §5.7.4:
 * 16 kHz · MONO · 15 blocks · 8 subbands · LOUDNESS allocation · bitpool 26 · syncword 0xAD,
 * giving a 57-byte frame that carries exactly 120 samples = 7.5 ms:
 * ```
 *   [0] 0xAD syncword   [1] 0x00   [2] 0x00   [3] CRC-8
 *   [4..7]   8 x 4-bit scale factors (one per subband, high nibble first)
 *   [8..56]  15 blocks x sum(bits[sb]) = 390 quantised bits + 2 padding bits
 * ```
 * The two zero bytes are where plain SBC keeps its parameters; a decoder must reject a frame whose
 * data[1]/data[2] are not zero.
 *
 * COEFFICIENT PROVENANCE: the 80-tap prototype (`proto_8_80`, A2DP spec Appendix B) is a
 * numerically designed filter with no closed form. The 41 half-window values are the spec table,
 * verified in the macOS tree coefficient-by-coefficient against the Q32 tables in ffmpeg/BlueZ. Two
 * properties pin them and are asserted by the tests: exact symmetry about tap 40, and near-perfect
 * reconstruction (analysis then synthesis reproduces the input at a delay of 73 samples).
 *
 * NOT A GENERAL SBC CODEC — only the mSBC configuration. Nothing on this wire produces another.
 *
 * THREADING: the codec objects are single-threaded value holders (a filterbank delay line each).
 * The downlink decoder is owned by `AudioSeam` (one per scid, on the OCBM read thread); the uplink
 * encoder by `MicrophoneCaptureManager` (on the mic send thread). Neither is shared.
 */
object Msbc {
    const val SYNCWORD: Int = 0xAD
    const val BLOCKS: Int = 15
    const val SUBBANDS: Int = 8
    const val BITPOOL: Int = 26
    const val SAMPLE_RATE: Int = 16000
    const val CHANNELS: Int = 1

    /** 15 blocks x 8 subbands — 7.5 ms at 16 kHz. */
    const val SAMPLES_PER_FRAME: Int = 120

    /** 4 header + 4 scale-factor + 49 audio bytes. */
    const val FRAME_BYTES: Int = 57

    /** One eSCO air packet: 2-byte H2 header + frame + 1 pad. See [MsbcFramer]. */
    const val PACKET_BYTES: Int = 60

    /** S16 bytes in one frame: 120 samples x 2. */
    const val PCM_BYTES_PER_FRAME: Int = SAMPLES_PER_FRAME * 2

    /** The filterbank's analysis+synthesis reconstruction delay, in samples. */
    const val RECONSTRUCTION_DELAY: Int = 73
}

object MsbcTables {
    /** `proto_8_80`, taps 0..40 (the window is symmetric about tap 40). A2DP spec Appendix B. */
    private val PROTO_HALF =
        doubleArrayOf(
            -0.00000000e+00,
            1.56575348e-04,
            3.43256397e-04,
            5.54620055e-04,
            8.23919429e-04,
            1.13992509e-03,
            1.47640170e-03,
            1.78371719e-03,
            2.01182533e-03,
            2.10371986e-03,
            1.99454557e-03,
            1.61656272e-03,
            9.02154483e-04,
            -1.78805320e-04,
            -1.64973084e-03,
            -3.49717448e-03,
            -5.65949455e-03,
            -8.02941155e-03,
            -1.04584442e-02,
            -1.27472337e-02,
            -1.46525260e-02,
            -1.59045607e-02,
            -1.62208471e-02,
            -1.53184105e-02,
            -1.29371807e-02,
            -8.85757525e-03,
            -2.92408443e-03,
            4.91578039e-03,
            1.46404076e-02,
            2.61098761e-02,
            3.90751399e-02,
            5.31873032e-02,
            6.79989457e-02,
            8.29847604e-02,
            9.75753888e-02,
            1.11196689e-01,
            1.23264551e-01,
            1.33264422e-01,
            1.40753508e-01,
            1.45389840e-01,
            1.46955073e-01,
        )

    /** The unfolded 80-tap prototype, exposed so a test can assert its symmetry. */
    val proto: DoubleArray = DoubleArray(80) { n -> if (n <= 40) PROTO_HALF[n] else PROTO_HALF[80 - n] }

    /** Analysis window: the prototype with the block-index sign fold `C[n] = p[n] * (-1)^floor(n/16)`. */
    val analysisWindow: DoubleArray = DoubleArray(80) { n -> if ((n / 16) % 2 == 0) proto[n] else -proto[n] }

    /** Synthesis window: `-8 x` the analysis window (x8 = subband count; the sign keeps polarity). */
    val synthesisWindow: DoubleArray = DoubleArray(80) { -8.0 * analysisWindow[it] }

    /** Analysis matrixing: `M[k][i] = cos((k + 1/2)(i - 4) pi/8)`, k = 0..7 subbands, i = 0..15. */
    val analysisMatrix: Array<DoubleArray> =
        Array(8) { k -> DoubleArray(16) { i -> cos((k + 0.5) * (i - 4.0) * PI / 8.0) } }

    /** Synthesis matrixing: `N[k][i] = cos((i + 1/2)(k + 4) pi/8)`, k = 0..15, i = 0..7 subbands. */
    val synthesisMatrix: Array<DoubleArray> =
        Array(16) { k -> DoubleArray(8) { i -> cos((i + 0.5) * (k + 4.0) * PI / 8.0) } }

    /** Loudness offsets for 8 subbands at 16 kHz (A2DP spec Appendix B). mSBC is 16 kHz only. */
    val LOUDNESS_OFFSET_16K: IntArray = intArrayOf(-2, 0, 0, 0, 0, 0, 0, 1)

    /** CRC-8, polynomial x^8+x^4+x^3+x^2+1 (0x1D), MSB-first, initial value 0x0F (A2DP §12.4.2). */
    private val CRC_TABLE =
        IntArray(256) { i ->
            var c = i
            repeat(8) { c = if (c and 0x80 != 0) ((c shl 1) xor 0x1D) and 0xFF else (c shl 1) and 0xFF }
            c
        }

    /**
     * CRC-8 over whole bytes. mSBC's CRC covers exactly 48 bits — data[1], data[2] and the four
     * packed scale-factor bytes — so the sub-byte tail of the general SBC case cannot arise here.
     */
    fun crc8(vararg bytes: Int): Int {
        var c = 0x0F
        for (b in bytes) c = CRC_TABLE[c xor (b and 0xFF)]
        return c
    }

    /**
     * The spec's bit-allocation loop, mono + LOUDNESS + 8 subbands (A2DP §12.6.3.3). Encoder and
     * decoder BOTH run it over the transmitted scale factors, which is why a decoder never has to
     * be told the allocation: it recomputes it.
     *
     * Complexity suppressed on purpose: this is the spec pseudocode transcribed step for step, and
     * it is pinned bit-for-bit against a reference bitstream. Splitting it into readable pieces
     * would hide the correspondence that makes it checkable.
     */
    @Suppress("CyclomaticComplexMethod")
    fun calculateBits(sf: IntArray): IntArray {
        val bitneed = IntArray(8)
        var maxBitneed = 0
        for (sb in 0 until 8) {
            if (sf[sb] == 0) {
                bitneed[sb] = -5
            } else {
                val loudness = sf[sb] - LOUDNESS_OFFSET_16K[sb]
                bitneed[sb] = if (loudness > 0) loudness / 2 else loudness
            }
            if (bitneed[sb] > maxBitneed) maxBitneed = bitneed[sb]
        }

        var bitcount = 0
        var slicecount = 0
        var bitslice = maxBitneed + 1
        // Terminates for every one of the 16^8 possible scale-factor sets (each subband contributes
        // for 13 consecutive bitslice values, so bitcount passes 26 long before the window closes).
        // The iteration cap guards a future edit on a hostile frame, not a reachable branch.
        var guardIters = 0
        do {
            bitslice -= 1
            bitcount += slicecount
            slicecount = 0
            for (sb in 0 until 8) {
                if (bitneed[sb] > bitslice + 1 && bitneed[sb] < bitslice + 16) {
                    slicecount += 1
                } else if (bitneed[sb] == bitslice + 1) {
                    slicecount += 2
                }
            }
            guardIters += 1
        } while (bitcount + slicecount < Msbc.BITPOOL && guardIters < 64)

        if (bitcount + slicecount == Msbc.BITPOOL) {
            bitcount += slicecount
            bitslice -= 1
        }

        val bits = IntArray(8)
        for (sb in 0 until 8) {
            bits[sb] = if (bitneed[sb] < bitslice + 2) 0 else minOf(16, bitneed[sb] - bitslice)
        }

        var sb = 0
        while (bitcount < Msbc.BITPOOL && sb < 8) {
            if (bits[sb] >= 2 && bits[sb] < 16) {
                bits[sb] += 1
                bitcount += 1
            } else if (bitneed[sb] == bitslice + 1 && Msbc.BITPOOL > bitcount + 1) {
                bits[sb] = 2
                bitcount += 2
            }
            sb += 1
        }
        sb = 0
        while (bitcount < Msbc.BITPOOL && sb < 8) {
            if (bits[sb] < 16) {
                bits[sb] += 1
                bitcount += 1
            }
            sb += 1
        }
        return bits
    }

    /** Swift's `.rounded()` — half away from zero — so decoded samples match the reference bit-for-bit. */
    internal fun roundHalfAway(x: Double): Int = if (x >= 0) floor(x + 0.5).toInt() else -floor(-x + 0.5).toInt()

    internal fun clampS16(v: Int): Short = v.coerceIn(Short.MIN_VALUE.toInt(), Short.MAX_VALUE.toInt()).toShort()
}

/**
 * One mSBC decode lane: the 160-sample synthesis delay line plus the frame parse.
 * NOT thread-safe by design — see the threading note on [Msbc].
 */
class MsbcDecoder {
    /**
     * Why a frame was rejected. Every case is a PLC event for the caller, but they are worth
     * telling apart in a log: [CRC_MISMATCH] is a corrupt air frame, [BAD_SYNC] usually means the
     * H2 framer mis-locked, and [TRUNCATED] means the seam handed us a short payload.
     */
    enum class Failure { SHORT_FRAME, BAD_SYNC, RESERVED_HEADER, CRC_MISMATCH, TRUNCATED }

    private val v = DoubleArray(160)
    private val u = DoubleArray(80)
    private val sub = DoubleArray(8)
    private val sf = IntArray(8)

    /** Set by [decode] when it returns null. */
    var lastFailure: Failure? = null
        private set

    /** Drop the filter state (call on stream (re)start; a stale delay line would ring for 10 blocks). */
    fun reset() {
        v.fill(0.0)
    }

    /** Decode one 57-byte mSBC frame at `frame[off]` into 120 S16 samples, or null with [lastFailure] set. */
    fun decode(
        frame: ByteArray,
        off: Int = 0,
        len: Int = frame.size - off,
    ): ShortArray? {
        val why = parseHeader(frame, off, len)
        if (why != null) return fail(why)

        val bits = MsbcTables.calculateBits(sf)
        val reader = MsbcBitReader(frame, off, Msbc.FRAME_BYTES, startBit = 8 * 8)
        val out = ShortArray(Msbc.SAMPLES_PER_FRAME)
        var outPos = 0
        repeat(Msbc.BLOCKS) {
            for (sb in 0 until 8) {
                val nb = bits[sb]
                if (nb == 0) {
                    sub[sb] = 0.0
                    continue
                }
                val q = reader.read(nb)
                if (q < 0) return fail(Failure.TRUNCATED)
                val levels = ((1 shl nb) - 1).toDouble()
                // Spec §12.6.4.2 dequantisation: 2^(sf+1) * ((2q+1)/levels - 1).
                sub[sb] = (1 shl (sf[sb] + 1)).toDouble() * ((2.0 * q + 1.0) / levels - 1.0)
            }
            synthesize(sub, out, outPos)
            outPos += 8
        }
        lastFailure = null
        return out
    }

    /** Validate the 8-byte header and unpack the scale factors into [sf]; null = well-formed. */
    private fun parseHeader(
        frame: ByteArray,
        off: Int,
        len: Int,
    ): Failure? {
        if (len < Msbc.FRAME_BYTES) return Failure.SHORT_FRAME

        fun f(i: Int) = frame[off + i].toInt() and 0xFF
        if (f(0) != Msbc.SYNCWORD) return Failure.BAD_SYNC
        // data[1]/data[2] are where plain SBC carries its parameters; mSBC pins them to zero.
        if (f(1) != 0 || f(2) != 0) return Failure.RESERVED_HEADER
        for (i in 0 until 8) {
            val b = f(4 + i / 2)
            sf[i] = if (i % 2 == 0) (b shr 4) and 0x0F else b and 0x0F
        }
        // CRC covers data[1], data[2] and the 32 scale-factor bits — 48 bits, byte-aligned.
        return if (f(3) != MsbcTables.crc8(f(1), f(2), f(4), f(5), f(6), f(7))) Failure.CRC_MISMATCH else null
    }

    private fun fail(why: Failure): ShortArray? {
        lastFailure = why
        return null
    }

    /** One 8-subband synthesis step: 8 subband samples in, 8 PCM samples out (spec §12.6.4.3). */
    private fun synthesize(
        s: DoubleArray,
        out: ShortArray,
        outPos: Int,
    ) {
        for (i in 159 downTo 16) v[i] = v[i - 16]
        for (k in 0 until 16) {
            val row = MsbcTables.synthesisMatrix[k]
            var acc = 0.0
            for (i in 0 until 8) acc += row[i] * s[i]
            v[k] = acc
        }
        for (i in 0 until 5) {
            for (j in 0 until 8) {
                u[i * 16 + j] = v[i * 32 + j]
                u[i * 16 + 8 + j] = v[i * 32 + 24 + j]
            }
        }
        val d = MsbcTables.synthesisWindow
        for (j in 0 until 8) {
            var acc = 0.0
            for (i in 0 until 10) acc += u[j + 8 * i] * d[j + 8 * i]
            out[outPos + j] = MsbcTables.clampS16(MsbcTables.roundHalfAway(acc))
        }
    }
}

/**
 * One mSBC encode lane: the 80-sample analysis delay line plus the frame pack.
 * NOT thread-safe by design — see the threading note on [Msbc].
 */
class MsbcEncoder {
    private val x = DoubleArray(80)
    private val y = DoubleArray(16)
    private val sub = Array(Msbc.BLOCKS) { DoubleArray(8) }
    private val sf = IntArray(8)

    fun reset() {
        x.fill(0.0)
    }

    /**
     * Encode exactly 120 samples (7.5 ms at 16 kHz mono) into one 57-byte frame.
     * Returns null only for a wrong-sized input — every 120-sample buffer encodes.
     */
    fun encode(
        pcm: ShortArray,
        off: Int = 0,
        len: Int = pcm.size - off,
    ): ByteArray? {
        if (len != Msbc.SAMPLES_PER_FRAME) return null
        for (b in 0 until Msbc.BLOCKS) analyze(pcm, off + b * 8, sub[b])

        // Scale factor: the smallest e with max|S| <= 2^(e+1), which is the range the dequantiser
        // spans. 4 bits on the wire, so e in 0..15 — and 15 is exactly enough for S16 input.
        for (sb in 0 until 8) {
            var maxAbs = 0.0
            for (b in 0 until Msbc.BLOCKS) maxAbs = maxOf(maxAbs, abs(sub[b][sb]))
            var e = 0
            while (e < 15 && maxAbs > (1 shl (e + 1)).toDouble()) e += 1
            sf[sb] = e
        }
        val bits = MsbcTables.calculateBits(sf)

        val frame = ByteArray(Msbc.FRAME_BYTES)
        frame[0] = Msbc.SYNCWORD.toByte()
        frame[1] = 0
        frame[2] = 0
        val writer = MsbcBitWriter(frame, 4, Msbc.FRAME_BYTES - 4)
        for (sb in 0 until 8) writer.write(sf[sb], 4)
        for (b in 0 until Msbc.BLOCKS) {
            for (sb in 0 until 8) {
                val nb = bits[sb]
                if (nb == 0) continue
                val levels = ((1 shl nb) - 1).toDouble()
                // Spec §12.6.3.4 quantisation, the exact inverse of the dequantiser.
                val scaled = sub[b][sb] / (1 shl (sf[sb] + 1)).toDouble()
                val q = floor((scaled + 1.0) * levels / 2.0)
                // Clamp: a subband sample sitting exactly on 2^(sf+1) (or a float ulp past it)
                // would otherwise write levels+1 and overflow into the next field.
                val qi = maxOf(0.0, minOf(levels, q)).toInt()
                writer.write(qi, nb)
            }
        }
        frame[3] =
            MsbcTables
                .crc8(
                    frame[1].toInt(),
                    frame[2].toInt(),
                    frame[4].toInt(),
                    frame[5].toInt(),
                    frame[6].toInt(),
                    frame[7].toInt(),
                ).toByte()
        return frame
    }

    /** One 8-subband analysis step (spec §12.6.3.1). */
    private fun analyze(
        pcm: ShortArray,
        offset: Int,
        s: DoubleArray,
    ) {
        for (i in 79 downTo 8) x[i] = x[i - 8]
        for (i in 0 until 8) x[i] = pcm[offset + 7 - i].toDouble()
        val c = MsbcTables.analysisWindow
        for (i in 0 until 16) {
            var acc = 0.0
            for (j in 0 until 5) acc += c[i + 16 * j] * x[i + 16 * j]
            y[i] = acc
        }
        for (k in 0 until 8) {
            val row = MsbcTables.analysisMatrix[k]
            var acc = 0.0
            for (i in 0 until 16) acc += row[i] * y[i]
            s[k] = acc
        }
    }
}

// ---- Bit IO (MSB-first, as SBC packs) ------------------------------------------------------------

internal class MsbcBitReader(
    private val bytes: ByteArray,
    private val base: Int,
    lenBytes: Int,
    startBit: Int,
) {
    private var pos = startBit
    private val end = lenBytes * 8

    /** Returns -1 rather than throwing when a (corrupt) allocation would read past the frame. */
    fun read(n: Int): Int {
        if (n <= 0 || n > 16 || pos + n > end) return -1
        var v = 0
        repeat(n) {
            v = (v shl 1) or ((bytes[base + (pos shr 3)].toInt() shr (7 - (pos and 7))) and 1)
            pos += 1
        }
        return v
    }
}

internal class MsbcBitWriter(
    private val out: ByteArray,
    private val base: Int,
    private val capacityBytes: Int,
) {
    private var bit = 0

    fun write(
        value: Int,
        n: Int,
    ) {
        for (k in n - 1 downTo 0) {
            val byteIndex = bit shr 3
            if (byteIndex >= capacityBytes) return // unreachable at bitpool 26; never throw
            if ((value shr k) and 1 == 1) {
                val idx = base + byteIndex
                out[idx] = (out[idx].toInt() or (1 shl (7 - (bit and 7)))).toByte()
            }
            bit += 1
        }
    }
}
