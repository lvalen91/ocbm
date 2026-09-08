// VehicleProfile.swift — the protocol-NEUTRAL vehicle + adapter profile, and the on-disk document
// that carries it.
//
// WHY THIS EXISTS (owner directive, 2026-09-04). Settings grew CarPlay-first: `VehicleConfigModel`
// (App/SettingsWindow.swift) is ~80 `@Published` fields shaped exactly like Apple's CarPlay Simulator
// VehicleConfig YAML, and Android Auto reached into that CarPlay-shaped model for six values
// (`AACapability.init(config:)`) while everything else it needed was 15 environment variables and
// hardcoded statics. The owner's insight is that most of those settings are ONE feature with TWO
// vocabularies: night mode is `setNightMode` to Apple and `[sensors] night_mode` to Google; the
// driver's seat is a Bool nobody on the box consumes to Apple and `driverposition left|right|center`
// to Google; branding is a PNG + label to Apple and `display_name` + a generic Exit to Google. The
// app must hold the FACT once, in words that belong to neither vendor, and apply the correct
// per-vendor rendering.
//
// Neither vendor's authoring format is a wire format. Apple's YAML is the CarPlay Simulator's file
// (on the wire it is the AirPlay /info plist + iAP2 Identify parameters) and Google's .ini is the
// Desktop Head Unit's (on the wire it is protobuf `gal.ServiceDiscoveryResponse`). This project
// inherited Apple's YAML only because the BOX parses it (`crates/vendor/receiver/src/vehicle_config.rs`),
// and the emitted document is already a hybrid — six top-level keys are Apple's, seven (`wireless`,
// `hot_handover`, `pairing`, `android_auto`, `audio`, `metadata`, `iapConfig`) are this project's.
// So (owner decision 2026-09-04): THIS profile is the SOURCE; the Apple-schema YAML is a RENDERED
// ARTIFACT of it, and `AACapability` is the second renderer. The box wire format does not change:
// the YAML renderer must keep producing byte-identical output for an unchanged profile
// (`tools/check_app_yaml_fixture.py` is a FATAL test), and this file must never learn either
// vendor's vocabulary — that is the renderers' job and `FeatureMatrix.swift` documents it.
//
// Constraints this file honours:
//   • Pure value types, `Codable` + `Sendable` + `Equatable`, Foundation only — no SwiftUI, no AppKit,
//     no IOKit — so it compiles into the headless harness (`tests/run_tests.sh`) and can cross to the
//     AA session thread by value. `VehicleConfigModel` (main actor, UserDefaults-backed) MATERIALIZES
//     one of these; the observable model is not replaced by this file.
//   • `VehicleProfile.default` / `AdapterSettings.default` reproduce today's shipped defaults EXACTLY
//     (the `VehicleConfigModel.init` literals as of 2026-09-04), so rendering the default profile
//     yields the fixture YAML unchanged.
//   • Enum raw values are the persistence spelling. They are chosen to be vendor-neutral where the
//     vendors disagree and to coincide with a vendor token only where both would spell it the same
//     (e.g. `chademo`), so that the JSON document reads as a description of a CAR, not of a protocol.

import Foundation

// MARK: - Vocabulary (vendor-neutral)

/// Which side of the car the driver sits on. A THREE-way fact — Google's DHU accepts
/// `driverposition = left|right|center` and gal `DriverPosition` has a CENTER value (3) — that the
/// old model flattened to `rightHandDrive: Bool`. Apple has no consumer for it on the box today;
/// Android Auto declares it in the service-discovery response (device-verified 2026-09-04: it decides
/// which edge gearhead puts its app rail on).
enum DriverPosition: String, Codable, Sendable, CaseIterable, Hashable {
    case left, right, center

    /// The legacy `vc.rightHandDrive` reading. Centre counts as not-right.
    var rightHandDrive: Bool { self == .right }

    /// LOAD-time reconciliation with the legacy `vc.rightHandDrive` Bool (finding 5, 2026-09-04).
    /// This build writes the pair in step (`apply`, the picker, `save()`), so on this build they
    /// can never disagree. They CAN disagree after a downgrade: an older build knows only the Bool,
    /// the user flips it there, and the one-shot `vc.profileKeysV1` migration has already run so it
    /// will not re-derive. A disagreement therefore means "the older build wrote last" and the
    /// legacy value is the user's most recent intent — take it. Agreement (including
    /// `center`/`false`, which is consistent) keeps the richer neutral value. Idempotent, needs no
    /// sentinel, and does nothing on a store this build wrote.
    static func reconciled(stored: DriverPosition, legacyRightHandDrive: Bool) -> DriverPosition {
        stored.rightHandDrive == legacyRightHandDrive ? stored : (legacyRightHandDrive ? .right : .left)
    }
}

/// Day/night appearance the head unit asks the phone for. `auto` = follow this Mac's own system
/// appearance (the head unit IS a macOS app, so "auto" has a real, local source — the effective
/// appearance — rather than a clock or a light sensor the box does not have). CarPlay renders it as
/// `setNightMode` (live) plus the UI/map appearance updates; Android Auto renders it as the
/// `night_mode` sensor, the ONLY appearance lever gearhead offers (DHU's `uitheme` is the DHU
/// window's own chrome, not a wire field). The old model's `nightMode: Bool` maps false→`light`,
/// true→`dark`; nobody had `auto`, so it is a new capability, not a changed default.
enum AppearanceTheme: String, Codable, Sendable, CaseIterable, Hashable {
    case auto, light, dark

    /// The legacy `vc.nightMode` reading. `auto` is resolved by the caller that knows the Mac's
    /// effective appearance; here it reads as not-night.
    var nightModeLegacy: Bool { self == .dark }

    /// LOAD-time reconciliation with the legacy `vc.nightMode` Bool — same contract as
    /// `DriverPosition.reconciled`: a disagreement can only come from an older build's write, so
    /// the legacy value wins; agreement keeps the neutral value (`auto`/`false` is consistent).
    static func reconciled(stored: AppearanceTheme, legacyNightMode: Bool) -> AppearanceTheme {
        stored.nightModeLegacy == legacyNightMode ? stored : (legacyNightMode ? .dark : .light)
    }
}

/// The head unit's primary control surface. Apple's `primaryInput` vocabulary is `Touchpad | Knobs`
/// only — every touchscreen head unit in the CarPlay Simulator bundle (9/10 configs) says `Touchpad`,
/// so `touchscreen` and `touchpad` BOTH render as Apple's `Touchpad`; `rotary` renders as `Knobs`.
/// Google's DHU has `inputmode = touch|rotary|controller|hybrid|dpad-1d|dpad-2d|none` plus separate
/// `touch`/`touchpad`/`controller` booleans; the AA renderer derives those from this plus
/// `InputDevices`. Which physical surfaces EXIST is `InputDevices`; this is only which one is primary.
enum PrimaryInput: String, Codable, Sendable, CaseIterable, Hashable {
    case touchscreen, touchpad, rotary
}

/// Powertrain. Raw values are Apple's four `EngineType` names because Google's `fueltypes`
/// (`unknown|leaded|unleaded|biodiesel|electric|other`) is the coarser vocabulary and maps onto these
/// losslessly in one direction (gasoline→unleaded, diesel→biodiesel is the nearest, cng→other);
/// the reverse would invent facts. Multi-select: a hybrid is genuinely two entries.
enum EngineType: String, Codable, Sendable, CaseIterable, Hashable {
    case gasoline, diesel, electric, cng
}

/// Charging connectors. Raw values are Apple's nine `SupportedChargingConnectors` tokens (the
/// finer vocabulary); Google's `evconnectors` (`j1772|chademo|combo-1|combo-2|supercharger`) maps
/// ccs1→combo-1, ccs2→combo-2, nacs*→supercharger, and has no word for mennekes/GB-T (dropped).
enum ChargingConnector: String, Codable, Sendable, CaseIterable, Hashable {
    case ccs1, ccs2, j1772, chademo, mennekes
    case gbtDC = "gbt_dc", gbtAC = "gbt_ac", nacsDC = "nacs_dc", nacsAC = "nacs_ac"
}

