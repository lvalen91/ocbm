//! mfi_local.rs — MFi 2.0C authentication over the box's LOCAL I2C chip (`/dev/i2c-1 @0x11`).
//!
//! The ported `bt_driver` originally called `carplay_iap2_core::mfi`, which is an auth-service TCP client
//! (`192.168.50.2:5290`) — the PoC's way of borrowing a *remote* CCPA's MFi chip because the Pi had
//! none. THIS box IS the CCPA: the genuine MFi 2.0C coprocessor sits on `/dev/i2c-1 @0x11` and is
//! driven directly, exactly as the wired `iap2d` daemon does (`ccpa/iap2d/src/main.rs`). This module
//! is that same proven direct-I2C cert-copy + challenge-sign, exposing `cert()`/`sign()` with the
//! same signatures the auth-service module had, so `bt_driver` swaps backends with a two-line change.
//!
//! One MFi chip, shared with `iap2d` — but the session arbiter guarantees only one transport is
//! active at a time, so wired and wireless never drive the chip concurrently.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Once;
use std::time::{Duration, Instant};

const MFI_ADDR: u16 = 0x11;
const I2C_M_RD: u16 = 0x0001;
const I2C_RDWR: libc::c_int = 0x0707;

static G_I2C: AtomicI32 = AtomicI32::new(-1);
static OPEN_ONCE: Once = Once::new();

/// Lazily open `/dev/i2c-1` once (O_RDWR). The I2C_RDWR ioctl carries the slave address per message,
/// so no I2C_SLAVE setup is needed — just the fd. Returns <0 if the device can't be opened.
fn i2c_fd() -> i32 {
    OPEN_ONCE.call_once(|| {
        // O_CLOEXEC: this is a PROCESS-LIFETIME fd (G_I2C, opened once) and this crate fork+execs the
        // detached A/V daemons via av::ensure_av_layer(). Without it, airplayd and rx-connect inherit an
        // open handle to the MFi chip's I2C bus and keep it for as long as they live.
        let fd = unsafe { libc::open(c"/dev/i2c-1".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            eprintln!("[mfi-local] FATAL: cannot open /dev/i2c-1 (MFi chip)");
        }
        G_I2C.store(fd, Ordering::Relaxed);
    });
    G_I2C.load(Ordering::Relaxed)
}

/// Path of the cross-process advisory lock serializing all MFi I2C access (#109). FOUR users share the
/// single `/dev/i2c-1` chip (corrected 2026-07-25 — this used to say "both daemons"): wired `iap2d`,
/// wireless `carplay-wireless` (here), `airplayd`'s `LocalMfiSigner`, and `receiver`'s tunnel handshake
/// via `mfi-i2c-local`. The cert/sign sequences are stateful (write challenge → go → poll status → read
/// result), so any interleaving corrupts both transactions. Every user `flock`s this path for the whole
/// duration of a cert()/sign(), each with a bounded 10s LOCK_NB poll (matching `airplayd`'s `MfiLock`
/// and `mfi-i2c-local`). The session arbiter also enforces single-transport, but this is the low-level
/// guarantee independent of it.
pub const MFI_LOCK_PATH: &[u8] = b"/tmp/carplay_mfi.lock\0";

/// RAII holder of the exclusive MFi lock — released (LOCK_UN + close) on drop, i.e. when the cert/sign
/// scope ends. Returns `None` if the lockfile can't be opened/locked (caller then aborts the op).
struct MfiLock(i32);
impl MfiLock {
    fn acquire() -> Option<MfiLock> {
        let fd = unsafe {
            libc::open(
                MFI_LOCK_PATH.as_ptr() as *const libc::c_char,
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
        // BOUNDED acquire (LOCK_NB + deadline), matching `ccpa/airplayd/src/main.rs`'s `MfiLock` and
        // `mfi-i2c-local`'s: a wedged holder must not block this caller forever. The legitimate worst
        // case is the sign path's ~2.1s poll x 3 retries ~= 6.3s, so 10s is a real ceiling.
        const DEADLINE: Duration = Duration::from_secs(10);
        let start = Instant::now();
        loop {
            if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Some(MfiLock(fd));
            }
            if start.elapsed() >= DEADLINE {
                eprintln!(
                    "[mfi-local] MFi lock not acquired within {}s — giving up rather than blocking \
                     the caller",
                    DEADLINE.as_secs()
                );
                unsafe { libc::close(fd) };
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
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
    let fd = i2c_fd();
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
        // I2C_RDWR returns the number of messages transferred — require BOTH (a partial transfer
        // means the read leg never happened, and `out` would be consumed uninitialized). Aligned with
        // the byte-for-byte twin in `mfi-i2c-local`, which this file must not drift from: `>= 0`
        // accepted a partial and let an all-zero buffer through as a valid signature (audit 1.6).
        if unsafe { libc::ioctl(fd, I2C_RDWR as _, &mut x) } == 2 {
            return true;
        }
        unsafe { libc::usleep(5000) };
    }
    false
}

fn i2c_wr(reg: u8, data: &[u8]) -> bool {
    let fd = i2c_fd();
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
        // I2C_RDWR returns the number of messages transferred — require the write to have landed.
        if unsafe { libc::ioctl(fd, I2C_RDWR as _, &mut x) } == 1 {
            return true;
        }
        unsafe { libc::usleep(5000) };
    }
    false
}

/// Env var selecting a REMOTE MFi service instead of this box's local i2c coprocessor.
///
/// Set it to `host:port` (e.g. `192.168.50.2:7789`) and `cert`/`sign` speak the `MFI1` protocol to
/// `mfid` instead of touching `/dev/i2c-1`. This exists for the **Raspberry Pi port**, where the Pi
/// runs the Bluetooth/iAP2 stack but has no coprocessor of its own and reaches the CCPA's over
/// USB-NCM.
///
/// UNSET is the CCPA's own case and leaves the local i2c path below byte-for-byte unchanged —
/// including `MfiLock`, which is meaningless on the remote path (the lock that matters lives on the
/// box holding the chip, and `mfid` takes it there).
const REMOTE_ADDR_ENV: &str = "CARPLAY_MFI_ADDR";

/// Resolved ONCE per process, matching the project's env-lever convention — editing the variable
/// mid-run does nothing until the daemon restarts.
fn remote_addr() -> Option<&'static str> {
    static ADDR: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    ADDR.get_or_init(|| match std::env::var(REMOTE_ADDR_ENV) {
        Ok(v) if !v.trim().is_empty() => {
            let v = v.trim().to_string();
            eprintln!("[mfi] REMOTE backend {v} — local /dev/i2c-1 will NOT be used");
            Some(v)
        }
        _ => None,
    })
    .as_deref()
}

/// MFI1 client for the remote coprocessor. Deliberately blocking and synchronous: `sign` is called
/// inside the iAP2 handshake against the phone's timeout, so no async boundary may appear here.
mod remote {
    use mfi_wire::{read_frame, write_frame, Op, Status, DIGEST_LEN};
    use std::io::Write;
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;

    const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
    /// Generous on purpose: a contended chip lock is bounded at 10 s inside the daemon and the sign
    /// path itself measures ~1.5 s over NCM. A tighter client timeout would report false failures.
    const IO_TIMEOUT: Duration = Duration::from_secs(30);

    fn exchange(addr: &str, op: Op, payload: &[u8]) -> Option<Vec<u8>> {
        let sa = match addr.to_socket_addrs().ok().and_then(|mut a| a.next()) {
            Some(sa) => sa,
            None => {
                eprintln!("[mfi] remote: cannot resolve {addr}");
                return None;
            }
        };
        let mut s = match TcpStream::connect_timeout(&sa, CONNECT_TIMEOUT) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[mfi] remote: connect {addr} failed: {e}");
                return None;
            }
        };
        let _ = s.set_read_timeout(Some(IO_TIMEOUT));
        let _ = s.set_write_timeout(Some(IO_TIMEOUT));
        let _ = s.set_nodelay(true);

        if let Err(e) = write_frame(&mut s, op as u8, payload).and_then(|()| s.flush()) {
            eprintln!("[mfi] remote: send failed: {e}");
            return None;
        }
        match read_frame(&mut s) {
            Ok((code, body)) => match Status::from_u8(code) {
                Some(Status::Ok) => Some(body),
                Some(st) => {
                    eprintln!("[mfi] remote: daemon returned {}", st.as_str());
                    None
                }
                None => {
                    eprintln!("[mfi] remote: unknown status 0x{code:02x}");
                    None
                }
            },
            Err(e) => {
                eprintln!("[mfi] remote: receive failed: {e}");
                None
            }
        }
    }

    pub fn cert(addr: &str) -> Option<Vec<u8>> {
        exchange(addr, Op::Cert, &[])
    }

    pub fn sign(addr: &str, chal: &[u8]) -> Option<Vec<u8>> {
        // Reject locally rather than let the daemon answer BadRequest — the chip would otherwise be
        // handed a short buffer and fail with an opaque NAK.
        if chal.len() != DIGEST_LEN {
            eprintln!(
                "[mfi] remote: digest is {} bytes, expected {DIGEST_LEN}",
                chal.len()
            );
            return None;
        }
        exchange(addr, Op::Sign, chal)
    }
}

