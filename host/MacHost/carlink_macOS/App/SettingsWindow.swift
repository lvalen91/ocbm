// SettingsWindow.swift — the app's Settings window (CarLink menu ▸ Settings…, ⌘,). SwiftUI.
//
// The Configuration tab is the host-authoritative VehicleConfig editor: it drives the YAML the app
// pushes to the box at SUBSCRIBE (`VehicleConfigModel.shared.yaml`), replacing the old hardcoded
// template. Every field is grounded in Apple's CarPlay `AccessoryConfig`/VehicleConfig schema
// (docs/carplay/03_SDK_GROUND_TRUTH.md §2, read from the Xcode-local CarPlaySimulator plugin) + this project's box-consumed
// fields. Controls are chosen by value type: booleans → Toggle, resolution → numeric fields with
// validated ranges, frame rate → segmented Picker.
//
// The box consumes a growing subset (today: main resolution, enablesHEVC, enablesDPad); the rest are
// forward-compatible schema fields (serde ignores what the box doesn't yet read), so the full config
// is authored here and the box adopts each field as it learns to.
//
// LAYOUT (Settings reorganisation, DESIGN.md §6 Phase 0, 2026-09-04): this file holds the MODEL and
// its YAML emitter, `SettingsRootView` and the AppKit host. The views live in App/Settings/
// (FieldInfo.swift, VehicleTab.swift, AdapterTab.swift, DiagnosticsTab.swift), moved byte-for-byte.
// `VehicleConfigModel` must NOT move and its emitter members must keep their exact declaration
// lines: tools/regen_app_yaml_fixture.py extracts `var yaml: String {`, `altDisplayPanelsYAML`,
// `clusterInitialURL`, `accessoryFields()`, `metadataYAML`, `audioYAML`, `viewArea2YAML` and `limitedUIFields()`
// from THIS path by string anchor and compiles them against a stub model. Relocating or reflowing
// any of those silently disarms the app→box drift guard (check_app_yaml_fixture.py).

import AppKit
import CoreImage
import SwiftUI

// MARK: - Model

/// One custom `audio.formats[]` entry the box advertises: a stream type, an optional audioType, an
/// input codec (mic capture on that stream; "none" = output-only) and an output codec (playback). The
/// codec strings are the box's `audio_format_bit` tokens. Codable so the list persists in UserDefaults.
struct AudioFormatRow: Identifiable, Codable, Equatable {
    var id = UUID()
    var streamType: Int = 102
    var audioType: String = "media"
    var input: String = "none"
    var output: String = "aac_lc_48k_stereo"
}

/// The editable VehicleConfig. Persisted to UserDefaults; `yaml` renders the Apple-schema document.
@MainActor
final class VehicleConfigModel: ObservableObject {
    static let shared = VehicleConfigModel()

    // Frame-rate vocabulary (user directive 2026-07-12). 24 fps was dropped from the box's
    // vocabulary — offering it silently yielded the default, so it is no longer offered.
    // The PANEL envelope is `PanelRule` (VehicleConfig.swift), not a set of constants here: each
    // axis admits 480–3840 and the product floor is applied by the panel's own orientation, the
    // way `ViewArea2Rule` applies it to a view area. The per-axis `minWidth/maxWidth/minHeight/
    // maxHeight` (800–3840 x 480–2160) that lived here until 2026-09-07 were a landscape-shaped
    // envelope: a portrait 2160x3840 panel was silently squared to 2160x2160 and the wired portrait
    // view-area sweep measured nothing for five cases. Every consumer now reads `PanelRule`.
    static let frameRates = [30, 60]

    /// The persisted main-video resolution — reads the SAME `vc.*` keys `save()` writes, with the
    /// model's defaults. The single source of truth for the resolution the adapter sees; window/view
    /// construction seeds from here so geometry can never diverge from what the iPhone encodes
    /// (usable from nonisolated contexts and before the shared model is first constructed).
    nonisolated static func persistedMainResolution() -> (width: Int, height: Int) {
        let ud = UserDefaults.standard
        let w = ud.object(forKey: "vc.mainWidth") as? Int ?? 1920
        let h = ud.object(forKey: "vc.mainHeight") as? Int ?? 1080
        // Same validation rationale as the old DisplayResolution.saved: a tampered/corrupt default
        // must not produce a zero/negative aspect downstream.
        guard w > 0, h > 0, w <= 8192, h <= 8192 else { return (1920, 1080) }
        return (w, h)
    }

    nonisolated static func persistedMainAspect() -> CGFloat {
        let r = persistedMainResolution()
        return CGFloat(r.width) / CGFloat(r.height)
    }

    // Identity
    @Published var name: String { didSet { markDirty() } }

    // Connectivity — wireless CarPlay capability (our `wireless:` YAML extension). The box supervisor
    // reads it from the pushed config and brings the BT+WiFi radios up (advertising as "CarLink") while
    // this app is connected, idling them when it disconnects. WIRED USB CarPlay is ALWAYS available
    // regardless; this only toggles whether wireless is offered ALONGSIDE it (first-come-wins). Default on.
    @Published var wirelessEnabled: Bool { didSet { markDirty() } }
    // Hot-Handover (our `hot_handover:` YAML extension). false = "Standard" (Apple-conformant, the default):
    // a cable plugged into an ACTIVE wireless session is left charge-only and wireless keeps running — Apple
    // selects transport once at session start (wired-preferred) and never migrates a live session. true =
    // force a live wireless->wired switch on cable insert (a non-standard extension; transport-selection
    // research 2026-08-01 / docs/ops/05_AUDITS.md). Only meaningful when wireless is enabled.
    @Published var hotHandover: Bool { didSet { markDirty() } }
    // Pairing association model (our `pairing:` YAML extension). false = Just-Works (the proven Carlinkit
    // posture, no code); true = Numeric Comparison (both the iPhone and the app show a 6-digit code to
    // match — a more OEM-head-unit-like experience, experimental for a dongle). Default false.
    @Published var pairingNumericComparison: Bool { didSet { markDirty() } }
    /// Box waits for this app's Pair/Cancel instead of confirming its own side at once. Unreachable with iOS
    /// as the peer (it fails the exchange before any human can answer) — off by default, kept for other peers.
    @Published var pairingInteractiveAnswer: Bool { didSet { markDirty() } }
    // Android Auto (our `android_auto:` YAML extension, docs/androidauto/02_ARBITRATION.md). The box's session_supervisor reads
    // it: when an Android phone is on the USB bus and no CarPlay transport owns the box, it arms
    // `aa-bridge` (AOAP switch + byte pump) and reports `pmWiredAa`, on which this app runs its own AA
    // head-unit engine over CH_IP. Default ON — the box's own default is opt-out (`android_auto: false`).
    // CarPlay is unaffected either way: an iPhone always wins the CarPlay path first.
    @Published var androidAutoEnabled: Bool { didSet { markDirty() } }
    // Wi-Fi access point (our `wifi_ap:` YAML extension). The box's session_supervisor.sh
    // `wifi_ap_enabled()` greps the pushed document for an EXPLICIT `wifi_ap: false`; anything else
    // (absent, true, misspelt) means enabled. So the default is emitted as NOTHING — the document
    // stays byte-identical to every pre-2026-09-04 push — and only a deliberate `false` reaches the
    // wire (`yaml`, beside the other emit-nothing-for-default blocks). false = the BT-only bridge
    // role (radio_ap_up.sh: Bluetooth + MFi only, no SoftAP is raised, so no wireless projection
    // gets a Wi-Fi leg). Until 2026-09-04 this key had no UI at all (DESIGN.md §7 defect 8).
    @Published var wifiAccessPoint: Bool { didSet { markDirty() } }

    // Main video
    @Published var mainWidth: Int { didSet { markDirty() } }
    @Published var mainHeight: Int { didSet { markDirty() } }
    @Published var maxFPS: Int { didSet { markDirty() } }

    // Main video — safe-area insets (px from each edge; 0 = flush, no inset). The video always fills
    // the full resolution; the safe area is the inset box CarPlay keeps its UI inside (curved panels).
    @Published var mainSafeLeft: Int { didSet { markDirty() } }
    @Published var mainSafeTop: Int { didSet { markDirty() } }
    @Published var mainSafeRight: Int { didSet { markDirty() } }
    @Published var mainSafeBottom: Int { didSet { markDirty() } }
    @Published var mainDrawOutsideSafe: Bool { didSet { markDirty() } }
    // Second MAIN view area (the CarPlay Dock "resize" button), in PANEL pixels — app-driven since
    // 2026-09-05; the box-side `CARPLAY_VIEWAREA2` lever is now the app-less fallback only. The
    // model VALIDATES (`viewArea2Verdict`, via `ViewArea2Rule`) and CLAMPS (`clampInPlace`); the
    // view renders the verdict. Emitted as the main stream's `viewAreas[1]` ONLY when enabled AND
    // the verdict is nil, so an off or invalid rect leaves the pushed YAML byte-identical.
    @Published var viewArea2Enabled: Bool { didSet { markDirty() } }
    @Published var viewArea2X: Int { didSet { markDirty() } }
    @Published var viewArea2Y: Int { didSet { markDirty() } }
    @Published var viewArea2W: Int { didSet { markDirty() } }
    @Published var viewArea2H: Int { didSet { markDirty() } }
    // How long CarPlay animates the Dock resize between view areas, in ms — the box's
    // `animationDurationMillis` on its `updateViewArea` answer (our top-level `view_area_anim_ms:`
    // YAML extension; device-proven 2026-09-09: 1000 visibly beat the hardcoded 3000). Absent on the
    // wire = 3000, so the emitter writes the key ONLY when the value differs and every existing
    // document stays byte-identical (the `wifi_ap` idiom, DESIGN.md §2). Range 0…10000, clamped.
    @Published var viewAreaAnimMs: Int { didSet { markDirty() } }
    /// What the last `clampInPlace()` CHANGED, one line each; empty when it stored every value as
    /// typed. Not persisted, never dirties the model. The Vehicle tab renders it under the panel
    /// fields and the control socket returns it from `set` / `save` / `get profile` — the honesty
    /// signal for a clamp (2026-09-07), the same shape as `AACapability.negotiationNotes`. A clamp
    /// that left no trace is how a landscape-only panel envelope survived a whole wired sweep.
    @Published private(set) var clampNotes: [String] = []

    // Alt / Nav video (instrument cluster / secondary panel)
    @Published var altVideoEnabled: Bool { didSet { markDirty() } }
    @Published var altWidth: Int { didSet { markDirty() } }
    @Published var altHeight: Int { didSet { markDirty() } }
    @Published var altFPS: Int { didSet { markDirty() } }
    // Alt video — safe-area insets (same semantics as main).
    @Published var altSafeLeft: Int { didSet { markDirty() } }
    @Published var altSafeTop: Int { didSet { markDirty() } }
    @Published var altSafeRight: Int { didSet { markDirty() } }
    @Published var altSafeBottom: Int { didSet { markDirty() } }
    @Published var altDrawOutsideSafe: Bool { didSet { markDirty() } }

    // Codec / audio
    @Published var enablesHEVC: Bool { didSet { markDirty() } }
    @Published var enablesMainBufferedAudio: Bool { didSet { markDirty() } }

    // Audio capability set — the declarative `audio:` section the box reads to build the advertised
    // `audioFormats`. `audioMode` selects HOW the set is chosen; `audioFormats` holds the custom entries.
    //   "auto"       → emit BOTH per-transport arms (audio.wired=wired_pcm / audio.wireless=wireless_8);
    //                  the box presents the arm matching the session transport (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B5 —
    //                  byte-equivalent to the box's old transport default)
    //   "wired_pcm"  → `audio: {preset: wired_pcm}`
    //   "wireless_8" → `audio: {preset: wireless_8}` (the full 8-entry AAC set: media + Siri/mic + alerts)
    //   "custom"     → `audio: {formats: [...]}` authored below (fully declarative — any HU audio config)
    @Published var audioMode: String { didSet { markDirty() } }
    @Published var audioFormats: [AudioFormatRow] { didSet { markDirty() } }

    static let audioModes = ["auto", "wired_pcm", "wireless_8", "custom"]

    /// One `chargingConnectors[]` row: a connector type plus its optional power rating in watts.
    ///
    /// Apple models the rating as a SEPARATE per-type sub-parameter (`PowerForConnectorTypeCCS2` and
    /// friends), not as a field of the connector — and each of those is single-valued, which is why
    /// the box drops a duplicate connector type rather than emitting the sub twice. The UI should
    /// therefore not offer the same type twice; if it slips through, the box keeps the first row.
    struct ConnectorRow: Identifiable, Equatable, Codable {
        var id = UUID()
        var type: String = "ccs2"
        /// `nil`/0 = omit the power sub entirely. An absent rating must not become a zero one.
        var powerWatts: UInt32? = nil

        enum CodingKeys: String, CodingKey { case type, powerWatts }
    }

    /// iAP2 metadata declaration tier (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B3) — WHICH Start*/Update message ids the accessory
    /// declares in Identify params 6/7 and then subscribes to. Replaces the box's `CARPLAY_METADATA`
    /// / `/tmp/carplay_metadata` bench levers as the primary control.
    ///
    /// Ships as `proven` — byte-equivalent to the box's compiled floor, so adopting this control
    /// changes nothing on the wire until it is deliberately raised. `rx-only` is NOT offered: it is
    /// a refuted dead end (docs/carplay/05_METADATA_AND_CONTROLS.md §6.2) and the box refuses it even if hand-authored.
    @Published var metadataTier: String { didSet { markDirty() } }
    /// Feature names dropped from the declaration (comma-separated in the UI, e.g. "call_history").
    @Published var metadataSkip: String { didSet { markDirty() } }
    static let metadataTiers = ["proven", "extended", "all"]

    // ---- Vehicle identity (docs/carplay/04_CAPABILITIES_AND_CONFIG.md C6/C7) — the EV-telematics foundation ----
    //
    // Emitted as `accessoryName:` and the `iapConfig:` block. C-3 landed (2026-09-02): iap2d's
    // Action::SendIdentify handler passes `vehicle_identity_from(&cfg)` into
    // `message::build_ident_info_with(...)`, so `engineTypes`/`chargingConnectors` reach the wired
    // Identify (param 20) on the next phone plug. `accessoryName` (C-6, param 21) is still
    // parse-only — no production caller of `accessory_name_bounded` exists yet. Either way, params
    // 20/21 are Identify content and an iOS `0x1D03` rejection cannot be retried within a session.

