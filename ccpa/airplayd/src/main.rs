//! airplayd — box-side AirPlay/CarPlay pairing daemon (armv7/musl).
//!
//! Terminates the iPhone's AirPlay session on the box's real `ncm0`, runs pair-setup / pair-verify
//! using the genuine **local** MFi 2.0C chip (`/dev/i2c-1 @0x11`) — NOT receiver_core's remote NCM
//! signing service — derives the ChaCha20 session keys, and (later) forwards the encrypted A/V + hands the
//! ephemeral key to the host app over OCBM. Reuses receiver_core's `mfi` / `pairing` crypto crates.
//!
//! FIRST SLICE (this): the local `MfiSigner` — chip cert-copy + challenge-sign, the exact two chip
//! operations `iap2d` already proved against a real iPhone this session (945-byte cert → 0xAA01,
//! 128-byte sig → 0xAA03, iPhone → 0xAA05 AuthSuccess) — exercised through receiver_core's
//! `MfiSigner` trait, proving the remote→local redirection and the crate reuse compile + work.

use mfi::auth_client::MfiSigner;
use pairing::setup::PeerSaver;
use pairing::verify::{Identity, PeerStore};
use receiver::info::{build_info, DeviceConfig};
use receiver::net::serve_connection;
use receiver::server::ControlServer;
use receiver::vehicle_config::VehicleConfig;
use receiver::{events, hid, levers};
use std::collections::HashMap;
use std::io;
use std::io::Read as _;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Cluster-content URLs for `showUI`, with Apple's query string.
///
/// Every `showUI` the CarPlay Simulator sends for the bare-cluster and map surfaces carries these
/// three toggles — they are literally "the elements inside the navigation video". Observed vocabulary
/// (`AirPlayShowUIURL.airPlayURL` @0x10002adbc): `showSpeedLimit` = `user`|`no`, `showCompass` =
/// `user`|`no`, `showETA` = `yes`|`no`, plus `maneuverLayout` which is appended only when the layout is
/// not `none` and was empty-valued in every captured case. We send the most-featureful observed
/// variant. `instructioncard` NEVER carries a query string in the Simulator, so it is sent bare.
///
/// The two URLs are spelled out in full rather than concatenated at runtime — these are wire literals
/// and a future reader should be able to grep the exact string that goes out.
/// Cluster surface identifiers — which content URL to build. Tracked in NAV_SURFACE so a
/// CMD_NAV_APPEARANCE toggle can rebuild the SAME surface's URL with the new query flags and re-showUI.
const SURFACE_NONE: u8 = 0;
const SURFACE_MAP: u8 = 1; // maps:/car/instrumentcluster/map
const SURFACE_CARD: u8 = 2; // maps:/car/instrumentcluster/instructioncard (bare — no query, matching Apple)
const SURFACE_APP: u8 = 3; // maps:/car/instrumentcluster ("Navigation App")
static NAV_SURFACE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(SURFACE_NONE);
/// Cluster appearance flags (CMD_NAV_APPEARANCE). Default = all on = the legacy hardcoded
/// showSpeedLimit=user & showCompass=user & showETA=yes.
static NAV_APPEARANCE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(
    ocbm_proto::NAV_APPEARANCE_SPEED_LIMIT
        | ocbm_proto::NAV_APPEARANCE_COMPASS
        | ocbm_proto::NAV_APPEARANCE_ETA,
);

/// Build the `showUI` URL for `surface` with the current appearance flags. map/app carry Apple's query
/// string (showSpeedLimit=user|no, showCompass=user|no, showETA=yes|no, + empty maneuverLayout);
/// instructioncard is sent bare — the Simulator never queries it.
fn cluster_url(surface: u8) -> String {
    let f = NAV_APPEARANCE.load(std::sync::atomic::Ordering::Relaxed);
    let sl = if f & ocbm_proto::NAV_APPEARANCE_SPEED_LIMIT != 0 { "user" } else { "no" };
    let cp = if f & ocbm_proto::NAV_APPEARANCE_COMPASS != 0 { "user" } else { "no" };
    let eta = if f & ocbm_proto::NAV_APPEARANCE_ETA != 0 { "yes" } else { "no" };
    let query = format!("?showSpeedLimit={sl}&showCompass={cp}&showETA={eta}&maneuverLayout=");
    match surface {
        SURFACE_MAP => format!("maps:/car/instrumentcluster/map{query}"),
        SURFACE_APP => format!("maps:/car/instrumentcluster{query}"),
        _ => "maps:/car/instrumentcluster/instructioncard".to_string(),
    }
}

// ---- Local MFi 2.0C chip over I2C (/dev/i2c-1 @0x11) — same proven path as iap2d / the MFi auth helper (ncm_carplayd) ----
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

/// `MfiSigner` backed by the on-board genuine MFi 2.0C coprocessor over local I2C — the box-resident
/// signing service. Drops in for receiver_core's `MfiAuthClient` (remote NCM TCP) with direct chip
/// access, so the MFi private key never leaves the adapter.
pub struct LocalMfiSigner {
    fd: libc::c_int,
}

/// Close the i2c handle on drop. A fresh `LocalMfiSigner` is opened per iPhone connection, so without
/// this the `/dev/i2c-1` fd would leak on every (re)connect and eventually exhaust the fd limit —
/// pairing would then fail with no obvious cause on the reconnect-heavy lifecycle (docs/carplay/02_SESSION_LIFECYCLE.md).
impl Drop for LocalMfiSigner {
    fn drop(&mut self) {
        if self.fd >= 0 {
            unsafe { libc::close(self.fd) };
        }
    }
}

impl LocalMfiSigner {
    pub fn open() -> io::Result<Self> {
        // Remote coprocessor (Raspberry Pi port): there is no /dev/i2c-1 on this host, so opening it
        // here would fail the whole control connection before a single trait method ran — which is
        // exactly what "MFi open: No such file or directory" was. Hand back a signer with no fd; the
        // trait impls below dispatch over the network and never touch `self.fd`.
        if mfi_wire::client::addr().is_some() {
            return Ok(Self { fd: -1 });
        }
        // O_CLOEXEC: never let the chip handle leak into a child process.
        let fd = unsafe {
            libc::open(c"/dev/i2c-1".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC)
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd })
    }

    fn rd(&self, reg: u8, out: &mut [u8]) -> bool {
        let mut r = reg;
        let mut m = [
            I2cMsg { addr: MFI_ADDR, flags: 0, len: 1, buf: &mut r },
            I2cMsg { addr: MFI_ADDR, flags: I2C_M_RD, len: out.len() as u16, buf: out.as_mut_ptr() },
        ];
        let mut x = I2cRdwr { msgs: m.as_mut_ptr(), nmsgs: 2 };
        for _ in 0..5 {
            if unsafe { libc::ioctl(self.fd, I2C_RDWR as _, &mut x) } >= 0 {
                return true;
            }
            unsafe { libc::usleep(5000) };
        }
        false
    }

    fn wr(&self, reg: u8, data: &[u8]) -> bool {
        let mut b = Vec::with_capacity(1 + data.len());
        b.push(reg);
        b.extend_from_slice(data);
        let mut m = I2cMsg { addr: MFI_ADDR, flags: 0, len: b.len() as u16, buf: b.as_mut_ptr() };
        let mut x = I2cRdwr { msgs: &mut m, nmsgs: 1 };
        for _ in 0..5 {
            if unsafe { libc::ioctl(self.fd, I2C_RDWR as _, &mut x) } >= 0 {
                return true;
            }
            unsafe { libc::usleep(5000) };
        }
        false
    }

    /// DeviceVersion (reg 0x00) — a cheap warm-up/liveness read (=0x03 on this unit).
    pub fn device_version(&self) -> Option<u8> {
        let mut v = [0u8; 1];
        if self.rd(0x00, &mut v) {
            Some(v[0])
        } else {
            None
        }
    }
}

/// Cross-process advisory lock serializing MFi I2C access, shared with `iap2d` (`MfiLock`),
/// `carplay-wireless` (`mfi_local::MFI_LOCK_PATH`) and `receiver`'s tunnel path
/// (`mfi-i2c-local::MFI_LOCK_PATH`). All four must agree on this path.
///
/// QC 2026-07-25 (HIGH): airplayd was the ONE chip user that took no lock at all. Both sequences
/// below are stateful multi-register transactions (cert = 0x30 then 0x31; sign = write 0x20/0x21,
/// trigger 0x10, poll 0x10 bit4, read 0x11/0x12), and airplayd + carplay-wireless are BOTH live around
/// a wireless bring-up. An interleave lets the second writer's challenge clobber the first's before its
/// trigger/poll completes — both sides then get a garbage signature and pairing fails with no
/// diagnostic. Note this only serializes; it does not change the (device-proven) register sequence.
///
/// Acquisition is bounded rather than a bare blocking `LOCK_EX`: the worst-case legitimate hold is the
/// sign path's ~2.1 s poll × 3 MFi retries ≈ 6.3 s, so a 10 s ceiling cannot fire spuriously but still
/// stops a wedged holder from hanging pairing forever (the failure then surfaces as a normal signer
/// error the handshake can report, instead of a silent stall).
struct MfiLock(i32);
impl MfiLock {
    fn acquire() -> io::Result<MfiLock> {
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
            return Err(io::Error::last_os_error());
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(MfiLock(fd));
            }
            if std::time::Instant::now() >= deadline {
                unsafe { libc::close(fd) };
                return Err(io::Error::other(
                    "MFi lock busy >10s (another daemon is wedged holding /tmp/carplay_mfi.lock)",
                ));
            }
            unsafe { libc::usleep(20_000) };
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

impl MfiSigner for LocalMfiSigner {
    /// CopyCertificate: reg 0x30 = length (2 BE), reg 0x31 = cert bytes (the genuine ~945-byte cert).
    fn copy_certificate(&mut self) -> io::Result<Vec<u8>> {
        // Remote coprocessor (Raspberry Pi port): this host has no /dev/i2c-1, so the chip is
        // reached over USB-NCM. Dispatching INSIDE the impl keeps `LocalMfiSigner`'s type identical,
        // so `ControlServer<_, LocalMfiSigner>` and every other generic instantiation are unchanged.
        if let Some(addr) = mfi_wire::client::addr() {
            return mfi_wire::client::cert(addr);
        }
        let _lock = MfiLock::acquire()?; // held for the whole 0x30→0x31 transaction
        let mut lb = [0u8; 2];
        if !self.rd(0x30, &mut lb) {
            return Err(io::Error::other("MFi cert length read failed"));
        }
        let n = ((lb[0] as usize) << 8) | lb[1] as usize;
        if n == 0 || n > 2048 {
            return Err(io::Error::other(format!("MFi cert length {n} out of range")));
        }
        let mut cert = vec![0u8; n];
        if !self.rd(0x31, &mut cert) {
            return Err(io::Error::other("MFi cert read failed"));
        }
        Ok(cert)
    }

