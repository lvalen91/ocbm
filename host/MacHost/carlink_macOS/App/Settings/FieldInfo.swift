// FieldInfo.swift — the (i) popover vocabulary for the Settings window: `FieldInfo` (per-key
// explanations, grounded in docs/carplay/04_CAPABILITIES_AND_CONFIG.md) and the two SwiftUI
// wrappers that surface them, `InfoLabel` / `InfoToggle`.
//
// Split out of App/SettingsWindow.swift on 2026-09-04 (Settings reorganisation, DESIGN.md §6
// Phase 0) so the Vehicle-tab worker can own the explanation strings without touching the model
// file. `InfoLabel` / `InfoToggle` lost `private` because their callers (the Vehicle and Adapter
// tabs) crossed a file boundary. `VehicleConfigModel` itself did NOT move:
// tools/regen_app_yaml_fixture.py text-extracts its YAML emitter from SettingsWindow.swift by
// string anchor, so relocating the model would silently disarm the app→box drift guard.
//
// DIVISION OF LABOUR since the same day (DESIGN.md §6 W3): these strings explain WHAT a control
// edits, in words that belong to neither vendor. WHICH protocol expresses it, HOW, and whether that
// has been seen to work on a phone is `FeatureMatrix` data, rendered beside every neutral control
// by FeatureBadges.swift. A protocol claim that lives only here cannot be checked and has already
// been wrong three times (DESIGN.md §7 defects 1–3), so new entries for neutral keys stay
// protocol-free; the CarPlay-exclusive keys below (`enables*`, `audioFormats`, `metadataTier`, …)
// keep their CarPlay wording because they render inside a CarPlay-badged sub-group and the badge
// carries the scope.

import SwiftUI

// MARK: - Info popover infrastructure

/// A short plain-language explanation of a config field, shown in an (i) popover. Text is grounded in
/// the CarPlay SDK (docs/carplay/04_CAPABILITIES_AND_CONFIG.md glossary).
// Strings grounded in the CarPlaySDK glossary research (docs/carplay/04_CAPABILITIES_AND_CONFIG.md); each ≤ ~240 chars for a tooltip.
enum FieldInfo {
    /// YAML keys the box currently serde-IGNORES (2026-07-31 review): they ride the pushed config
    /// forward-compatibly but have zero on-wire effect today. Their tooltips keep the descriptive
    /// text and get a ⚠️ marker appended, so no control overclaims what it does. LIVE levers
    /// (wireless, pairing, resolution, frame rate, safe areas, HEVC, the alt stream + its geometry,
    /// audio formats, dPadSupport, enablesViewAreas) are deliberately NOT in this set.
    ///
    /// The marker is a CARPLAY statement ("no effect on the box"). Every key still listed here is
    /// one the CarPlay YAML renderer reads, and the Vehicle tab surfaces the list ONLY inside a
    /// CarPlay-badged sub-group (via `isInert`), so the statement is always read in scope; the
    /// neutral input rows use the protocol-free `input*` entries below instead of these keys.
    /// A NEUTRAL key must never be listed: "name" was here until 2026-09-04 (defect 3) on the
    /// strength of vehicle_config.rs not mapping it to the accessory name — true for CarPlay, false
    /// for the app as a whole, because `AAWire.serviceDiscoveryResponseFull` advertises that same
    /// string to Android phones as display_name (field 14) and as headunit_info make AND model
    /// (field 17). Per-protocol scope for neutral keys is `FeatureMatrix`, not this set.
    ///
    /// GROUND TRUTH is the box's own inventory, `EMITTED_BUT_UNREAD` in
    /// crates/vendor/receiver/src/vehicle_config.rs (`every_emitted_key_is_parsed_or_knowingly_ignored`):
    /// every key there is here. This set is a SUPERSET, because that list means "not parsed" while
    /// this one means "no wire effect": `mediaButtonsSupport`, `touchpadSupport`, `touchScreenMode`
    /// (our `touchScreenHighFidelity`) and `steeringWheelSupport` ARE parsed into `HidConfig` (C-2)
    /// but nothing in receiver/ or airplayd/ consumes the parsed field (grep 2026-09-04), so they
    /// stay marked. A key that is neither in the box list nor a parsed-but-unconsumed field must
    /// NOT be here — that is how knob/telephony/corner-masks were mis-marked until 2026-08-10.
    private static let inertKeys: Set<String> = [
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
        // enablesUIAppearance/enablesMapAppearance/enablesFocusTransfer removed 2026-09-02 (verify_06
        // 10-M1): airplayd arms all three per connection (main.rs:797-799, levers::set_ui_appearance/
        // map_appearance/focus_transfer) — they were live, not inert.
        "enablesUIContext", "enablesUISync", "enablesFileTransfer",
        "enablesVehicleDataProtocol", "enablesDCX",
        // In the box's list (task #13 overlap: the named capability is unimplemented; the adjacent
        // `extendedFeatures` array is emitted unconditionally in /info). Was a hand-written "⚠️ has
        // no effect" in the description until 2026-09-04 — the pattern this set exists to replace.
        "enablesEnhancedSiri",
    ]
    private static let inertMarker =
        " ⚠️ Not yet implemented on the box — this setting rides the config but currently has no effect on the wire."

