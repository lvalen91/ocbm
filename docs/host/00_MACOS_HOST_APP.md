# The macOS host app

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 17; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

The shipping host application: decode, UI, settings/YAML authoring, and the SDK audit that shaped it.

**Other app-visible UI/session features (2026-09-03) with no dedicated section here** — pointers only:
`OCBMClient` now re-sends `CT_SETTIME` after every SUBSCRIBE, not just at HELLO; the Box Log window
gained dynamic per-source colours, backfill-entry dimming and a Hide-history filter
(`App/BoxLogWindow.swift`, `App/BoxLogStore.swift`); the pairing-code panel gained **Pair**/**Cancel**
buttons, shown only when interactive numeric-comparison pairing is on (`docs/wireless/01_BT_AND_RADIO.md`);
and the CCPA tab gained a confirmed **Enter NCM** action plus a `carlink://box/enter-ncm` URL handler
(`open -a <app> carlink://box/enter-ncm`, `App/AppDelegate.swift`). Wire-level detail for all of these is
in `docs/carplay/01_OCBM_PROTOCOL.md` and `docs/wireless/01_BT_AND_RADIO.md`, not here.

## Host app — SDK audit and plan

<!-- absorbed: ../host/00_MACOS_HOST_APP.md -->

Consolidated findings from a 12-facet audit of the macOS host app against Apple's CarPlaySDK ground
truth (docs/carplay/03_SDK_GROUND_TRUTH.md, the SDK binary, Apple's YAML/VDC templates). Each facet classified every behavior
ADHERES / DEVIATES, and every deviation was verified against the SDK and either JUSTIFIED or flagged
BUG/GAP. Governing rule: adhere to the SDK; deviations must be verified + justified.

### Headline
The crypto, framing, audio, video-decode, single-touch, window geometry, and session-lifecycle cores all
**adhere** and are byte-for-byte faithful to the box/SDK. The real issues are (a) **one correctness bug
that explains the current 4K poisoning**, and (b) an **incomplete OCBM migration** — touch was ported to
the OCBM/CarPlay path but commands, multi-touch, and diagnostics were left on the now-dead legacy `adapter`
path. The deep SDK deviations (per-packet RTP seq, NACK retransmit, flow control) are all correctly
delegated to the box or deferred with justification — roadmap, not bugs.

### Correction to one agent's framing
The nav/AltVideo audit concluded the app "can't be Apple-faithful because the dongle is the AirPlay
endpoint." That is true for the *legacy stock-firmware* model but **wrong for ccpa_custom**: our own
`carplayd` on the box IS the AirPlay receiver, so real AltVideo / VDC navigation / cluster ARE
implementable (in the box, forwarded over OCBM). The agent's useful finding stands: the 0x2C/508/509 nav
path is legacy proprietary code to retire — not a fidelity target.

### Tier 0 — correctness bugs — **ALL THREE RESOLVED (2026-08; see the 4K-track section below)**
1. **Video-frame size cap rejects 4K IDRs → constant poisoning [A/V-recovery].** `OCBMAVDecrypt.nextVideoMessage`
   caps a reassembled video message at `2 × OCBM.maxPayload` = 131072 B. A 4K IDR is far larger, so **every
   4K keyframe is rejected** → resync → the decoder never gets a clean IDR → permanent P-frame poisoning.
   This is a pre-existing bug (old parser had the same cap): benign at 1920×720, fatal at 2400×960+/4K, and
   it is the direct cause of "corrects on a keyframe, but constant." **Fix:** the reassembled *video-message*
   limit is a different concern from the OCBM *per-frame* transport limit (the seam already reassembles
   across OCBM frames) — size it to the max coded frame for the negotiated resolution/codec (several MB).
   **RESOLVED:** `maxVideoMessage = 16 MB` in `OCBMAVDecrypt.swift`; 4K IDRs pass, decoder gets clean keyframes.
2. **Renderer keyframe-recovery is a no-op [video-decode].** `VideoDecoder.onNeedsKeyFrame` (fired on a
   flush-required-to-resume) is log-only in the OCBM path; only the seq-gap detector requests keyframes.
   **RESOLVED:** `onNeedsKeyFrame` is a live wired callback → `requestKeyframe()`. (The class was renamed
   from `H264Decoder` and now decodes H.264 **and** HEVC.)
   **Fix:** wire `decoder.onNeedsKeyFrame → client.requestKeyframe()` (reuse the ≤1/500 ms throttle) so a
   renderer-driven poison also forces an IDR. Cheap, complements Tier 0 #1.
3. **Main-thread block on transport-lost teardown [USB].** `handleOCBMTransportLost → endSession →
   disconnect()` did a `queue.sync` blocking `WritePipeTO` (~7 s) on an already-dead pipe → UI beachball on
   the exact failure it's recovering from. **Fix:** send STOP fire-and-forget; never `queue.sync` a bulk
   write from the main actor.
   **RESOLVED (verified 2026-08-16):** `OCBMClient.disconnect()` is async end to end — the timer cancel +
   generation bump + `helloAcked` reset ride `queue.async`, and STOP is best-effort behind a bounded
   `DispatchSemaphore` grace while `transport.stop()` AbortPipes the endpoint. No `queue.sync` remains.

*(Deploy Tier 0 alongside the already-built ocbmd backpressure change — backpressure reduces the drops,
the frame-cap fix lets the recovery keyframes actually land. Together they are the real 4K fix.)*

### Tier 1 — the incomplete-OCBM-migration cluster (functional holes)
4. **Keyboard/media command surface is entirely dead in OCBM mode [command-map, touch, legacy].** Home,
   Siri, play/pause/next/prev, D-pad, knob all route to the nil legacy `adapter` — only touch was migrated.
   **Fix:** add OCBM `CH_INPUT` opcodes for HID media-buttons (uid 2) + D-pad, and `/command`
   (`requestUI` for Home, `requestSiri` for Siri); rewire `didPressCommand` to `ocbmClient`. Box already
   advertises the media-buttons HID device. (Ties to tasks #19 + #20.) Also: `disableBluetooth` is a
   fabricated non-Apple command (drop it); `NowPlayingManager` is never instantiated (dead — wire or remove).
   **RESOLVED (verified 2026-08-16) — the "entirely dead" statement is no longer true.**
   `AppDelegate.carPlayView(_:didPressCommand:)` guards on `ocbmClient` and routes the whole surface:
   media keys → `sendMediaButton` (uid 2), Home/Back/D-pad/knob → `sendNav` (uid-3 HID D-Pad — NOT
   `requestUI`, which did nothing as a Home button, 2026-07-12), Siri → `sendCommand(cmdSiriDown/Up)` with
   the paired UP on a 0.3 s deadline. `NowPlayingManager` was retired by deletion (no such file).
5. **Multi-touch / gestures dead in OCBM mode [touch].** Pinch + two-finger scroll are captured but routed
   to the nil adapter, and the box only advertises a single-touch HID descriptor. **Fix:** 2-point
   `CH_INPUT` sub-frame + box `HIDTouchScreenMulti` descriptor (12-B report). Interim: degrade `scrollWheel`
   to a single-finger drag when `ocbmClient != nil` so map scroll works. (Ties to #20.)
6. **HELLO not gated on HELLO_ACK → boot-race silent death [session, USB].** Confirmed by 3 audits and
   task #34. On a boot race the HELLO is lost, the app SUBSCRIBEs into the void, the 5-error path can't fire
   (timeouts, not errors), and the UI shows a misleading "Waiting for phone…". **Fix:** retransmit HELLO
   until HELLO_ACK with a bounded deadline; gate SUBSCRIBE/heartbeat on it; surface a real "box not ready".

### Tier 2 — config completeness + robustness hardening
7. **VehicleConfig completeness [config].** Add `enablesUIAppearance`/`enablesMapAppearance` (all templates
   set them; zero box change), then `viewAreas`/`safeArea`, then `hidConfig`/`primaryInput` (with #20).
   De-dup the duplicate 4K preset and fix the docs/carplay/04_CAPABILITIES_AND_CONFIG.md↔code default note. **4K@60 is the TARGET, retained
   (user directive):** iOS accepting the 4K `/info` + SETUP is the ground truth; Apple's simulator templates
   are examples, not the protocol ceiling, so the audit's "lower resolution / experimental" recommendation
   is OVERRULED. The project is optimized *for* clean, stable 4K@60 (see "4K@60 optimization track" below).
8. **Derive touch aspect from the decoded frame, not the advertised resolution [window].** Today safe only
   because iOS encodes at the advertised res; decoupling removes a latent touch-misregistration risk.
9. **Make the 8 s A/V-stall path actionable or honest [session].** It shows "Reconnecting…" but does
   nothing; the `OCBMAVDecrypt.reset()` + `resetWatchdog()` primitives exist but are never called. Wire a
   bounded STOP→reset→SUBSCRIBE, or relabel the status.
10. **USB robustness [USB].** Only `ClearPipeStall` on a real `kIOReturnPipeStall` (not on every idle
    timeout); fix the mislabeled "5 consecutive errors" (timeout should reset it) + add retry backoff.

### Tier 3 — legacy retirement + diagnostics
11. **Retire the dead legacy stack [legacy].** `AdapterProtocol`, `MessageSerializer`, `MessageParser`,
    `SessionTokenDecryptor`, `IAP2CallStateDecoder` are wholly unreachable in OCBM mode. Delete them + the
    adapter-only `MessageTypes` members + the dead `adapter?.…` call sites. **Preserve** the shared pieces:
    `USBDeviceID`/`kSupportedDevices`, `DisplayResolution`, the `Data` LE helpers. Keep `IAP2CallStateDecoder`'s
    TLV field maps as reference docs only.
12. **Rewire diagnostics to OCBM [legacy].** `SessionRecorder` + `ProtocolLogger` record NOTHING in OCBM
    mode (they hang off the skipped legacy framing) — "record a session" captures an empty file. Move their
    hooks to the OCBM raw-read/write path; keep the PIN-masking + throttling.
13. **Finish disabling the legacy reinit path [session].** The nav-resolution and fullscreen-screen-native
    paths still call `reinitializeAdapterSession`, which can needlessly tear down a live OCBM session; make
    them window-only like the menu paths. (The transport-lost caller is the one legitimate use — keep it.)
    **CLOSED BY REMOVAL (verified 2026-08-16) — this was REAL when written:** the 2026-07-25 tree still had
    a second call site, `reinitializeAdapterSession(reason: "screen-native resolution")`. Fullscreen support
    was removed 2026-08-02 and took that caller with it, so exactly ONE call site remains —
    `AppDelegate.handleOCBMTransportLost` → `reinitializeAdapterSession(reason: "OCBM transport lost")`, the
    legitimate use this item already exempted. A stale mention survives in a `USBDeviceManager.swift`
    comment — code comment only, no call.

### Tier 4 — deferred / roadmap (verified-justified deviations, not bugs)
- **HEVC decode — DONE (verified 2026-08-16; this bullet also contradicted Tier 0 #2's own note above).**
  `VideoDecoder` carries a full `.hevc` path — VPS/SPS/PPS parameter sets, `createHEVCFormatDescription`,
  HEVC NAL typing `(byte0 >> 1) & 0x3F` — and `OCBMAVBridge` parses the `hvcC` box out of the box's video
  config, falling back to a logged drop when the config is neither `avcC` nor `hvcC`, which is the FourCC
  guard this bullet asked for. Whether a session NEGOTIATES HEVC is app-pushed, not compiled: `enablesHEVC`
  arms the box's two gates per connection (`hevcInfo` in `/info` + `enabledFeatures:["hevc"]`) via
  `levers::set_hevc(vc.accessory_config.enables_hevc)` in carplayd, and the app's stored default is **ON**
  (`SettingsWindow`: `b("enablesHEVC", true)`). Only the app-less / parse-failure path clears the lever to
  off — that, not the app default, is the H.264 fallback this bullet assumed.
- **Real nav / AltVideo / VDC / NMEA GPS** — implementable in *our* box (see correction above); large feature.
- **NACK retransmit** — fuller SDK parity beyond forceKeyFrame-on-gap; needs a bidirectional NACK channel.
- **Audio**: the 24 kHz mic rate shipped (`MicCapture.startCapture(sampleRate:channels:)` takes the
  box-negotiated rate, no hardcode). The audio seam now resets on a box-signalled `F_NEW_SOURCE` frame
  flag and resyncs on `SEAM_MAGIC` so a producer swap can't desync the decrypt counter (2026-09-03; see
  `docs/carplay/01_OCBM_PROTOCOL.md`). Still open: watch long-session A/V clock drift (add a rate-matched
  ring buffer if it appears — not a full jitter buffer). Set decoder color attachments (709/601-4/sRGB)
  as cheap insurance.
- **OCBM cleanups**: the header `seq` is written-but-never-read (dead/footgun) and the "fragmentable"/SOM-EOM
  doc claim overstates v1 — either assert the single-frame contract on the host or implement coalescing.

### Telephony PCM lane + the 8 kHz mic uplink (2026-09-03)
Android Auto call audio never rides the projection link — gearhead leaves it on Bluetooth HFP/SCO — so
the box terminates the SCO link and forwards the call on the EXISTING voice sink (`CH_ALT_AUDIO`) using
a new audio-seam marker, `SEAM_PKT_PLAIN 0x03`: a `SEAM_FORMAT` of *PCM / 8000 Hz / 1 ch / 16-bit /
audio_type 1 (telephony)*, then one `[0x03][scid u64 LE][320 B]` message per 20 ms carrying raw S16LE
verbatim — no key, no RTP, no RFC 2198 (`OCBM/OCBMAVDecrypt.swift` `drainAudio`; the payload is
LITTLE-endian, unlike CarPlay's big-endian PCM, and `OCBMAudioStreamFormat.plainLE` is what stops
`OCBMAVBridge` byte-swapping it into white noise). Playback needs nothing new: 8 kHz mono voice is
already one of the 14 pre-warmed `AudioPlayer` nodes, `audio_type != 0` routes it to `navMixer`, and it
ducks media through the same energy-gated path as a Siri prompt. It logs `telephony PCM 8000 Hz/1ch
scid=… — player armed` once, the first 8 frames as `audio pkt trace … plain len=…`, and one
`telephony rx=<n> frames (<ms> audio)` per second. A plain frame arriving before its `SEAM_FORMAT` is
dropped and counted (`audioPlainNoFormatDrops`) rather than guessed at. **Uplink** is the existing
mic path with no new machinery: the box's gate asks for `uplink on 8000 1`, `MicCapture` keeps
capturing at the hardware rate and lets its `AVAudioConverter` resample (the input node is never asked
for 8 kHz), and the converted PCM is cut into exact **20 ms / 320 B** frames — a carried remainder, not
a padded or truncated one — before `sendMicPCM` puts them on `CH_MIC`. That chunking applies to every
rate, so a CarPlay Siri turn now ships 640 B frames instead of one 100 ms lump. It logs `mic uplink
armed 8000 Hz …` at the gate edge and `mic tx=<n> frames rms=<x>` each second, the RMS being the only
thing that distinguishes a live-but-silent uplink from a muted input device. Both lanes appear on the
`AVmon` line as `tel=<pps>` and `mictx=<pps>`.

#### Wideband (mSBC) on the same lane — 2026-09-04, NOT YET EXERCISED ON HARDWARE
The lane is codec-tagged now: `SEAM_FORMAT` codec `4` (`OCBM.seamCodecMsbc`, `ocbm-proto::SEAM_CODEC_MSBC`)
means the box negotiated HFP **wideband**, and each `SEAM_PKT_PLAIN` then carries one raw transparent-eSCO
read — 2-byte H2 header + 57-byte mSBC frame + pad, 60 B per 7.5 ms — instead of 20 ms of PCM. macOS ships
no SBC codec of any kind, so **the app decodes it itself**: `Audio/MSBCCodec.swift` is a from-the-spec mSBC
encoder + decoder (16 kHz, mono, 15 blocks, 8 subbands, LOUDNESS, bitpool 26, syncword 0xAD) and
`Audio/MSBCFramer.swift` is the eSCO transport around it — H2 resync, reassembly across split reads, and
packet-loss concealment driven by the H2 sequence number (the last good frame faded to zero, then silence).
Neither file imports anything but Foundation, so both run in the hardware-free harness, which is where the
filterbank's prototype table, a 20 dB-plus round trip, CRC rejection, framer resync and a decode check
against an independent fixed-point reference implementation are pinned. Playback is unchanged: 16 kHz mono
voice is already pre-warmed, and the decoded PCM goes through the same `feedPCM` + telephony pre-roll as
the narrowband lane. It logs `telephony mSBC 16000 Hz/1ch scid=… — player armed` and adds `plc=<n>` to the
per-second `telephony rx=` line whenever concealment ran.
**Uplink** follows the same gate, which grew an optional trailing codec byte
(`[state][rate u32 LE][ch][codec]`; the 7-byte form still means PCM and is what an OFF still sends).
On codec 4 `MicCapture` cuts the converted capture into **7.5 ms / 240 B** frames instead of 20 ms ones,
encodes each to a 57-byte mSBC frame, wraps it in an H2 header with the cycling sequence
(0x08/0x38/0xC8/0xF8) plus a pad byte, and sends the whole 60-byte packet as one `CH_MIC` chunk — the box
writes each chunk to the SCO socket verbatim, so a chunk boundary is a packet boundary. The PCM path is
byte-identical to before: the codec byte only ever selects the other branch. `tel=`/`mictx=` keep counting
chunks, which under mSBC is ~133/s per direction rather than 50/s.

### Android Auto in the Metadata window (2026-09-04)

The Metadata window's Media, Navigation and Phone panes are fed by a second source during an AA
session: the phone's MediaPlaybackStatus / NavigationStatus / PhoneStatus services, decoded in
`AA/AAMetadata.swift` and applied through `MetadataStore.applyAndroidAuto` with the same delta
semantics as iAP2. Navigation shows the phone's maneuver type as a glyph (the AA scheme in use sends
no image), the exit cue, step distance and the phone's own ETA string. State is cleared when the AA
session ends. Wire details: `docs/androidauto/01_SESSION_AND_AV.md` §"Metadata services".

### Settings window — projection-aware, vehicle-centric (reorganised 2026-09-04)

Design contract, API reference, rendering rules and test contract:
`host/MacHost/carlink_macOS/App/Settings/DESIGN.md`. This section states the contract; the tab
files under `App/Settings/` are the implementation. It replaces the T1 plan in
`docs/ops/08_FUTURE_TASKS.md`.

**Source of truth is a neutral profile, not either vendor's schema.** `VehicleProfile` +
`AdapterSettings` (`App/Settings/VehicleProfile.swift`, Foundation-only) describe the car and the box
in words that belong to neither vendor: panel geometry, insets, theme, driver position (left / right /
center), an 8-member driving-restriction set, powertrain, input devices, audio and metadata feeds,
plus quarantined `carPlay` / `androidAuto` extension blocks for what only one phone consumes. Two
renderers consume it:

- **CarPlay** — `VehicleConfigModel.yaml`, the Apple-schema document pushed to the box at SUBSCRIBE.
  The emitter's TEXT is unchanged and stays in `App/SettingsWindow.swift` under the string anchors
  `tools/regen_app_yaml_fixture.py` extracts; the fixture drift guard in `tests/run_tests.sh` still
  governs, and no existing configuration emits different bytes (the only new emission is
  `wifi_ap: false`, and only when the access point is switched off — absent still means enabled).
- **Android Auto** — `AACapability(profile:adapter:warn:)` (`AA/AACapability+Profile.swift`), in-process,
  into the protobuf `gal.ServiceDiscoveryResponse`. **Correction (2026-09-04):** until this change the
  AA engine took a six-value snapshot of the CarPlay model (`AACapability.init(config:)`: main
  width/height, max fps, name, `nightMode`, `rightHandDrive`); density, driving restrictions, voice
  rate and the metadata services were constants or `AA_*` environment levers. It now renders from the
  neutral profile: `display.panel` → resolution tier (+ margins when `androidAuto.fitPanelWithMargins`),
  `panel.dpi` → density, `appearance.theme` → the `night_mode` sensor (`auto` = this Mac's appearance,
  resolved at the AppDelegate call site), `driverPosition` → wire 2 / 1 / 3, `restrictions` → the
  `driving_status` mask, `video.hevcAllowed` / `androidAuto.preferHEVC` → codec. Approximations the
  renderer had to make are recorded as `negotiationNotes` and shown in the Vehicle tab. Bench `AA_*`
  variables still override; they are test fixtures, not vehicle facts.

**Tabs** (460×620): **Vehicle** (the neutral profile; sections Identity & branding, Display,
Appearance, Driving, Vehicle, Input, Audio, Data feeds), **Adapter** (wireless radios, hot hand-over,
pairing, Wi-Fi access point, Android Auto projection, app-driven SETUP, then the live box state that was
the CCPA tab), **Diagnostics** (the Box Log stream toggle + cap — the Adapter tab does NOT duplicate
it; DESIGN.md §1 claimed both until the 2026-09-05 correction). Each feature renders as: neutral
control → one badge per projection (supported / limited / unsupported) → per-protocol explanation rows
→ badged sub-groups for protocol-exclusive keys → a value note when the current value has one. Protocol
names appear only in those rows; the save bar and the reboot / NCM alerts are worded neutrally.

**Phase 3 — presentation — is designed but NOT implemented (2026-09-05).** The landed layout above is
dense: the Vehicle tab rests at ~95–105 form rows because every honesty mechanism (badge row, two
explanation rows, provenance glyph, value note) costs permanent vertical space. The contract for the
re-layout is `App/Settings/DESIGN.md` §11: each section becomes collapsible (all collapsed at open),
protocol-exclusive sub-groups become gate-and-reveal, the three separate explanation surfaces merge
into one `FieldPopover` opened from either the (i) or a badge. The window stays NON-resizable and
sizes itself to the current pane, with minimize/maximize dimmed, the title reflecting the visible
pane and the last pane restored on reopen — Apple's Settings guidance, adopted after an intermediate
draft proposed a resizable window and the owner reversed it (DESIGN.md §11.8). The pane switcher
becomes a **window toolbar** using the macOS 27 tabs role (`NSToolbarItemGroup.role = .tabs` /
SwiftUI `.pickerStyle(.tabs)`, both 27.0-only) — `TabView` and `.tabItem` leave the window. The
pinned bottom bars carrying a primary action are retired, since current guidance reserves a bottom
bar for small status information only; **where the Save control lands instead is the one decision
still open** (DESIGN.md §11.9) — the toolbar is occupied by the pane switcher, so it is not
automatically the answer. It is
strictly cosmetic — no binding, no emitted byte, no `FeatureMatrix`/`FieldInfo` string content
changes — and six live-computed warnings are pinned visible rather than moved behind hover.
Deployment target is macOS 27 only, so Phase 3 uses no `if #available` guards; Liquid Glass is
deliberately NOT applied, because Apple reserves it for the control layer and the SDK exposes no
`Form`/`Section` glass API at all.

**Provenance lives on `FeatureMatrix`** (`App/Settings/FeatureMatrix.swift`): 22 features × 2
projections, each with a support level, the vendor's term and key, the effect, and a dated verification
(device-proven / unverified / refuted), plus value-dependent notes. It is also the placement table for
exclusive keys and the two-way restriction mapping (`carPlayLimitedUIElements`,
`androidAutoDrivingStatus`; `typicalDriving` ⇒ AA mask 26, the value `AASession` used to hardcode;
keyboard and keypad share AA bit 2; media/other lists have no AA bit, video/voice/configuration no
CarPlay element — a projection that cannot express a member says so). **Verification state as of
2026-09-04** (corrected the same day — an earlier draft of this paragraph listed AA tier 5, the
portrait tiers and both CarPlay metadata tiers as unverified, and that stale claim was encoded into
`FeatureMatrix` before it was caught): **all nine AA codec tiers are device-verified** (Pixel 10 /
gearhead 17.5, wireless, one at a time — `docs/androidauto/01_SESSION_AND_AV.md`), but the evidence is
per **(tier, fps)**: tiers 1/2/3/6 ran at 60 fps, tiers 4 and 5 at both 30 and 60, tiers 7/8/9 at 30
only — so 3840×2160 is proven at 30 and 60 while a pairing like 1080×1920@60 has never run.
2560×1440 declared as H.264 is refuted (the phone answers "not allowed for the codec type").
**CarPlay metadata tiers:** `extended` is device-proven on the AirPlayTunnel arm (twice: 2026-07-25 at
340 B and 2026-08-10 at 342 B Identify, `0x1D02` accepted — `docs/carplay/05_METADATA_AND_CONTROLS.md`
§5.3/§6.6) and NOT proven on the wired arm, where no `extended` Identify has been run; the only
`extended` rejection on record (§6.1, an earlier form without the Stop ids) was on the tunnel arm and
is why the Stop fields exist. `all` is **REFUTED** on the tunnel arm (device evidence 2026-08-10, §
the tier-`all` box: iOS named the `voice_over_cursor` ids, and skipping them still drew a generic
param-6 reject). This matters because a `0x1D03` identification reject is unrecoverable within a
session — params 6/7 are `REQUIRED_IDENT_PARAMS`, the retry is byte-identical, and CarPlay stays dead
until the phone is unplugged and replugged — so `all` must never be the default. **Still unverified on a
device:** driver position CENTER (wire 3) and AA voice at 24 kHz. AA insets, status-bar policy and
powertrain are recorded in the profile but NOT sent.

**Two export artifacts, deliberately not merged.** Vehicle tab ▸ *Generated YAML* disclosure +
*Export YAML…* (CarPlay-badged) writes the RENDERED Apple-schema document — the bytes the box receives.
Vehicle tab ▸ *Profile document* (Import… / Export… / Presets) reads and writes the NEUTRAL
`VehicleProfileDocument`: JSON with a YAML-shaped key layout, `*.vehicleprofile.json`, Foundation's
coder with sorted keys and pretty-printing, so equal documents are byte-equal and re-encoding is a
no-op. It carries `schemaVersion` (1) with a per-version migration ladder; a newer schema is refused
rather than guessed at; a missing key at ANY depth takes that field's
default (corrected 2026-09-04 — decoding was all-or-nothing below the top level, which meant one new
nested field would have refused every profile a user had already exported). Only fields declared
without a default stay required: `BrandIcon.pngBase64/width/height` and `ConnectorSpec.type`. It is JSON, not a hand-rolled YAML subset, because docs/carplay/04 B3 already lost a
pushed document to one unescaped quote. Sixteen presets ship: the fixture-locked default, ten derived
from Google's DHU `config/*.ini` (`all_720p` / `loaded_720p` are one entry — not because the files are
identical, as this line said until 2026-09-04, but because their only difference is `loaded_720p`'s
`[sensors]` block, which is a runtime feed rather than a profile fact; `docs/ops/03_REFERENCE_INDEX.md` §F),
five from Apple's CarPlay Simulator templates. `dhu-6in` (750×450) is below the app's 800×480 floor
and is clamped on load with a notice rather than silently loading a different geometry. Note the clamp
is NOT CarPlay-only: `clampInPlace()` runs on the model before EITHER renderer sees it, so Android Auto
gets 800×480 too and only the 6.0" diagonal survives the load. Google's own `default_6in.ini` expresses
that panel as tier 800×480 with `marginwidth 50` / `marginheight 30` (visible 750×450, pixel-exact);
this app declares the whole tier and scales ×0.94 to the panel, which the AA renderer now states in a
negotiation note.

**Persistence is unchanged:** UserDefaults `vc.*`, the observable `VehicleConfigModel`. New neutral
keys (`driverPosition`, `theme`, `dpi`, `diagonalInches`, status-bar and AA-only restriction flags,
`wifiAccessPoint`, voice rate, telephony-over-projection, the three metadata feeds, the two AA
extension flags) default sanely when absent. A one-shot `vc.profileKeysV1` migration seeds
`driverPosition` / `theme` from the legacy `rightHandDrive` / `nightMode` booleans exactly once; those
two keep being written to UserDefaults `vc.*` as DERIVED values so a downgrade still reads something
sane — UserDefaults only for `nightMode`: the pushed YAML has carried neither key since 2026-09-02
(`crates/vendor/receiver/src/vehicle_config.rs`, the `EMITTED_BUT_UNREAD` comment). **2026-09-05:
`rightHandDrive` returns to the pushed YAML**, derived from `driverPosition == right`, because it is a
CarPlay `/info` boolean after all (R14G17 `AirPlayCommon.h:1103`); the box now parses it and emits
`/info rightHandDrive` — unverified on a device (docs/carplay/04_CAPABILITIES_AND_CONFIG.md
§rightHandDrive). Beyond that, nothing new is pushed to the box except `wifi_ap: false`.

**Vendor reference files.** Apple's CarPlay Simulator ships `VehicleConfigs/Configs/*.yaml`; Google's
Desktop Head Unit ships the same kind of thing as `config/*.ini` (see `docs/ops/03_REFERENCE_INDEX.md`
§F). Neither is a wire format: CarPlay's goes out as the AirPlay `/info` plist plus iAP2 Identify
parameters, Android Auto's as `gal.ServiceDiscoveryResponse`.

**Tests.** `host/MacHost/tests/SettingsTests.swift` (`runSettingsTests()`, wired into
`run_tests.sh` / `main.swift` by the integrator) covers the document round-trip and determinism, all
presets, schema tolerance, the on-disk read/write path, the restriction mapping in both directions,
`FeatureMatrix` completeness, and the neutral → Android Auto rendering. The `vc.profileKeysV1`
migration is NOT covered there: `VehicleConfigModel.migrateProfileKeysV1(_:)` takes the defaults
domain as a parameter for exactly that purpose, but it lives in `SettingsWindow.swift`, which the
harness cannot compile (AppKit / SwiftUI / `@MainActor`, and it drags in the decoder stack). The test
body exists behind `-D SETTINGS_TESTS_HAVE_MODEL` and prints SKIP otherwise; moving the migration into a
Foundation-only file (`App/VehicleConfig.swift` exists for this) makes it live.

### Android Auto in Settings ▸ stream performance (2026-09-03)
`StreamPerfSection` reads the OCBM decrypt layer's accumulators, which AA traffic never reaches (it
rides `CH_IP` → `AASession`), so an entire AA drive rendered four all-zero CarPlay rows. `AASession`
now publishes its own 1 Hz counters as an `AAStatsSnapshot` value (`OCBM/StreamMetrics.swift`, beside
the CarPlay rate math so the harness can test it) through a `Mutex` box, and the monitor's existing
1 Hz sampler converts it with `AARates.between` — per-second rates, never raw totals, with the
transport labelled from the box's projection mode. The rows hide when no AA session is publishing and
age out after 5 s if its loop stalls; `/tmp/carlink_metrics.json` gains a matching `aa` object (null
when no AA session), including the box-side `telephonyRxPerSec` / `micUplinkPerSec` from the lane
above.

### 4K@60 optimization track (primary goal — user directive: retain 4K@60, deliver clean stable video)
iOS negotiated 3840×2160 @ maxFPS 60 and the box is NOT CPU-bound (load ≈0.9). So clean 4K is a
transport-efficiency + recovery-correctness problem, not a resolution problem. Ordered path:
1. **Frame-cap fix — DONE (host).** Raise the reassembled video-message limit to 16 MB so 4K IDRs are no
   longer rejected. This alone should let keyframes land and clear the constant poisoning.
2. **Backpressure, not drop — BUILT (ocbmd), staged.** Gate the video read on the out-queue draining so a
   slow pipe throttles the iPhone's encoder (Apple flow control) instead of dropping P-frames. Fewer/zero
   drops → continuous seq → no poisoning.
3. **Renderer keyframe recovery — DONE (host).** `decoder.onNeedsKeyFrame → requestKeyframe()`.
4. **Forward-path efficiency (next).** Cut the `carplayd → :9001 → ocbmd` local-TCP-loopback copies (unix
   socket / splice) so the box sustains 4K@60 bitrate headroom and backpressure rarely engages. The stock
   firmware forwarded 4K@60 with no drops on this hardware — the target is parity.
5. **HEVC (later, halves bitrate).** Once H.264 4K@60 is clean, HEVC 4K@60 is easier (lower bandwidth); the
   3 gates (publish hevcInfo, accept hevc at SETUP, decode hvc1) become the follow-on.

### Host decode pipeline — bounded FIFOs, off the main thread (2026-09-03)
`Video/VideoDecoder.swift` moves each frame across two hand-offs: USB read queue → `decodeQueue`
(parse + sample-buffer build), then `decodeQueue` → `renderQueue` (`Receiver.enqueueImmediately`). Both
hand-offs are `AVCCFastPath.FrameFIFO`, a bounded FIFO whose depth defaults to **3** (the incoming frame
plus a two-frame consumer cushion); before this pass both were depth-1 latest-wins slots, and the
render hand-off ran on the main thread. Its overflow policy is the point:
- A frame is **protected** if it is an IDR *or* the frame immediately following an IDR, decided in
  stream order at push time and carried with the frame (an eviction never re-labels its neighbours).
- Overflow sheds the **oldest unprotected P**. If a keyframe already sits behind the hole — or the
  incoming frame is one — the chain repairs itself within `depth` frames and **no** keyframe is
  requested, which is why the fix also cuts the keyframe-request churn.
- If every queued frame is protected and the newcomer is a P, the **newcomer** is dropped and a keyframe
  requested — exactly the old "P over IDR" rule.
- If every queued frame is protected and the newcomer is an **IDR**, the oldest frame is evicted anyway.
  Refusing an incoming IDR would orphan every P that references it (~2 s of poison); evicting one queued
  frame costs at most the frames behind it, which that IDR repairs almost immediately.

The **producer is never blocked** on either hop — a full FIFO always resolves to a drop, so a stalled
consumer still cannot back-pressure the USB read path. The live-UI rule ("drop on backpressure, never
buffer") is intact; only the *choice of victim* changed. The depth knob survives: `maxDecodeDepth` /
`maxEnqueueDepth`, with AA still raising the decode hop to 64 to absorb VideoToolbox's one-time warm-up
burst, and `1` reproducing the old table exactly (the harness asserts that against the retained
`AVCCFastPath.resolveSlot` oracle rather than a copied table).

**The render hand-off (`drainEnqueue`) runs on `renderQueue`, a dedicated serial `.userInteractive`
queue — nothing on the frame path touches main any more** (the one remaining
`DispatchQueue.main.async` in the file is the `onDimensions` window-sizing callback, which is genuinely
UI). This is API-legal, not a liberty: in the macOS 27 AVFoundation swiftinterface
`AVSampleBufferVideoRenderer.Receiver` is neither `Sendable` nor `@MainActor` — `sampleBufferReceiver(adding:)`
returns it `sending`, i.e. a single-owner object transferred into one isolation domain, and Apple's own
usage example enqueues from a client-chosen serial queue, never main. `receiver` is built on the main
actor in `init` (the transfer point) and afterwards is touched only from `renderQueue`
(`enqueueImmediately` plus both `flush()` call sites); `synchronizer` and `displayLayer` stay
main-confined and are never read after init. `flush()` also gets stronger ordering from this: it is now
decodeQueue → renderQueue, a plain serial-queue guarantee, rather than racing the main thread's own UI
work.

**Latency is measured, not asserted**, because a queue trades frame loss for delay:
`VideoDecoder.wrapLatencyMs` (arrival → `CMSampleBuffer` built) and `.handoffLatencyMs` (arrival →
handed to the renderer), EWMA α=1/8, printed as `wraplat=<main>/<alt>ms` (`wrap>handoff`) in the `AVmon`
line and as per-stream `wrapLatencyMs`/`handoffLatencyMs` in `/tmp/carlink_metrics.json`. These names
replaced a first-session `decodeLatencyMs`/`declat` that measured the same zero-copy buffer wrap
(~0.1 ms) but implied VideoToolbox decode time — `Receiver` exposes no `VTDecompressionSession` and no
per-frame decode completion (its only feedback is `didFailToDecode`/`requiresFlushToResumeDecoding`/
`failed`), so the top-level JSON `decodeLatencyMs` key stays `null`, reserved for a build that routes
frames through an explicit decompression session. Read `handoffLatencyMs` beside `dropFps`: the fix is
working when `dropFps`≈0 and `handoffLatencyMs` stays inside one frame interval — if it climbs, the
depth-3 cushion is buying latency instead of buying frames.

**Status: the bounded-FIFO change (both hops) was measured on device** — drops collapsed from a
sustained trickle to a start-of-session-only burst (30 `evict-oldest-P`, all on the ENQUEUE hand-off, in
the first 25 s, then none), which is what motivated moving that hand-off off the main thread. **The
off-main-thread `renderQueue` change is built and unit-tested (53 harness cases) but UNMEASURED ON
DEVICE.** The next relaunch must confirm the start-of-session enqueue-queue burst is gone, `dropFps`≈0
in steady state, and `handoffLatencyMs` stays inside one frame interval.

### Bench control surface — `ControlServer` (`CARLINK_CTRL_PORT`), 2026-09-07

`App/ControlServer.swift` is a localhost line protocol that drives the app's INTENTS programmatically —
off unless `CARLINK_CTRL_PORT` is set, bound to `127.0.0.1` only, one command per line, one reply line
per command (`printf 'get session\n' | nc 127.0.0.1 $CARLINK_CTRL_PORT`). Three design rules, each
the answer to a bug that cost a session: every actuation goes through `ControlsBridge` exactly like
the UI buttons (a side path to the transport would hide the routing bug under test); `set` is an
explicit allowlist that writes the same `VehicleConfigModel` fields the form writes (a generic
writer could push a YAML the emitter has never seen); every read is one-line JSON and side-effect-free.

| Verb | Reply | Notes |
|---|---|---|
| `key <home\|back\|select\|up\|down\|left\|right\|play\|pause\|playpause\|next\|prev\|answer\|end\|assistant>` | prose | D-Pad / media / telephony panels |
| `knob <select\|home\|back\|cw\|ccw\|up\|down\|left\|right>` | prose | the knob panel — a separate call-site set from `key` |
| `tap <x 0-10000> <y 0-10000>` | prose | down+up through the view's real touch path |
| `dark\|night\|limitedui <on\|off>`, `siri`, `status` | prose | |
| `get session\|box\|av\|profile\|aa\|viewarea\|presets` | JSON | read-only |
| `preset <id>`, `set <key> <value>`, `save` | JSON | `save` commits; it lands at the next SUBSCRIBE. **Clamps are reported, not silent** — see below |
| `viewarea arm <WxH@X,Y>` / `viewarea off` (alias `viewarea arm off`) | JSON | arm the second main view area through the model |
| `viewarea request <index>` | JSON | command a view-area transition |
| `shot [path]` | JSON | PNG of the current decoded main-lane frame |

**`shot [path]`** — `{"ok":true,"path":..,"width":..,"height":..,"frameAgeMs":..,"sequence":..,
"decodeFailures":..}`, default path `/tmp/vashots/shot-<epoch>.png`; `{"ok":false,"path":..,"reason":..}`
before the first IDR. Why it exists: over WIRED CarPlay the phone's port is on the box, so there is no
phone log on this Mac, and iOS renders its view-area lockout banner ("CarPlay does not support this
display resolution") INTO the video stream and reports it in no log the accessory can read — the
pixels are the only verdict. The frame is NOT a screen capture: `Video/VideoDecoder.swift` renders
through `AVSampleBufferDisplayLayer`, whose renderer decodes internally and exposes no pixel buffer
(`copyDisplayedPixelBuffer()` is specified to return NULL while the synchronizer rate is non-zero,
and ours runs at 1.0 all session). So `FrameTap` (same file) takes the exact `CMSampleBuffer` the
renderer ACCEPTED (`drainEnqueue`, after `.enqueued`) and decodes it a second time through an explicit
`VTDecompressionSession`, keeping only the newest BGRA picture; `shot` writes it as sRGB RGBA8 PNG via
CoreImage. Identical bytes, no permission, nothing on the render path changed; the cost is a second
hardware decode, so the tap is installed only on the CarPlay `main` lane and only when the control
server is on. Read `frameAgeMs` before trusting a shot: a number that grows between shots means the
stream stopped and the PNG is the last thing iOS sent. `shot` runs on the socket thread, not the main
queue, so a 4K PNG encode does not stall the UI. **Unverified on device** (2026-09-07): built and
compiled; the first live session must confirm the shadow session decodes HEVC and H.264 and that
`frameAgeMs` tracks the live stream.

**A clamp is never silent (2026-09-07).** `set width/height/fps/dpi` answer with `stored` (what the
model now holds, read back) and `pendingClamp` (a DRY RUN of `clampInPlace`, i.e. what `save` will
actually do — the value is stored as given so `set width 480; set height 800` is order-independent);
`save` answers with `clamped` and the resulting `panel`; `get viewarea` carries `clampNotes` beside
the `panel` it verdicts against. **A caller that writes a value and does not read it back has no way
to know it was rewritten, and that cost a whole test pass**: the panel envelope used to be per-axis,
so `set height 3840` was silently clamped to 2160, every portrait view area then failed containment,
and five sweep cases scored INCONCLUSIVE while appearing to run. The envelope is now `PanelRule`
(orientation-agnostic, floor by the panel's own aspect) — see ../carplay/04_CAPABILITIES_AND_CONFIG.md.

**`viewarea arm <WxH@X,Y>`** writes `viewArea2Enabled/X/Y/W/H` — the same fields the Settings form
writes — and answers `{"ok":true,"armed":{"enabled","rect":{x,y,width,height},"spec","panel":{width,
height},"verdict":null|string,"legal":bool,"floor":{width,height},"active":bool},"dirty":bool,
"note":"not pushed until 'save'"}`. `verdict` is `ViewArea2Rule.verdict` — nil = legal, else the form's
own message in severity order (containment, parity, positivity: teardowns; product floor: lockout).
An illegal rect IS written (so `get viewarea` shows what was asked) but `active` stays false and the
emitter leaves the YAML byte-identical, exactly as the form behaves. `viewarea off` clears the enable
flag. Nothing is pushed until `save`; the rect lands at the next SUBSCRIBE. The spec grammar is the
bench tools' `WxH@X,Y` (`ViewAreaSpec` in `App/VehicleConfig.swift`, harness-tested); `:initial` is
refused because the app model does not author it. `get viewarea` now carries the same `armed` block
beside `observed` (what iOS reports) and `pushed` (what the last SUBSCRIBE carried) — `armed.active`
true while `pushed` lacks it means "save and reconnect".

**`viewarea request <index>`** — `{"ok":true,"index":n,"wire":..,"note":..}` or `{"ok":false,
"index":n,"reason":..}` (Android Auto owns the box, or no session). Path: `ControlsBridge.
requestViewArea` (intent table entry `.viewArea`, CarPlay-only, outcome in `lastSent`) →
`OCBMClient.sendViewArea` → `[INPUT_COMMAND 0x04][CMD_VIEW_AREA 0x11][index]` on `CH_INPUT` → ocbmd
relays opaquely → carplayd `handle_input_frame` → `receiver::events::switch_view_area(DISPLAY_UUID,
idx, "host viewArea")`, the SAME function that answers the phone's own `requestViewArea`, so the two
paths share one policy: an index `/info` never declared is refused box-side (`[events] host viewArea
index=N REFUSED — only M area(s) declared`), else `updateViewArea{uuid, viewAreaIndex,
animationDurationMillis: 3000, adjacentViewAreas}` goes out. `ok:true` means accepted for send, not
that iOS moved — confirm with `get viewarea` (`changes`, observed rect) and `shot`. Device-proven
2026-09-05: iOS's own request is advisory and the accessory is the authority, which is why commanding
the answer directly is a legitimate transition, not a hack. **Requires the 2026-09-07 carplayd on the
box** (`build.sh`'s carplayd stanza → `target/armv7-unknown-linux-musleabihf/release/carplayd`, push
with `ocbm-host`); an older carplayd logs `unknown INPUT_COMMAND 0x11 — dropped` and the fallback is
`tap` on the Dock resize button. ocbmd needs no change. **Unverified on device** as of 2026-09-07.

`tools/va_wired_sweep.sh` (docs/ops/02_TESTING.md) is the consumer these three were built for.

### Cross-cutting theme
The unifying story is **finish the OCBM migration**: the CarPlay path is correct where it was ported
(crypto, single-touch, config, lifecycle) and simply absent where it wasn't (commands, multi-touch,
diagnostics). Tiers 1 + 3 are that completion; Tier 0 is the one real bug plus two robustness fixes; the
rest is config growth and roadmap.
