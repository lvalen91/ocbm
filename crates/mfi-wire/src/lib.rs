//! mfi-wire — framing shared by the ephemeral MFi test daemon (`mfid`) and its clients.
//!
//! This exists because the NCM bring-up path needs the two MFi chip operations reachable over a
//! socket, and `crates/vendor/mfi/src/auth_client.rs` records that the previous TCP client to
//! Carlinkit's `ncm_carplayd` auth service was deleted (audit Fix #19) when production MFi became
//! local-only. This is the minimum replacement, scoped to bring-up.
//!
//! Frame layout, identical in both directions:
//!
//! ```text
//!   0       4       5                       9
//!   +-------+-------+-----------------------+------------------+
//!   | "MFI1"| code  | payload len (u32 BE)  | payload          |
//!   +-------+-------+-----------------------+------------------+
//! ```
//!
//! `code` is an [`Op`] on a request and a [`Status`] on a response. Keeping one frame shape for
//! both directions is what lets the daemon and the probe share this file rather than drifting.

#![forbid(unsafe_code)]

use std::io::{self, Read, Write};

pub const MAGIC: [u8; 4] = *b"MFI1";
pub const HEADER_LEN: usize = 9;

/// Upper bound on a payload in either direction. Real traffic is a 20-byte SHA-1 digest up and a
/// ~945-byte MFi certificate down, so this is generous while still bounding what a malformed or
/// hostile length field can make us allocate.
pub const MAX_PAYLOAD: usize = 4096;

/// The digest an MFi `create_signature` takes — SHA-1, always exactly 20 bytes. The daemon rejects
/// anything else rather than handing the chip a short buffer.
pub const DIGEST_LEN: usize = 20;

/// Default port. Arbitrary and unregistered; the daemon is bring-up-only and binds the USB-NCM link.
pub const DEFAULT_PORT: u16 = 7789;

/// A request opcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Liveness only — never touches the chip, so it is safe to poll while a session is running.
    Ping = 0x01,
    /// `copy_certificate` — the accessory's MFi certificate (DER).
    Cert = 0x02,
    /// `create_signature` over a [`DIGEST_LEN`]-byte digest.
    Sign = 0x03,
}

impl Op {
    pub fn from_u8(v: u8) -> Option<Op> {
        match v {
            0x01 => Some(Op::Ping),
            0x02 => Some(Op::Cert),
            0x03 => Some(Op::Sign),
            _ => None,
        }
    }
}

/// A response status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok = 0x00,
    /// `/tmp/carplay_mfi.lock` was held past its deadline by one of the other chip users. Per
    /// `mfi-i2c-local`'s own guidance this must NOT be retried in a tight loop — the wait cannot
    /// succeed sooner the second time, and retrying only multiplies the stall.
    LockBusy = 0x01,
    /// The chip NAKed or timed out. Retrying is reasonable.
    Chip = 0x02,
    /// Malformed request — bad opcode payload, or a digest that was not [`DIGEST_LEN`] bytes.
    BadRequest = 0x03,
    /// Well-formed frame carrying an opcode this daemon does not implement.
    Unsupported = 0x04,
}

impl Status {
    pub fn from_u8(v: u8) -> Option<Status> {
        match v {
            0x00 => Some(Status::Ok),
            0x01 => Some(Status::LockBusy),
            0x02 => Some(Status::Chip),
            0x03 => Some(Status::BadRequest),
            0x04 => Some(Status::Unsupported),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::LockBusy => "lock-busy",
            Status::Chip => "chip-error",
            Status::BadRequest => "bad-request",
            Status::Unsupported => "unsupported",
        }
    }
}

/// Serialize one frame. Returns `InvalidInput` rather than truncating an oversized payload.
pub fn write_frame<W: Write>(w: &mut W, code: u8, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "payload exceeds MAX_PAYLOAD",
        ));
    }
    let mut buf = Vec::with_capacity(HEADER_LEN + payload.len());
    buf.extend_from_slice(&MAGIC);
    buf.push(code);
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(payload);
    // One write for the whole frame: the box's TCP stack is fine either way, but a single syscall
    // keeps the wire trace readable when this is captured next to an iAP2 dump.
    w.write_all(&buf)
}

