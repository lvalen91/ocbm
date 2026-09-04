//! rfcomm_uspace.rs — RFCOMM (TS 07.10) server implemented in userspace over L2CAP.
//!
//! # Why this exists
//!
//! The Raspberry Pi AAOS kernel ships **without `CONFIG_BT_RFCOMM`**, so
//! `socket(AF_BLUETOOTH, SOCK_STREAM, BTPROTO_RFCOMM)` fails with `EPROTONOSUPPORT`. That is not an
//! oversight in the ROM: Android's own Bluetooth stack implements RFCOMM in userspace over L2CAP,
//! so AOSP never needs the kernel module. It only matters for something that drives the controller
//! directly — which is exactly what this daemon does.
//!
//! This module is the same answer Android's stack gives: speak RFCOMM ourselves on L2CAP PSM 3.
//!
//! # Shape of the thing
//!
//! [`accept_one`] deliberately mirrors `rfcomm::accept_one` — same signature, same `File` return —
//! so `bt_driver`/`main` are untouched. The `File` handed back is one end of a `socketpair`; a pump
//! thread owns the L2CAP socket and translates between the byte stream on that pair and RFCOMM UIH
//! frames. The caller therefore keeps reading and writing a plain stream and never sees framing.
//!
//! # Scope
//!
//! Both directions. [`accept_one`] is the inbound path used for first pairing; [`connect_to`] is the
//! outbound path `reconnect` uses once a phone is bonded — which is the one that actually matters in
//! practice, because after bonding iOS expects the ACCESSORY to open the iAP2 channel.
//!
//! The two differ in more than direction: as the multiplexer initiator both the C/R bit and the DLCI
//! direction bit invert. See [`cmd_cr`], [`rsp_cr`] and [`data_dlci`] — getting either wrong makes
//! the peer silently ignore every frame.
//!
//! Reference: RFCOMM 1.2 (TS 07.10 adapted). Frame/field formulas cross-checked against BlueZ's
//! `net/bluetooth/rfcomm/core.c`.

use std::io;
use std::os::unix::io::FromRawFd;
use std::sync::atomic::{AtomicBool, Ordering};

const AF_BLUETOOTH: libc::sa_family_t = 31;
const BTPROTO_L2CAP: libc::c_int = 0;
/// RFCOMM's well-known L2CAP PSM.
const RFCOMM_PSM: u16 = 0x0003;

/// Is the userspace implementation selected? `CARPLAY_RFCOMM_BACKEND=userspace` opts in.
///
/// Default is the KERNEL path, which is the proven one on the CCPA. The Raspberry Pi opts in
/// because its kernel is built without `CONFIG_BT_RFCOMM` and the kernel path cannot work there —
/// see the module header.
pub fn selected() -> bool {
    static SEL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SEL.get_or_init(|| {
        let on = std::env::var("CARPLAY_RFCOMM_BACKEND")
            .map(|v| v.trim().eq_ignore_ascii_case("userspace"))
            .unwrap_or(false);
        if on {
            eprintln!("[rfcomm-u] userspace RFCOMM selected (kernel BTPROTO_RFCOMM not used)");
        }
        on
    })
}

// ---------------------------------------------------------------- frame constants
// Control field values, all shown with the P/F bit already set where the spec requires it.
const CTRL_SABM: u8 = 0x3F; // 0x2F | P/F
const CTRL_UA: u8 = 0x73; // 0x63 | P/F
const CTRL_DM: u8 = 0x1F; // 0x0F | P/F
const CTRL_DISC: u8 = 0x53; // 0x43 | P/F
const CTRL_UIH: u8 = 0xEF; // P/F clear = no credit field
const CTRL_UIH_CREDIT: u8 = 0xFF; // P/F set = first payload byte is a credit grant

// Multiplexer-control message types, already shifted and tagged command/response.
// type byte = (type << 2) | (C/R << 1) | EA
const MCC_PN_CMD: u8 = 0x83;
const MCC_PN_RSP: u8 = 0x81;
const MCC_MSC_CMD: u8 = 0xE3;
const MCC_MSC_RSP: u8 = 0xE1;
const MCC_DISC_CMD: u8 = 0xC3; // Close Down (CLD)

/// Our maximum RFCOMM frame payload. A worst-case non-credit UIH frame is addr(1) + ctrl(1) +
/// length(2, 2-byte form for payloads >=128) + payload + FCS(1) — 5 bytes of overhead, matching
/// the kernel's own `s->mtu = min(omtu, imtu) - 5` (`rfcomm_accept_connection`,
/// `net/bluetooth/rfcomm/core.c`). 1019 = 1024 - 5, so a full frame never exceeds a 1024-byte
/// L2CAP SDU; iAP2 frames sit far below it either way.
const MAX_FRAME_SIZE: u16 = 1019;
const _: () = assert!(
    MAX_FRAME_SIZE as u32 + 1 /* addr */ + 1 /* ctrl */ + 2 /* length, 2-byte form */ + 1 /* FCS */
        <= 1024,
    "MAX_FRAME_SIZE plus worst-case frame overhead must fit a 1024-byte L2CAP SDU"
);
/// Credits we advertise in a credit UIH, and top back up as we consume the peer's traffic. This
/// field is a full octet, so 32 is legal here.
const INITIAL_CREDITS: u8 = 32;
/// Initial credits in a PN message. RFCOMM 1.2 5.5.3 makes `k` a THREE-BIT field (0..7) — BlueZ
/// sends 7. Putting 32 here masks to 0 on a conformant peer, i.e. an accidental grant of nothing.
const PN_INITIAL_CREDITS: u8 = 7;
const CREDIT_REFILL_THRESHOLD: u8 = 8;

/// CRC-8 table for the RFCOMM FCS (polynomial x^8 + x^2 + x + 1, reflected). Straight from the
/// TS 07.10 specification table.
const CRC_TABLE: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u8;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xE0
            } else {
                crc >> 1
            };
            bit += 1;
        }
        t[i] = crc;
        i += 1;
    }
    t
};

fn fcs(data: &[u8]) -> u8 {
    let mut crc = 0xFFu8;
    for b in data {
        crc = CRC_TABLE[(crc ^ b) as usize];
    }
    !crc
}

/// Address byte: `[DLCI 6][C/R 1][EA 1]`, matching BlueZ's `__addr(cr, dlci)`.
///
/// The C/R value depends on our multiplexer role, so callers pass it via [`cmd_cr`]/[`rsp_cr`]
/// rather than hardcoding it.
fn addr_byte(cr: bool, dlci: u8) -> u8 {
    ((dlci & 0x3F) << 2) | ((cr as u8) << 1) | 0x01
}

fn dlci_of(addr: u8) -> u8 {
    (addr & 0xFC) >> 2
}

/// C/R bit for a frame we ORIGINATE (SABM, DISC, and every UIH — data and multiplexer control
/// alike; the command/response distinction for mux control lives in the MCC type byte, not the
/// frame address, which is why BlueZ sends both with `__addr(s->initiator, 0)`).
fn cmd_cr(initiator: bool) -> bool {
    initiator
}

/// C/R bit for a RESPONSE we send (UA, DM).
fn rsp_cr(initiator: bool) -> bool {
    !initiator
}

