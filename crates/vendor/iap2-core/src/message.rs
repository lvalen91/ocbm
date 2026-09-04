//! iap2/message.rs — iAP2 control-session message BODIES: the TLV substrate and the 0x1D01 builder.
//!
//! PURPOSE. Builds the payloads that ride inside link packets — chiefly `build_ident_info`, the
//! `0x1D01` IdentifyInformation that tells the iPhone who we are and what CarPlay transport we offer.
//! Pure bytes, unit-tested. Faithful port of `iap2_pi.c`'s `bb_*`/`tlv_*`/`build_ident_info`.
//!
//! TLV group layout: `[group_len BE16][param_id BE16][value...]`, `group_len = 4 + value.len()`;
//! strings are NUL-terminated, `none` groups carry no value.
//!
//! ⚠️ SESSION-CRITICAL — `declare_wired`. `build_ident_info(..., declare_wired)` optionally adds the
//! wired-CarPlay message ids `0x4301`(sent)/`0x4300`(received). Per Apple's authoritative catalog
//! these are `CarPlayStartSession` (0x4301, source=Accessory) and `CarPlayAvailability` (0x4300,
//! source=Device). It MUST be passed FALSE. Declaring the pair makes the iPhone commit to a wired
//! `CarPlayStartSession` (0x4301) session-start handshake and abandon the mDNS path — but we never
//! send `0x4301`, so it stalls and NCM never comes up. The NCM/CarPlay bearer is actually brought up
//! DECLARATIVELY by the USBHostTransportComponent (param 16, `TransportSupportsCarPlay`) below — NOT
//! by declaring wired. (Device-verified 2026-07-05; ref ncm_carplayd/ccpa/probes/iap2_auth.c:326-332.)

use crate::spec;

/// A TLV-group builder that grows a `Vec` (the C used a fixed caller buffer with an `ok` flag).
#[derive(Debug, Default)]
pub struct Tlv(pub Vec<u8>);

impl Tlv {
    pub fn new() -> Self {
        Self::default()
    }

    fn be16(&mut self, v: u16) {
        self.0.push((v >> 8) as u8);
        self.0.push((v & 0xff) as u8);
    }

    /// A null-terminated string parameter.
    pub fn str(&mut self, pid: u16, s: &str) {
        let n = s.len() + 1;
        debug_assert!(4 + n <= u16::MAX as usize, "TLV str length overflows u16"); // audit Fix #23
        self.be16((4 + n) as u16);
        self.be16(pid);
        self.0.extend_from_slice(s.as_bytes());
        self.0.push(0);
    }

    /// A raw-bytes parameter.
    pub fn bytes(&mut self, pid: u16, d: &[u8]) {
        debug_assert!(4 + d.len() <= u16::MAX as usize, "TLV bytes length overflows u16"); // audit Fix #23
        self.be16((4 + d.len()) as u16);
        self.be16(pid);
        self.0.extend_from_slice(d);
    }

    pub fn u8v(&mut self, pid: u16, v: u8) {
        self.be16(5);
        self.be16(pid);
        self.0.push(v);
    }

    pub fn u16v(&mut self, pid: u16, v: u16) {
        self.be16(6);
        self.be16(pid);
        self.be16(v);
    }

    /// A `uint32` parameter (big-endian). Added for the CarPlay session-start builders
    /// (`session.rs`, e.g. `CarPlayStartSession` param 2 `Port`).
    pub fn u32v(&mut self, pid: u16, v: u32) {
        self.be16(8);
        self.be16(pid);
        self.0.extend_from_slice(&v.to_be_bytes());
    }

    /// An `int32` parameter (big-endian, two's complement) — e.g. `AssetInformation.AssetVersion`.
    pub fn i32v(&mut self, pid: u16, v: i32) {
        self.be16(8);
        self.be16(pid);
        self.0.extend_from_slice(&v.to_be_bytes());
    }

    /// A `bool` parameter — a single 0/1 byte (the iAP2 `bool` scalar type, distinct from the
    /// zero-length `none` presence type).
    pub fn boolv(&mut self, pid: u16, v: bool) {
        self.be16(5);
        self.be16(pid);
        self.0.push(v as u8);
    }

    /// A presence-only (empty value) parameter.
    pub fn none(&mut self, pid: u16) {
        self.be16(4);
        self.be16(pid);
    }
}

/// A single group: `[4+vlen BE16][pid BE16][value]` — the `group_one` helper (message-id list wrap).
pub fn group_one(pid: u16, value: &[u8]) -> Vec<u8> {
    let mut g = Tlv::new();
    g.bytes(pid, value);
    g.0
}

/// A raw BE16 list (used for the sent/received message-id lists inside params 6/7).
fn be16_list(ids: &[u16]) -> Vec<u8> {
    let mut v = Vec::with_capacity(ids.len() * 2);
    for &id in ids {
        v.extend_from_slice(&id.to_be_bytes());
    }
    v
}

