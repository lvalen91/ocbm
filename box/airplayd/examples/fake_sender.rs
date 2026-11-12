//! fake_sender — a bench stand-in for the iPhone, for testing AirPlay-only mode end to end WITHOUT a
//! phone: it does what the measured sender does (docs/ocbm/02_AIRPLAY_ONLY_MODE.md §4) against a
//! running airplayd, with real media, so the box's tunnel, the OCBM link and the host's decrypt can be
//! checked on real hardware. Never installed and not part of any image (a cargo example).
//!
//!   fake_sender HOST:PORT VIDEO.h264|VIDEO.h265 AUDIO.aac
//!
//! VIDEO = Annex-B H.264 (one slice per frame; sent as a bare avcC, as the iPhone does), or with a
//! `.h265`/`.hevc` name Annex-B HEVC (one slice per picture; sent the way the iPhone sends HEVC after
//! feature bit 42: an `hvc1` sample entry carrying an `hvcC` box, 4-byte NAL lengths), AUDIO = ADTS AAC-LC 44.1 kHz stereo. Sequence: transient
//! pair-setup, MFi auth-setup (the receiver's real chip answers), session SETUP, RECORD, type 110 (avcC
//! + every frame, an opcode-5 statistics frame now and then), type 96 with the first half of the audio
//! (each packet three times), TEARDOWN of 96 and a NEW type 96 with the second half, type 103 with all
//! of it (shk-keyed, TCP), volume / track / artwork, session TEARDOWN. The controller side of the
//! pairing is the pairing crate's own `srp::client_proof`; the stream encryption is the documented
//! ChaCha20-Poly1305 layout. Exit 0 = every request was answered 200.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use pairing::crypto::{hkdf_sha512, tlv_type};
use pairing::srp::client_proof;
use pairing::tlv;
use rtsp::control::{derive_control_keys, ControlChannel, ControlKeys};
use std::io::{Read, Write};
use std::net::{TcpStream, UdpSocket};
use std::time::Duration;

fn rt(s: &mut TcpStream, chan: &mut Option<ControlChannel>, head: &str, body: &[u8]) -> (u16, Vec<u8>) {
    let mut req = format!("{head}\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
    req.extend_from_slice(body);
    let wire = match chan.as_mut() {
        Some(c) => c.encrypt_frame(&req),
        None => req,
    };
    s.write_all(&wire).expect("control write");
    let (mut raw, mut plain, mut buf) = (Vec::new(), Vec::new(), [0u8; 4096]);
    loop {
        match chan.as_mut() {
            Some(c) => {
                while let Some((pt, n)) = c.decrypt_frame(&raw).expect("response decrypts") {
                    plain.extend_from_slice(&pt);
                    raw.drain(..n);
                }
            }
            None => plain.append(&mut raw),
        }
        if let Some(h) = plain.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&plain[..h]).to_string();
            let len: usize = head.lines().find_map(|l| l.strip_prefix("Content-Length: ")).map(|v| v.parse().unwrap()).unwrap_or(0);
            if plain.len() >= h + 4 + len {
                return (head.split(' ').nth(1).unwrap().parse().unwrap(), plain[h + 4..h + 4 + len].to_vec());
            }
        }
        let n = s.read(&mut buf).expect("control read");
        assert!(n > 0, "receiver closed the control connection");
        raw.extend_from_slice(&buf[..n]);
    }
}

fn seal(key: &[u8; 32], nonce8: [u8; 8], aad: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&nonce8);
    ChaCha20Poly1305::new(Key::from_slice(key)).encrypt(Nonce::from_slice(&n), Payload { msg, aad }).unwrap()
}

fn bplist(d: plist::Dictionary) -> Vec<u8> {
    let mut o = Vec::new();
    plist::Value::Dictionary(d).to_writer_binary(&mut o).unwrap();
    o
}

fn dict(fields: &[(&str, plist::Value)]) -> plist::Dictionary {
    let mut d = plist::Dictionary::new();
    for (k, v) in fields {
        d.insert((*k).into(), v.clone());
    }
    d
}

