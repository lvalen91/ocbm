// VehicleConfig.swift — the structured, transport-free slice of the pushed VehicleConfig that the
// app-driven SETUP author (AirPlaySetupSession) needs (plan P3).
//
// This is the Swift counterpart to the Rust harness's `AuthoringConfig` (host/ocbm-host/src/setup_driver.rs).
// The Rust side parses the pushed YAML into a minimal feature policy; here the app already OWNS the config
// as live `@Published` fields on `VehicleConfigModel`, so `VehicleConfig` is materialized from those
// fields (VehicleConfigModel.config) rather than re-parsed from YAML. That is the "one source of truth"
// the plan asks for: the SAME model fields drive BOTH the YAML pushed at SUBSCRIBE and the SETUP answers,
// so the two can never diverge — the box's /info and the host-authored SETUP response are built from one
// set of booleans.
//
// Pure + Codable + IOKit-free so it compiles into tests/run_tests.sh headlessly alongside AirPlaySetupSession.

import Foundation

/// The feature toggles + stream/display fields the SETUP author needs. `Codable` so it can be logged /
/// snapshotted; `Equatable` for tests.
struct VehicleConfig: Codable, Equatable {
    // Feature toggles that gate `enabledFeatures` (phase-1 SETUP response). Names mirror the box's
    // `accessoryConfig` booleans + the altVideoStreams presence, exactly the inputs Rust's
    // `AuthoringConfig` derives from.
    var enablesHEVC: Bool = false
    var enablesViewAreas: Bool = false
    var enablesCornerMasks: Bool = false
    var enablesLogTransfer: Bool = false
    var enablesMainBufferedAudio: Bool = false
    /// A non-empty `altVideoStreams[]` in the pushed config is what arms `altScreen` (box side:
    /// vehicle_config `alt_screen()`). The app tracks it as a single Bool (the stream contents are
    /// irrelevant to the feature echo — only presence).
    var altVideoStreamsPresent: Bool = false
    /// Mirrors the box's `view_areas_enabled()` auto-arm (verify_01 03-M2, `vehicle_config.rs:874`):
    /// the box negotiates `viewAreas` when EITHER `enablesViewAreas` is set OR the pushed main/alt
    /// stream defines a real (non-full-coverage) safe-area inset — an inset "just works" without the
    /// extra toggle. Computed by `VehicleConfigModel.config` from the SAME safe-area edge values the
    /// YAML emits, using the identical validity rule (`safe_area_inset` in the Rust twin): a rect
    /// that covers the whole panel is not an inset. Default false, so a config with no inset (0,0,0,0
    /// edges, the shipped default) stays byte-identical to today's `enabledFeatures()` output.
    var safeAreaInsetPresent: Bool = false
    /// Whether the pushed YAML actually requested app-driven SETUP. Not part of the feature echo;
    /// the AppDelegate reads it to decide whether to stand up the relay at all (default OFF).
    var appDrivenSetup: Bool = false

    // Stream / display geometry (carried so the author can sanity-check the negotiated geometry; the
    // box still owns every socket bind, so these are advisory here — the ports come from the relayed
    // local response, never re-invented host-side).
    var mainWidth: Int = 1920
    var mainHeight: Int = 1080
    var maxFPS: Int = 60
    var altWidth: Int = 800
    var altHeight: Int = 480
    var altFPS: Int = 30

    /// The `enabledFeatures` string array in session.rs's EXACT emission order
    /// (hevc, altScreen, viewAreas, cornerMasks, logTransfer, mainBuffered) so even an
    /// order-sensitive comparison matches the box. This is the Swift twin of
    /// `AuthoringConfig::enabled_features` — the gating must stay in lockstep with setup_driver.rs:
    ///   * hevc        <- enablesHEVC
    ///   * altScreen   <- altVideoStreams non-empty
    ///   * viewAreas   <- enablesViewAreas || enablesCornerMasks || safeAreaInsetPresent (03-M2 fix,
    ///                     mirrors the box's `view_areas_enabled()` inset auto-arm)
    ///   * cornerMasks <- enablesCornerMasks
    ///   * logTransfer <- enablesLogTransfer
    ///   * mainBuffered<- enablesMainBufferedAudio (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B4, app default OFF)
    /// session.rs emits two FURTHER tokens the host cannot author — iAPChannel and sessionManagement,
    /// gated on box-process env vars with no config key. They are NOT divergences: since the
    /// 2026-08-10 wireless flip `AirPlaySetupSession.authorPhase1` PRESERVES any token outside this
    /// vocabulary from the box's local response, which is what keeps the iAP2 tunnel alive on wireless.
    /// Mirrors `vehicle_config.rs`'s private `safe_area_inset(&SafeRect, panel_w, panel_h)` — the
    /// validity rule the box applies to decide whether a safe-area edge inset is a REAL inset (arms
    /// `viewAreas`) or a no-op full-panel rect (does not). `l/t/r/b` are the edge insets the Settings
    /// UI stores (never negative — `clampInsets` enforces that), `w`/`h` the panel/stream's pixel
    /// dimensions. Takes the SAME sx/sy/sw/sh derivation the YAML emitter's `va()` uses, so this is
    /// evaluated against exactly what is pushed on the wire, not an approximation.
    static func hasRealSafeAreaInset(left l: Int, top t: Int, right r: Int, bottom b: Int,
                                      width w: Int, height h: Int) -> Bool {
        let sx = max(0, l), sy = max(0, t)
        let sw = max(1, w - sx - max(0, r)), sh = max(1, h - sy - max(0, b))
        if sw <= 0 || sh <= 0 { return false }
        let coversFull = sx <= 0 && sy <= 0 && (sx + sw) >= w && (sy + sh) >= h
        return !coversFull
    }