    /// The name the owner gives THIS box; the iPhone displays it. Empty = keep the box's built-in
    /// per-device name (`CarLink-<wifi-suffix>`), which is what ships today.
    @Published var accessoryName: String { didSet { markDirty() } }
    /// Engine types this vehicle has. MULTI-select: a hybrid is genuinely two entries, which Apple's
    /// spec allows (param-20 sub 2 is `[0+]`, verified from the compiled spec archive).
    @Published var engineTypes: Set<String> { didSet { markDirty() } }
    /// Charging connectors, as `type:powerWatts` pairs (power optional). Only meaningful for
    /// electric/hybrid vehicles; ignored by iOS otherwise.
    @Published var chargingConnectors: [ConnectorRow] { didSet { markDirty() } }
    /// Whether to declare a `VehicleStatusComponent` (Identify param 21) at all.
    ///
    /// ⚠️ LEAVE THIS OFF until docs/carplay/04_CAPABILITIES_AND_CONFIG.md workstream C-4 lands. The box declares none of
    /// 0xA100/0xA101/0xA102 in its params 6/7 today, so declaring the component advertises a
    /// capability whose messages are never declared — the same shape as
    /// `OptionalMsgNotValidWithoutRequiredMsgs`, and a live `0x1D03` risk.
    @Published var vehicleStatusEnabled: Bool { didSet { markDirty() } }
    /// Which status capabilities to declare (Apple's own field names, lowerCamelCased).
    @Published var vehicleStatusCaps: Set<String> { didSet { markDirty() } }
    /// `steeringWheelSupport` — the one hidConfig field the app did not previously emit. Drives the
    /// DirectionButtons display-feature bit (0x20).
    @Published var steeringWheelSupport: Bool { didSet { markDirty() } }

    /// Apple's four EngineType enum values.
    static let engineTypeNames = ["gasoline", "diesel", "electric", "cng"]
    /// Human labels for the four; the YAML always carries the enum name, never these.
    static let engineDisplayNames = [
        "gasoline": "Gasoline / petrol", "diesel": "Diesel", "electric": "Electric", "cng": "CNG",
    ]
    /// Apple's nine SupportedChargingConnectors values, in enum order.
    static let connectorNames = [
        "ccs1", "ccs2", "j1772", "chademo", "mennekes", "gbt_dc", "gbt_ac", "nacs_dc", "nacs_ac",
    ]
    /// Param-21 capability flags. The unified `rangeWarning` and the per-engine `rangeWarning*` are
    /// MUTUALLY EXCLUSIVE by Apple's own spec note ("Do not include if vehicle reports unified range
    /// warning for all EngineTypes"), so the UI presents that as a choice and the box refuses the
    /// forbidden combination if one is hand-authored anyway.
    static let vehicleStatusCapNames = [
        "range", "rangeGasoline", "rangeDiesel", "rangeElectric", "rangeCNG",
        "rangeWarning",
        "rangeWarningGasoline", "rangeWarningDiesel", "rangeWarningElectric", "rangeWarningCNG",
        "outsideTemperature", "insideTemperature", "wiperStatus", "barometricPressure",
        "alerts", "passengerSeatStatus", "electricChargeInfo", "maxRangeInfo",
    ]
    /// THE C-4 GATE. `false` until the adapter declares 0xA100/0xA101/0xA102 in its identification.
    ///
    /// This is deliberately a compile-time constant gating the EMITTER, not just the UI control.
    /// A warning shown while authoring fires at the wrong moment: the setting persists, reloads and
    /// re-pushes on every connection, so an owner who ticked the box today would have it take effect
    /// automatically on their first session after the adapter support lands — no second prompt, no
    /// re-consent, and a rejected identification cannot be retried until the phone is replugged.
    /// Flip this in the SAME commit that adds those message ids to the declaration table.
    static let vehicleStatusUnlocked = false

    /// The per-engine range-warning flags, kept here so the UI and the emitter agree on which set is
    /// exclusive with the unified one.
    static let perEngineRangeWarnings: Set<String> = [
        "rangeWarningGasoline", "rangeWarningDiesel", "rangeWarningElectric", "rangeWarningCNG",
    ]
    /// The audio-format vocabulary the box + app support — mirrors `receiver::info::audio_format_bit`.
    /// These are the exact `in:`/`out:` tokens the box parses. `verifiedCodecs` is the subset device-
    /// proven end-to-end today (the rest advertise + negotiate but are not yet decode-confirmed on-box).
    /// ALIASES (2026-09-04) of `CarPlayExtensions.AudioFormat.codecs` / `.types` /
    /// `.streamTypes`. The lists moved into that Foundation-only file so the harness — which cannot
    /// compile THIS file (AppKit/SwiftUI, `@MainActor`) — can test `AudioFormat.validated()`, the
    /// guard that stops a hand-edited profile document putting free text into the pushed YAML.
    /// Kept under these names because the UI Pickers and `save()` already read them.
    static let audioCodecs = CarPlayExtensions.AudioFormat.codecs
    static let verifiedCodecs: Set<String> = [
        "none", "pcm_16k_mono", "pcm_48k_stereo", "aac_lc_48k_stereo", "aac_eld_16k_mono",
    ]
    /// `audioType` values iOS routes against (empty = the wired PCM catch-all — no audioType key).
    static let audioTypes = CarPlayExtensions.AudioFormat.types
    /// CarPlay audio stream types the box arms: 100 MainAudio (bidir, carries mic), 101 AltAudio,
    /// 102 MainHighAudio (realtime media, AAC-LC).
    static let audioStreamTypes = CarPlayExtensions.AudioFormat.streamTypes
    /// The seed for a fresh "custom" list: the three device-proven entries (media, Siri/mic, PCM
    /// compatibility) — a working baseline the user then edits toward the HU config under test.
    static let defaultCustomFormats: [AudioFormatRow] = [
        AudioFormatRow(streamType: 102, audioType: "media", input: "none", output: "aac_lc_48k_stereo"),
        AudioFormatRow(streamType: 100, audioType: "speechRecognition", input: "aac_eld_16k_mono", output: "aac_eld_16k_mono"),
        AudioFormatRow(streamType: 100, audioType: "compatibility", input: "pcm_16k_mono", output: "pcm_48k_stereo"),
    ]

    // Input — Apple `videoStreamsConfig.mainVideoStream.hidConfig` block (the SDK-correct home for
    // HID control support; see the CarPlaySimulator VehicleConfig templates). `primaryInput` is a
    // sibling of hidConfig. touchScreenMode is an enum string ("High Fidelty" [sic, Apple's spelling]
    // / "Disabled").
    @Published var dPadSupport: Bool { didSet { markDirty() } }
    @Published var knobSupport: Bool { didSet { markDirty() } }
    @Published var knobSupportsHomeAndBackButton: Bool { didSet { markDirty() } }
    @Published var knobSupportsNudge: Bool { didSet { markDirty() } }
    @Published var mediaButtonsSupport: Bool { didSet { markDirty() } }
    @Published var telephonyButtonsSupport: Bool { didSet { markDirty() } }
    @Published var touchpadSupport: Bool { didSet { markDirty() } }
    @Published var touchpadButtonsSupport: Bool { didSet { markDirty() } }
    @Published var touchScreenHighFidelity: Bool { didSet { markDirty() } }
    @Published var touchScreenSupportsCancel: Bool { didSet { markDirty() } }
    /// `hidConfig.touchScreenSupportsMultiTouch` (verify_06 10-M4, owner decision 2026-09-02): a LIVE
    /// lever the app never emitted. Box: `vehicle_config.rs:642` parses it, `airplayd main.rs:767`
    /// swaps the uid-1 touch HID descriptor to two-finger and clears its contact state. Default OFF —
    /// multi-touch is untested on this project, matching the box's own `#[serde(default)]` (false).
    @Published var touchScreenSupportsMultiTouch: Bool { didSet { markDirty() } }
    @Published var primaryInput: String { didSet { markDirty() } }
    // Apple's authoritative values only — "Touchscreen" is not a valid CarPlay primaryInput
    // (absent from every CarPlay Simulator vehicle config).
    static let primaryInputs = ["Touchpad", "Knobs"]

    // Appearance / display
    // NOT emitted in `yaml` (verify_06 10 open question #2, owner decision 2026-09-02): the box has
    // no consumer for either key (`vehicle_config.rs` parses and drops both), so pushing them was
    // pure noise on every SUBSCRIBE. Persistence stays (UserDefaults + the toggle in the UI) in case
    // a future box-side consumer lands; only the wire emission was dropped.
    @Published var nightMode: Bool { didSet { markDirty() } }
    @Published var rightHandDrive: Bool { didSet { markDirty() } }

    // Neutral vehicle-profile fields (Settings reorganisation, DESIGN.md §5, 2026-09-04). These are
    // the protocol-agnostic SOURCE both renderers derive from: `VehicleProfile` (W1's `profile`
    // extension) and `AACapability` (Android Auto). NONE of them is read by the CarPlay emitter
    // below — the box has no consumer for any of them — so the pushed YAML is unchanged for every
    // existing configuration. `nightMode` / `rightHandDrive` above stay stored and `save()` keeps
    // writing them so a DOWNGRADE to a build that only knows those two keys still reads sane
    // values; on this build `driverPosition` / `theme` are authoritative and the legacy pair is
    // derived from them (W1). Both were seeded ONCE from the legacy booleans by the
    // `profileKeysV1` migration in `init`.
    //
    // `driverPosition` = `DriverPosition` raw value ("left" / "right" / "center"; center declares
    // AA wire 3 and is unverified on device). `theme` = `AppearanceTheme` raw value ("auto" /
    // "light" / "dark"; auto = follow this Mac's effective appearance — DHU `uitheme` is NOT a wire
    // field, gearhead only ever sees the night_mode sensor, DESIGN.md §10).
    @Published var driverPosition: String { didSet { markDirty() } }
    @Published var theme: String { didSet { markDirty() } }
    // Panel physical description. `dpi` is what Android Auto declares as `density` (replaces the
    // `AA_DENSITY` default; CarPlay has no wire field for it). `diagonalInches` is informational,
    // 0 = unknown (nil in the profile).
    @Published var dpi: Int { didSet { markDirty() } }
    @Published var diagonalInches: Double { didSet { markDirty() } }
    // Status-bar policy — recorded only; neither renderer sends it until the AA field numbers are
    // confirmed (FeatureMatrix `.statusBar`).
    @Published var hideClock: Bool { didSet { markDirty() } }
    @Published var hideSignal: Bool { didSet { markDirty() } }
    @Published var hideBattery: Bool { didSet { markDirty() } }
    // The three `DrivingRestrictionSet` members with NO CarPlay `limitedUIConfig` element (video /
    // voiceInput / configuration are Android Auto `driving_status` bits only). The Apple five keep
    // living on `limitedUI*` below so the emitter's inputs are untouched.
    @Published var restrictVideo: Bool { didSet { markDirty() } }
    @Published var restrictVoiceInput: Bool { didSet { markDirty() } }
    @Published var restrictConfiguration: Bool { didSet { markDirty() } }
    // Audio profile: the voice/mic sink rate AA negotiates (replaces the `AA_VOICE_RATE` default;
    // the env var still overrides on the bench) and whether calls ride the projection link.
    @Published var voiceRateHz: Int { didSet { markDirty() } }
    @Published var telephonyOverProjection: Bool { didSet { markDirty() } }
    // Data feeds, individually (AA service descriptors, replacing `AA_METADATA`). CarPlay's own
    // `metadataTier` picker below governs its declaration and is untouched.
    @Published var metadataNowPlaying: Bool { didSet { markDirty() } }
    @Published var metadataNavigation: Bool { didSet { markDirty() } }
    @Published var metadataTelephony: Bool { didSet { markDirty() } }
    // Android Auto–exclusive rendering knobs (`AndroidAutoExtensions`): fit a non-tier panel with
    // margins instead of snapping to the nearest tier; prefer HEVC at ≤1080p.
    @Published var aaFitPanelWithMargins: Bool { didSet { markDirty() } }
    @Published var aaPreferHEVC: Bool { didSet { markDirty() } }

    @Published var enablesUIAppearance: Bool { didSet { markDirty() } }
    @Published var enablesMapAppearance: Bool { didSet { markDirty() } }
    @Published var enablesCornerMasks: Bool { didSet { markDirty() } }

    // Feature capabilities (Apple AccessoryConfig `enables*` set — docs/carplay/03_SDK_GROUND_TRUTH.md §2)
    @Published var enablesVideoPlayback: Bool { didSet { markDirty() } }
    @Published var enablesViewAreas: Bool { didSet { markDirty() } }
    @Published var enablesEnhancedSiri: Bool { didSet { markDirty() } }
    @Published var enablesFocusTransfer: Bool { didSet { markDirty() } }
    @Published var enablesUIContext: Bool { didSet { markDirty() } }
    @Published var enablesUISync: Bool { didSet { markDirty() } }
    @Published var enablesFileTransfer: Bool { didSet { markDirty() } }
    @Published var enablesLogTransfer: Bool { didSet { markDirty() } }
    @Published var enablesVehicleDataProtocol: Bool { didSet { markDirty() } }
    @Published var enablesDCX: Bool { didSet { markDirty() } }
    // App-driven SETUP (our `accessoryConfig.appDrivenSetup` extension; plan P3). When true the box
    // relays the RTSP/SETUP negotiation to this app over CH_RTSP and the app AUTHORS the response the
    // phone sees; the box's own local response is the fallback. Default OFF — the box only relays when
    // the pushed YAML sets it, so a stock config stays fully box-driven.
    @Published var appDrivenSetup: Bool { didSet { markDirty() } }

