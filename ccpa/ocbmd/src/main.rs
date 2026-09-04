//! ocbmd — box-side OCBM daemon (Rust, armv7/musl). Owns `/dev/usb_accessory` and
//! multiplexes CTRL (handshake), ECHO (loopback), CONSOLE (root PTY over bulk),
//! MFI (genuine Apple MFi 2.0C authentication bridge over `/dev/i2c-1`), IP (userspace
//! TCP/UDP mux), and FILE (verified binary deploy). See ../../docs/carplay/01_OCBM_PROTOCOL.md.

use ocbm_proto as p;
use std::collections::HashMap;
use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io::{IoSlice, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::time::{Duration, Instant};

const ACC_DEV: &str = "/dev/usb_accessory";

/// airplayd's local HID-input ingest (task #20). ocbmd relays each CH_INPUT sub-frame here, length-
/// prefixed, and airplayd turns it into an encrypted `hidSendReport` to the iPhone. Lazy-connected; a
/// failed write drops the socket so the next event reconnects (airplayd restarts per session).
const INPUT_INGEST_ADDR: &str = "127.0.0.1:9110";

/// airplayd's local mic-uplink ingest seam. ocbmd relays each CH_MIC payload here as `mic <len>\n<pcm>`
/// and reads the `uplink on <rate> <ch>` / `uplink off` back-channel, which it re-emits to the host as
/// CH_CTRL CT_UPLINK so the app gates mic capture on the real type-100 `input` SETUP edge. Bidirectional
/// + non-blocking so it slots into the poll loop; a write error drops the socket to reconnect next chunk.
const MIC_INGEST_ADDR: &str = "127.0.0.1:9112";

/// airplayd's app-driven-SETUP relay seam (plan P1; `receiver::relay::start_listener` on the box).
/// ocbmd is a DUMB BYTE PIPE for CH_RTSP: box→host it chunks whatever airplayd writes into ≤64 KiB
/// OCBM frames; host→box it writes CH_RTSP payloads back verbatim. All message framing
/// (`[u32 BE "RTSP"][u32 BE len][msg]`, RS_* ops) is endpoint-to-endpoint — airplayd ↔ host app.
const RTSP_INGEST_ADDR: &str = "127.0.0.1:9106";

/// What a pollfd slot represents, so the poll loop can dispatch after `poll()`.
#[derive(Clone, Copy)]
enum Kind {
    Acc,
    Pty,
    Conn(u16),
    Eth,
    Mic,             // the mic-uplink seam to airplayd (readable: `uplink on/off` back-channel)
    Rtsp,            // the SETUP-relay seam to airplayd (readable: box→host RS_OPEN/RS_REQ/RS_CLOSE bytes)
    AvListen(usize), // a local A/V seam listener (index into av_listeners)
    AvConn(usize),   // an accepted A/V seam connection (index into av_conns)
}

/// Raw L2 ethernet bridge for CH_ETH: an AF_PACKET socket on `ncm0` carries the iPhone's
/// wired-CarPlay link-local IPv6 (mDNS/NDP/RTSP/AirPlay) frames verbatim over OCBM to the host,
/// where they land on a virtual ethernet interface the receiver binds `fe80::` on. Raw AF_PACKET
/// is Linux-only, so this is cfg-gated with a stub elsewhere (keeps host-side unit tests building).
#[cfg(target_os = "linux")]
mod eth {
    use libc::c_void;
    use std::os::unix::io::RawFd;
    const ETH_P_ALL: u16 = 0x0003;
    const PACKET_OUTGOING: u8 = 4;

    /// Open a non-blocking AF_PACKET SOCK_RAW socket bound to `ifname`, or None if absent.
    pub fn open(ifname: &str) -> Option<RawFd> {
        let mut cname = [0u8; 32];
        let b = ifname.as_bytes();
        if b.is_empty() || b.len() >= cname.len() {
            return None;
        }
        cname[..b.len()].copy_from_slice(b);
        let idx = unsafe { libc::if_nametoindex(cname.as_ptr() as *const libc::c_char) };
        if idx == 0 {
            return None; // interface doesn't exist (yet)
        }
        let proto = (ETH_P_ALL.to_be()) as libc::c_int;
        // SOCK_CLOEXEC so this raw socket does not leak into the CONSOLE root shell on execv.
        let fd =
            unsafe { libc::socket(libc::AF_PACKET, libc::SOCK_RAW | libc::SOCK_CLOEXEC, proto) };
        if fd < 0 {
            return None;
        }
        let mut sll: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
        sll.sll_family = libc::AF_PACKET as u16;
        sll.sll_protocol = ETH_P_ALL.to_be();
        sll.sll_ifindex = idx as i32;
        let r = unsafe {
            libc::bind(
                fd,
                &sll as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t,
            )
        };
        if r < 0 {
            unsafe { libc::close(fd) };
            return None;
        }
        unsafe {
            let fl = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
        Some(fd)
    }

    /// Receive the next INCOMING frame (Some(len)), internally skipping frames we sent
    /// ourselves (PACKET_OUTGOING) so a bridged frame can't loop back out over OCBM. Returns
    /// None only when the socket is drained (would-block) or errors — so a caller can batch-drain.
    pub fn recv_frame(fd: RawFd, buf: &mut [u8]) -> Option<usize> {
        loop {
            let mut sa: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
            let mut salen = std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t;
            let n = unsafe {
                libc::recvfrom(
                    fd,
                    buf.as_mut_ptr() as *mut c_void,
                    buf.len(),
                    // MSG_TRUNC makes recvfrom report the REAL frame length rather than the copied
                    // one, so an oversize frame can be detected instead of being bridged truncated
                    // (a silently corrupt frame is worse than a dropped one).
                    libc::MSG_TRUNC,
                    &mut sa as *mut _ as *mut libc::sockaddr,
                    &mut salen,
                )
            };
            if n <= 0 {
                return None; // drained / error
            }
            if sa.sll_pkttype == PACKET_OUTGOING {
                continue; // our own transmitted frame echoed back — skip and read the next
            }
            if n as usize > buf.len() {
                continue; // did not fit: drop it rather than bridge a truncated frame
            }
            return Some(n as usize); // a real inbound frame
        }
    }

    /// Send one frame onto the bound interface (host -> iPhone direction).
    pub fn send_frame(fd: RawFd, data: &[u8]) {
        unsafe { libc::send(fd, data.as_ptr() as *const c_void, data.len(), 0) };
    }

    pub fn close(fd: RawFd) {
        unsafe { libc::close(fd) };
    }
}

#[cfg(not(target_os = "linux"))]
mod eth {
    use std::os::unix::io::RawFd;
    pub fn open(_ifname: &str) -> Option<RawFd> {
        None
    }
    pub fn recv_frame(_fd: RawFd, _buf: &mut [u8]) -> Option<usize> {
        None
    }
    pub fn send_frame(_fd: RawFd, _data: &[u8]) {}
    pub fn close(_fd: RawFd) {}
}

/// A relayed CH_IP connection — TCP stream or UDP datagram socket.
enum Conn {
    Tcp(TcpStream),
    Udp(UdpSocket),
}
impl Conn {
    fn raw_fd(&self) -> RawFd {
        match self {
            Conn::Tcp(s) => s.as_raw_fd(),
            Conn::Udp(s) => s.as_raw_fd(),
        }
    }
}

/// An in-progress CH_FILE push. Bytes land in a `.ocbm.part` temp; on a verified close
/// the temp is chmod'd and atomically renamed onto `path`, so a failed/aborted transfer
/// never leaves a half-written or non-executable binary in place.
struct FileXfer {
    f: File,
    path: String,
    tmp: String,
    mode: u32,
    crc: u32, // running CRC (seed p::CRC32_INIT), finalized at close
    size: u32,
}

/// CH_FILE receive state machine (one active transfer). Kept separate from the poll loop
/// so it can be driven directly in tests. `on_frame` returns `Some((status, crc, size))`
/// to send a FILE_ACK, or `None` for a silently-accepted data chunk.
#[derive(Default)]
struct FileState {
    cur: Option<FileXfer>,
}

impl FileState {
    fn on_frame(&mut self, pl: &[u8]) -> Option<(u8, u32, u32)> {
        match pl.first().copied() {
            Some(p::FILE_OPEN) => {
                if pl.len() < 5 {
                    return Some((p::FILE_ERR_OPEN, 0, 0));
                }
                // Mask off setuid/setgid/sticky bits: the file mode comes from the (trusted-but-still
                // untrusted-input) host, and a setuid-root binary written anywhere would be full remote
                // root. Keep only the standard rwx permission bits.
                let mode = u32::from_le_bytes([pl[1], pl[2], pl[3], pl[4]]) & 0o777;
                let path = match std::str::from_utf8(&pl[5..]) {
                    // Reject path traversal / relative escapes; deploys use absolute paths under the rootfs.
                    Ok(s) if !s.is_empty() && !s.contains("..") && s.starts_with('/') => {
                        s.to_string()
                    }
                    _ => return Some((p::FILE_ERR_OPEN, 0, 0)),
                };
                // discard any prior temp before starting a new one
                if let Some(old) = self.cur.take() {
                    let _ = std::fs::remove_file(&old.tmp);
                }
                let tmp = format!("{}.ocbm.part", path);
                match OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&tmp)
                {
                    Ok(f) => {
                        self.cur = Some(FileXfer {
                            f,
                            path,
                            tmp,
                            mode,
                            crc: p::CRC32_INIT,
                            size: 0,
                        });
                        Some((p::FILE_OK, 0, 0))
                    }
                    Err(_) => Some((p::FILE_ERR_OPEN, 0, 0)),
                }
            }
            Some(p::FILE_DATA) => {
                let data = &pl[1..];
                let failed = match self.cur.as_mut() {
                    Some(fx) => {
                        if fx.f.write_all(data).is_ok() {
                            fx.crc = p::crc32_update(fx.crc, data);
                            fx.size = fx.size.wrapping_add(data.len() as u32);
                            false
                        } else {
                            true
                        }
                    }
                    None => return Some((p::FILE_ERR_NOFILE, 0, 0)),
                };
                if failed {
                    if let Some(fx) = self.cur.take() {
                        let _ = std::fs::remove_file(&fx.tmp);
                    }
                    Some((p::FILE_ERR_WRITE, 0, 0))
                } else {
                    None // accepted, no per-chunk ack (keeps the pipe full)
                }
            }
            Some(p::FILE_CLOSE) => {
                let mut fx = match self.cur.take() {
                    Some(x) => x,
                    None => return Some((p::FILE_ERR_NOFILE, 0, 0)),
                };
                // A truncated close carries no verifiable crc/size — reject. The old (0,0)
                // default let a malformed close verify an empty transfer as FILE_OK.
                if pl.len() < 9 {
                    let _ = std::fs::remove_file(&fx.tmp);
                    return Some((p::FILE_ERR_VERIFY, p::crc32_final(fx.crc), fx.size));
                }
                let (exp_crc, exp_size) = (
                    u32::from_le_bytes([pl[1], pl[2], pl[3], pl[4]]),
                    u32::from_le_bytes([pl[5], pl[6], pl[7], pl[8]]),
                );
                let got_crc = p::crc32_final(fx.crc);
                let got_size = fx.size;
                let durable = fx.f.flush().is_ok() && fx.f.sync_all().is_ok();
                drop(fx.f); // close before rename
                if got_crc == exp_crc && got_size == exp_size && durable {
                    let _ =
                        std::fs::set_permissions(&fx.tmp, std::fs::Permissions::from_mode(fx.mode));
                    if std::fs::rename(&fx.tmp, &fx.path).is_ok() {
                        Some((p::FILE_OK, got_crc, got_size))
                    } else {
                        let _ = std::fs::remove_file(&fx.tmp);
                        Some((p::FILE_ERR_WRITE, got_crc, got_size))
                    }
                } else {
                    let _ = std::fs::remove_file(&fx.tmp);
                    Some((p::FILE_ERR_VERIFY, got_crc, got_size))
                }
            }
            _ => None,
        }
    }
}

// ---- MFi authentication bridge over I2C (/dev/i2c-1 @0x11) — ported from the ncm_carplayd MFi auth helper ----
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

struct Mfi {
    fd: RawFd,
    addr: u16,
}

/// Bounded `flock` on `/tmp/carplay_mfi.lock`, the same file `airplayd`, `iap2d`,
/// `carplay-wireless` and `mfi-i2c-local` use.
///
/// `ocbmd` is the FIFTH chip user and took no lock at all — so a CH_MFI `sign` from the host could
/// interleave its stateful write-challenge / poll / read-signature sequence with any other daemon's,
/// corrupting both. The serialization guarantee every "four chip users" comment in the tree asserts
/// was simply not complete. Same corruption class the 2026-07-25 QC pass fixed for `airplayd`.
///
/// Bounded, not `LOCK_EX`: a wedged peer must not hang the OCBM dispatch loop, which also carries
/// video, audio and the console.
struct MfiLock(RawFd);

impl MfiLock {
    fn acquire() -> Option<MfiLock> {
        const DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);
        let fd = unsafe {
            libc::open(
                c"/tmp/carplay_mfi.lock".as_ptr(),
                libc::O_CREAT | libc::O_RDWR | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return None;
        }
        let start = std::time::Instant::now();
        loop {
            if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Some(MfiLock(fd));
            }
            if start.elapsed() >= DEADLINE {
                eprintln!("[mfi] lock busy >10s — another chip user is wedged; refusing");
                unsafe { libc::close(fd) };
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
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

impl Mfi {
    fn open() -> Option<Mfi> {
        // O_CLOEXEC so the MFi chip handle does not leak into the CONSOLE root shell on execv.
        let fd = unsafe { libc::open(c"/dev/i2c-1".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            None
        } else {
            Some(Mfi { fd, addr: 0x11 })
        }
    }
    fn rd(&self, reg: u8, out: &mut [u8]) -> bool {
        let mut r = reg;
        let mut msgs = [
            I2cMsg {
                addr: self.addr,
                flags: 0,
                len: 1,
                buf: &mut r,
            },
            I2cMsg {
                addr: self.addr,
                flags: I2C_M_RD,
                len: out.len() as u16,
                buf: out.as_mut_ptr(),
            },
        ];
        let mut x = I2cRdwr {
            msgs: msgs.as_mut_ptr(),
            nmsgs: 2,
        };
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
        let mut msg = I2cMsg {
            addr: self.addr,
            flags: 0,
            len: b.len() as u16,
            buf: b.as_mut_ptr(),
        };
        let mut x = I2cRdwr {
            msgs: &mut msg,
            nmsgs: 1,
        };
        for _ in 0..5 {
            if unsafe { libc::ioctl(self.fd, I2C_RDWR as _, &mut x) } >= 0 {
                return true;
            }
            unsafe { libc::usleep(5000) };
        }
        false
    }
    /// CopyCertificate: reg 0x30 = length (2 BE), reg 0x31 = cert bytes.
    fn cert(&self) -> Option<Vec<u8>> {
        let _lock = MfiLock::acquire()?; // serialize the whole cert read vs the other four users
        let mut lb = [0u8; 2];
        if !self.rd(0x30, &mut lb) {
            return None;
        }
        let n = ((lb[0] as usize) << 8) | lb[1] as usize;
        if n == 0 || n > 2048 {
            return None;
        }
        let mut o = vec![0u8; n];
        if !self.rd(0x31, &mut o) {
            return None;
        }
        Some(o)
    }
    /// CreateSignature: write challenge (0x20 len, 0x21 data), go (0x10=1), poll 0x10 bit4, read 0x11 len / 0x12 sig.
    fn sign(&self, chal: &[u8]) -> Option<Vec<u8>> {
        let _lock = MfiLock::acquire()?; // hold across the ENTIRE stateful challenge/poll/read sequence
        let clen = chal.len();
        if !self.wr(0x20, &[(clen >> 8) as u8, clen as u8]) {
            return None;
        }
        if !self.wr(0x21, chal) {
            return None;
        }
        if !self.wr(0x10, &[0x01]) {
            return None;
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
            return None;
        }
        let mut sl = [0u8; 2];
        if !self.rd(0x11, &mut sl) {
            return None;
        }
        let n = ((sl[0] as usize) << 8) | sl[1] as usize;
        if n == 0 || n > 256 {
            return None;
        }
        let mut sig = vec![0u8; n];
        if !self.rd(0x12, &mut sig) {
            return None;
        }
        Some(sig)
    }
}

struct Daemon {
    acc: File,
    seq: u32,
    mfi: Option<Mfi>,
    ptm: Option<File>,
    file: FileState,
    eth: Option<RawFd>, // AF_PACKET raw socket bridging ncm0 <-> CH_ETH (host-driven, on demand)
    // Local A/V ingest seam: the box AirPlay session (airplayd / AvSession) connects to these local
    // ports and streams A/V; ocbmd muxes each onto its OCBM channel to the host app. The reusable
    // box-session -> ocbmd -> OCBM path (payload is decoded now via AvSession; encrypted later).
    av_listeners: Vec<(TcpListener, u16)>, // (listener, target OCBM channel)
    // (accepted stream, target OCBM channel, FRESH). `fresh` is true until the first byte from that
    // connection has been forwarded; that first frame carries `F_NEW_SOURCE` so the host drops whatever
    // partial message the PREVIOUS producer on the same channel left in its reassembly buffer. A
    // re-SETUP reconnects the seam and the old producer is dropped WITHOUT draining (see the `av_new`
    // handling), so without this the new producer's first bytes land mid-message and the host's
    // byte-stream reassembly for that channel desyncs permanently. Connection lifecycle only — the seam
    // payload is still forwarded byte-for-byte untouched (box forwards, app processes).
    av_conns: Vec<(TcpStream, u16, bool)>,
    conns: HashMap<u16, Conn>,
    out_hi: OutQueue, // priority output FIFO: CTRL/MFI/RTSP (reliable, latency-sensitive)
    out_console: OutQueue,                        // CH_CONSOLE — drained AFTER A/V so a console flood can't starve it (audit B3)
    // LIVE A/V — one queue PER stream so each video seam's read-gate keys on ITS OWN backlog, not a
    // shared one. A single shared FIFO coupled the two video streams + audio: the low-rate cluster
    // (:9005) / audio kept it non-empty, so the main 4K seam was almost never read → ~2fps + stalls
    // (audit 2026-07-12 H1). Separate queues + per-stream gating (see the poll loop) fix that; audio is
    // never gated and drains first so it never waits behind a video frame.
    out_video: OutQueue,                          // CH_VIDEO (main screen)
    out_alt_video: OutQueue,                      // CH_ALT_VIDEO (cluster / type-111)
    out_audio: OutQueue,                          // CH_MEDIA_AUDIO + CH_ALT_AUDIO (never gated)
    out_log: OutQueue, // CH_LOG — below CONSOLE, above bulk: diagnostics must never delay the
    // control plane, A/V, or an interactive rescue console, but they should still beat a file pull.
    out_lo: OutQueue,                             // bulk output FIFO: ECHO/IP/FILE/ETH (reliable)
    /// Universal-log tailer (`/tmp/box.log` -> CH_LOG). Off until the host sends `CT_LOG_CTL`.
    log: LogTail,
    /// Which queue is resting MID-FRAME on the shared accessory fd, if any. While set, that queue must
    /// be finished to a frame boundary before any other queue writes — see `drain` (frame splicing).
    wire_owner: Option<Wire>,
    av_dropped: u64, // live-A/V frames dropped at the OOM backstop (queue hit OUT_QUEUE_CAP). Video is
    // gated so it cannot reach the cap; in practice this only ever counts audio.
    av_backpressured: bool, // currently over the A/V queue cap (for transition-only logging)
    lo_dropped: u64,        // reliable-bulk frames dropped because out_lo hit its OOM cap
    lo_capped: bool,        // currently over the out_lo cap (for transition-only logging)
    lo_resync: bool, // dropping bulk frames until the next F_SOM after a mid-message cap-clear (#567)
    /// Set when a CT_HELLO cleared stale CH_IP sockets earlier in THIS dispatch pass, so the poll
    /// loop does not send the new host an IP_CLOSE for a conn id that was the dead host's. Cleared at
    /// the end of every pass.
    hello_cleared_conns: bool,
    last_phone_check: Option<std::time::Instant>, // throttle the /tmp/phone_present stat (#673)
    // Session presence (docs/carplay/02_SESSION_LIFECYCLE.md lifecycle): the host app SUBSCRIBEs + HEARTBEATs; a watchdog declares
    // it gone if beats stop. `present` is the cross-process signal (also mirrored to /tmp/host_present)
    // that rx_connect/airplayd will read to gate advertising + drive teardown.
    subscribed: bool,
    last_hb: Option<std::time::Instant>,
    present: bool,
    /// Who the host says it is: an optional UTF-8 label carried after the nonce in CT_HELLO.
    ///
    /// The nonce distinguishes host PROCESSES; it says nothing about what KIND of host they are. The
    /// box serves several — a GM head-unit app that owns Wi-Fi and uses the box as a BT+MFi bridge,
    /// an Android Auto host driving `aa-bridge` over CH_IP, bench tooling — and its behaviour and its
    /// logs both differ between them, yet until now "what is talking to me" was unanswerable. Purely
    /// diagnostic: nothing gates on it, and an absent label reads exactly as it always did.
    host_name: Option<String>,
    /// Last host instance nonce seen in CT_HELLO (see `ocbm_proto::CT_HELLO`). None until a host that
    /// supplies one connects; 0 is never stored (older hosts opt out by sending it).
    host_instance: Option<u32>,
    /// Set when a HELLO arrives with a DIFFERENT instance nonce while we still believe a host is
    /// present — i.e. the previous host died without CT_STOP and this is its replacement. Consumed by
    /// the next CT_SUBSCRIBE, which owes the presence re-arm that host's CT_STOP never sent.
    host_replaced: bool,
    /// When to restore `/tmp/host_present` to 1 after a silent re-arm. See [`REARM_HOLD`].
    rearm_deadline: Option<std::time::Instant>,
    /// When `/tmp/host_present` was last written 0 because presence actually DROPPED (`CT_STOP` or
    /// heartbeat loss — not the `rearm_presence_silently` dip, which never lowers `present`).
    ///
    /// The supervisor is EDGE-triggered on that flag at 1 Hz, so a 0 it never sampled is a 0 that
    /// never happened. Read once, by the `CT_SUBSCRIBE` raise, to decide whether the GONE edge has
    /// been visible long enough to raise the flag now or has to be held. See `raise_presence`.
    present_cleared_at: Option<std::time::Instant>,
    cfg: Vec<u8>, // last host-pushed YAML config — EPHEMERAL, per session, never persisted (docs/carplay/02_SESSION_LIFECYCLE.md)
    input_sock: Option<TcpStream>, // lazy connection to airplayd's HID-input ingest (task #20)
    input_fwd: u64, // count of HID input events relayed (observability)
    input_dropped: u64, // count of HID input events dropped (bad size / no seam / failed send)
    mic_sock: Option<TcpStream>, // lazy bidirectional connection to airplayd's mic-uplink seam (CH_MIC)
    mic_rx: Vec<u8>, // partial-line buffer for the mic seam's `uplink on/off` back-channel
    /// True between the `uplink on` we relayed to the host and the matching `uplink off`. When the
    /// seam drops in that window (peer closed after its `uplink off` raced our next PCM write, poll
    /// HUP, read error) the OFF is synthesized to the host: a seam owner that is gone cannot be
    /// consuming mic audio, and without this the app kept a hot mic for 45 min after a hangup
    /// (measured 2026-09-04, first live HFP calls).
    mic_uplink_on: bool,
    mic_fwd: u64,    // count of mic PCM chunks relayed (observability)
    // Bidirectional connection to airplayd's SETUP-relay seam (CH_RTSP ↔ :9106). EAGER while a host
    // is subscribed (see ensure_rtsp_seam) — the relay's RS_OPEN fires at pair-verify, BEFORE any
    // host→box bytes could lazily trigger a connect.
    rtsp_sock: Option<TcpStream>,
    phone_state: Option<bool>, // last phone-on-bus state mirrored to the host (None = not yet read)
    // Wireless SSP Numeric-Comparison pairing code mirrored to the host (ssp_agent writes /tmp/pairing_code
    // during pairing; None = not yet read, Some("") = cleared/hidden, Some("418926") = show it).
    pairing_code: Option<String>,
    /// Last phone identity JSON forwarded, so only CHANGES go on the wire. None = not yet read.
    phone_ident: Option<String>,
    last_phone_ident_check: Option<std::time::Instant>,
    /// Last BT phase forwarded, so only CHANGES go on the wire.
    bt_phase: Option<u8>,
    /// Last `CT_BOX_HEALTH` bitmask sent. None until the first tick, and reset on a fresh SUBSCRIBE
    /// so a newly attached host is told the current health without waiting for it to change.
    box_health: Option<u8>,
    /// Handle on the `bt_probe` thread's published samples. The ONLY way the dispatch loop learns
    /// anything about the controller — see [`BtProbe`] for the hang that forced it off this thread.
    bt: std::sync::Arc<BtProbe>,
    /// Throttle for [`Daemon::box_health_tick`]. Reading it costs a /proc walk, so it is sampled at
    /// a much lower rate than the file-backed mirrors next to it.
    last_box_health_check: Option<std::time::Instant>,
    last_pairing_check: Option<std::time::Instant>,
    last_bt_phase_check: Option<std::time::Instant>,
    /// Last projection mode (`PM_*`) forwarded, so only CHANGES go on the wire. None = not yet read.
    proj_mode: Option<u8>,
    last_proj_mode_check: Option<std::time::Instant>,
    /// Throttle for the periodic health line. See [`Daemon::health_tick`].
    last_health_log: Option<std::time::Instant>,
}

/// Host-presence heartbeat grace: if a subscribed host misses beats for this long, it is declared gone.
/// Host beats ~1/s. Widened 3s→10s (QC #428): 3s is 3-5x tighter than the adapter ground truth and its
/// expiry is maximally destructive (drops the subscription + ephemeral config, forcing a full
/// re-SUBSCRIBE + session rebuild). A macOS host can miss several consecutive beats to App-Nap / a brief
/// USB stall without the session actually being dead; 10s absorbs that while still bounding a truly-gone
/// host well under any user-perceptible hang.
const HEARTBEAT_GRACE: Duration = Duration::from_secs(10);

/// Where airplayd publishes the connected phone's identity (mirrors `receiver::session`'s constant;
/// ocbmd does not link that crate).
const PHONE_IDENT_FILE: &str = "/tmp/phone_identity";

/// How long `/tmp/host_present` must read 0 for the supervisor to SEE the re-arm edge.
///
/// The supervisor is a shell loop that polls the flag about once a second, so a false->true flip
/// written back-to-back is invisible to it — measured: the host reconnected cleanly and projection
/// never came back, because the edge the supervisor re-ARMs on never appeared. Two seconds clears one
/// poll interval with margin. Host-facing presence is NOT affected: `self.present` stays true
/// throughout, because the host really is present.
const REARM_HOLD: Duration = Duration::from_secs(2);

/// Cadence of the periodic health line while a host is subscribed. Long enough that it is not what
/// a captured log is mostly made of, short enough that a queue which has been backed up for a while
/// shows up in one.
const HEALTH_LOG_INTERVAL: Duration = Duration::from_secs(30);

/// Ceiling on the reliable output queues (`out_hi`/`out_lo`). Far above normal control/bulk traffic
/// (frames are small and infrequent) but far below the ~123 MB no-swap box's OOM point — a stalled host
/// reader or a console flood can't grow these without bound. The same cap also backstops the live-A/V
/// queues, where it is an OOM guard rather than a drop policy — video is gated on its own backlog so it
/// cannot reach the cap; ungated audio can, after a multi-second stall.
const OUT_QUEUE_CAP: usize = 1 << 20; // 1 MiB

/// The box's universal log: everything that is NOT parsed by another process appends here with
/// `O_APPEND` (the run scripts redirect stdout/stderr into it, ocbmd's own included). `/tmp` is
/// tmpfs, so this file is a bounded STAGING area and never storage — the tailer rotates it at the
/// cap rather than letting it eat RAM the OS and the daemons need. It is the ONLY source the
/// tailer writes to.
const LOG_FILE: &str = "/tmp/box.log";

/// The followed sources, `(LOG_SRC_* id, path)`, in the order they are polled.
///
/// Everything after the first entry is TAIL-ONLY. Those files could not simply be redirected into
/// `box.log`, because `session_supervisor.sh` and `projection_up.sh` PARSE them as IPC — the
/// pair-verify `grep`, the `tail -1` stall checks, and `bound_logs`' own reap list — so they keep
/// their own identity and lifecycle and the tailer never truncates them. `bound_logs` rewrites them
/// in place (`tail -c` into the same inode), which reads here as a shrink and restarts that source
/// at 0: the retained tail is re-sent, which is the right failure direction for a log.
const LOG_SOURCES: &[(u8, &str)] = &[
    (p::LOG_SRC_BOX, LOG_FILE),
    (p::LOG_SRC_AIRPLAYD, "/tmp/airplayd.log"),
    (p::LOG_SRC_AIRPLAYD_WL, "/tmp/airplayd_wl.log"),
    (p::LOG_SRC_IAP2D, "/tmp/iap2d.log"),
    (p::LOG_SRC_AA_BRIDGE, "/tmp/aa-bridge.log"),
    (p::LOG_SRC_RX_CONNECT, "/tmp/rx-connect.log"),
    (p::LOG_SRC_BT, "/tmp/bt.log"),
    (p::LOG_SRC_RADIO_AP_DHCP, "/tmp/radio_ap_dhcp.log"),
    (p::LOG_SRC_RADIO_BT_ATTACH, "/tmp/radio_bt_attach.log"),
    (p::LOG_SRC_RX_CONNECT_WL, "/tmp/rx-connect_wl.log"),
    (p::LOG_SRC_CARPLAY_WIRELESS, "/tmp/wl.log"),
];

/// Bytes read per tick ACROSS ALL SOURCES. Bounds the work one dispatch pass can spend on logs, so
/// a daemon that just dumped a megabyte cannot stall A/V — the backlog drains over the next ticks.
const LOG_READ_CHUNK: usize = 8192;

/// Ceiling on everything the log path holds in RAM: encoded entries not yet framed, plus framed
/// bytes not yet written. Over it the OLDEST pending entries are dropped and counted, so a stalled
/// host turns a log stream into a reported gap rather than an OOM. Two orders below OUT_QUEUE_CAP:
/// diagnostics must never compete with the control plane for the box's memory.
const LOG_QUEUE_CAP: usize = 64 * 1024;

/// Tailer cadence while streaming — the same `Instant`-gated pattern as the `/tmp/bt_phase` mirror.
const LOG_TICK: Duration = Duration::from_millis(250);

/// Cadence of the cap check while NOT streaming: one `stat`, no reads. It still has to run, because
/// "disabled" is where the box spends most of its life and the file grows the whole time.
const LOG_IDLE_TICK: Duration = Duration::from_secs(2);

/// Poll timeout on a fully idle box (no host, no pending deadline). Was an indefinite block: every
/// box daemon appends to the tmpfs log whether or not a host is attached, so the cap has to be
/// enforced with nobody connected — which no inbound byte would ever wake us for. 0.5 Hz of a
/// `stat()` is far below the 2 Hz this loop already runs at while subscribed.
const LOG_IDLE_POLL_MS: libc::c_int = 2000;

/// One followed file. Only `LOG_SRC_BOX` is ever written to (rotated at the cap); the rest are
/// read-only tails owned by the supervisor.
struct LogSource {
    id: u8,
    path: &'static str,
    /// Held open across ticks while streaming. `O_NONBLOCK`, so no read here can park the dispatch
    /// loop; `O_RDWR` for the staged source only, because the cap `ftruncate`s this same fd.
    fd: Option<File>,
    /// Inode behind `fd`, so a file REPLACED under us (rather than truncated in place) is detected
    /// — otherwise the fd keeps the old inode alive and the source silently reads EOF forever.
    ino: u64,
    /// Offset up to which this file has been emitted. Streaming starts at 0 on enable.
    off: u64,
    /// File offset that already existed the moment this source was (re)opened at `off == 0`.
    /// Entries whose bytes fall at or before this offset are replayed history, not something
    /// that happened while the host was watching — see [`p::LOG_F_BACKFILL`]. 0 means "none": a
    /// source that was empty at open time, or has not been opened since the last reset.
    backfill_until: u64,
    /// Tail of a line with no `\n` yet, carried to the next tick. Capped at `LOG_MAX_LINE`.
    partial: Vec<u8>,
    /// Set once `partial` hit the cap and the rest of that line is being discarded, so the entry
    /// that eventually carries it is marked `LOG_F_TRUNCATED`.
    partial_clipped: bool,
    /// Lines of THIS source lost to [`LOG_QUEUE_CAP`] since its last `LOG_F_DROPPED` report.
    dropped: u32,
    /// Latched so a failing source reports ONCE per failure run, not once per tick. Per source, not
    /// global: several of these files legitimately never exist (aa-bridge on a CarPlay-only box), and
    /// a shared latch that any other source's successful read cleared would print on every tick — into
    /// `box.log`, which is itself being tailed, so the error would feed itself lines to fail on.
    err_logged: bool,
}

impl LogSource {
    fn reset(&mut self) {
        self.fd = None;
        self.ino = 0;
        self.off = 0;
        self.backfill_until = 0;
        self.partial = Vec::new();
        self.partial_clipped = false;
        self.dropped = 0;
        self.err_logged = false;
    }
}

/// Tailer state for the CH_LOG stream. Grouped instead of spread across `Daemon`'s flat field list
/// because it is one self-contained machine whose `Default` IS the off state — which is also the
/// state a `CT_STOP` or a lost host must return it to.
struct LogTail {
    /// Armed by `CT_LOG_CTL`. Off by default; reset to off when the host goes away.
    enabled: bool,
    /// Rotation cap in bytes, applied to `LOG_SRC_BOX` only. 0 means "the host did not choose".
    cap: u64,
    srcs: Vec<LogSource>,
    /// Round-robin cursor: which source is polled first this tick, and which one gets this tick's
    /// single identity check. Advanced by one per tick so one chatty file cannot starve the others.
    rr: usize,
    /// Per-channel entry counter, incremented per entry and allowed to wrap. Advisory ordering
    /// only; deliberately NOT reset on re-enable, so a host can tell a re-arm from a gap.
    seq: u16,
    /// Drops attributed to the tailer itself (its own notes, or an entry with an unknown source).
    dropped_internal: u32,
    /// Encoded entries not yet packed into frames.
    pending: Vec<u8>,
    last_check: Option<Instant>,
}

impl Default for LogTail {
    fn default() -> Self {
        LogTail {
            enabled: false,
            cap: 0,
            srcs: LOG_SOURCES
                .iter()
                .map(|&(id, path)| LogSource {
                    id,
                    path,
                    fd: None,
                    ino: 0,
                    off: 0,
                    backfill_until: 0,
                    partial: Vec::new(),
                    partial_clipped: false,
                    dropped: 0,
                    err_logged: false,
                })
                .collect(),
            rr: 0,
            seq: 0,
            dropped_internal: 0,
            pending: Vec::new(),
            last_check: None,
        }
    }
}

impl LogTail {
    /// Log lines dropped at [`LOG_QUEUE_CAP`] and not yet reported to the host — per source plus the
    /// tailer's own. Each counter zeroes when its `LOG_F_DROPPED` report goes out, so this is the
    /// currently-unreported gap, not a lifetime total.
    fn dropped_pending(&self) -> u64 {
        self.srcs.iter().map(|s| s.dropped as u64).sum::<u64>() + self.dropped_internal as u64
    }

    /// The effective rotation cap: what the host asked for, or the protocol default when it sent 0.
    fn cap_bytes(&self) -> u64 {
        if self.cap == 0 {
            p::LOG_CAP_DEFAULT_KB as u64 * 1024
        } else {
            self.cap
        }
    }
}

/// Wall-clock milliseconds for an entry stamp. The box has no RTC battery, so this is bogus until
/// the host's `CT_SETTIME` lands; 0 on a clock before the epoch. Stamps are the host's problem to
/// interpret, never the box's to gate on.
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// How many source lines a CH_LOG frame payload stands for, PER SOURCE. A drop report counts the
/// lines it reports, so re-dropping one folds its count forward instead of losing it.
fn count_log_entries(payload: &[u8]) -> Vec<(u8, u32)> {
    let mut per: Vec<(u8, u32)> = Vec::new();
    let mut off = 0;
    while let Some((e, used)) = p::decode_log_entry(&payload[off..]) {
        let n = e.dropped_count().unwrap_or(1);
        match per.iter_mut().find(|(s, _)| *s == e.source) {
            Some((_, c)) => *c = c.saturating_add(n),
            None => per.push((e.source, n)),
        }
        off += used;
    }
    per
}

/// Split `chunk` into complete lines, carrying an unterminated tail in `partial` for the next call
/// and invoking `emit(line, truncated)` per complete line.
///
/// `partial` is capped at `LOG_MAX_LINE` with the overflow discarded (`clipped`), because the carry
/// is the one buffer here a WRITER controls the size of: a daemon dumping a megabyte with no newline
/// would otherwise grow it without bound, which is exactly the RAM the cap exists to protect.
/// `start_off` is the file offset of `chunk[0]`, so `emit`'s third argument — the file offset
/// just past the newline that completed this line — lets the caller tell backfilled history from
/// bytes that landed in the file during this read (see `LogSource::backfill_until`).
fn split_log_lines(
    chunk: &[u8],
    start_off: u64,
    partial: &mut Vec<u8>,
    clipped: &mut bool,
    mut emit: impl FnMut(&[u8], bool, u64),
) {
    let mut consumed = 0u64;
    for piece in chunk.split_inclusive(|&b| b == b'\n') {
        consumed += piece.len() as u64;
        let complete = piece.last() == Some(&b'\n');
        let mut body = if complete { &piece[..piece.len() - 1] } else { piece };
        // Strip a CRLF's \r so it doesn't become a control character inside every entry.
        if complete && body.last() == Some(&b'\r') {
            body = &body[..body.len() - 1];
        }
        let room = p::LOG_MAX_LINE - partial.len();
        if body.len() > room {
            partial.extend_from_slice(&body[..room]);
            *clipped = true;
        } else {
            partial.extend_from_slice(body);
        }
        if complete {
            emit(partial, *clipped, start_off + consumed);
            partial.clear();
            *clipped = false;
        }
    }
}

/// Parse a write-time-stamped line: `@<unix_ms> <rest>` (ASCII digits then one space). Returns
/// the stamp and the remainder with the prefix stripped, or `None` if `line` does not open with
/// the convention — those lines keep the read-time stamp the tailer would otherwise apply.
fn parse_log_stamp(line: &[u8]) -> Option<(u64, &[u8])> {
    let rest = line.strip_prefix(b"@")?;
    let sp = rest.iter().position(|&b| b == b' ')?;
    let (digits, tail) = rest.split_at(sp);
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let ms: u64 = std::str::from_utf8(digits).ok()?.parse().ok()?;
    Some((ms, &tail[1..]))
}

/// Connect to an `ip:port` target with a bounded deadline, then set the socket non-blocking so no
/// subsequent read/write on it can stall the single-threaded poll loop (#789/#834/#846). A blocking
/// connect to a dead/slow airplayd seam or an unreachable CH_IP target would otherwise wedge the whole
/// daemon (and its OCBM console) for the OS default connect timeout.
///
/// Literal addresses ONLY. `to_socket_addrs` on a hostname calls `getaddrinfo`, which has no
/// deadline of its own and would block this thread past `timeout` — the ceiling above bounds the
/// connect, not the resolve. Every in-tree target is a loopback literal (see `IP_OPEN` in
/// ocbm-proto), so this only refuses a host asking the box to resolve for it.
fn connect_seam(target: &str, timeout: Duration) -> Option<TcpStream> {
    let addr = target.parse::<SocketAddr>().ok()?;
    let s = TcpStream::connect_timeout(&addr, timeout).ok()?;
    s.set_nonblocking(true).ok()?;
    Some(s)
}

/// One `pair_answer` request line for carplay-wireless's control port, newline-terminated because
/// the server reads it with `read_line` and would otherwise block until its 5 s client timeout.
fn pair_answer_request(accept: bool) -> String {
    format!("{{\"cmd\":\"pair_answer\",\"accept\":{accept}}}\n")
}

/// The cross-process host-presence flag rx_connect/airplayd read to gate the session (docs/carplay/02_SESSION_LIFECYCLE.md).
///
/// The `cfg(test)` alias is not cosmetic: the presence/teardown tests really write and delete these
/// three files, and under the real names a `cargo test` on the box would tear down a live session.
#[cfg(not(test))]
const HOST_PRESENT_FLAG: &str = "/tmp/host_present";
#[cfg(test)]
const HOST_PRESENT_FLAG: &str = "/tmp/ocbmd_selftest_host_present";

/// The wireless SSP Numeric-Comparison code the ssp_agent publishes during pairing (absent = none).
const PAIRING_CODE_FILE: &str = "/tmp/pairing_code";
/// carplay-wireless's loopback JSON control port (`control::CONTROL_PORT`), where the host's
/// `CT_PAIR_CONFIRM` answer is delivered as `{"cmd":"pair_answer","accept":…}`. Loopback literal —
/// `connect_seam` refuses anything it would have to resolve.
///
/// The `cfg(test)` port is not cosmetic (same reasoning as `HOST_PRESENT_FLAG`): the forwarding test
/// binds a listener and asserts the bytes, and on the real port a `cargo test` run on the box would
/// answer a LIVE pairing prompt.
#[cfg(not(test))]
const WIRELESS_CONTROL_ADDR: &str = "127.0.0.1:9115";
#[cfg(test)]
const WIRELESS_CONTROL_ADDR: &str = "127.0.0.1:19115";
/// Written by `wireless::bt_driver::publish_bt_phase` on every iAP2 handshake transition.
const BT_PHASE_FILE: &str = "/tmp/bt_phase";

/// Phone-on-bus flag written (atomically) by session_supervisor.sh while a host is present; ocbmd
/// mirrors transitions to the host as SEV_PHONE_PRESENT/ABSENT (truthful "waiting for phone").
const PHONE_PRESENT_FLAG: &str = "/tmp/phone_present";

/// Ephemeral landing spot for the host-pushed `VehicleConfig` YAML (task #5 / docs/carplay/04_CAPABILITIES_AND_CONFIG.md). airplayd reads
/// this per control connection to build `/info`. It is written on SUBSCRIBE and removed on STOP /
/// heartbeat-loss / startup, so a config NEVER outlives its session (host-authoritative / ephemeral).
#[cfg(not(test))]
const CARPLAY_CFG_FILE: &str = "/tmp/carplay_cfg.yaml";
#[cfg(test)]
const CARPLAY_CFG_FILE: &str = "/tmp/ocbmd_selftest_cfg.yaml";

// --- CH_MGMT ("CCPA" tab) helpers. Dependency-free by design: direct /sys+/proc reads, no serde_json. ---
/// Persistent BR/EDR bond store (mirrors ssp_agent's `LINK_KEY_STORE`); 25-byte records, bdaddr first.
const BT_LINK_KEY_STORE: &str = "/etc/carplay/bt_link_keys";
/// Flag the supervisor watches to bounce carplay-wireless (restart-wireless / forget-device reload).
const WIRELESS_RESTART_FLAG: &str = "/tmp/wireless_restart";
// App-commanded radio inhibit (docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating): present = radios must be OFF now.
// Written/cleared ONLY from host CT_RADIO commands and the session lifecycle below (go_idle /
// fresh SUBSCRIBE / daemon startup) — this is an app-commanded surface, not an on-box lever.
// The supervisor polls it at 1 Hz alongside /tmp/host_present.
#[cfg(not(test))]
const RADIO_OFF_FLAG: &str = "/tmp/radio_off";
#[cfg(test)]
const RADIO_OFF_FLAG: &str = "/tmp/ocbmd_selftest_radio_off";

/// Read a file and trim it, or "" on any error (for the info snapshot's small /sys+/proc reads).
fn read_trim(path: &str) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Which of `names` (process-exe basenames) are running — one scan of /proc/<pid>/exe (no `pgrep` spawn).
fn running_procs(names: &[&str]) -> Vec<bool> {
    let mut found = vec![false; names.len()];
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for ent in rd.flatten() {
            if let Ok(target) = std::fs::read_link(ent.path().join("exe")) {
                if let Some(base) = target.file_name().and_then(|s| s.to_str()) {
                    for (i, n) in names.iter().enumerate() {
                        if base == *n {
                            found[i] = true;
                        }
                    }
                }
            }
        }
    }
    found
}

/// Rootfs `/` (total_kb, free_kb) via statvfs (no `df` spawn).
fn rootfs_stats_kb() -> (u64, u64) {
    unsafe {
        let mut s: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c"/".as_ptr(), &mut s) == 0 {
            let frsize = s.f_frsize as u64;
            return (
                s.f_blocks as u64 * frsize / 1024,
                s.f_bfree as u64 * frsize / 1024,
            );
        }
    }
    (0, 0)
}

/// Is Simple Pairing mode on? No /sys mirror exists, so parse `hciconfig hci0 sspmode` (one spawn).
///
/// Reads stdout to EOF rather than using `.output()`, and deliberately never waits. `main()` sets
/// `SIGCHLD = SIG_IGN` so the CONSOLE root shell auto-reaps; under that disposition Linux reaps every
/// child itself and `waitpid()` returns ECHILD. `.output()` waits, so it returned `Err` on every call
/// and `.unwrap_or(false)` reported SSP disabled on boxes where `hciconfig hci0 sspmode` says Enabled
/// — MGMT_GET_INFO's box-info was wrong for the whole life of that code. This is the same ECHILD trap
/// `wireless/src/av.rs:150` documents (#63) and avoids there by double-forking instead of ignoring
/// SIGCHLD. Fixed 2026-07-29. Nothing here needs the exit status; EOF on the pipe means the child is
/// gone either way, and SIG_IGN guarantees it leaves no zombie.
fn ssp_enabled() -> bool {
    let mut child = match std::process::Command::new("hciconfig")
        .args(["hci0", "sspmode"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    // Bounded read (was read_to_string to EOF with no deadline — a wedged hciconfig froze the whole
    // daemon; this now runs on the `bt_probe` thread, but the bound is still what keeps the SSP
    // sample from aging out). Still deliberately NO waitpid anywhere: SIGCHLD is SIG_IGN
    // so waitpid() returns ECHILD and the kernel auto-reaps; a killed or exited child leaves no
    // zombie either way. Poll the pipe with a 2 s overall deadline; on timeout or a hard error,
    // kill the child and report false — EOF on the pipe still means the child is gone.
    let pipe = match child.stdout.take() {
        Some(p) => p,
        None => return false,
    };
    let fd = pipe.as_raw_fd();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut out: Vec<u8> = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let _ = child.kill();
            return false;
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = remaining.as_millis().max(1) as libc::c_int;
        let r = unsafe { libc::poll(&mut pfd, 1, ms) };
        if r == 0 {
            let _ = child.kill(); // timed out — don't let one sample stall the probe thread
            return false;
        }
        if r < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue; // EINTR — retry with the remaining budget
            }
            let _ = child.kill();
            return false;
        }
        let mut tmp = [0u8; 128];
        let n = unsafe { libc::read(fd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len()) };
        if n == 0 {
            break; // EOF: the child is gone (SIG_IGN guarantees it leaves no zombie)
        } else if n > 0 {
            out.extend_from_slice(&tmp[..n as usize]);
        } else {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            let _ = child.kill();
            return false;
        }
    }
    String::from_utf8_lossy(&out).contains("Enabled")
}

/// `ssp_enabled()` behind a short-TTL cache. Every SSP sample is host-REQUESTED — SUBSCRIBE and
/// MGMT_GET_INFO (the app polling the CCPA tab) both set [`BtProbe::request_ssp`] — so an app sitting
/// on that tab would otherwise fork `hciconfig` every 2 s for the life of the session. SSP mode is set
/// once at Bluetooth bring-up and effectively never changes at runtime, so a 30 s TTL bounds the spawn
/// to at most once per 30 s while still picking up a wireless restart within that window (audit Fix #4).
/// Monotonic `Instant` (not the box's arbitrary RTC) drives the TTL. The `bt_probe` thread is the only
/// caller (it was the dispatch loop, which is the hang this moved off it), so holding the mutex across
/// the spawn is uncontended; if a second caller is ever added, compute `ssp_enabled()` outside the lock.
fn ssp_enabled_cached() -> bool {
    static CACHE: std::sync::Mutex<Option<(bool, Instant)>> = std::sync::Mutex::new(None);
    const TTL: Duration = Duration::from_secs(30);
    let mut g = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((v, t)) = *g {
        if t.elapsed() < TTL {
            return v;
        }
    }
    let v = ssp_enabled();
    *g = Some((v, Instant::now()));
    v
}

/// Is `hci0` present AND powered up?
///
/// `HCIGETDEVINFO` on a raw HCI socket — the same ioctl `hciconfig` uses to print `UP RUNNING`.
///
/// # Why not sysfs
///
/// The obvious test, `Path::exists("/sys/class/bluetooth/hci0")`, is what this replaced, and it is
/// too weak: the node is created when the controller is REGISTERED and survives `hciconfig hci0
/// down`, which `wireless_down` does deliberately (it leaves the module attached because re-attach
/// is the flaky part). So it could not distinguish a healthy idle radio from one powered down
/// mid-session — both read as "present".
///
/// The next obvious thing, reading `hci0/flags`, DOES NOT WORK HERE and was tried: on this box's
/// 3.14 kernel the node exposes only `address device name power subsystem type uevent`. There is no
/// `flags`, so that read fails and reports "no controller" against an `hci0` that is UP RUNNING —
/// strictly worse than the sysfs-exists test. Verified on hardware 2026-08-29; do not reintroduce it.
///
/// # Cost — and why this MUST NOT run on the dispatch loop
///
/// One `socket()` + one `ioctl()` + one `close()`, no fork. That reads as non-blocking and it is,
/// against a healthy controller. It is NOT against a controller being torn down: `HCIGETDEVINFO`
/// takes the HCI device locks, and while `hci_unregister_dev` holds them the ioctl parks in an
/// UNINTERRUPTIBLE wait on this 3.14 kernel — unkillable, unbounded. Reproduced on the bench while
/// `radio_hal.sh` was recovering a wedged controller (`kill -9 rtk_hciattach`, line discipline
/// closed, `hci0` unregistering): ocbmd stopped reading `/dev/usb_accessory` mid-ioctl and stayed
/// that way for >5 minutes — no HELLO_ACK, no exit, so no respawn — until a power cycle.
///
/// So this is called from ONE place only: the [`BtProbe`] thread. The dispatch loop reads its
/// atomics. Do not "simplify" it back onto the tick.
///
/// A false result covers the case that actually bit us: no controller registered at all, which is
/// what a missing `hci_uart` module produces while every layer above still reports success
/// (`docs/ops/06_CORRECTIONS_LEDGER.md` R-20W-5).
fn hci0_up() -> bool {
    // linux/bluetooth: AF_BLUETOOTH=31, BTPROTO_HCI=1. HCIGETDEVINFO = _IOR('H', 211, int).
    const AF_BLUETOOTH: libc::c_int = 31;
    const BTPROTO_HCI: libc::c_int = 1;
    const HCIGETDEVINFO: libc::c_ulong = 0x8004_48D3;
    const HCI_UP: u32 = 1 << 0;

    // `hci_dev_info`'s prefix, which is all we read: dev_id, name[8], bdaddr[6], flags. The kernel
    // writes the whole struct, so the buffer must be large enough for ALL of it — hence the padding
    // rather than a 4-field struct the ioctl would write past.
    #[repr(C)]
    struct HciDevInfo {
        dev_id: u16,
        name: [u8; 8],
        bdaddr: [u8; 6],
        flags: u32,
        _rest: [u8; 200], // features/pkt_type/mtus/stats; generous, never read
    }

    unsafe {
        let fd = libc::socket(AF_BLUETOOTH, libc::SOCK_RAW, BTPROTO_HCI);
        if fd < 0 {
            return false;
        }
        let mut di: HciDevInfo = std::mem::zeroed();
        di.dev_id = 0; // hci0
        let rc = libc::ioctl(fd, HCIGETDEVINFO as _, &mut di as *mut HciDevInfo);
        libc::close(fd);
        rc >= 0 && (di.flags & HCI_UP) != 0
    }
}

/// Milliseconds on a process-wide monotonic clock.
///
/// The `bt_probe` thread stamps a sample and the dispatch loop ages it, so the two need a shared
/// clock — and an `Instant` cannot live in an atomic. Both sides reduce it to a `u64` against one
/// lazily-fixed base. `+ 1` so a sample taken in the first millisecond of the process still stamps
/// non-zero: 0 is reserved for "the probe has never published", which is permanently stale.
fn mono_ms() -> u64 {
    static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    BASE.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

const BT_HCI_UP: u32 = 1 << 0;
const BT_SSP: u32 = 1 << 1;
/// SSP has been sampled at least once. Distinguishes "SSP is off" from "not asked yet", which the
/// old `Option<bool>` carried and a bare bit would lose.
const BT_SSP_VALID: u32 = 1 << 2;

/// How long a [`BtProbe`] sample may be trusted. Comfortably over the 2 s sample cadence plus the
/// 2 s bounded `hciconfig` read that can share a pass, so a healthy probe never trips it; short
/// enough that a probe wedged in the kernel becomes visible to the host inside one health mirror.
const BT_SAMPLE_MAX_AGE_MS: u64 = 10_000;

fn bt_pack(hci_up: bool, ssp: bool, ssp_valid: bool) -> u32 {
    (if hci_up { BT_HCI_UP } else { 0 })
        | (if ssp { BT_SSP } else { 0 })
        | (if ssp_valid { BT_SSP_VALID } else { 0 })
}

fn bt_unpack(v: u32) -> (bool, bool, bool) {
    (v & BT_HCI_UP != 0, v & BT_SSP != 0, v & BT_SSP_VALID != 0)
}

/// Is a sample stamped at `stamp_ms` still usable at `now_ms`? `stamp_ms == 0` = never published.
fn bt_sample_fresh(now_ms: u64, stamp_ms: u64) -> bool {
    stamp_ms != 0 && now_ms.saturating_sub(stamp_ms) <= BT_SAMPLE_MAX_AGE_MS
}

/// Every Bluetooth probe ocbmd performs, moved off the dispatch loop onto one long-lived thread.
///
/// `hci0_up()` (see its "why this MUST NOT run on the dispatch loop") and `ssp_enabled()` (which
/// forks `hciconfig`) are the only two, and both can park for an unbounded time while the radio
/// seam is recovering a controller. They ran inline on ocbmd's single-threaded `poll()` loop, so
/// one of them blocking stopped USB reads, the heartbeat, the MFi relay and A/V — the box went
/// silent with no exit, hence no respawn.
///
/// The thread samples on its OWN clock and publishes into these atomics; the dispatch loop only
/// ever loads them. If the thread never returns from the kernel the process keeps serving USB and
/// the samples simply go stale, which reads as UNKNOWN (see [`BtProbe::snapshot`]).
///
/// The thread is deliberately never joined on any exit path — it may never be joinable.
/// `std::process::exit` tears it down.
struct BtProbe {
    /// `BT_*` bits from the last published sample.
    state: std::sync::atomic::AtomicU32,
    /// [`mono_ms`] at that sample. 0 = none yet.
    stamp_ms: std::sync::atomic::AtomicU64,
    /// Set by the dispatch loop (SUBSCRIBE, MGMT_GET_INFO), consumed by the thread: sample SSP once.
    ssp_req: std::sync::atomic::AtomicBool,
}

impl BtProbe {
    fn new() -> Self {
        BtProbe {
            state: std::sync::atomic::AtomicU32::new(0),
            stamp_ms: std::sync::atomic::AtomicU64::new(0),
            ssp_req: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Publish one sample: state first, stamp last with `Release`, so a reader that sees a fresh
    /// stamp is guaranteed to see the state that belongs to it.
    fn publish(&self, hci_up: bool, ssp: bool, ssp_valid: bool) {
        use std::sync::atomic::Ordering;
        self.state
            .store(bt_pack(hci_up, ssp, ssp_valid), Ordering::Relaxed);
        self.stamp_ms.store(mono_ms(), Ordering::Release);
    }

    /// `(hci_up, ssp, fresh)`. A stale sample — a wedged or never-started probe thread — reports
    /// both bits false rather than the last value it happened to have: "we do not know" must not
    /// look like "the controller is up", because the host acts on `BH_HCI_PRESENT`.
    fn snapshot(&self) -> (bool, bool, bool) {
        use std::sync::atomic::Ordering;
        let stamp = self.stamp_ms.load(Ordering::Acquire);
        let (hci_up, ssp, ssp_valid) = bt_unpack(self.state.load(Ordering::Relaxed));
        let fresh = bt_sample_fresh(mono_ms(), stamp);
        (fresh && hci_up, fresh && ssp && ssp_valid, fresh)
    }

    /// Ask for a fresh SSP sample (once per session, at SUBSCRIBE; also on MGMT_GET_INFO, which the
    /// app can send with no subscription behind it). Fire-and-forget: it lands within one pass.
    fn request_ssp(&self) {
        self.ssp_req
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Start the `bt_probe` thread and hand back the handle the dispatch loop reads.
///
/// If the spawn itself fails the samples stay at "never published", i.e. permanently stale, which
/// the health mirror and the `probe_stale=1` health line both report — a degraded box, not a wedged
/// one.
fn bt_probe_start() -> std::sync::Arc<BtProbe> {
    let probe = std::sync::Arc::new(BtProbe::new());
    let t = std::sync::Arc::clone(&probe);
    let spawned = std::thread::Builder::new()
        .name("bt_probe".to_string())
        .spawn(move || {
            // SSP is latched across passes: it is set once at BT bring-up and does not change after
            // it, so it is re-read only when the dispatch loop asks.
            let mut ssp = false;
            let mut ssp_valid = false;
            loop {
                let hci_up = hci0_up();
                t.publish(hci_up, ssp, ssp_valid);
                if t.ssp_req.swap(false, std::sync::atomic::Ordering::Relaxed) {
                    // Published separately, and AFTER the hci sample: this one can take up to 2 s
                    // (bounded pipe read), and the hci bit must not be held back behind it.
                    ssp = ssp_enabled_cached();
                    ssp_valid = true;
                    t.publish(hci_up, ssp, ssp_valid);
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });
    if let Err(e) = spawned {
        eprintln!("[ocbmd] bt_probe thread failed to start ({e}) — BT health will read UNKNOWN");
    }
    probe
}

/// Envelope flags for one state-mirror frame: a complete single-frame message, plus `F_REPLAY` when
/// the mirror had no prior value (a fresh `CT_SUBSCRIBE`, or the first read after an `ocbmd` restart)
/// and is therefore telling the host something it may already know rather than reporting a change.
/// `is_none()` — not a "did the host just resubscribe" marker — is the predicate deliberately: the
/// restart case is also a None->Some transition and a resubscribe marker would miss it (audit 3.5).
fn mirror_flags(replay: bool) -> u8 {
    if replay {
        p::F_SOM | p::F_EOM | p::F_REPLAY
    } else {
        p::F_SOM | p::F_EOM
    }
}

/// The advertised accessory name derived like `iap2-core::accessory_name`: `CarLink-<last-4-hex>` of the
/// Wi-Fi MAC (matching the SSID), else the serial; `CarLink` if neither is readable.
fn bt_name_from(wifi_mac: &str, serial: &str) -> String {
    let sfx = |s: &str| -> Option<String> {
        let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        (hex.len() >= 4).then(|| hex[hex.len() - 4..].to_ascii_lowercase())
    };
    match sfx(wifi_mac).or_else(|| sfx(serial)) {
        Some(s) => format!("CarLink-{s}"),
        None => "CarLink".to_string(),
    }
}

/// Bonded BR/EDR devices as uppercase MAC strings. The 25-byte record's bdaddr is little-endian, so it
/// is reversed for display (and to match `forget_one_bond`'s comparison).
fn bonded_macs() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(b) = std::fs::read(BT_LINK_KEY_STORE) {
        for r in b.as_chunks::<25>().0 {
            out.push(format!(
                "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                r[5], r[4], r[3], r[2], r[1], r[0]
            ));
        }
    }
    out
}

/// Remove the bond record for `mac` (display form), atomically rewriting the store. True if the file was
/// read + rewritten (whether or not the mac was present — an absent mac is a no-op success).
fn forget_one_bond(mac: &str) -> bool {
    let want = mac.to_ascii_uppercase();
    let b = match std::fs::read(BT_LINK_KEY_STORE) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let mut out = Vec::new();
    for r in b.as_chunks::<25>().0 {
        let m = format!(
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            r[5], r[4], r[3], r[2], r[1], r[0]
        );
        if m != want {
            out.extend_from_slice(r);
        }
    }
    let tmp = format!("{BT_LINK_KEY_STORE}.tmp");
    std::fs::write(&tmp, &out)
        .and_then(|_| std::fs::rename(&tmp, BT_LINK_KEY_STORE))
        .is_ok()
}

/// Where airplayd persists AirPlay pairings — `[u8 id_len][id][32 B LTPK]` repeated.
const PEER_STORE: &str = "/etc/carplay_peers.bin";

/// Drop every stored AirPlay pairing, so a forgotten phone genuinely re-pairs from scratch.
///
/// Forgetting the BR/EDR bond alone is NOT a fresh pairing: the phone must redo Bluetooth SSP, but
/// its AirPlay long-term key survives here and the next session takes the fast pair-verify path. The
/// box would go on holding a 32-byte key for a device the user asked it to forget.
///
/// **Why the WHOLE store and not just that phone.** The peer store is keyed by the controller's
/// AirPlay pairing identity — the `IDENTIFIER` TLV from pair-setup M5 (`pairing/src/setup.rs:199`)
/// — not by its BR/EDR MAC, and that id never leaves the pairing crate (it is local to
/// `verify.rs:162`). Nothing on the box can answer "which LTPK belongs to this MAC", so a
/// per-device removal would need a MAC->pairing-id map recorded at SETUP. Chosen trade (owner
/// decision, 2026-08-16): clear all. The collateral cost is that OTHER bonded phones redo
/// pair-setup on their next connect — the slow path, not a prompt, since pair-setup is
/// MFi-authenticated and automatic. A slightly longer reconnect once, and nothing the user sees.
///
/// **Why deleting the file is enough.** A running airplayd holds the pairings in memory and
/// `save_peer` persists the WHOLE map, so a survivor would write the deleted keys straight back.
/// It cannot survive: both callers request a wireless restart, and `wireless_down` reaps airplayd
/// (`pkill -f "[a]irplayd"`) whenever the wireless session owns it, so it reloads from the absent
/// file. Absent is also airplayd's normal cold-start case ("peerstore: none ... (fresh)").
fn forget_airplay_peers() {
    match std::fs::remove_file(PEER_STORE) {
        Ok(()) => eprintln!("[ocbmd] mgmt: cleared AirPlay pairings ({PEER_STORE}) — next connect re-pairs"),
        // Already absent is the desired state, not a failure; anything else is worth naming.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("[ocbmd] mgmt: could NOT clear {PEER_STORE}: {e}"),
    }
}

/// Ask the supervisor to bounce carplay-wireless (it watches this flag).
fn request_wireless_restart() {
    let _ = std::fs::write(WIRELESS_RESTART_FLAG, "1");
}

/// Minimal JSON string escaping — used for the two free-form file-sourced values (serial, transport) so
/// a stray `"`/`\`/control char can never malform the info snapshot. The other string fields (MACs,
/// derived name) are provably hex-only. Cheap: returns the input unchanged in the common case.
fn json_escape(s: &str) -> String {
    if !s.bytes().any(|b| b == b'"' || b == b'\\' || b < 0x20) {
        return s.to_string();
    }
    let mut o = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// Land the host's ephemeral YAML config for airplayd to read. Written atomically (`.tmp` + rename) so
/// airplayd never reads a half-written file. An empty config removes the file (fall back to the box
/// default) rather than leaving a zero-byte one.
fn write_cfg_file(bytes: &[u8]) {
    if bytes.is_empty() {
        clear_cfg_file();
        return;
    }
    let tmp = format!("{CARPLAY_CFG_FILE}.tmp");
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, CARPLAY_CFG_FILE);
    }
}

/// Remove the ephemeral config so it can't outlive the session (STOP / heartbeat-loss / startup).
fn clear_cfg_file() {
    let _ = std::fs::remove_file(CARPLAY_CFG_FILE);
}

/// Write the presence flag atomically (`.tmp` + rename) so a concurrent shell `cat` in the supervisor
/// never reads an empty/partial file (which it would misread as "gone" and tear down a live session).
fn write_flag_atomic(path: &str, present: bool) {
    let tmp = format!("{path}.tmp");
    if std::fs::write(&tmp, if present { b"1" } else { b"0" }).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// A byte FIFO drained to the accessory fd with an O(1) read cursor — the consumed prefix is NOT
/// front-shifted per partial USB write (that was O(n²) on a multi-hundred-KB 4K frame; audit M-b). The
/// prefix is reclaimed cheaply when the queue fully drains, or compacted once it grows past a threshold.
#[derive(Default)]
struct OutQueue {
    buf: Vec<u8>,
    cursor: usize, // bytes already written to the fd; live data is buf[cursor..]
}
impl OutQueue {
    /// Bytes still pending (excludes the already-sent prefix).
    fn len(&self) -> usize {
        self.buf.len() - self.cursor
    }
    fn is_empty(&self) -> bool {
        self.cursor >= self.buf.len()
    }
    /// Reclaim the consumed prefix (on full-drain, or once it's large) so `buf` can't grow without
    /// bound across many partial drains. A file pull holds up to ~1 MiB pending here; the 64 KiB
    /// threshold keeps `buf` at pending + at-most-64 KiB rather than pending + everything ever sent.
    fn reclaim(&mut self) {
        if self.cursor >= self.buf.len() {
            self.buf.clear();
            self.cursor = 0;
        } else if self.cursor > 65536 {
            self.buf.drain(0..self.cursor);
            self.cursor = 0;
        }
    }
    /// Frame a message DIRECTLY into the queue tail: header on the stack, payload copied once —
    /// replaces the old frame-into-scratch-then-copy-into-queue double copy.
    fn push_frame(&mut self, ch: u16, flags: u8, seq: u32, payload: &[u8]) {
        self.reclaim();
        // Checked framing (audit Fix #3): the single funnel for every queued frame. An oversized payload
        // would build a header the receiver's Reassembler rejects, silently dropping the frame on this
        // RELIABLE stream (and churning resync) — so drop the whole message LOUDLY here rather than queue
        // a corrupt frame. Callers already cap their reads; this net stops a future uncapped caller from
        // regressing silently.
        if let Err(e) = p::try_frame_into(&mut self.buf, ch, flags, seq, payload) {
            eprintln!("[ocbmd] dropping oversized frame on ch {ch}: {} B > {} B max", e.len, e.max);
        }
    }
    /// Queue a frame whose first `already` bytes were ALREADY written to the fd by a partial vectored write
    /// (opt #1 fast-path spill). Requires an EMPTY queue (the fast path only fires on a frame boundary), so
    /// the frame lands at buf[0..]; `cursor = already` makes `drain_to` resume exactly where writev stopped.
    /// The re-queued header is byte-identical to what writev sent (both via ocbm-proto's header builder).
    fn push_partial(&mut self, ch: u16, flags: u8, seq: u32, payload: &[u8], already: usize) {
        debug_assert!(self.is_empty(), "push_partial on a non-empty queue would corrupt frame order");
        self.push_frame(ch, flags, seq, payload); // reclaim() empties buf → frame at [0..], cursor 0
        self.cursor = already.min(self.buf.len());
    }
    fn clear(&mut self) {
        self.buf.clear();
        self.cursor = 0;
    }
    /// Write as much as the device accepts.
    fn drain_to(&mut self, acc: &mut File) -> Drain {
        let start = self.cursor;
        while self.cursor < self.buf.len() {
            match acc.write(&self.buf[self.cursor..]) {
                Ok(0) => break,
                Ok(w) => self.cursor += w,
                Err(_) => break, // WouldBlock or error: retry next loop pass
            }
        }
        if self.cursor >= self.buf.len() {
            self.buf.clear();
            self.cursor = 0;
            Drain::Done
        } else if self.cursor > start {
            Drain::Partial // rested mid-frame: this queue now owns the wire
        } else {
            Drain::Blocked // wrote nothing: still on a frame boundary
        }
    }
}

/// Outcome of one drain attempt. The `Partial` vs `Blocked` distinction is what makes the
/// frame-ownership tracking in [`Daemon::drain`] precise: only a queue that actually wrote SOME of a
/// frame can be resting mid-frame, so only that case may claim the wire and invert priority.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Drain {
    Done,
    Partial,
    Blocked,
}
impl Drain {
    fn done(self) -> bool {
        self == Drain::Done
    }
}

/// Which output queue is currently resting MID-FRAME on the shared accessory fd.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Wire {
    Hi,
    Audio,
    Video,
    AltVideo,
    Console,
    Log,
    Lo,
}

impl Daemon {
    /// Frame directly into the target priority queue and enqueue (never blocks). The seq is consumed up
    /// front so a capped/resync-dropped frame still advances it, exactly as the old frame-then-drop did.
    fn send(&mut self, ch: u16, flags: u8, payload: &[u8]) {
        let n = p::HDR_LEN + payload.len();
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        // CH_RTSP rides out_hi with the control plane: the SETUP relay is the timing-critical
        // pair/SETUP/RECORD phase, and its whole failure model (3 s answer timeout → local fallback)
        // assumes the transport never queues it behind bulk traffic. Small, reliable, latency-first.
        if matches!(ch, p::CH_CTRL | p::CH_MFI | p::CH_RTSP) {
            // Reliable priority queue. Capped so a stalled host can't OOM the box; on
            // overflow we clear it (pathological — a control frame lost beats the whole daemon OOM-killed).
            // Worst-case resident: CAP + up-to-64 KiB reclaim prefix + one 16 B header — the same bound
            // the A/V queues already accept, so the OOM-backstop reasoning is unaffected.
            if self.out_hi.len() + n <= OUT_QUEUE_CAP {
                self.out_hi.push_frame(ch, flags, seq, payload);
            } else {
                self.out_hi.clear();
                // The queue we may have been mid-frame on just lost its remaining bytes; release
                // ownership or drain() would wait forever for a frame that no longer exists. (The
                // already-written partial tail is unavoidable here — the receiver resyncs on magic.
                // This is the pathological OOM-backstop path, not a normal one.)
                if self.wire_owner == Some(Wire::Hi) {
                    self.wire_owner = None;
                }
                eprintln!("[ocbmd] out_hi cap hit ({OUT_QUEUE_CAP} B) — cleared (host stalled?)");
            }
        } else if ch == p::CH_CONSOLE {
            // CH_CONSOLE has its OWN queue drained AFTER audio/video (audit B3): the root console is
            // independent of projection, so a high-volume console (a left-running `dmesg -w`, a chatty log,
            // a large `cat`) sitting in out_hi would be drained strictly ahead of A/V and freeze 4K video +
            // audio for its duration. Same cap/clear/wire_owner discipline as out_hi; latency-critical
            // control frames (CTRL/MFI/RTSP) stay at top priority above A/V.
            if self.out_console.len() + n <= OUT_QUEUE_CAP {
                self.out_console.push_frame(ch, flags, seq, payload);
            } else {
                self.out_console.clear();
                if self.wire_owner == Some(Wire::Console) {
                    self.wire_owner = None;
                }
                eprintln!("[ocbmd] out_console cap hit ({OUT_QUEUE_CAP} B) — cleared (console flood?)");
            }
        } else if ch == p::CH_LOG {
            // The tailer only frames what it has already checked will fit (it drops the OLDEST
            // pending entries instead — see `log_flush`), so this is a backstop, and it drops the
            // NEWEST frame rather than clearing a queue that may be resting mid-frame on the wire.
            // Clearing here would truncate a half-written frame for a diagnostics channel, which is
            // never worth making the host resync on magic.
            if self.out_log.len() + n <= LOG_QUEUE_CAP {
                self.out_log.push_frame(ch, flags, seq, payload);
            } else {
                for (src, lost) in count_log_entries(payload) {
                    self.log_drop(src, lost);
                }
            }
        } else if matches!(
            ch,
            p::CH_VIDEO | p::CH_ALT_VIDEO | p::CH_MEDIA_AUDIO | p::CH_ALT_AUDIO
        ) {
            // LIVE-UI (VNC) path — CarPlay is the screen NOW, not a replay. BACKPRESSURE, don't drop
            // (task #33 efficiency): the poll loop only pulls the next chunk for a given stream once THAT
            // stream's queue has drained, so a slow USB/host propagates back through the seam → airplayd →
            // the iPhone's screen socket, and the iPhone adapts its encode rate instead of us dropping
            // P-frames (which poisons the decoder until the next IDR). Per-stream queues keep the two
            // video streams independent (audit H1). OUT_QUEUE_CAP is only an OOM backstop, not a drop policy.
            self.drain();
            // FAST PATH (opt #1): on a true frame boundary — the fd is not mid-frame AND this stream's
            // queue is empty (the common case, since each A/V stream is gated to one in-flight frame) —
            // write [header][payload] STRAIGHT to the accessory fd with writev, skipping the copy of the
            // payload into the out-queue. That copy is a ~64 KiB memcpy per chunk at dual 4K@60 and buys
            // nothing here (there is never a second frame to coalesce with). A partial write spills the
            // remainder into the queue + owns the wire, so drain() finishes it exactly as a queued frame;
            // WouldBlock / error / oversize fall through to the queued path unchanged.
            let q_empty = match ch {
                p::CH_VIDEO => self.out_video.is_empty(),
                p::CH_ALT_VIDEO => self.out_alt_video.is_empty(),
                _ => self.out_audio.is_empty(),
            };
            if self.wire_owner.is_none() && q_empty && payload.len() <= p::MAX_PAYLOAD {
                let mut hdr = [0u8; p::HDR_LEN];
                // `write_header` wants the PAYLOAD length; `n` is the TOTAL frame size (HDR_LEN + payload),
                // used for the cap checks. `total` = the bytes write_vectored actually sends = `n`.
                p::write_header(&mut hdr, ch, flags, seq, payload.len());
                let total = n;
                match self.acc.write_vectored(&[IoSlice::new(&hdr), IoSlice::new(payload)]) {
                    Ok(w) if w >= total => {
                        // Full zero-copy write — nothing queued.
                        if self.av_backpressured {
                            eprintln!("[ocbmd] live-A/V backpressure cleared ({} dropped total)", self.av_dropped);
                            self.av_backpressured = false;
                        }
                        return;
                    }
                    Ok(w) if w > 0 => {
                        // Partial: re-queue the whole frame with `w` bytes marked written, own the wire so
                        // drain() resumes THIS queue mid-frame (never interleaves another stream).
                        let wire = match ch {
                            p::CH_VIDEO => { self.out_video.push_partial(ch, flags, seq, payload, w); Wire::Video }
                            p::CH_ALT_VIDEO => { self.out_alt_video.push_partial(ch, flags, seq, payload, w); Wire::AltVideo }
                            _ => { self.out_audio.push_partial(ch, flags, seq, payload, w); Wire::Audio }
                        };
                        self.wire_owner = Some(wire);
                        self.drain(); // try to push the remainder now (matches the queued path's post-drain)
                        return;
                    }
                    _ => {} // Ok(0) / WouldBlock / error → fall through to the queued path below
                }
            }
            let q = match ch {
                p::CH_VIDEO => &mut self.out_video,
                p::CH_ALT_VIDEO => &mut self.out_alt_video,
                _ => &mut self.out_audio, // CH_MEDIA_AUDIO | CH_ALT_AUDIO
            };
            if q.len() + n <= OUT_QUEUE_CAP {
                q.push_frame(ch, flags, seq, payload);
                if self.av_backpressured {
                    // Recovered: the queue is accepting frames again — announce it once (was previously
                    // set true and never cleared, so recovery was never logged; audit LOW).
                    eprintln!(
                        "[ocbmd] live-A/V backpressure cleared ({} dropped total)",
                        self.av_dropped
                    );
                    self.av_backpressured = false;
                }
            } else {
                // Pathological (host wedged, read-gate somehow skipped): drop to avoid OOM.
                self.av_dropped = self.av_dropped.wrapping_add(1);
                if !self.av_backpressured {
                    eprintln!("[ocbmd] live-A/V queue cap hit on ch 0x{ch:04x} — host wedged?");
                    self.av_backpressured = true;
                }
            }
            self.drain();
        } else {
            // Reliable bulk (ECHO/IP/FILE/ETH/METADATA), capped against OOM. On overflow, do NOT drop a
            // single mid-message frame (#567): a bulk message can span several frames (e.g. a large
            // ECHO relayed frame-by-frame with the source SOM/EOM flags, ~line 1079), and dropping one
            // interior frame truncates the receiver's reassembly of that message into corruption.
            // Instead clear the queue (OOM-safe, like out_hi) and RESYNC — drop every subsequent bulk
            // frame until the next F_SOM — so the peer only ever sees whole messages, never an orphaned
            // head or tail.
            if self.lo_resync {
                if flags & p::F_SOM != 0 {
                    self.lo_resync = false; // a fresh message begins; fall through and enqueue it
                } else {
                    self.lo_dropped = self.lo_dropped.wrapping_add(1);
                    return; // still inside the truncated message; keep dropping
                }
            }
            if self.out_lo.len() + n <= OUT_QUEUE_CAP {
                self.out_lo.push_frame(ch, flags, seq, payload);
                if self.lo_capped {
                    eprintln!(
                        "[ocbmd] out_lo cap cleared ({} dropped total)",
                        self.lo_dropped
                    );
                    self.lo_capped = false;
                }
            } else {
                self.out_lo.clear();
                // Release wire ownership if out_lo held it — see the same note on the out_hi cap above.
                if self.wire_owner == Some(Wire::Lo) {
                    self.wire_owner = None;
                }
                // If this overflowing frame is itself a whole message (EOM set), the next frame starts
                // fresh so no resync is needed; otherwise drop the rest of this message up to its EOM.
                self.lo_resync = flags & p::F_EOM == 0;
                self.lo_dropped = self.lo_dropped.wrapping_add(1);
                if !self.lo_capped {
                    eprintln!("[ocbmd] out_lo cap hit ({OUT_QUEUE_CAP} B) — cleared + resyncing (host stalled)");
                    self.lo_capped = true;
                }
                // CH_IP carries the Android Auto TLS stream, where a gap is unrecoverable: the host's
                // decrypt fails, or worse the phone silently stops receiving decryptable data and
                // answers by RESETTING ITS OWN USB GADGET (gearhead's "no data received" reset-repair
                // ladder), dropping out of accessory mode with no protocol teardown. Truncating those
                // frames and carrying on is therefore not survivable for AA — close every relayed
                // stream instead, which both ends already handle as a clean session end.
                self.close_ip_conns_after_gap();
            }
        }
    }

    /// Non-blocking flush in strict priority order. Stops at the first queue that can't fully drain, so a
    /// mid-frame partial tail in a higher queue is never interleaved with a lower queue's frame on the
    /// shared accessory fd (which would truncate the "reliable" frame — receiver resyncs but loses it).
    fn drain(&mut self) {
        // QC 2026-07-25 (HIGH — frame splicing). The old strict-priority-with-early-return protected
        // LOWER queues from a HIGHER queue's partial tail (a higher queue always resumes itself first),
        // but NOT the converse, which is the case that actually happens: a mid-frame rest is the NORMAL
        // state for `out_video` under USB backpressure (multiple partial writes per 4K frame). Once
        // `out_video.drain_to()` rested mid-frame, the very next `drain()` wrote `out_hi` and
        // `out_audio` FIRST — splicing a complete CTRL/audio frame into the middle of the half-written
        // video frame. The receiver's reassembler then swallows the spliced frame inside the video
        // payload (both frames lost) and resyncs on magic; a spliced CTRL frame can silently eat a
        // CT_SESSION_EVENT. Audio pushes are frequent and never gated, so any mid-frame rest with
        // concurrent audio was a splice window.
        //
        // Fix: whichever queue is resting mid-frame OWNS the wire and must be finished to a frame
        // boundary before any other queue writes a byte. Queues only ever contain whole frames
        // (`send` -> `push` appends a complete framed message), so "fully drained" IS a frame boundary.
        // Only a `Partial` result claims ownership — a queue that wrote nothing is still on a boundary,
        // so a merely-blocked low-priority queue can't invert priority.
        if let Some(owner) = self.wire_owner {
            let done = match owner {
                Wire::Hi => self.out_hi.drain_to(&mut self.acc).done(),
                Wire::Audio => self.out_audio.drain_to(&mut self.acc).done(),
                Wire::Video => self.out_video.drain_to(&mut self.acc).done(),
                Wire::AltVideo => self.out_alt_video.drain_to(&mut self.acc).done(),
                Wire::Console => self.out_console.drain_to(&mut self.acc).done(),
                Wire::Log => self.out_log.drain_to(&mut self.acc).done(),
                Wire::Lo => self.out_lo.drain_to(&mut self.acc).done(),
            };
            if !done {
                return; // still mid-frame — nothing else may touch the fd
            }
            self.wire_owner = None; // back on a frame boundary
        }

        // Strict priority. Order within A/V: audio first (low-latency, never behind a video frame),
        // then the two video streams independently.
        macro_rules! step {
            ($e:expr, $w:expr) => {
                match $e {
                    Drain::Done => {}
                    Drain::Partial => {
                        self.wire_owner = Some($w);
                        return;
                    }
                    Drain::Blocked => return,
                }
            };
        }
        step!(self.out_hi.drain_to(&mut self.acc), Wire::Hi);
        step!(self.out_audio.drain_to(&mut self.acc), Wire::Audio);
        // Strict priority: MAIN video (:9001) before the cluster lane (:9005). A fair-share alternation was
        // tried (perf 2026-08-09) to help the cluster, but it moved pacing jitter onto the primary display
        // (main went choppy) for no gain — the app-side decrypt decouple already cures the cluster's
        // starvation, so the box keeps main-first priority. Audio still drains ahead of both.
        step!(self.out_video.drain_to(&mut self.acc), Wire::Video);
        step!(self.out_alt_video.drain_to(&mut self.acc), Wire::AltVideo);
        // CH_CONSOLE below A/V (audit B3) but above the bulk out_lo — interactive enough to beat ECHO/IP/
        // FILE/ETH, never at the expense of live video/audio.
        step!(self.out_console.drain_to(&mut self.acc), Wire::Console);
        // CH_LOG under CONSOLE and over the bulk queue: the log is a diagnostic, so it yields to
        // everything a user is looking at live, but it must not sit behind a 32 MiB file pull —
        // half its value is being able to watch the box while something else is going wrong.
        step!(self.out_log.drain_to(&mut self.acc), Wire::Log);
        step!(self.out_lo.drain_to(&mut self.acc), Wire::Lo);
    }
    /// Bounded wait for the accessory fd to accept bytes — used by the paced flush loops so a
    /// backpressured fd is WAITED on (poll POLLOUT) instead of busy-spun on EAGAIN at 100% CPU.
    /// Capped at 100 ms per call so the caller's own deadline check stays live; errors and POLLERR
    /// just return (the caller's next drain()/deadline handles them).
    fn wait_acc_writable(&self, max: Duration) {
        let mut pfd = libc::pollfd {
            fd: self.acc.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        let ms = max.as_millis().clamp(1, 100) as libc::c_int;
        unsafe { libc::poll(&mut pfd, 1, ms) };
    }

    /// Transition the host-present signal. Mirrors to `/tmp/host_present` (the cross-process flag
    /// rx_connect/airplayd read to gate advertising + teardown) and notifies the host over CH_CTRL.
    /// No-op if unchanged, so it's cheap to call every watchdog tick.
    fn set_present(&mut self, present: bool) {
        if self.present == present {
            return;
        }
        self.present = present;
        write_flag_atomic(HOST_PRESENT_FLAG, present);
        if !present {
            self.present_cleared_at = Some(std::time::Instant::now());
        }
        let sev = if present {
            p::SEV_HOST_PRESENT
        } else {
            p::SEV_HOST_GONE
        };
        self.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_SESSION_EVENT, sev]);
        eprintln!(
            "[ocbmd] session: host {}",
            if present { "PRESENT" } else { "GONE" }
        );
    }

    /// Cycle the supervisor's presence flag WITHOUT telling the host it went away.
    ///
    /// The re-ARM the supervisor needs is a GONE->PRESENT edge on `/tmp/host_present`; the
    /// `CT_SESSION_EVENT` that normally accompanies it is for the HOST, and on the replacement path
    /// the host is the one that just arrived. Sending it a `SEV_HOST_GONE` there is actively wrong:
    /// measured on hardware, the host reads it as "the box dropped us", retires its A/V lanes and
    /// schedules a re-subscribe — so the very cycle meant to bring projection up tore it down again,
    /// once per attempt. The host is left with the `SEV_HOST_PRESENT` the SUBSCRIBE handler already
    /// sends it, which is true and sufficient.
    fn rearm_presence_silently(&mut self) {
        write_flag_atomic(HOST_PRESENT_FLAG, false);
        // The host IS present — only the flag dips, and only long enough to be observable.
        self.present = true;
        self.rearm_deadline = Some(std::time::Instant::now() + REARM_HOLD);
        eprintln!("[ocbmd] session: presence dipped for the supervisor's re-ARM (host not signalled)");
    }

    /// Raise presence for a `CT_SUBSCRIBE` that found the box idle — holding the `/tmp/host_present`
    /// **0** back if the supervisor cannot have sampled it yet.
    ///
    /// The supervisor is edge-triggered on that flag at 1 Hz, so a 0 written and overwritten between
    /// two of its samples never existed as far as it is concerned. Since `CT_STOP` became an
    /// immediate teardown, a scripted quit->relaunch does exactly that: `go_idle` writes 0, the
    /// SUBSCRIBE ~100 ms later writes 1, the supervisor reads 1 -> 1, runs neither `wireless_down`
    /// nor `wireless_up`, and the new host is left subscribed against the DEAD session's airplayd —
    /// the teardown silently skipped. (A human relaunch is far too slow to hit this; a bench script
    /// hits it every time.)
    ///
    /// So when the GONE edge is younger than [`REARM_HOLD`] — the constant that already defines "how
    /// long the flag must read 0 for the supervisor to see it" — leave the flag at 0 and let
    /// `presence_tick` write it at the deadline. Deferral is measured from when the flag WENT to 0,
    /// not from now, so the hold is exactly one `REARM_HOLD` however late the SUBSCRIBE lands.
    ///
    /// Only the cross-process FLAG waits. `present` is true on return, the host gets its
    /// `SEV_HOST_PRESENT` immediately, and `CT_HELLO`/`HELLO_ACK` never touch presence at all — so
    /// nothing the host is waiting on is delayed. `presence_tick` writes `self.present` rather than a
    /// hardcoded true, so a teardown landing inside the hold cancels the raise instead of resurrecting
    /// a host that has gone away again (`go_idle` clears `rearm_deadline` for that reason).
    fn raise_presence(&mut self, now: Instant) {
        let hold_until = self
            .present_cleared_at
            .map(|t| t + REARM_HOLD)
            .filter(|d| *d > now);
        let Some(deadline) = hold_until else {
            self.set_present(true);
            return;
        };
        // Same state change as `set_present(true)` minus the flag write, which `presence_tick` owns
        // until `deadline`. `present` going true here is also what makes a SECOND SUBSCRIBE inside
        // the hold take the caller's `else if self.present` arm instead of this one: the deadline is
        // set once, so the supervisor still sees exactly one 0->1 edge and brings the session up once.
        self.present = true;
        self.rearm_deadline = Some(deadline);
        self.send(
            p::CH_CTRL,
            p::F_SOM | p::F_EOM,
            &[p::CT_SESSION_EVENT, p::SEV_HOST_PRESENT],
        );
        eprintln!(
            "[ocbmd] session: host PRESENT (flag raise held {:?} more so the supervisor sees the GONE edge)",
            deadline.duration_since(now)
        );
    }

    /// Full session teardown to idle: drop presence + subscription + ephemeral cfg and close the
    /// host-side relays (eth bridge, CH_IP sockets, mic/input/RTSP seams). THE host-gone routine —
    /// every way a session can end (`CT_STOP`, heartbeat loss) funnels through it so their teardown
    /// can't drift, and the `/tmp/host_present` 1->0 edge it writes is what makes the supervisor run
    /// its complete wireless teardown back to IDLE. (The transport-death exit paths cannot call it —
    /// the fd is gone — so `clear_session_state_for_exit` replays its pure file effects instead.)
    ///
    /// `notify_host` splits the one thing the two paths must NOT share (audit 3.4). On heartbeat loss
    /// the host may be alive-but-stalled, and `SEV_HOST_GONE` is its only cue to re-subscribe — the
    /// `CT_HEARTBEAT` handler re-presents only while `subscribed`, so suppressing it there would leave
    /// the box silently dropping a live app's beats forever. On a clean `CT_STOP` the host has, by
    /// definition, already detached: the frame is written into the gadget FIFO with no reader, sits
    /// there, and is the FIRST thing the NEXT host reads — observed as HELLO -> HOST_GONE ->
    /// HELLO_ACK within 8 ms, which the app reads as "the box dropped us" on a link that just came
    /// up. `CT_HELLO`'s queue-clear cannot retract it. So: go idle silently.
    fn go_idle(&mut self, notify_host: bool) {
        if notify_host {
            self.set_present(false);
        } else if self.present {
            // Same state change as `set_present(false)` — flag file included, since the supervisor
            // does need the GONE edge — minus the CH_CTRL send.
            self.present = false;
            write_flag_atomic(HOST_PRESENT_FLAG, false);
            self.present_cleared_at = Some(std::time::Instant::now());
            eprintln!("[ocbmd] session: host GONE (not signalled — it sent CT_STOP and left)");
        }
        self.subscribed = false;
        self.cfg.clear();
        // A latched replacement flag must not outlive the session that set it. It is consumed only by
        // the SUBSCRIBE handler, and a host can send HELLO without ever subscribing (a link-only
        // reattach that deliberately withholds the radio-wake edge), so a stale `true` could survive
        // to force an unwanted re-arm on a much later, unrelated SUBSCRIBE.
        self.host_replaced = false;
        // Same reasoning, one deadline over: a pending re-ARM dip must not outlive the session that
        // started it. `presence_tick` restores HOST_PRESENT_FLAG when this elapses, so leaving it set
        // through a teardown would reassert presence for a host we have just declared GONE -- and the
        // supervisor's L2/L3 escalation reads that flag as authoritative. Unreachable with today's
        // constants (REARM_HOLD 2s < HEARTBEAT_GRACE 10s, so the dip always resolves first), but
        // nothing enforces that ordering and raising REARM_HOLD would silently open it — and a
        // CT_STOP now tears down with no grace at all, so the dip can be in flight when it lands.
        self.rearm_deadline = None;
        clear_cfg_file();
        // App loss resets the CT_RADIO inhibit: the next session's pushed config governs radios
        // afresh (docs/carplay/04_CAPABILITIES_AND_CONFIG.md — no stale app commands survive the app).
        let _ = std::fs::remove_file(RADIO_OFF_FLAG);
        // Close the ncm0<->CH_ETH bridge if it was left open (fd/CPU leak across sessions; audit LOW).
        if let Some(fd) = self.eth.take() {
            unsafe { libc::close(fd) };
        }
        // Drop the SETUP-relay seam: the departed host can no longer answer RS_REQs, and airplayd's
        // reader turns our EOF into HostGone → sticky local fallback for any in-flight exchange.
        self.rtsp_sock = None;
        // Drop the mic + input seams too (audit B3): they connect to the departed session's airplayd, and
        // `ensure_mic_seam` only reconnects when the socket is None — a stale seam would otherwise persist
        // until the old airplayd EOFs, delaying the next session's uplink gate. Symmetric with rtsp_sock.
        self.mic_sock = None;
        self.mic_rx.clear();
        self.mic_uplink_on = false; // the host that was gated ON is the one leaving
        self.input_sock = None;
        // Stop streaming the log and drop what was buffered for a host that is no longer there.
        // The file itself keeps being capped — log_tick runs unsubscribed for exactly that reason.
        self.log_reset();
        // Close orphaned CH_IP relay sockets (docs/ops/05_AUDITS.md): a departed host's TCP/UDP relays otherwise
        // persist until the remote closes. No IP_CLOSE is sent — the host is, by definition, gone.
        if !self.conns.is_empty() {
            eprintln!(
                "[ocbmd] session: closing {} orphaned CH_IP relay socket(s)",
                self.conns.len()
            );
            self.conns.clear();
        }
    }

    /// Watchdog: called each poll tick. If subscribed but the last heartbeat is older than the grace,
    /// declare the host gone (crash / stalled transport the backpressure path alone can't distinguish).
    ///
    /// Takes `now` from the caller: `Instant::now()` is a real syscall on this kernel-3.14 box (no
    /// vDSO for clock_gettime on armv7 there), and this runs on EVERY poll wake — thousands/sec
    /// during A/V. One timestamp per dispatch pass, shared with phone_tick/pairing_code_tick.
    fn presence_tick(&mut self, now: Instant) {
        // Restore the flag once the dip has been visible for a full supervisor poll.
        if let Some(d) = self.rearm_deadline {
            if now >= d {
                self.rearm_deadline = None;
                // Mirror `present`, do NOT hardcode true: the flag is a view of daemon state, and a
                // deadline that outlived its session (see go_idle) must not be able to invent one.
                write_flag_atomic(HOST_PRESENT_FLAG, self.present);
                eprintln!(
                    "[ocbmd] session: presence flag restored to {} — supervisor should be re-ARMing",
                    self.present
                );
            }
        }
        if self.subscribed && self.present {
            if let Some(hb) = self.last_hb {
                if now.duration_since(hb) >= HEARTBEAT_GRACE {
                    eprintln!(
                        "[ocbmd] session: heartbeat lost (> {:?}) — host gone",
                        HEARTBEAT_GRACE
                    );
                    // Past grace = the session is over; return fully idle (docs/carplay/02_SESSION_LIFECYCLE.md). Signalled: the
                    // host may still be listening and this is its cue to re-subscribe (audit 3.4).
                    self.go_idle(true);
                }
            }
        }
        self.phone_tick(now);
        self.pairing_code_tick(now);
        self.bt_phase_tick(now);
        self.proj_mode_tick(now);
        self.phone_ident_tick(now);
        self.box_health_tick(now);
        self.log_tick(now);
        self.health_tick(now);
    }

    /// One line describing every queue depth, every drop counter and the last `CT_BOX_HEALTH`
    /// bitmask. Pure formatting — no I/O and no state change — so it is asserted on directly.
    ///
    /// `video` is both video queues summed: the split is a scheduling detail (per-seam gating), and
    /// what this line answers is "is the wire keeping up".
    ///
    /// `probe_stale=1` appears only when the `bt_probe` thread has stopped publishing (wedged in the
    /// kernel, or never started). Without it a wedged probe is invisible: `bh` just loses two bits,
    /// which is indistinguishable from a radio that is genuinely down.
    fn health_line(&self, now: Instant) -> String {
        format!(
            "[ocbmd] health hi={} audio={} video={} console={} log={} lo={} dropped=av:{},lo:{},log:{},input:{} hb_age={} bh={}{}",
            self.out_hi.len(),
            self.out_audio.len(),
            self.out_video.len() + self.out_alt_video.len(),
            self.out_console.len(),
            self.out_log.len(),
            self.out_lo.len(),
            self.av_dropped,
            self.lo_dropped,
            self.log.dropped_pending(),
            self.input_dropped,
            match self.last_hb {
                Some(hb) => format!("{}s", now.duration_since(hb).as_secs()),
                None => "-".to_string(),
            },
            match self.box_health {
                Some(f) => format!("{f:#04x}"),
                None => "-".to_string(),
            },
            if self.bt.snapshot().2 { "" } else { " probe_stale=1" },
        )
    }

    /// Emit [`Daemon::health_line`] once per [`HEALTH_LOG_INTERVAL`] while a host is subscribed.
    ///
    /// Throttled on an `Instant`, not on a tick count: this runs on EVERY poll wake — thousands per
    /// second during A/V — and a per-tick line would be the log flood it exists to diagnose.
    fn health_tick(&mut self, now: Instant) {
        if !self.subscribed {
            // Clear the throttle so the next session's first line is immediate rather than up to
            // 30 s late on a deadline inherited from the previous one.
            self.last_health_log = None;
            return;
        }
        if let Some(prev) = self.last_health_log {
            if now.duration_since(prev) < HEALTH_LOG_INTERVAL {
                return;
            }
        }
        self.last_health_log = Some(now);
        eprintln!("{}", self.health_line(now));
    }

    /// Report the box's own readiness to the host as `CT_BOX_HEALTH`, on change.
    ///
    /// Same discipline as the other mirrors — changes only, throttled, re-emitted to a fresh host —
    /// but unlike them this is not mirroring a file the supervisor wrote. It samples the same sources
    /// `box_info_json` uses, so the two can never disagree, and collapses them to a bitmask because
    /// this can fire during a live A/V session and must stay cheap.
    ///
    /// 2 s, not 500 ms: the inputs are a /proc walk and a statvfs, and none of them change fast. A
    /// daemon dying is worth knowing about within a couple of seconds, not within half of one.
    fn box_health_tick(&mut self, now_t: std::time::Instant) {
        if !self.subscribed {
            return;
        }
        if let Some(prev) = self.last_box_health_check {
            if now_t.duration_since(prev) < Duration::from_secs(2) {
                return;
            }
        }
        self.last_box_health_check = Some(now_t);

        let procs = running_procs(&["iap2d", "airplayd", "carplay-wireless", "hostapd"]);
        let (total_kb, free_kb) = rootfs_stats_kb();
        // 5% or 2 MB, whichever is larger. The box writes its ephemeral session YAML, its logs and
        // its hostapd.conf to rootfs; running it to zero fails a session in ways that look like
        // anything but a full disk.
        let floor_kb = std::cmp::max(total_kb / 20, 2048);
        let mut f = 0u8;
        // Both Bluetooth bits are LOADS from the `bt_probe` thread's atomics — this thread never
        // probes the controller itself. `hci0_up()` blocks uninterruptibly while `hci_unregister_dev`
        // holds the HCI locks and `ssp_enabled()` forks `hciconfig`; either one inline here stops USB,
        // the heartbeat and A/V with no exit and therefore no respawn (see [`BtProbe`]).
        //
        // The bits report UP+RUNNING, not merely "the sysfs node exists" — see [`hci0_up`]. The
        // node-exists test that replaced survived `hciconfig hci0 down` (wireless_down deliberately
        // leaves the module attached), so it could not see a mid-session hci-down at all.
        //
        // A sample older than BT_SAMPLE_MAX_AGE_MS is UNKNOWN and clears both bits: a wedged probe
        // must not keep asserting the last-known-good controller state at the host.
        let (hci_up, ssp, _fresh) = self.bt.snapshot();
        if hci_up {
            f |= p::BH_HCI_PRESENT;
        }
        if ssp {
            f |= p::BH_SSP;
        }
        if procs[0] {
            f |= p::BH_IAP2D;
        }
        if procs[1] {
            f |= p::BH_AIRPLAYD;
        }
        if procs[2] {
            f |= p::BH_CARPLAY_WIRELESS;
        }
        if procs[3] {
            f |= p::BH_WLAN_AP;
        }
        if free_kb >= floor_kb {
            f |= p::BH_ROOTFS_OK;
        }

        if Some(f) != self.box_health {
            let replay = self.box_health.is_none();
            self.box_health = Some(f);
            self.send(p::CH_CTRL, mirror_flags(replay), &[p::CT_BOX_HEALTH, f]);
            eprintln!("[ocbmd] box health -> host: {f:#04x}");
        }
    }

    /// Mirror airplayd's `/tmp/phone_identity` to the host as `CT_PHONE_IDENT`.
    ///
    /// The file is written once per session from the phone's own AirPlay SETUP plist, and carries the
    /// name the user gave the device plus its `deviceID` — the BR/EDR MAC, which is what lets the app
    /// say WHICH bonded phone from `MGMT_INFO` is the live one. Same discipline as the other mirrors:
    /// changes only, throttled once latched, and never forwarded torn (airplayd renames it into
    /// place, so a read either sees the whole document or the previous one).
    fn phone_ident_tick(&mut self, now_t: std::time::Instant) {
        if !self.subscribed {
            return;
        }
        if self.phone_ident.is_some() {
            if let Some(prev) = self.last_phone_ident_check {
                if now_t.duration_since(prev) < Duration::from_millis(500) {
                    return;
                }
            }
        }
        self.last_phone_ident_check = Some(now_t);
        // Absent file => "" (no identity known yet). A torn read cannot happen (atomic rename), but an
        // oversized one is refused rather than framed: this rides one CH_CTRL frame.
        let now = std::fs::read_to_string(PHONE_IDENT_FILE)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if now.len() > 1024 {
            return;
        }
        if Some(&now) != self.phone_ident.as_ref() {
            let replay = self.phone_ident.is_none();
            self.phone_ident = Some(now.clone());
            let mut a = Vec::with_capacity(1 + now.len());
            a.push(p::CT_PHONE_IDENT);
            a.extend_from_slice(now.as_bytes());
            self.send(p::CH_CTRL, mirror_flags(replay), &a);
            eprintln!("[ocbmd] phone identity -> host: {now}");
        }
    }

    /// Mirror the ssp_agent's `/tmp/pairing_code` flag to the host as a `CT_PAIRING_CODE` message on each
    /// change: a non-empty payload is the 6-digit SSP Numeric-Comparison code to DISPLAY for the user to
    /// match against the iPhone; an empty payload clears it (pairing done, or Just-Works never wrote one).
    /// Same throttle/transition discipline as `phone_tick` (the file changes only during pairing).
    fn pairing_code_tick(&mut self, now_t: Instant) {
        if !self.subscribed {
            return;
        }
        if self.pairing_code.is_some() {
            if let Some(prev) = self.last_pairing_check {
                if now_t.duration_since(prev) < Duration::from_millis(500) {
                    return;
                }
            }
        }
        self.last_pairing_check = Some(now_t);
        // Absent file → "" (no code). A present file → its trimmed 6-digit content.
        let now = std::fs::read_to_string(PAIRING_CODE_FILE)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if Some(&now) != self.pairing_code.as_ref() {
            let replay = self.pairing_code.is_none();
            self.pairing_code = Some(now.clone());
            let mut pl = vec![p::CT_PAIRING_CODE];
            pl.extend_from_slice(now.as_bytes()); // empty = clear/hide
            self.send(p::CH_CTRL, mirror_flags(replay), &pl);
            if now.is_empty() {
                eprintln!("[ocbmd] pairing code cleared");
            } else {
                eprintln!("[ocbmd] pairing code -> host: {now}");
            }
        }
    }

    /// Forward the host's `CT_PAIR_CONFIRM` answer to carplay-wireless's control port as one
    /// newline-terminated JSON request. Body split out as [`pair_answer_request`] so the exact wire
    /// text is testable without a socket — the reader on the far side is a `read_line`, so the
    /// trailing newline is load-bearing.
    /// newline-terminated JSON request (`crates/vendor/wireless/src/control.rs`).
    ///
    /// BOUNDED and fire-and-forget, like every other loopback write on this thread: `connect_seam`
    /// caps the connect, a 250 ms `SO_SNDTIMEO` caps the write, and the response is never read. The
    /// poll loop also carries the host heartbeats, so a wedged or absent wireless daemon must cost a
    /// bounded stall, not a torn-down session (the MFi-bridge lesson — see `forward_rtsp`). A failure
    /// is logged and dropped: the SSP agent falls back to its own 55 s deadline and replies NO, which
    /// is the same outcome the user asked for on a Cancel and a safe one on a Pair.
    fn send_pair_answer(&mut self, accept: bool) {
        let verb = if accept { "PAIR" } else { "CANCEL" };
        let Some(mut s) = connect_seam(WIRELESS_CONTROL_ADDR, Duration::from_millis(250)) else {
            eprintln!(
                "[ocbmd] pair confirm: {verb} — carplay-wireless control port {WIRELESS_CONTROL_ADDR} \
                 unreachable (answer dropped; the box will time the prompt out)"
            );
            return;
        };
        let _ = s.set_nonblocking(false); // connect_seam hands back a non-blocking socket
        let _ = s.set_write_timeout(Some(Duration::from_millis(250)));
        match s.write_all(pair_answer_request(accept).as_bytes()) {
            Ok(()) => eprintln!("[ocbmd] pair confirm: {verb} -> carplay-wireless"),
            Err(e) => eprintln!("[ocbmd] pair confirm: {verb} — write failed: {e} (answer dropped)"),
        }
    }

    /// Mirror `/tmp/bt_phase` to the host as `CT_BT_PHASE` on each change.
    ///
    /// Exists because the host is not in the Bluetooth loop at all — the box owns the radio, and
    /// `SEV_PHONE_*` report the box's own USB bus, not BT. Without this a host app has no signal
    /// between `CT_SUBSCRIBE` and the phone appearing on Wi-Fi, which is the longest phase of the
    /// whole session; it could only poll `/tmp/wl.log` over a debug console.
    ///
    /// Same discipline as [`Self::pairing_code_tick`]: changes only, throttled once latched.
    fn bt_phase_tick(&mut self, now_t: std::time::Instant) {
        if !self.subscribed {
            return;
        }
        if self.bt_phase.is_some() {
            if let Some(prev) = self.last_bt_phase_check {
                if now_t.duration_since(prev) < Duration::from_millis(500) {
                    return;
                }
            }
        }
        self.last_bt_phase_check = Some(now_t);
        // Absent or unparseable => IDLE. A torn read must never be forwarded as a phase.
        let now = std::fs::read_to_string(BT_PHASE_FILE)
            .ok()
            .and_then(|s| s.trim().parse::<u8>().ok())
            .unwrap_or(p::BTP_IDLE);
        if Some(now) != self.bt_phase {
            let replay = self.bt_phase.is_none();
            self.bt_phase = Some(now);
            self.send(p::CH_CTRL, mirror_flags(replay), &[p::CT_BT_PHASE, now]);
            eprintln!("[ocbmd] bt phase -> host: {now}");
        }
    }

    /// Stream `/tmp/box.log` to the host on CH_LOG, and keep that file under its cap.
    ///
    /// Deliberately on the existing periodic tick rather than a thread: the same `Instant`-gated
    /// pattern as the `/tmp/bt_phase` and `/tmp/phone_identity` mirrors above, on the one dispatch
    /// thread, so it cannot race the A/V queues or the MFi relay.
    ///
    /// Unlike those mirrors this runs while UNSUBSCRIBED too. The cap is not a streaming concern:
    /// `/tmp` is tmpfs, every box daemon appends to this file from boot, and an idle box with no
    /// host is precisely where it would otherwise grow unwatched.
    fn log_tick(&mut self, now: Instant) {
        let every = if self.log.enabled { LOG_TICK } else { LOG_IDLE_TICK };
        if let Some(prev) = self.log.last_check {
            if now.duration_since(prev) < every {
                return;
            }
        }
        self.log.last_check = Some(now);
        if self.log.enabled {
            self.log_stream();
            self.log_flush();
        } else {
            self.log_rotate_idle();
        }
    }

    /// One streaming pass over every source, then the cap check on the staged one.
    ///
    /// The [`LOG_READ_CHUNK`] budget is shared: pass one gives each source an equal share in
    /// rotating order so a chatty file cannot starve the rest, pass two hands whatever is left to
    /// whoever still has data, so a single active source still gets the whole budget (which is what
    /// makes the enable-time backfill of a 256 KB `box.log` take seconds rather than minutes).
    fn log_stream(&mut self) {
        let n = self.log.srcs.len();
        if n == 0 {
            return;
        }
        let start = self.log.rr % n;
        self.log.rr = self.log.rr.wrapping_add(1);
        self.log_check_identity(start);
        let mut buf = vec![0u8; LOG_READ_CHUNK];
        let mut budget = LOG_READ_CHUNK;
        let mut at_eof = 0u32; // bit per source: read less than asked, so it has nothing more now
        debug_assert!(n <= 32, "at_eof is a u32 bitmask over the source table");
        for share in [LOG_READ_CHUNK.div_ceil(n), LOG_READ_CHUNK] {
            for k in 0..n {
                if budget == 0 {
                    break;
                }
                let i = (start + k) % n;
                if at_eof & (1 << i) != 0 {
                    continue;
                }
                let want = budget.min(share);
                let got = self.log_read_source(i, &mut buf[..want]);
                if got < want {
                    at_eof |= 1 << i;
                }
                budget -= got;
            }
        }
        self.log_enforce_cap();
    }

    /// Read at most `buf.len()` bytes from source `i` and turn the complete lines into entries.
    /// Returns the bytes read (0 on EOF, an absent file, or any error — never blocks, never fails).
    fn log_read_source(&mut self, i: usize, buf: &mut [u8]) -> usize {
        let (id, path) = (self.log.srcs[i].id, self.log.srcs[i].path);
        if self.log.srcs[i].fd.is_none() {
            use std::os::unix::fs::MetadataExt;
            use std::os::unix::fs::OpenOptionsExt;
            let mut o = OpenOptions::new();
            o.read(true).custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
            // Write access on the staged source only, and only so the cap can ftruncate this fd.
            // A tail-only source is opened read-only so a bug here CANNOT touch a file the
            // supervisor parses.
            if id == p::LOG_SRC_BOX {
                o.write(true);
            }
            match o.open(path) {
                Ok(f) => {
                    let md = f.metadata().ok();
                    self.log.srcs[i].ino = md.as_ref().map(|m| m.ino()).unwrap_or(0);
                    // Whatever is already in the file at open time is history, not something
                    // that happens while this pass streams it — see `backfill_until` above.
                    self.log.srcs[i].backfill_until = md.as_ref().map(|m| m.len()).unwrap_or(0);
                    self.log.srcs[i].fd = Some(f);
                    self.log.srcs[i].off = 0;
                }
                // Absent until its daemon runs — normal, and polled for. Reported once, not per tick.
                Err(e) => {
                    self.log_err(i, &format!("open {path}: {e}"));
                    return 0;
                }
            }
        }
        let size = match self.log.srcs[i].fd.as_ref().map(|f| f.metadata()) {
            Some(Ok(m)) => m.len(),
            Some(Err(e)) => {
                self.log.srcs[i].fd = None;
                self.log_err(i, &format!("stat {path}: {e}"));
                return 0;
            }
            None => return 0,
        };
        if size < self.log.srcs[i].off {
            // Shrank under us — `bound_logs` rewrites these files in place, and the staged one can
            // be truncated by anything. Restart at 0 rather than seek past EOF and then silently
            // stream nothing for the rest of the session.
            self.log_restart_source(i, "truncated externally");
        }
        let got = {
            use std::io::{Seek, SeekFrom};
            let off = self.log.srcs[i].off;
            let Some(f) = self.log.srcs[i].fd.as_mut() else { return 0 };
            match f.seek(SeekFrom::Start(off)).and_then(|_| f.read(buf)) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => 0,
                Err(e) => {
                    self.log.srcs[i].fd = None; // reopen next tick
                    self.log_err(i, &format!("read {path}: {e}"));
                    return 0;
                }
            }
        };
        let start_off = self.log.srcs[i].off;
        let backfill_until = self.log.srcs[i].backfill_until;
        self.log.srcs[i].off += got as u64;
        self.log.srcs[i].err_logged = false;
        let read_ms = now_unix_ms();
        {
            // Disjoint field borrows: the splitter owns the carry, the closure owns the output.
            let LogTail { srcs, pending, seq, .. } = &mut self.log;
            let s = &mut srcs[i];
            split_log_lines(
                &buf[..got],
                start_off,
                &mut s.partial,
                &mut s.partial_clipped,
                |line, truncated, end_off| {
                    let mut flags = if truncated { p::LOG_F_TRUNCATED } else { 0 };
                    // History streamed in by this pass, not something that happened live — a
                    // stamped line still gets this: backfill/live is about WHEN it was read, not
                    // who owns the timestamp.
                    if end_off <= backfill_until {
                        flags |= p::LOG_F_BACKFILL;
                    }
                    // `@<unix_ms> ` write-time stamp (docs/carplay/01_OCBM_PROTOCOL.md CH_LOG):
                    // use the writer's own clock instead of read time when present.
                    let (ms, text) = match parse_log_stamp(line) {
                        Some((stamp, rest)) => (stamp, rest),
                        None => (read_ms, line),
                    };
                    p::encode_log_entry(pending, id, flags, *seq, ms, text);
                    *seq = seq.wrapping_add(1);
                },
            );
        }
        got
    }

    /// One path `stat` per tick, rotating: has the file behind this source's fd been REPLACED
    /// (unlinked and recreated) rather than truncated in place? An open fd keeps the old inode
    /// alive, so without this the source would read EOF forever against a live new file. Done for
    /// one source per tick rather than all of them — 11 extra stats on every 250 ms tick would be a
    /// syscall tax on the A/V path for a condition that changes at most once per session.
    fn log_check_identity(&mut self, i: usize) {
        use std::os::unix::fs::MetadataExt;
        // `ino == 0` means the fstat at open failed, so there is nothing to compare against and a
        // check would restart the source on every tick forever.
        if self.log.srcs[i].fd.is_none() || self.log.srcs[i].ino == 0 {
            return;
        }
        let path = self.log.srcs[i].path;
        match std::fs::metadata(path) {
            Ok(m) if m.ino() == self.log.srcs[i].ino => {}
            // Replaced, or reaped and not back yet: let go of the stale inode and restart at 0. A
            // still-absent file just reopens (and fails, silently) on the next tick.
            _ => {
                self.log.srcs[i].fd = None;
                self.log.srcs[i].ino = 0;
                self.log_restart_source(i, "replaced or reaped");
            }
        }
    }

    /// Restart a source at offset 0, telling the host why so a discontinuity is never silent.
    ///
    /// Deliberately leaves `fd`/`ino` alone: the in-place-truncation caller still holds the right
    /// inode, and clearing `ino` there would make the next identity check read the file as replaced
    /// and restart it again, note and all, forever.
    fn log_restart_source(&mut self, i: usize, why: &str) {
        let id = {
            let s = &mut self.log.srcs[i];
            s.partial_clipped = false;
            if s.off == 0 && s.partial.is_empty() {
                return; // nothing had been streamed from it; no discontinuity to explain
            }
            s.off = 0;
            // The truncated-externally caller leaves `fd` open (see `log_read_source`), so
            // whatever is at the front of the file right now is history again — re-stamp it,
            // same as a fresh open. Absent fd (the replaced/reaped caller already cleared it)
            // just leaves the value for the next open to set.
            s.backfill_until = s.fd.as_ref().and_then(|f| f.metadata().ok()).map(|m| m.len()).unwrap_or(0);
            s.partial = Vec::new();
            s.id
        };
        self.log_internal(&format!("{} {why} — restarting from offset 0", p::log_source_name(id)));
    }

    /// Enforce the cap on the STAGED source only. The tail-only sources belong to the supervisor
    /// (`bound_logs` caps them and other processes parse them), so the tailer never truncates them
    /// — doing so would race a `grep` the session lifecycle depends on.
    ///
    /// Rotate only once everything visible has been emitted, so a rotation never eats un-streamed
    /// lines. `runaway` is the escape hatch: a writer sustaining more than the read budget would
    /// keep `off` behind `size` forever, and an unbounded tmpfs file is worse than a reported gap.
    /// Bytes appended between this decision and the `ftruncate` are lost either way — there is no
    /// truncate-if-unchanged syscall, and the window is microseconds against an over-cap file.
    fn log_enforce_cap(&mut self) {
        let Some(i) = self.log.srcs.iter().position(|s| s.id == p::LOG_SRC_BOX) else {
            return;
        };
        let cap = self.log.cap_bytes();
        let size = match self.log.srcs[i].fd.as_ref().map(|f| f.metadata()) {
            Some(Ok(m)) => m.len(),
            _ => return,
        };
        let off = self.log.srcs[i].off;
        if size <= cap || (off < size && size <= cap.saturating_mul(4)) {
            return;
        }
        let unstreamed = size.saturating_sub(off);
        let Some(f) = self.log.srcs[i].fd.as_ref() else { return };
        // O_APPEND writers are unaffected: the kernel recomputes their offset per write, so they
        // keep appending at the (now zero) end instead of leaving a hole full of NULs.
        if unsafe { libc::ftruncate(f.as_raw_fd(), 0) } < 0 {
            let e = std::io::Error::last_os_error();
            return self.log_err(i, &format!("ftruncate {LOG_FILE}: {e}"));
        }
        self.log.srcs[i].off = 0;
        // The file is empty now — nothing is backfill against a fresh 0-byte start, and a stale
        // (pre-rotation) value here would wrongly flag freshly-appended live lines as backfill
        // until `off` grew back past it.
        self.log.srcs[i].backfill_until = 0;
        self.log.srcs[i].partial = Vec::new();
        self.log.srcs[i].partial_clipped = false;
        self.log_internal(&format!("rotated {size} bytes at cap {cap} ({unstreamed} not streamed)"));
    }

    /// CAP, not-streaming arm: nobody is reading the file, so rotate it and leave one marker line
    /// behind saying what was lost. Written with `O_APPEND` like every other writer to this file.
    fn log_rotate_idle(&mut self) {
        for s in self.log.srcs.iter_mut() {
            s.fd = None; // nothing to hold open while disabled
        }
        let Ok(md) = std::fs::metadata(LOG_FILE) else {
            return; // not created yet — normal early in boot
        };
        if md.len() <= self.log.cap_bytes() {
            return;
        }
        let size = md.len();
        let Some(i) = self.log.srcs.iter().position(|s| s.id == p::LOG_SRC_BOX) else {
            return;
        };
        match OpenOptions::new().append(true).open(LOG_FILE) {
            Ok(mut f) => {
                if unsafe { libc::ftruncate(f.as_raw_fd(), 0) } < 0 {
                    let e = std::io::Error::last_os_error();
                    return self.log_err(i, &format!("ftruncate {LOG_FILE}: {e}"));
                }
                let _ = writeln!(f, "[log] rotated {size} bytes (not streamed)");
                self.log.srcs[i].err_logged = false;
            }
            Err(e) => self.log_err(i, &format!("open {LOG_FILE} for rotate: {e}")),
        }
    }

    /// Queue a tailer-generated entry (`LOG_SRC_INTERNAL`) so a host never sees a discontinuity —
    /// a rotation or an external truncation — without being told what caused it.
    fn log_internal(&mut self, msg: &str) {
        let seq = self.log.seq;
        self.log.seq = seq.wrapping_add(1);
        let ms = now_unix_ms();
        p::encode_log_entry(&mut self.log.pending, p::LOG_SRC_INTERNAL, 0, seq, ms, msg.as_bytes());
    }

    /// Report a source's I/O failure ONCE per failure run — see [`LogSource::err_logged`].
    fn log_err(&mut self, i: usize, what: &str) {
        if !self.log.srcs[i].err_logged {
            self.log.srcs[i].err_logged = true;
            eprintln!("[ocbmd] log tailer: {what}");
        }
    }

    /// Pack pending entries into CH_LOG frames (≤ `LOG_MAX_FRAME` of payload each), then enforce
    /// [`LOG_QUEUE_CAP`] by dropping the OLDEST entries still pending.
    ///
    /// Oldest-first because the newest lines are the ones describing whatever is going wrong now;
    /// and the count is folded forward through the drop report itself, so dropping a report never
    /// loses the lines it stood for.
    fn log_flush(&mut self) {
        // Drop reports describe the gap the host is about to see, so they ride the FRONT of the
        // next frame rather than turning up after the lines that followed the gap. One per source
        // with a nonzero count: which file lost lines is the whole point of the report.
        let mut head = Vec::new();
        let ms = now_unix_ms();
        for k in 0..self.log.srcs.len() {
            let n = std::mem::take(&mut self.log.srcs[k].dropped);
            if n > 0 {
                let (id, seq) = (self.log.srcs[k].id, self.log.seq);
                self.log.seq = seq.wrapping_add(1);
                p::encode_log_entry(&mut head, id, p::LOG_F_DROPPED, seq, ms, &n.to_le_bytes());
            }
        }
        let n = std::mem::take(&mut self.log.dropped_internal);
        if n > 0 {
            let seq = self.log.seq;
            self.log.seq = seq.wrapping_add(1);
            p::encode_log_entry(&mut head, p::LOG_SRC_INTERNAL, p::LOG_F_DROPPED, seq, ms, &n.to_le_bytes());
        }
        if !head.is_empty() {
            head.append(&mut self.log.pending);
            self.log.pending = head;
        }
        let mut sent = 0;
        while sent < self.log.pending.len() {
            let mut end = sent;
            while let Some((_, n)) = p::decode_log_entry(&self.log.pending[end..]) {
                if end + n - sent > p::LOG_MAX_FRAME {
                    break;
                }
                end += n;
            }
            if end == sent {
                // Unreachable: an entry is at most LOG_ENTRY_HDR + LOG_MAX_LINE, well under
                // LOG_MAX_FRAME. Reached only if `pending` were corrupt — drop it rather than spin.
                self.log.pending.clear();
                break;
            }
            if self.out_log.len() + p::HDR_LEN + (end - sent) > LOG_QUEUE_CAP {
                break; // no room; the rest stays pending and faces the oldest-drop below
            }
            let payload = self.log.pending[sent..end].to_vec();
            self.send(p::CH_LOG, p::F_SOM | p::F_EOM, &payload);
            sent = end;
        }
        self.log.pending.drain(..sent);
        while self.log.pending.len() + self.out_log.len() > LOG_QUEUE_CAP {
            let Some((e, n)) = p::decode_log_entry(&self.log.pending) else {
                self.log.pending.clear();
                break;
            };
            let (src, lost) = (e.source, e.dropped_count().unwrap_or(1));
            self.log_drop(src, lost);
            self.log.pending.drain(..n);
        }
    }

    /// Attribute `n` lost lines to source `src`. An id with no source (the tailer's own notes, or
    /// anything unrecognised) folds into the internal bucket rather than being silently discarded —
    /// a count that goes missing is exactly what a drop report exists to prevent.
    fn log_drop(&mut self, src: u8, n: u32) {
        match self.log.srcs.iter_mut().find(|s| s.id == src) {
            Some(s) => s.dropped = s.dropped.saturating_add(n),
            None => self.log.dropped_internal = self.log.dropped_internal.saturating_add(n),
        }
    }

    /// Return the tailer to its default OFF state. Per-session, exactly like the cfg and the relay
    /// seams: buffered lines are addressed to a host that is no longer there, and a host that comes
    /// back re-arms with its own `CT_LOG_CTL` (and gets the stream from offset 0 again).
    fn log_reset(&mut self) {
        self.log.enabled = false;
        self.log.pending = Vec::new();
        self.log.dropped_internal = 0;
        for s in self.log.srcs.iter_mut() {
            s.reset();
        }
    }

    /// Mirror the box's single-owner arbitration flag (`/tmp/projection_owner`, docs/androidauto/02_ARBITRATION.md) to the
    /// host as `CT_PROJ_MODE` on each change.
    ///
    /// This is the box telling the app WHICH projection transport it armed — the app cannot see the
    /// USB bus, the AOAP switch or `arm_aa`, so without it "the box is now doing Android Auto"
    /// reached the app only via the hand-set `AA_OCBM` env stand-in. On `PM_WIRED_AA` — and, since
    /// 2026-09-04, `PM_WIRELESS_AA` — the app runs its own AA head-unit engine over CH_IP to
    /// `aa-bridge` instead of the CarPlay decode path. Nothing here distinguishes the two: this tick
    /// is `owner().wire_code()` and `handle_ip` connects to whatever target the host names, so the
    /// wireless transport needed no ocbmd change at all. Both AA transports are served by the same
    /// `aa-bridge` process on the same `127.0.0.1:5277`.
    ///
    /// Reads through [`box_common::flags::owner`], so the legacy CarPlay-only `carplay_transport`
    /// fallback and the token spellings stay defined in ONE place shared with aa-bridge and the
    /// shell supervisor. Same discipline as [`Self::bt_phase_tick`]: changes only, throttled once
    /// latched (the flag changes on arm/teardown, not during A/V).
    fn proj_mode_tick(&mut self, now_t: std::time::Instant) {
        if !self.subscribed {
            return;
        }
        if self.proj_mode.is_some() {
            if let Some(prev) = self.last_proj_mode_check {
                if now_t.duration_since(prev) < Duration::from_millis(500) {
                    return;
                }
            }
        }
        self.last_proj_mode_check = Some(now_t);
        // Absent/garbled flag => None => PM_NONE (idle). `owner()` already treats an unknown token
        // as None, so a torn read degrades to "idle", never to a wrong transport.
        let now = box_common::flags::owner().wire_code();
        if Some(now) != self.proj_mode {
            let replay = self.proj_mode.is_none();
            self.proj_mode = Some(now);
            self.send(p::CH_CTRL, mirror_flags(replay), &[p::CT_PROJ_MODE, now]);
            eprintln!("[ocbmd] projection mode -> host: {now}");
        }
    }

    /// Mirror the supervisor's `/tmp/phone_present` flag to the host as a session event on each
    /// transition (2026-07-12): the app shows a TRUTHFUL "waiting for phone" the moment the box
    /// knows, instead of a fixed 20 s no-A/V watchdog. `phone_state` is None until the first read
    /// after SUBSCRIBE, so a freshly-(re)subscribed host always learns the current state once.
    fn phone_tick(&mut self, now_t: Instant) {
        if !self.subscribed {
            return;
        }
        // Throttle the flag stat (#673): presence_tick runs on EVERY poll wake — thousands/sec during
        // 4K A/V — but /tmp/phone_present only changes on a physical plug/unplug. Re-read at most ~2/s
        // so the hot A/V loop isn't burning thousands of pointless syscalls. Exception: the FIRST read
        // after SUBSCRIBE (which resets `last_phone_check`) is immediate, so the host learns the
        // current state without a half-second lag. Keying the throttle off `last_phone_check` rather
        // than `phone_state` matters: until the supervisor writes the flag every read fails and
        // latches no state, so a `phone_state.is_some()` guard would leave this unthrottled for the
        // whole session on any box where the flag is never written.
        if let Some(prev) = self.last_phone_check {
            if now_t.duration_since(prev) < Duration::from_millis(500) {
                return;
            }
        }
        self.last_phone_check = Some(now_t);
        let now = match std::fs::read_to_string(PHONE_PRESENT_FLAG) {
            Ok(s) => match s.trim() {
                "1" => Some(true),
                "0" => Some(false),
                _ => return, // partial/garbled write — keep last state
            },
            Err(_) => return, // supervisor hasn't evaluated yet — unknown, say nothing
        };
        if now != self.phone_state {
            let replay = self.phone_state.is_none();
            self.phone_state = now;
            let present = now == Some(true);
            let sev = if present {
                p::SEV_PHONE_PRESENT
            } else {
                p::SEV_PHONE_ABSENT
            };
            self.send(p::CH_CTRL, mirror_flags(replay), &[p::CT_SESSION_EVENT, sev]);
            eprintln!(
                "[ocbmd] session: phone {}",
                if present {
                    "PRESENT on bus"
                } else {
                    "ABSENT — waiting for plug"
                }
            );
        }
    }

    fn start_console(&mut self) {
        if self.ptm.is_some() {
            return;
        }
        let master;
        unsafe {
            let m = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            if m < 0 {
                return;
            }
            if libc::grantpt(m) != 0 || libc::unlockpt(m) != 0 {
                libc::close(m);
                return;
            }
            let sn = libc::ptsname(m);
            if sn.is_null() {
                libc::close(m);
                return;
            }
            let slave = CStr::from_ptr(sn).to_owned();
            let pid = libc::fork();
            if pid < 0 {
                libc::close(m);
                return;
            }
            if pid == 0 {
                libc::close(m);
                libc::setsid();
                let s = libc::open(slave.as_ptr(), libc::O_RDWR);
                if s < 0 {
                    libc::_exit(127);
                }
                libc::ioctl(s, libc::TIOCSCTTY as _, 0);
                libc::dup2(s, 0);
                libc::dup2(s, 1);
                libc::dup2(s, 2);
                if s > 2 {
                    libc::close(s);
                }
                let argv = [c"/bin/sh".as_ptr(), c"-l".as_ptr(), std::ptr::null()];
                libc::execv(argv[0], argv.as_ptr());
                libc::_exit(127);
            }
            // Non-blocking master (audit #5): a CH_CONSOLE input write with a full slave line-discipline
            // buffer must NOT block the single-threaded poll loop. With O_NONBLOCK the write returns
            // WouldBlock (overflow dropped) instead of stalling A/V/heartbeat; the Pty read path below is
            // made WouldBlock-aware so a spurious wake can't be mistaken for EOF and drop the console.
            let mfl = libc::fcntl(m, libc::F_GETFL);
            libc::fcntl(m, libc::F_SETFL, mfl | libc::O_NONBLOCK);
            master = m;
        }
        self.ptm = Some(unsafe { File::from_raw_fd(master) });
        self.send(
            p::CH_CONSOLE,
            p::F_SOM | p::F_EOM,
            b"[ocbmd] CONSOLE attached (root)\r\n",
        );
    }

    fn handle_mfi(&mut self, pl: &[u8]) {
        if pl.len() < 3 {
            return;
        }
        let op = pl[0];
        let ilen = ((pl[1] as usize) << 8) | pl[2] as usize;
        // Optional 1-byte correlation tag, appended AFTER the declared payload and echoed on the
        // response. The reply carries no opcode echo and no request id, so a host with two concurrent
        // chip users could only correlate by payload length — which cannot tell two 128-byte
        // SIGNATURES apart, and a late reply to a timed-out sign then answers the wrong digest. The
        // phone rejects that signature and drops the session, presenting as a crypto fault.
        //
        // Additive in both directions: we already ignored bytes past `3 + ilen`, and a host that does
        // not send one simply gets no tag back and keeps its old length-correlation behaviour.
        let tag: Option<u8> =
            if pl.len() == 3 + ilen + p::MFI_TAG_LEN { Some(pl[3 + ilen]) } else { None };
        let start = Instant::now();
        let result: Option<Vec<u8>> = match op {
            0x01 => self.mfi.as_ref().and_then(|m| m.cert()),
            // Upper bound before the chip is touched: `I2cMsg::len` is a u16, so a 65536-byte
            // challenge would wrap it to 0 and hand the coprocessor a zero-length write plus a "go".
            // MFi 2.0C challenges are 20/32-byte digests; 1024 is generous.
            0x02 if ilen > 0 && ilen <= 1024 && 3 + ilen <= pl.len() => {
                self.mfi.as_ref().and_then(|m| m.sign(&pl[3..3 + ilen]))
            }
            _ => {
                let mut resp = vec![0x02, 0, 0];
                if let Some(t) = tag {
                    resp.push(t);
                }
                self.send(p::CH_MFI, p::F_SOM | p::F_EOM, &resp);
                return;
            }
        };
        // Same trap as the CT_SRC bench (see CT_SRC below): a contended MFi request blocks this
        // single-threaded dispatch for up to ~12 s (10 s MfiLock deadline + chip polling) while
        // heartbeats sit unread in the kernel buffer — `presence_tick` would then see a stale
        // `last_hb` and tear down a live session. If we measurably blocked, refresh `last_hb`
        // (the host was alive throughout — we simply could not read its beats).
        if start.elapsed() >= Duration::from_secs(1) && self.last_hb.is_some() {
            self.last_hb = Some(Instant::now());
        }
        match result {
            Some(data) => {
                let mut resp = Vec::with_capacity(3 + data.len() + 1);
                resp.push(0);
                resp.push((data.len() >> 8) as u8);
                resp.push(data.len() as u8);
                resp.extend_from_slice(&data);
                if let Some(t) = tag {
                    resp.push(t);
                }
                self.send(p::CH_MFI, p::F_SOM | p::F_EOM, &resp);
            }
            // Errors carry the tag too — a misattributed FAILED is as damaging as a misattributed
            // signature to a caller that does not retry.
            None => {
                let mut resp = vec![0x01, 0, 0];
                if let Some(t) = tag {
                    resp.push(t);
                }
                self.send(p::CH_MFI, p::F_SOM | p::F_EOM, &resp);
            }
        }
    }

    fn send_ip(&mut self, typ: u8, id: u16, data: &[u8]) {
        let mut pl = Vec::with_capacity(3 + data.len());
        pl.push(typ);
        pl.extend_from_slice(&id.to_le_bytes());
        pl.extend_from_slice(data);
        self.send(p::CH_IP, p::F_SOM | p::F_EOM, &pl);
    }

    /// OCBM_CH_IP stream mux: OPEN (connect to target), DATA (relay), CLOSE.
    /// Tear down every relayed CH_IP stream after the reliable queue dropped frames.
    ///
    /// Called from the `out_lo` cap-clear. Those queues carry the Android Auto TLS stream box->host;
    /// once bytes are gone the stream cannot resynchronise, so continuing to relay is worse than
    /// stopping — the host would decrypt garbage and the phone would see its data dry up. Dropping the
    /// sockets makes both ends observe a normal end-of-stream: the host's AA session ends, aa-bridge
    /// sees EOF, the owner flag clears, and the next mode event rebuilds a clean session.
    fn close_ip_conns_after_gap(&mut self) {
        if self.conns.is_empty() {
            return;
        }
        eprintln!(
            "[ocbmd] out_lo gap — closing {} relayed CH_IP stream(s); a TLS stream cannot survive a gap",
            self.conns.len()
        );
        self.conns.clear();
    }

    fn handle_ip(&mut self, pl: &[u8]) {
        if pl.len() < 3 {
            return;
        }
        let typ = pl[0];
        let id = u16::from_le_bytes([pl[1], pl[2]]);
        let data = &pl[3..];
        match typ {
            p::IP_OPEN => {
                // connect_seam blocks up to 2 s on an unreachable/black-holing host:port; if it measurably
                // stalled, refresh last_hb so the watchdog doesn't miscount that stall as a lapsed heartbeat
                // and tear down a live session (audit B3) — the same guard the MFi / file-pull / CT_SRC
                // blocking paths already use.
                let start = Instant::now();
                let conn = std::str::from_utf8(data)
                    .ok()
                    .and_then(|t| connect_seam(t, Duration::from_secs(2)));
                if start.elapsed() >= Duration::from_secs(1) && self.last_hb.is_some() {
                    self.last_hb = Some(Instant::now());
                }
                match conn {
                    Some(s) => {
                        self.conns.insert(id, Conn::Tcp(s));
                    }
                    None => self.send_ip(p::IP_CLOSE, id, &[]),
                }
            }
            p::IP_OPEN_UDP => {
                let sock = std::str::from_utf8(data).ok().and_then(|t| {
                    // Literal only, same reason as connect_seam: `connect(&str)` resolves, and
                    // getaddrinfo would block the single-threaded poll loop with no deadline.
                    let addr = t.parse::<SocketAddr>().ok()?;
                    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
                    s.connect(addr).ok()?;
                    s.set_nonblocking(true).ok()?; // poll-loop recv/send must never block
                    Some(s)
                });
                match sock {
                    Some(s) => {
                        self.conns.insert(id, Conn::Udp(s));
                    }
                    None => self.send_ip(p::IP_CLOSE, id, &[]),
                }
            }
            p::IP_DATA => {
                // Non-blocking TCP relay (audit #4): write_all can partial-write then return WouldBlock,
                // silently truncating this "reliable" stream (the old `let _ =` discarded the error). We
                // keep no per-conn out-buffer in the poll loop, so on ANY short/failed write drop the
                // connection and tell the host (IP_CLOSE) — a clean reset + retry beats silent corruption.
                let drop = match self.conns.get_mut(&id) {
                    Some(Conn::Tcp(s)) => s.write_all(data).is_err(),
                    Some(Conn::Udp(s)) => {
                        let _ = s.send(data); // one datagram (loss is expected for UDP)
                        false
                    }
                    None => false,
                };
                if drop {
                    self.conns.remove(&id);
                    self.send_ip(p::IP_CLOSE, id, &[]);
                }
            }
            p::IP_CLOSE => {
                self.conns.remove(&id);
            }
            _ => {}
        }
    }

    /// Relay a CH_INPUT sub-frame (one HID event, e.g. INPUT_TOUCH) to airplayd's local ingest,
    /// length-prefixed (`[len u16 LE][payload]`) so airplayd frames it back out of the TCP stream.
    /// Lazy-connects; drops the socket on ANY failed/incomplete send so the next event reconnects with
    /// clean framing. The `:9110` seam has no magic and no resync, and `write_all` on a non-blocking
    /// socket can put a PARTIAL frame on the wire before surfacing WouldBlock (Linux `send()` accepts
    /// 1..N bytes when only a prefix fits) — keeping the socket after any error risks permanently
    /// desyncing airplayd's framing. A dropped event is harmless (a lost MOVE; DOWN/UP are rare).
    fn forward_input(&mut self, pl: &[u8]) {
        if pl.is_empty() || pl.len() > u16::MAX as usize {
            self.input_dropped = self.input_dropped.wrapping_add(1);
            eprintln!(
                "[ocbmd] input: dropped bad-size event ({} B; {} dropped total)",
                pl.len(),
                self.input_dropped
            );
            return;
        }
        if self.input_sock.is_none() {
            // Bounded, non-blocking connect (#834/#846): a plain blocking connect to a dead/slow
            // airplayd would wedge the whole poll loop. connect_seam also sets the socket non-blocking,
            // which is what the "non-blocking connect+write" contract above always ASSUMED but the old
            // `TcpStream::connect` never actually did.
            match connect_seam(INPUT_INGEST_ADDR, Duration::from_millis(500)) {
                Some(s) => {
                    let _ = s.set_nodelay(true); // low-latency input: don't Nagle-coalesce tiny reports
                    self.input_sock = Some(s);
                    eprintln!("[ocbmd] input: connected to airplayd {INPUT_INGEST_ADDR}");
                }
                None => {
                    // airplayd not listening (idle / no session) — drop the event. Log throttled
                    // (first + every 60th) so a session-less input burst can't flood the log.
                    self.input_dropped = self.input_dropped.wrapping_add(1);
                    if self.input_dropped == 1 || self.input_dropped.is_multiple_of(60) {
                        eprintln!(
                            "[ocbmd] input: airplayd not listening — event dropped ({} dropped total)",
                            self.input_dropped
                        );
                    }
                    return;
                }
            }
        }
        let mut buf = Vec::with_capacity(2 + pl.len());
        buf.extend_from_slice(&(pl.len() as u16).to_le_bytes());
        buf.extend_from_slice(pl);
        match self.input_sock.as_mut().map(|s| s.write_all(&buf)) {
            Some(Ok(())) => {
                self.input_fwd = self.input_fwd.wrapping_add(1);
                if self.input_fwd == 1 || self.input_fwd.is_multiple_of(60) {
                    eprintln!("[ocbmd] input: forwarded {} events", self.input_fwd);
                }
            }
            // ANY incomplete send is seam death — including WouldBlock, which `write_all` can surface
            // AFTER a partial prefix already hit the wire. There is no resync on this framing, so the
            // only safe recovery is a fresh connection (mirrors forward_mic).
            Some(Err(e)) => {
                self.input_dropped = self.input_dropped.wrapping_add(1);
                eprintln!(
                    "[ocbmd] input: relay write failed ({e}; {} B) — dropping socket to resync framing ({} dropped total)",
                    buf.len(),
                    self.input_dropped
                );
                self.input_sock = None; // reconnect on the next event with clean framing
            }
            None => {} // unreachable: the connect above either succeeded or returned
        }
    }

    /// Relay one CH_MIC payload (host mic PCM, S16LE) to airplayd's mic-uplink seam as a length-framed
    /// `mic <len>\n<pcm>` line. Lazy-connects (non-blocking + nodelay so the poll loop never stalls and
    /// tiny 20 ms chunks aren't Nagle-coalesced). A short/errored write drops the socket so the next
    /// chunk reconnects with clean framing — a dropped mic frame is an imperceptible glitch, and never
    /// wedges the daemon. airplayd not listening (idle / no session) → nothing to do.
    /// Ensure the mic seam to airplayd is connected (best-effort, idempotent). Established EAGERLY —
    /// not just when mic PCM flows — because the `uplink on/off` GATE travels back over this same seam,
    /// and the app only starts capturing (i.e. only produces mic PCM) AFTER it receives the gate. A
    /// data-triggered connect would therefore deadlock: no connection → gate never delivered → no capture
    /// → no data → no connection. A refused connect (airplayd idle / no session) is cheap on localhost.
    fn ensure_mic_seam(&mut self) {
        if self.mic_sock.is_some() {
            return;
        }
        // Bounded connect (#846): connect_seam sets a deadline AND non-blocking, so a dead/slow airplayd
        // can't stall the poll loop during bring-up (the old blocking connect could). Readable in the
        // poll loop; writes stay best-effort.
        if let Some(s) = connect_seam(MIC_INGEST_ADDR, Duration::from_millis(500)) {
            let _ = s.set_nodelay(true);
            self.mic_sock = Some(s);
            self.mic_rx.clear();
            eprintln!("[ocbmd] mic: connected to airplayd {MIC_INGEST_ADDR} (back-channel armed)");
        }
    }

    fn forward_mic(&mut self, pl: &[u8]) {
        if pl.is_empty() || pl.len() > (1 << 20) {
            return; // matches the receiver's mic-line cap (1 MiB); a bogus huge frame is dropped
        }
        self.ensure_mic_seam();
        if self.mic_sock.is_none() {
            return; // airplayd not listening (idle / no session) — nothing to do
        }
        let mut buf = Vec::with_capacity(16 + pl.len());
        buf.extend_from_slice(format!("mic {}\n", pl.len()).as_bytes());
        buf.extend_from_slice(pl);
        let ok = self
            .mic_sock
            .as_mut()
            .is_some_and(|s| s.write_all(&buf).is_ok());
        if !ok {
            self.drop_mic_seam("relay write failed — dropping socket, will reconnect");
        } else {
            self.mic_fwd = self.mic_fwd.wrapping_add(1);
            if self.mic_fwd == 1 || self.mic_fwd.is_multiple_of(200) {
                eprintln!(
                    "[ocbmd] mic: forwarded {} PCM chunks ({} B last)",
                    self.mic_fwd,
                    pl.len()
                );
            }
        }
    }

    /// CH_CTRL CT_UPLINK off to the host and clear the gate flag. Shared by the parsed `uplink off`
    /// line and the synthesized OFF on seam loss.
    ///
    /// SEVEN bytes, deliberately unchanged when the ON form grew a codec byte: OFF carries no format
    /// to describe, and it is the one edge an older host must never fail to parse — a missed OFF
    /// leaves a phone-facing microphone capturing after the call ended.
    fn send_uplink_off(&mut self) {
        self.send(
            p::CH_CTRL,
            p::F_SOM | p::F_EOM,
            &[p::CT_UPLINK, 0, 0, 0, 0, 0, 0],
        );
        self.mic_uplink_on = false;
    }

    /// Forget the mic seam socket (`ensure_mic_seam` reconnects on the next tick) and, if the host was
    /// gated ON through this seam, gate it OFF now. The seam's owner (airplayd per session, or
    /// carplay-wireless's SCO module for an HFP call) sends `uplink off` and closes; if our next PCM
    /// write to the closing peer fails first, that line is never drained — so the drop itself is the
    /// OFF edge. The box does not interpret the audio; it only closes the gate it opened.
    fn drop_mic_seam(&mut self, why: &str) {
        self.mic_sock = None;
        self.mic_rx.clear();
        if self.mic_uplink_on {
            self.send_uplink_off();
            eprintln!("[ocbmd] mic: seam dropped while uplink ON ({why}) — gated host OFF");
        } else {
            eprintln!("[ocbmd] mic: {why}");
        }
    }

    /// Drain the mic seam's readable back-channel (`uplink on <rate> <ch>` / `uplink off`, newline-framed)
    /// and re-emit each transition to the host as CH_CTRL CT_UPLINK so the app starts/stops mic capture on
    /// the real type-100 `input` SETUP edge. Called from the poll loop on a Mic POLLIN. A read EOF/error
    /// drops the socket (airplayd is per-session; it reconnects on the next CH_MIC chunk).
    fn drain_mic_backchannel(&mut self) {
        let mut tmp = [0u8; 512];
        let n = match self.mic_sock.as_mut() {
            Some(s) => match s.read(&mut tmp) {
                Ok(0) => {
                    self.drop_mic_seam("peer closed the seam");
                    return;
                }
                Ok(n) => n,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(e) => {
                    self.drop_mic_seam(&format!("read error ({e})"));
                    return;
                }
            },
            None => return,
        };
        self.mic_rx.extend_from_slice(&tmp[..n]);
        // Cap the partial-line buffer so a peer that never sends a newline can't grow it unbounded.
        if self.mic_rx.len() > 4096 {
            self.mic_rx.clear();
            return;
        }
        while let Some(nl) = self.mic_rx.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.mic_rx.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            self.on_mic_backchannel_line(line);
        }
    }

    /// One back-channel line. Split out of [`Self::drain_mic_backchannel`] so the ON/OFF wire form
    /// is testable without a socket — the payload the host sees is the whole contract here.
    fn on_mic_backchannel_line(&mut self, line: &str) {
        if let Some(rest) = line.strip_prefix("uplink on") {
            // `uplink on <rate> <ch> [codec]` — default 16 kHz mono if the fields are missing or
            // garbled. The fourth token is OPTIONAL and additive (added 2026-09-04 for HFP
            // wideband): airplayd never sends it and CVSD/HFP does not either, so its absence must
            // keep meaning PCM rather than becoming a parse failure that gates the mic off.
            let mut it = rest.split_whitespace();
            let rate: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(16000);
            let ch: u8 = it.next().and_then(|s| s.parse().ok()).unwrap_or(1);
            // An UNKNOWN fourth token is PCM, not a guess: a host told "codec 0" plays what it
            // captures, while a host told some codec it does not have would stop capturing.
            let codec: u8 = match it.next() {
                Some("msbc") => p::SEAM_CODEC_MSBC,
                _ => 0,
            };
            let mut pl = Vec::with_capacity(8);
            pl.push(p::CT_UPLINK);
            pl.push(1); // on
            pl.extend_from_slice(&rate.to_le_bytes());
            pl.push(ch);
            pl.push(codec);
            self.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &pl);
            self.mic_uplink_on = true;
            eprintln!("[ocbmd] mic: uplink ON {rate}Hz {ch}ch codec {codec} -> host");
        } else if line == "uplink off" {
            self.send_uplink_off();
            eprintln!("[ocbmd] mic: uplink OFF -> host");
        }
    }

    /// Ensure the SETUP-relay seam to airplayd is connected (best-effort, idempotent — the
    /// `ensure_mic_seam` clone). Established EAGERLY while a host is subscribed, not on first byte:
    /// airplayd's `RemoteSession` emits RS_OPEN at pair-verify — BEFORE any host→box bytes exist that
    /// could lazily trigger a connect — and its per-connection delegate selection gates on
    /// `relay::seam_up()`, which is only true once we are attached. A data-triggered connect would
    /// therefore permanently select the plain local session. A refused connect (airplayd restarting)
    /// is cheap on localhost; the ≤500 ms subscribed poll cadence throttles the retry.
    fn ensure_rtsp_seam(&mut self) {
        if self.rtsp_sock.is_some() {
            return;
        }
        if let Some(s) = connect_seam(RTSP_INGEST_ADDR, Duration::from_millis(500)) {
            // connect_seam hands back a NON-blocking socket (its poll-loop contract). Flip it back to
            // blocking with a 250 ms SO_SNDTIMEO instead: forward_rtsp must deliver a WHOLE RS_RESP —
            // a partial nonblocking write would desync the byte stream mid-message — but the write
            // must stay BOUNDED so a wedged airplayd can never starve the single-threaded poll loop
            // (and with it the host heartbeats: the MFi-bridge lesson — an unbounded box-side wait
            // once stalled dispatch past HEARTBEAT_GRACE and tore down a healthy session). The small
            // SO_RCVTIMEO bounds the read side against a spurious level-triggered POLLIN wake.
            let _ = s.set_nonblocking(false);
            let _ = s.set_write_timeout(Some(Duration::from_millis(250)));
            let _ = s.set_read_timeout(Some(Duration::from_millis(50)));
            let _ = s.set_nodelay(true); // rpc frames are small and latency-critical
            self.rtsp_sock = Some(s);
            eprintln!("[ocbmd] rtsp: connected to airplayd {RTSP_INGEST_ADDR} (SETUP relay armed)");
        }
    }

    /// Relay one CH_RTSP payload (host→box: RS_RESP / RS_ERR seam bytes) to airplayd's relay seam.
    /// Any write failure — including the 250 ms SO_SNDTIMEO expiring — DROPS the socket: recovery is
    /// by POLICY, not by retry. airplayd sees EOF on its reader, marks the seam down, fails every
    /// pending exchange fast (HostGone → sticky local fallback), and the next `ensure_rtsp_seam`
    /// tick reconnects fresh — so a wedged relay costs one bounded stall and one fallen-back
    /// exchange, never a starved heartbeat or a desynced half-written message.
    fn forward_rtsp(&mut self, pl: &[u8]) {
        if pl.is_empty() {
            return;
        }
        self.ensure_rtsp_seam();
        let Some(s) = self.rtsp_sock.as_mut() else {
            return; // airplayd not listening — the box side answers locally, nothing to do
        };
        if let Err(e) = s.write_all(pl) {
            eprintln!("[ocbmd] rtsp: relay write failed ({e}) — dropping socket (airplayd falls back local)");
            self.rtsp_sock = None;
        }
    }

    fn send_file_ack(&mut self, status: u8, crc: u32, size: u32) {
        let mut pl = Vec::with_capacity(10);
        pl.push(p::FILE_ACK);
        pl.push(status);
        pl.extend_from_slice(&crc.to_le_bytes());
        pl.extend_from_slice(&size.to_le_bytes());
        self.send(p::CH_FILE, p::F_SOM | p::F_EOM, &pl);
    }

    /// OCBM_CH_FILE: PULL (box->host retrieval) is handled here — it must actively send many frames, so
    /// it can't fit the `on_frame → single ack` push state machine. Everything else drives that machine.
    fn handle_file(&mut self, pl: &[u8]) {
        if pl.first() == Some(&p::FILE_PULL) {
            let start = Instant::now();
            self.handle_file_pull(&pl[1..]);
            // Same trap as CT_SRC / handle_mfi: a paced pull can block this single-threaded
            // dispatch for seconds while heartbeats sit unread — refresh `last_hb` so
            // `presence_tick` doesn't declare a live host gone right after a big pull.
            if start.elapsed() >= Duration::from_secs(1) && self.last_hb.is_some() {
                self.last_hb = Some(Instant::now());
            }
            return;
        }
        if let Some((status, crc, size)) = self.file.on_frame(pl) {
            self.send_file_ack(status, crc, size);
        }
    }

    /// CH_FILE PULL: stream the file at `path_bytes` back to the host as FILE_DATA sub-frames, then a
    /// terminal FILE_ACK carrying the end-to-end CRC-32 + size (the host verifies). Path is validated
    /// exactly like FILE_OPEN (absolute, no `..`) so a pull can't escape the rootfs. The read loop is
    /// PACED to the host's drain rate (same watermark as the CT_SRC bench): reading flash full-speed
    /// under USB contention repeatedly filled `out_lo` to its 1 MiB cap, whose clear-on-overflow lost a
    /// 1 MiB burst each time — a deterministic host-side CRC failure. A host that stops draining
    /// entirely is bounded by a deadline (abort, never a wedged dispatch loop).
    fn handle_file_pull(&mut self, path_bytes: &[u8]) {
        let path = match std::str::from_utf8(path_bytes) {
            Ok(s) if !s.is_empty() && !s.contains("..") && s.starts_with('/') => s.to_string(),
            _ => {
                self.send_file_ack(p::FILE_ERR_OPEN, 0, 0);
                return;
            }
        };
        // Refuse anything that is not a regular file BEFORE opening (audit #1). The old validation only
        // checked non-empty / no-".." / leading-'/', so a FILE_PULL of "/dev/zero" opened fine and the
        // read loop below never reached Ok(0) — the single-threaded poll loop spun forever (heartbeat,
        // A/V, CONSOLE and USB all frozen until the watchdog respawned us), and a FIFO path blocked even
        // earlier INSIDE File::open (O_RDONLY on a fifo with no writer). std::fs::metadata uses stat()
        // (no blocking open), so it is safe on fifos/devices, and is_file() rejects them. A size cap
        // bounds a pathological huge/sparse file so a single pull can't monopolize the poll loop.
        const MAX_PULL_BYTES: u64 = 32 * 1024 * 1024; // > any legit pull (binaries ~1.5M, logs, captures)
        match std::fs::metadata(&path) {
            Ok(m) if m.file_type().is_file() && m.len() <= MAX_PULL_BYTES => {}
            _ => {
                self.send_file_ack(p::FILE_ERR_OPEN, 0, 0);
                return;
            }
        }
        let mut f = match File::open(&path) {
            Ok(f) => f,
            Err(_) => {
                self.send_file_ack(p::FILE_ERR_NOFILE, 0, 0);
                return;
            }
        };
        let mut crc = p::CRC32_INIT;
        let mut size: u32 = 0;
        // One data opcode byte + chunk must fit a single OCBM payload (matches the push chunking).
        let mut buf = vec![0u8; p::MAX_PAYLOAD - 1];
        // Bound each paced wait: a host that STOPS draining must not wedge the single-threaded
        // dispatch loop forever. Per-chunk (not whole-pull) so a slow-but-live host is never cut off:
        // any host draining at all clears the 256 KiB watermark well inside 5 s, while a dead one
        // aborts fast — the host then sees a short stream → CRC mismatch → clean retry.
        const PULL_STALL_DEADLINE: Duration = Duration::from_secs(5);
        loop {
            match f.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let mut d = Vec::with_capacity(1 + n);
                    d.push(p::FILE_DATA);
                    d.extend_from_slice(&buf[..n]);
                    self.send(p::CH_FILE, p::F_SOM | p::F_EOM, &d);
                    self.drain(); // flush this frame before reading the next (bounded out_lo)
                    // Pace the flash read to the host's drain rate (same watermark as the CT_SRC
                    // bench): without this, `out_lo` hits its 1 MiB cap under USB contention and the
                    // cap-clear loses a 1 MiB burst — a deterministic host-side CRC failure.
                    let stall = Instant::now();
                    while self.out_lo.len() > 262_144 {
                        let elapsed = stall.elapsed();
                        if elapsed >= PULL_STALL_DEADLINE {
                            eprintln!("[ocbmd] file pull of {path} aborted — host stopped draining");
                            self.send_file_ack(p::FILE_ERR_WRITE, 0, 0);
                            return;
                        }
                        // Wait for POLLOUT instead of busy-spinning drain() against EAGAIN.
                        self.wait_acc_writable(PULL_STALL_DEADLINE - elapsed);
                        self.drain();
                    }
                    crc = p::crc32_update(crc, &buf[..n]);
                    size = size.wrapping_add(n as u32);
                }
                Err(_) => {
                    self.send_file_ack(p::FILE_ERR_WRITE, 0, 0);
                    return;
                }
            }
        }
        self.send_file_ack(p::FILE_OK, p::crc32_final(crc), size);
    }

    /// CH_MGMT — the app's "CCPA" tab. GET_INFO returns a JSON snapshot; the action verbs execute + ACK.
    /// Deliberately dependency-free (hand-rolled JSON, direct /sys+/proc reads) to keep ocbmd lean.
    fn handle_mgmt(&mut self, pl: &[u8]) {
        let verb = match pl.first() {
            Some(v) => *v,
            None => return,
        };
        match verb {
            p::MGMT_GET_INFO => {
                let json = self.box_info_json();
                let mut out = Vec::with_capacity(1 + json.len());
                out.push(p::MGMT_INFO);
                out.extend_from_slice(json.as_bytes());
                self.send(p::CH_MGMT, p::F_SOM | p::F_EOM, &out);
            }
            p::MGMT_REBOOT => {
                self.mgmt_ack(verb, 0);
                self.drain(); // flush the ack to the host before the box goes down
                let _ = std::process::Command::new("sh")
                    .args(["-c", "sleep 1; sync; reboot"]) // delay so the ack reaches the app
                    .spawn();
            }
            p::MGMT_ENTER_NCM => {
                // Arm the persistent NCM flag BEFORE acking so a host that sees status 0 knows the
                // next boot IS NCM even if the reboot spawn below were to fail. Sticky: the operator
                // returns the box over ssh (`rm /script/ncm_only; reboot`). Same file effects as
                // `tools/ocbm_install.sh revert`.
                let armed = std::fs::write("/script/ncm_only", b"").is_ok();
                let _ = std::fs::remove_file("/script/ocbm_trial");
                eprintln!("[ocbmd] mgmt: ENTER_NCM requested by host — flag armed={armed}; rebooting into NCM");
                self.mgmt_ack(verb, if armed { 0 } else { 1 });
                self.drain();
                if armed {
                    let _ = std::process::Command::new("sh")
                        .args(["-c", "sleep 1; sync; reboot"])
                        .spawn();
                }
            }
            p::MGMT_FORGET_ALL => {
                let existed = std::path::Path::new(BT_LINK_KEY_STORE).exists();
                let ok = !existed || std::fs::remove_file(BT_LINK_KEY_STORE).is_ok();
                forget_airplay_peers();
                request_wireless_restart(); // reload with empty keys → controller's bonds cleared
                self.mgmt_ack(verb, u8::from(!ok));
            }
            p::MGMT_FORGET_DEVICE => {
                let mac = std::str::from_utf8(&pl[1..]).unwrap_or("").trim();
                let ok = forget_one_bond(mac);
                forget_airplay_peers();
                request_wireless_restart();
                self.mgmt_ack(verb, u8::from(!ok));
            }
            p::MGMT_RESTART_WIRELESS => {
                request_wireless_restart();
                self.mgmt_ack(verb, 0);
            }
            _ => {}
        }
    }

    fn mgmt_ack(&mut self, verb: u8, status: u8) {
        self.send(
            p::CH_MGMT,
            p::F_SOM | p::F_EOM,
            &[p::MGMT_ACK, verb, status],
        );
    }

    /// Build the CCPA-tab info snapshot as compact JSON. Values here are all controlled (MACs, hex,
    /// numbers, fixed daemon names) so hand-rolled JSON needs no escaping. Cheap AND non-blocking: a
    /// few file reads, one /proc scan, one statvfs, and two atomic loads for the Bluetooth state.
    fn box_info_json(&self) -> String {
        let procs = running_procs(&["ocbmd", "iap2d", "airplayd", "carplay-wireless", "hostapd"]);
        let (rt_total, rt_free) = rootfs_stats_kb();
        // % used = 100 - free%. checked_div guards a zero total (statvfs failure) without a manual `if`.
        let rt_pct = (rt_free * 100)
            .checked_div(rt_total)
            .map_or(0, |free_pct| 100u64.saturating_sub(free_pct));
        let uptime_s: u64 = read_trim("/proc/uptime")
            .split('.')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let wifi_mac = read_trim("/sys/class/net/wlan0/address");
        // Safe on this thread, unlike the ioctl: `show_address` is a sprintf of `hdev->bdaddr` under
        // no HCI lock, and kernfs makes an unregistering attribute fail the open rather than block
        // the reader. Worst case it returns "" mid-teardown.
        let bt_mac = read_trim("/sys/class/bluetooth/hci0/address");
        let serial = read_trim("/etc/serial_number");
        let name = bt_name_from(&wifi_mac, &serial); // hex-filtered, so a raw serial is fine here
        let transport = json_escape(&read_trim("/tmp/carplay_transport"));
        let phone = read_trim("/tmp/phone_present") == "1";
        // The `bt_probe` thread's atomics, exactly what box_health_tick reads — the two must not be
        // able to disagree, and NEITHER may probe the controller from this thread (see [`BtProbe`]:
        // an inline `hci0_up()` here wedged the daemon while the radio seam was recovering hci0).
        // Stale => both false, i.e. UNKNOWN reported as not-up.
        let (hci_up, ssp, _fresh) = self.bt.snapshot();
        // MGMT_GET_INFO can arrive with no SUBSCRIBE behind it (the app's CCPA tab polls it), so ask
        // for the SSP sample here too; the answer lands on the probe thread's next pass.
        self.bt.request_ssp();
        let devs = bonded_macs()
            .iter()
            .map(|m| format!("\"{m}\""))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"bt_mac\":\"{bt_mac}\",\"wifi_mac\":\"{wifi_mac}\",\"serial\":\"{ser}\",\
             \"name\":\"{name}\",\"uptime_s\":{uptime_s},\"rootfs_pct\":{rt_pct},\
             \"rootfs_free_kb\":{rt_free},\"ssp\":{ssp},\"hci_up\":{hci_up},\"wlan_ap\":{ap},\
             \"transport\":\"{transport}\",\"host_present\":{host},\"phone_present\":{phone},\
             \"daemons\":{{\"ocbmd\":{d0},\"iap2d\":{d1},\"airplayd\":{d2},\"carplay_wireless\":{d3}}},\
             \"host_name\":\"{host_name}\",\"devices\":[{devs}]}}",
            ser = json_escape(&serial),
            host_name = json_escape(self.host_name.as_deref().unwrap_or("")),
            ap = procs[4],
            host = self.present,
            d0 = procs[0],
            d1 = procs[1],
            d2 = procs[2],
            d3 = procs[3],
        )
    }

    fn handle(&mut self, ch: u16, flags: u8, pl: &[u8]) {
        match ch {
            p::CH_CTRL => {
                if pl.first() == Some(&p::CT_HELLO) {
                    // Host instance nonce (trailing u32 LE, 0 = not supplied). A DIFFERENT nonce while
                    // we still think a host is present means the previous one died without CT_STOP:
                    // its airplayd went with it, but presence never dropped, so nothing would re-ARM.
                    // Flag it for the SUBSCRIBE that follows. Same nonce = the same host reattaching
                    // (USB blip, client rebuilt) over a session that never ended — nothing to re-arm.
                    // Carried out of the two parse blocks below only so the unconditional HELLO line
                    // (see below) can report what this HELLO actually said, including when nothing
                    // changed. 0 / "" read as "the host did not supply one".
                    let mut inst = 0u32;
                    let mut label = String::new();
                    if pl.len() >= 6 {
                        inst = u32::from_le_bytes([pl[2], pl[3], pl[4], pl[5]]);
                        if inst != 0 {
                            // `subscribed` as well as `present`, deliberately: this flag means "the
                            // previous host died mid-session without CT_STOP", and only a SUBSCRIBEd
                            // host has a session to die in. A predecessor that sent CT_STOP already
                            // went fully idle (both flags false), so its successor's HELLO reads as
                            // what it is — a fresh attach, not a replacement.
                            if self.present
                                && self.subscribed
                                && self.host_instance.is_some_and(|prev| prev != inst)
                            {
                                eprintln!(
                                    "[ocbmd] session: host instance changed while present ({:?} -> {inst:#010x}) — previous host died without CT_STOP; will re-arm",
                                    self.host_instance
                                );
                                self.host_replaced = true;
                            }
                            self.host_instance = Some(inst);
                        }
                    }
                    // Optional host label after the nonce. Additive and length-tolerant in both
                    // directions: an older host sends nothing and is unchanged, and an older box
                    // ignores the extra bytes exactly as this one used to.
                    if pl.len() > 6 {
                        let raw: String = String::from_utf8_lossy(&pl[6..])
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(64)
                            .collect();
                        label = raw.trim().to_string();
                        if !label.is_empty() && self.host_name.as_deref() != Some(label.as_str()) {
                            eprintln!("[ocbmd] session: host identifies as {label:?}");
                            self.host_name = Some(label.clone());
                        }
                    }
                    // REATTACH RESYNC: a HELLO means a fresh host session. Discard any stale outbound
                    // bytes left from a prior session so the new host sees HELLO_ACK first, not a
                    // leftover A/V/bulk frame (the "no HELLO_ACK" desync). The host also drains stale
                    // frames until the ACK; together they make host reconnects clean without an
                    // ocbmd restart. (host->box self-resyncs on magic in the reassembler.)
                    self.out_hi.clear();
                    self.out_video.clear();
                    self.out_alt_video.clear();
                    self.out_audio.clear();
                    self.out_lo.clear();
                    // out_console too: it is the one queue that can be resting MID-FRAME under a
                    // console flood, and clearing `wire_owner` below without emptying it would splice
                    // HELLO_ACK into the middle of that half-written frame — the very desync this
                    // block exists to prevent.
                    self.out_console.clear();
                    // out_log for the same reason: it can rest mid-frame, and `wire_owner` is
                    // cleared below.
                    self.out_log.clear();
                    // Every queue was just emptied, so nothing can still be mid-frame on the wire.
                    // Leaving a stale owner here would make the next drain() try to "finish" a queue
                    // that no longer has the frame it was writing.
                    self.wire_owner = None;
                    // Fresh session, fresh flow-control state: a stale `lo_resync` would silently
                    // drop the new session's bulk frames until the next F_SOM, and stale
                    // capped/backpressured flags would mis-gate the transition-only logging.
                    self.lo_resync = false;
                    self.lo_capped = false;
                    self.av_backpressured = false;
                    self.seq = 0;
                    // A NEW host attached, so any CH_IP relay socket still open belongs to the
                    // PREVIOUS one. Nothing else drops them: this reattach path clears the output
                    // queues but not `conns`, and the conns map is otherwise cleared only on CT_STOP,
                    // go_idle, or an out_lo gap. A host that died without CT_STOP and relaunched
                    // inside the heartbeat grace therefore left its corpse's socket being pumped,
                    // while the new host's IP_OPEN sat unaccepted in aa-bridge's backlog (the bridge
                    // serves ONE client per prepared accessory) — Android Auto retried forever.
                    // Conn ids are per-ATTEMPT on the host side (0x00AA + gen&0x3F), so a stale
                    // socket can never be adopted by the new host and there is nothing to preserve.
                    // On a CarPlay session `conns` is empty and this is a literal no-op.
                    if !self.conns.is_empty() {
                        eprintln!(
                            "[ocbmd] session: HELLO — dropping {} stale CH_IP relay socket(s) from the previous host",
                            self.conns.len()
                        );
                        self.conns.clear();
                        self.hello_cleared_conns = true;
                    }
                    let mut caps =
                        p::CAP_CONSOLE | p::CAP_ECHO | p::CAP_IP | p::CAP_FILE | p::CAP_ETH;
                    if self.mfi.is_some() {
                        caps |= p::CAP_MFI;
                    }
                    // Unconditional: everything above logs only CHANGES, so a host reattaching with
                    // the same nonce and label — the ordinary case, and the one a "why is there no
                    // HELLO in the log" investigation starts from — left no trace at all.
                    eprintln!(
                        "[ocbmd] HELLO nonce={inst:#010x} caps={caps:#x} label={label:?} replaced={}",
                        self.host_replaced
                    );
                    let mut a = [0u8; 7];
                    a[0] = p::CT_HELLO_ACK;
                    a[1] = p::VERSION;
                    a[2..6].copy_from_slice(&caps.to_le_bytes());
                    a[6] = if self.ptm.is_some() {
                        p::MODE_CONSOLE
                    } else {
                        p::MODE_PROJECTION
                    };
                    self.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &a);
                } else if pl.len() >= 2 && pl[0] == p::CT_MODE_SELECT && pl[1] == p::MODE_CONSOLE {
                    self.start_console();
                } else if pl.len() >= 9 && pl[0] == p::CT_SETTIME {
                    // host->box clock sync (no RTC battery): apply the wall clock, then ack
                    // back the seconds actually set so the host can confirm.
                    let secs = u64::from_le_bytes(pl[1..9].try_into().unwrap());
                    // `time_t` is 32-bit on the shipped armv7 musl build (libc applies its time64
                    // cfg to riscv32 only), so `secs as _` would WRAP past 2038-01-19 and set an
                    // arbitrary past date while acking success. Refuse instead — a box with an
                    // obviously wrong clock fails pairing loudly; one silently set to 1902 does not.
                    // `try_into`, not `as`: the target type is inferred from `tv_sec`, so this
                    // rejects what would not fit instead of naming the deprecated `libc::time_t`.
                    let secs_t = secs.try_into();
                    let mut ok = false;
                    if let Ok(sec) = secs_t {
                        // Built field-by-field from `zeroed()`, NOT with a struct literal. Under
                        // `musl32_time64` (required on riscv32 — see c2air/README.md) these types carry
                        // private padding fields and a literal will not compile. This form builds on
                        // every target.
                        let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
                        tv.tv_sec = sec;
                        ok = unsafe { libc::settimeofday(&tv, std::ptr::null()) } == 0;
                    } else {
                        eprintln!("[ocbmd] settime: REJECTED secs={secs} — out of range for this target's time_t");
                    }
                    if let (false, Ok(sec)) = (ok, secs_t) {
                        // Defensive second attempt. NOTE: the original riscv32 failure here was NOT a
                        // missing `SYS_settimeofday` — that first diagnosis was wrong. The real cause
                        // was the `libc` crate defaulting 32-bit musl to a 32-bit `time_t`, so this
                        // struct was 8 bytes where the time64 kernel wanted 16 and the call returned
                        // EINVAL. With the cfg set, plain `settimeofday` works on riscv32 and this
                        // branch is not taken. It is kept only because it is nearly free.
                        //
                        // A FALLBACK rather than an arch cfg on purpose: armv7 keeps its exact
                        // existing behaviour, since this only runs after a failure.
                        let first = std::io::Error::last_os_error();
                        let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
                        ts.tv_sec = sec;
                        ok = unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &ts) } == 0;
                        if ok {
                            eprintln!("[ocbmd] settime: settimeofday failed ({first}); clock_settime applied");
                        } else {
                            // Report BOTH errnos. The old code discarded them entirely, which made a
                            // one-line "rejected" the only symptom and cost a session to diagnose.
                            eprintln!(
                                "[ocbmd] settime: FAILED secs={secs} (tv_sec width {}B) — settimeofday: {first}; clock_settime: {}",
                                std::mem::size_of::<libc::time_t>(),
                                std::io::Error::last_os_error()
                            );
                        }
                    }
                    let mut a = [0u8; 10];
                    a[0] = p::CT_SETTIME;
                    a[1..9].copy_from_slice(&secs.to_le_bytes());
                    a[9] = if ok { 0 } else { 1 }; // 0 = applied, 1 = settimeofday failed
                    self.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &a);
                } else if pl.len() >= 5 && pl[0] == p::CT_SRC {
                    // Debug downlink flood (srcbench). CAP the host-supplied duration: this loop blocks the
                    // single-threaded poll for the whole window, so a bogus/huge value must not wedge the
                    // daemon (and its heartbeat) for minutes/days.
                    // QC 2026-07-25: the cap used to be 30 s, which is LONGER than HEARTBEAT_GRACE
                    // (10 s). Because this loop blocks the single-threaded dispatch, heartbeats
                    // arriving during the bench sit unread in the kernel buffer while `presence_tick`
                    // — which runs immediately after this pass — sees a stale `last_hb`. Any bench
                    // past the grace therefore declared the host GONE, dropped the subscription and
                    // deleted the ephemeral config, after which the buffered heartbeats were read with
                    // `subscribed == false` and ignored: the bench silently destroyed the live session.
                    // Clamp well under the grace AND refresh `last_hb` afterwards (the host really was
                    // alive throughout — we simply could not read its heartbeats).
                    let ms = (u32::from_le_bytes([pl[1], pl[2], pl[3], pl[4]]) as u128)
                        .min(HEARTBEAT_GRACE.as_millis() / 2);
                    let payload = vec![0xA5u8; 32768]; // downlink flood: box->host
                    let start = Instant::now();
                    while start.elapsed().as_millis() < ms {
                        self.send(p::CH_ECHO, p::F_SOM | p::F_EOM, &payload);
                        // backpressure: keep the queue bounded, paced to the host's drain rate
                        // (poll POLLOUT rather than busy-spinning drain() against EAGAIN)
                        while self.out_lo.len() > 262_144 && start.elapsed().as_millis() < ms {
                            self.wait_acc_writable(Duration::from_millis(10));
                            self.drain();
                        }
                    }
                    if self.last_hb.is_some() {
                        self.last_hb = Some(Instant::now());
                    }
                } else if pl.first() == Some(&p::CT_ETH_START) {
                    // begin bridging a netdev's raw L2 frames onto CH_ETH (default ncm0)
                    let ifn = if pl.len() > 1 {
                        std::str::from_utf8(&pl[1..]).unwrap_or("ncm0")
                    } else {
                        "ncm0"
                    };
                    if self.eth.is_none() {
                        self.eth = eth::open(ifn);
                    }
                } else if pl.first() == Some(&p::CT_ETH_STOP) {
                    if let Some(fd) = self.eth.take() {
                        eth::close(fd);
                    }
                } else if pl.first() == Some(&p::CT_SUBSCRIBE) {
                    // A replaced host — one whose predecessor died WITHOUT CT_STOP — owes a clean
                    // re-arm: its airplayd is gone (or bound to the dead host) and presence never
                    // dropped, so only a forced GONE->PRESENT edge makes the supervisor spawn a new
                    // one. A host that closed cleanly needs nothing here: CT_STOP already went idle,
                    // so `set_present(true)` below IS the edge.
                    let replaced = std::mem::take(&mut self.host_replaced);
                    // Diagnostic only (there is no warm-reuse path left to gate on it): says whether
                    // this SUBSCRIBE re-pushed the same YAML the previous one did.
                    let cfg_changed = self.cfg.as_slice() != &pl[1..];
                    // host receiver active: record its ephemeral config, stamp liveness, go present
                    self.subscribed = true;
                    self.last_hb = Some(Instant::now());
                    // FRESH-SESSION A/V RESYNC (2026-09-03). CT_HELLO clears these already; a SUBSCRIBE
                    // that did NOT follow a HELLO (a host re-arming over an attach that is still up)
                    // would otherwise hand the receiver the PREVIOUS session's queued tail — a partial
                    // seam message that its byte-stream reassembly splices onto the next producer's
                    // first bytes. Normally a no-op: each A/V stream is gated to one in-flight frame.
                    self.out_video.clear();
                    self.out_alt_video.clear();
                    self.out_audio.clear();
                    // If one of those was resting MID-FRAME it owns the wire, and leaving the owner set
                    // would make the next drain() try to finish a frame its queue no longer holds (the
                    // same hazard the CT_HELLO block documents). The receiver resyncs on the OCBM magic,
                    // so the cost is the single truncated frame we just chose to abandon.
                    if matches!(
                        self.wire_owner,
                        Some(Wire::Video) | Some(Wire::AltVideo) | Some(Wire::Audio)
                    ) {
                        self.wire_owner = None;
                    }
                    self.phone_state = None; // re-emit current phone state to this (fresh) host
                    self.last_phone_check = None; // ...and read the flag on the very next tick
                    self.pairing_code = None; // re-emit any live pairing code to this (fresh) host
                    self.phone_ident = None; // re-emit who the phone is to this (fresh) host
                    self.bt_phase = None; // and re-emit the current BT phase, so a host attaching
                                          // mid-handshake is not blind until the next transition
                                          // (which may never come).
                    self.last_box_health_check = None; // ...and let it re-emit immediately, not up to
                                                       // 2 s later on the next throttle window

                    // Re-sample SSP: a new session may have re-run bring-up. A REQUEST, not a probe —
                    // the `hciconfig` fork happens on the bt_probe thread and lands within one 2 s
                    // pass. So this session's FIRST CT_BOX_HEALTH can be one BH_SSP bit short, and
                    // the mirror sends a second frame when it arrives. That is the trade for never
                    // forking on this thread; the host already handles BOX_HEALTH as a change stream.
                    self.bt.request_ssp();
                    self.box_health = None; // re-emit the box's readiness to this (fresh) host, so it
                                            // can evaluate "am I green AND is the box green" without
                                            // having to poll MGMT_GET_INFO first
                    self.proj_mode = None; // likewise the projection mode: a host that re-attaches
                                           // to a box already owned by AA (or CarPlay) must learn
                                           // which engine to run without waiting for a re-arm.
                    self.cfg = pl[1..].to_vec();
                    // Land the ephemeral YAML for airplayd to read per connection (task #5 / docs/carplay/04_CAPABILITIES_AND_CONFIG.md).
                    write_cfg_file(&self.cfg);
                    // A fresh SUBSCRIBE's YAML is authoritative over any prior CT_RADIO inhibit.
                    //
                    // This clear is UNCONDITIONAL on purpose, and an attempt to make it conditional
                    // (hold the inhibit across a same-host/same-cfg reattach, so a host could return
                    // to the box without waking its radios under a live session) was reverted. It did
                    // not work and it was not safe:
                    //
                    //   - Inert where it was wanted. Losing the box stops heartbeats, and after
                    //     HEARTBEAT_GRACE (10 s) `go_idle` clears both `self.cfg` and the inhibit. A
                    //     real USB re-enumeration round trip on the host — detach, attach intent,
                    //     permission, claim, HELLO retries, MFi — takes far longer than that, so the
                    //     SUBSCRIBE that eventually arrives always sees an empty cfg, reads as
                    //     changed, and clears anyway.
                    //   - Unsafe where it did fire. The only window it covered was a quick Stop/Start
                    //     by the same process with unchanged settings, which is exactly where holding
                    //     the inhibit strands the box: `wireless_up()` returns early on the flag, no
                    //     BT phase is ever emitted, so the host arms no watchdog and never learns.
                    //
                    // The host keeps this lever by RE-ASSERTING CT_RADIO off after it reattaches,
                    // which does not depend on the box remembering anything across a session boundary.
                    let _ = std::fs::remove_file(RADIO_OFF_FLAG);
                    eprintln!(
                        "[ocbmd] session: SUBSCRIBE ({} B config, cfg_changed={cfg_changed} replaced={replaced})",
                        self.cfg.len()
                    );
                    if replaced {
                        // A REPLACEMENT host whose predecessor died without CT_STOP: presence never
                        // dropped, so it owes the teardown+re-arm CT_STOP would have done. airplayd
                        // re-establishes on the GONE->PRESENT edge. Silently, though — see
                        // rearm_presence_silently.
                        self.rearm_presence_silently();
                        self.send(
                            p::CH_CTRL,
                            p::F_SOM | p::F_EOM,
                            &[p::CT_SESSION_EVENT, p::SEV_HOST_PRESENT],
                        );
                    } else if self.present {
                        // Presence never dropped — a re-SUBSCRIBE by the SAME host (config re-push, or a
                        // reconnect inside the heartbeat grace). Confirm presence explicitly: set_present's
                        // edge guard would otherwise swallow the event.
                        self.send(
                            p::CH_CTRL,
                            p::F_SOM | p::F_EOM,
                            &[p::CT_SESSION_EVENT, p::SEV_HOST_PRESENT],
                        );
                    } else {
                        // The GONE->PRESENT edge the supervisor arms on — deferred if the preceding
                        // GONE cannot have been sampled yet. See `raise_presence`.
                        self.raise_presence(Instant::now());
                    }
                } else if pl.first() == Some(&p::CT_HEARTBEAT) {
                    self.last_hb = Some(Instant::now());
                    if self.subscribed {
                        self.set_present(true); // recover if a prior beat had lapsed
                    }
                } else if pl.first() == Some(&p::CT_STOP) {
                    // A clean host close IS the end of the session. Take the identical path heartbeat
                    // loss takes — `go_idle`, the one host-gone routine — so the `/tmp/host_present`
                    // 1->0 edge lands NOW and the supervisor runs its complete wireless teardown back
                    // to IDLE (the phone disconnects with it). No grace: the box previously held the
                    // session warm for 5 s hoping for a relaunch, which left a host-less projection
                    // running and the phone attached to nobody.
                    //
                    // SILENT (`notify_host = false`): the host has already detached, so a SEV_HOST_GONE
                    // would sit unread in the gadget FIFO and be the FIRST frame the NEXT host reads —
                    // see `go_idle`'s contract. The supervisor's teardown rides the flag file, not the
                    // event, so it is unaffected.
                    eprintln!("[ocbmd] session: STOP — host closed, ending the projection session");
                    self.go_idle(false);
                } else if pl.first() == Some(&p::CT_RADIO) {
                    // App-commanded mid-session radio kill switch (docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating). Actuation
                    // rides the proven flag-file→supervisor pattern (see /tmp/wireless_restart): the
                    // supervisor's 1 Hz loop observes the flag edge and runs wireless_down/up. This
                    // deliberately does NOT ride a config re-push — a mid-session CT_SUBSCRIBE with a
                    // changed cfg forces the R3 present-cycle session rebuild, which a radio toggle
                    // must not cause.
                    match pl.get(1) {
                        Some(0) => {
                            let _ = std::fs::write(RADIO_OFF_FLAG, "1");
                            eprintln!("[ocbmd] session: CT_RADIO off (radios inhibited)");
                        }
                        Some(1) => {
                            let _ = std::fs::remove_file(RADIO_OFF_FLAG);
                            eprintln!("[ocbmd] session: CT_RADIO on (inhibit cleared; cfg governs)");
                        }
                        other => eprintln!("[ocbmd] session: CT_RADIO bad arg {other:?} (ignored)"),
                    }
                } else if pl.first() == Some(&p::CT_PAIR_CONFIRM) {
                    // [CT_PAIR_CONFIRM][accept u8] — the user's answer to the CT_PAIRING_CODE prompt,
                    // forwarded to carplay-wireless's control port. That daemon owns the radio and the
                    // SSP agent; ocbmd only mirrors `/tmp/pairing_code` outward, so the answer has to
                    // cross back over a seam rather than a flag file: a flag would have to be RACE-free
                    // against the agent's own clear, and this direction is a single bounded request.
                    //
                    // A short frame is a CANCEL, not a pair: the safe direction for an unparseable
                    // request is to refuse a bond nobody confirmed, never to complete one.
                    let accept = pl.get(1).is_some_and(|&b| b != 0);
                    self.send_pair_answer(accept);
                } else if pl.first() == Some(&p::CT_LOG_CTL) {
                    // [CT_LOG_CTL][enabled u8][cap_kb u16 LE]. A short frame reads as "disable with
                    // the default cap", which is the safe direction: an unparseable request must
                    // never leave a stream running that the host does not know about.
                    let enable = pl.get(1).is_some_and(|&b| b != 0);
                    let cap_kb = if pl.len() >= 4 {
                        u16::from_le_bytes([pl[2], pl[3]])
                    } else {
                        0
                    };
                    let was = self.log.enabled;
                    self.log_reset(); // both directions start from the off state
                    self.log.cap = cap_kb as u64 * 1024; // 0 => LogTail::cap_bytes' default
                    self.log.enabled = enable;
                    // Fire on the very next tick rather than up to LOG_TICK later, the same
                    // `last_*_check = None` trick the phone/box-health mirrors use on SUBSCRIBE.
                    self.log.last_check = None;
                    eprintln!(
                        "[ocbmd] log stream {} (cap {} KiB{})",
                        if enable { "ENABLED — from offset 0" } else { "disabled" },
                        self.log.cap_bytes() / 1024,
                        if was == enable { ", unchanged" } else { "" }
                    );
                }
            }
            p::CH_ECHO => self.send(p::CH_ECHO, flags, pl),
            p::CH_CONSOLE => {
                if let Some(ptm) = self.ptm.as_mut() {
                    let _ = ptm.write_all(pl);
                }
            }
            p::CH_MFI => self.handle_mfi(pl),
            p::CH_IP => self.handle_ip(pl),
            p::CH_FILE => self.handle_file(pl),
            p::CH_ETH => {
                if let Some(fd) = self.eth {
                    eth::send_frame(fd, pl); // send frame onto ncm0 (host -> iPhone)
                }
            }
            p::CH_INPUT => self.forward_input(pl), // HID input host -> airplayd -> iPhone (task #20)
            p::CH_MIC => self.forward_mic(pl), // mic PCM host -> airplayd -> iPhone (type-100 uplink)
            p::CH_RTSP => self.forward_rtsp(pl), // SETUP-relay bytes host -> airplayd (RS_RESP/RS_ERR)
            p::CH_MGMT => self.handle_mgmt(pl), // box management (the app's "CCPA" tab)
            _ => {}
        }
    }
}

