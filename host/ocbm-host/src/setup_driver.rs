//! setup_driver — HOST-side, transport-free AirPlay/RTSP SETUP state machine (app-driven SETUP, P2).
//!
//! This is the P2 counterpart to P1's Stage-0 echo harness (`mode_setup_relay` in `main.rs`): instead
//! of echoing the box's relayed `local` response verbatim, the host **authors** the response the phone
//! sees. The box still owns every socket bind (pre-bind + oracle, plan §"Settled decisions 1"), so the
//! authored response takes its box-owned port values FROM the relayed local response and authors only
//! the parts that are genuine host policy — `enabledFeatures` (phase 1) and the per-stream response
//! shape (phase 2). Every authored response is diffed against the relayed local (the LIVE ORACLE), so a
//! divergence is visible immediately and is either a bug or a documented intentional difference.
//!
//! Transport-free by construction: the authoring functions are pure `(local, req, config) -> bytes`,
//! and every unit test is hermetic (no sockets). The wiring into the OCBM relay loop lives in `main.rs`.
//!
//! DEPENDENCIES: `plist` (parse the relayed local bplist + the request, serialize the authored
//! response — the SAME crate the box uses in `session.rs`, so the output is byte-comparable) and a
//! MINIMAL `serde_yaml` parse of the ~handful of `accessoryConfig` booleans the box derives its
//! feature set from. This deliberately does NOT link the `receiver` crate — a shared config crate is
//! the P3/Swift-era cleanup, not now (plan P3: `App/VehicleConfig.swift` becomes the one source of
//! truth for both the pushed YAML and the SETUP answers).

use std::io::Cursor;

use plist::{Dictionary, Value};
use serde::Deserialize;

/// Response-dict keys whose values are BOX-OWNED socket ports/handles (the box binds them and the host
/// copies them through). The live-oracle diff normalizes these to a placeholder before comparing, so a
/// clean session (host authors the same SHAPE the box did) is a MATCH regardless of which ephemeral
/// ports the kernel handed the box this run. A real authoring divergence — a missing/extra key, a
/// different `enabledFeatures`, an omitted stream — survives normalization and is reported.
const PORT_KEYS: &[&str] = &[
    "timingPort",
    "eventPort",
    "keepAlivePort",
    "dataPort",
    "controlPort",
    "streamID",
];

// ---------------------------------------------------------------------------------------------
// Authoring config — the minimal slice of the pushed VehicleConfig YAML that drives enabledFeatures.
// Mirrors receiver::vehicle_config's accessoryConfig booleans + the altVideoStreams presence, but is
// an INDEPENDENT minimal parse (no receiver-crate link). Fields map 1:1 onto the enabledFeatures
// tokens session.rs authors in setup_phase1 (see `enabled_features`).
// ---------------------------------------------------------------------------------------------

