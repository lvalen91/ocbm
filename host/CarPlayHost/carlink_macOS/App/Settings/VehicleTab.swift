// VehicleTab.swift — the Vehicle tab of the Settings window: the protocol-NEUTRAL vehicle profile,
// with what each projection does with every value rendered inline from `FeatureMatrix`.
//
// Rebuilt 2026-09-04 (Settings reorganisation, DESIGN.md §6 W3) from the former `ConfigurationTab`
// (since deleted) that Phase 0 moved here byte-for-byte out of App/SettingsWindow.swift. That form was shaped like
// Apple's CarPlay Simulator VehicleConfig YAML — "Main Video", "Limited UI Elements", "OEM Icon",
// "Advanced Capabilities" — and Android Auto was bolted on by reading six of its fields. The
// owner's steer: most of those settings are ONE feature with TWO vendor vocabularies (limited UI /
// driving_status, night mode / night_mode sensor, rightHandDrive / driverposition, oemIconConfig /
// display_name), so the tab is now laid out in `Feature.Section` order, one neutral control per
// `Feature`, and each control is followed by:
//
//   1. the badge row — one `ProjectionBadge` per projection with its support level
//      (`FeatureMatrix.supports(f)`), a click away from the full record;
//   2. the per-protocol explanation rows (`FeatureSupport.effect` + provenance), hidden when the
//      feature is uniform (`FeatureMatrix.isUniform`);
//   3. the vendor-EXCLUSIVE sub-groups, badged, INSIDE the feature they belong to — never on a
//      protocol tab (DESIGN.md §0 decision 4; `Feature.exclusiveKeys(on:)` is the placement table
//      and every `ExclusiveSubGroup` below names the (feature, projection) it implements);
//   4. a `ValueNoteLabel` when the current value has value-level provenance (which AA tier has
//      been seen to work, whether CENTER driver position has ever been declared, …).
//
// The discipline this replaces is the tooltip-prose one: "CarPlay-only" / "⚠️ not implemented" /
// "Android Auto: LIVE" sentences hand-written per key, which is how nightMode/rightHandDrive came
// to sit in a CarPlay "Appearance" section while being live only for AA (defect 2), and how `name`
// was called inert while AA advertises it in three places (defect 3). Nothing in this file says
// what a PROTOCOL does with a value; that text is `FeatureMatrix` data. The (i) popovers
// (FieldInfo.swift) explain what the neutral FACT is.
//
// What did not move: `VehicleConfigModel` and its YAML emitter stay in App/SettingsWindow.swift —
// tools/regen_app_yaml_fixture.py text-extracts the emitter by string anchor, and the pushed
// CarPlay document must stay byte-identical for every existing configuration (DESIGN.md §2). This
// file only binds to the model's existing `@Published` fields plus the neutral ones Phase 0 added
// (§5); the neutral → CarPlay derivation is W1's `profile` extension, and the neutral → Android Auto
// rendering is W2's `AACapability(profile:adapter:warn:)`, whose `negotiationNotes` this tab shows
// under Panel resolution (defect 4) so a geometry AA cannot express stops degrading silently.
//
// The row-level helpers (`ResolutionField`, `SafeAreaField`, `FrameRatePicker`, the audio editor
// and reference, `LiveAppearanceSection`) are the Phase 0 originals, reused as-is where the
// comments say so.

import AppKit
import SwiftUI

// MARK: - Row helpers (Phase 0 originals)

private struct ResolutionField: View {
    let title: String
    @Binding var width: Int
    @Binding var height: Int
    var infoKey: String = "panelResolution"  // alt row passes "altResolution" so its (i) shows the alt text

    /// Preset options; nil dims = Custom (fields become editable). `shortLabel` fits the segmented control.
    private static let presets: [(shortLabel: String, w: Int?, h: Int?)] = [
        ("SD", 1280, 720),
        ("HD", 1920, 1080),
        ("4K", 3840, 2160),
        ("Custom", nil, nil),
    ]

    /// Custom is an explicit user choice, NOT derivable from the numbers: e.g. 1280×720 both *is* the
    /// SD preset and is a legal custom value. Deriving `isCustom` purely from a preset match made
    /// "Custom" unreachable whenever the dims happened to equal a preset (the picker snapped back and
    /// the fields stayed greyed). Track the choice; seed it true when a loaded config matches no preset.
    @State private var forceCustom = false

    private static var customIndex: Int { presets.count - 1 }
    private var matchedPreset: Int? {
        Self.presets.firstIndex { $0.w == width && $0.h == height }
    }
    /// The preset index matching the current W×H, or the Custom index if the user chose Custom / no
    /// preset matches.
    private var selectedIndex: Int {
        if forceCustom { return Self.customIndex }
        return matchedPreset ?? Self.customIndex
    }
    private var isCustom: Bool { selectedIndex == Self.customIndex }

    /// Which field to paint red: `PanelRule`'s orientation-aware envelope (each side 480–3840, the
    /// floor by the panel's own aspect), never a per-axis range — the per-axis one was how a
    /// portrait panel read as "height out of range" (see `PanelRule`). Presets are always legal.
    private var axisError: (width: Bool, height: Bool) {
        isCustom ? PanelRule.axisOutOfRange(width: width, height: height) : (false, false)
    }
    private var widthError: Bool { axisError.width }
    private var heightError: Bool { axisError.height }

    var body: some View {
        // Segmented preset picker (matches the Frame Rate picker style).
        Picker(selection: Binding(
            get: { selectedIndex },
            set: { idx in
                if let w = Self.presets[idx].w, let h = Self.presets[idx].h {
                    width = w; height = h; forceCustom = false
                } else {
                    // Custom: keep current values; unlock the fields for editing.
                    forceCustom = true
                }
            }
        )) {
            ForEach(Self.presets.indices, id: \.self) { Text(Self.presets[$0].shortLabel).tag($0) }
        } label: {
            InfoLabel(title: title, key: infoKey)
        }
        .pickerStyle(.segmented)

        // The manual custom fields, labeled "Resolution"; editable only in Custom.
        LabeledContent("Resolution") {
            HStack(spacing: 4) {
                TextField("W", value: $width, format: .number)
                    .frame(width: 62).multilineTextAlignment(.trailing)
                    .foregroundStyle(widthError ? Color.red : Color.primary)
                Text("×").foregroundStyle(.secondary)
                TextField("H", value: $height, format: .number)
                    .frame(width: 62).multilineTextAlignment(.trailing)
                    .foregroundStyle(heightError ? Color.red : Color.primary)
                Text("px").foregroundStyle(.tertiary).font(.caption)
            }
            .textFieldStyle(.roundedBorder)
            .disabled(!isCustom)
            .opacity(isCustom ? 1 : 0.5)
        }
        // The verdict line, same contract as the view-area and insets lines: the rule's own text,
        // verbatim, naming the orientation it applied and what Save will store. Shown while typing.
        if isCustom, let verdict = PanelRule.verdict(width: width, height: height) {
            Text(verdict)
                .font(.caption).foregroundStyle(Color.red)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}

/// A live, proportional picture of the display: a faded box at the resolution's aspect ratio (the
/// viewArea — the video always fills this) with an inner solid box for the safe area, positioned by
/// the insets. Lets the user SEE the safe area, not just read numbers (WWDC 2019-252: the safe area
/// is the rectangle where CarPlay keeps interactive UI; outside it is black unless "draw outside" is on).
private struct SafeAreaPreview: View {
    let resW: Int, resH: Int
    let left: Int, top: Int, right: Int, bottom: Int

    var body: some View {
        let c = PreviewCanvas.size(resW: resW, resH: resH)
        let fw = CGFloat(max(resW, 1)), fh = CGFloat(max(resH, 1))
        let l = CGFloat(max(0, left)), t = CGFloat(max(0, top))
        let sw = max(1, fw - l - CGFloat(max(0, right)))
        let sh = max(1, fh - t - CGFloat(max(0, bottom)))
        VStack(spacing: 5) {
            ZStack(alignment: .topLeading) {
                // The display / viewArea — faded fill (video fills the whole rectangle).
                RoundedRectangle(cornerRadius: 5)
                    .fill(Color.accentColor.opacity(0.12))
                    .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(Color.secondary.opacity(0.55)))
                // The safe area — solid inner box, offset + sized proportionally to the insets.
                RoundedRectangle(cornerRadius: 3)
                    .fill(Color.accentColor.opacity(0.30))
                    .overlay(RoundedRectangle(cornerRadius: 3).strokeBorder(Color.accentColor, lineWidth: 1.5))
                    .frame(width: c.width * sw / fw, height: c.height * sh / fh)
                    .offset(x: c.width * l / fw, y: c.height * t / fh)
            }
            .frame(width: c.width, height: c.height)
            HStack(spacing: 12) {
                PreviewCanvas.legend(Color.accentColor.opacity(0.18), "Display \(resW)×\(resH)")
                PreviewCanvas.legend(Color.accentColor.opacity(0.55), "Safe area")
            }
            .font(.caption2).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 2)
    }
}

/// The ONE sizing + caption convention for every to-scale display picture in this tab
/// (`SafeAreaPreview`, `ViewAreasPreview`): a panel of the resolution's aspect fitted into 260×132
/// points, and a swatch-plus-label legend row. Shared so the two previews read as one picture
/// language rather than two.
private enum PreviewCanvas {
    static let maxW: CGFloat = 260, maxH: CGFloat = 132