/// What the head unit asks the phone to WITHHOLD while the car is moving — the union of Apple's
/// `limitedUIConfig` elements and Google's `driving_status` bits, named for the thing withheld.
///
/// Both vendors express this as a SET, and the old model kept two disconnected copies: six Apple
/// booleans in the YAML, and a hardcoded `AACapability.DrivingRestrictions.drivingDefault` for AA.
/// This is the one set both renderers derive from. Members that only one vendor can express are
/// still here (the fact is about the car; a renderer that cannot express it ignores it, and
/// `FeatureMatrix` says so). The vendor derivation is data in `FeatureMatrix.restrictionMapping`.
///
/// This is a CAPABILITY DECLARATION (what to withhold WHEN restricted), not a claim that the car is
/// moving. Whether restriction is currently ON is a runtime control (Controls window ▸ Limited UI /
/// AA `setDrivingRestricted`), and the bench proved conflating the two restricts a stationary car
/// (AACapability.swift, 2026-08-27).
struct DrivingRestrictionSet: OptionSet, Codable, Sendable, Hashable {
    let rawValue: UInt32
    init(rawValue: UInt32) { self.rawValue = rawValue }

    /// On-screen keyboard. Apple `softKeyboard`; Google NO_KEYBOARD_INPUT.
    static let keyboard       = DrivingRestrictionSet(rawValue: 1 << 0)
    /// Phone dial pad. Apple `softPhoneKeypad`; Google folds it into NO_KEYBOARD_INPUT.
    static let phoneKeypad    = DrivingRestrictionSet(rawValue: 1 << 1)
    /// Long music/media lists. Apple `musicLists`; Google has no bit.
    static let mediaLists     = DrivingRestrictionSet(rawValue: 1 << 2)
    /// Other long lists. Apple `nonMusicLists`; Google has no bit.
    static let otherLists     = DrivingRestrictionSet(rawValue: 1 << 3)
    /// Long alerts / message bodies. Apple `longAlerts` (wire `longUserAlert`); Google LIMIT_MESSAGE_LEN.
    static let longMessages   = DrivingRestrictionSet(rawValue: 1 << 4)
    /// Blank the projected video. Google NO_VIDEO; Apple has no element (it would blank CarPlay).
    static let video          = DrivingRestrictionSet(rawValue: 1 << 5)
    /// Voice input / assistant. Google NO_VOICE_INPUT; Apple has no element.
    static let voiceInput     = DrivingRestrictionSet(rawValue: 1 << 6)
    /// Free-form settings/configuration screens. Google NO_CONFIG; Apple has no element.
    static let configuration  = DrivingRestrictionSet(rawValue: 1 << 7)

    /// What `AACapability.DrivingRestrictions.drivingDefault` meant, in neutral words: withhold the
    /// input surfaces a driver should not be using and keep the picture and voice. Offered as a
    /// preset; the shipped default declares NOTHING (see `DrivingRestrictionPolicy`).
    static let typicalDriving: DrivingRestrictionSet = [.keyboard, .phoneKeypad, .longMessages, .configuration]
}

/// Whether to DECLARE a restriction set at all, and which. `declared == false` reproduces today's
/// shipped behaviour on both protocols: CarPlay emits no `limitedUIConfig` (iOS applies its own
/// default set — the proven, fixture-locked behaviour) and Android Auto keeps `drivingDefault` as
/// the mask it sends when restriction is switched on. Once `declared` is true, BOTH renderers derive
/// from `set` and the AA hardcode retires.
struct DrivingRestrictionPolicy: Codable, Sendable, Equatable {
    var declared: Bool = false
    var set: DrivingRestrictionSet = []
}

// MARK: - Display

/// Pixel insets from each panel edge that projected UI must stay inside. The video still fills the
/// whole panel; only interactive UI is held within the box (curved/occluded corners). Apple:
/// `viewAreas.safeArea` (arms `viewAreas` automatically when non-zero and not full-coverage —
/// `vehicle_config.rs:874`). Google: `contentinsets` / `stablecontentinsets` in the DHU vocabulary.
/// NOT the same thing as Android Auto's `margins`, which are codec pixels CROPPED AWAY to fit a
/// non-tier panel; those are derived by the AA renderer from `PanelGeometry`, never authored here.
struct PanelInsets: Codable, Sendable, Equatable {
    var left: Int = 0
    var top: Int = 0
    var right: Int = 0
    var bottom: Int = 0

    static let zero = PanelInsets()
    var isZero: Bool { left == 0 && top == 0 && right == 0 && bottom == 0 }
}

/// The physical panel a display is projected onto.
///
/// `width`/`height`: the exact pixel grid. Apple takes any size (`pixelDimensions`); Google takes one
/// of nine `resolution` tiers, so the AA renderer picks the nearest tier and crops margins — the
/// profile records the TRUE panel and the renderer reports what it had to do (`FeatureMatrix`
/// `.panelGeometry`, Android Auto: limited).
/// `maxFPS`: 30 or 60 — the only two values either vendor accepts (24 was dropped 2026-07-12).
/// `dpi`: pixel density gearhead scales its UI by (160 = mdpi, the DHU default; unclamped on the
/// phone, `AACapability` accepts 80–640). iOS infers density and has no field.
/// `diagonalInches`: optional physical size; when set, `impliedDPI` offers a computed density so
/// the owner can enter what is printed on the panel instead of guessing a dpi.
struct PanelGeometry: Codable, Sendable, Equatable {
    var width: Int = 1920
    var height: Int = 1080
    var maxFPS: Int = 60
    var dpi: Int = 160
    var diagonalInches: Double? = nil

    var isPortrait: Bool { height > width }
    var aspect: Double { height > 0 ? Double(width) / Double(height) : 0 }

    /// Density implied by the pixel grid and the physical diagonal, or nil when no diagonal is set
    /// OR when the pair cannot produce a sane density.
    ///
    /// EVERY step here is guarded, because this computes on UNTRUSTED input: an imported
    /// `.vehicleprofile.json` is hand-editable and `apply(_:)` writes `diagonalInches` straight to
    /// the model — `clampInPlace()` covers width/height/fps/insets and deliberately not this. The
    /// naive form (`Int((pixels / d).rounded())`) TRAPS and takes the whole app down:
    ///   * `diagonalInches: 1e-300` passes `d > 0`, so the quotient overflows `Int` and
    ///     `Int(_:)` hits "Double value cannot be converted to Int because the result would be
    ///     greater than Int.max" (reproduced 2026-09-04, exit 133).
    ///   * `width * width` in `Int` arithmetic overflows and traps for width ≥ ~3.04e9, which a
    ///     document can carry before any clamp runs.
    /// So: square in `Double`, and return nil rather than a number whenever the result is not a
    /// density a real panel could have. Returning nil is correct, not a cop-out — the field is an
    /// OFFER to compute dpi from a printed diagonal, and there is no honest answer for 1e-300 in.
    var impliedDPI: Int? {
        guard let d = diagonalInches, d > 0, d.isFinite else { return nil }
        let w = Double(width), h = Double(height)
        let pixels = (w * w + h * h).squareRoot()
        let dpi = (pixels / d).rounded()
        // 20–2000 dpi spans every panel that exists, with room either side; anything outside it is a
        // typo or a hostile document, not a screen.
        guard dpi.isFinite, dpi >= 20, dpi <= 2000 else { return nil }
        return Int(dpi)
    }
}

/// The main projected display: geometry + the inset box UI stays inside. `drawUIOutsideInsets`
/// is Apple's `drawUIOutsideSafeArea` (allow non-interactive chrome in the inset band); Google has
/// no equivalent (its content insets are advisory to the phone's layout only).
struct MainDisplay: Codable, Sendable, Equatable {
    var panel = PanelGeometry()
    var insets = PanelInsets.zero
    var drawUIOutsideInsets: Bool = false
}

