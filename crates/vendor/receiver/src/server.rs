//! `ControlServer` — the sans-IO RTSP control-plane state machine. It ties the [`rtsp`] message
//! codec + routing to the [`pairing`] / [`mfi`] handlers and, once pair-verify completes, flips the
//! connection to the encrypted [`rtsp::control`] channel. Transport-free so it is unit-testable with
//! a scripted controller; a thin tokio wrapper (Phase 6) just shuttles bytes to/from a TCP socket.
//!
//! Plaintext until pair-verify M4 is emitted; subsequent requests/responses are ChaCha20-Poly1305
//! framed. The pair-setup / pair-verify / auth-setup endpoints drive the real crypto from earlier
//! phases; `/info`, SETUP, RECORD etc. currently return routed placeholders (the session-bound
//! handlers land with the A/V session phase).

use mfi::auth_client::MfiSigner;
use mfi::sap::MfiSapServer;
use pairing::setup::{PairSetupServer, PeerSaver};
use pairing::verify::{Identity, PairVerifyServer, PeerStore};
use rtsp::control::{derive_control_keys, ControlChannel};
use rtsp::message::Request;
use rtsp::message::Response;
use rtsp::route::Route;

/// Combined pairing store (lookup for pair-verify + save for pair-setup).
pub trait Pairings: PeerStore + PeerSaver {}
impl<T: PeerStore + PeerSaver> Pairings for T {}

/// Pull the pair-setup/pair-verify `State` TLV (type 0x06) out of a request body for logging —
/// neither `pairing::setup::SetupError` nor `pairing::verify::PairError` exposes which M-state it
/// failed at, so we re-derive it from the wire the same way `exchange()` itself does.
fn request_tlv_state(body: &[u8]) -> String {
    pairing::tlv::decode(body)
        .ok()
        .and_then(|items| {
            items
                .into_iter()
                .find(|(t, _)| *t == pairing::crypto::tlv_type::STATE)
                .and_then(|(_, v)| v.first().copied())
        })
        .map(|n| n.to_string())
        .unwrap_or_else(|| "?".to_string())
}

/// Why a control connection was torn down.
///
/// Each variant carries its cause: `net.rs` logs this `{e:?}` as the only record of WHY a session
/// ended, and collapsing five unrelated failures into `Protocol`/`Decrypt` made that line useless --
/// an oversized frame even reported itself as a decrypt failure.
#[derive(Debug, PartialEq, Eq)]
pub enum ServerError {
    /// A genuinely malformed request. (A well-formed-but-incomplete request — split across frames —
    /// is buffered and awaited, NOT reported as this.)
    Protocol(rtsp::message::ParseError),
    /// The accumulator crossed `MAX_PLAINTEXT_ACCUM` without ever completing a request.
    Runaway,
    /// An inbound encrypted frame failed authentication, or declared an oversized length.
    Frame(rtsp::control::FrameError),
    /// A pair-setup or pair-verify exchange failed. Carries the underlying `pairing::` error's
    /// Display text so `net.rs`'s `{e:?}` teardown log names the real cause instead of "Pairing".
    Pairing(String),
    /// The MFi-SAP (`/auth-setup`) exchange failed.
    AuthSetup,
}

const OCTET: &str = "application/octet-stream";

/// Runaway guard for the decrypted-plaintext accumulator: a peer that streams frames without ever
/// completing an RTSP message must not grow it without bound. Aligned to the parser's own body cap
/// (`rtsp::message::MAX_BODY`) plus a header slack, so the encrypted channel is exactly as permissive
/// as the plaintext one — it never pre-empts a request `Request::parse` would accept, yet still bounds
/// a runaway stream. (No real CarPlay control message approaches even a fraction of this.)
const MAX_PLAINTEXT_ACCUM: usize = rtsp::message::MAX_BODY + 64 * 1024;
const BPLIST: &str = "application/x-apple-binary-plist";

