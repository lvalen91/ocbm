// FeatureBadges.swift — the SwiftUI rendering of `FeatureMatrix`: per-projection support badges, the
// per-protocol explanation rows, value-level provenance labels, and the badged container for a
// protocol-EXCLUSIVE sub-group. Everything on screen here is read from `FeatureMatrix` data — there
// is deliberately no free-text parameter on any of these views, because free text is how the
// Settings window came to say "CarPlay-only" over a control that was live only for Android Auto
// (defect 2) and "inert" over the head-unit name Android Auto advertises in three places (defect 3).
// If a row needs to say something new about a protocol, the sentence goes into
// `FeatureMatrix.support(_:on:)` where the compiler and the W5 tests (DESIGN.md §9 item 5: every
// feature × projection has a non-empty `effect`) can see it.
//
// This is the same discipline as `ControlsWindow.swift:69-104`'s `Capability` idiom — a control is
// routed to the owning protocol or VISIBLY unavailable, never a silent third thing — lifted from a
// Bool per intent to a `FeatureSupport` per (feature, projection) with the middle "limited" state a
// resolution snapped to a tier or a name shown without its icon needs.
//
// Created 2026-09-04 (Settings reorganisation, DESIGN.md §6 W3). SwiftUI-only; nothing here is
// compiled by the headless test harness, which checks the matrix directly.

import SwiftUI

// MARK: - Ten-word rule enforcement (DESIGN.md, owner 2026-09-08)

/// Defensive clamp for a caller-supplied string this file renders UNCONDITIONALLY inline (a
/// `GateReveal`/`ExclusiveSubGroup` `caption`, a value note). Every current call site is already
/// short (`VehicleTab.swift`'s longest caption is "vehicle status (C-4 gated)", 4 words), so this
/// never fires today — it exists so a FUTURE caption passed by a file this rule also binds
/// (`VehicleTab.swift`, `AdapterTab.swift`, …) cannot silently reintroduce inline explanation prose
/// through a parameter this component already accepts. `String` interpolation counts as one token,
/// same convention used to count `Feature.summary`/`FeatureSupport.effect` against the rule
/// elsewhere in this file.
private func tenWordClamp(_ text: String) -> String {
    let words = text.split(separator: " ")
    guard words.count > 10 else { return text }
    return words.prefix(10).joined(separator: " ") + "…"
}

// MARK: - Glyphs and colours (one place, so every row agrees)

/// Visual vocabulary for a support level. Green = the value is expressed as authored; orange = it is
/// expressed with an approximation or caveat the row names; grey = the protocol (or this app's
/// renderer for it) ignores it. Grey, not red: an unsupported feature is a fact about the vendor's
/// vocabulary, not an error the owner can fix.
extension FeatureSupport.Level {
    var symbolName: String {
        switch self {
        case .supported:   return "checkmark.circle.fill"
        case .limited:     return "exclamationmark.triangle.fill"
        case .unsupported: return "minus.circle"
        }
    }
    var tint: Color {
        switch self {
        case .supported:   return .green
        case .limited:     return .orange
        case .unsupported: return .secondary
        }
    }
    var word: String {
        switch self {
        case .supported:   return "supported"
        case .limited:     return "limited"
        case .unsupported: return "not expressed"
        }
    }
}

/// Visual vocabulary for provenance. The named states are `VerificationStatus`; "unverified" and
/// "refuted" both get a warning-class glyph because both mean "do not trust this value on a real
/// car until someone has" — the difference is that refuted also means "and do not re-try it
/// without new evidence" (the note says what happened).
///
/// Each switch carries a `default:` arm on purpose: a status the matrix adds (a source-verified
/// tier between unverified and device-proven was being discussed on 2026-09-04) must render as a
/// neutral "see the note" glyph rather than break the build or borrow the device-proven seal.
/// Give the new case its own arm when it lands; until then the compiler's "default will never be
/// executed" warning is the reminder. Ordered by trust, never by colour alone: the seal is reserved
/// for something seen on a phone.
extension Verification {
    var symbolName: String {
        switch status {
        case .deviceProven: return "checkmark.seal.fill"
        case .unverified:   return "questionmark.circle.fill"
        case .refuted:      return "xmark.octagon.fill"
        default:            return "doc.text.magnifyingglass"
        }
    }
    var tint: Color {
        switch status {
        case .deviceProven: return .green
        case .unverified:   return .orange
        case .refuted:      return .red
        default:            return .secondary
        }
    }
    /// "device-proven 2026-08-27", "unverified", "refuted 2026-09-04"; an unnamed status shows its
    /// raw value so the row is never silent about what it is.
    var shortLabel: String {
        let word: String
        switch status {
        case .deviceProven: word = "device-proven"
        case .unverified:   word = "unverified"
        case .refuted:      word = "refuted"
        default:            word = status.rawValue
        }
        if let date { return "\(word) \(date)" }
        return word
    }
}

