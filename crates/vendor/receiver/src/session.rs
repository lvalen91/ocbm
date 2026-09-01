//! A/V session / data plane (Milestone B). The sans-IO [`crate::server::ControlServer`] decrypts the
//! control requests and delegates the session-bound ones (SETUP/RECORD/TEARDOWN) to a
//! [`SessionDelegate`]; the real implementation ([`AvSession`]) owns the actual sockets/threads.
//!
//! B1 (this): SETUP **phase 1** (session) — allocate + open the receiver's NTP **timing** UDP socket
//! and the **event** TCP listener, answer with their ports, and run the timing responder (required
//! before the iPhone streams). Phase-2 streams + the RTP/screen receive loops land in B2–B4.

use std::collections::HashMap;
use std::io::{Cursor, IoSlice, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use portable_atomic::{AtomicU64};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use plist::{Dictionary, Value};

/// A serialized empty binary-plist dictionary — the reference's reply body for a `POST /command` whose
/// handler produced no `outParams` (docs/carplay/03_SDK_GROUND_TRUTH.md §2). Falls back to an empty body only if serialization
/// somehow fails, which cannot happen for a literal empty dict. Serialized once and cached (the bytes
/// are constant); each caller gets its own clone.
fn empty_plist_dict() -> Vec<u8> {
    static CACHED: OnceLock<Vec<u8>> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            let mut buf = Vec::new();
            match plist::to_writer_binary(&mut buf, &Dictionary::new()) {
                Ok(()) => buf,
                Err(_) => Vec::new(),
            }
        })
        .clone()
}

use crate::forward::{adts_from_aac_lc, annexb_from_avcc, tag_voice};
use crate::stream::{decrypt_audio_aad, derive_stream_keys, MIN_AUDIO_PACKET};

/// Probe which HKDF input salts the DataStream(130) key schedule, and in which direction (docs/carplay/05_METADATA_AND_CONTROLS.md §1.4).
///
/// The A/V streams salt with `DataStream-Salt<streamConnectionID>`, but the RCS SETUP carries no
/// `streamConnectionID` (it logged `scid=0`) — `_DataStreamSessionSetup` in `CarPlaySDK.framework` reads
/// a `seed` instead, so `seed` is the natural candidate for the role `scid` plays for A/V. Rather than
/// guess, try every plausible (salt-id, direction) pair against the FIRST inbound frame and report which
/// one authenticates. ChaCha20-Poly1305 is authenticated, so a wrong key cannot produce a false positive.
///
/// Returns `(salt id, read_is_output, description)`, or `None` if nothing authenticated. The caller
/// needs the id and direction — not just the read key — so it can derive the matching WRITE key and
/// transmit on the same channel.
fn probe_datastream_keys(
    shared: &[u8],
    frame: &[u8],
    candidates: &[(&str, u64)],
) -> Option<(u64, bool, String)> {
    if frame.len() < 2 {
        return None;
    }
    let len = u16::from_le_bytes([frame[0], frame[1]]) as usize;
    let total = 2 + len + 16;
    if frame.len() < total {
        return None;
    }
    // Frame counter 0 — first frame of a fresh connection.
    let mut nonce = [0u8; 12];
    nonce[4..12].copy_from_slice(&0u64.to_le_bytes());
    for (label, id) in candidates {
        let sk = derive_stream_keys(shared, *id);
        for (is_output, key) in [(true, sk.output), (false, sk.input)] {
            let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
            if cipher
                .decrypt(
                    Nonce::from_slice(&nonce),
                    Payload { msg: &frame[2..total], aad: &frame[0..2] },
                )
                .is_ok()
            {
                let dir = if is_output { "output" } else { "input" };
                return Some((
                    *id,
                    is_output,
                    format!("DataStream-Salt{id} ({label}), read={dir}"),
                ));
            }
        }
    }
    None
}

/// Drain every COMPLETE RCS message from the reassembly buffer.
///
/// One RCS message can span several crypto frames. The DataStream frames at 16384 bytes
/// (`NetSocketChaCha20Poly1305Configure`), while our SYN advertises `MaxPacketSize = 0xFFFF`, so the
/// phone may send a 65535-byte iAP2 link packet — a ~65567-byte RCS message, five frames' worth.
///
/// An earlier revision parsed each decrypted frame in isolation, so any message larger than one frame
/// failed `declared == pt.len()` and was dropped. Observed consequence: album artwork Setup and the
/// final small fragment parsed, the two 65525-byte data fragments did not, and the transfer was
/// acknowledged as complete while holding a truncated JPEG.
fn drain_rcs(buf: &mut Vec<u8>, is_iap: bool) {
    /// Refuse to buffer more than this. `totalLength` is a u32 from the peer; without a ceiling a
    /// corrupt length would let a remote party drive allocation on a 123 MB box.
    const MAX_RCS: usize = 256 * 1024;
    loop {
        if buf.len() < 4 {
            return;
        }
        let declared = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if !(crate::datastream::HEADER_LEN..=MAX_RCS).contains(&declared) {
            eprintln!(
                "[datastream] implausible RCS length {declared} — dropping {} B buffered",
                buf.len()
            );
            buf.clear();
            return;
        }
        if buf.len() < declared {
            return; // wait for the remaining frames
        }
        // Handle the message in place, then drain — no per-message copy (the old
        // `buf.drain(..).collect()` copied every RCS message just to hand it to a `&[u8]` sink).
        log_datastream_frame(&buf[..declared], is_iap);
        buf.drain(..declared);
    }
}

/// Handle one complete RCS message (docs/carplay/05_METADATA_AND_CONTROLS.md).
///
/// Strips the 32-byte RCS envelope and routes the iAP2 link frame into the tunnel state machine. An
/// earlier revision only LOGGED, because the payload shape was an open question; it is now settled from
/// real bytes — the envelope is `APMediaDataControlServer`'s header and the payload is a verbatim iAP2
/// link packet (see `datastream.rs`). Only the iAP client type is routed; the other SIX RCS client
/// types (LogTransfer, VehicleDataProtocol×2, UrlFling, OverlayUI, SenderSettingsData) are logged and
/// dropped. The gate is an allowlist of one, so the three types this project did not know about until
/// 2026-07-30 were always correctly excluded — see docs/carplay/05_METADATA_AND_CONTROLS.md §1.2 for the full table.
fn log_datastream_frame(pt: &[u8], is_iap: bool) {
    // Strip the 32-byte RCS envelope and hand the iAP2 link frame to the existing state machine, which
    // already implements DETECT/SYN/ACK, the MFi cert+challenge exchange, Identify, and the metadata
    // subscribes for the wired and BT links.
    match crate::datastream::unwrap(pt) {
        Some(f) => {
            let ctrl = f.payload.get(4).copied().unwrap_or(0);
            // Per-frame hex dump gated behind CARPLAY_EVENTS_LOG (R6) — this fires for every inbound
            // RCS frame. The NOT-FF-5A anomaly line below stays UNgated: it is load-bearing (a payload
            // that is not an iAP2 link frame is exactly the surprise this log exists to catch).
            if crate::events::events_log() {
                eprintln!(
                    "[datastream] RX kind={} type={} iAP2 {} B ctrl={ctrl:#04x}{} hex={:02x?}",
                    f.kind_str(),
                    f.msg_type_str(),
                    f.payload.len(),
                    if f.payload.starts_with(&[0xFF, 0x5A]) { "" } else { "  (NOT FF 5A)" },
                    &f.payload[..f.payload.len().min(48)]
                );
            } else if !f.payload.starts_with(&[0xFF, 0x5A]) {
                eprintln!(
                    "[datastream] RX {} B NOT FF 5A-framed (ctrl={ctrl:#04x})",
                    f.payload.len()
                );
            }
            // docs/carplay/05_METADATA_AND_CONTROLS.md: `'sync'` obliges us to answer with `'rply'` echoing the message id. Every frame
            // observed so far is `'asyn'`, which owes nothing — but if iOS ever sets
            // `sendMessageWithoutReply=false` our silence would block its send until a 10 s timeout, so
            // surface it loudly rather than letting it look like a protocol mystery later.
            //
            // THE REPLY SHAPE IS KNOWN, so whoever implements this does not have to derive it.
            // From `_controlServer_sendResponseInternal` in CarPlaySDK.framework (authority #1),
            // header build at +0x100..+0x134 of that function:
            //     +0x00  u32 BE   total length = 0x20
            //     +0x04  fourcc   'rply'
            //     +0x08  u64      reserved, zero
            //     +0x10  u32      messageType = 0   <-- ZERO, *not* 'cmnd'
            //     +0x14  u32 BE   the message id being replied to
            //     +0x1c  u32 BE   OSStatus
            // Note the messageType-zero detail: `datastream::wrap()` hardcodes `KIND_ASYN` and
            // `messageID = 0`, so reusing it verbatim would emit the wrong header.
            // Apple picks 'sync' over 'asyn' in `_controlServer_sendRequestInternal` exactly when a
            // completion block is attached, which is what `needs_reply()` already models.
            //
            // ⚠️ DO NOT IMPLEMENT THIS FROM THE SHAPE ABOVE ALONE — two disassemblies CONFLICT and the
            // conflict is unresolved (2026-08-11):
            //   * `_controlServer_sendResponseInternal` (CarPlaySDK, the ACCESSORY side) writes
            //     messageType = 0 at +0x10 for a reply.
            //   * `_apEndpointRemoteControlSession_startMessageHandling` (iOS 27 `AirPlaySender`) is
            //     documented in `datastream.rs`'s module header as the phone's ONLY RCS inbound
            //     dispatch, and it accepts `'cmnd'`/`'died'` and SILENTLY DROPS everything else —
            //     which is what an earlier revision of this code learned the hard way by stamping
            //     `'comm'` on TX and watching the link sit in `Pending` forever.
            // Both cannot be true as stated. The likeliest reconciliation is that replies are matched
            // to a pending request by messageID BEFORE that command dispatcher runs, so the filter
            // never sees them — but that is a hypothesis, not a reading.
            // Cost of guessing wrong is asymmetric: today an unanswered 'sync' stalls ONE phone send
            // until its timeout, whereas a malformed accessory frame is what previously wedged the
            // whole link. Nothing has ever been observed sending 'sync' on this stream, so the
            // correct move is to leave this unimplemented until someone establishes which phone-side
            // path consumes a 'rply' — not to ship the header above and hope.
            if f.needs_reply() {
                eprintln!(
                    "[datastream] *** inbound 'sync' (id={:#x}) expects a 'rply' — NOT IMPLEMENTED ***",
                    f.message_id
                );
            }
            if !is_iap {
                eprintln!("[datastream] non-iAP channel payload — logged only, not routed to iap_tunnel");
                return;
            }
            if !crate::iap_tunnel::handle_inbound(f.payload) {
                // Mirror the control-channel arm: `handle_inbound` returns false both for "no session"
                // and "not link-framed", and the second case is the deliberate bare-payload fallback.
                crate::events::dispatch_iap_tunnel_message(f.payload);
            }
        }
        None => {
            let ascii: String = pt
                .iter()
                .take(48)
                .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
                .collect();
            eprintln!(
                "[datastream] PLAINTEXT {} B [envelope did NOT parse] ascii={ascii:?} hex={:02x?}",
                pt.len(),
                &pt[..pt.len().min(64)]
            );
        }
    }
}

/// Monotonic milliseconds since the first call (process-lifetime base) — one shared clock for the
/// activity-idle watchdog, consistent across the control loop and the A/V stream threads. `0` is
/// reserved to mean "no activity yet" (the base is set at process start, so a real stamp is never 0).
pub fn now_ms() -> u64 {
    static BASE: OnceLock<Instant> = OnceLock::new();
    (BASE.get_or_init(Instant::now).elapsed().as_millis() as u64).max(1)
}

/// Where the phone's identity is published for ocbmd to mirror to the host app.
///
/// A FILE, deliberately: it is the same mechanism `/tmp/pairing_code`, `/tmp/bt_phase` and
/// `/tmp/phone_present` already use to cross the airplayd -> ocbmd boundary, and ocbmd already runs a
/// change-detecting tick over each. Adding a socket seam for one small once-per-session fact would be
/// a new failure mode for no gain.
pub const PHONE_IDENT_FILE: &str = "/tmp/phone_identity";

/// Publish who the phone is, from its phase-1 SETUP plist.
///
/// Every field is Apple's own key, lifted verbatim (`AirPlayReceiverServer.c:3213` reads `name`
/// beside `deviceID`/`macAddress`/`sessionUUID`). `name` is what the user typed in Settings ->
/// General -> About -> Name; `deviceID` is the BR/EDR MAC, which is what makes this joinable against
/// the bonded list in MGMT_INFO — it is the only thing on the wire that says WHICH bonded phone is
/// the live one.
///
/// Written whole with a rename so ocbmd can never read a half-written file, and skipped entirely
/// when the plist carries no name (nothing useful to say, and clobbering a good value with an empty
/// one would make the host forget a phone it already knows).
fn publish_phone_identity(d: &Dictionary) {
    let get = |k: &str| d.get(k).and_then(|v| v.as_string()).unwrap_or("");
    let name = get("name");
    if name.is_empty() {
        return;
    }
    // Hand-built JSON: the box crates carry no serializer, the field set is fixed, and the values are
    // short ASCII-ish strings from Apple. Quotes and backslashes are still escaped — a device name is
    // user-supplied text and `Owner"s iPhone` must not produce a document ocbmd cannot forward.
    let esc = |v: &str| v.replace('\\', "\\\\").replace('"', "\\\"");
    let json = format!(
        r#"{{"name":"{}","deviceID":"{}","model":"{}","osName":"{}","osVersion":"{}"}}"#,
        esc(name),
        esc(get("deviceID")),
        esc(get("model")),
        esc(get("osName")),
        esc(get("osVersion")),
    );
    let tmp = format!("{PHONE_IDENT_FILE}.tmp");
    if std::fs::write(&tmp, json.as_bytes())
        .and_then(|_| std::fs::rename(&tmp, PHONE_IDENT_FILE))
        .is_ok()
    {
        eprintln!("[session] phone identity: {json}");
    }
}

/// Handles the session-bound control requests after pair-verify (the encrypted ones).
pub trait SessionDelegate: Send {
    /// Called once when pair-verify succeeds, handing over the ECDH shared secret (stream/event keys).
    fn on_paired(&mut self, _shared_secret: [u8; 32]) {}
    /// Handle a SETUP request (phase 1 = session, phase 2 = streams). Returns the response plist.
    fn setup(&mut self, request_plist: &[u8]) -> Vec<u8>;
    /// Handle RECORD (start streaming). Returns an optional response body (usually empty).
    fn record(&mut self) -> Vec<u8> {
        Vec::new()
    }
    /// Handle a POST /command (a `{type, params}` plist). Returns the response plist body.
    fn command(&mut self, _request_plist: &[u8]) -> Vec<u8> {
        Vec::new()
    }
    /// Tear the session down. The request plist may carry a `streams` array (partial teardown of only
    /// those streams — the session stays alive) or none (full teardown of the whole session).
    fn teardown(&mut self, _request_plist: &[u8]) {}

    /// Handle to the last-A/V-activity timestamp (monotonic ms via [`now_ms`]) for the idle watchdog,
    /// if this delegate runs a data plane. `None` (default) ⇒ no A/V activity tracked yet.
    fn last_activity(&self) -> Option<Arc<AtomicU64>> {
        None
    }
}

/// No-op delegate (default / tests): returns empty responses.
pub struct NoSession;
impl SessionDelegate for NoSession {
    fn setup(&mut self, _req: &[u8]) -> Vec<u8> {
        Vec::new()
    }
}

/// The real A/V session. Forwards decoded streams to carlink_linux's IPC ports.
pub struct AvSession {
    shared: Option<[u8; 32]>,
    /// Whether the control plane (timing / event / keepAlive) is already set up.
    ///
    /// Apple's reference makes phase-1 SETUP idempotent: `AirPlayReceiverServer.c:3227` creates a
    /// session only `if( !inCnx->session )`, and `_ControlSetup` guards itself with
    /// `require_action( !inSession->controlSetup, exit2, err = kAlreadyInitializedErr )`
    /// (`AirPlayReceiverSession.c:1761`), cleared only in `_ControlTearDown`. So a repeat phase-1
    /// SETUP binds nothing, spawns nothing, and leaves the ports the phone is already using valid.
    ///
    /// We used to re-bind and hand iOS NEW ports, which is both a divergence from the reference and
    /// the reason a repeat SETUP stranded threads. If iOS keeps time-syncing to the ports from the
    /// first SETUP — as the reference implies it may — re-binding silently breaks that sync.
    control_setup: AtomicBool,
    timing_port: u16,
    event_port: u16,
    keepalive_port: u16,
    /// Event-channel listener, opened at SETUP but ACCEPTED at RECORD (the C accepts it in
    /// `_ControlStart` and holds the RECORD 200 until accept completes — the iPhone's cue to proceed).
    event_listener: Option<TcpListener>,
    /// The iPhone's control-connection peer address — the mic-uplink RTP destination. We keep the
    /// FULL `SocketAddr` (not just the `IpAddr`) so an IPv6 link-local `scope_id` (the interface,
    /// e.g. `%en10` on the wired NCM link) is preserved: sending RTP to a `fe80::…` destination with
    /// `scope_id == 0` fails (`EINVAL` / no route to host). Only the port is overridden per stream.
    peer_addr: Option<std::net::SocketAddr>,
    /// Session-liveness flag shared with every spawned stream thread. Cleared on TEARDOWN or when this
    /// `AvSession` is dropped (connection closed / abrupt drop); each thread polls it on its socket
    /// read-timeout and exits — so stream threads never outlive their session (no per-session leak).
    alive: Arc<AtomicBool>,
    /// Last A/V-data timestamp (monotonic ms via [`now_ms`]), stamped by the audio/screen threads on
    /// each packet/frame. The control-loop idle watchdog reads it so an active session with flowing A/V
    /// but a quiet control channel is NOT falsely torn down. `0` until the first A/V data arrives.
    activity: Arc<AtomicU64>,
    /// Per-A/V-stream liveness flags keyed by stream `type` (110 screen, 111 cluster, 100-102 audio).
    /// Each screen/audio thread polls ITS OWN flag (not the session-wide `alive`) so a PARTIAL TEARDOWN
    /// can stop just the named streams, and a re-SETUP of an already-live type stops the prior thread
    /// first — both fix the thread/socket accumulation that a session-wide-only flag couldn't (#406/#413).
    /// Behind a `Mutex` because `reset()` (`&self`, also called from `Drop`) must flip them all.
    /// Per-stream liveness flags, keyed by `(stream type, channelID)`.
    ///
    /// The key used to be the bare type — but ALL four RemoteControlSession channels are stream type
    /// 130, distinguished only by `clientTypeUUID`. A second RCS SETUP therefore evicted the live iAP
    /// channel's flag; its thread exited at the next poll, dropped the listener whose `dataPort` the
    /// response had already advertised, and called `datastream::clear_if`, killing the outbound sink.
    /// Outbound then degraded silently to the legacy `POST /command` carrier while inbound died —
    /// working-looking, and completely broken. Unreachable today (we advertise neither `logTransfer`
    /// nor `vehicleStateProtocol`), and armed the moment either is enabled for nav/cluster work.
    ///
    /// `channelID` and not the allocated `streamID`: `streamID` is a fresh counter per SETUP, so
    /// keying on it would break re-SETUP superseding and reintroduce the #406/#413 thread
    /// accumulation on the retry path — which one archived session shows firing 15 times.
    /// A/V streams pass an empty string and keep their supersede-by-type behaviour exactly.
    av_streams: Mutex<HashMap<(i64, String), Arc<AtomicBool>>>,
}

