//! Regression test for the SETUP `streamConnectionID == 0` guard (session.rs `setup_phase2`).
//!
//! WHAT BROKE. The guard added 2026-07-31 (`5ce9d1c`) skipped EVERY requested stream whose
//! `streamConnectionID` was 0 or absent, citing `AirPlayReceiverSession.c:4342-4343`. Stream type
//! **130** — the RemoteControlSession DataStream that carries the wireless iAP2 / metadata channel
//! (docs/carplay/05_METADATA_AND_CONTROLS.md) — legitimately arrives with **no `streamConnectionID` key at all**: the proven-good
//! 2026-07-25 capture records the request keys verbatim as
//! `["controlType","channelID","seed","clientUUID","type","wantsDedicatedSocket","sendMessageAsIs",
//! "clientTypeUUID"]` (`docs/ops/captures/2026-07-25_SUCCESS_airplayd_wl_handshake.txt:25`). So
//! `unwrap_or(0)` yielded 0, the stream was skipped before reaching the `130` arm, the response
//! carried no entry and therefore no `streamID` transport token, and iOS's entire outbound iAP2 path
//! never existed — the tunnel sat at `Init` with zero NowPlaying for ~10 days while A/V looked
//! perfect. The phone re-asked on cseq 35/36/37/38 and we refused every time.
//!
//! WHY IT LIVES IN ITS OWN INTEGRATION BINARY. The type-130 arm is gated on
//! `CARPLAY_WIRELESS_METADATA`, and arming an env var inside the unit-test binary would leak into
//! every sibling test in that process. A Cargo integration test gets its own process — the same
//! reasoning as `crates/vendor/iap2-core/tests/pushed_policy.rs`.

use std::io::Cursor;

use plist::{Dictionary, Value};
use receiver::session::{AvSession, SessionDelegate};

/// The iAP RCS client type (docs/carplay/05_METADATA_AND_CONTROLS.md §1.2) — the one `clientTypeUUID` allowed to drive the iAP2 link.
const IAP_CLIENT_TYPE: &str = "E9459FD0-BCAD-4C45-820F-1E72447EF2F2";

fn bin(d: Dictionary) -> Vec<u8> {
    let mut buf = Vec::new();
    Value::Dictionary(d).to_writer_binary(&mut buf).expect("plist");
    buf
}

fn streams_of(resp: &[u8]) -> Vec<Dictionary> {
    Value::from_reader(Cursor::new(resp))
        .expect("response is a binary plist")
        .into_dictionary()
        .expect("response is a dict")
        .get("streams")
        .and_then(|v| v.as_array())
        .expect("response carries a streams array")
        .iter()
        .map(|v| v.as_dictionary().expect("stream entry is a dict").clone())
        .collect()
}

fn int_key(d: &Dictionary, k: &str) -> Option<i64> {
    d.get(k).and_then(|v| {
        v.as_signed_integer()
            .or_else(|| v.as_unsigned_integer().map(|u| u as i64))
    })
}

/// Byte-shaped after the 2026-07-25 capture: NO `streamConnectionID` key anywhere in the request.
fn rcs_setup_request(wants_dedicated_socket: bool) -> Vec<u8> {
    let mut s = Dictionary::new();
    s.insert("type".into(), Value::Integer(130i64.into()));
    s.insert("channelID".into(), Value::String("5E:F7:F7:A9:CB:CD-RCS-1".into()));
    s.insert("clientTypeUUID".into(), Value::String(IAP_CLIENT_TYPE.into()));
    s.insert("clientUUID".into(), Value::String("E0CB6FB0-0000-0000-0000-0000C0FFEE01".into()));
    s.insert("controlType".into(), Value::Integer(2i64.into()));
    s.insert("seed".into(), Value::Integer(839_141_951_896_294_626i64.into()));
    s.insert("sendMessageAsIs".into(), Value::Boolean(true));
    s.insert(
        "wantsDedicatedSocket".into(),
        Value::Boolean(wants_dedicated_socket),
    );
    assert!(
        !s.contains_key("streamConnectionID"),
        "the fixture must reproduce the capture: type 130 carries NO streamConnectionID"
    );
    let mut req = Dictionary::new();
    req.insert("streams".into(), Value::Array(vec![Value::Dictionary(s)]));
    bin(req)
}

