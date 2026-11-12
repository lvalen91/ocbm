//! `ocbm-host airplay`: the host side of AirPlay-only mode (docs/ocbm/02_AIRPLAY_ONLY_MODE.md,
//! docs/ocbm/00_PROTOCOL.md "AirPlay-only mode"). Selects the mode with the AP credentials and the
//! receiver name, keeps the box alive with heartbeats, takes the tunnelled streams off CH_VIDEO /
//! CH_MEDIA_AUDIO / CH_METADATA, DECRYPTS them here with the keys the box hands over, and writes
//!
//!   DIR/video.h264   Annex-B H.264 (parameter sets from the in-stream avcC, then the frames), or
//!   DIR/video.h265   Annex-B HEVC (VPS/SPS/PPS from the hvcC inside the in-stream `hvc1` sample entry,
//!                    then the frames): which one is decided by the stream's own configuration frame
//!   DIR/audio.aac    ADTS, every audio stream's access units in arrival order
//!   DIR/events.log   mode / AP / session state, stream start/stop, metadata, a closing summary
//!   DIR/box.log      the box's own log, streamed over CH_LOG
//!   DIR/artwork-N.jpg
//!
//! The decrypt rules are the verified ones (`box/airplay-probe/src/bin/capcheck.rs`):
//!   video frame   ChaCha20-Poly1305, nonce = 4 zero bytes | seq u64 LE (from the wire), AAD = the 128-byte
//!                 header, body = ciphertext | tag. Opcode 0 only; opcode 1 is the plaintext codec
//!                 configuration (`ocbm_proto::seam::video_config`: a bare avcC, or an hvc1-wrapped hvcC).
//!   audio packet  `[RTP 12][ciphertext][tag 16][nonce 8]`, nonce = 4 zero bytes | the trailing 8,
//!                 AAD = RTP bytes 4..12. Payload = one raw AAC access unit.
//! The keys arrive on the lanes (`SEAM_KEY`); the pairing secret never leaves the box.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use ocbm_proto as p;
use ocbm_proto::seam::{video_config, MetaReader, SeamReader, SCREEN_HDR_LEN};
pub use ocbm_proto::seam::VideoCodec;
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn open_aead(key: &[u8; 32], nonce8: &[u8], aad: &[u8], msg: &[u8]) -> Option<Vec<u8>> {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(nonce8);
    ChaCha20Poly1305::new(Key::from_slice(key)).decrypt(Nonce::from_slice(&n), Payload { msg, aad }).ok()
}

/// Key material in a log: its length and first 4 bytes, never more.
fn redact(k: &[u8]) -> String {
    format!("{} B {}..", k.len(), k.iter().take(4).map(|b| format!("{b:02x}")).collect::<String>())
}

/// Length-prefixed NAL units (H.264 and HEVC alike) -> Annex-B. `None` when a length runs past the frame.
pub fn avc_frame_to_annexb(pt: &[u8], len_size: usize) -> Option<Vec<u8>> {
    let (mut at, mut out) = (0usize, Vec::with_capacity(pt.len() + 16));
    while at + len_size <= pt.len() {
        let l = pt[at..at + len_size].iter().fold(0usize, |a, x| a << 8 | usize::from(*x));
        at += len_size;
        if l == 0 || at + l > pt.len() {
            return None;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&pt[at..at + l]);
        at += l;
    }
    Some(out)
}

/// The 7-byte ADTS header of one AAC-LC access unit (no CRC).
pub fn adts_header(au_len: usize, rate: u32, ch: u8) -> Option<[u8; 7]> {
    const RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];
    let sr = RATES.iter().position(|r| *r == rate)? as u8;
    let len = au_len + 7;
    if len > 0x1FFF || ch == 0 || ch > 7 {
        return None;
    }
    Some([
        0xFF,
        0xF1,
        (1 << 6) | (sr << 2) | (ch >> 2),
        ((ch & 3) << 6) | (len >> 11) as u8,
        (len >> 3) as u8,
        ((len & 7) << 5) as u8 | 0x1F,
        0xFC,
    ])
}

#[derive(Default, Clone)]
pub struct AudioStat {
    pub ok: u64,
    pub bad: u64,
    pub empty: u64,
    pub no_key: u64,
    pub bytes: u64,
}

/// Where the decrypted video goes. `codec` is called each time a configuration frame names the codec,
/// BEFORE its parameter sets are written, so a file sink can pick `video.h264` or `video.h265`.
pub trait VideoSink: Write {
    fn codec(&mut self, _c: VideoCodec) {}
}

impl VideoSink for Vec<u8> {}

/// The command's video sink: `DIR/video.<ext>` per codec, created (truncated) the first time a
/// configuration frame names that codec and appended to after that. Bytes that arrive before any
/// configuration frame cannot be decoded and are counted, not written.
pub struct VideoFiles {
    dir: std::path::PathBuf,
    cur: Option<(VideoCodec, std::io::BufWriter<std::fs::File>)>,
    pub written: Vec<VideoCodec>,
    pub dropped: u64,
}

impl VideoFiles {
    pub fn new(dir: &std::path::Path) -> Self {
        VideoFiles { dir: dir.to_path_buf(), cur: None, written: Vec::new(), dropped: 0 }
    }
}

impl Write for VideoFiles {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        match self.cur.as_mut() {
            Some((_, f)) => f.write_all(b).map(|_| b.len()),
            None => {
                self.dropped += b.len() as u64;
                Ok(b.len())
            }
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.cur.as_mut().map_or(Ok(()), |(_, f)| f.flush())
    }
}

impl VideoSink for VideoFiles {
    fn codec(&mut self, c: VideoCodec) {
        if self.cur.as_ref().is_some_and(|(k, _)| *k == c) {
            return;
        }
        let _ = self.flush();
        let first = !self.written.contains(&c);
        let path = self.dir.join(format!("video.{}", c.ext()));
        let f = std::fs::OpenOptions::new().create(true).write(true).append(!first).truncate(first).open(&path);
        match f {
            Ok(f) => {
                self.cur = Some((c, std::io::BufWriter::new(f)));
                if first {
                    self.written.push(c);
                }
            }
            Err(e) => {
                eprintln!("airplay: cannot open {}: {e}", path.display());
                self.cur = None;
            }
        }
    }
}

#[derive(Default)]
pub struct Stats {
    pub video_keys: u64,
    pub video_ok: u64,
    pub video_bad: u64,
    pub video_idr: u64,
    pub video_cfg: u64,
    pub video_other: u64,
    pub video_gaps: u64,
    pub video_bytes: u64,
    /// The codec the most recent configuration frame named (None = no configuration frame yet).
    pub video_codec: Option<VideoCodec>,
    pub audio: Vec<(u64, AudioStat)>,
    pub events: u64,
    pub artwork: u64,
}

