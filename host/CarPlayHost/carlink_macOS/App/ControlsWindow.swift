// ControlsWindow.swift — the Window ▸ Controls viewer (user directive 2026-07-12), SwiftUI.
//
// Clickable buttons for the CarPlay input surfaces, grounded in Apple's CarPlay SDK (read from the
// Xcode-local CarPlaySimulator plugin — docs/20 §2):
//   • Media transport (Play/Pause/Next/Prev) rides the uid-2 Consumer-Control device (a single array
//     index; the box completes the press+release tap).
//   • Home / Back / D-pad Up/Down/Left/Right / Select ride the SEPARATE uid-3 D-Pad HIDDPad device
//     (Consumer AC Home 0x0223 / AC Back 0x0224 / Menu Up-Down-Left-Right-Pick 0x42–0x46), gated behind
//     CARPLAY_DPAD. (Home was previously a `requestUI` /command — a DIFFERENT function, "bring the
//     accessory UI forward", not the CarPlay Home button — which is why it did nothing.)
//   • Siri is press-and-HOLD over the AirPlay /command channel: `requestSiri` with siriAction
//     buttondown on press, buttonup on release (SDK `AirPlaySiriAction`).
//
// Buttons send DIRECTLY through `ControlsBridge.shared.client` (set by AppDelegate on connect) —
// no stored closures, so a reconnect can never leave a stale send path.

import AppKit
import SwiftUI

/// Bridges the SwiftUI Controls view to the live OCBM client. `client` is repointed on every
/// (re)connect by AppDelegate, so sends always reach the current session.
@MainActor
final class ControlsBridge: ObservableObject {
    static let shared = ControlsBridge()
    weak var client: OCBMClient?

    // MARK: - Protocol routing
    //
    // The Controls window expresses INTENTS — Play, Home, Back, a rotary detent, answer a call. The
    // intent is universal; only the wire differs. CarPlay takes OCBM opcodes against HID uids;
    // Android Auto takes Android keycodes on its input channel. So one window, one set of buttons,
    // and the routing happens here.
    //
    // The vocabulary below is still CarPlay's (`OCBM.mbtnPlay` …) because that is what the view
    // already speaks. Translating it to AA here — rather than inventing a third neutral enum and
    // rewriting every call site — keeps this change small; the mapping is the seam that matters.

    /// The live AA session, when Android Auto owns the box. Repointed by AppDelegate exactly like
    /// `client`, and nil for CarPlay.
    weak var aaSession: AASession?
    /// True while Android Auto is the active projection (driven by the box's CT_PROJ_MODE).
    @Published var isAndroidAuto = false

    /// Injects a touch through the SAME path a real tap takes, in the view's 0..10000 normalized
    /// space. Set by AppDelegate. Exists so a tap can be scripted (ControlServer `tap x y`) without a
    /// human at the trackpad — and it deliberately goes through the delegate rather than straight to
    /// the session, so it exercises the real coordinate scaling and clamp.
    var injectTouch: ((TouchAction, UInt32, UInt32) -> Void)?

    // MARK: - Availability
    //
    // ONE table saying which intents the ACTIVE protocol can express, consulted by both the router
    // and the view. This exists because the alternative kept failing: five separate controls shipped
    // wired to CarPlay-only commands and, under Android Auto, reached a client that did not own the
    // box and did nothing at all — the knob's Home/Back, the Day/Night toggle, Limited UI, and the
    // control-box mic. Each looked live. Each was found only by someone pressing it and reporting
    // that nothing happened.
    //
    // A control must therefore be one of two things, never a third: routed to the owning protocol,
    // or VISIBLY unavailable. Adding a protocol or an intent means updating this table, and the view
    // cannot drift from what the router can actually send.

    enum Control: CaseIterable {
        case media              // play/pause/next/prev
        case navigation         // D-Pad + Home/Back
        case knobNudge          // 4-way nudge -> D-Pad
        case knobRotate         // detent -> SCROLL_WHEEL relative event
        case call               // answer / end
        case callExtras         // mute, flash, delete, DTMF — no AA counterpart
        case assistant          // AA: MICROPHONE_1. CarPlay: Siri
        case nightMode          // AA: night_mode sensor. CarPlay: setNightMode
        case displayAppearance  // CarPlay UI/Map light-dark. AA derives it from night_mode
        case navAppearance      // CarPlay-only
        case cluster            // CarPlay cluster. AA has instrumentcluster, UNIMPLEMENTED
        case limitedUI          // AA: driving_status bitmask
        case keyframe           // AA has NO head-unit keyframe request at all
        case altDisplay         // AA projects one display
    }

    /// Can the ACTIVE protocol express this intent?
    func isAvailable(_ c: Control) -> Bool {
        guard isAndroidAuto else { return true }   // CarPlay expresses everything here
        switch c {
        case .media, .navigation, .knobNudge, .knobRotate, .call, .assistant, .nightMode, .limitedUI:
            return true
        case .displayAppearance:
            return true            // mapped onto night_mode — the only AA appearance lever
        case .callExtras, .navAppearance, .keyframe, .altDisplay:
            return false           // no AA counterpart exists
        case .cluster:
            return false           // AA supports it; WE do not yet (docs/host/02_ANDROID_AUTO.md Phase 4)
        }
    }

    /// Why a control is unavailable, for the wire readout and tooltips. Nil when it is available.
    func unavailableReason(_ c: Control) -> String? {
        guard !isAvailable(c) else { return nil }
        switch c {
        case .callExtras:      return "Android Auto has no counterpart (only Answer and End)"
        case .navAppearance:   return "Android Auto derives appearance from Night Mode"
        case .keyframe:        return "Android Auto has no head-unit keyframe request"
        case .altDisplay:      return "Android Auto projects a single display"
        case .cluster:         return "instrument cluster not implemented for Android Auto yet"
        default:               return "not available on Android Auto"
        }
    }

    /// Refuse an intent the active protocol cannot express, loudly. Returns true if handled here —
    /// the caller must NOT fall through to a transport that does not own the box.
    private func refuseIfUnavailable(_ c: Control, _ wire: String) -> Bool {
        guard let why = unavailableReason(c) else { return false }
        report(.droppedNotSubscribed, wire + " [\(why)]")
        return true
    }

    var siriAvailable: Bool { !isAndroidAuto }
    var keyframeRequestAvailable: Bool { isAvailable(.keyframe) }

    /// CarPlay media button -> Android keycode. Play and Pause both map to PLAY_PAUSE: Android's
    /// transport control is a toggle and has no distinct "play" that a head unit can send blind.
    private func aaKey(media index: UInt8) -> AACapability.Key? {
        switch index {
        // AA distinguishes all three: PLAY 0x7E, PAUSE 0x7F, TOGGLE_PLAY 0x55. They were collapsed
        // onto the toggle, which made the discrete Play and Pause buttons ambiguous — a toggle sent
        // while already playing pauses, which is the opposite of what the button says.
        case OCBM.mbtnPlay: return .mediaPlay
        case OCBM.mbtnPause: return .mediaPause
        case OCBM.mbtnPlayPause: return .mediaPlayPause
        case OCBM.mbtnNext: return .mediaNext
        case OCBM.mbtnPrev: return .mediaPrevious
        default: return nil
        }
    }

