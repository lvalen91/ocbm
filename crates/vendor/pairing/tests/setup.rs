//! pair-setup internal-consistency round-trip: a test controller (the SRP reference client +
//! Ed25519) completes M1→M6 against the server, proving the HomeKit-correct SRP-6a exchange, the
//! HKDF/nonce/signing conventions, the LTPK exchange, and that the pairing is recorded. (Wire-compat
//! with the iPhone's SRP client is validated at the live cutover.)

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use pairing::crypto::{chacha_decrypt, chacha_encrypt, hkdf_sha512, method, tlv_type};
use pairing::setup::{PairSetupServer, PeerSaver};
use pairing::srp::client_proof;
use pairing::tlv;
use pairing::verify::Identity;

const USERNAME: &[u8] = b"Pair-Setup";

#[derive(Default)]
struct Saved {
    id: Option<Vec<u8>>,
    pk: Option<[u8; 32]>,
}
impl PeerSaver for Saved {
    fn save_peer(&mut self, id: &[u8], ltpk: [u8; 32]) {
        self.id = Some(id.to_vec());
        self.pk = Some(ltpk);
    }
}

fn first(items: &[(u8, Vec<u8>)], ty: u8) -> Option<Vec<u8>> {
    items.iter().find(|(t, _)| *t == ty).map(|(_, v)| v.clone())
}

#[test]
fn pair_setup_round_trip_records_pairing() {
    let acc = Identity::new(b"AA:BB:CC:DD:EE:FF".to_vec(), [9u8; 32]);
    let code = b"3939".to_vec();
    let mut server = PairSetupServer::new(&acc, code.clone());
    let mut saved = Saved::default();

    // M1 → M2
    let m1 = tlv::encode(&[(tlv_type::STATE, &[1]), (tlv_type::METHOD, &[method::PAIR_SETUP])]);
    let (m2, _) = server.exchange(&m1, &mut saved).unwrap();
    let m2i = tlv::decode(&m2).unwrap();
    let salt = first(&m2i, tlv_type::SALT).unwrap();
    let b_pub = first(&m2i, tlv_type::PUBLIC_KEY).unwrap();

    // controller SRP (reference client)
    let (a_pub, client_m1, k) = client_proof(USERNAME, &code, &salt, &[4u8; 32], &b_pub);

    // M3 → M4
    let m3 = tlv::encode(&[
        (tlv_type::STATE, &[3]),
        (tlv_type::PUBLIC_KEY, &a_pub),
        (tlv_type::PROOF, &client_m1),
    ]);
    let (m4, _) = server.exchange(&m3, &mut saved).unwrap();
    let m4i = tlv::decode(&m4).unwrap();
    assert!(first(&m4i, tlv_type::ERROR).is_none(), "SRP setup-code verified");
    assert!(first(&m4i, tlv_type::PROOF).is_some(), "server returns M2 proof");

    // M5: controller LTPK exchange (uses the agreed SRP key K)
    let ctrl_ed = SigningKey::from_bytes(&[3u8; 32]);
    let ctrl_id = b"controller-1".to_vec();
    let ctrl_ltpk = ctrl_ed.verifying_key().to_bytes();
    let key: [u8; 32] = hkdf_sha512(&k, b"Pair-Setup-Encrypt-Salt", b"Pair-Setup-Encrypt-Info");
    let dk: [u8; 32] = hkdf_sha512(
        &k,
        b"Pair-Setup-Controller-Sign-Salt",
        b"Pair-Setup-Controller-Sign-Info",
    );
    let mut signed = Vec::new();
    signed.extend_from_slice(&dk);
    signed.extend_from_slice(&ctrl_id);
    signed.extend_from_slice(&ctrl_ltpk);
    let csig = ctrl_ed.sign(&signed).to_bytes();
    let sub = tlv::encode(&[
        (tlv_type::IDENTIFIER, &ctrl_id),
        (tlv_type::PUBLIC_KEY, &ctrl_ltpk),
        (tlv_type::SIGNATURE, &csig),
    ]);
    let m5 = tlv::encode(&[
        (tlv_type::STATE, &[5]),
        (tlv_type::ENCRYPTED_DATA, &chacha_encrypt(&key, b"PS-Msg05", &sub)),
    ]);
    let (m6, done) = server.exchange(&m5, &mut saved).unwrap();
    assert!(done);

    // controller verifies M6 (accessory LTPK + signature)
    let m6i = tlv::decode(&m6).unwrap();
    assert!(first(&m6i, tlv_type::ERROR).is_none());
    let pt = chacha_decrypt(&key, b"PS-Msg06", &first(&m6i, tlv_type::ENCRYPTED_DATA).unwrap())
        .expect("M6 decrypts");
    let pti = tlv::decode(&pt).unwrap();
    let acc_id = first(&pti, tlv_type::IDENTIFIER).unwrap();
    let acc_ltpk = first(&pti, tlv_type::PUBLIC_KEY).unwrap();
    let acc_sig = first(&pti, tlv_type::SIGNATURE).unwrap();
    let ak: [u8; 32] = hkdf_sha512(
        &k,
        b"Pair-Setup-Accessory-Sign-Salt",
        b"Pair-Setup-Accessory-Sign-Info",
    );
    let mut signed2 = Vec::new();
    signed2.extend_from_slice(&ak);
    signed2.extend_from_slice(&acc_id);
    signed2.extend_from_slice(&acc_ltpk);
    let avk = VerifyingKey::from_bytes(&acc.public_key()).unwrap();
    avk.verify(&signed2, &ed25519_dalek::Signature::from_slice(&acc_sig).unwrap())
        .expect("accessory signature valid");
    assert_eq!(acc_ltpk, acc.public_key().to_vec());

    // pairing recorded for the next pair-verify
    assert_eq!(saved.id.unwrap(), ctrl_id);
    assert_eq!(saved.pk.unwrap(), ctrl_ltpk);
}

