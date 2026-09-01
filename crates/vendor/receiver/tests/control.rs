//! ControlServer integration: a scripted controller (acting as the iPhone) runs pair-verify over
//! RTSP through the server, then sends an encrypted `/info` over the flipped control channel — all
//! offline. Exercises the full Phase 4+5 path: RTSP parse → route → pairing → encrypted transition.

use std::collections::HashMap;

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use mfi::auth_client::MfiSigner;
use pairing::crypto::{chacha_decrypt, chacha_encrypt, hkdf_sha512, tlv_type};
use pairing::setup::PeerSaver;
use pairing::tlv;
use pairing::verify::{Identity, PeerStore};
use receiver::server::ControlServer;
use rtsp::control::{derive_control_keys, ControlChannel, ControlKeys};
use x25519_dalek::{PublicKey, StaticSecret};

#[derive(Default)]
struct Store(HashMap<Vec<u8>, [u8; 32]>);
impl PeerStore for Store {
    fn find_peer(&self, id: &[u8]) -> Option<[u8; 32]> {
        self.0.get(id).copied()
    }
}
impl PeerSaver for Store {
    fn save_peer(&mut self, id: &[u8], ltpk: [u8; 32]) {
        self.0.insert(id.to_vec(), ltpk);
    }
}

struct NoSigner;
impl MfiSigner for NoSigner {
    fn copy_certificate(&mut self) -> std::io::Result<Vec<u8>> {
        Ok(Vec::new())
    }
    fn create_signature(&mut self, _d: &[u8]) -> std::io::Result<Vec<u8>> {
        Ok(Vec::new())
    }
}

fn rtsp_request(method: &str, path: &str, body: &[u8], cseq: u32) -> Vec<u8> {
    let mut s = format!("{method} {path} RTSP/1.0\r\nCSeq: {cseq}\r\nContent-Length: {}\r\n\r\n", body.len())
        .into_bytes();
    s.extend_from_slice(body);
    s
}

fn first(items: &[(u8, Vec<u8>)], ty: u8) -> Vec<u8> {
    items.iter().find(|(t, _)| *t == ty).map(|(_, v)| v.clone()).unwrap()
}

fn resp_body(bytes: &[u8]) -> Vec<u8> {
    let (req, _) = split_status(bytes);
    req
}

// minimal response parser: returns the body (after the blank line)
fn split_status(bytes: &[u8]) -> (Vec<u8>, u16) {
    let pos = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = std::str::from_utf8(&bytes[..pos]).unwrap();
    let status: u16 = head.lines().next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    (bytes[pos + 4..].to_vec(), status)
}

#[test]
fn pair_verify_over_rtsp_then_encrypted_info() {
    // accessory + a pre-existing pairing (controller already paired earlier)
    let acc = Identity::new(b"AA:BB:CC:DD:EE:FF".to_vec(), [9u8; 32]);
    let ctrl_ed = SigningKey::from_bytes(&[3u8; 32]);
    let ctrl_id = b"controller-1".to_vec();
    let mut store = Store::default();
    store.save_peer(&ctrl_id, ctrl_ed.verifying_key().to_bytes());

    let info = receiver::info::build_info(&receiver::info::DeviceConfig::default());
    let mut server = ControlServer::new(&acc, b"3939".to_vec(), store, NoSigner, info);

    // M1: controller ephemeral curve key, POST /pair-verify
    let ctrl_sk = StaticSecret::from([5u8; 32]);
    let ctrl_pk = PublicKey::from(&ctrl_sk).to_bytes();
    let m1 = tlv::encode(&[(tlv_type::STATE, &[1]), (tlv_type::PUBLIC_KEY, &ctrl_pk)]);
    let out = server.feed(&rtsp_request("POST", "/pair-verify", &m1, 1)).unwrap();
    assert!(!server.is_encrypted(), "still plaintext after M2");

    // controller processes M2
    let m2 = tlv::decode(&resp_body(&out)).unwrap();
    let acc_pk: [u8; 32] = first(&m2, tlv_type::PUBLIC_KEY).try_into().unwrap();
    let shared = ctrl_sk.diffie_hellman(&PublicKey::from(acc_pk)).to_bytes();
    let key: [u8; 32] =
        hkdf_sha512(&shared, b"Pair-Verify-Encrypt-Salt", b"Pair-Verify-Encrypt-Info");
    let sub = chacha_decrypt(&key, b"PV-Msg02", &first(&m2, tlv_type::ENCRYPTED_DATA)).unwrap();
    let subi = tlv::decode(&sub).unwrap();
    let acc_id = first(&subi, tlv_type::IDENTIFIER);
    let acc_sig = first(&subi, tlv_type::SIGNATURE);
    let mut signed = Vec::new();
    signed.extend_from_slice(&acc_pk);
    signed.extend_from_slice(&acc_id);
    signed.extend_from_slice(&ctrl_pk);
    VerifyingKey::from_bytes(&acc.public_key())
        .unwrap()
        .verify(&signed, &ed25519_dalek::Signature::from_slice(&acc_sig).unwrap())
        .expect("accessory signature valid");

    // M3: controller signs + sends, expecting M4 (still plaintext) then the channel flips
    let mut to_sign = Vec::new();
    to_sign.extend_from_slice(&ctrl_pk);
    to_sign.extend_from_slice(&ctrl_id);
    to_sign.extend_from_slice(&acc_pk);
    let csig = ctrl_ed.sign(&to_sign).to_bytes();
    let m3sub = tlv::encode(&[(tlv_type::IDENTIFIER, &ctrl_id), (tlv_type::SIGNATURE, &csig)]);
    let m3 = tlv::encode(&[
        (tlv_type::STATE, &[3]),
        (tlv_type::ENCRYPTED_DATA, &chacha_encrypt(&key, b"PV-Msg03", &m3sub)),
    ]);
    let out = server.feed(&rtsp_request("POST", "/pair-verify", &m3, 2)).unwrap();
    let m4 = tlv::decode(&resp_body(&out)).unwrap();
    assert!(m4.iter().all(|(t, _)| *t != tlv_type::ERROR), "verified → no error");
    assert!(server.is_encrypted(), "channel flips on after M4");
    assert_eq!(server.session_secret().unwrap(), shared);

    // Now send an ENCRYPTED GET /info over the control channel.
    let ckeys = derive_control_keys(&shared);
    // controller mirrors: it writes with the server's read key, reads with the server's write key.
    let mut ctrl_chan = ControlChannel::new(ControlKeys { read: ckeys.write, write: ckeys.read });
    let info_req = rtsp_request("GET", "/info", &[], 3);
    let frame = ctrl_chan.encrypt_frame(&info_req);
    let enc_resp = server.feed(&frame).unwrap();
    // decrypt the server's response frame
    let (pt, _used) = ctrl_chan.decrypt_frame(&enc_resp).unwrap().unwrap();
    let (_body, status) = split_status(&pt);
    assert_eq!(status, 200, "encrypted /info answered 200");
    assert!(pt.starts_with(b"RTSP/1.0 200"), "decrypted response is a valid RTSP status line");
}