    /// CarPlay D-Pad/nav action -> Android keycode.
    private func aaKey(nav action: UInt8) -> AACapability.Key? {
        switch action {
        case OCBM.navUp: return .dpadUp
        case OCBM.navDown: return .dpadDown
        case OCBM.navLeft: return .dpadLeft
        case OCBM.navRight: return .dpadRight
        case OCBM.navSelect: return .dpadCenter
        case OCBM.navHome: return .home
        case OCBM.navBack: return .back
        default: return nil
        }
    }

    /// CarPlay telephony index -> Android keycode.
    private func aaKey(telephony index: UInt8) -> AACapability.Key? {
        switch index {
        case OCBM.telAnswer: return .phone
        case OCBM.telEnd: return .callEnd
        default: return nil
        }
    }

    /// Send an intent as an AA key tap. Returns false when AA is not active or cannot express it,
    /// so the caller falls through to the CarPlay path.
    private func sendAA(_ key: AACapability.Key?, _ wire: String) -> Bool {
        guard isAndroidAuto else { return false }
        guard let key, let session = aaSession else {
            report(.droppedNotSubscribed, wire + " [Android Auto cannot express this]")
            return true   // handled: AA owns the box, so the CarPlay path must NOT also fire
        }
        session.tapKey(key)
        report(.sent, wire + " [AA \(key)]")
        return true
    }

    /// Trigger the Google Assistant — AA's counterpart to Siri.
    func assistant() {
        let wire = "InputReport{key KEYCODE_SEARCH}"
        guard isAndroidAuto, let session = aaSession else {
            report(.droppedNotSubscribed, wire); return
        }
        session.tapKey(.microphone1)
        report(.sent, wire)
    }

    /// True while the OCBM session is live (AppDelegate sets it from the coordinator's streaming
    /// signal; endSession() clears it). `client != nil` alone is NOT enough to claim a control
    /// reached the phone — the client exists from USB attach but silently drops sends until the box
    /// accepts the SUBSCRIBE and streams.
    @Published var sessionActive = false {
        didSet {
            // Re-push the user's Light/Dark + Night Mode when the PHONE A/V session comes up — NOT on the
            // OCBM `subscribed` edge. `send_command` on the box only reaches iOS once the AirPlay event
            // channel exists (established during the phone session's RECORD, well before first A/V frame);
            // OCBM `subscribed` fires at USB connect, which for wireless can be long before — or entirely
            // decoupled from — a phone session, so a re-push keyed there gets silently dropped and a phone
            // reconnect while OCBM stays subscribed never re-pushes at all. `sessionActive` cycles with the
            // phone session on every transport, so it is the correct trigger. No-op unless the user ever
            // set appearance (see `syncAppearance`).
            if sessionActive && !oldValue { syncAppearance() }
        }
    }

    /// Truthful SUBSCRIBE state, fed by `client.onSubscriptionState` (AppDelegate hops it to main). This
    /// — NOT sessionActive/first-frame — is what proves a control can reach the wire: during the
    /// SUBSCRIBE→first-frame window the box IS accepting input, so gating "did it send" on streaming would
    /// wrongly report those as dropped. `subscribed && !sessionActive` is the "establishing" window.
    @Published var subscribed = false

    /// The last control sent + the literal CarPlay wire mapping it produced, for the live readout.
    @Published var lastSent: String = "—"
    /// The cluster (type-111) content iOS is asked to encode — matching the CarPlay Simulator's own
    /// realtime Content selector (None / Instruction Card / Map / Navigation App). Each is a `requestUI`
    /// with a distinct URL; None is `stopUI`.
    @Published var clusterContent: ClusterContent = .none
    /// Whether limited UI (Drive restriction) is active.
    @Published var limitedUIOn: Bool = false

    /// Convenience for the alt-frame gate: is any cluster content requested (window may open)?
    var navVideoOn: Bool { clusterContent != .none }

