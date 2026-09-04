#!/usr/bin/env python3
"""Regenerate the app->box drift-guard fixture from the LIVE Swift emitter.

WHY THIS EXISTS. `the_apps_real_emitted_document_parses` in
crates/vendor/receiver/src/vehicle_config.rs is the only guard against app/box YAML
drift, and a HAND-TYPED version of it certified a BROKEN emitter: `va()`'s Swift
literal has no trailing newline, so an appended `initialURL:` line glued onto
`drawUIOutsideSafeArea: false` and serde rejected the WHOLE document -- silently
reverting resolution, HEVC, appDrivenSetup, audio and the metadata tier at once. The
fixture missed it because it had been hand-dedented and trimmed. A fixture nobody can
regenerate is hand-typed by definition, so "RE-GENERATE if you touch the emitter" was
an aspiration until this script made it executable.

HOW. Extracts VERBATIM out of SettingsWindow.swift: `VehicleConfigModel.yaml`,
`altDisplayPanelsYAML`, `clusterInitialURL`, `accessoryFields()` and `metadataYAML`.
Stubs the model's stored properties, runs the result under `xcrun swift`, and prints
the document. Also splices in the real `YamlEmit` from VehicleConfig.swift, so the
ESCAPER is exercised rather than stubbed to identity (which is how escaping bugs used
to produce a byte-identical fixture).

SCOPE, stated honestly because overstating it is what caused the incident: the
STRUCTURE, the concatenation seams, the escaper, the 16-key accessoryConfig block and
the metadata skip filter are all the real emitter's. The stored-property VALUES are
this script's stubs. STILL STUBBED, so their seams are NOT covered: `oemIconVariants`
and `iapConfigYAML`. Extend the harness before claiming
otherwise -- and update THIS paragraph when you do, because a stale scope note here is
exactly the kind of comment this repo keeps being misled by.

  python3 tools/regen_app_yaml_fixture.py [repo-root]        # default: cwd
"""
import subprocess, sys, os, tempfile

def block(src, startpat):
    i = next(n for n, l in enumerate(src) if startpat in l)
    depth = 0; out = []
    for n in range(i, len(src)):
        l = src[n]; out.append(l)
        depth += l.count('{') - l.count('}')
        if depth == 0 and n > i:
            break
    return '\n'.join(out)

