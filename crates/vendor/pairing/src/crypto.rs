//! Shared pairing primitives: the TLV item constants (HomeKit/AirPlay pairing protocol), and the
//! HKDF-SHA512 + ChaCha20-Poly1305 helpers used by both pair-setup and pair-verify.
//!
//! ChaCha20-Poly1305 here is the IETF AEAD with the pairing's 8-byte nonce **left-padded with four
//! zero bytes** to the 12-byte IETF nonce (the HomeKit convention; cf. `chacha20_poly1305_*_64x64`).

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha512;

/// TLV item types (HomeKit/AirPlay pairing). Values from AccessorySDK `PairingUtils.c`.
pub mod tlv_type {
    pub const METHOD: u8 = 0x00;
    pub const IDENTIFIER: u8 = 0x01;
    pub const SALT: u8 = 0x02;
    pub const PUBLIC_KEY: u8 = 0x03;
    pub const PROOF: u8 = 0x04;
    pub const ENCRYPTED_DATA: u8 = 0x05;
    pub const STATE: u8 = 0x06;
    pub const ERROR: u8 = 0x07;
    pub const RETRY_DELAY: u8 = 0x08;
    pub const CERTIFICATE: u8 = 0x09;
    pub const SIGNATURE: u8 = 0x0A;
    pub const PERMISSIONS: u8 = 0x0B;
    pub const FRAGMENT_DATA: u8 = 0x0C;
    pub const FRAGMENT_LAST: u8 = 0x0D;
    pub const FLAGS: u8 = 0x13;
    pub const SEPARATOR: u8 = 0xFF;
}

/// TLV `Method` values.
pub mod method {
    pub const PAIR_SETUP: u8 = 0;
    pub const MFI_PAIR_SETUP: u8 = 1;
    pub const VERIFY: u8 = 2;
}

/// TLV `Error` codes.
pub mod tlv_error {
    pub const UNKNOWN: u8 = 0x01;
    pub const AUTHENTICATION: u8 = 0x02;
    pub const BACKOFF: u8 = 0x03;
    pub const UNKNOWN_PEER: u8 = 0x04;
    pub const MAX_PEERS: u8 = 0x05;
    pub const MAX_TRIES: u8 = 0x06;
}

/// HKDF-SHA512: derive `OUT` bytes from `ikm` with the given salt/info strings.
pub fn hkdf_sha512<const OUT: usize>(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; OUT] {
    let hk = Hkdf::<Sha512>::new(Some(salt), ikm);
    let mut okm = [0u8; OUT];
    hk.expand(info, &mut okm).expect("HKDF output length valid");
    okm
}

/// Build the 12-byte IETF nonce from an 8-byte pairing nonce (`b"PV-Msg02"` etc.): 4 zero bytes
/// then the 8 nonce bytes.
fn nonce12(nonce8: &[u8; 8]) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(nonce8);
    n
}

/// AEAD-encrypt `plaintext` with `key` and the pairing 8-byte nonce; returns ciphertext‖tag(16).
pub fn chacha_encrypt(key: &[u8; 32], nonce8: &[u8; 8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let n = nonce12(nonce8);
    cipher
        .encrypt(Nonce::from_slice(&n), Payload { msg: plaintext, aad: &[] })
        .expect("ChaCha20-Poly1305 encryption")
}

/// AEAD-decrypt ciphertext‖tag(16) with `key` and the pairing 8-byte nonce.
/// Returns `None` if authentication fails.
pub fn chacha_decrypt(key: &[u8; 32], nonce8: &[u8; 8], ct_and_tag: &[u8]) -> Option<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let n = nonce12(nonce8);
    cipher
        .decrypt(Nonce::from_slice(&n), Payload { msg: ct_and_tag, aad: &[] })
        .ok()
}
