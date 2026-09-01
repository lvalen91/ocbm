// AVCCFastPath.swift — pure, dependency-free helpers for the AVCC fast path (V1) and the
// depth-limited decode/enqueue backpressure slot (V4).
//
// These are deliberately VideoToolbox-/AVFoundation-free (Foundation only) so the hardware-free
// swiftc test harness (tests/main.swift, compiled via tests/run_tests.sh) can exercise them without
// dragging in the video framework stack. VideoDecoder / OCBMAVBridge call straight into them.
//
// V1 rationale: when the decrypted access unit already uses 4-byte NAL length prefixes (iOS always
// sends 4; the box refines nalLengthSize from the avcC), the plaintext is byte-identical to the
// CMBlockBuffer payload VideoToolbox needs — so the steady-state frame needs zero format conversion.
// `walkAVCC` does one cheap validating pass over the length prefixes to (a) confirm structure and
// (b) sniff whether in-band SPS/PPS are present (mid-stream format change) and whether the AU carries
// a keyframe. Only when the length size is not 4 do we rewrite (`rewriteToFourByteLengths`), and only
// on format-change frames do we take the extra classification copy.

import Foundation

enum AVCCFastPath {

    // MARK: - Annex-B → AVCC (Android Auto path)

    /// Split an Annex-B byte stream into NAL payload ranges (start codes stripped).
    /// Handles both 3-byte (00 00 01) and 4-byte (00 00 00 01) start codes. Each NAL
    /// ends exactly where the next start code begins, so a NAL whose data legitimately
    /// ends in 0x00 is not truncated.
    static func annexBNALRanges(_ b: UnsafeRawBufferPointer) -> [Range<Int>] {
        let n = b.count
        // Record each start code: (scStart = where 00 00 (00) 01 begins, payload = after it).
        var codes: [(sc: Int, payload: Int)] = []
        var i = 0
        while i + 3 <= n {
            if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 {
                let sc = (i > 0 && b[i - 1] == 0) ? i - 1 : i // 4-byte code if a leading 0 precedes
                codes.append((sc, i + 3))
                i += 3
            } else {
                i += 1
            }
        }
        var ranges: [Range<Int>] = []
        for (k, c) in codes.enumerated() {
            let end = (k + 1 < codes.count) ? codes[k + 1].sc : n
            if end > c.payload { ranges.append(c.payload..<end) }
        }
        return ranges
    }

    /// Convert an Annex-B access unit (Android Auto's on-wire H.264) into a fresh
    /// AVCC buffer with 4-byte big-endian NAL length prefixes — the exact payload
    /// `VideoDecoder.decodeAndDisplay(avcc:)` expects. Returns nil if no NALs found.
    static func annexBToAVCC(_ annexb: Data) -> Data? {
        annexb.withUnsafeBytes { (raw: UnsafeRawBufferPointer) -> Data? in
            let ranges = annexBNALRanges(raw)
            guard !ranges.isEmpty, let base = raw.baseAddress else { return nil }
            let outCount = ranges.reduce(0) { $0 + 4 + $1.count }
            var out = Data(count: outCount)
            out.withUnsafeMutableBytes { (dst: UnsafeMutableRawBufferPointer) in
                let o = dst.baseAddress!.assumingMemoryBound(to: UInt8.self)
                var di = 0
                for r in ranges {
                    let len = r.count
                    o[di] = UInt8((len >> 24) & 0xFF)
                    o[di + 1] = UInt8((len >> 16) & 0xFF)
                    o[di + 2] = UInt8((len >> 8) & 0xFF)
                    o[di + 3] = UInt8(len & 0xFF)
                    di += 4
                    memcpy(dst.baseAddress! + di, base + r.lowerBound, len)
                    di += len
                }
            }
            return out
        }
    }

