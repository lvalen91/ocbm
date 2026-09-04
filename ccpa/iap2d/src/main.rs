//! iap2d — box-side iAP2 accessory L3 daemon (armv7/musl).
//!
//! A faithful port of carplayd's `src/iap2/driver.rs` (the reinforcing Rust session reference)
//! onto this box's transport: instead of FunctionFS ep1/ep2, it drives the single
//! `/dev/android_iap2` char device (the kernel android `f_iap2` function). Everything else —
//! link framing, the auth/identify state machine, the TLV message builders, the MFi bridge client —
//! is reused verbatim from `carplay-iap2-core`, which is machine-generated from Apple's
//! CarPlaySimulator.devicekitplugin (the authoritative protocol truth).
//!
//! Prereq: the phone-side cold-start (`tools/cold_start2.sh`) has role-switched the iPhone to USB
//! host and brought the iap2,ncm gadget up (state CONFIGURED). MFi auth runs on the LOCAL chip
//! (`/dev/i2c-1 @0x11`) directly from this daemon — there is no NCM MFi bridge (carplayd's
//! MFi auth helper /`mfi.rs` remote path is deliberately unused here; this setup has no NCM at all).

use carplay_iap2_core::{
    link::{self, Link},
    message, metadata, spec,
    state::{self, Action, State},
};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::OnceLock;
use std::thread::sleep;
use std::time::{Duration, Instant};

const IAP2_DEV: &str = "/dev/android_iap2";
/// Consecutive non-progress `iap.read()` results (persistent error or unexpected `Ok(0)`) before the
/// main loop gives up and exits (M2/L1) — see the `read_errs` comment in `main()`.
const READ_ERR_EXIT_THRESHOLD: u32 = 25;

