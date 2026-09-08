// VehicleConfigModel+Profile.swift — the bridge between the persisted, CarPlay-shaped
// `VehicleConfigModel` (App/SettingsWindow.swift) and the protocol-NEUTRAL `VehicleProfile` /
// `AdapterSettings` / `VehicleProfileDocument` (App/Settings/VehicleProfile.swift).
//
// WHY A BRIDGE AND NOT A REWRITE (DESIGN.md §0.5 / §2, 2026-09-04). The owner's decision is that
// the neutral profile is the SOURCE and Apple's YAML is a rendered artifact of it — but the model
// that renders that YAML is ~80 `@Published` fields persisted under `vc.*` UserDefaults keys, its
// emitter text is anchored VERBATIM by `tools/regen_app_yaml_fixture.py`, and every existing
// installation has a saved config in those keys. Replacing the store would (a) break the fixture
// anchors, (b) need a migration of every key, and (c) risk the pushed document — which this project
// has already lost once to a single unescaped quote (docs/carplay/04 B3). So the model STAYS the
// observable store, and this extension does two things and nothing else:
//
//   * `profile` / `adapterSettings` / `document` MATERIALIZE the neutral value from the live fields.
//     Pure reads. The emitter keeps reading the same fields it reads today, so the YAML is
//     byte-identical for every existing configuration (§2 drift guard) — this file never touches it.
//   * `apply(_:)` / `apply(preset:)` write a document BACK into the fields (Import, Presets). They
//     mark the form dirty and do NOT save: the user still presses Save, the same as any other edit,
//     so nothing reaches the box that the user has not seen in the form first. That is the same
//     contract the form itself has (`committedYAML` is what is pushed, not the live fields).
//
// The two directions are written as mirror images, field for field, so a reviewer can check them
// side by side. Where the mapping is NOT a bijection it is called out inline with the reason; the
// three lossy spots are the alt-stream DPI (no model field), the `.touchpad`-vs-`.touchscreen`
// primary-input distinction (Apple has no "Touchscreen" primaryInput, DESIGN.md §8), and connector
// power ratings of 0 (folded to "unrated", per `ConnectorRow`'s own comment).
//
// House rule: NO `import AppKit` / `SwiftUI` here. `VehicleConfigModel` itself lives in a SwiftUI
// file, so this file cannot compile in the hardware-free test harness anyway, but keeping it
// Foundation-only means the day the model is split out this bridge comes along for free.

import Foundation

extension VehicleConfigModel {

    // MARK: - Restriction table (DESIGN.md §8, both directions)

    /// One row per `DrivingRestrictionSet` member: the model Bool that stores it. The FIRST five are
    /// Apple's `limitedUIConfig` elements (the model has stored them for a long time under their
    /// Apple names, and the emitter reads them by those names — they cannot be renamed without
    /// touching the anchored `limitedUIFields()`); the LAST three are the AA-only members Phase 0
    /// added as `restrict*`. The membership and the Apple element names are the same facts
    /// `FeatureMatrix.restrictionMapping` carries (`carPlayElement` / `androidAutoBit`); this table
    /// adds only the storage location, which the matrix deliberately does not know about.
    ///
    /// Apple's other five `limitedUIConfig` keys (`japanMaps`, `pairedDevices`, `themeCustomization`,
    /// `automakerSettings`, `automakerSettingsInfoButton`) have NO neutral reading (DESIGN.md §10) and
    /// travel in `CarPlayExtensions.limitedUI*` instead — see `carPlayExtensions` below.
    private static let restrictionFields: [(member: DrivingRestrictionSet, field: ReferenceWritableKeyPath<VehicleConfigModel, Bool>)] = [
        (.keyboard,      \.limitedUISoftKeyboard),
        (.phoneKeypad,   \.limitedUISoftPhoneKeypad),
        (.mediaLists,    \.limitedUIMusicLists),
        (.otherLists,    \.limitedUINonMusicLists),
        (.longMessages,  \.limitedUILongAlerts),
        (.video,         \.restrictVideo),
        (.voiceInput,    \.restrictVoiceInput),
        (.configuration, \.restrictConfiguration),
    ]

