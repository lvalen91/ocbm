//! Minimal, `bluetoothd`-less SDP server for the iAP2 "Wireless iAPv2" service -- a faithful port
//! of `carlink_linux`'s `sdp_server.c` (proven live: "without this the iPhone browses SDP, finds no
//! iAP2 service, and disconnects" -- exactly the failure this project's own first live pairing test
//! reproduced, confirming this module is in fact required, not optional, for a connection to
//! survive past pairing).
//!
//! Binds an L2CAP `SOCK_SEQPACKET` socket to PSM 0x0001 (the well-known SDP PSM) and serves a single
//! pre-built service record (byte-exact from the reference's own CCPA capture) advertising the iAP2
//! UUID `00000000-deca-fade-deca-deafdecacaff` on the given RFCOMM channel. No `libbluetooth`
//! dependency -- the L2CAP sockaddr struct is hand-defined, matching the reference's own approach.

use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::sync::atomic::{AtomicBool, Ordering};

const AF_BLUETOOTH: libc::sa_family_t = 31;
const BTPROTO_L2CAP: libc::c_int = 0;
const SDP_PSM: u16 = 0x0001;

#[repr(C)]
#[derive(Clone, Copy)]
struct SockaddrL2 {
    l2_family: libc::sa_family_t,
    l2_psm: u16, // little-endian on the wire; this Pi is little-endian, so native == LE
    l2_bdaddr: [u8; 6],
    l2_cid: u16,
    l2_bdaddr_type: u8,
}

// SDP PDU identifiers.
const SDP_ERROR_RSP: u8 = 0x01;
const SDP_SVC_SEARCH_REQ: u8 = 0x02;
const SDP_SVC_SEARCH_RSP: u8 = 0x03;
const SDP_SVC_ATTR_REQ: u8 = 0x04;
const SDP_SVC_ATTR_RSP: u8 = 0x05;
const SDP_SVC_SEARCH_ATTR_REQ: u8 = 0x06;
const SDP_SVC_SEARCH_ATTR_RSP: u8 = 0x07;

// SDP error codes.
const SDP_E_INVALID_HANDLE: u16 = 0x0002;
const SDP_E_INVALID_SYNTAX: u16 = 0x0003;
const SDP_E_INVALID_PDU_SIZE: u16 = 0x0004;
const SDP_E_INVALID_CONTINUE: u16 = 0x0005;

/// Base handle. Each registered service gets `SVC_RECORD_HANDLE_BASE + n`; iAP2 is n=0, so its
/// handle stays `0x00010000` exactly as it has always been on the wire.
const SVC_RECORD_HANDLE_BASE: u32 = 0x0001_0000;

/// Ceiling on concurrently-served SDP clients. Generous for the real case (a car sees a handful of
/// phones) and finite, so a peer that opens channels and never speaks cannot spawn threads forever.
const MAX_SDP_CLIENTS: usize = 8;

/// Largest attribute-blob chunk emitted in one response, regardless of the client's requested
/// maximum. Comfortably under the smallest L2CAP MTU any real peer negotiates; the rest follows via
/// continuation state.
const MAX_RESPONSE_CHUNK: usize = 600;

/// The Bluetooth Base UUID. A 16- or 32-bit UUID in a search pattern is shorthand for
/// `0000xxxx-0000-1000-8000-00805F9B34FB`; comparing one against a full 128-bit service class means
/// expanding it first, or a `0x1101` in a pattern would never match anything.
const BASE_UUID: [u8; 16] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00,
    0x80, 0x00, 0x00, 0x80, 0x5f, 0x9b, 0x34, 0xfb,
];

/// UUIDs that EVERY record this server builds necessarily contains, and which therefore cannot
/// discriminate between services:
///   * `0x1002` PublicBrowseGroup — from each record's `BrowseGroupList`
///   * `0x0100` L2CAP and `0x0003` RFCOMM — from each record's `ProtocolDescriptorList`
///   * `0x1101` SerialPort — from the `BluetoothProfileDescriptorList` of the two `ServiceRecord`-
///     shaped records (`sdp_record.rs`'s `UUID16_SERIAL_PORT`)
///
/// `0x1101` is the one approximation here since the HFP record joined the table (2026-09-04): that
/// record's profile descriptor is Handsfree, not SerialPort, so a search for SerialPort ALONE now
/// over-selects it. Over-answering a browse-shaped query is benign — the intent of this arm is
/// "give me everything" — and the alternative is a per-service matchable-UUID list for one UUID no
/// observed phone searches on its own. A search that NAMES a service still selects only that
/// service, which is the property that matters.
///
/// A search pattern built ONLY from these is a "give me everything you have" query, and must return
/// every service. BENCH-PROVEN NECESSARY: Android's discovery makes exactly two searches — DID
/// `0x1200`, then L2CAP `0x0100` — and never uses PublicBrowseGroup. With only the browse group
/// handled, the L2CAP search matched nothing, fell through to the single-service fallback, and the
/// phone cached exactly one UUID for this box — `[BR/EDR UUIDs]` held only
/// `00000000-deca-fade-deca-deafdecacaff`
/// (`dumpsys bluetooth_manager`, 2026-09-01, with the Android Auto record registered but
/// unreachable.) Gearhead then logs `doesn't contain AA UUID and won't request SDP` and gives up.
///
/// This also matches the ONLY known-good oracle. The stock CCPA does not implement SDP at all — it
/// registers records with BlueZ (`sdp_record_register`, `sdp_set_access_protos`) and `sdpd` answers,
/// matching on the record's ACCUMULATED UUID set, which `sdp_set_access_protos` fills with exactly
/// L2CAP and RFCOMM. So this is what the box that provably works already does.
const UNIVERSAL_UUID16: [u16; 4] = [0x1002, 0x0100, 0x0003, 0x1101];

/// One registered service.
pub struct Service {
    pub handle: u32,
    /// Service-class UUID, big-endian, as it appears in the record.
    pub uuid128: [u8; 16],
    /// The encoded record (a `DE_SEQ` of attribute/value pairs).
    pub record: Vec<u8>,
    /// Human name, for logs only.
    pub name: &'static str,
    /// Mirrors `ServiceRecord::extra_class_uuid16` — the hedge lever's 16-bit UUID also appended
    /// to the `ServiceClassIDList`, when set. `select_services` must be able to match on it, or
    /// flipping the lever changes the record bytes without changing what the server matches on.
    pub extra_class_uuid16: Option<u16>,
}

/// `@<unix_ms> ` write-time stamp (docs/carplay/01_OCBM_PROTOCOL.md CH_LOG): the box.log tailer
/// parses this prefix and uses it instead of the millisecond it happened to READ the line at.
fn log(m: &str) {
    println!("@{} [sdp] {m}", now_ms());
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Is per-PDU SDP tracing on? Resolved once per process, matching the crate's env-lever
/// convention (`hci::native_selected`, `rfcomm_uspace`'s backend select). Off by default: every
/// inbound PDU logging unconditionally is noise on a box serving other SDP-heavy traffic — opt in
/// with `SDP_TRACE=1` for an actual troubleshooting session.
fn trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("SDP_TRACE").as_deref() == Ok("1"))
}

