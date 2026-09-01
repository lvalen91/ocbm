// OCBMFraming.swift — OCBM (Open CCPA Bulk Multiplexer) v1 wire framing for the host app.
//
// Faithful Swift port of the validated Rust `ocbm-proto` (crates/ocbm-proto/src/lib.rs). The adapter's
// `ocbmd` muxes every channel (CTRL, VIDEO, MEDIA_AUDIO, …) over the single USB bulk pipe using this
// 16-byte little-endian header; the reassembler pops whole frames and resyncs on the magic + hcheck so a
// mid-stream byte loss self-heals. This replaces the original riddlebox 0x55AA55AA framing.
//
// Header (16 bytes, little-endian):
//   off 0  magic   u32 = 0x4F43424D ("OCBM")
//   off 4  length  u32  (payload length)
//   off 8  channel u16
//   off 10 flags   u8   (F_SOM | F_EOM)
//   off 11 hcheck  u8   (XOR of header bytes 0..=10)
//   off 12 seq     u32

import Foundation

enum OCBM {
    static let magic: UInt32 = 0x4F43_424D
    static let hdrLen = 16
    static let version: UInt8 = 1
    static let maxPayload = 65536

    // Channels
    static let chCtrl: UInt16 = 0x0000
    static let chVideo: UInt16 = 0x0020
    static let chMediaAudio: UInt16 = 0x0021
    static let chAltAudio: UInt16 = 0x0022 // voice-sink streams (telephony/speechRecognition/alert), same seam framing
    static let chMetadata: UInt16 = 0x0023 // box->host session metadata ([u32 len][META_CMD 0x01][plist]) — Metadata window
    static let chAltVideo: UInt16 = 0x0024 // box->host ALT / navigation (cluster) screen stream — dedicated decoder
    static let chIp: UInt16 = 0x0010      // host<->box stream-mux relay (IP_OPEN/IP_DATA/IP_CLOSE by conn id); AA rides this to reach the box aa-bridge (Phase 2 prototype, pre-CH_AA)
    static let chInput: UInt16 = 0x0030   // host->box HID input (task #20)
    // CH_IP stream-mux sub-frame types (payload = [type u8][conn_id u16 LE][data]); mirror ocbm-proto IP_*.
    static let ipOpen: UInt8 = 0x01       // data = target "host:port"; box connect()s and relays the stream
    static let ipData: UInt8 = 0x02       // data = stream bytes
    static let ipClose: UInt8 = 0x03      // no data
    static let chMic: UInt16 = 0x0031     // host->box mic uplink PCM (S16LE @ negotiated rate); box RTP-uplinks to iPhone
    static let chMgmt: UInt16 = 0x0040    // box management (the "CCPA" tab): request/response, see mgmt* below
    static let chRtsp: UInt16 = 0x0041    // app-driven SETUP control relay (plan P3) — box<->host RTSP/SETUP seam

    // ── App-driven SETUP control-relay constants (plan P3) ──────────────────────────────────────
    // MIRRORED (not imported) from `crates/vendor/receiver/src/relay.rs` — the metadata.rs META_* /
    // OCBMAVDecrypt SEAM_MAGIC pattern: each endpoint declares its own local consts and the relay.rs
    // doc block is the cross-checked contract. Keep these byte-for-byte in step with relay.rs.
    static let rtspSeamMagic: [UInt8] = [0x52, 0x54, 0x53, 0x50] // "RTSP" big-endian (relay::SEAM_MAGIC)
    static let rtspSeamMax = 512 * 1024                          // relay::RELAY_SEAM_MAX (per-message ceiling)
    // Message ops — common header [op u8][conn u32 LE][cseq u32 LE].
    static let rsOpen: UInt8 = 0x01   // box->host [ver][flags b0=wireless][cfg_crc u32 LE][ctx_len u32 LE][ctx]
    static let rsReq: UInt8 = 0x02    // box->host [route u8][flags b0=NOTIFY][local_len u32 LE][local][req]
    static let rsResp: UInt8 = 0x03   // host->box [status u16 LE][response body]
    static let rsClose: UInt8 = 0x04  // box->host [reason u8]
    static let rsErr: UInt8 = 0x05    // host->box [code u8] -> box falls back to local
    static let rsVer: UInt8 = 1       // RS_OPEN protocol version (relay::RS_VER)
    // RS_REQ routes (4-7 reserved).
    static let routeSetup: UInt8 = 1
    static let routeRecord: UInt8 = 2
    static let routeTeardown: UInt8 = 3 // always NOTIFY in v1
    // RS_REQ flags bit0: NOTIFY (no RS_RESP owed); RS_OPEN flags bit0: wireless connection.
    static let reqFlagNotify: UInt8 = 0x01
    static let openFlagWireless: UInt8 = 0x01
    // RS_CLOSE reasons.
    static let closeEOF: UInt8 = 0
    static let closeHijack: UInt8 = 1
    static let closeError: UInt8 = 2
    static let closeReset: UInt8 = 3
    // CH_MGMT verbs — host->box requests (low range), box->host responses (0x8x).
    static let mgmtGetInfo: UInt8 = 0x01
    static let mgmtReboot: UInt8 = 0x02
    static let mgmtForgetAll: UInt8 = 0x03
    static let mgmtForgetDevice: UInt8 = 0x04 // + ascii MAC "AA:BB:.."
    static let mgmtRestartWireless: UInt8 = 0x05
    static let mgmtInfo: UInt8 = 0x81  // + utf8 JSON snapshot
    static let mgmtAck: UInt8 = 0x82   // + [verb u8][status u8]

