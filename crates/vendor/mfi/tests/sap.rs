//! MFi-SAP server tests. Drives the M1→M2 exchange with a mock signer (the real 945-byte cert +
//! 128-byte signature fixtures from capture `ours-20260617`) and an independent re-derivation of
//! the AES key/IV to prove the encrypted-signature round-trip — no live auth service needed.

use aes::cipher::{KeyIvInit, StreamCipher};
use mfi::auth_client::MfiSigner;
use mfi::sap::{MfiSapServer, VERSION1};
use sha1::{Digest, Sha1};
use std::io;
use x25519_dalek::{PublicKey, StaticSecret};

type Aes128Ctr = ctr::Ctr128BE<aes::Aes128>;

const CERT: &[u8] = include_bytes!("fixtures/mfi_cert.der"); // 945 B
const SIG: &[u8] = include_bytes!("fixtures/mfi_sign_testvector.sig"); // 128 B

struct MockSigner;
impl MfiSigner for MockSigner {
    fn copy_certificate(&mut self) -> io::Result<Vec<u8>> {
        Ok(CERT.to_vec())
    }
    fn create_signature(&mut self, digest: &[u8]) -> io::Result<Vec<u8>> {
        assert_eq!(digest.len(), 20, "MFi-SAP must sign a 20-byte SHA-1 digest");
        Ok(SIG.to_vec())
    }
}

#[test]
fn m2_structure_and_signature_round_trip() {
    let sk = [7u8; 32];
    let peer = [2u8; 32];
    let mut m1 = vec![VERSION1];
    m1.extend_from_slice(&peer);

    let mut srv = MfiSapServer::new();
    let m2 = srv.exchange_with_secret(&m1, &mut MockSigner, sk).unwrap();

    // Layout: <32 ourPK><4 BE certLen><cert><4 BE sigLen><AES-CTR(sig)>
    let our_pk = &m2[0..32];
    let cert_len = u32::from_be_bytes(m2[32..36].try_into().unwrap()) as usize;
    assert_eq!(cert_len, 945);
    assert_eq!(&m2[36..36 + cert_len], CERT);
    let so = 36 + cert_len;
    let sig_len = u32::from_be_bytes(m2[so..so + 4].try_into().unwrap()) as usize;
    assert_eq!(sig_len, 128);
    let enc_sig = &m2[so + 4..so + 4 + sig_len];
    assert_eq!(m2.len(), so + 4 + sig_len);
    assert_ne!(enc_sig, SIG, "signature must be AES-CTR encrypted in M2");

    // ourPK must be X25519(sk)·base
    let expect_pk = PublicKey::from(&StaticSecret::from(sk));
    assert_eq!(our_pk, expect_pk.as_bytes());

    // Independently re-derive key/IV from the shared secret and decrypt → original fixture sig.
    let shared = StaticSecret::from(sk).diffie_hellman(&PublicKey::from(peer));
    let key = sha1(&[b"AES-KEY", shared.as_bytes()]);
    let iv = sha1(&[b"AES-IV", shared.as_bytes()]);
    let mut dec = enc_sig.to_vec();
    Aes128Ctr::new_from_slices(&key[..16], &iv[..16])
        .unwrap()
        .apply_keystream(&mut dec);
    assert_eq!(dec, SIG, "decrypted M2 signature must match the fixture");
}

#[test]
fn rejects_bad_version_and_length() {
    let mut srv = MfiSapServer::new();
    // wrong version
    assert!(srv
        .exchange_with_secret(&[2u8; 33], &mut MockSigner, [0u8; 32])
        .is_err());
    // too short
    assert!(srv
        .exchange_with_secret(&[VERSION1, 0, 0, 0], &mut MockSigner, [0u8; 32])
        .is_err());
}

fn sha1(parts: &[&[u8]]) -> [u8; 20] {
    let mut h = Sha1::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}
