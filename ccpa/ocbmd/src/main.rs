//! ocbmd — box-side OCBM daemon (Rust, armv7/musl). Owns `/dev/usb_accessory` and
//! multiplexes CTRL (handshake), ECHO (loopback), CONSOLE (root PTY over bulk),
//! MFI (genuine Apple MFi 2.0C authentication bridge over `/dev/i2c-1`), IP (userspace
//! TCP/UDP mux), and FILE (verified binary deploy). See ../../docs/carplay/01_OCBM_PROTOCOL.md.

use ocbm_proto as p;
use std::collections::HashMap;
use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io::{IoSlice, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
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
                    0,
                    &mut sa as *mut _ as *mut libc::sockaddr,
                    &mut salen,
                )
            };
            if n <= 0 {
                return None; // drained / error
            }
            if sa.sll_pkttype != PACKET_OUTGOING {
                return Some(n as usize); // a real inbound frame
            }
            // else: our own transmitted frame echoed back — skip and read the next
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
    av_conns: Vec<(TcpStream, u16)>,       // (accepted stream, target OCBM channel)
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
    out_lo: OutQueue,                             // bulk output FIFO: ECHO/IP/FILE/ETH (reliable)
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
    // Clean-STOP grace (quick close/relaunch): CT_STOP arms this instead of going "gone" immediately,
    // so a fast app relaunch can re-SUBSCRIBE and REUSE the live wireless session rather than race a
    // full teardown+re-bring-up. presence_tick fires the real teardown when it elapses; a within-grace
    // SUBSCRIBE cancels it. Mirrors the grace the heartbeat-loss path already has (docs/carplay/02_SESSION_LIFECYCLE.md RECOVERING).
    stop_grace_deadline: Option<std::time::Instant>,
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
    /// the next CT_SUBSCRIBE, which owes a clean re-arm instead of a warm reuse.
    host_replaced: bool,
    /// When to restore `/tmp/host_present` to 1 after a silent re-arm. See [`REARM_HOLD`].
    rearm_deadline: Option<std::time::Instant>,
    cfg: Vec<u8>, // last host-pushed YAML config — EPHEMERAL, per session, never persisted (docs/carplay/02_SESSION_LIFECYCLE.md)
    input_sock: Option<TcpStream>, // lazy connection to airplayd's HID-input ingest (task #20)
    input_fwd: u64, // count of HID input events relayed (observability)
    input_dropped: u64, // count of HID input events dropped (bad size / no seam / failed send)
    mic_sock: Option<TcpStream>, // lazy bidirectional connection to airplayd's mic-uplink seam (CH_MIC)
    mic_rx: Vec<u8>, // partial-line buffer for the mic seam's `uplink on/off` back-channel
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
    /// SSP sampled once per session. See the note in [`Daemon::box_health_tick`].
    box_health_ssp: Option<bool>,
    /// Throttle for [`Daemon::box_health_tick`]. Reading it costs a /proc walk, so it is sampled at
    /// a much lower rate than the file-backed mirrors next to it.
    last_box_health_check: Option<std::time::Instant>,
    last_pairing_check: Option<std::time::Instant>,
    last_bt_phase_check: Option<std::time::Instant>,
    /// Last projection mode (`PM_*`) forwarded, so only CHANGES go on the wire. None = not yet read.
    proj_mode: Option<u8>,
    last_proj_mode_check: Option<std::time::Instant>,
}

/// Host-presence heartbeat grace: if a subscribed host misses beats for this long, it is declared gone.
/// Host beats ~1/s. Widened 3s→10s (QC #428): 3s is 3-5x tighter than the adapter ground truth and its
/// expiry is maximally destructive (drops the subscription + ephemeral config, forcing a full
/// re-SUBSCRIBE + session rebuild). A macOS host can miss several consecutive beats to App-Nap / a brief
/// USB stall without the session actually being dead; 10s absorbs that while still bounding a truly-gone
/// host well under any user-perceptible hang.
const HEARTBEAT_GRACE: Duration = Duration::from_secs(10);

