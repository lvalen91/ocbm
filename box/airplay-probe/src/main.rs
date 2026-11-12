//! airplay-probe — AirPlay "Phase 0" probe. Advertises a NON-car AirPlay receiver with a chosen
//! feature mask and logs everything an iPhone sends to it: request line, headers, body size, the
//! first 64 body bytes, plus a decode of pairing TLVs and binary plists.
//!
//! HARD LIMITS, enforced by construction (owner-scoped):
//!   * MFi auth-setup: ALLOWED since 2026-11-12 (owner lifted hard-limit 1 for this path only). It
//!     reuses `mfi::sap::MfiSapServer` and `mfi-i2c-local`'s `try_cert`/`try_sign` — the genuine
//!     on-board coprocessor over `/dev/i2c-1` under the shared `/tmp/carplay_mfi.lock`. No crypto
//!     and no I2C register sequence are written here; the chip's private key never leaves it. Only
//!     variants that advertise bit 26 answer it (`nomfi` disables, `mfi` forces).
//!   * No FairPlay: `fp-setup` is logged and refused. No Apple key blob exists in this file.
//!   * No new crypto: pairing is `pairing::setup::PairSetupServer` / `pairing::verify::
//!     PairVerifyServer` unchanged. `PairSetupServer` has NO transient mode (it ignores the Flags
//!     TLV and keeps the SRP session key private), so a transient pair-setup is driven by composing
//!     the crate's own public `pairing::srp::SrpServer` + `pairing::tlv` and handing the SRP key to
//!     the existing `rtsp::control` channel. That is glue over existing primitives, nothing more.
//!   * RAM only: the log path defaults to /tmp; nothing else is written anywhere. No flash writes.
//!
//! usage: airplay-probe [--name BASE] [--log PATH] [--port-base N] SPEC [SPEC...]
//!   SPEC = TAG[/MASK[/opt...]]; MASK = A | B | 0xLO,0xHI, then any +N / -N bit edits (`B-26+7`).
//!   One process advertises every SPEC at once as `BASE-TAG` on its own port with its own fresh
//!   identity, so variants that differ in ONE thing can be compared side by side in one log.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceInfo};
use mfi::auth_client::MfiSigner;
use mfi::sap::MfiSapServer;
use pairing::crypto::{tlv_error, tlv_type};
use pairing::setup::{PairSetupServer, PeerSaver};
use pairing::srp::SrpServer;
use pairing::tlv;
use pairing::verify::{Identity, PairVerifyServer, PeerStore};
use rtsp::control::{derive_control_keys, ControlChannel, ControlKeys};
use rtsp::message::{Request, Response};

// ---------------------------------------------------------------- logging

static LOG: OnceLock<Mutex<File>> = OnceLock::new();
static T0: OnceLock<Instant> = OnceLock::new();
static STOP: AtomicBool = AtomicBool::new(false);

fn log_line(s: &str) {
    let t = T0.get().map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("[{epoch} +{t:9.3}] {s}\n");
    // One lock for both sinks so lines from concurrent connections never interleave mid-line.
    if let Some(m) = LOG.get() {
        let mut f = m.lock().unwrap_or_else(|p| p.into_inner());
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
        let _ = std::io::stdout().write_all(line.as_bytes());
        let _ = std::io::stdout().flush();
    } else {
        print!("{line}");
    }
}
macro_rules! lg { ($($a:tt)*) => { log_line(&format!($($a)*)) } }

fn hex(b: &[u8], max: usize) -> String {
    let mut s = String::new();
    for x in b.iter().take(max) {
        s.push_str(&format!("{x:02x}"));
    }
    if b.len() > max {
        s.push_str("..");
    }
    s
}

fn ascii(b: &[u8], max: usize) -> String {
    b.iter()
        .take(max)
        .map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '.' })
        .collect()
}

fn urandom(buf: &mut [u8]) {
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(buf))
        .expect("/dev/urandom");
}

// ---------------------------------------------------------------- config

#[derive(Clone)]
struct Cfg {
    tag: String,
    label: String,
    /// Add `Apple-Jack-Status` to every response (legacy RAOP receivers send it).
    jack: bool,
    /// Extra `/info` keys for this variant only (`info.<key>=<value>` in the SPEC): an all-digit
    /// value is sent as an integer, `true`/`false` as a boolean, anything else as a string.
    extra_info: Vec<(String, String)>,
    /// `display=WxH@R`: the advertised display's pixel size and maxFPS. None = the long-standing
    /// default (1920x1080, maxFPS 30, refreshRate 1/60), kept byte-identical so B7 stays a control.
    display: Option<(u32, u32, u32)>,
    /// `maxfps=N`: maxFPS on its own, overriding the R of `display=`.
    maxfps: Option<u32>,
    /// `disp.<key>=<value>`: extra keys inside the `displays[0]` dictionary (typed like `info.`).
    extra_disp: Vec<(String, String)>,
    /// Answer `/auth-setup` with the genuine on-board MFi coprocessor (default on when bit 26 is
    /// advertised). `fp-setup` is never answered regardless of this.
    mfi_auth: bool,
    /// Where this variant's paired controllers are kept (RAM, beside the log); None in tests.
    peers_path: Option<std::path::PathBuf>,
    /// ONE Ed25519 identity per variant: `pk` in the TXT record and in /info is its public key,
    /// and pair-setup M6 / pair-verify M2 sign with it. They cannot disagree.
    identity: &'static Identity,
    features: u64,
    name: String,
    port: u16,
    srcvers: String,
    protovers: String,
    flags: u32,
    model: String,
    /// Advertise the AirPlay-2 style TXT keys (pk, vv, acl, rsf, gid, psi). Mask A deliberately keeps
    /// the exact CarPlay TXT key set so the ONLY variable against the CarPlay ad is the feature mask.
    ap2_txt: bool,
    raop: bool,
    pin: String,
    deny_status: u16,
    device_id: String,
    pi: String,
    pk: [u8; 32],
}

impl Cfg {
    fn features_txt(&self) -> String {
        format!("0x{:X},0x{:X}", self.features & 0xFFFF_FFFF, self.features >> 32)
    }
    fn bit(&self, n: u32) -> bool {
        self.features >> n & 1 == 1
    }
    fn airplay_txt(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = vec![
            ("deviceid".into(), self.device_id.clone()),
            ("features".into(), self.features_txt()),
            ("flags".into(), format!("0x{:X}", self.flags)),
            ("model".into(), self.model.clone()),
            ("protovers".into(), self.protovers.clone()),
            ("pi".into(), self.pi.clone()),
            ("srcvers".into(), self.srcvers.clone()),
        ];
        if self.ap2_txt {
            v.push(("pk".into(), hex(&self.pk, 32)));
            v.push(("vv".into(), "2".into()));
            v.push(("acl".into(), "0".into()));
            v.push(("rsf".into(), "0x0".into()));
            v.push(("gid".into(), self.pi.clone()));
            v.push(("psi".into(), self.pi.clone()));
            v.push(("gcgl".into(), "0".into()));
            v.push(("igl".into(), "0".into()));
            v.push(("fex".into(), b64_nopad(&self.features.to_le_bytes())));
        }
        v
    }
    fn raop_txt(&self) -> Vec<(String, String)> {
        // et = key-encryption types offered. 0 none; 3 FairPlay only if the mask claims it (bit 14);
        // 4 MFi-SAP only if the mask claims it (bit 26). Never 1: RSA needs a key we do not have.
        let mut et = String::from("0");
        if self.bit(14) {
            et.push_str(",3");
        }
        if self.bit(26) {
            et.push_str(",4");
        }
        [
            ("cn", "0,1,2,3"),
            ("da", "true"),
            ("et", et.as_str()),
            ("ft", self.features_txt().as_str()),
            ("sf", format!("0x{:X}", self.flags).as_str()),
            ("md", "0,1,2"),
            ("am", self.model.as_str()),
            ("pk", hex(&self.pk, 32).as_str()),
            ("tp", "UDP"),
            ("vn", "65537"),
            ("vs", self.srcvers.as_str()),
            ("vv", "2"),
            ("ch", "2"),
            ("sr", "44100"),
            ("ss", "16"),
            ("pw", "false"),
            ("sm", "false"),
            ("sv", "false"),
            ("txtvers", "1"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }
}

fn parse_hex(s: &str) -> Option<u64> {
    u64::from_str_radix(s.trim().trim_start_matches("0x").trim_start_matches("0X"), 16).ok()
}

fn b64_nopad(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::new();
    for c in b.chunks(3) {
        let n = u32::from(c[0]) << 16 | u32::from(*c.get(1).unwrap_or(&0)) << 8 | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..=c.len() {
            o.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    o
}

fn usage() -> ! {
    eprintln!(
        "usage: airplay-probe [--name BASE] [--log PATH] [--port-base N] SPEC [SPEC...]\n\
         SPEC = TAG[/MASK[/opt...]]   (MASK defaults to TAG, so plain `A` and `B` work)\n\
         MASK = A | B | 0xLO,0xHI, then +N / -N feature-bit edits, e.g. A+19, B-26+7\n\
         \x20 A = CarPlay mask 0x44440B80,0x61 minus Car(32) and CarPlayControl(37) = 0x44440B80,0x40\n\
         \x20 B = 0x1c340445f8a00 (airplay2-receiver dev note 'MFi - HK and coreutils and transient')\n\
         \x20 M = 0x40000280,0x10040 (bits 7,9,30,38,48: minimum pairing-capable sink, no MFi/FairPlay)\n\
         opts: raop ap2txt noap2txt jack mfi nomfi srcvers=V protovers=V model=M flags=0xN pin=NNNN\n\
         \x20     deny=N info.<key>=<value> (an extra /info key for this variant)\n\
         \x20     display=WxH@R (advertised display pixels and maxFPS; default 1920x1080, maxFPS 30)\n\
         \x20     maxfps=N  disp.<key>=<value> (an extra key inside displays[0])\n\
         global: --cap-mb N (per-stream capture ceiling in MB, default 12)\n\
         each SPEC is advertised as BASE-TAG on port-base + its position; its identity (deviceid,\n\
         pi, Ed25519 key = the advertised pk AND the pair-setup/pair-verify key) is kept in RAM next\n\
         to the log, keyed by the exact SPEC text, so it is stable until the SPEC changes or a reboot"
    );
    std::process::exit(2);
}

/// `A`, `B` or `0xLO,0xHI`, followed by any number of `+N` / `-N` feature-bit edits.
fn parse_mask(m: &str) -> Option<(u64, &'static str, &'static str, bool)> {
    let cut = m.find(['+', '-']).unwrap_or(m.len());
    let (base, mut edits) = m.split_at(cut);
    let (mut f, srcvers, protovers, ap2) = match base {
        "A" => (0x40_4444_0B80u64, "320.17", "1.0", false),
        "B" => (0x1_c340_445f_8a00u64, "366.0", "1.1", true),
        // M = the minimum pairing-capable sink: Screen(7) Audio(9) UnifiedAdvertiserInfo(30)
        // CoreUtilsPairingAndEncryption(38) TransientPairing(48). No MFi(26), no FairPlay(14).
        "M" => (0x1_0040_4000_0280u64, "366.0", "1.1", true),
        other => {
            let (lo, hi) = other.split_once(',').unwrap_or((other, "0"));
            let (lo, hi) = (parse_hex(lo)?, parse_hex(hi)?);
            if lo > 0xFFFF_FFFF || hi > 0xFFFF_FFFF {
                return None;
            }
            (hi << 32 | lo, "366.0", "1.1", true)
        }
    };
    while !edits.is_empty() {
        let set = edits.starts_with('+');
        let rest = &edits[1..];
        let end = rest.find(['+', '-']).unwrap_or(rest.len());
        let bit: u32 = rest[..end].parse().ok().filter(|b| *b < 64)?;
        if set {
            f |= 1 << bit;
        } else {
            f &= !(1 << bit);
        }
        edits = &rest[end..];
    }
    Some((f, srcvers, protovers, ap2))
}

fn fnv64(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, x| (h ^ u64::from(*x)).wrapping_mul(0x0000_0100_0000_01b3))
}

/// 54 bytes of identity material (32 Ed25519 seed + 22 for device id and pi). With `id_dir` it is
/// read back from, or created in, `<id_dir>/<tag>-<hash of the SPEC text>`: STABLE for an
/// unchanged variant (iOS keys a pairing on pi/pk, so a restart must not look like an impostor),
/// FRESH for a changed one (iOS also caches features by identity, so a changed variant must not
/// be read through the old one's cache). The directory sits beside the log, i.e. in RAM.
fn identity_material(spec: &str, tag: &str, id_dir: Option<&std::path::Path>) -> ([u8; 54], Option<std::path::PathBuf>) {
    let mut m = [0u8; 54];
    let Some(dir) = id_dir else {
        urandom(&mut m);
        return (m, None);
    };
    let safe: String = tag.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let base = dir.join(format!("{safe}-{:016x}", fnv64(spec.as_bytes())));
    match std::fs::read(&base) {
        Ok(b) if b.len() == m.len() => m.copy_from_slice(&b),
        _ => {
            urandom(&mut m);
            let _ = std::fs::create_dir_all(dir);
            if let Err(e) = std::fs::write(&base, m) {
                lg!("variant {tag}: identity NOT kept ({e}); it is fresh this run");
            }
        }
    }
    (m, Some(base.with_extension("peers")))
}

fn parse_spec(spec: &str, base_name: &str, port: u16, id_dir: Option<&std::path::Path>) -> Cfg {
    let mut parts = spec.split('/');
    let tag = parts.next().unwrap_or_default().to_string();
    let mask = parts.next().unwrap_or(&tag).to_string();
    if tag.is_empty() {
        usage();
    }
    let (features, srcvers, protovers, ap2_txt) = parse_mask(&mask).unwrap_or_else(|| usage());
    let (material, peers_path) = identity_material(spec, &tag, id_dir);
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&material[..32]);
    let r = &material[32..];
    // Locally-administered unicast MAC: cannot collide with a real device.
    let device_id = format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        (r[0] | 0x02) & 0xFE, r[1], r[2], r[3], r[4], r[5]
    );
    let pi = format!(
        "{}-{}-{}-{}-{}",
        hex(&r[6..10], 4), hex(&r[10..12], 2), hex(&r[12..14], 2), hex(&r[14..16], 2), hex(&r[16..22], 6)
    );
    let identity: &'static Identity = Box::leak(Box::new(Identity::new(pi.as_bytes().to_vec(), seed)));
    let mut c = Cfg {
        name: format!("{base_name}-{tag}"),
        tag,
        label: mask,
        jack: false,
        extra_info: Vec::new(),
        display: None,
        maxfps: None,
        extra_disp: Vec::new(),
        peers_path,
        identity,
        features,
        port,
        srcvers: srcvers.into(),
        protovers: protovers.into(),
        flags: 0x4,
        model: "OCBM-Probe".into(),
        ap2_txt,
        raop: false,
        pin: "3939".into(),
        deny_status: 403,
        // Owner lifted hard-limit (1) for auth-setup ONLY (2026-11-12): a variant that advertises
        // the MFi-SAP bit (26) answers /auth-setup with the genuine on-board coprocessor by default.
        // fp-setup (FairPlay) stays refused unconditionally. `nomfi`/`mfi` override per variant.
        mfi_auth: features >> 26 & 1 == 1,
        device_id,
        pi,
        pk: identity.public_key(),
    };
    for o in parts {
        let (k, v) = o.split_once('=').unwrap_or((o, ""));
        match k {
            "raop" => c.raop = true,
            "ap2txt" => c.ap2_txt = true,
            "noap2txt" => c.ap2_txt = false,
            "jack" => c.jack = true,
            "mfi" => c.mfi_auth = true,
            "nomfi" => c.mfi_auth = false,
            "srcvers" => c.srcvers = v.into(),
            "protovers" => c.protovers = v.into(),
            "model" => c.model = v.into(),
            "flags" => c.flags = parse_hex(v).unwrap_or_else(|| usage()) as u32,
            "pin" => c.pin = v.into(),
            "deny" => c.deny_status = v.parse().unwrap_or_else(|_| usage()),
            "display" => {
                // WxH@R, e.g. 3840x2160@60
                let parsed = v.split_once('@').and_then(|(wh, r)| {
                    let (w, h) = wh.split_once('x')?;
                    Some((w.parse().ok()?, h.parse().ok()?, r.parse().ok().filter(|r| *r > 0)?))
                });
                c.display = Some(parsed.unwrap_or_else(|| usage()));
            }
            "maxfps" => c.maxfps = Some(v.parse().unwrap_or_else(|_| usage())),
            _ if k.starts_with("info.") && k.len() > 5 => c.extra_info.push((k[5..].to_string(), v.to_string())),
            _ if k.starts_with("disp.") && k.len() > 5 => c.extra_disp.push((k[5..].to_string(), v.to_string())),
            _ => usage(),
        }
    }
    c
}

/// `MfiSigner` backed by the genuine on-board coprocessor over `mfi-i2c-local` — the SAME chip
/// access and the SAME `/tmp/carplay_mfi.lock` the receiver's AirPlay-tunnel path uses. No crypto
/// and no I2C register sequence is written here: `try_cert`/`try_sign` relay the phone's challenge
/// to the chip and return the chip's own signed response; the private key never leaves the chip.
struct ChipSigner;
impl MfiSigner for ChipSigner {
    fn copy_certificate(&mut self) -> std::io::Result<Vec<u8>> {
        chip_retry("cert", mfi_i2c_local::try_cert).map_err(|e| std::io::Error::other(format!("MFi chip cert: {e:?}")))
    }
    fn create_signature(&mut self, digest: &[u8]) -> std::io::Result<Vec<u8>> {
        chip_retry("sign", || mfi_i2c_local::try_sign(digest))
            .map_err(|e| std::io::Error::other(format!("MFi chip sign: {e:?}")))
    }
}

/// The receiver's own `mfi_retry` policy (`receiver/src/iap_tunnel.rs`), which `mfi-i2c-local`
/// expects of its callers: a chip NAK is retried, up to 3 attempts 20 ms apart (the coprocessor
/// NAKs while asleep or busy); a LOCK failure is never retried, because waiting out the same
/// holder again cannot help. Each attempt takes and releases the shared lock on its own.
fn chip_retry<T>(
    what: &str,
    mut op: impl FnMut() -> Result<T, mfi_i2c_local::MfiError>,
) -> Result<T, mfi_i2c_local::MfiError> {
    let mut last = mfi_i2c_local::MfiError::Chip;
    for attempt in 1..=3u32 {
        match op() {
            Ok(v) => {
                if attempt > 1 {
                    lg!("MFi {what}: chip answered on attempt {attempt}/3");
                }
                return Ok(v);
            }
            Err(mfi_i2c_local::MfiError::Chip) => {
                lg!("MFi {what}: attempt {attempt}/3 failed (chip NAK/timeout)");
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                lg!("MFi {what}: {e:?} — not retried (lock, not chip)");
                last = e;
                break;
            }
        }
    }
    Err(last)
}

/// Read-only chip liveness used at startup when any variant will answer auth-setup: fetch the
/// certificate once (under the shared lock) and report its length, so a dead chip is caught before
/// the iPhone ever asks. Does NOT sign anything.
fn chip_selftest() {
    for p in ["ocbmd", "carplayd", "airplayd", "iap2d", "btd", "mfid"] {
        if std::path::Path::new("/proc").read_dir().ok().into_iter().flatten().flatten().any(|e| {
            std::fs::read_to_string(e.path().join("comm")).map(|c| c.trim() == p).unwrap_or(false)
        }) {
            lg!("MFi self-test SKIPPED: chip user '{p}' is running — not touching the coprocessor");
            return;
        }
    }
    match chip_retry("self-test cert", mfi_i2c_local::try_cert) {
        Ok(cert) => lg!("MFi self-test: chip responded, certificate {} B first8={}", cert.len(), hex(&cert, 8)),
        Err(e) => lg!("MFi self-test FAILED: {e:?} — auth-setup variants will return 500 to the phone"),
    }
}

fn parse_args() -> Vec<Cfg> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (mut log, mut name, mut port_base) = ("/tmp/airplay-probe.log".to_string(), "OCBM".to_string(), 7000u16);
    let mut specs = Vec::new();
    let mut i = 0;
    while i < a.len() {
        let opt = a[i].clone();
        let mut val = || {
            i += 1;
            a.get(i).cloned().unwrap_or_else(|| usage())
        };
        match opt.as_str() {
            "--log" => log = val(),
            "--name" => name = val(),
            "--port-base" => port_base = val().parse().unwrap_or_else(|_| usage()),
            "--cap-mb" => {
                let mb: u64 = val().parse().ok().filter(|m| (1..=64).contains(m)).unwrap_or_else(|| usage());
                CAP_MAX_BYTES.store(mb * 1024 * 1024, Ordering::Relaxed);
            }
            s if s.starts_with("--") => usage(),
            _ => specs.push(opt),
        }
        i += 1;
    }
    if specs.is_empty() || specs.len() > 16 {
        usage();
    }
    let f = std::fs::OpenOptions::new().create(true).append(true).open(&log).unwrap_or_else(|e| {
        eprintln!("cannot open log {log}: {e}");
        std::process::exit(1);
    });
    let _ = LOG.set(Mutex::new(f));
    lg!("==== airplay-probe start: {} variant(s) log={log} pid={}", specs.len(), std::process::id());
    let _ = CAP_DIR.set(std::path::Path::new(&log).with_extension("cap"));
    let id_dir = std::path::Path::new(&log).with_extension("id");
    specs.iter().enumerate().map(|(n, s)| parse_spec(s, &name, port_base + n as u16, Some(&id_dir))).collect()
}

