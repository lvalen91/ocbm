//! Encrypted control channel — after pair-verify succeeds the receiver's RTSP connection becomes a
//! ChaCha20-Poly1305 secured channel. Keys are HKDF-SHA512 from the pair-verify shared secret with
//! salt `"Control-Salt"` and info `"Control-Read-Encryption-Key"` / `"Control-Write-Encryption-Key"`
//! (verbatim from `AirPlayCommon.h`). Frames are `[len:u16 LE][ciphertext][tag:16]`, AAD = the
//! 2-byte length, nonce = 4 zero bytes ‖ 64-bit LE frame counter — the documented AirPlay 2 format.
//!
//! ⚠ The frame layout (LE length, counter nonce, length-as-AAD) follows the open AirPlay 2 spec; it
//! is confirmed against the iPhone at the live cutover (the C uses NetTransportChaCha20Poly1305).

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha512;

const TAG_LEN: usize = 16;

/// Max plaintext per outbound frame — the C transport's `kMaxMessageWriteSize`. Larger messages are
/// split across frames.
const MAX_WRITE: usize = 1024;

/// Max ciphertext length accepted in one inbound frame — the C transport's `kMaxMessageReadSize`.
/// A frame claiming more is rejected (the iPhone drops the connection on oversize).
const MAX_READ: usize = 16 * 1024;

/// Inbound-frame decryption error.
#[derive(Debug, PartialEq, Eq)]
pub enum FrameError {
    /// The frame's declared length exceeds the read cap ([`MAX_READ`]) — a protocol violation
    /// (the real iPhone drops the connection on oversize), NOT a crypto failure.
    Oversized,
    /// ChaCha20-Poly1305 tag verification failed (tampering, or a counter desync).
    AuthFailed,
}

impl core::fmt::Display for FrameError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FrameError::Oversized => {
                f.write_str("control-channel frame declares a length over the read cap")
            }
            FrameError::AuthFailed => f.write_str("control-channel frame authentication failed"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Read/write keys for the encrypted control channel (server perspective: `read` decrypts inbound,
/// `write` encrypts outbound).
#[derive(Clone)]
pub struct ControlKeys {
    pub read: [u8; 32],
    pub write: [u8; 32],
}

/// Derive the control-channel keys from the pair-verify shared secret.
///
/// NOTE the deliberate cross-over: the C receiver derives `readKey`←`Control-Read-Encryption-Key`
/// and `writeKey`←`Control-Write-Encryption-Key`, then **swaps** them when configuring the transport
/// (`NetTransportChaCha20Poly1305Configure(…, writeKey /*as read*/, readKey /*as write*/)`). The info
/// names are from the *controller's* viewpoint, so the **server** decrypts inbound with
/// `Control-Write-Encryption-Key` and encrypts outbound with `Control-Read-Encryption-Key`.
pub fn derive_control_keys(shared_secret: &[u8]) -> ControlKeys {
    ControlKeys {
        read: hkdf32(shared_secret, b"Control-Salt", b"Control-Write-Encryption-Key"),
        write: hkdf32(shared_secret, b"Control-Salt", b"Control-Read-Encryption-Key"),
    }
}

/// Derive the event-channel keys (receiver→iPhone command channel: `hidSendReport`, etc.).
///
/// Unlike the control channel, the C does **not** swap when configuring the event transport
/// (`AirPlayReceiverSession.c` `_ControlStart`: `NetTransportChaCha20Poly1305Configure(…, readKey,
/// writeKey)` — direct). The receiver is the HTTP *client* here, so it encrypts outbound with
/// `Events-Write-Encryption-Key` and decrypts inbound with `Events-Read-Encryption-Key`.
pub fn derive_event_keys(shared_secret: &[u8]) -> ControlKeys {
    ControlKeys {
        read: hkdf32(shared_secret, b"Events-Salt", b"Events-Read-Encryption-Key"),
        write: hkdf32(shared_secret, b"Events-Salt", b"Events-Write-Encryption-Key"),
    }
}

/// A stateful encrypted channel: per-direction monotonic frame counters drive the nonces.
pub struct ControlChannel {
    keys: ControlKeys,
    read_counter: u64,
    write_counter: u64,
}

impl ControlChannel {
    pub fn new(keys: ControlKeys) -> Self {
        Self { keys, read_counter: 0, write_counter: 0 }
    }

    /// Encrypt an outbound message into one or more frames `[len:u16 LE][ciphertext‖tag]`. Messages
    /// larger than [`MAX_WRITE`] are split into ≤`MAX_WRITE`-byte frames, mirroring the C transport's
    /// `kMaxMessageWriteSize` chunking (the peer reassembles the stream), so we never emit a frame
    /// the iPhone would reject for exceeding its read cap.
    pub fn encrypt_frame(&mut self, plaintext: &[u8]) -> Vec<u8> {
        if plaintext.len() <= MAX_WRITE {
            return self.seal_one(plaintext);
        }
        let mut out = Vec::new();
        for chunk in plaintext.chunks(MAX_WRITE) {
            out.extend_from_slice(&self.seal_one(chunk));
        }
        out
    }

    /// Seal exactly one chunk (≤ `MAX_WRITE`) into a single frame, advancing the write counter.
    fn seal_one(&mut self, chunk: &[u8]) -> Vec<u8> {
        let len_le = (chunk.len() as u16).to_le_bytes();
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.keys.write));
        let nonce = nonce12(self.write_counter);
        self.write_counter += 1;
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: chunk, aad: &len_le })
            .expect("control-channel encrypt");
        let mut frame = Vec::with_capacity(2 + ct.len());
        frame.extend_from_slice(&len_le);
        frame.extend_from_slice(&ct);
        frame
    }

    /// Try to decrypt one inbound frame from the front of `buf`. Returns `Ok(None)` if more bytes are
    /// needed, `Ok(Some((plaintext, consumed)))` on success, `Err(FrameError)` on failure — see
    /// [`FrameError`] for which variant means what (an oversize is a protocol violation, not a
    /// tag/counter problem).
    pub fn decrypt_frame(&mut self, buf: &[u8]) -> Result<Option<(Vec<u8>, usize)>, FrameError> {
        if buf.len() < 2 {
            return Ok(None);
        }
        let len = u16::from_le_bytes([buf[0], buf[1]]) as usize;
        // Reject a frame claiming more than the C's kMaxMessageReadSize (16 KB) — the real iPhone
        // drops the connection on an oversized frame, so refuse to decrypt it rather than trust it.
        if len > MAX_READ {
            return Err(FrameError::Oversized);
        }
        let total = 2 + len + TAG_LEN;
        if buf.len() < total {
            return Ok(None);
        }
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.keys.read));
        let nonce = nonce12(self.read_counter);
        let pt = cipher
            .decrypt(Nonce::from_slice(&nonce), Payload { msg: &buf[2..total], aad: &buf[0..2] })
            .map_err(|_| FrameError::AuthFailed)?;
        self.read_counter += 1;
        Ok(Some((pt, total)))
    }
}

