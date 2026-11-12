//! The tunnel half of airplayd: ocbmd's media seams (one TCP connection per lane), the per-stream
//! forwarders that put the iPhone's still-encrypted packets on them, and the session state / event
//! publisher. Nothing in this file decrypts media: the only key use is copying a stream's 32-byte key
//! into the lane's key message.
//!
//! Lanes (docs/ocbm/00_PROTOCOL.md, Media transport and AirPlay-only mode):
//!   :9001 CH_VIDEO        type 110   key message + frame messages (seq = the nonce counter)
//!   :9002 CH_MEDIA_AUDIO  type 96 / 103   SEAM_KEY + SEAM_FORMAT + SEAM_PKT, all scid-tagged
//!   :9004 CH_METADATA     META_JSON events `{"kind":"airplay",...}` and META_ARTWORK

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ocbm_proto as p;
use ocbm_proto::seam;

use crate::lg;

/// The largest screen frame body accepted from the phone. Measured frames are tens of KiB; the bound
/// only stops a corrupt length from allocating the box's RAM.
pub const MAX_FRAME: usize = 8 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(250);

// ---------------------------------------------------------------- lanes

pub const PRE_KEY: u8 = 0;
pub const PRE_FORMAT: u8 = 1;
pub const PRE_CONFIG: u8 = 2;

struct LaneSock {
    sock: Option<TcpStream>,
    failures: u64,
}

/// One seam = one TCP connection to ocbmd (it keeps ONE producer per channel and drops the previous
/// one when another connects), shared by every stream of the lane. A message is written whole under
/// the socket lock, so two streams can never interleave inside one.
pub struct Lane {
    addr: String,
    /// `None` = writes block (video: ocbmd stops reading its seam while the host is behind, which is the
    /// backpressure that slows the phone's encoder instead of losing a frame). `Some` = give up after
    /// this long and reconnect (audio, metadata: late is worthless, and a control thread must not hang).
    write_timeout: Option<Duration>,
    sock: Mutex<LaneSock>,
    /// What a fresh seam connection must be told before any packet: each live stream's key message,
    /// its format (audio) and its codec configuration frame (video). Re-sent on every (re)connect,
    /// because ocbmd marks a new producer `F_NEW_SOURCE` and the host drops what it held.
    /// Its OWN lock, never held across a write: a control thread registers and forgets streams here
    /// while a forwarder may be blocked in a video write for as long as the host is behind.
    /// Lock order: `sock`, then `preamble`.
    preamble: Mutex<Vec<(u64, u8, Vec<u8>)>>,
}

impl Lane {
    pub fn new(addr: &str, write_timeout: Option<Duration>) -> Self {
        Lane {
            addr: addr.to_string(),
            write_timeout,
            sock: Mutex::new(LaneSock { sock: None, failures: 0 }),
            preamble: Mutex::new(Vec::new()),
        }
    }

    fn alive(s: &TcpStream) -> bool {
        // ocbmd never writes to a producer: a readable socket is its FIN or a reset.
        let mut b = [0u8; 1];
        let r = unsafe { libc::recv(s.as_raw_fd(), b.as_mut_ptr().cast(), 1, libc::MSG_PEEK | libc::MSG_DONTWAIT) };
        r != 0 && !(r < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::WouldBlock)
    }