/// The iAP2 service, byte-identical to what this server has always advertised.
///
/// Built by `sdp_record`'s encoder rather than the hand-laid array that used to live here; that
/// module's `carplay_record_matches_the_captured_bytes` test asserts the two are byte-for-byte
/// equal, so this is a refactor of how the bytes are produced, not of the bytes.
pub fn iap2_service(rfcomm_chan: u8) -> Service {
    Service {
        handle: SVC_RECORD_HANDLE_BASE,
        uuid128: crate::sdp_record::uuid128_from_u128(0x00000000_deca_fade_deca_deafdecacaff),
        record: crate::sdp_record::ServiceRecord {
            handle: SVC_RECORD_HANDLE_BASE,
            uuid128: crate::sdp_record::uuid128_from_u128(0x00000000_deca_fade_deca_deafdecacaff),
            rfcomm_channel: rfcomm_chan,
            name: "Wireless iAPv2",
            extra_class_uuid16: None,
        }
        .encode(),
        name: "Wireless iAPv2",
        extra_class_uuid16: None,
    }
}

/// The Android Auto wireless-projection service.
///
/// Name and channel both match what the stock CCPA's own `bluetoothDaemon` registers, recovered
/// from its record builders (AAP = channel 4).
pub fn android_auto_service(rfcomm_chan: u8) -> Service {
    const AA: u128 = 0x4de17a00_52cb_11e6_bdf4_0800200c9a66;
    Service {
        handle: SVC_RECORD_HANDLE_BASE + 1,
        uuid128: crate::sdp_record::uuid128_from_u128(AA),
        record: crate::sdp_record::ServiceRecord {
            handle: SVC_RECORD_HANDLE_BASE + 1,
            uuid128: crate::sdp_record::uuid128_from_u128(AA),
            rfcomm_channel: rfcomm_chan,
            name: "Wireless Android Auto Protocol",
            // Stock's shape. Flip to Some(0x1101) as the first hedge if a phone will not offer
            // wireless AA — see ServiceRecord::extra_class_uuid16.
            extra_class_uuid16: None,
        }
        .encode(),
        name: "Wireless Android Auto Protocol",
        extra_class_uuid16: None,
    }
}

/// The HFP **Hands-Free** service, on its own RFCOMM channel.
///
/// Advertised so gearhead's wireless-setup gate can be satisfied: it requires
/// `BluetoothProfile.HEADSET.getConnectedDevices()` to contain the head unit, i.e. an HFP SLC with
/// the phone as audio gateway and us as hands-free (`docs/androidauto/03_WIRELESS.md` §6b). The
/// record is what lets a phone whose `PhonePolicy` auto-connects HFP to a bonded HF device dial us;
/// the wireless crate's `hfp_hf` module also dials the phone's own AG record, which is what the
/// stock box does and the direction that is field-proven.
///
/// `uuid128` is the base-expanded `0x111E`, so a UUID16 search for Handsfree selects exactly this
/// record; `extra_class_uuid16` makes the second class (`0x1203 GenericAudio`) equally searchable.
pub fn hfp_hf_service(rfcomm_chan: u8) -> Service {
    let record = crate::sdp_record::HandsFreeRecord {
        handle: SVC_RECORD_HANDLE_BASE + 2,
        rfcomm_channel: rfcomm_chan,
        name: "Hands-Free",
        profile_version: 0x0107,
        supported_features: 0x003F,
    }
    .encode();
    Service {
        handle: SVC_RECORD_HANDLE_BASE + 2,
        uuid128: expand_uuid(crate::sdp_record::UUID16_HANDSFREE as u32),
        record,
        name: "Hands-Free",
        extra_class_uuid16: Some(crate::sdp_record::UUID16_GENERIC_AUDIO),
    }
}

/// The HSP **Headset** service, on its own RFCOMM channel.
///
/// Advertised ALONGSIDE [`hfp_hf_service`], not instead of it, because the two reach gearhead's
/// `BluetoothProfile.HEADSET` gate by different routes in AOSP and only the HFP one needs an AT
/// dialogue (`bta_ag_act.cc:533-540`; see `sdp_record::HeadsetRecord`). A phone whose `PhonePolicy`
/// auto-connects to a bonded headset-class device then finds a record either way.
pub fn hsp_hs_service(rfcomm_chan: u8) -> Service {
    let record = crate::sdp_record::HeadsetRecord {
        handle: SVC_RECORD_HANDLE_BASE + 3,
        rfcomm_channel: rfcomm_chan,
        name: "Headset",
        profile_version: 0x0102,
        remote_audio_volume_control: false,
    }
    .encode();
    Service {
        handle: SVC_RECORD_HANDLE_BASE + 3,
        uuid128: expand_uuid(crate::sdp_record::UUID16_HEADSET as u32),
        record,
        name: "Headset",
        extra_class_uuid16: Some(crate::sdp_record::UUID16_GENERIC_AUDIO),
    }
}

/// Wrap a set of records as the `ServiceSearchAttributeResponse` payload: SDP's "list of attribute
/// lists", i.e. an outer sequence containing each record.
///
/// Uses an 8-bit length while the body fits one and a 16-bit length otherwise. Both are legal, and
/// the 8-bit form keeps the single-record (CarPlay-only) response byte-identical to what shipped
/// before. The 16-bit fallback exists because two records already come to ~196 bytes of a 255-byte
/// ceiling: without it, a third service — or merely a longer service name — would overflow, and
/// since the release profile is `panic = "abort"`, that overflow would take the whole daemon down
/// and CarPlay with it.
fn wrap_attr_lists(records: &[&[u8]]) -> Vec<u8> {
    let body_len: usize = records.iter().map(|r| r.len()).sum();
    let mut out = Vec::with_capacity(body_len + 3);
    if body_len <= u8::MAX as usize {
        out.push(0x35);
        out.push(body_len as u8);
    } else {
        out.push(0x36);
        out.extend_from_slice(&(body_len as u16).to_be_bytes());
    }
    for r in records {
        out.extend_from_slice(r);
    }
    out
}

fn de_parse(p: &[u8]) -> Option<(usize, usize)> {
    let desc = *p.first()?;
    let typ = desc >> 3;
    let siz = desc & 0x07;
    let (hl, dl): (usize, usize) = match siz {
        0 => (1, if typ == 0 { 0 } else { 1 }),
        1 => (1, 2),
        2 => (1, 4),
        3 => (1, 8),
        4 => (1, 16),
        5 => (2, *p.get(1)? as usize),
        6 => (3, ((*p.get(1)? as usize) << 8) | *p.get(2)? as usize),
        7 => (
            5,
            ((*p.get(1)? as usize) << 24)
                | ((*p.get(2)? as usize) << 16)
                | ((*p.get(3)? as usize) << 8)
                | *p.get(4)? as usize,
        ),
        _ => return None,
    };
    // audit Fix #10: checked_add — for size-code 7 `dl` is a radio-controlled 32-bit length up to
    // 0xFFFF_FFFF; on armv7 (32-bit usize, release overflow-checks off) `hl + dl` would WRAP past this
    // bounds check and hand back a bogus length. Reject overflow (and out-of-bounds) here.
    let total = hl.checked_add(dl)?;
    if total > p.len() {
        return None;
    }
    Some((hl, dl))
}

/// Expand a 16- or 32-bit UUID to its full 128-bit form via the Bluetooth Base UUID.
fn expand_uuid(short: u32) -> [u8; 16] {
    let mut u = BASE_UUID;
    u[0..4].copy_from_slice(&short.to_be_bytes());
    u
}

