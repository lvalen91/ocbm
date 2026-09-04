//! wifi_handoff.rs — DESIGN Phase A3 scaffold: the iAP2 WiFi credential handoff (0x5700–0x5703).
//!
//! WHAT THIS IS. Spec-accurate message *definitions* (types + encoders + decoders) for the four
//! WiFi-handoff messages Apple's authoritative catalog specifies. Once the iAP2 control session is
//! up (post-IdentificationAccepted, see `bt_driver.rs`), this cluster is how the accessory hands the
//! iPhone the Wi-Fi AP credentials it must join to bring up the wireless-CarPlay video/audio bearer.
//!
//! DECODED FROM Apple's internal `.i2mspecarchive` (CarPlaySimulator.devicekitplugin, Xcode 27):
//!
//! | ID       | Dir        | Message                                      | Params |
//! |----------|------------|----------------------------------------------|--------|
//! | `0x5700` | accessory→ | RequestWiFiInformation                       | 0      |
//! | `0x5701` | device→    | WiFiInformation                              | 4      |
//! | `0x5702` | device→    | RequestAccessoryWiFiConfigurationInformation | 0      |
//! | `0x5703` | accessory→ | AccessoryWiFiConfigurationInformation        | 4      |
//!
//! TWO INDEPENDENT SUB-FLOWS (do not conflate them):
//!   * `RequestWiFiInformation`(0x5700, we send) → `WiFiInformation`(0x5701, device replies): the
//!     accessory asks the *device* for the SSID/passphrase of a network the device already knows.
//!   * `RequestAccessoryWiFiConfigurationInformation`(0x5702, device sends) →
//!     `AccessoryWiFiConfigurationInformation`(0x5703, we reply): the *device* asks the accessory to
//!     describe the AP the accessory itself is hosting, so the phone can join it. THIS is the one
//!     wireless CarPlay actually needs — our head unit hosts the AP and answers 0x5703.
//!
//! ⚠️ SecurityType is TWO DIFFERENT enums. Param 1 of 0x5701 and param 3 of 0x5703 are both named
//! "SecurityType" in the spec but have DIFFERENT value tables (see the two enums below). They are
//! kept as separate types deliberately.
//!
//! ⚠️ 0x5703 has NO param 0. Its params are ids 1..=4 (WiFiSSID=1, Passphrase=2, SecurityType=3,
//! Channel=4). 0x5701's params are ids 0..=3 (RequestStatus=0, SecurityType=1, WiFiSSID=2,
//! WiFiPassphrase=3).
//!
//! TLV BODY LAYOUT (shared iAP2 substrate, see iap2-core `message::Tlv`): a concatenation of groups
//! `[group_len BE16][param_id BE16][value...]`, `group_len = 4 + value.len()`. utf8 values are
//! NUL-terminated on the wire (matching `build_ident_info`'s device-verified convention); enums and
//! `uint8` are a single byte. These builders/parsers operate on that *body* — the caller wraps it
//! with `link.build_msg(sess, msg_id, body)` and strips the 6-byte message header before parsing.
//!
//! INTEGRATION BOUNDARY (TODO — on-hardware work, intentionally NOT wired here). These functions are
//! pure byte codecs with full unit coverage. What is deliberately left out because it cannot be
//! verified on this host and would be inventing behavior:
//!   1. Dispatch: routing 0x5702→build 0x5703 and 0x5700→parse 0x5701 inside `bt_driver`'s state
//!      machine. That requires an `iap2-core::state` addition (state.rs is owned by another agent /
//!      is device-scarred) — see the request to the coordinator in this task's report.
//!   2. The actual Wi-Fi AP: hostapd/SSID/passphrase/channel provisioning that `AccessoryWiFiConfig`
//!      must describe truthfully. `AccessoryWiFiConfig` is the *contract*; the socket/AP wiring that
//!      fills it from the live hostapd config is on-target work.
//!
//! This module is marked `#![allow(dead_code)]` for exactly that reason: it is a verified-correct
//! definition layer awaiting its on-hardware dispatch/AP glue, not dead code to delete.

#![allow(dead_code)]

use carplay_iap2_core::{message::Tlv, spec};

// ---- 0x5701 WiFiInformation (device→) enums -------------------------------------------------

/// `WiFiInformation` (0x5701) param 0 — **RequestStatus** (`enum`, required). Apple's value table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WiFiRequestStatus {
    Success = 0,
    UserDeclined = 1,
    NetworkInformationUnavailable = 2,
}

impl WiFiRequestStatus {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Success),
            1 => Some(Self::UserDeclined),
            2 => Some(Self::NetworkInformationUnavailable),
            _ => None,
        }
    }
}

