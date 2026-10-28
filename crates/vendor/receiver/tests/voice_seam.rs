//! Plan v5.8 V1a — the :9003 voice seam's v2 framing, end to end through `spawn_audio`.
//!
//! Serialized (one global lock: the tests share `OCBM_FWD_ENC` and wall-clock timing), a fresh
//! `TestSink` per test, and ephemeral ports on both sides: the "app" is a `TcpListener` on
//! 127.0.0.1:0, and RTP goes from a local UDP socket to the stream's own data socket.
//!
//! Only with `voice-tag-v2`; without it this binary is empty (tag v1 is pinned by
//! `forward::tests::eld_tagging`).
#![cfg(feature = "voice-tag-v2")]

use std::io::Read;
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use receiver::forward::{
    adts_from_aac_lc, END_SESSION_RESET, END_SUPERSEDED, END_TEARDOWN, VOICE_V2_AU_FLAG, VOICE_V2_BEGIN,
    VOICE_V2_CONTROL, VOICE_V2_END,
};
use receiver::session::voice_seam_test::{end_stream, spawn, TestSink};
use receiver::session::AudioCodec;
use receiver::stream::{derive_stream_keys, encrypt_audio};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    let g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // The app runs the on-box decode path; v2 exists only there.
    std::env::set_var("OCBM_FWD_ENC", "0");
    g
}

const SHARED: [u8; 32] = [7u8; 32];

#[derive(Debug, Clone, PartialEq)]
enum Frame {
    Begin { scid: u64, stream_type: u8, atype: u8, codec: u8, latency: i32, begin_seq: u32 },
    End { scid: u64, reason: u8, frames: u32, drops: u32, seq_gaps: u32 },
    Au { scid: u64, rx_us: u64, seq: u16, ts: u32, stream_type: u8, atype: u8, au: Vec<u8> },
    V1 { atype: u8, body: Vec<u8> },
}

fn be64(b: &[u8]) -> u64 {
    u64::from_be_bytes(b[..8].try_into().unwrap())
}
fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().unwrap())
}

/// Read one frame, or None on EOF / timeout.
fn read_frame(s: &mut TcpStream, timeout: Duration) -> Option<Frame> {
    s.set_read_timeout(Some(timeout)).unwrap();
    let mut hdr = [0u8; 11];
    s.read_exact(&mut hdr).ok()?;
    let atype = hdr[6];
    let len = be32(&hdr[7..]) as usize;
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).ok()?;
    Some(if atype == VOICE_V2_CONTROL {
        match body[0] {
            VOICE_V2_BEGIN => {
                assert_eq!(len, 35, "BEGIN payload length");
                Frame::Begin {
                    scid: be64(&body[9..]),
                    stream_type: body[17],
                    atype: body[18],
                    codec: body[19],
                    latency: be32(&body[26..]) as i32,
                    begin_seq: be32(&body[31..]),
                }
            }
            VOICE_V2_END => {
                assert_eq!(len, 34, "END payload length");
                Frame::End {
                    scid: be64(&body[9..]),
                    reason: body[17],
                    frames: be32(&body[18..]),
                    drops: be32(&body[22..]),
                    seq_gaps: be32(&body[26..]),
                }
            }
            k => panic!("unknown control kind {k}"),
        }
    } else if atype & VOICE_V2_AU_FLAG != 0 {
        Frame::Au {
            scid: be64(&body[0..]),
            rx_us: be64(&body[8..]),
            seq: u16::from_be_bytes([body[16], body[17]]),
            ts: be32(&body[18..]),
            stream_type: body[22],
            atype: atype & 0x7F,
            au: body[23..].to_vec(),
        }
    } else {
        Frame::V1 { atype, body }
    })
}

fn accept(l: &TcpListener, within: Duration) -> Option<TcpStream> {
    l.set_nonblocking(true).unwrap();
    let end = Instant::now() + within;
    loop {
        match l.accept() {
            Ok((s, _)) => {
                s.set_nonblocking(false).unwrap();
                return Some(s);
            }
            Err(_) if Instant::now() < end => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => return None,
        }
    }
}

