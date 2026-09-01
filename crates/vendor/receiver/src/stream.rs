//! A/V stream crypto + RTP — the deterministic, offline-testable core of the receiver session
//! (`AirPlayReceiverSession.c` subsystems #2/#4/#9). Clean-room from the protocol facts:
//!
//! - **Per-stream keys** (`_GetStreamSecurityKeys`): HKDF-SHA512 of the pair-verify shared secret,
//!   salt `"DataStream-Salt"+<streamConnectionID decimal>`, info `"DataStream-Output-Encryption-Key"`
//!   / `"DataStream-Input-Encryption-Key"`, 32-byte keys.
//! - **RTP**: the 12-byte header (`v_p_x_cc, m_pt, seq, ts, ssrc`, network order).
//! - **Audio packet**: ChaCha20-Poly1305 over payload laid out `[ciphertext][tag:16][nonce:8]`; the
//!   8-byte little-endian per-packet nonce is the trailing bytes, AAD = `ts‖ssrc` (RTP header bytes
//!   4..12) for a modern iOS client (`_MainAltAudioGetAADFromRTPHeader`, client > 13A1).
//!
//! The live session loop (sockets, jitter buffer, the iPhone's actual RTP) is the cutover; this
//! module is verified by encrypt→decrypt and parse round-trips.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha512;

/// Output (receiver→sender) and input (sender→receiver) keys for one data stream.
pub struct StreamKeys {
    pub output: [u8; 32],
    pub input: [u8; 32],
}

/// Derive a stream's keys from the pair-verify shared secret and its connection ID.
pub fn derive_stream_keys(pairing_shared: &[u8], stream_connection_id: u64) -> StreamKeys {
    let salt = format!("DataStream-Salt{stream_connection_id}");
    StreamKeys {
        output: hkdf32(pairing_shared, salt.as_bytes(), b"DataStream-Output-Encryption-Key"),
        input: hkdf32(pairing_shared, salt.as_bytes(), b"DataStream-Input-Encryption-Key"),
    }
}

/// The 12-byte RTP header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpHeader {
    pub version: u8,
    pub marker: bool,
    pub payload_type: u8,
    pub seq: u16,
    pub ts: u32,
    pub ssrc: u32,
}

pub const RTP_HEADER_SIZE: usize = 12;

impl RtpHeader {
    pub fn parse(buf: &[u8]) -> Option<RtpHeader> {
        if buf.len() < RTP_HEADER_SIZE {
            return None;
        }
        Some(RtpHeader {
            version: buf[0] >> 6,
            marker: buf[1] & 0x80 != 0,
            payload_type: buf[1] & 0x7f,
            seq: u16::from_be_bytes([buf[2], buf[3]]),
            ts: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
            ssrc: u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]),
        })
    }
}

/// The minimum encrypted audio packet: RTP header + tag(16) + nonce(8).
pub const MIN_AUDIO_PACKET: usize = RTP_HEADER_SIZE + 16 + 8;

/// Decrypt an audio RTP packet (`key` = the stream's input key). Returns the plaintext AU, or `None`
/// on malformed input / auth failure. Layout after the 12-byte header: `[ciphertext][tag:16][nonce:8]`,
/// AAD = `ts‖ssrc` (RTP header bytes 4..12) — the modern-client form (iOS > 13A1) the live receiver
/// uses. Use [`decrypt_audio_aad`] directly if a pre-13A1 (full 12-byte header) AAD is ever needed.
pub fn decrypt_audio(key: &[u8; 32], packet: &[u8]) -> Option<Vec<u8>> {
    if packet.len() < MIN_AUDIO_PACKET {
        return None;
    }
    decrypt_audio_aad(key, packet, &packet[4..RTP_HEADER_SIZE])
}

/// Decrypt an audio RTP packet with an explicit AAD slice. Per the C `_MainAltAudioGetAADFromRTPHeader`,
/// the AAD depends on the client OS version (NOT the stream type): a modern iOS client (> 13A1) uses
/// `ts‖ssrc` (header bytes 4..12, 8 bytes) for every audio stream — verified live for type-100 ELD —
/// while a pre-13A1 client uses the full 12-byte header. `packet` is the whole datagram (header + payload).
pub fn decrypt_audio_aad(key: &[u8; 32], packet: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
    if packet.len() < MIN_AUDIO_PACKET {
        return None;
    }
    let payload = &packet[RTP_HEADER_SIZE..];
    let n = payload.len();
    let nonce = nonce12(&payload[n - 8..]);
    let ct_and_tag = &payload[..n - 8]; // ciphertext ‖ 16-byte tag
    ChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(Nonce::from_slice(&nonce), Payload { msg: ct_and_tag, aad })
        .ok()
}

