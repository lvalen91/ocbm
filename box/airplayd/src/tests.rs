//! Loopback tests of the whole receiver: a controller built from the pairing crate's own
//! `srp::client_proof` and the same `rtsp::control` codec with the keys swapped drives the daemon
//! over TCP; three local listeners stand in for ocbmd's seams; the "host" side decrypts what came
//! out of them with the rules of docs/ocbm/02 §4.6. No crypto is written here.

use super::*;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use ocbm_proto::seam::{MetaReader, SeamReader};
use pairing::srp::client_proof;
use std::sync::Mutex;

struct FakeChip;
impl MfiSigner for FakeChip {
    fn copy_certificate(&mut self) -> std::io::Result<Vec<u8>> {
        Ok(vec![0x30; 945])
    }
    fn create_signature(&mut self, _: &[u8]) -> std::io::Result<Vec<u8>> {
        Ok(vec![0x5A; 128])
    }
}

/// A stand-in for one ocbmd seam: accepts producers, keeps every byte, counts connections.
#[derive(Clone)]
struct Seam {
    addr: String,
    bytes: Arc<Mutex<Vec<u8>>>,
    conns: Arc<AtomicU32>,
    kick: Arc<Mutex<Option<TcpStream>>>,
}

impl Seam {
    fn start() -> Seam {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let s = Seam {
            addr: l.local_addr().unwrap().to_string(),
            bytes: Arc::default(),
            conns: Arc::default(),
            kick: Arc::default(),
        };
        let me = s.clone();
        std::thread::spawn(move || {
            for mut c in l.incoming().flatten() {
                me.conns.fetch_add(1, Ordering::SeqCst);
                *me.kick.lock().unwrap() = Some(c.try_clone().unwrap());
                let me = me.clone();
                std::thread::spawn(move || {
                    let mut b = [0u8; 65536];
                    while let Ok(n) = c.read(&mut b) {
                        if n == 0 {
                            break;
                        }
                        me.bytes.lock().unwrap().extend_from_slice(&b[..n]);
                    }
                });
            }
        });
        s
    }

    /// Every whole seam message so far.
    fn msgs(&self) -> Vec<(u8, Vec<u8>)> {
        let mut r = SeamReader::new(16 << 20);
        r.push(&self.bytes.lock().unwrap(), false);
        std::iter::from_fn(|| r.next_msg()).collect()
    }

    fn events(&self) -> Vec<String> {
        let mut r = MetaReader::new();
        r.push(&self.bytes.lock().unwrap(), false);
        std::iter::from_fn(|| r.next_msg()).filter(|m| m.0 == p::META_JSON).map(|m| String::from_utf8(m.1).unwrap()).collect()
    }
}