struct Stream {
    data_addr: std::net::SocketAddr,
    tx: UdpSocket,
    alive: Arc<AtomicBool>,
    cause: Arc<AtomicU8>,
    key: [u8; 32],
    seq: u16,
    ts: u32,
}

#[allow(clippy::too_many_arguments)]
fn start(
    sink: &'static TestSink,
    port: u16,
    scid: u64,
    stream_type: i64,
    codec: AudioCodec,
    is_media: bool,
    atype: u8,
    sr: u32,
    ch: u16,
) -> Stream {
    let data = UdpSocket::bind("127.0.0.1:0").unwrap();
    let data_addr = data.local_addr().unwrap();
    let alive = Arc::new(AtomicBool::new(true));
    let cause = Arc::new(AtomicU8::new(0));
    spawn(data, SHARED, scid, stream_type, codec, is_media, atype, sr, ch, alive.clone(), cause.clone(), 80, sink, port);
    Stream {
        data_addr,
        tx: UdpSocket::bind("127.0.0.1:0").unwrap(),
        alive,
        cause,
        key: derive_stream_keys(&SHARED, scid).output,
        seq: 1000,
        ts: 48_000,
    }
}

impl Stream {
    fn send(&mut self, au: &[u8]) {
        let mut h = [0u8; 12];
        h[0] = 0x80;
        h[1] = 0x60;
        h[2..4].copy_from_slice(&self.seq.to_be_bytes());
        h[4..8].copy_from_slice(&self.ts.to_be_bytes());
        h[8..12].copy_from_slice(&0x1234_5678u32.to_be_bytes());
        let pkt = encrypt_audio(&self.key, &h, au, &[self.seq as u8; 8]);
        self.tx.send_to(&pkt, self.data_addr).unwrap();
        self.seq = self.seq.wrapping_add(1);
        self.ts = self.ts.wrapping_add(480);
    }
    fn end(&self, cause: u8) {
        end_stream(&self.cause, &self.alive, cause);
    }
}

fn app() -> (TcpListener, u16) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    (l, p)
}

/// Read frames until `pred` matches one; returns everything read (including the match).
fn read_until(s: &mut TcpStream, within: Duration, pred: impl Fn(&Frame) -> bool) -> Vec<Frame> {
    let end = Instant::now() + within;
    let mut out = Vec::new();
    while Instant::now() < end {
        if let Some(f) = read_frame(s, Duration::from_millis(100)) {
            let hit = pred(&f);
            out.push(f);
            if hit {
                return out;
            }
        }
    }
    panic!("no matching frame within {within:?}; got {out:?}");
}

#[test]
fn begin_is_written_before_any_packet() {
    let _g = serial();
    let (l, port) = app();
    let sink = TestSink::leak();
    let s = start(sink, port, 0xA1, 100, AudioCodec::AacEld, false, 2, 16_000, 1);
    let mut c = accept(&l, Duration::from_secs(2)).expect("BEGIN connects on entry, before any RTP");
    let f = read_frame(&mut c, Duration::from_secs(1)).expect("a frame");
    assert_eq!(
        f,
        Frame::Begin { scid: 0xA1, stream_type: 100, atype: 2, codec: 2, latency: 80, begin_seq: 0 }
    );
    s.end(END_TEARDOWN);
    let end = read_until(&mut c, Duration::from_secs(1), |f| matches!(f, Frame::End { .. }));
    assert_eq!(
        end.last().unwrap(),
        &Frame::End { scid: 0xA1, reason: END_TEARDOWN, frames: 0, drops: 0, seq_gaps: 0 }
    );
}