    /// CreateSignature over a 20-byte SHA-1 digest: write challenge (0x20 len / 0x21 data), trigger
    /// (0x10=1), poll status (0x10 bit4), read sig (0x11 len / 0x12 data) — genuine 128-byte RSA-1024.
    fn create_signature(&mut self, digest: &[u8]) -> io::Result<Vec<u8>> {
        if let Some(addr) = mfi_wire::client::addr() {
            return mfi_wire::client::sign(addr, digest);
        }
        // Held across the ENTIRE write→trigger→poll→read sequence, not just the first register: the
        // poll window is exactly where an interleaving writer would corrupt the in-flight challenge.
        let _lock = MfiLock::acquire()?;
        let clen = digest.len();
        if !self.wr(0x20, &[(clen >> 8) as u8, clen as u8]) {
            return Err(io::Error::other("MFi challenge-len write failed"));
        }
        if !self.wr(0x21, digest) {
            return Err(io::Error::other("MFi challenge write failed"));
        }
        if !self.wr(0x10, &[0x01]) {
            return Err(io::Error::other("MFi sign trigger failed"));
        }
        unsafe { libc::usleep(100_000) };
        let mut done = false;
        for _ in 0..200 {
            let mut st = [0u8; 1];
            if self.rd(0x10, &mut st) && (st[0] & 0x10) != 0 {
                done = true;
                break;
            }
            unsafe { libc::usleep(10_000) };
        }
        if !done {
            return Err(io::Error::other("MFi sign poll timeout"));
        }
        let mut sl = [0u8; 2];
        if !self.rd(0x11, &mut sl) {
            return Err(io::Error::other("MFi sig length read failed"));
        }
        let n = ((sl[0] as usize) << 8) | sl[1] as usize;
        if n == 0 || n > 256 {
            return Err(io::Error::other(format!("MFi sig length {n} out of range")));
        }
        let mut sig = vec![0u8; n];
        if !self.rd(0x12, &mut sig) {
            return Err(io::Error::other("MFi sig read failed"));
        }
        Ok(sig)
    }
}

fn hex8(b: &[u8]) -> String {
    b.iter().take(8).map(|x| format!("{x:02x}")).collect()
}

/// Poison-tolerant lock. House policy: never bare `.lock().unwrap()` on cross-thread state.
/// Production runs panic="abort" so poisoning is unreachable there; in test builds (unwind) a
/// panicking holder must not cascade into every later locker. Mirrors `receiver`'s `plock`.
fn plock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Pairing store: an in-memory map for fast, connection-shared lookup, **persisted to disk** so KNOWN
/// devices skip pair-setup on reconnect (pair-verify against the stored long-term key) — the fast
/// known-device path (docs/carplay/02_SESSION_LIFECYCLE.md). RiddleBox kept the same records in `/etc`. Only the pairing (id → LTPK)
/// persists; the session YAML config is EPHEMERAL and lives elsewhere (ocbmd `cfg`), never here.
/// Path via `PEERSTORE_PATH` (default `/etc/carplay_peers.bin`). If the path isn't writable the store
/// still works in-memory (logs a warning) — degrades to the old re-pair-each-boot behavior, never fails.
/// File format: repeated `[u8 id_len][id][32 LTPK]`.
#[derive(Clone)]
struct DiskPeers {
    map: Arc<Mutex<HashMap<Vec<u8>, [u8; 32]>>>,
    path: Arc<std::path::PathBuf>,
}
impl DiskPeers {
    fn new() -> Self {
        let path = std::path::PathBuf::from(
            std::env::var("PEERSTORE_PATH").unwrap_or_else(|_| "/etc/carplay_peers.bin".into()),
        );
        let mut map = HashMap::new();
        match std::fs::read(&path) {
            Ok(bytes) => {
                let n = Self::load_into(&bytes, &mut map);
                eprintln!("[airplayd] peerstore: loaded {n} known device(s) from {}", path.display());
            }
            Err(_) => eprintln!("[airplayd] peerstore: none at {} (fresh)", path.display()),
        }
        DiskPeers { map: Arc::new(Mutex::new(map)), path: Arc::new(path) }
    }
    fn load_into(mut b: &[u8], map: &mut HashMap<Vec<u8>, [u8; 32]>) -> usize {
        let mut n = 0;
        while !b.is_empty() {
            let idl = b[0] as usize;
            if b.len() < 1 + idl + 32 {
                break;
            }
            let id = b[1..1 + idl].to_vec();
            let mut ltpk = [0u8; 32];
            ltpk.copy_from_slice(&b[1 + idl..1 + idl + 32]);
            map.insert(id, ltpk);
            b = &b[1 + idl + 32..];
            n += 1;
        }
        n
    }
    /// Atomically dump the whole map (write `.tmp` + rename) — small, few peers, so rewrite-all is simplest.
    fn persist(&self, map: &HashMap<Vec<u8>, [u8; 32]>) {
        let mut out = Vec::new();
        for (id, ltpk) in map {
            if id.len() > 255 {
                continue;
            }
            out.push(id.len() as u8);
            out.extend_from_slice(id);
            out.extend_from_slice(ltpk);
        }
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // UNIQUE tmp name per write (audit B2): a fixed `.tmp` let two concurrent `save_peer` calls
        // truncate + write the same file, so one still-open fd could append after the other's rename had
        // already moved that inode live — corrupting the peerstore. A pid+counter-suffixed tmp makes each
        // writer's temp private; the atomic rename then publishes a WHOLE valid snapshot (last writer wins;
        // a momentarily-missing peer self-heals on the next save, but the file is never torn).
        static PERSIST_SEQ: portable_atomic::AtomicU64 = portable_atomic::AtomicU64::new(0);
        let uniq = PERSIST_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = self.path.with_extension(format!("tmp.{}.{}", std::process::id(), uniq));
        if let Err(e) = std::fs::write(&tmp, &out).and_then(|_| std::fs::rename(&tmp, &*self.path)) {
            let _ = std::fs::remove_file(&tmp); // don't leave a stray temp on failure
            eprintln!("[airplayd] peerstore: persist to {} failed: {e} (in-memory only)", self.path.display());
        }
    }
}
impl PeerStore for DiskPeers {
    fn find_peer(&self, id: &[u8]) -> Option<[u8; 32]> {
        plock(&self.map).get(id).copied()
    }
}
impl PeerSaver for DiskPeers {
    fn save_peer(&mut self, id: &[u8], ltpk: [u8; 32]) {
        // Insert + snapshot under the lock, then persist outside it (no file I/O while holding the map).
        let snapshot = {
            let mut m = plock(&self.map);
            m.insert(id.to_vec(), ltpk);
            eprintln!("[airplayd] pair-setup: peer saved ({}-byte id) → persisting", id.len());
            m.clone()
        };
        self.persist(&snapshot);
    }
}

/// Diagnostic: exercise the local MfiSigner on the chip (cert + a dummy signature).
fn mfi_selftest() {
    let mut signer = LocalMfiSigner::open().unwrap_or_else(|e| {
        eprintln!("[airplayd] open /dev/i2c-1: {e}");
        std::process::exit(1);
    });
    match signer.device_version() {
        Some(v) => println!("[airplayd] MFi chip open (local i2c), DeviceVersion=0x{v:02x}"),
        None => {
            eprintln!("[airplayd] MFi DeviceVersion read failed");
            std::process::exit(1);
        }
    }
    match signer.copy_certificate() {
        Ok(cert) => println!("[airplayd] copy_certificate: {} bytes  first8={}", cert.len(), hex8(&cert)),
        Err(e) => {
            eprintln!("[airplayd] copy_certificate failed: {e}");
            std::process::exit(1);
        }
    }
    match signer.create_signature(&[0x5Au8; 20]) {
        Ok(sig) => println!("[airplayd] create_signature(20B digest): {} bytes  first8={}", sig.len(), hex8(&sig)),
        Err(e) => {
            eprintln!("[airplayd] create_signature failed: {e}");
            std::process::exit(1);
        }
    }
    println!("[airplayd] LOCAL MfiSigner OK");
}

/// Arm TCP keepalive on the iPhone-facing control socket so an abrupt link loss is detected in ~12 s
/// (task #27), instead of falling only to the 30 s read-timeout backstop. Apple's CarPlaySDK sets
/// `SocketSetKeepAlive(sock, kAirPlayDataTimeoutSecs/10, 3)` = idle 3 s / interval 3 s / 3 probes in
/// `AirPlayReceiverSessionSetup`. LINUX idle option is `TCP_KEEPIDLE` (Darwin uses `TCP_KEEPALIVE`);
/// `SO_KEEPALIVE` alone leaves the ~2 h system default, so all four options are required. The fast
/// dead-transport detect must NOT be gated behind the 30 s A/V-idle backstop (they are separate signals).
fn arm_keepalive(stream: &std::net::TcpStream) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    // SAFETY: `fd` is a live socket owned by `stream` for this call; each call writes one c_int option.
    unsafe fn set(fd: i32, level: i32, name: i32, val: libc::c_int) -> io::Result<()> {
        let r = libc::setsockopt(
            fd,
            level,
            name,
            &val as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        if r != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    unsafe {
        set(fd, libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1)?;
        set(fd, libc::IPPROTO_TCP, libc::TCP_KEEPIDLE, 3)?; // Linux idle seconds (Darwin: TCP_KEEPALIVE)
        set(fd, libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, 3)?; // probe interval
        set(fd, libc::IPPROTO_TCP, libc::TCP_KEEPCNT, 3)?; // probe count
    }
    Ok(())
}

/// Accessory identity. MUST match rx-connect's advertised identity so the iPhone associates the pairing
/// with the mDNS-advertised accessory: the device id (AirPlay device id / MAC) + pairing identity
/// (HomeKit `pi`) + Ed25519 seed. These are pairing identity, NOT vehicle config — the YAML never
/// touches them.
///
/// PER-BOX (finding #636): fleet-fixed constants meant every CCPA collapsed into ONE iOS "car" record.
/// carplay-wireless now derives a per-box identity from hardware and passes it via env; airplayd (and
/// rx-connect) read it here, falling back to the historical fixed values when unset (the wired path,
/// where the single USB head-unit makes collisions moot). Computed once via OnceLock.
const DEVICE_ID_FALLBACK: &str = "AA:BB:CC:DD:EE:01";
const PI_FALLBACK: &str = "dd52a0b0-5435-42ea-99dc-0a6b96bb433b";
const ED_SEED_FALLBACK: [u8; 32] = [
    0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00,
    0x0f, 0x1e, 0x2d, 0x3c, 0x4b, 0x5a, 0x69, 0x78, 0x87, 0x96, 0xa5, 0xb4, 0xc3, 0xd2, 0xe1, 0xf0,
];

fn device_id() -> &'static str {
    static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    V.get_or_init(|| std::env::var("CARPLAY_DEVICE_ID").unwrap_or_else(|_| DEVICE_ID_FALLBACK.into()))
}

fn pairing_identity() -> &'static str {
    static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    V.get_or_init(|| std::env::var("CARPLAY_PI").unwrap_or_else(|_| PI_FALLBACK.into()))
}

