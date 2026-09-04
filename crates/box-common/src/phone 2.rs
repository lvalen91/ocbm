//! Phone-type detection — the unified resolver the arbiter needs to pick CarPlay vs Android Auto
//! for a freshly-plugged device. Today the box has two ISOLATED probes (CarPlay greps Apple 0x05ac,
//! aa-bridge matches Google 0x18d1); this is the single place that classifies by USB idVendor.
//!
//! Portable (plain sysfs file reads), so it compiles on the host too; only meaningful on the box.

use std::fs;

/// Apple's USB vendor id — a device in NORMAL mode reporting this is an iPhone (CarPlay path).
pub const APPLE_VID: u16 = 0x05ac;
/// Google's USB vendor id (Pixel and AOSP reference). Not exhaustive for Android — see [`classify`].
pub const GOOGLE_VID: u16 = 0x18d1;

/// What kind of phone (by USB vendor) is on the host-facing port, and thus which projection path
/// should own it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhoneType {
    /// Apple (0x05ac) — CarPlay (iAP2/AirPlay).
    Apple,
    /// Any non-Apple device that is a plausible Android Auto candidate. The DEFINITIVE test is the
    /// AOAP getProtocol probe (aa-bridge), because Android OEM vendor ids vary widely; treating
    /// "not Apple, not a hub/host-controller" as an AA candidate and letting the AOAP probe confirm
    /// is more robust than a hand-maintained OEM allowlist.
    Android,
    /// Root hubs / host controllers (Linux Foundation 0x1d6b) and anything not a phone.
    Unknown,
}

/// Linux Foundation vendor id — the box's own root hubs / gadget controllers, never a phone.
const LINUX_FOUNDATION_VID: u16 = 0x1d6b;

/// Classify a USB vendor id. Apple → CarPlay; root hub → Unknown; everything else is an Android
/// Auto candidate to be confirmed by the AOAP probe.
pub fn classify(vid: u16) -> PhoneType {
    match vid {
        APPLE_VID => PhoneType::Apple,
        LINUX_FOUNDATION_VID => PhoneType::Unknown,
        _ => PhoneType::Android,
    }
}

/// USB device class for a hub — mandatory in every hub's DEVICE descriptor (USB 2.0 §11.23.1).
pub const USB_CLASS_HUB: u8 = 0x09;

/// Classify with the device class as well as the vendor id.
///
/// `classify()` alone calls every non-Apple, non-root-hub vendor an Android candidate, which makes a
/// bare external hub — or a dashcam, or a card reader — look like a phone. That is not cosmetic: it
/// launches an aa-bridge that AOAP-probes the hub forever, and a permanently resident bridge is the
/// precondition for the CarPlay-hijack class of defect. A hub is unambiguous at the descriptor level,
/// so exclude it here. Devices that are NOT hubs still fall through to the vid-only rule, and the
/// AOAP probe remains the definitive test.
pub fn classify_dev(vid: u16, dev_class: u8) -> PhoneType {
    if dev_class == USB_CLASS_HUB {
        return PhoneType::Unknown;
    }
    classify(vid)
}

/// A device discovered on the host-facing USB bus via sysfs.
#[derive(Clone, Debug)]
pub struct Detected {
    pub sysfs: String, // e.g. /sys/bus/usb/devices/1-1
    pub vid: u16,
    pub pid: u16,
    pub kind: PhoneType,
}

/// Scan `/sys/bus/usb/devices/*` and return the phones (Apple or Android candidates), skipping root
/// hubs and interface nodes. This is the same sysfs source `session_supervisor.sh` greps (`idVendor`),
/// so the shell arbiter and any Rust arbiter agree on what is plugged. Returns Unknown-kind entries
/// filtered out.
pub fn detect(sys_bus_usb_devices: &str) -> Vec<Detected> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(sys_bus_usb_devices) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for ent in entries.flatten() {
        let dir = ent.path();
        // Interface nodes look like "1-1:1.0" — they carry no idVendor; skip by absence below.
        let vid = match read_hex_u16(&dir.join("idVendor")) {
            Some(v) => v,
            None => continue,
        };
        let pid = read_hex_u16(&dir.join("idProduct")).unwrap_or(0);
        let kind = classify(vid);
        if kind == PhoneType::Unknown {
            continue; // root hub / controller
        }
        out.push(Detected {
            sysfs: dir.to_string_lossy().into_owned(),
            vid,
            pid,
            kind,
        });
    }
    out
}

fn read_hex_u16(path: &std::path::Path) -> Option<u16> {
    let s = fs::read_to_string(path).ok()?;
    u16::from_str_radix(s.trim(), 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classify_dev_excludes_hubs() {
        // Measured on the box: root hub reads vid=1d6b class=09; the Pixel reads vid=18d1 class=00.
        assert_eq!(classify_dev(0x05e3, USB_CLASS_HUB), PhoneType::Unknown); // Genesys hub
        assert_eq!(classify_dev(0x2109, USB_CLASS_HUB), PhoneType::Unknown); // VIA hub
        assert_eq!(classify_dev(0x18d1, 0x00), PhoneType::Android); // Pixel, normal mode
        assert_eq!(classify_dev(0x18d1, 0x00), classify(0x18d1)); // non-hubs unchanged
        assert_eq!(classify_dev(0x05ac, 0x00), PhoneType::Apple);
    }

    #[test]
    fn classify_vendors() {
        assert_eq!(classify(0x05ac), PhoneType::Apple);
        assert_eq!(classify(0x18d1), PhoneType::Android); // Google Pixel
        assert_eq!(classify(0x04e8), PhoneType::Android); // Samsung
        assert_eq!(classify(0x1d6b), PhoneType::Unknown); // root hub
    }
}