/// Root of the pushed YAML — only the fields the host derives features from. serde ignores everything
/// else (the full VehicleConfig is much larger), exactly like the box's `VehicleConfig` does.
#[derive(Debug, Clone, Default, Deserialize)]
struct YamlRoot {
    #[serde(default, rename = "accessoryConfig")]
    accessory_config: YamlAccessory,
    #[serde(default, rename = "videoStreamsConfig")]
    video_streams_config: YamlVideoStreams,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct YamlAccessory {
    #[serde(default, rename = "enablesHEVC")]
    enables_hevc: bool,
    #[serde(default, rename = "enablesViewAreas")]
    enables_view_areas: bool,
    #[serde(default, rename = "enablesCornerMasks")]
    enables_corner_masks: bool,
    #[serde(default, rename = "enablesLogTransfer")]
    enables_log_transfer: bool,
    #[serde(default, rename = "enablesMainBufferedAudio")]
    enables_main_buffered_audio: bool,
    #[serde(default, rename = "appDrivenSetup")]
    app_driven_setup: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct YamlVideoStreams {
    /// A non-empty `altVideoStreams[]` is what arms the box's `altScreen` lever (vehicle_config.rs
    /// `alt_screen()` → airplayd `levers::set_altscreen`). Its contents are irrelevant to the feature
    /// echo — only its presence — so the element type is `IgnoredAny`-shaped (an empty struct).
    #[serde(default, rename = "altVideoStreams")]
    alt_video_streams: Vec<YamlIgnored>,
}

/// A YAML mapping we only count, never read. serde still needs a type to deserialize each element into.
#[derive(Debug, Clone, Default, Deserialize)]
struct YamlIgnored {}

/// The host's derived feature policy — the parsed, feature-relevant slice of the pushed YAML.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthoringConfig {
    pub hevc: bool,
    pub alt_screen: bool,
    pub view_areas: bool,
    pub corner_masks: bool,
    pub log_transfer: bool,
    pub main_buffered: bool,
    /// Whether the YAML actually requested app-driven SETUP. Not part of the feature echo; surfaced so
    /// the harness can warn if it is authoring against a config that never armed the box relay.
    pub app_driven_setup: bool,
}

impl AuthoringConfig {
    /// Parse the pushed YAML into the feature policy. A malformed document yields the all-false default
    /// (author nothing) rather than an error — the oracle diff will then loudly flag every feature the
    /// box echoed, which is a better signal than a silent panic on the relay thread.
    pub fn from_yaml(bytes: &[u8]) -> Self {
        let root: YamlRoot = match serde_yaml::from_slice(bytes) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[setup-driver] YAML parse failed ({e}) — authoring the empty feature set; the oracle will flag divergences");
                return Self::default();
            }
        };
        let ac = &root.accessory_config;
        // Feature gating derived to match session.rs setup_phase1. mainBuffered is config-primary
        // since docs/carplay/04_CAPABILITIES_AND_CONFIG.md B4 (authored here from enablesMainBufferedAudio, matching the box lever).
        // iAPChannel/sessionManagement live only in the box process (env-gated at the wireless
        // spawn site) and cannot be authored from the YAML — since the 2026-08-10 wireless flip they
        // are PRESERVED from the box's local response by `author_phase1` rather than being a
        // divergence. The env FORCE-ARMS of cornerMasks/logTransfer are still a documented (not a
        // bug) divergence.
        let corner_masks = ac.enables_corner_masks;
        Self {
            hevc: ac.enables_hevc,
            alt_screen: !root.video_streams_config.alt_video_streams.is_empty(),
            // viewAreas is echoed when the accessory enables it OR when cornerMasks is on (session.rs
            // `levers::viewareas() || levers::cornermasks()`). NOTE: the box ALSO turns viewAreas on for
            // a real inset safeArea even without the flag (vehicle_config `view_areas_enabled`); the host
            // does not replicate that inset detection here (minimal parse) — it is a documented divergence
            // the oracle catches, only reachable for a YAML with an inset but no enablesViewAreas.
            view_areas: ac.enables_view_areas || corner_masks,
            corner_masks,
            log_transfer: ac.enables_log_transfer,
            main_buffered: ac.enables_main_buffered_audio,
            app_driven_setup: ac.app_driven_setup,
        }
    }

    /// The `enabledFeatures` string array, in session.rs's exact emission order
    /// (hevc, altScreen, viewAreas, cornerMasks, logTransfer, mainBuffered) so even an
    /// order-sensitive comparison matches the box.
    pub fn enabled_features(&self) -> Vec<String> {
        let mut f = Vec::new();
        if self.hevc {
            f.push("hevc".to_string());
        }
        if self.alt_screen {
            f.push("altScreen".to_string());
        }
        if self.view_areas {
            f.push("viewAreas".to_string());
        }
        if self.corner_masks {
            f.push("cornerMasks".to_string());
        }
        if self.log_transfer {
            f.push("logTransfer".to_string());
        }
        if self.main_buffered {
            f.push("mainBuffered".to_string());
        }
        f
    }
}

// ---------------------------------------------------------------------------------------------
// The state machine.
// ---------------------------------------------------------------------------------------------

/// SETUP lifecycle state (plan P2): Idle → Phase1Done → StreamsActive, reset on full-TEARDOWN /
/// RS_CLOSE / CT_HELLO. The state is diagnostic + a light sanity surface (a phase-2 SETUP before a
/// phase-1 one is unusual); authoring itself is stateless (pure functions), so a lost transition can
/// never corrupt an authored response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Phase1Done,
    StreamsActive,
}

/// The host SETUP driver: the parsed config + the lifecycle state.
pub struct SetupDriver {
    pub state: State,
    cfg: AuthoringConfig,
}

impl SetupDriver {
    pub fn new(cfg: AuthoringConfig) -> Self {
        Self {
            state: State::Idle,
            cfg,
        }
    }

    /// Reset to Idle — called on full TEARDOWN, RS_CLOSE, and CT_HELLO (a new OCBM attach voids any
    /// in-flight session). Idempotent.
    pub fn reset(&mut self) {
        self.state = State::Idle;
    }

