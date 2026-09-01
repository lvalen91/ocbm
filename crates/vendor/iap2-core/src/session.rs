//! iap2/session.rs — the CarPlay session-start handshake pair: the `CarPlayStartSession` (0x4301)
//! body BUILDER and the `CarPlayAvailability` (0x4300) body PARSER.
//!
//! These are the two messages Apple defines for negotiating a CarPlay session start over iAP2 once
//! identify + auth complete. Names/ids/types are Apple-authoritative (decoded from
//! `CarPlaySimulator.devicekitplugin`, `iap2messages.json`):
//!
//! - `CarPlayAvailability` (0x4300, source=Device): the iPhone tells the accessory which CarPlay
//!   bearers are available (Wired / Wireless / ThemeAssets), each an `Available` bool plus the
//!   transport identifier the accessory advertised in its IdentificationInformation transport
//!   component. We PARSE this.
//! - `CarPlayStartSession` (0x4301, source=Accessory): the accessory asks the device to start a
//!   CarPlay session on a specific bearer, carrying the WirelessAttributes (SSID/passphrase/channel/
//!   IP/security), Port, DeviceIdentifier, PublicKey, versions, AssetInformation, and the
//!   SupportsMutualAuthentication flag. We BUILD this.
//!
//! ⚠️ CRITICAL — the WIRED path must NOT send `CarPlayStartSession` (0x4301). It is a device-verified
//! DEAD END: declaring/sending the wired 0x4301 pair traps iOS in a handshake carplayd never
//! completes and NCM never comes up — the wired CarPlay bearer is brought up DECLARATIVELY by the
//! USBHostTransportComponent's `TransportSupportsCarPlay` flag instead (see `message.rs` module
//! header + docs/carplay/02_SESSION_LIFECYCLE.md, device-verified 2026-07-05). This builder therefore exists for the WIRELESS
//! session-start path and for future use; it is NOT called on the wired path.
//!
//! Param ids below are Apple's; they are not (yet) mirrored in `spec.rs` (which only carries the
//! IdentificationInformation / NowPlaying / RouteGuidance / CallState param submodules) — see the
//! coordinator request in the round-2 report.

use crate::message::Tlv;

/// Parameter ids of `CarPlayStartSession` (0x4301, source=Accessory). Apple's authoritative names.
pub mod start_session {
    /// WiredAttributes — `group`.
    pub const WIRED_ATTRIBUTES: u16 = 0;
    /// WirelessAttributes — `group`.
    pub const WIRELESS_ATTRIBUTES: u16 = 1;
    /// Port — `uint32`.
    pub const PORT: u16 = 2;
    /// DeviceIdentifier — `utf8`.
    pub const DEVICE_IDENTIFIER: u16 = 3;
    /// PublicKey — `utf8`.
    pub const PUBLIC_KEY: u16 = 4;
    /// SourceVersion — `utf8`.
    pub const SOURCE_VERSION: u16 = 5;
    /// CarPlaySDKVersion — `utf8`.
    pub const CAR_PLAY_SDK_VERSION: u16 = 6;
    /// AssetInformation — `group`.
    pub const ASSET_INFORMATION: u16 = 7;
    /// SupportsMutualAuthentication — `bool`.
    pub const SUPPORTS_MUTUAL_AUTHENTICATION: u16 = 8;

    /// Sub-parameter ids of WiredAttributes (param 0).
    pub mod wired {
        /// IPAddress — `utf8` (accessory link-local IPv6 for the wired interface, no zone index).
        pub const IP_ADDRESS: u16 = 0;
    }
    /// Sub-parameter ids of WirelessAttributes (param 1).
    pub mod wireless {
        /// WiFiSSID — `utf8`.
        pub const WIFI_SSID: u16 = 0;
        /// Passphrase — `utf8`.
        pub const PASSPHRASE: u16 = 1;
        /// Channel — `uint8` (primary Wi-Fi channel for 40/80MHz, operating channel for 20MHz).
        pub const CHANNEL: u16 = 2;
        /// IPAddress — `utf8` (accessory link-local IPv6 for the Wi-Fi interface, no zone index).
        pub const IP_ADDRESS: u16 = 3;
        /// SecurityType — `enum` (`CarPlayStartSessionWiFiSecurityType`).
        pub const SECURITY_TYPE: u16 = 4;
        /// SecurityType::WPA3 Personal Transition Mode (values 0..=2 are Reserved).
        pub const SECURITY_TYPE_WPA3_PERSONAL_TRANSITION: u8 = 3;
        /// SecurityType::WPA3 Personal Only.
        pub const SECURITY_TYPE_WPA3_PERSONAL_ONLY: u8 = 4;
    }
    /// Sub-parameter ids of AssetInformation (param 7).
    pub mod asset {
        /// AssetIdentifier — `utf8`.
        pub const ASSET_IDENTIFIER: u16 = 0;
        /// AssetVersion — `int32`.
        pub const ASSET_VERSION: u16 = 1;
    }
}

