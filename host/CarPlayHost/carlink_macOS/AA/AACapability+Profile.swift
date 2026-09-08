import Foundation

// The neutral-profile → Android Auto bridge (W2, 2026-09-04; DESIGN.md §6, §8).
//
// Lives in its own file, NOT in AACapability.swift, for one reason: AACapability.swift must compile
// without the contract types (VehicleProfile.swift, FeatureMatrix.swift) — its plain-value init is
// the renderer's seam — and this file is the ONLY place the AA renderer names one. All three are
// Foundation-only and are in `tests/run_tests.sh`'s `swiftc` list (Phase 2, landed 2026-09-04), so
// the harness exercises this bridge directly (`AACapability(profile: .default, adapter: .default)`
// in tests/SettingsTests.swift). Keep it AppKit-free; `.auto` theme resolution stays at the call site.
//
// It replaced `AACapability.init(config:)` in SettingsWindow.swift (deleted in Phase 2), the
// "six-field bridge" that borrowed mainWidth/mainHeight/maxFPS/name/nightMode/rightHandDrive from
// the CarPlay-shaped model and left everything else (dpi, HEVC, restrictions, voice rate, metadata
// feeds, touchscreen presence) to constants and `AA_*` environment variables.
extension AACapability {

    /// Render the neutral vehicle profile as this head unit's Android Auto declaration.
    ///
    /// - `profile` / `adapter`: the neutral document's two halves (`VehicleConfigModel.profile` /
    ///   `.adapterSettings`, W1's extension). Nothing on the ADAPTER side reaches the AA wire —
    ///   `android_auto:` rides the pushed YAML — so `adapter` only contributes a note when AA is
    ///   disabled there and this declaration is therefore idle.
    /// - `autoThemeIsDark`: how `appearance.theme == .auto` resolves. The profile defines `auto` as
    ///   "follow this Mac's appearance" (DESIGN.md §10: DHU `uitheme` is NOT a wire field; gearhead
    ///   derives its theme from the `night_mode` sensor only), and the ONLY honest source for that is
    ///   `NSApp.effectiveAppearance` — which is AppKit, and this file must stay AppKit-free for the
    ///   harness. So the AppDelegate call site resolves it and passes the Bool; a headless caller
    ///   gets light (false) plus a note saying so. Resolved once, at session start: a Mac appearance
    ///   change mid-session is not pushed (the Controls window's night toggle is the live path).
    /// - `warn`: every approximation is logged through it AND recorded in `negotiationNotes`.
    init(profile: VehicleProfile, adapter: AdapterSettings,
         autoThemeIsDark: Bool? = nil,
         warn: (String) -> Void = { NSLog("[AA] \($0)") }) {
        var notes: [String] = []
        func note(_ s: String) { notes.append(s); warn(s) }

        // ── Appearance (defect 2, W2's part) ────────────────────────────────────────────────────
        let night: Bool
        switch profile.appearance.theme {
        case .dark:  night = true
        case .light: night = false
        case .auto:
            if let dark = autoThemeIsDark {
                night = dark
                note("theme auto -> night_mode \(dark) from this Mac's appearance at session start")
            } else {
                night = false
                note("theme auto -> night_mode false: no system appearance supplied (headless caller)")
            }
        }
        // Status-bar hide requests (DHU `hideclock` / `hidesignal` / `hidebattery`) are AUTHORED in
        // the profile but NOT SENT: the SDR field numbers for them are unconfirmed, and an
        // unrecognised field in service discovery is exactly the kind of thing that costs the whole
        // session. Recorded so the owner sees the gap rather than assuming it works.
        if !profile.appearance.statusBar.isDefault {
            note("status-bar hide requests (clock/signal/battery) recorded — not sent to Android Auto "
                 + "until the service-discovery field numbers are confirmed")
        }

        // ── Display ─────────────────────────────────────────────────────────────────────────────
        // Insets: AA's `contentinsets`/`stablecontentinsets` are the safe-area analogue, but the
        // SDR field is unconfirmed (DESIGN.md §10 — and AA `margins`, which ARE sent, are a
        // different mechanism: codec pixels cropped to fit a non-tier panel, derived below).
        let ins = profile.display.insets
        if !ins.isZero {
            note("panel insets L\(ins.left) T\(ins.top) R\(ins.right) B\(ins.bottom) recorded — not sent "
                 + "to Android Auto until the content-insets field is confirmed")
        }
        if profile.altDisplay.enabled {
            note("secondary display is not expressed to Android Auto (single video sink)")
        }

        // ── Driver seat (defect 2): a genuine ternary, not a Bool ───────────────────────────────
        let seat: DriverSeat
        switch profile.driverPosition {
        case .left:   seat = .left
        case .right:  seat = .right
        case .center: seat = .center     // wire 3; AACapability notes it as unverified
        }

        // ── Driving restrictions: the plumbing gap (DESIGN.md §10) ──────────────────────────────
        // `restrictions.declared` ⇒ the profile's set, mapped by the ONE mapping table
        // (`FeatureMatrix.androidAutoDrivingStatus`, never re-derived here) ⇒ the mask
        // `AASession.setDrivingRestricted(true)` sends. Not declared ⇒ `drivingDefault`, which is
        // today's behaviour and bit-identical to `.typicalDriving` (26).
        let mask: DrivingRestrictions
        if profile.restrictions.declared {
            let set = profile.restrictions.set
            mask = DrivingRestrictions(rawValue: UInt64(FeatureMatrix.androidAutoDrivingStatus(set)))
            let unexpressed = FeatureMatrix.unexpressedRestrictions(set, on: .androidAuto)
            if !unexpressed.isEmpty {
                note("no Android Auto driving_status bit for: "
                     + unexpressed.map(\.title).joined(separator: ", ") + " — not expressed")
            }
        } else {
            mask = .drivingDefault
            // Audit F2-5 (2026-09-04): `declared` is driven by CarPlay's "Limited UI" switch
            // (VehicleConfigModel+Profile.swift), so the AA-only members — video, voice input,
            // configuration — can be ticked while that switch is off, and this branch then sends
            // the default mask with no trace. Say so, but only when the authored set would change
            // the wire: a set that maps to 26 (`typicalDriving`) is bit-identical to the default and
            // a note there cries wolf. NO_VIDEO keeps needing both switches on purpose — it blanks
            // the projection — so this is a note, not a fallback to the set.
            let set = profile.restrictions.set
            let setMask = FeatureMatrix.androidAutoDrivingStatus(set)
            if !set.isEmpty, UInt64(setMask) != DrivingRestrictions.drivingDefault.rawValue {
                let notSent = FeatureMatrix.restrictionMapping.filter { m in
                    guard set.contains(m.restriction), let bit = m.androidAutoBit else { return false }
                    return UInt64(bit) & DrivingRestrictions.drivingDefault.rawValue == 0
                }.map(\.title)
                note("driving restrictions are authored but not declared (the Limited UI switch is off) — "
                     + "driving mode sends the default driving_status \(DrivingRestrictions.drivingDefault.rawValue), "
                     + "not the set's \(setMask)"
                     + (notSent.isEmpty ? "" : "; not sent: " + notSent.joined(separator: ", ")))
            }
        }

        // ── Branding / powertrain: name only, recorded ──────────────────────────────────────────
        if profile.branding.advertise {
            note("branding icon is not advertised to Android Auto (name only: headunit_info + display_name)")
        }
        if !profile.powertrain.engines.isEmpty || !profile.powertrain.connectors.isEmpty {
            note("powertrain (engines/connectors) recorded — not sent to Android Auto yet")
        }

        // ── Input ───────────────────────────────────────────────────────────────────────────────
        let touchscreen = profile.input.touchscreen != nil
        if !touchscreen {
            note("no touchscreen in the profile — declaring a controller-only head unit (keycodes only)")
        } else if profile.input.primary == .rotary {
            note("primary input rotary with a touchscreen present: gearhead treats D-Pad focus as "
                 + "secondary while a touchscreen is declared (device-observed: focus ring inconsistent)")
        }

        // ── Adapter ─────────────────────────────────────────────────────────────────────────────
        if !adapter.androidAuto {
            note("Android Auto is disabled on the adapter (android_auto: false) — this declaration is "
                 + "idle until it is enabled")
        }

        let panel = profile.display.panel
        self.init(mainWidth: panel.width, mainHeight: panel.height, maxFPS: panel.maxFPS, dpi: panel.dpi,
                  name: profile.identity.headUnitName,
                  nightMode: night, driverSeat: seat,
                  hevcAllowed: profile.video.hevcAllowed,
                  preferHEVC: profile.androidAuto.preferHEVC,
                  fitPanelWithMargins: profile.androidAuto.fitPanelWithMargins,
                  drivingMask: mask,
                  voiceRateHz: profile.audio.voiceRateHz,
                  telephonySink: profile.audio.telephonyOverProjection,
                  metadata: MetadataServices(mediaPlayback: profile.metadata.nowPlaying,
                                             navigationStatus: profile.metadata.navigation,
                                             phoneStatus: profile.metadata.telephony),
                  touchscreen: touchscreen,
                  notes: notes, warn: warn)
    }
}
