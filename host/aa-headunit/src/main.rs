//! aa-headunit — minimal Android Auto head-unit client (docs/host/02_ANDROID_AUTO.md).
//!
//! Connects to Google's Android Auto developer head-unit server on a phone
//! (default 127.0.0.1:5277 via `adb forward tcp:5277 tcp:5277`), performs the AA
//! handshake as aasdk's head unit does, advertises the service set gearhead
//! requires, drives the video channel, and captures the H.264 (Annex-B) stream to
//! a file. No hardware/box in the loop.
//!
//! This is interoperability/accessory development: the head-unit certificate is
//! presented only to the device owner's own phone during the owner's own session.
//!
//! Wire protocol. The values below are protocol facts, established from the decompiled
//! gearhead app and from live captures, and cross-checked against aasdk's GPL-licensed
//! implementation. No aasdk code is used here (see NOTICE.md); note the channel ids follow
//! gearhead's ordinals, not aasdk's.
//!   Transport frame  = [channel u8][flags u8] [size] [payload]
//!     flags = frameType(FIRST=1,LAST=2,BULK=3) | encType(PLAIN=0,ENC=8) | msgType(SPECIFIC=0,CONTROL=4)
//!     size  = u16 BE (SHORT); on a FIRST-only multiframe, u16 frame + u32 total (EXTENDED=6B)
//!   Message payload  = [messageId u16 BE][body]
//!   Handshake (control ch 0): VERSION_REQUEST(1) -> VERSION_RESPONSE(2) ->
//!     encapsulated TLS (ENCAPSULATED_SSL=3, head unit is TLS *client* presenting
//!     the HU cert) -> AUTH_COMPLETE(4) -> SERVICE_DISCOVERY_REQUEST(5) ->
//!     SERVICE_DISCOVERY_RESPONSE(6) advertising the sinks/sources.
//!   Then the phone opens channels; the head unit answers channel-open, the
//!   audio-focus/ping/nav-focus control handshakes, the sensor driving-status
//!   gate, video Setup->Config->VideoFocus, and ACKs each media frame.

mod proto;
mod tls;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

// ---- frame flag bits (aasdk Messenger enums) ----
const FT_FIRST: u8 = 1 << 0;
const FT_LAST: u8 = 1 << 1;
const FT_BULK: u8 = FT_FIRST | FT_LAST; // 0x03
const ENC_ENCRYPTED: u8 = 1 << 3; // 0x08
const MT_CONTROL: u8 = 1 << 2; // 0x04 (SPECIFIC = 0)

const CH_CONTROL: u8 = 0;
const CH_SENSOR: u8 = 1; // SENSOR enum ordinal
const CH_VIDEO: u8 = 3; // MEDIA_SINK_VIDEO enum ordinal; also the id we advertise
const CH_INPUT: u8 = 8; // INPUT_SOURCE enum ordinal

// ---- control message ids (ControlMessageType.proto) ----
const MSG_VERSION_REQUEST: u16 = 1;
const MSG_VERSION_RESPONSE: u16 = 2;
const MSG_ENCAPSULATED_SSL: u16 = 3;
const MSG_AUTH_COMPLETE: u16 = 4;
const MSG_SERVICE_DISCOVERY_REQUEST: u16 = 5;
const MSG_SERVICE_DISCOVERY_RESPONSE: u16 = 6;
const MSG_CHANNEL_OPEN_REQUEST: u16 = 7;
const MSG_CHANNEL_OPEN_RESPONSE: u16 = 8;
const MSG_PING_REQUEST: u16 = 11;
const MSG_PING_RESPONSE: u16 = 12;
const MSG_NAV_FOCUS_REQUEST: u16 = 13;
const MSG_NAV_FOCUS_NOTIFICATION: u16 = 14;
const MSG_BYEBYE_REQUEST: u16 = 15;
const MSG_BYEBYE_RESPONSE: u16 = 16;
const MSG_AUDIO_FOCUS_REQUEST: u16 = 18;
const MSG_AUDIO_FOCUS_NOTIFICATION: u16 = 19;
// AudioFocusRequestType (phone->HU) vs AudioFocusStateType (HU->phone) — distinct enums.
const AUDIO_FOCUS_TYPE_RELEASE: u64 = 4;
const AUDIO_FOCUS_STATE_GAIN: u64 = 1;
const AUDIO_FOCUS_STATE_LOSS: u64 = 3;

