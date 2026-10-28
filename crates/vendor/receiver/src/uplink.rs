//! Mic uplink — the head-unit→iPhone leg of the bidirectional **MainAudio** (stream type 100).
//!
//! The app captures the mic and ships each PCM chunk (S16LE at the negotiated rate) over OCBM
//! `CH_MIC`; ocbmd relays it to the receiver's mic-ingest seam as `mic <len>\n<pcm…>` lines. For each
//! active `speechRecognition`/`telephony` SETUP that offers an uplink (`input=true`, request
//! `dataPort`), we frame the PCM per the negotiated codec, RTP-packetize, encrypt with the stream's
//! **INPUT** key, and send to the iPhone — mirroring the C `_MainAltAudioSetup` uplink leg
//! (`AirPlayReceiverSessionWriteAudio`, authoritative in `CarPlaySDK.framework`).
//!
//! Codec is transport-determined:
//!   - **WIRED (USB-NCM):** iOS negotiates LPCM at the SETUP rate (8/16/24/32/44.1/48 kHz, 16-bit) — the
//!     mic is sent as raw **big-endian** PCM, no encoder (mirrors the PCM downlink). This is the only
//!     path the box builds (feature `mic-uplink`, no external codec).
//!   - **Wireless:** AAC-ELD (eld-codec / libfdk-aac), behind feature `mic-uplink-eld` (fdk-aac is not
//!     available on the box, so carplayd never enables it).
//!
//! Only ONE uplink is active at a time: the iPhone re-SETUPs MainAudio each Siri turn, and the newest
//! instance's key/port/codec supersede the prior (same philosophy as the downlink sink).

use std::io::{BufRead, BufReader, Read};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(feature = "mic-uplink-eld")]
use eld_codec::EldEncoder;

use crate::session::AudioCodec;
use crate::stream::encrypt_audio_aad;

/// The current uplink target (newest type-100 `input` SETUP wins). `None` when no uplink is active.
static UPLINK: Mutex<Option<UplinkState>> = Mutex::new(None);

/// Write-halves of the connected control-in sockets (the app's `ControlClient`). The receiver pushes
/// `uplink on <rate> <ch>` / `uplink off` back to the app so it can gate + format the mic capture on the
/// real type-100 `input` SETUP edge (Siri wants the mic BEFORE any downlink audio — a downlink-activity
/// gate would clip the onset). Usually one entry; a reconnect adds another and dead ones are pruned.
static CONTROL_TX: Mutex<Vec<TcpStream>> = Mutex::new(Vec::new());

/// Broadcast a back-channel line to every connected control-in peer, dropping any that error.
///
/// Bounded, per the house pattern (`events.rs::write_frame_or_fail` / `datastream.rs::
/// write_all_retrying`): each peer socket carries a 2 s SO_SNDTIMEO (set at registration) and the
/// write loop retries transient stalls up to a 6 s deadline. Previously this held `CONTROL_TX`
/// across `write_all` + `flush` on sockets with NO write timeout, so one wedged peer could block
/// `configure`/`clear` — and everything behind this lock — indefinitely.
fn notify_control(line: &str) {
    let mut txs = crate::plock(&CONTROL_TX);
    txs.retain_mut(|s| write_line_bounded(s, line.as_bytes()));
}