/// Turns lane messages into the three outputs. Generic over the sinks so the tests run in memory.
pub struct Capture<V: VideoSink, A: Write, E: Write> {
    pub video: V,
    pub audio: A,
    pub events: E,
    vkey: Option<([u8; 32], u64)>,
    len_size: usize,
    next_seq: Option<u64>,
    akeys: HashMap<u64, [u8; 32]>,
    afmt: HashMap<u64, (u8, u32, u8)>,
    pub stats: Stats,
    /// Where artwork goes; None = not saved (tests).
    pub art_dir: Option<std::path::PathBuf>,
}

impl<V: VideoSink, A: Write, E: Write> Capture<V, A, E> {
    pub fn new(video: V, audio: A, events: E) -> Self {
        Capture {
            video, audio, events,
            vkey: None, len_size: 4, next_seq: None,
            akeys: HashMap::new(), afmt: HashMap::new(),
            stats: Stats::default(), art_dir: None,
        }
    }

    pub fn note(&mut self, src: &str, text: &str) {
        let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let _ = writeln!(self.events, "{ms} {src} {text}");
        let _ = self.events.flush();
        self.stats.events += 1;
    }

    fn astat(&mut self, scid: u64) -> &mut AudioStat {
        if let Some(i) = self.stats.audio.iter().position(|(s, _)| *s == scid) {
            return &mut self.stats.audio[i].1;
        }
        self.stats.audio.push((scid, AudioStat::default()));
        &mut self.stats.audio.last_mut().unwrap().1
    }

    /// One CH_VIDEO message: `0x00` key `[key 32][scid 8]`, `0x01` frame `[seq 8][hdr 128][body]`.
    pub fn video_msg(&mut self, marker: u8, body: &[u8]) {
        match marker {
            0x00 if body.len() == 40 => {
                let key: [u8; 32] = body[..32].try_into().unwrap();
                let scid = u64::from_le_bytes(body[32..].try_into().unwrap());
                if self.vkey != Some((key, scid)) {
                    self.note("host", &format!("video key for scid {scid}: {}", redact(&key)));
                }
                // A key message is also the start of a data connection: the counter starts over.
                self.vkey = Some((key, scid));
                self.next_seq = None;
                self.stats.video_keys += 1;
            }
            0x01 if body.len() >= 8 + SCREEN_HDR_LEN => {
                let seq = u64::from_le_bytes(body[..8].try_into().unwrap());
                let (hdr, payload) = (&body[8..8 + SCREEN_HDR_LEN], &body[8 + SCREEN_HDR_LEN..]);
                match hdr[4] {
                    0 => {
                        if self.next_seq.is_some_and(|n| n != seq) {
                            self.stats.video_gaps += 1;
                            self.note("host", &format!("video seq jump: expected {} got {seq} (frames lost: a decoder needs a keyframe)", self.next_seq.unwrap()));
                        }
                        self.next_seq = Some(seq + 1);
                        // The nonce is the WIRE seq, never a local count: a host that counts desyncs at the first gap.
                        let pt = self.vkey.and_then(|(k, _)| open_aead(&k, &seq.to_le_bytes(), hdr, payload));
                        match pt.as_deref().and_then(|pt| avc_frame_to_annexb(pt, self.len_size)) {
                            Some(es) => {
                                let _ = self.video.write_all(&es);
                                self.stats.video_ok += 1;
                                self.stats.video_bytes += es.len() as u64;
                                self.stats.video_idr += u64::from(hdr[5] & 0x10 != 0);
                            }
                            None => self.stats.video_bad += 1,
                        }
                    }
                    1 => match video_config(payload) {
                        Some(c) => {
                            if self.stats.video_codec.is_some_and(|k| k != c.codec) {
                                println!("airplay: the stream switched codec to {}", c.codec.name());
                            }
                            self.stats.video_codec = Some(c.codec);
                            self.len_size = c.len_size;
                            self.video.codec(c.codec);
                            let _ = self.video.write_all(&c.annexb());
                            self.stats.video_cfg += 1;
                            let f = |o: usize| f32::from_le_bytes([hdr[o], hdr[o + 1], hdr[o + 2], hdr[o + 3]]);
                            let line = format!(
                                "video config ({} B): {}; source {}x{} output {}x{} offset x {} y {}",
                                payload.len(), c.describe(), f(16), f(20), f(56), f(60), f(48), f(52)
                            );
                            if self.stats.video_cfg == 1 {
                                println!("airplay: {line}");
                            }
                            self.note("host", &line);
                        }
                        None => {
                            let head: String = payload.iter().take(8).map(|b| format!("{b:02x}")).collect();
                            self.note("host", &format!("video config frame is neither avcC nor hvcC ({} B, first bytes {head}): not written", payload.len()))
                        }
                    },
                    _ => self.stats.video_other += 1, // opcode 5: the phone's plaintext statistics
                }
            }
            _ => {}
        }
    }

    /// One CH_MEDIA_AUDIO message (all scid-tagged): SEAM_KEY, SEAM_FORMAT, SEAM_PKT.
    pub fn audio_msg(&mut self, marker: u8, body: &[u8]) {
        match marker {
            p::SEAM_KEY if body.len() == 40 => {
                let key: [u8; 32] = body[..32].try_into().unwrap();
                let scid = u64::from_le_bytes(body[32..].try_into().unwrap());
                if self.akeys.insert(scid, key) != Some(key) {
                    self.note("host", &format!("audio key for scid {scid}: {}", redact(&key)));
                }
            }
            p::SEAM_FORMAT if body.len() == 16 => {
                let scid = u64::from_le_bytes(body[..8].try_into().unwrap());
                let f = (body[8], u32::from_le_bytes(body[9..13].try_into().unwrap()), body[13]);
                if self.afmt.insert(scid, f) != Some(f) {
                    self.note("host", &format!("audio format for scid {scid}: codec {} rate {} channels {}", f.0, f.1, f.2));
                }
            }
            p::SEAM_PKT if body.len() >= 8 + 12 + 16 + 8 => {
                let scid = u64::from_le_bytes(body[..8].try_into().unwrap());
                let pkt = &body[8..];
                let Some(key) = self.akeys.get(&scid).copied() else {
                    self.astat(scid).no_key += 1;
                    return;
                };
                let n = pkt.len();
                let Some(au) = open_aead(&key, &pkt[n - 8..], &pkt[4..12], &pkt[12..n - 8]) else {
                    self.astat(scid).bad += 1;
                    return;
                };
                if au.is_empty() {
                    self.astat(scid).empty += 1; // seen once per session in the measurements: nothing to decode
                    return;
                }
                let fmt = self.afmt.get(&scid).copied().unwrap_or((p::SEAM_CODEC_AAC_LC, 44100, 2));
                if fmt.0 == p::SEAM_CODEC_AAC_LC {
                    if let Some(h) = adts_header(au.len(), fmt.1, fmt.2) {
                        let _ = self.audio.write_all(&h);
                        let _ = self.audio.write_all(&au);
                    }
                }
                let st = self.astat(scid);
                st.ok += 1;
                st.bytes += au.len() as u64;
            }
            _ => {}
        }
    }