    // Limited UI elements — Apple `limitedUIConfig` (top-level key; box-side `LimitedUiConfig` in
    // `vehicle_config.rs`). Selects WHICH UI elements iOS restricts while limited-UI mode is on; the
    // runtime on/off itself is the Controls window's `/command setLimitedUI`. When the section is
    // disabled, nothing is emitted and iOS keeps its own default restriction set (`/info` stays
    // byte-identical to a build without the feature — the proven default behavior).
    @Published var limitedUIConfigEnabled: Bool { didSet { markDirty() } }
    @Published var limitedUISoftKeyboard: Bool { didSet { markDirty() } }
    @Published var limitedUISoftPhoneKeypad: Bool { didSet { markDirty() } }
    @Published var limitedUIMusicLists: Bool { didSet { markDirty() } }
    @Published var limitedUINonMusicLists: Bool { didSet { markDirty() } }
    @Published var limitedUIJapanMaps: Bool { didSet { markDirty() } }
    @Published var limitedUILongAlerts: Bool { didSet { markDirty() } }
    // The remaining four REAL Apple `LimitedUIConfig` CodingKeys. The box parses them ONLY for YAML
    // round-trip (vehicle_config.rs `LimitedUiConfig`); Apple's own `airPlayElements` emission
    // EXCLUDES them, so they NEVER reach /info `limitedUIElements`. Carried here so exported YAML
    // round-trips the full Apple schema — presented in the UI under an explicit "never emitted" caption.
    @Published var limitedUIPairedDevices: Bool { didSet { markDirty() } }
    @Published var limitedUIThemeCustomization: Bool { didSet { markDirty() } }
    @Published var limitedUIAutomakerSettings: Bool { didSet { markDirty() } }
    @Published var limitedUIAutomakerSettingsInfoButton: Bool { didSet { markDirty() } }

    // OEM icon (Apple `oemIconConfig`): the vehicle-maker logo on the CarPlay home screen. Static
    // config, emitted in /info only when enabled + an image is set. `oemIconBase64` is the PNG bytes.
    @Published var oemIconEnabled: Bool { didSet { markDirty() } }   // advertise oemIconConfig at all
    @Published var oemIconVisible: Bool { didSet { markDirty() } }   // oemIconVisible: show/hide on screen
    @Published var oemIconLabel: String { didSet { markDirty() } }   // oemIconLabel: the name shown
    @Published var oemIconBase64: String { didSet { markDirty() } }  // oemIcons: the PNG (base64)
    @Published var oemIconW: Int { didSet { markDirty() } }
    @Published var oemIconH: Int { didSet { markDirty() } }

    /// True when the form has edits not yet committed with Save. Drives the "Unsaved changes" hint.
    @Published private(set) var dirty = false
    /// The YAML captured at the last Save — this is what the app PUSHES to the box on the next
    /// connection. Editing the form updates the live `yaml` preview but NOT what's pushed until Save.
    private(set) var committedYAML: String = ""
    /// The structured config captured at the SAME instant as `committedYAML` (audit A4). App-driven SETUP
    /// must author from THIS, not the live `config`: the box is pushed `committedYAML`, so authoring from
    /// live @Published fields would let unsaved form edits make the phone's SETUP response contradict what
    /// the box advertised. nil only before the first load (launch sets it); callers fall back to `config`.
    private(set) var committedConfig: VehicleConfig?

    private var loading = false
    private var saving = false
    private let d = UserDefaults.standard
    private static let prefix = "vc."

    func markDirty() {
        guard !loading, !saving else { return }
        dirty = true
    }

    /// ONE-SHOT MIGRATION (Settings reorganisation, DESIGN.md §5, 2026-09-04): `driverPosition` and
    /// `theme` replace the `rightHandDrive` / `nightMode` booleans as the authoritative neutral
    /// values. Seed them from the legacy pair EXACTLY ONCE, so an owner who had right-hand drive or
    /// night mode set keeps it across the upgrade; afterwards the new keys are independent, and the
    /// legacy booleans — which `save()` keeps writing as DERIVED values for a downgrade — must never
    /// overwrite them again. Same idiom as `mbaDefaultFlippedB4` in `init`: a marker key checked for
    /// ABSENCE, so this runs once per defaults domain and is a no-op on every later launch. Takes
    /// the domain as a parameter (rather than reading `UserDefaults.standard` inline) so the test
    /// harness can run it against a throwaway suite (DESIGN.md §9 test 7 pins both halves).
    /// DELEGATES to `VehicleProfileKeyMigration.run` in the Foundation-only `App/VehicleConfig.swift`.
    /// The body moved there so `tests/run_tests.sh` — which cannot compile this AppKit/SwiftUI,
    /// `@MainActor` file — exercises the SHIPPED migration instead of a hand-copied twin of it
    /// (DESIGN.md §9 test 7). Keep this thin: the logic has exactly one home.
    static func migrateProfileKeysV1(_ ud: UserDefaults) {
        VehicleProfileKeyMigration.run(prefix: prefix, ud: ud)
    }

    private init() {
        loading = true
        let ud = UserDefaults.standard
        // ONE-SHOT MIGRATION (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B4): `enablesMainBufferedAudio` shipped defaulting to TRUE while
        // the box ignored it, and `accessoryFields()` is both the emission AND the persistence list —
        // so any Save ever performed wrote `true` into UserDefaults. Now that the box ARMS from this
        // key, a persisted `true` would advertise mainBuffered on every session (wireless included,
        // where iOS moving media to a buffered stream we don't serve silences audio). A default flip
        // alone can't fix that: `b()` only applies a default when the key is ABSENT. Drop the stale
        // key once so the new default (OFF) actually takes effect; the user can re-enable deliberately.
        if ud.object(forKey: Self.prefix + "mbaDefaultFlippedB4") == nil {
            ud.removeObject(forKey: Self.prefix + "enablesMainBufferedAudio")
            ud.set(true, forKey: Self.prefix + "mbaDefaultFlippedB4")
        }
        Self.migrateProfileKeysV1(ud)
        func b(_ k: String, _ def: Bool) -> Bool { ud.object(forKey: Self.prefix + k) as? Bool ?? def }
        func i(_ k: String, _ def: Int) -> Int { ud.object(forKey: Self.prefix + k) as? Int ?? def }
        name = ud.string(forKey: Self.prefix + "name") ?? "CarLink Widescreen"
        wirelessEnabled = b("wirelessEnabled", true)
        hotHandover = b("hotHandover", false)
        pairingNumericComparison = b("pairingNumericComparison", false)
        pairingInteractiveAnswer = b("pairingInteractiveAnswer", false)
        androidAutoEnabled = b("androidAutoEnabled", true)
        wifiAccessPoint = b("wifiAccessPoint", true)
        mainWidth = i("mainWidth", 1920); mainHeight = i("mainHeight", 1080); maxFPS = i("maxFPS", 60)
        mainSafeLeft = i("mainSafeLeft", 0); mainSafeTop = i("mainSafeTop", 0)
        mainSafeRight = i("mainSafeRight", 0); mainSafeBottom = i("mainSafeBottom", 0)
        mainDrawOutsideSafe = b("mainDrawOutsideSafe", false)
        viewArea2Enabled = b("viewArea2Enabled", false)
        viewArea2X = i("viewArea2X", 0); viewArea2Y = i("viewArea2Y", 0)
        viewArea2W = i("viewArea2W", 0); viewArea2H = i("viewArea2H", 0)
        viewAreaAnimMs = i("viewAreaAnimMs", 3000)
        altVideoEnabled = b("altVideoEnabled", false)
        altWidth = i("altWidth", 800); altHeight = i("altHeight", 480); altFPS = i("altFPS", 30)
        altSafeLeft = i("altSafeLeft", 0); altSafeTop = i("altSafeTop", 0)
        altSafeRight = i("altSafeRight", 0); altSafeBottom = i("altSafeBottom", 0)
        altDrawOutsideSafe = b("altDrawOutsideSafe", false)
        enablesHEVC = b("enablesHEVC", true)
        // Default OFF (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B4): the box now arms mainBuffered FROM this key per connection, and
        // Phase A advertises without serving — a default-on push would fire every session and risk
        // silent media if iOS moves to a buffered stream. Deliberate per-session opt-in only.
        enablesMainBufferedAudio = b("enablesMainBufferedAudio", false)
        // Audio capability set. Default "auto" = push both per-transport arms explicitly (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B5;
        // byte-equivalent to the proven transport-gated behavior on the wire). A stale/unknown
        // persisted mode coerces to "auto" so a bad value can't break the config.
        let savedAudioMode = ud.string(forKey: Self.prefix + "audioMode") ?? "auto"
        audioMode = Self.audioModes.contains(savedAudioMode) ? savedAudioMode : "auto"
        // Metadata tier (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B3). Ships `proven` = the box's compiled floor, so this control is
        // a wire no-op until deliberately raised; a stale/unknown persisted value coerces to it.
        let savedTier = ud.string(forKey: Self.prefix + "metadataTier") ?? "proven"
        metadataTier = Self.metadataTiers.contains(savedTier) ? savedTier : "proven"
        metadataSkip = ud.string(forKey: Self.prefix + "metadataSkip") ?? ""
        // Vehicle identity (C6/C7). All default to "absent", so an existing install keeps emitting
        // exactly what it emitted before this feature — the workstream is absent-off end to end.
        accessoryName = ud.string(forKey: Self.prefix + "accessoryName") ?? ""
        engineTypes = Set(ud.stringArray(forKey: Self.prefix + "engineTypes") ?? [])
        if let raw = ud.data(forKey: Self.prefix + "chargingConnectors"),
           let rows = try? JSONDecoder().decode([ConnectorRow].self, from: raw) {
            chargingConnectors = rows
        } else {
            chargingConnectors = []
        }
        vehicleStatusEnabled = ud.bool(forKey: Self.prefix + "vehicleStatusEnabled")
        vehicleStatusCaps = Set(ud.stringArray(forKey: Self.prefix + "vehicleStatusCaps") ?? [])
        if let raw = ud.data(forKey: Self.prefix + "audioFormats"),
           let rows = try? JSONDecoder().decode([AudioFormatRow].self, from: raw) {
            audioFormats = rows
        } else {
            audioFormats = Self.defaultCustomFormats
        }
        dPadSupport = b("dPadSupport", true)
        knobSupport = b("knobSupport", false)
        knobSupportsHomeAndBackButton = b("knobSupportsHomeAndBackButton", false)
        knobSupportsNudge = b("knobSupportsNudge", false)
        mediaButtonsSupport = b("mediaButtonsSupport", true)
        telephonyButtonsSupport = b("telephonyButtonsSupport", false)
        touchpadSupport = b("touchpadSupport", false)
        touchpadButtonsSupport = b("touchpadButtonsSupport", false)
        touchScreenHighFidelity = b("touchScreenHighFidelity", true)
        touchScreenSupportsCancel = b("touchScreenSupportsCancel", true)
        touchScreenSupportsMultiTouch = b("touchScreenSupportsMultiTouch", false)
        // Uses the `b(_:_:)` helper like its siblings rather than `ud.bool(forKey:)`: the helper
        // distinguishes "absent" from "false", which is what makes a future default flip
        // actually reach existing installs (the B4 lesson, docs/carplay/04_CAPABILITIES_AND_CONFIG.md).
        steeringWheelSupport = b("steeringWheelSupport", false)
        // Apple's vehicle configs only ever use "Touchpad" (9/10) or "Knobs" (1/10); "Touchscreen"
        // appears nowhere in the CarPlay Simulator bundle and, as an unknown enum raw value, a strict
        // Codable decoder would THROW and reject the whole config. Default to Touchpad and coerce any
        // stale/invalid persisted value (including a legacy "Touchscreen") to a valid one.
        let savedPrimary = ud.string(forKey: Self.prefix + "primaryInput") ?? "Touchpad"
        primaryInput = Self.primaryInputs.contains(savedPrimary) ? savedPrimary : "Touchpad"
        nightMode = b("nightMode", false)
        rightHandDrive = b("rightHandDrive", false)
        // Neutral profile fields (DESIGN.md §5). The enum-valued strings coerce a stale/unknown
        // persisted value to the default, same as `audioMode` / `metadataTier` above — the raw
        // value must always round-trip through the contract enum, or W1's `profile` getter would
        // have to invent a fallback at every read.
        driverPosition = DriverPosition(rawValue: ud.string(forKey: Self.prefix + "driverPosition") ?? "")?.rawValue ?? "left"
        theme = AppearanceTheme(rawValue: ud.string(forKey: Self.prefix + "theme") ?? "")?.rawValue ?? "light"
        dpi = i("dpi", 160)
        diagonalInches = ud.object(forKey: Self.prefix + "diagonalInches") as? Double ?? 0
        hideClock = b("hideClock", false); hideSignal = b("hideSignal", false); hideBattery = b("hideBattery", false)
        restrictVideo = b("restrictVideo", false); restrictVoiceInput = b("restrictVoiceInput", false)
        restrictConfiguration = b("restrictConfiguration", false)
        voiceRateHz = i("voiceRateHz", 48000)
        telephonyOverProjection = b("telephonyOverProjection", false)
        metadataNowPlaying = b("metadataNowPlaying", true); metadataNavigation = b("metadataNavigation", true)
        metadataTelephony = b("metadataTelephony", true)
        aaFitPanelWithMargins = b("aaFitPanelWithMargins", true)
        aaPreferHEVC = b("aaPreferHEVC", false)
        // Apple sets both true in all 10 vehicle-config templates.
        enablesUIAppearance = b("enablesUIAppearance", true)
        enablesMapAppearance = b("enablesMapAppearance", true)
        enablesCornerMasks = b("enablesCornerMasks", false)
        enablesVideoPlayback = b("enablesVideoPlayback", true)
        enablesViewAreas = b("enablesViewAreas", false)
        enablesEnhancedSiri = b("enablesEnhancedSiri", false)
        enablesFocusTransfer = b("enablesFocusTransfer", false)
        enablesUIContext = b("enablesUIContext", false)
        enablesUISync = b("enablesUISync", false)
        enablesFileTransfer = b("enablesFileTransfer", false)
        enablesLogTransfer = b("enablesLogTransfer", false)
        enablesVehicleDataProtocol = b("enablesVehicleDataProtocol", false)
        enablesDCX = b("enablesDCX", false)
        appDrivenSetup = b("appDrivenSetup", true) // DEFAULT ON, both transports since 2026-08-10 (box-driven local response is the sticky fallback)
        limitedUIConfigEnabled = b("limitedUIConfigEnabled", false)
        limitedUISoftKeyboard = b("limitedUISoftKeyboard", false)
        limitedUISoftPhoneKeypad = b("limitedUISoftPhoneKeypad", false)
        limitedUIMusicLists = b("limitedUIMusicLists", false)
        limitedUINonMusicLists = b("limitedUINonMusicLists", false)
        limitedUIJapanMaps = b("limitedUIJapanMaps", false)
        limitedUILongAlerts = b("limitedUILongAlerts", false)
        limitedUIPairedDevices = b("limitedUIPairedDevices", false)
        limitedUIThemeCustomization = b("limitedUIThemeCustomization", false)
        limitedUIAutomakerSettings = b("limitedUIAutomakerSettings", false)
        limitedUIAutomakerSettingsInfoButton = b("limitedUIAutomakerSettingsInfoButton", false)
        oemIconEnabled = b("oemIconEnabled", false)
        oemIconVisible = b("oemIconVisible", true)
        oemIconLabel = ud.string(forKey: Self.prefix + "oemIconLabel") ?? "CarLink"
        oemIconBase64 = ud.string(forKey: Self.prefix + "oemIconBase64") ?? ""
        oemIconW = ud.object(forKey: Self.prefix + "oemIconW") as? Int ?? 0
        oemIconH = ud.object(forKey: Self.prefix + "oemIconH") as? Int ?? 0
        // Clamp before committing (L-2, verify_06 10-L2): a persisted out-of-range value (e.g. a
        // stale `maxFPS: 24` from an earlier build, or an out-of-bounds resolution) must not reach
        // the wire on launch just because the owner hasn't hit Save yet. Idempotent — only assigns
        // on change — and `loading` is still true, so it does not mark the model dirty.
        clampInPlace()
        loading = false
        committedYAML = yaml   // the persisted state IS the committed/pushed state at launch
        committedConfig = config   // snapshot the structured config alongside the YAML (audit A4)
    }