/// Parameter ids of `CarPlayAvailability` (0x4300, source=Device). Apple's authoritative names.
pub mod availability {
    /// WiredAttributes — `group`.
    pub const WIRED_ATTRIBUTES: u16 = 0;
    /// WirelessAttributes — `group`.
    pub const WIRELESS_ATTRIBUTES: u16 = 1;
    /// ThemeAssetsAttributes — `group`.
    pub const THEME_ASSETS_ATTRIBUTES: u16 = 2;

    /// Sub-parameter ids of WiredAttributes (param 0).
    pub mod wired {
        /// Available — `bool`.
        pub const AVAILABLE: u16 = 0;
        /// USBTransportIdentifier — `utf8`.
        pub const USB_TRANSPORT_IDENTIFIER: u16 = 1;
    }
    /// Sub-parameter ids of WirelessAttributes (param 1).
    pub mod wireless {
        /// Available — `bool`.
        pub const AVAILABLE: u16 = 0;
        /// BluetoothTransportIdentifier — `utf8`.
        pub const BLUETOOTH_TRANSPORT_IDENTIFIER: u16 = 1;
    }
    /// Sub-parameter ids of ThemeAssetsAttributes (param 2).
    pub mod theme_assets {
        /// Available — `bool`.
        pub const AVAILABLE: u16 = 0;
    }
}

// ============================================================================================
// CarPlayStartSession (0x4301) BUILDER — WIRELESS session-start / future use only (see header).
// ============================================================================================

/// WirelessAttributes group (`CarPlayStartSession` param 1) — the Wi-Fi bearer the accessory offers.
#[derive(Debug, Clone)]
pub struct WirelessAttributes<'a> {
    /// WiFiSSID (sub 0).
    pub wifi_ssid: &'a str,
    /// Passphrase (sub 1).
    pub passphrase: &'a str,
    /// Channel (sub 2) — primary/operating Wi-Fi channel.
    pub channel: u8,
    /// IPAddress (sub 3) — accessory link-local IPv6 for the Wi-Fi interface (no zone index).
    pub ip_address: &'a str,
    /// SecurityType (sub 4) — one of `start_session::wireless::SECURITY_TYPE_*`.
    pub security_type: u8,
}

/// AssetInformation group (`CarPlayStartSession` param 7).
#[derive(Debug, Clone)]
pub struct AssetInformation<'a> {
    /// AssetIdentifier (sub 0).
    pub asset_identifier: &'a str,
    /// AssetVersion (sub 1).
    pub asset_version: i32,
}

/// A fully specified `CarPlayStartSession` (0x4301). Per Apple's iAP2 spec, **Port (param 2) is
/// REQUIRED** and the remaining fields are optional (#140 — the previous "every field is optional" was
/// wrong; a session-start without Port is malformed). Set only the optional fields the chosen bearer
/// needs. `wired_ip_address` populates the WiredAttributes group (param 0) and `wireless` the
/// WirelessAttributes group (param 1) — for a wireless session-start, set `wireless` and leave
/// `wired_ip_address` `None`.
#[derive(Debug, Clone, Default)]
pub struct CarPlayStartSession<'a> {
    /// WiredAttributes.IPAddress (param 0 → sub 0), if declaring a wired bearer.
    pub wired_ip_address: Option<&'a str>,
    /// WirelessAttributes (param 1), if declaring a wireless bearer.
    pub wireless: Option<WirelessAttributes<'a>>,
    /// Port (param 2).
    pub port: Option<u32>,
    /// DeviceIdentifier (param 3).
    pub device_identifier: Option<&'a str>,
    /// PublicKey (param 4).
    pub public_key: Option<&'a str>,
    /// SourceVersion (param 5).
    pub source_version: Option<&'a str>,
    /// CarPlaySDKVersion (param 6).
    pub car_play_sdk_version: Option<&'a str>,
    /// AssetInformation (param 7).
    pub asset: Option<AssetInformation<'a>>,
    /// SupportsMutualAuthentication (param 8).
    pub supports_mutual_authentication: Option<bool>,
}