/// Write one back-channel line, retrying transient stalls (WouldBlock/TimedOut/Interrupted) with
/// 20 ms sleeps up to a 6 s deadline. Returns false — the caller drops the peer — on deadline
/// expiry, `Ok(0)`, any other error, or a failed flush.
fn write_line_bounded(s: &mut TcpStream, line: &[u8]) -> bool {
    use std::io::ErrorKind::{Interrupted, TimedOut, WouldBlock};
    use std::io::Write as _;
    const DEADLINE: Duration = Duration::from_secs(6); // 3× the socket's own 2 s write timeout
    let start = Instant::now();
    let mut sent = 0usize;
    while sent < line.len() {
        match s.write(&line[sent..]) {
            Ok(0) => return false,
            Ok(n) => sent += n,
            Err(e) if matches!(e.kind(), WouldBlock | TimedOut | Interrupted) => {
                if start.elapsed() >= DEADLINE {
                    eprintln!(
                        "[uplink] back-channel TX stalled at {sent}/{} B ({e}) — dropping peer",
                        line.len()
                    );
                    return false;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return false,
        }
    }
    s.flush().is_ok()
}

/// How the mic PCM is framed for the wire — transport-determined at SETUP.
enum UplinkCodec {
    /// WIRED: raw big-endian PCM, packetized in fixed `frame` sample counts (no encoder).
    Pcm { frame: usize },
    /// Wireless: AAC-ELD via libfdk-aac.
    #[cfg(feature = "mic-uplink-eld")]
    Eld(EldEncoder),
}

struct UplinkState {
    /// A clone of the stream's data socket — uplink RTP is sent from the same source port the iPhone
    /// sends downlink to (matches the C, which uses `ctx->dataSock` for both).
    sock: UdpSocket,
    /// iPhone IP + the request's `dataPort` (the uplink receive port the iPhone advertised).
    dst: SocketAddr,
    /// The stream INPUT key (HKDF "DataStream-Input-Encryption-Key").
    key: [u8; 32],
    codec: UplinkCodec,
    /// Negotiated input format (echoed to the app so mic capture matches).
    rate: u32,
    channels: u16,
    /// Mic PCM not yet aligned to a full frame.
    accum: Vec<i16>,
    pt: u8, // RTP payload type = stream type (100)
    seq: u16,
    ts: u32,
    nonce: u64,
    sent: u64, // observability: uplink packets actually delivered to the socket
    /// One-shot latch so a broken `dst` logs once instead of per 20 ms packet.
    send_failed: bool,
}

impl UplinkState {
    fn push_pcm(&mut self, samples: &[i16]) {
        self.accum.extend_from_slice(samples);
        // Frame first (borrows `self.codec`/`self.accum`), collecting each ready payload + the ts advance;
        // then send (needs `&mut self`) — keeps the codec borrow from overlapping `send_au`.
        let mut ready: Vec<(Vec<u8>, u32)> = Vec::new();
        // QC 2026-07-25: the RTP timestamp advance below is in SAMPLE-FRAMES (per channel), not
        // interleaved samples. `accum` holds interleaved samples, so the two differ by `channels` —
        // previously both arms advanced by the interleaved count, which ran the RTP clock exactly 2x
        // too fast on a stereo input. Dormant today only because voice/telephony input negotiates
        // mono; it would surface the moment a stereo input format is used.
        let ch = (self.channels.max(1)) as usize;
        match &mut self.codec {
            #[cfg(feature = "mic-uplink-eld")]
            UplinkCodec::Eld(enc) => {
                // fdk-aac's `info.frameLength` is samples PER CHANNEL, so one encoder frame consumes
                // `frame_len * channels` interleaved samples. Draining only `frame_len` fed the
                // encoder half a frame on stereo as well as mis-advancing the clock.
                let per_ch = enc.frame_len().max(1);
                let take = per_ch * ch;
                while self.accum.len() >= take {
                    let frame: Vec<i16> = self.accum.drain(..take).collect();
                    let au = enc.encode(&frame); // may be empty while the encoder primes
                    ready.push((au, per_ch as u32));
                }
            }
            UplinkCodec::Pcm { frame } => {
                let fl = (*frame).max(1);
                while self.accum.len() >= fl {
                    let chunk: Vec<i16> = self.accum.drain(..fl).collect();
                    // iPhone expects big-endian (network-order) PCM — same as the downlink samples.
                    let mut be = Vec::with_capacity(chunk.len() * 2);
                    for s in &chunk {
                        be.extend_from_slice(&s.to_be_bytes());
                    }
                    // `frame` is built as (rate/50)*channels interleaved, so fl/ch == rate/50 == the
                    // per-channel sample count for a ~20 ms packet. Identical to `fl` when mono.
                    ready.push((be, (fl / ch).max(1) as u32));
                }
            }
        }
        for (au, adv) in ready {
            if !au.is_empty() {
                self.send_au(&au);
            }
            // Advance the RTP clock per input frame even while an encoder primes (empty AU), so
            // timestamps stay aligned to the audio sample position.
            self.ts = self.ts.wrapping_add(adv);
        }
    }

    fn send_au(&mut self, au: &[u8]) {
        let mut hdr = [0u8; 12];
        hdr[0] = 0x80; // version 2, no padding/extension/CSRC
        hdr[1] = self.pt; // payload type = 100, marker bit 0
        hdr[2..4].copy_from_slice(&self.seq.to_be_bytes());
        hdr[4..8].copy_from_slice(&self.ts.to_be_bytes());
        // ssrc = 0 (matches the C uplink); hdr[8..12] left zero.
        let nonce8 = self.nonce.to_le_bytes();
        let mut aad = [0u8; 8];
        aad.copy_from_slice(&hdr[4..12]); // ts‖ssrc — same AAD as the downlink for modern iOS
        let pkt = encrypt_audio_aad(&self.key, &hdr, au, &nonce8, &aad);
        // The sequence and nonce advance on the ATTEMPT -- the counter-nonce is burned the moment the
        // packet is encrypted and must never be reused, exactly as `datastream::send` argues.
        self.seq = self.seq.wrapping_add(1);
        self.nonce = self.nonce.wrapping_add(1);
        // `sent`, though, counts DELIVERIES: it feeds the "sent N packets" line, and a wrong `dst` or
        // a closed socket used to look identical there to a healthy uplink. The first error is logged
        // once so a silent uplink is diagnosable without a packet capture.
        match self.sock.send_to(&pkt, self.dst) {
            Ok(_) => self.sent += 1,
            Err(e) => {
                if !self.send_failed {
                    self.send_failed = true;
                    eprintln!("[uplink] send to {} failed: {e} (further errors not logged)", self.dst);
                }
                return;
            }
        }
        if self.sent == 1 || self.sent.is_multiple_of(100) {
            eprintln!("[uplink] sent {} packets → {} (last AU {} B)", self.sent, self.dst, au.len());
        }
    }
}

/// Configure (or supersede) the uplink for a new type-100 `input` SETUP. `send_sock` is a clone of the
/// stream's data socket; `dst` is the iPhone's `(ip, dataPort)`; `input_key` is the stream INPUT key.
/// `codec` is the negotiated input codec — PCM over wired (raw BE), AAC-ELD over wireless.
pub fn configure(
    send_sock: UdpSocket,
    dst: SocketAddr,
    input_key: [u8; 32],
    stream_type: u8,
    codec: AudioCodec,
    sample_rate: u32,
    channels: u16,
) {
    let (uc, desc) = match codec {
        AudioCodec::Pcm => {
            // ~20 ms packets (320 samples @ 16 kHz mono). ch is ~always 1 for voice input; scale so a
            // stereo frame still lands on ~20 ms of wall-clock.
            // `.max(1)` on the WHOLE product turned either degenerate input -- channels == 0, or a
            // sample rate under 50 -- into one-sample packets, i.e. one encrypted datagram per
            // sample. Clamp each factor instead, matching `push_pcm`'s own `self.channels.max(1)`.
            let frame = (sample_rate as usize / 50).max(1) * (channels.max(1)) as usize;
            (UplinkCodec::Pcm { frame }, format!("PCM BE, {frame}-sample packets"))
        }
        #[cfg(feature = "mic-uplink-eld")]
        AudioCodec::AacLc | AudioCodec::AacEld => match EldEncoder::new(sample_rate, channels) {
            Some(enc) => {
                let asc: String = enc.asc().iter().map(|b| format!("{b:02x}")).collect();
                (UplinkCodec::Eld(enc), format!("ELD, ASC {asc}"))
            }
            None => {
                eprintln!("[uplink] ELD encoder open failed ({sample_rate}Hz {channels}ch) — uplink disabled");
                return;
            }
        },
        // Opus is never a MIC-input codec (mic uplink is PCM, or AAC-ELD with the fdk feature) — bail.
        AudioCodec::Opus => {
            eprintln!("[uplink] Opus is not a supported mic-uplink input codec — uplink disabled");
            return;
        }
        // Wireless AAC-ELD uplink needs feature `mic-uplink-eld` (fdk-aac). The box is wired-only and
        // never negotiates a compressed input codec, so this is unreachable there — bail loudly rather
        // than silently ship PCM under an AAC payload type.
        #[cfg(not(feature = "mic-uplink-eld"))]
        AudioCodec::AacLc | AudioCodec::AacEld => {
            eprintln!(
                "[uplink] compressed input codec {codec:?} negotiated but `mic-uplink-eld` not built \
                 — uplink disabled (wired transport expects PCM)"
            );
            return;
        }
    };
    *crate::plock(&UPLINK) = Some(UplinkState {
        sock: send_sock,
        dst,
        key: input_key,
        codec: uc,
        rate: sample_rate,
        channels,
        accum: Vec::new(),
        pt: stream_type,
        seq: 0,
        ts: 0,
        nonce: 0,
        sent: 0,
        send_failed: false,
    });
    eprintln!("[uplink] mic → iPhone {dst} ({sample_rate}Hz {channels}ch {desc}) — armed");
    // Tell the app to start capturing at the negotiated format (the mic source tracks what iOS picked).
    notify_control(&format!("uplink on {sample_rate} {channels}\n"));
}

/// Clear the active uplink (on TEARDOWN / session end).
pub fn clear() {
    let was_armed = crate::plock(&UPLINK).take().is_some();
    if was_armed {
        notify_control("uplink off\n");
    }
}

/// Length of the control-in auth token: 128 bits. The Android app draws one per process from
/// `getrandom` in `JNI_OnLoad` and hands it only to its own `MicUplink`, through
/// [`ControlListener::admit`] (`NativeCore.micAuth`).
pub const MIC_TOKEN_LEN: usize = 16;

/// The listener's reply to a valid `auth` line, sent before any `uplink on/off` line (token mode
/// only; carplayd's no-token seam sends nothing, so ocbmd's protocol is unchanged). A client treats
/// the seam as up only once it has read this: a wrong token, a full authenticated cap or a stale
/// listener all close the connection WITHOUT it.
pub const AUTH_ACK: &str = "auth ok\n";

/// Longest control-in TEXT line, newline included (F136). Every legitimate line is short —
/// `auth <32 hex>` is 38 B, `mic <len>` at most 12 B — so a line that fills this is hostile or
/// desynced, and the connection closes. `mic` PCM BODIES are not lines: `read_exact` reads them
/// against the 1 MiB `mic` cap in [`read_control`].
const MAX_LINE: u64 = 256;

/// AUTHENTICATED control-in connections (F136); a peer that authenticates while two are live is
/// closed without [`AUTH_ACK`]. Two, not one: a superseded app session's `MicUplink` can still be
/// closing its socket while the next session's connects. Only peers that presented the token count,
/// so no other app can occupy these. (No-token mode has no auth, so there every connection counts,
/// from the accept.)
const MAX_LIVE: usize = 2;

/// Unauthenticated connections from a port NO in-process client reserved ([`ControlListener::admit`])
/// — the budget any local app can reach. When full, the next such connection is closed at the
/// accept. A flood can keep this pool full; that is why the real client never depends on it.
const PREAUTH_ANON: usize = 4;
/// How long an unreserved connection has to deliver its whole `auth` line. Short: the only
/// legitimate client writes the line right after `connect()` returns.
const ANON_AUTH_WINDOW: Duration = Duration::from_millis(250);

/// Unauthenticated connections whose source port the app reserved before connecting. Separate from
/// [`PREAUTH_ANON`], so a flood of unreserved connections cannot exhaust it, and only this process
/// can create a reservation, so no other app can exhaust it either.
const PREAUTH_RESERVED: usize = 4;
/// A reserved connection still has to present the token; it just cannot be crowded out meanwhile.
const RESERVED_AUTH_WINDOW: Duration = Duration::from_secs(1);

/// Outstanding source-port reservations. Each is one-shot (consumed by the accept it matches) and
/// expires after [`RESERVATION_TTL`], so a port the client gave up on does not linger for the OS to
/// hand to another app's socket. `MicUplink` makes at most one per 2 s attempt.
const RESERVATIONS: usize = 4;
const RESERVATION_TTL: Duration = Duration::from_secs(5);

/// At most one rejection line per this interval, carrying the count of the ones it stood for.
const REJECT_LOG_EVERY: Duration = Duration::from_secs(1);

/// One listener's shared state. Per listener, not global, so the caps count only this seam's peers.
struct ControlSeam {
    token: Option<[u8; MIC_TOKEN_LEN]>,
    live: AtomicUsize,
    preauth_anon: AtomicUsize,
    preauth_reserved: AtomicUsize,
    /// `(source port, when reserved)`; port 0 = free entry.
    reservations: Mutex<[(u16, Instant); RESERVATIONS]>,
    /// True while the accept thread owns the bound listener. Cleared BEFORE the listener is dropped
    /// (see [`spawn_serving`]), so [`ControlListener::admit`] never hands out the token for a port
    /// this process no longer holds.
    serving: AtomicBool,
    rejects: Mutex<RejectLog>,
}

#[derive(Default)]
struct RejectLog {
    total: u64,
    /// Rejections since the last line that were counted but not logged.
    suppressed: u64,
    last: Option<Instant>,
}

impl ControlSeam {
    fn new(token: Option<[u8; MIC_TOKEN_LEN]>) -> Arc<ControlSeam> {
        let now = Instant::now();
        Arc::new(ControlSeam {
            token,
            live: AtomicUsize::new(0),
            preauth_anon: AtomicUsize::new(0),
            preauth_reserved: AtomicUsize::new(0),
            reservations: Mutex::new([(0, now); RESERVATIONS]),
            serving: AtomicBool::new(false),
            rejects: Mutex::new(RejectLog::default()),
        })
    }

    /// Time-throttled: at most one line per [`REJECT_LOG_EVERY`], so a local process hammering the
    /// port costs one log line a second, not one per connection.
    fn reject(&self, peer: Option<SocketAddr>, why: &str) {
        let now = Instant::now();
        let mut g = crate::plock(&self.rejects);
        g.total += 1;
        if g.last.is_some_and(|t| now.duration_since(t) < REJECT_LOG_EVERY) {
            g.suppressed += 1;
            return;
        }
        g.last = Some(now);
        let (total, suppressed) = (g.total, std::mem::take(&mut g.suppressed));
        drop(g);
        let peer = peer.map_or_else(|| "?".to_string(), |p| p.to_string());
        if suppressed == 0 {
            eprintln!("[uplink] control-in {peer} rejected: {why} (rejection #{total})");
        } else {
            eprintln!(
                "[uplink] control-in {peer} rejected: {why} (rejection #{total}; {suppressed} more since \
                 the last line, not logged)"
            );
        }
    }

    /// Reserve `port` for the next accepted connection from exactly `127.0.0.1:port`: replaces a
    /// free or expired entry, else the oldest.
    fn reserve(&self, port: u16) {
        let now = Instant::now();
        let mut g = crate::plock(&self.reservations);
        let free = g.iter().position(|&(p, at)| p == 0 || now.duration_since(at) >= RESERVATION_TTL);
        let i = free.unwrap_or_else(|| {
            g.iter().enumerate().min_by_key(|(_, &(_, at))| at).map_or(0, |(i, _)| i)
        });
        g[i] = (port, now);
    }

    /// Drop the reservation for `port`, if it is still outstanding (the client's connect failed).
    fn cancel_reservation(&self, port: u16) {
        if port == 0 {
            return;
        }
        for e in crate::plock(&self.reservations).iter_mut().filter(|e| e.0 == port) {
            e.0 = 0;
        }
    }

    /// Consume a live reservation matching `peer`'s source port. The source ADDRESS must be exactly
    /// 127.0.0.1, the address the client binds: on Linux every 127.0.0.0/8 address is local, so
    /// another app can bind 127.0.0.2:P while ours holds 127.0.0.1:P, and `is_loopback()` would let
    /// that socket take the reservation (F136 fix round 2).
    fn take_reservation(&self, peer: SocketAddr) -> bool {
        if !reservable_peer(peer) {
            return false;
        }
        let now = Instant::now();
        let mut g = crate::plock(&self.reservations);
        match g.iter_mut().find(|(p, at)| *p == peer.port() && now.duration_since(*at) < RESERVATION_TTL) {
            Some(e) => {
                e.0 = 0;
                true
            }
            None => false,
        }
    }
}

/// The only source address a reservation can match: `127.0.0.1` exactly, nonzero port. Not
/// `is_loopback()` (any of 127.0.0.0/8, or `::1`).
fn reservable_peer(peer: SocketAddr) -> bool {
    peer.ip() == IpAddr::V4(Ipv4Addr::LOCALHOST) && peer.port() != 0
}

/// Which budget a connection slot comes out of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pool {
    Live,
    PreAuthAnon,
    PreAuthReserved,
}

impl Pool {
    fn counter(self, s: &ControlSeam) -> &AtomicUsize {
        match self {
            Pool::Live => &s.live,
            Pool::PreAuthAnon => &s.preauth_anon,
            Pool::PreAuthReserved => &s.preauth_reserved,
        }
    }

    fn cap(self) -> usize {
        match self {
            Pool::Live => MAX_LIVE,
            Pool::PreAuthAnon => PREAUTH_ANON,
            Pool::PreAuthReserved => PREAUTH_RESERVED,
        }
    }

    fn full(self) -> &'static str {
        match self {
            Pool::Live => "connection cap reached",
            Pool::PreAuthAnon => "unauthenticated cap reached",
            Pool::PreAuthReserved => "reserved unauthenticated cap reached",
        }
    }
}