    /// Commit the current form: clamp, persist to UserDefaults, snapshot the pushed YAML, clear dirty.
    /// This is the ONLY thing that changes what the adapter receives on the next connection.
    ///
    /// Returns `false` (and leaves the previously-committed config untouched) if the rendered YAML
    /// would exceed the box's SUBSCRIBE frame budget (verify_06 10-L1 / L-1): `OCBMClient.send`
    /// refuses any payload > `OCBM.maxPayload` (65536 B), and without this guard an oversize document
    /// (an unbounded `name`/`accessoryName`/`oemIconLabel`/`metadataSkip`/`audioFormats`) wedges every
    /// SUBSCRIBE silently — the app sits at "waiting" with only a log line, forever.
    @discardableResult
    func save() -> Bool {
        guard !loading else { return true }
        // COMMIT ANY FIELD STILL BEING EDITED, before anything reads the model.
        //
        // `TextField(value:format:)` on macOS writes its binding only on Return or when focus moves
        // to another responder — and a SwiftUI Button does NOT take focus. So clicking Save (or ⌘S)
        // with the caret still in a numeric field silently persisted the OLD value while the field
        // kept displaying what was typed and the bar said "Saved". Measured 2026-09-05: an owner set
        // dpi to 200, `get profile` still read 160 with `settingsDirty:false`, and the model had
        // never held 200. It affects every numeric field here — width, height, dpi, diagonal, and the
        // safe-area insets — i.e. whichever one was edited LAST before saving.
        //
        // Ending editing on the key window makes the field write its binding synchronously; `didSet`
        // then marks dirty and the save proceeds with the real value. Harmless when nothing is being
        // edited, and harmless on the control-socket `save` path (no key window).
        NSApp.keyWindow?.makeFirstResponder(nil)
        saving = true
        defer { saving = false }
        clampInPlace()
        let candidateYAML = yaml
        let candidateSize = Data(candidateYAML.utf8).count
        // Reserve a few bytes for the OCBM frame header, matching L-1's stated margin.
        guard candidateSize <= OCBM.maxPayload - 1 else {
            let alert = NSAlert()
            alert.messageText = "Configuration Too Large"
            alert.informativeText = "The generated configuration is \(candidateSize) bytes, over the "
                + "adapter's \(OCBM.maxPayload) byte SUBSCRIBE limit. Shorten the name, accessory "
                + "name, OEM icon label, metadata skip list, or audio format list, then Save again."
            alert.alertStyle = .critical
            alert.runModal()
            return false
        }
        // Keep the PERSISTED name clean, using the SAME set the emitter strips (`YamlEmit`) — this
        // used to be `CharacterSet.controlCharacters`, which is Cc u Cf and silently destroyed
        // legitimate content: an emoji ZWJ sequence collapsed into separate glyphs right in the
        // text field, and RTL labels lost their bidi marks. Cf is not fatal to the parser and is
        // now preserved everywhere. (Fatal = Cc; embedded newlines/tabs would actually fold
        // harmlessly, contrary to what this comment claimed before 2026-08-10.)
        let cleanName = YamlEmit.stripFatalControls(name)
        if cleanName != name { name = cleanName }
        let s = Self.prefix
        d.set(name, forKey: s + "name")
        d.set(wirelessEnabled, forKey: s + "wirelessEnabled")
        d.set(hotHandover, forKey: s + "hotHandover")
        d.set(pairingNumericComparison, forKey: s + "pairingNumericComparison")
        d.set(pairingInteractiveAnswer, forKey: s + "pairingInteractiveAnswer")
        d.set(androidAutoEnabled, forKey: s + "androidAutoEnabled")
        d.set(wifiAccessPoint, forKey: s + "wifiAccessPoint")
        d.set(mainWidth, forKey: s + "mainWidth"); d.set(mainHeight, forKey: s + "mainHeight"); d.set(maxFPS, forKey: s + "maxFPS")
        d.set(mainSafeLeft, forKey: s + "mainSafeLeft"); d.set(mainSafeTop, forKey: s + "mainSafeTop")
        d.set(mainSafeRight, forKey: s + "mainSafeRight"); d.set(mainSafeBottom, forKey: s + "mainSafeBottom")
        d.set(mainDrawOutsideSafe, forKey: s + "mainDrawOutsideSafe")
        d.set(viewArea2Enabled, forKey: s + "viewArea2Enabled")
        d.set(viewArea2X, forKey: s + "viewArea2X"); d.set(viewArea2Y, forKey: s + "viewArea2Y")
        d.set(viewArea2W, forKey: s + "viewArea2W"); d.set(viewArea2H, forKey: s + "viewArea2H")
        d.set(viewAreaAnimMs, forKey: s + "viewAreaAnimMs")
        d.set(altVideoEnabled, forKey: s + "altVideoEnabled")
        d.set(altWidth, forKey: s + "altWidth"); d.set(altHeight, forKey: s + "altHeight"); d.set(altFPS, forKey: s + "altFPS")
        d.set(altSafeLeft, forKey: s + "altSafeLeft"); d.set(altSafeTop, forKey: s + "altSafeTop")
        d.set(altSafeRight, forKey: s + "altSafeRight"); d.set(altSafeBottom, forKey: s + "altSafeBottom")
        d.set(altDrawOutsideSafe, forKey: s + "altDrawOutsideSafe")
        d.set(primaryInput, forKey: s + "primaryInput")
        // nightMode / rightHandDrive are read back in init() but were missing from save() → reverted to
        // false on every relaunch (audit M-f). Persist them here.
        d.set(nightMode, forKey: s + "nightMode")
        d.set(rightHandDrive, forKey: s + "rightHandDrive")
        // Neutral profile fields (DESIGN.md §5) — persisted under their own `vc.` keys.
        d.set(driverPosition, forKey: s + "driverPosition")
        d.set(theme, forKey: s + "theme")
        d.set(dpi, forKey: s + "dpi")
        d.set(diagonalInches, forKey: s + "diagonalInches")
        d.set(hideClock, forKey: s + "hideClock"); d.set(hideSignal, forKey: s + "hideSignal"); d.set(hideBattery, forKey: s + "hideBattery")
        d.set(restrictVideo, forKey: s + "restrictVideo"); d.set(restrictVoiceInput, forKey: s + "restrictVoiceInput")
        d.set(restrictConfiguration, forKey: s + "restrictConfiguration")
        d.set(voiceRateHz, forKey: s + "voiceRateHz")
        d.set(telephonyOverProjection, forKey: s + "telephonyOverProjection")
        d.set(metadataNowPlaying, forKey: s + "metadataNowPlaying"); d.set(metadataNavigation, forKey: s + "metadataNavigation")
        d.set(metadataTelephony, forKey: s + "metadataTelephony")
        d.set(aaFitPanelWithMargins, forKey: s + "aaFitPanelWithMargins")
        d.set(aaPreferHEVC, forKey: s + "aaPreferHEVC")
        d.set(audioMode, forKey: s + "audioMode")
        d.set(metadataTier, forKey: s + "metadataTier")
        d.set(metadataSkip, forKey: s + "metadataSkip")
        // Same treatment as `name` above: strip control characters BEFORE persisting, so the
        // stored value matches what is emitted. Emission is independently safe (quotedBody
        // strips too) — this keeps the two from silently disagreeing.
        d.set(YamlEmit.stripFatalControls(accessoryName), forKey: s + "accessoryName")
        d.set(Array(engineTypes), forKey: s + "engineTypes")
        if let raw = try? JSONEncoder().encode(chargingConnectors) {
            d.set(raw, forKey: s + "chargingConnectors")
        }
        d.set(vehicleStatusEnabled, forKey: s + "vehicleStatusEnabled")
        d.set(Array(vehicleStatusCaps), forKey: s + "vehicleStatusCaps")
        if let raw = try? JSONEncoder().encode(audioFormats) { d.set(raw, forKey: s + "audioFormats") }
        for (k, v) in hidFields() { d.set(v, forKey: s + k) }
        for (k, v) in accessoryFields() { d.set(v, forKey: s + k) }
        d.set(limitedUIConfigEnabled, forKey: s + "limitedUIConfigEnabled")
        for (_, k, v) in limitedUIFields() { d.set(v, forKey: s + k) }
        d.set(oemIconEnabled, forKey: s + "oemIconEnabled")
        d.set(oemIconVisible, forKey: s + "oemIconVisible")
        d.set(oemIconLabel, forKey: s + "oemIconLabel")
        d.set(oemIconBase64, forKey: s + "oemIconBase64")
        d.set(oemIconW, forKey: s + "oemIconW")
        d.set(oemIconH, forKey: s + "oemIconH")
        committedYAML = yaml
        committedConfig = config   // snapshot the structured config alongside the YAML (audit A4)
        dirty = false
        // Apply it to a box we are ALREADY connected to. Before this, Save only affected the next
        // connection — `client.sessionConfig` was set once at connect — so every box-side lever in
        // the pushed YAML was inert for the life of a session. `repushConfig` re-SUBSCRIBEs, and
        // declines while a CarPlay transport owns the box (see its doc: the box's presence dip would
        // restart a live CarPlay session).
        CCPABridge.shared.client?.repushConfig(data(), structured: config)   // `config` == committedConfig here
        return true
    }

    /// Load an OEM-icon PNG from `url`: keep the ORIGINAL PNG bytes (base64), read the pixel dimensions
    /// from the bitmap rep, and arm the icon. Apple requires PNG for `oemIcons`/`oemIcon`.
    func loadOemIcon(from url: URL) {
        guard let data = try? Data(contentsOf: url), let rep = NSBitmapImageRep(data: data) else { return }
        oemIconBase64 = data.base64EncodedString()
        oemIconW = rep.pixelsWide
        oemIconH = rep.pixelsHigh
        oemIconEnabled = true
    }