/// An optional second projected display (instrument cluster / secondary panel). Apple:
/// `altVideoStreams[]` + `initialURL`; Google: `displaytype cluster|auxiliary` + `instrumentcluster`
/// — supported by the protocol, NOT implemented by this app for AA yet (docs/androidauto/
/// 01_SESSION_AND_AV.md Phase 4, task T5). Shipped default: disabled, 800×480@30.
struct AltDisplay: Codable, Sendable, Equatable {
    var enabled: Bool = false
    var panel = PanelGeometry(width: 800, height: 480, maxFPS: 30, dpi: 160)
    var insets = PanelInsets.zero
    var drawUIOutsideInsets: Bool = false
}

/// Video codec policy. `hevcAllowed` is Apple's `enablesHEVC` (default on; the box arms HEVC from
/// it). For Android Auto the tiers above 1080p are H.265-ONLY on gearhead 17.5 (device-measured
/// 2026-09-04: 2560x1440 declared as H.264 → "No working configuration"), so the AA renderer must
/// either declare HEVC for those tiers or refuse to go above 1080p when this is false. Before this
/// profile existed AA ignored the toggle entirely (defect 5).
struct VideoCodecPolicy: Codable, Sendable, Equatable {
    var hevcAllowed: Bool = true
}

/// Status-bar elements the head unit asks the phone to hide because the car draws its own. Google's
/// DHU vocabulary is `hideclock` / `hidesignal` / `hidebattery`; iOS owns the CarPlay status bar and
/// has no such request. All false = today's behaviour (nothing hidden, nothing sent).
struct StatusBarPolicy: Codable, Sendable, Equatable {
    var hideClock: Bool = false
    var hideSignal: Bool = false
    var hideBattery: Bool = false

    var isDefault: Bool { !hideClock && !hideSignal && !hideBattery }
}

struct Appearance: Codable, Sendable, Equatable {
    var theme: AppearanceTheme = .light
    var statusBar = StatusBarPolicy()
}

// MARK: - Identity and branding

/// What the phone calls this head unit. `headUnitName` is the ONE name both protocols show: CarPlay
/// carries it as the YAML `name:` (which the box does not push to the phone — the accessory name
/// iOS shows is derived on-box from MAC + serial) and Android Auto advertises it as
/// `headunit_info.make/model` and `display_name` (AAWire.serviceDiscoveryResponseFull). It was in
/// `FieldInfo.inertKeys` as "config metadata only", which was true for CarPlay and false for AA
/// (defect 3).
struct Identity: Codable, Sendable, Equatable {
    var headUnitName: String = "CarLink Widescreen"
}

/// A small PNG the vehicle maker's logo is rendered from. CarPlay-ONLY at render time (Apple's
/// `oemIconConfig.images[]`, resampled to 120/180/256 by the YAML renderer); Android Auto shows
/// `label` (via the head-unit name) and a generic Exit glyph and has no custom-icon slot. The icon
/// stays IN the neutral model on purpose — it is a fact about the car's branding, and a renderer
/// that cannot show it simply does not.
struct BrandIcon: Codable, Sendable, Equatable {
    var pngBase64: String
    var width: Int
    var height: Int
}

/// Vehicle-maker branding on the projected home screen.
/// `advertise`: declare the branding block at all (Apple: emit `oemIconConfig`; default off, so the
///   fixture YAML is unchanged). `visible`: Apple's `oemIconVisible` — sending `visible: false` WITH
///   the icon present is the active hide signal iOS honours; omitting the block leaves the cached
///   icon on screen. `label`: Apple's `oemIconLabel`; default "CarLink".
struct Branding: Codable, Sendable, Equatable {
    var advertise: Bool = false
    var visible: Bool = true
    var label: String = "CarLink"
    var icon: BrandIcon? = nil
}

// MARK: - Vehicle facts

/// One charging connector with an optional power rating in watts (nil = unstated; an absent rating
/// must not become a zero one). Each type may appear ONCE — Apple carries one rating per type and
/// the box keeps only the first duplicate (`SettingsWindow.swift` ConnectorRow).
struct ConnectorSpec: Codable, Sendable, Equatable {
    var type: ChargingConnector
    var powerWatts: UInt32? = nil
}

/// Engine types + connectors. Apple: iAP2 Identify param 20 `engineTypes`/`chargingConnectors`
/// (live on the wired arm since C-3, 2026-09-02). Google: DHU `fueltypes` / `evconnectors`.
/// Was labelled CarPlay-only in the UI; it is not. Empty = the box's built-in default (gasoline).
struct Powertrain: Codable, Sendable, Equatable {
    /// Canonical order = `EngineType.allCases` order; keep sorted so the document is byte-stable.
    var engines: [EngineType] = []
    var connectors: [ConnectorSpec] = []

    var isElectrified: Bool { engines.contains(.electric) }
    /// Engines in canonical order with duplicates removed — what a renderer should iterate.
    var canonicalEngines: [EngineType] { EngineType.allCases.filter { engines.contains($0) } }
    /// Connectors with the first occurrence of each type kept, and none at all for a vehicle that
    /// is not electrified (both renderers agree an ICE car has no charging connectors).
    var effectiveConnectors: [ConnectorSpec] {
        guard isElectrified else { return [] }
        var seen = Set<ChargingConnector>()
        return connectors.filter { seen.insert($0.type).inserted }
    }
}

// MARK: - Input

struct Touchscreen: Codable, Sendable, Equatable {
    /// Two-finger contact. Apple `touchScreenSupportsMultiTouch` (default OFF — untested here,
    /// matches the box's serde default).
    var multiTouch: Bool = false
    /// Apple `touchScreenSupportsCancel`.
    var supportsCancel: Bool = true
}

struct Touchpad: Codable, Sendable, Equatable {
    /// Apple `touchpadButtonsSupport`; Google `touchpadtapasselect` is the nearest DHU flag.
    var buttons: Bool = false
}

struct RotaryKnob: Codable, Sendable, Equatable {
    /// Apple `knobSupportsHomeAndBackButton`.
    var homeAndBackButtons: Bool = false
    /// Apple `knobSupportsNudge` (4-way nudge). Google: D-pad events on the same controller.
    var nudge: Bool = false
}

/// Which physical control surfaces the head unit has. Each optional sub-struct being nil means
/// "not present". Apple: `hidConfig` (+ `primaryInput` sibling). Google: `touch` / `touchpad` /
/// `controller` flags + `inputmode`. Shipped default = a high-fidelity touchscreen with a D-pad and
/// media buttons and nothing else, which is exactly what the fixture YAML declares.
struct InputDevices: Codable, Sendable, Equatable {
    var primary: PrimaryInput = .touchscreen
    var touchscreen: Touchscreen? = Touchscreen()
    var touchpad: Touchpad? = nil
    var rotaryKnob: RotaryKnob? = nil
    /// Apple `dPadSupport` — LIVE on the box (`enablesDPad`); default on.
    var dPad: Bool = true
    /// Apple `mediaButtonsSupport`; Google media keycodes in `InputSourceService.keycodes_supported`.
    var mediaButtons: Bool = true
    /// Apple `telephonyButtonsSupport` (uid-5 HID entry, live); Google phone/callEnd keycodes.
    var telephonyButtons: Bool = false
    /// Apple `steeringWheelSupport` (DirectionButtons feature bit 0x20; parse-only on the box).
    var steeringWheelButtons: Bool = false
}

// MARK: - Audio and data feeds

