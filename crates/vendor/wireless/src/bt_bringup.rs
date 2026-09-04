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
    //
    // Truncated to fit the AD length byte rather than cast to it: a name long enough to wrap
    // `len + 2` past 255 would declare a length that disagrees with the bytes that follow, and the
    // controller parses the rest of the EIR from that number. `hci::write_eir` rejects > 240 bytes
    // total, so the cut is at the smaller of the two limits.
    // 240 total, less this AD's len+type+NUL (3) and the two 18-byte UUID ADs below.
    const MAX_NAME: usize = 240 - 3 - 2 * 18;
    let mut end = name.len().min(MAX_NAME);
    while end > 0 && !name.is_char_boundary(end) {
        end -= 1;
    }
    let name_bytes = &name.as_bytes()[..end];
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
    // Captured (not inherited) so a failure's stderr lands in the SAME log line as the argv and exit
    // status -- `hciconfig`'s own stderr is a few words with no context, and split across a raw
    // process's inherited stdio it is unattributable once other daemons are logging concurrently.
    let out = Command::new("hciconfig").args(args).output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("[carplay-wireless] hciconfig {args:?} failed: status={} stderr={stderr:?}", out.status);
        return Err(std::io::Error::other(format!(
            "hciconfig {args:?} failed: {}", out.status
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

/// Re-apply this unit's own post-attach SCO setup through the radio seam, after the DOWN->UP cycle
/// above has reset it away.
///
/// BEST-EFFORT BY DESIGN, on every axis:
///   * A unit whose vendor branch carries no SCO mapping exits 3 (`unsupported`) and that is a
///     finding, not a failure -- CarPlay does not need SCO and Android Auto degrades to no call
///     audio, which is strictly better than refusing to bring Bluetooth up at all.
///   * The seam is absent entirely on the Pi/AAOS port and on a dev host. Missing script, missing
///     `sh`, non-zero exit: all logged, none fatal.
///
/// Never composes the commands itself. `hcitool -i hci0 cmd 0x3f 0x1c 0x01 0x02 0x00 0x00 0x00` is
/// BCM4358's and `0x3f 0x1d 0x00` is NXP's; firing either at the wrong controller is the exact
/// class of mistake the seam exists to make impossible, so the choice stays in `radio_detect.sh`,
/// which reads it out of the unit's own `attach_bluetooth.sh`.
fn restore_sco_setup() {
    const HAL: &str = "/script/radio_hal.sh";
    if !std::path::Path::new(HAL).exists() {
        return; // not a CCPA rootfs (Pi/AAOS port, or a dev host) -- nothing to restore
    }
    // BOUNDED, and that bound is load-bearing. `sco_on` takes radio_hal's per-subsystem `bt` lock,
    // and `bt_on` can legitimately hold that lock for MINUTES against a wedged controller (its
    // convergence poll is 30 × `timeout 5` across 4 attempts). `lock_take` gives up after 30 s, so
    // an unbounded call here would stall `bring_up` — which runs once per active session, on the
    // session's critical path — for half a minute behind a bring-up that is already doing the work.
    // `timeout` is present on this platform (radio_hal.sh itself uses it); if it somehow is not,
    // fall back to a direct call rather than silently skipping the restore.
    let run = Command::new("timeout").args(["20", "sh", HAL, "sco_on"]).output();
    let run = match run {
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => {
            Command::new("sh").arg(HAL).arg("sco_on").output()
        }
        other => other,
    };
    match run {
        Ok(out) => {
            let code = out.status.code().unwrap_or(-1);
            // The seam logs its own detail to stdout; carry the one-line verdict so a box.log
            // reader does not have to correlate two files.
            let detail = String::from_utf8_lossy(&out.stdout);
            let last = detail.lines().last().unwrap_or("").trim();
            match code {
                0 => eprintln!("[bt-bringup] SCO setup restored after the down/up cycle ({last})"),
                3 => eprintln!(
                    "[bt-bringup] no SCO mapping for this unit -- HFP call audio will not work ({last})"
                ),
                124 => eprintln!(
                    "[bt-bringup] radio_hal.sh sco_on timed out after 20s (the bt lock is held by a \
                     concurrent bring-up) -- HFP call audio may be silent this session"
                ),
                _ => eprintln!("[bt-bringup] radio_hal.sh sco_on exited {code} ({last})"),
            }
        }
        Err(e) => eprintln!("[bt-bringup] could not run {HAL} sco_on: {e} (HFP call audio may be silent)"),
    }
}

/// Bring the controller up as a discoverable/connectable CarPlay-style accessory: set class, name,
/// EIR, then enable page+inquiry scan (discoverable + connectable, no timeout).
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
    // KNOWN SIDE EFFECT, NOW REPAIRED (2026-09-03): this controller is `hciattach`'d over UART, so it
    // carries HCI_QUIRK_RESET_ON_CLOSE -- the `down` makes the kernel issue a real HCI_Reset on close,
    // and the `up` re-reads Read_Buffer_Size. That discards `attach_bluetooth.sh`'s HFP setup (its
    // `hciconfig scomtu 240:32` plus the vendor `hcitool` SCO-to-HCI routing command where its chipset
    // branch has one).
    //
    // This comment used to end "deliberately NOT re-issued here: CarPlay carries ALL audio --
    // including telephony -- over the AirPlay/WiFi session, never over SCO/HFP". That reasoning was
    // sound and is now OBSOLETE: wireless ANDROID AUTO carries call and Assistant audio over
    // Bluetooth HFP/SCO (`sco_audio`), so the discarded setup is load-bearing again. It is restored
    // by `restore_sco_setup` below -- through the radio seam, never by this daemon composing raw HCI,
    // because which commands a unit needs is a per-chipset fact only that unit's own dispatcher
    // knows (docs/wireless/01_BT_AND_RADIO.md, the no-chipset-whitelist rule).
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
        restore_sco_setup();
        eprintln!("[carplay-wireless] HCI bring-up OK dev={hci_dev} name={name}");
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
    restore_sco_setup();
    eprintln!("[carplay-wireless] HCI bring-up OK dev={hci_dev} name={name}");
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

    /// The AD length byte must always describe the bytes that actually follow it: a name long
    /// enough to overflow it would desync the controller's parse of the rest of the EIR.
    #[test]
    fn an_overlong_name_is_truncated_not_wrapped() {
        let eir = eir_bytes(&"x".repeat(400));
        assert_eq!(eir[0] as usize, MAX_NAME_FOR_TEST + 2);
        assert_eq!(eir[1], 0x09);
        assert_eq!(eir[2 + MAX_NAME_FOR_TEST], 0x00, "NUL must follow the truncated name");
        assert!(eir.len() <= 240, "EIR is {} bytes", eir.len());
    }

    /// Mirrors `eir_bytes`'s own cap; kept here so a change to one fails the other.
    const MAX_NAME_FOR_TEST: usize = 240 - 3 - 2 * 18;

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
