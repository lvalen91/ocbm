# Settings reorganisation — design contract (2026-09-04)

Owner of this document: the DESIGN agent. Workers READ it and the two contract files
(`VehicleProfile.swift`, `FeatureMatrix.swift`); nobody edits those three during the parallel phase.
Everything a worker needs to code against is here; if something is missing, it is a defect in this
document — raise it, do not guess.

> **STATUS (2026-09-04, end of day): Phases 0, 1 and 2 have ALL LANDED.** Files registered in
> `project.pbxproj`; `AACapability.init(config:)` and its legacy forwarding init deleted;
> `SettingsRootView` rewired to Vehicle / Adapter / Diagnostics; both compatibility typealiases
> dropped; `runSettingsTests()` wired into `tests/main.swift`; build green; 1371 tests passing. The
> phase plan in §6 is kept as the record of how the work was split, not as work to do. **Facts in
> this document were corrected in place the same day** (marked "corrected 2026-09-04" below) after an
> evidence review: this document told W2 that AA tier 5 was unverified on a device, W2 encoded that in
> `FeatureMatrix.valueNotes`, and it shipped wrong — the committed evidence
> (`docs/androidauto/01_SESSION_AND_AV.md`) records all nine codec tiers device-verified. Treat every
> verification claim here as secondary to the docs/ evidence it cites.
>
> **STATUS (2026-09-05): Phase 3 — presentation — is DESIGNED, NOT IMPLEMENTED. NO CODE WAS
> WRITTEN.** §11 is the contract; §1, §0 decision 4, §6 and §8 were amended in place the same day to
> match it. Nothing in Phase 3 changes behaviour, bindings or emitted bytes: it is a pure re-layout
> of what Phases 0–2 already built, commissioned because the landed window is dense (Vehicle tab:
> ~95–105 form rows at rest, &gt;160 fully expanded) and reads as one long flat list. Until a Phase 3
> worker lands, the tab files still render the Phase 1 layout described in the amended §1 as "before
> Phase 3".
>
> **ONE DECISION IS OPEN and blocks part of the work: where the Save control lives (§11.9).** The
> owner settled the window (non-resizable, pane-sized), the pane switcher (a toolbar, macOS 27 tabs
> role) and the collapse policy (all collapsed, no persistence). Save was not settled, and the
> earlier justification for putting it in the toolbar was voided when the toolbar became the pane
> switcher. Ask; do not guess.
>
> **Platform baseline is macOS 27 ONLY** (§11.7), verified against the installed SDK on 2026-09-05:
> macOS 27.0, Xcode 27.0, Swift 6.4, deployment target 27.0, Swift 6 language mode. No
> `if #available` guards. **Liquid Glass is deliberately NOT adopted in this window** — Apple
> reserves it for the control layer, and the SDK exposes no `Form`/`Section` glass API at all.
>
> **§11.10 records a cross-session agreement with `zeno-ef`** (it owns the model half, Phase 3 owns
> the view half) covering a second-view-area control it is adding, plus two claims REFUTED on
> hardware that must not be rebuilt into the UI.

## 0. Decisions

1. **The neutral profile is the SOURCE; the Apple-schema YAML is a RENDERED ARTIFACT** (owner,
   2026-09-04). `VehicleProfile` + `AdapterSettings` describe the car and the box in words that
   belong to neither vendor. Two renderers consume it: the existing `VehicleConfigModel.yaml` text
   (CarPlay, pushed to the box at SUBSCRIBE) and `AACapability` (Android Auto, in-process). The box
   wire format does not change and the YAML drift guard still governs (§2).
2. **On-disk document = JSON with a YAML-shaped key layout** (`VehicleProfileDocument`). Foundation's
   coder, sorted keys, pretty-printed. Not a hand-rolled YAML subset: this project already lost a whole
   pushed document to one unescaped quote (docs/carplay/04 B3), and a second YAML parser is a second
   place for that. Trade-off: humans edit `"width": 1920` instead of `width: 1920`; gain: round-trip
   fidelity and determinism are the OS's problem, and equal documents are byte-equal.
3. **Provenance lives on `FeatureMatrix`, not on the profile.** Verified/unverified/refuted is a
   fact about (feature, protocol, this app's renderer); it is identical for every owner and changes
   with the code, not the value. Value-dependent cases are `FeatureMatrix.valueNotes` — for AA
   resolution the fact recorded is WHICH (tier, fps) pairing has run on a device (corrected
   2026-09-04: this line said "tier 3 proven, tier 5 not"; all nine tiers are device-verified, tiers
   1/2/3/6 at 60 fps, 4/5 at 30 and 60, 7/8/9 at 30 only — `docs/androidauto/01_SESSION_AND_AV.md`).
4. **Protocol-exclusive settings live as badged sub-groups INSIDE the feature they belong to**, never
   on a protocol tab. `Feature.exclusiveKeys(on:)` is the placement table. **Amended 2026-09-05
   (Phase 3):** the PLACEMENT rule is unchanged and still governed by `exclusiveKeys(on:)`; what
   changes is the container. The sub-group is now GATE-AND-REVEAL (`GateReveal`, §11) rather than an
   always-expanded `GroupBox`: before Phase 3 the `GroupBox` chrome — badge, "&lt;Projection&gt; only"
   label, border, padding — rendered even when the sub-group's own internal gate was closed (e.g.
   `ExclusiveSubGroup(...) { Toggle("Advertise OEM icon config"); if model.oemIconEnabled { … } }`
   drew a full box to hold one toggle). After Phase 3 a closed gate draws one row and no chrome.
5. **Persistence stays UserDefaults `vc.*`.** `VehicleConfigModel` remains the observable store; it
   gains a few neutral fields (§5) and MATERIALIZES a `VehicleProfile`. Existing keys keep working;
   new ones have one-shot idempotent migrations in the `mbaDefaultFlippedB4` idiom.

## 1. Tab structure

`SettingsRootView` becomes three panes (Vehicle / Adapter / Diagnostics — still the `Feature.Tab`
cases; only the SWITCHER changes, see below). **Window sizing amended 2026-09-05, REVISED the same day
(Phase 3):** it was 460×620 fixed for all three panes. An intermediate revision made it resizable;
the owner reversed that in favour of the HIG. **It stays NON-RESIZABLE and sizes itself to the
current pane.** Apple's Settings guidance is explicit on all four points, and Phase 3 adopts them
together:

- the window "accommodates the size of the current pane" — so each tab gets its own height rather
  than one frame serving a nine-section tab and a two-control tab equally badly;
- **dim (disable) the minimize and maximize buttons** — a settings window "isn't meant to be resized
  to see more". `styleMask` therefore does NOT gain `.resizable`;
- **the window title reflects the visible pane**, not a static "Settings";
- **the most recently viewed pane is restored on reopen.**

**Pane switcher amended 2026-09-05 (owner: "follow the Apple guidelines. So toolbar it'll be").**
The HIG's Settings page says "use toolbar panes for organizing settings". Phase 1 used a `TabView`
with `.tabItem`; an intermediate Phase 3 draft proposed modernising that to the value-based
`Tab { }` API. **Both are superseded: the pane switcher becomes a WINDOW TOOLBAR**, using the
macOS 27 tabs role rather than a plain segmented control (verified API in §11.7). `TabView` leaves
the window entirely and `SettingsRootView` becomes a switch on the selected pane, which is also what
makes the title-reflects-pane and restore-last-pane rules above trivial.

Per-pane sizing against a fixed window is the one non-trivial piece: see the implementation risk in
§11.6.

### Vehicle (`Feature.Tab.vehicle`) — the neutral profile, per-protocol renderings inline
Sections in `Feature.Section` order.

**Before Phase 3**, each feature rendered as: neutral control → badge row
(`FeatureMatrix.supports(f)`, one badge per projection: supported / limited / unsupported) →
per-protocol explanation rows (`FeatureSupport.effect`, hidden when `FeatureMatrix.isUniform(f)`) →
badged exclusive sub-groups (`Feature.exclusiveKeys(on:)`) → value note when the current value has a
`valueNote` (unverified/refuted values get the warning glyph).

**After Phase 3** (§11) each `Feature.Section` is a `CollapsibleFeatureSection`, collapsed by
default, and within it each feature renders as: heading (`Feature.title` + `FeatureBadgeStrip`) →
neutral control → gated `GateReveal` sub-groups. The three changes that remove rows from the resting
layout:

- **`FeatureExplanationRows` is no longer rendered unconditionally.** Its `FeatureSupport.effect`
  text moves into `FieldPopover`, which the badge already opens. It stays INLINE only where a
  projection is `.limited` or `.unsupported` — the case where a protocol silently approximates or
  discards the authored value, which the owner must not have to hover to discover — and where a
  caller passes `always: true` (today only `.wirelessRadios`, the defect-1 fix).
- **`ValueNoteLabel` is no longer a form row.** Its content folds into `FieldPopover` under the
  projection whose current value carries the note, via the same
  `FeatureMatrix.valueNote(for:on:value:)` lookup.
- **`Feature.summary` is no longer rendered.** It duplicates `FieldInfo.text[key]` almost verbatim
  for most features (compare `.summary` "The exact pixel grid of the panel the projection fills."
  with `FieldInfo.text["panelResolution"]`, the same sentence plus a clause). `Feature.title` stays
  at rest; the summary is popover content. The `summary` PROPERTY stays on `Feature` — it is still
  asserted by §9 and read by `FieldPopover`; only its inline render site goes.