/// Audio characteristics that are the car's, not a codec table's. `voiceRateHz`: sample rate of the
/// guidance/assistant streams (Android Auto guidance + system sinks; 48000 since 2026-09-04, when the
/// Pixel 10 accepted it and the owner confirmed the prompts sounded better; 16000/24000 are the
/// other values gearhead negotiates). `telephonyOverProjection`: offer a call-audio stream on the
/// projection link instead of Bluetooth HFP (AA: the `AA_TELEPHONY_SINK` experiment, default OFF
/// because an unrecognised sink costs the whole session). CarPlay's full `audio:` format table is a
/// CarPlay-exclusive sub-group (`CarPlayExtensions.audio*`).
struct AudioProfile: Codable, Sendable, Equatable {
    var voiceRateHz: Int = 48000
    var telephonyOverProjection: Bool = false
}

/// Which metadata feeds the head unit consumes from the phone. Google: the three service
/// descriptors (media_playback / navigation_status / phone_status, all accepted by gearhead 17.5 on
/// 2026-09-04; `AA_METADATA=0` withheld all three). Apple: the iAP2 declaration is governed by the
/// CarPlay-exclusive `metadataTier` picker; these three are advisory for CarPlay until the skip
/// list is mapped onto them (`FeatureMatrix` `.metadataFeeds`).
struct MetadataFeeds: Codable, Sendable, Equatable {
    var nowPlaying: Bool = true
    var navigation: Bool = true
    var telephony: Bool = true
}

// MARK: - Protocol-exclusive extensions

/// Settings ONLY CarPlay can express. They live INSIDE the neutral document (the document is the
/// source the YAML is rendered from, so it must carry them) but in their own block, so the vendor
/// vocabulary is quarantined. The UI shows each of these as a badged CarPlay sub-group under the
/// neutral feature it belongs to (`FeatureMatrix.exclusiveKeys`), never as a CarPlay tab.
/// Field names are Apple's own where they name an Apple key (`enablesHEVC` is neutral and lives in
/// `VideoCodecPolicy`; the rest of `accessoryConfig` is here). Defaults = `VehicleConfigModel.init`.
struct CarPlayExtensions: Codable, Sendable, Equatable {
    /// iAP2 Identify param 21 accessory name override; "" = keep the box's per-device
    /// `CarLink-<wifi-suffix>` (what ships today). Distinct from `Identity.headUnitName`, which the
    /// box deliberately does not map onto the advertised accessory name.
    var accessoryName: String = ""

    // `audio:` capability set — "auto" | "wired_pcm" | "wireless_8" | "custom" + the custom rows.
    var audioMode: String = "auto"
    var audioFormats: [AudioFormat] = AudioFormat.defaultCustomFormats

    /// iAP2 metadata declaration tier — "proven" | "extended" | "all" ("rx-only" is a refuted dead
    /// end and is NOT offered, docs/carplay/05_METADATA_AND_CONTROLS.md §6.2). Comma-separated skips.
    var metadataTier: String = "proven"
    var metadataSkip: String = ""

    /// Identify param 21 `VehicleStatusComponent`. Gated by `VehicleConfigModel.vehicleStatusUnlocked`
    /// (compile-time false until C-4 lands); carried here so a document round-trips.
    var vehicleStatusEnabled: Bool = false
    var vehicleStatusCaps: [String] = []

    var accessoryFlags = AccessoryFlags()

    /// The four real Apple `LimitedUIConfig` keys the box parses for YAML round-trip ONLY (Apple's
    /// `airPlayElements` never emits them), plus `japanMaps`, which has no neutral reading.
    var limitedUIJapanMaps: Bool = false
    var limitedUIPairedDevices: Bool = false
    var limitedUIThemeCustomization: Bool = false
    var limitedUIAutomakerSettings: Bool = false
    var limitedUIAutomakerSettingsInfoButton: Bool = false

    /// Apple `accessoryConfig.enables*` flags other than HEVC. Defaults are Apple's template values
    /// where Apple sets one (UI/Map appearance true in all 10 templates) and off otherwise.
    struct AccessoryFlags: Codable, Sendable, Equatable {
        var enablesMainBufferedAudio: Bool = false
        var enablesUIAppearance: Bool = true
        var enablesMapAppearance: Bool = true
        var enablesCornerMasks: Bool = false
        var enablesVideoPlayback: Bool = true
        var enablesViewAreas: Bool = false
        var enablesEnhancedSiri: Bool = false
        var enablesFocusTransfer: Bool = false
        var enablesUIContext: Bool = false
        var enablesUISync: Bool = false
        var enablesFileTransfer: Bool = false
        var enablesLogTransfer: Bool = false
        var enablesVehicleDataProtocol: Bool = false
        var enablesDCX: Bool = false
    }

    /// One `audio.formats[]` row — the neutral-document twin of `AudioFormatRow` (which carries a
    /// UUID for SwiftUI list identity and lives in the SwiftUI file). Codec strings are the box's
    /// `audio_format_bit` tokens.
    struct AudioFormat: Codable, Sendable, Equatable {
        var streamType: Int = 102
        var audioType: String = "media"
        var input: String = "none"
        var output: String = "aac_lc_48k_stereo"

        // ---- The advertised vocabulary, and the validator that guards it ------------------------
        //
        // These lists live HERE, in the Foundation-only profile, rather than on `VehicleConfigModel`
        // (AppKit/SwiftUI, `@MainActor`) for one reason: `tests/run_tests.sh` cannot compile the
        // model, so while the vocabulary and the validation sat there, the guard below could not be
        // tested at all. It guards a real defect found by audit on 2026-09-04, so leaving it
        // unguarded was not an option. `VehicleConfigModel` now aliases these.
        //
        // WHY THE GUARD EXISTS: the CarPlay emitter interpolates these three strings RAW into a YAML
        // flow mapping — `"  - {type: N, audioType: \(audioType), in: \(input), out: \(output)}"`.
        // That was safe only while the UI Pickers were the sole writers. Profile import made a
        // hand-edited document the first free-text path in, so `"output": "x}\n  bogus: [\""`
        // malformed the WHOLE pushed document and the box fell back to its built-in defaults for
        // resolution, HEVC, audio AND metadata — the same failure class as the B3 unescaped-quote
        // incident. Validate on the way in; never let an unknown spelling reach the emitter.

        /// The exact `in:`/`out:` tokens the box parses (mirrors `receiver::info::audio_format_bit`).
        static let codecs = [
            "none", "pcm_16k_mono", "pcm_48k_stereo",
            "aac_lc_44k_stereo", "aac_lc_48k_stereo",
            "aac_eld_48k_stereo", "aac_eld_44k_stereo",
            "aac_eld_16k_mono", "aac_eld_24k_mono", "aac_eld_32k_mono",
            "aac_eld_44k_mono", "aac_eld_48k_mono",
            "opus_16k_mono", "opus_24k_mono", "opus_48k_mono",
        ]
        /// `audioType` values iOS routes against (empty = the wired PCM catch-all — no audioType key).
        static let types = ["", "media", "default", "telephony", "speechRecognition", "alert", "compatibility"]
        /// Stream types the box arms: 100 MainAudio (bidir, carries mic), 101 AltAudio, 102 MainHighAudio.
        static let streamTypes = [100, 101, 102]

        /// This row with every unknown spelling replaced by the shipped default for that field.
        /// Total: the result is always emitter-safe regardless of what the document carried.
        func validated() -> AudioFormat {
            AudioFormat(
                streamType: Self.streamTypes.contains(streamType) ? streamType : 102,
                audioType: Self.types.contains(audioType) ? audioType : "media",
                input: Self.codecs.contains(input) ? input : "none",
                output: Self.codecs.contains(output) ? output : "aac_lc_48k_stereo")
        }

        /// The three device-proven entries `VehicleConfigModel.defaultCustomFormats` seeds.
        static let defaultCustomFormats: [AudioFormat] = [
            AudioFormat(streamType: 102, audioType: "media", input: "none", output: "aac_lc_48k_stereo"),
            AudioFormat(streamType: 100, audioType: "speechRecognition", input: "aac_eld_16k_mono", output: "aac_eld_16k_mono"),
            AudioFormat(streamType: 100, audioType: "compatibility", input: "pcm_16k_mono", output: "pcm_48k_stereo"),
        ]
    }
}

