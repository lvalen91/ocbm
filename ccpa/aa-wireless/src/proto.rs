//! Proto2 wire encoding for the wireless-AA Bluetooth bootstrap messages.
//!
//! Spec and provenance: `docs/androidauto/03_WIRELESS.md` §2d/§2e. The field numbers, enum values
//! and message shapes are protocol FACTS recovered from the reference stacks; none of their code is
//! used here (they are GPLv3, this repo is not — see that document's §7).
//!
//! Hand-rolled rather than generated. The whole bootstrap is seven messages carrying strings, two
//! `uint32`s and two enums; a protobuf runtime plus a build-time `protoc` would cost more rootfs
//! than the box has to spare (`docs/ops/00_BUILD_AND_DEPLOY.md`: ~3.4 MB free) to encode what fits
//! in this file. It also keeps the daemon free of a build-host toolchain dependency.
//!
//! THE TRAP THIS FILE EXISTS TO GET RIGHT: `Status` is a proto2 enum whose useful values are
//! NEGATIVE. Protobuf encodes a negative enum/int32 as its *sign-extended int64*, i.e. a 10-byte
//! varint -- not a 1-byte one, and not zigzag (that is `sint32`, which this is not). A decoder that
//! reads a varint into a `u32`, or that stops at 5 bytes, silently mis-reads every error the phone
//! can report. Since the negatives ARE the diagnostic surface, that bug would present as "the
//! bootstrap fails and the status says SUCCESS".

/// Protobuf wire types we use. Everything here is a varint or a length-delimited field.
const WIRE_VARINT: u32 = 0;
const WIRE_LEN: u32 = 2;

// ---------------------------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------------------------

/// Append a base-128 varint, low 7 bits first, continuation bit set on every byte but the last.
fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Append a field tag: `(field_number << 3) | wire_type`.
fn put_tag(out: &mut Vec<u8>, field: u32, wire: u32) {
    put_varint(out, ((field << 3) | wire) as u64);
}

/// Append a `uint32` field.
fn put_u32(out: &mut Vec<u8>, field: u32, v: u32) {
    put_tag(out, field, WIRE_VARINT);
    put_varint(out, v as u64);
}

/// Append an `int32`/enum field, sign-extending negatives to 10 bytes exactly as protobuf does.
fn put_i32(out: &mut Vec<u8>, field: u32, v: i32) {
    put_tag(out, field, WIRE_VARINT);
    put_varint(out, v as i64 as u64);
}

/// Append a length-delimited `string` field.
fn put_str(out: &mut Vec<u8>, field: u32, v: &str) {
    put_tag(out, field, WIRE_LEN);
    put_varint(out, v.len() as u64);
    out.extend_from_slice(v.as_bytes());
}

// ---------------------------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------------------------

/// A cursor over one protobuf message.
struct Decoder<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Decoder { buf, pos: 0 }
    }

    fn done(&self) -> bool {
        self.pos >= self.buf.len()
    }

    /// Read a varint as `u64`. Bounded at 10 bytes, which is the maximum a 64-bit varint can take
    /// and exactly what a sign-extended negative occupies.
    ///
    /// The shift reaches 63 on the tenth byte and no further, so it is always a legal `u64` shift —
    /// there is no overflow here. A well-formed sign-extended negative puts only bit 63 in that
    /// byte (0x01). A MALFORMED tenth byte with higher bits set has them discarded rather than
    /// rejected, which is what protobuf implementations do generally; the message is garbage either
    /// way, and the `Status` it decodes to will not match a known value, so `name()` reports
    /// `STATUS_UNKNOWN` rather than a plausible-looking lie.
    fn varint(&mut self) -> Option<u64> {
        let mut result: u64 = 0;
        for shift in 0..10 {
            let byte = *self.buf.get(self.pos)?;
            self.pos += 1;
            result |= ((byte & 0x7f) as u64) << (shift * 7);
            if byte & 0x80 == 0 {
                return Some(result);
            }
        }
        None // 11th continuation byte: malformed
    }

    /// Read a length-delimited field as a borrowed slice.
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = self.varint()? as usize;
        let end = self.pos.checked_add(len)?;
        let out = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(out)
    }

    /// Skip a field we do not model, so an unknown field never desynchronises the parse. Both
    /// references log-and-continue on unknown fields; a bootstrap that hard-failed on one would be
    /// brittle against a gearhead update.
    fn skip(&mut self, wire: u32) -> Option<()> {
        match wire {
            WIRE_VARINT => {
                self.varint()?;
            }
            WIRE_LEN => {
                self.bytes()?;
            }
            5 => self.pos = self.pos.checked_add(4)?, // fixed32
            1 => self.pos = self.pos.checked_add(8)?, // fixed64
            _ => return None,                          // groups: not used by these messages
        }
        Some(())
    }

    /// Read the next `(field_number, wire_type)` tag.
    fn tag(&mut self) -> Option<(u32, u32)> {
        let key = self.varint()?;
        let field = (key >> 3) as u32;
        let wire = (key & 0x7) as u32;
        if field == 0 {
            return None; // field 0 is invalid; treat as malformed rather than loop forever
        }
        Some((field, wire))
    }
}