    /// Whether a CarPlay YAML key is in the box-ignores set — for the Vehicle tab's CarPlay-badged
    /// "not yet read by the adapter" caption, so that list and the ⚠️ markers cannot disagree.
    /// Most HID keys are reached ONLY through here: their (i) text is the neutral `input*` entry on
    /// the control that sets them, and this set supplies the CarPlay caption.
    static func isInert(_ key: String) -> Bool { inertKeys.contains(key) }

    static let text: [String: String] = {
        var t = descriptions
        // Marker only where a description exists — an inert key whose control has moved to a
        // neutral entry (the HID set) is reached via `isInert`, not looked up here.
        for k in inertKeys where t[k] != nil { t[k] = t[k]! + inertMarker }
        return t
    }()

    // No entries for the adapter features (`wireless`, `pairing`, `hotHandover`, `android_auto`,
    // `wifi_ap`, `appDrivenSetup`) since 2026-09-04: those controls live on the Adapter tab, which
    // draws its per-protocol text from `FeatureSupport.effect`. The entries deleted here were the
    // CarPlay-worded originals ("Advertise the box for wireless CarPlay…" — defect 1) and a key
    // whose control has moved must not keep a description here, or the next reader trusts it.
    // SHORTENED 2026-09-08 (DESIGN.md §11.6 item 4 amendment — the owner-approved exception to
    // "no prose is rewritten"): Apple caps hover help at ~60-75 characters
    // (hig://general/offering-help) against this file's former 300pt paragraphs. 55 of these 59
    // entries were cut; 4 were already short enough to leave alone. The full ORIGINAL text of every
    // cut string, its shortened replacement, and a per-key fact-preservation note live in ONE place —
    // DESIGN.md §11.11 — rather than duplicated in a comment here, per this project's single-source-
    // of-truth rule for corrections (CLAUDE.md "Documentation rule"). Evidence notes, refuted-value
    // notes and the inert marker (FeatureMatrix.swift / the `inertMarker` constant above) are EXEMPT
    // and were not touched by this pass.
    private static let descriptions: [String: String] = [
        // Neutral `identity.headUnitName`. Per-protocol scope is FeatureMatrix `.headUnitName`: for
        // CarPlay it is the config's `name` only (the box derives the iOS-visible accessory name from
        // MAC + serial; the CarPlay-exclusive "Accessory name" below overrides that), for Android Auto
        // it IS the advertised head-unit name (display_name + headunit_info make/model). The old text
        // called it "config metadata only", which was the CarPlay half stated as the whole (defect 3).
        "name": "What the phone calls this head unit; each projection shows it its own way.",
        // Neutral `driverPosition` / `appearance.theme` (DESIGN.md §5, 2026-09-04). These REPLACE the
        // `rightHandDrive` / `nightMode` toggles that lived in a CarPlay "Appearance" section while
        // being live only for Android Auto (defect 2); the legacy booleans are still stored and are
        // derived from these two so a downgraded build reads sane values. Everything protocol-specific
        // that the old texts carried (the 2026-09-04 rail-side observation, the 2026-09-02 drop from
        // the YAML, setNightMode vs the static flag) now lives in FeatureMatrix `.driverPosition` /
        // `.theme` and their valueNotes, where the badges render it.
        // CORRECTED 2026-09-05: this said Centre "is a real option in both vendors' vocabularies".
        // It is not — Apple's key is the BOOLEAN /info `rightHandDrive` (R14G17 AirPlayCommon.h),
        // so CarPlay has no centre at all and Centre is sent to it as left. Only Android Auto has a
        // three-value DriverPosition (wire 2/1/3), and its centre is the untried one.
        "driverPosition": "Which side the driver sits on; sets which edge the app rail favors. CarPlay has no Centre.",
        // Protocol-free since 2026-09-04: the old tail claimed the CarPlay appearance capabilities
        // "decide whether iOS honours" this value, but nothing in the CarPlay path reads `theme` —
        // CarPlay's day/night is the live `ControlsBridge.nightModeOn` (ApKey.night). What each
        // projection does with it is `FeatureMatrix` `.theme`, on the badges.
        "theme": "Light, dark, or Auto (follow this Mac). Live-session switches below act independently.",
        // Added 2026-09-08 (DESIGN.md §11.4 TEN-WORD RULE sweep), moved verbatim off
        // `LiveAppearanceSection`'s section footer (VehicleTab.swift — was 58 words, visible
        // whenever Appearance is expanded). Not a per-key control description like the rest of this
        // file; it explains the whole "Display appearance — live session" section, so it hangs off
        // this synthetic key from that section's header via `InfoLabel`, not a `Feature`.
        "liveAppearance": "Acts on the connected phone now, on whichever projection is live (same as the sun/moon in each window's title bar). Needs a session; the choice is remembered and re-sent on reconnect. The badges are the Controls router's own availability per projection — hover one for the reason. The Appearance theme above is the profile's standing choice, not this.",
        "statusBar": "Status-bar elements the car draws itself, so the phone should hide them.",
        "dpi": "Panel density in dots per inch; 160 is the reference (mdpi) scale.",
        "diagonalInches": "Physical panel diagonal, for the density check. 0 = unknown. Not sent to either phone.",
        // Extended 2026-09-08 (DESIGN.md §11.4 TEN-WORD RULE sweep) to fold in the inline caption
        // that used to sit under the restriction checkboxes (VehicleTab.swift, was 16 words,
        // always visible when restrictions are declared): "Switched on and off at runtime in
        // Window ▸ Controls ▸ UI; this is the declaration." Facts preserved: the full menu path and
        // that this list is the declaration (vs. the runtime switch).
        "restrictions": "What the phone withholds while driving; switched live in Window ▸ Controls ▸ UI — this is the declaration.",
        "restrictionsDeclared": "Off = each phone's own default restrictions (the proven posture). On = exactly what's ticked below.",
        "voiceRateHz": "Voice/assistant audio sample rate; 48 kHz is clearer than the 16 kHz floor.",
        "telephonyOverProjection": "Route call audio over the projection link instead of car Bluetooth. Experimental.",
        "metadataFeeds": "Live feeds the head unit consumes: now-playing, navigation, call state.",
        "aaFitPanelWithMargins": "Fit a non-tier panel with cropped margins; off snaps smaller and letterboxes.",
        "aaPreferHEVC": "Android Auto already uses H.265 above 1080p. This asks for it at 1080p and below as well, for the bitrate saving.",
        "brandingLabel": "The vehicle maker's name shown on the projected home screen.",
        // Neutral display keys. Of the CarPlay-worded originals only `safeArea` (the alt stream's
        // insets, CarPlay-exclusive) and `altResolution` (the alt stream's size) still have a
        // control; `mainResolution`, `maxFPS` and `enablesHEVC` were removed 2026-09-04 because
        // nothing looked them up once the neutral rows replaced their controls (finding 2).
        "panelResolution": "The panel's exact pixel grid; also where touch coordinates are reported. A change needs a fresh session.",
        "panelFrameRate": "Highest frame rate the panel is driven at: 30 or 60.",
        "panelInsets": "Edges the projected UI keeps clear, for curved or occluded corners.",
        "hevcAllowed": "On by default — H.265 gives the same picture at a lower bitrate. Turn it off only for a decoder that handles H.264 more reliably.",
        "altDisplay": "A second projected surface, like an instrument cluster, with its own size/rate.",
        // Neutral input keys — the facts about the car's control surfaces. These are the ONLY
        // descriptions the input controls have: the CarPlay-worded `*Support` / `primaryInput`
        // entries were removed 2026-09-04 (finding 2) because no control looked them up any more;
        // the CarPlay YAML keys behind these rows survive only as `inertKeys` members, which feed
        // the CarPlay-badged caption via `isInert`. What each projection makes of a surface is
        // FeatureMatrix `.inputDevices` (keycodes for Android Auto, HID descriptors for CarPlay).
        //
        // `primaryInputDevice` STATES ITS SIDE EFFECTS because the model has no separate field for
        // the primary: it is derived from which surfaces exist (VehicleConfigModel+Profile.swift,
        // DESIGN.md §8), so "Touchpad" can only be shown while no touchscreen is declared. A
        // touchpad that is primary NEXT TO a touchscreen is not representable without a new
        // persisted key — that combination is flattened to "Touchscreen" on import.
        "primaryInputDevice": "Main input surface. Choosing Touchpad also turns the touchscreen off entirely.",
        "inputTouchscreen": "Off = no touchscreen declared to either projection. On = primary input unless a knob is present.",
        "inputTouchMultiTouch": "The touchscreen reports two fingers at once. Untested — verify on hardware.",
        "inputTouchCancel": "The touchscreen can cancel an in-progress touch (palm rejection).",
        "inputTouchpad": "The car has a remote touchpad driving a focus cursor.",
        "inputTouchpadButtons": "The touchpad also reports press/click buttons.",
        "inputKnob": "A rotary controller: turn to scroll, press to select.",
        "inputKnobHomeBack": "The knob has dedicated Home and Back presses.",
        "inputKnobNudge": "The knob tilts four ways as well as rotating, for grid navigation.",
        "inputDPad": "Four- or eight-way directional buttons with a select.",
        "inputMediaButtons": "Play/pause and skip transport keys on the car.",
        "inputTelephonyButtons": "Accept-call and end-call keys on the car.",
        "inputSteeringWheel": "Directional and select buttons on the steering wheel.",
        "engineTypesNeutral": "What powers this vehicle; a hybrid picks two (e.g. gasoline + electric).",
        "chargingConnectorsNeutral": "Which charging connectors this vehicle has, and optionally their power (0 = unstated).",
        "safeArea": "Insets CarPlay keeps interactive UI inside; 0 = flush with the panel.",
        "drawUIOutsideSafeArea": "When enabled, wallpaper is displayed outside the safe area, replacing the normal black background.",
        "viewArea2Enabled": "A second main view area; both are pushed and iOS picks which is live. Dock shows a resize button.",
        "viewArea2Rect": "Area 1's size and origin, in even pixels, fully inside the panel.",
        "viewAreaAnimMs": "How long CarPlay animates the Dock resize between the two view areas, in milliseconds (1000–10000). iOS honours the value literally — device-proven at both ends. "
            + "The box answers updateViewArea with this as animationDurationMillis. 3000 is the box's own default and is not sent; "
            + "1000 was visibly faster on hardware (2026-09-09).",
        "altResolution": "Pixel size of the secondary (instrument-cluster/nav) video stream.",
        "enablesVideoPlayback": "Advertises allowVideoPlayback so iOS can stream arbitrary fullscreen video apps, not just CarPlay UI.",
        "enablesMainBufferedAudio": "Advertises mainBufferedInfo: a ~2-min resilience buffer for drops. iOS disables it over wired USB; wireless can go silent — experimental.",
        // Extended 2026-09-08 (DESIGN.md §11.4 TEN-WORD RULE sweep) with the three per-mode
        // summaries formerly rendered inline whenever audioMode != "custom"
        // (AudioLabels.modeSummary, VehicleTab.swift — 14/12/16 words apiece), moved here verbatim.
        "audioFormats": "The exact audio capabilities the box advertises to iOS. Auto: PCM over wired · full AAC set over wireless (matches how the phone connects). Wired: PCM 16k/48k on types 100/101 — the wired media path (no audioType). Wireless: 8 entries: AAC-LC media (102) · AAC-ELD Siri/mic (100) · AAC-ELD alert (100/101) · PCM compatibility.",
        "metadataTier": "What the accessory declares to iOS at identification. Applies at next iAP2 link — unplug/replug, not just reconnect.",
        "accessoryName": "The name shown on the iPhone; default is CarLink + last 4 of the Wi-Fi address. Not yet applied by the adapter.",
        // Added 2026-09-08 (DESIGN.md §11.4 TEN-WORD RULE sweep), moved verbatim off
        // VehicleTab.swift's OEM-icon footnote (was 35 words, drawn unconditionally whenever
        // Identity is expanded — the worst offender in that sweep).
        "oemIconEnabled": "PNG, square (Apple ships 120/180/256). Static config — takes effect on the next connect. The config is emitted only when advertised AND an image is set; the maker label above rides inside it as oemIconLabel.",
        // Added 2026-09-08 (same sweep), moved verbatim off the false-branch caption under "Show
        // icon in CarPlay" (was 32 words); the inline caption keeps only the ≤10-word per-state
        // sentinel fact (oemIconVisible: true/false).
        "oemIconVisible": "iOS hides the icon (oemIconVisible: false is sent). Use this to hide it — turning off \"Advertise OEM icon config\" only stops sending the config, which leaves the last icon on screen.",
        "vehicleStatusEnabled": "Declares live vehicle status support. Greyed — the adapter can't serve it yet.",
        "metadataSkip": "Feature names to drop from the declaration above (adapter's feature-table names); unrecognized ones are ignored.",

        // ADAPTER-TAB ENTRIES, RE-ADDED 2026-09-08 under the ten-word rule (DESIGN.md §11.4).
        // The header above records that adapter entries were DELETED on 2026-09-04 (defect 1) — read
        // that note before assuming this reverts it. It does not. Those entries were deleted for being
        // CARPLAY-WORDED ("Advertise the box for wireless CarPlay…") on controls that gate BOTH
        // projections; the objection was to the protocol claim, not to the key existing. These four are
        // protocol-NEUTRAL and make no per-protocol claim — that stays `FeatureSupport.effect`, rendered
        // by the badges. They exist because the ten-word rule evicted their text from inline footnotes
        // and it had to land somewhere; every one of them is a fact that does not survive a ten-word cut.
        "wirelessRadios": "One radio gate for every wireless projection: the radios come up when this app connects and idle when it disconnects. Wired USB projection is unaffected either way — the box takes whichever transport connects first.",
        "wifiAccessPoint": "On is the default and adds nothing to the pushed document; off is written as wifi_ap: false. Off leaves wireless projection without a Wi-Fi leg unless the vehicle's own hotspot provides one.",
        "adapterControls": "Restart adapter reboots the CCPA — the reliable recovery if Bluetooth wedges. It interrupts any live session. NCM mode reboots the box as a USB network device for ssh maintenance; projection stays off until it is returned with `rm /script/ncm_only; reboot` over ssh.",
        "boxLogStream": "Streams the box's universal log (/tmp/box.log) over OCBM CH_LOG into Window ▸ Box Log, and into this app's own combined session log. Re-armed automatically after every SUBSCRIBE.",
        "audioFormatRow": "One advertised capability: stream type (100 mic, 102 media), audioType, input/output codec.",
        // The effect statement is the mechanical `inertKeys` marker, not prose here; what stays is
        // the project decision, which no key list can carry.
        "enablesEnhancedSiri": "Publishes enhancedSiriInfo: hardware Siri-button support (basic Siri already listens via mic regardless). Not pursued.",
        "enablesUIAppearance": "Lets the head unit drive CarPlay's look. Enabled in every Apple template.",
        "enablesMapAppearance": "Lets the head unit drive map appearance (mapAppearanceUpdate, changeMapZoomLevel); needed for the alt/cluster map stream.",
        "enablesCornerMasks": "Declares the display can be masked at the corners; the car streams per-corner opaque bitmaps at runtime (CarPlay's cutout).",
        "enablesViewAreas": "Support for view areas / safe areas within the display.",
        "enablesFocusTransfer": "Focus can move between CarPlay and the head unit's own UI.",
        "enablesUIContext": "Publishes which app/screen is showing so the car can react.",
        "enablesUISync": "Keeps certain UI state in step between CarPlay and the head unit. [inferred]",
        "enablesFileTransfer": "Lets iOS push asset files to the accessory. [inferred]",
        "enablesLogTransfer": "Advertises logTransfer, a diagnostic log archive (advertise/negotiate is device-proven wired). Box never serves one.",
        "enablesVehicleDataProtocol": "Advertises vehicleStateProtocol, opening the two-channel Vehicle Data Protocol (VDC) for route status and vehicle state.",
        "enablesDCX": "\"DCX\" — only the property name exists in the simulator; no CarPlaySDK string or wire mapping. Leave default.",
    ]
}

