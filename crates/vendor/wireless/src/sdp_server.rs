//! Minimal, `bluetoothd`-less SDP server for the iAP2 "Wireless iAPv2" service -- a faithful port
//! of `carlink_linux`'s `sdp_server.c` (proven live: "without this the iPhone browses SDP, finds no
//! iAP2 service, and disconnects" -- exactly the failure this project's own first live pairing test
//! reproduced, confirming this module is in fact required, not optional, for a connection to
//! survive past pairing).
//!
//! Binds an L2CAP `SOCK_SEQPACKET` socket to PSM 0x0001 (the well-known SDP PSM) and serves a single
//! pre-built service record (byte-exact from the reference's own CCPA capture) advertising the iAP2
//! UUID `00000000-deca-fade-deca-deafdecacaff` on the given RFCOMM channel. No `libbluetooth`
//! dependency -- the L2CAP sockaddr struct is hand-defined, matching the reference's own approach.

use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::sync::atomic::{AtomicBool, Ordering};

const AF_BLUETOOTH: libc::sa_family_t = 31;
const BTPROTO_L2CAP: libc::c_int = 0;
const SDP_PSM: u16 = 0x0001;

#[repr(C)]
#[derive(Clone, Copy)]
struct SockaddrL2 {
    l2_family: libc::sa_family_t,
    l2_psm: u16, // little-endian on the wire; this Pi is little-endian, so native == LE
    l2_bdaddr: [u8; 6],
    l2_cid: u16,
    l2_bdaddr_type: u8,
}

// SDP PDU identifiers.
const SDP_ERROR_RSP: u8 = 0x01;
const SDP_SVC_SEARCH_REQ: u8 = 0x02;
const SDP_SVC_SEARCH_RSP: u8 = 0x03;
const SDP_SVC_ATTR_REQ: u8 = 0x04;
const SDP_SVC_ATTR_RSP: u8 = 0x05;
const SDP_SVC_SEARCH_ATTR_REQ: u8 = 0x06;
const SDP_SVC_SEARCH_ATTR_RSP: u8 = 0x07;

// SDP error codes.
const SDP_E_INVALID_HANDLE: u16 = 0x0002;
const SDP_E_INVALID_SYNTAX: u16 = 0x0003;
const SDP_E_INVALID_PDU_SIZE: u16 = 0x0004;
const SDP_E_INVALID_CONTINUE: u16 = 0x0005;

const SVC_RECORD_HANDLE: u32 = 0x0001_0000;

/// The iAP2 "Wireless iAPv2" SDP service record, byte-exact from the reference project's own
/// CCPA capture (`bt.pcap`, handle 0x00010000). Byte 48 (the RFCOMM channel value) is patched at
/// init time -- see `RFCOMM_CHAN_IDX`.
#[rustfmt::skip]
const RECORD_TEMPLATE: [u8; 89] = [
    0x35, 0x57,
    // ServiceRecordHandle = 0x00010000
    0x09, 0x00, 0x00,  0x0a, 0x00, 0x01, 0x00, 0x00,
    // ServiceClassIDList: UUID128 00000000-deca-fade-deca-deafdecacaff
    0x09, 0x00, 0x01,  0x35, 0x11, 0x1c,
        0x00, 0x00, 0x00, 0x00, 0xde, 0xca, 0xfa, 0xde,
        0xde, 0xca, 0xde, 0xaf, 0xde, 0xca, 0xca, 0xff,
    // ProtocolDescriptorList: L2CAP -> RFCOMM channel 0x01 (channel patched at init)
    0x09, 0x00, 0x04,  0x35, 0x0c,
        0x35, 0x03, 0x19, 0x01, 0x00,
        0x35, 0x05, 0x19, 0x00, 0x03, 0x08, 0x01,
    // BrowseGroupList = PublicBrowseGroup (0x1002)
    0x09, 0x00, 0x05,  0x35, 0x03, 0x19, 0x10, 0x02,
    // BluetoothProfileDescriptorList = SerialPort (0x1101) v1.0
    0x09, 0x00, 0x09,  0x35, 0x08, 0x35, 0x06, 0x19, 0x11, 0x01, 0x09, 0x01, 0x00,
    // ServiceName (0x0100) = "Wireless iAPv2"
    0x09, 0x01, 0x00,  0x25, 0x0e,
        0x57, 0x69, 0x72, 0x65, 0x6c, 0x65, 0x73, 0x73, 0x20,
        0x69, 0x41, 0x50, 0x76, 0x32,
];
const RFCOMM_CHAN_IDX: usize = 48;

