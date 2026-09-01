//! Per-box accessory identity (#636). Fleet-fixed DEVICE_ID / pairing-identity / Ed25519-seed constants
//! meant every CCPA box advertised the SAME identity, so multiple boxes collapsed into ONE iOS "car"
//! record (adding a second box inherited the first's name/pairing). This derives a STABLE, PER-BOX
//! identity from a hardware-unique value and hands it to airplayd + rx-connect via env, so each box is
//! a distinct car while airplayd and rx-connect stay in lockstep (they read the same env).

use sha2::{Digest, Sha256};

/// First available hardware-unique id. wlan0's MAC is primary — it's per-box and wlan0 is always up by
/// the time the A/V layer starts (the AP is running); the SoC OTP serial and `/proc/cpuinfo` Serial are
/// fallbacks. A box with none readable falls back to a fixed marker (degenerate, effectively the old
/// single-identity behavior).
fn hardware_id() -> String {
    for path in [
        "/sys/class/net/wlan0/address",
        "/sys/devices/soc0/serial_number",
    ] {
        if let Ok(s) = std::fs::read_to_string(path) {
            let t = s.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    if let Ok(cpu) = std::fs::read_to_string("/proc/cpuinfo") {
        for line in cpu.lines() {
            if let Some(v) = line.strip_prefix("Serial") {
                let v = v.trim_start_matches([' ', '\t', ':']).trim();
                if !v.is_empty() {
                    return v.to_string();
                }
            }
        }
    }
    "ccpa-fallback".to_string()
}

/// Domain-separated SHA-256 so the device-id / pi / seed derived from one hardware id are independent.
fn digest(hwid: &str, tag: &str) -> [u8; 32] {
    let mut d = Sha256::new();
    d.update(hwid.as_bytes());
    d.update(b"|");
    d.update(tag.as_bytes());
    d.finalize().into()
}

/// The derived per-box identity, in the exact string forms airplayd/rx-connect consume via env.
pub struct BoxIdentity {
    /// AirPlay device id / MAC string, e.g. "A2:B4:…" — a valid unicast locally-administered address.
    pub device_id: String,
    /// HomeKit pairing identity (`pi`) as a UUID string.
    pub pi: String,
    /// Ed25519 accessory-key seed as 64 lowercase hex chars.
    pub ed_seed_hex: String,
}

/// Derive the per-box identity. Deterministic: same box → same identity across reboots.
pub fn derive() -> BoxIdentity {
    let hwid = hardware_id();

    let d = digest(&hwid, "device-id");
    // Format 6 bytes as a MAC; force a valid unicast Locally-Administered Address (set bit1, clear bit0
    // of the first octet) so it never collides with a real OUI or looks like a multicast address.
    let b0 = (d[0] & 0xFE) | 0x02;
    let device_id = format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        b0, d[1], d[2], d[3], d[4], d[5]
    );

    let p = digest(&hwid, "pi");
    let pi = format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7], p[8], p[9], p[10], p[11], p[12], p[13], p[14], p[15]
    );

    let seed = digest(&hwid, "ed-seed");
    let ed_seed_hex = seed.iter().map(|b| format!("{b:02x}")).collect();

    BoxIdentity {
        device_id,
        pi,
        ed_seed_hex,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_is_deterministic_and_well_formed() {
        let a = derive();
        let b = derive();
        assert_eq!(a.device_id, b.device_id, "same box → same device_id");
        assert_eq!(a.pi, b.pi);
        assert_eq!(a.ed_seed_hex, b.ed_seed_hex);
        assert_eq!(a.device_id.len(), 17, "MAC string AA:BB:CC:DD:EE:FF");
        assert_eq!(a.pi.len(), 36, "UUID with dashes");
        assert_eq!(a.ed_seed_hex.len(), 64, "32-byte seed as hex");
        // Locally-administered unicast: first octet has bit1 set, bit0 clear.
        let b0 = u8::from_str_radix(&a.device_id[0..2], 16).unwrap();
        assert_eq!(b0 & 0x03, 0x02, "device_id first octet is unicast LAA");
    }
}
