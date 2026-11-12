// AirPlayEvents.swift — the `{"kind":"airplay","ev":...}` objects the box sends as META_JSON on
// CH_METADATA (docs/ocbm/00_PROTOCOL.md "AirPlay-only mode", Events). Pure.
//
// Two rules from the protocol text are load-bearing here:
//   - integers are exact and may exceed 2^53 (scid): `scid` is read from the text as an unsigned 64-bit
//     value, never through a floating-point JSON number;
//   - the `sender` event carries the phone's deviceID and address. Neither is kept: this type has no
//     field for them, so they cannot reach the UI or the log.

import Foundation

enum AirPlayEvent: Equatable, Sendable {
    struct Sender: Equatable, Sendable {
        var name = ""
        var model = ""
        var osName = ""
        var osVersion = ""
        /// "NTP" = mirroring, "PTP" = audio.
        var timing = ""
        var mirroring = false
    }

    struct Track: Equatable, Sendable {
        var title: String?
        var artist: String?
        var album: String?
        var genre: String?
        var durationMs: UInt64?
    }

    struct Flush: Equatable, Sendable {
        var untilSeq: UInt32?
        var untilTS: UInt32?
        var fromSeq: UInt32?
        var fromTS: UInt32?
    }

    case session(state: String, reason: String)
    case sender(Sender)
    /// `type` = the SETUP's stream type (110 / 96 / 103); `detail` = a few SETUP scalars for the log.
    case streamStart(lane: String, scid: UInt64, type: Int?, detail: String)
    case streamStop(type: Int?, scid: UInt64, reason: String, messages: UInt64, bytes: UInt64, dropped: UInt64)
    case streamUnsupported(type: Int?)
    case volume(db: Double)
    case progress(start: UInt32, current: UInt32, end: UInt32)
    case track(Track)
    case artwork(bytes: Int)
    case anchor(rate: Double, rtpTime: UInt32?)
    case flush(Flush)
    case sync(scid: UInt64)
    case other(name: String)

    /// nil = not an AirPlay-mode event (another kind, or not a JSON object).
    static func parse(_ body: [UInt8]) -> AirPlayEvent? {
        guard let obj = try? JSONSerialization.jsonObject(with: Data(body)) as? [String: Any],
              obj["kind"] as? String == "airplay", let ev = obj["ev"] as? String else { return nil }
        let text = String(decoding: body, as: UTF8.self)
        func str(_ k: String, in d: [String: Any]? = nil) -> String { ((d ?? obj)[k] as? String) ?? "" }
        func num(_ k: String, in d: [String: Any]? = nil) -> NSNumber? { (d ?? obj)[k] as? NSNumber }
        func u32(_ k: String) -> UInt32? { num(k).map { UInt32(truncatingIfNeeded: $0.int64Value) } }
        let scid = exactUnsigned("scid", in: text) ?? 0
        switch ev {
        case "session":
            return .session(state: str("state"), reason: str("reason"))
        case "sender":
            return .sender(Sender(name: str("name"), model: str("model"), osName: str("osName"),
                                  osVersion: str("osVersion"), timing: str("timing"),
                                  mirroring: (obj["mirroring"] as? Bool) ?? false))
        case "stream":
            let setup = obj["setup"] as? [String: Any]
            let type = (num("type", in: setup) ?? num("type"))?.intValue
            switch str("state") {
            case "start":
                return .streamStart(lane: str("lane"), scid: scid, type: type, detail: setupDetail(setup))
            case "stop":
                return .streamStop(type: type, scid: scid, reason: str("reason"),
                                   messages: num("messages")?.uint64Value ?? 0,
                                   bytes: num("bytes")?.uint64Value ?? 0,
                                   dropped: num("dropped")?.uint64Value ?? 0)
            case "unsupported":
                return .streamUnsupported(type: type)
            default:
                return .other(name: "stream/" + str("state"))
            }
        case "volume":
            return .volume(db: num("db")?.doubleValue ?? 0)
        case "progress":
            return .progress(start: u32("start") ?? 0, current: u32("current") ?? 0, end: u32("end") ?? 0)
        case "track":
            func opt(_ k: String) -> String? { obj[k] as? String }
            return .track(Track(title: opt("title"), artist: opt("artist"), album: opt("album"),
                                genre: opt("genre"), durationMs: num("durationMs")?.uint64Value))
        case "artwork":
            return .artwork(bytes: num("bytes")?.intValue ?? 0)
        case "anchor":
            return .anchor(rate: num("rate")?.doubleValue ?? 1, rtpTime: u32("rtpTime"))
        case "flush":
            return .flush(Flush(untilSeq: u32("flushUntilSeq"), untilTS: u32("flushUntilTS"),
                                fromSeq: u32("flushFromSeq"), fromTS: u32("flushFromTS")))
        case "sync":
            return .sync(scid: scid)
        default:
            return .other(name: ev)
        }
    }

