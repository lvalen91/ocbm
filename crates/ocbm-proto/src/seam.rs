//! The media-lane seam messages (docs/ocbm/00_PROTOCOL.md, Media transport) as code: builders for a box
//! producer and a resynchronising reader for a host. The layouts are the ones `receiver::session` has
//! always written for CarPlay; AirPlay-only mode (`box/airplayd`) reuses them unchanged.
//!
//! Every message is `[u32 BE len][SEAM_MAGIC "SEAV"][marker u8][...]`, `len` counting every byte after
//! itself. Length prefixes are big-endian, every other field little-endian.
//!
//! | lane | marker | body after the marker |
//! |---|---|---|
//! | video | `0x00` key | `[key 32][scid u64 LE]` |
//! | video | `0x01` frame | `[seq u64 LE][hdr 128][body]` |
//! | audio | `SEAM_KEY 0x00` | `[key 32][scid u64 LE]` |
//! | audio | `SEAM_PKT 0x01` | `[scid u64 LE][RTP 12][ciphertext][tag 16][nonce 8]` |
//! | audio | `SEAM_FORMAT 0x02` | `[scid u64 LE][codec u8][rate u32 LE][ch u8][bits u8][audio_type u8]` |

use crate::{META_MAGIC, SEAM_FORMAT, SEAM_KEY, SEAM_MAGIC, SEAM_PKT};

/// The plaintext screen header in front of every video frame body.
pub const SCREEN_HDR_LEN: usize = 128;
pub const KEY_MSG_LEN: usize = 4 + 45;
pub const FORMAT_MSG_LEN: usize = 4 + 21;
/// Bytes of a video frame message in front of the frame body: `[len][magic][0x01][seq][hdr]`.
pub const VIDEO_FRAME_PREFIX_LEN: usize = 4 + 4 + 1 + 8 + SCREEN_HDR_LEN;
/// Bytes of an audio packet message in front of the packet: `[len][magic][0x01][scid]`.
pub const AUDIO_PKT_PREFIX_LEN: usize = 4 + 4 + 1 + 8;

/// `[len=45]["SEAV"][0x00][key 32][scid u64 LE]`: the key message of BOTH lane families.
pub fn key_msg(key: &[u8; 32], scid: u64) -> [u8; KEY_MSG_LEN] {
    let mut m = [0u8; KEY_MSG_LEN];
    m[0..4].copy_from_slice(&45u32.to_be_bytes());
    m[4..8].copy_from_slice(&SEAM_MAGIC);
    m[8] = SEAM_KEY;
    m[9..41].copy_from_slice(key);
    m[41..49].copy_from_slice(&scid.to_le_bytes());
    m
}

/// `[len=141+body]["SEAV"][0x01][seq u64 LE][hdr 128]`: write this, then the `body_len` body bytes.
pub fn video_frame_prefix(seq: u64, hdr: &[u8; SCREEN_HDR_LEN], body_len: usize) -> [u8; VIDEO_FRAME_PREFIX_LEN] {
    let mut m = [0u8; VIDEO_FRAME_PREFIX_LEN];
    m[0..4].copy_from_slice(&((141 + body_len) as u32).to_be_bytes());
    m[4..8].copy_from_slice(&SEAM_MAGIC);
    m[8] = 0x01;
    m[9..17].copy_from_slice(&seq.to_le_bytes());
    m[17..].copy_from_slice(hdr);
    m
}

/// `[len=13+pkt]["SEAV"][SEAM_PKT][scid u64 LE]`: write this, then the `pkt_len` packet bytes.
pub fn audio_pkt_prefix(scid: u64, pkt_len: usize) -> [u8; AUDIO_PKT_PREFIX_LEN] {
    let mut m = [0u8; AUDIO_PKT_PREFIX_LEN];
    m[0..4].copy_from_slice(&((13 + pkt_len) as u32).to_be_bytes());
    m[4..8].copy_from_slice(&SEAM_MAGIC);
    m[8] = SEAM_PKT;
    m[9..17].copy_from_slice(&scid.to_le_bytes());
    m
}

/// `[len=21]["SEAV"][SEAM_FORMAT][scid u64 LE][codec][rate u32 LE][ch][bits][audio_type]`.
pub fn audio_format_msg(scid: u64, codec: u8, rate: u32, ch: u8, bits: u8, audio_type: u8) -> [u8; FORMAT_MSG_LEN] {
    let mut m = [0u8; FORMAT_MSG_LEN];
    m[0..4].copy_from_slice(&21u32.to_be_bytes());
    m[4..8].copy_from_slice(&SEAM_MAGIC);
    m[8] = SEAM_FORMAT;
    m[9..17].copy_from_slice(&scid.to_le_bytes());
    m[17] = codec;
    m[18..22].copy_from_slice(&rate.to_le_bytes());
    m[22] = ch;
    m[23] = bits;
    m[24] = audio_type;
    m
}

