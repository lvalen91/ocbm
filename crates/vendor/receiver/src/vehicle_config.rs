//! `VehicleConfig` — the host-authored YAML config that drives the `/info` the box advertises.
//!
//! This mirrors Apple's own **CarPlaySimulator** authoring schema (`CarPlayConfigs.VehicleConfig`,
//! parsed there by Yams; see `ccpa_custom/docs/13` §2 and `reference/carplay_sdk/apple_vehicleconfigs/`).
//! The model is **host-authoritative / ephemeral** (docs/carplay/04_CAPABILITIES_AND_CONFIG.md): the macOS app ships a YAML at OCBM
//! SUBSCRIBE, ocbmd lands it at `/tmp/carplay_cfg.yaml`, and airplayd parses it **per control
//! connection** into a [`DeviceConfig`] before building `/info`. A config push = a fresh session = a
//! re-read `/info`, which is exactly the reconnect class the resolution lever lives in (docs/carplay/06_AV_PIPELINE.md).
//!
//! **Forward-compatible by construction:** serde ignores unknown fields, so the host may send the FULL
//! Apple `VehicleConfig` (viewAreas, HID, accessoryConfig, altVideoStreams, …) today and the box reads
//! only the fields it currently consumes. As the box learns to act on more of the schema (HEVC,
//! view/safe areas, cluster/altScreen, touch geometry), add the field here + map it in [`VehicleConfig::apply`].
//!
//! **What is applied today:**
//! - `displayPanelsConfig.mainDisplayPanel.pixelDimensions.{width,height}` — the coded-resolution
//!   lever the iPhone reads from `/info` `displays[].widthPixels/heightPixels`.
//! - `accessoryConfig.enablesHEVC` (2026-07-12, user directive) — parsed here; the CALLER (airplayd)
//!   arms the receiver's `CARPLAY_HEVC` lever from it per connection (hevcInfo in `/info` +
//!   `enabledFeatures:["hevc"]` in the SETUP phase-1 response). The host app's decoder is dual-codec
//!   with an hvcC pre-warm path, so an iOS switch to HEVC is consumed end-to-end. See `info.rs`'s
//!   HEVC caveats — wired HEVC is an A/B under test; rollback = push a config with `enablesHEVC: false`.
//!
//! Everything else parses but is intentionally not yet mapped.

use serde::Deserialize;

use crate::info::{audio_format_bit, audio_preset, AudioFormatSpec, DeviceConfig};

/// Root of the authoring YAML. Only the fields the box consumes (or is about to) are declared; any
/// other Apple `VehicleConfig` key present in the YAML is ignored by serde.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VehicleConfig {
    /// Authoring name of the template (e.g. "Widescreen"). This is config metadata, NOT the accessory's
    /// advertised display name — so it is deliberately not mapped onto `DeviceConfig.name`.
    #[serde(default)]
    pub name: String,
    /// `accessoryName` — the name the OWNER gives this box, which the iPhone displays (docs/carplay/04_CAPABILITIES_AND_CONFIG.md C7).
    /// OUR extension, not a stock Apple `VehicleConfig` key, and deliberately distinct from [`name`]
    /// above: that one names the TEMPLATE, this one names the ACCESSORY. Conflating them is the
    /// mistake docs/carplay/04_CAPABILITIES_AND_CONFIG.md warns about.
    ///
    /// PARSE-ONLY in C-2, exactly as C-1 landed the vehicle identity: nothing reads it yet, so the
    /// wire is unchanged. Applying it is C-6, and it is not a free change — it alters the advertised
    /// `/info` name, the Bonjour instance name AND iAP2 params 0/20, so it needs its own hardware
    /// session with the `idevicesyslog -p accessoryd` watch (name growth changes TLV lengths).
    ///
    /// ⚠️ C-6 PREREQUISITE — BOUND IT AT THE CALL SITE. This is the next unbounded free string
    /// heading for `Tlv::str`, and unlike the vehicle colour it lands in THREE TLV positions (param 0
    /// `Name`, param 20 sub 1 `Name`, param 20 sub 6 `DisplayName`, plus param 21 sub 1 once that
    /// arms). The `Tlv` length guards are `debug_assert!`s and are compiled OUT of the box's release
    /// build, so an over-long name silently truncates a `0x1D01` whose rejection is unrecoverable
    /// within a session. A 63-byte cap covers the mDNS instance label but NOT the iAP2 params.
    #[serde(default, rename = "accessoryName")]
    pub accessory_name: Option<String>,
    #[serde(default, rename = "displayPanelsConfig")]
    pub display_panels_config: DisplayPanelsConfig,
    #[serde(default, rename = "videoStreamsConfig")]
    pub video_streams_config: VideoStreamsConfig,
    #[serde(default, rename = "accessoryConfig")]
    pub accessory_config: AccessoryConfig,
    /// Apple `limitedUIConfig` — WHICH elements iOS restricts in limited-UI mode. Top-level, a
    /// sibling of `accessoryConfig`, matching Apple's own `Config.init(… accessoryConfig:,
    /// oemIconConfig:, limitedUIConfig:, …)`. Absent/all-false = emit nothing and let iOS use its
    /// default set. See [`LimitedUiConfig`]; the runtime on/off is a separate `/command`, not here.
    #[serde(default, rename = "limitedUIConfig")]
    pub limited_ui_config: LimitedUiConfig,
    /// Apple `oemIconConfig` — the vehicle-maker's logo on the CarPlay home screen (see [`OemIconConfig`]).
    /// Emitted in `/info` as `oemIcons`/`oemIconLabel`/`oemIconVisible` (AirPlayCommon.h). STATIC config,
    /// no runtime command; absent = emit nothing so `/info` stays byte-identical.
    #[serde(default, rename = "oemIconConfig")]
    pub oem_icon_config: OemIconConfig,
    /// Audio capability config (our extension, not a stock Apple `VehicleConfig` key): the exact
    /// `audioFormats` set the box advertises. Lets a user author any HU audio configuration for testing.
    /// Absent = the transport-gated default (PCM wired / 8-entry AAC wireless) is kept. See [`AudioConfig`].
    #[serde(default)]
    pub audio: AudioConfig,
}

/// One resolution of the OEM icon — a base64 PNG plus its pixel size.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct OemIconImage {
    #[serde(default, rename = "imageBase64")]
    pub image_base64: String,
    #[serde(default)]
    pub width: i64,
    #[serde(default)]
    pub height: i64,
}

/// Apple `oemIconConfig` — the vehicle-maker's logo shown on the CarPlay home screen. Delivered in
/// `/info` (`AirPlayReceiverServer.c` emits `oemIcons`/`oemIconLabel`/`oemIconVisible` inside the
/// in-session guard). STATIC config — no runtime command exists in R14G17. The PNG rides base64 in the
/// YAML (mac→box single-doc OCBM delivery has no separate asset channel); an empty `imageBase64` means
/// no icon, and the whole `/info` block is then omitted (byte-identical for users who set none).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct OemIconConfig {
    /// Multi-resolution icon set. Apple's AppStub emits 120/180/256 ("for each required size"); iOS
    /// renders only the label for a single-size set (device-confirmed 2026-08-02), so the host scales the
    /// source to all three. When non-empty this REPLACES the legacy single-image fields below.
    #[serde(default)]
    pub images: Vec<OemIconImage>,
    /// Legacy single-image base64 PNG (used only when `images` is empty). Empty ⇒ nothing emitted.
    #[serde(default, rename = "imageBase64")]
    pub image_base64: String,
    #[serde(default)]
    pub width: i64,
    #[serde(default)]
    pub height: i64,
    /// Optional text label shown with the icon (`oemIconLabel`).
    #[serde(default)]
    pub label: String,
    /// Whether iOS displays the icon on the home screen (`oemIconVisible`).
    #[serde(default)]
    pub visible: bool,
}

/// Decode standard base64 (RFC 4648, `+/` alphabet, optional `=` padding, whitespace ignored) to bytes.
/// Dependency-free — the box crates avoid pulling a base64 dep for one small PNG.
///
/// Deliberately LENIENT: it rejects only a byte outside the alphabet (returning empty, after which the
/// caller emits no icon — exactly the unconfigured behavior). `=` is skipped wherever it appears, an
/// input length not a multiple of 4 is accepted, and trailing bits are dropped. The app's own fixtures
/// rely on that (`"iVBORw0KGgp="`), and a corrupt PNG is rejected by iOS rather than by the box.
pub fn decode_base64(s: &str) -> Vec<u8> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc = 0u32;
    let mut nbits = 0u32;
    for &c in s.as_bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let Some(v) = val(c) else { return Vec::new() };
        acc = (acc << 6) | v as u32;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push((acc >> nbits) as u8);
        }
    }
    out
}

/// The YAML `audio:` section — the declarative CarPlay audio capability set. Per-transport
/// `wired:`/`wireless:` arms (each `{preset, formats}`) win over the flat keys; within an arm or
/// the flat form, an explicit non-empty `formats` list FULLY REPLACES the advertised set, else a
/// named `preset` baseline. Nothing resolved = keep the caller's default.
///
/// ```yaml
/// audio:
///   preset: wireless_8            # optional named baseline (wired_pcm | wireless_8)
///   formats:                      # optional explicit list — when present, replaces preset/default
///     - {type: 102, audioType: media, out: aac_lc_48k_stereo}
///     - {type: 100, audioType: speechRecognition, in: aac_eld_16k_mono, out: aac_eld_16k_mono}
///     - {type: 100, audioType: compatibility, in: pcm_16k_mono, out: "pcm_48k_stereo|pcm_16k_mono"}
///   wired: {preset: wired_pcm}    # per-transport arms — the session's transport picks one
///   wireless: {preset: wireless_8}
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AudioConfig {
    /// Named baseline: `wired_pcm` | `wireless_8` (alias `wireless_full`). Unknown = ignored (default kept).
    #[serde(default)]
    pub preset: Option<String>,
    /// Explicit advertised entries. When non-empty this REPLACES the preset/default entirely (fully
    /// declarative). Each entry `{type, audioType?, in?, out}` — see [`AudioFormatEntry`].
    #[serde(default)]
    pub formats: Vec<AudioFormatEntry>,
    /// Per-transport subsections (docs/carplay/04_CAPABILITIES_AND_CONFIG.md workstream B5): one pushed YAML serves whichever
    /// transport connects next, and wired needs the PCM catch-all while wireless needs the AAC set
    /// (advertising AAC over USB kills wired audio, docs/carplay/06_AV_PIPELINE.md) — so the app pushes BOTH arms and the
    /// box presents the matching one. Precedence: matched subsection > flat `preset`/`formats`
    /// (legacy, both transports) > transport-gated default (interim floor per docs/carplay/04_CAPABILITIES_AND_CONFIG.md).
    #[serde(default)]
    pub wired: Option<AudioSubConfig>,
    #[serde(default)]
    pub wireless: Option<AudioSubConfig>,
}

/// One per-transport arm of the `audio:` section — same `{preset, formats}` shape as the legacy
/// flat form.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AudioSubConfig {
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub formats: Vec<AudioFormatEntry>,
}

/// One YAML `audio.formats[]` entry. `in`/`out` are `|`-joined format NAMES (see
/// [`crate::info::audio_format_bit`] for the full vocabulary), e.g. `out: "aac_lc_48k_stereo"` or
/// `out: "pcm_48k_stereo|pcm_16k_mono"`. `audioType` omitted = the wired PCM catch-all style; `in`
/// omitted/empty = output-only (no mic capture on that stream).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AudioFormatEntry {
    /// `#[serde(default)]` so a `formats[]` entry missing `type` does not fail the WHOLE document —
    /// which is exactly what this file's per-leaf fallback policy forbids. A defaulted 0 is not in
    /// `SERVEABLE_STREAM_TYPES`, so `to_spec` rejects that one entry, loudly and on its own.
    #[serde(default, rename = "type")]
    pub stream_type: i64,
    #[serde(default, rename = "audioType")]
    pub audio_type: Option<String>,
    #[serde(default, rename = "in")]
    pub input: String,
    #[serde(default, rename = "out")]
    pub output: String,
}

impl AudioConfig {
    /// Resolve to the advertised set, or `None` to keep the caller's transport-gated default. The
    /// session transport is read the same way `info::default_audio_formats` reads it (the
    /// `CARPLAY_WIRELESS_AUDIO` env the wireless launcher sets — its role shrinks to transport
    /// indicator; format selection is the app's, docs/carplay/04_CAPABILITIES_AND_CONFIG.md B5). Precedence: matched per-transport
    /// subsection > flat keys > `None` (caller default). Within an arm: explicit `formats` wins
    /// over `preset`; an all-invalid `formats` list is treated as "keep default" (never advertise
    /// an empty `audioFormats`, which fails iOS activation).
    pub fn resolve(&self) -> Option<Vec<AudioFormatSpec>> {
        self.resolve_for(std::env::var("CARPLAY_WIRELESS_AUDIO").is_ok())
    }

    /// Transport-explicit body (separated so tests never mutate process env).
    pub fn resolve_for(&self, wireless: bool) -> Option<Vec<AudioFormatSpec>> {
        let arm = if wireless { &self.wireless } else { &self.wired };
        if let Some(sub) = arm {
            if let Some(specs) = resolve_audio_parts(&sub.preset, &sub.formats) {
                return Some(specs);
            }
            // A present-but-unresolvable subsection falls through to the flat keys, preserving the
            // never-advertise-empty rule.
        }
        resolve_audio_parts(&self.preset, &self.formats)
    }
}

/// Shared resolution body for the flat `audio:` keys and each per-transport arm.
fn resolve_audio_parts(
    preset: &Option<String>,
    formats: &[AudioFormatEntry],
) -> Option<Vec<AudioFormatSpec>> {
    if !formats.is_empty() {
        let specs: Vec<AudioFormatSpec> = formats.iter().filter_map(|e| e.to_spec()).collect();
        if specs.is_empty() {
            eprintln!("[vehicle_config] audio.formats had no valid entries — keeping default audio set");
            return None;
        }
        return Some(specs);
    }
    if let Some(p) = preset {
        if let Some(specs) = audio_preset(p) {
            return Some(specs);
        }
        eprintln!("[vehicle_config] unknown audio.preset '{p}' — keeping default audio set");
    }
    None
}