/// `WiFiInformation` (0x5701) param 1 — **SecurityType** (`enum`, optional). Apple's value table for
/// this message describes the security of the *device-known* network (note: distinct from the 0x5703
/// SecurityType enum, which is the accessory-hosted AP's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceWiFiSecurityType {
    Wep = 0,
    Wpa = 1,
    Wpa2 = 2,
    MixedWpaWpa2 = 3,
}

impl DeviceWiFiSecurityType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Wep),
            1 => Some(Self::Wpa),
            2 => Some(Self::Wpa2),
            3 => Some(Self::MixedWpaWpa2),
            _ => None,
        }
    }
}

/// `AccessoryWiFiConfigurationInformation` (0x5703) param 3 — **SecurityType** (`enum`, optional).
/// Apple's value table for the *accessory-hosted* AP. Note value 0 = None and value 2 folds
/// "WPA2 Personal/WPA3 Personal Transition Mode" — this is NOT the same enum as 0x5701's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessoryWiFiSecurityType {
    None = 0,
    Wep = 1,
    /// WPA2 Personal / WPA3 Personal Transition Mode.
    Wpa2OrWpa3Personal = 2,
}

impl AccessoryWiFiSecurityType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::None),
            1 => Some(Self::Wep),
            2 => Some(Self::Wpa2OrWpa3Personal),
            _ => None,
        }
    }
}

// ---- Param ids (from Apple's spec; local consts since spec.rs has no per-param submodule here) ----

/// Param ids inside `WiFiInformation` (0x5701).
mod wifi_info_param {
    pub const REQUEST_STATUS: u16 = 0; // enum, required
    pub const SECURITY_TYPE: u16 = 1; // enum, optional
    pub const WIFI_SSID: u16 = 2; // utf8, optional
    pub const WIFI_PASSPHRASE: u16 = 3; // utf8, optional
}

/// Param ids inside `AccessoryWiFiConfigurationInformation` (0x5703). NB: no param 0.
mod accessory_wifi_param {
    pub const WIFI_SSID: u16 = 1; // utf8, required
    pub const PASSPHRASE: u16 = 2; // utf8, optional (required if SecurityType != None)
    pub const SECURITY_TYPE: u16 = 3; // enum, optional
    pub const CHANNEL: u16 = 4; // uint8, optional
}

// ---- 0x5700 RequestWiFiInformation (accessory→, 0 params) -----------------------------------

/// Build the body of `RequestWiFiInformation` (0x5700). Zero parameters ⇒ empty body. Send with
/// `link.build_msg(sess, spec::MSG_REQUEST_WI_FI_INFORMATION, &body)`.
pub fn build_request_wifi_information() -> Vec<u8> {
    debug_assert_eq!(spec::MSG_REQUEST_WI_FI_INFORMATION, 0x5700);
    Vec::new()
}

// ---- 0x5701 WiFiInformation (device→) — PARSE ------------------------------------------------

/// Decoded `WiFiInformation` (0x5701). `request_status` is required by the spec; the rest are
/// optional and only present on `Success`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiFiInformation {
    pub request_status: Option<WiFiRequestStatus>,
    pub security_type: Option<DeviceWiFiSecurityType>,
    pub ssid: Option<String>,
    pub passphrase: Option<String>,
}

/// Parse a `WiFiInformation` (0x5701) message **body** (TLV groups, header already stripped).
/// Unknown params are ignored; malformed groups terminate parsing at that point.
pub fn parse_wifi_information(body: &[u8]) -> WiFiInformation {
    let mut out = WiFiInformation {
        request_status: None,
        security_type: None,
        ssid: None,
        passphrase: None,
    };
    for (pid, value) in TlvGroups::new(body) {
        match pid {
            wifi_info_param::REQUEST_STATUS => {
                out.request_status = value.first().and_then(|&b| WiFiRequestStatus::from_u8(b));
            }
            wifi_info_param::SECURITY_TYPE => {
                out.security_type = value
                    .first()
                    .and_then(|&b| DeviceWiFiSecurityType::from_u8(b));
            }
            wifi_info_param::WIFI_SSID => out.ssid = Some(decode_str(value)),
            wifi_info_param::WIFI_PASSPHRASE => out.passphrase = Some(decode_str(value)),
            _ => {}
        }
    }
    out
}

// ---- 0x5702 RequestAccessoryWiFiConfigurationInformation (device→, 0 params) -----------------

