//! airplayd — the box-side receiver of AirPlay-only mode (docs/ocbm/02_AIRPLAY_ONLY_MODE.md).
//!
//! Advertises ONE plain AirPlay receiver (no CarPlay bit, no iAP2, no Bluetooth) with the measured
//! feature set, pairs the iPhone (transient pair-setup), answers MFi `auth-setup` with the on-board
//! coprocessor, serves the RTSP control channel, and TUNNELS the media: every stream's packets go
//! to ocbmd's seams still encrypted, with the stream's key in front, for the host app to decrypt
//! and decode (`tunnel.rs`). Started and stopped by the mode switch (`/script/airplay_mode.sh`,
//! driven by `CT_MODE_SELECT MODE_AIRPLAY`); it never runs beside carplayd, iap2d or btd.
//!
//! Limits, by construction:
//!   * MFi: `mfi::sap::MfiSapServer` + `mfi-i2c-local` (`/dev/i2c-1`, `/tmp/carplay_mfi.lock` taken per
//!     transaction). No crypto and no I2C sequence is written here; the private key stays in the chip.
//!   * No FairPlay: `fp-setup` is refused, always. No Apple key material exists in this crate.
//!   * No media decrypt on the box. The pairing secret and the stream keys are session material:
//!     the log carries their length and first 4 bytes, never more.
//!
//! usage: airplayd [--cfg FILE] [--name NAME] [--display WxH@FPS] [--hevc] [--port N] [--ident FILE] [--state FILE]
//!   --cfg FILE   `name=` / `display=` / `hevc=` lines (the file ocbmd writes for the mode; other keys ignored)
//!   --hevc       also advertise feature bit 42 (SupportsScreenMultiCodec; `AIRPLAY_FLAG_HEVC`)

mod tunnel;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceInfo};
use mfi::auth_client::MfiSigner;
use mfi::sap::MfiSapServer;
use ocbm_proto as p;
use pairing::crypto::{hkdf_sha512, tlv_error, tlv_type};
use pairing::srp::SrpServer;
use pairing::tlv;
use rtsp::control::{derive_control_keys, ControlChannel, ControlKeys};
use rtsp::message::{Request, Response};
use tunnel::{AudioFormat, Hold, Stream, Tunnel};

static STOP: AtomicBool = AtomicBool::new(false);
static T0: OnceLock<Instant> = OnceLock::new();

/// One line on stderr (the mode script appends it to /tmp/box.log, the box's universal log).
pub fn log_line(s: &str) {
    let t = T0.get_or_init(Instant::now).elapsed();
    eprintln!("[airplayd] {:>5}.{:03} {s}", t.as_secs(), t.subsec_millis());
}

#[macro_export]
macro_rules! lg {
    ($($a:tt)*) => { $crate::log_line(&format!($($a)*)) };
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// How key material may appear in the log: its length and first 4 bytes.
fn redact(b: &[u8]) -> String {
    format!("{} B {}..", b.len(), hex(&b[..b.len().min(4)]))
}

fn urandom(buf: &mut [u8]) {
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(buf)).expect("/dev/urandom");
}

// ---------------------------------------------------------------- configuration

/// The feature set measured to carry both Music audio and screen mirroring (docs/ocbm/02 §4.1,
/// variant B7): bits 7, 9, 11, 15-20, 22, 26, 30, 38, 40, 41, 46, 47, 48. TXT `features=0x445F8A80,0x1C340`.
const FEATURES: u64 = 0x0001_C340_445F_8A80;
/// SupportsScreenMultiCodec. Measured on a real iPhone: with it AND a display larger than 1080p the phone
/// mirrors in HEVC (opcode 1 = an `hvc1` sample entry carrying `hvcC`); without either it stays H.264.
/// The tunnel forwards opcode 0 and 1 unchanged, so nothing else here depends on the codec.
const FEATURE_SCREEN_MULTI_CODEC: u64 = 1 << 42;
const SRCVERS: &str = "366.0";
const PROTOVERS: &str = "1.1";
const STATUS_FLAGS: u32 = 0x4;
const MODEL: &str = "OCBM-AirPlay";
/// The fixed code of transient pairing (the phone never asks the user for one).
const TRANSIENT_PIN: &[u8] = b"3939";

struct Cfg {
    name: String,
    display: (u16, u16, u8),
    /// The advertised feature bits: `FEATURES`, plus bit 42 when the mode asked for HEVC.
    features: u64,
    device_id: String,
    pi: String,
    pk: [u8; 32],
}

impl Cfg {
    fn new(name: String, display: (u16, u16, u8), hevc: bool, material: &[u8; 54]) -> Cfg {
        let r = &material[32..];
        let seed: [u8; 32] = material[..32].try_into().unwrap();
        Cfg {
            name,
            display,
            features: if hevc { FEATURES | FEATURE_SCREEN_MULTI_CODEC } else { FEATURES },
            // Locally-administered unicast MAC: cannot collide with a real device.
            device_id: format!("{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}", (r[0] | 0x02) & 0xFE, r[1], r[2], r[3], r[4], r[5]),
            pi: format!("{}-{}-{}-{}-{}", hex(&r[6..10]), hex(&r[10..12]), hex(&r[12..14]), hex(&r[14..16]), hex(&r[16..22])),
            // `pk` is advertised and unused in this flow: transient pairing is SRP only (§4.3).
            pk: pairing::verify::Identity::new(Vec::new(), seed).public_key(),
        }
    }