/// Hand-written because `#[derive(Default)]` gives `false` for every bool, and TWO of these must
/// default `true` to reproduce the pre-gating wire (docs/carplay/04_CAPABILITIES_AND_CONFIG.md #25).
///
/// This is NOT redundant with the `default_true` field attributes, and the difference is exactly the
/// bug a test caught here before it shipped: the per-field attributes apply when `accessoryConfig:`
/// is PRESENT but a key is missing, while THIS impl applies when the whole section is absent —
/// serde builds the struct via `Default` and never consults the field attributes at all. Getting
/// only one of the two right silently stops emitting the appearance keys for every config written
/// before the field existed. Both are required; keep them in agreement.
impl Default for AccessoryConfig {
    fn default() -> Self {
        Self {
            enables_hevc: false,
            enables_view_areas: false,
            enables_corner_masks: false,
            enables_log_transfer: false,
            enables_main_buffered_audio: false,
            app_driven_setup: false,
            enables_ui_appearance: true,
            enables_map_appearance: true,
            enables_focus_transfer: false,
        }
    }
}

/// serde's `#[serde(default)]` for a bool is `false`. Keys whose ABSENCE must reproduce today's wire
/// — i.e. anything the box used to emit unconditionally — need this instead, or adding the field
/// silently turns the feature off for every config that predates it.
fn default_true() -> bool {
    true
}

impl AudioFormatEntry {
    /// Convert to an [`AudioFormatSpec`], or `None` if a format name doesn't resolve (that entry is then
    /// skipped, so one typo can't poison the whole advert).
    ///
    /// ALSO skipped: a `type:` this box cannot actually serve. `stream_type` is a bare `i64` off the
    /// pushed YAML and used to flow straight into `/info audioFormats`, while `session.rs`'s SETUP
    /// dispatch serves only 100..=102 and OMITS anything else from its response. So `type: 107`
    /// (AuxIn) or `type: 103` (MainBuffered) advertised a stream we would then refuse to set up —
    /// advertise-without-serve, the exact hazard the `mainBuffered` comment in `session.rs` warns
    /// about, reachable from an ordinary config push with no code change.
    ///
    /// This is framing, not policy: the app still owns WHICH formats are advertised (docs/carplay/04_CAPABILITIES_AND_CONFIG.md
    /// directive 2), but a value the box cannot honour is not a value, and silently promising iOS a
    /// stream we drop is worse than dropping the entry loudly here. Widen `SERVEABLE_STREAM_TYPES`
    /// in the same commit that teaches `setup_phase2` a new arm — never before it.
    fn to_spec(&self) -> Option<AudioFormatSpec> {
        const SERVEABLE_STREAM_TYPES: &[i64] = &[100, 101, 102];
        if !SERVEABLE_STREAM_TYPES.contains(&self.stream_type) {
            eprintln!(
                "[vehicle_config] audio.formats entry type={} SKIPPED — the SETUP dispatch serves \
                 only {:?}, so advertising it in /info would promise a stream we refuse to set up",
                self.stream_type, SERVEABLE_STREAM_TYPES
            );
            return None;
        }
        let input = parse_format_mask(&self.input)?;
        let output = parse_format_mask(&self.output)?;
        Some(AudioFormatSpec {
            stream_type: self.stream_type,
            audio_type: self.audio_type.clone().filter(|s| !s.is_empty()),
            input_formats: input,
            output_formats: output,
        })
    }
}

/// Parse a `|`-joined list of audio-format names into an OR'd `audioFormat` bitmask. Empty string = 0
/// (omit the key). An unrecognized name returns `None` so the caller can skip the entry (logged).
fn parse_format_mask(s: &str) -> Option<i64> {
    if s.trim().is_empty() {
        return Some(0);
    }
    let mut mask = 0i64;
    for part in s.split('|') {
        match audio_format_bit(part) {
            Some(b) => mask |= b,
            None => {
                eprintln!(
                    "[vehicle_config] unknown audio format name '{}' — skipping entry",
                    part.trim()
                );
                return None;
            }
        }
    }
    Some(mask)
}

/// Apple `AccessoryConfig` (`enables*` toggles — docs/carplay/03_SDK_GROUND_TRUTH.md §2). Only the toggles the box acts on are
/// declared; the rest are serde-ignored like everything else.
#[derive(Debug, Clone, Deserialize)]
pub struct AccessoryConfig {
    /// Advertise + accept HEVC for the screen stream (the box's 2 of the 3 HEVC gates; the host
    /// app's dual-codec decoder is the third). Default false = the proven H.264 path.
    #[serde(default, rename = "enablesHEVC")]
    pub enables_hevc: bool,
    /// Advertise the `viewAreas` capability so iOS honors an inset `safeArea` (keeps CarPlay UI inside
    /// the safe rectangle for curved/occluded panels). Echoed in the SETUP `enabledFeatures` via the
    /// `CARPLAY_VIEWAREAS` lever. A real inset in the YAML also implies this on (see [`VehicleConfig::view_areas_enabled`]).
    #[serde(default, rename = "enablesViewAreas")]
    pub enables_view_areas: bool,
    /// Advertise `cornerMasks` so iOS renders CarPlay with rounded corners and streams the corner-mask
    /// bitmap (docs/carplay/06_AV_PIPELINE.md): a screen-level `/info` flag + the SETUP `enabledFeatures` echo, both via the
    /// `CARPLAY_CORNERMASKS` lever. Device-proven on iOS 27. Default false = plain rectangular projection.
    #[serde(default, rename = "enablesCornerMasks")]
    pub enables_corner_masks: bool,
    /// Advertise `logTransferInfo` in `/info` + echo `"logTransfer"` in the SETUP `enabledFeatures`
    /// (docs/carplay/04_CAPABILITIES_AND_CONFIG.md, Tier-1 advertise/negotiate — iOS pairs the token with its
    /// `enableCarPlayLoggingDataChannel`). The actual archive upload is a separate, phone-initiated
    /// exchange on the RCS LogTransfer DataStream and is not implemented yet; with only this on, the
    /// box is an honest passive responder that never receives a request outside a sysdiagnose.
    #[serde(default, rename = "enablesLogTransfer")]
    pub enables_log_transfer: bool,
    /// Advertise mainBufferedAudio (`mainBufferedInfo` in `/info` + the SETUP `"mainBuffered"`
    /// echo, both via `levers::mainbuffered`). App default OFF (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B4: opt-in per session —
    /// Phase A advertises without serving, so iOS moving media to a buffered stream would silence
    /// it; docs/carplay/04_CAPABILITIES_AND_CONFIG.md). Wired is device-tested benign (iOS negotiates it disabled over USB).
    #[serde(default, rename = "enablesMainBufferedAudio")]
    pub enables_main_buffered_audio: bool,
    /// App-driven SETUP (plan P1, our extension — not a stock Apple `AccessoryConfig` key): the host
    /// app authors the AirPlay SETUP/RECORD responses over the OCBM CH_RTSP relay; the box pre-runs
    /// its own unmodified session as the oracle + fallback (`receiver::relay`). Because this YAML
    /// only ever arrives from the host at SUBSCRIBE, setting it is simultaneously the host's
    /// capability declaration ("I will answer RS_REQs"). Absent/false = box-driven SETUP; the app
    /// ships it ON and it applies to BOTH transports since the 2026-08-10 wireless flip.
    #[serde(default, rename = "appDrivenSetup")]
    pub app_driven_setup: bool,
    /// `uiAppearanceMode`/`uiAppearanceSetting` and `mapAppearanceMode`/`mapAppearanceSetting` in
    /// `/info`. These were emitted UNCONDITIONALLY while the app shipped owner-facing toggles for
    /// them — the box deciding a value the app owns, i.e. a docs/carplay/04_CAPABILITIES_AND_CONFIG.md directive-2 violation rather than
    /// a missing feature (the runtime senders in `events.rs` have always existed).
    ///
    /// DEFAULT `true` ON PURPOSE, via `default_true`: a plain `#[serde(default)]` bool is `false`,
    /// which would silently STOP emitting keys we have always sent the moment any config omitted
    /// them. `true` reproduces today's wire exactly, so this only bites when the owner deliberately
    /// turns a toggle off — which is the whole point. The app also defaults both to `true`.
    #[serde(default = "default_true", rename = "enablesUIAppearance")]
    pub enables_ui_appearance: bool,
    #[serde(default = "default_true", rename = "enablesMapAppearance")]
    pub enables_map_appearance: bool,
    /// `viewAreaSupportsFocusTransfer` on every type-110 viewArea, previously hardcoded `false`.
    ///
    /// Defaults `false` — matching both the old hardcode and the app's own default — because unlike
    /// the appearance pair this one ADVERTISES A NEW CAPABILITY when enabled. Turning it on is a
    /// real wire change to be validated on hardware, not a byte-neutral plumbing fix.
    #[serde(default, rename = "enablesFocusTransfer")]
    pub enables_focus_transfer: bool,
}

/// Apple `LimitedUIConfig` — WHICH UI elements iOS restricts when limited-UI mode is on.
///
/// Two separate surfaces, and they are easy to confuse:
///   * **`limitedUI` is a RUNTIME toggle**, not a config key: `/command setLimitedUI {limitedUI:bool}`
///     on the event channel (Apple `kAirPlayCommand_SetLimitedUI`). No reconnect, no `/info` change,
///     no SETUP negotiation. Already implemented — `events::send_set_limited_ui`, driven from the
///     host app's Controls window over OCBM `CMD_LIMITED_UI_ON`/`_OFF`.
///   * **`limitedUIElements` is a STATIC `/info` capability** — the list of elements that restriction
///     applies to. Absent, iOS applies its own default set, which is why the runtime toggle already
///     "works" without this struct. Declaring it is what makes the element *selection* match the
///     Simulator's LimitedUIConfig checkboxes.
///
/// Wire form is an **array of element-name strings** (R14G17 `AirPlayCommon.h:1007-1013`,
/// "[Array] List of UI elements that are affected in limited UI mode"). The names pass through
/// verbatim from config to wire — `CarPlaySDK.framework` contains only the two *keys*, while all ten
/// element-name strings live in the Simulator app binary, the same pass-through pattern as
/// `showsInstruments`.
///
/// R14G17 defines the first five; the rest are post-2017 additions present in the current Simulator.
/// CT5 CINEMO (tier 2) corroborates the first five and their order — `AddLimitedUIElement(n)` with
/// 0=softKeyboard, 1=softKeypad, 2=nonMusicLists, 3=MusicLists, 4=japanMaps — though CINEMO uses
/// integer ids in its own API while the wire is strings.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LimitedUiConfig {
    #[serde(default, rename = "softKeyboard")]
    pub soft_keyboard: bool,
    #[serde(default, rename = "softPhoneKeypad")]
    pub soft_phone_keypad: bool,
    #[serde(default, rename = "nonMusicLists")]
    pub non_music_lists: bool,
    #[serde(default, rename = "musicLists")]
    pub music_lists: bool,
    #[serde(default, rename = "japanMaps")]
    pub japan_maps: bool,
    /// Post-2017; emitted as the wire string `longUserAlert` (see [`LimitedUiConfig::elements`]).
    #[serde(default, rename = "longAlerts")]
    pub long_alerts: bool,
    // --- Real LimitedUIConfig CodingKeys that airPlayElements does NOT emit. Parsed so a
    // --- Simulator-shaped YAML round-trips without erroring; never reach `limitedUIElements`.
    #[serde(default, rename = "pairedDevices")]
    pub paired_devices: bool,
    #[serde(default, rename = "themeCustomization")]
    pub theme_customization: bool,
    #[serde(default, rename = "automakerSettings")]
    pub automaker_settings: bool,
    #[serde(default, rename = "automakerSettingsInfoButton")]
    pub automaker_settings_info_button: bool,
}

impl LimitedUiConfig {
    /// Enabled element names for `/info` `limitedUIElements`, in Apple's exact emission order.
    ///
    /// Read directly out of the Simulator, NOT inferred — `CarPlayConfigs.LimitedUIConfig`'s
    /// `airPlayElements.getter : [Swift.String]` extension at `0x10010c8d4` in the app binary. The
    /// six literals are Swift small-string immediates (`movk` pairs, no `adrp`), decoded with their
    /// length discriminators matching each string exactly. The getter has no pointer-based literals,
    /// so six is the complete set.
    ///
    /// Three things here are counter-intuitive and were each got WRONG by inference first:
    ///   1. **Order is `musicLists` BEFORE `nonMusicLists`** — the reverse of R14G17's header
    ///      declaration order (`AirPlayCommon.h:1011-1012`).
    ///   2. **The wire string for the `longAlerts` config key is `longUserAlert`** — the YAML key and
    ///      the wire value differ. This is the only element where they do.
    ///   3. **Only SIX of the ten config fields reach the wire.** `pairedDevices`,
    ///      `themeCustomization`, `automakerSettings` and `automakerSettingsInfoButton` are real
    ///      `LimitedUIConfig` CodingKeys but `airPlayElements` never emits them — they drive
    ///      Simulator-side behaviour, not `limitedUIElements`. They are parsed here so a
    ///      Simulator-shaped YAML round-trips, and deliberately not emitted.
    ///
    /// Empty ⇒ emit nothing, so iOS keeps its own default restriction set and `/info` stays
    /// byte-identical to a build without this feature. That is also why the runtime toggle already
    /// works today without any of this: `limitedUIElements` selects WHICH elements restrict, it does
    /// not enable the feature.
    pub fn elements(&self) -> Vec<&'static str> {
        [
            (self.soft_keyboard, "softKeyboard"),
            (self.soft_phone_keypad, "softPhoneKeypad"),
            (self.music_lists, "musicLists"),
            (self.non_music_lists, "nonMusicLists"),
            (self.japan_maps, "japanMaps"),
            (self.long_alerts, "longUserAlert"),
        ]
        .iter()
        .filter_map(|&(on, name)| if on { Some(name) } else { None })
        .collect()
    }
}

