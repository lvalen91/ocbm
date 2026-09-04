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
//!     available on the box, so airplayd never enables it).
//!
//! Only ONE uplink is active at a time: the iPhone re-SETUPs MainAudio each Siri turn, and the newest
//! instance's key/port/codec supersede the prior (same philosophy as the downlink sink).

use std::io::{BufRead, BufReader, Read};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::Mutex;
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

/// Start the control-in listener once (process lifetime) on the caller-supplied `addr` — airplayd
/// passes `127.0.0.1:9112` (`MIC_INGEST_ADDR`); `:9110` is the SEPARATE HID input seam. Accepts carlink's connection and
/// routes `mic` PCM to the active uplink; `touch`/`cmd` lines are consumed and ignored (HID is a
/// separate subsystem). Safe to call once at startup.
pub fn start_control_listener(addr: String) {
    std::thread::spawn(move || {
        let listener = match TcpListener::bind(&addr) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[uplink] control-in bind {addr} failed: {e}");
                return;
            }
        };
        eprintln!("[uplink] control-in listening {addr} (mic PCM in / uplink back-channel out)");
        for conn in listener.incoming() {
            match conn {
                Ok(s) => {
                    std::thread::spawn(move || read_control(s));
                }
                Err(_) => continue,
            }
        }
    });
}

/// Read carlink's control-in stream: line-framed `mic <len>\n<len bytes PCM>`, plus `touch`/`cmd`
/// text lines we ignore. The `mic` PCM is 16 kHz mono S16LE.
fn read_control(stream: TcpStream) {
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    eprintln!("[uplink] control-in connected from {peer}");
    // Register a write-half for the uplink back-channel; if an uplink is already armed, tell this new
    // peer immediately so a mid-session (re)connect starts the mic.
    if let Ok(tx) = stream.try_clone() {
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
    let mut r = BufReader::new(stream);
    let mut line = String::new();
    let mut mic_chunks: u64 = 0;
    loop {
        line.clear();
        match r.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
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
                pcm.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
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
    // No explicit prune of this peer's write-half: every accepted socket shares the listener's
    // local address, so the comparison keyed on the FORMATTED peer alone -- and `peer_addr()`
    // failing at connect made that an empty string, which matches any other entry that also failed.
    // `notify_control`'s `retain_mut` already drops a write-half whose write fails, and a closed
    // peer fails immediately with EPIPE rather than stalling.
    eprintln!("[uplink] control-in {peer} closed");
}
