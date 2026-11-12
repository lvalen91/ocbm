// MirrorVideoConfig.swift — what the opcode-1 body of the screen stream is (docs/ocbm/02_AIRPLAY_ONLY_MODE.md
// §4.10): for H.264 a bare avcC record; for HEVC an 'hvc1' sample-entry box (size, 'hvc1', the 78-byte
// visual sample entry, then child boxes) whose 'hvcC' child carries VPS/SPS/PPS. The phone picks the codec
// per session (HEVC only with feature bit 42 and a canvas above 1080p), so a host must take both. Pure:
// Foundation and the shared `AVCCFastPath` record parsers only.

import Foundation

enum MirrorVideoCodec: String, Equatable, Sendable {
    case h264 = "H.264"
    case hevc = "HEVC"
}

struct MirrorVideoConfig: Equatable, Sendable {
    var codec: MirrorVideoCodec
    /// H.264: SPS, PPS. HEVC: VPS, SPS, PPS. Each without a length prefix or start code.
    var parameterSets: [[UInt8]]
    /// Bytes in each NAL length prefix of the frames (4 on every capture).
    var nalLengthSize: Int
    /// H.264 profile_idc / HEVC general_profile_idc.
    var profile: UInt8
    /// H.264 level_idc (31 = 3.1) / HEVC general_level_idc (153 = 5.1, i.e. 30 x level).
    var level: UInt8
    /// How the body carried it: "avcC", "hvc1/hvcC", ... (for the log).
    var container: String

    var profileName: String {
        switch codec {
        case .h264:
            switch profile {
            case 66: return "Baseline"
            case 77: return "Main"
            case 88: return "Extended"
            case 100: return "High"
            default: return "profile \(profile)"
            }
        case .hevc:
            switch profile {
            case 1: return "Main"
            case 2: return "Main 10"
            case 3: return "Main Still Picture"
            case 4: return "Range Extensions"
            default: return "profile \(profile)"
            }
        }
    }

    var levelName: String {
        switch codec {
        case .h264: return "\(level / 10).\(level % 10)"
        case .hevc:
            let tenths = Int(level) / 3 // level_idc = 30 x level
            return tenths % 10 == 0 ? "\(tenths / 10)" : "\(tenths / 10).\(tenths % 10)"
        }
    }

    /// "HEVC Main L5.1" / "H.264 High L3.1".
    var summary: String { "\(codec.rawValue) \(profileName) L\(levelName)" }

    /// Read an opcode-1 body. nil = neither an avcC nor an hvcC can be found in it.
    ///
    /// Order: a sample-entry box ('hvc1'/'hev1'/'avc1'/'avc3') is searched for its child config box; a
    /// bare record is tried as avcC and then as hvcC (both start with version 1, and the parsers check
    /// the parameter sets' own NAL headers, so one cannot pass for the other); last, any 'hvcC' or
    /// 'avcC' box anywhere in the body (a layout this build does not know, but still the record).
    static func parse(_ body: [UInt8]) -> MirrorVideoConfig? {
        if body.count >= 8, let entry = String(bytes: body[4..<8], encoding: .ascii),
           ["hvc1", "hev1", "avc1", "avc3"].contains(entry) {
            let size = Int(readBE32(body, 0))
            let end = size >= 8 && size <= body.count ? size : body.count
            // Children start after the 8-byte box header and the 78-byte VisualSampleEntry fields.
            if let c = childConfig(body, from: 8 + 78, to: end, entry: entry) { return c }
        }
        if let c = avc(body, container: "avcC") { return c }
        if let c = hevc(body, container: "hvcC") { return c }
        for (type, make) in [("hvcC", hevc), ("avcC", avc)] as [(String, ([UInt8], String) -> MirrorVideoConfig?)] {
            if let r = findBox(type, in: body), let c = make(Array(body[r]), "box \(type)") { return c }
        }
        return nil
    }

    private static func childConfig(_ b: [UInt8], from start: Int, to end: Int, entry: String) -> MirrorVideoConfig? {
        var i = start
        while i + 8 <= end {
            let size = Int(readBE32(b, i))
            guard size >= 8, i + size <= end else { return nil }
            let type = String(bytes: b[(i + 4)..<(i + 8)], encoding: .ascii) ?? ""
            let payload = Array(b[(i + 8)..<(i + size)])
            if type == "hvcC" { return hevc(payload, container: "\(entry)/hvcC") }
            if type == "avcC" { return avc(payload, container: "\(entry)/avcC") }
            i += size
        }
        return nil
    }

    /// The payload range of the first well-formed box of `type`: `[u32 BE size][type][payload]`.
    private static func findBox(_ type: String, in b: [UInt8]) -> Range<Int>? {
        let t = Array(type.utf8)
        guard b.count >= 8 else { return nil }
        var i = 4
        while i + 4 <= b.count {
            if b[i] == t[0], b[i + 1] == t[1], b[i + 2] == t[2], b[i + 3] == t[3] {
                let size = Int(readBE32(b, i - 4))
                if size >= 8, i - 4 + size <= b.count { return (i + 4)..<(i - 4 + size) }
            }
            i += 1
        }
        return nil
    }

    private static func avc(_ record: [UInt8], container: String) -> MirrorVideoConfig? {
        guard let (sps, pps, len) = AVCCFastPath.parseAvcC(record) else { return nil }
        return MirrorVideoConfig(codec: .h264, parameterSets: [sps, pps], nalLengthSize: len,
                                 profile: record[1], level: record[3], container: container)
    }

    private static func hevc(_ record: [UInt8], container: String) -> MirrorVideoConfig? {
        guard let (vps, sps, pps, len) = AVCCFastPath.parseHvcC(record) else { return nil }
        // hvcC: [1] = profile_space(2) tier(1) profile_idc(5); [12] = general_level_idc.
        return MirrorVideoConfig(codec: .hevc, parameterSets: [vps, sps, pps], nalLengthSize: len,
                                 profile: record[1] & 0x1F, level: record[12], container: container)
    }

    private static func readBE32(_ b: [UInt8], _ o: Int) -> UInt32 {
        UInt32(b[o]) << 24 | UInt32(b[o + 1]) << 16 | UInt32(b[o + 2]) << 8 | UInt32(b[o + 3])
    }
}
