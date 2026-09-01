//! OCBM v1 wire codec — the Open CCPA Bulk Multiplexer envelope.
//!
//! Projection-agnostic: the same envelope carries CarPlay and Android Auto. Nothing here is specific
//! to either.
//!
//! 16-byte little-endian header + payload, carried over the `/dev/usb_accessory` bulk pipe:
//! ```text
//!   off 0  magic   u32 = 0x4F43424D ("OCBM")   resync marker
//!   off 4  length  u32                          payload byte count
//!   off 8  channel u16                          logical channel ("the type")
//!   off 10 flags   u8                           bit0 SOM, bit1 EOM, bit2 REPLAY
//!   off 11 hcheck  u8                           XOR of header bytes 0..=10
//!   off 12 seq     u32                          per-endpoint sequence (global across channels; debug only)
//!   off 16 payload [length]
//! ```
//! See ../../docs/carplay/01_OCBM_PROTOCOL.md.

pub const MAGIC: u32 = 0x4F43_424D;
pub const HDR_LEN: usize = 16;
pub const VERSION: u8 = 1;
pub const MAX_PAYLOAD: usize = 65536;

// channels
pub const CH_CTRL: u16 = 0x0000;
pub const CH_MFI: u16 = 0x0001;
/// Length of the OPTIONAL 1-byte MFi correlation tag, appended after the declared payload of a
/// CH_MFI request and echoed on the response (`ocbmd::handle_mfi`). The reply carries no opcode echo
/// and no request id, so a host with two concurrent chip users could otherwise correlate only by
/// payload length — which cannot tell two 128-byte signatures apart, and a late reply to a timed-out
/// sign then answers the wrong digest. Additive in both directions: a peer that sends no tag gets
/// none back and keeps the old length-correlation behaviour.
pub const MFI_TAG_LEN: usize = 1;
pub const CH_CONSOLE: u16 = 0x0002;
pub const CH_IP: u16 = 0x0010;
pub const CH_FILE: u16 = 0x0011; // host->box file push (reliable binary deploy), see FILE_* sub-frames
pub const CH_ETH: u16 = 0x0012; // raw L2 ethernet frames bridged box<->host (each payload = one frame)
pub const CH_VIDEO: u16 = 0x0020; // box->host A/V: MAIN screen video. Forward-encrypted (ChaCha20-Poly1305) with seq+SEAM_MAGIC seam framing; the host decrypts + decodes (OCBM_FWD_ENC model). Box seam :9001
pub const CH_MEDIA_AUDIO: u16 = 0x0021; // box->host A/V: media audio, same forward-encrypted seam framing (box hands the per-stream key; host decrypts). Box seam :9002
pub const CH_ALT_AUDIO: u16 = 0x0022; // box->host A/V: voice-sink streams (telephony/speechRecognition/alert/default), same seam framing as media
pub const CH_METADATA: u16 = 0x0023; // box->host: session metadata (Metadata window). Seam :9004, framing [u32 BE "META"][u32 BE len][marker][payload] (v2, audit Fix #17); META_* markers below
pub const CH_ALT_VIDEO: u16 = 0x0024; // box->host: the ALT / navigation (instrument-cluster) screen stream. Same seq+SEAM_MAGIC forward-encrypted framing as CH_VIDEO; the host decodes it on a DEDICATED decoder. Box seam :9005.

// CH_METADATA seam markers (wire values; producers declare their own local consts — iap2d does not
// link this crate). Payloads are PLAINTEXT (the box decrypts the control connection; the host is
// the trusted consumer over the app-owned USB link).
//   0x01 META_CMD     — payload = the raw binary plist of an inbound iPhone POST /command ({type, params}) — from airplayd
//   0x02 META_JSON    — payload = one JSON object {"kind":…} of iAP2 metadata (nowPlaying/routeGuidance/maneuver/callState/communications/recentCall/favorite/artworkReady) — from iap2d
//   0x03 META_ARTWORK — payload = [artwork id u8][JPEG bytes] — album art via the iAP2 File-Transfer session
//   0x04 META_CORNERMASK — payload = [u32 BE display_width_px][PNG bytes] — iOS's exact per-resolution topLeftCornerMask (docs/carplay/06_AV_PIPELINE.md); host renders the corner curve from it — from airplayd (receiver)
pub const CH_MGMT: u16 = 0x0040; // box management (the app's "CCPA" tab). Request/response, see MGMT_* below.
// CH_RTSP — the app-driven-SETUP relay seam (plan: app-driven SETUP, phase P1). ocbmd is a DUMB BYTE
// PIPE on this channel: it chunks the box seam's bytes (airplayd relay listener, 127.0.0.1:9106) into
// ≤64 KiB OCBM frames both ways; ALL message framing is endpoint-to-endpoint (airplayd ↔ host app),
// which sidesteps MAX_PAYLOAD 64 KiB < RTSP MAX_BODY 256 KiB. Rides out_hi (latency-sensitive,
// reliable — the pair/SETUP/RECORD phase is timing-critical).
//
// Seam framing (survives an out_hi cap-clear truncation, which has no resync policy — receivers scan
// for the magic, exactly the CH_METADATA-v2 "META" pattern):
//   [u32 BE 0x52545350 "RTSP"][u32 BE len][msg],  len ≤ 512 KiB (RELAY_SEAM_MAX)
// Messages — common header [op u8][conn u32 LE][cseq u32 LE], then per-op payload (multibyte fields LE):
//   0x01 RS_OPEN   box→host  [ver=1][flags b0=wireless][cfg_crc u32][ctx_len u32][ctx bplist
//                            {peer, displayWidth, displayHeight}] — cfg_crc = crc32 of the YAML this
//                            connection's /info was built from; host compares vs what it pushed.
//   0x02 RS_REQ    box→host  [route u8][flags b0=NOTIFY][local_len u32][local-resp bplist][req bplist]
//                            routes: 1=SETUP, 2=RECORD, 3=TEARDOWN(NOTIFY); 4-7 reserved.
//   0x03 RS_RESP   host→box  [status u16][response body] — non-200 = host-reject → local fallback (v1).
//   0x04 RS_CLOSE  box→host  [reason u8: 0=eof 1=hijack 2=error 3=reset]
//   0x05 RS_ERR    host→box  [code u8] — box falls back to its local response.
// `conn` is monotonic per airplayd process (hijack ⇒ new conn; the single FIFO guarantees
// RS_CLOSE(old) precedes RS_OPEN(new)); the host drops messages for a non-current conn.
// The RS_*/magic constants are defined authoritatively in `receiver::relay` and MIRRORED here (and in
// the Swift/harness consumers) rather than imported — the same pattern as the META_* markers below:
// the box's receiver crates do not link ocbm-proto, so each endpoint declares its own local consts and
// this comment block is the cross-checked contract.
pub const CH_RTSP: u16 = 0x0041; // box↔host: app-driven SETUP relay (RS_* over the "RTSP"-magic seam; box seam :9106)
pub const CH_INPUT: u16 = 0x0030; // host->box: HID input (touch/buttons); ocbmd relays to airplayd -> iPhone
pub const CH_MIC: u16 = 0x0031; // host->box: mic uplink PCM (S16LE, negotiated rate/ch). ocbmd relays each
                                // payload to airplayd's mic-ingest seam; airplayd RTP-uplinks it to the iPhone
                                // on the active type-100 `input=true` MainAudio SETUP (Siri/telephony).
