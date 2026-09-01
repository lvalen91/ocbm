//! pair-verify round-trip: a test controller drives the full M1→M2→M3→M4 against the server,
//! proving mutual Ed25519 auth, ECDH agreement (both sides derive the same shared secret), and the
//! exact HKDF/nonce/signing-order conventions. No live iPhone needed.

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use pairing::crypto::{chacha_decrypt, chacha_encrypt, hkdf_sha512, tlv_type};
use pairing::tlv;
use pairing::verify::{Identity, PairVerifyServer, PeerStore};
use x25519_dalek::{PublicKey, StaticSecret};

struct OnePeer {
    id: Vec<u8>,
    pk: [u8; 32],
}
impl PeerStore for OnePeer {
    fn find_peer(&self, id: &[u8]) -> Option<[u8; 32]> {
        (id == self.id).then_some(self.pk)
    }
}

fn first(items: &[(u8, Vec<u8>)], ty: u8) -> Option<Vec<u8>> {
    items.iter().find(|(t, _)| *t == ty).map(|(_, v)| v.clone())
}

#[test]
fn pair_verify_round_trip_succeeds() {
    let acc = Identity::new(b"AA:BB:CC:DD:EE:FF".to_vec(), [9u8; 32]);
    let ctrl_ed = SigningKey::from_bytes(&[3u8; 32]);
    let ctrl_id = b"controller-1".to_vec();
    let peers = OnePeer { id: ctrl_id.clone(), pk: ctrl_ed.verifying_key().to_bytes() };

    let mut server = PairVerifyServer::new(&acc);

    // M1: controller ephemeral curve key
    let ctrl_sk = StaticSecret::from([5u8; 32]);
    let ctrl_pk = PublicKey::from(&ctrl_sk).to_bytes();
    let m1 = tlv::encode(&[(tlv_type::STATE, &[1]), (tlv_type::PUBLIC_KEY, &ctrl_pk)]);

    let (m2, done) = server.exchange(&m1, &peers).unwrap();
    assert!(!done);

    // controller processes M2
    let m2i = tlv::decode(&m2).unwrap();
    let acc_pk: [u8; 32] = first(&m2i, tlv_type::PUBLIC_KEY).unwrap().try_into().unwrap();
    let shared = ctrl_sk.diffie_hellman(&PublicKey::from(acc_pk)).to_bytes();
    let key: [u8; 32] =
        hkdf_sha512(&shared, b"Pair-Verify-Encrypt-Salt", b"Pair-Verify-Encrypt-Info");
    let sub = chacha_decrypt(&key, b"PV-Msg02", &first(&m2i, tlv_type::ENCRYPTED_DATA).unwrap())
        .expect("M2 decrypts");
    let subi = tlv::decode(&sub).unwrap();
    let acc_id = first(&subi, tlv_type::IDENTIFIER).unwrap();
    let acc_sig = first(&subi, tlv_type::SIGNATURE).unwrap();

    // controller verifies accessory signed acc_pk ‖ acc_id ‖ ctrl_pk
    let mut signed = Vec::new();
    signed.extend_from_slice(&acc_pk);
    signed.extend_from_slice(&acc_id);
    signed.extend_from_slice(&ctrl_pk);
    let acc_vk = VerifyingKey::from_bytes(&acc.public_key()).unwrap();
    acc_vk
        .verify(&signed, &ed25519_dalek::Signature::from_slice(&acc_sig).unwrap())
        .expect("accessory signature valid");

    // M3: controller signs ctrl_pk ‖ ctrl_id ‖ acc_pk
    let mut to_sign = Vec::new();
    to_sign.extend_from_slice(&ctrl_pk);
    to_sign.extend_from_slice(&ctrl_id);
    to_sign.extend_from_slice(&acc_pk);
    let ctrl_sig = ctrl_ed.sign(&to_sign).to_bytes();
    let m3sub =
        tlv::encode(&[(tlv_type::IDENTIFIER, &ctrl_id), (tlv_type::SIGNATURE, &ctrl_sig)]);
    let m3 = tlv::encode(&[
        (tlv_type::STATE, &[3]),
        (tlv_type::ENCRYPTED_DATA, &chacha_encrypt(&key, b"PV-Msg03", &m3sub)),
    ]);

    let (m4, done) = server.exchange(&m3, &peers).unwrap();
    assert!(done);
    let m4i = tlv::decode(&m4).unwrap();
    assert_eq!(first(&m4i, tlv_type::STATE).unwrap(), vec![4]);
    assert!(first(&m4i, tlv_type::ERROR).is_none(), "verified → no error");
    assert_eq!(
        server.shared_secret().unwrap(),
        shared,
        "server and controller agree on the ECDH shared secret"
    );
}

#[test]
fn pair_verify_rejects_unknown_controller() {
    let acc = Identity::new(b"acc".to_vec(), [1u8; 32]);
    let empty = OnePeer { id: b"nobody".to_vec(), pk: [0u8; 32] };
    let mut server = PairVerifyServer::new(&acc);

    let ctrl_sk = StaticSecret::from([5u8; 32]);
    let ctrl_pk = PublicKey::from(&ctrl_sk).to_bytes();
    let m1 = tlv::encode(&[(tlv_type::STATE, &[1]), (tlv_type::PUBLIC_KEY, &ctrl_pk)]);
    let (m2, _) = server.exchange(&m1, &empty).unwrap();

    let m2i = tlv::decode(&m2).unwrap();
    let acc_pk: [u8; 32] = first(&m2i, tlv_type::PUBLIC_KEY).unwrap().try_into().unwrap();
    let shared = ctrl_sk.diffie_hellman(&PublicKey::from(acc_pk)).to_bytes();
    let key: [u8; 32] =
        hkdf_sha512(&shared, b"Pair-Verify-Encrypt-Salt", b"Pair-Verify-Encrypt-Info");

    // an unpaired controller (its id isn't in the store)
    let ctrl_ed = SigningKey::from_bytes(&[7u8; 32]);
    let ctrl_id = b"stranger".to_vec();
    let mut to_sign = Vec::new();
    to_sign.extend_from_slice(&ctrl_pk);
    to_sign.extend_from_slice(&ctrl_id);
    to_sign.extend_from_slice(&acc_pk);
    let sig = ctrl_ed.sign(&to_sign).to_bytes();
    let m3sub = tlv::encode(&[(tlv_type::IDENTIFIER, &ctrl_id), (tlv_type::SIGNATURE, &sig)]);
    let m3 = tlv::encode(&[
        (tlv_type::STATE, &[3]),
        (tlv_type::ENCRYPTED_DATA, &chacha_encrypt(&key, b"PV-Msg03", &m3sub)),
    ]);

    let (m4, done) = server.exchange(&m3, &empty).unwrap();
    assert!(done);
    let m4i = tlv::decode(&m4).unwrap();
    assert_eq!(first(&m4i, tlv_type::ERROR).unwrap(), vec![0x02]); // Authentication
    assert!(server.shared_secret().is_none());
}
