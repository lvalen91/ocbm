// AdapterTab.swift — the Adapter tab: the box as a DEVICE, not the car. Two halves:
//
//  1. Projection enables (`Feature.Section.adapter`: wirelessRadios, hotHandover, pairing,
//     wifiAccessPoint, androidAutoProjection, appDrivenSetup). These are this project's own
//     top-level YAML keys (`wireless:`, `hot_handover:`, `pairing:`, `wifi_ap:`, `android_auto:`,
//     `accessoryConfig.appDrivenSetup`), read by tools/session_supervisor.sh / vehicle_config.rs and
//     never by a phone — so they ride the pushed document and need Save like any other field, but
//     they describe box BEHAVIOUR, not a vehicle fact, which is why they left the Vehicle tab
//     (DESIGN.md §1, 2026-09-04). Every control is rendered as neutral control → per-projection badge
//     row → per-projection effect rows, all read from `FeatureMatrix`; no hand-written protocol prose.
//  2. Live box state and box-level actions: `BoxHealth` / `BtPhase` (the CT_BOX_HEALTH / CT_BT_PHASE
//     decodes), `CCPABridge` (the observable the OCBM client feeds and AppDelegate wires), the
//     CCPAInfo snapshot, and Restart adapter / NCM mode. None of that rides the YAML.
//
// DEFECT 1 (DESIGN.md §7). Until 2026-09-04 the `wireless:` toggle was labelled "Wireless CarPlay".
// It is the box RADIO gate: tools/session_supervisor.sh `wireless_up()` returns early on
// `! wireless_enabled` (`[sup] wireless disabled by config (wireless: false) — wired-only`), and
// `arm_aa_wireless` is reachable ONLY from inside `wireless_up()` (the per-tick liveness self-heal
// is itself gated on `wireless_running`). One `btd` daemon advertises BOTH the iAP2 and
// the Android Auto SDP records. So the toggle silently killed the wireless Android Auto path that
// shipped 2026-09-04. The fix is entirely on THIS side: the control is labelled as the radio gate,
// both projections are badged as depending on it, and the two effect sentences come from
// `FeatureMatrix.support(.wirelessRadios, on:)`. The box side is deliberately NOT changed — a
// separate `wireless_android_auto:` key would alter the pushed YAML, which the drift guard forbids.
//
// DEFECT 8. `wifi_ap:` had no UI at all. The model field (`wifiAccessPoint`, Phase 0) is emitted as
// `wifi_ap: false` ONLY when disabled — the box's `wifi_ap_enabled()` greps for an explicit `false`
// and treats absent/anything-else as enabled — so the default document stays byte-identical to every
// pre-2026-09-04 push. The control here just binds the field; the polarity lives in the emitter.
//
// DEFECT 7 (this file's half). The Restart / NCM confirmations used to say "any live CarPlay session
// will drop". An Android Auto session drops too; the wording is now protocol-neutral.
//
// Split out of App/SettingsWindow.swift on 2026-09-04 (DESIGN.md §6 Phase 0). `BoxHealth`, `BtPhase`,
// `CCPABridge` and `HealthRow` are still byte-for-byte the SettingsWindow.swift originals; only the
// view (`AdapterTab`, formerly `CCPATab`) was rewritten in Phase 1. `VehicleConfigModel` did NOT move:
// tools/regen_app_yaml_fixture.py text-extracts its YAML emitter from SettingsWindow.swift by string
// anchor, so relocating the model would silently disarm the app→box drift guard.

import SwiftUI

// MARK: - CCPA management tab — live box state (CT_BOX_HEALTH / CT_BT_PHASE / CT_PHONE_IDENT)

