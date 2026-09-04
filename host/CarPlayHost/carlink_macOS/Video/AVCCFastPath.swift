// AVCCFastPath.swift — pure, dependency-free helpers for the AVCC fast path (V1) and the
// decode/enqueue backpressure queue (V5 `FrameFIFO`; V4's single-slot `resolveSlot` table is retained
// as the reference oracle for the depth-1 equivalence test).
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
    /// Handles both 3-byte (00 00 01) and 4-byte (00 00 00 01) start codes. Each NAL ends where the
    /// next start code begins — INCLUDING the leading 0x00 of a 4-byte code, so a zero immediately
    /// before the next start code is treated as part of that code, not as the previous NAL's last
    /// byte. That is correct: an Annex-B NAL never ends in 0x00 (trailing zeros belong to the byte
    /// stream), and VideoToolbox ignores the extra zeros left on the previous NAL by a 5+-byte code.
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

    /// HEVC Annex-B parameter sets: VPS (32), SPS (33), PPS (34), NAL type = (b >> 1) & 0x3F.
    static func hevcParameterSetsFromAnnexB(_ annexb: Data) -> (vps: Data, sps: Data, pps: Data)? {
        return annexb.withUnsafeBytes { (raw: UnsafeRawBufferPointer) -> (Data, Data, Data)? in
            let b = raw.bindMemory(to: UInt8.self)
            var vps: Data?, sps: Data?, pps: Data?
            for r in annexBNALRanges(raw) where !r.isEmpty {
                let d = Data(bytes: raw.baseAddress! + r.lowerBound, count: r.count)
                switch (b[r.lowerBound] >> 1) & 0x3F {
                case 32: vps = d
                case 33: sps = d
                case 34: pps = d
                default: break
                }
            }
            if let v = vps, let s = sps, let p = pps { return (v, s, p) }
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
        /// Bitmask of every NAL type seen in this AU (bit N ⇒ type N). H.264 types are 0…31, HEVC
        /// 0…63 — both fit a UInt64. DIAGNOSTICS ONLY: the walk already reads each NAL header, so
        /// recording it is free, and it lets the decoder name the NAL types of a frame VideoToolbox
        /// reported decode failures for WITHOUT a second pass over the (possibly freed) payload.
        /// Defaulted so existing `Walk(valid:hasParamSets:containsIDR:)` call sites are unaffected.
        var nalTypeMask: UInt64 = 0
    }

    /// Render a `Walk.nalTypeMask` as a compact ascending type list ("32,33,34,19"), for logs.
    static func nalTypeSummary(_ mask: UInt64) -> String {
        var out: [String] = []
        for t in 0..<64 where mask & (1 << UInt64(t)) != 0 { out.append(String(t)) }
        return out.isEmpty ? "-" : out.joined(separator: ",")
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
        var nalTypeMask: UInt64 = 0
        while i + 4 <= count {
            let len = (Int(b[i]) << 24) | (Int(b[i + 1]) << 16) | (Int(b[i + 2]) << 8) | Int(b[i + 3])
            // len <= 0 is a zero-length NAL; i+4+len > count is a truncated final NAL. Either refuses.
            if len <= 0 || i + 4 + len > count {
                return Walk(valid: false, hasParamSets: hasParamSets, containsIDR: containsIDR,
                            nalTypeMask: nalTypeMask)
            }
            let header = b[i + 4] // guaranteed in-bounds: len >= 1 ⇒ i+4 < count
            if isHEVC {
                let t = (header >> 1) & 0x3F
                if t == 32 || t == 33 || t == 34 { hasParamSets = true }
                if (16...21).contains(t) { containsIDR = true }
                nalTypeMask |= 1 << UInt64(t)
            } else {
                let t = header & 0x1F
                if t == 7 || t == 8 { hasParamSets = true }
                if t == 5 { containsIDR = true }
                nalTypeMask |= 1 << UInt64(t)
            }
            sawNAL = true
            i += 4 + len
        }
        // Exact consumption: any leftover (< 4 stray bytes, or an overshooting length caught above)
        // means the frame is malformed. Require at least one NAL.
        return Walk(valid: sawNAL && i == count, hasParamSets: hasParamSets, containsIDR: containsIDR,
                    nalTypeMask: nalTypeMask)
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

    // MARK: - V4 single-slot latest-wins drop table (superseded by FrameFIFO; kept as the oracle)
    //
    // SUPERSEDED 2026-09-03 by `FrameFIFO` below — the decoder no longer calls this. It stays because
    // it is the exact specification of the behaviour a depth-1 `FrameFIFO` must reproduce, and the
    // harness asserts that equivalence against it rather than against a hand-copied table.

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


    // MARK: - V5 bounded frame FIFO (2026-09-03) — replaces the V4 depth-1 latest-wins slots

    /// A small bounded FIFO with H.264/HEVC **reference-chain protection**, used for BOTH decoder
    /// hand-offs (USB read → decodeQueue, decodeQueue → main). Pure and generic over the payload so
    /// the hardware-free harness can exercise the whole drop policy without VideoToolbox.
    ///
    /// WHY IT REPLACED `resolveSlot`: the depth-1 latest-wins slots shed a frame on EVERY producer/
    /// consumer collision, and a shed frame punches a hole in the P-frame reference chain — every
    /// later frame then decodes with errors until the phone's next IDR (~2 s at its cadence). The
    /// 2026-09-03 device session logged 13 such drops in the first 12 s, each followed by a keyframe
    /// request; that is the user-visible stutter. A depth-3 FIFO gives the consumer a 2-frame cushion,
    /// so a transient main-thread/decodeQueue stall costs latency instead of the reference chain.
    ///
    /// Policy when the FIFO is FULL (in order):
    ///   1. Drop the OLDEST **unprotected** frame and admit the new one (`.evictedOldest`).
    ///   2. Everything queued is protected and the incoming frame is an IDR → drop the OLDEST frame
    ///      anyway (`.evictedProtected`). Dropping the incoming IDR instead would orphan every P that
    ///      references it (~2 s of poison); the evicted frame costs at most the queued frames behind
    ///      it, which the just-admitted IDR repairs within `depth` frames.
    ///   3. Everything queued is protected and the incoming frame is a P → drop the INCOMING frame and
    ///      ask for a keyframe (`.rejectedIncoming`) — exactly the old `resolveSlot` "P over IDR" rule.
    ///
    /// PROTECTED = the frame is an IDR, **or** it is the frame immediately following an IDR in stream
    /// order. The flag is computed once, at push time, and travels with the element: after an eviction
    /// the surviving neighbours keep the stream-order answer rather than the (wrong) post-eviction
    /// adjacency.
    ///
    /// The FIFO NEVER blocks a producer: a full queue always resolves to a drop, so a stalled consumer
    /// can never back-pressure the USB read path. It only changes WHICH frame is lost.
    /// What a `FrameFIFO.push` did to the queue. `index` is the position the shed frame occupied.
    /// Declared OUTSIDE the generic so the decoder's two differently-typed FIFOs (AVCC payloads on the
    /// decode hop, sample buffers on the enqueue hop) share ONE logging/accounting helper.
    enum FrameAdmissionOutcome: Equatable {
        /// Room in the FIFO — nothing was shed.
        case queued
        /// Full → the oldest UNPROTECTED (P) frame was dropped, the new frame admitted.
        case evictedOldest(index: Int)
        /// Full, everything queued protected, incoming IDR → oldest dropped, IDR admitted.
        case evictedProtected(index: Int)
        /// Full, everything queued protected, incoming P → the INCOMING frame was dropped.
        case rejectedIncoming

        /// Short rule name for the lane-tagged drop log ("which rule fired").
        var rule: String {
            switch self {
            case .queued:           return "queued"
            case .evictedOldest:    return "evict-oldest-P"
            case .evictedProtected: return "evict-oldest(all-protected,incoming-IDR)"
            case .rejectedIncoming: return "reject-incoming-P(all-protected)"
            }
        }
        /// True when a frame was lost (i.e. this push must be counted in `slotDrops`).
        var shedAFrame: Bool { self != .queued }
    }

    /// The result of a push: what happened, whether a keyframe must be requested, and the keyframe
    /// flag of the frame that was lost (nil ⇒ none lost) so the log can print `lost=IDR/P`.
    struct FrameAdmission: Equatable {
        var outcome: FrameAdmissionOutcome
        var requestKeyframe: Bool
        var droppedWasKeyframe: Bool?
        /// True when the queue was empty BEFORE this push, i.e. the caller must dispatch a drain.
        var wasEmpty: Bool
    }

    struct FrameFIFO<Element> {

        typealias Outcome = FrameAdmissionOutcome
        typealias Admission = FrameAdmission

        private struct Slot {
            var value: Element
            var keyframe: Bool
            /// Stream-order protection, frozen at push time (see the type doc).
            var protected: Bool
        }

        private var slots: [Slot] = []
        /// Keyframe flag of the last frame ADMITTED. A rejected push resets it to false: the next
        /// frame then follows a hole, not an IDR, so it earns no protection.
        private var lastAdmittedKeyframe = false

        /// Max frames held before the drop policy engages. 1 reproduces the V4 single-slot table.
        /// Settable (the AA lane raises it after construction); a shrink is not applied retroactively
        /// — the drop policy simply engages on the next push until the consumer drains the excess.
        var depth: Int { didSet { if depth < 1 { depth = 1 } } }

        init(depth: Int) { self.depth = max(1, depth) }

        var count: Int { slots.count }
        var isEmpty: Bool { slots.isEmpty }

        /// Offer a frame. Never blocks and never fails to make progress: either the frame is queued,
        /// or exactly one frame (queued or incoming) is shed.
        mutating func push(_ value: Element, keyframe: Bool) -> Admission {
            let wasEmpty = slots.isEmpty
            let protected = keyframe || lastAdmittedKeyframe

            if slots.count < depth {
                slots.append(Slot(value: value, keyframe: keyframe, protected: protected))
                lastAdmittedKeyframe = keyframe
                return Admission(outcome: .queued, requestKeyframe: false,
                                 droppedWasKeyframe: nil, wasEmpty: wasEmpty)
            }

            // Full. Rule 1: shed the oldest unprotected frame.
            if let victim = slots.firstIndex(where: { !$0.protected }) {
                // A hole only needs a fresh IDR if no keyframe FOLLOWS it — an IDR already behind the
                // victim (or the incoming frame itself) repairs the chain within `depth` frames, so
                // asking the phone for another would be pure churn.
                let repaired = keyframe || slots[(victim + 1)...].contains { $0.keyframe }
                let lost = slots.remove(at: victim).keyframe
                slots.append(Slot(value: value, keyframe: keyframe, protected: protected))
                lastAdmittedKeyframe = keyframe
                return Admission(outcome: .evictedOldest(index: victim), requestKeyframe: !repaired,
                                 droppedWasKeyframe: lost, wasEmpty: false)
            }

            // Rule 2: everything queued is protected, but the incoming frame is an IDR — it re-anchors
            // the picture, so the stalest frame is the cheaper loss.
            if keyframe {
                let lost = slots.remove(at: 0).keyframe
                slots.append(Slot(value: value, keyframe: keyframe, protected: protected))
                lastAdmittedKeyframe = true
                return Admission(outcome: .evictedProtected(index: 0), requestKeyframe: false,
                                 droppedWasKeyframe: lost, wasEmpty: false)
            }

            // Rule 3: everything queued is protected and the newcomer is a P — drop it, ask for an IDR.
            lastAdmittedKeyframe = false
            return Admission(outcome: .rejectedIncoming, requestKeyframe: true,
                             droppedWasKeyframe: keyframe, wasEmpty: false)
        }

        /// Take the oldest frame (FIFO order), or nil when empty.
        mutating func pop() -> Element? {
            guard !slots.isEmpty else { return nil }
            return slots.remove(at: 0).value
        }

        /// Drop everything (stream flush). Also clears the stream-order protection carry.
        mutating func removeAll() {
            slots.removeAll(keepingCapacity: true)
            lastAdmittedKeyframe = false
        }

        /// Keyframe flags of the queued frames, oldest → newest. Test/diagnostic accessor.
        var keyframeFlags: [Bool] { slots.map(\.keyframe) }
        /// Protection flags of the queued frames, oldest → newest. Test/diagnostic accessor.
        var protectionFlags: [Bool] { slots.map(\.protected) }
    }

    // MARK: - Codec-config records (avcC / hvcC)
    //
    // MOVED here verbatim from `VideoLane` (OCBM/OCBMAVBridge.swift) 2026-09-03 so the hardware-free
    // harness can exercise them — a 63-byte-SPS hvcC (the ALT/cluster lane's config) previously had no
    // test at all, and the only evidence it parsed correctly was a live log line. Behaviour unchanged:
    // `VideoLane.receiveConfig` calls straight into these.

    /// Unwrap an ISO VisualSampleEntry (`hvc1`/`hev1`/`avc1`) and return the payload of its nested
    /// `hvcC`/`avcC` configuration record. iOS sends the HEVC config this way (observed live
    /// 2026-07-12); per the box's own `unwrap_sample_description` (`crates/vendor/receiver/src/session.rs`)
    /// WIRELESS CarPlay delivers the VideoConfig as a sample description for H.264 too — that path used
    /// to return nil here, `parseAvcC` then rejected the wrapper (`b[0] != 1`), `configValid` stayed
    /// false and EVERY frame was dropped (black screen). Returns nil for a bare record (`b[0] == 1`),
    /// leaving the caller's `?? raw` to hand the record straight to the parsers, exactly as the box's
    /// early-return does.
    static func extractConfigRecord(fromSampleEntry b: [UInt8]) -> [UInt8]? {
        guard b.count >= 8, b[0] != 1 else { return nil } // bare avcC/hvcC begins configurationVersion=1
        // Structured walk: [0..4] box size (== total length), [4..8] fourcc, then the 78-byte
        // VisualSampleEntry fixed fields, then child atoms at offset 86.
        let boxSize = (Int(b[0]) << 24) | (Int(b[1]) << 16) | (Int(b[2]) << 8) | Int(b[3])
        let fourcc = String(bytes: b[4..<8], encoding: .ascii)
        if boxSize >= 94, boxSize <= b.count, fourcc == "hvc1" || fourcc == "hev1" || fourcc == "avc1" {
            var i = 86
            while i + 8 <= boxSize {
                let size = (Int(b[i]) << 24) | (Int(b[i + 1]) << 16) | (Int(b[i + 2]) << 8) | Int(b[i + 3])
                guard size >= 8, i + size <= boxSize else { break }
                if isConfigFourCC(b, i + 4) { return Array(b[(i + 8)..<(i + size)]) }
                i += size
            }
        }
        // Fallback, mirroring the box byte-for-byte: scan for the `hvcC`/`avcC` FourCC anywhere and take
        // everything after it. Covers a sample entry whose outer FourCC or fixed-field size we don't
        // model (the box gates on neither). The parsers are length-driven, so trailing atoms are ignored.
        if b.count >= 8 {
            for i in 0..<(b.count - 8) where isConfigFourCC(b, i) {
                return Array(b[(i + 4)...])
            }
        }
        return nil
    }

    /// `hvcC` or `avcC` at `b[i..<i+4]` — the two configuration-record atom types the box accepts.
    static func isConfigFourCC(_ b: [UInt8], _ i: Int) -> Bool {
        guard i + 4 <= b.count, b[i + 1] == 0x76, b[i + 2] == 0x63, b[i + 3] == 0x43 else { return false }
        return b[i] == 0x68 || b[i] == 0x61 // 'h'vcC / 'a'vcC
    }

    /// avcC box: [0]=version(1) [1..3]=profile/compat/level [4]=0xFC|(lenSizeMinusOne)
    /// [5]=0xE0|numSPS, per SPS [u16 len][bytes], then numPPS, per PPS [u16 len][bytes].
    /// Returns nil unless the first SPS really is an H.264 SPS (forbidden_zero_bit 0, NAL type 7).
    static func parseAvcC(_ b: [UInt8]) -> (sps: [UInt8], pps: [UInt8], lenSize: Int)? {
        guard b.count >= 7, b[0] == 1 else { return nil }
        let lenSize = Int(b[4] & 0x03) + 1
        var i = 5
        var sps: [UInt8]?
        var pps: [UInt8]?
        let numSPS = Int(b[i] & 0x1F); i += 1
        for _ in 0..<numSPS {
            guard i + 2 <= b.count else { return nil }
            let len = (Int(b[i]) << 8) | Int(b[i + 1]); i += 2
            guard len > 0, i + len <= b.count else { return nil }
            sps = Array(b[i..<i + len]); i += len
        }
        guard i < b.count else { return nil }
        let numPPS = Int(b[i]); i += 1
        for _ in 0..<numPPS {
            guard i + 2 <= b.count else { return nil }
            let len = (Int(b[i]) << 8) | Int(b[i + 1]); i += 2
            guard len > 0, i + len <= b.count else { return nil }
            pps = Array(b[i..<i + len]); i += len
        }
        guard let s = sps, let p = pps,
              s.first.map({ $0 & 0x80 == 0 && $0 & 0x1F == 7 }) == true, // H.264 SPS
              p.first.map({ $0 & 0x80 == 0 && $0 & 0x1F == 8 }) == true  // H.264 PPS
        else { return nil }
        return (s, p, lenSize)
    }

    /// hvcC box: 22 fixed header bytes ([21] low 2 bits = lenSizeMinusOne), [22]=numOfArrays, then per
    /// array [0]=completeness|NAL type, [1..2]=numNalus BE, per NALU [u16 len][bytes].
    /// Returns nil unless VPS(32)+SPS(33)+PPS(34) are all present with matching NAL headers.
    static func parseHvcC(_ b: [UInt8]) -> (vps: [UInt8], sps: [UInt8], pps: [UInt8], lenSize: Int)? {
        guard b.count >= 23, b[0] == 1 else { return nil }
        let lenSize = Int(b[21] & 0x03) + 1
        var i = 22
        let numArrays = Int(b[i]); i += 1
        var vps: [UInt8]?
        var sps: [UInt8]?
        var pps: [UInt8]?
        for _ in 0..<numArrays {
            guard i + 3 <= b.count else { return nil }
            let nalType = b[i] & 0x3F; i += 1
            let numNalus = (Int(b[i]) << 8) | Int(b[i + 1]); i += 2
            for _ in 0..<numNalus {
                guard i + 2 <= b.count else { return nil }
                let len = (Int(b[i]) << 8) | Int(b[i + 1]); i += 2
                guard len > 0, i + len <= b.count else { return nil }
                let nal = Array(b[i..<i + len]); i += len
                // The NALU's own header type must match the array's declared type.
                guard let h = nal.first, (h >> 1) & 0x3F == nalType else { return nil }
                switch nalType {
                case 32: vps = nal
                case 33: sps = nal
                case 34: pps = nal
                default: break // SEI arrays etc — fine, ignored
                }
            }
        }
        guard let v = vps, let s = sps, let p = pps else { return nil }
        return (v, s, p, lenSize)
    }
}
