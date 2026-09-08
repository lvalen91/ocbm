// ──────────────────────────────────────────────────────────────────────────────
// tests/SettingsTests.swift — Settings reorganisation test contract (DESIGN.md §9)
// ──────────────────────────────────────────────────────────────────────────────
// Hardware-free. Compiled by tests/run_tests.sh together with the live contract
// files (`App/Settings/VehicleProfile.swift`, `App/Settings/FeatureMatrix.swift`)
// and `AA/AACapability.swift`; uses the `check`/`section` helpers from
// tests/main.swift. Entry point: `runSettingsTests()`.
//
// Covers, in DESIGN.md §9 numbering:
//   1. document encode → decode → encode is byte-equal and `==`
//   2. every built-in preset round-trips; ids unique; `shipped` == `.default`
//   3. schemaVersion tolerance (minimal v1 doc → default; newer schema throws)
//   4. restriction mapping in both directions (CarPlay limitedUI / AA driving_status)
//   5. every Feature × Projection has a non-empty effect and vendorTerm
//   6. `AACapability(profile:adapter:)` renders the shipped default identically to
//      the pre-refactor snapshot, plus the §8 neutral→AA rules; the T4 geometry edge
//      cases and per-(tier, fps) verification notes; the `AA_HEVC` / `AA_NO_TOUCH`
//      bench levers (scoped setenv, environment restored)
//   7. the `vc.profileKeysV1` one-shot migration — see the `#if` note at the end:
//      it needs `VehicleConfigModel`, which this harness does not compile.
//
// Nothing here touches UserDefaults.standard, the box, or the network.
// ──────────────────────────────────────────────────────────────────────────────

import Foundation

// MARK: - Entry point

func runSettingsTests() {
    settingsDocumentRoundTripTests()
    settingsPresetTests()
    settingsSchemaVersionTests()
    settingsDocumentIOTests()
    settingsRestrictionMappingTests()
    settingsFeatureMatrixTests()
    settingsNeutralProfileTests()
    settingsAARendererTests()
    settingsAAGeometryTests()
    settingsAALeverTests()
    settingsAudioVocabularyTests()
    settingsHostileInputTests()
    settingsMigrationTests()
    settingsViewArea2RuleTests()
    settingsPanelRuleTests()
    settingsViewAreaSpecTests()
}

// MARK: - Helpers (file-private; names prefixed so they cannot collide with main.swift)

private func settingsEncode(_ doc: VehicleProfileDocument, _ what: String) -> Data? {
    do { return try VehicleProfileDocument.encode(doc) } catch {
        check(false, "\(what): encode threw \(error)")
        return nil
    }
}

private func settingsDecode(_ data: Data, _ what: String) -> VehicleProfileDocument? {
    do { return try VehicleProfileDocument.decode(data) } catch {
        check(false, "\(what): decode threw \(error)")
        return nil
    }
}

/// encode → decode → encode; asserts byte-equality and value-equality. Returns the decoded doc.
@discardableResult
private func settingsAssertRoundTrip(_ doc: VehicleProfileDocument, _ what: String) -> VehicleProfileDocument? {
    guard let bytes1 = settingsEncode(doc, what) else { return nil }
    guard let decoded = settingsDecode(bytes1, what) else { return nil }
    guard let bytes2 = settingsEncode(decoded, "\(what) (second pass)") else { return nil }
    check(decoded == doc, "\(what): decoded document == original")
    check(bytes1 == bytes2, "\(what): encode(decode(encode(doc))) is byte-identical (\(bytes1.count) B)")
    return decoded
}

private func settingsDecodeError(_ json: String) -> VehicleProfileDocument.DocumentError? {
    do {
        _ = try VehicleProfileDocument.decode(Data(json.utf8))
        return nil
    } catch let e as VehicleProfileDocument.DocumentError {
        return e
    } catch {
        return nil
    }
}

// MARK: - §9.1 Document round-trip determinism

private func settingsDocumentRoundTripTests() {
    section("Settings — VehicleProfileDocument round-trip (§9.1)")

    let def = VehicleProfileDocument()
    check(def.schemaVersion == VehicleProfileDocument.currentSchemaVersion, "default document carries the current schemaVersion")
    check(def.preset == nil, "default document has no preset tag")
    check(def.vehicle == .default && def.adapter == .default, "default document wraps VehicleProfile.default + AdapterSettings.default")
    check(VehicleProfileDocument.fileExtension == "vehicleprofile.json", "fileExtension is 'vehicleprofile.json'")

    settingsAssertRoundTrip(def, "default document")

    // Determinism: two independent encodes of equal values are byte-identical (sortedKeys).
    if let a = settingsEncode(def, "det A"), let b = settingsEncode(VehicleProfileDocument(), "det B") {
        check(a == b, "two encodes of equal documents are byte-identical")
        let text = String(decoding: a, as: UTF8.self)
        check(text.contains("\"schemaVersion\" : 1"), "encoded document is pretty-printed with schemaVersion 1")
        check(text.hasPrefix("{\n"), "encoded document is a pretty-printed JSON object")
    }

    // A non-default, every-block-touched document must round-trip too, including the base64 icon
    // (which is why slashes are not escaped) and the optional fields.
    var v = VehicleProfile.default
    v.identity.headUnitName = "Bench \"quoted\" unit / slash"
    v.branding = Branding(advertise: true, visible: true, label: "Bench",
                          icon: BrandIcon(pngBase64: "iVBORw0KGgo=//+/AAAA", width: 64, height: 64))
    v.display.panel = PanelGeometry(width: 2560, height: 1440, maxFPS: 30, dpi: 220, diagonalInches: 12.3)
    v.display.insets = PanelInsets(left: 8, top: 4, right: 8, bottom: 12)
    v.display.drawUIOutsideInsets = true
    v.altDisplay.enabled = true
    v.video.hevcAllowed = false
    v.appearance.theme = .dark
    v.appearance.statusBar = StatusBarPolicy(hideClock: true, hideSignal: false, hideBattery: true)
    v.driverPosition = .center
    v.restrictions = DrivingRestrictionPolicy(declared: true, set: [.keyboard, .video, .configuration])
    v.powertrain = Powertrain(engines: [.electric, .gasoline],
                              connectors: [ConnectorSpec(type: .nacsDC, powerWatts: 250_000), ConnectorSpec(type: .ccs1)])
    v.input = InputDevices(primary: .rotary, touchscreen: nil, touchpad: Touchpad(buttons: true),
                           rotaryKnob: RotaryKnob(homeAndBackButtons: true, nudge: true),
                           dPad: false, mediaButtons: true, telephonyButtons: true, steeringWheelButtons: true)
    v.audio = AudioProfile(voiceRateHz: 16000, telephonyOverProjection: true)
    v.metadata = MetadataFeeds(nowPlaying: false, navigation: true, telephony: false)
    v.carPlay.accessoryName = "Bench accessory"
    v.carPlay.metadataTier = "extended"
    v.androidAuto = AndroidAutoExtensions(fitPanelWithMargins: false, preferHEVC: true)
    var a = AdapterSettings.default
    a.wirelessRadios = false; a.wifiAccessPoint = false; a.pairingNumericComparison = true
    let rich = VehicleProfileDocument(preset: "bench", vehicle: v, adapter: a)

    if let back = settingsAssertRoundTrip(rich, "every-block document") {
        check(back.preset == "bench", "preset tag survives the round-trip")
        check(back.vehicle.branding.icon?.pngBase64 == "iVBORw0KGgo=//+/AAAA", "base64 icon with '/' survives unescaped")
        check(back.vehicle.display.panel.diagonalInches == 12.3, "optional diagonalInches survives")
        check(back.vehicle.restrictions.set == [.keyboard, .video, .configuration], "restriction OptionSet survives as its raw mask")
        check(back.vehicle.input.touchscreen == nil && back.vehicle.input.touchpad?.buttons == true, "optional input devices survive (nil stays nil)")
        check(back.adapter.wifiAccessPoint == false, "adapter wifiAccessPoint=false survives")
    }
    if let bytes = settingsEncode(rich, "slash check") {
        check(!String(decoding: bytes, as: UTF8.self).contains("\\/"), "encoder does not escape '/' (withoutEscapingSlashes)")
    }
}

// MARK: - §9.2 Presets

private func settingsPresetTests() {
    section("Settings — VehicleProfilePreset.builtIn (§9.2)")

    let presets = VehicleProfilePreset.builtIn
    check(presets.count == 16, "16 built-in presets (got \(presets.count))")
    check(Set(presets.map(\.id)).count == presets.count, "preset ids are unique")
    check(presets.allSatisfy { !$0.title.isEmpty && !$0.summary.isEmpty }, "every preset has a title and a summary")

    var project = 0, dhu = 0, apple = 0
    for p in presets {
        switch p.origin {
        case .project: project += 1
        case .desktopHeadUnit(let f): dhu += 1; check(f.hasSuffix(".ini"), "DHU preset \(p.id) names an .ini file")
        case .carPlaySimulator(let f): apple += 1; check(f.hasSuffix(".yaml"), "Apple preset \(p.id) names a .yaml file")
        }
    }
    check(project == 1 && dhu == 10 && apple == 5, "origins: 1 project, 10 DHU, 5 Apple (got \(project)/\(dhu)/\(apple))")

    guard let shipped = VehicleProfilePreset.named("shipped") else {
        check(false, "preset 'shipped' exists")
        return
    }
    check(shipped.vehicle == .default && shipped.adapter == .default, "'shipped' preset == VehicleProfile.default + AdapterSettings.default")
    check(shipped.document == VehicleProfileDocument(preset: "shipped"), "'shipped'.document is the default document tagged 'shipped'")
    check(VehicleProfilePreset.named("no-such-preset") == nil, "named(_:) returns nil for an unknown id")
    check(presets.first?.id == "shipped", "'shipped' is the first preset (the picker's default row)")

    for p in presets {
        if let back = settingsAssertRoundTrip(p.document, "preset \(p.id)") {
            check(back.preset == p.id, "preset \(p.id): document.preset == id")
            check(back.vehicle == p.vehicle && back.adapter == p.adapter, "preset \(p.id): vehicle/adapter survive")
        }
    }

    // Spot-check the catalogue facts DESIGN.md §8 relies on (a preset below the CarPlay floor
    // exists and is clamped on apply, and the DHU wide preset is the margins demo).
    if let six = VehicleProfilePreset.named("dhu-6in") {
        check(six.vehicle.display.panel.width == 750 && six.vehicle.display.panel.height == 450,
              "dhu-6in is 750×450 (below the 800×480 CarPlay floor; W1 clamps on apply)")
    } else { check(false, "preset 'dhu-6in' exists") }
    if let wide = VehicleProfilePreset.named("dhu-wide") {
        check(wide.vehicle.display.panel.width == 1280 && wide.vehicle.display.panel.height == 500,
              "dhu-wide is 1280×500 (not an AA tier → margins)")
    } else { check(false, "preset 'dhu-wide' exists") }
    if let d = VehicleProfilePreset.named("dhu-default") {
        check(d.vehicle.display.panel == PanelGeometry(width: 800, height: 480, maxFPS: 30),
              "dhu-default is 800×480 @30 dpi 160 (Google's reference head unit)")
    } else { check(false, "preset 'dhu-default' exists") }
}

// MARK: - §9.3 schemaVersion tolerance