    // ---- box->host telemetry the box emits today (see crates/ocbm-proto) ------------------------
    // These are defined here so this client stays a complete statement of the protocol even where it
    // does not yet act on the opcode. Conformance is checked by tools/proto_check.py.

    static let ctModeSelect: UInt8 = 0x03
    static let ctSrc: UInt8 = 0x04
    static let ctSettime: UInt8 = 0x05
    static let ctEthStart: UInt8 = 0x06
    static let ctEthStop: UInt8 = 0x07
    static let ctPhoneIdent: UInt8 = 0x18  // + JSON {name,deviceID,model,osName,osVersion}; empty = cleared

    /// CT_BT_PHASE (box->host): where Bluetooth bring-up has got to, emitted on change.
    static let ctBtPhase: UInt8 = 0x17
    static let btpIdle: UInt8 = 0x00
    static let btpLinkUp: UInt8 = 0x01
    static let btpAuthenticating: UInt8 = 0x02
    static let btpAuthenticated: UInt8 = 0x03
    static let btpIdentifying: UInt8 = 0x04
    static let btpIdentified: UInt8 = 0x05
    static let btpWifiHandoff: UInt8 = 0x06

    /// CT_BOX_HEALTH (box->host): one bitmask of box-side subsystem liveness.
    /// Read `bhHciPresent` FIRST when Bluetooth looks dead — 0x50 with bit 0 clear is the signature
    /// of a missing radio HAL script: no hci0 was ever created, while OCBM, MFi and CT_SUBSCRIBE all
    /// still report success.
    static let ctBoxHealth: UInt8 = 0x1A
    static let bhHciPresent: UInt8 = 0x01
    static let bhSsp: UInt8 = 0x02
    static let bhIap2d: UInt8 = 0x04
    static let bhAirplayd: UInt8 = 0x08
    static let bhCarplayWireless: UInt8 = 0x10
    static let bhWlanAp: UInt8 = 0x20
    static let bhRootfsOk: UInt8 = 0x40

    static let sevHostGone: UInt8 = 0x02

    static let fReplay: UInt8 = 0x04  // replay of a frame the box already sent (dedupe hint)

    // Channels this client does not open, kept for completeness.
    static let chMfi: UInt16 = 0x0001
    static let chConsole: UInt16 = 0x0002
    static let chFile: UInt16 = 0x0011
    static let chEth: UInt16 = 0x0012
    static let chEcho: UInt16 = 0x00FF
    static let chDiscard: UInt16 = 0x0FFF

    // CH_HELLO_ACK capability bits.
    static let capConsole: UInt32 = 0x0000_0001
    static let capEcho: UInt32 = 0x0000_0002
    static let capMfi: UInt32 = 0x0000_0004
    static let capIp: UInt32 = 0x0000_0008
    static let capFile: UInt32 = 0x0000_0010
    static let capEth: UInt32 = 0x0000_0020

    // CH_FILE sub-opcodes.
    static let fileOpen: UInt8 = 0x01
    static let fileData: UInt8 = 0x02
    static let fileClose: UInt8 = 0x03
    static let fileAck: UInt8 = 0x04
    static let filePull: UInt8 = 0x05
    static let fileOk: UInt8 = 0
    static let fileErrOpen: UInt8 = 1
    static let fileErrVerify: UInt8 = 2
    static let fileErrNofile: UInt8 = 3
    static let fileErrWrite: UInt8 = 4