/// DLCI for a data channel. `__dlci(!initiator, channel)` in BlueZ: the initiator of the
/// multiplexer addresses the responder's server channels with direction bit 0.
fn data_dlci(initiator: bool, channel: u8) -> u8 {
    (channel << 1) | u8::from(!initiator)
}

/// Encode a frame. `credits` is `Some(n)` only for a credit-bearing UIH.
fn build_frame(cr: bool, dlci: u8, ctrl: u8, payload: &[u8], credits: Option<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 6);
    out.push(addr_byte(cr, dlci));
    out.push(ctrl);

    // The credit octet is NOT counted here: RFCOMM 1.2 6.5.2 places it between the length
    // indicator and the information field, outside the length. BlueZ's rfcomm_send_credits()
    // sends len=0 followed by one credit byte. Counting it made every grant we send malformed.
    let body_len = payload.len();
    if body_len < 128 {
        out.push(((body_len as u8) << 1) | 0x01); // EA = 1, single-byte length
    } else {
        out.push(((body_len as u8) << 1) & 0xFE); // EA = 0
        out.push((body_len >> 7) as u8);
    }
    if let Some(c) = credits {
        out.push(c);
    }
    out.extend_from_slice(payload);

    // FCS covers addr+ctrl+length for framed types, but only addr+ctrl for UIH — a spec quirk that
    // is easy to get wrong and shows up as the peer silently dropping every frame.
    let fcs_span = if ctrl == CTRL_UIH || ctrl == CTRL_UIH_CREDIT {
        2
    } else {
        3
    };
    out.push(fcs(&out[..fcs_span]));
    out
}

/// A decoded inbound frame.
struct Frame {
    dlci: u8,
    ctrl: u8,
    /// Payload with any credit byte already stripped.
    payload: Vec<u8>,
    /// Credits the peer granted us on this frame.
    credits: Option<u8>,
}

/// Parse one frame out of an L2CAP packet. Returns `None` for anything malformed — the caller drops
/// it rather than trying to resynchronise, which is safe because L2CAP is packet-oriented.
fn parse_frame(buf: &[u8]) -> Option<Frame> {
    if buf.len() < 4 {
        return None;
    }
    let addr = buf[0];
    let ctrl = buf[1];
    let mut idx = 2;

    let mut len = (buf[idx] >> 1) as usize;
    if buf[idx] & 0x01 == 0 {
        // EA = 0: a second length byte follows.
        idx += 1;
        if idx >= buf.len() {
            return None;
        }
        len |= (buf[idx] as usize) << 7;
    }
    idx += 1;

    // The credit octet is outside the length field (see build_frame). Treating it as part of the
    // payload dropped every standalone grant (len=0) as malformed and truncated the last byte of
    // every piggybacked one.
    //
    // RFCOMM 6.5.2 defines the P/F-bit credit meaning only for DLCs using credit-based flow
    // control; DLCI 0 (the multiplexer control channel) never does. A P/F-set UIH on DLCI 0 from a
    // non-conformant peer must not have its MCC type byte consumed here as a credit grant.
    let mut credits = None;
    if ctrl == CTRL_UIH_CREDIT && dlci_of(addr) != 0 {
        if idx >= buf.len() {
            return None;
        }
        credits = Some(buf[idx]);
        idx += 1;
    }
    if buf.len() < idx + len {
        return None;
    }

    // Verify the trailing FCS and drop on mismatch. L2CAP already guarantees integrity, so this is
    // belt-and-braces — but a receiver that ignores the FCS cannot tell a real frame from a
    // mis-parsed one, which is exactly the failure mode this module is prone to.
    let fcs_pos = idx + len;
    if fcs_pos >= buf.len() {
        return None;
    }
    let span = if ctrl == CTRL_UIH || ctrl == CTRL_UIH_CREDIT {
        2
    } else {
        3
    };
    if buf[fcs_pos] != fcs(&buf[..span]) {
        return None;
    }

    Some(Frame {
        dlci: dlci_of(addr),
        ctrl,
        payload: buf[idx..idx + len].to_vec(),
        credits,
    })
}

// ---------------------------------------------------------------- L2CAP plumbing

#[repr(C)]
struct SockaddrL2 {
    l2_family: libc::sa_family_t,
    l2_psm: u16,
    l2_bdaddr: [u8; 6],
    l2_cid: u16,
    l2_bdaddr_type: u8,
}

// `include/net/bluetooth/l2cap.h`. Not in the libc crate — matches this file's existing hand-rolled
// Bluetooth constants (AF_BLUETOOTH, SockaddrL2 above).
const SOL_L2CAP: libc::c_int = 6;
const L2CAP_OPTIONS: libc::c_int = 0x01;

#[repr(C)]
struct L2capOptions {
    omtu: u16,
    imtu: u16,
    flush_to: u16,
    mode: u8,
    fcs: u8,
    max_tx: u8,
    txwin_size: u16,
}
const _: () = assert!(std::mem::size_of::<L2capOptions>() == 12);

/// L2CAP `imtu` we ask for. `L2CAP_DEFAULT_MTU` (672) is what the kernel leaves in place if nobody
/// asks for more, and the Configure Request only carries the MTU option when it differs from that
/// default — so without this call a compliant peer is only entitled to send 672-byte SDUs no matter
/// what MAX_FRAME_SIZE advertises in RFCOMM PN. +5 ties it to MAX_FRAME_SIZE's own derivation (see
/// that const's comment).
const L2CAP_IMTU: u16 = MAX_FRAME_SIZE + 5;

/// Raise the socket's L2CAP `imtu` above the kernel default. Must run before `connect()`/`listen()`
/// — accepted children inherit the parent's `imtu` via `l2cap_sock_init(sk, parent)`, and by the
/// time either call returns the Configure Request has already gone out at whatever `imtu` was set
/// at that point. Read-modify-write because `L2CAP_OPTIONS` is one struct covering flush_to/mode/
/// fcs/max_tx/txwin_size too — writing zeroed peers there would stomp the kernel's own defaults for
/// fields we don't care about.
fn set_l2cap_imtu(fd: libc::c_int) -> io::Result<()> {
    let mut opts: L2capOptions = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<L2capOptions>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            SOL_L2CAP,
            L2CAP_OPTIONS,
            &mut opts as *mut L2capOptions as *mut libc::c_void,
            &mut len,
        )
    } < 0
    {
        let e = io::Error::last_os_error();
        eprintln!("[rfcomm-u] getsockopt L2CAP_OPTIONS failed: {e}");
        return Err(e);
    }
    opts.imtu = L2CAP_IMTU;
    if unsafe {
        libc::setsockopt(
            fd,
            SOL_L2CAP,
            L2CAP_OPTIONS,
            &opts as *const L2capOptions as *const libc::c_void,
            std::mem::size_of::<L2capOptions>() as libc::socklen_t,
        )
    } < 0
    {
        let e = io::Error::last_os_error();
        eprintln!("[rfcomm-u] setsockopt L2CAP_OPTIONS (imtu={L2CAP_IMTU}) failed: {e}");
        return Err(e);
    }
    Ok(())
}