pub const CH_ECHO: u16 = 0x00FF;
pub const CH_DISCARD: u16 = 0x0FFF; // box parses + drops silently (uplink benchmark sink)

// flags
pub const F_SOM: u8 = 0x01;
pub const F_EOM: u8 = 0x02;
/// Set by the box on a state-mirror frame emitted because the receiver had no prior value for it
/// (a fresh `CT_SUBSCRIBE`, or the first read after an `ocbmd` restart) rather than because the
/// value changed. Lets a host tell "this is news" from "this is what you already knew": a mirror
/// file that is never cleared otherwise replays as a live event. Purely advisory — every existing
/// receiver on both ends ignores unknown flag bits, so the two sides may adopt it independently.
pub const F_REPLAY: u8 = 0x04;

// CTRL message types (first payload byte on CH_CTRL)
pub const CT_HELLO: u8 = 0x01; // host->box [CT_HELLO][ver][instance u32 LE] — the trailing 4 bytes are the
                               // HOST INSTANCE NONCE: a value fixed for the lifetime of one host session
                               // object and re-sent on every reattach it makes. 0 = "not supplied" (older
                               // hosts), which the box treats exactly as it always did.
                               //
                               // It exists because nothing else on the wire distinguishes one host PROCESS
                               // from another. A host killed without CT_STOP leaves the box `present`, and a
                               // relaunch inside the heartbeat grace keeps `last_hb` fresh from the NEW
                               // process — so the box sees an unbroken host, never re-arms projection, and
                               // the session survives with no A/V (airplayd is only spawned on the
                               // GONE->PRESENT edge of /tmp/host_present). Comparing nonces separates that
                               // from a genuine reattach by the SAME host, which can still warm-reuse.
pub const CT_HELLO_ACK: u8 = 0x02;
pub const CT_MODE_SELECT: u8 = 0x03;
pub const CT_SRC: u8 = 0x04; // [CT_SRC][u32 ms LE] -> box floods CH_ECHO for downlink benchmark
pub const CT_SETTIME: u8 = 0x05; // [CT_SETTIME][u64 unix_seconds LE] -> box sets its clock (no RTC battery)
pub const CT_ETH_START: u8 = 0x06; // [CT_ETH_START][iface bytes?] -> box bridges that netdev (default "ncm0") onto CH_ETH
pub const CT_ETH_STOP: u8 = 0x07; // [CT_ETH_STOP] -> box tears the raw-frame bridge down

// Session-control (docs/carplay/02_SESSION_LIFECYCLE.md lifecycle): the host app is the session's reason to exist. It SUBSCRIBEs
// (announces a live receiver + pushes its ephemeral YAML config), HEARTBEATs to prove liveness, and
// STOPs on clean exit. The box tracks presence and emits SESSION_EVENT on transitions; a heartbeat
// watchdog declares the host gone if beats stop (crash / stall the transport can't otherwise see).
pub const CT_SUBSCRIBE: u8 = 0x10; // host->box [CT_SUBSCRIBE][yaml config bytes...] -> receiver active
pub const CT_STOP: u8 = 0x11; // host->box [CT_STOP] -> receiver stopping (clean)
pub const CT_HEARTBEAT: u8 = 0x12; // host->box [CT_HEARTBEAT] -> liveness ping (send ~1/s)
pub const CT_SESSION_EVENT: u8 = 0x13; // box->host [CT_SESSION_EVENT][SEV_*] -> presence transition
pub const CT_UPLINK: u8 = 0x14; // box->host [CT_UPLINK][state u8][rate u32 LE][ch u8] -> mic-uplink gate.
                                // state 1=on (iPhone opened a type-100 input SETUP; app starts capturing at
                                // rate/ch), 0=off (TEARDOWN; app stops). Mirrors the receiver's `uplink on
                                // <rate> <ch>` / `uplink off` back-channel across OCBM to the host app.
pub const CT_PAIRING_CODE: u8 = 0x15; // box->host [CT_PAIRING_CODE][6 ascii digits | empty] -> the wireless
                                      // SSP Numeric-Comparison code to DISPLAY for the user to match against
                                      // the iPhone. Non-empty = show it; empty payload = clear/hide (pairing
                                      // done or Just-Works). ocbmd mirrors the ssp_agent's /tmp/pairing_code flag.
pub const CT_PHONE_IDENT: u8 = 0x18; // box->host [CT_PHONE_IDENT][utf8 JSON] -> who the connected phone IS.
                                     // {"name","deviceID","model","osName","osVersion"} lifted verbatim from the
                                     // phone's own AirPlay phase-1 SETUP plist (kAirPlayKey_Name and friends,
                                     // AirPlayReceiverServer.c:3213). `name` is what the user typed in Settings ->
                                     // General -> About -> Name; `deviceID` is the BR/EDR MAC, so it MATCHES an
                                     // entry in MGMT_INFO's bonded list and is the only thing that says WHICH
                                     // bonded phone is the live one. Empty payload = no identity yet / cleared.
pub const CT_BT_PHASE: u8 = 0x17; // box->host [CT_BT_PHASE][BTP_*] -> Bluetooth/iAP2 handshake
                                  // progress. The host is NOT in the BT loop (the box owns the
                                  // radio) and SEV_PHONE_* refer to the box's own USB bus, so
                                  // without this a host app has NO signal for the entire BT phase
                                  // and can only poll /tmp/wl.log over a debug console. Mirrored
                                  // from /tmp/bt_phase on change, same discipline as
                                  // CT_PAIRING_CODE. Advisory/monotonic-ish: a host must treat an
                                  // unknown value as "progress" and never gate on ordering.
pub const BTP_IDLE: u8 = 0x00; // no BT session in progress
pub const BTP_LINK_UP: u8 = 0x01; // RFCOMM/iAP2 link established (SYN-ACK)
pub const BTP_AUTHENTICATING: u8 = 0x02; // MFi cert/challenge exchange under way (0xAA01/0xAA03)
pub const BTP_AUTHENTICATED: u8 = 0x03; // 0xAA05 AuthenticationSucceeded
pub const BTP_IDENTIFYING: u8 = 0x04; // 0x1D01 IdentificationInformation sent
pub const BTP_IDENTIFIED: u8 = 0x05; // 0x1D02 accepted — the phone has the accessory
pub const BTP_WIFI_HANDOFF: u8 = 0x06; // 0x5703 sent: the phone now has the hotspot credentials
pub const CT_RADIO: u8 = 0x16; // host->box [CT_RADIO][0=radios off now | 1=radios on if cfg allows]
                               // (docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating: mid-session kill switch; radios-on is otherwise
                               // implied by SUBSCRIBE + config push, and app loss powers radios off)
