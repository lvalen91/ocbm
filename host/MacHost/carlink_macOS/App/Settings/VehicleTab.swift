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
//   1. the badge STRIP — one `ProjectionBadge` per projection with its support level
//      (`FeatureMatrix.supports(f)`), each one opening the SAME consolidated `FieldPopover`
//      (DESIGN.md §11.3) the (i) opens: description, both `FeatureSupport.effect`s, provenance,
//      and the value-level note for the value the control currently holds;
//   2. the vendor-EXCLUSIVE sub-groups, badged, INSIDE the feature they belong to — never on a
//      protocol tab (DESIGN.md §0 decision 4; `Feature.exclusiveKeys(on:)` is the placement table
//      and every `GateReveal` below names the (feature, projection) it implements). A control
//      whose OWN value computes the gate cannot sit inside the reveal it opens (`GateReveal.isOn`
//      is read-only by design), so it stays on its own row and carries the projection's badge
//      THERE, via `ExclusiveControlLabel` — never as a bare row under a badge that labels the
//      hidden group instead of the control.
//
// `Feature.summary`, `ValueNoteLabel` and `FeatureExplanationRows` are no longer rows of ANY kind
// here (DESIGN.md §1 Phase 3, then §11.4's TEN-WORD RULE, 2026-09-08): all three are popover
// content, the value note reached through the `currentValue` each `FeatureBlock` threads into its
// badge strip. THE TEN-WORD RULE — no explanatory text over ten words renders inline; it goes
// behind the (i)/badge popover, gets condensed, or the control is simplified until it needs less.
// It is stricter than the HIG and overrides any looser reading elsewhere. The exemptions are
// §11.4's own: the six live-computed warnings, the ⚠️ inert marker, `PanelRule.verdict` /
// `viewArea2Verdict`, the preview captions and the confirmation dialog — a judgement the reader
// must act on now is not "explanatory text", and hiding it behind a hover is the defect §11.4
// exists to prevent. Everything else added to this file is measured against ten words.
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
            .textFieldStyle(.bordered)
            .textInputBorderShape(.roundedRectangle)
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

/// The ONE sizing, COLOUR and caption convention for the to-scale display picture(s) in this tab.
/// A panel of the resolution's aspect fitted into 260×132 points, one designated colour per region,
/// and a swatch-plus-LABEL legend row. The label is not decoration: meaning is never carried by hue
/// alone here, because colour-only encoding fails for colour-blind readers and under Increase
/// Contrast, where fills flatten toward the same grey.
///
/// **The palette is deliberately NOT `Color.accentColor`**, which the two previous previews used for
/// BOTH the panel and the safe area. The accent is user-chosen in System Settings, so a Red accent
/// drew the safe area in the colour this file reserves for "illegal", and Graphite drew it in the
/// panel's own grey. These are the SYSTEM semantic colours instead: each resolves to a different RGB
/// in light and in dark appearance, so every region keeps its contrast under the app's live
/// light/dark toggle (`LiveAppearanceSection`) without a second palette.
///
///   * panel        — neutral grey (`.secondary`): the whole display; the ground the rest sits on.
///   * safe area    — blue: where CarPlay keeps interactive UI (WWDC 2019-252).
///   * resize area  — green when the model returns no verdict, red + DASHED when it does.
///
/// Green, not the previous orange, for the legal resize area: the pair that must be told apart is
/// the legal and illegal state of the SAME region, and orange-against-red is exactly the pair
/// red-green colour blindness confuses worst. Grey / blue / green stay separable under both common
/// CVD types, and the illegal state additionally switches the stroke to dashed and the legend text
/// to "not pushed" — three channels, so the verdict never rides on hue.
private enum PreviewCanvas {
    static let maxW: CGFloat = 260, maxH: CGFloat = 132

    // One region, one designated colour. Fills are translucent so a region nested inside another
    // still shows what it sits on; strokes are full-strength so the boundary survives Increase
    // Contrast and a printed screenshot.
    static let panelFill = Color.secondary.opacity(0.10)
    static let panelStroke = Color.secondary.opacity(0.55)
    static let safeFill = Color.blue.opacity(0.22)
    static let safeStroke = Color.blue
    static let areaFill = Color.green.opacity(0.28)
    static let areaStroke = Color.green
    static let illegalFill = Color.red.opacity(0.16)
    static let illegalStroke = Color.red

    /// The on-screen canvas for the display box — the resolution's aspect, scaled to fit maxW×maxH.
    static func size(resW: Int, resH: Int) -> CGSize {
        guard resW > 0, resH > 0 else { return CGSize(width: maxW, height: maxW * 9 / 16) }
        let a = CGFloat(resW) / CGFloat(resH)
        var w = maxW, h = maxW / a
        if h > maxH { h = maxH; w = maxH * a }
        return CGSize(width: w.rounded(), height: h.rounded())
    }

    /// A legend entry: the region's own fill and stroke, and its NAME. Both are always drawn — the
    /// swatch alone would be a colour-only encoding.
    static func legend(fill: Color, stroke: Color, _ text: String, dashed: Bool = false) -> some View {
        HStack(spacing: 4) {
            RoundedRectangle(cornerRadius: 2).fill(fill)
                .frame(width: 12, height: 10)
                .overlay(RoundedRectangle(cornerRadius: 2)
                    .strokeBorder(stroke, style: StrokeStyle(lineWidth: 1, dash: dashed ? [2, 2] : [])))
            Text(text)
        }
    }
}

/// **The one picture** (owner, 2026-09-09: "The 'Display' section is too clustered together… The use
/// of boxes to visualize to user what the parameters would view itself as. Each should have a
/// designated but appropriate color. Within one Main Box which shows the Safe/View Area and resize
/// area."). Every region of one video stream, to scale, in a SINGLE canvas:
///
///   * the panel — the full display, the outer boundary;
///   * the safe area — the main insets, drawn inside it;
///   * the resize area — the `viewArea2*` rect, when enabled (`area != nil`).
///
/// This REPLACES the two separate drawings the section used to carry — `SafeAreaPreview` (panel +
/// safe box) and `ViewAreasPreview` (area [0] + area [1]) — which lived in two different places and
/// drew the panel and the safe area twice between them. That duplication was a large part of the
/// clutter the owner named. The alt/cluster stream draws the SAME view with `area: nil`, so main and
/// alt remain one visual language rather than two (the reason `PreviewCanvas` was shared before).
///
/// **What it may not change** — this is a VIEW over the fields the emitter reads (DESIGN.md §11.10):
/// it clamps nothing, re-derives no rule, and mutates nothing. `area.verdict` is the MODEL's
/// `viewArea2Verdict`; a rect that has one is drawn red, DASHED and CLIPPED at the panel edge and
/// captioned "not pushed", because that is literally what happens — `viewArea2YAML` omits it and
/// CarPlay sees the full panel only. Severity is NOT colour-coded beyond that: odd / out-of-panel /
/// non-positive tears the session down while a below-floor rect only blacks the area out, and the
/// verdict STRING is the one place that distinguishes them. This view renders it; it never
/// classifies. Nothing here snaps or validates against a panel EDGE — an area touching no edge is
/// legal (refuted claim 1, §11.10) — and nothing implies a size floor separate from the all-even
/// rule (refuted claim 2).
///
/// **The resize area gets ONE solid box, not a box-inside-a-box.** `viewArea2YAML` writes its
/// `safeArea` as a copy of its `viewArea` (full-bleed) and the box re-emits it that way, so drawing
/// an inset inside it would draw an inset that is not in the YAML.
private struct DisplayLayoutPreview: View {
    /// The second view area, when the user has enabled it. `verdict` is the model's, passed in —
    /// this view never computes one.
    struct ResizeArea {
        let x: Int, y: Int, w: Int, h: Int
        let verdict: String?
    }

    let panelW: Int, panelH: Int
    let safeLeft: Int, safeTop: Int, safeRight: Int, safeBottom: Int
    var area: ResizeArea? = nil

