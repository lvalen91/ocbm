//! aa-wireless — the box-side wireless Android Auto bootstrap, as a LIBRARY.
//!
//! Design and protocol: `docs/androidauto/03_WIRELESS.md`; implementation plan in its §6b.
//!
//! WHY A LIBRARY AND NOT A DAEMON. This began as a separate binary. It cannot be one: the SDP
//! server binds L2CAP PSM 0x0001 with a hand-rolled socket and no `SO_REUSEADDR`, so a second
//! process serving SDP gets `EADDRINUSE` and is silently never advertised — the phone would browse
//! and find nothing. The stock CCPA does not work that way either: one `bluetoothDaemon`
//! advertises CarPlay AND Android Auto from a single Bluetooth identity and tells them apart by
//! which RFCOMM channel the phone opens (iAP2 = 1, AAP = 4).
//!
//! So wireless Android Auto is served BY `carplay-wireless`, which is device-proven and must not
//! regress. This crate supplies the parts that are purely Android Auto — the bootstrap protocol and
//! the exchange that drives it — and owns no sockets, no radio and no lifecycle.
//!
//! Everything here is transport-free and host-testable: `run_bootstrap` takes any `Read + Write`,
//! so the whole exchange is exercised by unit tests on the build host. That matters more than usual
//! because the CCPA offers no interactive debugger — it is OCBM or NCM, never both, so there is no
//! second channel to watch the box on while a host drives it. The code has to be right before it
//! ships.

pub mod proto;
pub mod wpp;

use box_common::flags::{self, ProjectionOwner};
use std::io::{Read, Write};

/// Default TCP port we advertise in `WifiStartRequest`. Ours to choose (§2f) — the C++ reference
/// uses 5000, the Rust one uses 5288, and the stock CCPA's own capture shows 54321. One definition,
/// overridable, never a second literal somewhere else.
pub const DEFAULT_PORT: u16 = 5288;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Assemble the AP parameters.
///
/// The passphrase is read here and handed to the socket; it is never logged and never written
/// anywhere durable. `log_params` below exists so that stays true by construction.
pub fn params_from_env() -> wpp::ApParams {
    wpp::ApParams {
        ssid: env_or("AAW_SSID", "carlink"),
        passphrase: env_or("AAW_PASSPHRASE", ""),
        bssid: env_or("AAW_BSSID", "00:00:00:00:00:00"),
        // FIELD-PROVEN: 8 = WPA2_PERSONAL. The stock CCPA's own working wireless-AA session reports
        // `securityMode: 8` (analysis/aa_full_session_adapter_20260315.txt:594, three occurrences),
        // matching aa-proxy-rs's `WifiInfoResponse.proto` table (docs/androidauto/03_WIRELESS.md).
        // An earlier cut defaulted to 24 (WPA2_ENTERPRISE), which never associated — consistent with
        // the phone attempting 802.1X for an enterprise mode.
        //
        // Overridable, but only to another MEMBER of the enum: an out-of-range override is refused
        // and the proven default used instead, because the failure it causes is silent.
        security_mode: env_or("AAW_SECURITY_MODE", "8")
            .parse::<i32>()
            .ok()
            .and_then(proto::SecurityMode::checked)
            .unwrap_or_else(|| {
                eprintln!("[aaw] AAW_SECURITY_MODE is not a WifiSecurityMode value — using 8");
                proto::SecurityMode::WPA2_PERSONAL
            }),
        access_point_type: proto::AccessPointType::STATIC,
        // The box's own SoftAP address. Confirmed three ways: `radio_ap_up.sh`, `av.rs`, and the
        // stock capture above (`ip: 192.168.43.1`). NOT 192.168.4.1, which an earlier cut used.
        ip_address: env_or("AAW_IP", box_common::net::AP_IP),
        port: env_or("AAW_PORT", "").parse().unwrap_or(DEFAULT_PORT),
    }
}