    static let ipOpenUdp: UInt8 = 0x04  // as ipOpen, UDP instead of TCP

    // CH_INPUT sub-frame types (payload = [type][...])
    static let inputTouch: UInt8 = 0x01   // [inputTouch][phase][nx u16 LE][ny u16 LE][finger]
    static let inputKeyframe: UInt8 = 0x02 // [inputKeyframe] -> box requests an iOS keyframe (task #33)
    static let inputKnob: UInt8 = 0x07     // [inputKnob][flags][nudge_x i8][nudge_y i8][rotation i8] -> box
                                           // sends one report on the Knob HID (uid 4); the Simulator's nav device.
    static let inputKeyframeAlt: UInt8 = 0x06 // [inputKeyframeAlt] -> box forces a keyframe on the ALT/cluster
                                              // stream (VideoStream.Alt1) specifically; a bare inputKeyframe only re-IDRs the main console.
    static let inputMediaBtn: UInt8 = 0x03 // [inputMediaBtn][index] -> box taps media-buttons HID uid 2 (task #35)
    static let inputCommand: UInt8 = 0x04  // [inputCommand][cmd] -> box sends the mapped /command (task #35)
    static let inputNav: UInt8 = 0x05      // [inputNav][nav] -> box taps the D-Pad HID uid 3 (SDK HIDDPad)

    // inputMediaBtn indices — Consumer-Control ARRAY index into the uid-2 media device (0 = release).
    static let mbtnPlay: UInt8 = 1
    static let mbtnPause: UInt8 = 2
    static let mbtnPlayPause: UInt8 = 3
    static let mbtnNext: UInt8 = 4
    static let mbtnPrev: UInt8 = 5

    // inputNav actions — the D-Pad (uid 3). The box builds Apple's exact 2-byte HIDDPad report.
    static let navUp: UInt8 = 1
    static let navDown: UInt8 = 2
    static let navLeft: UInt8 = 3
    static let navRight: UInt8 = 4
    static let navSelect: UInt8 = 5
    static let navHome: UInt8 = 6
    static let navBack: UInt8 = 7

    // inputCommand values. 0x01 (requestUI) / 0x02 (bare requestSiri) exist on the box (CMD_REQUEST_UI /
    // CMD_REQUEST_SIRI) but the host no longer sends them — Home rides the uid-3 D-Pad (navHome) and Siri
    // uses the hold pair below — so those two host constants were removed as dead (audit).
    static let cmdSiriDown: UInt8 = 0x03    // -> requestSiri siriAction: 2 (press)  INTEGER enum,
    static let cmdSiriUp: UInt8 = 0x04      // -> requestSiri siriAction: 3 (release) never a string.
    static let cmdNavStart: UInt8 = 0x05    // requestUI(cluster MAP) — start iOS's cluster encoder
    static let cmdNavStop: UInt8 = 0x06     // stopUI(cluster) — stop the cluster stream (None)
    static let cmdNavCard: UInt8 = 0x07     // requestUI(cluster INSTRUCTION CARD) — the maneuver/ETA card
    static let cmdLimitedUIOn: UInt8 = 0x08  // setLimitedUI(true) — restrict UI (Drive)
    static let cmdLimitedUIOff: UInt8 = 0x09 // setLimitedUI(false) — release (Park)
    static let cmdNavApp: UInt8 = 0x0A       // requestUI(cluster) — the "Navigation App" view
    static let cmdNavAppearance: UInt8 = 0x0B // [cmd][flags] — cluster showUI appearance toggles (below); box
                                              // rebuilds the current surface's query string + re-showUIs it.
    // Appearance flag bits (match ocbm_proto::NAV_APPEARANCE_*): the showUI query elements Apple exposes.
    static let navApSpeedLimit: UInt8 = 0x01 // showSpeedLimit=user | =no
    static let navApCompass: UInt8 = 0x02    // showCompass=user | =no
    static let navApETA: UInt8 = 0x04        // showETA=yes | =no
    static let cmdNavZoomIn: UInt8 = 0x0C    // changeMapZoomLevel zoomDirection=0 (+)
    static let cmdNavZoomOut: UInt8 = 0x0D   // changeMapZoomLevel zoomDirection=1 (−)
    // Display appearance (Light/Dark) — per-display UI/Map + global night mode (match ocbm_proto).
    // [inputCommand, cmd, stream, mode] for UI/Map; [inputCommand, cmdNightMode, on] for night.
    static let cmdUIAppearance: UInt8 = 0x0E   // uiAppearanceUpdate{uuid, appearanceMode}
    static let cmdMapAppearance: UInt8 = 0x0F  // mapAppearanceUpdate{uuid, appearanceMode}
    static let cmdNightMode: UInt8 = 0x10      // setNightMode{nightMode}
    static let appearanceStreamMain: UInt8 = 0x00 // DISPLAY_UUID
    static let appearanceStreamAlt: UInt8 = 0x01  // ALT_DISPLAY_UUID (alt-screen-gated)
    static let appearanceModeLight: UInt8 = 0x00
    static let appearanceModeDark: UInt8 = 0x01
    // Telephony (uid-5 HID): [inputTelephony][buttonIndex] — the box sends the 1-byte HID report then a
    // release. Index = the usage position in Apple's HIDTelephony descriptor.
    static let inputTelephony: UInt8 = 0x08
    static let telAnswer: UInt8 = 1   // Hook Switch (off-hook / accept)
    static let telFlash: UInt8 = 2    // Flash (swap / answer call-waiting)
    static let telEnd: UInt8 = 3      // Drop (end / hang up)
    static let telMute: UInt8 = 4     // Mute
    static let telDigit0: UInt8 = 5   // DTMF digit d → telDigit0 + d (0..9 = 5..14)
    static let telStar: UInt8 = 15    // *
    static let telPound: UInt8 = 16   // #
    static let telDelete: UInt8 = 17  // Delete
    // touch phase wire values (NOTE: distinct from MultiTouchAction's raw values)
    static let touchDown: UInt8 = 0x00
    static let touchMove: UInt8 = 0x01
    static let touchUp: UInt8 = 0x02

