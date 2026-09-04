//! The wireless-projection bootstrap: frame codec and state machine, with no I/O in it.
//!
//! Spec: `docs/androidauto/03_WIRELESS.md` §2c/§2d/§2f. This layer sits on the RFCOMM socket the
//! phone opens to our advertised channel, and its whole job is to end with the phone dialling the
//! TCP endpoint we named. From that first TCP byte it is the ordinary AA TLS session and none of
//! this is involved again.
//!
//! Deliberately transport-free: `Framer` takes bytes and `Bootstrap` takes decoded messages, so
//! the entire exchange is exercised by unit tests on the build host. The Bluetooth socket, the AP
//! bring-up and the TCP accept are the caller's problem. This is the half that is pure protocol,
//! and it is the half that a bench session cannot cheaply single-step.

use crate::proto;

// ---------------------------------------------------------------------------------------------
// Framing (§2c) -- [ length: u16 BE ][ message_id: u16 BE ][ payload: `length` bytes ]
// ---------------------------------------------------------------------------------------------

/// Bytes of header before the payload. `length` counts the PAYLOAD only, so a whole frame on the
/// wire is `HEADER_LEN + length`.
pub const HEADER_LEN: usize = 4;

/// Message ids (§2d). 1-7 appear in both reference implementations. 8, 9 and 11 appear in only one
/// of them: treat the ping pair as probable keepalive and `SETUP_INFO` as unidentified until it is
/// actually observed on the wire.
pub mod msg {
    pub const WIFI_START_REQUEST: u16 = 1;
    pub const WIFI_INFO_REQUEST: u16 = 2;
    pub const WIFI_INFO_RESPONSE: u16 = 3;
    pub const WIFI_VERSION_REQUEST: u16 = 4;
    pub const WIFI_VERSION_RESPONSE: u16 = 5;
    pub const WIFI_CONNECT_STATUS: u16 = 6;
    pub const WIFI_START_RESPONSE: u16 = 7;
    pub const WIFI_PING_REQUEST: u16 = 8;
    pub const WIFI_PING_RESPONSE: u16 = 9;
    pub const WIFI_SETUP_INFO: u16 = 11;

    /// Name for logging. An unrecognised id is worth seeing as a number.
    pub fn name(id: u16) -> &'static str {
        match id {
            WIFI_START_REQUEST => "WifiStartRequest",
            WIFI_INFO_REQUEST => "WifiInfoRequest",
            WIFI_INFO_RESPONSE => "WifiInfoResponse",
            WIFI_VERSION_REQUEST => "WifiVersionRequest",
            WIFI_VERSION_RESPONSE => "WifiVersionResponse",
            WIFI_CONNECT_STATUS => "WifiConnectionStatus",
            WIFI_START_RESPONSE => "WifiStartResponse",
            WIFI_PING_REQUEST => "WifiPingRequest",
            WIFI_PING_RESPONSE => "WifiPingResponse",
            WIFI_SETUP_INFO => "WifiSetupInfo",
            _ => "UNKNOWN",
        }
    }
}

/// One decoded frame off the RFCOMM stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub id: u16,
    pub payload: Vec<u8>,
}