private func settingsSchemaVersionTests() {
    section("Settings — schemaVersion tolerance (§9.3)")

    check(VehicleProfileDocument.currentSchemaVersion == 1, "current schema is v1")
    check(VehicleProfileDocument.migrationSteps().isEmpty, "no migration steps at v1")

    if let minimal = settingsDecode(Data("{\"schemaVersion\":1}".utf8), "minimal v1") {
        check(minimal == VehicleProfileDocument(), "{\"schemaVersion\":1} decodes to the default document")
    }

    // Partial documents: a missing top-level block takes its default; unknown keys are ignored.
    if let partial = settingsDecode(Data("{\"schemaVersion\":1,\"preset\":\"x\",\"future\":{\"k\":1}}".utf8), "partial v1") {
        check(partial.preset == "x" && partial.vehicle == .default && partial.adapter == .default,
              "missing vehicle/adapter blocks take defaults; unknown top-level keys are ignored")
    }

    let newer = settingsDecodeError("{\"schemaVersion\":99}")
    check(newer == .newerSchema(found: 99, supported: 1), "schemaVersion 99 throws .newerSchema(found: 99, supported: 1) (got \(String(describing: newer)))")
    check(settingsDecodeError("{}") == .missingSchemaVersion, "a document without schemaVersion throws .missingSchemaVersion")
    check(settingsDecodeError("{\"schemaVersion\":\"1\"}") == .missingSchemaVersion, "a non-integer schemaVersion throws .missingSchemaVersion")
    check(settingsDecodeError("[1,2,3]") == .notAnObject, "a JSON array throws .notAnObject")
    do {
        _ = try VehicleProfileDocument.decode(Data("not json".utf8))
        check(false, "malformed JSON must throw")
    } catch is VehicleProfileDocument.DocumentError {
        check(false, "malformed JSON is Foundation's error, not a DocumentError")
    } catch { check(true, "malformed JSON throws Foundation's error") }

    // Idempotence of decode on already-current output (the ladder is a no-op at v1).
    if let bytes = settingsEncode(VehicleProfileDocument(preset: "shipped"), "idempotence"),
       let once = settingsDecode(bytes, "idempotence 1"),
       let again = settingsEncode(once, "idempotence 2").flatMap({ settingsDecode($0, "idempotence 3") }) {
        check(once == again, "decode is idempotent on current-version output")
    }

    // The error type is descriptive (the UI shows `description`).
    let d = VehicleProfileDocument.DocumentError.newerSchema(found: 2, supported: 1).description
    check(d.contains("2") && d.contains("newer"), "DocumentError.newerSchema description names both versions")
}

// MARK: - ProfileDocumentIO (W1, Foundation-only): file read/write round-trip

private func settingsDocumentIOTests() {
    section("Settings — ProfileDocumentIO read/write (§9.1 on disk)")

    let dir = FileManager.default.temporaryDirectory
        .appendingPathComponent("carlink-settings-tests-\(UUID().uuidString)", isDirectory: true)
    do { try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true) } catch {
        check(false, "could not create temp dir: \(error)")
        return
    }
    defer { try? FileManager.default.removeItem(at: dir) }

    guard let wide = VehicleProfilePreset.named("dhu-wide") else { check(false, "preset 'dhu-wide' exists"); return }
    let doc = wide.document
    let url = dir.appendingPathComponent("wide.\(VehicleProfileDocument.fileExtension)")

    do {
        try ProfileDocumentIO.write(doc, to: url)
        let onDisk = try Data(contentsOf: url)
        let expected = try VehicleProfileDocument.encode(doc)
        check(onDisk == expected, "written bytes == VehicleProfileDocument.encode(doc) (\(onDisk.count) B)")
        let back = try ProfileDocumentIO.read(from: url)
        check(back == doc, "read(from:) returns the written document")
        check(back.preset == "dhu-wide", "preset tag survives the file round-trip")

        // Overwrite in place (atomic write replaces, never appends).
        try ProfileDocumentIO.write(VehicleProfileDocument(), to: url)
        let replaced = try ProfileDocumentIO.read(from: url)
        check(replaced == VehicleProfileDocument(), "overwriting a document replaces it")
        check(try Data(contentsOf: url) == VehicleProfileDocument.encode(VehicleProfileDocument()), "overwritten file is exactly the new encoding")
        check((try? FileManager.default.contentsOfDirectory(atPath: dir.path))?.count == 1, "atomic write leaves no temp file behind")
    } catch {
        check(false, "ProfileDocumentIO round-trip threw \(error)")
    }

    // Malformed inputs map to IOError.malformed with the file name and a reason.
    func ioError(_ text: String, _ name: String) -> ProfileDocumentIO.IOError? {
        let u = dir.appendingPathComponent(name)
        do { try Data(text.utf8).write(to: u) } catch { check(false, "could not write \(name): \(error)"); return nil }
        do { _ = try ProfileDocumentIO.read(from: u); return nil } catch let e as ProfileDocumentIO.IOError { return e } catch { return nil }
    }
    if case let .malformed(file, reason)? = ioError("{\"schemaVersion\":99}", "newer.json") {
        check(file == "newer.json" && reason.contains("newer"), "newer schema → IOError.malformed(file: newer.json, reason mentions 'newer') (got \(reason))")
    } else { check(false, "newer schema file throws IOError.malformed") }
    if case let .malformed(_, reason)? = ioError("[]", "array.json") {
        check(reason.contains("not a JSON object"), "JSON array → malformed with the DocumentError description (got \(reason))")
    } else { check(false, "JSON array file throws IOError.malformed") }
    // A complete document with one bad enum value: the reason names the coding path.
    if let full = settingsEncode(VehicleProfileDocument(), "bad enum base") {
        let text = String(decoding: full, as: UTF8.self)
        check(text.contains("\"driverPosition\" : \"left\""), "encoded default carries driverPosition 'left'")
        let broken = text.replacingOccurrences(of: "\"driverPosition\" : \"left\"", with: "\"driverPosition\" : \"sideways\"")
        if case let .malformed(_, reason)? = ioError(broken, "badenum.json") {
            check(reason.contains("driverPosition"), "bad enum value → malformed naming the coding path (got \(reason))")
        } else { check(false, "bad enum file throws IOError.malformed") }
    }
    // INVERTED 2026-09-04. This used to assert the opposite — that a partial `vehicle` block was
    // REJECTED with "missing key", i.e. that tolerance stopped at the top level. That behaviour
    // contradicted the contract the document's own comments and DESIGN.md advertised ("additive
    // fields need no schema bump"), and it meant one new nested field would invalidate every
    // profile a user had already exported. Nested decoding is now genuinely tolerant, so the test
    // pins the CONTRACT rather than the accident.
    if let doc = try? VehicleProfileDocument.decode(Data("{\"schemaVersion\":1,\"vehicle\":{\"driverPosition\":\"right\"}}".utf8)) {
        check(doc.vehicle.driverPosition == .right, "a partial vehicle block keeps the key it does carry")
        check(doc.vehicle.appearance.theme == VehicleProfile.default.appearance.theme,
              "a partial vehicle block defaults every key it omits, at any depth")
        check(doc.adapter == AdapterSettings.default, "an omitted top-level block is still defaulted whole")
    } else {
        check(false, "a partial vehicle block decodes (nested keys are optional, not all-or-nothing)")
    }
    // At DEPTH (2026-09-04 fix: nested `Codable` used to demand every key). A partial object fills
    // from the defaults at that depth; an optional sub-object the document omits stays nil (absence
    // must never conjure a touchscreen); one it carries as `{}` takes that object's defaults; array
    // rows fill per element; a key with no default (`ConnectorSpec.type`) stays required.
    let deep = "{\"schemaVersion\":1,\"vehicle\":{\"display\":{\"panel\":{\"width\":1280}},"
        + "\"input\":{\"primary\":\"touchpad\",\"touchpad\":{}},"
        + "\"carPlay\":{\"audioFormats\":[{\"streamType\":100}]}}}"
    if let doc = try? VehicleProfileDocument.decode(Data(deep.utf8)) {
        check(doc.vehicle.display.panel == PanelGeometry(width: 1280, height: 1080, maxFPS: 60, dpi: 160),
              "panel.width alone: height/maxFPS/dpi fill from the defaults two levels down (got \(doc.vehicle.display.panel))")
        check(doc.vehicle.display.insets == PanelInsets() && doc.vehicle.display.drawUIOutsideInsets == VehicleProfile.default.display.drawUIOutsideInsets,
              "siblings omitted beside a partial panel take their defaults")
        check(doc.vehicle.input.primary == .touchpad && doc.vehicle.input.touchscreen == nil,
              "input block present without 'touchscreen' ⇒ nil, not the default touchscreen")
        check(doc.vehicle.input.touchpad == Touchpad() && doc.vehicle.input.rotaryKnob == nil,
              "'touchpad': {} ⇒ a touchpad with default settings; omitted rotaryKnob stays nil")
        check(doc.vehicle.input.dPad == InputDevices().dPad && doc.vehicle.input.mediaButtons == InputDevices().mediaButtons,
              "omitted scalar keys inside a present input block take their defaults")
        check(doc.vehicle.carPlay.audioFormats == [CarPlayExtensions.AudioFormat(streamType: 100)],
              "an audioFormats row carrying only streamType fills the row defaults (got \(doc.vehicle.carPlay.audioFormats))")
        check(doc.vehicle.carPlay.metadataTier == "proven", "carPlay block present without metadataTier ⇒ default")
    } else { check(false, "a document partial at depth 2–3 decodes") }
    if case let .malformed(_, reason)? = ioError("{\"schemaVersion\":1,\"vehicle\":{\"powertrain\":{\"connectors\":[{\"powerWatts\":1}]}}}", "conn.json") {
        check(reason.contains("type"), "a connector without 'type' is rejected at its path, never invented (got \(reason))")
    } else { check(false, "a connector without 'type' throws IOError.malformed") }
    if case .malformed? = ioError("nope", "garbage.json") {
        check(true, "non-JSON → IOError.malformed")
    } else { check(false, "non-JSON file throws IOError.malformed") }

    // A missing file surfaces the filesystem error, not a malformed-document one.
    do {
        _ = try ProfileDocumentIO.read(from: dir.appendingPathComponent("missing.json"))
        check(false, "reading a missing file throws")
    } catch is ProfileDocumentIO.IOError {
        check(false, "a missing file is a filesystem error, not IOError.malformed")
    } catch {
        check(true, "missing file throws the filesystem error")
    }
    let e = ProfileDocumentIO.IOError.malformed(file: "x.json", reason: "why")
    check(e.errorDescription == "x.json is not a usable vehicle profile: why", "IOError.errorDescription names file and reason")
}

// MARK: - §9.4 Restriction mapping, both directions