/// How often stream threads wake from a blocking socket read to check the session-liveness flag.
const SHUTDOWN_POLL: Duration = Duration::from_millis(500);

/// A socket read/accept that returned because its timeout elapsed (not a real error).
fn is_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

impl Default for AvSession {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a control-plane binary plist from the phone, bounded.
///
/// `plist` 1.10 is safe on its own terms — it bounds allocations and cycle-checks refs — but it
/// enforces no NESTING-DEPTH limit, and `plist::Value`'s generated `Drop` is recursive. A body that
/// is a chain of one-element arrays therefore parses fine and blows the stack when the tree is
/// dropped. Nine bytes per level, and the process dies with `fatal runtime error: stack overflow` —
/// which under `panic = "abort"` is a dead daemon needing a physical power cycle.
///
/// Measured on an 8 MiB stack (the main thread, where `serve_connection` runs): 120 000 levels
/// survives, 140 000 aborts — a 1.26 MB body. `rtsp::message::MAX_BODY` is 256 KiB
/// (rtsp/src/message.rs:43; an earlier revision of this comment said 16 MiB, which was the OLD
/// pre-hardening value), so a body big enough to trigger the abort can no longer arrive over the
/// control connection at all — this cap is defense in depth for any other caller.
///
/// A depth limit is not expressible against `plist`'s public API, so the byte cap is the lever. Real
/// control plists are single-digit KB: the largest this project has observed is the 1482 B SUBSCRIBE
/// config and a 791 B SETUP.
fn parse_control_plist(body: &[u8]) -> Option<Value> {
    const MAX_CONTROL_PLIST: usize = 256 * 1024;
    if body.len() > MAX_CONTROL_PLIST {
        eprintln!(
            "[session] control plist {} B exceeds the {MAX_CONTROL_PLIST} B cap — refusing to parse \
             (deep-nesting stack-overflow guard)",
            body.len()
        );
        return None;
    }
    Value::from_reader(Cursor::new(body)).ok()
}

impl AvSession {
    pub fn new() -> Self {
        Self {
            shared: None,
            control_setup: AtomicBool::new(false),
            timing_port: 0,
            event_port: 0,
            keepalive_port: 0,
            event_listener: None,
            peer_addr: None,
            alive: Arc::new(AtomicBool::new(true)),
            activity: Arc::new(AtomicU64::new(0)),
            av_streams: Mutex::new(HashMap::new()),
        }
    }

    /// Allocate a fresh liveness flag for an A/V stream of type `ty`, first stopping any prior thread
    /// still registered for that type (a re-SETUP replaces its stream) — this is what prevents the
    /// thread/socket accumulation of #406/#413. Uses `&self` (interior `Mutex`) so callers can pass the
    /// returned flag alongside other `self.*.clone()` borrows without a borrow-checker conflict.
    /// Supersede-by-type: a new stream of this type stops the previous one. Correct for A/V, where
    /// there is one screen and one alt-screen.
    fn stream_flag(&self, ty: i64) -> Arc<AtomicBool> {
        self.stream_flag_keyed(ty, String::new())
    }

    /// Supersede by `(type, channel)`. Channels of the same type with DIFFERENT ids coexist; the same
    /// id re-SETUP still supersedes, which is what the observed RCS retry storm needs.
    fn stream_flag_keyed(&self, ty: i64, channel: String) -> Arc<AtomicBool> {
        let mut m = crate::plock(&self.av_streams);
        if let Some(prev) = m.remove(&(ty, channel.clone())) {
            prev.store(false, Ordering::Release); // stop the superseded stream's thread + drop its socket
        }
        let flag = Arc::new(AtomicBool::new(true));
        m.insert((ty, channel), flag.clone());
        flag
    }

    /// Stop all spawned stream threads and reset per-session global state (event channel, mic uplink,
    /// A/V sinks). Idempotent — safe to call from both TEARDOWN and `Drop` (abrupt connection loss).
    fn reset(&self) {
        self.alive.store(false, Ordering::Release);
        // Clearing this is what makes the next phase-1 SETUP bind again — the reference clears its
        // `controlSetup` in `_ControlTearDown` and nowhere else.
        self.control_setup.store(false, Ordering::Release);
        // Stop every per-stream A/V thread too — they poll their own flag, not the session `alive`, so
        // a full teardown / Drop must flip them all or they'd outlive the session (#406/#413).
        {
            let mut m = crate::plock(&self.av_streams);
            for (_, flag) in m.drain() {
                flag.store(false, Ordering::Release);
            }
        }
        #[cfg(feature = "mic-uplink")]
        crate::uplink::clear();
        crate::events::clear();
        clear_sinks();
    }

    /// Record the iPhone's control-connection peer address (WITH its IPv6 scope) so the type-100 mic
    /// uplink can target it on the correct interface. Pass the full `peer_addr()`, not `.ip()`.
    pub fn set_peer_addr(&mut self, addr: std::net::SocketAddr) {
        self.peer_addr = Some(addr);
    }

    /// Read back the recorded peer address (see [`Self::set_peer_addr`]). Added for the app-driven
    /// SETUP relay: `relay::RemoteSession` wraps this session and puts the peer in its RS_OPEN ctx
    /// bplist. Pure accessor — the one visibility addition the relay needed; no functional change.
    pub fn peer_addr(&self) -> Option<std::net::SocketAddr> {
        self.peer_addr
    }

    fn setup_phase1(&mut self, keep_alive: bool) -> Vec<u8> {
        // Idempotent, per Apple's reference (see `control_setup`). A repeat phase-1 SETUP with no
        // intervening TEARDOWN returns the SAME ports rather than re-binding: the phone is already
        // using them, and re-binding both breaks that and strands the previous threads.
        let reuse = self.control_setup.load(Ordering::Acquire);
        if reuse {
            eprintln!(
                "[session] SETUP phase1 repeated with no TEARDOWN — reusing timingPort {} \
                 eventPort {} keepAlivePort {} (reference: _ControlSetup is guarded by controlSetup)",
                self.timing_port, self.event_port, self.keepalive_port
            );
        }
        // Re-arm the session-liveness flag (#119): a full TEARDOWN (or a prior aborted SETUP) leaves
        // `alive=false`, and without this a re-SETUP on the SAME control connection would spawn session
        // threads (timing/event/keepAlive) that see `false` and exit immediately. A fresh SETUP means the
        // session is live again. (Per-stream A/V flags are minted fresh in `stream_flag`, so they need no
        // re-arm.)
        // Mint a FRESH flag rather than re-arming the shared one. Re-arming leaked a thread + fd per
        // TEARDOWN->SETUP cycle: `reset()` stores false, then this line stored true again, and any thread
        // parked inside its poll interval (100-500 ms) across that window never observed the false and
        // survived into the new session bound to the OLD sockets. On a single-core 528 MHz box a thread
        // spanning a sub-100 ms window is the norm, not a rare race. Replacing the Arc means every
        // previously-spawned thread keeps a permanently-false flag and exits within one poll interval;
        // every spawn site clones `self.alive` at spawn time, so new threads get the new one. This is the
        // pattern `stream_flag` already uses for per-stream flags, which are correspondingly immune.
        // Flip the OUTGOING flag before replacing it. Every writer (`reset`, `Drop`) acts on
        // `self.alive`, so once it is replaced the old Arc has no writer left for the process
        // lifetime. On the TEARDOWN -> SETUP path `reset()` already stored false and this is a no-op;
        // on a phase-1 SETUP with NO intervening TEARDOWN it reaps the `spawn_timing` and
        // `spawn_keepalive` threads and their UDP fds instead of stranding them permanently — proven
        // by test: strong_count was 3 after a full TEARDOWN and Drop. Also reaps them when a re-SETUP
        // fails at the bind below. This is what `stream_flag` already does, and what the comment
        // above always claimed this did.
        if !reuse {
            self.alive.store(false, Ordering::Release);
            self.alive = Arc::new(AtomicBool::new(true));
        }
        // NB: every session socket below binds `[::]:0` (IPv6), NOT `0.0.0.0` (IPv4). Wired CarPlay
        // runs over IPv6 link-local on the NCM link, so the iPhone connects to these ports at the
        // receiver's `fe80::…` address; an IPv4-only socket silently refuses those connections and the
        // iPhone tears the session down right after SETUP-phase1. `[::]` is dual-stack on macOS/Linux
        // (also accepts IPv4-mapped peers), so this is correct for both wired-IPv6 and wireless-IPv4.
        // Timing: bind a UDP socket and run the NTP-like responder. Bind failures are handled, NOT
        // `.expect()`-panicked (#139): the whole workspace builds with `panic="abort"`, so a panic on
        // this single thread would take DOWN THE ENTIRE airplayd — every other connection with it — over
        // a transient port-exhaustion. Instead fail just this SETUP (return an empty response; the iPhone
        // tears the one session down) and keep the daemon serving.
        if !reuse {
            let timing = match UdpSocket::bind("[::]:0") {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[session] bind timing UDP failed: {e} — failing this SETUP");
                    return Vec::new();
                }
            };
            self.timing_port = timing.local_addr().map(|a| a.port()).unwrap_or(0);
            spawn_timing(timing, self.alive.clone());
        }

        // Event channel: open the TCP listener now, but ACCEPT it at RECORD (not here) — the C's
        // ordering. Holding the accept until RECORD is what cues the iPhone to proceed.
        if !reuse {
            let events = match TcpListener::bind("[::]:0") {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("[session] bind event TCP failed: {e} — failing this SETUP");
                    return Vec::new();
                }
            };
            self.event_port = events.local_addr().map(|a| a.port()).unwrap_or(0);
            self.event_listener = Some(events);
        }

        let mut d = Dictionary::new();
        d.insert(
            "timingPort".into(),
            Value::Integer((self.timing_port as i64).into()),
        );
        d.insert(
            "eventPort".into(),
            Value::Integer((self.event_port as i64).into()),
        );

        // KeepAlive: the C returns keepAlivePort iff the request set keepAliveLowPower (it did).
        // Open a UDP socket for the low-power keep-alive beacons and advertise its port.
        if keep_alive && reuse && self.keepalive_port > 0 {
            d.insert(
                "keepAlivePort".into(),
                Value::Integer((self.keepalive_port as i64).into()),
            );
        } else if keep_alive {
            // Non-fatal bind (#139): keepAlive is optional low-power signalling, so on a bind failure
            // just advertise no port rather than failing the SETUP or panicking the daemon.
            match UdpSocket::bind("[::]:0") {
                Ok(ka) => {
                    self.keepalive_port = ka.local_addr().map(|a| a.port()).unwrap_or(0);
                    // spawn_keepalive (not spawn_udp_drain) so beacons feed the idle watchdog (#1130).
                    spawn_keepalive(ka, self.alive.clone(), self.activity.clone());
                    d.insert(
                        "keepAlivePort".into(),
                        Value::Integer((self.keepalive_port as i64).into()),
                    );
                }
                Err(e) => eprintln!(
                    "[session] bind keepAlive UDP failed: {e} — no keepAlivePort advertised"
                ),
            }
        }

        // SETUP-phase1 negotiated-feature echo. The SETUP *request* proposes `features:[…]`; the
        // receiver reads the accessory's echo back from **`enabledFeatures`** (the response key,
        // `_AirPlayCopyAccessoryEnabledFeatures`) — NOT `features`. Capture-confirmed: the genuine CCPA
        // session's request uses `features` (ttylog:4463) and its response uses `enabledFeatures`
        // (ttylog:4500) — see CAPTURE_VALIDATION_03 + facet 06. Echoing under the old `features` key
        // meant iOS read an EMPTY enabledFeatures set, so this lever never actually connected.
        //
        // Capability-honest default = EMPTY `[]`. The genuine CCPA echoes `["altScreen","viewAreas"]`
        // but it *owns* a type-111 display; ncm is a flat single-display receiver, so its honest set is
        // empty. The prior `["mainBuffered","videoPlayback"]` default is capture-refuted (media ran as
        // realtime PCM with no `mainBuffered`; `videoPlayback` has no receiver bit). Behavior-preserving
        // vs today (iOS effectively read an empty set before), now correctly keyed and honest. The
        // `CARPLAY_HEVC` env lever is kept for future experimentation (echo `["hevc"]`), now under the
        // correct key. `altScreen`/`viewAreas` ARE echoed below — but conditionally, and only now that
        // their backing `/info` structures exist (the 2nd display / the displays[].viewAreas). They stay
        // out of the default (empty) set so a plain session is byte-identical to before.
        let mut feats: Vec<Value> = Vec::new();
        if crate::levers::hevc() {
            feats.push(Value::String("hevc".into()));
        }
        // AltScreen must survive the SETUP feature-intersection for iOS to offer the type-111 cluster
        // stream (docs/carplay/06_AV_PIPELINE.md). Gated behind the same lever as the /info alt display.
        if crate::levers::altscreen() {
            feats.push(Value::String("altScreen".into()));
        }
        // viewAreas: iOS honors an inset `safeArea` (from /info displays[].viewAreas) ONLY when this
        // feature is negotiated here. The genuine CCPA echoes ["altScreen","viewAreas"]; the backing
        // /info structure is always present (info.rs::view_areas), so echoing is safe — the teardown
        // risk is the reverse (echoing a feature whose /info structure is missing). Gated on a real
        // inset / explicit enable via the viewAreas lever so full-bleed sessions stay byte-identical.
        // cornerMasks carries its per-view flag INSIDE the viewAreas structure, so negotiate viewAreas
        // whenever cornerMasks is on (belt-and-suspenders so the phone fully processes the view desc).
        if crate::levers::viewareas() || crate::levers::cornermasks() {
            feats.push(Value::String("viewAreas".into()));
        }
        // cornerMasks (Phase 1 experiment): the master enable. Requires the per-view `cornerMasks: true`
        // flag in /info (info.rs::view_areas) on ≥1 view — set on the main view under the same lever.
        // iOS logs `displayCornerMasksEnabled` when it reads this back (watch accessoryd).
        if crate::levers::cornermasks() {
            feats.push(Value::String("cornerMasks".into()));
        }
        // logTransfer: SETUP-response side of the `logTransferInfo` /info declaration — same
        // both-sides-present rule as hevc/iAPChannel (docs/carplay/04_CAPABILITIES_AND_CONFIG.md). carkitd pairs the token with
        // `enableCarPlayLoggingDataChannel` and logs `loggingDataChannel = 1` when it reads the echo.
        if crate::levers::logtransfer() {
            feats.push(Value::String("logTransfer".into()));
        }
        // mainBuffered (docs/carplay/04_CAPABILITIES_AND_CONFIG.md Phase A; docs/carplay/04_CAPABILITIES_AND_CONFIG.md B4) — config-primary via the same lever as the
        // /info mainBufferedInfo emission (armed per connection from the pushed
        // `enablesMainBufferedAudio`, app default OFF; env only as app-less bench fallback). SETUP-
        // response side of the declaration; iOS reads it back via
        // carEndpoint_readSupportedFeaturesFromSetupResponseAndNotify (logs the negotiated
        // buffered-audio state). No buffered stream is served; an inbound MainBuffered stream SETUP
        // is omitted below like any unimplemented stream. REGRESSION NOTE: if iOS accepts this and
        // moves media to a buffered stream, media audio would go silent — hence opt-in, never
        // default-on in the app.
        if crate::levers::mainbuffered() {
            feats.push(Value::String("mainBuffered".into()));
        }
        // iAPChannel: must be echoed here (the SETUP-response side of the feature-intersection gate)
        // to match `iAPChannelInfo` in `/info` (info.rs) — see the comment there for why both sides are
        // required. Same env gate as the tunnel send (events.rs) and the /info advertisement.
        if std::env::var("CARPLAY_WIRELESS_METADATA").is_ok() {
            feats.push(Value::String("iAPChannel".into()));
        }
        // sessionManagement: SETUP-response side of the sessionManagementInfo /info declaration
        // (info.rs) — both sides required, same both-sides-present rule as iAPChannel above. Its OWN
        // env var (docs/wireless/00_WIRELESS_CARPLAY.md #2.1), not shared with CARPLAY_WIRELESS_METADATA — see info.rs's comment.
        if std::env::var("CARPLAY_SESSION_MGMT").is_ok() {
            feats.push(Value::String("sessionManagement".into()));
        }
        d.insert("enabledFeatures".into(), Value::Array(feats));