pub const CT_PROJ_MODE: u8 = 0x19; // box->host [CT_PROJ_MODE][PM_*] -> WHICH projection transport owns the
                                   // box right now. Mirrors the box's single-owner arbitration flag
                                   // `/tmp/projection_owner` (docs/host/02_ANDROID_AUTO.mda), which session_supervisor and
                                   // aa-bridge claim first-come-wins. Without it the app cannot know
                                   // whether the box armed CarPlay or Android Auto, and had to be told by
                                   // hand (the `AA_OCBM` env stand-in). On PM_WIRED_AA the app runs its own
                                   // AA head-unit engine over CH_IP to the box's aa-bridge instead of the
                                   // CarPlay decode path. Emitted on change and re-emitted to every fresh
                                   // SUBSCRIBE, same discipline as CT_BT_PHASE. Advisory: an unknown value
                                   // means "some transport owns the box" — never gate on ordering.
pub const CT_BOX_HEALTH: u8 = 0x1A; // box->host [CT_BOX_HEALTH][flags u8] -> the box's own readiness, as
                                    // a bitmask of BH_*. Emitted on CHANGE and re-emitted to every fresh
                                    // SUBSCRIBE, same discipline as CT_BT_PHASE and CT_PROJ_MODE.
                                    //
                                    // It exists because until now the ONLY way a host could learn anything
                                    // about the box's health was to ASK — MGMT_GET_INFO, a JSON snapshot
                                    // returned on request and nothing else. In practice hosts asked once at
                                    // bring-up and never again, so a box whose hci went down, or whose
                                    // carplay-wireless died mid-session, looked exactly like a healthy one.
                                    // A host cannot decide "am I green AND is the box green" against a
                                    // snapshot it took minutes ago.
                                    //
                                    // Deliberately a bitmask, not JSON: this is on a change-triggered path
                                    // that may fire during a live A/V session, and the point is that it is
                                    // cheap enough to never think twice about. MGMT_INFO remains the place
                                    // to go for detail (identity, bonded list, free space).
                                    //
                                    // Advisory, like its siblings: an unknown bit means "something the host
                                    // does not model", never a reason to refuse a session.
pub const BH_HCI_PRESENT: u8 = 0x01; // hci0 exists AND is UP (HCI_UP in /sys/class/bluetooth/hci0/flags).
                                     // CORRECTED 2026-08-29: this used to test only that the sysfs node
                                     // EXISTED, which survives `hciconfig hci0 down` (wireless_down leaves
                                     // the module attached on purpose), so a mid-session hci-down could not
                                     // clear it. It now reflects the radio's actual power state.
                                     // A CLEAR bit still covers the most important case: no controller
                                     // registered at all, which is what a missing hci_uart module produces
                                     // — no hci0 is ever created while every layer above still reports
                                     // success (docs/ops/06_CORRECTIONS_LEDGER.md R-20W-5). Read this bit FIRST when BT
                                     // "does nothing".
pub const BH_SSP: u8 = 0x02; // Secure Simple Pairing enabled on hci0
pub const BH_IAP2D: u8 = 0x04; // iap2d running (wired CarPlay identify path)
pub const BH_AIRPLAYD: u8 = 0x08; // airplayd running
pub const BH_CARPLAY_WIRELESS: u8 = 0x10; // carplay-wireless running (BT + AP bring-up supervisor)
pub const BH_WLAN_AP: u8 = 0x20; // hostapd running — the box is raising its OWN AP (NOT the GM bridge role)
pub const BH_ROOTFS_OK: u8 = 0x40; // rootfs has headroom; clear means the box is close to full and may
                                   // fail to write logs, configs or the ephemeral session YAML

pub const PM_NONE: u8 = 0x00; // idle — no projection session; either phone kind may claim the box
pub const PM_WIRED_CP: u8 = 0x01; // wired CarPlay (projection_up -> iap2d + airplayd)
pub const PM_WIRELESS_CP: u8 = 0x02; // wireless CarPlay (carplay-wireless: BT + WiFi AP)
pub const PM_WIRED_AA: u8 = 0x03; // wired Android Auto (aa-bridge AOAP pump; app drives AA over CH_IP)
pub const PM_WIRELESS_AA: u8 = 0x04; // reserved — wireless Android Auto (docs/host/02_ANDROID_AUTO.mdf, Phase 3, unbuilt)

pub const SEV_HOST_PRESENT: u8 = 0x01; // subscribed + heartbeat-alive
pub const SEV_HOST_GONE: u8 = 0x02; // STOP, or heartbeat watchdog expired
                                    // Phone presence on the box's phone-facing bus (2026-07-12): the supervisor gates projection on the
                                    // iPhone actually being on the bus and publishes /tmp/phone_present; ocbmd mirrors transitions to the
                                    // host so the app can show a TRUTHFUL "waiting for phone" immediately (not a 20 s no-A/V watchdog).
pub const SEV_PHONE_PRESENT: u8 = 0x03; // iPhone (05ac) on the adapter bus
pub const SEV_PHONE_ABSENT: u8 = 0x04; // no iPhone on the adapter bus — plug one in

// CH_MGMT sub-messages (the app's "CCPA" tab). First payload byte = verb. Host->box requests are the
// low range; box->host responses are the 0x8x range.
//   Host->box:
pub const MGMT_GET_INFO: u8 = 0x01; // [MGMT_GET_INFO] -> box replies MGMT_INFO with a JSON snapshot
pub const MGMT_REBOOT: u8 = 0x02; // [MGMT_REBOOT] -> box ACKs then reboots (fork+delay so the ack flushes)
pub const MGMT_FORGET_ALL: u8 = 0x03; // [MGMT_FORGET_ALL] -> clear all BR/EDR bonds + restart wireless
pub const MGMT_FORGET_DEVICE: u8 = 0x04; // [MGMT_FORGET_DEVICE][ascii MAC "AA:BB:.."] -> drop that one bond
pub const MGMT_RESTART_WIRELESS: u8 = 0x05; // [MGMT_RESTART_WIRELESS] -> bounce carplay-wireless (re-advertise)
                                            //   Box->host:
pub const MGMT_INFO: u8 = 0x81; // [MGMT_INFO][utf8 JSON] — identity + health + bonded-device snapshot
pub const MGMT_ACK: u8 = 0x82; // [MGMT_ACK][verb u8][status u8] — 0 ok / 1 error, echoing the request verb

// modes
pub const MODE_PROJECTION: u8 = 0x00;
pub const MODE_CONSOLE: u8 = 0x01;

// capability bits
pub const CAP_CONSOLE: u32 = 0x0000_0001;
pub const CAP_ECHO: u32 = 0x0000_0002;
pub const CAP_MFI: u32 = 0x0000_0004;
pub const CAP_IP: u32 = 0x0000_0008;
pub const CAP_FILE: u32 = 0x0000_0010;
pub const CAP_ETH: u32 = 0x0000_0020; // box can bridge a raw-ethernet netdev (ncm0) over CH_ETH