// ---------------------------------------------------------------- pairing store (RAM only)

#[derive(Clone, Default)]
struct Peers(Arc<Mutex<HashMap<Vec<u8>, [u8; 32]>>>, Option<std::path::PathBuf>);

fn unhex(s: &str) -> Option<Vec<u8>> {
    (s.len() % 2 == 0).then(|| (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect())?
}

impl Peers {
    /// Controllers paired with this variant's identity earlier in this boot (non-transient
    /// pair-setup only; a transient pairing leaves nothing to keep).
    fn load(path: Option<std::path::PathBuf>) -> Self {
        let mut map = HashMap::new();
        if let Some(text) = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) {
            for l in text.lines() {
                let Some((id, pk)) = l.split_once(' ') else { continue };
                if let (Some(id), Some(pk)) = (unhex(id), unhex(pk).and_then(|v| <[u8; 32]>::try_from(v).ok())) {
                    map.insert(id, pk);
                }
            }
        }
        Peers(Arc::new(Mutex::new(map)), path)
    }
}
impl PeerStore for Peers {
    fn find_peer(&self, id: &[u8]) -> Option<[u8; 32]> {
        let r = self.0.lock().unwrap_or_else(|p| p.into_inner()).get(id).copied();
        lg!("    pair-verify peer lookup id='{}' -> {}", ascii(id, 64), if r.is_some() { "known" } else { "UNKNOWN" });
        r
    }
}
impl PeerSaver for Peers {
    fn save_peer(&mut self, id: &[u8], ltpk: [u8; 32]) {
        lg!("    pair-setup M5: controller id='{}' ltpk={} saved (RAM only)", ascii(id, 64), hex(&ltpk, 32));
        self.0.lock().unwrap_or_else(|p| p.into_inner()).insert(id.to_vec(), ltpk);
        if let Some(p) = &self.1 {
            let line = format!("{} {}\n", hex(id, id.len()), hex(&ltpk, 32));
            let _ = std::fs::OpenOptions::new().create(true).append(true).open(p).and_then(|mut f| f.write_all(line.as_bytes()));
        }
    }
}

// ---------------------------------------------------------------- decoders (log only)

fn tlv_name(t: u8) -> &'static str {
    match t {
        0x00 => "Method",
        0x01 => "Identifier",
        0x02 => "Salt",
        0x03 => "PublicKey",
        0x04 => "Proof",
        0x05 => "EncryptedData",
        0x06 => "State",
        0x07 => "Error",
        0x08 => "RetryDelay",
        0x09 => "Certificate",
        0x0A => "Signature",
        0x0B => "Permissions",
        0x13 => "Flags",
        0x19 => "AppFlags?",
        _ => "?",
    }
}

fn log_tlv(dir: &str, body: &[u8]) -> Vec<(u8, Vec<u8>)> {
    match tlv::decode(body) {
        Ok(items) => {
            for (t, v) in &items {
                lg!("    {dir} TLV 0x{t:02x} {:<13} len={:<4} {}", tlv_name(*t), v.len(), hex(v, 32));
            }
            items
        }
        Err(e) => {
            lg!("    {dir} TLV decode failed: {e}");
            Vec::new()
        }
    }
}

fn tlv_first(items: &[(u8, Vec<u8>)], t: u8) -> Option<&[u8]> {
    items.iter().find(|(k, _)| *k == t).map(|(_, v)| v.as_slice())
}

fn dump_plist(v: &plist::Value, indent: usize, out: &mut Vec<String>) {
    let pad = " ".repeat(indent);
    match v {
        plist::Value::Dictionary(d) => {
            for (k, val) in d.iter() {
                match val {
                    plist::Value::Dictionary(_) | plist::Value::Array(_) => {
                        out.push(format!("{pad}{k}:"));
                        if indent < 24 {
                            dump_plist(val, indent + 2, out);
                        }
                    }
                    // Key material: length and the first 4 bytes only, never the key.
                    plist::Value::Data(b) if is_secret_key(k) => out.push(format!(
                        "{pad}{k}: <data {} bytes: first4={} (rest withheld)>", b.len(), hex(&b[..b.len().min(4)], 4)
                    )),
                    _ => out.push(format!("{pad}{k}: {}", scalar(val))),
                }
            }
        }
        plist::Value::Array(a) => {
            for (i, val) in a.iter().enumerate() {
                match val {
                    plist::Value::Dictionary(_) | plist::Value::Array(_) => {
                        out.push(format!("{pad}[{i}]"));
                        if indent < 24 {
                            dump_plist(val, indent + 2, out);
                        }
                    }
                    _ => out.push(format!("{pad}[{i}] {}", scalar(val))),
                }
            }
        }
        other => out.push(format!("{pad}{}", scalar(other))),
    }
}

fn scalar(v: &plist::Value) -> String {
    match v {
        plist::Value::String(s) => format!("\"{s}\""),
        plist::Value::Integer(i) => match i.as_unsigned() {
            Some(u) => format!("{u} (0x{u:x})"),
            None => format!("{}", i.as_signed().unwrap_or(0)),
        },
        plist::Value::Boolean(b) => b.to_string(),
        plist::Value::Real(r) => r.to_string(),
        plist::Value::Data(d) => format!("<data {} bytes: {}>", d.len(), hex(d, 32)),
        other => format!("{other:?}"),
    }
}

fn parse_plist(body: &[u8]) -> Option<plist::Value> {
    if !body.starts_with(b"bplist00") {
        return None;
    }
    match plist::Value::from_reader(std::io::Cursor::new(body)) {
        Ok(v) => {
            let mut lines = Vec::new();
            dump_plist(&v, 6, &mut lines);
            lg!("    plist body:");
            for l in lines {
                lg!("{l}");
            }
            Some(v)
        }
        Err(e) => {
            lg!("    plist decode failed: {e}");
            None
        }
    }
}

fn to_bplist(d: plist::Dictionary) -> Vec<u8> {
    let mut out = Vec::new();
    plist::Value::Dictionary(d).to_writer_binary(&mut out).expect("bplist encode");
    out
}

// ---------------------------------------------------------------- session accounting

/// Byte/packet counters for one stream or port of a session.
#[derive(Default)]
struct StreamStat {
    label: String,
    bytes: AtomicU64,
    pkts: AtomicU64,
}

/// Wireless interfaces found at start (sysfs), for the Wi-Fi counters in the health line.
static WLAN_IFS: OnceLock<Vec<String>> = OnceLock::new();

/// (busy, softirq, total) jiffies of the whole box from `/proc/stat`, and this process's own.
fn cpu_ticks() -> Option<(u64, u64, u64, u64)> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let v: Vec<u64> = stat.lines().next()?.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
    if v.len() < 7 {
        return None;
    }
    let total: u64 = v.iter().take(8).sum();
    let me = std::fs::read_to_string("/proc/self/stat").ok()?;
    // utime and stime are fields 14 and 15; the command name (field 2) may contain spaces.
    let f: Vec<&str> = me.rsplit_once(')')?.1.split_whitespace().collect();
    let own = f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?;
    Some((total - v[3] - v[4], v[6], total, own))
}

/// One station block of `iw dev <if> station dump`, reduced to the counters that show link health.
fn wifi_stations(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in text.lines() {
        let t = l.trim();
        if let Some(rest) = t.strip_prefix("Station ") {
            out.push(format!("sta {}", rest.split_whitespace().next().unwrap_or("?")));
        } else if let (Some(cur), Some((k, v))) = (out.last_mut(), t.split_once(':')) {
            let short = match k.trim() {
                "signal" => "sig",
                "tx bitrate" => "txrate",
                "rx bitrate" => "rxrate",
                "tx retries" => "txretries",
                "tx failed" => "txfailed",
                "rx bytes" => "rxbytes",
                "tx bytes" => "txbytes",
                "rx drop misc" => "rxdrop",
                _ => continue,
            };
            let val: String = v.split_whitespace().take(2).collect::<Vec<_>>().join("");
            cur.push_str(&format!(" {short}={val}"));
        }
    }
    out
}

/// Box health for the 10-second timeline: load, CPU share since the last call, memory, Wi-Fi.
fn health_line(prev: &mut Option<(u64, u64, u64, u64)>) -> String {
    let load = std::fs::read_to_string("/proc/loadavg").map(|l| l.split_whitespace().take(3).collect::<Vec<_>>().join("/")).unwrap_or_else(|_| "?".into());
    let now = cpu_ticks();
    let cpu = match (*prev, now) {
        (Some(a), Some(b)) if b.2 > a.2 => {
            let dt = (b.2 - a.2) as f64;
            format!(
                "cpu busy={:.0}% softirq={:.0}% probe={:.0}%",
                (b.0 - a.0) as f64 * 100.0 / dt, (b.1 - a.1) as f64 * 100.0 / dt, (b.3 - a.3) as f64 * 100.0 / dt
            )
        }
        _ => "cpu n/a".into(),
    };
    *prev = now;
    let mut wifi = Vec::new();
    for ifn in WLAN_IFS.get().map(Vec::as_slice).unwrap_or(&[]) {
        if let Ok(o) = std::process::Command::new("iw").args(["dev", ifn, "station", "dump"]).output() {
            wifi.extend(wifi_stations(&String::from_utf8_lossy(&o.stdout)));
        }
    }
    format!("load={load} {cpu} mem_available={}kB tmp_free={}kB | {}", mem_available_kb(), cap_fs_free_kb(), if wifi.is_empty() { "no station".to_string() } else { wifi.join(" ; ") })
}

/// PTP message counts by type (index = low nibble of byte 0), for udp 319 and 320.
static PTP_SEEN: [[AtomicU64; 16]; 2] = [const { [const { AtomicU64::new(0) }; 16] }; 2];

fn ptp_counts() -> String {
    const NAMES: [&str; 16] = [
        "Sync", "Delay_Req", "Pdelay_Req", "Pdelay_Resp", "4", "5", "6", "7", "Follow_Up", "Delay_Resp",
        "Pdelay_Resp_FU", "Announce", "Signaling", "Management", "14", "15",
    ];
    let mut out = String::new();
    for (i, port) in [319, 320].iter().enumerate() {
        let parts: Vec<String> = (0..16)
            .filter_map(|t| {
                let n = PTP_SEEN[i][t].load(Ordering::Relaxed);
                (n > 0).then(|| format!("{}={n}", NAMES[t]))
            })
            .collect();
        out.push_str(&format!(" ptp{port}[{}]", parts.join(" ")));
    }
    out
}

/// One per control connection: what the sender asked for and what arrived, for the 10-second
/// timeline and the end-of-session summary.
struct Session {
    id: String,
    peer_ip: String,
    started: Instant,
    alive: AtomicBool,
    ticking: AtomicBool,
    teardown: AtomicBool,
    feedback: AtomicU64,
    streams: Mutex<Vec<Arc<StreamStat>>>,
    /// ("METHOD path", count) in first-seen order.
    requests: Mutex<Vec<(String, u64)>>,
    /// Every stream the sender requested, in order, with its key list.
    stream_reqs: Mutex<Vec<String>>,
    /// (type, streamConnectionID) of every stream requested, for a capture's .meta.
    scids: Mutex<Vec<(u64, u64)>>,
}