// ---- Direct MFi chip access over I2C (/dev/i2c-1 @0x11). No NCM bridge, no NCM.
// The chip is LOCAL to the box, so iap2d drives it directly — unlike carplayd's
// mfi.rs, which reaches a remote CCPA over an NCM-based TCP bridge (192.168.50.2:5290).
// This setup does not use the NCM interface at all. ----
static G_I2C: AtomicI32 = AtomicI32::new(-1);
const MFI_ADDR: u16 = 0x11;
const I2C_M_RD: u16 = 0x0001;
const I2C_RDWR: libc::c_int = 0x0707;
#[repr(C)]
struct I2cMsg {
    addr: u16,
    flags: u16,
    len: u16,
    buf: *mut u8,
}
#[repr(C)]
struct I2cRdwr {
    msgs: *mut I2cMsg,
    nmsgs: u32,
}
fn i2c_rd(reg: u8, out: &mut [u8]) -> bool {
    let fd = G_I2C.load(Ordering::Relaxed);
    if fd < 0 {
        return false;
    }
    let mut r = reg;
    let mut m = [
        I2cMsg {
            addr: MFI_ADDR,
            flags: 0,
            len: 1,
            buf: &mut r,
        },
        I2cMsg {
            addr: MFI_ADDR,
            flags: I2C_M_RD,
            len: out.len() as u16,
            buf: out.as_mut_ptr(),
        },
    ];
    let mut x = I2cRdwr {
        msgs: m.as_mut_ptr(),
        nmsgs: 2,
    };
    for _ in 0..5 {
        if unsafe { libc::ioctl(fd, I2C_RDWR as _, &mut x) } >= 0 {
            return true;
        }
        unsafe { libc::usleep(5000) };
    }
    false
}
fn i2c_wr(reg: u8, data: &[u8]) -> bool {
    let fd = G_I2C.load(Ordering::Relaxed);
    if fd < 0 {
        return false;
    }
    let mut b = Vec::with_capacity(1 + data.len());
    b.push(reg);
    b.extend_from_slice(data);
    let mut m = I2cMsg {
        addr: MFI_ADDR,
        flags: 0,
        len: b.len() as u16,
        buf: b.as_mut_ptr(),
    };
    let mut x = I2cRdwr {
        msgs: &mut m,
        nmsgs: 1,
    };
    for _ in 0..5 {
        if unsafe { libc::ioctl(fd, I2C_RDWR as _, &mut x) } >= 0 {
            return true;
        }
        unsafe { libc::usleep(5000) };
    }
    false
}
/// Cross-process advisory lock serializing MFi I2C access (#109). The one `/dev/i2c-1` chip now has
/// FOUR users that must all agree on this path (corrected 2026-07-25 — this used to say "both
/// daemons"): wired `iap2d` (here), `carplay-wireless` (`mfi_local::MFI_LOCK_PATH`), `airplayd`'s
/// `LocalMfiSigner`, and `receiver`'s tunnel handshake via `mfi-i2c-local`. The cert/sign sequences are
/// stateful, so any interleaving corrupts both transactions. RAII: LOCK_EX on acquire, LOCK_UN + close
/// on drop. Bounded at 10s (LOCK_NB + deadline), matching `airplayd`'s `MfiLock` for this same file.
struct MfiLock(i32);
impl MfiLock {
    fn acquire() -> Option<MfiLock> {
        let fd = unsafe {
            libc::open(
                c"/tmp/carplay_mfi.lock".as_ptr(),
                // O_CLOEXEC — an inherited LOCK_EX open-file-description outlives this process in a
                // detached daemon; if the holder dies before Drop can LOCK_UN, every one of the five
                // MFi users blocks to its deadline forever.
                libc::O_CREAT | libc::O_RDWR | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return None;
        }
        // BOUNDED acquire (review fix 2026-07-31; was a bare blocking `flock(LOCK_EX)`): this runs
        // inside the single-threaded handshake loop, so an unbounded wait on a wedged sibling holder
        // hung the whole daemon — the loop never regained control, and even its own 120s handshake
        // budget could not fire. 10s matches airplayd's bound for this same lock; the worst-case
        // legitimate hold (airplayd's sign path, ~2.1s poll × 3 MFi retries ≈ 6.3s) cannot trip it
        // spuriously. On timeout, `execute()` maps the resulting None to NoCommit (state held; the
        // phone re-asks) — a recoverable stall where the blocking wait was a permanent one.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Some(MfiLock(fd));
            }
            if Instant::now() >= deadline {
                unsafe { libc::close(fd) };
                log("MFi lock busy >10s (another daemon is wedged holding /tmp/carplay_mfi.lock)");
                return None;
            }
            sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for MfiLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0, libc::LOCK_UN);
            libc::close(self.0);
        }
    }
}

/// CopyCertificate: reg 0x30 len (2 BE), reg 0x31 cert.
fn mfi_cert() -> Option<Vec<u8>> {
    let _lock = MfiLock::acquire()?; // serialize vs carplay-wireless (#109)
    let mut lb = [0u8; 2];
    if !i2c_rd(0x30, &mut lb) {
        return None;
    }
    let n = ((lb[0] as usize) << 8) | lb[1] as usize;
    if n == 0 || n > 2048 {
        return None;
    }
    let mut o = vec![0u8; n];
    if !i2c_rd(0x31, &mut o) {
        return None;
    }
    Some(o)
}
/// CreateSignature: write challenge (0x20 len, 0x21 data), go (0x10=1), poll bit4, read 0x11/0x12.
fn mfi_sign(chal: &[u8]) -> Option<Vec<u8>> {
    let _lock = MfiLock::acquire()?; // hold across the whole stateful sequence (#109)
    let clen = chal.len();
    if !i2c_wr(0x20, &[(clen >> 8) as u8, clen as u8]) {
        return None;
    }
    if !i2c_wr(0x21, chal) {
        return None;
    }
    if !i2c_wr(0x10, &[0x01]) {
        return None;
    }
    unsafe { libc::usleep(100_000) };
    let mut done = false;
    for _ in 0..200 {
        let mut st = [0u8; 1];
        if i2c_rd(0x10, &mut st) && (st[0] & 0x10) != 0 {
            if st[0] != 0x10 {
                eprintln!("mfi: sign status 0x{:02x} (expected 0x10)", st[0]);
            }
            done = true;
            break;
        }
        unsafe { libc::usleep(10_000) };
    }
    if !done {
        return None;
    }
    let mut sl = [0u8; 2];
    if !i2c_rd(0x11, &mut sl) {
        return None;
    }
    let n = ((sl[0] as usize) << 8) | sl[1] as usize;
    if n == 0 || n > 256 {
        return None;
    }
    let mut sig = vec![0u8; n];
    if !i2c_rd(0x12, &mut sig) {
        return None;
    }
    Some(sig)
}