    /// "Choose PNG…" — pick an OEM-icon PNG from disk.
    func pickOemIcon() {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.png]
        panel.allowsMultipleSelection = false
        if panel.runModal() == .OK, let url = panel.url { loadOemIcon(from: url) }
    }

    /// Render `src` into a `size`×`size` PNG, **aspect-preserving** (fit + centre + transparent pad — a
    /// non-square logo is never stretched). Full quality by default; `posterize: true` colour-reduces as a
    /// LAST resort to hit a byte budget for a very busy source.
    private func scaledIconPNG(_ src: NSImage, _ size: Int, posterize: Bool = false) -> Data? {
        guard let rep = NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size,
            bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
            colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { return nil }
        rep.size = NSSize(width: size, height: size)
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
        NSGraphicsContext.current?.imageInterpolation = .high
        // Aspect-preserving FIT (centred, transparent-padded) — never distort a non-square source.
        let s = src.size
        let k = (s.width > 0 && s.height > 0) ? min(CGFloat(size) / s.width, CGFloat(size) / s.height) : 1
        let w = s.width * k, h = s.height * k
        src.draw(in: NSRect(x: (CGFloat(size) - w) / 2, y: (CGFloat(size) - h) / 2, width: w, height: h),
                 from: .zero, operation: .copy, fraction: 1.0)
        NSGraphicsContext.restoreGraphicsState()
        guard let base = rep.cgImage else { return nil }
        if !posterize {
            return NSBitmapImageRep(cgImage: base).representation(using: .png, properties: [:])
        }
        let ctx = CIContext(options: nil)
        let ci = CIImage(cgImage: base)
        var smallest: Data? = nil
        for levels in [24, 16, 12, 8, 6, 5, 4] {
            guard let f = CIFilter(name: "CIColorPosterize",
                                   parameters: [kCIInputImageKey: ci, "inputLevels": levels]),
                  let out = f.outputImage,
                  let r = ctx.createCGImage(out, from: ci.extent),
                  let png = NSBitmapImageRep(cgImage: r).representation(using: .png, properties: [:])
            else { continue }
            smallest = png
            if png.count <= 9_500 { return png }
        }
        return smallest
    }

    /// Apple's multi-resolution OEM icon set from the stored source PNG. iOS renders only the label for a
    /// single-size `oemIcons` array (device-confirmed 2026-08-02) and selects by display DPI — on the test
    /// device it picks the 180. So we render 120/180/256 at FULL quality and drop the LARGEST sizes until
    /// the set fits a HARD total budget (the whole `/info` config must stay under the box's 64 KB SUBSCRIBE
    /// frame — a 74 KB config on 2026-08-02 wedged SUBSCRIBE at "Waiting for adapter"). Colour-reduction is
    /// only a last resort for a single oversized size — the common flat vehicle logo ships pristine.
    func oemIconVariants() -> [(Int, String)] {
        guard let data = Data(base64Encoded: oemIconBase64), let src = NSImage(data: data) else { return [] }
        let maxTotalBase64 = 48_000   // hard cap; leaves room for the rest of the ~5 KB config under 64 KB
        func b64(_ d: Data) -> Int { d.count * 4 / 3 }
        func total(_ a: [(Int, Data)]) -> Int { a.reduce(0) { $0 + b64($1.1) } }
        // Full-quality renders first.
        var built: [(Int, Data)] = []
        for sz in [120, 180, 256] { if let png = scaledIconPNG(src, sz) { built.append((sz, png)) } }
        // Drop the largest sizes until the pristine set fits (120+180 alone is enough — iOS uses 180).
        while total(built) > maxTotalBase64, built.count > 1 { built.removeLast() }
        // Only if a SINGLE full-quality size still overflows (busy/photographic art) colour-reduce it.
        if total(built) > maxTotalBase64, let sz = built.first?.0,
           let small = scaledIconPNG(src, sz, posterize: true), b64(small) <= maxTotalBase64 {
            built = [(sz, small)]
        }
        // Absolute floor: never emit an over-budget set (that would wedge SUBSCRIBE) — label-only instead.
        if total(built) > maxTotalBase64 { return [] }
        return built.map { ($0.0, $0.1.base64EncodedString()) }
    }

    /// "Use Simulator icon" — reuse the CarPlay Simulator's own `OEMIcon.png` (180×180 RGB) if present
    /// on this Mac. No-op if the Simulator isn't installed at the expected path.
    func useSimulatorOemIcon() {
        let p = NSHomeDirectory() + "/Documents/carlink/carplay_simulator/CarPlay Simulator.app/Contents/Resources/VehicleConfigs/Images/OEMImages/OEMIcon.png"
        if FileManager.default.fileExists(atPath: p) { loadOemIcon(from: URL(fileURLWithPath: p)) }
    }

    /// Clamp numeric fields to the allowed ranges (called on load, on every save, and after
    /// `apply`). Assign only when the value changes — every write to an @Published prop still
    /// notifies SwiftUI, and the reentrancy guard already stops the didSet→save recursion.
    ///
    /// NEVER SILENT (2026-09-07): every value this moves is named in the returned notes and — unless
    /// `dryRun` — in `clampNotes`. `dryRun` computes what a commit WOULD change without writing, so
    /// the control-socket door can tell a caller what Save will do to the value it just stored.
    /// Panels go through `PanelRule` (orientation-agnostic envelope, floor by the panel's own
    /// aspect); the view-area fields are clamped to the CLAMPED panel, so the order here matters.
    ///
    /// INTERNAL, not `private`: `VehicleConfigModel+Profile.swift`'s `apply(_:)` must re-clamp after
    /// writing fields from an imported document or a preset (DESIGN.md §8), and Swift `private` is
    /// file-scoped. Duplicating the clamp in that extension would let the two copies drift — the
    /// imported-document path would then accept geometry the UI path rejects. `clampInsets` below
    /// stays private; only this outer entry point is shared.
    @discardableResult
    func clampInPlace(dryRun: Bool = false) -> [String] {
        var notes: [String] = []
        // Work on locals and commit at the end, so a dry run touches nothing.
        var mw = mainWidth, mh = mainHeight, aw = altWidth, ah = altHeight
        var fps = maxFPS, afps = altFPS
        var ml = mainSafeLeft, mt = mainSafeTop, mr = mainSafeRight, mb = mainSafeBottom
        var al = altSafeLeft, at = altSafeTop, ar = altSafeRight, ab = altSafeBottom
        var vx = viewArea2X, vy = viewArea2Y, vw = viewArea2W, vh = viewArea2H
        var anim = viewAreaAnimMs

        func panel(_ label: String, _ w: inout Int, _ h: inout Int) {
            let c = PanelRule.clamped(width: w, height: h)
            if let n = PanelRule.clampNote(label, requested: (w, h), applied: c) { notes.append(n) }
            w = c.width; h = c.height
        }
        panel("Panel", &mw, &mh)
        panel("Alt display", &aw, &ah)
        // 24 fps left the box's vocabulary — coerce a stale persisted 24 to the nearest offered rate.
        if !Self.frameRates.contains(fps) {
            let to = fps == 24 ? 30 : 60
            notes.append("Frame rate \(fps) is not offered — stored as \(to) fps (offered: 30, 60).")
            fps = to
        }
        if !Self.frameRates.contains(afps) {
            notes.append("Alt display frame rate \(afps) is not offered — stored as 30 fps (offered: 30, 60).")
            afps = 30
        }
        // Safe-area insets: never negative, and opposite edges must leave a positive safe box (≥16px).
        // The box also re-validates and falls back to full-bleed, so this is a UX guard, not the gate.
        func insets(_ label: String, _ l: inout Int, _ t: inout Int, _ r: inout Int, _ b: inout Int,
                    width: Int, height: Int) {
            let before = (l, t, r, b)
            clampInsets(left: &l, top: &t, right: &r, bottom: &b, width: width, height: height)
            if before != (l, t, r, b) {
                notes.append("\(label) insets \(before.0)/\(before.1)/\(before.2)/\(before.3) → \(l)/\(t)/\(r)/\(b) "
                             + "(left/top/right/bottom): never negative, and a ≥16 px safe box must remain.")
            }
        }
        insets("Panel", &ml, &mt, &mr, &mb, width: mw, height: mh)
        insets("Alt display", &al, &at, &ar, &ab, width: aw, height: ah)
        // Second view area: per-field bounds only (never negative, never larger than the panel's own
        // axis). Containment (`x + w <= W`) is deliberately NOT clamped here — trimming either the
        // origin or the size would silently change what the owner typed; it is reported by
        // `viewArea2Verdict` and gates emission instead (`viewArea2YAML`).
        func field(_ what: String, _ v: inout Int, _ hi: Int) {
            let c = min(max(v, 0), hi)
            if c != v { notes.append("View area \(what) \(v) → \(c) (0–\(hi) on this panel)."); v = c }
        }
        field("X", &vx, mw); field("Y", &vy, mh); field("width", &vw, mw); field("height", &vh, mh)
        // The wire contract's range (1000…10000 ms); the emitter clamps again so a value that has not
        // been through Save still cannot leave the range. Floor raised from 0 to 1000 on 2026-09-09:
        // iOS itself has NO floor (10 ms was device-proven to render an instant flicker) — this is a
        // product choice that sub-second reads as abrupt, not a protocol limit.
        let animC = min(max(anim, 1000), 10000)
        if animC != anim { notes.append("Resize animation \(anim) → \(animC) ms (1000–10000)."); anim = animC }

        if dryRun { return notes }
        func commit(_ v: Int, _ prop: ReferenceWritableKeyPath<VehicleConfigModel, Int>) {
            if self[keyPath: prop] != v { self[keyPath: prop] = v }
        }
        commit(mw, \.mainWidth); commit(mh, \.mainHeight); commit(aw, \.altWidth); commit(ah, \.altHeight)
        commit(fps, \.maxFPS); commit(afps, \.altFPS)
        commit(ml, \.mainSafeLeft); commit(mt, \.mainSafeTop); commit(mr, \.mainSafeRight); commit(mb, \.mainSafeBottom)
        commit(al, \.altSafeLeft); commit(at, \.altSafeTop); commit(ar, \.altSafeRight); commit(ab, \.altSafeBottom)
        commit(vx, \.viewArea2X); commit(vy, \.viewArea2Y); commit(vw, \.viewArea2W); commit(vh, \.viewArea2H)
        commit(anim, \.viewAreaAnimMs)
        if clampNotes != notes { clampNotes = notes }
        return notes
    }

    /// The second view area's verdict for the CURRENT panel: `nil` = legal, else one message in
    /// `ViewArea2Rule.verdict`'s SEVERITY order (containment → odd value → non-positive dimension,
    /// all teardowns → the product floor, a lockout), naming the failing value and quoting the bound
    /// that applies to what was typed. This is the contract with the Settings view: the view renders
    /// this string and does not re-derive any rule. Evaluated whether or not the area is enabled, so
    /// the form can show the verdict while the owner is still typing.
    var viewArea2Verdict: String? {
        ViewArea2Rule.verdict(x: viewArea2X, y: viewArea2Y, w: viewArea2W, h: viewArea2H,
                              panelW: mainWidth, panelH: mainHeight)
    }

    /// The product floor that applies to the typed area's own orientation (800×480 landscape,
    /// 480×800 portrait — `ViewArea2Rule.minimumSize`), for the view to display next to the fields.
    var viewArea2MinimumSize: (width: Int, height: Int) {
        ViewArea2Rule.minimumSize(width: viewArea2W, height: viewArea2H)
    }

    /// Enabled AND legal — the ONLY condition under which `viewAreas[1]` is emitted and `viewAreas`
    /// is auto-armed in the SETUP author (`config.viewArea2Present`).
    var viewArea2Active: Bool { viewArea2Enabled && viewArea2Verdict == nil }

    private func clampInsets(left: inout Int, top: inout Int, right: inout Int, bottom: inout Int,
                             width: Int, height: Int) {
        func nonNeg(_ v: inout Int) { if v < 0 { v = 0 } }
        nonNeg(&left); nonNeg(&top); nonNeg(&right); nonNeg(&bottom)
        // Keep at least 16px of safe box on each axis. Trim the trailing edge first, then — if the
        // excess is bigger than that edge — trim the leading edge too, so the ≥16px guarantee holds
        // even when the leading inset alone exceeds the axis (previously only right/bottom were cut,
        // so a large left/top inset could drive the safe box negative).
        if left + right > width - 16 {
            var over = left + right - (width - 16)
            let cutR = min(right, over); right -= cutR; over -= cutR
            if over > 0 { left = max(0, left - over) }
        }
        if top + bottom > height - 16 {
            var over = top + bottom - (height - 16)
            let cutB = min(bottom, over); bottom -= cutB; over -= cutB
            if over > 0 { top = max(0, top - over) }
        }
    }

    /// `videoStreamsConfig.mainVideoStream.hidConfig` boolean keys (Apple's template names).
    private func hidFields() -> [(String, Bool)] {
        [("dPadSupport", dPadSupport), ("knobSupport", knobSupport),
         ("knobSupportsHomeAndBackButton", knobSupportsHomeAndBackButton),
         ("knobSupportsNudge", knobSupportsNudge), ("mediaButtonsSupport", mediaButtonsSupport),
         ("telephonyButtonsSupport", telephonyButtonsSupport), ("touchpadSupport", touchpadSupport),
         ("touchpadButtonsSupport", touchpadButtonsSupport),
         ("touchScreenSupportsCancel", touchScreenSupportsCancel),
         ("touchScreenSupportsMultiTouch", touchScreenSupportsMultiTouch),
         ("touchScreenHighFidelity", touchScreenHighFidelity),
         ("steeringWheelSupport", steeringWheelSupport)]
    }

    /// `limitedUIConfig` entries as (yamlKey, defaultsKey, value). The YAML keys are EXACTLY the
    /// box's `LimitedUiConfig` serde names (`vehicle_config.rs` — softKeyboard, softPhoneKeypad,
    /// musicLists, nonMusicLists, japanMaps, longAlerts); the box maps `longAlerts` to the wire
    /// string `longUserAlert` itself. The defaults keys are the app-side `vc.*` names.
    private func limitedUIFields() -> [(String, String, Bool)] {
        [("softKeyboard", "limitedUISoftKeyboard", limitedUISoftKeyboard),
         ("softPhoneKeypad", "limitedUISoftPhoneKeypad", limitedUISoftPhoneKeypad),
         ("musicLists", "limitedUIMusicLists", limitedUIMusicLists),
         ("nonMusicLists", "limitedUINonMusicLists", limitedUINonMusicLists),
         ("japanMaps", "limitedUIJapanMaps", limitedUIJapanMaps),
         ("longAlerts", "limitedUILongAlerts", limitedUILongAlerts),
         // Real CodingKeys the box parses for round-trip ONLY — Apple's airPlayElements never emits
         // them, so they never reach /info limitedUIElements (vehicle_config.rs:208-218, 234-238).
         ("pairedDevices", "limitedUIPairedDevices", limitedUIPairedDevices),
         ("themeCustomization", "limitedUIThemeCustomization", limitedUIThemeCustomization),
         ("automakerSettings", "limitedUIAutomakerSettings", limitedUIAutomakerSettings),
         ("automakerSettingsInfoButton", "limitedUIAutomakerSettingsInfoButton", limitedUIAutomakerSettingsInfoButton)]
    }

    /// `accessoryConfig` `enables*` keys (Apple AccessoryConfig — docs/carplay/03_SDK_GROUND_TRUTH.md §2).
    private func accessoryFields() -> [(String, Bool)] {
        [("enablesMainBufferedAudio", enablesMainBufferedAudio), ("enablesHEVC", enablesHEVC),
         ("enablesUIAppearance", enablesUIAppearance), ("enablesMapAppearance", enablesMapAppearance),
         ("enablesCornerMasks", enablesCornerMasks), ("enablesVideoPlayback", enablesVideoPlayback),
         ("enablesViewAreas", enablesViewAreas), ("enablesEnhancedSiri", enablesEnhancedSiri),
         ("enablesFocusTransfer", enablesFocusTransfer), ("enablesUIContext", enablesUIContext),
         ("enablesUISync", enablesUISync), ("enablesFileTransfer", enablesFileTransfer),
         ("enablesLogTransfer", enablesLogTransfer), ("enablesVehicleDataProtocol", enablesVehicleDataProtocol),
         ("enablesDCX", enablesDCX), ("appDrivenSetup", appDrivenSetup)]
    }

    /// The main stream's SECOND `viewAreas[]` entry (the Dock resize target), or "" — appended to
    /// `va()`'s output inside `yaml`. Emitted ONLY when `viewArea2Enabled` AND `ViewArea2Rule.verdict`
    /// is nil for the current panel, so a disabled or illegal rect leaves the document BYTE-IDENTICAL
    /// to a build without the feature (the fixture guard pins the OFF document). Shape: same 4-space
    /// list level as `va()`'s `- viewArea:`; a LEADING newline because `va()`'s literal has none, and
    /// NO trailing newline because the enclosing literal's next line supplies it (the glued-`initialURL`
    /// trap, see `the_apps_real_emitted_document_parses`). The nested `safeArea` is the area itself in
    /// PANEL coordinates (Apple's Widescreen template shape; the box emits it full-bleed regardless).
    /// The box reads `viewArea` only (`vehicle_config.rs::second_main_view_area`); `initial` is not
    /// authored here, so the session starts in the full panel and the first press collapses.
    private var viewArea2YAML: String {
        guard viewArea2Enabled,
              ViewArea2Rule.verdict(x: viewArea2X, y: viewArea2Y, w: viewArea2W, h: viewArea2H,
                                    panelW: mainWidth, panelH: mainHeight) == nil
        else { return "" }
        return "\n    - viewArea:\n        originX: \(viewArea2X)\n        originY: \(viewArea2Y)\n"
            + "        width: \(viewArea2W)\n        height: \(viewArea2H)\n"
            + "      safeArea:\n        originX: \(viewArea2X)\n        originY: \(viewArea2Y)\n"
            + "        width: \(viewArea2W)\n        height: \(viewArea2H)\n"
            + "      drawUIOutsideSafeArea: \(mainDrawOutsideSafe)"
    }

    /// The structured, SETUP-relevant slice of this config (plan P3) — the single source the app-driven
    /// SETUP author reads. Materialized from the SAME `@Published` fields the `yaml` above interpolates,
    /// so the YAML pushed to the box and the host-authored SETUP answers are built from one set of
    /// booleans and can never diverge. `altVideoStreamsPresent` mirrors the YAML's `altVideoStreams[]`
    /// presence (`altVideoEnabled`), which is what arms the box's `altScreen` feature.
    var config: VehicleConfig {
        // 03-M2 fix: mirror the box's `view_areas_enabled()` inset auto-arm — a real main OR (when
        // the alt stream is enabled) alt safe-area inset arms `viewAreas` even with the toggle off,
        // exactly as the box already does locally. See `VehicleConfig.hasRealSafeAreaInset`.
        let mainInset = VehicleConfig.hasRealSafeAreaInset(
            left: mainSafeLeft, top: mainSafeTop, right: mainSafeRight, bottom: mainSafeBottom,
            width: mainWidth, height: mainHeight)
        let altInset = altVideoEnabled && VehicleConfig.hasRealSafeAreaInset(
            left: altSafeLeft, top: altSafeTop, right: altSafeRight, bottom: altSafeBottom,
            width: altWidth, height: altHeight)
        return VehicleConfig(
            enablesHEVC: enablesHEVC,
            enablesViewAreas: enablesViewAreas,
            enablesCornerMasks: enablesCornerMasks,
            enablesLogTransfer: enablesLogTransfer,
            enablesMainBufferedAudio: enablesMainBufferedAudio,
            altVideoStreamsPresent: altVideoEnabled,
            safeAreaInsetPresent: mainInset || altInset,
            viewArea2Present: viewArea2Active,
            appDrivenSetup: appDrivenSetup,
            mainWidth: mainWidth, mainHeight: mainHeight, maxFPS: maxFPS,
            altWidth: altWidth, altHeight: altHeight, altFPS: altFPS
        )
    }

    /// Render the VehicleConfig YAML pushed at SUBSCRIBE — the shape mirrors Apple's CarPlaySimulator
    /// `VehicleConfigs/Configs/*.yaml` templates (Widescreen etc.): `displayPanelsConfig` /
    /// `videoStreamsConfig` (with `viewAreas`, `hidConfig`, `primaryInput`) / `accessoryConfig`.
    var yaml: String {
        // viewArea = the full coded frame (video always fills the rectangle). safeArea = the inset box
        // (converted from per-edge insets) where CarPlay keeps its UI. l/t/r/b are px from each edge;
        // 0,0,0,0 = full-bleed. drawOutside permits UI in the viewArea↔safeArea gap.
        func va(_ w: Int, _ h: Int, _ l: Int, _ t: Int, _ r: Int, _ b: Int, _ drawOutside: Bool) -> String {
            let sx = max(0, l), sy = max(0, t)
            let sw = max(1, w - sx - max(0, r)), sh = max(1, h - sy - max(0, b))
            return """
                viewAreas:
                - viewArea:
                    originX: 0
                    originY: 0
                    width: \(w)
                    height: \(h)
                  safeArea:
                    originX: \(sx)
                    originY: \(sy)
                    width: \(sw)
                    height: \(sh)
                  drawUIOutsideSafeArea: \(drawOutside)
            """
        }
        var y = """
        name: "\(YamlEmit.quotedBody(name))"
        wireless: \(wirelessEnabled)
        hot_handover: \(hotHandover)
        rightHandDrive: \(driverPosition == DriverPosition.right.rawValue)
        pairing: \(pairingNumericComparison ? (pairingInteractiveAnswer ? "numeric_comparison_interactive" : "numeric_comparison") : "just_works")
        android_auto: \(androidAutoEnabled)
        displayPanelsConfig:
          mainDisplayPanel:
            displayPanelID: DisplayPanel.Main
            pixelDimensions:
              width: \(mainWidth)
              height: \(mainHeight)
          altDisplayPanels:\(altDisplayPanelsYAML)
        videoStreamsConfig:
          mainVideoStream:
            videoStreamID: VideoStream.Main
            pixelDimensions:
              width: \(mainWidth)
              height: \(mainHeight)
            maxFPS: \(maxFPS)
        \(va(mainWidth, mainHeight, mainSafeLeft, mainSafeTop, mainSafeRight, mainSafeBottom, mainDrawOutsideSafe))\(viewArea2YAML)
            hidConfig:
              dPadSupport: \(dPadSupport)
              knobSupport: \(knobSupport)
              knobSupportsHomeAndBackButton: \(knobSupportsHomeAndBackButton)
              knobSupportsNudge: \(knobSupportsNudge)
              mediaButtonsSupport: \(mediaButtonsSupport)
              telephonyButtonsSupport: \(telephonyButtonsSupport)
              touchpadSupport: \(touchpadSupport)
              touchpadButtonsSupport: \(touchpadButtonsSupport)
              touchScreenMode: \(touchScreenHighFidelity ? "High Fidelty" : "Disabled")
              touchScreenSupportsCancel: \(touchScreenSupportsCancel)
              touchScreenSupportsMultiTouch: \(touchScreenSupportsMultiTouch)
              steeringWheelSupport: \(steeringWheelSupport)
            primaryInput: \(primaryInput)
        """
        if altVideoEnabled {
            y += "\n  altVideoStreams:\n  - videoStreamID: VideoStream.Alt1\n"
            y += "    pixelDimensions:\n      width: \(altWidth)\n      height: \(altHeight)\n"
            y += "    maxFPS: \(altFPS)\n"
            // `va()` already emits `viewAreas:` at 4-space indent — the SAME level as the alt stream's
            // own `pixelDimensions`/`maxFPS` (children of the `- videoStreamID` list item). Do NOT add
            // an extra indent here: over-indenting folded `viewAreas` into the `maxFPS` scalar, which
            // made serde reject the whole alt block ("invalid type: string \"30 viewAreas\"").
            y += va(altWidth, altHeight, altSafeLeft, altSafeTop, altSafeRight, altSafeBottom, altDrawOutsideSafe)
            // `initialURL` belongs to the VIDEO STREAM, not the panel — verified against Apple's own
            // `Standard Navigation.yaml`, where it sits under `altVideoStreams[]`. docs/carplay/03_SDK_GROUND_TRUTH.md §5 calls a
            // per-stream `initialURL` one of the three things that "exist nowhere else on the wire".
            // LEADING newline is REQUIRED: `va()`'s multiline literal has NO trailing newline, so
            // without it this glues onto `drawUIOutsideSafeArea: false` and the WHOLE document fails
            // to parse — taking resolution, HEVC, appDrivenSetup, audio and the metadata tier down
            // with it. Caught in review after being introduced five lines below the comment warning
            // about this exact failure class.
            y += "\n    initialURL: \(Self.clusterInitialURL)\n"
        } else {
            y += "\n  altVideoStreams: []"
        }
        y += "\naccessoryConfig:\n"
        for (k, v) in accessoryFields() { y += "  \(k): \(v)\n" }
        // `limitedUIConfig` is a TOP-LEVEL sibling of accessoryConfig (Apple's schema; the box's
        // serde struct). Disabled ⇒ emit nothing, so the box's /info stays byte-identical to the
        // pre-feature behavior and iOS keeps its own default restriction set.
        if limitedUIConfigEnabled {
            y += "limitedUIConfig:\n"
            for (yk, _, v) in limitedUIFields() { y += "  \(yk): \(v)\n" }
        }
        // oemIconConfig — the vehicle-maker logo (Apple's schema; box `OemIconConfig`). Disabled or no
        // image ⇒ emit nothing, so /info stays byte-identical. The base64 is quoted (its `+//=` alphabet
        // is YAML-safe inside quotes).
        if oemIconEnabled && !oemIconBase64.isEmpty {
            let variants = oemIconVariants()   // 120/180/256, each a small PNG (Apple's AppStub sizes)
            if !variants.isEmpty {
                y += "oemIconConfig:\n"
                y += "  images:\n"
                for (sz, b64) in variants {
                    y += "    - width: \(sz)\n"
                    y += "      height: \(sz)\n"
                    y += "      imageBase64: \"\(b64)\"\n"
                }
                // Free text → `YamlEmit.quotedBody` (same helper `name:` uses): an unescaped `"`
                // or `\` in a label would malform the WHOLE pushed document, making the box fall
                // back to its built-in defaults for resolution/HEVC/appDrivenSetup/audio. Same
                // class as the metadata skip-field bug (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B3).
                y += "  label: \"\(YamlEmit.quotedBody(oemIconLabel))\"\n"
                // oemIconVisible parameter. Sending visible:false (with the icons STILL present) is the
                // active "hide" signal iOS honors — merely omitting oemIconConfig leaves the last/cached
                // icon on screen. So "Show OEM icon" advertises the config; "Show icon in CarPlay" toggles
                // this visible flag to show/hide it on the next connect.
                y += "  visible: \(oemIconVisible)\n"
            }
        }
        // `wifi_ap` — our top-level extension read by the box's session_supervisor.sh
        // (`wifi_ap_enabled()` greps for an explicit `wifi_ap: false`; absent = enabled). Enabled ⇒
        // emit nothing, so every pre-existing configuration pushes a byte-identical document — the
        // drift-guard fixture (tools/regen_app_yaml_fixture.py) runs with the default and pins that.
        // Only a deliberate OFF (the BT-only bridge role) reaches the wire.
        if !wifiAccessPoint { y += "wifi_ap: false\n" }
        // `view_area_anim_ms` — our top-level extension, the box's `animationDurationMillis` on its
        // `updateViewArea` answer. Absent = 3000 on the box, so 3000 emits NOTHING (same idiom, same
        // byte-identical fixture); the stub in tools/regen_app_yaml_fixture.py carries 3000.
        // Clamped to the contract's 1000…10000 here as well as in `clampInPlace`.
        // Range is a PRODUCT decision, not a protocol limit: iOS honours 10 ms (instant flicker)
        // and 10000 ms (full envelope) alike — device-proven 2026-09-09. Floor set to 1000 by the
        // owner because sub-second reads as abrupt in a car. Default 3000 still emits nothing.
        let animMs = min(max(viewAreaAnimMs, 1000), 10000)
        if animMs != 3000 { y += "view_area_anim_ms: \(animMs)\n" }
        y += audioYAML
        y += metadataYAML
        y += iapConfigYAML
        return y
    }

    /// Apple's standard cluster content URL, from `Standard Navigation.yaml` / `Standard Instrument
    /// Cluster.yaml`. Not owner-configurable yet — the adapter separately advertises the full
    /// three-URL set (map + instructioncard + base) that the genuine CCPA box does, so this is the
    /// stream's STARTING content, not the limit of what the cluster can show.
    static let clusterInitialURL = "maps:/car/instrumentcluster/map"

    /// `altDisplayPanels[]` — the cluster panel, in Apple's own shape.
    ///
    /// Emitted ONLY when the alt/cluster stream is enabled; otherwise an empty array, which is what
    /// this app has always sent. docs/carplay/03_SDK_GROUND_TRUTH.md §5 identifies the missing `/info` `displayPanels[]` array as
    /// the alt-content ROOT CAUSE: the modern panel dict is the only place `properties`
    /// (`displayProperties`), a nested `videoStreams[]` and a per-stream `initialURL` exist on the
    /// wire at all, and our legacy flat `displays[]` is "structurally incapable of defining anything
    /// inside" the cluster stream.
    ///
    /// Panel dimensions deliberately track the alt STREAM's, matching Apple's templates where both
    /// are 640x480. Apple's schema allows them to differ; if a real case ever needs that, it becomes
    /// its own pair of UI fields rather than a silent divergence.
    ///
    /// `DisplayPanelProperty` has EXACTLY three cases (docs/carplay/03_SDK_GROUND_TRUTH.md §5) — `dpManaged`,
    /// `additionalContent`, `showsInstruments` — and only the last appears in any stock Apple
    /// template, so that is the only one emitted.
    ///
    /// ⚠️ The adapter PARSES this but does not yet emit `/info` `displayPanels[]`; that step is a
    /// gated hardware experiment whose payoff docs/carplay/03_SDK_GROUND_TRUTH.md §5 records as INFERRED, not observed.
    /// INDENTATION IS LOAD-BEARING and is built with EXPLICIT spaces, not a multiline literal.
    ///
    /// Two traps, both hit while writing this. (1) Swift dedents a multiline literal relative to its
    /// CLOSING DELIMITER, which silently produced 4-space continuation lines — the same class of bug
    /// that once folded `viewAreas` into the `maxFPS` scalar and made serde reject the whole alt
    /// block. (2) This property is a SEPARATE expression from the enclosing `var yaml` literal, so it
    /// is NOT dedented by it — source columns here are EMITTED columns. An earlier revision reasoned
    /// about source columns and produced a valid-but-over-indented sequence at 10/12.
    ///
    /// EMITTED SHAPE, which is Apple's own (`Standard Navigation.yaml`): the key `altDisplayPanels:`
    /// lands at 2 (inside `displayPanelsConfig:`), the sequence entry at 2 — valid YAML, a block
    /// sequence may share its key's indent, and it is what Apple writes — and the entry's mapping
    /// keys at 4.
    private var altDisplayPanelsYAML: String {
        guard altVideoEnabled else { return " []" }
        return "\n"
            + "  - displayPanelID: DisplayPanel.Alt1\n"
            + "    pixelDimensions:\n"
            + "      width: \(altWidth)\n"
            + "      height: \(altHeight)\n"
            + "    displayProperties:\n"
            + "    - showsInstruments"
    }

    /// The `accessoryName:` + `iapConfig:` sections — the vehicle identity behind Identify params
    /// 20/21 (docs/carplay/04_CAPABILITIES_AND_CONFIG.md C6/C7).
    ///
    /// ABSENT-OFF, and that is the whole safety story: with nothing configured this emits an empty
    /// string, the box resolves its compiled baseline (EngineType=Gasoline, no param 21), and the
    /// Identify is byte-identical to what shipped before the feature. It is also the rollback path —
    /// clearing these fields restores baseline bytes with no rebuild and no box-side change.
    ///
    /// C-3 landed (2026-09-02): iap2d builds `engineTypes`/`chargingConnectors` into the wired
    /// Identify on the next phone plug. `accessoryName` remains parse-only (no production caller of
    /// `accessory_name_bounded`).
    private var iapConfigYAML: String {
        var s = ""
        // Free text → `YamlEmit.quotedBody`, the same helper `name:` and `oemIconLabel` use. An
        // unescaped `"` or `\` here would malform the WHOLE pushed document, and the box would then
        // fall back to built-in defaults for resolution/HEVC/appDrivenSetup/audio/metadata — a typo
        // in the car's name silently reverting every other setting (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B3's bug class).
        let name = accessoryName.trimmingCharacters(in: .whitespacesAndNewlines)
        if !name.isEmpty {
            s += "accessoryName: \"\(YamlEmit.quotedBody(name))\"\n"
        }

        // Emit `iapConfig:` only when something under it is actually set — an empty block is noise
        // that would still (harmlessly) resolve to baseline, but absent is cleaner and keeps the
        // pushed document byte-identical to pre-feature for an unconfigured install.
        let engines = Self.engineTypeNames.filter { engineTypes.contains($0) } // canonical order
        // Only meaningful for an electric/plug-in vehicle, and the editor is shown on the same
        // condition — config the owner cannot see must not keep riding the wire.
        let conns = engineTypes.contains("electric") ? dedupedConnectors() : []
        let caps = effectiveVehicleStatusCaps()
        // `vehicleStatus:` is emittable only once the adapter can service it — see
        // `vehicleStatusUnlocked`. Persisted intent must not leak onto the wire when C-4 lands.
        let emitStatus = vehicleStatusEnabled && Self.vehicleStatusUnlocked
        guard !engines.isEmpty || !conns.isEmpty || emitStatus else { return s }

        s += "iapConfig:\n"
        if !engines.isEmpty || !conns.isEmpty {
            s += "  vehicleInfo:\n"
            if !engines.isEmpty {
                // A hybrid is genuinely two entries — Apple's sub 2 is `[0+]`.
                s += "    engineTypes: [\(engines.joined(separator: ", "))]\n"
            }
            if !conns.isEmpty {
                s += "    chargingConnectors:\n"
                for c in conns {
                    s += "      - type: \(c.type)\n"
                    // Omit the key entirely when unset: an absent rating must not become a zero one.
                    if let w = c.powerWatts, w > 0 { s += "        powerWatts: \(w)\n" }
                }
            }
        }
        if emitStatus {
            s += "  vehicleStatus:\n"
            s += "    capabilities: [\(caps.joined(separator: ", "))]\n"
        }
        return s
    }

    /// Connector rows with duplicate TYPES removed, first row winning — mirroring what the box does.
    ///
    /// Apple's per-connector power subs are single-valued, so two rows of one type cannot be
    /// represented on the wire at all. The box drops the extras defensively; the app drops them here
    /// too, so the owner is not shown connectors that will be silently dropped. (The adapter also
    /// SORTS connectors by type onto the wire, so the preview's order is not the wire's order —
    /// the set matches, the sequence is the adapter's.)
    private func dedupedConnectors() -> [ConnectorRow] {
        var seen = Set<String>()
        return chargingConnectors.filter { row in
            let t = row.type.trimmingCharacters(in: .whitespaces).lowercased()
            guard Self.connectorNames.contains(t), !seen.contains(t) else { return false }
            seen.insert(t)
            return true
        }
    }

    /// The status capabilities actually emitted, with the forbidden combination resolved the same
    /// way the adapter resolves it.
    ///
    /// The order here is the UI GROUPING order, not Apple's sub-parameter order — the adapter
    /// sorts them onto the wire, because framing is the adapter's to own and only the values are
    /// the owner's to choose.
    ///
    /// Apple's note is imperative: the unified `rangeWarning` and the per-engine `rangeWarning*` are
    /// mutually exclusive ("Do not include if vehicle reports unified range warning for all
    /// EngineTypes"). If both are somehow selected, the unified one wins and the per-engine flags are
    /// dropped — identical to the box's own resolution, so the app's preview never disagrees with
    /// what is sent.
    private func effectiveVehicleStatusCaps() -> [String] {
        var chosen = vehicleStatusCaps
        if chosen.contains("rangeWarning") {
            chosen.subtract(Self.perEngineRangeWarnings)
        }
        return Self.vehicleStatusCapNames.filter { chosen.contains($0) }
    }

    /// The `metadata:` section — the iAP2 declaration tier + skip list the box arms once per link
    /// (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B3). Always emitted so the app is unambiguously the source of the tier; `proven`
    /// reproduces the box's compiled floor exactly.
    private var metadataYAML: String {
        var s = "metadata:\n  tier: \(metadataTier)\n"
        // Feature-table names are snake_case identifiers. FILTER to that charset rather than
        // interpolating free text: an unbalanced `]`, a `:`, a `#` or a quote would make the whole
        // pushed DOCUMENT malformed, and the box's receiver would then fall back to its built-in
        // defaults for resolution/HEVC/appDrivenSetup too — a typo in this field silently reverting
        // every other setting. Anything unrecognized is dropped here, which is what the tooltip says.
        let names = metadataSkip
            .split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
            .filter { $0.allSatisfy { $0.isLetter || $0.isNumber || $0 == "_" || $0 == "-" } }
        if !names.isEmpty {
            s += "  skip: [\(names.joined(separator: ", "))]\n"
        }
        return s
    }

    /// The `audio:` section — the declarative CarPlay audio capability set (box reads it to build the
    /// advertised `audioFormats`). "auto" pushes BOTH per-transport arms explicitly (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B5:
    /// the app always pushes the advertised set; the box presents the arm matching the session's
    /// transport, and its transport-gated default remains only as the interim safety floor for
    /// no-config/parse-failure paths). The pushed
    /// pair is byte-equivalent to that default, so "auto" sessions are unchanged on the wire.
    private var audioYAML: String {
        switch audioMode {
        case "wired_pcm", "wireless_8":
            return "audio:\n  preset: \(audioMode)\n"
        case "custom":
            let rows = audioFormats.filter { $0.output != "none" } // an entry must offer at least an output
            guard !rows.isEmpty else { return "" }
            var s = "audio:\n  formats:\n"
            for f in rows {
                var parts = ["type: \(f.streamType)"]
                if !f.audioType.isEmpty { parts.append("audioType: \(f.audioType)") }
                if f.input != "none" { parts.append("in: \(f.input)") }
                parts.append("out: \(f.output)")
                s += "  - {\(parts.joined(separator: ", "))}\n"
            }
            return s
        default:
            // "auto" — match transport: explicit per-arm push, equivalent to the box's floor.
            return "audio:\n  wired:\n    preset: wired_pcm\n  wireless:\n    preset: wireless_8\n"
        }
    }

    /// What the app pushes to the box — the last SAVED snapshot, not unsaved form edits.
    func data() -> Data { Data(committedYAML.utf8) }

    /// Revert every field to the shipped default (mirrors Apple's Widescreen template intent).
    func resetToDefault() {
        loading = true
        name = "CarLink Widescreen"
        wirelessEnabled = true
        hotHandover = false
        pairingNumericComparison = false
        pairingInteractiveAnswer = false
        androidAutoEnabled = true
        wifiAccessPoint = true
        mainWidth = 1920; mainHeight = 1080; maxFPS = 60
        mainSafeLeft = 0; mainSafeTop = 0; mainSafeRight = 0; mainSafeBottom = 0; mainDrawOutsideSafe = false
        viewArea2Enabled = false; viewArea2X = 0; viewArea2Y = 0; viewArea2W = 0; viewArea2H = 0
        viewAreaAnimMs = 3000
        altVideoEnabled = false; altWidth = 800; altHeight = 480; altFPS = 30
        altSafeLeft = 0; altSafeTop = 0; altSafeRight = 0; altSafeBottom = 0; altDrawOutsideSafe = false
        enablesHEVC = true; enablesMainBufferedAudio = false
        audioMode = "auto"; audioFormats = Self.defaultCustomFormats
        metadataTier = "proven"; metadataSkip = ""
        // Vehicle identity — all six back to "absent". Restore-defaults is what an owner
        // reaches for AFTER a rejected identification, so leaving any of these armed would
        // defeat the one recovery gesture they have.
        accessoryName = ""; engineTypes = []; chargingConnectors = []
        vehicleStatusEnabled = false; vehicleStatusCaps = []; steeringWheelSupport = false
        dPadSupport = true; knobSupport = false; knobSupportsHomeAndBackButton = false
        knobSupportsNudge = false; mediaButtonsSupport = true; telephonyButtonsSupport = false
        touchpadSupport = false; touchpadButtonsSupport = false
        touchScreenHighFidelity = true; touchScreenSupportsCancel = true; touchScreenSupportsMultiTouch = false; primaryInput = "Touchpad"
        nightMode = false; rightHandDrive = false
        driverPosition = "left"; theme = "light"; dpi = 160; diagonalInches = 0
        hideClock = false; hideSignal = false; hideBattery = false
        restrictVideo = false; restrictVoiceInput = false; restrictConfiguration = false
        voiceRateHz = 48000; telephonyOverProjection = false
        metadataNowPlaying = true; metadataNavigation = true; metadataTelephony = true
        aaFitPanelWithMargins = true; aaPreferHEVC = false
        enablesUIAppearance = true; enablesMapAppearance = true; enablesCornerMasks = false
        enablesVideoPlayback = true; enablesViewAreas = false; enablesEnhancedSiri = false
        enablesFocusTransfer = false; enablesUIContext = false; enablesUISync = false
        enablesFileTransfer = false; enablesLogTransfer = false
        enablesVehicleDataProtocol = false; enablesDCX = false
        appDrivenSetup = true // default ON (see load default)
        limitedUIConfigEnabled = false
        limitedUISoftKeyboard = false; limitedUISoftPhoneKeypad = false
        limitedUIMusicLists = false; limitedUINonMusicLists = false
        limitedUIJapanMaps = false; limitedUILongAlerts = false
        limitedUIPairedDevices = false; limitedUIThemeCustomization = false
        limitedUIAutomakerSettings = false; limitedUIAutomakerSettingsInfoButton = false
        oemIconEnabled = false; oemIconVisible = true; oemIconLabel = "CarLink"
        oemIconBase64 = ""; oemIconW = 0; oemIconH = 0
        loading = false
        save()
    }
}