    /// Returns `(writable, fresh)`: `fresh` = this call connected and already wrote the preamble.
    fn ensure(&self, g: &mut LaneSock) -> (bool, bool) {
        if g.sock.as_ref().is_some_and(|s| !Self::alive(s)) {
            g.sock = None;
        }
        if g.sock.is_some() {
            return (true, false);
        }
        let Ok(addr) = self.addr.parse::<SocketAddr>() else { return (false, false) };
        let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
            g.failures += 1;
            if g.failures == 1 || g.failures % 500 == 0 {
                lg!("seam {}: not reachable ({} failed attempts): dropping what was meant for it", self.addr, g.failures);
            }
            return (false, false);
        };
        let _ = s.set_nodelay(true);
        let _ = s.set_write_timeout(self.write_timeout);
        let pre: Vec<u8> = self.preamble.lock().unwrap_or_else(|e| e.into_inner()).iter().flat_map(|(_, _, m)| m.iter().copied()).collect();
        if s.write_all(&pre).is_err() {
            return (false, false);
        }
        if g.failures > 0 {
            lg!("seam {}: connected again after {} failed attempts", self.addr, g.failures);
            g.failures = 0;
        }
        g.sock = Some(s);
        (true, true)
    }

    fn write(g: &mut LaneSock, parts: &[&[u8]]) -> bool {
        let Some(s) = g.sock.as_mut() else { return false };
        for part in parts {
            if s.write_all(part).is_err() {
                // A message may be half written: only a new connection (F_NEW_SOURCE) re-frames the host.
                g.sock = None;
                return false;
            }
        }
        true
    }

    /// One whole message. `false` = not delivered (seam down or the write failed).
    pub fn send(&self, parts: &[&[u8]]) -> bool {
        let mut g = self.sock.lock().unwrap_or_else(|e| e.into_inner());
        if !self.ensure(&mut g).0 {
            return false;
        }
        Self::write(&mut g, parts)
    }

    /// Remember a message every later seam connection must see first (key, format, config). No I/O.
    pub fn register(&self, scid: u64, kind: u8, msg: Vec<u8>) {
        let mut pre = self.preamble.lock().unwrap_or_else(|e| e.into_inner());
        pre.retain(|(s, k, _)| !(*s == scid && *k == kind));
        pre.push((scid, kind, msg));
    }

    /// [`Lane::register`], then send it now. (If another thread reconnects in between, the host sees
    /// the message twice; every preamble message is idempotent.)
    pub fn announce(&self, scid: u64, kind: u8, msg: Vec<u8>) -> bool {
        self.register(scid, kind, msg.clone());
        let mut g = self.sock.lock().unwrap_or_else(|e| e.into_inner());
        match self.ensure(&mut g) {
            (false, _) => false,
            (true, true) => true, // the connect just wrote it with the rest of the preamble
            (true, false) => Self::write(&mut g, &[&msg]),
        }
    }

    /// Never blocks on a write in progress.
    pub fn forget(&self, scid: u64) {
        self.preamble.lock().unwrap_or_else(|e| e.into_inner()).retain(|(s, _, _)| *s != scid);
    }
}

// ---------------------------------------------------------------- state + events