/// The Ed25519 accessory-key seed. From `CARPLAY_ED_SEED` (64 hex chars) when carplay-wireless supplies
/// a per-box seed; otherwise the fixed fallback. A malformed/short env value falls back (never panics).
fn accessory_ed_seed() -> [u8; 32] {
    match std::env::var("CARPLAY_ED_SEED") {
        // Require ASCII too (not just len==64): a 64-BYTE value with a multibyte UTF-8 char would make
        // the `&h[i*2..i*2+2]` slice cross a char boundary and panic — fatal under panic="abort".
        Ok(h) if h.len() == 64 && h.is_ascii() => {
            let mut seed = ED_SEED_FALLBACK;
            let mut ok = true;
            for (i, b) in seed.iter_mut().enumerate() {
                match u8::from_str_radix(&h[i * 2..i * 2 + 2], 16) {
                    Ok(v) => *b = v,
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                seed
            } else {
                ED_SEED_FALLBACK
            }
        }
        _ => ED_SEED_FALLBACK,
    }
}

/// The ephemeral YAML config ocbmd lands on SUBSCRIBE (task #5 / docs/carplay/04_CAPABILITIES_AND_CONFIG.md). Absent when no host is
/// subscribed (idle) → airplayd falls back to the interim built-in default below.
const CARPLAY_CFG_FILE_DEFAULT: &str = "/tmp/carplay_cfg.yaml";

/// Where to read the pushed config from, overridable by `CARPLAY_CFG_FILE`.
///
/// The override exists for the Raspberry Pi / AAOS port, where the config producer is an ANDROID
/// APP rather than ocbmd. An app process cannot write `/tmp` (`EACCES` — it is not in the app
/// sandbox's writable set), so `com.carlink.projection` writes into its own data directory and
/// points us at it. airplayd runs as root there, so reading an app-private path is fine.
///
/// Same opt-in shape as every other Pi-port override (`CARPLAY_MFI_ADDR`, `CARPLAY_STATE_DIR`,
/// `CARPLAY_HOSTAPD_CONF`): unset, the CCPA behaves exactly as before. Resolved once and cached —
/// `getenv`/`setenv` are not mutually thread-safe and this is read from the per-connection config
/// load while other threads may still be arming levers.
fn carplay_cfg_file() -> &'static str {
    static CACHED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHED.get_or_init(|| {
        std::env::var("CARPLAY_CFG_FILE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| CARPLAY_CFG_FILE_DEFAULT.to_string())
    })
}

/// The effective advertised display resolution, shared with the HID-input ingest so touch scales to the
/// SAME geometry `/info` advertised (task #20 ↔ #5). Updated by `load_device_config` per connection.
static DISPLAY_WH: Mutex<(u16, u16)> = Mutex::new((1920, 720));

/// Live state of the two touch contacts, for the multi-touch HID report.
///
/// This exists because the two sides disagree about framing. OCBM sends ONE finger per
/// `INPUT_TOUCH` frame, but Apple's two-finger descriptor declares both `Finger` collections in a
/// SINGLE input report — so a frame for finger 1 has to be combined with whatever finger 0 is
/// currently doing before anything can be sent. Without this the second contact would overwrite the
/// first and pinch would read as a jumping single touch.
///
/// `finger` is the host's id from the wire; the slot INDEX is what goes out as Apple's transducer
/// index, so a contact keeps a stable identity for as long as it is down even if the host renumbers.
#[derive(Clone, Copy, Default)]
struct Contact {
    down: bool,
    finger: u8,
    nx: f64,
    ny: f64,
}
static CONTACTS: Mutex<[Contact; 2]> = Mutex::new([Contact { down: false, finger: 0, nx: 0.0, ny: 0.0 }; 2]);

/// Route a wire finger id to a contact slot: the slot already tracking it, else a free one.
///
/// Returns `None` when both slots are held by other fingers — a third finger, which Apple's
/// descriptor cannot express. Dropping it is correct and deliberate: the alternative is evicting a
/// live contact, which would break the pinch already in progress.
fn contact_slot(c: &[Contact; 2], finger: u8, phase: u8) -> Option<usize> {
    if let Some(i) = c.iter().position(|s| s.down && s.finger == finger) {
        return Some(i);
    }
    if phase == ocbm_proto::TOUCH_UP {
        return None; // an UP for a contact we are not tracking
    }
    c.iter().position(|s| !s.down)
}

/// The built-in default config: fixed identity + the interim 1920×720 (docs/carplay/06_AV_PIPELINE.md). Used verbatim when no
/// host config is present, and as the base a host `VehicleConfig` overlays onto.
fn base_device_config() -> DeviceConfig {
    DeviceConfig {
        device_id: device_id().into(),
        name: "CarLink".into(),
        pairing_identity: pairing_identity().into(),
        display_width: 1920,
        display_height: 720,
        ..Default::default()
    }
}

/// The `CARPLAY_APP_SETUP` harness override: `Some(true)`/`Some(false)` for "1"/"0", `None` when
/// unset/other. It BEATS the YAML in both directions (a "0" can force the proven box-driven path even
/// when a config asks for the relay), and it is deliberately re-read per connection rather than
/// cached: airplayd re-reads the whole config per control connection anyway, so this stays consistent
/// with how a config change lands (the reconnect-consumed class, docs/carplay/06_AV_PIPELINE.md) — unlike the receiver-side
/// `CARPLAY_RELAY_*` knobs, which are process-cached.
fn appsetup_override() -> Option<bool> {
    match std::env::var("CARPLAY_APP_SETUP").ok().as_deref() {
        Some("1") => Some(true),
        Some("0") => Some(false),
        _ => None,
    }
}

/// Reset every per-connection lever to its no-config baseline: plain `false` for the schema-driven
/// ones, the sanctioned bench env for the four that have one. Replaces the old `remove_var` block
/// (see the setenv-UB note in [`load_device_config`]).
///
/// THE RULE: a lever clears to its ENV form iff that env form is a sanctioned app-less bench
/// override — cornerMasks (docs/carplay/06_AV_PIPELINE.md §4), its logTransfer twin (docs/carplay/04_CAPABILITIES_AND_CONFIG.md), mainBuffered (docs/carplay/04_CAPABILITIES_AND_CONFIG.md
/// B4), and appDrivenSetup's `CARPLAY_APP_SETUP` harness switch. This function runs ONLY on the
/// no-config / parse-failure paths, i.e. exactly the app-less case. The rest clear to plain `false`
/// because their env form is vestigial (`CARPLAY_HEVC`, docs/carplay/04_CAPABILITIES_AND_CONFIG.md) or the value is schema-derived
/// from the YAML (dPad/knob/telephony from `hidConfig`, altScreen + dims from `altVideoStreams[]`,
/// viewAreas from the safeArea/enable). Either way a parsed config overwrites the lever afterwards,
/// so nothing here can decide anything for a configured connection.
fn clear_levers() {
    levers::set_hevc(false);
    // mainBuffered falls back to the bench env on these no-config / parse-failure paths only (NOT
    // plain false, and NOT the cornerMasks OR-force-arm shape — a stale bench flag must never
    // override an app's pushed `false`, which the config path below guarantees by setting the
    // lever from the YAML value alone, so with an app connected the env is inert).
    levers::set_mainbuffered(std::env::var_os("CARPLAY_MAINBUFFERED").is_some());
    events::set_dpad_advertised(false);
    events::set_knob_advertised(false);
    events::set_telephony_advertised(false); // audit Fix #14: was missing — a stale telephony advert
                                             // (5th HID) survived a bad/removed config into the next session
    // Same class of hazard, and worse here: a stale multi-touch advert would leave the HID-ingest
    // path emitting 12-byte reports at a session whose descriptor declares 5.
    events::set_multi_touch_advertised(false);
    *plock(&CONTACTS) = Default::default();
    levers::set_altscreen(false);
    levers::set_alt_dims(None);
    levers::set_viewareas(false);
    // Sanctioned bench overrides (see THE RULE above): `CARPLAY_CORNERMASKS` (docs/carplay/06_AV_PIPELINE.md §4) and its
    // logTransfer twin (docs/carplay/04_CAPABILITIES_AND_CONFIG.md). Clearing them to `false` here made both dead on the very first
    // connection — the app-less path is the only one they exist for. Presence-shaped, so the exact
    // twin is `set_mainbuffered` above, NOT `set_appsetup` below (that one is a tri-state VALUE
    // override where "0" can force OFF). Caveat worth keeping straight: on the CONFIG path these two
    // are armed as `config || env`, so the env DOES override a pushed `false` there — deliberately,
    // unlike mainBuffered which is strictly YAML-wins. This line only stops the app-less paths from
    // wiping the seed.
    levers::set_cornermasks(std::env::var_os("CARPLAY_CORNERMASKS").is_some());
    // docs/carplay/04_CAPABILITIES_AND_CONFIG.md #25 — the NO-CONFIG floor must reproduce the pre-gating wire exactly: the appearance
    // keys were emitted unconditionally and viewAreaSupportsFocusTransfer was hardcoded false. These
    // are NOT env-overridable, so they clear to constants rather than to env presence.
    levers::set_ui_appearance(true);
    levers::set_map_appearance(true);
    levers::set_focus_transfer(false);
    levers::set_logtransfer(std::env::var_os("CARPLAY_LOGTRANSFER").is_some());
    // appDrivenSetup clears to the env override (not plain false): a harness run with
    // CARPLAY_APP_SETUP=1 must still relay when no YAML landed (the no-config / parse-failure paths
    // both come through here).
    levers::set_appsetup(appsetup_override().unwrap_or(false));
}

/// Resolve the effective `DeviceConfig` for the connection about to be served. Reads the host-pushed
/// `VehicleConfig` YAML if ocbmd has landed one; on absence OR any parse error, logs and falls back to
/// the built-in default — a bad config can never take the box off the air. Read PER connection so a
/// config pushed at SUBSCRIBE is honored on the next session (the reconnect-consumed class, docs/carplay/06_AV_PIPELINE.md).
///
/// Also returns the config's CRC-32 (of the YAML bytes actually loaded; 0 when the built-in default
/// is in effect). The app-driven-SETUP relay echoes it in RS_OPEN so the host can verify THIS
/// connection's `/info` was built from the config it pushed at SUBSCRIBE — a mismatch means the box
/// is serving a stale/different config and the host should echo the local response verbatim.
fn load_device_config() -> (DeviceConfig, u32) {
    let base = base_device_config();
    let mut cfg_crc: u32 = 0;
    // Bound the host-pushed YAML before parsing (audit R6): serde_yaml is a recursive-descent parser, so
    // a pathologically large / deeply-nested config could drive it into a stack-overflow-abort. A real
    // VehicleConfig is a few KB; anything past this cap is treated as absent (fall back to base).
    const MAX_CFG_BYTES: usize = 256 * 1024;
    let cfg_path = carplay_cfg_file();
    let dev = match std::fs::read(cfg_path) {
        Ok(bytes) if !bytes.is_empty() && bytes.len() <= MAX_CFG_BYTES => match VehicleConfig::from_yaml(&bytes) {
            Ok(vc) => {
                let dev = vc.apply(base);
                // HEVC lever (user directive 2026-07-12): the receiver's two box-side gates
                // (`hevcInfo` in /info + `enabledFeatures:["hevc"]` in SETUP phase 1) both read
                // the lever dynamically, so arming it here makes the host-pushed
                // `accessoryConfig.enablesHEVC` the single source of truth, re-evaluated per
                // connection. Default/parse-failure/idle = off = the proven H.264 path.
                //
                // ALL of these used to be published via std::env::set_var/remove_var. That was
                // genuine UB: this runs on the serve thread on EVERY control connection while the
                // HID-ingest thread (and a hijacking connection's reads in info.rs/session.rs)
                // concurrently getenv the same names — musl's setenv frees the old value string and
                // unsetenv memmoves __environ, so a concurrent reader can SIGSEGV the daemon under
                // panic="abort". Now they go through `receiver::levers`, the atomic mirror pattern
                // `events::set_dpad_advertised` pioneered.
                // Metadata declaration tier for the AirPlayTunnel arm (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B3), parsed from the
                // SAME bytes this connection just read and CRC'd — not a second read of the path,
                // which could see a different generation than the CRC describes. The tier is an
                // iAP2-side concern the receiver's VehicleConfig deliberately does not carry, hence
                // the separate struct over the same document. Note this sits in the parse-SUCCESS
                // branch: a document the receiver rejects arms no tier either (conservative, and the
                // two parsers can fail independently). `arm_pushed_policy` is first-arm-wins, and
                // `iap_tunnel::start` (which builds the tunnel Identify) runs at session start —
                // strictly after this — so the declaration and the subscribes share one snapshot.
                iap2_core::config::Iap2Config::from_slice(&bytes).arm_metadata_policy();
                levers::set_hevc(vc.accessory_config.enables_hevc);
                // mainBuffered from the pushed config alone (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B4) — deliberately NO env OR:
                // a stale bench flag must never override an app that said `false` (media-silence
                // hazard). The bench env applies only on the no-config paths via clear_levers.
                levers::set_mainbuffered(vc.accessory_config.enables_main_buffered_audio);
                // D-Pad from Apple's schema `videoStreamsConfig.mainVideoStream.hidConfig.dPadSupport`.
                events::set_dpad_advertised(vc.dpad_support());
                // Knob (uid 4) from `hidConfig.knobSupport` — the Simulator's real navigation device.
                events::set_knob_advertised(vc.knob_support());
                // Telephony (uid 5) from `hidConfig.telephonyButtonsSupport` — Answer/End/Flash/DTMF.
                events::set_telephony_advertised(vc.telephony_support());
                // Two-finger touchscreen from `hidConfig.touchScreenSupportsMultiTouch`. This swaps the
                // uid-1 DESCRIPTOR, so the contact state has to start empty or the first report of the
                // new session would carry a contact left down by the old one.
                events::set_multi_touch_advertised(vc.multi_touch_support());
                *plock(&CONTACTS) = Default::default();
                // ALT / cluster screen from `videoStreamsConfig.altVideoStreams[]` (docs/carplay/06_AV_PIPELINE.md). Arms the
                // 2nd displays[] entry + altScreen feature + the type-111 SETUP forward path.
                levers::set_altscreen(vc.alt_screen());
                // Set ALT_W/H when dimensions are given; otherwise CLEAR them so info.rs falls back to
                // its 800×480 default rather than reading stale dims leaked from a prior connection
                // (audit M-c). Cleared on every alt-off/failure path below too.
                levers::set_alt_dims(if vc.alt_screen() { vc.alt_dimensions() } else { None });
                // viewAreas / safeArea (curved-panel UI inset). The geometry travels on `dev`
                // (main_safe_area / alt_safe_area, applied above); this lever only controls whether
                // SETUP echoes "viewAreas" in enabledFeatures — iOS honors an inset safeArea ONLY when
                // that feature is negotiated. Set when the host enables it or defines a real inset.
                levers::set_viewareas(vc.view_areas_enabled());
                // cornerMasks (docs/carplay/06_AV_PIPELINE.md): config-driven via accessoryConfig.enablesCornerMasks. The env
                // CARPLAY_CORNERMASKS (dev /tmp/cornermask_test flag) still force-arms it as an override.
                levers::set_cornermasks(
                    vc.corner_masks_enabled()
                        || std::env::var_os("CARPLAY_CORNERMASKS").is_some(),
                );
                // logTransfer (docs/carplay/04_CAPABILITIES_AND_CONFIG.md Half A): config-driven via accessoryConfig.enablesLogTransfer,
                // with the env CARPLAY_LOGTRANSFER as a dev force-arm override, mirroring cornerMasks.
                levers::set_logtransfer(
                    vc.log_transfer_enabled()
                        || std::env::var_os("CARPLAY_LOGTRANSFER").is_some(),
                );
                // docs/carplay/04_CAPABILITIES_AND_CONFIG.md #25 — three advertisements the box used to decide for itself while the app
                // shipped owner toggles for them. No env force-arm: unlike cornerMasks/logTransfer
                // these were never bench levers, and inventing one would add a second source of truth
                // for a value the app already owns.
                levers::set_ui_appearance(vc.ui_appearance_enabled());
                levers::set_map_appearance(vc.map_appearance_enabled());
                levers::set_focus_transfer(vc.focus_transfer_enabled());
                // appDrivenSetup (plan P1): config-driven, with the CARPLAY_APP_SETUP=1/0 env
                // override beating the YAML in BOTH directions (harness runs; a "0" force-reverts a
                // relay-enabled config to the proven box-driven path without repushing).
                levers::set_appsetup(
                    appsetup_override().unwrap_or(vc.app_driven_setup()),
                );
                // The crc the relay's RS_OPEN reports: computed over the exact YAML bytes this
                // connection's /info was built from (only on THIS success path — a rejected config
                // configured nothing, so it reports 0 like the default).
                cfg_crc = ocbm_proto::crc32(&bytes);
                println!(
                    // mainBuffered is logged because it is the one arm here whose failure mode is
                    // SILENT MEDIA — an operator diagnosing dead audio must be able to see whether
                    // this session advertised it, without capturing /info.
                    "[airplayd] cfg: {cfg_path} ({} B) → {}×{}@{} hevc={} dpad={} viewAreas={} mainBuffered={} safe={:?}",
                    bytes.len(),
                    dev.display_width,
                    dev.display_height,
                    dev.max_fps,
                    vc.accessory_config.enables_hevc,
                    vc.dpad_support(),
                    vc.view_areas_enabled(),
                    vc.accessory_config.enables_main_buffered_audio,
                    dev.main_safe_area,
                );
                dev
            }
            Err(e) => {
                // LOUD ON PURPOSE (docs/carplay/04_CAPABILITIES_AND_CONFIG.md #26). This is not a routine fallback — it is the OWNER'S
                // SETTINGS BEING DISCARDED, and the blast radius is the whole document, not the one
                // bad key. This exact path fired on 2026-08-11 when a glued newline in the app's
                // emitter made the alt-ON document unparseable: resolution, HEVC, appDrivenSetup,
                // audio AND the metadata tier all reverted at once, and the single quiet line here
                // read like normal operation. serde_yaml reports line and column — print them.
                eprintln!("[airplayd] *** PUSHED CONFIG REJECTED — {e} ***");
                eprintln!(
                    "[airplayd] *** the ENTIRE document was discarded, not just the bad key: \
                     reverting to {}×{}, HEVC off, box-driven SETUP, default audio, compiled \
                     metadata tier ***",
                    base.display_width, base.display_height
                );
                clear_levers(); // a bad config never leaves a stale lever armed
                base
            }
        },
        // EMPTY file. Distinct from absent: something wrote a zero-byte config, which means a push
        // produced nothing rather than the box simply being idle. Previously indistinguishable.
        Ok(bytes) if bytes.is_empty() => {
            eprintln!(
                "[airplayd] *** PUSHED CONFIG IS EMPTY (0 bytes) — falling back to compiled \
                 defaults; an app that pushed this sent nothing usable ***"
            );
            clear_levers();
            base
        }
        // OVERSIZED — refused before parsing (serde_yaml is recursive-descent; see the cap above).
        // Silently treating this as "absent" hid a config the owner really did push.
        Ok(bytes) => {
            eprintln!(
                "[airplayd] *** PUSHED CONFIG REFUSED: {} bytes exceeds the {MAX_CFG_BYTES}-byte \
                 cap — not parsed, compiled defaults used ***",
                bytes.len()
            );
            clear_levers();
            base
        }
        // A read error that is NOT "no such file" — permissions, I/O, a truncated tmpfs. Absence is
        // the normal idle state and stays quiet; anything else is worth saying out loud.
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            eprintln!("[airplayd] *** cannot read {cfg_path}: {e} — compiled defaults ***");
            clear_levers();
            base
        }
        // No host config landed (idle / not yet subscribed) — the interim built-in default (H.264).
        Err(_) => {
            clear_levers();
            base
        }
    };
    // Keep the HID-input scaling geometry in lockstep with what we advertise (task #20).
    *plock(&DISPLAY_WH) = (dev.display_width as u16, dev.display_height as u16);
    // `receiver::uplink` keeps its OWN copy for the touch path on its control socket (:9112), and
    // nothing was calling its setter — it sat at the compiled default 1920x720 forever. That path is
    // unused today (we drive touch through the :9110 HID seam, which uses the DISPLAY_WH above), but
    // a 1920x1080 panel would have had every Y coordinate scaled against 720 the moment anyone wired
    // it up, silently and only in the vertical axis. Set both from one place so they cannot disagree.
    receiver::uplink::set_display(dev.display_width as u16, dev.display_height as u16);
    (dev, cfg_crc)
}

