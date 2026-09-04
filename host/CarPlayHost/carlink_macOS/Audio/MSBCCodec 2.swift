// MSBCCodec.swift — mSBC (HFP 1.6 wideband speech) encoder + decoder, pure Swift, no dependencies.
//
// WHY THIS EXISTS: macOS ships no SBC codec of any kind (CoreAudio has AAC/ALAC/Opus, not SBC), and
// the box deliberately does not grow one — when the AG negotiates wideband, the SCO socket carries
// AIR FRAMES, and `carplay-wireless` forwards each read verbatim as SEAM_PKT_PLAIN under a
// SEAM_FORMAT of codec 4 (`OCBM.seamCodecMsbc`, `ocbm-proto::SEAM_CODEC_MSBC`). Decoding is the
// host's job in both directions: downlink (57-byte frames -> 16 kHz PCM) and uplink (mic PCM ->
// 57-byte frames the box writes straight to the SCO socket). See docs/carplay/01_OCBM_PROTOCOL.md.
//
// WHAT mSBC IS: plain SBC (A2DP 1.3 spec §12.4 "SBC frame format", §12.6 "SBC encoder/decoder")
// with every parameter frozen by HFP 1.6 §5.7.4 / Erratum 2409:
//     16 kHz · MONO · 15 blocks · 8 subbands · LOUDNESS allocation · bitpool 26 · syncword 0xAD
// giving a 57-byte frame that carries exactly 120 samples = 7.5 ms. Frame layout:
//     [0] 0xAD syncword   [1] 0x00   [2] 0x00   [3] CRC-8
//     [4..7]   8 × 4-bit scale factors (one per subband, high nibble first)
//     [8..56]  15 blocks × Σbits(sb) = 390 quantised bits + 2 padding bits
// The two zero bytes are where plain SBC keeps frequency/blocks/mode/allocation/subbands and the
// bitpool; a decoder must reject a frame whose data[1]/data[2] are not zero (they are covered by
// the CRC, so a corrupt one is caught either way).
//
// COEFFICIENT PROVENANCE — read before touching `protoHalf`:
// The 80-tap prototype filter of the SBC polyphase filterbank is a NUMERICALLY DESIGNED filter
// tabulated in the A2DP spec (Appendix B, "proto_8_80"); it has no closed form and cannot be
// re-derived. The 41 half-window values below are that table (it is symmetric about tap 40). They
// were transcribed here from the spec table and then verified, coefficient by coefficient, against
// the Q32 fixed-point transcription used by the public reference decoders
// (ffmpeg `libavcodec/sbcdec_data.h` / BlueZ `sbc`, which agree bit-for-bit with each other):
// all 41 agree to < 1.2e-10, the rounding of a Q32 constant. Two properties pin them further, and
// both are asserted by tests/main.swift:
//   * the unfolded prototype is EXACTLY symmetric (p[n] == p[80-n]);
//   * analysis followed by synthesis reconstructs the input with unity gain at a delay of 73
//     samples and ~65 dB residual — the near-perfect-reconstruction property that only the correct
//     prototype has. A single wrong sign or digit collapses this to noise.
// The stored window is FOLDED — C[n] = p[n]·(-1)^floor(n/16) — because both the analysis and the
// synthesis pseudocode index the modulation matrix by (n mod 16), and cos(θ + 2πj(k+½)) = (-1)^j
// cos(θ) absorbs the block index into the window. The synthesis window is the same table × -8
// (the ×8 is the subband count; the sign makes the round trip polarity-preserving, which is what
// the reference decoder does).
//
// NOT A GENERAL SBC CODEC. Only the mSBC configuration is implemented: no 4-subband mode, no
// stereo/joint-stereo, no SNR allocation, no other bitpool. A general SBC would need the joint
// stereo path and the header parse; nothing on this wire ever produces one.
//
// THREADING: both classes are single-threaded value holders (a filterbank delay line). The decoder
// lives on the audio decrypt queue (OCBMAVBridge), the encoder on the mic IO thread (MicCapture).
// Neither is Sendable and neither needs to be — each is owned by exactly one serial context.