// MARK: - Feature → FieldInfo key

/// The `FieldInfo.text` key carrying a `Feature`'s neutral, above-the-fold description — the same
/// key its own control already passes to `InfoLabel`/`InfoToggle` in `VehicleTab.swift` /
/// `AdapterTab.swift` today. There is no single generator for this map (each row's key was authored
/// by hand before `FieldPopover` existed), so it is kept in step with those call sites here.
/// The six Adapter features (`wirelessRadios` … `appDrivenSetup`) have no `FieldInfo` entry at all
/// (DESIGN.md §1: their prose is `FeatureSupport.effect`, not a tooltip) — their key resolves to
/// `rawValue`, which is deliberately absent from `descriptions`, so `FieldPopover` renders with no
/// above-the-fold paragraph for them, never a placeholder.
extension Feature {
    var fieldInfoKey: String {
        switch self {
        case .headUnitName:          return "name"
        case .branding:              return "brandingLabel"
        case .panelGeometry:         return "panelResolution"
        case .frameRate:             return "panelFrameRate"
        case .pixelDensity:          return "dpi"
        case .insets:                return "panelInsets"
        case .videoCodec:            return "hevcAllowed"
        case .altDisplay:            return "altDisplay"
        case .theme:                 return "theme"
        case .statusBar:             return "statusBar"
        case .driverPosition:        return "driverPosition"
        case .drivingRestrictions:   return "restrictions"
        case .powertrain:            return "engineTypesNeutral"
        case .inputDevices:          return "primaryInputDevice"
        case .audioProfile:          return "voiceRateHz"
        case .metadataFeeds:         return "metadataFeeds"
        case .wirelessRadios, .hotHandover, .pairing, .wifiAccessPoint,
             .androidAutoProjection, .appDrivenSetup:
            return rawValue
        }
    }
}