/// The provenance glyph with its short label; `help` carries the full evidence sentence.
struct VerificationGlyph: View {
    let verification: Verification
    var showLabel = true

    var body: some View {
        HStack(spacing: 3) {
            Image(systemName: verification.symbolName).foregroundStyle(verification.tint)
            if showLabel {
                Text(verification.shortLabel).foregroundStyle(.secondary)
            }
        }
        .font(.caption2)
        .help(verification.note)
        .accessibilityLabel("\(verification.shortLabel): \(verification.note)")
    }
}

// MARK: - Badges

/// One capsule: the projection's name plus (when a level is given) its support glyph. Used both
/// inside `FieldPopover`'s per-projection blocks (with a level) and as the header of an
/// `ExclusiveSubGroup` (without one — the sub-group's controls exist ONLY for that projection, so
/// "supported" would be tautological).
struct ProjectionBadge: View {
    let projection: Projection
    var level: FeatureSupport.Level? = nil

    var body: some View {
        HStack(spacing: 3) {
            if let level {
                Image(systemName: level.symbolName).foregroundStyle(level.tint)
            }
            Text(projection.displayName)
        }
        .font(.caption2.weight(.medium))
        .padding(.horizontal, 6).padding(.vertical, 2)
        .background(Capsule().fill((level?.tint ?? Color.accentColor).opacity(0.14)))
        .overlay(Capsule().strokeBorder((level?.tint ?? Color.accentColor).opacity(0.45)))
        .fixedSize()
    }
}

// MARK: - Explanation rows

/// One row per projection under a control: badge + level word, each opening the consolidated
/// `FieldPopover` for the full `FeatureSupport.effect` + provenance. Hidden when
/// `FeatureMatrix.isUniform(feature)` (both projections express the value as authored — the badges
/// already say so and repeating "declared as maxFPS" twice is noise), unless `always` is set.
///
/// DESIGN.md's ten-word rule (owner, 2026-09-08): this used to print `entry.support.effect` and a
/// `VerificationGlyph` inline — 18-56 words per row, the rule's worst-named offender, and its sole
/// remaining caller (`AdapterTab.swift:299`, `always: feature == .wirelessRadios`) passes `always:
/// true`, so that text rendered unconditionally on every load. The rule's own text names two ways to
/// fix an `always: true` row: condense to ten words, or "do not go inline at all." There is no
/// natural ten-word cut of a 20-56 word evidence sentence without losing the fact it exists to
/// state, so this takes the second option — the full sentence, and the `VerificationGlyph` that used
/// to sit beside it, moved entirely into `FieldPopover` (§11.3's consolidated popover, already the
/// single source of truth for this data). Only the level word (one token) stays inline, same
/// information the badge's icon already carries. This is the bound enforced IN THE COMPONENT: no
/// future caller of `FeatureExplanationRows`, `always: true` or not, can reintroduce inline effect
/// prose without editing this body again.
struct FeatureExplanationRows: View {
    let feature: Feature
    var always = false

