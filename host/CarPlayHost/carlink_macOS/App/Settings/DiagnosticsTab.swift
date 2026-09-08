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
                Text("Streams the box's universal log (/tmp/box.log) over OCBM CH_LOG into Window ▸ Box Log, and into this app's own combined session log. Re-armed automatically after every SUBSCRIBE.")
                    .font(.caption)
            }
        }
        .formStyle(.grouped)
    }
}