/// A held slot in one [`Pool`]; released on drop, including when the reader thread fails to spawn
/// and on panic unwind.
struct Slot {
    seam: Arc<ControlSeam>,
    pool: Pool,
}

impl Slot {
    fn acquire(seam: &Arc<ControlSeam>, pool: Pool) -> Option<Slot> {
        let cap = pool.cap();
        pool.counter(seam)
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < cap).then_some(n + 1))
            .ok()?;
        Some(Slot { seam: Arc::clone(seam), pool })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.pool.counter(&self.seam).fetch_sub(1, Ordering::AcqRel);
    }
}

/// Clears [`ControlSeam::serving`] when the accept thread ends, however it ends.
struct Serving(Arc<ControlSeam>);

impl Drop for Serving {
    fn drop(&mut self) {
        self.0.serving.store(false, Ordering::Release);
    }
}

/// A running control-in listener. Dropping the handle does NOT stop it: the accept thread owns the
/// socket for the life of the process.
pub struct ControlListener {
    seam: Arc<ControlSeam>,
    local: SocketAddr,
}

impl ControlListener {
    /// The bound address (the real port when `addr` asked for port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// True while the accept thread still holds the bound socket.
    pub fn is_serving(&self) -> bool {
        self.seam.serving.load(Ordering::Acquire)
    }

