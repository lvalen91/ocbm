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

/// One capsule: the projection's name plus (when a level is given) its support glyph. Used both in
/// `FeatureBadgeRow` (with a level) and as the header of an `ExclusiveSubGroup` (without one — the
/// sub-group's controls exist ONLY for that projection, so "supported" would be tautological).
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

/// One badge per projection, each opening a popover with the full `FeatureSupport` record for that
/// (feature, projection): the vendor's own term and schema key, the one-sentence effect, and the
/// provenance. This is the row the owner asked for — "what each protocol will actually do with the
/// value" — visible at a glance and readable in full on click.
struct FeatureBadgeRow: View {
    let feature: Feature

    var body: some View {
        HStack(spacing: 6) {
            ForEach(FeatureMatrix.supports(feature), id: \.projection) { entry in
                SupportBadge(feature: feature, projection: entry.projection, support: entry.support)
            }
        }
    }

    private struct SupportBadge: View {
        let feature: Feature
        let projection: Projection
        let support: FeatureSupport
        @State private var show = false

        var body: some View {
            Button { show.toggle() } label: {
                ProjectionBadge(projection: projection, level: support.level)
            }
            .buttonStyle(.plain)
            .help("\(projection.displayName): \(support.level.word) — \(support.effect)")
            .popover(isPresented: $show, arrowEdge: .bottom) {
                FeatureSupportDetail(feature: feature, projection: projection, support: support)
            }
        }
    }
}

/// The popover body: everything `FeatureMatrix` knows about one (feature, projection).
private struct FeatureSupportDetail: View {
    let feature: Feature
    let projection: Projection
    let support: FeatureSupport

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                ProjectionBadge(projection: projection, level: support.level)
                Text(support.level.word).font(.caption).foregroundStyle(support.level.tint)
                Spacer()
            }
            Text(feature.title).font(.headline)
            Text(support.effect).font(.callout).fixedSize(horizontal: false, vertical: true)
            Divider()
            Grid(alignment: .leading, horizontalSpacing: 8, verticalSpacing: 3) {
                GridRow {
                    Text("Their term").foregroundStyle(.secondary)
                    Text(support.vendorTerm)
                }
                GridRow {
                    Text("Schema key").foregroundStyle(.secondary)
                    Text(support.vendorKey ?? "— (nothing emitted)")
                        .font(.system(.caption, design: .monospaced))
                }
                GridRow {
                    Text("Format").foregroundStyle(.secondary)
                    Text(projection.schemaName)
                }
            }
            .font(.caption)
            Divider()
            HStack(alignment: .top, spacing: 4) {
                VerificationGlyph(verification: support.verification)
            }
            Text(support.verification.note)
                .font(.caption2).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(12)
        .frame(width: 340)
    }
}

// MARK: - Explanation rows

/// One caption row per projection under a control: "<badge> <effect> <provenance>". Hidden when
/// `FeatureMatrix.isUniform(feature)` (both projections express the value as authored — the badges
/// already say so and repeating "declared as maxFPS" twice is noise), unless `always` is set.
struct FeatureExplanationRows: View {
    let feature: Feature
    var always = false

    var body: some View {
        if always || !FeatureMatrix.isUniform(feature) {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(FeatureMatrix.supports(feature), id: \.projection) { entry in
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        ProjectionBadge(projection: entry.projection, level: entry.support.level)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(entry.support.effect)
                                .font(.caption)
                                .foregroundStyle(entry.support.isAvailable ? Color.primary : Color.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                            VerificationGlyph(verification: entry.support.verification)
                        }
                    }
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

/// The first row of a feature block: neutral title, the badge row, and the neutral summary.
struct FeatureHeading: View {
    let feature: Feature

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(alignment: .firstTextBaseline) {
                Text(feature.title).font(.subheadline.weight(.semibold))
                Spacer(minLength: 8)
                FeatureBadgeRow(feature: feature)
            }
            Text(feature.summary).font(.caption).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A protocol-EXCLUSIVE sub-group: controls that exist in one vendor's vocabulary only, rendered
/// INSIDE the neutral feature they belong to (owner steer, DESIGN.md §0 decision 4 — never on a
/// protocol tab) under that projection's badge. The placement table is `Feature.exclusiveKeys(on:)`;
/// callers pass the same feature + projection so a reader can cross-check the group against it.
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