// OCBM_CH_IP stream-mux sub-frame types. Payload = [type u8][conn_id u16 LE][data].
// AAOS has no NCM, so L3 is done in userspace over this channel (no kernel TUN needed).
pub const IP_OPEN: u8 = 0x01; // TCP: data = target "host:port"; box connect()s and relays
pub const IP_DATA: u8 = 0x02; // data = stream bytes (TCP) or one datagram (UDP)
pub const IP_CLOSE: u8 = 0x03; // no data
pub const IP_OPEN_UDP: u8 = 0x04; // UDP: data = target "host:port"; box binds + connect()s a UdpSocket

// OCBM_CH_FILE sub-frame types. Host pushes one file at a time to the box so binary
// deploys ride the reliable accessory pipe instead of corruption-prone base64-over-UART.
// Payload = [type u8][...]. The box streams to a "<path>.ocbm.part" temp, verifies the
// end-to-end CRC-32 on close, fchmod()s to the requested mode, then atomically renames
// into place — so a failed transfer never leaves a half-written (or non-exec) binary.
pub const FILE_OPEN: u8 = 0x01; // [FILE_OPEN][mode u32 LE][path bytes] -> box creates <path>.ocbm.part
pub const FILE_DATA: u8 = 0x02; // [FILE_DATA][chunk bytes]            -> box appends to the open temp
pub const FILE_CLOSE: u8 = 0x03; // [FILE_CLOSE][crc32 u32 LE][size u32 LE] -> box verifies, chmods, renames
pub const FILE_ACK: u8 = 0x04; // [FILE_ACK][status u8][crc32 u32 LE][size u32 LE]  box->host
                               // PULL (box->host retrieval, the mirror of push): host sends [FILE_PULL][path bytes]; the box streams
                               // the file back as FILE_DATA sub-frames (box->host direction, same 0x02 opcode) and terminates with a
                               // FILE_ACK carrying (FILE_OK, crc32, size) — the host reassembles and verifies the end-to-end CRC-32.
                               // On a bad path / open failure the box replies with a single FILE_ACK(FILE_ERR_OPEN|FILE_ERR_NOFILE).
pub const FILE_PULL: u8 = 0x05; // [FILE_PULL][path bytes] -> box streams <path> back (FILE_DATA…, then FILE_ACK)
                                // FILE_ACK status: 0 ok · 1 open failed · 2 crc/size mismatch · 3 no open file · 4 write/read/rename failed
pub const FILE_OK: u8 = 0;
pub const FILE_ERR_OPEN: u8 = 1;
pub const FILE_ERR_VERIFY: u8 = 2;
pub const FILE_ERR_NOFILE: u8 = 3;
pub const FILE_ERR_WRITE: u8 = 4;

// OCBM_CH_INPUT sub-frame types (task #20). Payload = [type u8][...]. Coordinates are NORMALIZED
// (u16 LE, 0..=65535 across the display); airplayd scales them to absolute HID coords using the SAME
// resolution it advertised in /info (task #5), so the box stays the single resolution authority.
pub const INPUT_TOUCH: u8 = 0x01; // [INPUT_TOUCH][phase u8][nx u16 LE][ny u16 LE][finger u8]
pub const INPUT_KEYFRAME: u8 = 0x02; // [INPUT_KEYFRAME] -> host asks the box to request an iOS keyframe (task #33)
pub const INPUT_KEYFRAME_ALT: u8 = 0x06; // [INPUT_KEYFRAME_ALT] -> host asks the box to force a keyframe on the
                                     // ALT/cluster stream (VideoStream.Alt1) SPECIFICALLY. A bare INPUT_KEYFRAME only re-IDRs the main
                                     // console (events::send_force_key_frame_stream(None)), so after a cluster view switch (Nav Card/Map/
                                     // Nav App requestUI) the nav feed gaps and stays frozen without addressing its own stream uuid.
                                     // Command/key surface (task #35): the host's media keys + Home/Siri ride CH_INPUT too, so ocbmd's
                                     // opaque relay to airplayd needs no change. Media buttons are HID device uid 2 (the advertised
                                     // "CarLink Media Buttons" Consumer-Control device, uid 2); its 1-byte report is an ARRAY INDEX into
                                     // the descriptor's usage list, and airplayd completes the tap (press index, then release 0). Home /
                                     // Back / D-pad ride a SEPARATE uid-3 D-Pad HID device (Apple's HIDDPadCreateDescriptor), gated behind
                                     // CARPLAY_DPAD and driven by INPUT_NAV — advertising it once broke session reconnect in the sibling
                                     // carplayd project (info.rs INCIDENT 2026-07-06), so it is flag-gated. Siri is an AirPlay `/command`
                                     // (requestSiri), dispatched box-side because only the box owns the encrypted event channel.
pub const INPUT_MEDIA_BTN: u8 = 0x03; // [INPUT_MEDIA_BTN][index u8] -> box taps media-buttons HID uid 2 (press+release)
pub const INPUT_COMMAND: u8 = 0x04; // [INPUT_COMMAND][cmd u8] -> box sends the mapped /command (CMD_*)
pub const INPUT_NAV: u8 = 0x05; // [INPUT_NAV][nav u8] -> box taps the D-Pad HID uid 3 (press+release); NAV_*
pub const INPUT_TELEPHONY: u8 = 0x08; // [INPUT_TELEPHONY][buttonIndex u8] -> box sends the 1-byte HID
                                      // Telephony report (index) then a release (0). 1=Answer(HookSwitch)
                                      // 2=Flash 3=End(Drop) 4=Mute 5..14=DTMF 0..9 15=* 16=# 17=Delete.
pub const INPUT_KNOB: u8 = 0x07; // [INPUT_KNOB][flags u8][nudge_x i8][nudge_y i8][rotation i8] -> box sends
                                 // one report on the Knob HID (uid 4): flags bit0 Select/bit1 Home/bit2 Back,
                                 // signed X/Y nudge (±127 = a 4-way arrow), signed relative rotation (±1/detent).
                                 // This is how the CarPlay Simulator drives ALL cluster/main navigation (0x06 = INPUT_KEYFRAME_ALT).
                                // INPUT_MEDIA_BTN indices — the Consumer-array index into the uid-2 media-buttons HID descriptor
                                // (receiver info.rs::media_buttons_descriptor); MEDIA-TRANSPORT ONLY (Home/Back/nav are the uid-3
                                // D-Pad, INPUT_NAV). 0 = release; the box completes press+release. Wire indices (the contract with
                                // the descriptor's usage order; airplayd forwards the raw index, the host names them in Swift):
                                //   1 play, 2 pause, 3 play/pause, 4 next, 5 prev

// INPUT_NAV actions — the CarPlay D-Pad (HID uid 3, Apple's exact HIDDPadCreateDescriptor). airplayd
// builds the 2-byte variable-bitfield report (byte0 Home/Back, byte1 Menu Select/Up/Down/Left/Right)
// and taps it (press then release). Distinct from the rotary Knob (a separate wheel device).
pub const NAV_UP: u8 = 1;
pub const NAV_DOWN: u8 = 2;
pub const NAV_LEFT: u8 = 3;
pub const NAV_RIGHT: u8 = 4;
pub const NAV_SELECT: u8 = 5;
pub const NAV_HOME: u8 = 6;
pub const NAV_BACK: u8 = 7;
// INPUT_COMMAND values
pub const CMD_REQUEST_UI: u8 = 0x01; // {type:"requestUI"} (bring the accessory UI foreground). NOT the
                                     // CarPlay Home button (that's the uid-3 D-Pad NAV_HOME); box handles
                                     // this if sent, but the host no longer uses it.