fn log(m: &str) {
    println!("[sdp] {m}");
}

/// Build `(record, search_attr_blob)` for the given RFCOMM channel -- `record` is the 89-byte
/// service record with its channel byte patched; `search_attr_blob` is the 91-byte outer sequence
/// (`35 59` + `record`) `ServiceSearchAttributeResponse` serves per the SDP spec's "list of
/// attribute lists" requirement.
fn build_records(rfcomm_chan: u8) -> (Vec<u8>, Vec<u8>) {
    let mut record = RECORD_TEMPLATE.to_vec();
    record[RFCOMM_CHAN_IDX] = rfcomm_chan;
    let mut ssa = Vec::with_capacity(2 + record.len());
    ssa.push(0x35);
    ssa.push(record.len() as u8);
    ssa.extend_from_slice(&record);
    (record, ssa)
}

/// Parse one SDP data-element header: returns `(header_len, data_len)`. Just enough to skip
/// search patterns / attribute-ID lists without inspecting their content, matching the reference.
fn de_parse(p: &[u8]) -> Option<(usize, usize)> {
    let desc = *p.first()?;
    let typ = desc >> 3;
    let siz = desc & 0x07;
    let (hl, dl): (usize, usize) = match siz {
        0 => (1, if typ == 0 { 0 } else { 1 }),
        1 => (1, 2),
        2 => (1, 4),
        3 => (1, 8),
        4 => (1, 16),
        5 => (2, *p.get(1)? as usize),
        6 => (3, ((*p.get(1)? as usize) << 8) | *p.get(2)? as usize),
        7 => (
            5,
            ((*p.get(1)? as usize) << 24)
                | ((*p.get(2)? as usize) << 16)
                | ((*p.get(3)? as usize) << 8)
                | *p.get(4)? as usize,
        ),
        _ => return None,
    };
    // audit Fix #10: checked_add — for size-code 7 `dl` is a radio-controlled 32-bit length up to
    // 0xFFFF_FFFF; on armv7 (32-bit usize, release overflow-checks off) `hl + dl` would WRAP past this
    // bounds check and hand back a bogus length. Reject overflow (and out-of-bounds) here.
    let total = hl.checked_add(dl)?;
    if total > p.len() {
        return None;
    }
    Some((hl, dl))
}

/// A small bounds-checked read cursor over a request's parameter block.
struct Cur<'a> {
    p: &'a [u8],
    off: usize,
}

impl<'a> Cur<'a> {
    fn u16(&mut self) -> Option<u16> {
        let v = u16::from_be_bytes(self.p.get(self.off..self.off + 2)?.try_into().ok()?);
        self.off += 2;
        Some(v)
    }
    fn u32(&mut self) -> Option<u32> {
        let v = u32::from_be_bytes(self.p.get(self.off..self.off + 4)?.try_into().ok()?);
        self.off += 4;
        Some(v)
    }
    /// Skip one complete data element (e.g. a service-search pattern or attribute-ID list).
    fn skip_de(&mut self) -> Option<()> {
        let (hl, dl) = de_parse(self.p.get(self.off..)?)?;
        // de_parse already bounds `hl + dl` to the remaining slice (so no wrap here once it succeeds),
        // but guard the cursor advance too (audit Fix #10, defense-in-depth on armv7).
        self.off = self.off.checked_add(hl)?.checked_add(dl)?;
        Some(())
    }
    /// Read the trailing continuation state -> resume offset. `0x00` => 0, `0x02 NN NN` => offset.
    /// The server only ever emits 0- or 2-byte cookies (see `serve_attr_blob`), so that's all this
    /// accepts -- anything else is rejected, matching the reference exactly.
    fn continuation(&mut self) -> Option<usize> {
        let info = *self.p.get(self.off)?;
        self.off += 1;
        if self.off + info as usize > self.p.len() {
            return None;
        }
        match info {
            0 => Some(0),
            2 => {
                let v = u16::from_be_bytes(self.p.get(self.off..self.off + 2)?.try_into().ok()?);
                self.off += 2;
                Some(v as usize)
            }
            _ => None,
        }
    }
}

fn sdp_send(stream: &mut impl Write, pdu_id: u8, tid: u16, params: &[u8]) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(5 + params.len());
    buf.push(pdu_id);
    buf.extend_from_slice(&tid.to_be_bytes());
    buf.extend_from_slice(&(params.len() as u16).to_be_bytes());
    buf.extend_from_slice(params);
    stream.write_all(&buf)
}

