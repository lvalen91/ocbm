//! Encapsulated-TLS engine for the Android Auto handshake.
//!
//! Same TLS role aasdk's head unit takes (`src/Transport/SSLWrapper.cpp`): the head unit is the TLS
//! *client* (`SSL_set_connect_state`) using TLSv1.2, presenting the head-unit
//! certificate + private key, with peer verification disabled (aasdk passes
//! `SSL_VERIFY_NONE`). TLS records never touch the socket directly — they move
//! through a pair of memory BIOs so each flight can be wrapped in an Android Auto
//! `ENCAPSULATED_SSL` control frame.
//!
//! We use the `openssl` crate only to parse the PEM cert/key into raw handles,
//! then drive the handshake through OpenSSL's C API so the bytes on the wire match what
//! the phone expects.

use foreign_types_shared::ForeignType;
use openssl::pkey::PKey;
use openssl::x509::{X509, X509NameRef};
use std::os::raw::{c_char, c_int, c_long, c_void};
use std::ptr;

// ---- opaque OpenSSL types ----
#[allow(non_camel_case_types)]
type SSL_CTX = c_void;
#[allow(non_camel_case_types)]
type SSL = c_void;
#[allow(non_camel_case_types)]
type SSL_METHOD = c_void;
#[allow(non_camel_case_types)]
type SSL_CIPHER = c_void;
#[allow(non_camel_case_types)]
type BIO = c_void;
#[allow(non_camel_case_types)]
type BIO_METHOD = c_void;
#[allow(non_camel_case_types)]
type X509_ = c_void;
#[allow(non_camel_case_types)]
type EVP_PKEY = c_void;

const SSL_VERIFY_NONE: c_int = 0;
const SSL_ERROR_WANT_READ: c_int = 2;
const SSL_ERROR_WANT_WRITE: c_int = 3;
const TLS1_2_VERSION: c_int = 0x0303; // Android Auto is a TLS 1.2 protocol

extern "C" {
    fn TLS_client_method() -> *const SSL_METHOD;
    fn SSL_CTX_new(method: *const SSL_METHOD) -> *mut SSL_CTX;
    fn SSL_CTX_use_certificate(ctx: *mut SSL_CTX, x: *mut X509_) -> c_int;
    fn SSL_CTX_use_PrivateKey(ctx: *mut SSL_CTX, pkey: *mut EVP_PKEY) -> c_int;
    fn SSL_CTX_set_verify(ctx: *mut SSL_CTX, mode: c_int, cb: *const c_void);
    // set_min/max_proto_version are macros in OpenSSL 3.x; call the underlying ctrl.
    fn SSL_CTX_ctrl(ctx: *mut SSL_CTX, cmd: c_int, larg: c_long, parg: *mut c_void) -> c_long;
    fn SSL_new(ctx: *mut SSL_CTX) -> *mut SSL;
    fn SSL_set_bio(ssl: *mut SSL, rbio: *mut BIO, wbio: *mut BIO);
    fn SSL_set_connect_state(ssl: *mut SSL);
    fn SSL_do_handshake(ssl: *mut SSL) -> c_int;
    fn SSL_get_error(ssl: *const SSL, ret: c_int) -> c_int;
    fn SSL_write(ssl: *mut SSL, buf: *const c_void, num: c_int) -> c_int;
    fn SSL_read(ssl: *mut SSL, buf: *mut c_void, num: c_int) -> c_int;
    fn SSL_get_current_cipher(ssl: *const SSL) -> *const SSL_CIPHER;
    fn SSL_CIPHER_get_name(c: *const SSL_CIPHER) -> *const c_char;
    fn SSL_get_version(ssl: *const SSL) -> *const c_char;
    fn SSL_free(ssl: *mut SSL);
    fn SSL_CTX_free(ctx: *mut SSL_CTX);

    fn BIO_new(method: *const BIO_METHOD) -> *mut BIO;
    fn BIO_free(bio: *mut BIO) -> c_int;
    fn BIO_s_mem() -> *const BIO_METHOD;
    fn BIO_read(bio: *mut BIO, buf: *mut c_void, len: c_int) -> c_int;
    fn BIO_write(bio: *mut BIO, buf: *const c_void, len: c_int) -> c_int;
    fn BIO_ctrl(bio: *mut BIO, cmd: c_int, larg: c_long, parg: *mut c_void) -> c_long;
}

const BIO_CTRL_PENDING: c_int = 10;
unsafe fn bio_pending(bio: *mut BIO) -> usize {
    BIO_ctrl(bio, BIO_CTRL_PENDING, 0, ptr::null_mut()) as usize
}

pub enum HsStatus {
    Done,
    WantRead,
}