pub fn jesc(v: &str) -> String {
    let mut o = String::with_capacity(v.len() + 2);
    for c in v.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// Everything the forwarders and the control connections share.
pub struct Tunnel {
    pub video: Lane,
    pub audio: Lane,
    pub meta: Lane,
    /// `/tmp/airplay_state`, read by ocbmd (CT_AIRPLAY_STATE, HELLO_ACK, MGMT_INFO). None in tests.
    state_path: Option<std::path::PathBuf>,
    /// Control connections that completed a session SETUP and are still open.
    sessions: AtomicU32,
    /// Media streams (110/96/103) currently set up.
    streams: AtomicU32,
    published: Mutex<(u8, u8)>,
}

impl Tunnel {
    pub fn new(video: &str, audio: &str, meta: &str, state_path: Option<std::path::PathBuf>) -> Self {
        Tunnel {
            video: Lane::new(video, None),
            audio: Lane::new(audio, Some(Duration::from_secs(2))),
            meta: Lane::new(meta, Some(Duration::from_secs(2))),
            state_path,
            sessions: AtomicU32::new(0),
            streams: AtomicU32::new(0),
            published: Mutex::new((p::AIRPLAY_SESSION_NONE, p::AIRPLAY_END_NONE)),
        }
    }

    /// One `{"kind":"airplay","ev":...}` object on CH_METADATA. `fields` is the rest of the object,
    /// already JSON (`"a":1,"b":"x"`), or empty.
    pub fn event(&self, ev: &str, fields: &str) {
        let sep = if fields.is_empty() { "" } else { "," };
        let j = format!("{{\"kind\":\"airplay\",\"ev\":\"{ev}\"{sep}{fields}}}");
        self.meta.send(&[&seam::meta_msg(p::META_JSON, j.as_bytes())]);
    }

    pub fn artwork(&self, jpeg: &[u8]) {
        let mut pl = Vec::with_capacity(1 + jpeg.len());
        pl.push(0); // artwork id, as the CarPlay metadata plane frames it
        pl.extend_from_slice(jpeg);
        self.meta.send(&[&seam::meta_msg(p::META_ARTWORK, &pl)]);
    }

    fn current(&self) -> u8 {
        if self.streams.load(Ordering::SeqCst) > 0 {
            p::AIRPLAY_SESSION_STREAMING
        } else if self.sessions.load(Ordering::SeqCst) > 0 {
            p::AIRPLAY_SESSION_CONNECTED
        } else {
            p::AIRPLAY_SESSION_WAITING
        }
    }

    /// Recompute the session state and publish it when it (or the end reason) changed.
    pub fn publish(&self, end: Option<u8>) {
        let mut g = self.published.lock().unwrap_or_else(|e| e.into_inner());
        let now = (self.current(), end.unwrap_or(g.1));
        if *g == now {
            return;
        }
        *g = now;
        if let Some(path) = &self.state_path {
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, format!("session={}\nend={}\n", now.0, now.1)).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
        lg!("session state: {} (last end: {})", p::airplay_session_name(now.0), p::airplay_end_name(now.1));
        self.event(
            "session",
            &format!("\"state\":\"{}\",\"reason\":\"{}\"", p::airplay_session_name(now.0), p::airplay_end_name(now.1)),
        );
    }

    pub fn session_opened(&self) {
        self.sessions.fetch_add(1, Ordering::SeqCst);
        self.publish(None);
    }

    pub fn session_closed(&self, end: u8) {
        self.sessions.fetch_sub(1, Ordering::SeqCst);
        self.publish(Some(end));
    }
}

// ---------------------------------------------------------------- streams

#[derive(Default)]
pub struct Stat {
    pub msgs: AtomicU64,
    pub bytes: AtomicU64,
    pub dropped: AtomicU64,
}

/// A stream the phone set up. Dropping the sockets is the forwarder thread's job: it sees `stop`
/// within [`POLL`] and returns.
pub struct Stream {
    pub ty: u64,
    pub scid: u64,
    stop: Arc<AtomicBool>,
    stat: Arc<Stat>,
    counted: bool,
    video: bool,
    done: bool,
}

impl Stream {
    /// Idempotent. Emits the stop event and drops the stream's keys from the lane preamble.
    pub fn stop(&mut self, t: &Tunnel, reason: &str) {
        if std::mem::replace(&mut self.done, true) {
            return;
        }
        self.stop.store(true, Ordering::SeqCst);
        if self.video { &t.video } else { &t.audio }.forget(self.scid);
        t.event(
            "stream",
            &format!(
                "\"state\":\"stop\",\"type\":{},\"scid\":{},\"reason\":\"{}\",\"messages\":{},\"bytes\":{},\"dropped\":{}",
                self.ty, self.scid, jesc(reason), self.stat.msgs.load(Ordering::Relaxed),
                self.stat.bytes.load(Ordering::Relaxed), self.stat.dropped.load(Ordering::Relaxed)
            ),
        );
        lg!(
            "stream type {} scid {} stopped ({reason}): {} messages, {} B forwarded, {} dropped",
            self.ty, self.scid, self.stat.msgs.load(Ordering::Relaxed), self.stat.bytes.load(Ordering::Relaxed),
            self.stat.dropped.load(Ordering::Relaxed)
        );
        if self.counted {
            t.streams.fetch_sub(1, Ordering::SeqCst);
            t.publish(None);
        }
    }
}

pub fn keepalive(s: &TcpStream) {
    // A phone that walks out of Wi-Fi range sends no FIN. Keepalive is protocol-neutral (no
    // application timeout is known to be safe: docs/ocbm/02 §4.9) and ends a dead peer in ~25 s.
    let fd = s.as_raw_fd();
    let set = |level, opt, v: libc::c_int| unsafe {
        libc::setsockopt(fd, level, opt, (&v as *const libc::c_int).cast(), std::mem::size_of::<libc::c_int>() as libc::socklen_t);
    };
    set(libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1);
    #[cfg(target_os = "linux")]
    {
        set(libc::IPPROTO_TCP, libc::TCP_KEEPIDLE, 10);
        set(libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, 5);
        set(libc::IPPROTO_TCP, libc::TCP_KEEPCNT, 3);
    }
}

/// Fill `buf` from a socket with a [`POLL`] read timeout. `false` = end of stream, error, or `stop`.
fn read_full(s: &mut TcpStream, buf: &mut [u8], stop: &AtomicBool) -> bool {
    let mut got = 0;
    while got < buf.len() {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        match s.read(&mut buf[got..]) {
            Ok(0) => return false,
            Ok(n) => got += n,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {}
            Err(_) => return false,
        }
    }
    true
}

/// Accept data connections on `l` until `stop`, running `run` on each (one at a time: a stream has
/// one data connection; a second one replaces a first that ended).
fn accept_loop(l: TcpListener, stop: Arc<AtomicBool>, mut run: impl FnMut(TcpStream) + Send + 'static) {
    let _ = l.set_nonblocking(true);
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match l.accept() {
                Ok((s, _)) => {
                    let _ = s.set_nonblocking(false);
                    let _ = s.set_read_timeout(Some(POLL));
                    keepalive(&s);
                    run(s);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => break,
            }
        }
    });
}