/// The app-pushed vehicle identity (Identify param 20), snapshotted ONCE per process — docs/carplay/04_CAPABILITIES_AND_CONFIG.md C-3.
///
/// WHY A SNAPSHOT AND NOT A FRESH `load()` PER ARM. `RetryIdentify` must rebuild the SAME body minus
/// the excluded params. `ocbmd` rewrites `/tmp/carplay_cfg.yaml` on every CT_SUBSCRIBE, so an app
/// reconnect landing between the Identify and its retry would otherwise change param 20 mid-recovery
/// — on the one message whose rejection is unrecoverable within a session. First-load-wins, matching
/// how the metadata tier is armed.
///
/// SAFE TO LAND AHEAD OF C-4 (which declares 0xA100/0xA101/0xA102) because the app CANNOT currently
/// emit a `vehicleStatus:` block: `SettingsWindow.vehicleStatusUnlocked` is a compile-time `false`
/// ANDed inside the YAML emitter, not merely a disabled control. So `status_caps` stays `None`,
/// param 21 is never built, and only param 20's CONTENT changes here.
fn vehicle_identity() -> &'static carplay_iap2_core::config::VehicleIdentity {
    vehicle_identity_from(&carplay_iap2_core::config::Iap2Config::load())
}

/// Same OnceLock as [`vehicle_identity`], but takes an already-loaded config so `SendIdentify` can
/// share ONE `Iap2Config::load()` between the metadata-policy arm and the identity snapshot (L2):
/// two independent loads left a microscopic window where `ocbmd`'s SUBSCRIBE-triggered rewrite of
/// `/tmp/carplay_cfg.yaml` could land between them, even though the comment above argues for one
/// snapshot. First-call-wins regardless of which loaded config is passed in.
fn vehicle_identity_from(
    cfg: &carplay_iap2_core::config::Iap2Config,
) -> &'static carplay_iap2_core::config::VehicleIdentity {
    static ID: OnceLock<carplay_iap2_core::config::VehicleIdentity> = OnceLock::new();
    ID.get_or_init(|| {
        let id = cfg.vehicle_identity();
        log(&format!(
            "vehicle identity armed: {}",
            if id.is_baseline() {
                "baseline (no pushed iapConfig, or it resolved to the compiled default)"
            } else {
                "PUSHED from the app's iapConfig"
            }
        ));
        id
    })
}

/// `@<unix_ms> ` write-time stamp (docs/carplay/01_OCBM_PROTOCOL.md CH_LOG): the box.log tailer
/// parses this prefix and uses it instead of the millisecond it happened to READ the line at, so
/// a burst of lines written in the same tick doesn't collapse onto one timestamp.
fn log(m: &str) {
    println!("@{} [iap2] {m}", now_ms());
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Host-gone detection: the phone-facing gadget drops out of CONFIGURED when the iPhone leaves.
fn host_configured() -> Option<bool> {
    std::fs::read_to_string("/sys/class/android_usb/android0/state")
        .ok()
        .map(|s| s.trim() == "CONFIGURED")
}

/// Write fully, retrying on WouldBlock (the char dev is opened non-blocking). false = give up.
/// Bounded: a transport that stays WouldBlock for 6 s is dead (host gone / gadget stalled), and an
/// unbounded retry pinned the daemon in this loop forever.
fn tx<W: Write>(w: &mut W, data: &[u8]) -> bool {
    let start = Instant::now();
    let mut written = 0;
    while written < data.len() {
        match w.write(&data[written..]) {
            Ok(0) => return false,
            Ok(n) => written += n,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if start.elapsed() >= Duration::from_secs(6) {
                    log(&format!("tx stalled at {written}/{} B — giving up", data.len()));
                    return false;
                }
                sleep(Duration::from_millis(20));
            }
            Err(_) => return false,
        }
    }
    true
}