/// The ONE place a box daemon's wire name becomes a display name.
///
/// The box renamed its daemons on 2026-09-08 (`airplayd` -> `carplayd`, `carplay-wireless` -> `btd`,
/// and `rx-connect` merged into `carplayd` as an in-process discovery thread) but DELIBERATELY PINNED
/// every wire contract to the OLD spellings, so an old app and a new box still agree:
///   * `MGMT_INFO` JSON still emits `airplayd` / `carplay_wireless` (`CCPAInfo.Daemons`),
///   * `CT_BOX_HEALTH` bit VALUES are unchanged (`OCBM.bhAirplayd` 0x08, `OCBM.bhCarplayWireless` 0x10),
///   * the box's `log_src_name()` still returns `airplayd` / `airplayd-wl`, so log lines stay tagged
///     `[box/airplayd]` and `BoxLogWindow` keeps grepping against the box's own `/tmp` files.
/// Decoupling therefore happens HERE, at the presentation layer, and nowhere else. Before this
/// existed the same wireless bit rendered three different ways in two files — `btd` in
/// `BoxHealth.checklist`, `wireless` in `daemonSummary`, `wireless` in `OCBM.boxHealthNames` — which
/// is the drift this table exists to stop. Renaming a wire key here does NOT rename it on the wire;
/// that is a coordinated box+app release, not an app-side edit.
enum DaemonDisplay {
    /// Display label for a `MGMT_INFO` daemon key or `CT_BOX_HEALTH` bit. Unknown keys pass through
    /// unchanged, so a daemon the box gains before this app knows about it still shows its wire name
    /// rather than vanishing from the list.
    static func label(wireKey: String) -> String {
        switch wireKey {
        case "airplayd":         return "carplayd"
        case "carplay_wireless": return "btd"
        default:                 return wireKey
        }
    }
}

/// `CT_BOX_HEALTH` bitmask (`OCBM.bh*`), named per docs/carplay/01_OCBM_PROTOCOL.md.
struct BoxHealth: OptionSet, Codable, Equatable {
    let rawValue: UInt8
    static let hciPresent = BoxHealth(rawValue: OCBM.bhHciPresent)
    static let ssp = BoxHealth(rawValue: OCBM.bhSsp)
    static let iap2d = BoxHealth(rawValue: OCBM.bhIap2d)
    static let airplayd = BoxHealth(rawValue: OCBM.bhAirplayd)
    static let carplayWireless = BoxHealth(rawValue: OCBM.bhCarplayWireless)
    static let wlanAp = BoxHealth(rawValue: OCBM.bhWlanAp)
    static let rootfsOk = BoxHealth(rawValue: OCBM.bhRootfsOk)

    /// `(label, ok)` pairs in wire-bit order, for a ✓/✗ list.
    var checklist: [(label: String, ok: Bool)] {
        [
            ("HCI present", contains(.hciPresent)),
            ("SSP", contains(.ssp)),
            ("iap2d", contains(.iap2d)),
            (DaemonDisplay.label(wireKey: "airplayd"), contains(.airplayd)),
            (DaemonDisplay.label(wireKey: "carplay_wireless"), contains(.carplayWireless)),
            ("Wi-Fi AP", contains(.wlanAp)),
            ("rootfs OK", contains(.rootfsOk)),
        ]
    }
}

/// `CT_BT_PHASE` value (`OCBM.btp*`). An unrecognised raw byte decodes to `nil` — advisory per
/// docs/carplay/01_OCBM_PROTOCOL.md, never coerced to a known phase.
enum BtPhase: UInt8, Codable {
    case idle = 0x00
    case linkUp = 0x01
    case authenticating = 0x02
    case authenticated = 0x03
    case identifying = 0x04
    case identified = 0x05
    case wifiHandoff = 0x06