/// Fetch the accessory certificate. CopyCertificate: reg `0x30` len (2 BE), reg `0x31` cert. Same
/// return shape as `carplay_iap2_core::mfi::cert()` (the auth op `0x01`), fed to iAP2 0xAA01.
pub fn cert() -> Option<Vec<u8>> {
    if let Some(addr) = remote_addr() {
        return remote::cert(addr);
    }
    let _lock = MfiLock::acquire()?; // serialize vs iap2d for the whole cert read (#109)
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

/// Sign a challenge digest. CreateSignature: write challenge (`0x20` len, `0x21` data), go
/// (`0x10`=1), poll status bit4, read `0x11` len / `0x12` sig. Same as the auth op `0x02`, fed to
/// iAP2 0xAA03.
pub fn sign(chal: &[u8]) -> Option<Vec<u8>> {
    if let Some(addr) = remote_addr() {
        return remote::sign(addr, chal);
    }
    let _lock = MfiLock::acquire()?; // hold the lock across the ENTIRE stateful sequence (#109)
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
        // Poll control-register 0x10 bit 4 = "signature ready" — byte-identical to the DEVICE-VERIFIED
        // wired iap2d path (ccpa/iap2d/src/main.rs:120). QC #124 flagged this as accepting non-ready
        // codes, but a stricter predicate would DIVERGE from the proven reference with no MFi-2.0C
        // register spec to ground it, risking a regression to working auth. The result is instead
        // validated downstream: a spurious "ready" yields a bogus 0x11 length that the `n == 0 || n >
        // 256` guard below rejects → sign() returns None → the caller's #210 retry re-drives cleanly.
        if i2c_rd(0x10, &mut st) && (st[0] & 0x10) != 0 {
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