enum ExecResult {
    Commit,
    NoCommit,
    Abort,
}

/// Execute a state-machine Action: MFi bridge calls + iAP2 message writes. Verbatim from
/// carplayd's driver::execute except metadata is logged (no on-box render/subscribe).
fn execute(action: Action, link: &mut Link, iap: &mut File) -> ExecResult {
    match action {
        Action::SendCert => match mfi_cert() {
            Some(cert) => {
                let body = message::group_one(0x0000, &cert);
                if tx(
                    iap,
                    &link.build_msg(1, spec::MSG_AUTHENTICATION_CERTIFICATE, &body),
                ) {
                    log(&format!(
                        "TX 0xAA01 AuthenticationCertificate ({} B)",
                        cert.len()
                    ));
                    ExecResult::Commit
                } else {
                    ExecResult::Abort
                }
            }
            None => {
                log("mfi_cert() failed (i2c chip read) — state held");
                ExecResult::NoCommit
            }
        },
        Action::SignChallenge(chal) => match mfi_sign(&chal) {
            Some(sig) => {
                let body = message::group_one(0x0000, &sig);
                if tx(
                    iap,
                    &link.build_msg(1, spec::MSG_AUTHENTICATION_RESPONSE, &body),
                ) {
                    log(&format!(
                        "TX 0xAA03 AuthenticationResponse ({} B sig)",
                        sig.len()
                    ));
                    ExecResult::Commit
                } else {
                    ExecResult::Abort
                }
            }
            None => {
                log("mfi::sign() failed — state held");
                ExecResult::NoCommit
            }
        },
        Action::SendIdentify => {
            // Second (and last) chance to arm the pushed tier: the app may have SUBSCRIBEd during
            // our link/auth seconds. No-op when main() already armed it (first-arm-wins), so the
            // declaration below and the subscribes after Identified always share one snapshot.
            // ONE load shared with the identity snapshot below (L2) — see `vehicle_identity_from`.
            let cfg = carplay_iap2_core::config::Iap2Config::load();
            cfg.arm_metadata_policy();
            // cp_iface:1 = the NCM data interface (#1) that carries CarPlay A/V (not this iAP2 iface #0).
            // Per-device name (e.g. "CarLink-b0df") so multiple boxes are distinct on the wired iAP2
            // identify too — same suffix as the Wi-Fi SSID + the wireless BT name (message::accessory_name).
            let ib = message::build_ident_info_with(
                &message::accessory_name("CarLink"),
                message::TransportComponent::Usb { cp_iface: 1 },
                false, // declare_wired=false: CarPlay A/V rides AirPlay-over-NCM, not wired iAP2
                vehicle_identity_from(&cfg),
            );
            if !tx(
                iap,
                &link.build_msg(1, spec::MSG_IDENTIFICATION_INFORMATION, &ib),
            ) {
                return ExecResult::Abort;
            }
            log(&format!(
                "TX 0x1D01 IdentificationInformation ({} B)",
                ib.len()
            ));
            ExecResult::Commit
        }
        Action::RetryIdentify(excluded) => {
            // Same snapshot as the original Identify — see `vehicle_identity()`. Calling
            // `build_ident_info_excluding` (the identity-less form) here would silently substitute the
            // baseline and change param 20 mid-recovery.
            let ib = message::build_ident_info_excluding_with(
                &message::accessory_name("CarLink"),
                message::TransportComponent::Usb { cp_iface: 1 },
                false,
                &excluded,
                vehicle_identity(),
            );
            if !tx(
                iap,
                &link.build_msg(1, spec::MSG_IDENTIFICATION_INFORMATION, &ib),
            ) {
                return ExecResult::Abort;
            }
            log(&format!(
                "TX 0x1D01 retry, stripped {excluded:?} ({} B)",
                ib.len()
            ));
            ExecResult::Commit
        }
        Action::Note(m) => {
            log(m);
            ExecResult::Commit
        }
        Action::Ignore => ExecResult::Commit,
        Action::Abort => ExecResult::Abort,
        Action::NowPlaying(np) => {
            log(&format!("NowPlaying: {np:?}"));
            ExecResult::Commit
        }
        Action::RouteGuidance(rg) => {
            log(&format!("RouteGuidance: {rg:?}"));
            ExecResult::Commit
        }
        Action::Maneuver(mv) => {
            log(&format!("Maneuver: {mv:?}"));
            ExecResult::Commit
        }
    }
}

