// DiagnosticsTab.swift — the Box Log stream toggle + cap (`BoxLogSettings`, its own UserDefaults
// keys, NOT part of the pushed VehicleConfig YAML: the box arms CH_LOG purely from CT_LOG_CTL) and
// the `DiagnosticsTab` view that exposes them.
//
// Split out of App/SettingsWindow.swift on 2026-09-04 (Settings reorganisation, DESIGN.md §6
// Phase 0) so the Diagnostics worker owns the view without touching the model file. Everything
// below was moved BYTE-FOR-BYTE from SettingsWindow.swift — no rewording, no reflow.
// `VehicleConfigModel` did NOT move: tools/regen_app_yaml_fixture.py text-extracts its YAML
// emitter from SettingsWindow.swift by string anchor, so relocating the model would silently
// disarm the app→box drift guard.
//
// Phase 1 (W4, 2026-09-04): content deliberately UNCHANGED. DESIGN.md §1 lists the Box Log toggle +
// cap under BOTH the Adapter tab and "Diagnostics — unchanged content"; it cannot be in both without
// two toggles for one setting, and moving it would leave this tab empty. It stays here — the log
// stream is a diagnostic of this app's session, not a box behaviour — and the Adapter tab does not
// duplicate it. Raised as a §1 inconsistency in the W4 report.
//
// 2026-09-09 (owner: "Move Stream Performance to the Diagnostics tab/area"): `StreamPerfSection()`
// (`App/StreamMetricsMonitor.swift`, frozen, unedited) moved here from `AdapterTab.swift`. It needed
// NO new wiring — it is a view over `StreamMetricsMonitor.shared`, a self-contained `ObservableObject`
// with its own 1 Hz timer, not over `AdapterTab`'s `store`/`model` or any adapter-connection state.
// This tab still has no `store`/busy concept and none was added. §1's "Box Log toggle + cap, and
// nothing else" description of this tab is now stale — see the report; DESIGN.md needs a §1 update.

import SwiftUI

// MARK: - Box Log settings (CT_LOG_CTL — NOT part of the pushed VehicleConfig YAML)

/// "Stream box log to this app" + cap — persisted directly in UserDefaults (its own keys, not the
/// `VehicleConfigModel.prefix` namespace) because this does not ride the YAML pushed at SUBSCRIBE: the
/// box arms/disarms CH_LOG purely from `CT_LOG_CTL` on CH_CTRL (docs/carplay/01_OCBM_PROTOCOL.md
/// CH_LOG). Default ON / 256 KB matches the box's own CT_LOG_CTL default (cap 0 ⇒ 256 KB).
@MainActor
final class BoxLogSettings: ObservableObject {
    static let shared = BoxLogSettings()

    private static let enabledKey = "boxLogStreamEnabled"
    private static let capKey = "boxLogCapKB"
    private let d = UserDefaults.standard

    @Published var streamEnabled: Bool { didSet { d.set(streamEnabled, forKey: Self.enabledKey); applyNow?(streamEnabled, UInt16(clamping: capKB)) } }
    @Published var capKB: Int { didSet { d.set(capKB, forKey: Self.capKey); applyNow?(streamEnabled, UInt16(clamping: capKB)) } }

    /// Wired by AppDelegate to the live `OCBMClient` (nil when disconnected) — lets a Settings change
    /// take effect immediately over `sendLogCtl` rather than waiting for the next SUBSCRIBE.
    var applyNow: ((Bool, UInt16) -> Void)?

    private init() {
        streamEnabled = d.object(forKey: Self.enabledKey) as? Bool ?? true
        capKB = d.object(forKey: Self.capKey) as? Int ?? 256
    }
}

struct DiagnosticsTab: View {
    @ObservedObject var settings = BoxLogSettings.shared

    var body: some View {
        Form {
            Section {
                Toggle("Stream box log to this app", isOn: $settings.streamEnabled)
                Stepper("Cap: \(settings.capKB) KB", value: $settings.capKB, in: 32...4096, step: 32)
                    .disabled(!settings.streamEnabled)
            } header: {
                Text("Box Log")
            } footer: {
                // Text moved to FieldInfo.text["boxLogStream"] 2026-09-08 (ten-word rule, DESIGN.md
                // §11.4). All four facts — /tmp/box.log, CH_LOG, both destinations, re-arm on
                // SUBSCRIBE — live behind this (i).
                InfoLabel(title: "About the box log stream", key: "boxLogStream")
                    .font(.caption)
            }
            // Live receive-side A/V stream health (measured on the Mac). Independent of the box log
            // section above — it reads the OCBM decrypt layer's per-stream counters at ~1 Hz. Moved
            // from the Adapter tab 2026-09-09 (owner ask); see header comment.
            StreamPerfSection()
        }
        .formStyle(.grouped)
    }
}