/// Apple `videoStreamsConfig` → `mainVideoStream` (pixelDimensions/maxFPS/hidConfig/primaryInput/
/// viewAreas). The HID control support lives HERE in Apple's schema, not in accessoryConfig.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VideoStreamsConfig {
    #[serde(default, rename = "mainVideoStream")]
    pub main_video_stream: VideoStreamConfig,
    /// Apple `altVideoStreams[]` — the instrument-cluster / navigation screen(s). A non-empty list
    /// asks the box to advertise + accept the alt (type-111) screen stream (docs/carplay/06_AV_PIPELINE.md).
    #[serde(default, rename = "altVideoStreams")]
    pub alt_video_streams: Vec<AltVideoStream>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AltVideoStream {
    #[serde(default, rename = "pixelDimensions")]
    pub pixel_dimensions: PixelDimensions,
    #[serde(default, rename = "maxFPS")]
    pub max_fps: i64,
    /// Per-stream view/safe areas (see [`ViewAreaEntry`]). First entry drives the alt display's safeArea.
    #[serde(default, rename = "viewAreas")]
    pub view_areas: Vec<ViewAreaEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct VideoStreamConfig {
    /// Negotiated max frame rate — iOS caps its encode at this (30/60). 0 = keep the box default.
    #[serde(default, rename = "maxFPS")]
    pub max_fps: i64,
    #[serde(default, rename = "hidConfig")]
    pub hid_config: HidConfig,
    /// Per-stream view/safe areas. First entry drives the main display's safeArea (`info.rs::view_areas`).
    #[serde(default, rename = "viewAreas")]
    pub view_areas: Vec<ViewAreaEntry>,
}

/// One `viewAreas[]` element: a `viewArea` (where content maps — normally the full frame) plus a
/// `safeArea` (the inset rectangle CarPlay keeps its UI inside). Apple authoring names (`originX` /
/// `originY` / `width` / `height`), which the box then re-emits under the wire's `…Pixels` keys.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ViewAreaEntry {
    #[serde(default, rename = "viewArea")]
    pub view_area: AreaRect,
    #[serde(default, rename = "safeArea")]
    pub safe_area: SafeRect,
    /// `drawUIOutsideSafeArea` — Apple puts this on `ViewAreaConfig`, a SIBLING of `viewArea`/
    /// `safeArea`, not inside `safeArea`. MOVED HERE 2026-07-30: it previously lived only on
    /// [`SafeRect`], so a host YAML — which has always emitted it at the correct Apple level — had it
    /// silently dropped by serde and the flag NEVER fired. Confirmed against
    /// `CarPlayConfigs.ViewAreaConfig.CodingKeys` (`viewArea, safeArea, safeAreaDisabled,
    /// statusBarEdge, transitionControl, focusTransfer, drawUIOutsideSafeArea`) and against
    /// `SettingsWindow.swift:355`, which emits it at the same indent as `safeArea:`.
    #[serde(default, rename = "drawUIOutsideSafeArea")]
    pub draw_ui_outside_safe_area: bool,
}

