//! DataStream(130) / RemoteControlSession — the real wireless iAP2 carrier (docs/carplay/05_METADATA_AND_CONTROLS.md).
//!
//! Modern iOS does **not** carry wireless iAP2 in `iAPSendMessage` / `POST /command` (that is the 2017
//! mechanism this project implemented from the R14G17 SDK). It opens a dedicated RCS channel — the
//! phone's `carEndpoint_createiAPChannelIfNeeded`, "Creating RCS channel for iAP", clientTypeUUID
//! `E9459FD0-BCAD-4C45-820F-1E72447EF2F2` — and tunnels iAP2 link frames over it.
//!
//! # The message layer (`APMediaDataControlServer`)
//!
//! 32-byte header, **all fields big-endian**, then the payload. Decoded from `CarPlaySDK.framework`'s
//! `controlServer_receiveData` / `controlServer_sendRequestInternal` / `controlServer_sendResponseInternal`:
//!
//! ```text
//! 0x00  u32  totalLength, INCLUDING this 32-byte header
//! 0x04  4CC  transport kind: 'sync' | 'asyn' | 'rply'   (anything else -> -6717 kFormatErr)
//! 0x08  u64  reserved, always zero
//! 0x10  u32  APMediaDataControlMessageType
//! 0x14  u64  messageID / replyToken  (random for 'sync', 0 for 'asyn', echoed in 'rply')
//! 0x1c  u32  OSStatus result         (meaningful only in 'rply')
//! ```
//!
//! # ⚠ The message type is DIRECTION-ASYMMETRIC — this was the blocker
//!
//! - phone → accessory sends **`'comm'`** (`0x636F6D6D`)
//! - accessory → phone must send **`'cmnd'`** (`0x636D6E64`)
//!
//! `_apEndpointRemoteControlSession_startMessageHandling` (iOS 27 `AirPlaySender`) is the phone's only
//! RCS inbound dispatch, and its first instructions are a filter:
//!
//! ```asm
//! mov  w8, #0x6564 ; movk w8, #0x6469, lsl #16   ; 'died'
//! cmp  w0, w8
//! mov  w8, #0x6e64 ; movk w8, #0x636d, lsl #16   ; 'cmnd'
//! ccmp w0, w8, #0x4, ne
//! b.ne <return noErr>                            ; silently dropped
//! ```
//!
//! Anything that is not `'cmnd'` is discarded with `noErr` — no log, no RST, no entry in the device's
//! iAP2 packet trace. An earlier revision of this file built the outbound header by reproducing the
//! iPhone's own frame byte-for-byte, which is correct for RX and wrong for TX: every ACK we sent was
//! stamped `'comm'` and hit that `b.ne`. The device sat in its FSM `Pending` state — which has no
//! timeout→RST edge — retransmitting SYN-ACK indefinitely.
//!
//! # Crypto
//!
//! Ordinary AirPlay frame codec (`[u16 LE len][ciphertext][tag:16]`, AAD = the length prefix, 12-byte
//! counter nonce, counters independent per direction), keyed by HKDF salt `DataStream-Salt<seed>` where
//! `seed` comes from this stream's own SETUP request. `AirPlayReceiverSessionDataStreamCreate` passes
//! the HKDF **Output** key as `MDC::EncryptionReadKey` and the **Input** key as `MDC::EncryptionWriteKey`,
//! confirming the direction mapping used here.

use std::io::Write;
use std::net::TcpStream;
use std::sync::atomic::{Ordering};
use portable_atomic::{AtomicU64};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rtsp::control::{ControlChannel, ControlKeys};

/// Fixed RCS envelope header length.
pub const HEADER_LEN: usize = 32;

/// Transport kind at 0x04 — asynchronous, no reply owed or expected.
pub const KIND_ASYN: &[u8; 4] = b"asyn";
/// Transport kind at 0x04 — synchronous, the receiver MUST answer with `'rply'`.
pub const KIND_SYNC: &[u8; 4] = b"sync";
/// Transport kind at 0x04 — a reply to a prior `'sync'`.
pub const KIND_RPLY: &[u8; 4] = b"rply";

/// Message type at 0x10 for phone → accessory.
pub const MSGTYPE_COMM: &[u8; 4] = b"comm";
/// Message type at 0x10 for accessory → phone. **The only type the phone accepts.**
pub const MSGTYPE_CMND: &[u8; 4] = b"cmnd";