    /// Reserve `local_port` — the port the in-process client has ALREADY BOUND its socket to on
    /// `127.0.0.1` and is about to connect from — and return the token it must send. `None` with no
    /// token configured, for port 0, or once the listener is no longer serving.
    ///
    /// Only this process can call this. The reservation matches only a peer at exactly
    /// `127.0.0.1:local_port`, and the client holds that address bound (without `SO_REUSEADDR`), so
    /// no other local socket can connect from it while the client holds it. (Another app CAN bind
    /// `127.0.0.2:local_port` on Linux; that peer does not match.) A connect that fails leaves the
    /// reservation behind; the client must [`release`](Self::release) it BEFORE closing its socket,
    /// while it still holds the port, and [`RESERVATION_TTL`] is the backstop if it never does. The
    /// reservation only buys a pre-auth slot no flood can take; the token still decides. NEVER log
    /// the returned token.
    pub fn admit(&self, local_port: u16) -> Option<[u8; MIC_TOKEN_LEN]> {
        let token = self.seam.token?;
        if local_port == 0 || !self.is_serving() {
            return None;
        }
        self.seam.reserve(local_port);
        Some(token)
    }

    /// Withdraw an outstanding reservation for `local_port` (no-op if the accept already consumed
    /// it). Call it before closing the socket bound to that port.
    pub fn release(&self, local_port: u16) {
        self.seam.cancel_reservation(local_port);
    }
}

/// Bind the control-in listener on the caller-supplied `addr` — carplayd passes `127.0.0.1:9112`
/// (`MIC_INGEST_ADDR`); `:9110` is the SEPARATE HID input seam — and serve it on its own thread for
/// the life of the process. Accepts carlink's connection and routes `mic` PCM to the active uplink;
/// `touch`/`cmd` lines are consumed and ignored (HID is a separate subsystem).
///
/// The bind is SYNCHRONOUS and its error is returned (F136 fix round 1, 2026-10-07): a caller that
/// publishes a token must do so only on `Ok`, because if another process already holds `addr`, a
/// client handed the token would send it — and then its mic PCM — to that process.
///
/// F136 (2026-10-07): a loopback port is reachable by every local process holding INTERNET, so:
///   - `token: Some(t)` — the first line must be `auth <32 lowercase-or-uppercase hex of t>\n`,
///     complete within the auth window, or the connection closes. The listener answers a valid one
///     with [`AUTH_ACK`]. Until then the peer is not registered for the `uplink on/off` back-channel
///     and no `mic` PCM is read from it. The comparison is constant-time. The gm_ccpa app passes its
///     per-process token.
///     Pre-auth connections never count against the [`MAX_LIVE`] authenticated cap; they come out
///     of two separate bounded pools: [`PREAUTH_RESERVED`] for a source port reserved in-process
///     with [`ControlListener::admit`] (1 s window), and [`PREAUTH_ANON`] for every other port
///     (250 ms window). A flood can fill the anonymous pool, never the reserved one.
///   - `token: None` — no authentication (carplayd on the box, whose only client, ocbmd, is a
///     separate process with no token to present). No ack; every connection is live from the accept
///     and counts against [`MAX_LIVE`]. The line cap applies.
///   - every text line is read through `take(MAX_LINE)`; a line that reaches 256 bytes closes it.
pub fn start_control_listener(
    addr: &str,
    token: Option<[u8; MIC_TOKEN_LEN]>,
) -> std::io::Result<ControlListener> {
    let mode = if token.is_some() { "auth required" } else { "no auth" };
    let l = spawn_serving(TcpListener::bind(addr)?, ControlSeam::new(token))?;
    eprintln!(
        "[uplink] control-in listening {} (mic PCM in / uplink back-channel out; {mode}, at most \
         {MAX_LIVE} live)",
        l.local
    );
    Ok(l)
}

/// Serve an already-bound listener on its own thread. Split from the bind so tests can stage
/// connections and reservations before the first accept.
fn spawn_serving(listener: TcpListener, seam: Arc<ControlSeam>) -> std::io::Result<ControlListener> {
    let local = listener.local_addr()?;
    seam.serving.store(true, Ordering::Release);
    // Built here, not in the thread: if the spawn fails, the closure — and this guard — is dropped
    // and `serving` goes false with the listener.
    let guard = Serving(Arc::clone(&seam));
    let accept_seam = Arc::clone(&seam);
    std::thread::Builder::new().name("uplink-accept".into()).spawn(move || {
        let listener = listener;
        // Declared after `listener`, so dropped BEFORE it on any exit (return or unwind): `serving`
        // is false before the port is released.
        let _serving = guard;
        serve_control(&listener, &accept_seam);
    })?;
    Ok(ControlListener { seam, local })
}

/// The accept loop. It never blocks on a peer: per connection it does accept, getpeername, a
/// reservation lookup and an atomic slot claim, then hands off or closes — so it drains the kernel
/// accept queue as fast as a flood can fill it.
fn serve_control(listener: &TcpListener, seam: &Arc<ControlSeam>) {
    use std::io::ErrorKind::{ConnectionAborted, ConnectionReset, Interrupted, WouldBlock};
    for conn in listener.incoming() {
        let s = match conn {
            Ok(s) => s,
            // A peer that reset while queued: nothing to wait for.
            Err(e) if matches!(e.kind(), ConnectionAborted | ConnectionReset | Interrupted | WouldBlock) => {
                continue
            }
            Err(e) => {
                // EMFILE/ENFILE/ENOBUFS repeat at once; back off instead of spinning a core on them.
                seam.reject(None, &format!("accept failed: {e}"));
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
        };
        let peer = s.peer_addr().ok();
        let pool = match (seam.token.is_some(), peer) {
            (false, _) => Pool::Live,
            (true, Some(p)) if seam.take_reservation(p) => Pool::PreAuthReserved,
            (true, _) => Pool::PreAuthAnon,
        };
        let Some(slot) = Slot::acquire(seam, pool) else {
            seam.reject(peer, pool.full());
            let _ = s.shutdown(Shutdown::Both);
            continue;
        };
        let spawned = std::thread::Builder::new()
            .name("uplink-ctl".into())
            .spawn(move || read_control(s, peer, slot));
        if let Err(e) = spawned {
            // The closure (stream + slot) is dropped with the error: socket closed, slot released.
            eprintln!("[uplink] control-in reader spawn failed: {e}");
        }
    }
}

/// Read the first line within `deadline`, capped at [`MAX_LINE`] bytes. `BufReader` keeps any bytes
/// after the newline for the main loop. Per-read SO_RCVTIMEO alone is not enough — a peer trickling
/// a byte per timeout would hold the slot for 256 timeouts — so the timeout is re-armed with the
/// REMAINING window before every read.
fn read_line_by(r: &mut BufReader<TcpStream>, deadline: Instant) -> Result<Vec<u8>, &'static str> {
    let mut line = Vec::with_capacity(64);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("no auth line within the auth window");
        }
        r.get_ref().set_read_timeout(Some(left)).map_err(|_| "set_read_timeout failed")?;
        let buf = match r.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                return Err("no auth line within the auth window");
            }
            Err(_) => return Err("read error before auth"),
        };
        if buf.is_empty() {
            return Err("closed before auth");
        }
        let room = MAX_LINE as usize - line.len();
        let window = &buf[..buf.len().min(room)];
        if let Some(i) = window.iter().position(|&b| b == b'\n') {
            line.extend_from_slice(&window[..=i]);
            r.consume(i + 1);
            return Ok(line);
        }
        let n = window.len();
        line.extend_from_slice(window);
        r.consume(n);
        if line.len() as u64 >= MAX_LINE {
            return Err("auth line over 256 bytes");
        }
    }
}