fn new_stream(t: &Tunnel, ty: u64, scid: u64, video: bool) -> Stream {
    t.streams.fetch_add(1, Ordering::SeqCst);
    Stream { ty, scid, stop: Arc::new(AtomicBool::new(false)), stat: Arc::new(Stat::default()), counted: true, video, done: false }
}

/// Type 110 (screen, TCP). Each frame is `[128-byte header][body]`; it goes out as one video-lane
/// frame message whose `seq` is the count of opcode-0 frames before it on this data connection: the
/// phone's nonce counter, which only an in-order reader of the whole stream can know. Opcode 1 (the
/// plaintext avcC) is kept and replayed to a seam that reconnects. Returns the stream and its data port.
pub fn start_screen(t: &Arc<Tunnel>, scid: u64, key: [u8; 32]) -> Option<(Stream, u16)> {
    let l = TcpListener::bind("0.0.0.0:0").ok()?;
    let port = l.local_addr().ok()?.port();
    let st = new_stream(t, 110, scid, true);
    // Register only: this runs on the control thread, and a video-lane write may block for as long
    // as the host is behind. The forwarder below does every write on that lane.
    t.video.register(scid, PRE_KEY, seam::key_msg(&key, scid).to_vec());
    let (t, stop, stat) = (t.clone(), st.stop.clone(), st.stat.clone());
    accept_loop(l, stop.clone(), move |mut s| {
        // A new data connection starts the counter again: say so with a fresh key message.
        t.video.announce(scid, PRE_KEY, seam::key_msg(&key, scid).to_vec());
        let mut counter = 0u64;
        let mut hdr = [0u8; seam::SCREEN_HDR_LEN];
        let mut body = Vec::new();
        let why = loop {
            if !read_full(&mut s, &mut hdr, &stop) {
                break "data connection ended";
            }
            let n = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as usize;
            if n > MAX_FRAME {
                break "implausible frame size";
            }
            body.resize(n, 0);
            if !read_full(&mut s, &mut body, &stop) {
                break "data connection ended mid-frame";
            }
            let prefix = seam::video_frame_prefix(counter, &hdr, n);
            let ok = if hdr[4] == 1 {
                let mut m = prefix.to_vec();
                m.extend_from_slice(&body);
                t.video.announce(scid, PRE_CONFIG, m)
            } else {
                t.video.send(&[&prefix, &body])
            };
            if hdr[4] == 0 {
                counter += 1;
            }
            if ok {
                stat.msgs.fetch_add(1, Ordering::Relaxed);
                stat.bytes.fetch_add((128 + n) as u64, Ordering::Relaxed);
            } else {
                stat.dropped.fetch_add(1, Ordering::Relaxed);
            }
        };
        lg!("type 110 scid {scid}: {why} after {counter} video frames");
    });
    Some((st, port))
}

/// What a `SEAM_FORMAT` says about an audio stream.
#[derive(Clone, Copy)]
pub struct AudioFormat {
    pub codec: u8,
    pub rate: u32,
    pub ch: u8,
}

fn announce_audio(t: &Tunnel, scid: u64, key: &[u8; 32], f: AudioFormat) {
    t.audio.announce(scid, PRE_KEY, seam::key_msg(key, scid).to_vec());
    t.audio.announce(scid, PRE_FORMAT, seam::audio_format_msg(scid, f.codec, f.rate, f.ch, 0, 0).to_vec());
}