    /// Extract the H.264 SPS (nal_unit_type 7) and PPS (8) payloads (no start codes)
    /// from an Annex-B config blob (Android Auto's MEDIA_CODEC_CONFIG), for
    /// `VideoDecoder.configure(codec: .h264, parameterSets: [SPS, PPS])`.
    static func h264ParameterSetsFromAnnexB(_ annexb: Data) -> (sps: Data, pps: Data)? {
        return annexb.withUnsafeBytes { (raw: UnsafeRawBufferPointer) -> (Data, Data)? in
            let b = raw.bindMemory(to: UInt8.self)
            var sps: Data?
            var pps: Data?
            for r in annexBNALRanges(raw) where !r.isEmpty {
                switch b[r.lowerBound] & 0x1F {
                case 7: sps = Data(bytes: raw.baseAddress! + r.lowerBound, count: r.count)
                case 8: pps = Data(bytes: raw.baseAddress! + r.lowerBound, count: r.count)
                default: break
                }
            }
            if let s = sps, let p = pps { return (s, p) }
            return nil
        }
    }

    // MARK: - V1 length-prefix walk

    /// The result of one validating pass over a 4-byte-length-prefixed AVCC access unit.
    struct Walk: Equatable {
        /// Structure is well-formed: every NAL length > 0, every NAL fully present, and the prefixes
        /// consume the buffer EXACTLY (no truncated final NAL, no trailing garbage).
        var valid: Bool
        /// An in-band parameter-set NAL (H.264 SPS/PPS 7/8, HEVC VPS/SPS/PPS 32/33/34) is present —
        /// this AU may carry a mid-stream format change and must go through the classify/diff path.
        var hasParamSets: Bool
        /// A keyframe-class NAL is present (H.264 IDR type 5; HEVC IRAP types 16…21).
        var containsIDR: Bool
    }

    /// One cheap pass over the 4-byte BE length prefixes. NO copies, NO allocation.
    /// `isHEVC` selects the NAL-type decode (H.264 `b & 0x1F` vs HEVC `(b >> 1) & 0x3F`).
    static func walkAVCC(_ p: UnsafeRawBufferPointer, isHEVC: Bool) -> Walk {
        let b = p.bindMemory(to: UInt8.self)
        let count = b.count
        var i = 0
        var hasParamSets = false
        var containsIDR = false
        var sawNAL = false
        while i + 4 <= count {
            let len = (Int(b[i]) << 24) | (Int(b[i + 1]) << 16) | (Int(b[i + 2]) << 8) | Int(b[i + 3])
            // len <= 0 is a zero-length NAL; i+4+len > count is a truncated final NAL. Either refuses.
            if len <= 0 || i + 4 + len > count {
                return Walk(valid: false, hasParamSets: hasParamSets, containsIDR: containsIDR)
            }
            let header = b[i + 4] // guaranteed in-bounds: len >= 1 ⇒ i+4 < count
            if isHEVC {
                let t = (header >> 1) & 0x3F
                if t == 32 || t == 33 || t == 34 { hasParamSets = true }
                if (16...21).contains(t) { containsIDR = true }
            } else {
                let t = header & 0x1F
                if t == 7 || t == 8 { hasParamSets = true }
                if t == 5 { containsIDR = true }
            }
            sawNAL = true
            i += 4 + len
        }
        // Exact consumption: any leftover (< 4 stray bytes, or an overshooting length caught above)
        // means the frame is malformed. Require at least one NAL.
        return Walk(valid: sawNAL && i == count, hasParamSets: hasParamSets, containsIDR: containsIDR)
    }

    /// Collect the byte ranges of the NAL PAYLOADS (excluding each 4-byte length prefix) from a
    /// buffer already validated by `walkAVCC`. Used only on the format-change path, so the small
    /// allocation is off the steady-state hot path. Offsets are into the raw buffer.
    static func nalPayloadRanges(_ p: UnsafeRawBufferPointer) -> [Range<Int>] {
        let b = p.bindMemory(to: UInt8.self)
        let count = b.count
        var ranges: [Range<Int>] = []
        var i = 0
        while i + 4 <= count {
            let len = (Int(b[i]) << 24) | (Int(b[i + 1]) << 16) | (Int(b[i + 2]) << 8) | Int(b[i + 3])
            if len <= 0 || i + 4 + len > count { break }
            ranges.append((i + 4)..<(i + 4 + len))
            i += 4 + len
        }
        return ranges
    }