/// Pull every UUID out of a ServiceSearchPattern (a `DE_SEQ` of UUIDs).
///
/// Returns `None` on anything malformed, and the CALLER treats that as "no match" and falls back to
/// the default service — never as a hard error. A phone that sends a pattern we cannot parse should
/// still get the answer it would have got before this server understood patterns at all.
fn parse_search_pattern(p: &[u8]) -> Option<Vec<[u8; 16]>> {
    let (hl, dl) = de_parse(p)?;
    let body = p.get(hl..hl + dl)?;
    let mut out = Vec::new();
    let mut off = 0usize;
    while off < body.len() {
        let rest = body.get(off..)?;
        let (ehl, edl) = de_parse(rest)?;
        let desc = *rest.first()?;
        // Type 3 (0b00011) is UUID; the size code gives 2, 4 or 16 bytes.
        if desc >> 3 == 3 {
            let val = rest.get(ehl..ehl + edl)?;
            match edl {
                2 => out.push(expand_uuid(u16::from_be_bytes(val.try_into().ok()?) as u32)),
                4 => out.push(expand_uuid(u32::from_be_bytes(val.try_into().ok()?))),
                16 => out.push(<[u8; 16]>::try_from(val).ok()?),
                _ => return None,
            }
        }
        off = off.checked_add(ehl)?.checked_add(edl)?;
    }
    Some(out)
}

/// Which services a search pattern selects.
///
/// The rule, and why each arm exists:
///   * PublicBrowseGroup / L2CAP / RFCOMM / SerialPort only -> EVERY service. This is the generic
///     browse; answering it with one record is how a second service never reaches the phone's UUID
///     cache.
///   * a specific service UUID matches -> exactly those services.
///   * a WELL-FORMED pattern that matches nothing -> NOTHING.
///   * a pattern we could not parse, or an empty one -> the default (first) service.
///
/// **CORRECTED 2026-09-04.** The third arm used to fall back to the default service too, and that
/// was a real defect, caught in the Pixel's own HCI snoop: after bonding, the phone asked for PnP
/// Information (`0x1200`) and Phonebook Access Client (`0x112E`) and got the iAP2 record back both
/// times. Answering a search with a record that does not contain the searched UUID is a protocol
/// lie — the phone caches a service class for us that we do not implement, and with an HFP record
/// now in the table the same bug would hand a phone the WRONG record for `0x111E`.
///
/// The parse-failure and empty-pattern arms are kept as they were, deliberately: those are our own
/// inability to read the request rather than a genuine miss, and before this server understood
/// patterns at all it answered every request with iAP2. No request an iPhone has ever made may
/// start getting a different answer because Android Auto was added.
fn select_services<'a>(services: &'a [Service], pattern: &[u8]) -> Vec<&'a Service> {
    let Some(uuids) = parse_search_pattern(pattern) else {
        return services.first().into_iter().collect();
    };
    // An EMPTY pattern is not "everything". `all()` is vacuously true over zero elements, so
    // without this guard a zero-length DES would fall into the universal arm below and return every
    // service — silently changing an answer this server has always given as the single iAP2 record.
    if uuids.is_empty() {
        return services.first().into_iter().collect();
    }
    // Every UUID is a non-discriminating one => the client is asking for everything.
    if uuids
        .iter()
        .all(|u| UNIVERSAL_UUID16.iter().any(|s| *u == expand_uuid(*s as u32)))
    {
        return services.iter().collect();
    }
    // A well-formed pattern that matches nothing selects NOTHING. `handle_service_search` answers
    // with a zero-length handle list and `handle_search_attr` with an empty `35 00` attribute list,
    // both of which are legal and are what a phone asking about a service we do not have expects.
    services
        .iter()
        .filter(|svc| {
            uuids.contains(&svc.uuid128)
                || svc
                    .extra_class_uuid16
                    .is_some_and(|extra| uuids.contains(&expand_uuid(extra as u32)))
        })
        .collect()
}

/// A small bounds-checked read cursor over a request's parameter block.
struct Cur<'a> {
    p: &'a [u8],
    off: usize,
}

impl<'a> Cur<'a> {
    fn u16(&mut self) -> Option<u16> {
        let v = u16::from_be_bytes(self.p.get(self.off..self.off + 2)?.try_into().ok()?);
        self.off += 2;
        Some(v)
    }
    fn u32(&mut self) -> Option<u32> {
        let v = u32::from_be_bytes(self.p.get(self.off..self.off + 4)?.try_into().ok()?);
        self.off += 4;
        Some(v)
    }
    /// Skip one complete data element (e.g. a service-search pattern or attribute-ID list).
    fn skip_de(&mut self) -> Option<()> {
        let (hl, dl) = de_parse(self.p.get(self.off..)?)?;
        // de_parse already bounds `hl + dl` to the remaining slice (so no wrap here once it succeeds),
        // but guard the cursor advance too (audit Fix #10, defense-in-depth on armv7).
        self.off = self.off.checked_add(hl)?.checked_add(dl)?;
        Some(())
    }
    /// Read the trailing continuation state -> resume offset. `0x00` => 0, `0x02 NN NN` => offset.
    /// The server only ever emits 0- or 2-byte cookies (see `serve_attr_blob`), so that's all this
    /// accepts -- anything else is rejected, matching the reference exactly.
    fn continuation(&mut self) -> Option<usize> {
        let info = *self.p.get(self.off)?;
        self.off += 1;
        if self.off + info as usize > self.p.len() {
            return None;
        }
        match info {
            0 => Some(0),
            2 => {
                let v = u16::from_be_bytes(self.p.get(self.off..self.off + 2)?.try_into().ok()?);
                self.off += 2;
                Some(v as usize)
            }
            _ => None,
        }
    }
}

fn sdp_send(stream: &mut impl Write, pdu_id: u8, tid: u16, params: &[u8]) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(5 + params.len());
    buf.push(pdu_id);
    buf.extend_from_slice(&tid.to_be_bytes());
    buf.extend_from_slice(&(params.len() as u16).to_be_bytes());
    buf.extend_from_slice(params);
    stream.write_all(&buf)
}

fn sdp_error(stream: &mut impl Write, tid: u16, code: u16) -> std::io::Result<()> {
    log(&format!("error response code=0x{code:04x}"));
    sdp_send(stream, SDP_ERROR_RSP, tid, &code.to_be_bytes())
}

