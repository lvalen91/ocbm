// AirPlayModeWire.swift — the CH_CTRL side of AirPlay-only mode (docs/ocbm/00_PROTOCOL.md,
// "AirPlay-only mode"): the MODE_AIRPLAY request and the three box answers. Pure: Foundation and the
// shared OCBM constants only. The Swift twin of `ocbm_proto::AirplayRequest`; the bytes are pinned
// against the same vectors in tests/main.swift.

import Foundation

extension OCBM {
    /// `AIRPLAY_REQ_FLAGS` `[flags u8]` (optional; absent = 0). Kept here rather than in the shared
    /// OCBMFraming.swift until `crates/ocbm-proto` carries it, so tools/proto_check.py stays green.
    static let airplayReqFlags: UInt8 = 0x06
    /// Bit 0: advertise multi-codec screen video (feature bit 42, `SupportsScreenMultiCodec`). The phone
    /// sends HEVC only with this AND a canvas larger than 1080p (docs/ocbm/02_AIRPLAY_ONLY_MODE.md §4.10).
    static let airplayFlagHevc: UInt8 = 0x01
}

/// `[CT_MODE_SELECT][MODE_AIRPLAY]` + `AIRPLAY_REQ_*` TLVs `[tag u8][len u8][value]`.
struct AirPlayModeRequest: Equatable, Sendable {
    struct Display: Hashable, Sendable {
        var width: UInt16
        var height: UInt16
        var maxFPS: UInt8
        /// The measured configuration; what the box assumes when the TLV is absent.
        static let measuredDefault = Display(width: 1920, height: 1080, maxFPS: 30)
        /// The window's menu. 3840x2160 is the canvas at which the phone sent HEVC (with bit 42).
        static let presets = [measuredDefault, Display(width: 3840, height: 2160, maxFPS: 60),
                              Display(width: 3840, height: 2160, maxFPS: 120)]
        static let widths: ClosedRange<UInt16> = 320...7680
        static let heights: ClosedRange<UInt16> = 320...7680
        static let rates: ClosedRange<UInt8> = 1...120

        /// "3840x2160@60", the form the box's MGMT_INFO `display` uses.
        var text: String { "\(width)x\(height)@\(maxFPS)" }

        /// Parses `text`; nil for anything else (no range check: `validate` does that).
        init?(text: String) {
            let parts = text.split(whereSeparator: { $0 == "x" || $0 == "@" })
            guard parts.count == 3, let w = UInt16(parts[0]), let h = UInt16(parts[1]), let f = UInt8(parts[2]) else { return nil }
            self.init(width: w, height: h, maxFPS: f)
        }

        init(width: UInt16, height: UInt16, maxFPS: UInt8) {
            self.width = width
            self.height = height
            self.maxFPS = maxFPS
        }

        /// Larger than 1080p: the measured condition, with the HEVC flag, for the phone to send HEVC.
        var isAbove1080p: Bool { Int(width) * Int(height) > 1920 * 1080 }
    }

    /// Why the box would answer `MODE_ACK_BAD_PARAMS`. The text never contains a field's value.
    enum Invalid: Error, Equatable, Sendable {
        case ssid, passphrase, name, channel, display

        var message: String {
            switch self {
            case .ssid: return "SSID must be 1 to 32 bytes of UTF-8 with no control characters"
            case .passphrase: return "Passphrase must be 8 to 63 printable ASCII characters"
            case .name: return "Receiver name must be at most 63 bytes of UTF-8 with no control characters"
            case .channel: return "Channel must be empty (box default), 1 to 14, or 36 to 165"
            case .display: return "Display must be 320 to 7680 pixels each way at 1 to 120 fps"
            }
        }
    }

    var ssid: String
    /// WPA2 passphrase. Held in memory only; never logged, never persisted (see `summary`).
    var psk: String
    /// The advertised receiver name. Empty = the box uses the SSID.
    var name: String = ""
    /// 0 = the box template's channel.
    var channel: UInt8 = 0
    var display: Display = .measuredDefault
    /// `AIRPLAY_REQ_FLAGS` bit 0: let the phone choose HEVC. Off = the TLV is not sent (the box's default).
    var hevc = false

    /// The box's own rules (`AirplayRequest::decode`), applied before the link is touched.
    func validate() -> Invalid? {
        if !Self.textOK(Array(ssid.utf8), min: 1, max: 32) { return .ssid }
        let p = Array(psk.utf8)
        if p.count < 8 || p.count > 63 || !p.allSatisfy({ (0x20...0x7E).contains($0) }) { return .passphrase }
        if !name.isEmpty && !Self.textOK(Array(name.utf8), min: 1, max: 63) { return .name }
        if !Self.channelOK(channel) { return .channel }
        if !Self.displayOK(display) { return .display }
        return nil
    }