struct Ctl {
    s: TcpStream,
    chan: Option<ControlChannel>,
    cseq: u32,
}

impl Ctl {
    fn req(&mut self, method: &str, ctype: &str, body: &[u8]) -> Vec<u8> {
        self.cseq += 1;
        let path = if method.starts_with("POST") || method.starts_with("GET /") { "" } else { " rtsp://sender/1" };
        let head = format!("{method}{path} RTSP/1.0\r\nCSeq: {}\r\nContent-Type: {ctype}", self.cseq);
        let (st, resp) = rt(&mut self.s, &mut self.chan, &head, body);
        println!("  {method} -> {st} ({} B)", resp.len());
        assert_eq!(st, 200, "{method} was not answered 200");
        resp
    }
    fn setup(&mut self, d: plist::Dictionary) -> plist::Dictionary {
        let r = self.req("SETUP", "application/x-apple-binary-plist", &bplist(d));
        plist::Value::from_reader(std::io::Cursor::new(r)).unwrap().into_dictionary().unwrap()
    }
    fn stream(&mut self, fields: &[(&str, plist::Value)]) -> (u16, u16) {
        let d = dict(&[("streams", plist::Value::Array(vec![plist::Value::Dictionary(dict(fields))]))]);
        let r = self.setup(d);
        let s = r.get("streams").unwrap().as_array().unwrap()[0].as_dictionary().unwrap().clone();
        let port = |k: &str| s.get(k).and_then(|v| v.as_unsigned_integer()).unwrap_or(0) as u16;
        (port("dataPort"), port("controlPort"))
    }
}

/// H.264: a bare avcC (what the iPhone sends) and one length-prefixed access unit per coded picture.
fn avc_config_and_frames(nals: &[&[u8]]) -> (Vec<u8>, Vec<(bool, Vec<u8>)>) {
    let (sps, pps) = (nals.iter().find(|n| n[0] & 0x1f == 7).expect("SPS"), nals.iter().find(|n| n[0] & 0x1f == 8).expect("PPS"));
    let mut avcc = vec![1, sps[1], sps[2], sps[3], 0xFF, 0xE1];
    avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(sps);
    avcc.push(1);
    avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(pps);
    let (mut frames, mut au) = (Vec::new(), Vec::new());
    for n in nals {
        let t = n[0] & 0x1f;
        if t == 7 || t == 8 {
            continue;
        }
        au.extend_from_slice(&(n.len() as u32).to_be_bytes());
        au.extend_from_slice(n);
        if t == 1 || t == 5 {
            frames.push((t == 5, std::mem::take(&mut au)));
        }
    }
    (avcc, frames)
}