/// One RTSP control connection.
pub struct ControlServer<'a, P: Pairings, S: MfiSigner> {
    identity: &'a Identity,
    setup_code: Vec<u8>,
    peers: P,
    signer: S,
    setup: Option<PairSetupServer<'a>>,
    verify: Option<PairVerifyServer<'a>>,
    sap: MfiSapServer,
    channel: Option<ControlChannel>,
    activate: Option<[u8; 32]>,
    shared: Option<[u8; 32]>,
    info_plist: Vec<u8>,
    session: Box<dyn crate::session::SessionDelegate>,
    verbose: bool,
    /// The advertised MAIN-display pixel width (displays[].widthPixels). Threaded in from
    /// airplayd so `forward_corner_mask` can tell the host the exact width iOS's streamed
    /// `topLeftCornerMask` corresponds to — the host scales the corner by `png_n / display_width`.
    /// 0 = unset (corner-mask forwarding is skipped).
    display_width: u32,
    rx: Vec<u8>,
    /// Decrypted control-channel plaintext awaiting a full RTSP message. A control request that iOS
    /// splits across ChaCha frames (it chunks at ~1 KB, like our own `encrypt_frame`) is reassembled
    /// here before parsing, so an incomplete request is buffered — not mis-parsed into a false
    /// mid-session teardown. Only used on the encrypted channel; reset per connection.
    ptx: Vec<u8>,
}

impl<'a, P: Pairings, S: MfiSigner> ControlServer<'a, P, S> {
    pub fn new(
        identity: &'a Identity,
        setup_code: impl Into<Vec<u8>>,
        peers: P,
        signer: S,
        info_plist: Vec<u8>,
    ) -> Self {
        Self {
            identity,
            setup_code: setup_code.into(),
            peers,
            signer,
            setup: None,
            verify: None,
            sap: MfiSapServer::new(),
            channel: None,
            activate: None,
            shared: None,
            info_plist,
            session: Box::new(crate::session::NoSession),
            verbose: false,
            display_width: 0,
            rx: Vec::new(),
            ptx: Vec::new(),
        }
    }

    /// Enable per-request logging to stderr (for the daemon's live bring-up).
    pub fn verbose(mut self, on: bool) -> Self {
        self.verbose = on;
        self
    }

    /// The advertised main-display pixel width — used to scale iOS's streamed corner mask on the host
    /// (`server.rs::forward_corner_mask`). Set from the same DeviceConfig that built `/info`.
    pub fn display_width(mut self, w: u32) -> Self {
        self.display_width = w;
        self
    }

    /// Attach the A/V session delegate that handles SETUP/RECORD/TEARDOWN (the data plane).
    pub fn session(mut self, delegate: Box<dyn crate::session::SessionDelegate>) -> Self {
        self.session = delegate;
        self
    }

    /// True once the connection has flipped to the encrypted control channel (post pair-verify).
    pub fn is_encrypted(&self) -> bool {
        self.channel.is_some()
    }

    /// The pair-verify ephemeral shared secret, available after a successful verify (seeds the
    /// session's per-stream keys in the A/V phase).
    pub fn session_secret(&self) -> Option<[u8; 32]> {
        self.shared
    }

    /// Milliseconds since the last A/V data on this session, or `None` if no A/V has flowed yet. The
    /// control-loop idle watchdog reads this so a live session with flowing A/V but a quiet control
    /// channel isn't falsely torn down on a control-read timeout.
    pub fn av_idle_ms(&self) -> Option<u64> {
        let ts = self.session.last_activity()?.load(std::sync::atomic::Ordering::Relaxed);
        (ts != 0).then(|| crate::session::now_ms().saturating_sub(ts))
    }

    /// Feed received bytes; returns the bytes to write back (responses, framed if encrypted).
    pub fn feed(&mut self, input: &[u8]) -> Result<Vec<u8>, ServerError> {
        self.rx.extend_from_slice(input);
        let mut out = Vec::new();
        while let Some((req, encrypted)) = self.next_request()? {
            // next_request() drains what it consumed itself (ciphertext frames from `rx`, and the
            // parsed request from the plaintext accumulator `ptx`).
            if self.verbose {
                eprintln!(
                    "[receiver] {} {} ({}, {} B body)",
                    req.method,
                    req.path(),
                    if encrypted { "enc" } else { "plain" },
                    req.body.len(),
                );
            }
            let resp = self.handle(&req)?;
            let bytes = resp.serialize();
            if encrypted {
                let ch = self.channel.as_mut().expect("encrypted ⇒ channel present");
                out.extend_from_slice(&ch.encrypt_frame(&bytes));
            } else {
                out.extend_from_slice(&bytes);
            }
            // pair-verify just succeeded → all subsequent traffic is encrypted.
            if let Some(secret) = self.activate.take() {
                self.channel = Some(ControlChannel::new(derive_control_keys(&secret)));
                self.shared = Some(secret);
                self.session.on_paired(secret);
                if self.verbose {
                    eprintln!("[receiver] pair-verify OK → control channel encrypted");
                }
            }
        }
        Ok(out)
    }

