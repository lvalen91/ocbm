//! C-2 schema guards (reviewer R4): the three properties the C-2 field docstrings PROMISE but that
//! no test pinned — an unrecognised `touchScreenMode` must not fail the document, an unmodelled
//! `hidConfig` key must not fail the document, and the whole app-emitted document (with the new
//! `steeringWheelSupport` line and the `accessoryName:` / `iapConfig:` tail) must still parse into
//! the SAME emitted values it produced before C-2.
//!
//! Why these three and not more: every one of them is the same failure mode, and it is the worst one
//! this module has. `airplayd::load_device_config` treats a parse error as "use the built-in
//! defaults", so ANY of these turning into a hard serde error silently reverts the owner's
//! resolution, HEVC, appDrivenSetup, audio set and metadata tier at once — a whole-config regression
//! reported as a single mistyped enum value.

use receiver::vehicle_config::VehicleConfig;

/// Byte-for-byte the document `SettingsWindow.swift`'s `yaml` getter emits with the C-2 additions,
/// Swift multiline-literal indentation stripped. Note what is and is NOT in the `hidConfig` block:
/// the app emits `touchScreenMode` (derived from its `touchScreenHighFidelity` toggle) and NOT
/// `touchScreenHighFidelity` itself, and it emits `steeringWheelSupport` LAST, after
/// `touchScreenSupportsCancel`.
const APP_DOC: &str = concat!(
    "name: \"CarLink Widescreen\"\n",
    "version: 1\n",
    "wireless: true\n",
    "hot_handover: false\n",
    "pairing: just_works\n",
    "rightHandDrive: false\n",
    "nightMode: false\n",
    "displayPanelsConfig:\n",
    "  mainDisplayPanel:\n",
    "    displayPanelID: DisplayPanel.Main\n",
    "    pixelDimensions:\n",
    "      width: 1920\n",
    "      height: 1080\n",
    "  altDisplayPanels: []\n",
    "videoStreamsConfig:\n",
    "  mainVideoStream:\n",
    "    videoStreamID: VideoStream.Main\n",
    "    pixelDimensions:\n",
    "      width: 1920\n",
    "      height: 1080\n",
    "    maxFPS: 60\n",
    "    viewAreas:\n",
    "    - viewArea:\n",
    "        originX: 0\n",
    "        originY: 0\n",
    "        width: 1920\n",
    "        height: 1080\n",
    "      safeArea:\n",
    "        originX: 0\n",
    "        originY: 0\n",
    "        width: 1920\n",
    "        height: 1080\n",
    "      drawUIOutsideSafeArea: false\n",
    "    hidConfig:\n",
    "      dPadSupport: true\n",
    "      knobSupport: false\n",
    "      knobSupportsHomeAndBackButton: false\n",
    "      knobSupportsNudge: false\n",
    "      mediaButtonsSupport: true\n",
    "      telephonyButtonsSupport: false\n",
    "      touchpadSupport: false\n",
    "      touchpadButtonsSupport: false\n",
    "      touchScreenMode: High Fidelty\n",
    "      touchScreenSupportsCancel: true\n",
    "      steeringWheelSupport: true\n",
    "    primaryInput: Touchpad\n",
    "  altVideoStreams: []\n",
    "accessoryConfig:\n",
    "  enablesMainBufferedAudio: false\n",
    "  enablesHEVC: true\n",
    "  enablesUIAppearance: true\n",
    "  enablesMapAppearance: true\n",
    "  enablesCornerMasks: false\n",
    "  enablesVideoPlayback: true\n",
    "  enablesViewAreas: false\n",
    "  enablesEnhancedSiri: false\n",
    "  enablesFocusTransfer: false\n",
    "  enablesUIContext: false\n",
    "  enablesUISync: false\n",
    "  enablesFileTransfer: false\n",
    "  enablesLogTransfer: false\n",
    "  enablesVehicleDataProtocol: false\n",
    "  enablesDCX: false\n",
    "  appDrivenSetup: true\n",
    "audio:\n",
    "  wired:\n",
    "    preset: wired_pcm\n",
    "  wireless:\n",
    "    preset: wireless_8\n",
    "metadata:\n",
    "  tier: proven\n",
    // ---- the C-2 tail: iapConfigYAML, appended AFTER metadata ----
    "accessoryName: \"Owner's \\\"Roadster\\\" \\\\ EV\"\n",
    "iapConfig:\n",
    "  vehicleInfo:\n",
    "    engineTypes: [gasoline, electric]\n",
    "    chargingConnectors:\n",
    "      - type: ccs2\n",
    "        powerWatts: 150000\n",
    "      - type: nacs_dc\n",
    "  vehicleStatus:\n",
    "    capabilities: [range, rangeElectric]\n",
);

