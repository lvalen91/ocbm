//! mfi — MFi authentication for the CarPlay head unit.
//!
//! **The live part of this crate is `sap` (MFi-SAP) and the `MfiSigner` trait in `auth_client`.**
//! Production MFi is direct local I2C to the coprocessor — see airplayd's `LocalMfiSigner`,
//! `crates/vendor/mfi-i2c-local`, `crates/vendor/wireless/src/mfi_local.rs` and `ccpa/iap2d`.
//!
//! The request/response framing helpers below (`build_get_certificate`, `build_sign`,
//! `parse_response_header`) are **dead code**: they spoke to a remote `ncm_carplayd` auth service over
//! TCP, whose client was deleted as audit Fix #19. They have no non-test callers. Kept only because
//! `ocbmd`'s `CH_MFI` handler speaks a byte-identical `[op][len:u16 BE]` shape by hand; unify or delete.
//!
//! ## Wire (big-endian u16 framing)
//! ```text
//! request : [op:u8][plen:u16 BE][payload]
//! response: [status:u8][rlen:u16 BE][payload]
//! ```
//! - `op` 0x01 = copy-certificate (plen 0; response payload = 945-byte cert)
//! - `op` 0x02 = create-signature (plen 20 = SHA-1 digest; response payload = 128-byte sig)
//! - `status` 0x00 = OK (anything else: payload[0] is an error code)
#![forbid(unsafe_code)]

pub mod auth_client;
pub mod sap;

/// Copy the accessory's MFi certificate (no request payload).
pub const OP_COPY_CERTIFICATE: u8 = 0x01;
/// Create an RSA signature over a 20-byte challenge digest.
pub const OP_CREATE_SIGNATURE: u8 = 0x02;
/// Response status byte for success.
pub const STATUS_OK: u8 = 0x00;
/// Length of the challenge digest signed by `OP_CREATE_SIGNATURE`.
pub const DIGEST_LEN: usize = 20;
/// Length of the signature returned by `OP_CREATE_SIGNATURE`.
pub const SIG_LEN: usize = 128;
/// Fixed length of the request/response header.
pub const HDR_LEN: usize = 3;

/// Build a request frame: `[op][plen:u16 BE][payload]`.
pub fn build_request(op: u8, payload: &[u8]) -> Vec<u8> {
    let plen = payload.len() as u16;
    let mut v = Vec::with_capacity(HDR_LEN + payload.len());
    v.push(op);
    v.extend_from_slice(&plen.to_be_bytes());
    v.extend_from_slice(payload);
    v
}

/// The `copy-certificate` request (op 0x01, empty payload).
pub fn build_get_certificate() -> Vec<u8> {
    build_request(OP_COPY_CERTIFICATE, &[])
}

/// The `create-signature` request (op 0x02) over a 20-byte digest.
/// Returns `None` if `digest` is not exactly [`DIGEST_LEN`] bytes.
pub fn build_sign(digest: &[u8]) -> Option<Vec<u8>> {
    if digest.len() != DIGEST_LEN {
        return None;
    }
    Some(build_request(OP_CREATE_SIGNATURE, digest))
}

/// A parsed response header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RespHeader {
    pub status: u8,
    pub len: usize,
}

impl RespHeader {
    pub fn is_ok(&self) -> bool {
        self.status == STATUS_OK
    }
}

/// Parse the 3-byte response header `[status][rlen:u16 BE]`.
/// Returns `None` if fewer than [`HDR_LEN`] bytes are present.
pub fn parse_response_header(hdr: &[u8]) -> Option<RespHeader> {
    if hdr.len() < HDR_LEN {
        return None;
    }
    Some(RespHeader {
        status: hdr[0],
        len: ((hdr[1] as usize) << 8) | hdr[2] as usize,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cert_request_is_three_bytes() {
        // op 0x01, plen 0 -> 01 00 00
        assert_eq!(build_get_certificate(), [0x01, 0x00, 0x00]);
    }

    #[test]
    fn sign_request_frames_20b_digest() {
        let digest = [0x11u8; 20];
        let req = build_sign(&digest).unwrap();
        // op 0x02, plen 0x0014 (=20), then the digest
        assert_eq!(&req[..3], &[0x02, 0x00, 0x14]);
        assert_eq!(&req[3..], &digest);
        assert_eq!(req.len(), HDR_LEN + DIGEST_LEN);
    }

    #[test]
    fn sign_rejects_wrong_digest_len() {
        assert!(build_sign(&[0u8; 19]).is_none());
        assert!(build_sign(&[0u8; 21]).is_none());
    }

    #[test]
    fn response_header_roundtrips() {
        // a 945-byte OK cert response header: 00 03 b1
        let h = parse_response_header(&[0x00, 0x03, 0xb1]).unwrap();
        assert!(h.is_ok());
        assert_eq!(h.len, 945);
    }
}
