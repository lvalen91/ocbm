//! pair-setup (accessory/server) — first-time pairing. SRP-6a (RFC 5054 3072-bit group, SHA-512)
//! proves the setup code, then an Ed25519 long-term-key exchange (ChaCha20-encrypted) records the
//! pairing so future sessions can pair-verify. Clean-room from `PairingUtils.c` facts: exact HKDF
//! salt/info strings, ChaCha20 PS-Msg05/06 nonces, and signing orders preserved. SRP-6a is our own
//! [`crate::srp`] (num-bigint, HomeKit `M1 = H(H(N)⊕H(g)|H(I)|s|A|B|K)` proof — the RustCrypto `srp`
//! crate's simplified proof is wire-incompatible); the LTPK exchange + key derivation reuse
//! [`crate::crypto`].
//!
//! ⚠ SRP wire-compatibility with the iPhone's client is the single thing **not** provable offline
//! (we have no captured pair-setup). It is confirmed at the live cutover; this module's tests prove
//! internal consistency (srp-client ↔ srp-server + the LTPK exchange round-trip).
//!
//! Flow: in `M1{State=1,Method}` → out `M2{State=2,Salt,PublicKey=B}`; in
//! `M3{State=3,PublicKey=A,Proof=M1}` → out `M4{State=4,Proof=M2}`; in
//! `M5{State=5,EncryptedData=enc(Identifier,PublicKey,Signature)}` → out
//! `M6{State=6,EncryptedData=enc(Identifier,PublicKey,Signature)}`.

use ed25519_dalek::{Signer, Verifier, VerifyingKey};

use crate::crypto::{chacha_decrypt, chacha_encrypt, hkdf_sha512, tlv_error, tlv_type};
use crate::srp::SrpServer;
use crate::tlv;
use crate::verify::Identity;

const USERNAME: &[u8] = b"Pair-Setup";

/// Records a newly-paired controller's Ed25519 long-term public key.
pub trait PeerSaver {
    fn save_peer(&mut self, id: &[u8], ltpk: [u8; 32]);
}

#[derive(Debug, PartialEq, Eq)]
pub enum SetupError {
    BadState,
    MalformedTlv,
    Srp,
    DecryptFailed,
}

impl core::fmt::Display for SetupError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            SetupError::BadState => "unexpected pair-setup state",
            SetupError::MalformedTlv => "malformed pair-setup TLV",
            SetupError::Srp => "SRP failure",
            SetupError::DecryptFailed => "pair-setup decrypt/auth failed",
        };
        f.write_str(s)
    }
}

impl std::error::Error for SetupError {}

/// One pair-setup exchange. Drive with successive [`exchange`](Self::exchange) calls.
// Anti-brute-force for pair-setup (audit #2), mirroring the MFi-SAP throttle in mfi/sap.rs. A valid
// controller sends ONE correct setup-code proof; repeated wrong M3 proofs are an online guessing attack
// against a low-entropy setup code. We (a) invalidate the SRP session on each wrong proof so a single
// connection can't retry the same handshake, (b) throttle with a growing backoff, and (c) after
// SETUP_MAX_TRIES process-wide failures refuse all further attempts with kTLVError_MaxTries until the
// process restarts / pairing is re-armed. Cleared on a successful proof.
static SETUP_FAILS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
const SETUP_MAX_TRIES: u32 = 10;
const STATE_DEAD: u8 = 0xFF;
fn setup_note_fail() -> u32 {
    SETUP_FAILS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}