pub const CMD_REQUEST_SIRI: u8 = 0x02; // DEPRECATED — bare {type:"requestSiri"}; iOS ignores it (validated 2026-07-11). Use the hold pair below.
                                       // Siri is a HOLD over /command requestSiri with siriAction (SDK enum: prewarm/buttondown/buttonup/
                                       // voiceactivation — docs/carplay/05_METADATA_AND_CONTROLS.md §2.4): send SIRI_DOWN on press, SIRI_UP on release. A keyboard tap
                                       // synthesizes the pair with a short gap host-side.
pub const CMD_SIRI_DOWN: u8 = 0x03; // -> {type:"requestSiri", params:{siriAction: 2}}  INTEGER enum,
pub const CMD_SIRI_UP: u8 = 0x04; // -> {type:"requestSiri", params:{siriAction: 3}}  never a string.
                                  // Navigation / instrument-cluster (type-111) video RUNTIME focus toggle. The alt display must already
                                  // be advertised in /info (CARPLAY_ALTSCREEN); these dynamically start/stop iOS's SECOND encoder without
                                  // a reconnect — the AirPlay equivalent of the old Carlinkit firmware's Cmd 508 RequestNaviScreenFocus /
                                  // 509 ReleaseNaviScreenFocus. START = {type:"requestUI", url:"maps:/car/instrumentcluster/map"};
                                  // STOP = {type:"stopUI", url:…}. Host toggles: ON → frames flow → Nav window opens; OFF → frames stop.
pub const CMD_NAV_START: u8 = 0x05; // requestUI(cluster MAP) — iPhone encodes the cluster map stream
pub const CMD_NAV_STOP: u8 = 0x06; // stopUI(cluster) — iPhone stops the cluster stream
                                   // Cluster CONTENT-TYPE selection (the maneuver/ETA "instruction card" is a distinct content type iOS
                                   // renders into the cluster video — `maps:/car/instrumentcluster/instructioncard` vs `.../map`). Lets
                                   // the host switch which surface iOS encodes so we can find the one that shows the in-video info cards.
pub const CMD_NAV_CARD: u8 = 0x07; // requestUI(cluster INSTRUCTION CARD)
                                   // limitedUI (Drive/Park): restrict the CarPlay UI (keyboard, phone keypad, long lists) as if the
                                   // vehicle shifted into Drive, and release it when parked. Runtime /command, no reconnect / no /info
                                   // change. Wire: {type:"setLimitedUI", params:{limitedUI:<bool>}}. (Apple SDK kAirPlayCommand_SetLimitedUI.)
pub const CMD_LIMITED_UI_ON: u8 = 0x08; // setLimitedUI{limitedUI:true} — restrict (Drive)
pub const CMD_LIMITED_UI_OFF: u8 = 0x09; // setLimitedUI{limitedUI:false} — release (Park)
pub const CMD_NAV_APP: u8 = 0x0A; // requestUI(maps:/car/instrumentcluster) — the "Navigation App" cluster view
// Cluster APPEARANCE toggles — the showUI query string elements Apple exposes in the Simulator's Alt1
// Appearance popover (`showSpeedLimit`/`showCompass`/`showETA`, AirPlayShowUIURL.airPlayURL). Wire form
// is 3 bytes: [INPUT_COMMAND][CMD_NAV_APPEARANCE][flags]. The box stores the flags, rebuilds the CURRENT
// cluster surface URL, re-showUIs it, and forces a cluster IDR so the split-seam host decoder re-syncs.
// Map/App surfaces only — instructioncard carries no query string (matching Apple). Default 0x07 = all on.
pub const CMD_NAV_APPEARANCE: u8 = 0x0B;
pub const NAV_APPEARANCE_SPEED_LIMIT: u8 = 0x01; // set → showSpeedLimit=user, clear → =no
pub const NAV_APPEARANCE_COMPASS: u8 = 0x02;     // set → showCompass=user,    clear → =no
pub const NAV_APPEARANCE_ETA: u8 = 0x04;         // set → showETA=yes,         clear → =no
// Cluster map zoom — the Simulator's Alt1 +/- buttons → changeMapZoomLevel{uuid, zoomDirection}.
// AirPlayZoomDirection: 0 = in (+), 1 = out (-). Cluster-only (VideoStream.Alt1).
pub const CMD_NAV_ZOOM_IN: u8 = 0x0C; // changeMapZoomLevel zoomDirection=0 (+)
pub const CMD_NAV_ZOOM_OUT: u8 = 0x0D; // changeMapZoomLevel zoomDirection=1 (−)

// Display APPEARANCE (Light/Dark) — the Simulator's per-display "UI Appearance" and "Map Appearance"
// pickers, plus the global "Night Mode" toggle. Verified from Apple's CarPlaySDK (events.rs cites the
// exact function offsets and the AppearanceMode/AppearanceSetting enum ints). Wire per command:
//   [INPUT_COMMAND][CMD_UI_APPEARANCE ][stream][mode]  → uiAppearanceUpdate{uuid, appearanceMode, ...}
//   [INPUT_COMMAND][CMD_MAP_APPEARANCE][stream][mode]  → mapAppearanceUpdate{uuid, appearanceMode, ...}
//   [INPUT_COMMAND][CMD_NIGHT_MODE    ][on]            → setNightMode{nightMode}
// stream: 0 = main display (DISPLAY_UUID), 1 = alt/cluster (ALT_DISPLAY_UUID, alt-screen-gated like the
// cluster commands). mode/on: 0 = light/off, 1 = dark/on. UI vs Map is the command, not a payload flag.
pub const CMD_UI_APPEARANCE: u8 = 0x0E;
pub const CMD_MAP_APPEARANCE: u8 = 0x0F;
pub const CMD_NIGHT_MODE: u8 = 0x10;
pub const APPEARANCE_STREAM_MAIN: u8 = 0x00;
pub const APPEARANCE_STREAM_ALT: u8 = 0x01;
pub const APPEARANCE_MODE_LIGHT: u8 = 0x00;
pub const APPEARANCE_MODE_DARK: u8 = 0x01;

