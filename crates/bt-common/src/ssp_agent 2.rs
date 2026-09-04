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
use std::sync::atomic::{AtomicBool, AtomicI8, Ordering};

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
const OP_SET_DISCOVERABLE: u16 = 0x0006; // [discoverable u8: 0 off / 1 general / 2 limited][timeout u16 LE, 0 = no timeout]
const OP_SET_CONNECTABLE: u16 = 0x0007;
const OP_SET_BONDABLE: u16 = 0x0009;
const OP_SET_SSP: u16 = 0x000B;
const OP_SET_IO_CAPABILITY: u16 = 0x0018;
const OP_PIN_CODE_REPLY: u16 = 0x0016;
const OP_USER_CONFIRM_REPLY: u16 = 0x001C;
const OP_USER_CONFIRM_NEG_REPLY: u16 = 0x001D;
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

/// Last two octets of a bdaddr (mgmt little-endian on the wire, i.e. `b[0]`/`b[1]` are the
/// least-significant bytes — the conventional "end" of the address) for log lines. Global rule:
/// never write a full device address into durable output; this is the identifying-enough remainder.
fn fmt_bdaddr_tail(b: &[u8]) -> String {
    if b.len() < 2 {
        return "??:??".into();
    }
    format!("{:02X}:{:02X}", b[1], b[0])
}