/// Are these credentials actually sendable?
///
/// `WifiInfoResponse.password` is REQUIRED, and an empty string is perfectly valid protobuf — so an
/// AP configured with `wpa_psk=` (a raw hex PSK) rather than `wpa_passphrase=` would produce a
/// well-formed message the phone simply cannot act on. It would associate with nothing, report
/// nothing, and the head unit would wait forever. Refuse up front and say why.
pub fn credentials_are_sendable(p: &wpp::ApParams) -> Result<(), &'static str> {
    if p.ssid.is_empty() {
        return Err("AP has no SSID");
    }
    if p.passphrase.is_empty() {
        return Err("AP has no ASCII passphrase (a raw wpa_psk= cannot be handed to the phone)");
    }
    if p.bssid.is_empty() || p.bssid == "00:00:00:00:00:00" {
        return Err("AP BSSID is unset");
    }
    // Length bounds, so `wpp::encode_frame`'s u16-length assert is dead by construction rather than
    // by luck: this crate is linked into `carplay-wireless` under `panic = "abort"`, where a panic
    // takes a live CarPlay session down with it (proto.rs's own rationale). These are the standard
    // limits — IEEE 802.11 SSID <= 32 bytes, WPA-PSK passphrase 8..=63 — so a config that violates
    // one describes an AP the phone could not have joined anyway.
    if p.ssid.len() > 32 {
        return Err("AP SSID is longer than 32 bytes");
    }
    if p.passphrase.len() < 8 || p.passphrase.len() > 63 {
        return Err("AP passphrase is not 8..=63 bytes");
    }
    if p.bssid.len() > 32 {
        return Err("AP BSSID is malformed (too long)");
    }
    Ok(())
}

/// Log what we will tell the phone, minus the secret.
pub fn log_params(p: &wpp::ApParams) {
    println!(
        "[aaw] ssid={} bssid={} security_mode={} ap_type={} endpoint={}:{} passphrase={}",
        p.ssid,
        p.bssid,
        p.security_mode.0,
        p.access_point_type.0,
        p.ip_address,
        p.port,
        if p.passphrase.is_empty() { "absent" } else { "present" }
    );
}

/// First-come-wins, matching what the wired arms already do (`02_ARBITRATION.md` §4).
///
/// Claimed BEFORE serving, not after: `aa-bridge` learned that the hard way -- claiming inside the
/// session meant the box sat idle with a running bridge and a plugged-in phone, each side waiting
/// for the other. Standing down on a CarPlay claim is the other half; CarPlay must never be dropped
/// mid-drive for a projection the driver did not ask for.
pub fn claim_owner() -> bool {
    let current = flags::owner();
    if current.is_carplay() {
        println!("[aaw] standing down — CarPlay owns the box ({})", current.as_str());
        return false;
    }
    if current == ProjectionOwner::WiredAa {
        println!("[aaw] standing down — wired AA owns the box");
        return false;
    }
    match flags::set_owner(ProjectionOwner::WirelessAa) {
        Ok(()) => {
            println!("[aaw] claimed projection owner = wireless-aa");
            true
        }
        Err(e) => {
            eprintln!("[aaw] could not claim owner: {e}");
            false
        }
    }
}

/// Release only if the claim is still ours.
///
/// `release_owner_if_ours` rather than a blanket clear: the wired bridge used to delete CarPlay's
/// claim while cleaning up after itself, and it took a device test to find (`02_ARBITRATION.md` §5).
pub fn release_owner_if_ours() {
    if flags::owner() == ProjectionOwner::WirelessAa {
        let _ = flags::clear_owner();
        println!("[aaw] released projection owner");
    }
}

/// How one bootstrap exchange ended.
///
/// Four distinct variants rather than one `Ok(())`, because the caller holds the projection-owner
/// claim across `run_bootstrap` and only `Established` means the phone is about to associate and
/// dial the endpoint. Collapsing them made the caller's unconditional release look correct.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The phone reported success; it should now associate and dial the TCP endpoint.
    Established,
    /// The phone reported a failure; the status is the diagnostic surface.
    Failed(proto::Status),
    /// The peer closed the stream, in this phase, before reporting either way.
    PeerClosed(wpp::Phase),
    /// The peer overran the frame-buffer bound; there is no resynchronisation point.
    FramingLost,
}

impl Outcome {
    pub fn is_established(&self) -> bool {
        matches!(self, Outcome::Established)
    }
}

