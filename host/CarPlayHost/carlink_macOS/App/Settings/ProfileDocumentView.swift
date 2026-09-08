// ProfileDocumentView.swift — Import / Export / Presets over the NEUTRAL profile document
// (`VehicleProfileDocument`, DESIGN.md §0 decision 2). Rendered as Form ROWS: W3's Vehicle tab
// embeds it inside its own `Section("Profile document")`, so there is no `Form` or `Section` here.
//
// WHY THIS EXISTS. Until 2026-09-04 the app could Export YAML but had NO import at all — the only
// `NSOpenPanel` in the Settings code was the OEM icon PNG picker — and no way to start from a known
// geometry other than "shipped default" or typing. Apple ships ten CarPlay Simulator templates and
// Google eleven DHU .ini presets for exactly that reason; `VehicleProfilePreset.builtIn` carries
// sixteen (shipped + 10 DHU-derived + 5 Apple-derived). This view is the UI over W1's document layer:
//
//   `ProfileDocumentIO.read(from:)` / `.write(_:to:)`   file bytes ⇄ VehicleProfileDocument
//   `VehicleConfigModel.document` / `.apply(_:)`        document ⇄ the @Published fields
//
// BOUNDARY (W3 relay via integrator, 2026-09-04): this block owns the neutral document only. The
// Apple-schema YAML — the rendered box config — keeps its own "Generated YAML" disclosure and
// "Export YAML…" action in VehicleTab.swift, CarPlay-badged, directly above this view. Do not add a
// second YAML export here; one artifact, one button.
//
// SEMANTICS. Import and Presets write into the live form and mark it dirty; they do NOT save. The
// user presses Save like after any other edit, so the "what is pushed to the box" rule stays one
// rule (`VehicleConfigModel.save`, and only it). Export writes `model.document` — the LIVE fields,
// the same thing the form shows — for the same reason the YAML export writes the live `yaml`.
//
// CLAMPING. `apply(_:)` ends with `clampInPlace()` and returns nothing (W1's deliberate choice, so
// this call site does not depend on a notice type that may grow). A document below the 800×480
// CarPlay floor (`dhu-6in`, 750×450 — a real DHU geometry, valid for Android Auto) is therefore
// corrected on load, and the ONLY way to know is to compare what was asked for with what the model
// holds afterwards. `clampSummary(requested:applied:)` does that on `display` / `altDisplay` and the
// row under the buttons says so — loading a preset and silently getting a different panel is the
// failure DESIGN.md §8 names.
//
// ERRORS. `error.localizedDescription`, verbatim. `ProfileDocumentIO.IOError` exists precisely
// because `DocumentError` / `DecodingError` are not `LocalizedError` and would render as "The
// operation couldn't be completed. (… error 1.)"; its text carries the file name, the JSON key path
// and the mismatch, which is what a hand-edited file needs. No error prose is authored here.
//
// Created 2026-09-04 (Settings reorganisation, DESIGN.md §6 W4).

import SwiftUI
import UniformTypeIdentifiers

struct ProfileDocumentView: View {
    @ObservedObject private var model: VehicleConfigModel
    /// The preset awaiting confirmation. A preset replaces EVERY value in the form (it is a whole
    /// document), so it is confirmed like Revert to Default is — with its `summary` and, when the
    /// geometry is below the floor, the clamp it will get, BEFORE anything changes.
    @State private var pendingPreset: VehicleProfilePreset?
    /// An imported document awaiting confirmation. Parsed FIRST (so a malformed file fails with its
    /// own error before anything is asked) and applied only on Yes: an import replaces every value
    /// exactly as a preset does and is confirmed the same way (finding 8 — it used to apply on the
    /// spot while presets asked).
    @State private var pendingImport: PendingImport?
    /// Outcome of the last Import / Preset / Export, shown as a row until the next action.
    @State private var outcome: Outcome?

    private struct PendingImport {
        let url: URL
        let document: VehicleProfileDocument
    }

    init(model: VehicleConfigModel) {
        _model = ObservedObject(wrappedValue: model)
    }

    private struct Outcome {
        enum Kind { case loaded, exported, clamped }
        let kind: Kind
        let text: String

        var symbol: String {
            switch kind {
            case .loaded:   return "checkmark.circle"
            case .exported: return "square.and.arrow.up"
            case .clamped:  return "exclamationmark.triangle"
            }
        }
        var tint: Color { kind == .clamped ? .orange : .secondary }
    }