/// Serve an attribute-list-style response (`ServiceAttributeResponse`/`ServiceSearchAttributeResponse`
/// share the exact wire shape): `ByteCount(2) | <chunk> | ContinuationState`. Chunks `blob` honoring
/// `maxbytes`, emitting a 2-byte BE offset cookie when more data remains or a bare `0x00` when done.
fn serve_attr_blob(
    stream: &mut impl Write,
    resp_pdu: u8,
    tid: u16,
    blob: &[u8],
    offset: usize,
    maxbytes: u16,
) -> std::io::Result<()> {
    if offset > blob.len() {
        return sdp_error(stream, tid, SDP_E_INVALID_CONTINUE);
    }
    let remaining = blob.len() - offset;
    let mut chunk = remaining;
    if maxbytes != 0 && chunk > maxbytes as usize {
        chunk = maxbytes as usize;
    }
    // Cap independently of what the client asked for. This server never reads the negotiated
    // outgoing L2CAP MTU, so a peer that negotiated a small one and then sent
    // MaximumAttributeByteCount=0xFFFF would have us build a PDU it cannot receive: `write_all`
    // fails EMSGSIZE, the channel closes, and an iPhone that browses SDP and finds no iAP2 service
    // disconnects. That never bit while one record meant a 97-byte response; two records make it
    // ~205 and a third would go further. Continuation makes chunking free, so cap and let the
    // client come back for the rest.
    if chunk > MAX_RESPONSE_CHUNK {
        chunk = MAX_RESPONSE_CHUNK;
    }
    if chunk == 0 && remaining > 0 {
        chunk = 1; // always make progress
    }
    let next = offset + chunk;
    let more = next < blob.len();

    let mut params = Vec::with_capacity(2 + chunk + 3);
    params.extend_from_slice(&(chunk as u16).to_be_bytes());
    params.extend_from_slice(&blob[offset..offset + chunk]);
    if more {
        params.push(0x02);
        params.extend_from_slice(&(next as u16).to_be_bytes());
    } else {
        params.push(0x00);
    }
    log(&format!(
        "blob total={} offset={offset} chunk={chunk} more={more}",
        blob.len()
    ));
    sdp_send(stream, resp_pdu, tid, &params)
}

/// `0x02 ServiceSearchRequest -> 0x03 ServiceSearchResponse`.
///
/// Now actually matches the pattern. It used to return the one known handle unconditionally, which
/// was fine with one record and wrong with two: an Android Auto search would resolve to iAP2's
/// handle, the phone would then fetch iAP2's record and read RFCOMM channel 1 out of it, and dial
/// the wrong service.
fn handle_service_search(
    stream: &mut impl Write,
    tid: u16,
    params: &[u8],
    services: &[Service],
) -> std::io::Result<()> {
    let mut c = Cur { p: params, off: 0 };
    let pattern_at = c.off;
    if c.skip_de().is_none() {
        return sdp_error(stream, tid, SDP_E_INVALID_SYNTAX);
    }
    let pattern = &params[pattern_at..c.off];
    let Some(max_recs) = c.u16() else {
        return sdp_error(stream, tid, SDP_E_INVALID_SYNTAX);
    };
    if c.continuation().is_none() {
        return sdp_error(stream, tid, SDP_E_INVALID_CONTINUE);
    }

    let mut hits = select_services(services, pattern);
    // MaximumServiceRecordCount is a cap the client sets; honour it rather than overrunning.
    if max_recs != 0 && hits.len() > max_recs as usize {
        hits.truncate(max_recs as usize);
    }
    log(&format!(
        "ServiceSearchRequest -- {} match(es): {}",
        hits.len(),
        hits.iter().map(|s| s.name).collect::<Vec<_>>().join(", ")
    ));

    let n = hits.len() as u16;
    let mut p = Vec::with_capacity(5 + 4 * hits.len());
    p.extend_from_slice(&n.to_be_bytes()); // TotalServiceRecordCount
    p.extend_from_slice(&n.to_be_bytes()); // CurrentServiceRecordCount
    for svc in &hits {
        p.extend_from_slice(&svc.handle.to_be_bytes());
    }
    p.push(0x00); // ContinuationState: none — the whole list always fits
    sdp_send(stream, SDP_SVC_SEARCH_RSP, tid, &p)
}

/// `0x04 ServiceAttributeRequest -> 0x05 ServiceAttributeResponse`.
fn handle_service_attr(
    stream: &mut impl Write,
    tid: u16,
    params: &[u8],
    services: &[Service],
) -> std::io::Result<()> {
    let mut c = Cur { p: params, off: 0 };
    let Some(handle) = c.u32() else {
        return sdp_error(stream, tid, SDP_E_INVALID_SYNTAX);
    };
    let Some(maxbytes) = c.u16() else {
        return sdp_error(stream, tid, SDP_E_INVALID_SYNTAX);
    };
    if c.skip_de().is_none() {
        return sdp_error(stream, tid, SDP_E_INVALID_SYNTAX);
    }
    let Some(resume) = c.continuation() else {
        return sdp_error(stream, tid, SDP_E_INVALID_CONTINUE);
    };
    log(&format!(
        "ServiceAttributeRequest handle=0x{handle:08x} maxbytes={maxbytes} resume={resume}"
    ));
    // Table lookup, not a comparison against one constant: with a second service registered, its
    // own handle must be servable or the phone that just learned it from a search gets an error.
    let Some(svc) = services.iter().find(|s| s.handle == handle) else {
        return sdp_error(stream, tid, SDP_E_INVALID_HANDLE);
    };
    serve_attr_blob(stream, SDP_SVC_ATTR_RSP, tid, &svc.record, resume, maxbytes)
}

/// `0x06 ServiceSearchAttributeRequest -> 0x07 ServiceSearchAttributeResponse`.
///
/// CONTINUATION INVARIANT: the blob served here is rebuilt from THIS request's search pattern every
/// time. The continuation cookie is a bare byte offset with no blob identity, so that is what makes
/// resuming safe — SDP clients repeat the full request on continuation, so the same pattern yields
/// the same blob and the offset still means what it meant. Never select the blob from server-side
/// state (a "current mode", the last-seen peer): a continuation would then index into a different
/// blob and hand the phone garbage that still parses.
fn handle_search_attr(
    stream: &mut impl Write,
    tid: u16,
    params: &[u8],
    services: &[Service],
) -> std::io::Result<()> {
    let mut c = Cur { p: params, off: 0 };
    let pattern_at = c.off;
    if c.skip_de().is_none() {
        return sdp_error(stream, tid, SDP_E_INVALID_SYNTAX);
    }
    let pattern = &params[pattern_at..c.off];
    let Some(maxbytes) = c.u16() else {
        return sdp_error(stream, tid, SDP_E_INVALID_SYNTAX);
    };
    if c.skip_de().is_none() {
        return sdp_error(stream, tid, SDP_E_INVALID_SYNTAX);
    }
    let Some(resume) = c.continuation() else {
        return sdp_error(stream, tid, SDP_E_INVALID_CONTINUE);
    };

    let hits = select_services(services, pattern);
    let records: Vec<&[u8]> = hits.iter().map(|s| s.record.as_slice()).collect();
    let blob = wrap_attr_lists(&records);
    log(&format!(
        "ServiceSearchAttributeRequest maxbytes={maxbytes} resume={resume} -- {} record(s), {} bytes",
        hits.len(),
        blob.len()
    ));
    serve_attr_blob(stream, SDP_SVC_SEARCH_ATTR_RSP, tid, &blob, resume, maxbytes)
}