/// Clean-exit grace: how long after a CT_STOP the box holds the wireless session warm before declaring
/// the host gone, so a quick app relaunch reuses it. 5s covers a cold app relaunch + SUBSCRIBE (which
/// can exceed the 3s a heartbeat blip would); well under the 10s the heartbeat path already tolerates.
const STOP_GRACE: Duration = Duration::from_secs(5);

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

/// Ceiling on the reliable output queues (`out_hi`/`out_lo`). Far above normal control/bulk traffic
/// (frames are small and infrequent) but far below the ~123 MB no-swap box's OOM point — a stalled host
/// reader or a console flood can't grow these without bound. The same cap also backstops the live-A/V
/// queues, where it is an OOM guard rather than a drop policy — video is gated on its own backlog so it
/// cannot reach the cap; ungated audio can, after a multi-second stall.
const OUT_QUEUE_CAP: usize = 1 << 20; // 1 MiB

/// Connect to a `host:port` target with a bounded deadline, then set the socket non-blocking so no
/// subsequent read/write on it can stall the single-threaded poll loop (#789/#834/#846). A blocking
/// connect to a dead/slow airplayd seam or an unreachable CH_IP target would otherwise wedge the whole
/// daemon (and its OCBM console) for the OS default connect timeout.
fn connect_seam(target: &str, timeout: Duration) -> Option<TcpStream> {
    let addr = target.to_socket_addrs().ok()?.next()?;
    let s = TcpStream::connect_timeout(&addr, timeout).ok()?;
    s.set_nonblocking(true).ok()?;
    Some(s)
}

/// The cross-process host-presence flag rx_connect/airplayd read to gate the session (docs/carplay/02_SESSION_LIFECYCLE.md).
const HOST_PRESENT_FLAG: &str = "/tmp/host_present";

/// The wireless SSP Numeric-Comparison code the ssp_agent publishes during pairing (absent = none).
const PAIRING_CODE_FILE: &str = "/tmp/pairing_code";
/// Written by `wireless::bt_driver::publish_bt_phase` on every iAP2 handshake transition.
const BT_PHASE_FILE: &str = "/tmp/bt_phase";

/// Phone-on-bus flag written (atomically) by session_supervisor.sh while a host is present; ocbmd
/// mirrors transitions to the host as SEV_PHONE_PRESENT/ABSENT (truthful "waiting for phone").
const PHONE_PRESENT_FLAG: &str = "/tmp/phone_present";

/// Ephemeral landing spot for the host-pushed `VehicleConfig` YAML (task #5 / docs/carplay/04_CAPABILITIES_AND_CONFIG.md). airplayd reads
/// this per control connection to build `/info`. It is written on SUBSCRIBE and removed on STOP /
/// heartbeat-loss / startup, so a config NEVER outlives its session (host-authoritative / ephemeral).
const CARPLAY_CFG_FILE: &str = "/tmp/carplay_cfg.yaml";

// --- CH_MGMT ("CCPA" tab) helpers. Dependency-free by design: direct /sys+/proc reads, no serde_json. ---
/// Persistent BR/EDR bond store (mirrors ssp_agent's `LINK_KEY_STORE`); 25-byte records, bdaddr first.
const BT_LINK_KEY_STORE: &str = "/etc/carplay/bt_link_keys";
/// Flag the supervisor watches to bounce carplay-wireless (restart-wireless / forget-device reload).
const WIRELESS_RESTART_FLAG: &str = "/tmp/wireless_restart";
// App-commanded radio inhibit (docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating): present = radios must be OFF now.
// Written/cleared ONLY from host CT_RADIO commands and the session lifecycle below (go_idle /
// fresh SUBSCRIBE / daemon startup) — this is an app-commanded surface, not an on-box lever.
// The supervisor polls it at 1 Hz alongside /tmp/host_present.
const RADIO_OFF_FLAG: &str = "/tmp/radio_off";

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
    // Bounded read on the dispatch thread (was read_to_string to EOF with no deadline — a wedged
    // hciconfig froze the whole daemon). Still deliberately NO waitpid anywhere: SIGCHLD is SIG_IGN
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
            let _ = child.kill(); // timed out — don't hang the dispatch thread
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

