//! Per-box accessory identity (#636). Fleet-fixed DEVICE_ID / pairing-identity / Ed25519-seed constants
//! meant every CCPA box advertised the SAME identity, so multiple boxes collapsed into ONE iOS "car"
//! record (adding a second box inherited the first's name/pairing). This derives a STABLE, PER-BOX
//! identity from a hardware-unique value and hands it to carplayd via env, so each box is
//! a distinct car while carplayd and rx-connect stay in lockstep (they read the same env).

use sha2::{Digest, Sha256};

/// The fixed hardware id a box with no readable Wi-Fi MAC, SoC serial or cpuinfo `Serial` falls back to.
/// It is only ever hashed (`derive_from`), so it changes the derived `device_id`/`pi`/Ed25519 seed of such
/// a box, and nothing else. Board-neutral since 2026-11-02 (it was `"ccpa-fallback"` before): a box that
/// takes this path got a new identity once with that change, so its iPhone re-pairs once.
const FALLBACK_HWID: &str = "box-fallback";

/// First available hardware-unique id. wlan0's MAC is primary — it's per-box and wlan0 is always up by
/// the time the A/V layer starts (the AP is running); the SoC OTP serial and `/proc/cpuinfo` Serial are
/// fallbacks. A box with none readable falls back to `FALLBACK_HWID` (degenerate, effectively the old
/// single-identity behavior).
fn hardware_id() -> String {
    // The Wi-Fi interface by enumeration (box_common::net::wlan_iface: `wlan0` on the vendor kernel,
    // `mlan0` on the mainline baseline); the literal `wlan0` until 2026-10-30.
    let wifi = format!("/sys/class/net/{}/address", box_common::net::wlan_iface());
    for path in [wifi.as_str(), "/sys/devices/soc0/serial_number"] {
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
    FALLBACK_HWID.to_string()
}

/// Domain-separated SHA-256 so the device-id / pi / seed derived from one hardware id are independent.
fn digest(hwid: &str, tag: &str) -> [u8; 32] {
    let mut d = Sha256::new();
    d.update(hwid.as_bytes());
    d.update(b"|");
    d.update(tag.as_bytes());
    d.finalize().into()
}

/// The derived per-box identity, in the exact string forms carplayd consume via env.
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
    derive_from(&hardware_id())
}

/// The derivation itself, from an explicit hardware id. Split out of `derive()` (2026-11-02) only so
/// the golden-vector tests below can pin it without reading `/sys` or `/proc`; `derive()` calls it
/// with `hardware_id()`, so the result for any box is unchanged.
fn derive_from(hwid: &str) -> BoxIdentity {
    let d = digest(hwid, "device-id");
    // Format 6 bytes as a MAC; force a valid unicast Locally-Administered Address (set bit1, clear bit0
    // of the first octet) so it never collides with a real OUI or looks like a multicast address.
    let b0 = (d[0] & 0xFE) | 0x02;
    let device_id = format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        b0, d[1], d[2], d[3], d[4], d[5]
    );

    let p = digest(hwid, "pi");
    let pi = format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7], p[8], p[9], p[10], p[11], p[12], p[13], p[14], p[15]
    );

    let seed = digest(hwid, "ed-seed");
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

    /// T1, golden vector for a synthetic hardware id (deliberately neither MAC- nor serial-shaped).
    /// The expected strings were computed outside Rust (Python `hashlib.sha256(hwid + "|" + tag)`,
    /// same formatting rules) and pinned BEFORE the fallback marker was renamed, so this test proves the
    /// derivation every box with a readable hardware id goes through is byte-for-byte unchanged.
    #[test]
    fn golden_vector_for_a_synthetic_hardware_id() {
        let id = derive_from("golden-test-hwid");
        assert_eq!(id.device_id, "E2:B9:F3:07:5E:CA");
        assert_eq!(id.pi, "c0ccf17f-ece5-4203-b791-0d8d41217eaf");
        assert_eq!(
            id.ed_seed_hex,
            "4bfad9f662d6bb49c0e69990b300b9dcb4b151395e1d9efa5e93bfc6d3fc6354"
        );
    }

    /// T2, the fallback marker itself: the board-neutral name, no board name in it, and not shaped like a
    /// real hardware id (a MAC or a hex serial), so it can never collide with one.
    #[test]
    fn fallback_marker_is_neutral_and_not_a_hardware_id() {
        assert_eq!(FALLBACK_HWID, "box-fallback");
        assert!(!FALLBACK_HWID.to_ascii_lowercase().contains("ccpa"));
        let octets: Vec<&str> = FALLBACK_HWID.split(':').collect();
        let mac_shaped = octets.len() == 6
            && octets
                .iter()
                .all(|o| o.len() == 2 && o.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!mac_shaped, "fallback marker must not parse as a MAC");
        assert!(
            !FALLBACK_HWID.chars().all(|c| c.is_ascii_hexdigit()),
            "fallback marker must not look like a hex serial"
        );
    }

    /// T3, the identity of a box that takes the fallback, pinned (computed outside Rust like T1). It
    /// differs from the identity the old marker gave: under `"ccpa-fallback"` the device_id was E6:6E:51:FB:31:26.
    #[test]
    fn fallback_identity_is_pinned() {
        let id = derive_from(FALLBACK_HWID);
        assert_eq!(id.device_id, "16:4F:30:AE:5A:0C");
        assert_eq!(id.pi, "924be9e1-46fd-1c60-89be-4e2e9fbbac8a");
        assert_eq!(
            id.ed_seed_hex,
            "da6b246b09271962b57825d2004ee28d1447df491c3479657eebf112125872b6"
        );
    }
}