/// One test, one process: the env arm below is process-global, and the assertions share it.
#[test]
fn type_130_setup_without_a_stream_connection_id_is_answered_with_a_transport_token() {
    // SAFETY: single-threaded test-process setup, before any `AvSession` is built. The 130 arm reads
    // this var directly (`session.rs` `130 if std::env::var_os("CARPLAY_WIRELESS_METADATA")…`), which
    // is the same gate `wireless/src/av.rs:348` sets on the real wireless spawn.
    unsafe {
        std::env::set_var("CARPLAY_WIRELESS_METADATA", "1");
    }

    // ---- (a) the shared-connection shape: no sockets bound, no threads spawned. -------------------
    let mut s = AvSession::new();
    let resp = s.setup(&rcs_setup_request(false)).unwrap();
    let streams = streams_of(&resp);
    assert_eq!(
        streams.len(),
        1,
        "a type-130 SETUP with no streamConnectionID must NOT be skipped — an empty streams array is \
         exactly the ~10-day wireless-metadata outage (iOS: 'Failed to obtain transport token from \
         SETUP response: -6727 kNotFoundErr')"
    );
    let e = &streams[0];
    assert_eq!(int_key(e, "type"), Some(130), "the entry must be the DataStream");
    let stream_id = int_key(e, "streamID").expect(
        "the entry MUST carry a `streamID` — that key IS the transport token iOS reads \
         (_DataStreamSessionSetup); without it the phone's outbound iAP2 path never exists",
    );
    assert_ne!(
        stream_id, 0,
        "0 is indistinguishable from absent on iOS's side — the token must be non-zero"
    );
    // The scid is echoed VERBATIM (0), never synthesized: type 130 keys its HKDF salt off `seed`.
    assert_eq!(int_key(e, "streamConnectionID"), Some(0));
    s.teardown(&[]); // full teardown (empty request) — stops any thread this SETUP spawned

    // ---- (b) the shape every capture on record actually uses: wantsDedicatedSocket=true. ----------
    let mut s = AvSession::new();
    let resp = s.setup(&rcs_setup_request(true)).unwrap();
    let streams = streams_of(&resp);
    assert_eq!(streams.len(), 1, "the dedicated-socket shape must be answered too");
    let e = &streams[0];
    assert!(int_key(e, "streamID").unwrap_or(0) != 0, "transport token required");
    assert!(
        int_key(e, "dataPort").unwrap_or(0) > 0,
        "wantsDedicatedSocket=true must advertise the bound dataPort"
    );
    s.teardown(&[]); // stops the accept thread the dedicated-socket arm spawned

    // ---- (c) the guard still holds for the A/V streams it was written for. ------------------------
    // `AirPlayReceiverSession.c:4342-4343` really does reject a zero scid on the SCREEN path, where
    // the scid IS the HKDF salt — a zero there derives the wrong key and every frame decrypts to
    // garbage with no error anywhere. Admitting 130 must not weaken that.
    let mut screen = Dictionary::new();
    screen.insert("type".into(), Value::Integer(110i64.into()));
    screen.insert("streamConnectionID".into(), Value::Integer(0i64.into()));
    let mut req = Dictionary::new();
    req.insert("streams".into(), Value::Array(vec![Value::Dictionary(screen)]));
    let mut s = AvSession::new();
    let resp = s.setup(&bin(req)).unwrap();
    assert!(
        streams_of(&resp).is_empty(),
        "a zero scid on an A/V stream must still be skipped (kVersionErr in Apple's receiver)"
    );
    s.teardown(&[]); // full teardown (empty request) — stops any thread this SETUP spawned
}