// MARK: - FieldPopover (Phase 3, DESIGN.md §11.2/§11.3)

/// The ONE popover a control's (i) or one of its projection badges opens — identical content either
/// way, so nobody needs to know which icon carries which fact (§11.3). Section 1 (`title` +
/// `FieldInfo.text[key]`) is ABOVE the fold; this is where the CarPlay-only ⚠️ inert marker
/// (baked into `FieldInfo.text` by `inertKeys` above) lives, and it must never be demoted below the
/// fold or read as if it applied to Android Auto. Section 2 is one block per projection: badge +
/// level word + `FeatureSupport.effect` + `VerificationGlyph`, with the former `ValueNoteLabel`
/// content folded in when `currentValue` carries a note, then the vendor term / schema key / format
/// footer.
///
/// `feature == nil` degrades to EXACTLY today's `InfoLabel` behaviour: `label()` is the whole
/// trigger, and the popover (when `FieldInfo.text[key]` exists) is the plain 300pt paragraph. This
/// is what lets `InfoLabel`/`InfoToggle` become thin wrappers with no interface change for their
/// ~57 call sites in `VehicleTab.swift`/`AdapterTab.swift`.
struct FieldPopover<Label: View>: View {
    let title: String
    let key: String
    let feature: Feature?
    var currentValue: String? = nil
    @ViewBuilder let label: () -> Label