    var body: some View {
        if always || !FeatureMatrix.isUniform(feature) {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(FeatureMatrix.supports(feature), id: \.projection) { entry in
                    FieldPopover(title: feature.title, key: feature.fieldInfoKey, feature: feature) {
                        HStack(alignment: .firstTextBaseline, spacing: 6) {
                            ProjectionBadge(projection: entry.projection, level: entry.support.level)
                            Text(entry.support.level.word)
                                .font(.caption)
                                .foregroundStyle(entry.support.isAvailable ? Color.primary : Color.secondary)
                            Image(systemName: "info.circle")
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                    }
                    .accessibilityLabel(
                        "\(entry.projection.displayName): \(entry.support.level.word). \(entry.support.effect)"
                    )
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

// MARK: - Value notes

/// Provenance for the CURRENT value of a control on one projection — "1920×1080: device-proven
/// 2026-08-27" — or nothing when the matrix has no note for that value. Unverified and refuted
/// values carry the warning glyph and the note's evidence, so a tier that closed the transport
/// (2560×1440 as H.264, 2026-09-04) is flagged at the moment it is selected, not at the moment the
/// phone hangs up. `value` is the matrix's string form (`FeatureMatrix.valueNotes` doc comment).
struct ValueNoteLabel: View {
    let feature: Feature
    let projection: Projection
    let value: String

    var body: some View {
        if let note = FeatureMatrix.valueNote(for: feature, on: projection, value: value) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                ProjectionBadge(projection: projection)
                VStack(alignment: .leading, spacing: 1) {
                    HStack(spacing: 4) {
                        Text(value).font(.system(.caption, design: .monospaced))
                        VerificationGlyph(verification: note.verification)
                    }
                    Text(note.verification.note)
                        .font(.caption2).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

// MARK: - Feature heading and exclusive sub-groups

/// The first row of a feature block: neutral title, the badge strip, and the neutral summary.
struct FeatureHeading: View {
    let feature: Feature

    /// DESIGN.md's ten-word rule (owner, 2026-09-08): `feature.summary` runs to 11+ words for
    /// several features ("The vehicle maker's name and logo on the projected home screen." alone is
    /// 11) and was never a lookup key a popover could bound — it rendered here unconditionally. The
    /// §11.2 component table calls for exactly this: `FeatureHeading` "survives, minus the summary
    /// line." The full sentence is still one (i)-tap away via the consolidated `FieldPopover`
    /// (`FeatureBadgeStrip` below), so nothing described is now undiscoverable — only unconditional.
    ///
    /// **Consistency fix, Change 1 (owner: "Make these all (i)").** Previously drew `FeatureBadgeRow`
    /// — one chip per projection, each opening a DIFFERENT single-projection popover
    /// (`FeatureSupportDetail`) — the same multi-control shape the owner flagged on the Vehicle tab,
    /// just with non-identical destinations rather than identical ones. §11.2 already specified this
    /// call site as `FeatureBadgeStrip`; `FeatureBadgeRow`/`FeatureSupportDetail` had no other caller
    /// and are removed rather than left as dead code two components could drift from.
    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            Text(feature.title).font(.subheadline.weight(.semibold))
            Spacer(minLength: 8)
            FeatureBadgeStrip(feature: feature)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A protocol-EXCLUSIVE sub-group: controls that exist in one vendor's vocabulary only, rendered
/// INSIDE the neutral feature they belong to (owner steer, DESIGN.md §0 decision 4 — never on a
/// protocol tab) under that projection's badge. The placement table is `Feature.exclusiveKeys(on:)`;
/// callers pass the same feature + projection so a reader can cross-check the group against it.
///
/// **Survives unchanged as the Phase 3 `GateReveal`'s OPEN-gate rendering** (DESIGN.md §11.2: today
/// this chrome drew even when the sub-group's own internal gate was closed — one toggle wearing a
/// full box; `GateReveal` below draws it only once that gate is open). Kept as its own type, rather
/// than folded into `GateReveal`, because `VehicleTab.swift`'s existing `ExclusiveSubGroup(...)`
/// call sites are being edited concurrently by another worker and must keep compiling either way.
struct ExclusiveSubGroup<Content: View>: View {
    let feature: Feature
    let projection: Projection
    var caption: String? = nil
    @ViewBuilder let content: () -> Content

    var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 6) {
                content()
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.top, 2)
        } label: {
            HStack(spacing: 6) {
                ProjectionBadge(projection: projection)
                Text("\(projection.displayName) only").font(.caption).foregroundStyle(.secondary)
                if let caption {
                    Text("· \(caption)").font(.caption).foregroundStyle(.tertiary)
                }
            }
        }
    }
}

// MARK: - Phase 3 components (DESIGN.md §11.2)

/// One `Feature.Section` as a collapsed-by-default disclosure (DESIGN.md §11.1 decision 1: all
/// sections collapsed on open, no exceptions, no persisted expansion state — plain `@State`, reset
/// each launch). Backed by `Section(isExpanded:content:header:)`, the public macOS-14 init
/// (SwiftUI.swiftinterface:14568-14571; DESIGN.md §11.7 correction), so it must be used where its
/// caller already is — inside a `Form`.
///
/// **`@State` trap (DESIGN.md §11.7, SDK 27 `@State`-as-macro):** the init below assigns `section`
/// and `content` only; it never assigns `expanded`, which is why this compiles at all. Assigning an
/// init-time value to `expanded` here would either fail ("variable used before being initialized")
/// or silently be ignored in favour of the inline `= false` default — reordering the assignments is
/// NOT the fix, only never assigning it in `init` is.
struct CollapsibleFeatureSection<Content: View>: View {
    let section: Feature.Section
    @ViewBuilder let content: () -> Content
    @State private var expanded = false

    init(_ section: Feature.Section, @ViewBuilder content: @escaping () -> Content) {
        self.section = section
        self.content = content
    }

    var body: some View {
        Section(isExpanded: $expanded) {
            content()
        } header: {
            Text(section.rawValue).font(.headline)
        }
    }
}

/// ONE (i) trigger for the whole feature, not one chip per projection (owner, 2026-09-09: "Make
/// these all (i) that when clicked shows the colored Projection state support… Instead of two
/// labels which when clicked show the same thing"). Before this change the strip drew one
/// `ProjectionBadge` per projection, and every one of them opened the identical `FieldPopover` for
/// this `feature` (§11.3's popover already renders ALL projections, coloured, in one place) — two
/// or more controls for one destination, and the strip's width grew with projection count for no
/// reason. Now there is exactly one glyph, and it opens that same already-consolidated popover
/// (`FieldPopoverContent`/`FieldPopoverProjectionBlock` in `FieldInfo.swift`, unmodified — it
/// already draws the badge, level word and colour for every projection, so no frozen-file change
/// was needed for the colour requirement). `currentValue`, when given, is threaded through exactly
/// as before so the popover folds in the former `ValueNoteLabel` content for whichever projection's
/// value note matches it.
///
/// **At-rest severity cue (the trade-off the owner asked to be flagged):** the per-projection
/// chips used to carry colour at rest — a `.limited`/`.unsupported` projection was visible without
/// opening anything. A bare (i) has none. This keeps a MINIMAL version of that signal within the
/// ten-word rule: the lone glyph itself is tinted by the worst level across the feature's
/// projections, using the same three `FeatureSupport.Level` colours (green/amber/grey) the popover
/// already uses — no new palette. What is still lost: WHICH projection is the amber/grey one, and
/// why, are no longer visible without a click — only "at least one projection is not fully
/// supported" survives at rest. That is a real reduction from two labelled chips; see the report.
struct FeatureBadgeStrip: View {
    let feature: Feature
    var currentValue: String? = nil

    /// `.limited` (amber — a value expressed but approximated) outranks `.unsupported` (grey — a
    /// value simply not expressed) which outranks `.supported` (green): the same severity order
    /// `FieldPopoverProjectionBlock` already implies by drawing amber as a warning and grey as
    /// neutral. Reused, not reinvented.
    private var worstLevel: FeatureSupport.Level {
        let levels = Set(FeatureMatrix.supports(feature).map(\.support.level))
        if levels.contains(.limited) { return .limited }
        if levels.contains(.unsupported) { return .unsupported }
        return .supported
    }

    /// Names every projection and its level, since VoiceOver loses the per-chip labels a sighted
    /// reader used to get from two separate `ProjectionBadge`s.
    private var accessibilitySummary: String {
        FeatureMatrix.supports(feature)
            .map { "\($0.projection.displayName) \($0.support.level.word)" }
            .joined(separator: ", ")
    }

    var body: some View {
        FieldPopover(title: feature.title, key: feature.fieldInfoKey, feature: feature,
                     currentValue: currentValue) {
            Image(systemName: "info.circle.fill")
                .foregroundStyle(worstLevel.tint)
                .font(.caption)
        }
        .help("Projection support: \(accessibilitySummary)")
        .accessibilityLabel("\(feature.title) projection support: \(accessibilitySummary)")
    }
}

/// The gate-and-reveal container for a protocol-exclusive sub-group (DESIGN.md §11.2, §11.5):
/// `isOn` is the caller's EXISTING gate (`altVideoEnabled`, `oemIconEnabled`, `touchScreenHighFidelity`,
/// …, §11.5's list of eight). Gate CLOSED draws exactly one row and no `GroupBox` chrome — just the
/// projection badge, the "<Projection> only" caption and an optional caption, so a reader still sees
/// the sub-group exists without paying its full height. Gate OPEN draws today's content verbatim, by
/// delegating straight to `ExclusiveSubGroup` so the two never drift apart.
struct GateReveal<Content: View>: View {
    let feature: Feature
    let projection: Projection
    let isOn: Bool
    var caption: String? = nil
    @ViewBuilder let content: () -> Content

    init(feature: Feature, projection: Projection, isOn: Bool, caption: String? = nil,
         @ViewBuilder content: @escaping () -> Content) {
        self.feature = feature
        self.projection = projection
        self.isOn = isOn
        self.caption = caption
        self.content = content
    }

    var body: some View {
        if isOn {
            ExclusiveSubGroup(feature: feature, projection: projection, caption: caption, content: content)
        } else {
            HStack(spacing: 6) {
                ProjectionBadge(projection: projection)
                Text("\(projection.displayName) only").font(.caption).foregroundStyle(.secondary)
                if let caption {
                    // Clamped (see `tenWordClamp` above) so this one-row closed state can never grow
                    // into the effect prose the rule bans — every caller today is 4 words or fewer.
                    Text("· \(tenWordClamp(caption))").font(.caption).foregroundStyle(.tertiary)
                }
                Spacer()
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}