// Audio seam framing v2 (task: all-rates/all-streams audio). Both audio seams (media :9002 →
// CH_MEDIA_AUDIO and voice :9003 → CH_ALT_AUDIO) carry length-prefixed messages
// `[u32 BE len][marker][...]` with EVERY message scid-tagged so concurrent streams on one seam
// (e.g. telephony + alert on the voice sink) can never clobber each other:
//   [SEAM_KEY   ][key 32][scid 8 LE]                                    — per-stream ChaCha20 key
//   [SEAM_PKT   ][scid 8 LE][raw encrypted RTP packet]                  — host decrypts
//   [SEAM_FORMAT][scid 8 LE][codec u8][rate u32 LE][ch u8][bits u8][audio_type u8]
// The host keeps per-scid key+format tables and pre-warms a playback path per format. Wired streams
// are all PCM at various rates; the codec byte is the WIRELESS prestage hook (AAC-LC/ELD/OPUS ride
// the identical wire — the box forwards encrypted RTP untouched either way).
// Wire values (consumers parse these as raw byte literals):
//   markers: 0x00 SEAM_KEY, 0x01 SEAM_PKT, 0x02 SEAM_FORMAT
//   SEAM_FORMAT codec: 0 PCM, 1 AAC-LC, 2 AAC-ELD, 3 OPUS
//   SEAM_FORMAT audio_type (the SETUP audioType): 0 media, 1 telephony, 2 speechRecognition (Siri),
//     3 alert, 4 default/absent, 5 compatibility (a MEDIA-carrying PCM fallback, deliberately not
//     folded into 4 — see receiver::forward::tag_voice and docs/carplay/06_AV_PIPELINE.md)
// touch phases
pub const TOUCH_DOWN: u8 = 0x00;
pub const TOUCH_MOVE: u8 = 0x01;
pub const TOUCH_UP: u8 = 0x02;

#[derive(Clone, Copy, Debug)]
pub struct Header {
    pub length: u32,
    pub channel: u16,
    pub flags: u8,
    pub seq: u32,
}

fn hcheck(h: &[u8]) -> u8 {
    h[..11].iter().fold(0u8, |a, &b| a ^ b)
}

/// Running-state seed for [`crc32_update`]. Feed chunks with `crc32_update`, then
/// [`crc32_final`] to get the value. `crc32_final(crc32_update(CRC32_INIT, all)) == crc32(all)`.
pub const CRC32_INIT: u32 = 0xFFFF_FFFF;

/// Compile-time CRC-32 lookup table (reflected, poly 0xEDB88320). One 1 KiB table —
/// deliberately single-table rather than slice-by-4 (4 KiB) to limit cache pressure on the
/// Cortex-A7's small L1D, which the A/V hot path also needs.
const CRC32_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            let mask = (c & 1).wrapping_neg();
            c = (c >> 1) ^ (0xEDB8_8320 & mask);
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

/// IEEE 802.3 / zlib CRC-32 (reflected, poly 0xEDB88320), dependency-free and table-driven
/// (single 1 KiB table, fits L1D; ~8x over the old bitwise loop on the Cortex-A7).
/// Incremental so the box can verify a pushed file as it streams. Matches Python's
/// `zlib.crc32`, so a deployed file can be cross-checked with standard tooling.
pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc = (crc >> 8) ^ CRC32_TABLE[((crc ^ b as u32) & 0xFF) as usize];
    }
    crc
}

/// Finalize a running CRC from [`crc32_update`] (applies the output inversion).
pub fn crc32_final(crc: u32) -> u32 {
    !crc
}

/// One-shot CRC-32 over a full buffer.
pub fn crc32(data: &[u8]) -> u32 {
    crc32_final(crc32_update(CRC32_INIT, data))
}

impl Header {
    /// Serialize into `buf[..HDR_LEN]`.
    pub fn write(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        buf[4..8].copy_from_slice(&self.length.to_le_bytes());
        buf[8..10].copy_from_slice(&self.channel.to_le_bytes());
        buf[10] = self.flags;
        buf[11] = hcheck(&buf[..11]);
        buf[12..16].copy_from_slice(&self.seq.to_le_bytes());
    }
    /// Parse and validate (magic + hcheck) a 16-byte header.
    pub fn parse(buf: &[u8]) -> Option<Header> {
        if buf.len() < HDR_LEN {
            return None;
        }
        if u32::from_le_bytes(buf[0..4].try_into().unwrap()) != MAGIC {
            return None;
        }
        if buf[11] != hcheck(&buf[..11]) {
            return None;
        }
        Some(Header {
            length: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            channel: u16::from_le_bytes(buf[8..10].try_into().unwrap()),
            flags: buf[10],
            seq: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
        })
    }
}

/// Build a full frame (header + payload) into `out`; returns total byte count.
/// Contract: `out.len() >= HDR_LEN + payload.len()` and `payload.len() <= MAX_PAYLOAD`
/// (a receiver's [`Reassembler::next`] rejects any frame whose declared length exceeds `MAX_PAYLOAD`).
pub fn frame(out: &mut [u8], channel: u16, flags: u8, seq: u32, payload: &[u8]) -> usize {
    debug_assert!(
        payload.len() <= MAX_PAYLOAD,
        "frame payload exceeds MAX_PAYLOAD"
    );
    debug_assert!(
        out.len() >= HDR_LEN + payload.len(),
        "frame out buffer too small"
    );
    let h = Header {
        length: payload.len() as u32,
        channel,
        flags,
        seq,
    };
    h.write(&mut out[..HDR_LEN]);
    out[HDR_LEN..HDR_LEN + payload.len()].copy_from_slice(payload);
    HDR_LEN + payload.len()
}

/// Build ONLY the 16-byte OCBM frame header (magic + hcheck) into a stack buffer — for VECTORED writes
/// that keep the payload uncopied (`writev([header, payload])`). Byte-identical to the header
/// [`frame_into`] produces, so a partial vectored write can be finished by re-queuing the same frame and
/// resuming at the already-written offset. `len` is the payload length (caller ensures `<= MAX_PAYLOAD`).
pub fn write_header(out: &mut [u8; HDR_LEN], channel: u16, flags: u8, seq: u32, len: usize) {
    debug_assert!(len <= MAX_PAYLOAD, "header length exceeds MAX_PAYLOAD");
    Header {
        length: len as u32,
        channel,
        flags,
        seq,
    }
    .write(out);
}

/// Append a full frame (header + payload) to a growable buffer — the zero-intermediate-copy variant
/// of [`frame`] for callers that queue frames: the 16-byte header is built on the stack and the
/// payload is copied exactly ONCE, into its final resting place.
pub fn frame_into(out: &mut Vec<u8>, channel: u16, flags: u8, seq: u32, payload: &[u8]) {
    debug_assert!(payload.len() <= MAX_PAYLOAD, "frame payload exceeds MAX_PAYLOAD");
    let mut hdr = [0u8; HDR_LEN];
    Header {
        length: payload.len() as u32,
        channel,
        flags,
        seq,
    }
    .write(&mut hdr);
    out.reserve(HDR_LEN + payload.len());
    out.extend_from_slice(&hdr);
    out.extend_from_slice(payload);
}

/// Error from the checked framing helpers: the payload exceeds [`MAX_PAYLOAD`], so a frame built from it
/// would carry a declared length every receiver's [`Reassembler::next`] rejects — emitting it silently
/// loses the frame on the reliable OCBM stream and churns the peer's byte-resync. The checked variants
/// surface this so the caller drops the whole message loudly instead of queuing a corrupt frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Oversize {
    pub len: usize,
    pub max: usize,
}

