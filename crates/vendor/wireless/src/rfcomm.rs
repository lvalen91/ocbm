//! RFCOMM listener for the iAP2 data channel -- accepts the connection Phase A1 confirmed the
//! iPhone attempts (and the kernel correctly rejected with Disconnect Mode, since nothing was
//! listening) right after a successful SDP lookup. No `libbluetooth` dependency, matching
//! `sdp_server.rs`'s established approach: the L2CAP/RFCOMM sockaddr structs are hand-defined.

use std::io::Error;
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::atomic::{AtomicBool, Ordering};

const AF_BLUETOOTH: libc::sa_family_t = 31;
const BTPROTO_RFCOMM: libc::c_int = 3;

#[repr(C)]
#[derive(Clone, Copy)]
struct SockaddrRc {
    rc_family: libc::sa_family_t,
    rc_bdaddr: [u8; 6],
    rc_channel: u8,
}

fn open_listener(channel: u8) -> std::io::Result<std::fs::File> {
    // SOCK_CLOEXEC: this listener is open when bt_driver drives av::ensure_av_layer(), which
    // fork+execs airplayd and rx-connect SETSID-DETACHED TO OUTLIVE THIS PROCESS (av.rs:13). Without
    // CLOEXEC those daemons inherit the RFCOMM listening fd and hold the channel after this process
    // dies, so a supervisor restart re-binds onto a channel someone else still owns.
    let fd = unsafe {
        libc::socket(
            AF_BLUETOOTH as libc::c_int,
            libc::SOCK_STREAM | crate::cloexec::SOCK_CLOEXEC,
            BTPROTO_RFCOMM,
        )
    };
    if fd < 0 {
        return Err(Error::last_os_error());
    }
    let addr = SockaddrRc {
        rc_family: AF_BLUETOOTH,
        rc_bdaddr: [0; 6],
        rc_channel: channel,
    };
    let ret = unsafe {
        libc::bind(
            fd,
            &addr as *const SockaddrRc as *const libc::sockaddr,
            std::mem::size_of::<SockaddrRc>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        let e = Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    if unsafe { libc::listen(fd, 1) } < 0 {
        let e = Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // Matches sdp_server.rs's / ssp_agent.rs's own discipline: a receive timeout on the listening fd
    // so the shutdown flag actually gets checked between accepts instead of blocking forever.
    // zeroed()+assign, not a struct literal: under `musl32_time64` (riscv32) these
    // types carry private padding and a literal does not compile.
    let mut timeout: libc::timeval = unsafe { std::mem::zeroed() };
    timeout.tv_sec = 1;
    // Checked, like bind/listen above and the house pattern (bt_driver.rs, sdp_server.rs): an
    // unnoticed failure here would leave accept() blocking forever, so the shutdown flag is never
    // polled. The accept thread logs the error and retries.
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
        let e = Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // SAFETY: fd is a freshly opened, bound, listening, exclusively-owned socket descriptor.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// Connect OUT to a peer's RFCOMM `channel` (Model B, accessory-initiated reconnect — docs/wireless/01_BT_AND_RADIO.md).
///
/// Mirrors `open_listener`'s hand-rolled `AF_BLUETOOTH`/`SOCK_STREAM`/`BTPROTO_RFCOMM` socket but
/// `connect()`s to the bonded phone instead of binding+listening. In BlueZ a connect to a peer that
/// has no ACL implicitly pages it (Create Connection), so this both pages the phone and opens the
/// iAP2 DLC in one call — the bare-HCI-page dead-end (docs/wireless/01_BT_AND_RADIO.md §Eliminated) is not needed.
///
/// `peer` is the 6-byte bdaddr in BlueZ/mgmt little-endian order — exactly as stored in the link-key
/// record (`ssp_agent::bonded_addrs`), so it passes straight through with no byte reversal.
///
/// A blocking `connect()` on a quiet or absent phone would park the reconnect thread forever, so a
/// bounded `SO_SNDTIMEO` is set first (same discipline as `bt_driver`'s accepted socket): the connect
/// returns `EINPROGRESS`/`ETIMEDOUT` instead, and the orchestrator retries with backoff. The returned
/// socket is CLOEXEC for the same reason `open_listener` is — it is live across `av::ensure_av_layer`'s
/// fork+exec of the detached daemons.
pub fn connect_to(peer: [u8; 6], channel: u8, connect_timeout_secs: i64) -> std::io::Result<std::fs::File> {
    let fd = unsafe {
        libc::socket(
            AF_BLUETOOTH as libc::c_int,
            libc::SOCK_STREAM | crate::cloexec::SOCK_CLOEXEC,
            BTPROTO_RFCOMM,
        )
    };
    if fd < 0 {
        return Err(Error::last_os_error());
    }
    // Bounded connect: SO_SNDTIMEO caps how long a blocking connect() parks on an unreachable peer.
    // tv_sec's width is target-dependent (i32 on armv7-musl, i64 on the dev host); `as _` infers the
    // field type from the struct literal so both targets build without naming the deprecated time_t alias.
    // zeroed()+assign, not a struct literal: under `musl32_time64` (riscv32) these
    // types carry private padding and a literal does not compile.
    let mut timeout: libc::timeval = unsafe { std::mem::zeroed() };
    timeout.tv_sec = connect_timeout_secs as _;
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDTIMEO,
            &timeout as *const libc::timeval as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        let e = Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    let addr = SockaddrRc {
        rc_family: AF_BLUETOOTH,
        rc_bdaddr: peer,
        rc_channel: channel,
    };
    let ret = unsafe {
        libc::connect(
            fd,
            &addr as *const SockaddrRc as *const libc::sockaddr,
            std::mem::size_of::<SockaddrRc>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        let e = Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // SAFETY: fd is a freshly opened, connected, exclusively-owned socket descriptor.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// Block until one RFCOMM client connects on `channel`, or `shutdown` is set. Returns `Ok(None)` on
/// shutdown, `Ok(Some(file))` on an accepted connection ready for iAP2 traffic.
pub fn accept_one(channel: u8, shutdown: &AtomicBool) -> std::io::Result<Option<std::fs::File>> {
    let listener = open_listener(channel)?;
    let listen_fd = listener.as_raw_fd();
    while !shutdown.load(Ordering::Relaxed) {
        let mut ra: SockaddrRc = unsafe { std::mem::zeroed() };
        let mut ralen = std::mem::size_of::<SockaddrRc>() as libc::socklen_t;
        // accept4(SOCK_CLOEXEC), not accept(): the accepted DLC is live across
        // av::ensure_av_layer()'s fork+exec of the detached daemons — see open_listener above.
        let cfd = unsafe {
            crate::cloexec::accept_cloexec(
                listen_fd,
                &mut ra as *mut SockaddrRc as *mut libc::sockaddr,
                &mut ralen,
            )
        };
        if cfd < 0 {
            let e = Error::last_os_error();
            if e.kind() == std::io::ErrorKind::WouldBlock {
                continue;
            }
            return Err(e);
        }
        // SAFETY: cfd is a freshly accepted, exclusively-owned connected socket descriptor.
        return Ok(Some(unsafe { std::fs::File::from_raw_fd(cfd) }));
    }
    Ok(None)
}