fn sdp_error(stream: &mut impl Write, tid: u16, code: u16) -> std::io::Result<()> {
    log(&format!("error response code=0x{code:04x}"));
    sdp_send(stream, SDP_ERROR_RSP, tid, &code.to_be_bytes())
}

/// Serve an attribute-list-style response (`ServiceAttributeResponse`/`ServiceSearchAttributeResponse`
/// share the exact wire shape): `ByteCount(2) | <chunk> | ContinuationState`. Chunks `blob` honoring
/// `maxbytes`, emitting a 2-byte BE offset cookie when more data remains or a bare `0x00` when done.
fn serve_attr_blob(
    stream: &mut impl Write,
    resp_pdu: u8,
    tid: u16,
    blob: &[u8],
    offset: usize,
    maxbytes: u16,
) -> std::io::Result<()> {
    if offset > blob.len() {
        return sdp_error(stream, tid, SDP_E_INVALID_CONTINUE);
    }
    let remaining = blob.len() - offset;
    let mut chunk = remaining;
    if maxbytes != 0 && chunk > maxbytes as usize {
        chunk = maxbytes as usize;
    }
    if chunk == 0 && remaining > 0 {
        chunk = 1; // always make progress
    }
    let next = offset + chunk;
    let more = next < blob.len();

    let mut params = Vec::with_capacity(2 + chunk + 3);
    params.extend_from_slice(&(chunk as u16).to_be_bytes());
    params.extend_from_slice(&blob[offset..offset + chunk]);
    if more {
        params.push(0x02);
        params.extend_from_slice(&(next as u16).to_be_bytes());
    } else {
        params.push(0x00);
    }
    log(&format!(
        "blob total={} offset={offset} chunk={chunk} more={more}",
        blob.len()
    ));
    sdp_send(stream, resp_pdu, tid, &params)
}

/// `0x02 ServiceSearchRequest -> 0x03 ServiceSearchResponse`. Always returns the single known
/// handle regardless of the search pattern's content -- this server only ever has one record, so
/// there's nothing to actually filter on, matching the reference's own deliberate shortcut.
fn handle_service_search(stream: &mut impl Write, tid: u16, params: &[u8]) -> std::io::Result<()> {
    let mut c = Cur { p: params, off: 0 };
    if c.skip_de().is_none() {
        return sdp_error(stream, tid, SDP_E_INVALID_SYNTAX);
    }
    let Some(_max_recs) = c.u16() else {
        return sdp_error(stream, tid, SDP_E_INVALID_SYNTAX);
    };
    if c.continuation().is_none() {
        return sdp_error(stream, tid, SDP_E_INVALID_CONTINUE);
    }
    log(&format!(
        "ServiceSearchRequest -- returning handle 0x{SVC_RECORD_HANDLE:08x}"
    ));
    let mut p = Vec::with_capacity(9);
    p.extend_from_slice(&1u16.to_be_bytes()); // TotalServiceRecordCount = 1
    p.extend_from_slice(&1u16.to_be_bytes()); // CurrentServiceRecordCount = 1
    p.extend_from_slice(&SVC_RECORD_HANDLE.to_be_bytes());
    p.push(0x00); // ContinuationState: none
    sdp_send(stream, SDP_SVC_SEARCH_RSP, tid, &p)
}

/// `0x04 ServiceAttributeRequest -> 0x05 ServiceAttributeResponse`.
fn handle_service_attr(
    stream: &mut impl Write,
    tid: u16,
    params: &[u8],
    record: &[u8],
) -> std::io::Result<()> {
    let mut c = Cur { p: params, off: 0 };
    let Some(handle) = c.u32() else {
        return sdp_error(stream, tid, SDP_E_INVALID_SYNTAX);
    };
    let Some(maxbytes) = c.u16() else {
        return sdp_error(stream, tid, SDP_E_INVALID_SYNTAX);
    };
    if c.skip_de().is_none() {
        return sdp_error(stream, tid, SDP_E_INVALID_SYNTAX);
    }
    let Some(resume) = c.continuation() else {
        return sdp_error(stream, tid, SDP_E_INVALID_CONTINUE);
    };
    log(&format!(
        "ServiceAttributeRequest handle=0x{handle:08x} maxbytes={maxbytes} resume={resume}"
    ));
    if handle != SVC_RECORD_HANDLE {
        return sdp_error(stream, tid, SDP_E_INVALID_HANDLE);
    }
    serve_attr_blob(stream, SDP_SVC_ATTR_RSP, tid, record, resume, maxbytes)
}