private func settingsRestrictionMappingTests() {
    section("Settings — restriction mapping (§9.4)")

    let typical = DrivingRestrictionSet.typicalDriving
    check(typical == [.keyboard, .phoneKeypad, .longMessages, .configuration], "typicalDriving = keyboard+phoneKeypad+longMessages+configuration")
    check(typical.rawValue == (1 | 2 | 16 | 128), "typicalDriving raw mask is 147")

    // Neutral → Android Auto driving_status.
    check(FeatureMatrix.androidAutoDrivingStatus(typical) == 26, "androidAutoDrivingStatus(.typicalDriving) == 26")
    check(UInt64(FeatureMatrix.androidAutoDrivingStatus(typical)) == AACapability.DrivingRestrictions.drivingDefault.rawValue,
          "androidAutoDrivingStatus(.typicalDriving) == AACapability.DrivingRestrictions.drivingDefault (\(AACapability.DrivingRestrictions.drivingDefault.rawValue))")
    check(FeatureMatrix.androidAutoDrivingStatus([]) == 0, "empty set → driving_status 0")
    let all = DrivingRestrictionSet(rawValue: 0xFF)
    check(FeatureMatrix.androidAutoDrivingStatus(all) == 31, "all eight restrictions → driving_status 31 (fullyRestricted)")
    check(UInt64(FeatureMatrix.androidAutoDrivingStatus(all)) == AACapability.DrivingRestrictions.fullyRestricted.rawValue,
          "all eight → AACapability.DrivingRestrictions.fullyRestricted")
    check(FeatureMatrix.androidAutoDrivingStatus(.video) == 1, "video → NO_VIDEO (1)")
    check(FeatureMatrix.androidAutoDrivingStatus(.keyboard) == 2 && FeatureMatrix.androidAutoDrivingStatus(.phoneKeypad) == 2,
          "keyboard and phoneKeypad both → NO_KEYBOARD_INPUT (2); AA has one bit for both")
    check(FeatureMatrix.androidAutoDrivingStatus(.voiceInput) == 4, "voiceInput → NO_VOICE_INPUT (4)")
    check(FeatureMatrix.androidAutoDrivingStatus(.configuration) == 8, "configuration → NO_CONFIG (8)")
    check(FeatureMatrix.androidAutoDrivingStatus(.longMessages) == 16, "longMessages → LIMIT_MESSAGE_LEN (16)")
    check(FeatureMatrix.androidAutoDrivingStatus([.mediaLists, .otherLists]) == 0, "mediaLists/otherLists have no AA bit → 0")

    // Neutral → CarPlay limitedUI elements.
    check(FeatureMatrix.carPlayLimitedUIElements(typical) == ["softKeyboard", "softPhoneKeypad", "longAlerts"],
          "carPlayLimitedUIElements(.typicalDriving) == [softKeyboard, softPhoneKeypad, longAlerts]")
    check(FeatureMatrix.carPlayLimitedUIElements([]) == [], "empty set → no CarPlay elements")
    check(FeatureMatrix.carPlayLimitedUIElements(all) == ["softKeyboard", "softPhoneKeypad", "musicLists", "nonMusicLists", "longAlerts"],
          "all eight → the five Apple limitedUI elements, in mapping order")
    check(FeatureMatrix.carPlayLimitedUIElements([.video, .voiceInput, .configuration]) == [],
          "video/voiceInput/configuration have no CarPlay element")

    // Both directions via the mapping table itself: each singleton maps to exactly its own cell.
    let mapping = FeatureMatrix.restrictionMapping
    check(mapping.count == 8, "restrictionMapping has 8 rows (the neutral set has 8 members, DESIGN.md §10)")
    check(Set(mapping.map(\.restriction.rawValue)).count == 8, "mapping rows are distinct restrictions")
    check(mapping.reduce(DrivingRestrictionSet()) { $0.union($1.restriction) } == all, "mapping rows cover every bit of the 8-bit set")
    for m in mapping {
        let single = m.restriction
        check(FeatureMatrix.carPlayLimitedUIElements(single) == (m.carPlayElement.map { [$0] } ?? []),
              "row \(m.title): CarPlay element round-trips")
        check(FeatureMatrix.androidAutoDrivingStatus(single) == (m.androidAutoBit ?? 0),
              "row \(m.title): AA bit round-trips")
        check(!m.title.isEmpty, "row \(m.title): has a title")
    }
    // Reverse direction: from a CarPlay element list / AA mask back to the neutral set, via the table.
    func neutral(fromCarPlay elements: [String]) -> DrivingRestrictionSet {
        mapping.filter { $0.carPlayElement.map(elements.contains) ?? false }.reduce(DrivingRestrictionSet()) { $0.union($1.restriction) }
    }
    func neutral(fromAndroidAuto mask: UInt32) -> DrivingRestrictionSet {
        mapping.filter { $0.androidAutoBit.map { mask & $0 != 0 } ?? false }.reduce(DrivingRestrictionSet()) { $0.union($1.restriction) }
    }
    check(neutral(fromCarPlay: ["softKeyboard", "softPhoneKeypad", "longAlerts"]) == [.keyboard, .phoneKeypad, .longMessages],
          "CarPlay [softKeyboard, softPhoneKeypad, longAlerts] → keyboard+phoneKeypad+longMessages")
    check(neutral(fromAndroidAuto: 26) == [.keyboard, .phoneKeypad, .longMessages, .configuration],
          "AA mask 26 → keyboard+phoneKeypad+longMessages+configuration (== typicalDriving)")
    check(neutral(fromAndroidAuto: 26) == typical, "AA 26 reverse-maps to typicalDriving exactly")
    check(neutral(fromAndroidAuto: 2) == [.keyboard, .phoneKeypad], "AA bit 2 alone reverse-maps to BOTH keyboard and phoneKeypad (lossy by design)")

    // What each projection cannot express — what the UI must surface as 'not sent'.
    let cpUnexpressed = FeatureMatrix.unexpressedRestrictions(typical, on: .carPlay).map(\.restriction)
    check(cpUnexpressed == [.configuration], "typicalDriving on CarPlay: only 'configuration' is unexpressed")
    check(FeatureMatrix.unexpressedRestrictions(typical, on: .androidAuto).isEmpty, "typicalDriving on Android Auto: fully expressed")
    check(FeatureMatrix.unexpressedRestrictions([.mediaLists, .otherLists], on: .androidAuto).count == 2,
          "media/other lists are unexpressed on Android Auto")
    check(FeatureMatrix.unexpressedRestrictions([], on: .carPlay).isEmpty && FeatureMatrix.unexpressedRestrictions([], on: .androidAuto).isEmpty,
          "empty set is unexpressed nowhere")

    // The policy type persists its set as the raw mask and defaults to 'not declared'.
    let policy = DrivingRestrictionPolicy()
    check(policy.declared == false && policy.set.isEmpty, "DrivingRestrictionPolicy default: declared=false, empty set")
    do {
        let enc = JSONEncoder(); enc.outputFormatting = [.sortedKeys]
        let bytes = try enc.encode(DrivingRestrictionPolicy(declared: true, set: typical))
        let back = try JSONDecoder().decode(DrivingRestrictionPolicy.self, from: bytes)
        check(back.declared && back.set == typical, "DrivingRestrictionPolicy round-trips through JSON")
    } catch { check(false, "DrivingRestrictionPolicy JSON threw \(error)") }
}

// MARK: - §9.5 FeatureMatrix completeness

private func settingsFeatureMatrixTests() {
    section("Settings — FeatureMatrix completeness (§9.5)")

    let features = Feature.allCases
    check(features.count == 22, "22 features (got \(features.count))")
    check(Feature.Section.allCases.count == 9, "9 sections (got \(Feature.Section.allCases.count))")
    check(Projection.allCases == [.carPlay, .androidAuto], "two projections, CarPlay first")
    check(Projection.carPlay.displayName == "CarPlay" && Projection.androidAuto.displayName == "Android Auto", "projection display names")
    check(Projection.androidAuto.schemaName.contains("Desktop Head Unit"), "AA schemaName names the Desktop Head Unit (.ini) as the vendor schema")
    check(Projection.carPlay.schemaName.contains("CarPlay Simulator"), "CarPlay schemaName names the CarPlay Simulator YAML")

    // Per cell: what the matrix must SAY. Every check here fails on a one-line table edit. The
    // forwarders (`isAvailable`, `supports`, `unavailableReason`, `isUniform`, `Section.tab`,
    // `features(in:)`) are no longer restated cell by cell — R6's audit (2026-09-04) counted ~340
    // such restatements in this loop that could only fail in lockstep with the source.
    for f in features {
        check(!f.title.isEmpty && !f.summary.isEmpty, "\(f): title + summary present")
        for p in Projection.allCases {
            let s = FeatureMatrix.support(f, on: p)
            check(!s.effect.isEmpty, "\(f) × \(p): non-empty effect")
            check(!s.vendorTerm.isEmpty, "\(f) × \(p): non-empty vendorTerm")
            check(!s.verification.note.isEmpty, "\(f) × \(p): verification carries a note")
            if s.level == .unsupported {
                check(s.vendorKey == nil, "\(f) × \(p): unsupported ⇒ no vendorKey")
            }
            switch s.verification.status {
            case .deviceProven, .refuted:
                check(s.verification.date != nil, "\(f) × \(p): proven/refuted verification is dated")
            case .unverified:
                check(s.verification.date == nil, "\(f) × \(p): unverified verification has no date")
            }
        }
    }

    // isUniform gates the per-protocol explanation rows. It is NOT "both levels equal": three
    // features are `limited` on both sides and each still needs its rows, because the two limits
    // differ. The uniform set is exactly the three features both protocols express in full.
    // `driverPosition` JOINED this set on 2026-09-05, and the test catching that is the point of
    // pinning a set rather than restating `isUniform`'s definition: CarPlay went from `.unsupported`
    // to `.supported` once `rightHandDrive` was implemented as the Apple `/info` key it always was,
    // so both projections now express driver side in full. A restatement-style assertion would have
    // stayed green through that change and told us nothing.
    let uniform = features.filter(FeatureMatrix.isUniform)
    check(uniform == [.frameRate, .driverPosition, .wirelessRadios, .wifiAccessPoint],
          "exactly frameRate/driverPosition/wirelessRadios/wifiAccessPoint are uniform (got \(uniform))")
    for f in [Feature.headUnitName, .drivingRestrictions, .inputDevices] {
        let levels = Projection.allCases.map { FeatureMatrix.support(f, on: $0).level }
        check(levels == [.limited, .limited] && !FeatureMatrix.isUniform(f),
              "\(f) is limited on BOTH projections and still not uniform (equal levels ≠ same behaviour)")
    }
    // The UI's availability idiom: a reason exists exactly for a cell the protocol cannot express.
    check(FeatureMatrix.unavailableReason(.appDrivenSetup, on: .androidAuto)?.isEmpty == false
          && FeatureMatrix.unavailableReason(.appDrivenSetup, on: .carPlay) == nil,
          "unavailableReason says why appDrivenSetup is CarPlay-only, and is nil where it is available")
    // Tab placement is a total partition: every feature lands on Vehicle or Adapter, both tabs are
    // used, and nothing is routed to Diagnostics (which carries no feature controls).
    let tabs = Set(features.map { $0.section.tab })
    check(tabs == [.vehicle, .adapter], "every feature is on the Vehicle or Adapter tab, both non-empty, none on Diagnostics (got \(tabs))")

    // features(in:) partitions allCases exactly once.
    let regrouped = Feature.Section.allCases.flatMap { FeatureMatrix.features(in: $0) }
    check(regrouped.count == features.count && Set(regrouped) == Set(features), "features(in:) partitions every feature exactly once")
    check(Feature.Section.allCases.allSatisfy { !FeatureMatrix.features(in: $0).isEmpty }, "no section is empty")
    check(FeatureMatrix.features(in: .adapter) == [.wirelessRadios, .hotHandover, .pairing, .wifiAccessPoint, .androidAutoProjection, .appDrivenSetup],
          "Adapter section is the six adapter features in DESIGN.md §1 order")

    // Exclusive sub-group placement table (DESIGN.md §1): spot checks + no key claimed twice.
    check(Feature.headUnitName.exclusiveKeys(on: .carPlay) == ["accessoryName"], "headUnitName ▸ CarPlay exclusive = accessoryName")
    check(Feature.headUnitName.exclusiveKeys(on: .androidAuto).isEmpty, "headUnitName ▸ AA has no exclusive keys")
    check(Feature.panelGeometry.exclusiveKeys(on: .androidAuto) == ["aaFitPanelWithMargins"], "panelGeometry ▸ AA exclusive = aaFitPanelWithMargins")
    check(Feature.videoCodec.exclusiveKeys(on: .androidAuto) == ["aaPreferHEVC"], "videoCodec ▸ AA exclusive = aaPreferHEVC")
    check(Feature.theme.exclusiveKeys(on: .carPlay) == ["enablesUIAppearance", "enablesMapAppearance"], "theme ▸ CarPlay exclusive = the two appearance enables")
    // 2026-09-05: the second main view area (Dock resize button) is five CarPlay-only model keys
    // under the insets feature (DESIGN.md §0 decision 4 — a badged sub-group, not a new Feature case).
    // Pinned in full so a key going missing from the placement table fails here, not in the view.
    check(Feature.insets.exclusiveKeys(on: .carPlay) == ["enablesViewAreas", "mainDrawOutsideSafe", "enablesCornerMasks",
                                                          "viewArea2Enabled", "viewArea2X", "viewArea2Y", "viewArea2W", "viewArea2H"],
          "insets ▸ CarPlay exclusive = the three view-area enables + the five viewArea2 keys")
    check(Feature.insets.exclusiveKeys(on: .androidAuto).isEmpty, "insets ▸ AA has no exclusive keys (the second view area is CarPlay-only)")
    // Was `== ["touchScreenHighFidelity"]` until 2026-09-04. That key governs BOTH protocols — it is
    // what makes the AA renderer declare a touchscreen — so listing it as CarPlay-exclusive hid a
    // shared control inside a protocol-only sub-group. Empty is the assertion that matters now.
    check(Feature.inputDevices.exclusiveKeys(on: .carPlay) == [], "inputDevices has no CarPlay-exclusive key: the touchscreen toggle governs both protocols")
    check(Feature.drivingRestrictions.exclusiveKeys(on: .carPlay).contains("limitedUIJapanMaps"), "drivingRestrictions ▸ CarPlay carries japanMaps (no neutral reading)")
    var seen: [String: String] = [:]
    var duplicates: [String] = []
    for f in features { for p in Projection.allCases { for k in f.exclusiveKeys(on: p) {
        if let prior = seen[k] { duplicates.append("\(k) (\(prior) and \(f)×\(p))") } else { seen[k] = "\(f)×\(p)" }
    } } }
    check(duplicates.isEmpty, "no exclusive key is claimed by two (feature, projection) cells: \(duplicates)")
    check(FeatureMatrix.features(in: .adapter).allSatisfy { f in Projection.allCases.allSatisfy { f.exclusiveKeys(on: $0).isEmpty } },
          "adapter features have no exclusive sub-groups")

    // Value notes: well-formed, and the DESIGN.md §7/§10 provenance decisions hold.
    for n in FeatureMatrix.valueNotes {
        check(!n.value.isEmpty && !n.verification.note.isEmpty, "valueNote \(n.feature)/\(n.projection)/\(n.value) is filled in")
    }
    // `valueNote(for:on:value:)` answers with `first`, so a duplicated (feature, projection, value)
    // triple would silently shadow its twin. The table must not carry one.
    let triples = FeatureMatrix.valueNotes.map { "\($0.feature)/\($0.projection)/\($0.value)" }
    check(Set(triples).count == triples.count, "no (feature, projection, value) triple appears twice in valueNotes")
    // CORRECTED 2026-09-04 (integrator). These previously asserted tiers 4/5 were unverified/refuted,
    // which contradicted the committed evidence table in docs/androidauto/01_SESSION_AND_AV.md: all
    // nine tiers streamed on a Pixel 10 / gearhead 17.5 on 2026-09-04. The refutation is real but it
    // belongs to the (tier, codec) PAIRING — 2560x1440 declared as H.264 — not to the tier, and the
    // note must say which, or the UI tells the owner a working panel size is unproven.
    for tier in ["2560x1440", "3840x2160", "720x1280", "1080x1920", "1440x2560", "2160x3840"] {
        check(FeatureMatrix.valueNote(for: .panelGeometry, on: .androidAuto, value: tier)?.verification.status == .deviceProven,
              "AA tier \(tier) is recorded as device-proven (2026-09-04 sweep)")
    }
    check(FeatureMatrix.valueNote(for: .panelGeometry, on: .androidAuto, value: "2560x1440")?
            .verification.note.contains("H.264") == true,
          "the 2560x1440 note still records that the H.264 pairing was refused")
    check(FeatureMatrix.valueNote(for: .panelGeometry, on: .androidAuto, value: "1920x1080")?.verification.status == .deviceProven,
          "1920x1080 on AA is device-proven")
    check(FeatureMatrix.valueNote(for: .driverPosition, on: .androidAuto, value: "center")?.verification.status == .unverified,
          "driver position CENTER is unverified (§7 defect 2)")
    check(FeatureMatrix.valueNote(for: .driverPosition, on: .androidAuto, value: "left")?.verification.status == .deviceProven
          && FeatureMatrix.valueNote(for: .driverPosition, on: .androidAuto, value: "right")?.verification.status == .deviceProven,
          "driver position left/right are device-proven")
    check(FeatureMatrix.valueNote(for: .metadataFeeds, on: .carPlay, value: "rx-only")?.verification.status == .refuted,
          "CarPlay metadata tier rx-only is refuted")
    check(FeatureMatrix.valueNote(for: .panelGeometry, on: .androidAuto, value: "1024x600") == nil, "no note for a value that is not a tier")

    // Defect 1: wireless radios are one switch for BOTH projections.
    check(FeatureMatrix.support(.wirelessRadios, on: .carPlay).level == .supported
          && FeatureMatrix.support(.wirelessRadios, on: .androidAuto).level == .supported
          && FeatureMatrix.isUniform(.wirelessRadios),
          "wirelessRadios is supported on both projections (defect 1)")
    check(FeatureMatrix.support(.wirelessRadios, on: .carPlay).vendorKey == "wireless"
          && FeatureMatrix.support(.wirelessRadios, on: .androidAuto).vendorKey == "wireless",
          "wirelessRadios maps to the single `wireless` project key on both")
    check(FeatureMatrix.support(.appDrivenSetup, on: .androidAuto).level == .unsupported, "appDrivenSetup is CarPlay-only")
    check(FeatureMatrix.support(.androidAutoProjection, on: .carPlay).level == .unsupported, "androidAutoProjection does not affect CarPlay")
}