/// Process one received buffer. Returns true to abort. Ported from carplayd's driver::process, plus
/// the metadata plane (session-2 artwork + the Device→Accessory update messages).
fn process(
    data: &[u8],
    link: &mut Link,
    st: &mut State,
    iap: &mut File,
    art: &mut metadata::Artwork,
    link_up: &mut bool,
) -> bool {
    let Some(rx) = link.parse(data) else {
        return false;
    };
    if rx.is_syn_ack() {
        log("SYN-ACK — link up");
        *link_up = true;
        return !tx(iap, &link.build_ack());
    }
    if rx.payload.is_empty() {
        return false;
    }
    if rx.sess == 2 {
        // File Transfer session — album artwork. ACK the link frame, then run the transfer state
        // machine; its reply (Accept / Success) goes back out on session 2.
        //
        // `Rx.payload` keeps the C's shape: the frame body INCLUDING its trailing payload-checksum
        // byte (verified in parse()). Control messages self-bound via their [40 40][total] header so
        // the extra byte is inert there — but a raw file-transfer fragment does NOT, so appending it
        // would inject one stray byte per fragment into the JPEG. Strip it here.
        if !tx(iap, &link.build_ack()) {
            return true;
        }
        let body = &rx.payload[..rx.payload.len().saturating_sub(1)];
        if let Some(reply) = art.on_session2(body) {
            let _ = tx(iap, &link.build_raw(2, &reply));
        }
        return false;
    }
    let Some(msg_id) = link::parse_msg_id(&rx.payload) else {
        return false;
    };
    if !tx(iap, &link.build_ack()) {
        return true;
    }
    // Metadata plane (docs/carplay/05_METADATA_AND_CONTROLS.md): the Device→Accessory updates we subscribed to. Parsed here (not in
    // the shared state machine) and forwarded to the host's Metadata window over the :9004 seam.
    // The body is the payload past the 6-byte [40 40][total][msgid] header.
    let body: &[u8] = rx.payload.get(6..).unwrap_or(&[]);
    // One dispatcher, shared with the wireless RCS tunnel (`receiver::events`). These two paths
    // diverged once — 0x4171 ListUpdate was handled here and silently dropped there.
    metadata::dispatch(msg_id, body);
    // opt #5: NowPlaying (0x5001) / RouteGuidance (0x5201) / Maneuver (0x5202) are display-only —
    // metadata::dispatch above already parsed the TLV and forwarded it to the :9004 seam (the app shows
    // it). state::on_message would RE-parse the SAME bytes into typed structs solely to debug-log them
    // (execute() Commits with the state UNCHANGED for these ids), so skip the redundant second TLV walk.
    // State is never advanced by these ids, so leaving `*st` untouched matches on_message's own result.
    if matches!(msg_id, 0x5001 | 0x5201 | 0x5202) {
        log(&format!("RX 0x{msg_id:04X} (metadata → seam)"));
        return false;
    }
    // Mirror the wireless arm's 0x1D03 diagnostic (bt_driver.rs): raw payload + per-message-id
    // decoded reason, so a wired reject is as debuggable as a wireless one (docs/wireless/00_WIRELESS_CARPLAY.md Phase 5.2).
    if msg_id == spec::MSG_IDENTIFICATION_REJECTED {
        log(&format!(
            "RX 0x1D03 IdentificationRejected raw payload ({} B): {:02x?}",
            rx.payload.len(),
            rx.payload
        ));
        log(&format!(
            "RX 0x1D03 decoded: {}",
            message::describe_reject(&rx.payload[6..])
        ));
    }
    let (next, action) = state::on_message(*st, msg_id, &rx.payload);
    match execute(action, link, iap) {
        ExecResult::Commit => {
            *st = next;
            log(&format!("RX 0x{msg_id:04X} -> {st:?}"));
            false
        }
        ExecResult::NoCommit => {
            log(&format!("RX 0x{msg_id:04X}: action failed, state held"));
            false
        }
        ExecResult::Abort => true,
    }
}