/// HID-input ingest (task #20): a process-lifetime local listener that ocbmd relays CH_INPUT sub-frames
/// into (length-prefixed `[len u16 LE][payload]`). Each touch sub-frame becomes an encrypted
/// `hidSendReport` to the iPhone via the session's event channel (`events::send_hid_report`, wired at
/// RECORD). No-op between sessions (no event channel). Runs on its own thread; a bad frame drops only
/// that connection (ocbmd reconnects on the next event).
/// Local mic-uplink ingest seam. ocbmd connects here, streams host mic PCM (`mic <len>\n<pcm>`), and
/// reads the `uplink on/off` gate back-channel. Distinct from the :9110 HID seam so the two subsystems
/// never share framing or contend for the port.
const MIC_INGEST_ADDR: &str = "127.0.0.1:9112";

fn start_input_listener() {
    std::thread::spawn(|| {
        let listener = match TcpListener::bind("127.0.0.1:9110") {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[airplayd] HID input ingest bind 127.0.0.1:9110 failed: {e}");
                return;
            }
        };
        eprintln!("[airplayd] HID input ingest on 127.0.0.1:9110 (task #20)");
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || read_input(stream));
        }
    });
}

/// Read length-prefixed input sub-frames off one ocbmd connection until it closes/errors.
fn read_input(mut stream: TcpStream) {
    let mut hdr = [0u8; 2];
    loop {
        // Every exit is LOGGED (was three silent `break`s): a dying ingest connection looked
        // identical to "no input arriving", which cost real debugging time.
        if let Err(e) = stream.read_exact(&mut hdr) {
            eprintln!("[input] ingest connection closed on header read ({e})");
            break;
        }
        let len = u16::from_le_bytes(hdr) as usize;
        if len == 0 || len > 64 {
            // implausible sub-frame length → treat as desync and drop the connection
            eprintln!("[input] implausible sub-frame length {len} — desync, dropping the ingest connection");
            break;
        }
        let mut pl = vec![0u8; len];
        if let Err(e) = stream.read_exact(&mut pl) {
            eprintln!("[input] ingest connection closed mid-payload ({len} B expected: {e})");
            break;
        }
        handle_input_frame(&pl);
    }
}

