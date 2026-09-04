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

/// The Android Auto wireless-projection UUID128, `4de17a00-52cb-11e6-bdf4-0800200c9a66`.
///
/// **DIAGNOSTIC SEARCH ONLY — CORRECTED 2026-09-04 (second pass).** An earlier reading had the
/// PHONE hosting this service and the head unit dialling it. That was wrong in the direction that
/// matters: gearhead is the CLIENT of this UUID
/// (`createRfcommSocketToServiceRecord(4de17a00-…)`, `ojk.java:31-35`) and never registers a
/// server for it, and the bench Pixel's SDP has no such record at all
/// (`AA-wireless-UUID search -> 2 bytes: 3500`, i.e. an empty attribute list). The record it dials
/// is OURS — `sdp_server::android_auto_service` on channel 4 — once the wireless-setup gate opens.
///
/// The search is kept because the empty answer IS the evidence, and because a peer that DID host it
/// would be worth knowing about. `reconnect` does not act on the result.
const AAWG_UUID128: [u8; 16] = [
    0x4d, 0xe1, 0x7a, 0x00, 0x52, 0xcb, 0x11, 0xe6, 0xbd, 0xf4, 0x08, 0x00, 0x20, 0x0c, 0x9a, 0x66,
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

/// `@<unix_ms> ` write-time stamp (docs/carplay/01_OCBM_PROTOCOL.md CH_LOG): the box.log tailer
/// parses this prefix and uses it instead of the millisecond it happened to READ the line at.
fn log(m: &str) {
    println!("@{} [sdp-client] {m}", now_ms());
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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

/// The Android Auto wireless-projection UUID128 search pattern: `DES { UUID128 AAWG }`. Same shape
/// as [`search_pattern_iap2`] — the only difference is which 16 bytes go in.
fn search_pattern_aawg() -> Vec<u8> {
    let mut p = Vec::with_capacity(19);
    p.push(0x35); // DES, 1-byte length follows
    p.push(0x11); // length = 17 (1 desc byte + 16 UUID bytes)
    p.push(0x1c); // UUID128 descriptor
    p.extend_from_slice(&AAWG_UUID128);
    p
}

/// A UUID16 search pattern: `DES { UUID16 v }`. Used for the audio-gateway searches, which are
/// 16-bit UUIDs rather than the 128-bit vendor ones above.
fn search_pattern_uuid16(v: u16) -> Vec<u8> {
    let mut p = Vec::with_capacity(5);
    p.push(0x35); // DES, 1-byte length follows
    p.push(0x03); // length = 3 (1 desc byte + 2 UUID bytes)
    p.push(0x19); // UUID16 descriptor
    p.extend_from_slice(&v.to_be_bytes());
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

/// Pull attribute `0x0311 SupportedFeatures` out of an attribute-list blob.
///
/// In a returned record an attribute is `<uint16 id><value>`, so the HFP AG's supported-features
/// bitmap is the byte string `09 03 11 09 HH LL` — attribute-id element, then a uint16 element.
/// Purely informational for us (we log it and it tells the next session what the AG claims); the
/// SLC does not depend on it, because HFP carries the same bitmap in `+BRSF` on the wire and THAT
/// is what `hfp_hf` acts on.
fn scan_hfp_supported_features(blob: &[u8]) -> Option<u16> {
    blob.windows(6)
        .find(|w| w[0] == 0x09 && w[1] == 0x03 && w[2] == 0x11 && w[3] == 0x09)
        .map(|w| u16::from_be_bytes([w[4], w[5]]))
}

/// Hold the ACL to `peer` open for `secs` by keeping an L2CAP channel to its SDP PSM alive, then
/// drop it.
///
/// **Its original purpose is closed.** It was an experiment (2026-09-01) built on the assumption
/// that gearhead would dial OUR Android Auto RFCOMM channel if we just held the link past its 5 s
/// `waitForHeadUnitConnected` window. It does eventually dial that channel, but not because of
/// time: it dials it only once `BluetoothProfile.HEADSET` reports us CONNECTED, which no amount of
/// idle ACL produces. `reconnect::attempt_headset` connects to the phone's audio gateway instead,
/// and the RFCOMM link that creates holds the ACL by itself.
///
/// KEPT, narrowed, for the one case that path does not cover: a bonded peer exposing NEITHER an
/// iAP2 service NOR any audio gateway ([`Services::has_audio_gateway`]). There is nothing to
/// connect to there, and holding the link is the only remaining lever for finding out what such a
/// peer does with time. Gated by `CARPLAY_ACL_HOLD_SECS` / `/tmp/acl_hold_secs`.
pub fn hold_acl(peer: [u8; 6], secs: u64, timeout_secs: i64) {
    match connect_sdp(peer, timeout_secs) {
        Ok(_sock) => {
            log(&format!("holding the ACL open for {secs}s (experiment)"));
            std::thread::sleep(std::time::Duration::from_secs(secs));
            log("releasing the held ACL");
            // `_sock` drops here, closing the channel.
        }
        Err(e) => log(&format!("could not hold the ACL: {e}")),
    }
}

/// What a bonded peer turned out to expose, from ONE SDP conversation.
///
/// One struct rather than several functions because every search MUST share one L2CAP channel: the
/// L2CAP connect is what pages the phone, and a second `query`-style call would page again and
/// spend most of its time on the connect.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub struct Services {
    /// RFCOMM channel of the phone's "Wireless iAP v2" service — an iPhone.
    pub iap2: Option<u8>,
    /// RFCOMM channel of the phone's Android Auto wireless-projection service ([`AAWG_UUID128`]).
    ///
    /// **Diagnostic only.** Kept because its absence is the evidence, not because anything acts on
    /// it: this Pixel has no such record (`AA-wireless-UUID search -> 2 bytes: 3500`) and gearhead
    /// never registers one — the phone is the CLIENT of that UUID, not its server
    /// (`ojk.java:31-35`). Nothing dials this. See `docs/androidauto/03_WIRELESS.md` §2f.
    pub aawg: Option<u8>,
    /// RFCOMM channel of the phone's **Handsfree Audio Gateway** (`0x111F`) service. This is the
    /// one that matters: completing an HFP SLC to it as the hands-free unit is what makes Android's
    /// `HeadsetService` report our address CONNECTED, which is gearhead's wireless-setup gate.
    /// Channel 4 on the bench Pixel — read from the search, never assumed.
    pub hfp_ag: Option<u8>,
    /// The AG's `0x0311 SupportedFeatures` bitmap, when its record carries one. Logged; the SLC
    /// uses the `+BRSF` value from the wire instead.
    pub hfp_ag_features: Option<u16>,
    /// RFCOMM channel of the phone's **Headset Audio Gateway** (`0x1112`) service — channel 3 on
    /// the bench Pixel. The no-AT fallback: AOSP opens the service level immediately on an inbound
    /// connection to an HSP AG channel (`bta_ag_act.cc:533-540`), where the HFP one waits for the
    /// SLC.
    pub hsp_ag: Option<u8>,
}

impl Services {
    /// Does this peer offer any headset-class gateway to connect to?
    pub fn has_audio_gateway(&self) -> bool {
        self.hfp_ag.is_some() || self.hsp_ag.is_some()
    }
}

/// Discover what the bonded `peer` exposes, over ONE SDP channel: iAP2 (an iPhone), then the two
/// audio-gateway records an Android phone answers with. `Err` only on connect/transport failure — a
/// peer that answers with nothing yields `Services::default()` with the raw catalog logged.
/// Handles SDP continuation by re-requesting with the cookie.
pub fn query(peer: [u8; 6], timeout_secs: i64) -> std::io::Result<Services> {
    let mut disp = peer;
    disp.reverse(); // human-readable bdaddr for the log only
    let human = disp.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":");
    log(&format!("querying SDP on {human} (PSM 0x{SDP_PSM:04x})"));
    let mut sock = connect_sdp(peer, timeout_secs)?;
    log("L2CAP SDP channel up (phone paged)");

    // (1) Targeted: the accessory-side iAP2 UUID. If iOS exposes an iAP2 RFCOMM service by this UUID,
    //     this pulls its channel directly — and an iPhone is done here, with no Android searches run.
    let iap2 = run_search(&mut sock, &search_pattern_iap2(), 1)?;
    log(&format!("iAP2-UUID search -> {} bytes: {}", iap2.len(), hex(&iap2)));
    if let Some(ch) = scan_rfcomm_channel(&iap2) {
        log(&format!("iAP2 RFCOMM channel on the phone = {ch}"));
        return Ok(Services { iap2: Some(ch), ..Services::default() });
    }

    let mut out = Services::default();

    // (2) Handsfree Audio Gateway (0x111F) — the primary Android path. The phone is the AG and we
    //     are the hands-free unit; completing the SLC flips `BluetoothProfile.HEADSET` to
    //     CONNECTED for our address, which is what gearhead's wireless-setup gate reads
    //     (`pcl.java:80`, `kzt.java:56-64`, `pco.java:24-29`).
    let hfp = run_search(&mut sock, &search_pattern_uuid16(0x111F), 20)?;
    log(&format!("HFP-AG (0x111f) search -> {} bytes: {}", hfp.len(), hex(&hfp)));
    out.hfp_ag = scan_rfcomm_channel(&hfp);
    out.hfp_ag_features = scan_hfp_supported_features(&hfp);
    if let Some(ch) = out.hfp_ag {
        match out.hfp_ag_features {
            Some(f) => log(&format!(
                "phone's HFP audio gateway on RFCOMM channel {ch} (SupportedFeatures 0x{f:04x})"
            )),
            None => log(&format!("phone's HFP audio gateway on RFCOMM channel {ch}")),
        }
    }

    // (3) Headset Audio Gateway (0x1112) — the no-AT fallback. AOSP arms the SLC timer only for an
    //     inbound HFP connection; an HSP one goes straight to `bta_ag_svc_conn_open` →
    //     `BTA_AG_CONN_EVT` → `BTHF_CONNECTION_STATE_SLC_CONNECTED` (`bta_ag_act.cc:533-540`),
    //     which is how both public dongles satisfy the same gate with no AT traffic at all.
    let hsp = run_search(&mut sock, &search_pattern_uuid16(0x1112), 30)?;
    log(&format!("HSP-AG (0x1112) search -> {} bytes: {}", hsp.len(), hex(&hsp)));
    out.hsp_ag = scan_rfcomm_channel(&hsp);
    if let Some(ch) = out.hsp_ag {
        log(&format!("phone's HSP audio gateway on RFCOMM channel {ch}"));
    }

    // (4) DIAGNOSTIC ONLY: the Android Auto wireless-projection UUID. Kept because its ABSENCE is
    //     the finding — gearhead is the client of this UUID, never its server, so a hit here would
    //     mean the phone is doing something no observed Android build does. Nothing dials it.
    let aawg = run_search(&mut sock, &search_pattern_aawg(), 50)?;
    log(&format!(
        "AA-wireless-UUID search (diagnostic) -> {} bytes: {}",
        aawg.len(),
        hex(&aawg)
    ));
    out.aawg = scan_rfcomm_channel(&aawg);
    if let Some(ch) = out.aawg {
        log(&format!(
            "UNEXPECTED: the phone HOSTS the Android Auto wireless-projection UUID on RFCOMM channel {ch} — no observed gearhead build does this; nothing dials it (docs/androidauto/03_WIRELESS.md §2f)"
        ));
    }

    if out.has_audio_gateway() {
        return Ok(out);
    }

    // (5) Browse-all (L2CAP UUID): dump the peer's ENTIRE SDP catalog so we can see what it DOES
    //     expose. Purely a diagnostic for a peer that matched NONE of the targeted searches — every
    //     service we can actually drive has been ruled out by this point.
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
        log("browse-all: the peer exposes NO RFCOMM service on BR/EDR — the accessory cannot connect OUT (redirects Model B; see docs/wireless/01_BT_AND_RADIO.md)");
    } else {
        log(&format!(
            "browse-all: the peer exposes RFCOMM channel(s) {chans:?} — but none under the iAP2, HFP-AG or HSP-AG UUIDs; raw catalog above identifies the service UUIDs"
        ));
    }
    Ok(out)
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

    /// The AA wireless-projection UUID is single-sourced from the bench Pixel's own SDP catalog and
    /// is the one thing in this module that CANNOT be re-derived from a spec — pin the bytes.
    #[test]
    fn aawg_uuid_is_4de17a00_52cb_11e6_bdf4_0800200c9a66() {
        assert_eq!(
            hex(&AAWG_UUID128),
            "4de17a0052cb11e6bdf40800200c9a66",
            "the Android Auto wireless-projection UUID must match gearhead's"
        );
    }

    #[test]
    fn ssa_request_has_aawg_uuid_and_range() {
        let req = build_ssa_request(&search_pattern_aawg(), &[0x00]);
        // Same DES/UUID128 shape as the iAP2 pattern — only the 16 UUID bytes differ.
        assert_eq!(&req[0..3], &[0x35, 0x11, 0x1c]);
        assert_eq!(&req[3..19], &AAWG_UUID128);
        assert_eq!(&req[19..21], &[0xff, 0xff]);
        assert_eq!(&req[21..28], &[0x35, 0x05, 0x0a, 0x00, 0x00, 0xff, 0xff]);
        assert_eq!(req[28], 0x00);
    }

    /// The two targeted searches must be byte-identical apart from the UUID, because they go through
    /// the same `run_search`/`build_ssa_request` path and a divergence would only show on hardware.
    #[test]
    fn the_two_targeted_patterns_differ_only_in_the_uuid() {
        let a = search_pattern_iap2();
        let b = search_pattern_aawg();
        assert_eq!(a.len(), b.len());
        assert_eq!(&a[0..3], &b[0..3]);
        assert_ne!(&a[3..], &b[3..]);
    }

    /// The Pixel answers the targeted search with the AA record's own ProtocolDescriptorList, so the
    /// channel scan runs against a single record — L2CAP then RFCOMM ch 8 (its catalog spans 3–8).
    #[test]
    fn scan_finds_the_aawg_channel_in_a_single_record_response() {
        let blob = [
            0x35, 0x0c, 0x35, 0x03, 0x19, 0x01, 0x00, 0x35, 0x05, 0x19, 0x00, 0x03, 0x08, 0x08,
        ];
        assert_eq!(scan_rfcomm_channel(&blob), Some(8));
    }

    #[test]
    fn services_default_is_nothing_at_all() {
        let s = Services::default();
        assert_eq!(s.iap2, None);
        assert_eq!(s.aawg, None);
        assert_eq!(s.hfp_ag, None);
        assert_eq!(s.hfp_ag_features, None);
        assert_eq!(s.hsp_ag, None);
        assert!(!s.has_audio_gateway());
    }

    #[test]
    fn has_audio_gateway_is_either_gateway() {
        assert!(Services { hfp_ag: Some(4), ..Services::default() }.has_audio_gateway());
        assert!(Services { hsp_ag: Some(3), ..Services::default() }.has_audio_gateway());
        // an iAP2 or AAWG hit alone is NOT an audio gateway
        assert!(!Services { iap2: Some(1), aawg: Some(8), ..Services::default() }.has_audio_gateway());
    }

    /// The 16-bit search pattern, for the two gateway UUIDs. Same `build_ssa_request` tail as the
    /// 128-bit ones; only the UUID element differs, and getting its length byte wrong yields a
    /// pattern the phone answers with nothing while looking perfectly plausible in a log.
    #[test]
    fn ssa_request_has_the_uuid16_pattern_and_range() {
        for (uuid, bytes) in [(0x111Fu16, [0x11, 0x1f]), (0x1112, [0x11, 0x12])] {
            let req = build_ssa_request(&search_pattern_uuid16(uuid), &[0x00]);
            assert_eq!(&req[0..3], &[0x35, 0x03, 0x19]);
            assert_eq!(&req[3..5], &bytes);
            assert_eq!(&req[5..7], &[0xff, 0xff]);
            assert_eq!(&req[7..14], &[0x35, 0x05, 0x0a, 0x00, 0x00, 0xff, 0xff]);
            assert_eq!(req[14], 0x00);
        }
    }

    /// We search for the GATEWAY UUIDs (`0x111F`, `0x1112`), never the accessory-side ones
    /// (`0x111E`, `0x1108`) — those are what WE advertise. Searching the wrong side of the pair
    /// returns an empty list from a phone and looks identical to "no HFP".
    #[test]
    fn the_gateway_searches_use_the_gateway_uuids() {
        assert_eq!(search_pattern_uuid16(0x111F)[3..], [0x11, 0x1f]);
        assert_eq!(search_pattern_uuid16(0x1112)[3..], [0x11, 0x12]);
        assert_eq!(bt_common::sdp_record::UUID16_HANDSFREE_AG, 0x111F);
        assert_eq!(bt_common::sdp_record::UUID16_HEADSET_AG, 0x1112);
        assert_ne!(bt_common::sdp_record::UUID16_HANDSFREE, 0x111F);
        assert_ne!(bt_common::sdp_record::UUID16_HEADSET, 0x1112);
    }

    /// The bench Pixel's own HFP AG record, as our browse read it: RFCOMM channel 4 and
    /// `SupportedFeatures` — the stock box logged `SDP: Supported features: 12f` for the same
    /// phone, which is the value pinned here.
    #[test]
    fn the_ag_record_yields_its_channel_and_supported_features() {
        #[rustfmt::skip]
        let blob = [
            0x35u8, 0x1a,
            0x09, 0x00, 0x04, 0x35, 0x0c,
                0x35, 0x03, 0x19, 0x01, 0x00,
                0x35, 0x05, 0x19, 0x00, 0x03, 0x08, 0x04,
            0x09, 0x03, 0x11, 0x09, 0x01, 0x2f,
        ];
        assert_eq!(scan_rfcomm_channel(&blob), Some(4));
        assert_eq!(scan_hfp_supported_features(&blob), Some(0x012f));
    }

    /// A record with no `0x0311` must read as "unknown", never as zero — a zero bitmap would say
    /// the AG supports nothing, and `hfp_hf` would skip `AT+CHLD=?` on the strength of it.
    #[test]
    fn a_record_without_supported_features_reads_as_none() {
        let blob = [0x35u8, 0x05, 0x19, 0x00, 0x03, 0x08, 0x03];
        assert_eq!(scan_hfp_supported_features(&blob), None);
        assert_eq!(scan_rfcomm_channel(&blob), Some(3));
    }

    /// The empty attribute list the bench Pixel actually returns for the AA-wireless UUID
    /// (`2 bytes: 3500`). It must scan as "no channel" rather than tripping the byte search.
    #[test]
    fn an_empty_attribute_list_yields_no_channel() {
        assert_eq!(scan_rfcomm_channel(&[0x35, 0x00]), None);
        assert_eq!(scan_hfp_supported_features(&[0x35, 0x00]), None);
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