// ---------------------------------------------------------------------------------------------
// Enums (docs/androidauto/03_WIRELESS.md §2e)
// ---------------------------------------------------------------------------------------------

/// Bootstrap status. SUCCESS is 0 and UNSOLICITED_MESSAGE is 1; every other value is negative and
/// names an actual failure. Always log `Status::name()`, never the bare number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status(pub i32);

// The full enum is carried deliberately: these are the protocol's vocabulary, and the negatives are
// the only diagnostic surface the bootstrap has. Keeping the set complete is what lets `name()` turn
// any status the phone sends into something a bench log can be read against, including the ones this
// daemon does not yet produce a code path for.
#[allow(dead_code)]
impl Status {
    pub const SUCCESS: Status = Status(0);
    pub const UNSOLICITED_MESSAGE: Status = Status(1);
    pub const NO_COMPATIBLE_VERSION: Status = Status(-1);
    pub const WIFI_INACCESSIBLE_CHANNEL: Status = Status(-2);
    pub const WIFI_INCORRECT_CREDENTIALS: Status = Status(-3);
    pub const PROJECTION_ALREADY_STARTED: Status = Status(-4);
    pub const WIFI_DISABLED: Status = Status(-5);
    pub const WIFI_NOT_YET_STARTED: Status = Status(-6);
    pub const INVALID_HOST: Status = Status(-7);
    pub const NO_SUPPORTED_WIFI_CHANNELS: Status = Status(-8);
    pub const INSTRUCT_USER_TO_CHECK_THE_PHONE: Status = Status(-9);
    pub const PHONE_WIFI_DISABLED: Status = Status(-10);
    pub const WIFI_NETWORK_UNAVAILABLE: Status = Status(-11);

    pub fn is_success(self) -> bool {
        self.0 == 0
    }

    pub fn name(self) -> &'static str {
        match self.0 {
            0 => "STATUS_SUCCESS",
            1 => "STATUS_UNSOLICITED_MESSAGE",
            -1 => "STATUS_NO_COMPATIBLE_VERSION",
            -2 => "STATUS_WIFI_INACCESSIBLE_CHANNEL",
            -3 => "STATUS_WIFI_INCORRECT_CREDENTIALS",
            -4 => "STATUS_PROJECTION_ALREADY_STARTED",
            -5 => "STATUS_WIFI_DISABLED",
            -6 => "STATUS_WIFI_NOT_YET_STARTED",
            -7 => "STATUS_INVALID_HOST",
            -8 => "STATUS_NO_SUPPORTED_WIFI_CHANNELS",
            -9 => "STATUS_INSTRUCT_USER_TO_CHECK_THE_PHONE",
            -10 => "STATUS_PHONE_WIFI_DISABLED",
            -11 => "STATUS_WIFI_NETWORK_UNAVAILABLE",
            _ => "STATUS_UNKNOWN",
        }
    }
}

/// AP security mode, as `WifiInfoResponse.security_mode` declares it.
///
/// Table per docs/androidauto/03_WIRELESS.md, the phone-validated values embedded in
/// aa-proxy-rs's `WifiInfoResponse.proto` (field 4): `8 = WPA2_PERSONAL`, confirmed both by the
/// stock CCPA's own captured wireless-AA session (`securityMode: 8`) and by the field result —
/// the earlier `24` (WPA2_ENTERPRISE) attempt never associated, consistent with the phone
/// attempting 802.1X negotiation for an enterprise mode against credentials that have no RADIUS
/// backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecurityMode(pub i32);

