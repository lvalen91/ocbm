# Apple SDK ground truth and conformance

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 13, 43, 26, 49; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

**Contents:** what the licensed SDK actually says → conformance corrections → Simulator conformance and verification.

## SDK ground truth

<!-- absorbed: ../carplay/03_SDK_GROUND_TRUTH.md -->

Consolidated ground-truth of Apple's CarPlay accessory protocol + config schema + full capability stack,
compiled read-only from Apple's publicly installed CarPlaySimulator SDK (the ground-truth SDK). Basis for the project's
YAML/VehicleConfig framework (task #5) and the feature roadmap (HEVC, AltVideo/nav, Siri, touch, `/command`).

Findings are marked **[E]** evidenced (literal string / exported symbol / config file / SDK resource offset cited)
or **[I]** inferred. Every fact links to its discovery location.

### 0. Sources (direct links)
- **Plugin bundle:** `/Applications/Xcode.app/Contents/SharedFrameworks/DeviceKit.framework/Versions/A/PlugIns/CarPlaySimulator.devicekitplugin`
- **CarPlaySDK binary** (Apple's AirPlay/CarPlay receiver "StarkSDK"; arm64e): `<plugin>/Contents/Frameworks/CarPlaySDK.framework/Versions/A/CarPlaySDK` — version **1.0**, `sourceVersion` **509.11**.
- **Simulator host / config parser:** `<plugin>/Contents/MacOS/CarPlaySimulator` (Swift module `CarPlayConfigs`, YAML via **Yams** — symbols `_TtC4Yams11YAMLDecoder`, `_TtC14CarPlayConfigs13VehicleConfig`).
- **YAML VehicleConfig templates (10):** `<plugin>/Contents/Resources/VehicleConfigs/Configs/*.yaml` → copied to `reference/carplay_sdk/apple_vehicleconfigs/`.
- **VDC (nav telemetry) schema:** `<plugin>/Contents/Frameworks/CarPlaySDK.framework/Versions/A/Resources/VDCSchema-External.json` + instance `<plugin>/Contents/Resources/VehicleConfigs/VehicleDataConfigs/Navigation/Navigation.vdc.json` → `reference/carplay_sdk/apple_vdc/`.
- **Retained reference notes (scratchpad):** `sdk_strings.txt`, `sim_strings.txt`, `all.txt`, `hidparse.py`, `cfstr.py`.
- **Project mapping targets:** `crates/vendor/receiver/src/info.rs` (`build_info`), `.../session.rs`, `.../events.rs`, `.../hid.rs`, `.../uplink.rs` (the pre-vendoring `ncm_carplayd/receiver_core` tree is gone); `ccpa_custom/ccpa/carplayd/src/main.rs`.

### 1. Two layers: authoring (YAML) vs wire (`/info`)
Apple's simulator loads a **`VehicleConfig` YAML** (authoring) and *translates* it into the on-wire AirPlay
**`/info` plist** the accessory sends the iPhone. Our framework mirrors the same split: our YAML ≈
`VehicleConfig`; `build_info` renders `/info`. Keep the vocabularies distinct (YAML `pixelDimensions.width`
↔ wire `displays[].widthPixels`). [E — the 10 YAML files + the on-wire keys in the binary + capture-locked info.rs]

### 2. `VehicleConfig` YAML schema — the authoring template [E]
Root `VehicleConfig` (module `CarPlayConfigs`, files `VehicleConfig.swift`, `DisplayPanelsConfig.swift`,
`VideoStreamsConfig.swift`, `HIDConfig.swift`, `AccessoryConfig.swift`, `LunaConfig.swift`, `CARPConfig.swift`):

| key | type | notes |
|---|---|---|
| `name`, `version`(=1), `sortID` | str/int | authoring metadata |
| `displayPanelsConfig` | obj | `mainDisplayPanel` + `altDisplayPanels[]` |
| `videoStreamsConfig` | obj | `mainVideoStream` + `altVideoStreams[]` |
| `accessoryConfig` | obj | the `enables*` feature toggles |
| `lunaConfig` | obj (opt) | video-playback preset sizes |
| `carpConfig` | obj (opt) | `carpIdentifier` (e.g. `Navigation`) → VDC |

- **`DisplayPanelConfig`**: `displayPanelID` (`DisplayPanel.Main`/`Alt1`/`Alt2`), `pixelDimensions{width,height}`, `displayProperties[]` (member `showsInstruments` = cluster).
- **`VideoStreamConfig`**: `videoStreamID` (`VideoStream.Main`/`Alt1`), `pixelDimensions`, `viewAreas[]{viewArea{originX,originY,width,height}, safeArea{…}}`, `hidConfig`, `primaryInput` (`Touchpad`/`Knobs`), `initialURL` (e.g. `maps:/car/instrumentcluster/map`).
- **`HIDConfig`**: `knobSupport`, `knobSupports{HomeAndBackButton,Nudge}`, `touchScreenMode` (`High Fidelty`[sic]/`Low Fidelty`/`Disabled`), `touchScreenSupports{Cancel,MultiTouch}`, `telephonyButtonsSupport`, `mediaButtonsSupport`, `touchpadSupport`, `touchpadButtonsSupport`, `dPadSupport` (+ binary-only: `touchpadWidth/Height`, `physicalDimensions`, `backButtonSupport`, telephony sub-flags).
- **`AccessoryConfig`** (full `enables*` set [E] from the binary; templates set only the first 4): `enablesMainBufferedAudio`, `enablesUIAppearance`, `enablesMapAppearance`, `enablesVideoPlayback`, `enablesViewAreas`, **`enablesHEVC`**, `enablesVehicleDataProtocol`, `enablesFileTransfer`, `enablesLogTransfer`, `enablesFocusTransfer`, `enablesUIContext`, `enablesUISync`, **`enablesEnhancedSiri`**, **`enablesCornerMasks`**, `enablesDCX`.
- **`LunaConfig`**: `videoPresetSizes[]{name, originX/Y, pixelWidth/Height, physicalWidth/Height, fullScreen, extendedMode}`.
- **`CARPConfig`**: `carpIdentifier` (`Navigation`) → loads a `.vdc.json`.

Templates: `Widescreen.yaml` = single 1920×720 main (closest to us); `*Instrument Cluster` add an Alt panel + alt video; `*Navigation` set `carpConfig`. See `reference/carplay_sdk/apple_vehicleconfigs/`.

### 3. `/info` wire schema + activation + change model [E]
Served by `_requestProcessInfo` (GET /info, binary plist; may carry a `qualifier` for a subset).

**Top-level `/info` keys [E]:** `deviceID`, `name`, `model`, `manufacturer`, `firmwareRevision`, `hardwareRevision`, `OSInfo`, `sourceVersion`, `bluetoothIDs`, `features`, `extendedFeatures`, `statusFlags`, **`displays[]`**, **`displayPanels`**, `hevcInfo`, `mainBufferedInfo`, `audioFormats`, `audioLatencies`, `hidDevices`, `hidLanguages`, `buttonInfo`, `limitedUI`/`limitedUIElements`, `nightMode`, `oemIcon(s)`, `rightHandDrive`, `modes`, `enhancedSiriInfo`, `altScreenSuggestUIURLs`, `uiContext{Last,Now}OnDisplayURLs`, `vehicleStateProtocolInfo`, `vehicleInformation`, `sessionManagementInfo`, `iAPChannelInfo`, `logTransferInfo`, `keepAlive{SendStatsAsBody,LowPower}`, `pluginConfigs`/`pluginMapping`/`pluginCount`, `protocolVersion`, `clientOSBuildVersionMin`, `txtAirPlay`, `viewAreas`, `safeArea`. Bonjour TXT mirror (`_UpdateBonjourAirPlay`): `deviceid`, `features`, `flags`, `model`, `srcvers`.

**Activation sequence [E]:** Bonjour TXT → **GET /info** (advertises the superset) → /pair-setup → /pair-verify → /auth-setup → GET /info (re-read, encrypted) → **SETUP phase1** (the iPhone sends a *feature-support description*; the accessory applies session params incl. `displayPanels`) → SETUP phase2 (streams) → **RECORD (= session established)**. RTSP verbs: `ANNOUNCE SETUP RECORD PAUSE FLUSH TEARDOWN OPTIONS POST GET PUT GET_PARAMETER SET_PARAMETER FLUSHBUFFERED SETRATE*`. Endpoints: `/info /pair-setup /pair-verify /auth-setup /feedback /command /logs /diag-info /metrics`.

**The SETUP feature-intersection gate [E]** (`### <X> supported: %s`): `/info` advertises the superset; the SETUP `features` array intersects it; `AirPlayCopyAccessoryEnabledFeatures` = the live set. Gated features: `enhancedSiri, altScreen, uiContext, cornerMasks, focusTransfer, h.264Level5.1, hevc, mainBuffered, sessionManagement, iAPChannel, logTransfer, vehicleStateProtocol`. **Advertising a capability isn't enough — it must survive this intersection.**

**Change-propagation model [E] — the key answer:**
- **Class A — reconnect-only** (read pre-SETUP): `features`/`extendedFeatures`/`statusFlags`, `audioFormats`, `hevcInfo`, `hidDevices`, identity/`sourceVersion`/`model`, `protocolVersion`, and **the existence of a display** (add/remove a `displays[]` entry). Change requires a fresh session (`/info` re-read). ← this is why our SUBSCRIBE-pushed config (= new session) works; resolution proven (docs/carplay/06_AV_PIPELINE.md).
- **Class B — live via `SessionControl` (`/command`)**: existing-panel geometry (`updateDisplayPanels`, incl. `pixelWidth/pixelHeight`), `updateViewArea`, `changeModes`, `setNightMode`, `uiAppearanceUpdate`/`mapAppearanceUpdate`/`changeMapZoomLevel`, `setLimitedUI`, `updateVehicleInformation`, focus, `hidSendReport`. No reconnect.

### 4. Display / view-area / cutout / multi-display [E]
**`/info` `displays[]` entry:** `uuid` (must equal a HID device's `displayUUID`), `type` (**110** = main; roles `Center_Display`/`Cluster_Display`/`Secondary_Cluster_Display`/`Passenger_Display`), `features` (bitmask; **0x0A** = HighFidelityTouch|Knobs), `primaryInputDevice` (0 = Undeclared on genuine wired), `maxFPS` (60), `widthPixels`/`heightPixels` (the coded-resolution lever), `widthPhysical`/`heightPhysical` (mm; 0/0 = unknown), `initialViewArea`, `adjacentViewAreas`, `viewAreas[]`. (`info.rs` `build_info`, `view_areas`, `touchscreen_descriptor`.)
**`viewAreas[]` entry:** `originXPixels/originYPixels/widthPixels/heightPixels`, `viewAreaTransitionControl`, `viewAreaStatusBarEdge`, `safeArea{originXPixels…, drawUIOutsideSafeArea}`. Runtime: `AirPlayReceiverSessionViewAreaUpdate`, `ScreenStreamSetViewArea`.
**Cutout = corner masks** (NOT notch/radius): advertise `cornerMasks` support (`AirPlayScreenDictSetCornerMasksSupport`), then stream an opaque per-corner bitmap at runtime (`ScreenStreamSetCornerMask`, `cornerMaskBuffer`/`cornerMaskLength`, `handleCornerMaskDataReceived:`). No `cornerRadius`/`notch`/`insetRect` keys exist.
**Multi-display:** extra `displays[]` entries (`AirPlayAltScreenDictCreate` vs `MainScreenDictCreate`); a cluster = an `altDisplayPanel{showsInstruments}` + an `altVideoStream` (with sub-rect `viewAreas`) routed via `altScreenURLs`/`initialURL: maps:/car/instrumentcluster/map`.
**Our status (CORRECTED 2026-08-16 — this line previously read "single main panel only; no cluster/altScreen, no corner masks, single flat viewArea", which §10's own table in this same file already contradicted).** cluster/altScreen ✅ (`vehicle_config.rs` `alt_screen()` → `carplayd/src/main.rs` `levers::set_altscreen` → the type-111 `displays[]` entry with `altScreenURLs`/`altScreenSuggestUIURLs`, `info.rs` `altScreenURLs`, echoed in `session.rs` `enabledFeatures`); corner masks ✅ (`vehicle_config.rs` `corner_masks_enabled()` → `levers.rs` `cornermasks` field → display-level flag in `info.rs`, with the mandatory safeArea omission documented beside it, echoed in `session.rs` `enabledFeatures`); viewAreas ✅ with a real inset `safeArea` + `drawUIOutsideSafeArea` + `viewAreaTransitionControl` + `viewAreaStatusBarEdge` + `viewAreaSupportsFocusTransfer`, type-110-gated (`info.rs` `view_areas()`). **Real remaining gap:** `/info` still carries only the legacy flat `displays[]`, never the modern `displayPanels[]` array — the app authors `altDisplayPanels[]` and the box parses it (`vehicle_config.rs` `alt_display_panels`) but nothing emits it. *(corrected 2026-10-28: line numbers replaced by symbol anchors after they drifted again; `levers::set_cornermasks` no longer exists, the lever is a `levers.rs` struct field.)*

### 5. Video codecs — H.264 + HEVC [E]
Codec is selected **in-band by the sample-description FourCC** in the screen-stream handler (~0x282240):
`'hvc1'`=`0x68766331`→HEVC, `'avc1'`=`0x61766331`→H.264. The accessory decodes/forwards whatever iOS sends;
color is hardwired (ITU-R 709 / 601-4 / sRGB).
**HEVC = 3 gates [E]:** (1) publish a non-null **`hevcInfo`** in `/info` (`_AirPlayReceiverServerPlatformCopyProperty("hevcInfo")`; the simulator gates it with a single `AccessoryConfig.enablesHEVC` bool); (2) SETUP `features` array must contain `hevc` (`CFArrayContainsValue`, cached session+0x15b, `AirPlayReceiverSessionHasFeatureHEVC` @0x80d8); (3) iOS then streams `hvc1`. `hevc` and `h.264Level5.1` are **separate** flags.
**Video-stream SETUP** (`_AirPlayReceiverSessionScreen_Setup` @0x12cdc): `uuid`, `type`, `latencyMs` (default 70), `params`. **No accessory-declared bitrate/profile/level** — resolution/FPS live in the screen dict; codec/profile ride in-band. Keyframe: `_AirPlayReceiverSessionForceKeyFrame` @0x271d68 → `{type:"forceKeyFrame", params:{uuid:<streamUUID>}}` (works for both codecs) (corrected 2026-08-01: was `{type, params:{forceKeyFrame:true}}`; see crates/vendor/receiver/src/events.rs:681).
**For our HEVC-only goal:** return non-null `hevcInfo` from `/info`, don't strip `hevc` at SETUP, and forward the `hvcC` atom + `hvc1` NAL framing intact (not assume `avcC`).

### 6. AltVideo / navigation / GPS / maps [E]
**AltVideo** (declarative): `altVideoStreams[]{videoStreamID: VideoStream.Alt1, pixelDimensions, viewAreas, initialURL: maps:/car/instrumentcluster/map}` + `altDisplayPanels[]{displayProperties:[showsInstruments]}` + `enablesMapAppearance`. Becomes a 2nd AirPlay screen stream `(AirPlayStreamType, UUID, port)` — display-only (no `hidConfig`). Runtime: `initialVideoStreams`, `ScreenStreamStart`, `getClusterLayer:`.
**Navigation / route guidance** = the **VDC `Navigation` accessory** `0E000002` over the two-channel `VehicleDataProtocol` (`_AirPlayReceiverSessionVehicleDataProtocol{1,2}Send`, ch1 low / ch2 high priority). Services (see `reference/carplay_sdk/apple_vdc/`): `RouteStatus`(1E000102, required — `RouteState`{NotActive/Active/Arrived/Loading/Locating/Rerouting/…}, `Origin`/`Destination`=PointOfInterest, `GeodeticSystem`{WGS84/GCJ02}, `RerouteReason`), `SystemInformation`(1E000103, required — `RouteSource`, names), `RouteSharing`(1E000104, optional — `SharingState`, `Legs`=RouteLeg, `CurrentLegIndex`, `Identifier`). Types: `Coordinate{lat,lon,alt}`, `PointOfInterest`, `RouteLeg`.
**GPS forwarding** (accessory→iPhone) = **NMEA-0183**: `GPRMC/GPGGA/GPGLL/GPGSA/GPGSV/GPHDT/GPVTG/GPZDA` + Apple-proprietary `OHPR` (IMU heading-pitch-roll), `PAACD`/`PAGCD`/`PASCD` (accel/gyro/speed for dead-reckoning). Field formats at binary ~0x34a5xx.
**Maps zoom:** `AirPlayReceiverSessionChangeMapZoomLevel(session, uuid, AirPlayZoomDirection, …)` → `{zoomDirection, zoomFactor}`. **TBT audio** = the `turns` AirPlay mode. **Cluster telemetry** = `ClusterDP.*` (speed/range/battery/fuel, `CommonStates.lua`).

### 7. Audio + Siri [E]
**Stream types:** `MainAudio` **100** / `MainHighAudio` **102** (realtime, UDP RTP), `AltAudio` **101**, `MainBuffered` (media/music, **TCP** — distinct from 102, own SETUP compression-type + `FLUSHBUFFERED` verb), `GeneralAudio` = `AuxOut` **106** / `AuxIn` **107** (Enhanced Siri mic uplink), `MainScreen` **110** / `AltScreen` **111**, `DataStream` **130**. **[E] Only 100/101/102/110 exist in R14G17 `AirPlayCommon.h:251-255`** — the rest are post-2017 and are evidenced by `CarPlaySDK` symbols (`_AuxInSetup`/`_AuxInTearDown`/`AudioStreamAuxInStart`, `_BufferedAudioSetup`/`_BufferedAudioThread`, `_MainAltAudioSetup`/`_MainAltAudioThread`) plus the 106/107 numbering (AuxOut 106 / AuxIn 107). **Silence in a 2017 source is not evidence of absence** (docs/ops/03_REFERENCE_INDEX.md §D). Audio detail: `docs/carplay/06_AV_PIPELINE.md`. `audioType` values: `media`, `telephony`, `speechRecognition`(Siri), `alert`, `default`. Modes: `screen/mainAudio/speech/phone/turns`.
**`audioFormat` catalog [E]** (`CODEC/rate[/depth]/chan`, bitmask): PCM 8k–48k/16/1–2; AAC-LC 44.1k·48k/2; AAC-ELD 16k–48k/1–2; OPUS 16k·24k·48k/1. Wired media = **PCM/48000/16/2** (our path). SETUP keys: `audioFormat`, `audioFormats`, **`audioInputFormats`** (mic/uplink, separate), `latencyMs`, `streamConnectionID`, `redundantAudio`, `compressionType`, `vocoderInfo`/`vocoderSampleRate`, `enhancedSiriInfo`, `mainBufferedInfo`. Parser: `_BufferedAudioParseFormatInfoFromSetupMessage`.
**Siri:** `enhancedSiri`/`enhancedSiriInfo`/`enhancedSiriParameters`; trigger `_AirPlayReceiverSessionRequestSiriActionInternal` (`AirPlaySiriAction`, `requestSiri`, `siriAction`, `siriTriggerZone`, `siriTriggerTimestamp`); voice trigger distinct (`kAFErrorSpeechAbortedFalseVoiceTrigger` = **iOS's second-pass detector rejecting the car's first-pass hit**). Uplink = **`AuxIn` 107** (`AudioStreamAuxInStart`) low-rate mono (AAC-ELD 16k/24k, OPUS 16k, PCM 16k); Siri downlink = **`AuxOut` 106**, a dedicated stream opened at Siri launch and mixed by the car against media + route guidance (three parallel streams).
**CORRECTED 2026-08-02 ×2:** (a) this line said *"button path via `accessoryAcquireFocus`"* — **wrong**, and it contradicted `../carplay/05_METADATA_AND_CONTROLS.md` §4/§6, which shows from disassembly that `HasFeatureFocusTransfer` gates focus-transfer and **not** Siri: the button needs no AcquireFocus and no mic/AuxIn at all (Classic Siri uses the phone's own mic). (b) Siri downlink is `AuxOut`, not "AltAudio/GeneralAudio" — `GeneralAudio` is the *name of the pair* (AuxOut/AuxIn), not the downlink.
**Enhanced Siri is a two-stage detector with stage one in the car** — always-on mic, ECNR, a couple-of-seconds historical ring buffer in the Communication Plug-in, **two mandatory detectors** (keyword + voice-activity; iOS picks which is used), then iOS re-verifies. Architecture: `wwdc2019-252.txt:86-134` (docs/ops/03_REFERENCE_INDEX.md §E); full write-up in `../carplay/05_METADATA_AND_CONTROLS.md` §5.
**Ducking:** `duckAudio`/`unduckAudio` (`AirPlayReceiverSessionDuckAudio_f`), parameterized `Delegating ducking of audio to %f within %f seconds` (target gain + fade). Focus/priority: `accessoryAcquireFocus`/`accessoryGiveFocus`/`deviceOfferFocus`/`transferPriority`.

### 8. Input / HID [E]
**`hidDevices[]` entry:** `name`, `uuid` (echoed in every report), `displayUUID` (**binds input→display**), `hidProductID`, `hidVendorID`, `hidCountryCode`, `hidDescriptor` (raw report-descriptor bytes). `_AirPlayInfoArrayAddHIDDevice` @0x31964f.
**Descriptor generators [E]:** `HIDTouchScreen{Single,Multi}[WithCancel]CreateDescriptor` (built at runtime, **X/Y Logical Maximum injected** from args = the bound display resolution), `HIDKnob/DPad/MediaButtons/SteeringWheel/Touchpad{Only,Buttons,MultiCharacter}/Telephony/Proximity CreateDescriptor` (static templates).
**Coordinate space [E] — critical for touch:** **absolute 16-bit in `[0, LogicalMaximum]`, NOT normalized, no scaling.** LogicalMaximum = the advertised display resolution. No report-ID prefix; devices disambiguated by `uuid`.
**Report byte layouts [E]** (`*FillReport`, little-endian): single-touch **5 B** `[tip][Xlo Xhi][Ylo Yhi]` (down: tip=1+coords; move: tip=1; up: tip=0); single+cancel 5 B `[tip|(cancel?2:0)]…`; multi-touch **12 B** `[0][tip0][X0][Y0][1][tip1][X1][Y1]`; knob 4 B `[buttons][X][Y][rot rel]`; dpad 2 B; media-buttons 1 B; steering 3 B; touchpad abs 16-bit X/Y (µm).
**Uplink [E]:** `AirPlayReceiverSessionSendHIDReport(session, uuid, reportPtr, len)` → control message `{hidSendReport, uuid, hidReport:<bytes>}`.
**For the touch task (#20):** advertise the single-touch (or multi) descriptor with LogicalMax = the CarPlay resolution, and emit `{hidSendReport, uuid, hidReport:[tip][X LE][Y LE]}` with coords absolute in `[0,res]`. Produce in `receiver_core/.../uplink.rs` + `hid.rs`; keep the resolution ↔ scaling in sync (`uplink::set_display`).

### 9. `/command` control surface [E]
Transport: `POST /command` (`_requestProcessCommand`), binary plist `{type, params}` → reply `{status}`. Dispatchers: `AirPlayReceiverSessionControl` (session), `…PlatformControl` (`duckAudio`/`unduckAudio`/`startSession`/`stopSession`/`performHapticFeedback`/`deviceOfferFocus`), `…ServerControl` (`startServer`/`stopServer`/`sessionDied`).
**Outbound (accessory→iPhone):** `changeModes`, `forceKeyFrame`, `setNightMode`, `accessoryAcquireFocus`/`accessoryGiveFocus`, `setLimitedUI`, `requestUI`/`suggestUI`/`stopUI`/`changeUIContext`, `updateViewArea`, **`updateDisplayPanels`** (params `displayPanels[]{name, originX/Y, pixelWidth/Height, physicalWidth/Height, fullScreen, extendedMode, zIndex, displayUUID, primaryInputDevice, maxFPS, initialURL, initialViewArea, videoStreams, viewAreaTransitionControl, viewAreaStatusBarEdge, drawUIOutsideSafeArea, uiAppearanceMode, mapAppearanceMode, zoomFactor, properties}`), `changeMapZoomLevel`, `uiAppearanceUpdate`/`mapAppearanceUpdate`, `updateVehicleInformation`, `updateVocoderInfo`, `hidSendReport`, `hidSetInputMode`, `requestSiri`, `iAPSendMessage`, `requestViewArea`.
**Inbound (iPhone→accessory, `..._f` callbacks):** `modesChanged`, `duckAudio`/`unduckAudio`, `showUI`, `stopSession` (`disconnectReason`), `startSession`, `performHapticFeedback`, `deviceOfferFocus`, `tearDownStreams`, `requestViewArea`, `setEnhancedSiriParams`, `setOEMLogConfiguration`, `handleLogArchiveRequest`.
**`disableBluetooth` is NOT a real command in this SDK** — legacy; only `bluetoothIDs` (an `/info` array) exists. → reshapes task #19: dispatch the *real* set, not `disableBluetooth`.
**Enums [E]:** entity{controller/accessory/none}; transferType{take/untake/borrow/unborrow}; priority{niceToHave/userInitiated/anytime/never}; speechMode{none/speaking/recognizing}; app-state{standby/audioOff/nativeVR/displayOff/backupCamera/uiNotification/videoPlayback}; gear{park/reverse/neutral/drive/unknown}; GPS-validity{deadReckonedAndValid/gpsOnlyAndValid/notValid}.
**CORRECTED 2026-08-16** — this line previously claimed `events.rs` covers only four verbs and listed nine missing outbound; five of those nine have existed for some time, and §10's table had it right all along. `events.rs` implemented **thirteen** outbound verbs at that date (18 `pub fn send_*` on 2026-10-28, incl. `send_update_view_area`): `iAPSendMessage`, `forceKeyFrame`, `changeMapZoomLevel`, `changeModes`, `requestUI`, `showUI`, `stopUI`, `setLimitedUI`, `uiAppearanceUpdate`, `mapAppearanceUpdate`, `setNightMode`, `requestSiri`(+`siriAction`), `hidSendReport`. Inbound `session.rs` acts on `modesChanged` + the iAP-tunnel frames and logs the rest (inbound `requestUI`/`suggestUI` are logging-only BY DESIGN — docs/carplay/05_METADATA_AND_CONTROLS.md §1.1, 2026-09-05; do not add handlers). **Genuinely missing outbound (`updateViewArea` since landed, see §10):** `updateDisplayPanels`, `updateVehicleInformation`, focus (`accessoryAcquireFocus`/`accessoryGiveFocus`), `changeUIContext`, `suggestUI`, `updateVocoderInfo`, `hidSetInputMode`, `requestViewArea` — i.e. exactly §10's list, which is the authoritative one.

### 10. Mapping to ccpa_custom + prioritized gaps
| capability | Apple mechanism | ccpa_custom status |
|---|---|---|
| Resolution | `displays[].widthPixels/heightPixels` (Class A) | ✅ app-pushed per control connection (`carplayd::load_device_config`); 1920×720 survives only as the app-less fallback in `base_device_config()` |
| Config framework | `VehicleConfig` YAML → `/info` | ✅ host-authoritative YAML → `vehicle_config.rs` → `/info`; regression-covered by `receiver/tests/r4_c2_schema.rs` |
| Live resolution/layout | `updateDisplayPanels` (Class B) | ⚠️ `updateDisplayPanels` ❌ not implemented (zero hits). **corrected 2026-10-19:** `updateViewArea` IS implemented — `events.rs` `send_update_view_area`, answering inbound `requestViewArea` (`session.rs:1805`) and host-commanded via OCBM `cmdViewArea` (`OCBMFraming.swift` `cmdViewArea = 0x11`) |
| HEVC | `hevcInfo` + SETUP `hevc` + `hvc1` in-band | ✅ `info.rs` publishes `hevcInfo` gated on `enablesHEVC`; `hevc` echoed at SETUP; `hvc1`/`hvcC` decoded host-side (`VideoDecoder.swift`) |
| Touch input | `hidSendReport{uuid, [tip][X][Y]}` abs coords | ✅ `hid.rs::touch_report` + `events.rs::send_hid_report` + host `CH_INPUT`; LogicalMax patched from the advertised resolution |
| Multi-display / cluster | extra `displays[]` + `altVideoStream` + altScreen URL | ✅ `altScreen` negotiated, streamed on `CH_ALT_VIDEO`, rendered by host `AltVideoWindow.swift` |
| Corner masks (cutout) | `cornerMasks` + `ScreenStreamSetCornerMask` | ✅ negotiated (`session.rs`), forwarded (`server.rs::forward_corner_mask`), host `CornerMask.swift` |
| Siri / telephony / alt audio | `audioType` streams + `requestSiri` + `AuxIn` | ✅ voice/telephony/alert routed to the :9003 sink → `CH_ALT_AUDIO`; mic uplink over `CH_MIC` with a REAL AAC-ELD encoder (`eld-codec`, not a stub); button-Siri via `requestSiri`. **Owner-confirmed on hardware 2026-08-10 — but see `R-13-2`: until `536dfb8` (2026-08-16) the encoder emitted LD-SBR and iOS silently discarded every access unit, so Siri heard nothing.** ❌ **Enhanced Siri only** (`AuxOut` 106 / `AuxIn` 107) + `MainBuffered` — omitted from SETUP (see the "Still unimplemented and therefore omitted" comment at `session.rs:1675`; `mainBuffered` itself is now echoed under `enablesMainBufferedAudio`, `session.rs:744-754`) *(corrected 2026-10-19: was `session.rs:1402`, drifted)* |
| Navigation / GPS | VDC `Navigation` accessory + NMEA | ⚠️ split: the **iAP2** NMEA plane is implemented but DELIBERATELY NOT DECLARED (`metadata/src/location.rs`; param 22 withheld by default). The **AirPlay VDC** `VehicleDataProtocol` is ❌ absent |
| `/command` handling | full catalog §9 | ⚠️ 18 `pub fn send_*` in `events.rs` (2026-10-19 count; incl. `showUI`/`stopUI`, `setLimitedUI`, `ui/mapAppearanceUpdate`, `setNightMode`, `updateViewArea`, `changeMapZoom`); inbound acts on `modesChanged`, `requestViewArea` + iAP-tunnel frames and logs the rest. ❌ still missing outbound (corrected 2026-10-19: `updateViewArea` removed from this list, it ships): `updateDisplayPanels`, `updateVehicleInformation`, focus, `changeUIContext`, `suggestUI`, `updateVocoderInfo`, `hidSetInputMode`, `requestViewArea` |

> **⚠️ CORRECTED 2026-08-10 — the table above previously had SEVEN of ten rows stale in the "we have
> not built this" direction.** It has since been rewritten and every ✅ is cited to code, so read the
> table as current. The process lesson (under-reporting misdirects planning; over-reporting hides
> outages — audit for both) is in [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-13-1`.



### Caveats
*(**CORRECTED 2026-08-16** — `AirPlayStreamType` is [E] per §7 of this doc (`AirPlayCommon.h:251-255`, 100/101/102/110) and `AirPlaySiriAction` is [E] per `AirPlayCommon.h:1366-1369`; both are emitted as integers. Only the remaining names in this bullet are still [I].)* Numeric enum values for `AirPlayStreamType`, `AirPlayAudioFormatIndex` bitmask bits, `AirPlaySiriAction`,
`AirPlayZoomDirection`, and PCM endianness are **[I]** (compiled-in, not emitted as strings). The internal
schema of `hevcInfo` is **[I]** (copied opaquely; the simulator exposes only a bool). Everything else above
is **[E]** with the cited location.

---

## Conformance corrections

<!-- absorbed: ../carplay/03_SDK_GROUND_TRUTH.md -->

**Original status: AUTHORITATIVE. Where this document and docs/wireless/00_WIRELESS_CARPLAY.md disagree, THIS document is
correct**,
because it is derived by reading Apple's licensed CarPlay Communication Plug-in **R14G17 source**
directly, whereas the conclusions it corrects were derived by inference from disassembly, vendor
firmware, and on-hardware behaviour.

Reference root (see `../ops/03_REFERENCE_INDEX.md`):
`~/Documents/carlink/old/carplay_RE/carplay_sdk/reference/apple_carplay_sdk_R14G17/` — below, `SDK/`. *(corrected 2026-10-19: the old `~/carlink/local_carplay_sdk/` path no longer exists; the tree lives under `old/carplay_RE/carplay_sdk/`.)*

Every claim below was read in that source this session and is cited `file:line`.

---

### 1. THE ROOT ERROR: inbound tunnel frames arrive on the CONTROL connection, not the event channel

> **⚠️ "The actual reason the tunnel never worked" IS NOT the actual reason.** The real inbound
> carrier is the RCS DataStream, SETUP stream type 130 (docs/carplay/05_METADATA_AND_CONTROLS.md). What still stands: that inbound
> `POST /command` arrives on the CONTROL connection, that the event channel's unsolicited-inbound
> mode is never armed, and the 2026-07-25 live observation. Full reasoning:
> [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-43-2`.

**What the source says (CONFIRMED):**
- `POST /command` on the main RTSP **control** connection → `_requestProcessCommand`
  (`SDK/AppleCarPlay/Sources/AirPlayReceiverServer.c:2391, :2492`) →
  `AirPlayReceiverSessionControl` (`:2516`) → the `iAPSendMessage` branch
  (`SDK/AppleCarPlay/Sources/AirPlayReceiverSession.c:572-576`) → `delegate.control_f`.
- The "event" socket is wrapped in an **HTTPClient** (`AirPlayReceiverSession.c:1840`, `_ControlStart`)
  used **only for accessory→phone requests**. Its unsolicited-inbound mode
  (`kHTTPClientFlag_Events`, `SDK/AppleCarPlay/AccessorySDK/Support/HTTPClient.h:167`) is **never
  enabled anywhere in the plug-in** — the SDK's only `HTTPClientSetFlags` call
  (`CarPlayControlClient.c:1466`) sets `Reachability`/`NonLinkLocal` only.

**Live confirmation on our own hardware (2026-07-25, first successful wireless session):** every
inbound command arrived on the control connection — 8× `modesChanged`, 1× `disableBluetooth` via
`POST /command` — while the event channel logged **no inbound traffic at all**.

**What this invalidates:**

- **docs/carplay/05_METADATA_AND_CONTROLS.md §1.3 (lines ~78-84) inverts the channel priority.** It frames control-connection delivery
  as a speculative *"If iOS ever delivers `iAPSendMessage` via the control connection instead"* and
  treats the event channel as primary. Per the source this is exactly backwards.
- **docs/wireless/00_WIRELESS_CARPLAY.md** consequently filed the only load-bearing inbound path among Phase 2's routine
  additive fixes (it is tagged "CONFIRMED safe as specified"; §2.7 is the item actually marked
  low-priority hygiene) — and implemented it by routing control-channel iAP data **straight to the
  post-Identify metadata parser**, bypassing the handshake state machine entirely. The misdirected
  priority is the fair criticism; the mislabel is not.
- **docs/wireless/00_WIRELESS_CARPLAY.md §"events.rs wiring"** wires the whole inbound feed — and `disableBluetooth` — to the event
  channel.

**Consequence, and the actual reason the tunnel never worked:** a phone-sent SYN-ACK (9 bytes,
`FF 5A`-headed) delivered to `dispatch_iap_tunnel_message` fails its `4040`/`FF5A+4040` shape checks
and is dropped with a log line. Nothing is ACKed, `peer_seq` never advances, and the state machine
never leaves `State::Init`. Meanwhile the event channel — where all the handshake machinery lived —
corresponds to a code path the reference **never arms**, so it plausibly never fires at all.

**Fixed 2026-07-25** in `session.rs::command()`: offer iAP frames to `iap_tunnel::handle_inbound`
first, falling through to `dispatch_iap_tunnel_message`, mirroring `events.rs`.

---

### 2. The `/command` reply shape is wrong

**Source (CONFIRMED):** an inbound `POST /command` is answered `200 OK` with a **binary-plist body**.
When the delegate produces no `outParams`, the server creates an **empty dictionary** and serializes
it (`AirPlayReceiverServer.c:2518-2524` → `_requestSendPlistResponse:3418-3441`, `Content-Type:
application/x-apple-binary-plist`). A delegate error yields `422 Unprocessable Entity` (`:2517`).
`AirPlayCommon.h:584-592` documents `iAPSendMessage` as having **"No response keys"** — which means an
empty dict, **not** an empty body. The reference's own sender-side completion parses that body
(`AirPlayReceiverSession.c:859-865`).

**Ours (before the fix):** `session.rs` returned `Vec::new()` and `server.rs` sent `200` with the
bplist content-type but a **zero-length body**. **FIXED** 2026-07-25 (commit "Zero-Ack link params for the tunnel, plus the /command reply body", 2026-07-25) —
`session.rs` now returns `empty_plist_dict()`.

One nuance worth recording so nobody over-claims it later: the reference's sender parses the reply
body only `if( inMsg->bodyLen > 0 )` (`AirPlayReceiverSession.c:859`), so the old zero-length body was
**not** ambiguous to a reference-derived client. The change is right because it matches what the
reference emits, not because the old shape was provably breaking anything.

**What this means for docs/wireless/00_WIRELESS_CARPLAY.md — corrected 2026-07-25 after review; the first version of this
section was itself wrong.** §2.5 states:

> *"V6 found, via Apple's own actual SDK source (`AirPlayReceiverSession.c`, `HTTPClient.c`): the real
> reference implementation **never replies on this channel at all**"*

**That factual claim and its attribution are CORRECT.** `HTTPClient.c:583-589` contains the only
inbound-request path in the whole client, and it consumes the message without replying:

```c
if( ( me->flags & kHTTPClientFlag_Events ) &&
    ( strncmpx( msg->header.protocolPtr, msg->header.protocolLen, "EVENT/1.0" ) == 0 ) )
{
    if( me->delegate.handleEvent_f ) me->delegate.handleEvent_f( msg, me->delegate.context );
    HTTPMessageReset( msg );
    continue;
}
```

An earlier draft of this document claimed §2.5 was "wrong AND misattributed" and that the matching
comment in `events.rs` "inherits the same error". **Both of those were wrong** — the source says
exactly what §2.5 says.

What §2.5 actually got wrong is narrower, and is about **relevance, not fact**: it aimed a reply at
the event channel, which per §1 carries no inbound requests at all, so the work was misdirected. Its
prescribed `Content-Length: 0` shape has no source backing, but it is not contradicted either —
the source simply never replies there. Only the **control**-channel reply shape is positively
specified, and that is the one we were getting wrong.

---

### 3. The `iAPChannel` / `iAPChannelInfo` "SETUP gate" has no basis in this SDK

> **⚠️ HALF RETRACTED — the two keys are NOT equivalent.** The `"iAPChannel"` `enabledFeatures` echo
> IS load-bearing (CarPlaySDK 509.11 + iOS 27 `AirPlaySender`) — absent from R14G17 only because the
> gate postdates 2017. The separate `iAPChannelInfo` `/info` key is still probably inert; that half
> of the doubt stands, as do the R14G17 grep findings and the rejection of docs/carplay/05_METADATA_AND_CONTROLS.md §1.1's causal
> chain. Worked through below; full reasoning: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-43-2`.

**Source (CONFIRMED):** the strings `iAPChannel`, `iAPChannelInfo` and `enabledFeatures` appear
**nowhere in R14G17** (repo-wide grep: zero hits). Features are a u64 bitmask under `features`
(`AirPlayCommon.h:943`, bits at `:808-823`); `extendedFeatures` is a string array whose only defined
values are `vocoderInfo` and `enhancedRequestCarUI` (`:835-838`). The reference consults **no
capability gate whatsoever** before delivering `iAPSendMessage` — the only precondition is
`delegate.control_f != NULL` (`AirPlayReceiverSession.c:572`).

**This leaves the following UNSUPPORTED for R14G17** (absence from a 2017-era SDK cannot by itself
invalidate a claim derived from iOS 27 material — see the caveat below):
- **docs/carplay/05_METADATA_AND_CONTROLS.md §III.4**'s claim, stated as disassembly-confirmed fact, that *"`iAPChannel`/`enableiAPChannel`
  is a CarPlay SETUP feature key, negotiated the same way as `fileTransfer`/`vehicleStateProtocol`…"*.
- **docs/carplay/05_METADATA_AND_CONTROLS.md §Short-answer** and **docs/carplay/05_METADATA_AND_CONTROLS.md §III.1**'s framing of a "missing `iAPChannel` SETUP gate".
- **docs/carplay/05_METADATA_AND_CONTROLS.md §1.1**'s causal chain (missing gate → iOS 400s every `iAPSendMessage`). docs/wireless/00_WIRELESS_CARPLAY.md itself
  attributes those 400s to the capital-`Data` bug, which is the better-supported explanation.
- **docs/carplay/05_METADATA_AND_CONTROLS.md §Part 2**'s scepticism about `iAPChannelInfo` was, by contrast, **right**.

**Caveat, stated honestly:** R14G17 is 2017 / iOS 10.3-era. A later iOS could have added gating this
reference cannot speak to. But nothing in any source we hold confirms one, and the belief that adding
this gate was the fix is not supported.

**Practical implication — HALF RETRACTED 2026-08-16.**

~~`iAPChannelInfo = {}` in `/info` and the `"iAPChannel"` `enabledFeatures` echo are almost certainly
inert extra keys. They are harmless and currently shipped; do not treat them as load-bearing, and do not
spend further effort there.~~

The caveat above — *"nothing in any source we hold confirms one"* — **was false when written.** Two
sources were on disk and indexed by `docs/ops/03_REFERENCE_INDEX.md` the same day. This section read a 2017 drop's silence as an
answer, which is the exact failure CLAUDE.md's order of authority exists to prevent.

- **`CarPlaySDK.framework` 509.11** (authority #1, the CURRENT receiver side): `iAPChannelInfo` sits in
  the `/info` key table between `features` and `firmwareRevision`, beside `hevcInfo`/`logTransferInfo`/
  `mainBufferedInfo`/`sessionManagementInfo`; and `enabledFeatures` →
  `CFArrayRef AirPlayCopyAccessoryEnabledFeatures(...)` → **`"Enabling iAP Channel support"`** →
  `iAPChannel`. `### iAP channel supported: %s` sits in the same contiguous gate-log block as
  `### HEVC supported: %s` and `### Log transfer supported: %s`.
- **iOS 27 `AirPlaySender`:** `carEndpoint_createSetupRequestFeatureList` proposes the literal pair
  `iAPChannel` / `enableiAPChannel` (`mined/AirPlaySender.strings.txt:10850-10851`), one entry below
  `logTransfer`/`enableCarPlayLoggingDataChannel`, and reads it back out of the SETUP **response**
  (`:10883`, `iAPChannel = %d`). `CarKit.strings.txt:1716` carries it in the phone's capability enum.

So **docs/carplay/05_METADATA_AND_CONTROLS.md §III.4's claim is CONFIRMED, not unsupported** — `iAPChannel` is negotiated exactly like
`fileTransfer`/`logTransfer`/`uiSync`. §3's grep finding stands only as scoped: those strings are absent
**from R14G17**, because the gate postdates 2017.

**The two keys are NOT equivalent — only one is retracted:**

- **`"iAPChannel"` in the SETUP-response `enabledFeatures` — LOAD-BEARING. Keep it.** The phone-side
  consumer of the negotiated state, `carEndpoint_createiAPChannelIfNeeded` ("Creating RCS channel for
  iAP", clientTypeUUID `E9459FD0-…`), is what opens the stream-130 DataStream docs/carplay/05_METADATA_AND_CONTROLS.md proved is the real
  carrier. Stripping it plausibly costs the whole tunnel, silently. `relay.rs` and `setup_driver.rs`
  already preserve it against the host's authoring path; that is correct.
- **`iAPChannelInfo = {}` in `/info` — probably inert, but legal and harmless; leave it.** It is a valid
  receiver-side key, but appears in **no** phone-side inventory: not in CarKit's `/info` list
  (`CarKit.strings.txt:1255-1266`) and not in `carEndpoint_validateInfoResponseKeyPresentForFeature`'s
  checked cluster (`AirPlaySender.strings.txt:10915-10920`). **docs/carplay/05_METADATA_AND_CONTROLS.md §Part 2's scepticism about
  `iAPChannelInfo` specifically was right and stays right.** Kept for both-sides-present symmetry, not
  because anything checks it.

**What this retraction does NOT rest on, and what it does NOT revive.** The comment at
`receiver/src/info.rs` citing *"device-observed 2026-07-22: 3/3 uniform 400"* is **not usable evidence**
and must not be cited: no capture from that date exists in `docs/ops/captures/`, git history begins
2026-07-25, the run it describes carried the known-fatal capital-`Data` key (docs/wireless/00_WIRELESS_CARPLAY.md hygiene item 1, `:31-34`), and
docs/carplay/05_METADATA_AND_CONTROLS.md §III.1 records that the casing fix and the echo were applied **together** — so it cannot
discriminate between them. `events.rs` attributes the same 400s to the casing bug, corroborated by
licensed source (`AirPlayCommon.h`: `#define kAirPlayKey_Data "data"`). **This section's judgement on
that point was correct and is unchanged: docs/carplay/05_METADATA_AND_CONTROLS.md §1.1's causal chain (missing gate → iOS 400s every
`iAPSendMessage`) stays refuted.** The keys are load-bearing because Apple's current receiver gates on
them — right conclusion, wrong reason.

---

### 4. The `data` key — settled from source, no more trial and error

> **⚠️ THE CHANNEL-ASYMMETRY FRAMING IS SCOPED TO THE WRONG CARRIER.** "Outbound on the EVENT
> socket, inbound on the CONTROL socket" describes the `POST /command` carrier only; inbound
> wireless iAP2 actually rides the RCS DataStream (docs/carplay/05_METADATA_AND_CONTROLS.md). The `data`-key finding itself — and
> the transport-check-not-capability-gate point — stand. Full reasoning:
> [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-43-2`.

**CONFIRMED:** `#define kAirPlayKey_Data "data"` (`AirPlayCommon.h:903`), with
`kAirPlayKey_Params "params"` (`:1048`), `kAirPlayKey_Type "type"` (`:1169`),
`kAirPlayCommand_iAPSendMessage "iAPSendMessage"` (`:593`).

`AirPlayCommon.h:584-592` documents the command in the **inbound** direction ("Sends an iAP message to
the receiver"), using the same key — so **inbound and outbound are symmetric**:
`{type:"iAPSendMessage", params:{data:<bytes>}}`.

Our outbound (`events.rs`) is **byte-conformant with the reference**, and now rests on the source
rather than on the device-observed 400s that first found it. The `data`/`Data`/`_data`/`_Data` casing
hedge on the inbound side is unnecessary — `"data"` is the only key — but harmless.

**The channel asymmetry, stated plainly** (otherwise "symmetric" above reads as contradicting §1): the
command and its keys are identical in both directions, but **the sockets are opposite**. Outbound goes
over the EVENT socket — `AirPlayReceiverSessionSendCommand` (`:816`) →
`dispatch_sync_f(eventQueue, …)` (`:832`) → `HTTPHeader_InitRequest(…, "POST", kAirPlayCommandPath, …)`
(`:779`) → `HTTPClientSendMessage( session->eventClient, msg )` (`:796`). Inbound arrives on the
CONTROL socket (§1). *Same command, same keys, opposite sockets.*

**The reference's real outbound precondition is a transport check, not a capability gate.**
`AirPlayReceiverSessionSendiAPMessage` hard-requires wireless at `:5346`:

```c
require_action( NetTransportTypeIsWireless( inSession->transportType ), exit, err = kUnsupportedErr );
```

That is what our `CARPLAY_WIRELESS_METADATA` env var stands in for (see §8) — and it is worth noting
this, not the absent `iAPChannel` gate of §3, is the only precondition the reference actually applies.

---

### 5. "No framing needed" was over-read

**docs/carplay/05_METADATA_AND_CONTROLS.md §5 and §7, and docs/carplay/05_METADATA_AND_CONTROLS.md §III.4** concluded from six disassembly passes that no framing/wrapper is
added anywhere, and **docs/wireless/00_WIRELESS_CARPLAY.md's hygiene note (item 3)** told future readers the FF5A question was
"very likely dead code in practice… not a live open question."

The narrow observation is correct — `AirPlaySender` adds no bytes — but it is fully **compatible with
the payload already being link-framed by the accessory**, which is exactly what the Integration Guide
requires (line 289: full iAP handshaking including detect and link synchronization). Those two facts
are about different layers. docs/wireless/00_WIRELESS_CARPLAY.md later reinstated the link framing that docs/wireless/00_WIRELESS_CARPLAY.md had told readers to
stop investigating.

**Rule going forward:** "the carrier adds no framing" says nothing about what the payload must contain.

**Additionally — the other half of that sentence is also wrong, and was left standing until now.**
docs/carplay/05_METADATA_AND_CONTROLS.md §5 concludes: *"there is no magic number, no length/framing wrapper, no sequence/ack
negotiation, and **no Identify-completion gate ANYWHERE** in the path…"*. The Identify half is
contradicted by Integration Guide line 289 (*"You must perform the full iAP handshaking over this
protocol which includes the detect sequence and link synchronization"*) and by docs/wireless/00_WIRELESS_CARPLAY.md's own premise.
docs/carplay/05_METADATA_AND_CONTROLS.md traced only as far as `accessoryd`'s XPC boundary and explicitly did **not** trace
`_iap2_endpoint_processIncomingData` (its own §6 step 6 says so) — which is exactly where such a gate
would live. A reader taking "no gate" as settled would conclude the handshake is unnecessary. It is
not: the guide requires it.

---

### 6. "Only remaining path is qemu dynamic analysis" was wrong when written

**docs/wireless/00_WIRELESS_CARPLAY.md §Results** and **docs/wireless/00_WIRELESS_CARPLAY.md §Conclusion** both state that the remaining path for wireless
metadata required dynamic (qemu) analysis of iOS internals, being "a fundamentally different kind of
investigation" and "explicitly out of scope".

The answer was in the licensed SDK already on disk: the Integration Guide's iAP2 section (lines
285-307) plus the control-channel delivery path in `AirPlayReceiverServer.c`. No emulation, no
disassembly, no hardware experiment required. See `docs/ops/03_REFERENCE_INDEX.md` for why this was missed and how to avoid
repeating it.

---

### 7. What docs/wireless/00_WIRELESS_CARPLAY.md got right, and what it inherited without saying so

**Right, and confirmed by the source:** the premise that this channel needs its own full iAP2
handshake (Guide line 289); that the accessory originates it ("**You** must perform…"); that it must
not start before the session-started milestone (Guide 299-302); and that BT iAP2 must not be
disconnected until `disableBluetooth` (Guide 306-307).

**Inherited from `bt_driver.rs`, not from the reference (INFERRED, and docs/wireless/00_WIRELESS_CARPLAY.md does not flag it):** the
specific choreography — accessory sends DETECT prelude, then SYN; phone answers SYN-ACK; accessory ACKs
each control frame — plus the SYN parameter values. **The SDK contains no iAP2 link layer at all**:
`SDK/Examples/AppleCarPlay_AppStub.c:756-777` (`_AirPlayHandleSessionStarted`) reads
`kAirPlayProperty_TransportType` and tests `NetTransportTypeIsWiFi` (`:766-775`) — but the wireless
branch, where the iAP2 session should be started, is a **bare comment**. The guide defers to the AISpec.

This mattered because docs/wireless/00_WIRELESS_CARPLAY.md's headline review fixes were about **adding ACKs**, while the guide's one
concrete link-layer recommendation is **Zero-Ack** (line 290). **Reconciled 2026-07-25** (commit "Zero-Ack link params for the tunnel, plus the /command reply body", 2026-07-25): the tunnel now sends Zero-Ack parameters with a fallback to the proven ones if the phone
declines. Per-frame ACKs remain correct under Zero-Ack — it means "ACK immediately / every N packets,
never batch on a timer", which is what we do.

**Timing was fine.** `iap_tunnel::start()` fires from `events::setup()` at RECORD after the event
socket is accepted — timing-equivalent to the reference's `started_f`
(`AirPlayReceiverSession.c:1108-1152`). An earlier hypothesis this session that we start "too early"
was **wrong**.

*(Corrected 2026-07-25 after review: an earlier draft justified this by saying `sessionStarted` "waits
only for thread creation, not for data connections". That reason is false — `_ControlStart` at `:1110`
does a **blocking** `SocketAccept` on the event socket at `:1836`, before `sessionStarted = true` at
`:1147`, so it does wait for exactly one data connection: the event connection. The conclusion is
unaffected and in fact strengthened, since our start point is RECORD-after-event-accept — precisely
the same milestone. `_ScreenStart` at `:4425-4438` genuinely is only a `pthread_create`; the screen
accept happens later inside `_ScreenThread`. And `kAirPlayCommand_StartSession` is a purely local
platform call (`AirPlayReceiverPOSIX.c:439-444`) that never reaches the phone.)*

---

### 8. Remaining genuinely-open items

- **Zero-Ack link parameters** (Guide line 290) — **RESOLVED 2026-07-25**, commit "Zero-Ack link params for the tunnel, plus the /command reply body", 2026-07-25,
  `crates/vendor/iap2-core/src/link.rs` `SYN_PARAMS_ZERO_ACK`. Concrete values are **not** in R14G17
  (the SDK has no iAP2 link layer) and the AISpec is not present on this machine (verified). They were
  recovered from two shipping implementations that encode the identical predicate — Apple's own
  `iAP2LinkIsNoRetransmit` (CarPlaySimulator @`0x3a3074`) and Cinemo's `OnReceiveSYN`
  (`libNmeIAP.so` @`0x202e2c`): `RetransmitTimeout`, `CumulativeAckTimeout`, `MaxRetransmissions` and
  `MaxCumulativeAcks` all zero. A peer-disagreement re-SYN fallback is implemented, mirroring both.
  **NOTE on a negative recorded wrongly in an earlier draft:** it is `AccessorySDK/**External/**` that is
  crypto-only. `AccessorySDK/Support/` is the SDK's own HTTP/CF/Bonjour layer — 89 files including
  `HTTPClient.c/.h`, `HTTPServer`, `HTTPMessage`, `CFLiteBinaryPlist`, `BonjourBrowser` — and §1 of this
  very document cites `AccessorySDK/Support/HTTPClient.h:167`.

#### 8.1 The Zero-Ack derivation, recorded here so it never needs re-deriving

> **⚠️ ONE PARAGRAPH BELOW IS WRONG: "Why we did not adopt template 2 verbatim".** Its rejection of
> `MaxRcvPacketLength = 0xFFFF` was wrong and was cited to block the change that was actually
> required — the tunnel needs `0xFFFF` (docs/carplay/05_METADATA_AND_CONTROLS.md §2.2). Everything else in §8.1 — the predicate, the
> struct/wire maps, the vendor confirmation, Apple's three templates and the decline path — stands.
> Full reasoning: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-43-2`.

Written down because a review found the justification existed ONLY in a code comment, while this
section still said the question was open — exactly the "cite without recording the path" failure
`docs/ops/03_REFERENCE_INDEX.md` exists to prevent. All addresses are in the CarPlaySimulator binary (`docs/ops/03_REFERENCE_INDEX.md` §C) unless
stated. Independently reproduced twice.

**The predicate**, `_iAP2LinkIsNoRetransmit` @ `0x3a3074`:

```
ldrh w8,[x0,#0x6]   ; retransmitTimeout      ldrb w9,[x0,#0x2]   ; maxRetransmissions
ldrh w9,[x0,#0x8]   ; cumAckTimeout          cbnz w9, fail
orr  w10,w8,w9                               ldrb w9,[x0,#0x3]   ; maxCumAck
tst  w10,#0xffff / b.ne fail                 cbz  w9, return_1
```

Exactly four fields; all four must be zero. `MaxOutstandingPackets` and `MaxRcvPacketLength` are **not**
tested. Failure log: `"SYN Param does NOT indicate NoRetransmit: retransmitTimeout=%d cumAckTimeout=%d
maxRetransmissions=%d maxCumAck=%d"`.

`_iAP2LinkIsValidSynParam` @ `0x3a31f4` calls it at `:0x3a3274` and, when true, **skips** the range
checks at `0x3a34e0`-`0x3a3560` (`retransmitTimeout >= 20`, `cumAckTimeout >= 10`,
`maxRetransmissions` in 1..=30, `maxCumAck <= maxOutstandingPackets`) — so the zeros are legal only in
this mode.

**The in-memory struct map** comes from `_iAP2LinkDebugPrintSYNParam` @ `0x3a5980`, whose format
strings name every offset it loads. Note it differs from the wire order — reading a struct copy instead
of a wire parse is the easy mistake here.

**The wire layout**, `_iAP2PacketParseSYNData` @ `0x3a841c` (and its serializer
`_iAP2PacketCreateSYNPacket` @ `0x3a8100` writing the identical map in reverse), cross-checked against
SpeedPlay's `iAP2PacketParseSYNData` @ `libcustomiap.so:0x8706` — byte-for-byte identical tables. This
three-way agreement is what confirms `link.rs`'s documented layout.

**Vendor confirmation**, Cinemo `libNmeIAP.so`: `InitLinkParams` @ `0x200258` zeroes `struct[0x2ce..0x2d3]`
(the same four); `OnReceiveSYN` @ `0x202e2c` `cbnz`-tests the same four on BOTH the wire block and its
own struct, sets the ZeroACK flag only if all are zero, and otherwise logs
`"ZeroACK link configuration is not supported"` @ `0x67aa0` and reverts. `max_outstanding_packets` sits
at `0x2cb`, outside the zeroed range, with its own `1..127` check @ `0x6bae0` — and its log string
`"max_outstanding_packets (ACK after): %u"` @ `0x67ae2` is why that field keeps meaning under Zero-Ack.

**Apple's three per-transport SYN templates**, selected by `-[iAPTransportWrapper
openTransportWithDelegate:]` @ `0x3ff8`:

| | type 0 @`0x3d3338` | type 1 @`0x3d335e` | **type 2 @`0x3d3384`** | ours |
|---|---|---|---|---|
| MaxOutstandingPackets | 5 | 5 | **20** | 32 |
| MaxRcvPacketLength | 4096 | 2048 | **0xFFFF** | 4096 |
| RetransmitTimeout | 2000 | 1500 | **0** | 0 |
| CumAckTimeout | 22 | 73 | **0** | 0 |
| MaxRetransmissions | 30 | 30 | **0** | 0 |
| MaxCumAck | 3 | 3 | **0** | 0 |
| sessions | 3 | 2 (no FileTransfer) | **3, incl. {3, ExternalAccessory, v1}** | 2 |

*(corrected 2026-10-19: the "ours" column is `SYN_PARAMS_ZERO_ACK` (`link.rs:78`) and is only accurate for non-tunnel use. The RCS DataStream/tunnel link now sends `SYN_PARAMS_ZERO_ACK_TUNNEL` (`link.rs:121`) with MaxRcvPacketLength **0xFFFF**, MaxOutstanding still 32, sessions still 2; its retransmit fallback is `SYN_PARAMS_TUNNEL_RETRANSMIT` (`link.rs:129`). See `R-43-2`.)*

Type 0 is the wired profile, type 1 Bluetooth (no FileTransfer session); type 2 is the only Zero-Ack
one, so it is the tunnel. That mapping is **INFERRED** (the enum ordering is not directly readable);
the template contents are **CONFIRMED**.

**Why we did not adopt template 2 verbatim. [SUPERSEDED for the tunnel 2026-10-19 — 0xFFFF IS adopted, `link.rs:121`; reassembly hole closed per `link.rs:88-92`.]** `MaxRcvPacketLength` advertises what the PEER may send
US, and neither `link.rs::parse` nor the coalescing walk buffers a frame split across deliveries — so
0xFFFF would advertise a size we cannot reassemble. 4096 is proven on BT/wired for this same message
set. The real caveat is the opposite one: 4096 also *caps* the largest single message iOS may send us,
so a tunnel-only message above ~4086 bytes would go silently missing. Revisit 0xFFFF for that reason,
not for reassembly. We also declare 2 sessions where Apple declares 3, and session version 1 where
Apple uses 2 — both left alone deliberately so the first hardware test has one variable.

**Apple's own decline path**, for reference: `_iAP2LinkAccessoryActionRestartSYNWithRetransmit` @
`0x3a61ec` resets seq/ack then calls `_iAP2LinkSetSYNAfterNoRetransmit` @ `0x3a3a20`, which sets
maxOutstanding=1, maxRetransmissions=30, maxPacketSize=128, retransmitTimeout=1000, cumAckTimeout=10,
maxCumAck=0 — more conservative than our fallback to `SYN_PARAMS`. Our fresh-`Link` rebuild matches
Apple's seq/ack reset.
- **`kAirPlayProperty_TransportType`** (`AirPlayCommon.h:2196`) is never read in our codebase; we use
  the `CARPLAY_WIRELESS_METADATA` env var as a wireless proxy. Functionally equivalent given one
  `carplayd` per transport, but it is a divergence worth knowing about.
- **The exact wire choreography** of the iAP2 handshake on this transport remains AISpec-side and
  therefore inherited from our BT driver. It is reasonable, but it is not reference-backed — treat it
  as the most likely remaining source of error after the fixes above.

---

## Simulator conformance

<!-- absorbed: ../carplay/03_SDK_GROUND_TRUTH.md -->

Twelve parallel agents checked the project's CarPlay-related code against ground truth:
the **standalone** `CarPlay Simulator.app` binary + `CarPlaySDK.framework` + `iAP2MessageKit`
message archive (read at the time from the mounted Additional-Tools-for-Xcode image,
`/Volumes/Additional Tools/Hardware/CarPlay Simulator.app`. **Path note 2026-08-16:** that volume is no
longer mounted, but the same standalone bundle is on disk at
`~/Documents/carlink/carplay_simulator/CarPlay Simulator.app` *(corrected 2026-10-19: was `~/Downloads/Carplay WWDC/Hardware/…`, no longer present; this is the on-disk mirror)* — indexed in
[`../ops/03_REFERENCE_INDEX.md`](../ops/03_REFERENCE_INDEX.md) §D, and the source these findings
actually came from. Xcode's `CarPlaySimulator.devicekitplugin` ships a SEPARATE copy of
`CarPlaySDK.framework` plus the same ten `VehicleConfig` templates and is the one CLAUDE.md ranks first
today; do not silently substitute one for the other when re-checking a finding below), cross-checked
against the vendored Apple SDK C sources and genuine CCPA wire captures. Every finding below is
grounded in the SDK's public symbols, its shipped constant templates, and the iAP2 message spec —
no assumptions.

### Headline

**No teardown-class (session-breaking) defects, and the crypto/decrypt path is byte-for-byte
correct.** The facets that must be exact for a session to establish and A/V to decrypt — pairing,
stream-key derivation, ChaCha20-Poly1305 nonce/AAD, `/info` keys, SETUP/RECORD negotiation,
audioFormats, HID descriptors — all MATCH Apple's SDK and the genuine capture.

One **live** wire bug was found and **fixed this session** (cluster content switching). The
remaining findings are latent (unreachable) correctness bugs *(corrected 2026-10-19: three of the four are since fixed and reachable at tier extended; the RouteGuidance selector gap is now verified for selector 3, 4/5 untested — see that section)*, one truthfulness issue in the host
UI, and a set of missing optional features the Simulator exposes.

---

### FIXED this session

> **corrected 2026-10-19 — SUPERSEDED.** `send_request_ui_url` / `send_stop_ui_url` no longer exist (`grep` over `events.rs` and `ccpa/carplayd/src/main.rs`: only `send_request_ui()` :1118, `send_show_ui(stream_id,url)` :1154, `send_stop_ui(stream_id)` :1177). The cluster-content path was changed 2026-07-30 to `showUI`/`stopUI` keyed by `params.uuid`, because `requestUI` carries no uuid and `stopUI` ignores a url (`ccpa/carplayd/src/main.rs:1192-1203`). The finding below (url nests under `params`) is historical; do not act on it.

**requestUI / stopUI `url` was at the top level; Apple nests it under `params`.**
`send_request_ui_url` / `send_stop_ui_url` (`events.rs`) emitted `{type, url}`. The SDK sender
builds a `params` sub-dict (`AirPlayReceiverSession.c:5711-5718`) and the receiver dispatcher reads
`CFDictionaryGetValue(inParams, kAirPlayKey_URL)` (`:719-723`); the shipped binary does the same. A
top-level `url` is silently ignored — which is exactly why selecting Map / Instruction Card /
Navigation App never visibly switched the cluster content. Now emits `{type, params:{url}}`.
Verified: `cargo build -p receiver` + `cargo check --tests` clean.

---

### Critical paths — all MATCH (no action)

| Facet | Verdict |
|---|---|
| Pairing (pair-verify ECDH, MFi-SAP, Ed25519) | MATCH — every HKDF salt/info string identical |
| Per-stream key derivation (HKDF-SHA512, raw-ECDH IKM) | MATCH byte-identical |
| Screen ChaCha20-Poly1305 nonce + 128B AAD + counter (opcode-0/body≥16) | MATCH |
| Audio nonce (trailing-8) + AAD (ts‖ssrc) + `[ct][tag][nonce]` | MATCH |
| Control keys (read/write **swapped**) + Event keys (non-swapped) | MATCH — asymmetry reproduced |
| SETUP response keys (all stream types), controlPort-only-on-102 | MATCH exact |
| RECORD event-channel accept + hold-until-accept | MATCH |
| `enabledFeatures` echo + **safe-empty default** | MATCH (our `[]` is *safer* than CCPA's) |
| `/info` keys (audioFormats/displays/hidDevices/modes/features/statusFlags) | MATCH — no invalid keys |
| audioFormats byte layout + audioType routing | MATCH |
| D-Pad (uid-3) descriptor + report bits | MATCH byte-for-byte |
| Touchscreen (uid-1) report (5-byte, width/height→logical-max) | Functional MATCH |
| VehicleConfig YAML keys | MATCH — no keys iOS would reject |
| Cluster content URLs (None/card/map/app) + altScreenURLs allowlist | MATCH one-for-one |
| setLimitedUI `{type, params:{limitedUI}}` | MATCH exact |
| Cluster mechanism (single type-111 + requestUI content-switch) | MATCH — our approach is correct |

This table confirms our accessory implements Apple's documented CarPlay authentication and stream-encryption exactly as specified.

The **iAP2 id21 (DestinationTimeZoneOffsetMinutes = int16 signed)** and **id24
(ArrivalBatteryLevel = uint32)** signedness fixes from the prior audit were **confirmed correct**
against the Kit's authoritative message archive.

---

### Latent correctness bugs (real, but currently unreachable)

> **corrected 2026-10-19 — items 1-3 below are FIXED in code and REACHABLE at tier `extended`/`all`;
> item 4 is still open.** The original text (2026-08) is struck/annotated per item. Evidence is the
> current source, checked field-by-field against `python3 tools/i2mspec_dump.py --message <id> --text`.

~~These are wrong param-id mappings that would corrupt data **if the feed flowed**…~~ Historical
framing: fixing each required *both* correcting the id and enabling the feed.

1. ~~**iAP2 `communications()` 0x4158** — the Kit has no id 3 … mislabeled.~~ **FIXED.**
   `metadata.rs:789 communications()` now maps carrier=4, cellularSupported=5, telephonyEnabled=6,
   mute=9, callCount=10, voicemail=11, availability flags 12-17 (no id 3), 1:1 with the spec dump.
   **Reachability is per tier:** `features.rs:456-460` declares `communications` (start 0x4157, stop
   0x4159, update 0x4158, `COMMS_FIELDS` :359) at `Tier::Extended`, and `message.rs:543` unions
   `received_ids()` into param 7 — so 0x4158 is declared and subscribed at `extended`/`all`, and
   NOT at the compiled default `proven` (`features.rs:202`; `RCV_MSG_IDS` `message.rs:148` itself
   still lacks it). Dispatch: `metadata.rs:1016 MSG_COMMUNICATIONS_UPDATE`. Device evidence: one 0x4158
   observed (`docs/ops/05_AUDITS.md:245-248`); field coverage on the host is partial (same place).
2. ~~**iAP2 `list_update()` 0x4171** — group ids wrong, shifted, extra wrapper.~~ **FIXED.**
   `metadata.rs:887 list_update()` uses groups 1 (recents) / 6 (favorites), flat per-entry ids
   0-9, plus the availability/count scalars (0,2,5,7). `features.rs:361-368 LIST_GROUPS`, feature
   `call_history` at `Tier::Extended` (:462-468). Wire-confirmed 2026-07-29
   (`docs/ops/captures/2026-07-29_iphone_0x4171_listupdate.txt`, `docs/ops/05_AUDITS.md:300-309`).
   Same per-tier reachability as item 1.
3. ~~**iAP2 `now_playing()` 0x5001** — genre/composer/queue/speed mis-numbered.~~ **FIXED.**
   `metadata.rs:515 now_playing()` maps genre=16, composer=18, queueIndex=2, queueCount=3,
   playbackSpeed=12 (all match the spec dump). Those fields still do not arrive: the 0x5000
   subscribe `start_now_playing()` (`metadata.rs:335`) is the wire-captured 40-byte stock request and
   deliberately omits them.
4. **RouteGuidance selector gap — VERIFIED for selector 3 (live, 2026-10-19); selectors 4/5 untested.**
   *(Superseded text, kept as history: "STILL OPEN ... parsers cannot fire".)* `start_route_guidance()`
   (`metadata.rs:350-365`) originally sent param 0 (component id 42) plus selectors id 1 (SourceName) and
   id 2 (SourceSupportsRouteGuidance). The spec dump for 0x5200 says id 3 `SupportsExitInfo` ("Must be
   included to receive ExitInfo in RouteGuidanceManeuverInformation"), id 4
   `DestinationTimeZoneOffsetMinutes`, id 5 `EVChargeInfo` (required for DestinationChargingInfo,
   ChargingStationInfo, Arrival/Departure/CompletedTrip battery).
   **Live result (2026-10-19, tier extended, AirPlay-tunnel arm via carplayd, highway route):**
   - Control, selectors 1,2 only (30 B tunnel frame): a 13-maneuver 0x5202 burst carried param ids 0..11
     only — no id 12, no id 13.
   - Adding selector 3 (0x5200 body 14 -> 18 B, tunnel frame 30 -> 34 B; iPhone-side accessoryd log
     showed the accessory 0x5200 with param ids 0,1,2,3): iOS delivered ExitInfo. The macOS app's
     "Exit Info (raw)" card showed `exitInfo: Exit 9` on maneuver idx 6 (maneuverType 53, Change Highway
     Right) of an 18-maneuver route, in two successive route bursts; other maneuvers had none. Box logged
     36x 0x5202 and 16x 0x5204. So the gate is real, selector 3 suffices, and the Rust path (0x5202 id 13
     -> `maneuver()` "exitInfo" JSON, `metadata.rs:732` -> app) works end to end.
   - **Second accessory type, 2026-10-19 (ledger `R-49-13`):** a stock CPC200 (ARMiPhoneIAP2 v6 appends selector 3 at the sender)
     received id 13 ("Exit 9", one maneuver of a 20-maneuver burst) and the iPhone-side capture caught it, so id 13 is proven as
     **raw wire bytes** there (not archived). The gate is on the iPhone; it held for both accessory types tested (two units, one iOS version, UNVERIFIED beyond that).
   - **Not proven (OCBM box):** id 13 as raw wire bytes — the iPhone-side capture dropped the 0x5202/0x5204 burst
     frames, so the evidence is the box parser's JSON. The hex lever (flag file `/tmp/carplay_rg_hex` /
     env `CARPLAY_RG_HEX`, `log_route_guidance_hex` in `crates/vendor/iap2-core/src/metadata.rs`, called from `crates/vendor/receiver/src/events.rs` and
     `ccpa/iap2d/src/main.rs`) is built but left off. One exit, one route; no off-ramp
     (type 8/22/23) maneuver tested; NEXT MANEUVER card rendering of exit text not observed. Logs and
     screenshots contain route data and are NOT archived: UNVERIFIED (no archived evidence).
   - **Selectors 4/5 (ids 21+):** 25 wire 0x5201 frames had ids 0..20 only while carkitd's pre-wire
     dictionary also held id 21 (timezone) and id 23 (charging list) => selectors 4/5 gate ids 21+ (wire
     absence evidenced; NOT yet tested by sending them). This also explains the old "iPhone sends ALL 21
     fields" note: 21 is what iOS sends with selectors 1,2 only.
   - Code state: selector 3 is sent UNCONDITIONALLY (`start_route_guidance()` in `crates/vendor/iap2-core/src/metadata.rs`; committed 2026-10-19, "iap2-core: send 0x5200 SupportsExitInfo (selector 3)..."; an interim placement); 4/5 unsent.
     Doctrine follow-up not done: selectors 3/4/5 should become app-pushed config
     (see `../ops/04_OPEN_ITEMS.md`).
   Corollary: the original text said "selectors id0/1/2"; id 0 is the component id, not a selector.
   Related, unparsed in the Rust parsers: 0x5201 ids 15 (VisibleInApp), 16/17/18 (Lane
   current index/total/present — lane data is instead taken from 0x5204, `metadata.rs:1172
   lane_guidance()`), 19/20 (SourceName/SupportsRouteGuidance echo), 22/23/25/26 (EV); 0x5202 ids 10/11
   (junction angles) and 12 (`LinkedLaneGuidanceInformation`) — so no maneuver->lane link on the host.
   Pre-change, the 13-maneuver burst had NO id 12 although 5 lane records (LaneGuidanceIndex 0..4)
   arrived: maneuvers are not linked to lane records on the wire. The app's "for maneuver #N" label
   (`MetadataWindow.swift:1082`) is its own derivation: it prints the 0x5204 id-1 LaneGuidanceIndex
   (emitted as `guidanceIndex`, `metadata.rs:1185`; `MetadataWindow.swift:817`) as a maneuver number.

The feeds that flow at the default (`proven`) tier — NowPlaying, RouteGuidance, CallState (`features.rs:427-442`) — parse
cleanly. Lane guidance 0x5204 is `Tier::Extended` (`features.rs:496-499`, `RidesOn("route_guidance")`, so it rides the 0x5200
subscribe but is only declared at `extended`/`all`), as are communications, call history, power, etc.

---

### Truthfulness issue in host UI

**`nightMode` and `rightHandDrive` are advertised in the host UI beyond what the box implements.**
`setNightMode` itself shipped — sender, OCBM command and corrected help text all exist. Still inert:
`VehicleConfig.nightMode` is neither parsed nor emitted as an `/info` key (dropped from the pushed YAML
2026-09-02; the live path is the runtime `setNightMode` command). `rightHandDrive` — **corrected
2026-09-05**: this sentence read "fully inert (no `/info` key, no parser)"; the "no `/info` key" half
was wrong — §3 above lists it among the `/info` keys, and R14G17 `AirPlayCommon.h:1103` /
`AirPlayReceiverServer.c:637-646` / Integration Guide line 385 define it as an Info Message boolean.
Box `/info` emission + parser + app re-emit are landing 2026-09-05, unverified on a device
(docs/carplay/04_CAPABILITIES_AND_CONFIG.md §rightHandDrive). *(corrected 2026-10-19: the code is now present — parse `vehicle_config.rs:47`, emit `info.rs:1270`, app YAML `SettingsWindow.swift:1079`; device verification remains UNVERIFIED (no archived evidence).)* Account: ../ops/06_CORRECTIONS_LEDGER.md `R-26-1`.

---

### Missing optional features the Simulator exposes (not defects)

> **⚠️ PARTIALLY SUPERSEDED — most of the bullets below have since landed (re-verified 2026-10-19):**
> cluster Show-flags, telephony control (HID uid 5), the rotary knob (HID uid 4),
> `enablesMapAppearance`/`mapAppearance`, the alt display dict's `showsInstruments` + `initialURL`,
> and lane guidance display. **Still open:** the full maneuver list, steering-wheel and touchpad HID,
> and `altDisplayPanels[]` as a separate emitted panel array (and the app-side iAP2 call-control
> senders). Symbols and gating: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-26-2`.

- ~~**Cluster "Show" flags** — … exact wire encoding needs one capture to pin.~~ **LANDED** (the
  ledger's R-26-2 still lists it as open — stale there). `ccpa/carplayd/src/main.rs:60 cluster_url()`
  appends `?showSpeedLimit=user|no&showCompass=user|no&showETA=yes|no&maneuverLayout=` to the map and
  Navigation-App surfaces (instructioncard stays bare); `NAV_APPEARANCE` flags (`main.rs:51`) are set by
  OCBM `cmdNavAppearance 0x0B` (`OCBMFraming.swift:262-268`). Encoding vocabulary comes from the
  Simulator's `AirPlayShowUIURL.airPlayURL` (`main.rs:35-40`); owner-confirmed on hardware 2026-08-11 per
  `vehicle_config.rs:749-756`.
- ~~**Telephony control**~~ **PARTLY LANDED.** HID uid 5 telephony (Answer/End/Flash/Mute/DTMF) is driven
  from the host (`ControlsWindow.swift:295 telephony()`), gated on `hidConfig.telephonyButtonsSupport`
  (`ccpa/carplayd/src/main.rs:789`). Favorites/Recents are now rendered (`MetadataWindow.swift`, from
  0x4171). The iAP2 call-control cluster 0x415A-0x4161 is DECLARED in param 6 (`message.rs:127-146`) but
  no sender exists anywhere in `crates/` or `ccpa/` (grep for `MSG_ACCEPT_CALL` etc. finds only
  `spec.rs`/`message.rs`) — declaration only, "never wire-captured" per `message.rs:133`.
- ~~**Lane guidance** display (no fields, no pane).~~ **LANDED.** `metadata.rs:1172 lane_guidance()` ->
  host `nav.lanes` -> "Lanes" card (`MetadataWindow.swift:1052`; `Lane` struct :172).
- **Full maneuver list** — STILL OPEN. The host stores up to 64 maneuvers (`nav.maneuvers`,
  `MetadataWindow.swift:214,635`) but renders only the current one plus a "#i of N received" row (:1087).
- **Rotary knob** — LANDED (uid 4, `carplayd/src/main.rs:787`). **Steering wheel / touchpad HID** —
  STILL OPEN: no `hidDevices[]` entry for either. `touchpadSupport`/`steeringWheelSupport` are parsed
  (`vehicle_config.rs:707-717`) but nothing outside tests reads them; the emitted features word is just
  `if levers::dpad() { 0x1A } else { 0x0A }` (`info.rs:906`), so the Touchpad bit 0x10 is claimed only
  under the `dpad` lever. *(Corrected 2026-10-19: the earlier text said the bits come from those fields.)*
- ~~**`enablesMapAppearance`/`mapAppearance` /info** + runtime appearance toggles.~~ **LANDED**
  (`info.rs` `add_appearance_keys`, `events.rs` `send_ui_appearance_update` / `send_map_appearance_update`).
- **`altDisplayPanels` with `displayProperties:[showsInstruments]` + `initialURL`** — the alt
  *display dict* carries `showsInstruments` + `initialURL` (landed); the separate `altDisplayPanels[]`/
  `displayPanels[]` array is parsed (`vehicle_config.rs:763`) but NOT emitted, and the root-cause claim
  that motivated it is refuted (§5 banner; `R-49-2`..`R-49-6`).
- **Multiple viewAreas / type-112 second cluster / dynamic ViewAreaUpdate** — already scoped as
  future/Android work in docs/carplay/06_AV_PIPELINE.md.

### Cosmetic / intentional divergences (low priority)

- `modes.resources[].transferType = 1` (Take) vs genuine wire `2` — both sustain the session; align to 2 to byte-match.
- Alt-display `maxFPS 60` vs capture `30`, physical dims `0/0` vs `304/76` — inert extra keys.
- ~~Rust receiver `AudioCodec` enum lacks OPUS (bit 28-30 misdecodes to AAC-ELD)~~ — **FIXED; noted 2026-08-16.** `session.rs`'s `AudioCodec` has an `Opus` variant and `decode_audio_format` maps bits 28/29/30 to Opus 16k/24k/48k mono; `info.rs` carries the matching `opus_16k_mono`/`opus_24k_mono`/`opus_48k_mono` tokens (docs/carplay/06_AV_PIPELINE.md). Still only reachable on wireless; PCM-only on the wired path remains correct.
- ~~`send_request_siri_action` … the code sends 1/2 … do not change the working values~~ — **INVERTED,
  CORRECTED 2026-08-16. Do not act on the struck text.** The code now sends **2/3** (`ccpa/carplayd/src/main.rs`, the `CMD_SIRI_DOWN`/`CMD_SIRI_UP` arms),
  changed deliberately on 2026-07-31: R14G17 `AirPlayCommon.h:1366-1369` gives 0 n/a · 1 prewarm ·
  2 buttondown · 3 buttonup, and a device log records `Siri Action - 2` then `- 3` on a real
  press/release. 1/2 was off by one (1 = prewarm). The "confirmed working on-device" evidence covered
  Siri *audio*, not that our `/command` initiated the session — see ../carplay/05_METADATA_AND_CONTROLS.md §2.4b.
- Media-buttons descriptor lags the shipped-ARM template (LMax 5 vs 6, missing usage 0x029E) — inert
  for transport (Apple's own FillReport writes only byte0).

---

## Simulator verification

<!-- absorbed: ../carplay/03_SDK_GROUND_TRUTH.md -->

Twenty verification agents were run against the CarPlay Simulator logs of 2026-07-29 and the
Simulator app bundle itself. This document records what was confirmed, what was refuted, what was
fixed, and what remains open.

Authority order used throughout (owner directive): Apple's licensed R14G17 SDK **and** the CarPlay
Simulator are both normative; CT5 CINEMO is second-tier supportive; everything else supplementary.

### 0. New reference material located

Two artifacts inside the Simulator bundle that this project had not been using:

- **`Contents/Frameworks/CarPlaySDK.framework/Versions/A/CarPlaySDK`** (sourceVersion 509.11) — the
  current receiver-side SDK. Contains the live HID descriptor table, the `_DataStreamSessionSetup`
  client-type dispatch, and the `/info` builder. This is the single most useful file in the bundle.
- **`Contents/Frameworks/iAP2MessageKit.framework/Resources/iap2messages-external.i2mspecarchive`**
  (196,872 B) — an iAP2 message spec archive **distinct from** the `iap2messages-internal` one
  `tools/i2mspec_dump.py` reads from Xcode. `--archive <path>` accepts it.

Also confirmed present and already in-repo: `reference/carplay_sdk/apple_vdc/VDCSchema-External.json`
carries the VehicleDataProtocol IIDs, formats and ranges. `Navigation.vdc.json` does **not** — it is
an instantiation config only. A future session looking for the schema will otherwise open the wrong
file.

### 1. Apple's HID descriptor table (`CarPlaySDK.framework`)

One contiguous `__TEXT,__const` blob, `0x2D94DC`–`0x2D978A`, segmented by matching each `malloc`
size and `adrp/add` target to its creator function:

| offset | len | function |
|--------|-----|----------|
| 0x2D94DC | 39 | `HIDDPadCreateDescriptor` |
| 0x2D9503 | 70 | `HIDKnobCreateDescriptor` |
| 0x2D9549 | 51 | `HIDKnobBasicCreateDescriptor` |
| 0x2D957C | 39 | `HIDKnobMinimalCreateDescriptor` |
| 0x2D95A3 | 40 | `HIDMediaButtonsCreateDescriptor` |
| 0x2D95CB | 22 | `HIDProximityCreateDescriptor` |
| 0x2D95E1 | 92 | `HIDSteeringWheelCreateDescriptor` |
| 0x2D963D | 57 | `HIDTelephonyCreateDescriptor` |
| 0x2D9676 | 160 | `HIDTouchpadMultiCharacterCreateDescriptor` |
| 0x2D9716 | 80 | `HIDTouchpadOnlyCreateDescriptor` |
| 0x2D9766 | 37 | `HIDTouchpadButtonsCreateDescriptor` |

The framework exports 22 HID symbols in total, so this table is not the whole surface.

**Knob (70 B), KnobBasic (51 B) and Telephony (57 B) are byte-identical to the R14G17 C source.**
Verified three independent ways: byte comparison against `AppleCarPlay/Platform/*.c`, item-tree parse
from HID first principles, and `dlopen` + direct invocation of the framework functions. Both knob
templates are emitted verbatim (`memcpy`, no post-write patching).

Two descriptors have evolved since 2017, neither in a way that affects report layout:

- **MediaButtons 37 → 40.** A clean additive extension: Logical Maximum `05`→`06` plus one inserted
  Consumer usage `0A 9E 02`. Indices 0–5 unchanged. (Usage `0x029E` was not identifiable from any
  source at hand; recorded as a raw value.)
- **TouchScreen.** Not a divergence — a *different function*. The Simulator calls
  `HIDTouchScreenMultiWithCancelCreateDescriptor`, which has no R14G17 counterpart
  (`grep -rn "WithCancel"` over the SDK returns zero). Like-for-like, `…MultiCreateDescriptor` went
  133 → 107 via encoding compaction; the cancel variant is that plus 4 bytes = 111. Report layout is
  unchanged at 12 bytes; cancel adds a *bit*, not a byte.

Width/height patching cannot change descriptor length — every patch offset lands on the two data
bytes of an `0x26` (Logical Maximum, 2-byte) item, and `*outLen` comes from `sizeof(template)`.
Confirmed by invocation at 800×480, 1920×720, 1280×1024, 4095×4095 and 1×1.

**Porting note.** The knob's padding uses `81 01` (Constant,Array) where convention is `81 03`
(Constant,Variable). It is legal HID and proven against real iOS, since Apple ships those exact
bytes. Do not "fix" it during a port.

### 2. HID device model

Apple builds HID **per video stream**, each stream getting a complete independent set. Observed:
`VideoStream.Main` → knob 1, touchScreen 2, telephonyButtons 3, mediaButtons 4;
`VideoStream.Alt1` → 5, 6, 7, 8.

IDs are **allocated at runtime**, not configured — no ID fields exist in any YAML or in `HIDConfig`.
`HIDController.reset()` (`0x10007f30c`) initialises a counter to 1 before the loop and carries it
across streams. Allocation order: knob → touchScreen → **steeringWheel** → telephonyButtons →
mediaButtons → touchpad → touchpadButtons → dPad.

Two traps for any implementation:

1. **`steeringWheelID` consumes an ID but emits no device** — there is no `addSteeringWheelDevice` in
   the binary. Enabling `steeringWheelSupport` or `notificationButton` leaves a *gap*. The emitted
   IDs are not guaranteed dense; do not hardcode `1..4 / 5..8`, replicate the allocator.
2. **Array emission order is not stable.** The second pass emitted Alt1's block before Main's with
   identical IDs. Bind by ID, never by array index.

Apple emits at most **7** HID device classes.

#### displayUUID binding

`hidDevices[].displayUUID` is a foreign key into `displays[].uuid`, and the binding key is the
**video-stream uuid** (the `uuid` of the `type: 111` entry), *not* `displayPanels[].uid`. Established
by disassembling all seven `_AirPlayInfoArrayAddHIDDevice` call sites — the 9th argument is loaded
with the literal `"VideoStream.Main"` / `"VideoStream.Alt1"`.

The mechanism is in the 2017 protocol (`AirPlayCommon.h:922`, `HIDUtils.h:151`,
`AirPlayReceiverSession.c:5436`) but R14G17's reference implementation calls `ScreenCopyMain()` once
and stamps that UUID onto every device. Per-*display* binding is 2017; per-*video-stream* scoping is
post-2017.

**Architectural consequence, and it is decisive:** `AirPlayReceiverSessionSendHIDReport`
(`AirPlayReceiverSession.c:5404-5422`) puts only the HID device uid and the report bytes on the wire
— there is no display or stream selector. The association is established exactly once, at `/info`
time. If every declared HID device points at the main display, no report can reach an alt display,
and no configuration or later message can recover it.

CINEMO corroborates the key (`hidDevice[%zu] displayUUID: %s, hid_uid: %u, name: %s`, and
`requested display UUID not found: %s` — a match-or-fail lookup).

### 3. RCS DataStream client types — seven, not four

Read literally from the `CFStringCompare` chain in `_DataStreamSessionSetup` (`0xdc04`–`0xdfe4`),
where each comparison sets a type tag and loads its name CFString on the next instruction:

| tag | name | UUID |
|-----|------|------|
| 1 | iAP | `E9459FD0-BCAD-4C45-820F-1E72447EF2F2` |
| 2 | LogTransfer | `75AD9926-4777-42B2-A7D8-823EBEECF7AA` |
| 3 | VehicleDataProtocol | `3E2F3C61-AAD0-42CB-A8AA-BF22186DA62E` |
| 4 | VehicleDataProtocolHigh | `FF4A6720-F2BE-4F56-A3E1-DB3B4E37D634` |
| 5 | UrlFling | `A6B27562-B43A-4F2D-B75F-82391E250194` |
| 6 | OverlayUI | `E3DC3EA6-E6C3-4B30-847C-B7ACFEBEA654` |
| 7 | SenderSettingsData | `BB493F61-A6B8-4769-8D74-80C23A9F71C4` |

docs/carplay/05_METADATA_AND_CONTROLS.md's "positional and therefore inferred" hedge is retired. Our gate is an allowlist of one, so
tags 5–7 were always correctly excluded.

**Divergence:** an *absent* `clientTypeUUID` sends Apple's receiver to the teardown path (`-6735`),
identically to an unrecognised one. We default absent to iAP. The branch should be unreachable, so
this is permissive-but-harmless — recorded, not changed.

**DataStream `id` is a per-session, setup-order counter that restarts at 1**, while the RCS channel
counter runs globally. The stable kind identifier is `clientTypeUUID`. `session.rs` already keys
`av_streams` on `(type, channelID)` for exactly this reason — no change needed.

### 4. `/info` key surface

The SDK requests **52** distinct server-level values. `EventLogger` brackets them into three phases:
startup (20), `SessionSetupRequest` (2), and **`InfoRequest` (36)** — and the InfoRequest block
reproduces R14G17's `AirPlayCopyServerInfo` in exact order, including the session-level `modes` fetch
in the same position. That is the `/info` builder, identified structurally.

The callback set and the wire set **intersect**; neither contains the other. R14G17 writes at least
seven `/info` keys with no callback (`protocolVersion`, `pi`, `sourceVersion`, `statusFlags`,
`keepAliveSendStatsAsBody`, `keepAliveLowPower`, `txtAirPlay`).

*(Snapshot of 2026-07-29, STALE as a current list, corrected 2026-10-28: `info.rs` now also emits `limitedUIElements`, `oemIcon`/`oemIcons`/`oemIconLabel`/`oemIconVisible` and `rightHandDrive`; the counts below were not re-taken. Still absent as of this check: `bluetoothIDs`, `hardwareRevision`, `hidLanguages`, `vehicleInformation` — `nightMode` and `OSInfo` not found by name either.)* We emitted 14 of the 52, or **10 on a default build** (four env-gated). Twelve keys were present in
both Apple sources and absent from ours: `bluetoothIDs`, `hardwareRevision`, `hidLanguages`,
`limitedUIElements`, `nightMode`, `oemIcon`, `oemIcons`, `oemIconLabel`, `oemIconVisible`, `OSInfo`,
`rightHandDrive`, `vehicleInformation`. **All twelve are optional** — each insert in
`AirPlayCopyServerInfo` is guarded by `if( obj )`, and the Simulator itself declines `oemIcon`. Their
absence is spec-conformant.

`clientOSBuildVersionMin` is **not** an `/info` key — it is read inward and used at
`AirPlayReceiverSession.c:910` to reject clients below a minimum.

`nightMode` and `rightHandDrive` are a step further along: the macOS host already writes both into
the generated YAML with user-facing toggles (`SettingsWindow.swift:322-323` @Published properties; *corrected 2026-10-19: was :363-364, drifted*), but
`vehicle_config.rs` never parses them, so serde drops them. Already recorded as a defect in docs/carplay/03_SDK_GROUND_TRUTH.md
§89-94. *(Historical as of 2026-09-02, when both were dropped from the YAML. 2026-09-05: `rightHandDrive`
alone comes back — it is one of the twelve optional `/info` keys above, and box emission + parse + app
re-emit are landing, unverified on a device; docs/carplay/04_CAPABILITIES_AND_CONFIG.md §rightHandDrive.)*

### 5. Alt / cluster video — root cause

> **⚠️ THE CENTRAL CLAIM OF THIS SECTION IS REFUTED.** Cluster content works and its elements
> are toggleable — via `showUI` QUERY PARAMETERS, not the missing `displayPanels[]`.
>
> What still stands: the factual inventory (CarPlaySDK emits both `displays` and
> `displayPanels`; the ~19-key panel dict; `legacyDisplayInfo` as the toggle;
> `DisplayPanelProperty` having exactly three cases). What does NOT: the causal claim, **the
> "Secondary gaps" paragraph** (`enablesMapAppearance` is parsed now, and all three appearance
> commands exist), and **the "no capture exists" caveat** (one does).
>
> Full reasoning, and the four corrections that followed it, are in
> [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-49-2`–`R-49-6`.

**We emit only the legacy flat `displays[]` array.** `CarPlaySDK` 509.11's `AirPlayCopyServerInfo`
emits **both** `displays` and `displayPanels`, and iOS requests both. The modern panel dict carries:

```
zIndex, widthPhysical, heightPhysical, initialVideoStreams, properties, videoStreams,
maxFPS, primaryInputDevice, initialURL, viewAreaTransitionControl, viewAreaStatusBarEdge,
viewAreaSupportsFocusTransfer, drawUIOutsideSafeArea, initialViewArea,
uiAppearanceMode, uiAppearanceSetting, mapAppearanceMode, mapAppearanceSetting, zoomFactor
```

`properties` (the `displayProperties` array), a nested `videoStreams[]`, and a per-stream
`initialURL` **exist nowhere else on the wire**. `AccessoryConfig.legacyDisplayInfo` is the toggle
that selects the old layout.

This is sufficient to negotiate and receive the type-111 stream and structurally incapable of
defining anything inside it — which is the observed symptom exactly. The genuine Carlinkit CCPA
wired capture shows the identical shape and identical limitation: its type-111 entry has no
`showsInstruments`, no `initialURL`, and there is no `displayPanels` block anywhere in the session.

Our `showsInstruments` and `initialURL` are **hardcoded constants placed on a legacy `displays[]`
entry**, where neither key exists in any Apple vocabulary (R14G17 `ScreenUtils.h:90-123`, CarPlaySDK
509.11, or the CCPA wire). Meanwhile the YAML's real `altDisplayPanels[].displayProperties` and
`altVideoStreams[].initialURL` are never parsed.

Secondary gaps: `uiContext` is a first-class SETUP feature (immediately after `altScreen` in the
feature table) with a `changeUIContext` command; we echo neither it nor the two `uiContext*URLs`
keys. `enablesMapAppearance` is set in all ten Apple templates and required per docs/carplay/06_AV_PIPELINE.md:126; we do
not parse it, and `mapAppearanceUpdate` / `changeMapZoomLevel` / `uiAppearanceUpdate` are absent.

**Not the cause** (each investigated and refuted): missing `approvedClusterURLs` — it is a
Simulator-side authoring field whose wire projections we already emit with a full 3-URL set,
exceeding the CCPA, and none of Apple's four working cluster configs sets it. Missing alt-stream HID
— Apple's four cluster templates carry no `hidConfig` on `altVideoStreams[]`, and the CCPA binds no
HID to its type-111 display.

`DisplayPanelProperty` has exactly three cases: `dpManaged`, `additionalContent`, `showsInstruments`.
Only the last appears in a stock template.

**Caveat:** no capture exists of any device *successfully driving* cluster content. The step from
emitting `displayPanels[]` to controllable cluster elements is inferred from Apple's schema and
command surface, not observed.

### 6. AppDiscovery — params 10/11 exonerated

`IAPEnablementConfig.appDiscovery` and `.externalAccessoryProtocol` are **peer booleans**, both
defaulting false, treated as strictly orthogonal by every consuming path. Apple's own accessory sends
`0xAD00` while declaring neither Identify param 10 nor param 11 — and `AppMatchTeamID` does not
appear in the Simulator binary at all.

`0xAD00 StartAppDiscoveryUpdates` parameters: 0 `CarPlayAppCategories`, 1 `CarPlayAppListMax`,
2 `CarPlayAppIconSize`, 3 `ExternalAccessoryAppCategories`, 4 `ExternalAccessoryAppListMax`.
Apple's defaults: categories `[]`, listMax 20, iconSize 120.

Categories (param ids): 0 AllCarPlayApps, 1 Messaging, 2 Calling, 3 Navigation, 4 Audio,
5 Automaker, 7 QuickOrdering, 8 EVCharging, 9 Parking, 10 Productivity, 11 Fueling, 12 DrivingTask.

**Still UNDETERMINED:** what flips `CarPlayAppListAvailable` from 0 Unknown to 2 Accepted. No
normative source states it. The one hint is vocabulary — the CarPlay enum is
Unknown/**Declined**/**Accepted** (consent language) where the ExternalAccessory sibling is
Unknown/Unavailable/Available (availability language) — suggesting a user-consent decision on the
phone. That is inference, not text.

**Next experiment, no box work required:** run Apple's own Simulator against the same iPhone with
`iapConfig.enablementConfig.appDiscovery: true` and no EA config, and read the `0xAD01` reply's
`CarPlayAppListAvailable`. If Apple's accessory gets **Accepted**, the gate is in our encoding and we
diff the bytes. If it also gets **Unknown**, params 10/11 are exonerated on the wire too and the next
move is a prompt hunt in `accessoryd`, not another Identify permutation.

### 7. Fixes applied in this pass

Code:

- `features.rs` — `app_discovery` trigger gains `also: &[0xAD03]`. We declared `0xAD04` as received
  without declaring its sender `0xAD03` in param 6, which is the
  `OptionalMsgNotValidWithoutRequiredMsgs` shape (docs/carplay/05_METADATA_AND_CONTROLS.md §5.6 rule 2). Apple declares
  `0xAD00`/`0xAD02`/`0xAD03` together. Safe w.r.t. the byte-pinned BT-time Identify: the
  `TransportComponent::Wireless` arm builds params 6/7 from a hardcoded list and never consults the
  feature table. Both byte-pin tests updated deliberately.
- `tools/i2mspec_dump.py` — `countExpressionEnum` legend corrected. The old map printed
  `[optional]` for every **required** parameter and omitted the genuinely-optional case 3. Decoded
  from `+[I2MTypeUtils …]` in `iAP2MessageKitCore`. This legend has now been wrong twice in this
  project's history; both times it produced a confident false reading of Apple's spec.

Comments and docs (no behaviour change):

- `info.rs` — display feature bit table corrected (`0x02` Knobs, `0x04` LowFi, `0x08` HighFi,
  `0x10` **Touchpad**, `0x20` **DirectionButtons**). The emitted value is unchanged.
- `../carplay/05_METADATA_AND_CONTROLS.md` §2.7.4 — correction box; the section's conclusion is wrong and is retained only for
  provenance.
- `docs/ops/03_REFERENCE_INDEX.md` — the claim that R14G17 has no "Knob-minimal builder" was wrong. `HIDKnob.c` defines
  both builders. This erroneous line plausibly caused the 2026-07-06 guessed-descriptor incident.
- `docs/carplay/05_METADATA_AND_CONTROLS.md` §1.2 — seven client types, hedge retired, Apple's absent-UUID behaviour recorded.
- `session.rs` — "four client types" → seven; the `docs/carplay/05_METADATA_AND_CONTROLS.md §5` cross-reference was wrong (the
  client-types topic is §1.2). **Correction, 2026-08-01:** `session.rs:882`'s client-types citation is
  `§1.2`. Separately, `crates/vendor/receiver/src/iap_tunnel.rs:95,145` also carried a stale `docs/carplay/05_METADATA_AND_CONTROLS.md §5`,
  but for the **SYN/DETECT resend** topic — whose correct section is **§2.1** ("Apple's implementation is
  available locally"), NOT §1.2 — and both were fixed to `§2.1` in the 2026-08-01 pass.
- `message.rs` — param-30 comment corrected per-field, plus the `lane_guidance` consequence and the
  transport-gating prerequisite.
- `features.rs` — `app_discovery_body` doc: sub id was misstated as 3 (code was right), Apple's full
  parameter set, `CarPlayAppListMax` omission, params 10/11 exonerated.
- `vehicle_config.rs` — `HidConfig` doc: Apple has 21 fields, we parse 2 and act on 1;
  `knob_support` marked as parsed-but-not-wired with the descriptor provenance.

Artifacts: `scratchpad/packed_new/` was **stale** — `ocbmd` and `btd` were
byte-identical to the older `packed/` set and predated the 2026-07-29 CLOEXEC and `ssp_enabled()`
work. All four repacked with UPX 3.96 from the current build.

### 8. Open, in priority order

1. **~~`lane_guidance` is inert.~~ REFUTED — see [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-49-8`.** Apple:
   Identify param 30 sub 8 `MaxLaneGuidanceStorageCapacity` *"Must be included to receive Lane
   Guidance instructions."* This section concluded that because we never send it, `0x5204` can
   never arrive regardless of tier or subscribe. **The 2026-07-29 "x12" capture claim is UNVERIFIED (no archived evidence): no file in `docs/ops/captures/` contains a 0x5204 record; see `../ops/05_AUDITS.md:565`.** *(Superseded 2026-10-19 by a live observation: at tier extended on the tunnel arm, WITHOUT Identify param 30 sub 8, the box counted 9 and 16 0x5204 messages in two route bursts and the app Lanes card populated. Still no archived raw capture — live-observed only, UNVERIFIED (no archived evidence) as a file.)*
   The param-30 expansion is still genuinely open; this stated reason for it is wrong. Same one level down: `metadata.rs` parses
   `CurrentRoadName` and `DestinationName`, which iOS will not populate while subs 2 and 3 are
   absent.
2. **Param 30 expansion.** Gate to `AirPlayTunnel` **first** — the block is currently ungated and
   growing it would grow the byte-pinned BT-time Identify. Then subs 2/3/4 = 40, 5 = 64, 7 = 32, and
   sub 6 = 4 / sub 8 = 2 — **not 0**. Zero on 6 or 8 makes iOS dump the entire route as individual
   `0x5202`/`0x5204` messages at route start and on every reroute, each needing its own
   ChaCha20-Poly1305 open and TLV walk on a 528 MHz single core. CINEMO ships both at 0, but that is
   an Android head unit with far more headroom.
3. **Emit `displayPanels[]`** — the alt-content root cause (§5). *(corrected 2026-10-19: the root-cause claim is REFUTED — see the §5 banner and `vehicle_config.rs:735-761`; cluster content works via `showUI` query flags. Emission would buy schema completeness only.)*
4. **HID port** — knob (70 B) and telephony (57 B) from `CarPlaySDK.framework`, replicating the
   allocator rather than hardcoding IDs. Lower risk than the incident history suggests: CINEMO passes
   MFi with descriptors that are *not* Apple's bytes (GM's knob is 92 B and drops AC Home entirely),
   so Apple's descriptors are a reference, not a required literal. Needs a hardware session.
5. **`displays[].features` value** — we advertise `0x10` Touchpad with no touchpad device *(corrected 2026-10-19: only when the `dpad` lever is on — `info.rs:906` `if levers::dpad() { 0x1A } else { 0x0A }`; the default is 0x0A = Knobs|HighFidelityTouch, and Knobs now has a device when `knobSupport` is armed)*. Correcting
   it is a wire change; the current value is hardware-validated, so it needs a session.
6. ~~**`knob_support`** is parsed and read nowhere; wiring it means adding the knob HID device (4).~~
   **STALE — corrected 2026-08-10.** `knob_support` IS wired end to end: `carplayd/src/main.rs`
   `events::set_knob_advertised(vc.knob_support())` publishes the uid-4 `hidDevices[]` entry, and the host
   has `HIDKnobView`/`HIDComplexKnobControlView` in `ControlsWindow.swift`. Item 4's descriptor
   port is also DONE and byte-verified: `info.rs::knob_descriptor()` is all 70 bytes of R14G17
   `HIDKnobCreateDescriptor`, and `telephony_descriptor()` all 57 of `HIDTelephonyCreateDescriptor`
   (compared programmatically 2026-08-10). What remains for both is HARDWARE validation of a 4th
   and 5th hidDevices entry, not code.

### 9. Claims refuted during this pass

Recorded because the project's method note asks for it. Each was stated confidently before
verification and did not survive it:

- That docs/carplay/03_SDK_GROUND_TRUTH.md conflated the VDC priority channel with the DataStream id. It does not — docs/carplay/03_SDK_GROUND_TRUTH.md's
  `{1,2}` are C function-name suffixes, and the capture *corroborates* its ch2 = high mapping.
- That R14G17's TouchScreen descriptor "diverged" 133 → 111. Different functions.
- That thirteen `/info` keys were missing and constituted a conformance gap. Twelve, all optional.
- That we subscribe before authentication where Apple subscribes after. Our state machine makes
  `0x1D02` unreachable until `Authenticated`; both send off the final handshake milestone.
- That `IAPEnablementConfig` maps almost 1:1 onto `features.rs`. ~39% overlap by union, and not
  independent — `features.rs` ids come from the same Apple spec archive.
- That the `/info` callback set is a superset of the wire keys. They intersect.
- That Identify param 10 gates AppDiscovery. Apple's own accessory declares neither 10 nor 11.
- That missing `approvedClusterURLs` and missing alt-stream HID explain the alt-video symptom.
- That Apple's SDK drops the non-scalar VehicleDataProtocol characteristics. Those errors come from
  the Simulator's own demo UI layer; the SDK accepted every value, and one failing IID (259) is a
  scalar.

### 10. Outcome of the fixes — one refuted on hardware

Deployed and retested the same day (`carplayd ea865488f8b55acf1c463193652dc1f5`):

**`limitedUI` — the 2026-07-30 "REFUTED" verdict was WRONG, and is corrected here (2026-09-08).**

> **RESOLVED — `limitedUIElements` IS the gate. Device-proven 2026-09-08** on the AAOS app (gminfo38,
> owner-observed both directions): declare the element array and the Apple Maps keyboard icon
> disappears on shift out of Park, leaving only Siri, and returns in Park. Nothing else changed —
> same `setLimitedUI` bytes, same `limitedUI: false`.
>
> **Mechanism.** `limitedUI` is a BOOLEAN; `limitedUIElements` is the SET it applies to. iOS 27 CarKit
> carries both on `CARScreenInfo` (`B _limitedUI`, `Q _limitedUIElements` — a bitmask), and
> `CARSessionConfiguration._limitableUserInterfaces` is built by
> `+_limitableUserInterfacesFromLimitedUIValues:` from the `/info` STRING ARRAY. **With no array the
> mask is 0, so `setLimitedUI(true)` faithfully restricts the empty set** — 2xx, no error, no effect.
> That is exactly what the original test saw and misread as "iOS ignores the command".
>
> **Why the elimination reasoning failed.** The 2026-07-30 run served no `limitedUIElements` because
> the box's YAML carried no `limitedUIConfig` at all — that feature landed the same day. So the mask
> was empty in the failing case, and the key was never actually under test. The supporting claim,
> "Apple's own is empty in a working session", was never observed: the Simulator logs on this Mac show
> only `/info` being REQUESTED (`Requesting Server Value For - limitedUIElements / limitedUI`) and
> contain no toggle exercise at all. An untested assumption was recorded as an eliminated candidate.
>
> The lesson worth keeping: a command that is acked 2xx and byte-identical to Apple's proves the
> TRANSPORT, never the SEMANTICS. A boolean whose scope is declared elsewhere will always ack.

Two things remain untested rather than refuted: whether anything restrictable was on screen when the
toggle fired (limited UI acts on keyboards and long lists), and our deliberate non-enforcement of
Apple's `sessionStarted` gate in `send_command`.

**Everything else in §7 deployed without regression** — 261 tests, clean ARM cross-build, session
establishes, A/V and metadata flow. The Siri, cluster-content and TEARDOWN fixes are structurally
verified but have not been individually exercised on hardware.

> **⚠️ THE OUTCOME BELOW IS FALSE — this pass DID cause a regression.** The `streamConnectionID`
> guard it shipped was applied to every stream type, and type 130 (the RCS DataStream carrying
> wireless iAP2) legitimately arrives without one — so the wireless metadata plane was dead for
> ten days, 2026-07-31 → 08-10. "A/V and metadata flow" above was true of A/V only.
> Full account, evidence and the process lesson: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-49-7`.

### 11. The boundary this investigation hit

Across `showUI`, `stopUI`, `requestUI`, `forceKeyFrame`, `changeUIContext`, `requestSiri` and
`changeModes`, the **accessory** SDK's only failure paths are null-arg (`-6705`), allocation failure
(`-6728`) and session-not-started (`-6709`). There are no capability checks anywhere, and
`AirPlayReceiverSessionHasFeature*` has zero internal callers — those accessors exist purely for the
platform integrator.

Consequence: **no amount of accessory-side reverse engineering can confirm why iOS ignores a
well-formed command.** The gating lives in the iPhone. The productive move is to make `/info` match
Apple's reference emission key-for-key — including empty-but-present values like `buttonInfo: []` and
`limitedUI: false` — and then diff a live Simulator session against ours on the same phone.