/// Drive M1→M4 with the correct code; returns the agreed SRP key `K`.
fn drive_to_m4(server: &mut PairSetupServer, saved: &mut Saved, code: &[u8]) -> Vec<u8> {
    let m1 = tlv::encode(&[(tlv_type::STATE, &[1]), (tlv_type::METHOD, &[method::PAIR_SETUP])]);
    let (m2, _) = server.exchange(&m1, saved).unwrap();
    let m2i = tlv::decode(&m2).unwrap();
    let salt = first(&m2i, tlv_type::SALT).unwrap();
    let b_pub = first(&m2i, tlv_type::PUBLIC_KEY).unwrap();
    let (a_pub, client_m1, k) = client_proof(USERNAME, code, &salt, &[4u8; 32], &b_pub);
    let m3 = tlv::encode(&[
        (tlv_type::STATE, &[3]),
        (tlv_type::PUBLIC_KEY, &a_pub),
        (tlv_type::PROOF, &client_m1),
    ]);
    let (m4, _) = server.exchange(&m3, saved).unwrap();
    let m4i = tlv::decode(&m4).unwrap();
    assert!(first(&m4i, tlv_type::ERROR).is_none(), "SRP setup-code verified");
    k
}

/// Build an M5 whose sub-TLV carries `sig` for controller `ctrl_ed` (pass a wrong signature to
/// exercise the auth-failure path).
fn build_m5(k: &[u8], ctrl_id: &[u8], ctrl_ltpk: &[u8; 32], sig: &[u8; 64]) -> Vec<u8> {
    let key: [u8; 32] = hkdf_sha512(k, b"Pair-Setup-Encrypt-Salt", b"Pair-Setup-Encrypt-Info");
    let sub = tlv::encode(&[
        (tlv_type::IDENTIFIER, ctrl_id),
        (tlv_type::PUBLIC_KEY, ctrl_ltpk),
        (tlv_type::SIGNATURE, sig),
    ]);
    tlv::encode(&[
        (tlv_type::STATE, &[5]),
        (tlv_type::ENCRYPTED_DATA, &chacha_encrypt(&key, b"PS-Msg05", &sub)),
    ])
}