    var body: some View {
        let c = PreviewCanvas.size(resW: panelW, resH: panelH)
        let fw = CGFloat(max(panelW, 1)), fh = CGFloat(max(panelH, 1))
        let sx = c.width / fw, sy = c.height / fh
        // REVEAL-ON-SET (owner, 2026-09-09): at 0,0,0,0 there is no safe area to show, so the panel
        // draws alone. Drawing a safe box coincident with the panel added a second rectangle and a
        // second legend swatch that carried no information at defaults.
        let hasInsets = max(safeLeft, 0) + max(safeTop, 0) + max(safeRight, 0) + max(safeBottom, 0) > 0
        let l = CGFloat(max(0, safeLeft)), t = CGFloat(max(0, safeTop))
        let sw = max(1, fw - l - CGFloat(max(0, safeRight)))
        let sh = max(1, fh - t - CGFloat(max(0, safeBottom)))
        let illegal = area?.verdict != nil
        // ALTERNATIVE LAYOUTS, NOT NESTED REGIONS (owner, 2026-09-09). Area 0 (the full panel) and
        // area 1 (this rect) are what the Dock resize button switches BETWEEN — iOS runs one or the
        // other, never both at once. So area 1 is drawn outline-forward over a light wash rather than
        // as an opaque child, and the legend names them as a pair.
        //
        // "Resize can go both ways": the rect may be LARGER or SMALLER than the safe area on either
        // axis. Z-ORDER BY AREA — the larger region is drawn first so the smaller always reads on
        // top; a fixed order would let a big area 1 swallow the safe box, or a big safe box hide a
        // small area 1. Fills stay translucent so an overlap is legible whichever way round it is.
        let areaPx = area.map { CGFloat(max($0.w, 1)) * CGFloat(max($0.h, 1)) } ?? 0
        let safePx = sw * sh
        let areaOnTop = areaPx <= safePx
        VStack(alignment: .leading, spacing: 6) {
            ZStack(alignment: .topLeading) {
                // PANEL — the full display. The video always fills this rectangle.
                RoundedRectangle(cornerRadius: 6)
                    .fill(PreviewCanvas.panelFill)
                    .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(PreviewCanvas.panelStroke))
                if areaOnTop {
                    safeBox(hasInsets: hasInsets, l: l, t: t, sw: sw, sh: sh, sx: sx, sy: sy)
                    resizeBox(sx: sx, sy: sy, illegal: illegal)
                } else {
                    resizeBox(sx: sx, sy: sy, illegal: illegal)
                    safeBox(hasInsets: hasInsets, l: l, t: t, sw: sw, sh: sh, sx: sx, sy: sy)
                }
            }
            .frame(width: c.width, height: c.height)
            // An out-of-panel rect runs off the canvas and is CUT HERE. That overflow IS the
            // failure — it is shown, never clamped away.
            .clipped()
            .frame(maxWidth: .infinity, alignment: .center)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 12) {
                    PreviewCanvas.legend(fill: PreviewCanvas.panelFill, stroke: PreviewCanvas.panelStroke,
                                         area == nil ? "Panel \(panelW)×\(panelH)" : "Area 0 · full panel")
                    if hasInsets {
                        PreviewCanvas.legend(fill: PreviewCanvas.safeFill, stroke: PreviewCanvas.safeStroke,
                                             "Safe area")
                    }
                }
                if let area {
                    PreviewCanvas.legend(fill: illegal ? PreviewCanvas.illegalFill : PreviewCanvas.areaFill,
                                         stroke: illegal ? PreviewCanvas.illegalStroke : PreviewCanvas.areaStroke,
                                         illegal ? "Area 1 · not pushed"
                                                 : "Area 1 · \(area.w)×\(area.h) @ (\(area.x), \(area.y))",
                                         dashed: illegal)
                    if !illegal {
                        Text("↔ Dock resize switches between these two layouts.")
                    }
                }
            }
            .font(.caption2).foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, alignment: .center)
            if let area {
                Text(area.verdict != nil
                     ? "Resize area is left out of the pushed config until it is legal; CarPlay will see the full panel only."
                     : "The resize area's safe area is its whole rect — the config carries no separate insets for it.")
                    .font(.caption2).foregroundStyle(illegal ? Color.red : Color.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.vertical, 2)
    }

    @ViewBuilder
    private func safeBox(hasInsets: Bool, l: CGFloat, t: CGFloat,
                         sw: CGFloat, sh: CGFloat, sx: CGFloat, sy: CGFloat) -> some View {
        if hasInsets {
            RoundedRectangle(cornerRadius: 3)
                .fill(PreviewCanvas.safeFill)
                .overlay(RoundedRectangle(cornerRadius: 3)
                    .strokeBorder(PreviewCanvas.safeStroke, lineWidth: 1.5))
                .frame(width: sw * sx, height: sh * sy)
                .offset(x: l * sx, y: t * sy)
        }
    }

    @ViewBuilder
    private func resizeBox(sx: CGFloat, sy: CGFloat, illegal: Bool) -> some View {
        if let area {
            RoundedRectangle(cornerRadius: 3)
                .fill(illegal ? PreviewCanvas.illegalFill : PreviewCanvas.areaFill)
                .overlay(RoundedRectangle(cornerRadius: 3)
                    // Dashed ONLY when illegal — the stroke is the second of the three channels
                    // (hue, dash, legend text) the verdict rides on; a dashed legal rect spent it.
                    .strokeBorder(illegal ? PreviewCanvas.illegalStroke : PreviewCanvas.areaStroke,
                                  style: StrokeStyle(lineWidth: 1.5, dash: illegal ? [4, 3] : [])))
                .frame(width: max(1, CGFloat(area.w) * sx), height: max(1, CGFloat(area.h) * sy))
                .offset(x: CGFloat(area.x) * sx, y: CGFloat(area.y) * sy)
        }
    }
}

/// The layout BOX — one bordered container around one stream's picture (owner, 2026-09-09: "The use
/// of boxes to visualize to user what the parameters would view itself as… Within one Main Box which
/// shows the Safe/View Area and resize area"), and since 2026-09-09 the FIRST thing in its sub-area
/// rather than the last: "Display layout visualization should be placed on top of the Display
/// section, below 'Panel Resolution' and above the Presets area." It is live — every field below it
/// in the same sub-area redraws it as it is typed.
///
/// PARAMETERISED, not main-only, because the Display section is now TWO PEER SUB-AREAS (owner: "Main
/// Display and Second Display are their own areas within 'Display'. Each having similar but also
/// unique parameters."). Main passes the panel, the main insets and the resize area; the cluster
/// passes its own panel and its own insets and no resize area (`viewArea2*` is main-display-only).
/// One component, so the two pictures cannot drift into two visual languages.
///
/// The badge is not decoration: it says the box is currently drawing a CARPLAY-EXCLUSIVE region —
/// the resize area on main (`exclusiveKeys(.insets, on: .carPlay)`), the safe area on the cluster
/// (`exclusiveKeys(.altDisplay, on: .carPlay)` == the four `altSafe*` keys). A panel is neutral, so
/// the badge is conditional, never permanent (§0 decision 4).
private struct DisplayLayoutBox: View {
    let title: String
    let panelW: Int, panelH: Int
    let safeLeft: Int, safeTop: Int, safeRight: Int, safeBottom: Int
    var area: DisplayLayoutPreview.ResizeArea? = nil
    /// True while the box is drawing a region that belongs to one projection only.
    var exclusiveToCarPlay: Bool = false
    /// One line under the picture for a state the PICTURE alone cannot say. Ten-word rule applies.
    var footnote: String? = nil

    var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 4) {
                DisplayLayoutPreview(
                    panelW: panelW, panelH: panelH,
                    safeLeft: safeLeft, safeTop: safeTop, safeRight: safeRight, safeBottom: safeBottom,
                    area: area)
                if let footnote {
                    Text(footnote)
                        .font(.caption2).foregroundStyle(.orange)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        } label: {
            HStack(spacing: 6) {
                Text(title).font(.caption).foregroundStyle(.secondary)
                if exclusiveToCarPlay {
                    ProjectionBadge(projection: .carPlay).help("CarPlay only")
                }
            }
        }
    }
}

/// The SECOND main view area — the CarPlay Dock "resize" button's target (DESIGN.md §11.10): a
/// toggle, the W×H@X,Y rect in panel pixels, and the model's verdict. A VIEW over
/// `VehicleConfigModel.viewArea2*`: it reads the same fields the emitter reads, clamps nothing, and
/// renders `viewArea2Verdict` verbatim — the rules (containment, all-even, positive, the 800×480 /
/// 480×800 product floor) live in `ViewArea2Rule`, not here. Mirrors `SafeAreaField`'s idiom:
/// per-value fields in a Grid, a one-line summary, then the verdict in the insets line's shape.
///
/// The PICTURE it used to own (`ViewAreasPreview`) is gone from here as of 2026-09-09: the panel and
/// the safe area it drew were already being drawn by the other preview, and the owner asked for one
/// main box instead of two drawings. `DisplayLayoutBox` — now the FIRST row of the Main display
/// sub-area, not the last — draws this rect as the third region of the single canvas, from these
/// same fields and this same verdict. Nothing about the rect's rendering changed: red, dashed,
/// clipped at the panel edge, captioned "not pushed".
///
/// SUBORDINATE TO VIEW AREAS (2026-09-09). This field renders only inside the unified area group,
/// which opens on `enablesViewAreas` — so a second area cannot be authored before view areas are on,
/// which is the dependency the old flat list hid. Its gate row carries the CarPlay badge ITSELF
/// (`ExclusiveControlLabel`) because that group is a NEUTRAL container: the main `mainSafe*` insets
/// inside it are `.limited`-on-Android-Auto, not CarPlay-exclusive, so the group may not wear one
/// "CarPlay only" label for everything in it (§0 decision 4). Badge the exclusive rows, not the box.
private struct ViewArea2Field: View {
    @ObservedObject var model: VehicleConfigModel