fn wait(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..400 {
        if ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

struct Rig {
    addr: SocketAddr,
    video: Seam,
    audio: Seam,
    meta: Seam,
    state: std::path::PathBuf,
}

fn rig() -> Rig {
    static N: AtomicU32 = AtomicU32::new(0);
    let (video, audio, meta) = (Seam::start(), Seam::start(), Seam::start());
    let state = std::env::temp_dir().join(format!("airplayd-test-{}-{}.state", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let tunnel = Arc::new(Tunnel::new(&video.addr, &audio.addr, &meta.addr, Some(state.clone())));
    tunnel.publish(None);
    let cfg = Cfg::new("Test Receiver".into(), (1280, 720, 60), false, &[7u8; 54]);
    let ctx = Arc::new(Ctx { cfg, tunnel, signer: || Box::new(FakeChip) });
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    accept_loop(l, ctx);
    Rig { addr, video, audio, meta, state }
}

/// One request/response; encrypts and decrypts through `chan` when it is set.
fn rt(s: &mut TcpStream, chan: &mut Option<ControlChannel>, head: &str, body: &[u8]) -> (u16, Vec<u8>) {
    let mut req = format!("{head}\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
    req.extend_from_slice(body);
    let wire = match chan.as_mut() {
        Some(c) => c.encrypt_frame(&req),
        None => req,
    };
    s.write_all(&wire).unwrap();
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
        let n = s.read(&mut buf).unwrap();
        assert!(n > 0, "server closed mid-response");
        raw.extend_from_slice(&buf[..n]);
    }
}

fn tlv_of(items: &[(u8, Vec<u8>)], t: u8) -> Option<&[u8]> {
    items.iter().find(|(k, _)| *k == t).map(|(_, v)| v.as_slice())
}

/// Transient pair-setup M1..M4, then the controller's view of the control channel. Returns the
/// pairing secret (the SRP session key) too: the "phone" encrypts its streams from it.
fn pair(s: &mut TcpStream) -> (Vec<u8>, Option<ControlChannel>) {
    let mut plain = None;
    let m1 = tlv::encode(&[(tlv_type::METHOD, &[0]), (tlv_type::STATE, &[1]), (tlv_type::FLAGS, &[0x10])]);
    let (st, m2) = rt(s, &mut plain, "POST /pair-setup RTSP/1.0\r\nCSeq: 1\r\nX-Apple-HKP: 4", &m1);
    assert_eq!(st, 200);
    let m2 = tlv::decode(&m2).unwrap();
    let (a_pub, proof, key) =
        client_proof(b"Pair-Setup", b"3939", tlv_of(&m2, tlv_type::SALT).unwrap(), &[7u8; 32], tlv_of(&m2, tlv_type::PUBLIC_KEY).unwrap());
    let m3 = tlv::encode(&[(tlv_type::STATE, &[3]), (tlv_type::PUBLIC_KEY, &a_pub), (tlv_type::PROOF, &proof)]);
    let (st, m4) = rt(s, &mut plain, "POST /pair-setup RTSP/1.0\r\nCSeq: 2", &m3);
    assert_eq!(st, 200);
    let m4 = tlv::decode(&m4).unwrap();
    assert!(tlv_of(&m4, tlv_type::ERROR).is_none(), "server accepted the proof");
    let k = derive_control_keys(&key);
    (key, Some(ControlChannel::new(ControlKeys { read: k.write, write: k.read })))
}

fn setup(s: &mut TcpStream, chan: &mut Option<ControlChannel>, d: plist::Dictionary) -> plist::Dictionary {
    let (st, resp) = rt(s, chan, "SETUP rtsp://x/1 RTSP/1.0\r\nCSeq: 9\r\nContent-Type: application/x-apple-binary-plist", &to_bplist(d));
    assert_eq!(st, 200);
    plist::Value::from_reader(std::io::Cursor::new(resp)).unwrap().into_dictionary().unwrap()
}

fn stream_req(fields: &[(&str, plist::Value)]) -> plist::Dictionary {
    let mut sd = plist::Dictionary::new();
    for (k, v) in fields {
        sd.insert((*k).into(), v.clone());
    }
    let mut d = plist::Dictionary::new();
    d.insert("streams".into(), plist::Value::Array(vec![plist::Value::Dictionary(sd)]));
    d
}

fn reply_port(d: &plist::Dictionary, key: &str) -> u16 {
    d.get("streams").unwrap().as_array().unwrap()[0].as_dictionary().unwrap().get(key).and_then(|v| v.as_unsigned_integer()).unwrap() as u16
}

fn session(s: &mut TcpStream, chan: &mut Option<ControlChannel>, ptp: bool) -> plist::Dictionary {
    let mut d = plist::Dictionary::new();
    d.insert("timingProtocol".into(), plist::Value::String(if ptp { "PTP" } else { "NTP" }.into()));
    d.insert("name".into(), plist::Value::String("Test \"Phone\"".into()));
    d.insert("isScreenMirroringSession".into(), plist::Value::Boolean(!ptp));
    setup(s, chan, d)
}

fn seal(key: &[u8; 32], nonce8: [u8; 8], aad: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&nonce8);
    ChaCha20Poly1305::new(Key::from_slice(key)).encrypt(Nonce::from_slice(&n), Payload { msg, aad }).unwrap()
}

fn open(key: &[u8], nonce8: &[u8], aad: &[u8], msg: &[u8]) -> Option<Vec<u8>> {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(nonce8);
    ChaCha20Poly1305::new(Key::from_slice(key)).decrypt(Nonce::from_slice(&n), Payload { msg, aad }).ok()
}

/// An audio packet as the phone sends it: `[RTP 12][ciphertext][tag 16][nonce 8]`, AAD = RTP 4..12.
fn audio_pkt(key: &[u8; 32], rtp: [u8; 12], nonce: u64, au: &[u8]) -> Vec<u8> {
    let mut pkt = rtp.to_vec();
    pkt.extend_from_slice(&seal(key, nonce.to_le_bytes(), &rtp[4..12], au));
    pkt.extend_from_slice(&nonce.to_le_bytes());
    pkt
}

/// The host's side of an audio `SEAM_PKT` body: `[scid 8][packet]` -> `(scid, access unit)`.
fn open_audio(key: &[u8], body: &[u8]) -> (u64, Vec<u8>) {
    let (scid, pkt) = (u64::from_le_bytes(body[..8].try_into().unwrap()), &body[8..]);
    let n = pkt.len();
    (scid, open(key, &pkt[n - 8..], &pkt[4..12], &pkt[12..n - 8]).expect("audio packet authenticates"))
}

fn state_of(r: &Rig) -> String {
    std::fs::read_to_string(&r.state).unwrap_or_default().replace('\n', " ").trim().to_string()
}

#[test]
fn mirroring_session_is_tunnelled_still_encrypted_with_the_keys_the_host_needs() {
    let r = rig();
    assert_eq!(state_of(&r), "session=1 end=0"); // waiting
    let mut s = TcpStream::connect(r.addr).unwrap();
    let (secret, mut chan) = pair(&mut s);

    // MFi auth-setup: M1 = 0x01 + a Curve25519 public key; M2 = pub | [len][cert] | [len][signature].
    let mut m1 = vec![1u8];
    m1.extend_from_slice(&[9u8; 32]);
    let (st, m2) = rt(&mut s, &mut chan, "POST /auth-setup RTSP/1.0\r\nCSeq: 3", &m1);
    assert_eq!((st, m2.len()), (200, 32 + 4 + 945 + 4 + 128));

    let sess = session(&mut s, &mut chan, false);
    assert!(sess.get("eventPort").and_then(|v| v.as_unsigned_integer()).unwrap() > 0);
    assert!(sess.get("timingPort").and_then(|v| v.as_unsigned_integer()).unwrap() > 0);
    let _event = TcpStream::connect(("127.0.0.1", sess.get("eventPort").unwrap().as_unsigned_integer().unwrap() as u16)).unwrap();
    wait("connected", || state_of(&r) == "session=2 end=0");

    // ---- type 110. The scid arrives as a NEGATIVE plist integer: the salt uses its unsigned form.
    let scid_signed = -1234567890123456789i64;
    let scid = scid_signed as u64;
    let rep = setup(&mut s, &mut chan, stream_req(&[("type", 110u64.into()), ("streamConnectionID", scid_signed.into()), ("latencyMs", 100u64.into())]));
    let vkey = stream_key(&secret, scid);
    let mut data = TcpStream::connect(("127.0.0.1", reply_port(&rep, "dataPort"))).unwrap();
    wait("streaming", || state_of(&r) == "session=3 end=0");
    let frame = |op: u8, flags: u8, body: &[u8]| {
        let mut h = [0u8; 128];
        h[0..4].copy_from_slice(&(body.len() as u32).to_le_bytes());
        h[4] = op;
        h[5] = flags;
        h[8..16].copy_from_slice(&0x0000_0010_8000_0000u64.to_le_bytes());
        h
    };
    let avcc = [1u8, 100, 0, 31, 0xFF, 0xE1, 0, 2, 0x67, 0x64, 1, 0, 2, 0x68, 0xEE];
    data.write_all(&frame(1, 0, &avcc)).unwrap();
    data.write_all(&avcc).unwrap();
    let mut sent = Vec::new();
    for n in 0..5u64 {
        if n == 3 {
            // opcode 5: plaintext statistics, NOT counted in the nonce
            data.write_all(&frame(5, 0, b"bplist00stats")).unwrap();
            data.write_all(b"bplist00stats").unwrap();
        }
        let nal = vec![n as u8 + 1; 300 + n as usize * 1000];
        let hdr = frame(0, if n == 0 { 0x10 } else { 0 }, &vec![0u8; nal.len() + 16]);
        let ct = seal(&vkey, n.to_le_bytes(), &hdr, &nal);
        // written in two pieces: the forwarder must reassemble across reads
        data.write_all(&hdr[..50]).unwrap();
        data.write_all(&hdr[50..]).unwrap();
        data.write_all(&ct).unwrap();
        sent.push(nal);
    }
    wait("7 video messages", || r.video.msgs().iter().filter(|m| m.0 == 0x01).count() == 7);
    let msgs = r.video.msgs();
    // The key message comes first and carries the HKDF output for THIS scid, never the pairing secret.
    assert_eq!(msgs[0].0, 0x00);
    assert_eq!((&msgs[0].1[..32], &msgs[0].1[32..]), (&vkey[..], &scid.to_le_bytes()[..]));
    assert!(!r.video.bytes.lock().unwrap().windows(secret.len()).any(|w| w == secret.as_slice()));
    let frames: Vec<&(u8, Vec<u8>)> = msgs.iter().filter(|m| m.0 == 0x01).collect();
    let mut got = Vec::new();
    let mut ops = Vec::new();
    for f in &frames {
        let (seq, hdr, body) = (u64::from_le_bytes(f.1[..8].try_into().unwrap()), &f.1[8..136], &f.1[136..]);
        ops.push((hdr[4], seq));
        match hdr[4] {
            0 => got.push(open(&msgs[0].1[..32], &seq.to_le_bytes(), hdr, body).expect("video frame authenticates with the wire seq")),
            1 => assert_eq!(body, avcc),
            _ => assert_eq!(body, b"bplist00stats"),
        }
    }
    assert_eq!(got, sent);
    // seq = opcode-0 frames before this message: config and statistics carry it without advancing it
    assert_eq!(ops, [(1, 0), (0, 0), (0, 1), (0, 2), (5, 3), (0, 3), (0, 4)]);

    // ---- type 96, every packet three times
    let rep = setup(&mut s, &mut chan, stream_req(&[("type", 96u64.into()), ("streamConnectionID", 77u64.into()), ("ct", 4u64.into()), ("sr", 44100u64.into()), ("spf", 1024u64.into())]));
    let akey = stream_key(&secret, 77);
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    let dst = ("127.0.0.1", reply_port(&rep, "dataPort"));
    assert!(reply_port(&rep, "controlPort") > 0);
    let rtp = |seq: u16| {
        let mut h = [0x80u8, 96, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        h[2..4].copy_from_slice(&seq.to_be_bytes());
        h[4..8].copy_from_slice(&(u32::from(seq) * 1024).to_be_bytes());
        h
    };
    // across the 16-bit sequence wrap
    let seqs: Vec<u16> = (0..12).map(|i| 65530u16.wrapping_add(i)).collect();
    for &seq in &seqs {
        let pkt = audio_pkt(&akey, rtp(seq), u64::from(seq) + 1, &[seq as u8; 40]);
        for _ in 0..3 {
            udp.send_to(&pkt, dst).unwrap();
        }
    }
    wait("12 unique audio packets", || r.audio.msgs().iter().filter(|m| m.0 == p::SEAM_PKT).count() >= 12);
    std::thread::sleep(Duration::from_millis(100));
    let am = r.audio.msgs();
    assert_eq!(am[0].0, p::SEAM_KEY);
    assert_eq!((&am[0].1[..32], &am[0].1[32..]), (&akey[..], &77u64.to_le_bytes()[..]));
    // SEAM_FORMAT: [scid][codec 1 = AAC-LC][rate LE][ch][bits][audio_type]
    assert_eq!((am[1].0, &am[1].1[..]), (p::SEAM_FORMAT, &[77, 0, 0, 0, 0, 0, 0, 0, 1, 0x44, 0xAC, 0, 0, 2, 0, 0][..]));
    let pk: Vec<&(u8, Vec<u8>)> = am.iter().filter(|m| m.0 == p::SEAM_PKT).collect();
    assert_eq!(pk.len(), 12, "three copies in, one out");
    for (m, seq) in pk.iter().zip(&seqs) {
        assert_eq!(open_audio(&akey, &m.1), (77, vec![*seq as u8; 40]));
    }

    // ---- the phone drops the audio stream and sets a new one up mid-session: a new scid, a new key
    let mut td = stream_req(&[("type", 96u64.into()), ("streamID", 0u64.into())]);
    td.insert("x".into(), plist::Value::Boolean(true));
    let (st, _) = rt(&mut s, &mut chan, "TEARDOWN rtsp://x/1 RTSP/1.0\r\nCSeq: 20\r\nContent-Type: application/x-apple-binary-plist", &to_bplist(td));
    assert_eq!(st, 200);
    assert_eq!(state_of(&r), "session=3 end=0", "the screen stream is still up");
    let rep = setup(&mut s, &mut chan, stream_req(&[("type", 96u64.into()), ("streamConnectionID", 78u64.into()), ("ct", 4u64.into()), ("sr", 44100u64.into())]));
    let akey2 = stream_key(&secret, 78);
    assert_ne!(akey, akey2);
    udp.send_to(&audio_pkt(&akey2, rtp(1), 9, b"second stream"), ("127.0.0.1", reply_port(&rep, "dataPort"))).unwrap();
    wait("the second stream's packet", || r.audio.msgs().iter().filter(|m| m.0 == p::SEAM_PKT).count() == 13);
    let am = r.audio.msgs();
    let k2 = am.iter().rfind(|m| m.0 == p::SEAM_KEY).unwrap();
    assert_eq!((&k2.1[..32], &k2.1[32..]), (&akey2[..], &78u64.to_le_bytes()[..]));
    assert_eq!(open_audio(&akey2, &am.last().unwrap().1), (78, b"second stream".to_vec()));

    // ---- volume, then the user stops: session TEARDOWN, control closed
    let (st, vol) = rt(&mut s, &mut chan, "GET_PARAMETER rtsp://x/1 RTSP/1.0\r\nCSeq: 30\r\nContent-Type: text/parameters", b"volume\r\n");
    assert_eq!((st, vol.as_slice()), (200, &b"volume: -20.000000\r\n"[..]));
    rt(&mut s, &mut chan, "SET_PARAMETER rtsp://x/1 RTSP/1.0\r\nCSeq: 31\r\nContent-Type: text/parameters", b"volume: -12.500000\r\n");
    let (st, _) = rt(&mut s, &mut chan, "TEARDOWN rtsp://x/1 RTSP/1.0\r\nCSeq: 32", b"");
    assert_eq!(st, 200);
    drop(s);
    wait("waiting again, ended by TEARDOWN", || state_of(&r) == "session=1 end=1");
    wait("the last session event", || r.meta.events().last().is_some_and(|e| e.contains("\"state\":\"waiting\"")));
    let ev = r.meta.events();
    let has = |needle: &str| ev.iter().any(|e| e.contains(needle));
    assert!(ev.iter().all(|e| e.starts_with("{\"kind\":\"airplay\",\"ev\":\"")));
    assert!(has("\"ev\":\"sender\",\"name\":\"Test \\\"Phone\\\"\""));
    assert!(has(&format!("\"ev\":\"stream\",\"state\":\"start\",\"lane\":\"video\",\"scid\":{scid},\"setup\":{{")));
    assert!(has("\"lane\":\"audio\",\"scid\":77,\"setup\":{") && has("\"ct\":4") && has("\"spf\":1024"));
    assert!(has("\"state\":\"stop\",\"type\":96,\"scid\":77,\"reason\":\"teardown\",\"messages\":12"));
    assert!(has("\"state\":\"stop\",\"type\":110") && has("\"reason\":\"session teardown\""));
    assert!(has("\"ev\":\"volume\",\"db\":-12.5"));
    assert!(has("\"ev\":\"session\",\"state\":\"streaming\"") && has("\"state\":\"waiting\",\"reason\":\"teardown\""));
    let _ = std::fs::remove_file(&r.state);
}

#[test]
fn buffered_audio_session_uses_shk_and_the_strict_reply() {
    let r = rig();
    let mut s = TcpStream::connect(r.addr).unwrap();
    let (secret, mut chan) = pair(&mut s);
    let sess = session(&mut s, &mut chan, true);
    // PTP session: no NTP port, the receiver names itself as a timing peer
    assert_eq!(sess.get("timingPort").and_then(|v| v.as_unsigned_integer()), Some(0));
    assert!(sess.get("timingPeerInfo").and_then(|v| v.as_dictionary()).is_some_and(|d| d.contains_key("Addresses")));
    let shk = [0xC3u8; 32];
    let rep = setup(
        &mut s, &mut chan,
        stream_req(&[("type", 103u64.into()), ("streamConnectionID", 5u64.into()), ("ct", 4u64.into()), ("audioFormat", 0x40_0000u64.into()), ("shk", plist::Value::Data(shk.to_vec()))]),
    );
    let st = rep.get("streams").unwrap().as_array().unwrap()[0].as_dictionary().unwrap();
    assert_eq!(st.get("audioBufferSize").and_then(|v| v.as_unsigned_integer()), Some(8 * 1024 * 1024));
    assert!(reply_port(&rep, "controlPort") > 0);
    let mut data = TcpStream::connect(("127.0.0.1", reply_port(&rep, "dataPort"))).unwrap();
    let mut wire = Vec::new();
    for n in 0..40u32 {
        // 24-bit sequence in RTP bytes 1..4, SSRC 0x16000000
        let mut rtp = [0x80u8, 0, 0, 0, 0, 0, 0, 0, 0x16, 0, 0, 0];
        rtp[1..4].copy_from_slice(&(n + 70000).to_be_bytes()[1..]);
        rtp[4..8].copy_from_slice(&(n * 1024).to_be_bytes());
        let pkt = audio_pkt(&shk, rtp, u64::from(n), &vec![n as u8; if n == 7 { 0 } else { 200 }]);
        wire.extend_from_slice(&((pkt.len() + 2) as u16).to_be_bytes());
        wire.extend_from_slice(&pkt);
    }
    for c in wire.chunks(333) {
        data.write_all(c).unwrap();
    }
    wait("40 buffered packets", || r.audio.msgs().iter().filter(|m| m.0 == p::SEAM_PKT).count() == 40);
    let am = r.audio.msgs();
    // the key on the lane is shk itself, not something derived from the pairing secret
    assert_eq!((am[0].0, &am[0].1[..32], &am[0].1[32..]), (p::SEAM_KEY, &shk[..], &5u64.to_le_bytes()[..]));
    assert_ne!(stream_key(&secret, 5), shk);
    for (n, m) in am.iter().filter(|m| m.0 == p::SEAM_PKT).enumerate() {
        assert_eq!(m.1[8], 0x80, "the u16 length prefix is stripped: the packet starts at its RTP header");
        assert_eq!(open_audio(&shk, &m.1), (5, vec![n as u8; if n == 7 { 0 } else { 200 }]));
    }
    // metadata on the control channel
    let dmap = [&b"mlit"[..], &26u32.to_be_bytes(), b"minm", &5u32.to_be_bytes(), b"Title", b"asar", &5u32.to_be_bytes(), b"Artis"].concat();
    rt(&mut s, &mut chan, "SET_PARAMETER rtsp://x/1 RTSP/1.0\r\nCSeq: 40\r\nContent-Type: application/x-dmap-tagged", &dmap);
    rt(&mut s, &mut chan, "SET_PARAMETER rtsp://x/1 RTSP/1.0\r\nCSeq: 41\r\nContent-Type: image/jpeg", &[0xFF, 0xD8, 0xFF, 0xD9]);
    rt(&mut s, &mut chan, "SET_PARAMETER rtsp://x/1 RTSP/1.0\r\nCSeq: 42\r\nContent-Type: text/parameters", b"progress: 100/200/300\r\n");
    let mut anchor = plist::Dictionary::new();
    anchor.insert("rate".into(), plist::Value::Integer(1u64.into()));
    anchor.insert("networkTimeFrac".into(), plist::Value::Integer(u64::MAX.into()));
    rt(&mut s, &mut chan, "SETRATEANCHORTIME rtsp://x/1 RTSP/1.0\r\nCSeq: 43\r\nContent-Type: application/x-apple-binary-plist", &to_bplist(anchor));
    // the control connection drops without a TEARDOWN (the phone left Wi-Fi)
    drop(s);
    wait("ended as peer-closed", || state_of(&r) == "session=1 end=2");
    let ev = r.meta.events();
    let has = |needle: &str| ev.iter().any(|e| e.contains(needle));
    assert!(has("\"ev\":\"track\",\"title\":\"Title\",\"artist\":\"Artis\""));
    assert!(has("\"ev\":\"artwork\",\"bytes\":4") && has("\"ev\":\"progress\",\"start\":100,\"current\":200,\"end\":300"));
    assert!(has("\"ev\":\"anchor\"") && has("\"networkTimeFrac\":18446744073709551615") && has("\"rate\":1"));
    assert!(has("\"ev\":\"sender\"") && has("\"timing\":\"PTP\""));
    // key material never rides the event lane: the stream event lists the SETUP's scalars only
    assert!(has("\"audioFormat\":4194304") && !has("shk"));
    let mut mr = MetaReader::new();
    mr.push(&r.meta.bytes.lock().unwrap(), false);
    let art: Vec<(u8, Vec<u8>)> = std::iter::from_fn(|| mr.next_msg()).filter(|m| m.0 == p::META_ARTWORK).collect();
    assert_eq!(art, [(p::META_ARTWORK, vec![0, 0xFF, 0xD8, 0xFF, 0xD9])]);
    let _ = std::fs::remove_file(&r.state);
}

#[test]
fn fairplay_non_transient_pairing_and_unpaired_setup_are_refused() {
    let r = rig();
    let mut s = TcpStream::connect(r.addr).unwrap();
    let mut plain = None;
    assert_eq!(rt(&mut s, &mut plain, "POST /fp-setup RTSP/1.0\r\nCSeq: 1", &[0x46, 0x50, 0x4c, 0x59]).0, 403);
    assert_eq!(rt(&mut s, &mut plain, "POST /fp-setup2 RTSP/1.0\r\nCSeq: 2", b"x").0, 403);
    assert_eq!(rt(&mut s, &mut plain, "POST /pair-verify RTSP/1.0\r\nCSeq: 3", &tlv::encode(&[(tlv_type::STATE, &[1])])).0, 470);
    // pair-setup without the transient flag: an error TLV, no SRP exchange started
    let (st, m2) = rt(&mut s, &mut plain, "POST /pair-setup RTSP/1.0\r\nCSeq: 4", &tlv::encode(&[(tlv_type::METHOD, &[0]), (tlv_type::STATE, &[1])]));
    assert_eq!(st, 200);
    assert!(tlv_of(&tlv::decode(&m2).unwrap(), tlv_type::ERROR).is_some());
    // SETUP before pairing has no secret to key a stream from
    let d = stream_req(&[("type", 110u64.into()), ("streamConnectionID", 1u64.into())]);
    assert_eq!(rt(&mut s, &mut plain, "SETUP rtsp://x/1 RTSP/1.0\r\nCSeq: 5", &to_bplist(d)).0, 400);
    // /info in the clear carries the advertised set and the display from the mode request
    let mut q = plist::Dictionary::new();
    q.insert("qualifier".into(), plist::Value::Array(vec![plist::Value::String("txtAirPlay".into())]));
    let (st, info) = rt(&mut s, &mut plain, "GET /info RTSP/1.0\r\nCSeq: 6", &to_bplist(q));
    assert_eq!(st, 200);
    let info = plist::Value::from_reader(std::io::Cursor::new(info)).unwrap().into_dictionary().unwrap();
    assert_eq!(info.get("features").and_then(|v| v.as_unsigned_integer()), Some(0x1_C340_445F_8A80));
    assert_eq!(info.get("name").and_then(|v| v.as_string()), Some("Test Receiver"));
    let disp = info.get("displays").unwrap().as_array().unwrap()[0].as_dictionary().unwrap();
    let n = |k: &str| disp.get(k).and_then(|v| v.as_unsigned_integer()).unwrap();
    assert_eq!((n("widthPixels"), n("heightPixels"), n("maxFPS")), (1280, 720, 60));
    let txt = String::from_utf8_lossy(info.get("txtAirPlay").unwrap().as_data().unwrap()).to_string();
    assert!(txt.contains("features=0x445F8A80,0x1C340") && txt.contains("srcvers=366.0") && txt.contains("pk="));
    assert_eq!(state_of(&r), "session=1 end=0", "none of this is a session");
    let _ = std::fs::remove_file(&r.state);
}

#[test]
fn advertised_feature_set_is_the_measured_one() {
    let bits: Vec<u32> = (0..64).filter(|n| FEATURES >> n & 1 == 1).collect();
    assert_eq!(bits, [7, 9, 11, 15, 16, 17, 18, 19, 20, 22, 26, 30, 38, 40, 41, 46, 47, 48]);
    assert_eq!(FEATURES >> 14 & 1, 0, "FairPlay is never advertised");
    assert_eq!(FEATURES & FEATURE_SCREEN_MULTI_CODEC, 0, "bit 42 only on request");
    let c = Cfg::new("n".into(), p::AIRPLAY_DEFAULT_DISPLAY, false, &[3u8; 54]);
    assert_eq!(c.features_txt(), "0x445F8A80,0x1C340");
    let txt: HashMap<String, String> = c.airplay_txt().into_iter().collect();
    assert_eq!(txt["deviceid"], "02:03:03:03:03:03"); // locally administered, unicast
    assert_eq!((txt["flags"].as_str(), txt["protovers"].as_str(), txt["vv"].as_str(), txt["acl"].as_str()), ("0x4", "1.1", "2", "0"));
    assert_eq!(txt["pk"].len(), 64);
    assert_eq!(txt["fex"], "gIpfREDDAQA"); // base64 of the 8 feature bytes, little-endian, unpadded
}

#[test]
fn identity_is_created_once_and_read_back() {
    let path = std::env::temp_dir().join(format!("airplayd-test-ident-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let a = identity_material(Some(&path));
    assert_eq!(identity_material(Some(&path)), a);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_ne!(identity_material(None), a);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_seam_that_reconnects_is_told_every_live_key_and_config_first() {
    let seam = Seam::start();
    let lane = tunnel::Lane::new(&seam.addr, Some(Duration::from_secs(2)));
    let key = |b: u8, scid: u64| p::seam::key_msg(&[b; 32], scid).to_vec();
    assert!(lane.announce(1, tunnel::PRE_KEY, key(1, 1)));
    assert!(lane.announce(2, tunnel::PRE_KEY, key(2, 2)));
    assert!(lane.announce(2, tunnel::PRE_KEY, key(3, 2)), "a re-announce replaces the stream's key");
    lane.forget(1);
    wait("3 key messages", || seam.msgs().len() == 3);
    // ocbmd restarts (or replaces the producer): the old connection dies
    seam.kick.lock().unwrap().take().unwrap().shutdown(std::net::Shutdown::Both).unwrap();
    seam.bytes.lock().unwrap().clear();
    let pkt = [p::seam::audio_pkt_prefix(2, 3).as_slice(), &[1, 2, 3]].concat();
    wait("the lane notices and reconnects", || lane.send(&[&pkt]) && seam.conns.load(Ordering::SeqCst) == 2);
    wait("preamble + packet", || seam.msgs().len() >= 2);
    let m = seam.msgs();
    assert_eq!((m[0].0, m[0].1[0], &m[0].1[32..]), (p::SEAM_KEY, 3, &2u64.to_le_bytes()[..]), "the live stream's current key, first");
    assert!(m.iter().all(|x| !(x.0 == p::SEAM_KEY && x.1[32..] == 1u64.to_le_bytes())), "a forgotten stream is not replayed");
    assert_eq!(m.last().unwrap().0, p::SEAM_PKT);
}

#[test]
fn dedup_forwards_the_first_copy_of_each_sequence_number_across_the_wrap() {
    let mut d = tunnel::Dedup::default();
    let mut out = Vec::new();
    for seq in (65500u32..65700).map(|s| s as u16) {
        for _ in 0..3 {
            if d.fresh(seq) {
                out.push(seq);
            }
        }
        // a late copy of an older packet, as redundancy delivers it
        assert!(!d.fresh(seq.wrapping_sub(1)) || seq == 65500);
    }
    assert_eq!(out.len(), 200);
    assert!(d.fresh(0xFFFF - 1000), "sequence numbers outside the window are new again");
}

#[test]
fn key_material_is_redacted_in_log_text() {
    let secret = [0xABu8; 64];
    let r = redact(&secret);
    assert_eq!(r, "64 B abababab..");
    assert!(!r.contains(&hex(&secret[..5])));
}

fn info_of(c: &Cfg) -> plist::Dictionary {
    plist::Value::from_reader(std::io::Cursor::new(build_info(c, true))).unwrap().into_dictionary().unwrap()
}

/// `AIRPLAY_FLAG_HEVC` adds feature bit 42 and nothing else, everywhere the features are advertised
/// (TXT `features` and `fex`, `/info` `features` and its `txtAirPlay`); without it nothing changes.
#[test]
fn hevc_flag_advertises_bit_42_and_only_when_requested() {
    let plain = Cfg::new("n".into(), (3840, 2160, 60), false, &[3u8; 54]);
    let hevc = Cfg::new("n".into(), (3840, 2160, 60), true, &[3u8; 54]);
    assert_eq!(hevc.features ^ plain.features, 1 << 42);
    assert_eq!(plain.features, FEATURES);
    let txt = |c: &Cfg| c.airplay_txt().into_iter().collect::<HashMap<String, String>>();
    assert_eq!(txt(&hevc)["features"], "0x445F8A80,0x1C740"); // the probe's verified H7 mask
    assert_eq!(txt(&plain)["features"], "0x445F8A80,0x1C340");
    assert_eq!(txt(&hevc)["fex"], "gIpfREDHAQA");
    assert_eq!(txt(&plain)["fex"], "gIpfREDDAQA");
    let f = |c: &Cfg| info_of(c).get("features").and_then(|v| v.as_unsigned_integer()).unwrap();
    assert_eq!((f(&hevc) >> 42 & 1, f(&plain) >> 42 & 1), (1, 0));
    let inner = String::from_utf8_lossy(info_of(&hevc).get("txtAirPlay").unwrap().as_data().unwrap()).to_string();
    assert!(inner.contains("features=0x445F8A80,0x1C740"), "{inner}");
}

/// The requested display lands in `/info` `displays[0]` with the probe's verified key set; the
/// measured default keeps its historical bytes.
#[test]
fn info_display_is_the_requested_one() {
    let disp = |c: &Cfg| info_of(c).get("displays").unwrap().as_array().unwrap()[0].as_dictionary().unwrap().clone();
    let k4 = disp(&Cfg::new("n".into(), (3840, 2160, 60), true, &[3u8; 54]));
    let n = |d: &plist::Dictionary, k: &str| d.get(k).and_then(|v| v.as_unsigned_integer());
    assert_eq!(
        (n(&k4, "widthPixels"), n(&k4, "heightPixels"), n(&k4, "width"), n(&k4, "height"), n(&k4, "maxFPS"), n(&k4, "features")),
        (Some(3840), Some(2160), Some(3840), Some(2160), Some(60), Some(14))
    );
    assert_eq!(k4.get("refreshRate").and_then(|v| v.as_real()), Some(1.0 / 60.0));
    assert!(k4.get("uuid").is_some() && k4.get("overscanned").is_some() && k4.get("rotation").is_some());
    let d = disp(&Cfg::new("n".into(), (1280, 720, 30), false, &[3u8; 54]));
    assert_eq!(d.get("refreshRate").and_then(|v| v.as_real()), Some(1.0 / 30.0));
    let d = disp(&Cfg::new("n".into(), p::AIRPLAY_DEFAULT_DISPLAY, false, &[3u8; 54]));
    assert_eq!((n(&d, "widthPixels"), n(&d, "heightPixels"), n(&d, "maxFPS")), (Some(1920), Some(1080), Some(30)));
    assert_eq!(d.get("refreshRate").and_then(|v| v.as_real()), Some(1.0 / 60.0), "the measured default is unchanged");
    // the CLI / cfg parser applies the request's ranges
    assert_eq!(parse_display("3840x2160@60"), Some((3840, 2160, 60)));
    assert_eq!(parse_display("1280x720"), Some((1280, 720, 30)));
    assert_eq!(parse_display("7681x2160@60"), None);
    assert_eq!(parse_display("3840x2160@121"), None);
    assert_eq!(parse_display("3840x2160@0"), None);
}
