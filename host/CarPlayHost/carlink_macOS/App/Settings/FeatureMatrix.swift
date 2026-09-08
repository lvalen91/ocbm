// FeatureMatrix.swift — per-feature, per-projection support: what CarPlay and Android Auto each DO
// with a neutral `VehicleProfile` fact, in data rather than in tooltip prose.
//
// WHY. Before this, protocol applicability lived in English inside ~60 `FieldInfo` tooltip strings
// ("CarPlay-only", "⚠️ Not implemented on the box", "Android Auto: LIVE — declared as
// driver_position…"), which is how the same setting came to be labelled CarPlay-only while being
// live only for AA (nightMode/rightHandDrive, defect 2), how `wireless:` came to say "Wireless
// CarPlay" while gating wireless AA (defect 1), and how "name" was called inert while AA advertises
// it (defect 3). Prose cannot be checked. This table can: the Vehicle tab renders a badge per
// projection and an explanation row per feature straight from it, and adding a projection or a
// feature means adding a case the compiler makes you fill in.
//
// The SHAPE is deliberately `ControlsWindow.swift:69-104`'s `Capability` idiom — an enum of intents
// plus `isAvailable` / `unavailableReason` — so the codebase has ONE way of saying "this protocol
// can(not) express that". `FeatureSupport` widens it with the middle state the Controls window did
// not need (supported-with-limits: AA can take a 1920×720 panel, just not exactly) and with the
// vendor's own term, the schema key, and PROVENANCE.
//
// PROVENANCE lives HERE, not on `VehicleProfile` (owner amendment, 2026-09-04). Whether a feature is
// device-proven, merely advertised, or refuted is a fact about (feature, protocol, THIS app's
// renderer) — it is the same for every owner's profile and changes when the code changes, not when
// a value does. Putting it on the profile would either be wrong for the next user ("1440p, verified"
// travels with a document it is not true for) or duplicated across every document. The value-
// dependent cases (tier 3 proven, tier 5 not) are `valueNotes`, keyed by feature + projection +
// the value's string form. Dates below are the ones recorded in the code they summarise
// (AACapability.swift, SettingsWindow.swift, session_supervisor.sh); when a claim is re-verified,
// update the entry here and the comment there in the same commit.
//
// Foundation only, no SwiftUI: compiles in the headless harness beside VehicleProfile.swift (it
// references `DrivingRestrictionSet` from there, so the two files compile as a pair).

import Foundation

// MARK: - Projections

/// The two projection protocols this head unit speaks. `ControlsBridge.isAndroidAuto` is the
/// runtime "which one is live" flag; this enum is the identity a support entry is keyed by.
enum Projection: String, CaseIterable, Codable, Sendable, Hashable {
    case carPlay, androidAuto

    var displayName: String {
        switch self {
        case .carPlay:     return "CarPlay"
        case .androidAuto: return "Android Auto"
        }
    }
    /// The vendor authoring format the term/key columns refer to.
    var schemaName: String {
        switch self {
        case .carPlay:     return "CarPlay Simulator VehicleConfig YAML"
        case .androidAuto: return "Desktop Head Unit .ini / gal ServiceDiscoveryResponse"
        }
    }
}

// MARK: - Provenance

enum VerificationStatus: String, Codable, Sendable, Hashable {
    /// Observed on a real phone against this app's renderer, on the recorded date — OR, when the
    /// note opens with "Source-verified", a fact established by reading code or a vendor schema on
    /// that date (`Verification.sourceVerified`). The two share one case deliberately: the UI
    /// (FeatureBadges.swift) and the tests switch exhaustively over this enum, so a fourth case is a
    /// cross-file change; the note is rendered verbatim, so the prefix is what keeps the badge honest.
    case deviceProven
    /// Declared/emitted by the renderer but never observed to take effect (or not rendered at all yet).
    case unverified
    /// Tried and observed NOT to work; the note says what happened. Do not re-try without new evidence.
    case refuted
}

struct Verification: Sendable, Equatable {
    let status: VerificationStatus
    /// ISO date of the observation, when there is one.
    let date: String?
    /// The evidence in one sentence — what was seen, on what.
    let note: String

    static func proven(_ date: String, _ note: String) -> Verification { .init(status: .deviceProven, date: date, note: note) }
    static func unverified(_ note: String) -> Verification { .init(status: .unverified, date: nil, note: note) }
    static func refuted(_ date: String, _ note: String) -> Verification { .init(status: .refuted, date: date, note: note) }
    /// A fact verified in SOURCE or a vendor SCHEMA, not on a phone: "no such key exists", "nothing
    /// reads this field", "these two arms share no migration path". Same status as `proven` (see the
    /// enum comment for why) but the note is forced to say so, because `deviceProven`'s promise is a
    /// phone observation and the 2026-09-04 audit found seven entries wearing that badge for grep
    /// results. `date` is the day the source was read; cite file:line so the next audit can re-grep.
    static func sourceVerified(_ date: String, _ note: String) -> Verification {
        .init(status: .deviceProven, date: date, note: "Source-verified, not a device observation: " + note)
    }
}

// MARK: - Support

/// What ONE projection does with ONE neutral feature.
struct FeatureSupport: Sendable, Equatable {
    enum Level: String, Codable, Sendable, Hashable {
        /// The value is expressed as authored.
        case supported
        /// The value is expressed, but approximated, partially, or with a caveat `effect` names.
        case limited
        /// The protocol (or this app's renderer for it) cannot express the value; it is ignored.
        case unsupported
    }

    let level: Level
    /// The vendor's own name for the feature (what their documentation calls it).
    let vendorTerm: String
    /// The schema key(s) the value lands in — YAML key path for CarPlay, DHU .ini key / SDR field for
    /// Android Auto. Nil when nothing is emitted.
    let vendorKey: String?
    /// ONE user-facing sentence: what this protocol will actually do with the value. Rendered
    /// verbatim as the per-protocol explanation row under the control.
    let effect: String
    let verification: Verification

    var isAvailable: Bool { level != .unsupported }
}

// MARK: - Features

/// One case per neutral feature — the unit the Vehicle/Adapter tabs are laid out in. Order here is
/// display order within a section.
enum Feature: String, CaseIterable, Codable, Sendable, Hashable {
    // Identity & branding
    case headUnitName, branding
    // Display
    case panelGeometry, frameRate, pixelDensity, insets, videoCodec, altDisplay
    // Appearance
    case theme, statusBar
    // Driving
    case driverPosition, drivingRestrictions
    // Vehicle
    case powertrain
    // Input
    case inputDevices
    // Audio
    case audioProfile
    // Data feeds
    case metadataFeeds
    // Adapter (box behaviour — agnostic to the projection)
    case wirelessRadios, hotHandover, pairing, wifiAccessPoint, androidAutoProjection, appDrivenSetup

    enum Section: String, CaseIterable, Sendable, Hashable {
        case identity = "Identity & branding"
        case display = "Display"
        case appearance = "Appearance"
        case driving = "Driving"
        case vehicle = "Vehicle"
        case input = "Input"
        case audio = "Audio"
        case dataFeeds = "Data feeds"
        case adapter = "Adapter"