/// CH_METADATA: `[u32 BE "META"][u32 BE len][marker][payload]`, `len` = 1 + payload.
pub fn meta_msg(marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(9 + payload.len());
    m.extend_from_slice(&META_MAGIC);
    m.extend_from_slice(&((1 + payload.len()) as u32).to_be_bytes());
    m.push(marker);
    m.extend_from_slice(payload);
    m
}

/// Host-side reassembly of one media lane (video or audio): feed it the channel's payload bytes in
/// arrival order, take whole messages out. ocbmd does not preserve message boundaries, so a message
/// may span many OCBM frames. On garbage it scans forward to the next `SEAM_MAGIC` and counts a resync.
pub struct SeamReader {
    buf: Vec<u8>,
    max: usize,
    pub resyncs: u64,
}

impl SeamReader {
    /// `max` = the largest message accepted (a video lane needs several MiB, an audio lane a few KiB).
    pub fn new(max: usize) -> Self {
        SeamReader { buf: Vec::new(), max, resyncs: 0 }
    }

    /// `new_source` = the OCBM frame carried `F_NEW_SOURCE`: the producer was replaced, so a partial
    /// message still held belongs to the old one and is dropped.
    pub fn push(&mut self, data: &[u8], new_source: bool) {
        if new_source {
            self.buf.clear();
        }
        self.buf.extend_from_slice(data);
    }

    /// The next whole message as `(marker, body after the marker)`.
    pub fn next_msg(&mut self) -> Option<(u8, Vec<u8>)> {
        loop {
            if self.buf.len() < 9 {
                return None;
            }
            let len = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
            if self.buf[4..8] != SEAM_MAGIC || len < 5 || len > self.max {
                // Torn or foreign bytes: re-align on the next magic; a message starts 4 bytes before it.
                // The search starts at 5, so the new start is at least one byte further on.
                self.resyncs += 1;
                match self.buf[5..].windows(4).position(|w| w == SEAM_MAGIC) {
                    Some(p) => {
                        self.buf.drain(..p + 1);
                        continue;
                    }
                    None => {
                        // A magic may straddle the next push: keep the 3 bytes it could start in and the
                        // 4 its length prefix would be.
                        let keep = self.buf.len().min(7);
                        self.buf.drain(..self.buf.len() - keep);
                        return None;
                    }
                }
            }
            if self.buf.len() < 4 + len {
                return None;
            }
            let marker = self.buf[8];
            let body = self.buf[9..4 + len].to_vec();
            self.buf.drain(..4 + len);
            return Some((marker, body));
        }
    }
}

/// Host-side reassembly of CH_METADATA: whole `(marker, payload)` messages, resyncing on `"META"`.
pub struct MetaReader {
    buf: Vec<u8>,
    pub resyncs: u64,
}

pub const META_MAX: usize = 4 * 1024 * 1024;

impl Default for MetaReader {
    fn default() -> Self {
        Self::new()
    }
}

impl MetaReader {
    pub fn new() -> Self {
        MetaReader { buf: Vec::new(), resyncs: 0 }
    }

    pub fn push(&mut self, data: &[u8], new_source: bool) {
        if new_source {
            self.buf.clear();
        }
        self.buf.extend_from_slice(data);
    }

    pub fn next_msg(&mut self) -> Option<(u8, Vec<u8>)> {
        loop {
            if self.buf.len() < 9 {
                return None;
            }
            let len = u32::from_be_bytes([self.buf[4], self.buf[5], self.buf[6], self.buf[7]]) as usize;
            if self.buf[0..4] != META_MAGIC || len == 0 || len > META_MAX {
                self.resyncs += 1;
                match self.buf[1..].windows(4).position(|w| w == META_MAGIC) {
                    Some(p) => {
                        self.buf.drain(..p + 1);
                        continue;
                    }
                    None => {
                        let keep = self.buf.len().min(3);
                        self.buf.drain(..self.buf.len() - keep);
                        return None;
                    }
                }
            }
            if self.buf.len() < 8 + len {
                return None;
            }
            let marker = self.buf[8];
            let body = self.buf[9..8 + len].to_vec();
            self.buf.drain(..8 + len);
            return Some((marker, body));
        }
    }
}