    /// One CH_METADATA message: META_JSON events and META_ARTWORK `[id][JPEG]`.
    pub fn meta_msg(&mut self, marker: u8, body: &[u8]) {
        match marker {
            p::META_JSON => {
                let j = String::from_utf8_lossy(body).to_string();
                if !j.contains("\"ev\":\"sync\"") {
                    println!("  event {j}");
                }
                self.note("meta", &j);
            }
            p::META_ARTWORK if body.len() > 1 => {
                self.stats.artwork += 1;
                if let Some(d) = &self.art_dir {
                    let _ = std::fs::write(d.join(format!("artwork-{}.jpg", self.stats.artwork)), &body[1..]);
                }
            }
            _ => {}
        }
    }

    pub fn summary(&self) -> Vec<String> {
        let s = &self.stats;
        let mut out = vec![format!(
            "video: codec {}; {} frames decrypted ({} flagged IDR), {} failed, {} config, {} other (statistics), {} seq jumps, {} key messages, {} B of elementary stream",
            s.video_codec.map_or("none (no configuration frame)", |c| c.name()),
            s.video_ok, s.video_idr, s.video_bad, s.video_cfg, s.video_other, s.video_gaps, s.video_keys, s.video_bytes
        )];
        for (scid, a) in &s.audio {
            let f = self.afmt.get(scid).copied().unwrap_or((255, 0, 0));
            out.push(format!(
                "audio scid {scid}: {} packets decrypted, {} failed, {} empty, {} before a key; codec {} {} Hz {} ch; {} B of access units",
                a.ok, a.bad, a.empty, a.no_key, f.0, f.1, f.2, a.bytes
            ));
        }
        if s.audio.is_empty() {
            out.push("audio: no packets".into());
        }
        out.push(format!("metadata: {} event lines, {} artwork image(s)", s.events, s.artwork));
        out
    }
}

// ---------------------------------------------------------------- the command

pub struct Opts {
    pub req: p::AirplayRequest,
    pub out: std::path::PathBuf,
    pub duration: u64,
    pub exit_after_session: bool,
    pub keep_mode: bool,
    pub box_log: bool,
    pub vid: u16,
    pub pid: u16,
}

pub const USAGE: &str = "usage: ocbm-host airplay --ssid SSID (--psk PASSPHRASE | env OCBM_AIRPLAY_PSK) --out DIR
         [--name RECEIVER_NAME] [--channel N] [--display WxH@FPS] [--hevc] [--duration SECONDS]
         [--exit-after-session] [--keep-mode] [--no-box-log] [--vid HEX] [--pid HEX]
  Selects AirPlay-only mode on the box (AP with exactly this SSID and passphrase, receiver advertised as
  RECEIVER_NAME, default the SSID), then captures until Ctrl-C / --duration / the first session's end and
  returns the box to projection mode (unless --keep-mode).
  --display  the panel advertised in /info (320..7680 x 320..7680 @ 1..120; default 1920x1080@30)
  --hevc     also advertise feature bit 42: with a display larger than 1080p the iPhone then mirrors in
             HEVC (written as video.h265); otherwise it stays H.264 (video.h264)";

impl Opts {
    pub fn parse(a: &[String]) -> Result<Opts, String> {
        let (mut ssid, mut psk, mut name, mut out) = (None, std::env::var("OCBM_AIRPLAY_PSK").ok(), String::new(), None);
        let (mut channel, mut display, mut flags) = (0u8, p::AIRPLAY_DEFAULT_DISPLAY, 0u8);
        let mut o = Opts {
            req: p::AirplayRequest { ssid: String::new(), psk: String::new(), name: String::new(), channel: 0, display, flags },
            out: Default::default(), duration: 0, exit_after_session: false, keep_mode: false, box_log: true, vid: 0x1314, pid: 0x2d00,
        };
        let mut i = 0;
        while i < a.len() {
            let flag = a[i].as_str();
            match flag {
                "--exit-after-session" => o.exit_after_session = true,
                "--keep-mode" => o.keep_mode = true,
                "--no-box-log" => o.box_log = false,
                "--hevc" => flags |= p::AIRPLAY_FLAG_HEVC,
                _ => {
                    i += 1;
                    let v = a.get(i).ok_or_else(|| format!("{flag} needs a value"))?.clone();
                    let bad = || format!("bad value for {flag}: {v}");
                    match flag {
                        "--ssid" => ssid = Some(v),
                        "--psk" => psk = Some(v),
                        "--name" => name = v,
                        "--out" => out = Some(v),
                        "--channel" => channel = v.parse().map_err(|_| bad())?,
                        "--duration" => o.duration = v.parse().map_err(|_| bad())?,
                        "--vid" => o.vid = u16::from_str_radix(&v, 16).map_err(|_| bad())?,
                        "--pid" => o.pid = u16::from_str_radix(&v, 16).map_err(|_| bad())?,
                        "--display" => {
                            let (wh, fps) = v.split_once('@').unwrap_or((&v, "30"));
                            let (w, h) = wh.split_once('x').ok_or_else(bad)?;
                            display = (w.parse().map_err(|_| bad())?, h.parse().map_err(|_| bad())?, fps.parse().map_err(|_| bad())?);
                        }
                        _ => return Err(format!("unknown option {flag}")),
                    }
                }
            }
            i += 1;
        }
        let ssid = ssid.ok_or("--ssid is required")?;
        o.req = p::AirplayRequest { name, ssid, psk: psk.ok_or("--psk (or OCBM_AIRPLAY_PSK) is required")?, channel, display, flags };
        o.out = out.ok_or("--out is required")?.into();
        // Validate here with the box's own rules, so a bad value is named before the link is opened.
        let dec = p::AirplayRequest::decode(&o.req.encode())
            .ok_or("out of range: SSID 1..32 bytes, passphrase 8..63 printable ASCII, name up to 63 bytes, channel 1..14 or 36..165, display 320..7680 x 320..7680 @ 1..120")?;
        o.req = dec; // (fills in the default name)
        Ok(o)
    }
}