root = sys.argv[1] if len(sys.argv) > 1 else '.'
p = os.path.join(root, 'host/CarPlayHost/carlink_macOS/App/SettingsWindow.swift')
src = open(p).read().split('\n')
harness = '''import Foundation
struct AudioFormatRow { var streamType = 102; var audioType = "media"; var input = "none"; var output = "aac_lc_48k_stereo" }

final class VehicleConfigModel {
    var name = "CarLink Widescreen"
    var wirelessEnabled = true, hotHandover = false, pairingNumericComparison = false, androidAutoEnabled = true
    var rightHandDrive = false, nightMode = false
    var mainWidth = 1920, mainHeight = 1080, maxFPS = 60
    var altWidth = 640, altHeight = 480, altFPS = 30
    var mainSafeLeft = 0, mainSafeTop = 0, mainSafeRight = 0, mainSafeBottom = 0
    var mainDrawOutsideSafe = false
    var altSafeLeft = 0, altSafeTop = 0, altSafeRight = 0, altSafeBottom = 0
    var altDrawOutsideSafe = false
    var altVideoEnabled = true          // <-- the CLUSTER-ON branch; flip for the OFF document
    var dPadSupport = true, knobSupport = false, knobSupportsHomeAndBackButton = false
    var knobSupportsNudge = false, mediaButtonsSupport = true, telephonyButtonsSupport = false
    var touchpadSupport = false, touchpadButtonsSupport = false
    var touchScreenHighFidelity = true, touchScreenSupportsCancel = true, steeringWheelSupport = true
    var touchScreenSupportsMultiTouch = false
    var primaryInput = "Touchpad"
    var limitedUIConfigEnabled = true, oemIconEnabled = true, oemIconBase64 = "iVBORw0KGgo="
    // HOSTILE ON PURPOSE. This is the only free text in the document that reaches YamlEmit.quotedBody,
    // and the escaper is the whole reason this seam matters: an unescaped `"` closes the scalar early
    // and an unescaped `\` is worse (`\b` is a VALID escape, so it silently yields a backspace with no
    // parse error at all). Either one takes out the ENTIRE pushed document. The trailing BEL must be
    // stripped as a Cc control.
    var oemIconLabel = "Owner's \\"Roadster\\" \\\\ EV\\u{7}"
    var oemIconVisible = true
    // The 16 accessoryConfig booleans, real values. The box parses only SIX of these; putting all
    // sixteen through the real emitter is what lets the Rust fixture pin that serde ignores the other
    // ten instead of failing the document.
    var enablesMainBufferedAudio = false, enablesHEVC = true, enablesUIAppearance = true
    var enablesMapAppearance = true, enablesCornerMasks = false, enablesVideoPlayback = false
    var enablesViewAreas = false, enablesEnhancedSiri = false, enablesFocusTransfer = false
    var enablesUIContext = false, enablesUISync = false, enablesFileTransfer = false
    var enablesLogTransfer = false, enablesVehicleDataProtocol = false, enablesDCX = false
    var appDrivenSetup = true
    var metadataTier = "proven"
    // Deliberately hostile: a bare `]` and a `:` must be FILTERED OUT by the real emitter, not
    // interpolated -- either would malform the whole document.
    var metadataSkip = "voice_over_cursor, bad]name, also:bad, call_history"
    // All TEN limitedUI toggles. Only SIX reach /info (Apple's airPlayElements); the other four are
    // round-trip only. Emitting all ten through the real emitter is what lets the Rust side pin that
    // asymmetry -- including the app `longAlerts` -> box `longUserAlert` RENAME, where a mismatch
    // would silently drop a restriction the owner asked for.
    var limitedUISoftKeyboard = true, limitedUISoftPhoneKeypad = false, limitedUIMusicLists = true
    var limitedUINonMusicLists = false, limitedUIJapanMaps = false, limitedUILongAlerts = true
    var limitedUIPairedDevices = true, limitedUIThemeCustomization = false
    var limitedUIAutomakerSettings = false, limitedUIAutomakerSettingsInfoButton = true
    func oemIconVariants() -> [(Int,String)] { [(120,"iVBORw0KGgo="),(180,"iVBORw0KGgp=")] }
    // CUSTOM audio mode on purpose: the preset branches are one-liners, but the `custom` branch is
    // where the app builds `type:` values by hand — and 107 (AuxIn) is one the box's SETUP dispatch
    // cannot serve, so this fixture proves the app CAN emit it and the box DROPS it rather than
    // advertising a stream it would then refuse. That end-to-end contract had no test.
    var audioMode = "custom"
    var audioFormats = [
        AudioFormatRow(streamType: 102, audioType: "media", input: "none", output: "aac_lc_48k_stereo"),
        AudioFormatRow(streamType: 107, audioType: "speechRecognition", input: "aac_eld_16k_mono", output: "aac_eld_16k_mono"),
        AudioFormatRow(streamType: 101, audioType: "alert", input: "none", output: "none"),
    ]
    var iapConfigYAML = ""
}
''' + block(open(os.path.join(root, 'host/CarPlayHost/carlink_macOS/App/VehicleConfig.swift'))
                .read().split('\n'), 'enum YamlEmit {') + '\n' \
    + 'extension VehicleConfigModel {\n' \
    + block(src, '    var yaml: String {') + '\n' \
    + '    ' + [l for l in src if 'static let clusterInitialURL' in l][0].strip() + '\n' \
    + block(src, '    private var altDisplayPanelsYAML: String {') + '\n' \
    + block(src, '    private func accessoryFields()') + '\n' \
    + block(src, '    private var metadataYAML: String {') + '\n' \
    + block(src, '    private var audioYAML: String {') + '\n' \
    + block(src, '    private func limitedUIFields()') + '\n}\n' \
    + 'FileHandle.standardOutput.write(VehicleConfigModel().yaml.data(using: .utf8)!)\n'
d = tempfile.mkdtemp()
f = os.path.join(d, 'emit.swift')
open(f, 'w').write(harness)
out = subprocess.run(['xcrun', 'swift', f], capture_output=True, text=True)
if out.returncode:
    sys.exit(out.stderr)
sys.stdout.write(out.stdout)
