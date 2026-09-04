//! SRP-6a server tuned for HomeKit/CarPlay pair-setup — RFC 5054 3072-bit group, generator 5,
//! SHA-512. The RustCrypto `srp` crate uses the simplified proof `M1 = H(A|B|K)`; HomeKit (and the
//! iPhone) require the full RFC-5054 proof `M1 = H( H(N)⊕H(g) | H(I) | s | A | B | K )` with values
//! zero-padded to the modulus length in every hash. So we implement the accessory (server) side
//! directly with num-bigint, matching the iPhone client (fast-srp-hap conventions).
//!
//! Flow: `new()` picks `b`, computes `v=g^x` and `B=k·v+g^b`. `verify(A, clientProof)` derives the
//! premaster `S=(A·v^u)^b`, `K=H(S)`, checks `M1`, and returns the server proof `M2=H(A|M1|K)`.

use num_bigint::BigUint;
use num_traits::Zero;
use sha2::{Digest, Sha512};
use subtle::ConstantTimeEq;

/// RFC 3526 / RFC 5054 3072-bit MODP prime (generator 5).
const N_HEX: &str = "\
FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74\
020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F1437\
4FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED\
EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF05\
98DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB\
9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3B\
E39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF695581718\
3995497CEA956AE515D2261898FA051015728E5A8AAAC42DAD33170D04507A33\
A85521ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7\
ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864\
D87602733EC86A64521F2B18177B200CBBE117577A615D6C770988C0BAD946E2\
08E24FA074E5AB3143DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF";

fn group_n() -> BigUint {
    BigUint::parse_bytes(N_HEX.as_bytes(), 16).expect("valid 3072-bit prime")
}

fn sha512(parts: &[&[u8]]) -> Vec<u8> {
    let mut h = Sha512::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().to_vec()
}

/// Left-pad a number's big-endian bytes to `len` (the modulus length) — RFC 5054 `PAD()`.
fn pad(x: &BigUint, len: usize) -> Vec<u8> {
    let b = x.to_bytes_be();
    if b.len() >= len {
        b
    } else {
        let mut out = vec![0u8; len - b.len()];
        out.extend_from_slice(&b);
        out
    }
}

/// SRP-6a accessory/server.
pub struct SrpServer {
    n: BigUint,
    n_len: usize,
    salt: Vec<u8>,
    username: Vec<u8>,
    v: BigUint,
    b: BigUint,
    b_pub: BigUint,
    session_key: Option<Vec<u8>>,
}

impl SrpServer {
    /// Create with a fixed server private `b` (random in production via [`new`](Self::new)).
    pub fn with_secret(username: &[u8], password: &[u8], salt: &[u8], b: &[u8]) -> Self {
        let n = group_n();
        let n_len = n.to_bytes_be().len();
        let g = BigUint::from(5u32);
        // k = H(N | PAD(g))
        let k = BigUint::from_bytes_be(&sha512(&[&pad(&n, n_len), &pad(&g, n_len)]));
        // x = H(s | H(I | ":" | P))
        let inner = sha512(&[username, b":", password]);
        let x = BigUint::from_bytes_be(&sha512(&[salt, &inner]));
        let v = g.modpow(&x, &n);
        let b_big = BigUint::from_bytes_be(b);
        // B = (k*v + g^b) mod N
        let b_pub = (&k * &v + g.modpow(&b_big, &n)) % &n;
        SrpServer {
            n,
            n_len,
            salt: salt.to_vec(),
            username: username.to_vec(),
            v,
            b: b_big,
            b_pub,
            session_key: None,
        }
    }

    /// Create with a fresh random server private `b`. `None` if the RNG fails — every sibling
    /// (`setup::m1_to_m2`, `verify`, `mfi::sap`) reports that as an error, and this crate is linked
    /// into a `panic = "abort"` binary where an `expect` would take the whole daemon down.
    pub fn new(username: &[u8], password: &[u8], salt: &[u8]) -> Option<Self> {
        let mut b = [0u8; 32];
        getrandom::getrandom(&mut b).ok()?;
        Some(Self::with_secret(username, password, salt, &b))
    }

    /// The server public value `B`, padded to the modulus length (sent in M2).
    pub fn b_pub(&self) -> Vec<u8> {
        pad(&self.b_pub, self.n_len)
    }