    /// The on-screen canvas for the display box — the resolution's aspect, scaled to fit maxW×maxH.
    static func size(resW: Int, resH: Int) -> CGSize {
        guard resW > 0, resH > 0 else { return CGSize(width: maxW, height: maxW * 9 / 16) }
        let a = CGFloat(resW) / CGFloat(resH)
        var w = maxW, h = maxW / a
        if h > maxH { h = maxH; w = maxH * a }
        return CGSize(width: w.rounded(), height: h.rounded())
    }

    static func legend(_ color: Color, _ text: String, dashed: Bool = false) -> some View {
        HStack(spacing: 4) {
            RoundedRectangle(cornerRadius: 2).fill(color)
                .frame(width: 12, height: 10)
                .overlay(RoundedRectangle(cornerRadius: 2)
                    .strokeBorder(dashed ? Color.red : Color.secondary.opacity(0.5),
                                  style: StrokeStyle(lineWidth: 1, dash: dashed ? [2, 2] : [])))
            Text(text)
        }
    }
}

/// The resize picture: BOTH main-stream view areas CarPlay will switch between, to scale on the
/// panel, so the owner sees the DIFFERENCE between the two layouts — area [0] is the full panel
/// (with the main safe box from the insets above), area [1] is the `viewArea2*` rect at its true
/// position and size (the origin is free, WWDC 2019-252; a floating or corner-pinned area is legal).
///
/// Honest about what the emitter writes (`VehicleConfigModel.viewArea2YAML`): area [1]'s nested
/// `safeArea` is a COPY of its `viewArea` — full-bleed, no insets of its own — so it is drawn as one
/// solid box, not as a view box with a safe box inside. Do not draw an inset that is not in the YAML.
///
/// A rect with a verdict (`viewArea2Verdict` non-nil) is drawn red and dashed and captioned "not
/// pushed": the emitter leaves it out of the document, and three of the four rules it can fail are
/// session teardowns on hardware. This view mutates nothing and re-derives no rule — the model's
/// verdict is the only judgement it renders.
private struct ViewAreasPreview: View {
    let panelW: Int, panelH: Int
    let safeLeft: Int, safeTop: Int, safeRight: Int, safeBottom: Int
    let x: Int, y: Int, w: Int, h: Int
    let verdict: String?

    var body: some View {
        let c = PreviewCanvas.size(resW: panelW, resH: panelH)
        let fw = CGFloat(max(panelW, 1)), fh = CGFloat(max(panelH, 1))
        let sx = c.width / fw, sy = c.height / fh
        // Area [0]'s safe box — the same arithmetic as SafeAreaPreview.
        let l = CGFloat(max(0, safeLeft)), t = CGFloat(max(0, safeTop))
        let sw = max(1, fw - l - CGFloat(max(0, safeRight)))
        let sh = max(1, fh - t - CGFloat(max(0, safeBottom)))
        let illegal = verdict != nil
        let area1 = illegal ? Color.red : Color.orange
        VStack(spacing: 5) {
            ZStack(alignment: .topLeading) {
                // Area [0] — the whole panel (video fills it), faded, exactly as SafeAreaPreview draws it.
                RoundedRectangle(cornerRadius: 5)
                    .fill(Color.accentColor.opacity(0.12))
                    .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(Color.secondary.opacity(0.55)))
                // Area [0]'s safe box, from the insets above.
                RoundedRectangle(cornerRadius: 3)
                    .fill(Color.accentColor.opacity(0.30))
                    .overlay(RoundedRectangle(cornerRadius: 3).strokeBorder(Color.accentColor, lineWidth: 1.5))
                    .frame(width: sw * sx, height: sh * sy)
                    .offset(x: l * sx, y: t * sy)
                // Area [1] — the resize target, at its true relative origin and size. Its safe area IS
                // this rect (full-bleed), hence one solid box. Drawn last so it reads on top of area 0;
                // an out-of-panel rect runs off the canvas edge, which `.clipped()` makes visible.
                RoundedRectangle(cornerRadius: 3)
                    .fill(area1.opacity(illegal ? 0.18 : 0.32))
                    .overlay(RoundedRectangle(cornerRadius: 3)
                        .strokeBorder(area1, style: StrokeStyle(lineWidth: 1.5, dash: illegal ? [4, 3] : [])))
                    .frame(width: max(1, CGFloat(w) * sx), height: max(1, CGFloat(h) * sy))
                    .offset(x: CGFloat(x) * sx, y: CGFloat(y) * sy)
            }
            .frame(width: c.width, height: c.height)
            .clipped()
            HStack(spacing: 12) {
                PreviewCanvas.legend(Color.accentColor.opacity(0.18), "Area 0 · panel \(panelW)×\(panelH)")
                PreviewCanvas.legend(Color.accentColor.opacity(0.55), "Safe area")
                PreviewCanvas.legend(area1.opacity(illegal ? 0.25 : 0.5),
                                     illegal ? "Area 1 · not pushed" : "Area 1 · \(w)×\(h) @ (\(x), \(y))",
                                     dashed: illegal)
            }
            .font(.caption2).foregroundStyle(.secondary)
            Text(illegal ? "Area 1 is left out of the pushed config until it is legal; CarPlay will see the full panel only."
                         : "Area 1's safe area is its whole rect — the config carries no separate insets for it.")
                .font(.caption2).foregroundStyle(illegal ? Color.red : Color.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 2)
    }
}

/// The SECOND main view area — the CarPlay Dock "resize" button's target (DESIGN.md §11.10): a
/// toggle, the W×H@X,Y rect in panel pixels, the model's verdict, and `ViewAreasPreview` drawing
/// both layouts to scale. A VIEW over `VehicleConfigModel.viewArea2*`: it reads the same fields the
/// emitter reads, clamps nothing, and renders `viewArea2Verdict` verbatim — the rules (containment,
/// all-even, positive, the 800×480 / 480×800 product floor) live in `ViewArea2Rule`, not here.
/// Mirrors `SafeAreaField`'s idiom: per-value fields in a Grid, a one-line summary, the verdict in
/// the insets line's shape, then the picture.
private struct ViewArea2Field: View {
    @ObservedObject var model: VehicleConfigModel

    private func field(_ label: String, _ value: Binding<Int>, invalid: Bool) -> some View {
        HStack(spacing: 3) {
            Text(label).font(.caption).foregroundStyle(.secondary).frame(width: 16, alignment: .leading)
            TextField("0", value: value, format: .number)
                .frame(width: 54).multilineTextAlignment(.trailing)
                .foregroundStyle(invalid ? Color.red : Color.primary)
        }
    }

    var body: some View {
        InfoToggle(title: "Second view area (Dock resize)", key: "viewArea2Enabled", isOn: $model.viewArea2Enabled)
        if model.viewArea2Enabled {
            let verdict = model.viewArea2Verdict
            let invalid = verdict != nil
            let floor = model.viewArea2MinimumSize
            LabeledContent {
                Grid(horizontalSpacing: 10, verticalSpacing: 4) {
                    GridRow { field("W", $model.viewArea2W, invalid: invalid); field("H", $model.viewArea2H, invalid: invalid) }
                    GridRow { field("X", $model.viewArea2X, invalid: invalid); field("Y", $model.viewArea2Y, invalid: invalid) }
                }
                .textFieldStyle(.roundedBorder)
            } label: {
                InfoLabel(title: "Area 1 (px)", key: "viewArea2Rect")
            }
            LabeledContent("Layouts") {
                Text("\(model.mainWidth) × \(model.mainHeight) full panel  ↔  "
                     + "\(model.viewArea2W) × \(model.viewArea2H) @ (\(model.viewArea2X), \(model.viewArea2Y))"
                     + " · floor \(floor.width) × \(floor.height)")
                    .font(.caption)
                    .foregroundStyle(invalid ? Color.red : .secondary)
            }
            if let verdict {
                Text(verdict)
                    .font(.caption).foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }
            ViewAreasPreview(panelW: model.mainWidth, panelH: model.mainHeight,
                             safeLeft: model.mainSafeLeft, safeTop: model.mainSafeTop,
                             safeRight: model.mainSafeRight, safeBottom: model.mainSafeBottom,
                             x: model.viewArea2X, y: model.viewArea2Y, w: model.viewArea2W, h: model.viewArea2H,
                             verdict: verdict)
        }
    }
}

/// Per-edge inset editor with a live visual preview. The user enters px from each edge; the
/// resulting safe box is shown as a number AND drawn to scale. Converted to the wire's absolute rect
/// (originX/width) by the CarPlay renderer on Save.
///
/// The `drawUIOutsideSafeArea` toggle that used to live here moved OUT (2026-09-04): the placement
/// table (`Feature.exclusiveKeys(.insets, on: .carPlay)`) lists `mainDrawOutsideSafe` as a CarPlay-
/// exclusive control, so the Display section renders it inside the CarPlay sub-group under this
/// field, gated on the same `isInset` rule. The alt stream never had it (main-display-only flag,
/// WWDC 2023-10150).
private struct SafeAreaField: View {
    @Binding var left: Int
    @Binding var top: Int
    @Binding var right: Int
    @Binding var bottom: Int
    let resWidth: Int
    let resHeight: Int
    var infoKey: String = "panelInsets"