/// Concatenate two id lists, dropping duplicates and preserving `base` order first. A duplicate id
/// in params 6/7 is malformed, and the feature table deliberately overlaps the wired-proven floor.
fn union(base: &[u16], extra: &[u16]) -> Vec<u16> {
    let mut out = base.to_vec();
    for &id in extra {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// IdentificationInformation param 6 (`MessagesSentByAccessory`): the exhaustive set of message ids
/// this accessory will SEND. Every id here is `Source::Accessory` in Apple's catalog — asserted by
/// `tests::declared_message_lists_are_direction_correct`. Names are Apple's authoritative message
/// names (see `spec::IAP2_MESSAGES`).
const SENT_MSG_IDS: &[u16] = &[
    spec::MSG_START_CALL_STATE_UPDATES,     // 0x4154
    spec::MSG_START_NOW_PLAYING_UPDATES,    // 0x5000
    spec::MSG_STOP_NOW_PLAYING_UPDATES,     // 0x5002
    spec::MSG_START_ROUTE_GUIDANCE_UPDATES, // 0x5200
    spec::MSG_STOP_ROUTE_GUIDANCE_UPDATES,  // 0x5203
    // Call-control cluster 0x415A–0x4161 (all Source::Accessory). ⚠️ never wire-captured — see the
    // SESSION-CRITICAL incident note in `build_ident_info`.
    spec::MSG_INITIATE_CALL,      // 0x415A
    spec::MSG_ACCEPT_CALL,        // 0x415B
    spec::MSG_END_CALL,           // 0x415C
    spec::MSG_SWAP_CALLS,         // 0x415D
    spec::MSG_MERGE_CALLS,        // 0x415E
    spec::MSG_HOLD_STATUS_UPDATE, // 0x415F
    spec::MSG_MUTE_STATUS_UPDATE, // 0x4160
    spec::MSG_SEND_DTMF,          // 0x4161
];

/// IdentificationInformation param 7 (`MessagesReceivedFromDevice`): the exhaustive set of message
/// ids this accessory expects to RECEIVE. Every id here is `Source::Device` in Apple's catalog —
/// asserted by `tests::declared_message_lists_are_direction_correct`.
const RCV_MSG_IDS: &[u16] = &[
    spec::MSG_DEVICE_LANGUAGE_UPDATE,                   // 0x4E0A
    spec::MSG_DEVICE_TIME_UPDATE,                       // 0x4E0B
    spec::MSG_DEVICE_TRANSPORT_IDENTIFIER_NOTIFICATION, // 0x4E0E
    spec::MSG_CALL_STATE_UPDATE,                        // 0x4155
    spec::MSG_NOW_PLAYING_UPDATE,                       // 0x5001
    spec::MSG_ROUTE_GUIDANCE_UPDATE,                    // 0x5201
    spec::MSG_ROUTE_GUIDANCE_MANEUVER_INFORMATION,      // 0x5202
];

/// Which bearer this identify declaration is for — selects which transport-component group (param
/// 16 or 24) `build_ident_info` builds. NOTE (corrected 2026-07-24, was stale/wrong): the rest of the
/// body is NOT uniformly shared — the message-id lists (params 6/7) are entirely bearer-specific, and
/// param 20's Identifier sub differs (1 wireless vs 2 wired). Only param 30 (as of docs/wireless/00_WIRELESS_CARPLAY.md Phase 5.0)
/// and the plain scalar params (0-5, 8-9, 12-13) are genuinely transport-independent.
#[derive(Debug, Clone, Copy)]
pub enum TransportComponent {
    /// USB-host transport (param 16, `IAP2-USBHost`), carrying the FunctionFS CarPlay interface
    /// number. This is the ONLY variant in production use today (wired CarPlay).
    Usb { cp_iface: u8 },
    /// Bluetooth/wireless CarPlay transport (param 24, `IAP2-Wireless`) — byte structure confirmed
    /// against `carlink_linux`'s own real BT capture fixture (see docs/carplay/04_CAPABILITIES_AND_CONFIG.md): component identifier 2,
    /// name "IAP2-Wireless", plus the `transportSupportsiAP2Connection`/`transportSupportsCarPlay`
    /// presence flags at sub-ids 2 and 4. No interface-number field — there's no USB analogue here.
    ///
    /// ⚠️ Device-confirmed 2026-07-07 (docs/carplay/04_CAPABILITIES_AND_CONFIG.md): param 24 alone is NOT sufficient -- a real iPhone
    /// rejected (`0x1D03`) an identify declaration missing the companion `bluetoothTransportComponent`
    /// (param 17) that the reference's own captured fixture always sends alongside it. `bt_mac` is
    /// that param's accessory-BT-MAC field -- confirmed cosmetic by the reference's own comment ("any
    /// 6 bytes accepted"), but its *presence* is load-bearing.
    Wireless { bt_mac: [u8; 6] },
    /// docs/wireless/00_WIRELESS_CARPLAY.md (2026-07-24): a SEPARATE iAP2 session run over the AirPlay `iAPSendMessage` tunnel,
    /// per Apple's own CarPlay Communication Plug-in R14G17 Integration Guide: once a wireless CarPlay
    /// session is established, "you must start a new iAP2 session over the CarPlay control channel"
    /// and "perform the full iAP handshaking... which includes the detect sequence and link
    /// synchronization" — the BT-time Identify (`Wireless` above) is ONLY for the WiFi handoff
    /// bootstrap and, per docs/wireless/00_WIRELESS_CARPLAY.md Phase 5.1/5.2's live-hardware findings, categorically rejects any
    /// metadata message-id growth in its params 6/7. This variant is the actual, correct home for
    /// those ids: same transport-component shape as `Wireless` (still describing a wireless-CarPlay-
    /// capable accessory), but params 6/7 declare the metadata Start*/Update message ids instead of
    /// the WiFi-handoff-only baseline. Same `bt_mac` field for the same reason `Wireless` has one
    /// (param 17's companion-presence requirement; value is cosmetic).
    AirPlayTunnel { bt_mac: [u8; 6] },
}

/// The per-device accessory name the iPhone displays for this box, e.g. `"CarLink-b0df"`. `base` is the
/// brand (`"CarLink"`); the suffix is the **last two hex octets of the Wi-Fi MAC** — the SAME derivation
/// as the Wi-Fi SSID (`ccpa-<suffix>`) and the low-level BT name in `bt_on.sh`, so a box shows ONE
/// consistent, distinct name on EVERY transport. Falls back to the SoC serial's last 4 hex when wlan0
/// isn't up (radio-independent, always present), and to bare `base` if neither source is readable.
/// Shared by the wired (`iap2d`) and wireless (`carplay-wireless`) identify paths so multiple boxes stop
/// collapsing into one iOS "car" — this is the human-readable half of the per-box identity story (the
/// cryptographic half is `wireless::box_identity`, which hashes the same hardware id).
pub fn accessory_name(base: &str) -> String {
    match device_suffix() {
        Some(sfx) => format!("{base}-{sfx}"),
        None => base.to_string(),
    }
}

/// The device suffix: the Wi-Fi MAC's last two octets (matching the SSID `ccpa-<suffix>`) when wlan0 is
/// up, else the box serial's last 4 hex — radio-independent, so a WIRED-ONLY box (radios never up) still
/// gets a per-device name. `/etc/serial_number` is the box's real serial file (the same fallback
/// `wlan_on.sh`/`bt_on.sh` use); `/sys/devices/soc0/serial_number` is a last resort (empty on this SoC).
/// `None` only on a dev host with none of them present (reads bare `base`).
fn device_suffix() -> Option<String> {
    for path in [
        "/sys/class/net/wlan0/address",
        "/etc/serial_number",
        "/sys/devices/soc0/serial_number",
    ] {
        if let Ok(s) = std::fs::read_to_string(path) {
            if let Some(sfx) = suffix_from(&s) {
                return Some(sfx);
            }
        }
    }
    None
}

/// Pure suffix extractor (testable): the last 4 hex digits of `raw`, lowercased. A Wi-Fi MAC
/// `"00:e0:4c:91:b0:df"` → `"b0df"`; a serial `"…AB12CD34"` → `"cd34"`. `None` if fewer than 4 hex chars.
fn suffix_from(raw: &str) -> Option<String> {
    let hex: String = raw.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    (hex.len() >= 4).then(|| hex[hex.len() - 4..].to_ascii_lowercase())
}

/// Build the IdentifyInformation body (the `0x1D01` payload) — the accessory's capability declaration.
/// `declare_wired` adds the wired-CarPlay message ids (`0x4301` sent, `0x4300` received) — only
/// meaningful (and only ever passed true) alongside `TransportComponent::Usb`.
///
/// This is the DEFAULT emission: it delegates to [`build_ident_info_excluding`] with an empty
/// exclusion set, so its output is byte-for-byte identical to the historical device-verified payload
/// (the byte-exact tests below pin this).
pub fn build_ident_info(name: &str, transport: TransportComponent, declare_wired: bool) -> Vec<u8> {
    build_ident_info_excluding(name, transport, declare_wired, &[])
}

/// Append a complete `[len BE16][pid BE16][value]` TLV group to `out`, but only when `pid` is not in
/// `exclude`. This is the mechanism behind the `0x1D03 IdentificationRejected` parameter-strip retry
/// (§1.4 of the Apple behavioral reference): on a reject, the device tells us which top-level param
/// ids it refused; we rebuild the identity payload OMITTING exactly those, and re-send once.
fn push_group_if(out: &mut Vec<u8>, exclude: &[u16], pid: u16, group: &[u8]) {
    if !exclude.contains(&pid) {
        out.extend_from_slice(group);
    }
}

/// REQUIRED IdentificationInformation params (Apple iAP2 spec): identity (Name…HardwareVersion, 0-5),
/// the two message-list params (6/7), PowerProvidingCapability (8), MaximumCurrentDrawnFromDevice (9),
/// CurrentLanguage (12), and SupportedLanguage (13). A 0x1D03 retry must retain these even if iOS
/// rejected one — see [`build_ident_info_excluding`] (#407).
const REQUIRED_IDENT_PARAMS: &[u16] = &[
    spec::ident::NAME,
    spec::ident::MODEL_IDENTIFIER,
    spec::ident::MANUFACTURER,
    spec::ident::SERIAL_NUMBER,
    spec::ident::FIRMWARE_VERSION,
    spec::ident::HARDWARE_VERSION,
    spec::ident::MESSAGES_SENT_BY_ACCESSORY,
    spec::ident::MESSAGES_RECEIVED_FROM_DEVICE,
    spec::ident::POWER_PROVIDING_CAPABILITY,
    spec::ident::MAXIMUM_CURRENT_DRAWN_FROM_DEVICE,
    spec::ident::CURRENT_LANGUAGE,
    spec::ident::SUPPORTED_LANGUAGE,
];

/// Build the IdentifyInformation body (`0x1D01`) OMITTING any top-level param whose id is in
/// `exclude`. With `exclude == &[]` this is byte-identical to [`build_ident_info`] (guaranteed by the
/// byte-exact tests). It is the retry-path builder for `0x1D03 IdentificationRejected`: decode the
/// rejected param ids via [`parse_rejected_param_ids`], pass them here, and re-send the result as a
/// fresh `0x1D01`.
///
/// Only TOP-LEVEL params are excludable (matching Apple's reference, which strips whole rejected
/// components/fields — e.g. `exclude = &[24]` drops the WirelessCarPlayTransportComponent group).
/// Sub-parameters within a group are never individually stripped.
pub fn build_ident_info_excluding(
    name: &str,
    transport: TransportComponent,
    declare_wired: bool,
    exclude: &[u16],
) -> Vec<u8> {
    // Forward the ambient metadata policy + process-cached skip into the injection form (audit Fix #25).
    // Byte-identical to reading them inline; the seam lets a test pin them (see the _with_policy form).
    build_ident_info_excluding_with_policy(
        name,
        transport,
        declare_wired,
        exclude,
        crate::features::Policy::active(),
        &crate::features::ambient_skip(),
        &crate::config::VehicleIdentity::baseline(),
    )
}

/// [`build_ident_info`] with an app-pushed [`VehicleIdentity`] driving params 20/21 (docs/carplay/04_CAPABILITIES_AND_CONFIG.md C6).
///
/// This is the ONLY entry point that can change param-20 content. Everything else delegates with
/// [`VehicleIdentity::baseline`], which is today's hardcoded emission, so adding this function
/// changed no existing byte. That is enforced, not assumed, by the hardcoded param-20 fixture in
/// `baseline_param20_is_byte_pinned` plus the pre-existing byte-exact Identify pins.
///
/// ⚠️ The wireless (BT-time) arm IGNORES `identity` and always emits the baseline. That pin is
/// enforced INSIDE the builder, not by convention at the call sites, because a wireless params-20
/// change breaks the WiFi handoff and `0x1D03` is unrecoverable within a session (docs/wireless/00_WIRELESS_CARPLAY.md,
/// docs/carplay/05_METADATA_AND_CONTROLS.md §6.2). `identity_is_ignored_on_the_wireless_arm` is the mechanical proof.
pub fn build_ident_info_with(
    name: &str,
    transport: TransportComponent,
    declare_wired: bool,
    identity: &crate::config::VehicleIdentity,
) -> Vec<u8> {
    build_ident_info_excluding_with(name, transport, declare_wired, &[], identity)
}

/// [`build_ident_info_excluding`] with an app-pushed identity — the `0x1D03` retry form of
/// [`build_ident_info_with`].
pub fn build_ident_info_excluding_with(
    name: &str,
    transport: TransportComponent,
    declare_wired: bool,
    exclude: &[u16],
    identity: &crate::config::VehicleIdentity,
) -> Vec<u8> {
    build_ident_info_excluding_with_policy(
        name,
        transport,
        declare_wired,
        exclude,
        crate::features::Policy::active(),
        &crate::features::ambient_skip(),
        identity,
    )
}

/// Injection form of [`build_ident_info_excluding`]: builds with an EXPLICIT metadata `policy` + `skip`
/// rather than the process-cached ambient ones, so a test pins the proven baseline regardless of a dev
/// box's `/tmp/carplay_metadata` (audit Fix #25). Production callers use `build_ident_info_excluding`.
pub(crate) fn build_ident_info_excluding_with_policy(
    name: &str,
    transport: TransportComponent,
    declare_wired: bool,
    exclude: &[u16],
    policy: crate::features::Policy,
    skip: &[&str],
    identity: &crate::config::VehicleIdentity,
) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();

    // A 0x1D03 IdentificationRejected retry must NEVER strip a REQUIRED IdentificationInformation param
    // (#407): dropping e.g. Name / the MessagesSent-or-Received lists / PowerProvidingCapability makes
    // the retry structurally invalid, so iOS rejects it again → the handshake aborts. Filter the caller's
    // exclude list down to strippable (optional) params only; required ids are retained even if iOS
    // "rejected" them. An empty exclude filters to empty, so the byte-exact-vs-`build_ident_info`
    // invariant is preserved.
    let exclude_owned: Vec<u16> = exclude
        .iter()
        .copied()
        .filter(|id| !REQUIRED_IDENT_PARAMS.contains(id))
        .collect();
    let exclude: &[u16] = &exclude_owned;

    // Each top-level param is built into a scratch `Tlv` and then conditionally appended, so that an
    // empty `exclude` reproduces the exact historical byte stream while a non-empty one drops whole
    // rejected groups. Scalar/string helpers here emit identical bytes to the previous direct-append
    // form — the byte-exact tests pin this.
    let mut push = |exclude: &[u16], pid: u16, build: &dyn Fn(&mut Tlv)| {
        let mut t = Tlv::new();
        build(&mut t);
        push_group_if(&mut out, exclude, pid, &t.0);
    };

    push(exclude, spec::ident::NAME, &|t| {
        t.str(spec::ident::NAME, name)
    }); // param 0
    push(exclude, spec::ident::MODEL_IDENTIFIER, &|t| {
        t.str(spec::ident::MODEL_IDENTIFIER, "Magic-Car-Link-1.00")
    }); // param 1
    push(exclude, spec::ident::MANUFACTURER, &|t| {
        t.str(spec::ident::MANUFACTURER, "Magic Tec.")
    }); // param 2
    push(exclude, spec::ident::SERIAL_NUMBER, &|t| {
        t.str(spec::ident::SERIAL_NUMBER, "2025.02.25.1521626a")
    }); // param 3
    push(exclude, spec::ident::FIRMWARE_VERSION, &|t| {
        t.str(spec::ident::FIRMWARE_VERSION, "1.0.0")
    }); // param 4
    push(exclude, spec::ident::HARDWARE_VERSION, &|t| {
        t.str(spec::ident::HARDWARE_VERSION, "1.0.0")
    }); // param 5

    // Params 6/7: raw BE16 lists of message ids the accessory sends / expects to receive.
    //
    // ⚠️ SESSION-CRITICAL, INCIDENT 2026-07-06: a change of this exact shape (declaring a new,
    // never-wire-verified capability here) previously broke the ENTIRE session -- not just the new
    // feature -- requiring a full carplayd+receiver restart and physical replug to recover. The
    // 0x415A-0x4161 call-control cluster below is even less certain than that incident's cause (it
    // comes from disassembly/strings, never wire-captured, and the reference project's OWN capture
    // never got this Communications plane negotiated at all). Test this declaration addition ALONE
    // — deploy, restart, replug, confirm the baseline session still establishes normally — BEFORE
    // adding any driver.rs send path for it (see docs on the hardware-buttons feature).
    // Message-id capability lists (params 6/7). The bearer selects the whole list, not just an append:
    //
    // WIRELESS: was byte-exact to the carlink_linux C reference (`iap2_bt/iap2_link.c`, the only impl
    //   that reached a proven FULL wireless-CarPlay session) through docs/wireless/00_WIRELESS_CARPLAY.md Phase 5.0 — sent =
    //   {0x5703}, received = {0x4E0A, 0x4E0B, 0x5702, 0x4E0D, 0x4E0E} — deliberately declaring NEITHER
    //   the CarPlay session-start pair 0x4300/0x4301 (which traps iOS in the wired-style 0x4301
    //   handshake we never complete — device-observed: adding them SUPPRESSED the "Use CarPlay" offer)
    //   NOR the NowPlaying / route-guidance / call-control cluster (which diverts iOS into plain
    //   media-accessory behavior — it starts NowPlaying updates instead of the WiFi handoff).
    //
    //   docs/wireless/00_WIRELESS_CARPLAY.md Phase 5.1 — TRIED AND REVERTED (2026-07-24, live-device evidence, not speculation): a
    //   from-scratch bring-up test with 0x5000 StartNowPlayingUpdates (sendable) / 0x5001
    //   NowPlayingUpdate (receivable) ALSO declared here confirmed the incident-era comment above's
    //   warning is real for this exact id pair. Device log (`/tmp/wl.log`), two separate connection
    //   attempts, identical outcome each time:
    //     TX 0x1D01 IdentificationInformation (305 B, wireless transport)
    //     RX 0x1D00 -> IdentSent
    //     TX 0x1D01 retry, stripped [6] (305 B, wireless)   <- byte-identical: param 6 is required,
    //                                                           can't actually be stripped (see below)
    //     RX 0x1D03 -> IdentRetried                          <- rejected again
    //     exit state=IdentRetried
    //     [wireless] RFCOMM session ended
    //   iOS explicitly 0x1D03-rejects param 6 (MessagesSentByAccessory) once 0x5000 is in it. Since
    //   params 6/7 are in `REQUIRED_IDENT_PARAMS`, `build_ident_info_excluding` can't actually drop
    //   them, so the retry is byte-identical and predictably rejected again — confirmed
    //   `Action::Abort`, then the phone drops the RFCOMM link rather than the accessory needing to.
    //   This IS software-recoverable (the RFCOMM accept loop in `main.rs` re-listens immediately, no
    //   physical replug) but is a genuine, repeatable handoff break — a livelock on reconnect, not a
    //   one-off fluke. Reverted immediately per docs/wireless/00_WIRELESS_CARPLAY.md. Declaring the NowPlaying ids ALONE (the
    //   case docs/carplay/05_METADATA_AND_CONTROLS.md called "the central open experiment") is now a CONFIRMED closed dead end for the
    //   wireless Identify-declaration approach specifically — the remaining path for NowPlaying (if
    //   any) is the deeper `accessoryd`-internal link-layer question (docs/carplay/05_METADATA_AND_CONTROLS.md Part 2), needing dynamic
    //   (qemu) analysis, not further static Identify changes. RouteGuidance (5.2) and CallState (5.3)
    //   were never attempted before this revert — that dead end is scoped to the NowPlaying id pair
    //   only, not to "any params 6/7 addition" as a class (untested whether the mechanism is
    //   content-specific or general — 5.2 below is the test of that).
    //
    //   docs/wireless/00_WIRELESS_CARPLAY.md Phase 5.2 — TRIED AND REVERTED (2026-07-24, live-device evidence): declared 0x5200
    //   StartRouteGuidanceUpdates (sendable) / 0x5201 RouteGuidanceUpdate, 0x5202
    //   RouteGuidanceManeuverInformation (receivable), layered on 5.0 alone (5.1 stayed reverted). A
    //   from-scratch bring-up test hit the IDENTICAL reject-livelock shape as 5.1's postmortem: TX
    //   0x1D01 (307 B) -> RX 0x1D03 -> TX retry stripped [6] (307 B, byte-identical) -> RX 0x1D03 again
    //   -> exit state=IdentRetried -> RFCOMM session ended. This run also captured the raw reject
    //   payload (new diagnostic logging added for this test, `bt_driver.rs`'s
    //   "RX 0x1D03 IdentificationRejected raw payload" line): `[40, 40, 00, 0a, 1d, 03, 00, 04, 00, 06,
    //   4c]` — the rejected-param TLV group decodes to `[len=0x0004][pid=0x0006]`, a bare ZERO-LENGTH
    //   "none" marker for param 6 with no embedded array of specific unsupported message ids. iOS gave
    //   no content-level detail — it flatly rejects param 6 the same way whether the addition was
    //   0x5000 (5.1) or 0x5200 (5.2), two unrelated message ids. That is real evidence (not proof, but
    //   2-for-2) that the mechanism is a GENERAL reaction to params 6/7 growing beyond the
    //   Phase-5.0-proven baseline (sent={0x5703}, received={0x4E0A,0x4E0B,0x5702,0x4E0D,0x4E0E}), not
    //   specific to NowPlaying's content. Per this file's own 5.1-postmortem conclusion: **5.3
    //   (CallState ids) is not worth attempting** — it would almost certainly hit the identical
    //   reject-livelock, and CallState's ids were independently flagged (docs/wireless/00_WIRELESS_CARPLAY.md V10) as the likeliest
    //   to ALSO risk the more severe media-accessory/HFP-profile diversion, so there is no upside case
    //   for trying it. The remaining path for wireless NowPlaying/RouteGuidance/CallState metadata (if
    //   any) is the deeper `accessoryd`-internal link-layer question (docs/carplay/05_METADATA_AND_CONTROLS.md Part 2), needing dynamic
    //   (qemu) analysis — static Identify-declaration changes to params 6/7 are now a closed dead end
    //   as a class, confirmed on real hardware across two independent id pairs.
    //
    //   The lone wireless-specific message previously omitted, 0x4E0D WirelessCarPlayUpdate, is
    //   included (predates 5.0; unaffected by the 5.1/5.2 reverts).
    //
    // WIRED (USB): unchanged — the full base list, plus the 0x4301/0x4300 pair only when
    //   declare_wired (callers pass false; the USBHostTransportComponent param 16 brings CarPlay up
    //   declaratively).
    let (sent, rcv) = if matches!(transport, TransportComponent::Wireless { .. }) {
        let mut s = Vec::new();
        s.extend_from_slice(&spec::MSG_ACCESSORY_WI_FI_CONFIGURATION_INFORMATION.to_be_bytes()); // 0x5703
        let mut r = Vec::new();
        r.extend_from_slice(&spec::MSG_DEVICE_LANGUAGE_UPDATE.to_be_bytes()); // 0x4E0A
        r.extend_from_slice(&spec::MSG_DEVICE_TIME_UPDATE.to_be_bytes()); // 0x4E0B
        r.extend_from_slice(
            &spec::MSG_REQUEST_ACCESSORY_WI_FI_CONFIGURATION_INFORMATION.to_be_bytes(),
        ); // 0x5702
        r.extend_from_slice(&spec::MSG_WIRELESS_CAR_PLAY_UPDATE.to_be_bytes()); // 0x4E0D
        r.extend_from_slice(&spec::MSG_DEVICE_TRANSPORT_IDENTIFIER_NOTIFICATION.to_be_bytes()); // 0x4E0E
        (s, r)
    } else if matches!(transport, TransportComponent::AirPlayTunnel { .. }) {
        // docs/wireless/00_WIRELESS_CARPLAY.md: this session's ENTIRE purpose is metadata, so params 6/7 declare exactly the
        // Start*/Update ids `events.rs::send_wireless_metadata_subscriptions` sends/expects over this
        // SAME tunnel — no WiFi-handoff ids (0x5702/0x5703/0x4E0x) here, that already happened on the
        // BT-time Identify and isn't this session's concern.
        // SCOPE: this list is the WIRED-PROVEN set and nothing more (2026-07-25). Note it is not purely
        // a trim: 0x4157/0x4170/0x4158/0x4171 were REMOVED, but 0x5002/0x5203 (the Stop counterparts)
        // were ADDED — they are in the wired-proven `SENT_MSG_IDS`, but they are new to THIS arm, so the
        // first hardware identify carries two ids this transport has never sent.
        //
        // It previously also declared 0x4157/0x4158 (Communications) and 0x4170/0x4171 (List). Those
        // four appear in NO device-proven identify in this repo, and they would have been declared for
        // the first time on the first identify ever to run on this transport. The downside is not
        // partial: params 6 and 7 are in `REQUIRED_IDENT_PARAMS`, so `build_ident_info_excluding`
        // cannot strip them — a `0x1D03` reject makes the retry byte-identical, the second reject
        // returns `Action::Abort`, and the whole tunnel session is cleared with no auth, no identify and
        // no subscribes. docs/wireless/00_WIRELESS_CARPLAY.md recorded iOS rejecting exactly this shape twice on the BT
        // identify, and `message.rs`'s own conclusion is that the reject is a GENERAL reaction to
        // params 6/7 growing beyond the proven baseline.
        //
        // Two nuances for whoever re-adds them:
        //
        //  * iOS 27's accessoryd DOES register inbound handlers for 0x4157 and 0x4170 (they appear in
        //    its 96-id handler list). That is NOT evidence they may be declared: handler registration
        //    and identify validation are separate mechanisms, and this repo already proves it — 0x4301
        //    is likewise in that handler list, yet declaring it on the identify was device-confirmed to
        //    suppress the "Use CarPlay" offer entirely (see the param-6 note above).
        //  * docs/wireless/00_WIRELESS_CARPLAY.md's rejections were measured on the BT-time identify, whose proven baseline is a
        //    different set again. So the literal rule "any params-6/7 growth is rejected" cannot be what
        //    is going on — this trimmed set is itself a wholesale departure from that BT baseline. The
        //    real argument for trimming is narrower and stands on its own: a reject here is
        //    UNRECOVERABLE, this identify has never run on hardware, and every id kept is one the wired
        //    identify already gets accepted. Re-add as a deliberate A/B once the link is proven, one
        //    pair at a time.
        // Generated from `features::table` (docs/carplay/05_METADATA_AND_CONTROLS.md) so params 6/7 and the subscribes that follow
        // cannot drift apart — they did for the project's whole history, which is why 0x4157/0x4170
        // were sent for a year with 0x4158/0x4171 undeclared and were silently ignored.
        // `CARPLAY_METADATA=proven` reverts to the 2026-07-25 device-accepted set without a rebuild;
        // `CARPLAY_METADATA_SKIP=power,app_discovery` drops individual features.
        let p = policy;
        let np = crate::metadata::start_now_playing;
        let rg = crate::metadata::start_route_guidance;
        (
            be16_list(&crate::features::sent_ids(&crate::features::active_with_skip(p.sent, np, rg, skip))),
            be16_list(&crate::features::received_ids(&crate::features::active_with_skip(p.recv, np, rg, skip))),
        )
    } else {
        // The wired-proven floor, unioned with the feature table (docs/carplay/05_METADATA_AND_CONTROLS.md). The floor is kept
        // verbatim so `CARPLAY_METADATA=proven` reproduces the declaration this transport has been
        // running with, including the call-control cluster and the 0x4E0x device updates.
        let p = policy;
        let np = crate::metadata::start_now_playing;
        let rg = crate::metadata::start_route_guidance;
        let sf = crate::features::active_with_skip(p.sent, np, rg, skip);
        let rf = crate::features::active_with_skip(p.recv, np, rg, skip);
        let mut s = be16_list(&union(SENT_MSG_IDS, &crate::features::sent_ids(&sf)));
        let mut r = be16_list(&union(RCV_MSG_IDS, &crate::features::received_ids(&rf)));
        if declare_wired {
            s.extend_from_slice(&spec::MSG_CAR_PLAY_START_SESSION.to_be_bytes()); // 0x4301
            r.extend_from_slice(&spec::MSG_CAR_PLAY_AVAILABILITY.to_be_bytes());
            // 0x4300
        }
        (s, r)
    };
    push(exclude, spec::ident::MESSAGES_SENT_BY_ACCESSORY, &|t| {
        t.bytes(spec::ident::MESSAGES_SENT_BY_ACCESSORY, &sent)
    }); // param 6
    push(exclude, spec::ident::MESSAGES_RECEIVED_FROM_DEVICE, &|t| {
        t.bytes(spec::ident::MESSAGES_RECEIVED_FROM_DEVICE, &rcv)
    }); // param 7

    push(exclude, spec::ident::POWER_PROVIDING_CAPABILITY, &|t| {
        t.u8v(spec::ident::POWER_PROVIDING_CAPABILITY, 2)
    }); // param 8 = Advanced
    push(
        exclude,
        spec::ident::MAXIMUM_CURRENT_DRAWN_FROM_DEVICE,
        &|t| t.u16v(spec::ident::MAXIMUM_CURRENT_DRAWN_FROM_DEVICE, 0),
    ); // param 9
    push(exclude, spec::ident::CURRENT_LANGUAGE, &|t| {
        t.str(spec::ident::CURRENT_LANGUAGE, "en")
    }); // param 12
    push(exclude, spec::ident::SUPPORTED_LANGUAGE, &|t| {
        t.str(spec::ident::SUPPORTED_LANGUAGE, "en")
    }); // param 13

    // Param 16 — USB-host transport component. LOAD-BEARING: this component (name "IAP2-USBHost" +
    // the CarPlay interface number `cp_iface`) with the transport-supports-CarPlay presence flag is
    // what makes iOS switch to NCM/host and bring the CarPlay bearer up DECLARATIVELY — no
    // StartCarPlay/0x4301 message is needed (ref ncm_carplayd/ACTIVITY_LOG.md:640-646).
    //
    // Param 17 — bluetoothTransportComponent. Wire order matters here (17, then 20, then 24, matching
    // the reference's own captured fixture byte-for-byte, docs/carplay/04_CAPABILITIES_AND_CONFIG.md) even though TLV param IDs are
    // self-describing — untested whether iOS's parser is truly order-independent, so this doesn't
    // take the risk. Device-confirmed 2026-07-07: sending param 24 WITHOUT this companion param gets
    // an `0x1D03` reject instead of `0x1D02` IdentifyAccept.
    if let TransportComponent::Usb { cp_iface } = transport {
        use spec::ident::usb_host as sub;
        let mut g16 = Tlv::new();
        g16.u16v(sub::TRANSPORT_COMPONENT_IDENTIFIER, 1); // sub 0
        g16.str(sub::TRANSPORT_COMPONENT_NAME, "IAP2-USBHost"); // sub 1
        g16.none(sub::TRANSPORT_SUPPORTSI_AP2_CONNECTION); // sub 2 (TransportSupportsiAP2Connection)
                                                           // sub 3 = USBHostTransportCarPlayInterfaceNumber (uint8): the accessory's NCM Control
                                                           // Interface number. (Apple spec IdentificationInformation §16.3 — was mis-described as a
                                                           // bare "CarPlay interface number" with no field name.)
        g16.u8v(sub::USB_HOST_TRANSPORT_CAR_PLAY_INTERFACE_NUMBER, cp_iface); // sub 3
        g16.none(sub::TRANSPORT_SUPPORTS_CAR_PLAY); // sub 4 (required — the CarPlay-capable flag)
        push(exclude, spec::ident::USB_HOST_TRANSPORT_COMPONENT, &|t| {
            t.bytes(spec::ident::USB_HOST_TRANSPORT_COMPONENT, &g16.0)
        }); // param 16
    }
    // docs/wireless/00_WIRELESS_CARPLAY.md: `AirPlayTunnel` also declares params 17/24 — same transport-component shape as
    // `Wireless`, since it's still fundamentally a wireless-CarPlay-capable accessory, just a fresh
    // session on a different channel. Untested whether iOS actually requires param 17 (the BT
    // transport component) here given this session doesn't run over BT at all; kept for now to
    // minimize simultaneous new variables versus the already-isolated params-6/7 change.
    if let TransportComponent::Wireless { bt_mac } | TransportComponent::AirPlayTunnel { bt_mac } =
        transport
    {
        use spec::ident::bluetooth as sub;
        let mut g17 = Tlv::new();
        g17.u16v(sub::TRANSPORT_COMPONENT_IDENTIFIER, 1); // sub 0
        g17.str(sub::TRANSPORT_COMPONENT_NAME, "IAP2-Bluetooth"); // sub 1
        g17.none(sub::TRANSPORT_SUPPORTSI_AP2_CONNECTION); // sub 2
                                                           // sub 3 = BluetoothTransportMediaAccessControlAddress (uint8[6]): the accessory's BT MAC,
                                                           // a valid 6-byte IEEE EUI-48. Value is cosmetic per the reference ("any 6 bytes accepted")
                                                           // but its PRESENCE is load-bearing (device-confirmed 2026-07-07, docs/carplay/04_CAPABILITIES_AND_CONFIG.md).
        g17.bytes(
            sub::BLUETOOTH_TRANSPORT_MEDIA_ACCESS_CONTROL_ADDRESS,
            &bt_mac,
        ); // sub 3
        push(exclude, spec::ident::BLUETOOTH_TRANSPORT_COMPONENT, &|t| {
            t.bytes(spec::ident::BLUETOOTH_TRANSPORT_COMPONENT, &g17.0)
        }); // param 17
    }

    // Param 20 — VehicleInformationComponent (Apple's authoritative name; the prior comment "an
    // identification component named after the accessory" was WRONG and caused the docs/carplay/04_CAPABILITIES_AND_CONFIG.md
    // "componentIdentifier" confusion). Sub 0 = Identifier (uint16), 1 = Name (utf8),
    // 2 = EngineType (enum: 0=Gasoline,1=Diesel,2=Electric,3=CNG), 6 = DisplayName (utf8).
    //
    // CONTENT IS APP-PUSHED since C-1 (docs/carplay/04_CAPABILITIES_AND_CONFIG.md C6). What this block emits comes from `identity`, and
    // the DEFAULT `identity` is `VehicleIdentity::baseline()` — EngineType=Gasoline and nothing else,
    // i.e. byte-for-byte the historical emission when no identity is pushed. iap2d passes the
    // app-pushed identity (C-3, see `build_ident_info_with` call sites in `ccpa/iap2d/src/main.rs`);
    // the tunnel and BT drivers still pass the baseline. `baseline_param20_is_byte_pinned` is the
    // golden fixture guarding the baseline path.
    //
    // Full spec surface (see `spec::ident::vehicle_info`): OPTIONAL subs 8 MapsDisplayName and
    // 9 SiriName are still NOT emitted; 10 VehicleColorHexCode, 11 SupportedChargingConnectors
    // (enum, 0=CCS1..8=NACS(AC)) and 12..20 PowerForConnectorType* (uint32) are now emitted WHEN AND
    // ONLY WHEN the app pushes them. Cardinality is not uniform and the resolver depends on it: subs
    // 2 and 11 are `[0+]` (repeatable), but each power sub 12-20 is `[optional]` = zero-or-one, which
    // is why `config::vehicle_identity` dedupes connectors before we get here.
    {
        use spec::ident::vehicle_info as sub;
        let mut g20 = Tlv::new();
        // Locally-chosen unique identifier: the C reference uses 1 on the wireless path, our wired
        // path historically uses 2. Keep wired at 2 (byte-stable) and match the reference on wireless.
        let vehicle_id: u16 = if matches!(
            transport,
            TransportComponent::Wireless { .. } | TransportComponent::AirPlayTunnel { .. }
        ) {
            1
        } else {
            2
        };
        // THE WIRELESS BYTE-PIN, enforced here rather than at the call sites (docs/wireless/00_WIRELESS_CARPLAY.md):
        // the BT-time Identify must stay byte-identical whatever the app pushes, so the wireless arm
        // substitutes the baseline. Structural, not conventional — see `build_ident_info_with`.
        let baseline;
        let identity = if matches!(transport, TransportComponent::Wireless { .. }) {
            baseline = crate::config::VehicleIdentity::baseline();
            &baseline
        } else {
            identity
        };

        g20.u16v(sub::IDENTIFIER, vehicle_id); // sub 0
        g20.str(sub::NAME, name); // sub 1
        // sub 2 — one TLV per engine type, ascending as configured. Apple's spec marks this `[0+]`
        // (verified from the compiled spec archive, C-0), so a hybrid emits two. The baseline is
        // exactly one Gasoline, reproducing the historical bytes.
        for &e in &identity.engine_types {
            g20.u8v(sub::ENGINE_TYPE, e);
        }
        g20.str(sub::DISPLAY_NAME, name); // sub 6
        // sub 10 — optional colour, emitted only when configured.
        if let Some(hex) = identity.color_hex.as_deref() {
            g20.str(sub::VEHICLE_COLOR_HEX_CODE, hex);
        }
        // subs 11 + 12-20 — the EV surface. All connector enums first, then their power values, so
        // sub ids ascend across the pair of loops (11.. then 12..20) like every other component.
        // WITHIN the power loop, ascending order depends on `connectors` already being sorted by
        // enum, which `config::vehicle_identity` guarantees; config order must not decide wire order.
        for &(enum_v, _) in &identity.connectors {
            g20.u8v(sub::SUPPORTED_CHARGING_CONNECTORS, enum_v);
        }
        for &(enum_v, watts) in &identity.connectors {
            if let (Some(w), Some(power_sub)) =
                (watts, crate::config::power_sub_for_connector(enum_v))
            {
                g20.u32v(power_sub, w);
            }
        }
        push(exclude, spec::ident::VEHICLE_INFORMATION_COMPONENT, &|t| {
            t.bytes(spec::ident::VEHICLE_INFORMATION_COMPONENT, &g20.0)
        }); // param 20

        // Param 21 — VehicleStatusComponent, emitted ONLY when the app pushed a `vehicleStatus:`
        // block (and never on the byte-pinned wireless arm, which the substitution above already
        // guarantees by clearing `status_caps`). Identifier 3 keeps it unique against param 20's
        // 1/2. SESSION-CRITICAL: this grows the Identify, so it is config-gated and off by default.
        if let Some(caps) = identity.status_caps.as_ref() {
            let g21 = vehicle_status_inner(3, name, caps);
            push(exclude, spec::ident::VEHICLE_STATUS_COMPONENT, &|t| {
                t.bytes(spec::ident::VEHICLE_STATUS_COMPONENT, &g21)
            }); // param 21
        }
    }

    // Param 24 — WirelessCarPlayTransportComponent (Apple's authoritative name; byte-confirmed
    // against a real BT capture, docs/carplay/04_CAPABILITIES_AND_CONFIG.md). Sub 0 = TransportComponentIdentifier, 1 =
    // TransportComponentName, 2 = TransportSupportsiAP2Connection (none, required), 4 =
    // TransportSupportsCarPlay (none, required); optional sub 5 = TransportSupportsThemeAssets (not
    // declared). Must come after param 20 and alongside param 17 above, matching capture order.
    if matches!(
        transport,
        TransportComponent::Wireless { .. } | TransportComponent::AirPlayTunnel { .. }
    ) {
        use spec::ident::wireless_carplay as sub;
        let mut g24 = Tlv::new();
        g24.u16v(sub::TRANSPORT_COMPONENT_IDENTIFIER, 2); // sub 0
        g24.str(sub::TRANSPORT_COMPONENT_NAME, "IAP2-Wireless"); // sub 1
        g24.none(sub::TRANSPORT_SUPPORTSI_AP2_CONNECTION); // sub 2
        g24.none(sub::TRANSPORT_SUPPORTS_CAR_PLAY); // sub 4
        push(
            exclude,
            spec::ident::WIRELESS_CAR_PLAY_TRANSPORT_COMPONENT,
            &|t| t.bytes(spec::ident::WIRELESS_CAR_PLAY_TRANSPORT_COMPONENT, &g24.0),
        ); // param 24
    }

    // Param 30 — RouteGuidanceDisplayComponent (Apple's authoritative name; the prior "location/
    // route-guidance component" comment was imprecise — the location component is param 22). Sub 0 =
    // Identifier (uint16), 1 = Name (utf8). Apple also defines seven OPTIONAL uint16 capacity fields
    // (sub 2 MaxCurrentRoadNameLength, 3 MaxDestinationNameLength, 4 MaxAfterManeuverRoadNameLength,
    // 5 MaxManeuverDescriptionLength, 6 MaxGuidanceManeuverStorageCapacity, 7
    // MaxLaneGuidanceDescriptionLength, 8 MaxLaneGuidanceStorageCapacity). We declare only the two
    // required fields today.
    //
    // ⚠️ CORRECTED 2026-07-30. This block used to say "a value of 0 / omission for the capacities
    // means 'not displayed to the user' per the spec". That is right for the five LENGTH fields
    // (2,3,4,5,7), whose notes all read "Not including this parameter or setting a value of 0
    // indicates that <field> is not displayed to the user" — but it is WRONG for the two STORAGE
    // fields, where Apple's own notes say the opposite:
    //   sub 6 MaxGuidanceManeuverStorageCapacity — "Number of maneuvers the accessory can store at
    //         once. Set to 0 to receive all maneuvers for a route."  0 = UNLIMITED, i.e. the
    //         maximum-traffic setting. Omission semantics are unspecified by Apple.
    //   sub 8 MaxLaneGuidanceStorageCapacity — "Must be included to receive Lane Guidance
    //         instructions. Set to 0 to recieve all Lane Guidance instructions for a route."
    //         (Apple's typo.) OMISSION = NO LANE GUIDANCE AT ALL; 0 = all of it.
    //
    // ⚠️ CORRECTED 2026-08-16 — the "iOS can never send 0x5204" claim below was REFUTED by our own
    // capture. This comment previously read: "because we never send sub 8 **iOS can never send
    // 0x5204** — that feature is inert today regardless of tier or subscribe." The 2026-07-29
    // session delivered `0x5204 LaneGuidanceInformation` x12 alongside `0x5201` x574 (docs/ops/05_AUDITS.md
    // §"`0x5204` confirmed via `RidesOn`"): lane guidance has no `Start*` of its own, is declared in
    // param 7, and rides `route_guidance`'s subscribe. So it is NOT inert, and sub 8 is not the gate
    // it was read as. What sub 8 actually controls is not established — do not substitute a new
    // theory here without a capture. docs/carplay/03_SDK_GROUND_TRUTH.md §8.1 carries the same refuted claim; see `R-49-8`.
    // Still believed true, and NOT refuted by that capture: `metadata.rs` parses CurrentRoadName
    // (0x5201 sub 3) and DestinationName (sub 4), which iOS will never populate while subs 2 and 3
    // are absent, so even a perfect transport delivers road-name-less route guidance.
    //
    // BEFORE EXPANDING THIS BLOCK, READ THIS: it is currently UNGATED across all three transports,
    // so growing it also grows the BT-time `Wireless` Identify — the one CLAUDE.md and
    // `features.rs` flag as byte-pinned after docs/wireless/00_WIRELESS_CARPLAY.md broke the WiFi handoff twice. Gate to
    // `AirPlayTunnel` (and optionally `Usb`) FIRST; that is a prerequisite, not an option.
    // Recommended values once gated: subs 2/3/4 = 40, 5 = 64, 7 = 32, and sub 6 = 4 / sub 8 = 2 —
    // NOT 0. Zero on 6 or 8 makes iOS dump the entire route as individual 0x5202/0x5204 messages at
    // route start and on every reroute (hundreds of messages, each needing its own ChaCha20-Poly1305
    // open + TLV walk on a 528 MHz single core). CT5 CINEMO ships both at 0, but that is an Android
    // head unit with orders of magnitude more headroom — a vendor choice, not a value to copy.
    // (See `spec::ident::route_guidance`, which already has all nine ids and names correct.)
    // docs/wireless/00_WIRELESS_CARPLAY.md Phase 5.0 (2026-07-24, EXPERIMENTAL, unverified on hardware — the first, most isolated
    // step of the wireless-metadata Identify changes): declare param 30 on the WIRELESS arm too, now
    // unconditionally, matching the wired arm byte-for-byte (id=42, name="RouteGuidance").
    //
    // Previously this was wired-only: the carlink_linux wireless reference's full-session identify
    // does NOT include a RouteGuidanceDisplayComponent (its params are 0-9,12,13,17,20,24), so it was
    // omitted here to stay byte-faithful to the only implementation that ever reached a proven full
    // wireless session. Phase 4 (docs/wireless/00_WIRELESS_CARPLAY.md) confirmed wireless metadata does not flow at all with the
    // proven-byte-faithful Identify — no RouteGuidance/NowPlaying/CallState replies over the tunnel
    // despite correctly-sent subscriptions and confirmed real media+nav engagement (docs/wireless/00_WIRELESS_CARPLAY.md-adjacent
    // finding). This is a precondition for the RouteGuidance subscribe (`metadata.rs:145`, hardcoded
    // component id 42) to reference a declared component at all — deployed and tested ALONE, with NO
    // metadata message ids added yet (those are 5.1-5.3), specifically to confirm this single param
    // does not itself break the BT->WiFi handoff before any further Identify change is layered on.
    //
    // ROLLBACK (param 30 SPECIFICALLY — see the "Params 6/7" comment block above, near
    // "docs/wireless/00_WIRELESS_CARPLAY.md Phase 5.1/5.2", for the SEPARATE rollback if a reject instead implicates params 6/7,
    // which as of 5.2 is the far more likely target for a params-6/7 reject, not this block): if the
    // handoff never initiates (iOS silently starts BT media-accessory NowPlaying instead of sending
    // 0x5702 within seconds of IdentifyAccept — the docs/wireless/00_WIRELESS_CARPLAY.md §Phase 5 fast-fail signal) AND a 0x1D03
    // IdentificationRejected names param 30 itself (not 6/7 — those are REQUIRED and never stripped;
    // see `REQUIRED_IDENT_PARAMS`, and see the params-6/7 rollback for that case instead), revert THIS
    // block to the wired-only gate by restoring the verbatim prior condition below (no other file
    // needs to change — see the review that confirmed no downstream code depends on wireless param
    // 30's presence):
    //
    //   if !matches!(transport, TransportComponent::Wireless { .. }) { <the body below, unchanged> }
    {
        use spec::ident::route_guidance as sub;
        let mut g30 = Tlv::new();
        g30.u16v(sub::IDENTIFIER, 42); // sub 0
        g30.str(sub::NAME, "RouteGuidance"); // sub 1
        push(
            exclude,
            spec::ident::ROUTE_GUIDANCE_DISPLAY_COMPONENT,
            &|t| t.bytes(spec::ident::ROUTE_GUIDANCE_DISPLAY_COMPONENT, &g30.0),
        ); // param 30
    }

    // `push`'s `&mut out` borrow ends at its last call above (NLL), so `out` is free to move out.
    out
}

/// Walk a `0x1D03 IdentificationRejected` TLV body and return the rejected TOP-LEVEL parameter ids
/// (§1.4 of the Apple behavioral reference). In IdentificationRejected each present top-level param
/// mirrors an IdentificationInformation (0x1D01) param id and marks that component/field as rejected
/// (e.g. param 24 = WirelessCarPlayTransportComponent rejected). Most are zero-length `none`
/// presence markers; params 6/7 carry a uint16 array of unsupported message ids as their value — we
/// return only the top-level id in every case, since [`build_ident_info_excluding`] strips whole
/// top-level groups.
///
/// The walk mirrors Apple's `I2MRichParser` delivered-size discipline: every group header is bounds-
/// checked against the remaining bytes before it is consumed; a truncated/malformed trailing group
/// stops the walk (returning the ids recovered so far) rather than panicking. `body` must be the TLV
/// body only — strip the `[40 40][total][msg_id]` 6-byte link header first.
pub fn parse_rejected_param_ids(body: &[u8]) -> Vec<u16> {
    let mut ids = Vec::new();
    let mut i = 0usize;
    while i + 4 <= body.len() {
        let glen = u16::from_be_bytes([body[i], body[i + 1]]) as usize;
        let pid = u16::from_be_bytes([body[i + 2], body[i + 3]]);
        // A group must be at least its 4-byte header and must fit in the remaining bytes.
        if glen < 4 || i + glen > body.len() {
            break;
        }
        ids.push(pid);
        i += glen;
    }
    ids
}

/// Decode the *message ids* iOS named as unsupported inside a `0x1D03` reject, per top-level param.
///
/// Params 6 and 7 carry a `uint16` array of the specific message ids the device will not accept;
/// every other param is a zero-length presence marker. Until 2026-07-25 every reject this project
/// recorded carried only the bare param-6 marker with no array, which is what led docs/wireless/00_WIRELESS_CARPLAY.md to
/// conclude that params-6/7 growth was rejected as a class. It is not: with param 6 held at the
/// accepted baseline and param 7 grown, iOS returned `[param 6][], [param 7][0x4171]` — naming
/// exactly one offending id out of eleven added.
///
/// Returns `(param id, unsupported message ids)` pairs, empty vec where iOS gave no detail.
pub fn parse_rejected_message_ids(body: &[u8]) -> Vec<(u16, Vec<u16>)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 4 <= body.len() {
        let glen = u16::from_be_bytes([body[i], body[i + 1]]) as usize;
        let pid = u16::from_be_bytes([body[i + 2], body[i + 3]]);
        if glen < 4 || i + glen > body.len() {
            break;
        }
        let ids = body[i + 4..i + glen]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        out.push((pid, ids));
        i += glen;
    }
    out
}

