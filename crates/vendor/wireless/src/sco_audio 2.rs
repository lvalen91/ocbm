//! HFP call audio: accept the phone's SCO link and bridge it to the host app.
//!
//! **Android Auto carries telephony over Bluetooth, not over the projection link.** That is the
//! whole reason this file exists. CarPlay hands every audio stream — media, Siri, telephony — to
//! the accessory inside the AirPlay session, so the box never needed a SCO path and
//! `bt_bringup::bring_up` deliberately did not restore the vendor's SCO setup after its DOWN→UP
//! cycle (docs/wireless/01_BT_AND_RADIO.md "Accepted side effect"). Android Auto does the
//! opposite: gearhead routes both calls AND the Assistant through the connected headset —
//! `kxr.java:118-150` calls `BluetoothHeadset.startVoiceRecognition(device)`, then
//! `AudioManager.setCommunicationDevice`, then `startBluetoothSco()` — so the audio arrives on an
//! (e)SCO channel on OUR HFP link and never appears in the AA channel at all. A bench run on
//! 2026-09-03 showed exactly that: an Assistant key press produced `mic=0` AA packets, because the
//! phone was waiting for a `+BVRA` and a SCO connection we did not serve.
//!
//! ## What this module does
//!
//!   * Binds a `BTPROTO_SCO` **listening** socket. The kernel only accepts an incoming
//!     (e)SCO Connection Request when something is listening (`sco_connect_ind`), so this socket is
//!     the thing that makes the phone's `startBluetoothSco()` succeed at the HCI level.
//!   * On accept: reads CVSD narrowband audio — 8 kHz mono S16LE, delivered in whatever SCO packet
//!     size the controller negotiated (48 B is typical for CVSD/HV3) — aggregates it into 20 ms
//!     320-byte frames, and writes each frame to ocbmd's voice-sink seam on `:9003`, which
//!     forwards it to the app on `CH_ALT_AUDIO`.
//!   * Pumps the app's microphone PCM back into the SCO socket, one write per read so the uplink
//!     is paced by the controller's own SCO clock rather than by a timer we would have to trust.
//!
//! ## CVSD by default, mSBC behind a lever
//!
//! With `hfp_hf::wbs_enabled()` off — the default — we advertise `AT+BRSF=63`, HF bit 7 (codec
//! negotiation) is clear, the AG never sends `+BCS`, and this file behaves exactly as it did before
//! wideband existed: CVSD, `Voice: 0x0060`, 20 ms / 320 B PCM frames. That path is the proven one
//! and every line below keeps it byte-identical.
//!
//! With the lever on and the AG agreeing, the AG picks mSBC and TWO things change at once:
//!
//!   * the SCO socket's `BT_VOICE` setting becomes `0x0003` (transparent), so the controller stops
//!     decoding and hands us the AIR FRAMES — 60 B per 7.5 ms: a 2-byte H2 header, a 57-byte mSBC
//!     frame, one pad byte;
//!   * the downlink stops being PCM. This box has no mSBC decoder and deliberately grows none: each
//!     SCO read goes to the voice seam VERBATIM as one `SEAM_PKT_PLAIN` under a `SEAM_FORMAT` of
//!     `ocbm_proto::SEAM_CODEC_MSBC`, and the app decodes. Aggregating to 20 ms here would be
//!     actively wrong — it would split frames across messages for no benefit, and the host
//!     resynchronises on H2, not on our framing.
//!
//! The uplink inverts that: the app hands back whole 60 B packets it encoded, and we write them
//! unmodified, one per SCO read. An underrun therefore SKIPS a write instead of sending silence —
//! there is no such thing as a synthesised silent mSBC packet without an encoder, and a buffer of
//! zeros is not one; the AG tolerates a missing eSCO packet and does not tolerate a corrupt frame.
//!
//! ## Bounded, always
//!
//! Every socket in here carries a 1 s `SO_RCVTIMEO`/`SO_SNDTIMEO` and every loop re-reads the
//! shutdown flag between operations — the same discipline as `hfp_hf::arm_socket_timeouts` and
//! `bt_common::rfcomm`'s listener, and for the same reason: a peer that opens a channel and then
//! says nothing must not park a thread the session teardown joins on.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use box_common::flags::{self, ProjectionOwner};

// ---------------------------------------------------------------------------------------------
// Wire constants
// ---------------------------------------------------------------------------------------------

/// ocbmd's voice-sink seam. Whatever is written here is chunked onto `CH_ALT_AUDIO` verbatim —
/// ocbmd is a dumb byte pipe on this port, so the framing below is ours to define and ours to keep
/// self-synchronizing.
const VOICE_SEAM_ADDR: &str = "127.0.0.1:9003";

/// airplayd's mic-uplink seam ("control-in"). During an Android Auto session airplayd is not
/// running, so this daemon LISTENS here instead and speaks the identical protocol ocbmd already
/// implements against airplayd: `mic <len>\n<pcm>` inbound, newline-framed
/// `uplink on <rate> <ch>` / `uplink off` outbound.
const MIC_SEAM_ADDR: &str = "127.0.0.1:9112";

/// The audio-seam magic, byte-identical to the video seam's and to `receiver::session::SEAM_MAGIC`.
const SEAM_MAGIC: [u8; 4] = [0x53, 0x45, 0x41, 0x56]; // "SEAV"
const SEAM_FORMAT: u8 = 0x02;

/// Stream id for the HFP/SCO telephony lane. ASCII `HFPSCO` + a stream ordinal, so a scid seen in a
/// host-side log names its own origin. Fixed, because there is exactly one SCO channel at a time —
/// the AG tears the old one down before opening another.
pub const SCO_SCID: u64 = 0x4846_5053_434F_0001;

/// CVSD narrowband: 8 kHz, mono, 16-bit.
pub const SCO_RATE: u32 = 8000;
pub const SCO_CHANNELS: u8 = 1;
pub const SCO_BITS: u8 = 16;
/// mSBC wideband: 16 kHz mono once DECODED. The wire is a bitstream, not samples.
pub const WB_RATE: u32 = 16000;
/// `SEAM_FORMAT` `audio_type`: 1 = telephony.
const ATYPE_TELEPHONY: u8 = 1;
/// `SEAM_FORMAT` `codec`: 0 = PCM.
const CODEC_PCM: u8 = 0;

/// One transparent-eSCO mSBC packet: 2 B H2 header + 57 B mSBC frame + 1 B pad, every 7.5 ms.
/// Fixed by HFP 1.6 §5.7.4, not by the controller — which is why the uplink can use it as a write
/// unit while the DOWNLINK still forwards whatever length the socket hands us.
pub const MSBC_PKT_BYTES: usize = 60;

// The BT_VOICE socket option (kernel 3.11+, so present on this box's 3.14). Hand-declared for the
// same reason `AF_BLUETOOTH`/`BTPROTO_SCO` are: `libc` exposes no Bluetooth socket constants.
const SOL_BLUETOOTH: libc::c_int = 274;
const BT_VOICE: libc::c_int = 11;
/// `BT_DEFER_SETUP`. Without it the kernel answers an incoming (e)SCO request from
/// `hci_conn_request_evt`, which builds `Accept_Synchronous_Connection_Request` out of the HCI
/// GLOBAL `hdev->voice_setting` and ignores the socket entirely — so `BT_VOICE` would be set and
/// never consulted, and a negotiated mSBC channel would arrive CVSD-decoded. With it, the accept is
/// deferred to `sco_conn_defer_accept(hcon, sco_pi(sk)->setting)`, i.e. to THIS socket's air mode.
const BT_DEFER_SETUP: libc::c_int = 7;
/// `struct bt_voice { __u16 setting; }`.
#[repr(C)]
#[derive(Clone, Copy)]
struct BtVoice {
    setting: u16,
}
/// Air mode "transparent data": the controller passes the eSCO payload through undecoded, which is
/// the only way mSBC frames can reach userspace at all.
const BT_VOICE_TRANSPARENT: u16 = 0x0003;
/// The kernel's own default: CVSD, 16-bit linear, 2's complement, MSB position 8.
const BT_VOICE_CVSD_16BIT: u16 = 0x0060;

/// Which codec the AG negotiated for this headset link, and therefore what every stage of the
/// pipeline does. One value, set by the AT layer on `+BCS` and read by both audio threads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoCodec {
    /// The default and the only proven path: the controller decodes, we carry 8 kHz PCM.
    Cvsd,
    /// Wideband. The controller passes air frames through; the app decodes and encodes them.
    Msbc,
}

impl ScoCodec {
    pub fn as_str(self) -> &'static str {
        match self {
            ScoCodec::Cvsd => "CVSD",
            ScoCodec::Msbc => "mSBC",
        }
    }
    fn from_u8(v: u8) -> Self {
        if v == 1 { ScoCodec::Msbc } else { ScoCodec::Cvsd }
    }
    fn as_num(self) -> u8 {
        match self {
            ScoCodec::Cvsd => 0,
            ScoCodec::Msbc => 1,
        }
    }
    /// The `BT_VOICE` air-mode setting this codec needs on the SCO socket.
    fn voice_setting(self) -> u16 {
        match self {
            ScoCodec::Cvsd => BT_VOICE_CVSD_16BIT,
            ScoCodec::Msbc => BT_VOICE_TRANSPARENT,
        }
    }
    /// The `SEAM_FORMAT` codec byte the host keys its decoder off.
    fn seam_codec(self) -> u8 {
        match self {
            ScoCodec::Cvsd => CODEC_PCM,
            ScoCodec::Msbc => ocbm_proto::SEAM_CODEC_MSBC,
        }
    }
    /// The DECODED sample rate — what `SEAM_FORMAT` and the mic seam both advertise. For mSBC this
    /// describes the audio, never the payload.
    fn rate(self) -> u32 {
        match self {
            ScoCodec::Cvsd => SCO_RATE,
            ScoCodec::Msbc => WB_RATE,
        }
    }
    /// Bytes of uplink the app owes us per write, or 0 for "whatever the read consumed" (CVSD).
    fn uplink_unit(self) -> usize {
        match self {
            ScoCodec::Cvsd => 0,
            ScoCodec::Msbc => MSBC_PKT_BYTES,
        }
    }
    /// Ceiling on buffered uplink (300 ms either way).
    fn uplink_max(self) -> usize {
        match self {
            ScoCodec::Cvsd => UPLINK_MAX_BYTES,
            // 300 ms / 7.5 ms = 40 packets.
            ScoCodec::Msbc => MSBC_PKT_BYTES * 40,
        }
    }
}