    // MARK: - V1 length-size rewrite (non-4-byte prefixes)

    /// Rewrite an access unit whose NAL length prefixes are `lenSize` (1/2/3) bytes into a fresh
    /// 4-byte-length-prefixed buffer — NO Annex-B intermediate. Two passes: count+validate, then
    /// write 4-byte BE lengths + memcpy each payload. Returns nil on malformed input.
    static func rewriteToFourByteLengths(_ avcc: Data, lenSize: Int) -> Data? {
        guard lenSize >= 1, lenSize <= 4 else { return nil }
        return avcc.withUnsafeBytes { (raw: UnsafeRawBufferPointer) -> Data? in
            let b = raw.bindMemory(to: UInt8.self)
            let count = b.count
            // Pass 1: count NALs and validate structure with `lenSize` prefixes.
            var i = 0
            var nalCount = 0
            while i + lenSize <= count {
                var len = 0
                for k in 0..<lenSize { len = (len << 8) | Int(b[i + k]) }
                i += lenSize
                if len <= 0 || i + len > count { return nil }
                i += len
                nalCount += 1
            }
            guard nalCount > 0, i == count else { return nil }

            // Pass 2: write into a right-sized buffer (each prefix grows by 4 - lenSize bytes).
            let outCount = count + nalCount * (4 - lenSize)
            var out = Data(count: outCount)
            out.withUnsafeMutableBytes { (dst: UnsafeMutableRawBufferPointer) in
                guard let o = dst.baseAddress, let src = raw.baseAddress else { return }
                var si = 0, di = 0
                while si + lenSize <= count {
                    var len = 0
                    for k in 0..<lenSize { len = (len << 8) | Int(b[si + k]) }
                    si += lenSize
                    let dp = o.assumingMemoryBound(to: UInt8.self)
                    dp[di]     = UInt8((len >> 24) & 0xFF)
                    dp[di + 1] = UInt8((len >> 16) & 0xFF)
                    dp[di + 2] = UInt8((len >> 8) & 0xFF)
                    dp[di + 3] = UInt8(len & 0xFF)
                    di += 4
                    memcpy(o + di, src + si, len)
                    di += len
                    si += len
                }
            }
            return out
        }
    }

    // MARK: - V4 single-slot latest-wins drop table

    /// Decide what a new frame does to a single-slot latest-wins handoff, given whether the frame
    /// currently occupying the slot is a keyframe (`oldIsKF == nil` ⇒ slot empty) and whether the
    /// incoming frame is a keyframe. Pure, so the whole table is unit-testable in isolation.
    ///
    /// - `store`: put the NEW frame in the slot (replacing the old one, or filling an empty slot).
    ///   `false` means DROP the new frame and leave the slot untouched.
    /// - `requestKeyframe`: a P-frame's decode chain was broken (a frame was dropped) — ask the box
    ///   for a fresh IDR so the decoder can re-sync.
    static func resolveSlot(oldIsKF: Bool?, newIsKF: Bool) -> (store: Bool, requestKeyframe: Bool) {
        guard let old = oldIsKF else { return (true, false) }   // empty → store
        switch (old, newIsKF) {
        case (false, false): return (true, true)    // P over P    → replace; chain broken → request KF
        case (false, true):  return (true, false)   // IDR over P  → replace; the IDR repaints cleanly
        case (true, false):  return (false, true)   // P over IDR  → DROP new P (never displace an
                                                     //               unseen IDR); request KF
        case (true, true):   return (true, false)   // IDR over IDR → replace; newer IDR still repaints
        }
    }
}