// ---- media/AV channel message ids (MediaMessageId.proto) ----
const MEDIA_DATA: u16 = 0; // [timestamp u64 BE][Annex-B]
const MEDIA_CODEC_CONFIG: u16 = 1; // [Annex-B] (SPS/PPS), no timestamp
const MEDIA_SETUP: u16 = 32768; // phone -> HU: Setup{ codec }
const MEDIA_START: u16 = 32769; // phone -> HU: Start{ session_id }
const MEDIA_STOP: u16 = 32770; // phone -> HU
const MEDIA_CONFIG: u16 = 32771; // HU -> phone: Config{ status, max_unacked, indices }
const MEDIA_ACK: u16 = 32772; // HU -> phone: Ack{ session_id, ack }
const MEDIA_VIDEO_FOCUS_REQUEST: u16 = 32775; // phone -> HU
const MEDIA_VIDEO_FOCUS_NOTIFICATION: u16 = 32776; // HU -> phone

// ---- sensor channel message ids (SensorMessageId.proto) ----
const SENSOR_REQUEST: u16 = 32769; // phone -> HU: SensorRequest{ type }
const SENSOR_RESPONSE: u16 = 32770; // HU -> phone: start response
const SENSOR_BATCH: u16 = 32771; // HU -> phone: SensorBatch (driving status / night)
const SENSOR_TYPE_DRIVING_STATUS: u64 = 13;
const SENSOR_TYPE_NIGHT_MODE: u64 = 10;

// ---- input channel message ids (InputMessageId.proto) ----
const INPUT_REPORT: u16 = 32769; // HU -> phone: InputReport{ touch_event }
const INPUT_KEY_BINDING_REQUEST: u16 = 32770; // phone -> HU
const INPUT_KEY_BINDING_RESPONSE: u16 = 32771; // HU -> phone
const INPUT_ACTION_DOWN: u32 = 0; // PointerAction
const INPUT_ACTION_UP: u32 = 1;

// Advertised video mode (VideoCodecResolutionType / VideoFrameRateType enums).
const VIDEO_RES_800X480: u32 = 1;
const VIDEO_FPS_30: u32 = 2;
const VIDEO_DENSITY: u32 = 160;
const VIDEO_WIDTH: u32 = 800; // must match the advertised resolution enum
const VIDEO_HEIGHT: u32 = 480;
const MEDIA_DATA_TS_LEN: usize = 8; // MEDIA_DATA prefix: u64 BE timestamp

// Upper bound on a reassembled multi-frame message, to bound memory on a hostile
// or buggy peer that streams FIRST..MIDDLE without ever sending LAST.
const MAX_REASSEMBLED: usize = 64 * 1024 * 1024;

// Advertised AA protocol version. 1.7 matches Google's current Desktop Head Unit
// (DHU v2.0, build 2022-03-30 — Controller::sendVersionRequest requests 1.7) and
// the version the Pixel reports back, so we build on the current foundation rather
// than aasdk's default 1.6. Bump if a newer phone/DHU reports higher in
// VERSION_RESPONSE (logged at connect).
const AA_VERSION_MAJOR: u16 = 1;
const AA_VERSION_MINOR: u16 = 7;

const CERT_PEM: &str = include_str!("../certs/headunit.crt");
const KEY_PEM: &str = include_str!("../certs/headunit.key");

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join("")
}

/// A parsed inbound frame (already reassembled if it spanned multiple frames).
struct Msg {
    channel: u8,
    encrypted: bool,
    payload: Vec<u8>, // [messageId u16 BE][body], still ciphertext if `encrypted`
}

struct Link {
    sock: TcpStream,
    // Partial multi-frame messages, keyed by channel: (encrypted, accumulated
    // PLAINTEXT). aasdk buffers per channel (MessageInStream
    // messageBuffer_[channelId]) so a frame from another channel interleaved
    // between a FIRST and LAST does not corrupt the reassembly — but note the
    // buffer holds PLAINTEXT, not ciphertext, and that distinction is the whole
    // bug fixed on 2026-08-27 (see `recv`).
    partial: std::collections::HashMap<u8, (bool, Vec<u8>)>,
}