    // EVERY lastSent update routes through `report(_:_:)`, now driven by the send's ACTUAL outcome (C3)
    // rather than a guess: the client tells us whether the control reached the wire (`sent`), was
    // swallowed for lack of a session (`droppedNotSubscribed`), or hit a USB write failure. Reporting
    // the outcome kills the old lie where a pre-subscribe command showed "Sent to iPhone: …".
    private func report(_ outcome: SendOutcome, _ wire: String) {
        switch outcome {
        case .sent:
            // Subscribed ⇒ it reached the box. If A/V isn't streaming yet the session is still coming up,
            // so distinguish that window (the input is delivered either way).
            lastSent = sessionActive ? "Sent — \(wire)" : "Sent (session establishing) — \(wire)"
        case .droppedNotSubscribed:
            lastSent = "⚠︎ Dropped (no session) — \(wire)"
        case .writeFailed:
            lastSent = "⚠︎ Failed (USB write) — \(wire)"
        }
    }
    // A nil client means no session at all — report the drop directly (the send would never fire, so its
    // completion never would either). Otherwise the outcome comes back through the client's completion.
    func media(_ index: UInt8, _ wire: String) {
        if sendAA(aaKey(media: index), wire) { return }
        guard let client else { report(.droppedNotSubscribed, wire); return }
        client.sendMediaButton(index) { [weak self] outcome in self?.report(outcome, wire) }
    }
    func nav(_ action: UInt8, _ wire: String) {
        if sendAA(aaKey(nav: action), wire) { return }
        guard let client else { report(.droppedNotSubscribed, wire); return }
        client.sendNav(action) { [weak self] outcome in self?.report(outcome, wire) }
    }
    /// Rotary Knob (uid 4) — the Simulator's navigation device: rotation (±1/detent), 4-way nudge
    /// (±127), and Select/Home/Back flags. Requires "Knob support" armed in Settings.
    func knob(flags: UInt8 = 0, nudgeX: Int8 = 0, nudgeY: Int8 = 0, rotation: Int8 = 0, _ wire: String) {
        // The knob's FLAGS carry Select/Home/Back, and ignoring them was a real bug: this panel has
        // its OWN Home and Back buttons, separate from the D-Pad panel's (which go through nav()),
        // and they were silently swallowed as "AA cannot express this". Device-proven 2026-08-27 with
        // gearhead's debug log: it received ONLY keycodes 19-22, and HOME/BACK never arrived —
        // because we never sent them. Every earlier "HOME/BACK do not work in AA" result was this,
        // not the protocol.
        //
        // A rotary DETENT is still not sent: AA carries it as a RelativeEvent with a delta (openauto
        // InputService.cpp:146 — NOT a plain button code, contrary to an earlier note), unbuilt.
        if isAndroidAuto {
            // A detent is a RELATIVE event with a signed delta, not a key — so it is sent directly
            // rather than through sendAA's key path.
            if rotation != 0, let session = aaSession {
                session.enqueueScroll(delta: rotation > 0 ? 1 : -1)
                report(.sent, wire + " [AA SCROLL_WHEEL \(rotation > 0 ? "+1" : "-1")]")
                return
            }
            let key: AACapability.Key? =
                  flags & 0x01 != 0 ? .dpadCenter          // Select
                : flags & 0x02 != 0 ? .home
                : flags & 0x04 != 0 ? .back
                : nudgeY < 0 ? .dpadUp : nudgeY > 0 ? .dpadDown
                : nudgeX < 0 ? .dpadLeft : nudgeX > 0 ? .dpadRight : nil
            if sendAA(key, wire) { return }
        }
        guard let client else { report(.droppedNotSubscribed, wire); return }
        client.sendKnob(flags: flags, nudgeX: nudgeX, nudgeY: nudgeY, rotation: rotation) {
            [weak self] outcome in self?.report(outcome, wire)
        }
    }
    /// Telephony (uid 5) — Answer/End/Flash/Mute + DTMF. Requires "Telephony buttons" armed in Settings.
    func telephony(_ index: UInt8, _ wire: String) {
        // Answer and End map to PHONE/CALL_END. Mute, Flash, Delete and the DTMF keypad have no AA
        // counterpart at all — refuse them by name instead of letting sendAA report a generic
        // "cannot express" after the fact.
        if isAndroidAuto, aaKey(telephony: index) == nil,
           refuseIfUnavailable(.callExtras, wire) { return }
        if sendAA(aaKey(telephony: index), wire) { return }
        guard let client else { report(.droppedNotSubscribed, wire); return }
        client.sendTelephony(index) { [weak self] outcome in self?.report(outcome, wire) }
    }
    func siriDown() {
        let wire = "/command requestSiri {siriAction:2 buttondown}"
        guard let client else { report(.droppedNotSubscribed, wire); return }
        client.sendCommand(OCBM.cmdSiriDown) { [weak self] outcome in self?.report(outcome, wire) }
    }
    func siriUp() {
        let wire = "/command requestSiri {siriAction:3 buttonup}"
        guard let client else { report(.droppedNotSubscribed, wire); return }
        client.sendCommand(OCBM.cmdSiriUp) { [weak self] outcome in self?.report(outcome, wire) }
    }
    /// Simple press → invoke Siri: a buttondown immediately followed by buttonup. A dedicated Siri/assistant
    /// button just needs the invoke; press-and-HOLD is only for dual-function call-pickup buttons (press =
    /// answer, hold = Siri), which we don't model here.
    /// Manual keyframe request — the same path a DETECTED video gap takes (`onVideoGap` /
    /// `onFrameDropped` -> `OCBMClient.requestKeyframe()` -> `ocbm_proto::INPUT_KEYFRAME` ->
    /// airplayd `handle_input_frame` -> `events::send_force_key_frame()`), exposed for the case the
    /// detectors miss: frames arriving and decoding, but the PICTURE wrong. Nothing new on the wire.
    /// No-op between sessions, by the box's own check.
    func requestKeyframe() {
        // AA has NO head-unit keyframe request (docs/host/02_ANDROID_AUTO.md): there is no message to send, and recovery
        // waits for the phone's periodic IDR. Sending the OCBM one during an AA session would command
        // a CarPlay path that does not own the box.
        guard keyframeRequestAvailable else { return }
        client?.requestKeyframe()
    }

    func siriPress() {
        // Route to the Assistant when Android Auto owns the box. Fixed at the SOURCE rather than at
        // the call site: the Controls window's Siri panel already swapped itself to an "Invoke
        // Assistant" button, but the video window's control-box mic icon calls straight through here,
        // so it kept sending CarPlay's /command requestSiri to a client that does not own the box and
        // did nothing at all (device-reported 2026-08-27). Any future caller gets the right transport
        // for free.
        if isAndroidAuto { assistant(); return }
        siriDown()
        siriUp()
    }

    /// Select the cluster content iOS renders into the type-111 stream, at runtime (no reconnect). The
    /// cluster display must already be advertised in /info. Instruction Card = the maneuver/ETA info
    /// cards; None releases focus + hides the window. Mirrors the Simulator's Content picker.
    func setClusterContent(_ c: ClusterContent) {
        // AA serves its cluster from NAVIGATION METADATA, not a video stream we can point at content
        // (docs/host/02_ANDROID_AUTO.md) — so there is nothing to select until we render a cluster ourselves.
        if refuseIfUnavailable(.cluster, "cluster content \(c)") { return }
        let cmd: UInt8
        switch c {
        case .none: cmd = OCBM.cmdNavStop
        case .instructionCard: cmd = OCBM.cmdNavCard
        case .map: cmd = OCBM.cmdNavStart
        case .navigationApp: cmd = OCBM.cmdNavApp
        }
        let wire = "/command \(c == .none ? "stopUI" : "requestUI") {url: \(c.url)}"
        guard let client else { report(.droppedNotSubscribed, wire); return }
        // Commit clusterContent ONLY when the command actually reached the box (.sent). Setting it up
        // front would claim a phone-side cluster state we never sent — and would re-arm the alt-window
        // gate (`navVideoOn`) for a request the phone never got. Until .sent lands the Picker's binding
        // keeps showing the real committed value.
        client.sendCommand(cmd) { [weak self] outcome in
            guard let self else { return }
            self.report(outcome, wire)
            if case .sent = outcome {
                self.clusterContent = c
                if c == .none { AltVideoWindowController.shared.sessionEnded() }
            }
        }
    }

    // MARK: - Cluster (nav) appearance flags — Speed Limit / Compass / ETA
    //
    // The showUI query elements the Simulator exposes in its Alt1 Appearance popover. Persisted; default
    // = all on (0x07, matching the box default). Relocated here from AltVideoWindowController so the
    // Nav/Alt chrome bar binds to them directly. Only the map / Nav App surfaces carry the query string,
    // so the UI disables the toggles otherwise; the box just stores the flags for the next surface.
    private static let navFlagsKey = "navAppearanceFlags"
    @Published var navAppearanceFlags: UInt8 = UInt8(
        UserDefaults.standard.object(forKey: ControlsBridge.navFlagsKey) as? Int
        ?? Int(OCBM.navApSpeedLimit | OCBM.navApCompass | OCBM.navApETA))

    /// Flip one appearance bit, persist, and push to the box (applies to the current/next cluster surface).
    func toggleNavAppearance(_ bit: UInt8) {
        if refuseIfUnavailable(.navAppearance, "nav appearance bit \(bit)") { return }
        navAppearanceFlags ^= bit
        UserDefaults.standard.set(Int(navAppearanceFlags), forKey: Self.navFlagsKey)
        client?.sendNavAppearance(navAppearanceFlags)
    }

    /// Re-push the persisted cluster-appearance flags (called when the alt decoder attaches). No-ops
    /// cleanly if not yet subscribed; the box default already matches the UI default.
    func syncNavAppearance() {
        client?.sendNavAppearance(navAppearanceFlags)
    }