/// 20 ms of 8 kHz mono S16LE. The frame size the app's playback path already expects from every
/// other audio lane, and small enough that a whole frame's loss is inaudible.
pub const FRAME_BYTES: usize = (SCO_RATE as usize / 50) * 2;

/// Ceiling on buffered uplink PCM (300 ms). Beyond this the app is producing faster than the SCO
/// clock consumes and the ONLY thing more buffering buys is latency, so the oldest audio is
/// dropped. Unbounded growth here would also be a slow leak on a box built with `panic = "abort"`.
const UPLINK_MAX_BYTES: usize = (SCO_RATE as usize / 1000) * 2 * 300;

const AF_BLUETOOTH: libc::sa_family_t = 31;
const BTPROTO_SCO: libc::c_int = 2;

#[repr(C)]
#[derive(Clone, Copy)]
struct SockaddrSco {
    sco_family: libc::sa_family_t,
    sco_bdaddr: [u8; 6],
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn log(m: &str) {
    println!("@{} [sco] {m}", now_ms());
}

// ---------------------------------------------------------------------------------------------
// Pure wire encoders — the half worth testing on a host
// ---------------------------------------------------------------------------------------------

/// `[u32 BE len][SEAM_MAGIC][marker][scid 8 LE][body…]`, `len` counting magic + marker + body.
fn seam_msg(marker: u8, scid: u64, body: &[u8]) -> Vec<u8> {
    let len = 4 + 1 + 8 + body.len();
    let mut m = Vec::with_capacity(4 + len);
    m.extend_from_slice(&(len as u32).to_be_bytes());
    m.extend_from_slice(&SEAM_MAGIC);
    m.push(marker);
    m.extend_from_slice(&scid.to_le_bytes());
    m.extend_from_slice(body);
    m
}

/// `SEAM_FORMAT` for the telephony lane: `audio_type=1`, mono, 16-bit, and either PCM at 8 kHz
/// (CVSD) or `SEAM_CODEC_MSBC` at 16 kHz (wideband).
///
/// Sent ONCE per SCO connection, before the first frame — the host keys its playback path off it,
/// and a frame that arrives for an unknown scid has nothing to play into. `bits`/`rate` describe the
/// DECODED audio in both cases; under mSBC the payload is a bitstream and the codec byte is the only
/// thing that says so.
pub fn sco_format_msg(scid: u64, codec: ScoCodec) -> Vec<u8> {
    let mut body = Vec::with_capacity(8);
    body.push(codec.seam_codec());
    body.extend_from_slice(&codec.rate().to_le_bytes());
    body.push(SCO_CHANNELS);
    body.push(SCO_BITS);
    body.push(ATYPE_TELEPHONY);
    seam_msg(SEAM_FORMAT, scid, &body)
}

/// The mic seam's `uplink on` line for a codec: `uplink on 8000 1` for CVSD — byte-identical to what
/// airplayd sends and to what shipped — and `uplink on 16000 1 msbc` for wideband.
///
/// The fourth token is deliberately ADDITIVE. ocbmd and airplayd both parse this line by
/// whitespace-split and both ignore a token they do not expect, so a box that speaks it to an old
/// host degrades to "16 kHz mono PCM", which is the wrong request but not a desync — whereas
/// changing the first three tokens' meaning would break every existing reader.
pub fn uplink_on_line(codec: ScoCodec) -> String {
    match codec {
        ScoCodec::Cvsd => format!("uplink on {SCO_RATE} {SCO_CHANNELS}"),
        ScoCodec::Msbc => format!("uplink on {WB_RATE} {SCO_CHANNELS} msbc"),
    }
}

/// `SEAM_PKT_PLAIN` carrying one 20 ms frame of raw S16LE. Unencrypted by construction: the
/// controller already decoded the CVSD, there is no RTP and no per-stream key.
pub fn sco_frame_msg(scid: u64, pcm: &[u8]) -> Vec<u8> {
    seam_msg(ocbm_proto::SEAM_PKT_PLAIN, scid, pcm)
}

/// Aggregate SCO packets into fixed-size frames.
///
/// SCO sockets are `SOCK_SEQPACKET`, so a read returns whole controller packets — for CVSD/HV3 that
/// is 48 bytes at 6 packets per 20 ms. An ODD-length packet is the one thing that can permanently
/// destroy the stream: every following sample would be assembled from the second byte of one and
/// the first of the next, which is not quiet distortion but full-scale noise. It cannot be a split
/// sample (SEQPACKET has no partial reads), so it is a misaligned packet, and the repair is to drop
/// the stray byte and say so.
pub struct FrameAggregator {
    buf: Vec<u8>,
    frame: usize,
    /// One-shot latch so a controller that always delivers odd packets logs once, not 50×/s.
    pub odd_seen: bool,
}

impl FrameAggregator {
    pub fn new(frame: usize) -> Self {
        Self { buf: Vec::with_capacity(frame * 2), frame: frame.max(2), odd_seen: false }
    }

    /// Feed one SCO packet; returns every complete frame it completed.
    pub fn push(&mut self, pkt: &[u8]) -> Vec<Vec<u8>> {
        let usable = if pkt.len() % 2 == 1 {
            self.odd_seen = true;
            &pkt[..pkt.len() - 1]
        } else {
            pkt
        };
        self.buf.extend_from_slice(usable);
        let mut out = Vec::new();
        while self.buf.len() >= self.frame {
            out.push(self.buf.drain(..self.frame).collect());
        }
        out
    }

