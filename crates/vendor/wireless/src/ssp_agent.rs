//! Bluetooth Secure Simple Pairing auto-accept agent -- a port of `carlink_linux`'s proven
//! `ssp_agent.c` (live-verified against a real iPhone by that project). Talks directly to BlueZ's
//! kernel mgmt control channel over a raw `AF_BLUETOOTH`/`BTPROTO_HCI` socket, hand-rolling the
//! wire structs exactly as the C reference does specifically to avoid a `libbluetooth`/`bluer`
//! dependency for this phase (`bluer` is deferred per the reference project's own docs).
//!
//! Sets IO capability to NoInputNoOutput, which per the Secure Simple Pairing spec forces the
//! Just Works association model -- no PIN/passkey ever needs to be shown or typed on either side.
//!
//! ⚠️ Exact mgmt event/command parameter byte layouts below follow the standard documented BlueZ
//! mgmt API and the reference's own confirmed opcodes, but (like all of this phase's raw HCI work)
//! this is inherently live-hardware integration and must be validated on real hardware. Byte offsets
//! here should be treated as the best-effort starting point to test against the real Pi, not as
//! independently re-derived/verified in this pass.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::sync::atomic::{AtomicBool, Ordering};

const AF_BLUETOOTH: libc::sa_family_t = 31;
const BTPROTO_HCI: libc::c_int = 1;
const HCI_DEV_NONE: u16 = 0xffff;
const HCI_CHANNEL_CONTROL: u16 = 3;

#[repr(C)]
struct SockaddrHci {
    hci_family: libc::sa_family_t,
    hci_dev: u16,
    hci_channel: u16,
}

// mgmt command opcodes (BlueZ kernel mgmt API).
const OP_SET_POWERED: u16 = 0x0005;
const OP_SET_CONNECTABLE: u16 = 0x0007;
const OP_SET_BONDABLE: u16 = 0x0009;
const OP_SET_SSP: u16 = 0x000B;
const OP_SET_IO_CAPABILITY: u16 = 0x0018;
const OP_PIN_CODE_REPLY: u16 = 0x0016;
const OP_USER_CONFIRM_REPLY: u16 = 0x001C;
const OP_USER_PASSKEY_REPLY: u16 = 0x001E;
const OP_LOAD_LINK_KEYS: u16 = 0x0012;

// mgmt event codes (BlueZ mgmt-api.txt). NB: 0x0006 is "New Settings", NOT New Link Key — the New Link
// Key event is 0x0009 (the earlier 0x0006 was wrong, so persistence never fired; caught on-device).
const EV_NEW_LINK_KEY: u16 = 0x0009;
const EV_PIN_CODE_REQUEST: u16 = 0x000E;
const EV_USER_CONFIRM_REQUEST: u16 = 0x000F;
const EV_USER_PASSKEY_REQUEST: u16 = 0x0010;
// mgmt command-result events — read after each setup command so a rejected setting (e.g. SET_SSP being
// refused on the IW416, which silently left "Simple Pairing mode: Disabled" and degraded pairing to
// legacy PIN) is VISIBLE instead of fire-and-forgotten. `[opcode u16][status u8][…]`.
const EV_CMD_COMPLETE: u16 = 0x0001;
const EV_CMD_STATUS: u16 = 0x0002;

// Connection/auth lifecycle events. These are NOT acted on — they are logged because they are the only
// window into what the controller is actually doing during a failed pairing. On this 3.14.52 kernel a
// successful Just-Works bond auto-accepts User Confirmation IN-KERNEL for NoInputNoOutput and never
// calls mgmt_user_confirm_request, so "no EV_USER_CONFIRM_REQUEST" is EXPECTED even on success and
// proves nothing on its own; DEVICE_CONNECTED/DISCONNECTED and AUTH_FAILED are the real signal.
const EV_DEVICE_CONNECTED: u16 = 0x000B;
const EV_DEVICE_DISCONNECTED: u16 = 0x000C;
const EV_CONNECT_FAILED: u16 = 0x000D;
const EV_AUTH_FAILED: u16 = 0x0011;

