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

    // Resolution / frame-rate bounds (user directive 2026-07-12). 24 fps was dropped from the box's
    // vocabulary — offering it silently yielded the default, so it is no longer offered.
    static let minWidth = 800, maxWidth = 3840
    static let minHeight = 480, maxHeight = 2160
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
    // Android Auto (our `android_auto:` YAML extension, docs/host/02_ANDROID_AUTO.mde). The box's session_supervisor reads
    // it: when an Android phone is on the USB bus and no CarPlay transport owns the box, it arms
    // `aa-bridge` (AOAP switch + byte pump) and reports `pmWiredAa`, on which this app runs its own AA
    // head-unit engine over CH_IP. Default ON — the box's own default is opt-out (`android_auto: false`).
    // CarPlay is unaffected either way: an iPhone always wins the CarPlay path first.
    @Published var androidAutoEnabled: Bool { didSet { markDirty() } }

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
    // Emitted as `accessoryName:` and the `iapConfig:` block. The box PARSES both today but does not
    // act on them yet (C-3 wires the identity into iap2d's Identify; C-6 applies the name), so
    // authoring here changes nothing on the wire until those land. That is deliberate: params 20/21
    // are Identify content, and an iOS `0x1D03` rejection cannot be retried within a session.

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
    static let audioCodecs = [
        "none", "pcm_16k_mono", "pcm_48k_stereo",
        "aac_lc_44k_stereo", "aac_lc_48k_stereo",
        "aac_eld_48k_stereo", "aac_eld_44k_stereo",
        "aac_eld_16k_mono", "aac_eld_24k_mono", "aac_eld_32k_mono",
        "aac_eld_44k_mono", "aac_eld_48k_mono",
        "opus_16k_mono", "opus_24k_mono", "opus_48k_mono",
    ]
    static let verifiedCodecs: Set<String> = [
        "none", "pcm_16k_mono", "pcm_48k_stereo", "aac_lc_48k_stereo", "aac_eld_16k_mono",
    ]
    /// `audioType` values iOS routes against (empty = the wired PCM catch-all — no audioType key).
    static let audioTypes = ["", "media", "default", "telephony", "speechRecognition", "alert", "compatibility"]
    /// CarPlay audio stream types the box arms: 100 MainAudio (bidir, carries mic), 101 AltAudio,
    /// 102 MainHighAudio (realtime media, AAC-LC).
    static let audioStreamTypes = [100, 101, 102]
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
    @Published var primaryInput: String { didSet { markDirty() } }
    // Apple's authoritative values only — "Touchscreen" is not a valid CarPlay primaryInput
    // (absent from every CarPlay Simulator vehicle config).
    static let primaryInputs = ["Touchpad", "Knobs"]

    // Appearance / display
    @Published var nightMode: Bool { didSet { markDirty() } }
    @Published var rightHandDrive: Bool { didSet { markDirty() } }
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
        func b(_ k: String, _ def: Bool) -> Bool { ud.object(forKey: Self.prefix + k) as? Bool ?? def }
        func i(_ k: String, _ def: Int) -> Int { ud.object(forKey: Self.prefix + k) as? Int ?? def }
        name = ud.string(forKey: Self.prefix + "name") ?? "CarLink Widescreen"
        wirelessEnabled = b("wirelessEnabled", true)
        hotHandover = b("hotHandover", false)
        pairingNumericComparison = b("pairingNumericComparison", false)
        androidAutoEnabled = b("androidAutoEnabled", true)
        mainWidth = i("mainWidth", 1920); mainHeight = i("mainHeight", 1080); maxFPS = i("maxFPS", 60)
        mainSafeLeft = i("mainSafeLeft", 0); mainSafeTop = i("mainSafeTop", 0)
        mainSafeRight = i("mainSafeRight", 0); mainSafeBottom = i("mainSafeBottom", 0)
        mainDrawOutsideSafe = b("mainDrawOutsideSafe", false)
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
        loading = false
        committedYAML = yaml   // the persisted state IS the committed/pushed state at launch
        committedConfig = config   // snapshot the structured config alongside the YAML (audit A4)
    }

    /// Commit the current form: clamp, persist to UserDefaults, snapshot the pushed YAML, clear dirty.
    /// This is the ONLY thing that changes what the adapter receives on the next connection.
    func save() {
        guard !loading else { return }
        saving = true
        defer { saving = false }
        clampInPlace()
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
        d.set(androidAutoEnabled, forKey: s + "androidAutoEnabled")
        d.set(mainWidth, forKey: s + "mainWidth"); d.set(mainHeight, forKey: s + "mainHeight"); d.set(maxFPS, forKey: s + "maxFPS")
        d.set(mainSafeLeft, forKey: s + "mainSafeLeft"); d.set(mainSafeTop, forKey: s + "mainSafeTop")
        d.set(mainSafeRight, forKey: s + "mainSafeRight"); d.set(mainSafeBottom, forKey: s + "mainSafeBottom")
        d.set(mainDrawOutsideSafe, forKey: s + "mainDrawOutsideSafe")
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
        CCPABridge.shared.client?.repushConfig(data())
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

    /// Clamp numeric fields to the allowed ranges (called on every save; the UI also clamps on commit).
    /// Assign only when the value changes — every write to an @Published prop still notifies SwiftUI,
    /// and the reentrancy guard already stops the didSet→save recursion.
    private func clampInPlace() {
        func clamp(_ v: inout Int, _ lo: Int, _ hi: Int) { let c = min(max(v, lo), hi); if c != v { v = c } }
        clamp(&mainWidth, Self.minWidth, Self.maxWidth)
        clamp(&mainHeight, Self.minHeight, Self.maxHeight)
        clamp(&altWidth, Self.minWidth, Self.maxWidth)
        clamp(&altHeight, Self.minHeight, Self.maxHeight)
        // 24 fps left the box's vocabulary — coerce a stale persisted 24 to the nearest offered rate.
        if !Self.frameRates.contains(maxFPS) { maxFPS = maxFPS == 24 ? 30 : 60 }
        if !Self.frameRates.contains(altFPS) { altFPS = 30 }
        // Safe-area insets: never negative, and opposite edges must leave a positive safe box (≥16px).
        // The box also re-validates and falls back to full-bleed, so this is a UX guard, not the gate.
        clampInsets(left: &mainSafeLeft, top: &mainSafeTop, right: &mainSafeRight, bottom: &mainSafeBottom,
                    width: mainWidth, height: mainHeight)
        clampInsets(left: &altSafeLeft, top: &altSafeTop, right: &altSafeRight, bottom: &altSafeBottom,
                    width: altWidth, height: altHeight)
    }

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

    /// The structured, SETUP-relevant slice of this config (plan P3) — the single source the app-driven
    /// SETUP author reads. Materialized from the SAME `@Published` fields the `yaml` above interpolates,
    /// so the YAML pushed to the box and the host-authored SETUP answers are built from one set of
    /// booleans and can never diverge. `altVideoStreamsPresent` mirrors the YAML's `altVideoStreams[]`
    /// presence (`altVideoEnabled`), which is what arms the box's `altScreen` feature.
    var config: VehicleConfig {
        VehicleConfig(
            enablesHEVC: enablesHEVC,
            enablesViewAreas: enablesViewAreas,
            enablesCornerMasks: enablesCornerMasks,
            enablesLogTransfer: enablesLogTransfer,
            enablesMainBufferedAudio: enablesMainBufferedAudio,
            altVideoStreamsPresent: altVideoEnabled,
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
        version: 1
        wireless: \(wirelessEnabled)
        hot_handover: \(hotHandover)
        pairing: \(pairingNumericComparison ? "numeric_comparison" : "just_works")
        android_auto: \(androidAutoEnabled)
        rightHandDrive: \(rightHandDrive)
        nightMode: \(nightMode)
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
        \(va(mainWidth, mainHeight, mainSafeLeft, mainSafeTop, mainSafeRight, mainSafeBottom, mainDrawOutsideSafe))
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
    /// The box PARSES this today but does not act on it yet (C-3 wires it into iap2d), so authoring
    /// here is safe ahead of that landing.
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
        androidAutoEnabled = true
        mainWidth = 1920; mainHeight = 1080; maxFPS = 60
        mainSafeLeft = 0; mainSafeTop = 0; mainSafeRight = 0; mainSafeBottom = 0; mainDrawOutsideSafe = false
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
        touchScreenHighFidelity = true; touchScreenSupportsCancel = true; primaryInput = "Touchpad"
        nightMode = false; rightHandDrive = false
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

// MARK: - Info popover infrastructure

/// A short plain-language explanation of a config field, shown in an (i) popover. Text is grounded in
/// the CarPlay SDK (docs/carplay/04_CAPABILITIES_AND_CONFIG.md glossary).
// Strings grounded in the CarPlaySDK glossary research (docs/carplay/04_CAPABILITIES_AND_CONFIG.md); each ≤ ~240 chars for a tooltip.
enum FieldInfo {
    /// YAML keys the box currently serde-IGNORES (2026-07-31 review): they ride the pushed config
    /// forward-compatibly but have zero on-wire effect today. Their tooltips keep the descriptive
    /// text and get the same ⚠️ marker convention nightMode/rightHandDrive already carry (inline,
    /// below), so no control overclaims what it does. LIVE levers (wireless, pairing, resolution,
    /// frame rate, safe areas, HEVC, the alt stream + its geometry, audio formats, dPadSupport,
    /// enablesViewAreas) are deliberately NOT in this set.
    private static let inertKeys: Set<String> = [
        "name",
        "enablesVideoPlayback", "primaryInput",
        // NOT inert, removed 2026-08-10 after a doc-vs-code sweep: airplayd ARMS all three from the
        // pushed config — knobSupport at main.rs:624 (set_knob_advertised -> the uid-4 hidDevices
        // entry), telephonyButtonsSupport at :626 (uid-5), enablesCornerMasks at :641. Marking them
        // inert told the owner a live setting does nothing.
        "mediaButtonsSupport",
        "knobSupportsHomeAndBackButton", "knobSupportsNudge", "touchpadSupport",
        "touchpadButtonsSupport", "touchScreenHighFidelity", "touchScreenSupportsCancel",
        // C-2: joins its already-inert HID siblings above. Auto-marking is the convention BECAUSE
        // the "un-mark it when it lands" ritual is removing the key here — a hand-written warning in
        // the description would be missed when C-7/C-8 wires the features word.
        "steeringWheelSupport",
        "enablesUIAppearance", "enablesMapAppearance", "enablesFocusTransfer",
        "enablesUIContext", "enablesUISync", "enablesFileTransfer",
        "enablesVehicleDataProtocol", "enablesDCX",
    ]
    private static let inertMarker =
        " ⚠️ Not yet implemented on the box — this setting rides the config but currently has no effect on the wire."

    static let text: [String: String] = {
        var t = descriptions
        for k in inertKeys { t[k] = (t[k] ?? "") + inertMarker }
        return t
    }()

    private static let descriptions: [String: String] = [
        // Config metadata only: the box (vehicle_config.rs) deliberately does NOT map YAML `name` to
        // the advertised accessory name, which is derived on-box from the MAC + serial.
        "name": "A label for this configuration template — config metadata only. The box deliberately does not use it as the advertised accessory name (that is derived on-box from the adapter's MAC + serial).",
        "wireless": "Advertise the box for wireless CarPlay (Bluetooth pairing + Wi-Fi handoff, shown as “CarLink” on the iPhone). The radios come up when this app connects and idle when it disconnects. Wired USB CarPlay works regardless — the box accepts whichever transport connects first. Off = wired-only (radios stay off).",
        "pairing": "Bluetooth pairing style. Off = Just-Works: iOS shows a simple “Pair?” prompt with no code (the proven Carlinkit dongle behavior). On = Numeric Comparison: the iPhone AND this app both show a 6-digit code (in the status area) to confirm they match — a more OEM-head-unit-like experience. Experimental for a dongle; iOS may be pickier about the CarPlay handshake in this mode.",
        "android_auto": "Project Android Auto from an Android phone plugged into the box. On (default): when an Android phone is on the box's USB bus and no CarPlay session owns it, the box switches the phone into accessory (AOAP) mode and pumps the Android Auto stream to this app, which runs the head unit itself. Off: an Android phone only charges. CarPlay is unaffected — an iPhone always takes the CarPlay path, and whichever phone connects first owns the box.",
        "hotHandover": "What happens when a USB cable is plugged into a live wireless session. Off = Standard (Apple-conformant default): the session stays wireless and USB just charges. On = Hot Hand-Over: the box forces a switch to wired on cable insert — a non-standard extension unique to this adapter.",
        // ⚠️ NOT YET IMPLEMENTED BOX-SIDE (2026-07-25; updated 2026-08-01): these two were previously
        // described as "sent in /info" / "switchable live via setNightMode", which was false — no
        // `nightMode`/`rightHandDrive` /info key exists and `vehicle_config.rs` does not parse them
        // (serde ignores them harmlessly). UPDATE 2026-08-01: a live `setNightMode` sender DOES now exist
        // (events.rs::send_set_night_mode, the Display-Appearance feature) — but the VehicleConfig
        // `nightMode` field here is still not wired to it, so both fields still have ZERO on-wire effect.
        // These descriptions are about the config fields, not the separate live appearance command.
        "rightHandDrive": "Tells CarPlay the driver sits on the right so iOS mirrors driver-focused layout that way. Off = left-hand drive. ⚠️ Not yet implemented on the box — this setting rides the config but currently has no effect on the wire.",
        "nightMode": "Drives CarPlay's light/dark UI (dark night appearance when on). ⚠️ This VehicleConfig field is not wired to /info and has no effect on the wire. Runtime night mode IS available via the live Night Mode toggle (Display Appearance — Live), which sends the separate setNightMode command; this config field does not.",
        "mainResolution": "The exact pixel grid CarPlay renders/streams (sets displays[].widthPixels/heightPixels + the touch coordinate space). Primary resolution lever; a change needs a fresh session. W 800–3840, H 480–2160.",
        "maxFPS": "Caps the video refresh rate to this display (Apple uses 60). Higher = smoother but more decode/bandwidth. Advertised as displays[].maxFPS; reconnect-only.",
        "safeArea": "Insets (px from each edge) of the box CarPlay keeps its UI inside. The video still fills the whole resolution; only interactive UI is held within the safe box — for curved/irregular panels where the corners/edges are occluded. 0 = flush (no inset). iOS honors this only when View areas is on.",
        "drawUIOutsideSafeArea": "Let CarPlay draw non-interactive UI in the gap between the full frame and the safe box. Off (default) = keep all UI strictly inside the safe area.",
        "altResolution": "Pixel size of the secondary (instrument-cluster / navigation) video stream — typically a smaller cluster screen. Omit for single-screen units.",
        "enablesHEVC": "Publishes non-null hevcInfo so iOS can stream efficient HEVC (H.265) instead of only H.264. Requires the unit to decode/forward HEVC.",
        "enablesVideoPlayback": "Advertises allowVideoPlayback so iOS can stream arbitrary fullscreen video apps (media/streaming), not just the CarPlay UI. Off = UI/nav only.",
        "enablesMainBufferedAudio": "Advertises mainBufferedInfo (~2-min media buffer, streamed from iPhone faster than real time) for playback resilience on an UNINTENTIONAL drop — media keeps playing while the link recovers. Does NOT improve audio quality. Advertise/negotiate is device-tested (wired: iOS negotiates it DISABLED over USB — it is a wireless-drop remedy). The box does not serve a buffered stream yet (the ~2-min buffer will live in THIS app — docs/carplay/04_CAPABILITIES_AND_CONFIG.md's owner-corrected architectural model, 2026-08-07), so on WIRELESS this is a deliberate per-session experiment: if iOS moves media to the buffered stream, media goes silent until you turn this off and reconnect. Default off; applies at the next connection.",
        "audioFormats": "The exact set of audio capabilities the box advertises to iOS (the /info audioFormats). iOS negotiates one entry per audioType from this set. Auto = match the transport (PCM over USB, the AAC set over wireless). Presets are ready-made sets; Custom lets you author any codec/rate/stream-type combination to test a specific head-unit audio config.",
        "metadataTier": "Which metadata feeds the accessory DECLARES to iOS in its iAP2 identification (and then subscribes to): now playing, call state, route guidance, and so on. Proven is the declaration a real iPhone accepted on 2026-07-25 and is what the adapter uses when nothing is pushed — leave it here unless you are deliberately testing a wider set. Extended adds the full paired Start/Stop set — accepted once on a wireless session's tunnel Identify (2026-07-25), but NOT proven on the wired Identify, where an earlier extended form was rejected. All declares everything in the capability table. ⚠️ iOS validates this: a declaration it rejects kills the whole identification for that connection and cannot be retried within it, so raise the tier one step at a time and watch the phone's own log (idevicesyslog -p accessoryd). Applies at the next iAP2 link — unplug/replug the phone, not just a reconnect of this app.",
        "accessoryName": "The name this adapter shows as on the iPhone. Leave empty to keep the adapter's built-in per-box name (CarLink plus the last four characters of its Wi-Fi address), which is what ships today and is what keeps two adapters distinguishable. A name you set is used verbatim, so make it distinct yourself. ⚠️ Not yet applied by the adapter — it is stored and pushed, but changing the advertised name touches the AirPlay /info name, the Bonjour service name and the iAP2 identification together, so it is enabled in a later step with the phone's own log being watched.",
        "engineTypes": "What powers this vehicle. Sent to iOS as part of the vehicle identification so CarPlay and Maps can adapt — the electric setting is the foundation for EV features such as charging-aware routing. A hybrid is genuinely two selections (for example gasoline AND electric); Apple's format allows several. Selecting nothing keeps the adapter's built-in default of gasoline, which is what it has always sent. ⚠️ Not yet applied by the adapter; a later step enables it with the phone's rejection log being watched, because iOS can refuse a whole identification it dislikes and cannot be asked again until the phone is replugged.",
        "chargingConnectors": "Which charging connectors this vehicle physically has, and optionally how fast each can charge. Only meaningful for electric or plug-in hybrid vehicles. Each connector type may appear ONCE — Apple's format carries one power rating per type, so a repeated type cannot be represented and the adapter keeps only the first. Set the power to 0 to leave the rating unstated; 0 is sent as \"no rating\", not as zero watts. ⚠️ Recorded in the pushed configuration but not yet applied — the adapter parses it and does not yet present it to the phone.",
        "vehicleStatusEnabled": "Declares that this vehicle can report live status to the phone — range, temperatures, charge state and so on. ⚠️ DISABLED until the adapter can service it. The adapter does not yet declare the messages that carry this data, so announcing the capability without them is exactly the kind of inconsistency iOS rejects, and a rejected identification kills CarPlay for that connection and cannot be retried until the phone is replugged. The control is shown greyed rather than hidden so you can see the capability is planned; it will unlock when the adapter declares the messages that carry this data.",
        "steeringWheelSupport": "This vehicle has steering-wheel buttons for CarPlay (up/down/left/right/select). Corresponds to the Direction Buttons capability iOS looks for. It is not yet wired to the display capability word the adapter sends — that is a later step, because correcting the word also drops a capability the adapter currently claims without backing it, which is a change worth watching a real session for.",
        "metadataSkip": "Comma-separated feature names to DROP from the declaration above (e.g. call_history). Use this to narrow a tier that iOS rejected, rather than dropping back a whole level. Names are the adapter's feature-table names; anything unrecognized is ignored. Leave empty for the full tier.",
        "audioModeAuto": "Advertise the set that matches how the phone connects: PCM-only over the USB/wired link (iOS delivers media as PCM there); the full AAC set (media AAC-LC + Siri/mic AAC-ELD) over wireless. The app pushes both arms explicitly and the adapter presents the matching one. The proven default — use this unless you're testing a specific config.",
        "audioFormatRow": "One advertised capability: a stream type (100 MainAudio carries the mic; 102 MainHighAudio is high-latency media (AAC-LC)), an audioType iOS routes against (e.g. media, speechRecognition), an input codec (mic capture; None = playback-only) and an output codec (playback to the box).",
        "enablesEnhancedSiri": "Publishes enhancedSiriInfo so the vehicle's own hardware Siri button can invoke Siri (siriAction prewarm / button-down / button-up), with supported-language hints and mixable Siri audio. Off = basic Siri via the CarPlay Siri button. Either way Siri listens on the car's microphone (the speechRecognition audio path) — enhanced just adds the hardware-button trigger, language hints, and audio mixing. ⚠️ Out of scope — not pursued (needs an independent hot-word / voice-analysis stack); the box does not declare enhancedSiri, so this toggle has no effect.",
        // Corrected 2026-07-31 against info.rs:335-370: dPadSupport gates ONLY the hidDevices[] D-Pad
        // entry and contributes NO display features bit (0x20 DirectionButtons comes from
        // steeringWheelSupport; 0x10 is Touchpad).
        "dPadSupport": "Advertises the 4/8-way directional-pad HID device, so list navigation (Up/Down/Left/Right/Select) works. Gates only the D-Pad hidDevices[] entry — it contributes no display features bit (Direction-Buttons 0x20 comes from steeringWheelSupport). Absent on knob-only units.",
        "mediaButtonsSupport": "Advertises play/pause/skip transport keys as a HID device.",
        "telephonyButtonsSupport": "Advertises accept/end call keys so hardware call control works.",
        "knobSupport": "Advertises a rotary knob HID — turn = scroll/rotate, press = select; CarPlay uses a focus-based UI. Prerequisite for the knob sub-options. NOTE the knob descriptor bytes are still the open item — Apple's literal builders (HIDKnob.c, 70 B and 51 B forms) have not been ported yet, so the device is advertised but its descriptor provenance is unresolved.",
        "knobSupportsHomeAndBackButton": "The knob reports dedicated Home and Back presses. Requires knob support.",
        "knobSupportsNudge": "The knob reports tilts (left/right/up/down) plus rotation, for grid navigation.",
        "touchpadSupport": "Advertises an absolute touchpad HID (finger position drives a focus cursor). Often paired with Primary input = Touchpad.",
        "touchpadButtonsSupport": "The touchpad reports press/click buttons in addition to position. Requires touchpad support.",
        "touchScreenHighFidelity": "High-fidelity absolute touch HID streaming continuous coords, so scroll/drag/gestures work like a phone. Maps to the HighFidelityTouch display bit (0x08). Off = no touchscreen (touchScreenMode: Disabled).",
        "touchScreenSupportsCancel": "The touch HID includes a cancel flag so interrupted / palm-rejected touches aren't treated as false taps.",
        "primaryInput": "The main input device, so CarPlay optimizes its control model: Touchpad (remote pad drives focus) or Knobs (rotary controller). Maps to displays[].primaryInputDevice.",
        "enablesUIAppearance": "Advertises UI-appearance control so the head unit can drive CarPlay's look (uiAppearanceUpdate). Enabled in every Apple template.",
        "enablesMapAppearance": "Advertises map-appearance control (mapAppearanceUpdate, changeMapZoomLevel); also required for the alt/cluster map stream.",
        "enablesCornerMasks": "Declares the display can be masked at the corners (rounded/cut edges); the car streams per-corner opaque bitmaps at runtime. CarPlay's \"cutout\" mechanism.",
        "enablesViewAreas": "Support declaring view areas / safe areas within the display (a usable sub-rectangle inside a wider panel, split layouts).",
        "enablesFocusTransfer": "Focus can move between CarPlay and the head unit's own UI (split screens / multi-display). Off = CarPlay keeps focus.",
        "enablesUIContext": "Publishes/updates UI context (which app/screen is showing) so the car can react. Off = no context reporting.",
        "enablesUISync": "CarPlay and the head unit keep certain UI state in step. Off = no sync. [inferred]",
        "enablesFileTransfer": "Advertises the file/asset transfer capability so iOS can push assets to the accessory. Exact wire use is not evidenced in Apple's sources. [inferred]",
        "enablesLogTransfer": "Advertises logTransfer — the accessory tells iOS it can supply a diagnostic log archive. The advertise/negotiate handshake is device-proven on the wire (wired). The box deliberately does NOT serve the archive and won't: not a privacy limit — Apple won't troubleshoot a non-conventional CarPlay implementation, so it would be wasted effort. Advertise/negotiate only; default off.",
        "enablesVehicleDataProtocol": "Advertises vehicleStateProtocol, opening the two-channel Vehicle Data Protocol for route status + vehicle state (VDC). Needed for turn-by-turn cluster / nav telemetry.",
        "enablesDCX": "\"DCX\" — purpose NOT evidenced. Only the property name exists in the simulator; no CarPlaySDK string or wire mapping was found. A \"dynamic content\" meaning is unverified. Leave default.",
        "appDrivenSetup": "Default ON, BOTH transports (wireless included since 2026-08-10). When on, the box relays the RTSP/SETUP negotiation to this app over CH_RTSP and the app AUTHORS the response the phone sees (the box's own local response is the sticky fallback on any relay failure). On wireless the app authors the video/audio streams it knows, echoes the box's own answer for the type-130 RCS DataStream that only that transport carries, and preserves the two adapter-only feature tokens it has no setting for (iAPChannel, sessionManagement) — dropping the first would break the phone's iAP2 metadata link. Off = fully box-driven SETUP. Takes effect on the NEXT connection, not immediately: reconnect the phone (unplug/replug) or restart the adapter — the app-side connection cycle alone is not enough, because airplayd reads the config per phone connection.",
        "limitedUIConfig": "Declares WHICH CarPlay UI elements the limited-UI (Drive) restriction applies to — advertised in /info as limitedUIElements. Off = declare nothing: iOS applies its own default restriction set (the proven behavior). The restriction itself is toggled at runtime in Window ▸ Controls ▸ UI.",
    ]
}

/// A form-row label with an (i) button that reveals the field's explanation in a popover.
private struct InfoLabel: View {
    let title: String
    let key: String
    @State private var show = false
    var body: some View {
        HStack(spacing: 5) {
            Text(title)
            if let info = FieldInfo.text[key] {
                Button { show.toggle() } label: {
                    Image(systemName: "info.circle").foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
                .popover(isPresented: $show, arrowEdge: .trailing) {
                    Text(info).font(.callout).padding(12).frame(width: 300)
                }
            }
        }
    }
}

/// A Toggle whose label carries an (i) info popover.
private struct InfoToggle: View {
    let title: String
    let key: String
    @Binding var isOn: Bool
    var body: some View { Toggle(isOn: $isOn) { InfoLabel(title: title, key: key) } }
}

// MARK: - Configuration form

private struct ResolutionField: View {
    let title: String
    @Binding var width: Int
    @Binding var height: Int
    var infoKey: String = "mainResolution"  // alt row passes "altResolution" so its (i) shows the alt text

    /// Preset options; nil dims = Custom (fields become editable). `shortLabel` fits the segmented control.
    private static let presets: [(shortLabel: String, w: Int?, h: Int?)] = [
        ("SD", 1280, 720),
        ("HD", 1920, 1080),
        ("4K", 3840, 2160),
        ("Custom", nil, nil),
    ]

    /// Custom is an explicit user choice, NOT derivable from the numbers: e.g. 1280×720 both *is* the
    /// SD preset and is a legal custom value. Deriving `isCustom` purely from a preset match made
    /// "Custom" unreachable whenever the dims happened to equal a preset (the picker snapped back and
    /// the fields stayed greyed). Track the choice; seed it true when a loaded config matches no preset.
    @State private var forceCustom = false

    private static var customIndex: Int { presets.count - 1 }
    private var matchedPreset: Int? {
        Self.presets.firstIndex { $0.w == width && $0.h == height }
    }
    /// The preset index matching the current W×H, or the Custom index if the user chose Custom / no
    /// preset matches.
    private var selectedIndex: Int {
        if forceCustom { return Self.customIndex }
        return matchedPreset ?? Self.customIndex
    }
    private var isCustom: Bool { selectedIndex == Self.customIndex }

    private var widthError: Bool {
        isCustom && (width < VehicleConfigModel.minWidth || width > VehicleConfigModel.maxWidth)
    }
    private var heightError: Bool {
        isCustom && (height < VehicleConfigModel.minHeight || height > VehicleConfigModel.maxHeight)
    }

    var body: some View {
        // Segmented preset picker (matches the Frame Rate picker style).
        Picker(selection: Binding(
            get: { selectedIndex },
            set: { idx in
                if let w = Self.presets[idx].w, let h = Self.presets[idx].h {
                    width = w; height = h; forceCustom = false
                } else {
                    // Custom: keep current values; unlock the fields for editing.
                    forceCustom = true
                }
            }
        )) {
            ForEach(Self.presets.indices, id: \.self) { Text(Self.presets[$0].shortLabel).tag($0) }
        } label: {
            InfoLabel(title: title, key: infoKey)
        }
        .pickerStyle(.segmented)

        // The manual custom fields, labeled "Resolution"; editable only in Custom.
        LabeledContent("Resolution") {
            HStack(spacing: 4) {
                TextField("W", value: $width, format: .number)
                    .frame(width: 62).multilineTextAlignment(.trailing)
                    .foregroundStyle(widthError ? Color.red : Color.primary)
                Text("×").foregroundStyle(.secondary)
                TextField("H", value: $height, format: .number)
                    .frame(width: 62).multilineTextAlignment(.trailing)
                    .foregroundStyle(heightError ? Color.red : Color.primary)
                Text("px").foregroundStyle(.tertiary).font(.caption)
            }
            .textFieldStyle(.roundedBorder)
            .disabled(!isCustom)
            .opacity(isCustom ? 1 : 0.5)
        }
    }
}

/// A live, proportional picture of the display: a faded box at the resolution's aspect ratio (the
/// viewArea — the video always fills this) with an inner solid box for the safe area, positioned by
/// the insets. Lets the user SEE the safe area, not just read numbers (WWDC 2019-252: the safe area
/// is the rectangle where CarPlay keeps interactive UI; outside it is black unless "draw outside" is on).
private struct SafeAreaPreview: View {
    let resW: Int, resH: Int
    let left: Int, top: Int, right: Int, bottom: Int
    private let maxW: CGFloat = 260, maxH: CGFloat = 132

    /// The on-screen canvas for the display box — the resolution's aspect, scaled to fit maxW×maxH.
    private var canvas: CGSize {
        guard resW > 0, resH > 0 else { return CGSize(width: maxW, height: maxW * 9 / 16) }
        let a = CGFloat(resW) / CGFloat(resH)
        var w = maxW, h = maxW / a
        if h > maxH { h = maxH; w = maxH * a }
        return CGSize(width: w.rounded(), height: h.rounded())
    }

    var body: some View {
        let c = canvas
        let fw = CGFloat(max(resW, 1)), fh = CGFloat(max(resH, 1))
        let l = CGFloat(max(0, left)), t = CGFloat(max(0, top))
        let sw = max(1, fw - l - CGFloat(max(0, right)))
        let sh = max(1, fh - t - CGFloat(max(0, bottom)))
        VStack(spacing: 5) {
            ZStack(alignment: .topLeading) {
                // The display / viewArea — faded fill (video fills the whole rectangle).
                RoundedRectangle(cornerRadius: 5)
                    .fill(Color.accentColor.opacity(0.12))
                    .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(Color.secondary.opacity(0.55)))
                // The safe area — solid inner box, offset + sized proportionally to the insets.
                RoundedRectangle(cornerRadius: 3)
                    .fill(Color.accentColor.opacity(0.30))
                    .overlay(RoundedRectangle(cornerRadius: 3).strokeBorder(Color.accentColor, lineWidth: 1.5))
                    .frame(width: c.width * sw / fw, height: c.height * sh / fh)
                    .offset(x: c.width * l / fw, y: c.height * t / fh)
            }
            .frame(width: c.width, height: c.height)
            HStack(spacing: 12) {
                legend(Color.accentColor.opacity(0.18), "Display \(resW)×\(resH)")
                legend(Color.accentColor.opacity(0.55), "Safe area")
            }
            .font(.caption2).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 2)
    }

    private func legend(_ color: Color, _ text: String) -> some View {
        HStack(spacing: 4) {
            RoundedRectangle(cornerRadius: 2).fill(color)
                .frame(width: 12, height: 10)
                .overlay(RoundedRectangle(cornerRadius: 2).strokeBorder(.secondary.opacity(0.5)))
            Text(text)
        }
    }
}

/// Per-edge safe-area inset editor with a live visual preview. The user enters px from each edge; the
/// resulting safe box is shown as a number AND drawn to scale. Converted to the wire's absolute rect
/// (originX/width) on Save. `allowsDrawOutside` gates the drawUIOutsideSafeArea toggle — a main-display-
/// only flag (WWDC 2023-10150), so cluster streams hide it.
private struct SafeAreaField: View {
    @Binding var left: Int
    @Binding var top: Int
    @Binding var right: Int
    @Binding var bottom: Int
    @Binding var drawOutside: Bool
    let resWidth: Int
    let resHeight: Int
    var allowsDrawOutside: Bool = true

    private var safeW: Int { resWidth - max(0, left) - max(0, right) }
    private var safeH: Int { resHeight - max(0, top) - max(0, bottom) }
    private var invalid: Bool {
        left < 0 || top < 0 || right < 0 || bottom < 0 || safeW < 16 || safeH < 16
    }
    private var isInset: Bool { left > 0 || top > 0 || right > 0 || bottom > 0 }

    private func field(_ label: String, _ value: Binding<Int>) -> some View {
        HStack(spacing: 3) {
            Text(label).font(.caption).foregroundStyle(.secondary).frame(width: 16, alignment: .leading)
            TextField("0", value: value, format: .number)
                .frame(width: 54).multilineTextAlignment(.trailing)
                .foregroundStyle(invalid ? Color.red : Color.primary)
        }
    }

    var body: some View {
        LabeledContent {
            Grid(horizontalSpacing: 10, verticalSpacing: 4) {
                GridRow { field("L", $left); field("T", $top) }
                GridRow { field("R", $right); field("B", $bottom) }
            }
            .textFieldStyle(.roundedBorder)
        } label: {
            InfoLabel(title: "Safe area (insets, px)", key: "safeArea")
        }

        LabeledContent("Safe box") {
            Text(isInset ? "\(max(0, safeW)) × \(max(0, safeH)) px @ (\(max(0, left)), \(max(0, top)))"
                         : "full frame (no inset)")
                .font(.caption)
                .foregroundStyle(invalid ? Color.red : .secondary)
        }
        if invalid {
            Text("Insets leave too little room — each side must keep ≥16 px.")
                .font(.caption).foregroundStyle(.red)
        }

        SafeAreaPreview(resW: resWidth, resH: resHeight, left: left, top: top, right: right, bottom: bottom)

        // drawUIOutsideSafeArea is a MAIN-display-only flag (WWDC 2023-10150) — cluster streams omit it.
        if allowsDrawOutside {
            InfoToggle(title: "Allow UI outside safe area", key: "drawUIOutsideSafeArea", isOn: $drawOutside)
                .disabled(!isInset)
                .opacity(isInset ? 1 : 0.5)
        }
    }
}

private struct FrameRatePicker: View {
    let title: String
    @Binding var fps: Int
    var body: some View {
        Picker(selection: $fps) {
            ForEach(VehicleConfigModel.frameRates, id: \.self) { Text("\($0) fps").tag($0) }
        } label: { InfoLabel(title: title, key: "maxFPS") }
        .pickerStyle(.segmented)
    }
}

// MARK: - Audio capability config

/// Friendly labels for the audio vocabulary (the raw tokens are the box's wire names).
enum AudioLabels {
    static func codec(_ t: String) -> String {
        if t == "none" { return "None" }
        return t
            .replacingOccurrences(of: "aac_lc", with: "AAC-LC")
            .replacingOccurrences(of: "aac_eld", with: "AAC-ELD")
            .replacingOccurrences(of: "pcm", with: "PCM")
            .replacingOccurrences(of: "opus", with: "Opus")
            .replacingOccurrences(of: "_", with: " ")
    }
    static func stream(_ t: Int) -> String {
        switch t {
        case 100: return "100 · MainAudio"
        case 101: return "101 · AltAudio"
        case 102: return "102 · MainHighAudio"
        default: return "\(t)"
        }
    }
    static func audioType(_ t: String) -> String { t.isEmpty ? "— (catch-all)" : t }
    /// One-line summary of what a non-custom mode advertises, so the user SEES the resolved set.
    static func modeSummary(_ mode: String) -> String {
        switch mode {
        case "auto": return "PCM over wired · full AAC set over wireless (matches how the phone connects)."
        case "wired_pcm": return "PCM 16k/48k on types 100/101 — the wired media path (no audioType)."
        case "wireless_8": return "8 entries: AAC-LC media (102) · AAC-ELD Siri/mic (100) · AAC-ELD alert (100/101) · PCM compatibility."
        default: return ""
        }
    }
}

/// A codec picker that marks each option device-verified (●) vs advertisable-but-not-yet-proven (○), so
/// the user knows which formats are documented-capable today. This is the honesty surface the config is
/// meant to provide.
private struct CodecPicker: View {
    let title: String
    @Binding var value: String
    var body: some View {
        Picker(title, selection: $value) {
            ForEach(VehicleConfigModel.audioCodecs, id: \.self) { c in
                let mark = VehicleConfigModel.verifiedCodecs.contains(c) ? "●" : "○"
                Text("\(mark)  \(AudioLabels.codec(c))").tag(c)
            }
        }
    }
}

/// The custom `audio.formats` editor: an add/remove list of advertised entries, each with stream type,
/// audioType, input (mic) and output (playback) codec pickers. Fully declarative — this is where any
/// head-unit audio configuration is authored for testing.
private struct AudioFormatsEditor: View {
    @ObservedObject var model: VehicleConfigModel

    var body: some View {
        ForEach($model.audioFormats) { $row in
            GroupBox {
                Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 6) {
                    GridRow {
                        Text("Stream").font(.caption).foregroundStyle(.secondary).gridColumnAlignment(.leading)
                        Picker("", selection: $row.streamType) {
                            ForEach(VehicleConfigModel.audioStreamTypes, id: \.self) {
                                Text(AudioLabels.stream($0)).tag($0)
                            }
                        }.labelsHidden()
                    }
                    GridRow {
                        Text("Type").font(.caption).foregroundStyle(.secondary)
                        Picker("", selection: $row.audioType) {
                            ForEach(VehicleConfigModel.audioTypes, id: \.self) {
                                Text(AudioLabels.audioType($0)).tag($0)
                            }
                        }.labelsHidden()
                    }
                    GridRow {
                        Text("In (mic)").font(.caption).foregroundStyle(.secondary)
                        CodecPicker(title: "", value: $row.input).labelsHidden()
                    }
                    GridRow {
                        Text("Out").font(.caption).foregroundStyle(.secondary)
                        CodecPicker(title: "", value: $row.output).labelsHidden()
                    }
                }
                HStack {
                    Spacer()
                    Button(role: .destructive) {
                        model.audioFormats.removeAll { $0.id == row.id }
                    } label: { Label("Remove", systemImage: "trash").labelStyle(.iconOnly) }
                    .buttonStyle(.borderless)
                }
            }
        }
        Button {
            model.audioFormats.append(AudioFormatRow())
        } label: { Label("Add format", systemImage: "plus.circle") }
        if model.audioFormats.allSatisfy({ $0.output == "none" }) {
            Text("At least one entry needs an output codec, or the box keeps its default set.")
                .font(.caption).foregroundStyle(.orange)
        }
    }
}

/// A read-only reference of every audio format the box + app support (the vocabulary a custom config
/// draws from). Fulfills the "list the capabilities that can be configured" ask; ● = device-verified.
private struct AudioCapabilitiesReference: View {
    @State private var expanded = false
    var body: some View {
        DisclosureGroup("Supported audio formats", isExpanded: $expanded) {
            VStack(alignment: .leading, spacing: 3) {
                Text("● device-verified · ○ advertisable, not yet on-box proven")
                    .font(.caption2).foregroundStyle(.secondary)
                ForEach(VehicleConfigModel.audioCodecs.filter { $0 != "none" }, id: \.self) { c in
                    let ok = VehicleConfigModel.verifiedCodecs.contains(c)
                    HStack(spacing: 6) {
                        Text(ok ? "●" : "○").foregroundStyle(ok ? Color.green : Color.secondary)
                        Text(AudioLabels.codec(c))
                        Spacer()
                        Text(c).font(.system(.caption2, design: .monospaced)).foregroundStyle(.tertiary)
                    }
                    .font(.caption)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// Live (runtime) display-appearance controls — the Light/Dark and Night Mode toggles the Simulator
/// exposes per display. These send `uiAppearanceUpdate` / `mapAppearanceUpdate` / `setNightMode` on the
/// live session (via `ControlsBridge`), distinct from the `/info` capability flags in the config
/// "Appearance" section above. The same state drives the inline sun/moon titlebar buttons; a change
/// here moves those and vice-versa. Requires the matching `enablesUIAppearance`/`enablesMapAppearance`
/// capability declared for iOS to honour it (the config section above).
private struct LiveAppearanceSection: View {
    @ObservedObject var bridge: ControlsBridge

    private func uiBinding(alt: Bool) -> Binding<Bool> {
        Binding(get: { alt ? bridge.altUIDark : bridge.mainUIDark },
                set: { bridge.setUIAppearance(alt: alt, dark: $0) })
    }
    private func mapBinding(alt: Bool) -> Binding<Bool> {
        Binding(get: { alt ? bridge.altMapDark : bridge.mainMapDark },
                set: { bridge.setMapAppearance(alt: alt, dark: $0) })
    }

    var body: some View {
        Section {
            Toggle("Main display — Dark UI", isOn: uiBinding(alt: false))
            Toggle("Main display — Dark map", isOn: mapBinding(alt: false))
            Toggle("Alt/cluster display — Dark UI", isOn: uiBinding(alt: true))
            Toggle("Alt/cluster display — Dark map", isOn: mapBinding(alt: true))
            Toggle("Send night mode now (day/night signal)",
                   isOn: Binding(get: { bridge.nightModeOn }, set: { bridge.setNightMode($0) }))
        } header: {
            Text("Display Appearance — Live")
        } footer: {
            Text("Sends Light/Dark to the phone on the live session (same as the sun/moon in each window's title bar). Needs a connected session; the choice is remembered and re-sent on reconnect. The four Light/Dark toggles require \"UI/Map appearance sync\" (the config Appearance section above) so iOS accepts them; the night-mode signal is independent. Distinct from the static \"Night mode\" config flag above, which is a /info capability declaration, not a live command.")
                .font(.caption)
        }
    }
}

struct ConfigurationTab: View {
    @ObservedObject var model = VehicleConfigModel.shared
    @State private var showYAML = false

    var body: some View {
        Form {
            Section("Identity") {
                // Inert on the box (see FieldInfo "name") — same InfoLabel/(i) affordance as the
                // other inert controls so the field doesn't overclaim what it does.
                TextField(text: $model.name) { InfoLabel(title: "Name", key: "name") }
            }

            Section {
                InfoToggle(title: "Wireless CarPlay", key: "wireless", isOn: $model.wirelessEnabled)
                if model.wirelessEnabled {
                    InfoToggle(title: "Numeric Comparison pairing", key: "pairing", isOn: $model.pairingNumericComparison)
                    InfoToggle(title: "Hot Hand-Over", key: "hotHandover", isOn: $model.hotHandover)
                }
                InfoToggle(title: "Android Auto", key: "android_auto", isOn: $model.androidAutoEnabled)
            } header: {
                Text("Connectivity")
            }

            Section {
                ResolutionField(title: "Preset", width: $model.mainWidth, height: $model.mainHeight)
                FrameRatePicker(title: "Frame rate", fps: $model.maxFPS)
                SafeAreaField(left: $model.mainSafeLeft, top: $model.mainSafeTop,
                              right: $model.mainSafeRight, bottom: $model.mainSafeBottom,
                              drawOutside: $model.mainDrawOutsideSafe,
                              resWidth: model.mainWidth, resHeight: model.mainHeight)
                InfoToggle(title: "HEVC (H.265)", key: "enablesHEVC", isOn: $model.enablesHEVC)
                InfoToggle(title: "Video playback", key: "enablesVideoPlayback", isOn: $model.enablesVideoPlayback)
            } header: {
                Text("Main Video")
            } footer: {
                Text("Width must be \(VehicleConfigModel.minWidth)–\(VehicleConfigModel.maxWidth) px and height \(VehicleConfigModel.minHeight)–\(VehicleConfigModel.maxHeight) px. Out-of-range values show in red and are corrected on Save.")
                    .font(.caption)
            }

            Section("Alt / Navigation Video") {
                Toggle("Enable alt video stream", isOn: $model.altVideoEnabled)
                if model.altVideoEnabled {
                    ResolutionField(title: "Preset", width: $model.altWidth, height: $model.altHeight, infoKey: "altResolution")
                    FrameRatePicker(title: "Frame rate", fps: $model.altFPS)
                    SafeAreaField(left: $model.altSafeLeft, top: $model.altSafeTop,
                                  right: $model.altSafeRight, bottom: $model.altSafeBottom,
                                  drawOutside: $model.altDrawOutsideSafe,
                                  resWidth: model.altWidth, resHeight: model.altHeight,
                                  allowsDrawOutside: false)
                }
            }

            Section {
                Picker(selection: $model.audioMode) {
                    Text("Auto — match transport").tag("auto")
                    Text("Wired — PCM").tag("wired_pcm")
                    Text("Wireless — AAC (full 8)").tag("wireless_8")
                    Text("Custom…").tag("custom")
                } label: { InfoLabel(title: "Audio formats", key: "audioFormats") }

                if model.audioMode == "custom" {
                    InfoLabel(title: "Custom advertised set", key: "audioFormatRow")
                    AudioFormatsEditor(model: model)
                } else {
                    Text(AudioLabels.modeSummary(model.audioMode))
                        .font(.caption).foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }

                AudioCapabilitiesReference()

                InfoToggle(title: "Main buffered audio (media)", key: "enablesMainBufferedAudio", isOn: $model.enablesMainBufferedAudio)
                InfoToggle(title: "Enhanced Siri", key: "enablesEnhancedSiri", isOn: $model.enablesEnhancedSiri)

            } header: {
                Text("Audio")
            } footer: {
                Text("The set of audio capabilities the box advertises to iOS. Auto keeps the proven per-transport default; Custom authors any codec/rate/stream-type combination for a specific head-unit test.")
                    .font(.caption)
            }

            Section("Input — HID (hidConfig)") {
                Picker(selection: $model.primaryInput) {
                    ForEach(VehicleConfigModel.primaryInputs, id: \.self) { Text($0).tag($0) }
                } label: { InfoLabel(title: "Primary input", key: "primaryInput") }
                InfoToggle(title: "D-Pad support", key: "dPadSupport", isOn: $model.dPadSupport)
                InfoToggle(title: "Media buttons", key: "mediaButtonsSupport", isOn: $model.mediaButtonsSupport)
                InfoToggle(title: "Telephony buttons", key: "telephonyButtonsSupport", isOn: $model.telephonyButtonsSupport)
                InfoToggle(title: "Knob support", key: "knobSupport", isOn: $model.knobSupport)
                if model.knobSupport {
                    InfoToggle(title: "Knob Home/Back buttons", key: "knobSupportsHomeAndBackButton", isOn: $model.knobSupportsHomeAndBackButton)
                    InfoToggle(title: "Knob nudge (4-way)", key: "knobSupportsNudge", isOn: $model.knobSupportsNudge)
                }
                InfoToggle(title: "Touchpad", key: "touchpadSupport", isOn: $model.touchpadSupport)
                if model.touchpadSupport {
                    InfoToggle(title: "Touchpad buttons", key: "touchpadButtonsSupport", isOn: $model.touchpadButtonsSupport)
                }
                InfoToggle(title: "Touchscreen high-fidelity", key: "touchScreenHighFidelity", isOn: $model.touchScreenHighFidelity)
                InfoToggle(title: "Touchscreen supports cancel", key: "touchScreenSupportsCancel", isOn: $model.touchScreenSupportsCancel)
                InfoToggle(title: "Steering-wheel buttons", key: "steeringWheelSupport", isOn: $model.steeringWheelSupport)
            }

            // Vehicle identity (docs/carplay/04_CAPABILITIES_AND_CONFIG.md C6/C7) — what the car IS, as opposed to what its screen is.
            // Everything here is absent-off: leave it untouched and the adapter presents exactly the
            // identity it presented before this panel existed.
            Section("Vehicle Identity") {
                TextField(text: $model.accessoryName, prompt: Text("CarLink-<box id>")) {
                    InfoLabel(title: "Accessory name", key: "accessoryName")
                }
                InfoLabel(title: "Engine type", key: "engineTypes")
                ForEach(VehicleConfigModel.engineTypeNames, id: \.self) { e in
                    Toggle(VehicleConfigModel.engineDisplayNames[e] ?? e, isOn: Binding(
                        get: { model.engineTypes.contains(e) },
                        set: { on in
                            if on { model.engineTypes.insert(e) } else { model.engineTypes.remove(e) }
                        }))
                }
                if model.engineTypes.contains("electric") || !model.chargingConnectors.isEmpty {
                    InfoLabel(title: "Charging connectors", key: "chargingConnectors")
                    ForEach($model.chargingConnectors) { $row in
                        HStack {
                            Picker("", selection: $row.type) {
                                ForEach(VehicleConfigModel.connectorNames, id: \.self) { Text($0).tag($0) }
                            }.labelsHidden().frame(width: 110)
                            TextField("kW", value: Binding(
                                get: { (row.powerWatts ?? 0) / 1000 },
                                // CLAMP BEFORE THE MULTIPLY: UInt32 overflow traps, and any entry
                                // above 4,294,967 kW would crash the app on a stray keystroke.
                                // 1,000 kW is already far beyond any production charger.
                                set: { row.powerWatts = $0 > 0 ? min($0, 1_000) * 1000 : nil }
                            ), format: .number).frame(width: 60)
                            Text("kW").foregroundStyle(.secondary)
                            Button(role: .destructive) {
                                model.chargingConnectors.removeAll { $0.id == row.id }
                            } label: { Image(systemName: "minus.circle") }.buttonStyle(.borderless)
                        }
                    }
                    Button("Add connector") {
                        // Offer a type not already used: Apple's per-connector power sub is
                        // single-valued, so a duplicate row cannot be represented on the wire and
                        // the adapter would drop it.
                        let used = Set(model.chargingConnectors.map(\.type))
                        let next = VehicleConfigModel.connectorNames.first { !used.contains($0) }
                        if let next { model.chargingConnectors.append(.init(type: next, powerWatts: nil)) }
                    }
                    .disabled(model.chargingConnectors.count >= VehicleConfigModel.connectorNames.count)
                }
                InfoToggle(title: "Declare vehicle status", key: "vehicleStatusEnabled", isOn: $model.vehicleStatusEnabled)
                    .disabled(!VehicleConfigModel.vehicleStatusUnlocked)
                if model.vehicleStatusEnabled {
                    Text("⚠️ Not yet supported by the adapter — the messages that service this component are not declared, and iOS can reject the whole identification for the connection. Leave off until the adapter ships it.")
                        .font(.caption).foregroundStyle(.orange)
                    ForEach(VehicleConfigModel.vehicleStatusCapNames, id: \.self) { c in
                        Toggle(c, isOn: Binding(
                            get: { model.vehicleStatusCaps.contains(c) },
                            set: { on in
                                if on { model.vehicleStatusCaps.insert(c) } else { model.vehicleStatusCaps.remove(c) }
                            }))
                        // Apple: the unified range warning and the per-engine ones are mutually
                        // exclusive, so show which selections are being ignored rather than
                        // silently dropping them at emission.
                        .foregroundStyle(
                            model.vehicleStatusCaps.contains("rangeWarning")
                                && VehicleConfigModel.perEngineRangeWarnings.contains(c)
                                ? AnyShapeStyle(.secondary) : AnyShapeStyle(.primary))
                    }
                    if model.vehicleStatusCaps.contains("rangeWarning")
                        && !model.vehicleStatusCaps.isDisjoint(with: VehicleConfigModel.perEngineRangeWarnings) {
                        Text("Apple's spec forbids combining the unified range warning with the per-engine ones — the greyed entries will not be sent.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
            }

            Section("Appearance") {
                InfoToggle(title: "Right-hand drive", key: "rightHandDrive", isOn: $model.rightHandDrive)
                InfoToggle(title: "Night mode", key: "nightMode", isOn: $model.nightMode)
                InfoToggle(title: "UI appearance sync", key: "enablesUIAppearance", isOn: $model.enablesUIAppearance)
                InfoToggle(title: "Map appearance sync", key: "enablesMapAppearance", isOn: $model.enablesMapAppearance)
                InfoToggle(title: "Corner masks (cutout)", key: "enablesCornerMasks", isOn: $model.enablesCornerMasks)
            }

            LiveAppearanceSection(bridge: ControlsBridge.shared)

            Section {
                InfoToggle(title: "Declare limited UI elements", key: "limitedUIConfig",
                           isOn: $model.limitedUIConfigEnabled)
                if model.limitedUIConfigEnabled {
                    Toggle("On-screen keyboard", isOn: $model.limitedUISoftKeyboard)
                    Toggle("Phone keypad", isOn: $model.limitedUISoftPhoneKeypad)
                    Toggle("Music lists", isOn: $model.limitedUIMusicLists)
                    Toggle("Non-music lists", isOn: $model.limitedUINonMusicLists)
                    Toggle("Japan maps", isOn: $model.limitedUIJapanMaps)
                    Toggle("Long alerts", isOn: $model.limitedUILongAlerts)
                    // NOT capability toggles: real Apple LimitedUIConfig keys that airPlayElements
                    // never emits — kept only so exported YAML round-trips the full Apple schema.
                    Text("The four below are parsed for YAML round-trip only — Apple never emits them, so they NEVER appear in /info limitedUIElements.")
                        .font(.caption).foregroundStyle(.orange)
                    Toggle("Paired devices (round-trip only)", isOn: $model.limitedUIPairedDevices)
                    Toggle("Theme customization (round-trip only)", isOn: $model.limitedUIThemeCustomization)
                    Toggle("Automaker settings (round-trip only)", isOn: $model.limitedUIAutomakerSettings)
                    Toggle("Automaker settings info button (round-trip only)", isOn: $model.limitedUIAutomakerSettingsInfoButton)
                }
            } header: {
                Text("Limited UI Elements")
            } footer: {
                Text("Which UI elements iOS restricts while limited-UI (Drive) mode is on — the runtime on/off lives in Window ▸ Controls ▸ UI. Disabled: nothing is declared and iOS keeps its own default restriction set.")
                    .font(.caption)
            }

            Section {
                Toggle("Show OEM icon on home screen", isOn: $model.oemIconEnabled)
                if model.oemIconEnabled {
                    HStack(spacing: 12) {
                        if !model.oemIconBase64.isEmpty,
                           let data = Data(base64Encoded: model.oemIconBase64),
                           let img = NSImage(data: data) {
                            Image(nsImage: img).resizable().aspectRatio(contentMode: .fit)
                                .frame(width: 44, height: 44).cornerRadius(6)
                            Text("\(model.oemIconW)×\(model.oemIconH) PNG").font(.caption).foregroundStyle(.secondary)
                        } else {
                            Text("No image chosen").font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Button("Choose PNG…") { model.pickOemIcon() }
                        Button("Use Simulator icon") { model.useSimulatorOemIcon() }
                    }
                    TextField("Label (name shown on the home screen)", text: $model.oemIconLabel)
                    Toggle("Show icon in CarPlay", isOn: $model.oemIconVisible)
                    Text(model.oemIconVisible
                         ? "iOS shows the icon (oemIconVisible: true)."
                         : "iOS hides the icon (oemIconVisible: false is sent). Use this to hide it — turning off \"Show OEM icon\" only stops sending the config, which leaves the last icon on screen.")
                        .font(.caption2).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
            } header: {
                Text("OEM Icon")
            } footer: {
                Text("The vehicle-maker logo on the CarPlay home screen (Apple oemIconConfig → /info oemIcons/oemIconLabel/oemIconVisible). PNG, square (Apple ships 120/180/256). Static config — takes effect on the next connect. \"Show OEM icon\" advertises the config; \"Show icon in CarPlay\" is the oemIconVisible flag. To HIDE an icon iOS already cached, keep \"Show OEM icon\" ON and turn \"Show icon in CarPlay\" OFF (sends visible:false) — turning the config off entirely just stops sending it, leaving the last icon on screen.")
                    .font(.caption)
            }

            Section("Advanced Capabilities") {
                InfoToggle(title: "View areas", key: "enablesViewAreas", isOn: $model.enablesViewAreas)
                InfoToggle(title: "Focus transfer", key: "enablesFocusTransfer", isOn: $model.enablesFocusTransfer)
                InfoToggle(title: "UI context handoff", key: "enablesUIContext", isOn: $model.enablesUIContext)
                InfoToggle(title: "UI sync", key: "enablesUISync", isOn: $model.enablesUISync)
                InfoToggle(title: "File transfer", key: "enablesFileTransfer", isOn: $model.enablesFileTransfer)
                InfoToggle(title: "Log transfer", key: "enablesLogTransfer", isOn: $model.enablesLogTransfer)
                InfoToggle(title: "Vehicle data protocol", key: "enablesVehicleDataProtocol", isOn: $model.enablesVehicleDataProtocol)
                InfoToggle(title: "DCX", key: "enablesDCX", isOn: $model.enablesDCX)
                InfoToggle(title: "App-driven SETUP (default, both transports)", key: "appDrivenSetup", isOn: $model.appDrivenSetup)
                Picker(selection: $model.metadataTier) {
                    Text("Proven — device-accepted baseline").tag("proven")
                    Text("Extended — full paired Start/Stop set").tag("extended")
                    Text("All — every capability in the table").tag("all")
                } label: { InfoLabel(title: "Metadata declaration", key: "metadataTier") }
                HStack {
                    InfoLabel(title: "Skip features", key: "metadataSkip")
                    TextField("e.g. call_history", text: $model.metadataSkip)
                        .textFieldStyle(.roundedBorder)
                }
            }

            Section {
                DisclosureGroup("Generated YAML", isExpanded: $showYAML) {
                    Text(model.yaml)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                HStack {
                    Button("Export YAML…", systemImage: "square.and.arrow.up") { exportYAML() }
                    Spacer()
                    Button("Revert to Default", systemImage: "arrow.uturn.backward", role: .destructive) {
                        confirmReset = true
                    }
                }
            }
        }
        .formStyle(.grouped)
        .safeAreaInset(edge: .bottom) {
            // Save bar — makes it explicit what's committed and when it applies.
            HStack(spacing: 10) {
                if model.dirty {
                    Label("Unsaved changes", systemImage: "pencil.circle.fill").foregroundStyle(.orange)
                } else {
                    Label("Saved — applies on next adapter connection", systemImage: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                }
                Spacer()
                Button("Save") { model.save() }
                    .keyboardShortcut("s", modifiers: .command)
                    .buttonStyle(.borderedProminent)
                    .disabled(!model.dirty)
            }
            .font(.callout)
            .padding(12)
            .background(.bar)
        }
        .confirmationDialog("Revert all configuration to the default?", isPresented: $confirmReset) {
            Button("Revert to Default", role: .destructive) { model.resetToDefault() }
            Button("Cancel", role: .cancel) {}
        }
    }

    @State private var confirmReset = false

    private func exportYAML() {
        let panel = NSSavePanel()
        panel.title = "Export VehicleConfig YAML"
        panel.nameFieldStringValue = "carlink_vehicleconfig.yaml"
        panel.allowedContentTypes = [.yaml]
        panel.canCreateDirectories = true
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            // Export the LIVE yaml — the same document the "Generated YAML" preview shows. (The
            // committed snapshot is what's pushed to the box, but exporting something other than
            // what's on screen was silently dishonest.)
            do {
                try Data(model.yaml.utf8).write(to: url)
            } catch {
                let alert = NSAlert()
                alert.messageText = "Export Failed"
                alert.informativeText = error.localizedDescription
                alert.alertStyle = .warning
                alert.runModal()
            }
        }
    }
}

// MARK: - CCPA management tab

/// Live bridge between the CCPA tab and the OCBM client (set by AppDelegate on connect, like
/// `ControlsBridge`). Holds the latest adapter snapshot + drives the box control actions over CH_MGMT.
@MainActor
final class CCPABridge: ObservableObject {
    static let shared = CCPABridge()
    weak var client: OCBMClient?
    @Published var info: CCPAInfo?
    @Published var lastUpdated: Date?
    @Published var statusText: String = "Not connected"
    @Published var busy = false
    /// True when the snapshot predates the current adapter session (set by `sessionEnded()`) — the
    /// data shown is from BEFORE the unplug/teardown. Cleared by a fresh query / successful receiveInfo.
    @Published var stale = false

    private var busyGen = 0

    /// Session teardown (AppDelegate.endSession): the OCBM link is gone, so any latched busy will
    /// never be ACKed and "Connected" is a lie. Keep the last snapshot visible but mark it stale.
    func sessionEnded() {
        clearBusy()
        statusText = "Disconnected"
        if info != nil { stale = true }
    }

    /// (Re)query the adapter snapshot. Also clears a stuck `busy` (an action whose ACK never arrived —
    /// e.g. reboot dropped the link), so Refresh always recovers the UI. No-op if not connected.
    ///
    /// The box answers CH_MGMT while idle (ocbmd's handle_mgmt has no session gate), which is why this
    /// tab works pre-projection — so no `subscribed` gate is wanted here, only the reply deadline below.
    func refresh() {
        clearBusy()
        guard let client else { info = nil; statusText = "Adapter not connected"; return }
        statusText = "Querying adapter…"
        stale = false
        client.requestBoxInfo()
        // Deadline: without it a box that never replies left "Querying adapter…" latched forever.
        armTimeout()
    }
    func receiveInfo(_ info: CCPAInfo?) {
        clearBusy()
        if let info {
            self.info = info
            lastUpdated = Date()
            statusText = "Connected"
            stale = false
        } else {
            statusText = "Failed to read adapter info"
        }
    }
    func receiveAck(verb: UInt8, status: UInt8) {
        clearBusy()
        if status != 0 { statusText = "Action failed" }
        // A reboot drops the OCBM link; anything else, re-query to reflect the new state.
        if verb != OCBM.mgmtReboot { client?.requestBoxInfo() }
    }
    func restartWireless() { setBusy(); client?.boxRestartWireless() }
    func forgetAll() { setBusy(); client?.boxForgetAll() }
    func forgetDevice(_ mac: String) { setBusy(); client?.boxForgetDevice(mac) }
    func reboot() { setBusy("Rebooting adapter…"); client?.boxReboot() }

    /// Enter the busy state with a self-healing timeout: if no ACK/info clears it within 6 s (a lost ACK,
    /// or an action that tore the link down before replying), reset so the form isn't stuck disabled.
    private func setBusy(_ status: String? = nil) {
        busy = true
        if let status { statusText = status }
        armTimeout()
    }
    /// Arm the shared 6 s self-heal deadline (generation-guarded — a newer action, receiveInfo's
    /// clearBusy or a re-arm voids it). Shared by setBusy() and refresh() so a query with no reply
    /// resolves to "No response from adapter" instead of latching its "…" status forever.
    private func armTimeout() {
        busyGen += 1
        let gen = busyGen
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 6_000_000_000)
            guard let self, self.busyGen == gen else { return } // a newer action / a completion voided it
            self.busy = false
            if self.statusText.hasSuffix("…") { self.statusText = "No response from adapter" }
        }
    }
    /// Clear busy and void any pending timeout (bump the generation so a stale timer no-ops).
    private func clearBusy() {
        busy = false
        busyGen += 1
    }

    /// "1d 03:14:05" style uptime from seconds.
    static func uptime(_ s: Int) -> String {
        let d = s / 86400, h = (s % 86400) / 3600, m = (s % 3600) / 60, sec = s % 60
        return d > 0 ? String(format: "%dd %02d:%02d:%02d", d, h, m, sec)
                     : String(format: "%02d:%02d:%02d", h, m, sec)
    }
}

/// A green/red status dot + label for a health row.
private struct HealthRow: View {
    let title: String
    let ok: Bool
    var detail: String = ""
    var body: some View {
        LabeledContent(title) {
            HStack(spacing: 6) {
                Circle().fill(ok ? Color.green : Color.red).frame(width: 8, height: 8)
                Text(detail.isEmpty ? (ok ? "up" : "down") : detail).foregroundStyle(.secondary)
            }
        }
    }
}

struct CCPATab: View {
    @ObservedObject var store = CCPABridge.shared
    @State private var confirmReboot = false
    @State private var confirmForgetAll = false
    @State private var forgetMac: String?

    var body: some View {
        Form {
            if let i = store.info {
                Section("Identity") {
                    LabeledContent("Name", value: i.name)
                    LabeledContent("Bluetooth MAC", value: i.bt_mac)
                    LabeledContent("Wi-Fi MAC", value: i.wifi_mac)
                    LabeledContent("Serial", value: i.serial)
                }
                Section("Health") {
                    LabeledContent("Uptime", value: CCPABridge.uptime(i.uptime_s))
                    LabeledContent("Storage", value: "\(i.rootfs_pct)% used · \(i.rootfs_free_kb / 1024) MB free")
                    HealthRow(title: "Bluetooth", ok: i.hci_up && i.ssp,
                              detail: i.hci_up ? (i.ssp ? "up · SSP on" : "up · SSP OFF") : "down")
                    HealthRow(title: "Wi-Fi AP", ok: i.wlan_ap)
                    LabeledContent("Transport", value: i.transport.isEmpty ? "idle" : i.transport)
                    // Phone presence is a state, not a fault — show it plainly (like Transport), not a red dot.
                    LabeledContent("Phone", value: i.phone_present ? "connected" : "none")
                    // Daemon health = the always-on core (ocbmd). airplayd/iap2d are on-demand (session-only),
                    // so requiring them would falsely read as a fault at idle; the detail lists what's running.
                    HealthRow(title: "Daemons", ok: i.daemons.ocbmd, detail: daemonSummary(i.daemons))
                }
                Section("Known Devices") {
                    if i.devices.isEmpty {
                        Text("No paired devices").foregroundStyle(.secondary)
                    } else {
                        ForEach(i.devices, id: \.self) { mac in
                            HStack {
                                Text(mac).font(.system(.body, design: .monospaced))
                                Spacer()
                                Button("Forget") { forgetMac = mac }
                                    .buttonStyle(.borderless).foregroundStyle(.red)
                            }
                        }
                    }
                }
            }
            // Live receive-side A/V stream health (measured on the Mac). Independent of the box mgmt
            // snapshot above — it reads the OCBM decrypt layer's per-stream counters at ~1 Hz.
            StreamPerfSection()
            Section {
                Button { store.restartWireless() } label: {
                    Label("Restart wireless stack", systemImage: "wifi")
                }
                Button(role: .destructive) { confirmForgetAll = true } label: {
                    Label("Forget all paired devices", systemImage: "trash")
                }
                Button(role: .destructive) { confirmReboot = true } label: {
                    Label("Restart adapter", systemImage: "arrow.clockwise.circle")
                }
            } header: {
                Text("Controls")
            } footer: {
                Text("Restart adapter reboots the CCPA — the reliable recovery if Bluetooth wedges. It interrupts any live CarPlay session.")
                    .font(.caption)
            }
        }
        .formStyle(.grouped)
        .disabled(store.busy)
        .safeAreaInset(edge: .bottom) {
            HStack(spacing: 10) {
                if store.busy { ProgressView().controlSize(.small) }
                Text(statusLine).font(.callout).foregroundStyle(.secondary)
                Spacer()
                Button("Refresh", systemImage: "arrow.clockwise") { store.refresh() }
            }
            .padding(12).background(.bar)
        }
        .onAppear { store.refresh() }
        .confirmationDialog("Restart the adapter now?", isPresented: $confirmReboot) {
            Button("Restart Adapter", role: .destructive) { store.reboot() }
            Button("Cancel", role: .cancel) {}
        } message: { Text("The CCPA will reboot and any live CarPlay session will drop.") }
        .confirmationDialog("Forget all paired devices?", isPresented: $confirmForgetAll) {
            Button("Forget All", role: .destructive) { store.forgetAll() }
            Button("Cancel", role: .cancel) {}
        } message: { Text("Every phone will need to pair again.") }
        .confirmationDialog("Forget this device?", isPresented: Binding(
            get: { forgetMac != nil }, set: { if !$0 { forgetMac = nil } }
        )) {
            Button("Forget", role: .destructive) { if let m = forgetMac { store.forgetDevice(m) }; forgetMac = nil }
            Button("Cancel", role: .cancel) { forgetMac = nil }
        } message: { Text(forgetMac.map { "\($0) will need to pair again." } ?? "") }
    }

    private var statusLine: String {
        if let d = store.lastUpdated, store.info != nil {
            let ago = max(0, Int(Date().timeIntervalSince(d)))
            // `stale` = the snapshot predates the current session (set on teardown) — say so rather
            // than let an old capture read as live data.
            let staleMark = store.stale ? " · stale (from previous session)" : ""
            return "\(store.statusText) · updated \(ago)s ago\(staleMark)"
        }
        return store.statusText
    }

    private func daemonSummary(_ d: CCPAInfo.Daemons) -> String {
        var up: [String] = []
        if d.ocbmd { up.append("ocbmd") }
        if d.iap2d { up.append("iap2d") }
        if d.airplayd { up.append("airplayd") }
        if d.carplay_wireless { up.append("wireless") }
        return up.isEmpty ? "none" : up.joined(separator: ", ")
    }
}

struct SettingsRootView: View {
    var body: some View {
        TabView {
            ConfigurationTab()
                .tabItem { Label("Configuration", systemImage: "slider.horizontal.3") }
            CCPATab()
                .tabItem { Label("CCPA", systemImage: "cpu") }
        }
        .frame(width: 460, height: 620)
    }
}

// MARK: - AppKit host

final class SettingsWindowController: NSWindowController {
    static let shared = SettingsWindowController()

    private convenience init() {
        let win = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 460, height: 620),
            styleMask: [.titled, .closable, .miniaturizable],
            backing: .buffered, defer: false)
        win.title = "Settings"
        win.isReleasedWhenClosed = false
        win.contentView = NSHostingView(rootView: SettingsRootView())
        self.init(window: win)
    }

    func show() {
        window?.center()
        window?.makeKeyAndOrderFront(nil)
    }
}
