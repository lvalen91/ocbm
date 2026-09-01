//! The iAP2 side's reader for the app-pushed `VehicleConfig` YAML (docs/carplay/04_CAPABILITIES_AND_CONFIG.md: anything configurable is
//! app-driven; the box presents what the app pushed).
//!
//! The macOS app ships one YAML at OCBM SUBSCRIBE and ocbmd lands it at `/tmp/carplay_cfg.yaml`
//! BEFORE it flips `/tmp/host_present` — and the supervisor only starts the phone-facing daemons on
//! that presence edge, so the document is always on disk before any Identify is built. The receiver
//! parses the same file for the `/info`-facing half (`receiver::vehicle_config`); this module reads
//! only the subtrees the iAP2 layer needs, and serde ignores everything else.
//!
//! ONE file, ONE schema, TWO readers with disjoint interests — deliberately not a second
//! representation of the same config (a derived side-file would be a divergence waiting to happen,
//! and would make ocbmd a config interpreter when it is defined as a dumb byte pipe, docs/carplay/01_OCBM_PROTOCOL.md).
//!
//! FOOTPRINT NOTE: serde + serde_yaml cost a MEASURED +66 KB packed / +132 KB unpacked on iap2d
//! (armv7-musl release, UPX-3.96: 200,784 -> 268,160 B packed). An earlier minimal probe predicted
//! +59 KB — real derive on these structs costs ~13% more, so quote the 66 KB figure. Accepted
//! because it is paid once for the whole iAP2 side and collapses into a single copy if the CarPlay
//! daemons are ever consolidated into one multi-applet binary.

use serde::Deserialize;

/// Where ocbmd lands the app-pushed document — the same hardcoded path the receiver and ocbmd use.
/// Deliberately NOT overridable by an environment variable: a per-reader override would let one
/// process read its `/info` half from one document and its iAP2 half from another, breaking this
/// module's whole point. [`Iap2Config::load_from`] is the test/bench seam instead.
pub const CFG_FILE: &str = "/tmp/carplay_cfg.yaml";

/// Refuse to read an implausibly large document rather than allocate on a corrupt/hostile file —
/// the same bound the receiver applies.
const MAX_CFG_BYTES: u64 = 256 * 1024;

/// The iAP2-relevant slice of the app's YAML. Every field is optional: a document that omits them
/// (or a missing file) yields the default, which leaves every existing compiled behaviour in place.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Iap2Config {
    /// Our extension (not a stock Apple `VehicleConfig` key): which metadata declaration tier the
    /// accessory advertises, and which features to drop from it.
    #[serde(default)]
    pub metadata: MetadataConfig,
    /// `iapConfig:` — the Simulator's own compiled-schema naming for the iAP2 subtree (docs/carplay/03_SDK_GROUND_TRUTH.md §7
    /// records `iapConfig.enablementConfig.appDiscovery`, so this is Apple's key, not ours).
    #[serde(rename = "iapConfig", default)]
    pub iap_config: IapConfigSection,
}

/// `iapConfig:` — vehicle identity for Identify params 20/21 (docs/carplay/04_CAPABILITIES_AND_CONFIG.md C6).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IapConfigSection {
    #[serde(rename = "vehicleInfo", default)]
    pub vehicle_info: Option<VehicleInfoConfig>,
    /// PRESENCE of this block is what arms param 21 at all — an absent block leaves the historical
    /// emission (no param 21) byte-for-byte intact. Note the three-way distinction, which is standard
    /// serde and fails safe: the key omitted entirely AND a null block (`vehicleStatus:` with nothing
    /// under it) both read as `None` (param 21 OFF); `vehicleStatus: {}` reads as `Some` with no
    /// capabilities and DOES arm it.
    #[serde(rename = "vehicleStatus", default)]
    pub vehicle_status: Option<VehicleStatusConfig>,
}

/// `iapConfig.vehicleInfo:` — Identify param 20 `VehicleInformationComponent` content.
///
/// ```yaml
/// iapConfig:
///   vehicleInfo:
///     engineTypes: [gasoline, electric]      # hybrid = two entries; sub 2 is [0+] in Apple's spec
///     chargingConnectors:
///       - type: ccs2
///         powerWatts: 150000
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VehicleInfoConfig {
    #[serde(rename = "engineTypes", default)]
    pub engine_types: Vec<String>,
    #[serde(rename = "chargingConnectors", default)]
    pub charging_connectors: Vec<ChargingConnectorConfig>,
    /// Sub 10, `utf8`. Free extra found while dumping Apple's table for C-0.
    #[serde(rename = "vehicleColorHexCode", default)]
    pub vehicle_color_hex_code: Option<String>,
}

/// One `chargingConnectors[]` entry: the connector enum (sub 11) plus its optional power rating,
/// which Apple models as a SEPARATE per-type sub (12-20), not as a field of the connector.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChargingConnectorConfig {
    #[serde(rename = "type", default)]
    pub connector_type: String,
    #[serde(rename = "powerWatts", default)]
    pub power_watts: Option<u32>,
}

/// `iapConfig.vehicleStatus:` — Identify param 21 `VehicleStatusComponent` capability flags.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VehicleStatusConfig {
    /// Apple's own field names, lowerCamelCased (`range`, `rangeElectric`, `electricChargeInfo`, …)
    /// so this list can be diffed directly against `tools/i2mspec_dump.py --message 0x1D01`.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// `metadata:` — the app-pushed replacement for the `CARPLAY_METADATA` / `/tmp/carplay_metadata`
/// bench levers (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B3).
///
/// ```yaml
/// metadata:
///   tier: extended          # proven | extended | all   (`rx-only` is REFUSED — docs/carplay/05_METADATA_AND_CONTROLS.md §6.2)
///   skip: [call_history]    # optional feature names to drop from the declaration
/// ```
///
/// The `skip` list REPLACES the bench levers' lists rather than concatenating with them: app intent
/// must not be silently mixed with stale on-box experiment state.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MetadataConfig {
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub skip: Vec<String>,
}

