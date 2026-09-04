//! SDP service-record construction.
//!
//! WHY THIS EXISTS. The CarPlay record shipped as a hand-laid 89-byte array copied from a capture,
//! with a `const RFCOMM_CHAN_IDX: usize = 48` pointing into it. That is correct and proven for the
//! one record it encodes. What it does not survive is EDITING: every sequence in SDP carries its own
//! length prefix, so changing the service name changes the outer length byte, and changing anything
//! before the protocol descriptor list also moves the channel offset. (The name alone does not move
//! that offset — `ServiceName` is the last attribute — which is exactly the kind of detail a
//! hand-laid second array invites getting backwards; see the test of the same name.)
//!
//! A wrong length byte does not fail loudly. The record parses as a truncated or over-long sequence
//! and the phone simply never matches the service. The CCPA gives no interactive debugger to catch
//! that — it is OCBM or NCM, never both, so there is no second channel to watch the box on while a
//! host drives it. The code has to be right before it ships.
//!
//! So: a real encoder, with the proven CarPlay record as its oracle. `tests::carplay_record_matches`
//! asserts this builder reproduces that captured array byte-for-byte. If that test passes, the
//! encoder is trustworthy for records nobody has captured.

/// RFCOMM server channels, matching what the stock CCPA `bluetoothDaemon` allocates: iAP2 = 1,
/// NearBy/Fast-Pair = 2, HiChain = 3, AAP (Android Auto) = 4, generic Serial Port = 15. Recovered
/// from that daemon's own per-service record builders. Picking 2 for Android Auto — as a first cut
/// here did — collides with the channel stock uses for Nearby.
pub const IAP2_RFCOMM_CHANNEL: u8 = 1;
pub const AAP_RFCOMM_CHANNEL: u8 = 4;
/// HFP Hands-Free. Stock's `hfpd` (nohands) never SERVES a channel — it only dials the phone's AG —
/// so there is no stock number to match here. 5 is the first free slot after stock's allocation
/// above, chosen so a later HiChain/Nearby port cannot collide with it.
pub const HFP_HF_RFCOMM_CHANNEL: u8 = 5;
/// HSP Headset, the second audio-profile record. Next free slot after the HF one.
pub const HSP_HS_RFCOMM_CHANNEL: u8 = 6;

/// SDP data-element descriptors (type << 3 | size-index).
const DE_UINT8: u8 = 0x08;
const DE_UINT16: u8 = 0x09;
const DE_UINT32: u8 = 0x0a;
const DE_UUID16: u8 = 0x19;
const DE_UUID128: u8 = 0x1c;
const DE_TEXT8: u8 = 0x25; // text string, 8-bit length
const DE_BOOL: u8 = 0x28; // boolean, one byte of data
const DE_SEQ8: u8 = 0x35; // sequence, 8-bit length

/// Byte offset of the RFCOMM channel in an encoded record.
///
/// Stable for any service NAME, since `ServiceName` is the last attribute — but only for the record
/// SHAPE this module builds (one UUID128 service class, one L2CAP+RFCOMM protocol descriptor, one
/// browse group, one profile descriptor). `ServiceRecord` cannot currently express any other shape,
/// which is the only reason this is safe to expose. If it gains UUID16/32 class lists or a second
/// protocol alternative, this constant must become a value returned from `encode()`.
pub const RFCOMM_CHANNEL_OFFSET: usize = 48;

/// Bytes the encoder emits regardless of the service name: everything up to and including the
/// ServiceName attribute id and its length byte.
const FIXED_BODY_LEN: usize = 73;

/// Longest service name that still fits the outer 8-bit sequence length.
pub const MAX_SERVICE_NAME: usize = u8::MAX as usize - FIXED_BODY_LEN;

/// Attribute ids we emit.
const ATTR_RECORD_HANDLE: u16 = 0x0000;
const ATTR_SERVICE_CLASS_ID_LIST: u16 = 0x0001;
const ATTR_PROTOCOL_DESCRIPTOR_LIST: u16 = 0x0004;
const ATTR_BROWSE_GROUP_LIST: u16 = 0x0005;
const ATTR_PROFILE_DESCRIPTOR_LIST: u16 = 0x0009;
const ATTR_SERVICE_NAME: u16 = 0x0100;
/// HFP's profile-specific `SupportedFeatures` attribute (HFP 1.7 §5.3).
const ATTR_HFP_SUPPORTED_FEATURES: u16 = 0x0311;
/// HSP's profile-specific `RemoteAudioVolumeControl` attribute (HSP 1.2 §5.2).
const ATTR_HSP_REMOTE_VOLUME_CONTROL: u16 = 0x0302;