// Tabs are named for WHO OWNS the setting, not for a protocol (DESIGN.md §1): Vehicle is the
// neutral profile with each row's per-protocol rendering shown inline, Adapter is the box and its
// radios, Diagnostics is neither. Protocol-exclusive settings live as badged sub-groups inside the
// feature they belong to — there is deliberately no "CarPlay" or "Android Auto" tab.
//
// DESIGN.md §1 / §11.6 risk 6: there is NO SwiftUI → AppKit toolbar bridge for a manually
// constructed NSWindow (confirmed empty by SDK audit against SwiftUI.swiftinterface /
// SwiftUICore.swiftinterface, 2026-09-08). `TabView` + `.tabItem` is gone; the pane switcher is a
// real `NSToolbar` built in `SettingsWindowController`, and `SettingsRootView` is a plain switch
// driven by the pane the toolbar (or the restored default) selected.

/// The three `Feature.Tab` cases, in the order they appear in the toolbar. Kept as its own type
/// (rather than reusing `FeatureMatrix.Feature.Tab`) because this is view/window state — the
/// toolbar's subitem order, the window title text and the restore-last-pane key — not a case over
/// `Feature` placement.
enum SettingsPane: Int, CaseIterable, Equatable {
    case vehicle, adapter, diagnostics

    var title: String {
        switch self {
        case .vehicle: return "Vehicle"
        case .adapter: return "Adapter"
        case .diagnostics: return "Diagnostics"
        }
    }