// ---- the codec configuration frame (screen opcode 1) ----
//
// A screen stream's opcode-1 body is plaintext and names the codec. Measured on a real iPhone
// (docs/ocbm/02_AIRPLAY_ONLY_MODE.md): an H.264 stream sends a BARE `avcC` record; an HEVC stream
// (feature bit 42 advertised and a display larger than 1080p) sends an ISO `hvc1` sample entry —
// `[u32 BE size]["hvc1"][78-byte visual sample entry][child boxes]` — whose `hvcC` child box carries
// the record (VPS/SPS/PPS, 4-byte NAL lengths), followed by other child boxes. The tunnel forwards
// the body unchanged; this is the one host-side parser of it.

/// The video codec a screen stream's configuration frame names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    Hevc,
}

impl VideoCodec {
    pub fn name(self) -> &'static str {
        match self {
            VideoCodec::H264 => "H.264",
            VideoCodec::Hevc => "HEVC (H.265)",
        }
    }

    /// The extension of an Annex-B elementary stream file of this codec.
    pub fn ext(self) -> &'static str {
        match self {
            VideoCodec::H264 => "h264",
            VideoCodec::Hevc => "h265",
        }
    }
}

/// A parsed opcode-1 body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoConfig<'a> {
    pub codec: VideoCodec,
    /// Bytes of each NAL length prefix in the stream's opcode-0 frames (1, 2 or 4).
    pub len_size: usize,
    /// The bare `avcC` / `hvcC` record.
    pub record: &'a [u8],
    /// The parameter-set NALs in record order as `(NAL type, NAL)`: SPS 7 / PPS 8 for H.264; VPS 32 /
    /// SPS 33 / PPS 34 (and any SEI array) for HEVC.
    pub nals: Vec<(u8, &'a [u8])>,
    /// The ISO sample entry the record came in (`hvc1`, `hev1`, `avc1`, `avc3`); None = a bare record
    /// or a bare configuration box.
    pub sample_entry: Option<[u8; 4]>,
}

fn be32(b: &[u8]) -> usize {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize
}

/// `avcC` (AVCDecoderConfigurationRecord). Strict: the reserved bits must be set and the first parameter
/// set must be an SPS, so an `hvcC` (which also starts with 0x01) is never mistaken for one. Bytes after
/// the PPS array (the High-profile extension) are allowed.
fn parse_avcc(b: &[u8]) -> Option<(usize, Vec<(u8, &[u8])>)> {
    if b.len() < 7 || b[0] != 1 || b[4] & 0xFC != 0xFC || b[5] & 0xE0 != 0xE0 {
        return None;
    }
    let (len_size, mut at, mut nals) = ((b[4] & 3) as usize + 1, 5usize, Vec::new());
    for mask in [0x1fu8, 0xff] {
        let n = (*b.get(at)? & mask) as usize;
        at += 1;
        for _ in 0..n {
            let l = u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?]) as usize;
            let nal = b.get(at + 2..at + 2 + l)?;
            nals.push((*nal.first()? & 0x1f, nal));
            at += 2 + l;
        }
    }
    (nals.first().map(|n| n.0) == Some(7)).then_some((len_size, nals))
}

/// `hvcC` (HEVCDecoderConfigurationRecord): 22 fixed bytes (profile @1, level @12, lengthSizeMinusOne in
/// @21), the array count @22, then per array `[completeness|type u8][u16 count]{[u16 len][NAL]}`. Must
/// carry an SPS; `exact` = must also end at the last byte (a bare record, where nothing else frames it).
fn parse_hvcc(b: &[u8], exact: bool) -> Option<(usize, Vec<(u8, &[u8])>)> {
    if b.len() < 23 || b[0] != 1 {
        return None;
    }
    let (len_size, mut at, mut nals) = ((b[21] & 3) as usize + 1, 23usize, Vec::new());
    for _ in 0..b[22] {
        let ty = *b.get(at)? & 0x3f;
        let n = u16::from_be_bytes([*b.get(at + 1)?, *b.get(at + 2)?]) as usize;
        at += 3;
        for _ in 0..n {
            let l = u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?]) as usize;
            nals.push((ty, b.get(at + 2..at + 2 + l)?));
            at += 2 + l;
        }
    }
    ((!exact || at == b.len()) && nals.iter().any(|(t, _)| *t == 33)).then_some((len_size, nals))
}

fn config_box<'a>(tag: &[u8], content: &'a [u8], entry: Option<[u8; 4]>) -> Option<VideoConfig<'a>> {
    let (codec, (len_size, nals)) = match tag {
        b"avcC" => (VideoCodec::H264, parse_avcc(content)?),
        b"hvcC" => (VideoCodec::Hevc, parse_hvcc(content, false)?),
        _ => return None,
    };
    Some(VideoConfig { codec, len_size, record: content, nals, sample_entry: entry })
}