    /// The 14 `enables*` accessory flags other than HEVC (HEVC is the neutral `VideoCodecPolicy`),
    /// in the same order as `CarPlayExtensions.AccessoryFlags` declares them. `appDrivenSetup` is
    /// NOT here: it is an adapter behaviour, not a CarPlay capability (`AdapterSettings.appDrivenSetup`).
    private static let accessoryFlagFields: [(flag: WritableKeyPath<CarPlayExtensions.AccessoryFlags, Bool>, field: ReferenceWritableKeyPath<VehicleConfigModel, Bool>)] = [
        (\.enablesMainBufferedAudio,   \.enablesMainBufferedAudio),
        (\.enablesUIAppearance,        \.enablesUIAppearance),
        (\.enablesMapAppearance,       \.enablesMapAppearance),
        (\.enablesCornerMasks,         \.enablesCornerMasks),
        (\.enablesVideoPlayback,       \.enablesVideoPlayback),
        (\.enablesViewAreas,           \.enablesViewAreas),
        (\.enablesEnhancedSiri,        \.enablesEnhancedSiri),
        (\.enablesFocusTransfer,       \.enablesFocusTransfer),
        (\.enablesUIContext,           \.enablesUIContext),
        (\.enablesUISync,              \.enablesUISync),
        (\.enablesFileTransfer,        \.enablesFileTransfer),
        (\.enablesLogTransfer,         \.enablesLogTransfer),
        (\.enablesVehicleDataProtocol, \.enablesVehicleDataProtocol),
        (\.enablesDCX,                 \.enablesDCX),
    ]

    // MARK: - Materialize (model → neutral)