    var symbolName: String {
        switch self {
        case .vehicle: return "car"
        case .adapter: return "cpu"
        case .diagnostics: return "text.alignleft"
        }
    }

    /// UserDefaults key for "restore the most recently viewed pane" (§1). Deliberately not a
    /// `vc.*` key: this is window UI state, not vehicle config, and must not round-trip through
    /// Import/Export or a preset.
    static let lastViewedDefaultsKey = "settingsWindow.lastViewedPane"

    /// **Deviation from DESIGN.md §11.6 risk 5, recorded here because it cannot be recorded in
    /// DESIGN.md from this file's write scope.** All three panes are `.formStyle(.grouped)`
    /// (`VehicleTab.swift:1403`, `AdapterTab.swift:330`, `DiagnosticsTab.swift:63`) — on macOS a
    /// grouped `Form` is a scrolling, `NSTableView`-backed container whose IDEAL size is NOT
    /// derived from its rows, so `NSHostingController.sizingOptions = [.intrinsicContentSize]`
    /// reports a near-zero fitting size regardless of content (measured: 460×84 with only width
    /// pinned). §11.6 risk 5's premise — that `sizingOptions` alone drives per-pane AND
    /// per-section-expansion resizing — does not hold for a grouped `Form`, and fixing it at the
    /// `Form` level is out of this worker's scope (`VehicleTab.swift`/`AdapterTab.swift` belong to
    /// other workers). This is a hardcoded floor per pane, chosen against DESIGN.md §11.4's
    /// resting-row counts; it gives each pane its own height (the literal §1 requirement) but does
    /// NOT grow the window when a `Section(isExpanded:)` opens — the Form scrolls internally
    /// instead. That gap is a real design regression against risk 5's stated acceptance test and
    /// needs the DESIGN.md owner to fold it in, not silent acceptance.
    var restingHeight: CGFloat {
        switch self {
        case .vehicle:     return 620
        case .adapter:     return 520
        case .diagnostics: return 280
        }
    }
}