/// `0x06 ServiceSearchAttributeRequest -> 0x07 ServiceSearchAttributeResponse`.
fn handle_search_attr(
    stream: &mut impl Write,
    tid: u16,
    params: &[u8],
    ssa: &[u8],
) -> std::io::Result<()> {
    let mut c = Cur { p: params, off: 0 };
    if c.skip_de().is_none() {
        return sdp_error(stream, tid, SDP_E_INVALID_SYNTAX);
    }
    let Some(maxbytes) = c.u16() else {
        return sdp_error(stream, tid, SDP_E_INVALID_SYNTAX);
    };
    if c.skip_de().is_none() {
        return sdp_error(stream, tid, SDP_E_INVALID_SYNTAX);
    }
    let Some(resume) = c.continuation() else {
        return sdp_error(stream, tid, SDP_E_INVALID_CONTINUE);
    };
    log(&format!(
        "ServiceSearchAttributeRequest maxbytes={maxbytes} resume={resume}"
    ));
    serve_attr_blob(stream, SDP_SVC_SEARCH_ATTR_RSP, tid, ssa, resume, maxbytes)
}

/// Serve one accepted L2CAP connection until it closes. `SOCK_SEQPACKET` means one `read()` yields
/// exactly one whole SDP PDU. The 1s SO_RCVTIMEO `run` sets on the accepted fd surfaces here as
/// WouldBlock/TimedOut — that's the shutdown poll, not an error: an iPhone legitimately idles on an
/// open SDP channel, and treating idle as fatal (or blocking forever without the timeout) either
/// drops a live client or wedges `main.rs`'s join so AV teardown never runs (the #106 orphan class).
fn serve_client(stream: &mut std::fs::File, record: &[u8], ssa: &[u8], shutdown: &AtomicBool) {
    let mut buf = [0u8; 4096];
    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) => {
                log("client closed");
                return;
            }
            Ok(n) => n,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                if shutdown.load(Ordering::Relaxed) {
                    log("shutdown requested -- closing SDP client");
                    return;
                }
                continue; // idle peer, no shutdown — keep serving
            }
            Err(e) => {
                log(&format!("recv: {e}"));
                return;
            }
        };
        if n < 5 {
            log(&format!("runt PDU ({n} bytes)"));
            continue;
        }
        let pdu = buf[0];
        let tid = u16::from_be_bytes([buf[1], buf[2]]);
        let plen = u16::from_be_bytes([buf[3], buf[4]]) as usize;
        if plen + 5 > n {
            log(&format!("truncated PDU: plen={plen} have={}", n - 5));
            let _ = sdp_error(stream, tid, SDP_E_INVALID_PDU_SIZE);
            continue;
        }
        let params = &buf[5..5 + plen];
        log(&format!("<- pdu=0x{pdu:02x} tid=0x{tid:04x} plen={plen}"));
        let result = match pdu {
            SDP_SVC_SEARCH_REQ => handle_service_search(stream, tid, params),
            SDP_SVC_ATTR_REQ => handle_service_attr(stream, tid, params, record),
            SDP_SVC_SEARCH_ATTR_REQ => handle_search_attr(stream, tid, params, ssa),
            _ => {
                log(&format!("unsupported pdu 0x{pdu:02x}"));
                sdp_error(stream, tid, SDP_E_INVALID_SYNTAX)
            }
        };
        if let Err(e) = result {
            log(&format!("send failed: {e}"));
            return;
        }
    }
}