// Complete so every legal value has a name to try if the phone rejects our credentials.
#[allow(dead_code)]
impl SecurityMode {
    pub const UNKNOWN: SecurityMode = SecurityMode(0);
    pub const OPEN: SecurityMode = SecurityMode(1);
    pub const WEP_64: SecurityMode = SecurityMode(2);
    pub const WEP_128: SecurityMode = SecurityMode(3);
    pub const WPA_PERSONAL: SecurityMode = SecurityMode(4);
    /// FIELD-PROVEN wire value for a WPA2-PSK AP — see the module doc above.
    pub const WPA2_PERSONAL: SecurityMode = SecurityMode(8);
    pub const WPA_WPA2_PERSONAL: SecurityMode = SecurityMode(12);
    pub const WPA_ENTERPRISE: SecurityMode = SecurityMode(20);
    pub const WPA2_ENTERPRISE: SecurityMode = SecurityMode(24);
    pub const WPA_WPA2_ENTERPRISE: SecurityMode = SecurityMode(28);

    const MEMBERS: [i32; 10] = [0, 1, 2, 3, 4, 8, 12, 20, 24, 28];

    pub fn is_defined(self) -> bool {
        Self::MEMBERS.contains(&self.0)
    }

    /// Accept a raw value only if it is a member of the enum.
    ///
    /// Returns `None` rather than panicking: this code is destined to run INSIDE
    /// `carplay-wireless`, which builds with `panic = "abort"`, so an assert here would take a
    /// live CarPlay session down over an Android-Auto-side configuration mistake.
    pub fn checked(v: i32) -> Option<SecurityMode> {
        let m = SecurityMode(v);
        m.is_defined().then_some(m)
    }
}

/// Whether the phone should treat our AP as fixed or as one it may be handed again later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessPointType(pub i32);

#[allow(dead_code)]
impl AccessPointType {
    pub const STATIC: AccessPointType = AccessPointType(0);
    pub const DYNAMIC: AccessPointType = AccessPointType(1);
}

// ---------------------------------------------------------------------------------------------
// Messages we SEND (head unit -> phone)
// ---------------------------------------------------------------------------------------------

/// `WifiStartRequest { required string ip_address = 1; required uint32 port = 2; }`
///
/// The endpoint the phone dials once it has associated. Both fields are required, so both are
/// always emitted.
pub fn encode_wifi_start_request(ip_address: &str, port: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(ip_address.len() + 8);
    put_str(&mut out, 1, ip_address);
    put_u32(&mut out, 2, port as u32);
    out
}

/// `WifiInfoResponse { ssid=1, password=2, bssid=3, security_mode=4, access_point_type=5 }`
///
/// The AP credentials. Fields 1-4 are required; `access_point_type` is optional and emitted anyway
/// because the references do and the phone is known to tolerate it.
pub fn encode_wifi_info_response(
    ssid: &str,
    password: &str,
    bssid: &str,
    security_mode: SecurityMode,
    access_point_type: AccessPointType,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(ssid.len() + password.len() + bssid.len() + 16);
    put_str(&mut out, 1, ssid);
    put_str(&mut out, 2, password);
    put_str(&mut out, 3, bssid);
    // A value outside the enum makes the phone drop the ENTIRE message (required field). A
    // `debug_assert` here would be compiled out of the shipping release build, so the check is the
    // caller's job via `SecurityMode::checked`; this only documents the invariant.
    put_i32(&mut out, 4, security_mode.0);
    put_i32(&mut out, 5, access_point_type.0);
    out
}

/// `WifiVersionRequest {}` and `WifiInfoRequest {}` carry no fields; the message id is the content.
pub fn encode_empty() -> Vec<u8> {
    Vec::new()
}

// ---------------------------------------------------------------------------------------------
// Messages we RECEIVE (phone -> head unit)
// ---------------------------------------------------------------------------------------------

/// `WifiVersionResponse { uint32 a = 1; uint32 b = 2; optional string c = 3; uint32 d = 4; }`
///
/// The reference names every field `unknown_value_*` and only logs them; so do we. Kept as parsed
/// values rather than discarded so a capture can be compared against the stock box's answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WifiVersionResponse {
    pub value_a: u32,
    pub value_b: u32,
    pub value_c: Option<String>,
    pub value_d: u32,
}