/// The `"airplay":{...}` object of a MGMT_INFO JSON (flat: no nested objects), verbatim. A `}` inside a
/// string value (an SSID may hold one) does not end it.
pub fn mgmt_airplay(j: &str) -> Option<&str> {
    let at = j.find("\"airplay\":{")? + "\"airplay\":".len();
    let (mut in_str, mut esc) = (false, false);
    for (i, c) in j[at..].char_indices() {
        match (in_str, esc, c) {
            (true, true, _) => esc = false,
            (true, false, '\\') => esc = true,
            (_, _, '"') => in_str = !in_str,
            (false, _, '}') => return Some(&j[at..at + i + 1]),
            _ => {}
        }
    }
    None
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
extern "C" fn on_sigint(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

pub fn run(link: &mut super::Link, o: &Opts) -> i32 {
    if let Err(e) = std::fs::create_dir_all(&o.out) {
        eprintln!("cannot create {}: {e}", o.out.display());
        return 2;
    }
    let mk = |n: &str| std::fs::File::create(o.out.join(n)).map(std::io::BufWriter::new);
    let (Ok(a), Ok(e), Ok(mut boxlog)) = (mk("audio.aac"), mk("events.log"), mk("box.log")) else {
        eprintln!("cannot create the output files in {}", o.out.display());
        return 2;
    };
    // video.h264 / video.h265 are created when the stream's configuration frame names the codec; a
    // stale one from an earlier run in the same directory would be mistaken for this run's.
    for c in [VideoCodec::H264, VideoCodec::Hevc] {
        let _ = std::fs::remove_file(o.out.join(format!("video.{}", c.ext())));
    }
    let v = VideoFiles::new(&o.out);
    let mut cap = Capture::new(v, a, e);
    cap.art_dir = Some(o.out.clone());
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_sigint as *const () as libc::sighandler_t);
    }
    let ctrl = p::F_SOM | p::F_EOM;
    // The passphrase is sent and never printed.
    println!(
        "airplay: requesting AirPlay-only mode: SSID {:?}, receiver name {:?}, channel {}, display {}x{}@{}, hevc (feature bit 42) {}, passphrase {} chars",
        o.req.ssid, o.req.name, if o.req.channel == 0 { "box default".to_string() } else { o.req.channel.to_string() },
        o.req.display.0, o.req.display.1, o.req.display.2, o.req.hevc(), o.req.psk.len()
    );
    if o.req.hevc() && o.req.display.0 <= 1920 && o.req.display.1 <= 1080 {
        println!("airplay: note: at 1080p or less the iPhone was measured to stay H.264 even with --hevc");
    }
    cap.note(
        "host",
        &format!(
            "MODE_SELECT AIRPLAY ssid={:?} name={:?} display={}x{}@{} hevc={}",
            o.req.ssid, o.req.name, o.req.display.0, o.req.display.1, o.req.display.2, o.req.hevc()
        ),
    );
    link.send(p::CH_CTRL, ctrl, &o.req.encode());
    let (mut vr, mut ar, mut mr) = (SeamReader::new(16 << 20), SeamReader::new(1 << 20), MetaReader::new());
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    let t0 = Instant::now();
    let (mut last_hb, mut last_report) = (Instant::now(), Instant::now());
    let (mut acked, mut state, mut had_session, mut code) = (false, (0u8, 0u8, 0u8), false, 0);
    let mut log_armed = false;
    loop {
        if INTERRUPTED.load(Ordering::SeqCst) {
            println!("airplay: interrupted");
            break;
        }
        if o.duration > 0 && t0.elapsed() >= Duration::from_secs(o.duration) {
            println!("airplay: --duration reached");
            break;
        }
        if !acked && t0.elapsed() > Duration::from_secs(4) {
            eprintln!("airplay: no CT_MODE_ACK from the box: this ocbmd does not know MODE_AIRPLAY");
            return 1;
        }
        if last_hb.elapsed() >= Duration::from_secs(1) {
            last_hb = Instant::now();
            link.send(p::CH_CTRL, ctrl, &[p::CT_HEARTBEAT]);
        }
        if last_report.elapsed() >= Duration::from_secs(5) && state.1 == p::AIRPLAY_SESSION_STREAMING {
            last_report = Instant::now();
            let aud: u64 = cap.stats.audio.iter().map(|(_, a)| a.ok).sum();
            println!("  ... {} video frames, {} audio packets decrypted so far", cap.stats.video_ok, aud);
        }
        let Some((ch, fl, l)) = link.recv(&mut out, Instant::now() + Duration::from_millis(200)) else { continue };
        let (pl, fresh) = (&out[..l], fl & p::F_NEW_SOURCE != 0);
        match ch {
            p::CH_CTRL if l >= 3 && pl[0] == p::CT_MODE_ACK && pl[1] == p::MODE_AIRPLAY => {
                acked = true;
                cap.note("state", &format!("MODE_ACK status={}", pl[2]));
                match pl[2] {
                    p::MODE_ACK_OK => println!("airplay: box accepted the mode request"),
                    p::MODE_ACK_BAD_PARAMS => {
                        eprintln!("airplay: box REFUSED the request: bad parameters");
                        return 1;
                    }
                    other => {
                        eprintln!("airplay: box cannot run AirPlay-only mode (status {other}): airplayd / airplay_mode.sh not installed");
                        return 1;
                    }
                }
                if o.box_log && !log_armed {
                    log_armed = true;
                    link.send(p::CH_CTRL, ctrl, &[p::CT_LOG_CTL, 1, 0, 0]);
                }
                // What the box made of the request (MGMT_INFO `airplay`: `hevc`, `display`).
                link.send(p::CH_MGMT, ctrl, &[p::MGMT_GET_INFO]);
            }
            p::CH_CTRL if l >= 4 && pl[0] == p::CT_AIRPLAY_STATE => {
                let st = (pl[1], pl[2], pl[3]);
                if st == state {
                    continue;
                }
                let line = format!(
                    "ap={} session={} last-end={}",
                    p::airplay_ap_name(st.0), p::airplay_session_name(st.1), p::airplay_end_name(st.2)
                );
                cap.note("state", &line);
                println!("airplay: {line}");
                if st.0 == p::AIRPLAY_AP_UP && state.0 != p::AIRPLAY_AP_UP {
                    link.send(p::CH_MGMT, ctrl, &[p::MGMT_GET_INFO]);
                }
                if st.0 == p::AIRPLAY_AP_UP && st.1 == p::AIRPLAY_SESSION_WAITING {
                    if state.1 > p::AIRPLAY_SESSION_WAITING {
                        println!("airplay: session ended ({})", p::airplay_end_name(st.2));
                        for l in cap.summary() {
                            println!("  {l}");
                        }
                    }
                    println!("airplay: waiting for sender (join Wi-Fi {:?}, then choose {:?} in Screen Mirroring or the AirPlay audio list)", o.req.ssid, o.req.name);
                }
                if st.0 == p::AIRPLAY_AP_FAILED {
                    println!("airplay: the box could not raise the AP (it retries every 10 s; see {}/box.log)", o.out.display());
                }
                had_session |= st.1 >= p::AIRPLAY_SESSION_CONNECTED;
                let ended = had_session && state.1 >= p::AIRPLAY_SESSION_CONNECTED && st.1 <= p::AIRPLAY_SESSION_WAITING;
                let left = acked && st == (0, 0, 0);
                state = st;
                if left {
                    eprintln!("airplay: the box left AirPlay-only mode");
                    code = 1;
                    break;
                }
                if ended && o.exit_after_session {
                    break;
                }
            }
            p::CH_MGMT if l > 1 && pl[0] == p::MGMT_INFO => {
                let j = String::from_utf8_lossy(&pl[1..]).to_string();
                let line = match mgmt_airplay(&j) {
                    Some(a) if o.req.hevc() && !a.contains("\"hevc\":") => {
                        format!("box airplay {a} (this ocbmd predates AIRPLAY_REQ_FLAGS: --hevc was ignored)")
                    }
                    Some(a) => format!("box airplay {a}"),
                    None => "box MGMT_INFO has no airplay object".into(),
                };
                cap.note("state", &line);
                println!("airplay: {line}");
            }
            p::CH_VIDEO => {
                vr.push(pl, fresh);
                while let Some((m, body)) = vr.next_msg() {
                    cap.video_msg(m, &body);
                }
            }
            p::CH_MEDIA_AUDIO => {
                ar.push(pl, fresh);
                while let Some((m, body)) = ar.next_msg() {
                    cap.audio_msg(m, &body);
                }
            }
            p::CH_METADATA => {
                mr.push(pl, fresh);
                while let Some((m, body)) = mr.next_msg() {
                    cap.meta_msg(m, &body);
                }
            }
            p::CH_LOG => {
                let mut b = pl;
                while let Some((e, used)) = p::decode_log_entry(b) {
                    let _ = writeln!(boxlog, "{} [{}] {}", e.unix_ms, p::log_source_name(e.source), String::from_utf8_lossy(e.text));
                    b = &b[used..];
                }
            }
            _ => {}
        }
    }
    if !o.keep_mode {
        link.send(p::CH_CTRL, ctrl, &[p::CT_MODE_SELECT, p::MODE_PROJECTION]);
        let until = Instant::now() + Duration::from_secs(2);
        while let Some((ch, _, l)) = link.recv(&mut out, until) {
            if ch == p::CH_CTRL && l >= 3 && out[0] == p::CT_MODE_ACK && out[1] == p::MODE_PROJECTION {
                println!("airplay: box is back in projection mode (AP down, receiver stopped)");
                break;
            }
        }
    } else {
        println!("airplay: --keep-mode: the box stays in AirPlay-only mode for one heartbeat grace (10 s) after this exits");
    }
    for l in cap.summary() {
        cap.note("summary", &l);
        println!("{l}");
    }
    if vr.resyncs + ar.resyncs + mr.resyncs > 0 {
        let l = format!("seam resyncs: video {} audio {} metadata {}", vr.resyncs, ar.resyncs, mr.resyncs);
        cap.note("summary", &l);
        println!("{l}");
    }
    let _ = (cap.video.flush(), cap.audio.flush(), cap.events.flush(), boxlog.flush());
    if cap.video.dropped > 0 {
        println!("airplay: {} B of video arrived before any configuration frame and were not written", cap.video.dropped);
    }
    let vids: Vec<String> = cap.video.written.iter().map(|c| format!("video.{}", c.ext())).collect();
    let vids = if vids.is_empty() { "no video file (no configuration frame)".to_string() } else { vids.join(", ") };
    println!("airplay: wrote {vids}, audio.aac, events.log, box.log in {}", o.out.display());
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocbm_proto::seam;

    fn seal(key: &[u8; 32], nonce8: [u8; 8], aad: &[u8], msg: &[u8]) -> Vec<u8> {
        let mut n = [0u8; 12];
        n[4..].copy_from_slice(&nonce8);
        ChaCha20Poly1305::new(Key::from_slice(key)).encrypt(Nonce::from_slice(&n), Payload { msg, aad }).unwrap()
    }

    fn hdr(op: u8, flags: u8, body_len: usize) -> [u8; 128] {
        let mut h = [0u8; 128];
        h[0..4].copy_from_slice(&(body_len as u32).to_le_bytes());
        h[4] = op;
        h[5] = flags;
        h
    }

    /// What the box puts on CH_VIDEO for one frame.
    fn video_frame(key: &[u8; 32], seq: u64, op: u8, flags: u8, plain: &[u8]) -> Vec<u8> {
        let body = if op == 0 { seal(key, seq.to_le_bytes(), &hdr(op, flags, plain.len() + 16), plain) } else { plain.to_vec() };
        let h = hdr(op, flags, body.len());
        [seam::video_frame_prefix(seq, &h, body.len()).as_slice(), &body].concat()
    }

    fn audio_pkt(key: &[u8; 32], scid: u64, seq: u16, nonce: u64, au: &[u8]) -> Vec<u8> {
        let mut rtp = [0x80u8, 96, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        rtp[2..4].copy_from_slice(&seq.to_be_bytes());
        rtp[4..8].copy_from_slice(&(u32::from(seq) * 1024).to_be_bytes());
        let mut pkt = rtp.to_vec();
        pkt.extend_from_slice(&seal(key, nonce.to_le_bytes(), &rtp[4..12], au));
        pkt.extend_from_slice(&nonce.to_le_bytes());
        [seam::audio_pkt_prefix(scid, pkt.len()).as_slice(), &pkt].concat()
    }

    fn feed(cap: &mut Capture<Vec<u8>, Vec<u8>, Vec<u8>>, lane: u8, bytes: &[u8], chunk: usize) {
        let mut r = SeamReader::new(16 << 20);
        for c in bytes.chunks(chunk) {
            r.push(c, false);
            while let Some((m, b)) = r.next_msg() {
                if lane == 0 { cap.video_msg(m, &b) } else { cap.audio_msg(m, &b) }
            }
        }
    }

    const AVCC: [u8; 17] = [1, 100, 0, 31, 0xFF, 0xE1, 0, 4, 0x67, 0x64, 0, 31, 1, 0, 2, 0x68, 0xEE];

    #[test]
    fn tunnelled_video_becomes_annex_b_using_the_wire_seq() {
        let key = [0x11u8; 32];
        let nal = |b: u8, n: usize| [&(n as u32).to_be_bytes()[..], &vec![b; n]].concat();
        let f0 = [nal(0x65, 900), nal(0x06, 10)].concat(); // an IDR slice + an SEI in one frame
        let f1 = nal(0x41, 300);
        let f2 = nal(0x41, 200);
        let mut lane = seam::key_msg(&key, 42).to_vec();
        lane.extend(video_frame(&key, 0, 1, 0, &AVCC));
        lane.extend(video_frame(&key, 0, 0, 0x10, &f0));
        lane.extend(video_frame(&key, 1, 5, 0, b"bplist00 statistics, plaintext"));
        lane.extend(video_frame(&key, 1, 0, 0, &f1));
        // frames 2..9 were lost on the way: the next one still decrypts, because the nonce is its own seq
        lane.extend(video_frame(&key, 10, 0, 0, &f2));
        // a frame under another key fails its tag and writes nothing
        lane.extend(video_frame(&[0x22; 32], 11, 0, 0, &f2));
        for chunk in [lane.len(), 61] {
            let mut cap = Capture::new(Vec::new(), Vec::new(), Vec::new());
            feed(&mut cap, 0, &lane, chunk);
            let sc = [0u8, 0, 0, 1];
            let want = [
                &sc[..], &[0x67, 0x64, 0, 31], &sc, &[0x68, 0xEE], // SPS, PPS from the avcC
                &sc, &vec![0x65; 900], &sc, &vec![0x06; 10],
                &sc, &vec![0x41; 300],
                &sc, &vec![0x41; 200],
            ]
            .concat();
            assert_eq!(cap.video, want);
            let s = &cap.stats;
            assert_eq!((s.video_ok, s.video_bad, s.video_idr, s.video_cfg, s.video_other, s.video_gaps), (3, 1, 1, 1, 1, 1));
            let log = String::from_utf8(cap.events.clone()).unwrap();
            assert!(log.contains("video key for scid 42: 32 B 11111111.."));
            assert!(!log.contains(&"11".repeat(5)), "the key is never written out");
            assert!(log.contains("video seq jump: expected 2 got 10"));
        }
    }

    #[test]
    fn tunnelled_audio_becomes_adts_per_stream_key_and_survives_a_rekey() {
        let (k1, k2, shk) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        let mut lane = Vec::new();
        lane.extend(audio_pkt(&k1, 7, 0, 1, b"dropped: no key yet"));
        lane.extend(seam::key_msg(&k1, 7));
        lane.extend(seam::audio_format_msg(7, p::SEAM_CODEC_AAC_LC, 44100, 2, 0, 0));
        lane.extend(audio_pkt(&k1, 7, 1, 2, &[0xA1; 300]));
        lane.extend(audio_pkt(&k1, 7, 2, 3, &[])); // the 0-byte access unit seen in the measurements
        // mid-session: the phone sets a new realtime stream up (new scid, new key), then buffered audio
        lane.extend(seam::key_msg(&k2, 8));
        lane.extend(seam::audio_format_msg(8, p::SEAM_CODEC_AAC_LC, 48000, 2, 0, 0));
        lane.extend(audio_pkt(&k2, 8, 1, 9, &[0xB2; 6]));
        lane.extend(audio_pkt(&k1, 8, 2, 10, &[0xEE; 6])); // the OLD key on the new stream: rejected
        lane.extend(seam::key_msg(&shk, 9));
        lane.extend(audio_pkt(&shk, 9, 1, 1, &[0xC3; 2000])); // no SEAM_FORMAT yet: AAC-LC 44.1 kHz stereo assumed
        let mut cap = Capture::new(Vec::new(), Vec::new(), Vec::new());
        feed(&mut cap, 1, &lane, 97);
        let want = [
            &adts_header(300, 44100, 2).unwrap()[..], &[0xA1; 300],
            &adts_header(6, 48000, 2).unwrap(), &[0xB2; 6],
            &adts_header(2000, 44100, 2).unwrap(), &[0xC3; 2000],
        ]
        .concat();
        assert_eq!(cap.audio, want);
        let st: HashMap<u64, AudioStat> = cap.stats.audio.iter().cloned().collect();
        assert_eq!((st[&7].ok, st[&7].no_key, st[&7].empty, st[&7].bad), (1, 1, 1, 0));
        assert_eq!((st[&8].ok, st[&8].bad), (1, 1));
        assert_eq!(st[&9].ok, 1);
    }

    #[test]
    fn adts_header_bytes_are_the_aac_lc_stereo_ones() {
        // 44.1 kHz stereo AAC-LC, 300-byte AU: syncword, MPEG-4 no CRC, profile LC, sr index 4, 2 channels
        assert_eq!(adts_header(300, 44100, 2), Some([0xFF, 0xF1, 0x50, 0x80, 0x26, 0x7F, 0xFC]));
        assert_eq!(adts_header(6, 48000, 2).unwrap()[2], 0x4C);
        assert_eq!(adts_header(10, 12345, 2), None);
        assert_eq!(adts_header(9000, 44100, 2), None); // does not fit the 13-bit frame length
    }

    #[test]
    fn options_are_validated_before_the_link_is_opened() {
        let a = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        let o = Opts::parse(&a("--ssid Bench --psk 12345678 --out /tmp/x --display 1280x720@60 --channel 36 --duration 30 --exit-after-session")).unwrap();
        assert_eq!((o.req.ssid.as_str(), o.req.name.as_str(), o.req.channel, o.req.display), ("Bench", "Bench", 36, (1280, 720, 60)));
        assert_eq!((o.duration, o.exit_after_session, o.keep_mode, o.vid, o.pid), (30, true, false, 0x1314, 0x2d00));
        assert_eq!(Opts::parse(&a("--ssid Bench --psk 12345678 --name Car --out /tmp/x")).unwrap().req.name, "Car");
        assert!(Opts::parse(&a("--ssid Bench --psk short --out /tmp/x")).is_err());
        assert!(Opts::parse(&a("--ssid Bench --psk 12345678")).is_err());
        assert!(Opts::parse(&a("--ssid Bench --psk 12345678 --out /tmp/x --bogus 1")).is_err());
        assert!(!o.req.hevc());
        let o = Opts::parse(&a("--ssid Bench --psk 12345678 --out /tmp/x --hevc --display 3840x2160@60")).unwrap();
        assert_eq!((o.req.hevc(), o.req.display), (true, (3840, 2160, 60)));
        assert!(o.req.encode().ends_with(&[p::AIRPLAY_REQ_FLAGS, 1, p::AIRPLAY_FLAG_HEVC]));
        assert_eq!(Opts::parse(&a("--ssid Bench --psk 12345678 --out /tmp/x --display 1280x720")).unwrap().req.display, (1280, 720, 30));
        for bad in ["7681x2160@60", "3840x2160@121", "3840x2160@0", "319x240@30", "3840@60", "axb@1"] {
            assert!(Opts::parse(&a(&format!("--ssid Bench --psk 12345678 --out /tmp/x --display {bad}"))).is_err(), "{bad}");
        }
    }

    #[test]
    fn mgmt_info_airplay_object_is_lifted_verbatim() {
        let j = r#"{"mode":"airplay","airplay":{"active":true,"ap":"up","ssid":"a}b\"c","name":"n","hevc":true,"display":"3840x2160@60"},"bundle":{}}"#;
        assert_eq!(
            mgmt_airplay(j),
            Some(r#"{"active":true,"ap":"up","ssid":"a}b\"c","name":"n","hevc":true,"display":"3840x2160@60"}"#)
        );
        assert_eq!(mgmt_airplay(r#"{"mode":"projection"}"#), None);
    }

    #[test]
    fn video_files_follow_the_configuration_frame() {
        let dir = std::env::temp_dir().join(format!("ocbm-host-vfiles-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut v = VideoFiles::new(&dir);
        v.write_all(b"before any config").unwrap();
        v.codec(VideoCodec::Hevc);
        v.write_all(b"hevc 1").unwrap();
        v.codec(VideoCodec::H264);
        v.write_all(b"avc").unwrap();
        v.codec(VideoCodec::Hevc);
        v.write_all(b" hevc 2").unwrap();
        v.flush().unwrap();
        drop(v.cur.take());
        assert_eq!((v.dropped, v.written.clone()), (17, vec![VideoCodec::Hevc, VideoCodec::H264]));
        assert_eq!(std::fs::read(dir.join("video.h265")).unwrap(), b"hevc 1 hevc 2");
        assert_eq!(std::fs::read(dir.join("video.h264")).unwrap(), b"avc");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End to end with real media, when ffmpeg is installed: encode H.264 + AAC-LC, wrap them exactly as
    /// the phone and the box would (avcC + length-prefixed NALs encrypted per frame; raw access units
    /// encrypted per packet), run them through the capture, and have ffmpeg decode what comes out.
    #[test]
    fn ffmpeg_media_survives_the_tunnel_and_decodes() {
        let ff = |args: &[&str]| std::process::Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).output();
        if ff(&["-version"]).map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("ffmpeg not installed: skipped");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ocbm-host-airplay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = |n: &str| dir.join(n).to_string_lossy().to_string();
        let o = ff(&["-f", "lavfi", "-i", "testsrc=size=320x240:rate=30", "-frames:v", "45", "-c:v", "libx264", "-profile:v", "high",
            "-pix_fmt", "yuv420p", "-bf", "0", "-x264-params", "keyint=300:slices=1:aud=0", "-f", "h264", &path("src.h264")]).unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let o = ff(&["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100:duration=2", "-ac", "2", "-c:a", "aac", "-b:a", "128k", "-f", "adts", &path("src.aac")]).unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

        // ---- the phone's side: Annex-B -> avcC + one access unit per frame
        let es = std::fs::read(path("src.h264")).unwrap();
        let mut nals: Vec<&[u8]> = Vec::new();
        let mut starts: Vec<(usize, usize)> = Vec::new(); // (start code offset, payload offset)
        let mut i = 0;
        while i + 3 <= es.len() {
            if es[i..].starts_with(&[0, 0, 1]) {
                starts.push((if i > 0 && es[i - 1] == 0 { i - 1 } else { i }, i + 3));
                i += 3;
            } else {
                i += 1;
            }
        }
        for (n, (_, at)) in starts.iter().enumerate() {
            nals.push(&es[*at..starts.get(n + 1).map(|s| s.0).unwrap_or(es.len())]);
        }
        let (sps, pps) = (nals.iter().find(|n| n[0] & 0x1f == 7).unwrap(), nals.iter().find(|n| n[0] & 0x1f == 8).unwrap());
        let mut avcc = vec![1, sps[1], sps[2], sps[3], 0xFF, 0xE1];
        avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(sps);
        avcc.push(1);
        avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(pps);
        let vkey = [0x5Au8; 32];
        let mut vlane = seam::key_msg(&vkey, 1).to_vec();
        vlane.extend(video_frame(&vkey, 0, 1, 0, &avcc));
        let (mut seq, mut au) = (0u64, Vec::new());
        for n in &nals {
            let t = n[0] & 0x1f;
            if t == 7 || t == 8 {
                continue;
            }
            au.extend_from_slice(&(n.len() as u32).to_be_bytes());
            au.extend_from_slice(n);
            if t == 1 || t == 5 {
                vlane.extend(video_frame(&vkey, seq, 0, if t == 5 { 0x10 } else { 0 }, &au));
                seq += 1;
                au.clear();
            }
        }
        assert_eq!(seq, 45);
        // ADTS -> raw access units, each in one encrypted RTP packet
        let adts = std::fs::read(path("src.aac")).unwrap();
        let akey = [0xA5u8; 32];
        let mut alane = seam::key_msg(&akey, 2).to_vec();
        alane.extend(seam::audio_format_msg(2, p::SEAM_CODEC_AAC_LC, 44100, 2, 0, 0));
        let (mut at, mut n_au) = (0usize, 0u16);
        while at + 7 <= adts.len() {
            let len = (usize::from(adts[at + 3] & 3) << 11) | (usize::from(adts[at + 4]) << 3) | usize::from(adts[at + 5] >> 5);
            alane.extend(audio_pkt(&akey, 2, n_au, u64::from(n_au) + 1, &adts[at + 7..at + len]));
            n_au += 1;
            at += len;
        }

        // ---- the host's side
        let mut cap = Capture::new(Vec::new(), Vec::new(), Vec::new());
        feed(&mut cap, 0, &vlane, 4096);
        feed(&mut cap, 1, &alane, 4096);
        assert_eq!((cap.stats.video_ok, cap.stats.video_bad, cap.stats.video_idr), (45, 0, 1));
        assert_eq!(cap.stats.audio[0].1.ok, u64::from(n_au));
        assert_eq!(cap.audio, adts, "the ADTS file is rebuilt byte for byte");
        std::fs::write(path("video.h264"), &cap.video).unwrap();
        std::fs::write(path("audio.aac"), &cap.audio).unwrap();
        let probe = |file: &str, sel: &str| {
            let o = std::process::Command::new("ffprobe")
                .args(["-v", "error", "-count_frames", "-select_streams", sel, "-show_entries", "stream=codec_name,nb_read_frames", "-of", "csv=p=0", &path(file)])
                .output()
                .unwrap();
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        assert_eq!(probe("video.h264", "v:0"), "h264,45");
        assert!(probe("audio.aac", "a:0").starts_with("aac,"));
        // and it decodes without a single error
        let o = ff(&["-xerror", "-i", &path("video.h264"), "-f", "null", "-"]).unwrap();
        assert!(o.status.success() && o.stderr.is_empty(), "{}", String::from_utf8_lossy(&o.stderr));
        let o = ff(&["-xerror", "-i", &path("audio.aac"), "-f", "null", "-"]).unwrap();
        assert!(o.status.success() && o.stderr.is_empty(), "{}", String::from_utf8_lossy(&o.stderr));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// HEVC end to end, when ffmpeg has libx265: encode HEVC, wrap it exactly as the iPhone does with
    /// feature bit 42 (opcode 1 = `[size]["hvc1"][78-byte visual sample entry][hvcC box][colr box]`,
    /// opcode 0 = 4-byte length-prefixed NALs, encrypted per frame), run it through the capture into a
    /// file sink, and have ffprobe/ffmpeg decode the resulting video.h265.
    #[test]
    fn ffmpeg_hevc_survives_the_tunnel_and_is_written_as_h265() {
        let ff = |args: &[&str]| std::process::Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).output();
        let has_x265 = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-encoders"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("libx265"))
            .unwrap_or(false);
        if !has_x265 {
            eprintln!("ffmpeg with libx265 not installed: skipped");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ocbm-host-airplay-hevc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = |n: &str| dir.join(n).to_string_lossy().to_string();
        let o = ff(&["-f", "lavfi", "-i", "testsrc=size=320x240:rate=30", "-frames:v", "40", "-c:v", "libx265", "-pix_fmt", "yuv420p",
            "-x265-params", "log-level=error:keyint=300:bframes=0:repeat-headers=1:aud=0", "-f", "hevc", &path("src.h265")]).unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

        // ---- the phone's side: Annex-B -> hvcC in an hvc1 sample entry + one access unit per picture
        let es = std::fs::read(path("src.h265")).unwrap();
        let mut starts: Vec<(usize, usize)> = Vec::new();
        let mut i = 0;
        while i + 3 <= es.len() {
            if es[i..].starts_with(&[0, 0, 1]) {
                starts.push((if i > 0 && es[i - 1] == 0 { i - 1 } else { i }, i + 3));
                i += 3;
            } else {
                i += 1;
            }
        }
        let nals: Vec<&[u8]> = (0..starts.len()).map(|n| &es[starts[n].1..starts.get(n + 1).map(|s| s.0).unwrap_or(es.len())]).collect();
        let ty = |n: &[u8]| (n[0] >> 1) & 0x3f;
        let ps = |t: u8| *nals.iter().find(|n| ty(n) == t).unwrap();
        let sps = ps(33);
        let mut rbsp = Vec::new();
        for (i, x) in sps[2..].iter().enumerate() {
            if !(*x == 3 && i >= 2 && sps[i] == 0 && sps[i + 1] == 0) {
                rbsp.push(*x);
            }
        }
        let mut hvcc = vec![1u8];
        hvcc.extend_from_slice(&rbsp[1..13]);
        hvcc.extend_from_slice(&[0xF0, 0, 0xFC, 0xFD, 0xF8, 0xF8, 0, 0, 0x0F, 3]);
        for t in [32u8, 33, 34] {
            hvcc.extend_from_slice(&[0x80 | t, 0, 1]);
            hvcc.extend_from_slice(&(ps(t).len() as u16).to_be_bytes());
            hvcc.extend_from_slice(ps(t));
        }
        let mut entry = vec![0u8; 8 + 78];
        entry[4..8].copy_from_slice(b"hvc1");
        entry.extend_from_slice(&((hvcc.len() + 8) as u32).to_be_bytes());
        entry.extend_from_slice(b"hvcC");
        entry.extend_from_slice(&hvcc);
        entry.extend_from_slice(&[&20u32.to_be_bytes()[..], b"colr", b"nclx", &[0, 1, 0, 1, 0, 1, 0, 0]].concat());
        let n = entry.len() as u32;
        entry[..4].copy_from_slice(&n.to_be_bytes());
        assert_eq!(&entry[4..8], b"hvc1");
        assert_eq!(&entry[86 + 4..86 + 8], b"hvcC", "the verified layout: hvcC right after the 78-byte entry");

        let vkey = [0x6Bu8; 32];
        let mut vlane = seam::key_msg(&vkey, 3).to_vec();
        vlane.extend(video_frame(&vkey, 0, 1, 0, &entry));
        let (mut seq, mut au, mut irap) = (0u64, Vec::new(), 0);
        for n in &nals {
            let t = ty(n);
            if (32..=35).contains(&t) {
                continue;
            }
            au.extend_from_slice(&(n.len() as u32).to_be_bytes());
            au.extend_from_slice(n);
            if t < 32 {
                let sync = (16..=21).contains(&t);
                irap += usize::from(sync);
                vlane.extend(video_frame(&vkey, seq, 0, if sync { 0x10 } else { 0 }, &au));
                seq += 1;
                au.clear();
            }
        }
        assert_eq!((seq, irap), (40, 1));

        // ---- the host's side, into the command's own file sink
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let mut cap = Capture::new(VideoFiles::new(&out), Vec::new(), Vec::new());
        let mut r = SeamReader::new(16 << 20);
        for c in vlane.chunks(1000) {
            r.push(c, false);
            while let Some((m, b)) = r.next_msg() {
                cap.video_msg(m, &b);
            }
        }
        cap.video.flush().unwrap();
        assert_eq!((cap.stats.video_ok, cap.stats.video_bad, cap.stats.video_idr, cap.stats.video_cfg), (40, 0, 1, 1));
        assert_eq!(cap.stats.video_codec, Some(VideoCodec::Hevc));
        assert_eq!(cap.video.written, [VideoCodec::Hevc]);
        assert!(!out.join("video.h264").exists(), "an HEVC stream is never written as .h264");
        assert!(cap.summary()[0].starts_with("video: codec HEVC (H.265); 40 frames decrypted"), "{}", cap.summary()[0]);
        let log = String::from_utf8(cap.events.clone()).unwrap();
        assert!(log.contains("hvcC in an 'hvc1' sample entry (HEVC / H.265): profile_idc 1 (Main)"), "{log}");
        let h265 = out.join("video.h265").to_string_lossy().to_string();
        let w = std::fs::read(&h265).unwrap();
        assert!(w.starts_with(&[0, 0, 0, 1]) && ty(&w[4..]) == 32, "VPS first");
        let o = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries", "stream=codec_name,width,height,nb_read_frames", "-of", "csv=p=0", &h265])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "hevc,320,240,40");
        let o = ff(&["-xerror", "-i", &h265, "-f", "null", "-"]).unwrap();
        assert!(o.status.success() && o.stderr.is_empty(), "{}", String::from_utf8_lossy(&o.stderr));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