    private func field(_ label: String, _ value: Binding<Int>, invalid: Bool) -> some View {
        HStack(spacing: 3) {
            Text(label).font(.caption).foregroundStyle(.secondary).frame(width: 16, alignment: .leading)
            TextField("", value: value, format: .number)
                .frame(width: 54).multilineTextAlignment(.trailing)
                .foregroundStyle(invalid ? Color.red : Color.primary)
                .accessibilityLabel(label)
        }
    }

    var body: some View {
        Toggle(isOn: $model.viewArea2Enabled) {
            ExclusiveControlLabel(title: "Second view area (Dock resize)", key: "viewArea2Enabled",
                                  projection: .carPlay)
        }
        if model.viewArea2Enabled {
            let verdict = model.viewArea2Verdict
            let invalid = verdict != nil
            let floor = model.viewArea2MinimumSize
            LabeledContent {
                Grid(horizontalSpacing: 10, verticalSpacing: 4) {
                    GridRow { field("W", $model.viewArea2W, invalid: invalid); field("H", $model.viewArea2H, invalid: invalid) }
                    GridRow { field("X", $model.viewArea2X, invalid: invalid); field("Y", $model.viewArea2Y, invalid: invalid) }
                }
                .textFieldStyle(.bordered)
                .textInputBorderShape(.roundedRectangle)
            } label: {
                InfoLabel(title: "Resize area (px)", key: "viewArea2Rect")
            }
            LabeledContent("Layouts") {
                Text("\(model.mainWidth) × \(model.mainHeight) full panel  ↔  "
                     + "\(model.viewArea2W) × \(model.viewArea2H) @ (\(model.viewArea2X), \(model.viewArea2Y))"
                     + " · floor \(floor.width) × \(floor.height)")
                    .font(.caption)
                    .foregroundStyle(invalid ? Color.red : .secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let verdict {
                Text(verdict)
                    .font(.caption).foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }
            // The resize ANIMATION (2026-09-09): `view_area_anim_ms`, the box's
            // `animationDurationMillis` on its updateViewArea answer. It animates the transition
            // between the full panel and the rect above, so it sits beside that rect and shows only
            // when a second area exists. CarPlay-exclusive (`exclusiveKeys(.insets, on: .carPlay)`),
            // badged on its own row like its neighbours. 3000 = the box default, not sent.
            LabeledContent {
                HStack(spacing: 4) {
                    TextField("", value: $model.viewAreaAnimMs, format: .number)
                        .frame(width: 62).multilineTextAlignment(.trailing)
                        .accessibilityLabel("Resize animation")
                    Text("ms").foregroundStyle(.tertiary).font(.caption)
                }
                .textFieldStyle(.bordered)
                .textInputBorderShape(.roundedRectangle)
            } label: {
                ExclusiveControlLabel(title: "Resize animation", key: "viewAreaAnimMs",
                                      projection: .carPlay)
            }
        }
    }
}

/// Per-edge inset editor. The user enters px from each edge; the resulting safe box is shown as a
/// number AND drawn to scale. Converted to the wire's absolute rect (originX/width) by the CarPlay
/// renderer on Save.
///
/// TWO instances exist — main and alt/cluster — and they are not symmetric:
///
///   * NEITHER draws a picture any more (2026-09-09). Each sub-area's box is the FIRST row of that
///     sub-area, above the fields, not below them — the owner's placement ruling — so this field
///     renders numbers only and `DisplayLayoutBox` renders the geometry. One picture per stream,
///     from the same model fields, in the one place the reader looks first.
///   * `lockedByCornerMasks` (default false) is the cornerMasks ↔ safe-area coupling (§11.10), and
///     it is MAIN-ONLY: `info.rs:576` is `let masks = is_main && crate::levers::cornermasks()`, so
///     the alt stream has no cornerMasks flag and KEEPS its safeArea. Greying both would make a
///     working alt control look broken — the alt call site takes the `false` default and cannot pass
///     it by accident. The reason is stated INLINE, not on hover: iOS HARD-FAILS
///     `checkCarPlayFeatureAcceptance` on "cornerMasks flag set but a safeArea defined in
///     viewAreas", and a hard fail is not a hover-help nicety.
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
    var lockedByCornerMasks: Bool = false

    private var safeW: Int { resWidth - max(0, left) - max(0, right) }
    private var safeH: Int { resHeight - max(0, top) - max(0, bottom) }
    private var invalid: Bool {
        left < 0 || top < 0 || right < 0 || bottom < 0 || safeW < 16 || safeH < 16
    }
    private var isInset: Bool { left > 0 || top > 0 || right > 0 || bottom > 0 }

    private func field(_ label: String, _ value: Binding<Int>) -> some View {
        HStack(spacing: 3) {
            Text(label).font(.caption).foregroundStyle(.secondary).frame(width: 16, alignment: .leading)
            TextField("", value: value, format: .number)
                .frame(width: 54).multilineTextAlignment(.trailing)
                .foregroundStyle(invalid ? Color.red : Color.primary)
                .accessibilityLabel(label)
        }
    }

    var body: some View {
        LabeledContent {
            Grid(horizontalSpacing: 10, verticalSpacing: 4) {
                GridRow { field("L", $left); field("T", $top) }
                GridRow { field("R", $right); field("B", $bottom) }
            }
            .textFieldStyle(.bordered)
            .textInputBorderShape(.roundedRectangle)
            // Dead, not hidden — the fields keep their values and come back the moment corner masks
            // go off, and the reason sits directly under them.
            .disabled(lockedByCornerMasks)
            .opacity(lockedByCornerMasks ? 0.5 : 1)
        } label: {
            InfoLabel(title: "Insets (px)", key: infoKey)
        }
        // One of §11.4's live-computed warnings in kind: a hard fail, stated where the dead fields
        // are, in ten words. iOS rejects the whole feature set when both are declared, so the box
        // omits the safeArea dict and every number above it stops being pushed.
        if lockedByCornerMasks {
            Text("Corner masks on — insets dropped; iOS forbids both together.")
                .font(.caption).foregroundStyle(.orange)
                .fixedSize(horizontal: false, vertical: true)
        }

        LabeledContent("Safe box") {
            Text(isInset ? "\(max(0, safeW)) × \(max(0, safeH)) px @ (\(max(0, left)), \(max(0, top)))"
                         : "full frame (no inset)")
                .font(.caption)
                .foregroundStyle(invalid ? Color.red : .secondary)
                // Trailing-aligned to the FORM row edge, which sits past the GroupBox clip; the
                // padding brings it in line with the fields above so no glyph is cut.
                .lineLimit(1).padding(.trailing, 8)
        }
        .opacity(lockedByCornerMasks ? 0.5 : 1)
        // One of the six live-computed warnings (DESIGN.md §11.4): unconditional inline text,
        // never hover-only, and not subject to the ten-word rule.
        if invalid {
            Text("Insets leave too little room — each side must keep ≥16 px.")
                .font(.caption).foregroundStyle(.red)
        }

        // The cluster/second display keeps its OWN box — "Cluster layout", a different panel with its
        // own resolution and its own insets — but it is drawn by `DisplayLayoutBox` at the TOP of the
        // Second display sub-area, not here under the fields (owner, 2026-09-09: the visualisation
        // goes on top). Reveal-on-set is unchanged and lives in `DisplayLayoutPreview`: an enabled
        // cluster with 0,0,0,0 insets shows just its panel. No resize area there either —
        // `viewArea2*` is main-display-only.
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
    // The per-mode summary formerly rendered inline here (14/12/16 words, always visible when
    // audioMode != "custom") moved verbatim into FieldInfo["audioFormats"] behind the "Audio
    // formats" (i) (DESIGN.md §11.4 TEN-WORD RULE, 2026-09-08).
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
            // Live-computed, orange — same class as the six protected warnings (DESIGN.md §11.4):
            // it flags an invalid config, so it is condensed to ten words rather than moved behind
            // a popover (was 15 words; "output codec" and "default set" facts kept).
            Text("Needs an output codec, or the box uses its defaults.")
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
            // Full explanation moved to FieldInfo["liveAppearance"] behind the (i) (DESIGN.md §11.4
            // TEN-WORD RULE, 2026-09-08): the footer was 58 words, always visible when this section
            // is expanded.
            InfoLabel(title: "Display appearance — live session", key: "liveAppearance")
        }
    }
}