| Section | Features | Exclusive sub-groups (badge) |
|---|---|---|
| Identity & branding | headUnitName, branding | CarPlay: accessoryName; CarPlay: OEM icon PNG + advertise + visible |
| Display | panelGeometry, frameRate, pixelDensity, insets, videoCodec, altDisplay | AA: fit with margins; CarPlay: enablesViewAreas / drawOutside / cornerMasks / second view area (`ViewArea2Field`: toggle + W×H@X,Y + verdict + `ViewAreasPreview`, §11.10); CarPlay: enablesVideoPlayback; AA: preferHEVC; CarPlay: alt-stream insets |
| Appearance | theme, statusBar | CarPlay: enablesUIAppearance / enablesMapAppearance |
| Driving | driverPosition, drivingRestrictions | CarPlay: japanMaps + 4 round-trip-only keys |
| Vehicle | powertrain | CarPlay: vehicleStatus (C-4 gated) |
| Input | inputDevices | CarPlay: touchScreenHighFidelity |
| Audio | audioProfile | CarPlay: audioMode / audioFormats / enablesMainBufferedAudio / enablesEnhancedSiri |
| Data feeds | metadataFeeds | CarPlay: metadataTier / metadataSkip / the transfer + DCX + VDP flags |

Save bar (neutral wording — "deferred while a session is live", no protocol named), Reset to defaults,
and the **Profile document** block (Import…, Export…, Presets ▸ `VehicleProfilePreset.builtIn`,
Export YAML kept as a CarPlay-badged secondary action) live at the bottom of this tab.

### Adapter (`Feature.Tab.adapter`) — box / radio / health / projection enables
Features `wirelessRadios, hotHandover, pairing, wifiAccessPoint, androidAutoProjection,
appDrivenSetup` with the same badge/explanation rows (this is where "Wireless radios" shows both
CarPlay and Android Auto as supported — defect 1). Below them the live box state that is `CCPATab`
today: CCPAInfo, box health, BT phase, phone ident, Restart adapter / NCM mode (prose neutral:
"any live session will drop").

**Corrected 2026-09-05.** This paragraph used to end "…and the Box Log stream toggle + cap
(`BoxLogSettings`, its own keys)", while the Diagnostics line below claimed the same control. One
setting cannot live on two tabs without two toggles for it, and the implementation put it on
Diagnostics only — `DiagnosticsTab.swift`'s header comment has carried this as an unresolved §1
inconsistency since 2026-09-04. Resolved in favour of the implementation: **the Box Log toggle + cap
live on Diagnostics and the Adapter tab does not duplicate them.** The log stream is a diagnostic of
this app's session, not a box behaviour.

**Phase 3 amendments (§11):** the five non-wireless enable sections become collapsible; the live box
state (Identity / Health / Live State / Known Devices, ~30 rows with a session up and two phones
paired) collapses behind ONE summary row that expands to today's stack unchanged; the two stacked
bottom bars are dissolved — what remains at the bottom is at most the one-line status string, which
is what a bottom bar is permitted to carry. Where Save, the dirty indicator and Refresh land instead
is the OPEN decision in §11.9; the toolbar is occupied by the pane switcher, so do not assume they
go there. The `stale (from previous session)` marker is PROMOTED into the collapsed
summary row (amber dot), because a reader who never expands must not read a pre-teardown snapshot as
live. Live box state STAYS on this tab — it was considered for Diagnostics and rejected: the
destructive actions belong beside the health rows they act on, and `Window ▸ Adapter Info`
(`AppDelegate.showAdapterInfo`) already renders a read-only subset, so a third copy would leave box
state with no owning surface.

### Diagnostics — `DiagnosticsTab`, in its own file: the Box Log stream toggle + cap, and nothing
else. Content unchanged by Phase 3 (two controls, no density problem to solve); it does NOT acquire
the Adapter tab's box telemetry.

## 2. Data flow and the drift guard

```
UserDefaults vc.*  ⇄  VehicleConfigModel (@Published, main actor)
                          │  var profile: VehicleProfile        (W1, extension)
                          │  var adapterSettings: AdapterSettings
                          │  func apply(_ doc: VehicleProfileDocument)   ← Import / Presets
                          │  var document: VehicleProfileDocument         → Export
                          ├── var yaml: String   (UNCHANGED text; CarPlay renderer; anchored, see below)
                          └── AACapability(profile:)             (W2; AA renderer)
```

**The YAML emitter's TEXT is anchored by tooling.** `tools/regen_app_yaml_fixture.py` extracts
these blocks VERBATIM from `host/CarPlayHost/carlink_macOS/App/SettingsWindow.swift` by string
anchor and compiles them against a stub model: `    var yaml: String {`, `static let
clusterInitialURL`, `    private var altDisplayPanelsYAML: String {`, `    private func
accessoryFields()`, `    private var metadataYAML: String {`, `    private var audioYAML: String {`,
`    private func limitedUIFields()`, plus `enum YamlEmit {` from `App/VehicleConfig.swift`.
Therefore:

- `VehicleConfigModel` and its emitter STAY in `App/SettingsWindow.swift` at that path with those
  anchors. Only VIEW code moves out (§6 Phase 0).
- The fixture document is NOT the default config — it is the hostile cluster-on / limitedUI-on /
  OEM-icon-on / custom-audio document in the regen stub. Any emitter change must leave that output
  byte-identical. New emission is allowed only in the "emit nothing for the default value" idiom
  already used by `limitedUIConfig` / `oemIconConfig` (e.g. `wifi_ap: false` only when disabled).
- If the emitter starts referencing a NEW model field, the stub class in
  `tools/regen_app_yaml_fixture.py` must gain that field (with the value that emits nothing) in the
  SAME commit, and `python3 tools/check_app_yaml_fixture.py .` must exit 0. Integrator only.
- Nothing in this refactor changes the emitted bytes for any existing configuration. Behaviour
  changes to the emitter are out of scope.

## 3. API reference — `VehicleProfile.swift`

All types: `Codable, Sendable, Equatable` (enums also `CaseIterable, Hashable`), Foundation only.

**Vocabulary enums** (raw value = persistence spelling)
- `DriverPosition { left, right, center }`
- `AppearanceTheme { auto, light, dark }` — `auto` = follow this Mac's effective appearance.
- `PrimaryInput { touchscreen, touchpad, rotary }`
- `EngineType { gasoline, diesel, electric, cng }` (Apple's tokens; Google's are coarser)
- `ChargingConnector { ccs1, ccs2, j1772, chademo, mennekes, gbtDC="gbt_dc", gbtAC="gbt_ac", nacsDC="nacs_dc", nacsAC="nacs_ac" }`

**`DrivingRestrictionSet: OptionSet<UInt32>`** — `.keyboard(1) .phoneKeypad(2) .mediaLists(4)
.otherLists(8) .longMessages(16) .video(32) .voiceInput(64) .configuration(128)`;
`.typicalDriving = [keyboard, phoneKeypad, longMessages, configuration]` (≡ today's
`AACapability.DrivingRestrictions.drivingDefault` mask 26).
**`DrivingRestrictionPolicy { declared: Bool = false; set: DrivingRestrictionSet = [] }`**

**Display**
- `PanelInsets { left, top, right, bottom: Int = 0; static zero; isZero }`
- `PanelGeometry { width=1920, height=1080, maxFPS=60, dpi=160, diagonalInches: Double? = nil; isPortrait; aspect; impliedDPI: Int? }`
- `MainDisplay { panel: PanelGeometry; insets: PanelInsets; drawUIOutsideInsets=false }`
- `AltDisplay { enabled=false; panel = 800×480@30; insets; drawUIOutsideInsets=false }`
- `VideoCodecPolicy { hevcAllowed = true }`
- `StatusBarPolicy { hideClock, hideSignal, hideBattery = false; isDefault }`
- `Appearance { theme = .light; statusBar }`

**Identity / branding / vehicle / input / audio / feeds**
- `Identity { headUnitName = "CarLink Widescreen" }`
- `BrandIcon { pngBase64: String; width: Int; height: Int }`
- `Branding { advertise=false; visible=true; label="CarLink"; icon: BrandIcon? = nil }`
- `ConnectorSpec { type: ChargingConnector; powerWatts: UInt32? = nil }`
- `Powertrain { engines: [EngineType] = []; connectors: [ConnectorSpec] = []; isElectrified; canonicalEngines; effectiveConnectors }`
- `Touchscreen { multiTouch=false; supportsCancel=true }`, `Touchpad { buttons=false }`, `RotaryKnob { homeAndBackButtons=false; nudge=false }`
- `InputDevices { primary = .touchscreen; touchscreen: Touchscreen? = Touchscreen(); touchpad: Touchpad? = nil; rotaryKnob: RotaryKnob? = nil; dPad=true; mediaButtons=true; telephonyButtons=false; steeringWheelButtons=false }`
- `AudioProfile { voiceRateHz = 48000; telephonyOverProjection = false }`
- `MetadataFeeds { nowPlaying = true; navigation = true; telephony = true }`