    private var safeW: Int { resWidth - max(0, left) - max(0, right) }
    private var safeH: Int { resHeight - max(0, top) - max(0, bottom) }
    private var invalid: Bool {
        left < 0 || top < 0 || right < 0 || bottom < 0 || safeW < 16 || safeH < 16
    }
    private var isInset: Bool { left > 0 || top > 0 || right > 0 || bottom > 0 }

    private func field(_ label: String, _ value: Binding<Int>) -> some View {
        HStack(spacing: 3) {
            Text(label).font(.caption).foregroundStyle(.secondary).frame(width: 16, alignment: .leading)
            TextField("0", value: value, format: .number)
                .frame(width: 54).multilineTextAlignment(.trailing)
                .foregroundStyle(invalid ? Color.red : Color.primary)
        }
    }

    var body: some View {
        LabeledContent {
            Grid(horizontalSpacing: 10, verticalSpacing: 4) {
                GridRow { field("L", $left); field("T", $top) }
                GridRow { field("R", $right); field("B", $bottom) }
            }
            .textFieldStyle(.roundedBorder)
        } label: {
            InfoLabel(title: "Insets (px)", key: infoKey)
        }

        LabeledContent("Safe box") {
            Text(isInset ? "\(max(0, safeW)) × \(max(0, safeH)) px @ (\(max(0, left)), \(max(0, top)))"
                         : "full frame (no inset)")
                .font(.caption)
                .foregroundStyle(invalid ? Color.red : .secondary)
        }
        if invalid {
            Text("Insets leave too little room — each side must keep ≥16 px.")
                .font(.caption).foregroundStyle(.red)
        }

        SafeAreaPreview(resW: resWidth, resH: resHeight, left: left, top: top, right: right, bottom: bottom)
    }
}

private struct FrameRatePicker: View {
    let title: String
    @Binding var fps: Int
    var infoKey: String = "panelFrameRate"
    var body: some View {
        Picker(selection: $fps) {
            ForEach(VehicleConfigModel.frameRates, id: \.self) { Text("\($0) fps").tag($0) }
        } label: { InfoLabel(title: title, key: infoKey) }
        .pickerStyle(.segmented)
    }
}

// MARK: - Audio capability config (Phase 0 originals; CarPlay-exclusive, rendered under the CarPlay badge)

/// Friendly labels for the audio vocabulary (the raw tokens are the box's wire names).
enum AudioLabels {
    static func codec(_ t: String) -> String {
        if t == "none" { return "None" }
        return t
            .replacingOccurrences(of: "aac_lc", with: "AAC-LC")
            .replacingOccurrences(of: "aac_eld", with: "AAC-ELD")
            .replacingOccurrences(of: "pcm", with: "PCM")
            .replacingOccurrences(of: "opus", with: "Opus")
            .replacingOccurrences(of: "_", with: " ")
    }
    static func stream(_ t: Int) -> String {
        switch t {
        case 100: return "100 · MainAudio"
        case 101: return "101 · AltAudio"
        case 102: return "102 · MainHighAudio"
        default: return "\(t)"
        }
    }
    static func audioType(_ t: String) -> String { t.isEmpty ? "— (catch-all)" : t }
    /// One-line summary of what a non-custom mode advertises, so the user SEES the resolved set.
    static func modeSummary(_ mode: String) -> String {
        switch mode {
        case "auto": return "PCM over wired · full AAC set over wireless (matches how the phone connects)."
        case "wired_pcm": return "PCM 16k/48k on types 100/101 — the wired media path (no audioType)."
        case "wireless_8": return "8 entries: AAC-LC media (102) · AAC-ELD Siri/mic (100) · AAC-ELD alert (100/101) · PCM compatibility."
        default: return ""
        }
    }
}

/// A codec picker that marks each option device-verified (●) vs advertisable-but-not-yet-proven (○), so
/// the user knows which formats are documented-capable today. This is the honesty surface the config is
/// meant to provide.
private struct CodecPicker: View {
    let title: String
    @Binding var value: String
    var body: some View {
        Picker(title, selection: $value) {
            ForEach(VehicleConfigModel.audioCodecs, id: \.self) { c in
                let mark = VehicleConfigModel.verifiedCodecs.contains(c) ? "●" : "○"
                Text("\(mark)  \(AudioLabels.codec(c))").tag(c)
            }
        }
    }
}

/// The custom `audio.formats` editor: an add/remove list of advertised entries, each with stream type,
/// audioType, input (mic) and output (playback) codec pickers. Fully declarative — this is where any
/// head-unit audio configuration is authored for testing.
private struct AudioFormatsEditor: View {
    @ObservedObject var model: VehicleConfigModel

    var body: some View {
        ForEach($model.audioFormats) { $row in
            GroupBox {
                Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 6) {
                    GridRow {
                        Text("Stream").font(.caption).foregroundStyle(.secondary).gridColumnAlignment(.leading)
                        Picker("", selection: $row.streamType) {
                            ForEach(VehicleConfigModel.audioStreamTypes, id: \.self) {
                                Text(AudioLabels.stream($0)).tag($0)
                            }
                        }.labelsHidden()
                    }
                    GridRow {
                        Text("Type").font(.caption).foregroundStyle(.secondary)
                        Picker("", selection: $row.audioType) {
                            ForEach(VehicleConfigModel.audioTypes, id: \.self) {
                                Text(AudioLabels.audioType($0)).tag($0)
                            }
                        }.labelsHidden()
                    }
                    GridRow {
                        Text("In (mic)").font(.caption).foregroundStyle(.secondary)
                        CodecPicker(title: "", value: $row.input).labelsHidden()
                    }
                    GridRow {
                        Text("Out").font(.caption).foregroundStyle(.secondary)
                        CodecPicker(title: "", value: $row.output).labelsHidden()
                    }
                }
                HStack {
                    Spacer()
                    Button(role: .destructive) {
                        model.audioFormats.removeAll { $0.id == row.id }
                    } label: { Label("Remove", systemImage: "trash").labelStyle(.iconOnly) }
                    .buttonStyle(.borderless)
                }
            }
        }
        Button {
            model.audioFormats.append(AudioFormatRow())
        } label: { Label("Add format", systemImage: "plus.circle") }
        if model.audioFormats.allSatisfy({ $0.output == "none" }) {
            Text("At least one entry needs an output codec, or the box keeps its default set.")
                .font(.caption).foregroundStyle(.orange)
        }
    }
}