impl ViewAreaEntry {
    /// Effective `drawUIOutsideSafeArea`, accepting BOTH placements: Apple's (on the entry) and the
    /// legacy nested-inside-`safeArea` spelling this crate used to require. Either being true wins, so
    /// pre-existing YAML written against the old shape keeps working.
    pub fn draw_outside(&self) -> bool {
        self.draw_ui_outside_safe_area || self.safe_area.draw_ui_outside_safe_area
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AreaRect {
    #[serde(default, rename = "originX")]
    pub origin_x: i64,
    #[serde(default, rename = "originY")]
    pub origin_y: i64,
    #[serde(default)]
    pub width: i64,
    #[serde(default)]
    pub height: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SafeRect {
    #[serde(default, rename = "originX")]
    pub origin_x: i64,
    #[serde(default, rename = "originY")]
    pub origin_y: i64,
    #[serde(default)]
    pub width: i64,
    #[serde(default)]
    pub height: i64,
    /// Allow UI to draw in the viewArea↔safeArea gap. Default false = keep UI strictly inside safeArea.
    #[serde(default, rename = "drawUIOutsideSafeArea")]
    pub draw_ui_outside_safe_area: bool,
}

/// Apple `hidConfig` — which HID controls the accessory supports (mirrors the CarPlaySimulator
/// VehicleConfig templates). `dPadSupport` gates the D-Pad HID device.
///
/// ⚠️ We parse EIGHT of Apple's TWENTY-ONE `hidConfig` fields and act on FOUR. The four added by
/// C-2 (`touchpadSupport`, `steeringWheelSupport`, `mediaButtonsSupport`, `touchScreenMode`) are
/// PARSE-ONLY until C-7/C-8 derive the display-features word from them
/// (`dPadSupport`, `knobSupport`, `telephonyButtonsSupport`, `touchScreenSupportsMultiTouch` — each
/// arms its HID device via the matching `events::set_*_advertised` lever in airplayd's
/// per-connection config apply). Apple's full set, read
/// from `CarPlayConfigs.HIDConfig` in the Simulator binary (ivar offsets +0x18..+0x3d), is:
/// `knobSupport`, `knobSupportsHomeAndBackButton`, `knobSupportsNudge`, `knobSupportsDPadNudgeFudge`,
/// `knobFocusTransfer{Left,Right,Up,Down}`, `lockPTFocus`, `touchScreenMode`,
/// `touchScreenSupportsCancel`, `touchScreenSupportsMultiTouch`, `touchpadSupport`, `touchpadWidth`,
/// `touchpadHeight`, `touchpadButtonsSupport`, `steeringWheelSupport`, `telephonyButtonsSupport`,
/// `mediaButtonsSupport`, `dPadSupport`, `notificationButton`. (`primaryInput` is NOT one of them —
/// in Apple's YAML it is a sibling key of `hidConfig` under `mainVideoStream`.)
///
/// `touchScreenMode` is an enum whose YAML spellings are `Disabled` / `Low Fidelty` / `High Fidelty`
/// — Apple's own misspelling, which must be matched literally.
///
/// Note the docstring above previously claimed `dPadSupport` also gates a "Direction Buttons (0x10)"
/// feature bit. It does not: 0x10 is **Touchpad**, Direction Buttons is 0x20 and comes from
/// `steeringWheelSupport`, and `dPadSupport` contributes nothing to `displays[].features` at all.
/// See the corrected bit table in `info.rs`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct HidConfig {
    #[serde(default, rename = "dPadSupport")]
    pub dpad_support: bool,
    /// WIRED since the knob lever landed: airplayd arms it per connection via
    /// `events::set_knob_advertised` (a thin delegate to `levers::set_knob`, so the HID-ingest
    /// gate reads the same cell) and `info.rs` emits the uid-4 `hidDevices[]` entry from it. The descriptor bytes
    /// must come from `HIDKnobCreateDescriptor` (70 B, home+back+nudge) or
    /// `HIDKnobBasicCreateDescriptor` (51 B) — both present verbatim in the licensed R14G17 source at
    /// `AppleCarPlay/Platform/HIDKnob.c` AND byte-identical in the current Simulator's
    /// `CarPlaySDK.framework` (file offsets 0x2D9503 / 0x2D9549). Do NOT hand-roll them: a guessed
    /// knob descriptor is what broke the box on 2026-07-06.
    #[serde(default, rename = "knobSupport")]
    pub knob_support: bool,
    /// `telephonyButtonsSupport` — advertises the uid-5 HID Telephony device (Answer/End/Flash/Mute + DTMF,
    /// Apple `HIDTelephony`). Opt-in + revertible; off = no telephony `hidDevices[]` entry.
    #[serde(default, rename = "telephonyButtonsSupport")]
    pub telephony_support: bool,
    /// `touchScreenSupportsMultiTouch` — Apple's own `hidConfig` key (one of the twenty-one), now
    /// ACTED ON rather than discarded: it selects `HIDTouchScreenMultiCreateDescriptor` for the
    /// uid-1 device. Off keeps the single-finger descriptor, so this is opt-in and revertible from
    /// the host YAML alone, in the same shape as `dPadSupport`.
    #[serde(default, rename = "touchScreenSupportsMultiTouch")]
    pub touch_screen_supports_multi_touch: bool,

    // ---- C-2 (docs/carplay/04_CAPABILITIES_AND_CONFIG.md C8): the display-`features` inputs. PARSE-ONLY for now. ----
    //
    // The app has ALREADY been emitting `touchpadSupport`, `touchScreenMode` and
    // `mediaButtonsSupport` (SettingsWindow.swift `hidFields()` + the `touchScreenMode` line); serde
    // silently discarded all three because no field existed here. So this is not new schema so much
    // as the box finally reading what it was already being told. `steeringWheelSupport` is the one
    // field the APP must add.
    //
    // Nothing consumes these yet, deliberately: `info.rs` still emits the constant
    // `if dpad() {0x1A} else {0x0A}`. Deriving the word honestly is C-7 (shadow-log the derived value
    // beside the emitted one) then C-8 (flip emission), because the honest derivation for the default
    // config is 0x08 — it DROPS the unbacked Knobs bit 0x02 we advertise today, and changing an
    // advertised input capability is a wire change that needs a hardware session.
    //
    // Corrected bit table (see this struct's docstring and `info.rs`): 0x02 Knobs <- knobSupport,
    // 0x04/0x08 Low/HighFidelityTouch <- touchScreenMode, 0x10 Touchpad <- touchpadSupport,
    // 0x20 DirectionButtons <- steeringWheelSupport. `dPadSupport` contributes NOTHING to the word.
    /// `touchpadSupport` — drives the Touchpad feature bit (0x10) ONLY. It deliberately does NOT add a
    /// `hidDevices[]` entry: the touchpad descriptor and the third-HID-device question belong to
    /// workstream D, and docs/wireless/00_WIRELESS_CARPLAY.md bars pushing a third device until the 2026-07-06 guessed-
    /// descriptor incident is resolved. The two-device floor (uid1 touchscreen + uid2 media buttons)
    /// is a constraint ON THE APP's config, surfaced in the app UI — not a box-side veto.
    #[serde(default, rename = "touchpadSupport")]
    pub touchpad_support: bool,
    /// `steeringWheelSupport` — drives DirectionButtons (0x20). The only one of these four the app
    /// does not emit today.
    #[serde(default, rename = "steeringWheelSupport")]
    pub steering_wheel_support: bool,
    /// `mediaButtonsSupport` — the uid-2 HID media-buttons device. Emitted by the app already.
    #[serde(default, rename = "mediaButtonsSupport")]
    pub media_buttons_support: bool,
    /// `touchScreenMode` — an ENUM STRING, not a bool: `"Disabled"` / `"Low Fidelty"` /
    /// `"High Fidelty"`. Apple's own misspelling of "Fidelity" is load-bearing and must be matched
    /// literally; the app already emits it in that exact form. Parsed as a free `String` rather than a
    /// serde enum so an unrecognised value degrades to "no touch bits" with a log at the point of USE
    /// (C-7) instead of failing the WHOLE document here — a parse failure would drop the pushed
    /// resolution, HEVC, appDrivenSetup and metadata tier along with it.
    #[serde(default, rename = "touchScreenMode")]
    pub touch_screen_mode: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DisplayPanelsConfig {
    #[serde(default, rename = "mainDisplayPanel")]
    pub main_display_panel: DisplayPanelConfig,
    /// `altDisplayPanels[]` — cluster / secondary panels. PARSED since 2026-08-10; still not emitted.
    ///
    /// docs/carplay/03_SDK_GROUND_TRUTH.md §5 names the missing `/info` `displayPanels[]` array as the alt-content ROOT CAUSE:
    /// CarPlaySDK 509.11's `AirPlayCopyServerInfo` emits BOTH `displays` and `displayPanels`, iOS
    /// requests both, and the modern panel dict is the only place `properties` (this
    /// `displayProperties` array), a nested `videoStreams[]` and a per-stream `initialURL` exist on
    /// the wire at all. Our legacy flat `displays[]` is "sufficient to negotiate and receive the
    /// type-111 stream and structurally incapable of defining anything inside it".
    ///
    /// THE APP AUTHORS THESE as of 2026-08-10: `SettingsWindow.altDisplayPanelsYAML` emits one panel
    /// (`DisplayPanel.Alt1`, dims tracking the alt stream, `displayProperties: [showsInstruments]`)
    /// when the cluster stream is on, and `altDisplayPanels: []` otherwise — byte-identical to what it
    /// always sent with the cluster off.
    ///
    /// ⚠️ DO NOT READ docs/carplay/03_SDK_GROUND_TRUTH.md §5 AS LIVE. It calls the missing `/info` `displayPanels[]` array the
    /// alt-content ROOT CAUSE, claiming our flat `displays[]` is "structurally incapable of defining
    /// anything inside" the cluster stream. **That is REFUTED by our own shipped code** (owner-confirmed
    /// on hardware 2026-08-11): cluster content works and its elements are toggleable. The mechanism is
    /// `showUI` with query parameters — `ClusterContent` {None, Instruction Card, Map, Navigation App}
    /// in `ControlsWindow.swift`, and `showSpeedLimit`/`showCompass`/`showETA` carried as query flags on
    /// the cluster URL (`airplayd/src/main.rs`, `NAV_APPEARANCE_*` in `ocbm-proto`), which airplayd's own
    /// comment calls "literally the elements inside the navigation video". That vocabulary was taken from
    /// Apple's Simulator (`AirPlayShowUIURL.airPlayURL`), so it is Apple's mechanism, not a workaround.
    ///
    /// So this parse buys schema completeness, NOT cluster control — the control already exists by a
    /// different route. Anyone reviving the `displayPanels[]` emission must first establish what it adds
    /// beyond `showUI`, because the justification previously written down is false.
    #[serde(default, rename = "altDisplayPanels")]
    pub alt_display_panels: Vec<DisplayPanelConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DisplayPanelConfig {
    #[serde(default, rename = "displayPanelID")]
    pub display_panel_id: String,
    #[serde(default, rename = "pixelDimensions")]
    pub pixel_dimensions: PixelDimensions,
    /// Apple's `DisplayPanelProperty` has EXACTLY three cases (docs/carplay/03_SDK_GROUND_TRUTH.md §5): `dpManaged`,
    /// `additionalContent`, `showsInstruments` — and only the last appears in any stock template.
    /// Parsed as free strings so an unrecognised value degrades to "ignored" at the point of USE
    /// rather than failing the WHOLE pushed document (a parse failure would drop the resolution,
    /// HEVC, appDrivenSetup and metadata tier along with it).
    #[serde(default, rename = "displayProperties")]
    pub display_properties: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PixelDimensions {
    #[serde(default)]
    pub width: i64,
    #[serde(default)]
    pub height: i64,
}

/// Return the safeArea as a real inset rectangle, or `None` if it's absent, degenerate, or covers the
/// whole panel. A full-frame safeArea is NOT an inset — treating it as one would flip the `viewAreas`
/// feature on for every config and stop non-curved sessions being byte-identical.
fn safe_area_inset(s: &SafeRect, panel_w: i64, panel_h: i64) -> Option<(i64, i64, i64, i64)> {
    if s.width <= 0 || s.height <= 0 {
        return None;
    }
    // `saturating_add`, not `+`: `originX: 9223372036854775807` in host YAML overflows — a panic under
    // debug assertions (tests, dev builds) and a silent wrap in the box's release profile.
    let covers_full = s.origin_x <= 0
        && s.origin_y <= 0
        && s.origin_x.saturating_add(s.width) >= panel_w
        && s.origin_y.saturating_add(s.height) >= panel_h;
    if covers_full {
        None
    } else {
        Some((s.origin_x, s.origin_y, s.width, s.height))
    }
}

impl VehicleConfig {
    /// Parse a host-pushed YAML config. Unknown Apple fields are ignored; a malformed document is an error.
    pub fn from_yaml(bytes: &[u8]) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_slice(bytes)
    }

    /// Overlay the fields the box currently consumes onto `base`, returning the effective config.
    ///
    /// Only the main display panel's pixel dimensions are applied, and ONLY when both are positive —
    /// a partial/garbled config must never zero the resolution out from under a working session; it
    /// falls through to `base` (the caller's interim default) instead.
    pub fn apply(&self, mut base: DeviceConfig) -> DeviceConfig {
        let d = &self
            .display_panels_config
            .main_display_panel
            .pixel_dimensions;
        if d.width > 0 && d.height > 0 {
            base.display_width = d.width;
            base.display_height = d.height;
        }
        // maxFPS (30/60) — iOS caps encode at this. 0 / out-of-range keeps the box default.
        let fps = self.video_streams_config.main_video_stream.max_fps;
        // 30 | 60 only. 24 REMOVED 2026-07-30: `CarPlayConfigs.FramesPerSecond` (Simulator
        // @0x10026a9ac) has exactly two cases — rawValue "30" and "60" — and R14G17
        // `AirPlayCommon.h:949` says "Defaults to 60". 24 is not in Apple's vocabulary, so a YAML
        // asking for it was advertising an unrepresentable rate; it now falls through and keeps the
        // default. (The WIRE type is Integer; Apple's "30"/"60" strings are YAML authoring values.)
        if [30, 60].contains(&fps) {
            base.max_fps = fps;
        }
        // Safe areas — first viewAreas entry per stream. Only a REAL inset (a safeArea strictly inside
        // the panel) is carried; a full-frame safeArea is treated as full-bleed (`None`) so non-curved
        // configs stay byte-identical. `info.rs::view_areas` re-validates against the panel and falls
        // back to full-bleed if out of bounds, so a bad inset can never break the stream.
        if let Some(e) = self
            .video_streams_config
            .main_video_stream
            .view_areas
            .first()
        {
            if let Some(rect) =
                safe_area_inset(&e.safe_area, base.display_width, base.display_height)
            {
                base.main_safe_area = Some(rect);
                base.main_draw_outside_safe = e.draw_outside();
            }
        }
        if let Some(stream) = self.video_streams_config.alt_video_streams.first() {
            let (aw, ah) = (
                stream.pixel_dimensions.width,
                stream.pixel_dimensions.height,
            );
            if let Some(e) = stream.view_areas.first() {
                if let Some(rect) = safe_area_inset(&e.safe_area, aw, ah) {
                    base.alt_safe_area = Some(rect);
                    base.alt_draw_outside_safe = e.draw_outside();
                }
            }
        }
        // Audio capability set (YAML-driven HU audio config). The transport-matched `wired:`/
        // `wireless:` arm wins, else the flat `formats`/`preset` keys; a resolved set REPLACES the
        // transport-gated default; absent/unresolved keeps it. This is the single lever that
        // decides which codecs/rates/stream-types the box advertises to iOS.
        if let Some(specs) = self.audio.resolve() {
            base.audio_formats = specs;
        }
        // Alt-stream maxFPS — previously parsed and dropped (the alt display inherited the MAIN
        // stream's FPS regardless of what the YAML asked for). Same 30/60 validation as the main
        // stream; anything else leaves 0 = inherit.
        if let Some(a) = self.video_streams_config.alt_video_streams.first() {
            if [30, 60].contains(&a.max_fps) {
                base.alt_max_fps = a.max_fps;
            }
        }
        // limitedUIElements — WHICH elements iOS restricts when the runtime setLimitedUI toggle is on.
        // Empty stays empty, so `/info` is byte-identical to before this feature unless a YAML asks
        // for it. The toggle itself is `/command setLimitedUI` and is independent of this list.
        base.limited_ui_elements =
            self.limited_ui_config.elements().into_iter().map(str::to_string).collect();
        // OEM icon — decode each resolution here; empty/invalid entries are skipped, and an empty set
        // leaves `oem_icons` empty so info.rs omits the keys (byte-identical /info). Prefer the
        // multi-resolution `images` set (Apple emits 120/180/256); fall back to the legacy single image.
        let oic = &self.oem_icon_config;
        let mut icons: Vec<(Vec<u8>, i64, i64)> = Vec::new();
        if !oic.images.is_empty() {
            for img in &oic.images {
                let png = decode_base64(&img.image_base64);
                if !png.is_empty() {
                    icons.push((png, img.width, img.height));
                }
            }
        } else {
            let png = decode_base64(&oic.image_base64);
            if !png.is_empty() {
                icons.push((png, oic.width, oic.height));
            }
        }
        if !icons.is_empty() {
            base.oem_icons = icons;
            base.oem_icon_label = oic.label.clone();
            base.oem_icon_visible = oic.visible;
        }
        base
    }

    /// Whether iOS should be told the `viewAreas` capability is active (echoed in SETUP
    /// `enabledFeatures`). True if the host set `accessoryConfig.enablesViewAreas`, OR any stream
    /// actually defines a *real inset* safeArea — so defining an inset "just works" without the extra
    /// toggle, while a full-frame (no-inset) config stays byte-identical to a receiver without the
    /// feature. iOS only honors an inset safeArea when this feature is negotiated.
    pub fn view_areas_enabled(&self) -> bool {
        if self.accessory_config.enables_view_areas {
            return true;
        }
        // Use the SAME effective main dims apply() validates against (audit Fix #6). apply() falls back
        // to the base DeviceConfig dims (1920×720 in production) when the YAML omits displayPanelsConfig
        // (panel dims 0×0); validating this gate against the raw 0×0 made an inset that apply() DOES
        // advertise (main_safe_area = Some) fail the gate, so the feature was never negotiated and iOS
        // silently ignored the inset. Mirror apply()'s fallback so the gate and the advertised geometry
        // always agree. (The alt path below already matches apply() — both use stream.pixel_dimensions.)
        let m = &self
            .display_panels_config
            .main_display_panel
            .pixel_dimensions;
        let (mw, mh) = if m.width > 0 && m.height > 0 {
            (m.width, m.height)
        } else {
            let d = DeviceConfig::default();
            (d.display_width, d.display_height)
        };
        let main_inset = self
            .video_streams_config
            .main_video_stream
            .view_areas
            .first()
            .and_then(|e| safe_area_inset(&e.safe_area, mw, mh))
            .is_some();
        let alt_inset = self
            .video_streams_config
            .alt_video_streams
            .first()
            .map(|s| {
                s.view_areas
                    .first()
                    .and_then(|e| {
                        safe_area_inset(
                            &e.safe_area,
                            s.pixel_dimensions.width,
                            s.pixel_dimensions.height,
                        )
                    })
                    .is_some()
            })
            .unwrap_or(false);
        main_inset || alt_inset
    }

    /// Whether to advertise `cornerMasks` (docs/carplay/06_AV_PIPELINE.md) — the host's `accessoryConfig.enablesCornerMasks`.
    /// Drives the `CARPLAY_CORNERMASKS` lever (screen-level `/info` flag + SETUP `enabledFeatures` echo).
    pub fn corner_masks_enabled(&self) -> bool {
        self.accessory_config.enables_corner_masks
    }

    /// The pushed `accessoryName`, BOUNDED — docs/carplay/04_CAPABILITIES_AND_CONFIG.md C-6. `None` when unset or empty after bounding.
    ///
    /// THE BOUND IS 63 UTF-8 BYTES, and it is derived rather than guessed:
    ///   * `Tlv::str` encodes `4 + L + 1` in a BE16        -> L <= 65530
    ///   * `Link::build_msg` encodes `16 + body` in a BE16 -> body <= 65519
    ///   * our own SYN advertises `MaxRcvPacketLength = 0x1000` -> body <= 4080, and with the name in
    ///     all four TLV positions the Identify grows 4 B per character, so the MTU alone allows ~901.
    ///   * the DNS-SD instance label maxes at 63 bytes (`rx-connect` publishes the name as one).
    ///
    /// 63 is therefore the BINDING constraint and is comfortably inside every iAP2 ceiling — the
    /// field comment above, which reads as though 63 fails to cover the iAP2 params, understates it.
    ///
    /// WHY BOUND AT ALL, given the overflow guards exist: `Tlv::str`, `Tlv::bytes` and
    /// `Link::build_msg` guard with `debug_assert!`, and the release profile sets no
    /// `debug-assertions` key (defaults false) with no `-C debug-assertions` in `.cargo/config.toml`
    /// or `build.sh`. Every one of those guards is a NO-OP on the box, and each encoder then
    /// truncates silently via `as u16`. The failure mode is a silently malformed `0x1D01` — no panic,
    /// no log — on the one message whose rejection cannot be recovered from within a session.
    ///
    /// Truncation is on a CHAR BOUNDARY: `Tlv::str` measures `s.len()`, which is BYTES, so a
    /// char-based cap would be wrong for non-ASCII and a naive byte cut would emit invalid UTF-8.
    pub fn accessory_name_bounded(&self) -> Option<String> {
        const MAX_NAME_BYTES: usize = 63;
        let raw = self.accessory_name.as_deref()?;
        // Cc controls would be rejected at the YAML stream level, but this is the box's own defence:
        // the app's stripping is courtesy, and a config can reach here from somewhere else.
        // Strip controls BEFORE trimming, not after: `"\x01 Name"` trims to itself (a control is not
        // whitespace), and filtering then left the leading space behind.
        let cleaned: String = raw.chars().filter(|c| !c.is_control()).collect();
        let cleaned = cleaned.trim();
        let out = match cleaned.char_indices().find(|(i, c)| i + c.len_utf8() > MAX_NAME_BYTES) {
            Some((cut, _)) => cleaned[..cut].to_string(),
            None => cleaned.to_string(),
        };
        (!out.is_empty()).then_some(out)
    }

    /// docs/carplay/04_CAPABILITIES_AND_CONFIG.md #25 — the app owns these three; `/info` used to decide them itself.
    pub fn ui_appearance_enabled(&self) -> bool {
        self.accessory_config.enables_ui_appearance
    }

    pub fn map_appearance_enabled(&self) -> bool {
        self.accessory_config.enables_map_appearance
    }

    pub fn focus_transfer_enabled(&self) -> bool {
        self.accessory_config.enables_focus_transfer
    }

    /// Whether to advertise `logTransfer` (docs/carplay/04_CAPABILITIES_AND_CONFIG.md Half A) — the host's
    /// `accessoryConfig.enablesLogTransfer`. Drives the logTransfer lever
    /// (`logTransferInfo` in `/info` + the SETUP `enabledFeatures` echo).
    pub fn log_transfer_enabled(&self) -> bool {
        self.accessory_config.enables_log_transfer
    }

    /// Whether the host asks for app-driven SETUP (`accessoryConfig.appDrivenSetup`, plan P1).
    /// Drives the `appsetup` lever the same way the other accessoryConfig toggles drive theirs: the
    /// CALLER (airplayd `load_device_config`) arms `levers::set_appsetup` per connection — apply()
    /// never touches levers in this crate, so the config→lever seam stays in one place.
    pub fn app_driven_setup(&self) -> bool {
        self.accessory_config.app_driven_setup
    }

    /// Whether the host YAML asks for the D-Pad HID device (Apple `hidConfig.dPadSupport`). Drives
    /// the `CARPLAY_DPAD` lever, which gates the uid-3 D-Pad HID device ONLY — it contributes
    /// nothing to the display features word. 0x10 there is Touchpad and 0x20 is DirectionButtons;
    /// see this file's `HidConfig` docstring and `info.rs` for the corrected bit table.
    pub fn dpad_support(&self) -> bool {
        self.video_streams_config
            .main_video_stream
            .hid_config
            .dpad_support
    }

    /// `hidConfig.touchScreenSupportsMultiTouch` — advertise Apple's two-finger touchscreen.
    pub fn multi_touch_support(&self) -> bool {
        self.video_streams_config
            .main_video_stream
            .hid_config
            .touch_screen_supports_multi_touch
    }

    /// Whether the host YAML asks for the rotary Knob HID device (Apple `hidConfig.knobSupport`).
    /// Drives the `CARPLAY_KNOB` lever (the uid-4 device — the Simulator's real navigation device).
    pub fn knob_support(&self) -> bool {
        self.video_streams_config
            .main_video_stream
            .hid_config
            .knob_support
    }

    /// Whether the host YAML asks for the Telephony HID device (`hidConfig.telephonyButtonsSupport`).
    pub fn telephony_support(&self) -> bool {
        self.video_streams_config
            .main_video_stream
            .hid_config
            .telephony_support
    }

    /// Whether the host YAML asks for the ALT / cluster screen (a non-empty `altVideoStreams[]`).
    /// Drives `CARPLAY_ALTSCREEN` — the 2nd `displays[]` entry + `altScreen` feature + the type-111
    /// SETUP path (docs/carplay/06_AV_PIPELINE.md).
    pub fn alt_screen(&self) -> bool {
        !self.video_streams_config.alt_video_streams.is_empty()
    }

    /// The alt screen's requested pixel size (first altVideoStream), for the 2nd display panel.
    pub fn alt_dimensions(&self) -> Option<(i64, i64)> {
        self.video_streams_config
            .alt_video_streams
            .first()
            .and_then(|s| {
                let d = &s.pixel_dimensions;
                (d.width > 0 && d.height > 0).then_some((d.width, d.height))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Apple `Widescreen.yaml` shape (trimmed): the box must read the MAIN panel's 1920×720 and
    /// NOT be confused by the identically-named `width`/`height` inside videoStreamsConfig.viewAreas.
    const WIDESCREEN: &str = r#"
name: Widescreen
version: 1
displayPanelsConfig:
  mainDisplayPanel:
    displayPanelID: DisplayPanel.Main
    pixelDimensions:
      width: 1920
      height: 720
videoStreamsConfig:
  mainVideoStream:
    videoStreamID: VideoStream.Main
    pixelDimensions:
      width: 1920
      height: 720
    viewAreas:
    - viewArea:
        originX: 640
        originY: 0
        width: 1280
        height: 720
    hidConfig:
      touchScreenMode: High Fidelty
accessoryConfig:
  enablesMainBufferedAudio: true
  enablesHEVC: false
"#;

    fn base() -> DeviceConfig {
        DeviceConfig { display_width: 1920, display_height: 720, ..Default::default() }
    }

    #[test]
    fn applies_main_panel_resolution_ignoring_extras() {
        let vc = VehicleConfig::from_yaml(WIDESCREEN.as_bytes()).expect("parse");
        assert_eq!(vc.name, "Widescreen");
        // pulls the MAIN panel dims, not the viewAreas 1280×720
        let dev = vc.apply(base());
        assert_eq!((dev.display_width, dev.display_height), (1920, 720));
        // accessoryConfig parsed: Widescreen template says enablesHEVC: false
        assert!(!vc.accessory_config.enables_hevc);
    }

    #[test]
    fn enables_hevc_parses_true_and_defaults_false() {
        let y = "accessoryConfig:\n  enablesHEVC: true\n";
        assert!(
            VehicleConfig::from_yaml(y.as_bytes())
                .unwrap()
                .accessory_config
                .enables_hevc
        );
        assert!(
            !VehicleConfig::from_yaml(b"name: Minimum\n")
                .unwrap()
                .accessory_config
                .enables_hevc
        );
    }

    #[test]
    fn enables_log_transfer_parses_true_and_defaults_false() {
        let y = "accessoryConfig:\n  enablesLogTransfer: true\n";
        assert!(
            VehicleConfig::from_yaml(y.as_bytes())
                .unwrap()
                .log_transfer_enabled()
        );
        assert!(
            !VehicleConfig::from_yaml(b"name: Minimum\n")
                .unwrap()
                .log_transfer_enabled()
        );
    }

    #[test]
    fn enables_main_buffered_audio_parses_true_and_defaults_false() {
        let y = "accessoryConfig:\n  enablesMainBufferedAudio: true\n";
        assert!(
            VehicleConfig::from_yaml(y.as_bytes())
                .unwrap()
                .accessory_config
                .enables_main_buffered_audio
        );
        assert!(
            !VehicleConfig::from_yaml(b"name: Minimum\n")
                .unwrap()
                .accessory_config
                .enables_main_buffered_audio
        );
    }

    #[test]
    fn app_driven_setup_parses_true_and_defaults_false() {
        let y = "accessoryConfig:\n  appDrivenSetup: true\n";
        assert!(
            VehicleConfig::from_yaml(y.as_bytes())
                .unwrap()
                .app_driven_setup()
        );
        // Absent = false: box-driven SETUP stays the default (a host that can't relay never pushes it).
        assert!(
            !VehicleConfig::from_yaml(b"name: Minimum\n")
                .unwrap()
                .app_driven_setup()
        );
    }

    #[test]
    fn non_widescreen_resolution_is_honored() {
        let yaml = "displayPanelsConfig:\n  mainDisplayPanel:\n    pixelDimensions:\n      width: 800\n      height: 480\n";
        let dev = VehicleConfig::from_yaml(yaml.as_bytes())
            .unwrap()
            .apply(base());
        assert_eq!((dev.display_width, dev.display_height), (800, 480));
    }

    #[test]
    fn empty_or_partial_config_falls_through_to_base() {
        // No display section at all → keep base (never zero the resolution).
        let dev = VehicleConfig::from_yaml(b"name: Minimum\n")
            .unwrap()
            .apply(base());
        assert_eq!((dev.display_width, dev.display_height), (1920, 720));
        // Zero dims (garbled) → also keep base.
        let z = "displayPanelsConfig:\n  mainDisplayPanel:\n    pixelDimensions:\n      width: 0\n      height: 0\n";
        let dev = VehicleConfig::from_yaml(z.as_bytes())
            .unwrap()
            .apply(base());
        assert_eq!((dev.display_width, dev.display_height), (1920, 720));
    }

    #[test]
    fn malformed_yaml_is_an_error() {
        assert!(VehicleConfig::from_yaml(b"\tthis: : is not: yaml:\n").is_err());
    }

    /// The six `limitedUIElements` wire names AND their order, pinned against Apple's own
    /// `CarPlayConfigs.LimitedUIConfig.airPlayElements` getter (Simulator app binary @0x10010c8d4,
    /// Swift small-string immediates decoded with matching length discriminators).
    ///
    /// This test exists because all three of these were got WRONG by inference before the binary was
    /// read: (1) `musicLists` comes BEFORE `nonMusicLists`, the reverse of R14G17's header order;
    /// (2) the `longAlerts` config key emits the wire string `longUserAlert`; (3) only six of the ten
    /// LimitedUIConfig fields reach the wire at all.
    #[test]
    fn limited_ui_elements_match_apples_airplay_elements_getter() {
        let all = LimitedUiConfig {
            soft_keyboard: true,
            soft_phone_keypad: true,
            music_lists: true,
            non_music_lists: true,
            japan_maps: true,
            long_alerts: true,
            // Real CodingKeys that airPlayElements never emits — set them all and prove they stay off
            // the wire.
            paired_devices: true,
            theme_customization: true,
            automaker_settings: true,
            automaker_settings_info_button: true,
        };
        assert_eq!(
            all.elements(),
            vec![
                "softKeyboard",
                "softPhoneKeypad",
                "musicLists",
                "nonMusicLists",
                "japanMaps",
                "longUserAlert",
            ],
            "must match LimitedUIConfig.airPlayElements exactly, including order"
        );
    }

    /// Absent/empty `limitedUIConfig` must leave the list empty so `/info` omits the key entirely and
    /// iOS keeps its own default restriction set — i.e. this feature is opt-in and byte-neutral.
    #[test]
    fn limited_ui_is_opt_in_and_byte_neutral_when_unset() {
        assert!(LimitedUiConfig::default().elements().is_empty());
        let vc = VehicleConfig::from_yaml(b"name: Test\n").expect("parses");
        assert!(vc.limited_ui_config.elements().is_empty());
        assert!(vc.apply(DeviceConfig::default()).limited_ui_elements.is_empty());
    }

    /// A Simulator-shaped `limitedUIConfig` block parses and selects only the wire subset.
    #[test]
    fn limited_ui_config_parses_from_apple_shaped_yaml() {
        let y = b"name: Test\nlimitedUIConfig:\n  softKeyboard: true\n  japanMaps: true\n  automakerSettings: true\n";
        let vc = VehicleConfig::from_yaml(y).expect("parses");
        assert_eq!(vc.limited_ui_config.elements(), vec!["softKeyboard", "japanMaps"]);
        assert!(vc.limited_ui_config.automaker_settings, "non-wire key still parses");
    }

    #[test]
    fn audio_preset_selects_named_baseline() {
        let y = "audio:\n  preset: wireless_8\n";
        let dev = VehicleConfig::from_yaml(y.as_bytes())
            .unwrap()
            .apply(base());
        assert_eq!(dev.audio_formats, crate::info::preset_wireless_8());
        // Alias resolves to the same set.
        let a = "audio:\n  preset: wireless_full\n";
        let dev = VehicleConfig::from_yaml(a.as_bytes())
            .unwrap()
            .apply(base());
        assert_eq!(dev.audio_formats, crate::info::preset_wireless_8());
    }

    #[test]
    fn per_transport_audio_subsections_beat_flat_and_match_arm() {
        // docs/carplay/04_CAPABILITIES_AND_CONFIG.md B5: the app's "auto" mode pushes BOTH arms; the box presents the matching one.
        let y = "audio:\n  wired:\n    preset: wired_pcm\n  wireless:\n    preset: wireless_8\n";
        let vc = VehicleConfig::from_yaml(y.as_bytes()).unwrap();
        assert_eq!(vc.audio.resolve_for(false).unwrap(), crate::info::preset_wired_pcm());
        assert_eq!(vc.audio.resolve_for(true).unwrap(), crate::info::preset_wireless_8());
        // A matched subsection beats the flat keys.
        let y = "audio:\n  preset: wireless_8\n  wired:\n    preset: wired_pcm\n";
        let vc = VehicleConfig::from_yaml(y.as_bytes()).unwrap();
        assert_eq!(vc.audio.resolve_for(false).unwrap(), crate::info::preset_wired_pcm());
        // The arm WITHOUT a subsection falls to the flat keys (legacy semantics preserved).
        assert_eq!(vc.audio.resolve_for(true).unwrap(), crate::info::preset_wireless_8());
        // An unresolvable subsection falls through to flat, then to None (keep default).
        let y = "audio:\n  wired:\n    preset: bogus\n";
        let vc = VehicleConfig::from_yaml(y.as_bytes()).unwrap();
        assert!(vc.audio.resolve_for(false).is_none());
    }

    #[test]
    fn explicit_audio_formats_replace_the_default() {
        // A hand-authored declarative set: media on type-102 AAC-LC + a mic-capable voice stream + a
        // multi-name OR'd output. This is the "test any HU config" surface.
        let y = r#"
audio:
  formats:
    - {type: 102, audioType: media, out: aac_lc_48k_stereo}
    - {type: 100, audioType: speechRecognition, in: aac_eld_16k_mono, out: aac_eld_16k_mono}
    - {type: 100, audioType: compatibility, in: pcm_16k_mono, out: "pcm_48k_stereo|pcm_16k_mono"}
"#;
        let dev = VehicleConfig::from_yaml(y.as_bytes())
            .unwrap()
            .apply(base());
        assert_eq!(dev.audio_formats.len(), 3);
        let media = &dev.audio_formats[0];
        assert_eq!(media.stream_type, 102);
        assert_eq!(media.audio_type.as_deref(), Some("media"));
        assert_eq!(media.input_formats, 0); // no `in:` → output-only
        assert_eq!(media.output_formats, 1 << 23); // aac_lc_48k_stereo
        let voice = &dev.audio_formats[1];
        assert_eq!(voice.input_formats, 1 << 26); // aac_eld_16k_mono mic
                                                  // The OR'd output resolves both names.
        assert_eq!(dev.audio_formats[2].output_formats, 0x8000 | 0x10);
    }

    /// THE DRIFT GUARD for C-2. The `hidConfig` keys the macOS app actually emits
    /// (`SettingsWindow.swift` `hidFields()` + its derived `touchScreenMode:` line), asserting the box now
    /// READS the three fields it has been silently discarding. If the app's key names or the
    /// `touchScreenMode` enum spelling ever change, this fails here rather than the box quietly
    /// falling back to defaults on a live session.
    ///
    /// Apple's misspelling "Fidelty" is deliberate and load-bearing — do not "fix" it.
    ///
    /// NOT included, deliberately: `touchScreenHighFidelity`. That is an app-INTERNAL `@Published`
    /// toggle from which the app DERIVES `touchScreenMode`; it has never been in the pushed YAML.
    /// An earlier revision listed it here and called this fixture "byte-for-byte", which would have
    /// invited a second, redundant input to the same feature bits at C-7.
    #[test]
    fn parses_the_hid_fields_the_app_already_emits() {
        let y = "\
videoStreamsConfig:
  mainVideoStream:
    hidConfig:
      dPadSupport: false
      knobSupport: false
      knobSupportsHomeAndBackButton: false
      knobSupportsNudge: false
      mediaButtonsSupport: true
      telephonyButtonsSupport: false
      touchpadSupport: true
      touchpadButtonsSupport: false
      touchScreenSupportsCancel: false
      touchScreenMode: High Fidelty
      steeringWheelSupport: true
";
        let cfg = VehicleConfig::from_yaml(y.as_bytes()).expect("app-shaped hidConfig must parse");
        let hid = &cfg.video_streams_config.main_video_stream.hid_config;
        assert!(hid.media_buttons_support, "mediaButtonsSupport was being dropped before C-2");
        assert!(hid.touchpad_support, "touchpadSupport was being dropped before C-2");
        assert!(hid.steering_wheel_support, "steeringWheelSupport (new in the app) must parse");
        assert_eq!(
            hid.touch_screen_mode, "High Fidelty",
            "Apple's misspelling is matched literally — the app emits this exact string"
        );
        // Unrelated keys the app also emits must still parse without disturbing anything.
        assert!(!hid.dpad_support && !hid.knob_support && !hid.telephony_support);
    }

    /// C-2 is parse-only. SCOPE OF THIS TEST, stated precisely because an earlier version of this
    /// docstring overclaimed and would have misled the C-7/C-8 hardware session:
    ///
    /// It compares `DeviceConfig` and NOTHING ELSE. That is total within `DeviceConfig` (a Debug
    /// comparison catches every field, including ones added later), but several emitted surfaces
    /// never travel through it — `accessory_config.*`, which airplayd reads straight off
    /// `VehicleConfig`; `app_driven_setup()` / `view_areas_enabled()` / `alt_screen()`, which are
    /// armed as `levers::` and each drive a SETUP `enabledFeatures` token; and — the one that
    /// matters here — the HID surface C-2 exists to feed, since `displays[].features` and
    /// `hidDevices[]` are emitted from `levers::dpad()/knob()/telephony()` in `info.rs`, not from
    /// `DeviceConfig`.
    ///
    /// So this test will NOT fail when C-8 flips the features word, because C-8 follows the
    /// established dpad/knob/telephony pattern: a `vc.touchpad_support()` accessor plus a lever arm
    /// in airplayd, both invisible from here. Do not read a green suite as proof the wire is
    /// unmoved. The wider closure is
    /// `tests/r4_c2_schema.rs::r4_c2_hid_fields_move_no_emitted_surface_including_the_non_device_config_ones`,
    /// which asserts the non-`DeviceConfig` surfaces too.
    #[test]
    fn c2_hid_fields_are_parse_only_and_change_no_output() {
        let base_dev = VehicleConfig::from_yaml(b"name: X\n").unwrap().apply(base());
        let with_hid = VehicleConfig::from_yaml(
            b"name: X\nvideoStreamsConfig:\n  mainVideoStream:\n    hidConfig:\n      touchpadSupport: true\n      steeringWheelSupport: true\n      mediaButtonsSupport: true\n      touchScreenMode: High Fidelty\n",
        )
        .unwrap()
        .apply(base());
        // `DeviceConfig` is not `PartialEq` — compare the full Debug rendering. SCOPE CAVEATS ARE IN
        // THE DOCSTRING and they matter: this does NOT observe the HID emission path, so it will not
        // catch C-8's flip. Do not restate the scope here; one statement of it, in one place.
        assert_eq!(
            format!("{base_dev:?}"),
            format!("{with_hid:?}"),
            "C-2 must not move any emitted value — deriving the features word is C-7/C-8 and needs \
             a hardware session (the honest derivation drops the unbacked Knobs bit 0x02)"
        );
    }

    /// `accessoryName` parses and is kept DISTINCT from the template `name`. Also parse-only: C-6
    /// applies it, because it changes /info, Bonjour and iAP2 params 0/20 together.
    #[test]
    fn accessory_name_parses_and_does_not_touch_the_template_name() {
        let cfg = VehicleConfig::from_yaml(b"name: Widescreen\naccessoryName: \"Owner Roadster\"\n")
            .expect("accessoryName must parse");
        assert_eq!(cfg.name, "Widescreen", "the template name is untouched");
        assert_eq!(cfg.accessory_name.as_deref(), Some("Owner Roadster"));
        // Absent stays absent — the whole workstream is absent-off.
        let plain = VehicleConfig::from_yaml(b"name: Widescreen\n").unwrap();
        assert_eq!(plain.accessory_name, None);
        // And it does NOT leak into the advertised name yet (that is C-6).
        assert_eq!(cfg.apply(base()).name, base().name);
    }

    /// Parses `altDisplayPanels[]` in APPLE'S OWN template shape — taken from
    /// `CarPlaySimulator.devicekitplugin/Contents/Resources/VehicleConfigs/Configs/Standard Navigation.yaml`,
    /// not invented. That file is the reference for what a real cluster panel looks like.
    #[test]
    fn parses_apples_alt_display_panel_shape() {
        let y = "\
displayPanelsConfig:
  mainDisplayPanel:
    displayPanelID: DisplayPanel.Main
    pixelDimensions:
      width: 800
      height: 480
  altDisplayPanels:
  - displayPanelID: DisplayPanel.Alt1
    pixelDimensions:
      width: 640
      height: 480
    displayProperties:
    - showsInstruments
";
        let cfg = VehicleConfig::from_yaml(y.as_bytes()).expect("Apple's own shape must parse");
        let alts = &cfg.display_panels_config.alt_display_panels;
        assert_eq!(alts.len(), 1);
        assert_eq!(alts[0].display_panel_id, "DisplayPanel.Alt1");
        assert_eq!(alts[0].pixel_dimensions.width, 640);
        assert_eq!(alts[0].pixel_dimensions.height, 480);
        assert_eq!(alts[0].display_properties, vec!["showsInstruments".to_string()]);
        // The main panel still parses alongside it.
        assert_eq!(cfg.display_panels_config.main_display_panel.pixel_dimensions.width, 800);
    }

    /// The app ships `altDisplayPanels: []` today, and an unrecognised `displayProperties` value must
    /// not fail the WHOLE document — that would silently drop resolution, HEVC, appDrivenSetup and
    /// the metadata tier with it.
    #[test]
    fn alt_display_panels_absent_empty_and_unknown_properties_are_all_harmless() {
        // Absent entirely.
        let a = VehicleConfig::from_yaml(b"name: X\n").unwrap();
        assert!(a.display_panels_config.alt_display_panels.is_empty());
        // The literal shape the app emits today.
        let b = VehicleConfig::from_yaml(b"displayPanelsConfig:\n  altDisplayPanels: []\n").unwrap();
        assert!(b.display_panels_config.alt_display_panels.is_empty());
        // An unrecognised property string parses and is carried, not rejected.
        let c = VehicleConfig::from_yaml(
            b"displayPanelsConfig:\n  altDisplayPanels:\n  - displayProperties: [dpManaged, someFutureThing]\n",
        )
        .expect("an unknown displayProperty must not fail the document");
        assert_eq!(
            c.display_panels_config.alt_display_panels[0].display_properties,
            vec!["dpManaged".to_string(), "someFutureThing".to_string()]
        );
    }

    /// The app's real emitted document, generated by `tools/regen_app_yaml_fixture.py`.
    /// Shared by the drift guard and the key-inventory guard below.
    const APP_EMITTED_DOCUMENT: &str = r#"name: "CarLink Widescreen"
wireless: true
hot_handover: false
pairing: just_works
android_auto: true
displayPanelsConfig:
  mainDisplayPanel:
    displayPanelID: DisplayPanel.Main
    pixelDimensions:
      width: 1920
      height: 1080
  altDisplayPanels:
  - displayPanelID: DisplayPanel.Alt1
    pixelDimensions:
      width: 640
      height: 480
    displayProperties:
    - showsInstruments
videoStreamsConfig:
  mainVideoStream:
    videoStreamID: VideoStream.Main
    pixelDimensions:
      width: 1920
      height: 1080
    maxFPS: 60
    viewAreas:
    - viewArea:
        originX: 0
        originY: 0
        width: 1920
        height: 1080
      safeArea:
        originX: 0
        originY: 0
        width: 1920
        height: 1080
      drawUIOutsideSafeArea: false
    hidConfig:
      dPadSupport: true
      knobSupport: false
      knobSupportsHomeAndBackButton: false
      knobSupportsNudge: false
      mediaButtonsSupport: true
      telephonyButtonsSupport: false
      touchpadSupport: false
      touchpadButtonsSupport: false
      touchScreenMode: High Fidelty
      touchScreenSupportsCancel: true
      touchScreenSupportsMultiTouch: false
      steeringWheelSupport: true
    primaryInput: Touchpad
  altVideoStreams:
  - videoStreamID: VideoStream.Alt1
    pixelDimensions:
      width: 640
      height: 480
    maxFPS: 30
    viewAreas:
    - viewArea:
        originX: 0
        originY: 0
        width: 640
        height: 480
      safeArea:
        originX: 0
        originY: 0
        width: 640
        height: 480
      drawUIOutsideSafeArea: false
    initialURL: maps:/car/instrumentcluster/map

accessoryConfig:
  enablesMainBufferedAudio: false
  enablesHEVC: true
  enablesUIAppearance: true
  enablesMapAppearance: true
  enablesCornerMasks: false
  enablesVideoPlayback: false
  enablesViewAreas: false
  enablesEnhancedSiri: false
  enablesFocusTransfer: false
  enablesUIContext: false
  enablesUISync: false
  enablesFileTransfer: false
  enablesLogTransfer: false
  enablesVehicleDataProtocol: false
  enablesDCX: false
  appDrivenSetup: true
limitedUIConfig:
  softKeyboard: true
  softPhoneKeypad: false
  musicLists: true
  nonMusicLists: false
  japanMaps: false
  longAlerts: true
  pairedDevices: true
  themeCustomization: false
  automakerSettings: false
  automakerSettingsInfoButton: true
oemIconConfig:
  images:
    - width: 120
      height: 120
      imageBase64: "iVBORw0KGgo="
    - width: 180
      height: 180
      imageBase64: "iVBORw0KGgp="
  label: "Owner's \"Roadster\" \\ EV"
  visible: true
audio:
  formats:
  - {type: 102, audioType: media, out: aac_lc_48k_stereo}
  - {type: 107, audioType: speechRecognition, in: aac_eld_16k_mono, out: aac_eld_16k_mono}
metadata:
  tier: proven
  skip: [voice_over_cursor, call_history]
"#;

    /// THE APP->BOX DRIFT GUARD, and it exists because a HAND-TYPED version of it certified a
    /// BROKEN emitter. This fixture is the VERBATIM output of `tools/regen_app_yaml_fixture.py`,
    /// which extracts the emitter out of `SettingsWindow.swift` and runs it under `xcrun swift`.
    /// If you touch the emitter, re-run that script and paste its output here — the instruction is
    /// executable, which is the whole point; the previous docstring said "RE-GENERATE" with no
    /// generator behind it, and the fixture it described was a hand-trimmed 33 lines of this 80.
    ///
    /// SCOPE: the structure and every concatenation seam are the real emitter's. The VALUES are the
    /// script's stubs, and four sub-emitters (`accessoryFields`, `limitedUIFields`,
    /// `oemIconVariants`, and the audio/metadata/iapConfig tails) are stubbed rather than
    /// extracted — their internal seams are NOT covered here.
    ///
    /// Three indentation traps live in this document, all hit in one sitting:
    ///   1. Swift dedents a multiline literal relative to its CLOSING DELIMITER.
    ///   2. `altDisplayPanelsYAML` is a SEPARATE expression interpolated INTO that literal, so it
    ///      escapes the dedent — its source columns are its emitted columns.
    ///   3. THE FATAL ONE, which only a fixture containing `videoStreamsConfig:` can catch: `va()`'s
    ///      literal has NO trailing newline, so an appended `initialURL` line GLUES onto
    ///      `drawUIOutsideSafeArea: false` and serde rejects the ENTIRE document — silently
    ///      reverting resolution, HEVC, appDrivenSetup, audio and the metadata tier together.
    #[test]
    fn the_apps_real_emitted_document_parses() {
        let y = APP_EMITTED_DOCUMENT;
        let cfg = VehicleConfig::from_yaml(y.as_bytes())
            .expect("the app's REAL emitted document must parse on the box");

        let alts = &cfg.display_panels_config.alt_display_panels;
        assert_eq!(alts.len(), 1);
        assert_eq!(alts[0].display_panel_id, "DisplayPanel.Alt1");
        assert_eq!(alts[0].pixel_dimensions.width, 640);
        assert_eq!(alts[0].display_properties, vec!["showsInstruments".to_string()]);
        assert_eq!(cfg.display_panels_config.main_display_panel.pixel_dimensions.width, 1920);

        // THE REGRESSION GUARD for the glued-initialURL bug. `initialURL` itself is an unknown
        // field on `AltVideoStream` and is dropped; what must survive is the STREAM.
        assert_eq!(
            cfg.video_streams_config.alt_video_streams.len(),
            1,
            "the alt STREAM must parse — a glued initialURL line kills the entire document"
        );
        assert_eq!(cfg.alt_dimensions(), Some((640, 480)));

        // Seams BELOW the alt stream. These are what actually regress when the document fails to
        // parse, and the old 33-line fixture could not see any of them.
        let hid = &cfg.video_streams_config.main_video_stream.hid_config;
        assert!(hid.dpad_support, "hidConfig rides after the main stream's viewAreas");
        assert_eq!(
            hid.touch_screen_mode, "High Fidelty",
            "Apple's own VehicleConfigs ship this typo; the app emits it deliberately [sic]"
        );
        assert!(cfg.accessory_config.enables_hevc, "the accessoryConfig tail");
        assert!(cfg.accessory_config.app_driven_setup);

        // THE ESCAPING SEAM. `oemIconLabel` is the only free text in the pushed document that runs
        // through `YamlEmit.quotedBody`, and the generator now extracts the REAL escaper out of
        // VehicleConfig.swift rather than stubbing it to the identity function -- which is what made
        // this seam untestable before. The source label is:
        //     Owner's "Roadster" \ EV<BEL>
        // so a correct emitter escapes the quote AND the backslash and strips the Cc control. Getting
        // either escape wrong destroys the WHOLE document, not just this field.
        let oic = &cfg.oem_icon_config;
        assert_eq!(
            oic.label, "Owner's \"Roadster\" \\ EV",
            "quote and backslash must survive the round trip and the BEL must be gone"
        );
        assert_eq!(oic.images.len(), 2, "the multi-resolution oemIcon set");
        assert_eq!(oic.images[0].width, 120);
        assert!(oic.visible);

        // THE 16-vs-6 CONTRACT, now pinned instead of implicit. The app emits SIXTEEN
        // accessoryConfig keys and this struct declares SIX; the other ten must be IGNORED, not
        // rejected. Without `deny_unknown_fields` that is serde's default, but it is load-bearing
        // enough to assert: if anyone ever adds `deny_unknown_fields` here, every pushed config
        // would fail to parse and the box would silently revert to compiled defaults.
        let ac = &cfg.accessory_config;
        assert!(ac.enables_hevc, "a KNOWN key still parses alongside ten unknown ones");
        assert!(ac.app_driven_setup);
        assert!(!ac.enables_corner_masks);
        // The two gated in docs/carplay/04_CAPABILITIES_AND_CONFIG.md #25 ride in the same block and must survive the unknown keys.
        assert!(ac.enables_ui_appearance);
        assert!(ac.enables_map_appearance);
        assert!(!ac.enables_focus_transfer);

        // limitedUI: the app emits TEN keys, only SIX are Apple `airPlayElements` and reach /info.
        // The other four are round-trip only. Pinned because the mapping carries a RENAME — the app
        // says `longAlerts`, the wire says `longUserAlert` — and a mismatch there would silently drop
        // a restriction the owner asked for, with the document still parsing perfectly.
        let els = cfg.limited_ui_config.elements();
        assert_eq!(
            els,
            vec!["softKeyboard", "musicLists", "longUserAlert"],
            "only the enabled airPlayElements, in emission order, with longAlerts renamed"
        );
        assert!(
            !els.contains(&"pairedDevices"),
            "pairedDevices is enabled in the document but is round-trip only — it must NOT reach /info"
        );

        // AUDIO, end to end. The app's `custom` mode builds `type:` values by hand and this document
        // really does carry 107 (AuxIn) — a stream `session.rs`'s SETUP dispatch cannot serve. The box
        // must DROP it rather than advertise a stream it would then refuse (952086d). Also note the
        // app's own emitter already filtered the `out: none` row, so the two filters compose.
        let dev = VehicleConfig::from_yaml(y.as_bytes()).unwrap().apply(base());
        let types: Vec<i64> = dev.audio_formats.iter().map(|f| f.stream_type).collect();
        assert_eq!(
            types,
            vec![102],
            "107 must be dropped by the box; 102 must survive — advertise-without-serve is the hazard"
        );
    }

    /// PARSE-ONLY: reading alt panels must not move anything the box EMITS. Asserted over the three
    /// surfaces a panel-driven implementation would naturally reach for, not just `DeviceConfig` —
    /// a `{:?}` comparison of `DeviceConfig` alone passed while mutants wired panel dimensions into
    /// `alt_dimensions()` (the 2nd `/info` `displays[]` entry) and `displayProperties` into
    /// `alt_screen()` (the SETUP `altScreen` feature + type-111 path), which are precisely the two
    /// wirings docs/carplay/03_SDK_GROUND_TRUTH.md §5 proposes. A test must be live where its claim is made.
    ///
    /// The `displayPanels[]` emission is a separate, gated hardware experiment whose justification
    /// is currently REFUTED (docs/carplay/03_SDK_GROUND_TRUTH.md §5) — a green suite here must never be read as "safe to emit".
    #[test]
    fn alt_display_panels_are_parse_only_today() {
        let panels = "name: X\ndisplayPanelsConfig:\n  altDisplayPanels:\n  - displayPanelID: DisplayPanel.Alt1\n    pixelDimensions:\n      width: 1234\n      height: 567\n    displayProperties: [showsInstruments]\n";
        let plain = VehicleConfig::from_yaml(b"name: X\n").unwrap();
        let with_panels = VehicleConfig::from_yaml(panels.as_bytes()).unwrap();

        assert_eq!(format!("{:?}", plain.clone().apply(base())), format!("{:?}", with_panels.clone().apply(base())));
        assert_eq!(plain.alt_screen(), with_panels.alt_screen(), "panels must not arm the alt screen");
        assert_eq!(plain.alt_dimensions(), with_panels.alt_dimensions(), "panel dims must not reach /info displays[]");
        assert_eq!(
            format!("{:?}", plain.accessory_config),
            format!("{:?}", with_panels.accessory_config),
            "panels must not move the SETUP-facing accessoryConfig"
        );
        // And the panel data really was parsed — otherwise this proves nothing.
        assert_eq!(with_panels.display_panels_config.alt_display_panels[0].pixel_dimensions.width, 1234);
    }

    #[test]
    fn parses_app_exact_indentation() {
        // Byte-for-byte the indentation the macOS Settings app emits (`  - {...}` at 2 spaces under
        // `formats:`), so a config authored in the UI round-trips through the box parser.
        let y = "audio:\n  formats:\n  - {type: 102, audioType: media, out: aac_lc_48k_stereo}\n  - {type: 100, audioType: speechRecognition, in: aac_eld_16k_mono, out: aac_eld_16k_mono}\n";
        let dev = VehicleConfig::from_yaml(y.as_bytes())
            .unwrap()
            .apply(base());
        assert_eq!(dev.audio_formats.len(), 2);
        assert_eq!(
            dev.audio_formats[1].audio_type.as_deref(),
            Some("speechRecognition")
        );
        assert_eq!(dev.audio_formats[1].input_formats, 1 << 26);
    }

    /// A pushed `type:` the SETUP dispatch cannot serve must never reach `/info`. Before this guard
    /// `stream_type` was an unvalidated `i64` flowing straight into the advert, so an ordinary config
    /// push of `type: 107` (AuxIn) or `type: 103` (MainBuffered) made the box promise iOS a stream
    /// `session.rs:793` would then omit from its SETUP response.
    ///
    /// The served set here MUST track `session.rs`'s `100..=102` audio arm. If you add an arm there
    /// and not here, this test still passes and the new stream stays unadvertised — annoying but
    /// safe. The reverse (widening here first) is the dangerous direction and is what this guards.
    #[test]
    fn unserveable_audio_stream_types_never_reach_the_advert() {
        let y = "audio:\n  formats:\n\
                 \x20 - {type: 102, audioType: media, out: aac_lc_48k_stereo}\n\
                 \x20 - {type: 107, audioType: speechRecognition, in: aac_eld_16k_mono}\n\
                 \x20 - {type: 103, audioType: media, out: aac_lc_48k_stereo}\n";
        let dev = VehicleConfig::from_yaml(y.as_bytes()).unwrap().apply(base());
        let types: Vec<i64> = dev.audio_formats.iter().map(|f| f.stream_type).collect();
        assert_eq!(
            types,
            vec![102],
            "107 (AuxIn) and 103 (MainBuffered) are parsed but unserved — advertising them promises \
             a stream the SETUP dispatch omits"
        );
    }

    /// docs/carplay/04_CAPABILITIES_AND_CONFIG.md #25. The gating is only safe if an ABSENT key reproduces the pre-gating wire, and the
    /// two appearance keys were emitted UNCONDITIONALLY while `viewAreaSupportsFocusTransfer` was
    /// hardcoded `false`. A plain `#[serde(default)]` bool is `false`, which would have silently
    /// stopped emitting the appearance keys for every config that predates the field — the exact
    /// class of silent regression this repo keeps hitting. Hence `default_true`, pinned here.
    #[test]
    fn appearance_gates_default_to_the_pre_gating_wire() {
        // No accessoryConfig at all — the shape of every config written before these keys existed.
        let none = VehicleConfig::from_yaml(b"name: X\n").unwrap();
        assert!(none.ui_appearance_enabled(), "absent key must keep emitting uiAppearance*");
        assert!(none.map_appearance_enabled(), "absent key must keep emitting mapAppearance*");
        assert!(
            !none.focus_transfer_enabled(),
            "focus transfer was hardcoded false; absent must NOT advertise a new capability"
        );

        // An accessoryConfig that sets OTHER keys must not disturb them either.
        let other = VehicleConfig::from_yaml(b"accessoryConfig:\n  enablesHEVC: true\n").unwrap();
        assert!(other.ui_appearance_enabled());
        assert!(other.map_appearance_enabled());
        assert!(!other.focus_transfer_enabled());

        // And the owner's toggles actually reach the box — the whole point of the task.
        let off = VehicleConfig::from_yaml(
            b"accessoryConfig:\n  enablesUIAppearance: false\n  enablesMapAppearance: false\n  enablesFocusTransfer: true\n",
        )
        .unwrap();
        assert!(!off.ui_appearance_enabled());
        assert!(!off.map_appearance_enabled());
        assert!(off.focus_transfer_enabled());
    }

    /// docs/carplay/04_CAPABILITIES_AND_CONFIG.md C-6. The bound exists because the `Tlv`/`Link` overflow guards are `debug_assert!`s
    /// compiled OUT of the box's release build, so an over-long name silently truncates a `0x1D01`
    /// — no panic, no log — on the message whose rejection is unrecoverable within a session.
    #[test]
    fn accessory_name_is_bounded_to_63_utf8_bytes_on_a_char_boundary() {
        let name = |y: &str| VehicleConfig::from_yaml(y.as_bytes()).unwrap().accessory_name_bounded();

        assert_eq!(name("name: X\n"), None, "absent stays absent");
        assert_eq!(name("accessoryName: \"\"\n"), None, "empty is not a name");
        assert_eq!(name("accessoryName: \"   \"\n"), None, "whitespace-only is not a name");
        assert_eq!(name("accessoryName: \"CarLink\"\n").as_deref(), Some("CarLink"));

        // Exactly at the limit survives whole; one over is cut to the limit.
        let sixty_three = "a".repeat(63);
        assert_eq!(name(&format!("accessoryName: \"{sixty_three}\"\n")).as_deref(), Some(&*sixty_three));
        let sixty_four = "a".repeat(64);
        let got = name(&format!("accessoryName: \"{sixty_four}\"\n")).unwrap();
        assert_eq!(got.len(), 63, "63 BYTES, not chars — Tlv::str measures s.len()");

        // MULTI-BYTE: 'é' is 2 bytes, so 32 of them = 64 bytes and the cut must land between
        // characters. A naive byte truncation here would emit invalid UTF-8 onto the wire.
        let e32 = "é".repeat(32);
        let got = name(&format!("accessoryName: \"{e32}\"\n")).unwrap();
        assert!(got.len() <= 63, "never exceeds the byte bound (got {})", got.len());
        assert_eq!(got.chars().count(), 31, "cut on a char boundary, not mid-codepoint");
        assert!(std::str::from_utf8(got.as_bytes()).is_ok(), "still valid UTF-8");

        // A 4-byte codepoint straddling the boundary must be dropped whole, not split.
        let emoji = "😀".repeat(16); // 64 bytes
        let got = name(&format!("accessoryName: \"{emoji}\"\n")).unwrap();
        assert_eq!(got.chars().count(), 15, "the straddling 4-byte char is dropped entirely");
        assert_eq!(got.len(), 60);

        // Controls are stripped BEFORE the trim: a control is not whitespace, so trimming first left
        // the space it was hiding behind in the result.
        assert_eq!(name("accessoryName: \"\\u0001 CarLink \"\n").as_deref(), Some("CarLink"));
        assert_eq!(name("accessoryName: \"\\u0001\\u0002\"\n"), None, "controls-only is not a name");
    }

    /// A bad leaf must never fail the whole document — the policy this file states for resolution,
    /// HEVC, `appDrivenSetup` and the metadata tier. A `formats[]` entry missing `type` used to be a
    /// serde "missing field" error, i.e. a whole-document failure that reverted EVERY key.
    #[test]
    fn a_formats_entry_missing_type_loses_only_that_entry() {
        let y = "audio:\n  formats:\n\
                 \x20 - {audioType: media, out: aac_lc_48k_stereo}\n\
                 \x20 - {type: 102, audioType: media, out: aac_lc_48k_stereo}\n";
        let cfg = VehicleConfig::from_yaml(y.as_bytes()).expect("document still parses");
        let dev = cfg.apply(base());
        let types: Vec<i64> = dev.audio_formats.iter().map(|f| f.stream_type).collect();
        assert_eq!(types, vec![102], "the typeless entry is rejected on its own, by SERVEABLE_STREAM_TYPES");
    }

    /// EVERY key the app emits must be a CONSCIOUS decision — parsed by someone, or knowingly
    /// ignored. Nothing may be silently dropped.
    ///
    /// WHY THIS EXISTS. Three separate audits on 2026-08-11 each found the app emitting keys the box
    /// never modelled: ten of sixteen `accessoryConfig` keys, five around `hidConfig`, and earlier
    /// three `hidConfig` keys that had been discarded for MONTHS. The document parses perfectly
    /// either way — serde ignores unknown fields — so no existing test could see it, and the app's
    /// UI happily offered owner-facing controls that reached nothing.
    ///
    /// The drift guard above catches a CHANGE in what is emitted. It cannot catch a key the box
    /// never modelled, because that is not drift — it is a gap that has been there since the key
    /// was added. This test closes that gap by inventory.
    ///
    /// WHEN THIS FAILS you have added (or removed) a key in the Swift emitter. Do NOT just paste the
    /// new name in. Decide which list it belongs in and say why:
    ///   * parse it — add a field to the relevant struct here, or in `iap2-core::config`;
    ///   * or add it to `EMITTED_BUT_UNREAD` with a comment naming what will read it and when.
    ///
    /// The config document has THREE independent consumers — this crate, `iap2-core` (metadata +
    /// iapConfig), and `tools/session_supervisor.sh` via `cfg_value` — and no single place knows
    /// the union, which is precisely why the gaps went unseen.
    #[test]
    fn every_emitted_key_is_parsed_or_knowingly_ignored() {
        /// Emitted by the app and read by NOBODY — not this crate, not iap2-core, not the
        /// supervisor. Verified by grep on 2026-08-11. Each is a live owner-facing control in the
        /// app's UI that currently reaches nothing, EXCEPT where noted.
        const EMITTED_BUT_UNREAD: &[&str] = &[
            // The six `accessoryConfig` capabilities that are simply unimplemented (docs/carplay/04_CAPABILITIES_AND_CONFIG.md #25
            // closed the three that WERE doctrine violations — those are parsed now).
            "enablesDCX",
            "enablesFileTransfer",
            "enablesUIContext",
            "enablesUISync",
            "enablesVehicleDataProtocol",
            "enablesVideoPlayback",
            // Overlaps task #13 — the named capability is unimplemented, though the adjacent
            // `extendedFeatures` array is emitted unconditionally in /info.
            "enablesEnhancedSiri",
            // hidConfig leftovers. The descriptors these describe are BUILT and byte-verified
            // against R14G17; only the config plumbing is missing (workstream D).
            "knobSupportsHomeAndBackButton",
            "knobSupportsNudge",
            "touchpadButtonsSupport",
            "touchScreenSupportsCancel",
            // A sibling of hidConfig, not inside it. vehicle_config.rs states outright that it is
            // not one of the parsed display features.
            "primaryInput",
            // Apple schema identifiers we echo for shape fidelity but never key off.
            "videoStreamID",
            // Documented as unparsed: the cluster URL rides showUI at runtime instead (docs/carplay/03_SDK_GROUND_TRUTH.md §5).
            "initialURL",
            // `nightMode` (driven at runtime by /command setNightMode instead) and `rightHandDrive`
            // (no consumer at all) were DROPPED from the app's emitted document 2026-09-02
            // (verify_06 10, owner decision (c)) — no longer emitted, so no longer listed here.
            // `version` (schema version marker, never read) was dropped the same way.
        ];

        /// Emitted for a consumer OUTSIDE this crate. Not a gap.
        const READ_ELSEWHERE: &[&str] = &[
            "android_auto", // tools/session_supervisor.sh — aa_enabled(): gates arming the AA bridge
            "hot_handover", // tools/session_supervisor.sh — gates the wired/wireless preempt
            "pairing",      // tools/session_supervisor.sh — SSP association model
        ];

        /// The COMPLETE inventory of keys the app emits. Asserting the exact set (not a
        /// subset) is the point: a key added to the Swift emitter fails this test until someone
        /// decides whether the box should parse it.
        const APP_EMITTED_KEYS: &[&str] = &[
            "accessoryConfig",
            "altDisplayPanels",
            "altVideoStreams",
            "android_auto",
            "appDrivenSetup",
            "audio",
            "audioType",
            "automakerSettings",
            "automakerSettingsInfoButton",
            "dPadSupport",
            "displayPanelID",
            "displayPanelsConfig",
            "displayProperties",
            "drawUIOutsideSafeArea",
            "enablesCornerMasks",
            "enablesDCX",
            "enablesEnhancedSiri",
            "enablesFileTransfer",
            "enablesFocusTransfer",
            "enablesHEVC",
            "enablesLogTransfer",
            "enablesMainBufferedAudio",
            "enablesMapAppearance",
            "enablesUIAppearance",
            "enablesUIContext",
            "enablesUISync",
            "enablesVehicleDataProtocol",
            "enablesVideoPlayback",
            "enablesViewAreas",
            "formats",
            "height",
            "hidConfig",
            "hot_handover",
            "imageBase64",
            "images",
            "in",
            "initialURL",
            "japanMaps",
            "knobSupport",
            "knobSupportsHomeAndBackButton",
            "knobSupportsNudge",
            "label",
            "limitedUIConfig",
            "longAlerts",
            "mainDisplayPanel",
            "mainVideoStream",
            "maxFPS",
            "mediaButtonsSupport",
            "metadata",
            "musicLists",
            "name",
            "nonMusicLists",
            "oemIconConfig",
            "originX",
            "originY",
            "out",
            "pairedDevices",
            "pairing",
            "pixelDimensions",
            "primaryInput",
            "safeArea",
            "skip",
            "softKeyboard",
            "softPhoneKeypad",
            "steeringWheelSupport",
            "telephonyButtonsSupport",
            "themeCustomization",
            "tier",
            "touchScreenMode",
            "touchScreenSupportsCancel",
            "touchScreenSupportsMultiTouch",
            "touchpadButtonsSupport",
            "touchpadSupport",
            "type",
            "videoStreamID",
            "videoStreamsConfig",
            "viewArea",
            "viewAreas",
            "visible",
            "width",
            "wireless",
        ];

        let parsed: serde_yaml::Value =
            serde_yaml::from_str(APP_EMITTED_DOCUMENT).expect("the emitted document must parse");
        fn walk(v: &serde_yaml::Value, out: &mut std::collections::BTreeSet<String>) {
            match v {
                serde_yaml::Value::Mapping(m) => {
                    for (k, val) in m {
                        if let Some(s) = k.as_str() {
                            out.insert(s.to_string());
                        }
                        walk(val, out);
                    }
                }
                serde_yaml::Value::Sequence(seq) => seq.iter().for_each(|e| walk(e, out)),
                _ => {}
            }
        }
        let mut found = std::collections::BTreeSet::new();
        walk(&parsed, &mut found);

        let expected: std::collections::BTreeSet<&str> = APP_EMITTED_KEYS.iter().copied().collect();
        let actual: std::collections::BTreeSet<&str> = found.iter().map(String::as_str).collect();
        let added: Vec<_> = actual.difference(&expected).collect();
        let removed: Vec<_> = expected.difference(&actual).collect();
        assert!(
            added.is_empty(),
            "NEW key(s) {added:?} in the app's emitted document. Decide: parse it (a field here or \
             in iap2-core::config), or add it to APP_EMITTED_KEYS *and* to EMITTED_BUT_UNREAD with \
             a comment naming what will read it and when. Do not just paste the name in."
        );
        assert!(
            removed.is_empty(),
            "key(s) {removed:?} vanished from the emitted document — update APP_EMITTED_KEYS and \
             check nothing on the box still depends on them."
        );
        // The unread list must stay a real subset, or it is decoration.
        for k in EMITTED_BUT_UNREAD.iter().chain(READ_ELSEWHERE) {
            assert!(expected.contains(k), "{k:?} is listed but the app no longer emits it");
        }
    }

    #[test]
    fn explicit_formats_win_over_preset() {
        let y = "audio:\n  preset: wired_pcm\n  formats:\n    - {type: 102, audioType: media, out: opus_48k_mono}\n";
        let dev = VehicleConfig::from_yaml(y.as_bytes())
            .unwrap()
            .apply(base());
        assert_eq!(dev.audio_formats.len(), 1);
        assert_eq!(dev.audio_formats[0].output_formats, 1 << 30); // opus_48k_mono
    }

    #[test]
    fn bad_audio_input_falls_back_to_base_default() {
        let base_default = base().audio_formats;
        // Unknown preset → keep default.
        let dev = VehicleConfig::from_yaml(b"audio:\n  preset: nonsense\n")
            .unwrap()
            .apply(base());
        assert_eq!(dev.audio_formats, base_default);
        // A formats list where every entry has an unknown codec name → no valid entries → keep default
        // (never advertise an empty audioFormats, which fails iOS activation).
        let y = "audio:\n  formats:\n    - {type: 102, out: made_up_codec}\n";
        let dev = VehicleConfig::from_yaml(y.as_bytes())
            .unwrap()
            .apply(base());
        assert_eq!(dev.audio_formats, base_default);
        // No `audio:` key at all → base default untouched.
        let dev = VehicleConfig::from_yaml(b"name: Minimum\n")
            .unwrap()
            .apply(base());
        assert_eq!(dev.audio_formats, base_default);
    }

    #[test]
    fn parses_inset_safe_area_onto_device_config() {
        // A 1920×720 main panel with a 100px left/right safe-area inset (originX 100, width 1720),
        // mirroring the captured cluster shape.
        let y = r#"
displayPanelsConfig:
  mainDisplayPanel:
    pixelDimensions: { width: 1920, height: 720 }
videoStreamsConfig:
  mainVideoStream:
    viewAreas:
    - viewArea: { originX: 0, originY: 0, width: 1920, height: 720 }
      safeArea: { originX: 100, originY: 0, width: 1720, height: 720, drawUIOutsideSafeArea: false }
accessoryConfig:
  enablesViewAreas: true
"#;
        let vc = VehicleConfig::from_yaml(y.as_bytes()).expect("parse");
        assert!(vc.view_areas_enabled());
        let dev = vc.apply(base());
        assert_eq!(dev.main_safe_area, Some((100, 0, 1720, 720)));
        assert!(!dev.main_draw_outside_safe);
    }

    #[test]
    fn full_bleed_or_missing_safe_area_stays_none() {
        // No viewAreas → None (full-bleed).
        let dev = VehicleConfig::from_yaml(b"name: Minimum\n")
            .unwrap()
            .apply(base());
        assert_eq!(dev.main_safe_area, None);
        // A safeArea present but zero-size (garbled) → None, and NOT flagged as enabled.
        let z = "videoStreamsConfig:\n  mainVideoStream:\n    viewAreas:\n    - safeArea: { width: 0, height: 0 }\n";
        let vc = VehicleConfig::from_yaml(z.as_bytes()).unwrap();
        assert!(!vc.view_areas_enabled());
        assert_eq!(vc.apply(base()).main_safe_area, None);
    }

    #[test]
    fn full_frame_safe_area_is_not_an_inset() {
        // A safeArea that exactly covers the 1920×720 panel must be treated as full-bleed: no inset
        // carried, viewAreas NOT auto-enabled (keeps non-curved configs byte-identical).
        let y = r#"
displayPanelsConfig:
  mainDisplayPanel:
    pixelDimensions: { width: 1920, height: 720 }
videoStreamsConfig:
  mainVideoStream:
    viewAreas:
    - viewArea: { originX: 0, originY: 0, width: 1920, height: 720 }
      safeArea: { originX: 0, originY: 0, width: 1920, height: 720 }
"#;
        let vc = VehicleConfig::from_yaml(y.as_bytes()).unwrap();
        assert!(
            !vc.view_areas_enabled(),
            "full-frame safeArea must not enable viewAreas"
        );
        assert_eq!(vc.apply(base()).main_safe_area, None);
    }

    #[test]
    fn explicit_enables_view_areas_flag_wins() {
        // Even with no inset, the explicit capability toggle turns the feature on.
        let y = "accessoryConfig:\n  enablesViewAreas: true\n";
        assert!(VehicleConfig::from_yaml(y.as_bytes())
            .unwrap()
            .view_areas_enabled());
    }

    #[test]
    fn view_areas_enabled_matches_apply_when_panel_dims_omitted() {
        // No displayPanelsConfig (raw panel dims 0×0). An origin-anchored inset (originX 0, width
        // 1720 < 1920) reads as "full frame" against 0×0, so the un-fixed gate returned false while
        // apply() carried the inset against the 1920×720 base. They must agree (audit Fix #6).
        let y = r#"
videoStreamsConfig:
  mainVideoStream:
    viewAreas:
    - viewArea: { originX: 0, originY: 0, width: 1920, height: 720 }
      safeArea: { originX: 0, originY: 0, width: 1720, height: 720 }
"#;
        let vc = VehicleConfig::from_yaml(y.as_bytes()).unwrap();
        assert_eq!(vc.apply(base()).main_safe_area, Some((0, 0, 1720, 720)));
        assert!(vc.view_areas_enabled(), "gate must agree with apply() (Fix #6)");
    }

    /// The exact document `com.carlink.projection`'s `VehicleConfigGenerator` emits on the
    /// Raspberry Pi / AAOS port.
    ///
    /// This is a CROSS-LANGUAGE contract test and it is the only thing that can catch the failure
    /// it guards against. serde ignores unknown keys, so a misspelled or mis-nested field in the
    /// Kotlin generator costs the feature with **no error on either side**: the box logs a
    /// successful parse and quietly advertises its compiled default. The Kotlin test asserts the
    /// generator emits these bytes; this asserts the bytes mean what the generator intends.
    ///
    /// Keep in step with `VehicleConfigGenerator.render` in
    /// `host/CarlinkAndroid/projection/src/main/kotlin/com/carlink/projection/`.
    const PI_PROJECTION_APP_DOCUMENT: &str = r#"
name: "AAOS Raspberry Pi (generated)"

displayPanelsConfig:
  mainDisplayPanel:
    pixelDimensions:
      width: 1920
      height: 1080

videoStreamsConfig:
  mainVideoStream:
    maxFPS: 60
    hidConfig:
      touchScreenMode: "High Fidelty"
      touchScreenSupportsMultiTouch: true
      telephonyButtonsSupport: true
      mediaButtonsSupport: true
      steeringWheelSupport: true
      knobSupport: false
      dPadSupport: false

accessoryConfig:
  enablesHEVC: true
  enablesFocusTransfer: true

limitedUIConfig:
  softKeyboard: true
  softPhoneKeypad: true
  nonMusicLists: true
  musicLists: true
  longAlerts: true
"#;

    /// The oemIcon block the projection app emits.
    ///
    /// Kept separate from the main document because the real one carries three base64 PNGs of a few
    /// KB each — the SHAPE is what matters here, not the payload. A single-size set is deliberately
    /// NOT what the app emits: iOS renders a label-only tile for that (device-confirmed, see
    /// `OemIconConfig::images`), so the app emits all three of Apple's sizes or none.
    #[test]
    fn pi_projection_app_oem_icon_shape_parses() {
        let y = r#"
oemIconConfig:
  visible: true
  label: "CarLink"
  images:
    - width: 120
      height: 120
      imageBase64: "iVBORw0KGgo="
    - width: 180
      height: 180
      imageBase64: "iVBORw0KGgo="
    - width: 256
      height: 256
      imageBase64: "iVBORw0KGgo="
"#;
        let vc = VehicleConfig::from_yaml(y.as_bytes()).expect("oemIcon block must parse");
        assert!(vc.oem_icon_config.visible, "visible must reach the config");
        assert_eq!(vc.oem_icon_config.label, "CarLink");
        assert_eq!(
            vc.oem_icon_config.images.len(),
            3,
            "all three of Apple's sizes, or iOS shows a label-only tile"
        );
        let sizes: Vec<i64> = vc.oem_icon_config.images.iter().map(|i| i.width).collect();
        assert_eq!(sizes, vec![120, 180, 256]);
        assert!(!vc.oem_icon_config.images[0].image_base64.is_empty());
    }

    #[test]
    fn pi_projection_app_document_parses_and_means_what_it_says() {
        let vc = VehicleConfig::from_yaml(PI_PROJECTION_APP_DOCUMENT.as_bytes())
            .expect("the projection app's generated config must parse");
        let dev = vc.apply(base());

        // Resolution comes from displayPanelsConfig, NOT from the video stream. If the generator
        // ever moves width/height under mainVideoStream these stay at the base() defaults and the
        // Pi advertises the wrong panel with no diagnostic anywhere.
        assert_eq!(
            (dev.display_width, dev.display_height),
            (1920, 1080),
            "pixelDimensions must reach DeviceConfig"
        );
        assert_eq!(dev.max_fps, 60, "maxFPS must be one of the two negotiable values");

        // HEVC-only is the whole point of the port's video path.
        assert!(vc.accessory_config.enables_hevc, "enablesHEVC must parse");
        assert!(vc.accessory_config.enables_focus_transfer);

        // hidConfig is nested INSIDE mainVideoStream in Apple's schema. A top-level hidConfig
        // parses as an unknown key and is dropped, so this assertion is the one that catches the
        // most tempting mistake.
        let hid = &vc.video_streams_config.main_video_stream.hid_config;
        assert!(hid.touch_screen_supports_multi_touch, "multi-touch must parse");
        assert!(hid.telephony_support);
        assert!(hid.media_buttons_support);
        assert!(hid.steering_wheel_support);
        assert_eq!(hid.touch_screen_mode, "High Fidelty", "Apple's own spelling");
        // Declared false rather than omitted, and must stay false: advertising a HID device that
        // never reports has broken session reconnect before.
        assert!(!hid.dpad_support);
        assert!(!hid.knob_support);

        // No cluster. AAOS owns HDMI port 1 as its own instrument cluster on this build, so a
        // CarPlay alt stream would contend for a surface the platform already claims.
        assert!(
            vc.video_streams_config.alt_video_streams.is_empty(),
            "the Pi port must not advertise an alt/cluster video stream"
        );
        assert!(vc.display_panels_config.alt_display_panels.is_empty());

        // limitedUI element selection. The ORDER is `elements()`'s, i.e. Apple's emission order —
        // note `musicLists` precedes `nonMusicLists`, which is the reverse of the integer ids CT5
        // CINEMO uses in its own API. Asserted verbatim rather than as a set so a reordering of the
        // wire emission cannot pass unnoticed.
        assert_eq!(
            vc.limited_ui_config.elements(),
            vec![
                "softKeyboard",
                "softPhoneKeypad",
                "musicLists",
                "nonMusicLists",
                "longUserAlert"
            ]
        );
    }
}