/// Turn one CH_INPUT sub-frame into a HID report to the iPhone. Single-touch MVP (uid 1): DOWN/MOVE =
/// tip down, UP = tip up; normalized 0..=65535 coords scaled to the advertised resolution (`DISPLAY_WH`).
fn handle_input_frame(pl: &[u8]) {
    use std::sync::atomic::{Ordering};
    use portable_atomic::{AtomicU64};
    static N: AtomicU64 = AtomicU64::new(0);
    // Host-detected video-loss keyframe request (task #33): relay ForceKeyFrame to iOS over the event
    // channel — the box owns that channel, so only it can issue the command. No-op between sessions.
    if pl.first() == Some(&ocbm_proto::INPUT_KEYFRAME) {
        if events::send_force_key_frame() {
            eprintln!("[input] video gap → ForceKeyFrame requested");
        }
        return;
    }
    // ALT/cluster keyframe request: the host's nav/cluster lane gapped (e.g. a Nav Card/Map/Nav App
    // view switch). A bare forceKeyFrame only re-IDRs the main console (VideoStream.Main), so the
    // cluster (VideoStream.Alt1) must be addressed by its own stream uuid or it stays frozen on
    // undecodable P-frames — the same stream-specific call session.rs uses for the cluster on reconnect.
    if pl.first() == Some(&ocbm_proto::INPUT_KEYFRAME_ALT) {
        if events::send_force_key_frame_stream(Some(events::CLUSTER_STREAM_ID)) {
            eprintln!("[input] ALT/cluster video gap → ForceKeyFrame requested for {}", events::CLUSTER_STREAM_ID);
        }
        return;
    }
    // Media key (task #35): a complete tap on the advertised media-buttons HID device (uid 2). The
    // 1-byte report is a Consumer ARRAY index, so a tap = press report [index] then release [0]
    // (index 0 = the unassigned/released state). Both ride the encrypted event channel; no-op
    // between sessions like every other input.
    if pl.first() == Some(&ocbm_proto::INPUT_MEDIA_BTN) && pl.len() >= 2 {
        let index = pl[1];
        // The uid-2 media descriptor is a Consumer ARRAY with Logical Max 5; only 1..=5 are valid usages
        // (0 = released). Reject anything else so we never send an out-of-range array index (audit LOW).
        if !(1..=5).contains(&index) {
            eprintln!("[input] media-btn index={index} out of range 1..=5 — dropped");
            return;
        }
        let pressed = events::send_hid_report(2, &hid::media_button_report(index));
        let released = pressed && events::send_hid_report(2, &hid::media_button_report(hid::media_button::NONE));
        eprintln!("[input] media-btn index={index} sent={}", pressed && released);
        return;
    }
    // D-Pad (HID uid 3, Apple's exact HIDDPadCreateDescriptor): a 2-byte VARIABLE bitfield report,
    // NOT the Consumer array — CarPlay only routes Menu Up/Down/Left/Right/Pick as navigation from
    // this device. byte0 bit0 AC Home / bit1 AC Back; byte1 bit1 Menu Pick (Select) / bit2 Up /
    // bit3 Down / bit4 Left / bit5 Right. Tap = press report then all-zero release.
    if pl.first() == Some(&ocbm_proto::INPUT_NAV) && pl.len() >= 2 {
        // The uid-3 D-Pad device is only advertised when the D-Pad lever is set; without it iOS has no
        // device to route these reports to and silently drops them. Skip + log instead of sending into
        // the void (audit LOW). Read the mirrored ATOMIC (`receiver::levers` via events) — this runs
        // on the input thread while load_device_config writes the levers on the serve thread
        // (getenv/setenv aren't mutually thread-safe; round-2 audit finding, since generalized to
        // every runtime-written lever).
        if !events::dpad_advertised() {
            eprintln!("[input] nav 0x{:02x} dropped — D-Pad not advertised (CARPLAY_DPAD off)", pl[1]);
            return;
        }
        let (b0, b1, name): (u8, u8, &str) = match pl[1] {
            ocbm_proto::NAV_HOME => (0x01, 0x00, "home"),
            ocbm_proto::NAV_BACK => (0x02, 0x00, "back"),
            ocbm_proto::NAV_SELECT => (0x00, 0x02, "select"),
            ocbm_proto::NAV_UP => (0x00, 0x04, "up"),
            ocbm_proto::NAV_DOWN => (0x00, 0x08, "down"),
            ocbm_proto::NAV_LEFT => (0x00, 0x10, "left"),
            ocbm_proto::NAV_RIGHT => (0x00, 0x20, "right"),
            other => {
                eprintln!("[input] unknown INPUT_NAV 0x{other:02x} — dropped");
                return;
            }
        };
        let pressed = events::send_hid_report(3, &[b0, b1]);
        let released = pressed && events::send_hid_report(3, &[0x00, 0x00]);
        eprintln!("[input] dpad {name} sent={}", pressed && released);
        return;
    }
    // Knob (HID uid 4, Apple's exact HIDKnobCreateDescriptor): a 4-byte report [flags, nudge_x, nudge_y,
    // rotation]. The Simulator's real navigation device — rotation (relative ±1/detent) moves the
    // selector, ±127 on X/Y is a four-way arrow nudge, flags bit0 = Select. Tap = report then all-zero
    // release, matching the Simulator's press/async-release pairing and mirroring the D-Pad path. The
    // signed values ride as raw u8 bytes (a -127 i8 is 0x81), exactly what the HID descriptor expects.
    if pl.first() == Some(&ocbm_proto::INPUT_KNOB) && pl.len() >= 5 {
        if !events::knob_advertised() {
            eprintln!("[input] knob dropped — Knob not advertised (CARPLAY_KNOB off)");
            return;
        }
        let sent = events::send_hid_report(4, &[pl[1], pl[2], pl[3], pl[4]]);
        let released = sent && events::send_hid_report(4, &[0x00, 0x00, 0x00, 0x00]);
        eprintln!(
            "[input] knob flags=0x{:02x} nudge=({},{}) rot={} sent={}",
            pl[1], pl[2] as i8, pl[3] as i8, pl[4] as i8, sent && released
        );
        return;
    }
    // Telephony (HID uid 5, Apple's exact HIDTelephony descriptor): a 1-byte ARRAY report = the pressed
    // usage index (1 HookSwitch/answer, 2 Flash, 3 Drop/end, 4 Mute, 5..14 DTMF 0..9, 15 *, 16 #, 17 Del).
    // Tap = report the index then a 0 release, mirroring the D-Pad/Knob press/release pairing.
    if pl.first() == Some(&ocbm_proto::INPUT_TELEPHONY) && pl.len() >= 2 {
        if !events::telephony_advertised() {
            eprintln!("[input] telephony dropped — Telephony not advertised (CARPLAY_TELEPHONY off)");
            return;
        }
        let sent = events::send_hid_report(5, &[pl[1]]);
        let released = sent && events::send_hid_report(5, &[0x00]);
        eprintln!("[input] telephony idx={} sent={}", pl[1], sent && released);
        return;
    }
    // Home/Siri (task #35): AirPlay `/command` types, dispatched here because the box owns the
    // encrypted event channel. Home = requestUI (bring the CarPlay UI foreground); Siri =
    // requestSiri (fire-and-forget activation).
    if pl.first() == Some(&ocbm_proto::INPUT_COMMAND) && pl.len() >= 2 {
        let (name, sent) = match pl[1] {
            ocbm_proto::CMD_REQUEST_UI => ("requestUI", events::send_request_ui()),
            // Deprecated bare shape (iOS ignores it — kept for A/B only; docs/carplay/05_METADATA_AND_CONTROLS.md §2.4).
            ocbm_proto::CMD_REQUEST_SIRI => ("requestSiri(bare)", events::send_request_siri()),
            // Siri hold pair: siriAction INT enum buttondown=2 on press, buttonup=3 on release
            // (SDK AirPlaySiriAction — must be an integer, not a string; docs/carplay/05_METADATA_AND_CONTROLS.md §2.4b).
            //
            // ⚠️ OFF-BY-ONE FIXED 2026-07-30. These were 1 and 2, i.e. we sent **prewarm** on press and
            // **buttondown** on release, and NEVER sent buttonup — so iOS was left with the Siri button
            // logically held and the turn never closed. Apple's `AirPlaySiriAction`
            // (`AirPlayCommon.h:1363-1386`) is: 0 n/a, 1 prewarm, 2 buttondown, 3 buttonup,
            // 4 voiceactivation. Confirmed five ways: the header; the SDK string table at
            // `__DATA_CONST 0x3AC4D0` = ["n/a","prewarm","buttondown","buttonup"] indexed by raw value;
            // `RequestSiriVoiceActivationWithSample` hardcoding `mov w1,#0x4`; the Simulator's Swift
            // `AirPlaySiriAction.rawValue` getter being literally `caseIndex + 1`; and cp.log:1802-1805,
            // where a real press/release logs `Siri Action - 2` then `Siri Action - 3`.
            //
            // The host UI was already right — `ControlsWindow.swift:53/57` displays "siriAction:2
            // buttondown" / "siriAction:3 buttonup". Only the box disagreed with its own readout.
            ocbm_proto::CMD_SIRI_DOWN => ("requestSiri(buttondown=2)", events::send_request_siri_action(2)),
            ocbm_proto::CMD_SIRI_UP => ("requestSiri(buttonup=3)", events::send_request_siri_action(3)),
            // Navigation/cluster video runtime toggle — dynamically start/stop iOS's 2nd encoder for the
            // already-advertised type-111 display (no reconnect). = old firmware Cmd 508/509.
            // ⚠️ COMMAND CORRECTED 2026-07-30 — these four all used to send `requestUI` / `stopUI`
            // with a URL. That was the wrong command entirely, and is the root cause of "AltVideo
            // starts and streams but its content can never be selected":
            //   * `requestUI` carries NO uuid (`_AirPlayReceiverSessionRequestUI` @0x26bf20 writes only
            //     params["url"]), so it CANNOT address VideoStream.Alt1 — it is the "bring the accessory
            //     UI forward" command, not cluster content selection.
            //   * `stopUI` IGNORES a url (`@0x26ceb0` writes only params["uuid"]), so `send_stop_ui_url`
            //     produced an unaddressed stop that named no stream at all.
            // Apple's own cluster picker (`SessionClusterContentView.updateShowUI` @0x1001386bc) calls
            // `sendShowUI(videoStreamID:url:)` / `sendStopUI(videoStreamID:)` → `showUI` / `stopUI`,
            // both keyed by params["uuid"]. `requestUI` has four callers in the Simulator and none of
            // them is the picker.
            //
            // The query string is deliberate too: every `showUI` the Simulator sends for the map and
            // bare-cluster surfaces carries `showSpeedLimit`/`showCompass`/`showETA` — those ARE "the
            // elements inside the navigation video". `instructioncard` never carries a query, matching
            // Apple. Values are Apple's own vocabulary (`user`/`no`, `yes`/`no`).
            // ⚠️ ALT-SCREEN GUARD, added 2026-07-30. `showUI`/`stopUI` name a display by uuid, and
            // `ALT_DISPLAY_UUID` only appears in `/info displays[]` when CARPLAY_ALTSCREEN is armed
            // (info.rs, from the host YAML's `altVideoStreams`). Without it we would address an entity
            // we never declared — iOS drops the command silently, with no error. That is precisely the
            // limitedUI bug class, and it was reintroduced by this morning's showUI change: the old
            // `requestUI` form carried no uuid, so it had nothing to be inconsistent with.
            ocbm_proto::CMD_NAV_START
            | ocbm_proto::CMD_NAV_STOP
            | ocbm_proto::CMD_NAV_CARD
            | ocbm_proto::CMD_NAV_APP
            | ocbm_proto::CMD_NAV_APPEARANCE
            | ocbm_proto::CMD_NAV_ZOOM_IN
            | ocbm_proto::CMD_NAV_ZOOM_OUT
                if !levers::altscreen() =>
            {
                eprintln!(
                    "[input] cluster command 0x{:02x} ignored — alt screen not advertised \
                     (no altVideoStreams in the host YAML), so {} is not in /info displays[] and \
                     iOS would drop the showUI/stopUI silently",
                    pl[1],
                    receiver::info::ALT_DISPLAY_UUID
                );
                return;
            }
            ocbm_proto::CMD_NAV_START => {
                events::set_nav_forward(true);
                NAV_SURFACE.store(SURFACE_MAP, std::sync::atomic::Ordering::Relaxed);
                ("navStart showUI(map)",
                 events::send_show_ui(events::CLUSTER_STREAM_ID, &cluster_url(SURFACE_MAP)))
            }
            ocbm_proto::CMD_NAV_STOP => {
                // Stop forwarding the cluster over OCBM (free the pipe) AND release focus.
                events::set_nav_forward(false);
                NAV_SURFACE.store(SURFACE_NONE, std::sync::atomic::Ordering::Relaxed);
                ("navStop stopUI(cluster)", events::send_stop_ui(events::CLUSTER_STREAM_ID))
            }
            // Content-type selection: request the INSTRUCTION-CARD surface instead of the map (the
            // maneuver/ETA card is a distinct cluster content type). Requires it advertised in altScreenURLs.
            ocbm_proto::CMD_NAV_CARD => {
                events::set_nav_forward(true);
                NAV_SURFACE.store(SURFACE_CARD, std::sync::atomic::Ordering::Relaxed);
                ("navCard showUI(instructioncard)",
                 events::send_show_ui(events::CLUSTER_STREAM_ID, &cluster_url(SURFACE_CARD)))
            }
            ocbm_proto::CMD_NAV_APP => {
                events::set_nav_forward(true);
                NAV_SURFACE.store(SURFACE_APP, std::sync::atomic::Ordering::Relaxed);
                ("navApp showUI(instrumentcluster)",
                 events::send_show_ui(events::CLUSTER_STREAM_ID, &cluster_url(SURFACE_APP)))
            }
            // Appearance toggles (speed limit / compass / ETA): store the flags, rebuild the CURRENT
            // queryable surface's URL, and re-showUI it. map/app only — instructioncard carries no query.
            // Follow with a cluster forceKeyFrame so the split-seam host decoder re-syncs to the
            // re-rendered content (our host decodes VideoStream.Alt1 out-of-process, unlike Apple's
            // monolithic receiver which re-renders in place with no re-key).
            ocbm_proto::CMD_NAV_APPEARANCE => {
                let flags = pl
                    .get(2)
                    .copied()
                    .unwrap_or_else(|| NAV_APPEARANCE.load(std::sync::atomic::Ordering::Relaxed));
                NAV_APPEARANCE.store(flags, std::sync::atomic::Ordering::Relaxed);
                let surface = NAV_SURFACE.load(std::sync::atomic::Ordering::Relaxed);
                if surface == SURFACE_MAP || surface == SURFACE_APP {
                    let ok = events::send_show_ui(events::CLUSTER_STREAM_ID, &cluster_url(surface));
                    if ok {
                        let _ = events::send_force_key_frame_stream(Some(events::CLUSTER_STREAM_ID));
                    }
                    ("navAppearance re-showUI", ok)
                } else {
                    ("navAppearance stored (no active query surface)", true)
                }
            }
            // Cluster map zoom (Simulator Alt1 +/- buttons) → changeMapZoomLevel{uuid, zoomDirection}.
            ocbm_proto::CMD_NAV_ZOOM_IN => ("navZoom changeMapZoom(in=0)", events::send_change_map_zoom(0)),
            ocbm_proto::CMD_NAV_ZOOM_OUT => ("navZoom changeMapZoom(out=1)", events::send_change_map_zoom(1)),
            // Display appearance (Light/Dark) — per-display UI/Map appearance + global night mode.
            // Payload: [stream][mode] for UI/Map, [on] for night. The ALT stream is alt-screen-gated
            // exactly like the cluster commands (addressing ALT_DISPLAY_UUID before it is in /info
            // displays[] = a silent iOS drop); the MAIN display and night mode are always in play.
            ocbm_proto::CMD_UI_APPEARANCE | ocbm_proto::CMD_MAP_APPEARANCE => {
                // Require the full [cmd, stream, mode] payload — a truncated frame must DROP, not silently
                // apply a default appearance change (only the trusted host writes this channel, and it
                // always sends 4 bytes, so a short frame means corruption, not intent).
                if pl.len() < 4 {
                    eprintln!("[input] appearance frame too short ({} bytes) — dropped", pl.len());
                    return;
                }
                let stream = pl[2];
                let dark = pl[3] == ocbm_proto::APPEARANCE_MODE_DARK;
                let alt = stream == ocbm_proto::APPEARANCE_STREAM_ALT;
                if alt && !levers::altscreen() {
                    eprintln!(
                        "[input] appearance for alt display ignored — alt screen not advertised, {} \
                         not in /info displays[], iOS would drop it silently",
                        receiver::info::ALT_DISPLAY_UUID
                    );
                    return;
                }
                let uuid = if alt {
                    events::CLUSTER_STREAM_ID
                } else {
                    receiver::info::DISPLAY_UUID
                };
                if pl[1] == ocbm_proto::CMD_UI_APPEARANCE {
                    ("uiAppearanceUpdate", events::send_ui_appearance_update(uuid, dark))
                } else {
                    ("mapAppearanceUpdate", events::send_map_appearance_update(uuid, dark))
                }
            }
            ocbm_proto::CMD_NIGHT_MODE => {
                if pl.len() < 3 {
                    eprintln!("[input] nightMode frame too short ({} bytes) — dropped", pl.len());
                    return;
                }
                let on = pl[2] != 0;
                ("setNightMode", events::send_set_night_mode(on))
            }
            // limitedUI (Drive/Park) — restrict / release the CarPlay UI at runtime.
            ocbm_proto::CMD_LIMITED_UI_ON => ("setLimitedUI(true)", events::send_set_limited_ui(true)),
            ocbm_proto::CMD_LIMITED_UI_OFF => ("setLimitedUI(false)", events::send_set_limited_ui(false)),
            other => {
                eprintln!("[input] unknown INPUT_COMMAND 0x{other:02x} — dropped");
                return;
            }
        };
        eprintln!("[input] command {name} sent={sent}");
        return;
    }
    if pl.first() == Some(&ocbm_proto::INPUT_TOUCH) && pl.len() >= 7 {
        let phase = pl[1];
        let nx = u16::from_le_bytes([pl[2], pl[3]]);
        let ny = u16::from_le_bytes([pl[4], pl[5]]);
        let finger = pl[6];
        let buttons: u8 = if phase == ocbm_proto::TOUCH_UP { 0 } else { 1 };
        let (w, h) = *plock(&DISPLAY_WH);
        let (x, y) = ((nx as f64 / 65535.0 * w as f64) as u16, (ny as f64 / 65535.0 * h as f64) as u16);

        // The report layout is chosen by the ADVERTISED descriptor, never by how many fingers happen
        // to be down: a 12-byte report against a single-finger descriptor is unparseable garbage.
        let sent = if events::multi_touch_advertised() {
            let mut c = plock(&CONTACTS);
            let Some(slot) = contact_slot(&c, finger, phase) else {
                eprintln!("[input] touch finger={finger} dropped — both contacts busy (Apple's descriptor holds two)");
                return;
            };
            c[slot] = Contact {
                // Keep the contact addressable for THIS report, then release the slot below.
                down: phase != ocbm_proto::TOUCH_UP,
                finger,
                nx: nx as f64 / 65535.0,
                ny: ny as f64 / 65535.0,
            };
            let report = hid::touch_report_multi_normalized(
                0,
                u8::from(c[0].down),
                c[0].nx,
                c[0].ny,
                1,
                u8::from(c[1].down),
                c[1].nx,
                c[1].ny,
                w,
                h,
            );
            drop(c);
            events::send_hid_report(1, &report)
        } else {
            // pl[6] = finger id — the single-finger descriptor has no transducer field to carry it.
            let report = hid::touch_report_normalized(buttons, nx as f64 / 65535.0, ny as f64 / 65535.0, w, h);
            events::send_hid_report(1, &report)
        };
        let n = N.fetch_add(1, Ordering::Relaxed) + 1;
        // Observability (task #20 bring-up): log every DOWN/UP and any send failure; sample MOVEs.
        let pname = match phase {
            x if x == ocbm_proto::TOUCH_DOWN => "down",
            x if x == ocbm_proto::TOUCH_UP => "up",
            _ => "move",
        };
        if phase != ocbm_proto::TOUCH_MOVE || n.is_multiple_of(30) || !sent {
            eprintln!("[input] #{n} {pname} x={x} y={y} (of {w}×{h}) hid_sent={sent}");
        }
        return;
    }
    // Terminal arm (was a silent fall-off): an unknown first byte, or a known one whose frame is too
    // short for its branch (touch < 7 B, the others < 2 B), lands here. Name it so a framing bug or a
    // protocol addition on the ocbmd side is visible instead of vanishing.
    eprintln!(
        "[input] unhandled input frame dropped: first byte {}, len {}",
        pl.first().map(|b| format!("{b:#04x}")).unwrap_or_else(|| "<empty>".into()),
        pl.len()
    );
}