        /// Which tab a section lives on. Vehicle = the neutral profile with per-protocol rows inline;
        /// Adapter = box/radio/health/projection enables.
        var tab: Tab { self == .adapter ? .adapter : .vehicle }
    }

    enum Tab: String, CaseIterable, Sendable { case vehicle = "Vehicle", adapter = "Adapter", diagnostics = "Diagnostics" }

    var section: Section {
        switch self {
        case .headUnitName, .branding:                                          return .identity
        case .panelGeometry, .frameRate, .pixelDensity, .insets, .videoCodec, .altDisplay: return .display
        case .theme, .statusBar:                                                return .appearance
        case .driverPosition, .drivingRestrictions:                             return .driving
        case .powertrain:                                                       return .vehicle
        case .inputDevices:                                                     return .input
        case .audioProfile:                                                     return .audio
        case .metadataFeeds:                                                    return .dataFeeds
        case .wirelessRadios, .hotHandover, .pairing, .wifiAccessPoint, .androidAutoProjection, .appDrivenSetup: return .adapter
        }
    }

    /// Neutral, vendor-free control title.
    var title: String {
        switch self {
        case .headUnitName:          return "Head unit name"
        case .branding:              return "Vehicle branding"
        case .panelGeometry:         return "Panel resolution"
        case .frameRate:             return "Frame rate"
        case .pixelDensity:          return "Pixel density"
        case .insets:                return "UI insets"
        case .videoCodec:            return "Video codec"
        case .altDisplay:            return "Second display"
        case .theme:                 return "Appearance"
        case .statusBar:             return "Status bar"
        case .driverPosition:        return "Driver position"
        case .drivingRestrictions:   return "Driving restrictions"
        case .powertrain:            return "Powertrain and charging"
        case .inputDevices:          return "Input devices"
        case .audioProfile:          return "Audio"
        case .metadataFeeds:         return "Metadata feeds"
        case .wirelessRadios:        return "Wireless radios"
        case .hotHandover:           return "Hot hand-over"
        case .pairing:               return "Bluetooth pairing"
        case .wifiAccessPoint:       return "Wi-Fi access point"
        case .androidAutoProjection: return "Android Auto projection"
        case .appDrivenSetup:        return "App-driven SETUP"
        }
    }

    /// One neutral sentence describing the FACT the control edits (protocol-free; the per-protocol
    /// sentences are `FeatureSupport.effect`).
    var summary: String {
        switch self {
        case .headUnitName:          return "What the phone calls this head unit."
        case .branding:              return "The vehicle maker's name and logo on the projected home screen."
        case .panelGeometry:         return "The exact pixel grid of the panel the projection fills."
        case .frameRate:             return "Highest frame rate the panel is driven at (30 or 60)."
        case .pixelDensity:          return "How large UI elements are drawn, as dots per inch."
        case .insets:                return "Edges the projected UI must keep clear (curved or occluded corners)."
        case .videoCodec:            return "Whether the phone may encode video as H.265 (HEVC)."
        case .altDisplay:            return "A second projected surface such as an instrument cluster."
        case .theme:                 return "Light, dark, or follow this Mac's appearance."
        case .statusBar:             return "Status-bar elements the car draws itself, so the phone should hide them."
        case .driverPosition:        return "Which side of the car the driver sits on."
        case .drivingRestrictions:   return "What the phone withholds while the car is moving."
        case .powertrain:            return "Engine types and charging connectors this vehicle has."
        case .inputDevices:          return "The control surfaces the car has: touchscreen, touchpad, rotary knob, buttons."
        case .audioProfile:          return "Voice-stream sample rate and whether call audio rides the projection link."
        case .metadataFeeds:         return "Which now-playing, navigation and call feeds the head unit consumes."
        case .wirelessRadios:        return "Bring up Bluetooth and Wi-Fi and advertise for wireless projection while the app is connected."
        case .hotHandover:           return "Switch a live wireless session to the cable when one is plugged in."
        case .pairing:               return "Bluetooth association model: Just-Works or Numeric Comparison."
        case .wifiAccessPoint:       return "Whether the box runs its own Wi-Fi access point or rides the vehicle's hotspot."
        case .androidAutoProjection: return "Offer Android Auto to an Android phone on the box's USB bus."
        case .appDrivenSetup:        return "Let this app author the session SETUP response instead of the box."
        }
    }

    /// `VehicleConfigModel` property names of the vendor-EXCLUSIVE controls that belong UNDER this
    /// feature as a badged sub-group (owner steer: exclusive settings live inside the feature they
    /// belong to, never on their own tab). Empty = no exclusive sub-group. The UI worker renders
    /// these below the neutral control with the projection's badge; the model worker keeps them
    /// as fields of `CarPlayExtensions` / `AndroidAutoExtensions`.
    func exclusiveKeys(on p: Projection) -> [String] {
        switch (self, p) {
        case (.headUnitName, .carPlay):        return ["accessoryName"]
        case (.branding, .carPlay):            return ["oemIconEnabled", "oemIconVisible", "oemIconBase64"]
        case (.panelGeometry, .androidAuto):   return ["aaFitPanelWithMargins"]
        // + the second main view area (Dock resize button, 2026-09-05): a CarPlay-only rect that
        // lives under the insets feature per DESIGN.md §0 decision 4 rather than as its own case.
        case (.insets, .carPlay):              return ["enablesViewAreas", "mainDrawOutsideSafe", "enablesCornerMasks",
                                                       "viewArea2Enabled", "viewArea2X", "viewArea2Y", "viewArea2W", "viewArea2H"]
        case (.videoCodec, .carPlay):          return ["enablesVideoPlayback"]
        case (.videoCodec, .androidAuto):      return ["aaPreferHEVC"]
        case (.altDisplay, .carPlay):          return ["altSafeLeft", "altSafeTop", "altSafeRight", "altSafeBottom", "altDrawOutsideSafe"]
        case (.theme, .carPlay):               return ["enablesUIAppearance", "enablesMapAppearance"]
        case (.drivingRestrictions, .carPlay): return ["limitedUIJapanMaps", "limitedUIPairedDevices", "limitedUIThemeCustomization",
                                                       "limitedUIAutomakerSettings", "limitedUIAutomakerSettingsInfoButton"]
        case (.powertrain, .carPlay):          return ["vehicleStatusEnabled", "vehicleStatusCaps"]
        // EMPTIED 2026-09-04. `touchScreenHighFidelity` was listed here as CarPlay-exclusive, which
        // put the Touchscreen control inside the CarPlay-only sub-group — but that field is exactly
        // what decides whether ANDROID AUTO declares a touchscreen at all
        // (VehicleConfigModel+Profile derives `input.touchscreen` from it, and
        // AACapability+Profile reads `input.touchscreen != nil`). Marking a field CarPlay-exclusive
        // while a second protocol reads it is the same overclaim class this matrix exists to kill,
        // and it was the root of the "picker snaps to Touchscreen" contradiction: the control lived
        // in one protocol's box while governing both. It is now a neutral toggle in the input block.
        case (.inputDevices, .carPlay):        return []
        case (.audioProfile, .carPlay):        return ["audioMode", "audioFormats", "enablesMainBufferedAudio", "enablesEnhancedSiri"]
        case (.metadataFeeds, .carPlay):       return ["metadataTier", "metadataSkip", "enablesFocusTransfer", "enablesUIContext",
                                                       "enablesUISync", "enablesFileTransfer", "enablesLogTransfer",
                                                       "enablesVehicleDataProtocol", "enablesDCX"]
        default:                               return []
        }
    }
}