    /// Bytes held back waiting for the rest of a frame. Always `< frame`.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }
}

/// The `mic <len>` header of the uplink seam's inbound framing. `None` for any other line — the
/// seam also carries `touch`/`cmd` lines that are not this lane's business.
///
/// The cap matches ocbmd's own (`forward_mic` refuses > 1 MiB), so a corrupt length can never make
/// us allocate against a peer's say-so.
pub fn parse_mic_header(line: &str) -> Option<usize> {
    let rest = line.trim_end().strip_prefix("mic ")?;
    match rest.trim().parse::<usize>() {
        Ok(n) if n > 0 && n <= (1 << 20) => Some(n),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Uplink {
    buf: VecDeque<u8>,
    /// Uplink packets the app did not have ready in time — silence under CVSD, a SKIPPED write under
    /// mSBC. Logged periodically: a steady underrun means the app is not capturing, which is
    /// otherwise invisible from this side.
    underruns: u64,
}

impl Uplink {
    /// CVSD: take exactly `n` bytes, zero-padding a short buffer. The SCO link is isochronous:
    /// there is no such thing as "send less this tick", so a gap in the app's audio has to become
    /// silence — and for CVSD, silence IS a run of zero samples.
    fn take(&mut self, n: usize) -> Vec<u8> {
        let have = self.buf.len().min(n);
        let mut out: Vec<u8> = self.buf.drain(..have).collect();
        if have < n {
            self.underruns += 1;
            out.resize(n, 0);
        }
        out
    }

    /// mSBC: one WHOLE packet or nothing.
    ///
    /// The `None` is the point. A short mSBC write is not quiet audio, it is a corrupt frame: the
    /// AG's decoder would find no H2 sync where it expects one and mute or squelch. There is no
    /// all-silence packet to substitute either — a valid one is an ENCODER's output, and this box has
    /// no encoder — so the honest move is to send nothing that tick. eSCO carries a packet-loss
    /// concealment path for exactly this, and a dropped packet is 7.5 ms.
    fn take_packet(&mut self, n: usize) -> Option<Vec<u8>> {
        if n == 0 || self.buf.len() < n {
            self.underruns += 1;
            return None;
        }
        Some(self.buf.drain(..n).collect())
    }

    /// Buffer app audio, dropping the OLDEST past `max` — beyond it, more buffering buys only
    /// latency, and unbounded growth would be a slow leak on a box built with `panic = "abort"`.
    ///
    /// `unit` (0 for CVSD) keeps the drop a whole number of packets: trimming 13 bytes off the front
    /// of an mSBC queue would leave every later `take_packet` straddling two air frames, which is the
    /// permanent version of the transient this trim exists to fix.
    fn push(&mut self, data: &[u8], max: usize, unit: usize) {
        self.buf.extend(data);
        if self.buf.len() > max {
            let mut excess = self.buf.len() - max;
            if unit > 1 {
                excess = excess.div_ceil(unit).saturating_mul(unit).min(self.buf.len());
            }
            self.buf.drain(..excess);
        }
    }
}

struct State {
    /// The AG told us audio is coming (ringing / active call / `+BVRA: 1`), so bring the mic seam
    /// up NOW. The app has to receive `uplink on` and start capturing before the first SCO packet
    /// arrives, or the onset of the call is clipped.
    armed: AtomicBool,
    sco_open: AtomicBool,
    uplink: Mutex<Uplink>,
    /// Write half of the currently-connected mic-seam peer (ocbmd), for the `uplink on/off`
    /// back-channel.
    mic_tx: Mutex<Option<TcpStream>>,
    frames: AtomicU64,
    /// The negotiated codec, as [`ScoCodec::as_num`]. Written by the AT thread on `+BCS`, read by
    /// both audio threads. Starts CVSD, which is what the AG opens when nothing is negotiated.
    codec: AtomicU8,
    /// Whether the listener actually carries `BT_DEFER_SETUP`. When it does not, the kernel picks
    /// the air mode from `hdev->voice_setting` and nothing this process does to a socket can change
    /// it — so wideband must be refused rather than promised (`set_codec` returns false and the AT
    /// layer answers `AT+BAC=1`).
    defer_setup: AtomicBool,
    /// Latched when a CHILD's `BT_VOICE` setsockopt failed. One-way, for the life of the link: the
    /// air mode is applied per accepted connection, so a failure there is a property of this kernel
    /// and this controller, not of that one call, and re-offering mSBC afterwards would produce the
    /// same dropped SCO connection on every attempt.
    no_transparent: AtomicBool,
}

impl State {
    fn wants_audio(&self) -> bool {
        self.armed.load(Ordering::Relaxed) || self.sco_open.load(Ordering::Relaxed)
    }
    fn codec(&self) -> ScoCodec {
        ScoCodec::from_u8(self.codec.load(Ordering::Relaxed))
    }
}

/// `setsockopt(SOL_BLUETOOTH, BT_VOICE)` — the air mode the next accepted SCO connection inherits.
///
/// Applied to the LISTENER, never to an accepted socket: the kernel fixes the air mode when it
/// answers the AG's Connection Request, so by the time `accept` returns it is far too late.
fn set_defer_setup(fd: libc::c_int) -> std::io::Result<()> {
    let on: libc::c_int = 1;
    let r = unsafe {
        libc::setsockopt(
            fd,
            SOL_BLUETOOTH,
            BT_DEFER_SETUP,
            &on as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if r < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Finish a deferred accept, and only then let the pump have the socket.
///
/// The shape is BlueZ's own `bt_io_accept` with the per-connection air mode in front, and each step
/// is load-bearing:
///   0. `apply_voice` — see below.
///   1. If the socket is ALREADY writable there was nothing deferred (a kernel without the option,
///      or a listener whose `setsockopt` failed) — return, and in particular do not do step 2, which
///      on a live SEQPACKET socket would truncate a real audio packet down to one byte.
///   2. One read is the trigger. `sco_sock_recvmsg` sees `BT_CONNECT2 + BT_SK_DEFER_SETUP`, calls
///      `sco_conn_defer_accept(hcon, sco_pi(sk)->setting)` — the air mode this whole feature exists
///      to control — and returns 0 IMMEDIATELY. That zero is success, not an EOF; treating it as a
///      closed link (which the pump's own `Ok(0)` arm would) is how this reads as "the phone hung
///      up" on every single call.
///   3. `POLLOUT` is the link coming up: `bt_sock_poll` withholds it in `BT_CONNECT2`/`BT_CONFIG`
///      and grants it in `BT_CONNECTED`, so it is the kernel's own answer to "is the eSCO channel
///      live", and `POLLHUP` is the negotiation failing (`sco_conn_del` → `BT_CLOSED`).
///
/// `apply_voice` runs FIRST and its failure aborts the whole thing. The child is in `BT_CONNECT2`
/// at that instant, which with `BT_OPEN`/`BT_BOUND` is one of the only three states kernel 3.14's
/// `sco_sock_setsockopt` accepts `BT_VOICE` in — and after the trigger read it is `BT_CONFIG`, where
/// it does not. So this is not merely the natural order: it is the only window that exists, and it
/// is passed in as a closure so the ordering is a property of this function rather than of every
/// call site.
///
/// Bounded at 5 s and sliced at 500 ms so the daemon can still go quiet while an AG dithers.
fn complete_deferred_accept<F>(
    fd: libc::c_int,
    shutdown: &AtomicBool,
    apply_voice: F,
) -> std::io::Result<()>
where
    F: FnOnce() -> std::io::Result<()>,
{
    apply_voice()?;
    let mut pfd = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
    // SAFETY: one initialised pollfd, count 1, on a descriptor this function borrows.
    if unsafe { libc::poll(&mut pfd, 1, 0) } > 0 && pfd.revents & libc::POLLOUT != 0 {
        return Ok(());
    }
    let mut c = [0u8; 1];
    // SAFETY: a 1-byte write into a local buffer.
    let n = unsafe { libc::read(fd, c.as_mut_ptr() as *mut libc::c_void, 1) };
    if n < 0 {
        let e = std::io::Error::last_os_error();
        // The trigger landed either way — these three mean "no data", not "no accept".
        if !matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::Interrupted
        ) {
            return Err(e);
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("the daemon is going quiet"));
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the controller never reported the (e)SCO link up",
            ));
        }
        let mut pfd = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
        // SAFETY: as above.
        let r = unsafe { libc::poll(&mut pfd, 1, 500) };
        if r < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(std::io::Error::other(
                "the controller refused or dropped the (e)SCO setup",
            ));
        }
        if pfd.revents & libc::POLLOUT != 0 {
            return Ok(());
        }
    }
}

/// Put the just-accepted (deferred) child in the air mode the AG negotiated, and log which.
///
/// The CHILD and not the listener: 3.14 refuses `BT_VOICE` on a `BT_LISTEN` socket outright, and the
/// deferred child in `BT_CONNECT2` is the one place a per-connection value can be set — which is
/// also exactly what `sco_conn_defer_accept(hcon, sco_pi(sk)->setting)` will read one step later.
fn apply_child_voice(fd: libc::c_int, codec: ScoCodec) -> std::io::Result<()> {
    set_voice_setting(fd, codec.voice_setting())?;
    match codec {
        ScoCodec::Msbc => log("SCO voice setting -> transparent (mSBC)"),
        ScoCodec::Cvsd => log("SCO voice setting -> CVSD"),
    }
    Ok(())
}

fn set_voice_setting(fd: libc::c_int, setting: u16) -> std::io::Result<()> {
    let v = BtVoice { setting };
    let r = unsafe {
        libc::setsockopt(
            fd,
            SOL_BLUETOOTH,
            BT_VOICE,
            &v as *const BtVoice as *const libc::c_void,
            std::mem::size_of::<BtVoice>() as libc::socklen_t,
        )
    };
    if r < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Process-wide single-flight for the SCO path.
///
/// **Not defensive programming — a reachable interleaving.** `hold_headset_link_inner` runs in up to
/// THREE threads at once: the outbound reconnect attempt and the two inbound accept loops
/// (`HFP_HF_RFCOMM_CHANNEL` and `HSP_HS_RFCOMM_CHANNEL` in `main.rs`). A phone that connects both
/// records, or that connects in while we are dialling out, would construct two `ScoAudio`s. The
/// second one's SCO `bind` fails `EADDRINUSE` and its thread EXITS — so if the first link then
/// drops, the surviving link has no SCO listener at all and call audio dies with no log line
/// implicating the link that actually went away. The `:9112` listener would meanwhile spin on a
/// bind that can never succeed while the sibling holds it.
///
/// So the second instance is INERT and says so, and the SCO path always belongs to exactly one
/// headset link.
static SCO_PATH_TAKEN: AtomicBool = AtomicBool::new(false);

/// Owns the SCO listener and the mic-seam listener for the lifetime of one headset link.
pub struct ScoAudio {
    shutdown: Arc<AtomicBool>,
    state: Arc<State>,
    handles: Vec<std::thread::JoinHandle<()>>,
    /// False when another headset link already owns the SCO path; this handle then does nothing and
    /// must not release the flag on drop.
    owns_path: bool,
}

impl ScoAudio {
    /// Start the SCO listener and the mic-seam server. Never fails: a controller that cannot host a
    /// SCO socket, or a `:9112` still owned by a dying airplayd, must not take the headset link —
    /// and therefore gearhead's HFP gate — down with it. Every failure is logged and retried.
    pub fn start(local_bdaddr: Option<[u8; 6]>) -> ScoAudio {
        let shutdown = Arc::new(AtomicBool::new(false));
        let state = Arc::new(State {
            armed: AtomicBool::new(false),
            sco_open: AtomicBool::new(false),
            uplink: Mutex::new(Uplink::default()),
            mic_tx: Mutex::new(None),
            frames: AtomicU64::new(0),
            codec: AtomicU8::new(ScoCodec::Cvsd.as_num()),
            defer_setup: AtomicBool::new(false),
            no_transparent: AtomicBool::new(false),
        });
        let mut handles = Vec::with_capacity(2);
        let owns_path = !SCO_PATH_TAKEN.swap(true, Ordering::AcqRel);
        if !owns_path {
            log("another headset link already owns the SCO path — this link carries control only");
            return ScoAudio { shutdown, state, handles, owns_path };
        }
        {
            let (sd, st) = (shutdown.clone(), state.clone());
            handles.push(std::thread::spawn(move || sco_listen(local_bdaddr, &sd, &st)));
        }
        {
            let (sd, st) = (shutdown.clone(), state.clone());
            handles.push(std::thread::spawn(move || mic_seam_serve(&sd, &st)));
        }
        ScoAudio { shutdown, state, handles, owns_path }
    }

    /// The AG signalled that audio is imminent. Idempotent; only the first transition logs.
    pub fn arm(&self, why: &str) {
        if !self.owns_path {
            return;
        }
        if !self.state.armed.swap(true, Ordering::Relaxed) {
            log(&format!("armed by {why} — opening the mic seam ahead of the SCO connection"));
        }
    }

    /// No call and no voice recognition. Idempotent.
    pub fn disarm(&self, why: &str) {
        if !self.owns_path {
            return;
        }
        if self.state.armed.swap(false, Ordering::Relaxed) {
            log(&format!("disarmed by {why}"));
        }
    }

    /// Record the negotiated codec, and answer whether the AG can be TOLD we accept it. `false`
    /// means the caller must not send `AT+BCS=2` and should narrow the offer with `AT+BAC=1`
    /// (`hfp_hf::CodecChoice::NarrowToCvsd`) — the AG must never be left expecting an air mode this
    /// box cannot select.
    ///
    /// **It deliberately does NOT touch any socket, and that is a device finding, not a style
    /// choice.** Applying `BT_VOICE` to the LISTENER was the first implementation and it fails on
    /// this box: kernel 3.14's `sco_sock_setsockopt` accepts `BT_VOICE` only in `BT_OPEN`,
    /// `BT_BOUND` or `BT_CONNECT2`, and a listening socket is `BT_LISTEN`, so every call returned
    /// `EINVAL` — even for the CVSD 0x0060 it was already set to (device log, 2026-09-04 11:46Z).
    /// The two states that DO accept it are where the setting now happens: once on the listener
    /// between `bind` and `listen` (`BT_BOUND`, the default the child inherits), and once per
    /// accepted connection while the deferred child sits in `BT_CONNECT2`, before the trigger read.
    /// So the codec recorded here is a promise the ACCEPT path keeps, one connection later.
    pub fn set_codec(&self, codec: ScoCodec) -> bool {
        if !self.owns_path {
            // Not ours to promise. Another headset link holds the one SCO listener, and telling this
            // AG "transparent" would configure nothing while the AG happily opened a wideband
            // channel into the sibling's CVSD socket.
            log(&format!(
                "the AG asked for {} but another headset link owns the SCO path — answering CVSD only",
                codec.as_str()
            ));
            return false;
        }
        let prev = ScoCodec::from_u8(self.state.codec.swap(codec.as_num(), Ordering::Relaxed));
        if prev != codec {
            // Whatever the app queued was in the OTHER format. Feeding CVSD PCM into a transparent
            // channel (or mSBC frames into a CVSD one) is full-scale noise, and it would be sent
            // before the app could possibly have reacted to the new `uplink on`.
            if let Ok(mut u) = self.state.uplink.lock() {
                u.buf.clear();
            }
        }
        if codec == ScoCodec::Cvsd {
            return true;
        }
        if !self.state.defer_setup.load(Ordering::Relaxed) {
            // Without the deferred accept the kernel takes the air mode from `hdev->voice_setting`
            // and never reads the socket. Answering `AT+BCS=2` anyway is the exact failure this
            // check exists to prevent: the AG sends mSBC air frames into a channel the controller is
            // still decoding as CVSD, and the call is noise both ways.
            log("the SCO listener has no BT_DEFER_SETUP — the air mode cannot be selected per \
                 connection, so wideband is refused and the AG is offered CVSD only");
            self.state.codec.store(ScoCodec::Cvsd.as_num(), Ordering::Relaxed);
            return false;
        }
        if self.state.no_transparent.load(Ordering::Relaxed) {
            log("a previous connection could not be put in transparent mode — wideband stays \
                 refused on this link, the AG is offered CVSD only");
            self.state.codec.store(ScoCodec::Cvsd.as_num(), Ordering::Relaxed);
            return false;
        }
        true
    }
}

impl Drop for ScoAudio {
    /// Teardown is Drop and ONLY Drop, deliberately.
    ///
    /// An explicit `stop()` alongside it would be a second path that the eight `return`s in
    /// `reconnect::hold_headset_link_inner` could each forget — and forgetting means the SCO socket
    /// and `:9112` stay held by threads whose link is gone, so the NEXT headset link's bind fails
    /// for a reason nothing logs. Tying it to the binding's scope makes that unrepresentable.
    ///
    /// Bounded: every loop these threads run re-reads the shutdown flag at least once a second, so
    /// the join costs at most that.
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
        if self.owns_path {
            // Released only AFTER both threads have joined, so the next link's bind cannot race the
            // close of the socket this one held. Releasing before the join is the classic version of
            // this bug and would reintroduce exactly the EADDRINUSE the flag exists to prevent.
            SCO_PATH_TAKEN.store(false, Ordering::Release);
            let f = self.state.frames.load(Ordering::Relaxed);
            if f > 0 {
                log(&format!("SCO audio stopped after {f} frames on this headset link"));
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// SCO listener
// ---------------------------------------------------------------------------------------------

fn set_timeouts(fd: libc::c_int, secs: i64) -> std::io::Result<()> {
    // zeroed()+assign, not a struct literal: under `musl32_time64` (riscv32) these types carry
    // private padding and a literal does not compile. Same as `hfp_hf::arm_socket_timeouts`.
    let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
    // `as _`, not a direct assignment: `tv_sec` is `i32` on armv7-musl (32-bit `time_t`) and `i64`
    // on the x86_64/aarch64 hosts this crate's tests run on. A literal `i64` compiles on the host
    // and fails the box build — which is exactly how this line was written the first time.
    tv.tv_sec = secs as _;
    for opt in [libc::SO_RCVTIMEO, libc::SO_SNDTIMEO] {
        let r = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                opt,
                &tv as *const libc::timeval as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        };
        if r < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Open + bind + listen a SCO server socket.
///
/// `SOCK_SEQPACKET` is not a preference: SCO is packet-oriented and a stream socket would let a
/// read straddle two controller packets, which is how the odd-length misalignment in
/// [`FrameAggregator`] would become permanent instead of one-off.
///
/// CLOEXEC for the same reason `bt_common::rfcomm::open_listener` is: this listener is open while
/// `av::ensure_av_layer` fork+execs setsid-detached daemons, and without it they inherit the socket
/// and hold the SCO channel after this process dies.
fn sco_open_listener(local: Option<[u8; 6]>, initial: u16) -> std::io::Result<std::fs::File> {
    let fd = unsafe {
        libc::socket(
            AF_BLUETOOTH as libc::c_int,
            libc::SOCK_SEQPACKET | crate::cloexec::SOCK_CLOEXEC,
            BTPROTO_SCO,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let addr = SockaddrSco { sco_family: AF_BLUETOOTH, sco_bdaddr: local.unwrap_or([0u8; 6]) };
    let r = unsafe {
        libc::bind(
            fd,
            &addr as *const SockaddrSco as *const libc::sockaddr,
            std::mem::size_of::<SockaddrSco>() as libc::socklen_t,
        )
    };
    if r < 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // BT_VOICE goes on HERE, between `bind` and `listen`, and nowhere else on this socket. Kernel
    // 3.14's `sco_sock_setsockopt` accepts the option only in `BT_OPEN`, `BT_BOUND` or
    // `BT_CONNECT2`; after `listen` the socket is `BT_LISTEN` and every attempt is `EINVAL` — proven
    // on the device 2026-09-04, where it failed even for the 0x0060 the socket already had. This is
    // the DEFAULT that an accepted child inherits through `sco_sock_init`; the per-connection value
    // is applied to the child itself in `apply_child_voice`.
    if let Err(e) = set_voice_setting(fd, initial) {
        // Not fatal, and not silent: 0x0060 is also the kernel's own default, so a CVSD link is
        // unaffected. Only the wideband promise depends on this working, and `set_codec` refuses
        // wideband on any link whose accept path cannot select the air mode.
        log(&format!(
            "BT_VOICE setsockopt({initial:#06x}) on the bound listener failed: {e} (errno {:?})",
            e.raw_os_error()
        ));
    }
    if unsafe { libc::listen(fd, 1) } < 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // Bounds `accept`: BlueZ's `sco_sock_accept` waits with the socket's own receive timeout, so
    // this is what lets the loop below re-read the shutdown flag once a second instead of parking
    // forever on a phone that never places a call.
    if let Err(e) = set_timeouts(fd, 1) {
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // SAFETY: fd is a freshly opened, bound, listening, exclusively-owned descriptor.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

fn sco_listen(local: Option<[u8; 6]>, shutdown: &AtomicBool, state: &State) {
    let listener = match sco_open_listener(local, state.codec().voice_setting()) {
        Ok(l) => l,
        Err(e) => {
            // Not fatal to the headset link: the SLC is what gearhead's gate reads, and it is
            // already up. Only call audio is lost, and saying so is the whole point.
            log(&format!(
                "SCO listener could not bind: {e} — HFP call and Assistant audio will not flow \
                 (is the controller attached, and did radio_hal.sh sco_on run?)"
            ));
            return;
        }
    };
    let lfd = listener.as_raw_fd();
    // BT_DEFER_SETUP, for BOTH codecs and before anything can be accepted. On the non-deferred path
    // the kernel builds the accept from `hdev->voice_setting` and the `BT_VOICE` above is dead
    // configuration; on the deferred one it uses the socket's own setting, which is the only reason
    // the transparent air mode can ever reach the wire. Armed unconditionally so CVSD and mSBC take
    // one code path — a defer that only existed under the lever would leave the wideband case as the
    // only untested accept shape.
    match set_defer_setup(lfd) {
        Ok(()) => {
            // Stored BEFORE the listener dup below: `set_codec` reads this while holding the
            // listener mutex, so publishing in this order means "saw a listener" implies "saw this".
            state.defer_setup.store(true, Ordering::Relaxed);
            log("SCO listener armed with BT_DEFER_SETUP (voice setting applied per connection)");
        }
        Err(e) => {
            // CVSD keeps working exactly as it does today — this is the accept path that shipped.
            // Wideband cannot, and says so rather than promising an air mode it cannot select.
            log(&format!(
                "BT_DEFER_SETUP setsockopt failed: {e} (errno {:?}) — falling back to the \
                 non-deferred accept; the kernel will pick the air mode from hdev->voice_setting, \
                 so this link is CVSD only",
                e.raw_os_error()
            ));
            state.codec.store(ScoCodec::Cvsd.as_num(), Ordering::Relaxed);
        }
    }
    log("SCO listener up (CVSD 8 kHz mono) — awaiting the phone's audio connection");
    sco_accept_loop(lfd, shutdown, state);
}

/// The accept loop, split out so the listener's one-time setup above reads as a single unit —
/// every `BT_VOICE`/`BT_DEFER_SETUP` decision is made once, before anything can be accepted.
fn sco_accept_loop(lfd: libc::c_int, shutdown: &AtomicBool, state: &State) {
    while !shutdown.load(Ordering::Relaxed) {
        let cfd = unsafe { libc::accept(lfd, std::ptr::null_mut(), std::ptr::null_mut()) };
        if cfd < 0 {
            let e = std::io::Error::last_os_error();
            match e.kind() {
                std::io::ErrorKind::WouldBlock
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::Interrupted => continue,
                _ => {
                    log(&format!("SCO accept failed: {e} — listener closing"));
                    return;
                }
            }
        }
        // SAFETY: cfd is a freshly accepted, exclusively-owned descriptor.
        let conn = unsafe { std::fs::File::from_raw_fd(cfd) };
        if let Err(e) = set_timeouts(cfd, 1) {
            log(&format!("SCO: could not arm socket timeouts on the accepted link: {e} — dropping it rather than risk an unbounded read"));
            continue;
        }
        // Complete the deferred accept BEFORE anything else touches the socket: the child's air
        // mode goes on here (the only window for it), then the trigger read, then the wait for the
        // link. Nothing writes until the pump's first read has returned, and the pump does not start
        // until this does.
        if state.defer_setup.load(Ordering::Relaxed) {
            let want = state.codec();
            let mut voice_failed = false;
            let r = complete_deferred_accept(cfd, shutdown, || {
                apply_child_voice(cfd, want).inspect_err(|_| voice_failed = true)
            });
            if let Err(e) = r {
                if voice_failed {
                    // Latch, so the AT layer narrows to CVSD on the next `+BCS` (`AT+BAC=1`) instead
                    // of promising an air mode that drops the connection every time.
                    state.no_transparent.store(true, Ordering::Relaxed);
                    state.codec.store(ScoCodec::Cvsd.as_num(), Ordering::Relaxed);
                    log(&format!(
                        "BT_VOICE setsockopt({:#06x}) on the accepted child failed: {e} (errno \
                         {:?}) — dropping this connection; wideband is refused from here on and the \
                         AG will be offered CVSD only",
                        want.voice_setting(),
                        e.raw_os_error()
                    ));
                } else {
                    log(&format!("SCO deferred accept failed: {e} (errno {:?})", e.raw_os_error()));
                }
                continue; // `conn` drops here, closing the child
            }
        }
        state.sco_open.store(true, Ordering::Relaxed);
        let codec = state.codec();
        // The mSBC counterpart is logged on the FIRST READ instead, because the packet size the
        // controller chose is the one number a wideband bench actually needs and it is not knowable
        // here.
        if codec == ScoCodec::Cvsd {
            log("SCO connected — CVSD narrowband, 20 ms/320 B frames to the voice lane");
        }
        let (frames, residue, why) = sco_pump(&conn, shutdown, state);
        state.sco_open.store(false, Ordering::Relaxed);
        state.frames.fetch_add(frames, Ordering::Relaxed);
        log(&format!(
            "SCO closed after {frames} {} frames, {residue} B unframed ({why})",
            codec.as_str()
        ));
    }
}

/// One SCO connection. Returns `(frames forwarded, bytes left unframed, why it ended)`.
///
/// The residue is diagnostic and always `< 320`: SCO packets do not divide a 20 ms frame evenly, so
/// a healthy call ends with a partial frame in hand. A residue that is consistently the SAME value
/// across calls is normal; one that grows is not, and is the first thing to look at if the audio
/// ever sounds clocked wrong.
fn sco_pump(conn: &std::fs::File, shutdown: &AtomicBool, state: &State) -> (u64, usize, &'static str) {
    // Read ONCE, for the life of this connection. A `+BCS` that changes the codec always tears the
    // (e)SCO channel down first, so re-reading per packet could only ever produce a pipeline whose
    // halves disagree about what the bytes in flight are.
    let codec = state.codec();
    let mut sink = VoiceSink::new(codec);
    let mut agg = FrameAggregator::new(FRAME_BYTES);
    let mut buf = [0u8; 1024];
    let mut frames = 0u64;
    let mut reader: &std::fs::File = conn;
    let mut writer: &std::fs::File = conn;
    let mut logged_odd = false;
    let mut logged_write_err = false;
    let mut last_underrun_log = Instant::now();
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return (frames, agg.pending(), "the daemon is going quiet");
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => return (frames, agg.pending(), "the phone closed the audio channel"),
            Ok(n) => n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // A whole second with no SCO packet. The link is still open as far as the kernel is
                // concerned; do NOT write uplink here — every write is paced by a read, and a write
                // with no matching read would run the uplink ahead of the controller's clock.
                continue;
            }
            Err(e) => {
                let why = if e.raw_os_error() == Some(libc::ECONNRESET) { "reset" } else { "read error" };
                return (frames, agg.pending(), why);
            }
        };

        if codec == ScoCodec::Msbc {
            // VERBATIM, one seam message per SCO read: no aggregation, no H2 rewriting, no odd-byte
            // repair. Everything the CVSD path does to keep S16 alignment is meaningless on a
            // bitstream, and the app resynchronises on the H2 header (0x01 then
            // 0x08/0x38/0xC8/0xF8), which only survives if we do not touch it.
            if frames == 0 {
                log(&format!(
                    "SCO connected — mSBC wideband, transparent eSCO packets of {n} B to the voice lane"
                ));
                sink.arm();
            }
            sink.send(&buf[..n]);
            frames += 1;
        } else {
            for frame in agg.push(&buf[..n]) {
                if frames == 0 {
                    sink.arm();
                }
                sink.send(&frame);
                frames += 1;
            }
        }
        if agg.odd_seen && !logged_odd {
            logged_odd = true;
            log("SCO delivered an ODD-length packet — dropping the stray byte to keep S16 alignment (every later sample would otherwise be assembled from two different ones)");
        }

        // Uplink, paced by the read we just did: exactly as many bytes back as came in, so the
        // controller's own SCO clock drives the rate and nothing here has to guess at it.
        // The lock is taken, drained, and RELEASED BEFORE the log below. Holding a mutex across a
        // `println!` means holding it across the stdout lock and, through the supervisor's pipe,
        // across a write to a reader we do not control — the mic thread would block on `push` for
        // as long as that took, and a stalled reader would turn a log line into an audio dropout.
        // A `+BCS` that lands while THIS connection is open would leave the mic seam announcing one
        // format while the socket still carries the other — the app's next chunk would be encoded
        // for a channel that does not exist yet. Out of spec (the AG tears (e)SCO down before it
        // renegotiates) and therefore never seen, but the failure mode is noise on a live call, and
        // going mute for the rest of a connection is the cheaper wrong answer.
        if state.codec() != codec {
            continue;
        }
        let (out, underruns) = match state.uplink.lock() {
            Ok(mut u) => {
                // CVSD pads a short buffer with silence; mSBC cannot (see `take_packet`) and skips
                // the write entirely.
                let o = match codec {
                    ScoCodec::Cvsd => Some(u.take(n)),
                    ScoCodec::Msbc => u.take_packet(MSBC_PKT_BYTES),
                };
                let un = if last_underrun_log.elapsed() >= Duration::from_secs(5) {
                    std::mem::take(&mut u.underruns)
                } else {
                    0
                };
                (o, un)
            }
            // A poisoned lock is a panic in the mic thread, which on this box (`panic = "abort"`)
            // cannot happen — but silence beats propagating the panic into the audio path.
            Err(_) => (matches!(codec, ScoCodec::Cvsd).then(|| vec![0u8; n]), 0),
        };
        if underruns > 0 {
            last_underrun_log = Instant::now();
            let what = match codec {
                ScoCodec::Cvsd => "packets of silence sent to the phone",
                ScoCodec::Msbc => "eSCO packets skipped (no silent mSBC frame exists to send)",
            };
            log(&format!("uplink underrun: {underruns} {what} in the last 5 s (is the app capturing?)"));
        }
        let Some(out) = out else { continue };
        match writer.write_all(&out) {
            Ok(()) => {}
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::Interrupted =>
            {
                // Dropped uplink packet. Never retried: SCO is isochronous, and a late packet is
                // worse than a missing one.
            }
            // EINVAL/EMSGSIZE only — every OTHER error still tears the pump down below.
            Err(ref e)
                if codec == ScoCodec::Msbc
                    && matches!(e.raw_os_error(), Some(libc::EINVAL) | Some(libc::EMSGSIZE)) =>
            {
                // `sco_send_frame` rejects a write longer than the connection's SCO MTU with
                // `EINVAL`. The vendor's own `hciconfig hci0 scomtu 240:32` is 240 BYTES over 32
                // packets, so a 60 B mSBC packet fits with room to spare and this is not expected —
                // which is exactly why it is worth one loud line if it ever happens. Logged once
                // with the read size (the controller's own packet length) and the call kept up:
                // downlink audio still flows, and a mute uplink beats a dropped call.
                if !logged_write_err {
                    logged_write_err = true;
                    log(&format!(
                        "uplink write of {} B failed: {e} — the SCO MTU cannot take a whole mSBC \
                         packet (downlink packets are {n} B); the uplink is mute for this call",
                        out.len()
                    ));
                }
            }
            Err(_) => return (frames, agg.pending(), "uplink write failed"),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Downlink: the voice seam to ocbmd
// ---------------------------------------------------------------------------------------------

/// The `:9003` connection, with a bounded reconnect. Lazily armed on the first frame so a call that
/// never produces audio does not churn a connection.
struct VoiceSink {
    sock: Option<TcpStream>,
    last_try: Option<Instant>,
    dropped: u64,
    reported: bool,
    /// Fixed for the life of one SCO connection — it is what the `SEAM_FORMAT` declared, and a
    /// reconnect must re-declare the SAME thing or the host's decoder and the payload disagree.
    codec: ScoCodec,
}

impl VoiceSink {
    fn new(codec: ScoCodec) -> Self {
        Self { sock: None, last_try: None, dropped: 0, reported: false, codec }
    }

    /// Connect and declare the format. Retries at most every 2 s.
    fn arm(&mut self) {
        if self.sock.is_some() {
            return;
        }
        if let Some(t) = self.last_try {
            if t.elapsed() < Duration::from_secs(2) {
                return;
            }
        }
        self.last_try = Some(Instant::now());
        let addr: SocketAddr = match VOICE_SEAM_ADDR.parse() {
            Ok(a) => a,
            Err(_) => return,
        };
        match TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                // 250 ms, and the number matters. This write happens INSIDE the SCO read/write
                // loop, so however long it blocks is time we are not reading the SCO socket — the
                // controller's buffer overruns and the uplink loses its pacing. A 1 s timeout would
                // cost ~50 SCO packets before we noticed; 250 ms costs ~12 and still leaves two
                // orders of magnitude over a healthy loopback write.
                let _ = s.set_write_timeout(Some(Duration::from_millis(250)));
                let mut s = s;
                if s.write_all(&sco_format_msg(SCO_SCID, self.codec)).is_ok() {
                    let what = match self.codec {
                        ScoCodec::Cvsd => "PCM",
                        ScoCodec::Msbc => "mSBC",
                    };
                    log(&format!(
                        "voice sink {VOICE_SEAM_ADDR} connected — SEAM_FORMAT scid={SCO_SCID:#018x} {what} {} Hz mono 16-bit telephony",
                        self.codec.rate()
                    ));
                    self.sock = Some(s);
                    self.reported = false;
                }
            }
            Err(e) => {
                if !self.reported {
                    self.reported = true;
                    log(&format!("voice sink {VOICE_SEAM_ADDR} connect failed: {e} — call audio has nowhere to go (is ocbmd up with a host subscribed?)"));
                }
            }
        }
    }

    fn send(&mut self, pcm: &[u8]) {
        self.arm();
        let Some(s) = self.sock.as_mut() else {
            self.dropped += 1;
            return;
        };
        if s.write_all(&sco_frame_msg(SCO_SCID, pcm)).is_err() {
            // Drop the socket rather than keep writing into it: ocbmd replaces a seam producer
            // without draining the old one, so a half-written message is exactly the desync the
            // SEAM_MAGIC exists to recover from. Reconnecting starts a clean message boundary and
            // re-sends SEAM_FORMAT, which the host needs anyway for the new producer.
            log("voice sink write failed — reconnecting on the next frame");
            self.sock = None;
        }
    }
}

impl Drop for VoiceSink {
    fn drop(&mut self) {
        if let Some(s) = self.sock.take() {
            let _ = s.shutdown(Shutdown::Both);
        }
        if self.dropped > 0 {
            log(&format!("voice sink: {} frames discarded with no connection", self.dropped));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Uplink: we become airplayd's mic seam
// ---------------------------------------------------------------------------------------------

/// Serve `:9112` while this box owns wireless Android Auto and the AG wants audio.
///
/// **Why the ownership gate is load-bearing.** `:9112` belongs to airplayd, which binds it at
/// startup for every CarPlay session. If we held it across a CarPlay session start, airplayd's bind
/// would fail and the WIRED/wireless CarPlay microphone — Siri, phone calls — would be dead with
/// only a line in airplayd's own log to say so. The projection owner flag is set before
/// `av::ensure_av_layer` spawns airplayd, so releasing on any owner that is not `wireless-aa`
/// closes that window. The converse race — airplayd still dying while we try to bind — is handled
/// by simply retrying, since a stale listener disappears when its process does.
fn mic_seam_serve(shutdown: &AtomicBool, state: &State) {
    let mut listener: Option<TcpListener> = None;
    let mut bind_reported = false;
    let mut gate_reported = ProjectionOwner::None;
    while !shutdown.load(Ordering::Relaxed) {
        let owner = flags::owner();
        // Any Android Auto owner (wireless or WIRED — a wired session's calls ride this link too, 2026-09-04).
        let allowed = owner == ProjectionOwner::WirelessAa || owner == ProjectionOwner::WiredAa;
        if !state.wants_audio() || !allowed {
            if listener.take().is_some() {
                notify(state, "uplink off");
                close_mic_peer(state);
                log("mic seam released 127.0.0.1:9112 — sent 'uplink off'");
            }
            if !allowed && state.wants_audio() && gate_reported != owner {
                gate_reported = owner;
                log(&format!(
                    "mic uplink idle: the box does not own an Android Auto session (owner={}) — not binding {MIC_SEAM_ADDR}",
                    owner.as_str()
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }
        gate_reported = ProjectionOwner::None;

        if listener.is_none() {
            match TcpListener::bind(MIC_SEAM_ADDR) {
                Ok(l) => {
                    bind_reported = false;
                    log(&format!(
                        "mic seam listening {MIC_SEAM_ADDR} (we are airplayd's mic seam for this session)"
                    ));
                    listener = Some(l);
                }
                Err(e) => {
                    if !bind_reported {
                        bind_reported = true;
                        log(&format!("mic seam bind {MIC_SEAM_ADDR} failed: {e} — retrying (a previous airplayd may still be dying)"));
                    }
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
            }
        }

        // Accept under the listener's own 1 s receive timeout, so the ownership and wants_audio
        // checks at the top of the loop run once a second instead of only when a peer arrives.
        // The result is reduced to a plain enum BEFORE `listener` is touched again: holding the
        // `&TcpListener` across the rebind would not compile, and hiding that behind a clone would
        // hide a real lifetime question.
        enum Acc {
            Served(TcpStream, std::net::SocketAddr),
            Idle,
            Rebind(String),
        }
        let acc = {
            let l = listener.as_ref().expect("bound just above");
            if let Err(e) = set_timeouts(l.as_raw_fd(), 1) {
                Acc::Rebind(format!("could not bound accept: {e}"))
            } else {
                match l.accept() {
                    Ok((s, peer)) => Acc::Served(s, peer),
                    Err(ref e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock
                                | std::io::ErrorKind::TimedOut
                                | std::io::ErrorKind::Interrupted
                        ) =>
                    {
                        Acc::Idle
                    }
                    Err(e) => Acc::Rebind(format!("accept failed: {e}")),
                }
            }
        };
        match acc {
            Acc::Served(s, peer) => {
                log(&format!("mic seam peer connected from {peer}"));
                serve_mic_peer(s, shutdown, state);
                log("mic seam peer disconnected");
            }
            Acc::Idle => {}
            Acc::Rebind(why) => {
                log(&format!("mic seam {why} — rebinding"));
                listener = None;
            }
        }
    }
    if listener.is_some() {
        notify(state, "uplink off");
        close_mic_peer(state);
    }
}

/// Push a back-channel line to the connected peer. Best-effort; a failure drops the peer, which
/// ocbmd handles by reconnecting.
fn notify(state: &State, line: &str) {
    let Ok(mut g) = state.mic_tx.lock() else { return };
    if let Some(s) = g.as_mut() {
        if s.write_all(format!("{line}\n").as_bytes()).is_err() {
            *g = None;
        }
    }
}

fn close_mic_peer(state: &State) {
    if let Ok(mut g) = state.mic_tx.lock() {
        if let Some(s) = g.take() {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
}

/// One mic-seam connection: announce the format, then read `mic <len>\n<pcm>` until it closes.
fn serve_mic_peer(stream: TcpStream, shutdown: &AtomicBool, state: &State) {
    let _ = stream.set_nodelay(true);
    // 250 ms, NOT the 1 s the other sockets use. This loop's tick is what bounds how long we can
    // still be holding `:9112` after this box stops owning wireless Android Auto — and the thing
    // waiting for the port is airplayd, which binds ONCE at startup and gives up if it fails, so a
    // lost race silently kills the microphone for a whole CarPlay session. See the owner re-check
    // below; a shorter tick is the cheap half of narrowing that window.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    if let Ok(tx) = stream.try_clone() {
        if let Ok(mut g) = state.mic_tx.lock() {
            *g = Some(tx);
        }
    }
    // The gate the app waits on. 8 kHz mono for CVSD, because that is what CVSD is and resampling on
    // the phone side of an 8 kHz link buys nothing; `16000 1 msbc` once wideband is negotiated.
    let mut announced = state.codec();
    notify(state, &uplink_on_line(announced));
    log(&format!(
        "mic seam: sent '{}' — the app should start capturing",
        uplink_on_line(announced)
    ));

    let mut r = std::io::BufReader::new(stream);
    let mut line = String::new();
    let mut chunks = 0u64;
    loop {
        if shutdown.load(Ordering::Relaxed) || !state.wants_audio() {
            break;
        }
        // Re-checked HERE and not only in the accept loop above. `serve_mic_peer` blocks for as
        // long as the peer stays connected, and ocbmd's mic seam stays connected for the whole time
        // a host is subscribed — so without this, an INBOUND headset link (which deliberately
        // outlives the projection session) would keep `:9112` after CarPlay took the box.
        let o = flags::owner();
        if o != ProjectionOwner::WirelessAa && o != ProjectionOwner::WiredAa {
            log("mic seam: the box no longer owns an Android Auto session — releasing 127.0.0.1:9112 to airplayd");
            break;
        }
        // The mic seam is deliberately brought up on `arm()`, which is BEFORE the AG has negotiated
        // a codec — so the first `uplink on` is almost always the CVSD one and the `+BCS` lands
        // after it. Re-announcing is therefore the normal path, not an edge case: without it the app
        // would keep encoding 8 kHz PCM into a wideband call.
        let now = state.codec();
        if now != announced {
            announced = now;
            notify(state, &uplink_on_line(now));
            log(&format!(
                "mic seam: codec changed — re-sent '{}'",
                uplink_on_line(now)
            ));
        }
        match read_line_tolerant(&mut r, &mut line) {
            LineResult::Line => {}
            // Deliberately NO `line.clear()` here: `read_line` appends what it managed to read
            // before the timeout, so clearing on an idle tick would silently eat half a header and
            // desync this seam permanently. The clear happens once the line has been dealt with.
            LineResult::Idle => continue,
            LineResult::Closed => break,
        }
        let Some(len) = parse_mic_header(&line) else {
            line.clear();
            continue; // `touch`/`cmd`/anything else on this seam is not ours
        };
        line.clear();
        let mut pcm = vec![0u8; len];
        if read_exact_tolerant(&mut r, &mut pcm, shutdown).is_err() {
            break;
        }
        chunks += 1;
        if chunks == 1 {
            log(&format!("mic seam: first uplink PCM chunk ({len} B) — the app is capturing"));
        }
        if let Ok(mut u) = state.uplink.lock() {
            u.push(&pcm, announced.uplink_max(), announced.uplink_unit());
        }
    }
    notify(state, "uplink off");
    close_mic_peer(state);
}

enum LineResult {
    Line,
    Idle,
    Closed,
}

/// `read_line` that treats the socket's 1 s timeout as "idle", not as failure.
///
/// A plain `BufRead::read_line` returns `Err(WouldBlock)` on a timeout AFTER having consumed
/// whatever it already read, so retrying naively can lose a partial line. It cannot here: the peer
/// writes `mic <len>\n` in a single `write_all` together with the PCM, so a timeout means nothing
/// was in flight at all. Documented because the assumption is what makes this safe.
fn read_line_tolerant<R: std::io::BufRead>(r: &mut R, out: &mut String) -> LineResult {
    match r.read_line(out) {
        Ok(0) => LineResult::Closed,
        Ok(_) => LineResult::Line,
        Err(ref e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::Interrupted
            ) =>
        {
            LineResult::Idle
        }
        Err(_) => LineResult::Closed,
    }
}

/// `read_exact` across the 1 s socket timeout, bounded by an overall deadline so a peer that
/// announces 320 bytes and sends 4 cannot park this thread.
fn read_exact_tolerant<R: Read>(
    r: &mut R,
    buf: &mut [u8],
    shutdown: &AtomicBool,
) -> std::io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got = 0usize;
    while got < buf.len() {
        if shutdown.load(Ordering::Relaxed) || Instant::now() >= deadline {
            return Err(std::io::Error::other("mic chunk timed out"));
        }
        match r.read(&mut buf[got..]) {
            Ok(0) => return Err(std::io::Error::other("mic seam closed mid-chunk")),
            Ok(n) => got += n,
            Err(ref e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// The controller's own BD address, in the little-endian byte order the Bluetooth sockaddrs use.
///
/// sysfs prints it big-endian text (`00:11:22:…`), the wire wants it reversed. Getting this
/// backwards would bind the listener to an address no controller has, and the bind would succeed —
/// then no SCO connection would ever arrive, with nothing anywhere saying why. `None` (BDADDR_ANY)
/// is the correct fallback and works on a single-adapter box.
pub fn local_bdaddr(hci_dev: &str) -> Option<[u8; 6]> {
    let text = std::fs::read_to_string(format!("/sys/class/bluetooth/{hci_dev}/address")).ok()?;
    parse_bdaddr(text.trim())
}

/// `"00:11:22:33:44:55"` → little-endian `[0x55,0x44,0x33,0x22,0x11,0x00]`.
pub fn parse_bdaddr(s: &str) -> Option<[u8; 6]> {
    let mut out = [0u8; 6];
    let mut n = 0;
    for (i, part) in s.split(':').enumerate() {
        if i >= 6 {
            return None;
        }
        out[5 - i] = u8::from_str_radix(part, 16).ok()?;
        n += 1;
    }
    (n == 6).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_aggregate_to_twenty_milliseconds() {
        let mut a = FrameAggregator::new(FRAME_BYTES);
        // Six 48 B CVSD packets are 288 B — not yet a frame.
        for _ in 0..6 {
            assert!(a.push(&[0xAAu8; 48]).is_empty());
        }
        assert_eq!(a.pending(), 288);
        // The seventh completes one 320 B frame and leaves 16 B over.
        let out = a.push(&[0xAAu8; 48]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), FRAME_BYTES);
        assert_eq!(a.pending(), 16);
        assert!(!a.odd_seen);
    }

    #[test]
    fn a_large_packet_can_complete_several_frames_at_once() {
        let mut a = FrameAggregator::new(FRAME_BYTES);
        let out = a.push(&vec![0u8; FRAME_BYTES * 3 + 4]);
        assert_eq!(out.len(), 3);
        assert_eq!(a.pending(), 4);
    }

    /// The alignment repair: one odd packet must cost one byte, not every sample after it.
    #[test]
    fn an_odd_packet_drops_its_stray_byte_and_latches() {
        let mut a = FrameAggregator::new(FRAME_BYTES);
        assert!(a.push(&[1u8; 49]).is_empty());
        assert_eq!(a.pending(), 48, "the 49th byte must be dropped, not carried");
        assert!(a.odd_seen);
    }

    #[test]
    fn seam_format_is_twenty_one_bytes_of_telephony_pcm() {
        let m = sco_format_msg(SCO_SCID, ScoCodec::Cvsd);
        assert_eq!(u32::from_be_bytes(m[0..4].try_into().unwrap()), 21, "len counts magic+marker+body");
        assert_eq!(&m[4..8], b"SEAV");
        assert_eq!(m[8], 0x02, "SEAM_FORMAT");
        assert_eq!(u64::from_le_bytes(m[9..17].try_into().unwrap()), SCO_SCID);
        assert_eq!(m[17], 0, "codec 0 = PCM");
        assert_eq!(u32::from_le_bytes(m[18..22].try_into().unwrap()), 8000);
        assert_eq!(m[22], 1, "mono");
        assert_eq!(m[23], 16, "S16");
        assert_eq!(m[24], 1, "audio_type 1 = telephony");
        assert_eq!(m.len(), 4 + 21);
    }

    #[test]
    fn a_plain_frame_is_marker_three_and_thirteen_bytes_of_overhead() {
        let pcm = vec![0x5Au8; FRAME_BYTES];
        let m = sco_frame_msg(SCO_SCID, &pcm);
        assert_eq!(
            u32::from_be_bytes(m[0..4].try_into().unwrap()) as usize,
            13 + FRAME_BYTES
        );
        assert_eq!(&m[4..8], b"SEAV");
        assert_eq!(m[8], ocbm_proto::SEAM_PKT_PLAIN);
        assert_eq!(m[8], 0x03, "the wire value the host parses");
        assert_eq!(u64::from_le_bytes(m[9..17].try_into().unwrap()), SCO_SCID);
        assert_eq!(&m[17..], &pcm[..]);
    }

    /// A host that lost sync must be able to re-align on the magic — the property the whole v2
    /// framing exists for, asserted here for the new marker too.
    #[test]
    fn every_message_carries_the_resync_magic_at_the_same_offset() {
        for m in [sco_format_msg(1, ScoCodec::Cvsd), sco_frame_msg(1, &[0u8; 4])] {
            assert_eq!(&m[4..8], &SEAM_MAGIC);
        }
    }

    #[test]
    fn mic_headers_parse_and_bogus_ones_do_not() {
        assert_eq!(parse_mic_header("mic 320\n"), Some(320));
        assert_eq!(parse_mic_header("mic 320\r\n"), Some(320));
        assert_eq!(parse_mic_header("mic 1048576\n"), Some(1 << 20));
        assert_eq!(parse_mic_header("mic 1048577\n"), None, "over ocbmd's own 1 MiB cap");
        assert_eq!(parse_mic_header("mic 0\n"), None);
        assert_eq!(parse_mic_header("mic -8\n"), None);
        assert_eq!(parse_mic_header("touch 1 2 3\n"), None);
        assert_eq!(parse_mic_header("cmd whatever\n"), None);
        assert_eq!(parse_mic_header("micro 320\n"), None);
    }

    #[test]
    fn the_uplink_zero_pads_a_short_buffer_and_counts_the_underrun() {
        let mut u = Uplink::default();
        u.push(&[1, 2, 3, 4], UPLINK_MAX_BYTES, 0);
        let got = u.take(8);
        assert_eq!(got, vec![1, 2, 3, 4, 0, 0, 0, 0]);
        assert_eq!(u.underruns, 1);
        // A satisfied take must not count as an underrun.
        u.push(&[9; 8], UPLINK_MAX_BYTES, 0);
        assert_eq!(u.take(8), vec![9u8; 8]);
        assert_eq!(u.underruns, 1);
    }

    /// Latency, not memory, is what an over-producing app costs — but only if the buffer is capped.
    #[test]
    fn the_uplink_drops_the_oldest_audio_past_three_hundred_milliseconds() {
        let mut u = Uplink::default();
        u.push(&vec![7u8; UPLINK_MAX_BYTES], UPLINK_MAX_BYTES, 0);
        u.push(&[1, 2, 3, 4], UPLINK_MAX_BYTES, 0);
        assert_eq!(u.buf.len(), UPLINK_MAX_BYTES);
        let tail: Vec<u8> = u.buf.iter().rev().take(4).rev().copied().collect();
        assert_eq!(tail, vec![1, 2, 3, 4], "the newest audio must survive");
    }

    // ---- wideband (mSBC) -----------------------------------------------------------------

    /// The wideband `SEAM_FORMAT`: codec 4, 16 kHz. Same 21-byte shape as the CVSD one, because the
    /// host parses one layout and reads the codec byte to decide what the payload is.
    #[test]
    fn the_msbc_format_declares_codec_four_at_sixteen_kilohertz() {
        let m = sco_format_msg(SCO_SCID, ScoCodec::Msbc);
        assert_eq!(u32::from_be_bytes(m[0..4].try_into().unwrap()), 21);
        assert_eq!(&m[4..8], b"SEAV");
        assert_eq!(m[8], 0x02, "SEAM_FORMAT");
        assert_eq!(m[17], 4, "codec 4 = mSBC");
        assert_eq!(m[17], ocbm_proto::SEAM_CODEC_MSBC);
        assert_eq!(u32::from_le_bytes(m[18..22].try_into().unwrap()), 16000);
        assert_eq!(m[22], 1, "mono");
        assert_eq!(m[23], 16, "the DECODED audio is S16 — this field is not about the payload");
        assert_eq!(m[24], 1, "audio_type 1 = telephony");
        // The CVSD form must not have moved.
        assert_eq!(sco_format_msg(SCO_SCID, ScoCodec::Cvsd)[17], 0);
    }

    /// The whole point of the wideband downlink: one SCO read goes out as one message, byte for
    /// byte, H2 header included. An aggregator or a "repair" here would be the bug.
    #[test]
    fn an_msbc_packet_rides_the_seam_verbatim() {
        let mut pkt = vec![0u8; MSBC_PKT_BYTES];
        pkt[0] = 0x01; // H2 sync
        pkt[1] = 0xC8; // H2 sequence
        pkt[2] = 0xAD; // mSBC frame sync
        let m = sco_frame_msg(SCO_SCID, &pkt);
        assert_eq!(u32::from_be_bytes(m[0..4].try_into().unwrap()) as usize, 13 + MSBC_PKT_BYTES);
        assert_eq!(m[8], ocbm_proto::SEAM_PKT_PLAIN);
        assert_eq!(&m[17..], &pkt[..], "the payload must be untouched");
        assert_eq!(m[17], 0x01);
        assert_eq!(m[18], 0xC8);
    }

    /// The air-mode settings are the two magic numbers this whole feature turns on. Transcribed from
    /// the kernel's `SCO_AIRMODE_TRANSP` / `SCO_AIRMODE_CVSD`, and wrong by one bit is a call of
    /// noise.
    #[test]
    fn the_voice_settings_are_the_kernels_own_values() {
        assert_eq!(ScoCodec::Msbc.voice_setting(), 0x0003);
        assert_eq!(ScoCodec::Cvsd.voice_setting(), 0x0060);
        assert_eq!(std::mem::size_of::<BtVoice>(), 2, "struct bt_voice is one u16");
        assert_eq!(ScoCodec::from_u8(ScoCodec::Msbc.as_num()), ScoCodec::Msbc);
        assert_eq!(ScoCodec::from_u8(ScoCodec::Cvsd.as_num()), ScoCodec::Cvsd);
        assert_eq!(ScoCodec::from_u8(200), ScoCodec::Cvsd, "an unknown value must degrade to CVSD");
        assert_eq!(ScoCodec::Msbc.as_str(), "mSBC");
        assert_eq!(ScoCodec::Cvsd.as_str(), "CVSD");
    }

    /// The mic seam's fourth token. `uplink on 8000 1` must stay EXACTLY what shipped — ocbmd and
    /// airplayd have both parsed that line for a year.
    #[test]
    fn the_mic_seam_line_gains_a_fourth_token_only_for_wideband() {
        assert_eq!(uplink_on_line(ScoCodec::Cvsd), "uplink on 8000 1");
        assert_eq!(uplink_on_line(ScoCodec::Msbc), "uplink on 16000 1 msbc");
        assert_eq!(uplink_on_line(ScoCodec::Cvsd).split_whitespace().count(), 4);
        assert_eq!(uplink_on_line(ScoCodec::Msbc).split_whitespace().count(), 5);
    }

    /// A short mSBC buffer must yield NOTHING. Padding it would hand the AG a frame with no H2 sync
    /// where one is due — audible as a squelch, and unrecoverable until the decoder re-syncs — and
    /// there is no silent mSBC packet to substitute without an encoder.
    #[test]
    fn an_msbc_underrun_skips_the_write_instead_of_padding() {
        let mut u = Uplink::default();
        u.push(&[0xAA; 59], 2400, MSBC_PKT_BYTES);
        assert_eq!(u.take_packet(MSBC_PKT_BYTES), None, "59 B is not a packet");
        assert_eq!(u.underruns, 1);
        u.push(&[0xBB; 1], 2400, MSBC_PKT_BYTES);
        let got = u.take_packet(MSBC_PKT_BYTES).expect("60 B is");
        assert_eq!(got.len(), MSBC_PKT_BYTES);
        assert_eq!(got[58], 0xAA);
        assert_eq!(got[59], 0xBB);
        assert_eq!(u.underruns, 1, "a satisfied take must not count as an underrun");
        // Two packets in one `mic` chunk is the normal case: the app batches.
        u.push(&[7u8; MSBC_PKT_BYTES * 2], 2400, MSBC_PKT_BYTES);
        assert!(u.take_packet(MSBC_PKT_BYTES).is_some());
        assert!(u.take_packet(MSBC_PKT_BYTES).is_some());
        assert_eq!(u.take_packet(MSBC_PKT_BYTES), None);
    }

    /// The overflow trim must drop WHOLE packets. Trimming a partial one would leave every later
    /// `take_packet` straddling two air frames — the permanent version of the transient glitch the
    /// trim exists to fix.
    #[test]
    fn the_msbc_trim_keeps_packet_alignment() {
        let max = MSBC_PKT_BYTES * 4;
        let mut u = Uplink::default();
        for i in 0..6u8 {
            let mut pkt = vec![i; MSBC_PKT_BYTES];
            pkt[0] = 0x01;
            u.push(&pkt, max, MSBC_PKT_BYTES);
        }
        assert!(u.buf.len() <= max);
        assert_eq!(u.buf.len() % MSBC_PKT_BYTES, 0, "the queue must stay packet-aligned");
        let head = u.take_packet(MSBC_PKT_BYTES).expect("a whole packet");
        assert_eq!(head[0], 0x01, "the H2 header must still be at offset 0");
        assert_eq!(head[1], 2, "the two oldest packets are the ones dropped");
    }

    /// A connected socketpair with one packet queued on it, standing in for an accepted SCO child.
    fn deferred_accept_fixture() -> ([libc::c_int; 2], [u8; 4]) {
        let mut sv = [0 as libc::c_int; 2];
        // SAFETY: a 2-element array of c_int, as socketpair(2) requires.
        let r = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) };
        assert_eq!(r, 0, "socketpair: {}", std::io::Error::last_os_error());
        let pkt = [0x01u8, 0x08, 0xAD, 0x00];
        // SAFETY: writing 4 bytes from a local buffer to an owned descriptor.
        let n = unsafe { libc::write(sv[1], pkt.as_ptr() as *const libc::c_void, pkt.len()) };
        assert_eq!(n, pkt.len() as isize);
        (sv, pkt)
    }

    fn drain_and_close(sv: [libc::c_int; 2]) -> Vec<u8> {
        let mut got = [0u8; 4];
        // SAFETY: reading into a local buffer from an owned descriptor.
        let n = unsafe { libc::read(sv[0], got.as_mut_ptr() as *mut libc::c_void, got.len()) };
        for fd in sv {
            // SAFETY: descriptors this test owns and does not use again.
            unsafe { libc::close(fd) };
        }
        got[..n.max(0) as usize].to_vec()
    }

    /// The guard that protects the CVSD path from the deferred-accept trigger: on a socket that is
    /// ALREADY connected there is nothing to complete, and the one-byte read must not happen — on a
    /// live SEQPACKET SCO socket it would truncate a real audio packet down to one byte. A
    /// socketpair is connected and writable from birth, exactly like an accepted socket on a kernel
    /// that ignored `BT_DEFER_SETUP`.
    #[test]
    fn a_connected_socket_completes_without_consuming_a_packet() {
        let (sv, pkt) = deferred_accept_fixture();
        let never = AtomicBool::new(false);
        let mut applied = false;
        let r = complete_deferred_accept(sv[0], &never, || {
            applied = true;
            Ok(())
        });
        assert!(r.is_ok());
        assert!(applied, "the per-connection air mode must be applied on every accepted child");
        assert_eq!(drain_and_close(sv), pkt, "the trigger read must not have consumed anything");
    }

    /// ORDER, which is the whole reason the setter is a closure: `BT_VOICE` is only settable while
    /// the child is in `BT_CONNECT2`, and the trigger read moves it to `BT_CONFIG`. A failure to set
    /// it must therefore abort BEFORE the read — the packet still being there is the proof, and the
    /// caller closes the child rather than letting the AG open a channel in the wrong air mode.
    #[test]
    fn a_failed_child_voice_setting_aborts_before_the_trigger_read() {
        let (sv, pkt) = deferred_accept_fixture();
        let never = AtomicBool::new(false);
        let r = complete_deferred_accept(sv[0], &never, || {
            Err(std::io::Error::from_raw_os_error(libc::EINVAL))
        });
        let e = r.expect_err("a failed air mode must fail the accept");
        assert_eq!(e.raw_os_error(), Some(libc::EINVAL), "the errno must survive for the log");
        assert_eq!(drain_and_close(sv), pkt, "nothing may have been read from the child");
    }

    /// The mapping the device log turned on: 3.14 accepts `BT_VOICE` only in `BT_OPEN`/`BT_BOUND`/
    /// `BT_CONNECT2`, so these two values are applied at exactly two moments — bound listener, and
    /// deferred child — and never to a `BT_LISTEN` socket, which answers `EINVAL` even for 0x0060.
    #[test]
    fn the_child_air_mode_follows_the_negotiated_codec() {
        assert_eq!(ScoCodec::Msbc.voice_setting(), BT_VOICE_TRANSPARENT);
        assert_eq!(ScoCodec::Cvsd.voice_setting(), BT_VOICE_CVSD_16BIT);
        assert_eq!(BT_DEFER_SETUP, 7);
        assert_eq!(SOL_BLUETOOTH, 274);
        assert_eq!(BT_VOICE, 11);
    }

    #[test]
    fn bdaddr_text_is_reversed_for_the_wire() {
        assert_eq!(
            parse_bdaddr("00:11:22:33:44:55"),
            Some([0x55, 0x44, 0x33, 0x22, 0x11, 0x00])
        );
        assert_eq!(parse_bdaddr("00:11:22:33:44:55").unwrap()[5], 0x00);
        assert_eq!(parse_bdaddr("00:11:22:33:44"), None);
        assert_eq!(parse_bdaddr("00:11:22:33:44:55:78"), None);
        assert_eq!(parse_bdaddr("not an address"), None);
    }
}