/// Checked [`frame`]: `Err(Oversize)` if `payload.len() > MAX_PAYLOAD`, emitting nothing; otherwise
/// identical to `frame`. `out` must still be `>= HDR_LEN + payload.len()`.
pub fn try_frame(
    out: &mut [u8],
    channel: u16,
    flags: u8,
    seq: u32,
    payload: &[u8],
) -> Result<usize, Oversize> {
    if payload.len() > MAX_PAYLOAD {
        return Err(Oversize { len: payload.len(), max: MAX_PAYLOAD });
    }
    Ok(frame(out, channel, flags, seq, payload))
}

/// Checked [`frame_into`]: `Err(Oversize)` if `payload.len() > MAX_PAYLOAD`, appending nothing; otherwise
/// identical to `frame_into`. The safety net that stops a future uncapped caller from silently corrupting
/// the reliable OCBM stream.
pub fn try_frame_into(
    out: &mut Vec<u8>,
    channel: u16,
    flags: u8,
    seq: u32,
    payload: &[u8],
) -> Result<(), Oversize> {
    if payload.len() > MAX_PAYLOAD {
        return Err(Oversize { len: payload.len(), max: MAX_PAYLOAD });
    }
    frame_into(out, channel, flags, seq, payload);
    Ok(())
}

/// Streaming frame reassembler: `push()` bulk reads, `next()` pops complete frames.
/// Resyncs on the magic + hcheck, so a mid-stream byte loss self-heals. Uses a read cursor
/// (`start`) rather than draining from the front, so popping a frame is O(1) amortized instead
/// of O(n) — the consumed prefix is compacted lazily to keep memory bounded.
pub struct Reassembler {
    buf: Vec<u8>,
    start: usize, // read cursor: `buf[start..]` is the unconsumed bytes
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::new() // keep the capacity reservation new() makes (satisfies clippy::new_without_default)
    }
}

