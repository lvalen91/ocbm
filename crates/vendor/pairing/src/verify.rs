//! pair-verify (accessory/server side) — the per-session handshake an already-paired controller
//! runs each time it connects. Curve25519 ECDH for an ephemeral shared secret, Ed25519 long-term
//! signatures for mutual authentication, HKDF-SHA512 key derivation, ChaCha20-Poly1305 for the
//! M2/M3 sub-TLVs. Clean-room from the protocol facts in `PairingUtils.c` (exact salt/info/nonce
//! strings and signing orders preserved); built on x25519-dalek / ed25519-dalek / hkdf / chacha.
//!
//! Flow: in `M1{State=1, PublicKey=ctrl ephemeral}` → out `M2{State=2, PublicKey=our ephemeral,
//! EncryptedData=enc(Identifier‖Signature)}`; in `M3{State=3, EncryptedData=enc(Identifier‖
//! Signature)}` → out `M4{State=4}` (or `{State=4, Error=Authentication}`). On success the
//! ephemeral [`shared_secret`](PairVerifyServer::shared_secret) seeds the session's stream keys.

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::crypto::{chacha_decrypt, chacha_encrypt, hkdf_sha512, tlv_error, tlv_type};
use crate::tlv;

/// The accessory's stable identity: a UTF-8 identifier + its Ed25519 long-term key.
pub struct Identity {
    pub id: Vec<u8>,
    pub signing: SigningKey,
}

impl Identity {
    pub fn new(id: impl Into<Vec<u8>>, ed_secret: [u8; 32]) -> Self {
        Self { id: id.into(), signing: SigningKey::from_bytes(&ed_secret) }
    }
    pub fn public_key(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }
}

/// Looks up the Ed25519 long-term public key of a previously-paired controller.
pub trait PeerStore {
    fn find_peer(&self, id: &[u8]) -> Option<[u8; 32]>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum PairError {
    BadState,
    MalformedTlv,
    DecryptFailed,
    UnknownPeer,
    BadSignature,
    /// The local system RNG failed — an accessory-side fault, not bad peer input.
    Rng,
}

impl core::fmt::Display for PairError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            PairError::BadState => "unexpected pair-verify state",
            PairError::MalformedTlv => "malformed pair-verify TLV",
            PairError::DecryptFailed => "pair-verify decrypt/auth failed",
            PairError::UnknownPeer => "controller not paired",
            PairError::BadSignature => "controller signature invalid",
            PairError::Rng => "system RNG failure",
        };
        f.write_str(s)
    }
}

impl std::error::Error for PairError {}

/// One pair-verify exchange. Drive with successive [`exchange`](Self::exchange) calls.
pub struct PairVerifyServer<'a> {
    identity: &'a Identity,
    state: u8,
    our_curve_pk: [u8; 32],
    peer_curve_pk: [u8; 32],
    shared: [u8; 32],
    encrypt_key: [u8; 32],
    done: bool,
}

impl<'a> PairVerifyServer<'a> {
    pub fn new(identity: &'a Identity) -> Self {
        Self {
            identity,
            state: 0,
            our_curve_pk: [0; 32],
            peer_curve_pk: [0; 32],
            shared: [0; 32],
            encrypt_key: [0; 32],
            done: false,
        }
    }

    /// The ephemeral ECDH shared secret — valid only after a successful exchange. The receiver
    /// derives the per-stream ChaCha20 keys from this.
    pub fn shared_secret(&self) -> Option<[u8; 32]> {
        self.done.then_some(self.shared)
    }

    /// Process one inbound message, returning the reply and whether the exchange is complete.
    pub fn exchange<P: PeerStore>(
        &mut self,
        input: &[u8],
        peers: &P,
    ) -> Result<(Vec<u8>, bool), PairError> {
        let items = tlv::decode(input).map_err(|_| PairError::MalformedTlv)?;
        let state = first(&items, tlv_type::STATE).ok_or(PairError::MalformedTlv)?;
        match state.first() {
            Some(1) => self.m1_to_m2(&items),
            Some(3) => self.m3_to_m4(&items, peers),
            _ => Err(PairError::BadState),
        }
    }