const UUID16_L2CAP: u16 = 0x0100;
const UUID16_RFCOMM: u16 = 0x0003;
const UUID16_PUBLIC_BROWSE_GROUP: u16 = 0x1002;
const UUID16_SERIAL_PORT: u16 = 0x1101;
/// Handsfree — the HF (accessory) side service class. `0x111F` is the AG side and is what the
/// PHONE advertises; we must never claim it.
pub const UUID16_HANDSFREE: u16 = 0x111E;
/// Handsfree Audio Gateway — the phone's side, searched for by `sdp_client`, never advertised here.
pub const UUID16_HANDSFREE_AG: u16 = 0x111F;
/// Headset — the HS (accessory) side service class. `0x1112` is the AG side, which the PHONE
/// advertises and `sdp_client` searches for; we must never claim it.
pub const UUID16_HEADSET: u16 = 0x1108;
/// Headset Audio Gateway — the phone's side, searched for by `sdp_client`, never advertised here.
pub const UUID16_HEADSET_AG: u16 = 0x1112;
/// GenericAudio, the second class both audio-profile records carry.
pub const UUID16_GENERIC_AUDIO: u16 = 0x1203;

fn put_uint16(out: &mut Vec<u8>, v: u16) {
    out.push(DE_UINT16);
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_uint32(out: &mut Vec<u8>, v: u32) {
    out.push(DE_UINT32);
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_uuid16(out: &mut Vec<u8>, v: u16) {
    out.push(DE_UUID16);
    out.extend_from_slice(&v.to_be_bytes());
}

/// An 8-bit-length sequence wrapping `body`. Panics above 255 bytes, which no record here reaches —
/// a longer one would need `DE_SEQ16` and is a deliberate not-yet-supported case rather than a
/// silent truncation.
fn put_seq8(out: &mut Vec<u8>, body: &[u8]) {
    assert!(body.len() <= u8::MAX as usize, "SDP sequence too long for an 8-bit length");
    out.push(DE_SEQ8);
    out.push(body.len() as u8);
    out.extend_from_slice(body);
}

/// One attribute: its id as a uint16 element, then its value.
fn put_attr(out: &mut Vec<u8>, id: u16, value: &[u8]) {
    put_uint16(out, id);
    out.extend_from_slice(value);
}

/// What a single-service, RFCOMM-based SDP record needs to say.
#[derive(Clone, Debug)]
pub struct ServiceRecord<'a> {
    pub handle: u32,
    /// The 128-bit service class UUID, big-endian as it appears on the wire.
    pub uuid128: [u8; 16],
    pub rfcomm_channel: u8,
    pub name: &'a str,
    /// An extra 16-bit UUID to append to the `ServiceClassIDList`, or `None`.
    ///
    /// Default `None`, which matches the STOCK CCPA: its record builders emit the 128-bit class
    /// alone, with SerialPort appearing only in the profile-descriptor list — and stock does
    /// wireless Android Auto successfully on this hardware, so that shape is proven and copying it
    /// is the right default.
    ///
    /// openauto instead puts SerialPort `0x1101` in the class list too. That is the documented
    /// first hedge if a phone declines to offer wireless AA (`03_WIRELESS.md` §6b): set this to
    /// `Some(0x1101)` and retest. It is a lever rather than a default precisely because the two
    /// references disagree and only one of them is known to work on this box.
    pub extra_class_uuid16: Option<u16>,
}

impl ServiceRecord<'_> {
    /// Encode the record body: a single `DE_SEQ8` sequence of attribute/value pairs.
    ///
    /// Attribute order is ascending by id, which is what the SDP spec requires of a
    /// ServiceAttributeResponse and what the captured CarPlay record does.
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(96);

        let mut v = Vec::new();
        put_uint32(&mut v, self.handle);
        put_attr(&mut body, ATTR_RECORD_HANDLE, &v);

        // ServiceClassIDList: a sequence holding the one 128-bit class UUID.
        let mut class = Vec::with_capacity(20);
        class.push(DE_UUID128);
        class.extend_from_slice(&self.uuid128);
        if let Some(extra) = self.extra_class_uuid16 {
            put_uuid16(&mut class, extra);
        }
        v.clear();
        put_seq8(&mut v, &class);
        put_attr(&mut body, ATTR_SERVICE_CLASS_ID_LIST, &v);

        // ProtocolDescriptorList: seq{ seq{L2CAP}, seq{RFCOMM, channel} }
        let mut l2cap = Vec::new();
        put_uuid16(&mut l2cap, UUID16_L2CAP);
        let mut rfcomm = Vec::new();
        put_uuid16(&mut rfcomm, UUID16_RFCOMM);
        rfcomm.push(DE_UINT8);
        rfcomm.push(self.rfcomm_channel);
        let mut protos = Vec::new();
        put_seq8(&mut protos, &l2cap);
        put_seq8(&mut protos, &rfcomm);
        v.clear();
        put_seq8(&mut v, &protos);
        put_attr(&mut body, ATTR_PROTOCOL_DESCRIPTOR_LIST, &v);

        // BrowseGroupList: seq{ PublicBrowseGroup } — without this the phone's browse never
        // reaches the record, however correct the rest of it is.
        let mut browse = Vec::new();
        put_uuid16(&mut browse, UUID16_PUBLIC_BROWSE_GROUP);
        v.clear();
        put_seq8(&mut v, &browse);
        put_attr(&mut body, ATTR_BROWSE_GROUP_LIST, &v);

        // BluetoothProfileDescriptorList: seq{ seq{ SerialPort, v1.0 } }
        let mut profile = Vec::new();
        put_uuid16(&mut profile, UUID16_SERIAL_PORT);
        put_uint16(&mut profile, 0x0100); // version 1.0
        let mut profiles = Vec::new();
        put_seq8(&mut profiles, &profile);
        v.clear();
        put_seq8(&mut v, &profiles);
        put_attr(&mut body, ATTR_PROFILE_DESCRIPTOR_LIST, &v);

        // ServiceName
        let name = self.name.as_bytes();
        // The OUTER sequence is what actually bounds this, not the name element. The fixed part of
        // the body is FIXED_BODY_LEN bytes, so a name that fits the name element (<=255) can still
        // overflow the outer `put_seq8` and panic there instead of here. Guard the real bound.
        assert!(
            name.len() <= MAX_SERVICE_NAME,
            "service name is {} bytes; the outer sequence allows at most {MAX_SERVICE_NAME}",
            name.len()
        );
        v.clear();
        v.push(DE_TEXT8);
        v.push(name.len() as u8);
        v.extend_from_slice(name);
        put_attr(&mut body, ATTR_SERVICE_NAME, &v);

        let mut out = Vec::with_capacity(body.len() + 2);
        put_seq8(&mut out, &body);
        out
    }
}

