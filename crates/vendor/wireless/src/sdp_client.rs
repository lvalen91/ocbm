//! Minimal, `bluetoothd`-less SDP *client* for Model-B accessory-initiated reconnect (docs/wireless/01_BT_AND_RADIO.md).
//!
//! On reconnect the box must discover which RFCOMM channel the bonded iPhone exposes its iAP2 service
//! on before it can `rfcomm::connect_to` it — the box has no `sdptool`. This is the symmetric peer of
//! `sdp_server.rs`: it L2CAP-connects to the phone's well-known SDP PSM 0x0001 (which in BlueZ
//! implicitly pages the phone, bringing the ACL up), sends a `ServiceSearchAttributeRequest` for the
//! iAP2 UUID, reassembles any continuation chunks, and pulls the RFCOMM channel out of the returned
//! `ProtocolDescriptorList`.
//!
//! Open unknown this module exists to ANSWER (docs/wireless/01_BT_AND_RADIO.md): whether iOS exposes an iAP2/EA RFCOMM service
//! at all on reconnect, and on what channel. So it always logs the raw response hex — if the channel
//! scan fails, the capture tells the next session exactly what iOS returned.

use std::io::{Read, Write};
use std::os::fd::FromRawFd;

const AF_BLUETOOTH: libc::sa_family_t = 31;
const BTPROTO_L2CAP: libc::c_int = 0;
const SDP_PSM: u16 = 0x0001;

const SDP_ERROR_RSP: u8 = 0x01;
const SDP_SVC_SEARCH_ATTR_REQ: u8 = 0x06;
const SDP_SVC_SEARCH_ATTR_RSP: u8 = 0x07;

/// The PHONE-side iAP2 service UUID128 (`02030302-1d19-415f-86f2-22a2106a0a77`), named
/// "Wireless iAP v2" in iOS's SDP catalog — device-confirmed on 2026-08-01 by browsing the phone's
/// full record set (docs/wireless/01_BT_AND_RADIO.md). This is NOT the accessory-side UUID `sdp_server.rs` advertises
/// (`…decacaff`); the phone advertises its own iAP2 endpoint under this distinct UUID on RFCOMM ch 1.
/// (iOS also exposes "Wireless iAP5" under `…decacafe`, one byte off from the accessory UUID — a red
/// herring the first probe fell into.)
const IAP2_UUID128: [u8; 16] = [
    0x02, 0x03, 0x03, 0x02, 0x1d, 0x19, 0x41, 0x5f, 0x86, 0xf2, 0x22, 0xa2, 0x10, 0x6a, 0x0a, 0x77,
];

#[repr(C)]
#[derive(Clone, Copy)]
struct SockaddrL2 {
    l2_family: libc::sa_family_t,
    l2_psm: u16, // little-endian on the wire; this box is little-endian, so native == LE
    l2_bdaddr: [u8; 6],
    l2_cid: u16,
    l2_bdaddr_type: u8,
}

fn log(m: &str) {
    println!("[sdp-client] {m}");
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join("")
}