    /// limitedUI — restrict the CarPlay UI as if the vehicle shifted into Drive (true) / release (false).
    func setLimitedUI(_ on: Bool) {
        // AA's nearest equivalent is driving_status — ONE bit where CarPlay has a whole catalogue of
        // what stays available while moving. Committed immediately: unlike the CarPlay path there is
        // no per-command outcome to wait on.
        if isAndroidAuto, let session = aaSession {
            session.setDrivingRestricted(on)
            limitedUIOn = on
            report(.sent, "SensorBatch{driving_status: \(on ? "restricted" : "unrestricted")}")
            return
        }
        let wire = "/command setLimitedUI {limitedUI: \(on)}"
        guard let client else { report(.droppedNotSubscribed, wire); return }
        // Commit limitedUIOn only on .sent — don't flip the toggle for a command that didn't reach the phone.
        client.sendCommand(on ? OCBM.cmdLimitedUIOn : OCBM.cmdLimitedUIOff) { [weak self] outcome in
            guard let self else { return }
            self.report(outcome, wire)
            if case .sent = outcome { self.limitedUIOn = on }
        }
    }

    // MARK: - Display appearance (Light/Dark) + Night Mode
    //
    // Mirrors the CarPlay Simulator's per-display "UI Appearance" / "Map Appearance" pickers and the
    // global "Night Mode" toggle. Wire shapes verified from Apple's CarPlaySDK (see events.rs). The box
    // does the actual /command dispatch; here we hold the user's choice, persist it, and re-push it on
    // each (re)connect. The inline titlebar sun/moon toggles a whole display (UI + Map together); the
    // Settings window exposes the individual UI / Map / Night switches.

    private enum ApKey {
        static let mainUI = "appearance.mainUIDark"
        static let mainMap = "appearance.mainMapDark"
        static let altUI = "appearance.altUIDark"
        static let altMap = "appearance.altMapDark"
        static let night = "appearance.nightMode"
        static let touched = "appearance.touched"
    }

    @Published var mainUIDark = UserDefaults.standard.bool(forKey: ApKey.mainUI)
    @Published var mainMapDark = UserDefaults.standard.bool(forKey: ApKey.mainMap)
    @Published var altUIDark = UserDefaults.standard.bool(forKey: ApKey.altUI)
    @Published var altMapDark = UserDefaults.standard.bool(forKey: ApKey.altMap)
    @Published var nightModeOn = UserDefaults.standard.bool(forKey: ApKey.night)
    /// True once the user changes any appearance control — gates whether we push appearance at all, so a
    /// user who never touches it gets iOS's native default rather than an explicit "light" we invented.
    private var appearanceTouched = UserDefaults.standard.bool(forKey: ApKey.touched)

    /// Whether the given display is currently in dark mode (UI). Drives the sun/moon icon state.
    func displayIsDark(alt: Bool) -> Bool { alt ? altUIDark : mainUIDark }

    /// Inline sun/moon: flip a whole display (UI + Map) between Light and Dark and push both.
    func toggleDisplayDark(alt: Bool) {
        setDisplayDark(alt: alt, dark: !displayIsDark(alt: alt))
    }

    /// Set both UI and Map appearance for a display, persist, and push.
    func setDisplayDark(alt: Bool, dark: Bool) {
        // Android Auto has NO per-display, UI-vs-Map appearance model — `night_mode` on the sensor
        // channel is the only lever, and gearhead derives everything from it. The sun/moon in the
        // video window's control box used to call straight through to CarPlay's
        // `/command uiAppearanceUpdate`, which under AA reaches a client that does not own the box:
        // the button did nothing at all (device-reported 2026-08-27). Map it to the sensor instead.
        if isAndroidAuto {
            guard !alt else {
                // AA projects one display; there is no alt panel to theme separately.
                report(.droppedNotSubscribed, "alt display appearance [Android Auto has one display]")
                return
            }
            guard let session = aaSession else {
                report(.droppedNotSubscribed, "SensorBatch{night_mode}"); return
            }
            // Keep the icon and the persisted state in step with what we actually sent.
            mainUIDark = dark; mainMapDark = dark
            UserDefaults.standard.set(dark, forKey: ApKey.mainUI)
            UserDefaults.standard.set(dark, forKey: ApKey.mainMap)
            nightModeOn = dark
            UserDefaults.standard.set(dark, forKey: ApKey.night)
            session.setNightMode(dark)
            report(.sent, "SensorBatch{night_mode: \(dark)}")
            return
        }
        setUIAppearance(alt: alt, dark: dark)
        setMapAppearance(alt: alt, dark: dark)
    }

    func setUIAppearance(alt: Bool, dark: Bool) {
        // Under AA there is no separate UI-vs-Map appearance: night_mode is the whole model, and
        // setDisplayDark is the path that maps onto it. Route there rather than sending a CarPlay
        // command to a client that does not own the box.
        if isAndroidAuto { setDisplayDark(alt: alt, dark: dark); return }
        if alt { altUIDark = dark } else { mainUIDark = dark }
        UserDefaults.standard.set(dark, forKey: alt ? ApKey.altUI : ApKey.mainUI)
        markTouched()
        let stream = alt ? OCBM.appearanceStreamAlt : OCBM.appearanceStreamMain
        let wire = "/command uiAppearanceUpdate {\(alt ? "alt" : "main"), \(dark ? "dark" : "light")}"
        guard let client else { report(.droppedNotSubscribed, wire); return }
        client.sendAppearance(stream: stream, dark: dark, isMap: false) { [weak self] o in self?.report(o, wire) }
    }

    func setMapAppearance(alt: Bool, dark: Bool) {
        if isAndroidAuto { setDisplayDark(alt: alt, dark: dark); return }
        if alt { altMapDark = dark } else { mainMapDark = dark }
        UserDefaults.standard.set(dark, forKey: alt ? ApKey.altMap : ApKey.mainMap)
        markTouched()
        let stream = alt ? OCBM.appearanceStreamAlt : OCBM.appearanceStreamMain
        let wire = "/command mapAppearanceUpdate {\(alt ? "alt" : "main"), \(dark ? "dark" : "light")}"
        guard let client else { report(.droppedNotSubscribed, wire); return }
        client.sendAppearance(stream: stream, dark: dark, isMap: true) { [weak self] o in self?.report(o, wire) }
    }

    func setNightMode(_ on: Bool) {
        nightModeOn = on
        UserDefaults.standard.set(on, forKey: ApKey.night)
        markTouched()
        // Android Auto has no light/dark control of its own — `night_mode` on the sensor channel IS
        // the lever, and gearhead derives its theme (and Maps' Day/Night "Auto") from it.
        if isAndroidAuto, let session = aaSession {
            session.setNightMode(on)
            report(.sent, "SensorBatch{night_mode: \(on)}")
            return
        }
        let wire = "/command setNightMode {nightMode: \(on)}"
        guard let client else { report(.droppedNotSubscribed, wire); return }
        client.sendNightMode(on) { [weak self] o in self?.report(o, wire) }
    }