    var body: some View {
        // "…profile…" in both titles: the CarPlay-schema "Export YAML…" sits a few rows up, and the
        // two artifacts must not read as one (finding 8).
        HStack(spacing: 8) {
            Button("Import profile…", systemImage: "square.and.arrow.down") { importDocument() }
            Button("Export profile…", systemImage: "square.and.arrow.up") { exportDocument() }
            presetsMenu
            Spacer()
        }
        // `confirmationDialog(_:isPresented:presenting:)` on a row: SwiftUI hoists the sheet to the
        // window, so attaching it here rather than to W3's Form keeps the whole flow in this file.
        // On the button row itself — the zero-height `Color.clear` spacer this used to hang off
        // still rendered as an empty Form row (finding 8).
        .confirmationDialog(
            pendingPreset.map { "Load preset “\($0.title)”?" } ?? "",
            isPresented: Binding(get: { pendingPreset != nil }, set: { if !$0 { pendingPreset = nil } }),
            presenting: pendingPreset
        ) { preset in
            Button("Load Preset") { apply(preset: preset) }
            Button("Cancel", role: .cancel) { pendingPreset = nil }
        } message: { preset in
            Text(presetMessage(preset))
        }
        .confirmationDialog(
            pendingImport.map { "Import “\($0.url.lastPathComponent)”?" } ?? "",
            isPresented: Binding(get: { pendingImport != nil }, set: { if !$0 { pendingImport = nil } }),
            presenting: pendingImport
        ) { pending in
            Button("Import") { apply(import: pending) }
            Button("Cancel", role: .cancel) { pendingImport = nil }
        } message: { pending in
            Text(importMessage(pending))
        }
        // The file format, once, so an owner knows what to hand-edit and what Import accepts.
        Text("JSON, .\(VehicleProfileDocument.fileExtension), schema v\(VehicleProfileDocument.currentSchemaVersion). Loading a document or a preset replaces every value in the form; press Save to push it.")
            .font(.caption).foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        if let outcome {
            Label(outcome.text, systemImage: outcome.symbol)
                .font(.caption)
                .foregroundStyle(outcome.tint)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    // MARK: Presets

    /// `VehicleProfilePreset.builtIn` grouped by origin, in catalogue order within each group. The
    /// origin is what an owner picks by ("the DHU 720p one", "Apple's Widescreen"); the `summary`
    /// is the hover text and repeats in the confirmation.
    private var presetsMenu: some View {
        Menu {
            ForEach(PresetGroup.allCases, id: \.self) { group in
                let members = VehicleProfilePreset.builtIn.filter { group.contains($0) }
                if !members.isEmpty {
                    Section(group.title) {
                        ForEach(members) { preset in
                            Button(preset.title) { pendingPreset = preset }
                                .help(preset.summary)
                        }
                    }
                }
            }
        } label: {
            Label("Presets", systemImage: "list.bullet.rectangle")
        }
        .fixedSize()
    }

    private enum PresetGroup: CaseIterable {
        case project, desktopHeadUnit, carPlaySimulator

        var title: String {
            switch self {
            case .project:          return "This project"
            case .desktopHeadUnit:  return "Google Desktop Head Unit"
            case .carPlaySimulator: return "Apple CarPlay Simulator"
            }
        }
        func contains(_ p: VehicleProfilePreset) -> Bool {
            switch (self, p.origin) {
            case (.project, .project), (.desktopHeadUnit, .desktopHeadUnit), (.carPlaySimulator, .carPlaySimulator):
                return true
            default:
                return false
            }
        }
    }

    private func presetMessage(_ preset: VehicleProfilePreset) -> String {
        var lines = [preset.summary]
        switch preset.origin {
        case .project:
            break
        case let .desktopHeadUnit(file):
            lines.append("Derived from the DHU preset \(file).")
        case let .carPlaySimulator(file):
            lines.append("Derived from the CarPlay Simulator template \(file).")
        }
        // Pre-flight the clamp so the owner sees "you will get 800×480" before saying yes, not after.
        if let clamp = Self.clampSummary(requested: preset.vehicle, applied: Self.clamped(preset.vehicle)) {
            lines.append(clamp)
        }
        lines.append("Every value in the form is replaced. Nothing is pushed until Save.")
        return lines.joined(separator: "\n\n")
    }

    private func apply(preset: VehicleProfilePreset) {
        pendingPreset = nil
        let requested = preset.vehicle
        model.apply(preset: preset)
        finishLoad(what: "Preset “\(preset.title)”", requested: requested)
    }

    // MARK: Import / Export

    private func importDocument() {
        let panel = NSOpenPanel()
        panel.title = "Import Vehicle Profile"
        panel.message = "Choose a .\(VehicleProfileDocument.fileExtension) document"
        panel.allowedContentTypes = [.json]
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            do {
                // Parse now, apply after the confirmation: a bad file fails here with its own
                // error, and a good one is shown for what it is before it replaces the form.
                pendingImport = PendingImport(url: url, document: try ProfileDocumentIO.read(from: url))
            } catch {
                // IOError.malformed → "<file> is not a usable vehicle profile: <path>: <reason>".
                Self.alert("Import Failed", error.localizedDescription)
            }
        }
    }

    /// Same shape as `presetMessage`: where the document came from, what the clamp will do to it,
    /// and that every value goes.
    private func importMessage(_ pending: PendingImport) -> String {
        var lines: [String] = []
        if let preset = pending.document.preset {
            lines.append("Saved from preset “\(preset)”.")
        }
        if let clamp = Self.clampSummary(requested: pending.document.vehicle,
                                         applied: Self.clamped(pending.document.vehicle)) {
            lines.append(clamp)
        }
        lines.append("Every value in the form is replaced. Nothing is pushed until Save.")
        return lines.joined(separator: "\n\n")
    }

    private func apply(import pending: PendingImport) {
        pendingImport = nil
        model.apply(pending.document)
        let origin = pending.document.preset.map { " (from preset “\($0)”)" } ?? ""
        finishLoad(what: "\(pending.url.lastPathComponent)\(origin)", requested: pending.document.vehicle)
    }

    private func exportDocument() {
        let panel = NSSavePanel()
        panel.title = "Export Vehicle Profile"
        panel.nameFieldStringValue = "carlink.\(VehicleProfileDocument.fileExtension)"
        panel.allowedContentTypes = [.json]
        panel.canCreateDirectories = true
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            do {
                // The LIVE document (the fields on screen), not the committed snapshot — same rule as
                // the YAML export; exporting something other than what is shown is silently dishonest.
                try ProfileDocumentIO.write(model.document, to: url)
                outcome = Outcome(kind: .exported, text: "Exported \(url.lastPathComponent).")
            } catch {
                Self.alert("Export Failed", error.localizedDescription)
            }
        }
    }

