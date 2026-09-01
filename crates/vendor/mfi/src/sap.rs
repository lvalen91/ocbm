//! MFi-SAP — the accessory (server) side of the MFi Secure Association Protocol used by the
//! receiver's `/auth-setup`. Station-to-Station key exchange: X25519 ECDH for the shared secret,
//! SHA-1-derived AES-128-CTR key/IV, and an RSA signature over `SHA1(ourPK ‖ peerPK)` produced by
//! the MFi chip (via [`MfiSigner`]). Clean-room from the protocol behaviour (cf. `MFiSAP.c`) on
//! RustCrypto + x25519-dalek — no Apple source transliterated.
//!
//! Exchange (server): in `M1 = <1:version><32:peer X25519 pub>`, out
//! `M2 = <32:our X25519 pub><4:BE certLen><cert><4:BE sigLen><AES-CTR(sig)>`.

use aes::cipher::{KeyIvInit, StreamCipher};
use sha1::{Digest, Sha1};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::auth_client::MfiSigner;

type Aes128Ctr = ctr::Ctr128BE<aes::Aes128>;

/// The only supported MFi-SAP version.
pub const VERSION1: u8 = 1;
const ECDH_LEN: usize = 32;

/// Consecutive malformed-M1 count, for the anti-brute-force handshake throttle (#74).
static SAP_FAILS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Anti-brute-force throttle: after a few consecutive malformed/wrong-version M1s (the signature of a
/// host hammering `/auth-setup` — a valid iPhone sends one well-formed M1 per session), delay the
/// rejection by a progressively longer sleep (capped at 2s). Reset by [`sap_pass`] on any good
/// exchange, so a legitimate client is never penalised. Mirrors the MFi-SAP accessory's own retry
/// throttle. Called on the error paths of [`MfiSapServer::exchange_with_secret`].
fn sap_fail_backoff() {
    use std::sync::atomic::Ordering;
    let fails = SAP_FAILS.fetch_add(1, Ordering::Relaxed) + 1;
    if fails > 3 {
        let ms = ((fails - 3) as u64 * 200).min(2000);
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

/// Clear the failure counter after a successful handshake (#74).
fn sap_pass() {
    SAP_FAILS.store(0, std::sync::atomic::Ordering::Relaxed);
}

/// MFi-SAP server errors.
#[derive(Debug)]
pub enum SapError {
    /// M1 was malformed (wrong length).
    BadInput,
    /// Unsupported MFi-SAP version.
    Version(u8),
    /// The signer (auth service / MFi chip) failed.
    Signer(std::io::Error),
    /// `crypt` was called before a completed exchange.
    NotReady,
    /// The local system RNG failed — an accessory-side fault, not bad peer input.
    Rng,
}

impl core::fmt::Display for SapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SapError::BadInput => write!(f, "malformed MFi-SAP M1"),
            SapError::Version(v) => write!(f, "unsupported MFi-SAP version {v}"),
            SapError::Signer(e) => write!(f, "MFi signer failed: {e}"),
            SapError::NotReady => write!(f, "MFi-SAP exchange not completed"),
            SapError::Rng => write!(f, "system RNG failure"),
        }
    }
}

impl std::error::Error for SapError {}

/// MFi-SAP accessory/server. One per `/auth-setup` exchange; after [`exchange`](Self::exchange)
/// the same instance can [`crypt`](Self::crypt) the post-auth stream (the CTR counter continues).
pub struct MfiSapServer {
    aes: Option<Aes128Ctr>,
}

impl Default for MfiSapServer {
    fn default() -> Self {
        Self::new()
    }
}

impl MfiSapServer {
    pub fn new() -> Self {
        Self { aes: None }
    }

    /// Process client M1 and produce server M2, generating a fresh random ECDH key.
    pub fn exchange<S: MfiSigner>(&mut self, m1: &[u8], signer: &mut S) -> Result<Vec<u8>, SapError> {
        let mut sk = [0u8; 32];
        getrandom::getrandom(&mut sk).map_err(|_| SapError::Rng)?;
        self.exchange_with_secret(m1, signer, sk)
    }

    /// Like [`exchange`](Self::exchange) but with a caller-supplied ECDH secret — for deterministic
    /// tests / capture replay. Production code uses [`exchange`](Self::exchange).
    pub fn exchange_with_secret<S: MfiSigner>(
        &mut self,
        m1: &[u8],
        signer: &mut S,
        sk: [u8; 32],
    ) -> Result<Vec<u8>, SapError> {
        if m1.len() != 1 + ECDH_LEN {
            sap_fail_backoff();
            return Err(SapError::BadInput);
        }
        if m1[0] != VERSION1 {
            sap_fail_backoff();
            return Err(SapError::Version(m1[0]));
        }
        let mut peer = [0u8; ECDH_LEN];
        peer.copy_from_slice(&m1[1..]);

        let secret = StaticSecret::from(sk);
        let our_pk = PublicKey::from(&secret);
        let shared = secret.diffie_hellman(&PublicKey::from(peer));
        let shared = shared.as_bytes();

        // Salted SHA-1 of the shared secret → AES key (first 16 of the 20-byte hash) and IV.
        let aes_key = sha1_of(&[b"AES-KEY", &shared[..]]);
        let aes_iv = sha1_of(&[b"AES-IV", &shared[..]]);
        // The MFi chip signs SHA1(ourPK ‖ peerPK); the cert lets the phone verify it.
        let digest = sha1_of(&[our_pk.as_bytes(), &peer]);

        let mut sig = signer.create_signature(&digest).map_err(SapError::Signer)?;
        let cert = signer.copy_certificate().map_err(SapError::Signer)?;

        let mut cipher =
            Aes128Ctr::new_from_slices(&aes_key[..16], &aes_iv[..16]).expect("16-byte key/iv");
        cipher.apply_keystream(&mut sig);
        self.aes = Some(cipher);

        let mut m2 = Vec::with_capacity(ECDH_LEN + 4 + cert.len() + 4 + sig.len());
        m2.extend_from_slice(our_pk.as_bytes());
        m2.extend_from_slice(&(cert.len() as u32).to_be_bytes());
        m2.extend_from_slice(&cert);
        m2.extend_from_slice(&(sig.len() as u32).to_be_bytes());
        m2.extend_from_slice(&sig);
        sap_pass(); // good handshake — clear the brute-force backoff (#74)
        Ok(m2)
    }

    /// Encrypt/decrypt post-auth data in place. AES-CTR is symmetric, and the counter continues
    /// from where M2 left off (matching `MFiSAP_Encrypt`/`MFiSAP_Decrypt` sharing one context).
    pub fn crypt(&mut self, data: &mut [u8]) -> Result<(), SapError> {
        self.aes.as_mut().ok_or(SapError::NotReady)?.apply_keystream(data);
        Ok(())
    }
}

fn sha1_of(parts: &[&[u8]]) -> [u8; 20] {
    let mut h = Sha1::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}