/// The AirPlay pairing server: terminate the control connection on `ncm0:5000`, run pair-setup /
/// pair-verify with the LOCAL MFi chip via the reused `ControlServer`, and report the derived session
/// secret. (mDNS advertise, then the encrypted-A/V-forward + session-key handoff over OCBM, are next.)
/// The wired supervisor reads this to learn which bearer owns the live airplayd session, so it never
/// SIGKILLs / escalates a healthy WIRELESS session (the critical QC finding). `wireless` when the
/// control peer is on the wlan0 AP subnet, `wired` otherwise; removed when no session is being served.
const TRANSPORT_FLAG: &str = "/tmp/carplay_transport";

/// Whether the control peer is on the wlan0 AP subnet (192.168.43.0/24, arriving as an IPv4-mapped
/// `::ffff:192.168.43.x`) — a WIRELESS session; a wired NCM peer is an IPv6 link-local (`fe80::…`).
/// Factored out of [`write_transport_flag`] because app-driven SETUP reports the transport to the
/// host in RS_OPEN flags bit0 — one classifier, no drift. (It gated the delegate selection too until
/// the 2026-08-10 wireless flip; the relay now runs on both transports.)
fn peer_is_wireless(stream: &TcpStream) -> bool {
    // Range check on the parsed octets, not a string substring (audit B7): a substring match could be
    // fooled by an unforeseen address rendering. Unwrap the IPv4-mapped form (`::ffff:192.168.43.x`) or a
    // bare IPv4 and test membership in 192.168.43.0/24; a wired NCM peer's `fe80::` link-local is not
    // v4-mapped → None → wired.
    stream
        .peer_addr()
        .map(|a| {
            let v4 = match a.ip() {
                std::net::IpAddr::V4(v4) => Some(v4),
                std::net::IpAddr::V6(v6) => v6.to_ipv4_mapped(),
            };
            v4.map(|ip| {
                let o = ip.octets();
                o[0] == 192 && o[1] == 168 && o[2] == 43
            })
            .unwrap_or(false)
        })
        .unwrap_or(false)
}