    fn next_request(&mut self) -> Result<Option<(Request, bool)>, ServerError> {
        if let Some(channel) = self.channel.as_mut() {
            // Encrypted control channel: decrypt-drain every COMPLETE frame off the ciphertext buffer
            // into the plaintext accumulator, THEN parse one full RTSP message out of it. This
            // reassembles a request iOS split across frames (it chunks at ~1 KB); a well-formed-but-
            // partial request stays buffered instead of being mis-parsed as malformed → a false
            // mid-session teardown. (The old code parsed each frame's plaintext in isolation.)
            // Borrowing the field directly (if-let, not is_some + expect) keeps `self.rx`/`self.ptx`
            // usable — field-disjoint borrows.
            loop {
                let frame = channel
                    .decrypt_frame(&self.rx)
                    .map_err(ServerError::Frame)?;
                match frame {
                    Some((plaintext, used)) => {
                        self.ptx.extend_from_slice(&plaintext);
                        self.rx.drain(..used);
                    }
                    None => break, // partial frame — wait for more ciphertext
                }
            }
            if self.ptx.len() > MAX_PLAINTEXT_ACCUM {
                return Err(ServerError::Runaway); // frames without a complete request
            }
            match Request::parse(&self.ptx).map_err(ServerError::Protocol)? {
                Some((req, consumed)) => {
                    self.ptx.drain(..consumed);
                    Ok(Some((req, true)))
                }
                None => Ok(None), // full request not yet assembled — wait for more frames (NOT a teardown)
            }
        } else {
            // QC 2026-07-25 (HIGH): the SAME runaway guard the encrypted path above has always had.
            // On the plaintext (pre-pair-verify) path `rx` IS the accumulator, and `Request::parse`
            // returns Ok(None) — with no size check of its own — for as long as no `\r\n\r\n`
            // terminator arrives. Any peer with network adjacency to `[::]:5000` (the wlan0 AP subnet,
            // the NCM link) could therefore stream header-less bytes until allocation failed, and
            // under `panic = "abort"` that kills airplayd outright on a 123 MB no-swap box. Pre-auth,
            // no session required. Deliberately the same bound as the encrypted path so this can never
            // reject a request `Request::parse` would have accepted; a tighter pre-encryption cap is
            // defensible (real pair-setup/verify/info messages are a few KB at most) but is a
            // behavioral change, so it is left as a separate decision.
            if self.rx.len() > MAX_PLAINTEXT_ACCUM {
                return Err(ServerError::Runaway);
            }
            match Request::parse(&self.rx).map_err(ServerError::Protocol)? {
                Some((req, consumed)) => {
                    self.rx.drain(..consumed);
                    Ok(Some((req, false)))
                }
                None => Ok(None),
            }
        }
    }