/// Encrypt an AU into an audio RTP packet with an explicit AAD slice (the encrypt counterpart of
/// [`decrypt_audio_aad`]). The mic uplink uses `ts‖ssrc` (header bytes 4..12) for a modern iOS client,
/// matching the C `_MainAltAudioGetAADFromRTPHeader`. Layout: `[header][ciphertext][tag:16][nonce8]`.
pub fn encrypt_audio_aad(
    key: &[u8; 32],
    header: &[u8; 12],
    au: &[u8],
    nonce8: &[u8; 8],
    aad: &[u8],
) -> Vec<u8> {
    let nonce = nonce12(nonce8);
    let ct_tag = ChaCha20Poly1305::new(Key::from_slice(key))
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: au, aad })
        .expect("audio encrypt");
    let mut pkt = Vec::with_capacity(12 + ct_tag.len() + 8);
    pkt.extend_from_slice(header);
    pkt.extend_from_slice(&ct_tag);
    pkt.extend_from_slice(nonce8);
    pkt
}

/// Encrypt an AU into an audio RTP packet (the sender side — used for tests and the mic-style uplink).
/// AAD is `ts‖ssrc` (header bytes 4..12), the modern-client form matching [`decrypt_audio`].
pub fn encrypt_audio(key: &[u8; 32], header: &[u8; 12], au: &[u8], nonce8: &[u8; 8]) -> Vec<u8> {
    encrypt_audio_aad(key, header, au, nonce8, &header[4..12])
}

fn hkdf32(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha512>::new(Some(salt), ikm);
    let mut okm = [0u8; 32];
    hk.expand(info, &mut okm).expect("32-byte HKDF");
    okm
}

fn nonce12(nonce8: &[u8]) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(nonce8);
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_keys_deterministic_distinct_per_id() {
        let secret = [0x5a; 32];
        let a = derive_stream_keys(&secret, 42);
        let b = derive_stream_keys(&secret, 42);
        let c = derive_stream_keys(&secret, 43);
        assert_eq!(a.output, b.output);
        assert_eq!(a.input, b.input);
        assert_ne!(a.output, a.input);
        assert_ne!(a.output, c.output); // connection id is in the salt
    }

    #[test]
    fn rtp_header_parses() {
        let buf = [0x80, 0xE0, 0x12, 0x34, 0x00, 0x00, 0x10, 0x00, 0xDE, 0xAD, 0xBE, 0xEF, 0x99];
        let h = RtpHeader::parse(&buf).unwrap();
        assert_eq!(h.version, 2);
        assert!(h.marker);
        assert_eq!(h.payload_type, 0x60);
        assert_eq!(h.seq, 0x1234);
        assert_eq!(h.ts, 0x1000);
        assert_eq!(h.ssrc, 0xDEADBEEF);
        assert!(RtpHeader::parse(&buf[..11]).is_none());
    }

    #[test]
    fn audio_packet_round_trip() {
        let keys = derive_stream_keys(&[7u8; 32], 100);
        let header = [0x80, 0x60, 0x00, 0x01, 0x00, 0x00, 0x04, 0x00, 0x11, 0x22, 0x33, 0x44];
        let au = b"an AAC access unit payload";
        let nonce8 = [1, 0, 0, 0, 0, 0, 0, 0];
        let pkt = encrypt_audio(&keys.input, &header, au, &nonce8);
        assert_eq!(&pkt[..12], &header); // header is the plaintext prefix (AAD = bytes 4..12)
        assert_eq!(&pkt[pkt.len() - 8..], &nonce8); // nonce trails
        let pt = decrypt_audio(&keys.input, &pkt).unwrap();
        assert_eq!(pt, au);
    }

    #[test]
    fn tampered_audio_fails() {
        let keys = derive_stream_keys(&[7u8; 32], 100);
        let header = [0x80, 0x60, 0, 1, 0, 0, 4, 0, 0, 0, 0, 1];
        let mut pkt = encrypt_audio(&keys.input, &header, b"payload", &[2, 0, 0, 0, 0, 0, 0, 0]);
        let mid = pkt.len() / 2;
        pkt[mid] ^= 0xFF;
        assert!(decrypt_audio(&keys.input, &pkt).is_none());
    }
}