    /// Author a SETUP response. Phase 1 vs phase 2 splits on the presence of the `streams` key in the
    /// REQUEST — identical to the box (session.rs:1559). Advances the lifecycle state.
    pub fn on_setup(&mut self, local: &[u8], req: &[u8]) -> Vec<u8> {
        if request_has_streams(req) {
            self.state = State::StreamsActive;
            author_phase2(local, req)
        } else {
            self.state = State::Phase1Done;
            author_phase1(local, &self.cfg)
        }
    }

    /// Author a RECORD response. The box's reference answer is a bodyless 200 (session.rs `record`
    /// returns `Vec::new()`), so the host authors the same empty body. State is unchanged (RECORD runs
    /// while StreamsActive). `local` is accepted for symmetry (and is normally empty here).
    pub fn on_record(&mut self, _local: &[u8]) -> Vec<u8> {
        Vec::new()
    }

    /// Process a TEARDOWN NOTIFY (no response owed). A FULL teardown (no `streams` key) resets the
    /// driver; a partial teardown (a `streams` array naming only some streams) keeps the session, same
    /// split as session.rs `teardown`.
    pub fn on_teardown(&mut self, req: &[u8]) {
        if !request_has_streams(req) {
            self.reset();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Pure authoring functions.
// ---------------------------------------------------------------------------------------------

/// Does this SETUP/TEARDOWN request carry a `streams` array? (phase-2 / partial-teardown marker).
pub fn request_has_streams(req: &[u8]) -> bool {
    parse_dict(req)
        .map(|d| d.contains_key("streams"))
        .unwrap_or(false)
}

/// Every feature token the host can author from config. A token the BOX put in the local response
/// that is not in this set is box-owned and must be PRESERVED — see [`author_phase1`].
pub const HOST_AUTHORABLE_FEATURES: &[&str] =
    &["hevc", "altScreen", "viewAreas", "cornerMasks", "logTransfer", "mainBuffered"];

/// Phase-1 authoring: copy the box-owned control ports (timingPort/eventPort/keepAlivePort — and any
/// other key the box put there) through from the relayed `local` response, and author
/// `enabledFeatures` from the pushed YAML. Copying every non-`enabledFeatures` key through is what
/// "the box owns the binds, the host owns the feature policy" means in practice.
pub fn author_phase1(local: &[u8], cfg: &AuthoringConfig) -> Vec<u8> {
    let mut d = parse_dict(local).unwrap_or_default();
    // Box-only tokens carried by the local response are UNIONED in, not overwritten. Load-bearing
    // on WIRELESS (the bug that blocked the 2026-08-10 flip): the box emits `iAPChannel` and
    // `sessionManagement` there, gated on spawn-site env vars with no config key. `iAPChannel` is
    // not cosmetic — `/info` advertises `iAPChannelInfo` AND this echo must carry the token, or iOS
    // never binds a handler and 400s every `iAPSendMessage` (device-observed 3/3). The iAP2 tunnel's
    // DETECT+SYN rides that command, so overwriting would kill the tunnel every wireless session.
    // Appending AFTER the authored six reproduces the box's own order (session.rs emits the two
    // box-only tokens last), which matters because both oracles compare ORDERED sequences. A future
    // box-only token emitted mid-list would need slotting, not appending, or it reads as a WARN.
    let box_only: Vec<Value> = match d.get("enabledFeatures") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| match v {
                Value::String(s) if !HOST_AUTHORABLE_FEATURES.contains(&s.as_str()) => {
                    Some(Value::String(s.clone()))
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let mut feats: Vec<Value> = cfg
        .enabled_features()
        .into_iter()
        .map(Value::String)
        .collect();
    feats.extend(box_only);
    d.insert("enabledFeatures".into(), Value::Array(feats));
    to_binary(&d)
}

/// Phase-2 authoring: for each REQUESTED stream, author the per-type response dict — taking the
/// box-bound `dataPort`/`controlPort`/`streamID` from the matching stream in the relayed `local`
/// response (the box owns the socket) and authoring the rest (`type`, `streamConnectionID`, and WHICH
/// keys a given stream type carries). A requested stream the box did not answer (bind failure, invalid
/// scid, unimplemented type) has no local match and is likewise omitted here — mirroring session.rs,
/// which OMITS unsupported streams rather than erroring the whole SETUP.
pub fn author_phase2(local: &[u8], req: &[u8]) -> Vec<u8> {
    let local_streams = dict_streams(local);
    let req_streams = dict_streams(req);
    let mut consumed = vec![false; local_streams.len()];
    let mut out: Vec<Value> = Vec::new();

    for rs in &req_streams {
        let Some(rd) = rs.as_dictionary() else {
            continue;
        };
        let ty = int_key(rd, "type").unwrap_or(0);
        let scid = int_key(rd, "streamConnectionID").unwrap_or(0);
        // Find the box's answer for THIS requested stream: same type, and — for stream types whose
        // local answer echoes the scid (audio 100-102, DataStream 130) — the same scid, so duplicate
        // audio types can't be crossed. Screen 110/111 answers carry no scid, matched on type alone
        // (at most one of each per session). First unconsumed match wins (request order == box order).
        let idx = local_streams.iter().enumerate().position(|(i, ls)| {
            if consumed[i] {
                return false;
            }
            let Some(ld) = ls.as_dictionary() else {
                return false;
            };
            if int_key(ld, "type") != Some(ty) {
                return false;
            }
            match int_key(ld, "streamConnectionID") {
                Some(local_scid) => local_scid == scid,
                None => true,
            }
        });
        let Some(i) = idx else {
            continue; // box omitted this stream — omit it too
        };
        consumed[i] = true;
        let ld = local_streams[i].as_dictionary().expect("matched a dict above");
        out.push(author_stream(ty, scid, ld));
    }

    let mut d = Dictionary::new();
    d.insert("streams".into(), Value::Array(out));
    to_binary(&d)
}

/// Author one stream's response dict, per the box's per-type shape (session.rs setup_phase2), pulling
/// the box-owned port values from the matched local stream dict `ld`:
///   * 110 / 111 (screen): `{type, dataPort}` — no scid echoed.
///   * 100 / 101 (audio):  `{type, streamConnectionID, dataPort}`.
///   * 102 (media audio):  `{type, streamConnectionID, dataPort, controlPort}` — 102 alone has RTCP.
///   * anything else (e.g. the 130 RCS DataStream, which only WIRELESS carries): pass the box's local answer
///     through verbatim so the oracle stays clean and the box keeps ownership of those binds
///     on both transports.
fn author_stream(ty: i64, scid: i64, ld: &Dictionary) -> Value {
    let copy_port = |r: &mut Dictionary, k: &str| {
        if let Some(v) = ld.get(k) {
            r.insert(k.into(), v.clone());
        }
    };
    match ty {
        110 | 111 => {
            let mut r = Dictionary::new();
            r.insert("type".into(), Value::Integer(ty.into()));
            copy_port(&mut r, "dataPort");
            Value::Dictionary(r)
        }
        100..=102 => {
            let mut r = Dictionary::new();
            r.insert("type".into(), Value::Integer(ty.into()));
            r.insert("streamConnectionID".into(), Value::Integer(scid.into()));
            copy_port(&mut r, "dataPort");
            if ty == 102 {
                copy_port(&mut r, "controlPort");
            }
            Value::Dictionary(r)
        }
        // Out of host-authoring scope (e.g. the 130 RCS DataStream) — permanent design, not a
        // milestone gate, since the box owns those binds on both transports: echo the
        // box's own answer so nothing regresses and the oracle reports MATCH.
        _ => Value::Dictionary(ld.clone()),
    }
}

// ---------------------------------------------------------------------------------------------
// Live oracle — diff the host-authored response against the box's relayed local response.
// ---------------------------------------------------------------------------------------------

/// Compare an authored response against the box's local response, plist-semantically, after
/// normalizing the box-owned port values (which the host copied through). An empty result = MATCH; a
/// non-empty result names each diverging path (`enabledFeatures`, `streams[1].controlPort`, …). A
/// divergence is either a real authoring bug or a documented intentional difference.
pub fn oracle_diff(authored: &[u8], local: &[u8]) -> Vec<String> {
    let a = normalize(parse_value(authored));
    let l = normalize(parse_value(local));
    let mut diffs = Vec::new();
    collect_diffs("", &a, &l, &mut diffs);
    diffs
}

/// Parse response bytes to a plist Value, or an empty dict if the body is empty/unparseable (a RECORD
/// body is legitimately empty; treating it as an empty dict keeps the oracle from crashing on it).
fn parse_value(bytes: &[u8]) -> Value {
    if bytes.is_empty() {
        return Value::Dictionary(Dictionary::new());
    }
    Value::from_reader(Cursor::new(bytes)).unwrap_or_else(|_| Value::Dictionary(Dictionary::new()))
}

/// Recursively replace every [`PORT_KEYS`] value with a placeholder so the diff is about SHAPE, not
/// which ephemeral port the kernel handed the box. Dives into `streams[]` dicts too.
fn normalize(v: Value) -> Value {
    match v {
        Value::Dictionary(d) => {
            let mut out = Dictionary::new();
            for (k, val) in d.into_iter() {
                if PORT_KEYS.contains(&k.as_str()) {
                    out.insert(k, Value::Integer(0i64.into()));
                } else {
                    out.insert(k, normalize(val));
                }
            }
            Value::Dictionary(out)
        }
        Value::Array(a) => Value::Array(a.into_iter().map(normalize).collect()),
        other => other,
    }
}

/// Walk two normalized Values in lockstep, pushing a path string for every point they diverge:
/// a value mismatch, a key present on one side only, or a differing array length.
fn collect_diffs(path: &str, a: &Value, b: &Value, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Dictionary(da), Value::Dictionary(db)) => {
            let mut keys: Vec<&String> = da.keys().chain(db.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let child = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                match (da.get(k), db.get(k)) {
                    (Some(va), Some(vb)) => collect_diffs(&child, va, vb, out),
                    (Some(_), None) => out.push(format!("{child} (authored-only)")),
                    (None, Some(_)) => out.push(format!("{child} (local-only)")),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(aa), Value::Array(ab)) => {
            if aa.len() != ab.len() {
                out.push(format!("{path} (array len {} vs {})", aa.len(), ab.len()));
            }
            for (i, (ea, eb)) in aa.iter().zip(ab.iter()).enumerate() {
                collect_diffs(&format!("{path}[{i}]"), ea, eb, out);
            }
        }
        _ => {
            if a != b {
                out.push(if path.is_empty() {
                    "<root>".to_string()
                } else {
                    path.to_string()
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// plist helpers.
// ---------------------------------------------------------------------------------------------

/// Parse response/request bytes into a top-level Dictionary, or `None` if empty/unparseable/not a dict.
fn parse_dict(bytes: &[u8]) -> Option<Dictionary> {
    if bytes.is_empty() {
        return None;
    }
    Value::from_reader(Cursor::new(bytes))
        .ok()
        .and_then(|v| v.into_dictionary())
}

/// The `streams` array of a request/response, or empty.
fn dict_streams(bytes: &[u8]) -> Vec<Value> {
    parse_dict(bytes)
        .and_then(|d| d.get("streams").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default()
}

/// Read an integer plist key that may be stored signed or unsigned (iOS/plist mixes both).
fn int_key(d: &Dictionary, k: &str) -> Option<i64> {
    d.get(k).and_then(|v| {
        v.as_signed_integer()
            .or_else(|| v.as_unsigned_integer().map(|u| u as i64))
    })
}

/// Serialize a Dictionary to a binary plist — the same encoder session.rs uses, so the bytes are
/// directly comparable to the box's.
fn to_binary(d: &Dictionary) -> Vec<u8> {
    let mut buf = Vec::new();
    // A literal dict of integers/strings/arrays never fails to serialize; an empty Vec on the
    // impossible error path is still a valid (if empty) response the oracle will flag.
    if Value::Dictionary(d.clone()).to_writer_binary(&mut buf).is_err() {
        buf.clear();
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bin(d: &Dictionary) -> Vec<u8> {
        to_binary(d)
    }

    /// Build a representative phase-1 local response as the box would: the three control ports plus an
    /// authored enabledFeatures set.
    fn box_phase1(features: &[&str], keep_alive: bool) -> Vec<u8> {
        let mut d = Dictionary::new();
        d.insert("timingPort".into(), Value::Integer(50001i64.into()));
        d.insert("eventPort".into(), Value::Integer(50002i64.into()));
        if keep_alive {
            d.insert("keepAlivePort".into(), Value::Integer(50003i64.into()));
        }
        d.insert(
            "enabledFeatures".into(),
            Value::Array(features.iter().map(|s| Value::String(s.to_string())).collect()),
        );
        bin(&d)
    }

    // Phase-1: host authoring reproduces the box's local response exactly (after port normalization).
    #[test]
    fn phase1_reproduces_box_local_response() {
        // Config with HEVC + viewAreas + cornerMasks + logTransfer + mainBuffered on; box echoes the
        // same set/order. mainBuffered is pinned here (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B4) so a future edit cannot slot it at
        // a different position in one author than the other.
        let yaml = b"accessoryConfig:\n  enablesHEVC: true\n  enablesViewAreas: true\n  enablesCornerMasks: true\n  enablesLogTransfer: true\n  enablesMainBufferedAudio: true\n";
        let cfg = AuthoringConfig::from_yaml(yaml);
        assert_eq!(
            cfg.enabled_features(),
            vec!["hevc", "viewAreas", "cornerMasks", "logTransfer", "mainBuffered"],
            "order + membership must match session.rs setup_phase1"
        );
        let local = box_phase1(
            &["hevc", "viewAreas", "cornerMasks", "logTransfer", "mainBuffered"],
            true,
        );
        let authored = author_phase1(&local, &cfg);
        assert!(
            oracle_diff(&authored, &local).is_empty(),
            "authored phase-1 must equal the box local after port-normalization: {:?}",
            oracle_diff(&authored, &local)
        );
        // Ports were copied through verbatim (not re-invented).
        let ad = parse_dict(&authored).unwrap();
        assert_eq!(int_key(&ad, "timingPort"), Some(50001));
        assert_eq!(int_key(&ad, "eventPort"), Some(50002));
        assert_eq!(int_key(&ad, "keepAlivePort"), Some(50003));
    }

    // WIRELESS shape: the box emits two tokens the host has no config key for (iAPChannel,
    // sessionManagement — env-gated at the wireless spawn site). They MUST survive host authoring:
    // /info advertises iAPChannelInfo and iOS 400s every iAPSendMessage unless this echo carries the
    // token, which would kill the iAP2 tunnel's DETECT+SYN on every wireless session. This fixture is
    // what the 2026-08-10 wireless flip was missing.
    #[test]
    fn phase1_preserves_box_only_features_on_wireless() {
        let yaml = b"accessoryConfig:\n  enablesHEVC: true\n";
        let cfg = AuthoringConfig::from_yaml(yaml);
        let local = box_phase1(&["hevc", "iAPChannel", "sessionManagement"], true);
        let authored = author_phase1(&local, &cfg);
        let d = parse_dict(&authored).unwrap();
        let feats: Vec<String> = match d.get("enabledFeatures") {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_string().map(str::to_string))
                .collect(),
            _ => Vec::new(),
        };
        assert_eq!(
            feats,
            vec!["hevc", "iAPChannel", "sessionManagement"],
            "box-only tokens must be preserved, host-authorable ones authored"
        );
        // …and the oracle must call that a MATCH, not a divergence.
        assert!(
            oracle_diff(&authored, &local).is_empty(),
            "preserved box-only tokens must not read as an authoring divergence: {:?}",
            oracle_diff(&authored, &local)
        );
    }

    // altScreen comes from a non-empty altVideoStreams[], slotted in session.rs's order (after hevc).
    #[test]
    fn alt_screen_feature_from_alt_video_streams() {
        let yaml = b"accessoryConfig:\n  enablesHEVC: true\nvideoStreamsConfig:\n  altVideoStreams:\n  - pixelDimensions: { width: 800, height: 480 }\n";
        let cfg = AuthoringConfig::from_yaml(yaml);
        assert_eq!(cfg.enabled_features(), vec!["hevc", "altScreen"]);
    }

    // Phase-2: a single screen (type 110) stream — authored dict is {type, dataPort}, no scid, with the
    // box-bound dataPort copied through.
    #[test]
    fn phase2_single_screen_stream() {
        // Box local answer for the screen: {type:110, dataPort:6001}.
        let mut ls = Dictionary::new();
        ls.insert("type".into(), Value::Integer(110i64.into()));
        ls.insert("dataPort".into(), Value::Integer(6001i64.into()));
        let mut local = Dictionary::new();
        local.insert("streams".into(), Value::Array(vec![Value::Dictionary(ls)]));
        let local = bin(&local);

        // Phone request: {type:110, streamConnectionID:0x1122}.
        let mut rs = Dictionary::new();
        rs.insert("type".into(), Value::Integer(110i64.into()));
        rs.insert("streamConnectionID".into(), Value::Integer(0x1122i64.into()));
        let mut req = Dictionary::new();
        req.insert("streams".into(), Value::Array(vec![Value::Dictionary(rs)]));
        let req = bin(&req);

        let authored = author_phase2(&local, &req);
        assert!(
            oracle_diff(&authored, &local).is_empty(),
            "screen phase-2 must match the box local: {:?}",
            oracle_diff(&authored, &local)
        );
        // The authored screen dict must carry the box port and NOT invent a streamConnectionID.
        let streams = dict_streams(&authored);
        assert_eq!(streams.len(), 1);
        let sd = streams[0].as_dictionary().unwrap();
        assert_eq!(int_key(sd, "type"), Some(110));
        assert_eq!(int_key(sd, "dataPort"), Some(6001));
        assert!(sd.get("streamConnectionID").is_none(), "type 110 echoes no scid");
    }

    // Phase-2: a media-audio (type 102) stream keeps scid + dataPort + controlPort (the RTCP port).
    #[test]
    fn phase2_media_audio_keeps_control_port() {
        let mut ls = Dictionary::new();
        ls.insert("type".into(), Value::Integer(102i64.into()));
        ls.insert("streamConnectionID".into(), Value::Integer(0xABCDi64.into()));
        ls.insert("dataPort".into(), Value::Integer(7001i64.into()));
        ls.insert("controlPort".into(), Value::Integer(7002i64.into()));
        let mut local = Dictionary::new();
        local.insert("streams".into(), Value::Array(vec![Value::Dictionary(ls)]));
        let local = bin(&local);

        let mut rs = Dictionary::new();
        rs.insert("type".into(), Value::Integer(102i64.into()));
        rs.insert("streamConnectionID".into(), Value::Integer(0xABCDi64.into()));
        let mut req = Dictionary::new();
        req.insert("streams".into(), Value::Array(vec![Value::Dictionary(rs)]));
        let req = bin(&req);

        let authored = author_phase2(&local, &req);
        assert!(
            oracle_diff(&authored, &local).is_empty(),
            "media-audio phase-2 must match: {:?}",
            oracle_diff(&authored, &local)
        );
        let sd = dict_streams(&authored)[0].as_dictionary().unwrap().clone();
        assert_eq!(int_key(&sd, "controlPort"), Some(7002));
        assert_eq!(int_key(&sd, "streamConnectionID"), Some(0xABCD));
    }

    // A requested stream the box omitted (no local match) is omitted by the host too.
    #[test]
    fn phase2_omits_streams_the_box_omitted() {
        // Box answered only the screen; the phone also asked for an unimplemented type 120.
        let mut ls = Dictionary::new();
        ls.insert("type".into(), Value::Integer(110i64.into()));
        ls.insert("dataPort".into(), Value::Integer(6001i64.into()));
        let mut local = Dictionary::new();
        local.insert("streams".into(), Value::Array(vec![Value::Dictionary(ls)]));
        let local = bin(&local);

        let mk = |ty: i64| {
            let mut d = Dictionary::new();
            d.insert("type".into(), Value::Integer(ty.into()));
            d.insert("streamConnectionID".into(), Value::Integer(9i64.into()));
            Value::Dictionary(d)
        };
        let mut req = Dictionary::new();
        req.insert("streams".into(), Value::Array(vec![mk(110), mk(120)]));
        let req = bin(&req);

        let authored = author_phase2(&local, &req);
        assert_eq!(dict_streams(&authored).len(), 1, "type 120 must be omitted like the box");
        assert!(oracle_diff(&authored, &local).is_empty());
    }

    // WIRELESS phase-2: the type-130 RCS DataStream. Its request carries NO `streamConnectionID`
    // (docs/ops/captures/2026-07-25_SUCCESS_airplayd_wl_handshake.txt:25 lists the request keys), so the
    // scid defaults to 0 on BOTH sides and the match must still find the box's answer. The box's
    // entry — `streamID` above all, the transport token iOS reads — must reach the phone verbatim: a
    // dropped or reshaped entry is byte-for-byte the same outage as the box-side skip guard
    // (session.rs `if scid == 0 && ty != 130`), i.e. iAP2 tunnel stuck at Init, no NowPlaying.
    // Load-bearing since the 2026-08-10 wireless flip put this authoring path on the wireless SETUP.
    #[test]
    fn phase2_datastream_130_transport_token_survives_authoring() {
        let mut ls = Dictionary::new();
        ls.insert("type".into(), Value::Integer(130i64.into()));
        ls.insert("streamConnectionID".into(), Value::Integer(0i64.into()));
        ls.insert("streamID".into(), Value::Integer(1i64.into()));
        ls.insert("dataPort".into(), Value::Integer(51305i64.into()));
        let mut local = Dictionary::new();
        local.insert("streams".into(), Value::Array(vec![Value::Dictionary(ls)]));
        let local = bin(&local);

        // The phone's request: type 130 and NO streamConnectionID key at all.
        let mut rs = Dictionary::new();
        rs.insert("type".into(), Value::Integer(130i64.into()));
        rs.insert("channelID".into(), Value::String("5E:F7:F7:A9:CB:CD-RCS-1".into()));
        rs.insert("wantsDedicatedSocket".into(), Value::Boolean(true));
        assert!(!rs.contains_key("streamConnectionID"));
        let mut req = Dictionary::new();
        req.insert("streams".into(), Value::Array(vec![Value::Dictionary(rs)]));
        let req = bin(&req);

        let authored = author_phase2(&local, &req);
        let streams = dict_streams(&authored);
        assert_eq!(streams.len(), 1, "the RCS DataStream must not be dropped by host authoring");
        let sd = streams[0].as_dictionary().unwrap();
        assert_eq!(int_key(sd, "type"), Some(130));
        assert_eq!(
            int_key(sd, "streamID"),
            Some(1),
            "the transport token must be echoed verbatim — without it iOS logs kNotFoundErr and \
             never builds its outbound iAP2 transport streams"
        );
        assert_eq!(int_key(sd, "dataPort"), Some(51305));
        assert!(
            oracle_diff(&authored, &local).is_empty(),
            "the 130 echo must read as a MATCH, not a divergence: {:?}",
            oracle_diff(&authored, &local)
        );
    }

    // Phase split on the `streams` key drives the state machine, same rule as session.rs:1559.
    #[test]
    fn phase_split_and_state_transitions() {
        let cfg = AuthoringConfig::from_yaml(b"accessoryConfig:\n  appDrivenSetup: true\n");
        let mut drv = SetupDriver::new(cfg);
        assert_eq!(drv.state, State::Idle);

        // Phase-1 request (no streams).
        let mut p1 = Dictionary::new();
        p1.insert("keepAliveLowPower".into(), Value::Boolean(true));
        let p1 = bin(&p1);
        assert!(!request_has_streams(&p1));
        let local1 = box_phase1(&[], true);
        drv.on_setup(&local1, &p1);
        assert_eq!(drv.state, State::Phase1Done);

        // Phase-2 request (has streams).
        let mut ls = Dictionary::new();
        ls.insert("type".into(), Value::Integer(110i64.into()));
        ls.insert("dataPort".into(), Value::Integer(6001i64.into()));
        let mut local2 = Dictionary::new();
        local2.insert("streams".into(), Value::Array(vec![Value::Dictionary(ls)]));
        let local2 = bin(&local2);
        let mut rs = Dictionary::new();
        rs.insert("type".into(), Value::Integer(110i64.into()));
        rs.insert("streamConnectionID".into(), Value::Integer(3i64.into()));
        let mut p2 = Dictionary::new();
        p2.insert("streams".into(), Value::Array(vec![Value::Dictionary(rs)]));
        let p2 = bin(&p2);
        assert!(request_has_streams(&p2));
        drv.on_setup(&local2, &p2);
        assert_eq!(drv.state, State::StreamsActive);

        // Partial teardown (streams present) keeps the session; full teardown resets.
        drv.on_teardown(&p2);
        assert_eq!(drv.state, State::StreamsActive, "partial teardown keeps the session");
        let full = bin(&Dictionary::new()); // empty dict → no streams key → full teardown
        drv.on_teardown(&full);
        assert_eq!(drv.state, State::Idle, "full teardown resets to Idle");
    }

    // Deliberate divergence: flip enablesHEVC OFF in the config and the host's authored enabledFeatures
    // drops "hevc" — so the oracle flags a mismatch against a box local that still carries it. This is
    // the app-driven-SETUP proof: the phone would see the HOST's set, not the box's.
    #[test]
    fn divergence_when_config_flips_hevc_off() {
        let cfg_off = AuthoringConfig::from_yaml(b"accessoryConfig:\n  enablesHEVC: false\n");
        assert!(cfg_off.enabled_features().is_empty());
        // The box (running an older/other config) still echoed hevc.
        let box_local = box_phase1(&["hevc"], false);
        let authored = author_phase1(&box_local, &cfg_off);
        let diff = oracle_diff(&authored, &box_local);
        assert!(
            diff.iter().any(|d| d.contains("enabledFeatures")),
            "flipping hevc off must diverge on enabledFeatures, got {diff:?}"
        );
    }

    // RECORD authors a bodyless 200, matching session.rs `record` returning Vec::new().
    #[test]
    fn record_authors_empty_body() {
        let cfg = AuthoringConfig::default();
        let mut drv = SetupDriver::new(cfg);
        assert!(drv.on_record(&[]).is_empty());
    }

    // Malformed YAML yields the empty feature set (author nothing) rather than panicking.
    #[test]
    fn malformed_yaml_is_empty_features() {
        let cfg = AuthoringConfig::from_yaml(b"\tnot: : valid: yaml:\n");
        assert!(cfg.enabled_features().is_empty());
    }
}