// MARK: - The matrix

enum FeatureMatrix {

    /// The `Capability.isAvailable` idiom: can this projection express the feature at all?
    static func isAvailable(_ f: Feature, on p: Projection) -> Bool { support(f, on: p).isAvailable }

    /// The `Capability.unavailableReason` idiom: nil when available, else the `effect` sentence.
    static func unavailableReason(_ f: Feature, on p: Projection) -> String? {
        let s = support(f, on: p)
        return s.isAvailable ? nil : s.effect
    }

    static func features(in section: Feature.Section) -> [Feature] {
        Feature.allCases.filter { $0.section == section }
    }

    /// Every projection's support for a feature, in `Projection.allCases` order — the badge row.
    static func supports(_ f: Feature) -> [(projection: Projection, support: FeatureSupport)] {
        Projection.allCases.map { ($0, support(f, on: $0)) }
    }

    /// True when both projections express the feature the same way (no per-protocol rows needed
    /// beyond the badges).
    static func isUniform(_ f: Feature) -> Bool {
        Projection.allCases.allSatisfy { support(f, on: $0).level == .supported }
    }

    // swiftlint:disable function_body_length
    static func support(_ f: Feature, on p: Projection) -> FeatureSupport {
        switch (f, p) {

        // ---- Identity & branding ----
        // CORRECTED 2026-09-04 (audit finding 6). The old effect said the box "derives the accessory
        // name from its MAC and serial", which reads as if the profile name were an input. It is not:
        // the base is the string literal "CarLink" at both iap2d Identify sites; only the SUFFIX is
        // derived. And the CarPlay-only "Accessory name" override this row points at is parsed by the
        // box (C-6 bounded it to 63 bytes, commit b96f643 2026-08-11 — "the prerequisite, not the
        // application") and read by nothing: `accessory_name_bounded()` has no caller.
        case (.headUnitName, .carPlay):
            return FeatureSupport(level: .limited, vendorTerm: "name", vendorKey: "name",
                effect: "Stored as the config document's name only. iOS shows \"CarLink-<suffix>\" — the base is hardcoded on the box and the suffix is the Wi-Fi MAC's last two octets (or the serial's last 4 hex); the profile name never reaches the phone, and the CarPlay-only Accessory name below is parsed by the box but not applied yet.",
                verification: .sourceVerified("2026-09-04", "ccpa/iap2d/src/main.rs:370,392 pass message::accessory_name(\"CarLink\") at both Identify sites (iap2-core message.rs:193-206 derives the suffix); crates/vendor/wireless/src/main.rs:287 uses the same for the BT name; vehicle_config.rs:948 accessory_name_bounded() has no caller — docs/ops/04_OPEN_ITEMS.md:337-341"))
        // CORRECTED 2026-09-04 (audit finding 5). Was `.supported` + proven "the phone shows it". No
        // docs/androidauto/ file records the name being observed on the phone, and what is emitted is
        // not the profile's name verbatim: AAWire appends " OCBM" to display_name. Declared, not observed.
        case (.headUnitName, .androidAuto):
            return FeatureSupport(level: .limited, vendorTerm: "display_name / headunit_info", vendorKey: "display_name (SDR field 14) + headunit_info (field 17)",
                effect: "Emitted in every service discovery as display_name \"<name> OCBM\" and as headunit_info make = name, model = \"OCBM\", software name = name, build = \"carlink-macos\"; whether and where the phone displays it has not been observed.",
                verification: .unverified("AAWire.swift:484-485 (headunit_info) and :517 (display_name) emit it; no docs/androidauto/ record of the phone showing the name — declared, not observed"))

        case (.branding, .carPlay):
            return FeatureSupport(level: .supported, vendorTerm: "oemIconConfig", vendorKey: "oemIconConfig",
                effect: "Shows the label and the custom icon on the CarPlay home screen; visible:false with the icon present is the hide signal.",
                verification: .unverified("emitted since 2026-07; the fixture locks the YAML but on-screen display has no recorded device date"))
        case (.branding, .androidAuto):
            return FeatureSupport(level: .limited, vendorTerm: "display_name", vendorKey: "display_name",
                effect: "Shows the name only, with a generic Exit glyph — Android Auto has no custom OEM icon slot.",
                verification: .sourceVerified("2026-09-04", "no icon field exists in the SDR/DHU vocabulary (AAWire.swift serviceDiscoveryResponseFull emits none); the name path is the head-unit-name path"))

        // ---- Display ----
        case (.panelGeometry, .carPlay):
            return FeatureSupport(level: .supported, vendorTerm: "pixelDimensions", vendorKey: "displayPanelsConfig / videoStreamsConfig.mainVideoStream.pixelDimensions",
                effect: "Rendered and streamed at exactly this size (800–3840 × 480–2160).",
                verification: .proven("2026-07-12", "the box arms resolution from the pushed config; 1920x1080 in daily use"))
        case (.panelGeometry, .androidAuto):
            return FeatureSupport(level: .limited, vendorTerm: "resolution (VideoCodecResolutionType)", vendorKey: "resolution",
                effect: "Snapped to one of nine tiers; a non-tier panel is declared as the enclosing tier with cropped margins, then scaled to the panel.",
                verification: .proven("2026-09-04", "800x480, 1280x720 and 1920x1080 accepted by a Pixel 10; margin fitting (T4) verified; tiers above 1080p need HEVC — see valueNotes"))

        // CORRECTED 2026-09-04 (audit finding 4). The old note's "24 was dropped because the box
        // silently yielded the default" exists only as a code comment (SettingsWindow.swift:45-46, owner
        // directive 2026-07-12) — no docs/ file or session log records the observation, and :796 only
        // migrates a stale persisted 24. What IS evidenced is the 60 fps daily-use declaration; 30 has
        // no dated CarPlay observation of its own (valueNotes carry the per-value split).
        case (.frameRate, .carPlay):
            return FeatureSupport(level: .supported, vendorTerm: "maxFPS", vendorKey: "videoStreamsConfig.mainVideoStream.maxFPS",
                effect: "Declared as the stream's maximum; 30 or 60. 60 is the daily-use declaration; 30 is offered but has not been run on a phone.",
                verification: .proven("2026-07-12", "maxFPS 60 is the default (SettingsWindow.swift:460) and the /info value on the wire (docs/carplay/03_SDK_GROUND_TRUTH.md:69) in every hardware session since 2026-07-12; 24 was removed from the picker by owner directive (SettingsWindow.swift:45) with no recorded session behind the reason — see valueNotes"))
        // CORRECTED 2026-09-04 (audit finding 4). "Both rates negotiated 2026-08-27" over-claimed: the
        // committed table (docs/androidauto/01_SESSION_AND_AV.md:31-38) is per (tier, fps), and its only
        // 2026-08-27 rows are 60→60; every 30→30 row is dated 2026-09-04.
        case (.frameRate, .androidAuto):
            return FeatureSupport(level: .supported, vendorTerm: "framerate (VideoFrameRateType)", vendorKey: "framerate",
                effect: "Declared as 30 or 60 fps — the only two values Android Auto has. Which rate has been run depends on the tier: see the per-tier notes; a (tier, rate) pair outside them has not been negotiated on a phone.",
                verification: .proven("2026-09-04", "per (tier, fps), docs/androidauto/01_SESSION_AND_AV.md:31-38: 60→60 on tiers 1-3 (2026-08-27 / 09-04), 4, 5 and 6; 30→30 on tiers 4, 5, 7, 8 and 9 (all 2026-09-04). AACapability.Resolution.deviceVerified(atFPS:) is the code-side copy of that table"))

        case (.pixelDensity, .carPlay):
            return FeatureSupport(level: .unsupported, vendorTerm: "—", vendorKey: nil,
                effect: "CarPlay has no density field; iOS infers scale from the pixel size.",
                verification: .sourceVerified("2026-09-04", "no density key in Apple's VehicleConfig schema (the Simulator's ten templates) or in the /info displays[] entry (docs/carplay/03_SDK_GROUND_TRUTH.md:69)"))
        case (.pixelDensity, .androidAuto):
            return FeatureSupport(level: .supported, vendorTerm: "dpi", vendorKey: "dpi (SDR VideoConfiguration.density)",
                effect: "Handed to the phone's virtual display; UI scales as dpi/160 while the tier and margins stay fixed.",
                verification: .proven("2026-09-04", "AA_DENSITY override observed to rescale gearhead's UI; the profile field replaces the env var"))

        case (.insets, .carPlay):
            return FeatureSupport(level: .supported, vendorTerm: "viewAreas.safeArea", vendorKey: "videoStreamsConfig.mainVideoStream.viewAreas.safeArea",
                effect: "iOS keeps interactive UI inside the inset box; a non-zero inset arms View Areas automatically.",
                // Date CORRECTED 2026-09-04 (audit finding 8): 2026-08-31 was the doc-consolidation date,
                // not the observation. The hardware run is 2026-07-12.
                verification: .proven("2026-07-12", "hardware: a 100 px L/R inset on the 1920×720 main → viewAreas=true safe=(100,0,1720,720), RECORD reached, video fail=0, 0 teardowns (docs/carplay/04_CAPABILITIES_AND_CONFIG.md:845); vehicle_config.rs:874 view_areas_enabled() auto-arms from a real inset"))
        case (.insets, .androidAuto):
            return FeatureSupport(level: .limited, vendorTerm: "contentinsets / stablecontentinsets", vendorKey: "contentinsets",
                effect: "Android Auto has content insets in its vocabulary, but this app does not send them yet — the value is recorded and ignored for AA.",
                verification: .unverified("DHU parses contentinsets/stablecontentinsets; the SDR field carrying them is not yet identified in AAWire — do not claim support until it is"))

        case (.videoCodec, .carPlay):
            return FeatureSupport(level: .supported, vendorTerm: "enablesHEVC", vendorKey: "accessoryConfig.enablesHEVC",
                effect: "Advertises H.265 support; the box arms its decoder from this.",
                verification: .proven("2026-07-31", "box arms HEVC from the pushed key"))
        case (.videoCodec, .androidAuto):
            return FeatureSupport(level: .limited, vendorTerm: "MEDIA_CODEC_VIDEO_H265", vendorKey: "audiocodec/videocodec (SDR VideoConfiguration.codec)",
                effect: "Tiers above 1080p are H.265-only: with HEVC disallowed the renderer stays at 1080p; below that H.264 is used unless Prefer HEVC is on.",
                verification: .proven("2026-09-04", "all nine tiers streamed with 0 drops at their required codec (Pixel 10 / gearhead 17.5). What is REFUTED is the pairing, not a tier: 2560x1440 declared as H.264 → 'not allowed for the codec type' → 'No working configuration' and a closed transport. gearhead's ivf.B is a policy allowlist over (resolution, codec) — not a codec capability limit"))

        case (.altDisplay, .carPlay):
            return FeatureSupport(level: .supported, vendorTerm: "altVideoStreams", vendorKey: "videoStreamsConfig.altVideoStreams[]",
                effect: "Declares a second stream (cluster/secondary) with its own size, rate and insets.",
                verification: .proven("2026-08", "alt stream arms on the box and the app opens AltVideoWindow"))
        case (.altDisplay, .androidAuto):
            return FeatureSupport(level: .unsupported, vendorTerm: "instrumentcluster / displaytype cluster", vendorKey: nil,
                effect: "Android Auto supports a cluster display; this app does not implement it for AA yet (task T5).",
                // Citation CORRECTED 2026-09-04 (audit finding 8): 01_SESSION_AND_AV.md has no "Phase 4".
                verification: .unverified("docs/ops/08_FUTURE_TASKS.md T5 (raised 2026-09-04) — not started; the carrier is a second MediaSinkService with display_type CLUSTER/AUXILIARY"))

        // ---- Appearance ----
        // CORRECTED 2026-09-04 (audit finding 1). Was `.supported` + "sent live as night mode" — false.
        // Nothing on the CarPlay path reads the stored theme: the Controls window seeds `nightModeOn`
        // and the four appearance flags from ITS OWN UserDefaults keys and sends those. The night-mode
        // command exists (events.rs send_set_night_mode) — the profile value just never feeds it.
        // Only the AA renderer consumes `appearance.theme`.
        case (.theme, .carPlay):
            return FeatureSupport(level: .unsupported, vendorTerm: "setNightMode + uiAppearanceUpdate/mapAppearanceUpdate (runtime, Controls window)", vendorKey: nil,
                effect: "Not applied to CarPlay: the stored theme has no reader on this path. CarPlay night mode and the UI/map appearance come from the Controls window's own live toggles (their own saved state), sent as /command setNightMode and the appearance updates; the two switches below only enable those commands.",
                verification: .sourceVerified("2026-09-04", "grep of every non-Settings, non-AA Swift file finds no reader of appearance.theme; ControlsWindow.swift:445 seeds nightModeOn from its own ApKey.night default and :555 sends it — the profile theme reaches only the AA renderer"))
        case (.theme, .androidAuto):
            return FeatureSupport(level: .limited, vendorTerm: "[sensors] night_mode", vendorKey: "night_mode",
                effect: "Dark = night sensor on, Light = off — the only appearance lever Android Auto has; Auto follows this Mac's appearance.",
                verification: .proven("2026-08-27", "AASession.setNightMode flips gearhead's theme mid-session"))

        case (.statusBar, .carPlay):
            return FeatureSupport(level: .unsupported, vendorTerm: "—", vendorKey: nil,
                effect: "iOS owns the CarPlay status bar; there is no request to hide its elements.",
                verification: .sourceVerified("2026-09-04", "no status-bar hide key in Apple's VehicleConfig schema or the /command vocabulary (docs/carplay/03_SDK_GROUND_TRUTH.md:107)"))
        case (.statusBar, .androidAuto):
            return FeatureSupport(level: .limited, vendorTerm: "hideclock / hidesignal / hidebattery", vendorKey: "hideclock",
                effect: "Android Auto can hide the clock, signal and battery; this app records the choice but does not send it yet.",
                verification: .unverified("DHU config keys exist; the SDR field numbers are unconfirmed — the AA renderer must verify against the DHU binary before emitting"))

        // ---- Driving ----
        // CORRECTED AGAIN 2026-09-05, and this is the interesting one. Two prior passes said CarPlay
        // had no driver-side mechanism at all — 2026-09-02 dropped the key as unused, and 2026-09-04
        // "fixed" the note while keeping `.unsupported`. Both were reasoning from OUR schema instead
        // of Apple's: `rightHandDrive` is an Info Message key, not a VehicleConfig key, so it had no
        // consumer *there* and never would have. Apple R14G17 is explicit — AirPlayCommon.h:1103
        // `#define kAirPlayKey_RightHandDrive "rightHandDrive"` ("[Boolean] Whether or not to use
        // right-hand drive mode"), AirPlayReceiverServer.c:637-646 reads it from config into /info,
        // and the Integration Guide lists it beside oemIconVisible/OSInfo. This project's own
        // docs/carplay/03_SDK_GROUND_TRUTH.md:58 had it in the /info key list the whole time.
        case (.driverPosition, .carPlay):
            return FeatureSupport(level: .supported, vendorTerm: "rightHandDrive", vendorKey: "/info rightHandDrive",
                effect: "Tells iOS the driver sits on the right so CarPlay mirrors driver-focused layout toward that side. Boolean — Apple has no centre value, so Centre is sent as left. Applies on the next connection.",
                verification: .proven("2026-09-05", "DEVICE-VERIFIED on a 2400x960 ultrawide over WIRELESS CarPlay (iPhone / airplayd_wl): with rightHandDrive true, iOS moved the app rail, status bar, clock and app-grid button to the RIGHT edge — owner-confirmed on screen. Closes the same session that landed it: info.rs emits the /info boolean, vehicle_config.rs parses the YAML key, the app derives it from driverPosition. Centre still has no CarPlay reading (Apple's key is boolean) and is sent as left"))
        case (.driverPosition, .androidAuto):
            return FeatureSupport(level: .supported, vendorTerm: "driverposition (gal DriverPosition)", vendorKey: "driverposition (SDR field 6)",
                effect: "Declared at session start; decides which edge the app rail sits on. Takes effect on the next session.",
                verification: .proven("2026-09-04", "1 = RIGHT put the rail right, 2 = LEFT put it left on a Pixel 10; CENTER (3) untested — see valueNotes"))

        case (.drivingRestrictions, .carPlay):
            return FeatureSupport(level: .limited, vendorTerm: "limitedUIConfig", vendorKey: "limitedUIConfig",
                effect: "Keyboard, phone keypad, media lists, other lists and long messages map to CarPlay elements; video, voice and configuration have none. Undeclared = iOS's own default set.",
                verification: .proven("2026-07", "limitedUIElements appear in /info when declared; the on/off is the Controls window's setLimitedUI"))
        case (.drivingRestrictions, .androidAuto):
            return FeatureSupport(level: .limited, vendorTerm: "driving_status (DrivingStatus bitmask)", vendorKey: "[sensors] driving_status",
                effect: "Keyboard/keypad, long messages, video, voice and configuration map to the five bits; list restrictions have none. Sent when restriction is switched on.",
                verification: .proven("2026-08-27", "sending 1 blanked video instead of the keyboard — the value is a bitmask, not a bool; DHU console: restrict video|keyboard|voice|config|message"))

        // ---- Vehicle ----
        case (.powertrain, .carPlay):
            return FeatureSupport(level: .supported, vendorTerm: "engineTypes / chargingConnectors", vendorKey: "iapConfig (Identify param 20)",
                effect: "Built into the wired iAP2 identification on the next phone plug; a rejected identification cannot be retried until replug.",
                // CORRECTED 2026-09-04 (audit finding 8). Was proven 2026-09-02 "C-3". C-3 resolved
                // 2026-09-01 and it is a CODE landing (docs/ops/04_OPEN_ITEMS.md:337): the identity now
                // rides the wire, but no session log records an iPhone accepting an app-authored param 20.
                verification: .unverified("C-3 landed 2026-09-01 as code — ccpa/iap2d/src/main.rs passes vehicle_identity_from(&cfg) into build_ident_info_with at both Identify sites (docs/ops/04_OPEN_ITEMS.md:337-341); only the compiled baseline identity (Gasoline, no connectors) has been through phone sessions, an app-authored powertrain has not been observed accepted"))
        case (.powertrain, .androidAuto):
            return FeatureSupport(level: .limited, vendorTerm: "fueltypes / evconnectors", vendorKey: "fueltypes",
                effect: "Android Auto's vocabulary is coarser (unleaded/biodiesel/electric/other; j1772/chademo/combo/supercharger); this app does not send it yet.",
                verification: .unverified("DHU keys exist; the SDR/sensor carrier is not yet identified in AAWire"))

        // ---- Input ----
        // CORRECTED 2026-09-04 (audit finding 2). Was `.supported` while 9 of the 13 controls in this
        // block are box-inert. The box parses eight of Apple's twenty-one hidConfig keys and ACTS on
        // four; the rest (and the `primaryInput` sibling) are emitted, parsed or not, and read by
        // nothing until C-7/C-8 derive the display-features word (vehicle_config.rs:599-603,
        // :1863-1877 EMITTED_BUT_UNREAD; App/Settings/FieldInfo.swift inertKeys agrees).
        case (.inputDevices, .carPlay):
            return FeatureSupport(level: .limited, vendorTerm: "hidConfig + primaryInput", vendorKey: "videoStreamsConfig.mainVideoStream.hidConfig",
                effect: "Four surfaces reach the wire: D-pad, rotary knob (HID uid-4), telephony buttons (uid-5) and two-finger touch. Touchpad, steering-wheel and media-button support, primaryInput, high-fidelity touch and the knob/touchpad/touchscreen sub-options are pushed but parse-only on the box until it derives the display-features word from them.",
                verification: .proven("2026-08-10", "airplayd/src/main.rs:758-764 arms dPadSupport, knobSupport (uid-4), telephonyButtonsSupport (uid-5) and touchScreenSupportsMultiTouch from the pushed config; vehicle_config.rs:599-603 marks touchpadSupport/steeringWheelSupport/mediaButtonsSupport/touchScreenMode PARSE-ONLY and :1863-1877 lists knobSupportsHomeAndBackButton, knobSupportsNudge, touchpadButtonsSupport, touchScreenSupportsCancel and primaryInput as emitted-but-unread"))
        case (.inputDevices, .androidAuto):
            return FeatureSupport(level: .limited, vendorTerm: "touch / touchpad / controller / inputmode", vendorKey: "inputmode",
                effect: "Declares a touchscreen and the keycodes for buttons; touchpad and rotary render as controller keycodes (D-pad, scroll wheel), not as a pointer surface.",
                verification: .proven("2026-08-27", "InputSourceService keycodes_supported declared; scroll wheel is button code 65536"))

        // ---- Audio ----
        case (.audioProfile, .carPlay):
            return FeatureSupport(level: .limited, vendorTerm: "audio formats (audioFormats / audioType)", vendorKey: "audio",
                effect: "Voice rate and telephony are set through the CarPlay audio format table below; the neutral values are not applied to CarPlay directly.",
                verification: .proven("2026-08", "wired_pcm / wireless_8 presets device-proven; custom rows negotiate"))
        case (.audioProfile, .androidAuto):
            return FeatureSupport(level: .supported, vendorTerm: "MediaSinkService (guidance/system rate, TELEPHONY sink)", vendorKey: "audioconfig",
                effect: "Guidance and system sinks are declared at the voice rate; telephony adds a fourth sink the phone may open.",
                verification: .proven("2026-09-04", "48 kHz guidance/system accepted by gearhead 17.5; the telephony sink is an experiment nobody has seen the phone open"))

        // ---- Data feeds ----
        case (.metadataFeeds, .carPlay):
            return FeatureSupport(level: .limited, vendorTerm: "iAP2 Identify params 6/7 (metadata tier)", vendorKey: "metadata",
                effect: "The tier picker below governs what is declared; the three feed switches are advisory for CarPlay until the skip list maps onto them.",
                verification: .proven("2026-08", "proven tier byte-equivalent to the box's compiled floor; rx-only refuted — see valueNotes"))
        case (.metadataFeeds, .androidAuto):
            return FeatureSupport(level: .supported, vendorTerm: "media_playback / navigation_status / phone_status services", vendorKey: "playbackstatus / navstatusconfig / phonecluster",
                effect: "Each switch declares one service descriptor; the phone opens the channel and streams the feed.",
                verification: .proven("2026-09-04", "all three descriptors accepted by a Pixel 10 / gearhead 17.5 and channels opened"))

        // ---- Adapter ----
        case (.wirelessRadios, .carPlay):
            return FeatureSupport(level: .supported, vendorTerm: "wireless (project key)", vendorKey: "wireless",
                effect: "Advertises for wireless CarPlay (Bluetooth pairing + Wi-Fi hand-off) while the app is connected.",
                verification: .proven("2026-08-11", "session_supervisor.sh wireless_up()"))
        case (.wirelessRadios, .androidAuto):
            return FeatureSupport(level: .supported, vendorTerm: "wireless (project key)", vendorKey: "wireless",
                effect: "The SAME switch: wireless Android Auto is armed inside the wireless bring-up, so off here also disables wireless AA.",
                verification: .sourceVerified("2026-09-04", "tools/session_supervisor.sh wireless_up() returns at :857-861 on wireless:false, before the arm_aa_wireless call at :1002; one carplay-wireless daemon serves both SDP records"))

        case (.hotHandover, .carPlay):
            return FeatureSupport(level: .supported, vendorTerm: "hot_handover (project key)", vendorKey: "hot_handover",
                effect: "Forces a live wireless→wired switch on cable insert (non-standard; Apple never migrates a session).",
                // Citation CORRECTED 2026-09-04 (audit finding 8): docs/ops/05_AUDITS.md has no such content.
                verification: .proven("2026-08-01", "commit 587342a: the live wireless→wired switch hardware-validated and gated behind hot_handover: true (session_supervisor.sh:757, preempt at :1234); docs/wireless/01_BT_AND_RADIO.md:563-564, docs/androidauto/02_ARBITRATION.md:54-55, docs/androidauto/03_WIRELESS.md:483"))
        case (.hotHandover, .androidAuto):
            return FeatureSupport(level: .unsupported, vendorTerm: "—", vendorKey: nil,
                effect: "No wireless→wired hand-over path exists for Android Auto in the supervisor.",
                verification: .sourceVerified("2026-09-04", "tools/session_supervisor.sh: arm_aa_wireless (:367) and the wired aa-bridge arm are separate; the only preempt (:1234) is gated on wired_iphone_on_usb, so no AA migration path exists"))

        case (.pairing, .carPlay):
            return FeatureSupport(level: .supported, vendorTerm: "pairing (project key)", vendorKey: "pairing",
                effect: "Just-Works (proven) or Numeric Comparison (both sides show a 6-digit code).",
                verification: .proven("2026-08", "Just-Works is the shipped posture; Numeric Comparison experimental"))
        case (.pairing, .androidAuto):
            return FeatureSupport(level: .limited, vendorTerm: "Bluetooth RFCOMM credential hand-off", vendorKey: "pairing",
                effect: "Wireless Android Auto pairs over the same Bluetooth radio; whether the association model is honoured for it is unverified.",
                verification: .unverified("no wireless-AA pairing observation recorded with Numeric Comparison on"))

        case (.wifiAccessPoint, .carPlay), (.wifiAccessPoint, .androidAuto):
            return FeatureSupport(level: .supported, vendorTerm: "wifi_ap (project key)", vendorKey: "wifi_ap",
                effect: "Off = the box is Bluetooth + MFi coprocessor only and the vehicle's hotspot carries the session (the gm_ccpa bridge role).",
                verification: .proven("2026-08", "session_supervisor.sh wifi_ap_enabled(); bt_on.sh is independent of wlan_on.sh"))

        case (.androidAutoProjection, .carPlay):
            return FeatureSupport(level: .unsupported, vendorTerm: "—", vendorKey: nil,
                effect: "Does not affect CarPlay; an iPhone always takes the CarPlay path first.",
                verification: .proven("2026-08-25", "docs/androidauto/02_ARBITRATION.md"))
        case (.androidAutoProjection, .androidAuto):
            return FeatureSupport(level: .supported, vendorTerm: "android_auto (project key)", vendorKey: "android_auto",
                effect: "The box switches an Android phone to accessory mode and pumps the stream to this app; off = the phone only charges.",
                verification: .proven("2026-08-27", "wired AA sessions on a Pixel 10"))

        case (.appDrivenSetup, .carPlay):
            return FeatureSupport(level: .supported, vendorTerm: "accessoryConfig.appDrivenSetup (project key)", vendorKey: "accessoryConfig.appDrivenSetup",
                effect: "The box relays RTSP/SETUP to this app, which authors the response; the box's local response is the fallback.",
                verification: .proven("2026-08-10", "default ON on both transports"))
        case (.appDrivenSetup, .androidAuto):
            return FeatureSupport(level: .unsupported, vendorTerm: "—", vendorKey: nil,
                effect: "Android Auto has no SETUP negotiation; the head-unit engine already runs in this app.",
                verification: .sourceVerified("2026-08-27", "AA service discovery is authored in-process (AAWire.swift serviceDiscoveryResponseFull); there is no box-side AA response to relay"))
        }
    }
    // swiftlint:enable function_body_length