/// The RESOLVED vehicle identity the Identify builder consumes: YAML names already mapped to Apple's
/// wire enums/sub-ids, so `message.rs` never does string matching on the hot path and an unknown name
/// is reported ONCE at parse time rather than silently dropped at emit time.
///
/// [`VehicleIdentity::baseline`] is exactly today's hardcoded emission, so every existing call site
/// keeps its bytes by construction. The proof is `message.rs::baseline_param20_is_byte_pinned`, a
/// HARDCODED param-20 byte fixture — deliberately not a comparison between two builder calls, which
/// would move together under any edit to `baseline()` and prove only that the wrappers delegate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VehicleIdentity {
    /// Param 20 sub 2, one TLV per entry. Apple's spec marks this `[0+]` (verified from the compiled
    /// spec archive, C-0), so a hybrid legitimately emits two.
    pub engine_types: Vec<u8>,
    /// Param 20: `(connector enum for sub 11, optional power in W for the matching sub 12-20)`.
    pub connectors: Vec<(u8, Option<u32>)>,
    /// Param 20 sub 10.
    pub color_hex: Option<String>,
    /// Param 21 capability sub-ids. `None` = do not emit param 21 at all (the historical default).
    pub status_caps: Option<Vec<u16>>,
}

impl Default for VehicleIdentity {
    fn default() -> Self {
        Self::baseline()
    }
}

impl VehicleIdentity {
    /// Today's compiled emission: EngineType=Gasoline, nothing else. Any call site passing this
    /// produces byte-identical Identify bytes to the pre-C6 builder.
    pub fn baseline() -> Self {
        Self {
            engine_types: vec![crate::spec::ident::vehicle_info::ENGINE_TYPE_GASOLINE],
            connectors: Vec::new(),
            color_hex: None,
            status_caps: None,
        }
    }

    /// Is this the untouched baseline? WILL be used for iap2d's one-line startup log when C-3 wires
    /// the config in (nothing calls it yet); also keeps the
    /// "config present but empty" case from looking like a configured EV.
    pub fn is_baseline(&self) -> bool {
        *self == Self::baseline()
    }
}

/// `engineTypes` name -> param-20 sub-2 enum. Apple's four values; matched case-insensitively.
fn engine_type_id(name: &str) -> Option<u8> {
    use crate::spec::ident::vehicle_info as sub;
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "gasoline" | "petrol" => sub::ENGINE_TYPE_GASOLINE,
        "diesel" => sub::ENGINE_TYPE_DIESEL,
        "electric" | "ev" => sub::ENGINE_TYPE_ELECTRIC,
        "cng" => sub::ENGINE_TYPE_CNG,
        _ => return None,
    })
}

/// `chargingConnectors[].type` -> the param-20 sub-11 `SupportedChargingConnectors` enum.
///
/// Separators are STRIPPED, not folded to `_`. An earlier revision replaced `-`/space with `_`, which
/// worked for `nacs-dc` -> `nacs_dc` but silently turned `CCS-2` into `ccs_2`, matching no arm — so a
/// perfectly ordinary spelling of a Type-2 connector was dropped as "unknown". Stripping normalizes
/// every spelling of every name uniformly (`CCS-2`, `ccs 2`, `CCS_2` -> `ccs2`; `NACS-DC` -> `nacsdc`),
/// which also matters for dedup: two spellings of one connector must collapse to one enum, because
/// Apple's per-connector power subs are single-valued.
fn connector_enum(name: &str) -> Option<u8> {
    use crate::spec::ident::vehicle_info as sub;
    let norm: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect();
    Some(match norm.as_str() {
        "ccs1" => sub::SUPPORTED_CHARGING_CONNECTORS_CCS1,
        "ccs2" => sub::SUPPORTED_CHARGING_CONNECTORS_CCS2,
        "j1772" => sub::SUPPORTED_CHARGING_CONNECTORS_J1772,
        "chademo" => sub::SUPPORTED_CHARGING_CONNECTORS_CH_ADE_MO,
        "mennekes" => sub::SUPPORTED_CHARGING_CONNECTORS_MENNEKES,
        "gbtdc" => sub::SUPPORTED_CHARGING_CONNECTORS_GB_T_DC_,
        "gbtac" => sub::SUPPORTED_CHARGING_CONNECTORS_GB_T_AC_,
        "nacsdc" => sub::SUPPORTED_CHARGING_CONNECTORS_NACS_DC_,
        "nacsac" => sub::SUPPORTED_CHARGING_CONNECTORS_NACS_AC_,
        _ => return None,
    })
}

/// Connector enum (sub 11 value) -> the matching `PowerForConnectorType*` sub id (12-20).
///
/// Apple models a connector's power rating as a SEPARATE per-type sub rather than a field of the
/// connector, so this pairing is what keeps `{type, powerWatts}` in the YAML honest on the wire.
/// Written as an explicit table rather than `12 + enum`: the arithmetic holds for Apple's current
/// nine, but it is a coincidence of their ordering, not a rule, and a connector added out of order
/// would silently mis-assign power to the wrong type. `power_sub_matches_the_current_arithmetic`
/// pins the coincidence so the day it breaks is visible.
pub fn power_sub_for_connector(enum_v: u8) -> Option<u16> {
    use crate::spec::ident::vehicle_info as sub;
    Some(match enum_v {
        sub::SUPPORTED_CHARGING_CONNECTORS_CCS1 => sub::POWER_FOR_CONNECTOR_TYPE_CCS1,
        sub::SUPPORTED_CHARGING_CONNECTORS_CCS2 => sub::POWER_FOR_CONNECTOR_TYPE_CCS2,
        sub::SUPPORTED_CHARGING_CONNECTORS_J1772 => sub::POWER_FOR_CONNECTOR_TYPE_J1772,
        sub::SUPPORTED_CHARGING_CONNECTORS_CH_ADE_MO => sub::POWER_FOR_CONNECTOR_TYPE_CH_ADE_MO,
        sub::SUPPORTED_CHARGING_CONNECTORS_MENNEKES => sub::POWER_FOR_CONNECTOR_TYPE_MENNEKES,
        sub::SUPPORTED_CHARGING_CONNECTORS_GB_T_DC_ => sub::POWER_FOR_CONNECTOR_TYPE_GBT_DC,
        sub::SUPPORTED_CHARGING_CONNECTORS_GB_T_AC_ => sub::POWER_FOR_CONNECTOR_TYPE_GBT_AC,
        sub::SUPPORTED_CHARGING_CONNECTORS_NACS_DC_ => sub::POWER_FOR_CONNECTOR_TYPE_NACS_DC,
        sub::SUPPORTED_CHARGING_CONNECTORS_NACS_AC_ => sub::POWER_FOR_CONNECTOR_TYPE_NACS_AC,
        _ => return None,
    })
}

