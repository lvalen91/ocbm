//! capcheck — OFF-BOX check of an airplay-probe stream capture (`<name>.bin` + `<name>.meta`).
//! Host tool only (feature `capcheck`); never built for or run on the box.
//!
//!   capcheck <name>.bin <name>.meta [--out DIR]
//!
//! Step 1, always: do the captured packets AUTHENTICATE, and under which key and layout? The
//! EXPECTED hypothesis is the one the OCBM receiver already implements; every other key / AAD /
//! counter combination tried is a labelled ALTERNATIVE and reported separately, so a result never
//! blurs which hypothesis it supports. No crypto is implemented here: audio goes through
//! `receiver::stream::decrypt_audio_aad`, stream keys through `receiver::stream::derive_stream_keys`
//! or the pairing crate's `hkdf_sha512`, a screen frame through the same `chacha20poly1305` call
//! `receiver::session::spawn_screen` makes.
//!
//!   103 buffered audio (TCP)  `[u16 BE len][RTP 12][ciphertext][tag 16][nonce 8]`, key = `shk`
//!   96  realtime audio (UDP)  same packet shape (captured with the same length prefix); EXPECTED
//!       key = HKDF-SHA512(pairing secret, "DataStream-Salt"+scid, Output key) — the receiver's
//!       realtime-audio rule. AAD = RTP bytes 4..12 in both.
//!   110 screen (TCP)          `[128-byte header][body]`, key as for 96, AAD = the header, nonce =
//!       4 zero bytes ‖ u64 LE count of opcode-0 frames.
//!
//! Step 2, with `--out DIR` and only for a hypothesis that authenticated: write the DECRYPTED
//! elementary stream for a decoder to try (`.h264` or `.h265` Annex-B, by the stream's own codec
//! configuration frame: `avcC` or `hvcC`; `.aac` ADTS). The owner's own media,
//! kept under /tmp by the caller.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use pairing::crypto::hkdf_sha512;
use receiver::stream::{decrypt_audio_aad, derive_stream_keys};

#[derive(Default)]
struct Meta {
    ty: u64,
    scid: Option<u64>,
    /// (stream type, streamConnectionID) of every stream of the session.
    scids: Vec<(u64, u64)>,
    shk: Option<Vec<u8>>,
    secret: Option<Vec<u8>>,
    secret_src: String,
    /// Scalar fields of the stream's SETUP dictionary.
    fields: Vec<(String, String)>,
}

impl Meta {
    fn field(&self, k: &str) -> Option<&str> {
        self.fields.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn hex(b: &[u8], max: usize) -> String {
    let mut s: String = b.iter().take(max).map(|x| format!("{x:02x}")).collect();
    if b.len() > max {
        s.push_str("..");
    }
    s
}

fn parse_meta(text: &str) -> Meta {
    let mut m = Meta::default();
    for l in text.lines() {
        let Some((k, v)) = l.split_once('=') else { continue };
        match k {
            "type" => m.ty = v.parse().unwrap_or(0),
            "scid" => m.scid = v.parse().ok(),
            "shk" => m.shk = unhex(v),
            "secret" => m.secret = unhex(v),
            "secret_src" => m.secret_src = v.to_string(),
            _ => {
                if let Some(t) = k.strip_prefix("scid_") {
                    if let (Ok(t), Ok(id)) = (t.parse(), v.parse()) {
                        m.scids.push((t, id));
                    }
                } else if let Some(f) = k.strip_prefix("f_") {
                    m.fields.push((f.to_string(), v.to_string()));
                }
            }
        }
    }
    m
}

/// Candidate keys. The EXPECTED one for the stream type is first; the rest are alternatives.
fn keys(m: &Meta) -> Vec<(String, [u8; 32])> {
    let mut expected = Vec::new();
    let mut alt = Vec::new();
    let shk = m.shk.as_deref().and_then(|k| <[u8; 32]>::try_from(k).ok()).map(|k| ("shk from the stream SETUP".to_string(), k));
    if let Some(sec) = &m.secret {
        if let Some(scid) = m.scid {
            let k = derive_stream_keys(sec, scid);
            expected.push((format!("HKDF(pairing secret, DataStream-Salt{scid} [this stream], Output-Encryption-Key)"), k.output));
            alt.push((format!("HKDF(pairing secret, DataStream-Salt{scid} [this stream], Input-Encryption-Key)"), k.input));
        }
        for (t, id) in m.scids.iter().filter(|(_, id)| Some(*id) != m.scid) {
            let k = derive_stream_keys(sec, *id);
            alt.push((format!("HKDF(pairing secret, DataStream-Salt{id} [the type-{t} stream's id], Output-Encryption-Key)"), k.output));
            alt.push((format!("HKDF(pairing secret, DataStream-Salt{id} [the type-{t} stream's id], Input-Encryption-Key)"), k.input));
        }
        for (salt, info) in [
            ("DataStream-Salt", "DataStream-Output-Encryption-Key"),
            ("DataStream-Salt", "DataStream-Input-Encryption-Key"),
            ("Control-Salt", "Control-Write-Encryption-Key"),
            ("Control-Salt", "Control-Read-Encryption-Key"),
            ("Events-Salt", "Events-Write-Encryption-Key"),
            ("Events-Salt", "Events-Read-Encryption-Key"),
        ] {
            alt.push((format!("HKDF(pairing secret, {salt} [no id], {info})"), hkdf_sha512::<32>(sec, salt.as_bytes(), info.as_bytes())));
        }
    }
    match (m.ty, shk) {
        // Buffered audio is keyed by its own shk; everything derived is an alternative.
        (103, Some(s)) => {
            let mut v = vec![s];
            v.append(&mut expected);
            v.append(&mut alt);
            v
        }
        (_, s) => {
            expected.extend(alt);
            expected.extend(s);
            expected
        }
    }
}

/// One hypothesis and how it fared.
struct Row {
    name: String,
    ok: usize,
    tried: usize,
    expected: bool,
}

// ---------------------------------------------------------------- audio (103, 96, other RTP)

/// Split a capture into whole packets (each without its length prefix).
fn audio_packets(cap: &[u8]) -> Vec<&[u8]> {
    let (mut out, mut at) = (Vec::new(), 0usize);
    while at + 2 <= cap.len() {
        let len = u16::from_be_bytes([cap[at], cap[at + 1]]) as usize;
        if len < 2 + 12 + 16 + 8 || at + len > cap.len() {
            break;
        }
        out.push(&cap[at + 2..at + len]);
        at += len;
    }
    out
}

const AADS: [&str; 3] = ["AAD = RTP bytes 4..12", "AAD = whole 12-byte RTP header", "no AAD"];

fn aad_of<'a>(p: &'a [u8], which: usize) -> &'a [u8] {
    match which {
        0 => &p[4..12],
        1 => &p[..12],
        _ => &[],
    }
}