/// Read back the peer-accepted outgoing MTU after connect/accept. A peer that ignored or undercut
/// our `imtu` request is still only obligated to accept SDUs up to its own advertised `omtu` — the
/// kernel returns EMSGSIZE rather than fragmenting a larger one.
fn get_l2cap_omtu(fd: libc::c_int) -> io::Result<u16> {
    let mut opts: L2capOptions = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<L2capOptions>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            SOL_L2CAP,
            L2CAP_OPTIONS,
            &mut opts as *mut L2capOptions as *mut libc::c_void,
            &mut len,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(opts.omtu)
}

/// Clamp an RFCOMM-negotiated MFS to what the L2CAP transport underneath will actually carry.
/// Only applies when the peer's `omtu` came back smaller than what we asked for.
fn clamp_to_omtu(mfs: u16, omtu: Option<u16>) -> u16 {
    match omtu {
        Some(o) if o > 0 && o < L2CAP_IMTU => mfs.min(o.saturating_sub(5)).max(23),
        _ => mfs,
    }
}

/// Listener on PSM 3, opened once per process and reused across `accept_one` calls — rebinding a
/// well-known PSM on every call would race with the previous socket's TIME_WAIT.
fn listener() -> io::Result<libc::c_int> {
    // A Mutex<Option<fd>>, NOT OnceLock<Result<..>>: caching a FAILURE would permanently disable the
    // backend while `main`'s loop kept logging "retrying". A transient first-call failure (bind
    // before the controller is usable, or EADDRINUSE from a previous instance's leaked socket) must
    // be retryable. The successful fd is process-lifetime and deliberately never closed.
    static FD: std::sync::Mutex<Option<libc::c_int>> = std::sync::Mutex::new(None);
    let mut g = FD.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(fd) = *g {
        return Ok(fd);
    }
    let fd = open_listener()?;
    *g = Some(fd);
    Ok(fd)
}

fn open_listener() -> io::Result<libc::c_int> {
    // SOCK_CLOEXEC for the same reason as sdp_server's PSM-1 listener: av.rs fork+execs detached
    // daemons, and a leaked well-known PSM binding cannot be recovered without a reboot.
    let fd = unsafe {
        libc::socket(
            AF_BLUETOOTH as libc::c_int,
            libc::SOCK_SEQPACKET | crate::cloexec::SOCK_CLOEXEC,
            BTPROTO_L2CAP,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let addr = SockaddrL2 {
        l2_family: AF_BLUETOOTH,
        l2_psm: RFCOMM_PSM,
        l2_bdaddr: [0; 6],
        l2_cid: 0,
        l2_bdaddr_type: 0,
    };
    let rc = unsafe {
        libc::bind(
            fd,
            &addr as *const SockaddrL2 as *const libc::sockaddr,
            std::mem::size_of::<SockaddrL2>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        let e = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // Before listen(): accepted children inherit this imtu, so it must be set before the first
    // incoming connection can be configured against it.
    if let Err(e) = set_l2cap_imtu(fd) {
        unsafe { libc::close(fd) };
        return Err(e);
    }
    if unsafe { libc::listen(fd, 1) } < 0 {
        let e = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // 1 s accept timeout so the caller's shutdown flag is honoured promptly.
    // zeroed()+assign, not a struct literal: under `musl32_time64` (riscv32) these
    // types carry private padding and a literal does not compile.
    let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
    tv.tv_sec = 1;
    // Checked, matching rfcomm.rs/sdp_server.rs's discipline: an unnoticed failure here would leave
    // accept() blocking forever, so the shutdown flag is never polled.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &tv as *const libc::timeval as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        let e = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    eprintln!("[rfcomm-u] listening on L2CAP PSM 0x{RFCOMM_PSM:04x} (userspace RFCOMM)");
    Ok(fd)
}

fn write_all_fd(fd: libc::c_int, buf: &[u8]) -> io::Result<()> {
    let mut off = 0;
    while off < buf.len() {
        let n = unsafe {
            libc::write(
                fd,
                buf[off..].as_ptr() as *const libc::c_void,
                buf.len() - off,
            )
        };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "short L2CAP write"));
        }
        off += n as usize;
    }
    Ok(())
}

/// Read one L2CAP packet. `Ok(None)` on a receive timeout, so callers can poll a shutdown flag.
fn read_packet(fd: libc::c_int, buf: &mut [u8]) -> io::Result<Option<usize>> {
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n < 0 {
        let e = io::Error::last_os_error();
        return match e.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted => {
                Ok(None)
            }
            _ => Err(e),
        };
    }
    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "peer closed L2CAP",
        ));
    }
    Ok(Some(n as usize))
}

// ---------------------------------------------------------------- multiplexer

/// Handle one multiplexer-control message (a UIH on DLCI 0). Returns the negotiated peer MTU when
/// the message was a PN, so the session can clamp its frame size.
fn handle_mux_control(
    fd: libc::c_int,
    payload: &[u8],
    peer_mtu: &mut u16,
    peer_credits: &mut u8,
    init: bool,
) -> io::Result<()> {
    if payload.len() < 2 {
        return Ok(());
    }
    let mtype = payload[0];
    let len = (payload[1] >> 1) as usize;
    let value = &payload[2..payload.len().min(2 + len)];

    match mtype {
        MCC_PN_CMD => {
            // PN value: [dlci][CL|frame_type][priority][ack_timer][mfs lo][mfs hi][max_retrans][credits]
            if value.len() < 8 {
                return Ok(());
            }
            let req_dlci = value[0] & 0x3F;
            let their_mfs = u16::from(value[4]) | (u16::from(value[5]) << 8);
            *peer_mtu = their_mfs.min(MAX_FRAME_SIZE).max(23);

            // CL nibble 0xF in the request asks for credit-based flow control; 0xE in the response
            // accepts it. Anything else means the peer wants the legacy non-credit mode, which we
            // do not implement — mirror their choice rather than force ours.
            let credit_flow = (value[1] & 0xF0) == 0xF0;
            // value[7] is how many frames the peer is allowing US to send. Without seeding this the
            // transmit side starts at zero credits and can never send anything.
            *peer_credits = value[7];
            let mut rsp = [0u8; 8];
            rsp[0] = req_dlci;
            rsp[1] = if credit_flow { 0xE0 } else { 0x00 };
            rsp[2] = value[2]; // priority echoed
            rsp[3] = value[3]; // ack timer echoed
            rsp[4] = (*peer_mtu & 0xFF) as u8;
            rsp[5] = (*peer_mtu >> 8) as u8;
            rsp[6] = value[6];
            rsp[7] = PN_INITIAL_CREDITS;

            let mut mcc = Vec::with_capacity(10);
            mcc.push(MCC_PN_RSP);
            mcc.push(((rsp.len() as u8) << 1) | 0x01);
            mcc.extend_from_slice(&rsp);
            eprintln!(
                "[rfcomm-u] PN dlci={req_dlci} mfs={} credit_flow={credit_flow}",
                *peer_mtu
            );
            write_all_fd(fd, &build_frame(cmd_cr(init), 0, CTRL_UIH, &mcc, None))
        }
        MCC_MSC_CMD => {
            // Modem status. Echo it back as a response, then send our own command so the peer sees
            // our signals asserted — without this some stacks never consider the DLC usable.
            let mut rsp = Vec::with_capacity(2 + value.len());
            rsp.push(MCC_MSC_RSP);
            rsp.push(((value.len() as u8) << 1) | 0x01);
            rsp.extend_from_slice(value);
            write_all_fd(fd, &build_frame(cmd_cr(init), 0, CTRL_UIH, &rsp, None))?;

            let mut cmd = Vec::with_capacity(4);
            cmd.push(MCC_MSC_CMD);
            cmd.push((2 << 1) | 0x01);
            cmd.push(value.first().copied().unwrap_or(0x03)); // addr byte of the DLC
            cmd.push(0x8D); // RTC | RTR | DV, EA set
            write_all_fd(fd, &build_frame(cmd_cr(init), 0, CTRL_UIH, &cmd, None))
        }
        MCC_PN_RSP | MCC_MSC_RSP => Ok(()),
        MCC_DISC_CMD => Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "peer closed the multiplexer",
        )),
        other => {
            // Non-supported command: the spec wants a NSC response. Log and ignore — every message
            // we care about is handled above, and a silent drop here was worth making visible.
            eprintln!("[rfcomm-u] unhandled mux control type 0x{other:02x}");
            Ok(())
        }
    }
}