    var displayName: String {
        switch self {
        case .idle: return "Idle"
        case .linkUp: return "Link up"
        case .authenticating: return "Authenticating"
        case .authenticated: return "Authenticated"
        case .identifying: return "Identifying"
        case .identified: return "Identified"
        case .wifiHandoff: return "Wi-Fi handoff"
        }
    }
}

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

    // Live box state — mirrored 1:1 from OCBMClient's CT_BOX_HEALTH/CT_BT_PHASE/CT_PHONE_IDENT
    // callbacks. Deliberately NOT persisted across a session: `sessionEnded()` clears all three so a
    // reconnect never shows a phantom carry-over from the previous box/phone — a fresh CT_SUBSCRIBE
    // always re-emits its own current values (docs/carplay/01_OCBM_PROTOCOL.md, "re-emitted after
    // each CT_SUBSCRIBE").
    @Published var boxHealth: BoxHealth?
    @Published var boxHealthUpdated: Date?
    @Published var btPhase: BtPhase?
    @Published var btPhaseUpdated: Date?
    @Published var phoneIdent: PhoneIdent?
    @Published var phoneIdentUpdated: Date?

    private var busyGen = 0

    /// Session teardown (AppDelegate.endSession): the OCBM link is gone, so any latched busy will
    /// never be ACKed and "Connected" is a lie. Keep the last snapshot visible but mark it stale.
    func sessionEnded() {
        clearBusy()
        statusText = "Disconnected"
        if info != nil { stale = true }
        boxHealth = nil
        boxHealthUpdated = nil
        btPhase = nil
        btPhaseUpdated = nil
        phoneIdent = nil
        phoneIdentUpdated = nil
    }

    func receiveBoxHealth(_ bits: UInt8) {
        boxHealth = BoxHealth(rawValue: bits)
        boxHealthUpdated = Date()
    }
    func receiveBtPhase(_ phase: UInt8) {
        btPhase = BtPhase(rawValue: phase)
        btPhaseUpdated = Date()
    }
    func receivePhoneIdent(_ ident: PhoneIdent?) {
        phoneIdent = ident
        phoneIdentUpdated = Date()
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
        if verb != OCBM.mgmtReboot && verb != OCBM.mgmtEnterNCM { client?.requestBoxInfo() }
        if verb == OCBM.mgmtEnterNCM && status == 0 { statusText = "Rebooting into NCM mode…" }
    }
    func restartWireless() { setBusy(); client?.boxRestartWireless() }
    func forgetAll() { setBusy(); client?.boxForgetAll() }
    func forgetDevice(_ mac: String) { setBusy(); client?.boxForgetDevice(mac) }
    func reboot() { setBusy("Rebooting adapter…"); client?.boxReboot() }
    /// Sticky NCM maintenance mode (ssh/telnet over USB-NCM, no OCBM). Return via ssh:
    /// `rm /script/ncm_only; reboot`. Also reachable without the UI via `carlink://box/enter-ncm`.
    func enterNCM() { setBusy("Entering NCM mode…"); client?.boxEnterNCM() }

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
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(6))
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


// MARK: - The Adapter tab

/// The Adapter tab, one of the two tabs `SettingsRootView` (App/SettingsWindow.swift) hosts; the
/// `CCPATab` name it replaced is gone (2026-09-04).
struct AdapterTab: View {
    @ObservedObject var model = VehicleConfigModel.shared
    @ObservedObject var store = CCPABridge.shared
    @State private var confirmReboot = false
    @State private var confirmForgetAll = false
    @State private var confirmEnterNCM = false
    @State private var forgetMac: String?
    /// Phase 3: the ~25-30-row live box telemetry stack (Identity/Health/Live State/Known Devices)
    /// collapses behind one summary row (DESIGN.md §1, §11.6). Plain `@State`, not persisted, matching
    /// `CollapsibleFeatureSection`'s own precedent (§11.1 decision 2 — no `@AppStorage`).
    @State private var boxStateExpanded = false