    fn handle(&mut self, req: &Request) -> Result<Response, ServerError> {
        let route = Route::classify(&req.method, req.path());
        // Pre-encryption gate. Authority: the CURRENT Apple receiver (CarPlaySDK.framework in the 2026
        // CarPlay Simulator) refuses any non-pairing request before pair-verify with HTTP 401
        // ("### Unverified RTSP request denied"); R14G17 (2017) had no such gate, which is the hole this
        // closes. We gate exactly the session-plane routes that reach the A/V session delegate and its
        // zero-key `setup_phase2` fallback (session.rs `shared.unwrap_or([0u8; 32])`), so a plaintext
        // peer with adjacency to `[::]:5000` cannot drive media setup pre-pair-verify. These six routes
        // are exclusively post-pair-verify in a correct exchange (the channel flips to encrypted in
        // `feed()` before the next request parses), so this never rejects legitimate traffic.
        //
        // NOTE (deviation from the Simulator, deliberate): Apple's gate is an ALLOW-LIST (deny by
        // default); this is a DENY-LIST. We still permit `/info` + `OPTIONS` (and the empty-ack routes)
        // pre-encryption to avoid regressing the discovery handshake (owner directive: wired must not
        // regress) — captures show `/info` arriving encrypted, but docs/carplay/03_SDK_GROUND_TRUTH.md documents a possible plaintext
        // first-contact `GET /info`, and neither `/info` (public data) nor `OPTIONS` reaches session
        // state, so gating them adds no security. Consequence of the deny-list: a NEW session-plane route
        // added to the match below defaults to ALLOWED — add it to this guard when introduced.
        if !self.is_encrypted()
            && matches!(
                route,
                Route::Setup
                    | Route::Record
                    | Route::Teardown
                    | Route::Command
                    | Route::GetParameter
                    | Route::SetParameter
            )
        {
            if self.verbose {
                eprintln!(
                    "[receiver] unverified request denied: {} {}",
                    req.method,
                    req.path()
                );
            }
            return Ok(Response::status(req, 401, "Unauthorized"));
        }
        match route {
            Route::PairSetup => {
                if let Some(p) = pairsetup_dump_path() {
                    use std::io::Write as _;
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p)
                    {
                        let _ = f.write_all(&(req.body.len() as u32).to_le_bytes());
                        let _ = f.write_all(&req.body);
                    }
                }
                if self.setup.is_none() {
                    self.setup =
                        Some(PairSetupServer::new(self.identity, self.setup_code.clone()));
                }
                let setup = self.setup.as_mut().unwrap();
                let m_in = request_tlv_state(&req.body);
                match setup.exchange(&req.body, &mut self.peers) {
                    Ok((body, _done)) => {
                        if self.verbose {
                            eprintln!("[receiver] pair-setup M{m_in} ok");
                        }
                        Ok(Response::ok(req, Some(OCTET), body))
                    }
                    Err(e) => {
                        eprintln!("[receiver] pair-setup M{m_in} FAIL: {e}");
                        Err(ServerError::Pairing(e.to_string()))
                    }
                }
            }
            Route::PairVerify => {
                if self.verify.is_none() {
                    self.verify = Some(PairVerifyServer::new(self.identity));
                }
                let verify = self.verify.as_mut().unwrap();
                let m_in = request_tlv_state(&req.body);
                match verify.exchange(&req.body, &self.peers) {
                    Ok((body, done)) => {
                        if done {
                            // Some only if verification actually succeeded; None ⇒ stays plaintext.
                            self.activate = verify.shared_secret();
                        }
                        if self.verbose {
                            eprintln!("[receiver] pair-verify M{m_in} ok");
                        }
                        Ok(Response::ok(req, Some(OCTET), body))
                    }
                    Err(e) => {
                        eprintln!("[receiver] pair-verify M{m_in} FAIL: {e}");
                        Err(ServerError::Pairing(e.to_string()))
                    }
                }
            }
            Route::AuthSetup => match self.sap.exchange(&req.body, &mut self.signer) {
                Ok(body) => {
                    if self.verbose {
                        eprintln!("[receiver] auth-setup (MFi-SAP) OK → {} B M2", body.len());
                    }
                    Ok(Response::ok(req, Some(OCTET), body))
                }
                Err(e) => {
                    if self.verbose {
                        eprintln!("[receiver] auth-setup FAILED: {e:?}");
                    }
                    Err(ServerError::AuthSetup)
                }
            },
            Route::Options => {
                let mut r = Response::ok(req, None, Vec::new());
                r.headers.push((
                    "Public".into(),
                    "ANNOUNCE, SETUP, RECORD, PAUSE, FLUSH, TEARDOWN, OPTIONS, GET_PARAMETER, \
                     SET_PARAMETER, POST, GET, PUT"
                        .into(),
                ));
                Ok(r)
            }
            Route::Info => {
                // Dump the served /info when the corner-mask capture is armed, so we can inspect exactly
                // what per-view/display keys (incl. cornerMasks) reach the phone.
                if let Ok(dir) = std::env::var("CARPLAY_CORNERMASK_CAPTURE") {
                    let _ = std::fs::write(format!("{dir}/served_info.bplist"), &self.info_plist);
                }
                Ok(Response::ok(req, Some(BPLIST), self.info_plist.clone()))
            }
            Route::Setup => {
                if let Ok(p) = std::env::var("CARPLAY_SETUP_CAPTURE") {
                    let _ = std::fs::write(&p, &req.body);
                }
                capture_corner_mask("setup", &req.body);
                // Forward iOS's exact per-resolution corner mask to the host (docs/carplay/06_AV_PIPELINE.md Phase-3b): the
                // SETUP dict carries `topLeftCornerMask`. Gated on the cornerMasks lever inside.
                forward_corner_mask(self.display_width, &req.body);
                match self.session.setup(&req.body) {
                    Ok(resp) => Ok(Response::ok(req, Some(BPLIST), resp)),
                    // Mirrors `_ControlSetup`'s `require_noerr(err, exit)` (R14G17
                    // `AirPlayReceiverSession.c:900-914`): a genuine bind/setup failure answers 500,
                    // not 200 with an empty body. Keeps the connection open, same as the 401 gate
                    // above — the phone tears the session down on its own.
                    Err(crate::session::SetupError) => Ok(Response::status(req, 500, "Internal Server Error")),
                }
            }
            Route::Record => {
                let resp = self.session.record();
                Ok(Response::ok(req, None, resp))
            }
            Route::Teardown => {
                // Pass the body so the session can distinguish partial (streams[]) from full teardown.
                self.session.teardown(&req.body);
                Ok(Response::ok(req, None, Vec::new()))
            }
            Route::Command => {
                if let Ok(p) = std::env::var("CARPLAY_COMMAND_CAPTURE") {
                    use std::io::Write;
                    if let Ok(mut f) =
                        std::fs::OpenOptions::new().create(true).append(true).open(&p)
                    {
                        let _ = f.write_all(&(req.body.len() as u32).to_le_bytes());
                        let _ = f.write_all(&req.body);
                    }
                }
                Ok(Response::ok(req, Some(BPLIST), self.session.command(&req.body)))
            }
            Route::GetParameter => {
                // The C GET_PARAMETER returns a `text/parameters` body (volume/name). Answer the
                // common volume query (0.0 dB = full); a full parameter parser bound to the session
                // volume state lands with the A/V phase.
                Ok(Response::ok(req, Some("text/parameters"), b"volume: 0.000000\r\n".to_vec()))
            }
            // Remaining session-bound endpoints: routed acks (DiagInfo/Log are 200 OK in the C).
            Route::SetParameter => {
                // A likely carrier for the runtime corner-mask update (carEndpoint_updateDisplayCornerMasks);
                // capture it when armed. Body is otherwise unused (parameters are ACKed empty).
                capture_corner_mask("setparameter", &req.body);
                // SET_PARAMETER is iOS's runtime corner-mask update carrier
                // (carEndpoint_updateDisplayCornerMasks) — forward it too so a runtime change reaches
                // the host, not just the initial SETUP mask.
                forward_corner_mask(self.display_width, &req.body);
                Ok(Response::ok(req, None, Vec::new()))
            }
            Route::Flush | Route::Feedback | Route::DiagInfo | Route::Log => {
                Ok(Response::ok(req, None, Vec::new()))
            }
            Route::NotFound => Ok(Response::status(req, 404, "Not Found")),
            Route::NotImplemented => Ok(Response::status(req, 501, "Not Implemented")),
        }
    }
}