    private func markTouched() {
        if !appearanceTouched {
            appearanceTouched = true
            UserDefaults.standard.set(true, forKey: ApKey.touched)
        }
    }

    /// Re-push the persisted appearance to a freshly-active phone session. No-op if the user never set it
    /// (leave iOS's default) or if there is no client yet. Alt-display sends are gated box-side on the
    /// alt screen being advertised, so they drop harmlessly when there is no cluster.
    func syncAppearance() {
        guard appearanceTouched, let client else { return }
        client.sendAppearance(stream: OCBM.appearanceStreamMain, dark: mainUIDark, isMap: false)
        client.sendAppearance(stream: OCBM.appearanceStreamMain, dark: mainMapDark, isMap: true)
        client.sendAppearance(stream: OCBM.appearanceStreamAlt, dark: altUIDark, isMap: false)
        client.sendAppearance(stream: OCBM.appearanceStreamAlt, dark: altMapDark, isMap: true)
        client.sendNightMode(nightModeOn)
    }

    /// AppDelegate.endSession(): the session is gone. Clear the gate and the stale surfaces —
    /// leftover clusterContent would re-arm the alt-window gate (`navVideoOn`) on the next connect,
    /// and a stale limitedUI toggle / lastSent readout would claim phone state that no longer
    /// exists. State-only reset: no sends (there is nothing left to send to).
    func sessionEnded() {
        sessionActive = false
        subscribed = false
        // Restore-to-off is CORRECT, not a missing feature: on teardown the phone's own cluster/limitedUI
        // state resets too, so replaying our last sticky toggles on the next connect would re-assert state
        // the phone no longer holds (and would need a live session to send anyway). Deliberately no replay.
        clusterContent = .none
        limitedUIOn = false
        lastSent = "—"
    }
}

/// The cluster content types the CarPlay Simulator exposes as a realtime picker; each maps to a
/// `maps:/car/instrumentcluster…` URL requested via requestUI (None = stopUI).
enum ClusterContent: String, CaseIterable, Identifiable {
    case none = "None"
    case instructionCard = "Instruction Card"
    case map = "Map"
    case navigationApp = "Navigation App"
    var id: String { rawValue }
    var url: String {
        switch self {
        case .none: return "—"
        case .instructionCard: return "maps:/car/instrumentcluster/instructioncard"
        case .map: return "maps:/car/instrumentcluster/map"
        case .navigationApp: return "maps:/car/instrumentcluster"
        }
    }
    var systemImage: String {
        switch self {
        case .none: return "nosign"
        case .instructionCard: return "arrow.triangle.turn.up.right.diamond"
        case .map: return "map"
        case .navigationApp: return "location.north.line"
        }
    }
}

/// The literal CarPlay wire mapping for each control — shown under every button so it's clear exactly
/// what reaches the iPhone. Grounded in the SDK (HID descriptors + FillReport, docs/20).
enum Wire {
    static let play = "HID uid 2 · Consumer 0x00B0 Play"
    static let pause = "HID uid 2 · Consumer 0x00B1 Pause"
    static let playPause = "HID uid 2 · Consumer 0x00CD Play/Pause"
    static let next = "HID uid 2 · Consumer 0x00B5 Scan Next"
    static let prev = "HID uid 2 · Consumer 0x00B6 Scan Prev"
    static let home = "HID uid 3 · D-Pad byte0 bit0 AC Home 0x0223"
    static let back = "HID uid 3 · D-Pad byte0 bit1 AC Back 0x0224"
    static let up = "HID uid 3 · D-Pad byte1 bit2 Menu Up 0x42"
    static let down = "HID uid 3 · D-Pad byte1 bit3 Menu Down 0x43"
    static let left = "HID uid 3 · D-Pad byte1 bit4 Menu Left 0x44"
    static let right = "HID uid 3 · D-Pad byte1 bit5 Menu Right 0x45"
    static let select = "HID uid 3 · D-Pad byte1 bit1 Menu Pick 0x41"
    // Telephony (uid 5, Apple HIDTelephony)
    static let telAnswer = "HID uid 5 · Telephony Hook Switch 0x20 (answer)"
    static let telEnd = "HID uid 5 · Telephony Drop 0x26 (end)"
    static let telFlash = "HID uid 5 · Telephony Flash 0x21 (swap / call-waiting)"
    static let telMute = "HID uid 5 · Telephony Mute 0x2F"
    static let telStar = "HID uid 5 · Telephony PhoneKey * 0xBA"
    static let telPound = "HID uid 5 · Telephony PhoneKey # 0xBB"
    static let telDelete = "HID uid 5 · Keyboard DELETE 0x2A"
    static func telDigit(_ d: Int) -> String {
        "HID uid 5 · Telephony PhoneKey \(d) 0x\(String(0xB0 + d, radix: 16, uppercase: true))"
    }
}

// MARK: - Buttons

/// A momentary control button (tap → single action) with its CarPlay wire mapping shown beneath.
private struct CtrlButton: View {
    let label: String
    var systemImage: String? = nil
    var width: CGFloat = 96
    var wire: String? = nil
    let action: () -> Void

    var body: some View {
        VStack(spacing: 3) {
            Button(action: action) {
                HStack(spacing: 5) {
                    if let systemImage { Image(systemName: systemImage) }
                    Text(label)
                }
                .frame(width: width, height: 30)
            }
            .buttonStyle(.bordered)
            .controlSize(.large)
            if let wire {
                Text(wire).font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary).frame(width: width + 24).multilineTextAlignment(.center)
            }
        }
    }
}

/// The persistent footer: the literal CarPlay command last sent to the iPhone.
private struct WireReadout: View {
    @ObservedObject var bridge: ControlsBridge
    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "arrow.up.right.circle.fill").foregroundStyle(.tint)
            Text("Sent to iPhone:").font(.caption).foregroundStyle(.secondary)
            Text(bridge.lastSent).font(.system(.caption, design: .monospaced)).textSelection(.enabled)
            Spacer()
        }
        .padding(8)
        .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 8))
    }
}

/// A press-and-hold button — fires `onDown` on press and `onUp` on release, however long the hold.
/// Used for Siri (requestSiri buttondown/buttonup) via a drag gesture with min distance 0.
private struct HoldButton: View {
    let label: String
    var systemImage: String? = nil
    let onDown: () -> Void
    let onUp: () -> Void
    @State private var held = false

    var body: some View {
        HStack(spacing: 6) {
            if let systemImage { Image(systemName: systemImage) }
            Text(label)
        }
        .frame(width: 150, height: 34)
        .background(held ? Color.accentColor.opacity(0.35) : Color(nsColor: .controlColor),
                    in: RoundedRectangle(cornerRadius: 7))
        .overlay(RoundedRectangle(cornerRadius: 7).stroke(.separator))
        .contentShape(Rectangle())
        .gesture(
            DragGesture(minimumDistance: 0)
                .onChanged { _ in if !held { held = true; onDown() } }
                .onEnded { _ in held = false; onUp() }
        )
        // Safety net: if the gesture is interrupted or the view is torn down mid-hold (window
        // deactivates, SwiftUI cancels the drag, or the row rebuilds), onEnded may never fire —
        // which would leave the phone with Siri held down forever. Force the buttonup on disappear.
        .onDisappear { if held { held = false; onUp() } }
    }
}