/// Write [`TRANSPORT_FLAG`] for the control peer (see [`peer_is_wireless`] for the classification).
/// Atomic (`.tmp` + rename) so the shell supervisor never reads a partial value.
fn write_transport_flag(stream: &TcpStream) {
    let wireless = peer_is_wireless(stream);
    let val = if wireless { "wireless" } else { "wired" };
    let tmp = format!("{TRANSPORT_FLAG}.tmp");
    if std::fs::write(&tmp, val).is_ok() {
        let _ = std::fs::rename(&tmp, TRANSPORT_FLAG);
    }
}

fn clear_transport_flag() {
    let _ = std::fs::remove_file(TRANSPORT_FLAG);
}

fn run_pairing_server() -> io::Result<()> {
    let identity = Identity::new(pairing_identity().as_bytes().to_vec(), accessory_ed_seed());

    let peers = DiskPeers::new();
    let listen = "[::]:5000";
    let listener = TcpListener::bind(listen)?;
    println!(
        "[airplayd] pairing server on {listen} · device 'CarLink' ({}) · setup 3939 · \
         config {} (per-connection)",
        device_id(),
        carplay_cfg_file()
    );

    // SESSION HIJACK (critical QC finding). The old loop served ONE control connection at a time,
    // blocking in serve_connection, so a second (preempting) phone starved in the TCP backlog until the
    // first tore down — and the wired supervisor's establishment-stall watchdog then escalated that
    // starvation all the way to REBOOTING the box at a perfectly healthy phone. Apple's receiver instead
    // HIJACKS (`_HijackHTTPServerConnections`): it accepts the newcomer and tears the incumbent down.
    // An accept thread does exactly that here — each new connection shuts down the current session's
    // stream (unblocking its serve_connection at once, whose AvSession then Drops → reset()), then hands
    // the new stream to the serve loop. Last-connect-wins, no backlog starvation.
    let (tx, rx) = std::sync::mpsc::channel::<TcpStream>();
    let current: std::sync::Arc<std::sync::Mutex<Option<TcpStream>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let accept_current = current.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let stream = match conn {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[airplayd] accept: {e}");
                    continue;
                }
            };
            if let Some(s) = plock(&accept_current).as_ref() {
                let _ = s.shutdown(std::net::Shutdown::Both);
                eprintln!("[airplayd] hijack: new control connection preempts the current session");
            }
            if tx.send(stream).is_err() {
                break; // serve loop gone
            }
        }
    });

    while let Ok(mut stream) = rx.recv() {
        // Fix #8 (queued-connection starvation): the accept thread shuts down only the *registered
        // serving* stream, never streams still queued in the channel. Drain to the NEWEST queued
        // connection here, shutting down the older ones, so a connection can't wait behind an older
        // queued one for a full session lifetime — true last-connect-wins, matching the hijack model.
        while let Ok(newer) = rx.try_recv() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            eprintln!("[airplayd] hijack: superseded a queued control connection with a newer one");
            stream = newer;
        }
        let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
        println!("[airplayd] ── control connection from {peer} ──");
        // Register as the current (hijackable) session + publish the bearer IMMEDIATELY — before the
        // per-connection setup (config read, MFi open) — so a connection preempting this one during that
        // setup window still finds a `current` to shut down (narrows the registration race the verifier
        // flagged). Cleared on EVERY exit below (serve end, hijack, or the signer-open early-out).
        write_transport_flag(&stream);
        if let Ok(c) = stream.try_clone() {
            *plock(&current) = Some(c);
        }
        // Resolve the effective /info for THIS connection from the host's ephemeral YAML (or the
        // built-in default when idle). Per-connection so a SUBSCRIBE-pushed config lands on the next
        // session — the reconnect-consumed class the resolution lever lives in (docs/carplay/06_AV_PIPELINE.md / docs/carplay/04_CAPABILITIES_AND_CONFIG.md).
        // cfg_crc identifies the loaded YAML for the SETUP relay's RS_OPEN (0 = built-in default).
        let (dev, cfg_crc) = load_device_config();
        let info = build_info(&dev);
        // 30 s read-timeout = quiet-but-live backstop (resilience #3). TCP keepalive 3/3/3 = fast
        // dead-transport detect (~12 s, resilience #2 / Apple 3/3/3) — arm both; they are independent.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
        match arm_keepalive(&stream) {
            Ok(()) => eprintln!("[airplayd] control keepalive armed (3s/3s/3 → ~12s dead-link detect)"),
            Err(e) => eprintln!("[airplayd] keepalive arm failed: {e}"),
        }
        let signer = match LocalMfiSigner::open() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[airplayd] MFi open: {e}");
                *plock(&current) = None; // drop the early-out registration
                clear_transport_flag();
                continue;
            }
        };
        let mut av = receiver::session::AvSession::new();
        if let Ok(addr) = stream.peer_addr() {
            av.set_peer_addr(addr);
        }
        // Delegate selection (plan P1): app-driven SETUP wraps the unmodified AvSession in the relay
        // delegate on BOTH transports (wireless flip, owner directive 2026-08-10 — it was wired-only
        // through the previous milestone), whenever the host's config armed the lever AND ocbmd's
        // relay seam is actually attached (no host ⇒ plain AvSession, zero new behavior).
        // Wireless safety has TWO halves, and only the first is "by construction":
        //  (a) STREAMS: the host authors the types it knows (110/111, 100-102) and echoes the box's
        //      local answer for anything else — notably the type-130 RCS DataStream only wireless
        //      carries (AirPlaySetupSession.swift / setup_driver.rs "out of host-authoring scope").
        //  (b) FEATURES: phase-1 AUTHORS `enabledFeatures` rather than echoing it, and the box emits
        //      two tokens on wireless the app has no config key for (`iAPChannel`,
        //      `sessionManagement`). Both twins UNION those through; stripping `iAPChannel` makes
        //      iOS 400 every `iAPSendMessage`, killing the iAP2 tunnel's DETECT+SYN. This was the
        //      gate's round-1 rejection of the flip — do not "simplify" the union away.
        // RS_OPEN reports the transport to the host in flags bit0, so the host can tell them apart.
        // The peer addr is already set on `av`; RemoteSession reads it from inner for its RS_OPEN ctx.
        let wireless = peer_is_wireless(&stream);
        let session: Box<dyn receiver::session::SessionDelegate> =
            if levers::appsetup() && receiver::relay::seam_up() {
                eprintln!(
                    "[airplayd] app-driven SETUP armed for this connection (relay seam up, {})",
                    if wireless { "wireless" } else { "wired" }
                );
                Box::new(receiver::relay::RemoteSession::new(
                    av,
                    cfg_crc,
                    wireless,
                    dev.display_width as u32,
                    dev.display_height as u32,
                ))
            } else {
                Box::new(av)
            };
        let mut server = ControlServer::new(&identity, b"3939".to_vec(), peers.clone(), signer, info)
            .verbose(true)
            .display_width(dev.display_width as u32)
            .session(session);
        let result = serve_connection(stream, &mut server);
        *plock(&current) = None;
        clear_transport_flag();
        match result {
            Ok(()) => match server.session_secret() {
                // Do NOT log the secret's bytes: it's the pair-verify ECDH shared secret that seeds every
                // A/V stream key. Log only that it was derived.
                Some(_secret) => println!("[airplayd] *** PAIR-VERIFY COMPLETE — session secret derived ***"),
                None => println!("[airplayd] connection closed (encrypted={})", server.is_encrypted()),
            },
            Err(e) => eprintln!("[airplayd] connection error: {e}"),
        }
    }
    Ok(())
}