/// `ssp_enabled()` behind a short-TTL cache. `box_info_json` runs on the single-threaded dispatch loop
/// and is invoked per MGMT_GET_INFO (the app polling the CCPA tab); calling the raw `hciconfig` spawn —
/// up to a 2 s bounded pipe read — on every refresh can stall A/V. SSP mode is set once at Bluetooth
/// bring-up and effectively never changes at runtime, so a 30 s TTL bounds the spawn to at most once per
/// 30 s while still picking up a wireless restart within that window (audit Fix #4). Monotonic `Instant`
/// (not the box's arbitrary RTC) drives the TTL. Single-threaded dispatch is the only caller, so holding
/// the mutex across the (rare) spawn is uncontended; if a second caller is ever added, compute
/// `ssp_enabled()` outside the lock.
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
/// # Cost
///
/// One `socket()` + one `ioctl()` + one `close()` per health tick (2 s), no fork and no blocking —
/// affordable on `ocbmd`'s single-threaded dispatch loop, which also carries the MFi relay and the
/// heartbeat. Forking `hciconfig` there would not be (see the SSP note in `box_health_tick`).
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
        for r in b.chunks_exact(25) {
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
    for r in b.chunks_exact(25) {
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

    /// Full session teardown to idle: drop presence + subscription + ephemeral cfg, close the host-side
    /// relays (eth bridge, CH_IP sockets), and clear any pending clean-STOP grace. Shared by the
    /// heartbeat-loss and clean-STOP-grace-expiry paths so their teardown can't drift.
    ///
    /// `notify_host` splits the one thing the two paths must NOT share (audit 3.4). On heartbeat loss
    /// the host may be alive-but-stalled, and `SEV_HOST_GONE` is its only cue to re-subscribe — the
    /// `CT_HEARTBEAT` handler re-presents only while `subscribed`, so suppressing it there would leave
    /// the box silently dropping a live app's beats forever. On clean-STOP-grace expiry the host has,
    /// by definition of that path, already sent `CT_STOP` and detached: the frame is written into the
    /// gadget FIFO with no reader, sits there, and is the FIRST thing the NEXT host reads — observed
    /// as HELLO -> HOST_GONE -> HELLO_ACK within 8 ms, which the app reads as "the box dropped us" on
    /// a link that just came up. `CT_HELLO`'s queue-clear cannot retract it. So: go idle silently.
    fn go_idle(&mut self, notify_host: bool) {
        if notify_host {
            self.set_present(false);
        } else if self.present {
            // Same state change as `set_present(false)` — flag file included, since the supervisor
            // does need the GONE edge — minus the CH_CTRL send.
            self.present = false;
            write_flag_atomic(HOST_PRESENT_FLAG, false);
            eprintln!("[ocbmd] session: host GONE (not signalled — it sent CT_STOP and left)");
        }
        self.subscribed = false;
        self.stop_grace_deadline = None;
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
        // constants (REARM_HOLD 2s < STOP_GRACE 5s < HEARTBEAT_GRACE 10s, so the dip always resolves
        // first), but nothing enforces that ordering and raising REARM_HOLD would silently open it.
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
        self.input_sock = None;
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
        // Clean-STOP grace expiry: CT_STOP deferred going "gone" so a quick relaunch could reuse the
        // live session; no relaunch arrived in the window, so tear down now. `subscribed` is already
        // false from CT_STOP, so this MUST be OUTSIDE the `subscribed && present` gate below or it can
        // never fire — and the poll timeout is held at 500ms while a deadline is pending (see the poll
        // loop) so this tick actually runs on an otherwise-idle box.
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
        if let Some(deadline) = self.stop_grace_deadline {
            if now >= deadline {
                eprintln!("[ocbmd] session: STOP grace elapsed ({:?}) — host gone", STOP_GRACE);
                self.go_idle(false);
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
        // This now reports UP+RUNNING, not merely "the sysfs node exists" — see [`hci0_up`]. The
        // node-exists test this replaced survived `hciconfig hci0 down` (wireless_down deliberately
        // leaves the module attached), so it could not see a mid-session hci-down at all.
        if hci0_up() {
            f |= p::BH_HCI_PRESENT;
        }
        // Sampled ONCE per session, not per tick. `ssp_enabled_cached` spawns `hciconfig` behind a
        // 30 s TTL, and it lived behind a host-initiated MGMT_GET_INFO. Calling it from a 2 s tick
        // guarantees the TTL expires and the fork happens every 30 s for the life of every session —
        // on the single-threaded dispatch loop, with a 2 s stall if hciconfig ever wedges, including
        // during live A/V. SSP is configured at BT bring-up and does not change after it.
        let ssp = match self.box_health_ssp {
            Some(v) => v,
            None => {
                let v = ssp_enabled_cached();
                self.box_health_ssp = Some(v);
                v
            }
        };
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

    /// Mirror the box's single-owner arbitration flag (`/tmp/projection_owner`, docs/host/02_ANDROID_AUTO.mda) to the
    /// host as `CT_PROJ_MODE` on each change.
    ///
    /// This is the box telling the app WHICH projection transport it armed — the app cannot see the
    /// USB bus, the AOAP switch or `arm_aa`, so without it "the box is now doing Android Auto"
    /// reached the app only via the hand-set `AA_OCBM` env stand-in. On `PM_WIRED_AA` the app runs
    /// its own AA head-unit engine over CH_IP to `aa-bridge` instead of the CarPlay decode path.
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
        // so the hot A/V loop isn't burning thousands of pointless syscalls. Exception: when we have no
        // state yet (None, e.g. right after a fresh SUBSCRIBE reset it), read immediately so the host
        // learns the current state without a half-second lag.
        if self.phone_state.is_some() {
            if let Some(prev) = self.last_phone_check {
                if now_t.duration_since(prev) < Duration::from_millis(500) {
                    return;
                }
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
            0x02 if ilen > 0 && 3 + ilen <= pl.len() => {
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
                    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
                    s.connect(t).ok()?;
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
            eprintln!("[ocbmd] mic: relay write failed — dropping socket, will reconnect");
            self.mic_sock = None;
            self.mic_rx.clear();
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

    /// Drain the mic seam's readable back-channel (`uplink on <rate> <ch>` / `uplink off`, newline-framed)
    /// and re-emit each transition to the host as CH_CTRL CT_UPLINK so the app starts/stops mic capture on
    /// the real type-100 `input` SETUP edge. Called from the poll loop on a Mic POLLIN. A read EOF/error
    /// drops the socket (airplayd is per-session; it reconnects on the next CH_MIC chunk).
    fn drain_mic_backchannel(&mut self) {
        let mut tmp = [0u8; 512];
        let n = match self.mic_sock.as_mut() {
            Some(s) => match s.read(&mut tmp) {
                Ok(0) => {
                    self.mic_sock = None;
                    self.mic_rx.clear();
                    return; // airplayd closed the seam
                }
                Ok(n) => n,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(_) => {
                    self.mic_sock = None;
                    self.mic_rx.clear();
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
            if let Some(rest) = line.strip_prefix("uplink on") {
                // `uplink on <rate> <ch>` — default 16 kHz mono if the fields are missing/garbled.
                let mut it = rest.split_whitespace();
                let rate: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(16000);
                let ch: u8 = it.next().and_then(|s| s.parse().ok()).unwrap_or(1);
                let mut pl = Vec::with_capacity(7);
                pl.push(p::CT_UPLINK);
                pl.push(1); // on
                pl.extend_from_slice(&rate.to_le_bytes());
                pl.push(ch);
                self.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &pl);
                eprintln!("[ocbmd] mic: uplink ON {rate}Hz {ch}ch -> host");
            } else if line == "uplink off" {
                self.send(
                    p::CH_CTRL,
                    p::F_SOM | p::F_EOM,
                    &[p::CT_UPLINK, 0, 0, 0, 0, 0, 0],
                );
                eprintln!("[ocbmd] mic: uplink OFF -> host");
            }
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
    /// numbers, fixed daemon names) so hand-rolled JSON needs no escaping. Cheap: a few file reads, one
    /// /proc scan, one statvfs, one `hciconfig sspmode` (SSP mode has no /sys mirror).
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
        let bt_mac = read_trim("/sys/class/bluetooth/hci0/address");
        let serial = read_trim("/etc/serial_number");
        let name = bt_name_from(&wifi_mac, &serial); // hex-filtered, so a raw serial is fine here
        let transport = json_escape(&read_trim("/tmp/carplay_transport"));
        let phone = read_trim("/tmp/phone_present") == "1";
        let hci_up = std::path::Path::new("/sys/class/bluetooth/hci0").exists();
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
            ssp = ssp_enabled_cached(),
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
                    // (USB blip, client rebuilt), which can still warm-reuse a live airplayd.
                    if pl.len() >= 6 {
                        let inst = u32::from_le_bytes([pl[2], pl[3], pl[4], pl[5]]);
                        if inst != 0 {
                            // `subscribed` as well as `present`, deliberately. CT_STOP keeps
                            // `present` true for STOP_GRACE and drops only `subscribed`, so a normal
                            // relaunch inside that window is a NEW pid with a NEW nonce against a
                            // still-present box. Testing presence alone would call that a replacement
                            // and force a re-arm — destroying the warm reuse the grace exists for, and
                            // dropping a live session on a quick app restart. A predecessor that sent
                            // CT_STOP by definition did not die without one, which is the only thing
                            // this flag is for.
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
                        let label: String = String::from_utf8_lossy(&pl[6..])
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(64)
                            .collect();
                        let label = label.trim().to_string();
                        if !label.is_empty() && self.host_name.as_deref() != Some(label.as_str()) {
                            eprintln!("[ocbmd] session: host identifies as {label:?}");
                            self.host_name = Some(label);
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
                    // Built field-by-field from `zeroed()`, NOT with a struct literal. Under
                    // `musl32_time64` (required on riscv32 — see c2air/README.md) these types carry
                    // private padding fields and a literal will not compile. This form builds on
                    // every target.
                    let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
                    tv.tv_sec = secs as _;
                    let mut ok = unsafe { libc::settimeofday(&tv, std::ptr::null()) } == 0;
                    if !ok {
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
                        ts.tv_sec = secs as _;
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
                    // Cancel any pending clean-STOP teardown FIRST (a within-grace relaunch — do this
                    // before the cfg-handling below so a rapid double-relaunch can't leave a stale
                    // deadline). Reuse = the grace was pending AND presence never dropped; also note
                    // whether the new cfg differs from the one the reused airplayd already loaded.
                    let reusing = self.stop_grace_deadline.take().is_some() && self.present;
                    // A replaced host owes the same clean re-arm a cfg change does: its airplayd is gone
                    // (or bound to the dead host), and only a GONE->PRESENT edge makes the supervisor
                    // spawn a new one.
                    let replaced = std::mem::take(&mut self.host_replaced);
                    let cfg_changed = self.cfg.as_slice() != &pl[1..];
                    // host receiver active: record its ephemeral config, stamp liveness, go present
                    self.subscribed = true;
                    self.last_hb = Some(Instant::now());
                    self.phone_state = None; // re-emit current phone state to this (fresh) host
                    self.pairing_code = None; // re-emit any live pairing code to this (fresh) host
                    self.phone_ident = None; // re-emit who the phone is to this (fresh) host
                    self.bt_phase = None; // and re-emit the current BT phase, so a host attaching
                                          // mid-handshake is not blind until the next transition
                                          // (which may never come).
                    self.last_box_health_check = None; // ...and let it re-emit immediately, not up to
                                                       // 2 s later on the next throttle window
                    self.box_health_ssp = None; // re-sample SSP: a new session may have re-run bring-up
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
                        "[ocbmd] session: SUBSCRIBE ({} B config, reuse={reusing} cfg_changed={cfg_changed} replaced={replaced})",
                        self.cfg.len()
                    );
                    if (reusing && cfg_changed) || replaced {
                        // Either the settings changed during the grace (the reused airplayd still holds
                        // the OLD cfg and won't re-read it mid-session, risk-pass R3), or this is a
                        // REPLACEMENT host whose predecessor died without CT_STOP. Both owe the clean
                        // re-arm a deferred STOP would have done: airplayd re-establishes on the
                        // GONE->PRESENT edge. Silently, though — see rearm_presence_silently.
                        self.rearm_presence_silently();
                        self.send(
                            p::CH_CTRL,
                            p::F_SOM | p::F_EOM,
                            &[p::CT_SESSION_EVENT, p::SEV_HOST_PRESENT],
                        );
                    } else if self.present {
                        // Reuse (or a fast-reconnect within the heartbeat grace): confirm presence to the
                        // relaunched host — set_present's edge guard would otherwise swallow the event.
                        self.send(
                            p::CH_CTRL,
                            p::F_SOM | p::F_EOM,
                            &[p::CT_SESSION_EVENT, p::SEV_HOST_PRESENT],
                        );
                    } else {
                        self.set_present(true);
                    }
                } else if pl.first() == Some(&p::CT_HEARTBEAT) {
                    self.last_hb = Some(Instant::now());
                    if self.subscribed {
                        self.set_present(true); // recover if a prior beat had lapsed
                    }
                } else if pl.first() == Some(&p::CT_STOP) {
                    // Clean host exit — but DEFER going "gone" behind STOP_GRACE so a quick app relaunch
                    // can re-SUBSCRIBE and REUSE the live wireless session instead of racing a full
                    // teardown+re-bring-up (the quick-close/relaunch bug). `subscribed` drops now (stop
                    // projecting A/V); `present` and the cfg stay until the grace elapses so a within-grace
                    // SUBSCRIBE can cancel the teardown and compare configs (see presence_tick + CT_SUBSCRIBE).
                    // Host-side relays (eth/CH_IP) close now — a reused session re-establishes them (R6).
                    self.subscribed = false;
                    self.stop_grace_deadline = Some(Instant::now() + STOP_GRACE);
                    if let Some(fd) = self.eth.take() {
                        unsafe { libc::close(fd) };
                    }
                    // Drop the SETUP-relay seam with the other host-side relays: the host that would
                    // answer RS_REQs just STOPped, and airplayd's EOF-driven HostGone fallback is
                    // exactly the designed path. A within-grace relaunch re-establishes it on the
                    // eager tick after its SUBSCRIBE.
                    self.rtsp_sock = None;
                    self.mic_sock = None; // drop the mic + input seams with the other host-side relays (audit B3)
                    self.input_sock = None;
                    if !self.conns.is_empty() {
                        eprintln!(
                            "[ocbmd] session: closing {} orphaned CH_IP relay socket(s)",
                            self.conns.len()
                        );
                        self.conns.clear();
                    }
                    eprintln!("[ocbmd] session: STOP (holding {:?} for a quick relaunch)", STOP_GRACE);
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

fn main() {
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
        let mut v = [0u8; 1];
        let _ = m.rd(0x00, &mut v); // warm-up (DeviceVersion)
    }
    unsafe {
        // non-blocking accessory fd so a stalled host reader can never wedge the daemon
        let fd = acc.as_raw_fd();
        let fl = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
    // Local A/V seam listeners: the box session forwards video->:9001, media audio->:9002; ocbmd
    // muxes each onto its OCBM channel. Non-blocking so they slot into the poll loop.
    let av_listeners: Vec<(TcpListener, u16)> = [
        (9001u16, p::CH_VIDEO),
        (9002u16, p::CH_MEDIA_AUDIO),
        (9003u16, p::CH_ALT_AUDIO),
        (9004u16, p::CH_METADATA),
        (9005u16, p::CH_ALT_VIDEO),
    ]
    .iter()
    .filter_map(|&(port, ch)| {
        TcpListener::bind(("127.0.0.1", port)).ok().map(|l| {
            let _ = l.set_nonblocking(true);
            (l, ch)
        })
    })
    .collect();
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
        out_lo: OutQueue::default(),
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
        stop_grace_deadline: None,
        host_name: None,
        box_health: None,
        box_health_ssp: None,
        last_box_health_check: None,
        host_instance: None,
        rearm_deadline: None,
        phone_ident: None,
        last_phone_ident_check: None,
        host_replaced: false,
        cfg: Vec::new(),
        input_sock: None,
        input_fwd: 0,
        input_dropped: 0,
        mic_sock: None,
        mic_rx: Vec::new(),
        mic_fwd: 0,
        rtsp_sock: None,
        phone_state: None,
        pairing_code: None,
        bt_phase: None,
        last_pairing_check: None,
        last_bt_phase_check: None,
        proj_mode: None,
        last_proj_mode_check: None,
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
    // Auto-reap forked children (the CONSOLE root shell) so exited console sessions don't become
    // zombies. Nothing here waitpid()s, so ignoring SIGCHLD is safe and cleaner than tracking pids.
    //
    // BUT: this makes every waitpid() in this process return ECHILD, so anything spawning a child
    // here must not depend on its exit status — no `Command::output()`, no `Command::status()`.
    // `ssp_enabled()` did, and silently answered `false` forever; see its doc comment. The two
    // Command users left (`ssp_enabled`, the reboot at :1532) both read a pipe or fire-and-forget.
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
        for (idx, (s, ch)) in d.av_conns.iter().enumerate() {
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
        // OR while a clean-STOP grace is pending so presence_tick can fire its deferred teardown on an
        // otherwise-idle box (without this the poll blocks on -1 and the grace never expires — the
        // real-exit case, masked in a naive quick-relaunch test where the inbound SUBSCRIBE wakes poll).
        // Block indefinitely only when truly idle.
        let timeout_ms = if d.subscribed || d.stop_grace_deadline.is_some() || d.rearm_deadline.is_some() {
            500
        } else {
            -1
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
        let mut av_new: Vec<(TcpStream, u16)> = Vec::new();
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
                        Kind::Mic => d.mic_sock = None, // ensure_mic_seam reconnects on the next tick
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
                        None => (0, true),
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
                        Ok(n) => d.send(ch, p::F_SOM | p::F_EOM, &avbuf[..n]),
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
        for (s, ch) in av_new {
            d.av_conns.retain(|(_, c)| *c != ch);
            d.av_conns.push((s, ch));
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
            out_lo: OutQueue::default(),
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
            stop_grace_deadline: None,
            host_name: None,
            box_health: None,
            box_health_ssp: None,
            last_box_health_check: None,
            host_instance: None,
            rearm_deadline: None,
            phone_ident: None,
            last_phone_ident_check: None,
            host_replaced: false,
            cfg: Vec::new(),
            input_sock: None,
            input_fwd: 0,
            input_dropped: 0,
            mic_sock: None,
            mic_rx: Vec::new(),
            mic_fwd: 0,
            rtsp_sock: None,
            phone_state: None,
            pairing_code: None,
            bt_phase: None,
            last_pairing_check: None,
            last_bt_phase_check: None,
            proj_mode: None,
            last_proj_mode_check: None,
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
    fn hello_inside_stop_grace_is_not_a_replacement() {
        // THE REGRESSION THIS GUARDS: CT_STOP holds `present` for STOP_GRACE and drops only
        // `subscribed`, so a normal relaunch inside that window is a new pid with a new nonce
        // against a still-present box. Classifying that as a replacement forces a silent presence
        // re-arm, which the supervisor answers with wireless_down/up -- a dropped live session.
        let mut d = td();
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &hello(0xAAAA_AAAA, b""));
        // Presence is established by CT_SUBSCRIBE, not CT_HELLO; short-circuit to a live session.
        d.present = true;
        d.subscribed = true;
        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_STOP]);
        assert!(d.present, "CT_STOP must hold presence for the grace");
        assert!(!d.subscribed);

        d.handle(p::CH_CTRL, p::F_SOM | p::F_EOM, &hello(0xBBBB_BBBB, b""));
        assert!(!d.host_replaced, "a within-grace relaunch is a warm reuse, not a replacement");
        assert_eq!(d.host_instance, Some(0xBBBB_BBBB));
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

    // ---- go_idle(notify_host) (2026-08-27) ----------------------------------------------------

    #[test]
    fn go_idle_signalled_tells_the_host_it_went_away() {
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
}