/// Validate `vehicleColorHexCode` down to the one shape Apple's field can mean: `#RRGGBB` (the `#`
/// optional on input, always emitted). Returns `None` — drop the sub — for anything else.
///
/// This is NOT the box overriding the app's choice: `#GGHHII` is not a colour the owner picked, it is
/// a malformed value, and dropping one optional sub is strictly better than putting it on the wire.
/// The concrete hazard is that sub 10 is free text landing in a NUL-terminated TLV: a YAML `"\0"`
/// escape survives into the string, and the phone's parser then reads a value whose length disagrees
/// with its declared length. Free text reaching a parser unsanitized is a mistake this project has
/// already paid for once (the app's YAML emitter, docs/carplay/04_CAPABILITIES_AND_CONFIG.md E). Length is bounded here too — the
/// `Tlv` length guards are `debug_assert!`s and are compiled out of the box's release build.
fn sanitize_color_hex(s: &str) -> Option<String> {
    let t = s.trim().trim_start_matches('#');
    if t.len() == 6 && t.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(format!("#{}", t.to_ascii_uppercase()))
    } else {
        if !s.trim().is_empty() {
            eprintln!(
                "[iap2-config] vehicleColorHexCode {s:?} is not #RRGGBB — dropped (sub 10 omitted)"
            );
        }
        None
    }
}

/// `vehicleStatus.capabilities[]` name -> param-21 presence-flag sub id (Apple's own field names).
fn status_cap_id(name: &str) -> Option<u16> {
    use crate::message::vehicle_status as sub;
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "range" => sub::RANGE,
        "outsidetemperature" => sub::OUTSIDE_TEMPERATURE,
        "insidetemperature" => sub::INSIDE_TEMPERATURE,
        "rangewarning" => sub::RANGE_WARNING,
        "rangegasoline" => sub::RANGE_GASOLINE,
        "rangediesel" => sub::RANGE_DIESEL,
        "rangeelectric" => sub::RANGE_ELECTRIC,
        "rangecng" => sub::RANGE_CNG,
        "rangewarninggasoline" => sub::RANGE_WARNING_GASOLINE,
        "rangewarningdiesel" => sub::RANGE_WARNING_DIESEL,
        "rangewarningelectric" => sub::RANGE_WARNING_ELECTRIC,
        "rangewarningcng" => sub::RANGE_WARNING_CNG,
        "wiperstatus" => sub::WIPER_STATUS,
        "barometricpressure" => sub::BAROMETRIC_PRESSURE,
        "alerts" => sub::ALERTS,
        "passengerseatstatus" => sub::PASSENGER_SEAT_STATUS,
        "electricchargeinfo" => sub::ELECTRIC_CHARGE_INFO,
        "maxrangeinfo" => sub::MAX_RANGE_INFO,
        _ => return None,
    })
}

impl Iap2Config {
    /// Resolve the pushed `iapConfig:` into the wire-ready [`VehicleIdentity`].
    ///
    /// Falls back to [`VehicleIdentity::baseline`] whenever the config says nothing usable, so an
    /// absent, empty or wholly-unrecognised block can never change the Identify bytes. Unknown names
    /// are logged and skipped rather than failing the parse — a typo in one connector must not cost
    /// the owner a handshake (`0x1D03` is unrecoverable within a session).
    ///
    /// EVERY list is DEDUPED here, and that is a correctness requirement rather than tidiness.
    /// Apple's own cardinalities (C-0, from the compiled spec archive) are mixed: sub 2 `EngineType`
    /// and sub 11 `SupportedChargingConnectors` are `[0+]`, but the per-connector power subs (12-20)
    /// and every param-21 capability flag are `[optional]` — i.e. **zero or one**. Two
    /// `chargingConnectors` rows of the same type (an ordinary copy-paste, and one this module's own
    /// forgiving name matching actively manufactures by folding `ccs2`/`CCS2`/`CCS-2` together) would
    /// otherwise emit sub 13 TWICE with contradictory values: a malformed component on the one
    /// message whose rejection cannot be recovered from. `message.rs`'s `union()` applies the same
    /// rule to params 6/7 for the same reason.
    ///
    /// Deduping also BOUNDS the emission by construction, which is the only real defence the box has:
    /// engines cap at 4, connectors at 9, capabilities at 18, so no pushed list can drive param 20
    /// past the ~4 KB wired `MaxRcvPacketLength` or the 64 KB u16 group-length ceiling (whose
    /// overflow guards are `debug_assert!`s, compiled OUT of the box's release build).
    pub fn vehicle_identity(&self) -> VehicleIdentity {
        let mut id = VehicleIdentity::baseline();
        if let Some(vi) = self.iap_config.vehicle_info.as_ref() {
            // Only REPLACE the baseline engine list if at least one name resolved; a block listing
            // only typos must not emit an Identify with zero EngineType subs.
            let mut engines: Vec<u8> = Vec::new();
            for n in &vi.engine_types {
                match engine_type_id(n) {
                    Some(v) if engines.contains(&v) => {
                        eprintln!("[iap2-config] duplicate engineType {n:?} — dropped")
                    }
                    Some(v) => engines.push(v),
                    None => eprintln!("[iap2-config] unknown engineType {n:?} — skipped"),
                }
            }
            if !engines.is_empty() {
                id.engine_types = engines;
            } else if !vi.engine_types.is_empty() {
                eprintln!("[iap2-config] no engineTypes resolved — keeping the gasoline baseline");
            }
            for c in &vi.charging_connectors {
                match connector_enum(&c.connector_type) {
                    // First row wins. Apple's power sub for a type is single-valued, so a second row
                    // for the same connector cannot be represented at all — dropping it is the only
                    // well-formed option, and the owner needs to be told which rating survived.
                    Some(enum_v) if id.connectors.iter().any(|(e, _)| *e == enum_v) => eprintln!(
                        "[iap2-config] duplicate chargingConnector {:?} — dropped (first row wins; \
                         Apple's PowerForConnectorType* subs are single-valued)",
                        c.connector_type
                    ),
                    Some(enum_v) => id.connectors.push((enum_v, c.power_watts)),
                    None => eprintln!(
                        "[iap2-config] unknown chargingConnector type {:?} — skipped",
                        c.connector_type
                    ),
                }
            }
            // Ascending by connector enum, which is also ascending by power sub id (the two run in
            // lockstep — `power_sub_matches_the_current_arithmetic` pins that). Config order must not
            // decide wire order: a Tesla owner listing `nacs_dc` before `ccs2` would otherwise emit
            // sub 19 before sub 13. Apple's spec archive does not state an ordering REQUIREMENT and
            // R14G17 carries no iAP2 Identify encoder to cite, so this is grounded in the project's
            // own standing decision, not in a claim about Apple: see `message.rs`'s param-16 comment
            // ("Wire order matters here ... untested whether iOS's parser is truly order-independent,
            // so this doesn't take the risk").
            id.connectors.sort_by_key(|(e, _)| *e);
            id.color_hex = vi
                .vehicle_color_hex_code
                .as_ref()
                .and_then(|s| sanitize_color_hex(s));
        }
        if let Some(vs) = self.iap_config.vehicle_status.as_ref() {
            let mut caps: Vec<u16> = Vec::new();
            for n in &vs.capabilities {
                match status_cap_id(n) {
                    Some(v) if caps.contains(&v) => {
                        eprintln!("[iap2-config] duplicate vehicleStatus capability {n:?} — dropped")
                    }
                    Some(v) => caps.push(v),
                    None => {
                        eprintln!("[iap2-config] unknown vehicleStatus capability {n:?} — skipped")
                    }
                }
            }
            // Apple's own rule (spec notes, C-0): the unified RangeWarning is mutually exclusive with
            // the per-engine RangeWarning* flags — "Do not include if vehicle reports unified range
            // warning for all EngineTypes." That is imperative, not advisory, so we REFUSE the
            // combination rather than relaying it. docs/carplay/04_CAPABILITIES_AND_CONFIG.md directive 2 gives the app the choice among
            // VALID options; it does not make the box a conduit for output Apple's table forbids (the
            // same reason the pushed metadata path refuses `rx-only`). Keeping the unified flag is the
            // conservative half: it is the one Apple says covers every engine type.
            use crate::message::vehicle_status as sub;
            const PER_ENGINE: [u16; 4] = [
                sub::RANGE_WARNING_GASOLINE,
                sub::RANGE_WARNING_DIESEL,
                sub::RANGE_WARNING_ELECTRIC,
                sub::RANGE_WARNING_CNG,
            ];
            if caps.contains(&sub::RANGE_WARNING) && caps.iter().any(|c| PER_ENGINE.contains(c)) {
                eprintln!(
                    "[iap2-config] vehicleStatus declares BOTH rangeWarning and a per-engine \
                     rangeWarning*; Apple's spec forbids that combination — dropping the per-engine \
                     flags and keeping the unified rangeWarning"
                );
                caps.retain(|c| !PER_ENGINE.contains(c));
            }
            // A per-engine range/warning flag for an engine type the vehicle does not declare
            // contradicts Apple's note ("Include for vehicles that support <X> EngineType"). Warn
            // only — the owner may be mid-edit, and this is not malformed, just inconsistent.
            for (flag, engine, label) in [
                (sub::RANGE_ELECTRIC, crate::spec::ident::vehicle_info::ENGINE_TYPE_ELECTRIC, "electric"),
                (sub::RANGE_DIESEL, crate::spec::ident::vehicle_info::ENGINE_TYPE_DIESEL, "diesel"),
                (sub::RANGE_CNG, crate::spec::ident::vehicle_info::ENGINE_TYPE_CNG, "cng"),
            ] {
                if caps.contains(&flag) && !id.engine_types.contains(&engine) {
                    eprintln!(
                        "[iap2-config] vehicleStatus declares a {label} range flag but engineTypes \
                         does not include {label} — emitting as configured"
                    );
                }
            }
            // Ascending by sub id, for the SAME reason connectors are sorted above: config order
            // must not decide wire order. The app's capability list is a UI grouping (range flags
            // together, then temperatures, ...), which maps to sub ids 3, 9, 10, 11, 12, 6, 13 ...
            // — emitting in that order would put sub 6 after sub 12. Framing is box-owned; only the
            // VALUES are the app's to choose.
            caps.sort_unstable();
            // PRESENCE of the block arms param 21, even with an empty capability list: an empty
            // component is a meaningful (if minimal) declaration, and silently dropping it would
            // hide a config mistake.
            id.status_caps = Some(caps);
        }
        id
    }