/// `auth <32 hex>\n` (optional `\r`) → the 16 presented bytes. Strict: anything else is `None`.
fn parse_auth_line(line: &[u8]) -> Option<[u8; MIC_TOKEN_LEN]> {
    fn nib(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    let body = line.strip_suffix(b"\n")?;
    let body = body.strip_suffix(b"\r").unwrap_or(body);
    let hex = body.strip_prefix(b"auth ")?;
    if hex.len() != 2 * MIC_TOKEN_LEN {
        return None;
    }
    let mut out = [0u8; MIC_TOKEN_LEN];
    for (o, &[hi, lo]) in out.iter_mut().zip(hex.as_chunks::<2>().0) {
        *o = (nib(hi)? << 4) | nib(lo)?;
    }
    Some(out)
}

/// Constant-time equality over the fixed-length token: no early exit on the first differing byte.
fn ct_eq(a: &[u8; MIC_TOKEN_LEN], b: &[u8; MIC_TOKEN_LEN]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

/// Token mode: read and check the `auth` line within the pool's window, trade the pre-auth `slot`
/// for a [`Pool::Live`] one, clear the read timeout and send [`AUTH_ACK`]. On `Err` the caller
/// rejects and closes; `slot` is released either way.
fn authenticate(
    r: &mut BufReader<TcpStream>,
    token: &[u8; MIC_TOKEN_LEN],
    slot: Slot,
) -> Result<Slot, &'static str> {
    let window = if slot.pool == Pool::PreAuthReserved { RESERVED_AUTH_WINDOW } else { ANON_AUTH_WINDOW };
    let line = read_line_by(r, Instant::now() + window)?;
    let presented = parse_auth_line(&line).ok_or("first line is not `auth <32 hex>`")?;
    if !ct_eq(&presented, token) {
        return Err("wrong token");
    }
    let live = Slot::acquire(&slot.seam, Pool::Live).ok_or("authenticated, but the 2 live slots are taken")?;
    drop(slot);
    // An authenticated client idles between Siri turns for as long as the session lasts, so the
    // auth window's read timeout must not outlive the auth.
    r.get_ref().set_read_timeout(None).map_err(|_| "clear read timeout failed")?;
    // House pattern (events.rs/datastream.rs): 2 s SO_SNDTIMEO, bounded retry in `write_line_bounded`.
    // Set before the ack so the ack itself cannot wedge this thread on a peer that never reads.
    let _ = r.get_ref().set_write_timeout(Some(Duration::from_secs(2)));
    // Written BEFORE the peer is registered in CONTROL_TX, so it precedes any `uplink on/off`.
    if !write_line_bounded(r.get_mut(), AUTH_ACK.as_bytes()) {
        return Err("auth ack write failed");
    }
    Ok(live)
}

/// Read carlink's control-in stream: line-framed `mic <len>\n<len bytes PCM>`, plus `touch`/`cmd`
/// text lines we ignore. The `mic` PCM is 16 kHz mono S16LE. With a token configured, the first
/// line must authenticate (see [`start_control_listener`]); `slot` is then a pre-auth slot, else it
/// is already [`Pool::Live`].
fn read_control(stream: TcpStream, peer: Option<SocketAddr>, slot: Slot) {
    let peer_s = peer.map_or_else(|| "?".to_string(), |p| p.to_string());
    let seam = Arc::clone(&slot.seam);
    let mut r = BufReader::new(stream);
    // Held to the end of this function: the live slot.
    let _live = if let Some(token) = seam.token.as_ref() {
        match authenticate(&mut r, token, slot) {
            Ok(live) => {
                eprintln!("[uplink] control-in connected from {peer_s} (authenticated)");
                live
            }
            Err(why) => {
                seam.reject(peer, why);
                let _ = r.get_ref().shutdown(Shutdown::Both);
                return;
            }
        }
    } else {
        eprintln!("[uplink] control-in connected from {peer_s} (no auth configured)");
        slot
    };
    // Register a write-half for the uplink back-channel; if an uplink is already armed, tell this new
    // peer immediately so a mid-session (re)connect starts the mic. Only AFTER authentication: an
    // unauthenticated peer must not learn when the uplink arms.
    if let Ok(tx) = r.get_ref().try_clone() {
        // House pattern (events.rs/datastream.rs): 2 s SO_SNDTIMEO on the socket, 6 s retry deadline
        // in `write_line_bounded`, so a wedged peer costs a bounded stall instead of a forever-block.
        let _ = tx.set_write_timeout(Some(Duration::from_secs(2)));
        crate::plock(&CONTROL_TX).push(tx);
        // Copy the armed format OUT of the UPLINK guard before notifying: the previous if-let held
        // UPLINK (a scrutinee temporary) across the back-channel socket write inside notify_control.
        let armed = crate::plock(&UPLINK).as_ref().map(|s| (s.rate, s.channels));
        if let Some((rate, channels)) = armed {
            notify_control(&format!("uplink on {rate} {channels}\n"));
        }
    }
    let mut line = String::new();
    let mut mic_chunks: u64 = 0;
    loop {
        line.clear();
        match (&mut r).take(MAX_LINE).read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if !line.ends_with('\n') {
            // MAX_LINE bytes with no newline (over-long: hostile or desynced), or EOF mid-line.
            if line.len() as u64 >= MAX_LINE {
                seam.reject(peer, "line over 256 bytes");
            }
            break;
        }
        if let Some(rest) = line.trim_end().strip_prefix("mic ") {
            let len: usize = match rest.trim().parse() {
                Ok(n) if n > 0 && n <= (1 << 20) => n,
                _ => continue,
            };
            let mut pcm = vec![0u8; len];
            if r.read_exact(&mut pcm).is_err() {
                break;
            }
            let samples: Vec<i16> =
                pcm.as_chunks::<2>().0.iter().map(|&c| i16::from_le_bytes(c)).collect();
            mic_chunks += 1;
            if mic_chunks == 1 || mic_chunks.is_multiple_of(100) {
                let armed = crate::plock(&UPLINK).is_some();
                eprintln!("[uplink] mic PCM rx: {mic_chunks} chunks ({} samples, uplink armed={armed})", samples.len());
            }
            if let Some(state) = crate::plock(&UPLINK).as_mut() {
                state.push_pcm(&samples);
            }
        }
        // `touch`/`cmd …` lines: not this seam's concern (see doc comment above), consumed and ignored.
    }
    // No prune of this peer's write-half from CONTROL_TX by key: every accepted socket shares the
    // listener's local address, so a comparison keyed on the FORMATTED peer alone matched any other
    // entry whose `peer_addr()` also failed. The shutdown below makes that entry's next write fail
    // (EPIPE), and `notify_control`'s `retain_mut` drops it then. It also makes an over-long line
    // a real close: the registered write-half would otherwise keep the socket open.
    let _ = r.get_ref().shutdown(Shutdown::Both);
    eprintln!("[uplink] control-in {peer_s} closed");
}

#[cfg(test)]
mod control_tests {
    //! F136: the `:9112` control-in seam's auth, ack, line cap and connection pools, against a real
    //! loopback listener. `UPLINK` is process-global and `session` tests clear it concurrently
    //! (`AvSession::reset` → `uplink::clear`), so the PCM assertions check that the uplink was still
    //! OURS afterwards and retry the scenario if a concurrent test cleared it.
    //!
    //! The sustained-flood test with a REAL reserved client (bind-before-connect needs libc, which
    //! this `forbid(unsafe_code)` crate does not have) lives in carplay-jni:
    //! `tests::mic_seam_flood_never_blocks_the_reserved_client`.
    use super::*;
    use std::io::Write as _;
    use std::net::Ipv4Addr;

    /// The tests here arm the global `UPLINK`; serialize them with each other.
    static SERIAL: Mutex<()> = Mutex::new(());

    const TOKEN: [u8; MIC_TOKEN_LEN] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
    ];
    /// One 20 ms frame at 16 kHz mono = 320 samples = 640 bytes = exactly one uplink packet.
    const CHUNK: usize = 640;

    fn hex(t: &[u8; MIC_TOKEN_LEN]) -> String {
        t.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn listen(token: Option<[u8; MIC_TOKEN_LEN]>) -> SocketAddr {
        start_control_listener("127.0.0.1:0", token).unwrap().local_addr()
    }

    fn auth_line(t: &[u8; MIC_TOKEN_LEN]) -> String {
        format!("auth {}\n", hex(t))
    }

    fn mic_chunk() -> Vec<u8> {
        let mut v = format!("mic {CHUNK}\n").into_bytes();
        v.extend((0..CHUNK).map(|i| i as u8));
        v
    }

    /// (closed by the server within `d`, bytes the server sent before that).
    fn closed_within(s: &mut TcpStream, d: Duration) -> (bool, usize) {
        let end = Instant::now() + d;
        let mut got = 0;
        let mut b = [0u8; 256];
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return (false, got);
            }
            s.set_read_timeout(Some(left)).unwrap();
            match s.read(&mut b) {
                Ok(0) => return (true, got),
                Ok(n) => got += n,
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                    return (false, got);
                }
                Err(_) => return (true, got), // reset: closed
            }
        }
    }

    /// Send the auth line and read the reply line (empty = closed or nothing within `d`).
    fn auth_reply(c: &mut TcpStream, t: &[u8; MIC_TOKEN_LEN], d: Duration) -> String {
        let _ = c.write_all(auth_line(t).as_bytes());
        c.set_read_timeout(Some(d)).unwrap();
        let mut line = String::new();
        let _ = BufReader::new(c.try_clone().unwrap()).read_line(&mut line);
        line
    }

    /// Arm the global uplink at 16 kHz mono PCM toward a fresh local UDP socket (the "iPhone").
    fn arm() -> UdpSocket {
        let phone = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        phone.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        let tx = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        configure(tx, phone.local_addr().unwrap(), [7u8; 32], 100, AudioCodec::Pcm, 16_000, 1);
        phone
    }

    /// The armed uplink is still the one [`arm`] installed for `phone`, and has pushed nothing.
    fn still_ours_untouched(phone: &UdpSocket) -> Option<bool> {
        let dst = phone.local_addr().unwrap();
        let g = crate::plock(&UPLINK);
        let s = g.as_ref().filter(|s| s.dst == dst)?;
        Some(s.sent == 0 && s.seq == 0 && s.accum.is_empty())
    }

    fn got_packet(phone: &UdpSocket) -> Option<usize> {
        let mut b = [0u8; 2048];
        phone.recv_from(&mut b).ok().map(|(n, _)| n)
    }

    #[test]
    fn auth_line_parser_is_strict() {
        let good = auth_line(&TOKEN);
        assert_eq!(parse_auth_line(good.as_bytes()), Some(TOKEN));
        assert_eq!(parse_auth_line(good.to_uppercase().replace("AUTH", "auth").as_bytes()), Some(TOKEN));
        assert_eq!(parse_auth_line(format!("auth {}\r\n", hex(&TOKEN)).as_bytes()), Some(TOKEN));
        assert_eq!(parse_auth_line(format!("auth {}", hex(&TOKEN)).as_bytes()), None, "no newline");
        assert_eq!(parse_auth_line(format!("auth  {}\n", hex(&TOKEN)).as_bytes()), None);
        assert_eq!(parse_auth_line(format!("auth {}0\n", hex(&TOKEN)).as_bytes()), None, "33 hex");
        assert_eq!(parse_auth_line(format!("auth +{}\n", &hex(&TOKEN)[1..]).as_bytes()), None);
        assert_eq!(parse_auth_line(b"mic 4\n"), None);
        assert!(ct_eq(&TOKEN, &TOKEN));
        let mut other = TOKEN;
        other[15] ^= 1;
        assert!(!ct_eq(&TOKEN, &other));
    }

    #[test]
    fn no_auth_is_closed_and_pushes_no_pcm() {
        let _g = crate::plock(&SERIAL);
        let addr = listen(Some(TOKEN));
        for attempt in 1..=5 {
            let phone = arm();
            // `mic` as the FIRST line, no auth: closed at once (not a valid auth line), no PCM read.
            let mut c = TcpStream::connect(addr).unwrap();
            c.write_all(&mic_chunk()).unwrap();
            let (closed, bytes) = closed_within(&mut c, Duration::from_millis(900));
            assert!(closed, "an unauthenticated `mic` line must close the connection");
            assert_eq!(bytes, 0, "an unauthenticated peer must get no ack and no `uplink on`");
            assert!(got_packet(&phone).is_none(), "PCM from an unauthenticated peer reached the phone");
            match still_ours_untouched(&phone) {
                Some(untouched) => {
                    assert!(untouched, "the armed uplink consumed PCM from an unauthenticated peer");
                    // Positive control on the SAME armed uplink: the real client gets through.
                    let mut ok = TcpStream::connect(addr).unwrap();
                    ok.write_all(auth_line(&TOKEN).as_bytes()).unwrap();
                    ok.write_all(&mic_chunk()).unwrap();
                    if let Some(n) = got_packet(&phone) {
                        assert!(n > CHUNK, "one RTP packet: header + {CHUNK} B PCM + tag, got {n}");
                        return;
                    }
                    eprintln!("[test] attempt {attempt}: positive control saw no packet — retrying");
                }
                None => eprintln!("[test] attempt {attempt}: uplink cleared concurrently — retrying"),
            }
        }
        panic!("5 attempts lost the armed uplink to concurrent tests");
    }

    #[test]
    fn silence_is_closed_after_the_anonymous_auth_window() {
        let _g = crate::plock(&SERIAL);
        let addr = listen(Some(TOKEN));
        let mut c = TcpStream::connect(addr).unwrap();
        let t0 = Instant::now();
        let (closed, bytes) = closed_within(&mut c, Duration::from_secs(4));
        let took = t0.elapsed();
        assert!(closed, "a silent peer must be closed");
        assert_eq!(bytes, 0);
        assert!(took >= Duration::from_millis(200), "closed after {took:?}: before the 250 ms window ran out");
        assert!(took < Duration::from_millis(900), "closed after {took:?}: the anonymous window is 250 ms");
    }

    #[test]
    fn trickled_auth_line_is_bounded_by_the_window_not_per_read() {
        let _g = crate::plock(&SERIAL);
        let addr = listen(Some(TOKEN));
        let mut c = TcpStream::connect(addr).unwrap();
        let t0 = Instant::now();
        // One byte every 100 ms never trips a per-read timeout, but the window is total.
        let mut closed = false;
        for b in b"auth 0011223344" {
            if c.write_all(&[*b]).is_err() {
                closed = true;
                break;
            }
            if closed_within(&mut c, Duration::from_millis(100)).0 {
                closed = true;
                break;
            }
        }
        assert!(closed, "a trickled auth line must be closed");
        assert!(t0.elapsed() < Duration::from_millis(900), "closed only after {:?}", t0.elapsed());
    }

    #[test]
    fn wrong_token_is_closed_without_an_ack() {
        let _g = crate::plock(&SERIAL);
        let addr = listen(Some(TOKEN));
        let mut wrong = TOKEN;
        wrong[0] ^= 0x80;
        let mut c = TcpStream::connect(addr).unwrap();
        c.write_all(auth_line(&wrong).as_bytes()).unwrap();
        let t0 = Instant::now();
        let (closed, bytes) = closed_within(&mut c, Duration::from_millis(900));
        assert!(closed, "a wrong token must close the connection");
        assert_eq!(bytes, 0, "a wrong token must not be acked");
        assert!(t0.elapsed() < Duration::from_millis(900));
    }

    #[test]
    fn over_long_line_is_closed_before_and_after_auth() {
        let _g = crate::plock(&SERIAL);
        let addr = listen(Some(TOKEN));
        let long = vec![b'x'; 300];
        // Before auth: closed on the cap, well inside the window.
        let mut pre = TcpStream::connect(addr).unwrap();
        pre.write_all(&long).unwrap();
        let t0 = Instant::now();
        assert!(closed_within(&mut pre, Duration::from_millis(900)).0, "300-byte pre-auth line not closed");
        assert!(t0.elapsed() < Duration::from_millis(900));
        // After auth: alive past the window, then a 300-byte line closes it.
        let mut post = TcpStream::connect(addr).unwrap();
        assert_eq!(auth_reply(&mut post, &TOKEN, Duration::from_millis(900)), AUTH_ACK);
        assert!(!closed_within(&mut post, Duration::from_millis(1300)).0, "authenticated peer was closed");
        let _ = post.write_all(&long);
        assert!(closed_within(&mut post, Duration::from_millis(900)).0, "300-byte post-auth line not closed");
        // A short `touch` line is fine, and so is the cap exactly: 255 bytes + newline = 256.
        let mut ok = TcpStream::connect(addr).unwrap();
        ok.write_all(auth_line(&TOKEN).as_bytes()).unwrap();
        ok.write_all(b"touch 1 2 3\n").unwrap();
        let mut at_cap = vec![b'y'; 255];
        at_cap.push(b'\n');
        ok.write_all(&at_cap).unwrap();
        assert!(!closed_within(&mut ok, Duration::from_millis(400)).0, "a short line closed the peer");
    }

    #[test]
    fn third_authenticated_peer_gets_no_ack_and_a_freed_slot_is_reusable() {
        let _g = crate::plock(&SERIAL);
        let addr = listen(Some(TOKEN));
        let mut c1 = TcpStream::connect(addr).unwrap();
        assert_eq!(auth_reply(&mut c1, &TOKEN, Duration::from_millis(900)), AUTH_ACK);
        let mut c2 = TcpStream::connect(addr).unwrap();
        assert_eq!(auth_reply(&mut c2, &TOKEN, Duration::from_millis(900)), AUTH_ACK);
        let mut c3 = TcpStream::connect(addr).unwrap();
        let t0 = Instant::now();
        assert_eq!(auth_reply(&mut c3, &TOKEN, Duration::from_millis(900)), "", "3rd authenticated peer acked");
        assert!(closed_within(&mut c3, Duration::from_millis(900)).0, "3rd authenticated peer not closed");
        assert!(t0.elapsed() < Duration::from_millis(900), "c3 closed by the window, not the live cap");
        // c1/c2 authenticated and outlive every window.
        assert!(!closed_within(&mut c1, Duration::from_millis(1200)).0, "c1 closed");
        assert!(!closed_within(&mut c2, Duration::from_millis(100)).0, "c2 closed");
        // Free a slot; the reader sees EOF asynchronously, so poll for the slot coming back.
        drop(c1);
        let mut admitted = false;
        for _ in 0..20 {
            let mut c4 = TcpStream::connect(addr).unwrap();
            if auth_reply(&mut c4, &TOKEN, Duration::from_millis(300)) == AUTH_ACK {
                admitted = true;
                break;
            }
        }
        assert!(admitted, "a freed slot was never reusable");
    }

    /// The fix-round-1 regression: unauthenticated peers used to take the 2 live slots for 1 s each.
    /// Now they come out of the anonymous pool, which caps at PREAUTH_ANON and never touches `live`.
    #[test]
    fn silent_unauthenticated_peers_cannot_take_the_live_slots() {
        let _g = crate::plock(&SERIAL);
        let addr = listen(Some(TOKEN));
        // MAX_LIVE silent peers: the old design's cap was now full.
        let silent: Vec<TcpStream> = (0..MAX_LIVE).map(|_| TcpStream::connect(addr).unwrap()).collect();
        std::thread::sleep(Duration::from_millis(30)); // let the accept loop claim their slots first
        let mut c = TcpStream::connect(addr).unwrap();
        assert_eq!(
            auth_reply(&mut c, &TOKEN, Duration::from_millis(200)),
            AUTH_ACK,
            "an authenticating client was blocked by {MAX_LIVE} silent unauthenticated peers"
        );
        drop(silent);
        std::thread::sleep(Duration::from_millis(50)); // their readers see EOF and free the anon slots
        // The anonymous pool's own bound: PREAUTH_ANON silent peers fill it; the next unreserved
        // connection is closed at the accept, with nothing sent and long before any window.
        let held: Vec<TcpStream> = (0..PREAUTH_ANON).map(|_| TcpStream::connect(addr).unwrap()).collect();
        std::thread::sleep(Duration::from_millis(30));
        let mut over = TcpStream::connect(addr).unwrap();
        let t0 = Instant::now();
        assert_eq!(closed_within(&mut over, Duration::from_millis(900)), (true, 0), "anonymous pool not capped");
        assert!(t0.elapsed() < Duration::from_millis(150), "closed by the window, not the cap: {:?}", t0.elapsed());
        drop(held);
        drop(c);
    }

    /// The reserved pool: staged so the accept order is fixed. The anonymous pool is full of silent
    /// attackers when the reserved client is accepted, and it still gets in; an unreserved client in
    /// the same position does not (the positive and negative halves of the same guard).
    #[test]
    fn reserved_port_gets_in_while_the_anonymous_pool_is_full() {
        let _g = crate::plock(&SERIAL);
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = l.local_addr().unwrap();
        let seam = ControlSeam::new(Some(TOKEN));
        // Queued in the kernel before the first accept, so accepted in this order.
        let attackers: Vec<TcpStream> =
            (0..PREAUTH_ANON + 2).map(|_| TcpStream::connect(addr).unwrap()).collect();
        let mut unreserved = TcpStream::connect(addr).unwrap();
        let mut reserved = TcpStream::connect(addr).unwrap();
        seam.reserve(reserved.local_addr().unwrap().port());
        let _handle = spawn_serving(l, Arc::clone(&seam)).unwrap();
        assert_eq!(auth_reply(&mut reserved, &TOKEN, Duration::from_millis(900)), AUTH_ACK);
        assert_eq!(
            auth_reply(&mut unreserved, &TOKEN, Duration::from_millis(900)),
            "",
            "an unreserved client behind a full anonymous pool must be closed (cap not enforced?)"
        );
        assert_eq!(seam.live.load(Ordering::Acquire), 1);
        drop(attackers);
    }

    #[test]
    fn reservations_are_one_shot_loopback_only_and_expire() {
        let seam = ControlSeam::new(Some(TOKEN));
        let lo = |p: u16| SocketAddr::from((Ipv4Addr::LOCALHOST, p));
        seam.reserve(40001);
        assert!(!seam.take_reservation(SocketAddr::from(([10, 0, 0, 1], 40001))), "non-loopback matched");
        // Fix round 2: other loopback addresses do not match either (Linux: 127.0.0.2 is local and
        // bindable by any app while ours holds 127.0.0.1:P). macOS has only 127.0.0.1 on lo0, so this
        // is the predicate, not a live connection.
        assert!(!seam.take_reservation(SocketAddr::from(([127, 0, 0, 2], 40001))), "127.0.0.2 matched");
        assert!(!seam.take_reservation(SocketAddr::from(([127, 1, 2, 3], 40001))), "127.1.2.3 matched");
        assert!(!seam.take_reservation(SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 40001))), "::1 matched");
        let mapped = Ipv4Addr::LOCALHOST.to_ipv6_mapped();
        assert!(!seam.take_reservation(SocketAddr::from((mapped, 40001))), "::ffff:127.0.0.1 matched");
        assert!(!reservable_peer(lo(0)));
        assert!(!seam.take_reservation(lo(40002)), "wrong port matched");
        assert!(seam.take_reservation(lo(40001)));
        assert!(!seam.take_reservation(lo(40001)), "a reservation was used twice");
        // Expiry.
        seam.reserve(40003);
        {
            let mut g = crate::plock(&seam.reservations);
            let e = g.iter_mut().find(|e| e.0 == 40003).unwrap();
            e.1 = Instant::now().checked_sub(RESERVATION_TTL).unwrap();
        }
        assert!(!seam.take_reservation(lo(40003)), "an expired reservation matched");
        // Fix round 2: a released reservation is gone at once, and release of an unknown port is a no-op.
        seam.reserve(40004);
        seam.cancel_reservation(40005);
        seam.cancel_reservation(40004);
        assert!(!seam.take_reservation(lo(40004)), "a released reservation matched");
        // Bounded: the oldest is replaced once all RESERVATIONS entries are live.
        for p in 0..=RESERVATIONS as u16 {
            seam.reserve(41000 + p);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!seam.take_reservation(lo(41000)), "the oldest reservation was not replaced");
        assert!((1..=RESERVATIONS as u16).all(|p| seam.take_reservation(lo(41000 + p))));
    }

    /// Fix round 1: the bind is synchronous and its failure reaches the caller, and `admit` hands
    /// out the token only for a serving token-mode listener and a real port.
    #[test]
    fn bind_failure_is_returned_and_admit_is_gated() {
        let squatter = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let taken = squatter.local_addr().unwrap().to_string();
        assert!(start_control_listener(&taken, Some(TOKEN)).is_err(), "bind on a held port reported Ok");
        let l = start_control_listener("127.0.0.1:0", Some(TOKEN)).unwrap();
        assert!(l.is_serving());
        assert_eq!(l.admit(0), None);
        assert_eq!(l.admit(40100), Some(TOKEN));
        assert!(l.seam.take_reservation(SocketAddr::from((Ipv4Addr::LOCALHOST, 40100))), "admit did not reserve");
        // Fix round 2: release() withdraws an unconsumed reservation.
        assert_eq!(l.admit(40103), Some(TOKEN));
        l.release(40103);
        assert!(!l.seam.take_reservation(SocketAddr::from((Ipv4Addr::LOCALHOST, 40103))), "release left it claimable");
        let open = start_control_listener("127.0.0.1:0", None).unwrap();
        assert_eq!(open.admit(40101), None, "no-token listener handed out a token");
        // Not serving any more (the accept thread's guard dropped): no token.
        l.seam.serving.store(false, Ordering::Release);
        assert_eq!(l.admit(40102), None, "admit ignored a stopped listener");
    }

    #[test]
    fn rejection_log_is_throttled_by_time() {
        let seam = ControlSeam::new(Some(TOKEN));
        for _ in 0..500 {
            seam.reject(None, "test");
        }
        {
            let g = crate::plock(&seam.rejects);
            assert_eq!(g.total, 500);
            assert_eq!(g.suppressed, 499, "more than one line logged within one interval");
        }
        crate::plock(&seam.rejects).last = Instant::now().checked_sub(REJECT_LOG_EVERY);
        seam.reject(None, "test");
        let g = crate::plock(&seam.rejects);
        assert_eq!((g.total, g.suppressed), (501, 0), "the next interval's line did not flush the count");
    }

    #[test]
    fn authenticated_peer_gets_the_ack_then_the_gate_and_its_pcm_reaches_the_phone() {
        let _g = crate::plock(&SERIAL);
        let addr = listen(Some(TOKEN));
        for attempt in 1..=5 {
            let phone = arm();
            let mut c = TcpStream::connect(addr).unwrap();
            c.write_all(auth_line(&TOKEN).as_bytes()).unwrap();
            // Armed before the connect: the server tells the new peer at once, AFTER the ack.
            c.set_read_timeout(Some(Duration::from_millis(900))).unwrap();
            let mut rd = BufReader::new(c.try_clone().unwrap());
            let (mut ack, mut gate) = (String::new(), String::new());
            let ack_ok = rd.read_line(&mut ack).is_ok();
            let gate_ok = rd.read_line(&mut gate).is_ok();
            c.write_all(&mic_chunk()).unwrap();
            match got_packet(&phone) {
                Some(n) => {
                    assert!(ack_ok && gate_ok, "no ack/gate line for an authenticated peer");
                    assert_eq!(ack, AUTH_ACK);
                    assert_eq!(gate, "uplink on 16000 1\n");
                    assert!(n > CHUNK, "one RTP packet: header + {CHUNK} B PCM + tag, got {n}");
                    return;
                }
                None => eprintln!("[test] attempt {attempt}: no packet (uplink cleared concurrently?) — retrying"),
            }
        }
        panic!("authenticated PCM never reached the phone in 5 attempts");
    }

    #[test]
    fn no_token_mode_accepts_pcm_without_auth() {
        // carplayd's mode (box; ocbmd has no token).
        let _g = crate::plock(&SERIAL);
        let addr = listen(None);
        for attempt in 1..=5 {
            let phone = arm();
            let mut c = TcpStream::connect(addr).unwrap();
            c.write_all(&mic_chunk()).unwrap();
            if let Some(n) = got_packet(&phone) {
                assert!(n > CHUNK);
                return;
            }
            eprintln!("[test] attempt {attempt}: no packet — retrying");
        }
        panic!("no-token PCM never reached the phone in 5 attempts");
    }
}