/// The two BR/EDR **audio-profile** records this box advertises — HFP Hands-Free and HSP Headset.
///
/// WHY A SECOND SHAPE AT ALL. [`ServiceRecord`] encodes exactly one shape: a single UUID128 service
/// class, L2CAP+RFCOMM, one browse group, a SerialPort profile descriptor, a name. Its
/// [`RFCOMM_CHANNEL_OFFSET`] is a public constant that is only sound *because* that shape is fixed,
/// and its own doc says so. An audio-profile record has a two-UUID16 class list, its own profile
/// UUID and version, and a profile-specific attribute AFTER the name — so folding it into
/// `ServiceRecord` would either move that offset or bury the shipping records under optionality.
/// A sibling encoder sharing the `put_*` primitives keeps every byte of the iAP2 and Android Auto
/// records exactly where it was (asserted by `existing_records_are_untouched_by_the_hf_addition`).
///
/// WHY THEY EXIST. gearhead 17.5 will not start wireless setup unless
/// `BluetoothProfile.HEADSET.getDevicesMatchingConnectionStates({CONNECTED, CONNECTING})` contains
/// the head unit — the phone must be the audio gateway and we the headset side (`pcl.java:80`,
/// `kzt.java:56-64`, `pco.java:24-29`, `ozb.java:139`; the failure event is
/// `WIRELESS_SETUP_FAILED_TO_START_NO_HFP_FROM_HU_PRESENCE`). Advertising BOTH records is what lets
/// a phone whose `PhonePolicy` auto-connects to a bonded headset-class device dial US, on either
/// profile; the wireless crate's `hfp_hf` module is the other half and dials the phone.
/// See `docs/androidauto/03_WIRELESS.md` §6b/§6d.

/// The profile-specific attribute that trails the service name in an audio-profile record.
#[derive(Clone, Copy, Debug)]
enum TrailingAttr {
    /// `0x0311 SupportedFeatures`, a 16-bit bitmap (HFP).
    Uint16 { id: u16, value: u16 },
    /// `0x0302 RemoteAudioVolumeControl`, a boolean (HSP).
    Bool { id: u16, value: bool },
}