fn open_l2cap_listener() -> std::io::Result<std::fs::File> {
    // SOCK_CLOEXEC — this is the highest-stakes one in the tree. PSM 0x0001 is a well-known,
    // single-holder PSM, and this socket is open when bt_driver drives av::ensure_av_layer(), which
    // fork+execs airplayd/rx-connect SETSID-DETACHED TO OUTLIVE THIS PROCESS (av.rs:13). Without
    // CLOEXEC they inherit the PSM-1 binding; if this process then dies without reaching
    // teardown_av_layer() (SIGKILL/panic — `panic = "abort"`, so no unwinding), the restarted
    // instance's bind() finds the PSM still held by a daemon it cannot see, `run` returns Err, and
    // the failure is exactly the one this module's header documents: the iPhone browses SDP, finds
    // no iAP2 service, and disconnects.
    let fd = unsafe {
        libc::socket(
            AF_BLUETOOTH as libc::c_int,
            libc::SOCK_SEQPACKET | crate::cloexec::SOCK_CLOEXEC,
            BTPROTO_L2CAP,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let addr = SockaddrL2 {
        l2_family: AF_BLUETOOTH,
        l2_psm: SDP_PSM,
        l2_bdaddr: [0; 6],
        l2_cid: 0,
        l2_bdaddr_type: 0,
    };
    let ret = unsafe {
        libc::bind(
            fd,
            &addr as *const SockaddrL2 as *const libc::sockaddr,
            std::mem::size_of::<SockaddrL2>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    if unsafe { libc::listen(fd, 1) } < 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // A receive timeout on accept() would need a poll loop of its own; simplest for this phase is
    // a timeout on the listening fd so `run`'s shutdown check gets a chance between accepts.
    // zeroed()+assign, not a struct literal: under `musl32_time64` (riscv32) these
    // types carry private padding and a literal does not compile.
    let mut timeout: libc::timeval = unsafe { std::mem::zeroed() };
    timeout.tv_sec = 1;
    // Checked, like bind/listen above and the accepted-fd re-set in `run` below: an unnoticed
    // failure here would leave accept() blocking forever, so `run`'s shutdown check never fires.
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
    // SAFETY: fd is a freshly opened, bound, listening, exclusively-owned socket descriptor.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// Run the SDP accept loop until `shutdown` is set. One client at a time (matching the reference's
/// `listen(srv, 1)` backlog), each served to completion (or disconnect) before accepting the next.
pub fn run(rfcomm_chan: u8, shutdown: &AtomicBool) -> std::io::Result<()> {
    let (record, ssa) = build_records(rfcomm_chan);
    let listener = open_l2cap_listener()?;
    log(&format!(
        "serving 'Wireless iAPv2' (handle 0x{SVC_RECORD_HANDLE:08x}, RFCOMM ch {rfcomm_chan}) on L2CAP PSM 0x{SDP_PSM:04x}"
    ));

    let listen_fd = std::os::fd::AsRawFd::as_raw_fd(&listener);
    while !shutdown.load(Ordering::Relaxed) {
        let mut ra: SockaddrL2 = unsafe { std::mem::zeroed() };
        let mut ralen = std::mem::size_of::<SockaddrL2>() as libc::socklen_t;
        // accept4(SOCK_CLOEXEC) — same reasoning as open_l2cap_listener above.
        let cfd = unsafe {
            crate::cloexec::accept_cloexec(
                listen_fd,
                &mut ra as *mut SockaddrL2 as *mut libc::sockaddr,
                &mut ralen,
            )
        };
        if cfd < 0 {
            let e = std::io::Error::last_os_error();
            // audit Fix #10: a single TRANSIENT accept error must not kill the SDP server for the whole
            // session — a client that RSTs mid-handshake (ECONNABORTED) or an EINTR would otherwise stop
            // ALL SDP responses, i.e. "phone finds no iAP2 service". Keep accepting on those (matching
            // bt_driver's resilient RFCOMM accept); only a genuinely unexpected error (likely a broken
            // listener) still surfaces as Err. The while-guard re-checks shutdown each pass, and the 1s
            // SO_RCVTIMEO bounds each accept, so continue-on-transient cannot trap the thread at teardown.
            match e.kind() {
                std::io::ErrorKind::WouldBlock
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::Interrupted => continue,
                _ => return Err(e),
            }
        }
        log("client connected on SDP PSM");
        // Re-set SO_RCVTIMEO on the ACCEPTED fd (review fix 2026-07-31): Linux Bluetooth sockets do
        // NOT inherit the listener's timeout on accept (l2cap_sock_alloc → sock_init_data resets
        // sk_rcvtimeo on this 3.14.52 kernel) — the timeout set in `open_l2cap_listener` only covers
        // accept() itself. Without this, `serve_client` blocks forever on an idling-but-connected
        // peer and the shutdown flag is never polled. Same explicit re-set bt_driver.rs does on ITS
        // accepted RFCOMM socket, and checked like ssp_agent.rs's, since an unnoticed failure here
        // re-opens the forever-block.
        // zeroed()+assign, not a struct literal: under `musl32_time64` (riscv32) these
        // types carry private padding and a literal does not compile.
        let mut timeout: libc::timeval = unsafe { std::mem::zeroed() };
        timeout.tv_sec = 1;
        let ret = unsafe {
            libc::setsockopt(
                cfd,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                &timeout as *const libc::timeval as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        };
        let sockopt_err = if ret < 0 {
            Some(std::io::Error::last_os_error())
        } else {
            None
        };
        let mut client = unsafe { std::fs::File::from_raw_fd(cfd) };
        if let Some(e) = sockopt_err {
            // Serving without the timeout would block unboundedly on an idle peer — drop this
            // client (File closes cfd) and keep accepting rather than risk the wedge.
            log(&format!("SO_RCVTIMEO on accepted fd failed: {e} -- dropping client"));
            continue;
        }
        serve_client(&mut client, &record, &ssa, shutdown);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_records_patches_rfcomm_channel() {
        let (record, ssa) = build_records(1);
        assert_eq!(record[RFCOMM_CHAN_IDX], 1);
        assert_eq!(record.len(), 89);
        assert_eq!(ssa.len(), 91);
        assert_eq!(&ssa[0..2], &[0x35, 89]);
        assert_eq!(&ssa[2..], &record[..]);
    }

    #[test]
    fn build_records_with_different_channel() {
        let (record, _) = build_records(7);
        assert_eq!(record[RFCOMM_CHAN_IDX], 7);
    }

    #[test]
    fn de_parse_single_byte_length() {
        // desc=0x08 (type=1,size=0) -> 1-byte data element, header 1, data 1.
        assert_eq!(de_parse(&[0x08, 0xAB]), Some((1, 1)));
    }

    #[test]
    fn de_parse_explicit_one_byte_length() {
        // desc=0x05 (size code 5) -> next byte is an explicit 1-byte length.
        assert_eq!(de_parse(&[0x05, 0x03, 1, 2, 3]), Some((2, 3)));
    }

    #[test]
    fn cursor_continuation_none() {
        let mut c = Cur { p: &[0x00], off: 0 };
        assert_eq!(c.continuation(), Some(0));
    }

    #[test]
    fn cursor_continuation_with_offset() {
        let mut c = Cur {
            p: &[0x02, 0x01, 0x2c],
            off: 0,
        };
        assert_eq!(c.continuation(), Some(0x012c));
    }

    #[test]
    fn cursor_continuation_rejects_unknown_info_length() {
        let mut c = Cur {
            p: &[0x05, 0, 0, 0, 0, 0],
            off: 0,
        };
        assert_eq!(c.continuation(), None);
    }

    #[test]
    fn serve_attr_blob_chunks_when_over_maxbytes() {
        let mut out = Vec::new();
        let blob = [1u8, 2, 3, 4, 5];
        serve_attr_blob(&mut out, SDP_SVC_ATTR_RSP, 0x1234, &blob, 0, 3).unwrap();
        // header: pdu(1) tid(2) plen(2), then ByteCount(2)=3, 3 data bytes, continuation 02 00 03
        assert_eq!(out[0], SDP_SVC_ATTR_RSP);
        let plen = u16::from_be_bytes([out[3], out[4]]) as usize;
        let params = &out[5..5 + plen];
        assert_eq!(&params[0..2], &3u16.to_be_bytes());
        assert_eq!(&params[2..5], &[1, 2, 3]);
        assert_eq!(&params[5..8], &[0x02, 0x00, 0x03]);
    }

    #[test]
    fn serve_attr_blob_no_continuation_when_it_fits() {
        let mut out = Vec::new();
        let blob = [1u8, 2, 3];
        serve_attr_blob(&mut out, SDP_SVC_ATTR_RSP, 0, &blob, 0, 0).unwrap();
        let plen = u16::from_be_bytes([out[3], out[4]]) as usize;
        let params = &out[5..5 + plen];
        assert_eq!(&params[0..2], &3u16.to_be_bytes());
        assert_eq!(&params[2..5], &[1, 2, 3]);
        assert_eq!(params[5], 0x00);
    }

    #[test]
    fn handle_service_search_always_returns_the_one_handle() {
        let mut out = Vec::new();
        // search pattern: an empty-ish sequence (desc=0x35 size-code5, then explicit len 0)
        let params = [
            0x35u8, 0x00, /* max_recs */ 0x00, 0x0a, /* continuation */ 0x00,
        ];
        handle_service_search(&mut out, 0x0007, &params).unwrap();
        assert_eq!(out[0], SDP_SVC_SEARCH_RSP);
        let plen = u16::from_be_bytes([out[3], out[4]]) as usize;
        let p = &out[5..5 + plen];
        assert_eq!(&p[0..2], &1u16.to_be_bytes());
        assert_eq!(&p[2..4], &1u16.to_be_bytes());
        assert_eq!(&p[4..8], &SVC_RECORD_HANDLE.to_be_bytes());
    }
}