/// Parse a screen stream's opcode-1 body, in this order: an ISO sample entry (`hvc1`/`hev1`/`avc1`/
/// `avc3`) whose child boxes hold an `hvcC` or `avcC`; a configuration box found anywhere in the body
/// whose size field is consistent (a bare box, or an unknown wrapper); a bare `avcC` record; a bare
/// `hvcC` record that parses to its last byte. None = no usable configuration.
pub fn video_config(b: &[u8]) -> Option<VideoConfig<'_>> {
    const VISUAL_SAMPLE_ENTRY: usize = 78; // ISO/IEC 14496-12 VisualSampleEntry fields after the box header
    if let Some(e) = b.get(4..8).filter(|e| matches!(*e, b"hvc1" | b"hev1" | b"avc1" | b"avc3")) {
        let entry: [u8; 4] = e.try_into().unwrap();
        let end = be32(b).min(b.len());
        let mut at = 8 + VISUAL_SAMPLE_ENTRY;
        while at + 8 <= end {
            let size = be32(&b[at..]);
            if size < 8 || at + size > end {
                break;
            }
            if let Some(c) = config_box(&b[at + 4..at + 8], &b[at + 8..at + size], Some(entry)) {
                return Some(c);
            }
            at += size;
        }
    }
    for p in 4..b.len().saturating_sub(3) {
        if matches!(&b[p..p + 4], b"avcC" | b"hvcC") {
            let size = be32(&b[p - 4..]);
            if size >= 8 && p - 4 + size <= b.len() {
                if let Some(c) = config_box(&b[p..p + 4], &b[p + 4..p - 4 + size], None) {
                    return Some(c);
                }
            }
        }
    }
    if let Some((len_size, nals)) = parse_avcc(b) {
        return Some(VideoConfig { codec: VideoCodec::H264, len_size, record: b, nals, sample_entry: None });
    }
    let (len_size, nals) = parse_hvcc(b, true)?;
    Some(VideoConfig { codec: VideoCodec::Hevc, len_size, record: b, nals, sample_entry: None })
}