    var body: some View {
        Form {
            // ---- 1. Projection enables. Phase 3: all six `Feature.adapter` cases share exactly one
            // `Feature.Section` (`.adapter`), so they collapse behind ONE `CollapsibleFeatureSection`
            // rather than six separate disclosures — `.wirelessRadios` collapses with everything else
            // (DESIGN.md §11.1, a knowing trade: it mislabels nothing, it is just one chevron away).
            // Still enumerated from `FeatureMatrix.features(in:)`, never a hand-curated list, so a new
            // adapter feature without a control is still a compile error in `control(for:)`.
            CollapsibleFeatureSection(.adapter) {
                ForEach(FeatureMatrix.features(in: .adapter), id: \.self) { feature in
                    Section {
                        FeatureHeading(feature: feature)
                        control(for: feature)
                        // `always` for the radio gate: it is UNIFORM (both projections .supported), so
                        // the default would hide exactly the two rows that fix defect 1 — "the SAME
                        // switch: wireless Android Auto is armed inside the wireless bring-up". This
                        // survives the collapse — it is preserved WITHIN the section, one chevron away,
                        // never deleted (DESIGN.md §11.1).
                        //
                        // TEN-WORD RULE JUDGEMENT CALL (surfaced, not made here): the two rows this
                        // renders inline via `always: true` are `FeatureMatrix.support(.wirelessRadios,
                        // on:)` `.effect` strings (frozen file) — carPlay 13 words ("Advertises for
                        // wireless CarPlay (Bluetooth pairing + Wi-Fi hand-off) while the app is
                        // connected."), androidAuto 19 words ("The SAME switch: wireless Android Auto
                        // is armed inside the wireless bring-up, so off here also disables wireless
                        // AA."). Both exceed ten, and DESIGN.md §11 says even the `always: true`
                        // exception "must be condensed to ten words or they do not go inline at all."
                        // Recommendation: shorten both `FeatureSupport.effect` strings in
                        // FeatureMatrix.swift to ≤10 words while keeping the "one switch governs both
                        // projections" fact (the defect-1 fix) — do not just delete this call, that
                        // re-opens defect 1. Not done here: FeatureMatrix.swift is frozen.
                        FeatureExplanationRows(feature: feature, always: feature == .wirelessRadios)
                    } footer: {
                        if let note = footnote(for: feature) {
                            Text(note).font(.caption)
                        }
                    }
                }
            }

            // ---- 2. Live box state. `.disabled(store.busy)` is scoped to THIS half so a pending
            // CH_MGMT action (6 s self-heal at most) does not freeze the enables above. Phase 3: the
            // telemetry stack (`boxState`) moves behind a `Section(isExpanded:)` whose HEADER is the
            // one always-visible summary row — the `stale` marker is promoted into it as an amber dot
            // so a reader who never expands never mistakes a pre-teardown snapshot for live data
            // (DESIGN.md §1, §11.6). `boxState` itself is untouched: expanding reveals today's stack
            // verbatim. Refresh moves into this same header (nothing in DESIGN.md pins its location,
            // unlike Save) but keeps `.disabled(false)` so it stays usable even while `store.busy` is
            // latched — it is the self-heal action, and disabling it here would be a behaviour change.
            Group {
                Section(isExpanded: $boxStateExpanded) {
                    boxState
                } header: {
                    boxStateSummaryHeader
                }
                boxControls
            }
            .disabled(store.busy)
        }
        .formStyle(.grouped)
        .safeAreaInset(edge: .bottom) { bottomBar }
        .onAppear { store.refresh() }
        // Defect 7: neutral wording — a live Android Auto session drops on reboot exactly as a
        // CarPlay one does. Protocol names belong to the `FeatureSupport.effect` rows only.
        .confirmationDialog("Restart the adapter now?", isPresented: $confirmReboot) {
            Button("Restart Adapter", role: .destructive) { store.reboot() }
            Button("Cancel", role: .cancel) {}
        } message: { Text("The adapter will reboot and any live projection session will drop.") }
        .confirmationDialog("Forget all paired devices?", isPresented: $confirmForgetAll) {
            Button("Forget All", role: .destructive) { store.forgetAll() }
            Button("Cancel", role: .cancel) {}
        } message: { Text("Every phone will need to pair again.") }
        .confirmationDialog("Reboot into NCM maintenance mode?", isPresented: $confirmEnterNCM) {
            Button("Enter NCM Mode", role: .destructive) { store.enterNCM() }
            Button("Cancel", role: .cancel) {}
        } message: { Text("The adapter reboots as a USB network device with ssh/telnet enabled and no projection of any kind; any live session drops. It stays in NCM mode until returned over ssh (rm /script/ncm_only; reboot).") }
        .confirmationDialog("Forget this device?", isPresented: Binding(
            get: { forgetMac != nil }, set: { if !$0 { forgetMac = nil } }
        )) {
            Button("Forget", role: .destructive) { if let m = forgetMac { store.forgetDevice(m) }; forgetMac = nil }
            Button("Cancel", role: .cancel) { forgetMac = nil }
        } message: { Text(forgetMac.map { "\($0) will need to pair again." } ?? "") }
    }