import Foundation

// MARK: - Frozen mSBC parameters

enum MSBC {
    static let syncword: UInt8 = 0xAD
    static let blocks = 15
    static let subbands = 8
    static let bitpool = 26
    static let sampleRate = 16000
    /// 15 blocks × 8 subbands — 7.5 ms at 16 kHz.
    static let samplesPerFrame = 120
    /// 4 header + 4 scale-factor + 49 audio bytes.
    static let frameBytes = 57
    /// One eSCO air packet: 2-byte H2 header + frame + 1 pad. See MSBCFramer.
    static let packetBytes = 60
}

// MARK: - Shared tables

enum MSBCTables {
    /// `proto_8_80`, taps 0…40 (the window is symmetric about tap 40). A2DP spec Appendix B.
    /// Sign pattern: positive through tap 12, negative 13…26, positive 27…40 — the designed
    /// prototype's first sidelobe, not an artefact.
    private static let protoHalf: [Double] = [
        -0.00000000e+00, 1.56575348e-04, 3.43256397e-04, 5.54620055e-04,
        8.23919429e-04, 1.13992509e-03, 1.47640170e-03, 1.78371719e-03,
        2.01182533e-03, 2.10371986e-03, 1.99454557e-03, 1.61656272e-03,
        9.02154483e-04, -1.78805320e-04, -1.64973084e-03, -3.49717448e-03,
        -5.65949455e-03, -8.02941155e-03, -1.04584442e-02, -1.27472337e-02,
        -1.46525260e-02, -1.59045607e-02, -1.62208471e-02, -1.53184105e-02,
        -1.29371807e-02, -8.85757525e-03, -2.92408443e-03, 4.91578039e-03,
        1.46404076e-02, 2.61098761e-02, 3.90751399e-02, 5.31873032e-02,
        6.79989457e-02, 8.29847604e-02, 9.75753888e-02, 1.11196689e-01,
        1.23264551e-01, 1.33264422e-01, 1.40753508e-01, 1.45389840e-01,
        1.46955073e-01,
    ]

    /// The unfolded 80-tap prototype, exposed so a test can assert its symmetry.
    static let proto: [Double] = (0..<80).map { n in n <= 40 ? protoHalf[n] : protoHalf[80 - n] }

    /// Analysis window: the prototype with the block-index sign fold baked in.
    static let analysisWindow: [Double] = (0..<80).map { n in
        (n / 16) % 2 == 0 ? proto[n] : -proto[n]
    }

    /// Synthesis window: `-8 ×` the analysis window (see the header note on the ×8 and the sign).
    static let synthesisWindow: [Double] = analysisWindow.map { -8.0 * $0 }

    /// Analysis matrixing: `M[k][i] = cos((k + ½)(i − 4)π/8)`, k = 0…7 subbands, i = 0…15.
    static let analysisMatrix: [[Double]] = (0..<8).map { k in
        (0..<16).map { i in cos((Double(k) + 0.5) * (Double(i) - 4.0) * .pi / 8.0) }
    }

    /// Synthesis matrixing: `N[k][i] = cos((i + ½)(k + 4)π/8)`, k = 0…15, i = 0…7 subbands.
    static let synthesisMatrix: [[Double]] = (0..<16).map { k in
        (0..<8).map { i in cos((Double(i) + 0.5) * (Double(k) + 4.0) * .pi / 8.0) }
    }

    /// Loudness offsets for 8 subbands, row 0 = 16 kHz (A2DP spec Appendix B). mSBC is 16 kHz only,
    /// so only this row can ever be indexed; the others are omitted rather than left as dead data.
    static let loudnessOffset16k: [Int] = [-2, 0, 0, 0, 0, 0, 0, 1]

    /// CRC-8, polynomial x⁸+x⁴+x³+x²+1 (0x1D), MSB-first, initial value 0x0F (A2DP spec §12.4.2).
    private static let crcTable: [UInt8] = (0..<256).map { i in
        var c = UInt8(i)
        for _ in 0..<8 { c = (c & 0x80) != 0 ? (c << 1) ^ 0x1D : (c << 1) }
        return c
    }