/// The same document with the whole C-2 tail removed — what the app emitted before this change for
/// an unconfigured install (`iapConfigYAML` returns "" when nothing is set).
fn app_doc_without_c2_tail() -> String {
    let cut = APP_DOC.find("accessoryName:").expect("tail marker");
    APP_DOC[..cut].to_string()
}

/// THE app/box drift guard. The tail the app now appends must (a) parse, (b) bind `accessoryName`
/// through `YamlEmit.quotedBody`'s escaping intact, and (c) change NOTHING the box already emitted.
///
/// (c) is the load-bearing half and is why the comparison is against the pre-C-2 document rather
/// than against a hand-built `DeviceConfig`: the risk is not that `accessoryName` is parsed wrong,
/// it is that appending two top-level keys after `metadata:` makes serde reject the whole document,
/// at which point `airplayd::load_device_config` silently reverts resolution, HEVC, appDrivenSetup
/// and the audio set to the compiled defaults.
#[test]
fn r4_c2_tail_parses_and_moves_nothing_the_box_already_emitted() {
    let base = receiver::info::DeviceConfig::default();
    let before = VehicleConfig::from_yaml(app_doc_without_c2_tail().as_bytes())
        .expect("the pre-C-2 app document must parse");
    let after = VehicleConfig::from_yaml(APP_DOC.as_bytes())
        .expect("appending accessoryName:/iapConfig: must not break the document");

    // (b) the escaped body survives the round trip exactly.
    assert_eq!(
        after.accessory_name.as_deref(),
        Some("Owner's \"Roadster\" \\ EV"),
        "YamlEmit.quotedBody escaping must round-trip through serde_yaml"
    );
    assert_eq!(before.accessory_name, None, "pre-C-2 documents carry no name");

    // (c) every emitted surface is unmoved. DeviceConfig's Debug covers the /info-shaping fields;
    // the four accessors below are the emitted values that are NOT in DeviceConfig at all, which is
    // precisely what a DeviceConfig-only comparison would miss.
    assert_eq!(
        format!("{:?}", before.apply(base.clone())),
        format!("{:?}", after.apply(base)),
        "the C-2 tail must not move a single /info value"
    );
    assert_eq!(before.accessory_config.enables_hevc, after.accessory_config.enables_hevc);
    assert_eq!(before.app_driven_setup(), after.app_driven_setup());
    assert_eq!(before.view_areas_enabled(), after.view_areas_enabled());
    assert_eq!(before.alt_screen(), after.alt_screen());
    // ...and the values are the ones the app actually asked for, so the assertion above is not
    // vacuously comparing two default configs.
    assert!(after.accessory_config.enables_hevc && after.app_driven_setup());
    assert_eq!(after.apply(receiver::info::DeviceConfig::default()).display_height, 1080);

    // The hidConfig block the app really emits binds all four C-2 fields.
    let hid = &after.video_streams_config.main_video_stream.hid_config;
    assert!(hid.dpad_support && hid.media_buttons_support && hid.steering_wheel_support);
    assert!(!hid.touchpad_support);
    assert_eq!(hid.touch_screen_mode, "High Fidelty");
}