    // MARK: Projection enables

    /// The neutral control for one adapter feature. Exhaustive over `Feature` so a new adapter case
    /// cannot ship without a control; non-adapter cases never reach here (`features(in: .adapter)`).
    @ViewBuilder
    private func control(for feature: Feature) -> some View {
        switch feature {
        case .wirelessRadios:
            // The radio gate (defect 1). No protocol in the label: the badge row under the heading
            // shows BOTH projections as supported, and the effect rows say why they are one switch.
            InfoToggle(title: "Advertise for wireless projection", key: "wirelessRadios", isOn: $model.wirelessEnabled)

        case .hotHandover:
            Toggle("Switch a live wireless session to the cable on insert", isOn: $model.hotHandover)
                .disabled(!model.wirelessEnabled)

        case .pairing:
            // Two toggles for one feature: the association model, and who answers it. The second is
            // meaningful only under Numeric Comparison (the model comment: "Unreachable with iOS as
            // the peer — kept for other peers"), so it follows the first. Both are DISABLED, not
            // hidden, when the radios are off — the badges stay visible so the gate is legible.
            Toggle("Numeric Comparison (6-digit code shown on both sides)", isOn: $model.pairingNumericComparison)
                .disabled(!model.wirelessEnabled)
            Toggle("Answer pairing code in-app (not usable with iPhone as peer)", isOn: $model.pairingInteractiveAnswer)
                .disabled(!model.wirelessEnabled || !model.pairingNumericComparison)

        case .wifiAccessPoint:
            // Defect 8. Polarity: ON is the default and emits NOTHING; only OFF reaches the wire as
            // `wifi_ap: false` (emitter, SettingsWindow.swift `yaml`). Do not invert this binding.
            InfoToggle(title: "Run the box's own Wi-Fi access point", key: "wifiAccessPoint", isOn: $model.wifiAccessPoint)

        case .androidAutoProjection:
            Toggle("Offer projection to an Android phone on box's USB bus", isOn: $model.androidAutoEnabled)

        case .appDrivenSetup:
            Toggle("Author the session SETUP response in this app", isOn: $model.appDrivenSetup)

        case .headUnitName, .branding, .panelGeometry, .frameRate, .pixelDensity, .insets, .videoCodec,
             .altDisplay, .theme, .statusBar, .driverPosition, .drivingRestrictions, .powertrain,
             .inputDevices, .audioProfile, .metadataFeeds:
            // Vehicle-tab features (W3). Listed rather than `default:` so a feature added to the
            // adapter section without a control fails to compile instead of rendering nothing.
            EmptyView()
        }
    }