        eprintln!(
            "[session] SETUP phase1 → timingPort {} eventPort {} keepAlivePort {}",
            self.timing_port, self.event_port, self.keepalive_port
        );
        // Control plane is up. Cleared only by `reset()` (full TEARDOWN / Drop), matching the
        // reference's `_ControlTearDown`.
        self.control_setup.store(true, Ordering::Release);
        let mut buf = Vec::new();
        Value::Dictionary(d)
            .to_writer_binary(&mut buf)
            .expect("plist");
        buf
    }

    /// SETUP phase 2 (streams). For each requested stream, open its data socket and answer with the
    /// allocated `dataPort`. Screen (type 110, H.264) is a TCP listener the iPhone connects to;
    /// audio (100/101/102) is UDP (B4). Faithful to the C `_ScreenSetup`/`_*AudioSetup` responses.
    fn setup_phase2(&mut self, req: &Dictionary) -> Vec<u8> {
        let shared = self.shared.unwrap_or([0u8; 32]);
        let mut resp = Vec::new();
        if let Some(streams) = req.get("streams").and_then(|v| v.as_array()) {
            for s in streams {
                let Some(sd) = s.as_dictionary() else {
                    continue;
                };
                let ty = sd
                    .get("type")
                    .and_then(|v| v.as_signed_integer())
                    .unwrap_or(0);
                // DIAGNOSTIC (mic uplink): dump the FULL stream dict for audio-range streams so we can see
                // exactly what iOS sends — in particular whether it EVER sets `input` (the mic-uplink
                // request) and on which `audioType`. If `input` never appears, the head unit isn't
                // advertising mic capability in a form iOS accepts (an /info audioFormats problem), not a
                // pipeline problem. Remove once the uplink negotiation is confirmed.
                if (100..=112).contains(&ty) {
                    let mut parts: Vec<String> = Vec::new();
                    for (k, v) in sd.iter() {
                        let vs = v
                            .as_boolean()
                            .map(|b| b.to_string())
                            .or_else(|| v.as_unsigned_integer().map(|n| n.to_string()))
                            .or_else(|| v.as_signed_integer().map(|n| n.to_string()))
                            .or_else(|| v.as_string().map(|s| s.to_string()))
                            .unwrap_or_else(|| "<complex>".to_string());
                        parts.push(format!("{k}={vs}"));
                    }
                    eprintln!(
                        "[session] >>> SETUP stream dict (type {ty}): {}",
                        parts.join(", ")
                    );
                }
                // streamConnectionID may be stored signed or unsigned in the plist — accept both.
                let scid = sd
                    .get("streamConnectionID")
                    .and_then(|v| {
                        v.as_unsigned_integer()
                            .or_else(|| v.as_signed_integer().map(|s| s as u64))
                    })
                    .unwrap_or(0);
                // scid 0 is INVALID for the A/V streams, not a benign default. Apple hard-fails those:
                // `AirPlayReceiverSession.c:4343` requires a non-zero `streamConnectionID` and returns
                // `kVersionErr` otherwise. It is the HKDF salt input — a zero scid derives the WRONG
                // key, so every frame on that stream decrypts to garbage with no error reported
                // anywhere. Skipping is strictly better than proceeding into guaranteed corruption, and
                // it turns an otherwise silent failure into one line. Never observed on hardware for
                // A/V: our own capture carries a real non-zero scid
                // (docs/ops/captures/2026-07-24_airplayd_phase12_session.log:31).
                //
                // THIS IS AN ALLOWLIST, AND IT MUST STAY ONE. The condition it encodes is "scid is
                // this stream's HKDF salt", which is true of the A/V types and nothing else, so it may
                // only ever name types whose key derivation actually consumes scid (`spawn_screen` /
                // `spawn_audio` -> `derive_stream_keys(&shared, scid)`). A type absent from this list
                // is NOT thereby blessed: it falls through to its own arm, and an unimplemented one is
                // omitted at the `_` arm with a named diagnostic — which is what Apple's own receiver
                // does (`AirPlayReceiverSession.c:947-949`) and is the fail-safe default.
                //
                // Written as a deny-list (`ty != 130`) this guard applied an A/V-only precondition to
                // every stream type Apple adds next, and that is not a hypothetical: it is exactly how
                // the 2026-07-31 -> 08-10 wireless-metadata outage happened. Added in 5ce9d1c against
                // ALL types, it silently killed the metadata plane for ten days — iOS SETUPs the RCS
                // iAP channel with NO `streamConnectionID` key at all, so `unwrap_or(0)` yields 0, the
                // stream was skipped before reaching the `130` arm below, no `streamID` transport token
                // was ever returned, and the phone's entire outbound iAP2 path never existed. The
                // tunnel sat at `Init` with zero NowPlaying while A/V looked perfect, which is why it
                // survived ten commits to this file. Device-proven, with the box log and the
                // before/after contrast committed at
                // `docs/ops/captures/2026-08-10_REGRESSION_datastream130_scid_rejected.txt` (33 rejections
                // in one session; the phone re-asked from cseq 3 onward and we refused every time).
                // Already reachable beyond 130: we advertise `mainBuffered`, so iOS may request a
                // buffered stream we do not implement, and it deserves the `_` arm's diagnostic.
                //
                // Three independent reasons the R14G17 citation cannot govern type 130:
                //   1. Line 4343 is the SCREEN path (line 4354 derives the screen AES key). The other
                //      two scid checks (1927 `_MainAudioSetup`, 3307 `_MainAltAudioSetup`) are
                //      conditional on `pairVerifySession`. CAUTION: `pairVerifySession` is always set
                //      in a CarPlay session, so those two DO fire — the conditionality defeats the
                //      inference that Apple has a blanket all-streams rule; it is NOT licence to relax
                //      scid on A/V. Both audio paths additionally MINT a UUID-derived connectionID
                //      when scid is absent (1913-1925, 3293-3305) rather than failing.
                //   2. Type 130 does not EXIST in R14G17 — `AirPlayCommon.h:251-255` stops at 110. A
                //      2017 drop cannot speak to a stream type added after it (CLAUDE.md: silence in
                //      R14G17 is not an answer).
                //   3. scid is NOT this stream's salt. Type 130 keys off `DataStream-Salt<seed>` taken
                //      from its own SETUP request. Device-proven at
                //      `docs/ops/captures/2026-07-25_SUCCESS_airplayd_wl_handshake.txt:25` (the request key
                //      list carries no `streamConnectionID`) and `:36` (`key schedule SOLVED:
                //      DataStream-Salt839141951896294626 (seed)`). Corroborated in the current receiver
                //      side: `_DataStreamSessionSetup` (CarPlaySDK.framework) reads `seed` and feeds
                //      `asprintf("%s%llu", "DataStream-Salt", seed)`, and `streamConnectionID` appears
                //      nowhere in that binary's text.
                //      DO NOT be fooled by R14G17's `_GetStreamSecurityKeys:4723-4747`, which builds
                //      `"DataStream-Salt" + scid`: that is the SCREEN/AUDIO use of the same constant.
                //      Type 130 reuses the constant with a DIFFERENT id (`seed`). Reading 4723-4747 as
                //      "the DataStream salt is scid" is the most likely way this fix gets reverted.
                //
                // Keep echoing scid verbatim below — do not synthesize a non-zero one. The 07-25
                // session answered with scid=0 echoed back and the phone accepted it; docs/carplay/05_METADATA_AND_CONTROLS.md §1.3
                // records that we never read `streamConnectionID` back from our response at all.
                //
                // SCOPE: admitting 130 does NOT restore proven code. At the last 07-25 commit
                // (c1c5901) this file had no 130 arm, no scid guard and no key probe, and
                // `datastream.rs` did not exist — the code behind the SUCCESS capture was never
                // committed in that form and was rewritten into 5ce9d1c alongside the guard that made
                // it unreachable. The capture proves the PROTOCOL SHAPE; the arm below has zero
                // hardware hours. Treat its next run as a first run.
                if scid == 0 && matches!(ty, 100..=102 | 110 | 111) {
                    eprintln!(
                        "[session] SETUP stream type={ty} has streamConnectionID=0 — INVALID (it is \
                         the HKDF salt); skipping. Apple returns kVersionErr for this."
                    );
                    continue;
                }
                match ty {
                    110 => {
                        let listener = match TcpListener::bind("[::]:0") {
                            Ok(l) => l,
                            Err(e) => {
                                eprintln!(
                                    "[session] bind screen TCP failed: {e} — skipping stream 110"
                                );
                                continue;
                            }
                        };
                        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
                        eprintln!(
                            "[session] SETUP phase2 screen(110) scid={scid} → dataPort {port}"
                        );
                        let flag = self.stream_flag(110);
                        spawn_screen(listener, shared, scid, 9001, flag, self.activity.clone());
                        let mut r = Dictionary::new();
                        r.insert("type".into(), Value::Integer(110.into()));
                        r.insert("dataPort".into(), Value::Integer((port as i64).into()));
                        resp.push(Value::Dictionary(r));
                    }
                    111 => {
                        // ALT / cluster screen (docs/carplay/06_AV_PIPELINE.md): iOS's second AirPlay screen stream for the
                        // instrument-cluster map (getClusterLayer:). Identical transport to type 110 —
                        // forwarded (fwd-enc) to a SEPARATE sink :9005 → ocbmd CH_ALT_VIDEO → the host's
                        // dedicated alt decoder + floating window.
                        let listener = match TcpListener::bind("[::]:0") {
                            Ok(l) => l,
                            Err(e) => {
                                eprintln!("[session] bind alt-screen TCP failed: {e} — skipping stream 111");
                                continue;
                            }
                        };
                        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
                        eprintln!("[session] SETUP phase2 ALT screen(111) scid={scid} → dataPort {port} (→ :9005)");
                        let flag = self.stream_flag(111);
                        spawn_screen(listener, shared, scid, 9005, flag, self.activity.clone());
                        let mut r = Dictionary::new();
                        r.insert("type".into(), Value::Integer(111.into()));
                        r.insert("dataPort".into(), Value::Integer((port as i64).into()));
                        resp.push(Value::Dictionary(r));
                        // Nav video OFF BY DEFAULT. iOS auto-encodes the cluster the moment it's set up
                        // while a nav route is active, and `stopUI` does NOT reliably stop it (verified:
                        // the event channel isn't even up yet at SETUP — RECORD wires it later — so a
                        // stopUI here is a no-op, and even post-RECORD iOS keeps encoding while navigating).
                        // The real, reliable gate is the BOX forward gate: `spawn_screen` drops the cluster
                        // frames unless `events::nav_forward()` is set, so the second stream never enters
                        // the OCBM pipe and can't starve the main 4K stream. The host toggles nav_forward
                        // via CMD_NAV_START/CARD/APP (on) / CMD_NAV_STOP (off). Default off (audit M-e/H1).
                        eprintln!("[session] SETUP phase2 ALT screen(111): default-OFF (box forward-gated until nav toggled on)");
                    }
                    100..=102 => {
                        // Audio (UDP RTP). 102=MainHighAudio (AAC-LC media), 100/101=Main/AltAudio
                        // (AAC-ELD low-latency). The receiver binds the dataPort; the iPhone sends RTP
                        // to it. Only 102 carries a controlPort (RTCP); 100/101 carry none.
                        let fmt = sd
                            .get("audioFormat")
                            .and_then(|v| {
                                v.as_unsigned_integer()
                                    .or_else(|| v.as_signed_integer().map(|s| s as u64))
                            })
                            .unwrap_or(0);
                        let Some((codec, sr, ch)) = decode_audio_format(fmt) else {
                            continue; // unrecognized format — skip this stream rather than mis-decode (#911)
                        };
                        // Route by audioType, not stream type: media → media sink (:9002), everything
                        // else (telephony/speechRecognition/alert/default) → voice sink (:9003). Over the
                        // WIRED transport iOS delivers media on stream type 100 as PCM (audioType:"media").
                        let audio_type = sd
                            .get("audioType")
                            .and_then(|v| v.as_string())
                            .unwrap_or("")
                            .to_string();
                        let is_media = audio_type == "media";
                        let data = match UdpSocket::bind("[::]:0") {
                            Ok(s) => s,
                            Err(e) => {
                                eprintln!("[session] bind audio data UDP failed: {e} — skipping stream {ty}");
                                continue;
                            }
                        };
                        let dport = data.local_addr().map(|a| a.port()).unwrap_or(0);
                        eprintln!(
                            "[session] SETUP phase2 audio({ty}) scid={scid} fmt={fmt:#x} \
                             {sr}Hz {ch}ch {codec:?} audioType={audio_type:?} → dataPort {dport}",
                        );
                        // Bidirectional MainAudio (type 100): if the iPhone offers an uplink
                        // (`input=true` + its `dataPort`), set up the mic→iPhone uplink to that port
                        // using the negotiated `codec` (wired PCM / wireless ELD). Mirrors the C
                        // `_MainAltAudioSetup` uplink leg (sendPort = request dataPort).
                        #[cfg(feature = "mic-uplink")]
                        if ty == 100 {
                            let wants_input = sd
                                .get("input")
                                .and_then(|v| {
                                    v.as_boolean()
                                        .or_else(|| v.as_unsigned_integer().map(|n| n != 0))
                                })
                                .unwrap_or(false);
                            let iphone_port = sd
                                .get("dataPort")
                                .and_then(|v| {
                                    v.as_unsigned_integer()
                                        .or_else(|| v.as_signed_integer().map(|s| s as u64))
                                })
                                .unwrap_or(0);
                            if wants_input && iphone_port > 0 {
                                match (self.peer_addr, data.try_clone()) {
                                    (Some(peer), Ok(send_sock)) => {
                                        let input_key = derive_stream_keys(&shared, scid).input;
                                        // Uplink dst = the control peer's address (KEEPING its IPv6
                                        // scope_id) with only the port swapped to the stream's uplink
                                        // port. Mirrors the C `SockAddrCopy(peerAddr)+SockAddrSetPort`.
                                        let dst = uplink_dst(peer, iphone_port as u16);
                                        crate::uplink::configure(send_sock, dst, input_key, ty as u8, codec, sr, ch);
                                    }
                                    (None, _) => eprintln!(
                                        "[session] type-100 input requested but no peer addr — uplink skipped"
                                    ),
                                    (_, Err(e)) => eprintln!("[session] uplink socket clone failed: {e}"),
                                }
                            }
                        }
                        // Wire audio_type tag for the seam SEAM_FORMAT message (ocbm-proto ATYPE_*).
                        let atype: u8 = match audio_type.as_str() {
                            "media" => 0,
                            "telephony" => 1,
                            "speechRecognition" => 2,
                            "alert" => 3,
                            // `compatibility` is a first-class advertised audioType (info.rs
                            // preset_wireless_8) and is a MEDIA-carrying PCM fallback, not a
                            // "default" voice stream. Folding it into 4 made a consumer route it
                            // by format alone — a 48k stereo compatibility stream looks exactly
                            // like alt-audio/nav.
                            "compatibility" => 5,
                            _ => 4, // "default" / absent
                        };
                        let flag = self.stream_flag(ty);
                        // QC 2026-07-25: cloned BEFORE the move below because `stream_flag(ty)` is
                        // destructive — calling it a second time for the same `ty` would flip the
                        // flag we just handed to `spawn_audio` and kill that stream immediately.
                        let ctrl_flag = flag.clone();
                        spawn_audio(
                            data,
                            shared,
                            scid,
                            ty,
                            codec,
                            is_media,
                            atype,
                            sr,
                            ch,
                            flag,
                            self.activity.clone(),
                        );
                        let mut r = Dictionary::new();
                        r.insert("type".into(), Value::Integer(ty.into()));
                        r.insert(
                            "streamConnectionID".into(),
                            Value::Integer((scid as i64).into()),
                        );
                        r.insert("dataPort".into(), Value::Integer((dport as i64).into()));
                        if ty == 102 {
                            // Non-fatal bind (#139): on failure just omit controlPort rather than panic.
                            match UdpSocket::bind("[::]:0") {
                                Ok(ctrl) => {
                                    let cport = ctrl.local_addr().map(|a| a.port()).unwrap_or(0);
                                    r.insert(
                                        "controlPort".into(),
                                        Value::Integer((cport as i64).into()),
                                    );
                                    // QC 2026-07-25: was `self.alive.clone()` (session-wide). That tied
                                    // this RTCP socket + drain thread to full teardown only, so a
                                    // PARTIAL teardown of stream 102, or a 102 re-SETUP (which mints a
                                    // fresh ctrl socket every time), left the previous drain thread and
                                    // its UDP socket alive for the rest of the session — the same
                                    // accumulation class #406/#413 fixed for the DATA sockets via
                                    // `stream_flag`; the ctrl socket was simply missed. Mid-session
                                    // media re-SETUPs are real (see the capture notes above).
                                    spawn_udp_drain(ctrl, ctrl_flag); // RTCP ctrl — bound but idle
                                }
                                Err(e) => eprintln!(
                                    "[session] bind audio ctrl UDP failed: {e} — no controlPort"
                                ),
                            }
                        }
                        resp.push(Value::Dictionary(r));
                    }
                    // Gated so wired inertness is STRUCTURAL, not merely empirical. No wired capture
                    // contains a type-130 SETUP, but if one ever did, `log_datastream_frame` would route
                    // its payload into `dispatch_iap_tunnel_message` -> `metadata::emit_json` -> the
                    // 127.0.0.1:9004 seam — the SAME seam wired `iap2d` feeds. That is a real
                    // contamination path, and "no capture shows it" is a weaker guarantee than a gate.
                    // The wireless spawn site (`wireless/src/av.rs`) is the only setter of this var.
                    130 if std::env::var_os("CARPLAY_WIRELESS_METADATA").is_some() => {
                        // DataStream / RemoteControlSession (docs/carplay/05_METADATA_AND_CONTROLS.md). THE wireless-metadata blocker:
                        // modern iOS carries iAP2 over a dedicated RCS *channel*, not over the 2017-era
                        // `iAPSendMessage` POST /command. The phone calls `carEndpoint_createiAPChannelIfNeeded`
                        // ("Creating RCS channel for iAP", iOS 27 AirPlaySender) which SETUPs this stream
                        // with `clientTypeUUID = E9459FD0-BCAD-4C45-820F-1E72447EF2F2` (the iAP client type),
                        // then expects a **transport token** back — the `streamID` key — before it can build
                        // its transport streams.
                        //
                        // Until 2026-07-25 this fell into the `_` arm below and we pushed NOTHING into the
                        // response array, so iOS logged, 15× per session:
                        //   apsession_setupStreamsCreatingResponse:6578: false condition
                        //   Failed to obtain transport token from SETUP response: -6727 kNotFoundErr
                        //   apEndpointRemoteControlSession_ensureAndCopyTransportStreams:663: kNotFoundErr
                        //   apEndpointRemoteControlSession_sendMessageInternal:1025: kNotFoundErr
                        // i.e. the phone's ENTIRE outbound message path to us never existed. That is why the
                        // tunnel handshake got a 200 OK on every send and never one inbound iAP2 frame.
                        //
                        // Shape confirmed against the receiver-side reference — `_DataStreamSessionSetup`
                        // in `CarPlaySDK.framework` (Xcode's CarPlaySimulator.devicekitplugin), which reads
                        // `channelID`/`clientTypeUUID`/`controlType`/`wantsDedicatedSocket`/`seed` and whose
                        // inbound iAP2 delivery point is `_AirPlayReceiverSessioniAPDataReceive`. Stream type
                        // 130 does not exist in the 2017 R14G17 SDK (`AirPlayCommon.h:251-255` stops at 110),
                        // which is why every prior pass working from that source missed it.
                        let s = |k: &str| sd.get(k).and_then(|v| v.as_string()).unwrap_or("").to_string();
                        let channel_id = s("channelID");
                        let client_type = s("clientTypeUUID");
                        let control_type = sd
                            .get("controlType")
                            .and_then(|v| {
                                v.as_signed_integer()
                                    .or_else(|| v.as_unsigned_integer().map(|u| u as i64))
                            })
                            .unwrap_or(-1);
                        // NOTE the default. An ABSENT key currently routes into the unimplemented
                        // shared arm below, i.e. a dead channel — and the two cases are
                        // indistinguishable in the log, so make them distinguishable rather than
                        // guess. Apple's own receiver does not branch on this at all:
                        // `_AirPlayReceiverSessionSetup` calls DataStreamCreate/Start/GetPort once
                        // each and sets the port key UNCONDITIONALLY, consuming `wantsDedicatedSocket`
                        // only for a log line. So `true` is very likely the faithful default — but
                        // every capture on record sends the key explicitly set to true, so this arm
                        // has never been observed, and flipping a default on the session-setup path
                        // we spent ten days unbreaking is not something to do blind. Left as-is,
                        // now visible; flip it when a capture shows the key absent.
                        let wants_socket_raw = sd.get("wantsDedicatedSocket").and_then(|v| {
                            v.as_boolean()
                                .or_else(|| v.as_unsigned_integer().map(|n| n != 0))
                        });
                        if wants_socket_raw.is_none() {
                            eprintln!(
                                "[session] *** DataStream(130) SETUP omitted `wantsDedicatedSocket` — \
                                 defaulting to the UNIMPLEMENTED shared arm; Apple's receiver always \
                                 binds a port here, so this default is a suspect ***"
                            );
                        }
                        let wants_socket = wants_socket_raw.unwrap_or(false);
                        // `seed` — read by `_DataStreamSessionSetup` (CarPlaySDK) and the prime suspect
                        // for the HKDF salt id on this stream, since the RCS SETUP carries no
                        // `streamConnectionID`. Logged unconditionally: if it is absent the probe below
                        // will say so, and the raw key set tells us what to try next.
                        let seed = sd
                            .get("seed")
                            .and_then(|v| {
                                v.as_unsigned_integer()
                                    .or_else(|| v.as_signed_integer().map(|s| s as u64))
                            })
                            .unwrap_or(0);
                        let all_keys: Vec<&str> = sd.keys().map(|k| k.as_str()).collect();
                        // Monotonic, never zero: iOS treats the token as an opaque int64 handle and a 0
                        // would be indistinguishable from "absent" on its side.
                        static NEXT_STREAM_ID: AtomicU64 = AtomicU64::new(1);
                        let stream_id = NEXT_STREAM_ID.fetch_add(1, Ordering::Relaxed) as i64;
                        // docs/carplay/05_METADATA_AND_CONTROLS.md §1.2 (NOT §5 — that cross-reference was wrong and got copied into
                        // several places): SEVEN RCS client types exist, not four. Only the iAP one may
                        // drive the iAP2 link — LogTransfer, the two VehicleDataProtocol channels, and
                        // UrlFling / OverlayUI / SenderSettingsData would otherwise steal the single
                        // global outbound sink AND have their payloads fed to the iAP2 state machine.
                        // This is an allowlist of one, so the three types discovered on 2026-07-30 were
                        // already excluded correctly.
                        //
                        // An ABSENT uuid is treated as iAP. NOTE this DIVERGES from Apple: the SDK's
                        // `_DataStreamSessionSetup` sends an absent `clientTypeUUID` to the teardown
                        // path (-6735), identically to an unrecognised one. Kept because the branch
                        // should be unreachable — the phone logs `clientTypeUUID=(-)` at RCS-creation
                        // time but does send it in the SETUP request, and docs/carplay/05_METADATA_AND_CONTROLS.md §1.3 says to gate on
                        // the request, not the log line — so this is belt-and-braces for the live path.
                        const IAP_CLIENT_TYPE: &str = "E9459FD0-BCAD-4C45-820F-1E72447EF2F2";
                        let is_iap =
                            client_type.is_empty() || client_type.eq_ignore_ascii_case(IAP_CLIENT_TYPE);
                        eprintln!(
                            "[session] SETUP phase2 DataStream(130) scid={scid} channelID={channel_id:?} \
                             clientTypeUUID={client_type:?} controlType={control_type} \
                             wantsDedicatedSocket={wants_socket} seed={seed} reqKeys={all_keys:?} \
                             → streamID={stream_id}{}",
                            if is_iap { "  [iAP channel]" } else { "  [NON-iAP channel]" }
                        );
                        let mut r = Dictionary::new();
                        r.insert("type".into(), Value::Integer(ty.into()));
                        r.insert(
                            "streamConnectionID".into(),
                            Value::Integer((scid as i64).into()),
                        );
                        // The transport token. `streamID` is the key `_DataStreamSessionSetup` populates;
                        // iOS then logs either "Use shared connection with transport token: %lld" (no port)
                        // or "Use dedicated socket with remote port: %d, transport token: %lld".
                        r.insert("streamID".into(), Value::Integer(stream_id.into()));
                        if wants_socket {
                            // Dedicated socket requested: bind TCP and advertise the port. We accept and log
                            // the framing rather than routing it yet — the encryption context for this stream
                            // derives from `DataStream-Salt` / `DataStream-{Output,Input}-Encryption-Key`
                            // (CarPlaySDK), which is the next piece to implement once we can see real bytes.
                            match std::net::TcpListener::bind("[::]:0") {
                                Ok(l) => {
                                    let dport = l.local_addr().map(|a| a.port()).unwrap_or(0);
                                    r.insert("dataPort".into(), Value::Integer((dport as i64).into()));
                                    eprintln!(
                                        "[session] DataStream(130) dedicated socket listening on {dport}"
                                    );
                                    // Per-stream flag, not the session-wide `alive` (F13). Two things
                                    // this buys: a PARTIAL TEARDOWN naming type 130 now actually reaps
                                    // this listener+thread (`reset_stream` removes the entry and flips
                                    // the flag; with `self.alive` it was a silent no-op), and a
                                    // mid-session re-SETUP supersedes the previous 130 thread instead of
                                    // accumulating one per SETUP. It also closes the `alive` re-arm race:
                                    // `setup_phase1` sets the session flag back to true, so a thread that
                                    // had not yet observed the false window could be resurrected against
                                    // a stale listener — a fresh Arc per SETUP cannot be. `reset()` drains
                                    // and flips every entry in `av_streams`, so full teardown still stops
                                    // this thread.
                                    let alive = self
                                        .stream_flag_keyed(130, channel_id.clone());
                                    // One accepted connection per SETUP, enforced.
                                    //
                                    // The listener below loops on `accept()`, and the key schedule is
                                    // derived per connection from `shared` + the SETUP's `seed`, both
                                    // of which are constant for the life of this listener. A SECOND
                                    // connection would therefore re-derive the SAME ChaCha20-Poly1305
                                    // key pair with BOTH frame counters back at zero — full nonce
                                    // reuse against the first connection's frames, which recovers
                                    // keystream by XOR and reuses the Poly1305 one-time key.
                                    //
                                    // Apple's model makes this unreachable by construction: each
                                    // stream's keys are salted with a per-SETUP `streamConnectionID`,
                                    // so a new connection always implies a new SETUP and a new salt.
                                    // We cache the probed id across connections, so we must refuse
                                    // explicitly. Not known to be reachable — no capture shows iOS
                                    // reconnecting RCS without re-SETUPing — but nonce reuse is
                                    // catastrophic rather than degrading, so it is guarded regardless.
                                    //
                                    // The PRIMARY enforcement is now that the thread `return`s (and
                                    // drops the listener) when its one connection ends, so a
                                    // reconnect gets ECONNREFUSED and forces a fresh SETUP; the
                                    // in-loop refusal below is belt-and-braces for that.
                                    let mut accepted_one = false;
                                    // DataStream crypto (docs/carplay/05_METADATA_AND_CONTROLS.md §1.5, frame codec). Same frame codec as the control /
                                    // event channels — `[u16 LE len][ciphertext][tag:16]`, AAD = the 2-byte
                                    // length, 12-byte counter nonce — which the live capture confirms: every
                                    // observed frame was 76 B prefixed `3a 00` (0x3a = 58 → 2 + 58 + 16).
                                    // Only the key schedule differs: HKDF salt `DataStream-Salt<scid>` with
                                    // info `DataStream-{Output,Input}-Encryption-Key`, which
                                    // `stream::derive_stream_keys` already implements for the A/V streams.
                                    //
                                    // Direction follows the established convention in this file: `.output`
                                    // decrypts iPhone→receiver (as at `spawn_screen`/`spawn_audio`), `.input`
                                    // encrypts receiver→iPhone (as at the mic uplink).
                                    // Candidate HKDF salt ids, most-likely first. `seed` is the RCS
                                    // analogue of the A/V streams' `streamConnectionID` (docs/carplay/05_METADATA_AND_CONTROLS.md §1.4).
                                    let cands: Vec<(&'static str, u64)> = vec![
                                        ("seed", seed),
                                        ("streamID", stream_id as u64),
                                        ("scid", scid),
                                        ("zero", 0),
                                    ];
                                    thread::spawn(move || {
                                        l.set_nonblocking(true).ok();
                                        loop {
                                            match l.accept() {
                                                Ok((mut c, peer)) => {
                                                    if accepted_one {
                                                        // Second connection on this listener — see
                                                        // the `accepted_one` note above. Re-deriving
                                                        // the same key schedule with both counters
                                                        // at zero is nonce reuse, which is
                                                        // catastrophic rather than degrading.
                                                        eprintln!(
                                                            "[datastream] REFUSING second connection from {peer} — \
                                                             the key schedule for this SETUP is already in use \
                                                             (re-deriving it would reuse ChaCha20-Poly1305 nonces)"
                                                        );
                                                        let _ = c.shutdown(std::net::Shutdown::Both);
                                                        continue;
                                                    }
                                                    // Never read on the live path any more — the
                                                    // `return` when this connection ends makes a
                                                    // second accept unreachable — but kept armed so
                                                    // the refusal above survives any future refactor
                                                    // that resumes accepting.
                                                    #[allow(unused_assignments)]
                                                    {
                                                        accepted_one = true;
                                                    }
                                                    eprintln!("[datastream] connected from {peer}");
                                                    c.set_read_timeout(Some(
                                                        std::time::Duration::from_millis(500),
                                                    ))
                                                    .ok();
                                                    // Bound the WRITE too (#108's lesson, applied to
                                                    // this socket): `datastream::send` holds the global
                                                    // SINK lock across `write_all`, and its callers hold
                                                    // SESSION — an abrupt wireless drop with a full
                                                    // kernel send buffer would otherwise block for the
                                                    // whole TCP retry window (~15 min) and wedge
                                                    // teardown, which takes SESSION too.
                                                    c.set_write_timeout(Some(
                                                        std::time::Duration::from_secs(2),
                                                    ))
                                                    .ok();
                                                    // Bare 9-byte ACKs are latency-critical and would
                                                    // otherwise be Nagle-eligible behind unacked data.
                                                    c.set_nodelay(true).ok();
                                                    // Codec is built lazily: the first inbound frame is
                                                    // used to PROBE which salt/direction is correct, then
                                                    // the channel is created with the winning key and the
                                                    // frame counter starts at 0 for the rest of the
                                                    // connection.
                                                    let mut chan: Option<rtsp::control::ControlChannel> =
                                                        None;
                                                    let mut probed = false;
                                                    let mut auth_failed = false;
                                                    let mut ds_generation: Option<u64> = None;
                                                    let mut acc: Vec<u8> = Vec::new();
                                                    // RCS reassembly buffer. One RCS message can span
                                                    // MULTIPLE crypto frames: the DataStream frames at
                                                    // 16384 B, but `MaxPacketSize = 0xFFFF` lets the
                                                    // phone send a 65535-byte iAP2 link packet, which
                                                    // becomes a ~65567-byte RCS message. Artwork data
                                                    // fragments hit this every time.
                                                    let mut rcs: Vec<u8> = Vec::new();
                                                    let mut buf = [0u8; 2048];
                                                    loop {
                                                        match c.read(&mut buf) {
                                                            Ok(0) => {
                                                                eprintln!("[datastream] peer closed (EOF)");
                                                                break;
                                                            }
                                                            Ok(n) => {
                                                                acc.extend_from_slice(&buf[..n]);
                                                                if chan.is_none() && !probed {
                                                                    match probe_datastream_keys(
                                                                        &shared, &acc, &cands,
                                                                    ) {
                                                                        Some((id, read_is_output, desc)) => {
                                                                            probed = true;
                                                                            eprintln!(
                                                                                "[datastream] key schedule SOLVED: {desc}"
                                                                            );
                                                                            let sk = derive_stream_keys(&shared, id);
                                                                            // Read decrypts iPhone→receiver;
                                                                            // write is the opposite key of
                                                                            // the same pair.
                                                                            let (rk, wk) = if read_is_output {
                                                                                (sk.output, sk.input)
                                                                            } else {
                                                                                (sk.input, sk.output)
                                                                            };
                                                                            let keys = rtsp::control::ControlKeys { read: rk, write: wk };
                                                                            chan = Some(
                                                                                rtsp::control::ControlChannel::new(
                                                                                    keys.clone(),
                                                                                ),
                                                                            );
                                                                            // Register the outbound half so
                                                                            // every `iap_tunnel` send now
                                                                            // goes over THIS channel
                                                                            // (docs/carplay/05_METADATA_AND_CONTROLS.md). A separate
                                                                            // ControlChannel instance: the
                                                                            // read and write frame counters
                                                                            // are independent per direction.
                                                                            match c.try_clone() {
                                                                                Ok(tx) => {
                                                                                    // Register the sink ONLY. Do NOT restart
                                                                                    // the iAP2 link here.
                                                                                    //
                                                                                    // A previous revision called `reset()` +
                                                                                    // `start()` at this point, on the theory
                                                                                    // that a link must be established over the
                                                                                    // channel it will live on (the DETECT+SYN
                                                                                    // at RECORD having gone out over
                                                                                    // `POST /command`). The iPhone's own iAP2
                                                                                    // packet trace, read from `accessoryd` on
                                                                                    // 2026-07-25, disproves the premise: it
                                                                                    // labels this transport **"AirPlay"** and
                                                                                    // treats `iAPSendMessage` and this
                                                                                    // DataStream as ONE transport. Its trace:
                                                                                    //   Acc  0xee DETECT
                                                                                    //   Acc  0x80 SYN  seq=0x00
                                                                                    //   iPod 0xc0 SYN-ACK seq=0x0a   (6 ms!)
                                                                                    //   Acc  0xee DETECT   <-- this restart
                                                                                    //   Acc  0x80 SYN      <-- killed it
                                                                                    //   iPod 0xc0 SYN-ACK seq=0x52 …forever
                                                                                    // The phone answered the FIRST SYN
                                                                                    // correctly; re-DETECTing tore down the
                                                                                    // link it had just brought up, and it
                                                                                    // never saw an ACK afterwards. The
                                                                                    // in-flight handshake is already valid —
                                                                                    // registering the sink is enough, and
                                                                                    // every subsequent send simply takes the
                                                                                    // RCS path from here.
                                                                                    if is_iap {
                                                                                        ds_generation = Some(
                                                                                            crate::datastream::register(tx, keys),
                                                                                        );
                                                                                    } else {
                                                                                        eprintln!(
                                                                                            "[datastream] non-iAP client type — not registering the iAP2 sink"
                                                                                        );
                                                                                    }
                                                                                }
                                                                                Err(e) => eprintln!(
                                                                                    "[datastream] socket clone failed: {e} — receive-only, sends stay on POST /command"
                                                                                ),
                                                                            }
                                                                        }
                                                                        None => {
                                                                            // Only give up once a whole
                                                                            // frame is buffered — a short
                                                                            // read is not a failed probe.
                                                                            if acc.len() >= 2 {
                                                                                let need = 2 + u16::from_le_bytes([acc[0], acc[1]]) as usize + 16;
                                                                                if acc.len() >= need {
                                                                                    probed = true;
                                                                                    eprintln!(
                                                                                        "[datastream] key probe EXHAUSTED over {} candidates \
                                                                                         (frame {} B, first 32: {:02x?}) — salt is none of seed/streamID/scid/0",
                                                                                        cands.len(),
                                                                                        need,
                                                                                        &acc[..acc.len().min(32)]
                                                                                    );
                                                                                }
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                                let Some(chan) = chan.as_mut() else {
                                                                    // Keep `acc` when the probe has not
                                                                    // yet run to exhaustion: a SHORT READ
                                                                    // is not a failed probe, and clearing
                                                                    // here discards the buffered prefix so
                                                                    // the next read lands mid-frame and the
                                                                    // probe can never authenticate again —
                                                                    // silently killing the connection for
                                                                    // its lifetime (docs/carplay/05_METADATA_AND_CONTROLS.md review).
                                                                    //
                                                                    // Once the probe IS exhausted, no key
                                                                    // exists that can ever decrypt this
                                                                    // connection — reading on would discard
                                                                    // data forever while looking alive, so
                                                                    // close it (bounded) instead.
                                                                    if probed {
                                                                        eprintln!(
                                                                            "[datastream] key probe exhausted with no match — \
                                                                             closing the undecryptable connection"
                                                                        );
                                                                        break;
                                                                    }
                                                                    continue;
                                                                };
                                                                loop {
                                                                    match chan.decrypt_frame(&acc) {
                                                                        Ok(Some((pt, used))) => {
                                                                            acc.drain(..used);
                                                                            rcs.extend_from_slice(&pt);
                                                                            drain_rcs(&mut rcs, is_iap);
                                                                        }
                                                                        Ok(None) => break, // need more
                                                                        Err(e) => {
                                                                            // Print the variant:
                                                                            // Oversized is a protocol
                                                                            // violation, AuthFailed a
                                                                            // counter desync — either
                                                                            // way unrecoverable.
                                                                            eprintln!(
                                                                                "[datastream] decrypt FAILED ({e:?}, {} B buffered, first 32: {:02x?}) — \
                                                                                 dropping the connection",
                                                                                acc.len(),
                                                                                &acc[..acc.len().min(32)]
                                                                            );
                                                                            acc.clear();
                                                                            auth_failed = true;
                                                                            break;
                                                                        }
                                                                    }
                                                                }
                                                                if auth_failed {
                                                                    // Authenticated stream: once the
                                                                    // counters desync every later frame
                                                                    // fails too. Drop the connection
                                                                    // instead of re-logging forever on a
                                                                    // 528 MHz core.
                                                                    break;
                                                                }
                                                            }
                                                            Err(ref e) if is_timeout(e) => {
                                                                // Periodic pre-Identify handshake budget
                                                                // check (audit Fix #20): fires ~every
                                                                // 500 ms even with NO inbound, recovering a
                                                                // tunnel that SYN-ACKed then went silent —
                                                                // the wedge start() (record/modesChanged
                                                                // one-shots) never re-examines.
                                                                crate::iap_tunnel::tick();
                                                                if !alive.load(Ordering::Acquire) {
                                                                    // Release OUR sink before leaving —
                                                                    // this path bypasses the teardown
                                                                    // below, and relying on
                                                                    // `events::clear()` to cover it
                                                                    // makes correctness depend on a
                                                                    // second, unrelated call site.
                                                                    if let Some(g) = ds_generation.take() {
                                                                        crate::datastream::clear_if(g);
                                                                    }
                                                                    return;
                                                                }
                                                            }
                                                            Err(e) => {
                                                                eprintln!("[datastream] read error: {e}");
                                                                break;
                                                            }
                                                        }
                                                    }
                                                    if let Some(g) = ds_generation.take() {
                                                        crate::datastream::clear_if(g);
                                                    }
                                                    // The one connection this SETUP's key schedule
                                                    // permits has ended. Do NOT go back to accepting:
                                                    // the old loop kept the listener open and refused
                                                    // every reconnect forever (holding the thread + fd
                                                    // while the phone retried into a closed door).
                                                    // Returning DROPS the listener, so a reconnect
                                                    // attempt gets ECONNREFUSED and the phone re-SETUPs
                                                    // with a fresh salt — which is the only path that
                                                    // doesn't reuse ChaCha20-Poly1305 nonces (see the
                                                    // `accepted_one` note above).
                                                    eprintln!(
                                                        "[datastream] connection ended — dropping the listener \
                                                         (a reconnect needs a fresh SETUP/key schedule)"
                                                    );
                                                    return;
                                                }
                                                Err(ref e)
                                                    if e.kind() == std::io::ErrorKind::WouldBlock =>
                                                {
                                                    if !alive.load(Ordering::Acquire) {
                                                        return;
                                                    }
                                                    thread::sleep(std::time::Duration::from_millis(
                                                        100,
                                                    ));
                                                }
                                                Err(e) => {
                                                    eprintln!("[datastream] accept error: {e}");
                                                    return;
                                                }
                                            }
                                        }
                                    });
                                }
                                // A bind failure lands us in EXACTLY the shape the `else` arm below
                                // warns about — a streamID with no `dataPort` and no listener — so it
                                // must be equally loud. It previously logged one quiet line and fell
                                // through to `resp.push(r)`, i.e. the one dead-channel path that
                                // announced itself as a routine fallback. There is nothing to fall
                                // back TO: "replying shared-connection" named a transport we do not
                                // implement.
                                Err(e) => {
                                    eprintln!(
                                        "[session] *** DataStream(130) dedicated socket bind FAILED: {e} ***"
                                    );
                                    eprintln!(
                                        "[session] *** streamID {stream_id} is returned WITHOUT a dataPort \
                                         and with no listener — inbound delivery for this channel will be \
                                         DROPPED (this is the unimplemented shared-connection shape, \
                                         reached by accident) ***"
                                    );
                                }
                            }
                        } else {
                            // Every capture on record requests a dedicated socket
                            // (wantsDedicatedSocket=true), so this arm has never been exercised on
                            // hardware — and the shared-connection transport it implies (RCS frames
                            // multiplexed onto the control connection, addressed by the transport
                            // token) is NOT implemented. Be loud about it rather than silently
                            // returning a streamID the phone will talk into a void.
                            eprintln!(
                                "[session] *** DataStream(130) wantsDedicatedSocket=false — the \
                                 SHARED-CONNECTION transport is NOT IMPLEMENTED ***"
                            );
                            eprintln!(
                                "[session] *** streamID {stream_id} is returned, but inbound \
                                 delivery for this channel will be DROPPED ***"
                            );
                        }
                        resp.push(Value::Dictionary(r));
                    }
                    _ => {
                        // OMIT unsupported streams from the response — this is what Apple's own receiver
                        // does (`AirPlayReceiverSession.c:947-949`: `default: atr_ulog("### Unsupported
                        // stream type: %d"); break;` — it logs, sets no `err`, and adds no entry).
                        //
                        // A revision of this arm briefly pushed a bare `{type}` entry on the theory that
                        // a missing entry fails the whole SETUP response. That was a WRONG
                        // generalisation from the stream-130 case: 130 failed because the phone needs a
                        // `streamID` transport token in its entry (docs/carplay/05_METADATA_AND_CONTROLS.md §1.3), not because an entry
                        // was absent. If a missing entry were fatal, Apple's own receiver would break
                        // every session containing a stream it does not implement.
                        //
                        // Still unimplemented and therefore omitted: AuxOutAudio, AuxInAudio,
                        // MainBuffered. They are not requested in any capture on record (mainBuffered
                        // and enhancedSiri both negotiate to disabled), so this is currently unreachable.
                        eprintln!("[session] SETUP phase2 stream type {ty} NOT IMPLEMENTED — omitted from the response");
                    }
                }
            }
        }
        let mut d = Dictionary::new();
        d.insert("streams".into(), Value::Array(resp));
        let mut buf = Vec::new();
        Value::Dictionary(d)
            .to_writer_binary(&mut buf)
            .expect("plist");
        buf
    }
}

impl SessionDelegate for AvSession {
    fn on_paired(&mut self, shared_secret: [u8; 32]) {
        self.shared = Some(shared_secret);
    }

    /// Capture: log + dump every POST /command (duckAudio/unduckAudio/flushAudio/setVolume/… — the
    /// audio-focus signalling iOS uses during feature overlap). `CARPLAY_CMD_DUMP` writes full plists.
    fn command(&mut self, request_plist: &[u8]) -> Vec<u8> {
        // Parse ONCE (R5): both the `type` read here and the iAP `data` extraction below draw from
        // this — the body used to be parsed twice per inbound command.
        let parsed = parse_control_plist(request_plist).and_then(|v| v.into_dictionary());
        let ty = parsed
            .as_ref()
            .and_then(|d| {
                d.get("type")
                    .and_then(|v| v.as_string())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "?".into());
        let is_iap = ty.contains("iAP") || ty.contains("iap");
        eprintln!(
            "[command] ← iPhone POST /command type='{ty}' ({} B){}",
            request_plist.len(),
            if is_iap {
                "  *** iAP2-OVER-AIRPLAY TUNNEL FRAME ***"
            } else {
                ""
            }
        );
        // Wireless-metadata investigation (2026-07-17): unconditionally capture EVERY inbound
        // /command plist to a fixed box file, so the exact iAP2-over-AirPlay tunnel frames (type
        // "iAPSendMessage" — the wireless-only metadata carrier) can be pulled over OCBM and decoded
        // offline: their plist key layout, the `_data` framing (FF5A link / 4040 control-session /
        // bare msg-id) and the iAP2 msg-id. Length-prefixed `[u32 LE len][plist]`. /tmp is tmpfs
        // (pull before any reboot). Pure read-only observation — no protocol action, no A/V risk.
        {
            use std::io::Write as _;
            // Size-capped (audit R4): /tmp is tmpfs (RAM-backed), so an uncapped append would exhaust RAM
            // over a long session and OOM-kill the daemon. The goal is only to capture the FIRST tunnel
            // frames to learn their format, so 4 MiB from session start is far more than enough — stop
            // appending past the cap rather than grow without bound.
            const CAP_MAX: u64 = 4 * 1024 * 1024;
            let path = "/tmp/carplay_cmd_capture.bin";
            let grown = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            if grown < CAP_MAX {
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                    let _ = f.write_all(&(request_plist.len() as u32).to_le_bytes());
                    let _ = f.write_all(request_plist);
                }
            }
        }
        // Metadata forward (host Metadata window): every inbound command plist rides the :9004 seam
        // — through `metadata::emit`, which owns the ONE connection this process makes to it. This
        // used to open a second, independent socket to the same port; ocbmd keeps one producer per
        // channel, so the two connections evicted each other in a loop and both sides silently lost
        // records. See `metadata::emit_command_plist`.
        iap2_core::metadata::emit_command_plist(request_plist);
        if let Ok(p) = std::env::var("CARPLAY_CMD_DUMP") {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&p)
            {
                let _ = f.write_all(&(request_plist.len() as u32).to_le_bytes());
                let _ = f.write_all(request_plist);
            }
        }
        // Decode iAP-tunnel frames arriving on the CONTROL channel. docs/wireless/00_WIRELESS_CARPLAY.md #2.4 added this as a
        // speculative "in case iOS ever delivers `iAPSendMessage` here instead" hedge.
        //
        // FIXED 2026-07-25 — it is not a hedge, it is THE inbound path, and it was routed wrong.
        // Live evidence from the first successful wireless session: every inbound command from iOS
        // arrived HERE (8x modesChanged, 1x disableBluetooth on `POST /command`), while the event
        // channel received nothing inbound at all. Apple's own R14G17 source agrees — phone->accessory
        // `kAirPlayCommand_iAPSendMessage` is delivered through the session control dispatch
        // (`AirPlayReceiverSession.c`), i.e. exactly this connection.
        //
        // The bug: this called `dispatch_iap_tunnel_message` DIRECTLY, which expects a bare
        // `[40 40][len][msg-id][body]` metadata payload. A link-layer frame (SYN-ACK, or any
        // handshake step, which is `FF 5A`-headed) is not that shape, so it was dropped as
        // unrecognized and the tunnel's state machine could never advance past `Init` — matching the
        // observed "we send DETECT+SYN, nothing ever comes back". Now mirrors the event-channel path
        // in `events.rs`: offer the frame to the handshake state machine FIRST, and fall through to
        // the metadata dispatcher only if the tunnel doesn't claim it.
        // Same one-shot tunnel nudge the event-channel handler runs — wired here too because THIS is
        // the channel `modesChanged` actually arrives on (see the note above).
        if ty == "modesChanged" {
            crate::events::modes_changed_tunnel_nudge();
        }
        if is_iap {
            // From the single parse above — no second parse, no dictionary clone.
            if let Some(data) = parsed
                .as_ref()
                .and_then(|d| d.get("params").and_then(|v| v.as_dictionary()))
                .and_then(|p| {
                    ["data", "Data", "_data", "_Data"]
                        .iter()
                        .find_map(|k| p.get(k).and_then(|v| v.as_data()))
                })
            {
                if !crate::iap_tunnel::handle_inbound(data) {
                    crate::events::dispatch_iap_tunnel_message(data);
                }
            }
        }
        // docs/carplay/03_SDK_GROUND_TRUTH.md §2: the reference answers an inbound `POST /command` with 200 + a BINARY-PLIST body.
        // When the delegate produces no `outParams` it serializes an EMPTY DICTIONARY, it does not send
        // an empty body (`AirPlayReceiverServer.c:2518-2524` -> `_requestSendPlistResponse:3418-3441`).
        // `AirPlayCommon.h:584-592` documents `iAPSendMessage` as having "No response keys", which
        // means exactly that: an empty dict. We previously returned a zero-length body while still
        // advertising the binary-plist content type.
        //
        // Calibration (corrected 2026-07-25 after review — the first version of this comment
        // overstated the case): the reference's sender parses the reply body only
        // `if( inMsg->bodyLen > 0 )` (`AirPlayReceiverSession.c:859`), so the old empty body was NOT
        // ambiguous to a reference-derived client. This change is right because it matches what the
        // reference emits, not because the old shape was provably breaking anything.
        empty_plist_dict()
    }

    fn teardown(&mut self, request_plist: &[u8]) {
        // Partial teardown (request carries a `streams` array) tears down ONLY those streams and keeps
        // the session alive (the C's `outDone=false`); full teardown (no `streams`) ends everything.
        let streams = parse_control_plist(request_plist)
            .ok_or(())
            .ok()
            .and_then(|v| v.into_dictionary())
            .and_then(|d| d.get("streams").and_then(|v| v.as_array()).cloned());
        // ⚠️ `.filter(|s| !s.is_empty())` ADDED 2026-07-30 — this branched on PRESENCE, Apple branches
        // on COUNT. `AirPlayReceiverSession.c:1030-1089`:
        //     streamCount = streams ? CFArrayGetCount( streams ) : 0;
        //     if( streamCount > 0 ) { _ControlIdleStateTransition( … ); goto exit; }   // PARTIAL
        //     … _ScreenTearDown / _TearDownStream ×2 / _ControlTearDown / _TimingFinalize …
        //     if( outDone ) *outDone = ( streamCount == 0 );
        // So an EMPTY array is a FULL teardown for Apple, and `outDone = true` makes the server release
        // the session and reset didAnnounce/didAudioSetup/didScreenSetup/didRecord.
        //
        // `Value::as_array()` returns `Some(&vec![])` for `streams: []`, so we took the partial branch,
        // stopped nothing, logged `stopped []`, and returned WITHOUT `reset()` — leaking every stream
        // thread and socket plus the timing/event/keepAlive plane and `control_setup`.
        //
        // Honest limit: the 2026-07-29 Simulator capture contains no `streams: []` TEARDOWN (all seven
        // are non-empty partials), and HTTP bodies weren't logged, so there is no direct evidence iOS
        // sends an empty array. What IS certain is that Apple's source defines the semantics and ours
        // implemented the opposite. The other two shapes were already right: absent body and
        // non-array `streams` both fall through to `reset()`, matching `CFDictionaryGetCFArray`.
        if let Some(streams) = streams.filter(|s| !s.is_empty()) {
            // Stop exactly the named streams by type via their per-stream flags (#406/#413) — the thread
            // exits on its next poll and drops its socket — instead of the old no-op that leaked a thread
            // + UDP/TCP socket per re-SETUP. The session (timing/event/keepAlive) stays alive.
            let mut stopped: Vec<i64> = Vec::new();
            {
                let mut m = crate::plock(&self.av_streams);
                for s in &streams {
                    if let Some(ty) = s
                        .as_dictionary()
                        .and_then(|sd| sd.get("type"))
                        .and_then(|v| v.as_signed_integer())
                    {
                        // Reap every channel of this type. With one instance per type — every
                        // path any capture shows — this is bit-identical to the old
                        // `m.remove(&ty)`; it only differs once a second RCS channel exists.
                        let keys: Vec<(i64, String)> =
                            m.keys().filter(|(k, _)| *k == ty).cloned().collect();
                        for k in keys {
                            if let Some(flag) = m.remove(&k) {
                                flag.store(false, Ordering::Release);
                                stopped.push(ty);
                            }
                        }
                    }
                }
            }
            eprintln!("[session] partial TEARDOWN — stopped streams {stopped:?}, session kept");
            // The mic uplink is the INPUT leg of the type-100 MainAudio stream, so it dies WITH that
            // stream — not only at session end (`reset()`, which was the sole `clear()` caller).
            // Apple reaches `_TearDownStream` from this same partial path
            // (AirPlayReceiverSession.c:1041-1046 -> :4840) and joins `sendAudioThread` there.
            //
            // iOS creates and destroys a type-100 `speechRecognition` stream PER SIRI TURN, so
            // without this the host was told `uplink on` and never `uplink off`: measured on the
            // bench as 424 `armed=true` lines and zero `armed=false` in one session, a microphone
            // held open for 95 minutes, and RTP still going to a dataPort the phone had closed.
            //
            // Gated on 100 alone, symmetric with the one place we ARM (`configure` is only ever
            // called under `ty == 100`). 102/MainHighAudio also routes to mainAudioCtx in the C, but
            // we never arm an uplink for it, so clearing on it would be a no-op with a misleading
            // comment.
            //
            // ⚠️ PI-VERIFIED ONLY (2026-08-16). Exercised on the Raspberry Pi's WIRELESS path,
            // where type 100 is the speechRecognition stream. It has NOT run on a CCPA, where
            // type 100 is the WIRED MEDIA stream (PCM — see the routing note in spawn_audio: wired
            // media is 100, not 102). The semantics are the same either way — the uplink is that
            // stream's input leg and dies with it — but on wired this means a media-stream teardown
            // now also drops the mic, which is correct and untested. Watch for it if wired mic
            // behaviour changes.
            #[cfg(feature = "mic-uplink")]
            if stopped.contains(&100) {
                crate::uplink::clear();
            }
            return;
        }
        eprintln!("[session] full TEARDOWN — stopping stream threads + resetting session state");
        self.reset();
    }

    fn last_activity(&self) -> Option<Arc<AtomicU64>> {
        Some(self.activity.clone())
    }

    fn setup(&mut self, request_plist: &[u8]) -> Vec<u8> {
        if let Ok(p) = std::env::var("CARPLAY_SETUP_DUMP") {
            // Dump each raw SETUP request plist (to pin the feature-token array the iPhone proposes,
            // incl. "hevc" — the receiver-side HEVC lever, per the AirPlaySender gate).
            static SN: AtomicU64 = AtomicU64::new(0);
            let _ = std::fs::write(
                format!("{p}.{}", SN.fetch_add(1, Ordering::Relaxed)),
                request_plist,
            );
        }
        let dict = parse_control_plist(request_plist)
            .ok_or(())
            .ok()
            .and_then(|v| v.into_dictionary());
        let has_streams = dict
            .as_ref()
            .map(|d| d.contains_key("streams"))
            .unwrap_or(false);
        // Phase 1 only (no `streams`): the phone tells us who it is, once per session. Publish it for
        // ocbmd to mirror to the host app — see `PHONE_IDENT_FILE`.
        if !has_streams {
            if let Some(d) = dict.as_ref() {
                publish_phone_identity(d);
            }
        }
        let keep_alive = dict
            .as_ref()
            .and_then(|d| d.get("keepAliveLowPower"))
            .and_then(|v| v.as_boolean())
            .unwrap_or(false);
        if has_streams {
            if let Ok(p) = std::env::var("CARPLAY_STREAMS_CAPTURE") {
                // Sequence each capture (`<p>.0`, `.1`, …) so a later media re-SETUP doesn't clobber
                // the earlier voice (type-100) request — the one we need for the mic-uplink question.
                use std::sync::atomic::{AtomicU32, Ordering};
                static SEQ: AtomicU32 = AtomicU32::new(0);
                let i = SEQ.fetch_add(1, Ordering::Relaxed);
                let _ = std::fs::write(format!("{p}.{i}"), request_plist);
            }
            self.setup_phase2(dict.as_ref().unwrap())
        } else {
            self.setup_phase1(keep_alive)
        }
    }

    fn record(&mut self) -> Vec<u8> {
        // Accept the event-channel TCP HERE (the C's _ControlStart), holding the RECORD 200 OK until
        // it completes. The iPhone connects the event channel after the SETUP response, so by RECORD
        // it's usually pending; poll briefly (up to ~5s) like the C's bounded SocketAccept.
        // Our `_ControlStart` success flag. Apple treats control-start failure as fatal to the whole
        // session: `_ControlStart` is `require_noerr`-gated (`AirPlayReceiverSession.c:1108-1112`), so a
        // failure skips the `sessionStarted = true` assignment at `:1147` entirely and RECORD is answered
        // 500 (`AirPlayReceiverServer.c:3136`). Nothing may then be sent, ever. We can't return 500 here
        // without regressing the proven wired baseline, so we carry the outcome forward and use it to gate
        // the tunnel open below — same effect for the wireless-only path this governs.
        let mut control_started = false;
        if let Some(listener) = self.event_listener.take() {
            listener.set_nonblocking(true).ok();
            let mut accepted = false;
            for _ in 0..50 {
                match listener.accept() {
                    Ok((stream, peer)) => {
                        eprintln!("[session] RECORD: event channel accepted from {peer}");
                        stream.set_nonblocking(false).ok();
                        // Wire the encrypted event channel (receiver→iPhone commands, e.g. touch
                        // hidSendReport) — the C's _ControlStart. Was a plain drain before.
                        crate::events::setup(
                            stream,
                            self.shared.unwrap_or([0u8; 32]),
                            self.alive.clone(),
                        );
                        accepted = true;
                        // Active post-RECORD session-focus handshake, byte-exact to the proven-working
                        // carplayd `_SessionStarted` (`airplay_receiver_main.c:1249-1260`): requestUI
                        // FIRST, then TakeScreen(videoFocus@500). ENABLED UNCONDITIONALLY 2026-07-02 after
                        // the 7-agent reconciliation: this is the ONE accessory behavior carplayd (real
                        // Apple SDK, same iOS 27, working audio) does that we omitted. It was previously
                        // env-gated + only ever tried with a since-fixed buggy Untake, so it never got a
                        // clean test. Grounded: iOS audio SETUP is activation-driven (fires on media-play,
                        // not connect), and this handshake completes the session/focus bring-up carplayd
                        // relies on. Does NOT take MainAudio (the iOS-27 trace confirms the borrow layer is
                        // runtime routing, not a SETUP gate) — screen focus only, exactly like carplayd.
                        let u = crate::events::send_request_ui();
                        let m = crate::events::send_take_screen();
                        // NOTE 2026-07-02: a take/untake-MainAudio A/B was tested BOTH ways (Take=1,
                        // Untake=2) — NEITHER produced an audio SETUP, so the modes take/untake of
                        // MainAudio is NOT the audio gate (confirms the iOS-27 trace: modes = runtime
                        // routing, not stream creation). The real gate is the SETUP-phase1 `mainBuffered`
                        // feature echo above. The A/B helper was removed as dead code (audit 2026-07-12).
                        eprintln!("[session] RECORD: session-focus handshake sent (requestUI={u}, takeScreen={m})");
                        // ALT / cluster video. VERIFIED mechanism (docs/carplay/04_CAPABILITIES_AND_CONFIG.md [CAP]; the old Carlinkit
                        // firmware's "two nav commands" = Cmd 508 RequestNaviScreenFocus / 509 Release):
                        //   1. Advertising `altScreen` in /info + enabledFeatures makes iOS AUTO-SETUP the
                        //      type-111 stream — the offer IS the setup trigger.
                        //   2. `send_take_screen()` above is the screen-resource focus.
                        //   3. `requestUI(maps:/car/instrumentcluster/map)` makes iOS actually ENCODE and
                        //      send frames into the 111 stream (the content/focus gate = Cmd 508).
                        // This is now HOST-TOGGLE driven, NOT auto-sent: nav video starts OFF and the user
                        // turns it on from the app (OCBM CMD_NAV_START → requestUI, CMD_NAV_STOP → stopUI).
                        // So the type-111 stream is set up on connect but stays idle (no frames, Nav window
                        // closed) until the user toggles it — matching the dynamic on-demand model.
                        break;
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(100));
                    }
                    Err(e) => {
                        eprintln!("[session] RECORD: event accept error: {e}");
                        break;
                    }
                }
            }
            if !accepted {
                eprintln!("[session] RECORD: event channel not connected within timeout");
            }
            control_started = accepted;
        }
        // Open the iAP2-over-AirPlay tunnel HERE — the last step of the start sequence, once the event
        // channel, the stream threads and the session-focus handshake above have all completed. This is
        // our equivalent of Apple setting `sessionStarted = true` at the end of
        // `AirPlayReceiverSessionStart` (`AirPlayReceiverSession.c:1147`), which is the hard gate on
        // every command send (`:825` — `require_action_quiet( inSession->sessionStarted, …, kStateErr )`).
        //
        // MOVED HERE 2026-07-25 (docs/carplay/02_SESSION_LIFECYCLE.md). It previously ran inside `events::setup()`, i.e. at the
        // event-channel accept — inside our `_ControlStart` equivalent, several steps before Apple's own
        // guard permits a single byte to be sent. The observable signature of that was exactly what live
        // sessions showed: iOS accepts the carrier (`iAPSendMessage` is answered with a bodyless 2xx, see
        // `events.rs::handle_inbound_event`) but never binds an iAP2 link to it, so no inbound iAP2 frame
        // of any kind ever arrives. CT5 CINEMO — the authoritative shipping reference after Apple's own
        // SDK — defers even further, posting `MESSAGE_START_IAP` from `handleSessionStarted()`
        // (`CarPlayManager.java:1052`) so the WiFi iAP2 link is created strictly after the AirPlay
        // session is started, never before.
        //
        // docs/wireless/00_WIRELESS_CARPLAY.md #2.6: gate on the env var ALONE, not also a peer-IP heuristic. `CARPLAY_WIRELESS_METADATA`
        // is process-scoped and set only at the wireless spawn site (`crates/vendor/wireless/src/av.rs`);
        // the architecture runs exactly one `airplayd` at a time (the wired supervisor's launch line never
        // sets this var), so the env check alone is a reliable proxy for "this is a wireless session" — and
        // unlike the removed `peer_addr()`-contains-`"192.168.43."` check, it can't be defeated by a
        // `peer_addr()` error or an IPv6 peer silently disabling the whole feature.
        // ORDER MATTERS (docs/carplay/05_METADATA_AND_CONTROLS.md): open the tunnel BEFORE publishing the started flag. Previously the
        // flag was set first, and a `modesChanged` arriving in that window let the CONTROL thread call
        // `iap_tunnel::start()` ahead of us — RECORD then sent a SECOND DETECT+SYN milliseconds later.
        // The iPhone's own packet trace shows exactly that: DETECT #2 reached it 5.7 ms BEFORE it wrote
        // SYN-ACK #1, so this was a startup race, not the reaction-to-SYN-ACK story first assumed.
        if control_started && std::env::var_os("CARPLAY_WIRELESS_METADATA").is_some() {
            // docs/wireless/00_WIRELESS_CARPLAY.md: establish this session's OWN iAP2 link/Identify first — per Apple's own Integration
            // Guide the tunnel carries no iAP2 session state until we do, and CINEMO likewise runs a full
            // fresh identification over the tunnel rather than continuing the BT one.
            // `send_wireless_metadata_subscriptions()` only ever runs once THIS handshake reaches
            // IdentifyAccept (see `iap_tunnel::execute`'s `Action::Note("IdentifyAccept")` arm).
            crate::iap_tunnel::start();
        }
        if control_started {
            crate::events::mark_session_started(true);
        }
        eprintln!("[session] RECORD done");
        Vec::new()
    }
}

impl Drop for AvSession {
    fn drop(&mut self) {
        // Connection closed (graceful TEARDOWN already ran, or the transport detected an abrupt drop):
        // stop every session thread and reset all per-session state. Idempotent (safe after teardown).
        self.reset();
    }
}

/// Build the mic-uplink destination from the control-connection peer, preserving the IPv6 `scope_id`
/// (interface) and swapping in the stream's uplink port. Wired CarPlay runs over IPv6 link-local, and
/// sending to a `fe80::…` address requires the scope; `SocketAddr::new(ip, port)` would zero it.
#[cfg(feature = "mic-uplink")]
fn uplink_dst(peer: std::net::SocketAddr, port: u16) -> std::net::SocketAddr {
    use std::net::{SocketAddr, SocketAddrV4, SocketAddrV6};
    match peer {
        SocketAddr::V6(a) => {
            SocketAddr::V6(SocketAddrV6::new(*a.ip(), port, a.flowinfo(), a.scope_id()))
        }
        SocketAddr::V4(a) => SocketAddr::V4(SocketAddrV4::new(*a.ip(), port)),
    }
}

/// Reset the persistent per-sink forwarding connections (`MEDIA_SINK`/`VOICE_SINK`/`META_SINK`) so the
/// next session reconnects fresh rather than writing to a stale/closed carlink socket.
fn clear_sinks() {
    for sink in [&MEDIA_SINK, &VOICE_SINK] {
        let mut g = crate::plock(sink);
        g.sock = None;
        // Invalidate any socket a `forward_to_sink` caller took out before this clear — it will see
        // the epoch mismatch on restore and drop the socket instead of putting it back.
        g.epoch = g.epoch.wrapping_add(1);
    }
    // There is no metadata sink to clear here any more: `metadata::emit_command_plist` owns the one
    // :9004 connection this process makes, because ocbmd keeps a single producer per channel and two
    // sockets from the same process evicted each other in a loop. The dead `META_SINK` static was
    // removed rather than left as a trap for the next reader.
    //
    // Deliberately NOT resetting `metadata`'s seam here. ocbmd's producer slot is CONNECTION-lifetime,
    // not session-lifetime — `av_conns` is mutated only on accept and on read-close, never at a
    // CarPlay session boundary — so a socket held across sessions keeps a perfectly valid slot. A
    // reset would buy nothing and would force a reconnect whose eviction can discard the unread tail
    // of a frame still in ocbmd's buffer, desyncing the host's length-prefixed reader.
}

/// Read exactly `buf.len()` bytes, tolerating the shutdown-poll read timeout (re-checking `alive` on
/// each timeout so a torn-down session's thread exits). Returns false on EOF, error, or shutdown.
/// Replaces `read_exact`, which would lose partial data on a timeout.
fn read_full(sock: &mut TcpStream, buf: &mut [u8], alive: &AtomicBool) -> bool {
    let mut off = 0;
    while off < buf.len() {
        match sock.read(&mut buf[off..]) {
            Ok(0) => return false, // EOF (peer closed)
            Ok(n) => off += n,
            Err(ref e) if is_timeout(e) => {
                if !alive.load(Ordering::Acquire) {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    true
}

/// Run the NTP-like timing responder: reply to each `kRTCPTypeTimeSyncRequest` (210) with a
/// `…Response` (211), echoing the client's transmit stamp as our originate and filling receive/
/// transmit with our NTP clock. 32-byte `RTCPTimeSyncPacket`, all fields network order. Wakes every
/// `SHUTDOWN_POLL` to re-check `alive`, so it exits when the session ends.
fn spawn_timing(sock: UdpSocket, alive: Arc<AtomicBool>) {
    thread::spawn(move || {
        sock.set_read_timeout(Some(SHUTDOWN_POLL)).ok();
        let mut buf = [0u8; 64];
        while alive.load(Ordering::Acquire) {
            match sock.recv_from(&mut buf) {
                Ok((n, peer)) if n >= 32 && buf[1] == 210 => {
                    let t2 = ntp_now();
                    let mut r = [0u8; 32];
                    r[0] = 0x80;
                    r[1] = 211;
                    r[2..4].copy_from_slice(&7u16.to_be_bytes()); // length = 32/4 - 1
                    r[4..8].copy_from_slice(&buf[4..8]); // rtpTimestamp echo
                    r[8..16].copy_from_slice(&buf[24..32]); // client T1 (its transmit) → our originate
                    r[16..24].copy_from_slice(&t2.to_be_bytes()); // T2 receive
                    r[24..32].copy_from_slice(&ntp_now().to_be_bytes()); // T3 transmit
                    let _ = sock.send_to(&r, peer);
                }
                Ok(_) => {}
                Err(ref e) if is_timeout(e) => {} // wake to re-check `alive` at the top of the loop
                Err(_) => break,
            }
        }
    });
}

/// Fwd-enc video seam sync marker (task #33 / docs/carplay/06_AV_PIPELINE.md). Each forwarded video message's payload begins
/// with these 4 bytes so the host can **re-align after a lost/torn packet** — the RTP-sequence-number
/// analogue Apple's screen transport carries. On the wire (via `forward_screen`): `[u32 BE len][SEAM_MAGIC]
/// [marker]…`. The host normally parses sequentially; on a desync it scans for SEAM_MAGIC and resyncs.
pub const SEAM_MAGIC: [u8; 4] = [0x53, 0x45, 0x41, 0x56]; // "SEAV"

/// Forward one screen message (VideoConfig SPS/PPS, or one Annex-B VideoFrame) to the video IPC port
/// (:9001), **length-prefixed** as `[u32 BE len][annexb bytes]`. Each call is one screen message, so the
/// length prefix gives the consumer clean access-unit boundaries — a macOS VideoToolbox sink needs exactly
/// one `CMSampleBuffer` per frame and TCP hides the per-write boundaries. (Re)connects lazily and heals
/// after a dropped connection (the screen analogue of `forward_to_sink`); a connect/write failure drops
/// the frame and retries on the next (never breaks the stream); `up` logs only on up↔down transitions.
fn forward_screen(out: &mut Option<TcpStream>, up: &mut bool, port: u16, data: &[u8]) {
    forward_screen2(out, up, port, data, &[]);
}

/// Two-part variant of [`forward_screen`]: writes `[len BE][part1][part2]` without concatenating the
/// parts. The fwd-enc hot path passes the small header (magic+marker+seq+hdr) as `part1` and the large
/// encrypted frame **body** as `part2`, so the body is written straight from the read buffer with NO
/// per-frame copy — the biggest CPU saving on the 4K@60 forward path (was copied twice: into `fm`, then
/// into `framed`). Only the tiny `[len][part1]` prefix is assembled. Single-part callers pass `part2=&[]`.
/// Write `a` then `b` to `s` in as few syscalls as possible via a single `write_vectored`, looping only
/// on a partial write (std's `write_all_vectored` is still unstable). The loopback sink has a large
/// socket buffer, so the common case is one syscall for both slices. Returns false on any write error.
fn write_two(s: &mut TcpStream, mut a: &[u8], mut b: &[u8]) -> bool {
    while !a.is_empty() || !b.is_empty() {
        match s.write_vectored(&[IoSlice::new(a), IoSlice::new(b)]) {
            Ok(0) => return false,
            Ok(mut n) => {
                if n >= a.len() {
                    n -= a.len();
                    a = &[];
                    b = &b[n..];
                } else {
                    a = &a[n..];
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
    }
    true
}

fn forward_screen2(
    out: &mut Option<TcpStream>,
    up: &mut bool,
    port: u16,
    part1: &[u8],
    part2: &[u8],
) {
    if out.is_none() {
        // Bound the CONNECT (same fix and reasoning as `forward_to_sink`): an unbounded loopback
        // connect against a full accept backlog on the local consumer blocks in SYN-retry for
        // minutes, stalling the screen thread mid-session. Same 2 s ceiling as the write timeout.
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
            Ok(s) => {
                eprintln!("[screen] carlink :{port} connected");
                let _ = s.set_nodelay(true); // low-latency: flush each frame, don't Nagle-coalesce
                let _ = s.set_write_timeout(Some(Duration::from_secs(2))); // audit R3: never block the
                                                                           // screen thread on a stalled sink
                *out = Some(s);
                *up = true;
                // A consumer just (re)connected — request a fresh keyframe so it gets an IDR + SPS/PPS
                // to start decoding (CarPlay otherwise sends config/IDR only at stream start → a
                // mid-stream join sees P-frames only = black screen). Name the CLUSTER stream for the
                // :9005 (type-111) seam (#129) — a bare forceKeyFrame only re-IDRs the main console, so
                // the cluster would stay black on resume.
                let sid = if port == 9005 { Some(crate::events::CLUSTER_STREAM_ID) } else { None };
                // Fire-and-forget on a DETACHED thread — never inline. `send_force_key_frame_stream` ->
                // `send_command` takes the process-global EVENT mutex and holds it across a socket write
                // that can spin up to the 6 s `write_frame_or_fail` DEADLINE (events.rs) on a stalled or
                // contended event channel. This code runs on the SCREEN thread (the one reading frames
                // from the iPhone), so doing it inline froze screen-frame ingestion — and with it the
                // iPhone's own send buffer — for that whole window (perf audit 2026-08-09: the one place
                // control-plane locking reached into the A/V hot path). Reconnects are rare (stream start
                // + post-write-failure, the latter already bounded by the sink's 2 s SO_SNDTIMEO), so a
                // short-lived thread per reconnect is cheap; a dropped keyframe request only costs a
                // slightly longer wait for iOS's next natural IDR.
                std::thread::spawn(move || {
                    if crate::events::send_force_key_frame_stream(sid) {
                        eprintln!("[screen] requested ForceKeyFrame for the new :{port} consumer");
                    }
                });
            }
            Err(_) => {
                if *up {
                    eprintln!("[screen] carlink :{port} down — dropping frames, will reconnect");
                    *up = false;
                }
                return; // drop this frame; retry on the next
            }
        }
    }
    if let Some(s) = out.as_mut() {
        // Assemble only the small `[len][part1]` prefix; write the (large) body `part2` uncopied.
        // Prefix goes on the STACK — no per-frame heap alloc (perf audit 2026-08-09). In the committed
        // OCBM_FWD_ENC path part1 is the 141 B frame prefix or the smaller key message, both ≤ HEAD_MAX;
        // the on-box-decrypt fallback (fwd_enc unset) forwards larger Annex-B frames and correctly takes
        // the heap branch below — so the stack fast path never panics and every caller stays byte-exact.
        let total = (part1.len() + part2.len()) as u32;
        const HEAD_MAX: usize = 4 + 141;
        let mut stackbuf = [0u8; HEAD_MAX];
        // Write the `[len][part1]` prefix and the `part2` body in ONE vectored write (opt #2) instead of
        // two write_all calls — halves the write syscalls on the dominant small-P-frame case (and, with
        // set_nodelay, avoids pushing the tiny prefix as its own loopback segment + extra consumer read).
        let ok = if 4 + part1.len() <= HEAD_MAX {
            stackbuf[0..4].copy_from_slice(&total.to_be_bytes());
            stackbuf[4..4 + part1.len()].copy_from_slice(part1);
            write_two(s, &stackbuf[..4 + part1.len()], part2)
        } else {
            let mut head = Vec::with_capacity(4 + part1.len());
            head.extend_from_slice(&total.to_be_bytes());
            head.extend_from_slice(part1);
            write_two(s, &head, part2)
        };
        if !ok {
            *out = None; // drop the dead connection; reconnect on the next frame
            if *up {
                eprintln!("[screen] carlink :{port} write failed — will reconnect");
                *up = false;
            }
        }
    }
}

/// Screen (video) data plane: accept the iPhone's TCP connection on the screen dataPort, decrypt each
/// frame, and forward Annex-B H.264 to carlink_linux's video IPC port (:9001). Wire = repeated
/// `AirPlayScreenHeader`(128 B: bodySize u32 LE @0, opcode u8 @4) + body. ChaCha20-Poly1305 with the
/// stream's OUTPUT key (the key the C decrypts incoming data with), AAD = the 128-B header, nonce =
/// 4 zero bytes ‖ a per-frame 64-bit LE counter from 0. opcode 0 = VideoFrame (AVCC), 1 = VideoConfig
/// (avcC SPS/PPS). Faithful to AirPlayReceiverSessionScreen.c `_ProcessFrame`.
fn spawn_screen(
    listener: TcpListener,
    shared: [u8; 32],
    stream_connection_id: u64,
    sink_port: u16,
    alive: Arc<AtomicBool>,
    activity: Arc<AtomicU64>,
) {
    thread::spawn(move || {
        let keys = derive_stream_keys(&shared, stream_connection_id);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&keys.output));
        // Accept the iPhone's screen connection, but poll so we bail if the session ends first.
        listener.set_nonblocking(true).ok();
        let (mut sock, peer) = loop {
            match listener.accept() {
                Ok(x) => break x,
                Err(ref e) if is_timeout(e) => {
                    if !alive.load(Ordering::Acquire) {
                        return;
                    }
                    thread::sleep(SHUTDOWN_POLL);
                }
                Err(e) => {
                    eprintln!("[screen] accept failed: {e}");
                    return;
                }
            }
        };
        sock.set_nonblocking(false).ok();
        sock.set_read_timeout(Some(SHUTDOWN_POLL)).ok();
        eprintln!(
            "[screen] iPhone connected from {peer}; forwarding video → 127.0.0.1:{sink_port}"
        );
        // Lazy, self-healing sink (C8 hardening): connect on the first frame and reconnect after any
        // write failure, dropping frames while carlink is down — a momentary carlink blip must NOT
        // permanently kill video for the session (was: connect-once + `break` on error). Mirrors the
        // audio `forward_to_sink`. `sink_up` gates logging to up↔down transitions (no per-frame spam).
        let mut out: Option<TcpStream> = None;
        let mut sink_up = false;
        let mut counter: u64 = 0;
        // fwd-enc: the per-VideoFrame sequence stamped into each forwarded frame so the host can resync
        // its decrypt counter after a dropped frame (task #33). Advances on opcode-0 frames ONLY, so it
        // equals the iPhone's per-VideoFrame nonce counter (which the host uses as the ChaCha20 counter).
        let mut enc_seq: u64 = 0;
        let mut nal_len = 4usize; // AVCC NAL length size; refined from the avcC config
        let mut hdr = [0u8; 128];
        // Reused frame-body buffer, hoisted out of the loop: allocating + zeroing a fresh Vec per
        // frame cost ~18 MB/s of pure memset at 4K60. Grown on demand, never shrunk; each frame
        // works on `body_buf[..body_size]`.
        let mut body_buf: Vec<u8> = Vec::new();
        let mut frames: u64 = 0;
        // Per-lane frame accounting (perf 2026-08-09): BR = frames received from the iPhone (enc_seq,
        // advances on opcode-0 PRE-gate), BF = frames actually forwarded to the seam (out.is_some()
        // after the forward). Emitted to stderr (UART/session log) every ~2 s keyed by sink_port so a
        // live session shows box-received vs box-forwarded per lane (:9001 main / :9005 cluster) — the
        // measurement that localizes cluster loss (source vs box-gate vs box→app transport vs app slot).
        let mut fwd_video: u64 = 0;
        let mut recv_base: u64 = 0;
        let mut fwd_base: u64 = 0;
        let mut last_report = std::time::Instant::now();
        macro_rules! acct_report {
            () => {
                let el = last_report.elapsed().as_secs_f64();
                if el >= 2.0 {
                    eprintln!(
                        "[screen] acct lane=:{sink_port} recv={:.0}/s fwd={:.0}/s (enc_seq={enc_seq} fwd={fwd_video})",
                        (enc_seq - recv_base) as f64 / el,
                        (fwd_video - fwd_base) as f64 / el,
                    );
                    recv_base = enc_seq;
                    fwd_base = fwd_video;
                    last_report = std::time::Instant::now();
                }
            };
        }
        // COMMITTED MODEL (ccpa_custom): by DEFAULT (unless OCBM_FWD_ENC is explicitly disabled), do NOT
        // decrypt — forward the encrypted frames + the
        // key to the seam so the HOST app decrypts. Wire (each a forward_screen length-prefixed msg):
        //   key:   [SEAM_MAGIC "SEAV"][0x00][key.output 32B][scid 8B LE]
        //   frame: [SEAM_MAGIC "SEAV"][0x01][seq u64 LE][hdr 128B][body]
        // The host takes the nonce counter from the WIRE seq (nonce = 0,0,0,0 ++ seq_le64, aad = hdr).
        // seq advances on opcode-0 (hdr[4]) frames only, and advances even for frames we gate away, so a
        // host-local counter would desync permanently at the first gap.
        let fwd_enc = crate::levers::fwd_enc(); // safe default: forward-encrypted unless OCBM_FWD_ENC explicitly =0/false/off/empty
        // Hoisted for the same reason as `fwd_enc`: read once, not once per frame. As the first
        // operand of an `&&` this was a getenv (env lock + linear environ scan) on every frame
        // forever, and each one races any concurrent set_var.
        let screen_dump = std::env::var("CARPLAY_SCREEN_DUMP").is_ok();
        let mut key_sent = false; // fwd-enc: whether the current seam connection has been handed the key
        loop {
            if !read_full(&mut sock, &mut hdr, &alive) {
                break;
            }
            activity.store(now_ms(), Ordering::Relaxed); // A/V progress → feeds the idle watchdog
            let body_size = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as usize;
            let opcode = hdr[4];
            if body_size == 0 {
                continue;
            }
            // Bound the UNAUTHENTICATED wire length before allocating (audit R1, CRITICAL). This header
            // field is read before the body is decrypted/authenticated, so a crafted frame with
            // body_size ~= 0xFFFF_FFFF would force a multi-GB `vec![0u8; body_size]` and abort the daemon
            // (Rust aborts on alloc failure; the workspace builds panic="abort") — a trivial remote DoS
            // for anyone reachable on the wireless/NCM link. A real screen frame (even a 4K keyframe) is
            // well under this cap; an oversized value is a corrupt/hostile stream → drop it and reconnect.
            const MAX_FRAME_BODY: usize = 8 * 1024 * 1024;
            if body_size > MAX_FRAME_BODY {
                eprintln!("[screen] frame body_size {body_size} > {MAX_FRAME_BODY} cap — dropping stream");
                break;
            }
            if body_buf.len() < body_size {
                body_buf.resize(body_size, 0);
            }
            let body = &mut body_buf[..body_size];
            if !read_full(&mut sock, body, &alive) {
                break;
            }
            // Diagnostics: dump the first frame's key material + bytes for offline decrypt analysis.
            if frames == 0 && counter == 0 {
                if let Ok(p) = std::env::var("CARPLAY_SCREEN_CAPTURE") {
                    let mut cap = Vec::new();
                    cap.extend_from_slice(&shared);
                    cap.extend_from_slice(&stream_connection_id.to_le_bytes());
                    cap.extend_from_slice(&keys.output);
                    cap.extend_from_slice(&hdr);
                    cap.extend_from_slice(body);
                    let _ = std::fs::write(&p, &cap);
                    eprintln!(
                        "[screen] captured first frame (op {opcode}, {body_size}B body) → {p}"
                    );
                }
            }
            // COMMITTED MODEL: forward encrypted — hand the key, then raw [hdr][body] per frame. The key
            // is (re)sent whenever the seam (re)connects (not just once), so a mid-stream reconnect never
            // leaves the host without the decrypt key. `out.is_none()` = the next write will reconnect;
            // a failed write resets `out` to None so we resend on the following frame.
            if fwd_enc {
                // Cluster (type-111 → :9005) bandwidth gate: unless the host has toggled nav video on,
                // DROP the cluster frames here so the second stream never enters the OCBM/USB pipe and
                // stalls the main 4K video. Keep `enc_seq` advancing (it must equal the iPhone's
                // per-VideoFrame nonce counter) so the host can resync when nav resumes; tear the seam
                // down so resume reconnects + requests a fresh keyframe.
                if sink_port == 9005 && !crate::events::nav_forward() {
                    if out.is_some() {
                        out = None;
                        key_sent = false;
                        sink_up = false;
                    }
                    if opcode == 0 {
                        enc_seq += 1;
                    }
                    frames += 1;
                    acct_report!(); // BR advances, BF does not (cluster gated/dropped here)
                    continue;
                }
                if out.is_none() || !key_sent {
                    let mut km = Vec::with_capacity(4 + 1 + 32 + 8);
                    km.extend_from_slice(&SEAM_MAGIC); // resync marker
                    km.push(0x00u8); // KEY marker
                    km.extend_from_slice(&keys.output);
                    km.extend_from_slice(&stream_connection_id.to_le_bytes());
                    forward_screen(&mut out, &mut sink_up, sink_port, &km);
                    key_sent = out.is_some();
                    if key_sent && frames == 0 {
                        eprintln!("[screen] fwd-enc: handed video key (scid={stream_connection_id}) to seam");
                    }
                }
                // [SEAM_MAGIC][0x01][seq u64 LE][hdr 128] | [body] — magic lets the host re-align after a
                // lost packet; seq lets it resync the decrypt counter. Body is written uncopied (part2).
                // Built on the STACK (fixed 141 B) — no per-frame heap alloc on the hot 30-60 fps path
                // (perf audit 2026-08-09). All sub-slices are exact-length so `copy_from_slice` can't panic.
                let mut pfx = [0u8; 4 + 1 + 8 + 128];
                pfx[0..4].copy_from_slice(&SEAM_MAGIC);
                pfx[4] = 0x01u8; // FRAME marker
                pfx[5..13].copy_from_slice(&enc_seq.to_le_bytes());
                pfx[13..141].copy_from_slice(&hdr);
                forward_screen2(&mut out, &mut sink_up, sink_port, &pfx, body);
                if out.is_none() {
                    key_sent = false; // the write dropped the seam — resend the key after reconnect
                }
                // seq tracks the iPhone's per-VideoFrame nonce counter: advance on opcode-0 frames only.
                if opcode == 0 {
                    enc_seq += 1;
                    if out.is_some() {
                        fwd_video += 1; // reached the seam (BF)
                    }
                }
                frames += 1;
                if frames == 1 {
                    eprintln!("[screen] fwd-enc: forwarding ENCRYPTED frames (no on-box decrypt)");
                }
                acct_report!();
                continue;
            }
            // CARPLAY_SCREEN_DUMP: hex the head of each of the first frames on the ON-BOX DECRYPT path.
            // The proven wireless capture runs fwd-enc and never exercises this branch, so when a
            // VideoConfig parses to 0 B there is nothing in any log that says why. The opcode-1 body is
            // plaintext, so its first bytes identify the record directly: an avcC starts 01 <profile>
            // <compat> <level> FF E1, an hvcC starts 01 followed by the HEVC profile_tier_level. This is
            // also the only place the in-band codec identity is visible before any parsing (docs/carplay/03_SDK_GROUND_TRUTH.md §5).
            if screen_dump && frames < 6 {
                let head: Vec<String> =
                    body.iter().take(32).map(|b| format!("{b:02x}")).collect();
                eprintln!(
                    "[screen] DUMP frame#{frames} opcode={opcode} body_size={body_size} body.len={} head=[{}]",
                    body.len(),
                    head.join(" ")
                );
            }
            // Per the C `_ProcessFrame`: the ChaCha20 decrypt is ONLY for VideoFrame (opcode 0);
            // VideoConfig (opcode 1) is plaintext avcC, and the per-frame nonce advances only on
            // VideoFrame decrypts.
            let annexb = match opcode {
                0 => {
                    let plain = if body_size >= 16 {
                        let mut nonce = [0u8; 12];
                        nonce[4..].copy_from_slice(&counter.to_le_bytes());
                        counter += 1;
                        match cipher.decrypt(
                            Nonce::from_slice(&nonce),
                            Payload {
                                msg: &*body,
                                aad: &hdr,
                            },
                        ) {
                            Ok(p) => p,
                            Err(_) => {
                                // `counter` was already advanced for THIS frame above, so the frame
                                // that failed is `counter - 1` — the old log named the NEXT frame.
                                eprintln!("[screen] decrypt failed at frame {}", counter - 1);
                                break;
                            }
                        }
                    } else {
                        body.to_vec()
                    };
                    annexb_from_avcc(&plain, nal_len) // VideoFrame (AVCC → Annex-B)
                }
                1 => {
                    // VideoConfig: PLAINTEXT codec config (no decrypt). Auto-detect avcC (H.264) vs
                    // hvcC (HEVC) and parse accordingly (#826): the old code assumed avcC unconditionally,
                    // so an HEVC session's hvcC record parsed to garbage/0-byte config → black screen.
                    // `nal_len` (the frame length-prefix size) is read from the matching config field.
                    //
                    // 2026-08-05: on WIRELESS the body is not a bare configuration record at all — it is a
                    // QuickTime **sample-description box**, e.g.
                    //   00 00 00 f7  68 76 63 31 ("hvc1") …  <nested hvcC atom>
                    // The bare-record parsers below read that header as a config record, find no parameter
                    // sets, and return 0 B — which is exactly the "first frame decoded (0 B Annex-B)" seen
                    // on the head unit, with every subsequent frame then converted using a default
                    // `nal_len`. Unwrap to the nested atom first. Bare records (which start with the 0x01
                    // configurationVersion) are passed through untouched, so the wired path is unchanged.
                    let body = unwrap_sample_description(body);
                    let avcc = avcc_config_to_annexb(body);
                    if first_nal_is_h264_param_set(&avcc) {
                        if body.len() >= 5 {
                            nal_len = ((body[4] & 0x03) + 1) as usize; // avcC lengthSizeMinusOne + 1
                        }
                        avcc
                    } else {
                        if body.len() >= 22 {
                            nal_len = ((body[21] & 0x03) + 1) as usize; // hvcC lengthSizeMinusOne + 1
                        }
                        hvcc_config_to_annexb(body)
                    }
                }
                _ => continue,
            };
            forward_screen(&mut out, &mut sink_up, sink_port, &annexb);
            frames += 1;
            if frames == 1 {
                eprintln!("[screen] first frame decoded ({} B Annex-B)", annexb.len());
            }
        }
        eprintln!("[screen] stream ended after {frames} frames");
    });
}


/// Unwrap a QuickTime sample-description box to the nested `avcC`/`hvcC` configuration record.
///
/// Wireless CarPlay sends the VideoConfig (opcode 1) as a sample description — `[u32 size][FourCC
/// 'hvc1'|'avc1'] …` with the configuration record nested inside as its own atom — rather than as the
/// bare record the wired path delivers. Returns the payload of the first `hvcC`/`avcC` atom found, or
/// the input unchanged when it already looks like a bare record (`configurationVersion == 1`) or when
/// no atom is present, so every existing caller keeps its previous behaviour.
fn unwrap_sample_description(body: &[u8]) -> &[u8] {
    // A bare avcC/hvcC record always begins with configurationVersion = 1.
    if body.first() == Some(&1) {
        return body;
    }
    // Scan for the nested atom's FourCC. Atom layout is [u32 size][FourCC][payload], so the record
    // starts 4 bytes past the type. A linear scan is fine: these bodies are a few hundred bytes and
    // this runs once per stream.
    if body.len() >= 8 {
        for i in 0..body.len().saturating_sub(8) {
            let tag = &body[i..i + 4];
            if tag == b"hvcC" || tag == b"avcC" {
                return &body[i + 4..];
            }
        }
    }
    body
}

/// Convert an avcC config record (opcode 1) to Annex-B SPS/PPS (start-code-prefixed NALs).
fn avcc_config_to_annexb(avcc: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if avcc.len() < 6 {
        return out;
    }
    let num_sps = (avcc[5] & 0x1F) as usize;
    let mut i = 6;
    let emit = |out: &mut Vec<u8>, nal: &[u8]| {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
    };
    for _ in 0..num_sps {
        if i + 2 > avcc.len() {
            return out;
        }
        let len = u16::from_be_bytes([avcc[i], avcc[i + 1]]) as usize;
        i += 2;
        if i + len > avcc.len() {
            return out;
        }
        emit(&mut out, &avcc[i..i + len]);
        i += len;
    }
    if i >= avcc.len() {
        return out;
    }
    let num_pps = avcc[i] as usize;
    i += 1;
    for _ in 0..num_pps {
        if i + 2 > avcc.len() {
            return out;
        }
        let len = u16::from_be_bytes([avcc[i], avcc[i + 1]]) as usize;
        i += 2;
        if i + len > avcc.len() {
            return out;
        }
        emit(&mut out, &avcc[i..i + len]);
        i += len;
    }
    out
}

/// True if the first Annex-B NAL (after the `00 00 00 01` start code) is an H.264 SPS (type 7) or PPS
/// (type 8) — the marker that an avcC parse actually produced a valid H.264 parameter set. Used to
/// tell avcC from hvcC config records (#826): an hvcC record parsed as avcC yields no such NAL.
fn first_nal_is_h264_param_set(annexb: &[u8]) -> bool {
    // [00 00 00 01][nal_header …] — H.264 NAL type is the low 5 bits of the first NAL byte.
    annexb.len() >= 5 && matches!(annexb[4] & 0x1F, 7 | 8)
}

/// Convert an hvcC (HEVC) config record to Annex-B VPS/SPS/PPS. Layout per ISO/IEC 14496-15 §8.3.3.1:
/// a fixed 22-byte header, `numOfArrays` at byte 22, then that many arrays, each = `[completeness|
/// reserved|NAL_type u8][numNalus u16 BE]` followed by `numNalus × ([nalUnitLength u16 BE][nal])`.
/// Every NAL (VPS/SPS/PPS) is emitted start-code-prefixed. Bounds-checked; returns what it parsed.
fn hvcc_config_to_annexb(hvcc: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if hvcc.len() < 23 {
        return out;
    }
    let num_arrays = hvcc[22] as usize;
    let mut i = 23;
    for _ in 0..num_arrays {
        if i + 3 > hvcc.len() {
            return out;
        }
        let num_nalus = u16::from_be_bytes([hvcc[i + 1], hvcc[i + 2]]) as usize;
        i += 3;
        for _ in 0..num_nalus {
            if i + 2 > hvcc.len() {
                return out;
            }
            let len = u16::from_be_bytes([hvcc[i], hvcc[i + 1]]) as usize;
            i += 2;
            if i + len > hvcc.len() {
                return out;
            }
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(&hvcc[i..i + len]);
            i += len;
        }
    }
    out
}

/// Audio codec of a CarPlay audio stream, decoded from the `audioFormat` bitmask.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AudioCodec {
    /// Raw LPCM, signed 16-bit, interleaved, little-endian. **WIRED CarPlay media** (T=USB) uses this
    /// on stream type 100 (`audioFormat 0x8000` = PCM 48k/16/stereo) — see WIRED_AUDIO_ROOT_CAUSE.md.
    Pcm,
    AacLc,
    AacEld,
    /// Opus — wireless CarPlay uses it for some low-latency audio (indices 28-30). Forwarded encrypted
    /// like AAC (the box never decodes it); SEAM codec byte 3.
    Opus,
}

/// Decode an `audioFormat` bitmask → (codec, sample_rate, channels) (C `kAirPlayAudioFormat_*`).
/// Adds the PCM formats (bit 4 = PCM 16k mono, bit 15 = PCM 48k 16-bit stereo) that the wired path uses;
/// previously PCM fell through to the AAC-ELD default and was mis-decoded.
fn decode_audio_format(fmt: u64) -> Option<(AudioCodec, u32, u16)> {
    use AudioCodec::*;
    Some(match fmt {
        f if f == 1 << 4 => (Pcm, 16000, 1), // PCM 16kHz 16-bit mono (0x10)
        f if f == 1 << 15 => (Pcm, 48000, 2), // PCM 48kHz 16-bit stereo (0x8000) — wired media
        f if f == (1 << 15) | (1 << 4) => (Pcm, 48000, 2), // combined mask 0x8010 → prefer PCM 48k stereo
        f if f == 1 << 22 => (AacLc, 44100, 2),            // AAC-LC 44.1k stereo
        f if f == 1 << 23 => (AacLc, 48000, 2),            // AAC-LC 48k stereo
        f if f == 1 << 24 => (AacEld, 44100, 2),           // AAC-ELD 44.1k stereo
        f if f == 1 << 25 => (AacEld, 48000, 2),           // AAC-ELD 48k stereo
        f if f == 1 << 26 => (AacEld, 16000, 1),           // AAC-ELD 16k mono
        f if f == 1 << 27 => (AacEld, 24000, 1),           // AAC-ELD 24k mono
        f if f == 1 << 28 => (Opus, 16000, 1),             // OPUS 16k mono (#911)
        f if f == 1 << 29 => (Opus, 24000, 1),             // OPUS 24k mono
        f if f == 1 << 30 => (Opus, 48000, 1),             // OPUS 48k mono
        f if f == 1 << 31 => (AacEld, 44100, 1),           // AAC-ELD 44.1k mono
        f if f == 1u64 << 32 => (AacEld, 48000, 1),        // AAC-ELD 48k mono
        f if f == 1u64 << 43 => (AacEld, 32000, 1),        // AAC-ELD 32k mono
        // Unknown format: REJECT rather than silently mis-decoding as ELD-24k (#911 — the old default
        // guessed, producing garbage/silence for a format we didn't actually negotiate). The caller
        // skips the stream. Apple returns -6735 for an invalid index; we do the equivalent.
        _ => {
            eprintln!("[session] audio format {fmt:#x} not recognized — rejecting stream");
            return None;
        }
    })
}

/// Persistent per-sink forwarding connections to carlink, mirroring the C av_forwarding_hooks
/// `g_aac_fd` / `g_aac_eld_fd`: ONE socket per sink for the whole receiver lifetime, opened lazily and
/// shared across EVERY stream SETUP. The previous per-`spawn_audio` `TcpStream::connect` opened a fresh
/// :9002/:9003 connection per stream re-SETUP; combined with carlink's single-threaded serial accept
/// loop (`crates/carlink/src/source/carplayd.rs` `serve`), the iPhone's repeated voice re-SETUPs (Siri
/// re-SETUPs MainAudio ~4×/turn) starved the later, content-bearing connections → voice silence. One
/// persistent connection per sink — exactly what the working C receiver does — avoids it.
static MEDIA_SINK: Mutex<SinkSlot> = Mutex::new(SinkSlot { sock: None, epoch: 0 });
static VOICE_SINK: Mutex<SinkSlot> = Mutex::new(SinkSlot { sock: None, epoch: 0 });

/// A persistent sink slot: the socket plus an epoch bumped by [`clear_sinks`], so a writer that took
/// the socket out (to do its blocking I/O with the lock RELEASED) can tell whether the slot was
/// cleared while it was away — and drop the stale socket instead of resurrecting it into the next
/// session.
struct SinkSlot {
    sock: Option<TcpStream>,
    epoch: u64,
}

/// Write one already-framed AU to a persistent sink, (re)connecting lazily on first use or after a
/// dropped connection. Returns `false` if connect/write failed (carlink down/restarted): the socket is
/// reset so the next AU reconnects — a strict, more-robust superset of the C, which never reopens.
///
/// The blocking I/O runs OUTSIDE the sink lock: holding the mutex across `connect_timeout` (2 s) +
/// `write_all` (2 s SO_SNDTIMEO) stalled every OTHER stream sharing the sink for up to ~4 s per
/// wedged AU. The socket is taken out under the lock, connected/written unlocked, then restored —
/// unless [`clear_sinks`] ran in between (epoch mismatch) or another thread already installed a
/// fresh socket, in which case ours is dropped.
fn forward_to_sink(sink: &Mutex<SinkSlot>, port: u16, framed: &[u8]) -> bool {
    let (sock, epoch) = {
        let mut g = crate::plock(sink);
        (g.sock.take(), g.epoch)
    };
    let mut sock = match sock {
        Some(s) => s,
        None => {
            // Bound the CONNECT too, not just the write. This now runs only on stream threads (audio) —
            // the META_CMD path that once made it a control-thread concern moved to
            // `metadata::emit_command_plist` — but an unbounded loopback connect against a full accept
            // backlog on ocbmd still blocks in SYN-retry for minutes, stalling that stream. Same fix
            // and reasoning as `iap2-core/src/metadata.rs::emit`.
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
            match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
                Ok(s) => {
                    // Bound writes (audit R3): without a timeout a stalled local :9002/:9003 consumer
                    // makes write_all block the stream thread forever. Same 2 s as events.rs.
                    let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
                    s
                }
                Err(e) => {
                    eprintln!("[audio] connect carlink :{port} failed: {e}");
                    return false;
                }
            }
        }
    };
    let ok = sock.write_all(framed).is_ok();
    if ok {
        let mut g = crate::plock(sink);
        if g.epoch == epoch && g.sock.is_none() {
            g.sock = Some(sock); // still ours — restore for the next AU
        }
        // else: the slot was cleared (session teardown) or refilled while we were unlocked — drop
        // ours rather than clobber the newer socket or resurrect one teardown meant to kill.
    }
    // On write failure the socket is simply dropped; the next AU reconnects.
    ok
}

/// Audio data plane: receive RTP on the stream's dataPort, decrypt, and forward AAC to carlink — 102
/// (MainHighAudio, AAC-LC) → media :9002 as ADTS; 100/101 (Main/AltAudio, AAC-ELD) → voice :9003 as
/// length-tagged ELD. ChaCha20-Poly1305 with the stream OUTPUT key; per the C the nonce is the trailing
/// 8 bytes of the payload and the AAD is `ts‖ssrc` (header[4..12]) for every audio stream on a modern
/// iOS client (the C gates AAD on OS version, not stream type — verified live for type-100 ELD).
#[allow(clippy::too_many_arguments)]
fn spawn_audio(
    sock: UdpSocket,
    shared: [u8; 32],
    scid: u64,
    stream_type: i64,
    codec: AudioCodec,
    is_media: bool,
    atype: u8,
    sr: u32,
    ch: u16,
    alive: Arc<AtomicBool>,
    activity: Arc<AtomicU64>,
) {
    thread::spawn(move || {
        sock.set_read_timeout(Some(SHUTDOWN_POLL)).ok();
        let key = derive_stream_keys(&shared, scid).output;
        // COMMITTED MODEL: forward encrypted — hand the key once, then the raw RTP packets, so the HOST
        // decrypts (mirror of spawn_screen). Audio needs no counter: its nonce is the packet's trailing
        // 8 bytes. Framed [u32 BE len][marker][payload] to match the CH_VIDEO wire. BOTH sinks reach
        // the host: media (:9002 -> CH_MEDIA_AUDIO) and voice (:9003 -> CH_ALT_AUDIO, see :2562 and
        // ocbmd's seam table). See docs/carplay/02_SESSION_LIFECYCLE.md + docs/carplay/00_ARCHITECTURE.md.
        //
        // CORRECTED 2026-08-10. This comment previously said voice ":9003 has no OCBM channel yet
        // (forward is a harmless no-op there)" — false since CH_ALT_AUDIO landed, and contradicted 50
        // lines below in this same function. It is worth naming because that exact sentence
        // propagated into docs/ops/04_OPEN_ITEMS.md item 6 ("nav voice on :9003 has no OCBM channel yet"), survived a
        // documentation audit, and produced a WRONG STATUS REPORT to the owner on 2026-08-10 — while
        // nav prompts, Siri speech and two-way calls were all working on his hardware. A stale
        // comment in a hot path is not cosmetic; it is the seed of a stale doc.
        let fwd_enc = crate::levers::fwd_enc(); // safe default: forward-encrypted unless OCBM_FWD_ENC explicitly =0/false/off/empty
        // Route to the PERSISTENT per-sink connection (shared across all stream SETUPs), not a fresh
        // per-stream socket — see MEDIA_SINK/VOICE_SINK. Route by audioType (media → :9002) not stream
        // type: wired media is stream type 100 (PCM), NOT 102.
        let (sink, port, label): (&Mutex<SinkSlot>, u16, &str) = if is_media {
            (&MEDIA_SINK, 9002, "media")
        } else {
            (&VOICE_SINK, 9003, "voice")
        };
        eprintln!("[audio] stream {stream_type} {codec:?} → {label} :{port} ({sr}Hz {ch}ch)");
        let mut buf = [0u8; 4096];
        let mut frames: u64 = 0;
        let mut drops: u64 = 0;
        let mut key_sent = false; // fwd-enc: whether the current seam connection has been handed the key
        // Reused per-packet framing buffer (opt #3): hoisted out of the loop + clear()'d each packet so the
        // fwd-enc path does NO per-RTP-packet heap alloc (grown once, kept). Wired PCM 48k is the highest
        // packet-rate audio case, so this removes the one remaining audio-lane alloc there.
        let mut framed_buf: Vec<u8> = Vec::new();
        loop {
            let (n, _) = match sock.recv_from(&mut buf) {
                Ok(x) => x,
                Err(ref e) if is_timeout(e) => {
                    if alive.load(Ordering::Acquire) {
                        continue; // idle stream, still live — keep waiting
                    }
                    break; // session torn down
                }
                Err(_) => break,
            };
            if n < MIN_AUDIO_PACKET {
                continue;
            }
            activity.store(now_ms(), Ordering::Relaxed); // A/V progress → feeds the idle watchdog
            let pkt = &buf[..n];
            if frames == 0 && drops == 0 {
                if let Ok(p) = std::env::var("CARPLAY_AUDIO_CAPTURE") {
                    let mut cap = Vec::new();
                    cap.extend_from_slice(&shared);
                    cap.extend_from_slice(&scid.to_le_bytes());
                    cap.extend_from_slice(&key);
                    cap.extend_from_slice(&(stream_type as u32).to_le_bytes());
                    cap.extend_from_slice(pkt);
                    let _ = std::fs::write(format!("{p}.{stream_type}"), &cap);
                    eprintln!("[audio] stream {stream_type}: captured first packet ({n}B)");
                }
            }
            if fwd_enc {
                // Seam framing v2 (all-rates/all-streams): EVERY message is scid-tagged so concurrent
                // streams sharing one sink (telephony + alert on voice) can't clobber each other. The
                // voice sink (:9003) now has an OCBM channel (CH_ALT_AUDIO) — both sinks speak the
                // identical framing. (Re)hand key + format whenever the seam (re)connects, so a
                // mid-stream reconnect never leaves the host keyless or format-blind.
                if !key_sent {
                    let mut km = Vec::with_capacity(1 + 32 + 8);
                    km.push(0x00u8); // SEAM_KEY
                    km.extend_from_slice(&key);
                    km.extend_from_slice(&scid.to_le_bytes());
                    let mut framed = (km.len() as u32).to_be_bytes().to_vec();
                    framed.extend_from_slice(&km);
                    key_sent = forward_to_sink(sink, port, &framed);
                    if key_sent {
                        // SEAM_FORMAT: [0x02][scid 8][codec][rate u32 LE][ch][bits][audio_type].
                        // Wired is PCM at various rates; the codec byte is the wireless prestage hook
                        // (AAC-LC/ELD/OPUS forward identically — the box never touches the payload).
                        let codec_w: u8 = match codec {
                            AudioCodec::Pcm => 0,
                            AudioCodec::AacLc => 1,
                            AudioCodec::AacEld => 2,
                            AudioCodec::Opus => 3,
                        };
                        let bits: u8 = if matches!(codec, AudioCodec::Pcm) {
                            16
                        } else {
                            0
                        };
                        let mut fm = Vec::with_capacity(1 + 8 + 8);
                        fm.push(0x02u8); // SEAM_FORMAT
                        fm.extend_from_slice(&scid.to_le_bytes());
                        fm.push(codec_w);
                        fm.extend_from_slice(&sr.to_le_bytes());
                        fm.push(ch as u8);
                        fm.push(bits);
                        fm.push(atype);
                        let mut framed = (fm.len() as u32).to_be_bytes().to_vec();
                        framed.extend_from_slice(&fm);
                        key_sent = forward_to_sink(sink, port, &framed);
                    }
                    if key_sent && frames == 0 {
                        eprintln!(
                            "[audio] fwd-enc: handed {label} key+format (scid={scid} {codec:?} {sr}Hz {ch}ch atype={atype}) to seam"
                        );
                    }
                }
                // Single allocation: [len u32 BE][SEAM_PKT 0x01][scid 8][pkt]. Was 3 allocs + 2 copies
                // per RTP packet (build `fm`, then `to_vec()` a 4-byte Vec and grow it past capacity) —
                // the audio analogue of the video pfx/head fix (perf audit 2026-08-09). Same bytes on
                // the wire; wired PCM 48k is the highest packet-rate case so this matters most there.
                let body_len = 1 + 8 + pkt.len();
                framed_buf.clear();
                framed_buf.extend_from_slice(&(body_len as u32).to_be_bytes());
                framed_buf.push(0x01u8); // SEAM_PKT (raw encrypted RTP)
                framed_buf.extend_from_slice(&scid.to_le_bytes());
                framed_buf.extend_from_slice(pkt);
                if !forward_to_sink(sink, port, &framed_buf) {
                    key_sent = false; // seam dropped → resend key+format after reconnect
                }
                frames += 1;
                if frames == 1 {
                    eprintln!(
                        "[audio] fwd-enc: forwarding ENCRYPTED {label} RTP (no on-box decrypt)"
                    );
                }
                continue;
            }
            // AAD = ts‖ssrc (RTP header bytes 4..12) for ALL audio streams — verified live: stream
            // 100 (ELD) decrypts with this, not the full 12-byte header.
            let aad: &[u8] = &pkt[4..12];
            let au = match decrypt_audio_aad(&key, pkt, aad) {
                Some(au) => au,
                None => {
                    drops += 1;
                    if drops == 1 {
                        eprintln!("[audio] stream {stream_type}: decrypt failed");
                    }
                    continue;
                }
            };
            if let Some(p) = au_dump_path() {
                use std::io::Write as _;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(format!("{p}.t{stream_type}.s{scid}"))
                {
                    let _ = f.write_all(&(au.len() as u32).to_le_bytes());
                    let _ = f.write_all(&au);
                }
            }
            let au_len = au.len();
            let framed = match codec {
                // Raw LPCM (wired) — media :9002 gets the samples verbatim (fixed 48k/16/stereo, the
                // sink plays it raw). Voice :9003 carries MIXED formats (telephony/Siri 16k mono,
                // alert/nav 48k stereo) over ONE persistent socket, so each AU is rate/ch-tagged.
                AudioCodec::Pcm if is_media => au,
                AudioCodec::Pcm => tag_voice(&au, sr, ch, atype),
                AudioCodec::AacLc => match adts_from_aac_lc(&au, sr, ch as u8) {
                    Some(f) => f,
                    None => continue,
                },
                AudioCodec::AacEld => tag_voice(&au, sr, ch, atype),
                // Opus (wireless): the box doesn't decode it — forward the AU rate/ch-tagged like ELD so
                // the host decoder handles it. (In the default fwd-enc mode the encrypted payload is
                // forwarded whole and this on-box path isn't taken.)
                AudioCodec::Opus => tag_voice(&au, sr, ch, atype),
            };
            // Persistent sink: a write failure means carlink is momentarily down — drop this AU and
            // let the sink reconnect on the next one. Do NOT break (the sink is shared across re-SETUPs).
            if !forward_to_sink(sink, port, &framed) {
                drops += 1;
                continue;
            }
            frames += 1;
            if frames == 1 {
                eprintln!("[audio] stream {stream_type}: first AU forwarded ({au_len} B)");
            }
            // No per-frame/per-100 running log (churn): the session-boundary total below is enough.
        }
        eprintln!("[audio] stream {stream_type} ended after {frames} frames ({drops} drops)");
    });
}

/// `CARPLAY_AU_DUMP` — decoded-AU dump path. SPAWN-scoped (set by the parent before exec, never
/// written at runtime), so it is read ONCE: the old per-AU `env::var` was a `getenv` on every audio
/// packet, on the hot path.
fn au_dump_path() -> Option<&'static str> {
    static V: OnceLock<Option<String>> = OnceLock::new();
    V.get_or_init(|| std::env::var("CARPLAY_AU_DUMP").ok()).as_deref()
}

/// Drain a UDP socket (keep-alive beacons): receive + discard, keeping the port live.
fn spawn_udp_drain(sock: UdpSocket, alive: Arc<AtomicBool>) {
    thread::spawn(move || {
        sock.set_read_timeout(Some(SHUTDOWN_POLL)).ok();
        let mut buf = [0u8; 256];
        loop {
            match sock.recv_from(&mut buf) {
                Ok(_) => {}
                Err(ref e) if is_timeout(e) => {
                    if !alive.load(Ordering::Acquire) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
}

/// Like [`spawn_udp_drain`] but stamps `activity` on every received beacon (#1130). The low-power
/// keepAlive stream is precisely the signal that a SLEEPING iPhone is still present, so it must feed
/// the control-loop idle watchdog — otherwise the 30s no-A/V backstop tears down a phone that is
/// dutifully beaconing (media paused, screen dimmed), forcing a needless reconnect.
fn spawn_keepalive(sock: UdpSocket, alive: Arc<AtomicBool>, activity: Arc<AtomicU64>) {
    thread::spawn(move || {
        sock.set_read_timeout(Some(SHUTDOWN_POLL)).ok();
        let mut buf = [0u8; 256];
        loop {
            match sock.recv_from(&mut buf) {
                Ok(_) => activity.store(now_ms(), Ordering::Relaxed),
                Err(ref e) if is_timeout(e) => {
                    if !alive.load(Ordering::Acquire) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
}

/// Current time as a 64-bit NTP timestamp (seconds since 1900 in the high 32 bits, fraction in the low).
fn ntp_now() -> u64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs() + 2_208_988_800; // 1900→1970 offset
    let frac = ((d.subsec_nanos() as u64) << 32) / 1_000_000_000;
    (secs << 32) | frac
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two RemoteControlSession channels are BOTH stream type 130, distinguished only by
    /// `channelID`. Keyed on the bare type, the second evicted the first — killing the live iAP
    /// channel's listener and, via `clear_if`, the outbound sink. Unreachable today, armed the day
    /// `logTransfer`/`vehicleStateProtocol` are advertised for nav/cluster work.
    #[test]
    fn rcs_channels_of_the_same_type_do_not_evict_each_other() {
        let s = AvSession::new();
        let iap = s.stream_flag_keyed(130, "AA:BB-RCS-1".into());
        let log = s.stream_flag_keyed(130, "AA:BB-RCS-2".into());
        assert!(iap.load(Ordering::Acquire), "the iAP channel was evicted by an unrelated RCS");
        assert!(log.load(Ordering::Acquire));

        // Re-SETUP of the SAME channel must still supersede — the archived retry storm re-SETUPs one
        // channel repeatedly, and letting those accumulate is the #406/#413 thread leak.
        let iap2 = s.stream_flag_keyed(130, "AA:BB-RCS-1".into());
        assert!(!iap.load(Ordering::Acquire), "re-SETUP of the same channel must supersede");
        assert!(iap2.load(Ordering::Acquire));
        assert!(log.load(Ordering::Acquire), "superseding one channel must not touch another");
    }

    /// A/V streams keep supersede-by-type exactly as before — one screen, one alt-screen.
    #[test]
    fn av_streams_still_supersede_by_type() {
        let s = AvSession::new();
        let first = s.stream_flag(110);
        let second = s.stream_flag(110);
        assert!(!first.load(Ordering::Acquire));
        assert!(second.load(Ordering::Acquire));
        // A different type is untouched.
        let alt = s.stream_flag(111);
        let _ = s.stream_flag(110);
        assert!(alt.load(Ordering::Acquire));
    }

    /// Phase-1 SETUP is idempotent, per Apple's reference: `_ControlSetup` is guarded by
    /// `require_action( !inSession->controlSetup, … )`, so a repeat binds nothing and the ports the
    /// phone is already using stay valid. We used to re-bind and hand iOS NEW ports — a divergence
    /// from the reference, and the reason a repeat SETUP stranded the previous `spawn_timing` /
    /// `spawn_keepalive` threads on an Arc nothing could flip again.
    #[test]
    fn repeat_phase1_setup_is_idempotent() {
        let mut s = AvSession::new();
        let first = s.setup(&[]);
        let ports = |resp: &[u8]| -> (u64, u64) {
            let d = Value::from_reader(Cursor::new(resp)).unwrap().into_dictionary().unwrap();
            (
                d.get("timingPort").unwrap().as_unsigned_integer().unwrap(),
                d.get("eventPort").unwrap().as_unsigned_integer().unwrap(),
            )
        };
        let arc1 = s.alive.clone();
        let p1 = ports(&first);

        let second = s.setup(&[]); // repeat phase-1 SETUP, no TEARDOWN between
        assert_eq!(ports(&second), p1, "a repeat SETUP must reuse the ports the phone is using");
        assert!(Arc::ptr_eq(&arc1, &s.alive), "a repeat SETUP must not re-mint the liveness flag");
        assert!(arc1.load(Ordering::Acquire), "the session must still be live");
    }

    /// After a full TEARDOWN the next SETUP DOES bind again — and must reap the old flag rather than
    /// leaving the previous session's threads parked on it for the process lifetime.
    #[test]
    fn setup_after_teardown_rebinds_and_reaps_the_old_flag() {
        let mut s = AvSession::new();
        let _ = s.setup(&[]);
        let first = s.alive.clone();
        s.reset();
        assert!(!first.load(Ordering::Acquire));

        let _ = s.setup(&[]);
        assert!(!Arc::ptr_eq(&first, &s.alive), "a post-TEARDOWN SETUP must mint a fresh flag");
        assert!(!first.load(Ordering::Acquire), "the old flag must stay false");
        assert!(s.alive.load(Ordering::Acquire));
    }

    #[test]
    fn phase1_response_has_ports_and_opens_sockets() {
        let mut s = AvSession::new();
        let resp = s.setup(&[]); // empty/invalid plist ⇒ treated as no-streams ⇒ phase 1
        let d = Value::from_reader(Cursor::new(&resp))
            .unwrap()
            .into_dictionary()
            .unwrap();
        assert!(d.get("timingPort").unwrap().as_unsigned_integer().unwrap() > 0);
        assert!(d.get("eventPort").unwrap().as_unsigned_integer().unwrap() > 0);
    }

    #[test]
    fn ntp_now_is_after_2020() {
        // 2020-01-01 in NTP seconds ≈ 3786825600
        assert!((ntp_now() >> 32) > 3_786_825_600);
    }
}