/// The shared body encoder for both audio-profile records. Attributes ascend by id:
/// `0x0000, 0x0001, 0x0004, 0x0005, 0x0009, 0x0100, <trailing>` — and the trailing ids (`0x0302`,
/// `0x0311`) are both above `0x0100`, which is what makes "name then trailing" the correct order.
fn encode_audio_record(
    handle: u32,
    class_uuids: &[u16],
    rfcomm_channel: u8,
    profile_uuid: u16,
    profile_version: u16,
    name: &str,
    trailing: TrailingAttr,
) -> Vec<u8> {
    let mut body = Vec::with_capacity(96);
    let mut v = Vec::new();

    put_uint32(&mut v, handle);
    put_attr(&mut body, ATTR_RECORD_HANDLE, &v);

    // ServiceClassIDList. Every UUID here is separately searchable, which is the point of listing
    // GenericAudio as well: a phone that searches only `0x1203` must still find us.
    let mut class = Vec::with_capacity(3 * class_uuids.len());
    for u in class_uuids {
        put_uuid16(&mut class, *u);
    }
    v.clear();
    put_seq8(&mut v, &class);
    put_attr(&mut body, ATTR_SERVICE_CLASS_ID_LIST, &v);

    // ProtocolDescriptorList: seq{ seq{L2CAP}, seq{RFCOMM, channel} } — identical shape to
    // ServiceRecord's, which is why one channel scan works on every record this crate builds.
    let mut l2cap = Vec::new();
    put_uuid16(&mut l2cap, UUID16_L2CAP);
    let mut rfcomm = Vec::new();
    put_uuid16(&mut rfcomm, UUID16_RFCOMM);
    rfcomm.push(DE_UINT8);
    rfcomm.push(rfcomm_channel);
    let mut protos = Vec::new();
    put_seq8(&mut protos, &l2cap);
    put_seq8(&mut protos, &rfcomm);
    v.clear();
    put_seq8(&mut v, &protos);
    put_attr(&mut body, ATTR_PROTOCOL_DESCRIPTOR_LIST, &v);

    // BrowseGroupList: seq{ PublicBrowseGroup }.
    let mut browse = Vec::new();
    put_uuid16(&mut browse, UUID16_PUBLIC_BROWSE_GROUP);
    v.clear();
    put_seq8(&mut v, &browse);
    put_attr(&mut body, ATTR_BROWSE_GROUP_LIST, &v);

    // BluetoothProfileDescriptorList: seq{ seq{ <profile>, version } }. The profile UUID is the
    // profile's own, NOT SerialPort — a stack that matched the class list can still decline on a
    // wrong profile descriptor.
    let mut profile = Vec::new();
    put_uuid16(&mut profile, profile_uuid);
    put_uint16(&mut profile, profile_version);
    let mut profiles = Vec::new();
    put_seq8(&mut profiles, &profile);
    v.clear();
    put_seq8(&mut v, &profiles);
    put_attr(&mut body, ATTR_PROFILE_DESCRIPTOR_LIST, &v);

    // The trailing attribute is encoded BEFORE the name is appended, only so its length is known
    // for the bound check below; it is appended after, keeping ids ascending.
    let mut tail = Vec::new();
    match trailing {
        TrailingAttr::Uint16 { id, value } => {
            v.clear();
            put_uint16(&mut v, value);
            put_attr(&mut tail, id, &v);
        }
        TrailingAttr::Bool { id, value } => {
            v.clear();
            v.push(DE_BOOL);
            v.push(u8::from(value));
            put_attr(&mut tail, id, &v);
        }
    }

    // ServiceName. The OUTER sequence is what bounds this, not the name element: a name that fits
    // the element (<=255) can still overflow `put_seq8` and panic there instead of here, which is
    // the same trap `ServiceRecord::encode` guards. The bound is computed rather than a constant
    // because it depends on the class-list length and the trailing attribute.
    let name_b = name.as_bytes();
    let overhead = body.len() + 3 + 2 + tail.len(); // body so far + attr id + text hdr + trailing
    let max_name = u8::MAX as usize - overhead;
    assert!(
        name_b.len() <= max_name,
        "audio-profile service name is {} bytes; the outer sequence allows at most {max_name}",
        name_b.len()
    );
    v.clear();
    v.push(DE_TEXT8);
    v.push(name_b.len() as u8);
    v.extend_from_slice(name_b);
    put_attr(&mut body, ATTR_SERVICE_NAME, &v);
    body.extend_from_slice(&tail);

    let mut out = Vec::with_capacity(body.len() + 2);
    put_seq8(&mut out, &body);
    out
}

/// The HFP **Hands-Free** service record.
#[derive(Clone, Debug)]
pub struct HandsFreeRecord<'a> {
    pub handle: u32,
    pub rfcomm_channel: u8,
    pub name: &'a str,
    /// HFP profile version in the `BluetoothProfileDescriptorList`. `0x0107` = 1.7.
    pub profile_version: u16,
    /// Attribute `0x0311 SupportedFeatures`.
    ///
    /// `0x003F` mirrors the `AT+BRSF=63` the stock box's `hfpd` sends
    /// (`aa_full_session_adapter_20260315.txt:536`), which is the only known-good value against
    /// this phone. Note the two bitmaps are NOT the same field: SDP bit 5 is Wide-Band Speech where
    /// BRSF bit 5 is Enhanced Call Status. We never open SCO, so an over-claimed WBS bit costs
    /// nothing — but it is a field rather than a constant precisely so it can be cleared to
    /// `0x001F` without touching the encoder if a phone ever acts on it.
    pub supported_features: u16,
}

/// The HSP **Headset** (HS) service record.
///
/// Carried alongside the HF one because the two profiles reach gearhead's gate by DIFFERENT routes
/// in AOSP, and only one of them needs an AT dialogue: an inbound RFCOMM connection to the phone's
/// HSP AG channel opens the service level immediately (`bta_ag_act.cc:533-540` — the SLC timer is
/// armed only `if conn_service == BTA_AG_HFP`, otherwise `bta_ag_svc_conn_open` fires
/// `BTA_AG_CONN_EVT` → `BTHF_CONNECTION_STATE_SLC_CONNECTED` → HeadsetStateMachine `mConnected`),
/// whereas the HFP AG channel needs the SLC (or its timer) first. Both public dongles use the HSP
/// route with no AT traffic; stock uses the HFP one. We advertise both and try both.
#[derive(Clone, Debug)]
pub struct HeadsetRecord<'a> {
    pub handle: u32,
    pub rfcomm_channel: u8,
    pub name: &'a str,
    /// HSP profile version. `0x0102` = 1.2.
    pub profile_version: u16,
    /// Attribute `0x0302 RemoteAudioVolumeControl`. `false`: this headset never carries audio, so
    /// claiming remote volume control would invite `+VGS`/`+VGM` traffic for a speaker we do not
    /// have.
    pub remote_audio_volume_control: bool,
}

