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
    /// The pushed main stream carries a SECOND view area (`viewAreas[1]`, the Dock resize button) —
    /// i.e. `VehicleConfigModel.viewArea2Enabled` AND the rect passed `ViewArea2Rule.verdict`. Mirrors
    /// the box's `view_areas_enabled()` second-area auto-arm (2026-09-05): a second area without the
    /// `viewAreas` feature in SETUP is a button that never appears, so it arms the feature the same
    /// way a real inset does. Default false: an unchanged config authors exactly today's feature set.
    var viewArea2Present: Bool = false
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
    ///                     || viewArea2Present (2026-09-05, the same auto-arm for a second main area)
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
        if enablesViewAreas || enablesCornerMasks || safeAreaInsetPresent || viewArea2Present { f.append("viewAreas") }
        if enablesCornerMasks { f.append("cornerMasks") }
        if enablesLogTransfer { f.append("logTransfer") }
        if enablesMainBufferedAudio { f.append("mainBuffered") }
        return f
    }
}

/// Legality of a SECOND main view area (the CarPlay Dock "resize" button) — the rules applied to
/// `viewAreas[1]`, evaluated UP FRONT because iOS reports NONE of them at declaration time: three
/// are session TEARDOWNS discovered only after RECORD, the fourth is a render-time lockout. Every
/// rule here is device-measured (2026-09-05, docs/carplay/06_AV_PIPELINE.md §3 "Bench lever") or an
/// owner decision, and that doc is the owning record — correct it there first. The model's
/// `viewArea2Verdict` is the contract the Settings view renders, and the YAML emitter refuses to
/// emit an entry this returns a verdict for.
///
/// Lives HERE (not in `SettingsWindow.swift`) for the same reason as `YamlEmit`: `tests/run_tests.sh`
/// and `tools/regen_app_yaml_fixture.py` compile this file and not the SwiftUI model, so the rule is
/// testable and the emitter it gates can be extracted verbatim.
///
/// The box re-validates containment (`info.rs::view_area_2`, the same gate the bench lever passes
/// through) and refuses loudly; parity and the product floor are app-side only.
enum ViewArea2Rule {
    /// PRODUCT FLOOR (owner decision, 2026-09-05): the smallest view area the app will offer is
    /// **800 x 480 in landscape and 480 x 800 in portrait**, for CarPlay AND Android Auto — the
    /// documented CarPlay/AA minimum display resolution. Orientation comes from the AREA'S OWN
    /// aspect (`w >= h` → landscape, `w < h` → portrait). This is a product constraint, NOT a
    /// device limit: iOS itself tolerates far smaller areas (measured floors near 350 x 312 on a
    /// 1080x1920 panel; below them it locks the area out black with `lockOutMode: viewAreaTooSmall`
    /// / "CarPlay does not support this display resolution", session surviving). Tolerance is not
    /// support — do NOT relax this against the measured numbers. A `385 * 0.65 * scale` formula
    /// extracted from the iOS 27 binaries was hardware-refuted three times and is deliberately gone.
    static let landscapeFloor = (width: 800, height: 480)
    static let portraitFloor = (width: 480, height: 800)

    /// The floor that applies to an area of this aspect: `w >= h` → 800 x 480, else 480 x 800.
    static func minimumSize(width w: Int, height h: Int) -> (width: Int, height: Int) {
        w >= h ? landscapeFloor : portraitFloor
    }