    // Frame flags
    static let fSom: UInt8 = 0x01
    static let fEom: UInt8 = 0x02

    // CH_CTRL message types (first payload byte)
    static let ctHello: UInt8 = 0x01
    static let ctHelloAck: UInt8 = 0x02
    static let ctSubscribe: UInt8 = 0x10
    static let ctStop: UInt8 = 0x11
    static let ctHeartbeat: UInt8 = 0x12
    static let ctSessionEvent: UInt8 = 0x13
    static let ctUplink: UInt8 = 0x14        // box->host [ctUplink][state u8][rate u32 LE][ch u8] — mic-uplink gate (1=on/0=off)
    static let ctPairingCode: UInt8 = 0x15   // box->host [ctPairingCode][6 ascii digits | empty] — SSP Numeric-Comparison code to show (empty = clear)
    static let ctRadio: UInt8 = 0x16         // host->box [ctRadio][0=radios off now | 1=radios on if cfg allows] — docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating
    static let ctProjMode: UInt8 = 0x19      // box->host [ctProjMode][pm*] — WHICH transport owns the box (docs/host/02_ANDROID_AUTO.mde).
                                             // Mirrors the box's /tmp/projection_owner arbitration flag; on pmWiredAa the
                                             // app runs its AA head-unit engine over CH_IP instead of the CarPlay decoders.
    static let pmNone: UInt8 = 0x00          // idle — no projection session
    static let pmWiredCp: UInt8 = 0x01       // wired CarPlay
    static let pmWirelessCp: UInt8 = 0x02    // wireless CarPlay
    static let pmWiredAa: UInt8 = 0x03       // wired Android Auto (box aa-bridge AOAP pump)
    static let pmWirelessAa: UInt8 = 0x04    // reserved — wireless AA (docs/host/02_ANDROID_AUTO.mdf, unbuilt)
    static let sevHostPresent: UInt8 = 0x01
    static let sevPhonePresent: UInt8 = 0x03 // iPhone on the adapter bus (truthful phone presence)
    static let sevPhoneAbsent: UInt8 = 0x04  // no iPhone on the adapter bus — show "waiting for phone" NOW

    /// Human-readable name for a `pm*` projection mode (logging only; an unknown value is reported
    /// verbatim rather than coerced — per the protocol an unknown mode means "some transport owns
    /// the box", never "idle").
    static func projModeName(_ mode: UInt8) -> String {
        switch mode {
        case pmNone: return "idle"
        case pmWiredCp: return "wired CarPlay"
        case pmWirelessCp: return "wireless CarPlay"
        case pmWiredAa: return "wired Android Auto"
        case pmWirelessAa: return "wireless Android Auto"
        default: return "unknown(0x\(String(mode, radix: 16)))"
        }
    }