#[test]
fn au_carries_scid_rx_time_seq_ts_and_stream_type() {
    let _g = serial();
    let (l, port) = app();
    let sink = TestSink::leak();
    let mut s = start(sink, port, 0xB2, 101, AudioCodec::AacEld, false, 4, 48_000, 2);
    let mut c = accept(&l, Duration::from_secs(2)).unwrap();
    assert!(matches!(read_frame(&mut c, Duration::from_secs(1)), Some(Frame::Begin { .. })));
    let t0 = receiver::session::voice_seam_test::clock_us();
    let (seq0, ts0) = (s.seq, s.ts);
    s.send(&[1, 2, 3]);
    s.send(&[4, 5]);
    let a = read_frame(&mut c, Duration::from_secs(1)).unwrap();
    let b = read_frame(&mut c, Duration::from_secs(1)).unwrap();
    match (a, b) {
        (
            Frame::Au { scid, rx_us, seq, ts, stream_type, atype, au },
            Frame::Au { rx_us: rx2, seq: seq2, ts: ts2, au: au2, .. },
        ) => {
            assert_eq!((scid, stream_type, atype), (0xB2, 101, 4));
            assert_eq!((seq, ts, au), (seq0, ts0, vec![1, 2, 3]));
            assert_eq!((seq2, ts2, au2), (seq0 + 1, ts0 + 480, vec![4, 5]));
            assert!(rx_us >= t0 && rx2 >= rx_us, "rx stamps on the voice clock, monotonic");
        }
        other => panic!("expected two v2 AUs, got {other:?}"),
    }
    s.end(END_TEARDOWN);
    let fr = read_until(&mut c, Duration::from_secs(1), |f| matches!(f, Frame::End { .. }));
    assert!(matches!(fr.last(), Some(Frame::End { frames: 2, drops: 0, seq_gaps: 0, .. })));
}

#[test]
fn seq_gaps_are_counted_in_end() {
    let _g = serial();
    let (l, port) = app();
    let sink = TestSink::leak();
    let mut s = start(sink, port, 0xB3, 100, AudioCodec::AacEld, false, 1, 16_000, 1);
    let mut c = accept(&l, Duration::from_secs(2)).unwrap();
    s.send(&[1]);
    s.seq = s.seq.wrapping_add(2); // two packets lost
    s.send(&[2]);
    read_until(&mut c, Duration::from_secs(1), |f| matches!(f, Frame::Au { au, .. } if au == &vec![2]));
    s.end(END_TEARDOWN);
    let fr = read_until(&mut c, Duration::from_secs(1), |f| matches!(f, Frame::End { .. }));
    assert!(matches!(fr.last(), Some(Frame::End { frames: 2, seq_gaps: 2, .. })), "{fr:?}");
}

#[test]
fn non_media_aac_lc_is_v2_tagged_not_raw_adts() {
    let _g = serial();
    let (l, port) = app();
    let sink = TestSink::leak();
    let mut s = start(sink, port, 0xC3, 100, AudioCodec::AacLc, false, 4, 44_100, 2);
    let mut c = accept(&l, Duration::from_secs(2)).unwrap();
    assert!(matches!(read_frame(&mut c, Duration::from_secs(1)), Some(Frame::Begin { codec: 1, .. })));
    s.send(&[9, 9, 9, 9]);
    match read_frame(&mut c, Duration::from_secs(1)).unwrap() {
        Frame::Au { au, atype, .. } => {
            assert_eq!(au, vec![9, 9, 9, 9], "the raw AU, no ADTS header");
            assert_eq!(atype, 4);
        }
        f => panic!("expected a v2 AU, got {f:?}"),
    }
    s.end(END_TEARDOWN);
}

/// Media streams are out of v2's scope: with the feature on, :9002 output is byte-identical to the
/// v1 path — ADTS for AAC-LC, no BEGIN, no END, no tag.
#[test]
fn media_output_is_byte_identical_with_the_feature_on() {
    let _g = serial();
    let (l, port) = app();
    let sink = TestSink::leak();
    let mut s = start(sink, port, 0xD4, 102, AudioCodec::AacLc, true, 0, 48_000, 2);
    // No BEGIN: a media stream connects only on its first AU.
    assert!(accept(&l, Duration::from_millis(300)).is_none(), "media must not connect for a BEGIN");
    let aus: [&[u8]; 3] = [&[0x11; 20], &[0x22; 7], &[0x33; 64]];
    for au in aus {
        s.send(au);
    }
    let mut c = accept(&l, Duration::from_secs(2)).unwrap();
    let want: Vec<u8> = aus.iter().flat_map(|a| adts_from_aac_lc(a, 48_000, 2).unwrap()).collect();
    let mut got = vec![0u8; want.len()];
    c.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    c.read_exact(&mut got).unwrap();
    assert_eq!(got, want);
    s.end(END_TEARDOWN);
    // Nothing follows: no END on the media seam.
    let mut extra = [0u8; 1];
    c.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
    assert!(c.read(&mut extra).map(|n| n == 0).unwrap_or(true), "no END on :9002");
}