    /// CRC-8 over whole bytes. mSBC's CRC covers exactly 48 bits — data[1], data[2] and the four
    /// packed scale-factor bytes — so the sub-byte tail of the general SBC case cannot arise here.
    static func crc8(_ bytes: [UInt8]) -> UInt8 {
        var c: UInt8 = 0x0F
        for b in bytes { c = crcTable[Int(c ^ b)] }
        return c
    }

    /// The spec's bit-allocation loop, mono + LOUDNESS + 8 subbands (A2DP spec §12.6.3.3).
    /// Encoder and decoder BOTH run it over the transmitted scale factors, which is why a decoder
    /// never has to be told the allocation: it recomputes it.
    static func calculateBits(scaleFactors sf: [Int]) -> [Int] {
        var bitneed = [Int](repeating: 0, count: 8)
        var maxBitneed = 0
        for sb in 0..<8 {
            if sf[sb] == 0 {
                bitneed[sb] = -5
            } else {
                let loudness = sf[sb] - loudnessOffset16k[sb]
                bitneed[sb] = loudness > 0 ? loudness / 2 : loudness
            }
            if bitneed[sb] > maxBitneed { maxBitneed = bitneed[sb] }
        }

        var bitcount = 0
        var slicecount = 0
        var bitslice = maxBitneed + 1
        // Terminates for every one of the 16^8 possible scale-factor sets: each subband contributes
        // to `slicecount` for 13 consecutive bitslice values, so bitcount passes 26 long before the
        // window closes. The iteration cap is a guard against a future edit breaking that property
        // on a hostile frame, not a reachable branch.
        var guardIters = 0
        repeat {
            bitslice -= 1
            bitcount += slicecount
            slicecount = 0
            for sb in 0..<8 {
                if bitneed[sb] > bitslice + 1 && bitneed[sb] < bitslice + 16 {
                    slicecount += 1
                } else if bitneed[sb] == bitslice + 1 {
                    slicecount += 2
                }
            }
            guardIters += 1
        } while bitcount + slicecount < MSBC.bitpool && guardIters < 64

        if bitcount + slicecount == MSBC.bitpool {
            bitcount += slicecount
            bitslice -= 1
        }

        var bits = [Int](repeating: 0, count: 8)
        for sb in 0..<8 {
            if bitneed[sb] < bitslice + 2 {
                bits[sb] = 0
            } else {
                bits[sb] = min(16, bitneed[sb] - bitslice)
            }
        }

        var sb = 0
        while bitcount < MSBC.bitpool && sb < 8 {
            if bits[sb] >= 2 && bits[sb] < 16 {
                bits[sb] += 1
                bitcount += 1
            } else if bitneed[sb] == bitslice + 1 && MSBC.bitpool > bitcount + 1 {
                bits[sb] = 2
                bitcount += 2
            }
            sb += 1
        }
        sb = 0
        while bitcount < MSBC.bitpool && sb < 8 {
            if bits[sb] < 16 {
                bits[sb] += 1
                bitcount += 1
            }
            sb += 1
        }
        return bits
    }
}

// MARK: - Decoder

/// One mSBC decode lane: the 160-sample synthesis delay line plus the frame parse.
/// NOT thread-safe by design — see the threading note in the file header.
final class MSBCDecoder {
    /// Why a frame was rejected. Every case is a PLC event for the caller, but they are worth
    /// telling apart in a log: `crc` is a corrupt air frame, `sync` usually means the H2 framer
    /// mis-locked, and `truncated` means the seam handed us a short payload.
    enum Failure: Error, Equatable {
        case shortFrame
        case badSync
        case reservedHeader
        case crcMismatch
        case truncated
    }

    private var v = [Double](repeating: 0, count: 160)

    /// Drop the filter state (call on stream (re)start; a stale delay line would ring for 10 blocks).
    func reset() { for i in 0..<160 { v[i] = 0 } }