/// Wireless lifecycle fix: in wired CarPlay the host app subscribes BEFORE the phone session starts,
/// so the stream always opens with VideoConfig+IDR while a consumer is attached. Wireless inverts
/// that — the phone's session can start (and go static, sending ~no frames) long before the Mac app
/// subscribes; a late-joining host then has no SPS/PPS/IDR to decode and no frame flow to detect a
/// gap on (the app's gap-driven keyframe path never fires on zero frames, and the screen forwarder's
/// on-connect request only fires when ITS :9001 socket reconnects — that socket belongs to ocbmd and
/// outlives host attach/detach). ocbmd mirrors host presence to /tmp/host_present: poll it and relay
/// a ForceKeyFrame to the phone on each 0→1 edge. No-op between sessions (send_command fails without
/// an event channel) and harmless wired (the flag is already 1 before the session starts, so no edge
/// fires mid-session).
fn start_host_present_watcher() {
    std::thread::spawn(|| {
        let read = || {
            std::fs::read_to_string("/tmp/host_present").map(|s| s.trim() == "1").unwrap_or(false)
        };
        let mut last = read();
        // A host-subscribe edge sets this; we retry the keyframe until the send actually succeeds
        // (#722). The old code consumed the 0→1 edge even when send_force_key_frame() returned false —
        // which it does whenever the host subscribes BEFORE RECORD has wired the event channel — so the
        // request was silently lost and the host stayed on P-frames (undecodable) until the next edge.
        let mut pending = false;
        loop {
            std::thread::sleep(Duration::from_millis(250));
            let now = read();
            if now && !last {
                pending = true; // host just (re)subscribed — it needs a fresh IDR
            }
            last = now;
            if !now {
                pending = false; // host gone — drop any stale pending request
            } else if pending && events::send_force_key_frame() {
                eprintln!("[screen] host subscribed mid-session → ForceKeyFrame requested");
                pending = false; // delivered
            }
        }
    });
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("mfi") {
        mfi_selftest();
    } else {
        // A/V mode — log LOUDLY at startup. Safe default is forward-encrypted; on-box decode is only
        // reached by an explicit OCBM_FWD_ENC opt-out, so a launcher that drops the env prefix can't
        // silently re-arm decode. If this ever prints "ON-BOX DECODE" in production, a launcher is wrong.
        eprintln!(
            "[airplayd] A/V mode: {}",
            if levers::fwd_enc() {
                "forward-encrypted — box hands the per-stream key, host decrypts (OCBM_FWD_ENC model)"
            } else {
                "ON-BOX DECODE — OCBM_FWD_ENC explicitly disabled (dev/legacy fallback)"
            }
        );
        start_input_listener(); // HID input ingest (task #20) — process-lifetime, before the accept loop
        start_host_present_watcher(); // wireless: keyframe-on-host-subscribe (fn doc)
        // Mic-uplink ingest seam — a DEDICATED port, separate from the :9110 HID seam so the working HID
        // path is untouched. ocbmd relays host mic PCM here as `mic <len>\n<pcm>` and reads the
        // `uplink on <rate> <ch>` / `uplink off` back-channel. The RTP uplink itself is armed by the
        // type-100 `input=true` MainAudio SETUP inside receiver::session (feature `mic-uplink`).
        receiver::uplink::start_control_listener(MIC_INGEST_ADDR.to_string());
        // App-driven-SETUP relay seam (plan P1) — process-lifetime like the two listeners above.
        // ocbmd connects OUT to :9106 while a host is subscribed (its eager-connect tick); with no
        // host the seam stays down and the per-connection selection falls back to plain AvSession.
        receiver::relay::start_listener();
        if let Err(e) = run_pairing_server() {
            eprintln!("[airplayd] pairing server failed: {e}");
            std::process::exit(1);
        }
    }
}