    /// `nil` = legal. Otherwise ONE message, the FIRST rule violated in SEVERITY order — the first
    /// three end the session, the fourth does not, and the UI must not present them alike:
    ///   1. containment — `x + w <= panelW && y + h <= panelH`. TEARDOWN, device-proven
    ///      (`1416x842@492,59` on a 1416x842 panel).
    ///   2. parity — `x`, `y`, `w`, `h` must ALL be even. TEARDOWN: `kFigEndpointError_InvalidParameter
    ///      -16720` from `carEndpoint_copyScreenInfo:7001` → `setupScreenStreams:8536` →
    ///      `setupStreams:8957` → `activateInternal:9987` → `Activate_block_invoke_2:10117`.
    ///      One-pixel isolation on 1080x1920: `356x400@240,760` renders, `357x400@240,760` dies;
    ///      `601x400@240,760` (odd width, far above any floor) dies; `600x400@240,761` (odd origin Y
    ///      only) dies. HEVC 4:2:0 chroma subsampling cannot express an odd extent. This is also what
    ///      killed the historical `1416x842@492,59` and `1600x842@800,59` (odd Y).
    ///   3. positive dimensions — `w > 0 && h > 0 && x >= 0 && y >= 0`. A zero dimension trips iOS's
    ///      `Pixel display view dimension(s) set to 0` validator → TEARDOWN.
    ///   4. the product floor (`minimumSize`) — LOCKOUT, not teardown: the area renders black with
    ///      "CarPlay does not support this display resolution" and the session survives.
    /// There is deliberately NO other origin rule: no must-touch-an-edge. A floating portrait area
    /// touching no edge (`1080x1600@0,160`) was accepted on hardware.
    /// Message style follows the insets verdict ("Insets leave too little room — …"); each names the
    /// failing value(s) and quotes the bound that applies to what the owner typed.
    static func verdict(x: Int, y: Int, w: Int, h: Int, panelW: Int, panelH: Int) -> String? {
        // Overflow-safe extents: these are free-typed Ints, and `x + w` on an absurd value would trap.
        let (right, ox) = x.addingReportingOverflow(w)
        let (bottom, oy) = y.addingReportingOverflow(h)
        if ox || oy || right > panelW || bottom > panelH {
            let ext = ox || oy ? "past the representable range" : "to \(right) × \(bottom) px"
            return "Area extends \(ext), outside the \(panelW) × \(panelH) panel — CarPlay tears the session down."
        }
        let odd = [("X", x), ("Y", y), ("width", w), ("height", h)].filter { $0.1 & 1 != 0 }
        if !odd.isEmpty {
            let list = odd.map { "\($0.0) \($0.1)" }.joined(separator: ", ")
            return "Odd value: \(list) — X, Y, width and height must all be even (HEVC 4:2:0 cannot encode an odd extent); "
                + "an odd value tears the session down."
        }
        if w <= 0 || h <= 0 || x < 0 || y < 0 {
            return "Width and height must be positive and the origin non-negative — a zero dimension is a session teardown."
        }
        let m = minimumSize(width: w, height: h)
        if w < m.width || h < m.height {
            let orientation = w >= h ? "landscape (width ≥ height)" : "portrait (width < height)"
            var failing: [String] = []
            if w < m.width { failing.append("width \(w) < \(m.width)") }
            if h < m.height { failing.append("height \(h) < \(m.height)") }
            return "Area too small — a \(orientation) area must be at least \(m.width) × \(m.height) px "
                + "(\(failing.joined(separator: ", "))); CarPlay locks it out with "
                + "“CarPlay does not support this display resolution”."
        }
        return nil
    }
}

/// The envelope a PANEL (the main or the alt video stream's `pixelDimensions`) may occupy, and the
/// ONE clamp that enforces it — `VehicleConfigModel.clampInPlace`, the Settings field colouring,
/// the import/preset pre-flight and the control-socket door all come here. Kept OUT of
/// `ViewArea2Rule` so the YAML-fixture harness (tools/regen_app_yaml_fixture.py extracts that enum
/// verbatim) does not grow a dependency; this rule depends on it, never the reverse.
///
/// ORIENTATION-AGNOSTIC since 2026-09-07. Each axis admits the same range, `minSide`…`maxSide`,
/// and the product floor is applied by ORIENTATION — chosen by the panel's OWN aspect, exactly as
/// `ViewArea2Rule.minimumSize` chooses it for a view area (`w >= h` → 800 x 480, else 480 x 800) —
/// never per axis. The previous bounds were per-axis (W 800–3840, H 480–2160): a landscape-shaped
/// envelope that silently squared a portrait 2160x3840 panel to 2160x2160, so every second view
/// area of the wired portrait sweep (e.g. `480x800@840,1520`) failed containment against a panel
/// 2160 tall, the emitter dropped it, and all five portrait cases came back INCONCLUSIVE while the
/// landscape half passed 5/5. Portrait is a first-class case, not a limit to preserve: Apple's own
/// Simulator ships `Portrait.yaml` at 900x1200, taller than its widest landscape template.
/// `maxSide` is NOT a measured iOS limit — 3840x2160 is device-proven on the wire (2026-09-07,
/// five view-area sizes accepted) and nothing above it has been tried.
enum PanelRule {
    /// Longest side admitted on EITHER axis.
    static let maxSide = 3840
    /// Shortest side admitted on either axis: the short side of the product floor. Derived from
    /// `ViewArea2Rule`'s floor so there is one source of truth for the number.
    static let minSide = min(ViewArea2Rule.landscapeFloor.height, ViewArea2Rule.portraitFloor.width)