    /// The CH_CTRL payload, type byte included. Does not validate.
    func encode() -> [UInt8] {
        var o: [UInt8] = [OCBM.ctModeSelect, OCBM.modeAirplay]
        func tlv(_ tag: UInt8, _ v: [UInt8]) {
            let n = min(v.count, 255)
            o.append(tag)
            o.append(UInt8(n))
            o.append(contentsOf: v[..<n])
        }
        tlv(OCBM.airplayReqSsid, Array(ssid.utf8))
        tlv(OCBM.airplayReqPsk, Array(psk.utf8))
        if !name.isEmpty { tlv(OCBM.airplayReqName, Array(name.utf8)) }
        if channel != 0 { o.append(contentsOf: [OCBM.airplayReqChannel, 1, channel]) }
        o.append(contentsOf: [OCBM.airplayReqDisplay, 5,
                              UInt8(display.width & 0xff), UInt8(display.width >> 8),
                              UInt8(display.height & 0xff), UInt8(display.height >> 8),
                              display.maxFPS])
        if hevc { o.append(contentsOf: [OCBM.airplayReqFlags, 1, OCBM.airplayFlagHevc]) }
        return o
    }

    /// Parse and validate, exactly as the box does: nil = it would answer `MODE_ACK_BAD_PARAMS`.
    /// `name` comes back as the SSID when its TLV is absent. Used by the tests and the synthetic box.
    static func decode(_ pl: [UInt8]) -> AirPlayModeRequest? {
        guard pl.count >= 2, pl[0] == OCBM.ctModeSelect, pl[1] == OCBM.modeAirplay else { return nil }
        var ssid: [UInt8]?
        var psk: [UInt8]?
        var name: [UInt8]?
        var channel: UInt8 = 0
        var display = Display.measuredDefault
        var flags: UInt8 = 0
        var i = 2
        while i < pl.count {
            guard i + 2 <= pl.count, i + 2 + Int(pl[i + 1]) <= pl.count else { return nil }
            let tag = pl[i]
            let v = Array(pl[(i + 2)..<(i + 2 + Int(pl[i + 1]))])
            i += 2 + v.count
            switch tag {
            case OCBM.airplayReqSsid:
                guard textOK(v, min: 1, max: 32) else { return nil }
                ssid = v
            case OCBM.airplayReqPsk:
                guard v.count >= 8, v.count <= 63, v.allSatisfy({ (0x20...0x7E).contains($0) }) else { return nil }
                psk = v
            case OCBM.airplayReqName:
                guard textOK(v, min: 1, max: 63) else { return nil }
                name = v
            case OCBM.airplayReqChannel:
                guard v.count == 1, channelOK(v[0]) else { return nil }
                channel = v[0]
            case OCBM.airplayReqDisplay:
                guard v.count == 5 else { return nil }
                let d = Display(width: UInt16(v[0]) | UInt16(v[1]) << 8, height: UInt16(v[2]) | UInt16(v[3]) << 8, maxFPS: v[4])
                guard displayOK(d) else { return nil }
                display = d
            case OCBM.airplayReqFlags:
                // Bits other than bit 0 are reserved and must be 0 (the seam's definition).
                guard v.count == 1, v[0] & ~OCBM.airplayFlagHevc == 0 else { return nil }
                flags = v[0]
            default:
                break // a later revision's tag
            }
        }
        guard let s = ssid, let p = psk else { return nil }
        let ssidText = String(decoding: s, as: UTF8.self)
        return AirPlayModeRequest(ssid: ssidText, psk: String(decoding: p, as: UTF8.self),
                                  name: name.map { String(decoding: $0, as: UTF8.self) } ?? ssidText,
                                  channel: channel, display: display, hevc: flags & OCBM.airplayFlagHevc != 0)
    }

    /// For logs: every field except the passphrase, which is reduced to its length.
    var summary: String {
        let ch = channel == 0 ? "box default" : String(channel)
        let shown = name.isEmpty ? "(the SSID)" : "\"\(name)\""
        return "SSID \"\(ssid)\", receiver name \(shown), channel \(ch), display "
            + "\(display.text), HEVC \(hevc ? "allowed" : "off"), passphrase \(psk.utf8.count) chars"
    }

    /// UTF-8 with no control characters (C0, DEL, C1): it lands in a config line and a DNS label.
    private static func textOK(_ v: [UInt8], min: Int, max: Int) -> Bool {
        guard v.count >= min, v.count <= max, let s = String(bytes: v, encoding: .utf8) else { return false }
        return !s.unicodeScalars.contains { $0.properties.generalCategory == .control }
    }