/// Settings ONLY Android Auto can express. Small on purpose: most of what the 15 `AA_*` environment
/// variables did is either a neutral fact now carried above (density, driver position, voice rate,
/// metadata feeds, telephony sink) or a bench-only lever that must stay an environment variable
/// (`AA_FORCE_RES`, `AA_FORCE_FPS`, `AA_PANEL`, `AA_PROTO`, `AA_LEGACY_VIDEO`, `AA_SKIP_AUDIO_ACK`,
/// `AA_NO_TOUCH`, `AA_READ_STALL_S` — they exist to exercise paths the owner's real profile cannot
/// reach, and a profile is a description of a car, not a test fixture).
struct AndroidAutoExtensions: Codable, Sendable, Equatable {
    /// Fit a non-tier panel by declaring the enclosing tier with cropped margins (T4, 2026-09-04)
    /// rather than the nearest smaller tier. `AA_MARGINS=0` was the bench spelling of `false`.
    var fitPanelWithMargins: Bool = true
    /// Declare H.265 even at ≤1080p (where H.264 is the proven path). `AA_HEVC=1` was the bench
    /// spelling. Ignored when `VideoCodecPolicy.hevcAllowed` is false.
    var preferHEVC: Bool = false
}

// MARK: - The vehicle profile

/// The neutral vehicle profile: every fact both projections render, plus the two quarantined
/// vendor-exclusive blocks. See the file header for why this is the source and YAML a rendering.
struct VehicleProfile: Codable, Sendable, Equatable {
    var identity = Identity()
    var branding = Branding()
    var display = MainDisplay()
    var altDisplay = AltDisplay()
    var video = VideoCodecPolicy()
    var appearance = Appearance()
    var driverPosition: DriverPosition = .left
    var restrictions = DrivingRestrictionPolicy()
    var powertrain = Powertrain()
    var input = InputDevices()
    var audio = AudioProfile()
    var metadata = MetadataFeeds()
    var carPlay = CarPlayExtensions()
    var androidAuto = AndroidAutoExtensions()

    /// Today's shipped defaults, field for field (`VehicleConfigModel.init`, 2026-09-04). Rendering
    /// this through the YAML renderer must reproduce the fixture byte-for-byte.
    static let `default` = VehicleProfile()

    /// The `rightHandDrive: Bool` reading of `driverPosition`, for the one-shot UserDefaults
    /// migration and for any legacy consumer that still wants a Bool. Centre counts as not-right.
    var rightHandDrive: Bool { driverPosition.rightHandDrive }

    /// The `nightMode: Bool` reading of `appearance.theme` for legacy consumers. `auto` is resolved
    /// by the caller that knows the Mac's effective appearance; here it reads as not-night.
    var nightModeLegacy: Bool { appearance.theme.nightModeLegacy }

    /// Every geometry value that differs between this profile (as IMPORTED) and `applied` (what the
    /// model holds after `apply` ran `clampInPlace()`), as one human-readable line each — the
    /// import summary's raw material. Inspects the alt display REGARDLESS of `altDisplay.enabled`,
    /// because `clampInPlace()` clamps `altWidth`/`altHeight`/`altFPS` regardless: a document with
    /// `enabled: false` and a 100×100 alt panel imports "silently" and re-exports as 800×480 —
    /// data changed, and until 2026-09-04 the summary only looked at alt when it was on (finding 4).
    /// A disabled display's line says so, so the user knows nothing on the wire moved.
    /// Compares what `clampInPlace()` touches (width/height/fps/insets); dpi and diagonal are
    /// written verbatim and are not clamped, so they cannot differ.
    func normalizationChanges(after applied: VehicleProfile) -> [String] {
        var lines: [String] = []
        func panel(_ label: String, _ a: PanelGeometry, _ b: PanelGeometry, _ suffix: String) {
            if a.width != b.width || a.height != b.height {
                lines.append("\(label): \(a.width)×\(a.height) → \(b.width)×\(b.height)\(suffix)")
            }
            if a.maxFPS != b.maxFPS { lines.append("\(label) frame rate: \(a.maxFPS) → \(b.maxFPS) fps\(suffix)") }
        }
        func insets(_ label: String, _ a: PanelInsets, _ b: PanelInsets, _ suffix: String) {
            if a != b {
                lines.append("\(label) insets: \(a.left)/\(a.top)/\(a.right)/\(a.bottom) → "
                             + "\(b.left)/\(b.top)/\(b.right)/\(b.bottom) (left/top/right/bottom)\(suffix)")
            }
        }
        panel("Main display", display.panel, applied.display.panel, "")
        insets("Main display", display.insets, applied.display.insets, "")
        let altSuffix = altDisplay.enabled ? "" : " (alt display is off; its stored geometry was still normalised)"
        panel("Alt display", altDisplay.panel, applied.altDisplay.panel, altSuffix)
        insets("Alt display", altDisplay.insets, applied.altDisplay.insets, altSuffix)
        return lines
    }
}

// MARK: - Adapter (box / radio) settings

/// Behaviour of the BOX that is agnostic to which projection is running. These ride the pushed YAML
/// as this project's own top-level extension keys (`wireless`, `hot_handover`, `pairing`,
/// `android_auto`, `wifi_ap`, `accessoryConfig.appDrivenSetup`) and are read by
/// `tools/session_supervisor.sh` / `vehicle_config.rs`, never by a phone. Defaults =
/// `VehicleConfigModel.init` (2026-09-04).
struct AdapterSettings: Codable, Sendable, Equatable {
    /// Bring the BT + Wi-Fi radios up and advertise while the app is connected. ONE daemon
    /// (`carplay-wireless`) advertises BOTH the wireless-CarPlay and the wireless-Android-Auto SDP
    /// records, and `session_supervisor.sh` `wireless_up()` returns early when this is false — so it
    /// gates wireless AA as well as wireless CarPlay. The UI labelled it "Wireless CarPlay" (defect 1).
    var wirelessRadios: Bool = true
    /// Force a live wireless→wired switch on cable insert (non-standard; Apple never migrates a live
    /// session). CarPlay-only in practice — AA has no wireless→wired handover path in the supervisor.
    var hotHandover: Bool = false
    /// Bluetooth association model: false = Just-Works (proven), true = Numeric Comparison.
    var pairingNumericComparison: Bool = false
    /// Box waits for this app's Pair/Cancel instead of confirming on its own. Unreachable with iOS
    /// as the peer; kept for other peers.
    var pairingInteractiveAnswer: Bool = false
    /// `wifi_ap:` (`session_supervisor.sh` `wifi_ap_enabled`). false = the box is BT radio + MFi
    /// coprocessor only and the vehicle's own hotspot carries the AirPlay endpoint (the gm_ccpa
    /// bridge role). Had NO UI at all (defect 8). Absent/true = the box runs its own AP.
    var wifiAccessPoint: Bool = true
    /// Arm `aa-bridge` when an Android phone is on the bus and no CarPlay transport owns the box.
    var androidAuto: Bool = true
    /// Relay RTSP/SETUP to this app so it authors the response (`accessoryConfig.appDrivenSetup`).
    /// A box behaviour, CarPlay-only by nature (AA has no SETUP). Default ON since 2026-08-10.
    var appDrivenSetup: Bool = true

    static let `default` = AdapterSettings()
}

// MARK: - The on-disk document