// MARK: - §8 Neutral profile derivations (Foundation-only half of the rendering rules)

private func settingsNeutralProfileTests() {
    section("Settings — neutral profile derivations (§8, Foundation-only)")

    let def = VehicleProfile.default
    check(def.identity.headUnitName == "CarLink Widescreen", "default head unit name")
    check(def.display.panel == PanelGeometry(), "default panel is 1920×1080 @60 dpi 160")
    check(def.display.panel.width == 1920 && def.display.panel.height == 1080 && def.display.panel.maxFPS == 60 && def.display.panel.dpi == 160,
          "PanelGeometry defaults are 1920/1080/60/160")
    check(def.driverPosition == .left && def.rightHandDrive == false, "default driver position is left ⇒ rightHandDrive false")
    check(def.appearance.theme == .light && def.nightModeLegacy == false, "default theme is light ⇒ nightModeLegacy false")
    check(def.video.hevcAllowed, "HEVC allowed by default (matches enablesHEVC default true)")
    check(def.restrictions == DrivingRestrictionPolicy(), "restrictions not declared by default")
    check(def.input.primary == .touchscreen && def.input.touchscreen != nil && def.input.dPad && def.input.mediaButtons,
          "default input: touchscreen primary + D-pad + media buttons")
    check(def.audio.voiceRateHz == 48000 && !def.audio.telephonyOverProjection, "default audio: 48 kHz voice, no telephony over projection")
    check(def.metadata == MetadataFeeds(nowPlaying: true, navigation: true, telephony: true), "all three metadata feeds on by default")
    check(def.androidAuto == AndroidAutoExtensions(fitPanelWithMargins: true, preferHEVC: false), "AA extensions default: margins on, preferHEVC off")
    check(def.carPlay.metadataTier == "proven" && def.carPlay.audioMode == "auto", "CarPlay extensions default: proven tier, auto audio")
    check(def.branding.icon == nil && !def.branding.advertise && def.branding.visible, "branding default: no icon, not advertised, visible")

    // Legacy-boolean derivations (what W1 writes back into rightHandDrive / nightMode for a downgrade).
    var v = def
    v.driverPosition = .right
    check(v.rightHandDrive, ".right ⇒ rightHandDrive true")
    v.driverPosition = .center
    check(!v.rightHandDrive, ".center ⇒ rightHandDrive false (legacy bool cannot express center)")
    v.appearance.theme = .dark
    check(v.nightModeLegacy, ".dark ⇒ nightModeLegacy true")
    v.appearance.theme = .auto
    check(!v.nightModeLegacy, ".auto ⇒ nightModeLegacy false (resolution happens at the AppDelegate call site)")

    // Vocabulary raw values are the persistence spellings (vc.driverPosition / vc.theme).
    check(DriverPosition.allCases.map(\.rawValue) == ["left", "right", "center"], "DriverPosition raw values")
    check(AppearanceTheme.allCases.map(\.rawValue) == ["auto", "light", "dark"], "AppearanceTheme raw values")
    check(PrimaryInput.allCases.map(\.rawValue) == ["touchscreen", "touchpad", "rotary"], "PrimaryInput raw values")
    check(ChargingConnector.gbtDC.rawValue == "gbt_dc" && ChargingConnector.nacsAC.rawValue == "nacs_ac", "connector raw values use snake_case")

    // Geometry helpers.
    let portrait = PanelGeometry(width: 720, height: 1280)
    check(portrait.isPortrait && !PanelGeometry().isPortrait, "isPortrait ⇔ height > width")
    check(abs(PanelGeometry().aspect - 16.0 / 9.0) < 1e-9, "aspect of 1920×1080 is 16:9")
    check(PanelGeometry(width: 0, height: 0).aspect == 0, "aspect of a zero panel is 0, not NaN")
    check(PanelGeometry().impliedDPI == nil, "impliedDPI is nil without a diagonal")
    check(PanelGeometry(width: 1920, height: 1080, diagonalInches: 13.8).impliedDPI == 160, "1920×1080 on 13.8\" implies 160 dpi")
    check(PanelGeometry(width: 800, height: 480, diagonalInches: 0).impliedDPI == nil, "a zero diagonal implies nothing")
    check(PanelInsets().isZero && PanelInsets.zero.isZero && !PanelInsets(left: 1).isZero, "PanelInsets.isZero")
    check(StatusBarPolicy().isDefault && !StatusBarPolicy(hideClock: true).isDefault, "StatusBarPolicy.isDefault")

    // Powertrain: connectors only matter for an electrified vehicle, and duplicates are dropped
    // (mirrors the iapConfig param-20 dedupe on the box side).
    let gas = Powertrain(engines: [.gasoline], connectors: [ConnectorSpec(type: .ccs1)])
    check(!gas.isElectrified && gas.effectiveConnectors.isEmpty, "gasoline: not electrified, connectors ignored")
    let ev = Powertrain(engines: [.electric, .gasoline],
                        connectors: [ConnectorSpec(type: .ccs1, powerWatts: 150_000), ConnectorSpec(type: .ccs1), ConnectorSpec(type: .nacsDC)])
    check(ev.isElectrified, "electric+gasoline: electrified (PHEV)")
    check(ev.effectiveConnectors.map(\.type) == [.ccs1, .nacsDC], "effectiveConnectors de-duplicates by type, first wins")
    check(ev.effectiveConnectors.first?.powerWatts == 150_000, "first connector's power survives the dedupe")
    check(ev.canonicalEngines == [.gasoline, .electric], "canonicalEngines is in EngineType declaration order")

    // AdapterSettings polarity (DESIGN.md §10: wifi_ap is ENABLED when absent).
    let ad = AdapterSettings.default
    check(ad.wirelessRadios && ad.wifiAccessPoint && ad.androidAuto && ad.appDrivenSetup, "adapter defaults: radios, AP, AA, app-driven SETUP on")
    check(!ad.hotHandover && !ad.pairingNumericComparison && !ad.pairingInteractiveAnswer, "adapter defaults: hot hand-over and numeric comparison off")

    // CarPlay-exclusive audio table twin of AudioFormatRow (no UUID).
    let fmt = CarPlayExtensions.AudioFormat()
    check(fmt.streamType == 102 && fmt.audioType == "media" && fmt.input == "none" && fmt.output == "aac_lc_48k_stereo", "AudioFormat defaults")
    check(!CarPlayExtensions.AudioFormat.defaultCustomFormats.isEmpty, "defaultCustomFormats is populated")
}

// MARK: - §9.6 Android Auto renderer (W2's `init(profile:adapter:warn:)`)