    /// The neutral vehicle profile, materialized from the live `@Published` fields. A fresh value on
    /// every read — cheap (a few dozen scalar copies plus the OEM icon base64 string, which is
    /// shared storage, not copied), and it means the profile can never go stale relative to the form.
    ///
    /// `driverPosition` / `appearance.theme` come from the Phase 0 neutral fields, NOT from the legacy
    /// `rightHandDrive` / `nightMode` booleans. Those two are derived ON WRITE ONLY — `apply` and the
    /// pickers set them from the neutral value, and `save()` keeps writing them so a downgrade still
    /// reads sane values (DESIGN.md §5). On LOAD they are NOT derived: `init` reads both keys
    /// independently, and the one-shot `vc.profileKeysV1` migration never re-runs, so a Bool an
    /// older build flipped after a downgrade is not reflected here until `init` reconciles the
    /// pair (`DriverPosition.reconciled(stored:legacyRightHandDrive:)` /
    /// `AppearanceTheme.reconciled(stored:legacyNightMode:)`, finding 5, 2026-09-04). An unknown
    /// persisted spelling falls back to the shipped default — `init` already validates both keys, so
    /// this is belt-and-braces for a tampered defaults domain, not a path that runs in practice.
    var profile: VehicleProfile {
        var v = VehicleProfile.default

        v.identity.headUnitName = name

        // Branding: `oemIconEnabled` is "advertise oemIconConfig at all"; the PNG is optional even
        // when advertising (the emitter has its own "emit nothing without a PNG" guard). An empty
        // base64 string is the model's "no icon" — the profile makes that an explicit nil so a
        // document never carries a 0×0 icon with an empty payload.
        v.branding.advertise = oemIconEnabled
        v.branding.visible = oemIconVisible
        v.branding.label = oemIconLabel
        v.branding.icon = oemIconBase64.isEmpty ? nil
            : BrandIcon(pngBase64: oemIconBase64, width: oemIconW, height: oemIconH)

        // Main display. `diagonalInches == 0` is the model's "unknown" (UserDefaults has no nil).
        v.display.panel = PanelGeometry(width: mainWidth, height: mainHeight, maxFPS: maxFPS, dpi: dpi,
                                        diagonalInches: diagonalInches > 0 ? diagonalInches : nil)
        v.display.insets = PanelInsets(left: mainSafeLeft, top: mainSafeTop, right: mainSafeRight, bottom: mainSafeBottom)
        v.display.drawUIOutsideInsets = mainDrawOutsideSafe

        // Alt (cluster) stream. LOSSY: the model has no alt-stream DPI or diagonal, so the alt panel
        // carries `PanelGeometry`'s defaults for both (160 / nil). Nothing on either wire reads them
        // for the cluster today, so nothing is lost on the wire — only in the document.
        v.altDisplay.enabled = altVideoEnabled
        v.altDisplay.panel = PanelGeometry(width: altWidth, height: altHeight, maxFPS: altFPS)
        v.altDisplay.insets = PanelInsets(left: altSafeLeft, top: altSafeTop, right: altSafeRight, bottom: altSafeBottom)
        v.altDisplay.drawUIOutsideInsets = altDrawOutsideSafe

        v.video.hevcAllowed = enablesHEVC

        v.appearance.theme = AppearanceTheme(rawValue: theme) ?? .light
        v.appearance.statusBar = StatusBarPolicy(hideClock: hideClock, hideSignal: hideSignal, hideBattery: hideBattery)

        v.driverPosition = DriverPosition(rawValue: driverPosition) ?? .left

        // Restrictions: `declared` is the NEUTRAL "declare a restriction set at all" switch. Its
        // storage is `limitedUIConfigEnabled` — an Apple-named key because the model stored it long
        // before the profile existed and `save()`/the emitter read it by that name — but it is not
        // a CarPlay control: `VehicleTab` presents it as "Declare restrictions" with no badge, the
        // AA-only members (`restrictVideo` / `restrictVoiceInput` / `restrictConfiguration`) are
        // shown under it, and BOTH renderers gate on it (CarPlay: emit `limitedUIConfig`; AA:
        // `AACapability+Profile` swaps `drivingDefault` for the mapped set). Decision 2026-09-04
        // (finding 6): the gating STAYS. Ungating the AA members would let a single tick declare
        // NO_VIDEO, which blanks the projection; today that takes two deliberate switches
        // ("Declare restrictions" + the member), the safety property the review asked to keep.
        // The set is assembled from the table above; the AA renderer reduces it to its 5-bit mask
        // with `FeatureMatrix.androidAutoDrivingStatus`.
        v.restrictions.declared = limitedUIConfigEnabled
        v.restrictions.set = Self.restrictionFields.reduce(into: DrivingRestrictionSet()) { acc, row in
            if self[keyPath: row.field] { acc.insert(row.member) }
        }

        // Powertrain. `engineTypes` is a Set<String> of Apple's tokens (which the neutral enum uses
        // verbatim); emit in `EngineType.allCases` order so two models with the same set produce the
        // same document bytes. Unknown strings are dropped rather than crashing — the Set is
        // user-editable only through the pickers, so this is defensive, not expected.
        v.powertrain.engines = EngineType.allCases.filter { engineTypes.contains($0.rawValue) }
        // Connectors keep the model's order AND duplicates: the emitter dedupes for the wire
        // (`dedupedConnectors()`), but the document should show what the user authored. A rating of
        // 0 is folded to "unrated" — `ConnectorRow`'s own comment says nil/0 = omit the power sub,
        // and an absent rating must not become a zero one in a document either.
        v.powertrain.connectors = chargingConnectors.compactMap { row in
            guard let type = ChargingConnector(rawValue: row.type) else { return nil }
            return ConnectorSpec(type: type, powerWatts: (row.powerWatts ?? 0) > 0 ? row.powerWatts : nil)
        }

        // Input. `touchScreenHighFidelity` is the model's ONLY "a touchscreen exists" switch (the
        // emitter renders it as `touchScreenMode: High Fidelty` vs `Disabled`), so presence maps to
        // it; the per-surface Bools become the optional sub-structs. Primary input reverse mapping
        // is DESIGN.md §8 verbatim: Apple has no "Touchscreen" primaryInput, so `"Touchpad"` means
        // "the touchpad" only when there IS a touchpad and NO touchscreen; otherwise it is the
        // touchscreen (the Apple templates all say Touchpad with a touchscreen present).
        v.input.primary = {
            switch primaryInput {
            case "Knobs": return .rotary
            default:      return (touchpadSupport && !touchScreenHighFidelity) ? .touchpad : .touchscreen
            }
        }()
        v.input.touchscreen = touchScreenHighFidelity
            ? Touchscreen(multiTouch: touchScreenSupportsMultiTouch, supportsCancel: touchScreenSupportsCancel) : nil
        v.input.touchpad = touchpadSupport ? Touchpad(buttons: touchpadButtonsSupport) : nil
        v.input.rotaryKnob = knobSupport
            ? RotaryKnob(homeAndBackButtons: knobSupportsHomeAndBackButton, nudge: knobSupportsNudge) : nil
        v.input.dPad = dPadSupport
        v.input.mediaButtons = mediaButtonsSupport
        v.input.telephonyButtons = telephonyButtonsSupport
        v.input.steeringWheelButtons = steeringWheelSupport

        v.audio = AudioProfile(voiceRateHz: voiceRateHz, telephonyOverProjection: telephonyOverProjection)
        v.metadata = MetadataFeeds(nowPlaying: metadataNowPlaying, navigation: metadataNavigation, telephony: metadataTelephony)

        v.carPlay = carPlayExtensions
        v.androidAuto = AndroidAutoExtensions(fitPanelWithMargins: aaFitPanelWithMargins, preferHEVC: aaPreferHEVC)
        return v
    }