    private static func channelOK(_ c: UInt8) -> Bool { c == 0 || (1...14).contains(c) || (36...165).contains(c) }

    private static func displayOK(_ d: Display) -> Bool {
        Display.widths.contains(d.width) && Display.heights.contains(d.height) && Display.rates.contains(d.maxFPS)
    }
}

/// `CT_AIRPLAY_STATE [ap][session][last_end]`.
struct AirPlayBoxState: Equatable, Sendable {
    var ap: UInt8 = OCBM.airplayApDown
    var session: UInt8 = OCBM.airplaySessionNone
    var lastEnd: UInt8 = OCBM.airplayEndNone

    /// What the box sends once when the mode is left for any reason.
    var isModeLeft: Bool { ap == 0 && session == 0 && lastEnd == 0 }

    var apName: String { AirPlayWire.apName(ap) }
    var sessionName: String { AirPlayWire.sessionName(session) }
    var lastEndName: String { AirPlayWire.endName(lastEnd) }
}

/// The fields of a `CT_HELLO_ACK` this app shows. `bundle` and `airplay` are nil from a box whose ACK
/// is too short to carry them (older firmware), which is a state, not an error.
struct HelloAckInfo: Equatable, Sendable {
    var version: UInt8
    var caps: UInt32
    var activeMode: UInt8
    var bundle: BoxBundleIdentity?
    var airplayAP: UInt8?
    var airplaySession: UInt8?

    var supportsAirPlay: Bool { caps & OCBM.capAirplay != 0 }
}

enum AirPlayWire {
    /// `[CT_HELLO_ACK][ver][caps u32 LE][active_mode]` (7 bytes, every box), then the bundle tail (to 40)
    /// and the mode tail (to 42).
    static func parseHelloAck(_ pl: [UInt8]) -> HelloAckInfo? {
        guard pl.count >= 7, pl[0] == OCBM.ctHelloAck else { return nil }
        var info = HelloAckInfo(version: pl[1], caps: OCBM.readLE32(pl, 2), activeMode: pl[6],
                                bundle: OCBM.helloAckBundle(pl))
        if pl.count >= OCBM.helloAckModeLen {
            info.airplayAP = pl[OCBM.helloAckApStateOff]
            info.airplaySession = pl[OCBM.helloAckSessionStateOff]
        }
        return info
    }

    /// `[CT_MODE_ACK][mode][status]`.
    static func parseModeAck(_ pl: [UInt8]) -> (mode: UInt8, status: UInt8)? {
        guard pl.count >= 3, pl[0] == OCBM.ctModeAck else { return nil }
        return (pl[1], pl[2])
    }

    static func parseState(_ pl: [UInt8]) -> AirPlayBoxState? {
        guard pl.count >= 4, pl[0] == OCBM.ctAirplayState else { return nil }
        return AirPlayBoxState(ap: pl[1], session: pl[2], lastEnd: pl[3])
    }

    static func apName(_ s: UInt8) -> String {
        switch s {
        case OCBM.airplayApDown: return "down"
        case OCBM.airplayApStarting: return "starting"
        case OCBM.airplayApUp: return "up"
        case OCBM.airplayApFailed: return "failed"
        default: return "unknown(\(s))"
        }
    }

    static func sessionName(_ s: UInt8) -> String {
        switch s {
        case OCBM.airplaySessionNone: return "none"
        case OCBM.airplaySessionWaiting: return "waiting"
        case OCBM.airplaySessionConnected: return "connected"
        case OCBM.airplaySessionStreaming: return "streaming"
        default: return "unknown(\(s))"
        }
    }

    static func endName(_ s: UInt8) -> String {
        switch s {
        case OCBM.airplayEndNone: return "none"
        case OCBM.airplayEndTeardown: return "teardown"
        case OCBM.airplayEndPeerClosed: return "peer-closed"
        case OCBM.airplayEndError: return "error"
        default: return "unknown(\(s))"
        }
    }

    static func modeAckName(_ s: UInt8) -> String {
        switch s {
        case OCBM.modeAckOk: return "ok"
        case OCBM.modeAckBadParams: return "bad parameters"
        case OCBM.modeAckUnsupported: return "unsupported (airplayd / airplay_mode.sh not installed)"
        default: return "status \(s)"
        }
    }

    static func modeName(_ m: UInt8) -> String {
        switch m {
        case OCBM.modeProjection: return "projection"
        case OCBM.modeConsole: return "console"
        case OCBM.modeAirplay: return "airplay"
        default: return "mode \(m)"
        }
    }
}