fn hkdf32(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha512>::new(Some(salt), ikm);
    let mut okm = [0u8; 32];
    hk.expand(info, &mut okm).expect("32-byte HKDF");
    okm
}

fn nonce12(counter: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_le_bytes());
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_key_direction_matches_apple_swap() {
        // Guards the deliberate read/write cross-over vs the Apple transport (see derive_control_keys).
        let secret = b"abcdefghabcdefghabcdefghabcdefgh";
        let keys = derive_control_keys(secret);
        assert_eq!(keys.read, hkdf32(secret, b"Control-Salt", b"Control-Write-Encryption-Key"));
        assert_eq!(keys.write, hkdf32(secret, b"Control-Salt", b"Control-Read-Encryption-Key"));
    }

    #[test]
    fn control_keys_are_deterministic_and_distinct() {
        let k1 = derive_control_keys(b"shared-secret-32-bytes----------!");
        let k2 = derive_control_keys(b"shared-secret-32-bytes----------!");
        assert_eq!(k1.read, k2.read);
        assert_eq!(k1.write, k2.write);
        assert_ne!(k1.read, k1.write);
    }

    #[test]
    fn frame_round_trip_across_peers() {
        // peer A writes with its `write` key; peer B must read with the SAME key → mirror the keys.
        let a_keys = derive_control_keys(b"abcdefghabcdefghabcdefghabcdefgh");
        let b_keys = ControlKeys { read: a_keys.write, write: a_keys.read };
        let mut a = ControlChannel::new(a_keys);
        let mut b = ControlChannel::new(b_keys);

        for msg in [&b"first frame"[..], b"second, longer frame \x00\x01\x02", b""] {
            let frame = a.encrypt_frame(msg);
            let (pt, used) = b.decrypt_frame(&frame).unwrap().unwrap();
            assert_eq!(used, frame.len());
            assert_eq!(pt, msg);
        }
    }

    #[test]
    fn partial_frame_needs_more() {
        let keys = derive_control_keys(b"k0k0k0k0k0k0k0k0k0k0k0k0k0k0k0k0");
        let mut ch = ControlChannel::new(keys.clone());
        let mut rx = ControlChannel::new(ControlKeys { read: keys.write, write: keys.read });
        let frame = ch.encrypt_frame(b"payload");
        assert_eq!(rx.decrypt_frame(&frame[..3]).unwrap(), None); // header+partial
    }

    #[test]
    fn tampered_frame_fails_auth() {
        let keys = derive_control_keys(b"zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz");
        let mut tx = ControlChannel::new(keys.clone());
        let mut rx = ControlChannel::new(ControlKeys { read: keys.write, write: keys.read });
        let mut frame = tx.encrypt_frame(b"secret");
        let n = frame.len();
        frame[n - 1] ^= 0xFF; // corrupt the tag
        assert_eq!(rx.decrypt_frame(&frame), Err(FrameError::AuthFailed));
    }

    #[test]
    fn oversized_frame_is_a_protocol_error_not_auth() {
        let keys = derive_control_keys(b"oooooooooooooooooooooooooooooooo");
        let mut rx = ControlChannel::new(keys);
        // Declared length 0xFFFF > MAX_READ — rejected before any decrypt is attempted.
        let frame = [0xFFu8, 0xFF, 0x00, 0x00];
        assert_eq!(rx.decrypt_frame(&frame), Err(FrameError::Oversized));
    }
}