/// Try every key × AAD on a sample, then the best on everything. Returns the rows and, if any
/// hypothesis authenticated packets, the winning (key, AAD index).
fn check_audio(pkts: &[&[u8]], m: &Meta) -> (Vec<Row>, Option<([u8; 32], usize)>) {
    let sample = &pkts[..pkts.len().min(400)];
    let mut rows = Vec::new();
    let mut best: Option<(usize, [u8; 32], usize, usize)> = None; // ok, key, aad, row index
    for (ki, (name, key)) in keys(m).into_iter().enumerate() {
        for (ai, aad_name) in AADS.iter().enumerate() {
            let ok = sample.iter().filter(|p| decrypt_audio_aad(&key, p, aad_of(p, ai)).is_some()).count();
            if ok > best.as_ref().map(|b| b.0).unwrap_or(0) {
                best = Some((ok, key, ai, rows.len()));
            }
            rows.push(Row {
                name: format!("{name}; [RTP 12][ciphertext][tag 16][nonce 8]; {aad_name}"),
                ok,
                tried: sample.len(),
                expected: ki == 0 && ai == 0,
            });
        }
    }
    // Re-count the winner over the whole capture.
    if let Some((_, key, ai, ri)) = best {
        rows[ri].ok = pkts.iter().filter(|p| decrypt_audio_aad(&key, p, aad_of(p, ai)).is_some()).count();
        rows[ri].tried = pkts.len();
        return (rows, Some((key, ai)));
    }
    (rows, None)
}

/// AAC-LC sample rate from the stream's `audioFormat` bit (the AirPlay audio-format enum).
fn aac_lc_rate(m: &Meta) -> Option<u32> {
    match m.field("audioFormat").and_then(|v| v.parse::<u64>().ok()) {
        Some(0x40_0000) => Some(44_100),
        Some(0x80_0000) => Some(48_000),
        _ => None,
    }
}

fn adts(au: &[u8], rate: u32) -> [u8; 7] {
    let (len, sr) = (au.len() + 7, if rate == 48_000 { 3u8 } else { 4u8 });
    // AAC LC (profile 2 -> field 1), 2 channels, no CRC.
    [0xFF, 0xF1, (1 << 6) | (sr << 2), (2 << 6) | (len >> 11) as u8, (len >> 3) as u8, ((len & 7) << 5) as u8 | 0x1F, 0xFC]
}