impl Session {
    fn new(id: String, peer_ip: String) -> Arc<Self> {
        Arc::new(Session {
            id,
            peer_ip,
            started: Instant::now(),
            alive: AtomicBool::new(true),
            ticking: AtomicBool::new(false),
            teardown: AtomicBool::new(false),
            feedback: AtomicU64::new(0),
            streams: Mutex::new(Vec::new()),
            requests: Mutex::new(Vec::new()),
            stream_reqs: Mutex::new(Vec::new()),
            scids: Mutex::new(Vec::new()),
        })
    }
    fn stream(&self, label: &str) -> Arc<StreamStat> {
        let s = Arc::new(StreamStat { label: label.to_string(), ..StreamStat::default() });
        self.streams.lock().unwrap_or_else(|p| p.into_inner()).push(s.clone());
        s
    }
    fn note_request(&self, method: &str, path: &str) {
        let key = format!("{method} {}", ascii(path.as_bytes(), 60));
        let mut r = self.requests.lock().unwrap_or_else(|p| p.into_inner());
        match r.iter_mut().find(|(k, _)| *k == key) {
            Some(e) => e.1 += 1,
            None => r.push((key, 1)),
        }
    }
    fn totals(&self) -> String {
        let s = self.streams.lock().unwrap_or_else(|p| p.into_inner());
        if s.is_empty() {
            return "no streams".into();
        }
        s.iter()
            .map(|x| format!("{}: {} B / {} pkts", x.label, x.bytes.load(Ordering::Relaxed), x.pkts.load(Ordering::Relaxed)))
            .collect::<Vec<_>>()
            .join(" | ")
    }
    /// One line every 10 s from the first SETUP until the control connection ends.
    fn start_ticker(self: &Arc<Self>) {
        if self.ticking.swap(true, Ordering::Relaxed) {
            return;
        }
        let me = self.clone();
        std::thread::spawn(move || {
            let mut next = 10u64;
            let mut cpu = cpu_ticks();
            while me.alive.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(500));
                if me.started.elapsed().as_secs() >= next && me.alive.load(Ordering::Relaxed) {
                    lg!(
                        "TIMELINE #{} +{next}s feedback={} | {} |{} census_pkts={}",
                        me.id, me.feedback.load(Ordering::Relaxed), me.totals(), ptp_counts(), NET_PKTS.load(Ordering::Relaxed)
                    );
                    lg!("HEALTH #{} +{next}s {}", me.id, health_line(&mut cpu));
                    next += 10;
                }
            }
        });
    }
}

// ---------------------------------------------------------------- catch-all port census

/// (protocol, source address, destination port) -> (packets, TCP SYNs) for every IPv4 packet that
/// arrived for us on a wireless interface, INCLUDING ports nothing listens on.
static FLOWS: OnceLock<Mutex<HashMap<(u8, [u8; 4], u16), (u64, u64)>>> = OnceLock::new();
/// Every packet the census has examined: shown in the timeline so a silent census is visible.
static NET_PKTS: AtomicU64 = AtomicU64::new(0);

fn ports_touched(peer_ip: &str) -> String {
    let Some(m) = FLOWS.get() else { return "census not running (no wireless interface, or not Linux)".into() };
    let m = m.lock().unwrap_or_else(|p| p.into_inner());
    let mut v: Vec<String> = m
        .iter()
        .filter(|((_, src, _), _)| std::net::Ipv4Addr::from(*src).to_string() == peer_ip)
        .map(|((proto, _, port), (pkts, syns))| {
            if *proto == 6 {
                format!("tcp/{port} pkts={pkts} syn={syns}")
            } else {
                format!("udp/{port} pkts={pkts}")
            }
        })
        .collect();
    v.sort();
    if v.is_empty() { "none seen on the wireless interface".into() } else { v.join("; ") }
}

/// Record one IPv4 packet (header bytes only are needed). Returns a line to log when the flow is
/// new or the packet opens a TCP connection.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn census_packet(ip: &[u8], listening: impl Fn(u8, u16) -> bool) -> Option<String> {
    if ip.len() < 20 || ip[0] >> 4 != 4 {
        return None;
    }
    let (ihl, proto) = ((ip[0] & 0x0f) as usize * 4, ip[9]);
    if !(proto == 6 || proto == 17) || u16::from_be_bytes([ip[6], ip[7]]) & 0x1fff != 0 || ip.len() < ihl + 4 {
        return None;
    }
    let src = [ip[12], ip[13], ip[14], ip[15]];
    let sport = u16::from_be_bytes([ip[ihl], ip[ihl + 1]]);
    let dport = u16::from_be_bytes([ip[ihl + 2], ip[ihl + 3]]);
    let syn = proto == 6 && ip.len() > ihl + 13 && ip[ihl + 13] & 0x12 == 0x02;
    let mut m = FLOWS.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner());
    let e = m.entry((proto, src, dport)).or_insert((0, 0));
    e.0 += 1;
    if syn {
        e.1 += 1;
    }
    (e.0 == 1 || syn).then(|| {
        format!(
            "NET {} {}:{sport} -> :{dport}{}{}",
            if proto == 6 { "tcp" } else { "udp" },
            std::net::Ipv4Addr::from(src),
            if syn { " SYN" } else { " (first packet)" },
            if listening(proto, dport) { "" } else { "  <-- NOTHING LISTENS on this port" }
        )
    })
}

/// Is a local socket bound to this port right now? (`/proc/net/tcp`/`udp`: local address column.)
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn port_bound(proto: u8, port: u16) -> bool {
    let path = if proto == 6 { "/proc/net/tcp" } else { "/proc/net/udp" };
    let needle = format!(":{port:04X} ");
    std::fs::read_to_string(path)
        .map(|t| t.lines().skip(1).any(|l| l.split_whitespace().nth(1).is_some_and(|a| format!("{a} ").ends_with(&needle))))
        .unwrap_or(true)
}

/// Sniff inbound IPv4 on every wireless interface with a packet socket (no packet filter, no extra
/// package). The interface is enumerated from sysfs, never assumed.
#[cfg(target_os = "linux")]
fn net_census() {
    let ifaces: Vec<String> = std::fs::read_dir("/sys/class/net")
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter(|e| e.path().join("phy80211").exists() || e.path().join("wireless").exists())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    if ifaces.is_empty() {
        lg!("NET census: no wireless interface found; inbound ports will not be recorded");
    }
    let _ = WLAN_IFS.set(ifaces.clone());
    for name in ifaces {
        let Ok(cname) = std::ffi::CString::new(name.clone()) else { continue };
        let idx = unsafe { libc::if_nametoindex(cname.as_ptr()) };
        let proto_be = (libc::ETH_P_IP as u16).to_be();
        let fd = unsafe { libc::socket(libc::AF_PACKET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, i32::from(proto_be)) };
        if fd < 0 || idx == 0 {
            lg!("NET census: cannot open a packet socket on {name}: {}", std::io::Error::last_os_error());
            continue;
        }
        let mut sll: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
        sll.sll_family = libc::AF_PACKET as u16;
        sll.sll_protocol = proto_be;
        sll.sll_ifindex = idx as i32;
        let rc = unsafe {
            libc::bind(fd, std::ptr::addr_of!(sll).cast(), std::mem::size_of::<libc::sockaddr_ll>() as u32)
        };
        if rc != 0 {
            lg!("NET census: bind to {name} failed: {}", std::io::Error::last_os_error());
            unsafe { libc::close(fd) };
            continue;
        }
        lg!("NET census: recording every inbound IPv4 tcp/udp destination port on {name} (headers only)");
        FLOWS.get_or_init(Default::default);
        std::thread::spawn(move || {
            let mut buf = [0u8; 96];
            loop {
                let mut from: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
                let mut flen = std::mem::size_of::<libc::sockaddr_ll>() as u32;
                // MSG_TRUNC: only the first 96 bytes are copied, whatever the packet size.
                let n = unsafe {
                    libc::recvfrom(
                        fd, buf.as_mut_ptr().cast(), buf.len(), libc::MSG_TRUNC,
                        std::ptr::addr_of_mut!(from).cast(), &mut flen,
                    )
                };
                if n <= 0 {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                // PACKET_HOST only: addressed to this box (not our own output, not broadcast).
                if from.sll_pkttype != 0 {
                    continue;
                }
                NET_PKTS.fetch_add(1, Ordering::Relaxed);
                if let Some(line) = census_packet(&buf[..(n as usize).min(buf.len())], port_bound) {
                    lg!("{line}");
                }
            }
        });
    }
}
#[cfg(not(target_os = "linux"))]
fn net_census() {}

// ---------------------------------------------------------------- sinks (log-only data ports)

static SINKS: AtomicU32 = AtomicU32::new(0);
const MAX_SINKS: u32 = 96;

/// Where stream captures go: `<log>.cap/` beside the log, i.e. RAM. Unset in tests (no files).
static CAP_DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
/// Per-stream capture ceiling. Each (variant, stream type) has ONE capture file that the next
/// stream of that kind overwrites.
static CAP_MAX_BYTES: AtomicU64 = AtomicU64::new(12 * 1024 * 1024);
fn cap_max() -> usize {
    CAP_MAX_BYTES.load(Ordering::Relaxed) as usize
}

/// Free space, in kB, of the filesystem holding the captures (tmpfs has its own ceiling, separate
/// from available memory). `u64::MAX` when it cannot be read.
fn cap_fs_free_kb() -> u64 {
    let Some(dir) = CAP_DIR.get().and_then(|d| d.parent()).and_then(|p| std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()) else {
        return u64::MAX;
    };
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(dir.as_ptr(), &mut st) } != 0 {
        return u64::MAX;
    }
    (st.f_bavail as u64).saturating_mul(st.f_frsize as u64) / 1024
}
/// Capturing also stops when the capture filesystem has less than this left.
const MIN_FS_FREE_KB: u64 = 6 * 1024;
/// Capturing stops while less than this much memory is available: the captures live in tmpfs.
const MIN_FREE_KB: u64 = 15 * 1024;

fn mem_available_kb() -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| l.strip_prefix("MemAvailable:")?.trim().trim_end_matches("kB").trim().parse().ok())
        })
        .unwrap_or(u64::MAX)
}

/// One capture file plus its `.meta`, bounded by the capture ceiling and by available memory.
struct CapFile {
    file: Option<File>,
    written: usize,
    since_check: usize,
    name: String,
}

impl CapFile {
    fn open(name: &str, meta: &str) -> CapFile {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let mut c = CapFile { file: None, written: 0, since_check: 0, name: name.to_string() };
        let Some(dir) = CAP_DIR.get() else { return c };
        if mem_available_kb() < MIN_FREE_KB + (cap_max() as u64 / 1024) / 4 || cap_fs_free_kb() < MIN_FS_FREE_KB + 1024 {
            lg!("CAPTURE {name}: NOT started, {} kB memory available, {} kB free in the capture filesystem", mem_available_kb(), cap_fs_free_kb());
            return c;
        }
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        let mut o = std::fs::OpenOptions::new();
        o.create(true).write(true).truncate(true).mode(0o600);
        let _ = o.open(dir.join(format!("{name}.meta"))).and_then(|mut f| f.write_all(meta.as_bytes()));
        c.file = o.open(dir.join(format!("{name}.bin"))).ok();
        c
    }
    fn write(&mut self, data: &[u8]) {
        let Some(f) = self.file.as_mut() else { return };
        let take = data.len().min(cap_max().saturating_sub(self.written));
        self.since_check += take;
        let low = self.since_check >= 256 * 1024 && {
            self.since_check = 0;
            mem_available_kb() < MIN_FREE_KB || cap_fs_free_kb() < MIN_FS_FREE_KB
        };
        if take > 0 && !low && f.write_all(&data[..take]).is_ok() {
            self.written += take;
        }
        if low || self.written >= cap_max() || take < data.len() {
            lg!(
                "CAPTURE {}: stopped at {} B ({}); the stream is still counted, no longer saved",
                self.name, self.written, if low { "memory or capture-filesystem floor reached" } else { "capture cap reached" }
            );
            self.file = None;
        }
    }
}

/// Streaming framer for the two TCP media layouts. Holds at most one frame head (or, for the first
/// few frames, one whole frame) so a multi-megabyte stream costs no memory.
///   103: `[u16 BE len][RTP 12][ciphertext][tag 16][nonce 8]` (len counts itself)
///   110: `[128-byte header: bodySize u32 LE @0, opcode u8 @4, flags @5, time 32.32 LE @8][body]`
struct Framer {
    ty: u64,
    head: Vec<u8>,
    want: usize,
    skip: usize,
    body: usize,
    frames: u64,
    dead: bool,
    /// A frame whose head is parsed but whose body is still arriving: counted when it completes.
    pending: bool,
    cur_op: usize,
    op_logged: [u8; 256],
    ops: [u64; 256],
}

impl Framer {
    fn new(ty: u64) -> Self {
        let want = if ty == 110 { 128 } else { 2 };
        Framer { ty, head: Vec::new(), want, skip: 0, body: 0, frames: 0, dead: ty != 110 && ty != 103, pending: false, cur_op: 0, op_logged: [0; 256], ops: [0; 256] }
    }

    fn feed(&mut self, label: &str, mut data: &[u8], stat: &StreamStat) {
        while !data.is_empty() && !self.dead {
            if self.skip > 0 {
                let n = self.skip.min(data.len());
                self.skip -= n;
                data = &data[n..];
                if self.skip == 0 && self.pending {
                    self.complete(stat);
                }
                continue;
            }
            let n = (self.want - self.head.len()).min(data.len());
            self.head.extend_from_slice(&data[..n]);
            data = &data[n..];
            if self.head.len() < self.want {
                return;
            }
            if self.ty == 103 {
                self.step_audio(label, stat);
            } else {
                self.step_screen(label, stat);
            }
        }
    }

    fn complete(&mut self, stat: &StreamStat) {
        self.pending = false;
        self.frames += 1;
        self.ops[self.cur_op] += 1;
        stat.pkts.fetch_add(1, Ordering::Relaxed);
    }

    fn step_audio(&mut self, label: &str, stat: &StreamStat) {
        let len = u16::from_be_bytes([self.head[0], self.head[1]]) as usize;
        if self.head.len() == 2 {
            if len < 2 + 12 + 16 + 8 {
                lg!("SINK [{label}] framing: length prefix {len} is not a buffered-audio packet; parsing stops");
                self.dead = true;
            } else if self.frames < 10 {
                self.want = len; // buffer the whole packet to log its shape
                return;
            } else {
                self.skip = len - 2;
                self.pending = true;
            }
        } else {
            let pkt = &self.head[2..];
            lg!(
                "SINK [{label}] pkt#{} len-prefix={len} rtp12={} ciphertext+tag={} B nonce8={}",
                self.frames + 1, hex(&pkt[..12], 12), pkt.len() - 12 - 8, hex(&pkt[pkt.len() - 8..], 8)
            );
            self.complete(stat);
        }
        self.head.clear();
        self.want = 2;
    }

    fn step_screen(&mut self, label: &str, stat: &StreamStat) {
        let op = self.head[4] as usize;
        if self.head.len() == 128 {
            self.body = u32::from_le_bytes([self.head[0], self.head[1], self.head[2], self.head[3]]) as usize;
            if self.body > 16 * 1024 * 1024 {
                lg!("SINK [{label}] framing: body size {} is implausible; parsing stops", self.body);
                self.dead = true;
                return;
            }
            // A non-video frame (codec config, report, ...) is not encrypted media: keep up to 64
            // bytes of its body for the log, for the first three of each opcode.
            if op != 0 && self.op_logged[op] < 3 && self.body > 0 {
                self.want = 128 + self.body.min(64);
                return;
            }
        }
        self.cur_op = op;
        let extra = self.head.len() - 128;
        if self.frames < 10 || extra > 0 {
            let secs = u64::from_le_bytes(self.head[8..16].try_into().unwrap()) as f64 / 4_294_967_296.0;
            lg!(
                "SINK [{label}] frame#{} body={} B opcode={op} flags=0x{:02x} time={secs:.3}s hdr[0..64]={}",
                self.frames + 1, self.body, self.head[5], hex(&self.head[..64], 64)
            );
        }
        if extra > 0 {
            self.op_logged[op] += 1;
            lg!("SINK [{label}] frame#{} opcode {op} body[0..{extra}]={}", self.frames + 1, hex(&self.head[128..], 64));
        }
        self.skip = self.body - extra;
        if self.skip > 0 {
            self.pending = true;
        } else {
            self.complete(stat);
        }
        self.head.clear();
        self.want = 128;
    }

    fn opcode_summary(&self) -> String {
        (0..256).filter(|o| self.ops[*o] > 0).map(|o| format!("op{o}×{}", self.ops[o])).collect::<Vec<_>>().join(" ")
    }
}

/// A TCP data port for a media stream: logs the first reads and the framing, keeps up to
/// the capture ceiling bytes in `<cap dir>/<name>.bin` with the facts an OFF-box decrypt test needs in
/// `<name>.meta`, and otherwise discards. No read timeout: it reads until the peer closes.
fn capture_sink(label: String, ty: u64, name: String, meta: String, stat: Arc<StreamStat>) -> u16 {
    if SINKS.fetch_add(1, Ordering::Relaxed) >= MAX_SINKS {
        return 0;
    }
    let Ok(l) = TcpListener::bind("0.0.0.0:0") else { return 0 };
    let port = l.local_addr().map(|a| a.port()).unwrap_or(0);
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let (label, name, meta, stat) = (label.clone(), name.clone(), meta.clone(), stat.clone());
            std::thread::spawn(move || {
                let mut s = s;
                let peer = s.peer_addr().map(|a| a.to_string()).unwrap_or_default();
                lg!("SINK tcp:{port} [{label}] connection from {peer}");
                let mut cap = CapFile::open(&name, &meta);
                let mut framer = Framer::new(ty);
                let mut buf = vec![0u8; 32 * 1024];
                let (mut total, mut reads) = (0usize, 0u32);
                let why = loop {
                    match s.read(&mut buf) {
                        Ok(0) => break "peer closed".to_string(),
                        Err(e) => break format!("read error: {e}"),
                        Ok(n) => {
                            reads += 1;
                            if reads <= 4 {
                                lg!("SINK tcp:{port} [{label}] read#{reads} {n} B: {}", hex(&buf[..n], 64));
                            }
                            total += n;
                            stat.bytes.fetch_add(n as u64, Ordering::Relaxed);
                            cap.write(&buf[..n]);
                            framer.feed(&label, &buf[..n], &stat);
                        }
                    }
                };
                lg!(
                    "SINK tcp:{port} [{label}] {peer} ended ({why}) after {total} B, {} whole frames {}; saved {} B as {name}.bin",
                    framer.frames, framer.opcode_summary(), cap.written
                );
            });
        }
    });
    port
}