/// Byte offset of the RFCOMM channel inside a [`HandsFreeRecord`] or a [`HeadsetRecord`]. Both have
/// a two-UUID16 class list, so the offset is the same for either; it is fixed for any service name
/// for the same reason as [`RFCOMM_CHANNEL_OFFSET`] — `ServiceName` comes after it — and pinned by
/// `audio_profile_channel_lands_at_the_documented_offset`.
pub const AUDIO_PROFILE_RFCOMM_CHANNEL_OFFSET: usize = 37;

impl HandsFreeRecord<'_> {
    pub fn encode(&self) -> Vec<u8> {
        encode_audio_record(
            self.handle,
            &[UUID16_HANDSFREE, UUID16_GENERIC_AUDIO],
            self.rfcomm_channel,
            UUID16_HANDSFREE,
            self.profile_version,
            self.name,
            TrailingAttr::Uint16 { id: ATTR_HFP_SUPPORTED_FEATURES, value: self.supported_features },
        )
    }
}

impl HeadsetRecord<'_> {
    pub fn encode(&self) -> Vec<u8> {
        encode_audio_record(
            self.handle,
            &[UUID16_HEADSET, UUID16_GENERIC_AUDIO],
            self.rfcomm_channel,
            UUID16_HEADSET,
            self.profile_version,
            self.name,
            TrailingAttr::Bool {
                id: ATTR_HSP_REMOTE_VOLUME_CONTROL,
                value: self.remote_audio_volume_control,
            },
        )
    }
}