**Vendor-exclusive blocks** (quarantined vocabulary; rendered as badged sub-groups)
- `CarPlayExtensions { accessoryName=""; audioMode="auto"; audioFormats: [AudioFormat]; metadataTier="proven"; metadataSkip=""; vehicleStatusEnabled=false; vehicleStatusCaps: [String]=[]; accessoryFlags: AccessoryFlags; limitedUIJapanMaps/PairedDevices/ThemeCustomization/AutomakerSettings/AutomakerSettingsInfoButton = false }`
  - `AccessoryFlags` = the 14 `enables*` keys other than HEVC, defaults as `VehicleConfigModel.init`.
  - `AudioFormat { streamType=102; audioType="media"; input="none"; output="aac_lc_48k_stereo"; static defaultCustomFormats }` — twin of `AudioFormatRow` without the UUID.
- `AndroidAutoExtensions { fitPanelWithMargins = true; preferHEVC = false }`

**`VehicleProfile`** — `identity, branding, display, altDisplay, video, appearance, driverPosition
(.left), restrictions, powertrain, input, audio, metadata, carPlay, androidAuto`;
`static let default`; `rightHandDrive: Bool` (== .right); `nightModeLegacy: Bool` (== .dark).

**`AdapterSettings`** — `wirelessRadios=true, hotHandover=false, pairingNumericComparison=false,
pairingInteractiveAnswer=false, wifiAccessPoint=true, androidAuto=true, appDrivenSetup=true`;
`static let default`.

**`VehicleProfileDocument`** — `schemaVersion (currentSchemaVersion = 1)`, `preset: String?`,
`vehicle`, `adapter`; `static fileExtension = "vehicleprofile.json"`;
`static encode(_:) throws -> Data` (sortedKeys, prettyPrinted, withoutEscapingSlashes — deterministic);
`static decode(_:) throws` (refuses newer schema with `DocumentError.newerSchema`, runs
`migrationSteps()` v→v+1 idempotently, tolerant of missing keys → defaults AT ANY DEPTH —
corrected 2026-09-04; nested structs used synthesised `Codable` and demanded every key, so the
"additive fields need no bump" contract stated here was false and one new nested field would have
refused every previously exported profile. Fields without a default stay required:
`BrandIcon.pngBase64/width/height`, `ConnectorSpec.type`);
`static migrationSteps() -> [(from: Int, apply: (inout [String: Any]) -> Void)]` (empty at v1).

**`VehicleProfilePreset`** — `id, title, origin (.project | .desktopHeadUnit(file:) |
.carPlaySimulator(file:)), summary, vehicle, adapter; document`;
`static builtIn: [VehicleProfilePreset]` (16: shipped, 10 DHU-derived, 5 Apple-derived);
`static named(_:)`.

## 4. API reference — `FeatureMatrix.swift`

- `Projection { carPlay, androidAuto }` — `displayName`, `schemaName`. Runtime "which is live" stays `ControlsBridge.isAndroidAuto`.
- `VerificationStatus { deviceProven, unverified, refuted }`; `Verification { status, date: String?, note }` with `.proven(date, note)`, `.unverified(note)`, `.refuted(date, note)`.
- `FeatureSupport { level: .supported | .limited | .unsupported; vendorTerm; vendorKey: String?; effect; verification; isAvailable }`.
- `Feature` (22 cases, §1 table) — `section: Section`, `title`, `summary`, `exclusiveKeys(on:) -> [String]` (VehicleConfigModel property names). `Feature.Section` (9, `rawValue` = heading, `tab`), `Feature.Tab { vehicle, adapter, diagnostics }`.
- `FeatureMatrix` (namespace):
  - `support(_:on:) -> FeatureSupport`, `supports(_:) -> [(projection, support)]`, `isAvailable(_:on:)`, `unavailableReason(_:on:) -> String?` (the `ControlsWindow.Capability` idiom), `features(in:)`, `isUniform(_:)`.
  - `valueNotes: [ValueNote { feature, projection, value: String, verification }]`, `valueNotes(for:on:)`, `valueNote(for:on:value:)`. Value strings: AA tiers `"1920x1080"`, driver `"left|right|center"`, fps `"30|60"`, metadata tier `"proven|extended|all|rx-only"`, voice rate `"48000"`.
  - `restrictionMapping: [RestrictionMapping { restriction, title, carPlayElement: String?, androidAutoBit: UInt32? }]`, `carPlayLimitedUIElements(_:) -> [String]`, `androidAutoDrivingStatus(_:) -> UInt32`, `unexpressedRestrictions(_:on:)`.

## 5. Model additions (Phase 0, Integrator, in `SettingsWindow.swift`)

New `@Published` stored fields on `VehicleConfigModel`, read in `init` with `b()`/`i()`/string
helpers, written in `save()`, reset in `resetToDefaults()`. None is referenced by the emitter
except `wifiAccessPoint` (§2 stub rule).

| Field | Type | `vc.` key | Default | Migration |
|---|---|---|---|---|
| `driverPosition` | String (DriverPosition raw) | `driverPosition` | `"left"` | one-shot `vc.profileKeysV1`: absent → `rightHandDrive ? "right" : "left"` |
| `theme` | String (AppearanceTheme raw) | `theme` | `"light"` | same one-shot: absent → `nightMode ? "dark" : "light"` |
| `dpi` | Int | `dpi` | 160 | none |
| `diagonalInches` | Double (0 = nil) | `diagonalInches` | 0 | none |
| `hideClock`, `hideSignal`, `hideBattery` | Bool | same | false | none |
| `restrictVideo`, `restrictVoiceInput`, `restrictConfiguration` | Bool | same | false | none (AA-only members; the Apple five stay on `limitedUI*`) |
| `wifiAccessPoint` | Bool | `wifiAccessPoint` | true | none; emitter adds `wifi_ap: false` ONLY when false |
| `voiceRateHz` | Int | `voiceRateHz` | 48000 | none |
| `telephonyOverProjection` | Bool | same | false | none |
| `metadataNowPlaying`, `metadataNavigation`, `metadataTelephony` | Bool | same | true | none |
| `aaFitPanelWithMargins` | Bool | same | true | none |
| `aaPreferHEVC` | Bool | same | false | none |