    /// Common tail of Import and Preset: report what loaded and, if `clampInPlace()` moved the
    /// geometry, exactly from what to what. `model.profile` is read AFTER `apply`, so it is the
    /// clamped truth.
    private func finishLoad(what: String, requested: VehicleProfile) {
        if let clamp = Self.clampSummary(requested: requested, applied: model.profile) {
            outcome = Outcome(kind: .clamped, text: "Loaded \(what) — \(clamp) Save to push.")
        } else {
            outcome = Outcome(kind: .loaded, text: "Loaded \(what). Save to push.")
        }
    }

    // MARK: Clamp detection

    /// One sentence naming every geometry the model changed on load, or nil when it took the
    /// document as authored. Compares the fields `VehicleConfigModel.clampInPlace()` touches
    /// (panel size, frame rate, insets, on both displays) — not the whole profile, because
    /// `apply` also normalises a few round-trip-only values that are not the owner's concern.
    static func clampSummary(requested r: VehicleProfile, applied a: VehicleProfile) -> String? {
        var changes: [String] = []
        func geometry(_ label: String, _ req: PanelGeometry, _ got: PanelGeometry) {
            if req.width != got.width || req.height != got.height {
                changes.append("\(label) \(req.width)×\(req.height) → \(got.width)×\(got.height)")
            }
            if req.maxFPS != got.maxFPS {
                changes.append("\(label) \(req.maxFPS) fps → \(got.maxFPS) fps")
            }
        }
        func insets(_ label: String, _ req: PanelInsets, _ got: PanelInsets) {
            if req != got { changes.append("\(label) insets adjusted") }
        }
        geometry("panel", r.display.panel, a.display.panel)
        insets("panel", r.display.insets, a.display.insets)
        if r.altDisplay.enabled {
            geometry("alt display", r.altDisplay.panel, a.altDisplay.panel)
            insets("alt display", r.altDisplay.insets, a.altDisplay.insets)
        }
        guard !changes.isEmpty else { return nil }
        return "adjusted to the app's limits (\(PanelRule.envelopeDescription)): "
            + changes.joined(separator: "; ") + "."
    }

    /// Pre-flight twin of `clampInPlace()` for the confirmation text — width/height/FPS only, the
    /// three things a shipped preset can trip (`dhu-6in`). The authoritative clamp still runs inside
    /// `apply`; `finishLoad` reports whatever it actually did, so a divergence here can only make
    /// the preview incomplete, never the outcome wrong.
    private static func clamped(_ v: VehicleProfile) -> VehicleProfile {
        var out = v
        func clamp(_ p: inout PanelGeometry) {
            // The SAME rule the model applies (orientation-aware, `PanelRule`), not a re-derivation.
            let c = PanelRule.clamped(width: p.width, height: p.height)
            p.width = c.width; p.height = c.height
            if !VehicleConfigModel.frameRates.contains(p.maxFPS) { p.maxFPS = p.maxFPS == 24 ? 30 : 60 }
        }
        clamp(&out.display.panel)
        if out.altDisplay.enabled {
            clamp(&out.altDisplay.panel)
            if !VehicleConfigModel.frameRates.contains(v.altDisplay.panel.maxFPS) { out.altDisplay.panel.maxFPS = 30 }
        }
        return out
    }

    @MainActor
    private static func alert(_ title: String, _ text: String) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = text
        alert.alertStyle = .warning
        alert.runModal()
    }
}