/// Recursively find the first value stored under `key` anywhere in a plist tree.
fn plist_find_key<'a>(v: &'a plist::Value, key: &str) -> Option<&'a plist::Value> {
    match v {
        plist::Value::Dictionary(d) => d
            .get(key)
            .or_else(|| d.iter().find_map(|(_, c)| plist_find_key(c, key))),
        plist::Value::Array(a) => a.iter().find_map(|c| plist_find_key(c, key)),
        _ => None,
    }
}

/// Corner-mask capture (Phase 1 of the corner-mask experiment). The accessory-side byte format is
/// undocumented — Apple's own CarPlaySDK/Simulator never decodes the buffer, it copies the opaque
/// `topLeftCornerMask` blob verbatim and hands it to the app delegate — so the ONLY way to learn the
/// wire bytes is to capture what the phone actually sends once we advertise the feature.
///
/// Armed by `CARPLAY_CORNERMASK_CAPTURE=<dir>`. When a request body carries `topLeftCornerMask` as a
/// Data value, its raw bytes are written to `<dir>/cornermask_<route>.bin` and the length + leading
/// magic bytes are logged (so PNG `89 50 4e 47` / JPEG `ff d8` / raw are identifiable at a glance). As a
/// safety net, any binary-plist body is frame-appended to `<dir>/<route>_bplists.dump` so the mask is
/// never lost if it rides a message shape we didn't anticipate. Gated entirely behind the env var, so
/// default builds do no extra work.
/// Forward iOS's streamed `topLeftCornerMask` PNG to the host app so it renders the EXACT corner curve
/// for the negotiated resolution (docs/carplay/06_AV_PIPELINE.md Phase-3b), replacing the app's hardcoded width-fraction guess.
///
/// Gated on `levers::cornermasks()` (nothing emitted when the feature is off → byte-identical default)
/// and on a known display width. Extracts the PNG from the request body (the SETUP dict, or the
/// SET_PARAMETER runtime update) and hands it + the advertised main-display width to the metadata seam.
/// `emit_cornermask` is try_lock/best-effort, safe to call on the RTSP control thread.
fn forward_corner_mask(display_width: u32, body: &[u8]) {
    if display_width == 0 || !crate::levers::cornermasks() {
        return;
    }
    if let Ok(val) = plist::Value::from_reader(std::io::Cursor::new(body)) {
        if let Some(plist::Value::Data(png)) = plist_find_key(&val, "topLeftCornerMask") {
            eprintln!(
                "[cornermask] forwarding topLeftCornerMask to host: {} B @ display_width={display_width}",
                png.len()
            );
            iap2_core::metadata::emit_cornermask(display_width, png);
        }
    }
}