/// Build an outbound RCS frame carrying one iAP2 link packet.
///
/// Kind `'asyn'` (we owe no reply and expect none) and message type **`'cmnd'`** — see the module docs
/// for why that byte sequence is load-bearing.
pub fn wrap(payload: &[u8]) -> Vec<u8> {
    let total = (HEADER_LEN + payload.len()) as u32;
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&total.to_be_bytes()); // 0x00 totalLength
    out.extend_from_slice(KIND_ASYN); // 0x04 transport kind
    out.extend_from_slice(&0u64.to_be_bytes()); // 0x08 reserved
    out.extend_from_slice(MSGTYPE_CMND); // 0x10 messageType  <-- direction-asymmetric
    out.extend_from_slice(&0u64.to_be_bytes()); // 0x14 messageID (0 for 'asyn')
    out.extend_from_slice(&0u32.to_be_bytes()); // 0x1c OSStatus
    out.extend_from_slice(payload);
    out
}

/// One parsed inbound RCS frame.
pub struct Frame<'a> {
    /// Transport kind at 0x04 (`'sync'` obliges us to reply; `'asyn'` does not).
    pub kind: [u8; 4],
    /// Message type at 0x10 — `'comm'` from the phone.
    pub msg_type: [u8; 4],
    /// Reply token at 0x14; echo it if `kind == 'sync'`.
    pub message_id: u64,
    /// The iAP2 link packet.
    pub payload: &'a [u8],
}

impl Frame<'_> {
    /// Does the sender expect a `'rply'`?
    pub fn needs_reply(&self) -> bool {
        &self.kind == KIND_SYNC
    }
    pub fn kind_str(&self) -> String {
        String::from_utf8_lossy(&self.kind).to_string()
    }
    pub fn msg_type_str(&self) -> String {
        String::from_utf8_lossy(&self.msg_type).to_string()
    }
}

/// Parse one inbound RCS frame. `None` if the buffer is not exactly one well-formed message.
///
/// The strict `declared == pt.len()` check is deliberate: handing `iap_tunnel` a mis-sliced frame is
/// worse than dropping it. Callers needing to handle a message split across crypto frames must
/// reassemble on `declared` before calling this.
pub fn unwrap(pt: &[u8]) -> Option<Frame<'_>> {
    if pt.len() < HEADER_LEN {
        return None;
    }
    let declared = u32::from_be_bytes([pt[0], pt[1], pt[2], pt[3]]) as usize;
    if declared != pt.len() {
        return None;
    }
    let mut kind = [0u8; 4];
    kind.copy_from_slice(&pt[4..8]);
    // Only the three documented kinds are legal; the receiver rejects anything else with kFormatErr.
    if &kind != KIND_ASYN && &kind != KIND_SYNC && &kind != KIND_RPLY {
        return None;
    }
    let mut msg_type = [0u8; 4];
    msg_type.copy_from_slice(&pt[16..20]);
    let mut idb = [0u8; 8];
    idb.copy_from_slice(&pt[20..28]);
    Some(Frame {
        kind,
        msg_type,
        message_id: u64::from_be_bytes(idb),
        payload: &pt[HEADER_LEN..],
    })
}