impl Reassembler {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(HDR_LEN + MAX_PAYLOAD),
            start: 0,
        }
    }
    pub fn push(&mut self, data: &[u8]) {
        self.compact();
        self.buf.extend_from_slice(data);
    }
    /// Reclaim the consumed prefix. Called after popping / on push so `buf` cannot grow without
    /// bound as the cursor advances; a fully-drained buffer resets to empty.
    fn compact(&mut self) {
        if self.start >= self.buf.len() {
            self.buf.clear();
            self.start = 0;
        } else if self.start > HDR_LEN + MAX_PAYLOAD {
            self.buf.drain(0..self.start);
            self.start = 0;
        }
    }
    /// Pop the next complete frame, copying its payload into `out`.
    /// Returns `(channel, flags, payload_len)` or `None` if no complete frame is buffered.
    /// Contract: `out.len() >= MAX_PAYLOAD` (every accepted frame fits, since `next` rejects any
    /// declared length above `MAX_PAYLOAD`). Callers in this workspace always pass a `MAX_PAYLOAD`
    /// buffer; a smaller buffer is a programming error, asserted in debug builds.
    pub fn next(&mut self, out: &mut [u8]) -> Option<(u16, u8, usize)> {
        debug_assert!(
            out.len() >= MAX_PAYLOAD,
            "Reassembler::next requires out.len() >= MAX_PAYLOAD"
        );
        loop {
            let avail = self.buf.len() - self.start;
            if avail < HDR_LEN {
                self.compact();
                return None; // need more bytes for a header
            }
            let h = match Header::parse(&self.buf[self.start..self.start + HDR_LEN]) {
                Some(h) => h,
                None => {
                    self.start += 1; // bad magic/hcheck: resync one byte and retry
                    continue;
                }
            };
            let plen = h.length as usize;
            if plen > MAX_PAYLOAD {
                self.start += 1; // implausible length: treat the header as junk and resync
                continue;
            }
            let total = HDR_LEN + plen;
            if avail < total {
                self.compact();
                return None; // valid header, waiting on the rest of the payload
            }
            if plen > out.len() {
                // Caller violated the `out.len() >= MAX_PAYLOAD` contract. Drop the whole (in-sync)
                // frame rather than panic on the copy — stays framed, no desync. Asserted in debug.
                debug_assert!(
                    false,
                    "out buffer ({}) smaller than frame payload ({plen})",
                    out.len()
                );
                self.start += total;
                self.compact();
                continue;
            }
            out[..plen].copy_from_slice(&self.buf[self.start + HDR_LEN..self.start + total]);
            self.start += total;
            self.compact();
            return Some((h.channel, h.flags, plen));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f_replay_is_a_distinct_bit_that_does_not_disturb_framing() {
        // Bit2 was documented reserved and is validated by no receiver on either end, so the two
        // sides may adopt it in either order. That only holds while it stays clear of SOM/EOM and
        // the Reassembler keeps ignoring flags for framing.
        assert_eq!(F_REPLAY, 0x04);
        assert_eq!(F_REPLAY & (F_SOM | F_EOM), 0);

        let mut buf = [0u8; HDR_LEN + 8];
        let n = frame(&mut buf, CH_CTRL, F_SOM | F_EOM | F_REPLAY, 3, &[CT_BT_PHASE, BTP_IDENTIFIED]);
        let mut r = Reassembler::new();
        r.push(&buf[..n]);
        let mut out = vec![0u8; MAX_PAYLOAD];
        let (ch, fl, len) = r.next(&mut out).expect("a replay-flagged frame is still a frame");
        assert_eq!(ch, CH_CTRL);
        assert_eq!(fl, F_SOM | F_EOM | F_REPLAY, "the flag must survive the round trip");
        assert_eq!(&out[..len], &[CT_BT_PHASE, BTP_IDENTIFIED]);
        assert!(r.next(&mut out).is_none());
    }

    #[test]
    fn ct_box_health_does_not_collide_with_the_ctrl_space() {
        // docs/carplay/01_OCBM_PROTOCOL.md "Self-describing streams" now says the CT_* space is 0x01-0x1A.
        assert_eq!(CT_BOX_HEALTH, 0x1A);
        assert_ne!(CT_BOX_HEALTH, CT_PROJ_MODE);
        assert_ne!(CT_BOX_HEALTH, CT_BT_PHASE);
    }
    #[test]
    fn roundtrip() {
        let mut buf = [0u8; 64];
        let n = frame(&mut buf, CH_ECHO, F_SOM | F_EOM, 7, b"hello");
        let h = Header::parse(&buf[..HDR_LEN]).unwrap();
        assert_eq!(h.channel, CH_ECHO);
        assert_eq!(h.length, 5);
        assert_eq!(h.seq, 7);
        assert_eq!(&buf[HDR_LEN..n], b"hello");
    }
    #[test]
    fn frame_into_matches_frame() {
        // frame_into must produce byte-for-byte the same frame as frame() for the same inputs,
        // across an empty payload, a small one, and a full MAX_PAYLOAD one.
        for payload in [
            &b""[..],
            &b"hello"[..],
            &vec![0xA5u8; MAX_PAYLOAD][..],
        ] {
            let mut flat = vec![0u8; HDR_LEN + payload.len()];
            let n = frame(&mut flat, CH_VIDEO, F_SOM | F_EOM, 0xDEAD_BEEF, payload);
            let mut grown = Vec::new();
            frame_into(&mut grown, CH_VIDEO, F_SOM | F_EOM, 0xDEAD_BEEF, payload);
            assert_eq!(&flat[..n], &grown[..]);
        }
    }
    #[test]
    fn reassemble_with_resync() {
        let mut r = Reassembler::new();
        let mut f = [0u8; 64];
        let n = frame(&mut f, CH_CTRL, F_SOM, 1, &[0xAB, 0xCD]);
        r.push(&[0xFF, 0x00]); // junk prefix
        r.push(&f[..n]);
        let mut out = vec![0u8; MAX_PAYLOAD];
        let (ch, _fl, l) = r.next(&mut out).unwrap();
        assert_eq!(ch, CH_CTRL);
        assert_eq!(&out[..l], &[0xAB, 0xCD]);
        assert!(r.next(&mut out).is_none());
    }
    #[test]
    fn reassemble_coalesced_multiple_frames() {
        // Two whole frames delivered in ONE push must both come out via successive next() calls.
        let mut r = Reassembler::new();
        let (mut a, mut b) = ([0u8; 64], [0u8; 64]);
        let na = frame(&mut a, CH_MFI, F_SOM | F_EOM, 1, b"one");
        let nb = frame(&mut b, CH_CTRL, F_SOM | F_EOM, 2, b"two!!");
        let mut both = Vec::new();
        both.extend_from_slice(&a[..na]);
        both.extend_from_slice(&b[..nb]);
        r.push(&both);
        let mut out = vec![0u8; MAX_PAYLOAD];
        let (c1, _, l1) = r.next(&mut out).unwrap();
        assert_eq!((c1, &out[..l1]), (CH_MFI, &b"one"[..]));
        let (c2, _, l2) = r.next(&mut out).unwrap();
        assert_eq!((c2, &out[..l2]), (CH_CTRL, &b"two!!"[..]));
        assert!(r.next(&mut out).is_none());
    }
    #[test]
    fn reassemble_split_header_across_pushes() {
        // A frame whose header is split across two push() calls must still parse.
        let mut r = Reassembler::new();
        let mut f = [0u8; 64];
        let n = frame(&mut f, CH_VIDEO, F_SOM | F_EOM, 9, b"pixels");
        r.push(&f[..7]); // mid-header
        let mut out = vec![0u8; MAX_PAYLOAD];
        assert!(r.next(&mut out).is_none());
        r.push(&f[7..n]); // the rest
        let (ch, _, l) = r.next(&mut out).unwrap();
        assert_eq!((ch, &out[..l]), (CH_VIDEO, &b"pixels"[..]));
    }
    #[test]
    fn resync_over_midstream_junk_between_frames() {
        // Junk (including a stray copy of the magic's low byte) between two frames must be skipped.
        let mut r = Reassembler::new();
        let (mut a, mut b) = ([0u8; 64], [0u8; 64]);
        let na = frame(&mut a, CH_CTRL, F_SOM | F_EOM, 1, b"aa");
        let nb = frame(&mut b, CH_ECHO, F_SOM | F_EOM, 2, b"bb");
        r.push(&a[..na]);
        r.push(&[0x4D, 0x00, 0x42, 0xFF, 0x11]); // junk, includes a 'M' (magic byte)
        r.push(&b[..nb]);
        let mut out = vec![0u8; MAX_PAYLOAD];
        let (c1, _, l1) = r.next(&mut out).unwrap();
        assert_eq!((c1, &out[..l1]), (CH_CTRL, &b"aa"[..]));
        let (c2, _, l2) = r.next(&mut out).unwrap();
        assert_eq!((c2, &out[..l2]), (CH_ECHO, &b"bb"[..]));
    }
    #[test]
    fn oversize_declared_length_is_rejected_and_resyncs() {
        // A header with a valid magic+hcheck but a length > MAX_PAYLOAD must not stall; the reassembler
        // treats it as junk, resyncs, and still recovers a real frame that follows.
        let mut r = Reassembler::new();
        let mut bad = [0u8; HDR_LEN];
        Header {
            length: (MAX_PAYLOAD as u32) + 1,
            channel: CH_CTRL,
            flags: 0,
            seq: 0,
        }
        .write(&mut bad);
        r.push(&bad);
        let mut good = [0u8; 64];
        let n = frame(&mut good, CH_CTRL, F_SOM | F_EOM, 1, b"ok");
        r.push(&good[..n]);
        let mut out = vec![0u8; MAX_PAYLOAD];
        let (ch, _, l) = r.next(&mut out).unwrap();
        assert_eq!((ch, &out[..l]), (CH_CTRL, &b"ok"[..]));
    }
    #[test]
    fn crc32_known_vector() {
        // Canonical CRC-32/ISO-HDLC check value for "123456789".
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0x0000_0000);
    }
    #[test]
    fn eth_frame_roundtrips_through_reassembler() {
        // A full 1514-byte ethernet frame (14 hdr + 1500 MTU) must survive CH_ETH framing.
        let mut r = Reassembler::new();
        let frame_bytes: Vec<u8> = (0..1514u32).map(|i| (i ^ 0x5A) as u8).collect();
        let mut buf = vec![0u8; HDR_LEN + frame_bytes.len()];
        let n = frame(&mut buf, CH_ETH, F_SOM | F_EOM, 3, &frame_bytes);
        r.push(&buf[..n]);
        let mut out = vec![0u8; MAX_PAYLOAD];
        let (ch, _fl, l) = r.next(&mut out).unwrap();
        assert_eq!(ch, CH_ETH);
        assert_eq!(&out[..l], &frame_bytes[..]);
    }
    #[test]
    fn crc32_incremental_matches_oneshot() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 37 + 11) as u8).collect();
        let mut c = CRC32_INIT;
        for chunk in data.chunks(7) {
            c = crc32_update(c, chunk);
        }
        assert_eq!(crc32_final(c), crc32(&data));
    }
    #[test]
    fn try_frame_rejects_oversize_accepts_max() {
        let over = vec![0u8; MAX_PAYLOAD + 1];
        let at_max = vec![0u8; MAX_PAYLOAD];
        let mut out = vec![0u8; HDR_LEN + MAX_PAYLOAD + 1];
        // try_frame: over → Err (nothing emitted), exactly MAX_PAYLOAD → Ok.
        assert!(try_frame(&mut out, CH_CTRL, F_SOM | F_EOM, 1, &over).is_err());
        assert!(try_frame(&mut out, CH_CTRL, F_SOM | F_EOM, 1, &at_max).is_ok());
        // try_frame_into: over → Err and appends NOTHING; MAX_PAYLOAD and a small payload → Ok.
        let mut v = Vec::new();
        assert!(try_frame_into(&mut v, CH_CTRL, F_SOM | F_EOM, 1, &over).is_err());
        assert!(v.is_empty(), "try_frame_into must append nothing on Err");
        assert!(try_frame_into(&mut v, CH_CTRL, F_SOM | F_EOM, 1, &at_max).is_ok());
        assert_eq!(v.len(), HDR_LEN + MAX_PAYLOAD);
        v.clear();
        assert!(try_frame_into(&mut v, CH_CTRL, F_SOM | F_EOM, 1, b"ok").is_ok());
        assert_eq!(v.len(), HDR_LEN + 2);
    }
}