/// Expected values are the pre-refactor snapshot: `AACapability.init(config:)` on the shipped default
/// produced 1920×1080 @60, density 160, driver wire 2 (LEFT), H.264, no margins, night off. Fields
/// asserted are the ones `AASession` puts in the ServiceDiscoveryResponse. Bench env overrides
/// (`AA_FORCE_RES`, `AA_FORCE_FPS`, `AA_DENSITY`, `AA_DRIVER_POSITION`, `AA_HEVC`, `AA_MARGINS`,
/// `AA_PANEL`) would change these; the harness runs with none set.
private func settingsAARendererTests() {
    section("Settings — AACapability(profile:adapter:) rendering (§9.6, §8)")

    let env = ProcessInfo.processInfo.environment
    let overrides = ["AA_FORCE_RES", "AA_FORCE_FPS", "AA_DENSITY", "AA_DRIVER_POSITION", "AA_HEVC", "AA_MARGINS", "AA_PANEL",
                     "AA_VOICE_RATE", "AA_TELEPHONY_SINK", "AA_METADATA", "AA_NO_TOUCH"].filter { env[$0] != nil }
    check(overrides.isEmpty, "no AA_* bench override is set in the test environment (found \(overrides))")

    var warnings: [String] = []
    let cap = AACapability(profile: .default, adapter: .default, warn: { warnings.append($0) })
    check(cap.resolution == .r1920x1080, "default profile declares VIDEO_1920x1080 (got \(cap.resolution))")
    check(cap.frameRate == .fps60, "default profile declares 60 fps (got \(cap.frameRate))")
    check(cap.density == 160, "default profile declares density 160 (got \(cap.density))")
    check(cap.driverPosition == 2, "default profile declares driver_position 2 = LEFT (got \(cap.driverPosition))")
    check(cap.videoCodecHEVC == false, "default profile declares H.264 (tier 3 does not need HEVC, preferHEVC off)")
    check(cap.hasMargins == false, "default profile is an exact tier ⇒ no margins")
    check(cap.touchSize.w == 1920 && cap.touchSize.h == 1080, "touch surface is the full tier (got \(cap.touchSize))")
    check(cap.visibleWidth == 0 && cap.visibleHeight == 0, "exact tier ⇒ no visible-rect override recorded")
    check(cap.nightMode == false, "light theme ⇒ night_mode false")
    check(cap.name == "CarLink Widescreen", "head-unit name comes from identity.headUnitName")
    check(warnings.isEmpty, "the shipped default renders without approximation warnings (got \(warnings))")
    check(cap.negotiationNotes.isEmpty, "the shipped default has no negotiation notes (got \(cap.negotiationNotes))")
    check(cap.driverSeat == .left, "default profile ⇒ driverSeat .left")
    check(cap.drivingMask.rawValue == 26 && cap.drivingMask == .drivingDefault, "undeclared restrictions ⇒ drivingMask is drivingDefault (26)")
    check(cap.metadata == .all, "all three metadata feeds on ⇒ MetadataServices.all")
    check(cap.audioSinks.count == 3, "default profile declares three audio sinks (media, guidance, system) — got \(cap.audioSinks.count)")
    check(!cap.telephonySink, "telephonyOverProjection off ⇒ no telephony sink")
    check(cap.audioSinks.filter(\.voice).allSatisfy { $0.rate == 48000 }, "voice sinks run at the profile's 48 kHz")
    check(cap.declaresTouchscreen, "touchscreen present ⇒ InputSourceService declares a touchscreen")
    check((AACapability.Resolution.landscape + AACapability.Resolution.portrait).allSatisfy(\.deviceVerified),
          "every AA tier is device-verified (all nine swept 2026-09-04)")
    check(AACapability.Resolution.r2560x1440.needsHEVC && AACapability.Resolution.r3840x2160.needsHEVC
          && !AACapability.Resolution.r1920x1080.needsHEVC,
          "the surviving constraint is the codec pairing: tiers above 1080p are H.265-only")

    // §8: theme .auto is resolved by the caller (Mac appearance); nil means 'unknown' ⇒ light + a note.
    var autoP = VehicleProfile.default
    autoP.appearance.theme = .auto
    check(AACapability(profile: autoP, adapter: .default, autoThemeIsDark: true, warn: { _ in }).nightMode == true, ".auto + autoThemeIsDark:true ⇒ night_mode true")
    check(AACapability(profile: autoP, adapter: .default, autoThemeIsDark: false, warn: { _ in }).nightMode == false, ".auto + autoThemeIsDark:false ⇒ night_mode false")
    let autoUnknown = AACapability(profile: autoP, adapter: .default, warn: { _ in })
    check(autoUnknown.nightMode == false && !autoUnknown.negotiationNotes.isEmpty, ".auto with no resolution ⇒ light + a negotiation note")

    // §8: declared restrictions → mask via FeatureMatrix; an inexpressible member is noted by title.
    var rp = VehicleProfile.default
    rp.restrictions = DrivingRestrictionPolicy(declared: true, set: [.keyboard, .mediaLists, .video])
    let restricted = AACapability(profile: rp, adapter: .default, warn: { _ in })
    check(restricted.drivingMask.rawValue == 3, "declared {keyboard, mediaLists, video} ⇒ driving_status 3 (got \(restricted.drivingMask.rawValue))")
    check(restricted.negotiationNotes.contains { $0.contains("Long media lists") }, "the unexpressed 'Long media lists' member is noted (got \(restricted.negotiationNotes))")
    rp.restrictions = DrivingRestrictionPolicy(declared: true, set: .typicalDriving)
    check(AACapability(profile: rp, adapter: .default, warn: { _ in }).drivingMask.rawValue == 26, "declared typicalDriving ⇒ 26, same as the undeclared default")

    // §8: metadata feeds gate the three service descriptors individually.
    var mp = VehicleProfile.default
    mp.metadata = MetadataFeeds(nowPlaying: false, navigation: true, telephony: false)
    let md = AACapability(profile: mp, adapter: .default, warn: { _ in }).metadata
    check(!md.mediaPlayback && md.navigationStatus && !md.phoneStatus, "metadata feeds map one-to-one onto MediaPlayback / NavigationStatus / PhoneStatus")

    // §8: input + audio blocks.
    var ip = VehicleProfile.default
    ip.input = InputDevices(primary: .rotary, touchscreen: nil, rotaryKnob: RotaryKnob())
    check(!AACapability(profile: ip, adapter: .default, warn: { _ in }).declaresTouchscreen, "no touchscreen in the profile ⇒ none declared")
    var ap = VehicleProfile.default
    ap.audio = AudioProfile(voiceRateHz: 16000, telephonyOverProjection: true)
    let audio = AACapability(profile: ap, adapter: .default, warn: { _ in })
    check(audio.audioSinks.filter(\.voice).allSatisfy { $0.rate == 16000 }, "voiceRateHz 16000 ⇒ guidance/system sinks at 16 kHz")
    check(audio.telephonySink && audio.audioSinks.count == 4, "telephonyOverProjection ⇒ a fourth (telephony) sink is declared (got \(audio.audioSinks.count))")

    // §8: driverPosition → wire 2 / 1 / 3.
    var p = VehicleProfile.default
    p.driverPosition = .right
    check(AACapability(profile: p, adapter: .default, warn: { _ in }).driverPosition == 1, ".right ⇒ driver_position 1")
    p.driverPosition = .center
    check(AACapability(profile: p, adapter: .default, warn: { _ in }).driverPosition == 3, ".center ⇒ driver_position 3 (CENTER, unverified on device)")

    // §8: theme → night_mode sensor (auto is resolved by the caller, not here).
    p = .default
    p.appearance.theme = .dark
    check(AACapability(profile: p, adapter: .default, warn: { _ in }).nightMode == true, ".dark ⇒ night_mode true")

    // §8: dpi → density.
    p = .default
    p.display.panel.dpi = 220
    check(AACapability(profile: p, adapter: .default, warn: { _ in }).density == 220, "panel.dpi 220 ⇒ density 220")

    // §8 / defect 4: a non-tier panel with fitPanelWithMargins ⇒ smallest containing tier + margins,
    // and a negotiation note the Vehicle tab shows; without margins ⇒ nearest tier, no margins.
    if let wide = VehicleProfilePreset.named("dhu-wide") {
        var notes: [String] = []
        let fit = AACapability(profile: wide.vehicle, adapter: wide.adapter, warn: { notes.append($0) })
        check(fit.resolution == .r1280x720, "dhu-wide 1280×500 with margins ⇒ tier 1280×720 (got \(fit.resolution))")
        check(fit.visibleWidth == 1280 && fit.visibleHeight == 500, "dhu-wide visible rect is 1280×500 (got \(fit.visibleWidth)×\(fit.visibleHeight))")
        check(fit.hasMargins && fit.margins.w == 0 && fit.margins.h == 220, "dhu-wide margins are 0×220")
        check(!fit.negotiationNotes.isEmpty, "dhu-wide records a negotiation note (defect 4)")

        var noFit = wide.vehicle
        noFit.androidAuto.fitPanelWithMargins = false
        let nearest = AACapability(profile: noFit, adapter: wide.adapter, warn: { _ in })
        check(!nearest.hasMargins, "dhu-wide without margins ⇒ no margins")
        check(nearest.resolution == .r800x480,
              "dhu-wide without margins ⇒ the largest tier INSIDE 1280×500, which is 800×480 (1280×720 is taller than the panel) (got \(nearest.resolution))")
        check(nearest.negotiationNotes.contains { $0.contains("upscaled") },
              "dhu-wide without margins notes that 800×480 is upscaled to the panel (got \(nearest.negotiationNotes))")
    } else { check(false, "preset 'dhu-wide' exists") }

    // §8: 800×480 @30 (dhu-default) is an exact tier at the DHU's reference values.
    if let d = VehicleProfilePreset.named("dhu-default") {
        let c = AACapability(profile: d.vehicle, adapter: d.adapter, warn: { _ in })
        check(c.resolution == .r800x480 && c.frameRate == .fps30 && !c.hasMargins, "dhu-default ⇒ VIDEO_800x480 @30, no margins")
    }

    // §7 defect 5: hevcAllowed=false clamps to ≤1080p and notes it; preferHEVC ⇒ HEVC at ≤1080p.
    p = .default
    p.display.panel = PanelGeometry(width: 2560, height: 1440, maxFPS: 60)
    p.video.hevcAllowed = false
    let clamped = AACapability(profile: p, adapter: .default, warn: { _ in })
    check(clamped.resolution.rawValue <= AACapability.Resolution.r1920x1080.rawValue,
          "2560×1440 with HEVC disallowed ⇒ clamped to ≤1080p (got \(clamped.resolution))")
    check(!clamped.videoCodecHEVC, "HEVC disallowed ⇒ H.264 declared")
    check(!clamped.negotiationNotes.isEmpty, "HEVC clamp is recorded as a negotiation note")

    p = .default
    p.video.hevcAllowed = true
    p.androidAuto.preferHEVC = true
    check(AACapability(profile: p, adapter: .default, warn: { _ in }).videoCodecHEVC, "preferHEVC ⇒ HEVC declared at 1080p")

    p = .default
    p.display.panel = PanelGeometry(width: 2560, height: 1440, maxFPS: 60)
    p.video.hevcAllowed = true
    let qhd = AACapability(profile: p, adapter: .default, warn: { _ in })
    check(qhd.resolution == .r2560x1440 && qhd.videoCodecHEVC, "2560×1440 with HEVC allowed ⇒ tier 4 declared as HEVC")

    // §8: restrictions → the mask AASession sends on setDrivingRestricted(true) — through the
    // RENDERER. Until 2026-09-04 this called FeatureMatrix.androidAutoDrivingStatus directly and so
    // never exercised the bridge it sits under.
    check(AACapability.DrivingRestrictions.drivingDefault.rawValue == 26, "AACapability.DrivingRestrictions.drivingDefault is 26")
    p = .default
    p.restrictions = DrivingRestrictionPolicy(declared: true, set: [.video, .voiceInput])
    let aaOnly = AACapability(profile: p, adapter: .default, warn: { _ in })
    check(aaOnly.drivingMask.rawValue == 5,
          "declared {video, voiceInput} ⇒ drivingMask 5 reaches the wire (the AA-only members CarPlay cannot express; got \(aaOnly.drivingMask.rawValue))")
    check(aaOnly.drivingMask.contains(.noVideo) && aaOnly.negotiationNotes.contains { $0.contains("NO_VIDEO") },
          "NO_VIDEO in the mask is called out: it blanks the projection while driving")
    check(!aaOnly.drivingRestricted, "a declared restriction set is a capability, not a driving signal: the session starts unrestricted")
}

// MARK: - §9.6 AA renderer: per-(tier, fps) verification, T4 geometry edge cases, undeclared restrictions