/// Human-readable name for a mgmt event code (BlueZ mgmt-api.txt), for the diagnostic fallback below.
fn mgmt_event_name(ev: u16) -> &'static str {
    match ev {
        0x0001 => "CMD_COMPLETE",
        0x0002 => "CMD_STATUS",
        0x0003 => "CONTROLLER_ERROR",
        0x0004 => "INDEX_ADDED",
        0x0005 => "INDEX_REMOVED",
        0x0006 => "NEW_SETTINGS",
        0x0007 => "CLASS_OF_DEV_CHANGED",
        0x0008 => "LOCAL_NAME_CHANGED",
        0x0009 => "NEW_LINK_KEY",
        0x000A => "NEW_LONG_TERM_KEY",
        0x000B => "DEVICE_CONNECTED",
        0x000C => "DEVICE_DISCONNECTED",
        0x000D => "CONNECT_FAILED",
        0x000E => "PIN_CODE_REQUEST",
        0x000F => "USER_CONFIRM_REQUEST",
        0x0010 => "USER_PASSKEY_REQUEST",
        0x0011 => "AUTH_FAILED",
        0x0012 => "DEVICE_FOUND",
        0x0013 => "DISCOVERING",
        0x0014 => "DEVICE_BLOCKED",
        0x0015 => "DEVICE_UNBLOCKED",
        0x0016 => "DEVICE_UNPAIRED",
        0x0017 => "PASSKEY_NOTIFY",
        _ => "UNKNOWN",
    }
}

/// Format a bdaddr (little-endian on the wire) as the conventional big-endian colon form.
fn fmt_bdaddr(b: &[u8]) -> String {
    if b.len() < 6 {
        return "??:??:??:??:??:??".into();
    }
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        b[5], b[4], b[3], b[2], b[1], b[0]
    )
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

/// Persistent BR/EDR link-key store (#152). On a persistent path (JFFS2 `/etc`, matching the
/// supervisor's reboot-budget file) so a bonded iPhone survives a reboot/daemon-restart — without it,
/// the kernel loses the bond on restart and the iPhone's reconnect fails BR/EDR auth (AUTH_FAILED),
/// forcing a manual "forget + re-pair". `/var/lib` (the BlueZ default) can be a tmpfs symlink on this
/// box and would defeat the purpose. One 25-byte record per bond: `[bdaddr 6][addr_type 1][key_type 1]
/// [value 16][pin_length 1]` — the exact mgmt Load-Link-Keys key layout.
/// Directory holding persistent Bluetooth state. Overridable via `CARPLAY_STATE_DIR`: on the
/// Raspberry Pi port `/etc` is a symlink into the read-mostly `/system` partition, so bonds belong
/// under `/data`. Unset keeps the CCPA's `/etc/carplay` exactly as before.
pub(crate) fn state_dir() -> &'static str {
    static D: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    D.get_or_init(|| {
        std::env::var("CARPLAY_STATE_DIR").unwrap_or_else(|_| "/etc/carplay".to_string())
    })
}

fn link_key_store() -> &'static str {
    static P: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    P.get_or_init(|| format!("{}/bt_link_keys", state_dir()))
}
const LINK_KEY_RECORD_LEN: usize = 25;