#[test]
fn end_follows_the_flag_flip_within_one_packet() {
    let _g = serial();
    let (l, port) = app();
    let sink = TestSink::leak();
    let mut s = start(sink, port, 0xE5, 100, AudioCodec::AacEld, false, 2, 16_000, 1);
    let mut c = accept(&l, Duration::from_secs(2)).unwrap();
    for _ in 0..5 {
        s.send(&[1]);
    }
    read_until(&mut c, Duration::from_secs(1), |f| matches!(f, Frame::Au { seq, .. } if *seq == 1004));
    s.end(END_TEARDOWN);
    let flip = Instant::now();
    for _ in 0..5 {
        s.send(&[2]);
    }
    let fr = read_until(&mut c, Duration::from_secs(1), |f| matches!(f, Frame::End { .. }));
    let late = fr.iter().filter(|f| matches!(f, Frame::Au { .. })).count();
    assert!(late <= 1, "{late} AUs forwarded after the flip");
    match fr.last().unwrap() {
        Frame::End { reason, frames, .. } => {
            assert_eq!(*reason, END_TEARDOWN);
            assert_eq!(*frames as usize, 5 + late, "END.frames = AUs the app read");
        }
        _ => unreachable!(),
    }
    assert!(flip.elapsed() < Duration::from_millis(500));
}

#[test]
fn end_on_a_silent_stream_lands_within_the_50ms_poll() {
    let _g = serial();
    let (l, port) = app();
    let sink = TestSink::leak();
    let s = start(sink, port, 0xE6, 101, AudioCodec::AacEld, false, 4, 48_000, 2);
    let mut c = accept(&l, Duration::from_secs(2)).unwrap();
    assert!(matches!(read_frame(&mut c, Duration::from_secs(1)), Some(Frame::Begin { .. })));
    std::thread::sleep(Duration::from_millis(200));
    s.end(END_SUPERSEDED);
    let t = Instant::now();
    let f = read_frame(&mut c, Duration::from_secs(1)).expect("END");
    assert!(matches!(f, Frame::End { reason: END_SUPERSEDED, .. }), "{f:?}");
    assert!(t.elapsed() < Duration::from_millis(200), "END took {:?}", t.elapsed());
}

/// Two streams share one sink; the app drops the connection; on the next connection BOTH streams
/// precede their next AU with a re-sent BEGIN (begin_seq > 0).
#[test]
fn peer_reconnect_makes_both_streams_resend_begin() {
    let _g = serial();
    let (l, port) = app();
    let sink = TestSink::leak();
    let mut a = start(sink, port, 0xF1, 100, AudioCodec::AacEld, false, 1, 16_000, 1);
    let mut b = start(sink, port, 0xF2, 101, AudioCodec::AacEld, false, 4, 48_000, 2);
    let mut c1 = accept(&l, Duration::from_secs(2)).unwrap();
    let mut begins = std::collections::HashSet::new();
    while begins.len() < 2 {
        if let Some(Frame::Begin { scid, begin_seq: 0, .. }) = read_frame(&mut c1, Duration::from_secs(1)) {
            begins.insert(scid);
        }
    }
    let gen1 = sink.conn_gen();
    drop(c1); // the app side goes away
    // Keep both streams sending until the writers notice and reconnect.
    let mut c2 = None;
    let end = Instant::now() + Duration::from_secs(3);
    while c2.is_none() && Instant::now() < end {
        a.send(&[0xA]);
        b.send(&[0xB]);
        c2 = accept(&l, Duration::from_millis(20));
    }
    let mut c2 = c2.expect("a reconnect");
    assert!(sink.conn_gen() > gen1);
    let mut first: std::collections::HashMap<u64, Frame> = Default::default();
    let end = Instant::now() + Duration::from_secs(2);
    while first.len() < 2 && Instant::now() < end {
        a.send(&[0xA]);
        b.send(&[0xB]);
        while let Some(f) = read_frame(&mut c2, Duration::from_millis(30)) {
            let id = match &f {
                Frame::Begin { scid, .. } | Frame::Au { scid, .. } | Frame::End { scid, .. } => *scid,
                Frame::V1 { .. } => panic!("v1 frame on a v2 seam"),
            };
            first.entry(id).or_insert(f);
        }
    }
    for id in [0xF1u64, 0xF2] {
        match first.get(&id) {
            Some(Frame::Begin { begin_seq, .. }) => assert!(*begin_seq > 0, "scid {id:#x}: re-send must count"),
            other => panic!("scid {id:#x}: first frame on the new connection must be BEGIN, got {other:?}"),
        }
    }
    a.end(END_TEARDOWN);
    b.end(END_TEARDOWN);
}