private func settingsAAGeometryTests() {
    section("Settings — AACapability geometry, verification notes, undeclared restrictions (§9.6, T4)")
    typealias R = AACapability.Resolution

    // deviceVerified(atFPS:) transcribes the evidence table per PAIRING, not per tier.
    check(!R.p1080x1920.deviceVerified(atFPS: 60), "tier 7 (1080×1920) at 60 fps is NOT verified (only streamed at 30)")
    check(R.p1080x1920.deviceVerified(atFPS: 30), "tier 7 at 30 fps is verified")
    check(R.r2560x1440.deviceVerified(atFPS: 30), "tier 4 (2560×1440) at 30 fps is verified")
    check(R.r800x480.deviceVerified(atFPS: 30) && R.r800x480.deviceVerified(atFPS: 60),
          "tier 1 (800×480) is verified at BOTH rates: 800×480@30 was the pre-57501aa hardcoded baseline that ran working sessions")
    check(!R.r1920x1080.deviceVerified(atFPS: 30), "tier 3 (1920×1080) at 30 fps is not verified (the sweep ran it at 60 only)")
    check(R.p1080x1920.deviceVerifiedRates == [.fps30] && R.r800x480.deviceVerifiedRates == [.fps30, .fps60],
          "deviceVerifiedRates lists the proven rates in 30/60 order")
    check(R.r800x480.deviceVerified(atFPS: 45) && !R.r1920x1080.deviceVerified(atFPS: 45),
          "the Hz convenience snaps like the renderer (45 → 30): proven for tier 1, not for tier 3")

    // The renderer's note fires on an unverified PAIRING only and names the rate that has streamed.
    var p = VehicleProfile.default
    p.display.panel = PanelGeometry(width: 1080, height: 1920, maxFPS: 60)
    let tall60 = AACapability(profile: p, adapter: .default, warn: { _ in })
    check(tall60.resolution == .p1080x1920 && tall60.frameRate == .fps60, "1080×1920 @60 declares tier 7 at 60 (got \(tall60.resolution) \(tall60.frameRate))")
    check(tall60.negotiationNotes.contains { $0.contains("has not been verified") && $0.contains("30 fps") },
          "1080×1920 @60 notes the unverified pairing and names 30 as the proven rate (got \(tall60.negotiationNotes))")
    p.display.panel.maxFPS = 30
    check(!AACapability(profile: p, adapter: .default, warn: { _ in }).negotiationNotes.contains { $0.contains("has not been verified") },
          "1080×1920 @30 is a verified pairing: no note")
    if let d = VehicleProfilePreset.named("dhu-default") {
        let c = AACapability(profile: d.vehicle, adapter: d.adapter, warn: { _ in })
        check(c.negotiationNotes.isEmpty, "dhu-default (800×480 @30, the original working baseline) renders with NO notes (got \(c.negotiationNotes))")
    } else { check(false, "preset 'dhu-default' exists") }

    // Same-aspect sub-tier, through the PLAIN init: a preset is clamped to 800×480 on load, so a
    // 750×450 profile can only reach the renderer this way.
    let sub = AACapability(mainWidth: 750, mainHeight: 450, maxFPS: 30, name: "p", nightMode: false, warn: { _ in })
    check(sub.resolution == .r800x480 && !sub.hasMargins && sub.visibleWidth == 0,
          "750×450 ⇒ the whole tier 800×480, no margins, no visible override (got \(sub.resolution) \(sub.visibleWidth))")
    check(sub.negotiationNotes.count == 1 && sub.negotiationNotes[0].contains("same aspect as 800x480 but smaller")
          && sub.negotiationNotes[0].contains("×0.94") && sub.negotiationNotes[0].contains("margins 50x30"),
          "750×450 notes the down-scale (×0.94) and the DHU's own margins-50x30 alternative (got \(sub.negotiationNotes))")

    // Upscale fallback: 2400×960 with HEVC disallowed — no ≤1080p tier contains it.
    let up = AACapability(mainWidth: 2400, mainHeight: 960, maxFPS: 60, name: "p", nightMode: false, hevcAllowed: false, warn: { _ in })
    check(up.resolution == .r1920x1080 && up.visibleWidth == 1920 && up.visibleHeight == 768,
          "2400×960 no-HEVC ⇒ tier 1920×1080, visible 1920×768 (got \(up.resolution) \(up.visibleWidth)×\(up.visibleHeight))")
    check(up.margins.w == 0 && up.margins.h == 312 && !up.videoCodecHEVC, "2400×960 no-HEVC ⇒ margins 0×312, H.264")
    check(up.negotiationNotes.contains { $0.contains("upscaled ×1.25") }, "the upscale is noted with its factor ×1.25 (got \(up.negotiationNotes))")
    check(up.negotiationNotes.contains { $0.contains("would declare 2560x1440") && $0.contains("clamped to 1920x1080") },
          "the HEVC clamp names the tier it gave up")
    let upHEVC = AACapability(mainWidth: 2400, mainHeight: 960, maxFPS: 60, name: "p", nightMode: false, hevcAllowed: true, warn: { _ in })
    check(upHEVC.resolution == .r2560x1440 && upHEVC.videoCodecHEVC && upHEVC.visibleWidth == 2560 && upHEVC.visibleHeight == 1024
          && !upHEVC.negotiationNotes.contains { $0.contains("upscaled") },
          "2400×960 with HEVC allowed ⇒ 2560×1440 as H.265 contains it (visible 2560×1024): no upscale")

    // Odd panel axis: 1920×1079 stays on tier 3 with a 2-px margin; no whole-tier escalation.
    let odd = AACapability(mainWidth: 1920, mainHeight: 1079, maxFPS: 60, name: "p", nightMode: false, warn: { _ in })
    check(odd.resolution == .r1920x1080 && odd.margins.w == 0 && odd.margins.h == 2,
          "1920×1079 ⇒ tier 1920×1080, margins 0×2 (got \(odd.resolution) \(odd.margins))")
    check(!odd.videoCodecHEVC && odd.visibleWidth == 1920 && odd.visibleHeight == 1078, "1920×1079 ⇒ H.264, visible 1920×1078 (even)")
    check(odd.negotiationNotes.count == 1 && odd.negotiationNotes[0].contains("odd panel axis"),
          "1920×1079: exactly one note, naming the odd-axis rounding, no upscale note (got \(odd.negotiationNotes))")
    check(R.tierAndVisible(width: 1920, height: 1079).tier == .r1920x1080 && R.tierAndVisible(width: 1920, height: 1081).tier == .r2560x1440,
          "one pixel over the tier (1081) escalates to 2560×1440; one pixel under (1079) does not")

    // Audit F2-5: restrictions authored while the Limited UI switch is off. The default mask still
    // goes on the wire, and a note names the members silently not sent — but only when the set
    // would change the wire; typicalDriving is bit-identical to the default and must stay silent.
    p = .default
    p.restrictions = DrivingRestrictionPolicy(declared: false, set: [.video])
    let authored = AACapability(profile: p, adapter: .default, warn: { _ in })
    check(authored.drivingMask == .drivingDefault, "undeclared {video} ⇒ the default mask 26 still goes on the wire")
    check(authored.negotiationNotes.contains { $0.contains("authored but not declared") && $0.contains("not sent: Video") },
          "undeclared {video} is noted, naming Video as not sent (got \(authored.negotiationNotes))")
    p.restrictions = DrivingRestrictionPolicy(declared: false, set: .typicalDriving)
    check(!AACapability(profile: p, adapter: .default, warn: { _ in }).negotiationNotes.contains { $0.contains("authored but not declared") },
          "undeclared typicalDriving is bit-identical to the default: no note (it would cry wolf)")
    p.restrictions = DrivingRestrictionPolicy(declared: false, set: [.keyboard])
    let kb = AACapability(profile: p, adapter: .default, warn: { _ in })
    check(kb.negotiationNotes.contains { $0.contains("authored but not declared") && !$0.contains("not sent") },
          "undeclared {keyboard} (mask 2 ≠ 26) is noted, but nothing is listed as not sent: bit 2 is inside the default (got \(kb.negotiationNotes))")
}

// MARK: - §9.6 AA_* bench levers (scoped setenv; the harness environment stays clean)

/// The renderer reads `ProcessInfo.processInfo.environment` at init, which reflects a runtime
/// `setenv`, so each lever is set for exactly one render and unset again — the pre/postcondition
/// checks prove the section leaves no lever behind for the sections after it.
private func settingsAALeverTests() {
    section("Settings — AA_HEVC / AA_NO_TOUCH levers (§9.6, audit F2-2 / F2-6)")
    let levers = ["AA_HEVC", "AA_NO_TOUCH"]
    check(levers.allSatisfy { ProcessInfo.processInfo.environment[$0] == nil }, "precondition: no lever is set before this section")
    defer {
        for l in levers { unsetenv(l) }
        check(levers.allSatisfy { ProcessInfo.processInfo.environment[$0] == nil }, "postcondition: every lever is unset again")
    }
    func with<T>(_ key: String, _ value: String, _ body: () -> T) -> T {
        setenv(key, value, 1)
        defer { unsetenv(key) }
        return body()
    }
    func render(_ p: VehicleProfile) -> AACapability { AACapability(profile: p, adapter: .default, warn: { _ in }) }
    func hevcNotes(_ c: AACapability) -> [String] { c.negotiationNotes.filter { $0.contains("AA_HEVC") } }

    // AA_HEVC=1 is noted exactly when it CHANGES the declared codec (audit F2-2: the lever used to
    // produce codec_type=7 with empty notes on a profile that allowed HEVC).
    var p = VehicleProfile.default                         // tier 3, hevcAllowed, preferHEVC off ⇒ H.264
    let forced = with("AA_HEVC", "1") { render(p) }
    check(forced.videoCodecHEVC, "AA_HEVC=1 declares H.265 on the shipped default")
    check(hevcNotes(forced).count == 1 && hevcNotes(forced)[0].contains("the profile would declare H.264"),
          "AA_HEVC=1 on a profile that would declare H.264 is noted, once (got \(forced.negotiationNotes))")
    p.androidAuto.preferHEVC = true                        // the profile ALREADY declares H.265 here
    let already = with("AA_HEVC", "1") { render(p) }
    check(already.videoCodecHEVC && hevcNotes(already).isEmpty,
          "AA_HEVC=1 with preferHEVC on changes nothing ⇒ no lever note (got \(already.negotiationNotes))")
    p = .default
    p.display.panel = PanelGeometry(width: 2560, height: 1440, maxFPS: 60)   // tier 4 is H.265-only anyway
    let qhd = with("AA_HEVC", "1") { render(p) }
    check(qhd.resolution == .r2560x1440 && qhd.videoCodecHEVC && hevcNotes(qhd).isEmpty,
          "AA_HEVC=1 at an H.265-only tier changes nothing ⇒ no lever note (got \(qhd.negotiationNotes))")
    p = .default
    p.video.hevcAllowed = false
    let over = with("AA_HEVC", "1") { render(p) }
    check(over.videoCodecHEVC && hevcNotes(over).count == 1 && hevcNotes(over)[0].contains("although the profile disallows HEVC"),
          "AA_HEVC=1 overrides hevcAllowed=false and says so, once (got \(over.negotiationNotes))")
    check(!render(p).videoCodecHEVC, "with the lever unset again, hevcAllowed=false declares H.264")
    let zero = with("AA_HEVC", "0") { render(VehicleProfile.default) }
    check(!zero.videoCodecHEVC && hevcNotes(zero).isEmpty, "AA_HEVC=0 is a no-op (only =1 is the lever)")

    // AA_NO_TOUCH is `== "1"` like every other lever; `=0` used to disable the touchscreen too (F2-6).
    let noTouch0 = with("AA_NO_TOUCH", "0") { render(.default) }
    check(noTouch0.declaresTouchscreen && !noTouch0.negotiationNotes.contains { $0.contains("AA_NO_TOUCH") },
          "AA_NO_TOUCH=0 is a no-op: touchscreen still declared, no note (got \(noTouch0.negotiationNotes))")
    let noTouch1 = with("AA_NO_TOUCH", "1") { render(.default) }
    check(!noTouch1.declaresTouchscreen && noTouch1.negotiationNotes.contains { $0.contains("AA_NO_TOUCH override") },
          "AA_NO_TOUCH=1 drops the touchscreen and notes the override")
    var rotary = VehicleProfile.default
    rotary.input = InputDevices(primary: .rotary, touchscreen: nil, rotaryKnob: RotaryKnob())
    let noTouchAlready = with("AA_NO_TOUCH", "1") { render(rotary) }
    check(!noTouchAlready.declaresTouchscreen && !noTouchAlready.negotiationNotes.contains { $0.contains("AA_NO_TOUCH") },
          "AA_NO_TOUCH=1 on a profile with no touchscreen changes nothing ⇒ no lever note")
}

// MARK: - Hostile-document regressions (2026-09-04)

/// `AudioFormat.validated()` is the guard between an imported document and the CarPlay YAML emitter,
/// which interpolates these three strings RAW into a flow mapping. Before the guard, one hand-edited
/// row could malform the whole pushed document and make the box fall back to built-in defaults for
/// resolution, HEVC, audio AND metadata (the B3 failure class). Testable here only since the
/// vocabulary and the validator moved into the Foundation-only profile.
private func settingsAudioVocabularyTests() {
    section("Settings — imported audio rows cannot reach the emitter unvalidated")
    typealias Row = CarPlayExtensions.AudioFormat

    // The exact payload the audit used: a closing brace + newline that would terminate the flow
    // mapping early and inject a sibling key into the pushed document.
    let hostile = Row(streamType: 999, audioType: "x}\n  bogus: [\"",
                      input: "\" evil", output: "x}\n  injected: true")
    let v = hostile.validated()
    check(v.streamType == 102, "an unknown streamType falls back to 102")
    check(v.audioType == "media", "an injected audioType falls back to 'media'")
    check(v.input == "none", "an injected input codec falls back to 'none'")
    check(v.output == "aac_lc_48k_stereo", "an injected output codec falls back to the shipped default")
    for field in [v.audioType, v.input, v.output] {
        check(!field.contains("}") && !field.contains("\n") && !field.contains("\""),
              "no validated field carries a YAML flow-mapping metacharacter (got \(field))")
    }
    // Totality: whatever a document carries, every field of the result is in the vocabulary.
    check(Row.streamTypes.contains(v.streamType) && Row.types.contains(v.audioType)
          && Row.codecs.contains(v.input) && Row.codecs.contains(v.output),
          "validated() is total — every field lands in the advertised vocabulary")
    // And it must NOT mangle a legitimate row.
    for row in Row.defaultCustomFormats {
        check(row.validated() == row, "a shipped default row survives validation unchanged (\(row.audioType))")
    }
    let legit = Row(streamType: 100, audioType: "speechRecognition", input: "aac_eld_16k_mono", output: "opus_48k_mono")
    check(legit.validated() == legit, "a valid non-default row is passed through untouched")
    check(Row(streamType: 101, audioType: "", input: "none", output: "pcm_48k_stereo").validated().audioType == "",
          "the empty audioType is a REAL vocabulary member (the wired PCM catch-all), not an unknown")
}


