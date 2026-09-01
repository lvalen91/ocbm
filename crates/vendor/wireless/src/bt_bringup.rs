//! Raw Bluetooth Class-of-Device/name/EIR bring-up, porting `carlink_linux`'s proven
//! `carplay_bt_raw_up.sh` (live-verified against a real iPhone by that project) via `hciconfig` --
//! confirmed present on this Pi's Raspberry Pi OS `bluez` build despite upstream deprecating it
//! elsewhere, so shelling out here matches a proven reference exactly rather than hand-rolling raw
//! HCI ioctls for this phase.
//!
//! `bluetoothd` MUST be masked (not just stopped) before this runs -- it otherwise fights the raw
//! CoD/EIR settings and answers its own (empty) SDP. That requirement is the load-bearing part and
//! stands on its own.
//!
//! CORRECTED 2026-08-11: this comment used to justify it by analogy -- "the wired mDNS path already
//! masks `avahi-daemon` for the identical reason -- see docs/ops/04_OPEN_ITEMS.md". NEITHER
//! half survives checking. That handoff contains no avahi content, and `avahi` appears NOWHERE in
//! this repo. The precedent was asserted, not established. Removed rather than repointed, because
//! carrying a citation to a source that does not contain the claim is worse than carrying no
//! citation at all.

use std::process::Command;

/// Class of Device: Major Service "Audio" + Major Device "Audio/Video" + Minor "Hands-free" --
/// the exact value the reference project's own comment identifies as "the CCPA value" (i.e. what a
/// real CarPlay dongle presents, which is what iOS's CarPlay accessory filter looks for).
pub const CLASS_OF_DEVICE: u32 = 0x200408;

/// The iAP2 service UUID (128-bit) a CarPlay-capable accessory advertises, and the CarPlay
/// discovery marker UUID, byte-exact from the reference project's own capture-verified EIR.
const IAP2_SERVICE_UUID: [u8; 16] = [
    0x00, 0x00, 0x00, 0x00, 0xde, 0xca, 0xfa, 0xde, 0xde, 0xca, 0xde, 0xaf, 0xde, 0xca, 0xca, 0xff,
];
const CARPLAY_MARKER_UUID: [u8; 16] = [
    0xd3, 0x1f, 0xbf, 0x50, 0x5d, 0x57, 0x27, 0x97, 0xa2, 0x40, 0x41, 0xcd, 0x48, 0x43, 0x88, 0xec,
];

/// Build the Extended Inquiry Response data `hciconfig <dev> inqdata` expects: a sequence of
/// `[len][type][data...]` AD structures -- Complete Local Name (type 0x09) + two 128-bit Service
/// UUID lists (type 0x06 incomplete, 0x07 complete), matching the reference's exact AD layout.
pub fn eir_bytes(name: &str) -> Vec<u8> {
    let mut eir = Vec::new();

    // AD: Complete Local Name — NUL-terminated. The working carlink_linux EIR (byte-identical to a
    // genuine CCPA's /etc/bluetooth/eir_info) appends a trailing 0x00 and its length byte counts it
    // (`0f 09 <name> 00` for a 13-char name). Reproduce that exactly.
    let name_bytes = name.as_bytes();
    eir.push((name_bytes.len() + 2) as u8); // type byte + name bytes + trailing NUL
    eir.push(0x09);
    eir.extend_from_slice(name_bytes);
    eir.push(0x00);

    // AD: 128-bit Service UUID list (incomplete), the iAP2 service.
    eir.push(1 + 16);
    eir.push(0x06);
    eir.extend_from_slice(&IAP2_SERVICE_UUID);

    // AD: 128-bit Service UUID list (complete), the CarPlay discovery marker.
    eir.push(1 + 16);
    eir.push(0x07);
    eir.extend_from_slice(&CARPLAY_MARKER_UUID);

    eir
}

fn eir_hex(name: &str) -> String {
    eir_bytes(name).iter().map(|b| format!("{b:02x}")).collect()
}

fn run(args: &[&str]) -> std::io::Result<()> {
    let status = Command::new("hciconfig").args(args).status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "hciconfig {args:?} failed: {status}"
        )));
    }
    Ok(())
}

/// Stop the box's stock BT daemons that would fight this raw stack. On the IW416 box, `bt_on.sh`
/// brings `hci0` up but also starts `hcid` (and possibly `bluetoothDaemon`), which runs its own SDP
/// server on L2CAP PSM 1 and would answer the iPhone's SDP with no iAP2 record -- the exact failure
/// the PoC documents for `bluetoothd`. Best-effort `killall` (ignore "no such process"); neither is
/// respawn-protected, so this is enough. This is the ccpa_custom analog of the PoC masking
/// `bluetoothd` via systemd.
fn stop_conflicting_daemons() {
    for d in ["hcid", "bluetoothDaemon", "sdpd"] {
        let _ = Command::new("killall").arg(d).status();
    }
}