/// Bridges the AppKit toolbar's selection to the hosted SwiftUI content. Owned by
/// `SettingsWindowController`; `SettingsRootView` only observes it, so a toolbar click and a
/// programmatic pane change go through the same path. Plain `ObservableObject`/`@Published` per
/// DESIGN.md §11.7 — `@Bindable` does not apply to a non-`Observable` type, and there is nothing
/// here that warrants promoting this to `@Observable` either. `@MainActor` stated explicitly
/// (rather than left to inference) since both `SettingsWindowController` and every SwiftUI
/// observer of this object are main-actor-isolated.
@MainActor
final class SettingsPaneSelection: ObservableObject {
    @Published var pane: SettingsPane {
        didSet {
            guard pane != oldValue else { return }
            UserDefaults.standard.set(pane.rawValue, forKey: SettingsPane.lastViewedDefaultsKey)
            onChange?(pane)
        }
    }

    /// Set by the window controller so it can keep the window title and the toolbar's own
    /// `selectedIndex` in step, regardless of whether the change came from a toolbar click or a
    /// restored default.
    var onChange: ((SettingsPane) -> Void)?

    init() {
        let saved = UserDefaults.standard.integer(forKey: SettingsPane.lastViewedDefaultsKey)
        pane = SettingsPane(rawValue: saved) ?? .vehicle
    }
}

struct SettingsRootView: View {
    @ObservedObject var selection: SettingsPaneSelection

    var body: some View {
        // A `Form` has no intrinsic WIDTH — it fills whatever width it is offered — so
        // `sizingOptions = [.intrinsicContentSize]` alone reports a fitting width of 0. Pin the
        // width all three panes share (460, the width the window used before Phase 3).
        //
        // HEIGHT is also pinned, per pane, and that is a deviation from the original plan — see
        // `SettingsPane.restingHeight`. A grouped `Form` (all three panes) is List-backed and its
        // ideal height is not derived from its rows, so leaving height unconstrained measured
        // 460×84 regardless of content. Each pane still gets its OWN height (§1's literal
        // requirement); what is lost is the window growing/shrinking live when a
        // `Section(isExpanded:)` toggles — the Form scrolls internally instead.
        //
        // A `switch` here (rather than this `ZStack`) tears down the two unselected panes' view
        // trees on every pane change, resetting every tab-local `@State` — `ResolutionField`'s
        // `forceCustom` snap-back guard, all nine `CollapsibleFeatureSection.expanded` flags, an
        // in-flight confirmation dialog, etc. DESIGN.md §11.1 decision 2 is "plain `@State`, reset
        // each launch" — reset on every SWITCH is a stricter, unintended behaviour the old
        // `TabView` never had (`NSTabView` keeps every tab's content view alive once created). A
        // `ZStack` with all three panes permanently in the hierarchy, hidden by opacity rather than
        // removed, restores that: SwiftUI keeps each pane's identity and `@State` across switches
        // because none of them ever leaves the view tree.
        //
        // This does NOT reopen the per-pane-sizing problem above: sizing here is driven by the
        // explicit `.frame(width:height:)` below, keyed off `selection.pane`, not by the
        // `ZStack`'s own fitting size — so there is no risk of the window settling to the MAX of
        // the three panes' sizes the way there would be if sizing were left to sizingOptions/
        // intrinsic content size on this container.
        // `.disabled(_:)` on each hidden pane matters beyond hit-testing: both `VehicleTab` and
        // `AdapterTab` bind `⌘S` to their own `Button("Save") { model.save() }`
        // (`VehicleTab.swift:1512`, `AdapterTab.swift:553`). With all three panes permanently
        // mounted (above), BOTH buttons are simultaneously live in the hierarchy whenever
        // `model.dirty` — measured: with two identical, enabled `⌘S` shortcuts registered at
        // once, SwiftUI fires NEITHER (verified with an `lldb` breakpoint on
        // `VehicleConfigModel.save()`: zero hits after ⌘S on a dirty model, where the same
        // breakpoint reliably caught the button's own click). A disabled button's
        // `keyboardShortcut` does not respond, so disabling every pane but the current one leaves
        // exactly one live `⌘S` registration — the visible pane's — without disturbing `@State`
        // (`.disabled` only sets an environment value; it does not affect view identity).
        ZStack {
            VehicleTab()
                .opacity(selection.pane == .vehicle ? 1 : 0)
                .allowsHitTesting(selection.pane == .vehicle)
                .accessibilityHidden(selection.pane != .vehicle)
                .disabled(selection.pane != .vehicle)
            AdapterTab()
                .opacity(selection.pane == .adapter ? 1 : 0)
                .allowsHitTesting(selection.pane == .adapter)
                .accessibilityHidden(selection.pane != .adapter)
                .disabled(selection.pane != .adapter)
            DiagnosticsTab()
                .opacity(selection.pane == .diagnostics ? 1 : 0)
                .allowsHitTesting(selection.pane == .diagnostics)
                .accessibilityHidden(selection.pane != .diagnostics)
                .disabled(selection.pane != .diagnostics)
        }
        .frame(width: 460, height: selection.pane.restingHeight)
    }
}

// MARK: - AppKit host

/// Owns the Settings `NSWindow`: a real `NSToolbar` pane switcher (§11.6 risk 6) and per-pane
/// sizing via `NSHostingController.sizingOptions` (§11.6 risk 5) on a window whose `styleMask`
/// never gains `.resizable` (§11.8) — the two mechanisms are orthogonal, per DESIGN.md.
final class SettingsWindowController: NSWindowController, NSToolbarDelegate {
    static let shared = SettingsWindowController()

    private static let toolbarIdentifier = NSToolbar.Identifier("SettingsWindow.toolbar")
    private static let paneSwitcherIdentifier = NSToolbarItem.Identifier("SettingsWindow.paneSwitcher")

    // Swift forbids touching `self` (even a defaulted stored property) in a convenience init
    // before delegating to `self.init(window:)`, so these are implicitly-unwrapped and populated
    // immediately afterward, before `init` returns or anything else can observe them.
    private var selection: SettingsPaneSelection!
    private var hostingController: NSHostingController<SettingsRootView>!
    private var paneGroup: NSToolbarItemGroup?

    private convenience init() {
        // §1 / HIG: "dim (disable) the minimize and maximize buttons" — a settings window "isn't
        // meant to be resized to see more". `.resizable` was already never added (§11.8); dropping
        // `.miniaturizable` too (rather than only disabling the button) matters because
        // `NSWindow.performMiniaturize(_:)` — what `main.swift` binds ⌘M to — honours the style
        // mask, not the button's `isEnabled` state. Without `.miniaturizable`/`.resizable` in the
        // mask, AppKit draws both buttons dimmed on its own; no manual `isEnabled = false` needed.
        let win = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 460, height: 620),
            styleMask: [.titled, .closable],
            backing: .buffered, defer: false)
        win.isReleasedWhenClosed = false
        self.init(window: win)

        let selection = SettingsPaneSelection()
        self.selection = selection

        // §11.6 risk 5, RESOLVED: `NSHostingController.sizingOptions = [.intrinsicContentSize]`
        // reports the hosted SwiftUI content's fitting size to AppKit, and the window follows via
        // `preferredContentSize` propagation on both triggers that matter here — a pane switch and
        // a `Section(isExpanded:)` toggle — because both recompute the SwiftUI layout. `styleMask`
        // above never gains `.resizable`; the two mechanisms are orthogonal (§11.8).
        let hosting = NSHostingController(rootView: SettingsRootView(selection: selection))
        hosting.sizingOptions = [.intrinsicContentSize]
        hostingController = hosting
        win.contentViewController = hosting

        // §11.6 risk 6, RESOLVED: no SwiftUI → AppKit toolbar bridge exists for a manually
        // constructed NSWindow, so the pane switcher is a real NSToolbar, assigned directly.
        // HIG requires it stay noncustomizable and always indicate the active pane.
        let toolbar = NSToolbar(identifier: Self.toolbarIdentifier)
        toolbar.delegate = self
        toolbar.allowsUserCustomization = false
        toolbar.displayMode = .iconOnly
        win.toolbar = toolbar

        selection.onChange = { [weak self] pane in self?.applyPaneChrome(pane) }
        applyPaneChrome(selection.pane)
    }

    private func applyPaneChrome(_ pane: SettingsPane) {
        // §1: "the window title reflects the visible pane", not a static "Settings".
        window?.title = pane.title
        paneGroup?.selectedIndex = pane.rawValue
        // §11.6 risk 5, revised: measured directly (Debug and Release, repeatedly) that
        // `NSHostingController.sizingOptions = [.intrinsicContentSize]` does NOT resize this
        // window in practice — the window settled at 460×84 regardless of the hosted content's
        // own `.frame(height:)`, both before and after pinning height explicitly on the SwiftUI
        // side. Whatever propagates `preferredContentSize` to the window is not firing here (this
        // window is built from a bare `NSWindow` with `contentViewController` assigned after
        // construction, not from `NSWindow(contentViewController:)` — that may be exactly the gap,
        // but rather than trust an unverified theory a second time, drive the window size
        // directly and deterministically from the same per-pane constant `SettingsRootView` pins
        // its content to. `setContentSize(_:)` keeps the window's top-left corner fixed, so this
        // does not jump the window around on a pane switch.
        window?.setContentSize(NSSize(width: 460, height: pane.restingHeight))
    }

    @objc private func paneSwitcherChanged(_ sender: NSToolbarItemGroup) {
        guard let pane = SettingsPane(rawValue: sender.selectedIndex) else { return }
        selection.pane = pane
    }

    // MARK: NSToolbarDelegate

    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [Self.paneSwitcherIdentifier]
    }

    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [Self.paneSwitcherIdentifier]
    }

    func toolbar(
        _ toolbar: NSToolbar,
        itemForItemIdentifier itemIdentifier: NSToolbarItem.Identifier,
        willBeInsertedIntoToolbar flag: Bool
    ) -> NSToolbarItem? {
        guard itemIdentifier == Self.paneSwitcherIdentifier else { return nil }
        // Confirmed by runtime measurement, not just the header comment: hand-building
        // `NSToolbarItemGroup(itemIdentifier:)` with plain `NSToolbarItem` subitems and a
        // `target`/`action` on the GROUP produces a toolbar that renders three icons but never
        // calls `paneSwitcherChanged` on click (verified with an `lldb` breakpoint — zero hits
        // across all three icons) and never shows a persistent selection. `selectionMode` is
        // documented (`NSToolbarItemGroup.h:75-78`) as applying "only when using one of the
        // constructors ... with a system defined control representation" — i.e. exactly the
        // `groupWithItemIdentifier:images:selectionMode:labels:target:action:` factory below,
        // which builds a real segmented-control-backed view and IS click-responsive.
        let images = SettingsPane.allCases.map {
            NSImage(systemSymbolName: $0.symbolName, accessibilityDescription: $0.title) ?? NSImage()
        }
        let labels = SettingsPane.allCases.map(\.title)
        let group = NSToolbarItemGroup(
            itemIdentifier: itemIdentifier,
            images: images,
            selectionMode: .selectOne,
            labels: labels,
            target: self,
            action: #selector(paneSwitcherChanged(_:)))
        group.role = .tabs // NSToolbarItemGroupRoleTabs, macOS 27 — DESIGN.md §11.7; orthogonal to
                            // the factory's selection-mode control representation set above.
        group.selectedIndex = selection.pane.rawValue
        // `NSToolbar.h:213-218`: a vended item is not guaranteed to be inserted, and a fresh item
        // must be returned on every call. Only track it as *the* on-screen group when this vend is
        // actually going in, so a later non-inserted vend can't leave `applyPaneChrome` updating an
        // orphaned group while the real one stops tracking selection.
        if flag {
            paneGroup = group
        }
        return group
    }

    func show() {
        window?.center()
        window?.makeKeyAndOrderFront(nil)
    }
}

// MARK: - AA projection from the observable model
//
// REMOVED 2026-09-04: `AACapability.init(config:)`, the six-field bridge that snapshotted
// mainWidth/mainHeight/maxFPS/name/nightMode/rightHandDrive out of this CarPlay-shaped model.
// Android Auto now renders from the NEUTRAL profile via `AACapability.init(profile:adapter:
// autoThemeIsDark:warn:)` in AA/AACapability+Profile.swift, which is what AppDelegate calls.
//
// The old bridge could not be kept as a convenience: it is structurally incapable of expressing the
// two values that made the profile neutral in the first place — `theme == .auto` and
// `driverPosition == .center` both collapse into the legacy booleans it read. Leaving it in place
// would have offered a call site that silently downgrades them.