/// Human-readable name for a stored/loaded link-key's `key_type` byte, per the BlueZ mgmt API
/// (mgmt-api.txt "Link Key Type" values used by both `Load Link Keys` and `New Link Key`).
fn key_type_name(kt: u8) -> &'static str {
    match kt {
        0x00 => "combination",
        0x03 => "debug",
        0x04 => "unauth_p192",
        0x05 => "auth_p192",
        0x06 => "changed",
        0x07 => "unauth_p256",
        0x08 => "auth_p256",
        _ => "unknown",
    }
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
// `pub` rather than `pub(crate)` since the extraction into bt-common: carplay-wireless's
// control.rs sites its `projection_policy.json` alongside the link-key store, and that call
// now crosses a crate boundary.
pub fn state_dir() -> &'static str {
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
    use std::io::Write;
    if std::fs::create_dir_all(state_dir()).is_err() {
        return false;
    }
    let tmp = format!("{}.tmp.{}", link_key_store(), std::process::id());
    // fsync before rename: std::fs::write returns once the page cache accepts the bytes, so on an
    // abrupt power loss (the normal shutdown of a car accessory) the rename can be durable while the
    // new file's contents are not, yielding an empty/truncated store on the next boot.
    let result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, link_key_store())
    })();
    if result.is_err() {
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
/// would need a mgmt `Unpair Device` (0x001B); this module IS a mgmt client (`HCI_CHANNEL_CONTROL`,
/// `open_mgmt_socket`) elsewhere, but `forget_bond` runs on the control-socket thread and has no
/// handle to the mgmt socket owned by `run`'s event loop, so it cannot send that command from here.
/// The minimal way to add live unpair would be a channel from `forget_bond` to the event loop.
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

/// `@<unix_ms> ` write-time stamp (docs/carplay/01_OCBM_PROTOCOL.md CH_LOG): the box.log tailer
/// parses this prefix and uses it instead of the millisecond it happened to READ the line at.
fn log(m: &str) {
    println!("@{} [ssp-agent] {m}", now_ms());
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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

/// Rejection cap (2026-09-03 finding): a phone that still holds a stale link key for us rejects a
/// box-initiated numeric-comparison confirm with "No pairing agent" and iOS re-sends
/// USER_CONFIRM_REQUEST rather than ever completing with NEW_LINK_KEY. Auto-accepting forever gives
/// the operator no signal that this is happening, so past `REJECT_WARN_THRESHOLD` confirms on the
/// same connection with no bond formed, we still reply (never break the Just-Works/numeric-comparison
/// contract) but log once and let the caller surface `BTP_PAIR_REJECTED` to the app.
const REJECT_WARN_THRESHOLD: u32 = 3;

/// Per-bdaddr USER_CONFIRM_REQUEST counter for the rejection-cap warning above. Keyed on the 6-byte
/// bdaddr in on-wire (little-endian) order, exactly as it arrives in the mgmt event — callers never
/// need to reverse it. `warned` suppresses repeat log lines/hook calls for the same connection once
/// the threshold has fired; both entries are cleared on DEVICE_DISCONNECTED or NEW_LINK_KEY, i.e.
/// "per connection" as specified.
#[derive(Default)]
struct ConfirmTracker {
    counts: std::collections::HashMap<[u8; 6], u32>,
    warned: std::collections::HashSet<[u8; 6]>,
}

impl ConfirmTracker {
    fn new() -> Self {
        Self::default()
    }

    fn key(addr: &[u8]) -> [u8; 6] {
        let mut a = [0u8; 6];
        a[..addr.len().min(6)].copy_from_slice(&addr[..addr.len().min(6)]);
        a
    }

    /// Record one USER_CONFIRM_REQUEST for `addr` and return `Some(count)` the FIRST time `count`
    /// reaches [`REJECT_WARN_THRESHOLD`] for this connection (i.e. fires exactly once until reset).
    fn record_confirm(&mut self, addr: &[u8]) -> Option<u32> {
        let k = Self::key(addr);
        let count = self.counts.entry(k).or_insert(0);
        *count += 1;
        if *count >= REJECT_WARN_THRESHOLD && self.warned.insert(k) {
            Some(*count)
        } else {
            None
        }
    }

    /// Clear the streak for `addr` — a fresh bond (NEW_LINK_KEY) or a torn-down connection
    /// (DEVICE_DISCONNECTED) both start the next connection's count at zero.
    fn reset(&mut self, addr: &[u8]) {
        let k = Self::key(addr);
        self.counts.remove(&k);
        self.warned.remove(&k);
    }
}

// ---- Numeric Comparison: the head unit's half of the yes/no ----------------------------------

/// The head unit's answer to a Numeric-Comparison prompt, handed to the mgmt loop from another
/// thread.
///
/// Bluetooth SSP Numeric Comparison is defined as a comparison a HUMAN makes on BOTH devices; a box
/// that replies "yes" by itself has an MITM-resistance claim it cannot support (the phone's user sees
/// a code that nobody on this side ever checked). The macOS app now shows the code and two buttons,
/// its answer arrives as `CT_PAIR_CONFIRM` → ocbmd → the wireless daemon's control port
/// (`{"cmd":"pair_answer","accept":…}`) → here.
///
/// One slot, latest-writer-wins, consumed by [`PairAnswer::take`]. It is deliberately NOT a queue:
/// an answer is only ever meaningful for the confirm currently on screen, and a stale one must never
/// be applied to the next pairing attempt (see `PendingConfirmState::arm`, which drains it).
#[derive(Default)]
pub struct PairAnswer(AtomicI8);

const ANSWER_NONE: i8 = 0;
const ANSWER_ACCEPT: i8 = 1;
const ANSWER_CANCEL: i8 = -1;

impl PairAnswer {
    pub const fn new() -> Self {
        Self(AtomicI8::new(ANSWER_NONE))
    }

    /// Record the user's answer. Overwrites an unconsumed one — the newest tap is the intent.
    pub fn set(&self, accept: bool) {
        self.0.store(
            if accept { ANSWER_ACCEPT } else { ANSWER_CANCEL },
            Ordering::Release,
        );
    }

    /// Consume the answer, if any. Never blocks.
    pub fn take(&self) -> Option<bool> {
        match self.0.swap(ANSWER_NONE, Ordering::AcqRel) {
            ANSWER_ACCEPT => Some(true),
            ANSWER_CANCEL => Some(false),
            _ => None,
        }
    }

    /// Drop any unconsumed answer (a fresh confirm request must not inherit the previous one's).
    pub fn clear(&self) {
        self.0.store(ANSWER_NONE, Ordering::Release);
    }
}

/// How long the box waits for the head unit's answer before giving up and replying NO.
///
/// 55 s, inside `rfcomm_uspace::PAIRING_HOLD_SECS` (60): the connect hold must still be running when
/// the negative reply lands, so `pairing_aware_connect` sees `/tmp/pair_rejected` and aborts rather
/// than timing out on its own with the SSP exchange still half-open.
pub const PAIR_CONFIRM_WAIT_SECS: u64 = 55;

/// One outstanding `EV_USER_CONFIRM_REQUEST` we have published a code for and not yet answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingConfirm {
    bdaddr: [u8; 6],
    addr_type: u8,
    /// The controller index the request arrived on — the reply must go back to the SAME controller.
    index: u16,
    deadline_ms: u64,
}

/// The pending-confirm state machine: pure logic, no socket, so it is unit-testable end to end.
///
/// Exactly one confirm can be pending, because exactly one code can be on the app's screen.
#[derive(Default)]
struct PendingConfirmState {
    pending: Option<PendingConfirm>,
}

impl PendingConfirmState {
    fn new() -> Self {
        Self::default()
    }

    fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Arm a wait for `bdaddr`. Returns the confirm this one SUPERSEDES, which the caller must
    /// answer NO — a request we can no longer display a code for is one we cannot honestly accept.
    ///
    /// A REPEAT from the same device (iOS re-sends the request while it waits) keeps the ORIGINAL
    /// deadline: otherwise a phone that re-sends every few seconds would push the deadline past
    /// `PAIRING_HOLD_SECS` forever and the connect hold would expire first.
    fn arm(
        &mut self,
        bdaddr: [u8; 6],
        addr_type: u8,
        index: u16,
        now_ms: u64,
    ) -> Option<PendingConfirm> {
        let mut deadline_ms = now_ms + PAIR_CONFIRM_WAIT_SECS * 1000;
        let superseded = match self.pending.take() {
            Some(prev) if prev.bdaddr == bdaddr => {
                deadline_ms = prev.deadline_ms;
                None
            }
            other => other,
        };
        self.pending = Some(PendingConfirm { bdaddr, addr_type, index, deadline_ms });
        superseded
    }

    /// Apply the user's answer. `None` = there was nothing pending, so the answer is stray and must
    /// be dropped rather than remembered.
    fn answered(&mut self, accept: bool) -> Option<(PendingConfirm, bool)> {
        self.pending.take().map(|p| (p, accept))
    }

    /// The deadline for the pending confirm, if it has passed.
    fn expired(&mut self, now_ms: u64) -> Option<PendingConfirm> {
        match self.pending {
            Some(p) if now_ms >= p.deadline_ms => self.pending.take(),
            _ => None,
        }
    }

    /// Forget the pending confirm for `bdaddr` WITHOUT replying — the link it belonged to is gone
    /// (DEVICE_DISCONNECTED) or already resolved (NEW_LINK_KEY, or a reply we sent inline). Returns
    /// whether anything was actually dropped.
    fn cancel(&mut self, bdaddr: &[u8]) -> bool {
        match self.pending {
            Some(p) if bdaddr.len() >= 6 && p.bdaddr == bdaddr[0..6] => {
                self.pending = None;
                true
            }
            _ => false,
        }
    }
}

/// Answer one pending confirm: `OP_USER_CONFIRM_REPLY` (yes) or `OP_USER_CONFIRM_NEG_REPLY` (no), on
/// the controller the request arrived on. Best-effort like every other reply here — a write failure
/// leaves the kernel to time the SSP exchange out, which is what happens today anyway.
fn send_confirm_reply(sock: &mut File, p: &PendingConfirm, accept: bool) {
    let mut reply = Vec::with_capacity(7);
    reply.extend_from_slice(&p.bdaddr);
    reply.push(p.addr_type);
    let op = if accept { OP_USER_CONFIRM_REPLY } else { OP_USER_CONFIRM_NEG_REPLY };
    let cmd = build_cmd(op, p.index, &reply);
    let _ = sock.write_all(&cmd);
}

/// The side effects that go with answering NO: the code on screen is dead, and the connect attempt
/// must stop retrying. `rfcomm_uspace::pairing_aware_connect` aborts on `PAIR_REJECTED_FLAG` and the
/// reconnect driver backs off and removes it — the same flag the rejection-streak path raises.
fn refuse_pairing(bdaddr: &[u8; 6]) {
    clear_pairing_code();
    let _ = std::fs::write(crate::rfcomm_uspace::PAIR_REJECTED_FLAG, fmt_bdaddr_tail(bdaddr));
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
///
/// `on_pair_rejected` is called (at most once per connection) when a phone rejects
/// [`REJECT_WARN_THRESHOLD`] confirms in a row without ever completing a bond — see
/// `ConfirmTracker`. `bt-common` has no visibility into `ocbm-proto`'s `BTP_*` phase wire or
/// `crates/vendor/wireless`'s `bt_driver::publish_bt_phase` (a different crate), so this is a plain
/// callback hook rather than a hardcoded publish call; the wireless daemon wires it to
/// `BTP_PAIR_REJECTED`. `None` (e.g. in tests) just means the warning is log-only.
///
/// `pair_answer` is the head unit's yes/no for Numeric Comparison (see [`PairAnswer`]). With it,
/// numeric mode WAITS for a real answer instead of auto-accepting. `None` — or the
/// `CARPLAY_SSP_INTERACTIVE=1` lever — wait for the head unit's yes/no instead of confirming at once.
pub fn run(
    controller_index: u16,
    shutdown: &AtomicBool,
    on_pair_rejected: Option<&dyn Fn()>,
    pair_answer: Option<&PairAnswer>,
) -> std::io::Result<()> {
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
        // Per-record detail (last-two-octets only — never the full address in a log line): the
        // stored record layout is `[bdaddr 6][addr_type 1][key_type 1][value 16][pin_length 1]`.
        for rec in stored.chunks_exact(LINK_KEY_RECORD_LEN) {
            let addr_type = rec[6];
            let key_type = rec[7];
            log(&format!(
                "  bond ..{} addr_type={addr_type} key_type={} (0x{key_type:02x})",
                fmt_bdaddr_tail(&rec[0..6]),
                key_type_name(key_type)
            ));
        }
    }

    let (io_cap, numeric) = pairing_mode_io_cap();
    clear_pairing_code(); // drop any stale code from a prior session

    // Setup commands, each with its mgmt completion read back (see apply_setting). SET_SSP is the
    // load-bearing one — if the controller rejects it, pairing degrades to legacy PIN and iOS refuses
    // CarPlay. bt_bringup also forces `hciconfig sspmode 1` so SSP is on even if this mgmt path is quirky.
    apply_setting(&mut sock, OP_SET_POWERED, controller_index, &[1], "SET_POWERED=1");
    apply_setting(&mut sock, OP_SET_BONDABLE, controller_index, &[1], "SET_BONDABLE=1");
    apply_setting(&mut sock, OP_SET_CONNECTABLE, controller_index, &[1], "SET_CONNECTABLE=1");
    // General discoverable, no timeout (device-proven 2026-09-03). bt_bringup's `hciconfig piscan`
    // enables inquiry scan at the HCI level, but the kernel's mgmt layer rewrites Scan_Enable from
    // its OWN flags on SET_CONNECTABLE — with the discoverable flag never set through mgmt the box
    // silently dropped inquiry scan and never appeared in the iPhone's Bluetooth list. That matters
    // because iOS refuses a box-INITIATED numeric-comparison re-pair on sight (pairingComplete 162,
    // 0.3 ms after the request, prompt never shown) — the flow that works is the car-standard one:
    // the user taps the head unit in Settings ▸ Bluetooth, iOS initiates and shows its code, the
    // head unit confirms. That requires being discoverable. The stock CCPA is visible as
    // "CarLink-xxxx" permanently too.
    apply_setting(&mut sock, OP_SET_DISCOVERABLE, controller_index, &[1, 0, 0], "SET_DISCOVERABLE=general");
    apply_setting(&mut sock, OP_SET_SSP, controller_index, &[1], "SET_SSP=1");
    apply_setting(&mut sock, OP_SET_IO_CAPABILITY, controller_index, &[io_cap], "SET_IO_CAPABILITY");

    // Product default in numeric mode = confirm OUR side immediately (device-proven 2026-09-03/04
    // against iOS 27: when the box initiates and confirms at once, iOS shows its code sheet and the
    // user's comparison happens on the phone → authenticated key; when the box instead waits for the
    // head unit's yes/no, iOS fails the exchange 0.3 ms after its own confirm request and never shows
    // a sheet; and a phone-initiated pairing from Settings runs as Just-Works because iOS offers
    // NoInputNoOutput). The spec-literal "both humans answer" flow is therefore unreachable with iOS
    // as the peer. It stays available for other peers / bench work behind CARPLAY_SSP_INTERACTIVE=1
    // (the supervisor sets it from the host YAML `pairing: numeric_comparison_interactive`).
    let interactive_lever = std::env::var("CARPLAY_SSP_INTERACTIVE").as_deref() == Ok("1");
    let interactive = numeric && pair_answer.is_some() && interactive_lever;
    if numeric {
        log("pairing mode: NUMERIC COMPARISON (DisplayYesNo) — a 6-digit code will be shown to match");
        if interactive {
            log(&format!(
                "CARPLAY_SSP_INTERACTIVE=1 — the head unit must answer the code within \
                 {PAIR_CONFIRM_WAIT_SECS}s or the box replies NO (not reachable with iOS as the peer)"
            ));
        } else if interactive_lever {
            log("CARPLAY_SSP_INTERACTIVE=1 but no pair-answer channel wired — confirming our side immediately");
        } else {
            log("numeric confirms: our side is confirmed immediately; the user compares and confirms on the phone");
        }
    } else {
        log("pairing mode: Just-Works (NoInputNoOutput) — no code (proven CCPA default)");
    }
    log("pairing agent running");
    let mut confirm_tracker = ConfirmTracker::new();
    let mut confirms = PendingConfirmState::new();
    let mut buf = [0u8; 512];
    loop {
        if shutdown.load(Ordering::Relaxed) {
            clear_pairing_code(); // don't leave a stale code showing after teardown
            // A confirm we are about to stop servicing gets a definite NO rather than a stall: the
            // phone shows "Pairing Unsuccessful" now instead of spinning until its own timeout.
            if confirms.is_pending() {
                if let Some(p) = confirms.expired(u64::MAX) {
                    log(&format!(
                        "shutting down with a confirm pending for ..{} — replying no",
                        fmt_bdaddr_tail(&p.bdaddr)
                    ));
                    send_confirm_reply(&mut sock, &p, false);
                }
            }
            return Ok(());
        }

        // Serviced BEFORE the (1 s SO_RCVTIMEO-bounded) read, so a pending confirm is resolved on
        // time whether or not another mgmt event ever arrives.
        //
        // The user's answer is applied first: if the deadline and the answer land in the same
        // iteration, the human who actually looked at the code wins.
        if let Some(accept) = pair_answer.and_then(|a| a.take()) {
            match confirms.answered(accept) {
                Some((p, true)) => {
                    log(&format!(
                        "head unit CONFIRMED the code for ..{} — pairing",
                        fmt_bdaddr_tail(&p.bdaddr)
                    ));
                    send_confirm_reply(&mut sock, &p, true);
                }
                Some((p, false)) => {
                    log(&format!(
                        "head unit CANCELLED pairing with ..{} — replying no",
                        fmt_bdaddr_tail(&p.bdaddr)
                    ));
                    send_confirm_reply(&mut sock, &p, false);
                    refuse_pairing(&p.bdaddr);
                }
                // Stray answer (the code was already cleared by a disconnect, or the app answered
                // twice). Dropped, never banked for the next request — banking it would auto-accept
                // a confirm nobody looked at.
                None => log(&format!(
                    "pair answer ({}) with no confirm pending — ignored",
                    if accept { "pair" } else { "cancel" }
                )),
            }
        }
        if let Some(p) = confirms.expired(now_ms()) {
            log(&format!(
                "no answer from the head unit in {PAIR_CONFIRM_WAIT_SECS} s — replying no to ..{}",
                fmt_bdaddr_tail(&p.bdaddr)
            ));
            send_confirm_reply(&mut sock, &p, false);
            refuse_pairing(&p.bdaddr);
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
                // Load-Link-Keys layout). store_hint==0 means "don't persist" — honor it, but ALWAYS
                // log the event: store_hint==0 used to be silently skipped, which hid a real bond from
                // the log on the one path (a controller declining to ask for storage) that most needs
                // to be visible.
                if param_len > LINK_KEY_RECORD_LEN {
                    // (store_hint byte + the 25-byte record)
                    let store_hint = params[0];
                    let record_bytes = &params[1..1 + LINK_KEY_RECORD_LEN];
                    let addr_type = record_bytes[6];
                    let key_type = record_bytes[7];
                    log(&format!(
                        "NEW_LINK_KEY ..{} store_hint={store_hint} addr_type={addr_type} key_type={} (0x{key_type:02x})",
                        fmt_bdaddr_tail(&record_bytes[0..6]),
                        key_type_name(key_type)
                    ));
                    if store_hint != 0 {
                        let mut record = [0u8; LINK_KEY_RECORD_LEN];
                        record.copy_from_slice(record_bytes);
                        persist_link_key(&record);
                    }
                    // A real bond just formed for this device — its stored key is fresh, so any prior
                    // rejection streak no longer applies, and any confirm still pending for it is
                    // resolved (answering it now would be a reply to a completed exchange).
                    confirm_tracker.reset(&record_bytes[0..6]);
                    confirms.cancel(&record_bytes[0..6]);
                } else {
                    log(&format!("NEW_LINK_KEY (short event, len={param_len})"));
                }
                clear_pairing_code(); // bond formed → pairing complete → app hides the code
                let _ = std::fs::remove_file(crate::rfcomm_uspace::PAIR_REJECTED_FLAG);
            }
            EV_USER_CONFIRM_REQUEST => {
                // Numeric Comparison: the 6-digit value is `[value u32 LE]` at param offset 8 (after
                // bdaddr 6 + addr_type 1 + confirm_hint 1). Publish it for the app to display and then
                // WAIT for the head unit's yes/no (`PairAnswer`) — the spec's comparison is one a human
                // makes on BOTH devices, so the box no longer answers for itself. Just-Works: no
                // meaningful value (confirm_hint=1); auto-accept as before.
                // `confirm_hint` (params[7]) is the kernel's word on whether a human comparison is
                // required (0) or the pairing resolved to Just-Works (1) — honour it rather than our
                // own IO capability, per the SSP rules (a DisplayYesNo device still gets Just-Works
                // against a NoInputNoOutput peer).
                let hint_just_works = params.get(7) == Some(&1);
                // Numeric comparison for real: a code to compare AND (in interactive mode) a human on
                // this side to compare it. `hint_just_works` still overrides our IO capability.
                let comparing = numeric && !hint_just_works && param_len >= 12;
                let wait_for_user = comparing && interactive;
                if comparing {
                    let value =
                        u32::from_le_bytes([params[8], params[9], params[10], params[11]]);
                    let code = format!("{:06}", value % 1_000_000);
                    if wait_for_user {
                        // Drain BEFORE the code is published, never after: an answer meant for an
                        // older code (app double-tap, or a cancel that raced a disconnect) must not
                        // survive into this one, and no answer to THIS code can exist yet because
                        // nothing has been shown.
                        if let Some(a) = pair_answer {
                            a.clear();
                        }
                    }
                    write_pairing_code(&code);
                    if wait_for_user {
                        log(&format!(
                            "numeric-comparison code = {code} — waiting up to {PAIR_CONFIRM_WAIT_SECS}s for \
                             the head unit's yes/no (the connect is held {}s)",
                            crate::rfcomm_uspace::PAIRING_HOLD_SECS
                        ));
                    } else {
                        log(&format!(
                            "numeric-comparison code = {code} — confirm it matches the iPhone (auto-accepting; \
                             the connect is held up to {}s for the phone-side tap)",
                            crate::rfcomm_uspace::PAIRING_HOLD_SECS
                        ));
                    }
                } else {
                    log("auto-confirming pairing request (Just Works)");
                }
                if let Some((bdaddr, addr_type)) = addr(params) {
                    // Rejection-streak accounting FIRST, reply always -- never break the link over
                    // this, just make it visible. See `ConfirmTracker`/`REJECT_WARN_THRESHOLD`.
                    let streak = confirm_tracker.record_confirm(&bdaddr);
                    if let Some(count) = streak {
                        log(&format!(
                            "phone ..{} rejected pairing {count} times — its stored key no longer \
                             matches ours; re-pair on the iPhone (Settings ▸ Bluetooth) or forget the box",
                            fmt_bdaddr_tail(&bdaddr)
                        ));
                        // The code on screen is dead now, and the connect wait must stop retrying:
                        // clear the code, raise the rejected flag (rfcomm_uspace::connect_to aborts
                        // on it; the reconnect driver backs off and removes it).
                        clear_pairing_code();
                        let _ = std::fs::write(
                            crate::rfcomm_uspace::PAIR_REJECTED_FLAG,
                            fmt_bdaddr_tail(&bdaddr),
                        );
                        if let Some(hook) = on_pair_rejected {
                            hook();
                        }
                    }
                    // Wait for the user only when there is still a code on screen to answer. The
                    // streak arm above just CLEARED it, so arming there would strand the confirm for
                    // the full deadline with nothing able to answer it — keep the proven
                    // reply-anyway behaviour on that path instead.
                    if wait_for_user && streak.is_none() {
                        if let Some(old) = confirms.arm(bdaddr, addr_type, index, now_ms()) {
                            // Two phones mid-pair at once: only one code can be displayed, so the one
                            // we can no longer show is answered NO rather than left hanging.
                            log(&format!(
                                "a confirm for ..{} superseded the pending one for ..{} — replying no to the older",
                                fmt_bdaddr_tail(&bdaddr),
                                fmt_bdaddr_tail(&old.bdaddr)
                            ));
                            send_confirm_reply(&mut sock, &old, false);
                        }
                    } else {
                        // Replying inline: drop any pending confirm for this device first, or the
                        // deadline would later fire a reply over an exchange we just answered.
                        confirms.cancel(&bdaddr);
                        // YES everywhere the old agent said yes — Just-Works, `confirm_hint == 1`,
                        // and the bench lever. The ONE exception is the streak arm in interactive
                        // mode: it has just cleared the code, so no human can answer, and saying yes
                        // there would reintroduce exactly the unattended auto-accept this path
                        // exists to remove. Say NO instead — the same answer the deadline gives.
                        let accept = !wait_for_user;
                        let p = PendingConfirm { bdaddr, addr_type, index, deadline_ms: 0 };
                        send_confirm_reply(&mut sock, &p, accept);
                    }
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
            EV_DEVICE_DISCONNECTED => {
                if let Some((bdaddr, _)) = addr(params) {
                    // Next connection starts its rejection count at zero, and a code the app is
                    // still showing belongs to a link that no longer exists.
                    confirm_tracker.reset(&bdaddr);
                    clear_pairing_code();
                    // The link is gone: there is nobody to reply to, and a later deadline firing a
                    // negative reply for a dead connection could only confuse the next one.
                    if confirms.cancel(&bdaddr) {
                        log(&format!(
                            "..{} disconnected while its code was on screen — pairing confirm dropped",
                            fmt_bdaddr_tail(&bdaddr)
                        ));
                    }
                }
                log(&format!(
                    "DEVICE_DISCONNECTED {} reason={}",
                    addr(params).map_or("(short)".into(), |(b, _)| fmt_bdaddr(&b)),
                    params.get(7).map_or("?".to_string(), |r| format!("0x{r:02x}"))
                ))
            }
            EV_CONNECT_FAILED => log(&format!(
                "CONNECT_FAILED {} status={}",
                addr(params).map_or("(short)".into(), |(b, _)| fmt_bdaddr(&b)),
                params.get(7).map_or("?".to_string(), |s| format!("0x{s:02x}"))
            )),
            // mgmt ev_auth_failed params = `[bdaddr 6][addr_type 1][status 1]` — status at offset 7.
            EV_AUTH_FAILED => log(&format!(
                "AUTH_FAILED ..{} status={} — a stale/mismatched link key is the usual cause; \
                 check {} and the iPhone's Settings > General > CarPlay list",
                addr(params).map_or("????".into(), |(b, _)| fmt_bdaddr_tail(&b)),
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
    fn key_type_name_covers_the_named_mgmt_values() {
        assert_eq!(key_type_name(0x00), "combination");
        assert_eq!(key_type_name(0x03), "debug");
        assert_eq!(key_type_name(0x04), "unauth_p192");
        assert_eq!(key_type_name(0x05), "auth_p192");
        assert_eq!(key_type_name(0x06), "changed");
        assert_eq!(key_type_name(0x07), "unauth_p256");
        assert_eq!(key_type_name(0x08), "auth_p256");
        assert_eq!(key_type_name(0xff), "unknown");
    }

    #[test]
    fn fmt_bdaddr_tail_is_the_last_two_octets_never_the_full_address() {
        // On-wire mgmt order is little-endian; b[1]/b[0] are the address's own last two octets.
        let addr = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
        assert_eq!(fmt_bdaddr_tail(&addr), "22:11");
        assert_eq!(fmt_bdaddr_tail(&[]), "??:??");
    }

    #[test]
    fn confirm_tracker_fires_once_at_threshold_then_stays_quiet() {
        let mut t = ConfirmTracker::new();
        let a = [1, 2, 3, 4, 5, 6];
        assert_eq!(t.record_confirm(&a), None); // 1
        assert_eq!(t.record_confirm(&a), None); // 2
        assert_eq!(t.record_confirm(&a), Some(REJECT_WARN_THRESHOLD)); // 3 -- fires once
        assert_eq!(t.record_confirm(&a), None); // 4 -- already warned, stays quiet
        assert_eq!(t.record_confirm(&a), None); // 5
    }

    #[test]
    fn confirm_tracker_is_independent_per_bdaddr() {
        let mut t = ConfirmTracker::new();
        let a = [1, 2, 3, 4, 5, 6];
        let b = [9, 9, 9, 9, 9, 9];
        for _ in 0..(REJECT_WARN_THRESHOLD - 1) {
            assert_eq!(t.record_confirm(&a), None);
        }
        // b has never been seen, so it must not have inherited a's streak.
        assert_eq!(t.record_confirm(&b), None);
    }

    #[test]
    fn confirm_tracker_reset_restarts_the_streak() {
        let mut t = ConfirmTracker::new();
        let a = [1, 2, 3, 4, 5, 6];
        assert_eq!(t.record_confirm(&a), None);
        assert_eq!(t.record_confirm(&a), None);
        t.reset(&a); // e.g. NEW_LINK_KEY or DEVICE_DISCONNECTED landed
        assert_eq!(t.record_confirm(&a), None); // back to count 1, not 3
        assert_eq!(t.record_confirm(&a), None);
        assert_eq!(t.record_confirm(&a), Some(REJECT_WARN_THRESHOLD));
    }

    // ---- Numeric-Comparison pending-confirm state machine (pure logic, no mgmt socket) ----------

    const A: [u8; 6] = [1, 2, 3, 4, 5, 6];
    const B: [u8; 6] = [9, 9, 9, 9, 9, 9];

    #[test]
    fn a_confirm_stays_pending_until_the_head_unit_answers() {
        let mut s = PendingConfirmState::new();
        assert!(s.arm(A, 0, 0, 1_000).is_none());
        assert!(s.is_pending());
        // Nothing happens on its own before the deadline — this is the whole point: no auto-accept.
        assert_eq!(s.expired(1_000 + PAIR_CONFIRM_WAIT_SECS * 1000 - 1), None);
        let (p, accept) = s.answered(true).expect("pending confirm");
        assert!(accept && p.bdaddr == A && p.addr_type == 0);
        assert!(!s.is_pending());
    }

    #[test]
    fn the_deadline_is_the_only_thing_that_answers_for_the_user() {
        let mut s = PendingConfirmState::new();
        s.arm(A, 0, 0, 1_000);
        let p = s
            .expired(1_000 + PAIR_CONFIRM_WAIT_SECS * 1000)
            .expect("deadline reached");
        assert_eq!(p.bdaddr, A);
        assert!(!s.is_pending()); // consumed — it must not fire a second negative reply
        assert_eq!(s.expired(u64::MAX), None);
    }

    #[test]
    fn the_wait_is_inside_the_connect_hold() {
        // A deadline at or past PAIRING_HOLD_SECS would let the hold expire with the SSP exchange
        // still open, which is exactly the stall this replaced.
        assert!(PAIR_CONFIRM_WAIT_SECS < crate::rfcomm_uspace::PAIRING_HOLD_SECS);
    }

    #[test]
    fn a_repeat_from_the_same_phone_does_not_extend_the_deadline() {
        // iOS re-sends USER_CONFIRM_REQUEST while it waits. If each repeat pushed the deadline out,
        // a phone re-sending every few seconds would outlive the connect hold and never be answered.
        let mut s = PendingConfirmState::new();
        s.arm(A, 0, 0, 1_000);
        assert!(s.arm(A, 0, 0, 30_000).is_none(), "a repeat supersedes nothing");
        assert!(s.expired(1_000 + PAIR_CONFIRM_WAIT_SECS * 1000).is_some());
    }

    #[test]
    fn a_second_phone_supersedes_the_first_which_must_be_answered_no() {
        let mut s = PendingConfirmState::new();
        s.arm(A, 0, 0, 1_000);
        let old = s.arm(B, 0, 0, 2_000).expect("the displaced confirm is handed back");
        assert_eq!(old.bdaddr, A);
        // ...and the NEW one owns the screen, with its own fresh deadline.
        let (p, _) = s.answered(true).unwrap();
        assert_eq!(p.bdaddr, B);
        assert_eq!(p.deadline_ms, 2_000 + PAIR_CONFIRM_WAIT_SECS * 1000);
    }

    #[test]
    fn an_answer_with_nothing_pending_is_dropped_not_banked() {
        // Otherwise a stale "Pair" tap would silently confirm the NEXT code, which nobody looked at.
        let mut s = PendingConfirmState::new();
        assert!(s.answered(true).is_none());
        s.arm(A, 0, 0, 1_000);
        assert!(s.is_pending(), "the stray answer did not pre-confirm this one");
    }

    #[test]
    fn a_disconnect_cancels_the_confirm_for_that_device_only() {
        let mut s = PendingConfirmState::new();
        s.arm(A, 0, 0, 1_000);
        assert!(!s.cancel(&B), "some other device's disconnect must not clear our prompt");
        assert!(s.is_pending());
        assert!(s.cancel(&A));
        assert!(!s.is_pending());
        // No reply is owed to a dead link, and no deadline may fire for it later.
        assert_eq!(s.expired(u64::MAX), None);
        assert!(!s.cancel(&A), "cancelling twice is a no-op, not a second drop");
    }

    #[test]
    fn cancel_ignores_a_short_address() {
        let mut s = PendingConfirmState::new();
        s.arm(A, 0, 0, 1_000);
        assert!(!s.cancel(&[1, 2, 3]));
        assert!(s.is_pending());
    }

    #[test]
    fn the_reply_is_addressed_to_the_controller_the_request_came_from() {
        let mut s = PendingConfirmState::new();
        s.arm(A, 1, 7, 1_000);
        let (p, _) = s.answered(false).unwrap();
        assert_eq!((p.index, p.addr_type), (7, 1));
    }

    #[test]
    fn pair_answer_is_one_slot_latest_wins_and_consumed_once() {
        let a = PairAnswer::new();
        assert_eq!(a.take(), None);
        a.set(true);
        a.set(false); // the newest tap is the intent
        assert_eq!(a.take(), Some(false));
        assert_eq!(a.take(), None, "an answer is consumed exactly once");
        a.set(true);
        a.clear(); // a fresh confirm request drains anything unconsumed
        assert_eq!(a.take(), None);
    }

    #[test]
    fn build_cmd_with_empty_params() {
        let cmd = build_cmd(OP_SET_SSP, 2, &[]);
        assert_eq!(cmd, vec![0x0B, 0x00, 0x02, 0x00, 0x00, 0x00]);
    }
}