/// Serializes the read-modify-write of the link-key store.
///
/// ⚠️ PI-VERIFIED ONLY (2026-08-16). The race this closes is platform-independent and the fix is
/// the conservative direction (a lock plus a pid-unique temp name), but the persistence path it
/// guards is how a CCPA remembers a paired phone across reboots. Not run on CCPA hardware.
///
/// `persist_link_key` runs on the mgmt event thread; `forget_bond` runs on the control-socket
/// thread. Both read the WHOLE file, edit, and write it back — so without this a Forget that starts
/// before a NEW_LINK_KEY lands rewrites the store from a snapshot taken before it and deletes the
/// just-paired phone's key. That phone then fails BR/EDR auth on its next connect with no local
/// explanation at all.
static STORE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Publish the whole store atomically. Caller MUST hold [`STORE_LOCK`].
///
/// The temp name carries the pid because `persist` and `forget` previously used the SAME
/// `<store>.tmp`: two writers could interleave bytes into one temp file and then rename THAT into
/// place, publishing a store that is half one write and half the other. The lock covers this
/// process; the pid covers a second `carplay-wireless` overlapping during a restart.
fn write_store(bytes: &[u8]) -> bool {
    if std::fs::create_dir_all(state_dir()).is_err() {
        return false;
    }
    let tmp = format!("{}.tmp.{}", link_key_store(), std::process::id());
    if std::fs::write(&tmp, bytes)
        .and_then(|_| std::fs::rename(&tmp, link_key_store()))
        .is_err()
    {
        // Never leave a partial file beside the live store.
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    true
}

/// Read the stored link-key records (a flat sequence of 25-byte records; empty/absent → none).
fn load_stored_link_keys() -> Vec<u8> {
    match std::fs::read(link_key_store()) {
        Ok(b) if b.len() >= LINK_KEY_RECORD_LEN => {
            let usable = (b.len() / LINK_KEY_RECORD_LEN) * LINK_KEY_RECORD_LEN; // drop any partial tail
            b[..usable].to_vec()
        }
        _ => Vec::new(),
    }
}

/// The bdaddrs of every bonded phone (Model B reconnect, docs/wireless/01_BT_AND_RADIO.md). Returns each record's 6-byte
/// bdaddr in the stored mgmt little-endian order — the exact order `rfcomm::connect_to` /
/// `sdp_client::query` want for their sockaddrs, so no reversal. Empty when nothing is bonded, which
/// makes the reconnect orchestrator idle (it re-checks on a slow interval; audit Fix #22).
pub fn bonded_addrs() -> Vec<[u8; 6]> {
    load_stored_link_keys()
        .chunks_exact(LINK_KEY_RECORD_LEN)
        .map(|r| {
            let mut a = [0u8; 6];
            a.copy_from_slice(&r[0..6]);
            a
        })
        .collect()
}

/// Persist one link-key record, replacing any existing record for the same bdaddr (a re-pair supersedes
/// the old key). Atomic (`.tmp` + rename). Best-effort: a write failure just means this bond won't
/// survive a reboot (logged), never a crash.
fn persist_link_key(record: &[u8; LINK_KEY_RECORD_LEN]) {
    // Poison is not a reason to skip persistence: the state this guards is a FILE, so a panicking
    // writer cannot have left a half-updated value behind the lock.
    let _guard = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Read INSIDE the lock — that is the entire point. The previous code read, edited and wrote
    // with no mutual exclusion against forget_bond doing the same.
    let existing = load_stored_link_keys();
    let mut keys = Vec::with_capacity(existing.len() + LINK_KEY_RECORD_LEN);
    for chunk in existing.chunks_exact(LINK_KEY_RECORD_LEN) {
        // Drop any existing record for this bdaddr — a re-pair supersedes the old key.
        if chunk[0..6] != record[0..6] {
            keys.extend_from_slice(chunk);
        }
    }
    keys.extend_from_slice(record);
    if write_store(&keys) {
        log(&format!(
            "link key persisted ({} bond(s) stored)",
            keys.len() / LINK_KEY_RECORD_LEN
        ));
    } else {
        log("link-key persist: write failed (non-fatal — bond won't survive reboot)");
    }
}

/// Drop the stored bond for `addr` — the "Forget this phone" action from the projection app's device
/// screen (`control::handle_request`). `addr` is in the same mgmt little-endian order
/// [`bonded_addrs`] returns.
///
/// Returns false when nothing was stored for that address, so the caller can report "not bonded"
/// rather than a success that removed nothing.
///
/// **This removes only the PERSISTED key, not any live pairing in the controller.** The controller
/// forgets on its own at power-down, and the next bring-up re-pairs from scratch because there is no
/// stored key to offer — which is what "forget" means here. Removing it from the controller as well
/// would need a mgmt `Unpair Device`, and this process deliberately talks raw HCI rather than mgmt
/// (mgmt cannot express the CarPlay EIR marker, which is why `hci.rs` exists at all).
pub fn forget_bond(addr: &[u8; 6]) -> bool {
    let _guard = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let keys = load_stored_link_keys();
    let mut out = Vec::with_capacity(keys.len());
    let mut removed = false;
    for chunk in keys.chunks_exact(LINK_KEY_RECORD_LEN) {
        if &chunk[0..6] == addr.as_slice() {
            removed = true;
        } else {
            out.extend_from_slice(chunk);
        }
    }
    if !removed {
        return false;
    }
    if !write_store(&out) {
        log("forget: write failed — bond not removed");
        return false;
    }
    log(&format!(
        "bond forgotten ({} bond(s) remain)",
        out.len() / LINK_KEY_RECORD_LEN
    ));
    true
}

// SSP IO capabilities → association model (against iOS's KeyboardDisplay):
//   NoInputNoOutput (0x03) → Just-Works — no code shown; the PROVEN Carlinkit-CCPA posture (default).
//   DisplayYesNo    (0x01) → Numeric Comparison — both sides show a 6-digit code to confirm; a more
//                            OEM-head-unit-like experience (config-selectable, EXPERIMENTAL for a dongle).
const IO_CAP_NO_INPUT_NO_OUTPUT: u8 = 0x03;
const IO_CAP_DISPLAY_YES_NO: u8 = 0x01;

/// Cross-process flag: the current Numeric-Comparison 6-digit code (or absent = no code / Just-Works).
/// ocbmd watches it and relays the value to the host app over CH_CTRL so it can display it in the status
/// area for the user to match against the iPhone (same file-flag → ocbmd → CT_* pattern as host_present).
const PAIRING_CODE_FLAG: &str = "/tmp/pairing_code";

/// Select the SSP IO capability from `CARPLAY_PAIRING_MODE` (set per-config by the supervisor from the
/// host YAML `pairing:` field). Returns `(io_cap, is_numeric)`. Default = Just-Works (the proven default).
fn pairing_mode_io_cap() -> (u8, bool) {
    match std::env::var("CARPLAY_PAIRING_MODE").as_deref() {
        Ok("numeric") | Ok("numeric_comparison") => (IO_CAP_DISPLAY_YES_NO, true),
        _ => (IO_CAP_NO_INPUT_NO_OUTPUT, false),
    }
}

/// Publish the Numeric-Comparison code for the app to display (best-effort; a write failure just means
/// the code isn't shown, never a crash). `clear_pairing_code` removes it once pairing completes.
fn write_pairing_code(code: &str) {
    let _ = std::fs::write(PAIRING_CODE_FLAG, code);
}
fn clear_pairing_code() {
    let _ = std::fs::remove_file(PAIRING_CODE_FLAG);
}

fn log(m: &str) {
    println!("[ssp-agent] {m}");
}

/// Send a setup mgmt command and READ its completion, logging the status. Serializes setup (so e.g.
/// SET_SSP can't race an in-flight power change) and — critically — makes a rejected setting observable
/// instead of the old fire-and-forget that silently left SSP disabled. Best-effort: a missing response
/// (timeout) or an error is logged, never fatal. Bounded so an unrelated event stream can't wedge setup.
fn apply_setting(sock: &mut File, opcode: u16, index: u16, params: &[u8], name: &str) {
    let cmd = build_cmd(opcode, index, params);
    if let Err(e) = sock.write_all(&cmd) {
        log(&format!("{name}: send failed (non-fatal): {e}"));
        return;
    }
    let mut buf = [0u8; 512];
    for _ in 0..8 {
        let n = match sock.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                log(&format!("{name}: no mgmt response (timeout, non-fatal)"));
                return;
            }
            Err(_) => return,
        };
        if n < 6 {
            continue;
        }
        let ev = u16::from_le_bytes([buf[0], buf[1]]);
        let plen = u16::from_le_bytes([buf[4], buf[5]]) as usize;
        if n < 6 + plen || plen < 3 {
            continue;
        }
        let ev_opcode = u16::from_le_bytes([buf[6], buf[7]]);
        let status = buf[8];
        if (ev == EV_CMD_COMPLETE || ev == EV_CMD_STATUS) && ev_opcode == opcode {
            if status == 0 {
                log(&format!("{name}: ok"));
            } else {
                log(&format!("{name}: mgmt status 0x{status:02x} (rejected — non-fatal)"));
            }
            return;
        }
        // else: an unrelated async event (e.g. NEW_SETTINGS) — keep reading for our completion.
    }
}