/// The neutral profile as a file: `{ schemaVersion, preset?, vehicle, adapter }`.
///
/// ENCODING DECISION (2026-09-04). JSON via Foundation's `JSONEncoder`/`JSONDecoder`, with sorted
/// keys and pretty printing, under a YAML-shaped nesting (one object per feature, the same names as
/// the Swift types). NOT YAML, even though the repo reads YAML everywhere: Swift bundles no YAML
/// coder, and a hand-rolled subset parser would be a second YAML surface in a project whose YAML
/// bugs already malformed a whole pushed document once (the metadata skip-field bug,
/// docs/carplay/04_CAPABILITIES_AND_CONFIG.md B3 — an unescaped quote made the box fall back to
/// built-in defaults for everything). The trade-off is that a human edits `"width": 1920` instead
/// of `width: 1920`; the win is that round-trip fidelity is Foundation's problem, the output is
/// deterministic (sorted keys ⇒ `git diff`-able, and two encodes of equal values are byte-equal),
/// and the parser is one the OS ships and fuzzes. If a YAML rendering is ever wanted for humans it
/// is a one-way EXPORT of this document, never the persistence format.
///
/// The file is what "Import…" reads and "Export…" writes (there was Export YAML but no import of
/// anything before this — the OEM-icon PNG picker was the only NSOpenPanel), and what presets are.
struct VehicleProfileDocument: Codable, Sendable, Equatable {
    /// Bump when a field is renamed/re-typed/removed, and add a step to `migrationSteps` that
    /// rewrites the previous shape into the new one. Additive fields need NO bump — AT ANY DEPTH —
    /// provided the new field is declared with a default (`var x: T = …`, or an Optional): `decode`
    /// fills every missing key, nested ones included, from the encoded default document before the
    /// synthesised `Codable` runs (see `fillingMissingKeys`). The exceptions are fields declared
    /// WITHOUT a default (`BrandIcon`'s three, `ConnectorSpec.type`): those are genuinely required,
    /// and a document lacking one is refused with "missing key" naming the path.
    ///
    /// Until 2026-09-04 this comment made the same promise but only the top level honoured it —
    /// every nested struct used synthesised `init(from:)`, which requires ALL keys, so one added
    /// nested field with no bump would have refused every previously exported document
    /// (reproduced: `{"vehicle":{"driverPosition":"left"}}` → `keyNotFound 'appDrivenSetup'`).
    static let currentSchemaVersion = 1
    /// Recommended file extension for the document.
    static let fileExtension = "vehicleprofile.json"

    var schemaVersion: Int = VehicleProfileDocument.currentSchemaVersion
    /// The built-in preset this document was derived from, if any. Informational — a loaded
    /// document is a plain profile whatever it started as.
    var preset: String? = nil
    var vehicle: VehicleProfile = .default
    var adapter: AdapterSettings = .default

    init(preset: String? = nil, vehicle: VehicleProfile = .default, adapter: AdapterSettings = .default) {
        self.preset = preset
        self.vehicle = vehicle
        self.adapter = adapter
    }

    enum DocumentError: Error, Equatable, CustomStringConvertible {
        /// Written by a newer app. Refuse rather than guess — silently dropping fields a newer app
        /// relies on is how a config quietly loses a setting the owner made on purpose.
        case newerSchema(found: Int, supported: Int)
        case notAnObject
        case missingSchemaVersion

        var description: String {
            switch self {
            case let .newerSchema(found, supported):
                return "profile schemaVersion \(found) is newer than this app supports (\(supported))"
            case .notAnObject: return "profile document is not a JSON object"
            case .missingSchemaVersion: return "profile document has no integer schemaVersion"
            }
        }
    }

    /// Deterministic encoding: sorted keys, pretty-printed, slashes unescaped (base64 icons carry
    /// `/`). Equal documents encode to identical bytes, which is what the round-trip test asserts.
    static func encode(_ doc: VehicleProfileDocument) throws -> Data {
        let enc = JSONEncoder()
        enc.outputFormatting = [.sortedKeys, .prettyPrinted, .withoutEscapingSlashes]
        return try enc.encode(doc)
    }

    /// Decode with the migration ladder applied first. Idempotent: a current-version document passes
    /// through untouched; an older one is rewritten step by step (v→v+1) on the raw JSON object and
    /// its `schemaVersion` bumped by each step, so re-running is a no-op. This is the same shape as
    /// the `mbaDefaultFlippedB4` one-shot in `VehicleConfigModel.init`, applied per document rather
    /// than per UserDefaults store: the guard is the version number the step itself advances.
    static func decode(_ data: Data) throws -> VehicleProfileDocument {
        guard var raw = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw DocumentError.notAnObject
        }
        guard var version = raw["schemaVersion"] as? Int else { throw DocumentError.missingSchemaVersion }
        if version > currentSchemaVersion {
            throw DocumentError.newerSchema(found: version, supported: currentSchemaVersion)
        }
        for step in migrationSteps() where step.from >= version {
            step.apply(&raw)
            version = step.from + 1
            raw["schemaVersion"] = version
        }
        let filled = try fillingMissingKeys(raw)
        let migrated = try JSONSerialization.data(withJSONObject: filled)
        return try JSONDecoder().decode(VehicleProfileDocument.self, from: migrated)
    }

    /// Tolerant decoding at every depth: fill each key the document LACKS with the default
    /// document's value for that key, recursing into objects the document DOES carry, then let the
    /// synthesised `Codable` decode the completed object. This is one function instead of a
    /// hand-written `init(from:)` in each of ~25 nested structs — and, unlike those, it cannot be
    /// forgotten: a field added with `= default` is tolerated the moment it is declared, because the
    /// templates are encoded `VehicleProfileDocument` values.
    ///
    /// The one subtlety is that `JSONEncoder` writes a nil Optional as an ABSENT key, so "absent"
    /// is ambiguous: `input.touchscreen` missing from a `dhu-hybrid` export means "no touchscreen",
    /// while `display.panel.dpi` missing from an old export means "take the default". Three
    /// templates, all computed here (microseconds; `decode` runs on an Import click, and a
    /// `static let` of `[String: Any]` would just raise `Sendable` questions), resolve it:
    ///   * `required` — the default with EVERY Optional nil. A key present here is non-optional and
    ///     is filled when missing; a key absent here is an Optional and is left absent → nil.
    ///     Absence must never conjure a touchscreen the document deliberately omitted.
    ///   * `defaults` — the real default document: the VALUE a missing non-optional key takes
    ///     (`input` missing outright → the default input set, touchscreen included).
    ///   * `populated` — the default with every defaultable Optional sub-object PRESENT: the shape
    ///     to recurse through, and the fill source inside an optional object the document itself
    ///     carries (`"touchpad": {}` means "a touchpad with default settings", not "missing key
    ///     'buttons'"). Sets one level of Optionals only; an Optional inside an optional object
    ///     would need its own nil in `required` before it could be added — none exists today.
    /// Arrays: an element template is taken from the first element of the `populated` array (the
    /// three `audioFormats` rows all carry every key, and the first row IS `AudioFormat()`), so a
    /// row missing `input` takes the row default. `connectors` is empty in every template on
    /// purpose — `ConnectorSpec.type` has no default and must stay required rather than be invented.
    ///
    /// What this does NOT do: change a value that is present (a wrong type or a bad enum spelling
    /// still fails at its path, as the tests assert), touch `null` (decoded as Swift does: nil for
    /// an Optional, an error for anything else), or alter a complete document — every key present
    /// ⇒ nothing filled ⇒ the same object the synthesised decoder saw before this existed, which is
    /// what keeps `encode(decode(x)) == x` for exported files and for every built-in preset.
    static func fillingMissingKeys(_ raw: [String: Any]) throws -> [String: Any] {
        func object(_ doc: VehicleProfileDocument) throws -> [String: Any] {
            let data = try JSONEncoder().encode(doc)
            return try JSONSerialization.jsonObject(with: data) as? [String: Any] ?? [:]
        }
        var required = VehicleProfileDocument()
        required.vehicle.input.touchscreen = nil          // the only Optional whose default is non-nil
        var populated = VehicleProfileDocument()
        populated.vehicle.input.touchscreen = Touchscreen()
        populated.vehicle.input.touchpad = Touchpad()
        populated.vehicle.input.rotaryKnob = RotaryKnob()
        return fill(raw, defaults: try object(VehicleProfileDocument()),
                    required: try object(required), populated: try object(populated))
    }

    /// `defaults == nil` means "inside an optional object the default did not carry" — the fill
    /// value then comes from `populated`, and so does `required` (an optional object's own fields
    /// are all required, see above).
    private static func fill(_ doc: [String: Any], defaults: [String: Any]?, required: [String: Any],
                             populated: [String: Any]) -> [String: Any] {
        var out = doc
        for (key, template) in populated {
            if let present = out[key] {
                if let presentObject = present as? [String: Any], let templateObject = template as? [String: Any] {
                    out[key] = fill(presentObject,
                                    defaults: defaults?[key] as? [String: Any],
                                    required: required[key] as? [String: Any] ?? templateObject,
                                    populated: templateObject)
                } else if let presentArray = present as? [Any], let templateArray = template as? [Any],
                          let elementTemplate = templateArray.first as? [String: Any] {
                    out[key] = presentArray.map { element -> Any in
                        guard let elementObject = element as? [String: Any] else { return element }
                        return fill(elementObject, defaults: nil, required: elementTemplate, populated: elementTemplate)
                    }
                }
            } else if required[key] != nil {
                out[key] = defaults?[key] ?? template
            }
        }
        return out
    }

    /// One step per historical schema version, in ascending order. Each rewrites the raw object
    /// from shape `from` to shape `from + 1`. Empty while the schema is at 1; the first bump adds
    /// `(from: 1, apply: …)` here and a test that feeds a v1 fixture through `decode`.
    static func migrationSteps() -> [(from: Int, apply: (inout [String: Any]) -> Void)] {
        []
    }

    /// Top-level tolerance for callers that hand this type to a bare `JSONDecoder` (the harness
    /// does): a missing block takes its default. Through `decode` this is redundant —
    /// `fillingMissingKeys` has already completed the object at every depth — but it costs nothing
    /// and keeps `{"schemaVersion": 1}` a valid document by either route. Unknown keys are ignored
    /// by `JSONDecoder` already.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decodeIfPresent(Int.self, forKey: .schemaVersion) ?? Self.currentSchemaVersion
        preset = try c.decodeIfPresent(String.self, forKey: .preset)
        vehicle = try c.decodeIfPresent(VehicleProfile.self, forKey: .vehicle) ?? .default
        adapter = try c.decodeIfPresent(AdapterSettings.self, forKey: .adapter) ?? .default
    }
}