    /// Protocol-NEUTRAL footnotes — facts about the box that neither projection's effect sentence
    /// carries. Anything that names a protocol goes into `FeatureMatrix`, not here (defect 7).
    private func footnote(for feature: Feature) -> String? {
        switch feature {
        case .wirelessRadios:
            // Text moved to FieldInfo.text["wirelessRadios"] 2026-09-08 (ten-word rule, DESIGN.md
            // §11.4) — reached by the (i) on this feature's own toggle in `control(for:)`.
            return nil
        case .hotHandover, .pairing:
            return model.wirelessEnabled ? nil : "Needs Wireless radios; currently off, so no effect."
        case .wifiAccessPoint:
            // Text moved to FieldInfo.text["wifiAccessPoint"] 2026-09-08 (ten-word rule) — reached by
            // the (i) on this feature's own toggle in `control(for:)`. Polarity fact preserved there.
            return nil
        case .androidAutoProjection:
            return "Off: Android phone only charges. First to connect wins."
        case .appDrivenSetup:
            return "Box answers itself if this app doesn't respond."
        default:
            return nil
        }
    }

    // MARK: Live box state

    /// The ONE row visible when the box-state telemetry stack is collapsed. Carries the amber "stale"
    /// dot (promoted out of the status line so it reads even without expanding — DESIGN.md §1). Purely
    /// informational — no interactive control lives here, because this header sits INSIDE the
    /// `.disabled(store.busy)` `Group` (risk 1) and a nested `.disabled(false)` cannot re-enable what
    /// an ancestor `.disabled(true)` already turned off (`disabled(_:)` propagates down the hierarchy;
    /// an outer modifier always wins over an inner one). Refresh therefore stays in `bottomBar`,
    /// outside this `Group`, exactly where it always was — it is the self-heal action for a stuck
    /// `busy`, so it must stay reachable precisely when this section reads "busy".
    private var boxStateSummaryHeader: some View {
        HStack(spacing: 8) {
            if store.stale {
                Circle().fill(Color.orange).frame(width: 8, height: 8)
            }
            Text("Box state")
            if store.stale {
                Text("stale").font(.caption).foregroundStyle(.orange)
            }
            Spacer()
            Text(store.statusText).font(.caption).foregroundStyle(.secondary)
        }
    }

    @ViewBuilder
    private var boxState: some View {
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
        }
        // Box-pushed live state (CT_BOX_HEALTH / CT_BT_PHASE / CT_PHONE_IDENT on CH_CTRL) — independent
        // of the CH_MGMT snapshot above, so it renders before/without a GET_INFO reply. Same three
        // values Window ▸ Adapter Info prints; `sessionEnded()` clears them so nothing here is a
        // carry-over from a previous box or phone.
        if store.boxHealth != nil || store.btPhase != nil || store.phoneIdent != nil {
            Section("Live State") {
                if let health = store.boxHealth {
                    ForEach(health.checklist, id: \.label) { item in
                        HealthRow(title: item.label, ok: item.ok)
                    }
                }
                if let phase = store.btPhase {
                    LabeledContent("Bluetooth phase", value: phase.displayName)
                }
                if let ident = store.phoneIdent {
                    LabeledContent("Phone", value: "\(ident.model) (\(ident.osName) \(ident.osVersion))")
                    LabeledContent("Phone name", value: ident.name)
                }
            }
        }
        if let i = store.info {
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
    }