/// `RequestAccessoryWiFiConfigurationInformation` (0x5702) carries no parameters; receipt is the
/// entire signal. Returns `true` if `body` is the expected empty body (tolerates trailing padding).
pub fn is_request_accessory_wifi_configuration(body: &[u8]) -> bool {
    debug_assert_eq!(
        spec::MSG_REQUEST_ACCESSORY_WI_FI_CONFIGURATION_INFORMATION,
        0x5702
    );
    body.is_empty()
}

// ---- 0x5703 AccessoryWiFiConfigurationInformation (accessory→) — BUILD -----------------------

/// The accessory-hosted AP configuration the head unit reports in response to 0x5702. `ssid` is the
/// only required field; `passphrase` is required by the spec whenever `security_type != None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessoryWiFiConfig {
    pub ssid: String,
    pub passphrase: Option<String>,
    pub security_type: Option<AccessoryWiFiSecurityType>,
    pub channel: Option<u8>,
}

/// Build the body of `AccessoryWiFiConfigurationInformation` (0x5703). Params are emitted in
/// ascending id order (1,2,3,4), matching `build_ident_info`'s convention. Send with
/// `link.build_msg(sess, spec::MSG_ACCESSORY_WI_FI_CONFIGURATION_INFORMATION, &body)`.
pub fn build_accessory_wifi_configuration_information(cfg: &AccessoryWiFiConfig) -> Vec<u8> {
    debug_assert_eq!(spec::MSG_ACCESSORY_WI_FI_CONFIGURATION_INFORMATION, 0x5703);
    let mut tlv = Tlv::new();
    tlv.str(accessory_wifi_param::WIFI_SSID, &cfg.ssid);
    if let Some(pass) = &cfg.passphrase {
        tlv.str(accessory_wifi_param::PASSPHRASE, pass);
    }
    if let Some(sec) = cfg.security_type {
        tlv.u8v(accessory_wifi_param::SECURITY_TYPE, sec as u8);
    }
    if let Some(ch) = cfg.channel {
        tlv.u8v(accessory_wifi_param::CHANNEL, ch);
    }
    tlv.0
}

// ---- On-target AP glue: fill AccessoryWiFiConfig from the box's live hostapd.conf --------------

/// Read the box's hosted-AP configuration from `/etc/hostapd.conf` (the same file
/// `start_bluetooth_wifi.sh` raises the AP from) into the `AccessoryWiFiConfig` reported over 0x5703,
/// so the credentials handed to the iPhone are exactly what hostapd is broadcasting. Missing fields
/// fall back to sane defaults. `wpa=`/`wpa_key_mgmt=` present + a passphrase ⇒ WPA2/WPA3-Personal.
///
/// `None` when the file names no SSID. There is no safe default here: an invented SSID describes an
/// AP that is not running, so the phone leaves Bluetooth to join a network that does not exist and
/// never arrives — and the caller would have stood the A/V layer up and claimed the transport for
/// it. Callers must refuse rather than substitute.
pub fn read_hostapd_ap_config() -> Option<AccessoryWiFiConfig> {
    // Path override for hosts whose AP config does not live at the box's `/etc/hostapd.conf`. The
    // Raspberry Pi port runs hostapd standalone (outside Android's Wi-Fi framework) with its conf
    // under /data. Unset keeps the CCPA behaviour exactly as before.
    //
    // This is the answer the accessory gives to iAP2 0x5702 — the credentials the iPhone will use
    // to join. It MUST describe the AP that is actually running, or the phone leaves Bluetooth and
    // never arrives.
    let path = std::env::var("CARPLAY_HOSTAPD_CONF")
        .unwrap_or_else(|_| "/etc/hostapd.conf".to_string());
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    if text.is_empty() {
        eprintln!("[wifi_handoff] WARNING: {path} is empty or unreadable — 0x5703 will carry no credentials");
    }
    let mut ssid = String::new();
    let mut passphrase = String::new();
    let mut channel: Option<u8> = None;
    let mut has_wpa = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("ssid=") {
            ssid = v.to_string();
        } else if let Some(v) = line.strip_prefix("wpa_passphrase=") {
            passphrase = v.to_string();
        } else if line.starts_with("wpa_psk=") {
            // A raw hex PSK secures the AP but is NOT an ASCII passphrase we can hand the phone (audit B5).
            // Mark the AP secured so we never misreport it as OPEN just because it uses wpa_psk= instead of
            // wpa_passphrase=; auto-handoff still needs wpa_passphrase= for the phone to actually join.
            has_wpa = true;
        } else if let Some(v) = line.strip_prefix("channel=") {
            // channel=0 is hostapd ACS (auto-select), not a literal channel — omit it rather than report 0.
            channel = v.trim().parse().ok().filter(|&c| c != 0);
        } else if line.starts_with("wpa=") || line.starts_with("wpa_key_mgmt=") {
            has_wpa = true;
        }
    }
    if ssid.is_empty() {
        eprintln!("[wifi_handoff] {path} names no SSID — refusing to describe an AP that is not running");
        return None;
    }
    let (security_type, passphrase) = if has_wpa {
        // Secured AP: advertise WPA2/WPA3 and hand over the passphrase if we have one. A secured AP without
        // a usable passphrase (wpa_psk-only) is still advertised as secured rather than open (audit B5).
        (
            Some(AccessoryWiFiSecurityType::Wpa2OrWpa3Personal),
            if passphrase.is_empty() { None } else { Some(passphrase) },
        )
    } else {
        (Some(AccessoryWiFiSecurityType::None), None)
    };
    Some(AccessoryWiFiConfig {
        ssid,
        passphrase,
        security_type,
        channel,
    })
}