/// PTP is FOLLOW-ONLY for a receiver (the sender is the clock master; the reference follower,
/// nqptp, only listens on 319/320 and transmits nothing in normal operation). So: listen, count,
/// log the first of each message type, answer nothing.
fn ptp_listen(port: u16) {
    let s = match UdpSocket::bind(("0.0.0.0", port)) {
        Ok(s) => s,
        Err(e) => {
            lg!("PTP udp:{port} not bound ({e}); PTP traffic to it will not be logged");
            return;
        }
    };
    lg!("PTP udp:{port} listening (log only, nothing is ever sent)");
    std::thread::spawn(move || {
        let mut buf = [0u8; 1500];
        let mut seen = [0u64; 16];
        while let Ok((n, from)) = s.recv_from(&mut buf) {
            if n == 0 {
                continue;
            }
            let ty = (buf[0] & 0x0f) as usize;
            seen[ty] += 1;
            PTP_SEEN[usize::from(port == 320)][ty].fetch_add(1, Ordering::Relaxed);
            if seen[ty] <= 3 {
                lg!(
                    "PTP udp:{port} type 0x{ty:x} #{} {n} B from {from} ver={} seq={} : {}",
                    seen[ty], buf.get(1).map(|v| v & 0x0f).unwrap_or(0),
                    buf.get(30..32).map(|b| u16::from_be_bytes([b[0], b[1]])).unwrap_or(0), hex(&buf[..n], 64)
                );
            }
        }
    });
}

/// Log one decrypted event-channel message: its head lines and, when the body is a plist, the plist.
fn log_event_message(label: &str, msg: &[u8]) {
    let head_end = msg.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4).unwrap_or(msg.len());
    for l in String::from_utf8_lossy(&msg[..head_end]).lines().filter(|l| !l.is_empty()).take(12) {
        lg!("EVENT [{label}] < {}", ascii(l.as_bytes(), 200));
    }
    let body = &msg[head_end..];
    if !body.is_empty() && parse_plist(body).is_none() {
        lg!("EVENT [{label}] < body {} B: {}", body.len(), hex(body, 64));
    }
}

/// A TCP port that accepts and logs what it is sent, never answers, and has NO read timeout: it
/// holds the connection until the PEER closes it (the event channel must stay open for the whole
/// session). With `keys` (the event channel) inbound bytes are also decrypted with the existing
/// `rtsp::control` codec and logged decoded.
fn tcp_sink(label: String, stat: Arc<StreamStat>, keys: Option<ControlKeys>) -> u16 {
    if SINKS.fetch_add(1, Ordering::Relaxed) >= MAX_SINKS {
        return 0;
    }
    let Ok(l) = TcpListener::bind("0.0.0.0:0") else { return 0 };
    let port = l.local_addr().map(|a| a.port()).unwrap_or(0);
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let (label, stat, keys) = (label.clone(), stat.clone(), keys.clone());
            std::thread::spawn(move || {
                let mut s = s;
                let peer = s.peer_addr().map(|a| a.to_string()).unwrap_or_default();
                lg!("SINK tcp:{port} [{label}] connection from {peer} (held open until the peer closes)");
                // Either direction's key may be the inbound one; the first frame decides.
                let mut chans: Vec<ControlChannel> = keys
                    .map(|k| vec![
                        ControlChannel::new(k.clone()),
                        ControlChannel::new(ControlKeys { read: k.write, write: k.read }),
                    ])
                    .unwrap_or_default();
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                let (mut total, mut reads) = (0usize, 0u32);
                let why = loop {
                    match s.read(&mut buf) {
                        Ok(0) => break "peer closed".to_string(),
                        Err(e) => break format!("read error: {e}"),
                        Ok(n) => {
                            reads += 1;
                            total += n;
                            stat.bytes.fetch_add(n as u64, Ordering::Relaxed);
                            stat.pkts.fetch_add(1, Ordering::Relaxed);
                            if reads <= 8 {
                                lg!("SINK tcp:{port} [{label}] read#{reads} {n} B: {}", hex(&buf[..n], 64));
                            }
                            if chans.is_empty() || raw.len() > 256 * 1024 {
                                continue;
                            }
                            raw.extend_from_slice(&buf[..n]);
                            if chans.len() == 2 {
                                // decrypt_frame only advances a channel's counter on success.
                                let mut hit = None;
                                for (i, ch) in chans.iter_mut().enumerate() {
                                    if let Ok(Some((pt, used))) = ch.decrypt_frame(&raw) {
                                        hit = Some((i, pt, used));
                                        break;
                                    }
                                }
                                match hit {
                                    Some((i, pt, used)) => {
                                        raw.drain(..used);
                                        lg!("EVENT [{label}] inbound decrypts with the events {} key", if i == 0 { "read" } else { "write" });
                                        log_event_message(&label, &pt);
                                        let keep = chans.swap_remove(i);
                                        chans = vec![keep];
                                    }
                                    None if raw.len() >= 2 + usize::from(u16::from_le_bytes([raw[0], raw[1]])) + 16 => {
                                        lg!("EVENT [{label}] inbound does NOT decrypt with either events key; raw only from here");
                                        chans.clear();
                                        continue;
                                    }
                                    None => continue,
                                }
                            }
                            while let Ok(Some((pt, used))) = chans[0].decrypt_frame(&raw) {
                                raw.drain(..used);
                                log_event_message(&label, &pt);
                            }
                        }
                    }
                };
                lg!("SINK tcp:{port} [{label}] {peer} ended ({why}) after {total} B in {reads} reads");
            });
        }
    });
    port
}

/// A UDP port that counts and logs the first datagrams, never answers. With `capture`, datagrams
/// are saved as `[u16 BE len][datagram]` (len counts itself) — the same framing as type 103, so
/// one checker reads both.
fn udp_sink(label: String, stat: Arc<StreamStat>, capture: Option<(String, String)>) -> u16 {
    if SINKS.fetch_add(1, Ordering::Relaxed) >= MAX_SINKS {
        return 0;
    }
    let Ok(s) = UdpSocket::bind("0.0.0.0:0") else { return 0 };
    let port = s.local_addr().map(|a| a.port()).unwrap_or(0);
    std::thread::spawn(move || {
        let mut buf = [0u8; 2048];
        let mut cap: Option<CapFile> = None;
        let mut n_pkts = 0u64;
        while let Ok((n, from)) = s.recv_from(&mut buf) {
            n_pkts += 1;
            stat.bytes.fetch_add(n as u64, Ordering::Relaxed);
            stat.pkts.fetch_add(1, Ordering::Relaxed);
            if n_pkts <= 4 {
                lg!("SINK udp:{port} [{label}] pkt#{n_pkts} {n} B from {from}: {}", hex(&buf[..n], 64));
            }
            if let Some((name, meta)) = &capture {
                let c = cap.get_or_insert_with(|| CapFile::open(name, meta));
                if c.file.is_some() {
                    let mut rec = ((n + 2) as u16).to_be_bytes().to_vec();
                    rec.extend_from_slice(&buf[..n]);
                    c.write(&rec);
                }
            }
        }
    });
    port
}

// ---------------------------------------------------------------- request bodies (log only)

/// Data under these plist keys is key material: length and first 4 bytes only.
fn is_secret_key(k: &str) -> bool {
    let l = k.to_ascii_lowercase();
    l.contains("key") || l.contains("secret") || l.contains("password") || l == "shk" || l == "eiv"
}

/// DAAP/DMAP tagged data (`application/x-dmap-tagged`): `[4-char tag][u32 BE len][value]`, nested.
fn dump_dmap(mut b: &[u8], depth: usize, out: &mut Vec<String>) {
    const CONTAINERS: [&[u8; 4]; 8] = [b"mlit", b"cmst", b"mlcl", b"mcon", b"msrv", b"adbs", b"apso", b"mdcl"];
    while b.len() >= 8 && out.len() < 64 {
        let (tag, len) = (&b[..4], u32::from_be_bytes([b[4], b[5], b[6], b[7]]) as usize);
        if b.len() < 8 + len {
            out.push(format!("{}(truncated tag {})", " ".repeat(depth * 2), ascii(tag, 4)));
            return;
        }
        let v = &b[8..8 + len];
        let name = match tag {
            b"minm" => " title/name",
            b"asar" => " artist",
            b"asal" => " album",
            b"asgn" => " genre",
            b"ascp" => " composer",
            b"astm" => " duration ms",
            b"astn" => " track number",
            b"asdk" => " data kind",
            b"caps" => " play status",
            b"mper" => " persistent id",
            b"mikd" => " item kind",
            _ => "",
        };
        let pad = " ".repeat(depth * 2);
        if depth < 4 && CONTAINERS.iter().any(|c| &c[..] == tag) {
            out.push(format!("{pad}{}{name} ({len} B):", ascii(tag, 4)));
            dump_dmap(v, depth + 1, out);
        } else if matches!(tag, b"minm" | b"asar" | b"asal" | b"asgn" | b"ascp" | b"asaa")
            || (!matches!(v.len(), 1 | 2 | 4 | 8) && std::str::from_utf8(v).is_ok())
        {
            out.push(format!("{pad}{}{name}: \"{}\"", ascii(tag, 4), String::from_utf8_lossy(&v[..v.len().min(160)])));
        } else if v.len() > 8 {
            out.push(format!("{pad}{}{name}: {} B {}", ascii(tag, 4), v.len(), hex(v, 16)));
        } else {
            let n = v.iter().fold(0u64, |a, x| a << 8 | u64::from(*x));
            out.push(format!("{pad}{}{name}: {n} ({} B)", ascii(tag, 4), v.len()));
        }
        b = &b[8 + len..];
    }
}

static ARTWORK_SAVED: AtomicBool = AtomicBool::new(false);

/// Log a request body according to its content type. Binary plists are decoded by the caller
/// (`parse_plist`); their raw hex is not logged at all, because a plist can carry key material.
fn log_body(id: &str, req: &Request) {
    let b = &req.body;
    if b.is_empty() {
        return;
    }
    let ct = req.header("Content-Type").unwrap_or("").to_ascii_lowercase();
    if b.starts_with(b"bplist00") {
        lg!("    B binary plist {} B (decoded below; raw hex withheld)", b.len());
    } else if ct.starts_with("text/") || ct.contains("sdp") {
        for l in String::from_utf8_lossy(&b[..b.len().min(2048)]).lines() {
            lg!("    B text  {}", ascii(l.as_bytes(), 240));
        }
    } else if ct.starts_with("image/") {
        lg!("    B artwork {ct} {} B, magic {}", b.len(), hex(b, 8));
        if b.len() <= 2 * 1024 * 1024 && mem_available_kb() > MIN_FREE_KB && !ARTWORK_SAVED.swap(true, Ordering::Relaxed) {
            if let Some(dir) = CAP_DIR.get() {
                let _ = std::fs::create_dir_all(dir);
                let p = dir.join(format!("artwork-first.{}", if ct.contains("png") { "png" } else { "jpg" }));
                match std::fs::write(&p, b) {
                    Ok(()) => lg!("#{id} first artwork saved to {} (RAM)", p.display()),
                    Err(e) => lg!("#{id} artwork not saved: {e}"),
                }
            }
        }
    } else if ct.contains("dmap") {
        let mut lines = Vec::new();
        dump_dmap(b, 0, &mut lines);
        lg!("    B DMAP {} B:", b.len());
        for l in lines {
            lg!("      {l}");
        }
    } else {
        lg!("    B[0..64] {}", hex(b, 64));
        lg!("    B ascii  {}", ascii(b, 64));
    }
}

// ---------------------------------------------------------------- /info

/// A SPEC value as a plist value: all digits -> integer, `true`/`false` -> boolean, else string.
fn typed(v: &str) -> plist::Value {
    match v {
        "true" => plist::Value::Boolean(true),
        "false" => plist::Value::Boolean(false),
        n if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => plist::Value::Integer(n.parse::<u64>().unwrap_or(0).into()),
        other => plist::Value::String(other.to_string()),
    }
}

fn build_info(c: &Cfg, with_txt: bool) -> Vec<u8> {
    use plist::Value as V;
    let mut d = plist::Dictionary::new();
    d.insert("deviceID".into(), V::String(c.device_id.clone()));
    d.insert("macAddress".into(), V::String(c.device_id.clone()));
    d.insert("features".into(), V::Integer(c.features.into()));
    d.insert("statusFlags".into(), V::Integer(u64::from(c.flags).into()));
    d.insert("model".into(), V::String(c.model.clone()));
    d.insert("manufacturer".into(), V::String("OCBM".into()));
    d.insert("name".into(), V::String(c.name.clone()));
    d.insert("pi".into(), V::String(c.pi.clone()));
    d.insert("pk".into(), V::Data(c.pk.to_vec()));
    d.insert("psi".into(), V::String(c.pi.clone()));
    d.insert("sourceVersion".into(), V::String(c.srcvers.clone()));
    d.insert("protocolVersion".into(), V::String(c.protovers.clone()));
    d.insert("keepAliveLowPower".into(), V::Boolean(true));
    d.insert("keepAliveSendStatsAsBody".into(), V::Boolean(true));
    if c.ap2_txt {
        d.insert("vv".into(), V::Integer(2u64.into()));
    }
    if c.bit(9) {
        let mut l = plist::Dictionary::new();
        l.insert("audioType".into(), V::String("default".into()));
        l.insert("inputLatencyMicros".into(), V::Integer(0u64.into()));
        l.insert("outputLatencyMicros".into(), V::Integer(400_000u64.into()));
        d.insert("audioLatencies".into(), V::Array(vec![V::Dictionary(l)]));
    }
    if c.bit(7) {
        let mut s = plist::Dictionary::new();
        s.insert("uuid".into(), V::String(c.pi.clone()));
        // The sender reads widthPixels/heightPixels and maxFPS from this dictionary (its
        // APEndpointDisplayDescriptionCreateWithDisplayInfo); the macOS receiver also emits the
        // legacy width/height. Sizes and rate are per variant (`display=WxH@R`).
        let (w, h, rate) = c.display.unwrap_or((1920, 1080, 30));
        for k in ["widthPixels", "width"] {
            s.insert(k.into(), V::Integer(u64::from(w).into()));
        }
        for k in ["heightPixels", "height"] {
            s.insert(k.into(), V::Integer(u64::from(h).into()));
        }
        s.insert("widthPhysical".into(), V::Integer(0u64.into()));
        s.insert("heightPhysical".into(), V::Integer(0u64.into()));
        s.insert("features".into(), V::Integer(14u64.into()));
        s.insert("maxFPS".into(), V::Integer(u64::from(c.maxfps.unwrap_or(rate)).into()));
        // refreshRate is a legacy key (a period in seconds) the current sender does not read; the
        // default keeps its historical 1/60, a `display=` variant states 1/R for consistency.
        s.insert("refreshRate".into(), V::Real(c.display.map(|d| 1.0 / f64::from(d.2)).unwrap_or(1.0 / 60.0)));
        for (k, v) in &c.extra_disp {
            s.insert(k.clone(), typed(v));
        }
        s.insert("overscanned".into(), V::Boolean(false));
        s.insert("rotation".into(), V::Boolean(false));
        d.insert("displays".into(), V::Array(vec![V::Dictionary(s)]));
    }
    for (k, v) in &c.extra_info {
        d.insert(k.clone(), typed(v));
    }
    if with_txt {
        let mut txt = Vec::new();
        for (k, v) in c.airplay_txt() {
            let e = format!("{k}={v}");
            txt.push(e.len().min(255) as u8);
            txt.extend_from_slice(&e.as_bytes()[..e.len().min(255)]);
        }
        d.insert("txtAirPlay".into(), V::Data(txt));
    }
    to_bplist(d)
}

// ---------------------------------------------------------------- one connection

const OCTET: &str = "application/octet-stream";
const BPLIST: &str = "application/x-apple-binary-plist";