/// Singleton lock file. `ocbm_boot.sh` starts ocbmd directly AND inittab respawns `run_ocbmd.sh`,
/// whose one-shot `pgrep` guard races the first instance's fork — so two ocbmd really can coexist,
/// both reading `/dev/usb_accessory`, and the loser comes up with a healthy control plane but zero
/// A/V listeners (the ports are already bound). That presents as "link fine, MFi fine, console
/// fine, no video", which is why it survived so long.
const PID_LOCK_FILE: &str = "/tmp/ocbmd.pid";

/// `flock(LOCK_EX|LOCK_NB)` `path`, then write our pid into it.
///
/// The returned fd MUST be held for the process lifetime: the lock lives on the open file
/// description, so closing it releases the lock while we are still running. `ErrorKind::WouldBlock`
/// means another live instance holds it.
fn acquire_pid_lock(path: &str) -> std::io::Result<RawFd> {
    let cpath = std::ffi::CString::new(path)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let fd = unsafe {
        libc::open(
            cpath.as_ptr(),
            libc::O_CREAT | libc::O_RDWR | libc::O_CLOEXEC,
            0o644,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // Truncate first: a shorter pid written over a longer one would otherwise leave the previous
    // holder's trailing digits behind, and this file is meant to be readable by a shell supervisor.
    let s = format!("{}\n", std::process::id());
    unsafe {
        libc::ftruncate(fd, 0);
        let _ = libc::write(fd, s.as_ptr() as *const libc::c_void, s.len());
    }
    Ok(fd)
}

/// Liveness stamp for the supervisor.
///
/// CONTRACT: mtime older than 15 s while the accessory is configured ⇒ ocbmd is wedged;
/// `session_supervisor.sh` escalates (a separate change ships that). Absent = ocbmd is not running,
/// which the supervisor already detects via [`PID_LOCK_FILE`] and handles differently.
///
/// It exists because the hang this file's [`BtProbe`] fixes had NO outward symptom the supervisor
/// could see: the process was alive and holding its pid lock, `/tmp/host_present` still read 1, and
/// it never exited, so nothing respawned it for >5 minutes. A pid is not liveness.
const ALIVE_FILE: &str = "/tmp/ocbmd_alive";

/// Rate limit on [`touch_alive_file`]: the dispatch loop wakes thousands of times/sec during A/V and
/// the supervisor polls at ~1 Hz, so more than 1 Hz here is pure syscall overhead.
const ALIVE_TOUCH_INTERVAL: Duration = Duration::from_secs(1);

/// Set `ALIVE_FILE`'s mtime to now. One `utimensat` with a NULL `times` — no open, no write, no
/// buffer, and no `time_t` to get wrong on 32-bit musl (the kernel supplies the timestamp).
fn touch_alive_file() {
    if let Ok(c) = std::ffi::CString::new(ALIVE_FILE) {
        unsafe {
            libc::utimensat(libc::AT_FDCWD, c.as_ptr(), std::ptr::null(), 0);
        }
    }
}

/// Clear the session state a departed host owns, on the transport-death exit paths.
///
/// Those paths `exit(1)` immediately, and startup was the ONLY place that cleared these files — so
/// on a box whose supervisor does not respawn ocbmd, `/tmp/host_present` stayed at 1 forever and the
/// supervisor believed a host was attached with nothing left to talk to. Replays exactly the pure
/// file effects `go_idle` and `main()`'s startup perform; the `SEV_HOST_GONE` frame is deliberately
/// NOT sent, because the accessory fd that would carry it is precisely what just died.
fn clear_session_state_for_exit() {
    write_flag_atomic(HOST_PRESENT_FLAG, false);
    clear_cfg_file();
    let _ = std::fs::remove_file(RADIO_OFF_FLAG);
    // These paths exit on purpose, for respawn — the supervisor must read "gone", not "wedged".
    // Only reached after the file was created; the earlier exits (duplicate instance, ACC_DEV,
    // required listener) must NOT remove it, one of them because a LIVE instance owns it.
    let _ = std::fs::remove_file(ALIVE_FILE);
}

fn main() {
    // Singleton guard BEFORE anything with a side effect — see [`PID_LOCK_FILE`]. The fd is held
    // (never closed) for the process lifetime; the kernel releases the lock when we exit.
    let _pid_lock = match acquire_pid_lock(PID_LOCK_FILE) {
        Ok(fd) => fd,
        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            eprintln!("[ocbmd] another instance holds {PID_LOCK_FILE} — exiting");
            // 0, NOT 1: inittab/run_ocbmd.sh respawn on exit, and a duplicate that keeps exiting
            // non-zero is a restart storm against a perfectly healthy first instance.
            std::process::exit(0);
        }
        // A /tmp we cannot even create a file in is not a reason to leave the box without its
        // daemon — come up unguarded, but say so.
        Err(e) => {
            eprintln!("[ocbmd] pid lock {PID_LOCK_FILE} unusable ({e}) — continuing unguarded");
            -1
        }
    };
    // panic = "abort" workspace-wide, so a panic is the last thing this process ever does and the
    // hook's output is the only record of it. Chain rather than replace: the default hook carries
    // the location and any RUST_BACKTRACE.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("[ocbmd] PANIC: {info}");
        prev_hook(info);
    }));
    eprintln!(
        "[ocbmd] start pid={} version={}",
        std::process::id(),
        env!("CARGO_PKG_VERSION")
    );
    // The one extra thread this daemon runs. Started here — after the singleton guard, so a losing
    // duplicate never touches the controller — and never joined; see [`BtProbe`].
    let bt = bt_probe_start();
    let acc = OpenOptions::new()
        .read(true)
        .write(true)
        .open(ACC_DEV)
        .unwrap_or_else(|e| {
            eprintln!("open {}: {}", ACC_DEV, e);
            std::process::exit(1);
        });
    let mfi = Mfi::open();
    if let Some(ref m) = mfi {
        // Warm-up (DeviceVersion) under /tmp/carplay_mfi.lock, like every other chip access. ocbmd
        // respawns on every `ocbm-host` command, so this runs far more often than "once at boot" —
        // unlocked it can land inside airplayd's HELD write-challenge/trigger/poll/read sequence and
        // corrupt both transactions. Skipped, not forced, if the lock is unavailable: a warm-up read
        // is not worth stepping on a live handshake.
        if let Some(_lock) = MfiLock::acquire() {
            let mut v = [0u8; 1];
            let _ = m.rd(0x00, &mut v);
        }
    }
    unsafe {
        // non-blocking accessory fd so a stalled host reader can never wedge the daemon
        let fd = acc.as_raw_fd();
        let fl = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
    // Local A/V seam listeners: the box session forwards video->:9001, media audio->:9002; ocbmd
    // muxes each onto its OCBM channel. Non-blocking so they slot into the poll loop.
    // A failed bind used to be dropped SILENTLY by a `filter_map(...ok())`, which is how a second
    // ocbmd could come up with a perfect control plane and no A/V at all — no log line anywhere said
    // so. Every failure is now reported, and the two seams without which the box projects nothing
    // are fatal. :9003/:9004/:9005 stay optional on purpose: the voice, metadata and cluster lanes
    // are per-deployment (a CarPlay-only or non-cluster box never opens them), and taking down main
    // projection because the cluster port is occupied would be the worse failure.
    let mut av_listeners: Vec<(TcpListener, u16)> = Vec::new();
    let mut missing_required = false;
    for (port, ch, required) in [
        (9001u16, p::CH_VIDEO, true),
        (9002u16, p::CH_MEDIA_AUDIO, true),
        (9003u16, p::CH_ALT_AUDIO, false),
        (9004u16, p::CH_METADATA, false),
        (9005u16, p::CH_ALT_VIDEO, false),
    ] {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => {
                let _ = l.set_nonblocking(true);
                av_listeners.push((l, ch));
            }
            Err(e) => {
                eprintln!("[ocbmd] bind 127.0.0.1:{port} failed: {e}");
                missing_required |= required;
            }
        }
    }
    if missing_required {
        eprintln!("[ocbmd] required A/V seam listener missing — exiting");
        std::process::exit(1);
    }
    let mut d = Daemon {
        acc,
        seq: 0,
        mfi,
        ptm: None,
        file: FileState::default(),
        eth: None,
        av_listeners,
        av_conns: Vec::new(),
        conns: HashMap::new(),
        out_hi: OutQueue::default(),
        out_console: OutQueue::default(),
        out_video: OutQueue::default(),
        out_alt_video: OutQueue::default(),
        out_audio: OutQueue::default(),
        out_log: OutQueue::default(),
        out_lo: OutQueue::default(),
        log: LogTail::default(),
        wire_owner: None,
        av_dropped: 0,
        av_backpressured: false,
        lo_dropped: 0,
        lo_capped: false,
        lo_resync: false,
        hello_cleared_conns: false,
        last_phone_check: None,
        subscribed: false,
        last_hb: None,
        present: false,
        host_name: None,
        box_health: None,
        bt,
        last_box_health_check: None,
        host_instance: None,
        rearm_deadline: None,
        present_cleared_at: None,
        phone_ident: None,
        last_phone_ident_check: None,
        host_replaced: false,
        cfg: Vec::new(),
        input_sock: None,
        input_fwd: 0,
        input_dropped: 0,
        mic_sock: None,
        mic_rx: Vec::new(),
        mic_uplink_on: false,
        mic_fwd: 0,
        rtsp_sock: None,
        phone_state: None,
        pairing_code: None,
        bt_phase: None,
        last_pairing_check: None,
        last_bt_phase_check: None,
        proj_mode: None,
        last_proj_mode_check: None,
        last_health_log: None,
    };
    let mut reasm = p::Reassembler::new();
    let mut rbuf = vec![0u8; p::HDR_LEN + p::MAX_PAYLOAD];
    let mut plbuf = vec![0u8; p::MAX_PAYLOAD];
    // Hoisted out of the Kind::Conn / Kind::AvConn dispatch arms (perf): each declared a fresh
    // ~64 KiB stack array, whose zero-init on EVERY readable wake wipes the Cortex-A7's whole L1D.
    // Reuse is safe: contents are always overwritten by the read and only buf[..n] is consumed.
    let mut connbuf = vec![0u8; p::MAX_PAYLOAD - 3];
    let mut avbuf = vec![0u8; p::MAX_PAYLOAD];
    // Initialize the presence flag to "0" at startup: `present` starts false but set_present is
    // edge-guarded, so without this an ocbmd restart could leave a stale "1" from a prior crash.
    write_flag_atomic(HOST_PRESENT_FLAG, false);
    // Drop any config left by a prior (crashed) session so airplayd never reads a stale one at idle.
    clear_cfg_file();
    // Same for a stale CT_RADIO inhibit from a crashed prior ocbmd (tmpfs, but a daemon respawn
    // without a reboot would otherwise inherit it).
    let _ = std::fs::remove_file(RADIO_OFF_FLAG);
    // Create the liveness stamp (see [`ALIVE_FILE`]) — and note File::create also freshens the mtime
    // of one left behind by a predecessor, so a new instance never inherits a stale "wedged" reading.
    if let Err(e) = File::create(ALIVE_FILE) {
        eprintln!("[ocbmd] cannot create {ALIVE_FILE} ({e}) — supervisor liveness unavailable");
    }
    let mut last_alive_touch: Option<Instant> = None;
    // Auto-reap forked children (the CONSOLE root shell) so exited console sessions don't become
    // zombies. Nothing here waitpid()s, so ignoring SIGCHLD is safe and cleaner than tracking pids.
    //
    // BUT: this makes every waitpid() in this process return ECHILD, so anything spawning a child
    // here must not depend on its exit status — no `Command::output()`, no `Command::status()`.
    // `ssp_enabled()` did, and silently answered `false` forever; see its doc comment. The two
    // Command users left (`ssp_enabled`, the `MGMT_REBOOT` spawn) both read a pipe or fire-and-forget.
    unsafe {
        libc::signal(libc::SIGCHLD, libc::SIG_IGN);
    }

    // Hoisted out of the loop (perf): the poll set is rebuilt thousands of times/sec during A/V, and
    // a fresh Vec each pass re-allocates both every wake. clear() keeps the capacity.
    let mut fds: Vec<libc::pollfd> = Vec::new();
    let mut kinds: Vec<Kind> = Vec::new();
    loop {
        // rebuild the pollfd set each pass: accessory (+POLLOUT if backlog) + PTY + live conns
        let want_out = !d.out_hi.is_empty()
            || !d.out_video.is_empty()
            || !d.out_alt_video.is_empty()
            || !d.out_audio.is_empty()
            || !d.out_console.is_empty()   // audit B3: without this, console-only backlog never arms POLLOUT → the tail freezes when unsubscribed (poll blocks -1)
            || !d.out_lo.is_empty();
        fds.clear();
        kinds.clear();
        let acc_ev = libc::POLLIN | if want_out { libc::POLLOUT } else { 0 };
        fds.push(libc::pollfd {
            fd: d.acc.as_raw_fd(),
            events: acc_ev,
            revents: 0,
        });
        kinds.push(Kind::Acc);
        if let Some(ptm) = d.ptm.as_ref() {
            fds.push(libc::pollfd {
                fd: ptm.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            kinds.push(Kind::Pty);
        }
        if let Some(fd) = d.eth {
            fds.push(libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            });
            kinds.push(Kind::Eth);
        }
        if let Some(s) = d.mic_sock.as_ref() {
            // Poll the mic seam read-side only (writes are best-effort in forward_mic) so the
            // `uplink on/off` back-channel reaches the host promptly.
            fds.push(libc::pollfd {
                fd: s.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            kinds.push(Kind::Mic);
        }
        if let Some(s) = d.rtsp_sock.as_ref() {
            // SETUP-relay seam read side: box→host RS_OPEN/RS_REQ/RS_CLOSE bytes from airplayd. The
            // write side (host→box RS_RESP) is bounded-blocking in forward_rtsp, never polled.
            fds.push(libc::pollfd {
                fd: s.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            kinds.push(Kind::Rtsp);
        }
        for (id, s) in d.conns.iter() {
            fds.push(libc::pollfd {
                fd: s.raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            kinds.push(Kind::Conn(*id));
        }
        for (idx, (l, _)) in d.av_listeners.iter().enumerate() {
            fds.push(libc::pollfd {
                fd: l.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            kinds.push(Kind::AvListen(idx));
        }
        for (idx, (s, ch, _fresh)) in d.av_conns.iter().enumerate() {
            // Backpressure each VIDEO seam on ITS OWN queue only (audit H1): pull the next chunk once THAT
            // stream has drained, so a slow pipe throttles that iPhone encoder instead of us dropping
            // P-frames (task #33) — and the cluster (:9005) can never gate the main 4K seam (:9001). Audio
            // (low-rate) is never gated so it never starves behind video.
            let gated = match *ch {
                p::CH_VIDEO => !d.out_video.is_empty(),
                p::CH_ALT_VIDEO => !d.out_alt_video.is_empty(),
                _ => false,
            };
            if gated {
                continue;
            }
            fds.push(libc::pollfd {
                fd: s.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            kinds.push(Kind::AvConn(idx));
        }
        // Bounded timeout while a host is subscribed so the heartbeat watchdog can fire without I/O,
        // OR while a presence re-arm dip is pending so presence_tick can restore the flag on an
        // otherwise-idle box (without this the poll blocks on -1 and the dip never resolves).
        // Block indefinitely only when truly idle.
        let timeout_ms = if d.subscribed || d.rearm_deadline.is_some() {
            500
        } else {
            LOG_IDLE_POLL_MS
        };
        let r = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
        if r < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue; // EINTR: benign, retry
            }
            eprintln!("[ocbmd] poll error: {err} — backing off");
            std::thread::sleep(Duration::from_millis(100)); // avoid a tight 100% CPU spin on a hard error
            continue;
        }
        let mut av_close: Vec<usize> = Vec::new();
        // Newly-accepted A/V producers are DEFERRED to after the dispatch loop: mutating `av_conns`
        // mid-pass would shift the indices the pre-built `Kind::AvConn(idx)` entries hold, misrouting one
        // stream's bytes onto another's channel during a :9005 reconnect (audit M-a).
        let mut av_new: Vec<(TcpStream, u16)> = Vec::new(); // (stream, channel); pushed as fresh below
        for i in 0..fds.len() {
            let re = fds[i].revents;
            if re & libc::POLLIN == 0 {
                // POLLIN is clear. If this is a hangup/error (POLLHUP/POLLERR/POLLNVAL), the fd is
                // PERMANENTLY ready — testing only POLLIN skips it every pass, so poll() returns
                // immediately forever = 100% CPU busy-spin that starves the USB accessory reads.
                // (QC #1243 — this actually wedged the box + our only OCBM console after a CH_CONSOLE
                // PTY detached.) Actively drop the dead fd so it leaves the pollset next rebuild.
                if re & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                    match kinds[i] {
                        Kind::Pty => d.ptm = None, // console shell exited → drop the master pty
                        Kind::Eth => {
                            if let Some(fd) = d.eth.take() {
                                unsafe { libc::close(fd) };
                            }
                        }
                        Kind::Mic => d.drop_mic_seam("poll HUP/ERR"), // ensure_mic_seam reconnects on the next tick
                        Kind::Rtsp => d.rtsp_sock = None, // ensure_rtsp_seam reconnects on the next tick
                        Kind::Conn(id) => {
                            d.conns.remove(&id);
                            d.send_ip(p::IP_CLOSE, id, &[]);
                        }
                        Kind::AvConn(idx) => av_close.push(idx),
                        Kind::AvListen(_) => {} // a listen socket hangup shouldn't occur; nothing to drop
                        Kind::Acc => {
                            // The USB accessory transport itself hung up. It can't be dropped + reopened
                            // in place, and POLLHUP stays ready forever, so continuing would log-flood +
                            // 100% CPU spin. Exit so run_ocbmd.sh (inittab respawn) restarts us with a
                            // fresh accessory fd once USB is back — the daemon's designed recovery path.
                            // (A host-app quit does NOT trigger this: it stops reading bulk but leaves the
                            // gadget bound; only a real cable/gadget teardown HUPs this fd.)
                            eprintln!("[ocbmd] accessory POLLHUP/POLLERR — USB transport gone; exiting for respawn");
                            clear_session_state_for_exit();
                            std::process::exit(1);
                        }
                    }
                }
                continue; // POLLOUT-only wake is handled by the drain() below; dead fds handled above
            }
            match kinds[i] {
                Kind::Acc => {
                    // QC 2026-07-25: the POLLHUP busy-spin guard above only fires when POLLIN is
                    // CLEAR. Many drivers assert POLLIN alongside POLLERR/POLLHUP on teardown (to
                    // unblock readers), and in that case dispatch lands here instead — where the old
                    // `unwrap_or(0)` mapped BOTH a persistent EIO and a genuine EOF to "no data": no
                    // progress, no exit, and poll returns instantly forever. That is the exact 100%-CPU
                    // wedge the guard above exists to prevent, on the one fd that matters most.
                    // Treat Ok(0) (EOF) and any non-WouldBlock error as transport death, taking the
                    // same exit-for-respawn path.
                    match d.acc.read(&mut rbuf) {
                        Ok(0) => {
                            eprintln!(
                                "[ocbmd] accessory EOF on read — USB transport gone; exiting for respawn"
                            );
                            clear_session_state_for_exit();
                            std::process::exit(1);
                        }
                        Ok(n) => {
                            reasm.push(&rbuf[..n]);
                            while let Some((ch, flags, len)) = reasm.next(&mut plbuf) {
                                d.handle(ch, flags, &plbuf[..len]); // no per-frame copy
                            }
                        }
                        // Spurious wake, or a signal-interrupted read. ocbmd installs no signal
                        // handlers today so EINTR is not expected, but treating it as transport death
                        // would be plainly wrong if that ever changes — retry on the next pass instead.
                        Err(ref e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                            ) => {}
                        Err(e) => {
                            eprintln!(
                                "[ocbmd] accessory read error ({e}) — USB transport gone; exiting for respawn"
                            );
                            clear_session_state_for_exit();
                            std::process::exit(1);
                        }
                    }
                }
                Kind::Pty => {
                    let mut t = [0u8; 4096];
                    // WouldBlock-aware (audit #5): the master is now O_NONBLOCK, so a spurious POLLIN wake
                    // can return WouldBlock — it must NOT be mistaken for EOF (the old `unwrap_or(0)` +
                    // `n==0 => drop` would have killed the console). Only a real Ok(0)/error drops it.
                    // Today the pollset guarantees `ptm` is Some when a Kind::Pty slot exists, but under
                    // panic=abort an unwrap here turns any future reorder into a whole-daemon kill — so a
                    // stale wake is skipped instead.
                    let Some(ptm) = d.ptm.as_mut() else { continue };
                    // Read into a temporary first: the borrow of d.ptm ends here, before the arms
                    // reassign d.ptm / call d.send.
                    let res = ptm.read(&mut t);
                    match res {
                        Ok(0) => d.ptm = None, // true EOF: console shell exited
                        Ok(n) => d.send(p::CH_CONSOLE, p::F_SOM | p::F_EOM, &t[..n]),
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {} // no data yet
                        Err(_) => d.ptm = None,
                    }
                }
                Kind::Eth => {
                    if let Some(fd) = d.eth {
                        let mut fb = [0u8; 2048]; // one ethernet frame (MTU 1500 + hdr)
                                                  // batch-drain the raw socket this wake; recv_frame skips our own sent frames
                        for _ in 0..256 {
                            match eth::recv_frame(fd, &mut fb) {
                                Some(n) => d.send(p::CH_ETH, p::F_SOM | p::F_EOM, &fb[..n]),
                                None => break,
                            }
                        }
                    }
                }
                Kind::Conn(id) => {
                    // MAX_PAYLOAD - 3: send_ip prepends a 3-byte sub-header (typ + id u16), so the
                    // framed payload is read.len() + 3. Reading a full MAX_PAYLOAD would produce a
                    // 65539-byte frame that the receiver's Reassembler rejects (> MAX_PAYLOAD) —
                    // silently dropping a full TCP read on this RELIABLE relay (and panicking the
                    // debug-build frame() assert). Cap the read so the framed payload fits exactly.
                    // `connbuf` is hoisted above the loop — see its comment (L1D wipe per wake).
                    // (bytes, should_close)
                    let outcome = match d.conns.get_mut(&id) {
                        Some(Conn::Tcp(s)) => match s.read(&mut connbuf) {
                            Ok(0) => (0usize, true), // TCP EOF -> close
                            Ok(n) => (n, false),
                            // audit Fix #4: a spurious level-triggered POLLIN wake can surface
                            // WouldBlock/Interrupted on a healthy relay socket — NOT connection death
                            // (the sibling UDP/AvConn/Pty/Acc arms already special-case it). Only a hard
                            // error closes.
                            Err(ref e)
                                if matches!(
                                    e.kind(),
                                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                                ) =>
                            {
                                (0usize, false)
                            }
                            Err(_) => (0usize, true), // real error -> close
                        },
                        Some(Conn::Udp(s)) => match s.recv(&mut connbuf) {
                            Ok(n) => (n, false), // datagram; UDP never auto-closes
                            Err(_) => (0, false),
                        },
                        // Already gone. The poll set was built BEFORE this pass, so the only way a
                        // `Kind::Conn` id is missing now is that something earlier in this same pass
                        // removed it — the host's own IP_CLOSE, a CT_STOP, or the HELLO clear. In
                        // every one of those the peer already knows, so closing again is dead
                        // traffic (and an id a relaunched host has since reused).
                        None => (0, false),
                    };
                    if outcome.0 > 0 {
                        d.send_ip(p::IP_DATA, id, &connbuf[..outcome.0]);
                    } else if outcome.1 {
                        d.conns.remove(&id);
                        // QC 2026-07-25 follow-up: only notify a host that is still there. A `CT_STOP`
                        // handled EARLIER IN THIS SAME dispatch pass now clears `conns` (orphaned-socket
                        // fix), so a `Kind::Conn` entry dispatched after it finds `None` -> `(0, true)`
                        // and would queue an IP_CLOSE toward a host that just STOPped. Harmless (out_lo
                        // is cleared on the next HELLO) but pure dead traffic. The heartbeat-loss clear
                        // has no such window — it runs in `presence_tick`, after the dispatch loop.
                        // ...and the HELLO-triggered clear inverts that reasoning: there `present`
                        // is TRUE (a new host just attached), so this would deliver an IP_CLOSE for a
                        // DEAD conn id to the NEW host — and the ids collide, because the host's
                        // generation counter restarts per process, so a relaunched app's first
                        // attempt reuses the same id and its transport would adopt the close.
                        if d.present && !d.hello_cleared_conns {
                            d.send_ip(p::IP_CLOSE, id, &[]);
                        }
                    }
                }
                Kind::Mic => {
                    // the mic seam's `uplink on/off` back-channel → CH_CTRL CT_UPLINK to the host
                    d.drain_mic_backchannel();
                }
                Kind::Rtsp => {
                    // box→host: chunk airplayd's relay-seam bytes onto CH_RTSP (≤64 KiB per OCBM
                    // frame; the endpoint framing has its own magic, so chunk boundaries are free).
                    // `avbuf` (MAX_PAYLOAD) is reused — arms run sequentially within a pass.
                    let res = d.rtsp_sock.as_mut().map(|s| s.read(&mut avbuf));
                    match res {
                        Some(Ok(0)) => d.rtsp_sock = None, // airplayd closed (restart) → reconnect on tick
                        Some(Ok(n)) => d.send(p::CH_RTSP, p::F_SOM | p::F_EOM, &avbuf[..n]),
                        // Spurious level-triggered wake bounded by the 50 ms SO_RCVTIMEO — not death.
                        Some(Err(ref e))
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock
                                    | std::io::ErrorKind::TimedOut
                                    | std::io::ErrorKind::Interrupted
                            ) => {}
                        Some(Err(_)) => d.rtsp_sock = None,
                        None => {}
                    }
                }
                Kind::AvListen(idx) => {
                    // accept a local A/V producer (the box session) → tag it with the target channel.
                    // DEFER the av_conns mutation to after the loop (av_new) so it can't shift the indices
                    // this pass's AvConn(idx) entries were built with (audit M-a).
                    let ch = d.av_listeners[idx].1;
                    let accepted = d.av_listeners[idx].0.accept();
                    if let Ok((s, _)) = accepted {
                        let _ = s.set_nonblocking(true);
                        av_new.push((s, ch));
                    }
                }
                Kind::AvConn(idx) => {
                    // stream the A/V bytes onto the target OCBM channel (bulk queue). Read a full
                    // MAX_PAYLOAD chunk (was 32 KB): at 4K@60 this halves the OCBM-frame + USB-write count
                    // per video frame, cutting per-frame syscall/framing overhead on the CPU-bound box.
                    // `avbuf` is hoisted above the loop — see its comment (L1D wipe per wake).
                    let ch = d.av_conns[idx].1;
                    let res = d.av_conns[idx].0.read(&mut avbuf);
                    match res {
                        Ok(0) => av_close.push(idx),
                        Ok(n) => {
                            // FIRST frame from this producer carries F_NEW_SOURCE: the previous producer
                            // on this channel was dropped without draining, so the host must discard the
                            // partial message it is still holding rather than splice this one onto it.
                            // Advisory + connection-scoped — no byte of the seam payload is inspected.
                            let mut fl = p::F_SOM | p::F_EOM;
                            if d.av_conns[idx].2 {
                                fl |= p::F_NEW_SOURCE;
                                d.av_conns[idx].2 = false;
                            }
                            d.send(ch, fl, &avbuf[..n]);
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(_) => av_close.push(idx),
                    }
                }
            }
        }
        // drop closed A/V connections (reverse index order keeps the rest valid)
        av_close.sort_unstable();
        av_close.dedup();
        for &idx in av_close.iter().rev() {
            if idx < d.av_conns.len() {
                d.av_conns.remove(idx);
            }
        }
        // Now apply the deferred accepts (indices are no longer needed). One producer per channel: drop
        // any stale prior connection on the same channel (a re-SETUP reconnects) so av_conns can't grow
        // with orphaned local sockets.
        // `true` = fresh: its first forwarded frame gets F_NEW_SOURCE (see the `av_conns` field comment).
        for (s, ch) in av_new {
            d.av_conns.retain(|(_, c, _)| *c != ch);
            d.av_conns.push((s, ch, true));
        }
        // The HELLO-cleared-conns suppression is scoped to ONE dispatch pass: by the next wake the
        // stale ids are gone from `conns`, so a genuine close for a NEW id must be delivered normally.
        d.hello_cleared_conns = false;
        // Watchdog AFTER the dispatch loop: any heartbeat that arrived in this wake has now refreshed
        // `last_hb`, so we never spuriously tear down a live session on a beat that already landed.
        d.presence_tick(Instant::now());
        // Keep the mic back-channel to airplayd connected while a session is live, so the `uplink on`
        // gate can reach the host the instant iOS opens a type-100 `input` SETUP (before any mic PCM
        // exists). Cheap refused-connect while airplayd is down; the ≤500 ms subscribed poll cadence
        // throttles the retry. See ensure_mic_seam for the deadlock this avoids.
        if d.subscribed {
            d.ensure_mic_seam();
            // SETUP-relay seam, same eager discipline — and doubly so: airplayd's per-connection
            // delegate selection reads relay::seam_up() at the moment a phone connects, and its
            // RS_OPEN fires at pair-verify. Both happen before any host→box relay byte exists, so a
            // lazy (data-triggered) connect would permanently select the plain local session.
            d.ensure_rtsp_seam();
        }
        d.drain(); // non-blocking flush (hi priority first)

        // Liveness LAST, so the stamp means "a whole dispatch pass completed", not "poll() returned".
        // The `continue` paths above (EINTR, hard poll error) deliberately skip it: a loop that only
        // ever fails poll() is exactly the wedge the supervisor should escalate on.
        let now = Instant::now();
        if last_alive_touch.is_none_or(|t| now.duration_since(t) >= ALIVE_TOUCH_INTERVAL) {
            last_alive_touch = Some(now);
            touch_alive_file();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    // -----------------------------------------------------------------------------------------
    // Harness for the SESSION/CTRL/MFI paths.
    //
    // `Daemon::send` frames into in-memory `OutQueue`s, never straight to the fd, so a Daemon over
    // /dev/null is a complete and honest test target for everything below: what the box would have
    // put on the wire is exactly what lands in `out_hi`.
    // -----------------------------------------------------------------------------------------

    /// Serializes the tests that move host presence.
    ///
    /// `HOST_PRESENT_FLAG` is ONE path for the whole process (the selftest variant of the real
    /// `/tmp/host_present`), and `cargo test` runs tests in parallel — so a test asserting the flag
    /// reads `0` races every other test whose `CT_SUBSCRIBE` writes `1`. Hold this for the duration
    /// of any test that writes or reads presence. Poisoning is ignored on purpose: the mutex guards
    /// a filesystem path, not invariants, and a panic in one test must not cascade into the rest.
    fn presence_flag_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A Daemon with no MFi chip, no listeners and a /dev/null accessory fd.
    fn td() -> Daemon {
        Daemon {
            acc: OpenOptions::new().write(true).open("/dev/null").unwrap(),
            seq: 0,
            mfi: None,
            ptm: None,
            file: FileState::default(),
            eth: None,
            av_listeners: Vec::new(),
            av_conns: Vec::new(),
            conns: HashMap::new(),
            out_hi: OutQueue::default(),
            out_console: OutQueue::default(),
            out_video: OutQueue::default(),
            out_alt_video: OutQueue::default(),
            out_audio: OutQueue::default(),
            out_log: OutQueue::default(),
            out_lo: OutQueue::default(),
            log: LogTail::default(),
            wire_owner: None,
            av_dropped: 0,
            av_backpressured: false,
            lo_dropped: 0,
            lo_capped: false,
            lo_resync: false,
            hello_cleared_conns: false,
            last_phone_check: None,
            subscribed: false,
            last_hb: None,
            present: false,
            host_name: None,
            box_health: None,
            // No thread: the tests drive the atomics directly. Seeded with a FRESH all-false sample
            // so the default fixture exercises the normal (probe-healthy) path; the staleness path
            // is reached by resetting `stamp_ms` to 0 (see `health_line_flags_a_stale_bt_probe`).
            bt: {
                let p = std::sync::Arc::new(BtProbe::new());
                p.publish(false, false, false);
                p
            },
            last_box_health_check: None,
            host_instance: None,
            rearm_deadline: None,
            present_cleared_at: None,
            phone_ident: None,
            last_phone_ident_check: None,
            host_replaced: false,
            cfg: Vec::new(),
            input_sock: None,
            input_fwd: 0,
            input_dropped: 0,
            mic_sock: None,
            mic_rx: Vec::new(),
        mic_uplink_on: false,
            mic_fwd: 0,
            rtsp_sock: None,
            phone_state: None,
            pairing_code: None,
            bt_phase: None,
            last_pairing_check: None,
            last_bt_phase_check: None,
            proj_mode: None,
            last_proj_mode_check: None,
            last_health_log: None,
        }
    }

    /// Pop everything queued on the priority (CTRL/MFI/RTSP) queue as `(channel, flags, payload)`,
    /// parsed back through a real `Reassembler` so the header the host would actually see is the
    /// thing under test.
    fn sent(d: &mut Daemon) -> Vec<(u16, u8, Vec<u8>)> {
        let mut r = p::Reassembler::new();
        r.push(&d.out_hi.buf[d.out_hi.cursor..]);
        d.out_hi.clear();
        let mut out = vec![0u8; p::MAX_PAYLOAD];
        let mut v = Vec::new();
        while let Some((ch, fl, n)) = r.next(&mut out) {
            v.push((ch, fl, out[..n].to_vec()));
        }
        v
    }

    // ---- mic-uplink gate relay ----------------------------------------------------------------

    /// The one CT_UPLINK the host actually sees, for a given back-channel line.
    fn uplink_payload(line: &str) -> Vec<u8> {
        let mut d = td();
        d.on_mic_backchannel_line(line);
        let out = sent(&mut d);
        assert_eq!(out.len(), 1, "one line must produce exactly one CTRL frame: {out:?}");
        assert_eq!(out[0].0, p::CH_CTRL);
        assert_eq!(out[0].2[0], p::CT_UPLINK);
        out[0].2.clone()
    }

    /// The CVSD/CarPlay form. Three tokens in, and the first seven payload bytes must be what they
    /// have always been — every shipped host parses this by fixed offset.
    #[test]
    fn the_uplink_gate_carries_codec_zero_when_the_seam_names_no_codec() {
        let pl = uplink_payload("uplink on 8000 1");
        assert_eq!(pl.len(), 8, "ON grew one byte");
        assert_eq!(pl[1], 1, "state = on");
        assert_eq!(u32::from_le_bytes(pl[2..6].try_into().unwrap()), 8000);
        assert_eq!(pl[6], 1, "mono");
        assert_eq!(pl[7], 0, "no fourth token means PCM, not a guess");
        // airplayd's own line, unchanged.
        assert_eq!(uplink_payload("uplink on 16000 1")[7], 0);
    }

    /// The wideband form: `uplink on 16000 1 msbc` — the app must be told to hand back mSBC packets
    /// rather than PCM, and 16 kHz alone does not say that (CarPlay's own uplink is 16 kHz PCM).
    #[test]
    fn the_uplink_gate_relays_the_msbc_codec_byte() {
        let pl = uplink_payload("uplink on 16000 1 msbc");
        assert_eq!(pl.len(), 8);
        assert_eq!(pl[1], 1);
        assert_eq!(u32::from_le_bytes(pl[2..6].try_into().unwrap()), 16000);
        assert_eq!(pl[6], 1);
        assert_eq!(pl[7], p::SEAM_CODEC_MSBC);
        assert_eq!(pl[7], 4);
    }

    /// An unknown fourth token must degrade to PCM, never gate the mic off: a host told "codec 0"
    /// captures something, a host told a codec it has no encoder for captures nothing.
    #[test]
    fn an_unknown_uplink_codec_token_degrades_to_pcm() {
        assert_eq!(uplink_payload("uplink on 16000 1 lc3")[7], 0);
        assert_eq!(uplink_payload("uplink on 16000 1 MSBC")[7], 0, "the token is lowercase on the wire");
        assert_eq!(uplink_payload("uplink on 16000 1 msbc extra")[7], p::SEAM_CODEC_MSBC);
    }

    /// OFF stays the seven-byte all-zero form byte-identically. It is the edge that STOPS a
    /// microphone, and an older host that failed to parse it would keep capturing after the call.
    #[test]
    fn the_uplink_off_edge_is_unchanged() {
        let pl = uplink_payload("uplink off");
        assert_eq!(pl, vec![p::CT_UPLINK, 0, 0, 0, 0, 0, 0]);
        assert_eq!(pl.len(), 7);
    }

    /// Lines that are not ours must not reach the host at all — the seam also carries `touch`/`cmd`.
    #[test]
    fn a_foreign_back_channel_line_sends_nothing() {
        let mut d = td();
        // NB "uplink on…" with no space is NOT in this list: `strip_prefix("uplink on")` has always
        // matched it and this change does not touch that. Nothing produces such a line, and
        // tightening the prefix here would be an unrelated behaviour change.
        for l in ["touch 1 2 3", "cmd whatever", "uplink", "uplink of", "mic 320"] {
            d.on_mic_backchannel_line(l);
        }
        assert!(sent(&mut d).is_empty());
    }

    // ---- CH_LOG universal-log tailer ----------------------------------------------------------

    /// Pop everything queued on the CH_LOG queue, parsed back into entries as the host would see
    /// them: `(source, flags, text)`. The seq/stamp are advisory and would only make these brittle.
    fn log_sent(d: &mut Daemon) -> Vec<(u8, u8, Vec<u8>)> {
        let mut r = p::Reassembler::new();
        r.push(&d.out_log.buf[d.out_log.cursor..]);
        d.out_log.clear();
        let mut out = vec![0u8; p::MAX_PAYLOAD];
        let mut v = Vec::new();
        while let Some((ch, _, n)) = r.next(&mut out) {
            assert_eq!(ch, p::CH_LOG);
            let mut off = 0;
            while off < n {
                let (e, used) = p::decode_log_entry(&out[off..n]).expect("well-formed entry");
                off += used;
                v.push((e.source, e.flags, e.text.to_vec()));
            }
            assert_eq!(off, n, "entries must tile the payload exactly");
        }
        v
    }

    /// Only the LINE entries, as strings — internal notes are separate assertions.
    fn log_lines(v: &[(u8, u8, Vec<u8>)]) -> Vec<String> {
        v.iter()
            .filter(|(s, _, _)| *s != p::LOG_SRC_INTERNAL)
            .map(|(_, _, t)| String::from_utf8_lossy(t).into_owned())
            .collect()
    }

    fn notes(v: &[(u8, u8, Vec<u8>)]) -> Vec<String> {
        v.iter()
            .filter(|(s, f, _)| *s == p::LOG_SRC_INTERNAL && *f & p::LOG_F_DROPPED == 0)
            .map(|(_, _, t)| String::from_utf8_lossy(t).into_owned())
            .collect()
    }

    /// A Daemon whose tailer follows ONE temp file, so a test never touches the real box logs.
    /// The path is leaked because `LogSource` holds a `&'static str` — a handful of bytes per test.
    fn td_log(id: u8, tag: &str) -> (Daemon, String) {
        let path: &'static str = Box::leak(
            format!("/tmp/ocbmd_logtest_{}_{tag}.log", std::process::id()).into_boxed_str(),
        );
        let _ = std::fs::remove_file(path);
        let mut d = td();
        d.log.srcs = vec![LogSource {
            id,
            path,
            fd: None,
            ino: 0,
            off: 0,
            backfill_until: 0,
            partial: Vec::new(),
            partial_clipped: false,
            dropped: 0,
            err_logged: false,
        }];
        d.log.enabled = true;
        (d, path.to_string())
    }

    /// Run one tailer pass NOW, bypassing the Instant gate (a test must not sleep 250 ms).
    fn log_tick_now(d: &mut Daemon) {
        d.log.last_check = None;
        d.log_tick(Instant::now());
    }

    fn append(path: &str, s: &str) {
        let mut f = OpenOptions::new().create(true).append(true).open(path).unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }

    #[test]
    fn split_carries_a_partial_line_to_the_next_read() {
        // The property the 8 KB read budget depends on: a line split across two reads must arrive
        // once, whole, and only after its newline — never as two half-lines.
        let (mut partial, mut clipped) = (Vec::new(), false);
        let mut got: Vec<String> = Vec::new();
        let feed = |chunk: &[u8], partial: &mut Vec<u8>, clipped: &mut bool, got: &mut Vec<String>| {
            split_log_lines(chunk, 0, partial, clipped, |line, _t, _end| {
                got.push(String::from_utf8_lossy(line).into_owned())
            });
        };
        feed(b"[a] one\n[b] tw", &mut partial, &mut clipped, &mut got);
        assert_eq!(got, ["[a] one"]);
        assert_eq!(partial, b"[b] tw");
        feed(b"o\n\n[c] three\n", &mut partial, &mut clipped, &mut got);
        assert_eq!(got, ["[a] one", "[b] two", "", "[c] three"]);
        assert!(partial.is_empty(), "a fully consumed chunk leaves no carry");
        // CRLF must not put a control character inside every entry.
        feed(b"[d] crlf\r\n", &mut partial, &mut clipped, &mut got);
        assert_eq!(got.last().unwrap(), "[d] crlf");
    }

    #[test]
    fn a_writer_with_no_newline_cannot_grow_the_carry_without_bound() {
        // The carry is the one buffer a WRITER controls the size of. A megabyte with no '\n' must
        // cost LOG_MAX_LINE of RAM, not a megabyte, and the eventual entry must say it was clipped.
        let (mut partial, mut clipped) = (Vec::new(), false);
        let mut got: Vec<(usize, bool)> = Vec::new();
        let feed = |chunk: &[u8], partial: &mut Vec<u8>, clipped: &mut bool, got: &mut Vec<(usize, bool)>| {
            split_log_lines(chunk, 0, partial, clipped, |line, t, _end| got.push((line.len(), t)));
        };
        for _ in 0..128 {
            feed(&vec![b'x'; 8192], &mut partial, &mut clipped, &mut got);
            assert!(partial.len() <= p::LOG_MAX_LINE, "carry grew to {}", partial.len());
        }
        assert!(got.is_empty(), "nothing is emitted until a newline arrives");
        feed(b"\ntail\n", &mut partial, &mut clipped, &mut got);
        assert_eq!(got, [(p::LOG_MAX_LINE, true), (4, false)]);
        assert!(!clipped, "the clip flag must not leak into the NEXT line");
    }

    #[test]
    fn enabling_streams_the_whole_file_from_offset_zero_then_follows_eof() {
        // "Backfill" is not a separate opcode: the file is small by construction, so streaming from
        // offset 0 IS everything since boot. Then it must follow, not re-send.
        let (mut d, path) = td_log(p::LOG_SRC_AIRPLAYD, "backfill");
        append(&path, "[airplayd] boot\n[airplayd] pair ok\n");
        log_tick_now(&mut d);
        let v = log_sent(&mut d);
        assert_eq!(log_lines(&v), ["[airplayd] boot", "[airplayd] pair ok"]);
        assert!(v.iter().all(|(s, _, _)| *s == p::LOG_SRC_AIRPLAYD), "entries carry their source id");

        log_tick_now(&mut d);
        assert!(log_sent(&mut d).is_empty(), "EOF with no new bytes must send nothing");

        append(&path, "[airplayd] RECORD\n");
        log_tick_now(&mut d);
        assert_eq!(log_lines(&log_sent(&mut d)), ["[airplayd] RECORD"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn lines_present_at_open_time_are_flagged_backfill_and_live_lines_are_not() {
        // The device-proven defect: a re-enable re-streamed the WHOLE file with no way to tell
        // "already happened" from "happening now". Everything up to the size at open time must
        // carry LOG_F_BACKFILL; anything appended after that, on the SAME pass or a later tick,
        // must not.
        let (mut d, path) = td_log(p::LOG_SRC_AIRPLAYD, "backfillflag");
        append(&path, "[airplayd] boot\n[airplayd] pair ok\n");
        log_tick_now(&mut d);
        let v = log_sent(&mut d);
        let lines: Vec<&(u8, u8, Vec<u8>)> = v.iter().filter(|(s, _, _)| *s == p::LOG_SRC_AIRPLAYD).collect();
        assert_eq!(lines.len(), 2);
        assert!(
            lines.iter().all(|(_, f, _)| f & p::LOG_F_BACKFILL != 0),
            "everything already on disk at open time is backfill"
        );

        append(&path, "[airplayd] RECORD\n");
        log_tick_now(&mut d);
        let v2 = log_sent(&mut d);
        let (_, f, t) = &v2.iter().find(|(s, _, _)| *s == p::LOG_SRC_AIRPLAYD).unwrap();
        assert_eq!(String::from_utf8_lossy(t), "[airplayd] RECORD");
        assert_eq!(f & p::LOG_F_BACKFILL, 0, "a line appended after open time is live");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_write_time_stamped_line_uses_its_own_clock_and_strips_the_prefix() {
        // `@<unix_ms> ` (docs/carplay/01_OCBM_PROTOCOL.md CH_LOG): a writer that knows exactly
        // when a burst happened must be able to say so, rather than every line in the burst
        // landing on the one millisecond the tailer happened to read it.
        let (mut d, path) = td_log(p::LOG_SRC_IAP2D, "stamped");
        append(&path, "@1234567890123 [iap2d] numeric-comparison code = 874736\n[iap2d] unstamped\n");
        log_tick_now(&mut d);
        let v = log_sent(&mut d);
        let lines: Vec<&(u8, u8, Vec<u8>)> = v.iter().filter(|(s, _, _)| *s == p::LOG_SRC_IAP2D).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(String::from_utf8_lossy(&lines[0].2), "[iap2d] numeric-comparison code = 874736");
        assert_eq!(String::from_utf8_lossy(&lines[1].2), "[iap2d] unstamped");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parse_log_stamp_rejects_non_convention_prefixes() {
        assert_eq!(parse_log_stamp(b"@123 rest"), Some((123, &b"rest"[..])));
        assert_eq!(parse_log_stamp(b"no-at-sign here"), None);
        assert_eq!(parse_log_stamp(b"@nodigits x"), None);
        assert_eq!(parse_log_stamp(b"@123nospace"), None);
        assert_eq!(parse_log_stamp(b"@"), None);
    }

    #[test]
    fn an_in_place_truncation_restarts_the_source_at_zero_and_says_so() {
        // `bound_logs` rewrites the per-daemon logs in place (tail -c into the SAME inode). Read as
        // a shrink, that must restart at 0 — the alternative is seeking past EOF and streaming
        // nothing for the rest of the session, which is silent and unrecoverable.
        let (mut d, path) = td_log(p::LOG_SRC_IAP2D, "shrink");
        append(&path, "[iap2d] a\n[iap2d] b\n");
        log_tick_now(&mut d);
        assert_eq!(log_lines(&log_sent(&mut d)).len(), 2);

        std::fs::write(&path, "[iap2d] c\n").unwrap(); // truncate + rewrite, same inode
        log_tick_now(&mut d);
        let v = log_sent(&mut d);
        assert_eq!(log_lines(&v), ["[iap2d] c"]);
        assert!(notes(&v).iter().any(|n| n.contains("iap2d") && n.contains("truncated externally")));
        assert_eq!(d.log.srcs[0].off, "[iap2d] c\n".len() as u64);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_tail_only_source_that_disappears_and_comes_back_resumes_at_zero() {
        // The reaped-and-recreated case. Our open fd keeps the OLD inode alive, so without the
        // identity check the source reads EOF forever against a live new file — a daemon whose log
        // was rotated away would go permanently silent to the host with no error anywhere.
        let (mut d, path) = td_log(p::LOG_SRC_AA_BRIDGE, "reap");
        append(&path, "[aa] first\n");
        log_tick_now(&mut d);
        assert_eq!(log_lines(&log_sent(&mut d)), ["[aa] first"]);

        std::fs::remove_file(&path).unwrap();
        log_tick_now(&mut d);
        let v = log_sent(&mut d);
        assert!(log_lines(&v).is_empty(), "an absent file yields no lines");
        assert!(notes(&v).iter().any(|n| n.contains("replaced or reaped")));
        assert!(d.log.srcs[0].fd.is_none(), "the stale inode must be let go");

        append(&path, "[aa] second\n"); // reappears as a NEW inode
        log_tick_now(&mut d);
        assert_eq!(log_lines(&log_sent(&mut d)), ["[aa] second"]);
        // And it keeps following the new file, rather than one-shotting it.
        append(&path, "[aa] third\n");
        log_tick_now(&mut d);
        assert_eq!(log_lines(&log_sent(&mut d)), ["[aa] third"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_cap_rotates_only_the_staged_source_and_only_once_it_is_all_streamed() {
        // (a) of the cap contract. The rotation must not eat lines the host has not seen, and it
        // must actually happen — /tmp is tmpfs, and this file is staging, not storage.
        let (mut d, path) = td_log(p::LOG_SRC_BOX, "cap");
        d.log.cap = 64;
        append(&path, &"[x] 0123456789\n".repeat(8)); // 120 B > cap
        log_tick_now(&mut d);
        let v = log_sent(&mut d);
        assert_eq!(log_lines(&v).len(), 8, "every line is streamed BEFORE the rotation");
        assert!(notes(&v).iter().any(|n| n.contains("rotated 120 bytes") && n.contains("(0 not streamed)")));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0, "the file must be emptied");
        assert_eq!(d.log.srcs[0].off, 0);

        // O_APPEND writers keep working across the ftruncate, and the tailer picks up where they
        // now write instead of leaving a hole.
        append(&path, "[x] after\n");
        log_tick_now(&mut d);
        assert_eq!(log_lines(&log_sent(&mut d)), ["[x] after"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_tail_only_source_over_the_cap_is_never_truncated() {
        // The supervisor PARSES these files (grep -q Identified, tail -1 stall checks) and owns
        // their lifecycle. Truncating one from here would race a check the session depends on.
        let (mut d, path) = td_log(p::LOG_SRC_AIRPLAYD, "notrunc");
        d.log.cap = 16;
        append(&path, &"[airplayd] line\n".repeat(20)); // 320 B, way over
        log_tick_now(&mut d);
        assert_eq!(log_lines(&log_sent(&mut d)).len(), 20);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 320, "must be left alone");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn while_disabled_the_staged_log_is_still_capped_and_marked() {
        // (b) of the cap contract, and the reason log_tick runs unsubscribed at all: an idle box
        // with no host is where this file spends most of its life growing.
        let (mut d, path) = td_log(p::LOG_SRC_BOX, "idlecap");
        d.log.enabled = false;
        d.log.cap = 32;
        append(&path, &"[x] 0123456789\n".repeat(4)); // 60 B > cap
        // The rotate path opens LOG_FILE itself, so point the test at it only if that is safe:
        // instead assert the decision, which is what this test owns.
        assert!(std::fs::metadata(&path).unwrap().len() > d.log.cap_bytes());
        log_tick_now(&mut d);
        assert!(log_sent(&mut d).is_empty(), "disabled means NOTHING goes on the wire");
        assert!(d.log.srcs[0].fd.is_none(), "and no fd is held open");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_queue_cap_drops_the_oldest_lines_and_reports_them_per_source() {
        // A stalled host must cost a REPORTED gap, not the box's RAM — and the report has to name
        // which file lost lines, or it tells the reader nothing actionable.
        let (mut d, path) = td_log(p::LOG_SRC_IAP2D, "drop");
        // Fill well past LOG_QUEUE_CAP in one tick: 8 KB read budget per tick, so pump ticks.
        let line = format!("[iap2d] {}\n", "y".repeat(200));
        append(&path, &line.repeat(600)); // ~125 KB
        for _ in 0..40 {
            log_tick_now(&mut d);
        }
        assert!(
            d.log.pending.len() + d.out_log.len() <= LOG_QUEUE_CAP,
            "the log path must stay under its cap, not grow with the backlog"
        );
        let v = log_sent(&mut d);
        // Drain what fit, then flush once more so the pending drop report is framed.
        log_tick_now(&mut d);
        let v2 = log_sent(&mut d);
        let reports: Vec<(u8, u32)> = v
            .iter()
            .chain(v2.iter())
            .filter(|(_, f, _)| *f & p::LOG_F_DROPPED != 0)
            .map(|(s, _, t)| (*s, u32::from_le_bytes(t.as_slice().try_into().unwrap())))
            .collect();
        assert!(!reports.is_empty(), "lines were dropped and never reported");
        assert!(reports.iter().all(|(s, n)| *s == p::LOG_SRC_IAP2D && *n > 0));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ct_pair_confirm_forwards_the_users_answer_to_the_wireless_daemon() {
        let _flag = presence_flag_lock(); // shares HOST_PRESENT_FLAG (CT_SUBSCRIBE) with the others
        let l = std::net::TcpListener::bind(WIRELESS_CONTROL_ADDR).expect("test control port");
        let mut d = td();
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_SUBSCRIBE]);

        let line = |l: &std::net::TcpListener| {
            let (s, _) = l.accept().unwrap();
            let mut r = std::io::BufReader::new(s);
            let mut line = String::new();
            std::io::BufRead::read_line(&mut r, &mut line).unwrap();
            line
        };

        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_PAIR_CONFIRM, 1]);
        assert_eq!(line(&l), "{\"cmd\":\"pair_answer\",\"accept\":true}\n");

        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_PAIR_CONFIRM, 0]);
        assert_eq!(line(&l), "{\"cmd\":\"pair_answer\",\"accept\":false}\n");

        // Any non-zero byte is a yes (the host writes 1; a future client must not be able to mean
        // "cancel" with 2)...
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_PAIR_CONFIRM, 2]);
        assert_eq!(line(&l), "{\"cmd\":\"pair_answer\",\"accept\":true}\n");

        // ...but a TRUNCATED request is a CANCEL, never a pair: an unparseable frame must not be
        // able to complete a bond no human confirmed.
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_PAIR_CONFIRM]);
        assert_eq!(line(&l), "{\"cmd\":\"pair_answer\",\"accept\":false}\n");
    }

    #[test]
    fn a_pair_answer_with_no_wireless_daemon_listening_is_dropped_not_fatal() {
        // The poll loop carries the host heartbeats: an absent control port must cost a bounded
        // ECONNREFUSED and nothing else. (Nothing binds the test port here.)
        let _flag = presence_flag_lock();
        let mut d = td();
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_SUBSCRIBE]);
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_PAIR_CONFIRM, 1]);
    }

    #[test]
    fn ct_log_ctl_arms_the_stream_and_ct_stop_disarms_it() {
        let _flag = presence_flag_lock(); // shares HOST_PRESENT_FLAG with every other presence test
        // Default OFF, and per-session like the cfg: a host that leaves must not leave a stream
        // running, and one that comes back re-arms and gets offset 0 again.
        let mut d = td();
        assert!(!d.log.enabled, "the default is off");
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_SUBSCRIBE]);

        let mut on = vec![p::CT_LOG_CTL, 1];
        on.extend_from_slice(&64u16.to_le_bytes());
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &on);
        assert!(d.log.enabled);
        assert_eq!(d.log.cap_bytes(), 64 * 1024);

        // cap_kb 0 means the protocol default, not "no cap".
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_LOG_CTL, 1, 0, 0]);
        assert_eq!(d.log.cap_bytes(), p::LOG_CAP_DEFAULT_KB as u64 * 1024);

        d.log.srcs[0].off = 4242; // pretend it streamed
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_STOP]);
        assert!(!d.log.enabled, "CT_STOP must disarm it");
        assert_eq!(d.log.srcs[0].off, 0, "and a re-arm restarts from offset 0");
        assert!(d.log.pending.is_empty());

        // A truncated request reads as "disable", never as "leave it running".
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &on);
        assert!(d.log.enabled);
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_LOG_CTL]);
        assert!(!d.log.enabled);
    }

    #[test]
    fn log_frames_never_exceed_the_declared_payload_bound() {
        // The host sizes its reader against LOG_MAX_FRAME; a frame over it would be a protocol lie.
        let (mut d, path) = td_log(p::LOG_SRC_BOX, "framesize");
        d.log.cap = 1 << 30; // no rotation in this test
        append(&path, &"[x] pack me\n".repeat(600));
        log_tick_now(&mut d);
        let mut r = p::Reassembler::new();
        r.push(&d.out_log.buf[d.out_log.cursor..]);
        let mut out = vec![0u8; p::MAX_PAYLOAD];
        let mut frames = 0;
        while let Some((ch, fl, n)) = r.next(&mut out) {
            assert_eq!(ch, p::CH_LOG);
            assert_eq!(fl, p::F_SOM | p::F_EOM, "every log frame is a whole message");
            assert!(n <= p::LOG_MAX_FRAME, "frame payload {n} over LOG_MAX_FRAME");
            frames += 1;
        }
        assert!(frames > 1, "8 KB of lines must pack into several frames");
        let _ = std::fs::remove_file(&path);
    }

    fn hello(inst: u32, label: &[u8]) -> Vec<u8> {
        let mut pl = vec![p::CT_HELLO, 1];
        pl.extend_from_slice(&inst.to_le_bytes());
        pl.extend_from_slice(label);
        pl
    }

    // ---- CH_MFI correlation tag (2026-08-27) --------------------------------------------------
    //
    // The reply carries no opcode echo and no request id. The tag is the ONLY thing that can tell
    // two 128-byte signature replies apart, so every reply shape has to echo it or the host falls
    // back to length correlation and can answer the wrong digest.

    #[test]
    fn mfi_error_reply_echoes_the_request_tag() {
        let mut d = td(); // mfi: None -> every op fails -> status 0x01
        d.handle_mfi(&[0x01, 0, 0, 0x5A]);
        let f = sent(&mut d);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].0, p::CH_MFI);
        assert_eq!(f[0].2, vec![0x01, 0, 0, 0x5A], "FAILED reply must carry the tag");
    }

    #[test]
    fn mfi_sign_reply_echoes_the_tag_past_the_digest() {
        let mut d = td();
        let mut req = vec![0x02, 0, 20];
        req.extend_from_slice(&[0xAB; 20]);
        req.push(0x7F);
        d.handle_mfi(&req);
        let f = sent(&mut d);
        assert_eq!(f[0].2, vec![0x01, 0, 0, 0x7F]);
    }

    #[test]
    fn mfi_unknown_opcode_reply_echoes_the_tag() {
        let mut d = td();
        d.handle_mfi(&[0x77, 0, 0, 0x33]);
        let f = sent(&mut d);
        assert_eq!(f[0].2, vec![0x02, 0, 0, 0x33], "unknown-op reply must carry the tag");
    }

    #[test]
    fn mfi_untagged_request_gets_an_untagged_reply() {
        // Wire compatibility both ways: an older host that sends no tag must see byte-for-byte the
        // reply it always saw, or its length correlation breaks.
        let mut d = td();
        d.handle_mfi(&[0x01, 0, 0]);
        assert_eq!(sent(&mut d)[0].2, vec![0x01, 0, 0]);

        let mut req = vec![0x02, 0, 20];
        req.extend_from_slice(&[0xAB; 20]);
        d.handle_mfi(&req);
        assert_eq!(sent(&mut d)[0].2, vec![0x01, 0, 0]);

        d.handle_mfi(&[0x77, 0, 0]);
        assert_eq!(sent(&mut d)[0].2, vec![0x02, 0, 0]);
    }

    #[test]
    fn mfi_short_or_lying_frame_never_panics() {
        let mut d = td();
        d.handle_mfi(&[]); // below the 3-byte header
        d.handle_mfi(&[0x01, 0]);
        d.handle_mfi(&[0x02, 0xFF, 0xFF, 1, 2, 3]); // ilen 65535, 3 bytes present
        d.handle_mfi(&[0x02, 0, 5, 1, 2]); // ilen 5, 2 bytes present
        d.handle_mfi(&[0x02, 0, 0]); // sign with a zero-length digest
    }

    // ---- F_REPLAY on the state mirrors (2026-08-27) -------------------------------------------

    #[test]
    fn mirror_flags_marks_replay_only_when_there_was_no_prior_value() {
        assert_eq!(mirror_flags(false), p::F_SOM | p::F_EOM);
        assert_eq!(mirror_flags(true), p::F_SOM | p::F_EOM | p::F_REPLAY);
        // A receiver that does not know bit2 must still see a complete single frame.
        assert_ne!(mirror_flags(true) & p::F_SOM, 0);
        assert_ne!(mirror_flags(true) & p::F_EOM, 0);
    }

    // ---- CT_HELLO: host label + replacement detection (2026-08-27) ----------------------------

    #[test]
    fn hello_records_the_optional_host_label() {
        let mut d = td();
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &hello(0x1234_5678, b"gm-ccpa head unit"));
        assert_eq!(d.host_name.as_deref(), Some("gm-ccpa head unit"));
        assert_eq!(d.host_instance, Some(0x1234_5678));
    }

    #[test]
    fn hello_without_a_label_leaves_host_name_unset() {
        let mut d = td();
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &hello(7, b""));
        assert_eq!(d.host_name, None);
    }

    #[test]
    fn hello_label_is_sanitised_and_bounded() {
        let mut d = td();
        let mut label = b"ab\ncd\x00ef".to_vec();
        label.extend(std::iter::repeat_n(b'x', 200));
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &hello(9, &label));
        let n = d.host_name.clone().unwrap();
        assert!(!n.chars().any(|c| c.is_control()), "control chars must be stripped: {n:?}");
        assert!(n.chars().count() <= 64, "label must be bounded, got {}", n.chars().count());
    }

    #[test]
    fn hello_label_survives_into_mgmt_info_json() {
        let mut d = td();
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &hello(1, b"bench tool"));
        assert!(d.box_info_json().contains("\"host_name\":\"bench tool\""));
    }

    #[test]
    fn hello_after_a_clean_stop_is_not_a_replacement() {
        let _flag = presence_flag_lock(); // shares HOST_PRESENT_FLAG with every other presence test
        // A clean CT_STOP ends the session outright, so the next host's HELLO meets an IDLE box.
        // It must not be classified as a REPLACEMENT: that latches `host_replaced`, and the
        // SUBSCRIBE after it would answer with a silent presence re-arm (wireless_down/up) on top
        // of the teardown CT_STOP already performed.
        let mut d = td();
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &hello(0xAAAA_AAAA, b""));
        // Presence is established by CT_SUBSCRIBE, not CT_HELLO; short-circuit to a live session.
        d.present = true;
        d.subscribed = true;
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_STOP]);
        assert!(!d.present, "CT_STOP must drop presence immediately — no grace");
        assert!(!d.subscribed);

        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &hello(0xBBBB_BBBB, b""));
        assert!(!d.host_replaced, "a relaunch after a clean STOP is a fresh attach, not a replacement");
        assert_eq!(d.host_instance, Some(0xBBBB_BBBB));
    }

    #[test]
    fn ct_stop_takes_the_same_teardown_path_as_a_lost_heartbeat() {
        let _flag = presence_flag_lock(); // shares HOST_PRESENT_FLAG with every other presence test
        // The behaviour this locks: a clean host close and a heartbeat-loss host-gone must leave the
        // daemon in the SAME state, because the supervisor's complete wireless teardown is driven off
        // the /tmp/host_present 1->0 edge both of them write. The only sanctioned difference is the
        // CH_CTRL SEV_HOST_GONE, which CT_STOP suppresses (the host has already detached — go_idle).
        let live = |d: &mut Daemon| {
            d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_SUBSCRIBE, b'x']);
            d.host_replaced = true; // latched state a teardown must not leave behind
            assert!(d.present && d.subscribed);
        };

        let mut stopped = td();
        live(&mut stopped);
        stopped.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_STOP]);

        let mut lost = td();
        live(&mut lost);
        lost.last_hb = Some(Instant::now() - HEARTBEAT_GRACE - Duration::from_secs(1));
        lost.presence_tick(Instant::now());

        for d in [&stopped, &lost] {
            assert!(!d.present, "presence must be down");
            assert!(!d.subscribed, "the subscription must be down");
            assert!(d.cfg.is_empty(), "the ephemeral cfg must be gone");
            assert!(!d.host_replaced, "a latched replacement must not outlive the session");
            assert!(d.rearm_deadline.is_none());
            assert!(!d.log.enabled, "the log stream must be disarmed");
            assert!(d.conns.is_empty(), "orphaned CH_IP relays must be closed");
            assert!(d.rtsp_sock.is_none() && d.mic_sock.is_none() && d.input_sock.is_none());
            assert!(d.eth.is_none());
        }
        // ...and the flag the supervisor actually reads says GONE. Safe to assert on the shared path
        // because `presence_flag_lock` is held: `lost` wrote it last, and it wrote 0.
        assert_eq!(std::fs::read_to_string(HOST_PRESENT_FLAG).unwrap().trim(), "0");
    }

    #[test]
    fn a_subscribe_racing_its_own_gone_edge_holds_the_flag_down_for_rearm_hold() {
        let _flag = presence_flag_lock(); // shares HOST_PRESENT_FLAG with every other presence test
        // THE REGRESSION THIS GUARDS: the supervisor samples /tmp/host_present at 1 Hz and acts on
        // EDGES. Now that CT_STOP tears down immediately, a scripted quit->relaunch writes 0 then 1
        // between two samples; the supervisor reads 1 -> 1, runs no teardown and no bring-up, and the
        // new host ends up subscribed against the dead session's airplayd. So a raise that lands
        // inside REARM_HOLD of the GONE edge must leave the flag at 0 and let presence_tick raise it.
        let mut d = td();
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_SUBSCRIBE, b'x']);
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_STOP]);
        let gone_at = d.present_cleared_at.expect("CT_STOP must stamp the GONE edge");

        // Relaunch 100 ms later: present + signalled at once, but the FLAG write is deferred to
        // exactly REARM_HOLD after the edge — not after the SUBSCRIBE.
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_SUBSCRIBE, b'x']);
        assert!(d.present, "the host is present immediately — only the flag waits");
        assert_eq!(d.rearm_deadline, Some(gone_at + REARM_HOLD));

        // A second SUBSCRIBE inside the hold must not re-arm: one 0->1 edge, one bring-up.
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_SUBSCRIBE, b'x']);
        assert_eq!(d.rearm_deadline, Some(gone_at + REARM_HOLD), "no double bring-up");

        // At the deadline presence_tick performs the single raise and disarms.
        d.presence_tick(gone_at + REARM_HOLD);
        assert!(d.rearm_deadline.is_none());
        assert!(d.present);
    }

    #[test]
    fn a_subscribe_long_after_the_gone_edge_raises_presence_immediately() {
        let _flag = presence_flag_lock(); // shares HOST_PRESENT_FLAG with every other presence test
        // The mirror case: the supervisor has certainly sampled the 0, so holding the flag down any
        // longer would just delay the session for nothing.
        let mut d = td();
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_SUBSCRIBE, b'x']);
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_STOP]);
        d.present_cleared_at = Some(Instant::now() - REARM_HOLD - Duration::from_secs(1));

        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_SUBSCRIBE, b'x']);
        assert!(d.present);
        assert!(d.rearm_deadline.is_none(), "an already-visible GONE edge needs no hold");
    }

    #[test]
    fn hello_from_a_new_nonce_on_a_live_session_is_a_replacement() {
        let mut d = td();
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &hello(0xAAAA_AAAA, b""));
        d.present = true;
        d.subscribed = true; // never STOPped: the predecessor died
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &hello(0xBBBB_BBBB, b""));
        assert!(d.host_replaced, "a died-without-STOP predecessor must still be detected");
    }

    #[test]
    fn hello_with_a_zero_nonce_never_triggers_replacement() {
        // 0 means "not supplied" -- older hosts opt out and must keep the old blind behaviour.
        let mut d = td();
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &hello(0xAAAA_AAAA, b""));
        d.present = true;
        d.subscribed = true;
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &hello(0, b""));
        assert!(!d.host_replaced);
        assert_eq!(d.host_instance, Some(0xAAAA_AAAA), "0 must not overwrite a real nonce");
    }

    #[test]
    fn hello_clears_the_console_queue_too_so_the_ack_is_not_spliced_mid_frame() {
        let _flag = presence_flag_lock(); // shares HOST_PRESENT_FLAG with every other presence test
        // out_console is the one queue that can be resting MID-FRAME (a `dmesg -w` flood against a
        // stalled host reader). Dropping `wire_owner` without emptying it makes the next drain start
        // at out_hi and write HELLO_ACK into the middle of that console frame -- the host's
        // reassembler swallows the ACK as payload, which is the desync this resync exists to prevent.
        let mut d = td();
        d.send(p::CH_CONSOLE, p::F_SOM | p::F_EOM, b"unread console flood");
        d.wire_owner = Some(Wire::Console);
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &hello(0xC0FF_EE00, b""));
        assert!(d.out_console.is_empty(), "a stale console frame must not outlive the HELLO resync");
        assert!(d.wire_owner.is_none());
    }

    // ---- go_idle(notify_host) (2026-08-27) ----------------------------------------------------

    #[test]
    fn go_idle_signalled_tells_the_host_it_went_away() {
        let _flag = presence_flag_lock(); // shares HOST_PRESENT_FLAG with every other presence test
        let mut d = td();
        d.present = true;
        d.subscribed = true;
        let _ = sent(&mut d);
        d.go_idle(true);
        let gone = sent(&mut d);
        assert!(
            gone.iter().any(|(ch, _, pl)| *ch == p::CH_CTRL
                && pl.as_slice() == [p::CT_SESSION_EVENT, p::SEV_HOST_GONE]),
            "heartbeat-loss teardown must emit SEV_HOST_GONE -- it is the stalled host's only cue"
        );
        assert!(!d.present && !d.subscribed);
    }

    #[test]
    fn go_idle_silent_emits_nothing_but_still_drops_presence() {
        let _flag = presence_flag_lock(); // shares HOST_PRESENT_FLAG with every other presence test
        // On clean-STOP-grace expiry the host has already detached. The frame would sit in the
        // gadget FIFO and be the FIRST thing the NEXT host reads, which reads as "the box dropped
        // us" on a link that just came up.
        let mut d = td();
        d.present = true;
        d.subscribed = true;
        let _ = sent(&mut d);
        d.go_idle(false);
        assert!(sent(&mut d).is_empty(), "silent teardown must queue no CTRL frame");
        assert!(!d.present, "presence must still drop -- the supervisor needs the GONE edge");
        assert!(!d.subscribed);
    }

    #[test]
    fn go_idle_clears_a_pending_rearm_so_presence_cannot_be_reasserted_after_teardown() {
        let _flag = presence_flag_lock(); // shares HOST_PRESENT_FLAG with every other presence test
        // `rearm_presence_silently` dips HOST_PRESENT_FLAG and arms a deadline; `presence_tick`
        // restores the flag when it elapses. If a teardown lands in between, restoring would
        // reassert presence for a host already declared GONE -- and the supervisor's L2/L3
        // escalation treats that flag as authoritative.
        for notify in [true, false] {
            let mut d = td();
            d.present = true;
            d.subscribed = true;
            d.rearm_presence_silently();
            assert!(d.rearm_deadline.is_some());

            d.go_idle(notify);
            assert!(!d.present, "notify={notify}");
            assert!(d.rearm_deadline.is_none(), "a pending re-ARM must not outlive its session");

            // Even if one somehow survived, the restore must mirror `present`, never hardcode true.
            d.rearm_deadline = Some(Instant::now() - Duration::from_secs(1));
            d.presence_tick(Instant::now());
            assert_eq!(
                std::fs::read_to_string(HOST_PRESENT_FLAG).unwrap().trim(),
                "0",
                "the flag must mirror daemon state, not the deadline"
            );
        }
    }

    #[test]
    fn go_idle_clears_the_latched_replacement_flag() {
        let _flag = presence_flag_lock(); // shares HOST_PRESENT_FLAG with every other presence test
        // host_replaced is latched at CT_HELLO and consumed only by CT_SUBSCRIBE. A host may send
        // HELLO and never subscribe, so a stale `true` could force an unwanted re-arm much later.
        for notify in [true, false] {
            let mut d = td();
            d.host_replaced = true;
            d.host_instance = Some(1);
            d.go_idle(notify);
            assert!(!d.host_replaced, "notify={notify}");
        }
    }

    // ---- CT_BOX_HEALTH (2026-08-27) -----------------------------------------------------------

    #[test]
    fn box_health_is_silent_while_unsubscribed() {
        let mut d = td();
        d.box_health_tick(Instant::now());
        assert!(sent(&mut d).is_empty());
        assert_eq!(d.box_health, None);
    }

    #[test]
    fn box_health_first_emission_is_flagged_as_a_replay_then_stays_quiet() {
        let mut d = td();
        d.subscribed = true;
        d.box_health_tick(Instant::now());
        let f = sent(&mut d);
        assert_eq!(f.len(), 1, "a fresh subscriber must be told the current health");
        assert_eq!(f[0].0, p::CH_CTRL);
        assert_eq!(f[0].2[0], p::CT_BOX_HEALTH);
        assert_eq!(f[0].2.len(), 2, "payload is [CT_BOX_HEALTH][flags]");
        assert_ne!(f[0].1 & p::F_REPLAY, 0, "the first value is a replay, not a change");

        // Throttled, and unchanged health is not re-sent.
        d.box_health_tick(Instant::now());
        assert!(sent(&mut d).is_empty());
        d.last_box_health_check = None;
        d.box_health_tick(Instant::now());
        assert!(sent(&mut d).is_empty(), "only CHANGES go on the wire after the first");
    }

    #[test]
    fn box_health_change_is_sent_without_the_replay_flag() {
        let mut d = td();
        d.subscribed = true;
        d.box_health_tick(Instant::now());
        let first = d.box_health.unwrap();
        let _ = sent(&mut d);
        d.box_health = Some(!first); // pretend the previous sample differed
        d.last_box_health_check = None;
        d.box_health_tick(Instant::now());
        let f = sent(&mut d);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].1 & p::F_REPLAY, 0, "a real change must NOT be marked replay");
        assert_eq!(f[0].1, p::F_SOM | p::F_EOM);
    }

    #[test]
    fn box_health_bits_do_not_collide() {
        let all = [
            p::BH_HCI_PRESENT, p::BH_SSP, p::BH_IAP2D, p::BH_AIRPLAYD,
            p::BH_CARPLAY_WIRELESS, p::BH_WLAN_AP, p::BH_ROOTFS_OK,
        ];
        let mut seen = 0u8;
        for b in all {
            assert_eq!(b.count_ones(), 1, "each health bit must be a single bit");
            assert_eq!(seen & b, 0, "health bits must not overlap");
            seen |= b;
        }
    }

    /// Frame a CH_FILE sub-frame exactly as the host does, push it through a real
    /// Reassembler, pop it, and hand the payload to FileState — the full wire path.
    fn feed(
        fs: &mut FileState,
        reasm: &mut p::Reassembler,
        seq: &mut u32,
        pl: &[u8],
    ) -> Option<(u8, u32, u32)> {
        let mut buf = vec![0u8; p::HDR_LEN + pl.len()];
        let n = p::frame(&mut buf, p::CH_FILE, p::F_SOM | p::F_EOM, *seq, pl);
        *seq = seq.wrapping_add(1);
        reasm.push(&buf[..n]);
        let mut out = vec![0u8; p::MAX_PAYLOAD];
        let (ch, _fl, l) = reasm.next(&mut out).expect("complete frame");
        assert_eq!(ch, p::CH_FILE);
        fs.on_frame(&out[..l])
    }

    #[test]
    fn file_push_roundtrip_content_and_mode() {
        let dir = std::env::temp_dir().join(format!("ocbmd_ft_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("iap2d").to_str().unwrap().to_string();

        let data: Vec<u8> = (0..40_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let crc = p::crc32(&data);
        let size = data.len() as u32;

        let mut fs = FileState::default();
        let mut reasm = p::Reassembler::new();
        let mut seq = 0u32;

        let mut open = vec![p::FILE_OPEN];
        open.extend_from_slice(&0o755u32.to_le_bytes());
        open.extend_from_slice(dest.as_bytes());
        assert_eq!(
            feed(&mut fs, &mut reasm, &mut seq, &open),
            Some((p::FILE_OK, 0, 0))
        );

        for part in data.chunks(9000) {
            let mut d = vec![p::FILE_DATA];
            d.extend_from_slice(part);
            assert_eq!(feed(&mut fs, &mut reasm, &mut seq, &d), None); // silent accept
        }

        let mut close = vec![p::FILE_CLOSE];
        close.extend_from_slice(&crc.to_le_bytes());
        close.extend_from_slice(&size.to_le_bytes());
        assert_eq!(
            feed(&mut fs, &mut reasm, &mut seq, &close),
            Some((p::FILE_OK, crc, size))
        );

        assert_eq!(std::fs::read(&dest).unwrap(), data); // content byte-identical
        assert!(!std::path::Path::new(&format!("{}.ocbm.part", dest)).exists()); // temp gone
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755); // exec bit set (fixes the deploy gotcha)

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn file_push_crc_mismatch_is_rejected_atomically() {
        let dir = std::env::temp_dir().join(format!("ocbmd_ftbad_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("bad").to_str().unwrap().to_string();

        let mut fs = FileState::default();
        let mut reasm = p::Reassembler::new();
        let mut seq = 0u32;

        let mut open = vec![p::FILE_OPEN];
        open.extend_from_slice(&0o644u32.to_le_bytes());
        open.extend_from_slice(dest.as_bytes());
        assert_eq!(
            feed(&mut fs, &mut reasm, &mut seq, &open),
            Some((p::FILE_OK, 0, 0))
        );

        let mut d = vec![p::FILE_DATA];
        d.extend_from_slice(b"hello world");
        feed(&mut fs, &mut reasm, &mut seq, &d);

        let mut close = vec![p::FILE_CLOSE];
        close.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes()); // wrong crc
        close.extend_from_slice(&11u32.to_le_bytes());
        assert!(matches!(
            feed(&mut fs, &mut reasm, &mut seq, &close),
            Some((p::FILE_ERR_VERIFY, _, 11))
        ));

        // a failed transfer leaves nothing behind — no dest, no temp
        assert!(!std::path::Path::new(&dest).exists());
        assert!(!std::path::Path::new(&format!("{}.ocbm.part", dest)).exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn file_push_truncated_close_is_rejected() {
        // A FILE_CLOSE too short to carry crc+size (opcode only) must be rejected as a verify
        // failure — the old (0,0) default let a malformed close verify an empty transfer as OK.
        let dir = std::env::temp_dir().join(format!("ocbmd_fttrunc_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("trunc").to_str().unwrap().to_string();

        let mut fs = FileState::default();
        let mut reasm = p::Reassembler::new();
        let mut seq = 0u32;

        let mut open = vec![p::FILE_OPEN];
        open.extend_from_slice(&0o644u32.to_le_bytes());
        open.extend_from_slice(dest.as_bytes());
        assert_eq!(
            feed(&mut fs, &mut reasm, &mut seq, &open),
            Some((p::FILE_OK, 0, 0))
        );

        // 1-byte close: no crc/size fields at all
        assert!(matches!(
            feed(&mut fs, &mut reasm, &mut seq, &[p::FILE_CLOSE]),
            Some((p::FILE_ERR_VERIFY, _, _))
        ));

        // a rejected transfer leaves nothing behind — no dest, no temp
        assert!(!std::path::Path::new(&dest).exists());
        assert!(!std::path::Path::new(&format!("{}.ocbm.part", dest)).exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- singleton pid lock -------------------------------------------------------------------

    #[test]
    fn a_second_pid_lock_on_the_same_path_is_refused() {
        // The guard has to hold against a SECOND PROCESS, but flock is per open-file-description,
        // so a second open()+flock in THIS process exercises exactly the same rejection — which is
        // what the race between ocbm_boot.sh's direct start and run_ocbmd.sh's respawn produces.
        let path = format!("/tmp/ocbmd_selftest_pid_{}.pid", std::process::id());
        std::fs::remove_file(&path).ok();

        let first = acquire_pid_lock(&path).expect("first instance must take the lock");
        let second = acquire_pid_lock(&path);
        assert!(
            matches!(&second, Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "a second instance must be refused with WouldBlock, got {second:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            std::process::id().to_string(),
            "the holder's pid must be readable by a shell supervisor"
        );

        // Release-and-reacquire is deliberately NOT asserted here: it is kernel semantics, and a
        // sibling test's `hciconfig` spawn can hold an inherited copy of this fd for the fork/exec
        // window (O_CLOEXEC closes it only at exec), which made the assertion flake.
        unsafe { libc::close(first) };
        std::fs::remove_file(&path).ok();
    }

    // ---- fresh-session A/V resync ---------------------------------------------------------------

    #[test]
    fn subscribe_clears_the_av_queues_and_releases_an_av_wire_owner() {
        let _flag = presence_flag_lock(); // CT_SUBSCRIBE writes the presence flag
        let mut d = td();
        // A previous session's tail, with the audio queue resting mid-frame (it owns the wire).
        d.out_video.buf.extend_from_slice(&[0u8; 7]);
        d.out_alt_video.buf.extend_from_slice(&[0u8; 4]);
        d.out_audio.buf.extend_from_slice(&[0u8; 5]);
        d.out_lo.buf.extend_from_slice(&[0u8; 6]);
        d.wire_owner = Some(Wire::Audio);

        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_SUBSCRIBE, b'x']);

        assert!(d.out_video.is_empty(), "stale main-video tail must not reach the new host");
        assert!(d.out_alt_video.is_empty(), "stale cluster-video tail must not reach the new host");
        assert!(d.out_audio.is_empty(), "stale audio tail would splice onto the new producer's bytes");
        assert_eq!(
            d.wire_owner, None,
            "an A/V queue that owned the wire mid-frame must release it — drain() would otherwise \
             try to finish a frame the queue no longer holds"
        );
        // SUBSCRIBE is not HELLO: the reliable bulk queue is untouched.
        assert!(!d.out_lo.is_empty(), "CT_SUBSCRIBE must not discard reliable bulk");
    }

    #[test]
    fn subscribe_leaves_a_non_av_wire_owner_alone() {
        let _flag = presence_flag_lock();
        let mut d = td();
        // out_lo resting mid-frame: SUBSCRIBE does not clear it, so it must keep the wire. Clearing
        // the owner here would let the next drain() splice another queue into its half-written frame.
        d.out_lo.buf.extend_from_slice(&[0u8; 6]);
        d.wire_owner = Some(Wire::Lo);
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_SUBSCRIBE, b'x']);
        assert_eq!(d.wire_owner, Some(Wire::Lo));
    }

    // ---- periodic health line -----------------------------------------------------------------

    #[test]
    fn health_line_reports_every_queue_and_counter() {
        let mut d = td();
        d.out_hi.buf.extend_from_slice(&[0u8; 3]);
        d.out_audio.buf.extend_from_slice(&[0u8; 5]);
        d.out_video.buf.extend_from_slice(&[0u8; 7]);
        d.out_alt_video.buf.extend_from_slice(&[0u8; 4]); // both video lanes are summed
        d.out_console.buf.extend_from_slice(&[0u8; 1]);
        d.out_log.buf.extend_from_slice(&[0u8; 2]);
        d.out_lo.buf.extend_from_slice(&[0u8; 6]);
        d.av_dropped = 9;
        d.lo_dropped = 8;
        d.input_dropped = 7;
        d.log.dropped_internal = 2;
        d.log.srcs[0].dropped = 3;
        d.box_health = Some(0x2A);
        let now = Instant::now();
        d.last_hb = Some(now - Duration::from_secs(4));
        assert_eq!(
            d.health_line(now),
            "[ocbmd] health hi=3 audio=5 video=11 console=1 log=2 lo=6 \
             dropped=av:9,lo:8,log:5,input:7 hb_age=4s bh=0x2a"
        );
    }

    // ---- bt_probe sample encoding + staleness ---------------------------------------------------

    #[test]
    fn bt_sample_packs_and_unpacks_every_combination() {
        for hci in [false, true] {
            for ssp in [false, true] {
                for valid in [false, true] {
                    assert_eq!(bt_unpack(bt_pack(hci, ssp, valid)), (hci, ssp, valid));
                }
            }
        }
        // The three bits are independent and occupy the low three, so a decode never bleeds.
        assert_eq!(bt_pack(true, false, true), 0b101);
        assert_eq!(bt_pack(false, true, false), 0b010);
    }

    #[test]
    fn bt_sample_freshness_expires_at_the_cap_and_zero_means_never() {
        // 0 = the probe thread has never published (or failed to start): permanently stale, no
        // matter how early we ask. This is the case the `+1` in mono_ms() keeps unambiguous.
        assert!(!bt_sample_fresh(0, 0));
        assert!(!bt_sample_fresh(5_000, 0));
        assert!(!bt_sample_fresh(u64::MAX, 0));

        assert!(bt_sample_fresh(1_000, 1_000)); // same instant
        assert!(bt_sample_fresh(1_000 + BT_SAMPLE_MAX_AGE_MS, 1_000)); // exactly at the cap
        assert!(!bt_sample_fresh(1_001 + BT_SAMPLE_MAX_AGE_MS, 1_000)); // one ms past it

        // A stamp from the "future" (a reader that sampled mono_ms() before the writer stored its
        // own) must read fresh, not wrap into a huge age — hence saturating_sub.
        assert!(bt_sample_fresh(1_000, 1_005));
    }

    #[test]
    fn bt_snapshot_reports_unknown_as_not_up_when_stale() {
        let p = BtProbe::new();
        p.publish(true, true, true);
        assert_eq!(p.snapshot(), (true, true, true));

        // Wedged probe thread: the last sample said "controller up, SSP on", and once it ages out
        // neither bit may still be asserted at the host.
        p.stamp_ms.store(0, std::sync::atomic::Ordering::Release);
        assert_eq!(p.snapshot(), (false, false, false));

        // ssp_valid gates the SSP bit independently of freshness: never sampled != sampled-as-off.
        p.publish(true, true, false);
        assert_eq!(p.snapshot(), (true, false, true));
    }

    #[test]
    fn health_line_flags_a_stale_bt_probe() {
        let mut d = td();
        assert!(!d.health_line(Instant::now()).contains("probe_stale"));
        d.bt.stamp_ms.store(0, std::sync::atomic::Ordering::Release);
        let l = d.health_line(Instant::now());
        assert!(l.ends_with(" bh=- probe_stale=1"), "{l}");
        // And the health mirror drops BH_HCI_PRESENT rather than reporting a remembered value.
        d.bt.publish(true, true, true);
        d.bt.stamp_ms.store(0, std::sync::atomic::Ordering::Release);
        d.subscribed = true;
        d.box_health_tick(Instant::now());
        assert_eq!(d.box_health.unwrap() & (p::BH_HCI_PRESENT | p::BH_SSP), 0);
    }

    #[test]
    fn health_line_says_dash_when_there_is_no_heartbeat_or_health_yet() {
        let d = td();
        let l = d.health_line(Instant::now());
        assert!(l.contains(" hb_age=- "), "{l}");
        assert!(l.ends_with(" bh=-"), "{l}");
    }

    #[test]
    fn health_tick_logs_once_per_interval_and_resets_between_sessions() {
        let mut d = td();
        let t0 = Instant::now();

        // Unsubscribed: never logs.
        d.health_tick(t0);
        assert_eq!(d.last_health_log, None);

        d.subscribed = true;
        d.health_tick(t0);
        assert_eq!(d.last_health_log, Some(t0), "first tick of a session logs immediately");

        // Every subsequent poll wake (thousands/sec during A/V) must be a no-op until the interval.
        d.health_tick(t0 + Duration::from_millis(1));
        d.health_tick(t0 + HEALTH_LOG_INTERVAL - Duration::from_millis(1));
        assert_eq!(d.last_health_log, Some(t0), "one line per interval, not per tick");

        let t1 = t0 + HEALTH_LOG_INTERVAL;
        d.health_tick(t1);
        assert_eq!(d.last_health_log, Some(t1));

        // Teardown clears the throttle so the next session does not wait out this one's deadline.
        d.subscribed = false;
        d.health_tick(t1 + Duration::from_millis(1));
        assert_eq!(d.last_health_log, None);
    }
}