fn extract_audio(pkts: &[&[u8]], key: &[u8; 32], aad: usize, m: &Meta, out: &std::path::Path, name: &str) {
    let mut seen = std::collections::HashSet::new();
    let (mut aus, mut dup, mut pts, mut ssrcs) = (Vec::new(), 0usize, std::collections::BTreeSet::new(), std::collections::BTreeSet::new());
    for p in pkts {
        pts.insert(p[1] & 0x7f);
        ssrcs.insert(u32::from_be_bytes([p[8], p[9], p[10], p[11]]));
        let Some(au) = decrypt_audio_aad(key, p, aad_of(p, aad)) else { continue };
        // Realtime audio repeats packets (redundantAudio): keep the first copy of each (seq, timestamp).
        if !seen.insert((p[2], p[3], p[4], p[5], p[6], p[7])) {
            dup += 1;
            continue;
        }
        aus.push(au);
    }
    let mut sizes: Vec<usize> = aus.iter().map(Vec::len).collect();
    sizes.sort_unstable();
    println!(
        "  RTP payload types {pts:?}, SSRCs {}, {} unique packets, {dup} repeats dropped ({:.2} copies per packet)",
        ssrcs.len(), aus.len(), (aus.len() + dup) as f64 / aus.len().max(1) as f64
    );
    println!(
        "  decrypted AU sizes: min {} / median {} / max {} B; first AU starts {}",
        sizes.first().unwrap_or(&0), sizes.get(sizes.len() / 2).unwrap_or(&0), sizes.last().unwrap_or(&0),
        aus.iter().find(|a| a.len() > 16).map(|a| hex(a, 12)).unwrap_or_default()
    );
    let spf: u64 = m.field("spf").and_then(|v| v.parse().ok()).unwrap_or(1024);
    match aac_lc_rate(m) {
        Some(rate) => {
            let path = out.join(format!("{name}.aac"));
            let mut f = Vec::new();
            for au in &aus {
                f.extend_from_slice(&adts(au, rate));
                f.extend_from_slice(au);
            }
            std::fs::write(&path, f).expect("write aac");
            println!(
                "  payload per SETUP: AAC-LC {rate} Hz stereo, {spf} samples per packet, RAW access units (no ADTS on the wire)"
            );
            println!("EXTRACT aac={} aus={} expect_seconds={:.1}", path.display(), aus.len(), (aus.len() as u64 * spf) as f64 / f64::from(rate));
        }
        None => println!(
            "  NOT EXTRACTED: audioFormat {:?} / ct {:?} is not an AAC-LC format this tool wraps",
            m.field("audioFormat"), m.field("ct")
        ),
    }
}

// ---------------------------------------------------------------- screen (110)

/// Whole screen frames as (header, body).
fn screen_frames(cap: &[u8]) -> Vec<(&[u8], &[u8])> {
    let (mut out, mut at) = (Vec::new(), 0usize);
    while at + 128 <= cap.len() {
        let body = u32::from_le_bytes([cap[at], cap[at + 1], cap[at + 2], cap[at + 3]]) as usize;
        if body > 16 * 1024 * 1024 || at + 128 + body > cap.len() {
            break;
        }
        out.push((&cap[at..at + 128], &cap[at + 128..at + 128 + body]));
        at += 128 + body;
    }
    out
}

/// The receiver reads the opcode as the single byte at offset 4.
fn opcode(hdr: &[u8]) -> u8 {
    hdr[4]
}

fn nonce(counter: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_le_bytes());
    n
}

fn check_screen(frames: &[(&[u8], &[u8])], m: &Meta) -> (Vec<Row>, Option<[u8; 32]>) {
    let video = frames.iter().filter(|(h, _)| opcode(h) == 0).count();
    let mut rows = Vec::new();
    let mut winner = None;
    for (ki, (name, key)) in keys(m).into_iter().enumerate() {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        // The receiver's layout: counter advances on opcode-0 frames only.
        let ok = frames
            .iter()
            .filter(|(h, _)| opcode(h) == 0)
            .enumerate()
            .filter(|(i, (h, b))| cipher.decrypt(Nonce::from_slice(&nonce(*i as u64)), Payload { msg: b, aad: h }).is_ok())
            .count();
        if ok > 0 && ok == video && winner.is_none() {
            winner = Some(key);
        }
        rows.push(Row { name: format!("{name}; AAD = 128-byte header; nonce = count of opcode-0 frames"), ok, tried: video, expected: ki == 0 });
        // Alternative: counter advances on every frame.
        let ok = frames
            .iter()
            .enumerate()
            .filter(|(i, (h, b))| opcode(h) == 0 && cipher.decrypt(Nonce::from_slice(&nonce(*i as u64)), Payload { msg: b, aad: h }).is_ok())
            .count();
        rows.push(Row { name: format!("{name}; AAD = 128-byte header; nonce = index over ALL frames"), ok, tried: video, expected: false });
    }
    (rows, winner)
}

// The opcode-1 body (codec configuration) is parsed by the one shared parser the host uses too,
// `ocbm_proto::seam::video_config`: a bare avcC (the iPhone's H.264 runs), an `hvc1` sample entry whose
// `hvcC` child box carries VPS/SPS/PPS (its HEVC runs, feature bit 42 + a display above 1080p), a bare
// configuration box, or a bare hvcC record.
use ocbm_proto::seam::{video_config, VideoCodec};

fn f32_at(h: &[u8], o: usize) -> f32 {
    f32::from_le_bytes([h[o], h[o + 1], h[o + 2], h[o + 3]])
}