    /// Read + parse the pushed config. Returns the default on ANY failure (absent file, oversize,
    /// unreadable, malformed YAML) — a bad document must never be worse than no document, since the
    /// compiled `proven` floor behind it is the device-accepted baseline.
    pub fn load() -> Self {
        Self::load_from(CFG_FILE)
    }

    /// Parse bytes already in hand. Preferred by any caller that has just read the document itself
    /// (airplayd reads it per control connection and reports a CRC over exactly those bytes) — a
    /// second read could see a different generation than the one that CRC describes.
    pub fn from_slice(bytes: &[u8]) -> Self {
        if bytes.is_empty() {
            return Self::default(); // empty == absent, not a parse error
        }
        match serde_yaml::from_slice::<Self>(bytes) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[iap2-config] pushed config parse failed ({e}) — using defaults");
                Self::default()
            }
        }
    }

    /// [`Self::load`] against an explicit path (the test/bench seam).
    pub fn load_from(path: &str) -> Self {
        match std::fs::metadata(path) {
            Ok(m) if m.len() > MAX_CFG_BYTES => {
                eprintln!("[iap2-config] {path} is {} B (> {MAX_CFG_BYTES}) — ignoring", m.len());
                return Self::default();
            }
            Ok(_) => {}
            Err(_) => return Self::default(), // absent = the normal idle case, not an error
        }
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[iap2-config] {path} unreadable ({e}) — using defaults");
                return Self::default();
            }
        };
        match serde_yaml::from_slice::<Self>(&bytes) {
            Ok(c) => c,
            Err(e) => {
                // Say the CONSEQUENCE, not just the failure (docs/carplay/04_CAPABILITIES_AND_CONFIG.md #26). This parser is
                // independent of the receiver's — the receiver can accept a document this one
                // rejects — so "using defaults" here means the pushed METADATA TIER is dropped and
                // the compiled floor declares instead. That is a wire-visible change to the
                // Identify, and it previously read like routine housekeeping.
                eprintln!(
                    "[iap2-config] *** {path} REJECTED ({e}) — the pushed metadata tier and skip \
                     list are DISCARDED; the compiled floor will declare instead ***"
                );
                Self::default()
            }
        }
    }

    /// Arm the process-wide metadata policy from this config, if it names a tier.
    ///
    /// Returns whether the policy was armed by this call. Safe (and expected) to call more than
    /// once — [`crate::features::arm_pushed_policy`] is first-arm-wins, so a later call cannot
    /// change a declaration out from under a live link.
    pub fn arm_metadata_policy(&self) -> bool {
        match self.metadata.tier.as_deref() {
            Some(t) if !t.trim().is_empty() => {
                crate::features::arm_pushed_policy(t, &self.metadata.skip)
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared-fixture guard: this is the exact `metadata:` shape the macOS app emits, asserted
    /// here so the iAP2 reader and the app's emitter can never drift apart silently.
    const APP_EMITTED: &str = "name: CarLink Widescreen\nmetadata:\n  tier: extended\n  skip: [call_history]\naccessoryConfig:\n  enablesHEVC: true\n";

    #[test]
    fn parses_the_app_emitted_metadata_section() {
        let c: Iap2Config = serde_yaml::from_slice(APP_EMITTED.as_bytes()).unwrap();
        assert_eq!(c.metadata.tier.as_deref(), Some("extended"));
        assert_eq!(c.metadata.skip, vec!["call_history".to_string()]);
    }

    /// The multi-name rendering the app produces for two or more skips (`", "` join inside a flow
    /// sequence). Pinned separately because the single-name fixture above cannot catch a change to
    /// the separator — and this is the app/box drift guard.
    #[test]
    fn parses_the_app_emitted_multi_name_skip() {
        let c: Iap2Config =
            serde_yaml::from_slice(b"metadata:\n  tier: all\n  skip: [call_history, power]\n").unwrap();
        assert_eq!(c.metadata.tier.as_deref(), Some("all"));
        assert_eq!(c.metadata.skip, vec!["call_history".to_string(), "power".to_string()]);
    }

    #[test]
    fn unrelated_keys_and_absent_section_are_harmless() {
        // A full VehicleConfig with no metadata section: serde ignores the receiver-facing keys.
        let y = "name: X\ndisplayPanelsConfig:\n  mainDisplayPanel:\n    pixelDimensions: {width: 1920, height: 720}\n";
        let c: Iap2Config = serde_yaml::from_slice(y.as_bytes()).unwrap();
        assert!(c.metadata.tier.is_none());
        assert!(c.metadata.skip.is_empty());
        assert!(!c.arm_metadata_policy()); // nothing to arm
    }

    #[test]
    fn missing_file_and_garbage_yield_defaults() {
        let c = Iap2Config::load_from("/nonexistent/carplay_cfg.yaml");
        assert!(c.metadata.tier.is_none());

        let dir = std::env::temp_dir().join("iap2cfg_garbage_test");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("bad.yaml");
        std::fs::write(&p, b"\t\tthis: is: not: yaml: [[[").unwrap();
        let c = Iap2Config::load_from(p.to_str().unwrap());
        assert!(c.metadata.tier.is_none());
        let _ = std::fs::remove_file(&p);
    }

    // ---- C6: vehicle identity (iapConfig) ----

    /// The shape the app emits for a plug-in hybrid. Doubles as the app/box drift guard for the
    /// `iapConfig` subtree, the same role `APP_EMITTED` plays for `metadata:`.
    ///
    /// NOTE the `vehicleStatus:` half is DELIBERATELY ahead of the app: since C-2 the macOS app
    /// gates that block behind a compile-time `vehicleStatusUnlocked = false` (it must not ship
    /// before C-4 declares 0xA100/0xA101/0xA102), so the app cannot currently emit it. This fixture
    /// pins the shape in advance on purpose — do not "reconcile" the difference by deleting it. The
    /// `vehicleInfo:` half is byte-faithful to today's emitter.
    const APP_EMITTED_IAPCONFIG: &str = "\
name: CarLink Widescreen
iapConfig:
  vehicleInfo:
    engineTypes: [gasoline, electric]
    chargingConnectors:
      - type: ccs2
        powerWatts: 150000
      - type: nacs_dc
  vehicleStatus:
    capabilities: [range, rangeElectric, electricChargeInfo]
";

    #[test]
    fn parses_the_app_emitted_iap_config() {
        use crate::message::vehicle_status as vs;
        use crate::spec::ident::vehicle_info as sub;
        let c: Iap2Config = serde_yaml::from_slice(APP_EMITTED_IAPCONFIG.as_bytes()).unwrap();
        let id = c.vehicle_identity();
        assert_eq!(
            id.engine_types,
            vec![sub::ENGINE_TYPE_GASOLINE, sub::ENGINE_TYPE_ELECTRIC]
        );
        assert_eq!(
            id.connectors,
            vec![
                (sub::SUPPORTED_CHARGING_CONNECTORS_CCS2, Some(150_000)),
                (sub::SUPPORTED_CHARGING_CONNECTORS_NACS_DC_, None),
            ]
        );
        assert_eq!(
            id.status_caps,
            Some(vec![vs::RANGE, vs::RANGE_ELECTRIC, vs::ELECTRIC_CHARGE_INFO])
        );
        assert!(!id.is_baseline());
    }

    /// A document with no `iapConfig:` must resolve to the exact pre-C6 emission. This is what makes
    /// the whole workstream absent-off, and it is the config-level rollback path (docs/carplay/04_CAPABILITIES_AND_CONFIG.md): push a
    /// YAML without the block and the next Identify is byte-baseline, with no rebuild.
    #[test]
    fn absent_iap_config_is_the_baseline() {
        let c: Iap2Config = serde_yaml::from_slice(b"name: X\n").unwrap();
        assert!(c.vehicle_identity().is_baseline());
        assert!(Iap2Config::default().vehicle_identity().is_baseline());
    }

    /// A typo must cost the owner a skipped field, never a handshake: `0x1D03` is unrecoverable
    /// within a session, so an unresolvable engine list falls back to the gasoline baseline rather
    /// than emitting an Identify with zero EngineType subs.
    #[test]
    fn unknown_names_are_skipped_not_fatal() {
        use crate::spec::ident::vehicle_info as sub;
        let y = "iapConfig:\n  vehicleInfo:\n    engineTypes: [plutonium]\n    chargingConnectors:\n      - type: banana\n        powerWatts: 5\n";
        let c: Iap2Config = serde_yaml::from_slice(y.as_bytes()).unwrap();
        let id = c.vehicle_identity();
        assert_eq!(id.engine_types, vec![sub::ENGINE_TYPE_GASOLINE], "baseline kept");
        assert!(id.connectors.is_empty(), "unknown connector dropped");
        // One good + one bad keeps the good one only.
        let y2 = "iapConfig:\n  vehicleInfo:\n    engineTypes: [plutonium, electric]\n";
        let c2: Iap2Config = serde_yaml::from_slice(y2.as_bytes()).unwrap();
        assert_eq!(
            c2.vehicle_identity().engine_types,
            vec![sub::ENGINE_TYPE_ELECTRIC]
        );
    }

    /// PRESENCE of `vehicleStatus:` arms param 21 even with no capabilities — an empty component is
    /// a real (if minimal) declaration, and silently dropping it would hide a config mistake.
    #[test]
    fn vehicle_status_presence_arms_param21_even_when_empty() {
        let c: Iap2Config =
            serde_yaml::from_slice(b"iapConfig:\n  vehicleStatus:\n    capabilities: []\n").unwrap();
        assert_eq!(c.vehicle_identity().status_caps, Some(Vec::new()));
        // ...while its ABSENCE leaves param 21 off, which is the session-critical default.
        let c2: Iap2Config = serde_yaml::from_slice(b"iapConfig:\n  vehicleInfo:\n    engineTypes: [diesel]\n").unwrap();
        assert_eq!(c2.vehicle_identity().status_caps, None);
    }

    /// Names are matched case-insensitively and tolerate the separators a human might type.
    #[test]
    fn name_matching_is_forgiving() {
        use crate::spec::ident::vehicle_info as sub;
        let y = "iapConfig:\n  vehicleInfo:\n    engineTypes: [ Electric ]\n    chargingConnectors:\n      - type: \"NACS-DC\"\n";
        let c: Iap2Config = serde_yaml::from_slice(y.as_bytes()).unwrap();
        let id = c.vehicle_identity();
        assert_eq!(id.engine_types, vec![sub::ENGINE_TYPE_ELECTRIC]);
        assert_eq!(id.connectors, vec![(sub::SUPPORTED_CHARGING_CONNECTORS_NACS_DC_, None)]);
    }

    /// THE cardinality guard. Apple marks each `PowerForConnectorType*` sub `[optional]` = zero-or-one,
    /// so two rows of the same connector would emit sub 13 twice with contradictory values — malformed,
    /// on the one message whose rejection is unrecoverable. This module's own forgiving name matching
    /// manufactures the case from an ordinary copy-paste (`ccs2` / `CCS2` / `CCS-2` all fold together).
    #[test]
    fn duplicate_connectors_are_deduped_first_row_wins() {
        use crate::spec::ident::vehicle_info as sub;
        // Plain repetition of the SAME spelling: this must be caught by dedup, not by anything else.
        let y = "iapConfig:\n  vehicleInfo:\n    chargingConnectors:\n      - type: ccs2\n        powerWatts: 150000\n      - type: ccs2\n        powerWatts: 50000\n";
        let c: Iap2Config = serde_yaml::from_slice(y.as_bytes()).unwrap();
        assert_eq!(
            c.vehicle_identity().connectors,
            vec![(sub::SUPPORTED_CHARGING_CONNECTORS_CCS2, Some(150_000))],
            "the second row for the same connector must be dropped, first rating kept"
        );

        // And via DIFFERENT spellings, which is the copy-paste case the forgiving matcher creates.
        // An earlier revision passed this test for the wrong reason: `CCS-2` normalized to `ccs_2`,
        // matched no arm, and was dropped as UNKNOWN rather than deduped. Mutation testing caught it.
        //
        // The separator spelling is FIRST on purpose. With it second, "collapsed by dedup" and
        // "discarded as unknown" both yield `[(CCS2, Some(150_000))]`, so the assertion could not
        // tell them apart and the test still passed under a reverted normalizer (R4 mutation D3b).
        // Leading with `CCS-2` makes the two outcomes differ in the surviving power rating:
        // 150 kW if it resolved and won as the first row, 50 kW if it was silently dropped.
        let y2 = "iapConfig:\n  vehicleInfo:\n    chargingConnectors:\n      - type: \"CCS-2\"\n        powerWatts: 150000\n      - type: ccs2\n        powerWatts: 50000\n      - type: \"ccs 2\"\n";
        let c2: Iap2Config = serde_yaml::from_slice(y2.as_bytes()).unwrap();
        assert_eq!(
            c2.vehicle_identity().connectors,
            vec![(sub::SUPPORTED_CHARGING_CONNECTORS_CCS2, Some(150_000))],
            "every spelling of one connector must collapse to a single enum"
        );
    }

    /// Every separator spelling of every connector name resolves — the regression guard for the
    /// `CCS-2` -> `ccs_2` bug above, checked across the whole vocabulary rather than one example.
    #[test]
    fn connector_spellings_all_resolve() {
        use crate::spec::ident::vehicle_info as sub;
        for (spellings, want) in [
            (vec!["ccs1", "CCS-1", "ccs 1", "CCS_1"], sub::SUPPORTED_CHARGING_CONNECTORS_CCS1),
            (vec!["ccs2", "CCS-2", "ccs 2"], sub::SUPPORTED_CHARGING_CONNECTORS_CCS2),
            (vec!["j1772", "J-1772"], sub::SUPPORTED_CHARGING_CONNECTORS_J1772),
            (vec!["chademo", "CHAdeMO"], sub::SUPPORTED_CHARGING_CONNECTORS_CH_ADE_MO),
            (vec!["mennekes", "Mennekes"], sub::SUPPORTED_CHARGING_CONNECTORS_MENNEKES),
            (vec!["gbt_dc", "GBT-DC", "gbt dc"], sub::SUPPORTED_CHARGING_CONNECTORS_GB_T_DC_),
            (vec!["gbt_ac", "GBT-AC"], sub::SUPPORTED_CHARGING_CONNECTORS_GB_T_AC_),
            (vec!["nacs_dc", "NACS-DC"], sub::SUPPORTED_CHARGING_CONNECTORS_NACS_DC_),
            (vec!["nacs_ac", "NACS-AC"], sub::SUPPORTED_CHARGING_CONNECTORS_NACS_AC_),
        ] {
            for s in spellings {
                assert_eq!(connector_enum(s), Some(want), "spelling {s:?} failed to resolve");
            }
        }
        assert_eq!(connector_enum("banana"), None);
    }

    /// Same cardinality rule for param 21's flags (all `[optional]`) and, for consistency, engines.
    #[test]
    fn duplicate_capabilities_and_engines_are_deduped() {
        use crate::message::vehicle_status as vs;
        use crate::spec::ident::vehicle_info as sub;
        let y = "iapConfig:\n  vehicleInfo:\n    engineTypes: [electric, electric, ev]\n  vehicleStatus:\n    capabilities: [range, range, rangeElectric]\n";
        let c: Iap2Config = serde_yaml::from_slice(y.as_bytes()).unwrap();
        let id = c.vehicle_identity();
        assert_eq!(id.engine_types, vec![sub::ENGINE_TYPE_ELECTRIC]);
        assert_eq!(id.status_caps, Some(vec![vs::RANGE, vs::RANGE_ELECTRIC]));
    }

    /// Config order must not decide wire order: a Tesla owner listing `nacs_dc` first would otherwise
    /// emit power sub 19 before sub 13. Sorting at resolve time keeps the group ascending.
    #[test]
    fn connectors_are_sorted_so_the_wire_order_is_ascending() {
        use crate::spec::ident::vehicle_info as sub;
        let y = "iapConfig:\n  vehicleInfo:\n    chargingConnectors:\n      - type: nacs_dc\n        powerWatts: 250000\n      - type: ccs2\n        powerWatts: 150000\n";
        let c: Iap2Config = serde_yaml::from_slice(y.as_bytes()).unwrap();
        let got = c.vehicle_identity().connectors;
        assert_eq!(
            got,
            vec![
                (sub::SUPPORTED_CHARGING_CONNECTORS_CCS2, Some(150_000)),
                (sub::SUPPORTED_CHARGING_CONNECTORS_NACS_DC_, Some(250_000)),
            ]
        );
        // ...and therefore the power subs ascend too.
        let subs: Vec<u16> = got
            .iter()
            .filter_map(|(e, w)| w.map(|_| power_sub_for_connector(*e).unwrap()))
            .collect();
        assert!(subs.windows(2).all(|w| w[0] < w[1]), "power subs must ascend: {subs:?}");
    }

    /// Sub 10 is free text landing in a NUL-terminated TLV, so it is validated to `#RRGGBB` and
    /// dropped otherwise. The NUL case is the one that actually corrupts the frame.
    #[test]
    fn color_hex_is_validated_and_normalized() {
        let id = |y: &str| {
            serde_yaml::from_slice::<Iap2Config>(y.as_bytes())
                .unwrap()
                .vehicle_identity()
                .color_hex
        };
        assert_eq!(
            id("iapConfig:\n  vehicleInfo:\n    vehicleColorHexCode: \"1a2b3c\"\n").as_deref(),
            Some("#1A2B3C"),
            "the leading # is optional on input and always emitted"
        );
        assert_eq!(
            id("iapConfig:\n  vehicleInfo:\n    vehicleColorHexCode: \"#1a2b3c\"\n").as_deref(),
            Some("#1A2B3C")
        );
        for bad in [
            "\"ab\\0cd\"",       // embedded NUL — would desync a NUL-terminated TLV
            "\"#12345\"",         // too short
            "\"#1234567\"",       // too long
            "\"#GGHHII\"",        // not hex
            "\"\"",               // empty
        ] {
            let y = format!("iapConfig:\n  vehicleInfo:\n    vehicleColorHexCode: {bad}\n");
            assert_eq!(id(&y), None, "expected {bad} to be dropped");
        }
    }

    /// Apple's note makes the unified RangeWarning mutually exclusive with the per-engine flags, and
    /// it is imperative ("Do not include if..."). We drop the per-engine half rather than relay a
    /// combination Apple's own table forbids — the same stance as refusing a pushed `rx-only` tier.
    #[test]
    fn conflicting_range_warnings_drop_the_per_engine_flags() {
        use crate::message::vehicle_status as vs;
        let y = "iapConfig:\n  vehicleStatus:\n    capabilities: [range, rangeWarning, rangeWarningElectric, rangeWarningDiesel]\n";
        let c: Iap2Config = serde_yaml::from_slice(y.as_bytes()).unwrap();
        assert_eq!(
            c.vehicle_identity().status_caps,
            Some(vec![vs::RANGE, vs::RANGE_WARNING]),
            "unified kept, per-engine dropped"
        );
        // Per-engine flags ALONE are legal and must survive untouched.
        let y2 = "iapConfig:\n  vehicleStatus:\n    capabilities: [rangeWarningElectric]\n";
        let c2: Iap2Config = serde_yaml::from_slice(y2.as_bytes()).unwrap();
        assert_eq!(c2.vehicle_identity().status_caps, Some(vec![vs::RANGE_WARNING_ELECTRIC]));
    }

    /// Dedup is what BOUNDS the emission: no pushed list can exceed Apple's own vocabulary, so
    /// param 20 cannot be driven past the wired ~4 KB MaxRcvPacketLength or the 64 KB u16 group
    /// ceiling (whose `debug_assert!` guards are compiled out of the box's release build).
    #[test]
    fn absurd_repetition_cannot_grow_the_emission() {
        let engines = vec!["electric"; 20_000].join(", ");
        let conns = "      - type: ccs2\n        powerWatts: 1\n".repeat(5_000);
        let y = format!(
            "iapConfig:\n  vehicleInfo:\n    engineTypes: [{engines}]\n    chargingConnectors:\n{conns}"
        );
        let c: Iap2Config = serde_yaml::from_slice(y.as_bytes()).unwrap();
        let id = c.vehicle_identity();
        assert_eq!(id.engine_types.len(), 1, "20k duplicate engines collapse to one");
        assert_eq!(id.connectors.len(), 1, "5k duplicate connectors collapse to one");
        // Hard ceilings, independent of the input: Apple's vocabulary is 4 engines, 9 connectors.
        assert!(id.engine_types.len() <= 4 && id.connectors.len() <= 9);
    }

    /// THE BYTE CEILING, asserted in CI rather than defended at runtime.
    ///
    /// Deliberately chosen over a release-mode length check inside the emitter: that would cost every
    /// Identify a check for a condition dedup + colour validation already make structurally
    /// impossible, and it would fail on the box mid-handshake instead of here. This fails the day
    /// someone adds an UNBOUNDED field to param 20/21 — which is the real residual risk, since the
    /// `Tlv` length guards are `debug_assert!`s and are compiled out of the box's release profile.
    ///
    /// The maximal LEGAL pushed config: every engine type, every connector at max power, a colour,
    /// and the full capability list. Measured 501 B wired vs a 279 B baseline — ~12% of the wired
    /// ~4 KB `MaxRcvPacketLength`. The 1 KB bound leaves headroom without being a rubber stamp.
    ///
    /// NOTE `name` is NOT bounded here and is not part of `iapConfig`. C-2's `accessoryName` makes it
    /// app-pushed and lands it in THREE TLV positions (param 0, param 20 sub 1, param 20 sub 6, plus
    /// param 21 sub 1 once armed) — bounding it at the iap2d call site is a C-2/C-3 prerequisite.
    #[test]
    fn a_maximal_legal_config_stays_far_inside_the_packet_budget() {
        use crate::message::{build_ident_info, build_ident_info_with, TransportComponent};
        let conns: String = ["ccs1", "ccs2", "j1772", "chademo", "mennekes", "gbt_dc", "gbt_ac", "nacs_dc", "nacs_ac"]
            .iter()
            .map(|t| format!("      - type: {t}\n        powerWatts: 4294967295\n"))
            .collect();
        let y = format!(
            "iapConfig:\n  vehicleInfo:\n    engineTypes: [gasoline, diesel, electric, cng]\n    \
             vehicleColorHexCode: \"#1A2B3C\"\n    chargingConnectors:\n{conns}  vehicleStatus:\n    \
             capabilities: [range, outsideTemperature, insideTemperature, rangeWarning, rangeGasoline, \
             rangeDiesel, rangeElectric, rangeCNG, wiperStatus, barometricPressure, alerts, \
             passengerSeatStatus, electricChargeInfo, maxRangeInfo]\n"
        );
        let transport = TransportComponent::Usb { cp_iface: 1 };
        let baseline = build_ident_info("CarLink", transport, true);
        let emit = |doc: &str| {
            let id = serde_yaml::from_slice::<Iap2Config>(doc.as_bytes())
                .unwrap()
                .vehicle_identity();
            build_ident_info_with("CarLink", transport, true, &id)
        };

        let maximal = emit(&y);
        assert!(
            maximal.len() < 1024,
            "a maximal legal pushed config must stay far inside the wired packet budget, got {} B \
             (baseline {} B)",
            maximal.len(),
            baseline.len()
        );
        assert!(maximal.len() > baseline.len(), "the maximal config must actually emit more");

        // THE MUTATION-KILLING HALF, and the reason this test earns its keep. The same maximal config
        // with a 60 KB colour: validation must DROP it, so the emission stays at the ceiling above.
        // If any future edit lets an unbounded string through — the exact residual risk, since the
        // `Tlv` length guards are `debug_assert!`s compiled out of the box's release build — this
        // assertion fires in CI instead of the box emitting a silently truncated 0x1D01 mid-handshake.
        let hostile = y.replace(
            "vehicleColorHexCode: \"#1A2B3C\"",
            &format!("vehicleColorHexCode: \"{}\"", "A".repeat(60_000)),
        );
        assert_eq!(
            emit(&hostile).len(),
            maximal.len() - 12, // the dropped colour sub: 4-byte TLV header + "#1A2B3C\0"
            "an oversized value must be dropped, not emitted — the packet budget is not negotiable"
        );
    }

    /// THE APP→BOX VOCABULARY GUARD. Every name the macOS app's Vehicle Identity panel can put in a
    /// pushed document must resolve to a wire id here.
    ///
    /// These three lists are mirrored from `SettingsWindow.swift` (`engineTypeNames`,
    /// `connectorNames`, `vehicleStatusCapNames`) and MUST be kept in step with it. The app can only
    /// emit names from those static arrays, so if every one resolves, no UI selection can produce a
    /// silently-dropped field. This is the drift class that has bitten this project repeatedly — three
    /// `hidConfig` keys the app emitted were discarded by the box for months before C-2 — and the
    /// capability list was the last app-side vocabulary with no guard on either side.
    #[test]
    fn every_name_the_app_can_emit_resolves_to_a_wire_id() {
        // SettingsWindow.swift: engineTypeNames
        for n in ["gasoline", "diesel", "electric", "cng"] {
            assert!(engine_type_id(n).is_some(), "app engine type {n:?} does not resolve");
        }
        // SettingsWindow.swift: connectorNames
        for n in [
            "ccs1", "ccs2", "j1772", "chademo", "mennekes", "gbt_dc", "gbt_ac", "nacs_dc", "nacs_ac",
        ] {
            assert!(connector_enum(n).is_some(), "app connector {n:?} does not resolve");
            assert!(
                power_sub_for_connector(connector_enum(n).unwrap()).is_some(),
                "app connector {n:?} has no power sub"
            );
        }
        // SettingsWindow.swift: vehicleStatusCapNames — all 18.
        let app_caps = [
            "range", "rangeGasoline", "rangeDiesel", "rangeElectric", "rangeCNG",
            "rangeWarning",
            "rangeWarningGasoline", "rangeWarningDiesel", "rangeWarningElectric", "rangeWarningCNG",
            "outsideTemperature", "insideTemperature", "wiperStatus", "barometricPressure",
            "alerts", "passengerSeatStatus", "electricChargeInfo", "maxRangeInfo",
        ];
        assert_eq!(app_caps.len(), 18, "the app's capability list changed size — re-mirror it");
        for n in app_caps {
            assert!(status_cap_id(n).is_some(), "app capability {n:?} does not resolve");
        }
        // ...and the ids are distinct, so no two app names collapse onto one flag.
        let mut ids: Vec<u16> = app_caps.iter().filter_map(|n| status_cap_id(n)).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "two app capability names map to the same sub id");
    }

    /// Param-21 capability flags must reach the wire in ASCENDING sub-id order, whatever order the
    /// app lists them in. The app's vocabulary is a UI grouping (all the range flags together, then
    /// temperatures, …) which maps to sub ids 3, 9, 10, 11, 12, 6, 13, … — so emitting in config
    /// order would put sub 6 after sub 12. Same rule, and same reason, as the connector sort: the app
    /// owns the VALUES, the box owns the FRAMING.
    #[test]
    fn status_capabilities_reach_the_wire_in_ascending_sub_order() {
        use crate::message::vehicle_status as vs;
        // Deliberately listed in the app's UI order, which is NOT ascending by sub id.
        let y = "iapConfig:\n  vehicleStatus:\n    capabilities: [range, rangeElectric, rangeWarning, outsideTemperature, maxRangeInfo, alerts]\n";
        let caps = serde_yaml::from_slice::<Iap2Config>(y.as_bytes())
            .unwrap()
            .vehicle_identity()
            .status_caps
            .expect("block present");
        assert_eq!(
            caps,
            vec![
                vs::RANGE,               // 3
                vs::OUTSIDE_TEMPERATURE, // 4
                vs::RANGE_WARNING,       // 6
                vs::RANGE_ELECTRIC,      // 11
                vs::ALERTS,              // 19
                vs::MAX_RANGE_INFO,      // 30
            ]
        );
        assert!(caps.windows(2).all(|w| w[0] < w[1]), "must be strictly ascending: {caps:?}");
    }

    /// Pins the coincidence that Apple's power subs (12-20) run in the same order as the connector
    /// enums (0-8). [`power_sub_for_connector`] is a table precisely so the day this stops holding is
    /// a visible test failure rather than power silently attributed to the wrong connector.
    #[test]
    fn power_sub_matches_the_current_arithmetic() {
        for enum_v in 0u8..=8 {
            assert_eq!(
                power_sub_for_connector(enum_v),
                Some(12 + enum_v as u16),
                "connector enum {enum_v}"
            );
        }
        assert_eq!(power_sub_for_connector(9), None, "unknown connector has no power sub");
    }

    /// An empty/whitespace tier must not reach `arm_pushed_policy` as a "word" at all.
    #[test]
    fn blank_tier_arms_nothing() {
        let c: Iap2Config = serde_yaml::from_slice(b"metadata:\n  tier: \"   \"\n").unwrap();
        assert!(!c.arm_metadata_policy());
    }
}