/// Run the multiplexer until the data DLC for `channel` is open. Returns the DLCI.
fn open_dlc(fd: libc::c_int, channel: u8, shutdown: &AtomicBool) -> io::Result<(u8, u16, u8)> {
    const INIT: bool = false; // inbound: the phone established the multiplexer
    let mut buf = [0u8; 2048];
    let mut peer_mtu = MAX_FRAME_SIZE;
    let mut peer_credits = 0u8;
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "shutdown"));
        }
        let n = match read_packet(fd, &mut buf)? {
            Some(n) => n,
            None => continue,
        };
        let f = match parse_frame(&buf[..n]) {
            Some(f) => f,
            None => {
                eprintln!("[rfcomm-u] dropping malformed frame ({n} bytes)");
                continue;
            }
        };

        match f.ctrl {
            CTRL_SABM if f.dlci == 0 => {
                eprintln!("[rfcomm-u] mux SABM — session up");
                write_all_fd(fd, &build_frame(rsp_cr(INIT), 0, CTRL_UA, &[], None))?;
            }
            // Accept the data DLC on ANY DLCI whose server channel matches ours. The direction bit
            // depends on which side initiated the multiplexer, and mirroring the peer's DLCI avoids
            // having to infer it.
            CTRL_SABM if (f.dlci >> 1) == channel => {
                write_all_fd(fd, &build_frame(rsp_cr(INIT), f.dlci, CTRL_UA, &[], None))?;
                eprintln!("[rfcomm-u] DLC open on dlci={} (channel {channel})", f.dlci);
                return Ok((f.dlci, peer_mtu, peer_credits));
            }
            CTRL_SABM => {
                // A channel we do not serve — refuse politely instead of ignoring, or the peer
                // retries until it times out.
                eprintln!("[rfcomm-u] refusing SABM for dlci={} (not ours)", f.dlci);
                write_all_fd(fd, &build_frame(rsp_cr(INIT), f.dlci, CTRL_DM, &[], None))?;
            }
            CTRL_UIH | CTRL_UIH_CREDIT if f.dlci == 0 => {
                handle_mux_control(fd, &f.payload, &mut peer_mtu, &mut peer_credits, INIT)?;
            }
            CTRL_DISC => {
                write_all_fd(fd, &build_frame(rsp_cr(INIT), f.dlci, CTRL_UA, &[], None))?;
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "peer disconnected during DLC setup",
                ));
            }
            _ => {}
        }
    }
}

/// Pump bytes between the caller's socketpair end and RFCOMM UIH frames on the L2CAP socket.
fn pump(l2: libc::c_int, sp: libc::c_int, dlci: u8, mtu: u16, init: bool, seed_credits: u8) {
    // Credits we may spend sending, and credits we have granted the peer.
    let mut tx_credits: i32 = i32::from(seed_credits);
    let mut granted: u8 = 0;
    let mut buf = [0u8; 2048];

    // Grant an opening allowance so the phone can talk before we have anything to say.
    if write_all_fd(
        l2,
        &build_frame(cmd_cr(init), dlci, CTRL_UIH_CREDIT, &[], Some(INITIAL_CREDITS)),
    )
    .is_ok()
    {
        granted = INITIAL_CREDITS;
    }

    loop {
        let mut fds = [
            libc::pollfd {
                fd: l2,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: sp,
                // Only ask for readability when we have credit to send. POLLHUP/POLLERR are
                // delivered regardless of the mask and MUST still be handled below — otherwise a
                // caller that closes while credits are exhausted (the normal teardown state) makes
                // poll return instantly forever and this loop spins at 100% CPU.
                events: if tx_credits > 0 { libc::POLLIN } else { 0 },
                revents: 0,
            },
        ];
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, 1000) };
        if rc < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            eprintln!("[rfcomm-u] poll failed: {e}");
            return;
        }

        // Either side hanging up ends the session — checked before the POLLIN arms so a hangup is
        // never starved by a busy peer.
        if fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            eprintln!("[rfcomm-u] L2CAP hung up — closing session");
            return;
        }
        if fds[1].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            eprintln!("[rfcomm-u] caller hung up — sending DISC");
            let _ = write_all_fd(l2, &build_frame(cmd_cr(init), dlci, CTRL_DISC, &[], None));
            return;
        }

        // ---- inbound: L2CAP -> caller
        if fds[0].revents & libc::POLLIN != 0 {
            let n = match read_packet(l2, &mut buf) {
                Ok(Some(n)) => n,
                Ok(None) => 0,
                Err(e) => {
                    eprintln!("[rfcomm-u] session ended: {e}");
                    return;
                }
            };
            if n > 0 {
                if let Some(f) = parse_frame(&buf[..n]) {
                    // Credit only for OUR DLC: a DLCI-0 UIH with P/F set would otherwise have its
                    // MCC type byte consumed as a grant.
                    if let (Some(c), true) = (f.credits, f.dlci == dlci) {
                        tx_credits += i32::from(c);
                    }
                    match f.ctrl {
                        CTRL_UIH | CTRL_UIH_CREDIT if f.dlci == dlci => {
                            if !f.payload.is_empty() {
                                if write_all_fd(sp, &f.payload).is_err() {
                                    eprintln!("[rfcomm-u] caller end closed");
                                    return;
                                }
                                granted = granted.saturating_sub(1);
                                if granted <= CREDIT_REFILL_THRESHOLD {
                                    let top_up = INITIAL_CREDITS - granted;
                                    if write_all_fd(
                                        l2,
                                        &build_frame(
                                            cmd_cr(init),
                                            dlci,
                                            CTRL_UIH_CREDIT,
                                            &[],
                                            Some(top_up),
                                        ),
                                    )
                                    .is_ok()
                                    {
                                        granted += top_up;
                                    }
                                }
                            }
                        }
                        CTRL_UIH | CTRL_UIH_CREDIT if f.dlci == 0 => {
                            let (mut ignored, mut extra) = (mtu, 0u8);
                            if let Err(e) =
                                handle_mux_control(l2, &f.payload, &mut ignored, &mut extra, init)
                            {
                                eprintln!("[rfcomm-u] mux control: {e}");
                                return;
                            }
                        }
                        CTRL_DISC => {
                            let _ = write_all_fd(l2, &build_frame(rsp_cr(init), f.dlci, CTRL_UA, &[], None));
                            eprintln!("[rfcomm-u] peer sent DISC — closing");
                            return;
                        }
                        _ => {}
                    }
                }
            }
        }

        // ---- outbound: caller -> L2CAP
        if fds[1].revents & libc::POLLIN != 0 {
            let cap = usize::from(mtu).min(buf.len());
            let n = unsafe { libc::read(sp, buf.as_mut_ptr() as *mut libc::c_void, cap) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                eprintln!("[rfcomm-u] caller read failed: {e}");
                return;
            }
            if n == 0 {
                eprintln!("[rfcomm-u] caller closed the stream — sending DISC");
                let _ = write_all_fd(l2, &build_frame(cmd_cr(init), dlci, CTRL_DISC, &[], None));
                return;
            }
            if write_all_fd(
                l2,
                &build_frame(cmd_cr(init), dlci, CTRL_UIH, &buf[..n as usize], None),
            )
            .is_err()
            {
                eprintln!("[rfcomm-u] L2CAP write failed — closing");
                return;
            }
            tx_credits -= 1;
        }
    }
}