    func enabledFeatures() -> [String] {
        var f: [String] = []
        if enablesHEVC { f.append("hevc") }
        if altVideoStreamsPresent { f.append("altScreen") }
        if enablesViewAreas || enablesCornerMasks || safeAreaInsetPresent { f.append("viewAreas") }
        if enablesCornerMasks { f.append("cornerMasks") }
        if enablesLogTransfer { f.append("logTransfer") }
        if enablesMainBufferedAudio { f.append("mainBuffered") }
        return f
    }
}

/// CRC-32 over the pushed YAML, so the app can tell whether the box is serving the config it thinks
/// it pushed (docs/carplay/04_CAPABILITIES_AND_CONFIG.md #6).
///
/// MUST match `ocbm-proto::crc32` byte for byte or the comparison is worse than useless — it would
/// report drift on every connection. That is the standard reflected CRC-32 (poly 0xEDB88320, init
/// 0xFFFF_FFFF, final complement), the same one used for the file push, verified against the
/// canonical "123456789" -> 0xCBF43926 check value in the test suite.
///
/// Lives HERE rather than next to its caller in AppDelegate.swift for the reason this file exists:
/// `tests/run_tests.sh` compiles this file and not that one.
enum CRC32 {
    private static let table: [UInt32] = (0..<256).map { i -> UInt32 in
        var c = UInt32(i)
        for _ in 0..<8 { c = (c & 1) != 0 ? (0xEDB8_8320 ^ (c >> 1)) : (c >> 1) }
        return c
    }

    static func compute(_ bytes: [UInt8]) -> UInt32 {
        var crc: UInt32 = 0xFFFF_FFFF
        for b in bytes {
            crc = (crc >> 8) ^ table[Int((crc ^ UInt32(b)) & 0xFF)]
        }
        return ~crc
    }

    static func compute(_ data: Data) -> UInt32 { compute([UInt8](data)) }
}

/// YAML emission helpers shared by every free-text field the app pushes to the box.
///
/// Lives HERE, not next to the emitter in `SettingsWindow.swift`, because `tests/run_tests.sh`
/// compiles this file and not that one — this placement is what makes the escaping testable without
/// dragging SwiftUI into the harness. (Moving it back would fail the harness compile loudly, not
/// silently drop coverage.)
enum YamlEmit {
    /// What we strip: Cc only — C0, DEL, C1. This is a deliberate SUPERSET of what the parser
    /// actually rejects: tab/LF/CR/NEL are inside these ranges but parse fine (they fold to
    /// spaces). They go anyway because these fields are single-line display text, where a folded
    /// newline is never what the user meant. Do NOT "correct" the set down to the true fatal list.
    /// Deliberately NOT `CharacterSet.controlCharacters`, which is Cc ∪ Cf: every Cf character
    /// (ZWJ, ZWSP, soft hyphen, LRM/RLM, BOM) parses fine, and stripping them silently mangles real
    /// content — an emoji ZWJ sequence collapses into separate glyphs, an RTL label can reorder.
    /// Verified codepoint-by-codepoint against the box's own `serde_yaml` 0.9.
    static let fatalControls: CharacterSet = {
        var cs = CharacterSet()
        cs.insert(charactersIn: Unicode.Scalar(0x00)!...Unicode.Scalar(0x1F)!)
        cs.insert(charactersIn: Unicode.Scalar(0x7F)!...Unicode.Scalar(0x9F)!)
        // U+FFFE/U+FFFF (verify_06 10-L3): unsafe-libyaml's reader rejects these noncharacters at
        // the stream level even though they are valid Swift Unicode.Scalars that survive
        // quotedBody's escaping — a pasted one fails the WHOLE pushed document, same failure class
        // as the Cc range above.
        cs.insert(Unicode.Scalar(0xFFFE)!)
        cs.insert(Unicode.Scalar(0xFFFF)!)
        return cs
    }()

    /// Render `s` as the BODY of a double-quoted YAML scalar (caller supplies the quotes).
    ///
    /// Two distinct hazards, both of which take out the WHOLE pushed document — not just the field —
    /// because the box's receiver then falls back to its built-in defaults for resolution, HEVC,
    /// appDrivenSetup and audio (`airplayd::load_device_config`'s parse-failure arm):
    ///
    /// 1. `"` and `\` must be escaped. An unescaped `"` closes the scalar early; an unescaped `\`
    ///    is worse than it looks — `\s` is an invalid escape and fails the parse loudly, but `\b`
    ///    is a VALID one and silently yields a backspace, corrupting the value with no error at all.
    ///    Escape order is LOAD-BEARING: backslash first, then quote — reversing it double-escapes
    ///    the backslashes the quote-pass introduces and re-opens the bug.
    /// 2. Cc control characters are rejected at the stream level, which no escaping at this site can
    ///    fix — so they are stripped (see [`fatalControls`]; Cf is deliberately preserved).
    ///
    /// Both facts verified against the box's own `serde_yaml` 0.9 and libyaml, not reasoned about.
    /// Strip the characters that would break a pushed document, WITHOUT touching Cf. Shared with
    /// the persistence path so the app has exactly one definition of "characters we remove".
    static func stripFatalControls(_ s: String) -> String {
        s.components(separatedBy: fatalControls).joined()
    }

    static func quotedBody(_ s: String) -> String {
        s.components(separatedBy: fatalControls)
            .joined()
            .replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
    }
}