/// Build one mgmt command frame: `[opcode LE16][controller index LE16][param len LE16][params]`.
fn build_cmd(opcode: u16, index: u16, params: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(6 + params.len());
    buf.extend_from_slice(&opcode.to_le_bytes());
    buf.extend_from_slice(&index.to_le_bytes());
    buf.extend_from_slice(&(params.len() as u16).to_le_bytes());
    buf.extend_from_slice(params);
    buf
}

fn open_mgmt_socket() -> std::io::Result<File> {
    // SOCK_CLOEXEC — this HCI mgmt socket is live across av::ensure_av_layer()'s fork+exec of the
    // detached daemons (av.rs:13); see sdp_server::open_l2cap_listener for the full reasoning.
    let fd = unsafe {
        libc::socket(
            AF_BLUETOOTH as libc::c_int,
            libc::SOCK_RAW | crate::cloexec::SOCK_CLOEXEC,
            BTPROTO_HCI,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let addr = SockaddrHci {
        hci_family: AF_BLUETOOTH,
        hci_dev: HCI_DEV_NONE,
        hci_channel: HCI_CHANNEL_CONTROL,
    };
    let ret = unsafe {
        libc::bind(
            fd,
            &addr as *const SockaddrHci as *const libc::sockaddr,
            std::mem::size_of::<SockaddrHci>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // A receive timeout (rather than a blocking read with no way to interrupt it) so `run`'s
    // shutdown check actually gets a chance to fire promptly -- mirrors this project's established
    // discipline around not trusting a single blocking syscall to eventually return (see
    // rust/carplayd/src/iap2/driver.rs's `write_interruptible` doc for the exact incident that
    // discipline comes from).
    // zeroed()+assign, not a struct literal: under `musl32_time64` (riscv32) these
    // types carry private padding and a literal does not compile.
    let mut timeout: libc::timeval = unsafe { std::mem::zeroed() };
    timeout.tv_sec = 1;
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &timeout as *const libc::timeval as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // SAFETY: fd is a freshly opened, bound, exclusively-owned raw socket descriptor -- File takes
    // ownership and will close it on drop.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Bring `controller_index` (typically 0 for `hci0`) up with SSP enabled and IO capability set to
/// force Just Works, then poll until `shutdown` is set. Best-effort: each setup command's failure is
/// logged, not fatal -- `bt_bringup::bring_up`'s own `hciconfig` calls may have already configured
/// some of this.
///
/// CORRECTION (2026-07-25): this used to claim it "auto-accepts every pairing confirmation". On this
/// box's 3.14.52 kernel a Just-Works bond with NoInputNoOutput is auto-accepted IN-KERNEL and
/// `mgmt_user_confirm_request` is never called, so `EV_USER_CONFIRM_REQUEST` does NOT arrive on a
/// successful pairing — its absence is expected and proves nothing. In practice this agent is: the
/// mgmt settings setup, the NEW_LINK_KEY persistence path, the Numeric-Comparison code publisher, and
/// (since 2026-07-25) the connection/auth event logger. The auto-accept arms only fire in
/// Numeric-Comparison mode or on a legacy-PIN fallback.
pub fn run(controller_index: u16, shutdown: &AtomicBool) -> std::io::Result<()> {
    let mut sock = open_mgmt_socket()?;

    // Load persisted link keys FIRST (#152) so a previously-bonded iPhone re-authenticates after a
    // reboot/restart instead of failing BR/EDR auth and demanding a re-pair. LOAD_LINK_KEYS params =
    // `[debug_keys u8=0][key_count u16 LE][records…]`; each record is the 25-byte store layout.
    let stored = load_stored_link_keys();
    let count = (stored.len() / LINK_KEY_RECORD_LEN) as u16;
    let mut load_params = Vec::with_capacity(3 + stored.len());
    load_params.push(0u8); // debug_keys = false
    load_params.extend_from_slice(&count.to_le_bytes());
    load_params.extend_from_slice(&stored);
    // QC 2026-07-25: read the completion status instead of firing and forgetting. A rejected load was
    // previously INVISIBLE, and it matters twice over: the kernel validates every record's addr_type
    // byte (must be 0x00 = BDADDR_BREDR) and returns MGMT_STATUS_INVALID_PARAMS on the first bad one —
    // and a rejected LOAD_LINK_KEYS also SKIPS the kernel's own `hci_link_keys_clear`, so stale kernel
    // keys survive and a fresh pairing can then fail BR/EDR auth with no local explanation.
    apply_setting(
        &mut sock,
        OP_LOAD_LINK_KEYS,
        controller_index,
        &load_params,
        &format!("LOAD_LINK_KEYS(count={count})"),
    );
    if count > 0 {
        log(&format!(
            "loaded {count} persisted link key(s) — bonded phones can reconnect without re-pair"
        ));
    }

    let (io_cap, numeric) = pairing_mode_io_cap();
    clear_pairing_code(); // drop any stale code from a prior session

    // Setup commands, each with its mgmt completion read back (see apply_setting). SET_SSP is the
    // load-bearing one — if the controller rejects it, pairing degrades to legacy PIN and iOS refuses
    // CarPlay. bt_bringup also forces `hciconfig sspmode 1` so SSP is on even if this mgmt path is quirky.
    apply_setting(&mut sock, OP_SET_POWERED, controller_index, &[1], "SET_POWERED=1");
    apply_setting(&mut sock, OP_SET_BONDABLE, controller_index, &[1], "SET_BONDABLE=1");
    apply_setting(&mut sock, OP_SET_CONNECTABLE, controller_index, &[1], "SET_CONNECTABLE=1");
    apply_setting(&mut sock, OP_SET_SSP, controller_index, &[1], "SET_SSP=1");
    apply_setting(&mut sock, OP_SET_IO_CAPABILITY, controller_index, &[io_cap], "SET_IO_CAPABILITY");

    if numeric {
        log("pairing mode: NUMERIC COMPARISON (DisplayYesNo) — a 6-digit code will be shown to match");
    } else {
        log("pairing mode: Just-Works (NoInputNoOutput) — no code (proven CCPA default)");
    }
    log("auto-accept pairing agent running");
    let mut buf = [0u8; 512];
    loop {
        if shutdown.load(Ordering::Relaxed) {
            clear_pairing_code(); // don't leave a stale code showing after teardown
            return Ok(());
        }
        let n = match sock.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        };
        if n < 6 {
            continue;
        }
        let event = u16::from_le_bytes([buf[0], buf[1]]);
        let index = u16::from_le_bytes([buf[2], buf[3]]);
        let param_len = u16::from_le_bytes([buf[4], buf[5]]) as usize;
        if n < 6 + param_len {
            continue; // truncated read -- ignore rather than panic
        }
        let params = &buf[6..6 + param_len];
        // QC 2026-07-25: bdaddr/addr_type are now derived per-arm rather than up front. The old code
        // pre-filtered `param_len < 6` here, which silently discarded every short event (CMD_COMPLETE,
        // DISCOVERING, ...) BEFORE the match could see it -- part of why this loop appeared to never
        // fire at all during the 2026-07-24 pairing failure.
        let addr = |p: &[u8]| -> Option<([u8; 6], u8)> {
            if p.len() >= 7 {
                let mut a = [0u8; 6];
                a.copy_from_slice(&p[0..6]);
                Some((a, p[6]))
            } else {
                None
            }
        };

        match event {
            EV_NEW_LINK_KEY => {
                // The kernel just created a bond. Persist the key (#152) so it survives a reboot. Event
                // params = `[store_hint u8][record: bdaddr 6][addr_type 1][key_type 1][value 16]
                // [pin_length 1]` = 1 + 25 bytes; store the 25-byte record verbatim (it IS the mgmt
                // Load-Link-Keys layout). store_hint==0 means "don't persist" — honor it.
                if param_len > LINK_KEY_RECORD_LEN {
                    // (store_hint byte + the 25-byte record)
                    let store_hint = params[0];
                    if store_hint != 0 {
                        let mut record = [0u8; LINK_KEY_RECORD_LEN];
                        record.copy_from_slice(&params[1..1 + LINK_KEY_RECORD_LEN]);
                        persist_link_key(&record);
                    }
                }
                clear_pairing_code(); // bond formed → pairing complete → app hides the code
            }
            EV_USER_CONFIRM_REQUEST => {
                // Numeric Comparison: the 6-digit value is `[value u32 LE]` at param offset 8 (after
                // bdaddr 6 + addr_type 1 + confirm_hint 1). Publish it for the app to display, then
                // auto-accept — the box "agrees" and the user verifies the match visually + taps Pair on
                // the iPhone. Just-Works: no meaningful value (confirm_hint=1); just auto-accept.
                if numeric && param_len >= 12 {
                    let value =
                        u32::from_le_bytes([params[8], params[9], params[10], params[11]]);
                    let code = format!("{:06}", value % 1_000_000);
                    write_pairing_code(&code);
                    log(&format!(
                        "numeric-comparison code = {code} — confirm it matches the iPhone (auto-accepting)"
                    ));
                } else {
                    log("auto-confirming pairing request (Just Works)");
                }
                if let Some((bdaddr, addr_type)) = addr(params) {
                    let mut reply = Vec::with_capacity(7);
                    reply.extend_from_slice(&bdaddr);
                    reply.push(addr_type);
                    let cmd = build_cmd(OP_USER_CONFIRM_REPLY, index, &reply);
                    let _ = sock.write_all(&cmd);
                }
            }
            EV_USER_PASSKEY_REQUEST => {
                log("auto-replying passkey 0");
                if let Some((bdaddr, addr_type)) = addr(params) {
                    let mut reply = Vec::with_capacity(11);
                    reply.extend_from_slice(&bdaddr);
                    reply.push(addr_type);
                    reply.extend_from_slice(&0u32.to_le_bytes());
                    let cmd = build_cmd(OP_USER_PASSKEY_REPLY, index, &reply);
                    let _ = sock.write_all(&cmd);
                }
            }
            EV_PIN_CODE_REQUEST => {
                log("auto-replying PIN 0000");
                if let Some((bdaddr, addr_type)) = addr(params) {
                    let pin = b"0000";
                    let mut reply = Vec::with_capacity(6 + 1 + 1 + 16);
                    reply.extend_from_slice(&bdaddr);
                    reply.push(addr_type);
                    reply.push(pin.len() as u8);
                    reply.extend_from_slice(pin);
                    reply.resize(6 + 1 + 1 + 16, 0); // pin_code is a fixed 16-byte field, zero-padded
                    let cmd = build_cmd(OP_PIN_CODE_REPLY, index, &reply);
                    let _ = sock.write_all(&cmd);
                }
            }

            // QC 2026-07-25: connection/auth lifecycle — logged, never acted on. These are the events
            // that were previously invisible and are the discriminating evidence during a failed
            // pairing: DEVICE_CONNECTED/DISCONNECTED alone (phone connects for SDP, then leaves) points
            // at the controller event mask; AUTH_FAILED points at a stale/mismatched link key.
            EV_DEVICE_CONNECTED => log(&format!(
                "DEVICE_CONNECTED {}",
                addr(params).map_or("(short)".into(), |(b, _)| fmt_bdaddr(&b))
            )),
            EV_DEVICE_DISCONNECTED => log(&format!(
                "DEVICE_DISCONNECTED {} reason={}",
                addr(params).map_or("(short)".into(), |(b, _)| fmt_bdaddr(&b)),
                params.get(7).map_or("?".to_string(), |r| format!("0x{r:02x}"))
            )),
            EV_CONNECT_FAILED => log(&format!(
                "CONNECT_FAILED {} status={}",
                addr(params).map_or("(short)".into(), |(b, _)| fmt_bdaddr(&b)),
                params.get(7).map_or("?".to_string(), |s| format!("0x{s:02x}"))
            )),
            EV_AUTH_FAILED => log(&format!(
                "AUTH_FAILED {} status={} — a stale/mismatched link key is the usual cause; \
                 check {} and the iPhone's Settings > General > CarPlay list",
                addr(params).map_or("(short)".into(), |(b, _)| fmt_bdaddr(&b)),
                params.get(7).map_or("?".to_string(), |s| format!("0x{s:02x}")),
                link_key_store()
            )),

            // QC 2026-07-25: the old `_ => {}` silently dropped EVERY unrecognized mgmt event with zero
            // logging, which is the sole reason the 2026-07-24 "Pairing Unsuccessful" failure was
            // undiagnosable from the box side. Log the raw event/index/params for anything unhandled.
            _ => log(&format!(
                "mgmt event 0x{event:04x} {} idx={index} len={param_len} [{}]",
                mgmt_event_name(event),
                hex(params)
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_cmd_matches_mgmt_header_layout() {
        let cmd = build_cmd(OP_SET_POWERED, 0, &[1]);
        assert_eq!(cmd, vec![0x05, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01]);
    }

    #[test]
    fn build_cmd_with_empty_params() {
        let cmd = build_cmd(OP_SET_SSP, 2, &[]);
        assert_eq!(cmd, vec![0x0B, 0x00, 0x02, 0x00, 0x00, 0x00]);
    }
}