fn send_audio(t: &Tunnel, scid: u64, pkt: &[u8], stat: &Stat) {
    let mut m = Vec::with_capacity(seam::AUDIO_PKT_PREFIX_LEN + pkt.len());
    m.extend_from_slice(&seam::audio_pkt_prefix(scid, pkt.len()));
    m.extend_from_slice(pkt);
    if t.audio.send(&[&m]) {
        stat.msgs.fetch_add(1, Ordering::Relaxed);
        stat.bytes.fetch_add(pkt.len() as u64, Ordering::Relaxed);
    } else {
        stat.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

/// `[RTP 12][ciphertext >= 0][tag 16][nonce 8]`.
const MIN_AUDIO_PKT: usize = 12 + 16 + 8;

/// Remembers the last 64 RTP sequence numbers of a realtime stream: the phone sends every packet
/// three times (`redundantAudio 2`) and the USB link loses none, so one copy is forwarded.
pub struct Dedup {
    seen: [u32; 64],
    next: usize,
}

impl Default for Dedup {
    fn default() -> Self {
        Dedup { seen: [u32::MAX; 64], next: 0 }
    }
}

impl Dedup {
    /// `true` = first time this sequence number is seen in the window.
    pub fn fresh(&mut self, seq: u16) -> bool {
        if self.seen.contains(&u32::from(seq)) {
            return false;
        }
        self.seen[self.next] = u32::from(seq);
        self.next = (self.next + 1) % self.seen.len();
        true
    }
}

/// Type 96 (realtime audio, UDP data + UDP control). Datagrams are forwarded verbatim as `SEAM_PKT`,
/// first copy of each sequence number only. The control port's ~1/s sync packets (RTP time against
/// the phone's network clock) go out as `sync` events. Returns the stream, data port, control port.
pub fn start_realtime(t: &Arc<Tunnel>, scid: u64, key: [u8; 32], f: AudioFormat) -> Option<(Stream, u16, u16)> {
    let data = UdpSocket::bind("0.0.0.0:0").ok()?;
    let ctrl = UdpSocket::bind("0.0.0.0:0").ok()?;
    let (dp, cp) = (data.local_addr().ok()?.port(), ctrl.local_addr().ok()?.port());
    let _ = data.set_read_timeout(Some(POLL));
    let _ = ctrl.set_read_timeout(Some(POLL));
    let st = new_stream(t, 96, scid, false);
    announce_audio(t, scid, &key, f);
    let (t2, stop, stat) = (t.clone(), st.stop.clone(), st.stat.clone());
    std::thread::spawn(move || {
        let mut buf = [0u8; 2048];
        let mut dd = Dedup::default();
        while !stop.load(Ordering::Relaxed) {
            let n = match data.recv_from(&mut buf) {
                Ok((n, _)) => n,
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => continue,
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
            };
            if n < MIN_AUDIO_PKT || !dd.fresh(u16::from_be_bytes([buf[2], buf[3]])) {
                continue;
            }
            send_audio(&t2, scid, &buf[..n], &stat);
        }
    });
    let (t3, stop) = (t.clone(), st.stop.clone());
    std::thread::spawn(move || {
        let mut buf = [0u8; 256];
        while !stop.load(Ordering::Relaxed) {
            let n = match ctrl.recv_from(&mut buf) {
                Ok((n, _)) => n,
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => continue,
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
            };
            if n == 20 && buf[1] == 0xd4 {
                // [80|90 d4 00 04][rtpTime - latency u32][network time 32.32][rtpTime u32], big-endian
                let u = |o: usize| u32::from_be_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]);
                t3.event(
                    "sync",
                    &format!(
                        "\"type\":96,\"scid\":{scid},\"first\":{},\"rtpPlay\":{},\"netSecs\":{},\"netFrac\":{},\"rtpNow\":{}",
                        buf[0] & 0x10 != 0, u(4), u(8), u(12), u(16)
                    ),
                );
            }
        }
    });
    Some((st, dp, cp))
}

/// Type 103 (buffered audio, TCP data `[u16 BE len, counting itself][packet]` + a UDP control port
/// nothing was ever seen on). Each packet is forwarded as `SEAM_PKT` without its length prefix: the
/// same `[RTP 12][ciphertext][tag 16][nonce 8]` shape as type 96, RTP sequence 24 bits wide.
pub fn start_buffered(t: &Arc<Tunnel>, scid: u64, key: [u8; 32], f: AudioFormat) -> Option<(Stream, u16, u16)> {
    let l = TcpListener::bind("0.0.0.0:0").ok()?;
    let ctrl = UdpSocket::bind("0.0.0.0:0").ok()?;
    let (dp, cp) = (l.local_addr().ok()?.port(), ctrl.local_addr().ok()?.port());
    let st = new_stream(t, 103, scid, false);
    announce_audio(t, scid, &key, f);
    let (t, stop, stat) = (t.clone(), st.stop.clone(), st.stat.clone());
    accept_loop(l, stop.clone(), move |mut s| {
        let _hold = &ctrl; // the control port stays bound for as long as the stream lives
        let mut len = [0u8; 2];
        let mut pkt = vec![0u8; 65536];
        let why = loop {
            if !read_full(&mut s, &mut len, &stop) {
                break "data connection ended";
            }
            let n = u16::from_be_bytes(len) as usize;
            if n < 2 + MIN_AUDIO_PKT {
                break "length prefix is not a buffered-audio packet";
            }
            if !read_full(&mut s, &mut pkt[..n - 2], &stop) {
                break "data connection ended mid-packet";
            }
            send_audio(&t, scid, &pkt[..n - 2], &stat);
        };
        lg!("type 103 scid {scid}: {why} after {} packets", stat.msgs.load(Ordering::Relaxed));
    });
    Some((st, dp, cp))
}