    private var boxControls: some View {
        Section {
            // 2x2 grid of EQUAL-WIDTH buttons (owner, 2026-09-09). Four `Button`s as bare Form rows
            // sized to their own labels, so they stepped raggedly from "Restart adapter" to "Enter NCM
            // maintenance mode" down the left edge. `Grid` + `.frame(maxWidth: .infinity)` on each
            // label makes every cell the width of the widest, and `.gridCellUnsizedAxes` is NOT used
            // because we WANT them stretched. Roles are unchanged: three stay `.destructive` so they
            // keep their tint, and every action still only SETS a `confirm*` flag — the four
            // `.confirmationDialog` modifiers stay attached at the top level of `body` (DESIGN.md
            // §11.6 risk 3: re-parenting a dialog into a nested container can detach its binding).
            Grid(horizontalSpacing: 8, verticalSpacing: 8) {
                GridRow {
                    Button { store.restartWireless() } label: {
                        Label("Restart wireless stack", systemImage: "wifi")
                            .frame(maxWidth: .infinity)
                    }
                    Button(role: .destructive) { confirmForgetAll = true } label: {
                        Label("Forget all paired devices", systemImage: "trash")
                            .frame(maxWidth: .infinity)
                    }
                }
                GridRow {
                    Button(role: .destructive) { confirmReboot = true } label: {
                        Label("Restart adapter", systemImage: "arrow.clockwise.circle")
                            .frame(maxWidth: .infinity)
                    }
                    Button(role: .destructive) { confirmEnterNCM = true } label: {
                        Label("Enter NCM mode", systemImage: "terminal")
                            .frame(maxWidth: .infinity)
                    }
                }
            }
            .frame(maxWidth: .infinity)
        } header: {
            Text("Controls")
        } footer: {
            // Defect 7: "any live session", not "any live CarPlay session".
            // Text moved to FieldInfo.text["adapterControls"] 2026-09-08 (ten-word rule, DESIGN.md
            // §11.4). All five facts — including the exact `rm /script/ncm_only; reboot` return
            // command — live behind this (i). The destructive actions' own confirmation dialogs keep
            // their FULL content regardless (§11.4 exemption); only this explainer moved.
            InfoLabel(title: "About these controls", key: "adapterControls")
                .font(.caption)
        }
    }

    // MARK: Bottom bar

    /// Phase 3 dissolved the two stacked bars this used to be (DESIGN.md §1, §11.6, §11.9). Explicitly
    /// OUT OF SCOPE for this pass: auto-save, and moving/removing Save — both stay exactly as they
    /// were, `if model.dirty` and all, because Save's placement is its own unresolved design pass
    /// (§11.9) and this tab's Save has never been `.disabled(!model.dirty)` — the button does not
    /// exist when clean, and that stays true. Refresh STAYS here too (not relocated into
    /// `boxStateSummaryHeader`, which sits inside the `.disabled(store.busy)` `Group` — an inner
    /// `.disabled(false)` cannot re-enable what an outer `.disabled(true)` turned off, so Refresh must
    /// live outside that `Group` to remain the self-heal action while busy, exactly as before). What
    /// is retired is the second bar's OWN chrome: one row now, status text + `ProgressView` + Refresh —
    /// "a small amount of information," per the HIG bottom-bar rule §11.9 quotes. Absent rather than
    /// empty when there is nothing to say.
    @ViewBuilder
    private var bottomBar: some View {
        if model.dirty || store.busy || !statusLine.isEmpty {
            VStack(spacing: 8) {
                if model.dirty {
                    HStack(spacing: 10) {
                        Label("Unsaved changes — Save pushes to adapter (deferred while live)",
                              systemImage: "pencil.circle.fill")
                            .foregroundStyle(.orange)
                            .lineLimit(2)
                        Spacer()
                        Button("Save") { model.save() }
                            .keyboardShortcut("s", modifiers: .command)
                            .buttonStyle(.borderedProminent)
                    }
                }
                if store.busy || !statusLine.isEmpty {
                    HStack(spacing: 10) {
                        if store.busy { ProgressView().controlSize(.small) }
                        if !statusLine.isEmpty {
                            Text(statusLine).foregroundStyle(.secondary).lineLimit(1)
                        }
                        Spacer()
                        Button("Refresh", systemImage: "arrow.clockwise") { store.refresh() }
                    }
                }
            }
            .font(.callout)
            .padding(12).background(.bar)
        }
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
        if d.airplayd { up.append(DaemonDisplay.label(wireKey: "airplayd")) }
        if d.carplay_wireless { up.append(DaemonDisplay.label(wireKey: "carplay_wireless")) }
        return up.isEmpty ? "none" : up.joined(separator: ", ")
    }
}