    /// The floor for a panel of this aspect — `ViewArea2Rule`'s, unchanged.
    static func minimumSize(width w: Int, height h: Int) -> (width: Int, height: Int) {
        ViewArea2Rule.minimumSize(width: w, height: h)
    }

    /// The size the model will STORE for a requested size: orientation from the request's own
    /// aspect, then each axis into [floor axis, `maxSide`]. Overflow-safe on any `Int`. The floor
    /// for an orientation is itself of that orientation, so a clamp never flips a panel's aspect.
    static func clamped(width w: Int, height h: Int) -> (width: Int, height: Int) {
        let floor = minimumSize(width: w, height: h)
        return (min(max(w, floor.width), maxSide), min(max(h, floor.height), maxSide))
    }

    /// Which axes `clamped` would move, for the form to colour the offending field.
    static func axisOutOfRange(width w: Int, height h: Int) -> (width: Bool, height: Bool) {
        let c = clamped(width: w, height: h)
        return (c.width != w, c.height != h)
    }

    /// `nil` = inside the envelope. Otherwise ONE message naming the orientation the rule applied,
    /// the failing axis and the bound — same style as `ViewArea2Rule.verdict` and the insets line.
    static func verdict(width w: Int, height h: Int) -> String? {
        let c = clamped(width: w, height: h)
        guard c.width != w || c.height != h else { return nil }
        let floor = minimumSize(width: w, height: h)
        let orientation = w >= h ? "landscape (width ≥ height)" : "portrait (width < height)"
        var failing: [String] = []
        if w < floor.width { failing.append("width \(w) < \(floor.width)") }
        if h < floor.height { failing.append("height \(h) < \(floor.height)") }
        if w > maxSide { failing.append("width \(w) > \(maxSide)") }
        if h > maxSide { failing.append("height \(h) > \(maxSide)") }
        return "A \(orientation) panel must be \(floor.width) × \(floor.height) to \(maxSide) × \(maxSide) px "
            + "(\(failing.joined(separator: ", "))) — Save stores \(c.width) × \(c.height)."
    }

    /// One line naming what a clamp changed (`"Panel 2160 × 2160 → …"`), or nil when nothing moved.
    /// The honesty signal for a clamp — the same shape as `AACapability.negotiationNotes`.
    static func clampNote(_ label: String, requested r: (width: Int, height: Int),
                          applied a: (width: Int, height: Int)) -> String? {
        guard r.width != a.width || r.height != a.height else { return nil }
        let floor = minimumSize(width: r.width, height: r.height)
        let orientation = r.width >= r.height ? "landscape" : "portrait"
        return "\(label) \(r.width) × \(r.height) was outside the app's limits for a \(orientation) panel "
            + "(\(floor.width) × \(floor.height) to \(maxSide) × \(maxSide)) — stored as \(a.width) × \(a.height)."
    }

    /// The envelope as one phrase, for summaries that quote the limits.
    static let envelopeDescription = "each side \(minSide)–\(maxSide) px, at least "
        + "\(ViewArea2Rule.landscapeFloor.width) × \(ViewArea2Rule.landscapeFloor.height) landscape / "
        + "\(ViewArea2Rule.portraitFloor.width) × \(ViewArea2Rule.portraitFloor.height) portrait"
}

