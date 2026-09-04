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
    static let chLog: UInt16 = 0x0042     // box->host: the box's universal log stream (/tmp/box.log), see LogEntry below

    // ── CH_LOG (0x0042) — box's universal log stream ────────────────────────────────────────────
    // Frame payload = one or more entries: [source u8][flags u8][seq u16 LE][unix_ms u64 LE][len u16
    // LE][text: len bytes]. `source` 0 = /tmp/box.log (lines carry their own [ocbmd]/[airplayd]/…
    // prefixes), 255 = tailer-internal (host-synthesized markers, e.g. a seq-gap notice, never sent by
    // the box). `seq` is a per-channel u16 counter, +1 per entry, wrapping — a gap means loss. Armed
    // host->box by CT_LOG_CTL (below); the box resets to disabled on STOP/host-gone, so a host must
    // re-send it after every fresh SUBSCRIBE.
    // `source` ids (spec extension, box side `LOG_SRC_*` in `ocbm-proto` — reconciled here if the
    // box's final numbering differs; this table is the ONLY place that maps id -> name host-side).
    // 0 = /tmp/box.log (universal — lines carry their own [ocbmd]/[airplayd]/… prefixes); 1..8 = the
    // supervisor's per-daemon logs; 255 = tailer-internal (host-synthesized markers only).
    static let logSourceBox: UInt8 = 0
    static let logSourceAirplayd: UInt8 = 1
    static let logSourceAirplayWl: UInt8 = 2
    static let logSourceIap2d: UInt8 = 3
    static let logSourceAaBridge: UInt8 = 4
    static let logSourceRxConnect: UInt8 = 5
    static let logSourceBt: UInt8 = 6
    static let logSourceRadioApDhcp: UInt8 = 7
    static let logSourceRadioBtAttach: UInt8 = 8
    static let logSourceRxConnectWl: UInt8 = 9
    static let logSourceWl: UInt8 = 10
    static let logSourceTailer: UInt8 = 255
    static let logFlagDropped: UInt8 = 0x01   // len == 4; text is a u32 LE count of lines the box dropped
    static let logFlagTruncated: UInt8 = 0x02 // the line was clipped at 1024 B box-side
    static let logFlagBackfill: UInt8 = 0x04  // replayed from existing box.log at enable time, not live

    /// Human name for a CH_LOG `source` id — the file basename minus `.log`. Unknown ids (a numbering
    /// the box adds later, before this table is reconciled) fall back to `"src<id>"` rather than being
    /// dropped, so a new source is still visible instead of vanishing from the Box Log window.
    static func logSourceName(_ id: UInt8) -> String {
        switch id {
        case logSourceBox: return "box"
        case logSourceAirplayd: return "airplayd"
        case logSourceAirplayWl: return "airplayd_wl"
        case logSourceIap2d: return "iap2d"
        case logSourceAaBridge: return "aa-bridge"
        case logSourceRxConnect: return "rx-connect"
        case logSourceBt: return "bt"
        case logSourceRadioApDhcp: return "radio_ap_dhcp"
        case logSourceRadioBtAttach: return "radio_bt_attach"
        case logSourceRxConnectWl: return "rx-connect_wl"
        case logSourceWl: return "wl"
        case logSourceTailer: return "internal"
        default: return "src\(id)"
        }
    }

    /// CT_LOG_CTL (0x1B) on CH_CTRL, host->box: [CT_LOG_CTL][enabled u8][cap_kb u16 LE]. cap 0 = box
    /// default (256 KB). On enable the box streams from offset 0 (everything since boot) then follows;
    /// disable stops.
    static let ctLogCtl: UInt8 = 0x1B
    /// CT_SETTIME (0x05) host->box: [CT_SETTIME][unix_seconds u64 LE]. The CCPA has no RTC battery; every box log
    /// stamp (read-time and daemon write-time) is bogus until this lands, so it is sent right after each SUBSCRIBE.
    static let ctSetTime: UInt8 = 0x05

    /// Build the CT_LOG_CTL payload (does not include the CH_CTRL channel wrapper — pass to `send`).
    static func logCtl(enabled: Bool, capKB: UInt16) -> [UInt8] {
        var p: [UInt8] = [ctLogCtl, enabled ? 1 : 0, 0, 0]
        writeLE16(&p, 2, capKB)
        return p
    }

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
    static let mgmtEnterNCM: UInt8 = 0x06 // box arms /script/ncm_only and reboots into NCM (sticky; return via ssh)
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
    static let btpPairRejected: UInt8 = 0x07

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
    static let fNewSource: UInt8 = 0x08 // first frame from a newly accepted box A/V seam producer — reset reassembly

    // Audio seam v2 markers (`[u32 BE len][SEAM_MAGIC "SEAV"][marker]…`, see OCBMAVDecrypt.drainAudio).
    // 0x00 SEAM_KEY / 0x01 SEAM_PKT / 0x02 SEAM_FORMAT are parsed as literals in the drain switch; only
    // the newest one is named here because the AA telephony lane is the first producer outside CarPlay.
    /// `[0x03][scid 8 LE][payload]` — UNENCRYPTED access unit (no RTP, no key). The box's Android Auto
    /// telephony lane: HFP/SCO S16LE PCM forwarded verbatim on CH_ALT_AUDIO after a PCM SEAM_FORMAT.
    static let seamPktPlain: UInt8 = 0x03

    /// SEAM_FORMAT `codec` values. 0 PCM · 1 AAC-LC · 2 AAC-ELD · 3 OPUS ride the wire as literals
    /// (OCBMAVDecrypt parses the byte, the enum lives in ocbm-proto); only mSBC is named here,
    /// because it is the one value the HOST has to act on — the payload under it is a compressed
    /// bitstream this app decodes itself (Audio/MSBCCodec.swift), not something to hand a player.
    /// `ocbm-proto::SEAM_CODEC_MSBC`. Spelled `Msbc`, not `MSBC`, so tools/proto_check.py's
    /// camelCase→SCREAMING_SNAKE mapping lands on SEAM_CODEC_MSBC and actually checks the value.
    static let seamCodecMsbc: UInt8 = 4

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
    static let ctPairConfirm: UInt8 = 0x1C   // host->box [ctPairConfirm][accept u8: 1=pair, 0=cancel] — the USER'S
                                             // answer to the ctPairingCode prompt. SSP Numeric Comparison needs a
                                             // real yes/no on BOTH devices, so the box waits for this (up to 55 s,
                                             // inside its 60 s connect hold) instead of auto-accepting.
    static let ctRadio: UInt8 = 0x16         // host->box [ctRadio][0=radios off now | 1=radios on if cfg allows] — docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating
    static let ctProjMode: UInt8 = 0x19      // box->host [ctProjMode][pm*] — WHICH transport owns the box (docs/androidauto/02_ARBITRATION.md).
                                             // Mirrors the box's /tmp/projection_owner arbitration flag; on pmWiredAa the
                                             // app runs its AA head-unit engine over CH_IP instead of the CarPlay decoders.
    static let pmNone: UInt8 = 0x00          // idle — no projection session
    static let pmWiredCp: UInt8 = 0x01       // wired CarPlay
    static let pmWirelessCp: UInt8 = 0x02    // wireless CarPlay
    static let pmWiredAa: UInt8 = 0x03       // wired Android Auto (box aa-bridge AOAP pump)
    static let pmWirelessAa: UInt8 = 0x04    // wireless Android Auto (box aa-bridge --wireless TCP pump; device-proven 2026-09-04)
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

    /// Human-readable name for a `btp*` phase (logging only; per docs/carplay/01_OCBM_PROTOCOL.md
    /// this is advisory — an unrecognised value is reported verbatim, never coerced to IDLE).
    static func btPhaseName(_ phase: UInt8) -> String {
        switch phase {
        case btpIdle: return "IDLE"
        case btpLinkUp: return "LINK_UP"
        case btpAuthenticating: return "AUTHENTICATING"
        case btpAuthenticated: return "AUTHENTICATED"
        case btpIdentifying: return "IDENTIFYING"
        case btpIdentified: return "IDENTIFIED"
        case btpWifiHandoff: return "WIFI_HANDOFF"
        case btpPairRejected: return "PAIR_REJECTED"
        default: return "unknown(0x\(String(phase, radix: 16)))"
        }
    }

    /// Space-separated names of the SET `bh*` bits in a CT_BOX_HEALTH bitmask (logging only).
    static func boxHealthNames(_ bits: UInt8) -> String {
        var out: [String] = []
        if bits & bhHciPresent != 0 { out.append("hci") }
        if bits & bhSsp != 0 { out.append("ssp") }
        if bits & bhIap2d != 0 { out.append("iap2d") }
        if bits & bhAirplayd != 0 { out.append("airplayd") }
        if bits & bhCarplayWireless != 0 { out.append("wireless") }
        if bits & bhWlanAp != 0 { out.append("ap") }
        if bits & bhRootfsOk != 0 { out.append("rootfs") }
        return out.isEmpty ? "none" : out.joined(separator: " ")
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
    static func readLE64(_ b: [UInt8], _ o: Int) -> UInt64 {
        var v: UInt64 = 0
        for i in 0..<8 { v |= UInt64(b[o + i]) << (8 * i) }
        return v
    }
}