/// `impliedDPI` computes on UNTRUSTED input — an imported `.vehicleprofile.json` is hand-editable and
/// `apply(_:)` writes `diagonalInches` unclamped. The pre-fix form trapped and took the app down
/// (`Int(_:)` of an out-of-range Double, reproduced exit 133). These pin that every hostile pair
/// returns nil instead of crashing or inventing a density.
private func settingsHostileInputTests() {
    section("Settings — hostile document input (impliedDPI)")
    func dpi(_ w: Int, _ h: Int, _ d: Double?) -> Int? {
        var g = PanelGeometry(); g.width = w; g.height = h; g.diagonalInches = d
        return g.impliedDPI
    }
    check(dpi(1920, 1080, nil) == nil, "no diagonal ⇒ nil")
    check(dpi(1920, 1080, 0) == nil, "zero diagonal ⇒ nil")
    check(dpi(1920, 1080, -5) == nil, "negative diagonal ⇒ nil")
    check(dpi(1920, 1080, 1e-300) == nil, "1e-300 diagonal ⇒ nil, not a trap (the crash case)")
    check(dpi(1920, 1080, .infinity) == nil, "infinite diagonal ⇒ nil")
    check(dpi(1920, 1080, .nan) == nil, "NaN diagonal ⇒ nil")
    check(dpi(Int.max, Int.max, 10) == nil, "Int.max pixel grid ⇒ nil, not an overflow trap")
    check(dpi(1920, 1080, 1e300) == nil, "absurdly large diagonal ⇒ nil (below the 20 dpi floor)")
    // And it still answers correctly for a real panel: 1920x1080 over 15.6" is ~141 dpi.
    check(dpi(1920, 1080, 15.6) == 141, "a real panel still computes: 1920x1080 @ 15.6in ⇒ 141 dpi")
    check(dpi(800, 480, 6.0) == 155, "the DHU 6in panel: 800x480 @ 6in ⇒ 155 dpi")
}

// MARK: - §9.7 UserDefaults migration (`vc.profileKeysV1`)

/// LIVE since 2026-09-04. The migration body was lifted out of `VehicleConfigModel` (in the
/// AppKit/SwiftUI `App/SettingsWindow.swift`, which this harness cannot compile) into
/// `VehicleProfileKeyMigration` in the Foundation-only `App/VehicleConfig.swift`; the model now
/// delegates to it. So these checks run against the SHIPPED code path, not a hand-copied twin —
/// which matters more here than anywhere else in this file: the migration derives the neutral
/// `driverPosition`/`theme` keys from the legacy `rightHandDrive`/`nightMode` booleans, runs exactly
/// once per defaults domain behind the `profileKeysV1` sentinel, and can never re-run to correct a
/// bad derivation.
private func settingsMigrationTests() {
    section("Settings — vc.profileKeysV1 migration (§9.7)")
    let suite = "carlink.settings-tests.\(UUID().uuidString)"
    guard let ud = UserDefaults(suiteName: suite) else {
        check(false, "could not create a throwaway UserDefaults suite")
        return
    }
    defer { ud.removePersistentDomain(forName: suite) }
    let vc = "vc."

    // Fresh legacy store: right-hand drive + night mode set, no marker.
    ud.set(true, forKey: vc + "rightHandDrive")
    ud.set(true, forKey: vc + "nightMode")
    check(ud.object(forKey: vc + "profileKeysV1") == nil, "precondition: no profileKeysV1 marker")
    VehicleProfileKeyMigration.run(prefix: vc, ud: ud)
    check(ud.string(forKey: vc + "driverPosition") == "right", "migration seeds driverPosition = right from rightHandDrive")
    check(ud.string(forKey: vc + "theme") == "dark", "migration seeds theme = dark from nightMode")
    check(ud.bool(forKey: vc + "profileKeysV1"), "migration sets the profileKeysV1 marker")

    // Idempotent: a second run changes nothing, even after the legacy pair flips.
    let snapshot = ud.persistentDomain(forName: suite) as NSDictionary?
    ud.set(false, forKey: vc + "rightHandDrive")
    ud.set(false, forKey: vc + "nightMode")
    VehicleProfileKeyMigration.run(prefix: vc, ud: ud)
    check(ud.string(forKey: vc + "driverPosition") == "right" && ud.string(forKey: vc + "theme") == "dark",
          "second run does not overwrite driverPosition/theme from the (now flipped) legacy pair")
    ud.set(true, forKey: vc + "rightHandDrive"); ud.set(true, forKey: vc + "nightMode")
    check((ud.persistentDomain(forName: suite) as NSDictionary?) == snapshot, "second run leaves the domain byte-identical")

    // A store with the new keys already present and no marker keeps them (absent-only seeding).
    let suite2 = suite + ".b"
    if let ud2 = UserDefaults(suiteName: suite2) {
        defer { ud2.removePersistentDomain(forName: suite2) }
        ud2.set(true, forKey: vc + "rightHandDrive")
        ud2.set("center", forKey: vc + "driverPosition")
        VehicleProfileKeyMigration.run(prefix: vc, ud: ud2)
        check(ud2.string(forKey: vc + "driverPosition") == "center", "an existing driverPosition is not overwritten by the legacy bool")
        check(ud2.string(forKey: vc + "theme") == "light", "absent theme with nightMode unset seeds light")
    }

    // THE SENTINEL ITSELF. R6 proved by mutation (2026-09-04) that deleting the
    // `guard ud.object(forKey: prefix + "profileKeysV1") == nil else { return }` left the suite at
    // 1359/0 — every existing check passed on absent-only seeding alone, so the guard the section
    // comment calls "the point" was untested. This case fails without it: marker present, the
    // neutral key ABSENT, and a legacy value that WOULD seed it. A migration that re-runs here would
    // resurrect a key the user deliberately cleared, and it can never be undone (one shot per domain).
    let suiteS = suite + ".sentinel"
    if let udS = UserDefaults(suiteName: suiteS) {
        defer { udS.removePersistentDomain(forName: suiteS) }
        udS.set(true, forKey: vc + "profileKeysV1")
        udS.set(true, forKey: vc + "rightHandDrive")
        udS.set(true, forKey: vc + "nightMode")
        VehicleProfileKeyMigration.run(prefix: vc, ud: udS)
        check(udS.object(forKey: vc + "driverPosition") == nil,
              "sentinel present ⇒ migration does NOT seed driverPosition even with rightHandDrive set")
        check(udS.object(forKey: vc + "theme") == nil,
              "sentinel present ⇒ migration does NOT seed theme even with nightMode set")
    }

    // A store with neither legacy key seeds the defaults.
    let suite3 = suite + ".c"
    if let ud3 = UserDefaults(suiteName: suite3) {
        defer { ud3.removePersistentDomain(forName: suite3) }
        VehicleProfileKeyMigration.run(prefix: vc, ud: ud3)
        check(ud3.string(forKey: vc + "driverPosition") == "left" && ud3.string(forKey: vc + "theme") == "light",
              "empty store seeds left/light")
    }
}

// MARK: - Panel envelope (PanelRule, 2026-09-07)

/// `VehicleConfigModel.clampInPlace()` stores `PanelRule.clamped` for the main and alt panels and
/// nothing else, and the Settings form, the import pre-flight and the control-socket door all read
/// the same enum — so these pin the ONE rule every panel dimension passes through. The bug they
/// guard: a per-axis envelope (W 800–3840, H 480–2160) that squared a portrait 2160x3840 panel to
/// 2160x2160 and made the wired portrait sweep measure nothing for five cases.
private func settingsPanelRuleTests() {
    section("Settings — panel envelope (PanelRule)")
    func same(_ w: Int, _ h: Int, _ why: String) {
        let c = PanelRule.clamped(width: w, height: h)
        check(c.width == w && c.height == h, "\(w)x\(h) survives the clamp unchanged — \(why) (got \(c.width)x\(c.height))")
        check(PanelRule.verdict(width: w, height: h) == nil, "\(w)x\(h) has no verdict")
        check(PanelRule.clampNote("Panel", requested: (w, h), applied: c) == nil, "\(w)x\(h) yields no clamp note")
        let ax = PanelRule.axisOutOfRange(width: w, height: h)
        check(!ax.width && !ax.height, "\(w)x\(h) paints neither field red")
    }
    func moves(_ w: Int, _ h: Int, to ew: Int, _ eh: Int, _ why: String) {
        let c = PanelRule.clamped(width: w, height: h)
        check(c.width == ew && c.height == eh, "\(w)x\(h) → \(ew)x\(eh) — \(why) (got \(c.width)x\(c.height))")
        check(PanelRule.verdict(width: w, height: h) != nil, "\(w)x\(h) has a verdict")
        let note = PanelRule.clampNote("Panel", requested: (w, h), applied: c)
        check(note?.contains("\(w) × \(h)") == true && note?.contains("\(ew) × \(eh)") == true,
              "\(w)x\(h): the clamp note names both the requested and the stored size (\(note ?? "nil"))")
        let ax = PanelRule.axisOutOfRange(width: w, height: h)
        check(ax.width == (ew != w) && ax.height == (eh != h), "\(w)x\(h) paints exactly the moved axis red")
    }

    // The envelope is orientation-agnostic: the same range on both axes, floor from the floor's short side.
    check(PanelRule.maxSide == 3840, "each axis admits up to 3840")
    check(PanelRule.minSide == 480, "each axis admits down to 480 (the floor's short side)")
    check(PanelRule.minimumSize(width: 2160, height: 3840) == ViewArea2Rule.minimumSize(width: 2160, height: 3840)
          && PanelRule.minimumSize(width: 3840, height: 2160) == ViewArea2Rule.minimumSize(width: 3840, height: 2160),
          "the panel floor IS ViewArea2Rule's floor — one source of truth, chosen by the panel's own aspect")

    // THE BUG: portrait 4K, and the portrait sweep's every panel, round-trip unchanged.
    same(2160, 3840, "portrait 4K — the wired portrait sweep's panel (was squared to 2160x2160)")
    same(3840, 2160, "landscape 4K, device-proven 2026-09-07")
    same(1080, 1920, "portrait FHD")
    same(1440, 2560, "portrait QHD")
    same(720, 1280, "portrait HD")
    same(900, 1200, "the Simulator's own Portrait.yaml")
    same(480, 800, "the portrait product floor exactly")
    same(800, 480, "the landscape product floor exactly")
    same(700, 1000, "portrait with width < 800 — legal now; the per-axis 800 minimum rejected it")
    same(3840, 3840, "square at the ceiling — square reads as landscape and 3840 >= 800")
    same(2160, 2160, "the shape the bug produced is itself legal, which is why nothing complained")

    // The orientation-aware floor still rejects what it should — by the panel's OWN aspect.
    moves(480, 480, to: 800, 480, "square reads as landscape → 800x480")
    moves(400, 800, to: 480, 800, "portrait below the portrait floor's width")
    moves(480, 700, to: 480, 800, "portrait below the portrait floor's height")
    moves(800, 400, to: 800, 480, "landscape below the landscape floor's height")
    moves(750, 450, to: 800, 480, "dhu-6in stays clamped to Standard (DESIGN.md §8)")
    moves(100, 100, to: 800, 480, "tiny square → landscape floor")
    moves(4000, 3000, to: 3840, 3000, "landscape over the ceiling on width only")
    moves(2160, 4000, to: 2160, 3840, "portrait over the ceiling on height only — height is NOT capped at 2160")
    moves(5000, 5000, to: 3840, 3840, "both axes over the ceiling")
    // A clamp never flips orientation: the floor for an orientation has that orientation.
    for (w, h) in [(500, 490), (490, 500), (1, 2), (2, 1)] {
        let c = PanelRule.clamped(width: w, height: h)
        check((w >= h) == (c.width >= c.height), "\(w)x\(h): clamping preserves orientation (→ \(c.width)x\(c.height))")
    }
    // Hostile input: the fields are free-typed Ints and the door takes any Int.
    _ = PanelRule.clamped(width: Int.max, height: Int.min)
    _ = PanelRule.verdict(width: Int.min, height: Int.max)
    check(PanelRule.clamped(width: Int.max, height: Int.min) == (3840, 480), "Int.max x Int.min → 3840x480, no trap")
    check(PanelRule.clamped(width: 0, height: 0) == (800, 480), "0x0 → the landscape floor")
    // The verdict names the orientation it applied, so the owner learns WHICH floor bit.
    check(PanelRule.verdict(width: 400, height: 800)?.contains("portrait") == true, "portrait verdict says portrait")
    check(PanelRule.verdict(width: 800, height: 400)?.contains("landscape") == true, "landscape verdict says landscape")
    check(PanelRule.verdict(width: 2160, height: 4000)?.contains("4000 > 3840") == true, "over-ceiling verdict quotes the bound")
    check(PanelRule.envelopeDescription.contains("480–3840") && PanelRule.envelopeDescription.contains("480 × 800 portrait"),
          "the envelope description quotes both the axis range and the portrait floor")
}