/// Build a frame for the wire.
pub fn encode_frame(id: u16, payload: &[u8]) -> Vec<u8> {
    // The length field is a u16. Truncating instead of refusing would put a frame on the wire whose
    // header disagrees with its body, desynchronising the peer's parser for the rest of the session.
    assert!(
        payload.len() <= u16::MAX as usize,
        "frame payload {} exceeds the u16 length field",
        payload.len()
    );
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// The largest frame the wire format can express: the length field is a `u16`.
pub const MAX_FRAME: usize = HEADER_LEN + u16::MAX as usize;

/// Hard cap on buffered bytes — one whole maximum frame plus a partial next one. Reaching this
/// means the peer is not speaking this protocol, since a caller that drains can never need more.
const MAX_BUFFERED: usize = 2 * MAX_FRAME;

/// Reassembles frames from a byte stream.
///
/// RFCOMM is a stream, not a datagram service: a read can return half a frame, or two and a half.
/// The wired AA path already paid for assuming otherwise once -- the 57-94 s session death in
/// `01_SESSION_AND_AV.md` §2 was a reassembly bug -- so this buffers and only yields whole frames.
///
/// The buffer is explicitly BOUNDED. In normal operation it cannot grow past one frame because the
/// length field is a `u16` and a draining caller consumes each frame as it completes. A peer that
/// never completes one, or a caller that stops draining, would otherwise grow it without limit, so
/// crossing `MAX_BUFFERED` poisons the framer instead: it drops the buffer and yields nothing
/// further, turning a slow leak into a visible protocol failure the caller can act on.
#[derive(Default)]
pub struct Framer {
    buf: Vec<u8>,
    poisoned: bool,
}

impl Framer {
    pub fn new() -> Self {
        Framer { buf: Vec::new(), poisoned: false }
    }

    /// Feed freshly-read bytes in.
    pub fn push(&mut self, bytes: &[u8]) {
        if self.poisoned {
            return;
        }
        if self.buf.len().saturating_add(bytes.len()) > MAX_BUFFERED {
            self.buf.clear();
            self.buf.shrink_to_fit();
            self.poisoned = true;
            return;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// Has the peer overrun the buffer bound? A poisoned framer never yields another frame; the
    /// caller should drop the connection.
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Pull the next complete frame, or `None` while more bytes are needed.
    pub fn next_frame(&mut self) -> Option<Frame> {
        if self.poisoned || self.buf.len() < HEADER_LEN {
            return None;
        }
        let len = u16::from_be_bytes([self.buf[0], self.buf[1]]) as usize;
        let id = u16::from_be_bytes([self.buf[2], self.buf[3]]);
        let total = HEADER_LEN + len;
        if self.buf.len() < total {
            return None;
        }
        let payload = self.buf[HEADER_LEN..total].to_vec();
        self.buf.drain(..total);
        Some(Frame { id, payload })
    }

    /// Bytes held pending more input. Diagnostic only — a `Framer` that never drains is how a
    /// stalled bootstrap will present, so this is worth being able to log.
    #[allow(dead_code)]
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }
}

// ---------------------------------------------------------------------------------------------
// The bootstrap state machine (§2f)
// ---------------------------------------------------------------------------------------------

/// Everything the head unit must be able to tell the phone. Sourced from the running AP and the
/// app-pushed config -- never hardcoded here, and in particular the passphrase is read at runtime
/// and only ever written to the socket, never to a log or a file.
#[derive(Clone, Debug)]
pub struct ApParams {
    pub ssid: String,
    pub passphrase: String,
    /// AP MAC, `AA:BB:CC:DD:EE:FF`.
    pub bssid: String,
    pub security_mode: proto::SecurityMode,
    pub access_point_type: proto::AccessPointType,
    /// Our address on the AP subnet -- what the phone dials after it associates.
    pub ip_address: String,
    /// Our TCP port. Carried in `WifiStartRequest`, so it is ours to choose (§2f); config-driven,
    /// never a second literal.
    pub port: u16,
}

/// Where the exchange has got to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Nothing sent yet.
    Idle,
    /// Opening pair sent; waiting on the phone.
    Offered,
    /// Credentials handed over; the phone should now associate and dial us.
    CredentialsSent,
    /// The phone reported success. The TCP accept is what matters from here.
    Established,
    /// The phone reported a failure; `Bootstrap::failure` carries which.
    Failed,
}

/// What the caller should do as a result of feeding in a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Write these bytes to the RFCOMM socket.
    Send(Vec<u8>),
    /// Nothing to do.
    None,
}

/// Is the unverified ping reply (message ids 8/9) enabled? Off unless `AAW_ANSWER_PING=1`.
fn ping_replies_enabled() -> bool {
    std::env::var("AAW_ANSWER_PING").as_deref() == Ok("1")
}

/// Drives the bootstrap. Holds no socket and performs no I/O; feed it frames, act on what it returns.
pub struct Bootstrap {
    params: ApParams,
    phase: Phase,
    failure: Option<proto::Status>,
}