/// Serve one accepted L2CAP connection until it closes. `SOCK_SEQPACKET` means one `read()` yields
/// exactly one whole SDP PDU. The 1s SO_RCVTIMEO `run` sets on the accepted fd surfaces here as
/// WouldBlock/TimedOut — that's the shutdown poll, not an error: an iPhone legitimately idles on an
/// open SDP channel, and treating idle as fatal (or blocking forever without the timeout) either
/// drops a live client or wedges `main.rs`'s join so AV teardown never runs (the #106 orphan class).
fn serve_client(
    stream: &mut std::fs::File,
    services: &[Service],
    shutdown: &AtomicBool,
    stopping: &AtomicBool,
) {
    let mut buf = [0u8; 4096];
    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) => {
                log("client closed");
                return;
            }
            Ok(n) => n,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // BOTH flags. `stopping` is set whenever the accept loop leaves for any reason,
                // including a fatal accept error — without it, `thread::scope` would join these
                // threads on that path while they happily served idle clients, and the server would
                // hang instead of returning the error.
                if shutdown.load(Ordering::Relaxed) || stopping.load(Ordering::Relaxed) {
                    log("shutdown requested -- closing SDP client");
                    return;
                }
                continue; // idle peer, no shutdown — keep serving
            }
            Err(e) => {
                log(&format!("recv: {e}"));
                return;
            }
        };
        if n < 5 {
            log(&format!("runt PDU ({n} bytes)"));
            continue;
        }
        let pdu = buf[0];
        let tid = u16::from_be_bytes([buf[1], buf[2]]);
        let plen = u16::from_be_bytes([buf[3], buf[4]]) as usize;
        if plen + 5 > n {
            log(&format!("truncated PDU: plen={plen} have={}", n - 5));
            let _ = sdp_error(stream, tid, SDP_E_INVALID_PDU_SIZE);
            continue;
        }
        let params = &buf[5..5 + plen];
        if trace_enabled() {
            log(&format!("<- pdu=0x{pdu:02x} tid=0x{tid:04x} plen={plen}"));
        }
        let result = match pdu {
            SDP_SVC_SEARCH_REQ => handle_service_search(stream, tid, params, services),
            SDP_SVC_ATTR_REQ => handle_service_attr(stream, tid, params, services),
            SDP_SVC_SEARCH_ATTR_REQ => handle_search_attr(stream, tid, params, services),
            _ => {
                log(&format!("unsupported pdu 0x{pdu:02x}"));
                sdp_error(stream, tid, SDP_E_INVALID_SYNTAX)
            }
        };
        if let Err(e) = result {
            log(&format!("send failed: {e}"));
            return;
        }
    }
}