Keep `rightHandDrive` and `nightMode` as stored UserDefaults fields; W1's `profile` derives them FROM
`driverPosition`/`theme` and `save()` keeps writing them to `vc.*` (a downgrade still reads sane
values). They are UserDefaults-only: the emitted YAML has carried neither key since 2026-09-02
(`vehicle_config.rs`'s `EMITTED_BUT_UNREAD` comment), and this reorganisation did not reintroduce them.
`AACapability.init(config:)` (formerly SettingsWindow.swift:2387) and its legacy forwarding init WERE
deleted by the Integrator in Phase 2 (2026-09-04) after W2's `init(profile:)` landed.

## 6. Phases and file ownership

**Phase 0 — Integrator, serial, before any worker starts.**
1. Split `App/SettingsWindow.swift`: keep `AudioFormatRow`, `VehicleConfigModel` (whole class incl.
   emitter, anchors untouched), `SettingsWindowController`, `SettingsRootView` and the
   `AACapability` extension there; MOVE, byte-for-byte, `FieldInfo` + `InfoLabel`/`InfoToggle` →
   `App/Settings/FieldInfo.swift`; `ResolutionField`…`FrameRatePicker`, `AudioLabels`…
   `AudioCapabilitiesReference`, `LiveAppearanceSection`, `ConfigurationTab` →
   `App/Settings/VehicleTab.swift`; `BoxHealth`, `BtPhase`, `CCPABridge`, `HealthRow`, `CCPATab` →
   `App/Settings/AdapterTab.swift`; `BoxLogSettings`, `DiagnosticsTab` → `App/Settings/DiagnosticsTab.swift`.
   Drop `private` on moved helpers that cross files.
2. Add the §5 fields. 3. Register the new files in `project.pbxproj`. 4. `check_app_yaml_fixture.py`
   exits 0; app builds; `run_tests.sh` green. Commit. Workers branch from here.

**Phase 1 — five workers in parallel.** WRITE = may create/edit; READ = may only read.
Nobody edits `SettingsWindow.swift`, `project.pbxproj`, `tests/run_tests.sh`, `tools/*`, or the
three contract files.

| Worker | WRITE | READ |
|---|---|---|
| **W1 Model bridge** | `App/Settings/VehicleConfigModel+Profile.swift` (new: `extension VehicleConfigModel { var profile; var adapterSettings; var document; func apply(_ doc:) ; func apply(preset:) }`), `App/Settings/ProfileDocumentIO.swift` (new: file read/write helpers, no UI) | SettingsWindow.swift, VehicleProfile.swift, FeatureMatrix.swift, VehicleConfig.swift |
| **W2 AA renderer** | `AA/AACapability.swift`, `AA/AAWire.swift`, `AA/AASession.swift`, `App/AppDelegate.swift` (only the lines constructing `AACapability`) | VehicleProfile.swift, FeatureMatrix.swift, SettingsWindow.swift, ControlsWindow.swift |
| **W3 Vehicle tab** | `App/Settings/VehicleTab.swift`, `App/Settings/FieldInfo.swift`, `App/Settings/FeatureBadges.swift` (new: `FeatureBadgeRow(feature:)`, `FeatureExplanationRows(feature:)`, `ValueNoteLabel(feature:projection:value:)`), `App/CornerMask.swift`/`App/CarPlayView.swift` NOT touched | VehicleProfile.swift, FeatureMatrix.swift, SettingsWindow.swift, W1's extension signatures (this doc) |
| **W4 Adapter + Diagnostics + document UI** | `App/Settings/AdapterTab.swift`, `App/Settings/DiagnosticsTab.swift`, `App/Settings/ProfileDocumentView.swift` (new: Import/Export/Presets; `struct ProfileDocumentView: View { init(model: VehicleConfigModel) }` — W3 embeds it) | VehicleProfile.swift, FeatureMatrix.swift, SettingsWindow.swift, W1's signatures |
| **W5 Tests + docs** | `tests/SettingsTests.swift` (new: `func runSettingsTests()` using the harness's `check`/`section`), `docs/host/00_MACOS_HOST_APP.md` (correct in place — the Settings section), `docs/androidauto/00_ARCHITECTURE.md` (the "AA reads six values from the CarPlay model" claim) | everything |

Cross-worker contract (signatures W3/W4 code against before W1 lands):
```swift
extension VehicleConfigModel {                       // W1
    var profile: VehicleProfile { get }              // materialized from the @Published fields
    var adapterSettings: AdapterSettings { get }
    var document: VehicleProfileDocument { get }     // preset: nil
    func apply(_ doc: VehicleProfileDocument)        // writes fields, marks dirty; does NOT save
    func apply(preset: VehicleProfilePreset)         // = apply(preset.document)
}
enum ProfileDocumentIO {                             // W1
    static func read(from url: URL) throws -> VehicleProfileDocument
    static func write(_ doc: VehicleProfileDocument, to url: URL) throws
}
extension AACapability {                             // W2
    init(profile: VehicleProfile, adapter: AdapterSettings, warn: (String) -> Void)
    var negotiationNotes: [String] { get }           // what the renderer had to approximate (defect 4) — W3 shows them
}
```

**Phase 2 — Integrator. DONE 2026-09-04.** Registered the new files in `project.pbxproj`; added
`tests/SettingsTests.swift` to the `swiftc` list in `run_tests.sh` and `runSettingsTests()` to
`tests/main.swift`; deleted `AACapability.init(config:)` and its legacy forwarding init; wired
`SettingsRootView` to the three tab files (Vehicle / Adapter / Diagnostics); dropped both
compatibility typealiases; build green; 1371 tests passing. (Written in the imperative above until
2026-09-04 with no completion marker, which read as work still to do.)

**Phase 3 — presentation. DESIGNED 2026-09-05, NOT IMPLEMENTED.** Purely cosmetic: collapse, gate,
consolidate the popovers, resize the window. No `@Published` field, no binding target, no emitter
input, no `FeatureMatrix`/`FieldInfo` STRING CONTENT, and no emitted byte changes. The contract is
§11.

| Worker | WRITE | READ | FROZEN |
|---|---|---|---|
| **Presentation** | `App/Settings/FeatureBadges.swift`, `App/Settings/FieldInfo.swift` (view code only — NOT the `descriptions` / `inertKeys` data), `App/Settings/VehicleTab.swift`, `App/Settings/AdapterTab.swift`, `App/Settings/ProfileDocumentView.swift` (container only), and in `App/SettingsWindow.swift` **only** `SettingsRootView` + `SettingsWindowController` (window chrome, §1) | this document, `FeatureMatrix.swift`, `VehicleProfile.swift`, `tests/SettingsTests.swift` | `VehicleConfigModel` and its emitter (the §2 anchors), `FeatureMatrix.swift`, `VehicleProfile.swift`, `tests/SettingsTests.swift`, `DiagnosticsTab.swift`, `tools/*`, `project.pbxproj` |

The `SettingsWindow.swift` exception is narrow and load-bearing: `SettingsRootView` and
`SettingsWindowController` sit in that file below the model, and §2's anchors are all INSIDE
`VehicleConfigModel`. A Phase 3 worker may edit those two view types and must not touch anything
above them. `python3 tools/check_app_yaml_fixture.py .` must still exit 0 — if it does not, the
worker has crossed the line.

## 7. Defect ownership

| # | Defect | Owner | Fix shape |
|---|---|---|---|
| 1 | `wireless:` labelled "Wireless CarPlay", gates wireless AA | **W4** (label + badges from `.wirelessRadios`) | Adapter tab renders both projections supported; tooltip from `FeatureSupport.effect`. Box side unchanged (a separate `wireless_android_auto:` key would change the YAML). |
| 2 | nightMode/rightHandDrive in a CarPlay "Appearance" section, live only for AA | **W3** (placement: Driving ▸ driverPosition, Appearance ▸ theme with badges), **W1** (derive from `driverPosition`/`theme`), **W2** (consume `profile.driverPosition` ternary + `theme`) | CENTER declares wire 3 and is shown with its `valueNote` (unverified). `auto` resolves via `NSApp.effectiveAppearance` in W2's AppDelegate call site, not in AACapability. |
| 3 | `name` in `FieldInfo.inertKeys` though AA advertises it | **W3** | Remove from `inertKeys`; the two explanation rows come from `.headUnitName`. |
| 4 | Resolution/FPS silently degrade under AA | **W2** (`negotiationNotes`) + **W3** (render under Panel resolution when `isAndroidAuto` or always, with the tier + margins) | `AACapability` keeps `warn` and additionally records the notes. |
| 5 | `enablesHEVC` ignored by AA | **W2** | `hevcAllowed=false` ⇒ clamp tier to ≤1080p and note it; `true` ⇒ HEVC on tiers that need it; `androidAuto.preferHEVC` ⇒ HEVC at ≤1080p. |
| 6 | DHU .ini table says 3840×1260 for tier 5 | **W2** | Keep `r3840x2160 = 5` (the DHU's protobuf enum is `VIDEO_3840x2160`; the `.ini` string is a DHU typo). **Corrected 2026-09-04:** this row used to say "Tier 5 unverified … the `valueNote` stays `unverified` until a device run", and that instruction was WRONG on the day it was written — tier 5 (H.265) was device-verified 2026-09-04 at 30→30 and 60→60, 0 drops (`docs/androidauto/01_SESSION_AND_AV.md`). The `valueNote` records the (tier, fps) pairings that have run, not a blanket "unverified". |
| 7 | CarPlay-only prose on agnostic controls | **W3** (save bar), **W4** (reboot/NCM alerts) | Neutral wording; protocol names only in `FeatureSupport.effect` rows. |
| 8 | `wifi_ap:` has no UI | **W4** (control from `.wifiAccessPoint`), **Integrator** (field + `wifi_ap: false` emission + regen stub) | Emit only when false; absent stays the enabled default. |

## 8. Rendering rules (both renderers derive from the same neutral value)

This table is the neutral→wire DERIVATION and is unaffected by Phase 3. Note only that the VIEW no
longer echoes every row of it inline: since Phase 3 the per-projection `effect` sentences reach the
screen through `FieldPopover` (§11) rather than as always-visible caption rows. The strings
themselves, and this table as their source of truth, are unchanged.

| Neutral | CarPlay (W1 keeps the emitter's inputs identical) | Android Auto (W2) |
|---|---|---|
| `identity.headUnitName` | `name:` | `display_name` (+ " OCBM"), `headunit_info` make/model |
| `branding` | `oemIconConfig` when `advertise && icon != nil` | name only |
| `display.panel` W×H, `maxFPS` | `pixelDimensions`, `maxFPS` | `Resolution.tierAndVisible` when `androidAuto.fitPanelWithMargins`, else `nearest`; `FrameRate.nearest` |
| `display.panel.dpi` | — | `density` (replaces `AA_DENSITY`; env still overrides for the bench) |
| `display.insets` | `viewAreas.safeArea` | recorded; NOT sent until the SDR field is confirmed (`FeatureSupport` says so) |
| `video.hevcAllowed` | `enablesHEVC` | §7 defect 5 |
| `appearance.theme` | `setNightMode` live (Controls window path) | `night_mode` sensor: dark=true, light=false, auto=Mac appearance |
| `appearance.statusBar` | — | recorded; NOT sent until field numbers are confirmed |
| `driverPosition` | not emitted | wire 2 / 1 / 3 for left / right / center (`AA_DRIVER_POSITION` still overrides) |
| `restrictions` | `declared` ⇒ `limitedUIConfig` with `FeatureMatrix.carPlayLimitedUIElements(set)` + the CarPlay-exclusive five | mask sent on `setDrivingRestricted(true)` = `declared ? androidAutoDrivingStatus(set) : drivingDefault` |
| `powertrain` | `iapConfig` param 20 (existing) | recorded; not sent yet |
| `input` | `hidConfig` + `primaryInput` (`touchscreen`/`touchpad` → "Touchpad", `rotary` → "Knobs"; `touchscreen == nil` → `touchScreenMode: Disabled`) | touchscreen presence + keycodes (existing) |
| `audio.voiceRateHz`, `telephonyOverProjection` | via CarPlay audio table only | `voiceSinkRate`, telephony sink (replace `AA_VOICE_RATE` / `AA_TELEPHONY_SINK` defaults; env still overrides) |
| `metadata.*` | tier picker governs | three service descriptors individually (replaces `AA_METADATA`) |
| `AdapterSettings` | `wireless`, `hot_handover`, `pairing`, `android_auto`, `wifi_ap` (only when false), `appDrivenSetup` | — |

The CarPlay column is achieved WITHOUT touching the emitter: W1's `profile` getter derives neutral
values from the existing `@Published` fields (`restrictions.declared` ← `limitedUIConfigEnabled`,
`restrictions.set` ← the five `limitedUI*` Bools + `restrictVideo/VoiceInput/Configuration`,
`input.primary` ← `primaryInput` + `touchpadSupport` + `touchScreenHighFidelity`, …) and
`apply(_:)` writes them back through `FeatureMatrix.restrictionMapping` / the same rules in reverse.
The emitter keeps reading the fields it reads today, so its text and output are unchanged.

W1's reverse mapping for persisted keys (materializing `profile.input.primary`): `"Knobs"` →
`.rotary`; `"Touchpad"` → `.touchpad` when `touchpadSupport && !touchScreenHighFidelity`, else
`.touchscreen`. No new key. `apply(_:)` runs `clampInPlace()` afterwards, so a preset below the
800×480 CarPlay floor (`dhu-6in`, 750×450) is clamped on load — W4 shows the preset's `summary`
and a clamp notice rather than silently loading a different geometry.

**The panel envelope is `PanelRule` (VehicleConfig.swift), corrected 2026-09-07.** Each axis admits
480–3840 and the product floor is applied by the panel's OWN orientation (`w >= h` → 800×480, else
480×800 — `ViewArea2Rule.minimumSize`, one source of truth). Until 2026-09-07 the model carried
per-axis constants (`minWidth/maxWidth` 800–3840, `minHeight/maxHeight` 480–2160): a landscape-shaped
envelope that silently squared a portrait 2160×3840 panel to 2160×2160, so the wired portrait
view-area sweep's five cases all failed containment against a 2160-tall panel and came back
INCONCLUSIVE while the landscape half passed. Those constants are gone; the field colouring
(`ResolutionField`), the import/preset pre-flight (`ProfileDocumentView.clamped`), the model
(`clampInPlace`) and the control socket all read `PanelRule`. **A clamp is never silent:**
`clampInPlace()` records every value it moved in `VehicleConfigModel.clampNotes` (not persisted,
never dirties), which the Vehicle tab renders under the panel field in the `negotiationNotes` style
and the control socket returns from `save` (`clamped`), `get viewarea` (`clampNotes`) and — as a dry
run of the same function — from `set` (`pendingClamp`, `stored`), so a caller whose write will not
survive Save is told before it reads back a different number. `ResolutionField` also shows
`PanelRule.verdict` in red while typing, naming the orientation and what Save will store.

Bench environment variables stay environment variables (`AA_FORCE_RES`, `AA_FORCE_FPS`, `AA_PANEL`,
`AA_PROTO`, `AA_LEGACY_VIDEO`, `AA_SKIP_AUDIO_ACK`, `AA_NO_TOUCH`, `AA_READ_STALL_S`) and continue to
override the profile; they are test fixtures, not vehicle facts.

## 9. Test contract (W5, `tests/SettingsTests.swift`)

**Phase 3 changes nothing here, stated explicitly so no reader goes looking for a diff.** These
assertions are on `FeatureMatrix` / `VehicleProfileDocument` / the migration, never on views, so a
view-only pass cannot break them by construction. They do however CONSTRAIN it: the new components
must keep enumerating from `FeatureMatrix.features(in:)` / `exclusiveKeys(on:)` / `isUniform(_:)`
and never from a hand-curated view-side list, or a feature the tests still expect to exist will
silently stop being rendered anywhere. Item 5 in particular ("every `Feature` × `Projection` has a
non-empty `effect`") is what guarantees `FieldPopover` always has something to show.

1. `VehicleProfileDocument()` encode → decode → encode is byte-equal and `==`.
2. Every `VehicleProfilePreset.builtIn` round-trips; ids unique; `shipped` == `.default`.
3. `decode("{\"schemaVersion\":1}")` == default document; `schemaVersion: 99` throws `.newerSchema`.
4. `FeatureMatrix.androidAutoDrivingStatus(.typicalDriving) == 26` (== `drivingDefault.rawValue`);
   `carPlayLimitedUIElements(.typicalDriving) == ["softKeyboard","softPhoneKeypad","longAlerts"]`.
5. Every `Feature` × `Projection` has a non-empty `effect` and a `vendorTerm`.
6. `AACapability(profile: .default, adapter: .default)` declares 1920×1080 @60, density 160,
   driver wire 2, H.264, no margins — identical to today's `init(config:)` output (W2 exposes what
   is needed to assert this).
7. Migration: a UserDefaults suite with `vc.rightHandDrive=true`, `vc.nightMode=true` and no
   `vc.profileKeysV1` yields `driverPosition == "right"`, `theme == "dark"`, and running the
   migration twice changes nothing.

## 10. Where the parity map was wrong or incomplete (checked 2026-09-04)

- **Night/theme**: DHU `uitheme auto|light|dark` is NOT a wire field the head unit sends; gearhead
  derives its theme from the `night_mode` sensor only. The neutral `auto` case is therefore defined
  as "follow this Mac's appearance", not as an AA value.
- **Driving restriction**: AA `driving_status` has 5 bits, the map said "5-bit mask" — correct — but
  `AACapability` already models it as an OptionSet; the actual gap was that `AASession` sends a
  hardcoded `drivingDefault` and CarPlay's six booleans never reach it. Also Apple's `japanMaps`
  has no neutral reading and stays CarPlay-exclusive; the neutral set has 8 members, not 6 or 5.
- **Tier 5**: the DHU binary carries BOTH `VIDEO_3840x2160` (protobuf enum name) and `3840x1260`
  (its `.ini` parser's string table). The code's `r3840x2160 = 5` is consistent with the enum; the
  `.ini` string is a DHU typo. Device-verified 2026-09-04 at 30 and 60 fps (Pixel 10 / gearhead 17.5,
  wireless, H.265, 0 drops) — this bullet said "still unverified on device either way" until the
  same-day correction; see the §0 decision 3 note for the per-(tier, fps) precision.
- **Insets**: AA `contentinsets`/`stablecontentinsets` are the safe-area analogue; AA `margins`
  are a DIFFERENT mechanism (codec pixels cropped to fit a non-tier panel), which the map folded
  into one row. The profile separates them: insets are authored, margins are derived.
- **Branding/name**: AA sends `name` in THREE places (`headunit_info` make AND model, and
  `display_name` as "<name> OCBM"); the map listed make/model + display_name as if distinct facts.
- **Metadata**: `all_720p.ini` and `loaded_720p.ini` share an identical `[general]` section
  (touchpad + touchpadnavigation + controller + instrumentcluster + playbackstatus, 1280×720, dpi 160,
  30 fps) but are NOT identical files (corrected 2026-09-04 by re-reading both on disk — this bullet
  said "identical diffs"): `loaded_720p.ini` carries a `[sensors]` block (`location` / `night_mode` /
  `driving_status` = true) and `all_720p.ini` is the only DHU config with no `[sensors]` section. The
  map counted eleven distinct presets. Sixteen presets ship here, with those two as one entry — which
  remains defensible because `[sensors]` are runtime feeds the DHU simulates, not profile facts, and
  the neutral profile has no field for them. (`VehicleProfile.swift` still describes the pair as
  "identical" in its preset comments — a source correction outside this document's scope.)
- **`wifi_ap`**: default is ENABLED when absent (`wifi_ap_enabled()` greps only for an explicit
  `false`); the map did not state the polarity, which matters for byte-identical emission.
- Nothing else in the map was contradicted by the code or the DHU files.

## 11. Presentation contract (Phase 3, designed 2026-09-05)

**Why.** Phases 0–2 got the DATA right — neutral profile, per-protocol badges, provenance — and got
the layout wrong. Every honesty mechanism was paid for in permanent vertical rows: a badge row, two
explanation rows, a provenance glyph and a value note under nearly every control. The Vehicle tab
rests at ~95–105 form rows and exceeds 160 with every gate open; the Adapter tab carries ~30
always-visible telemetry rows under six enable sections and two stacked bottom bars. The owner's
verdict (2026-09-05): "more crowded, more complicated… all informational details or descriptions
should be in (i) hover overs… things or categories should be collapseable." Phase 3 keeps every
mechanism and stops rendering it at rest.

**Target.** Vehicle: 11 rows at rest. Adapter: ~10. Nothing removed — every control one click away.

### 11.1 Owner decisions (2026-09-05)

1. **All sections collapsed on open, no exceptions.** Auto-expanding sections holding a non-default
   value was offered and DECLINED in favour of the cleanest resting state. Accepted trade-off: a
   non-default value you set earlier is invisible until you open its section. The mitigation, if this
   later bites, is a change-count badge on the collapsed header (`"3 changed"`, computed read-only
   against `VehicleProfile.default`) — deliberately NOT built now.
2. **No persisted expansion state.** Plain `@State`, reset each launch, like today's two disclosures
   (`AudioCapabilitiesReference.expanded`, `showYAML`). No new `@AppStorage` key. (Remembering state
   across launches was the third option and was not chosen.)
3. **Window stays NON-RESIZABLE and sizes to the current pane** (§1). An intermediate revision made
   it resizable; the owner reversed that the same day in favour of Apple's Settings guidance, which
   also brings the dimmed minimize/maximize buttons, the pane-reflecting window title, and
   restore-last-pane-on-reopen.
4. **The pinned bottom bar carrying a primary action is retired** (§11.9). The toolbar is the PANE
   SWITCHER (§1), so where Save itself lands is the one decision Phase 3 did NOT settle — see the
   OPEN item in §11.9. Do not start the bottom-bar work without resolving it.
5. **Deployment target is macOS 27 only** (confirmed owner intent, 2026-09-05; the project already
   built this way). Phase 3 therefore uses NO `if #available` guards and may use any API in the 27
   SDK unconditionally.

`.wirelessRadios` collapses with everything else. Its `always: true` explanation rows — the defect-1
fix — are preserved WITHIN the expanded section, so the "one radio gate, both projections" sentences
still exist; collapse puts them one chevron away rather than deleting them. This is a weaker
guarantee than Phase 1's and is accepted knowingly: defect 1 was an actively WRONG label, whereas a
collapsed section mislabels nothing.

### 11.2 Components

Ten view types become eight. Inputs are `Feature`, `Projection`, `FeatureSupport`, `Verification`
and `FieldInfo` lookup keys — **no component takes a free-text `String` that is not a lookup key**
(the standing rule, `FeatureBadges.swift` header). That rule is what stopped "CarPlay-only" from
being hand-written over an Android-Auto-live control (defects 1–3); a new visual skin must not
reopen it.

| Component | Status | Contract |
|---|---|---|
| `ProjectionBadge` | **survives unchanged** | the one visual atom; capsule, glyph + name |
| `VerificationGlyph` | **survives**, but only INSIDE `FieldPopover` — never an inline form row | glyph + short label; `.help()` carries `Verification.note` |
| `FeatureHeading` | **survives**, minus the summary line | `Feature.title` + `FeatureBadgeStrip` |
| `FeatureBadgeRow` | **absorbed** into `FeatureBadgeStrip` | had exactly one caller (`FeatureHeading`) — a private detail wearing a public name |
| `FeatureSupportDetail` | **absorbed** into `FieldPopover` | its content is popover-section 2 |
| `FeatureExplanationRows` | **absorbed** into `FieldPopover`; inline only per §1's two exceptions | keeps its existing `always: Bool` parameter as the mechanism — do NOT invent new suppression logic |
| `ValueNoteLabel` | **absorbed** into `FieldPopover` | keyed by `FeatureMatrix.valueNote(for:on:value:)`, unchanged |
| `ExclusiveSubGroup` | **becomes `GateReveal`** | see below |
| `InfoLabel` | **becomes `FieldPopover`** | signature-compatible; `InfoToggle` needs no interface change |
| `InfoToggle` | **survives** as a thin wrapper over `FieldPopover` | 33 call sites, all unaffected |
| `CollapsibleFeatureSection` | **new** | `section: Feature.Section` + `@ViewBuilder content`; `@State` expanded, default false |
| `FeatureBadgeStrip` | **new** | `feature: Feature`, optional `currentValue: String?`; at rest draws only today's badges |
| `GateReveal` | **new** | `feature`, `projection`, `isOn: Bool` (the caller's EXISTING gate), optional `caption`, content. Gate closed ⇒ one row, no `GroupBox` chrome. Gate open ⇒ today's content verbatim |
| `FieldPopover` | **new** | `title`, `key` (→ `FieldInfo.text[key]`), `feature: Feature?`. With `feature == nil` it degrades to exactly today's `InfoLabel` behaviour |

### 11.3 The consolidated popover

Today, learning about one setting takes THREE surfaces: the (i) for the neutral description, each
badge for that projection's `effect` + provenance, and a hover on the glyph for the evidence note.
`FieldPopover` is one popover, opened from EITHER the (i) or a badge, with identical content either
way — so nobody needs to know which icon carries which fact. 340pt wide (today's
`FeatureSupportDetail` width).

```
┌──────────────────────────────────────────────┐
│  Panel resolution                              │ ← Feature.title
│  The exact pixel grid of the panel the         │ ← FieldInfo.text[key]  (ABOVE the fold)
│  projection fills; also the space touch        │
│  coordinates are reported in.                  │
├──────────────────────────────────────────────┤
│  ⬤ CarPlay          supported                  │ ← ProjectionBadge + level.word
│    Rendered and streamed at exactly this size. │ ← FeatureSupport.effect
│    ✓ device-proven 2026-07-12                  │ ← VerificationGlyph
│                                                 │
│  ⬤ Android Auto     limited                     │
│    Snapped to the nearest tier…                │
│    ✓ device-proven 2026-09-04                  │
│    ⚠ 1920×1080: proven at 60 fps                │ ← the former ValueNoteLabel, folded in
├──────────────────────────────────────────────┤
│  Their term · Schema key · Format              │ ← below the fold
└──────────────────────────────────────────────┘
```

Honesty rules that survive verbatim:

- **The ⚠️ inert marker** (`FieldInfo.isInert(key)`) renders in the top block, never demoted into a
  sub-disclosure. "This setting has no effect on the wire" must be as visible as the description it
  qualifies. It stays a CARPLAY statement — it must never be surfaced as if it applied to Android
  Auto (`FieldInfo.swift`'s own scoping rule), and `InputSection`'s `hidKeys`/`inertHIDTitles`
  caption stays the single render site for the HID list, never hand-duplicated into a new component.
- **Refuted values** render with the red `xmark.octagon.fill` and their `note` VERBATIM — the note is
  the "do not retry without new evidence" record.
- Uniform, device-proven features collapse to a three-line popover; that is where most of the saving
  comes from.

### 11.4 What stays visible at rest — the density floor

Roughly 55–60 short strings: 22 `Feature.title`, 9 `Feature.Section` headers, ~25 control-row
labels, the save-bar state, and `ProfileDocumentView`'s three buttons. A redesign may not go below
this.

**Six live-computed warnings NEVER move to hover.** A warning you must hover to find is not a
warning:

| Warning | Site |
|---|---|
| implied-DPI mismatch ("differs by more than 20; UI will draw larger than life-size") | `VehicleTab.swift` DisplaySection |
| "Insets leave too little room — each side must keep ≥16 px." | `SafeAreaField` |
| `AACapability.negotiationNotes` ⚠️ rows (defect 4) | `AANegotiationReadout` |
| "…parsed for YAML round-trip only — Apple never emits them" | Driving ▸ CarPlay extras |
| vehicle-status "⚠️ Not yet supported by the adapter" | Vehicle ▸ vehicleStatus |
| "Apple's spec forbids combining the unified range warning with the per-engine ones" | Vehicle ▸ vehicleStatusCaps |

Confirmation-dialog text keeps its full content and protocol-neutral wording (defect 7). The
`AANegotiationReadout`'s one-line "Declares W×H @ fps, codec" summary stays inline when its section
is open; only its ⚠️ note rows are conditional, and they are conditional on being non-empty — i.e.
always shown when they exist.

**Disabled-not-hidden stays disabled-not-hidden.** `mainDrawOutsideSafe` (gated on an inset being
set), `aaPreferHEVC` (gated on HEVC), and the wireless-off `hotHandover` / `pairing` toggles remain
visible-but-disabled with their explanatory footnote. These are controls whose availability is
temporarily false with the enabling control visible nearby; hiding them would satisfy "reveal on
enable" superficially while deleting the thing that teaches the dependency — and `AdapterTab.swift`
already says so in situ ("Both are DISABLED, not hidden … so the gate is legible"). Collapse gates
DISPLAY; it never changes a control's enabled state.

### 11.5 Gate-and-reveal map

`GateReveal` systematises a pattern the code already had in eight places — the mechanism is not new,
its consistent application is: `altVideoEnabled` (→ alt resolution, frame rate, alt-stream insets),
`oemIconEnabled` (→ icon picker, visibility toggle), `touchScreenHighFidelity` (→ multi-touch,
cancel), `touchpadSupport` (→ touchpad buttons), `knobSupport` (→ Home/Back, nudge),
`limitedUIConfigEnabled` (→ 8 restriction rows + CarPlay extras), `audioMode == "custom"` (→
`AudioFormatsEditor`), `vehicleStatusEnabled` (→ caps + conflict warning).

### 11.6 Implementation risks

1. **`.disabled(store.busy)` scope** (`AdapterTab.swift`) currently wraps `boxState` +
   `StreamPerfSection` + `boxControls` as ONE `Group`, deliberately excluding the enables above.
   Splitting those into separate collapsibles must re-apply it to the same three trees. Narrowing it
   is a behaviour change, not a layout change.
2. **`.onAppear { store.refresh() }`** must stay on the top-level `AdapterTab.body`. Moved onto a
   collapsed child it may never fire; duplicated across children it fires repeatedly.
3. **`.confirmationDialog` modifiers stay attached to the top-level view.** Only the triggering
   buttons may move; re-parenting a dialog into a menu or a collapsed branch can detach its
   presentation binding.
4. **No prose is rewritten.** Every relocation moves an existing `FieldInfo.text` /
   `FeatureSupport.effect` / literal string into a different container. New wording needs owner
   approval as a separate step.
5. **Per-pane sizing against a non-resizable window** (§1) is the hardest piece of Phase 3 and the
   one most likely to be got wrong. The window is an AppKit `NSWindow` built by
   `SettingsWindowController` hosting an `NSHostingView`; a fixed `.frame(width:height:)` on
   `SettingsRootView` is what currently forces one size on all three panes. Removing it is not
   sufficient — the controller must resize the window when the selected tab changes AND when a
   `Section(isExpanded:)` toggles, or a collapsed pane leaves dead space and an expanded one clips.
   Expect to drive the window frame from the hosting view's fitting size, and treat "does the window
   settle to the right height when a section expands" as the acceptance test for this item. Do not
   let this leak into a `.resizable` style mask as a shortcut — that is the decision §11.8 reversed.
6. **A window toolbar on an `NSHostingView`-hosted window** (§11.9) needs the toolbar to exist on the
   `NSWindow`; SwiftUI `.toolbar` content does not materialise a toolbar by itself in this hosting
   arrangement. Verify the `⌘S` shortcut still fires once the button is a toolbar item and not a
   `Form` descendant — that is a real regression risk, not a theoretical one.


### 11.7 Platform baseline — macOS 27 / Swift 6.4 (verified 2026-09-05)

Everything in this subsection was verified ON THIS MACHINE against the installed SDK or resolved
build settings, not from recollection. Where a fact could not be verified it is marked as such.

**Toolchain.** macOS 27.0 (26A5425a), Xcode 27.0 (27A5252f), Apple Swift 6.4, SDK MacOSX27.0.
`MACOSX_DEPLOYMENT_TARGET = 27.0`, `SWIFT_VERSION = 6.0` with `EFFECTIVE_SWIFT_VERSION = 6` — genuine
Swift 6 language mode, so complete data-race checking is on by default and the Swift-5-era
`SWIFT_STRICT_CONCURRENCY` dial is correctly unset. macOS 27 is BETA; treat any 27-only behaviour as
provisional.

**The harness stays pinned at `-target …-macos15`** (`tests/run_tests.sh:38`) and its comment
understates why. It cites `Synchronization.Mutex`, but its real job is enforcing the availability
floor on the four files it compiles — `VehicleProfile.swift`, `FeatureMatrix.swift`,
`ProfileDocumentIO.swift`, `AACapability+Profile.swift`. Raising it to macos27 would not break the
build today; it would silently delete the guard that stops those files acquiring a macOS-27-only API.
**No Settings VIEW file may ever be added to that list**: it would fail both for missing peer types
and for any macOS 26/27 modifier the redesign adopts, blocking a protocol-logic suite over a
cosmetic change.

**Adopt (verified present, with the older construct each replaces):**

| Adopt | Replaces | Availability | Evidence |
|---|---|---|---|
| `Section(isExpanded:content:header:)` | a hand-rolled `DisclosureGroup` + `@State` per section | API since macOS 14; **first-class rendering path only from macOS 26** | The initializer body branches `if #available(macOS 26.0, *) { Section.create(isExpanded:…) } else { unsafeBitCast(…) }`. At target 27.0 the modern path is always taken. |
| **Toolbar pane switcher with the macOS 27 tabs role** — AppKit `NSToolbarItemGroup.role = .tabs`, or SwiftUI `Picker(...).pickerStyle(.tabs)` | the whole `TabView` + `.tabItem { Label(…) }` construct in `SettingsRootView` (a 10.15 idiom) | **macOS 27.0** | SUPERSEDES the `Tab { }` row an earlier draft of this table carried; §1 moves the switcher into the toolbar, so `TabView` goes away rather than being modernised. Verified in the SDK: `@available(iOS 27.0, macOS 27.0, …) extension PickerStyle where Self == TabsPickerStyle` / `static var tabs`; AppKit `NSToolbarItemGroupRoleTabs = 1` with `@property NSToolbarItemGroupRole role API_AVAILABLE(macos(27.0))`, and the same on `NSSegmentedControl.role`. macOS 27 added this role precisely so a pane switcher is visually distinct from a value picker AND announces as tabs to VoiceOver — before 27 it had to be faked with a plain segmented control. |
| `.textFieldStyle(.bordered)` + `.textInputBorderShape(.roundedRectangle)` | `.textFieldStyle(.roundedBorder)` — **formally deprecated** | macOS 27 SDK | `@available(macOS, introduced: 10.15, deprecated: 100000.0, message: "Use textFieldStyle(.bordered) with textInputBorderShape(.roundedRectangle)")`. 6 call sites in `VehicleTab.swift` (`ResolutionField`, `SafeAreaField`, `ViewArea2Field`, the density + diagonal fields in `DisplaySection`, `DataFeedsSection`). Soft-deprecation — no warning today, but it is marked. |
| `concentricCornerRadii` / `ConcentricRectangle` / `.rect(corners:isUniform:)` | hardcoded `RoundedRectangle(cornerRadius: 5/3/2)` and `.cornerRadius(6)` | macOS 27 SDK | Current guidance is that radii are CONCENTRIC with their container, not fixed values; Apple publishes no numeric radius. Sites in `VehicleTab.swift`: `SafeAreaPreview`, `ViewAreasPreview`, `PreviewCanvas.legend` and the OEM icon thumbnail. |
| `Task.sleep(for: .seconds(6))`, and drop the redundant `@MainActor` label | `Task { @MainActor in try? await Task.sleep(nanoseconds: 6_000_000_000) }` | — | `AdapterTab.swift:200-205`. `Task {}` created from an isolated sync context already inherits isolation (SE-0338). Purely cosmetic; `sleep(nanoseconds:)` is NOT deprecated. |

**Do NOT adopt:**

- **No Liquid Glass anywhere in this window's content.** Apple is explicit: "Don't use Liquid Glass
  in the content layer… Use standard materials for elements in the content layer." Independently,
  a grep of the whole SwiftUI + SwiftUICore interfaces for any tie between
  `glassEffect`/`GlassEffectContainer`/`backgroundExtensionEffect` and `Form`/`Section`/`FormStyle`
  returns ZERO matches — the glass modifiers are unconstrained `extension View`, so they will compile
  inside a `Form` and mean nothing. A settings form gets the current look by using STANDARD
  COMPONENTS and rebuilding; that is the whole adoption story here.
- **`@Observable` for section-expansion state.** Use plain `@State private var expanded = false`,
  matching the three existing precedents (`VehicleTab.swift:360`, `FeatureBadges.swift:155`,
  `FieldInfo.swift:201`). A reference type for a few `Bool`s is over-engineering.
- **`@Bindable` on `VehicleConfigModel`.** The SDK ships a hard compile error for this:
  `@available(*, unavailable, message: "@Bindable only works with Observable types. For
  ObservableObject types, use @ObservedObject instead.")`. The frozen model stays `@ObservedObject`.
  `ObservableObject`/`@Published` are NOT deprecated in this SDK (checked directly — only
  `@_originallyDefinedIn` re-homing attributes).
- **A pure-SwiftUI `Settings` scene replacing `SettingsWindowController`.** There is nothing newer
  than `NSWindow` + `NSHostingView`; the `Settings` scene is unchanged since macOS 11. Swapping the
  hosting model is an architectural change with lifecycle implications, not a cosmetic one.
- **`FeatureBadges.swift:68,76,87` "default will never be executed".** Intentional — the file's own
  comment says that warning IS the reminder to add a real arm when a verification status lands.

**Free on rebuild, no code:** standard controls, bars, sheets and popovers pick up the current
material; the macOS 26→27 `NSSegmentedCell` glass rendering fix; scroll-edge blending on standard
scroll containers; disabled-checkbox de-tinting; `Slider` no longer `NSSlider`-backed.

**Build state.** The project builds clean-but-noisy: 27 warnings, NONE in the Settings view files
except the three intentional ones above. The real concurrency debt is `AppDelegate.swift:393,417`,
`VideoChromeOverlay.swift:130,132`, `OCBMAVDecrypt.swift:258`, plus 12 deprecated Secure Transport
calls in `AATLS.swift`. All out of scope here; recorded so a Phase 3 worker does not think they
caused them.

**Unverified — do not cite a version for these:** `.presentationCompactAdaptation` and
`GlassButtonStyle` availability could not be pinned from the interface. Reduce-Transparency fallback
behaviour for custom glass views is third-party-sourced, not in Apple's own docs. Whether
`.formStyle(.grouped)` picked up glass treatment automatically in macOS 26/27 is not addressed in
Apple's `Form`/`FormStyle` documentation either way.

### 11.8 Two conflicts with Apple guidance — RESOLVED 2026-09-05 (owner)

Both were raised as open questions against an earlier draft of this section, and both were resolved
IN FAVOUR OF THE HIG. Recorded with the losing side intact so a later reader can see the trade that
was made and why.

1. **Resizable window — REVERSED to non-resizable.** The earlier draft specified resizable
   (min 460×480), reasoning that collapsible sections make content height genuinely variable
   (~200pt collapsed to well over 620pt expanded) and that Apple's Settings page was last updated
   **10 June 2024**, predating both Liquid Glass and this layout. The owner chose the HIG: dim
   minimize/maximize, size to the current pane. §1 and §11.1 decision 3 now state the HIG position;
   the variable-height problem is solved by per-pane sizing plus scrolling within a pane, not by
   letting the user resize.
2. **Save in a pinned bottom bar — MOVED to the window toolbar.** Apple's Windows page (updated
   **9 June 2025**, current) says: "Avoid putting critical information or actions in a bottom bar,
   because people often relocate a window in a way that hides its bottom edge. If you must include
   one, use it only to display a small amount of information." A Save button is exactly the critical
   action that rule names. See §11.9 for the replacement.

### 11.9 Toolbar and the retired bottom bars

**CORRECTED 2026-09-05.** An earlier draft of this section justified moving Save to the toolbar on
the grounds that the toolbar was empty, "because the pane switcher is a `TabView` tab strip, not an
`NSToolbar`". §1 then made the toolbar the pane switcher, which voids that justification entirely.
The bottom bar is still retired; where Save goes is now OPEN.

**Settled:** the pinned bottom bar must stop carrying a primary action. Apple's Windows page
(updated 9 June 2025, current): "Avoid putting critical information or actions in a bottom bar,
because people often relocate a window in a way that hides its bottom edge. If you must include one,
use it only to display a small amount of information."

**OPEN — the one decision Phase 3 did not settle. Do not guess it; ask the owner.** With the toolbar
occupied by the pane switcher, Save has three possible homes:

1. **Toolbar, trailing, beside the centred tabs group.** Legal and buildable; Apple's own Settings
   windows carry no Save at all, so there is no precedent to copy. Dirty indicator condenses to a
   label or dot beside it.
2. **Bottom bar, status-only exception.** Keeps Save where it is and accepts the documented
   deviation from the rule quoted above.
3. **Auto-save, no Save control anywhere.** The only fully HIG-native answer, and the only one that
   makes the question disappear. **NOT COSMETIC** — this app batches edits and pushes on Save,
   deferring while a projection session is live, so removing the control changes WHEN config reaches
   the adapter. Needs its own design pass and owner sign-off; it must not ride along with Phase 3.

**Whichever wins, these do not regress:** `⌘S`; `.disabled(!model.dirty)` on the Vehicle tab; the
deferred-push wording ("pushed to the adapter now (deferred while a session is live)") — it may move
into a `.help()` but is not deleted; Adapter's Refresh action; and the `stale (from previous
session)` marker.

**What may remain at the bottom regardless:** the one-line status string only — Adapter's
`"Connected · updated 3s ago · stale (from previous session)"` and its `ProgressView` while
`store.busy`. That is "a small amount of information directly related to a window's contents", which
the rule permits. Absent rather than empty when there is nothing to say.

**What may remain at the bottom:** the one-line status string only — Adapter's
`"Connected · updated 3s ago · stale (from previous session)"` and its `ProgressView` while
`store.busy`. That is "a small amount of information directly related to a window's contents", which
is what the rule permits. If there is nothing to say, the bar is absent rather than empty.

### 11.10 The second view area — agreed with session `zeno-ef`, 2026-09-05

Cross-session agreement, recorded here because it lived only in two transcripts. That session owns
the MODEL half (`VehicleConfigModel`, the emitter, `FeatureMatrix.swift`, `SettingsTests.swift`);
Phase 3 owns the VIEW half. It confirmed it has touched none of the six view files.

**The control it is adding:** a CarPlay view-area second-rect authoring UI — a toggle plus a
validated W×H@X,Y rect that must be contained in the panel.

**Names (agreed).** `viewArea2Enabled: Bool` and `viewArea2X / viewArea2Y / viewArea2W /
viewArea2H: Int`, `vc.` prefix, defaults false/0. Separate Ints match the model's existing geometry
style (`mainSafeLeft…`, `altWidth`); `viewArea2*` deliberately mirrors the box-side
`ViewArea2::contained_in` so both halves grep together. Absent-off, so the YAML fixture stays
byte-identical for every existing configuration (§2).

**Placement (agreed): NO new `Feature` case.** §0 decision 4 governs — a CarPlay-only concept must
not acquire a neutral name. It extends the existing array:

```swift
case (.insets, .carPlay): return ["enablesViewAreas", "mainDrawOutsideSafe", "enablesCornerMasks",
                                  "viewArea2Enabled", "viewArea2X", "viewArea2Y",
                                  "viewArea2W", "viewArea2H"]
```

`SettingsTests.swift:504-513` pins these arrays and is updated in the same commit — `zeno-ef`'s
file, not Phase 3's. **Correction (2026-09-07):** the array is a placement TABLE, not a renderer —
`VehicleTab.swift` hand-writes every `ExclusiveSubGroup`, so "renders automatically once the array
grows" was wrong; the five keys had no control until the view half landed. It now lives in
`VehicleTab.swift` as `ViewArea2Field` (the toggle, the W×H@X,Y fields in `SafeAreaField`'s Grid
idiom, a "Layouts" summary line naming both layouts and the floor for the typed aspect, then
`viewArea2Verdict` verbatim in the insets-verdict shape) and `ViewAreasPreview`, rendered as the
last rows of the insets CarPlay sub-group. `ViewAreasPreview` shares `PreviewCanvas` (the 260×132
fit and the legend swatch) with `SafeAreaPreview` so the two pictures are one visual language.

**What the preview draws (owner ask: "see the resize area differences").** The panel to scale;
area [0] = the full panel with the main safe box from the insets; area [1] = the `viewArea2*` rect
at its true relative origin and size, drawn on top in a second hue. It is a view over the same
`VehicleConfigModel` fields the emitter reads — it clamps nothing and re-derives no rule. A rect the
model returns a verdict for is drawn red, dashed, clipped at the panel edge where it overflows, and
captioned "not pushed", because that is literally what happens: `viewArea2YAML` omits it and CarPlay
sees the full panel only. Severity is not colour-coded beyond that — the verdict string is the one
place that says teardown vs lockout, and the view renders it rather than classifying it.

**Area [1] has no safe insets of its own — by emitter, not by preview.** `viewArea2YAML` writes
area [1]'s `safeArea` as a copy of its `viewArea` (full-bleed), the box reads only `viewArea` for
entry [1] (`vehicle_config.rs::second_main_view_area`) and re-emits the safe area full-bleed
regardless, and there are no `viewArea2Safe*` model fields. Apple's model (docs/carplay/06_AV_PIPELINE.md §2,
WWDC 2019-252) is that EVERY view area carries its own safe area, a subset of it. The preview draws
what is emitted — one solid box, captioned "Area 1's safe area is its whole rect" — and does NOT draw
an inset that is not in the YAML. Giving area [1] its own insets is a cross-cutting change (four
model fields + profile schema + `exclusiveKeys` + the pinned test arrays + the emitter, AND the
box-side reader/emitter, or the pushed insets would be silently dropped); it is deliberately NOT
landed and needs owner sign-off.

**Validation split (agreed).** The rule lives in the MODEL and is exposed as a computed verdict; the
view renders it in the same shape as the existing "Insets leave too little room — each side must
keep ≥16 px" line. Phase 3 does NOT reimplement `contained_in` in SwiftUI. The model encodes: panel
containment (`x+w <= panelW && y+h <= panelH`); **all four of x/y/w/h EVEN** (odd is a session
teardown — see below); `w > 0 && h > 0 && x >= 0 && y >= 0`; and the OWNER PRODUCT FLOOR of
**800×480 landscape / 480×800 portrait**, orientation chosen by the area's own aspect.
The `385 * 0.65 * scale` formula this paragraph used to state is **REFUTED as the gate** — it
mispredicts `480x400`, `400x400` and `384x400`, all of which render on hardware. iOS's own
tolerance is far lower (measured 350×304 on 1080×1920) and produces a *lockout*, not a teardown;
the product floor is what ships. See `ViewArea2Rule` in `App/VehicleConfig.swift`.

**The cornerMasks ↔ safe-area coupling, and its exact scope.** `enablesViewAreas` and
`enablesCornerMasks` are NOT independent checkboxes. The feature token is
`enablesViewAreas || enablesCornerMasks || safeAreaInsetPresent` (`App/VehicleConfig.swift:88`), and
box-side, cornerMasks-on omits the `safeArea` dict from the viewArea entry, because iOS hard-fails
`checkCarPlayFeatureAcceptance` with "cornerMasks flag set but a safeArea defined in viewAreas"
(`crates/vendor/receiver/src/info.rs:565-620`). So the UI must surface:

- cornerMasks ON → the **MAIN** safe-area inset fields go dead, with the reason stated INLINE, not
  on hover (a hard-fail is not a nicety).
- cornerMasks OFF while `enablesViewAreas` is false and no inset is present → viewAreas support is
  silently dropped; say so.

**MAIN-ONLY — verified, and the peer's first statement of this rule was wrong.** It is
`let masks = is_main && crate::levers::cornermasks()` (`info.rs:576`; the peer cited :571, and
corrected itself when challenged). The alt/cluster stream has no cornerMasks flag and **keeps its
safeArea**. The Vehicle tab has TWO `SafeAreaField`s — greying both would make a working alt-stream
control look broken. Grey the main one only.

**Tone: "these interact", not "this combination is broken."** cornerMasks OFF with a second area was
accepted and resized on hardware on 2026-09-05 (portrait `1080x1600@0,160` on a 1080×1920 panel) —
the first hardware run of the `masks == false` branch, where both areas carry their own nested
`safeArea`. Only the cornerMasks-ON case is a hard-fail. Overstating the off case would steer the
owner away from a combination that demonstrably works.

**Two refuted claims — do NOT rebuild either into the UI** (both refuted on hardware 2026-09-05 and
corrected in place in `docs/carplay/06_AV_PIPELINE.md:965,972`):

1. **"An area must touch a vertical edge" is REFUTED.** It was inferred from three landscape
   samples; a floating portrait area touching no panel edge was accepted. Build no edge-snap
   affordance and no edge validation.
2. **The `1416x842@492,59` failure was an ODD ORIGIN Y — SOLVED 2026-09-05.** Not a size floor, not
   a feature/structure check (`cornerMasks` and `focusTransfer` are both orthogonal and accept).
   All four of x/y/w/h must be EVEN or iOS returns `-16720` from `carEndpoint_copyScreenInfo:7001`
   and tears the session down; HEVC 4:2:0 cannot express an odd extent. One-pixel proof:
   `356x400@240,760` renders, `357x400@240,760` tears down.
   **Two failure classes the UI must NOT present alike:** odd / out-of-panel / non-positive kills
   the session (block at input); below the product floor only blacks the area out
   (`viewAreaTooSmall` lockout, session survives).