    /// Decode one 57-byte mSBC frame into 120 S16 samples, or fail with a reason.
    func decode(_ frame: ArraySlice<UInt8>) -> Result<[Int16], Failure> {
        guard frame.count >= MSBC.frameBytes else { return .failure(.shortFrame) }
        let f = Array(frame.prefix(MSBC.frameBytes))
        guard f[0] == MSBC.syncword else { return .failure(.badSync) }
        // data[1]/data[2] are where plain SBC carries its parameters; mSBC pins them to zero.
        guard f[1] == 0 && f[2] == 0 else { return .failure(.reservedHeader) }

        var sf = [Int](repeating: 0, count: 8)
        for i in 0..<8 {
            let b = f[4 + i / 2]
            sf[i] = Int(i % 2 == 0 ? (b >> 4) & 0x0F : b & 0x0F)
        }
        // CRC covers data[1], data[2] and the 32 scale-factor bits — 48 bits, byte-aligned.
        guard f[3] == MSBCTables.crc8([f[1], f[2], f[4], f[5], f[6], f[7]]) else {
            return .failure(.crcMismatch)
        }

        let bits = MSBCTables.calculateBits(scaleFactors: sf)
        var reader = MSBCBitReader(f, startBit: 8 * 8) // audio starts after header + scale factors
        var out = [Int16]()
        out.reserveCapacity(MSBC.samplesPerFrame)
        var sub = [Double](repeating: 0, count: 8)

        for _ in 0..<MSBC.blocks {
            for sb in 0..<8 {
                let nb = bits[sb]
                if nb == 0 { sub[sb] = 0; continue }
                guard let q = reader.read(nb) else { return .failure(.truncated) }
                let levels = Double((1 << nb) - 1)
                // Spec §12.6.4.2 dequantisation: 2^(sf+1) · ((2q+1)/levels − 1).
                sub[sb] = Double(1 << (sf[sb] + 1)) * ((2.0 * Double(q) + 1.0) / levels - 1.0)
            }
            synthesize(sub, into: &out)
        }
        return .success(out)
    }

    /// One 8-subband synthesis step: 8 subband samples in, 8 PCM samples out (spec §12.6.4.3).
    private func synthesize(_ s: [Double], into out: inout [Int16]) {
        for i in stride(from: 159, through: 16, by: -1) { v[i] = v[i - 16] }
        for k in 0..<16 {
            let row = MSBCTables.synthesisMatrix[k]
            var acc = 0.0
            for i in 0..<8 { acc += row[i] * s[i] }
            v[k] = acc
        }
        var u = [Double](repeating: 0, count: 80)
        for i in 0..<5 {
            for j in 0..<8 {
                u[i * 16 + j] = v[i * 32 + j]
                u[i * 16 + 8 + j] = v[i * 32 + 24 + j]
            }
        }
        let d = MSBCTables.synthesisWindow
        for j in 0..<8 {
            var acc = 0.0
            for i in 0..<10 { acc += u[j + 8 * i] * d[j + 8 * i] }
            out.append(Int16(clamping: Int(acc.rounded())))
        }
    }
}

// MARK: - Encoder

/// One mSBC encode lane: the 80-sample analysis delay line plus the frame pack.
/// NOT thread-safe by design — see the threading note in the file header.
final class MSBCEncoder {
    private var x = [Double](repeating: 0, count: 80)

    func reset() { for i in 0..<80 { x[i] = 0 } }