// MARK: - Presets

/// A named, built-in profile — the neutral counterpart of Apple's ten CarPlay Simulator
/// `VehicleConfigs/Configs/*.yaml` and Google's eleven DHU `config/*.ini`. Each vendor ships presets
/// because a head-unit developer starts from a known geometry and input set; the neutral document
/// can carry both vendors' catalogues because every preset is a description of a CAR.
struct VehicleProfilePreset: Sendable, Identifiable, Equatable {
    enum Origin: Sendable, Equatable {
        /// This project's own.
        case project
        /// Derived from a DHU preset (`~/Library/Android/sdk/extras/google/auto/config/<file>`).
        case desktopHeadUnit(file: String)
        /// Derived from a CarPlay Simulator template (`…/VehicleConfigs/Configs/<file>`).
        case carPlaySimulator(file: String)
    }

    let id: String
    let title: String
    let origin: Origin
    /// One sentence for the picker: what is distinctive about this preset.
    let summary: String
    let vehicle: VehicleProfile
    let adapter: AdapterSettings

    var document: VehicleProfileDocument {
        VehicleProfileDocument(preset: id, vehicle: vehicle, adapter: adapter)
    }
}

extension VehicleProfilePreset {
    /// Builder: start from the shipped default and mutate. Every preset differs from `.default` in
    /// a handful of fields, and saying only those keeps the catalogue reviewable.
    private static func make(_ id: String, _ title: String, _ origin: Origin, _ summary: String,
                             _ edit: (inout VehicleProfile) -> Void) -> VehicleProfilePreset {
        var v = VehicleProfile.default
        edit(&v)
        return VehicleProfilePreset(id: id, title: title, origin: origin, summary: summary,
                                    vehicle: v, adapter: .default)
    }

    /// Apple's Simulator input set — every template declares knob (+home/back, +nudge), telephony
    /// and media buttons, and a touchpad with buttons; all but Knob Only add a Hi-Fi touchscreen and
    /// (Standard/Widescreen) a D-pad.
    private static func appleInputs(_ v: inout VehicleProfile, touchscreen: Bool, dPad: Bool) {
        v.input.touchscreen = touchscreen ? Touchscreen() : nil
        v.input.primary = touchscreen ? .touchscreen : .rotary
        v.input.rotaryKnob = RotaryKnob(homeAndBackButtons: true, nudge: true)
        v.input.touchpad = touchscreen ? Touchpad(buttons: true) : nil
        v.input.telephonyButtons = true
        v.input.mediaButtons = true
        v.input.dPad = dPad
    }