/// Run one bootstrap exchange over an already-connected stream.
///
/// Transport-agnostic on purpose: this is the function the RFCOMM socket will call once it exists,
/// and the TCP stand-in calls it today.
pub fn run_bootstrap<S: Read + Write>(
    stream: &mut S,
    params: wpp::ApParams,
) -> std::io::Result<Outcome> {
    let mut boot = wpp::Bootstrap::new(params);
    let mut framer = wpp::Framer::new();

    // We speak first (§2f step 2).
    let opening = boot.on_connect();
    stream.write_all(&opening)?;
    println!("[aaw] sent WifiVersionRequest + WifiStartRequest");

    let mut buf = [0u8; 1024];
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            println!("[aaw] peer closed, phase={:?}", boot.phase());
            return Ok(Outcome::PeerClosed(boot.phase()));
        }
        framer.push(&buf[..n]);
        if framer.is_poisoned() {
            // The peer overran the frame-buffer bound, so it is not speaking this protocol. Drop
            // the link rather than keep reading: there is no resynchronisation point in a stream
            // whose length prefixes we can no longer trust.
            eprintln!("[aaw] framing lost — peer overran the buffer bound; dropping connection");
            return Ok(Outcome::FramingLost);
        }

        while let Some(frame) = framer.next_frame() {
            println!(
                "[aaw] <- {} (id={}, {} bytes)",
                wpp::msg::name(frame.id),
                frame.id,
                frame.payload.len()
            );
            if frame.id == wpp::msg::WIFI_VERSION_RESPONSE {
                if let Some(v) = proto::decode_wifi_version_response(&frame.payload) {
                    println!("[aaw]    version a={} b={} c={:?} d={}", v.value_a, v.value_b, v.value_c, v.value_d);
                }
            }
            if let wpp::Action::Send(bytes) = boot.on_frame(&frame) {
                stream.write_all(&bytes)?;
                println!("[aaw] -> reply, {} bytes", bytes.len());
            }
        }

        match boot.phase() {
            wpp::Phase::Established => {
                println!("[aaw] bootstrap OK — phone should now associate and dial the endpoint");
                return Ok(Outcome::Established);
            }
            wpp::Phase::Failed => {
                // Name, never the bare number: the negatives ARE the diagnostic surface.
                let st = boot.failure().unwrap_or(proto::Status::SUCCESS);
                eprintln!("[aaw] bootstrap FAILED: {} ({})", st.name(), st.0);
                return Ok(Outcome::Failed(st));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A canned inbound script plus a capture of what we wrote. `read` returns 0 once the script is
    /// exhausted, which is exactly how a peer that closes presents.
    struct Scripted {
        inbound: Vec<u8>,
        pos: usize,
        outbound: Vec<u8>,
    }

    impl Read for Scripted {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = (self.inbound.len() - self.pos).min(buf.len());
            buf[..n].copy_from_slice(&self.inbound[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    impl Write for Scripted {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.outbound.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn params() -> wpp::ApParams {
        wpp::ApParams {
            ssid: "carlink-test".into(),
            passphrase: "passphrase".into(),
            bssid: "AA:BB:CC:DD:EE:FF".into(),
            security_mode: proto::SecurityMode::WPA2_PERSONAL,
            access_point_type: proto::AccessPointType::STATIC,
            ip_address: "192.168.43.1".into(),
            port: DEFAULT_PORT,
        }
    }

    /// Pinned at 8 (`WPA2_PERSONAL`) — the field-proven wire value. Field 4, varint:
    /// tag `(4<<3)|0 = 0x20`, then 8.
    #[test]
    fn default_security_mode_encodes_as_varint_8() {
        let p = params_from_env();
        assert_eq!(p.security_mode.0, 8);
        let body = proto::encode_wifi_info_response(
            "s",
            "p",
            "b",
            p.security_mode,
            proto::AccessPointType::STATIC,
        );
        assert!(
            body.windows(2).any(|w| w == [0x20, 0x08]),
            "field 4 must encode as varint 8: {body:02X?}"
        );
    }

    /// Length bounds keep `wpp::encode_frame`'s u16 assert unreachable under `panic = "abort"`.
    #[test]
    fn out_of_range_credential_lengths_are_refused() {
        let mut p = params();
        p.ssid = "x".repeat(33);
        assert!(credentials_are_sendable(&p).is_err());
        let mut p = params();
        p.passphrase = "short".into();
        assert!(credentials_are_sendable(&p).is_err());
        let mut p = params();
        p.passphrase = "x".repeat(64);
        assert!(credentials_are_sendable(&p).is_err());
        assert!(credentials_are_sendable(&params()).is_ok());
    }

    /// The caller keeps the projection-owner claim on `Established` and drops it otherwise, so the
    /// two must be distinguishable from the return value alone -- they used to be the same `Ok(())`.
    #[test]
    fn established_and_peer_closed_are_distinguishable_outcomes() {
        // `WifiConnectionStatus { status = SUCCESS }`: field 1, varint, 0.
        let mut script = wpp::encode_frame(wpp::msg::WIFI_INFO_REQUEST, &proto::encode_empty());
        script.extend_from_slice(&wpp::encode_frame(wpp::msg::WIFI_CONNECT_STATUS, &[0x08, 0x00]));
        let mut ok = Scripted { inbound: script, pos: 0, outbound: Vec::new() };
        assert_eq!(run_bootstrap(&mut ok, params()).unwrap(), Outcome::Established);

        let mut silent = Scripted { inbound: Vec::new(), pos: 0, outbound: Vec::new() };
        assert_eq!(
            run_bootstrap(&mut silent, params()).unwrap(),
            Outcome::PeerClosed(wpp::Phase::Offered)
        );
    }
}