    /// The CarPlay-exclusive block: vocabulary that has no neutral reading and is rendered as badged
    /// sub-groups inside the feature it belongs to (DESIGN.md §0.4). Values are copied verbatim —
    /// `init` already validates `audioMode` / `metadataTier` against the model's own lists.
    /// `vehicleStatusCaps` is a Set in the model; it is SORTED here so the document is deterministic
    /// (§0.2: equal documents are byte-equal), the emitter applies its own canonical order anyway.
    private var carPlayExtensions: CarPlayExtensions {
        var cp = CarPlayExtensions()
        cp.accessoryName = accessoryName
        cp.audioMode = audioMode
        cp.audioFormats = audioFormats.map {
            CarPlayExtensions.AudioFormat(streamType: $0.streamType, audioType: $0.audioType, input: $0.input, output: $0.output)
        }
        cp.metadataTier = metadataTier
        cp.metadataSkip = metadataSkip
        cp.vehicleStatusEnabled = vehicleStatusEnabled
        cp.vehicleStatusCaps = vehicleStatusCaps.sorted()
        for row in Self.accessoryFlagFields {
            cp.accessoryFlags[keyPath: row.flag] = self[keyPath: row.field]
        }
        cp.limitedUIJapanMaps = limitedUIJapanMaps
        cp.limitedUIPairedDevices = limitedUIPairedDevices
        cp.limitedUIThemeCustomization = limitedUIThemeCustomization
        cp.limitedUIAutomakerSettings = limitedUIAutomakerSettings
        cp.limitedUIAutomakerSettingsInfoButton = limitedUIAutomakerSettingsInfoButton
        return cp
    }