/// Build a PN command for the DLC we want to open, requesting credit-based flow control.
fn pn_command(dlci: u8) -> Vec<u8> {
    let mfs = MAX_FRAME_SIZE;
    let value = [
        dlci,
        0xF0, // CL = request credit flow control, frame_type = 0 (UIH)
        0x00, // priority
        0x00, // ack timer (default)
        (mfs & 0xFF) as u8,
        (mfs >> 8) as u8,
        0x00, // max retransmissions
        PN_INITIAL_CREDITS,
    ];
    let mut mcc = Vec::with_capacity(10);
    mcc.push(MCC_PN_CMD);
    mcc.push(((value.len() as u8) << 1) | 0x01);
    mcc.extend_from_slice(&value);
    mcc
}

/// Read frames until `want` matches one, or the deadline passes.
fn await_frame<F: Fn(&Frame) -> bool>(
    fd: libc::c_int,
    want: F,
    deadline: std::time::Instant,
) -> io::Result<Frame> {
    let mut buf = [0u8; 2048];
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for RFCOMM response",
            ));
        }
        if let Some(n) = read_packet(fd, &mut buf)? {
            if let Some(f) = parse_frame(&buf[..n]) {
                if want(&f) {
                    return Ok(f);
                }
                if f.ctrl == CTRL_DM {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        format!("peer refused DLCI {} (DM)", f.dlci),
                    ));
                }
            }
        }
    }
}

/// Connect OUT to a peer's RFCOMM `channel` — the accessory-initiated reconnect path for a phone we
/// are already bonded with.
///
/// Mirrors `rfcomm::connect_to`. Here we are the multiplexer INITIATOR, which flips both the C/R bit
/// and the DLCI direction bit relative to [`accept_one`] — see [`cmd_cr`] and [`data_dlci`].
/// Pairing-aware connect wait (device-proven 2026-09-03). The L2CAP connect is where SSP pairing
/// happens when the phone enforces authentication: the kernel holds the connect until the phone's
/// user has answered the numeric-comparison prompt. A flat `connect_timeout_secs` tore that prompt
/// down before anyone could compare and tap, and every retry produced a NEW code (the owner saw the
/// app cycling codes). So while `/tmp/pairing_code` is fresh for THIS attempt (written by
/// `ssp_agent` on USER_CONFIRM_REQUEST, mtime >= our start) the deadline stretches to
/// [`PAIRING_HOLD_SECS`]; and once `ssp_agent` reports that the phone rejected the re-pair
/// (`/tmp/pair_rejected`, mtime >= our start) the wait ends at once with `PermissionDenied`, so the
/// caller backs off instead of the kernel re-requesting authentication every ~0.5 s.
pub const PAIRING_HOLD_SECS: u64 = 60;
/// Written by `ssp_agent` with the 6-digit code while a numeric-comparison confirm is outstanding.
pub const PAIRING_CODE_FLAG: &str = "/tmp/pairing_code";
/// Written by `ssp_agent` when the phone has rejected the re-pair repeatedly on one connection.
pub const PAIR_REJECTED_FLAG: &str = "/tmp/pair_rejected";

#[derive(Debug, PartialEq, Eq)]
enum WaitDecision {
    Wait,
    TimedOut,
    Rejected,
}

/// Pure decision for one poll tick of the connect wait. `base` is the caller's plain timeout.
fn connect_wait_decision(
    elapsed: std::time::Duration,
    base: std::time::Duration,
    code_fresh: bool,
    rejected_fresh: bool,
) -> WaitDecision {
    if rejected_fresh {
        return WaitDecision::Rejected;
    }
    let limit = if code_fresh {
        base.max(std::time::Duration::from_secs(PAIRING_HOLD_SECS))
    } else {
        base
    };
    if elapsed >= limit {
        WaitDecision::TimedOut
    } else {
        WaitDecision::Wait
    }
}

/// True when `path` exists and was modified at or after `since` (a flag left by an EARLIER attempt
/// must not steer this one).
fn flag_fresh(path: &str, since: std::time::SystemTime) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| t >= since)
        .unwrap_or(false)
}

/// `connect()` with a pairing-aware wait, for BOTH RFCOMM backends. Connects non-blocking, then polls
/// with [`connect_wait_decision`] each 250 ms: a plain `connect_timeout_secs` deadline, stretched to
/// [`PAIRING_HOLD_SECS`] while a numeric-comparison prompt is outstanding for this attempt, and cut
/// short with `PermissionDenied` once `ssp_agent` reports the phone rejected the re-pair. The socket
/// is returned to blocking mode on success. The kernel RFCOMM backend is what the CCPA supervisor
/// runs (`CARPLAY_RFCOMM_BACKEND` unset), so this MUST be called from `rfcomm::connect_to` too —
/// the first cut of the hold lived only in the userspace path and never ran on the box (2026-09-03).
pub(crate) fn pairing_aware_connect(
    fd: libc::c_int,
    addr: *const libc::sockaddr,
    addr_len: libc::socklen_t,
    connect_timeout_secs: i64,
) -> io::Result<()> {
    let fl = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    unsafe { libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK) };
    let rc = unsafe { libc::connect(fd, addr, addr_len) };
    if rc < 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(e);
        }
        let start = std::time::Instant::now();
        // 1 s of slack so a flag written in the same tick as our start still counts as ours.
        let since = std::time::SystemTime::now() - std::time::Duration::from_secs(1);
        let base = std::time::Duration::from_secs(connect_timeout_secs.max(1) as u64);
        let mut held_logged = false;
        // Latched once a prompt has been seen: the bond forming clears the code flag a moment BEFORE
        // the connect completes (device-proven 2026-09-03: NEW_LINK_KEY at +12 s, connect reported
        // "timed out" in the same tick because the hold fell back to the 8 s base), so the extended
        // limit must survive the flag going away for the rest of this attempt.
        let mut held = false;
        loop {
            let mut pfd = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
            let n = unsafe { libc::poll(&mut pfd, 1, 250) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if n > 0 {
                let mut err: libc::c_int = 0;
                let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                unsafe {
                    libc::getsockopt(
                        fd,
                        libc::SOL_SOCKET,
                        libc::SO_ERROR,
                        &mut err as *mut libc::c_int as *mut libc::c_void,
                        &mut len,
                    )
                };
                if err != 0 {
                    return Err(io::Error::from_raw_os_error(err));
                }
                break;
            }
            let code_fresh = flag_fresh(PAIRING_CODE_FLAG, since);
            held |= code_fresh;
            let rejected = flag_fresh(PAIR_REJECTED_FLAG, since);
            match connect_wait_decision(start.elapsed(), base, held, rejected) {
                WaitDecision::Wait => {
                    if code_fresh && !held_logged {
                        eprintln!(
                            "[rfcomm] pairing prompt outstanding on the phone — holding the connect up to \
                             {PAIRING_HOLD_SECS}s for the user to confirm"
                        );
                        held_logged = true;
                    }
                }
                WaitDecision::TimedOut => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "connect timed out"));
                }
                WaitDecision::Rejected => {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "pairing rejected by the phone (re-pair on the iPhone)",
                    ));
                }
            }
        }
    }
    unsafe { libc::fcntl(fd, libc::F_SETFL, fl) };
    Ok(())
}