fn open_l2cap_listener() -> std::io::Result<std::fs::File> {
    // SOCK_CLOEXEC — this is the highest-stakes one in the tree. PSM 0x0001 is a well-known,
    // single-holder PSM, and this socket is open when bt_driver drives av::ensure_av_layer(), which
    // fork+execs airplayd/rx-connect SETSID-DETACHED TO OUTLIVE THIS PROCESS (av.rs:13). Without
    // CLOEXEC they inherit the PSM-1 binding; if this process then dies without reaching
    // teardown_av_layer() (SIGKILL/panic — `panic = "abort"`, so no unwinding), the restarted
    // instance's bind() finds the PSM still held by a daemon it cannot see, `run` returns Err, and
    // the failure is exactly the one this module's header documents: the iPhone browses SDP, finds
    // no iAP2 service, and disconnects.
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
    let addr = SockaddrL2 {
        l2_family: AF_BLUETOOTH,
        l2_psm: SDP_PSM,
        l2_bdaddr: [0; 6],
        l2_cid: 0,
        l2_bdaddr_type: 0,
    };
    let ret = unsafe {
        libc::bind(
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
    if unsafe { libc::listen(fd, 1) } < 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // A receive timeout on accept() would need a poll loop of its own; simplest for this phase is
    // a timeout on the listening fd so `run`'s shutdown check gets a chance between accepts.
    // zeroed()+assign, not a struct literal: under `musl32_time64` (riscv32) these
    // types carry private padding and a literal does not compile.
    let mut timeout: libc::timeval = unsafe { std::mem::zeroed() };
    timeout.tv_sec = 1;
    // Checked, like bind/listen above and the accepted-fd re-set in `run` below: an unnoticed
    // failure here would leave accept() blocking forever, so `run`'s shutdown check never fires.
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &timeout as *const libc::timeval as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // SAFETY: fd is a freshly opened, bound, listening, exclusively-owned socket descriptor.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// Run the SDP accept loop until `shutdown` is set. One client at a time (matching the reference's
/// `listen(srv, 1)` backlog), each served to completion (or disconnect) before accepting the next.
pub fn run(rfcomm_chan: u8, shutdown: &AtomicBool) -> std::io::Result<()> {
    run_services(vec![iap2_service(rfcomm_chan)], shutdown)
}

/// Serve an arbitrary set of services from the one SDP server.
///
/// There can only be one: L2CAP PSM 0x0001 is a well-known single-holder port and this binds it
/// without `SO_REUSEADDR`, so a second process trying to advertise its own service gets
/// `EADDRINUSE` and is silently never discoverable. That is why wireless Android Auto is served
/// from inside this daemon rather than beside it, and it matches what the stock box does: one
/// `bluetoothDaemon`, one Bluetooth identity, several records, told apart by RFCOMM channel.
pub fn run_services(services: Vec<Service>, shutdown: &AtomicBool) -> std::io::Result<()> {
    let listener = open_l2cap_listener()?;
    for svc in &services {
        log(&format!(
            "serving '{}' (handle 0x{:08x}) on L2CAP PSM 0x{SDP_PSM:04x}",
            svc.name, svc.handle
        ));
    }
    let services = std::sync::Arc::new(services);
    let live = std::sync::atomic::AtomicUsize::new(0);
    // Set on EVERY exit from the accept loop, so client threads wind down and `thread::scope` can
    // join them. Distinct from the caller's `shutdown`, which stays false on the error path.
    let stopping = AtomicBool::new(false);

    let result = std::thread::scope(|scope| -> std::io::Result<()> {

    let listen_fd = std::os::fd::AsRawFd::as_raw_fd(&listener);
    while !shutdown.load(Ordering::Relaxed) {
        let mut ra: SockaddrL2 = unsafe { std::mem::zeroed() };
        let mut ralen = std::mem::size_of::<SockaddrL2>() as libc::socklen_t;
        // accept4(SOCK_CLOEXEC) — same reasoning as open_l2cap_listener above.
        let cfd = unsafe {
            crate::cloexec::accept_cloexec(
                listen_fd,
                &mut ra as *mut SockaddrL2 as *mut libc::sockaddr,
                &mut ralen,
            )
        };
        if cfd < 0 {
            let e = std::io::Error::last_os_error();
            // audit Fix #10: a single TRANSIENT accept error must not kill the SDP server for the whole
            // session — a client that RSTs mid-handshake (ECONNABORTED) or an EINTR would otherwise stop
            // ALL SDP responses, i.e. "phone finds no iAP2 service". Keep accepting on those (matching
            // bt_driver's resilient RFCOMM accept); only a genuinely unexpected error (likely a broken
            // listener) still surfaces as Err. The while-guard re-checks shutdown each pass, and the 1s
            // SO_RCVTIMEO bounds each accept, so continue-on-transient cannot trap the thread at teardown.
            match e.kind() {
                std::io::ErrorKind::WouldBlock
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::Interrupted => continue,
                _ => {
                    stopping.store(true, Ordering::Relaxed);
                    return Err(e);
                }
            }
        }
        log("client connected on SDP PSM");
        // Re-set SO_RCVTIMEO on the ACCEPTED fd (review fix 2026-07-31): Linux Bluetooth sockets do
        // NOT inherit the listener's timeout on accept (l2cap_sock_alloc → sock_init_data resets
        // sk_rcvtimeo on this 3.14.52 kernel) — the timeout set in `open_l2cap_listener` only covers
        // accept() itself. Without this, `serve_client` blocks forever on an idling-but-connected
        // peer and the shutdown flag is never polled. Same explicit re-set bt_driver.rs does on ITS
        // accepted RFCOMM socket, and checked like ssp_agent.rs's, since an unnoticed failure here
        // re-opens the forever-block.
        // zeroed()+assign, not a struct literal: under `musl32_time64` (riscv32) these
        // types carry private padding and a literal does not compile.
        let mut timeout: libc::timeval = unsafe { std::mem::zeroed() };
        timeout.tv_sec = 1;
        let ret = unsafe {
            libc::setsockopt(
                cfd,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                &timeout as *const libc::timeval as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        };
        let sockopt_err = if ret < 0 {
            Some(std::io::Error::last_os_error())
        } else {
            None
        };
        let mut client = unsafe { std::fs::File::from_raw_fd(cfd) };
        if let Some(e) = sockopt_err {
            // Serving without the timeout would block unboundedly on an idle peer — drop this
            // client (File closes cfd) and keep accepting rather than risk the wedge.
            log(&format!("SO_RCVTIMEO on accepted fd failed: {e} -- dropping client"));
            continue;
        }
        // CONCURRENT, one thread per client. This server used to run `serve_client` to completion
        // on the accept loop with a backlog of 1. That was safe only while iPhones were the only
        // thing that ever browsed us: `serve_client`'s own contract is that a phone LEGITIMATELY
        // idles on an open SDP channel, so once an Android Auto record is advertised, an idling
        // Android phone would hold the single slot while an arriving iPhone's connect sat unserved
        // — and an iPhone that browses SDP and finds no iAP2 service disconnects. That is the exact
        // CarPlay failure this module was written to prevent, and adding AA would have introduced
        // it, so the fix ships in the same change.
        //
        // Scoped threads so `shutdown` and the service table can be borrowed rather than cloned
        // into 'static; bounded so a hostile or broken peer cannot spawn threads without limit.
        if live.load(Ordering::Relaxed) >= MAX_SDP_CLIENTS {
            log("too many concurrent SDP clients -- dropping this one");
            continue;
        }
        live.fetch_add(1, Ordering::Relaxed);
        let svcs = services.clone();
        let live_ref = &live;
        let stop_ref = &stopping;
        scope.spawn(move || {
            let mut client = client;
            serve_client(&mut client, &svcs, shutdown, stop_ref);
            live_ref.fetch_sub(1, Ordering::Relaxed);
        });
    }
    stopping.store(true, Ordering::Relaxed);
    Ok(())
    });
    stopping.store(true, Ordering::Relaxed);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iap2_record_is_unchanged_by_the_table_rewrite() {
        // The bytes CarPlay sees must not move. `sdp_record`'s own test pins this record against
        // the array captured off a real CCPA; this pins that the SERVER still serves exactly it.
        let svc = iap2_service(1);
        assert_eq!(svc.record.len(), 89);
        assert_eq!(svc.handle, SVC_RECORD_HANDLE_BASE);
        assert_eq!(svc.record[crate::sdp_record::RFCOMM_CHANNEL_OFFSET], 1);
        let ssa = wrap_attr_lists(&[svc.record.as_slice()]);
        assert_eq!(ssa.len(), 91);
        assert_eq!(&ssa[0..2], &[0x35, 89]);
        assert_eq!(&ssa[2..], &svc.record[..]);
    }

    #[test]
    fn services_carry_their_own_rfcomm_channels() {
        assert_eq!(iap2_service(7).record[crate::sdp_record::RFCOMM_CHANNEL_OFFSET], 7);
        assert_eq!(android_auto_service(4).record[crate::sdp_record::RFCOMM_CHANNEL_OFFSET], 4);
    }

    #[test]
    fn de_parse_single_byte_length() {
        // desc=0x08 (type=1,size=0) -> 1-byte data element, header 1, data 1.
        assert_eq!(de_parse(&[0x08, 0xAB]), Some((1, 1)));
    }

    #[test]
    fn de_parse_explicit_one_byte_length() {
        // desc=0x05 (size code 5) -> next byte is an explicit 1-byte length.
        assert_eq!(de_parse(&[0x05, 0x03, 1, 2, 3]), Some((2, 3)));
    }

    #[test]
    fn cursor_continuation_none() {
        let mut c = Cur { p: &[0x00], off: 0 };
        assert_eq!(c.continuation(), Some(0));
    }

    #[test]
    fn cursor_continuation_with_offset() {
        let mut c = Cur {
            p: &[0x02, 0x01, 0x2c],
            off: 0,
        };
        assert_eq!(c.continuation(), Some(0x012c));
    }

    #[test]
    fn cursor_continuation_rejects_unknown_info_length() {
        let mut c = Cur {
            p: &[0x05, 0, 0, 0, 0, 0],
            off: 0,
        };
        assert_eq!(c.continuation(), None);
    }

    #[test]
    fn serve_attr_blob_chunks_when_over_maxbytes() {
        let mut out = Vec::new();
        let blob = [1u8, 2, 3, 4, 5];
        serve_attr_blob(&mut out, SDP_SVC_ATTR_RSP, 0x1234, &blob, 0, 3).unwrap();
        // header: pdu(1) tid(2) plen(2), then ByteCount(2)=3, 3 data bytes, continuation 02 00 03
        assert_eq!(out[0], SDP_SVC_ATTR_RSP);
        let plen = u16::from_be_bytes([out[3], out[4]]) as usize;
        let params = &out[5..5 + plen];
        assert_eq!(&params[0..2], &3u16.to_be_bytes());
        assert_eq!(&params[2..5], &[1, 2, 3]);
        assert_eq!(&params[5..8], &[0x02, 0x00, 0x03]);
    }

    #[test]
    fn serve_attr_blob_no_continuation_when_it_fits() {
        let mut out = Vec::new();
        let blob = [1u8, 2, 3];
        serve_attr_blob(&mut out, SDP_SVC_ATTR_RSP, 0, &blob, 0, 0).unwrap();
        let plen = u16::from_be_bytes([out[3], out[4]]) as usize;
        let params = &out[5..5 + plen];
        assert_eq!(&params[0..2], &3u16.to_be_bytes());
        assert_eq!(&params[2..5], &[1, 2, 3]);
        assert_eq!(params[5], 0x00);
    }

    /// A `DE_SEQ8` search pattern holding one `DE_UUID16` element (`0x19 XX XX`).
    fn uuid16_search_pattern(uuid: u16) -> Vec<u8> {
        let mut des = vec![0x35u8, 3, 0x19];
        des.extend_from_slice(&uuid.to_be_bytes());
        des
    }

    #[test]
    fn select_services_serial_port_uuid_is_universal() {
        // SerialPort (0x1101) is in every record's BluetoothProfileDescriptorList, so a pattern
        // asking for it alone must be treated as "everything", not fall through to the fallback
        // (M2: the lever's UUID must actually be reachable by search, not just present in bytes).
        let services = [iap2_service(1), android_auto_service(4)];
        let hits = select_services(&services, &uuid16_search_pattern(0x1101));
        assert_eq!(hits.len(), 2, "0x1101 alone must match every service, not just the fallback");
    }

    #[test]
    fn select_services_matches_the_extra_class_uuid_lever() {
        // With the hedge lever flipped, a search for that extra UUID (not the 128-bit class UUID,
        // and not one of the universal UUIDs) must hit the service carrying it.
        let mut aa = android_auto_service(4);
        aa.extra_class_uuid16 = Some(0x1234);
        let services = [iap2_service(1), aa];
        let hits = select_services(&services, &uuid16_search_pattern(0x1234));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "Wireless Android Auto Protocol");

        // Without the lever set, the same search matches nothing — and now RETURNS nothing rather
        // than the iAP2 record. See `select_services`' 2026-09-04 correction.
        let services = [iap2_service(1), android_auto_service(4)];
        assert!(select_services(&services, &uuid16_search_pattern(0x1234)).is_empty());
    }

    /// THE DEFECT, from the Pixel's own HCI snoop (2026-09-04): after bonding, the phone asked for
    /// PnP Information (0x1200) and Phonebook Access Client (0x112E) and this server answered both
    /// with the iAP2 record. A search must never be answered with a record that does not contain
    /// the searched UUID.
    #[test]
    fn a_search_for_a_service_we_do_not_have_returns_nothing() {
        let services = all_four();
        for uuid in [0x1200u16, 0x112e, 0x110a, 0x1132, 0x111f, 0x1112] {
            let hits = select_services(&services, &uuid16_search_pattern(uuid));
            assert!(hits.is_empty(), "0x{uuid:04x} must select no service, got {}", hits.len());
        }
        // ...and the wire form of that is a zero-length handle list, not an error.
        let mut out = Vec::new();
        handle_service_search(&mut out, 1, &search_params(&uuid16_search_pattern(0x1200)), &services)
            .unwrap();
        assert_eq!(out[0], SDP_SVC_SEARCH_RSP);
        assert!(handles_from_search_response(&out).is_empty());
        // ...and an empty `35 00` attribute list from the attribute form.
        let mut out = Vec::new();
        let mut params = uuid16_search_pattern(0x1200);
        params.extend_from_slice(&0xffffu16.to_be_bytes());
        params.extend_from_slice(&[0x35, 0x00, 0x00]);
        handle_search_attr(&mut out, 1, &params, &services).unwrap();
        assert_eq!(out[0], SDP_SVC_SEARCH_ATTR_RSP);
        let plen = u16::from_be_bytes([out[3], out[4]]) as usize;
        let p = &out[5..5 + plen];
        assert_eq!(u16::from_be_bytes([p[0], p[1]]) as usize, 2);
        assert_eq!(&p[2..4], &[0x35, 0x00]);
    }

    /// Each audio-profile record must be selected by BOTH of its class UUIDs, and by neither the
    /// other record's nor the audio-gateway UUIDs the PHONE advertises.
    #[test]
    fn the_audio_profile_records_are_selected_by_their_own_class_uuids() {
        let services = all_four();
        let hits = select_services(&services, &uuid16_search_pattern(0x111e));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].handle, SVC_RECORD_HANDLE_BASE + 2);
        let hits = select_services(&services, &uuid16_search_pattern(0x1108));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].handle, SVC_RECORD_HANDLE_BASE + 3);
        // GenericAudio is in both class lists, so it selects both and only those two.
        let hits = select_services(&services, &uuid16_search_pattern(0x1203));
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].handle, SVC_RECORD_HANDLE_BASE + 2);
        assert_eq!(hits[1].handle, SVC_RECORD_HANDLE_BASE + 3);
    }

    /// Every registered handle must be servable — a phone that just learned one from a search and
    /// then gets INVALID_HANDLE will not connect.
    #[test]
    fn all_four_handles_are_distinct_and_servable() {
        let services = all_four();
        let mut handles: Vec<u32> = services.iter().map(|s| s.handle).collect();
        handles.sort_unstable();
        handles.dedup();
        assert_eq!(handles.len(), 4);
        for svc in &services {
            let mut out = Vec::new();
            let mut params = svc.handle.to_be_bytes().to_vec();
            params.extend_from_slice(&0xffffu16.to_be_bytes());
            params.extend_from_slice(&[0x35, 0x00, 0x00]);
            handle_service_attr(&mut out, 1, &params, &services).unwrap();
            assert_eq!(out[0], SDP_SVC_ATTR_RSP, "{} must be servable", svc.name);
        }
    }

    /// A browse must reach all four records, or a service the phone never caches is a service it
    /// never dials — the bench-proven failure this arm was added for.
    #[test]
    fn a_browse_returns_all_four_records() {
        for pattern in [
            vec![0x35u8, 0x03, 0x19, 0x10, 0x02], // PublicBrowseGroup
            vec![0x35, 0x03, 0x19, 0x01, 0x00],   // L2CAP
        ] {
            let mut out = Vec::new();
            handle_service_search(&mut out, 1, &search_params(&pattern), &all_four()).unwrap();
            assert_eq!(handles_from_search_response(&out).len(), 4, "pattern {pattern:?}");
        }
    }

    #[test]
    fn handle_service_search_always_returns_the_one_handle() {
        let mut out = Vec::new();
        // search pattern: an empty-ish sequence (desc=0x35 size-code5, then explicit len 0)
        let params = [
            0x35u8, 0x00, /* max_recs */ 0x00, 0x0a, /* continuation */ 0x00,
        ];
        // Kept as a REGRESSION test, byte-for-byte as it was before this server understood search
        // patterns: an unmatched pattern must still get exactly the iAP2 handle it always got.
        let services = vec![iap2_service(1), android_auto_service(4)];
        handle_service_search(&mut out, 0x0007, &params, &services).unwrap();
        assert_eq!(out[0], SDP_SVC_SEARCH_RSP);
        let plen = u16::from_be_bytes([out[3], out[4]]) as usize;
        let p = &out[5..5 + plen];
        assert_eq!(&p[0..2], &1u16.to_be_bytes());
        assert_eq!(&p[2..4], &1u16.to_be_bytes());
        assert_eq!(&p[4..8], &SVC_RECORD_HANDLE_BASE.to_be_bytes());
    }

    fn both() -> Vec<Service> {
        vec![iap2_service(1), android_auto_service(4)]
    }

    /// Everything `main.rs` actually registers.
    fn all_four() -> Vec<Service> {
        vec![
            iap2_service(crate::sdp_record::IAP2_RFCOMM_CHANNEL),
            android_auto_service(crate::sdp_record::AAP_RFCOMM_CHANNEL),
            hfp_hf_service(crate::sdp_record::HFP_HF_RFCOMM_CHANNEL),
            hsp_hs_service(crate::sdp_record::HSP_HS_RFCOMM_CHANNEL),
        ]
    }

    /// A UUID128 search pattern for one service.
    fn pattern_uuid128(u: u128) -> Vec<u8> {
        let mut v = vec![0x35, 17, 0x1c];
        v.extend_from_slice(&u.to_be_bytes());
        v
    }

    fn search_params(pattern: &[u8]) -> Vec<u8> {
        let mut v = pattern.to_vec();
        v.extend_from_slice(&[0x00, 0x0a]); // max_recs
        v.push(0x00); // continuation
        v
    }

    fn handles_from_search_response(out: &[u8]) -> Vec<u32> {
        let plen = u16::from_be_bytes([out[3], out[4]]) as usize;
        let p = &out[5..5 + plen];
        let n = u16::from_be_bytes([p[2], p[3]]) as usize;
        (0..n).map(|i| u32::from_be_bytes(p[4 + i * 4..8 + i * 4].try_into().unwrap())).collect()
    }

    /// The whole point of the change: an Android Auto search must resolve to the AA handle, not
    /// iAP2's. Getting iAP2's back means the phone reads RFCOMM channel 1 and dials CarPlay.
    #[test]
    fn an_android_auto_search_returns_the_android_auto_handle() {
        let mut out = Vec::new();
        let params = search_params(&pattern_uuid128(0x4de17a00_52cb_11e6_bdf4_0800200c9a66));
        handle_service_search(&mut out, 1, &params, &both()).unwrap();
        assert_eq!(handles_from_search_response(&out), vec![SVC_RECORD_HANDLE_BASE + 1]);
    }

    #[test]
    fn an_iap2_search_still_returns_only_iap2() {
        let mut out = Vec::new();
        let params = search_params(&pattern_uuid128(0x00000000_deca_fade_deca_deafdecacaff));
        handle_service_search(&mut out, 1, &params, &both()).unwrap();
        assert_eq!(handles_from_search_response(&out), vec![SVC_RECORD_HANDLE_BASE]);
    }

    /// A PublicBrowseGroup browse is how Android populates `getUuids()`. Answering it with one
    /// record is how a second service stays invisible to the phone forever.
    #[test]
    fn a_public_browse_group_search_returns_both() {
        let mut out = Vec::new();
        let params = search_params(&[0x35, 0x03, 0x19, 0x10, 0x02]);
        handle_service_search(&mut out, 1, &params, &both()).unwrap();
        assert_eq!(
            handles_from_search_response(&out),
            vec![SVC_RECORD_HANDLE_BASE, SVC_RECORD_HANDLE_BASE + 1]
        );
    }

    /// THE BENCH-PROVEN CASE. Android's discovery searches L2CAP 0x0100 and nothing else useful;
    /// before this it matched no service and fell back to iAP2 alone, so the phone cached exactly
    /// one UUID and gearhead refused to request SDP.
    #[test]
    fn an_l2cap_search_returns_every_service() {
        let mut out = Vec::new();
        let params = search_params(&[0x35, 0x03, 0x19, 0x01, 0x00]); // UUID16 0x0100
        handle_service_search(&mut out, 1, &params, &both()).unwrap();
        assert_eq!(
            handles_from_search_response(&out),
            vec![SVC_RECORD_HANDLE_BASE, SVC_RECORD_HANDLE_BASE + 1]
        );
    }

    /// RFCOMM is equally non-discriminating — both records carry it in their protocol descriptors.
    #[test]
    fn an_rfcomm_search_returns_every_service() {
        let mut out = Vec::new();
        let params = search_params(&[0x35, 0x03, 0x19, 0x00, 0x03]);
        handle_service_search(&mut out, 1, &params, &both()).unwrap();
        assert_eq!(handles_from_search_response(&out).len(), 2);
    }

    /// An EMPTY pattern must NOT be read as "everything". `all()` over zero elements is vacuously
    /// true, so without an explicit guard a zero-length DES would return every service and silently
    /// change an answer this server has always given as the single iAP2 record.
    #[test]
    fn an_empty_pattern_is_not_everything() {
        let services = both();
        let hits = select_services(&services, &[0x35, 0x00]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].handle, SVC_RECORD_HANDLE_BASE);
    }

    /// A pattern MIXING a universal UUID with a specific one is not a "give me everything" query:
    /// per SDP it selects records containing BOTH, which is only the named service.
    #[test]
    fn a_universal_uuid_mixed_with_a_specific_one_is_not_everything() {
        let mut pattern = vec![0x35, 20, 0x19, 0x01, 0x00, 0x1c];
        pattern.extend_from_slice(&0x4de17a00_52cb_11e6_bdf4_0800200c9a66u128.to_be_bytes());
        let services = both();
        let hits = select_services(&services, &pattern);
        assert_eq!(hits.len(), 1, "L2CAP + AA UUID must select only Android Auto");
        assert_eq!(hits[0].handle, SVC_RECORD_HANDLE_BASE + 1);
    }

    /// A blob larger than the chunk cap must be split rather than sent whole: this server never
    /// reads the negotiated L2CAP MTU, so an over-large PDU would fail EMSGSIZE and close the
    /// channel — which for an iPhone means "found no iAP2 service" and a disconnect.
    #[test]
    fn a_large_blob_is_chunked_even_when_the_client_asks_for_everything() {
        let blob = vec![0xa5u8; MAX_RESPONSE_CHUNK * 2];
        let mut out = Vec::new();
        serve_attr_blob(&mut out, SDP_SVC_SEARCH_ATTR_RSP, 1, &blob, 0, 0xffff).unwrap();
        let plen = u16::from_be_bytes([out[3], out[4]]) as usize;
        let p = &out[5..5 + plen];
        let chunk = u16::from_be_bytes([p[0], p[1]]) as usize;
        assert_eq!(chunk, MAX_RESPONSE_CHUNK);
        assert_eq!(p[2 + chunk], 0x02, "must offer a continuation cookie");
    }

    /// An unparseable pattern must fall back to iAP2, never error — no request an iPhone has ever
    /// made may start failing because Android Auto was added.
    #[test]
    fn a_malformed_pattern_falls_back_to_iap2() {
        let services = both();
        assert_eq!(select_services(&services, &[0xff, 0xff]).len(), 1);
        assert_eq!(select_services(&services, &[0xff, 0xff])[0].handle, SVC_RECORD_HANDLE_BASE);
    }

    /// Each service's own handle must be servable, or a phone that just learned it from a search
    /// gets INVALID_HANDLE when it asks for the record.
    #[test]
    fn both_handles_are_servable_and_unknown_ones_error() {
        for (h, chan) in [(SVC_RECORD_HANDLE_BASE, 1u8), (SVC_RECORD_HANDLE_BASE + 1, 4)] {
            let mut out = Vec::new();
            let mut params = h.to_be_bytes().to_vec();
            params.extend_from_slice(&0xffffu16.to_be_bytes());
            params.extend_from_slice(&[0x35, 0x00, 0x00]);
            handle_service_attr(&mut out, 1, &params, &both()).unwrap();
            assert_eq!(out[0], SDP_SVC_ATTR_RSP, "handle 0x{h:08x} must be servable");
            let plen = u16::from_be_bytes([out[3], out[4]]) as usize;
            let rec = &out[7..5 + plen - 1];
            assert_eq!(rec[crate::sdp_record::RFCOMM_CHANNEL_OFFSET], chan);
        }
        let mut out = Vec::new();
        let mut params = 0xdead_beefu32.to_be_bytes().to_vec();
        params.extend_from_slice(&0xffffu16.to_be_bytes());
        params.extend_from_slice(&[0x35, 0x00, 0x00]);
        handle_service_attr(&mut out, 1, &params, &both()).unwrap();
        assert_eq!(out[0], SDP_ERROR_RSP);
    }

    /// One record keeps the 8-bit outer length (CarPlay's response is unchanged); a body over 255
    /// bytes must switch to the 16-bit form rather than overflowing — an overflow would assert, and
    /// `panic = "abort"` would take the daemon and CarPlay down with it.
    #[test]
    fn the_outer_sequence_grows_to_16_bit_when_needed() {
        let one = both();
        let small = wrap_attr_lists(&[one[0].record.as_slice()]);
        assert_eq!(&small[0..2], &[0x35, 89]);

        let big = vec![0u8; 300];
        let large = wrap_attr_lists(&[&big]);
        assert_eq!(large[0], 0x36);
        assert_eq!(u16::from_be_bytes([large[1], large[2]]) as usize, 300);
    }
}