impl Bootstrap {
    pub fn new(params: ApParams) -> Self {
        Bootstrap { params, phase: Phase::Idle, failure: None }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The status the phone failed with, once `phase()` is `Failed`.
    pub fn failure(&self) -> Option<proto::Status> {
        self.failure
    }

    /// The opening pair, sent unprompted the moment the phone's RFCOMM connection lands (§2f step 2).
    ///
    /// The head unit speaks first here. A head unit that waits for the phone to say something will
    /// wait forever -- the same shape of deadlock the wired bridge hit when it claimed the owner
    /// flag only after a host connected (`02_ARBITRATION.md` §4).
    pub fn on_connect(&mut self) -> Vec<u8> {
        let mut out = encode_frame(msg::WIFI_VERSION_REQUEST, &proto::encode_empty());
        out.extend_from_slice(&encode_frame(
            msg::WIFI_START_REQUEST,
            &proto::encode_wifi_start_request(&self.params.ip_address, self.params.port),
        ));
        self.phase = Phase::Offered;
        out
    }

    /// Feed one received frame in. Returns what to send, if anything.
    ///
    /// Unknown and unhandled ids are ignored rather than fatal: the id space has at least three
    /// members we have never seen on a wire (§2d), and dropping the link over one would turn a
    /// harmless surprise into a failed bootstrap.
    pub fn on_frame(&mut self, frame: &Frame) -> Action {
        match frame.id {
            msg::WIFI_INFO_REQUEST => {
                // A failed bootstrap stays failed. Without this guard a phone that reports
                // WIFI_INCORRECT_CREDENTIALS and then simply retries would silently move us back to
                // CredentialsSent, the operator would never see the failure, and a later SUCCESS
                // would sail past the guard in `note_status` and report OK. `note_status` guarded
                // this and `on_frame` did not, which is the kind of asymmetry that survives review.
                // `Established` is excluded for the mirror-image reason: a phone that re-asks for
                // credentials after reporting success would otherwise walk the phase BACK to
                // CredentialsSent, so a machine kept alive past success would report a state the
                // exchange has already left.
                if self.phase == Phase::Failed || self.phase == Phase::Established {
                    return Action::None;
                }
                let payload = proto::encode_wifi_info_response(
                    &self.params.ssid,
                    &self.params.passphrase,
                    &self.params.bssid,
                    self.params.security_mode,
                    self.params.access_point_type,
                );
                self.phase = Phase::CredentialsSent;
                Action::Send(encode_frame(msg::WIFI_INFO_RESPONSE, &payload))
            }

            msg::WIFI_START_RESPONSE => {
                if let Some(resp) = proto::decode_wifi_start_response(&frame.payload) {
                    self.note_status(resp.status);
                }
                Action::None
            }

            msg::WIFI_CONNECT_STATUS => {
                if let Some(st) = proto::decode_wifi_connection_status(&frame.payload) {
                    self.note_status(st.status);
                }
                Action::None
            }

            // Ids 8/9 appear in exactly ONE source (a public Rust implementation) and in none of the
            // .proto files, openauto, or the stock firmware on this bench. Answering a ping we have
            // not confirmed exists means emitting an unknown id mid-bootstrap if 8 means something
            // else. Off by default; `AAW_ANSWER_PING=1` turns it on for the bench run that settles
            // it. See docs/androidauto/03_WIRELESS.md §2d.
            msg::WIFI_PING_REQUEST if ping_replies_enabled() => {
                Action::Send(encode_frame(msg::WIFI_PING_RESPONSE, &proto::encode_empty()))
            }

            // Version response is informational; the reference only logs it. Parsed by the caller
            // when it wants the values.
            msg::WIFI_VERSION_RESPONSE => Action::None,

            _ => Action::None,
        }
    }

    /// A success never downgrades an already-recorded failure, and a failure is sticky: the phone
    /// can send several status messages, and the first real error is the one worth reporting.
    fn note_status(&mut self, status: proto::Status) {
        if status.is_success() {
            // SUCCESS is only meaningful once the phone actually HAS the credentials. Nothing in
            // the protocol orders the phone's replies, so a WifiStartResponse{SUCCESS} can arrive
            // before WifiInfoRequest; treating that as "established" would end the exchange before
            // the SSID and passphrase were ever sent, and report success while doing it.
            if self.phase == Phase::CredentialsSent {
                self.phase = Phase::Established;
            }
        } else {
            self.phase = Phase::Failed;
            if self.failure.is_none() {
                self.failure = Some(status);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> ApParams {
        ApParams {
            ssid: "carlink".into(),
            passphrase: "secret".into(),
            bssid: "00:11:22:33:44:55".into(),
            security_mode: proto::SecurityMode::WPA2_ENTERPRISE,
            access_point_type: proto::AccessPointType::STATIC,
            ip_address: "192.168.4.1".into(),
            port: 5288,
        }
    }

    #[test]
    fn frame_header_is_big_endian_length_then_id() {
        let f = encode_frame(msg::WIFI_START_REQUEST, &[0xaa, 0xbb, 0xcc]);
        assert_eq!(&f[..4], &[0x00, 0x03, 0x00, 0x01]);
        assert_eq!(&f[4..], &[0xaa, 0xbb, 0xcc]);
    }

    #[test]
    fn empty_payload_frame_is_header_only() {
        let f = encode_frame(msg::WIFI_INFO_REQUEST, &[]);
        assert_eq!(f, vec![0x00, 0x00, 0x00, 0x02]);
    }

    /// RFCOMM is a stream: a frame can arrive one byte at a time and must not be yielded early.
    #[test]
    fn framer_reassembles_across_arbitrary_splits() {
        let whole = encode_frame(msg::WIFI_INFO_RESPONSE, &[1, 2, 3, 4, 5]);
        let mut f = Framer::new();
        for byte in &whole[..whole.len() - 1] {
            f.push(&[*byte]);
            assert!(f.next_frame().is_none(), "partial frame must not be yielded");
        }
        f.push(&[whole[whole.len() - 1]]);
        let got = f.next_frame().expect("complete frame");
        assert_eq!(got.id, msg::WIFI_INFO_RESPONSE);
        assert_eq!(got.payload, vec![1, 2, 3, 4, 5]);
        assert_eq!(f.buffered(), 0);
    }

    /// Two frames in one read must both come out, in order, with nothing left over.
    #[test]
    fn framer_yields_multiple_frames_from_one_push() {
        let mut bytes = encode_frame(msg::WIFI_VERSION_REQUEST, &[]);
        bytes.extend_from_slice(&encode_frame(msg::WIFI_START_REQUEST, &[9, 9]));
        let mut f = Framer::new();
        f.push(&bytes);
        assert_eq!(f.next_frame().unwrap().id, msg::WIFI_VERSION_REQUEST);
        let second = f.next_frame().unwrap();
        assert_eq!(second.id, msg::WIFI_START_REQUEST);
        assert_eq!(second.payload, vec![9, 9]);
        assert!(f.next_frame().is_none());
        assert_eq!(f.buffered(), 0);
    }

    #[test]
    fn head_unit_speaks_first_with_version_then_start() {
        let mut b = Bootstrap::new(params());
        assert_eq!(b.phase(), Phase::Idle);
        let opening = b.on_connect();
        let mut f = Framer::new();
        f.push(&opening);
        assert_eq!(f.next_frame().unwrap().id, msg::WIFI_VERSION_REQUEST);
        let start = f.next_frame().unwrap();
        assert_eq!(start.id, msg::WIFI_START_REQUEST);
        assert!(f.next_frame().is_none());
        assert_eq!(b.phase(), Phase::Offered);

        // the endpoint we advertise must be the one we were configured with
        let decoded_port_bytes = proto::encode_wifi_start_request("192.168.4.1", 5288);
        assert_eq!(start.payload, decoded_port_bytes);
    }

    #[test]
    fn info_request_is_answered_with_credentials() {
        let mut b = Bootstrap::new(params());
        b.on_connect();
        let action = b.on_frame(&Frame { id: msg::WIFI_INFO_REQUEST, payload: vec![] });
        match action {
            Action::Send(bytes) => {
                let mut f = Framer::new();
                f.push(&bytes);
                assert_eq!(f.next_frame().unwrap().id, msg::WIFI_INFO_RESPONSE);
            }
            other => panic!("expected credentials, got {other:?}"),
        }
        assert_eq!(b.phase(), Phase::CredentialsSent);
    }

    /// SUCCESS establishes the session once credentials have actually been sent.
    ///
    /// This test previously fed SUCCESS straight after `on_connect()` and asserted `Established` —
    /// i.e. it encoded the very ordering bug that let the head unit report success without ever
    /// handing over the SSID. Corrected 2026-09-01 to drive the real sequence.
    #[test]
    fn success_status_establishes_after_credentials() {
        let mut b = Bootstrap::new(params());
        b.on_connect();
        b.on_frame(&Frame { id: msg::WIFI_INFO_REQUEST, payload: vec![] });
        assert_eq!(b.phase(), Phase::CredentialsSent);

        let payload = vec![(3 << 3), 0]; // field 3 varint 0 == SUCCESS
        b.on_frame(&Frame { id: msg::WIFI_START_RESPONSE, payload });
        assert_eq!(b.phase(), Phase::Established);
        assert!(b.failure().is_none());
    }

    /// A failure must be sticky and must report the FIRST real error, not a later success.
    #[test]
    fn failure_is_sticky_and_keeps_the_first_error() {
        let mut b = Bootstrap::new(params());
        b.on_connect();

        let mut payload = Vec::new();
        // field 1 varint -3 (WIFI_INCORRECT_CREDENTIALS), sign-extended
        payload.push(1 << 3);
        let mut v = (-3i64) as u64;
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                payload.push(byte);
                break;
            }
            payload.push(byte | 0x80);
        }
        b.on_frame(&Frame { id: msg::WIFI_CONNECT_STATUS, payload });
        assert_eq!(b.phase(), Phase::Failed);
        assert_eq!(b.failure(), Some(proto::Status::WIFI_INCORRECT_CREDENTIALS));

        // a later SUCCESS must not paper over it
        b.on_frame(&Frame { id: msg::WIFI_START_RESPONSE, payload: vec![(3 << 3), 0] });
        assert_eq!(b.phase(), Phase::Failed);
        assert_eq!(b.failure(), Some(proto::Status::WIFI_INCORRECT_CREDENTIALS));
    }

    /// A retried WifiInfoRequest after a failure must NOT resurrect the bootstrap. The original
    /// test replayed a success directly after the failure and so missed this path entirely.
    #[test]
    fn a_retried_info_request_cannot_resurrect_a_failed_bootstrap() {
        let mut b = Bootstrap::new(params());
        b.on_connect();
        b.on_frame(&Frame { id: msg::WIFI_INFO_REQUEST, payload: vec![] });
        assert_eq!(b.phase(), Phase::CredentialsSent);

        // phone reports bad credentials
        let mut bad = Vec::new();
        bad.push(1 << 3);
        let mut v = (-3i64) as u64;
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 { bad.push(byte); break; }
            bad.push(byte | 0x80);
        }
        b.on_frame(&Frame { id: msg::WIFI_CONNECT_STATUS, payload: bad });
        assert_eq!(b.phase(), Phase::Failed);

        // phone retries: we must stay Failed and send nothing
        assert_eq!(b.on_frame(&Frame { id: msg::WIFI_INFO_REQUEST, payload: vec![] }), Action::None);
        assert_eq!(b.phase(), Phase::Failed);

        // and a later SUCCESS must still not claim victory
        b.on_frame(&Frame { id: msg::WIFI_START_RESPONSE, payload: vec![(3 << 3), 0] });
        assert_eq!(b.phase(), Phase::Failed);
        assert_eq!(b.failure(), Some(proto::Status::WIFI_INCORRECT_CREDENTIALS));
    }

    /// SUCCESS arriving BEFORE the phone asked for credentials must not establish the session —
    /// nothing in the protocol orders the phone's replies, and ending there would drop the link
    /// without ever having sent the SSID or passphrase.
    #[test]
    fn success_before_credentials_does_not_establish() {
        let mut b = Bootstrap::new(params());
        b.on_connect();
        assert_eq!(b.phase(), Phase::Offered);

        b.on_frame(&Frame { id: msg::WIFI_START_RESPONSE, payload: vec![(3 << 3), 0] });
        assert_eq!(b.phase(), Phase::Offered, "must still be waiting to send credentials");

        // the proper order does establish
        b.on_frame(&Frame { id: msg::WIFI_INFO_REQUEST, payload: vec![] });
        b.on_frame(&Frame { id: msg::WIFI_START_RESPONSE, payload: vec![(3 << 3), 0] });
        assert_eq!(b.phase(), Phase::Established);
    }

    /// Ids 8/9 are single-sourced and unconfirmed, so the reply is OFF by default; this exercises
    /// the opt-in path that a bench run would use to settle them.
    #[test]
    fn ping_request_is_answered_only_when_enabled() {
        let mut b = Bootstrap::new(params());
        b.on_connect();
        assert_eq!(
            b.on_frame(&Frame { id: msg::WIFI_PING_REQUEST, payload: vec![] }),
            Action::None,
            "unverified ping reply must be off by default"
        );
        std::env::set_var("AAW_ANSWER_PING", "1");
        match b.on_frame(&Frame { id: msg::WIFI_PING_REQUEST, payload: vec![] }) {
            Action::Send(bytes) => {
                let mut f = Framer::new();
                f.push(&bytes);
                assert_eq!(f.next_frame().unwrap().id, msg::WIFI_PING_RESPONSE);
            }
            other => {
                std::env::remove_var("AAW_ANSWER_PING");
                panic!("expected a ping response, got {other:?}");
            }
        }
        std::env::remove_var("AAW_ANSWER_PING");
    }

    /// An id we have never seen on a wire must not kill the bootstrap.
    #[test]
    fn unknown_message_id_is_ignored() {
        let mut b = Bootstrap::new(params());
        b.on_connect();
        assert_eq!(b.on_frame(&Frame { id: 4242, payload: vec![1, 2] }), Action::None);
        assert_eq!(b.on_frame(&Frame { id: msg::WIFI_SETUP_INFO, payload: vec![] }), Action::None);
        assert_eq!(b.phase(), Phase::Offered);
    }

    /// A peer that streams bytes which never complete a frame must not grow the buffer forever.
    #[test]
    fn framer_poisons_rather_than_growing_without_bound() {
        let mut f = Framer::new();
        // A header claiming a full 65535-byte payload, then a flood that never completes it.
        f.push(&[0xff, 0xff, 0x00, 0x01]);
        let chunk = vec![0u8; 8192];
        for _ in 0..40 {
            f.push(&chunk);
        }
        assert!(f.is_poisoned(), "buffer bound must be enforced");
        assert!(f.next_frame().is_none(), "a poisoned framer yields nothing");
        assert_eq!(f.buffered(), 0, "poisoning must release the buffer");
    }

    /// The bound must not fire on legitimate traffic — a maximum-size frame still parses.
    #[test]
    fn framer_accepts_a_maximum_size_frame() {
        let payload = vec![0x5a; u16::MAX as usize];
        let whole = encode_frame(msg::WIFI_INFO_RESPONSE, &payload);
        assert_eq!(whole.len(), MAX_FRAME);
        let mut f = Framer::new();
        f.push(&whole);
        assert!(!f.is_poisoned());
        let got = f.next_frame().expect("max-size frame must parse");
        assert_eq!(got.payload.len(), u16::MAX as usize);
    }

    #[test]
    fn message_names_cover_the_known_id_space() {
        for id in [1u16, 2, 3, 4, 5, 6, 7, 8, 9, 11] {
            assert_ne!(msg::name(id), "UNKNOWN", "id {id} should be named");
        }
        assert_eq!(msg::name(10), "UNKNOWN");
    }
}