    @State private var show = false

    init(title: String, key: String, feature: Feature? = nil, currentValue: String? = nil,
         @ViewBuilder label: @escaping () -> Label) {
        self.title = title
        self.key = key
        self.feature = feature
        self.currentValue = currentValue
        self.label = label
    }

    private var info: String? { FieldInfo.text[key] }

    var body: some View {
        if feature == nil && info == nil {
            // Nothing to show — today's InfoLabel drew the bare label with no icon at all.
            label()
        } else {
            Button { show.toggle() } label: { label() }
                .buttonStyle(.plain)
                .accessibilityLabel("About \(title)")
                .popover(isPresented: $show, arrowEdge: .trailing) {
                    if let feature {
                        FieldPopoverContent(title: title, feature: feature, info: info, currentValue: currentValue)
                    } else if let info {
                        Text(info).font(.callout).padding(12).frame(width: 300)
                    }
                }
        }
    }
}

/// Popover body when `feature` is known: sections 1–3 of §11.3's consolidated popover.
private struct FieldPopoverContent: View {
    let title: String
    let feature: Feature
    let info: String?
    let currentValue: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.headline)
            if let info {
                Text(info).font(.callout).fixedSize(horizontal: false, vertical: true)
            }
            Divider()
            VStack(alignment: .leading, spacing: 10) {
                ForEach(FeatureMatrix.supports(feature), id: \.projection) { entry in
                    FieldPopoverProjectionBlock(feature: feature, projection: entry.projection,
                                                 support: entry.support, currentValue: currentValue)
                }
            }
        }
        .padding(12)
        .frame(width: 340)
    }
}