/// One decoded CH_LOG entry. `source`/`flags` are `OCBM.logSource*`/`OCBM.logFlag*`. `droppedCount` is
/// non-nil only for a DROPPED marker (`flags & logFlagDropped != 0`); `rawLen` is the wire `len` field
/// (the encoded byte count of `text`/the dropped-count marker), kept so a caller can account for bytes
/// consumed without re-deriving it from `text.utf8.count` (which would undercount on invalid UTF-8).
struct LogEntry: Equatable {
    let source: UInt8
    let flags: UInt8
    let seq: UInt16
    let unixMs: UInt64
    let text: String
    let droppedCount: UInt32?
    let rawLen: Int
    /// True ONLY for a HOST-SYNTHESIZED seq-gap marker (never sent by the box, never produced by
    /// `parseLogEntries`) — `OCBMClient.handleLog` sets this when `seq` jumps, and stamps `source`
    /// with the SAME source as the entry that revealed the gap, so the marker renders against the
    /// right per-source log (`[box/<sourceName>] !! seq gap …`) instead of an opaque "internal" tag.
    var isGapMarker: Bool = false

    var isDropped: Bool { flags & OCBM.logFlagDropped != 0 }
    var isTruncated: Bool { flags & OCBM.logFlagTruncated != 0 }
    /// Replayed from `/tmp/box.log`'s existing content at CT_LOG_CTL-enable time, not observed live —
    /// see `OCBM.logFlagBackfill`. Same "already happened" history on every reconnect, not new events.
    var isBackfill: Bool { flags & OCBM.logFlagBackfill != 0 }
}