/// A view-area rect in the `WxH@X,Y` spelling the bench tools already use (`tools/va_limit_probe.sh`,
/// the box's `CARPLAY_VIEWAREA2` lever), parsed ONCE here for the ControlServer's `viewarea arm`.
///
/// Pure and strict: four non-negative decimal integers, no sign, no whitespace, no suffix. The
/// probe's `:initial` suffix is deliberately REFUSED rather than dropped — the app model does not
/// author `initial` (docs/ops/04_OPEN_ITEMS.md handoff, known-open 5), so silently accepting it
/// would arm a rect the caller believes starts the session in the area when it does not. Parsing
/// says nothing about legality; that is `ViewArea2Rule.verdict` against the panel.
struct ViewAreaSpec: Equatable {
    let x: Int
    let y: Int
    let w: Int
    let h: Int

    static func parse(_ spec: String) -> ViewAreaSpec? {
        // "WxH@X,Y" → ["WxH", "X,Y"] → ["W","H"], ["X","Y"]. `omittingEmptySubsequences: false` so
        // "x800@0,0" or "800x@0,0" yields an empty field that `Int()` rejects, instead of collapsing.
        let sizeOrigin = spec.split(separator: "@", omittingEmptySubsequences: false)
        guard sizeOrigin.count == 2 else { return nil }
        let size = sizeOrigin[0].split(separator: "x", omittingEmptySubsequences: false)
        let origin = sizeOrigin[1].split(separator: ",", omittingEmptySubsequences: false)
        guard size.count == 2, origin.count == 2 else { return nil }
        func field(_ s: Substring) -> Int? {
            // Decimal digits only: `Int("+800")` and `Int("-0")` parse, and neither is a spec.
            guard !s.isEmpty, s.allSatisfy({ $0.isASCII && $0.isNumber }) else { return nil }
            return Int(s)
        }
        guard let w = field(size[0]), let h = field(size[1]),
              let x = field(origin[0]), let y = field(origin[1]) else { return nil }
        return ViewAreaSpec(x: x, y: y, w: w, h: h)
    }

    /// The canonical spelling back out, so a reply can echo what was armed in the same grammar.
    var description: String { "\(w)x\(h)@\(x),\(y)" }
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

/// The one-shot `vc.profileKeysV1` UserDefaults migration, lifted out of `SettingsWindow.swift`.
///
/// It lives HERE, in a Foundation-only file, for one reason: `tests/run_tests.sh` cannot compile
/// `SettingsWindow.swift` (AppKit + SwiftUI + `@MainActor`), so while this logic sat on
/// `VehicleConfigModel` the §9.7 test could only run against a hand-copied duplicate of it — which
/// is precisely the drift hazard the test exists to catch. `VehicleConfigModel.migrateProfileKeysV1`
/// now delegates here, so the shipped code and the tested code are the same code.
///
/// Why this migration is worth testing at all: it derives the neutral `driverPosition`/`theme` keys
/// from the legacy `rightHandDrive`/`nightMode` booleans, runs EXACTLY ONCE per user (the
/// `profileKeysV1` sentinel), and can never re-run to correct itself. A wrong derivation silently
/// mis-seeds a saved config with no second chance — the same failure shape as the
/// `mbaDefaultFlippedB4` migration it is modelled on.
enum VehicleProfileKeyMigration {
    /// Seed `driverPosition`/`theme` from the legacy booleans, once. Takes the defaults domain as a
    /// parameter so a test can run it against a throwaway suite rather than the user's own.
    ///
    /// Idempotent by the sentinel, and per-key absent-only: a user who already set `driverPosition`
    /// explicitly is never overwritten by the boolean it was once derived from.
    static func run(prefix: String, ud: UserDefaults) {
        guard ud.object(forKey: prefix + "profileKeysV1") == nil else { return }
        if ud.object(forKey: prefix + "driverPosition") == nil {
            ud.set(ud.bool(forKey: prefix + "rightHandDrive") ? DriverPosition.right.rawValue
                                                              : DriverPosition.left.rawValue,
                   forKey: prefix + "driverPosition")
        }
        if ud.object(forKey: prefix + "theme") == nil {
            ud.set(ud.bool(forKey: prefix + "nightMode") ? AppearanceTheme.dark.rawValue
                                                         : AppearanceTheme.light.rawValue,
                   forKey: prefix + "theme")
        }
        ud.set(true, forKey: prefix + "profileKeysV1")
    }
}