/// One projection's block: badge + level word, effect, provenance, the folded-in value note when
/// `currentValue` matches, and the vendor term / schema key / format footer (§11.3's "below the
/// fold" line, repeated per projection since the two vendors' terms differ).
private struct FieldPopoverProjectionBlock: View {
    let feature: Feature
    let projection: Projection
    let support: FeatureSupport
    let currentValue: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                ProjectionBadge(projection: projection, level: support.level)
                Text(support.level.word).font(.caption).foregroundStyle(support.level.tint)
            }
            Text(support.effect).font(.caption).fixedSize(horizontal: false, vertical: true)
            VerificationGlyph(verification: support.verification)
            // Refuted values render their note VERBATIM, not just on hover — the note is the
            // "do not retry without new evidence" record (DESIGN.md §11.3).
            Text(support.verification.note)
                .font(.caption2).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if let currentValue,
               let note = FeatureMatrix.valueNote(for: feature, on: projection, value: currentValue) {
                HStack(spacing: 4) {
                    Text(currentValue).font(.system(.caption2, design: .monospaced))
                    VerificationGlyph(verification: note.verification)
                }
                Text(note.verification.note)
                    .font(.caption2).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Text("\(support.vendorTerm) · \(support.vendorKey ?? "— (nothing emitted)") · \(projection.schemaName)")
                .font(.caption2).foregroundStyle(.tertiary)
        }
    }
}

// MARK: - InfoLabel / InfoToggle (survive as thin FieldPopover wrappers, byte-compatible)

/// A form-row label with an (i) button that reveals the field's explanation in a popover.
/// **Signature unchanged** (34 `InfoToggle` + ~23 `InfoLabel` call sites in `VehicleTab.swift`).
/// Reimplemented on top of `FieldPopover(feature: nil)`, which is defined to degrade to exactly
/// this shape: the title is NOT itself part of the trigger (matching today's behaviour — only the
/// (i) icon is clickable), and the icon exists at all only when `FieldInfo.text[key]` has an entry.
struct InfoLabel: View {
    let title: String
    let key: String
    var body: some View {
        HStack(spacing: 5) {
            Text(title)
            if FieldInfo.text[key] != nil {
                FieldPopover(title: title, key: key) {
                    Image(systemName: "info.circle").foregroundStyle(.secondary)
                }
            }
        }
    }
}

/// A Toggle whose label carries an (i) info popover.
struct InfoToggle: View {
    let title: String
    let key: String
    @Binding var isOn: Bool
    var body: some View { Toggle(isOn: $isOn) { InfoLabel(title: title, key: key) } }
}