/// Safe ad-hoc health-check / self-test: `IAP2D_SELFTEST=1 iap2d` runs the EXACT `Link::new()` →
/// write-DETECT → `build_syn()` → write-SYN sequence that's bounded (via bisection) as the crash
/// site in the UPX-packing investigation — but against a throwaway scratch file, NEVER the real
/// `/dev/android_iap2` char device or the `/dev/i2c-1` MFi chip. This exists specifically so that
/// binary/memory-layout questions (does this code sequence run at all under a given build?) can be
/// answered ad-hoc, safely, repeatedly — without touching the real USB gadget device, which does NOT
/// tolerate ad-hoc/out-of-band opens: an ad-hoc run against the real device once wedged the whole
/// box (UART + USB gadget both went unresponsive, needing a physical power cycle to recover). If
/// this self-test ALSO crashes, the bug is pure code/UPX-stub and safely reproducible; if it
/// doesn't, the real device's driver behavior is implicated and must only be tested via the normal
/// session_supervisor-driven lifecycle (a real phone attached), never ad-hoc.
fn run_selftest() {
    eprintln!("[iap2d-selftest] scratch-file mode — never touches /dev/android_iap2 or /dev/i2c-1");
    let path = "/tmp/iap2d_selftest_target";
    let mut target = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[iap2d-selftest] FAIL: could not open scratch file {path}: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("[iap2d-selftest] opened scratch target");
    let mut link = Link::new();
    eprintln!("[iap2d-selftest] Link::new() ok");
    if !tx(&mut target, &link::DETECT) {
        eprintln!("[iap2d-selftest] FAIL: DETECT write failed");
        std::process::exit(1);
    }
    eprintln!("[iap2d-selftest] DETECT write ok");
    let syn = link.build_syn(&link::SYN_PARAMS);
    eprintln!("[iap2d-selftest] build_syn ok ({} bytes)", syn.len());
    if !tx(&mut target, &syn) {
        eprintln!("[iap2d-selftest] FAIL: SYN write failed");
        std::process::exit(1);
    }
    eprintln!("[iap2d-selftest] SYN write ok");
    eprintln!("[iap2d-selftest] PASSED — full Link/DETECT/SYN sequence completed without crashing");
    let _ = std::fs::remove_file(path);
}

/// `panic = "abort"` (workspace-wide) means the default hook's stderr line is the only trace of a
/// crash the supervisor sees — prefix it so it's greppable in the merged host log stream.
fn install_panic_hook(name: &'static str) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("[{name}] PANIC: {info}");
        default_hook(info);
    }));
}