/// L2CAP-connect to `peer`'s SDP PSM (implicitly pages the phone in BlueZ). `timeout_secs` bounds both
/// the connect and each subsequent read so a quiet phone can't wedge the reconnect thread.
fn connect_sdp(peer: [u8; 6], timeout_secs: i64) -> std::io::Result<std::fs::File> {
    let fd = unsafe {
        libc::socket(
            AF_BLUETOOTH as libc::c_int,
            libc::SOCK_SEQPACKET | crate::cloexec::SOCK_CLOEXEC,
            BTPROTO_L2CAP,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // tv_sec's width is target-dependent (i32 on armv7-musl, i64 on the dev host); `as _` infers the
    // field type from the struct literal so both targets build without naming the deprecated time_t alias.
    // zeroed()+assign, not a struct literal: under `musl32_time64` (riscv32) these
    // types carry private padding and a literal does not compile.
    let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
    tv.tv_sec = timeout_secs as _;
    for opt in [libc::SO_SNDTIMEO, libc::SO_RCVTIMEO] {
        let ret = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                opt,
                &tv as *const libc::timeval as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        };
        if ret < 0 {
            let e = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(e);
        }
    }
    let addr = SockaddrL2 {
        l2_family: AF_BLUETOOTH,
        l2_psm: SDP_PSM,
        l2_bdaddr: peer,
        l2_cid: 0,
        l2_bdaddr_type: 0,
    };
    let ret = unsafe {
        libc::connect(
            fd,
            &addr as *const SockaddrL2 as *const libc::sockaddr,
            std::mem::size_of::<SockaddrL2>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // SAFETY: fd is a freshly opened, connected, exclusively-owned socket descriptor.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// The iAP2 UUID128 search pattern: `DES { UUID128 iAP2 }`.
fn search_pattern_iap2() -> Vec<u8> {
    let mut p = Vec::with_capacity(19);
    p.push(0x35); // DES, 1-byte length follows
    p.push(0x11); // length = 17 (1 desc byte + 16 UUID bytes)
    p.push(0x1c); // UUID128 descriptor
    p.extend_from_slice(&IAP2_UUID128);
    p
}

/// The L2CAP UUID16 (0x0100) search pattern: `DES { UUID16 0x0100 }`. Every connectable BR/EDR
/// service's ProtocolDescriptorList contains L2CAP, so this matches essentially ALL of the peer's
/// SDP records — a browse-all, to enumerate what iOS actually exposes (docs/wireless/01_BT_AND_RADIO.md open unknown).
fn search_pattern_l2cap() -> Vec<u8> {
    vec![0x35, 0x03, 0x19, 0x01, 0x00]
}

/// Build a `ServiceSearchAttributeRequest` param block for `search_pattern`: max attr bytes 0xFFFF,
/// the full 0x0000..0xFFFF attribute range, and `cont` as the trailing continuation state.
fn build_ssa_request(search_pattern: &[u8], cont: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(search_pattern.len() + 12);
    p.extend_from_slice(search_pattern);
    // MaximumAttributeByteCount
    p.extend_from_slice(&0xFFFFu16.to_be_bytes());
    // AttributeIDList: DES { uint32 range 0x0000_FFFF }
    p.push(0x35);
    p.push(0x05);
    p.push(0x0a); // uint32 descriptor
    p.extend_from_slice(&0x0000_FFFFu32.to_be_bytes());
    // ContinuationState
    p.extend_from_slice(cont);
    p
}

/// Run one `ServiceSearchAttributeRequest` transaction over `sock` for `search_pattern`, reassembling
/// any continuation chunks. Returns the assembled attribute-list blob (may be empty `35 00`).
fn run_search(sock: &mut std::fs::File, search_pattern: &[u8], first_tid: u16) -> std::io::Result<Vec<u8>> {
    let mut attr_lists: Vec<u8> = Vec::new();
    let mut cont: Vec<u8> = vec![0x00];
    let mut tid = first_tid;
    let mut buf = [0u8; 4096];
    // Bound the continuation reassembly (audit B5): a peer returning endless non-zero continuation cookies
    // (or a zero-progress `count==0` each round) must not loop forever or grow `attr_lists` unbounded — the
    // one reassembly path in the crate that previously had no ceiling (contrast bt_driver's MAX_REASSEMBLY).
    const MAX_ROUNDS: usize = 64;
    const MAX_ATTR_BYTES: usize = 64 * 1024;
    let mut rounds = 0usize;
    loop {
        rounds += 1;
        if rounds > MAX_ROUNDS || attr_lists.len() > MAX_ATTR_BYTES {
            log(&format!("SDP reassembly exceeded bound (rounds={rounds}, bytes={}) — stopping", attr_lists.len()));
            break;
        }
        let params = build_ssa_request(search_pattern, &cont);
        let mut pdu = Vec::with_capacity(5 + params.len());
        pdu.push(SDP_SVC_SEARCH_ATTR_REQ);
        pdu.extend_from_slice(&tid.to_be_bytes());
        pdu.extend_from_slice(&(params.len() as u16).to_be_bytes());
        pdu.extend_from_slice(&params);
        sock.write_all(&pdu)?;

        let n = sock.read(&mut buf)?;
        if n < 5 {
            log(&format!("runt SDP response ({n} bytes)"));
            break;
        }
        let rpdu = buf[0];
        let plen = u16::from_be_bytes([buf[3], buf[4]]) as usize;
        if rpdu == SDP_ERROR_RSP {
            log(&format!("phone returned SDP_ERROR_RSP ({})", hex(&buf[5..n.min(5 + plen)])));
            break;
        }
        if rpdu != SDP_SVC_SEARCH_ATTR_RSP || 5 + plen > n {
            log(&format!("unexpected SDP response pdu=0x{rpdu:02x} plen={plen} n={n}"));
            break;
        }
        let params = &buf[5..5 + plen];
        if params.len() < 2 {
            break;
        }
        let count = u16::from_be_bytes([params[0], params[1]]) as usize;
        if 2 + count > params.len() {
            log("SDP response chunk length overruns PDU");
            break;
        }
        attr_lists.extend_from_slice(&params[2..2 + count]);
        let cs = &params[2 + count..];
        match cs.first() {
            Some(0) | None => break,
            Some(&len) => {
                let want = 1 + len as usize;
                if cs.len() < want {
                    log("truncated continuation state");
                    break;
                }
                cont = cs[..want].to_vec();
                tid = tid.wrapping_add(1);
            }
        }
    }
    Ok(attr_lists)
}

/// Scan an attribute-list blob for an RFCOMM `ProtocolDescriptorList` entry and return its channel.
/// The canonical encoding of "RFCOMM, channel N" is `19 00 03  08 NN` (UUID16 0x0003 = RFCOMM, then
/// a uint8 = the channel) — byte-identical to what `sdp_server.rs` itself emits (line ~62), so this
/// is the spec encoding, not a heuristic. Returns the first channel found.
fn scan_rfcomm_channel(blob: &[u8]) -> Option<u8> {
    let mut i = 0;
    while i + 4 < blob.len() {
        if blob[i] == 0x19 && blob[i + 1] == 0x00 && blob[i + 2] == 0x03 && blob[i + 3] == 0x08 {
            return Some(blob[i + 4]);
        }
        i += 1;
    }
    None
}

/// Discover the iAP2 RFCOMM channel the bonded `peer` exposes. `Ok(Some(ch))` on success, `Ok(None)`
/// if the phone answered but exposes no iAP2 RFCOMM service (raw response logged for diagnosis),
/// `Err` on connect/transport failure. Handles SDP continuation by re-requesting with the cookie.
pub fn query(peer: [u8; 6], timeout_secs: i64) -> std::io::Result<Option<u8>> {
    let mut disp = peer;
    disp.reverse(); // human-readable bdaddr for the log only
    let human = disp.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":");
    log(&format!("querying SDP on {human} (PSM 0x{SDP_PSM:04x})"));
    let mut sock = connect_sdp(peer, timeout_secs)?;
    log("L2CAP SDP channel up (phone paged)");

    // (1) Targeted: the accessory-side iAP2 UUID. If iOS exposes an iAP2 RFCOMM service by this UUID,
    //     this pulls its channel directly.
    let iap2 = run_search(&mut sock, &search_pattern_iap2(), 1)?;
    log(&format!("iAP2-UUID search -> {} bytes: {}", iap2.len(), hex(&iap2)));
    if let Some(ch) = scan_rfcomm_channel(&iap2) {
        log(&format!("iAP2 RFCOMM channel on the phone = {ch}"));
        return Ok(Some(ch));
    }

    // (2) Browse-all (L2CAP UUID): dump iOS's ENTIRE SDP catalog so we can see what it DOES expose —
    //     the decisive diagnostic for docs/wireless/01_BT_AND_RADIO.md's open unknown. Every RFCOMM channel found is logged.
    let all = run_search(&mut sock, &search_pattern_l2cap(), 100)?;
    log(&format!("browse-all (L2CAP) -> {} bytes: {}", all.len(), hex(&all)));
    let mut chans = Vec::new();
    let mut i = 0;
    while i + 4 < all.len() {
        if all[i] == 0x19 && all[i + 1] == 0x00 && all[i + 2] == 0x03 && all[i + 3] == 0x08 {
            chans.push(all[i + 4]);
            i += 5;
        } else {
            i += 1;
        }
    }
    if chans.is_empty() {
        log("browse-all: iOS exposes NO RFCOMM service on BR/EDR — the accessory cannot connect OUT (redirects Model B; see docs/wireless/01_BT_AND_RADIO.md)");
    } else {
        log(&format!(
            "browse-all: iOS exposes RFCOMM channel(s) {chans:?} — but none under the iAP2 UUID; raw catalog above identifies the service UUIDs"
        ));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssa_request_has_iap2_uuid_and_range() {
        let req = build_ssa_request(&search_pattern_iap2(), &[0x00]);
        // DES header + UUID128 descriptor + the 16 UUID bytes
        assert_eq!(&req[0..3], &[0x35, 0x11, 0x1c]);
        assert_eq!(&req[3..19], &IAP2_UUID128);
        // MaxAttributeByteCount 0xFFFF
        assert_eq!(&req[19..21], &[0xff, 0xff]);
        // AttributeIDList: DES uint32 0x0000FFFF
        assert_eq!(&req[21..28], &[0x35, 0x05, 0x0a, 0x00, 0x00, 0xff, 0xff]);
        // trailing continuation
        assert_eq!(req[28], 0x00);
    }

    #[test]
    fn l2cap_browse_pattern_is_uuid16_0100() {
        assert_eq!(search_pattern_l2cap(), vec![0x35, 0x03, 0x19, 0x01, 0x00]);
    }

    #[test]
    fn scan_finds_rfcomm_channel() {
        // The canonical RFCOMM descriptor as sdp_server emits it: 19 00 03 08 NN
        let blob = [0x35, 0x05, 0x19, 0x00, 0x03, 0x08, 0x07];
        assert_eq!(scan_rfcomm_channel(&blob), Some(0x07));
    }

    #[test]
    fn scan_returns_none_without_rfcomm() {
        // L2CAP-only descriptor (UUID 0x0100), no RFCOMM
        let blob = [0x35, 0x03, 0x19, 0x01, 0x00];
        assert_eq!(scan_rfcomm_channel(&blob), None);
    }

    #[test]
    fn scan_picks_channel_after_full_pdl() {
        // Mirrors sdp_server's ProtocolDescriptorList: L2CAP then RFCOMM ch 1
        let blob = [
            0x35, 0x0c, 0x35, 0x03, 0x19, 0x01, 0x00, 0x35, 0x05, 0x19, 0x00, 0x03, 0x08, 0x01,
        ];
        assert_eq!(scan_rfcomm_channel(&blob), Some(1));
    }
}