    /// Box / radio / projection enables — the seven top-level keys of the pushed YAML that are this
    /// project's, not Apple's (`wireless`, `hot_handover`, `pairing`, `android_auto`, `wifi_ap`,
    /// `appDrivenSetup`). Their model names are the pre-existing ones (`wirelessEnabled`,
    /// `androidAutoEnabled`), kept because the emitter and `save()` read them by those names.
    var adapterSettings: AdapterSettings {
        AdapterSettings(wirelessRadios: wirelessEnabled,
                        hotHandover: hotHandover,
                        pairingNumericComparison: pairingNumericComparison,
                        pairingInteractiveAnswer: pairingInteractiveAnswer,
                        wifiAccessPoint: wifiAccessPoint,
                        androidAuto: androidAutoEnabled,
                        appDrivenSetup: appDrivenSetup)
    }

    /// The exportable document: current profile + adapter settings, `preset: nil`. A document
    /// exported from the form is the user's, even if it happens to equal a built-in preset — the
    /// `preset` id is set only by `apply(preset:)`'s input, never inferred, so Export never claims a
    /// provenance the user did not choose.
    var document: VehicleProfileDocument {
        VehicleProfileDocument(preset: nil, vehicle: profile, adapter: adapterSettings)
    }

    // MARK: - Apply (neutral → model)