pub struct HeadUnitTls {
    ctx: *mut SSL_CTX,
    ssl: *mut SSL,
    rbio: *mut BIO, // inbound: we BIO_write ciphertext here, SSL reads it
    wbio: *mut BIO, // outbound: SSL writes ciphertext here, we BIO_read it
    // keep the cert/key alive for the lifetime of the context
    _cert: X509,
    _key: PKey<openssl::pkey::Private>,
}

impl HeadUnitTls {
    pub fn new(cert_pem: &str, key_pem: &str) -> Result<Self, String> {
        let cert = X509::from_pem(cert_pem.as_bytes()).map_err(|e| format!("cert parse: {e}"))?;
        let key = PKey::private_key_from_pem(key_pem.as_bytes()).map_err(|e| format!("key parse: {e}"))?;
        unsafe {
            let ctx = SSL_CTX_new(TLS_client_method());
            if ctx.is_null() {
                return Err("SSL_CTX_new failed".into());
            }
            if SSL_CTX_use_certificate(ctx, cert.as_ptr() as *mut X509_) != 1 {
                SSL_CTX_free(ctx);
                return Err("SSL_CTX_use_certificate failed".into());
            }
            if SSL_CTX_use_PrivateKey(ctx, key.as_ptr() as *mut EVP_PKEY) != 1 {
                SSL_CTX_free(ctx);
                return Err("SSL_CTX_use_PrivateKey failed".into());
            }
            SSL_CTX_set_verify(ctx, SSL_VERIFY_NONE, ptr::null());
            // Pin TLS 1.2: aasdk's head unit uses TLSv1_2_client_method, and the
            // AA cipher is ECDHE-RSA-AES128-GCM-SHA256 (TLS 1.2). Without this,
            // OpenSSL 3.x negotiates TLS 1.3, whose post-handshake NewSessionTicket
            // record breaks the single-record decrypt path.
            const SSL_CTRL_SET_MIN_PROTO_VERSION: c_int = 123;
            const SSL_CTRL_SET_MAX_PROTO_VERSION: c_int = 124;
            SSL_CTX_ctrl(ctx, SSL_CTRL_SET_MIN_PROTO_VERSION, TLS1_2_VERSION as c_long, ptr::null_mut());
            SSL_CTX_ctrl(ctx, SSL_CTRL_SET_MAX_PROTO_VERSION, TLS1_2_VERSION as c_long, ptr::null_mut());

            let ssl = SSL_new(ctx);
            if ssl.is_null() {
                SSL_CTX_free(ctx);
                return Err("SSL_new failed".into());
            }
            let rbio = BIO_new(BIO_s_mem());
            let wbio = BIO_new(BIO_s_mem());
            if rbio.is_null() || wbio.is_null() {
                // SSL_set_bio hasn't run yet, so SSL_free won't free these — do it here.
                if !rbio.is_null() {
                    BIO_free(rbio);
                }
                if !wbio.is_null() {
                    BIO_free(wbio);
                }
                SSL_free(ssl);
                SSL_CTX_free(ctx);
                return Err("BIO_new failed".into());
            }
            SSL_set_bio(ssl, rbio, wbio); // SSL takes ownership of both BIOs
            SSL_set_connect_state(ssl); // head unit is the TLS client

            Ok(Self { ctx, ssl, rbio, wbio, _cert: cert, _key: key })
        }
    }

    /// Run one SSL_do_handshake step. Caller must drain outbound after this and,
    /// on WantRead, feed one inbound record then call again.
    pub fn advance_handshake(&mut self) -> Result<HsStatus, String> {
        unsafe {
            let ret = SSL_do_handshake(self.ssl);
            if ret == 1 {
                return Ok(HsStatus::Done);
            }
            let err = SSL_get_error(self.ssl, ret);
            match err {
                SSL_ERROR_WANT_READ | SSL_ERROR_WANT_WRITE => Ok(HsStatus::WantRead),
                _ => Err(format!("SSL_do_handshake error code {err}: {}", openssl_errs())),
            }
        }
    }

    /// Pull the next chunk of ciphertext the SSL engine has queued to send.
    /// Returns None when the outbound BIO is empty.
    pub fn take_outbound(&mut self) -> Option<Vec<u8>> {
        unsafe {
            let pending = bio_pending(self.wbio);
            if pending == 0 {
                return None;
            }
            let mut buf = vec![0u8; pending];
            let n = BIO_read(self.wbio, buf.as_mut_ptr() as *mut c_void, pending as c_int);
            if n <= 0 {
                return None;
            }
            buf.truncate(n as usize);
            Some(buf)
        }
    }

    /// Feed one inbound ciphertext record (the body of an ENCAPSULATED_SSL frame).
    pub fn feed_inbound(&mut self, data: &[u8]) {
        unsafe {
            let mut off = 0;
            while off < data.len() {
                let n = BIO_write(
                    self.rbio,
                    data[off..].as_ptr() as *const c_void,
                    (data.len() - off) as c_int,
                );
                if n <= 0 {
                    break;
                }
                off += n as usize;
            }
        }
    }

