//! Box-side network constants that more than one crate has to agree on.
//!
//! One definition, deliberately. Before this module the SoftAP gateway address existed as three
//! independent literals — `btd`'s `av.rs`, `aa-wireless`'s `AAW_IP` default, and the
//! shell AP bring-up — and the wireless Android Auto bootstrap ADVERTISES it to the phone in
//! `WifiStartRequest.ip_address`. A drift between the address we advertise and the address the
//! pump binds is invisible on the box and fatal on the phone: it associates, dials an endpoint
//! nobody is listening on, and the head unit waits forever with no error anywhere.

/// The gateway address the box's own SoftAP serves on the WLAN interface.
///
/// Field-confirmed three ways: `boards/ccpa-imx6ul/rootfs/script/start_bluetooth_wifi.sh` (`WLANIP`), the AirPlay
/// receiver bind in `crates/vendor/wireless/src/av.rs`, and the stock CCPA's own working wireless
/// Android Auto capture (`ip: 192.168.43.1`). NOT `192.168.4.1`, which an earlier cut used.
///
/// This is an ADDRESS, not an interface name — the interface name is an insmod parameter on this
/// hardware and must never be hardcoded (see the radio rules in CLAUDE.md). The shell AP layer owns
/// the address, so if that ever changes, change it here and the Rust side follows in one place.
pub const AP_IP: &str = "192.168.43.1";

/// The unit's Wi-Fi interface, whatever its driver named it: `wlan0` (rtw88, the vendor 3.14 kernel's
/// drivers), `mlan0` (mainline mwifiex on the baseline image), `sta0` (vendor Realtek `if2name=`). The
/// name is an insmod parameter, so it is ENUMERATED (`/sys/class/net/*/phy80211`), never assumed
/// (CLAUDE.md radio rules). `wlan0` keeps precedence among several, so a box that has it behaves as
/// before; with none at all the answer is `wlan0`, the historical constant, for callers that only
/// pass the name on.
#[must_use]
pub fn wlan_iface() -> String {
    wlan_iface_in(std::path::Path::new("/sys/class/net"))
}

/// [`wlan_iface`] against an arbitrary sysfs `class/net` directory (tests).
#[must_use]
pub fn wlan_iface_in(net: &std::path::Path) -> String {
    let mut names: Vec<String> = std::fs::read_dir(net)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter(|e| e.path().join("phy80211").exists())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names.sort_by_key(|n| n != "wlan0");
    names.into_iter().next().unwrap_or_else(|| "wlan0".to_string())
}

#[cfg(test)]
mod tests {
    use super::wlan_iface_in;

    fn sysfs(ifaces: &[(&str, bool)]) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "box-common-net-{}-{}",
            std::process::id(),
            ifaces.iter().map(|(n, _)| *n).collect::<Vec<_>>().join("-")
        ));
        let _ = std::fs::remove_dir_all(&d);
        for (name, wireless) in ifaces {
            let i = d.join(name);
            std::fs::create_dir_all(&i).unwrap();
            if *wireless {
                std::fs::create_dir_all(i.join("phy80211")).unwrap();
            }
        }
        d
    }

    #[test]
    fn wlan_iface_enumerates_the_wireless_interface() {
        assert_eq!(wlan_iface_in(&sysfs(&[("lo", false), ("usb0", false), ("mlan0", true)])), "mlan0");
        assert_eq!(wlan_iface_in(&sysfs(&[("mlan0", true), ("wlan0", true)])), "wlan0");
        assert_eq!(wlan_iface_in(&sysfs(&[("lo", false), ("ncm0", false)])), "wlan0");
        assert_eq!(wlan_iface_in(std::path::Path::new("/nonexistent-box-common")), "wlan0");
    }
}