/// Bring the controller up as a discoverable/connectable CarPlay-style accessory: set class, name,
/// EIR, then enable page+inquiry scan (discoverable + connectable, no timeout).
/// Is the HCI UART line discipline registered with the kernel?
///
/// `hci_uart` is a LOADABLE MODULE on the CCPA (RFCOMM/SCO/L2CAP and the BT core are built in; only
/// this one is not), shipped inside `/lib/firmware/nxp/iw416_ko.tar.gz`. Something has to extract and
/// `insmod` it before `hciattach` runs -- on this box that is `radio_hal.sh`, which the supervisor
/// invokes with its exit status unread.
///
/// When the module is absent, `hciattach`'s `ioctl(TIOCSETD, N_HCI)` fails `EINVAL` and prints
/// "Can't set line discipline: Invalid argument" -- three steps downstream of the real cause, and
/// directly beneath a flawless firmware download. That combination cost a full session on
/// 2026-08-28: no `hci0`, no pairing, no CarPlay, and every layer above still reporting success.
///
/// `/proc/tty/ldiscs` answers it in one read, so check before walking into it. See ccpa_custom
/// `docs/ops/06_CORRECTIONS_LEDGER.md` R-20W-5.
fn hci_ldisc_registered() -> bool {
    std::fs::read_to_string("/proc/tty/ldiscs")
        .map(|s| s.lines().any(|l| l.split_whitespace().next() == Some("n_hci")))
        .unwrap_or(true) // unreadable /proc: do not block bring-up on a diagnostic
}

pub fn bring_up(hci_dev: &str, name: &str) -> std::io::Result<()> {
    // Name the real fault at its source rather than letting it surface as an unexplained EINVAL.
    // Deliberately a LOG, not an early return: this is diagnostic, the controller may already be
    // attached by some path we do not model, and refusing to proceed on a heuristic would be worse
    // than the silence it replaces.
    if !hci_ldisc_registered() {
        eprintln!(
            "[bt-bringup] WARNING: no n_hci line discipline registered — hciattach cannot create \
             {hci_dev} and BT will not come up. hci_uart is a loadable module here; check that \
             /script/radio_hal.sh and /script/radio_detect.sh are installed (ccpa_custom \
             docs/ops/06_CORRECTIONS_LEDGER.md R-20W-5)."
        );
    }
    stop_conflicting_daemons(); // box: free L2CAP PSM 1 (SDP) from the stock hcid before we bind it

    // docs/wireless/01_BT_AND_RADIO.md: force a DOWN->UP cycle, not a bare `up`. The kernel sends its Set_Event_Mask (which is
    // what enables the SSP events IO_Capability_Request 0x31 / User_Confirmation_Request 0x33 /
    // User_Passkey_Request 0x34 / Simple_Pairing_Complete 0x36) ONLY from `hci_dev_do_open`, i.e. an
    // HCIDEVUP on a DOWN device -- verified against the v3.14.52 sources: `hci_setup_event_mask`
    // (hci_core.c:1081) is reachable only via `hci_init2_req` -> `__hci_init` -> `hci_dev_do_open`,
    // `hci_dev_reset` (hci_core.c:2108) does NOT re-init, and `hci_dev_do_open` returns -EALREADY when
    // HCI_UP is set (which `hciconfig up` swallows, exiting 0). So any earlier raw HCI_Reset (the bug
    // removed from attach_bluetooth.sh) or `hciconfig reset` leaves the controller on the spec-default
    // mask -- SDP and ACL keep working (their events are below 0x2D), and bonded RECONNECTS keep
    // working (Link_Key_Request 0x17 is also below it), but fresh SSP pairing silently never reaches
    // the host and the phone reports "Pairing Unsuccessful". Doing this here makes the daemon
    // self-sufficient regardless of what the boot scripts did to the controller beforehand. `down` is
    // best-effort (already-down returns 0); only the `up` is load-bearing.
    //
    // SAFETY (corrected 2026-07-25 -- an earlier version of this comment wrongly claimed "runs once at
    // startup"): `bring_up` runs once per ACTIVE SESSION, re-entered from `main.rs`'s arbiter loop on
    // every wired->wireless handback. The real guarantee is ordering within `run_active_session`: the
    // SSP mgmt socket, the SDP listener and the RFCOMM listener are all spawned AFTER this returns,
    // and the previous session's threads are joined BEFORE re-entry -- so the down/up never hits a
    // bound socket or a live listener. During the quiet period `go_quiet`'s `noscan` prevents any new
    // inbound ACL, so no in-use connection can be severed either.
    //
    // KNOWN, ACCEPTED SIDE EFFECT: this controller is `hciattach`'d over UART, so it carries
    // HCI_QUIRK_RESET_ON_CLOSE -- the `down` makes the kernel issue a real HCI_Reset on close, and the
    // `up` re-reads Read_Buffer_Size. That discards `attach_bluetooth.sh`'s HFP setup (its
    // `hciconfig scomtu 240:32` plus the two vendor `hcitool` commands: SCO-to-HCI routing and BLE
    // power). Deliberately NOT re-issued here: CarPlay carries ALL audio -- including telephony -- over
    // the AirPlay/WiFi session, never over SCO/HFP, so nothing in this project's proven path depends on
    // them, and re-issuing vendor-opaque raw HCI from this daemon would add untested writes to the
    // controller for no functional gain. If HFP-over-BT is ever actually exercised, restore them after
    // this `up`.
    // Native backend (Raspberry Pi port): `hciconfig` is BlueZ userspace and does not exist on
    // Android, so every operation below is done with ioctls + raw HCI command packets instead. The
    // ORDER and the DOWN->UP cycle are identical — see the reasoning above, it is load-bearing.
    if crate::hci::native_selected() {
        let dev = crate::hci::dev_id(hci_dev)?;
        let _ = crate::hci::dev_down(dev); // best-effort: already-down returns EALREADY
        crate::hci::dev_up(dev)?;
        let _ = crate::hci::write_ssp_mode(dev, true); // non-fatal, as with `hciconfig sspmode`
        crate::hci::write_class_of_device(dev, CLASS_OF_DEVICE)?;
        crate::hci::write_local_name(dev, name)?;
        crate::hci::write_eir(dev, &eir_bytes(name))?;
        crate::hci::set_scan(dev, crate::hci::SCAN_PAGE_AND_INQUIRY)?;
        return Ok(());
    }

    let _ = run(&[hci_dev, "down"]);
    run(&[hci_dev, "up"])?;
    // Force Simple Pairing mode ON at the HCI level (HCI Write_Simple_Pairing_Mode). The NXP IW416
    // firmware defaults SSP OFF and the mgmt SET_SSP path can be rejected silently, leaving "Simple
    // Pairing mode: Disabled" → pairing degrades to legacy PIN → iOS refuses the iAP2/CarPlay handshake
    // (SDP loops, RFCOMM never opens). This deterministically flips the readback; SSP is required for
    // BOTH Just-Works and Numeric Comparison. Non-fatal if the build's hciconfig lacks `sspmode`.
    let _ = run(&[hci_dev, "sspmode", "1"]);
    run(&[hci_dev, "class", &format!("0x{CLASS_OF_DEVICE:06x}")])?;
    run(&[hci_dev, "name", name])?;
    run(&[hci_dev, "inqdata", &eir_hex(name)])?;
    run(&[hci_dev, "piscan"])?;
    Ok(())
}