struct Conn<'a> {
    id: String,
    cfg: &'a Cfg,
    peers: Peers,
    setup: Option<PairSetupServer<'static>>,
    /// Transient pair-setup SRP state (the crate's own SrpServer, driven directly).
    transient: Option<SrpServer>,
    verify: Option<PairVerifyServer<'static>>,
    /// MFi-SAP (`/auth-setup`) exchange; one per connection, holds the AES-CTR context after M2.
    sap: MfiSapServer,
    /// Keys to switch to AFTER the current response has gone out in the clear.
    pending: Option<(ControlKeys, &'static str)>,
    /// The pairing secret the control channel is keyed from (transient: SRP session key; else the
    /// pair-verify shared secret), kept so a capture's .meta can name it for the off-box decrypt test.
    secret: Option<(Vec<u8>, &'static str)>,
    /// Our address on this connection (timingPeerInfo).
    local_ip: String,
    sess: Arc<Session>,
}

impl Conn<'_> {
    fn deny(&self, req: &Request, what: &str) -> Response {
        lg!(
            "#{} >>> {what}: REFUSED BY DESIGN -> {}",
            self.id, self.cfg.deny_status
        );
        Response::status(req, self.cfg.deny_status, "Forbidden")
    }

    /// `/auth-setup` — MFi-SAP M1 -> M2, driven by the existing `mfi::sap::MfiSapServer` and the
    /// genuine coprocessor via `mfi-i2c-local`. Reached only when the variant advertises bit 26.
    fn auth_setup(&mut self, req: &Request) -> Response {
        let et = req.header("X-Apple-ET");
        lg!(
            "#{} auth-setup: X-Apple-ET={:?} body={} B version=0x{:02x} first16={}",
            self.id, et, req.body.len(), req.body.first().copied().unwrap_or(0), hex(&req.body, 16)
        );
        if !self.cfg.mfi_auth {
            return self.deny(req, "auth-setup (variant has MFi auth disabled)");
        }
        let mut signer = ChipSigner;
        match self.sap.exchange(&req.body, &mut signer) {
            Ok(m2) => {
                // M2 = <32 ourPub><4 BE certLen><cert><4 BE sigLen><AES-CTR(sig)>. Log the shape and
                // first 16 bytes only; never the full certificate or signature.
                let clen = m2.get(32..36).map(|b| u32::from_be_bytes(b.try_into().unwrap()));
                let slen = clen
                    .map(|c| 36 + c as usize)
                    .and_then(|o| m2.get(o..o + 4))
                    .map(|b| u32::from_be_bytes(b.try_into().unwrap()));
                lg!(
                    "#{} auth-setup M2: {} B total, cert {:?} B, sig {:?} B, ourPub16={} cert16={}",
                    self.id, m2.len(), clen, slen, hex(&m2[..32.min(m2.len())], 16),
                    m2.get(36..).map(|c| hex(c, 16)).unwrap_or_default()
                );
                let mut r = Response::ok(req, Some(OCTET), m2);
                if let Some(et) = et {
                    r.headers.push(("X-Apple-ET".into(), et.into()));
                }
                r
            }
            Err(e) => {
                lg!("#{} auth-setup FAILED in MFi-SAP: {e} -> 500", self.id);
                Response::status(req, 500, "Internal Server Error")
            }
        }
    }

    fn pair_setup(&mut self, req: &Request) -> Response {
        let items = log_tlv("in ", &req.body);
        let state = tlv_first(&items, tlv_type::STATE).and_then(|v| v.first().copied()).unwrap_or(0);
        let method = tlv_first(&items, tlv_type::METHOD).and_then(|v| v.first().copied());
        let flags = tlv_first(&items, tlv_type::FLAGS).map(|v| {
            v.iter().rev().fold(0u64, |a, b| a << 8 | u64::from(*b)) // little-endian
        });
        lg!(
            "#{} pair-setup M{state} method={method:?} flags={} X-Apple-HKP={:?}",
            self.id,
            flags.map(|f| format!("0x{f:x}")).unwrap_or_else(|| "absent".into()),
            req.header("X-Apple-HKP")
        );
        if state == 1 {
            self.setup = None;
            self.transient = None;
            if method == Some(1) {
                lg!(
                    "#{} NOTE: Method=1 (MFi pair-setup) requested. The probe continues with the SRP \
                     proof ONLY; M4 will carry no MFi certificate/signature (never signed here).",
                    self.id
                );
            }
            if flags.is_some_and(|f| f & 0x10 != 0) {
                lg!(
                    "#{} TRANSIENT pair-setup (Flags bit 4). PairSetupServer has no transient mode; \
                     driving pairing::srp::SrpServer directly (pin {})",
                    self.id, self.cfg.pin
                );
                let mut salt = [0u8; 16];
                urandom(&mut salt);
                let Some(srv) = SrpServer::new(b"Pair-Setup", self.cfg.pin.as_bytes(), &salt) else {
                    return Response::status(req, 500, "Internal Server Error");
                };
                let out = tlv::encode(&[
                    (tlv_type::STATE, &[2]),
                    (tlv_type::SALT, &salt),
                    (tlv_type::PUBLIC_KEY, &srv.b_pub()),
                ]);
                self.transient = Some(srv);
                log_tlv("out", &out);
                return Response::ok(req, Some(OCTET), out);
            }
            lg!(
                "#{} NON-transient pair-setup: if the iPhone asks for a code, enter {}",
                self.id, self.cfg.pin
            );
        }
        if state == 3 {
            if let Some(mut srv) = self.transient.take() {
                let a = tlv_first(&items, tlv_type::PUBLIC_KEY).unwrap_or(&[]);
                let proof = tlv_first(&items, tlv_type::PROOF).unwrap_or(&[]);
                let out = match srv.verify(a, proof) {
                    Some(m2) => {
                        let key = srv.session_key().map(<[u8]>::to_vec).unwrap_or_default();
                        lg!(
                            "#{} transient SRP proof OK; session key {} B -> control channel \
                             encrypts after this reply",
                            self.id, key.len()
                        );
                        self.pending = Some((derive_control_keys(&key), "transient pair-setup SRP key"));
                        self.secret = Some((key, "transient pair-setup SRP session key"));
                        tlv::encode(&[(tlv_type::STATE, &[4]), (tlv_type::PROOF, &m2)])
                    }
                    None => {
                        lg!("#{} transient SRP proof MISMATCH (phone used another code?)", self.id);
                        tlv::encode(&[(tlv_type::STATE, &[4]), (tlv_type::ERROR, &[tlv_error::AUTHENTICATION])])
                    }
                };
                log_tlv("out", &out);
                return Response::ok(req, Some(OCTET), out);
            }
        }
        let identity = self.cfg.identity;
        let setup = self
            .setup
            .get_or_insert_with(|| PairSetupServer::new(identity, self.cfg.pin.as_bytes().to_vec()));
        match setup.exchange(&req.body, &mut self.peers) {
            Ok((out, done)) => {
                log_tlv("out", &out);
                lg!("#{} pair-setup M{state} -> M{} (exchange finished: {done})", self.id, state + 1);
                Response::ok(req, Some(OCTET), out)
            }
            Err(e) => {
                lg!("#{} pair-setup M{state} FAILED in pairing crate: {e} -> 470", self.id);
                Response::status(req, 470, "Connection Authorization Required")
            }
        }
    }

    fn pair_verify(&mut self, req: &Request) -> Response {
        let items = log_tlv("in ", &req.body);
        let state = tlv_first(&items, tlv_type::STATE).and_then(|v| v.first().copied()).unwrap_or(0);
        lg!("#{} pair-verify M{state} X-Apple-HKP={:?}", self.id, req.header("X-Apple-HKP"));
        let identity = self.cfg.identity;
        if state == 1 {
            self.verify = None;
        }
        let verify = self.verify.get_or_insert_with(|| PairVerifyServer::new(identity));
        match verify.exchange(&req.body, &self.peers) {
            Ok((out, done)) => {
                log_tlv("out", &out);
                if done {
                    match verify.shared_secret() {
                        Some(s) => {
                            lg!("#{} pair-verify COMPLETE -> control channel encrypts after this reply", self.id);
                            self.pending = Some((derive_control_keys(&s), "pair-verify shared secret"));
                            self.secret = Some((s.to_vec(), "pair-verify shared secret"));
                        }
                        None => lg!("#{} pair-verify finished WITHOUT success (stays plaintext)", self.id),
                    }
                }
                Response::ok(req, Some(OCTET), out)
            }
            Err(e) => {
                lg!("#{} pair-verify M{state} FAILED in pairing crate: {e} -> 470", self.id);
                Response::status(req, 470, "Connection Authorization Required")
            }
        }
    }

    /// What an OFF-box decrypt test of this stream needs. Holds session key material, so it goes
    /// only into the capture's `.meta` (RAM, 0600), never into the log.
    fn stream_meta(&self, ty: u64, scid: Option<u64>, sd: &plist::Dictionary) -> String {
        use plist::Value as V;
        let mut meta = format!("type={ty}\nvariant={}\nconn={}\n", self.cfg.tag, self.id);
        if let Some(scid) = scid {
            meta.push_str(&format!("scid={scid}\n"));
        }
        for (t, id) in self.sess.scids.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            meta.push_str(&format!("scid_{t}={id}\n"));
        }
        for (k, v) in sd.iter() {
            match v {
                V::Integer(i) => meta.push_str(&format!("f_{k}={}\n", i.as_unsigned().map(|u| u.to_string()).unwrap_or_else(|| i.as_signed().unwrap_or(0).to_string()))),
                V::String(x) => meta.push_str(&format!("f_{k}={}\n", ascii(x.as_bytes(), 80))),
                V::Boolean(x) => meta.push_str(&format!("f_{k}={x}\n")),
                _ => {}
            }
        }
        if let Some(V::Data(shk)) = sd.get("shk") {
            meta.push_str(&format!("shk={}\n", hex(shk, shk.len())));
        }
        if let Some((sec, src)) = &self.secret {
            meta.push_str(&format!("secret={}\nsecret_src={src}\n", hex(sec, sec.len())));
        }
        meta
    }

    fn setup_req(&mut self, req: &Request, plist: Option<&plist::Value>) -> Response {
        use plist::Value as V;
        self.sess.start_ticker();
        let Some(d) = plist.and_then(|p| p.as_dictionary()) else {
            // Legacy (AirPlay 1 / RAOP) SETUP: parameters ride the Transport header.
            lg!("#{} SETUP (legacy, no plist) Transport={:?}", self.id, req.header("Transport"));
            self.sess.stream_reqs.lock().unwrap_or_else(|p| p.into_inner()).push("legacy RAOP SETUP".into());
            let (dp, cp, tp) = (
                udp_sink(format!("#{} raop data", self.id), self.sess.stream("raop data"), None),
                udp_sink(format!("#{} raop control", self.id), self.sess.stream("raop control"), None),
                udp_sink(format!("#{} raop timing", self.id), self.sess.stream("raop timing"), None),
            );
            let mut r = Response::ok(req, None, Vec::new());
            r.headers.push((
                "Transport".into(),
                format!("RTP/AVP/UDP;unicast;mode=record;server_port={dp};control_port={cp};timing_port={tp}"),
            ));
            r.headers.push(("Session".into(), "1".into()));
            return r;
        };
        let keyinfo = |d: &plist::Dictionary| -> String {
            ["ekey", "eiv", "et", "shk", "streamConnectionID", "timingProtocol", "isScreenMirroringSession"]
                .iter()
                .map(|k| match d.get(k) {
                    Some(V::Data(b)) => format!("{k}=PRESENT({} B)", b.len()),
                    Some(v) => format!("{k}={}", scalar(v)),
                    None => format!("{k}=absent"),
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        let at = self.sess.started.elapsed().as_secs();
        let keys: Vec<&String> = d.keys().collect();
        lg!("#{} SETUP top-level keys: {keys:?}", self.id);
        lg!("#{} SETUP Q2 top-level: {}", self.id, keyinfo(d));
        let mut out = plist::Dictionary::new();
        match d.get("streams").and_then(|s| s.as_array()) {
            Some(streams) => {
                let mut outs = Vec::new();
                for s in streams {
                    let Some(sd) = s.as_dictionary() else { continue };
                    let ty = sd.get("type").and_then(|t| t.as_unsigned_integer()).unwrap_or(0);
                    let skeys: Vec<&String> = sd.keys().collect();
                    // A plist integer may arrive negative: the salt uses the unsigned 64-bit value.
                    let scid = sd.get("streamConnectionID").and_then(|v| match v {
                        V::Integer(i) => Some(i.as_unsigned().unwrap_or_else(|| i.as_signed().unwrap_or(0) as u64)),
                        _ => None,
                    });
                    lg!("#{} SETUP stream type {ty} keys: {skeys:?}", self.id);
                    lg!("#{} SETUP Q2 stream type {ty}: {}", self.id, keyinfo(sd));
                    self.sess.stream_reqs.lock().unwrap_or_else(|p| p.into_inner()).push(format!(
                        "+{at}s type {ty} scid={} keys={skeys:?}", scid.map(|s| s.to_string()).unwrap_or_else(|| "none".into())
                    ));
                    if let Some(scid) = scid {
                        self.sess.scids.lock().unwrap_or_else(|p| p.into_inner()).push((ty, scid));
                    }
                    let meta = self.stream_meta(ty, scid, sd);
                    let name = format!("{}-type{ty}", self.cfg.tag);
                    let data_stat = self.sess.stream(&format!("type {ty} data"));
                    let mut o = plist::Dictionary::new();
                    o.insert("type".into(), V::Integer(ty.into()));
                    if matches!(ty, 110 | 103) {
                        // Media over TCP: capture plus what an off-box decrypt test needs.
                        let p = capture_sink(format!("#{} stream type {ty} data", self.id), ty, name, meta, data_stat);
                        o.insert("dataPort".into(), V::Integer(u64::from(p).into()));
                        if ty == 103 {
                            // Buffered audio (the reference receiver's reply): the receiver states its
                            // buffer size and a control port alongside the TCP data port.
                            let cp = udp_sink(
                                format!("#{} stream type {ty} control", self.id),
                                self.sess.stream(&format!("type {ty} control")), None,
                            );
                            o.insert("audioBufferSize".into(), V::Integer((8u64 * 1024 * 1024).into()));
                            o.insert("controlPort".into(), V::Integer(u64::from(cp).into()));
                        }
                    } else if ty == 130 {
                        let p = tcp_sink(format!("#{} stream type {ty} data", self.id), data_stat, None);
                        o.insert("dataPort".into(), V::Integer(u64::from(p).into()));
                    } else {
                        // Every other type (96 realtime audio, 100/101, anything new): UDP data,
                        // captured as length-prefixed datagrams, plus a UDP control port.
                        let p = udp_sink(format!("#{} stream type {ty} data", self.id), data_stat, Some((name, meta)));
                        let cp = udp_sink(
                            format!("#{} stream type {ty} control", self.id),
                            self.sess.stream(&format!("type {ty} control")), None,
                        );
                        o.insert("dataPort".into(), V::Integer(u64::from(p).into()));
                        o.insert("controlPort".into(), V::Integer(u64::from(cp).into()));
                    }
                    outs.push(V::Dictionary(o));
                }
                out.insert("streams".into(), V::Array(outs));
            }
            None => {
                self.sess.stream_reqs.lock().unwrap_or_else(|p| p.into_inner()).push(format!(
                    "+{at}s session SETUP timingProtocol={} keys={keys:?}",
                    d.get("timingProtocol").and_then(|v| v.as_string()).unwrap_or("absent")
                ));
                // The event channel is encrypted with the Events keys of the same pairing secret
                // (existing rtsp::control derivation); the sink decrypts inbound only to LOG it.
                let ev_keys = self.secret.as_ref().map(|(sec, _)| rtsp::control::derive_event_keys(sec));
                let ep = tcp_sink(format!("#{} event", self.id), self.sess.stream("event"), ev_keys);
                out.insert("eventPort".into(), V::Integer(u64::from(ep).into()));
                if d.get("timingProtocol").and_then(|v| v.as_string()) == Some("PTP") {
                    // PTP session (the reference receiver's reply): no NTP timing port; name ourselves
                    // as a timing peer. The clock itself is followed on udp 319/320, never answered.
                    out.insert("timingPort".into(), V::Integer(0u64.into()));
                    let mut tpi = plist::Dictionary::new();
                    tpi.insert("Addresses".into(), V::Array(vec![V::String(self.local_ip.clone())]));
                    tpi.insert("ID".into(), V::String(self.local_ip.clone()));
                    out.insert("timingPeerInfo".into(), V::Dictionary(tpi));
                } else {
                    let tp = udp_sink(format!("#{} timing", self.id), self.sess.stream("timing"), None);
                    out.insert("timingPort".into(), V::Integer(u64::from(tp).into()));
                }
            }
        }
        let mut lines = Vec::new();
        dump_plist(&V::Dictionary(out.clone()), 6, &mut lines);
        lg!("#{} SETUP reply plist:", self.id);
        for l in lines {
            lg!("{l}");
        }
        Response::ok(req, Some(BPLIST), to_bplist(out))
    }

    fn handle(&mut self, req: &Request) -> Response {
        let plist = parse_plist(&req.body);
        let path = req.path().to_string();
        let m = req.method.as_str();
        // fp-setup (FairPlay) is refused unconditionally — never replayed, never answered. Matched by
        // substring so no spelling (fp-setup, fp-setup2) slips through.
        if path.contains("fp-setup") {
            return self.deny(req, "fp-setup (FairPlay)");
        }
        if path.contains("auth-setup") {
            return self.auth_setup(req);
        }
        match (m, path.as_str()) {
            ("GET", "/info") => {
                let want_txt = plist
                    .as_ref()
                    .and_then(|p| p.as_dictionary())
                    .and_then(|d| d.get("qualifier"))
                    .and_then(|q| q.as_array())
                    .is_some_and(|a| a.iter().any(|v| v.as_string() == Some("txtAirPlay")));
                let body = build_info(self.cfg, want_txt);
                lg!("#{} /info -> {} B bplist (txtAirPlay included: {want_txt})", self.id, body.len());
                Response::ok(req, Some(BPLIST), body)
            }
            ("POST", "/pair-pin-start") => {
                lg!("#{} pair-pin-start: a real receiver would now DISPLAY a code; enter {} on the iPhone", self.id, self.cfg.pin);
                Response::ok(req, None, Vec::new())
            }
            ("POST", "/pair-setup") => self.pair_setup(req),
            ("POST", "/pair-verify") => self.pair_verify(req),
            ("POST", "/feedback") => {
                self.sess.feedback.fetch_add(1, Ordering::Relaxed);
                Response::ok(req, None, Vec::new())
            }
            ("POST", "/command" | "/audioMode") => Response::ok(req, None, Vec::new()),
            ("POST", _) => {
                // An endpoint this probe does not know: say so in the log, but answer an empty 200
                // so one unknown POST does not end a long capture.
                lg!("#{} UNKNOWN endpoint POST {path} -> empty 200 (logged above)", self.id);
                Response::ok(req, None, Vec::new())
            }
            ("GET", _) => {
                lg!("#{} UNKNOWN endpoint GET {path} -> 404", self.id);
                Response::status(req, 404, "Not Found")
            }
            ("TEARDOWN", _) => {
                self.sess.teardown.store(true, Ordering::Relaxed);
                lg!("#{} TEARDOWN at +{}s", self.id, self.sess.started.elapsed().as_secs());
                Response::ok(req, None, Vec::new())
            }
            ("OPTIONS", _) => {
                let mut r = Response::ok(req, None, Vec::new());
                r.headers.push((
                    "Public".into(),
                    "ANNOUNCE, SETUP, RECORD, PAUSE, FLUSH, FLUSHBUFFERED, TEARDOWN, OPTIONS, POST, GET, PUT, \
                     GET_PARAMETER, SET_PARAMETER, SETPEERS, SETRATEANCHORTIME"
                        .into(),
                ));
                r
            }
            ("ANNOUNCE", _) => {
                // AirPlay 1 audio: the SDP names the key transport (rsaaeskey / fpaeskey / mfiaeskey).
                for line in String::from_utf8_lossy(&req.body).lines() {
                    lg!("    sdp {}", ascii(line.as_bytes(), 200));
                }
                Response::ok(req, None, Vec::new())
            }
            ("SETUP", _) => self.setup_req(req, plist.as_ref()),
            ("GET_PARAMETER", _) => {
                let want = String::from_utf8_lossy(&req.body).trim().to_string();
                lg!("#{} GET_PARAMETER '{want}'", self.id);
                if want == "volume" {
                    Response::ok(req, Some("text/parameters"), b"volume: -20.000000\r\n".to_vec())
                } else {
                    Response::ok(req, None, Vec::new())
                }
            }
            ("SET_PARAMETER", _) => {
                if req.header("Content-Type") == Some("text/parameters") {
                    lg!("#{} SET_PARAMETER {}", self.id, ascii(&req.body, 200));
                }
                Response::ok(req, None, Vec::new())
            }
            ("RECORD", _) => {
                let mut r = Response::ok(req, None, Vec::new());
                r.headers.push(("Audio-Latency".into(), "0".into()));
                r
            }
            _ => Response::ok(req, None, Vec::new()),
        }
    }
}

fn serve(mut s: TcpStream, n: u32, cfg: &'static Cfg, peers: Peers) {
    // `#3/B7` = third connection overall, to variant B7.
    let id = format!("{n}/{}", cfg.tag);
    let peer = s.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let local = s.local_addr().map(|a| a.to_string()).unwrap_or_default();
    lg!("#{id} CONNECT from {peer} to {local}");
    // No read timeout anywhere on a session: the probe never ends one; only the peer does.
    let _ = s.set_read_timeout(None);
    let _ = s.set_nodelay(true);
    let sess = Session::new(id.clone(), s.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default());
    let mut c = Conn {
        id: id.clone(), cfg, peers, setup: None, transient: None, verify: None,
        sap: MfiSapServer::new(), pending: None, secret: None,
        local_ip: s.local_addr().map(|a| a.ip().to_string()).unwrap_or_default(),
        sess: sess.clone(),
    };
    let mut chan: Option<ControlChannel> = None;
    // After key activation, the first inbound bytes decide whether the PEER actually switched.
    let mut decided = true;
    let (mut raw, mut plain) = (Vec::<u8>::new(), Vec::<u8>::new());
    let mut n_req = 0u32;
    let mut buf = [0u8; 8192];
    let why = 'conn: loop {
        // 1. raw -> plain
        if chan.is_some() && !decided && raw.len() >= 2 {
            // An encrypted frame starts with a u16 LE length <= 16384. Two uppercase ASCII letters
            // read as >= 0x4141 = 16705, so they can only be a plaintext method token.
            if raw[0].is_ascii_uppercase() && raw[1].is_ascii_uppercase() {
                lg!("#{id} NOTE: peer kept sending PLAINTEXT after key activation; probe falls back to plaintext");
                chan = None;
            }
            decided = true;
        }
        match chan.as_mut() {
            Some(ch) if decided => loop {
                match ch.decrypt_frame(&raw) {
                    Ok(Some((pt, n))) => {
                        plain.extend_from_slice(&pt);
                        raw.drain(..n);
                    }
                    Ok(None) => break,
                    Err(e) => {
                        lg!("#{id} DECRYPT FAILED ({e}); raw[0..64]={}", hex(&raw, 64));
                        break 'conn "decrypt failure";
                    }
                }
            },
            Some(_) => {}
            None => plain.append(&mut raw),
        }
        // 2. one request
        match Request::parse(&plain) {
            Ok(Some((req, used))) => {
                plain.drain(..used);
                n_req += 1;
                lg!(
                    "#{id}.{n_req} <<< {} {} {} [{}] body={} B",
                    req.method, req.uri, req.version.token(),
                    if chan.is_some() { "ENCRYPTED" } else { "plain" },
                    req.body.len()
                );
                for (k, v) in &req.headers {
                    lg!("    H {k}: {v}");
                }
                sess.note_request(&req.method, req.path());
                log_body(&id, &req);
                let mut resp = c.handle(&req);
                resp.headers.push(("Server".into(), format!("AirTunes/{}", cfg.srcvers)));
                if cfg.jack {
                    resp.headers.push(("Apple-Jack-Status".into(), "connected; type=analog".into()));
                }
                let wire = resp.serialize();
                // The FULL response as sent: every head line, then the body (hex, capped at 512 B).
                let head_len = wire.len() - resp.body.len();
                lg!("#{id}.{n_req} >>> {} {} body={} B", resp.status, resp.reason, resp.body.len());
                for l in String::from_utf8_lossy(&wire[..head_len]).lines().filter(|l| !l.is_empty()) {
                    lg!("    > {l}");
                }
                if !resp.body.is_empty() {
                    lg!("    > B[0..512] {}", hex(&resp.body, 512));
                }
                let wire = match chan.as_mut() {
                    Some(ch) => ch.encrypt_frame(&wire),
                    None => wire,
                };
                if let Err(e) = s.write_all(&wire) {
                    lg!("#{id} write failed: {e}");
                    break "write failure";
                }
                if let Some((keys, src)) = c.pending.take() {
                    lg!("#{id} control channel keys ACTIVE (from {src})");
                    chan = Some(ControlChannel::new(keys));
                    decided = false;
                    // Anything already buffered behind this request arrived after the peer's own
                    // switch point, so it belongs to the raw (possibly encrypted) stream.
                    plain.append(&mut raw);
                    std::mem::swap(&mut raw, &mut plain);
                }
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                lg!("#{id} UNPARSEABLE request ({e:?}); {} B, [0..64]={} ascii={}", plain.len(), hex(&plain, 64), ascii(&plain, 64));
                break "parse error";
            }
        }
        if plain.len() > 512 * 1024 || raw.len() > 512 * 1024 {
            break "oversized buffer";
        }
        // 3. more bytes
        match s.read(&mut buf) {
            Ok(0) => break "peer closed",
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(e) => {
                lg!("#{id} read error: {e}");
                break "read error/timeout";
            }
        }
    };
    if !plain.is_empty() || !raw.is_empty() {
        lg!(
            "#{id} leftover at close: plain {} B [{}] raw {} B [{}]",
            plain.len(), hex(&plain, 64), raw.len(), hex(&raw, 64)
        );
    }
    lg!("#{id} DISCONNECT ({why}) after {n_req} request(s)");
    sess.alive.store(false, Ordering::Relaxed);
    if sess.ticking.load(Ordering::Relaxed) {
        // End-of-session summary (only for connections that got as far as a SETUP).
        let end = match (why, sess.teardown.load(Ordering::Relaxed)) {
            ("peer closed", true) => "TEARDOWN, then the peer closed the control connection".to_string(),
            ("peer closed", false) => "peer closed the control connection (no TEARDOWN)".to_string(),
            (other, t) => format!("error: {other} (TEARDOWN seen: {t})"),
        };
        lg!("SUMMARY #{id} end: {end}; duration {} s; {} requests", sess.started.elapsed().as_secs(), n_req);
        let reqs: Vec<String> = sess.requests.lock().unwrap_or_else(|p| p.into_inner()).iter().map(|(k, n)| format!("{k} x{n}")).collect();
        lg!("SUMMARY #{id} requests: {}", reqs.join("; "));
        for r in sess.stream_reqs.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            lg!("SUMMARY #{id} stream requested: {r}");
        }
        lg!("SUMMARY #{id} totals: feedback={} | {} |{}", sess.feedback.load(Ordering::Relaxed), sess.totals(), ptp_counts());
        lg!("SUMMARY #{id} ports touched by {}: {}", sess.peer_ip, ports_touched(&sess.peer_ip));
    }
}