fn capture_corner_mask(route: &str, body: &[u8]) {
    let dir = match std::env::var("CARPLAY_CORNERMASK_CAPTURE") {
        Ok(d) => d,
        Err(_) => return,
    };
    if let Ok(val) = plist::Value::from_reader(std::io::Cursor::new(body)) {
        match plist_find_key(&val, "topLeftCornerMask") {
            Some(plist::Value::Data(bytes)) => {
                let magic: String = bytes
                    .iter()
                    .take(16)
                    .map(|b| format!("{b:02x} "))
                    .collect();
                eprintln!(
                    "[cornermask] {route}: topLeftCornerMask Data len={} magic={}",
                    bytes.len(),
                    magic.trim_end()
                );
                let _ = std::fs::write(format!("{dir}/cornermask_{route}.bin"), bytes);
                return;
            }
            Some(_) => {
                eprintln!("[cornermask] {route}: topLeftCornerMask present but null / non-Data (no mask)")
            }
            None => {}
        }
    }
    // Fallback: preserve any binary-plist body verbatim (u32-LE length prefix + bytes) for offline
    // inspection, in case the mask rides a shape our key search missed.
    if body.starts_with(b"bplist") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!("{dir}/{route}_bplists.dump"))
        {
            let _ = f.write_all(&(body.len() as u32).to_le_bytes());
            let _ = f.write_all(body);
        }
    }
}

/// Bench lever: `CARPLAY_PAIRSETUP_DUMP=<path>` appends every raw `/pair-setup` request body to
/// `path`, length-prefixed (`[u32 LE len][body]`) — same format as `session.rs`'s `CARPLAY_CMD_DUMP`.
/// Resolved once per process (house pattern, `session.rs::au_dump_path`), so editing the env var
/// mid-run does nothing until restart.
fn pairsetup_dump_path() -> Option<&'static str> {
    static V: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    V.get_or_init(|| std::env::var("CARPLAY_PAIRSETUP_DUMP").ok()).as_deref()
}

#[cfg(test)]
mod tests {
    //! Control-frame reassembly (spec §1): a control request iOS splits across ChaCha frames must be
    //! reassembled and parsed as ONE request — an incomplete request must buffer, never false-teardown.
    use super::*;
    use rtsp::control::ControlKeys;

