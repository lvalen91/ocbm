//! A/V forward framing — the transforms the C `_Carplayd*` hooks apply before writing decoded/decrypted
//! elementary streams to carlink_linux over the IPC seam (`proto-core::ports`, carplayd/IPC_CONTRACT.md):
//!
//! - **Video** :9001 — AVCC (length-prefixed NALs) → Annex-B (`00 00 00 01` start codes).
//! - **Media audio** :9002 — wireless: raw AAC-LC AU → ADTS frame (self-describing rate/channels);
//!   wired: raw PCM 48k/16/stereo forwarded verbatim (fixed format, no framing).
//! - **Voice audio** :9003 — every AU tagged `[rate:u32 BE][ch:u16 BE][atype:u8][len:u32 BE][AU]`, because the
//!   one persistent socket carries mixed formats (wired PCM: telephony/Siri 16k mono, alert/nav 48k
//!   stereo; wireless: AAC-ELD).
//!
//! These mirror exactly what carlink_linux's `carplayd` source already consumes; verified by format
//! round-trips here.

/// Convert an AVCC access unit (each NAL prefixed by an `nal_length_size`-byte big-endian length) to
/// Annex-B (`00 00 00 01` start code before each NAL). Malformed trailing bytes are dropped.
pub fn annexb_from_avcc(au: &[u8], nal_length_size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(au.len() + 16);
    let mut i: usize = 0;
    // Every add here is checked. The box target is armv7 (`usize` = 32 bits) and the release profile
    // sets `panic = "abort"` with overflow-checks off, so `i + len` on a 4-byte length field of
    // 0xFFFFFFFF wraps to a value BELOW `au.len()`, the guard passes, and `&au[i..i + len]` is
    // evaluated with start > end — an abort, and this box needs a physical power cycle. Verified by
    // execution on the 32-bit target: `au = [FF FF FF FF]`, nal_length_size 4, gives `au[4..3]`.
    // Harmless on a 64-bit host, which is why it survived the test suite and every dev run.
    while i.checked_add(nal_length_size).is_some_and(|e| e <= au.len()) {
        let mut len = 0usize;
        for &b in &au[i..i + nal_length_size] {
            len = (len << 8) | b as usize;
        }
        i += nal_length_size;
        let Some(end) = i.checked_add(len) else { break };
        if len == 0 || end > au.len() {
            break;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&au[i..end]);
        i = end;
    }
    out
}

/// MPEG-4 sampling-frequency index for the ADTS header.
fn samplerate_index(rate: u32) -> Option<u8> {
    Some(match rate {
        96000 => 0,
        88200 => 1,
        64000 => 2,
        48000 => 3,
        44100 => 4,
        32000 => 5,
        24000 => 6,
        22050 => 7,
        16000 => 8,
        12000 => 9,
        11025 => 10,
        8000 => 11,
        7350 => 12,
        _ => return None,
    })
}

/// Wrap a raw AAC-LC access unit in a 7-byte ADTS header (profile AAC-LC). Returns `None` for an
/// unsupported sample rate.
pub fn adts_from_aac_lc(au: &[u8], rate: u32, channels: u8) -> Option<Vec<u8>> {
    let sfi = samplerate_index(rate)?;
    let frame_len = 7 + au.len();
    if frame_len > 0x1FFF {
        return None;
    }
    let chan = channels & 0x7;
    let mut out = Vec::with_capacity(frame_len);
    out.push(0xFF);
    out.push(0xF1); // MPEG-4, layer 0, protection absent
    // profile = AAC-LC (object type 2 → "profile" field = 1)
    out.push((1 << 6) | (sfi << 2) | (chan >> 2));
    out.push(((chan & 0x3) << 6) | ((frame_len >> 11) as u8 & 0x3));
    out.push((frame_len >> 3) as u8);
    out.push(((frame_len as u8 & 0x7) << 5) | 0x1F);
    out.push(0xFC);
    out.extend_from_slice(au);
    Some(out)
}