pub fn decode_wifi_version_response(buf: &[u8]) -> Option<WifiVersionResponse> {
    let mut d = Decoder::new(buf);
    let mut out = WifiVersionResponse::default();
    while !d.done() {
        let (field, wire) = d.tag()?;
        match (field, wire) {
            (1, WIRE_VARINT) => out.value_a = d.varint()? as u32,
            (2, WIRE_VARINT) => out.value_b = d.varint()? as u32,
            (3, WIRE_LEN) => out.value_c = Some(String::from_utf8_lossy(d.bytes()?).into_owned()),
            (4, WIRE_VARINT) => out.value_d = d.varint()? as u32,
            _ => d.skip(wire)?,
        }
    }
    Some(out)
}

/// `WifiStartResponse { optional string ip_address = 1; optional uint32 port = 2; required Status status = 3; }`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiStartResponse {
    pub ip_address: Option<String>,
    pub port: Option<u32>,
    pub status: Status,
}

pub fn decode_wifi_start_response(buf: &[u8]) -> Option<WifiStartResponse> {
    let mut d = Decoder::new(buf);
    let mut out = WifiStartResponse { ip_address: None, port: None, status: Status::SUCCESS };
    // `status` is REQUIRED. Tracking presence rather than defaulting: a seed value of SUCCESS with
    // no presence check means a message missing field 3 — or carrying it at an unexpected wire type,
    // which falls through to `skip` — decodes as "everything is fine". That is precisely the
    // "the bootstrap fails and the status says SUCCESS" failure this module exists to prevent.
    let mut saw_status = false;
    while !d.done() {
        let (field, wire) = d.tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => out.ip_address = Some(String::from_utf8_lossy(d.bytes()?).into_owned()),
            (2, WIRE_VARINT) => out.port = Some(d.varint()? as u32),
            (3, WIRE_VARINT) => {
                out.status = Status(d.varint()? as i64 as i32);
                saw_status = true;
            }
            _ => d.skip(wire)?,
        }
    }
    saw_status.then_some(out)
}

/// `WifiConnectionStatus { required Status status = 1; optional string error_message = 2; }`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiConnectionStatus {
    pub status: Status,
    pub error_message: Option<String>,
}