    struct DummyPeers;
    impl PeerStore for DummyPeers {
        fn find_peer(&self, _id: &[u8]) -> Option<[u8; 32]> {
            None
        }
    }
    impl PeerSaver for DummyPeers {
        fn save_peer(&mut self, _id: &[u8], _ltpk: [u8; 32]) {}
    }
    struct DummySigner;
    impl MfiSigner for DummySigner {
        fn copy_certificate(&mut self) -> std::io::Result<Vec<u8>> {
            Ok(Vec::new())
        }
        fn create_signature(&mut self, _digest: &[u8]) -> std::io::Result<Vec<u8>> {
            Ok(Vec::new())
        }
    }

    /// A client channel (whose write key == the server's read key, so the server decrypts it) plus a
    /// >1-frame RTSP request (2000-byte body ⇒ `encrypt_frame` chunks it into ≥2 frames).
    fn client_and_big_request(secret: &[u8]) -> (ControlChannel, Vec<u8>) {
        let sk = derive_control_keys(secret);
        let client = ControlChannel::new(ControlKeys { read: sk.write, write: sk.read });
        let body = vec![b'x'; 2000]; // > MAX_WRITE (1024) ⇒ multiple frames
        let mut req = format!(
            "POST /feedback RTSP/1.0\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        req.extend_from_slice(&body);
        assert!(req.len() > 1024, "request must exceed one frame to exercise reassembly");
        (client, req)
    }

    fn server_with_channel<'a>(
        identity: &'a Identity,
        secret: &[u8],
    ) -> ControlServer<'a, DummyPeers, DummySigner> {
        let mut s = ControlServer::new(identity, b"3939".to_vec(), DummyPeers, DummySigner, Vec::new());
        s.channel = Some(ControlChannel::new(derive_control_keys(secret)));
        s
    }

    #[test]
    fn encrypted_request_reassembled_when_all_frames_present() {
        let secret = [7u8; 32];
        let identity = Identity::new(b"testdev".to_vec(), [1u8; 32]);
        let mut srv = server_with_channel(&identity, &secret);
        let (mut client, req) = client_and_big_request(&secret);
        let frames = client.encrypt_frame(&req);
        let first = 2 + u16::from_le_bytes([frames[0], frames[1]]) as usize + 16;
        assert!(first < frames.len(), "expected ≥2 frames");

        srv.rx.extend_from_slice(&frames);
        let (got, enc) = srv.next_request().expect("no error").expect("one request");
        assert!(enc);
        assert_eq!(got.path(), "/feedback");
        assert_eq!(got.body.len(), 2000);
        assert!(srv.next_request().expect("no error").is_none(), "buffer drained");
    }

    #[test]
    fn encrypted_partial_request_buffers_not_teardown() {
        let secret = [9u8; 32];
        let identity = Identity::new(b"testdev".to_vec(), [2u8; 32]);
        let mut srv = server_with_channel(&identity, &secret);
        let (mut client, req) = client_and_big_request(&secret);
        let frames = client.encrypt_frame(&req);
        let first = 2 + u16::from_le_bytes([frames[0], frames[1]]) as usize + 16;

        // First frame only: a complete FRAME but an incomplete REQUEST → must buffer (Ok(None)), not error.
        srv.rx.extend_from_slice(&frames[..first]);
        assert!(
            srv.next_request().expect("partial request must NOT be an error").is_none(),
            "incomplete request must buffer, not tear down"
        );
        // Remainder arrives → the request completes.
        srv.rx.extend_from_slice(&frames[first..]);
        let (got, _) = srv.next_request().expect("no error").expect("completed request");
        assert_eq!(got.body.len(), 2000);
    }

    /// Pre-encryption gate (authority: the current CarPlaySDK returns 401 "### Unverified RTSP request
    /// denied"). A plaintext session-plane request before pair-verify must be refused 401 — never
    /// delegated to the zero-key session path — while the same request on the encrypted channel is
    /// delegated normally.
    #[test]
    fn plaintext_session_route_denied_401() {
        let identity = Identity::new(b"testdev".to_vec(), [3u8; 32]);
        let mut srv =
            ControlServer::new(&identity, b"3939".to_vec(), DummyPeers, DummySigner, Vec::new());
        let out = srv
            .feed(b"SETUP rtsp://x RTSP/1.0\r\nCSeq: 1\r\nContent-Length: 0\r\n\r\n")
            .expect("no server error");
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.starts_with("RTSP/1.0 401"),
            "plaintext SETUP must be denied 401, got status line: {:?}",
            text.lines().next()
        );
    }

    /// `_ControlSetup` on failure answers 500, not a 200 with an empty body (R14G17
    /// `AirPlayReceiverSession.c:900-914`). A `SessionDelegate::setup` that fails a bind/setup must
    /// surface as HTTP 500, not a `200 OK` with a zero-length bplist.
    struct FailingSetupSession;
    impl crate::session::SessionDelegate for FailingSetupSession {
        fn setup(&mut self, _request_plist: &[u8]) -> Result<Vec<u8>, crate::session::SetupError> {
            Err(crate::session::SetupError)
        }
    }

    #[test]
    fn failed_setup_answers_500_not_200_empty() {
        let secret = [6u8; 32];
        let identity = Identity::new(b"testdev".to_vec(), [5u8; 32]);
        let mut srv = server_with_channel(&identity, &secret).session(Box::new(FailingSetupSession));
        let sk = derive_control_keys(&secret);
        let mut client = ControlChannel::new(ControlKeys { read: sk.write, write: sk.read });
        let frames =
            client.encrypt_frame(b"SETUP rtsp://x RTSP/1.0\r\nCSeq: 1\r\nContent-Length: 0\r\n\r\n");
        let out = srv.feed(&frames).expect("no server error");
        let (plain, _used) = client.decrypt_frame(&out).expect("decrypt").expect("one frame");
        let text = String::from_utf8_lossy(&plain);
        assert!(
            text.starts_with("RTSP/1.0 500"),
            "a failed SETUP must answer 500, not a 200-with-empty-body, got status line: {:?}",
            text.lines().next()
        );
    }

    /// `ServerError::Pairing` must carry the underlying `pairing::` error's Display text — a bare
    /// `map_err(|_| ServerError::Pairing)` collapsed every pair-setup failure into one
    /// undifferentiated variant, which is what this test guards against regressing.
    #[test]
    fn pairing_error_preserves_underlying_message() {
        let identity = Identity::new(b"testdev".to_vec(), [8u8; 32]);
        let mut srv =
            ControlServer::new(&identity, b"3939".to_vec(), DummyPeers, DummySigner, Vec::new());
        // A pair-verify M1 on a fresh server is fine, but pair-setup with an out-of-sequence State
        // (9 — not 1/3/5) drives `SetupError::BadState` deterministically without needing a real SRP
        // exchange first.
        let body = pairing::tlv::encode(&[(pairing::crypto::tlv_type::STATE, &[9])]);
        let req = format!(
            "POST /pair-setup RTSP/1.0\r\nCSeq: 1\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let mut raw = req.into_bytes();
        raw.extend_from_slice(&body);
        let err = srv.feed(&raw).expect_err("bad pair-setup state must error");
        match err {
            ServerError::Pairing(msg) => assert_eq!(
                msg, "unexpected pair-setup state",
                "must preserve SetupError::BadState's Display text, got {msg:?}"
            ),
            other => panic!("expected ServerError::Pairing, got {other:?}"),
        }
    }

    #[test]
    fn encrypted_session_route_delegated() {
        let secret = [5u8; 32];
        let identity = Identity::new(b"testdev".to_vec(), [4u8; 32]);
        let mut srv = server_with_channel(&identity, &secret);
        let sk = derive_control_keys(&secret);
        let mut client = ControlChannel::new(ControlKeys { read: sk.write, write: sk.read });
        let frames =
            client.encrypt_frame(b"SETUP rtsp://x RTSP/1.0\r\nCSeq: 1\r\nContent-Length: 0\r\n\r\n");
        let out = srv.feed(&frames).expect("no server error");
        let (plain, _used) =
            client.decrypt_frame(&out).expect("decrypt ok").expect("a response frame");
        let text = String::from_utf8_lossy(&plain);
        assert!(
            text.starts_with("RTSP/1.0 200"),
            "encrypted SETUP must be delegated (200), got status line: {:?}",
            text.lines().next()
        );
    }
}