// MARK: - Sections

private struct Section<Content: View>: View {
    let title: String
    let note: String?
    @ViewBuilder var content: Content
    init(_ title: String, note: String? = nil, @ViewBuilder content: () -> Content) {
        self.title = title; self.note = note; self.content = content()
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title.uppercased()).font(.caption).fontWeight(.semibold).foregroundStyle(.secondary)
            content
            if let note {
                Text(note).font(.caption2).foregroundStyle(.tertiary).fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: 12))
    }
}

/// The D-pad: Select in the center, arrows in cardinal positions (Up top; Left · SEL · Right; Down).
private struct DirectionalPad: View {
    let bridge: ControlsBridge
    private let cell: CGFloat = 52

    private func key(_ symbol: String, _ nav: UInt8, _ wire: String) -> some View {
        Button { bridge.nav(nav, wire) } label: {
            Image(systemName: symbol).font(.title3).frame(width: cell, height: cell)
        }
        .buttonStyle(.bordered)
    }
    private func center() -> some View {
        Button { bridge.nav(OCBM.navSelect, Wire.select) } label: {
            Text("SEL").font(.headline).frame(width: cell, height: cell)
        }
        .buttonStyle(.borderedProminent)
    }
    private var spacer: some View { Color.clear.frame(width: cell, height: cell) }

    var body: some View {
        VStack(spacing: 6) {
            HStack(spacing: 6) { spacer; key("chevron.up", OCBM.navUp, Wire.up); spacer }
            HStack(spacing: 6) {
                key("chevron.left", OCBM.navLeft, Wire.left)
                center()
                key("chevron.right", OCBM.navRight, Wire.right)
            }
            HStack(spacing: 6) { spacer; key("chevron.down", OCBM.navDown, Wire.down); spacer }
        }
    }
}

/// Tabs mirror the CarPlay SIMULATOR's own HID control views (`HIDContainerTabView` → `HIDMediaView`,
/// `HIDDPadView`, `HIDKnobView`/`HIDComplexKnobControlView`, `HIDSteeringWheelView`, `HIDTelephonyView`,
/// `HIDTouchpadView`). We reimplement the ones our stack drives; the rest show their real control
/// layout with the HID-device requirement noted.
struct ControlsRootView: View {
    @StateObject private var bridge = ControlsBridge.shared

    var body: some View {
        TabView {
            MediaControlView(bridge: bridge)
                .tabItem { Label("Media", systemImage: "play.circle") }
            // D-Pad tab removed 2026-08-02: the Knob's directional buttons (uid 4) cover directional
            // navigation; the discrete HIDDPad was redundant and not driven. Steering-Wheel tab removed
            // (empty placeholder — wheels carry nav/media controls already present in other tabs). The
            // "UI" tab removed too: cluster Content now lives inline in the Nav/Alt Video titlebar, and
            // Limited UI is a niche runtime toggle. (`DPadControlView`/`SteeringWheelControlView`/
            // `UIControlView` structs are retained below, unused, for reference.)
            KnobControlView(bridge: bridge)
                .tabItem { Label("Knob", systemImage: "dial.medium") }
            TelephonyControlView(bridge: bridge)
                .tabItem { Label("Phone", systemImage: "phone") }
            SiriControlView(bridge: bridge)
                .tabItem { Label("Siri", systemImage: "waveform") }
        }
        .padding(12)
        .frame(minWidth: 420, minHeight: 460)
    }
}

/// Runtime CarPlay-UI controls that don't map to a physical HID: the cluster (type-111) CONTENT
/// selector — mirroring the CarPlay Simulator's own realtime picker (None / Instruction Card / Map /
/// Navigation App), each a `requestUI` with a `maps:/car/instrumentcluster…` URL — and the limitedUI
/// (Drive) restriction toggle. Both are `/command`s on the live event channel; no reconnect.
private struct UIControlView: View {
    @ObservedObject var bridge: ControlsBridge
    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            // Cluster content — the info cards ("Instruction Card") are a content type iOS renders
            // into the cluster VIDEO; select it here at runtime.
            VStack(alignment: .leading, spacing: 8) {
                Text("Cluster Content").font(.headline)
                Picker("", selection: Binding(get: { bridge.clusterContent },
                                              set: { bridge.setClusterContent($0) })) {
                    ForEach(ClusterContent.allCases) { c in
                        Label(c.rawValue, systemImage: c.systemImage).tag(c)
                    }
                }
                .pickerStyle(.radioGroup)
                .labelsHidden()
                Text(bridge.clusterContent == .none
                     ? "Cluster idle — no frames encoded, Nav window closed."
                     : "iOS encodes the “\(bridge.clusterContent.rawValue)” surface into the cluster stream; the Nav window opens when frames arrive.")
                    .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                if !VehicleConfigModel.shared.altVideoEnabled {
                    Label("Enable “Alt / Navigation Video” in Settings → Configuration and reconnect first, so iOS advertises the cluster display.",
                          systemImage: "exclamationmark.triangle")
                        .font(.caption).foregroundStyle(.orange).fixedSize(horizontal: false, vertical: true)
                }
            }

            Divider()

            // limitedUI — restrict the CarPlay UI as if shifted into Drive.
            VStack(alignment: .leading, spacing: 6) {
                Toggle(isOn: Binding(get: { bridge.limitedUIOn }, set: { bridge.setLimitedUI($0) })) {
                    Text("Limited UI (Drive)")
                }
                .toggleStyle(.switch)
                Text("Restricts CarPlay UI as if the vehicle is in Drive — hides the on-screen keyboard, phone keypad and long scrollable lists. Off = parked (full UI).")
                    .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                Text("Wire: /command setLimitedUI {limitedUI: bool}")
                    .font(.system(size: 9, design: .monospaced)).foregroundStyle(.tertiary)
            }

            Spacer()
        }
        .padding()
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }
}