    fn m1_to_m2(&mut self, m1: &[(u8, Vec<u8>)]) -> Result<(Vec<u8>, bool), PairError> {
        let peer_pk = first(m1, tlv_type::PUBLIC_KEY)
            .filter(|v| v.len() == 32)
            .ok_or(PairError::MalformedTlv)?;
        self.peer_curve_pk.copy_from_slice(&peer_pk);

        // ephemeral key: random → HKDF → X25519 scalar (matches PairingUtils' ECDH-Salt/Info step)
        let mut rnd = [0u8; 32];
        getrandom::getrandom(&mut rnd).map_err(|_| PairError::Rng)?;
        let curve_sk: [u8; 32] =
            hkdf_sha512(&rnd, b"Pair-Verify-ECDH-Salt", b"Pair-Verify-ECDH-Info");
        let secret = StaticSecret::from(curve_sk);
        self.our_curve_pk = PublicKey::from(&secret).to_bytes();
        self.shared = secret.diffie_hellman(&PublicKey::from(self.peer_curve_pk)).to_bytes();
        self.encrypt_key =
            hkdf_sha512(&self.shared, b"Pair-Verify-Encrypt-Salt", b"Pair-Verify-Encrypt-Info");

        // sign ourCurvePK ‖ accessoryID ‖ peerCurvePK
        let mut signed = Vec::with_capacity(32 + self.identity.id.len() + 32);
        signed.extend_from_slice(&self.our_curve_pk);
        signed.extend_from_slice(&self.identity.id);
        signed.extend_from_slice(&self.peer_curve_pk);
        let sig = self.identity.signing.sign(&signed).to_bytes();

        let sub = tlv::encode(&[
            (tlv_type::IDENTIFIER, &self.identity.id),
            (tlv_type::SIGNATURE, &sig),
        ]);
        let enc = chacha_encrypt(&self.encrypt_key, b"PV-Msg02", &sub);

        // C emits EncryptedData, then State, then PublicKey in M2.
        let m2 = tlv::encode(&[
            (tlv_type::ENCRYPTED_DATA, &enc),
            (tlv_type::STATE, &[2]),
            (tlv_type::PUBLIC_KEY, &self.our_curve_pk),
        ]);
        self.state = 2;
        Ok((m2, false))
    }

    fn m3_to_m4<P: PeerStore>(
        &mut self,
        m3: &[(u8, Vec<u8>)],
        peers: &P,
    ) -> Result<(Vec<u8>, bool), PairError> {
        if self.state != 2 {
            return Err(PairError::BadState);
        }
        let enc = first(m3, tlv_type::ENCRYPTED_DATA).ok_or(PairError::MalformedTlv)?;
        let pt = chacha_decrypt(&self.encrypt_key, b"PV-Msg03", &enc)
            .ok_or(PairError::DecryptFailed)?;
        let sub = tlv::decode(&pt).map_err(|_| PairError::MalformedTlv)?;
        let peer_id = first(&sub, tlv_type::IDENTIFIER).ok_or(PairError::MalformedTlv)?;
        let sig = first(&sub, tlv_type::SIGNATURE)
            .filter(|v| v.len() == 64)
            .ok_or(PairError::MalformedTlv)?;

        let result = (|| {
            let peer_ed = peers.find_peer(&peer_id).ok_or(PairError::UnknownPeer)?;
            let vk = VerifyingKey::from_bytes(&peer_ed).map_err(|_| PairError::BadSignature)?;
            let sig = ed25519_dalek::Signature::from_slice(&sig)
                .map_err(|_| PairError::BadSignature)?;
            // controller signed peerCurvePK ‖ controllerID ‖ ourCurvePK
            let mut signed = Vec::with_capacity(32 + peer_id.len() + 32);
            signed.extend_from_slice(&self.peer_curve_pk);
            signed.extend_from_slice(&peer_id);
            signed.extend_from_slice(&self.our_curve_pk);
            vk.verify(&signed, &sig).map_err(|_| PairError::BadSignature)
        })();

        match result {
            Ok(()) => {
                self.done = true;
                Ok((tlv::encode(&[(tlv_type::STATE, &[4])]), true))
            }
            Err(_) => {
                // Kill the handshake, matching `setup`'s M3/M5 failure paths: leaving state = 2 let
                // a peer resend M3 with a different Identifier/Signature indefinitely on one
                // session, i.e. unbounded `find_peer` probing of the peer store. A restart from M1
                // regenerates the ephemeral key.
                self.state = 0xFF;
                Ok((
                    // C emits Error before State on the failure path.
                    tlv::encode(&[
                        (tlv_type::ERROR, &[tlv_error::AUTHENTICATION]),
                        (tlv_type::STATE, &[4]),
                    ]),
                    true,
                ))
            }
        }
    }
}

fn first(items: &[(u8, Vec<u8>)], ty: u8) -> Option<Vec<u8>> {
    items.iter().find(|(t, _)| *t == ty).map(|(_, v)| v.clone())
}