fn valid_m5(k: &[u8]) -> Vec<u8> {
    let ctrl_ed = SigningKey::from_bytes(&[3u8; 32]);
    let ctrl_id = b"controller-1".to_vec();
    let ctrl_ltpk = ctrl_ed.verifying_key().to_bytes();
    let dk: [u8; 32] = hkdf_sha512(
        k,
        b"Pair-Setup-Controller-Sign-Salt",
        b"Pair-Setup-Controller-Sign-Info",
    );
    let mut signed = Vec::new();
    signed.extend_from_slice(&dk);
    signed.extend_from_slice(&ctrl_id);
    signed.extend_from_slice(&ctrl_ltpk);
    let csig = ctrl_ed.sign(&signed).to_bytes();
    build_m5(k, &ctrl_id, &ctrl_ltpk, &csig)
}

#[test]
fn m5_replay_after_success_is_rejected() {
    let acc = Identity::new(b"acc".to_vec(), [9u8; 32]);
    let mut server = PairSetupServer::new(&acc, b"3939".to_vec());
    let mut saved = Saved::default();
    let k = drive_to_m4(&mut server, &mut saved, b"3939");
    let m5 = valid_m5(&k);
    let (_m6, done) = server.exchange(&m5, &mut saved).unwrap();
    assert!(done);
    // Replaying the identical M5 must NOT re-drive M6 — the session is terminal after success.
    assert_eq!(
        server.exchange(&m5, &mut saved),
        Err(pairing::setup::SetupError::BadState)
    );
}

#[test]
fn m5_retry_after_auth_failure_is_rejected() {
    let acc = Identity::new(b"acc".to_vec(), [9u8; 32]);
    let mut server = PairSetupServer::new(&acc, b"3939".to_vec());
    let mut saved = Saved::default();
    let k = drive_to_m4(&mut server, &mut saved, b"3939");
    // Well-formed M5 (decrypts fine) with a WRONG controller signature → Authentication error.
    let ctrl_ed = SigningKey::from_bytes(&[3u8; 32]);
    let ctrl_ltpk = ctrl_ed.verifying_key().to_bytes();
    let bad_m5 = build_m5(&k, b"controller-1", &ctrl_ltpk, &[0u8; 64]);
    let (reply, done) = server.exchange(&bad_m5, &mut saved).unwrap();
    assert!(done);
    let ri = tlv::decode(&reply).unwrap();
    assert_eq!(first(&ri, tlv_type::ERROR).unwrap(), vec![0x02]); // Authentication
    assert!(saved.id.is_none(), "failed M5 must not record a pairing");
    // The session is dead — a second M5 (even a now-valid one) must be refused.
    assert_eq!(
        server.exchange(&valid_m5(&k), &mut saved),
        Err(pairing::setup::SetupError::BadState)
    );
}

#[test]
fn pair_setup_wrong_code_fails_srp() {
    let acc = Identity::new(b"acc".to_vec(), [1u8; 32]);
    let mut server = PairSetupServer::new(&acc, b"3939".to_vec());
    let mut saved = Saved::default();

    let m1 = tlv::encode(&[(tlv_type::STATE, &[1]), (tlv_type::METHOD, &[method::PAIR_SETUP])]);
    let (m2, _) = server.exchange(&m1, &mut saved).unwrap();
    let m2i = tlv::decode(&m2).unwrap();
    let salt = first(&m2i, tlv_type::SALT).unwrap();
    let b_pub = first(&m2i, tlv_type::PUBLIC_KEY).unwrap();

    // controller uses the WRONG code → its proof won't match
    let (a_pub, client_m1, _k) = client_proof(USERNAME, b"0000", &salt, &[4u8; 32], &b_pub);
    let m3 = tlv::encode(&[
        (tlv_type::STATE, &[3]),
        (tlv_type::PUBLIC_KEY, &a_pub),
        (tlv_type::PROOF, &client_m1),
    ]);
    let (m4, done) = server.exchange(&m3, &mut saved).unwrap();
    assert!(done);
    let m4i = tlv::decode(&m4).unwrap();
    assert_eq!(first(&m4i, tlv_type::ERROR).unwrap(), vec![0x02]); // Authentication
    assert!(saved.id.is_none());
}