/// Tag a raw voice AU (AAC-ELD, or wired PCM) for the :9003 stream:
/// `[rate:u32 BE][ch:u16 BE][atype:u8][len:u32 BE][AU]`.
///
/// `atype` is the CarPlay audio PURPOSE — 0 media, 1 telephony, 2 speechRecognition, 3 alert,
/// 4 default/absent, 5 compatibility — matching `session.rs`'s mapping and the seam-v2 `SEAM_FORMAT` byte.
///
/// It is carried here because :9003 multiplexes EVERY non-media stream onto one socket, and the
/// three purposes a consumer most needs to separate — telephony, speechRecognition and the Siri
/// `default` downlink — are all negotiated as AAC-ELD 16 kHz mono. Without this byte they are
/// byte-for-byte indistinguishable, and `(rate, channels)` yields one bit of information where five
/// purposes are needed. A consumer forced to guess routes call audio to the assistant output, which
/// on an Android Automotive head unit means the wrong volume group and a call whose volume the user
/// cannot adjust.
///
/// WIRE CHANGE: this inserts one byte between `ch` and `len` versus the original 10-byte header.
/// Consumers must be updated in the same change. Spec: `docs/carplay/01_OCBM_PROTOCOL.md`
/// ("The `:9003` voice seam tag"). The Android consumer lives in the sibling gm_ccpa repo.
pub fn tag_voice(au: &[u8], rate: u32, channels: u16, atype: u8) -> Vec<u8> {
    let mut v = Vec::with_capacity(11 + au.len());
    v.extend_from_slice(&rate.to_be_bytes());
    v.extend_from_slice(&channels.to_be_bytes());
    v.push(atype);
    v.extend_from_slice(&(au.len() as u32).to_be_bytes());
    v.extend_from_slice(au);
    v
}

// ---- voice tag v2 (plan v5.8 V1a; feature `voice-tag-v2`, enabled only by carplay-jni) ----------
//
// Every v2 frame keeps the 11-byte v1 head `[rate u32 BE][ch u16 BE][atype u8][len u32 BE]`, so a
// reader always knows how many bytes follow. Two head forms are new:
//
// - **AU, v2**: `atype | 0x80`, and `len` covers the 23-byte extension PLUS the AU:
//   `[scid u64 BE][rx_us u64 BE][seq u16 BE][rtp_ts u32 BE][stream_type u8][AU]`.
//   `rx_us` is `session::voice_clock_us()` when the RTP packet was received; `seq`/`rtp_ts` are the
//   packet's RTP header fields (bytes 2..4 and 4..8), which tag v1 dropped.
// - **control**, `atype = 0xFF`: rate/ch are the stream's values; the payload is
//   `[kind u8][write_us u64 BE]…` (see [`voice_begin_v2`], [`voice_end_v2`]).
//
// Every multi-byte field is BIG endian, like the v1 head (the seam-v2 SEAM_* messages are a
// different, little-endian wire and are not touched).

/// Bytes between the v2 head and the AU.
#[cfg(feature = "voice-tag-v2")]
pub const VOICE_V2_EXT_LEN: usize = 23;
/// `atype` bit marking a v2 AU frame (the low 7 bits are the v1 purpose code).
#[cfg(feature = "voice-tag-v2")]
pub const VOICE_V2_AU_FLAG: u8 = 0x80;
/// `atype` of a v2 control frame.
#[cfg(feature = "voice-tag-v2")]
pub const VOICE_V2_CONTROL: u8 = 0xFF;
/// Control kinds.
#[cfg(feature = "voice-tag-v2")]
pub const VOICE_V2_BEGIN: u8 = 1;
#[cfg(feature = "voice-tag-v2")]
pub const VOICE_V2_END: u8 = 2;
/// END reasons — the cause stored with the stream's liveness flag before it is cleared.
#[cfg(feature = "voice-tag-v2")]
pub const END_UNKNOWN: u8 = 0;
#[cfg(feature = "voice-tag-v2")]
pub const END_TEARDOWN: u8 = 1;
#[cfg(feature = "voice-tag-v2")]
pub const END_SESSION_RESET: u8 = 2;
#[cfg(feature = "voice-tag-v2")]
pub const END_SUPERSEDED: u8 = 3;
#[cfg(feature = "voice-tag-v2")]
pub const END_SOCKET_ERROR: u8 = 4;

/// One v2 AU frame: v1 head with `atype | 0x80`, the 23-byte extension, then the AU.
#[cfg(feature = "voice-tag-v2")]
#[allow(clippy::too_many_arguments)]
pub fn tag_voice_v2(
    au: &[u8],
    rate: u32,
    channels: u16,
    atype: u8,
    scid: u64,
    rx_us: u64,
    seq: u16,
    rtp_ts: u32,
    stream_type: u8,
) -> Vec<u8> {
    let mut v = Vec::with_capacity(11 + VOICE_V2_EXT_LEN + au.len());
    v.extend_from_slice(&rate.to_be_bytes());
    v.extend_from_slice(&channels.to_be_bytes());
    v.push((atype & 0x7F) | VOICE_V2_AU_FLAG);
    v.extend_from_slice(&((VOICE_V2_EXT_LEN + au.len()) as u32).to_be_bytes());
    v.extend_from_slice(&scid.to_be_bytes());
    v.extend_from_slice(&rx_us.to_be_bytes());
    v.extend_from_slice(&seq.to_be_bytes());
    v.extend_from_slice(&rtp_ts.to_be_bytes());
    v.push(stream_type);
    v.extend_from_slice(au);
    v
}

