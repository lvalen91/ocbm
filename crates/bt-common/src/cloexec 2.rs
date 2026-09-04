//! `SOCK_CLOEXEC` / `accept4` shims — Linux-only primitives, defined here once so `rfcomm`,
//! `sdp_server` and `ssp_agent` don't each hand-roll them (they already hand-define `AF_BLUETOOTH`
//! and the `BTPROTO_*` values three times over; this at least stops that at three).
//!
//! **Why any of this matters.** `bt_driver` calls `av::ensure_av_layer()`, which fork+execs
//! `airplayd` and `rx-connect` **setsid-detached to outlive this process** (`av.rs:13`). Every
//! Bluetooth socket this crate holds is open at that moment. Without close-on-exec those daemons
//! inherit them — including the L2CAP binding on PSM 0x0001, which is a well-known single-holder
//! PSM. If `carplay-wireless` then dies without reaching `teardown_av_layer()` (SIGKILL, or a panic:
//! the profile is `panic = "abort"`, so there is no unwinding and no `Drop`), the restarted instance
//! cannot re-bind the PSM, `sdp_server::run` returns `Err`, and the box exhibits the exact failure
//! `sdp_server.rs`'s own header documents as device-confirmed: the iPhone browses SDP, finds no iAP2
//! service, and disconnects.
//!
//! `ocbmd` already got this right (`SOCK_CLOEXEC` on its `AF_PACKET` socket, with a comment saying
//! why); this crate did not. Added 2026-07-29.
//!
//! The fallback arms exist purely so the crate still compiles for `cargo test` on the macOS dev
//! host — `libc` exposes neither `SOCK_CLOEXEC` nor `accept4` there. None of that code is reachable
//! on a real target; the Bluetooth address families it is used with do not exist on macOS either.
//!
//! **The cfg MUST name `android` as well as `linux`.** Rust reports `target_os = "android"` for
//! `aarch64-linux-android`, so a bare `cfg(target_os = "linux")` silently selects the *fallback*
//! there: `SOCK_CLOEXEC` becomes `0` and `accept_cloexec` degrades to a plain `accept`. Every
//! Bluetooth socket then leaks into the detached daemons on the Raspberry Pi port — observed as
//! `RFCOMM accept error: Address already in use` after a restart, with a surviving `rx-connect`
//! still holding the PSM. This is the exact failure the module was written to prevent, reintroduced
//! by a cfg that looked target-agnostic and was not.

/// `SOCK_CLOEXEC`, to be OR'd into the `type` argument of `socket(2)`.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SOCK_CLOEXEC: libc::c_int = libc::SOCK_CLOEXEC;
/// Host-build placeholder — `socket(2)` here is never actually called (see module docs).
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub const SOCK_CLOEXEC: libc::c_int = 0;

/// `accept(2)` returning a descriptor that is already close-on-exec.
///
/// On Linux this is `accept4(SOCK_CLOEXEC)`, which sets the flag atomically. The two-step
/// `accept` + `fcntl(F_SETFD)` alternative has a window in which a concurrent `fork`+`exec` leaks the
/// descriptor; that window is real here, because `bt_driver` spawns the detached daemons while these
/// accept loops are live.
///
/// # Safety
/// `addr` must be valid for writes of `*len` bytes, and `len` must be a valid initialised pointer —
/// the same contract as `accept(2)` itself.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub unsafe fn accept_cloexec(
    fd: libc::c_int,
    addr: *mut libc::sockaddr,
    len: *mut libc::socklen_t,
) -> libc::c_int {
    libc::accept4(fd, addr, len, SOCK_CLOEXEC)
}

/// Host-build fallback: `accept` then `fcntl(F_SETFD, FD_CLOEXEC)`. Non-atomic, but unreachable off
/// the box (see module docs).
///
/// # Safety
/// Same contract as the Linux arm above.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub unsafe fn accept_cloexec(
    fd: libc::c_int,
    addr: *mut libc::sockaddr,
    len: *mut libc::socklen_t,
) -> libc::c_int {
    let cfd = libc::accept(fd, addr, len);
    if cfd >= 0 {
        libc::fcntl(cfd, libc::F_SETFD, libc::FD_CLOEXEC);
    }
    cfd
}