    // MARK: Value-level provenance

    /// Verification that depends on the VALUE, not just the feature: which tiers, positions and tiers
    /// of metadata have actually been seen to work. `value` is the neutral value's string form as
    /// the UI presents it (e.g. "2560x1440", "center", "rx-only").
    struct ValueNote: Sendable, Equatable {
        let feature: Feature
        let projection: Projection
        let value: String
        let verification: Verification
    }

    static let valueNotes: [ValueNote] = [
        // AA resolution tiers (AACapability.Resolution).
        // Per-(tier, fps) precision added 2026-09-04 (audit finding 3). The committed table
        // (docs/androidauto/01_SESSION_AND_AV.md:31-38) is keyed by tier AND rate, and only tiers 4
        // and 5 were run at both. A note that says "proven" for a tier without naming the rate lets
        // a profile asking 1080×1920@60 look verified when nobody has run that pairing. Each note
        // now states the rate(s) run and the rate(s) NOT run; `AACapability.Resolution
        // .deviceVerified(atFPS:)` is the code-side copy of the same table and the renderer's
        // per-negotiation note reads from it — keep the two in step.
        ValueNote(feature: .panelGeometry, projection: .androidAuto, value: "800x480",
                  verification: .proven("2026-08-27", "tier 1 as H.264, 60→60 (2026-08-27 / 09-04) — the reference tier; 30 fps has not been run at this tier")),
        ValueNote(feature: .panelGeometry, projection: .androidAuto, value: "1280x720",
                  verification: .proven("2026-08-27", "tier 2 as H.264, 60→60 (2026-08-27 / 09-04): Pixel 10 accepted and sent 1280×720 video; 30 fps has not been run at this tier")),
        ValueNote(feature: .panelGeometry, projection: .androidAuto, value: "1920x1080",
                  verification: .proven("2026-08-27", "tier 3 as H.264, 60→60 (2026-08-27 / 09-04): AA_FORCE_RES=1080, clean 1920x1080 at slotDrops=0; 30 fps has not been run at this tier")),
        // Tiers 4-9 corrected 2026-09-04 (integrator): they were carried here as unverified/refuted
        // from AACapability.swift's older comment, which only credited the 2026-08-27 run. The
        // committed evidence table in docs/androidauto/01_SESSION_AND_AV.md records ALL NINE tiers
        // device-verified 2026-09-04, one at a time via AA_FORCE_RES/AA_FORCE_FPS, phone-side
        // decision read from CAR.VIDEO. Marking a proven tier unverified is not a safe default: it
        // makes the renderer emit a "not verified" negotiation note on a tier the owner has seen
        // stream, which teaches them to ignore the notes that matter.
        ValueNote(feature: .panelGeometry, projection: .androidAuto, value: "2560x1440",
                  verification: .proven("2026-09-04", "tier 4 as H.265: BOTH rates run, 30→30 and 60→60, streaming, 0 drops. As H.264 it is REFUSED ('not allowed for the codec type') — the codec pairing is the constraint, not the tier")),
        ValueNote(feature: .panelGeometry, projection: .androidAuto, value: "3840x2160",
                  verification: .proven("2026-09-04", "tier 5 as H.265: BOTH rates run, 30→30 and 60→60, streaming, 0 drops. The DHU's .ini parser spells this slot '3840x1260' while its protobuf enum is VIDEO_3840x2160 — the ini string is a Google typo and the enum is what goes on the wire")),
        ValueNote(feature: .panelGeometry, projection: .androidAuto, value: "720x1280",
                  verification: .proven("2026-09-04", "portrait tier 6 as H.264, 60→60 only, streaming; 30 fps has not been run at this tier")),
        ValueNote(feature: .panelGeometry, projection: .androidAuto, value: "1080x1920",
                  verification: .proven("2026-09-04", "portrait tier 7 as H.264, 30→30 only; owner-confirmed scaling and touch. 60 fps has NOT been run at this tier — 1080×1920@60 is a pairing nobody has negotiated")),
        ValueNote(feature: .panelGeometry, projection: .androidAuto, value: "1440x2560",
                  verification: .proven("2026-09-04", "portrait tier 8 as H.265, 30→30 only, streaming, 0 drops; 60 fps has not been run at this tier")),
        ValueNote(feature: .panelGeometry, projection: .androidAuto, value: "2160x3840",
                  verification: .proven("2026-09-04", "portrait tier 9 as H.265, 30→30 only, streaming, 0 drops; 60 fps has not been run at this tier")),

        // AA driver position (AACapability.driverPosition).
        ValueNote(feature: .driverPosition, projection: .androidAuto, value: "left",
                  verification: .proven("2026-09-04", "wire 2 → app rail on the left edge (Pixel 10 / gearhead 17.5)")),
        ValueNote(feature: .driverPosition, projection: .androidAuto, value: "right",
                  verification: .proven("2026-09-04", "wire 1 → app rail on the right edge")),
        ValueNote(feature: .driverPosition, projection: .androidAuto, value: "center",
                  verification: .unverified("wire 3 (CENTER) never declared; DHU accepts 'center' in its config")),

        // AA frame rate. "30" date CORRECTED 2026-09-04 (audit finding 4): the 2026-08-27 rows of the
        // evidence table are all 60→60; the first DATED 30→30 is 2026-09-04. 30 was the hardcoded
        // constant before commit 57501aa (2026-08-27), so earlier tier-1 sessions ran at it
        // (docs/androidauto/00_ARCHITECTURE.md:94 "30 fps"), but no dated record of one exists.
        ValueNote(feature: .frameRate, projection: .androidAuto, value: "60",
                  verification: .proven("2026-08-27", "VideoFrameRateType 1 (60): 60→60 on tiers 1-3 (2026-08-27 / 09-04) and 4, 5, 6 (2026-09-04); NOT run on portrait tiers 7, 8, 9 — docs/androidauto/01_SESSION_AND_AV.md:31-38")),
        ValueNote(feature: .frameRate, projection: .androidAuto, value: "30",
                  verification: .proven("2026-09-04", "VideoFrameRateType 2 (30): 30→30 on tiers 4, 5, 7, 8, 9 (2026-09-04, docs/androidauto/01_SESSION_AND_AV.md:31-38) AND on tier 1 (800x480) as the pre-57501aa hardcoded baseline — AASession declared videoRes 1 / videoFps 2 and ran working sessions on it before 2026-08-27, which is why absence from the AA_FORCE_RES sweep table is not absence of evidence. Not run at 30 on tiers 2, 3, 6")),

        // CarPlay frame rate (audit finding 4): the feature row is proven on 60; 30 is a picker value
        // with no CarPlay session behind it. Kept per-value so the badge does not vouch for both.
        ValueNote(feature: .frameRate, projection: .carPlay, value: "60",
                  verification: .proven("2026-07-12", "the default maxFPS (SettingsWindow.swift:460) and the /info displays[].maxFPS on the wire (docs/carplay/03_SDK_GROUND_TRUTH.md:69) in every hardware session since")),
        ValueNote(feature: .frameRate, projection: .carPlay, value: "30",
                  verification: .unverified("offered by the picker (SettingsWindow.swift frameRates = [30, 60]); no CarPlay session with maxFPS 30 is recorded in docs/carplay/")),

        // CarPlay metadata tiers (docs/carplay/05_METADATA_AND_CONTROLS.md).
        ValueNote(feature: .metadataFeeds, projection: .carPlay, value: "proven",
                  verification: .proven("2026-08", "byte-equivalent to the box's compiled floor")),
        // CORRECTED 2026-09-04. Both were carried here as `unverified`, contradicting the committed
        // evidence in docs/carplay/05_METADATA_AND_CONTROLS.md — and for `all` that was the more
        // dangerous direction of the two. Downgrading a REFUTED value to "unverified" drops the
        // do-not-retry guard on a setting whose failure mode is unrecoverable: an iOS `0x1D03`
        // identification rejection cannot be retried within a session (params 6/7 are un-strippable,
        // so the retry is byte-identical and the second reject aborts), which means CarPlay is dead
        // until the phone is physically replugged. A badge reading "unverified" invites exactly the
        // experiment the evidence already says kills the session.
        ValueNote(feature: .metadataFeeds, projection: .carPlay, value: "extended",
                  // Wording CORRECTED 2026-09-04 (F3, corrections ledger R-47-2): the earlier form here
                  // said `extended` was "rejected on the WIRED arm". The only recorded `extended`
                  // rejection (05 §6.1, the pre-Stop-id form) was a TUNNEL session — every §6 session
                  // is (05:469-475). There is no record of `extended` ever being run on the wired arm.
                  // "Never run there" is an open experiment with a known blast radius; "rejected there"
                  // would tell the owner not to try — different decisions, so the distinction matters.
                  verification: .proven("2026-08-10", "device-accepted on the AirPlayTunnel arm twice: 2026-07-25 (§6.6, ~340 B) and re-measured 2026-08-10 (~342 B, 0x1D02 accepted, docs/carplay/05_METADATA_AND_CONTROLS.md:189-190). NOT YET EXERCISED on the WIRED Identify — no record of it running there, accepted or rejected; raise it there one step at a time and watch `idevicesyslog -p accessoryd`, because a 0x1D03 is unrecoverable until replug")),
        ValueNote(feature: .metadataFeeds, projection: .carPlay, value: "all",
                  verification: .refuted("2026-08-10", "REFUTED on the AirPlayTunnel arm: iOS rejected the full table (voice_over_cursor ids) and iterative skipping was shown to be a dead end. A rejected identification kills CarPlay for that connection and cannot be retried until the phone is replugged — do not re-try without new evidence")),
        ValueNote(feature: .metadataFeeds, projection: .carPlay, value: "rx-only",
                  verification: .refuted("2026-08", "docs/carplay/05_METADATA_AND_CONTROLS.md §6.2 — the box refuses it even if hand-authored; not offered")),

        // Audio voice rate (AACapability.voiceSinkRate).
        ValueNote(feature: .audioProfile, projection: .androidAuto, value: "48000",
                  verification: .proven("2026-09-04", "guidance + system initialised at 48000 mono by gearhead 17.5")),
        ValueNote(feature: .audioProfile, projection: .androidAuto, value: "16000",
                  verification: .proven("2026-08-27", "the 2016 integration-guide floor; the original default")),
        ValueNote(feature: .audioProfile, projection: .androidAuto, value: "24000",
                  verification: .unverified("accepted by the lever's validation; never negotiated")),
    ]