    fn features_txt(&self) -> String {
        format!("0x{:X},0x{:X}", self.features & 0xFFFF_FFFF, self.features >> 32)
    }

    fn airplay_txt(&self) -> Vec<(String, String)> {
        [
            ("deviceid", self.device_id.clone()),
            ("features", self.features_txt()),
            ("flags", format!("0x{STATUS_FLAGS:X}")),
            ("model", MODEL.into()),
            ("protovers", PROTOVERS.into()),
            ("pi", self.pi.clone()),
            ("srcvers", SRCVERS.into()),
            ("pk", hex(&self.pk)),
            ("vv", "2".into()),
            ("acl", "0".into()),
            ("rsf", "0x0".into()),
            ("gid", self.pi.clone()),
            ("psi", self.pi.clone()),
            ("gcgl", "0".into()),
            ("igl", "0".into()),
            ("fex", b64_nopad(&self.features.to_le_bytes())),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    }
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

/// The receiver's identity (32-byte Ed25519 seed + 22 bytes for device id and `pi`), read back from or
/// created in `path`. Stable across restarts on purpose: iOS keys its cache of a receiver's features
/// on `pi`/`deviceid`, so a new identity per start would leave stale twins in the phone's list.
fn identity_material(path: Option<&std::path::Path>) -> [u8; 54] {
    let mut m = [0u8; 54];
    if let Some(path) = path {
        if let Ok(b) = std::fs::read(path) {
            if b.len() == m.len() {
                m.copy_from_slice(&b);
                return m;
            }
        }
        urandom(&mut m);
        use std::os::unix::fs::OpenOptionsExt;
        let wrote = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path).and_then(|mut f| f.write_all(&m));
        match wrote {
            Ok(()) => lg!("identity created in {}", path.display()),
            Err(e) => lg!("identity NOT kept ({}: {e}); it is new at every start", path.display()),
        }
    } else {
        urandom(&mut m);
    }
    m
}

/// `WxH[@FPS]` (FPS absent = 30), within the `AIRPLAY_REQ_DISPLAY` ranges.
fn parse_display(v: &str) -> Option<(u16, u16, u8)> {
    let (wh, fps) = v.split_once('@').unwrap_or((v, "30"));
    let (w, h) = wh.split_once('x')?;
    let d = (w.trim().parse().ok()?, h.trim().parse().ok()?, fps.trim().parse().ok()?);
    p::airplay_display_ok(d).then_some(d)
}

// ---------------------------------------------------------------- MFi (the chip, through the lock)

/// `MfiSigner` backed by the on-board coprocessor. `try_cert`/`try_sign` take and release
/// `/tmp/carplay_mfi.lock` per transaction, so ocbmd's `CH_MFI` handler can share the chip.
struct ChipSigner;

static CERT: OnceLock<Vec<u8>> = OnceLock::new();

/// A chip NAK (asleep or busy) is retried with a growing pause; a LOCK failure is not, because waiting
/// out the same holder again cannot help (`mfi-i2c-local`'s contract for its callers).
fn chip_retry<T>(what: &str, mut op: impl FnMut() -> Result<T, mfi_i2c_local::MfiError>) -> Result<T, mfi_i2c_local::MfiError> {
    let mut last = mfi_i2c_local::MfiError::Chip;
    for attempt in 1..=5u32 {
        match op() {
            Ok(v) => {
                if attempt > 1 {
                    lg!("MFi {what}: chip answered on attempt {attempt}/5");
                }
                return Ok(v);
            }
            Err(mfi_i2c_local::MfiError::Chip) => std::thread::sleep(Duration::from_millis(20 << attempt)),
            Err(e) => {
                last = e;
                break;
            }
        }
    }
    lg!("MFi {what}: FAILED ({last:?})");
    Err(last)
}

impl MfiSigner for ChipSigner {
    fn copy_certificate(&mut self) -> std::io::Result<Vec<u8>> {
        // The certificate is public and constant: one chip transaction per process, not per phone.
        if let Some(c) = CERT.get() {
            return Ok(c.clone());
        }
        let c = chip_retry("certificate", mfi_i2c_local::try_cert).map_err(|e| std::io::Error::other(format!("MFi chip cert: {e:?}")))?;
        Ok(CERT.get_or_init(|| c).clone())
    }
    fn create_signature(&mut self, digest: &[u8]) -> std::io::Result<Vec<u8>> {
        chip_retry("signature", || mfi_i2c_local::try_sign(digest)).map_err(|e| std::io::Error::other(format!("MFi chip sign: {e:?}")))
    }
}

/// What answers `/auth-setup`: the chip on the box, a stand-in in tests.
type SignerFn = fn() -> Box<dyn MfiSigner + Send>;

struct BoxedSigner(Box<dyn MfiSigner + Send>);
impl MfiSigner for BoxedSigner {
    fn copy_certificate(&mut self) -> std::io::Result<Vec<u8>> {
        self.0.copy_certificate()
    }
    fn create_signature(&mut self, digest: &[u8]) -> std::io::Result<Vec<u8>> {
        self.0.create_signature(digest)
    }
}

// ---------------------------------------------------------------- /info

const OCTET: &str = "application/octet-stream";
const BPLIST: &str = "application/x-apple-binary-plist";

fn to_bplist(d: plist::Dictionary) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = plist::Value::Dictionary(d).to_writer_binary(&mut out);
    out
}