impl Link {
    /// Send one BULK frame. `payload` is the full message payload
    /// ([messageId][body]) — already TLS-encrypted iff `encrypted`.
    fn send(&mut self, channel: u8, encrypted: bool, control: bool, payload: &[u8]) -> std::io::Result<()> {
        let mut flags = FT_BULK;
        if encrypted {
            flags |= ENC_ENCRYPTED;
        }
        if control {
            flags |= MT_CONTROL;
        }
        // SHORT size: u16 BE of payload length. Every message this head unit sends
        // (control replies, small protobufs, TLS handshake records) is well under
        // 64 KiB, so we only emit single BULK frames; a larger one is a bug, not a
        // panic.
        if payload.len() > u16::MAX as usize {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("payload {} bytes exceeds SHORT frame limit (EXTENDED framing not implemented)", payload.len()),
            ));
        }
        let mut frame = Vec::with_capacity(4 + payload.len());
        frame.push(channel);
        frame.push(flags);
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        frame.extend_from_slice(payload);
        eprintln!(
            "  TX ch={channel} flags=0x{flags:02x} len={} payload={}",
            payload.len(),
            hex(&payload[..payload.len().min(16)])
        );
        self.sock.write_all(&frame)
    }

    fn read_exact(&mut self, n: usize) -> std::io::Result<Vec<u8>> {
        let mut buf = vec![0u8; n];
        self.sock.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// Receive one full message, reassembling FIRST..MIDDLE..LAST runs that may
    /// be interleaved across channels (buffered per channel, like aasdk).
    ///
    /// DECRYPTS EACH FRAME AS IT ARRIVES and accumulates PLAINTEXT. There is one
    /// TLS stream shared by every channel, so its ciphertext must be fed in
    /// arrival order; reassembling a fragmented message's CIPHERTEXT first (what
    /// this did until 2026-08-27, following aasdk) withholds the FIRST fragment's
    /// bytes while feeding the next frame's, and the phone DOES interleave.
    /// Device trace from the macOS client, which had the identical bug:
    ///
    ///     ch=3 flags=0x9 len=16149 FIRST   <- fragmented message starts
    ///     ch=4 flags=0xb len=8231  BULK    <- another channel, before its LAST
    ///     decrypt FAILED on ch=4
    ///
    /// Out-of-order ciphertext is an errSSLDecryptionFail/bad-record the context
    /// never recovers from; it killed every session at an unpredictable 57-94 s.
    /// That the phone interleaves at all proves encryption is PER FRAME, not per
    /// message. A frame whose ciphertext ends mid-record simply yields no
    /// plaintext here — the TLS layer keeps the remainder for the next frame.
    ///
    /// NOT re-verified on hardware in the Rust client (the fix was proven in the
    /// macOS client over OCBM); it is the same transform on the same framing.
    /// `tls` is None only before the context exists (the plaintext VERSION
    /// exchange); an ENCRYPTED frame arriving then is a protocol error, not
    /// something to pass through as if it were clear.
    fn recv(&mut self, mut tls: Option<&mut tls::HeadUnitTls>) -> std::io::Result<Msg> {
        loop {
            let hdr = self.read_exact(2)?;
            let channel = hdr[0];
            let flags = hdr[1];
            let frame_type = flags & FT_BULK;
            let encrypted = flags & ENC_ENCRYPTED != 0;

            // Size field: SHORT (2) normally; EXTENDED (6) only on a FIRST-only
            // frame (FIRST set, LAST clear) — u16 frame len + u32 total (ignored).
            let size_bytes = self.read_exact(2)?;
            let frame_len = u16::from_be_bytes([size_bytes[0], size_bytes[1]]) as usize;
            if frame_type == FT_FIRST {
                let _total = self.read_exact(4)?;
            }

            let payload = self.read_exact(frame_len)?;
            eprintln!(
                "  RX ch={channel} flags=0x{flags:02x} len={frame_len} payload={}",
                hex(&payload[..payload.len().min(16)])
            );

            // Decrypt HERE, before any per-channel buffering, so the TLS stream
            // is fed strictly in arrival order (see the doc comment above).
            let payload = if encrypted {
                let tls = tls.as_deref_mut().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("ENCRYPTED frame on ch={channel} before the TLS context exists"),
                    )
                })?;
                tls.decrypt(&payload).map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, format!("decrypt failed on ch={channel}: {e}"))
                })?
            } else {
                payload
            };

            match frame_type {
                FT_BULK => return Ok(Msg { channel, encrypted, payload }),
                FT_FIRST => {
                    self.partial.insert(channel, (encrypted, payload));
                }
                _ => {
                    // MIDDLE or LAST: append to this channel's partial buffer.
                    if let Some(entry) = self.partial.get_mut(&channel) {
                        entry.1.extend_from_slice(&payload);
                        if entry.1.len() > MAX_REASSEMBLED {
                            self.partial.remove(&channel);
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("reassembled message on ch={channel} exceeded {MAX_REASSEMBLED} bytes"),
                            ));
                        }
                        if flags & FT_LAST != 0 {
                            let (enc, acc) = self.partial.remove(&channel).unwrap();
                            return Ok(Msg { channel, encrypted: enc, payload: acc });
                        }
                    } else {
                        // Stray continuation with no FIRST — drop it (aasdk marks
                        // the frame invalid). Keep reading.
                        eprintln!("  .. dropping stray continuation frame on ch={channel}");
                    }
                }
            }
        }
    }
}