    /// The decimal digits after the first `"key":` in `text`, as an unsigned 64-bit value. The box
    /// writes these members itself, top level first, so the first occurrence is the top-level one.
    static func exactUnsigned(_ key: String, in text: String) -> UInt64? {
        guard let r = text.range(of: "\"\(key)\":") else { return nil }
        let digits = text[r.upperBound...].prefix { $0.isASCII && $0.isNumber }
        return UInt64(digits)
    }

    /// The SETUP scalars worth a log line. Never key material: the box leaves data members out.
    private static func setupDetail(_ setup: [String: Any]?) -> String {
        guard let setup else { return "" }
        let keys = ["ct", "audioFormat", "sr", "spf", "latencyMs", "latencyMin", "latencyMax", "redundantAudio"]
        return keys.compactMap { k in (setup[k] as? NSNumber).map { "\(k) \($0)" } }.joined(separator: ", ")
    }

    /// One line for the log pane, or nil for events too frequent to print (`sync`, `progress`).
    var logLine: String? {
        switch self {
        case .session(let state, let reason):
            return "session \(state) (last end: \(reason))"
        case .sender(let s):
            let kind = s.mirroring ? "screen mirroring" : "audio"
            return "sender \"\(s.name)\" \(s.model) \(s.osName) \(s.osVersion), \(kind), timing \(s.timing)"
        case .streamStart(let lane, let scid, let type, let detail):
            let t = type.map { "type \($0)" } ?? "type ?"
            return "stream start: \(t) on the \(lane) lane, scid \(scid)" + (detail.isEmpty ? "" : " (\(detail))")
        case .streamStop(let type, let scid, let reason, let messages, let bytes, let dropped):
            let t = type.map { "type \($0)" } ?? "type ?"
            return "stream stop: \(t) scid \(scid), \(reason); box forwarded \(messages) messages / \(bytes) B, dropped \(dropped)"
        case .streamUnsupported(let type):
            return "stream NOT tunnelled: type \(type.map(String.init) ?? "?")"
        case .volume(let db):
            return "volume \(String(format: "%.1f", db)) dB"
        case .track(let t):
            return "track: \(t.title ?? "(no title)")" + (t.artist.map { " / \($0)" } ?? "")
        case .artwork(let bytes):
            return bytes == 0 ? "artwork cleared" : "artwork \(bytes) B"
        case .anchor(let rate, let rtpTime):
            return "anchor: rate \(rate)" + (rtpTime.map { ", rtpTime \($0)" } ?? "")
        case .flush(let f):
            let parts = [f.fromSeq.map { "from seq \($0)" }, f.untilSeq.map { "until seq \($0)" }].compactMap { $0 }
            return "flush " + (parts.isEmpty ? "(no range)" : parts.joined(separator: ", "))
        case .other(let name):
            return "event \(name)"
        case .sync, .progress:
            return nil
        }
    }
}

/// AirPlay's volume scale: 0 dB = full, -30 dB = slider at 0, -144 = mute.
enum AirPlayVolume {
    /// Slider position 0...1 for a `volume` event's dB value.
    static func sliderPosition(db: Double) -> Double {
        guard db.isFinite, db > -30 else { return 0 }
        return min(1, max(0, (db + 30) / 30))
    }
}