    /// Encode exactly 120 samples (7.5 ms at 16 kHz mono) into one 57-byte frame.
    /// Returns nil only for a wrong-sized input — every 120-sample buffer encodes.
    func encode(_ pcm: [Int16]) -> [UInt8]? {
        guard pcm.count == MSBC.samplesPerFrame else { return nil }
        var sub = [[Double]](repeating: [Double](repeating: 0, count: 8), count: MSBC.blocks)
        for b in 0..<MSBC.blocks {
            sub[b] = analyze(pcm, offset: b * 8)
        }

        // Scale factor: the smallest e with max|S| ≤ 2^(e+1), which is the range the dequantiser
        // spans. 4 bits on the wire, so e ∈ 0…15 — and 15 is exactly enough for S16 input.
        var sf = [Int](repeating: 0, count: 8)
        for sb in 0..<8 {
            var maxAbs = 0.0
            for b in 0..<MSBC.blocks { maxAbs = max(maxAbs, abs(sub[b][sb])) }
            var e = 0
            while e < 15 && maxAbs > Double(1 << (e + 1)) { e += 1 }
            sf[sb] = e
        }
        let bits = MSBCTables.calculateBits(scaleFactors: sf)

        var frame = [UInt8](repeating: 0, count: MSBC.frameBytes)
        frame[0] = MSBC.syncword
        frame[1] = 0
        frame[2] = 0
        var writer = MSBCBitWriter(capacityBytes: MSBC.frameBytes - 4)
        for sb in 0..<8 { writer.write(UInt32(sf[sb]), bits: 4) }
        for b in 0..<MSBC.blocks {
            for sb in 0..<8 {
                let nb = bits[sb]
                if nb == 0 { continue }
                let levels = Double((1 << nb) - 1)
                // Spec §12.6.3.4 quantisation, the exact inverse of the dequantiser above.
                let scaled = sub[b][sb] / Double(1 << (sf[sb] + 1))
                let q = ((scaled + 1.0) * levels / 2.0).rounded(.down)
                // Clamp: a subband sample sitting exactly on 2^(sf+1) (or a float ulp past it)
                // would otherwise write levels+1 and overflow into the next field.
                let qi = Int(max(0, min(levels, q)))
                writer.write(UInt32(qi), bits: nb)
            }
        }
        let body = writer.finish()
        for i in 0..<min(body.count, MSBC.frameBytes - 4) { frame[4 + i] = body[i] }
        frame[3] = MSBCTables.crc8([frame[1], frame[2], frame[4], frame[5], frame[6], frame[7]])
        return frame
    }

    /// One 8-subband analysis step (spec §12.6.3.1).
    private func analyze(_ pcm: [Int16], offset: Int) -> [Double] {
        for i in stride(from: 79, through: 8, by: -1) { x[i] = x[i - 8] }
        for i in 0..<8 { x[i] = Double(pcm[offset + 7 - i]) }
        let c = MSBCTables.analysisWindow
        var y = [Double](repeating: 0, count: 16)
        for i in 0..<16 {
            var acc = 0.0
            for j in 0..<5 { acc += c[i + 16 * j] * x[i + 16 * j] }
            y[i] = acc
        }
        var s = [Double](repeating: 0, count: 8)
        for k in 0..<8 {
            let row = MSBCTables.analysisMatrix[k]
            var acc = 0.0
            for i in 0..<16 { acc += row[i] * y[i] }
            s[k] = acc
        }
        return s
    }
}

// MARK: - Bit IO (MSB-first, as SBC packs)

struct MSBCBitReader {
    private let bytes: [UInt8]
    private var pos: Int
    private let end: Int

    init(_ bytes: [UInt8], startBit: Int) {
        self.bytes = bytes
        self.pos = startBit
        self.end = bytes.count * 8
    }

    /// Returns nil rather than trapping when a (corrupt) allocation would read past the frame.
    mutating func read(_ n: Int) -> UInt32? {
        guard n > 0, n <= 16, pos + n <= end else { return nil }
        var v: UInt32 = 0
        for _ in 0..<n {
            v = (v << 1) | UInt32((bytes[pos >> 3] >> (7 - (pos & 7))) & 1)
            pos += 1
        }
        return v
    }
}

struct MSBCBitWriter {
    private var out: [UInt8]
    private var bit = 0

    init(capacityBytes: Int) { out = [UInt8](repeating: 0, count: capacityBytes) }

    mutating func write(_ value: UInt32, bits n: Int) {
        for k in stride(from: n - 1, through: 0, by: -1) {
            let byteIndex = bit >> 3
            guard byteIndex < out.count else { return } // unreachable at bitpool 26; never trap
            if (value >> UInt32(k)) & 1 == 1 { out[byteIndex] |= 1 << (7 - UInt8(bit & 7)) }
            bit += 1
        }
    }

    /// The remaining bits of the last byte are already zero — SBC's padding is defined as zero.
    func finish() -> [UInt8] { out }
}