/// Split a message payload into [messageId u16 BE] + body.
fn split_msgid(p: &[u8]) -> (u16, &[u8]) {
    if p.len() < 2 {
        return (0, &[]); // 0 is never a valid message id here → callers error out
    }
    let id = u16::from_be_bytes([p[0], p[1]]);
    (id, &p[2..])
}

/// Send a message: payload = [msgid BE][body], TLS-encrypted iff `encrypted`,
/// framed BULK + (ENCRYPTED if `encrypted`) + (CONTROL if `control` else SPECIFIC).
fn send_msg(
    link: &mut Link,
    tls: &mut tls::HeadUnitTls,
    channel: u8,
    msgid: u16,
    encrypted: bool,
    control: bool,
    body: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut plain = Vec::with_capacity(2 + body.len());
    plain.extend_from_slice(&msgid.to_be_bytes());
    plain.extend_from_slice(body);
    if encrypted {
        let ct = tls.encrypt(&plain)?;
        link.send(channel, true, control, &ct)?;
    } else {
        link.send(channel, false, control, &plain)?;
    }
    Ok(())
}

/// Send an ENCRYPTED message (the common post-auth case).
fn send_enc(
    link: &mut Link,
    tls: &mut tls::HeadUnitTls,
    channel: u8,
    msgid: u16,
    control: bool,
    body: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    send_msg(link, tls, channel, msgid, true, control, body)
}