struct Sink {
    sock: TcpStream,
    chan: ControlChannel,
    /// Bumped on every `register`; `clear_if` uses it so a stale connection's teardown cannot drop a
    /// newer live sink (they overlap during a mid-session re-SETUP).
    generation: u64,
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Register the live RCS socket as the outbound iAP2 path. Returns the generation token to hand back
/// to [`clear_if`] when this connection ends.
pub fn register(sock: TcpStream, keys: ControlKeys) -> u64 {
    // Take the SINK lock FIRST and hold it across the allocation. Previously the counter had its own
    // mutex and was released before SINK was taken, so two overlapping RCS connections could interleave
    // as: A allocates gen 1 -> preempted -> B allocates gen 2 and installs -> A resumes and installs
    // gen 1, overwriting the NEWER live sink. When B's connection later closed, `clear_if(2)` would see
    // gen 1, decline to clear, and leave iAP2 transmitting on the superseded socket.
    let mut guard = crate::plock(&SINK);
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    // Belt and braces: even under the lock, never let an older registration displace a newer one.
    if let Some(existing) = guard.as_ref() {
        if existing.generation > generation {
            eprintln!(
                "[datastream] refusing to install gen {generation} over newer live sink (gen {})",
                existing.generation
            );
            return generation;
        }
    }
    *guard = Some(Sink { sock, chan: ControlChannel::new(keys), generation });
    eprintln!(
        "[datastream] outbound sink registered (gen {generation}) — iAP2 now transmits over the RCS channel"
    );
    generation
}

/// Drop the sink only if it is still the one registered by `generation`.
pub fn clear_if(generation: u64) {
    let mut guard = crate::plock(&SINK);
    match guard.as_ref() {
        Some(s) if s.generation == generation => {
            *guard = None;
            eprintln!("[datastream] outbound sink cleared (gen {generation})");
        }
        Some(s) => eprintln!(
            "[datastream] stale connection (gen {generation}) closed; keeping live sink (gen {})",
            s.generation
        ),
        None => {}
    }
}

/// Unconditionally drop the sink (session teardown).
pub fn clear() {
    if crate::plock(&SINK).take().is_some() {
        eprintln!("[datastream] outbound sink cleared (session teardown)");
    }
}

/// Is the RCS channel available as the iAP2 transport?
pub fn active() -> bool {
    crate::plock(&SINK).is_some()
}

/// Send one iAP2 link frame over the RCS channel. Returns false if no sink is registered or the write
/// fails — the caller then falls back to the legacy `iAPSendMessage` path.
pub fn send(iap_frame: &[u8]) -> bool {
    let mut guard = crate::plock(&SINK);
    let Some(sink) = guard.as_mut() else {
        return false;
    };
    let framed = sink.chan.encrypt_frame(&wrap(iap_frame));
    let ctrl = iap_frame.get(4).copied().unwrap_or(0);
    match write_all_retrying(&mut sink.sock, &framed) {
        Ok(()) => {
            // Per-frame hex dump gated behind CARPLAY_EVENTS_LOG (R6) — this fires for every
            // outbound iAP2 frame, which is far too chatty for a 528 MHz core at steady state.
            if crate::events::events_log() {
                eprintln!(
                    "[datastream] TX(RCS,'cmnd') iAP2 {} B ctrl={ctrl:#04x} ({} B on wire) hex={:02x?}",
                    iap_frame.len(),
                    framed.len(),
                    &iap_frame[..iap_frame.len().min(48)]
                );
            }
            true
        }
        Err(e) => {
            // Unrecoverable, and it genuinely is: `encrypt_frame` has already advanced the ChaCha
            // write counter, so these exact bytes are the only ones the peer can decrypt at this
            // counter value. If they cannot be delivered the stream is desynced for good — hence
            // shutdown, so the phone sees a clean close rather than a frame it can never parse.
            eprintln!("[datastream] TX failed after retries: {e} — dropping sink, closing socket");
            let _ = sink.sock.shutdown(std::net::Shutdown::Both);
            *guard = None;
            false
        }
    }
}

/// Write the whole buffer, retrying transient stalls instead of abandoning it.
///
/// A single `WouldBlock`/`TimedOut` used to drop the sink permanently — `register` is called exactly
/// once per accepted RCS connection (`session.rs`, inside `if chan.is_none() && !probed`), so nothing
/// could ever re-register. The reader half kept working, so the tunnel went ONE-WAY: every ACK,
/// Identify step and subscribe silently fell back to `POST /command`, which the phone does not route
/// into its iAP2 stack. ⚠ UNRESOLVED CONTRADICTION: docs/carplay/05_METADATA_AND_CONTROLS.md §1.1 — the authoritative transport
/// document — says the opposite, that `POST /command` "remains a working outbound carrier" (iOS
/// 2xx-acks it), and `events::send_iap_message` keeps it as the deliberate pre-RCS fallback on that
/// basis. Whether those acked frames actually reach the phone's iAP2 stack has not been settled on
/// hardware; do not treat either claim as established until it is.
/// That is the same silent-one-way shape as the original `'comm'`/`'cmnd'`
/// blocker, and a full kernel send buffer during a 4K video burst on the same wireless link is enough
/// to trigger it.
///
/// Retrying rather than keeping-and-skipping is the point: the counter is burned at encrypt time, so
/// "keep the sink, drop this frame" would leave the peer permanently one frame behind. Either these
/// bytes go out, or the channel is finished.
fn write_all_retrying(sock: &mut TcpStream, buf: &[u8]) -> std::io::Result<()> {
    use std::io::ErrorKind::{Interrupted, TimedOut, WouldBlock};
    const DEADLINE: Duration = Duration::from_secs(6); // 3× the socket's own 2 s write timeout
    let start = Instant::now();
    let mut sent = 0usize;
    while sent < buf.len() {
        match sock.write(&buf[sent..]) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "peer accepted no bytes",
                ))
            }
            Ok(n) => sent += n,
            Err(e) if matches!(e.kind(), WouldBlock | TimedOut | Interrupted) => {
                if start.elapsed() >= DEADLINE {
                    return Err(e);
                }
                eprintln!(
                    "[datastream] TX stalled ({e}) at {sent}/{} B — retrying",
                    buf.len()
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes observed from the iPhone 2026-07-25 — a SYN|ACK (control 0xC0) inside the envelope.
    /// Note `61 73 79 6e` ('asyn') at 0x04 and `63 6f 6d 6d` ('comm') at 0x10.
    const OBSERVED_RX: &[u8] = &[
        0x00, 0x00, 0x00, 0x3a, 0x61, 0x73, 0x79, 0x6e, 0, 0, 0, 0, 0, 0, 0, 0, 0x63, 0x6f, 0x6d,
        0x6d, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0x5a, 0x00, 0x1a, 0xc0, 0x7f, 0x00, 0x00,
        0x4e, 0x01, 0x20, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x02,
        0x01, 0x01, 0xdb,
    ];

    #[test]
    fn unwraps_the_observed_iphone_syn_ack() {
        let f = unwrap(OBSERVED_RX).expect("envelope parses");
        assert_eq!(&f.kind, KIND_ASYN);
        assert_eq!(&f.msg_type, MSGTYPE_COMM, "phone sends 'comm'");
        assert_eq!(f.message_id, 0, "'asyn' carries no reply token");
        assert!(!f.needs_reply());
        assert_eq!(f.payload.len(), 26);
        assert_eq!(&f.payload[..2], &[0xff, 0x5a], "iAP2 link magic");
        assert_eq!(f.payload[4], 0xc0, "control byte SYN|ACK");
    }

    /// THE regression test for the blocker: outbound must be 'cmnd', never 'comm'.
    /// `write_all_retrying` must survive a stall and still deliver every byte in order. The old
    /// `write_all` dropped the sink on the first `WouldBlock`, and since `register` runs once per
    /// accepted connection nothing could re-register — the tunnel went one-way for the rest of the
    /// session (docs/ops/05_AUDITS.md §3). Uses a real socket pair so the stall is a real kernel condition.
    #[test]
    fn transient_write_stall_does_not_lose_bytes() {
        use std::io::Read;
        use std::net::{TcpListener, TcpStream as Ts};
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().unwrap();
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let expect = payload.clone();

        // Reader deliberately starts late and reads slowly, so the writer's send buffer fills and
        // `write` returns a short count or WouldBlock/TimedOut.
        let reader = std::thread::spawn(move || {
            let (mut c, _) = l.accept().expect("accept");
            std::thread::sleep(Duration::from_millis(120));
            let mut got = Vec::new();
            let mut chunk = [0u8; 4096];
            while got.len() < expect.len() {
                match c.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => got.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            got
        });

        let mut w = Ts::connect(addr).expect("connect");
        w.set_write_timeout(Some(Duration::from_millis(50))).unwrap();
        write_all_retrying(&mut w, &payload).expect("retrying write must deliver everything");
        drop(w);

        let got = reader.join().expect("reader");
        assert_eq!(got.len(), payload.len(), "byte count mismatch after stall");
        assert_eq!(got, payload, "bytes reordered or corrupted across the retry");
    }

    /// The phone's own filter, `_apEndpointRemoteControlSession_startMessageHandling`
    /// (iOS 27 `AirPlaySender` @ 0x25175a3d4):
    ///
    /// ```asm
    /// mov  w8, 0x6e64 ; movk w8, 0x636d, lsl 16   ; w8 = 0x636D6E64 = 'cmnd'
    /// ```
    ///
    /// Anything else takes the reject path — no logging call, branches before the `CFRetain`, returns
    /// `noErr`. The device sits in FSM state `Pending`, which has no timeout-to-RST edge, and
    /// retransmits SYN-ACK forever with no diagnostic on either side. That was the final blocker.
    ///
    /// The expected value is RECONSTRUCTED from the two ARM64 immediates rather than written as a
    /// byte literal, so falsifying this test means editing quoted disassembly in the module docs, not
    /// flipping a constant. The old form asserted `MSGTYPE_CMND` against itself: changing the
    /// constant to `b"cmdn"` passed the entire suite and silently reinstated the shipped blocker.
    #[test]
    fn outbound_message_type_is_cmnd_not_comm() {
        let framed = wrap(&[0xde, 0xad, 0xbe, 0xef]);
        let phone_filter_imm: u32 = (0x636d << 16) | 0x6e64;
        assert_eq!(
            &framed[16..20],
            &phone_filter_imm.to_be_bytes(),
            "the phone drops any messageType but 'cmnd', silently and with noErr"
        );
        // Anchored to a real captured iPhone frame, not to our own constant: the TX type must differ
        // from the RX type the phone uses toward us.
        assert_ne!(
            &framed[16..20],
            &OBSERVED_RX[16..20],
            "TX messageType must differ from the captured inbound type"
        );
    }

    /// `'sync'` and `'rply'` were wholly unpinned — breaking either passed the suite. A broken
    /// `KIND_SYNC` is worse than cosmetic: `unwrap`'s whitelist would then reject every real `'sync'`
    /// frame, so the accessory would never emit the `'rply'` such a frame obliges.
    #[test]
    fn transport_kind_four_ccs_are_apples_literal_bytes() {
        assert_eq!(KIND_ASYN, b"asyn");
        assert_eq!(KIND_SYNC, b"sync");
        assert_eq!(KIND_RPLY, b"rply");
        assert_eq!(MSGTYPE_COMM, b"comm");
        assert_eq!(MSGTYPE_CMND, b"cmnd");
        // Every kind must be distinct — a copy-paste collision would route frames to the wrong arm.
        let all = [KIND_ASYN, KIND_SYNC, KIND_RPLY];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn wrap_produces_a_well_formed_32_byte_header() {
        let payload = [0xaa; 26];
        let framed = wrap(&payload);
        assert_eq!(framed.len(), HEADER_LEN + 26);
        assert_eq!(
            u32::from_be_bytes([framed[0], framed[1], framed[2], framed[3]]) as usize,
            framed.len(),
            "totalLength includes the header"
        );
        assert_eq!(&framed[4..8], KIND_ASYN);
        assert_eq!(&framed[8..16], &[0u8; 8], "reserved");
        assert_eq!(&framed[20..28], &[0u8; 8], "messageID zero for 'asyn'");
        assert_eq!(&framed[28..32], &[0u8; 4], "OSStatus zero");
        assert_eq!(&framed[HEADER_LEN..], &payload);
    }

    #[test]
    fn wrap_round_trips_through_unwrap() {
        let payload = [0x11u8; 40];
        let framed = wrap(&payload);
        let f = unwrap(&framed).unwrap();
        assert_eq!(&f.msg_type, MSGTYPE_CMND);
        assert_eq!(f.payload, &payload);
    }

    #[test]
    fn rejects_a_length_that_disagrees_with_the_buffer() {
        let mut bad = OBSERVED_RX.to_vec();
        bad[3] = 0x3b; // claim 59 bytes in a 58-byte buffer
        assert!(unwrap(&bad).is_none());
    }

    #[test]
    fn rejects_an_unknown_transport_kind() {
        let mut bad = OBSERVED_RX.to_vec();
        bad[4..8].copy_from_slice(b"junk");
        assert!(unwrap(&bad).is_none());
    }

    #[test]
    fn rejects_a_runt() {
        assert!(unwrap(&OBSERVED_RX[..HEADER_LEN - 1]).is_none());
    }

    #[test]
    fn sync_frames_are_flagged_as_needing_a_reply() {
        let mut sync = OBSERVED_RX.to_vec();
        sync[4..8].copy_from_slice(KIND_SYNC);
        sync[20..28].copy_from_slice(&0x1122334455667788u64.to_be_bytes());
        let f = unwrap(&sync).unwrap();
        assert!(f.needs_reply());
        assert_eq!(f.message_id, 0x1122334455667788);
    }
}