/// A control frame: v1 head with `atype = 0xFF` and `len = payload.len()`.
#[cfg(feature = "voice-tag-v2")]
fn voice_control_v2(rate: u32, channels: u16, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(11 + payload.len());
    v.extend_from_slice(&rate.to_be_bytes());
    v.extend_from_slice(&channels.to_be_bytes());
    v.push(VOICE_V2_CONTROL);
    v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    v.extend_from_slice(payload);
    v
}

/// The BEGIN a voice stream writes on entry to its thread (and re-sends after a seam reconnect):
/// payload `[kind=1][write_us u64][scid u64][stream_type u8][audio_type u8][codec u8][rate u32]
/// [ch u16][audio_latency_ms i32 (-1 = absent)][input u8][begin_seq u32]` — 35 bytes.
/// `codec` uses the SEAM_FORMAT codes (0 PCM, 1 AAC-LC, 2 AAC-ELD, 3 Opus).
#[cfg(feature = "voice-tag-v2")]
#[allow(clippy::too_many_arguments)]
pub fn voice_begin_v2(
    write_us: u64,
    scid: u64,
    stream_type: u8,
    audio_type: u8,
    codec: u8,
    rate: u32,
    channels: u16,
    audio_latency_ms: i32,
    input: bool,
    begin_seq: u32,
) -> Vec<u8> {
    let mut p = Vec::with_capacity(35);
    p.push(VOICE_V2_BEGIN);
    p.extend_from_slice(&write_us.to_be_bytes());
    p.extend_from_slice(&scid.to_be_bytes());
    p.push(stream_type);
    p.push(audio_type);
    p.push(codec);
    p.extend_from_slice(&rate.to_be_bytes());
    p.extend_from_slice(&channels.to_be_bytes());
    p.extend_from_slice(&audio_latency_ms.to_be_bytes());
    p.push(u8::from(input));
    p.extend_from_slice(&begin_seq.to_be_bytes());
    voice_control_v2(rate, channels, &p)
}

/// The END a voice stream writes when its thread leaves: payload `[kind=2][write_us u64][scid u64]
/// [reason u8][frames u32][drops u32][seq_gaps u32][max_inter_arrival_ms u32]` — 34 bytes.
#[cfg(feature = "voice-tag-v2")]
#[allow(clippy::too_many_arguments)]
pub fn voice_end_v2(
    rate: u32,
    channels: u16,
    write_us: u64,
    scid: u64,
    reason: u8,
    frames: u32,
    drops: u32,
    seq_gaps: u32,
    max_inter_arrival_ms: u32,
) -> Vec<u8> {
    let mut p = Vec::with_capacity(34);
    p.push(VOICE_V2_END);
    p.extend_from_slice(&write_us.to_be_bytes());
    p.extend_from_slice(&scid.to_be_bytes());
    p.push(reason);
    p.extend_from_slice(&frames.to_be_bytes());
    p.extend_from_slice(&drops.to_be_bytes());
    p.extend_from_slice(&seq_gaps.to_be_bytes());
    p.extend_from_slice(&max_inter_arrival_ms.to_be_bytes());
    voice_control_v2(rate, channels, &p)
}

#[cfg(test)]
mod tests {
    /// A NAL length field large enough to wrap `usize` must terminate the walk, not produce reversed
    /// slice bounds. On the armv7 box `usize` is 32 bits and the release profile aborts on panic, so
    /// the pre-fix code turned a malformed length into a daemon kill needing a power cycle. This test
    /// passes trivially on a 64-bit host — it is a guard against the `checked_add` being removed, and
    /// the real proof was an execution on the 32-bit target (see the comment in `annexb_from_avcc`).
    #[test]
    fn oversized_nal_length_terminates_instead_of_panicking() {
        for au in [
            vec![0xFF, 0xFF, 0xFF, 0xFF],
            vec![0xFF, 0xFF, 0xFF, 0xFF, 0x01, 0x02],
            vec![0x7F, 0xFF, 0xFF, 0xFF, 0x01],
            vec![0x00, 0x00, 0x00, 0x00],
        ] {
            let out = annexb_from_avcc(&au, 4);
            assert!(out.len() <= au.len() + 4, "walked past the buffer for {au:02x?}");
        }
        // A well-formed AU still converts.
        let au = vec![0x00, 0x00, 0x00, 0x02, 0xAA, 0xBB];
        assert_eq!(annexb_from_avcc(&au, 4), vec![0, 0, 0, 1, 0xAA, 0xBB]);
    }

    use super::*;

    #[test]
    fn avcc_to_annexb() {
        // two NALs, 4-byte lengths
        let au = [0, 0, 0, 3, 0x67, 1, 2, 0, 0, 0, 2, 0x68, 9];
        let annexb = annexb_from_avcc(&au, 4);
        assert_eq!(annexb, vec![0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 9]);
    }