impl VideoConfig<'_> {
    /// The parameter sets as Annex-B (`00 00 00 01` + NAL each), in record order: what goes in front of
    /// the stream's frames in an elementary-stream file.
    pub fn annexb(&self) -> Vec<u8> {
        let mut o = Vec::new();
        for (_, n) in &self.nals {
            o.extend_from_slice(&[0, 0, 0, 1]);
            o.extend_from_slice(n);
        }
        o
    }

    /// One line for a log: codec, profile, level and the parameter sets.
    pub fn describe(&self) -> String {
        let r = self.record;
        let count = |t: u8| self.nals.iter().filter(|(ty, _)| *ty == t).count();
        let wrap = self.sample_entry.map(|e| format!(" in an '{}' sample entry", String::from_utf8_lossy(&e))).unwrap_or_default();
        match self.codec {
            VideoCodec::H264 => format!(
                "avcC{wrap} (H.264): profile {} level {}, NAL length size {}, SPS x{} PPS x{}",
                r[1], r[3], self.len_size, count(7), count(8)
            ),
            VideoCodec::Hevc => format!(
                "hvcC{wrap} (HEVC / H.265): profile_idc {} ({}) tier {} level {:.1}, chroma format {}, bit depth {} luma / {} chroma, \
                 NAL length size {}; VPS x{} SPS x{} PPS x{}, other parameter NALs x{}",
                r[1] & 0x1f,
                match r[1] & 0x1f {
                    1 => "Main",
                    2 => "Main 10",
                    3 => "Main Still Picture",
                    4 => "Range Extensions",
                    _ => "other",
                },
                if r[1] & 0x20 != 0 { "High" } else { "Main" },
                f64::from(r[12]) / 30.0,
                r[16] & 3,
                (r[17] & 7) + 8,
                (r[18] & 7) + 8,
                self.len_size,
                count(32),
                count(33),
                count(34),
                self.nals.iter().filter(|(t, _)| !matches!(t, 32..=34)).count()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    #[test]
    fn key_message_bytes_are_pinned() {
        let key: [u8; 32] = core::array::from_fn(|i| i as u8);
        let m = key_msg(&key, 0x1122_3344_5566_7788);
        assert_eq!(m.len(), 49);
        assert_eq!(&m[0..4], &[0, 0, 0, 45]); // len BE, counts magic + marker + key + scid
        assert_eq!(&m[4..8], b"SEAV");
        assert_eq!(m[8], 0x00);
        assert_eq!(&m[9..41], &key);
        assert_eq!(&m[41..49], &[0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]); // scid LE, TRAILS the key
    }

    #[test]
    fn video_frame_message_bytes_are_pinned() {
        let mut hdr = [0u8; SCREEN_HDR_LEN];
        hdr[0..4].copy_from_slice(&3u32.to_le_bytes()); // bodySize
        hdr[4] = 0; // opcode 0 = encrypted video frame
        let p = video_frame_prefix(7, &hdr, 3);
        assert_eq!(p.len(), 145);
        assert_eq!(&p[0..4], &(141u32 + 3).to_be_bytes());
        assert_eq!(&p[4..8], b"SEAV");
        assert_eq!(p[8], 0x01);
        assert_eq!(&p[9..17], &7u64.to_le_bytes()); // the nonce counter, from the wire
        assert_eq!(&p[17..145], &hdr);
    }

    #[test]
    fn audio_messages_bytes_are_pinned() {
        let p = audio_pkt_prefix(0x0102_0304_0506_0708, 42);
        assert_eq!(&p[0..4], &(13u32 + 42).to_be_bytes());
        assert_eq!(&p[4..8], b"SEAV");
        assert_eq!(p[8], SEAM_PKT);
        assert_eq!(&p[9..17], &[8, 7, 6, 5, 4, 3, 2, 1]);

        let f = audio_format_msg(0x0102_0304_0506_0708, SEAM_CODEC_AAC_LC, 44100, 2, 0, 0);
        assert_eq!(f.len(), 25);
        assert_eq!(&f[0..4], &[0, 0, 0, 21]);
        assert_eq!(&f[4..8], b"SEAV");
        assert_eq!(f[8], SEAM_FORMAT);
        assert_eq!(&f[9..17], &[8, 7, 6, 5, 4, 3, 2, 1]);
        assert_eq!(f[17], 1); // AAC-LC
        assert_eq!(&f[18..22], &[0x44, 0xAC, 0, 0]); // 44100 LE
        assert_eq!(&f[22..25], &[2, 0, 0]); // ch, bits, audio_type
    }

    #[test]
    fn meta_message_bytes_are_pinned() {
        let m = meta_msg(META_JSON, b"{}");
        assert_eq!(m, [b'M', b'E', b'T', b'A', 0, 0, 0, 3, 0x02, b'{', b'}']);
    }

    #[test]
    fn reader_reassembles_across_arbitrary_chunks_and_resyncs_after_a_torn_message() {
        let key = [9u8; 32];
        let hdr = [0u8; SCREEN_HDR_LEN];
        let mut stream = Vec::new();
        stream.extend_from_slice(&key_msg(&key, 5));
        stream.extend_from_slice(&video_frame_prefix(0, &hdr, 4));
        stream.extend_from_slice(b"abcd");
        // A torn message (an OCBM frame lost at a queue cap): the head of a frame whose body never
        // arrives. Its declared length swallows what follows; the reader must find its way back.
        let torn = video_frame_prefix(1, &hdr, 4000);
        stream.extend_from_slice(&torn[..60]);
        for seq in 2..60u64 {
            stream.extend_from_slice(&video_frame_prefix(seq, &hdr, 2));
            stream.extend_from_slice(b"xy");
        }
        for chunk in [1usize, 7, 64, 4096] {
            let mut r = SeamReader::new(1 << 20);
            let mut got = Vec::new();
            for c in stream.chunks(chunk) {
                r.push(c, false);
                while let Some(m) = r.next_msg() {
                    got.push(m);
                }
            }
            assert_eq!(got[0].0, 0x00, "chunk {chunk}");
            assert_eq!(got[0].1.len(), 40);
            assert_eq!(&got[1].1[8 + 128..], b"abcd");
            assert!(r.resyncs >= 1);
            // The torn message itself comes out once, with the bytes that followed it as its body (a
            // host's AEAD rejects it); after it every message is whole again, in order, up to the last.
            assert_eq!(u64::from_le_bytes(got[2].1[0..8].try_into().unwrap()), 1);
            assert_eq!(got[2].1.len(), 8 + 128 + 4000);
            let tail: Vec<u64> = got[3..].iter().map(|m| u64::from_le_bytes(m.1[0..8].try_into().unwrap())).collect();
            assert!(tail.len() >= 20, "chunk {chunk}: {} messages after the gap", tail.len());
            assert_eq!(*tail.last().unwrap(), 59);
            assert!(tail.windows(2).all(|w| w[1] == w[0] + 1));
            assert!(got[3..].iter().all(|m| m.0 == 0x01 && &m.1[8 + 128..] == b"xy"));
        }
    }

    #[test]
    fn new_source_drops_the_partial_message_of_the_old_producer() {
        let mut r = SeamReader::new(1 << 16);
        let m = audio_pkt_prefix(1, 100);
        r.push(&m, false); // head only; the 100 packet bytes never come
        assert!(r.next_msg().is_none());
        let mut next = audio_pkt_prefix(2, 3).to_vec();
        next.extend_from_slice(&[1, 2, 3]);
        r.push(&next, true);
        let (marker, body) = r.next_msg().unwrap();
        assert_eq!(marker, SEAM_PKT);
        assert_eq!(&body[0..8], &2u64.to_le_bytes());
        assert_eq!(&body[8..], &[1, 2, 3]);
        assert_eq!(r.resyncs, 0);
    }

    #[test]
    fn meta_reader_round_trip_and_resync() {
        let mut s = b"junk".to_vec();
        s.extend_from_slice(&meta_msg(META_JSON, b"{\"kind\":\"airplay\"}"));
        s.extend_from_slice(&meta_msg(META_ARTWORK, &[0, 0xFF, 0xD8]));
        let mut r = MetaReader::new();
        let mut got = Vec::new();
        for c in s.chunks(5) {
            r.push(c, false);
            while let Some(m) = r.next_msg() {
                got.push(m);
            }
        }
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], (META_JSON, b"{\"kind\":\"airplay\"}".to_vec()));
        assert_eq!(got[1], (META_ARTWORK, vec![0, 0xFF, 0xD8]));
    }

    #[test]
    fn airplay_request_bytes_are_pinned_and_validated() {
        let r = AirplayRequest {
            ssid: "My AP".into(),
            psk: "password1".into(),
            name: "Car".into(),
            channel: 36,
            display: (1280, 720, 60),
            flags: 0,
        };
        let b = r.encode();
        let want: Vec<u8> = [
            &[0x03u8, 0x02][..],          // CT_MODE_SELECT, MODE_AIRPLAY
            &[0x01, 5][..], b"My AP",     // SSID
            &[0x02, 9][..], b"password1", // PSK
            &[0x03, 3][..], b"Car",       // NAME
            &[0x04, 1, 36][..],           // CHANNEL
            &[0x05, 5, 0x00, 0x05, 0xD0, 0x02, 60][..], // DISPLAY 1280 x 720 LE, 60
        ]
        .concat();
        assert_eq!(b, want);
        assert_eq!(AirplayRequest::decode(&b), Some(r.clone()));

        // name absent -> the SSID; channel absent -> 0; display absent -> the default; unknown tag skipped
        let min = [&[0x03u8, 0x02, 0x7E, 2, 1, 2][..], &[0x02, 8][..], b"12345678", &[0x01, 1, b'x'][..]].concat();
        let d = AirplayRequest::decode(&min).unwrap();
        assert_eq!((d.ssid.as_str(), d.name.as_str(), d.channel, d.display), ("x", "x", 0, AIRPLAY_DEFAULT_DISPLAY));

        let bad = |f: &dyn Fn(&mut AirplayRequest)| {
            let mut x = r.clone();
            f(&mut x);
            AirplayRequest::decode(&x.encode())
        };
        assert_eq!(bad(&|x| x.psk = "short".into()), None); // < 8
        assert_eq!(bad(&|x| x.psk = "p".repeat(64)), None); // > 63
        assert_eq!(bad(&|x| x.psk = "pass\nword".into()), None); // not printable ASCII
        assert_eq!(bad(&|x| x.ssid = String::new()), None);
        assert_eq!(bad(&|x| x.ssid = "s".repeat(33)), None);
        assert_eq!(bad(&|x| x.ssid = "a\nssid=evil".into()), None); // a control character
        assert_eq!(bad(&|x| x.name = "n".repeat(64)), None);
        assert_eq!(bad(&|x| x.channel = 15), None);
        assert_eq!(bad(&|x| x.display = (0, 720, 60)), None);
        // display ranges: 320..=7680 each side, 1..=120 fps
        for d in [(319, 720, 60), (1280, 319, 60), (7681, 2160, 60), (3840, 7681, 60), (1280, 720, 0), (1280, 720, 121)] {
            assert_eq!(bad(&|x| x.display = d), None, "{d:?}");
        }
        for d in [(320, 320, 1), (7680, 7680, 120), (3840, 2160, 60)] {
            assert_eq!(bad(&|x| x.display = d).map(|x| x.display), Some(d));
        }
        assert_eq!(AirplayRequest::decode(&[&b[..b.len() - 7], &[0x05, 4, 0, 5, 0xD0, 2][..]].concat()), None); // DISPLAY not 5 B
        assert!(!r.hevc());
        assert!(bad(&|x| x.ssid = "caf\u{e9} / & \\ $x `y`".into()).is_some()); // shell/sed metacharacters are data
        assert_eq!(AirplayRequest::decode(&b[..b.len() - 1]), None); // truncated TLV
        assert_eq!(AirplayRequest::decode(&[CT_MODE_SELECT, MODE_CONSOLE]), None);
        assert_eq!(AirplayRequest::decode(&[CT_MODE_SELECT, MODE_AIRPLAY]), None); // no SSID/PSK
    }

    #[test]
    fn airplay_request_flags_tlv_is_optional_and_never_refused() {
        assert_eq!((AIRPLAY_REQ_FLAGS, AIRPLAY_FLAG_HEVC, AIRPLAY_FLAGS_KNOWN), (0x06, 0x01, 0x01));
        let r = AirplayRequest {
            ssid: "A".into(),
            psk: "12345678".into(),
            name: String::new(),
            channel: 0,
            display: (3840, 2160, 60),
            flags: AIRPLAY_FLAG_HEVC,
        };
        let b = r.encode();
        let want: Vec<u8> = [
            &[0x03u8, 0x02][..],
            &[0x01, 1, b'A'][..],
            &[0x02, 8][..], b"12345678",
            &[0x05, 5, 0x00, 0x0F, 0x70, 0x08, 60][..], // DISPLAY 3840 x 2160 LE, 60
            &[0x06, 1, 0x01][..],                       // FLAGS: HEVC
        ]
        .concat();
        assert_eq!(b, want);
        let d = AirplayRequest::decode(&b).unwrap();
        assert!(d.hevc());
        assert_eq!((d.display, d.flags, d.name.as_str()), ((3840, 2160, 60), AIRPLAY_FLAG_HEVC, "A"));
        let base = &b[..b.len() - 3]; // the same request without the FLAGS TLV
        let with = |tlv: &[u8]| AirplayRequest::decode(&[base, tlv].concat());
        assert_eq!(with(&[]).map(|x| x.flags), Some(0)); // absent = 0
        assert_eq!(with(&[0x06, 0]).map(|x| x.flags), Some(0)); // empty = 0, not an error
        assert_eq!(with(&[0x06, 1, 0xFE]).map(|x| x.flags), Some(0)); // unknown bits ignored
        assert_eq!(with(&[0x06, 1, 0xFF]).map(|x| x.flags), Some(AIRPLAY_FLAG_HEVC));
        assert_eq!(with(&[0x06, 3, 0x01, 0xAA, 0xBB]).map(|x| x.flags), Some(AIRPLAY_FLAG_HEVC)); // longer: byte 0 only
        assert_eq!(with(&[0x06, 2, 0x01]), None); // a truncated TLV is still a truncated TLV
        // flags 0 is not encoded: the request is byte-identical to one from before the TLV existed
        let mut r0 = r.clone();
        r0.flags = 0;
        assert_eq!(r0.encode(), base);
        // unknown bits a host should not have set are dropped on decode
        r0.flags = 0x80;
        assert_eq!(AirplayRequest::decode(&r0.encode()).map(|x| x.flags), Some(0));
    }

    /// The opcode-1 bodies of both codecs, in the layouts measured on a real iPhone.
    fn hvcc_record() -> Vec<u8> {
        // 22 fixed bytes: Main (profile 1), level 5.1 (153), 4:2:0, 8-bit, lengthSizeMinusOne 3; three arrays
        let mut r = vec![1u8, 0x01, 0x60, 0, 0, 0, 0xB0, 0, 0, 0, 0, 0, 153, 0xF0, 0, 0xFC, 0xFD, 0xF8, 0xF8, 0, 0, 0x0F, 3];
        for (t, nal) in [(32u8, &[0x40u8, 0x01, 0x0C][..]), (33, &[0x42, 0x01, 0x01, 0x01]), (34, &[0x44, 0x01, 0xC0])] {
            r.extend_from_slice(&[0x80 | t, 0, 1]);
            r.extend_from_slice(&(nal.len() as u16).to_be_bytes());
            r.extend_from_slice(nal);
        }
        r
    }

    fn hvc1_entry(rec: &[u8]) -> Vec<u8> {
        let mut child = ((rec.len() + 8) as u32).to_be_bytes().to_vec();
        child.extend_from_slice(b"hvcC");
        child.extend_from_slice(rec);
        let colr = [&20u32.to_be_bytes()[..], b"colr", b"nclx", &[0, 1, 0, 1, 0, 1, 0, 0]].concat(); // a trailing child box
        let mut e = vec![0u8; 8 + 78];
        e[4..8].copy_from_slice(b"hvc1");
        e[24..26].copy_from_slice(&3840u16.to_be_bytes()); // width/height inside the visual sample entry
        e[26..28].copy_from_slice(&2160u16.to_be_bytes());
        e.extend(child);
        e.extend(colr);
        let n = e.len() as u32;
        e[..4].copy_from_slice(&n.to_be_bytes());
        e
    }

    #[test]
    fn video_config_reads_a_bare_avcc_and_an_hvc1_wrapped_hvcc() {
        let avcc = [1u8, 100, 0, 31, 0xFF, 0xE1, 0, 4, 0x67, 0x64, 0, 31, 1, 0, 2, 0x68, 0xEE, 0xFD, 0xF8, 0xF8, 0];
        let c = video_config(&avcc).unwrap();
        assert_eq!((c.codec, c.len_size, c.sample_entry), (VideoCodec::H264, 4, None));
        assert_eq!(c.annexb(), [0, 0, 0, 1, 0x67, 0x64, 0, 31, 0, 0, 0, 1, 0x68, 0xEE]);
        assert!(c.describe().starts_with("avcC (H.264): profile 100 level 31"), "{}", c.describe());

        let rec = hvcc_record();
        let e = hvc1_entry(&rec);
        let c = video_config(&e).unwrap();
        assert_eq!((c.codec, c.len_size, c.sample_entry, c.record), (VideoCodec::Hevc, 4, Some(*b"hvc1"), &rec[..]));
        assert_eq!(c.nals.iter().map(|n| n.0).collect::<Vec<_>>(), [32, 33, 34]);
        assert_eq!(
            c.annexb(),
            [&[0u8, 0, 0, 1, 0x40, 0x01, 0x0C][..], &[0, 0, 0, 1, 0x42, 0x01, 0x01, 0x01], &[0, 0, 0, 1, 0x44, 0x01, 0xC0]].concat()
        );
        let d = c.describe();
        assert!(d.contains("in an 'hvc1' sample entry") && d.contains("profile_idc 1 (Main)") && d.contains("level 5.1"), "{d}");
        assert!(d.contains("VPS x1 SPS x1 PPS x1"), "{d}");
        assert_eq!(video_config(&e).map(|c| c.codec.ext()), Some("h265"));
        // a bare hvcC box and a bare hvcC record are HEVC too; an hvcC never reads as avcC
        let boxed = [&((rec.len() + 8) as u32).to_be_bytes()[..], b"hvcC", &rec].concat();
        assert_eq!(video_config(&boxed).map(|c| (c.codec, c.sample_entry)), Some((VideoCodec::Hevc, None)));
        assert_eq!(video_config(&rec).map(|c| c.codec), Some(VideoCodec::Hevc));
        // garbage, a truncated entry, and a record without an SPS are no configuration
        assert!(video_config(b"bplist00 statistics").is_none());
        assert!(video_config(&e[..60]).is_none());
        let mut no_sps = rec.clone();
        no_sps[22] = 1;
        no_sps.truncate(23 + 3 + 2 + 3);
        assert!(video_config(&no_sps).is_none());
    }

    #[test]
    fn hello_ack_mode_tail_is_appended_after_the_bundle_tail() {
        assert_eq!(HELLO_ACK_LEN, 40);
        assert_eq!((HELLO_ACK_AP_STATE_OFF, HELLO_ACK_SESSION_STATE_OFF, HELLO_ACK_MODE_LEN), (40, 41, 42));
        let mut a = [0u8; HELLO_ACK_MODE_LEN];
        a[0] = CT_HELLO_ACK;
        a[6] = MODE_AIRPLAY;
        a[40] = AIRPLAY_AP_UP;
        a[41] = AIRPLAY_SESSION_WAITING;
        assert_eq!(hello_ack_airplay(&a), Some((AIRPLAY_AP_UP, AIRPLAY_SESSION_WAITING)));
        assert!(hello_ack_bundle(&a).is_some()); // the longer ACK still parses for a bundle-only reader
        assert_eq!(hello_ack_airplay(&a[..40]), None); // a box without the tail
        assert_eq!((CT_MODE_ACK, CT_AIRPLAY_STATE, MODE_AIRPLAY, CAP_AIRPLAY), (0x1D, 0x1E, 0x02, 0x40));
    }
}