    /// Verify the client's M3 (`A`, `clientProof`). On success returns the server proof `M2` and
    /// stores the session key (HKDF IKM); on mismatch returns `None`.
    pub fn verify(&mut self, a_pub: &[u8], client_proof: &[u8]) -> Option<Vec<u8>> {
        // A legitimate A is at most the modulus length. TLV8 coalescing lets a peer deliver an
        // arbitrarily long PublicKey, which would otherwise feed BigUint multiplication and `pad()`
        // (which hands back oversized bytes UNPADDED, so the hashes would silently change shape).
        if a_pub.len() > self.n_len {
            return None;
        }
        let a = BigUint::from_bytes_be(a_pub);
        if (&a % &self.n).is_zero() {
            return None;
        }
        // u = H(PAD(A) | PAD(B))
        let u = BigUint::from_bytes_be(&sha512(&[&pad(&a, self.n_len), &pad(&self.b_pub, self.n_len)]));
        // RFC 5054 §2.5.4: abort on u == 0 (2^-512, but the check is one line).
        if u.is_zero() {
            return None;
        }
        // S = (A * v^u)^b mod N
        let s = (&a * self.v.modpow(&u, &self.n) % &self.n).modpow(&self.b, &self.n);
        // K = H(PAD(S))
        let k = sha512(&[&pad(&s, self.n_len)]);
        // M1 = H( H(N) XOR H(g) | H(I) | s | PAD(A) | PAD(B) | K ).
        // NOTE: H(g) is over the MINIMAL generator byte ([5]) — NOT padded to N (verified against a
        // live iPhone capture; this is the one place HomeKit diverges from a naive PAD-everything read).
        let hn = sha512(&[&pad(&self.n, self.n_len)]);
        let hg = sha512(&[&[5u8]]);
        let hng: Vec<u8> = hn.iter().zip(&hg).map(|(x, y)| x ^ y).collect();
        let hi = sha512(&[&self.username]);
        let m1 = sha512(&[
            &hng,
            &hi,
            &self.salt,
            &pad(&a, self.n_len),
            &pad(&self.b_pub, self.n_len),
            &k,
        ]);
        // Constant-time compare — this is the setup-code proof, so a timing-variable `!=` would leak
        // how many prefix bytes of a guess matched. Length mismatch yields 0, same as `!=`.
        if m1.ct_eq(client_proof).unwrap_u8() == 0 {
            return None;
        }
        // M2 = H(PAD(A) | M1 | K)
        let m2 = sha512(&[&pad(&a, self.n_len), &m1, &k]);
        self.session_key = Some(k);
        Some(m2)
    }

    /// The SRP session key `K` (HKDF input keying material) — available after a successful verify.
    pub fn session_key(&self) -> Option<&[u8]> {
        self.session_key.as_deref()
    }
}

/// Reference SRP-6a client (controller side), mirroring [`SrpServer`] — for tests and tooling.
/// Returns `(A padded, M1 client proof, K session key)` given the client private `a`.
pub fn client_proof(
    username: &[u8],
    password: &[u8],
    salt: &[u8],
    a_priv: &[u8],
    b_pub: &[u8],
) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let n = group_n();
    let n_len = n.to_bytes_be().len();
    let g = BigUint::from(5u32);
    let a = BigUint::from_bytes_be(a_priv);
    let a_pub = g.modpow(&a, &n);
    let bb = BigUint::from_bytes_be(b_pub);
    let k = BigUint::from_bytes_be(&sha512(&[&pad(&n, n_len), &pad(&g, n_len)]));
    let x = BigUint::from_bytes_be(&sha512(&[salt, &sha512(&[username, b":", password])]));
    let v = g.modpow(&x, &n);
    let u = BigUint::from_bytes_be(&sha512(&[&pad(&a_pub, n_len), &pad(&bb, n_len)]));
    // S = (B - k*v)^(a + u*x) mod N
    let kv = (&k * &v) % &n;
    let base = (&bb + &n - kv) % &n;
    let exp = &a + &u * &x;
    let s = base.modpow(&exp, &n);
    let key = sha512(&[&pad(&s, n_len)]);
    let hn = sha512(&[&pad(&n, n_len)]);
    let hg = sha512(&[&[5u8]]);
    let hng: Vec<u8> = hn.iter().zip(&hg).map(|(p, q)| p ^ q).collect();
    let hi = sha512(&[username]);
    let m1 = sha512(&[&hng, &hi, salt, &pad(&a_pub, n_len), &pad(&bb, n_len), &key]);
    (pad(&a_pub, n_len), m1, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(username: &[u8], password: &[u8], salt: &[u8], a_priv: &[u8], b_pub: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        client_proof(username, password, salt, a_priv, b_pub)
    }

    #[test]
    fn srp_round_trip() {
        let (user, pass, salt) = (b"Pair-Setup".as_slice(), b"3939".as_slice(), &[1u8; 16][..]);
        let mut server = SrpServer::with_secret(user, pass, salt, &[2u8; 32]);
        let (a_pub, m1, ckey) = client(user, pass, salt, &[3u8; 32], &server.b_pub());
        let m2 = server.verify(&a_pub, &m1).expect("client proof verifies");
        assert_eq!(server.session_key().unwrap(), ckey.as_slice(), "shared key agrees");
        // client checks M2 = H(A | M1 | K)
        let expect_m2 = sha512(&[&a_pub, &m1, &ckey]);
        assert_eq!(m2, expect_m2);
    }

    /// An oversized `A` must be refused before it reaches `pad()`, which would return it unpadded.
    #[test]
    fn oversized_a_rejected() {
        let mut server = SrpServer::with_secret(b"Pair-Setup", b"3939", &[1u8; 16], &[2u8; 32]);
        let n_len = server.b_pub().len();
        assert!(server.verify(&vec![0xAAu8; n_len + 1], &[0u8; 64]).is_none());
        assert!(server.session_key().is_none());
    }

    #[test]
    fn new_returns_a_server() {
        assert!(SrpServer::new(b"Pair-Setup", b"3939", &[1u8; 16]).is_some());
    }

    #[test]
    fn wrong_proof_rejected() {
        let mut server = SrpServer::with_secret(b"Pair-Setup", b"3939", &[1u8; 16], &[2u8; 32]);
        let (a_pub, _m1, _k) = client(b"Pair-Setup", b"3939", &[1u8; 16], &[3u8; 32], &server.b_pub());
        assert!(server.verify(&a_pub, &[0u8; 64]).is_none());
        assert!(server.session_key().is_none());
    }
}