/// Ask the kernel for GENERAL BONDING on an outgoing BR/EDR socket (`BT_SECURITY_MEDIUM`).
/// Device-proven 2026-09-03: with no level set the box's own side of a box-initiated connect is
/// "no bonding", so a bond formed on it arrives as `NEW_LINK_KEY store_hint=0` (the kernel's
/// `hci_persistent_key` says not persistent) even though the phone did bond — the fresh
/// authenticated key was never written to disk and the next boot prompted again. MEDIUM maps to
/// `HCI_AT_GENERAL_BONDING`, which is what an accessory that expects to reconnect silently wants.
/// Best-effort: an old kernel refusing the option just keeps the previous behaviour.
pub(crate) fn request_bonding(fd: libc::c_int) {
    const SOL_BLUETOOTH: libc::c_int = 274;
    const BT_SECURITY: libc::c_int = 4;
    const BT_SECURITY_MEDIUM: u8 = 2;
    #[repr(C)]
    struct BtSecurity {
        level: u8,
        key_size: u8,
    }
    let sec = BtSecurity { level: BT_SECURITY_MEDIUM, key_size: 0 };
    let rc = unsafe {
        libc::setsockopt(
            fd,
            SOL_BLUETOOTH,
            BT_SECURITY,
            &sec as *const BtSecurity as *const libc::c_void,
            std::mem::size_of::<BtSecurity>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        eprintln!(
            "[rfcomm] BT_SECURITY=MEDIUM refused ({}) — bonds formed on this connect may not persist",
            io::Error::last_os_error()
        );
    }
}

pub fn connect_to(
    peer: [u8; 6],
    channel: u8,
    connect_timeout_secs: i64,
) -> io::Result<std::fs::File> {
    const INIT: bool = true;
    let fd = unsafe {
        libc::socket(
            AF_BLUETOOTH as libc::c_int,
            libc::SOCK_SEQPACKET | crate::cloexec::SOCK_CLOEXEC,
            BTPROTO_L2CAP,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // Guard so every early return closes the socket exactly once.
    struct Guard(libc::c_int);
    impl Drop for Guard {
        fn drop(&mut self) {
            if self.0 >= 0 {
                unsafe { libc::close(self.0) };
            }
        }
    }
    let mut guard = Guard(fd);

    // `as _`, not a concrete type: time_t is i32 on armv7-musl (the CCPA) and i64 on the host and
    // aarch64 (the Pi), so a fixed type fails to compile on one of them. Same pattern as
    // rfcomm.rs:115.
    // zeroed()+assign, not a struct literal: under `musl32_time64` (riscv32) these
    // types carry private padding and a literal does not compile.
    let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
    tv.tv_sec = connect_timeout_secs.max(1) as _;
    // BOTH timeouts. SO_SNDTIMEO is what actually bounds the blocking connect() below — with only
    // SO_RCVTIMEO an absent bonded phone parks the reconnect thread for the kernel's full page/L2CAP
    // timeout instead of connect_timeout_secs. The kernel implementation sets it for this reason.
    for opt in [libc::SO_RCVTIMEO, libc::SO_SNDTIMEO] {
        unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                opt,
                &tv as *const libc::timeval as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            );
        }
    }

    // Before connect(): the Configure Request goes out as part of the connect() handshake, so imtu
    // must already be raised by the time it's called.
    set_l2cap_imtu(fd)?;
    request_bonding(fd);

    let addr = SockaddrL2 {
        l2_family: AF_BLUETOOTH,
        l2_psm: RFCOMM_PSM,
        l2_bdaddr: peer,
        l2_cid: 0,
        l2_bdaddr_type: 0,
    };
    // Non-blocking connect so the wait can be pairing-aware — shared with the kernel backend
    // (`rfcomm::connect_to`), which is what the CCPA supervisor actually runs.
    pairing_aware_connect(
        fd,
        &addr as *const SockaddrL2 as *const libc::sockaddr,
        std::mem::size_of::<SockaddrL2>() as libc::socklen_t,
        connect_timeout_secs,
    )?;
    eprintln!("[rfcomm-u] L2CAP PSM 3 up to peer — opening multiplexer as initiator");
    // L2CAP config completes synchronously inside connect(); the peer's real omtu is known now, even
    // though the RFCOMM MFS it bounds isn't negotiated until the PN exchange below.
    let omtu = get_l2cap_omtu(fd).ok();

    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(connect_timeout_secs.max(1) as u64);

    // 1. Multiplexer session: SABM on DLCI 0 -> UA.
    write_all_fd(fd, &build_frame(cmd_cr(INIT), 0, CTRL_SABM, &[], None))?;
    await_frame(fd, |f| f.ctrl == CTRL_UA && f.dlci == 0, deadline)?;

    // 2. Negotiate the DLC parameters (and credit-based flow control).
    let dlci = data_dlci(INIT, channel);
    write_all_fd(
        fd,
        &build_frame(cmd_cr(INIT), 0, CTRL_UIH, &pn_command(dlci), None),
    )?;
    let pn_rsp = await_frame(
        fd,
        |f| {
            f.dlci == 0
                && matches!(f.ctrl, CTRL_UIH | CTRL_UIH_CREDIT)
                && f.payload.first() == Some(&MCC_PN_RSP)
        },
        deadline,
    )?;
    // MCC layout is [type][len][value..], so the PN value starts at payload[2]: mfs is value[4..6]
    // and the credit grant is value[7].
    let (mtu, seed) = if pn_rsp.payload.len() >= 10 {
        (
            (u16::from(pn_rsp.payload[6]) | (u16::from(pn_rsp.payload[7]) << 8))
                .clamp(23, MAX_FRAME_SIZE),
            pn_rsp.payload[9],
        )
    } else {
        (MAX_FRAME_SIZE, 0)
    };
    let mtu = clamp_to_omtu(mtu, omtu);
    if omtu.is_some_and(|o| o < L2CAP_IMTU) {
        eprintln!(
            "[rfcomm-u] peer L2CAP omtu={} below requested {L2CAP_IMTU} — clamping outgoing frames to {mtu}",
            omtu.unwrap()
        );
    }

    // 3. Open the data channel.
    write_all_fd(fd, &build_frame(cmd_cr(INIT), dlci, CTRL_SABM, &[], None))?;
    await_frame(fd, |f| f.ctrl == CTRL_UA && f.dlci == dlci, deadline)?;
    eprintln!("[rfcomm-u] outbound DLC open on dlci={dlci} (channel {channel}, mfs={mtu})");

    // 4. Assert our modem signals; some peers will not treat the DLC as usable until they see MSC.
    let mut msc = Vec::with_capacity(4);
    msc.push(MCC_MSC_CMD);
    msc.push((2 << 1) | 0x01);
    msc.push(addr_byte(cmd_cr(INIT), dlci));
    msc.push(0x8D);
    write_all_fd(fd, &build_frame(cmd_cr(INIT), 0, CTRL_UIH, &msc, None))?;

    let mut sv = [0 as libc::c_int; 2];
    if unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | crate::cloexec::SOCK_CLOEXEC,
            0,
            sv.as_mut_ptr(),
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let (caller_end, pump_end) = (sv[0], sv[1]);
    guard.0 = -1; // ownership passes to the pump thread

    std::thread::spawn(move || {
        pump(fd, pump_end, dlci, mtu, INIT, seed);
        unsafe {
            libc::close(pump_end);
            libc::close(fd);
        }
        eprintln!("[rfcomm-u] outbound pump thread exited");
    });

    Ok(unsafe { std::fs::File::from_raw_fd(caller_end) })
}

/// Accept one inbound RFCOMM connection on `channel`.
///
/// Mirrors `rfcomm::accept_one` exactly — including returning `Ok(None)` on a timeout so the caller
/// can re-check its shutdown flag — so `main.rs` can dispatch between the two without caring which
/// implementation is live.
pub fn accept_one(channel: u8, shutdown: &AtomicBool) -> io::Result<Option<std::fs::File>> {
    let lfd = listener()?;
    // CONTRACT: `Ok(None)` means SHUTDOWN, never "timed out" — `main.rs` breaks its accept loop on
    // it. The 1 s socket timeout exists only so the shutdown flag is polled promptly, so a timeout
    // must loop here rather than return. Returning `Ok(None)` on timeout killed the accept thread
    // one second after every bring-up.
    let cfd = loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(None);
        }
        // accept4(SOCK_CLOEXEC), not accept(): the accepted connection is live across
        // av::ensure_av_layer()'s fork+exec of the detached daemons, and a leaked L2CAP connection
        // cannot be torn down without a reboot.
        let fd = unsafe {
            crate::cloexec::accept_cloexec(lfd, std::ptr::null_mut(), std::ptr::null_mut())
        };
        if fd >= 0 {
            break fd;
        }
        let e = io::Error::last_os_error();
        match e.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted => {
                continue
            }
            _ => return Err(e),
        }
    };
    eprintln!("[rfcomm-u] L2CAP connection accepted — negotiating multiplexer");

    // Same 1 s receive timeout on the accepted socket, so open_dlc can honour `shutdown`.
    // zeroed()+assign, not a struct literal: under `musl32_time64` (riscv32) these
    // types carry private padding and a literal does not compile.
    let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
    tv.tv_sec = 1;
    // Checked, matching rfcomm.rs/sdp_server.rs's discipline: an unnoticed failure here would leave
    // open_dlc's read() blocking forever, so the shutdown flag is never polled.
    let rc = unsafe {
        libc::setsockopt(
            cfd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &tv as *const libc::timeval as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        let e = io::Error::last_os_error();
        unsafe { libc::close(cfd) };
        return Err(e);
    }

    let (dlci, mtu, seed) = match open_dlc(cfd, channel, shutdown) {
        Ok(v) => v,
        Err(e) => {
            unsafe { libc::close(cfd) };
            return if e.kind() == io::ErrorKind::Interrupted {
                Ok(None)
            } else {
                Err(e)
            };
        }
    };
    // cfd inherited its imtu from the listener; the peer's real omtu is only known post-accept.
    let omtu = get_l2cap_omtu(cfd).ok();
    let mtu = clamp_to_omtu(mtu, omtu);
    if omtu.is_some_and(|o| o < L2CAP_IMTU) {
        eprintln!(
            "[rfcomm-u] peer L2CAP omtu={} below requested {L2CAP_IMTU} — clamping outgoing frames to {mtu}",
            omtu.unwrap()
        );
    }

    // socketpair: one end to the caller as a plain stream, the other to the pump thread.
    // SOCK_CLOEXEC on BOTH ends. Without it av.rs's fork+exec leaks duplicates into the detached
    // daemons, the pump's read() never returns 0, and its only teardown signal never fires — leaking
    // a thread, two fds and a live RFCOMM session to the phone on every connect/disconnect cycle.
    let mut sv = [0 as libc::c_int; 2];
    if unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | crate::cloexec::SOCK_CLOEXEC,
            0,
            sv.as_mut_ptr(),
        )
    } < 0
    {
        let e = io::Error::last_os_error();
        unsafe { libc::close(cfd) };
        return Err(e);
    }
    let (caller_end, pump_end) = (sv[0], sv[1]);

    std::thread::spawn(move || {
        pump(cfd, pump_end, dlci, mtu, false, seed);
        unsafe {
            libc::close(pump_end);
            libc::close(cfd);
        }
        eprintln!("[rfcomm-u] pump thread exited");
    });

    Ok(Some(unsafe { std::fs::File::from_raw_fd(caller_end) }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn connect_wait_holds_for_the_pairing_prompt_and_aborts_on_rejection() {
        use std::time::Duration as D;
        let base = D::from_secs(8);
        assert_eq!(connect_wait_decision(D::from_secs(3), base, false, false), WaitDecision::Wait);
        assert_eq!(connect_wait_decision(D::from_secs(8), base, false, false), WaitDecision::TimedOut);
        // A fresh prompt stretches the wait to PAIRING_HOLD_SECS...
        assert_eq!(connect_wait_decision(D::from_secs(30), base, true, false), WaitDecision::Wait);
        assert_eq!(connect_wait_decision(D::from_secs(60), base, true, false), WaitDecision::TimedOut);
        // ...and a rejection ends it regardless of elapsed time or an outstanding prompt.
        assert_eq!(connect_wait_decision(D::from_secs(1), base, true, true), WaitDecision::Rejected);
        // A caller timeout longer than the hold is respected as-is.
        assert_eq!(connect_wait_decision(D::from_secs(70), D::from_secs(90), true, false), WaitDecision::Wait);
    }

    #[test]
    fn flag_fresh_ignores_files_older_than_the_attempt() {
        let dir = std::env::temp_dir().join(format!("rfcomm_flag_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("flag");
        let fs = f.to_str().unwrap();
        assert!(!flag_fresh(fs, std::time::SystemTime::now()));
        std::fs::write(&f, b"1").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        assert!(flag_fresh(fs, std::time::SystemTime::now() - std::time::Duration::from_secs(5)));
        assert!(!flag_fresh(fs, later));
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    /// The FCS table and algorithm are the single easiest thing to get subtly wrong; a bad FCS looks
    /// like the peer ignoring us. This is the worked example from the TS 07.10 spec: the FCS over a
    /// SABM on DLCI 0 (addr 0x03, ctrl 0x3F, len 0x01) is 0x1C.
    #[test]
    fn fcs_matches_spec_example() {
        assert_eq!(fcs(&[0x03, 0x3F, 0x01]), 0x1C);
    }

    #[test]
    fn l2cap_options_is_kernel_abi_size() {
        // include/net/bluetooth/l2cap.h struct l2cap_options is 12 bytes; a mismatch here means our
        // repr(C) has drifted from the kernel's layout and every setsockopt/getsockopt call below is
        // silently reading/writing garbage.
        assert_eq!(std::mem::size_of::<L2capOptions>(), 12);
    }

    #[test]
    fn clamp_to_omtu_only_shrinks_when_peer_undercuts() {
        // No omtu read back (getsockopt failed) — leave the RFCOMM-negotiated MFS alone.
        assert_eq!(clamp_to_omtu(1019, None), 1019);
        // Peer granted at least what we asked for — no clamp.
        assert_eq!(clamp_to_omtu(1019, Some(L2CAP_IMTU)), 1019);
        assert_eq!(clamp_to_omtu(1019, Some(2000)), 1019);
        // Peer undercut us (e.g. stuck at L2CAP_DEFAULT_MTU 672) — clamp to omtu - 5, floored at 23
        // (the RFCOMM 1.2 minimum MFS).
        assert_eq!(clamp_to_omtu(1019, Some(672)), 667);
        assert_eq!(clamp_to_omtu(1019, Some(20)), 23);
        // A bogus 0 omtu must not be trusted as "peer accepts nothing" or "unbounded" — treat like
        // "didn't read back".
        assert_eq!(clamp_to_omtu(1019, Some(0)), 1019);
    }

    #[test]
    fn addr_and_dlci_round_trip() {
        for dlci in [0u8, 1, 2, 3, 12, 63] {
            assert_eq!(dlci_of(addr_byte(true, dlci)), dlci);
            assert_eq!(dlci_of(addr_byte(false, dlci)), dlci);
        }
        // EA bit always set, C/R reflected.
        assert_eq!(addr_byte(true, 0) & 0x03, 0x03);
        assert_eq!(addr_byte(false, 0) & 0x03, 0x01);
    }

    #[test]
    fn short_frame_uses_single_length_byte() {
        let f = build_frame(false, 2, CTRL_UIH, b"hello", None);
        // addr, ctrl, len, payload(5), fcs
        assert_eq!(f.len(), 3 + 5 + 1);
        assert_eq!(f[2], (5 << 1) | 0x01);
    }

    #[test]
    fn long_frame_uses_two_length_bytes() {
        let payload = vec![0xAAu8; 200];
        let f = build_frame(false, 2, CTRL_UIH, &payload, None);
        assert_eq!(f.len(), 4 + 200 + 1);
        assert_eq!(f[2] & 0x01, 0, "EA must be clear for a two-byte length");
        let len = ((f[2] >> 1) as usize) | ((f[3] as usize) << 7);
        assert_eq!(len, 200);
    }

    /// RFCOMM 1.2 6.5.2: the credit octet sits BETWEEN the length indicator and the information
    /// field and is excluded from the length. Counting it corrupts every credit-bearing frame.
    #[test]
    fn credit_octet_is_excluded_from_the_length() {
        let f = build_frame(false, 2, CTRL_UIH_CREDIT, b"xy", Some(7));
        assert_eq!(f[1], CTRL_UIH_CREDIT);
        assert_eq!(f[2], (2 << 1) | 0x01, "length covers the payload only");
        assert_eq!(f[3], 7, "credit octet follows the length");
    }

    /// A standalone grant is len=0 plus one credit octet. This is what the phone sends to top us
    /// up, and mis-parsing it starves the transmit side permanently.
    #[test]
    fn standalone_credit_grant_round_trips() {
        let f = build_frame(false, 2, CTRL_UIH_CREDIT, &[], Some(15));
        assert_eq!(f[2], 0x01, "length must be 0 with EA set");
        let p = parse_frame(&f).expect("standalone grant must parse");
        assert_eq!(p.credits, Some(15));
        assert!(p.payload.is_empty());
    }

    /// PN's `k` is a 3-bit field; anything above 7 masks to garbage on a conformant peer.
    #[test]
    fn pn_initial_credits_fit_three_bits() {
        assert!(PN_INITIAL_CREDITS <= 7);
        let pn = pn_command(2);
        assert!(pn[9] <= 7, "PN credit field must be 0..7");
    }

    #[test]
    fn parse_rejects_bad_fcs() {
        let mut f = build_frame(false, 2, CTRL_UIH, b"abc", None);
        let last = f.len() - 1;
        f[last] ^= 0xFF;
        assert!(parse_frame(&f).is_none(), "a bad FCS must be rejected");
    }

    #[test]
    fn parse_round_trips_build() {
        let f = build_frame(false, 4, CTRL_UIH, b"payload", None);
        let p = parse_frame(&f).expect("parses");
        assert_eq!(p.dlci, 4);
        assert_eq!(p.ctrl, CTRL_UIH);
        assert_eq!(p.payload, b"payload");
        assert!(p.credits.is_none());
    }

    #[test]
    fn parse_strips_credits_from_payload() {
        let f = build_frame(false, 4, CTRL_UIH_CREDIT, b"data", Some(9));
        let p = parse_frame(&f).expect("parses");
        assert_eq!(p.credits, Some(9));
        assert_eq!(p.payload, b"data", "credit byte must not leak into the payload");
    }

    #[test]
    fn parse_round_trips_long_frame() {
        let payload = vec![0x5Au8; 300];
        let f = build_frame(false, 6, CTRL_UIH, &payload, None);
        let p = parse_frame(&f).expect("parses");
        assert_eq!(p.payload.len(), 300);
        assert_eq!(p.payload, payload);
    }

    #[test]
    fn parse_rejects_truncated() {
        assert!(parse_frame(&[]).is_none());
        assert!(parse_frame(&[0x03, 0x3F]).is_none());
        // Declares 50 bytes but carries none.
        assert!(parse_frame(&[0x0B, CTRL_UIH, (50 << 1) | 1, 0x00]).is_none());
    }

    /// UIH's FCS covers only addr+ctrl, everything else covers addr+ctrl+len. Getting this backwards
    /// is silently fatal, so assert both shapes explicitly.
    #[test]
    fn fcs_span_differs_for_uih() {
        let uih = build_frame(false, 2, CTRL_UIH, b"z", None);
        assert_eq!(*uih.last().unwrap(), fcs(&uih[..2]));
        let sabm = build_frame(true, 0, CTRL_UA, &[], None);
        assert_eq!(*sabm.last().unwrap(), fcs(&sabm[..3]));
    }
}