/// `touch_screen_mode`'s docstring promises an unrecognised value "degrades to no touch bits with a
/// log at the point of USE instead of failing the WHOLE document here". Nothing pinned that. Turn
/// the field into a serde enum (the obvious "tighten it up" refactor) and this is the test that
/// stops it, because the cost is not a mis-parsed HID bit — it is the owner's resolution, HEVC,
/// appDrivenSetup and audio set all reverting to compiled defaults on the next push.
#[test]
fn r4_unrecognised_touch_screen_mode_never_costs_the_rest_of_the_document() {
    for mode in ["Ludicrous Fidelty", "high fidelty", "High Fidelity", "", "Low Fidelty"] {
        let y = format!(
            "displayPanelsConfig:\n  mainDisplayPanel:\n    pixelDimensions:\n      width: 800\n      height: 480\n\
             videoStreamsConfig:\n  mainVideoStream:\n    hidConfig:\n      touchScreenMode: \"{mode}\"\n\
             accessoryConfig:\n  enablesHEVC: true\n  appDrivenSetup: true\n"
        );
        let vc = VehicleConfig::from_yaml(y.as_bytes())
            .unwrap_or_else(|e| panic!("touchScreenMode {mode:?} must not fail the document: {e}"));
        assert_eq!(
            vc.video_streams_config.main_video_stream.hid_config.touch_screen_mode, mode,
            "the raw string is carried verbatim to the C-7 point of use"
        );
        // The rest of the pushed config is intact — this is the property that matters.
        assert!(vc.accessory_config.enables_hevc, "HEVC survived {mode:?}");
        assert!(vc.app_driven_setup(), "appDrivenSetup survived {mode:?}");
        let dev = vc.apply(receiver::info::DeviceConfig::default());
        assert_eq!((dev.display_width, dev.display_height), (800, 480), "resolution survived {mode:?}");
    }
}

/// Forward compatibility is this module's stated design property ("serde ignores unknown fields, so
/// the host may send the FULL Apple `VehicleConfig` today"), and C-2 is the first change to add
/// fields to `HidConfig` — the point at which someone reaches for `deny_unknown_fields` to "catch
/// typos". Apple has 21 `hidConfig` keys and the box models 7; the other 14 plus the app's own
/// sibling keys must keep riding through.
#[test]
fn r4_unmodelled_hid_keys_ride_through_without_dropping_the_document() {
    let y = "\
displayPanelsConfig:
  mainDisplayPanel:
    pixelDimensions:
      width: 1280
      height: 720
videoStreamsConfig:
  mainVideoStream:
    hidConfig:
      dPadSupport: true
      knobSupportsHomeAndBackButton: true
      knobSupportsNudge: true
      knobSupportsDPadNudgeFudge: true
      knobFocusTransferLeft: true
      knobFocusTransferRight: true
      knobFocusTransferUp: true
      knobFocusTransferDown: true
      lockPTFocus: true
      touchScreenSupportsCancel: true
      touchScreenSupportsMultiTouch: true
      touchpadButtonsSupport: true
      touchpadWidth: 100
      touchpadHeight: 60
      notificationButton: true
      touchScreenHighFidelity: true
      steeringWheelSupport: true
      touchScreenMode: High Fidelty
      aKeyAppleHasNotInventedYet: 42
    primaryInput: Touchpad
accessoryConfig:
  enablesHEVC: true
  someFutureAccessoryKey: true
";
    let vc = VehicleConfig::from_yaml(y.as_bytes())
        .expect("unmodelled Apple/app/future keys must be IGNORED, never fatal");
    let hid = &vc.video_streams_config.main_video_stream.hid_config;
    assert!(hid.dpad_support && hid.steering_wheel_support);
    assert_eq!(hid.touch_screen_mode, "High Fidelty");
    assert!(vc.accessory_config.enables_hevc);
    assert_eq!(
        vc.apply(receiver::info::DeviceConfig::default()).display_width,
        1280,
        "the rest of the document still applied"
    );
    // `touchScreenHighFidelity` is the app's INTERNAL toggle name and is deliberately NOT a box
    // field — the app derives `touchScreenMode` from it. If a future change adds it to `HidConfig`,
    // the two would be redundant inputs to the same feature bits; that is the drift this pins.
    assert!(
        !format!("{hid:?}").contains("high_fidelity"),
        "touchScreenHighFidelity must stay app-internal — touchScreenMode is the box's input"
    );
}

