//! pairing — AirPlay pair-setup (SRP-6a), pair-verify (Curve25519/Ed25519/ChaCha20), and the
//! TLV8 codec they ride on. Clean-room reimplementation of the `PairingUtils.c` / `TLVUtils.c`
//! protocol flow on top of the `srp`, `x25519-dalek`, `ed25519-dalek`, and `chacha20poly1305`
//! crates (added when pair-verify/pair-setup land).
//!
//! Migration phase 1 (done): the TLV8 codec — see [`tlv`]. Phase 3 adds the pairing handshakes.

#![forbid(unsafe_code)]

pub mod crypto;
pub mod setup;
pub mod srp;
pub mod tlv;
pub mod verify;