/// A read-only reference of every audio format the box + app support (the vocabulary a custom config
/// draws from). Fulfills the "list the capabilities that can be configured" ask; ● = device-verified.
private struct AudioCapabilitiesReference: View {
    @State private var expanded = false
    var body: some View {
        DisclosureGroup("Supported audio formats", isExpanded: $expanded) {
            VStack(alignment: .leading, spacing: 3) {
                Text("● device-verified · ○ advertisable, not yet on-box proven")
                    .font(.caption2).foregroundStyle(.secondary)
                ForEach(VehicleConfigModel.audioCodecs.filter { $0 != "none" }, id: \.self) { c in
                    let ok = VehicleConfigModel.verifiedCodecs.contains(c)
                    HStack(spacing: 6) {
                        Text(ok ? "●" : "○").foregroundStyle(ok ? Color.green : Color.secondary)
                        Text(AudioLabels.codec(c))
                        Spacer()
                        Text(c).font(.system(.caption2, design: .monospaced)).foregroundStyle(.tertiary)
                    }
                    .font(.caption)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// Per-projection badges for ONE live intent, read from `ControlsBridge`'s own availability table
/// (`isAvailable` / `unavailableReason`) — the runtime counterpart of `FeatureMatrix`, and the only
/// truthful source for a control that acts on the live session rather than on the profile.
///
/// WHY NOT A `FeatureBlock` (finding 4): a `FeatureBlock` renders a `Feature`, and a `Feature` is a
/// unit of the profile DOCUMENT — `features(in:)`, `exclusiveKeys`, the import/export tests all
/// assume so. The live toggles edit no document field; their owner is the Controls router, whose
/// table already says per intent what the active projection can express. So the badges are read
/// from THAT table, through a bridge held in each projection's state, and cannot drift from what
/// the router will actually send — the same idiom `FeatureBadges.swift` cites for the matrix.
private struct LiveControlBadges: View {
    let control: ControlsBridge.Control

    /// `isAvailable` answers for the ACTIVE projection only, so each projection gets a bridge in
    /// that state. Consulted, never sent through: no client or session is ever attached.
    @MainActor private static let probes: [Projection: ControlsBridge] = {
        let carPlay = ControlsBridge(); carPlay.isAndroidAuto = false
        let androidAuto = ControlsBridge(); androidAuto.isAndroidAuto = true
        return [.carPlay: carPlay, .androidAuto: androidAuto]
    }()

    /// `.unsupported` with the router's own reason when it refuses the intent; `.limited` when the
    /// router maps it onto a different lever (`displayAppearance` under Android Auto is routed to
    /// the night_mode sensor by `setDisplayDark` — the `.theme` matrix row's sentence is reused, so
    /// the two places that describe that mapping are one string); `.supported` otherwise.
    static func support(_ c: ControlsBridge.Control, on p: Projection) -> (level: FeatureSupport.Level, help: String) {
        let probe = Self.probes[p]!
        if let why = probe.unavailableReason(c) {
            return (.unsupported, "\(p.displayName): \(why)")
        }
        if c == .displayAppearance, probe.isAndroidAuto {
            return (.limited, "\(p.displayName): " + FeatureMatrix.support(.theme, on: p).effect)
        }
        return (.supported, "\(p.displayName): \(FeatureSupport.Level.supported.word)")
    }

    var body: some View {
        HStack(spacing: 6) {
            ForEach(Projection.allCases, id: \.self) { p in
                let s = Self.support(control, on: p)
                ProjectionBadge(projection: p, level: s.level).help(s.help)
            }
        }
    }
}

/// Live (runtime) display-appearance controls — the Light/Dark and Night Mode toggles the Simulator
/// exposes per display. Each sends through `ControlsBridge` on the live session, and each row is
/// badged from the bridge's own table (`LiveControlBadges`): the main Light/Dark toggles are the
/// `displayAppearance` intent (routed to night_mode under Android Auto), the alt ones the
/// `altDisplay` intent (refused under Android Auto — one display), night mode the `nightMode`
/// intent. Distinct from the stored `theme` in the Appearance section above. The same state drives
/// the inline sun/moon titlebar buttons; a change here moves those and vice-versa.
private struct LiveAppearanceSection: View {
    @ObservedObject var bridge: ControlsBridge

    private func uiBinding(alt: Bool) -> Binding<Bool> {
        Binding(get: { alt ? bridge.altUIDark : bridge.mainUIDark },
                set: { bridge.setUIAppearance(alt: alt, dark: $0) })
    }
    private func mapBinding(alt: Bool) -> Binding<Bool> {
        Binding(get: { alt ? bridge.altMapDark : bridge.mainMapDark },
                set: { bridge.setMapAppearance(alt: alt, dark: $0) })
    }

    private func row(_ title: String, _ control: ControlsBridge.Control, isOn: Binding<Bool>) -> some View {
        Toggle(isOn: isOn) {
            HStack(spacing: 6) {
                Text(title)
                Spacer(minLength: 6)
                LiveControlBadges(control: control)
            }
        }
    }

    var body: some View {
        Section {
            row("Main display — Dark UI", .displayAppearance, isOn: uiBinding(alt: false))
            row("Main display — Dark map", .displayAppearance, isOn: mapBinding(alt: false))
            row("Alt/cluster display — Dark UI", .altDisplay, isOn: uiBinding(alt: true))
            row("Alt/cluster display — Dark map", .altDisplay, isOn: mapBinding(alt: true))
            row("Send night mode now (day/night signal)", .nightMode,
                isOn: Binding(get: { bridge.nightModeOn }, set: { bridge.setNightMode($0) }))
        } header: {
            Text("Display appearance — live session")
        } footer: {
            Text("Acts on the connected phone now, on whichever projection is live (same as the sun/moon in each window's title bar). Needs a session; the choice is remembered and re-sent on reconnect. The badges are the Controls router's own availability per projection — hover one for the reason. The Appearance theme above is the profile's standing choice, not this.")
                .font(.caption)
        }
    }
}

// MARK: - Feature block

/// One neutral feature as a run of Form rows: heading (title + badges + summary) → the neutral
/// control(s) → the per-protocol explanation rows → extras (value notes, exclusive sub-groups). The
/// order is DESIGN.md §1's, and it is fixed here rather than at each call site so no feature can
/// forget its badges.
private struct FeatureBlock<Neutral: View, Extras: View>: View {
    let feature: Feature
    @ViewBuilder let neutral: () -> Neutral
    @ViewBuilder let extras: () -> Extras

    var body: some View {
        FeatureHeading(feature: feature)
        neutral()
        FeatureExplanationRows(feature: feature)
        extras()
    }
}

extension FeatureBlock where Extras == EmptyView {
    init(feature: Feature, @ViewBuilder neutral: @escaping () -> Neutral) {
        self.init(feature: feature, neutral: neutral, extras: { EmptyView() })
    }
}

// MARK: - Sections

private struct IdentitySection: View {
    @ObservedObject var model: VehicleConfigModel

    var body: some View {
        Section(Feature.Section.identity.rawValue) {
            FeatureBlock(feature: .headUnitName) {
                TextField(text: $model.name) { InfoLabel(title: "Name", key: "name") }
            } extras: {
                // exclusiveKeys(.headUnitName, on: .carPlay) == ["accessoryName"]
                ExclusiveSubGroup(feature: .headUnitName, projection: .carPlay) {
                    TextField(text: $model.accessoryName, prompt: Text("CarLink-<box id>")) {
                        InfoLabel(title: "Accessory name", key: "accessoryName")
                    }
                }
            }

            FeatureBlock(feature: .branding) {
                TextField(text: $model.oemIconLabel) { InfoLabel(title: "Maker label", key: "brandingLabel") }
            } extras: {
                // exclusiveKeys(.branding, on: .carPlay) == ["oemIconEnabled", "oemIconVisible", "oemIconBase64"]
                ExclusiveSubGroup(feature: .branding, projection: .carPlay, caption: "OEM icon") {
                    Toggle("Advertise OEM icon config", isOn: $model.oemIconEnabled)
                    if model.oemIconEnabled {
                        HStack(spacing: 12) {
                            if !model.oemIconBase64.isEmpty,
                               let data = Data(base64Encoded: model.oemIconBase64),
                               let img = NSImage(data: data) {
                                Image(nsImage: img).resizable().aspectRatio(contentMode: .fit)
                                    .frame(width: 44, height: 44).cornerRadius(6)
                                Text("\(model.oemIconW)×\(model.oemIconH) PNG").font(.caption).foregroundStyle(.secondary)
                            } else {
                                Text("No image chosen").font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer()
                            Button("Choose PNG…") { model.pickOemIcon() }
                            Button("Use Simulator icon") { model.useSimulatorOemIcon() }
                        }
                        Toggle("Show icon in CarPlay", isOn: $model.oemIconVisible)
                        Text(model.oemIconVisible
                             ? "iOS shows the icon (oemIconVisible: true)."
                             : "iOS hides the icon (oemIconVisible: false is sent). Use this to hide it — turning off \"Advertise OEM icon config\" only stops sending the config, which leaves the last icon on screen.")
                            .font(.caption2).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    }
                    Text("PNG, square (Apple ships 120/180/256). Static config — takes effect on the next connect. The config is emitted only when advertised AND an image is set; the maker label above rides inside it as oemIconLabel.")
                        .font(.caption2).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }
}

/// Defect 4: what the Android Auto renderer will DECLARE for the current geometry — the tier, the
/// visible (margin-cropped) rect, the margins, the codec — plus every approximation it had to make
/// (`AACapability.negotiationNotes`, W2). Computed from the same `profile` / `adapterSettings` the
/// session will be built from, so it cannot drift from the real negotiation; the bench environment
/// overrides (`AA_FORCE_RES`, `AA_PANEL`, …) are honoured here too because the session honours them.
/// Rendered ALWAYS, not only while AA is live: the point is to see the consequence while authoring.
private struct AANegotiationReadout: View {
    @ObservedObject var model: VehicleConfigModel
    @ObservedObject var bridge = ControlsBridge.shared

    var body: some View {
        // `autoThemeIsDark` resolves the profile's `auto` theme the same way the AppDelegate call
        // site does (AACapability+Profile.swift keeps AppKit out for the harness, so the caller
        // supplies it); without it W2 appends a "no resolver" note that would be false here.
        // `warn` is a no-op: every note is already in `negotiationNotes`, and a View body re-renders.
        let cap = AACapability(profile: model.profile, adapter: model.adapterSettings,
                               autoThemeIsDark: NSApp.effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua,
                               warn: { _ in })
        let tier = cap.resolution.size
        let fps = cap.frameRate == .fps60 ? 60 : 30
        let codec = cap.videoCodecHEVC ? "H.265" : "H.264"
        let geometry: String = cap.hasMargins
            ? "visible \(cap.visibleWidth)×\(cap.visibleHeight), margins \(cap.margins.w)×\(cap.margins.h) px — cropped, then scaled to \(model.mainWidth)×\(model.mainHeight)"
            : "no margins (the tier is the panel)"
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                ProjectionBadge(projection: .androidAuto)
                Text("Negotiation preview").font(.caption.weight(.semibold))
                if bridge.isAndroidAuto {
                    Text("· live projection").font(.caption2).foregroundStyle(.secondary)
                }
            }
            Text("Declares \(tier.w)×\(tier.h) @ \(fps) fps, \(codec); \(geometry).")
                .font(.caption).fixedSize(horizontal: false, vertical: true)
            ForEach(Array(cap.negotiationNotes.enumerated()), id: \.offset) { item in
                HStack(alignment: .firstTextBaseline, spacing: 4) {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                    Text(item.element).fixedSize(horizontal: false, vertical: true)
                }
                .font(.caption2)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// What the model's last `clampInPlace()` changed (`VehicleConfigModel.clampNotes`), rendered like
/// the AA negotiation notes: one orange line per moved value, nothing when nothing moved. This is
/// the honesty signal for a clamp — a value the owner (or the control socket) supplied that the
/// model stored differently is named here, next to the field, until the next Save clears it.
private struct ClampNotesReadout: View {
    @ObservedObject var model: VehicleConfigModel
    /// Only the notes that start with this label (e.g. "Panel", "Alt display"); nil = all of them.
    var prefix: String? = nil

    var body: some View {
        let notes = model.clampNotes.filter { prefix.map($0.hasPrefix) ?? true }
        ForEach(Array(notes.enumerated()), id: \.offset) { item in
            HStack(alignment: .firstTextBaseline, spacing: 4) {
                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                Text(item.element).fixedSize(horizontal: false, vertical: true)
            }
            .font(.caption2)
        }
    }
}

private struct DisplaySection: View {
    @ObservedObject var model: VehicleConfigModel

    private var mainIsInset: Bool {
        model.mainSafeLeft > 0 || model.mainSafeTop > 0 || model.mainSafeRight > 0 || model.mainSafeBottom > 0
    }

    /// The panel's implied density from its diagonal, for the owner to compare with `dpi`. Uses the
    /// profile's own arithmetic (`PanelGeometry.impliedDPI`) so the two can never disagree.
    private var impliedDPI: Int? {
        PanelGeometry(width: model.mainWidth, height: model.mainHeight, maxFPS: model.maxFPS,
                      dpi: model.dpi, diagonalInches: model.diagonalInches > 0 ? model.diagonalInches : nil)
            .impliedDPI
    }

    var body: some View {
        Section(Feature.Section.display.rawValue) {
            FeatureBlock(feature: .panelGeometry) {
                ResolutionField(title: "Preset", width: $model.mainWidth, height: $model.mainHeight)
            } extras: {
                ClampNotesReadout(model: model, prefix: "Panel")
                ValueNoteLabel(feature: .panelGeometry, projection: .androidAuto,
                               value: "\(model.mainWidth)x\(model.mainHeight)")
                AANegotiationReadout(model: model)
                // exclusiveKeys(.panelGeometry, on: .androidAuto) == ["aaFitPanelWithMargins"]
                ExclusiveSubGroup(feature: .panelGeometry, projection: .androidAuto) {
                    InfoToggle(title: "Fit a non-tier panel with margins", key: "aaFitPanelWithMargins",
                               isOn: $model.aaFitPanelWithMargins)
                }
            }

            FeatureBlock(feature: .frameRate) {
                FrameRatePicker(title: "Frame rate", fps: $model.maxFPS)
            } extras: {
                ValueNoteLabel(feature: .frameRate, projection: .androidAuto, value: "\(model.maxFPS)")
            }

            FeatureBlock(feature: .pixelDensity) {
                LabeledContent {
                    HStack(spacing: 4) {
                        TextField("160", value: $model.dpi, format: .number)
                            .frame(width: 62).multilineTextAlignment(.trailing)
                        Text("dpi").foregroundStyle(.tertiary).font(.caption)
                    }
                    .textFieldStyle(.roundedBorder)
                } label: { InfoLabel(title: "Density", key: "dpi") }
                LabeledContent {
                    HStack(spacing: 4) {
                        TextField("0", value: $model.diagonalInches, format: .number.precision(.fractionLength(0...1)))
                            .frame(width: 62).multilineTextAlignment(.trailing)
                        Text("in").foregroundStyle(.tertiary).font(.caption)
                    }
                    .textFieldStyle(.roundedBorder)
                } label: { InfoLabel(title: "Diagonal", key: "diagonalInches") }
                if let implied = impliedDPI {
                    let off = abs(implied - model.dpi) > 20
                    let diagonal = model.diagonalInches.formatted(.number.precision(.fractionLength(0...1)))
                    let tail = off
                        ? " — the declared density differs by more than 20; UI will draw \(model.dpi > implied ? "larger" : "smaller") than life-size."
                        : "."
                    Text(verbatim: "\(model.mainWidth)×\(model.mainHeight) over \(diagonal)″ implies about \(implied) dpi" + tail)
                        .font(.caption).foregroundStyle(off ? Color.orange : Color.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }

            FeatureBlock(feature: .insets) {
                SafeAreaField(left: $model.mainSafeLeft, top: $model.mainSafeTop,
                              right: $model.mainSafeRight, bottom: $model.mainSafeBottom,
                              resWidth: model.mainWidth, resHeight: model.mainHeight)
            } extras: {
                // exclusiveKeys(.insets, on: .carPlay) == ["enablesViewAreas", "mainDrawOutsideSafe", "enablesCornerMasks",
                //                                          "viewArea2Enabled", "viewArea2X", "viewArea2Y", "viewArea2W", "viewArea2H"]
                ExclusiveSubGroup(feature: .insets, projection: .carPlay) {
                    InfoToggle(title: "View areas", key: "enablesViewAreas", isOn: $model.enablesViewAreas)
                    InfoToggle(title: "Allow UI outside safe area", key: "drawUIOutsideSafeArea", isOn: $model.mainDrawOutsideSafe)
                        .disabled(!mainIsInset)
                        .opacity(mainIsInset ? 1 : 0.5)
                    InfoToggle(title: "Corner masks (cutout)", key: "enablesCornerMasks", isOn: $model.enablesCornerMasks)
                    // The five viewArea2 keys: toggle + rect + verdict + the two-layout picture (DESIGN.md §11.10).
                    ViewArea2Field(model: model)
                }
            }

            FeatureBlock(feature: .videoCodec) {
                InfoToggle(title: "Allow H.265 (HEVC)", key: "hevcAllowed", isOn: $model.enablesHEVC)
            } extras: {
                // exclusiveKeys(.videoCodec, on: .carPlay) == ["enablesVideoPlayback"]
                ExclusiveSubGroup(feature: .videoCodec, projection: .carPlay) {
                    InfoToggle(title: "Video playback", key: "enablesVideoPlayback", isOn: $model.enablesVideoPlayback)
                }
                // exclusiveKeys(.videoCodec, on: .androidAuto) == ["aaPreferHEVC"]
                ExclusiveSubGroup(feature: .videoCodec, projection: .androidAuto) {
                    InfoToggle(title: "Prefer HEVC at and below 1080p", key: "aaPreferHEVC", isOn: $model.aaPreferHEVC)
                        .disabled(!model.enablesHEVC)
                        .opacity(model.enablesHEVC ? 1 : 0.5)
                }
            }

            FeatureBlock(feature: .altDisplay) {
                Toggle(isOn: $model.altVideoEnabled) { InfoLabel(title: "Enable second display", key: "altDisplay") }
                if model.altVideoEnabled {
                    ResolutionField(title: "Preset", width: $model.altWidth, height: $model.altHeight, infoKey: "altResolution")
                    FrameRatePicker(title: "Frame rate", fps: $model.altFPS)
                }
            } extras: {
                ClampNotesReadout(model: model, prefix: "Alt display")
                if model.altVideoEnabled {
                    // exclusiveKeys(.altDisplay, on: .carPlay) == ["altSafeLeft", "altSafeTop", "altSafeRight",
                    // "altSafeBottom", "altDrawOutsideSafe"]. `altDrawOutsideSafe` is listed but NOT
                    // rendered: drawUIOutsideSafeArea is a main-display-only flag (WWDC 2023-10150),
                    // the emitter never wrote it for the alt stream, and the Phase 0 form hid it
                    // (`allowsDrawOutside: false`). Kept off-screen rather than offered and ignored.
                    ExclusiveSubGroup(feature: .altDisplay, projection: .carPlay, caption: "stream insets") {
                        SafeAreaField(left: $model.altSafeLeft, top: $model.altSafeTop,
                                      right: $model.altSafeRight, bottom: $model.altSafeBottom,
                                      resWidth: model.altWidth, resHeight: model.altHeight,
                                      infoKey: "safeArea")
                    }
                }
            }
        }
    }
}

private struct AppearanceSection: View {
    @ObservedObject var model: VehicleConfigModel

    /// `theme` is the source; the legacy `nightMode` Bool is kept in step ONLY so a downgraded
    /// build that knows just `vc.nightMode` reads a sane value (DESIGN.md §5). Since 2026-09-04 no
    /// code reads it as an input — the AA renderer is built from the profile and takes `theme`
    /// directly — so this is a write-only downgrade breadcrumb, not a second source of truth. `auto` maps to legacy false: the Bool cannot express "follow the Mac", which
    /// is resolved at the AppDelegate call site via NSApp.effectiveAppearance.
    private var theme: Binding<AppearanceTheme> {
        Binding(get: { AppearanceTheme(rawValue: model.theme) ?? .light },
                set: { model.theme = $0.rawValue; model.nightMode = ($0 == .dark) })
    }

    private static func label(_ t: AppearanceTheme) -> String {
        switch t {
        case .auto:  return "Auto (this Mac)"
        case .light: return "Light"
        case .dark:  return "Dark"
        }
    }

    var body: some View {
        Section(Feature.Section.appearance.rawValue) {
            FeatureBlock(feature: .theme) {
                Picker(selection: theme) {
                    ForEach(AppearanceTheme.allCases, id: \.self) { Text(Self.label($0)).tag($0) }
                } label: { InfoLabel(title: "Theme", key: "theme") }
                .pickerStyle(.segmented)
            } extras: {
                // exclusiveKeys(.theme, on: .carPlay) == ["enablesUIAppearance", "enablesMapAppearance"]
                ExclusiveSubGroup(feature: .theme, projection: .carPlay) {
                    InfoToggle(title: "UI appearance sync", key: "enablesUIAppearance", isOn: $model.enablesUIAppearance)
                    InfoToggle(title: "Map appearance sync", key: "enablesMapAppearance", isOn: $model.enablesMapAppearance)
                }
            }

            FeatureBlock(feature: .statusBar) {
                InfoLabel(title: "Elements the car draws itself", key: "statusBar")
                Toggle("Hide clock", isOn: $model.hideClock)
                Toggle("Hide signal strength", isOn: $model.hideSignal)
                Toggle("Hide battery", isOn: $model.hideBattery)
            }
        }
    }
}

/// One neutral restriction as a row: its title, the switch, and a mini badge per projection saying
/// whether that vendor has an element/bit for it (`FeatureMatrix.restrictionMapping`) — so "Long
/// media lists" visibly does nothing for Android Auto and "Video" visibly does nothing for CarPlay,
/// without a sentence anyone had to write.
private struct RestrictionRow: View {
    let mapping: FeatureMatrix.RestrictionMapping
    @Binding var isOn: Bool

    var body: some View {
        Toggle(isOn: $isOn) {
            HStack(spacing: 6) {
                Text(mapping.title)
                Spacer(minLength: 6)
                ProjectionBadge(projection: .carPlay,
                                level: mapping.carPlayElement != nil ? .supported : .unsupported)
                    .help(mapping.carPlayElement.map { "limitedUIConfig.\($0)" } ?? "CarPlay has no limited-UI element for this")
                ProjectionBadge(projection: .androidAuto,
                                level: mapping.androidAutoBit != nil ? .supported : .unsupported)
                    .help(mapping.androidAutoBit.map { "driving_status bit \($0)" } ?? "Android Auto has no driving_status bit for this")
            }
        }
    }
}

private struct DrivingSection: View {
    @ObservedObject var model: VehicleConfigModel

    /// Same downgrade breadcrumb as `theme` → `nightMode`: keep the legacy `rightHandDrive` in step
    /// with the source field purely so an older build reads a sane value. Nothing consumes it as an
    /// input since 2026-09-04.
    /// `center` maps to legacy false (the Bool cannot say centre; the AA renderer declares wire 3).
    private var position: Binding<DriverPosition> {
        Binding(get: { DriverPosition(rawValue: model.driverPosition) ?? .left },
                set: { model.driverPosition = $0.rawValue; model.rightHandDrive = ($0 == .right) })
    }

    private static let positions: [DriverPosition] = [.left, .center, .right]
    private static func label(_ p: DriverPosition) -> String {
        switch p {
        case .left:   return "Left"
        case .center: return "Centre"
        case .right:  return "Right"
        }
    }

    /// The model field behind each neutral member. The Apple five live on the `limitedUI*` fields
    /// the YAML emitter reads (so its inputs are untouched, DESIGN.md §8); the three members with no
    /// CarPlay element live on the Phase 0 `restrict*` fields. The mapping itself is
    /// `FeatureMatrix.restrictionMapping`; this is only which `@Published` stores each bit.
    private func binding(for r: DrivingRestrictionSet) -> Binding<Bool> {
        if r == .keyboard      { return $model.limitedUISoftKeyboard }
        if r == .phoneKeypad   { return $model.limitedUISoftPhoneKeypad }
        if r == .mediaLists    { return $model.limitedUIMusicLists }
        if r == .otherLists    { return $model.limitedUINonMusicLists }
        if r == .longMessages  { return $model.limitedUILongAlerts }
        if r == .video         { return $model.restrictVideo }
        if r == .voiceInput    { return $model.restrictVoiceInput }
        if r == .configuration { return $model.restrictConfiguration }
        // A member the matrix knows and this view does not: surface it inert rather than crash.
        // Adding a case to `DrivingRestrictionSet` means adding a field here in the same commit.
        return .constant(false)
    }

    var body: some View {
        Section(Feature.Section.driving.rawValue) {
            FeatureBlock(feature: .driverPosition) {
                Picker(selection: position) {
                    ForEach(Self.positions, id: \.self) { Text(Self.label($0)).tag($0) }
                } label: { InfoLabel(title: "Driver sits", key: "driverPosition") }
                .pickerStyle(.segmented)
            } extras: {
                ValueNoteLabel(feature: .driverPosition, projection: .androidAuto, value: model.driverPosition)
            }

            FeatureBlock(feature: .drivingRestrictions) {
                InfoToggle(title: "Declare restrictions", key: "restrictionsDeclared", isOn: $model.limitedUIConfigEnabled)
                if model.limitedUIConfigEnabled {
                    InfoLabel(title: "While the car is moving, withhold:", key: "restrictions")
                    ForEach(FeatureMatrix.restrictionMapping, id: \.restriction) { m in
                        RestrictionRow(mapping: m, isOn: binding(for: m.restriction))
                    }
                    Text("Switched on and off at runtime in Window ▸ Controls ▸ UI; this is the declaration.")
                        .font(.caption2).foregroundStyle(.secondary)
                }
            } extras: {
                if model.limitedUIConfigEnabled {
                    // exclusiveKeys(.drivingRestrictions, on: .carPlay) == ["limitedUIJapanMaps",
                    // "limitedUIPairedDevices", "limitedUIThemeCustomization", "limitedUIAutomakerSettings",
                    // "limitedUIAutomakerSettingsInfoButton"]
                    ExclusiveSubGroup(feature: .drivingRestrictions, projection: .carPlay, caption: "limitedUIConfig extras") {
                        Toggle("Japan maps", isOn: $model.limitedUIJapanMaps)
                        // NOT capability toggles: real Apple LimitedUIConfig keys that airPlayElements
                        // never emits — kept only so exported YAML round-trips the full Apple schema.
                        Text("The four below are parsed for YAML round-trip only — Apple never emits them, so they NEVER appear in /info limitedUIElements.")
                            .font(.caption).foregroundStyle(.orange).fixedSize(horizontal: false, vertical: true)
                        Toggle("Paired devices (round-trip only)", isOn: $model.limitedUIPairedDevices)
                        Toggle("Theme customization (round-trip only)", isOn: $model.limitedUIThemeCustomization)
                        Toggle("Automaker settings (round-trip only)", isOn: $model.limitedUIAutomakerSettings)
                        Toggle("Automaker settings info button (round-trip only)", isOn: $model.limitedUIAutomakerSettingsInfoButton)
                    }
                }
            }
        }
    }
}

private struct VehicleSection: View {
    @ObservedObject var model: VehicleConfigModel

    var body: some View {
        Section(Feature.Section.vehicle.rawValue) {
            // Everything here is absent-off: leave it untouched and the adapter presents exactly the
            // identity it presented before this panel existed (docs/carplay/04_CAPABILITIES_AND_CONFIG.md C6/C7).
            FeatureBlock(feature: .powertrain) {
                InfoLabel(title: "Engine type", key: "engineTypesNeutral")
                ForEach(VehicleConfigModel.engineTypeNames, id: \.self) { e in
                    Toggle(VehicleConfigModel.engineDisplayNames[e] ?? e, isOn: Binding(
                        get: { model.engineTypes.contains(e) },
                        set: { on in
                            if on { model.engineTypes.insert(e) } else { model.engineTypes.remove(e) }
                        }))
                }
                if model.engineTypes.contains("electric") || !model.chargingConnectors.isEmpty {
                    InfoLabel(title: "Charging connectors", key: "chargingConnectorsNeutral")
                    ForEach($model.chargingConnectors) { $row in
                        HStack {
                            Picker("", selection: $row.type) {
                                ForEach(VehicleConfigModel.connectorNames, id: \.self) { Text($0).tag($0) }
                            }.labelsHidden().frame(width: 110)
                            TextField("kW", value: Binding(
                                get: { (row.powerWatts ?? 0) / 1000 },
                                // CLAMP BEFORE THE MULTIPLY: UInt32 overflow traps, and any entry
                                // above 4,294,967 kW would crash the app on a stray keystroke.
                                // 1,000 kW is already far beyond any production charger.
                                set: { row.powerWatts = $0 > 0 ? min($0, 1_000) * 1000 : nil }
                            ), format: .number).frame(width: 60)
                            Text("kW").foregroundStyle(.secondary)
                            Button(role: .destructive) {
                                model.chargingConnectors.removeAll { $0.id == row.id }
                            } label: { Image(systemName: "minus.circle") }.buttonStyle(.borderless)
                        }
                    }
                    Button("Add connector") {
                        // Offer a type not already used: Apple's per-connector power sub is
                        // single-valued, so a duplicate row cannot be represented on the wire and
                        // the adapter would drop it.
                        let used = Set(model.chargingConnectors.map(\.type))
                        let next = VehicleConfigModel.connectorNames.first { !used.contains($0) }
                        if let next { model.chargingConnectors.append(.init(type: next, powerWatts: nil)) }
                    }
                    .disabled(model.chargingConnectors.count >= VehicleConfigModel.connectorNames.count)
                }
            } extras: {
                // exclusiveKeys(.powertrain, on: .carPlay) == ["vehicleStatusEnabled", "vehicleStatusCaps"]
                ExclusiveSubGroup(feature: .powertrain, projection: .carPlay, caption: "vehicle status (C-4 gated)") {
                    InfoToggle(title: "Declare vehicle status", key: "vehicleStatusEnabled", isOn: $model.vehicleStatusEnabled)
                        .disabled(!VehicleConfigModel.vehicleStatusUnlocked)
                    if model.vehicleStatusEnabled {
                        Text("⚠️ Not yet supported by the adapter — the messages that service this component are not declared, and iOS can reject the whole identification for the connection. Leave off until the adapter ships it.")
                            .font(.caption).foregroundStyle(.orange).fixedSize(horizontal: false, vertical: true)
                        ForEach(VehicleConfigModel.vehicleStatusCapNames, id: \.self) { c in
                            Toggle(c, isOn: Binding(
                                get: { model.vehicleStatusCaps.contains(c) },
                                set: { on in
                                    if on { model.vehicleStatusCaps.insert(c) } else { model.vehicleStatusCaps.remove(c) }
                                }))
                            // Apple: the unified range warning and the per-engine ones are mutually
                            // exclusive, so show which selections are being ignored rather than
                            // silently dropping them at emission.
                            .foregroundStyle(
                                model.vehicleStatusCaps.contains("rangeWarning")
                                    && VehicleConfigModel.perEngineRangeWarnings.contains(c)
                                    ? AnyShapeStyle(.secondary) : AnyShapeStyle(.primary))
                        }
                        if model.vehicleStatusCaps.contains("rangeWarning")
                            && !model.vehicleStatusCaps.isDisjoint(with: VehicleConfigModel.perEngineRangeWarnings) {
                            Text("Apple's spec forbids combining the unified range warning with the per-engine ones — the greyed entries will not be sent.")
                                .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                        }
                    }
                }
            }
        }
    }
}

private struct InputSection: View {
    @ObservedObject var model: VehicleConfigModel

    /// The neutral primary input. READ from the profile derivation itself
    /// (`VehicleConfigModel+Profile.swift`, DESIGN.md §8) — the very value `AACapability(profile:)`
    /// is built from — so the picker cannot show one thing while the Android Auto renderer is
    /// handed another (audit finding 1: the old inline copy of the rule drifted the moment the
    /// CarPlay sub-group's touch toggle was flipped).
    ///
    /// WRITE makes the chosen surface present. "Touchpad" ALSO turns the touchscreen off, and that
    /// is the one side effect here: the model stores no primary of its own (Apple's vocabulary has
    /// none — every Simulator template with a touchscreen says `primaryInput: Touchpad`), so the
    /// derivation can only read "touchpad" while no touchscreen is declared. A touchpad that is
    /// primary NEXT TO a touchscreen needs a persisted key the model does not have (not this file's
    /// to add; a profile document carrying that combination is flattened to Touchscreen on import —
    /// the documented lossy spot in VehicleConfigModel+Profile.swift). The side effect is visible —
    /// the neutral Touchscreen toggle one row down flips with it — and stated in the (i).
    ///
    /// Emitted CarPlay bytes for the reachable states are exactly the Phase 0 form's: Touchscreen →
    /// `primaryInput: Touchpad` + `touchScreenMode: High Fidelty` (touchpadSupport untouched, so the
    /// Apple-template state "Touchpad with a high-fidelity touchscreen" IS this one); Touchpad →
    /// `primaryInput: Touchpad` + `touchpadSupport: true` + `touchScreenMode: Disabled`; Rotary →
    /// `primaryInput: Knobs` + `knobSupport: true`. No new persisted key.
    private var primary: Binding<PrimaryInput> {
        Binding(
            get: { model.profile.input.primary },
            set: { p in
                switch p {
                case .rotary:
                    model.primaryInput = "Knobs"
                    model.knobSupport = true
                case .touchpad:
                    model.primaryInput = "Touchpad"
                    model.touchpadSupport = true
                    model.touchScreenHighFidelity = false
                case .touchscreen:
                    model.primaryInput = "Touchpad"
                    model.touchScreenHighFidelity = true
                }
            })
    }

    private static func label(_ p: PrimaryInput) -> String {
        switch p {
        case .touchscreen: return "Touchscreen"
        case .touchpad:    return "Touchpad"
        case .rotary:      return "Rotary knob"
        }
    }

    /// The CarPlay YAML keys behind the neutral input rows, for the "not yet read by the adapter"
    /// caption inside the CarPlay sub-group. Membership comes from `FieldInfo.isInert`, so this
    /// caption and the ⚠️ markers are one list. The keys have no (i) text of their own any more —
    /// the neutral `input*` entries on the rows above are the descriptions (finding 2).
    private static let hidKeys: [(key: String, title: String)] = [
        ("primaryInput", "primary input"), ("touchScreenHighFidelity", "touchscreen (touchScreenMode)"),
        ("dPadSupport", "D-pad"), ("mediaButtonsSupport", "media buttons"),
        ("telephonyButtonsSupport", "telephony buttons"), ("knobSupport", "knob"),
        ("knobSupportsHomeAndBackButton", "knob Home/Back"), ("knobSupportsNudge", "knob nudge"),
        ("touchpadSupport", "touchpad"), ("touchpadButtonsSupport", "touchpad buttons"),
        ("touchScreenSupportsCancel", "touch cancel"), ("touchScreenSupportsMultiTouch", "multi-touch"),
        ("steeringWheelSupport", "steering-wheel buttons"),
    ]
    private var inertHIDTitles: [String] { Self.hidKeys.filter { FieldInfo.isInert($0.key) }.map(\.title) }

    var body: some View {
        Section(Feature.Section.input.rawValue) {
            FeatureBlock(feature: .inputDevices) {
                Picker(selection: primary) {
                    ForEach(PrimaryInput.allCases, id: \.self) { Text(Self.label($0)).tag($0) }
                } label: { InfoLabel(title: "Primary input", key: "primaryInputDevice") }
                .pickerStyle(.segmented)

                // NEUTRAL, not CarPlay-exclusive (2026-09-04, finding 1): `touchScreenHighFidelity`
                // is the model's only "a touchscreen exists" switch — the profile derives
                // `input.touchscreen` presence from it and `AACapability+Profile` gates the Android
                // Auto touchscreen declaration on that presence — so it lives beside the other
                // presence facts, where flipping it is visibly a change to BOTH projections. The
                // placement table (`Feature.exclusiveKeys(.inputDevices, on: .carPlay)`, FeatureMatrix.swift)
                // and tests/SettingsTests.swift:455 still list it as CarPlay-exclusive; both should be
                // emptied to match this rendering (neither file is this tab's to edit).
                InfoToggle(title: "Touchscreen", key: "inputTouchscreen", isOn: $model.touchScreenHighFidelity)
                if model.touchScreenHighFidelity {
                    InfoToggle(title: "Touchscreen: multi-touch", key: "inputTouchMultiTouch", isOn: $model.touchScreenSupportsMultiTouch)
                    InfoToggle(title: "Touchscreen: cancel", key: "inputTouchCancel", isOn: $model.touchScreenSupportsCancel)
                }
                InfoToggle(title: "Touchpad", key: "inputTouchpad", isOn: $model.touchpadSupport)
                if model.touchpadSupport {
                    InfoToggle(title: "Touchpad buttons", key: "inputTouchpadButtons", isOn: $model.touchpadButtonsSupport)
                }
                InfoToggle(title: "Rotary knob", key: "inputKnob", isOn: $model.knobSupport)
                if model.knobSupport {
                    InfoToggle(title: "Knob Home/Back buttons", key: "inputKnobHomeBack", isOn: $model.knobSupportsHomeAndBackButton)
                    InfoToggle(title: "Knob nudge (4-way)", key: "inputKnobNudge", isOn: $model.knobSupportsNudge)
                }
                InfoToggle(title: "D-pad", key: "inputDPad", isOn: $model.dPadSupport)
                InfoToggle(title: "Media buttons", key: "inputMediaButtons", isOn: $model.mediaButtonsSupport)
                InfoToggle(title: "Telephony buttons", key: "inputTelephonyButtons", isOn: $model.telephonyButtonsSupport)
                InfoToggle(title: "Steering-wheel buttons", key: "inputSteeringWheel", isOn: $model.steeringWheelSupport)
            } extras: {
                // exclusiveKeys(.inputDevices, on: .carPlay) lists `touchScreenHighFidelity`; it is
                // rendered as the neutral Touchscreen toggle above (see the comment there), so this
                // sub-group carries only the CarPlay statement no neutral row can make: which of the
                // pushed HID keys the adapter does not read yet (`FieldInfo.isInert`). This is why
                // the CarPlay badge on this block is not "supported" as authored — most of what the
                // rows above set rides the config without a CarPlay effect today.
                if !inertHIDTitles.isEmpty {
                    ExclusiveSubGroup(feature: .inputDevices, projection: .carPlay, caption: "hidConfig") {
                        Text("Pushed but not yet read by the adapter (rides the config with no CarPlay effect): "
                             + inertHIDTitles.joined(separator: ", ") + ".")
                            .font(.caption2).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
        }
    }
}

private struct AudioSection: View {
    @ObservedObject var model: VehicleConfigModel

    /// The rates the AA voice sink lever accepts (`AACapability.voiceSinkRate` validation).
    private static let voiceRates = [16000, 24000, 48000]

    var body: some View {
        Section(Feature.Section.audio.rawValue) {
            FeatureBlock(feature: .audioProfile) {
                Picker(selection: $model.voiceRateHz) {
                    ForEach(Self.voiceRates, id: \.self) { Text("\($0 / 1000) kHz").tag($0) }
                } label: { InfoLabel(title: "Voice rate", key: "voiceRateHz") }
                .pickerStyle(.segmented)
                InfoToggle(title: "Calls over the projection link", key: "telephonyOverProjection", isOn: $model.telephonyOverProjection)
            } extras: {
                ValueNoteLabel(feature: .audioProfile, projection: .androidAuto, value: "\(model.voiceRateHz)")
                // exclusiveKeys(.audioProfile, on: .carPlay) == ["audioMode", "audioFormats",
                // "enablesMainBufferedAudio", "enablesEnhancedSiri"]
                ExclusiveSubGroup(feature: .audioProfile, projection: .carPlay, caption: "advertised format table") {
                    Picker(selection: $model.audioMode) {
                        Text("Auto — match transport").tag("auto")
                        Text("Wired — PCM").tag("wired_pcm")
                        Text("Wireless — AAC (full 8)").tag("wireless_8")
                        Text("Custom…").tag("custom")
                    } label: { InfoLabel(title: "Audio formats", key: "audioFormats") }

                    if model.audioMode == "custom" {
                        InfoLabel(title: "Custom advertised set", key: "audioFormatRow")
                        AudioFormatsEditor(model: model)
                    } else {
                        Text(AudioLabels.modeSummary(model.audioMode))
                            .font(.caption).foregroundStyle(.secondary)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }

                    AudioCapabilitiesReference()

                    InfoToggle(title: "Main buffered audio (media)", key: "enablesMainBufferedAudio", isOn: $model.enablesMainBufferedAudio)
                    InfoToggle(title: "Enhanced Siri", key: "enablesEnhancedSiri", isOn: $model.enablesEnhancedSiri)
                }
            }
        }
    }
}

private struct DataFeedsSection: View {
    @ObservedObject var model: VehicleConfigModel

    var body: some View {
        Section(Feature.Section.dataFeeds.rawValue) {
            FeatureBlock(feature: .metadataFeeds) {
                InfoLabel(title: "Feeds the head unit consumes", key: "metadataFeeds")
                Toggle("Now playing", isOn: $model.metadataNowPlaying)
                Toggle("Navigation (turn-by-turn)", isOn: $model.metadataNavigation)
                Toggle("Telephony (call state)", isOn: $model.metadataTelephony)
            } extras: {
                ValueNoteLabel(feature: .metadataFeeds, projection: .carPlay, value: model.metadataTier)
                // exclusiveKeys(.metadataFeeds, on: .carPlay) == ["metadataTier", "metadataSkip",
                // "enablesFocusTransfer", "enablesUIContext", "enablesUISync", "enablesFileTransfer",
                // "enablesLogTransfer", "enablesVehicleDataProtocol", "enablesDCX"]
                ExclusiveSubGroup(feature: .metadataFeeds, projection: .carPlay, caption: "iAP2 declaration + transfer flags") {
                    Picker(selection: $model.metadataTier) {
                        Text("Proven — device-accepted baseline").tag("proven")
                        Text("Extended — full paired Start/Stop set").tag("extended")
                        Text("All — every capability in the table").tag("all")
                    } label: { InfoLabel(title: "Metadata declaration", key: "metadataTier") }
                    HStack {
                        InfoLabel(title: "Skip features", key: "metadataSkip")
                        TextField("e.g. call_history", text: $model.metadataSkip)
                            .textFieldStyle(.roundedBorder)
                    }
                    InfoToggle(title: "Focus transfer", key: "enablesFocusTransfer", isOn: $model.enablesFocusTransfer)
                    InfoToggle(title: "UI context handoff", key: "enablesUIContext", isOn: $model.enablesUIContext)
                    InfoToggle(title: "UI sync", key: "enablesUISync", isOn: $model.enablesUISync)
                    InfoToggle(title: "File transfer", key: "enablesFileTransfer", isOn: $model.enablesFileTransfer)
                    InfoToggle(title: "Log transfer", key: "enablesLogTransfer", isOn: $model.enablesLogTransfer)
                    InfoToggle(title: "Vehicle data protocol", key: "enablesVehicleDataProtocol", isOn: $model.enablesVehicleDataProtocol)
                    InfoToggle(title: "DCX", key: "enablesDCX", isOn: $model.enablesDCX)
                }
            }
        }
    }
}

// MARK: - The tab

/// The Vehicle tab, one of the two tabs `SettingsRootView` (App/SettingsWindow.swift) hosts. The
/// former `ConfigurationTab` name and its alias are gone (2026-09-04).
struct VehicleTab: View {
    @ObservedObject var model = VehicleConfigModel.shared
    @State private var showYAML = false
    @State private var confirmReset = false

    var body: some View {
        Form {
            IdentitySection(model: model)
            DisplaySection(model: model)
            AppearanceSection(model: model)
            LiveAppearanceSection(bridge: ControlsBridge.shared)
            DrivingSection(model: model)
            VehicleSection(model: model)
            InputSection(model: model)
            AudioSection(model: model)
            DataFeedsSection(model: model)

            // The document pushed to the ADAPTER (DESIGN.md §0 decision 1: the profile is the source,
            // this YAML is a rendered artifact). It is NOT CarPlay-only (finding 5): the Apple-schema
            // part is the CarPlay rendering of the profile above, but the same document carries the
            // Adapter tab's keys (`wireless`, `hot_handover`, `pairing`, `android_auto`, `wifi_ap`,
            // `appDrivenSetup`) that gate Android Auto too — so both badges, and the caption says what is and is not
            // in it. The neutral document is the block below it.
            Section {
                HStack(spacing: 6) {
                    ProjectionBadge(projection: .carPlay)
                    ProjectionBadge(projection: .androidAuto)
                    Text("Pushed adapter document").font(.subheadline.weight(.semibold))
                }
                Text("The YAML this app pushes to the adapter at SUBSCRIBE. Two things in one document: the CarPlay rendering of the profile above (Apple's VehicleConfig schema — displays, hidConfig, accessoryConfig, audio, iAP2 metadata), and the Adapter tab's settings (wireless, hot_handover, pairing, android_auto, wifi_ap, appDrivenSetup), which gate both projections. Android Auto's own declaration is not in here — it is built in-app from the same profile and previewed under Panel resolution. Read-only; edit the profile or the Adapter tab, not the YAML.")
                    .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                DisclosureGroup("Generated YAML", isExpanded: $showYAML) {
                    Text(model.yaml)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                Button("Export YAML…", systemImage: "square.and.arrow.up") { exportYAML() }
            }

            // The neutral document: Import / Export / Presets (W4's view; DESIGN.md §1).
            Section("Profile document") {
                ProfileDocumentView(model: model)
            }

            Section {
                Button("Reset to defaults", systemImage: "arrow.uturn.backward", role: .destructive) {
                    confirmReset = true
                }
            }
        }
        .formStyle(.grouped)
        .safeAreaInset(edge: .bottom) {
            // Save bar — makes it explicit what's committed and when it applies. Neutral wording
            // (defect 7): a session of EITHER projection defers the push, and which protocol does
            // what with a value is said by the rows above, never here.
            HStack(spacing: 10) {
                if model.dirty {
                    Label("Unsaved changes", systemImage: "pencil.circle.fill").foregroundStyle(.orange)
                } else {
                    Label("Saved — pushed to the adapter now (deferred while a session is live)", systemImage: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                }
                Spacer()
                Button("Save") { model.save() }
                    .keyboardShortcut("s", modifiers: .command)
                    .buttonStyle(.borderedProminent)
                    .disabled(!model.dirty)
            }
            .font(.callout)
            .padding(12)
            .background(.bar)
        }
        .confirmationDialog("Reset every vehicle and adapter setting to the defaults?", isPresented: $confirmReset) {
            Button("Reset to defaults", role: .destructive) { model.resetToDefault() }
            Button("Cancel", role: .cancel) {}
        }
    }

    private func exportYAML() {
        let panel = NSSavePanel()
        panel.title = "Export VehicleConfig YAML"
        panel.nameFieldStringValue = "carlink_vehicleconfig.yaml"
        panel.allowedContentTypes = [.yaml]
        panel.canCreateDirectories = true
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            // Export the LIVE yaml — the same document the "Generated YAML" preview shows. (The
            // committed snapshot is what's pushed to the box, but exporting something other than
            // what's on screen was silently dishonest.)
            do {
                try Data(model.yaml.utf8).write(to: url)
            } catch {
                let alert = NSAlert()
                alert.messageText = "Export Failed"
                alert.informativeText = error.localizedDescription
                alert.alertStyle = .warning
                alert.runModal()
            }
        }
    }
}