// MARK: - Feature block

/// One neutral feature as a run of Form rows: heading (`Feature.title` + `FeatureBadgeStrip`) → the
/// neutral control(s) → the per-protocol explanation rows when a projection is `.limited` or
/// `.unsupported` → extras (the `GateReveal` exclusive sub-groups). The order is DESIGN.md
/// §11.2/§11.3's, and it is fixed here rather than at each call site so no feature can forget its
/// badges.
///
/// The heading is spelled out here rather than delegated to `FeatureHeading` because Phase 3 needs
/// the badge strip to carry `currentValue` (below) and `FeatureHeading` takes only a `Feature` —
/// and because `Feature.summary` no longer renders as a row (DESIGN.md §1: it duplicates
/// `FieldInfo.text[key]`, and `FieldPopover` reads it instead). The `summary` PROPERTY stays on
/// `Feature`; only this render site is gone.
///
/// `currentValue` is the matrix's string form of the value the neutral control currently holds
/// (`FeatureMatrix.valueNotes`' doc comment: "1920x1080", "60", "center", "48000", "extended").
/// Threading it into `FeatureBadgeStrip` is what folds the former `ValueNoteLabel` row into the
/// popover, under whichever projection has a note for that exact value — so a tier that closed the
/// transport is still flagged the moment it is selected, just without a permanent row. Features
/// with no value-level provenance leave it nil.
private struct FeatureBlock<Neutral: View, Extras: View>: View {
    let feature: Feature
    var currentValue: String? = nil
    /// False when an enclosing peer header already names this feature and carries its badge strip —
    /// a subsection label whose only other content is an (i) is a duplicate row (owner, 2026-09-09).
    var showsHeading: Bool = true
    @ViewBuilder let neutral: () -> Neutral
    @ViewBuilder let extras: () -> Extras