    #[test]
    fn aac_lc_adts_header() {
        let au = vec![0xAA; 20];
        let adts = adts_from_aac_lc(&au, 48000, 2).unwrap();
        assert_eq!(adts.len(), 7 + 20);
        assert_eq!(adts[0], 0xFF);
        assert_eq!(adts[1], 0xF1);
        // sfi for 48k = 3, channels = 2
        assert_eq!((adts[2] >> 2) & 0xF, 3);
        let chan = ((adts[2] & 1) << 2) | (adts[3] >> 6);
        assert_eq!(chan, 2);
        // frame length field
        let flen = (((adts[3] & 0x3) as usize) << 11) | ((adts[4] as usize) << 3) | ((adts[5] >> 5) as usize);
        assert_eq!(flen, 27);
        assert!(adts_from_aac_lc(&au, 12345, 2).is_none());
    }

    #[test]
    fn eld_tagging() {
        let au = [1, 2, 3, 4];
        let t = tag_voice(&au, 16000, 1, 2);
        assert_eq!(&t[0..4], &16000u32.to_be_bytes());
        assert_eq!(&t[4..6], &1u16.to_be_bytes());
        assert_eq!(t[6], 2, "atype (speechRecognition) rides between ch and len");
        assert_eq!(&t[7..11], &4u32.to_be_bytes());
        assert_eq!(&t[11..], &au);
        // Header is 11 bytes, not the original 10 — the byte a consumer must be updated for.
        assert_eq!(t.len(), 11 + au.len());
    }

    /// Golden vectors for the v2 voice wire (plan v5.8 V1a). The Kotlin parser's VoiceFrameTest
    /// carries the SAME bytes; a change on either side must change both.
    #[cfg(feature = "voice-tag-v2")]
    #[test]
    fn voice_v2_golden_vectors() {
        let au = [0xA1, 0xA2, 0xA3];
        let t = tag_voice_v2(&au, 16_000, 1, 1, 0x0102_0304_0506_0708, 0x1122_3344_5566_7788, 0xBEEF, 0xCAFE_F00D, 100);
        let want: Vec<u8> = [
            &[0x00, 0x00, 0x3E, 0x80][..],              // rate 16000
            &[0x00, 0x01],                              // ch 1
            &[0x81],                                    // atype telephony | 0x80
            &[0x00, 0x00, 0x00, 0x1A],                  // len = 23 + 3
            &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08], // scid
            &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88], // rx_us
            &[0xBE, 0xEF],                              // seq
            &[0xCA, 0xFE, 0xF0, 0x0D],                  // rtp_ts
            &[100],                                     // stream_type
            &au,
        ]
        .concat();
        assert_eq!(t, want);

        let b = voice_begin_v2(0x10, 0x0102_0304_0506_0708, 101, 4, 2, 48_000, 2, -1, false, 3);
        let want_b: Vec<u8> = [
            &[0x00, 0x00, 0xBB, 0x80][..],              // rate 48000
            &[0x00, 0x02],
            &[0xFF],
            &[0x00, 0x00, 0x00, 0x23],                  // len 35
            &[0x01],                                    // BEGIN
            &[0, 0, 0, 0, 0, 0, 0, 0x10],               // write_us
            &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08],
            &[101, 4, 2],                               // stream_type, audio_type default, codec ELD
            &[0x00, 0x00, 0xBB, 0x80],                  // rate
            &[0x00, 0x02],                              // ch
            &[0xFF, 0xFF, 0xFF, 0xFF],                  // audioLatencyMs absent = -1
            &[0x00],                                    // input
            &[0x00, 0x00, 0x00, 0x03],                  // begin_seq
        ]
        .concat();
        assert_eq!(b, want_b);

        let e = voice_end_v2(16_000, 1, 0x20, 0x0102_0304_0506_0708, END_SUPERSEDED, 757, 2, 1, 480);
        let want_e: Vec<u8> = [
            &[0x00, 0x00, 0x3E, 0x80][..],
            &[0x00, 0x01],
            &[0xFF],
            &[0x00, 0x00, 0x00, 0x22],                  // len 34
            &[0x02],                                    // END
            &[0, 0, 0, 0, 0, 0, 0, 0x20],
            &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08],
            &[0x03],                                    // superseded
            &[0x00, 0x00, 0x02, 0xF5],                  // frames 757
            &[0x00, 0x00, 0x00, 0x02],                  // drops
            &[0x00, 0x00, 0x00, 0x01],                  // seq gaps
            &[0x00, 0x00, 0x01, 0xE0],                  // max inter-arrival 480 ms
        ]
        .concat();
        assert_eq!(e, want_e);
    }
}