// ---------------------------------------------------------------- main

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

fn main() {
    let _ = T0.set(Instant::now());
    let cfgs: &'static [Cfg] = Box::leak(parse_args().into_boxed_slice());
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    let any_mfi = cfgs.iter().any(|c| c.mfi_auth);
    lg!(
        "limits: fp-setup refused (no FairPlay); auth-setup answered with the on-board MFi chip for \
         {} of {} variant(s); RAM-only state, nothing to flash",
        cfgs.iter().filter(|c| c.mfi_auth).count(), cfgs.len()
    );
    if any_mfi {
        chip_selftest();
    }
    ptp_listen(319);
    ptp_listen(320);
    net_census();
    let mdns = match ServiceDaemon::new() {
        Ok(m) => m,
        Err(e) => {
            lg!("FATAL: mDNS daemon: {e}");
            std::process::exit(1);
        }
    };
    static CONN: AtomicU32 = AtomicU32::new(0);
    let mut registered = Vec::new();
    for cfg in cfgs {
        let bits: Vec<String> = (0..64).filter(|n| cfg.bit(*n)).map(|n| n.to_string()).collect();
        lg!(
            "variant {} mask {} features={} (0x{:x}) bits [{}] name='{}' port={} srcvers={} protovers={} \
             flags=0x{:x} model={} raop={} ap2txt={} jack={} deny-status={} extra /info keys={:?} \
             display={:?} maxfps={:?} extra display keys={:?}",
            cfg.tag, cfg.label, cfg.features_txt(), cfg.features, bits.join(","), cfg.name, cfg.port,
            cfg.srcvers, cfg.protovers, cfg.flags, cfg.model, cfg.raop, cfg.ap2_txt, cfg.jack, cfg.deny_status,
            cfg.extra_info, cfg.display, cfg.maxfps, cfg.extra_disp
        );
        lg!(
            "variant {} identity: deviceid={} pi={} pk={} (pk advertised in TXT: {}; same key signs pair-verify)",
            cfg.tag, cfg.device_id, cfg.pi, hex(&cfg.pk, 32), cfg.ap2_txt
        );
        let listener = match TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], cfg.port))) {
            Ok(l) => l,
            Err(e) => {
                lg!("FATAL: cannot bind :{}: {e}", cfg.port);
                std::process::exit(1);
            }
        };
        // Advertise only once the port is listening. addr-auto publishes each interface's own
        // address on that interface, so the AP side and (if up) the NCM side both resolve.
        let host = format!("ocbm-probe-{}.local.", cfg.device_id.replace(':', "").to_lowercase());
        let mut services = vec![("_airplay._tcp.local.", cfg.name.clone(), cfg.airplay_txt())];
        if cfg.raop {
            services.push((
                "_raop._tcp.local.",
                format!("{}@{}", cfg.device_id.replace(':', ""), cfg.name),
                cfg.raop_txt(),
            ));
        }
        for (ty, inst, txt) in services {
            let props: HashMap<String, String> = txt.iter().cloned().collect();
            let info = ServiceInfo::new(ty, &inst, &host, "", cfg.port, props).map(|i| i.enable_addr_auto());
            match info.and_then(|i| {
                let full = i.get_fullname().to_string();
                mdns.register(i).map(|_| full)
            }) {
                Ok(full) => {
                    let t: Vec<String> = txt.iter().map(|(k, v)| format!("{k}={v}")).collect();
                    lg!("mDNS registered {full} host={host} port={} TXT: {}", cfg.port, t.join(" "));
                    registered.push(full);
                }
                Err(e) => {
                    lg!("FATAL: mDNS register {ty}: {e}");
                    std::process::exit(1);
                }
            }
        }
        let peers = Peers::load(cfg.peers_path.clone());
        std::thread::spawn(move || {
            for s in listener.incoming() {
                match s {
                    Ok(s) => {
                        let n = CONN.fetch_add(1, Ordering::Relaxed) + 1;
                        let peers = peers.clone();
                        std::thread::spawn(move || serve(s, n, cfg, peers));
                    }
                    Err(e) => {
                        lg!("accept error: {e}");
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
            }
        });
    }
    lg!("listening: {} variant(s) — waiting for a sender", cfgs.len());

    while !STOP.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
    }
    // Goodbye packets: without them the iPhone keeps a stale entry for the record TTL and a
    // variant switch would leave dead receivers in its list.
    for full in &registered {
        if let Ok(rx) = mdns.unregister(full) {
            let _ = rx.recv_timeout(Duration::from_secs(2));
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    if let Ok(rx) = mdns.shutdown() {
        let _ = rx.recv_timeout(Duration::from_secs(2));
    }
    lg!("==== airplay-probe stop (signal): mDNS goodbye sent for {} service(s)", registered.len());
}

// ---------------------------------------------------------------- tests
// The controller side below is the pairing crate's own `srp::client_proof` ("for tests and
// tooling") and the same `rtsp::control` codec with the two keys swapped. No crypto is written here.

#[cfg(test)]
mod tests {
    use super::*;
    use pairing::srp::client_proof;

    fn start(spec: &str) -> SocketAddr {
        let cfg: &'static Cfg = Box::leak(Box::new(parse_spec(spec, "t", 0, None)));
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for (n, s) in l.incoming().flatten().enumerate() {
                std::thread::spawn(move || serve(s, n as u32 + 1, cfg, Peers::default()));
            }
        });
        addr
    }

    /// One request/response; encrypts and decrypts through `chan` when it is set.
    fn rt(s: &mut TcpStream, chan: &mut Option<ControlChannel>, head: &str, body: &[u8]) -> (u16, Vec<u8>) {
        let mut req = format!("{head}\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        req.extend_from_slice(body);
        let wire = match chan.as_mut() {
            Some(c) => c.encrypt_frame(&req),
            None => req,
        };
        s.write_all(&wire).unwrap();
        let (mut raw, mut plain, mut buf) = (Vec::new(), Vec::new(), [0u8; 4096]);
        loop {
            match chan.as_mut() {
                Some(c) => {
                    while let Some((pt, n)) = c.decrypt_frame(&raw).expect("response decrypts") {
                        plain.extend_from_slice(&pt);
                        raw.drain(..n);
                    }
                }
                None => plain.append(&mut raw),
            }
            if let Some(h) = plain.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&plain[..h]).to_string();
                let len: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("Content-Length: "))
                    .map(|v| v.parse().unwrap())
                    .unwrap_or(0);
                if plain.len() >= h + 4 + len {
                    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
                    return (status, plain[h + 4..h + 4 + len].to_vec());
                }
            }
            let n = s.read(&mut buf).unwrap();
            assert!(n > 0, "server closed mid-response");
            raw.extend_from_slice(&buf[..n]);
        }
    }

    /// Transient pair-setup M1..M4 in the clear; returns the SRP session key.
    fn transient(s: &mut TcpStream) -> Vec<u8> {
        let mut plain = None;
        let m1 = tlv::encode(&[(tlv_type::METHOD, &[0]), (tlv_type::STATE, &[1]), (tlv_type::FLAGS, &[0x10])]);
        let (st, m2) = rt(s, &mut plain, "POST /pair-setup RTSP/1.0\r\nCSeq: 1\r\nX-Apple-HKP: 4", &m1);
        assert_eq!(st, 200);
        let m2 = tlv::decode(&m2).unwrap();
        let (salt, b_pub) = (tlv_first(&m2, tlv_type::SALT).unwrap(), tlv_first(&m2, tlv_type::PUBLIC_KEY).unwrap());
        let (a_pub, proof, key) = client_proof(b"Pair-Setup", b"3939", salt, &[7u8; 32], b_pub);
        let m3 = tlv::encode(&[(tlv_type::STATE, &[3]), (tlv_type::PUBLIC_KEY, &a_pub), (tlv_type::PROOF, &proof)]);
        let (st, m4) = rt(s, &mut plain, "POST /pair-setup RTSP/1.0\r\nCSeq: 2", &m3);
        assert_eq!(st, 200);
        let m4 = tlv::decode(&m4).unwrap();
        assert_eq!(tlv_first(&m4, tlv_type::STATE), Some(&[4u8][..]));
        assert!(tlv_first(&m4, tlv_type::ERROR).is_none(), "server accepted the proof");
        assert_eq!(tlv_first(&m4, tlv_type::PROOF).map(<[u8]>::len), Some(64));
        key
    }

    #[test]
    fn transient_pair_setup_then_encrypted_info_and_setup() {
        let addr = start("T/B-26+7");
        let mut s = TcpStream::connect(addr).unwrap();
        let key = transient(&mut s);
        // Controller view of the same keys: its write key is the server's read key.
        let k = derive_control_keys(&key);
        let mut chan = Some(ControlChannel::new(ControlKeys { read: k.write, write: k.read }));
        let (st, info) = rt(&mut s, &mut chan, "GET /info RTSP/1.0\r\nCSeq: 3", b"");
        assert_eq!(st, 200);
        let info = plist::Value::from_reader(std::io::Cursor::new(info)).unwrap();
        let d = info.as_dictionary().unwrap();
        assert_eq!(d.get("features").and_then(|f| f.as_unsigned_integer()), Some(0x1_c340_405f_8a80));
        assert!(d.get("displays").is_some(), "bit 7 advertises a display");

        let mut stream = plist::Dictionary::new();
        stream.insert("type".into(), plist::Value::Integer(110u64.into()));
        stream.insert("streamConnectionID".into(), plist::Value::Integer(42u64.into()));
        let mut req = plist::Dictionary::new();
        req.insert("streams".into(), plist::Value::Array(vec![plist::Value::Dictionary(stream)]));
        let (st, resp) = rt(
            &mut s, &mut chan,
            "SETUP rtsp://x/1 RTSP/1.0\r\nCSeq: 4\r\nContent-Type: application/x-apple-binary-plist",
            &to_bplist(req),
        );
        assert_eq!(st, 200);
        let resp = plist::Value::from_reader(std::io::Cursor::new(resp)).unwrap();
        let out = resp.as_dictionary().unwrap().get("streams").unwrap().as_array().unwrap()[0].as_dictionary().unwrap();
        assert_eq!(out.get("type").and_then(|t| t.as_unsigned_integer()), Some(110));
        assert!(out.get("dataPort").and_then(|t| t.as_unsigned_integer()).unwrap() > 0);
    }

    #[test]
    fn peer_that_stays_plaintext_after_transient_setup_is_still_served() {
        let addr = start("T/B");
        let mut s = TcpStream::connect(addr).unwrap();
        let _ = transient(&mut s);
        let (st, body) = rt(&mut s, &mut None, "GET /info RTSP/1.0\r\nCSeq: 3", b"");
        assert_eq!(st, 200);
        assert!(body.starts_with(b"bplist00"));
    }

    /// fp-setup is always refused; auth-setup is refused when the variant has MFi auth off (`nomfi`),
    /// so the host test never touches a chip it does not have. The live chip path is exercised on the
    /// box, not here.
    #[test]
    fn fairplay_always_refused_and_authsetup_refused_without_mfi() {
        let addr = start("T/B/nomfi");
        let mut s = TcpStream::connect(addr).unwrap();
        let key = transient(&mut s);
        let k = derive_control_keys(&key);
        let mut chan = Some(ControlChannel::new(ControlKeys { read: k.write, write: k.read }));
        for path in ["/auth-setup", "/fp-setup", "/fp-setup2"] {
            let (st, body) = rt(&mut s, &mut chan, &format!("POST {path} RTSP/1.0\r\nCSeq: 9"), &[1, 2, 3]);
            assert_eq!((st, body.len()), (403, 0), "{path}");
        }
    }

    /// H7 is B7 plus feature bit 42 and nothing else; extra /info keys ride only on the variant
    /// that names them.
    #[test]
    fn hevc_variant_is_b7_plus_bit_42_and_info_keys_are_per_variant() {
        let (b7, h7) = (parse_spec("B7/B+7", "t", 0, None), parse_spec("H7/B+7+42", "t", 0, None));
        assert_eq!(h7.features ^ b7.features, 1 << 42);
        assert_eq!(h7.features_txt(), "0x445F8A80,0x1C740");
        assert!(h7.mfi_auth && h7.ap2_txt && h7.srcvers == b7.srcvers && h7.protovers == b7.protovers);
        let info = |c: &Cfg| plist::Value::from_reader(std::io::Cursor::new(build_info(c, false))).unwrap();
        assert_eq!(info(&h7).as_dictionary().unwrap().get("features").and_then(|v| v.as_unsigned_integer()), Some(h7.features));
        let hr = parse_spec("Hr/B+7+42/info.receiverHDRCapability=4k60/info.x=12/info.y=true", "t", 0, None);
        let v = info(&hr);
        let d = v.as_dictionary().unwrap();
        assert_eq!(d.get("receiverHDRCapability").and_then(|v| v.as_string()), Some("4k60"));
        assert_eq!(d.get("x").and_then(|v| v.as_unsigned_integer()), Some(12));
        assert_eq!(d.get("y").and_then(|v| v.as_boolean()), Some(true));
        assert!(info(&h7).as_dictionary().unwrap().get("receiverHDRCapability").is_none());
    }

    /// The default display dictionary is unchanged (B7 stays the control); `display=` / `maxfps=` /
    /// `disp.` change only the variant that names them.
    #[test]
    fn display_is_spec_driven_and_the_default_is_unchanged() {
        let disp = |spec: &str| {
            let v = plist::Value::from_reader(std::io::Cursor::new(build_info(&parse_spec(spec, "t", 0, None), false))).unwrap();
            v.as_dictionary().unwrap().get("displays").unwrap().as_array().unwrap()[0].as_dictionary().unwrap().clone()
        };
        let int = |d: &plist::Dictionary, k: &str| d.get(k).and_then(|v| v.as_unsigned_integer());
        let b7 = disp("B7/B+7");
        assert_eq!((int(&b7, "widthPixels"), int(&b7, "heightPixels"), int(&b7, "width"), int(&b7, "height")), (Some(1920), Some(1080), Some(1920), Some(1080)));
        assert_eq!((int(&b7, "maxFPS"), int(&b7, "features")), (Some(30), Some(14)));
        assert_eq!(b7.get("refreshRate").and_then(|v| v.as_real()), Some(1.0 / 60.0));
        assert_eq!(b7.len(), 12, "no key added to the default dictionary");
        let d = disp("D/B+7+42/display=3840x2160@120");
        assert_eq!((int(&d, "widthPixels"), int(&d, "heightPixels"), int(&d, "width"), int(&d, "maxFPS")), (Some(3840), Some(2160), Some(3840), Some(120)));
        assert_eq!(d.get("refreshRate").and_then(|v| v.as_real()), Some(1.0 / 120.0));
        let d = disp("D/B+7/display=1280x720@30/maxfps=60/disp.widthPixelsMax=3840/disp.receiverSupports444=true");
        assert_eq!((int(&d, "heightPixels"), int(&d, "maxFPS"), int(&d, "widthPixelsMax")), (Some(720), Some(60), Some(3840)));
        assert_eq!(d.get("receiverSupports444").and_then(|v| v.as_boolean()), Some(true));
    }

    #[test]
    fn wifi_station_counters_are_reduced_to_one_line_each() {
        let text = "Station aa:bb:cc:dd:ee:ff (on mlan0)\n\tinactive time:\t10 ms\n\trx bytes:\t1234\n\ttx bytes:\t99\n\ttx retries:\t7\n\ttx failed:\t1\n\tsignal:  \t-48 dBm\n\ttx bitrate:\t150.0 MBit/s MCS 7\n\trx bitrate:\t130.0 MBit/s\n";
        let v = wifi_stations(text);
        assert_eq!(v, vec!["sta aa:bb:cc:dd:ee:ff rxbytes=1234 txbytes=99 txretries=7 txfailed=1 sig=-48dBm txrate=150.0MBit/s rxrate=130.0MBit/s"]);
    }

    #[test]
    fn mfi_auth_defaults_on_only_when_bit26_is_advertised() {
        assert!(parse_spec("B/B", "t", 0, None).mfi_auth, "bit 26 set -> answer auth-setup");
        assert!(!parse_spec("M/M", "t", 0, None).mfi_auth, "no bit 26 -> no auth-setup");
        assert!(parse_spec("Mm/M+26", "t", 0, None).mfi_auth);
        assert!(!parse_spec("B/B/nomfi", "t", 0, None).mfi_auth, "nomfi overrides");
        assert!(parse_spec("M/M/mfi", "t", 0, None).mfi_auth, "mfi forces on");
    }

    /// The buffered-audio flow: a PTP session SETUP names us as a timing peer with no NTP port, and
    /// the type-103 stream SETUP gets a TCP data port, a buffer size and a control port.
    #[test]
    fn ptp_session_and_buffered_audio_setup_replies() {
        use plist::Value as V;
        let addr = start("T/B/nomfi");
        let mut s = TcpStream::connect(addr).unwrap();
        let key = transient(&mut s);
        let k = derive_control_keys(&key);
        let mut chan = Some(ControlChannel::new(ControlKeys { read: k.write, write: k.read }));
        let head = "SETUP rtsp://x/1 RTSP/1.0\r\nCSeq: 5\r\nContent-Type: application/x-apple-binary-plist";

        let mut req = plist::Dictionary::new();
        req.insert("timingProtocol".into(), V::String("PTP".into()));
        let (st, resp) = rt(&mut s, &mut chan, head, &to_bplist(req));
        assert_eq!(st, 200);
        let resp = V::from_reader(std::io::Cursor::new(resp)).unwrap();
        let d = resp.as_dictionary().unwrap();
        assert!(d.get("eventPort").and_then(|v| v.as_unsigned_integer()).unwrap() > 0);
        assert_eq!(d.get("timingPort").and_then(|v| v.as_unsigned_integer()), Some(0));
        let tpi = d.get("timingPeerInfo").and_then(|v| v.as_dictionary()).unwrap();
        assert_eq!(tpi.get("ID").and_then(|v| v.as_string()), Some("127.0.0.1"));
        assert_eq!(tpi.get("Addresses").and_then(|v| v.as_array()).map(Vec::len), Some(1));

        let mut stream = plist::Dictionary::new();
        stream.insert("type".into(), V::Integer(103u64.into()));
        stream.insert("streamConnectionID".into(), V::Integer(7u64.into()));
        stream.insert("shk".into(), V::Data(vec![0xAB; 32]));
        let mut req = plist::Dictionary::new();
        req.insert("streams".into(), V::Array(vec![V::Dictionary(stream)]));
        let (st, resp) = rt(&mut s, &mut chan, head, &to_bplist(req));
        assert_eq!(st, 200);
        let resp = V::from_reader(std::io::Cursor::new(resp)).unwrap();
        let o = resp.as_dictionary().unwrap().get("streams").unwrap().as_array().unwrap()[0].as_dictionary().unwrap();
        let port = |k: &str| o.get(k).and_then(|v| v.as_unsigned_integer()).unwrap_or(0);
        assert_eq!(port("type"), 103);
        assert!(port("dataPort") > 0 && port("controlPort") > 0);
        assert_eq!(port("audioBufferSize"), 8 * 1024 * 1024);
        // The data port really accepts.
        TcpStream::connect(("127.0.0.1", port("dataPort") as u16)).unwrap();
    }

    #[test]
    fn key_material_is_withheld_from_the_decoded_plist_log() {
        use plist::Value as V;
        let mut stream = plist::Dictionary::new();
        stream.insert("shk".into(), V::Data((0u8..32).collect()));
        stream.insert("eiv".into(), V::Data(vec![0xEE; 16]));
        stream.insert("audioFormat".into(), V::Integer(0x40000u64.into()));
        let mut top = plist::Dictionary::new();
        top.insert("streams".into(), V::Array(vec![V::Dictionary(stream)]));
        top.insert("ekey".into(), V::Data(vec![0xCC; 72]));
        let mut lines = Vec::new();
        dump_plist(&V::Dictionary(top), 0, &mut lines);
        let text = lines.join("\n");
        assert!(text.contains("shk: <data 32 bytes: first4=00010203 (rest withheld)>"), "{text}");
        assert!(!text.contains("0405"), "bytes past the first four of shk must not appear: {text}");
        assert!(text.contains("ekey: <data 72 bytes: first4=cccccccc") && !text.contains("cccccccccc"), "{text}");
        assert!(text.contains("eiv: <data 16 bytes: first4=eeeeeeee") && !text.contains("eeeeeeeeee"), "{text}");
        assert!(text.contains("audioFormat: 262144 (0x40000)"), "{text}");
    }

    #[test]
    fn streaming_framer_counts_frames_across_arbitrary_read_boundaries() {
        // 30 buffered-audio packets of varying size, then a truncated one.
        let mut cap = Vec::new();
        for i in 0..30u16 {
            let len = 60 + i * 7;
            cap.extend_from_slice(&len.to_be_bytes());
            cap.extend(std::iter::repeat(0x11).take(len as usize - 2));
        }
        cap.extend_from_slice(&[0x00, 0x50, 0x22]);
        for chunk in [1usize, 3, 61, 4096] {
            let (mut f, st) = (Framer::new(103), StreamStat::default());
            cap.chunks(chunk).for_each(|c| f.feed("t", c, &st));
            assert_eq!((f.frames, st.pkts.load(Ordering::Relaxed), f.dead), (30, 30, false), "chunk {chunk}");
        }
        // Screen: codec (op 1, 37 B), video (op 0), report (op 5, 200 B), video, then a partial header.
        let mut cap = Vec::new();
        for (op, body) in [(1u8, 37usize), (0, 5000), (5, 200), (0, 1), (0, 0)] {
            let mut h = [0u8; 128];
            h[..4].copy_from_slice(&(body as u32).to_le_bytes());
            h[4] = op;
            cap.extend_from_slice(&h);
            cap.extend(std::iter::repeat(0x33).take(body));
        }
        cap.extend_from_slice(&[0u8; 40]);
        for chunk in [1usize, 7, 128, 129, 9000] {
            let (mut f, st) = (Framer::new(110), StreamStat::default());
            cap.chunks(chunk).for_each(|c| f.feed("t", c, &st));
            assert_eq!((f.frames, f.ops[0], f.ops[1], f.ops[5]), (5, 3, 1, 1), "chunk {chunk}");
        }
        // A length prefix too small to be a packet stops parsing for good.
        let (mut f, st) = (Framer::new(103), StreamStat::default());
        f.feed("t", &[0x00, 0x05, 1, 2, 3], &st);
        assert!(f.dead && f.frames == 0);
    }

    #[test]
    fn port_census_records_flows_and_flags_unlistened_ports() {
        // IPv4 + TCP SYN from 192.168.43.100:50000 to :7001, then :9999 (nothing listens), then UDP :319.
        let pkt = |proto: u8, dport: u16, flags: u8| {
            let mut p = vec![0x45, 0, 0, 40, 0, 0, 0, 0, 64, proto, 0, 0, 192, 168, 43, 100, 192, 168, 43, 1];
            p.extend_from_slice(&50000u16.to_be_bytes());
            p.extend_from_slice(&dport.to_be_bytes());
            p.extend_from_slice(&[0; 9]);
            p.push(flags);
            p.extend_from_slice(&[0; 6]);
            p
        };
        let listening = |_: u8, port: u16| port != 9999;
        assert!(census_packet(&pkt(6, 7001, 0x02), listening).unwrap().ends_with("-> :7001 SYN"));
        assert!(census_packet(&pkt(6, 7001, 0x10), listening).is_none(), "an ACK on a known flow is not logged");
        assert!(census_packet(&pkt(6, 9999, 0x02), listening).unwrap().contains("NOTHING LISTENS"));
        assert!(census_packet(&pkt(17, 319, 0), listening).unwrap().contains("udp 192.168.43.100:50000 -> :319 (first packet)"));
        let t = ports_touched("192.168.43.100");
        assert!(t.contains("tcp/7001 pkts=2 syn=1") && t.contains("tcp/9999 pkts=1 syn=1") && t.contains("udp/319 pkts=1"), "{t}");
        assert_eq!(ports_touched("10.0.0.1"), "none seen on the wireless interface");
    }

    #[test]
    fn dmap_metadata_and_secret_key_names() {
        let mut item = Vec::new();
        for (tag, val) in [(b"minm", &b"Song Title"[..]), (b"asar", b"The Artist"), (b"astm", &[0, 3, 0x0d, 0x40])] {
            item.extend_from_slice(tag);
            item.extend_from_slice(&(val.len() as u32).to_be_bytes());
            item.extend_from_slice(val);
        }
        let mut top = b"mlit".to_vec();
        top.extend_from_slice(&(item.len() as u32).to_be_bytes());
        top.extend_from_slice(&item);
        let mut lines = Vec::new();
        dump_dmap(&top, 0, &mut lines);
        let text = lines.join("\n");
        assert!(text.contains("minm title/name: \"Song Title\"") && text.contains("asar artist: \"The Artist\""), "{text}");
        assert!(text.contains("astm duration ms: 200000"), "{text}");
        assert!(is_secret_key("shk") && is_secret_key("ekey") && is_secret_key("streamEncryptionKey") && !is_secret_key("pk"));
    }

    /// LIVE end-to-end check against a RUNNING probe (`PROBE_ADDR=ip:port`, run with `--ignored
    /// --features capcheck`): pair transiently, SETUP a 103, a 110 and a 96 stream, and send media
    /// encrypted the way the receiver's own code expects, so the probe's captures can be pulled and
    /// run through capcheck and a decoder. With `LIVE_AAC` (an ADTS file) and `LIVE_H264` (an
    /// Annex-B file, one slice per frame) the media is real and must then DECODE; otherwise it is
    /// filler that only authenticates. Test media only, never anyone's content.
    #[cfg(feature = "capcheck")]
    #[test]
    #[ignore]
    fn live_capture_round_trip() {
        use chacha20poly1305::aead::{Aead, KeyInit, Payload};
        use plist::Value as V;
        let addr: SocketAddr = std::env::var("PROBE_ADDR").expect("PROBE_ADDR=ip:port").parse().unwrap();

        // Raw AAC access units: strip the 7-byte ADTS header of each frame.
        let aus: Vec<Vec<u8>> = match std::env::var("LIVE_AAC") {
            Ok(p) => {
                let (b, mut at, mut v) = (std::fs::read(p).unwrap(), 0usize, Vec::new());
                while at + 7 <= b.len() && b[at] == 0xFF {
                    let len = ((b[at + 3] as usize & 3) << 11) | ((b[at + 4] as usize) << 3) | (b[at + 5] as usize >> 5);
                    v.push(b[at + 7..at + len].to_vec());
                    at += len;
                }
                v
            }
            Err(_) => (0..25u8).map(|i| vec![i; 400]).collect(),
        };
        // Annex-B -> NAL units.
        let split_nals = |b: &[u8]| -> Vec<Vec<u8>> {
            let mut starts: Vec<(usize, usize)> = Vec::new(); // (start code pos, NAL start)
            let mut i = 0;
            while i + 3 <= b.len() {
                if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 {
                    starts.push((if i > 0 && b[i - 1] == 0 { i - 1 } else { i }, i + 3));
                    i += 3;
                } else {
                    i += 1;
                }
            }
            (0..starts.len()).map(|n| b[starts[n].1..starts.get(n + 1).map(|x| x.0).unwrap_or(b.len())].to_vec()).collect()
        };
        // HEVC (`LIVE_H265`, an Annex-B file, one slice per picture): hvcC from VPS/SPS/PPS.
        let hevc: Option<(Vec<u8>, Vec<(bool, Vec<u8>)>)> = std::env::var("LIVE_H265").ok().map(|p| {
            let (mut ps, mut frames, mut cur): ([Vec<u8>; 3], Vec<(bool, Vec<u8>)>, Vec<u8>) = Default::default();
            for nal in split_nals(&std::fs::read(p).unwrap()) {
                match (nal[0] >> 1) & 0x3f {
                    t @ 32..=34 => ps[t as usize - 32] = nal,
                    35 => {}
                    t => {
                        cur.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                        cur.extend_from_slice(&nal);
                        if t < 32 {
                            frames.push(((16..=21).contains(&t), std::mem::take(&mut cur)));
                        }
                    }
                }
            }
            // profile_tier_level sits 1 byte into the SPS payload, after emulation-prevention removal.
            let mut rbsp = Vec::new();
            for (i, x) in ps[1][2..].iter().enumerate() {
                if !(*x == 3 && i >= 2 && ps[1][i] == 0 && ps[1][i + 1] == 0) {
                    rbsp.push(*x);
                }
            }
            let mut hvcc = vec![1u8];
            hvcc.extend_from_slice(&rbsp[1..13]); // profile byte, 4 compatibility, 6 constraint, level
            hvcc.extend_from_slice(&[0xF0, 0, 0xFC, 0xFD, 0xF8, 0xF8, 0, 0, 0x0F, 3]);
            for (i, nal) in ps.iter().enumerate() {
                hvcc.extend_from_slice(&[0x80 | (32 + i as u8), 0, 1]);
                hvcc.extend_from_slice(&(nal.len() as u16).to_be_bytes());
                hvcc.extend_from_slice(nal);
            }
            (hvcc, frames)
        });
        // H.264: avcC from SPS/PPS, and one AVCC access unit per coded picture.
        let (avcc, frames): (Vec<u8>, Vec<(bool, Vec<u8>)>) = match std::env::var("LIVE_H264") {
            _ if hevc.is_some() => hevc.unwrap(),
            Ok(p) => {
                let b = std::fs::read(p).unwrap();
                let mut starts: Vec<(usize, usize)> = Vec::new(); // (start code pos, NAL start)
                let mut i = 0;
                while i + 3 <= b.len() {
                    if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 {
                        starts.push((if i > 0 && b[i - 1] == 0 { i - 1 } else { i }, i + 3));
                        i += 3;
                    } else {
                        i += 1;
                    }
                }
                let (mut sps, mut pps, mut frames, mut cur) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
                for (n, (_, s)) in starts.iter().enumerate() {
                    let nal = &b[*s..starts.get(n + 1).map(|x| x.0).unwrap_or(b.len())];
                    match nal[0] & 0x1f {
                        7 => sps = nal.to_vec(),
                        8 => pps = nal.to_vec(),
                        9 => {}
                        t => {
                            cur.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                            cur.extend_from_slice(nal);
                            if t == 1 || t == 5 {
                                frames.push((t == 5, std::mem::take(&mut cur)));
                            }
                        }
                    }
                }
                let mut avcc = vec![1, sps[1], sps[2], sps[3], 0xFF, 0xE1];
                avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
                avcc.extend_from_slice(&sps);
                avcc.push(1);
                avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
                avcc.extend_from_slice(&pps);
                (avcc, frames)
            }
            Err(_) => (vec![1, 0x64, 0, 0x28, 0xFF, 0xE1, 0, 2, 0x67, 1, 1, 0, 2, 0x68, 9], (0..4u8).map(|i| (i == 0, vec![0, 0, 0, 2, 0x65, i])).collect()),
        };

        let mut s = TcpStream::connect(addr).unwrap();
        let secret = transient(&mut s);
        let k = derive_control_keys(&secret);
        let mut chan = Some(ControlChannel::new(ControlKeys { read: k.write, write: k.read }));
        let shk = [0x5Au8; 32];
        let scid = |ty: u64| 0x0102_0304_0000u64 + ty;
        let streams = [103u64, 110, 96]
            .iter()
            .map(|ty| {
                let mut d = plist::Dictionary::new();
                d.insert("type".into(), V::Integer((*ty).into()));
                d.insert("streamConnectionID".into(), V::Integer(scid(*ty).into()));
                if *ty != 110 {
                    d.insert("audioFormat".into(), V::Integer(0x40_0000u64.into()));
                    d.insert("ct".into(), V::Integer(4u64.into()));
                    d.insert("spf".into(), V::Integer(1024u64.into()));
                }
                if *ty == 103 {
                    d.insert("shk".into(), V::Data(shk.to_vec()));
                }
                V::Dictionary(d)
            })
            .collect();
        let mut req = plist::Dictionary::new();
        req.insert("streams".into(), V::Array(streams));
        let head = "SETUP rtsp://x/1 RTSP/1.0\r\nCSeq: 5\r\nContent-Type: application/x-apple-binary-plist";
        let (st, resp) = rt(&mut s, &mut chan, head, &to_bplist(req));
        assert_eq!(st, 200);
        let resp = V::from_reader(std::io::Cursor::new(resp)).unwrap();
        let outs = resp.as_dictionary().unwrap().get("streams").unwrap().as_array().unwrap().clone();
        let port = |i: usize| outs[i].as_dictionary().unwrap().get("dataPort").unwrap().as_unsigned_integer().unwrap() as u16;
        let rtp = |i: usize| {
            let mut hdr = [0x80u8, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            hdr[2..4].copy_from_slice(&(i as u16).to_be_bytes());
            hdr[4..8].copy_from_slice(&(i as u32 * 1024).to_be_bytes());
            hdr
        };

        // 103: buffered audio over TCP under shk.
        let mut a = TcpStream::connect((addr.ip(), port(0))).unwrap();
        for (i, au) in aus.iter().enumerate() {
            let pkt = receiver::stream::encrypt_audio(&shk, &rtp(i), au, &(i as u64).to_le_bytes());
            a.write_all(&((pkt.len() + 2) as u16).to_be_bytes()).unwrap();
            a.write_all(&pkt).unwrap();
        }
        drop(a);

        // 110: screen over TCP under the derived output key; codec frame first, a report in between.
        let key = receiver::stream::derive_stream_keys(&secret, scid(110)).output;
        let cipher = chacha20poly1305::ChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(&key));
        let mut v = TcpStream::connect((addr.ip(), port(1))).unwrap();
        let send = |v: &mut TcpStream, op: u8, flags: u8, t: f64, body: &[u8], ctr: Option<u64>| {
            let mut hdr = [0u8; 128];
            hdr[4] = op;
            hdr[5] = flags;
            hdr[8..16].copy_from_slice(&((t * 4_294_967_296.0) as u64).to_le_bytes());
            let body = match ctr {
                Some(c) => {
                    hdr[..4].copy_from_slice(&(body.len() as u32 + 16).to_le_bytes());
                    let mut nonce = [0u8; 12];
                    nonce[4..].copy_from_slice(&c.to_le_bytes());
                    cipher.encrypt(chacha20poly1305::Nonce::from_slice(&nonce), Payload { msg: body, aad: &hdr }).unwrap()
                }
                None => {
                    hdr[..4].copy_from_slice(&(body.len() as u32).to_le_bytes());
                    body.to_vec()
                }
            };
            v.write_all(&hdr).unwrap();
            v.write_all(&body).unwrap();
        };
        send(&mut v, 1, 0, 100.0, &avcc, None);
        for (i, (idr, f)) in frames.iter().enumerate() {
            if i == 2 {
                send(&mut v, 5, 0, 100.0, &[0xEE; 80], None);
            }
            send(&mut v, 0, if *idr { 0x10 } else { 0 }, 100.0 + i as f64 / 30.0, f, Some(i as u64));
        }
        drop(v);

        // 96: realtime audio over UDP under the derived output key of ITS stream id, each packet 3 times.
        let key96 = receiver::stream::derive_stream_keys(&secret, scid(96)).output;
        let u = UdpSocket::bind("0.0.0.0:0").unwrap();
        for (i, au) in aus.iter().enumerate() {
            let pkt = receiver::stream::encrypt_audio(&key96, &rtp(i), au, &(i as u64).to_le_bytes());
            for _ in 0..3 {
                u.send_to(&pkt, (addr.ip(), port(2))).unwrap();
            }
            if i % 20 == 0 {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        // `LIVE_HOLD_SECS=N` keeps the session open (a /feedback every 2 s) so the probe's
        // 10-second TIMELINE and HEALTH lines can be seen.
        let hold: u64 = std::env::var("LIVE_HOLD_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        for _ in 0..hold / 2 {
            std::thread::sleep(Duration::from_secs(2));
            let _ = rt(&mut s, &mut chan, "POST /feedback RTSP/1.0\r\nCSeq: 6", b"");
        }
        let _ = rt(&mut s, &mut chan, "POST /feedback RTSP/1.0\r\nCSeq: 6", b"");
        let _ = rt(&mut s, &mut chan, "TEARDOWN rtsp://x/1 RTSP/1.0\r\nCSeq: 7", b"");
        std::thread::sleep(Duration::from_millis(800));
    }

    #[test]
    fn mask_presets_and_bit_edits() {
        assert_eq!(parse_mask("A").unwrap().0, 0x40_4444_0B80);
        assert_eq!(parse_mask("B").unwrap().0, 0x1_c340_445f_8a00);
        assert_eq!(parse_mask("A+19").unwrap().0, 0x40_444C_0B80);
        assert_eq!(parse_mask("B-26+7").unwrap().0, 0x1_c340_405f_8a80);
        assert_eq!(parse_mask("0x80,0x1+9").unwrap().0, 0x1_0000_0280);
        assert_eq!(parse_mask("M").unwrap().0, 1 << 7 | 1 << 9 | 1 << 30 | 1 << 38 | 1 << 48);
        assert!(parse_mask("A+64").is_none());
        assert!(parse_mask("Q").is_none());
        // fex is the little-endian feature bytes, base64 without padding.
        assert_eq!(b64_nopad(&0x1_c340_445f_8a00u64.to_le_bytes()), "AIpfREDDAQA");
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    use ed25519_dalek::Verifier as _;

    /// The advertised `pk` must be the key pair-verify M2 is signed with, and it must survive a
    /// restart of an unchanged variant while a changed variant gets a new one.
    #[test]
    fn advertised_pk_is_the_pair_verify_key_and_is_stable_per_spec() {
        let dir = std::env::temp_dir().join(format!("airplay-probe-idtest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = parse_spec("M/M", "t", 0, Some(&dir));
        let again = parse_spec("M/M", "t", 0, Some(&dir));
        let changed = parse_spec("M/M+59", "t", 0, Some(&dir));
        assert_eq!((a.pk, &a.pi, &a.device_id), (again.pk, &again.pi, &again.device_id));
        assert_ne!(a.pk, changed.pk);
        let txt = a.airplay_txt();
        let get = |k: &str| txt.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get("pk"), Some(hex(&a.pk, 32)));
        assert_eq!(get("pi"), Some(a.pi.clone()));
        assert_eq!(get("features").as_deref(), Some("0x40000280,0x10040"));

        // Drive pair-verify M1 -> M2 as a controller and check M2's signature against the TXT pk.
        let secret = x25519_dalek::StaticSecret::from([9u8; 32]);
        let our_pk = x25519_dalek::PublicKey::from(&secret).to_bytes();
        let mut srv = PairVerifyServer::new(a.identity);
        let m1 = tlv::encode(&[(tlv_type::STATE, &[1]), (tlv_type::PUBLIC_KEY, &our_pk)]);
        let (m2, _) = srv.exchange(&m1, &Peers::default()).unwrap();
        let m2 = tlv::decode(&m2).unwrap();
        let srv_pk: [u8; 32] = tlv_first(&m2, tlv_type::PUBLIC_KEY).unwrap().try_into().unwrap();
        let shared = secret.diffie_hellman(&x25519_dalek::PublicKey::from(srv_pk)).to_bytes();
        let key: [u8; 32] = pairing::crypto::hkdf_sha512(&shared, b"Pair-Verify-Encrypt-Salt", b"Pair-Verify-Encrypt-Info");
        let sub = pairing::crypto::chacha_decrypt(&key, b"PV-Msg02", tlv_first(&m2, tlv_type::ENCRYPTED_DATA).unwrap()).unwrap();
        let sub = tlv::decode(&sub).unwrap();
        assert_eq!(tlv_first(&sub, tlv_type::IDENTIFIER), Some(a.pi.as_bytes()), "identifier is the advertised pi");
        let mut signed = srv_pk.to_vec();
        signed.extend_from_slice(a.pi.as_bytes());
        signed.extend_from_slice(&our_pk);
        let sig = ed25519_dalek::Signature::from_slice(tlv_first(&sub, tlv_type::SIGNATURE).unwrap()).unwrap();
        ed25519_dalek::VerifyingKey::from_bytes(&a.pk).unwrap().verify(&signed, &sig).expect("M2 verifies under the advertised pk");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