/// Stream A's BEGIN went out on connection 1 only. After a reconnect carried only B, A's END must
/// not be written on connection 2.
#[test]
fn end_is_not_written_on_a_connection_that_never_carried_the_begin() {
    let _g = serial();
    let (l, port) = app();
    let sink = TestSink::leak();
    let a = start(sink, port, 0x1A, 100, AudioCodec::AacEld, false, 1, 16_000, 1);
    let mut c1 = accept(&l, Duration::from_secs(2)).unwrap();
    assert!(matches!(read_frame(&mut c1, Duration::from_secs(1)), Some(Frame::Begin { scid: 0x1A, .. })));
    drop(c1);
    let mut b = start(sink, port, 0x1B, 101, AudioCodec::AacEld, false, 4, 48_000, 2);
    // B's entry BEGIN may land on the dead connection 1; it reconnects with its next AU(s).
    let mut c2 = None;
    let end = Instant::now() + Duration::from_secs(3);
    while c2.is_none() && Instant::now() < end {
        b.send(&[0xB]);
        c2 = accept(&l, Duration::from_millis(20));
    }
    let mut c2 = c2.expect("B reconnects");
    read_until(&mut c2, Duration::from_secs(1), |f| matches!(f, Frame::Au { scid: 0x1B, .. }));
    a.end(END_TEARDOWN);
    std::thread::sleep(Duration::from_millis(300));
    b.end(END_TEARDOWN);
    let fr = read_until(&mut c2, Duration::from_secs(1), |f| matches!(f, Frame::End { scid: 0x1B, .. }));
    assert!(
        !fr.iter().any(|f| matches!(f, Frame::End { scid: 0x1A, .. })),
        "A's END landed on a connection without its BEGIN: {fr:?}"
    );
}

/// A session reset clears the sink (drops the socket, bumps the epoch) and ends every stream with
/// `session reset`. The END must not open a new connection to say so.
#[test]
fn session_reset_opens_no_end_only_connection() {
    let _g = serial();
    let (l, port) = app();
    let sink = TestSink::leak();
    let mut s = start(sink, port, 0x2C, 100, AudioCodec::AacEld, false, 2, 16_000, 1);
    let mut c = accept(&l, Duration::from_secs(2)).unwrap();
    s.send(&[1]);
    read_until(&mut c, Duration::from_secs(1), |f| matches!(f, Frame::Au { .. }));
    // AvSession::reset stores the causes, clears the flags, then clear_sinks — microseconds apart,
    // while the stream thread looks at its flag at most every 50 ms. Clear FIRST here: that is the
    // interleaving under test (the thread reaches its END after the socket is gone).
    sink.clear();
    s.cause.store(END_SESSION_RESET, Ordering::Release);
    s.alive.store(false, Ordering::Release);
    assert!(accept(&l, Duration::from_millis(400)).is_none(), "END opened a connection after a reset");
    // The old connection was dropped by the clear: EOF, and no END on it.
    assert!(read_frame(&mut c, Duration::from_millis(200)).is_none());
}