impl CarPlayStartSession<'_> {
    /// Serialize to the `CarPlayStartSession` (0x4301) body. Params are emitted in ascending id
    /// order; absent options are skipped.
    ///
    /// ⚠️ Do NOT send this on the wired path (device-verified dead end — see module header).
    pub fn build(&self) -> Vec<u8> {
        use start_session as p;
        // NOTE (#140): Port (param 2) is REQUIRED per Apple's spec; a real session-start MUST set it.
        // We don't hard-assert here — `build()` is also used to serialize partial/empty structs in unit
        // tests, and 0x4301 isn't sent on the live wireless flow (which hands off via 0x5703) — but the
        // struct doc records the requirement so any future sender includes Port.
        let mut body = Tlv::new();

        if let Some(ip) = self.wired_ip_address {
            let mut g = Tlv::new();
            g.str(p::wired::IP_ADDRESS, ip); // sub 0
            body.bytes(p::WIRED_ATTRIBUTES, &g.0); // param 0
        }
        if let Some(w) = &self.wireless {
            let mut g = Tlv::new();
            g.str(p::wireless::WIFI_SSID, w.wifi_ssid); // sub 0
            g.str(p::wireless::PASSPHRASE, w.passphrase); // sub 1
            g.u8v(p::wireless::CHANNEL, w.channel); // sub 2
            g.str(p::wireless::IP_ADDRESS, w.ip_address); // sub 3
            g.u8v(p::wireless::SECURITY_TYPE, w.security_type); // sub 4 (enum → 1 byte)
            body.bytes(p::WIRELESS_ATTRIBUTES, &g.0); // param 1
        }
        if let Some(port) = self.port {
            body.u32v(p::PORT, port); // param 2
        }
        if let Some(v) = self.device_identifier {
            body.str(p::DEVICE_IDENTIFIER, v); // param 3
        }
        if let Some(v) = self.public_key {
            body.str(p::PUBLIC_KEY, v); // param 4
        }
        if let Some(v) = self.source_version {
            body.str(p::SOURCE_VERSION, v); // param 5
        }
        if let Some(v) = self.car_play_sdk_version {
            body.str(p::CAR_PLAY_SDK_VERSION, v); // param 6
        }
        if let Some(a) = &self.asset {
            let mut g = Tlv::new();
            g.str(p::asset::ASSET_IDENTIFIER, a.asset_identifier); // sub 0
            g.i32v(p::asset::ASSET_VERSION, a.asset_version); // sub 1
            body.bytes(p::ASSET_INFORMATION, &g.0); // param 7
        }
        if let Some(b) = self.supports_mutual_authentication {
            body.boolv(p::SUPPORTS_MUTUAL_AUTHENTICATION, b); // param 8
        }
        body.0
    }
}

// ============================================================================================
// CarPlayAvailability (0x4300) PARSER.
// ============================================================================================

/// One CarPlay bearer's availability as reported by the device (`Available` + its transport id).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BearerAvailability {
    /// The `Available` bool (sub 0). `None` if the device omitted it.
    pub available: Option<bool>,
    /// The transport identifier string (WiredAttributes.USBTransportIdentifier /
    /// WirelessAttributes.BluetoothTransportIdentifier). `None` for the wired/wireless groups if
    /// omitted; always `None` for the theme-assets group (which carries no identifier).
    pub transport_identifier: Option<String>,
}

/// Parsed `CarPlayAvailability` (0x4300) body. Each group is present only if the device sent it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CarPlayAvailability {
    /// WiredAttributes (param 0).
    pub wired: Option<BearerAvailability>,
    /// WirelessAttributes (param 1).
    pub wireless: Option<BearerAvailability>,
    /// ThemeAssetsAttributes (param 2) — only an `Available` bool, no transport identifier.
    pub theme_assets: Option<BearerAvailability>,
}

/// Failure reason for `parse_carplay_availability`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// A TLV group's declared length is < 4 or overruns the buffer at the given offset.
    Truncated(usize),
}

/// Walk a flat TLV buffer (`[group_len BE16][pid BE16][value(group_len-4)]`…) yielding
/// `(param_id, value)` pairs. Returns `Err` on a malformed length.
fn walk_tlvs(buf: &[u8]) -> Result<Vec<(u16, &[u8])>, ParseError> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= buf.len() {
        let glen = u16::from_be_bytes([buf[i], buf[i + 1]]) as usize;
        let pid = u16::from_be_bytes([buf[i + 2], buf[i + 3]]);
        if glen < 4 || i + glen > buf.len() {
            return Err(ParseError::Truncated(i));
        }
        out.push((pid, &buf[i + 4..i + glen]));
        i += glen;
    }
    Ok(out)
}