fn parse_plist(body: &[u8]) -> Option<plist::Value> {
    if !body.starts_with(b"bplist00") {
        return None;
    }
    plist::Value::from_reader(std::io::Cursor::new(body)).ok()
}

fn build_info(c: &Cfg, with_txt: bool) -> Vec<u8> {
    use plist::Value as V;
    let mut d = plist::Dictionary::new();
    d.insert("deviceID".into(), V::String(c.device_id.clone()));
    d.insert("macAddress".into(), V::String(c.device_id.clone()));
    d.insert("features".into(), V::Integer(c.features.into()));
    d.insert("statusFlags".into(), V::Integer(u64::from(STATUS_FLAGS).into()));
    d.insert("model".into(), V::String(MODEL.into()));
    d.insert("manufacturer".into(), V::String("OCBM".into()));
    d.insert("name".into(), V::String(c.name.clone()));
    d.insert("pi".into(), V::String(c.pi.clone()));
    d.insert("pk".into(), V::Data(c.pk.to_vec()));
    d.insert("psi".into(), V::String(c.pi.clone()));
    d.insert("sourceVersion".into(), V::String(SRCVERS.into()));
    d.insert("protocolVersion".into(), V::String(PROTOVERS.into()));
    d.insert("keepAliveLowPower".into(), V::Boolean(true));
    d.insert("keepAliveSendStatsAsBody".into(), V::Boolean(true));
    d.insert("vv".into(), V::Integer(2u64.into()));
    let mut l = plist::Dictionary::new();
    l.insert("audioType".into(), V::String("default".into()));
    l.insert("inputLatencyMicros".into(), V::Integer(0u64.into()));
    l.insert("outputLatencyMicros".into(), V::Integer(400_000u64.into()));
    d.insert("audioLatencies".into(), V::Array(vec![V::Dictionary(l)]));
    // The display the phone renders for: the host's panel, from the mode request. The key set is the
    // airplay-probe's (verified on a real iPhone up to 3840x2160 at maxFPS 120): the sender reads
    // widthPixels/heightPixels and maxFPS; width/height are the legacy pair the macOS receiver also sends.
    let (w, h, fps) = c.display;
    let mut s = plist::Dictionary::new();
    s.insert("uuid".into(), V::String(c.pi.clone()));
    for k in ["widthPixels", "width"] {
        s.insert(k.into(), V::Integer(u64::from(w).into()));
    }
    for k in ["heightPixels", "height"] {
        s.insert(k.into(), V::Integer(u64::from(h).into()));
    }
    s.insert("widthPhysical".into(), V::Integer(0u64.into()));
    s.insert("heightPhysical".into(), V::Integer(0u64.into()));
    s.insert("features".into(), V::Integer(14u64.into()));
    s.insert("maxFPS".into(), V::Integer(u64::from(fps).into()));
    // A legacy period the current sender does not read: the measured default keeps its 1/60, any other
    // display states 1/maxFPS (as the probe's `display=` variants do).
    let period = if c.display == p::AIRPLAY_DEFAULT_DISPLAY { 1.0 / 60.0 } else { 1.0 / f64::from(fps) };
    s.insert("refreshRate".into(), V::Real(period));
    s.insert("overscanned".into(), V::Boolean(false));
    s.insert("rotation".into(), V::Boolean(false));
    d.insert("displays".into(), V::Array(vec![V::Dictionary(s)]));
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

// ---------------------------------------------------------------- one control connection

struct Ctx {
    cfg: Cfg,
    tunnel: Arc<Tunnel>,
    signer: SignerFn,
}

struct Conn {
    id: u32,
    ctx: Arc<Ctx>,
    peer: String,
    local_ip: String,
    transient: Option<SrpServer>,
    sap: MfiSapServer,
    /// Keys to switch to AFTER the current response has gone out in the clear.
    pending: Option<ControlKeys>,
    /// The pairing secret (transient pair-setup: the 64-byte SRP session key). Session material.
    secret: Option<Vec<u8>>,
    authed: bool,
    /// Set by the session SETUP; stops the event listener when the connection ends.
    session: Option<Arc<AtomicBool>>,
    streams: Vec<Stream>,
    holds: Vec<Hold>,
    teardown: bool,
}

fn scid_of(sd: &plist::Dictionary) -> Option<u64> {
    // A plist integer may arrive negative: the key salt uses the unsigned 64-bit value (§4.5).
    match sd.get("streamConnectionID") {
        Some(plist::Value::Integer(i)) => Some(i.as_unsigned().unwrap_or_else(|| i.as_signed().unwrap_or(0) as u64)),
        _ => None,
    }
}

/// HKDF-SHA512(pairing secret, "DataStream-Salt" + scid, "DataStream-Output-Encryption-Key"): the key
/// of a type 110 or type 96 stream (§4.6). Derived here so the pairing secret never leaves the box.
fn stream_key(secret: &[u8], scid: u64) -> [u8; 32] {
    hkdf_sha512::<32>(secret, format!("DataStream-Salt{scid}").as_bytes(), b"DataStream-Output-Encryption-Key")
}

/// SETUP `ct` (and the type 103 `audioFormat` bit) -> the `SEAM_FORMAT` description.
fn audio_format(sd: &plist::Dictionary) -> AudioFormat {
    let int = |k: &str| sd.get(k).and_then(|v| v.as_unsigned_integer());
    let codec = match int("ct") {
        Some(1) => p::SEAM_CODEC_PCM,
        Some(4) => p::SEAM_CODEC_AAC_LC,
        Some(8) => p::SEAM_CODEC_AAC_ELD,
        _ => p::SEAM_CODEC_UNKNOWN,
    };
    // Every stream measured is 44.1 kHz stereo (audioFormat 0x400000); type 103 names no `sr`.
    AudioFormat { codec, rate: int("sr").unwrap_or(44100) as u32, ch: 2 }
}

impl Conn {
    fn tunnel(&self) -> &Arc<Tunnel> {
        &self.ctx.tunnel
    }

    fn auth_setup(&mut self, req: &Request) -> Response {
        let mut signer = BoxedSigner((self.ctx.signer)());
        match self.sap.exchange(&req.body, &mut signer) {
            Ok(m2) => {
                self.authed = true;
                lg!("#{} auth-setup: answered by the MFi coprocessor ({} B reply)", self.id, m2.len());
                let mut r = Response::ok(req, Some(OCTET), m2);
                if let Some(et) = req.header("X-Apple-ET") {
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

    /// Transient pair-setup only (M1 with Flags bit 4 -> M2, M3 -> M4): SRP-6a through
    /// `pairing::srp::SrpServer`, no Ed25519 exchange, nothing stored. Anything else is refused.
    fn pair_setup(&mut self, req: &Request) -> Response {
        let items = tlv::decode(&req.body).unwrap_or_default();
        let first = |t: u8| items.iter().find(|(k, _)| *k == t).map(|(_, v)| v.as_slice());
        let state = first(tlv_type::STATE).and_then(|v| v.first().copied()).unwrap_or(0);
        let err = |code: u8| tlv::encode(&[(tlv_type::STATE, &[state.wrapping_add(1)]), (tlv_type::ERROR, &[code])]);
        match state {
            1 => {
                self.transient = None;
                let flags = first(tlv_type::FLAGS).map(|v| v.iter().rev().fold(0u64, |a, b| a << 8 | u64::from(*b)));
                if !flags.is_some_and(|f| f & 0x10 != 0) {
                    lg!("#{} pair-setup M1 is not transient (flags {flags:?}): refused", self.id);
                    return Response::ok(req, Some(OCTET), err(tlv_error::AUTHENTICATION));
                }
                let mut salt = [0u8; 16];
                urandom(&mut salt);
                let Some(srv) = SrpServer::new(b"Pair-Setup", TRANSIENT_PIN, &salt) else {
                    return Response::status(req, 500, "Internal Server Error");
                };
                let out = tlv::encode(&[(tlv_type::STATE, &[2]), (tlv_type::SALT, &salt), (tlv_type::PUBLIC_KEY, &srv.b_pub())]);
                self.transient = Some(srv);
                Response::ok(req, Some(OCTET), out)
            }
            3 => {
                let Some(mut srv) = self.transient.take() else {
                    return Response::ok(req, Some(OCTET), err(tlv_error::AUTHENTICATION));
                };
                let out = match srv.verify(first(tlv_type::PUBLIC_KEY).unwrap_or(&[]), first(tlv_type::PROOF).unwrap_or(&[])) {
                    Some(m2) => {
                        let key = srv.session_key().map(<[u8]>::to_vec).unwrap_or_default();
                        lg!("#{} transient pair-setup done; pairing secret {}", self.id, redact(&key));
                        self.pending = Some(derive_control_keys(&key));
                        self.secret = Some(key);
                        tlv::encode(&[(tlv_type::STATE, &[4]), (tlv_type::PROOF, &m2)])
                    }
                    None => {
                        lg!("#{} transient pair-setup: SRP proof mismatch", self.id);
                        err(tlv_error::AUTHENTICATION)
                    }
                };
                Response::ok(req, Some(OCTET), out)
            }
            _ => Response::ok(req, Some(OCTET), err(tlv_error::AUTHENTICATION)),
        }
    }

    fn setup(&mut self, req: &Request, plist: Option<&plist::Value>) -> Response {
        use plist::Value as V;
        let (Some(d), Some(secret)) = (plist.and_then(|v| v.as_dictionary()), self.secret.clone()) else {
            // No plist = a legacy RAOP SETUP; no secret = SETUP before pairing. Neither is this receiver.
            lg!("#{} SETUP refused (plist: {}, paired: {})", self.id, plist.is_some(), self.secret.is_some());
            return Response::status(req, 400, "Bad Request");
        };
        let t = self.tunnel().clone();
        let mut out = plist::Dictionary::new();
        let Some(streams) = d.get("streams").and_then(|s| s.as_array()) else {
            // Phase 1: the session. Event port (held open, §4.6) and timing (never answered, §4.7).
            {
                // A second session SETUP on one connection replaces the first one's event listener
                // and is still ONE session.
                let again = self.session.take().map(|old| old.store(true, Ordering::SeqCst)).is_some();
                let stop = Arc::new(AtomicBool::new(false));
                self.session = Some(stop.clone());
                let ptp = d.get("timingProtocol").and_then(|v| v.as_string()) == Some("PTP");
                let ep = tunnel::start_event(stop).unwrap_or(0);
                out.insert("eventPort".into(), V::Integer(u64::from(ep).into()));
                if ptp {
                    out.insert("timingPort".into(), V::Integer(0u64.into()));
                    let mut tpi = plist::Dictionary::new();
                    tpi.insert("Addresses".into(), V::Array(vec![V::String(self.local_ip.clone())]));
                    tpi.insert("ID".into(), V::String(self.local_ip.clone()));
                    out.insert("timingPeerInfo".into(), V::Dictionary(tpi));
                } else {
                    let tp = tunnel::hold_udp().map(|(h, port)| {
                        self.holds.push(h);
                        port
                    });
                    out.insert("timingPort".into(), V::Integer(u64::from(tp.unwrap_or(0)).into()));
                }
                let s = |k: &str| d.get(k).and_then(|v| v.as_string()).unwrap_or("");
                lg!(
                    "#{} session SETUP from '{}' ({} {} {}), timing {}, MFi authenticated: {}",
                    self.id, s("name"), s("model"), s("osName"), s("osVersion"), if ptp { "PTP" } else { "NTP" }, self.authed
                );
                if !again {
                    t.session_opened();
                }
                let mirroring = d.get("isScreenMirroringSession").and_then(|v| v.as_boolean()).unwrap_or(false);
                t.event(
                    "sender",
                    &format!(
                        "\"name\":\"{}\",\"model\":\"{}\",\"osName\":\"{}\",\"osVersion\":\"{}\",\"deviceID\":\"{}\",\"timing\":\"{}\",\"mirroring\":{mirroring},\"address\":\"{}\"",
                        tunnel::jesc(s("name")), tunnel::jesc(s("model")), tunnel::jesc(s("osName")), tunnel::jesc(s("osVersion")),
                        tunnel::jesc(s("deviceID")), if ptp { "PTP" } else { "NTP" }, tunnel::jesc(&self.peer)
                    ),
                );
            }
            return Response::ok(req, Some(BPLIST), to_bplist(out));
        };
        let mut outs = Vec::new();
        for s in streams {
            let Some(sd) = s.as_dictionary() else { continue };
            let ty = sd.get("type").and_then(|v| v.as_unsigned_integer()).unwrap_or(0);
            let scid = scid_of(sd);
            let mut o = plist::Dictionary::new();
            o.insert("type".into(), V::Integer(ty.into()));
            let mut port = |k: &str, v: u16| {
                o.insert(k.into(), V::Integer(u64::from(v).into()));
            };
            // A fresh key at every (re)SETUP: a new streamConnectionID is a new stream (§4.6).
            let started = match (ty, scid) {
                (110, Some(scid)) => {
                    let key = stream_key(&secret, scid);
                    tunnel::start_screen(&t, scid, key).map(|(st, dp)| {
                        port("dataPort", dp);
                        (st, "video", key)
                    })
                }
                (96, Some(scid)) => {
                    let key = stream_key(&secret, scid);
                    tunnel::start_realtime(&t, scid, key, audio_format(sd)).map(|(st, dp, cp)| {
                        port("dataPort", dp);
                        port("controlPort", cp);
                        (st, "audio", key)
                    })
                }
                (103, Some(scid)) => match sd.get("shk") {
                    // Buffered audio is keyed by `shk` from this SETUP, not by the pairing secret (§4.6).
                    Some(V::Data(shk)) if shk.len() == 32 => {
                        let key: [u8; 32] = shk.as_slice().try_into().unwrap();
                        tunnel::start_buffered(&t, scid, key, audio_format(sd)).map(|(st, dp, cp)| {
                            port("dataPort", dp);
                            port("controlPort", cp);
                            (st, "audio", key)
                        })
                    }
                    _ => {
                        lg!("#{} SETUP type 103 without a 32-byte shk: not tunnelled", self.id);
                        None
                    }
                },
                _ => None,
            };
            match started {
                Some((st, lane, key)) => {
                    if ty == 103 {
                        // The reply that works (§4.5): without controlPort and this the phone tears the
                        // stream down within 10 ms.
                        o.insert("audioBufferSize".into(), V::Integer((8u64 * 1024 * 1024).into()));
                    }
                    lg!("#{} SETUP stream type {ty} scid {} -> {lane} lane, key {}", self.id, st.scid, redact(&key));
                    t.event(
                        "stream",
                        &format!("\"state\":\"start\",\"lane\":\"{lane}\",\"scid\":{},\"setup\":{{{}}}", st.scid, tunnel::plist_scalars(sd)),
                    );
                    self.streams.push(st);
                    t.publish(None);
                }
                None => {
                    // Not a stream this receiver tunnels (only 110, 96 and 103 were ever requested).
                    // Give it ports so the reply is well-formed; nothing reads them.
                    lg!("#{} SETUP stream type {ty}: NOT tunnelled (ports held, nothing read)", self.id);
                    let h = if ty == 130 { tunnel::hold_tcp() } else { tunnel::hold_udp() };
                    if let Some((h, dp)) = h {
                        o.insert("dataPort".into(), V::Integer(u64::from(dp).into()));
                        self.holds.push(h);
                    }
                    t.event("stream", &format!("\"state\":\"unsupported\",\"setup\":{{{}}}", tunnel::plist_scalars(sd)));
                }
            }
            outs.push(V::Dictionary(o));
        }
        out.insert("streams".into(), V::Array(outs));
        Response::ok(req, Some(BPLIST), to_bplist(out))
    }

    fn teardown_req(&mut self, plist: Option<&plist::Value>) {
        let t = self.tunnel().clone();
        let named = plist.and_then(|v| v.as_dictionary()).and_then(|d| d.get("streams")).and_then(|s| s.as_array());
        match named {
            Some(list) => {
                for s in list.iter().filter_map(|s| s.as_dictionary()) {
                    let ty = s.get("type").and_then(|v| v.as_unsigned_integer()).unwrap_or(0);
                    // A stream TEARDOWN names the type, not the connection id: stop every live one of it.
                    for st in self.streams.iter_mut().filter(|st| st.ty == ty) {
                        st.stop(&t, "teardown");
                    }
                }
            }
            None => {
                self.teardown = true;
                for st in self.streams.iter_mut() {
                    st.stop(&t, "session teardown");
                }
            }
        }
    }

    fn set_parameter(&mut self, req: &Request) {
        let t = self.tunnel();
        match req.header("Content-Type").unwrap_or("") {
            "text/parameters" => {
                for line in String::from_utf8_lossy(&req.body).lines() {
                    let Some((k, v)) = line.split_once(':') else { continue };
                    let v = v.trim();
                    match k.trim() {
                        "volume" => {
                            if let Ok(db) = v.parse::<f64>() {
                                // dB: 0 = full, -30 = slider 0, -144 = mute (§4.8)
                                t.event("volume", &format!("\"db\":{db}"));
                            }
                        }
                        "progress" => {
                            let n: Vec<u64> = v.split('/').filter_map(|x| x.trim().parse().ok()).collect();
                            if n.len() == 3 {
                                t.event("progress", &format!("\"start\":{},\"current\":{},\"end\":{}", n[0], n[1], n[2]));
                            }
                        }
                        _ => {}
                    }
                }
            }
            "image/jpeg" => {
                t.artwork(&req.body);
                t.event("artwork", &format!("\"bytes\":{}", req.body.len()));
            }
            "image/none" => t.event("artwork", "\"bytes\":0"),
            "application/x-dmap-tagged" => {
                let f = tunnel::dmap_track(&req.body);
                if !f.is_empty() {
                    t.event("track", &f);
                }
            }
            _ => {}
        }
    }

    fn handle(&mut self, req: &Request) -> Response {
        let path = req.path().to_string();
        // FairPlay is refused unconditionally: never replayed, never answered (owner scope).
        if path.contains("fp-setup") {
            lg!("#{} fp-setup (FairPlay): refused by design", self.id);
            return Response::status(req, 403, "Forbidden");
        }
        if path.contains("auth-setup") {
            return self.auth_setup(req);
        }
        let plist = parse_plist(&req.body);
        let ok = |r: &Request| Response::ok(r, None, Vec::new());
        match (req.method.as_str(), path.as_str()) {
            ("GET", "/info") => {
                let want_txt = plist
                    .as_ref()
                    .and_then(|v| v.as_dictionary())
                    .and_then(|d| d.get("qualifier"))
                    .and_then(|q| q.as_array())
                    .is_some_and(|a| a.iter().any(|v| v.as_string() == Some("txtAirPlay")));
                Response::ok(req, Some(BPLIST), build_info(&self.ctx.cfg, want_txt))
            }
            ("POST", "/pair-setup") => self.pair_setup(req),
            // Transient pairing stores no controller, so there is nothing to verify against.
            ("POST", "/pair-verify") => Response::status(req, 470, "Connection Authorization Required"),
            ("POST", "/feedback" | "/command" | "/audioMode") => ok(req),
            ("POST", _) => {
                lg!("#{} unknown POST {path} -> empty 200", self.id);
                ok(req)
            }
            ("GET", _) => Response::status(req, 404, "Not Found"),
            ("OPTIONS", _) => {
                let mut r = ok(req);
                r.headers.push((
                    "Public".into(),
                    // (the list the measured receiver answered with, unchanged)
                    "ANNOUNCE, SETUP, RECORD, PAUSE, FLUSH, FLUSHBUFFERED, TEARDOWN, OPTIONS, POST, GET, PUT, \
                     GET_PARAMETER, SET_PARAMETER, SETPEERS, SETRATEANCHORTIME"
                        .into(),
                ));
                r
            }
            ("SETUP", _) => self.setup(req, plist.as_ref()),
            ("RECORD", _) => {
                let mut r = ok(req);
                r.headers.push(("Audio-Latency".into(), "0".into()));
                r
            }
            ("GET_PARAMETER", _) => {
                if String::from_utf8_lossy(&req.body).trim() == "volume" {
                    Response::ok(req, Some("text/parameters"), b"volume: -20.000000\r\n".to_vec())
                } else {
                    ok(req)
                }
            }
            ("SET_PARAMETER", _) => {
                self.set_parameter(req);
                ok(req)
            }
            // Buffered-audio playout facts the host's player needs: the anchor (rate, RTP time against
            // the phone's network clock) and the flush range.
            ("SETRATEANCHORTIME", _) | ("FLUSHBUFFERED", _) => {
                if let Some(d) = plist.as_ref().and_then(|v| v.as_dictionary()) {
                    let ev = if req.method == "FLUSHBUFFERED" { "flush" } else { "anchor" };
                    self.tunnel().event(ev, &tunnel::plist_scalars(d));
                }
                ok(req)
            }
            ("TEARDOWN", _) => {
                self.teardown_req(plist.as_ref());
                ok(req)
            }
            _ => ok(req),
        }
    }

    /// The connection is over: stop what it set up and publish how the session ended.
    fn finish(&mut self, why: &str) {
        let t = self.tunnel().clone();
        for st in self.streams.iter_mut() {
            st.stop(&t, "session end");
        }
        self.holds.clear();
        if let Some(stop) = self.session.take() {
            stop.store(true, Ordering::SeqCst);
            let end = match (self.teardown, why) {
                (true, _) => p::AIRPLAY_END_TEARDOWN,
                (false, "peer closed") => p::AIRPLAY_END_PEER_CLOSED,
                _ => p::AIRPLAY_END_ERROR,
            };
            t.session_closed(end);
        }
    }
}

fn serve(mut s: TcpStream, id: u32, ctx: Arc<Ctx>) {
    let peer = s.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    let _ = s.set_read_timeout(None); // never ended from this side: only the peer, or keepalive
    let _ = s.set_nodelay(true);
    tunnel::keepalive(&s);
    let mut c = Conn {
        id,
        ctx,
        peer: peer.clone(),
        local_ip: s.local_addr().map(|a| a.ip().to_string()).unwrap_or_default(),
        transient: None,
        sap: MfiSapServer::new(),
        pending: None,
        secret: None,
        authed: false,
        session: None,
        streams: Vec::new(),
        holds: Vec::new(),
        teardown: false,
    };
    let mut chan: Option<ControlChannel> = None;
    // After key activation, the first inbound bytes decide whether the PEER actually switched.
    let mut decided = true;
    let (mut raw, mut plain) = (Vec::<u8>::new(), Vec::<u8>::new());
    let mut n_req = 0u32;
    let mut buf = [0u8; 8192];
    let why = 'conn: loop {
        if chan.is_some() && !decided && raw.len() >= 2 {
            // An encrypted frame starts with a u16 LE length <= 16384; two uppercase ASCII letters read
            // as >= 16705, so they can only be a plaintext method token.
            if raw[0].is_ascii_uppercase() && raw[1].is_ascii_uppercase() {
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
                    Err(_) => break 'conn "control decrypt failure",
                }
            },
            Some(_) => {}
            None => plain.append(&mut raw),
        }
        match Request::parse(&plain) {
            Ok(Some((req, used))) => {
                plain.drain(..used);
                n_req += 1;
                if req.path() != "/feedback" {
                    lg!("#{id} {} {} ({} B){}", req.method, req.path(), req.body.len(), if chan.is_some() { "" } else { " [plain]" });
                }
                let mut resp = c.handle(&req);
                resp.headers.push(("Server".into(), format!("AirTunes/{SRCVERS}")));
                let wire = resp.serialize();
                let wire = match chan.as_mut() {
                    Some(ch) => ch.encrypt_frame(&wire),
                    None => wire,
                };
                if s.write_all(&wire).is_err() {
                    break "write failure";
                }
                if let Some(keys) = c.pending.take() {
                    chan = Some(ControlChannel::new(keys));
                    decided = false;
                    // Anything buffered behind this request arrived after the peer's own switch point.
                    plain.append(&mut raw);
                    std::mem::swap(&mut raw, &mut plain);
                }
                continue;
            }
            Ok(None) => {}
            Err(_) => break "unparseable request",
        }
        if plain.len() > 512 * 1024 || raw.len() > 512 * 1024 {
            break "oversized request";
        }
        match s.read(&mut buf) {
            Ok(0) => break "peer closed",
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break "read error",
        }
    };
    let had_session = c.session.is_some();
    c.finish(why);
    if had_session || n_req > 2 {
        lg!("#{id} {peer} closed ({why}) after {n_req} requests; teardown seen: {}", c.teardown);
    }
}

// ---------------------------------------------------------------- main

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

/// PTP is follow-only for a receiver and this one does not follow yet (§4.7): the ports are bound so
/// the phone's Sync/Announce do not draw ICMP port-unreachable, and everything is discarded.
fn ptp_discard(port: u16) {
    match UdpSocket::bind(("0.0.0.0", port)) {
        Ok(s) => {
            std::thread::spawn(move || {
                let mut b = [0u8; 1500];
                while s.recv_from(&mut b).is_ok() {}
            });
        }
        Err(e) => lg!("PTP udp:{port} not bound ({e})"),
    }
}

fn accept_loop(l: TcpListener, ctx: Arc<Ctx>) {
    static CONN: AtomicU32 = AtomicU32::new(0);
    std::thread::spawn(move || {
        for s in l.incoming() {
            match s {
                Ok(s) => {
                    let (ctx, n) = (ctx.clone(), CONN.fetch_add(1, Ordering::Relaxed) + 1);
                    std::thread::spawn(move || serve(s, n, ctx));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    });
}

fn main() {
    let _ = T0.set(Instant::now());
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (mut name, mut display, mut port, mut hevc) = ("OCBM AirPlay".to_string(), p::AIRPLAY_DEFAULT_DISPLAY, 7000u16, false);
    let (mut ident, mut state) = ("/etc/airplay_ident".to_string(), "/tmp/airplay_state".to_string());
    let usage = || -> ! {
        eprintln!("usage: airplayd [--cfg FILE] [--name NAME] [--display WxH@FPS] [--hevc] [--port N] [--ident FILE] [--state FILE]");
        std::process::exit(64)
    };
    let mut i = 0;
    while i < a.len() {
        if a[i] == "--hevc" {
            hevc = true;
            i += 1;
            continue;
        }
        let v = a.get(i + 1).cloned().unwrap_or_else(|| usage());
        match a[i].as_str() {
            "--cfg" => {
                for line in std::fs::read_to_string(&v).unwrap_or_default().lines() {
                    match line.split_once('=') {
                        Some(("name", n)) if !n.is_empty() => name = n.to_string(),
                        Some(("display", d)) => display = parse_display(d).unwrap_or(display),
                        Some(("hevc", h)) => hevc = h.trim() == "1",
                        _ => {}
                    }
                }
            }
            "--name" => name = v,
            "--display" => display = parse_display(&v).unwrap_or_else(|| usage()),
            "--port" => port = v.parse().unwrap_or_else(|_| usage()),
            "--ident" => ident = v,
            "--state" => state = v,
            _ => usage(),
        }
        i += 2;
    }
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    let cfg = Cfg::new(name, display, hevc, &identity_material(Some(std::path::Path::new(&ident))));
    lg!(
        "start: name '{}' display {}x{}@{} hevc {} features {} deviceid {} pid {}",
        cfg.name, display.0, display.1, display.2, hevc, cfg.features_txt(), cfg.device_id, std::process::id()
    );
    // The chip, before the phone can ask: a NAK at start is common (§4.3) and retried here.
    match ChipSigner.copy_certificate() {
        Ok(c) => lg!("MFi self-test: coprocessor answered, certificate {} B", c.len()),
        Err(e) => lg!("MFi self-test FAILED ({e}): auth-setup is retried per request and answers 500 until the chip does"),
    }
    let tunnel = Arc::new(Tunnel::new("127.0.0.1:9001", "127.0.0.1:9002", "127.0.0.1:9004", Some(state.clone().into())));
    let listener = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port))).unwrap_or_else(|e| {
        lg!("FATAL: cannot bind :{port}: {e}");
        std::process::exit(1)
    });
    ptp_discard(319);
    ptp_discard(320);
    let mdns = ServiceDaemon::new().unwrap_or_else(|e| {
        lg!("FATAL: mDNS daemon: {e}");
        std::process::exit(1)
    });
    // Advertised only once the port is listening. addr-auto publishes each interface's own address.
    let host = format!("ocbm-airplay-{}.local.", cfg.device_id.replace(':', "").to_lowercase());
    let props: HashMap<String, String> = cfg.airplay_txt().into_iter().collect();
    let full = ServiceInfo::new("_airplay._tcp.local.", &cfg.name, &host, "", port, props)
        .map(|i| i.enable_addr_auto())
        .and_then(|i| {
            let full = i.get_fullname().to_string();
            mdns.register(i).map(|_| full)
        })
        .unwrap_or_else(|e| {
            lg!("FATAL: mDNS register: {e}");
            std::process::exit(1)
        });
    let ctx = Arc::new(Ctx { cfg, tunnel: tunnel.clone(), signer: || Box::new(ChipSigner) });
    accept_loop(listener, ctx);
    lg!("mDNS registered {full} on port {port}: waiting for a sender");
    tunnel.publish(None);

    while !STOP.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
    }
    // Goodbye packets: without them the iPhone keeps a dead receiver in its list for the record TTL.
    if let Ok(rx) = mdns.unregister(&full) {
        let _ = rx.recv_timeout(Duration::from_secs(2));
    }
    std::thread::sleep(Duration::from_millis(300));
    if let Ok(rx) = mdns.shutdown() {
        let _ = rx.recv_timeout(Duration::from_secs(2));
    }
    let _ = std::fs::remove_file(&state);
    lg!("stop (signal): mDNS goodbye sent");
}

#[cfg(test)]
mod tests;
