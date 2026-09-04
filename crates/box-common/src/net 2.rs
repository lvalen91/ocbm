//! Box-side network constants that more than one crate has to agree on.
//!
//! One definition, deliberately. Before this module the SoftAP gateway address existed as three
//! independent literals — `carplay-wireless`'s `av.rs`, `aa-wireless`'s `AAW_IP` default, and the
//! shell AP bring-up — and the wireless Android Auto bootstrap ADVERTISES it to the phone in
//! `WifiStartRequest.ip_address`. A drift between the address we advertise and the address the
//! pump binds is invisible on the box and fatal on the phone: it associates, dials an endpoint
//! nobody is listening on, and the head unit waits forever with no error anywhere.

/// The gateway address the box's own SoftAP serves on the WLAN interface.
///
/// Field-confirmed three ways: `ccpa/rootfs/script/start_bluetooth_wifi.sh` (`WLANIP`), the AirPlay
/// receiver bind in `crates/vendor/wireless/src/av.rs`, and the stock CCPA's own working wireless
/// Android Auto capture (`ip: 192.168.43.1`). NOT `192.168.4.1`, which an earlier cut used.
///
/// This is an ADDRESS, not an interface name — the interface name is an insmod parameter on this
/// hardware and must never be hardcoded (see the radio rules in CLAUDE.md). The shell AP layer owns
/// the address, so if that ever changes, change it here and the Rust side follows in one place.
pub const AP_IP: &str = "192.168.43.1";