/// Stop being discoverable/connectable -- used when the arbiter preempts this session in favor of
/// wired, or on shutdown. Does not power the controller off (a future phase reuses it for RFCOMM).
pub fn go_quiet(hci_dev: &str) -> std::io::Result<()> {
    if crate::hci::native_selected() {
        let dev = crate::hci::dev_id(hci_dev)?;
        return crate::hci::set_scan(dev, crate::hci::SCAN_NONE);
    }
    run(&[hci_dev, "noscan"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eir_starts_with_complete_local_name() {
        let eir = eir_bytes("CarLink");
        assert_eq!(eir[0], 9); // type byte + "CarLink" (7 bytes) + trailing NUL
        assert_eq!(eir[1], 0x09);
        assert_eq!(&eir[2..9], b"CarLink");
        assert_eq!(eir[9], 0x00); // NUL terminator, counted in the length above
    }

    #[test]
    fn eir_contains_both_service_uuids() {
        let eir = eir_bytes("CarLink");
        // iAP2 service UUID AD starts right after the name AD (2-byte header + 7-byte name + 1 NUL).
        let iap2_start = 2 + 7 + 1;
        assert_eq!(eir[iap2_start], 17); // 1 (type) + 16 (UUID)
        assert_eq!(eir[iap2_start + 1], 0x06);
        assert_eq!(&eir[iap2_start + 2..iap2_start + 18], &IAP2_SERVICE_UUID);

        let marker_start = iap2_start + 18;
        assert_eq!(eir[marker_start], 17);
        assert_eq!(eir[marker_start + 1], 0x07);
        assert_eq!(
            &eir[marker_start + 2..marker_start + 18],
            &CARPLAY_MARKER_UUID
        );
    }

    #[test]
    fn eir_hex_is_lowercase_no_separators() {
        let hex = eir_hex("CarLink");
        assert!(hex
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(hex.len(), eir_bytes("CarLink").len() * 2);
    }
}