/// Decode zero or more CH_LOG entries from one frame payload. Pure and bounds-checked: every field is
/// validated before use, and a malformed/truncated tail simply stops the scan (the valid prefix already
/// decoded is returned) rather than trapping — mirrors `OCBMReassembler`'s "never trust the wire"
/// discipline. The caller (`OCBMClient`) is responsible for logging a dropped remainder (throttled).
func parseLogEntries(_ payload: [UInt8]) -> [LogEntry] {
    let hdrLen = 14 // source(1) + flags(1) + seq(2) + unix_ms(8) + len(2)
    var out: [LogEntry] = []
    var off = 0
    while off + hdrLen <= payload.count {
        let source = payload[off]
        let flags = payload[off + 1]
        let seq = OCBM.readLE16(payload, off + 2)
        let unixMs = OCBM.readLE64(payload, off + 4)
        let len = Int(OCBM.readLE16(payload, off + 12))
        let textStart = off + hdrLen
        guard textStart + len <= payload.count else { break } // truncated tail — drop the remainder
        let bytes = len > 0 ? Array(payload[textStart..<textStart + len]) : []
        off = textStart + len
        if flags & OCBM.logFlagDropped != 0 && len == 4 {
            let count = OCBM.readLE32(bytes, 0)
            out.append(LogEntry(source: source, flags: flags, seq: seq, unixMs: unixMs,
                                 text: "", droppedCount: count, rawLen: len))
        } else {
            let text = String(bytes: bytes, encoding: .utf8) ?? ""
            out.append(LogEntry(source: source, flags: flags, seq: seq, unixMs: unixMs,
                                 text: text, droppedCount: nil, rawLen: len))
        }
    }
    return out
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