// ---- shared TLV helpers ----------------------------------------------------------------------

/// utf8 value decode: the wire form is NUL-terminated (see module docs); drop a single trailing NUL
/// if present and lossily decode. Kept tolerant since the device is the encoder here.
fn decode_str(value: &[u8]) -> String {
    let end = value.iter().position(|&b| b == 0).unwrap_or(value.len());
    String::from_utf8_lossy(&value[..end]).into_owned()
}

/// Iterator over TLV groups in a message body: yields `(param_id, value)` for each
/// `[group_len BE16][param_id BE16][value…]`. Stops on the first malformed/truncated group.
struct TlvGroups<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> TlvGroups<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
}

impl<'a> Iterator for TlvGroups<'a> {
    type Item = (u16, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let rest = self.buf.get(self.pos..)?;
        if rest.len() < 4 {
            return None;
        }
        let group_len = ((rest[0] as usize) << 8) | rest[1] as usize;
        if group_len < 4 || group_len > rest.len() {
            return None;
        }
        let pid = ((rest[2] as u16) << 8) | rest[3] as u16;
        let value = &rest[4..group_len];
        self.pos += group_len;
        Some((pid, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_wifi_information_is_empty_body() {
        // 0x5700 has 0 params.
        assert!(build_request_wifi_information().is_empty());
    }

    #[test]
    fn build_accessory_wifi_config_full_byte_layout() {
        let cfg = AccessoryWiFiConfig {
            ssid: "CarLink-AP".to_string(),
            passphrase: Some("secretpw".to_string()),
            security_type: Some(AccessoryWiFiSecurityType::Wpa2OrWpa3Personal),
            channel: Some(44),
        };
        let body = build_accessory_wifi_configuration_information(&cfg);

        // Expected: four groups in id order 1,2,3,4.
        // SSID (pid 1): NUL-terminated "CarLink-AP" (10+1=11 value bytes) -> group_len = 4+11 = 15.
        let mut expected = Vec::new();
        expected.extend_from_slice(&[0x00, 15, 0x00, accessory_wifi_param::WIFI_SSID as u8]);
        expected.extend_from_slice(b"CarLink-AP\0");
        // Passphrase (pid 2): "secretpw" (8+1=9) -> group_len = 13.
        expected.extend_from_slice(&[0x00, 13, 0x00, accessory_wifi_param::PASSPHRASE as u8]);
        expected.extend_from_slice(b"secretpw\0");
        // SecurityType (pid 3): u8 value 2 -> group_len = 5.
        expected.extend_from_slice(&[0x00, 5, 0x00, accessory_wifi_param::SECURITY_TYPE as u8, 2]);
        // Channel (pid 4): u8 value 44 -> group_len = 5.
        expected.extend_from_slice(&[0x00, 5, 0x00, accessory_wifi_param::CHANNEL as u8, 44]);

        assert_eq!(body, expected);
    }

    #[test]
    fn build_accessory_wifi_config_omits_optional_fields() {
        // Open network: only the required SSID (pid 1) is present.
        let cfg = AccessoryWiFiConfig {
            ssid: "Open".to_string(),
            passphrase: None,
            security_type: None,
            channel: None,
        };
        let body = build_accessory_wifi_configuration_information(&cfg);
        let mut expected = Vec::new();
        expected.extend_from_slice(&[0x00, 9, 0x00, accessory_wifi_param::WIFI_SSID as u8]);
        expected.extend_from_slice(b"Open\0");
        assert_eq!(body, expected);
    }

    #[test]
    fn accessory_wifi_config_roundtrips_through_the_group_iterator() {
        let cfg = AccessoryWiFiConfig {
            ssid: "Head-Unit".to_string(),
            passphrase: Some("pw".to_string()),
            security_type: Some(AccessoryWiFiSecurityType::Wep),
            channel: Some(6),
        };
        let body = build_accessory_wifi_configuration_information(&cfg);
        let groups: Vec<(u16, &[u8])> = TlvGroups::new(&body).collect();
        assert_eq!(groups.len(), 4);
        assert_eq!(groups[0].0, accessory_wifi_param::WIFI_SSID);
        assert_eq!(decode_str(groups[0].1), "Head-Unit");
        assert_eq!(groups[1].0, accessory_wifi_param::PASSPHRASE);
        assert_eq!(decode_str(groups[1].1), "pw");
        assert_eq!(groups[2].0, accessory_wifi_param::SECURITY_TYPE);
        assert_eq!(groups[2].1, &[AccessoryWiFiSecurityType::Wep as u8]);
        assert_eq!(groups[3].0, accessory_wifi_param::CHANNEL);
        assert_eq!(groups[3].1, &[6u8]);
    }

    #[test]
    fn parse_wifi_information_full() {
        // Build a device->accessory 0x5701 body by hand and parse it.
        // RequestStatus(pid 0)=Success(0), SecurityType(pid 1)=WPA2(2),
        // WiFiSSID(pid 2)="Net", WiFiPassphrase(pid 3)="pass".
        let mut body = Vec::new();
        body.extend_from_slice(&[0x00, 5, 0x00, wifi_info_param::REQUEST_STATUS as u8, 0]);
        body.extend_from_slice(&[0x00, 5, 0x00, wifi_info_param::SECURITY_TYPE as u8, 2]);
        body.extend_from_slice(&[0x00, 8, 0x00, wifi_info_param::WIFI_SSID as u8]);
        body.extend_from_slice(b"Net\0");
        body.extend_from_slice(&[0x00, 9, 0x00, wifi_info_param::WIFI_PASSPHRASE as u8]);
        body.extend_from_slice(b"pass\0");

        let info = parse_wifi_information(&body);
        assert_eq!(info.request_status, Some(WiFiRequestStatus::Success));
        assert_eq!(info.security_type, Some(DeviceWiFiSecurityType::Wpa2));
        assert_eq!(info.ssid.as_deref(), Some("Net"));
        assert_eq!(info.passphrase.as_deref(), Some("pass"));
    }

    #[test]
    fn parse_wifi_information_declined_only_status() {
        // UserDeclined: only RequestStatus present, no network info.
        let body = [0x00, 5, 0x00, wifi_info_param::REQUEST_STATUS as u8, 1];
        let info = parse_wifi_information(&body);
        assert_eq!(info.request_status, Some(WiFiRequestStatus::UserDeclined));
        assert_eq!(info.security_type, None);
        assert_eq!(info.ssid, None);
        assert_eq!(info.passphrase, None);
    }

    #[test]
    fn parse_wifi_information_ignores_unknown_param_and_survives_truncation() {
        // Unknown pid 99 group, then a valid status; then a truncated trailing group.
        let mut body = Vec::new();
        body.extend_from_slice(&[0x00, 6, 0x00, 99, 0xAB, 0xCD]);
        body.extend_from_slice(&[0x00, 5, 0x00, wifi_info_param::REQUEST_STATUS as u8, 2]);
        body.extend_from_slice(&[0x00, 20, 0x00, 2]); // group_len 20 > remaining -> stop
        let info = parse_wifi_information(&body);
        assert_eq!(
            info.request_status,
            Some(WiFiRequestStatus::NetworkInformationUnavailable)
        );
        assert_eq!(info.ssid, None);
    }

    #[test]
    fn request_accessory_wifi_configuration_is_empty() {
        assert!(is_request_accessory_wifi_configuration(&[]));
        assert!(!is_request_accessory_wifi_configuration(&[0x00]));
    }

    #[test]
    fn security_type_enums_are_distinct_tables() {
        // The two "SecurityType" enums must not be conflated: value 2 means WPA2 in 0x5701 but
        // WPA2/WPA3-Personal in 0x5703; value 0 is WEP in 0x5701 but None in 0x5703.
        assert_eq!(
            DeviceWiFiSecurityType::from_u8(0),
            Some(DeviceWiFiSecurityType::Wep)
        );
        assert_eq!(
            AccessoryWiFiSecurityType::from_u8(0),
            Some(AccessoryWiFiSecurityType::None)
        );
        assert_eq!(
            DeviceWiFiSecurityType::from_u8(3),
            Some(DeviceWiFiSecurityType::MixedWpaWpa2)
        );
        assert_eq!(AccessoryWiFiSecurityType::from_u8(3), None); // 0x5703 has no value 3
    }
}