    /// The catalogue. DHU-derived entries reproduce the .ini's geometry/input/sensor facts (read
    /// 2026-09-04 from the installed DHU). DHU `[sensors]` lines are runtime feeds, not profile
    /// facts, except `fueltypes`/`evconnectors` — which is why `all_720p.ini` and `loaded_720p.ini`
    /// are ONE entry: their `[general]` sections are identical and they differ only in that
    /// `loaded_720p` carries the `[sensors]` block (`location`/`night_mode`/`driving_status`) and
    /// `all_720p` has no `[sensors]` section at all. Apple-derived entries reproduce the template's
    /// geometry and HID set; `Minimum.yaml` (748×456) is OMITTED because it is below the app's
    /// enforced 800×480 floor and `clampInPlace()` would silently turn it into Standard. (`dhu-6in`
    /// is below the same floor and is KEPT, because its title is the DHU's own name for that panel
    /// and its summary says what the app does to it — see the entry.)
    static let builtIn: [VehicleProfilePreset] = [
        VehicleProfilePreset(id: "shipped", title: "CarLink Widescreen (shipped default)", origin: .project,
                             summary: "1920×1080 @60, touchscreen + D-pad + media buttons — the fixture-locked default.",
                             vehicle: .default, adapter: .default),

        // ---- Google Desktop Head Unit ----
        make("dhu-default", "DHU default — 800×480 @30", .desktopHeadUnit(file: "default.ini"),
             "The DHU's baseline: smallest tier, 30 fps, touch only.") { v in
            v.display.panel = PanelGeometry(width: 800, height: 480, maxFPS: 30)
            v.input = InputDevices(primary: .touchscreen, touchscreen: Touchscreen(), dPad: false, mediaButtons: false)
        },
        make("dhu-720p", "DHU 1280×720 @30", .desktopHeadUnit(file: "default_720p.ini"),
             "HD tier, touch only.") { v in
            v.display.panel = PanelGeometry(width: 1280, height: 720, maxFPS: 30)
            v.input = InputDevices(primary: .touchscreen, touchscreen: Touchscreen(), dPad: false, mediaButtons: false)
        },
        make("dhu-1080p", "DHU 1920×1080 @30", .desktopHeadUnit(file: "default_1080p.ini"),
             "Full-HD tier at the DHU's 30 fps, touch only.") { v in
            v.display.panel = PanelGeometry(width: 1920, height: 1080, maxFPS: 30)
            v.input = InputDevices(primary: .touchscreen, touchscreen: Touchscreen(), dPad: false, mediaButtons: false)
        },
        make("dhu-wide", "DHU wide — 1280×500 ultrawide", .desktopHeadUnit(file: "default_wide.ini"),
             "1280×720 tier with 220 px of vertical margin: the panel is 1280×500. Exercises AA margin fitting.") { v in
            v.display.panel = PanelGeometry(width: 1280, height: 500, maxFPS: 30)
            v.input = InputDevices(primary: .touchscreen, touchscreen: Touchscreen(), dPad: false, mediaButtons: false)
        },
        // The .ini declares the 800×480 tier with `marginwidth 50 / marginheight 30` — a 750×450
        // visible panel, pixel-exact. The profile records that true panel (750×450 @ 6.0"), but
        // NEITHER renderer receives it today: `apply` ends with `clampInPlace()`, whose 800×480
        // floor is applied to the MODEL before any renderer runs, so on load this preset becomes
        // `dhu-default` plus `diagonalInches: 6.0`. (And were the floor lifted, the AA renderer's
        // `tierAndVisible` would fit a 5:3 panel to the 800×480 tier with 0×0 margins and downscale,
        // not declare 50×30 — R2 review, 2026-09-04.) The summary says what actually happens
        // rather than promising AA-only geometry the app cannot deliver.
        make("dhu-6in", "DHU 6-inch — 750×450", .desktopHeadUnit(file: "default_6in.ini"),
             "Google's 6-inch panel: 800×480 tier, 50×30 margins → 750×450 visible. The app's 800×480 floor clamps it to 800×480 on load (both protocols); only the 6.0\" diagonal survives.") { v in
            v.display.panel = PanelGeometry(width: 750, height: 450, maxFPS: 30, dpi: 160, diagonalInches: 6.0)
            v.input = InputDevices(primary: .touchscreen, touchscreen: Touchscreen(), dPad: false, mediaButtons: false)
        },
        // Over `default_720p.ini` this adds touchpad (+navigation), controller, instrumentcluster
        // and `playbackstatus = true` — the DHU's "show the media_playback feed" switch, i.e.
        // `metadata.nowPlaying`, which the baseline already declares on. So the feeds stay at the
        // baseline's all-true; a "loaded" preset must never declare FEWER than the file it extends
        // (until 2026-09-04 it set navigation/telephony false, the reverse of the .ini diff).
        // `all_720p.ini` is this file minus the `[sensors]` block — same profile, one entry.
        make("dhu-loaded-720p", "DHU loaded 720p — every input + cluster", .desktopHeadUnit(file: "loaded_720p.ini"),
             "Touch + touchpad + rotary controller, instrument cluster and playback status. all_720p.ini is the same minus its [sensors] block.") { v in
            v.display.panel = PanelGeometry(width: 1280, height: 720, maxFPS: 30)
            v.input = InputDevices(primary: .touchscreen, touchscreen: Touchscreen(), touchpad: Touchpad(),
                                   rotaryKnob: RotaryKnob(homeAndBackButtons: true, nudge: true),
                                   dPad: true, mediaButtons: true)
            v.altDisplay.enabled = true
        },
        make("dhu-hybrid", "DHU hybrid — touchpad + rotary, no touchscreen", .desktopHeadUnit(file: "hybrid.ini"),
             "No touch surface on the panel; a touchpad with navigation plus a rotary controller.") { v in
            v.display.panel = PanelGeometry(width: 800, height: 480, maxFPS: 30)
            v.input = InputDevices(primary: .touchpad, touchscreen: nil, touchpad: Touchpad(),
                                   rotaryKnob: RotaryKnob(homeAndBackButtons: true, nudge: true),
                                   dPad: true, mediaButtons: false)
        },
        make("dhu-rotary", "DHU rotary — controller only", .desktopHeadUnit(file: "rotary.ini"),
             "No touch at all; everything through the rotary controller.") { v in
            v.display.panel = PanelGeometry(width: 800, height: 480, maxFPS: 30)
            v.input = InputDevices(primary: .rotary, touchscreen: nil, touchpad: nil,
                                   rotaryKnob: RotaryKnob(homeAndBackButtons: true, nudge: true),
                                   dPad: true, mediaButtons: false)
        },
        make("dhu-touchpad", "DHU touchpad — tap to select", .desktopHeadUnit(file: "touchpad.ini"),
             "Touchpad with navigation and tap-as-select; no touchscreen.") { v in
            v.display.panel = PanelGeometry(width: 800, height: 480, maxFPS: 30)
            v.input = InputDevices(primary: .touchpad, touchscreen: nil, touchpad: Touchpad(buttons: true),
                                   rotaryKnob: nil, dPad: false, mediaButtons: false)
        },
        make("dhu-sensors", "DHU sensors — plug-in hybrid, Tesla connector", .desktopHeadUnit(file: "default_sensors.ini"),
             "fueltypes unleaded,electric + evconnectors supercharger; the sensor feeds are runtime, not profile.") { v in
            v.display.panel = PanelGeometry(width: 800, height: 480, maxFPS: 30)
            v.input = InputDevices(primary: .touchscreen, touchscreen: Touchscreen(), dPad: false, mediaButtons: false)
            v.powertrain = Powertrain(engines: [.gasoline, .electric], connectors: [ConnectorSpec(type: .nacsDC)])
        },

        // ---- Apple CarPlay Simulator ----
        make("apple-standard", "Apple Standard — 800×480", .carPlaySimulator(file: "Standard.yaml"),
             "Apple's baseline: 800×480, touchscreen, knob, touchpad, D-pad, media + telephony buttons.") { v in
            v.identity.headUnitName = "Standard"
            v.display.panel = PanelGeometry(width: 800, height: 480, maxFPS: 60)
            appleInputs(&v, touchscreen: true, dPad: true)
        },
        make("apple-knob-only", "Apple Standard Knob Only", .carPlaySimulator(file: "Standard Knob Only.yaml"),
             "800×480, touchScreenMode Disabled, primaryInput Knobs.") { v in
            v.identity.headUnitName = "Standard Knob Only"
            v.display.panel = PanelGeometry(width: 800, height: 480, maxFPS: 60)
            appleInputs(&v, touchscreen: false, dPad: false)
            v.input.touchpad = nil
        },
        make("apple-widescreen", "Apple Widescreen — 1920×720", .carPlaySimulator(file: "Widescreen.yaml"),
             "Apple's ultrawide template. Not an AA tier: AA renders it as 1280×720 with margins.") { v in
            v.identity.headUnitName = "Widescreen"
            v.display.panel = PanelGeometry(width: 1920, height: 720, maxFPS: 60)
            appleInputs(&v, touchscreen: true, dPad: true)
        },
        // Apple's template declares `VideoStream.Alt1` at `pixelDimensions` 1920×720 — the SAME
        // grid as the main stream, full-coverage viewArea/safeArea, `initialURL
        // maps:/car/instrumentcluster/map`. Until 2026-09-04 this entry inherited the app's
        // 800×480 alt default and its summary said so; that was the app's number, not Apple's. The
        // template states no frame rate for either stream, so the alt keeps the app's 30.
        make("apple-widescreen-cluster", "Apple Widescreen Instrument Cluster", .carPlaySimulator(file: "Widescreen Instrument Cluster.yaml"),
             "1920×720 main plus a 1920×720 cluster stream (initialURL maps:/car/instrumentcluster/map).") { v in
            v.identity.headUnitName = "Widescreen Instrument Cluster"
            v.display.panel = PanelGeometry(width: 1920, height: 720, maxFPS: 60)
            appleInputs(&v, touchscreen: true, dPad: true)
            v.altDisplay = AltDisplay(enabled: true, panel: PanelGeometry(width: 1920, height: 720, maxFPS: 30))
        },
        make("apple-portrait", "Apple Portrait — 900×1200", .carPlaySimulator(file: "Portrait.yaml"),
             "Apple's portrait template. AA has portrait tiers (720×1280 up); this maps with margins.") { v in
            v.identity.headUnitName = "Portrait"
            v.display.panel = PanelGeometry(width: 900, height: 1200, maxFPS: 60)
            appleInputs(&v, touchscreen: true, dPad: false)
        },
    ]

    static func named(_ id: String) -> VehicleProfilePreset? { builtIn.first { $0.id == id } }
}
