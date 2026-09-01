//! MFi signer abstraction — the [`MfiSigner`] trait the MFi-SAP server calls to fetch the accessory
//! certificate and sign challenge digests. The production impl is airplayd's `LocalMfiSigner` over the
//! local i2c MFi chip; tests use a mock. (audit Fix #19 removed the dead `MfiAuthClient` TCP client to
//! the ncm_carplayd auth service — all production MFi is the local coprocessor.)

use std::io;

/// Supplies the MFi certificate + RSA signatures (the genuine MFi chip on the CCPA, reached directly
/// over local I2C). Abstracted as a trait so the MFi-SAP server can be unit-tested with a mock signer.
pub trait MfiSigner {
    /// The accessory's MFi certificate (DER), used by the phone to verify the signature.
    fn copy_certificate(&mut self) -> io::Result<Vec<u8>>;
    /// RSA signature over a 20-byte SHA-1 digest.
    fn create_signature(&mut self, digest: &[u8]) -> io::Result<Vec<u8>>;
}