/// Receive one message and (if the frame is ENCRYPTED) decrypt it.
/// Returns (channel, was_encrypted, msgid, body).
fn recv_msg(
    link: &mut Link,
    tls: &mut tls::HeadUnitTls,
) -> Result<(u8, bool, u16, Vec<u8>), Box<dyn std::error::Error>> {
    let m = link.recv(Some(tls))?;   // frames are decrypted on arrival, inside recv
    let encrypted = m.encrypted;
    let payload = m.payload;
    if payload.len() < 2 {
        return Err(format!("message too short ({} bytes) on ch={}", payload.len(), m.channel).into());
    }
    let (id, body) = split_msgid(&payload);
    Ok((m.channel, encrypted, id, body.to_vec()))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:5277".into());
    let out_path = std::env::args().nth(2).unwrap_or_else(|| "/tmp/aa_capture.h264".into());
    let max_frames: u32 = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(120);
    // Optional touch injection to drive the AA UI: AA_TAP="x,y" (in 800x480 space).
    let tap: Option<(u32, u32)> = std::env::var("AA_TAP").ok().and_then(|s| {
        let mut p = s.split(',');
        Some((p.next()?.trim().parse().ok()?, p.next()?.trim().parse().ok()?))
    });
    eprintln!("[aa-headunit] connecting to {addr}");
    let sock = TcpStream::connect(&addr)?;
    sock.set_nodelay(true)?;
    sock.set_read_timeout(Some(Duration::from_secs(15)))?;
    let mut link = Link { sock, partial: std::collections::HashMap::new() };

    // --- 1. VERSION_REQUEST (plaintext, SPECIFIC) ---
    // payload = [msgid=1 BE][major u16 BE][minor u16 BE]
    let mut vr = Vec::new();
    vr.extend_from_slice(&MSG_VERSION_REQUEST.to_be_bytes());
    vr.extend_from_slice(&AA_VERSION_MAJOR.to_be_bytes());
    vr.extend_from_slice(&AA_VERSION_MINOR.to_be_bytes());
    eprintln!("[aa-headunit] -> VERSION_REQUEST {AA_VERSION_MAJOR}.{AA_VERSION_MINOR}");
    link.send(CH_CONTROL, false, false, &vr)?;

    // --- 2. VERSION_RESPONSE ---
    let m = link.recv(None)?;   // plaintext frame; no TLS context yet
    let (id, body) = split_msgid(&m.payload);
    if id != MSG_VERSION_RESPONSE {
        return Err(format!("expected VERSION_RESPONSE(2), got msgid {id}").into());
    }
    if body.len() >= 6 {
        let major = u16::from_be_bytes([body[0], body[1]]);
        let minor = u16::from_be_bytes([body[2], body[3]]);
        let status = u16::from_be_bytes([body[4], body[5]]);
        eprintln!("[aa-headunit] <- VERSION_RESPONSE {major}.{minor} status={status}");
        // aasdk aborts only on STATUS_NO_COMPATIBLE_VERSION (-1 == 0xFFFF).
        if status == 0xFFFF {
            return Err("phone reported STATUS_NO_COMPATIBLE_VERSION (bump advertised version)".into());
        }
    } else {
        eprintln!("[aa-headunit] <- VERSION_RESPONSE (short body {} bytes)", body.len());
    }

    // --- 3. Encapsulated TLS handshake (head unit is the TLS client) ---
    eprintln!("[aa-headunit] starting encapsulated TLS handshake");
    let mut tls = tls::HeadUnitTls::new(CERT_PEM, KEY_PEM)?;

    // Drive: do_handshake -> drain out -> send ENCAPSULATED_SSL; on WANT_READ,
    // recv one ENCAPSULATED_SSL frame -> feed in -> repeat.
    loop {
        let status = tls.advance_handshake()?;
        // flush any handshake bytes the SSL engine produced
        while let Some(out) = tls.take_outbound() {
            let mut p = Vec::with_capacity(2 + out.len());
            p.extend_from_slice(&MSG_ENCAPSULATED_SSL.to_be_bytes());
            p.extend_from_slice(&out);
            link.send(CH_CONTROL, false, false, &p)?;
        }
        match status {
            tls::HsStatus::Done => {
                eprintln!("[aa-headunit] TLS handshake COMPLETE: {}", tls.describe());
                break;
            }
            tls::HsStatus::WantRead => {
                let m = link.recv(Some(&mut tls))?;   // handshake frames are PLAINTEXT — passed through
                let (id, sslbody) = split_msgid(&m.payload);
                if id != MSG_ENCAPSULATED_SSL {
                    return Err(format!("expected ENCAPSULATED_SSL(3) during handshake, got {id}").into());
                }
                tls.feed_inbound(sslbody);
            }
        }
    }

    // --- 4. AUTH_COMPLETE (head unit -> phone, PLAIN) ---
    // In the head-unit role WE send AuthComplete once TLS is up; the phone does
    // not send one back. Body = AuthResponse{status=STATUS_SUCCESS}.
    let mut ac = Vec::new();
    ac.extend_from_slice(&MSG_AUTH_COMPLETE.to_be_bytes());
    ac.extend_from_slice(&proto::auth_response_success());
    eprintln!("[aa-headunit] -> AUTH_COMPLETE (status=0)");
    link.send(CH_CONTROL, false, false, &ac)?;

    // --- 5. SERVICE_DISCOVERY_REQUEST (phone -> head unit, ENCRYPTED) ---
    // The phone now sends US the ServiceDiscoveryRequest. Receiving and
    // decrypting it is the Phase-0 success signal: it proves the authenticated,
    // encrypted channel is live in both directions.
    let m = link.recv(Some(&mut tls))?;
    if !m.encrypted {
        eprintln!("[aa-headunit] WARNING: frame not marked ENCRYPTED (flags said plaintext)");
    }
    let clear = m.payload;   // recv decrypted it on arrival; decrypting again would be a double-decrypt
    let (id, body) = split_msgid(&clear);
    eprintln!("[aa-headunit] <- msgid={id} ({} bytes plaintext) on ch={}", clear.len(), m.channel);

    if id != MSG_SERVICE_DISCOVERY_REQUEST {
        eprintln!("[aa-headunit] expected SERVICE_DISCOVERY_REQUEST(5), got msgid {id}");
        eprintln!("[aa-headunit]   body: {}", proto::summarize(body));
        return Err(format!("unexpected msgid {id} after auth").into());
    }
    eprintln!("[aa-headunit] SERVICE_DISCOVERY_REQUEST decrypted OK");
    eprintln!("[aa-headunit]   {}", proto::summarize(body));
    eprintln!("[aa-headunit] PHASE 0 stage OK: authenticated + encrypted AA session established.");

    // --- 6. SERVICE_DISCOVERY_RESPONSE: video + input + sensor + audio (Phase 1) ---
    let sd = proto::service_discovery_response_full(
        VIDEO_RES_800X480,
        VIDEO_FPS_30,
        VIDEO_DENSITY,
        VIDEO_WIDTH,
        VIDEO_HEIGHT,
    );
    eprintln!("[aa-headunit] -> SERVICE_DISCOVERY_RESPONSE (video+input+sensor+audio, 800x480@30)");
    send_enc(&mut link, &mut tls, CH_CONTROL, MSG_SERVICE_DISCOVERY_RESPONSE, false, &sd)?;

    // --- Phase 1: drive the video channel and capture the H.264 stream ---
    run_video_capture(&mut link, &mut tls, &out_path, max_frames, tap)?;
    Ok(())
}

