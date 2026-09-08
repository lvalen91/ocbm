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
    private static let descriptions: [String: String] = [
        // Neutral `identity.headUnitName`. Per-protocol scope is FeatureMatrix `.headUnitName`: for
        // CarPlay it is the config's `name` only (the box derives the iOS-visible accessory name from
        // MAC + serial; the CarPlay-exclusive "Accessory name" below overrides that), for Android Auto
        // it IS the advertised head-unit name (display_name + headunit_info make/model). The old text
        // called it "config metadata only", which was the CarPlay half stated as the whole (defect 3).
        "name": "The name the phone knows this head unit by. Each projection renders it its own way — see the badges beside the field. Kept short: it rides every pushed configuration.",
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
        "driverPosition": "Which side of the car the driver sits on. Decides which edge the projected app rail and driver-focused layout favour. Android Auto takes all three values; CarPlay's key is a right-hand-drive boolean, so Centre reaches it as left. Centre has not been tried on a phone on either projection.",
        // Protocol-free since 2026-09-04: the old tail claimed the CarPlay appearance capabilities
        // "decide whether iOS honours" this value, but nothing in the CarPlay path reads `theme` —
        // CarPlay's day/night is the live `ControlsBridge.nightModeOn` (ApKey.night). What each
        // projection does with it is `FeatureMatrix` `.theme`, on the badges.
        "theme": "The profile's standing choice of light or dark projected UI, or Auto to follow this Mac's own appearance. The badges say how each projection receives it. The live-session switches further down act on the connected phone now, independently of this value.",
        "statusBar": "Status-bar elements the car draws itself, so the phone should not: clock, signal strength, battery. Recorded in the profile; a projection sends it only when its wire field for it is confirmed — the badges say which.",
        "dpi": "Dots per inch of the panel, used by projections that scale their UI from a density rather than from the pixel size. 160 is the reference (mdpi) density; a higher value draws larger UI on the same pixel grid.",
        "diagonalInches": "Physical diagonal of the panel, for the record and for the implied-density check beside it. 0 = unknown. Not sent to either phone.",
        "restrictions": "What the phone withholds while the car is moving. Declared here; switched on and off at runtime in Window ▸ Controls. Each row says which projection can express it — a projection with no equivalent ignores that row.",
        "restrictionsDeclared": "Off = declare nothing and let each phone apply its own default restriction set (the proven posture). On = declare exactly the rows ticked below.",
        "voiceRateHz": "Sample rate of the voice / assistant audio path. 48 kHz gives noticeably clearer prompts than the 16 kHz floor; the badges say which projection negotiates it directly and which reaches it through its own audio table.",
        "telephonyOverProjection": "Route phone-call audio over the projection link instead of the car's own Bluetooth hands-free profile. An experiment: leave off unless you are testing it.",
        "metadataFeeds": "Which live feeds the head unit consumes from the phone: now-playing, turn-by-turn navigation, call state. Each is one service declaration on projections that declare them individually; the CarPlay tier picker below governs CarPlay's declaration as a whole.",
        "aaFitPanelWithMargins": "When the panel is not one of Android Auto's fixed video sizes, declare the enclosing size and tell the phone to keep its UI inside a panel-shaped sub-rectangle (margins), which this app crops away and scales to the panel. Off = snap to the nearest smaller size instead and letterbox.",
        "aaPreferHEVC": "Ask the phone for H.265 even at sizes where H.264 would do. Sizes above 1080p are H.265-only regardless; this only changes the codec at and below 1080p.",
        "brandingLabel": "The vehicle maker's name shown on the projected home screen. A projection with a custom-icon slot shows it beside the icon; one without shows the name alone.",
        // Neutral display keys. Of the CarPlay-worded originals only `safeArea` (the alt stream's
        // insets, CarPlay-exclusive) and `altResolution` (the alt stream's size) still have a
        // control; `mainResolution`, `maxFPS` and `enablesHEVC` were removed 2026-09-04 because
        // nothing looked them up once the neutral rows replaced their controls (finding 2).
        "panelResolution": "The exact pixel grid of the panel the projection fills; also the space touch coordinates are reported in. A change needs a fresh session. Each side 480–3840 px, and at least 800×480 for a landscape panel or 480×800 for a portrait one (orientation from the panel's own aspect) — an out-of-range value shows in red with what Save will store, and a note under the field records any correction.",
        "panelFrameRate": "Highest frame rate the panel is driven at. Both projections offer exactly 30 and 60; a higher rate is smoother and costs more decode and link bandwidth.",
        "panelInsets": "Pixels from each edge that the projected UI keeps clear, for curved or occluded panel edges. The video still fills the whole panel; only interactive UI is held inside the box. 0 = flush. The projections that honour it are shown by the badges.",
        "hevcAllowed": "Whether the phone may encode the projection as H.265 (HEVC) rather than H.264. Needs the decoder on this Mac, which is present; the badges say how each projection uses the permission.",
        "altDisplay": "A second projected surface such as an instrument cluster or a navigation strip, with its own size and rate. Leave off for a single-screen head unit.",
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
        "primaryInputDevice": "The control surface the driver mainly uses. Choosing Touchscreen turns the touchscreen on (a touchpad may stay present beside it). Choosing Touchpad turns the touchpad on AND the touchscreen OFF — both projections then see a controller-only car; there is no way to declare a touchpad as primary next to a touchscreen. Choosing Rotary knob turns the knob on and leaves the other surfaces as they are.",
        "inputTouchscreen": "The car has a touchscreen. Off = no touchscreen is declared to either projection, and the phone drives its UI from the other surfaces only (focus / keycodes). Turning it on makes it the primary input unless the rotary knob is; turning it off with Touchpad present makes the touchpad primary.",
        "inputTouchMultiTouch": "The touchscreen reports two fingers at once. Default off — multi-finger gestures are untested on this project; enable deliberately and verify on hardware before shipping it.",
        "inputTouchCancel": "The touchscreen can cancel a touch in progress (palm rejection, interrupted gestures), so an aborted touch is not taken as a tap.",
        "inputTouchpad": "The car has a remote touchpad whose finger position drives a focus cursor. Usually the primary input on cars that have one.",
        "inputTouchpadButtons": "The touchpad also reports press/click buttons, not just position.",
        "inputKnob": "The car has a rotary controller: turn = scroll or rotate, press = select. Prerequisite for the two knob options.",
        "inputKnobHomeBack": "The knob has dedicated Home and Back presses.",
        "inputKnobNudge": "The knob tilts in four directions as well as rotating, for grid navigation.",
        "inputDPad": "Four- or eight-way directional buttons with a select, for list navigation.",
        "inputMediaButtons": "Play/pause and skip transport keys on the car.",
        "inputTelephonyButtons": "Accept-call and end-call keys on the car.",
        "inputSteeringWheel": "Directional and select buttons on the steering wheel.",
        "engineTypesNeutral": "What powers this vehicle. A hybrid is genuinely two selections (for example gasoline AND electric). Selecting nothing leaves each projection's own default. The badges say which projection currently receives it and when it takes effect.",
        "chargingConnectorsNeutral": "Which charging connectors this vehicle physically has, and optionally how fast each can charge (0 = unstated). Only meaningful for electric or plug-in hybrid vehicles; one row per connector type.",
        "safeArea": "Insets (px from each edge) of the box CarPlay keeps its UI inside. The video still fills the whole resolution; only interactive UI is held within the safe box — for curved/irregular panels where the corners/edges are occluded. 0 = flush (no inset). iOS honors this whenever View areas is negotiated — a real (non-zero, non-full-panel) inset here arms it automatically, even with View areas off, matching the adapter's own local behavior.",
        "drawUIOutsideSafeArea": "Let CarPlay draw non-interactive UI in the gap between the full frame and the safe box. Off (default) = keep all UI strictly inside the safe area.",
        "viewArea2Enabled": "Declare a SECOND main view area so the CarPlay Dock shows the resize button and the user can switch between the full panel (area 0) and this smaller rect (area 1). Both are pushed in the config; iOS picks which is live. Off = one layout, no resize button.",
        "viewArea2Rect": "Area 1 in panel pixels: width, height and the top-left origin. The origin is free (a corner, an edge, or floating). All four values must be EVEN and the rect must fit inside the panel — an odd value or an overflow tears the session down on hardware, so such a rect is not pushed at all. Below the product floor (800×480 landscape / 480×800 portrait, by the rect’s own aspect) CarPlay blacks the area out instead. Area 1’s safe area is the whole rect; there are no separate insets for it.",
        "altResolution": "Pixel size of the secondary (instrument-cluster / navigation) video stream — typically a smaller cluster screen. Omit for single-screen units.",
        "enablesVideoPlayback": "Advertises allowVideoPlayback so iOS can stream arbitrary fullscreen video apps (media/streaming), not just the CarPlay UI. Off = UI/nav only.",
        "enablesMainBufferedAudio": "Advertises mainBufferedInfo (~2-min media buffer, streamed from iPhone faster than real time) for playback resilience on an UNINTENTIONAL drop — media keeps playing while the link recovers. Does NOT improve audio quality. Advertise/negotiate is device-tested (wired: iOS negotiates it DISABLED over USB — it is a wireless-drop remedy). The box does not serve a buffered stream yet (the ~2-min buffer will live in THIS app — docs/carplay/04_CAPABILITIES_AND_CONFIG.md's owner-corrected architectural model, 2026-08-07), so on WIRELESS this is a deliberate per-session experiment: if iOS moves media to the buffered stream, media goes silent until you turn this off and reconnect. Default off; applies at the next connection.",
        "audioFormats": "The exact set of audio capabilities the box advertises to iOS (the /info audioFormats). iOS negotiates one entry per audioType from this set. Auto = match the transport (PCM over USB, the AAC set over wireless). Presets are ready-made sets; Custom lets you author any codec/rate/stream-type combination to test a specific head-unit audio config.",
        "metadataTier": "Which metadata feeds the accessory DECLARES to iOS in its iAP2 identification (and then subscribes to): now playing, call state, route guidance, and so on. Proven is the declaration a real iPhone accepted on 2026-07-25 and is what the adapter uses when nothing is pushed — leave it here unless you are deliberately testing a wider set. Extended adds the full paired Start/Stop set — accepted once on a wireless session's tunnel Identify (2026-07-25), but NOT proven on the wired Identify, where an earlier extended form was rejected. All declares everything in the capability table. ⚠️ iOS validates this: a declaration it rejects kills the whole identification for that connection and cannot be retried within it, so raise the tier one step at a time and watch the phone's own log (idevicesyslog -p accessoryd). Applies at the next iAP2 link — unplug/replug the phone, not just a reconnect of this app.",
        "accessoryName": "The name this adapter shows as on the iPhone. Leave empty to keep the adapter's built-in per-box name (CarLink plus the last four characters of its Wi-Fi address), which is what ships today and is what keeps two adapters distinguishable. A name you set is used verbatim, so make it distinct yourself. ⚠️ Not yet applied by the adapter — it is stored and pushed, but changing the advertised name touches the AirPlay /info name, the Bonjour service name and the iAP2 identification together, so it is enabled in a later step with the phone's own log being watched.",
        "vehicleStatusEnabled": "Declares that this vehicle can report live status to the phone — range, temperatures, charge state and so on. ⚠️ DISABLED until the adapter can service it. The adapter does not yet declare the messages that carry this data, so announcing the capability without them is exactly the kind of inconsistency iOS rejects, and a rejected identification kills CarPlay for that connection and cannot be retried until the phone is replugged. The control is shown greyed rather than hidden so you can see the capability is planned; it will unlock when the adapter declares the messages that carry this data.",
        "metadataSkip": "Comma-separated feature names to DROP from the declaration above (e.g. call_history). Use this to narrow a tier that iOS rejected, rather than dropping back a whole level. Names are the adapter's feature-table names; anything unrecognized is ignored. Leave empty for the full tier.",
        "audioFormatRow": "One advertised capability: a stream type (100 MainAudio carries the mic; 102 MainHighAudio is high-latency media (AAC-LC)), an audioType iOS routes against (e.g. media, speechRecognition), an input codec (mic capture; None = playback-only) and an output codec (playback to the box).",
        // The effect statement is the mechanical `inertKeys` marker, not prose here; what stays is
        // the project decision, which no key list can carry.
        "enablesEnhancedSiri": "Publishes enhancedSiriInfo so the vehicle's own hardware Siri button can invoke Siri (siriAction prewarm / button-down / button-up), with supported-language hints and mixable Siri audio. Off = basic Siri via the CarPlay Siri button. Either way Siri listens on the car's microphone (the speechRecognition audio path) — enhanced just adds the hardware-button trigger, language hints, and audio mixing. Not pursued on this project: it needs an independent hot-word / voice-analysis stack.",
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
    ]
}

/// A form-row label with an (i) button that reveals the field's explanation in a popover.
struct InfoLabel: View {
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
struct InfoToggle: View {
    let title: String
    let key: String
    @Binding var isOn: Bool
    var body: some View { Toggle(isOn: $isOn) { InfoLabel(title: title, key: key) } }
}