/// A stream type this daemon does not tunnel: its ports exist (the phone needs a reply) and nothing
/// is read from them. Held until the session ends.
pub struct Hold {
    _tcp: Option<TcpListener>,
    _udp: Vec<UdpSocket>,
}

pub fn hold_tcp() -> Option<(Hold, u16)> {
    let l = TcpListener::bind("0.0.0.0:0").ok()?;
    let port = l.local_addr().ok()?.port();
    Some((Hold { _tcp: Some(l), _udp: Vec::new() }, port))
}

pub fn hold_udp() -> Option<(Hold, u16)> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    let port = s.local_addr().ok()?.port();
    Some((Hold { _tcp: None, _udp: vec![s] }, port))
}

/// The event channel: the phone connects and nothing flows (docs/ocbm/02 §4.6), but closing it ends
/// the session, so it is accepted and held open, unread bytes discarded, until the session stops.
pub fn start_event(stop: Arc<AtomicBool>) -> Option<u16> {
    let l = TcpListener::bind("0.0.0.0:0").ok()?;
    let port = l.local_addr().ok()?.port();
    let st = stop.clone();
    accept_loop(l, stop, move |mut s| {
        let mut buf = [0u8; 1024];
        while !st.load(Ordering::Relaxed) {
            match s.read(&mut buf) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {}
                Err(_) => break,
            }
        }
    });
    Some(port)
}

// ---------------------------------------------------------------- metadata bodies

/// The scalar members of a plist dictionary as JSON object members (`"k":v,...`). Data (key material
/// included), arrays and dictionaries are left out; integers are exact (they can exceed 2^53).
pub fn plist_scalars(d: &plist::Dictionary) -> String {
    use plist::Value as V;
    let mut o = Vec::new();
    for (k, v) in d.iter() {
        let val = match v {
            V::Integer(i) => i.as_unsigned().map(|u| u.to_string()).or_else(|| i.as_signed().map(|s| s.to_string())),
            V::Boolean(b) => Some(b.to_string()),
            V::Real(r) if r.is_finite() => Some(r.to_string()),
            V::String(s) => Some(format!("\"{}\"", jesc(&s.chars().take(128).collect::<String>()))),
            _ => None,
        };
        if let Some(val) = val {
            o.push(format!("\"{}\":{val}", jesc(k)));
        }
    }
    o.join(",")
}

/// `application/x-dmap-tagged` track info -> JSON members (title, artist, album, genre, durationMs).
pub fn dmap_track(body: &[u8]) -> String {
    fn walk(mut b: &[u8], depth: u32, out: &mut Vec<String>) {
        while b.len() >= 8 {
            let len = u32::from_be_bytes([b[4], b[5], b[6], b[7]]) as usize;
            if b.len() < 8 + len {
                return;
            }
            let (code, v) = (&b[..4], &b[8..8 + len]);
            let name = match code {
                b"minm" => Some("title"),
                b"asar" => Some("artist"),
                b"asal" => Some("album"),
                b"asgn" => Some("genre"),
                _ => None,
            };
            if let Some(name) = name {
                out.push(format!("\"{name}\":\"{}\"", jesc(&String::from_utf8_lossy(v).chars().take(256).collect::<String>())));
            } else if code == b"astm" && len == 4 {
                out.push(format!("\"durationMs\":{}", u32::from_be_bytes([v[0], v[1], v[2], v[3]])));
            } else if matches!(code, b"mlit" | b"mlcl" | b"cmst" | b"mdst") && depth < 4 {
                walk(v, depth + 1, out);
            }
            b = &b[8 + len..];
        }
    }
    let mut out = Vec::new();
    walk(body, 0, &mut out);
    out.join(",")
}