fn setup_backoff(fails: u32) {
    if fails > 3 {
        let ms = ((fails - 3) as u64 * 300).min(3000);
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

pub struct PairSetupServer<'a> {
    identity: &'a Identity,
    setup_code: Vec<u8>,
    state: u8,
    srp: Option<SrpServer>,
    srp_key: Vec<u8>,
    done: bool,
}

impl<'a> PairSetupServer<'a> {
    pub fn new(identity: &'a Identity, setup_code: impl Into<Vec<u8>>) -> Self {
        Self {
            identity,
            setup_code: setup_code.into(),
            state: 0,
            srp: None,
            srp_key: Vec::new(),
            done: false,
        }
    }

    /// True once M6 has been produced (pairing recorded).
    pub fn is_done(&self) -> bool {
        self.done
    }

    pub fn exchange<P: PeerSaver>(
        &mut self,
        input: &[u8],
        peers: &mut P,
    ) -> Result<(Vec<u8>, bool), SetupError> {
        let items = tlv::decode(input).map_err(|_| SetupError::MalformedTlv)?;
        let state = first(&items, tlv_type::STATE).ok_or(SetupError::MalformedTlv)?;
        match state.first() {
            Some(1) => self.m1_to_m2(),
            Some(3) => self.m3_to_m4(&items),
            Some(5) => self.m5_to_m6(&items, peers),
            _ => Err(SetupError::BadState),
        }
    }

    fn m1_to_m2(&mut self) -> Result<(Vec<u8>, bool), SetupError> {
        // Refuse to START a new handshake once the process-wide failure cap is hit (audit #2): without
        // this, an attacker sidesteps per-session SRP invalidation by opening a fresh session per guess.
        if SETUP_FAILS.load(std::sync::atomic::Ordering::Relaxed) > SETUP_MAX_TRIES {
            return Ok((
                tlv::encode(&[
                    (tlv_type::ERROR, &[tlv_error::MAX_TRIES]),
                    (tlv_type::STATE, &[2]),
                ]),
                true,
            ));
        }
        let mut salt = [0u8; 16];
        getrandom::getrandom(&mut salt).map_err(|_| SetupError::Srp)?;
        let server = SrpServer::new(USERNAME, &self.setup_code, &salt);
        let b_pub = server.b_pub();
        self.srp = Some(server);
        self.state = 2;
        Ok((
            tlv::encode(&[
                (tlv_type::STATE, &[2]),
                (tlv_type::SALT, &salt),
                (tlv_type::PUBLIC_KEY, &b_pub),
            ]),
            false,
        ))
    }

    fn m3_to_m4(&mut self, m3: &[(u8, Vec<u8>)]) -> Result<(Vec<u8>, bool), SetupError> {
        if self.state != 2 {
            return Err(SetupError::BadState);
        }
        let a_pub = first(m3, tlv_type::PUBLIC_KEY).ok_or(SetupError::MalformedTlv)?;
        let client_proof = first(m3, tlv_type::PROOF).ok_or(SetupError::MalformedTlv)?;
        let server = self.srp.as_mut().ok_or(SetupError::BadState)?;
        // (Removed the CARPLAY_SRP_CAPTURE diagnostic that dumped the setup code + SRP private exponent
        // to disk — a secret leak in release builds. The M1 formula is now validated against a real
        // iPhone, so the capture is no longer needed.)

        match server.verify(&a_pub, &client_proof) {
            Some(m2_proof) => {
                self.srp_key = server.session_key().expect("verified ⇒ key").to_vec();
                self.state = 4;
                SETUP_FAILS.store(0, std::sync::atomic::Ordering::Relaxed); // good proof — clear the cap
                Ok((
                    tlv::encode(&[(tlv_type::STATE, &[4]), (tlv_type::PROOF, &m2_proof)]),
                    false,
                ))
            }
            None => {
                // Wrong client proof = a bad setup-code guess (audit #2). INVALIDATE the SRP session so
                // this connection can't retry the same handshake (the attacker must restart from M1),
                // then count + throttle, and escalate to MaxTries once the process-wide cap is exceeded.
                self.srp = None;
                self.state = STATE_DEAD;
                let fails = setup_note_fail();
                setup_backoff(fails);
                let err = if fails > SETUP_MAX_TRIES {
                    tlv_error::MAX_TRIES
                } else {
                    tlv_error::AUTHENTICATION
                };
                // C emits Error before State on the failure path.
                Ok((
                    tlv::encode(&[(tlv_type::ERROR, &[err]), (tlv_type::STATE, &[4])]),
                    true,
                ))
            }
        }
    }

    fn m5_to_m6<P: PeerSaver>(
        &mut self,
        m5: &[(u8, Vec<u8>)],
        peers: &mut P,
    ) -> Result<(Vec<u8>, bool), SetupError> {
        if self.state != 4 {
            return Err(SetupError::BadState);
        }
        let key: [u8; 32] =
            hkdf_sha512(&self.srp_key, b"Pair-Setup-Encrypt-Salt", b"Pair-Setup-Encrypt-Info");
        let enc = first(m5, tlv_type::ENCRYPTED_DATA).ok_or(SetupError::MalformedTlv)?;
        let pt = chacha_decrypt(&key, b"PS-Msg05", &enc).ok_or(SetupError::DecryptFailed)?;
        let sub = tlv::decode(&pt).map_err(|_| SetupError::MalformedTlv)?;
        let ctrl_id = first(&sub, tlv_type::IDENTIFIER).ok_or(SetupError::MalformedTlv)?;
        let ctrl_ltpk = first(&sub, tlv_type::PUBLIC_KEY)
            .filter(|v| v.len() == 32)
            .ok_or(SetupError::MalformedTlv)?;
        let ctrl_sig = first(&sub, tlv_type::SIGNATURE)
            .filter(|v| v.len() == 64)
            .ok_or(SetupError::MalformedTlv)?;

        // verify controller signed HKDF(K,Controller-Sign) ‖ ctrlID ‖ ctrlLTPK
        let dk: [u8; 32] = hkdf_sha512(
            &self.srp_key,
            b"Pair-Setup-Controller-Sign-Salt",
            b"Pair-Setup-Controller-Sign-Info",
        );
        let mut signed = Vec::with_capacity(32 + ctrl_id.len() + 32);
        signed.extend_from_slice(&dk);
        signed.extend_from_slice(&ctrl_id);
        signed.extend_from_slice(&ctrl_ltpk);
        let mut ltpk = [0u8; 32];
        ltpk.copy_from_slice(&ctrl_ltpk);
        let ok = VerifyingKey::from_bytes(&ltpk).ok().is_some_and(|vk| {
            ed25519_dalek::Signature::from_slice(&ctrl_sig)
                .map(|sig| vk.verify(&signed, &sig).is_ok())
                .unwrap_or(false)
        });
        if !ok {
            // Kill the handshake (matching the M3 failure path): leaving state = 4 here would let
            // the same connection re-drive M5 against the same SRP key indefinitely.
            self.state = STATE_DEAD;
            return Ok((
                // C emits Error before State on the failure path.
                tlv::encode(&[
                    (tlv_type::ERROR, &[tlv_error::AUTHENTICATION]),
                    (tlv_type::STATE, &[6]),
                ]),
                true,
            ));
        }
        peers.save_peer(&ctrl_id, ltpk);

        // accessory side: sign HKDF(K,Accessory-Sign) ‖ accID ‖ accLTPK
        let ak: [u8; 32] = hkdf_sha512(
            &self.srp_key,
            b"Pair-Setup-Accessory-Sign-Salt",
            b"Pair-Setup-Accessory-Sign-Info",
        );
        let acc_ltpk = self.identity.public_key();
        let mut signed2 = Vec::with_capacity(32 + self.identity.id.len() + 32);
        signed2.extend_from_slice(&ak);
        signed2.extend_from_slice(&self.identity.id);
        signed2.extend_from_slice(&acc_ltpk);
        let sig = self.identity.signing.sign(&signed2).to_bytes();

        let sub2 = tlv::encode(&[
            (tlv_type::IDENTIFIER, &self.identity.id),
            (tlv_type::PUBLIC_KEY, &acc_ltpk),
            (tlv_type::SIGNATURE, &sig),
        ]);
        let enc2 = chacha_encrypt(&key, b"PS-Msg06", &sub2);
        self.done = true;
        // Terminal: a replayed M5 after success must not re-drive M6 (the m5_to_m6 state guard
        // rejects anything but state 4).
        self.state = 6;
        Ok((
            // C emits EncryptedData before State in M6.
            tlv::encode(&[
                (tlv_type::ENCRYPTED_DATA, &enc2),
                (tlv_type::STATE, &[6]),
            ]),
            true,
        ))
    }
}

fn first(items: &[(u8, Vec<u8>)], ty: u8) -> Option<Vec<u8>> {
    items.iter().find(|(t, _)| *t == ty).map(|(_, v)| v.clone())
}