pub fn decode_wifi_connection_status(buf: &[u8]) -> Option<WifiConnectionStatus> {
    let mut d = Decoder::new(buf);
    let mut out = WifiConnectionStatus { status: Status::SUCCESS, error_message: None };
    let mut saw_status = false; // REQUIRED — see decode_wifi_start_response.
    while !d.done() {
        let (field, wire) = d.tag()?;
        match (field, wire) {
            (1, WIRE_VARINT) => {
                out.status = Status(d.varint()? as i64 as i32);
                saw_status = true;
            }
            (2, WIRE_LEN) => {
                out.error_message = Some(String::from_utf8_lossy(d.bytes()?).into_owned())
            }
            _ => d.skip(wire)?,
        }
    }
    saw_status.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_encodes_multibyte() {
        let mut out = Vec::new();
        put_varint(&mut out, 0);
        assert_eq!(out, [0x00]);
        out.clear();
        put_varint(&mut out, 127);
        assert_eq!(out, [0x7f]);
        out.clear();
        put_varint(&mut out, 128);
        assert_eq!(out, [0x80, 0x01]);
        out.clear();
        put_varint(&mut out, 5288);
        assert_eq!(out, [0xa8, 0x29]);
    }

    /// The trap this module exists to get right: a negative proto2 enum is a 10-byte varint.
    #[test]
    fn negative_status_is_ten_byte_varint() {
        let mut out = Vec::new();
        put_i32(&mut out, 3, -3);
        // tag (field 3, varint) + 10 payload bytes
        assert_eq!(out.len(), 11, "sign-extended negative must occupy 10 bytes");
        assert_eq!(out[0], (3 << 3) | WIRE_VARINT as u8);
    }

    /// Every negative Status must survive encode -> decode with its value intact. A decoder that
    /// truncated to u32 or stopped at 5 bytes would pass for SUCCESS and fail this.
    #[test]
    fn every_negative_status_round_trips() {
        for raw in -11..=1i32 {
            let mut payload = Vec::new();
            put_i32(&mut payload, 3, raw);
            let decoded = decode_wifi_start_response(&payload).expect("decodes");
            assert_eq!(decoded.status, Status(raw), "status {raw} round-trip");
            assert_ne!(
                Status(raw).name(),
                "STATUS_UNKNOWN",
                "status {raw} should have a name"
            );
        }
    }

    #[test]
    fn status_success_only_for_zero() {
        assert!(Status::SUCCESS.is_success());
        assert!(!Status::UNSOLICITED_MESSAGE.is_success());
        assert!(!Status::WIFI_INCORRECT_CREDENTIALS.is_success());
    }

    #[test]
    fn start_request_carries_ip_and_port() {
        let bytes = encode_wifi_start_request("192.168.4.1", 5288);
        // field 1, length-delimited, 11 bytes of ASCII
        assert_eq!(bytes[0], (1 << 3) | WIRE_LEN as u8);
        assert_eq!(bytes[1], 11);
        assert_eq!(&bytes[2..13], b"192.168.4.1");
        // field 2, varint 5288
        assert_eq!(bytes[13], (2 << 3) | WIRE_VARINT as u8);
        assert_eq!(&bytes[14..16], &[0xa8, 0x29]);
    }

    #[test]
    fn info_response_emits_all_five_fields_in_order() {
        let bytes = encode_wifi_info_response(
            "ssid",
            "pass",
            "00:11:22:33:44:55",
            SecurityMode::WPA2_PERSONAL,
            AccessPointType::STATIC,
        );
        let mut d = Decoder::new(&bytes);
        let mut seen = Vec::new();
        while !d.done() {
            let (field, wire) = d.tag().expect("tag");
            seen.push(field);
            d.skip(wire).expect("skip");
        }
        assert_eq!(seen, vec![1, 2, 3, 4, 5]);
    }

    /// The passphrase must reach the wire verbatim -- including one with bytes that would break a
    /// naive null-terminated or length-guessed encoder.
    #[test]
    fn passphrase_survives_awkward_bytes() {
        let pw = "p@ss w/ spaces:and=signs";
        let bytes = encode_wifi_info_response(
            "s",
            pw,
            "b",
            SecurityMode::WPA2_PERSONAL,
            AccessPointType::STATIC,
        );
        let mut d = Decoder::new(&bytes);
        let mut found = None;
        while !d.done() {
            let (field, wire) = d.tag().expect("tag");
            if field == 2 && wire == WIRE_LEN {
                found = Some(String::from_utf8(d.bytes().expect("bytes").to_vec()).unwrap());
            } else {
                d.skip(wire).expect("skip");
            }
        }
        assert_eq!(found.as_deref(), Some(pw));
    }

    #[test]
    fn version_response_decodes_and_keeps_optional_string() {
        let mut payload = Vec::new();
        put_u32(&mut payload, 1, 7);
        put_u32(&mut payload, 2, 9);
        put_str(&mut payload, 3, "abc");
        put_u32(&mut payload, 4, 11);
        let v = decode_wifi_version_response(&payload).expect("decodes");
        assert_eq!(v.value_a, 7);
        assert_eq!(v.value_b, 9);
        assert_eq!(v.value_c.as_deref(), Some("abc"));
        assert_eq!(v.value_d, 11);
    }

    /// A field we do not model must not desynchronise the parse -- gearhead may add one.
    #[test]
    fn unknown_fields_are_skipped_not_fatal() {
        let mut payload = Vec::new();
        put_u32(&mut payload, 1, 7);
        put_str(&mut payload, 99, "surprise");
        put_u32(&mut payload, 77, 12345);
        put_u32(&mut payload, 4, 11);
        let v = decode_wifi_version_response(&payload).expect("unknown fields tolerated");
        assert_eq!(v.value_a, 7);
        assert_eq!(v.value_d, 11);
    }

    #[test]
    fn connection_status_carries_error_message() {
        let mut payload = Vec::new();
        put_i32(&mut payload, 1, -3);
        put_str(&mut payload, 2, "bad key");
        let st = decode_wifi_connection_status(&payload).expect("decodes");
        assert_eq!(st.status, Status::WIFI_INCORRECT_CREDENTIALS);
        assert_eq!(st.error_message.as_deref(), Some("bad key"));
    }

    #[test]
    fn truncated_payload_is_rejected_not_panicking() {
        // length says 10 bytes of string, buffer holds 2
        let payload = [(1 << 3) | WIRE_LEN as u8, 10, b'a', b'b'];
        assert!(decode_wifi_start_response(&payload).is_none());
    }

    #[test]
    fn empty_message_decodes_to_defaults() {
        let v = decode_wifi_version_response(&[]).expect("empty is valid");
        assert_eq!(v, WifiVersionResponse::default());
    }
}