/// Read one frame. A clean peer close surfaces as [`io::ErrorKind::UnexpectedEof`], which callers
/// treat as end-of-connection rather than as an error worth logging.
pub fn read_frame<R: Read>(r: &mut R) -> io::Result<(u8, Vec<u8>)> {
    let mut hdr = [0u8; HEADER_LEN];
    r.read_exact(&mut hdr)?;
    if hdr[0..4] != MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad magic"));
    }
    let code = hdr[4];
    let len = u32::from_be_bytes([hdr[5], hdr[6], hdr[7], hdr[8]]) as usize;
    if len > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "declared payload exceeds MAX_PAYLOAD",
        ));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok((code, payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn roundtrip_empty_and_payload() {
        for payload in [vec![], vec![0xAAu8; 20], vec![0x5Au8; 945]] {
            let mut buf = Vec::new();
            write_frame(&mut buf, Op::Sign as u8, &payload).unwrap();
            let (code, got) = read_frame(&mut Cursor::new(&buf)).unwrap();
            assert_eq!(code, Op::Sign as u8);
            assert_eq!(got, payload);
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = Vec::new();
        write_frame(&mut buf, Op::Ping as u8, &[]).unwrap();
        buf[0] = b'X';
        let err = read_frame(&mut Cursor::new(&buf)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    /// A hostile length field must be refused from the header alone — before any allocation.
    #[test]
    fn rejects_oversized_declared_length() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC);
        buf.push(Op::Cert as u8);
        buf.extend_from_slice(&(u32::MAX).to_be_bytes());
        let err = read_frame(&mut Cursor::new(&buf)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn rejects_oversized_write() {
        let mut buf = Vec::new();
        let err = write_frame(&mut buf, Op::Cert as u8, &vec![0u8; MAX_PAYLOAD + 1]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// A closed connection surfaces as EOF rather than as corruption, so an ordinary client
    /// disconnect is not logged as a protocol error. Note this does NOT distinguish a clean close
    /// from a peer that closed mid-frame (see `truncated_payload_is_eof`) — both are EOF and both
    /// end the connection, which is why the daemon treats them identically.
    #[test]
    fn clean_close_is_eof() {
        let err = read_frame(&mut Cursor::new(Vec::new())).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn truncated_payload_is_eof() {
        let mut buf = Vec::new();
        write_frame(&mut buf, Op::Sign as u8, &[1, 2, 3, 4]).unwrap();
        buf.truncate(HEADER_LEN + 2);
        let err = read_frame(&mut Cursor::new(&buf)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    /// The property the daemon's multi-request loop depends on: `read_frame` consumes EXACTLY one
    /// frame and leaves the stream positioned on the next magic. Without this, a second request on
    /// the same connection would desynchronize.
    #[test]
    fn consecutive_frames_on_one_stream() {
        let mut buf = Vec::new();
        write_frame(&mut buf, Op::Ping as u8, &[]).unwrap();
        write_frame(&mut buf, Op::Sign as u8, &[0xAB; DIGEST_LEN]).unwrap();
        write_frame(&mut buf, Op::Cert as u8, &[0x01, 0x02]).unwrap();

        let mut cur = Cursor::new(&buf);
        assert_eq!(read_frame(&mut cur).unwrap(), (Op::Ping as u8, vec![]));
        assert_eq!(
            read_frame(&mut cur).unwrap(),
            (Op::Sign as u8, vec![0xAB; DIGEST_LEN])
        );
        assert_eq!(
            read_frame(&mut cur).unwrap(),
            (Op::Cert as u8, vec![0x01, 0x02])
        );
        assert_eq!(
            read_frame(&mut cur).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    /// Exactly `MAX_PAYLOAD` is legal; one byte more is refused from the header alone.
    #[test]
    fn max_payload_boundary() {
        let mut buf = Vec::new();
        write_frame(&mut buf, Op::Cert as u8, &vec![0u8; MAX_PAYLOAD]).unwrap();
        let (_, got) = read_frame(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(got.len(), MAX_PAYLOAD);

        let mut over = Vec::new();
        over.extend_from_slice(&MAGIC);
        over.push(Op::Cert as u8);
        over.extend_from_slice(&((MAX_PAYLOAD + 1) as u32).to_be_bytes());
        assert_eq!(
            read_frame(&mut Cursor::new(&over)).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn truncated_header_is_eof() {
        let mut buf = Vec::new();
        write_frame(&mut buf, Op::Cert as u8, &[]).unwrap();
        buf.truncate(5);
        assert_eq!(
            read_frame(&mut Cursor::new(&buf)).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn code_conversions() {
        assert_eq!(Op::from_u8(0x02), Some(Op::Cert));
        assert_eq!(Op::from_u8(0x7F), None);
        assert_eq!(Status::from_u8(0x01), Some(Status::LockBusy));
        assert_eq!(Status::from_u8(0x7F), None);
    }
}

/// Blocking client for a remote MFi service (`mfid`).
///
/// Lives here rather than in each caller because there are now two independent consumers on the
/// Raspberry Pi port — `carplay-wireless` (BT-time iAP2 auth) and `airplayd` (`auth-setup` during
/// the AirPlay session) — and a second hand-rolled copy of the framing is exactly how the two drift.
///
/// Deliberately blocking and synchronous: `sign` is called inside handshakes that run against the
/// phone's own timeout, so no async boundary may be introduced here.
pub mod client {
    use super::{read_frame, write_frame, Op, Status, DIGEST_LEN};
    use std::io::{self, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;

    const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
    /// Generous on purpose: the daemon bounds a contended chip lock at 10 s and the sign path itself
    /// measures ~1.5 s over USB-NCM. A tighter timeout would report false failures.
    ///
    /// DO NOT install this crate as the iAP2 tunnel's remote signer on a vehicle path. Combined with
    /// CONNECT_TIMEOUT that is a ~35 s worst case, and `iap_tunnel::handle_inbound` holds the
    /// module-wide `SESSION` mutex across the chip call — so the RCS reader, the event handler,
    /// `POST /command` and teardown all wait on it, none of them with a deadline of their own. That
    /// is survivable HERE because this crate is a bench/NCM bring-up tool, not a live projection
    /// session. The vehicle path (gm_ccpa's OCBM `CH_MFI` relay) uses a 4 s total budget instead.
    /// See `receiver::iap_tunnel::set_remote_signer`, which spells out the calibration.
    const IO_TIMEOUT: Duration = Duration::from_secs(30);

    fn exchange(addr: &str, op: Op, payload: &[u8]) -> io::Result<Vec<u8>> {
        let sa = addr
            .to_socket_addrs()
            .ok()
            .and_then(|mut a| a.next())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, format!("cannot resolve {addr}"))
            })?;
        let mut s = TcpStream::connect_timeout(&sa, CONNECT_TIMEOUT)?;
        let _ = s.set_read_timeout(Some(IO_TIMEOUT));
        let _ = s.set_write_timeout(Some(IO_TIMEOUT));
        let _ = s.set_nodelay(true);
        write_frame(&mut s, op as u8, payload)?;
        s.flush()?;
        let (code, body) = read_frame(&mut s)?;
        match Status::from_u8(code) {
            Some(Status::Ok) => Ok(body),
            Some(st) => Err(io::Error::other(format!("mfid returned {}", st.as_str()))),
            None => Err(io::Error::other(format!("mfid returned unknown status 0x{code:02x}"))),
        }
    }

    /// Fetch the accessory's MFi certificate (DER).
    pub fn cert(addr: &str) -> io::Result<Vec<u8>> {
        exchange(addr, Op::Cert, &[])
    }

    /// Sign a [`DIGEST_LEN`]-byte SHA-1 digest. Rejects a wrong-length digest locally rather than
    /// letting the coprocessor answer with an opaque NAK.
    pub fn sign(addr: &str, digest: &[u8]) -> io::Result<Vec<u8>> {
        if digest.len() != DIGEST_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("digest is {} bytes, expected {DIGEST_LEN}", digest.len()),
            ));
        }
        exchange(addr, Op::Sign, digest)
    }

    /// The env var selecting a remote coprocessor, shared by every consumer so they cannot disagree.
    pub const ADDR_ENV: &str = "CARPLAY_MFI_ADDR";

    /// Resolved ONCE per process, matching the project's env-lever convention.
    pub fn addr() -> Option<&'static str> {
        static ADDR: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        ADDR.get_or_init(|| match std::env::var(ADDR_ENV) {
            Ok(v) if !v.trim().is_empty() => {
                let v = v.trim().to_string();
                eprintln!("[mfi] REMOTE coprocessor {v} — local /dev/i2c-1 will NOT be used");
                Some(v)
            }
            _ => None,
        })
        .as_deref()
    }
}