    /// hcheck = XOR of header bytes 0..<11.
    static func hcheck(_ h: ArraySlice<UInt8>) -> UInt8 {
        h.prefix(11).reduce(UInt8(0)) { $0 ^ $1 }
    }

    /// Build a full frame (header + payload) into a new Data.
    static func frame(channel: UInt16, flags: UInt8, seq: UInt32, payload: [UInt8]) -> [UInt8] {
        // An over-maxPayload length field makes the box's reassembler treat the header as junk and
        // byte-resync through the ENTIRE payload — a stream tear, not a dropped message. Never emit one.
        precondition(payload.count <= maxPayload,
                     "OCBM frame payload \(payload.count) B exceeds maxPayload \(maxPayload) — would tear the stream")
        var out = [UInt8](repeating: 0, count: hdrLen + payload.count)
        writeLE32(&out, 0, magic)
        writeLE32(&out, 4, UInt32(payload.count))
        writeLE16(&out, 8, channel)
        out[10] = flags
        out[11] = hcheck(out[0..<16])
        writeLE32(&out, 12, seq)
        if !payload.isEmpty { out.replaceSubrange(hdrLen..<hdrLen + payload.count, with: payload) }
        return out
    }

    static func writeLE16(_ b: inout [UInt8], _ o: Int, _ v: UInt16) {
        b[o] = UInt8(v & 0xff); b[o + 1] = UInt8((v >> 8) & 0xff)
    }
    static func writeLE32(_ b: inout [UInt8], _ o: Int, _ v: UInt32) {
        b[o] = UInt8(v & 0xff); b[o + 1] = UInt8((v >> 8) & 0xff)
        b[o + 2] = UInt8((v >> 16) & 0xff); b[o + 3] = UInt8((v >> 24) & 0xff)
    }
    static func readLE16(_ b: [UInt8], _ o: Int) -> UInt16 {
        UInt16(b[o]) | (UInt16(b[o + 1]) << 8)
    }
    static func readLE32(_ b: [UInt8], _ o: Int) -> UInt32 {
        UInt32(b[o]) | (UInt32(b[o + 1]) << 8) | (UInt32(b[o + 2]) << 16) | (UInt32(b[o + 3]) << 24)
    }
}

/// A parsed OCBM frame handed to the session layer.
struct OCBMFrame {
    let channel: UInt16
    let flags: UInt8
    let payload: [UInt8]
}

/// Streaming reassembler: `push()` raw bulk reads, `next()` pops whole frames. Cursor-based (no O(n²)
/// front-drain), resyncs on magic + hcheck — mirrors the hardened Rust `Reassembler`.
final class OCBMReassembler {
    private var buf: [UInt8] = []
    private var start = 0

    func push(_ data: [UInt8]) {
        compact()
        buf.append(contentsOf: data)
    }

    private func compact() {
        if start >= buf.count {
            buf.removeAll(keepingCapacity: true)
            start = 0
        } else if start > OCBM.hdrLen + OCBM.maxPayload {
            buf.removeFirst(start)
            start = 0
        }
    }

    /// Pop the next complete frame, or nil if none is fully buffered yet.
    func next() -> OCBMFrame? {
        while true {
            let avail = buf.count - start
            if avail < OCBM.hdrLen { compact(); return nil }
            // Validate header: magic + hcheck.
            let magic = OCBM.readLE32(buf, start)
            let hc = OCBM.hcheck(buf[start..<start + 11])
            if magic != OCBM.magic || hc != buf[start + 11] {
                start += 1 // resync one byte
                continue
            }
            let plen = Int(OCBM.readLE32(buf, start + 4))
            if plen > OCBM.maxPayload {
                start += 1 // implausible length → junk, resync
                continue
            }
            let total = OCBM.hdrLen + plen
            if avail < total { compact(); return nil } // waiting on the rest
            let channel = OCBM.readLE16(buf, start + 8)
            let flags = buf[start + 10]
            let payload = Array(buf[start + OCBM.hdrLen..<start + total])
            start += total
            compact()
            return OCBMFrame(channel: channel, flags: flags, payload: payload)
        }
    }
}