/// The parse-only claim, closed over EVERY emitted surface rather than just `DeviceConfig`.
///
/// `vehicle_config::tests::c2_hid_fields_are_parse_only_and_change_no_output` compares two
/// `DeviceConfig` Debug renderings, which is sound for what `apply()` writes but blind to the four
/// values that reach the wire WITHOUT going through `DeviceConfig` — `accessory_config.*` (read
/// straight off `VehicleConfig` by airplayd) and the `app_driven_setup()` / `view_areas_enabled()` /
/// `alt_screen()` accessors, each of which drives a SETUP `enabledFeatures` token. Wiring a C-2
/// field into any of those is a wire change that the Debug comparison passes clean.
#[test]
fn r4_c2_hid_fields_move_no_emitted_surface_including_the_non_device_config_ones() {
    let off = VehicleConfig::from_yaml(b"name: X\n").unwrap();
    let on = VehicleConfig::from_yaml(
        b"name: X\nvideoStreamsConfig:\n  mainVideoStream:\n    hidConfig:\n      touchpadSupport: true\n      steeringWheelSupport: true\n      mediaButtonsSupport: true\n      touchScreenMode: High Fidelty\n",
    )
    .unwrap();
    let base = receiver::info::DeviceConfig::default();
    assert_eq!(
        format!("{:?}", off.apply(base.clone())),
        format!("{:?}", on.apply(base)),
        "C-2 hid fields must not move any /info value"
    );
    // The surfaces DeviceConfig's Debug cannot see.
    assert_eq!(off.app_driven_setup(), on.app_driven_setup(), "SETUP appDrivenSetup moved");
    assert_eq!(off.view_areas_enabled(), on.view_areas_enabled(), "enabledFeatures viewAreas moved");
    assert_eq!(off.alt_screen(), on.alt_screen(), "enabledFeatures altScreen moved");
    assert_eq!(off.alt_dimensions(), on.alt_dimensions(), "alt display geometry moved");
    let (a, b) = (&off.accessory_config, &on.accessory_config);
    assert_eq!(format!("{a:?}"), format!("{b:?}"), "accessoryConfig (read directly by airplayd) moved");

    // Same closure for accessoryName, which the author's test only checks against `DeviceConfig.name`.
    let named = VehicleConfig::from_yaml(b"name: X\naccessoryName: Roadster\n").unwrap();
    assert_eq!(off.app_driven_setup(), named.app_driven_setup());
    assert_eq!(off.view_areas_enabled(), named.view_areas_enabled());
    assert_eq!(off.alt_screen(), named.alt_screen());
    assert_eq!(
        format!("{:?}", off.apply(receiver::info::DeviceConfig::default())),
        format!("{:?}", named.apply(receiver::info::DeviceConfig::default())),
    );
}

/// `accessoryName: ""` is not the same as an absent key, and C-6 must not treat it as one: an empty
/// `Some` applied to `/info` `name`, the Bonjour instance label and iAP2 param 0 would advertise a
/// nameless accessory. The app never emits it (it guards on `!name.isEmpty`), so this pins the
/// hand-authored / third-party-pusher case before C-6 makes it reachable.
#[test]
fn r4_empty_accessory_name_is_distinguishable_from_absent() {
    let empty = VehicleConfig::from_yaml(b"accessoryName: \"\"\n").unwrap();
    assert_eq!(empty.accessory_name.as_deref(), Some(""));
    let absent = VehicleConfig::from_yaml(b"name: X\n").unwrap();
    assert_eq!(absent.accessory_name, None);
    // Whitespace-only likewise reaches the box verbatim — the app trims, a hand-authored push does
    // not, so C-6's call site is where this has to be rejected, not here.
    let blank = VehicleConfig::from_yaml(b"accessoryName: \"   \"\n").unwrap();
    assert_eq!(blank.accessory_name.as_deref(), Some("   "));
}