/// Human-readable summary of a reject, for the one log line that matters when an Identify fails.
pub fn describe_reject(body: &[u8]) -> String {
    parse_rejected_message_ids(body)
        .into_iter()
        .map(|(pid, ids)| {
            if ids.is_empty() {
                format!("param {pid} (no detail)")
            } else {
                let named: Vec<String> = ids
                    .iter()
                    .map(|i| format!("0x{i:04X} {}", crate::spec::name_of(*i).unwrap_or("?")))
                    .collect();
                format!("param {pid} unsupported: {}", named.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// `AcceptCall` (0x415B) body -- EXPERIMENTAL, unwired (not called from driver.rs yet, see the
/// SESSION-CRITICAL note on `build_ident_info`'s param 6 list above). Empty body: `accessoryd`
/// strings confirm "AcceptCall message with no acceptAction parameter - falling back to
/// Accept/HoldAndAccept", so omitting the action parameter should trigger the sensible default
/// without needing to reverse-engineer its TLV encoding (never wire-captured).
pub fn build_accept_call() -> Vec<u8> {
    Vec::new()
}

/// `EndCall` (0x415C) body -- same rationale as `build_accept_call`: `accessoryd` strings confirm
/// "EndCall message with no endAction parameter - falling back to End/Decline".
pub fn build_end_call() -> Vec<u8> {
    Vec::new()
}

// ============================================================================================
// SESSION-CRITICAL, OPT-IN identify components (params 21 & 22) — NOT emitted by default.
// ============================================================================================
//
// ⚠️ `build_ident_info` — the DEFAULT emission — still does not reach either component: it passes
// `VehicleIdentity::baseline()`, whose `status_caps` is `None`, so param 21 is absent exactly as
// before. Adding a never-wire-verified capability group to the 0x1D01 body has previously broken the
// ENTIRE session (see the INCIDENT 2026-07-06 note on param 6).
//
// UPDATED 2026-08-10 (C-1): param 21 is now reachable, but ONLY through `build_ident_info_with` with
// an identity whose `status_caps` is `Some` — i.e. only when the app pushes a `vehicleStatus:` block,
// and only on a non-wireless arm. Param 22 remains unreachable (no caller at all).
//
// ⚠️ DO NOT ENABLE `vehicleStatus:` ON HARDWARE YET. `features.rs` declares NONE of 0xA100 /
// 0xA101 / 0xA102 in any tier, so emitting param 21 today advertises a VehicleStatusComponent whose
// messages params 6/7 never declare. That is the SAME SHAPE AS the
// `OptionalMsgNotValidWithoutRequiredMsgs` rule (docs/carplay/05_METADATA_AND_CONTROLS.md §5.6 rule 2) — note the analogy: rule 2 is
// device-proven for params 6/7 INTERNAL consistency (a receive declared without its send), and
// extending it to "a component declared in param 21 without its messages in 6/7" is well-reasoned
// inference, NOT a proven rule. It is precisely the "declaring a capability we never service" case
// this block warns about. The companion declaration is workstream C-4 and MUST land first; plan_C's
// claim that C-4 and C-5 are order-swappable is retracted (docs/carplay/04_CAPABILITIES_AND_CONFIG.md, C row).
//
// Treat emitting either as a SESSION-CRITICAL opt-in: deploy the declaration ALONE, restart, replug,
// confirm the baseline session still establishes, THEN wire a data path. Names/ids are
// Apple-authoritative (iap2messages.json, IdentificationInformation 0x1D01).

/// Sub-parameter ids of IdentificationInformation param 21 (`VehicleStatusComponent`), Apple's
/// authoritative names (iap2messages.json 0x1D01 §21). Not (yet) mirrored in `spec.rs` — see the
/// authoritative (verified 2026-08-10 against `tools/i2mspec_dump.py --message 0x1D01`, which is
/// where these ids must always come from). Sub 0 `Identifier` is uint16, sub 1 `Name` is utf8;
/// every capability sub (id ≥ 3) is a zero-length `none` PRESENCE flag meaning "the accessory can
/// report this datum in a VehicleStatusUpdate (0xA101)".
pub mod vehicle_status {
    /// Identifier — `uint16`. All Identifiers must be unique across identify components.
    pub const IDENTIFIER: u16 = 0;
    /// Name — `utf8`.
    pub const NAME: u16 = 1;
    /// Range — `none`.
    pub const RANGE: u16 = 3;
    /// OutsideTemperature — `none`.
    pub const OUTSIDE_TEMPERATURE: u16 = 4;
    /// InsideTemperature — `none`.
    pub const INSIDE_TEMPERATURE: u16 = 5;
    /// RangeWarning — `none` (unified range warning for single- or multi-EngineType vehicles).
    pub const RANGE_WARNING: u16 = 6;
    /// RangeGasoline — `none`.
    pub const RANGE_GASOLINE: u16 = 9;
    /// RangeDiesel — `none`.
    pub const RANGE_DIESEL: u16 = 10;
    /// RangeElectric — `none`.
    pub const RANGE_ELECTRIC: u16 = 11;
    /// RangeCNG — `none`.
    pub const RANGE_CNG: u16 = 12;
    /// RangeWarningGasoline — `none`.
    pub const RANGE_WARNING_GASOLINE: u16 = 13;
    /// RangeWarningDiesel — `none`.
    pub const RANGE_WARNING_DIESEL: u16 = 14;
    /// RangeWarningElectric — `none`.
    pub const RANGE_WARNING_ELECTRIC: u16 = 15;
    /// RangeWarningCNG — `none`.
    pub const RANGE_WARNING_CNG: u16 = 16;
    /// WiperStatus — `none`.
    pub const WIPER_STATUS: u16 = 17;
    /// BarometricPressure — `none`.
    pub const BAROMETRIC_PRESSURE: u16 = 18;
    /// Alerts — `none`.
    pub const ALERTS: u16 = 19;
    /// PassengerSeatStatus — `none`.
    pub const PASSENGER_SEAT_STATUS: u16 = 20;
    /// ElectricChargeInfo — `none`.
    pub const ELECTRIC_CHARGE_INFO: u16 = 21;
    /// MaxRangeInfo — `none`.
    pub const MAX_RANGE_INFO: u16 = 30;
}

/// Sub-parameter ids of IdentificationInformation param 22 (`LocationInformationComponent`),
/// Apple's authoritative names (iap2messages.json 0x1D01 §22). Sub 0 `Identifier` is uint16, sub 1
/// `Name` is utf8; every capability sub (id ≥ 17) is a zero-length `none` PRESENCE flag declaring
/// the accessory can generate the corresponding NMEA sentence / sensor stream in a
/// LocationInformation (0xFFFB) message.
pub mod location_info {
    /// Identifier — `uint16`. Identifiers shall be unique.
    pub const IDENTIFIER: u16 = 0;
    /// Name — `utf8`.
    pub const NAME: u16 = 1;
    /// GlobalPositioningSystemFixData — `none` (accessory can generate NMEA GPGGA sentences).
    pub const GLOBAL_POSITIONING_SYSTEM_FIX_DATA: u16 = 17;
    /// RecommendedMinimumSpecificGPSTransitData — `none` (NMEA GPRMC sentences).
    pub const RECOMMENDED_MINIMUM_SPECIFIC_GPS_TRANSIT_DATA: u16 = 18;
    /// GPSSatellitesInView — `none`.
    pub const GPS_SATELLITES_IN_VIEW: u16 = 19;
    /// VehicleSpeedData — `none` (NMEA PASCD sentences).
    pub const VEHICLE_SPEED_DATA: u16 = 20;
    /// VehicleGyroData — `none`.
    pub const VEHICLE_GYRO_DATA: u16 = 21;
    /// VehicleAccelerometerData — `none`.
    pub const VEHICLE_ACCELEROMETER_DATA: u16 = 22;
    /// VehicleHeadingData — `none`.
    pub const VEHICLE_HEADING_DATA: u16 = 23;
}

/// Build IdentificationInformation param 21 (`VehicleStatusComponent`) as a complete TLV group
/// (`[4+vlen BE16][pid=21 BE16][sub-TLVs]`), analogous to param 20's inline construction in
/// `build_ident_info`. `identifier` is the component's unique uint16 id, `name` its utf8 name, and
/// `capabilities` the ordered list of `vehicle_status::*` presence-flag sub-ids to advertise.
///
/// ⚠️ SESSION-CRITICAL opt-in — see the module banner above. This is NOT emitted by
/// `build_ident_info`; appending its output to the 0x1D01 body requires on-hardware verification.
pub fn build_vehicle_status_component(
    identifier: u16,
    name: &str,
    capabilities: &[u16],
) -> Vec<u8> {
    group_one(
        spec::ident::VEHICLE_STATUS_COMPONENT,
        &vehicle_status_inner(identifier, name, capabilities),
    ) // param 21
}

/// The sub-TLV payload of param 21, WITHOUT the enclosing group header.
///
/// Split out so the in-Identify emission path and the standalone
/// [`build_vehicle_status_component`] share one definition of the component's contents — the two
/// must not be allowed to drift, since only the standalone form is directly unit-tested.
fn vehicle_status_inner(identifier: u16, name: &str, capabilities: &[u16]) -> Vec<u8> {
    let mut g = Tlv::new();
    g.u16v(vehicle_status::IDENTIFIER, identifier); // sub 0
    g.str(vehicle_status::NAME, name); // sub 1
    for &cap in capabilities {
        g.none(cap); // each capability is a zero-length presence flag
    }
    g.0
}

/// Build IdentificationInformation param 22 (`LocationInformationComponent`) as a complete TLV group
/// (`[4+vlen BE16][pid=22 BE16][sub-TLVs]`). `capabilities` is the ordered list of `location_info::*`
/// presence-flag sub-ids to advertise.
///
/// ⚠️ SESSION-CRITICAL opt-in — see the module banner above. NOT emitted by `build_ident_info`.
pub fn build_location_information_component(
    identifier: u16,
    name: &str,
    capabilities: &[u16],
) -> Vec<u8> {
    let mut g = Tlv::new();
    g.u16v(location_info::IDENTIFIER, identifier); // sub 0
    g.str(location_info::NAME, name); // sub 1
    for &cap in capabilities {
        g.none(cap); // each capability is a zero-length presence flag
    }
    group_one(spec::ident::LOCATION_INFORMATION_COMPONENT, &g.0) // param 22
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an Identify body with the compiled-default proven policy + empty skip PINNED explicitly,
    /// so a byte-pinning test is hermetic against `/tmp/carplay_metadata` / `CARPLAY_METADATA*` (audit
    /// Fix #25). `declare_wired` matches the real call sites (false). "Default == proven" is verified
    /// separately + hermetically by `features::default_policy_is_proven`.
    fn proven_ident_body(name: &str, transport: TransportComponent) -> Vec<u8> {
        build_ident_info_excluding_with_policy(
            name,
            transport,
            false,
            &[],
            crate::features::Policy {
                sent: crate::features::Tier::Proven,
                recv: crate::features::Tier::Proven,
                subscribe: crate::features::Tier::Proven,
            },
            &[],
            // Baseline identity: this helper pins the PRE-C6 bytes, so it must never carry pushed
            // vehicle config. The identity-aware assertions build their own bodies explicitly.
            &crate::config::VehicleIdentity::baseline(),
        )
    }

    #[test]
    fn device_suffix_extraction() {
        // Wi-Fi MAC -> last 2 octets, lowercase, colons stripped (matches the SSID `ccpa-b0df`).
        assert_eq!(suffix_from("00:e0:4c:91:b0:df").as_deref(), Some("b0df"));
        assert_eq!(suffix_from("00:E0:4C:91:B0:DF\n").as_deref(), Some("b0df")); // upper + trailing NL
                                                                                 // SoC serial -> last 4 hex.
        assert_eq!(suffix_from("0a1b2c3dAB12CD34").as_deref(), Some("cd34"));
        // Too few hex chars -> None (caller falls through to the next source / bare base name).
        assert_eq!(suffix_from("xyz"), None);
        assert_eq!(suffix_from(""), None);
        // accessory_name with a real suffix would be "CarLink-b0df"; on a dev host with no /sys sources
        // device_suffix() returns None so it's the bare brand — assert that fallback shape.
        assert!(accessory_name("CarLink").starts_with("CarLink"));
    }

    #[test]
    fn tlv_encodings_match_c_layout() {
        let mut t = Tlv::new();
        t.str(0, "AB"); // group_len = 4 + 3 (AB\0) = 7
        assert_eq!(t.0, vec![0x00, 0x07, 0x00, 0x00, b'A', b'B', 0x00]);

        let mut u = Tlv::new();
        u.u8v(8, 2);
        assert_eq!(u.0, vec![0x00, 0x05, 0x00, 0x08, 0x02]);

        let mut w = Tlv::new();
        w.u16v(9, 0);
        assert_eq!(w.0, vec![0x00, 0x06, 0x00, 0x09, 0x00, 0x00]);

        let mut n = Tlv::new();
        n.none(2);
        assert_eq!(n.0, vec![0x00, 0x04, 0x00, 0x02]);
    }

    #[test]
    fn group_one_wraps_value() {
        // group_one(0x0000, cert) — the MFi cert/sig wrapper used for 0xAA01/0xAA03.
        assert_eq!(
            group_one(0x0000, &[0xAA, 0xBB]),
            vec![0x00, 0x06, 0x00, 0x00, 0xAA, 0xBB]
        );
    }

    #[test]
    fn ident_info_starts_with_name_group_and_declares_wired() {
        let body = build_ident_info("CarLink", TransportComponent::Usb { cp_iface: 1 }, true);
        // First group: str(0,"CarLink") -> len = 4 + 8 = 12 (0x0c), pid 0, "CarLink\0"
        assert_eq!(&body[0..4], &[0x00, 0x0C, 0x00, 0x00]);
        assert_eq!(&body[4..12], b"CarLink\0");
        // Wired ids must appear when declared.
        assert!(
            body.windows(2).any(|w| w == [0x43, 0x01]),
            "0x4301 sent id present"
        );
        assert!(
            body.windows(2).any(|w| w == [0x43, 0x00]),
            "0x4300 recv id present"
        );
        // And absent when not declared.
        let plain = build_ident_info("CarLink", TransportComponent::Usb { cp_iface: 1 }, false);
        assert!(!plain.windows(2).any(|w| w == [0x43, 0x01]));
    }

    #[test]
    fn declared_message_lists_are_direction_correct() {
        // Every id declared in param 6 (MessagesSentByAccessory) must originate AT the accessory,
        // and every id in param 7 (MessagesReceivedFromDevice) must originate at the device — per
        // Apple's authoritative `source` field (spec::source_of).
        for &id in SENT_MSG_IDS {
            assert_eq!(
                spec::source_of(id),
                Some(spec::Source::Accessory),
                "param-6 sent id {id:#06x} must be Source::Accessory"
            );
        }
        for &id in RCV_MSG_IDS {
            assert_eq!(
                spec::source_of(id),
                Some(spec::Source::Device),
                "param-7 rcv id {id:#06x} must be Source::Device"
            );
        }
        // The optional wired pair keeps the same direction discipline.
        assert_eq!(
            spec::source_of(spec::MSG_CAR_PLAY_START_SESSION),
            Some(spec::Source::Accessory),
            "CarPlayStartSession (0x4301) is accessory-sourced"
        );
        assert_eq!(
            spec::source_of(spec::MSG_CAR_PLAY_AVAILABILITY),
            Some(spec::Source::Device),
            "CarPlayAvailability (0x4300) is device-sourced"
        );
    }

    #[test]
    fn ident_info_usb_transport_matches_captured_param16_bytes() {
        let body = build_ident_info("CarLink", TransportComponent::Usb { cp_iface: 7 }, false);
        // param 16 group: 00 15 00 10  <u16v(0,1)=00 06 00 00 00 01>  <str(1,"IAP2-USBHost")=00 11 00 01 ..\0>
        //                 <none(2)=00 04 00 02>  <u8v(3,7)=00 05 00 03 07>  <none(4)=00 04 00 04>
        let mut expected = Vec::new();
        expected.extend_from_slice(&[0x00, 0x06, 0x00, 0x00, 0x00, 0x01]); // u16v(0,1)
        expected.extend_from_slice(&[0x00, 0x11, 0x00, 0x01]);
        expected.extend_from_slice(b"IAP2-USBHost\0");
        expected.extend_from_slice(&[0x00, 0x04, 0x00, 0x02]); // none(2)
        expected.extend_from_slice(&[0x00, 0x05, 0x00, 0x03, 0x07]); // u8v(3,7)
        expected.extend_from_slice(&[0x00, 0x04, 0x00, 0x04]); // none(4)

        let group_hdr = [0x00, (4 + expected.len()) as u8, 0x00, 0x10];
        let needle = [&group_hdr[..], &expected[..]].concat();
        assert!(
            body.windows(needle.len()).any(|w| w == needle.as_slice()),
            "param 16 group not found byte-exact in ident_info body"
        );
    }

    #[test]
    fn ident_info_wireless_transport_matches_captured_param24_bytes() {
        // Byte-exact against carlink_linux's real BT capture fixture (docs/carplay/04_CAPABILITIES_AND_CONFIG.md): componentIdentifier=2,
        // name "IAP2-Wireless", then the two "none" presence flags at sub-ids 2 and 4.
        let mac = [0x38, 0xba, 0xb0, 0x7d, 0xd3, 0x8d];
        let body = build_ident_info(
            "CarLink",
            TransportComponent::Wireless { bt_mac: mac },
            false,
        );
        let mut value = Vec::new();
        value.extend_from_slice(&[0x00, 0x06, 0x00, 0x00, 0x00, 0x02]); // u16v(0,2)
        value.extend_from_slice(&[0x00, 0x12, 0x00, 0x01]); // "IAP2-Wireless" is 13 chars, len=4+14=18=0x12
        value.extend_from_slice(b"IAP2-Wireless\0");
        value.extend_from_slice(&[0x00, 0x04, 0x00, 0x02]); // none(2)
        value.extend_from_slice(&[0x00, 0x04, 0x00, 0x04]); // none(4)
        assert_eq!(
            value.len(),
            32,
            "sub-TLVs must total 32 bytes per the captured fixture"
        );

        let group_hdr = [0x00, (4 + value.len()) as u8, 0x00, 0x18]; // param 24 = 0x0018
        assert_eq!(
            group_hdr,
            [0x00, 0x24, 0x00, 0x18],
            "outer group header must match the capture exactly"
        );
        let needle = [&group_hdr[..], &value[..]].concat();
        assert!(
            body.windows(needle.len()).any(|w| w == needle.as_slice()),
            "param 24 wireless transport group not found byte-exact in ident_info body"
        );
    }

    #[test]
    fn ident_info_wireless_message_lists_are_byte_pinned() {
        // The wireless bearer's MessagesSentByAccessory (param 6) and MessagesReceivedFromDevice
        // (param 7) are LOAD-BEARING and byte-exact to the carlink_linux C reference (#249):
        // sent = {0x5703}; received = {0x4E0A, 0x4E0B, 0x5702, 0x4E0D, 0x4E0E}. Any drift re-diverts
        // iOS out of the CarPlay offer (the hard-won A3 unlock), so pin them here — the wired lists
        // (SENT_MSG_IDS / RCV_MSG_IDS) are pinned by their own tests.
        //
        // docs/wireless/00_WIRELESS_CARPLAY.md Phase 5.1 and 5.2 (2026-07-24): both briefly grew these lists (0x5000/0x5001, then
        // 0x5200/0x5201/0x5202) and this test was updated each time to match — both REVERTED after
        // live-device testing confirmed iOS 0x1D03-rejects param 6 for either addition, identically
        // (see the reject-mechanics comment above the wireless sent/rcv construction, including the
        // captured raw reject payload). This test is back to pinning the pre-5.1/5.2 / Phase-5.0-only
        // reference lists — considered the stable baseline going forward; params-6/7 growth is now a
        // closed dead end as a class, not just for these two specific id pairs.
        let mac = [0x38, 0xba, 0xb0, 0x7d, 0xd3, 0x8d];
        let body = build_ident_info(
            "CarLink",
            TransportComponent::Wireless { bt_mac: mac },
            false,
        );
        // param 6 group: [len=0x0006][pid=0x0006][0x57 0x03]
        let sent = [0x00, 0x06, 0x00, 0x06, 0x57, 0x03];
        // param 7 group: [len=0x000E][pid=0x0007][0x4E0A 0x4E0B 0x5702 0x4E0D 0x4E0E]
        let recv = [
            0x00, 0x0E, 0x00, 0x07, 0x4E, 0x0A, 0x4E, 0x0B, 0x57, 0x02, 0x4E, 0x0D, 0x4E, 0x0E,
        ];
        assert!(
            body.windows(sent.len()).any(|w| w == sent),
            "wireless MessagesSentByAccessory (param 6 = {{0x5703}}) not byte-exact in ident body"
        );
        assert!(
            body.windows(recv.len()).any(|w| w == recv),
            "wireless MessagesReceivedFromDevice (param 7 = {{4E0A,4E0B,5702,4E0D,4E0E}}) not byte-exact"
        );
    }

    #[test]
    fn ident_info_wired_message_lists_are_byte_pinned() {
        // QC 2026-07-25: the WIRED params 6/7 had NO byte-pin test. The wireless and AirPlayTunnel
        // lists are pinned with length-prefixed needles (above/below), but wired was guarded only by
        // `declared_message_lists_are_direction_correct` — a direction-only check that passes for ANY
        // correctly-directioned edit — plus a 2-byte presence check for 0x4301/0x4300. Adding,
        // removing, or reordering an id in SENT_MSG_IDS/RCV_MSG_IDS would therefore have passed the
        // entire suite, despite the wired Identify being a hardware-proven surface and the comment on
        // the wireless test above already claiming "the wired lists are pinned by their own tests".
        // This closes that gap. `declare_wired = false` matches both real call sites in `iap2d`.
        let body = proven_ident_body("CarLink", TransportComponent::Usb { cp_iface: 1 });

        // The declaration is GENERATED: the wired-proven floor (`SENT_MSG_IDS`/`RCV_MSG_IDS`)
        // unioned with `features::table` at whatever tier the policy selects. Pinned here to the proven
        // policy (the compiled default), so the build reproduces the floor exactly — the property that
        // keeps a rebuild from silently changing a hardware-proven surface.
        let group = |pid: u8, ids: &[u16]| {
            let mut v = ((4 + ids.len() * 2) as u16).to_be_bytes().to_vec();
            v.extend_from_slice(&[0x00, pid]);
            v.extend(ids.iter().flat_map(|i| i.to_be_bytes()));
            v
        };
        let sent = group(6, SENT_MSG_IDS);
        let recv = group(7, RCV_MSG_IDS);
        assert!(
            body.windows(sent.len()).any(|w| w == sent),
            "proven-policy wired param 6 is not the proven floor"
        );
        assert!(
            body.windows(recv.len()).any(|w| w == recv),
            "proven-policy wired param 7 is not the proven floor"
        );

        // And the Extended tier, which is what `CARPLAY_METADATA=extended` declares. Pinned so a
        // table change is a deliberate one. Device-ACCEPTED on the RCS tunnel 2026-07-25 once every
        // Start carried its Stop (docs/carplay/05_METADATA_AND_CONTROLS.md §6.6): ~342 B Identify, `RX 0x1D02 -> Identified`, 8
        // subscribes. An earlier form WITHOUT the Stop ids was rejected (§6.1/§6.3) — that is what
        // the Stop fields exist for, not a property of this shape. The wired A/B has not been run.
        let f = crate::features::active_with_skip(
            crate::features::Tier::Extended,
            crate::metadata::start_now_playing,
            crate::metadata::start_route_guidance,
            &[], // hermetic (audit Fix #24): don't read the real /tmp CARPLAY_METADATA_SKIP
        );
        assert_eq!(
            union(SENT_MSG_IDS, &crate::features::sent_ids(&f)),
            vec![
                0x4154, 0x5000, 0x5002, 0x5200, 0x5203, // metadata subscribes (proven)
                0x415A, 0x415B, 0x415C, 0x415D, 0x415E, 0x415F, 0x4160, 0x4161, // call control
                // Extended subscribes. Each Start is declared WITH its Stop — iOS returns
                // `RequiredInfoMissing` on the Stop id otherwise (device-proven 2026-07-25, features
                // Power / Destination Sharing / App Links). 0x10DD DestinationInformationStatus is a
                // third required companion for Destination Sharing.
                // 0xAD03 RequestAppDiscoveryAppIcons added 2026-07-30: 0xAD04 was declared in
                // param 7 without its sender in param 6 — the
                // `OptionalMsgNotValidWithoutRequiredMsgs` shape (docs/carplay/05_METADATA_AND_CONTROLS.md §5.6 rule 2). Apple's own
                // Simulator declares 0xAD00/0xAD02/0xAD03 together.
                0x4157, 0x4159, 0x4170, 0x4172, 0xAE00, 0xAE02, 0xAD00, 0xAD02, 0xAD03, 0x10DA,
                0x10DC, 0x10DD,
            ]
        );
        assert_eq!(
            union(RCV_MSG_IDS, &crate::features::received_ids(&f)),
            vec![
                0x4E0A, 0x4E0B, 0x4E0E, 0x4155, 0x5001, 0x5201, 0x5202, // proven floor
                0x4158, 0x4171, 0xAE01, 0xAD01, 0xAD04, 0x5204, 0x10DB, 0x4E09, 0x4E0C, 0x4E0D,
            ]
        );

        // Every id we subscribe to must be declared sendable, and every update we expect declared
        // receivable — at the tier that actually drives each list. Drift between the two silenced
        // 0x4157/0x4170 for the project's history.
        //
        // NOTE the `rx-only` policy deliberately breaks the first half: param 6 stays at the proven
        // baseline while the extended subscribes still go out. That is an iAP2 contract violation,
        // and it is a REFUTED DEAD END (docs/carplay/05_METADATA_AND_CONTROLS.md §6.2) — it was proposed on the theory that iOS only
        // ever polices param 6, and the very next session had iOS name param 7 explicitly
        // (`[param 6][], [param 7][0x4171]`). Retained only so the experiment is not repeated.
        for feat in &f {
            for u in feat.updates {
                assert!(
                    union(RCV_MSG_IDS, &crate::features::received_ids(&f)).contains(u),
                    "{} expects 0x{u:04X} undeclared",
                    feat.name
                );
            }
        }
    }

    /// `CARPLAY_METADATA=proven` must reproduce the declaration this transport was running on
    /// 2026-07-25, when wireless metadata first worked. If this drifts, the recovery lever is dead.
    /// SESSION-CRITICAL (#407): params 6 and 7 must be un-strippable. On a `0x1D03` the driver
    /// re-sends the Identify with the rejected params removed — but an Identify without params 6/7 is
    /// structurally invalid, so the retry is rejected again and the handshake aborts. Deleting those
    /// two entries from `REQUIRED_IDENT_PARAMS` passed all 88 tests; the existing exclusion test only
    /// ever exercised `NAME`.
    #[test]
    fn required_ident_params_are_pinned() {
        assert_eq!(
            REQUIRED_IDENT_PARAMS,
            &[0u16, 1, 2, 3, 4, 5, 6, 7, 8, 9, 12, 13],
            "REQUIRED_IDENT_PARAMS changed — params 6/7 must stay un-strippable (#407)"
        );
        // And every one of them must actually resist exclusion, not just NAME.
        for &pid in REQUIRED_IDENT_PARAMS {
            let body = build_ident_info_excluding(
                "CarLink",
                TransportComponent::Usb { cp_iface: 1 },
                false,
                &[pid],
            );
            assert!(
                top_level_param_ids(&body).contains(&pid),
                "required param {pid} was strippable"
            );
        }
    }

    #[test]
    fn proven_tier_reproduces_the_wired_and_tunnel_baselines() {
        let f = crate::features::active_with_skip(
            crate::features::Tier::Proven,
            crate::metadata::start_now_playing,
            crate::metadata::start_route_guidance,
            &[], // hermetic (audit Fix #24): don't read the real /tmp CARPLAY_METADATA_SKIP
        );
        // Literal lists, NOT `SENT_MSG_IDS.to_vec()`. The old form compared the constant to itself
        // and could not fail: appending 0x4157 to `SENT_MSG_IDS` — growth on a hardware-proven
        // surface — passed the whole suite, because `union` dedupes and 0x4157 is the head of the
        // Extended extras list, so the union output was byte-identical.
        assert_eq!(
            union(SENT_MSG_IDS, &crate::features::sent_ids(&f)),
            vec![
                0x4154, 0x5000, 0x5002, 0x5200, 0x5203, //
                0x415A, 0x415B, 0x415C, 0x415D, 0x415E, 0x415F, 0x4160, 0x4161,
            ],
            "proven tier grew the wired param 6"
        );
        assert_eq!(
            union(RCV_MSG_IDS, &crate::features::received_ids(&f)),
            vec![0x4E0A, 0x4E0B, 0x4E0E, 0x4155, 0x5001, 0x5201, 0x5202],
            "proven tier grew the wired param 7"
        );

        // Anchored to the wire, not to a literal:
        // docs/ops/captures/2026-07-24_iap2d_wired_metadata_session.log line 13 —
        // "[iap2] TX 0x1D01 IdentificationInformation (290 B)" — the body iOS ACCEPTED. Every id
        // added to or removed from params 6/7 shifts this by 2. If you are reading this because it
        // failed, the declaration changed on a hardware-proven surface: re-capture before editing 290.
        assert_eq!(
            proven_ident_body("CarLink-b0df", TransportComponent::Usb { cp_iface: 1 }).len(),
            290,
            "wired Identify body no longer matches the captured, accepted 290-byte declaration"
        );
        assert_eq!(crate::features::sent_ids(&f), vec![0x5000, 0x5002, 0x5200, 0x5203, 0x4154]);
        assert_eq!(crate::features::received_ids(&f), vec![0x5001, 0x5201, 0x5202, 0x4155]);
    }

    #[test]
    fn ident_info_airplay_tunnel_declares_the_generated_metadata_ids() {
        // The AirPlayTunnel arm's whole purpose is metadata, so params 6/7 are generated wholly from
        // `features::table` — no WiFi-handoff ids, no call-control cluster, nothing the wired floor
        // carries for its own reasons. Pinning the id sequence forces any table change to be a
        // deliberate one: a `0x1D03` reject on this transport is UNRECOVERABLE (params 6/7 are in
        // `REQUIRED_IDENT_PARAMS`, so the retry is byte-identical, the second reject aborts, and the
        // tunnel session is cleared with no auth, no identify and no subscribes).
        let mac = [0x38, 0xba, 0xb0, 0x7d, 0xd3, 0x8d];
        let body = proven_ident_body("CarLink", TransportComponent::AirPlayTunnel { bt_mac: mac });
        // Built with the proven policy pinned (the compiled default), hermetic vs /tmp: the body must
        // carry exactly the 2026-07-25 device-accepted lists.
        let group = |pid: u8, ids: &[u16]| {
            let mut v = ((4 + ids.len() * 2) as u16).to_be_bytes().to_vec();
            v.extend_from_slice(&[0x00, pid]);
            v.extend(ids.iter().flat_map(|i| i.to_be_bytes()));
            v
        };
        assert!(
            body.windows(14).any(|w| w == group(6, &[0x5000, 0x5002, 0x5200, 0x5203, 0x4154])),
            "proven-policy tunnel param 6 is not the device-accepted baseline"
        );
        assert!(
            body.windows(12).any(|w| w == group(7, &[0x5001, 0x5201, 0x5202, 0x4155])),
            "proven-policy tunnel param 7 is not the device-accepted baseline"
        );

        let f = crate::features::active_with_skip(
            crate::features::Tier::Extended,
            crate::metadata::start_now_playing,
            crate::metadata::start_route_guidance,
            &[], // hermetic (audit Fix #24): don't read the real /tmp CARPLAY_METADATA_SKIP
        );
        let sent_ids = crate::features::sent_ids(&f);

        let recv_ids = crate::features::received_ids(&f);
        assert_eq!(
            sent_ids,
            vec![
                0x5000, 0x5002, 0x5200, 0x5203, 0x4154, // proven
                // Start/Stop pairs — iOS rejects a Start whose Stop is undeclared with
                // `RequiredInfoMissing` (device-proven 2026-07-25).
                // 0xAD03 RequestAppDiscoveryAppIcons added 2026-07-30: 0xAD04 was declared in
                // param 7 without its sender in param 6 — the
                // `OptionalMsgNotValidWithoutRequiredMsgs` shape (docs/carplay/05_METADATA_AND_CONTROLS.md §5.6 rule 2). Apple's own
                // Simulator declares 0xAD00/0xAD02/0xAD03 together.
                0x4157, 0x4159, 0x4170, 0x4172, 0xAE00, 0xAE02, 0xAD00, 0xAD02, 0xAD03, 0x10DA,
                0x10DC, 0x10DD,
            ]
        );
        assert_eq!(
            recv_ids,
            vec![
                0x5001, 0x5201, 0x5202, 0x4155, 0x4158, 0x4171, 0xAE01, 0xAD01, 0xAD04, 0x5204,
                0x10DB, 0x4E09, 0x4E0A, 0x4E0B, 0x4E0C, 0x4E0D, 0x4E0E,
            ]
        );

        // The WiFi-handoff pair belongs to the BT-time Identify and must never appear here. The
        // 0x4E0x device updates DO now appear — they are ordinary metadata (device name, language,
        // time, UUID) declared by the `device_info` feature, and the wired identify has carried
        // 0x4E0A/0x4E0B/0x4E0E for its whole history.
        for handoff_id in [0x5702u16, 0x5703] {
            assert!(
                !sent_ids.contains(&handoff_id) && !recv_ids.contains(&handoff_id),
                "AirPlayTunnel params 6/7 must not declare WiFi-handoff id 0x{handoff_id:04X}"
            );
        }

        let ids = top_level_param_ids(&body);
        assert!(
            ids.contains(&spec::ident::BLUETOOTH_TRANSPORT_COMPONENT),
            "AirPlayTunnel must still declare param 17 (required companion of param 24)"
        );
        assert!(
            ids.contains(&spec::ident::WIRELESS_CAR_PLAY_TRANSPORT_COMPONENT),
            "AirPlayTunnel must declare param 24 (it's still a wireless-CarPlay-capable accessory)"
        );
        assert!(
            !ids.contains(&spec::ident::USB_HOST_TRANSPORT_COMPONENT),
            "AirPlayTunnel must not declare param 16 (USB transport) — that's the wired arm only"
        );
    }

    #[test]
    fn ident_info_wireless_transport_declares_route_guidance_param30() {
        // docs/wireless/00_WIRELESS_CARPLAY.md Phase 5.0 (2026-07-24, EXPERIMENTAL): the wireless Identify now declares param 30
        // (RouteGuidanceDisplayComponent, id=42) unconditionally, matching the wired arm byte-for-byte
        // — this pins that so a future revert-back-to-wired-only-gate (see the ROLLBACK comment above
        // the param-30 block) would be caught by a failing test, not silently reintroduced. If Phase
        // 5.0 is ever rolled back, this test should be deleted/inverted alongside it.
        let mac = [0x38, 0xba, 0xb0, 0x7d, 0xd3, 0x8d];
        let body = build_ident_info(
            "CarLink",
            TransportComponent::Wireless { bt_mac: mac },
            false,
        );
        let mut value = Vec::new();
        value.extend_from_slice(&[0x00, 0x06, 0x00, 0x00, 0x00, 0x2A]); // u16v(0, 42)
        value.extend_from_slice(&[0x00, 0x12, 0x00, 0x01]); // "RouteGuidance" is 13 chars, len=4+14=0x12
        value.extend_from_slice(b"RouteGuidance\0");
        assert_eq!(value.len(), 24, "sub-TLVs must total 24 bytes");

        let group_hdr = [0x00, (4 + value.len()) as u8, 0x00, 0x1E]; // param 30 = 0x001E
        assert_eq!(group_hdr, [0x00, 0x1C, 0x00, 0x1E]);
        let needle = [&group_hdr[..], &value[..]].concat();
        assert!(
            body.windows(needle.len()).any(|w| w == needle.as_slice()),
            "param 30 RouteGuidanceDisplayComponent not found byte-exact in wireless ident_info body"
        );

        let ids = top_level_param_ids(&body);
        assert!(
            ids.contains(&spec::ident::ROUTE_GUIDANCE_DISPLAY_COMPONENT),
            "wireless top-level params must now include param 30"
        );
    }

    #[test]
    fn ident_info_wireless_transport_includes_required_param17_companion() {
        // Device-confirmed 2026-07-07 (docs/carplay/04_CAPABILITIES_AND_CONFIG.md): param 24 without this companion param 17
        // (bluetoothTransportComponent) gets an 0x1D03 reject from a real iPhone instead of
        // 0x1D02 IdentifyAccept. Byte-exact against the reference's own captured fixture.
        let mac = [0x38, 0xba, 0xb0, 0x7d, 0xd3, 0x8d];
        let body = build_ident_info(
            "CarLink",
            TransportComponent::Wireless { bt_mac: mac },
            false,
        );
        let mut value = Vec::new();
        value.extend_from_slice(&[0x00, 0x06, 0x00, 0x00, 0x00, 0x01]); // u16v(0,1)
        value.extend_from_slice(&[0x00, 0x13, 0x00, 0x01]); // "IAP2-Bluetooth" is 14 chars, len=4+15=19=0x13
        value.extend_from_slice(b"IAP2-Bluetooth\0");
        value.extend_from_slice(&[0x00, 0x04, 0x00, 0x02]); // none(2)
        value.extend_from_slice(&[0x00, 0x0A, 0x00, 0x03]); // bytes(3, 6-byte MAC): len=4+6=10=0x0A
        value.extend_from_slice(&mac);
        assert_eq!(
            value.len(),
            39,
            "sub-TLVs must total 39 bytes per the captured fixture"
        );

        let group_hdr = [0x00, (4 + value.len()) as u8, 0x00, 0x11]; // param 17 = 0x0011
        assert_eq!(
            group_hdr,
            [0x00, 0x2B, 0x00, 0x11],
            "outer group header must match the capture exactly"
        );
        let needle = [&group_hdr[..], &value[..]].concat();
        assert!(
            body.windows(needle.len()).any(|w| w == needle.as_slice()),
            "param 17 bluetoothTransportComponent group not found byte-exact in ident_info body"
        );
        // param 17 must appear BEFORE param 24 (matching the captured wire order).
        let pos17 = body.windows(4).position(|w| w == group_hdr).unwrap();
        let pos24 = body
            .windows(4)
            .position(|w| w == [0x00, 0x24, 0x00, 0x18])
            .unwrap();
        assert!(
            pos17 < pos24,
            "param 17 must precede param 24, matching capture order"
        );
    }

    #[test]
    fn scalar_tlv_helpers_match_layout() {
        let mut a = Tlv::new();
        a.u32v(2, 0x0102_0304); // Port-style uint32: len = 8
        assert_eq!(a.0, vec![0x00, 0x08, 0x00, 0x02, 0x01, 0x02, 0x03, 0x04]);

        let mut b = Tlv::new();
        b.i32v(1, -2); // AssetVersion-style int32 (two's complement 0xFFFFFFFE)
        assert_eq!(b.0, vec![0x00, 0x08, 0x00, 0x01, 0xFF, 0xFF, 0xFF, 0xFE]);

        let mut c = Tlv::new();
        c.boolv(8, true); // SupportsMutualAuthentication-style bool: len = 5, one 0/1 byte
        assert_eq!(c.0, vec![0x00, 0x05, 0x00, 0x08, 0x01]);
        let mut d = Tlv::new();
        d.boolv(8, false);
        assert_eq!(d.0, vec![0x00, 0x05, 0x00, 0x08, 0x00]);
    }

    #[test]
    fn vehicle_status_component_byte_layout() {
        // param 21 group: [4+vlen][pid=0x15] <u16v(0,7)> <str(1,"VS")> <none(3)> <none(17)>
        let g = build_vehicle_status_component(
            7,
            "VS",
            &[vehicle_status::RANGE, vehicle_status::WIPER_STATUS],
        );
        let mut value = Vec::new();
        value.extend_from_slice(&[0x00, 0x06, 0x00, 0x00, 0x00, 0x07]); // u16v(0,7)
        value.extend_from_slice(&[0x00, 0x07, 0x00, 0x01, b'V', b'S', 0x00]); // str(1,"VS")
        value.extend_from_slice(&[0x00, 0x04, 0x00, 0x03]); // none(3) Range
        value.extend_from_slice(&[0x00, 0x04, 0x00, 0x11]); // none(17) WiperStatus
        let mut expected = vec![0x00, (4 + value.len()) as u8, 0x00, 0x15]; // param 21 = 0x0015
        expected.extend_from_slice(&value);
        assert_eq!(g, expected);
    }

    #[test]
    fn location_information_component_byte_layout() {
        // param 22 group: [4+vlen][pid=0x16] <u16v(0,3)> <str(1,"LOC")> <none(17)>
        let g = build_location_information_component(
            3,
            "LOC",
            &[location_info::GLOBAL_POSITIONING_SYSTEM_FIX_DATA],
        );
        let mut value = Vec::new();
        value.extend_from_slice(&[0x00, 0x06, 0x00, 0x00, 0x00, 0x03]); // u16v(0,3)
        value.extend_from_slice(&[0x00, 0x08, 0x00, 0x01, b'L', b'O', b'C', 0x00]); // str(1,"LOC")
        value.extend_from_slice(&[0x00, 0x04, 0x00, 0x11]); // none(17) GPGGA capable
        let mut expected = vec![0x00, (4 + value.len()) as u8, 0x00, 0x16]; // param 22 = 0x0016
        expected.extend_from_slice(&value);
        assert_eq!(g, expected);
    }

    /// Walk a top-level 0x1D01 body into its `(param_id)` sequence: each entry is
    /// `[group_len BE16][pid BE16][value(group_len-4)]`.
    fn top_level_param_ids(body: &[u8]) -> Vec<u16> {
        let mut ids = Vec::new();
        let mut i = 0;
        while i + 4 <= body.len() {
            let glen = u16::from_be_bytes([body[i], body[i + 1]]) as usize;
            let pid = u16::from_be_bytes([body[i + 2], body[i + 3]]);
            assert!(
                glen >= 4 && i + glen <= body.len(),
                "malformed TLV group at {i}"
            );
            ids.push(pid);
            i += glen;
        }
        assert_eq!(i, body.len(), "trailing bytes after last TLV group");
        ids
    }

    /// The VALUE bytes of one top-level param group (`None` when the param is absent).
    fn top_level_param_value(body: &[u8], want: u16) -> Option<Vec<u8>> {
        let mut i = 0;
        while i + 4 <= body.len() {
            let glen = u16::from_be_bytes([body[i], body[i + 1]]) as usize;
            let pid = u16::from_be_bytes([body[i + 2], body[i + 3]]);
            assert!(glen >= 4 && i + glen <= body.len(), "malformed TLV group at {i}");
            if pid == want {
                return Some(body[i + 4..i + glen].to_vec());
            }
            i += glen;
        }
        None
    }

    /// A maximal pushed identity: hybrid engines, two connectors with power, a colour, and a param-21
    /// block. Deliberately exercises every field C6 added at once.
    fn maximal_ev_identity() -> crate::config::VehicleIdentity {
        use crate::spec::ident::vehicle_info as sub;
        crate::config::VehicleIdentity {
            engine_types: vec![sub::ENGINE_TYPE_GASOLINE, sub::ENGINE_TYPE_ELECTRIC],
            connectors: vec![
                (sub::SUPPORTED_CHARGING_CONNECTORS_CCS2, Some(150_000)),
                (sub::SUPPORTED_CHARGING_CONNECTORS_NACS_DC_, None),
            ],
            color_hex: Some("#1A2B3C".into()),
            status_caps: Some(vec![
                vehicle_status::RANGE,
                vehicle_status::RANGE_ELECTRIC,
                vehicle_status::ELECTRIC_CHARGE_INFO,
            ]),
        }
    }

    /// THE byte-pin that makes docs/wireless/00_WIRELESS_CARPLAY.md structural instead of conventional: whatever the app
    /// pushes, the BT-time (wireless) Identify is byte-identical to the baseline. A wireless params-20
    /// change breaks the WiFi handoff, and `0x1D03` is unrecoverable within a session — so this must
    /// be enforced by the builder, not by remembering not to pass an identity at the call site.
    #[test]
    fn identity_is_ignored_on_the_wireless_arm() {
        let transport = TransportComponent::Wireless {
            bt_mac: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
        };
        let baseline = build_ident_info_with(
            "CarLink",
            transport,
            false,
            &crate::config::VehicleIdentity::baseline(),
        );
        let pushed = build_ident_info_with("CarLink", transport, false, &maximal_ev_identity());
        assert_eq!(
            baseline, pushed,
            "the wireless Identify must not change when the app pushes a vehicle identity"
        );
        // And it must equal the historical no-identity emission too.
        assert_eq!(baseline, build_ident_info("CarLink", transport, false));
        // Param 21 must never appear on this arm even though the identity requested it.
        assert!(!top_level_param_ids(&pushed).contains(&spec::ident::VEHICLE_STATUS_COMPONENT));
    }

    /// THE BT-TIME WHOLE-BODY BYTE PIN — every byte of the wireless Identify, not just param 20.
    ///
    /// CLAUDE.md states "the Bluetooth-time Identify stays byte-pinned". Until this test that promise
    /// was only kept for param 20 (via `baseline_param20_is_byte_pinned` and the wireless identity
    /// substitution): an edit adding a WHOLE NEW PARAM to this arm passed the entire suite. That is
    /// exactly the failure docs/wireless/00_WIRELESS_CARPLAY.md records — iOS `0x1D03`-rejected params-6/7 growth here
    /// twice, breaking the WiFi handoff, and a `0x1D03` is unrecoverable within a session.
    ///
    /// DEVICE-ANCHORED, not merely self-consistent. This body is 301 bytes, and 301 is the length a
    /// real iPhone ACCEPTED: `docs/wireless/01_BT_AND_RADIO.md:63` —
    /// "[bt-driver] TX 0x1D01 IdentificationInformation (301 B, wireless transport)" followed by
    /// "IdentifyAccept ... RX 0x1D02 -> Identified". Contrast
    /// `docs/wireless/00_WIRELESS_CARPLAY.md:52`, where the grown 305-byte
    /// variant was rejected twice (and :90 for the 307-byte Phase-5.2 twin). The `bt_mac` is
    /// `bt_driver::ACCESSORY_BT_MAC`, i.e. what the box actually sends — its VALUE is documented
    /// cosmetic (any 6 bytes are accepted; only its PRESENCE is load-bearing), but using the real one
    /// keeps this literal readable as a true wire dump. The name is 12 chars because the real box sends
    /// `accessory_name("CarLink")` = "CarLink-<wifi-suffix>"; a 7-char "CarLink" yields 286 B and
    /// would NOT correspond to any capture.
    ///
    /// IF THIS TEST FAILS you are changing the BT-time Identify on the wire. Do not update the
    /// literal to make it pass. Re-read docs/wireless/00_WIRELESS_CARPLAY.md and docs/carplay/04_CAPABILITIES_AND_CONFIG.md first: the pin is a
    /// sequencing constraint pending per-transport applicability in the pushed config, and growth
    /// here has already cost this project the WiFi handoff twice.
    #[test]
    fn bt_time_identify_is_byte_pinned_whole_body() {
        const GOLDEN_HEX: &str = "\
00 11 00 00 43 61 72 4c 69 6e 6b 2d 62 30 64 66 \
00 00 18 00 01 4d 61 67 69 63 2d 43 61 72 2d 4c \
69 6e 6b 2d 31 2e 30 30 00 00 0f 00 02 4d 61 67 \
69 63 20 54 65 63 2e 00 00 18 00 03 32 30 32 35 \
2e 30 32 2e 32 35 2e 31 35 32 31 36 32 36 61 00 \
00 0a 00 04 31 2e 30 2e 30 00 00 0a 00 05 31 2e \
30 2e 30 00 00 06 00 06 57 03 00 0e 00 07 4e 0a \
4e 0b 57 02 4e 0d 4e 0e 00 05 00 08 02 00 06 00 \
09 00 00 00 07 00 0c 65 6e 00 00 07 00 0d 65 6e \
00 00 2b 00 11 00 06 00 00 00 01 00 13 00 01 49 \
41 50 32 2d 42 6c 75 65 74 6f 6f 74 68 00 00 04 \
00 02 00 0a 00 03 02 00 00 00 00 01 00 31 00 14 \
00 06 00 00 00 01 00 11 00 01 43 61 72 4c 69 6e \
6b 2d 62 30 64 66 00 00 05 00 02 00 00 11 00 06 \
43 61 72 4c 69 6e 6b 2d 62 30 64 66 00 00 24 00 \
18 00 06 00 00 00 02 00 12 00 01 49 41 50 32 2d \
57 69 72 65 6c 65 73 73 00 00 04 00 02 00 04 00 \
04 00 1c 00 1e 00 06 00 00 00 2a 00 12 00 01 52 \
6f 75 74 65 47 75 69 64 61 6e 63 65 00";
        let want: Vec<u8> = GOLDEN_HEX
            .split_whitespace()
            .map(|h| u8::from_str_radix(h, 16).expect("golden hex"))
            .collect();
        assert_eq!(want.len(), 301, "the golden itself must be the device-accepted length");

        let transport = TransportComponent::Wireless {
            // Locally-administered placeholder (02:00:00:00:00:01) — the golden body only needs a
            // deterministic 6-byte value here, not a real device's BT address.
            bt_mac: [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
        };
        assert_eq!(
            build_ident_info("CarLink-b0df", transport, false),
            want,
            "the BT-time Identify changed — see this test's doc comment BEFORE touching the literal"
        );
        // And it stays pinned against a pushed identity, which is what makes the wireless
        // substitution in the builder structural rather than conventional.
        assert_eq!(
            build_ident_info_with("CarLink-b0df", transport, false, &maximal_ev_identity()),
            want,
            "a pushed vehicle identity must not reach the BT-time Identify"
        );
    }

    /// Pins WRAPPER DELEGATION ONLY: that `build_ident_info` and `build_ident_info_with(baseline())`
    /// take the same path. It deliberately does NOT prove anything about the pre-C6 bytes and CANNOT
    /// — both sides funnel into `build_ident_info_excluding_with_policy` carrying the same identity,
    /// so any edit to `baseline()` moves them together. An earlier revision of this docstring claimed
    /// the byte property; a Gasoline→Diesel mutation left this test green, which is how the claim was
    /// caught. **The byte property is pinned by [`baseline_param20_is_byte_pinned`]** (a hardcoded
    /// fixture) and by `proven_tier_reproduces_the_wired_and_tunnel_baselines` (whole-body length).
    #[test]
    fn baseline_identity_reproduces_the_historical_body() {
        for (transport, declare_wired) in [
            (TransportComponent::Usb { cp_iface: 1 }, true),
            (TransportComponent::Usb { cp_iface: 1 }, false),
            (
                TransportComponent::AirPlayTunnel {
                    bt_mac: [0, 1, 2, 3, 4, 5],
                },
                false,
            ),
            (
                TransportComponent::Wireless {
                    bt_mac: [0, 1, 2, 3, 4, 5],
                },
                false,
            ),
        ] {
            assert_eq!(
                build_ident_info("CarLink", transport, declare_wired),
                build_ident_info_with(
                    "CarLink",
                    transport,
                    declare_wired,
                    &crate::config::VehicleIdentity::baseline()
                ),
                "baseline identity changed the bytes for {transport:?}/{declare_wired}"
            );
        }
    }

    /// The GOLDEN pre-C6 param-20 body, written out as literal bytes rather than re-derived from
    /// `VehicleIdentity::baseline()`.
    ///
    /// Without this, the equality above is tautological: both sides funnel into
    /// `build_ident_info_excluding_with_policy` carrying `baseline()`, so editing `baseline()` moves
    /// both sides together and the test cannot see it (proven — mutating the baseline engine type to
    /// Diesel leaves `baseline_identity_reproduces_the_historical_body` green). "Zero wire change"
    /// is the claim the whole workstream rests on, so it needs a constant to compare against.
    #[test]
    fn baseline_param20_is_byte_pinned() {
        // subs: u16v(0, id) | str(1,"CarLink") | u8v(2, EngineType::Gasoline=0) | str(6,"CarLink")
        let golden = |id: u8| {
            let mut v = vec![0x00, 0x06, 0x00, 0x00, 0x00, id];
            v.extend_from_slice(&[0x00, 0x0C, 0x00, 0x01]);
            v.extend_from_slice(b"CarLink\0");
            v.extend_from_slice(&[0x00, 0x05, 0x00, 0x02, 0x00]);
            v.extend_from_slice(&[0x00, 0x0C, 0x00, 0x06]);
            v.extend_from_slice(b"CarLink\0");
            v
        };
        for (transport, id) in [
            (TransportComponent::Usb { cp_iface: 1 }, 2u8),
            (
                TransportComponent::AirPlayTunnel {
                    bt_mac: [0, 1, 2, 3, 4, 5],
                },
                1,
            ),
            (
                TransportComponent::Wireless {
                    bt_mac: [0, 1, 2, 3, 4, 5],
                },
                1,
            ),
        ] {
            let body = build_ident_info("CarLink", transport, false);
            assert_eq!(
                top_level_param_value(&body, spec::ident::VEHICLE_INFORMATION_COMPONENT)
                    .expect("param 20 present"),
                golden(id),
                "pre-C6 param 20 drifted for {transport:?}"
            );
        }
    }

    /// Param-20 EV encoding, asserted as exact bytes: sub order ascending, one sub-2 per engine type
    /// (Apple marks it `[0+]`, verified from the compiled spec archive), connectors before their
    /// power values, and a power sub emitted ONLY for the connector that carried one.
    #[test]
    fn param20_encodes_the_pushed_ev_identity() {
        use crate::spec::ident::vehicle_info as sub;
        let body = build_ident_info_with(
            "CarLink",
            TransportComponent::Usb { cp_iface: 1 },
            false,
            &maximal_ev_identity(),
        );
        let g20 = top_level_param_value(&body, spec::ident::VEHICLE_INFORMATION_COMPONENT)
            .expect("param 20 present");

        let mut want = Tlv::new();
        want.u16v(sub::IDENTIFIER, 2); // wired keeps identifier 2 (byte-stable)
        want.str(sub::NAME, "CarLink");
        want.u8v(sub::ENGINE_TYPE, sub::ENGINE_TYPE_GASOLINE);
        want.u8v(sub::ENGINE_TYPE, sub::ENGINE_TYPE_ELECTRIC);
        want.str(sub::DISPLAY_NAME, "CarLink");
        want.str(sub::VEHICLE_COLOR_HEX_CODE, "#1A2B3C");
        want.u8v(sub::SUPPORTED_CHARGING_CONNECTORS, sub::SUPPORTED_CHARGING_CONNECTORS_CCS2);
        want.u8v(sub::SUPPORTED_CHARGING_CONNECTORS, sub::SUPPORTED_CHARGING_CONNECTORS_NACS_DC_);
        want.u32v(sub::POWER_FOR_CONNECTOR_TYPE_CCS2, 150_000);
        // NACS_DC carried no powerWatts, so no sub 19 — an absent rating must not become a zero one.
        assert_eq!(g20, want.0);
    }

    /// Param 21 is emitted ONLY when the app pushed a `vehicleStatus:` block, and its contents match
    /// the standalone builder (the two share `vehicle_status_inner`, and this proves it).
    #[test]
    fn param21_is_config_gated_and_matches_the_standalone_builder() {
        let transport = TransportComponent::Usb { cp_iface: 1 };
        let mut id = crate::config::VehicleIdentity::baseline();

        // status_caps: None -> absent (the historical default; the session-critical guard).
        let without = build_ident_info_with("CarLink", transport, false, &id);
        assert!(!top_level_param_ids(&without).contains(&spec::ident::VEHICLE_STATUS_COMPONENT));

        let caps = vec![vehicle_status::RANGE_ELECTRIC, vehicle_status::ELECTRIC_CHARGE_INFO];
        id.status_caps = Some(caps.clone());
        let with = build_ident_info_with("CarLink", transport, false, &id);
        let ids = top_level_param_ids(&with);
        assert!(ids.contains(&spec::ident::VEHICLE_STATUS_COMPONENT));
        // Ascending param order is preserved: 21 lands directly after 20.
        let p20 = ids.iter().position(|&p| p == spec::ident::VEHICLE_INFORMATION_COMPONENT);
        let p21 = ids.iter().position(|&p| p == spec::ident::VEHICLE_STATUS_COMPONENT);
        assert_eq!(p21, p20.map(|i| i + 1), "param 21 must follow param 20");
        // Same bytes as the standalone builder's group value.
        let standalone = build_vehicle_status_component(3, "CarLink", &caps);
        assert_eq!(
            top_level_param_value(&with, spec::ident::VEHICLE_STATUS_COMPONENT).unwrap(),
            standalone[4..].to_vec()
        );
        // An EMPTY capability list still emits the component (presence of the block is the arm).
        id.status_caps = Some(Vec::new());
        let empty = build_ident_info_with("CarLink", transport, false, &id);
        assert!(top_level_param_ids(&empty).contains(&spec::ident::VEHICLE_STATUS_COMPONENT));
    }

    /// The byte-pin is ONE arm, not "everything that isn't USB". docs/carplay/04_CAPABILITIES_AND_CONFIG.md scopes the app-pushed
    /// config to the WIRED *and* AirPlayTunnel Identifies, so the tunnel must actually receive what
    /// the app pushed. Widening the pin to `!matches!(…Usb…)` — a one-token slip that reads as a
    /// tightening — strands the tunnel on the baseline, and before this test the ENTIRE lib suite
    /// still passed under exactly that mutation (`identity_is_ignored_on_the_wireless_arm` only
    /// pins the direction that must NOT change).
    #[test]
    fn identity_reaches_the_airplay_tunnel_arm() {
        use crate::spec::ident::vehicle_info as sub;
        let transport = TransportComponent::AirPlayTunnel {
            bt_mac: [0, 1, 2, 3, 4, 5],
        };
        let pushed = build_ident_info_with("CarLink", transport, false, &maximal_ev_identity());
        assert_ne!(
            pushed,
            build_ident_info("CarLink", transport, false),
            "the AirPlayTunnel Identify must reflect a pushed vehicle identity"
        );

        // Same param-20 body as the wired arm asserts, except the Identifier: the tunnel keeps the
        // C reference's 1 (see `vehicle_id`), and that byte must not drift either.
        let mut want = Tlv::new();
        want.u16v(sub::IDENTIFIER, 1);
        want.str(sub::NAME, "CarLink");
        want.u8v(sub::ENGINE_TYPE, sub::ENGINE_TYPE_GASOLINE);
        want.u8v(sub::ENGINE_TYPE, sub::ENGINE_TYPE_ELECTRIC);
        want.str(sub::DISPLAY_NAME, "CarLink");
        want.str(sub::VEHICLE_COLOR_HEX_CODE, "#1A2B3C");
        want.u8v(sub::SUPPORTED_CHARGING_CONNECTORS, sub::SUPPORTED_CHARGING_CONNECTORS_CCS2);
        want.u8v(sub::SUPPORTED_CHARGING_CONNECTORS, sub::SUPPORTED_CHARGING_CONNECTORS_NACS_DC_);
        want.u32v(sub::POWER_FOR_CONNECTOR_TYPE_CCS2, 150_000);
        assert_eq!(
            top_level_param_value(&pushed, spec::ident::VEHICLE_INFORMATION_COMPONENT)
                .expect("param 20 present"),
            want.0
        );
        // Param 21 is config-gated, so its presence here is only reachable via the pushed identity.
        assert!(top_level_param_ids(&pushed).contains(&spec::ident::VEHICLE_STATUS_COMPONENT));
    }

    /// A `0x1D03` retry must be able to strip params 20 AND 21 once they carry pushed content.
    /// Neither is in `REQUIRED_IDENT_PARAMS`, so the required-param filter must not veto them, and
    /// the retry must drop the WHOLE group — an EV param 20 that shed only its new subs, or a
    /// param 21 that escaped `exclude` entirely, would re-send the exact thing iOS just rejected and
    /// abort the session. `build_ident_info_excluding_with` had no test at all before this one.
    #[test]
    fn a_1d03_retry_can_strip_the_pushed_vehicle_params() {
        let transport = TransportComponent::Usb { cp_iface: 1 };
        let id = maximal_ev_identity();
        let full_ids = top_level_param_ids(&build_ident_info_with("CarLink", transport, false, &id));
        for pid in [
            spec::ident::VEHICLE_INFORMATION_COMPONENT,
            spec::ident::VEHICLE_STATUS_COMPONENT,
        ] {
            assert!(full_ids.contains(&pid), "pushed body must contain param {pid}");
            assert!(
                !REQUIRED_IDENT_PARAMS.contains(&pid),
                "param {pid} must stay strippable on a retry"
            );
        }

        for exclude in [
            vec![spec::ident::VEHICLE_INFORMATION_COMPONENT],
            vec![spec::ident::VEHICLE_STATUS_COMPONENT],
            vec![
                spec::ident::VEHICLE_INFORMATION_COMPONENT,
                spec::ident::VEHICLE_STATUS_COMPONENT,
            ],
        ] {
            let retry =
                build_ident_info_excluding_with("CarLink", transport, false, &exclude, &id);
            let ids = top_level_param_ids(&retry);
            // Exactly the excluded params vanish; every survivor keeps its place.
            let expected: Vec<u16> = full_ids
                .iter()
                .copied()
                .filter(|p| !exclude.contains(p))
                .collect();
            assert_eq!(ids, expected, "retry with exclude={exclude:?}");
            // And the group is gone WHOLE — no orphaned EV sub-TLV left loose in the body. The
            // 150 kW power rating only ever exists inside param 20, so its bytes are the probe.
            let watts = 150_000u32.to_be_bytes();
            assert_eq!(
                retry.windows(4).any(|w| w == watts),
                !exclude.contains(&spec::ident::VEHICLE_INFORMATION_COMPONENT),
                "param 20's EV subs must survive or vanish WITH the group (exclude={exclude:?})"
            );
        }
    }

    #[test]
    fn default_ident_emission_excludes_optional_components() {
        // SESSION-CRITICAL guard: params 21 (VehicleStatusComponent) and 22
        // (LocationInformationComponent) must NEVER appear as top-level params in the default
        // 0x1D01 body for either transport — they are opt-in helpers only.
        for transport in [
            TransportComponent::Usb { cp_iface: 1 },
            TransportComponent::Wireless { bt_mac: [0; 6] },
        ] {
            for declare_wired in [false, true] {
                let body = build_ident_info("CarLink", transport, declare_wired);
                let ids = top_level_param_ids(&body);
                assert!(
                    !ids.contains(&spec::ident::VEHICLE_STATUS_COMPONENT),
                    "param 21 (VehicleStatusComponent) must not be emitted by default"
                );
                assert!(
                    !ids.contains(&spec::ident::LOCATION_INFORMATION_COMPONENT),
                    "param 22 (LocationInformationComponent) must not be emitted by default"
                );
                // Param 20 (VehicleInformationComponent) SHOULD still be present — it is baseline.
                assert!(
                    ids.contains(&spec::ident::VEHICLE_INFORMATION_COMPONENT),
                    "param 20 (VehicleInformationComponent) is baseline and must remain"
                );
            }
        }
    }

    #[test]
    fn build_ident_info_delegates_byte_identically_with_empty_exclude() {
        // SESSION-CRITICAL: the default emission (empty exclude) MUST be byte-for-byte identical to
        // the historical device-verified payload for EVERY transport / declare_wired combination.
        for transport in [
            TransportComponent::Usb { cp_iface: 1 },
            TransportComponent::Usb { cp_iface: 7 },
            TransportComponent::Wireless {
                bt_mac: [0x38, 0xba, 0xb0, 0x7d, 0xd3, 0x8d],
            },
        ] {
            for declare_wired in [false, true] {
                let default_body = build_ident_info("CarLink", transport, declare_wired);
                let excluding_empty =
                    build_ident_info_excluding("CarLink", transport, declare_wired, &[]);
                assert_eq!(
                    default_body, excluding_empty,
                    "empty-exclude output must equal build_ident_info byte-for-byte"
                );
            }
        }
    }

    #[test]
    fn excluding_drops_only_the_named_top_level_params() {
        let transport = TransportComponent::Usb { cp_iface: 1 };
        let full = build_ident_info("CarLink", transport, false);
        let full_ids = top_level_param_ids(&full);
        // Sanity: the baseline USB body contains these top-level params.
        for pid in [
            spec::ident::NAME,
            spec::ident::ROUTE_GUIDANCE_DISPLAY_COMPONENT,
            spec::ident::VEHICLE_INFORMATION_COMPONENT,
        ] {
            assert!(full_ids.contains(&pid), "baseline must contain param {pid}");
        }

        // Strip the RouteGuidanceDisplayComponent (30) + VehicleInformationComponent (20).
        let exclude = [
            spec::ident::ROUTE_GUIDANCE_DISPLAY_COMPONENT,
            spec::ident::VEHICLE_INFORMATION_COMPONENT,
        ];
        let stripped = build_ident_info_excluding("CarLink", transport, false, &exclude);
        let stripped_ids = top_level_param_ids(&stripped);
        assert!(!stripped_ids.contains(&spec::ident::ROUTE_GUIDANCE_DISPLAY_COMPONENT));
        assert!(!stripped_ids.contains(&spec::ident::VEHICLE_INFORMATION_COMPONENT));
        // Every OTHER top-level param that was present must still be present, in the same order.
        let expected: Vec<u16> = full_ids
            .iter()
            .copied()
            .filter(|id| !exclude.contains(id))
            .collect();
        assert_eq!(
            stripped_ids, expected,
            "only the excluded params may be dropped, order preserved"
        );
    }

    #[test]
    fn excluding_a_required_param_is_ignored() {
        // #407: a 0x1D03 IdentificationRejected retry must NEVER strip a REQUIRED param. Excluding
        // Name (param 0) is therefore a NO-OP — Name is retained so the retry stays structurally valid,
        // instead of the old behavior that dropped it and guaranteed a second reject → abort.
        let transport = TransportComponent::Usb { cp_iface: 1 };
        let body = build_ident_info_excluding("CarLink", transport, false, &[spec::ident::NAME]);
        let ids = top_level_param_ids(&body); // panics if malformed
        assert!(
            ids.contains(&spec::ident::NAME),
            "required Name must survive a reject-strip"
        );
        assert_eq!(
            ids.first(),
            Some(&spec::ident::NAME),
            "Name (param 0) remains first"
        );
    }

    #[test]
    fn parse_rejected_param_ids_single_none_param() {
        // One rejected top-level `none` param 24 (WirelessCarPlayTransportComponent): 00 04 00 18.
        let body = [0x00, 0x04, 0x00, 0x18];
        assert_eq!(parse_rejected_param_ids(&body), vec![0x0018]);
    }

    #[test]
    fn parse_rejected_param_ids_multiple_params_including_valued() {
        // param 30 (none), param 6 (uint16[] value = [0x415A]), param 20 (none).
        let mut body = Vec::new();
        body.extend_from_slice(&[0x00, 0x04, 0x00, 0x1E]); // none(30)
        body.extend_from_slice(&[0x00, 0x06, 0x00, 0x06, 0x41, 0x5A]); // bytes(6, [0x41,0x5A])
        body.extend_from_slice(&[0x00, 0x04, 0x00, 0x14]); // none(20)
        assert_eq!(parse_rejected_param_ids(&body), vec![30, 6, 20]);
    }

    #[test]
    fn parse_rejected_param_ids_empty_body() {
        assert_eq!(parse_rejected_param_ids(&[]), Vec::<u16>::new());
    }

    #[test]
    fn parse_rejected_param_ids_stops_on_truncated_trailing_group() {
        // A valid none(24) then a truncated group header claiming len 0x0008 with only 2 bytes left:
        // the walk yields the recovered id and stops rather than reading OOB.
        let body = [0x00, 0x04, 0x00, 0x18, 0x00, 0x08, 0x00, 0x1E]; // second group needs 8 bytes, only 4 present
        assert_eq!(parse_rejected_param_ids(&body), vec![0x0018]);
    }

    #[test]
    fn parse_rejected_param_ids_stops_on_undersized_group_len() {
        // A group advertising len < 4 (here 0x0003) is malformed — stop.
        let body = [0x00, 0x03, 0x00, 0x18];
        assert_eq!(parse_rejected_param_ids(&body), Vec::<u16>::new());
    }

    #[test]
    fn parse_rejected_param_ids_round_trips_with_builder_exclusion() {
        // A device that rejects the WirelessCarPlayTransportComponent (24) sends 0x1D03 with a
        // present param 24; feeding parse_rejected_param_ids -> build_ident_info_excluding must drop
        // exactly that top-level group.
        let reject_body = [0x00, 0x04, 0x00, 0x18]; // none(24)
        let rejected = parse_rejected_param_ids(&reject_body);
        assert_eq!(
            rejected,
            vec![spec::ident::WIRELESS_CAR_PLAY_TRANSPORT_COMPONENT]
        );
        let transport = TransportComponent::Wireless { bt_mac: [0; 6] };
        let retry = build_ident_info_excluding("CarLink", transport, false, &rejected);
        let ids = top_level_param_ids(&retry);
        assert!(
            !ids.contains(&spec::ident::WIRELESS_CAR_PLAY_TRANSPORT_COMPONENT),
            "the rejected param 24 must be absent from the retry payload"
        );
    }

    /// `describe_reject` is the shared 0x1D03 decoder both `bt_driver.rs` (wireless) and `iap2d`
    /// (wired) log verbatim — a param with no nested message-id detail must still name the param,
    /// and a param carrying nested ids must name each one.
    #[test]
    fn describe_reject_names_bare_param_and_nested_message_ids() {
        // param 30 (RouteGuidanceDisplayComponent), no nested detail.
        let bare = [0x00, 0x04, 0x00, 0x1E];
        assert_eq!(describe_reject(&bare), "param 30 (no detail)");

        // param 6 (MessagesSentByAccessory) carrying one rejected message id, 0x415A.
        let with_ids = [0x00, 0x06, 0x00, 0x06, 0x41, 0x5A];
        let desc = describe_reject(&with_ids);
        assert!(
            desc.starts_with("param 6 unsupported: 0x415A"),
            "expected param 6 with the named message id, got {desc:?}"
        );
    }
}