// MARK: - Second main view area (ViewArea2Rule, 2026-09-05)

/// The model's `viewArea2Verdict` is `ViewArea2Rule.verdict` over the live fields, so these pin the
/// rule itself. The rejection cases are device-measured teardowns/lockouts or the owner's product
/// floor (docs/carplay/06_AV_PIPELINE.md §3), and each check would FAIL if its rule were removed — a
/// stub returning nil fails every rejection check, a stub returning a fixed string fails the accepts.
private func settingsViewArea2RuleTests() {
    section("Settings: second view area — containment, parity, positivity, product floor")
    typealias R = ViewArea2Rule

    // Device-proven AND above the product floor: landscape 1600x960@800,0 on 2400x960 (the GOOD run;
    // 800+1600 == 2400, the boundary is INCLUSIVE) and floating portrait 1080x1600@0,160 on 1080x1920.
    check(R.verdict(x: 800, y: 0, w: 1600, h: 960, panelW: 2400, panelH: 960) == nil,
          "1600x960@800,0 on 2400x960 is legal (inclusive right edge)")
    check(R.verdict(x: 0, y: 160, w: 1080, h: 1600, panelW: 1080, panelH: 1920) == nil,
          "1080x1600@0,160 portrait (device-proven, touches no edge) is legal — no edge-touch rule exists")
    check(R.verdict(x: 0, y: 0, w: 800, h: 480, panelW: 1920, panelH: 1080) == nil, "800x480 is exactly the landscape floor")
    check(R.verdict(x: 0, y: 0, w: 480, h: 800, panelW: 1080, panelH: 1920) == nil, "480x800 is exactly the portrait floor")
    check(R.verdict(x: 0, y: 0, w: 480, h: 480, panelW: 1080, panelH: 1920) != nil, "480x480 is landscape (w >= h) and fails the 800 width floor")

    // 1. Containment — the device-proven TEARDOWN (1416x842@492,59 on a 1416x842 panel → 1908x901).
    // (That rect ALSO has an odd Y; containment must be what is reported, being the earlier rule.)
    let spill = R.verdict(x: 492, y: 59, w: 1416, h: 842, panelW: 1416, panelH: 842)
    check(spill != nil && spill!.contains("1908 × 901") && spill!.contains("1416 × 842") && !spill!.contains("Odd"),
          "1416x842@492,59 on 1416x842 is refused for containment first and the verdict quotes extent + panel: \(spill ?? "nil")")
    check(R.verdict(x: 802, y: 0, w: 1600, h: 960, panelW: 2400, panelH: 960) != nil, "two px past the right edge is refused (all even, so this is containment)")
    check(R.verdict(x: 0, y: 2, w: 2400, h: 960, panelW: 2400, panelH: 960) != nil, "two px past the bottom edge is refused")
    // Containment outranks the floor: a rect that is BOTH outside and too small reports the teardown.
    let both = R.verdict(x: 2000, y: 0, w: 500, h: 100, panelW: 2400, panelH: 960)
    check(both != nil && both!.contains("outside"), "containment is reported before the floor: \(both ?? "nil")")
    // Overflow must refuse, not trap (free-typed Ints).
    check(R.verdict(x: Int.max, y: 0, w: 100, h: 100, panelW: 2400, panelH: 960) != nil, "x + w overflow is a refusal, not a crash")

    // 2. Parity — ALL FOUR values even. One-pixel isolation on 1080x1920 (each of these rects is
    // otherwise legal): 356x400 rendered / 357x400 tore down; 601x400 (odd width, far above any
    // floor); 600x400@240,761 (odd origin Y only). Tested here at floor-clearing sizes so the
    // verdict cannot be the floor's.
    let oddW = R.verdict(x: 240, y: 760, w: 801, h: 800, panelW: 1080, panelH: 1920)
    check(oddW != nil && oddW!.contains("Odd") && oddW!.contains("width 801"), "odd width is a parity refusal naming the value: \(oddW ?? "nil")")
    let oddY = R.verdict(x: 0, y: 761, w: 1080, h: 800, panelW: 1080, panelH: 1920)
    check(oddY != nil && oddY!.contains("Odd") && oddY!.contains("Y 761"), "odd origin Y alone is a parity refusal: \(oddY ?? "nil")")
    check(R.verdict(x: 1, y: 0, w: 1600, h: 960, panelW: 2400, panelH: 960)?.contains("X 1") == true, "odd origin X is refused")
    check(R.verdict(x: 0, y: 0, w: 1600, h: 959, panelW: 2400, panelH: 960)?.contains("height 959") == true, "odd height is refused")
    let two = R.verdict(x: 1, y: 0, w: 1601, h: 960, panelW: 2400, panelH: 960)
    check(two != nil && two!.contains("X 1") && two!.contains("width 1601"), "every odd value is named, not just the first: \(two ?? "nil")")
    // The historical unexplained teardown: 1600x842@800,59 on 2400x960 is CONTAINED (2400x901) and
    // above the floor — parity (odd Y) is the only rule left, and it must be the one reported.
    let hist = R.verdict(x: 800, y: 59, w: 1600, h: 842, panelW: 2400, panelH: 960)
    check(hist != nil && hist!.contains("Odd") && hist!.contains("Y 59"), "1600x842@800,59 is explained by odd Y: \(hist ?? "nil")")
    // Parity outranks the floor: an odd, undersized rect reports the teardown, not the lockout.
    let oddSmall = R.verdict(x: 240, y: 760, w: 357, h: 400, panelW: 1080, panelH: 1920)
    check(oddSmall != nil && oddSmall!.contains("Odd"), "357x400 reports parity (teardown) before the floor (lockout): \(oddSmall ?? "nil")")

    // 3. Positive dimensions — the `Pixel display view dimension(s) set to 0` validator. Zero is even,
    // so parity does not mask it.
    let zeroW = R.verdict(x: 0, y: 0, w: 0, h: 960, panelW: 2400, panelH: 960)
    check(zeroW != nil && zeroW!.contains("positive"), "zero width is refused as non-positive: \(zeroW ?? "nil")")
    check(R.verdict(x: 0, y: 0, w: 1600, h: 0, panelW: 2400, panelH: 960)?.contains("positive") == true, "zero height is refused")
    check(R.verdict(x: -2, y: 0, w: 1600, h: 960, panelW: 2400, panelH: 960)?.contains("non-negative") == true, "negative (even) origin is refused")

    // 4. The PRODUCT floor — 800x480 landscape / 480x800 portrait, orientation from the area's own
    // aspect. A lockout, not a teardown, and the verdict says so. Both axes, both sides of the bound.
    check(R.minimumSize(width: 1600, height: 960) == (800, 480), "landscape floor is 800x480")
    check(R.minimumSize(width: 480, height: 800) == (480, 800), "portrait floor is 480x800")
    check(R.minimumSize(width: 500, height: 500) == (800, 480), "a square counts as landscape (w >= h)")
    // 600x400@240,760 RENDERS on the device — and the app declines to offer it: 600 < 800.
    let six = R.verdict(x: 240, y: 760, w: 600, h: 400, panelW: 1080, panelH: 1920)
    check(six != nil && six!.contains("width 600 < 800") && six!.contains("800 × 480") && six!.contains("does not support"),
          "600x400 (renders on hardware) is refused by the product floor, naming the dimension and the lockout: \(six ?? "nil")")
    let narrow = R.verdict(x: 0, y: 0, w: 798, h: 480, panelW: 1920, panelH: 1080)
    check(narrow != nil && narrow!.contains("width 798 < 800") && !narrow!.contains("height 480 <"), "798x480 fails the width floor only: \(narrow ?? "nil")")
    let short = R.verdict(x: 0, y: 0, w: 800, h: 478, panelW: 1920, panelH: 1080)
    check(short != nil && short!.contains("height 478 < 480") && !short!.contains("width 800 <"), "800x478 fails the height floor only: \(short ?? "nil")")
    let portraitShort = R.verdict(x: 0, y: 0, w: 480, h: 798, panelW: 1080, panelH: 1920)
    check(portraitShort != nil && portraitShort!.contains("480 × 800") && portraitShort!.contains("height 798 < 800"),
          "480x798 quotes the PORTRAIT floor: \(portraitShort ?? "nil")")
    check(R.verdict(x: 0, y: 0, w: 478, h: 800, panelW: 1080, panelH: 1920)?.contains("width 478 < 480") == true, "478x800 fails the portrait width floor")
    // 480x300: the on-device lockout case from the bench — refused up front for the same reason.
    check(R.verdict(x: 240, y: 760, w: 480, h: 300, panelW: 1080, panelH: 1920)?.contains("does not support") == true,
          "480x300 (device lockout) is refused as too small")
    // The floor is a product constant, not panel-derived: the same rect fails on a bigger panel too.
    check(R.verdict(x: 0, y: 0, w: 600, h: 400, panelW: 2400, panelH: 960) != nil, "floor does not relax on a larger panel")
}

// MARK: - ViewAreaSpec (ControlServer `viewarea arm`, 2026-09-07)

/// The `WxH@X,Y` grammar the bench tools share. Strict by design: every rejection below is a spelling
/// that `Int()` alone would have accepted (sign, whitespace, the probe's `:initial` suffix).
private func settingsViewAreaSpecTests() {
    section("ViewAreaSpec: WxH@X,Y parse (ControlServer viewarea arm)")
    check(ViewAreaSpec.parse("800x480@0,0") == ViewAreaSpec(x: 0, y: 0, w: 800, h: 480), "800x480@0,0 parses")
    check(ViewAreaSpec.parse("1600x960@800,0") == ViewAreaSpec(x: 800, y: 0, w: 1600, h: 960), "1600x960@800,0 parses")
    check(ViewAreaSpec.parse("1600x960@800,0")?.description == "1600x960@800,0", "description round-trips the canonical spelling")
    // Parsing is not legality: an odd, oversized or below-floor rect parses and is judged by ViewArea2Rule.
    check(ViewAreaSpec.parse("357x400@240,760") == ViewAreaSpec(x: 240, y: 760, w: 357, h: 400), "odd width parses (the verdict is ViewArea2Rule's job)")
    check(ViewAreaSpec.parse("800x480@0,0:initial") == nil, ":initial is refused — the app model does not author it")
    check(ViewAreaSpec.parse("800x480") == nil, "missing origin is refused")
    check(ViewAreaSpec.parse("800@0,0") == nil, "missing height is refused")
    check(ViewAreaSpec.parse("x480@0,0") == nil, "empty width is refused")
    check(ViewAreaSpec.parse("800x480@0") == nil, "missing Y is refused")
    check(ViewAreaSpec.parse("800x480@0,") == nil, "empty Y is refused")
    check(ViewAreaSpec.parse("-800x480@0,0") == nil, "sign is refused")
    check(ViewAreaSpec.parse("800x480@+0,0") == nil, "plus sign is refused")
    check(ViewAreaSpec.parse("800 x480@0,0") == nil, "whitespace is refused")
    check(ViewAreaSpec.parse("800x480@0,0@1,1") == nil, "a second @ is refused")
    check(ViewAreaSpec.parse("") == nil, "empty string is refused")
    check(ViewAreaSpec.parse("off") == nil, "'off' is not a rect (the verb handles it before parsing)")
}