    var body: some View {
        if showsHeading {
            HStack(alignment: .firstTextBaseline) {
                Text(feature.title).font(.subheadline.weight(.semibold))
                Spacer(minLength: 8)
                FeatureBadgeStrip(feature: feature, currentValue: currentValue)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        neutral()
        // NO `FeatureExplanationRows` here (DESIGN.md §11.4, TEN-WORD RULE, 2026-09-08 owner ruling
        // against a screenshot of `.headUnitName`): its `FeatureSupport.effect` sentences run 40-55
        // words and rendered for every non-uniform feature. §1's earlier exception ("inline when a
        // projection is .limited or .unsupported") is SUPERSEDED — the inline signal it protected is
        // now carried by the badge strip above, which draws each projection's level glyph at two
        // words per chip; the prose it explains is popover content, reachable from either the badge
        // or the (i) since both open the same `FieldPopover(feature:)`. Do not re-add this call, and
        // do not condense `FeatureMatrix`'s effect strings — the popover wants them at full length.
        // The component itself stays defined for AdapterTab.swift:299's `always: true` caller.
        extras()
    }
}

extension FeatureBlock where Extras == EmptyView {
    init(feature: Feature, currentValue: String? = nil, showsHeading: Bool = true,
         @ViewBuilder neutral: @escaping () -> Neutral) {
        self.init(feature: feature, currentValue: currentValue, showsHeading: showsHeading, neutral: neutral,
                  extras: { EmptyView() })
    }
}

/// The label of a PROTOCOL-EXCLUSIVE control that cannot live inside the `GateReveal` it belongs to,
/// because its own value IS that reveal's gate (`GateReveal.isOn` is a read-only `Bool` by design —
/// the control that flips a gate cannot be hosted by the gate). Such a control keeps the projection
/// badge on its OWN row: DESIGN.md §0 decision 4 says a protocol-exclusive setting lives under its
/// projection's badge, and a bare row with the badge sitting on the collapsed group BELOW it labels
/// the wrong thing — which is how "Enhanced Siri", "Audio formats" and "Declare vehicle status" came
/// to read as protocol-neutral.
///
/// Same shape as `RestrictionRow` and `LiveAppearanceSection.row` below/above. No free-text
/// parameter (`FeatureBadges.swift`'s standing rule): `title`/`key` are the `FieldInfo` lookup keys
/// `InfoLabel` already takes, and the badge and its help are rendered from `Projection` — the caller
/// asserts membership by citing `Feature.exclusiveKeys(on:)` at the site, exactly as the `GateReveal`
/// call sites do.
private struct ExclusiveControlLabel: View {
    let title: String
    let key: String
    let projection: Projection

    var body: some View {
        HStack(spacing: 6) {
            InfoLabel(title: title, key: key)
            Spacer(minLength: 6)
            ProjectionBadge(projection: projection)
                .help("\(projection.displayName) only")
        }
    }
}

// MARK: - Sections

private struct IdentitySection: View {
    @ObservedObject var model: VehicleConfigModel

    var body: some View {
        CollapsibleFeatureSection(.identity) {
            FeatureBlock(feature: .headUnitName) {
                TextField(text: $model.name) { InfoLabel(title: "Name", key: "name") }
            } extras: {
                // exclusiveKeys(.headUnitName, on: .carPlay) == ["accessoryName"]
                GateReveal(feature: .headUnitName, projection: .carPlay, isOn: true) {
                    TextField(text: $model.accessoryName, prompt: Text("CarLink-<box id>")) {
                        InfoLabel(title: "Accessory name", key: "accessoryName")
                    }
                }
            }

            FeatureBlock(feature: .branding) {
                TextField(text: $model.oemIconLabel) { InfoLabel(title: "Maker label", key: "brandingLabel") }
            } extras: {
                // exclusiveKeys(.branding, on: .carPlay) == ["oemIconEnabled", "oemIconVisible", "oemIconBase64"]
                // The enabling toggle stays outside the reveal — GateReveal's `isOn` is read-only, so
                // the control that flips it cannot live inside its own gated content (§11.5's
                // `oemIconEnabled` gate: → icon picker, visibility toggle). It is CarPlay-exclusive
                // all the same, so it carries the badge ITSELF rather than borrowing the one on the
                // group below it (DESIGN.md §0 decision 4).
                Toggle(isOn: $model.oemIconEnabled) {
                    ExclusiveControlLabel(title: "Advertise OEM icon config", key: "oemIconEnabled",
                                          projection: .carPlay)
                }
                GateReveal(feature: .branding, projection: .carPlay, isOn: model.oemIconEnabled, caption: "OEM icon") {
                    HStack(spacing: 12) {
                        if !model.oemIconBase64.isEmpty,
                           let data = Data(base64Encoded: model.oemIconBase64),
                           let img = NSImage(data: data) {
                            Image(nsImage: img).resizable().aspectRatio(contentMode: .fit)
                                .frame(width: 44, height: 44).clipShape(RoundedRectangle(cornerRadius: 6))
                            Text("\(model.oemIconW)×\(model.oemIconH) PNG").font(.caption).foregroundStyle(.secondary)
                        } else {
                            Text("No image chosen").font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Button("Choose PNG…") { model.pickOemIcon() }
                        Button("Use Sim Icon") { model.useSimulatorOemIcon() }
                    }
                    // Full "why" (was 32 words on the false branch) moved to FieldInfo["oemIconVisible"]
                    // behind the (i) below (DESIGN.md §11.4 TEN-WORD RULE); the inline caption keeps
                    // only the two per-state sentinel facts, condensed to ≤10 words each.
                    InfoToggle(title: "Show icon in CarPlay", key: "oemIconVisible", isOn: $model.oemIconVisible)
                    Text(model.oemIconVisible
                         ? "iOS shows the icon (oemIconVisible: true)."
                         : "iOS hides the icon (oemIconVisible: false is sent).")
                        .font(.caption2).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
                // Moved verbatim to FieldInfo["oemIconEnabled"], behind the "Advertise OEM icon
                // config" (i) above (DESIGN.md §11.4 TEN-WORD RULE, 2026-09-08): this footnote was
                // 35 words and drew unconditionally whenever Identity is expanded — 56% of that
                // section's resting words.
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
///
/// **ONE ROW, NOT SIX (owner, 2026-09-09).** This block used to draw a ~30-word summary sentence plus
/// one ⚠️ row per note — five or six wrapped lines under Panel resolution, on every open, in a
/// section the owner had already called over-crowded. It is now one row: the badge, two words, an
/// (i), and — when the renderer had to approximate something — an amber glyph and a COUNT. The
/// summary and every note, VERBATIM and unabridged, are the popover. Nothing is deleted; a reader
/// who sees "3 not expressed to Android Auto" is one click from the three sentences.
///
/// **This overrides DESIGN.md §11.4**, which lists `AACapability.negotiationNotes` among the six
/// live-computed warnings that may NEVER move to hover, and adds "the one-line 'Declares W×H @ fps,
/// codec' summary stays inline when its section is open". The owner overrode both for this block on
/// 2026-09-09; §11.4 needs amending to record the exception. The warning's PURPOSE is preserved and
/// is why the cue is a count and not a bare (i): the at-rest signal "the geometry you typed is not
/// what Android Auto will get, in N places" still renders without a hover. What moved behind the
/// click is WHICH approximations — not THAT there are any. The other five §11.4 warnings are
/// untouched and stay unconditional inline text.
private struct AANegotiationReadout: View {
    @ObservedObject var model: VehicleConfigModel
    @ObservedObject var bridge = ControlsBridge.shared
    /// Popover presentation only — no model state, nothing persisted (`@State` is a MACRO in SDK 27:
    /// initialised at the declaration, never assigned in an `init`, and this view has no `init`).
    @State private var show = false

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
        let summary = "Declares \(tier.w)×\(tier.h) @ \(fps) fps, \(codec); \(geometry)."
        let notes = cap.negotiationNotes
        HStack(spacing: 6) {
            ProjectionBadge(projection: .androidAuto)
            Text("Negotiation preview").font(.caption.weight(.semibold))
            Button { show.toggle() } label: {
                Image(systemName: "info.circle").foregroundStyle(.secondary).font(.caption)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("About the Android Auto negotiation preview")
            .popover(isPresented: $show, arrowEdge: .trailing) {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Android Auto negotiation").font(.headline)
                    // The former inline summary, unchanged and at full length: a popover is not
                    // bound by the ten-word rule (§11.4 scopes it to what renders INLINE).
                    Text(summary).font(.callout).fixedSize(horizontal: false, vertical: true)
                    if !notes.isEmpty {
                        Divider()
                        // Every note VERBATIM, with the ⚠️ it carried inline. Not one word cut.
                        ForEach(Array(notes.enumerated()), id: \.offset) { item in
                            HStack(alignment: .firstTextBaseline, spacing: 4) {
                                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                                Text(item.element).fixedSize(horizontal: false, vertical: true)
                            }
                            .font(.caption)
                        }
                    }
                }
                .padding(12)
                .frame(width: 340)
            }
            // The severity cue that stays at rest: glyph + COUNT, six words, no prose (§11.4's
            // purpose kept, its form overridden). Nothing renders here when the renderer expressed
            // the geometry exactly — an amber row that is always there is not a warning.
            if !notes.isEmpty {
                HStack(spacing: 3) {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                    Text("\(notes.count) not expressed to Android Auto")
                }
                .font(.caption)
                .foregroundStyle(.orange)
                .accessibilityElement(children: .combine)
                .accessibilityLabel("\(notes.count) values not expressed to Android Auto; open the info button for each")
            }
            if bridge.isAndroidAuto {
                Text("· live").font(.caption2).foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
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
        // Guarded: an empty `ForEach` still occupies a row in a grouped `Form`, which showed up as
        // a blank line whenever nothing had been clamped — i.e. almost always.
        if !notes.isEmpty {
            ForEach(Array(notes.enumerated()), id: \.offset) { item in
            HStack(alignment: .firstTextBaseline, spacing: 4) {
                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                Text(item.element).fixedSize(horizontal: false, vertical: true)
            }
            .font(.caption2)
            }
        }
    }
}

/// The Display section as TWO PEER SUB-AREAS plus one global block (owner, 2026-09-09: "Main Display
/// and Second Display are their own areas within 'Display'. Each having similar but also unique
/// parameters."). Both peers are built in the SAME order so they read as parallels:
///
///   1. the sub-area header;
///   2. the enable (second display only — main is always on);
///   3. that stream's `DisplayLayoutBox`, FIRST ("placed on top… below 'Panel Resolution' and above
///      the Presets area") — the picture the rest of the sub-area drives;
///   4. preset / resolution W×H, frame rate (+ density and diagonal on main, which has no alt twin);
///   5. that stream's area group: a safe-area toggle, then its insets and readout.
///
/// Where they differ, and why — all three are MAIN-ONLY, so the second display never offers them:
///   * corner masks — `let masks = is_main && crate::levers::cornermasks()` (`info.rs:576`); the alt
///     stream has no cornerMasks flag and KEEPS its safeArea, so its inset fields are never greyed;
///   * the second view area — `viewArea2*` has no alt equivalent;
///   * `drawUIOutsideSafeArea` — WWDC 2023-10150; the emitter never wrote it for the alt stream.
///
/// The video codec block is GLOBAL — one copy after both peers, never duplicated into each.
///
/// The reorganisation exists to expose ONE coupling: safe area, view areas, corner masks and the
/// resize area are one feature on the box, not four toggles. The token is `enablesViewAreas ||
/// enablesCornerMasks || safeAreaInsetPresent || viewArea2Present` (`App/VehicleConfig.swift:94`), so
/// a non-zero inset, a mask or a legal resize area arms it with the toggle OFF. The main area group
/// therefore (a) opens on the toggle, (b) ALSO opens when the model says the token is armed without
/// it, naming the cause inline, and (c) says when nothing arms it and the box drops the feature.
/// Feature ORDER inside a section is a layout choice; what is pinned (`SettingsTests.swift`) is
/// `Feature.Section` membership and `Feature.exclusiveKeys(on:)`, and neither changes here.
private struct DisplaySection: View {
    @ObservedObject var model: VehicleConfigModel

    /// The SECOND display's safe-area toggle has no persisted key of its own and must not gain one
    /// (the YAML fixture pins the emitted document). It is DERIVED: on ⇔ the alt insets are non-zero,
    /// which is exactly the emitter's notion of "an alt safe area exists". This flag only keeps the
    /// fields open after the user flips it on and before they type a value. View-local, never
    /// persisted. `@State` is a MACRO in SDK 27: initialised here, never assigned in an `init`, and
    /// this view has no `init` (the synthesised `init(model:)` skips it).
    @State private var altSafeRevealed = false

    private var mainIsInset: Bool {
        model.mainSafeLeft > 0 || model.mainSafeTop > 0 || model.mainSafeRight > 0 || model.mainSafeBottom > 0
    }
    private var altIsInset: Bool {
        model.altSafeLeft > 0 || model.altSafeTop > 0 || model.altSafeRight > 0 || model.altSafeBottom > 0
    }

    /// The token as the MODEL arms it — `config.enabledFeatures()` is the same array the box is armed
    /// from (`VehicleConfig.swift:94`), never re-derived here where it would drift.
    private var viewAreasArmed: Bool { model.config.enabledFeatures().contains("viewAreas") }

    /// Armed with the toggle OFF — the silent arm made explicit. The causes are the `VehicleConfig`
    /// fields the model itself materialised (`safeAreaInsetPresent` covers BOTH streams' insets, by
    /// the emitter's own `hasRealSafeAreaInset` rule), listed in the token's own order.
    private var armedWithoutToggle: [String] {
        guard !model.enablesViewAreas, viewAreasArmed else { return [] }
        let c = model.config
        var causes: [String] = []
        if c.safeAreaInsetPresent { causes.append("insets") }
        if c.enablesCornerMasks { causes.append("corner masks") }
        if c.viewArea2Present { causes.append("resize area") }
        return causes
    }

    /// The main area group is open when the toggle is on OR something else armed the token — a
    /// control that is silently pushing must never be hidden.
    private var mainAreaOpen: Bool { model.enablesViewAreas || !armedWithoutToggle.isEmpty }

    /// Second-display safe area: on ⇔ insets present (or just revealed). OFF ZEROES THE INSETS —
    /// that is what "no safe area" means to the emitter (a full-panel rect is not a real inset,
    /// `VehicleConfig.hasRealSafeAreaInset`), so the toggle and the YAML can never disagree.
    private var altSafeBinding: Binding<Bool> {
        Binding(
            get: { altSafeRevealed || altIsInset },
            set: { on in
                altSafeRevealed = on
                if !on {
                    model.altSafeLeft = 0; model.altSafeTop = 0
                    model.altSafeRight = 0; model.altSafeBottom = 0
                }
            })
    }

    /// The panel's implied density from its diagonal, for the owner to compare with `dpi`. Uses the
    /// profile's own arithmetic (`PanelGeometry.impliedDPI`) so the two can never disagree.
    private var impliedDPI: Int? {
        PanelGeometry(width: model.mainWidth, height: model.mainHeight, maxFPS: model.maxFPS,
                      dpi: model.dpi, diagonalInches: model.diagonalInches > 0 ? model.diagonalInches : nil)
            .impliedDPI
    }

    /// The resize area as the emitter sees it: drawn whenever ENABLED (a legal one is pushed and
    /// arms the token even with view areas off; an illegal one is drawn red/dashed/"not pushed").
    /// `verdict` is the model's — the view classifies nothing.
    private var resizeArea: DisplayLayoutPreview.ResizeArea? {
        guard model.viewArea2Enabled else { return nil }
        return .init(x: model.viewArea2X, y: model.viewArea2Y, w: model.viewArea2W, h: model.viewArea2H,
                     verdict: model.viewArea2Verdict)
    }

    /// One header per peer, identical in style, so the two sub-areas read as parallels.
    /// `feature:` folds that feature's badge strip + (i) INTO this header (owner, 2026-09-09).
    /// Before, a peer header ("MAIN DISPLAY") was immediately followed by a `FeatureBlock` heading
    /// naming the same thing ("Panel resolution") with the (i) on it — two rows where one says
    /// everything. The block then passes `showsHeading: false` so it renders only its controls.
    private func peerHeader(_ title: String, divider: Bool, feature: Feature? = nil) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            if divider { Divider() }
            HStack(alignment: .firstTextBaseline) {
                Text(title)
                    .font(.subheadline.weight(.bold))
                    .textCase(.uppercase)
                    .kerning(0.6)
                    .foregroundStyle(.secondary)
                if let feature {
                    Spacer(minLength: 8)
                    FeatureBadgeStrip(feature: feature)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.top, divider ? 6 : 0)
    }

    var body: some View {
        CollapsibleFeatureSection(.display) {
            mainDisplay
            secondDisplay
            // GLOBAL — one codec block after both peers, never inside either. No explicit
            // `Divider()` here (2026-09-09): a grouped `Form` already draws a separator between
            // rows, so an added one stacked a second near-identical line plus its padding and read
            // on screen as two BLANK ROWS under the cluster's "Safe area" toggle. The Form's own
            // separator is the boundary. Still not an uppercase header — that read as a third
            // display.
            FeatureBlock(feature: .videoCodec) {
                InfoToggle(title: "Allow H.265 (HEVC)", key: "hevcAllowed", isOn: $model.enablesHEVC)
            } extras: {
                // exclusiveKeys(.videoCodec, on: .carPlay) == ["enablesVideoPlayback"]
                GateReveal(feature: .videoCodec, projection: .carPlay, isOn: true) {
                    InfoToggle(title: "Video playback", key: "enablesVideoPlayback", isOn: $model.enablesVideoPlayback)
                }
                // exclusiveKeys(.videoCodec, on: .androidAuto) == ["aaPreferHEVC"]
                // Disabled-not-hidden (DESIGN.md §11.4): gated on HEVC, stays visible-but-disabled.
                GateReveal(feature: .videoCodec, projection: .androidAuto, isOn: true) {
                    // Label states the SCOPE, not a condition (owner, 2026-09-09): H.265 is on by
                    // default because it is the more efficient codec, not because a resolution
                    // demands it. AA already uses it above 1080p; this extends it downward.
                    InfoToggle(title: "H.265 below 1080p too", key: "aaPreferHEVC", isOn: $model.aaPreferHEVC)
                        .disabled(!model.enablesHEVC)
                        .opacity(model.enablesHEVC ? 1 : 0.5)
                }
            }

            // ANDROID AUTO NEGOTIATION — moved to the bottom of the whole section 2026-09-09
            // (owner). It was sitting between Resolution and the area controls on the MAIN peer,
            // which put an Android Auto readout in the middle of the CarPlay geometry it does not
            // govern. It is a readout of what THIS WHOLE SECTION negotiated down to, so it reads
            // last, after both display peers and the codec block. `aaFitPanelWithMargins` follows
            // it because the margins are the mechanism the readout reports on.
            //
            // NOTE for a later reader: `AANegotiationReadout` and the margins toggle are declared
            // by `exclusiveKeys(.panelGeometry, on: .androidAuto)`, so they belong to the
            // panelGeometry FEATURE even though they no longer render inside its block. The
            // placement table in DESIGN.md §1 still lists them under Display ▸ panelGeometry —
            // that is still true of the DATA; only the render position moved.
            AANegotiationReadout(model: model)
            GateReveal(feature: .panelGeometry, projection: .androidAuto, isOn: true) {
                InfoToggle(title: "Fit a non-tier panel with margins", key: "aaFitPanelWithMargins",
                           isOn: $model.aaFitPanelWithMargins)
            }
        }
    }

    // MARK: Main display

    @ViewBuilder
    private var mainDisplay: some View {
        let masks = model.enablesCornerMasks
        peerHeader("Main display", divider: false, feature: .panelGeometry)

        // `currentValue` is the AA tier string form (`FeatureMatrix.valueNotes`: "1920x1080") —
        // the former ValueNoteLabel row, now folded into the badge strip's popover.
        FeatureBlock(feature: .panelGeometry,
                     currentValue: "\(model.mainWidth)x\(model.mainHeight)",
                     showsHeading: false) {
            // THE MAIN BOX, first: under the "Panel resolution" heading, above the presets. With
            // corner masks on the box OMITS the safeArea dict (iOS hard-fails on both together), so
            // the picture is given no insets and says why — it draws what CarPlay will see, never an
            // inset that is not pushed. The resize area is drawn from the same fields and the same
            // verdict the emitter reads: red, dashed, clipped, "not pushed" when illegal.
            DisplayLayoutBox(
                title: "Main layout",
                panelW: model.mainWidth, panelH: model.mainHeight,
                safeLeft: masks ? 0 : model.mainSafeLeft, safeTop: masks ? 0 : model.mainSafeTop,
                safeRight: masks ? 0 : model.mainSafeRight, safeBottom: masks ? 0 : model.mainSafeBottom,
                area: resizeArea,
                exclusiveToCarPlay: resizeArea != nil,
                footnote: masks && mainIsInset ? "Corner masks on — safe area not pushed." : nil)
            ResolutionField(title: "Preset", width: $model.mainWidth, height: $model.mainHeight)
        } extras: {
            ClampNotesReadout(model: model, prefix: "Panel")
        }

        // AREA GROUP MOVED HERE 2026-09-09 (owner): directly under the Resolution area, before
        // frame rate and density. The safe area, the masks and the resize area are all expressed in
        // the panel's own pixels, so they belong beside the resolution that defines those pixels —
        // frame rate and density are panel facts that move no region in the picture.
        // THE MAIN AREA GROUP — one feature on the box, presented as one group. The master toggle is
        // the EXISTING `enablesViewAreas` (no new persisted key); it is CarPlay-exclusive
        // (`exclusiveKeys(.insets, on: .carPlay)`) and IS the gate, so it wears its badge on its own
        // row (`ExclusiveControlLabel`) rather than living inside a `GateReveal`. The group it opens
        // is a NEUTRAL container: the `mainSafe*` insets inside it are `.limited` on Android Auto,
        // not CarPlay-exclusive, so the box may not wear one "CarPlay only" label for everything in
        // it (§0 decision 4). The exclusive rows inside are badged individually.
        FeatureBlock(feature: .insets) {
            Toggle(isOn: $model.enablesViewAreas) {
                ExclusiveControlLabel(title: "Safe area / view areas", key: "enablesViewAreas",
                                      projection: .carPlay)
            }
            // The OTHER half of the coupling (§11.10): nothing arms the token, so the box drops it.
            if !viewAreasArmed {
                Text("Off with no insets — the box drops viewAreas support.")
                    .font(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            // The SILENT ARM, said out loud: the toggle is off but the token is pushed anyway.
            if !armedWithoutToggle.isEmpty {
                HStack(alignment: .firstTextBaseline, spacing: 4) {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                    Text("Armed anyway by: \(armedWithoutToggle.joined(separator: ", ")).")
                }
                .font(.caption).foregroundStyle(.orange)
                .fixedSize(horizontal: false, vertical: true)
            }
            if mainAreaOpen {
                GroupBox {
                    VStack(alignment: .leading, spacing: 6) {
                        // MAIN insets. `lockedByCornerMasks` is passed HERE and nowhere else: the
                        // coupling is main-stream-only (`info.rs:576`, `let masks = is_main && …`).
                        // The reason renders INLINE under the dead fields, inside `SafeAreaField`.
                        SafeAreaField(left: $model.mainSafeLeft, top: $model.mainSafeTop,
                                      right: $model.mainSafeRight, bottom: $model.mainSafeBottom,
                                      resWidth: model.mainWidth, resHeight: model.mainHeight,
                                      lockedByCornerMasks: masks)
                        // Disabled-not-hidden (DESIGN.md §11.4): gated on an inset being set — and
                        // on that inset actually being pushed, which masks prevent.
                        Toggle(isOn: $model.mainDrawOutsideSafe) {
                            // Renamed 2026-09-09 (owner, after confirming on hardware): was "Allow UI
                            // outside safe area", which described the WIRE KEY rather than the effect and
                            // implied tappable UI relocates. It does not — only the backdrop moves.
                            ExclusiveControlLabel(title: "Extend wallpaper", key: "drawUIOutsideSafeArea",
                                                  projection: .carPlay)
                        }
                        .disabled(!mainIsInset || masks)
                        .opacity(mainIsInset && !masks ? 1 : 0.5)
                        Toggle(isOn: $model.enablesCornerMasks) {
                            ExclusiveControlLabel(title: "Corner masks (cutout)", key: "enablesCornerMasks",
                                                  projection: .carPlay)
                        }
                        // The five viewArea2 keys: toggle + rect + verdict (DESIGN.md §11.10). Only
                        // meaningful with view areas on, which is why it lives in this group; its
                        // picture is the main box above — one canvas, not two.
                        ViewArea2Field(model: model)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.top, 2)
                } label: {
                    Text("Safe area, masks and resize area").font(.caption).foregroundStyle(.secondary)
                }
            }
        }

        // "30"/"60" carries a note on BOTH projections (AA per tier, CarPlay 30 unverified);
        // one `currentValue` reaches both blocks of the popover.
        FeatureBlock(feature: .frameRate, currentValue: "\(model.maxFPS)") {
            FrameRatePicker(title: "Frame rate", fps: $model.maxFPS)
        }

        // Heading dropped: "Pixel density" then a "Density" row said the same thing twice. The
        // badge strip moves onto the Density row's own label.
        FeatureBlock(feature: .pixelDensity, showsHeading: false) {
            LabeledContent {
                HStack(spacing: 4) {
                    TextField("", value: $model.dpi, format: .number)
                        .frame(width: 62).multilineTextAlignment(.trailing)
                        .accessibilityLabel("Density")
                    Text("dpi").foregroundStyle(.tertiary).font(.caption)
                }
                .textFieldStyle(.bordered)
                .textInputBorderShape(.roundedRectangle)
            } label: {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    InfoLabel(title: "Density", key: "dpi")
                    FeatureBadgeStrip(feature: .pixelDensity)
                }
            }
            LabeledContent {
                HStack(spacing: 4) {
                    TextField("", value: $model.diagonalInches, format: .number.precision(.fractionLength(0...1)))
                        .frame(width: 62).multilineTextAlignment(.trailing)
                        .accessibilityLabel("Diagonal")
                    Text("in").foregroundStyle(.tertiary).font(.caption)
                }
                .textFieldStyle(.bordered)
                .textInputBorderShape(.roundedRectangle)
            } label: { InfoLabel(title: "Diagonal", key: "diagonalInches") }
            // One of the five protected live-computed warnings (DESIGN.md §11.4): unconditional
            // inline text, exempt from the ten-word rule.
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
    }

    // MARK: Second display

    @ViewBuilder
    private var secondDisplay: some View {
        peerHeader("Second display", divider: true, feature: .altDisplay)

        FeatureBlock(feature: .altDisplay, showsHeading: false) {
            Toggle("Enable", isOn: $model.altVideoEnabled)
            if model.altVideoEnabled {
                // THE CLUSTER BOX, first — the same component as the main box, so the two pictures
                // are one visual language. Its own panel, its own insets, NO resize area
                // (`viewArea2*` is main-only) and NO mask lock (the alt stream keeps its safeArea).
                // Reveal-on-set is `DisplayLayoutPreview`'s: at 0,0,0,0 the panel draws alone. The
                // four `altSafe*` keys are CarPlay-exclusive, so the badge appears when they draw.
                DisplayLayoutBox(
                    title: "Cluster layout",
                    panelW: model.altWidth, panelH: model.altHeight,
                    safeLeft: model.altSafeLeft, safeTop: model.altSafeTop,
                    safeRight: model.altSafeRight, safeBottom: model.altSafeBottom,
                    exclusiveToCarPlay: altIsInset)
                ResolutionField(title: "Preset", width: $model.altWidth, height: $model.altHeight, infoKey: "altResolution")
                // SAME ORDER AS THE MAIN PEER (owner, 2026-09-09): the area controls sit directly
                // under Resolution, before frame rate — the insets are in this panel's pixels, so
                // they belong beside the resolution that defines them. This stream's OWN safe-area
                // toggle (derived, see `altSafeBinding`) then its OWN insets. NO masks, NO
                // draw-outside, NO resize area: all three are main-only.
                GateReveal(feature: .altDisplay, projection: .carPlay, isOn: true, caption: "safe area") {
                    Toggle("Safe area", isOn: altSafeBinding)
                    if altSafeBinding.wrappedValue {
                        SafeAreaField(left: $model.altSafeLeft, top: $model.altSafeTop,
                                      right: $model.altSafeRight, bottom: $model.altSafeBottom,
                                      resWidth: model.altWidth, resHeight: model.altHeight,
                                      infoKey: "safeArea")
                    }
                }
                FrameRatePicker(title: "Frame rate", fps: $model.altFPS)
            }
        } extras: {
            ClampNotesReadout(model: model, prefix: "Alt display")
            // exclusiveKeys(.altDisplay, on: .carPlay) == ["altSafeLeft", "altSafeTop", "altSafeRight",
            // "altSafeBottom", "altDrawOutsideSafe"]. `altDrawOutsideSafe` is listed but NOT
            // rendered: drawUIOutsideSafeArea is a main-display-only flag (WWDC 2023-10150), the
            // emitter never wrote it for the alt stream. Kept off-screen rather than offered and
            // ignored. Gate is `altVideoEnabled` (§11.5) — the enabling toggle above.
            //
            // This stream's OWN safe-area toggle (derived, see `altSafeBinding`) then its OWN insets.
            // NO `lockedByCornerMasks` here — the alt call site takes the `false` default and cannot
            // pass it by accident.
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
        CollapsibleFeatureSection(.appearance) {
            FeatureBlock(feature: .theme) {
                Picker(selection: theme) {
                    ForEach(AppearanceTheme.allCases, id: \.self) { Text(Self.label($0)).tag($0) }
                } label: { InfoLabel(title: "Theme", key: "theme") }
                .pickerStyle(.segmented)
            } extras: {
                // exclusiveKeys(.theme, on: .carPlay) == ["enablesUIAppearance", "enablesMapAppearance"]
                GateReveal(feature: .theme, projection: .carPlay, isOn: true) {
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

            // Live (runtime) appearance controls nest here rather than at the top level of the Form
            // (DESIGN.md §11 audit: these 5 rows were named as part of the density problem even
            // though they are not a `Feature`). Thematically the same topic as Theme/Status bar
            // above, so folding them under the same collapsible reaches the resting-row target
            // without inventing a new collapsible container for a non-`Feature.Section` block.
            LiveAppearanceSection(bridge: ControlsBridge.shared)
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
        CollapsibleFeatureSection(.driving) {
            // `model.driverPosition` is already the matrix's string form ("left"/"right"/"center"),
            // so it is the value note key verbatim — CENTER's "never declared" note now surfaces in
            // the badge popover instead of a standing row.
            FeatureBlock(feature: .driverPosition, currentValue: model.driverPosition) {
                Picker(selection: position) {
                    ForEach(Self.positions, id: \.self) { Text(Self.label($0)).tag($0) }
                } label: { InfoLabel(title: "Driver sits", key: "driverPosition") }
                .pickerStyle(.segmented)
            }

            FeatureBlock(feature: .drivingRestrictions) {
                InfoToggle(title: "Declare restrictions", key: "restrictionsDeclared", isOn: $model.limitedUIConfigEnabled)
                if model.limitedUIConfigEnabled {
                    InfoLabel(title: "While the car is moving, withhold:", key: "restrictions")
                    ForEach(FeatureMatrix.restrictionMapping, id: \.restriction) { m in
                        RestrictionRow(mapping: m, isOn: binding(for: m.restriction))
                    }
                    // Runtime-switching fact folded into FieldInfo["restrictions"] (behind the
                    // "While the car is moving, withhold:" (i) directly above), DESIGN.md §11.4
                    // TEN-WORD RULE — this caption was 16 words.
                }
            } extras: {
                // exclusiveKeys(.drivingRestrictions, on: .carPlay) == ["limitedUIJapanMaps",
                // "limitedUIPairedDevices", "limitedUIThemeCustomization", "limitedUIAutomakerSettings",
                // "limitedUIAutomakerSettingsInfoButton"]. Gate is `limitedUIConfigEnabled` (§11.5),
                // the "Declare restrictions" toggle above.
                GateReveal(feature: .drivingRestrictions, projection: .carPlay, isOn: model.limitedUIConfigEnabled, caption: "limitedUIConfig extras") {
                    Toggle("Japan maps", isOn: $model.limitedUIJapanMaps)
                    // NOT capability toggles: real Apple LimitedUIConfig keys that airPlayElements
                    // never emits — kept only so exported YAML round-trips the full Apple schema.
                    // Warning #4 (DESIGN.md §11.4) — never moves to hover.
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

private struct VehicleSection: View {
    @ObservedObject var model: VehicleConfigModel

    var body: some View {
        CollapsibleFeatureSection(.vehicle) {
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
                            // macOS 27 renders `TextField(_ titleKey:value:format:)`'s title as a
                            // VISIBLE trailing label, not a placeholder — so "kW" drew twice, once
                            // from the title and once from the explicit `Text("kW")` below. Title
                            // emptied; the explicit Text stays as the single unit label.
                            TextField("", value: Binding(
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
                // exclusiveKeys(.powertrain, on: .carPlay) == ["vehicleStatusEnabled", "vehicleStatusCaps"].
                // The enabling toggle stays outside the reveal (GateReveal's `isOn` is read-only,
                // §11.5's `vehicleStatusEnabled` gate: → caps + conflict warning) and therefore
                // carries the CarPlay badge on its own row — the group below it is gated on this
                // very toggle, so its badge cannot stand in for this one.
                Toggle(isOn: $model.vehicleStatusEnabled) {
                    ExclusiveControlLabel(title: "Declare vehicle status", key: "vehicleStatusEnabled",
                                          projection: .carPlay)
                }
                .disabled(!VehicleConfigModel.vehicleStatusUnlocked)
                GateReveal(feature: .powertrain, projection: .carPlay, isOn: model.vehicleStatusEnabled, caption: "vehicle status (C-4 gated)") {
                    // Warning #5 (DESIGN.md §11.4) — never moves to hover.
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
                        // Warning #6 (DESIGN.md §11.4) — never moves to hover.
                        Text("Apple's spec forbids combining the unified range warning with the per-engine ones — the greyed entries will not be sent.")
                            .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
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
        CollapsibleFeatureSection(.input) {
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
                    GateReveal(feature: .inputDevices, projection: .carPlay, isOn: true, caption: "hidConfig") {
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
        CollapsibleFeatureSection(.audio) {
            // The voice rate is the value with AA provenance ("48000" proven, "24000" never
            // negotiated); it keys the popover's value note (formerly a ValueNoteLabel row).
            FeatureBlock(feature: .audioProfile, currentValue: "\(model.voiceRateHz)") {
                Picker(selection: $model.voiceRateHz) {
                    ForEach(Self.voiceRates, id: \.self) { Text("\($0 / 1000) kHz").tag($0) }
                } label: { InfoLabel(title: "Voice rate", key: "voiceRateHz") }
                .pickerStyle(.segmented)
                InfoToggle(title: "Calls over the projection link", key: "telephonyOverProjection", isOn: $model.telephonyOverProjection)
            } extras: {
                // exclusiveKeys(.audioProfile, on: .carPlay) == ["audioMode", "audioFormats",
                // "enablesMainBufferedAudio", "enablesEnhancedSiri"] — ALL FOUR are CarPlay-only.
                // The mode picker stays outside the reveal because it is the control that computes
                // the gate (`audioMode == "custom"`, §11.5) and GateReveal's `isOn` is read-only, so
                // it wears the badge on its own row. The other two keys gate nothing, so they go
                // back INSIDE a sub-group — always revealed, since there is no gate to close.
                Picker(selection: $model.audioMode) {
                    Text("Auto — match transport").tag("auto")
                    Text("Wired — PCM").tag("wired_pcm")
                    Text("Wireless — AAC (full 8)").tag("wireless_8")
                    Text("Custom…").tag("custom")
                } label: {
                    ExclusiveControlLabel(title: "Audio formats", key: "audioFormats",
                                          projection: .carPlay)
                }
                // Per-mode summary lives in FieldInfo["audioFormats"] now, behind the "Audio
                // formats" (i) above — see AudioLabels.audioType note.
                GateReveal(feature: .audioProfile, projection: .carPlay, isOn: model.audioMode == "custom", caption: "advertised format table") {
                    InfoLabel(title: "Custom advertised set", key: "audioFormatRow")
                    AudioFormatsEditor(model: model)
                }
                // `enablesMainBufferedAudio` + `enablesEnhancedSiri` and the format reference they
                // are read against: CarPlay-exclusive with no gate of their own, so `isOn: true` —
                // the same always-revealed idiom the other ungated exclusive groups in this file use
                // (.headUnitName, .panelGeometry, .insets, .videoCodec, .theme, .metadataFeeds).
                GateReveal(feature: .audioProfile, projection: .carPlay, isOn: true, caption: "buffered media + Siri") {
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
        CollapsibleFeatureSection(.dataFeeds) {
            // `model.metadataTier` is the value note key verbatim ("proven"/"extended"/"all", and
            // "rx-only" if an imported document carries it) — the REFUTED "all" note and its
            // do-not-retry record now reach the reader through the badge popover.
            FeatureBlock(feature: .metadataFeeds, currentValue: model.metadataTier) {
                InfoLabel(title: "Feeds the head unit consumes", key: "metadataFeeds")
                Toggle("Now playing", isOn: $model.metadataNowPlaying)
                Toggle("Navigation (turn-by-turn)", isOn: $model.metadataNavigation)
                Toggle("Telephony (call state)", isOn: $model.metadataTelephony)
            } extras: {
                // exclusiveKeys(.metadataFeeds, on: .carPlay) == ["metadataTier", "metadataSkip",
                // "enablesFocusTransfer", "enablesUIContext", "enablesUISync", "enablesFileTransfer",
                // "enablesLogTransfer", "enablesVehicleDataProtocol", "enablesDCX"]
                GateReveal(feature: .metadataFeeds, projection: .carPlay, isOn: true, caption: "iAP2 declaration + transfer flags") {
                    Picker(selection: $model.metadataTier) {
                        Text("Proven — device-accepted baseline").tag("proven")
                        Text("Extended — full paired Start/Stop set").tag("extended")
                        Text("All — every capability in the table").tag("all")
                    } label: { InfoLabel(title: "Metadata declaration", key: "metadataTier") }
                    HStack {
                        InfoLabel(title: "Skip features", key: "metadataSkip")
                        // Same macOS 27 behaviour on the `text:` overload: "e.g. call_history"
                        // was meant as a PLACEHOLDER but sat in the titleKey position, so it drew as
                        // a second visible label beside "Skip features". Moved to `prompt:`, which is
                        // the placeholder slot, and the field keeps an explicit accessibility label.
                        TextField("", text: $model.metadataSkip, prompt: Text("e.g. call_history"))
                            .textFieldStyle(.bordered)
                            .textInputBorderShape(.roundedRectangle)
                            .accessibilityLabel("Skip features")
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
    @State private var showPushedDocument = false
    @State private var confirmReset = false

    var body: some View {
        Form {
            IdentitySection(model: model)
            DisplaySection(model: model)
            AppearanceSection(model: model)
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
            //
            // Collapsed behind its own `DisclosureGroup` (plain `@State`, not persisted, matching
            // every other Phase 3 disclosure — DESIGN.md §11.1 decision 2): this block is not a
            // `Feature.Section` so it cannot use `CollapsibleFeatureSection`, but it is one of the
            // 11 resting rows the tab is sized against (DESIGN.md §11, §1) and must not sit expanded.
            Section {
                DisclosureGroup(isExpanded: $showPushedDocument) {
                    // LEFT IN PLACE (DESIGN.md §11.4 TEN-WORD RULE audit, 2026-09-08): 80 words, but
                    // it costs nothing at rest — `showPushedDocument` starts collapsed (decision above)
                    // and this is the ONLY content of that reveal, not a control's caption, so there
                    // is no (i)-bearing control to move it behind without inventing one. Lowest
                    // priority per the task brief; condensing risks the "config is emitted only when
                    // X" class of fact this project has lost before (FieldInfo.swift comment, 2026-09-08).
                    Text("The YAML this app pushes to the adapter at SUBSCRIBE. Two things in one document: the CarPlay rendering of the profile above (Apple's VehicleConfig schema — displays, hidConfig, accessoryConfig, audio, iAP2 metadata), and the Adapter tab's settings (wireless, hot_handover, pairing, android_auto, wifi_ap, appDrivenSetup), which gate both projections. Android Auto's own declaration is not in here — it is built in-app from the same profile and previewed under Panel resolution. Read-only; edit the profile or the Adapter tab, not the YAML.")
                        .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    DisclosureGroup("Generated YAML", isExpanded: $showYAML) {
                        Text(model.yaml)
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    Button("Export YAML…", systemImage: "square.and.arrow.up") { exportYAML() }
                } label: {
                    HStack(spacing: 6) {
                        ProjectionBadge(projection: .carPlay)
                        ProjectionBadge(projection: .androidAuto)
                        Text("Pushed adapter document").font(.subheadline.weight(.semibold))
                    }
                }
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