fn extract_screen(frames: &[(&[u8], &[u8])], key: &[u8; 32], out: &std::path::Path, name: &str) {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let mut logged = [0u8; 256];
    let (mut es, mut len_size, mut codec) = (Vec::new(), 4usize, None::<VideoCodec>);
    let (mut ctr, mut video, mut idr, mut bad_nal) = (0u64, 0usize, 0usize, 0usize);
    let (mut wire_bytes, mut idr_at, mut dims) = (0usize, Vec::new(), String::from("no codec frame"));
    let (mut t_first, mut t_last) = (None, 0f64);
    for (h, b) in frames {
        let op = opcode(h);
        let secs = u64::from_le_bytes(h[8..16].try_into().unwrap()) as f64 / 4_294_967_296.0;
        if op != 0 {
            if logged[op as usize] < 3 {
                logged[op as usize] += 1;
                // Codec configuration is not a secret: show all of it (it is a few hundred bytes at most).
                println!(
                    "  opcode {op} frame: body {} B, flags 0x{:02x}, hdr[0..16]={}, body={}",
                    b.len(), h[5], hex(h, 16), hex(b, if op == 1 { 512 } else { 64 })
                );
            }
            if op == 1 {
                println!(
                    "  codec frame header floats: source {} x {} (@16/@20), {} x {} (@40/@44), x offset {} (@48), y offset {} (@52), output {} x {} (@56/@60)",
                    f32_at(h, 16), f32_at(h, 20), f32_at(h, 40), f32_at(h, 44), f32_at(h, 48), f32_at(h, 52), f32_at(h, 56), f32_at(h, 60)
                );
                dims = format!(
                    "source {}x{}, output {}x{}, offset x {} y {}",
                    f32_at(h, 16), f32_at(h, 20), f32_at(h, 56), f32_at(h, 60), f32_at(h, 48), f32_at(h, 52)
                );
                match video_config(b) {
                    Some(c) => {
                        if codec.is_some_and(|k| k != c.codec) {
                            println!("  the stream SWITCHED codec to {} mid-capture", c.codec.name());
                        }
                        (len_size, codec) = (c.len_size, Some(c.codec));
                        println!("  opcode 1 body parses as {}", c.describe());
                        es.extend_from_slice(&c.annexb());
                    }
                    None => println!(
                        "  opcode 1 body parses as NEITHER avcC NOR hvcC, bare or boxed (first bytes {}): no parameter sets recovered",
                        hex(b, 8)
                    ),
                }
            }
            continue;
        }
        let pt = cipher.decrypt(Nonce::from_slice(&nonce(ctr)), Payload { msg: b, aad: h });
        ctr += 1;
        let Ok(pt) = pt else { continue };
        video += 1;
        wire_bytes += 128 + b.len();
        if h[5] & 0x10 != 0 {
            idr += 1;
            idr_at.push((video, secs));
        }
        t_first.get_or_insert(secs);
        t_last = secs;
        // AVCC (length-prefixed NALs) -> Annex-B.
        let mut at = 0usize;
        while at + len_size <= pt.len() {
            let l = pt[at..at + len_size].iter().fold(0usize, |a, x| a << 8 | usize::from(*x));
            at += len_size;
            if l == 0 || at + l > pt.len() {
                bad_nal += 1;
                break;
            }
            es.extend_from_slice(&[0, 0, 0, 1]);
            es.extend_from_slice(&pt[at..at + l]);
            at += l;
        }
    }
    let span = t_last - t_first.unwrap_or(t_last);
    let fps = if span > 0.0 && video > 1 { (video - 1) as f64 / span } else { 0.0 };
    println!(
        "  {video} video frames decrypted ({idr} flagged IDR), header timestamps span {span:.2} s -> {fps:.2} fps; {bad_nal} frame(s) with a malformed NAL length"
    );
    // Without a configuration frame the codec is unknown: the frames are kept as `.es`, never labelled.
    let have_cfg = codec.is_some();
    if !have_cfg {
        println!("  NOT EXTRACTED as a playable stream: no usable codec configuration frame (opcode 1) in the capture");
    }
    let path = out.join(format!("{name}.{}", codec.map_or("es", |c| c.ext())));
    std::fs::write(&path, es).expect("write elementary stream");
    println!("  codec per the stream's own configuration frame: {}", codec.map_or("unknown (no configuration frame)", |c| c.name()));
    let codec = match codec {
        Some(VideoCodec::H264) => "h264",
        Some(VideoCodec::Hevc) => "hevc",
        None => "unknown",
    };
    // Bitrate over the CAPTURED part (headers + ciphertext of video frames over their timestamp span);
    // keyframes are the frames flagged 0x10 in the header.
    let kbps = if span > 0.0 { wire_bytes as f64 * 8.0 / span / 1000.0 } else { 0.0 };
    let key = match idr_at.as_slice() {
        [first, .., last] => format!(
            "{idr} keyframes, mean interval {:.2} s / {:.0} frames",
            (last.1 - first.1) / (idr - 1) as f64, (last.0 - first.0) as f64 / (idr - 1) as f64
        ),
        _ => format!("{idr} keyframe(s) in the capture, so no interval"),
    };
    println!(
        "RESULT video stream (captured part): codec {codec}, {video} frames over {span:.2} s = {fps:.2} fps, {kbps:.0} kbit/s, {key}; codec frame: {dims}"
    );
    println!("EXTRACT video={} codec={codec} frames={video} fps={fps:.3} have_config={have_cfg}", path.display());
}