/// HEVC the way the iPhone sends it: `[size]["hvc1"][78-byte visual sample entry][hvcC box]`, then one
/// length-prefixed access unit per picture (VCL NAL types 0..=31; IRAP 16..=21 flagged as sync).
fn hevc_config_and_frames(nals: &[&[u8]]) -> (Vec<u8>, Vec<(bool, Vec<u8>)>) {
    let ty = |n: &[u8]| (n[0] >> 1) & 0x3f;
    let ps = |t: u8| *nals.iter().find(|n| ty(n) == t).unwrap_or_else(|| panic!("HEVC parameter set type {t}"));
    let (vps, sps, pps) = (ps(32), ps(33), ps(34));
    // profile_tier_level: the 12 bytes after the SPS's first payload byte, emulation prevention removed.
    let mut rbsp = Vec::new();
    for (i, x) in sps[2..].iter().enumerate() {
        if !(*x == 3 && i >= 2 && sps[i] == 0 && sps[i + 1] == 0) {
            rbsp.push(*x);
        }
    }
    let mut hvcc = vec![1u8];
    hvcc.extend_from_slice(&rbsp[1..13]); // profile byte, 4 compatibility, 6 constraint, level
    hvcc.extend_from_slice(&[0xF0, 0, 0xFC, 0xFD, 0xF8, 0xF8, 0, 0, 0x0F, 3]);
    for (t, nal) in [(32u8, vps), (33, sps), (34, pps)] {
        hvcc.extend_from_slice(&[0x80 | t, 0, 1]);
        hvcc.extend_from_slice(&(nal.len() as u16).to_be_bytes());
        hvcc.extend_from_slice(nal);
    }
    let mut entry = vec![0u8; 8 + 78];
    entry[4..8].copy_from_slice(b"hvc1");
    entry[8 + 6..8 + 8].copy_from_slice(&1u16.to_be_bytes()); // data_reference_index
    entry.extend_from_slice(&((hvcc.len() + 8) as u32).to_be_bytes());
    entry.extend_from_slice(b"hvcC");
    entry.extend_from_slice(&hvcc);
    let n = entry.len() as u32;
    entry[..4].copy_from_slice(&n.to_be_bytes());
    let (mut frames, mut au) = (Vec::new(), Vec::new());
    for n in nals {
        let t = ty(n);
        if (32..=34).contains(&t) || t == 35 {
            continue;
        }
        au.extend_from_slice(&(n.len() as u32).to_be_bytes());
        au.extend_from_slice(n);
        if t < 32 {
            frames.push(((16..=21).contains(&t), std::mem::take(&mut au)));
        }
    }
    (entry, frames)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 4 {
        eprintln!("usage: fake_sender HOST:PORT VIDEO.h264|VIDEO.h265 AUDIO.aac");
        std::process::exit(64);
    }
    let host = a[1].rsplit_once(':').expect("HOST:PORT").0.to_string();
    let (es, adts) = (std::fs::read(&a[2]).expect("video file"), std::fs::read(&a[3]).expect("audio file"));

    // ---- media: Annex-B -> NAL units; ADTS -> raw access units
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= es.len() {
        if es[i..].starts_with(&[0, 0, 1]) {
            starts.push((if i > 0 && es[i - 1] == 0 { i - 1 } else { i }, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let nals: Vec<&[u8]> = starts.iter().enumerate().map(|(n, (_, at))| &es[*at..starts.get(n + 1).map(|s| s.0).unwrap_or(es.len())]).collect();
    let hevc = a[2].ends_with(".h265") || a[2].ends_with(".hevc");
    let (avcc, frames) = if hevc { hevc_config_and_frames(&nals) } else { avc_config_and_frames(&nals) };
    let mut aus: Vec<&[u8]> = Vec::new();
    let mut at = 0usize;
    while at + 7 <= adts.len() {
        let len = (usize::from(adts[at + 3] & 3) << 11) | (usize::from(adts[at + 4]) << 3) | usize::from(adts[at + 5] >> 5);
        aus.push(&adts[at + 7..at + len]);
        at += len;
    }
    println!("media: {} video frames, {} audio access units", frames.len(), aus.len());

    // ---- control: transient pair-setup, auth-setup, session
    let s = TcpStream::connect(&a[1]).expect("connect");
    let mut c = Ctl { s, chan: None, cseq: 0 };
    let m1 = tlv::encode(&[(tlv_type::METHOD, &[0]), (tlv_type::STATE, &[1]), (tlv_type::FLAGS, &[0x10])]);
    let m2 = tlv::decode(&c.req("POST /pair-setup", "application/octet-stream", &m1)).unwrap();
    let get = |t: u8| m2.iter().find(|(k, _)| *k == t).map(|(_, v)| v.clone()).unwrap();
    let mut rnd = [0u8; 64];
    std::fs::File::open("/dev/urandom").unwrap().read_exact(&mut rnd).unwrap();
    let (a_pub, proof, secret) = client_proof(b"Pair-Setup", b"3939", &get(tlv_type::SALT), &rnd[..32], &get(tlv_type::PUBLIC_KEY));
    let m3 = tlv::encode(&[(tlv_type::STATE, &[3]), (tlv_type::PUBLIC_KEY, &a_pub), (tlv_type::PROOF, &proof)]);
    let m4 = tlv::decode(&c.req("POST /pair-setup", "application/octet-stream", &m3)).unwrap();
    assert!(!m4.iter().any(|(k, _)| *k == tlv_type::ERROR), "receiver rejected the SRP proof");
    let k = derive_control_keys(&secret);
    c.chan = Some(ControlChannel::new(ControlKeys { read: k.write, write: k.read }));
    let mut sap = vec![1u8];
    sap.extend_from_slice(&rnd[32..]);
    let m2 = c.req("POST /auth-setup", "application/octet-stream", &sap);
    let cert_len = u32::from_be_bytes(m2[32..36].try_into().unwrap()) as usize;
    println!("auth-setup: receiver answered {} B (certificate {cert_len} B, signature {} B)", m2.len(), m2.len() - 40 - cert_len);
    let sess = c.setup(dict(&[
        ("timingProtocol", "NTP".into()),
        ("isScreenMirroringSession", true.into()),
        ("name", "fake sender".into()),
        ("model", "Bench1,1".into()),
        ("deviceID", "02:00:00:00:00:01".into()),
    ]));
    let event_port = sess.get("eventPort").and_then(|v| v.as_unsigned_integer()).unwrap() as u16;
    let _event = TcpStream::connect((host.as_str(), event_port)).expect("event port");
    c.req("RECORD", "text/parameters", b"");
    let key_of = |scid: u64| hkdf_sha512::<32>(&secret, format!("DataStream-Salt{scid}").as_bytes(), b"DataStream-Output-Encryption-Key");

    // ---- type 110: the screen. A scid with the top bit set travels as a negative plist integer.
    let vscid = 0xF000_0000_0000_0000u64 | u64::from_le_bytes(rnd[..8].try_into().unwrap()) >> 4;
    let (vport, _) = c.stream(&[("type", 110u64.into()), ("streamConnectionID", (vscid as i64).into()), ("latencyMs", 100u64.into())]);
    let vkey = key_of(vscid);
    let host2 = host.clone();
    let video = std::thread::spawn(move || {
        let mut d = TcpStream::connect((host2.as_str(), vport)).expect("video data port");
        let hdr = |op: u8, flags: u8, n: usize, t: u64| {
            let mut h = [0u8; 128];
            h[0..4].copy_from_slice(&(n as u32).to_le_bytes());
            h[4] = op;
            h[5] = flags;
            h[8..16].copy_from_slice(&((1000 << 32) + t * 143_165_576).to_le_bytes()); // 32.32 seconds, 30 fps
            h
        };
        d.write_all(&hdr(1, 0, avcc.len(), 0)).unwrap();
        d.write_all(&avcc).unwrap();
        for (n, (idr, f)) in frames.iter().enumerate() {
            if n % 30 == 29 {
                d.write_all(&hdr(5, 0, 12, n as u64)).unwrap();
                d.write_all(b"bplist00stat").unwrap();
            }
            let h = hdr(0, if *idr { 0x10 } else { 0 }, f.len() + 16, n as u64);
            d.write_all(&h).unwrap();
            d.write_all(&seal(&vkey, (n as u64).to_le_bytes(), &h, f)).unwrap();
            std::thread::sleep(Duration::from_millis(33));
        }
        frames.len()
    });

    // ---- type 96: realtime audio, each packet three times; torn down and set up again half way
    let udp = UdpSocket::bind("0.0.0.0:0").unwrap();
    let half = aus.len() / 2;
    let mut seq = 65000u16; // crosses the 16-bit wrap
    for (part, range) in [(0..half), (half..aus.len())].into_iter().enumerate() {
        let scid = 9600 + part as u64;
        let (dport, cport) = c.stream(&[
            ("type", 96u64.into()), ("streamConnectionID", scid.into()), ("ct", 4u64.into()), ("audioFormat", 0x40_0000u64.into()),
            ("sr", 44100u64.into()), ("spf", 1024u64.into()), ("redundantAudio", 2u64.into()),
        ]);
        let key = key_of(scid);
        for (n, au) in aus[range].iter().enumerate() {
            let mut rtp = [0x80u8, 96, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            rtp[2..4].copy_from_slice(&seq.to_be_bytes());
            rtp[4..8].copy_from_slice(&(n as u32 * 1024).to_be_bytes());
            let nonce = (n as u64 + 1).to_le_bytes();
            let mut pkt = rtp.to_vec();
            pkt.extend_from_slice(&seal(&key, nonce, &rtp[4..12], au));
            pkt.extend_from_slice(&nonce);
            for _ in 0..3 {
                udp.send_to(&pkt, (host.as_str(), dport)).unwrap();
            }
            if n % 43 == 0 {
                // the ~1/s sync packet on the control port
                let mut sync = [0x80u8, 0xd4, 0, 4, 0, 0, 0, 0, 0, 0, 3, 0xe8, 0, 0, 0, 0, 0, 0, 0, 0];
                sync[16..20].copy_from_slice(&(n as u32 * 1024).to_be_bytes());
                udp.send_to(&sync, (host.as_str(), cport)).unwrap();
            }
            seq = seq.wrapping_add(1);
            std::thread::sleep(Duration::from_millis(12)); // about twice real time
        }
        std::thread::sleep(Duration::from_millis(300));
        let td = dict(&[("streams", plist::Value::Array(vec![plist::Value::Dictionary(dict(&[("type", 96u64.into()), ("streamID", 0u64.into())]))]))]);
        c.req("TEARDOWN", "application/x-apple-binary-plist", &bplist(td));
    }

    // ---- type 103: buffered audio, keyed by shk from the SETUP, sent as fast as TCP takes it
    let shk: [u8; 32] = rnd[32..].try_into().unwrap();
    let (bport, _) = c.stream(&[
        ("type", 103u64.into()), ("streamConnectionID", 10300u64.into()), ("ct", 4u64.into()), ("audioFormat", 0x40_0000u64.into()),
        ("spf", 1024u64.into()), ("shk", plist::Value::Data(shk.to_vec())),
    ]);
    let mut d = TcpStream::connect((host.as_str(), bport)).expect("buffered data port");
    for (n, au) in aus.iter().enumerate() {
        let mut rtp = [0x80u8, 0, 0, 0, 0, 0, 0, 0, 0x16, 0, 0, 0];
        rtp[1..4].copy_from_slice(&(n as u32 + 70_000).to_be_bytes()[1..]);
        rtp[4..8].copy_from_slice(&(n as u32 * 1024).to_be_bytes());
        let nonce = (n as u64).to_le_bytes();
        let mut pkt = rtp.to_vec();
        pkt.extend_from_slice(&seal(&shk, nonce, &rtp[4..12], au));
        pkt.extend_from_slice(&nonce);
        d.write_all(&((pkt.len() + 2) as u16).to_be_bytes()).unwrap();
        d.write_all(&pkt).unwrap();
    }
    c.req("SET_PARAMETER", "text/parameters", b"volume: -17.500000\r\n");
    let dmap = [&b"mlit"[..], &33u32.to_be_bytes(), b"minm", &10u32.to_be_bytes(), b"Test Track", b"asar", &7u32.to_be_bytes(), b"Nobody!"].concat();
    c.req("SET_PARAMETER", "application/x-dmap-tagged", &dmap);
    c.req("SET_PARAMETER", "image/jpeg", &[0xFF, 0xD8, 0xFF, 0xE0, 0, 2, 0xFF, 0xD9]);
    c.req("POST /feedback", "application/octet-stream", b"");
    let sent = video.join().expect("video thread");
    std::thread::sleep(Duration::from_millis(500));
    c.req("TEARDOWN", "application/x-apple-binary-plist", b"");
    println!("done: {sent} video frames, {} audio AUs on type 96 (x3 each) and {} on type 103", aus.len(), aus.len());
}