    /// Write a document into the live fields. Marks the form dirty; does NOT save and does NOT touch
    /// `committedYAML` — the box sees nothing until the user presses Save, exactly as for a manual
    /// edit. Every field the document can express is written (including back to its default when
    /// the document is silent — `VehicleProfileDocument.decode` already filled missing keys with
    /// defaults), so after `apply(doc)` the model's state is a function of `doc` alone, not of what
    /// was in the form before. That is what makes Presets predictable: loading `dhu-default` twice
    /// gives the same form both times.
    ///
    /// Ends with `clampInPlace()` (DESIGN.md §8): a preset below the 800×480 floor (`dhu-6in`,
    /// 750×450) is clamped on load rather than accepted, because the floor is the box's vocabulary,
    /// not a UI preference — and it is applied to the MODEL, so it binds BOTH renderers, not just
    /// CarPlay (after load `dhu-6in` is `dhu-default` plus `diagonalInches: 6.0`; the preset's
    /// summary says so). The caller that wants to tell the user about it (W4's document view)
    /// compares the imported profile with `model.profile` after the call via
    /// `doc.vehicle.normalizationChanges(after: model.profile)`, which inspects the alt display
    /// whether or not it is enabled — `clampInPlace()` clamps `altWidth`/`altHeight`/`altFPS`
    /// regardless of `altVideoEnabled`, so a disabled 100×100 alt panel silently re-exports as
    /// 800×480 unless the summary looks (finding 4, 2026-09-04). The signature deliberately returns
    /// nothing so W3/W4's call sites do not depend on a notice type that may grow.
    ///
    /// Field writes go through the `@Published` setters one at a time, so `objectWillChange` fires
    /// ~80 times; SwiftUI coalesces per run-loop turn and this only happens on an explicit Import /
    /// Preset click, so batching is not worth the private `loading` dance `resetToDefault` does.
    func apply(_ doc: VehicleProfileDocument) {
        let v = doc.vehicle
        let a = doc.adapter

        // ---- Adapter ----
        wirelessEnabled = a.wirelessRadios
        hotHandover = a.hotHandover
        pairingNumericComparison = a.pairingNumericComparison
        pairingInteractiveAnswer = a.pairingInteractiveAnswer
        wifiAccessPoint = a.wifiAccessPoint
        androidAutoEnabled = a.androidAuto
        appDrivenSetup = a.appDrivenSetup

        // ---- Identity / branding ----
        name = v.identity.headUnitName
        oemIconEnabled = v.branding.advertise
        oemIconVisible = v.branding.visible
        oemIconLabel = v.branding.label
        oemIconBase64 = v.branding.icon?.pngBase64 ?? ""
        oemIconW = v.branding.icon?.width ?? 0
        oemIconH = v.branding.icon?.height ?? 0

        // ---- Display ----
        mainWidth = v.display.panel.width
        mainHeight = v.display.panel.height
        maxFPS = v.display.panel.maxFPS
        dpi = v.display.panel.dpi
        diagonalInches = v.display.panel.diagonalInches ?? 0
        mainSafeLeft = v.display.insets.left
        mainSafeTop = v.display.insets.top
        mainSafeRight = v.display.insets.right
        mainSafeBottom = v.display.insets.bottom
        mainDrawOutsideSafe = v.display.drawUIOutsideInsets

        altVideoEnabled = v.altDisplay.enabled
        altWidth = v.altDisplay.panel.width
        altHeight = v.altDisplay.panel.height
        altFPS = v.altDisplay.panel.maxFPS
        // (alt panel dpi / diagonal: no model field — see `profile`)
        altSafeLeft = v.altDisplay.insets.left
        altSafeTop = v.altDisplay.insets.top
        altSafeRight = v.altDisplay.insets.right
        altSafeBottom = v.altDisplay.insets.bottom
        altDrawOutsideSafe = v.altDisplay.drawUIOutsideInsets

        enablesHEVC = v.video.hevcAllowed

        // ---- Appearance / driver — the neutral pair is authoritative; the legacy booleans are
        // DERIVED from it on this WRITE so `save()` keeps writing downgrade-safe values: a build that
        // only knows `vc.nightMode` / `vc.rightHandDrive` still reads a sane fact. No renderer reads
        // them as an input any more — `AACapability.init(config:)` was deleted in Phase 2
        // (2026-09-04) and the AA renderer takes `theme`/`driverPosition` directly. `init` DOES
        // still read them, independently of the neutral keys, which is why a downgrade round trip
        // needs the load-time reconciliation described on `profile`. `auto` and `center` have no
        // legacy reading and derive to false, the legacy default and least surprising downgrade.
        theme = v.appearance.theme.rawValue
        nightMode = v.nightModeLegacy
        hideClock = v.appearance.statusBar.hideClock
        hideSignal = v.appearance.statusBar.hideSignal
        hideBattery = v.appearance.statusBar.hideBattery
        driverPosition = v.driverPosition.rawValue
        rightHandDrive = v.rightHandDrive

        // ---- Restrictions ----
        limitedUIConfigEnabled = v.restrictions.declared
        for row in Self.restrictionFields {
            self[keyPath: row.field] = v.restrictions.set.contains(row.member)
        }

        // ---- Powertrain ----
        engineTypes = Set(v.powertrain.engines.map(\.rawValue))
        chargingConnectors = v.powertrain.connectors.map {
            ConnectorRow(type: $0.type.rawValue, powerWatts: $0.powerWatts)
        }

        // ---- Input (mirror of `profile`; see the presence/primary notes there) ----
        // Absent surfaces reset their sub-flags to the shipped defaults rather than leaving whatever
        // the form had: a nil `touchscreen` must not remember a previous multi-touch setting, or the
        // next "enable touchscreen" click would resurrect a value the document never carried.
        touchScreenHighFidelity = v.input.touchscreen != nil
        touchScreenSupportsMultiTouch = v.input.touchscreen?.multiTouch ?? false
        touchScreenSupportsCancel = v.input.touchscreen?.supportsCancel ?? true
        touchpadSupport = v.input.touchpad != nil
        touchpadButtonsSupport = v.input.touchpad?.buttons ?? false
        knobSupport = v.input.rotaryKnob != nil
        knobSupportsHomeAndBackButton = v.input.rotaryKnob?.homeAndBackButtons ?? false
        knobSupportsNudge = v.input.rotaryKnob?.nudge ?? false
        dPadSupport = v.input.dPad
        mediaButtonsSupport = v.input.mediaButtons
        telephonyButtonsSupport = v.input.telephonyButtons
        steeringWheelSupport = v.input.steeringWheelButtons
        // LOSSY by Apple's vocabulary: `.touchpad` and `.touchscreen` both render as "Touchpad"
        // (there is no "Touchscreen" primaryInput in any CarPlay Simulator template); `profile`
        // recovers which one from surface presence. Only `.rotary` is a distinct spelling.
        primaryInput = v.input.primary == .rotary ? "Knobs" : "Touchpad"

        // ---- Audio / feeds ----
        voiceRateHz = v.audio.voiceRateHz
        telephonyOverProjection = v.audio.telephonyOverProjection
        metadataNowPlaying = v.metadata.nowPlaying
        metadataNavigation = v.metadata.navigation
        metadataTelephony = v.metadata.telephony

        // ---- CarPlay-exclusive block. `audioMode` / `metadataTier` are validated against the
        // model's own lists exactly as `init` validates the persisted keys: a hand-edited document
        // with an unknown spelling falls back to the shipped default instead of reaching the
        // emitter, which would print it verbatim into the pushed YAML.
        let cp = v.carPlay
        accessoryName = cp.accessoryName
        audioMode = Self.audioModes.contains(cp.audioMode) ? cp.audioMode : "auto"
        // VALIDATE the three free-text strings against the vocabularies, exactly as `audioMode` and
        // `metadataTier` are validated above and below. Until 2026-09-04 these were copied verbatim,
        // which was safe only while the UI Pickers were the sole writers. Import made a hand-edited
        // document the first free-text path in, and the emitter interpolates these raw into a YAML
        // flow mapping:
        //     "  - {type: 102, audioType: \(f.audioType), in: \(f.input), out: \(f.output)}"
        // so `"output": "x}\n  bogus: [\""` malforms the WHOLE pushed document and the box falls
        // back to its built-in defaults for resolution, HEVC, audio AND metadata — the same failure
        // class as the B3 unescaped-quote incident. An unknown spelling falls back to the shipped
        // default row value rather than reaching the emitter.
        audioFormats = cp.audioFormats.map { row in
            let v = row.validated()   // see AudioFormat.validated() — guards the YAML emitter
            return AudioFormatRow(streamType: v.streamType, audioType: v.audioType,
                                  input: v.input, output: v.output)
        }
        metadataTier = Self.metadataTiers.contains(cp.metadataTier) ? cp.metadataTier : "proven"
        metadataSkip = cp.metadataSkip
        vehicleStatusEnabled = cp.vehicleStatusEnabled
        vehicleStatusCaps = Set(cp.vehicleStatusCaps)
        for row in Self.accessoryFlagFields {
            self[keyPath: row.field] = cp.accessoryFlags[keyPath: row.flag]
        }
        limitedUIJapanMaps = cp.limitedUIJapanMaps
        limitedUIPairedDevices = cp.limitedUIPairedDevices
        limitedUIThemeCustomization = cp.limitedUIThemeCustomization
        limitedUIAutomakerSettings = cp.limitedUIAutomakerSettings
        limitedUIAutomakerSettingsInfoButton = cp.limitedUIAutomakerSettingsInfoButton

        // ---- Android Auto-exclusive block ----
        aaFitPanelWithMargins = v.androidAuto.fitPanelWithMargins
        aaPreferHEVC = v.androidAuto.preferHEVC

        // Same guard `init` and `save()` apply: resolution bounds, the 30/60 fps vocabulary, and the
        // ≥16 px safe box. Runs AFTER every write so the inset check sees the new width/height.
        clampInPlace()
        // Every setter above already marked dirty via its `didSet`; this is the explicit statement
        // of the contract ("apply marks dirty") so it survives a future change to those observers.
        markDirty()
    }

    /// `apply(preset.document)`. Kept as its own entry point so the Presets menu reads as what it is,
    /// and so a later "remember which preset this came from" needs one call site, not a search.
    func apply(preset: VehicleProfilePreset) {
        apply(preset.document)
    }
}