fn main() {
    install_panic_hook("iap2d");
    if std::env::var_os("IAP2D_SELFTEST").is_some() {
        run_selftest();
        return;
    }

    // Arm the metadata declaration tier from the app-pushed YAML (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B3) BEFORE the link comes
    // up. ocbmd lands `/tmp/carplay_cfg.yaml` before it flips `/tmp/host_present`, and the
    // supervisor only runs projection_up (which spawns us) on that presence edge, so the document
    // is normally already on disk here. It is re-attempted at SendIdentify to close the window
    // where the app SUBSCRIBEd during our link/auth seconds; `arm_pushed_policy` is
    // first-arm-wins, so the two attempts can never disagree — and params 6/7 and the subscribes
    // that follow are generated from the ONE snapshot armed here.
    carplay_iap2_core::config::Iap2Config::load().arm_metadata_policy();

    let mut iap = match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(IAP2_DEV)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("open {IAP2_DEV}: {e}");
            std::process::exit(1);
        }
    };
    log("opened /dev/android_iap2 — iAP2 transport ready");
    // open the LOCAL MFi chip directly over i2c — no remote bridge, no NCM
    let i2c = unsafe { libc::open(c"/dev/i2c-1".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if i2c < 0 {
        // Without the chip every auth Action fails and the handshake is doomed anyway — running on
        // is just a slower, more confusing failure. The supervisor respawns iap2d while the phone
        // is present (session_supervisor.sh), so exiting IS the retry.
        log("FATAL: cannot open /dev/i2c-1 (MFi chip)");
        std::process::exit(1);
    }
    G_I2C.store(i2c, Ordering::Relaxed);
    // Advisory only: a NAK here must not kill the daemon — the real reads retry per-transaction.
    // But it is still a chip transaction, so it takes the same cross-process lock as cert/sign: the
    // one thing worse than a failed warm-up is one that lands inside another chip user's
    // write→trigger→poll→read window (airplayd's sign path) and corrupts THAT transaction.
    let mut v = [0u8; 1];
    let warm = match MfiLock::acquire() {
        Some(_lock) => i2c_rd(0x00, &mut v),
        None => false,
    };
    if warm {
        log(&format!(
            "MFi chip open (local i2c), DeviceVersion=0x{:02x}",
            v[0]
        ));
    } else {
        log("MFi chip open (local i2c), DeviceVersion read failed (transient NAK or chip lock busy)");
    }

    let mut link = Link::new();
    if !tx(&mut iap, &link::DETECT) {
        log("detect write failed");
        std::process::exit(1);
    }
    let syn = link.build_syn(&link::SYN_PARAMS);
    if !tx(&mut iap, &syn) {
        log("SYN write failed");
        std::process::exit(1);
    }
    log("TX detect + SYN — waiting for SYN-ACK");

    let mut st = State::Init;
    let start = Instant::now();
    let mut last_syn = Instant::now();
    let mut link_up = false;
    let mut buf = [0u8; 8192];
    let mut seen_configured = false;
    // Throttle the host-gone check (opt #4): host_configured() opens+reads a sysfs file and allocs a
    // String; it ran EVERY loop pass (≥20 Hz during a session) purely to detect a slow event. ~1 Hz is
    // ample — the 120 s handshake budget and the read-error path are the fast host-gone signals.
    let mut last_host_check = Instant::now();
    let mut art = metadata::Artwork::default();
    let mut subscribed = false;
    // M2/L1: bound consecutive non-progress reads (persistent errors or unexpected Ok(0)) so a
    // host-gone daemon exits instead of spinning forever behind the 1 Hz sysfs poll. 25 * 200 ms = 5 s,
    // comparable to `tx()`'s 6 s bound.
    let mut read_errs = 0u32;

    loop {
        let identified = st >= State::Identified;
        if !identified && start.elapsed() >= Duration::from_secs(120) {
            log("handshake budget expired before Identify");
            break;
        }
        // Metadata subscriptions (docs/carplay/05_METADATA_AND_CONTROLS.md): once Identified, ask iOS for the feeds behind the host's
        // Metadata window. iOS sends NOTHING un-subscribed, and an empty field list is silently
        // ignored — each body names the exact fields (metadata.rs). Best-effort: a failed subscribe
        // never aborts an otherwise-healthy CarPlay session (this is additive to the A/V path).
        if identified && !subscribed {
            subscribed = true;
            // The subscribe list is generated from `iap2_core::features` (docs/carplay/05_METADATA_AND_CONTROLS.md), the same table
            // that generates the wired Identify's params 6/7. Before the table, these were two
            // hand-maintained lists in two files: 0x4157/0x4170 were being subscribed while
            // 0x4158/0x4171 were undeclared, so iOS ignored both for the project's whole history.
            // `CARPLAY_METADATA=proven` narrows both back to the device-accepted baseline;
            // `CARPLAY_METADATA_SKIP=power,app_discovery` drops named features.
            let features = carplay_iap2_core::features::active(
                carplay_iap2_core::features::Policy::active().subscribe,
                metadata::start_now_playing,
                metadata::start_route_guidance,
            );
            log(&format!(
                "metadata: {} features ({})",
                features.len(),
                features.iter().map(|f| f.name).collect::<Vec<_>>().join(", ")
            ));
            for (id, name, body) in features.iter().filter_map(|f| {
                f.start().map(|start| (start, format!("0x{start:04X} {}", f.name), f.build_body()))
            }) {
                let n = body.len();
                if tx(&mut iap, &link.build_msg(1, id, &body)) {
                    log(&format!("TX {name} ({n} B)"));
                } else {
                    log(&format!("TX {name} failed (non-fatal)"));
                }
                sleep(Duration::from_millis(30)); // don't burst the link
            }
        }
        if last_host_check.elapsed() >= Duration::from_secs(1) {
            last_host_check = Instant::now();
            match host_configured() {
                Some(true) => seen_configured = true,
                Some(false) if seen_configured => {
                    log("host gone (gadget no longer CONFIGURED)");
                    break;
                }
                _ => {}
            }
        }
        match iap.read(&mut buf) {
            Ok(0) => {
                // Char-device convention here is EAGAIN for "no data"; a 0-byte read is unusual and,
                // on other fds, usually means EOF/hangup (L1). Fold it into the same persistent-error
                // counter as a real read error so a host-gone Ok(0) storm still exits (M2) rather than
                // sleeping forever behind the 1 Hz sysfs poll.
                read_errs += 1;
                if read_errs >= READ_ERR_EXIT_THRESHOLD {
                    log("read returning Ok(0) persistently — exiting");
                    break;
                }
                sleep(Duration::from_millis(50));
            }
            Ok(n) => {
                read_errs = 0;
                // COALESCED READS: the char device hands us several link packets per read.
                //
                // During auth/identify we deliberately process only the FIRST packet — that is the
                // proven `iap2_auth.c` behavior the handshake depends on (the iPhone retransmits the
                // rest on our per-packet ACK; see link.rs's coalesced-read note).
                //
                // Once Identified we drain EVERY packet in the read. The File-Transfer fragments
                // (album artwork) are ACKed and never retransmitted, so a first-packet-only drain
                // dropped roughly half of them and truncated the JPEG (2026-07-12: 47 KB of 92 KB).
                let mut off = 0usize;
                let mut aborted = false;
                // The loop ends on a torn tail packet (`packet_len` == None). CORRECTION 2026-07-25:
                // this used to claim "next read continues it". It does NOT — there is no carry-over
                // buffer; the next `iap.read(&mut buf)` overwrites from offset 0, so a link packet
                // torn across two reads is discarded outright. That is tolerable only because a
                // discarded packet was never ACKed, so the iPhone's link layer retransmits it off the
                // stale cumulative ack (this covers session-2 fragments too — "never retransmitted"
                // applies only to ACKed ones). Stop there; retransmit covers it.
                while let Some(plen) = link::packet_len(&buf[off..n]) {
                    if process(
                        &buf[off..off + plen],
                        &mut link,
                        &mut st,
                        &mut iap,
                        &mut art,
                        &mut link_up,
                    ) {
                        aborted = true;
                        break;
                    }
                    off += plen;
                    // Pre-Identify: keep the proven one-packet-per-read behavior (retransmit covers
                    // the rest). Post-Identify: walk the whole coalesced read.
                    if st < State::Identified || off >= n {
                        break;
                    }
                }
                if aborted {
                    break;
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                read_errs = 0;
                if !link_up && last_syn.elapsed() >= Duration::from_secs(1) {
                    let _ = tx(&mut iap, &syn);
                    last_syn = Instant::now();
                }
                sleep(Duration::from_millis(50));
            }
            Err(e) => {
                // The `last_host_check` comment above claims the read-error path is a fast host-gone
                // signal; nothing enforced that until now (M2). `tools/session_supervisor.sh` uses
                // `pgrep iap2d` as its wired-CarPlay liveness evidence, so a daemon spinning on a
                // persistent read error must exit rather than sleep forever.
                read_errs += 1;
                if read_errs >= READ_ERR_EXIT_THRESHOLD {
                    log(&format!("read failing persistently ({e}) — exiting"));
                    break;
                }
                sleep(Duration::from_millis(200));
            }
        }
    }
    log(&format!("exit state={st:?}"));
    std::process::exit(if st >= State::Authenticated { 0 } else { 5 });
}