/// HIDMediaView — media transport (uid 2) + Home/Back (D-Pad AC Home/Back usages).
/// Reusable control surface — hosted both in the Controls TabView and in a standalone Control-Box popup.
struct MediaControlView: View {
    let bridge: ControlsBridge
    var body: some View {
        VStack(spacing: 18) {
            Section("Transport", note: "HID media buttons (uid 2). Single-press taps.") {
                VStack(spacing: 8) {
                    HStack(spacing: 8) {
                        CtrlButton(label: "Play", systemImage: "play.fill", width: 84, wire: Wire.play) { bridge.media(OCBM.mbtnPlay, Wire.play) }
                        CtrlButton(label: "Pause", systemImage: "pause.fill", width: 84, wire: Wire.pause) { bridge.media(OCBM.mbtnPause, Wire.pause) }
                        CtrlButton(label: "Play/Pause", systemImage: "playpause.fill", width: 118, wire: Wire.playPause) { bridge.media(OCBM.mbtnPlayPause, Wire.playPause) }
                    }
                    HStack(spacing: 8) {
                        CtrlButton(label: "Previous", systemImage: "backward.fill", width: 118, wire: Wire.prev) { bridge.media(OCBM.mbtnPrev, Wire.prev) }
                        CtrlButton(label: "Next", systemImage: "forward.fill", width: 118, wire: Wire.next) { bridge.media(OCBM.mbtnNext, Wire.next) }
                    }
                }
            }
            // Home & Back removed 2026-08-02 — they live on the Knob tab (uid 4 Home/Back flags).
            Spacer()
            WireReadout(bridge: bridge)
        }
        .padding(16)
    }
}

/// HIDDPadView — the discrete directional pad + Select (Apple's exact HIDDPad device, uid 3).
private struct DPadControlView: View {
    let bridge: ControlsBridge
    var body: some View {
        VStack(spacing: 16) {
            Text("D-Pad").font(.headline)
            Text("Apple HIDDPad device (uid 3): Menu Up/Down/Left/Right + Pick (Select), AC Home/Back. Discrete directional navigation — distinct from the rotary Knob.")
                .font(.caption).foregroundStyle(.secondary).multilineTextAlignment(.center)
                .frame(maxWidth: 360)
            DirectionalPad(bridge: bridge)
                .padding(.vertical, 8)
            HStack(spacing: 8) {
                CtrlButton(label: "Home", systemImage: "house.fill", width: 90, wire: Wire.home) { bridge.nav(OCBM.navHome, Wire.home) }
                CtrlButton(label: "Back", systemImage: "chevron.backward", width: 90, wire: Wire.back) { bridge.nav(OCBM.navBack, Wire.back) }
            }
            Spacer()
            WireReadout(bridge: bridge)
        }
        .padding(16)
    }
}

/// HIDKnobView — the ROTARY knob (uid 4), Apple's exact HIDKnobCreateDescriptor. The CarPlay
/// Simulator drives ALL navigation through this device: rotation moves the selector, the four nudge
/// arrows move 4-way, and the center button selects. Requires "Knob support" in Settings (advertises
/// the 4th HID device, behind the reconnect-incident guard).
/// Reusable control surface — hosted both in the Controls TabView and in a standalone Control-Box popup.
struct KnobControlView: View {
    let bridge: ControlsBridge
    @State private var dragAngle: Double? = nil   // last touch angle (deg), nil when not dragging
    @State private var accum: Double = 0          // accumulated rotation until a detent fires
    @State private var visualRotation: Double = 0 // the knob's on-screen spin (follows the drag 1:1)

    private let knob: CGFloat = 150
    private let detentDeg: Double = 22.5          // Simulator feel: π/8 → 16 detents / turn

    var body: some View {
        VStack(spacing: 16) {
            Text("Rotary Knob").font(.headline)
            Text("Drag the knob to rotate (moves the selector), tap an arrow to nudge 4-way, tap the center to Select. Enable “Knob support” in Settings → Configuration and reconnect first.")
                .font(.caption).foregroundStyle(.secondary).multilineTextAlignment(.center)
                .frame(maxWidth: 360)

            // The knurled rotary knob framed by four directional arrows — the Simulator's layout.
            ZStack {
                // HID Y is screen-convention: up = negative, down = positive (device-verified 2026-08-01).
                arrow("arrowtriangle.up.fill").offset(y: -(knob / 2 + 30))
                    .onTapGesture { bridge.knob(nudgeY: -127, "knob nudge up") }
                arrow("arrowtriangle.down.fill").offset(y: knob / 2 + 30)
                    .onTapGesture { bridge.knob(nudgeY: 127, "knob nudge down") }
                arrow("arrowtriangle.left.fill").offset(x: -(knob / 2 + 30))
                    .onTapGesture { bridge.knob(nudgeX: -127, "knob nudge left") }
                arrow("arrowtriangle.right.fill").offset(x: knob / 2 + 30)
                    .onTapGesture { bridge.knob(nudgeX: 127, "knob nudge right") }

                // The knob VISUAL spins with the drag; the gesture rides a separate NON-rotated overlay
                // so the rotation never feeds back into the angle math.
                ZStack {
                    RidgedKnob(size: knob).rotationEffect(.degrees(visualRotation))
                    Circle().fill(Color.clear).contentShape(Circle())
                        .gesture(rotationGesture)
                        .onTapGesture { bridge.knob(flags: 0x01, "knob select") } // center = Select
                }
                .frame(width: knob, height: knob)
            }
            .frame(width: knob + 120, height: knob + 120)

            HStack(spacing: 20) {
                CtrlButton(label: "Home", systemImage: "house") { bridge.knob(flags: 0x02, "knob home") }
                CtrlButton(label: "Back", systemImage: "chevron.backward") { bridge.knob(flags: 0x04, "knob back") }
            }
            Spacer()
        }
        .padding(16)
    }

    private func arrow(_ system: String) -> some View {
        Image(systemName: system).font(.system(size: 34)).foregroundStyle(.secondary)
            .frame(width: 50, height: 50).contentShape(Rectangle())
    }

    /// Drag around the knob → emit one rotation detent (±1) per `detentDeg` swept, sign = direction.
    private var rotationGesture: some Gesture {
        DragGesture(minimumDistance: 1)
            .onChanged { v in
                let dx = v.location.x - knob / 2, dy = v.location.y - knob / 2
                let ang = atan2(dy, dx) * 180 / .pi
                if let last = dragAngle {
                    var d = ang - last
                    if d > 180 { d -= 360 } else if d < -180 { d += 360 }
                    visualRotation += d          // spin the knob face 1:1 with the finger
                    accum += d
                    while accum >= detentDeg { accum -= detentDeg; bridge.knob(rotation: 1, "knob rotate CW") }
                    while accum <= -detentDeg { accum += detentDeg; bridge.knob(rotation: -1, "knob rotate CCW") }
                }
                dragAngle = ang
            }
            .onEnded { _ in dragAngle = nil; accum = 0 }
    }
}

/// A knurled rotary-encoder knob drawn to match the CarPlay Simulator: a ridged outer rim, an inner
/// bevel ring, and a raised center cap.
private struct RidgedKnob: View {
    let size: CGFloat
    var body: some View {
        ZStack {
            Circle().fill(Color(white: 0.30))                                   // knob body
            Canvas { ctx, sz in                                                 // knurled rim ridges
                let c = CGPoint(x: sz.width / 2, y: sz.height / 2)
                let outer = sz.width / 2, inner = outer - 11
                let ridges = 40
                for i in 0..<ridges {
                    let a = Double(i) / Double(ridges) * 2 * .pi
                    var p = Path()
                    p.move(to: CGPoint(x: c.x + cos(a) * inner, y: c.y + sin(a) * inner))
                    p.addLine(to: CGPoint(x: c.x + cos(a) * outer, y: c.y + sin(a) * outer))
                    ctx.stroke(p, with: .color(Color(white: 0.20)), lineWidth: 2.5)
                }
            }
            Circle().strokeBorder(Color(white: 0.45), lineWidth: 2)
                .padding(14)                                                    // inner bevel ring
            Circle().fill(Color(white: 0.34)).padding(30)                       // raised center cap
        }
        .frame(width: size, height: size)
    }
}