/// Decode an iAP2 `bool` value (single byte, non-zero = true). Empty value decodes as `None`.
fn decode_bool(v: &[u8]) -> Option<bool> {
    v.first().map(|&b| b != 0)
}

/// Decode an iAP2 NUL-terminated `utf8` value into an owned `String` (lossy on invalid UTF-8),
/// stripping the trailing NUL if present.
fn decode_str(v: &[u8]) -> String {
    let end = v.iter().position(|&b| b == 0).unwrap_or(v.len());
    String::from_utf8_lossy(&v[..end]).into_owned()
}

/// Parse a `CarPlayAvailability` (0x4300) body. Unknown top-level params and unknown sub-params are
/// ignored (forward-compatible); a malformed TLV length is a hard error.
pub fn parse_carplay_availability(body: &[u8]) -> Result<CarPlayAvailability, ParseError> {
    let mut out = CarPlayAvailability::default();
    for (pid, value) in walk_tlvs(body)? {
        match pid {
            availability::WIRED_ATTRIBUTES => {
                let mut b = BearerAvailability::default();
                for (sub, sv) in walk_tlvs(value)? {
                    match sub {
                        availability::wired::AVAILABLE => b.available = decode_bool(sv),
                        availability::wired::USB_TRANSPORT_IDENTIFIER => {
                            b.transport_identifier = Some(decode_str(sv))
                        }
                        _ => {}
                    }
                }
                out.wired = Some(b);
            }
            availability::WIRELESS_ATTRIBUTES => {
                let mut b = BearerAvailability::default();
                for (sub, sv) in walk_tlvs(value)? {
                    match sub {
                        availability::wireless::AVAILABLE => b.available = decode_bool(sv),
                        availability::wireless::BLUETOOTH_TRANSPORT_IDENTIFIER => {
                            b.transport_identifier = Some(decode_str(sv))
                        }
                        _ => {}
                    }
                }
                out.wireless = Some(b);
            }
            availability::THEME_ASSETS_ATTRIBUTES => {
                let mut b = BearerAvailability::default();
                for (sub, sv) in walk_tlvs(value)? {
                    if sub == availability::theme_assets::AVAILABLE {
                        b.available = decode_bool(sv);
                    }
                }
                out.theme_assets = Some(b);
            }
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_session_wireless_body_byte_layout() {
        let msg = CarPlayStartSession {
            wireless: Some(WirelessAttributes {
                wifi_ssid: "AP",
                passphrase: "pw",
                channel: 36,
                ip_address: "fe80::1",
                security_type: start_session::wireless::SECURITY_TYPE_WPA3_PERSONAL_ONLY,
            }),
            port: Some(7000),
            device_identifier: Some("dev"),
            supports_mutual_authentication: Some(true),
            ..Default::default()
        };
        let body = msg.build();

        // Expected, param by param (ascending id).
        let mut exp = Vec::new();

        // param 1 WirelessAttributes group.
        let mut g = Vec::new();
        g.extend_from_slice(&[0x00, 0x07, 0x00, 0x00, b'A', b'P', 0x00]); // str(0,"AP")
        g.extend_from_slice(&[0x00, 0x07, 0x00, 0x01, b'p', b'w', 0x00]); // str(1,"pw")
        g.extend_from_slice(&[0x00, 0x05, 0x00, 0x02, 36]); // u8v(2,36)
        g.extend_from_slice(&[0x00, 0x0C, 0x00, 0x03]); // str(3,"fe80::1"): len 4+8=12
        g.extend_from_slice(b"fe80::1\0");
        g.extend_from_slice(&[0x00, 0x05, 0x00, 0x04, 0x04]); // u8v(4, WPA3 Only=4)
        exp.extend_from_slice(&[0x00, (4 + g.len()) as u8, 0x00, 0x01]); // group hdr, param 1
        exp.extend_from_slice(&g);

        // param 2 Port uint32 = 7000 (0x00001B58).
        exp.extend_from_slice(&[0x00, 0x08, 0x00, 0x02, 0x00, 0x00, 0x1B, 0x58]);
        // param 3 DeviceIdentifier "dev".
        exp.extend_from_slice(&[0x00, 0x08, 0x00, 0x03, b'd', b'e', b'v', 0x00]);
        // param 8 SupportsMutualAuthentication = true.
        exp.extend_from_slice(&[0x00, 0x05, 0x00, 0x08, 0x01]);

        assert_eq!(body, exp);
    }

    #[test]
    fn start_session_with_wired_and_asset_groups() {
        let msg = CarPlayStartSession {
            wired_ip_address: Some("fe80::2"),
            asset: Some(AssetInformation { asset_identifier: "a", asset_version: -1 }),
            ..Default::default()
        };
        let body = msg.build();

        let mut exp = Vec::new();
        // param 0 WiredAttributes { IPAddress "fe80::2" }.
        let mut g0 = Vec::new();
        g0.extend_from_slice(&[0x00, 0x0C, 0x00, 0x00]); // str(0,"fe80::2") len 12
        g0.extend_from_slice(b"fe80::2\0");
        exp.extend_from_slice(&[0x00, (4 + g0.len()) as u8, 0x00, 0x00]);
        exp.extend_from_slice(&g0);
        // param 7 AssetInformation { AssetIdentifier "a", AssetVersion -1 }.
        let mut g7 = Vec::new();
        g7.extend_from_slice(&[0x00, 0x06, 0x00, 0x00, b'a', 0x00]); // str(0,"a") len 6
        g7.extend_from_slice(&[0x00, 0x08, 0x00, 0x01, 0xFF, 0xFF, 0xFF, 0xFF]); // i32(1,-1)
        exp.extend_from_slice(&[0x00, (4 + g7.len()) as u8, 0x00, 0x07]);
        exp.extend_from_slice(&g7);

        assert_eq!(body, exp);
    }

    #[test]
    fn empty_start_session_is_empty_body() {
        assert!(CarPlayStartSession::default().build().is_empty());
    }

    #[test]
    fn availability_roundtrip_all_bearers() {
        // Hand-build a 0x4300 body: wired{available=true, usbId="USB0"},
        // wireless{available=false, btId="BT1"}, themeAssets{available=true}.
        let mut body = Tlv::new();
        {
            let mut g = Tlv::new();
            g.boolv(availability::wired::AVAILABLE, true);
            g.str(availability::wired::USB_TRANSPORT_IDENTIFIER, "USB0");
            body.bytes(availability::WIRED_ATTRIBUTES, &g.0);
        }
        {
            let mut g = Tlv::new();
            g.boolv(availability::wireless::AVAILABLE, false);
            g.str(availability::wireless::BLUETOOTH_TRANSPORT_IDENTIFIER, "BT1");
            body.bytes(availability::WIRELESS_ATTRIBUTES, &g.0);
        }
        {
            let mut g = Tlv::new();
            g.boolv(availability::theme_assets::AVAILABLE, true);
            body.bytes(availability::THEME_ASSETS_ATTRIBUTES, &g.0);
        }

        let parsed = parse_carplay_availability(&body.0).expect("well-formed body parses");
        assert_eq!(
            parsed.wired,
            Some(BearerAvailability {
                available: Some(true),
                transport_identifier: Some("USB0".to_string()),
            })
        );
        assert_eq!(
            parsed.wireless,
            Some(BearerAvailability {
                available: Some(false),
                transport_identifier: Some("BT1".to_string()),
            })
        );
        assert_eq!(
            parsed.theme_assets,
            Some(BearerAvailability { available: Some(true), transport_identifier: None })
        );
    }

    #[test]
    fn availability_absent_groups_are_none() {
        // Only the wireless group present.
        let mut body = Tlv::new();
        let mut g = Tlv::new();
        g.boolv(availability::wireless::AVAILABLE, true);
        body.bytes(availability::WIRELESS_ATTRIBUTES, &g.0);

        let parsed = parse_carplay_availability(&body.0).unwrap();
        assert!(parsed.wired.is_none());
        assert!(parsed.theme_assets.is_none());
        assert_eq!(parsed.wireless.unwrap().available, Some(true));
    }

    #[test]
    fn availability_truncated_length_is_error() {
        // group_len says 0x0020 but only a few bytes follow.
        let bad = [0x00, 0x20, 0x00, 0x00, 0x01];
        assert!(matches!(
            parse_carplay_availability(&bad),
            Err(ParseError::Truncated(0))
        ));
    }

    #[test]
    fn availability_ignores_unknown_params() {
        let mut body = Tlv::new();
        body.u16v(0x00FF, 0x1234); // unknown top-level param
        let parsed = parse_carplay_availability(&body.0).unwrap();
        assert_eq!(parsed, CarPlayAvailability::default());
    }
}