/// Phase 1 event loop: react to the phone's channel-open / setup / start / data,
/// send the required responses (channel-open OK, config READY, video focus, and
/// an ACK per media message), and write the H.264 Annex-B stream to `out_path`.
fn run_video_capture(
    link: &mut Link,
    tls: &mut tls::HeadUnitTls,
    out_path: &str,
    max_frames: u32,
    tap: Option<(u32, u32)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = std::fs::File::create(out_path)?;
    let mut session_id: i64 = 0;
    let mut frames: u32 = 0;
    let mut config_seen = false;
    let mut total_bytes: usize = 0;
    let start = Instant::now();
    let mut tapped = false;

    eprintln!("[aa-headunit] Phase 1: waiting for the phone to open the video channel...");
    loop {
        let (ch, enc, id, body) = match recv_msg(link, tls) {
            Ok(v) => v,
            Err(e) => {
                // A read timeout after we've captured frames is a normal end.
                if frames > 0 {
                    eprintln!("[aa-headunit] stream ended ({e})");
                    break;
                }
                return Err(e);
            }
        };

        // Ping keepalive (control channel): the phone drives PingRequest on its own
        // cadence and tracks our replies; echo the timestamp back or it tears the
        // link down on its ping timeout a few seconds in. aasdk sends pings
        // PLAIN + SPECIFIC; mirror the request's encryption to match the phone.
        if ch == CH_CONTROL && id == MSG_PING_REQUEST {
            let ts = proto::get_field_varint(&body, 1).unwrap_or(0);
            send_msg(link, tls, CH_CONTROL, MSG_PING_RESPONSE, enc, false, &proto::ping_response(ts))?;
            continue;
        }

        // Nav focus (control channel): the phone requests it when turn-by-turn
        // guidance starts; grant PROJECTED. Soft — does not gate initial video.
        if ch == CH_CONTROL && id == MSG_NAV_FOCUS_REQUEST {
            eprintln!("[aa-headunit] <- NAV_FOCUS_REQUEST -> NavFocus(PROJECTED)");
            send_enc(link, tls, CH_CONTROL, MSG_NAV_FOCUS_NOTIFICATION, false, &proto::nav_focus_projected())?;
            continue;
        }

        // Phone-initiated shutdown: ack and end cleanly.
        if ch == CH_CONTROL && id == MSG_BYEBYE_REQUEST {
            eprintln!("[aa-headunit] <- BYEBYE_REQUEST -> BYEBYE_RESPONSE, ending");
            let _ = send_enc(link, tls, CH_CONTROL, MSG_BYEBYE_RESPONSE, false, &[]);
            break;
        }

        // Audio-focus handshake (control channel): the phone requests focus after
        // the SD response and waits for this notification before opening channels.
        if ch == CH_CONTROL && id == MSG_AUDIO_FOCUS_REQUEST {
            let req_type = proto::get_field_varint(&body, 1).unwrap_or(AUDIO_FOCUS_TYPE_RELEASE + 1);
            let state = if req_type == AUDIO_FOCUS_TYPE_RELEASE { AUDIO_FOCUS_STATE_LOSS } else { AUDIO_FOCUS_STATE_GAIN };
            eprintln!("[aa-headunit] <- AUDIO_FOCUS_REQUEST (type={req_type}) -> notification(state={state})");
            send_enc(link, tls, CH_CONTROL, MSG_AUDIO_FOCUS_NOTIFICATION, false, &proto::audio_focus_notification(state))?;
            continue;
        }

        // Channel-open arrives on every advertised channel (1/3/6/8) — always ACK OK.
        if id == MSG_CHANNEL_OPEN_REQUEST {
            eprintln!("[aa-headunit] <- CHANNEL_OPEN_REQUEST (ch {ch}) -> OK");
            send_enc(link, tls, ch, MSG_CHANNEL_OPEN_RESPONSE, true, &proto::channel_open_response_ok())?;
            continue;
        }

        // Media setup on any advertised sink (video ch3, audio ch6): reply
        // Config(READY). Video additionally needs VideoFocus(PROJECTED) to stream;
        // audio is configured then left idle (its media is ignored — see below).
        if id == MEDIA_SETUP {
            send_enc(link, tls, ch, MEDIA_CONFIG, false, &proto::media_config_ready())?;
            if ch == CH_VIDEO {
                eprintln!("[aa-headunit] <- MEDIA SETUP (video) -> Config(READY) + VideoFocus(PROJECTED)");
                send_enc(link, tls, CH_VIDEO, MEDIA_VIDEO_FOCUS_NOTIFICATION, false, &proto::video_focus_projected())?;
            } else {
                eprintln!("[aa-headunit] <- MEDIA SETUP (ch {ch}) -> Config(READY)");
            }
            continue;
        }

        match (ch, id) {
            (CH_SENSOR, SENSOR_REQUEST) => {
                let stype = proto::get_field_varint(&body, 1).unwrap_or(0);
                eprintln!("[aa-headunit] <- SENSOR_REQUEST (type={stype}) -> start-response");
                send_enc(link, tls, CH_SENSOR, SENSOR_RESPONSE, false, &proto::sensor_start_response_ok())?;
                if stype == SENSOR_TYPE_DRIVING_STATUS {
                    // The safety gate: report UNRESTRICTED or the phone won't project.
                    eprintln!("[aa-headunit]    -> SensorBatch DRIVE_STATUS_UNRESTRICTED");
                    send_enc(link, tls, CH_SENSOR, SENSOR_BATCH, false, &proto::sensor_batch_driving_unrestricted())?;
                } else if stype == SENSOR_TYPE_NIGHT_MODE {
                    send_enc(link, tls, CH_SENSOR, SENSOR_BATCH, false, &proto::sensor_batch_night(false))?;
                }
            }
            (CH_INPUT, INPUT_KEY_BINDING_REQUEST) => {
                eprintln!("[aa-headunit] <- KEY_BINDING_REQUEST -> OK");
                send_enc(link, tls, CH_INPUT, INPUT_KEY_BINDING_RESPONSE, false, &proto::key_binding_response_ok())?;
            }
            (CH_VIDEO, MEDIA_VIDEO_FOCUS_REQUEST) => {
                eprintln!("[aa-headunit] <- VIDEO_FOCUS_REQUEST -> VideoFocus(PROJECTED)");
                send_enc(link, tls, CH_VIDEO, MEDIA_VIDEO_FOCUS_NOTIFICATION, false, &proto::video_focus_projected())?;
            }
            (CH_VIDEO, MEDIA_START) => {
                session_id = proto::get_field_varint(&body, 1).unwrap_or(0) as i64;
                eprintln!("[aa-headunit] <- MEDIA START (session_id={session_id})");
            }
            (CH_VIDEO, MEDIA_CODEC_CONFIG) => {
                // Whole body is Annex-B config (SPS/PPS). Write verbatim, then ACK.
                file.write_all(&body)?;
                total_bytes += body.len();
                config_seen = true;
                eprintln!("[aa-headunit] <- CODEC_CONFIG ({} bytes) [{}]", body.len(), proto::summarize_h264(&body));
                send_enc(link, tls, CH_VIDEO, MEDIA_ACK, false, &proto::media_ack(session_id))?;
            }
            (CH_VIDEO, MEDIA_DATA) => {
                // [timestamp u64 BE][Annex-B]; strip the 8-byte timestamp.
                if body.len() < MEDIA_DATA_TS_LEN {
                    eprintln!("[aa-headunit] short MEDIA_DATA ({} bytes), skipping", body.len());
                } else {
                    let au = &body[MEDIA_DATA_TS_LEN..];
                    file.write_all(au)?;
                    total_bytes += au.len();
                    frames += 1;
                    if frames <= 3 || frames % 30 == 0 {
                        eprintln!("[aa-headunit] <- frame {frames} ({} bytes) [{}]", au.len(), proto::summarize_h264(au));
                    }
                }
                send_enc(link, tls, CH_VIDEO, MEDIA_ACK, false, &proto::media_ack(session_id))?;

                // Once video is flowing, optionally inject one tap (DOWN+UP) to
                // drive the AA UI and prove the input-uplink path end to end.
                if !tapped {
                    if let Some((x, y)) = tap {
                        if frames >= 8 {
                            let t = start.elapsed().as_nanos() as u64;
                            eprintln!("[aa-headunit] -> INPUT tap ({x},{y})");
                            send_enc(link, tls, CH_INPUT, INPUT_REPORT, false, &proto::input_report_touch(t, x, y, INPUT_ACTION_DOWN))?;
                            let t2 = start.elapsed().as_nanos() as u64;
                            send_enc(link, tls, CH_INPUT, INPUT_REPORT, false, &proto::input_report_touch(t2, x, y, INPUT_ACTION_UP))?;
                            tapped = true;
                        }
                    }
                }

                if frames >= max_frames {
                    eprintln!("[aa-headunit] reached max_frames={max_frames}, stopping");
                    break;
                }
            }
            (CH_VIDEO, MEDIA_STOP) => {
                eprintln!("[aa-headunit] <- MEDIA STOP");
                break;
            }
            (c, m) => {
                // Unmodeled control chatter (ping, other channels) — log and ignore.
                eprintln!("[aa-headunit] .. ignoring ch={c} msgid={m} ({} bytes)", body.len());
            }
        }
    }

    // Clean teardown: tell the phone we're leaving so it releases its head-unit
    // session. Without this it retains an "Already connected" state that crashes
    // its :projection process on our next connect.
    let _ = send_enc(link, tls, CH_CONTROL, MSG_BYEBYE_REQUEST, false, &proto::byebye_request());

    file.flush()?;
    eprintln!("\n[aa-headunit] capture complete: {frames} frames, {total_bytes} bytes -> {out_path}");
    if config_seen && frames > 0 {
        eprintln!("[aa-headunit] PHASE 1 SUCCESS: captured H.264 video from the phone (box-free).");
        eprintln!("[aa-headunit] play with: ffplay {out_path}   (or: ffmpeg -i {out_path} out.mp4)");
    } else {
        eprintln!("[aa-headunit] PHASE 1 INCOMPLETE: config_seen={config_seen} frames={frames}");
    }
    Ok(())
}