/// HIDSteeringWheelView — steering-wheel buttons. Not advertised; shown with the requirement.
private struct SteeringWheelControlView: View {
    var body: some View {
        VStack(spacing: 14) {
            Image(systemName: "steeringwheel").font(.system(size: 54)).foregroundStyle(.tertiary)
            Text("Steering-Wheel Controls").font(.headline)
            Text("Select / Back + Menu directions + a wheel axis (Apple HIDSteeringWheel device). Requires an additional HID device; not enabled. Media, Home/Back, D-Pad and Siri are available on the other tabs.")
                .font(.caption).foregroundStyle(.secondary).multilineTextAlignment(.center)
                .frame(maxWidth: 360)
            Spacer()
        }
        .padding(16)
    }
}

/// HIDTelephonyView — call control. Needs the HID Telephony device; call state shows in Metadata.
/// Reusable control surface — hosted both in the Controls TabView and in a standalone Control-Box popup.
struct TelephonyControlView: View {
    let bridge: ControlsBridge
    // DTMF keypad rows: (label, telephony index, wire). Digit d → OCBM.telDigit0 + d; * / # are separate.
    private let keypad: [[(String, UInt8, String)]] = [
        [("1", OCBM.telDigit0 + 1, Wire.telDigit(1)), ("2", OCBM.telDigit0 + 2, Wire.telDigit(2)), ("3", OCBM.telDigit0 + 3, Wire.telDigit(3))],
        [("4", OCBM.telDigit0 + 4, Wire.telDigit(4)), ("5", OCBM.telDigit0 + 5, Wire.telDigit(5)), ("6", OCBM.telDigit0 + 6, Wire.telDigit(6))],
        [("7", OCBM.telDigit0 + 7, Wire.telDigit(7)), ("8", OCBM.telDigit0 + 8, Wire.telDigit(8)), ("9", OCBM.telDigit0 + 9, Wire.telDigit(9))],
        [("✱", OCBM.telStar, Wire.telStar), ("0", OCBM.telDigit0, Wire.telDigit(0)), ("#", OCBM.telPound, Wire.telPound)],
    ]

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Section("Call Control", note: "Apple HID Telephony (uid 5). Arm “Telephony buttons” in Settings ▸ Configuration ▸ Input, then reconnect.") {
                HStack(spacing: 10) {
                    CtrlButton(label: "Answer", systemImage: "phone.fill", width: 92, wire: Wire.telAnswer) { bridge.telephony(OCBM.telAnswer, Wire.telAnswer) }
                    CtrlButton(label: "End", systemImage: "phone.down.fill", width: 92, wire: Wire.telEnd) { bridge.telephony(OCBM.telEnd, Wire.telEnd) }
                    CtrlButton(label: "Flash", systemImage: "arrow.2.squarepath", width: 92, wire: Wire.telFlash) { bridge.telephony(OCBM.telFlash, Wire.telFlash) }
                        .disabled(!bridge.isAvailable(.callExtras))
                        .help(bridge.unavailableReason(.callExtras) ?? "")
                    CtrlButton(label: "Mute", systemImage: "mic.slash.fill", width: 92, wire: Wire.telMute) { bridge.telephony(OCBM.telMute, Wire.telMute) }
                        .disabled(!bridge.isAvailable(.callExtras))
                        .help(bridge.unavailableReason(.callExtras) ?? "")
                }
            }
            Section("DTMF Keypad", note: "Telephony PhoneKey usages (0xB0–0xBB) — in-call tone dialing.") {
                VStack(spacing: 8) {
                    ForEach(keypad.indices, id: \.self) { r in
                        HStack(spacing: 8) {
                            ForEach(keypad[r].indices, id: \.self) { c in
                                let k = keypad[r][c]
                                CtrlButton(label: k.0, width: 64, wire: k.2) { bridge.telephony(k.1, k.2) }
                            }
                        }
                    }
                    CtrlButton(label: "Delete", systemImage: "delete.left", width: 100, wire: Wire.telDelete) { bridge.telephony(OCBM.telDelete, Wire.telDelete) }
                        .disabled(!bridge.isAvailable(.callExtras))
                        .help(bridge.unavailableReason(.callExtras) ?? "")
                }
            }
            Spacer()
            WireReadout(bridge: bridge)
        }
        .padding(16)
    }
}

/// Siri — a simple press that invokes Siri (requestSiri buttondown then buttonup). Press-and-hold is only
/// meaningful on dual-function call-pickup buttons (press = answer, hold = Siri), not a dedicated button.
private struct SiriControlView: View {
    let bridge: ControlsBridge
    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "waveform").font(.system(size: 48)).foregroundStyle(.tint)
            // Siri has no Android Auto equivalent — the assistant is Google's, and it is reached by a
            // keycode, not by Apple's requestSiri command. Offer the RIGHT one for whoever owns the
            // box rather than showing a Siri button that cannot work.
            Text(bridge.siriAvailable ? "Siri" : "Google Assistant").font(.headline)
            Text(bridge.siriAvailable
                 ? "Tap to invoke Siri — sends requestSiri buttondown then buttonup (SDK AirPlaySiriAction). Needs the mic uplink to hear you."
                 : "Android Auto owns the box. Tap to invoke the Google Assistant — sends KEYCODE_SEARCH on the AA input channel. Needs the mic uplink (channel 9) to hear you.")
                .font(.caption).foregroundStyle(.secondary).multilineTextAlignment(.center)
                .frame(maxWidth: 360)
            if bridge.siriAvailable {
                CtrlButton(label: "Invoke Siri", systemImage: "mic.fill", width: 160,
                           wire: "/command requestSiri buttondown → buttonup") { bridge.siriPress() }
            } else {
                CtrlButton(label: "Invoke Assistant", systemImage: "mic.fill", width: 190,
                           wire: "InputReport{key KEYCODE_SEARCH}") { bridge.assistant() }
            }
            Spacer()
            WireReadout(bridge: bridge)
        }
        .padding(16)
    }
}

// MARK: - AppKit host

final class ControlsWindowController: NSWindowController {
    static let shared = ControlsWindowController()

    private convenience init() {
        let win = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 420, height: 620),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        win.title = "Controls"
        win.isReleasedWhenClosed = false
        win.contentView = NSHostingView(rootView: ControlsRootView())
        self.init(window: win)
    }

    func show() {
        window?.center()
        window?.makeKeyAndOrderFront(nil)
    }
}