/// Parse a UUID string (`4de17a00-52cb-11e6-bdf4-0800200c9a66`) into 16 big-endian bytes.
///
/// A `const` byte array would be shorter, but a UUID written as digits is checkable against the
/// documentation by eye and a byte array is not — and getting it wrong yields a service the phone
/// simply never matches, with no error anywhere.
pub const fn uuid128_from_u128(v: u128) -> [u8; 16] {
    v.to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 89-byte iAP2 record captured from a real CCPA (`sdp_server::RECORD_TEMPLATE`), with
    /// RFCOMM channel 1. This is the oracle: the encoder is only trustworthy for records nobody has
    /// captured if it reproduces the one that IS captured, exactly.
    #[rustfmt::skip]
    const CAPTURED_IAP2: [u8; 89] = [
        0x35, 0x57,
        0x09, 0x00, 0x00,  0x0a, 0x00, 0x01, 0x00, 0x00,
        0x09, 0x00, 0x01,  0x35, 0x11, 0x1c,
            0x00, 0x00, 0x00, 0x00, 0xde, 0xca, 0xfa, 0xde,
            0xde, 0xca, 0xde, 0xaf, 0xde, 0xca, 0xca, 0xff,
        0x09, 0x00, 0x04,  0x35, 0x0c,
            0x35, 0x03, 0x19, 0x01, 0x00,
            0x35, 0x05, 0x19, 0x00, 0x03, 0x08, 0x01,
        0x09, 0x00, 0x05,  0x35, 0x03, 0x19, 0x10, 0x02,
        0x09, 0x00, 0x09,  0x35, 0x08, 0x35, 0x06, 0x19, 0x11, 0x01, 0x09, 0x01, 0x00,
        0x09, 0x01, 0x00,  0x25, 0x0e,
            0x57, 0x69, 0x72, 0x65, 0x6c, 0x65, 0x73, 0x73, 0x20,
            0x69, 0x41, 0x50, 0x76, 0x32,
    ];

    fn iap2_record(chan: u8) -> ServiceRecord<'static> {
        ServiceRecord {
            handle: 0x0001_0000,
            uuid128: uuid128_from_u128(0x00000000_deca_fade_deca_deafdecacaff),
            rfcomm_channel: chan,
            name: "Wireless iAPv2",
            extra_class_uuid16: None,
        }
    }

    /// THE test. If this fails, nothing else in this module can be trusted.
    #[test]
    fn carplay_record_matches_the_captured_bytes() {
        assert_eq!(iap2_record(1).encode(), CAPTURED_IAP2.to_vec());
    }

    /// The old code patched byte 48 to set the channel. Prove the encoder puts it there too, so the
    /// two agree about more than just the default.
    #[test]
    fn rfcomm_channel_lands_at_the_documented_offset() {
        for chan in [1u8, 2, 7, 30] {
            let enc = iap2_record(chan).encode();
            assert_eq!(enc[RFCOMM_CHANNEL_OFFSET], chan, "channel byte for {chan}");
            let mut expect = CAPTURED_IAP2;
            expect[RFCOMM_CHANNEL_OFFSET] = chan;
            assert_eq!(enc, expect.to_vec());
        }
    }

    /// What a different service name actually changes.
    ///
    /// It does NOT move the RFCOMM channel byte: `ServiceName` is the LAST attribute, so everything
    /// before it — including the protocol descriptor list — keeps its offset. The first version of
    /// this test asserted otherwise and failed, which is the point of having it.
    ///
    /// What the name DOES change is the outer sequence length prefix, and that is the byte a
    /// hand-edited copy of the captured array gets wrong: the record still parses as a truncated or
    /// over-long sequence, and the phone declines to match a service it cannot read.
    #[test]
    fn a_different_name_changes_the_length_prefix_but_not_the_channel_offset() {
        let short = ServiceRecord { name: "AA", ..iap2_record(3) }.encode();
        let long = ServiceRecord { name: "Android Auto Wireless", ..iap2_record(3) }.encode();

        assert_ne!(short.len(), long.len(), "a longer name makes a longer record");
        assert_eq!(short[1] as usize, short.len() - 2, "outer length tracks the body");
        assert_eq!(long[1] as usize, long.len() - 2, "outer length tracks the body");

        // The channel byte holds its offset regardless of the name.
        assert_eq!(short[RFCOMM_CHANNEL_OFFSET], 3);
        assert_eq!(long[RFCOMM_CHANNEL_OFFSET], 3);
    }

    /// The hedge lever must actually widen the ServiceClassIDList, and must not disturb the record
    /// when it is off (which is the shipping default, matching stock).
    #[test]
    fn the_extra_class_uuid_lever_appends_to_the_class_list() {
        let off = iap2_record(1).encode();
        let on = ServiceRecord { extra_class_uuid16: Some(0x1101), ..iap2_record(1) }.encode();
        assert_eq!(off, CAPTURED_IAP2.to_vec(), "off must remain byte-identical to the capture");
        assert_eq!(on.len(), off.len() + 3, "a UUID16 element is 3 bytes");
        // the class-list sequence header grows by 3 too
        assert_eq!(on[13], 0x35);
        assert_eq!(on[14], off[14] + 3);
    }

    /// The name bound must reflect the OUTER sequence, not the name element. A 200-byte name fits
    /// the name element but overflows the record; it must be rejected here with a useful message
    /// rather than panicking deeper in `put_seq8`.
    #[test]
    fn service_name_bound_matches_the_outer_sequence() {
        assert_eq!(MAX_SERVICE_NAME, 182);
        let longest = "x".repeat(MAX_SERVICE_NAME);
        let enc = ServiceRecord { name: &longest, ..iap2_record(1) }.encode();
        assert_eq!(enc[1] as usize, enc.len() - 2);
        assert_eq!(enc.len(), 2 + u8::MAX as usize);
    }

    #[test]
    #[should_panic(expected = "the outer sequence allows at most")]
    fn an_over_long_service_name_is_refused_with_the_real_bound() {
        let too_long = "x".repeat(MAX_SERVICE_NAME + 1);
        let _ = ServiceRecord { name: &too_long, ..iap2_record(1) }.encode();
    }

    fn hf_record(chan: u8) -> HandsFreeRecord<'static> {
        HandsFreeRecord {
            handle: 0x0001_0002,
            rfcomm_channel: chan,
            name: "Hands-Free",
            profile_version: 0x0107,
            supported_features: 0x003F,
        }
    }

    /// THE HF test: the whole record, byte for byte. Written out rather than recomputed from the
    /// encoder, so a change to any length prefix, attribute order or UUID fails here instead of on
    /// a phone that silently declines to connect.
    #[test]
    fn hands_free_record_matches_the_expected_bytes() {
        #[rustfmt::skip]
        let expect: Vec<u8> = vec![
            0x35, 0x4e,
            0x09, 0x00, 0x00,  0x0a, 0x00, 0x01, 0x00, 0x02,
            0x09, 0x00, 0x01,  0x35, 0x06, 0x19, 0x11, 0x1e, 0x19, 0x12, 0x03,
            0x09, 0x00, 0x04,  0x35, 0x0c,
                0x35, 0x03, 0x19, 0x01, 0x00,
                0x35, 0x05, 0x19, 0x00, 0x03, 0x08, 0x05,
            0x09, 0x00, 0x05,  0x35, 0x03, 0x19, 0x10, 0x02,
            0x09, 0x00, 0x09,  0x35, 0x08, 0x35, 0x06, 0x19, 0x11, 0x1e, 0x09, 0x01, 0x07,
            0x09, 0x01, 0x00,  0x25, 0x0a,
                0x48, 0x61, 0x6e, 0x64, 0x73, 0x2d, 0x46, 0x72, 0x65, 0x65,
            0x09, 0x03, 0x11,  0x09, 0x00, 0x3f,
        ];
        let enc = hf_record(HFP_HF_RFCOMM_CHANNEL).encode();
        assert_eq!(enc, expect);
        assert_eq!(enc[1] as usize, enc.len() - 2, "outer length tracks the body");
        assert_eq!(enc.len(), 80);
    }

    /// The class list must carry Handsfree `0x111E` — the HF side. `0x111F` is the AUDIO GATEWAY,
    /// which is what the PHONE advertises; claiming it would make the phone see a second AG and
    /// never satisfy gearhead's `BluetoothProfile.HEADSET` gate.
    #[test]
    fn the_hf_record_claims_the_hands_free_class_never_the_gateway() {
        let enc = hf_record(5).encode();
        assert_eq!(UUID16_HANDSFREE, 0x111E);
        assert_eq!(UUID16_HANDSFREE_AG, 0x111F);
        // class list: outer(2) + handle attr(8) + attr id(3) + seq hdr(2) = 15
        assert_eq!(&enc[15..21], &[0x19, 0x11, 0x1e, 0x19, 0x12, 0x03]);
        assert!(!enc.windows(3).any(|w| w == [0x19, 0x11, 0x1f]), "must not advertise the AG class");
    }

    #[test]
    fn audio_profile_channel_lands_at_the_documented_offset() {
        for chan in [1u8, 5, 9, 30] {
            let enc = hf_record(chan).encode();
            assert_eq!(enc[AUDIO_PROFILE_RFCOMM_CHANNEL_OFFSET], chan, "channel byte for {chan}");
        }
        // and it does not move with the name, because ServiceName comes after it
        let long = HandsFreeRecord { name: "Hands-Free Unit (CarLink)", ..hf_record(5) }.encode();
        assert_eq!(long[AUDIO_PROFILE_RFCOMM_CHANNEL_OFFSET], 5);
        assert_eq!(long[1] as usize, long.len() - 2);
    }

    /// `SupportedFeatures` is the last attribute (0x0311 > 0x0100), and its value is the one that
    /// mirrors the stock box's `AT+BRSF=63`.
    #[test]
    fn supported_features_is_the_last_attribute_and_matches_brsf_63() {
        let enc = hf_record(5).encode();
        assert_eq!(&enc[enc.len() - 6..], &[0x09, 0x03, 0x11, 0x09, 0x00, 0x3f]);
        assert_eq!(0x003Fu16, 63);
        let cleared = HandsFreeRecord { supported_features: 0x001F, ..hf_record(5) }.encode();
        assert_eq!(&cleared[cleared.len() - 6..], &[0x09, 0x03, 0x11, 0x09, 0x00, 0x1f]);
        assert_eq!(cleared.len(), enc.len(), "the lever must not resize the record");
    }

    /// Adding the HF shape must not have moved one byte of the two records that already ship. This
    /// is the guard the module's own doc comment demands of any edit to it.
    #[test]
    fn existing_records_are_untouched_by_the_hf_addition() {
        assert_eq!(iap2_record(1).encode(), CAPTURED_IAP2.to_vec());
        assert_eq!(RFCOMM_CHANNEL_OFFSET, 48);
        assert_eq!(FIXED_BODY_LEN, 73);
        assert_eq!(MAX_SERVICE_NAME, 182);
        let aa = ServiceRecord {
            handle: 0x0001_0001,
            uuid128: uuid128_from_u128(0x4de17a00_52cb_11e6_bdf4_0800200c9a66),
            rfcomm_channel: AAP_RFCOMM_CHANNEL,
            name: "Wireless Android Auto Protocol",
            extra_class_uuid16: None,
        }
        .encode();
        assert_eq!(aa.len(), 105);
        assert_eq!(aa[RFCOMM_CHANNEL_OFFSET], 4);
    }

    #[test]
    #[should_panic(expected = "the outer sequence allows at most")]
    fn an_over_long_hf_service_name_is_refused_with_the_real_bound() {
        // The HF record is 80 bytes with a 10-byte name, i.e. 70 fixed, so the longest name the
        // outer 8-bit sequence admits is 255 - 68 = 187. `longest_hf_name_still_encodes` pins the
        // accepted side of that boundary; this pins the rejected side, with the message naming the
        // real bound rather than panicking deeper in `put_seq8`.
        let too_long = "x".repeat(188);
        let _ = HandsFreeRecord { name: &too_long, ..hf_record(5) }.encode();
    }

    #[test]
    fn longest_hf_name_still_encodes() {
        let longest = "x".repeat(187);
        let enc = HandsFreeRecord { name: &longest, ..hf_record(5) }.encode();
        assert_eq!(enc.len(), 2 + u8::MAX as usize);
        assert_eq!(enc[1], u8::MAX);
        assert_eq!(enc[AUDIO_PROFILE_RFCOMM_CHANNEL_OFFSET], 5);
    }

    fn hs_record(chan: u8) -> HeadsetRecord<'static> {
        HeadsetRecord {
            handle: 0x0001_0003,
            rfcomm_channel: chan,
            name: "Headset",
            profile_version: 0x0102,
            remote_audio_volume_control: false,
        }
    }

    /// THE HSP test, byte for byte. The `0x0302` boolean is the one element type no other record in
    /// this module emits, so a wrong descriptor byte would go unnoticed everywhere else.
    #[test]
    fn headset_record_matches_the_expected_bytes() {
        #[rustfmt::skip]
        let expect: Vec<u8> = vec![
            0x35, 0x4a,
            0x09, 0x00, 0x00,  0x0a, 0x00, 0x01, 0x00, 0x03,
            0x09, 0x00, 0x01,  0x35, 0x06, 0x19, 0x11, 0x08, 0x19, 0x12, 0x03,
            0x09, 0x00, 0x04,  0x35, 0x0c,
                0x35, 0x03, 0x19, 0x01, 0x00,
                0x35, 0x05, 0x19, 0x00, 0x03, 0x08, 0x06,
            0x09, 0x00, 0x05,  0x35, 0x03, 0x19, 0x10, 0x02,
            0x09, 0x00, 0x09,  0x35, 0x08, 0x35, 0x06, 0x19, 0x11, 0x08, 0x09, 0x01, 0x02,
            0x09, 0x01, 0x00,  0x25, 0x07, 0x48, 0x65, 0x61, 0x64, 0x73, 0x65, 0x74,
            0x09, 0x03, 0x02,  0x28, 0x00,
        ];
        let enc = hs_record(HSP_HS_RFCOMM_CHANNEL).encode();
        assert_eq!(enc, expect);
        assert_eq!(enc[1] as usize, enc.len() - 2);
        assert_eq!(enc[AUDIO_PROFILE_RFCOMM_CHANNEL_OFFSET], HSP_HS_RFCOMM_CHANNEL);
    }

    /// The HS record must claim the HEADSET class `0x1108`, never the AG `0x1112` — same reasoning
    /// as the HF/AG split, and the phone's own HSP AG record is what `sdp_client` dials.
    #[test]
    fn the_hs_record_claims_the_headset_class_never_the_gateway() {
        let enc = hs_record(6).encode();
        assert_eq!(UUID16_HEADSET, 0x1108);
        assert_eq!(UUID16_HEADSET_AG, 0x1112);
        assert_eq!(&enc[15..21], &[0x19, 0x11, 0x08, 0x19, 0x12, 0x03]);
        assert!(!enc.windows(3).any(|w| w == [0x19, 0x11, 0x12]), "must not advertise the AG class");
    }

    /// `RemoteAudioVolumeControl` is a BOOLEAN (descriptor 0x28), not a uint8, and it is false: we
    /// never carry audio, so claiming volume control would invite +VGS/+VGM for a nonexistent
    /// speaker. Flipping it must change exactly one byte.
    #[test]
    fn remote_audio_volume_control_is_a_false_boolean() {
        let off = hs_record(6).encode();
        let on = HeadsetRecord { remote_audio_volume_control: true, ..hs_record(6) }.encode();
        assert_eq!(&off[off.len() - 5..], &[0x09, 0x03, 0x02, 0x28, 0x00]);
        assert_eq!(&on[on.len() - 5..], &[0x09, 0x03, 0x02, 0x28, 0x01]);
        assert_eq!(off.len(), on.len());
    }

    /// The two audio-profile records must not collide on an RFCOMM channel, with each other or with
    /// the two projection records — the kernel would return EADDRINUSE on the second bind and the
    /// affected service would silently never be connectable.
    #[test]
    fn every_advertised_rfcomm_channel_is_distinct() {
        let chans = [
            IAP2_RFCOMM_CHANNEL,
            AAP_RFCOMM_CHANNEL,
            HFP_HF_RFCOMM_CHANNEL,
            HSP_HS_RFCOMM_CHANNEL,
        ];
        let mut sorted = chans.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), chans.len(), "duplicate RFCOMM channel in {chans:?}");
    }

    /// The AA record we will actually serve. Locked down so a later edit cannot quietly change the
    /// UUID: this is the value from docs/androidauto/03_WIRELESS.md §2b.
    #[test]
    fn android_auto_record_carries_the_projection_uuid() {
        let rec = ServiceRecord {
            handle: 0x0001_0001,
            uuid128: uuid128_from_u128(0x4de17a00_52cb_11e6_bdf4_0800200c9a66),
            rfcomm_channel: AAP_RFCOMM_CHANNEL,
            name: "Wireless Android Auto Protocol",
            extra_class_uuid16: None,
        };
        let enc = rec.encode();
        // UUID128 element begins after: outer(2) + handle attr(8) + class-list attr id(3) + seq(2)
        let uuid_at = 2 + 8 + 3 + 2;
        assert_eq!(enc[uuid_at], DE_UUID128);
        assert_eq!(
            &enc[uuid_at + 1..uuid_at + 17],
            &[0x4d, 0xe1, 0x7a, 0x00, 0x52, 0xcb, 0x11, 0xe6,
              0xbd, 0xf4, 0x08, 0x00, 0x20, 0x0c, 0x9a, 0x66]
        );
        assert_eq!(enc[1] as usize, enc.len() - 2);
    }
}