    /// Encrypt a full plaintext message ([messageId||protobuf]); returns the TLS
    /// record bytes to put in an ENCRYPTED frame payload.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, String> {
        unsafe {
            let n = SSL_write(self.ssl, plaintext.as_ptr() as *const c_void, plaintext.len() as c_int);
            if n as usize != plaintext.len() {
                return Err(format!("SSL_write wrote {n}/{}: {}", plaintext.len(), openssl_errs()));
            }
            let mut out = Vec::new();
            while let Some(chunk) = self.take_outbound() {
                out.extend_from_slice(&chunk);
            }
            Ok(out)
        }
    }

    /// Decrypt an ENCRYPTED frame payload back to [messageId||protobuf].
    /// Drains all application-data records the fed ciphertext yields; a
    /// WANT_READ/WANT_WRITE after at least one record simply means "no more
    /// data right now" (mirrors aasdk's Cryptor::decrypt), not an error.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, String> {
        self.feed_inbound(ciphertext);
        let mut out = Vec::new();
        unsafe {
            loop {
                let mut buf = vec![0u8; 16 * 1024];
                let n = SSL_read(self.ssl, buf.as_mut_ptr() as *mut c_void, buf.len() as c_int);
                if n > 0 {
                    out.extend_from_slice(&buf[..n as usize]);
                    continue;
                }
                let err = SSL_get_error(self.ssl, n);
                if err == SSL_ERROR_WANT_READ || err == SSL_ERROR_WANT_WRITE {
                    break;
                }
                return Err(format!("SSL_read returned {n} (err {err}): {}", openssl_errs()));
            }
        }
        if out.is_empty() {
            return Err("decrypt produced no plaintext (need more ciphertext)".into());
        }
        Ok(out)
    }

    pub fn describe(&self) -> String {
        unsafe {
            let ver = cstr(SSL_get_version(self.ssl));
            let cipher = SSL_get_current_cipher(self.ssl);
            let name = if cipher.is_null() { "<none>".to_string() } else { cstr(SSL_CIPHER_get_name(cipher)) };
            format!("{ver} / {name}")
        }
    }

    /// LOG-ONLY, no verification-policy effect: reads the phone's presented leaf certificate
    /// (peer verification stays `SSL_VERIFY_NONE`, set in `new()` above — this never fails the
    /// handshake) and logs its subject + issuer, warning if the issuer is not Google Automotive
    /// Link. Uses `openssl_sys::SSL_get1_peer_certificate` (the up-reffing OpenSSL 3 accessor;
    /// the linked OpenSSL here is 3.6.4) rather than a hand-written extern, since `openssl-sys`
    /// is already a declared dependency of this crate and was otherwise unused.
    pub fn log_peer_certificate_issuer(&self) {
        unsafe {
            let raw = openssl_sys::SSL_get1_peer_certificate(self.ssl as *const openssl_sys::SSL);
            if raw.is_null() {
                eprintln!("[aa-headunit] peer cert: SSL_get1_peer_certificate returned null");
                return;
            }
            // Takes ownership of `raw`; freed (X509_free) when `cert` drops at the end of this fn.
            let cert = X509::from_ptr(raw);
            let subject = name_oneline(cert.subject_name());
            let issuer = name_oneline(cert.issuer_name());
            if issuer.contains("Google Automotive Link") {
                eprintln!("[aa-headunit] peer cert subject=\"{subject}\" issuer=GAL-issued (Google Automotive Link)");
            } else {
                eprintln!(
                    "[aa-headunit] WARNING peer cert subject=\"{subject}\" issuer=\"{issuer}\" \
                     does not contain \"Google Automotive Link\""
                );
            }
        }
    }
}

/// `subject=value, subject=value, ...` rendering of an X509 name, for the log-only peer-issuer
/// check above. Never renders certificate bytes — only the parsed RDN short names and values.
fn name_oneline(name: &X509NameRef) -> String {
    name.entries()
        .filter_map(|e| e.data().to_string().ok().map(|v| format!("{}={}", e.object(), v)))
        .collect::<Vec<_>>()
        .join(", ")
}

impl Drop for HeadUnitTls {
    fn drop(&mut self) {
        unsafe {
            if !self.ssl.is_null() {
                SSL_free(self.ssl); // also frees the two BIOs it owns
            }
            if !self.ctx.is_null() {
                SSL_CTX_free(self.ctx);
            }
        }
    }
}

unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return "<null>".into();
    }
    std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
}

/// Collect queued OpenSSL error strings via the high-level crate's error stack.
fn openssl_errs() -> String {
    let e = openssl::error::ErrorStack::get();
    if e.errors().is_empty() {
        "no openssl error on stack".into()
    } else {
        e.to_string()
    }
}