    static func valueNotes(for f: Feature, on p: Projection) -> [ValueNote] {
        valueNotes.filter { $0.feature == f && $0.projection == p }
    }

    static func valueNote(for f: Feature, on p: Projection, value: String) -> ValueNote? {
        valueNotes.first { $0.feature == f && $0.projection == p && $0.value == value }
    }

    // MARK: Driving-restriction derivation

    /// How each neutral restriction renders per vendor — the ONE table both renderers derive from,
    /// so CarPlay's `limitedUIConfig` and AA's `driving_status` cannot drift apart again.
    /// `carPlayElement`: the `limitedUIConfig` YAML key (nil = Apple has no element).
    /// `androidAutoBit`: the `DrivingStatus` bit (nil = Google has no bit). Values are gal's:
    /// NO_VIDEO=1, NO_KEYBOARD_INPUT=2, NO_VOICE_INPUT=4, NO_CONFIG=8, LIMIT_MESSAGE_LEN=16 —
    /// matching `AACapability.DrivingRestrictions` and the DHU console's five `restrict` verbs.
    struct RestrictionMapping: Sendable, Equatable {
        let restriction: DrivingRestrictionSet
        let title: String
        let carPlayElement: String?
        let androidAutoBit: UInt32?
    }

    static let restrictionMapping: [RestrictionMapping] = [
        RestrictionMapping(restriction: .keyboard,      title: "On-screen keyboard",   carPlayElement: "softKeyboard",    androidAutoBit: 2),
        RestrictionMapping(restriction: .phoneKeypad,   title: "Phone keypad",         carPlayElement: "softPhoneKeypad", androidAutoBit: 2),
        RestrictionMapping(restriction: .mediaLists,    title: "Long media lists",     carPlayElement: "musicLists",      androidAutoBit: nil),
        RestrictionMapping(restriction: .otherLists,    title: "Other long lists",     carPlayElement: "nonMusicLists",   androidAutoBit: nil),
        RestrictionMapping(restriction: .longMessages,  title: "Long messages/alerts", carPlayElement: "longAlerts",      androidAutoBit: 16),
        RestrictionMapping(restriction: .video,         title: "Video",                carPlayElement: nil,               androidAutoBit: 1),
        RestrictionMapping(restriction: .voiceInput,    title: "Voice input",          carPlayElement: nil,               androidAutoBit: 4),
        RestrictionMapping(restriction: .configuration, title: "Configuration screens", carPlayElement: nil,              androidAutoBit: 8),
    ]

    /// `limitedUIConfig` keys set true for a neutral set, in the YAML emitter's order.
    static func carPlayLimitedUIElements(_ set: DrivingRestrictionSet) -> [String] {
        restrictionMapping.compactMap { set.contains($0.restriction) ? $0.carPlayElement : nil }
    }

    /// The `driving_status` bitmask for a neutral set.
    static func androidAutoDrivingStatus(_ set: DrivingRestrictionSet) -> UInt32 {
        restrictionMapping.reduce(0) { acc, m in
            guard set.contains(m.restriction), let bit = m.androidAutoBit else { return acc }
            return acc | bit
        }
    }

    /// Which neutral members a projection cannot express — for the "ignored on X" caption.
    static func unexpressedRestrictions(_ set: DrivingRestrictionSet, on p: Projection) -> [RestrictionMapping] {
        restrictionMapping.filter { m in
            guard set.contains(m.restriction) else { return false }
            switch p {
            case .carPlay:     return m.carPlayElement == nil
            case .androidAuto: return m.androidAutoBit == nil
            }
        }
    }
}