// ---------------------------------------------------------------- main

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let out = a.iter().position(|x| x == "--out").and_then(|i| a.get(i + 1)).map(std::path::PathBuf::from);
    if a.len() < 2 || a[0].starts_with("--") {
        eprintln!("usage: capcheck <name>.bin <name>.meta [--out DIR]");
        std::process::exit(2);
    }
    let cap = std::fs::read(&a[0]).unwrap_or_else(|e| {
        eprintln!("{}: {e}", a[0]);
        std::process::exit(1)
    });
    let m = parse_meta(&std::fs::read_to_string(&a[1]).unwrap_or_else(|e| {
        eprintln!("{}: {e}", a[1]);
        std::process::exit(1)
    }));
    let name = std::path::Path::new(&a[0]).file_stem().and_then(|s| s.to_str()).unwrap_or("capture").to_string();
    println!(
        "capture {} B, stream type {}, streamConnectionID {:?}, shk {}, pairing secret {} ({})",
        cap.len(), m.ty, m.scid,
        m.shk.as_ref().map(|k| format!("{} B", k.len())).unwrap_or_else(|| "absent".into()),
        m.secret.as_ref().map(|k| format!("{} B", k.len())).unwrap_or_else(|| "absent".into()),
        m.secret_src
    );
    let fields: Vec<String> = m.fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
    println!("stream SETUP fields: {}", fields.join(" "));

    let rows = if m.ty == 110 {
        let frames = screen_frames(&cap);
        let mut ops = std::collections::BTreeMap::new();
        frames.iter().for_each(|(h, _)| *ops.entry(opcode(h)).or_insert(0usize) += 1);
        println!("{} whole screen frames; by opcode {ops:?} (0 = encrypted video frame; others are tested for nothing)", frames.len());
        let (rows, winner) = check_screen(&frames, &m);
        if let (Some(dir), Some(key)) = (&out, winner) {
            extract_screen(&frames, &key, dir, &name);
        }
        rows
    } else {
        let pkts = audio_packets(&cap);
        println!("{} whole packets (RTP 12-byte header assumed; first packet {})", pkts.len(), pkts.first().map(|p| hex(p, 16)).unwrap_or_default());
        let (rows, winner) = check_audio(&pkts, &m);
        if let (Some(dir), Some((key, aad))) = (&out, winner) {
            extract_audio(&pkts, &key, aad, &m, dir, &name);
        }
        rows
    };
    if rows.is_empty() {
        println!("VERDICT type {}: NOT VERIFIED — the .meta carries no key material to test with", m.ty);
        std::process::exit(1);
    }
    for r in rows.iter().filter(|r| r.ok > 0 || r.expected) {
        println!("  {:>6} / {:<6} authenticate under{}: {}", r.ok, r.tried, if r.expected { " [EXPECTED]" } else { " [alternative]" }, r.name);
    }
    println!("  ({} hypotheses tried; the others authenticated nothing)", rows.len());
    let exp = rows.iter().find(|r| r.expected);
    let full = |r: &&Row| r.tried > 0 && r.ok == r.tried;
    if let Some(r) = exp.filter(full) {
        println!("VERDICT type {}: VERIFIED — all {} authenticate under the receiver's existing rule: {}", m.ty, r.tried, r.name);
    } else if let Some(r) = rows.iter().find(full) {
        println!("VERDICT type {}: VERIFIED under an ALTERNATIVE (not the receiver's existing rule) — all {}: {}", m.ty, r.tried, r.name);
    } else if let Some(r) = rows.iter().max_by_key(|r| r.ok).filter(|r| r.ok > 0) {
        println!("VERDICT type {}: PARTIAL — {} of {} authenticate under: {}", m.ty, r.ok, r.tried, r.name);
        std::process::exit(1);
    } else {
        println!("VERDICT type {}: NOT VERIFIED — none of {} hypotheses authenticates any packet", m.ty, rows.len());
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use receiver::stream::encrypt_audio;

    fn audio_capture(key: &[u8; 32], n: u32, copies: usize) -> Vec<u8> {
        let mut cap = Vec::new();
        for i in 0..n {
            let mut hdr = [0x80u8, 0x60, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4];
            hdr[2..4].copy_from_slice(&(i as u16).to_be_bytes());
            hdr[4..8].copy_from_slice(&(i * 1024).to_be_bytes());
            let pkt = encrypt_audio(key, &hdr, &vec![i as u8; 200 + i as usize], &u64::from(i).to_le_bytes());
            for _ in 0..copies {
                cap.extend_from_slice(&((pkt.len() + 2) as u16).to_be_bytes());
                cap.extend_from_slice(&pkt);
            }
        }
        cap
    }

    #[test]
    fn buffered_audio_under_shk_is_the_expected_rule() {
        let shk = [7u8; 32];
        let mut cap = audio_capture(&shk, 20, 1);
        cap.extend_from_slice(&[0x01, 0x40, 0xAA]); // a truncated trailing packet is ignored
        let m = Meta { ty: 103, scid: Some(5), shk: Some(shk.to_vec()), secret: Some(vec![9; 64]), ..Meta::default() };
        let pkts = audio_packets(&cap);
        let (rows, winner) = check_audio(&pkts, &m);
        assert_eq!(pkts.len(), 20);
        let exp = rows.iter().find(|r| r.expected).unwrap();
        assert!(exp.name.starts_with("shk") && exp.name.contains("4..12"), "{}", exp.name);
        assert_eq!((exp.ok, exp.tried), (20, 20));
        assert!(rows.iter().filter(|r| !r.expected).all(|r| r.ok == 0), "no alternative may authenticate");
        assert!(winner.is_some());
        let wrong = Meta { shk: Some(vec![8; 32]), ..m };
        let (rows, winner) = check_audio(&pkts, &wrong);
        assert!(rows.iter().all(|r| r.ok == 0) && winner.is_none());
    }

    #[test]
    fn realtime_audio_expected_rule_is_the_derived_output_key_and_alternatives_are_labelled() {
        let (secret, scid, other) = (vec![3u8; 64], 77u64, 99u64);
        let m = |scid| Meta {
            ty: 96, scid: Some(scid), scids: vec![(110, other), (96, 77)], secret: Some(secret.clone()),
            fields: vec![("audioFormat".into(), "4194304".into()), ("spf".into(), "1024".into())],
            ..Meta::default()
        };
        // Keyed the way the receiver keys realtime audio: expected rule verifies.
        let cap = audio_capture(&derive_stream_keys(&secret, scid).output, 30, 3);
        let pkts = audio_packets(&cap);
        let (rows, winner) = check_audio(&pkts, &m(scid));
        let exp = rows.iter().find(|r| r.expected).unwrap();
        assert!(exp.name.contains("DataStream-Salt77 [this stream], Output") && exp.name.contains("4..12"), "{}", exp.name);
        assert_eq!((exp.ok, exp.tried), (90, 90));
        // Extraction drops the two repeats of every packet and wraps AAC-LC in ADTS.
        let dir = std::env::temp_dir().join(format!("capcheck-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (key, aad) = winner.unwrap();
        extract_audio(&pkts, &key, aad, &m(scid), &dir, "t96");
        let aac = std::fs::read(dir.join("t96.aac")).unwrap();
        assert_eq!(&aac[..2], &[0xFF, 0xF1]);
        assert_eq!(aac.len(), (0..30).map(|i| 7 + 200 + i).sum::<usize>());
        assert_eq!(((aac[3] as usize & 3) << 11) | ((aac[4] as usize) << 3) | (aac[5] as usize >> 5), 207, "ADTS frame length");
        std::fs::remove_dir_all(&dir).unwrap();
        // Keyed with the SCREEN stream's id instead: only a labelled alternative verifies.
        let cap = audio_capture(&derive_stream_keys(&secret, other).output, 10, 1);
        let pkts = audio_packets(&cap);
        let (rows, _) = check_audio(&pkts, &m(scid));
        assert_eq!(rows.iter().find(|r| r.expected).unwrap().ok, 0);
        let hit: Vec<&Row> = rows.iter().filter(|r| r.ok == 10).collect();
        assert!(hit.len() == 1 && hit[0].name.contains("DataStream-Salt99 [the type-110 stream's id], Output"), "{}", hit[0].name);
    }

    #[test]
    fn screen_verifies_and_extracts_annex_b() {
        let (secret, scid) = (vec![3u8; 64], 0x1122_3344_5566u64);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&derive_stream_keys(&secret, scid).output));
        // avcC: one 4-byte SPS, one 2-byte PPS, 4-byte NAL lengths.
        let avcc = [1u8, 0x64, 0, 0x28, 0xFF, 0xE1, 0, 4, 0x67, 1, 2, 3, 1, 0, 2, 0x68, 9];
        let (mut cap, mut ctr) = (Vec::new(), 0u64);
        for (i, op) in [1u8, 0, 0, 5, 0].iter().enumerate() {
            let mut hdr = [0u8; 128];
            hdr[4] = *op;
            hdr[8..16].copy_from_slice(&(((10 + i as u64) << 32) / 5).to_le_bytes()); // 0.2 s apart
            let body = match op {
                0 => {
                    // one 5-byte NAL, AVCC framed
                    let plain = [0u8, 0, 0, 5, 0x65, i as u8, 2, 3, 4];
                    hdr[..4].copy_from_slice(&(plain.len() as u32 + 16).to_le_bytes());
                    let c = cipher.encrypt(Nonce::from_slice(&nonce(ctr)), Payload { msg: &plain, aad: &hdr }).unwrap();
                    ctr += 1;
                    c
                }
                1 => avcc.to_vec(),
                _ => vec![0xEE; 40],
            };
            if *op != 0 {
                hdr[..4].copy_from_slice(&(body.len() as u32).to_le_bytes());
            }
            cap.extend_from_slice(&hdr);
            cap.extend_from_slice(&body);
        }
        let m = Meta { ty: 110, scid: Some(scid), secret: Some(secret), ..Meta::default() };
        let frames = screen_frames(&cap);
        let (rows, winner) = check_screen(&frames, &m);
        assert_eq!(frames.len(), 5);
        let exp = rows.iter().find(|r| r.expected).unwrap();
        assert!(exp.name.contains("Output") && exp.name.contains("opcode-0"), "{}", exp.name);
        assert_eq!((exp.ok, exp.tried), (3, 3));
        assert!(rows.iter().filter(|r| r.name.contains("Input")).all(|r| r.ok == 0));
        let dir = std::env::temp_dir().join(format!("capcheck-test-v-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        extract_screen(&frames, &winner.unwrap(), &dir, "t110");
        let es = std::fs::read(dir.join("t110.h264")).unwrap();
        // SPS, PPS, then three IDR NALs, each behind a start code.
        assert_eq!(&es[..8], &[0, 0, 0, 1, 0x67, 1, 2, 3]);
        assert_eq!(&es[8..14], &[0, 0, 0, 1, 0x68, 9]);
        assert_eq!(es.len(), 8 + 6 + 3 * 9);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn hevc_codec_frame_is_recognised_and_extracted_as_annex_b() {
        // hvcC: 22 fixed bytes (Main 10, level 4.0 = 120, 10-bit, 4-byte NAL lengths), three arrays.
        let mut hvcc = vec![1u8, 0x02, 0x20, 0, 0, 0, 0x90, 0, 0, 0, 0, 0, 120, 0xF0, 0, 0xFC, 0xFD, 0xFA, 0xFA, 0, 0, 0x0F, 3];
        for (ty, nal) in [(32u8, &[0x40u8, 1, 0xAA][..]), (33, &[0x42, 1, 0xBB, 0xBC]), (34, &[0x44, 1, 0xCC])] {
            hvcc.extend_from_slice(&[0x80 | ty, 0, 1]);
            hvcc.extend_from_slice(&(nal.len() as u16).to_be_bytes());
            hvcc.extend_from_slice(nal);
        }
        let cfg = |b: &[u8]| video_config(b).map(|c| (c.codec, c.len_size, c.nals.len()));
        assert_eq!(cfg(&hvcc), Some((VideoCodec::Hevc, 4, 3)), "a bare hvcC is HEVC, never avcC");
        // ...also when wrapped in an ISO box, and an avcC still parses as H.264.
        let mut boxed = (hvcc.len() as u32 + 8).to_be_bytes().to_vec();
        boxed.extend_from_slice(b"hvcC");
        boxed.extend_from_slice(&hvcc);
        assert_eq!(cfg(&boxed), Some((VideoCodec::Hevc, 4, 3)));
        let avcc = [1u8, 0x64, 0, 0x28, 0xFF, 0xE1, 0, 4, 0x67, 1, 2, 3, 1, 0, 2, 0x68, 9];
        assert_eq!(cfg(&avcc), Some((VideoCodec::H264, 4, 2)));
        assert!(video_config(&[1, 2, 3, 4, 5, 6, 7, 8, 9]).is_none());

        let (secret, scid) = (vec![5u8; 64], 4242u64);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&derive_stream_keys(&secret, scid).output));
        let mut cap = Vec::new();
        let put = |op: u8, body: Vec<u8>, t: u64| {
            let mut hdr = [0u8; 128];
            hdr[..4].copy_from_slice(&(body.len() as u32).to_le_bytes());
            hdr[4] = op;
            hdr[8..16].copy_from_slice(&(t << 30).to_le_bytes()); // 0.25 s steps
            (hdr, body)
        };
        let (h, b) = put(1, hvcc.clone(), 40);
        cap.extend_from_slice(&h);
        cap.extend_from_slice(&b);
        for i in 0..3u64 {
            // one 6-byte IDR_W_RADL NAL (type 19: header 0x26 0x01), 4-byte length prefix
            let plain = [0u8, 0, 0, 6, 0x26, 1, i as u8, 7, 8, 9];
            let (mut h, _) = put(0, vec![0; plain.len() + 16], 40 + i);
            h[..4].copy_from_slice(&(plain.len() as u32 + 16).to_le_bytes());
            let c = cipher.encrypt(Nonce::from_slice(&nonce(i)), Payload { msg: &plain, aad: &h }).unwrap();
            cap.extend_from_slice(&h);
            cap.extend_from_slice(&c);
        }
        let m = Meta { ty: 110, scid: Some(scid), secret: Some(secret), ..Meta::default() };
        let frames = screen_frames(&cap);
        let (rows, winner) = check_screen(&frames, &m);
        assert_eq!((rows.iter().find(|r| r.expected).unwrap().ok, frames.len()), (3, 4));
        let dir = std::env::temp_dir().join(format!("capcheck-test-hevc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        extract_screen(&frames, &winner.unwrap(), &dir, "t110");
        assert!(!dir.join("t110.h264").exists(), "an HEVC stream is not written as .h264");
        let es = std::fs::read(dir.join("t110.h265")).unwrap();
        // VPS, SPS, PPS, then the three slices, each behind a start code.
        assert_eq!(&es[..7], &[0, 0, 0, 1, 0x40, 1, 0xAA]);
        assert_eq!(&es[7..15], &[0, 0, 0, 1, 0x42, 1, 0xBB, 0xBC]);
        assert_eq!(&es[15..22], &[0, 0, 0, 1, 0x44, 1, 0xCC]);
        assert_eq!(es.len(), 22 + 3 * 10);
        assert_eq!(&es[22..28], &[0, 0, 0, 1, 0x26, 1]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The verified real HEVC layout (the defect this fixes: it was labelled "NEITHER avcC NOR hvcC" and
    /// then written as .h264): `[u32 size]["hvc1"][78-byte visual sample entry][u32 size]["hvcC"][record]`
    /// followed by another child box, as the opcode-1 body. Extracted as .h265 with VPS/SPS/PPS first.
    #[test]
    fn hvc1_wrapped_hvcc_is_hevc_and_extracted_as_h265() {
        let mut hvcc = vec![1u8, 0x01, 0x60, 0, 0, 0, 0xB0, 0, 0, 0, 0, 0, 0x7B, 0xF0, 0, 0xFC, 0xFD, 0xF8, 0xF8, 0, 0, 0x0F, 3];
        for (ty, nal) in [(32u8, &[0x40u8, 1, 0x0C][..]), (33, &[0x42, 1, 0x01, 0x60]), (34, &[0x44, 1, 0xC1])] {
            hvcc.extend_from_slice(&[0xA0 | ty, 0, 1]); // array_completeness set, as on the wire
            hvcc.extend_from_slice(&(nal.len() as u16).to_be_bytes());
            hvcc.extend_from_slice(nal);
        }
        let mut body = vec![0u8; 8 + 78];
        body[4..8].copy_from_slice(b"hvc1");
        body[8 + 24..8 + 26].copy_from_slice(&994u16.to_be_bytes());
        body[8 + 26..8 + 28].copy_from_slice(&2160u16.to_be_bytes());
        body.extend_from_slice(&(hvcc.len() as u32 + 8).to_be_bytes());
        body.extend_from_slice(b"hvcC");
        body.extend_from_slice(&hvcc);
        body.extend_from_slice(&[0, 0, 0, 22]); // a trailing child box (the real body has 22 bytes there)
        body.extend_from_slice(b"fiel");
        body.extend_from_slice(&[0; 14]);
        let n = body.len() as u32;
        body[..4].copy_from_slice(&n.to_be_bytes());
        let c = video_config(&body).unwrap();
        assert_eq!((c.codec, c.len_size, c.sample_entry), (VideoCodec::Hevc, 4, Some(*b"hvc1")));
        assert!(c.describe().contains("level 4.1"), "{}", c.describe());

        let (secret, scid) = (vec![9u8; 64], 77u64);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&derive_stream_keys(&secret, scid).output));
        let mut cap = Vec::new();
        let mut hdr = [0u8; 128];
        hdr[..4].copy_from_slice(&n.to_le_bytes());
        hdr[4] = 1;
        cap.extend_from_slice(&hdr);
        cap.extend_from_slice(&body);
        for i in 0..2u64 {
            let plain = [0u8, 0, 0, 4, 0x26, 1, i as u8, 7];
            let mut h = [0u8; 128];
            h[..4].copy_from_slice(&(plain.len() as u32 + 16).to_le_bytes());
            h[8..16].copy_from_slice(&((40 + i) << 30).to_le_bytes());
            cap.extend_from_slice(&h);
            cap.extend_from_slice(&cipher.encrypt(Nonce::from_slice(&nonce(i)), Payload { msg: &plain, aad: &h }).unwrap());
        }
        let m = Meta { ty: 110, scid: Some(scid), secret: Some(secret), ..Meta::default() };
        let frames = screen_frames(&cap);
        let (_, winner) = check_screen(&frames, &m);
        let dir = std::env::temp_dir().join(format!("capcheck-test-hvc1-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        extract_screen(&frames, &winner.unwrap(), &dir, "t110");
        assert!(!dir.join("t110.h264").exists() && !dir.join("t110.es").exists());
        let es = std::fs::read(dir.join("t110.h265")).unwrap();
        assert_eq!(&es[..7], &[0, 0, 0, 1, 0x40, 1, 0x0C]);
        assert_eq!(&es[7..15], &[0, 0, 0, 1, 0x42, 1, 0x01, 0x60]);
        assert_eq!(&es[15..22], &[0, 0, 0, 1, 0x44, 1, 0xC1]);
        assert_eq!(es.len(), 22 + 2 * 8);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn meta_round_trip() {
        let m = parse_meta("type=96\nvariant=B7\nscid=42\nscid_110=7\nscid_96=42\nf_ct=4\nf_audioFormat=4194304\nshk=0a0b\nsecret=ff\nsecret_src=x y\n");
        assert_eq!((m.ty, m.scid, m.shk.clone(), m.secret.clone(), m.secret_src.as_str()), (96, Some(42), Some(vec![10, 11]), Some(vec![255]), "x y"));
        assert_eq!(m.scids, vec![(110, 7), (96, 42)]);
        assert_eq!((m.field("ct"), aac_lc_rate(&m)), (Some("4"), Some(44_100)));
    }
}
