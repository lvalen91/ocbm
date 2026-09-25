# CarlinkAndroid — the AAOS host app (OCBM)

The head-unit counterpart to `host/MacHost/`: an Android app for a GM **gminfo3.7** unit (2024
Silverado, Intel, AAOS 12L / API 32) that claims the CPC200-CCPA over **OCBM** and runs **full**
wireless CarPlay — the adapter's own WiFi and Bluetooth, the adapter as the AirPlay endpoint, all
media crossing the USB bulk pipe.

> Not to be confused with the sibling `gm_ccpa` project, which offloads WiFi to the *vehicle's* SoftAP
> and reduces the adapter to BT + MFi. That is a different architecture. This one uses the full OCBM
> stack. `gm_ccpa` is still where several of the components below were proven on this hardware.

## WHERE THINGS STAND (updated 2026-09-18, end of session)

> **Status note, 2026-09-18 — active, three work packages landed today.** This replaces a
> 2026-09-11 note (removed) that called the app "dormant, and behind" and pointed readers at
> `gm_ccpa`/`host/MacHost/` instead — that was true on 2026-08-16 and is not true now. Today's
> build gate is green
> (`./gradlew :app:assembleDebug :app:testDebugUnitTest :app:detekt :app:ktlintCheck`,
> `python3 tools/proto_check.py`), and three things landed in `:app`: the 3P native shell (WP1, see
> below, box-verified on the emulator and a live box, no phone), phone-call audio (WP2, JVM-tested only
> — no phone call yet), and a resolution/orientation-agnostic UI redesign (device-measured on two AVDs
> and a live box, including two real iPhone CarPlay sessions). Nothing is committed yet.
> **2026-09-25:** that dashboard UI was replaced by a port of the `carlink_native` screens — see
> "UI — carlink_native screens on the OCBM stack" below.
> `crates/ocbm-proto` remains canonical; `OcbmProto.kt` remains an app-owned fork of it since
> 2026-09-11 (`gm_ccpa` no longer symlinks it) — that relationship did not change today. `CMD_VIEW_AREA`
> (0x11) was added to `OcbmProto.kt` today, closing one of `:app`'s two `proto_check.py` gaps (the
> other, `F_BOTH` not being in `ocbm-proto`, is unrelated and still open). Do not re-share this file
> into `gm_ccpa`.

**The adapter is currently in `ncm_only` mode.** `/script/ncm_only` exists, so `start_main_service.sh`
skips `ocbm_boot.sh` and the box comes up as a USB-NCM network device instead of the OCBM accessory.

- Shell: `python3 tools/boxsh.py run '<cmd>'` (telnet to 192.168.50.2; the Mac gets a DHCP lease on
  192.168.50.x). Verified working.
- **The OCBM link and the Android app will NOT work in this mode.** To go back:
  `python3 tools/boxsh.py run 'rm -f /script/ncm_only; sync; reboot'`, wait ~50 s, then the adapter
  re-enumerates as 1314:2d00 and `target/release/ocbm-host` / the app work again.
- The USB stick is NOT mounted in this mode (`/mnt/UPAN` is an empty directory, no `/dev/sda*`), so
  the backups written there are present on the stick but unreachable until OCBM mode returns.

**Box binaries deployed this session** (all built from this tree, all UNPACKED — the shipped ones are
UPX-packed and about half the size; do NOT pack with the host toolchain, see the deploy section):

| Path | Size | Carries |
|---|---|---|
| `/usr/sbin/ocbmd` | 453200 | host instance nonce, `CT_PHONE_IDENT` mirror, silent presence re-arm, forget clears the AirPlay peer store |
| `/usr/sbin/airplayd` | 1793056 | two-finger HID descriptor + contact coalescing, phase-1 SETUP identity publisher |
| `/usr/sbin/btd` | 495976 | independent `/tmp/setup_dump` gate |
| `/script/run_ocbmd.sh` | — | opt-in deploy dead-man, armed with `/script/ocbm_deadman_on` (currently DISARMED) |

Backups: `/mnt/UPAN/ccpa_backups/` on the box's USB stick, plus local copies of `bt_link_keys` and
`carplay_peers.bin` in this session's scratchpad (both restored to the box after testing, so the
phone's pairing is intact).

**Next up:** Phases 2 (UI) and 3 (box-side preferred device) of **`KNOWN_DEVICES_PLAN.md`**, which is
a complete, evidence-backed spec — read it before starting either. Phase 1 (persistent history) and
the Remove semantics are landed and hardware-verified.

## UI — carlink_native screens on the OCBM stack (2026-09-25, emulator-measured)

The in-screen dashboard (frosted-glass cards, `FrostedGlass.kt`) is gone. The UI is now the
`carlink_native` structure, restyled 1:1 from its screenshots, on top of the unchanged
`CarlinkManager` / OCBM / USB / display-detection code:

- **`ui/MainScreen.kt`** — the projection surface (laid out to `DisplayProfile.surfaceInsets` exactly
  as before; `handleTouchEvent` byte-identical) and, while not STREAMING, the loading overlay: logo,
  spinner, `[ status ]`, the CT_PROJ_MODE/CT_BOX_HEALTH line, and top-left **Settings** / **Reset
  Device** (= `MainActivity.reinitialize()`, the full session rebuild).
- **`ui/SettingsScreen.kt`** — slides in OVER `MainScreen` (`CarlinkApp` in `MainActivity.kt`, so the
  SurfaceView survives). Navigation rail: back, **Phones** / **Control** tabs, close-app (confirm →
  `stop()` + `finishAffinity`), version. Reached from the overlay's Settings button or, mid-session,
  from the CarPlay OEM "Carlink" tile (`requestUI` → `onHostUIPressed`). Closing it calls
  `recoverVideoFromOverlay()`.
- **Phones** (`ui/settings/PhonesTab.kt`) — the known-device grid (`AdaptiveGrid`, column count follows
  the pane width). A history record the box no longer holds a link key for (`DeviceInfo.bonded ==
  false`) now reads "Not paired with adapter" and is not tappable — it used to render identically to
  a bonded one.
- **Control** (`ui/settings/ControlTab.kt`) — "Adapter" card: box status, Disconnect Phone
  (`disconnectPhone()`), Reboot Adapter (confirm → `rebootAdapter()`), Disconnect Adapter (`stop()`).
  "App Control" card: Display Mode (`DisplayModeDialog`, live bar preview, Apply → persisted +
  session rebuild via `MainActivity.onDisplayModeSelected`), Reset Decoder (`resetVideoDecoder()`), Reset Connection
  (`restart()` — the in-manager restart, distinct from Reset Device). Two-pane on an expanded
  landscape window, stacked otherwise (`DashboardLayout.arrangement`). The in-app Siri button and
  the assistant-picker button were removed on 2026-09-25 (owner request); `requestSiri()` and the
  `voice/` services stay, reachable through the wheel key / assistant tiers below.
- **Dropped, no OCBM backing:** the Adapter Configuration dialog (audio source, mic source, media
  delay, video resolution, FPS, drive side, GPS forwarding, cluster navigation, WiFi band — all
  riddleBox `BoxSettings`/`AdapterConfig` knobs; the OCBM path pushes `VehicleConfigYaml` from the
  detected display and hardcodes the rest), Reset Cluster (no cluster in this build), and the Logs
  tab (`Logger` has no file sink or level switch — `adb logcat` only).

No protocol or state-machine change was needed; `MainScreen` gained one plain callback
(`onStateChanged`) so the overlay can gate its buttons on the connection state without polling, and
`CarlinkManager.videoFramesRendered()` (a read of the epoch's frame counter) so the loading overlay
lifts on the FIRST RENDERED FRAME — carlink_native's STREAMING edge — instead of waiting for the
first media metadata, which is where OCBM stamps STREAMING (`markStreamingIfFirstMedia`). Without
it the "[ Phone connected ]" spinner sat over live video until something played.

**OEM tile → Settings → back, end to end (2026-09-25, box-measured).** carlink_native's wiring is
`CommandMessage(REQUEST_HOST_UI=3)` → `onHostUIPressed` → open Settings directly; on close
`recoverVideoFromOverlay()` = codec flush + ONE keyframe request (`CommandMapping.FRAME`, 0x0C). No
take-screen / changeModes command exists in the old protocol either. The OCBM port is the same
shape: inbound `{type:"requestUI"}` rides `META_CMD` → `onCommandPlist` → `onHostUIPressed` →
Settings; back → `recoverVideoFromOverlay()` = `INPUT_KEYFRAME` (+ the 750 ms watchdog that
rebuilds the decoder only if the counter does not move). Measured: iOS does NOT stop the main video
stream when the tile is pressed — the frame counter kept advancing under the overlay whenever the
CarPlay screen had motion (947→977 over a 9 s overlay on the app grid; 220→220 only because the grid
is static) and resumed at once after the keyframe (no watchdog rebuild in 6 loops). The accessory
→ controller `requestUI` (`CMD_REQUEST_UI` 0x01, box-handled at `carplayd main.rs:1169`) and the
`changeModes` Take that the box sends itself at RECORD (`events::send_take_screen`) are the only
screen-focus verbs; neither is needed for this return path and neither is sent by the app. There is
no Android Auto equivalent in this build (CarPlay-only; `PM_WIRED_AA` is a box mode, not an app
path).

**OEM-tile tap reliability.** When the app grid has been sitting on screen since the session
(re)started, the FIRST TWO `adb shell input tap`s on the tile are silently ignored and the third
produces the `requestUI` (META arrives 18-25 ms after that tap's DOWN); this held at 3 s and 12 s
spacing, with a 120 ms press, on the ported build AND on the pre-port baseline (2/2), so it is not
the port. Once CarPlay has navigated away and back to the grid (dashboard → grid) the first tap
fires (4/4 in the final loops). Regular icons (Music, Podcasts, Now Playing) respond to
the first tap, and the box relays every inbound `/command` unconditionally
(`session.rs` `fn command` → `emit_command_plist`), so the drop is upstream of `META_CMD` — the
iPhone or the box's `[command] ← iPhone` log would settle it (`/tmp/carplay_cmd_capture.bin` on the
box records every inbound plist; pull it next time the box is in shell mode). Measured loops, all
green: 6/6 tile → Settings → back on the ported build (frames counter and the Now Playing progress
bar advance after every back; no watchdog rebuild), back with no session → main page. One wart seen
there, pre-existing: after Disconnect Adapter the status line still reads the last text ("Phone
connected") because `stop()` does not reset it.
Measured on the `ultrawide` AVD with the live box and a wired iPhone: session → STREAMING, touch
reaches iOS, Settings over the live video, Display Mode apply (2400x960 → 2400x788 rebuild),
portrait via `wm size 800x1280` (800x1108 session, stacked Control tab). One pre-existing quirk
stands out more now that the overlay is opaque: STREAMING is stamped by the first media metadata
(`markStreamingIfFirstMedia`), so with the phone's player paused at connect the loading overlay
stays over a decoding video until something plays — unchanged from the dashboard build.

## Settings → OCBM action mappings (2026-09-25, chevy12 emulator + live wired CarPlay)

Owner-confirmed intent: **Reset Connection and Reset Device are deliberate CLEAN-SLATE resets** (end
the phone session, close the app↔box link, let the box run its own session-end lifecycle, wait,
reclaim, start a NEW session); **Disconnect Phone must end the phone's session on wired AND
wireless**. Every control now sends what its label says, or is disabled with the reason. Box-side
TODOs are in [docs/ops/04_OPEN_ITEMS.md](../../docs/ops/04_OPEN_ITEMS.md) ("Host-app controls
waiting on box verbs") and the proposed verbs in
[docs/carplay/01_OCBM_PROTOCOL.md](../../docs/carplay/01_OCBM_PROTOCOL.md) ("Proposed MGMT verbs").

| Control | What it sends now | Evidence |
|---|---|---|
| Reset Connection (Settings ▸ Control) and Reset Device (loading overlay) | ONE path, `CarlinkManager.cleanSlateReset()` → `CleanSlateReset`: confirm → CT_STOP → close OCBM → release the USB interface (IO thread) → wait **6 s** → reclaim → HELLO with a ROTATED instance nonce → SUBSCRIBE. Status line: "Resetting…" / "Waiting for adapter…" / "Reconnecting…". `restart()` and the `reinitialize()` route are gone for these buttons | timeline below; `CleanSlateResetTest` |
| Disconnect Phone | **Nothing.** Disabled with "Requires adapter firmware support". Gate: `OcbmClient.supportsPhoneDisconnect` (false: OCBM has no verb and no capability bit). It used to send `MGMT_RESTART_WIRELESS` in disguise. Shown disabled rather than hidden so the driver sees WHY; enabling it later is the one-line cap check | screenshot `03_control_tab` |
| Restart Wireless (new) | `MGMT_RESTART_WIRELESS`, confirmed; the text says a wired phone is not affected | live: ACK status=0, no SESSION_EVENT / PROJ_MODE / BOX_HEALTH change, wired CarPlay stayed up |
| Reboot Adapter | `MGMT_REBOOT`, confirmed; dialog now says ~50 s and warns about the USB permission prompt after re-enumeration | dialog screenshot |
| Disconnect Adapter | `CarlinkManager.disconnect()` = `stopImpl` under the lifecycle mutex on IO, now confirmed; the stop path resets the status line to "Disconnected" | dialog screenshot |
| Close App | the same `disconnect()`; `finishAffinity()` runs after it returns, so onDestroy's synchronous `release()` never overlaps a teardown in flight | code |
| Phones cards | information-only (no tap; there is no targeted-connect verb, and the tap used to bounce wireless). Remove: bonded → `MGMT_FORGET_DEVICE`; **unbonded → local history delete, nothing sent** (the box holds no bond and its FORGET handler restarts wireless unconditionally) | screenshot `02_remove_unbonded_dialog` |
| Display Mode ▸ Apply & Restart | MacHost's rule (`OCBMClient.swift` `sessionConfigChanged`): a phone session owns the box (`CT_PROJ_MODE` wired/wireless CP) → persist, "Display mode saved — applies at the next connection" dialog, window re-asserted to the APPLIED mode (the dialog's live preview would otherwise leave bars under a session built without them); applied by a `BoxStatusListener` when the box reports no phone session — skipped while a clean-slate reset is in flight. No phone session → rebuild now, as before | live: log `phone session active (WIRED_CP) — mode saved`; no `[REINIT]`; screenshot `09_display_mode_deferred_notice` |
| Back from Settings | keyframe + 750 ms watchdog gated on a live video epoch, not `state == STREAMING` (OCBM stamps STREAMING on first media metadata, so wired sessions sit in DEVICE_CONNECTED with video) | live: `Recovering video after overlay close (state=DEVICE_CONNECTED)` |
| Wireless pairing code | Pair / Cancel dialog on the main screen → `CT_PAIR_CONFIRM` 1/0 (`OcbmClient.sendPairConfirm`); one answer per prompt, box cancels on its own after 55 s | `OcbmClientTest` pins the bytes; NOT hardware-exercised (no wireless pairing today) |

Also in this pass: the stale `STOP_GRACE` comments went (CT_STOP is an immediate `go_idle` since
2026-09-03); `handleErrorImpl` now really skips CT_STOP (`OcbmClient.stop(sendStop = false)`) — every
path into it is a dead pipe; and the empty `CT_PAIRING_CODE` the box replays on every SUBSCRIBE no
longer paints "Pairing..." over a wired session's CONNECTING overlay (it did, measured 03:47).

### The wait is 6 s, derived, not guessed

There is NO box-reported "teardown finished" the app can wait on: after CT_STOP ocbmd's `go_idle`
clears `subscribed`, and every box→host mirror (`proj_mode_tick`, `phone_tick`, `bt_phase_tick`)
returns early on `!subscribed`. So the wait is a timer, derived from the supervisor's own teardown
(`tools/session_supervisor.sh`): 1 Hz presence poll (`:1461`) + its own 4 s "past teardown
settle" deferral before wireless comes back up (`:1190`; the same one Restart-wireless rides, `:905`)
+ 1 s margin. The serialized wired teardown — `teardown` (`:479`) → `kill_session` (`:291`):
SIGTERM carplayd, `sleep 1` (`:302`), SIGKILL (`:303`), `release_carplay_owner` (`:304`) — completes
≤ 2 s after CT_STOP; the detached radio teardown (`:1092` `sleep 1`, `radio_hal.sh:470` ≤ 5 s
hostapd wait) bounds the worst case at 7 s. ocbmd's `REARM_HOLD` (2 s, `main.rs:643`) guarantees
the 1 Hz poll sees the GONE edge even for a fast SUBSCRIBE. Full derivation in `CleanSlateReset`'s KDoc.

### Measured: one clean-slate reset (Reset Connection, wired iPhone, 2026-09-25 03:55:41)

```
+0.000  Reset Connection confirmed              (03:55:41.746)
+0.163  >> CT_STOP                              (client closed; lanes drained)
+0.247  USB interface released; nonce 0x02493916 -> 0x9041b6e0; "Waiting for adapter…"
+6.248  "Reconnecting…"                         (6.0 s wait)
+6.260  claimed interface 0; >> CT_HELLO; << HELLO_ACK 1 ms later
+6.312  >> CT_SUBSCRIBE
+6.394  << HOST_PRESENT, PHONE_PRESENT, BT_PHASE IDLE, PROJ_MODE NONE, PHONE_IDENT, BOX_HEALTH iap2d
        (the box IS idle: NONE is the replayed current state; the phone never left the bus)
+6.395  [deferred display mode from the mid-session test fired here: REINIT → second CT_STOP at
         +6.516, HELLO +6.931, SUBSCRIBE +7.001 — 0.6 s, session state identical]
+11.04  << BOX_HEALTH iap2d|airplayd   (carplayd re-armed)
+11.54  << PROJ_MODE WIRED_CP
+13.24  DEVICE_CONNECTED ("Phone connected")
+13.45  first video frame on screen
```

No `SEV_PHONE_ABSENT`, no error, no reconnect chain. Confirm-to-video 13.5 s, of which 6.0 s is
the deliberate wait and ~5 s is carplayd's own bring-up after re-arm. Two more timelines from the
same session are in 04_OPEN_ITEMS.md item 3 under "Host-app controls waiting on box verbs" — the
reported PROJ_MODE NONE → PHONE_ABSENT drop was NOT reproduced by any of the three.

### Follow-ups (listed, not implemented)

Forget-all (`MGMT_FORGET_ALL`), NCM maintenance mode (`MGMT_ENTER_NCM`), box log view (`CH_LOG`),
radio inhibit (`CT_RADIO`), preferred-device UI (`KnownDeviceStore.preferredMac` is stored, no
box verb acts on it), vehicle / metadata / audio config UI, adapter info panel (`MGMT_INFO` typed
surface). All exist on MacHost's Adapter tab; none is wired on Android.

## Provenance — this is a three-way graft

| From | What was taken | Status |
|---|---|---|
| `carlink_native_personal` | The product: frosted-glass UI, Controls dashboard, `MediaSessionManager` / `AlbumArtCache` / MediaBrowserService, `DualStreamAudioManager`, `MicrophoneCaptureManager`, `PlatformDetector` / `AudioConfig`, `H264Renderer`, day/night. **Authoritative for working AAOS behaviour.** | imported whole, builds green |
| `gm_ccpa` | `ocbm/` (framing, proto, transport, USB bulk, client) and `av/` (`HevcRenderer`, `AacPlayer`, `VoiceRouter`; the first two replaced 2026-09-25 by `VideoRenderer` / `MediaAudioPlayer`) — **head-unit-proven** | imported, repackaged `zeno.gmccpa` → `com.carlink` |
| `ccpa_custom` | The A/V transport half neither of the above has: this document's `ocbm/seam/`, plus `CH_INPUT` / `CH_METADATA` / the config push (still to come) | new code |

`applicationId` is **`zeno.carlink.ocbm`**, deliberately distinct from the riddleBox app's
`zeno.carlink`: that app is the working fallback on a head unit that is also a daily driver, and a
shared id would install over it with no way back. Both can run side by side and be A/B'd.

## What is device-proven, and where

Do not re-litigate these; they are hardware records, not inference.

- **HEVC hardware decode at 2400x960** — `OMX.Intel.hw_vd.h265`, limits `64x64`–`3840x2160`,
  `blocks-per-second 1–972000` (`gm_ccpa/evidence/05_codecs.txt`). 2400x960@60 needs 540,000 —
  inside the limit, so decode is not the ceiling. Live:
  `gm_ccpa/evidence/session_2026-08-05/05_timeline.txt:32-38` (`csd-0=105 B (VPS+SPS+PPS)`,
  FIRST FRAME RENDERED, 2231 frames, 0 AUs dropped).
- **AAC-LC 48 kHz through the vehicle speakers** — same session. The AAC decoder also advertises
  profile 39, so **AAC-ELD decode and encode are both available** on this unit.
- **Wireless AAC-LC 48 kHz stereo media is what the box negotiates** — `preset_wireless_8`
  (`crates/vendor/receiver/src/info.rs`, `pub fn preset_wireless_8` — currently `:1008-1035`),
  device-proven in
  `ccpa_custom/docs/ops/captures/2026-07-25_SUCCESS_airplayd_wl_handshake.txt:71-75`. Wireless is this
  project's architecture, so AAC stereo is the default, not an aspiration.

## `ocbm/seam/` — the forward-encrypted A/V seam

The one piece neither source project had. Under `OCBM_FWD_ENC` (the box default — `levers.rs:45-56`
returns true when the env is absent) the box **never decrypts A/V**: it forwards the iPhone's frames
byte-for-byte and hands over the per-stream key. The host decrypts. That is also why it suits a 60 fps
target — the box spends no CPU on crypto, which `docs/carplay/06_AV_PIPELINE.md` records as the framerate ceiling.

**The design decision worth knowing:** the seam layer is a *transcoder into the legacy framing*, so all
three proven renderers are used **byte-identical, with no modification**. They were written against the
box's legacy on-box-decrypt TCP seams, so each already speaks a framing we can synthesise:

| Renderer | Framing it expects | Produced by |
|---|---|---|
| `VideoRenderer` (was `HevcRenderer` until 2026-09-25; now H.264 **or** HEVC, latched from the first parameter set) | `[u32 BE len][Annex-B AU]` | `VideoSeam` |
| `VoiceRouter` | `[u32 BE rate][u16 BE ch][u8 atype][u8 codec][u32 BE len][AU]` (`VoiceTag`, 12 B — *corrected 2026-09-18: the codec byte was added so the router branches on it instead of assuming AAC-ELD; no longer byte-compatible with the box's legacy 11-byte `:9003` tag, which nothing in `:app` consumes*) | `AudioSeam` |
| `MediaAudioPlayer` (was `AacPlayer`, ADTS-only, until 2026-09-25) | the same 12-byte `VoiceTag` framing as the voice lane — codec 0 = S16LE PCM (wired), 1 = raw AAC-LC AU (wireless) | `AudioSeam` |

`SeamPipe` is the join: a bounded blocking `InputStream` the renderers consume exactly as they would a
socket. `VoiceRouter` even carries the diagnostic for the mismatch this removes — it rejects an
implausible rate with *"the seam is probably speaking the forward-encrypted v2 framing"*. It now never
sees it. A consequence worth keeping: pointing a renderer at the box's own legacy seam instead is a
one-line change, so an A/B is cheap.

`SeamPipe`'s bound is **backpressure, not buffering**. When a lane backs up, `write` blocks the OCBM
read thread → the USB pipe fills → the box's per-stream read-gate stops pulling → the iPhone throttles
*that* encoder. Unbounded queueing would convert a transient decode stall into unbounded memory growth
and a latency spike that never recovers.

### Rules that are each their own bug

1. **The video message cap is 16 MB, not `MAX_PAYLOAD`.** One message spans many 64 KiB OCBM frames.
   The macOS host capped at `2 * maxPayload` and silently rejected every 4K keyframe, leaving the
   decoder permanently poisoned (`OCBMAVDecrypt.swift:104-109`).
2. **`seq` is the decrypt nonce, so it advances even on failure.** The box consumed that nonce.
   Freezing it makes the next good frame look like a second gap. Tested.
3. **`audioType` 5 (`compatibility`) is media, not voice** (the `atype` match in
   `crates/vendor/receiver/src/session.rs`, currently `:932-942`). The macOS
   `isVoice { audioType != 0 }` shortcut would put music on the AAOS assistant volume group and
   self-duck it for the session. Tested.
4. **The video config may be a full ISO sample entry.** `unwrapSampleEntry` handles `avc1`/`avcC` as
   well as `hvc1`/`hev1` — `OCBMAVBridge.swift:80` handles only the latter two, which would black-screen
   a wireless session that fell back to H.264. Tested both ways.
5. **An AU with no `SEAM_FORMAT` yet is dropped, not guessed.** Guessing rate/channels/atype is how a
   stream lands on the wrong AAOS volume group.

### Tests

`app/src/test/kotlin/com/carlink/ocbm/seam/SeamTest.kt` — 15 tests, JVM-only, no adapter and no
emulator. The sealing helpers re-derive nonce and AAD from the wire spec rather than calling
`SeamCrypto`, so an error in the production derivation cannot cancel itself out.
`AudioSeamPlainTest.kt` (15 tests, 2026-09-18) covers the `SEAM_PKT_PLAIN` / mSBC path — see
"Phone-call audio" below.

```sh
./gradlew :app:testSideloadDebugUnitTest --tests 'com.carlink.ocbm.seam.SeamTest'
```

## Protocol core — landed

- **A/V is routed.** `OcbmClient.dispatch` feeds `CH_VIDEO` / `CH_MEDIA_AUDIO` / `CH_ALT_AUDIO` /
  `CH_METADATA` to the seams inline on the read thread (backpressure is the design, see `OcbmAvLanes`).
  `CH_ALT_VIDEO` is counted, not decoded — no cluster display is advertised.
- **`OcbmAvLanes`** owns the pipes, seams and consumer threads, and enforces the one rule that makes
  inline dispatch safe: whoever owns a pipe closes it in its consumer's `finally`.
- **The arming guard.** `lanes` is non-null only after a successful `CT_SUBSCRIBE`, and `subscribe()` is
  now gated on `helloAcked`. Pre-handshake A/V is counted and dropped instead of reaching a decoder
  with a stale key — previously true only by accident.
- **`MetadataSeam`** — `CH_METADATA` had no consumer at all.
- **Uplink**: `sendTouch` / `sendMediaButton` / `sendCommand` / `sendNightMode` / `sendAppearance` /
  `sendNav` / `sendTelephony` / `requestKeyframe` / `sendMicPcm` / `setRadios`, over one `txLock` and an
  `ocbm-tx` thread so a UI-thread touch can never block on a 2 s USB write.
- **`CT_UPLINK`, `CT_BT_PHASE`, split host/phone presence** are surfaced. `CT_UPLINK` was previously
  parsed and thrown away; `CT_BT_PHASE` did not exist in Kotlin and is the only signal the host has for
  the entire Bluetooth phase.
- **Lifecycle**: `setDeadHandler` on the transport (dispatched off the read thread, so teardown cannot
  join itself), a one-shot `onLinkDead` distinct from the recoverable `SEV_HOST_GONE`, `clearHalt` via
  `controlTransfer` before declaring a stalled endpoint dead, and an explicit single-use contract.
- **`VehicleConfigYaml`** renders the pushed document, pinned byte-for-byte by a golden test.

### One design finding worth carrying

`VideoSeam`'s output pipe is **swappable** (`attach`). `VideoRenderer` (was `HevcRenderer` until 2026-09-25) binds its Surface at construction,
so its pipe is surface-scoped — but the ChaCha20 key and frame sequence are **session**-scoped. Rebuilding
the seam on a surface change would discard the key, and the box only re-sends it when its *own* seam
reconnects, which a host-side surface change does not cause. Every frame after the first surface swap
would then fail to decrypt, permanently. `VideoSeamAttachTest` pins it.

## Session layer — landed (plan Part B)

`CarlinkManager` was rewritten in place: **3131 → 1914 lines** (2333 as of 2026-08-16, having since
re-grown with the OEM icon, phone identity and known-devices work), with its public API preserved
exactly, so `MainActivity`, `MainScreen`, `PhonesTab` and the whole `media/` layer compile with
**zero edits**. The riddleBox message pump is gone.

- **The pump is replaced by callbacks.** `handleMessage`'s 390-line switch, `handleAudioCommand`
  (166 lines) and the `VoiceMode` state machine are deleted. OCBM states things directly, so
  nothing is inferred: `onSessionKeyed` → `DEVICE_CONNECTED`, first `nowPlaying` → `STREAMING`,
  `CT_BT_PHASE` → truthful handshake status, `CT_UPLINK` → the mic gate.
- **Two whole bug classes went with it.** The `VoiceMode` machine existed because
  `PHONECALL_START` arrived ~130 ms *before* `SIRI_STOP`, so a naive reading killed the call's
  microphone; OCBM routes per stream by `audioType`, so there is no ordering to reason about. And
  audio formats are no longer guessed from a `decodeType` byte — each stream carries `SEAM_FORMAT`.
- **Video is an epoch.** `VideoRenderer` (was `HevcRenderer`) binds its Surface at construction, so pipe + renderer +
  consumer thread are retired as a unit. The `join(1500)` on retire is not optional: without it a
  second decoder is configured while the first is still draining, and this VPU's codec pool is
  small enough that exhausting it is a permanently black screen. `pauseVideo` closes the pipe, and
  that is also the drop policy — a paused-but-open pipe would backpressure the OCBM read thread and
  stall audio and control traffic too.
- **`setState` / `setStatusText` / `updateMediaSessionState` / the wake-lock pair are carried over
  verbatim.** They contain no protocol content and all of the AAOS arbitration, including the
  `CONNECTING ⇒ setProjectionActive(connectingPhase = true)` edge that gets Carlink into the
  playback-primary slot before another source becomes undisplaceable, and the FGS keep-alive guard
  during reconnect backoff.
- **`sendMultiTouch` keeps its exact signature**, so `MainScreen.handleTouchEvent` — 110 lines of
  deadband, ACTION_CANCEL UP-synthesis and DOWN→MOVE demotion — is untouched.

### Dead code removed (~6,500 lines)

`protocol/AdapterDriver`, `protocol/MessageParser`, `usb/UsbDeviceWrapper`,
`audio/DualStreamAudioManager`, `audio/AudioFormats`, `platform/AudioConfig`,
`platform/PlatformDetector`, `ui/settings/AdapterConfigPreference`, `video/H264Renderer.java`,
`util/AppExecutors.java`, and `MessageParserTest`. Verified first that every cross-reference from
the surviving files into these was KDoc-only; the sole real dependency
(`MicrophoneCaptureManager → AudioRingBuffer`) is entirely within the survivors.

**The test count fell 168 → 99, and that is the honest number**: `MessageParserTest`'s 69 tests
exercised the riddleBox parser that no longer exists. (It is back to **150** as of 2026-08-16 — the
mic, plist and known-device suites added since, plus four more `VehicleConfigYamlTest` cases.)

`protocol/MessageSerializer.kt` and `MessageTypes.kt` survive because the app still uses
`TouchPoint`, `MultiTouchAction`, `PhoneType`, `AdapterConfig` and `KnownDevices` — but most of
their other contents are now dead. Trimming them to the input/config value types (and moving them
out of a package called `protocol`) is a worthwhile follow-up.

### Regressions this stage makes real

These were accepted decisions, but they are live now rather than theoretical:

- ~~**Pinch-zoom in Maps does not work.**~~ **SUPERSEDED 2026-08-15 — multi-touch shipped**, see
  "Multi-touch" below. `sendMultiTouch` forwards up to `MAX_CONTACTS = 2` pointers and `airplayd`
  coalesces them into Apple's single two-finger HID report; nothing suppresses secondary pointers any
  more. A THIRD pointer is still dropped, because two is Apple's descriptor capacity.
- **`connectToDevice` and `disconnectPhone` both bounce wireless**, because OCBM has no targeted
  connect and no per-phone disconnect verb. Right with one paired phone, wrong with several. The
  status text says "Restarting wireless..." rather than pretending otherwise.
- ~~**Device cards show bare MACs**, and `connectedBtMac` is a single-bonded-device heuristic.~~
  **SUPERSEDED 2026-08-16 by Known-devices Phase 1** (see "Known devices" below): cards render
  remembered names from `KnownDeviceStore` via `mergeDeviceList` — a bond learned but never yet
  identified still shows its bare MAC until `CT_PHONE_IDENT` names it — and `_connectedBtMac` prefers
  `connectedPhoneMac`, falling back to the single-bond heuristic over `bondedMacs` only when no
  identity has arrived.
- **Escalation Pattern B is gone** (no `SCANNING_DEVICE` analogue); A and C survive. Pattern A is
  re-keyed to "no `CT_HELLO_ACK`", keeping the exact `"no initial response"` substring that remote
  log filters match on.
- **There is no internal restart trigger any more.** detekt caught `requestRestart` as unused,
  which is the correct signal: riddleBox restarted from UNPLUGGED / Phase-0, whereas the box now
  holds the session across a phone departure and a dead link goes through the reconnect path.

## QC pass 2 — 12 agents, 2026-08-15

Static analysis over all 16,978 lines / 45 files, partitioned 12 ways. Cross-checked against
`crates/ocbm-proto`, the box daemons, the Swift host, and both reference apps.

**Confirmed sound** (recording this so it is not re-litigated): every OCBM constant and all ~40 of
their source citations; the pushed YAML byte-for-byte against `APP_DOC`; the video and audio
nonce/AAD derivations against Rust *and* Swift; `Reassembler.next()`'s owned-copy invariant;
`runConsumer`'s close-on-every-exit-path guarantee; the `SEV_HOST_GONE` backoff (flap escalation is
genuinely unreachable); seq/write atomicity under one `txLock`; `retireVideoEpoch`'s ordering and
bounded join. `media/` and all UI files are byte-identical to `carlink_native_personal`;
`HevcRenderer`/`AacPlayer`/`VoiceRouter` are semantically identical to gm_ccpa (ktlint-only diffs).

**Fixed:**

| Was | Now |
|---|---|
| `release()` outside `lifecycleMutex`; `released` checked once, before a ~80 s `findDevice` — a released manager could claim USB and run a zombie session against its replacement | `acquireTransport()` re-checks `released` after discovery and after open |
| No exit from `STREAMING` on phone departure — frozen frame, live touch, status text hidden behind it | `onPhonePresence` drops to `DEVICE_CONNECTED` |
| `onUplinkGate` ran inline on the read thread, against this file's own contract → hot mic orphaned into the next session at the wrong rate | scope hop + client-identity guard |
| `onCaptureError` declared and invoked, never assigned → Siri/call silent for the rest of a gate-on | wired to `stopMicrophoneCapture()` |
| Mid-session epoch reopen never requested a keyframe, and an unconfigured `HevcRenderer` cannot (its `!configured` return precedes its own request gate); `VideoSeam.onGap` cannot cover it either, since seq advances with no pipe attached | `client?.requestKeyframe()` inside `openVideoEpoch` |
| `tryRecreateAudioRecord` guarded on `isRunning`, which a *new* session also sets → zombie published over the live record | thread-identity guard, publish after check |
| Stale `onSessionKeyed` post could set `DEVICE_CONNECTED` after teardown's `DISCONNECTED`, wedging reconnect *and* USB-attach (both gated on `DISCONNECTED`) | `if (client !== c) return@launch` |
| `release()` leaked the wake lock (2 h) + FGS when the `DISCONNECTED` post was cancelled or the state already matched | unconditional cleanup in `release()` |
| `VoiceRouter` promised a track rebuild it could not perform — a released sink stayed keyed with `isConfigured=false`, `feed()` early-returned forever, `sweepIdle` skipped it. One `ERROR_DEAD_OBJECT` killed that purpose until the lanes generation retired | revive in `route()` when `!isConfigured`. Revive rather than evict: `configure()`'s own `CONFIGURE_RETRY_MS` gate then preserves the two error paths' differing intent, where a fresh Sink's zero backoff would rebuild per frame |
| `OcbmClient.stopped` latched at the end of `stop()` | latched first |
| `onHostPresence` unwired; `VideoSurface.kt` cited a stale line | wired (`c.onHostPresence = { … }` in `wireClientCallbacks`). **The line "correction" was itself wrong when committed and is still wrong in the code:** `VideoSurface.kt:33` cites `CarlinkManager.kt:1124`, but `onSurfaceDestroyed` was at `:1137` in that very commit and is at `:1313` today. Cite the symbol, not the line |

**Known and accepted, not defects to chase:**

- **PCM media is dropped, not played.** `AudioSeam.route` gates the media leg on AAC-LC. Correctly
  never misrouted to the voice sink, but silent — so a *wired* phone has no media audio. This makes
  the wireless-only decision load-bearing rather than incidental.
- **Audio focus is taken at lanes-armed** (box subscribe), not at phone presence — so the adapter
  plugged in with no iPhone holds media focus and mutes other vehicle sources. Decided 2026-08-15 to
  keep: it guarantees focus before any audio can arrive. Revisit if it annoys on hardware.
- **A consume-loop `break` is now generation-fatal.** Under gm_ccpa `consume` ran per TCP connection
  and the box re-dialed; `runConsumer` runs it once and its `finally` closes the pipe. The rule that
  fixes the USB-read-thread wedge is what makes those breaks permanent.
- ~~`onHostUIPressed` is still never fired — `META_CMD` is unparsed, so the dashboard overlay is
  unreachable. No dead control is drawn: `oemIconVisible` is read only by the retired
  `MessageSerializer`, and `vehicleConfigSpec()` sends only name/width/height/maxFps.~~
  **SUPERSEDED 2026-08-15 by the OEM-icon commit (846c492), which landed hours after this QC pass.**
  `META_CMD` IS parsed (`CarlinkManager.onMetadata` → `onCommandPlist`) and DOES fire
  `onHostUIPressed` on `requestUI`; `VehicleConfigYaml` emits `oemIconConfig` including `visible`;
  and `vehicleConfigSpec()` passes `oemIconImages` + `oemIconLabel` alongside
  name/width/height/maxFps. What survived at the time: **safe-area/cutout geometry never reaches the
  wire** — `vehicleConfigSpec()` consumed none of `MainActivity`'s `viewAreaData`/`safeAreaData`.
  **LARGELY SUPERSEDED 2026-09-18** by the content-area work below ("Three rectangles"): the detected
  content area (window minus visible bars) now IS what `vehicleConfigSpec()`/`CT_SUBSCRIBE` declares,
  device-measured on the emulator and a live box. Cutout/waterfall/corner-arc insets specifically are
  still JVM-proven only — no available panel has any, live box included — so that narrower geometry
  still does not have an on-hardware wire proof. See "3P native shell" and "Three rectangles" below.
- `MicrophoneCaptureManager.stop()`'s `join(1000)` expiring mid-`read()` will now deliver a spurious
  "capture died" callback after a *normal* stop. Harmless (it only clears an already-clearing flag),
  but it will appear in logs.

## Drive state -> CarPlay limitedUI (2026-08-15, hardware-verified)

The AAOS gear selector now drives CarPlay's limited-UI mode, matching the macOS control box's
"Limited UI (Drive)" toggle.

**Source signal is `CarUxRestrictionsManager`, not `GEAR_SELECTION`.** Reading the gear directly
needs `android.car.permission.CAR_POWERTRAIN`, which is `signature|privileged` and unavailable to a
sideloaded app. UX restrictions need no permission to listen, and AAOS already derives them from
gear and speed — `inject-vhal-event GEAR_SELECTION 8` flips `DO: false UxR: 0` to `DO: true
UxR: 16`, verified on device. It is also the better semantic match: `isRequiresDistractionOptimization`
and `setLimitedUI` are the same statement.

`android.car` is an optional shared library — `useLibrary("android.car")` (compile-only),
`<uses-library required="false">`, and every touch guarded by `FEATURE_AUTOMOTIVE`, so the APK still
installs and runs on a phone.

**Both halves are required, and this cost a test cycle to learn.** The runtime switch is
`CMD_LIMITED_UI_ON/OFF` (0x08/0x09) on CH_INPUT — that alone changed nothing. Absent a
`limitedUIConfig` the box omits `limitedUIElements` from `/info` (`info.rs:126`) and iOS falls back
to its own default set, which was measured to leave the Maps search keyboard visible. The pushed
config now declares the six wire-emitting elements. Traced against the macOS app to confirm: its 'D'
button does ONLY `sendCommand(cmdLimitedUIOn)` (`ControlsWindow.swift:181-190`) — the declaration
comes from its Settings YAML, exactly as it now does here.

Verified: Maps Search shows keyboard + Siri in Park, collapses to a single "Ask Siri" button in
Drive, and restores on the way back.

## Session management: a fast app restart (FIXED 2026-08-15, hardware-verified)

Reinstalling or `am force-stop`-ing while a session was live and relaunching immediately left the box
unable to serve A/V: the new host subscribed, `PHONE_PRESENT` arrived instantly, and no session key
ever followed. It hung at CONNECTING indefinitely.

The box learns a host is gone two ways — a clean `CT_STOP`, or `HEARTBEAT_GRACE` (10 s, `ocbmd`
`main.rs:562`) elapsing. A SIGKILL bypasses the first, and a relaunch inside the grace window defeats
the second: heartbeats simply continue, from a different process. Nothing on the wire distinguished
one host instance from another, so from ocbmd's side the host never left, and the redundant
`CT_SUBSCRIBE` against `present=true` never re-armed projection — which matters because
`session_supervisor.sh` spawns `airplayd` ONLY on the GONE->PRESENT edge of `/tmp/host_present`.

**The fix: a host instance nonce in `CT_HELLO`'s four reserved bytes** (u32 LE; 0 = "not supplied",
so an older host behaves exactly as before). It is scoped to the host SESSION — one `CarlinkManager`
— deliberately NOT to the client: the contract is one client per USB session, so a client-scoped
value would change on every reattach and re-arm projection for a mere USB blip, a ~45 s rebuild
instead of a warm reuse. Same nonce = same host reattaching; different nonce while `present` = the
previous host is gone.

**It was wrong twice before it worked, and both are worth knowing:**

1. **It signalled the wrong audience.** `set_present(false)` emits `SEV_HOST_GONE`, and the host reads
   that as "the box dropped us" — so it retired its A/V lanes and re-subscribed, and the cycle meant
   to bring projection UP tore it down, once per attempt. The re-arm edge is for the SUPERVISOR; the
   host is the one that just arrived. `rearm_presence_silently()` now dips the flag without emitting
   the event, and the host is told `SEV_HOST_PRESENT`, which is true and sufficient.
2. **Even silenced, the edge was invisible.** The supervisor polls `/tmp/host_present` about once a
   second, so a `false`->`true` flip written back-to-back is never observed — the host reconnected
   cleanly and projection still never returned. The flag now dips for `REARM_HOLD` (2 s, one poll
   interval with margin) before being restored, with the poll timeout held short so the tick fires.
   Host-facing `present` stays true throughout.

Verified: SIGKILL the app and relaunch ~1 s later now reaches STREAMING in ~30 s.

## Phone identity (2026-08-15, hardware-verified)

The CarPlay connection has always carried the iPhone's name; we were discarding it. Apple's receiver
reads `name` from the phase-1 SETUP body plist beside `deviceID`/`macAddress`/`sessionUUID`
(`AirPlayReceiverServer.c:3213`), with an `X-Apple-Client-Name` header fallback (`:2354-2360`).

Captured from the real SETUP: `name=<owner> iPhone`, `deviceID=64:31:35:8c:29:69`, `model=iPhone18,4`,
`osName=iPhone OS`, `osVersion=27.0`.

`deviceID` is the **BR/EDR MAC**, so it joins against `MGMT_INFO`'s bonded list — the only thing on
the wire that says WHICH bonded phone is live. The receiver publishes it to `/tmp/phone_identity`
(atomic rename) and ocbmd mirrors changes as `CT_PHONE_IDENT` (0x18). A file, deliberately:
`/tmp/pairing_code`, `/tmp/bt_phase` and `/tmp/phone_present` already cross the airplayd->ocbmd
boundary that way and ocbmd already runs a change-detecting tick over each.

Capturing the SETUP had been reachable only via `/tmp/mainbuffered_test`, which also arms
mainBufferedAudio — a flag whose own comment warns it can SILENCE MEDIA on the wireless arm. The dump
now has its own `/tmp/setup_dump` flag, so a read-only diagnostic no longer costs an audio risk.

## Known devices (Phase 1 landed 2026-08-16, hardware-verified)

The device list now renders from the app's own history instead of waiting for an adapter session:
with the app at CONNECTING — no session, no `MGMT_INFO` — the card already reads "<owner> iPhone /
Last seen: 0 minutes ago".

The app must own this. The box physically cannot supply it: the link-key record is 25 bytes of mgmt
Load-Link-Keys layout with no room for a name, a timestamp or a device class, and `MGMT_INFO.devices`
is a bare MAC array read straight out of it. `CT_PHONE_IDENT.deviceID` is the join key that makes an
app-side history possible at all.

`com.carlink.device.KnownDeviceStore` — SharedPreferences, one versioned JSON document. Not DataStore
(deliberately removed from this project as "write-only dead I/O", and its Preferences flavour has no
list type). `commit()` not `apply()`, because a head-unit power cut IS the normal shutdown here.
Writes go through a store-owned thread, not `CarlinkManager`'s scope, which `release()` cancels
before teardown.

**The emitter builds its JSON explicitly rather than calling `JSONObject.toString()`.** That is not
style: JSONObject's key order is implementation-defined — AOSP backs it with a LinkedHashMap, the
reference org.json with a HashMap — so the same snapshot serialised to different bytes on device than
under test. Escaping is still delegated to `JSONObject.quote`.

Two live bugs fixed on the way: `_connectedBtMac` was derived from `_pairedDevices.singleOrNull()`,
which merging history into that list would have made null for a user with one bonded phone plus one
historical entry — silently killing the "Connected" highlight for exactly the single-phone case it
was built for; and `forgetDevice` now deletes the persisted record, which `recentlyForgotten` (in
memory, elapsedRealtime-based) cannot cover.

Also note: unit tests needed `org.json:json` added, because the unit-test `android.jar` ships stubs
whose methods throw — which is why nothing in this app that parses JSON had ever been unit-tested.

Phases 2 (UI) and 3 (box-side preferred device) are specified in **`KNOWN_DEVICES_PLAN.md`**.

## Remove forgets both pairings (2026-08-16, hardware-verified)

Owner decision: Remove forgets the phone from the app AND the box, so the next connection from that
phone is a fresh pairing. Clearing the BR/EDR bond alone did not achieve that — the phone redoes
Bluetooth SSP, but its AirPlay long-term key survived in `/etc/carplay_peers.bin` and the next session
took the fast pair-verify path.

Both `MGMT_FORGET_DEVICE` and `MGMT_FORGET_ALL` now clear the whole peer store. Per-device removal is
impossible: the store is keyed by the controller's AirPlay pairing identity (the `IDENTIFIER` TLV from
pair-setup M5), not the BR/EDR MAC, and that id never leaves the pairing crate. The accepted trade is
that other bonded phones redo pair-setup once — the slow path, not a prompt.

Deleting the file suffices even though a running airplayd holds the pairings in memory and `save_peer`
persists the WHOLE map: both callers request a wireless restart, and `wireless_down` reaps airplayd
whenever the wireless session owns it, so it reloads from the absent file.

## Multi-touch (2026-08-15, hardware-verified)

Two-finger pinch/zoom/rotate now works. It needed changes on BOTH sides, and the box half is the
substantive one.

**The wire and the HID report disagree about framing, and that is the whole problem.** OCBM sends
ONE finger per `INPUT_TOUCH` frame, but Apple's `HIDTouchScreenMultiCreateDescriptor` declares both
`Finger` collections in a SINGLE input report. So `airplayd` holds contact state and coalesces: a
frame for one finger is combined with whatever the other is currently doing before anything is sent.
Without that the second contact overwrites the first and a pinch reads as a jumping single touch.
This resolves the open question carried in the plan since design.

Descriptor is transcribed byte-for-byte from the licensed R14G17 source. The transcription was
checked by DERIVING the four geometry patch offsets from the byte layout and comparing them to
Apple's literals — 0x2F/0x30, 0x3C/0x3D, 0x6E/0x6F, 0x7B/0x7C all match, 133 B total. Each finger
carries its own logical maxima, so all four must be patched or the second contact reports in a
different coordinate space than the first.

**Exactly TWO contacts, because that is Apple's capacity, not a policy choice.** A third pointer is
dropped rather than remapped: making room means evicting a live contact, which breaks the gesture in
progress. Extending the descriptor was considered and rejected — no licensed reference exists for a
wider shape, and a guessed HID descriptor is what broke this box on 2026-07-06.

Gated on `hidConfig.touchScreenSupportsMultiTouch` (Apple's own key, previously parsed by nobody),
armed per connection through the same lever path as `dPadSupport`, and RESET on teardown. That reset
is not hygiene: the descriptor determines the report LAYOUT (12 B vs 5 B), so a stale advert would
have the HID path emitting reports the session's descriptor cannot parse.

## Deploying to the box without NCM

The emulator claims the adapter's USB device exclusively, so the Mac has no NCM interface and
`boxsh.py`/telnet are unavailable while it runs. `ocbm-host` works over the same OCBM bulk pipe and
covers the whole loop: `pull` (verified backup), `push` (CRC-checked), and `console` (a root shell).

Traps worth knowing, each hit once:

- **Do not run a pushed binary to "test" it.** `airplayd.new --help` does not print help, it STARTS
  the daemon and blocks the console shell; every later command queues behind it. Ctrl-C recovers.
- **`console` leaves the box in `mode=1`.** HELLO_ACK's mode reflects whether the console pty is
  open, and there is no `CT_MODE_SELECT` back to PROJECTION — exit the shell (or reboot) or the next
  session starts against a box still in console mode.
- **Do not UPX-pack with the host toolchain.** `tools/README.md:14`: host UPX 5.x segfaults the box's
  3.14 kernel; packing requires UPX 3.96 in the Lima VM. Unpacked pushes fine (1.79 MB vs the
  shipped 895 KB) — rootfs is jffs2 with only ~4 MB free, so check `df` first.
- **Keep backups off rootfs.** The USB stick mounts at `/mnt/UPAN` (vfat, 60 GB). The pre-multitouch
  binary lives at `/mnt/UPAN/ccpa_backups/airplayd.pre-multitouch.crc934ca03d`.

## OEM icon + the return path (2026-08-15, hardware-verified)

The CarPlay home screen now carries a "Carlink" tile, and tapping it brings this app forward. Both
halves were already supported by the box and unused by us.

**Advertising it** is `oemIconConfig` in the pushed YAML — `images[]` + `label` + `visible`, emitted
between `limitedUIConfig` and `audio` to match the box's struct order. All THREE of Apple's sizes
(120/180/256, `AppleCarPlay_AppStub.c:611-637`) are mandatory: the box's notes record a
device-confirmed finding that iOS renders only the LABEL for a single-size set, so a partial set is
no icon at all.

**The return path** is `META_CMD` on CH_METADATA, which carries the raw binary plist of an inbound
iPhone `POST /command`. The verb is `requestUI` — Apple's own words are "the function to call when
the controller requests accessory UI" (`AirPlayReceiverSession.h:189`), and it is the SAME verb name
the host sends outbound. We had been dropping `META_CMD` entirely, which is why `onHostUIPressed`
was declared, implemented in `MainScreen`, and never fired. With it wired, the whole host-UI overlay
— today the Settings screen (Phones / Control tabs), Reboot Adapter / Reset Connection, and
`recoverVideoFromOverlay` — stops being dead code.

`BinaryPlist` is a minimal `bplist00` reader for exactly that payload. macOS gets this from
`PropertyListSerialization`; Android has nothing equivalent, and scanning the bytes for the ASCII
verb is not sound (a verb name can appear inside a URL). It parses on the OCBM READ THREAD, so it
returns null rather than throwing on anything malformed — every read is bounds-checked, refs are
validated, recursion is depth-capped, and a test asserts that every truncation and every single-byte
corruption of a real payload fails safely. Fixtures come from Apple's own encoder (`plutil -convert
binary1`), so a shared misreading of the format cannot cancel itself out.

**The icon is flattened to [dominant colour + foreground layer], and that is a size decision made
from measurement.** This app's adaptive background is a detailed image that will not compress: the
three required sizes came to 115,852 B of base64 against a budget of 57,344, because `CT_SUBSCRIBE`
carries `[verb][yaml]` in ONE OCBM frame capped at `MAX_PAYLOAD`. Filling with the backdrop's own
dominant colour and drawing only the logo brings the set to 33,996 B (a 36,059 B config), keeps the
mark and the palette, and leaves ~20 KB of headroom. The budget itself is DERIVED from
`MAX_PAYLOAD` rather than chosen — the first hand-picked value was 48 KB and rejected a perfectly
deliverable 52 KB set on hardware.

## Not done yet

- **The config guard.** The document is pinned by a Kotlin golden test, but `tools/check_app_yaml_fixture.py`
  only knows about the Swift emitter. Until a Kotlin arm exists, this is a second emitter of a schema
  that has silently drifted before. A Rust test asserting the Android document parses with the box's own
  serde is the stronger of the two remaining guards.
- `CH_MGMT` typed surface (`MgmtInfo`) and `CH_FILE` `FILE_PULL` for config readback.
- **The host-side `mgmtLock` gap.** `mgmtAction`/`mgmtGetInfo` clear the response queue with no lock,
  so two concurrent MGMT actions can steal each other's ACK. Latent today; Phase 3 puts a MGMT verb
  behind a UI tap, which makes it reachable.
- **`onHostUIPressed` is wired, but `META_CMD` is only partly consumed.** `requestUI` is handled;
  every other inbound iPhone command is logged once by verb and dropped. `BinaryPlist` can decode
  them all when a use appears.
- ~~**The FGS microphone type never applies** — the service ran `types=0x2` (mediaPlayback only)
  because the first `startForeground` happened during CONNECTING, before `RECORD_AUDIO` was
  granted, so mic capture worked only while the activity was visible.~~ **RE-CHECKED 2026-09-18,
  found already fixed (not part of today's three WPs — pre-existing in the codebase, not caught by
  an earlier pass of this document).** `CarlinkMediaBrowserService.startForegroundMode()`
  unconditionally ORs `FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK`, `..._CONNECTED_DEVICE` and
  `..._MICROPHONE` into every `ServiceCompat.startForeground` call
  (`media/CarlinkMediaBrowserService.kt:294-297`), and the manifest declares all three
  (`AndroidManifest.xml:138`, `foregroundServiceType="mediaPlayback|connectedDevice|microphone"`).
  Not independently hardware-verified as part of this session (no backgrounding-during-a-call test
  run today) — flagged here rather than left silently stale.
- ~~**Cutout/safe-area geometry never reaches the wire.**~~ **DONE, and hardware-verified beyond the
  emulator (2026-09-18) — see "Three rectangles — a real defect found and fixed" under "3P native
  shell" below.** `DisplayProfile.detect(activity, visibleBarTypes)` derives the content area (window
  minus the stable insets of whichever bars the active `DisplayMode` leaves visible), and that — not
  the panel, not the raw window — is what `CarlinkManager.vehicleConfigSpec()` declares, what the
  decoder is sized to, and what the video surface is laid out to. A real defect from measuring the
  window instead of the content area (a 1-pixel-tall SurfaceView mismatch in portrait, 800x1151 vs.
  declared 800x1150) was found and fixed the same day via a `surfaceInsets` parity-pixel correction.
  Measured on the emulator AND on the live box in both fullscreen and system-bars-visible, landscape
  AND portrait (see the measured table below), including a real iPhone CarPlay session tapping into a
  bar-offset surface correctly. The legacy `viewAreaData`/`safeAreaData` blobs in `AdapterConfig` are
  still computed and still unread — harmless leftovers. The cutout/waterfall/corner arms remain
  JVM-proven only (`DisplayProfileTest`) — no available panel, emulator or live box, has any.
- **Test coverage gaps that matter.** `VideoSeam` claims `hvc1`/`hev1` but only `hvc1` and `avc1`
  unwrapping are exercised; a live session shipping `hev1` would hit an untested branch and
  black-screen. No test covers a non-`F_BOTH` (fragmented) OCBM message, or the heartbeat-driven
  re-subscribe after `SEV_HOST_GONE` (the retire side is covered, the re-subscribe side is not).
  The mic path was closed on 2026-08-15 — see below.

## 3P native shell (WP1, 2026-09-18) — emulator + live box, no phone session

Goal: integrate into AAOS the way a native CarPlay head unit does, as an ORDINARY third-party app.

**The governing constraint, verified on a running AAOS 15 image (2026-09-18):** `pm list
permissions -f` shows `android.car.permission.CAR_PROJECTION`, `ACCESS_CAR_PROJECTION_STATUS`,
`CAR_NAVIGATION_MANAGER`, `CAR_UX_RESTRICTIONS_CONFIGURATION` and `CAR_DRIVING_STATE` are all
`signature|privileged` — unreachable to a sideloaded app; only `CAR_INFO` is `normal`.
`CarUxRestrictionsManager` *listening* (used below, "Drive state -> CarPlay limitedUI") and
`CarAppFocusManager` focus claims (used below, "`CarAppFocusManager`") need no permission and stay
in scope. The public SDK jar (`platforms/android-35/optional/android.car.jar`) has 55 classes and
contains NEITHER `CarProjectionManager` NOR `ClusterHomeManager` — so cluster turn-by-turn is out of
reach for a 3P app, full stop; this is not a permission gap to be re-argued later.
**Consequence: `:app` rejects `:projection`'s `CarProjectionBridge` route** (`projection/src/main/
kotlin/com/carlink/projection/CarProjectionBridge.kt`) and does not use `car-system-stubs`
(`car-system-stubs/src/main/java/android/car/CarProjectionManager.java`,
`.../projection/ProjectionStatus.java` — signature-permission stand-ins, compile-only for
`:projection`). Record this so `ProjectionStatus`/`CarProjectionManager` for `:app` is not
re-proposed; the SDK evidence above is why, not a preference.

**Module split.** `:app` (applicationId `zeno.carlink.ocbm`, `app/build.gradle.kts:21`) owns the
USB/OCBM transport to the adapter directly (`ocbm/UsbBulkTransport.kt`) and is the product this
document describes: adapter-bridged CarPlay running as an ordinary 3P app on AAOS. `:projection` is
a different arrangement — it consumes a loopback TCP seam (`projection/src/main/kotlin/com/carlink/
projection/SeamListener.kt:78`) and pairs with the privileged `CarProjectionBridge` route just
rejected above. The two modules are not two versions of the same thing; only `:app` is load-bearing
for this document.

This app still declares zero car permissions. Everything below was run on
the AAOS 15 emulator (API 35, 2400x960) with the CCPA box attached over USB — subscribed session,
no iPhone — so "box-verified" means the OCBM frames left and the box's replies parsed; nothing
below has been seen by an iPhone yet.

**MediaSession as the integration point** (`media/MediaSessionManager.kt`, `media/MediaKeyDecoder.kt`).
`MediaLibrarySession.Callback.onMediaButtonEvent` now decodes raw KeyEvents: PLAY/PAUSE/NEXT/PREV,
PLAY_PAUSE as the dedicated HID toggle (`MEDIA_BTN_PLAY_PAUSE`, never split from mirrored state), a
HEADSETHOOK short press (answer a ringing call, else toggle — never hang up), HEADSETHOOK long press /
VOICE_ASSIST → Siri. Measured: `cmd media_session dispatch play-pause` and `input keyevent
KEYCODE_MEDIA_NEXT` land in our callback (`media key 85 -> PLAY_PAUSE`, `87 -> NEXT`) once the session is
the platform's "media button session" (it is, after the USB grant; before that the keys go elsewhere).
**Finding:** `input keyevent --longpress KEYCODE_HEADSETHOOK` reaches the session as a bare ACTION_UP —
AOSP's `MediaSessionService` swallows the voice-key long press and calls the *assistant* itself. So
tier 2 (long-press via the session) only works on a head unit that passes the raw hold through; on
stock AOSP it collapses into tier 3. Decoder state machine is JVM-tested (`MediaKeyDecoderTest`).

**Now-playing** (`media/NowPlayingInfo.kt`). `CarlinkManager.processNowPlaying` now also merges
`genre`, `composer`, `trackNumber`, `trackCount` (the macOS `MediaSnapshot` set); `appName` was
already carried and lands in `setSubtitle` + `setStation`. Published with `MEDIA_TYPE_MUSIC` and
`isPlayable`. Unverified against a phone — no nowPlaying frames without one.

**Siri activation — measured route table (2026-09-25, emulator-5554 = chevy12, AAOS 14, app as user 10,
live wired CarPlay).** Wire form for every app-side route is the `CMD_SIRI_DOWN`/`CMD_SIRI_UP` hold
pair (`OcbmClient.sendSiriPress()` → two `INPUT_COMMAND` frames → box `/command requestSiri
{siriAction: 2}` then `{siriAction: 3}`, `events.rs`; the bare `CMD_REQUEST_SIRI` is deprecated and iOS
ignores it). Proof of activation in every "yes" row below: `<< UPLINK ON 16000Hz 1ch codec=0 (mic gate)`,
`[MIC] Capture started at 16000Hz 1ch`, `audio format … codec=PCM 16000Hz 1ch atype=2 -> voice`,
`[voice] siri: PCM 16000Hz 1ch -> AudioTrack(usage=16), decoder=direct PCM`, `[voice] assistant SPEAKING —
pausing media`, and the Siri orb in the CarPlay frame.

| Route | Injected as | Reaches the app? | Siri activates? |
|---|---|---|---|
| A. Press-and-hold the CarPlay dock home button (touch forwarding only) | `input swipe 80 932 80 932 1500` (dock button at x≈80; y=932 with system bars visible, y≈1054 in fullscreen-immersive) | yes — plain `INPUT_TOUCH` DOWN … UP; the box turns them into HID digitizer reports and iOS times the hold itself, so no MOVE keep-alive or timestamps are needed | **yes** (`/tmp/siri_a3.png`: orb; log lines above at 02:08:03) |
| B1. `cmd car_service inject-key 231` (VHAL `HW_KEY_INPUT`, tap and `-t 1000` hold) with the platform assistant left as Google's | VHAL → `CarInputService` | no — `CAR.INPUT: voice key, invoke AssistUtilsHelper` → Google `AutoVoiceInteractionSes` opens; no `[KEY]` line | no (Google assistant session instead) |
| B2. same, with this app selected as the digital assistant (`settings put secure --user 10 voice_interaction_service` + `assistant` = `zeno.carlink.ocbm/com.carlink.voice.CarlinkVoiceInteractionService`) | VHAL → `CarInputService` → `AssistUtils` → `CarlinkVoiceInteractionService` | yes — `[VOICE] PTT (flags=0x23) -> Siri hold pair sent`, `[SIRI] press -> sent` | **yes** (`/tmp/siri_b2.png`: orb + AAOS mic indicator; 02:11:53) |
| B3. `input keyevent 231` / `--longpress 231` (VOICE_ASSIST via InputManager) | window manager | no — nothing reaches the activity | no |
| B4. `input keyevent 84` (SEARCH) and `--longpress 84` | activity `onKeyDown` | yes — `[KEY] voice key 84 -> Siri sent` | **yes** (02:09:00; the long-press variant re-sends the pair, which toggles Siri if already up) |
| B5. `input keyevent 219` (ASSIST) | window manager (assist gesture) | no | no |
| B6. `input keyevent --longpress 79` (HEADSETHOOK, the MediaSession route: `MediaKeyDecoder` → `onVoiceAssist` → `requestSiri`) | MediaSession media-button dispatch | no — the app's session never received the key on the emulator (media keys go to the active media session; a paused CarPlay session was not it) | no |
| B7. `cmd car_service inject-key -t 1000 5` (CALL hold, push-to-talk on some units) | `CarInputService` | no | no |
| Emulator steering-wheel controls | none exist for a voice key; the VHAL path IS `inject-key` (B1/B2) | — | — |

The first attempt at route A failed for a reason worth keeping: with system bars visible the AAOS
bottom bar spans `y>1002`, so a hold at `y=1054` hit the bar's own home button and the launcher
came to the front (`ActivityTaskManager: START … CarLauncher`). Locate the dock button from a
screenshot for the active display mode. `handleTouchEvent` is unchanged — no long-press special case
is needed on the app side.

**Implemented routes (all third-party legal, nothing GM-only):**
1. Activity key: `MainActivity.onKeyDown` → `VoiceKeys.triggers(keyCode, repeatCount)` (VOICE_ASSIST /
   ASSIST / SEARCH, first DOWN only; `VoiceKeysTest`). On the emulator only SEARCH arrives this way.
2. Assistant role: `voice/CarlinkVoiceInteractionService` + session + `CarlinkRecognitionService`
   (manifest, `BIND_VOICE_INTERACTION` held by the system; the app needs no permission). Inert until the
   user picks Carlink under Settings > Apps > Default apps > Digital assistant (`android.app.role.ASSISTANT`;
   a third-party app may hold it on the emulator — verified above — and no `ACTION_VOICE_COMMAND` /
   `ASSIST` intent filter is needed, `CarInputService` calls `AssistUtils` directly). Cost: while Carlink
   is the assistant the platform `SpeechRecognizer` is our stub (`ERROR_CLIENT`). The emulator's
   setting was restored to `com.google.android.carassistant` after the test.
3. MediaSession voice key (`MediaKeyDecoder.VOICE_ASSIST` / HEADSETHOOK hold → `onVoiceAssist` →
   `requestSiri`) stays wired for head units that route it, but did not fire on the emulator.

**GM head unit (CT5 image under `/tmp/ct5_aaos`, read-only).** The steering-wheel voice button is a
plain `KEYCODE_VOICE_ASSIST`: `system/usr/keylayout/gminfo3-virtual.kl` `key 216 VOICE_ASSIST SWC`
(and `Vendor_18d1_Product_0200.kl` `0x246 VOICE_ASSIST`, `0x247 ASSIST`). GM ships Google's
`com.google.android.carassistant` (`GsaVoiceInteractionService`) as the assistant with per-brand RROs
(`chevrolet.vcd_chevy_ff.com.google.android.carassistant.apk` for CHEVROLET_12, etc.); no GM input
service or `config_customInputService` overlay was found (only SystemUI/CarSettings theme RROs). So on
a GM unit a third-party foreground app receives NOTHING from the wheel button (same as B1): the key goes
`CarInputService` → `AssistUtils` → Google Assistant. It reaches Carlink only via route 2, which needs
the "Digital assistant" picker; whether GM's CarSettings exposes it is UNVERIFIED (AOSP's
`AssistantAndVoiceSettingsActivity` exists in CarSettings; GM's theme RRO could hide the entry). The
touch route A works regardless.

**Box state surfaced** (`OcbmClient.onProjMode`/`onBoxHealth`, `lastProjMode`/`lastBoxHealth`/
`boxHealthKnown`; `CarlinkManager.projectionMode`/`boxHealth`, `BoxStatusListener`; dashboard line
`BoxStatusLine`). Box-verified: `<< PROJ_MODE NONE`, `<< BOX_HEALTH HCI|SSP|btd|hostapd|rootfs-ok`
parsed and rendered as "NONE · HCI|btd|hostapd|rootfs-ok" under the status text. Both are re-emitted by
the box on every SUBSCRIBE, so every teardown path resets them to unknown (`clearSessionUiState`).

**Telephony** — `OcbmClient.sendTelephony(index: Byte): Boolean` (`[INPUT_TELEPHONY][index]` on
CH_INPUT, `Ocbm.TEL_*`) exposed as `CarlinkManager.sendTelephony(index: Byte): Boolean`; wire bytes
pinned by `OcbmShellInputTest`. `callui/CallNotificationHost` posts a `Notification.CallStyle` card
(channel `carlink_calls`, id 1002 — distinct from the media FGS slot 1001) for a ringing/ongoing iPhone
call from the iAP2 `callState` records (`callui/CallState`, reduced by `CallRoster`), with
Answer → `TEL_ANSWER`, Decline/Hang up → `TEL_END` through a non-exported dynamic receiver.
Android 14+ admits CallStyle only from an FGS or with a `fullScreenIntent`; we set the latter
(`USE_FULL_SCREEN_INTENT` declared; if the OS refuses, the IAE is caught and a plain card with the
same actions is posted). Robolectric-proven (`CallNotificationHostTest`: template, ongoing flag,
full-screen intent, action → HID index); NOT seen on a device — no phone, no call.
`CarlinkManager.addCallStateListener { presentation -> }` is the hook for the audio path.

**`CarAppFocusManager`** (`car/AppFocusClaimer.kt`): NAVIGATION focus is requested while iAP2
`routeGuidance.routeGuidanceState != 0` and abandoned at 0 / teardown — the permission-free way to make
this app the platform's nav-context owner and tell a native maps app it lost the road.
`VOICE_COMMAND` focus is deliberately NOT claimed: nothing in the AOSP car stack acts on it for a 3P
app, and the only "Siri is listening" edge we have (the mic gate) also fires for calls. Unverified
end-to-end (needs a phone navigating); on the emulator `Car.createCar` + `getCarManager` succeed.

**The seam decision (media session vs. call card vs. `VoiceRouter` focus).** Three rules:
1. The MediaSession mirrors iOS NowPlaying ONLY. A call never touches it — iOS pauses its own player
   and reports `playbackStatus 2`, which we mirror; forcing a pause here would desynchronise the card
   from the phone, and forcing play after a call would guess. The media lane's audio focus
   (`MediaAudioPlayer`, was `AacPlayer`) is likewise untouched by call state.
2. Call audio focus belongs to `VoiceRouter`'s `Purpose.CALL` lane (USAGE_VOICE_COMMUNICATION,
   GAIN_TRANSIENT, 3 s idle) and is driven by AUDIO ARRIVING, never by `callState` metadata — the
   metadata can lag or be absent (a wired-only phone), and a lane that opened on a rumour would hold
   the telephony context with nothing to play (the stuck-knob bug). WP2 added no new lane and no new
   focus request; the call card adds none either.
3. The call card is the only thing keyed on `callState`, plus one input decision: a HEADSETHOOK short
   press answers while `callPresentation` is `Incoming`. `clearSessionUiState()` drops the card and the
   nav-focus claim on every teardown path (stop, reboot, error), posted to main.
   Net effect: a call cannot leave the media session in a wrong state because nothing writes to it
   on call events, and cannot leave the focus stack wrong because the only focus holder for call
   audio is the lane that also releases it on silence.

**Launch-time display detection** (`util/WindowMetricsCompat.kt`: `DisplayProfile`, `SafeAreaMath`,
`PanelGeometry`; `ocbm/VehicleConfigYaml.kt`: `PanelRule`, `ViewAreaRule`). Three rectangles, never
conflated: the PHYSICAL panel (`maximumWindowMetrics`), the APP WINDOW (`currentWindowMetrics` —
equal to the panel only when fullscreen), and the CONTENT AREA (window minus the system bars the
`DisplayMode` keeps visible). The content area is read from the platform at launch (after the decor
view is attached — earlier the insets come back all-zero) and is the source of truth for the
CT_SUBSCRIBE config, the decoder size and the surface rect; `AdapterConfig` only fills in when no
profile exists. The display mode (`ui/settings/DisplayModePreference.kt`: `SYSTEM_UI_VISIBLE` /
`STATUS_BAR_HIDDEN` / `FULLSCREEN_IMMERSIVE` / `NAV_BAR_HIDDEN`, ints persisted — never renumber)
is a user choice from the dashboard footer, default Fullscreen everywhere (= the previous hard-coded
behaviour; no head-unit identity check). Changing it rebuilds the session exactly like a panel
change; the cutout/waterfall insets are re-expressed relative to the content area. `maxFps` snaps to 30/60 (round first, so 59.94 is 60; 50 Hz is 30).
Safe area = panel minus max(cutout, waterfall, corner-arc inset), origin rounded UP to even and far
edges DOWN, dropped back to the full panel if below the product floor. Legality ported from the macOS
`PanelRule`/`ViewArea2Rule`: floor 800x480 landscape / 480x800 portrait by the panel's OWN aspect,
3840 max side, even/odd, containment, positivity — the spec's `init` refuses anything iOS tears a
session down for. Measured on the running emulator with the box attached: launch reads
`2400x960@60.00Hz 160dpi (16.2") -> panel 2400x960@60 safe 2400x960@0,0`; `wm size 1280x720` fires
`onConfigurationChanged` → `panel changed 2400x960 -> 1280x720 — rebuilding session` → `[CONFIG] panel
1280x720@60 ... (detected)` → a fresh `CT_SUBSCRIBE`; `wm size reset` brings it back the same way
(emulator left at 2400x960 / 160). A portrait 800x1280@120dpi panel reads as a legal portrait panel
(floor 480x800). Cutout/waterfall/corner arms are JVM-proven only (`DisplayProfileTest`) — no
available panel has any.

### Three rectangles — a real defect found and fixed (UI pass, 2026-09-18)

WP1's `DisplayProfile.detect()` (paragraph above) originally measured `currentWindowMetrics.bounds`
— the app WINDOW — for everything, including what went out in `CT_SUBSCRIBE`. Because the app was
hardcoded immersive at the time, window = panel = surface, so it was self-consistent only by
accident: nothing forced window and content area to be the same rectangle once a `DisplayMode` other
than fullscreen existed. `util/WindowMetricsCompat.kt`'s `DisplayProfile` (`:132-273`) now keeps
three rectangles distinct: `physicalWidthPx`/`physicalHeightPx` (`maximumWindowMetrics`),
`windowWidthPx`/`windowHeightPx` (`currentWindowMetrics`), and `widthPx`/`heightPx` — the CONTENT
AREA, window minus the stable insets of whichever bars the active `DisplayMode` leaves visible. **The
content area, not the window or the panel, is what `CT_SUBSCRIBE` carries**, because it is the
rectangle the CarPlay surface actually occupies and therefore the basis for `INPUT_TOUCH`'s 0..65535
normalisation; cutout/waterfall insets are re-based relative to it, not the panel.

`surfaceInsets` (`:224-236`) is bars-plus-a-parity-pixel so the rendered surface IS the declared
panel — added after this was measured wrong on the real box in portrait: content came out
800x**1151** while the declared/CT_SUBSCRIBE panel was 800x1150, a one-pixel-tall SurfaceView
mismatch that the earlier window-only measurement could not see because it never had two
independent numbers to disagree.

Measured on the live box, 2026-09-18:

| mode | physical | bars | content → `CT_SUBSCRIBE` |
|---|---|---|---|
| Fullscreen 2400x960 | 2400x960 | 0 | 2400x960 |
| System bars visible 2400x960 | 2400x960 | T76 B96 | 2400x788 |
| Fullscreen portrait | 800x1280 | 0 | 800x1280 |
| System bars visible portrait | 800x1280 | T57 B72 | 800x1150 |

**Portrait verified on a real 800x1280 / density 120 AVD**, density read from the platform (120, not
assumed 160): `Display.Mode` 60.000004 Hz snaps to 60, and `[CONFIG] panel 800x1280@60 ... dpi=120
diag=12.6" (detected)` is emitted immediately before `subscribe()`. The `PanelRule`/`ViewAreaRule`
ported from the macOS `VehicleConfig.swift` (referenced in the paragraph above) ACCEPT a portrait
panel — floor 480x800 by the panel's own aspect, no landscape clamp — which is worth recording
because those rules were originally written and tested only against wide landscape panels. iOS
accepted the pushed portrait config and rendered a 4-column portrait CarPlay home screen; a tap on
the CarPlay tile through the resulting non-immersive, bar-offset surface produced `[UI_NAV] Host UI
requested`, confirming the touch basis (content area, not window or panel) end to end on real
hardware with a real iPhone, not just the emulator.

### Dashboard layout (2026-09-18) — resolution- and orientation-agnostic redesign

> **2026-09-25:** the in-screen dashboard was replaced by the carlink_native screens (see "UI — carlink_native
> screens on the OCBM stack"). `WindowLayout` survives and now arranges the Settings › Control cards (two-pane vs
> stacked); the rest of this section is the historical rationale.

`ui/adaptive/WindowLayout.kt` (new file) drives the dashboard from `androidx.window:window-core:1.5.0`
(`app/build.gradle.kts:169`) — chosen over the older `material3-window-size-class`, which is
deprecated in favour of `androidx.window.core.layout.WindowSizeClass`. `rememberWindowLayoutInfo()`
derives the class from `LocalWindowInfo.containerSize`, so an in-process `wm size` / rotation / split
re-lays the dashboard without an Activity restart.

Arrangement: `DashboardArrangement.TWO_PANE` at width ≥ 840 dp AND landscape; `STACKED` otherwise —
portrait always stacks regardless of width, and **height never picks the arrangement**: a short
window keeps its arrangement and scrolls instead of collapsing to a column (`WindowLayout.kt:47-70`,
unit-tested in `app/src/test/kotlin/com/carlink/ui/adaptive/WindowLayoutTest.kt`). In `TWO_PANE` the
content block keeps the gminfo look — `max(70% of width, 1100dp)` — with the adapter column at
`(0.26·content).coerceIn(300, 420)` dp, replacing a `weight(0.24)` that used to collapse to ~170dp at
1024x600.

`PhonesTab`'s old `CARD_WIDTH` (360.dp, explicitly tuned to gminfo37 and previously cited at
`PhonesTab.kt:58-66`) is replaced by `AdaptiveGrid` (`WindowLayout.kt:116-`), a custom NON-lazy
`Layout` giving `GridCells.Adaptive` semantics with 200..280dp cells. Non-lazy is deliberate, not an
oversight: `AdaptiveGrid` lives inside a `verticalScroll` column, and `LazyVerticalGrid` demands a
bounded height there — it crashes on the unbounded height a `verticalScroll` gives it.

Every fixed `height(ButtonMinHeight)` is now `heightIn(min = ButtonMinHeight)`, and both arrangements
are wrapped in `verticalScroll` (centred when the content fits without scrolling). The version pill —
previously overlaid on the cards at `Alignment.BottomEnd`, where it could sit on top of content — is
now in a footer row instead. The single-panel-only prose this replaced was removed from
`MainScreen.kt`, `PhonesTab.kt:58-66`, and a stale "788 usable height" comment in
`MainActivity.onCreate` (that number was this same gminfo hardcode, from the old single fixed-panel
assumption).

Screenshot-verified at 2400x960/160, 1920x720/160+320, 1280x720/160, 1024x600/160+240, 800x1280/160
on the reshaped landscape AVD and on the real portrait AVD (800x1280/120). `MainActivity` keeps the
broad `configChanges` in full (`AndroidManifest.xml:67-77`, documented in the manifest comment, no
`screenOrientation`): a system-driven recreate on rotation/resize would tear down the live USB
session and the HWC plane. `onConfigurationChanged` now compares `geometry` AND `barInsets`, not
geometry alone, so a bar/inset change (e.g. a `DisplayMode` switch with the panel unchanged) rebuilds
the session even when the panel dimensions are identical.

`dpi`/`diagonalInches` are detected and logged but deliberately NOT emitted: `vehicle_config.rs` has
no slot for either, the receiver's `/info` hard-codes `widthPhysical: 0`, and the macOS host's own
field help says `diagonalInches` is not sent to either phone (its `dpi` is an Android Auto field).
`CMD_VIEW_AREA` (0x11) is now DEFINED in `OcbmProto.kt`, closing one of `:app`'s two
`proto_check.py` gaps (the other, `F_BOTH`, is unrelated), but the mid-session push is NOT wired —
a panel/mode change rebuilds the session instead of pushing a live view-area update; this app still
declares a single view area with `enablesViewAreas: false`. `WindowMetricsCompat`'s old
window-only-measurement code path is now callerless, superseded by the three-rectangle version above.

**Also found today, unrelated to layout:** the launcher icon assets are byte-identical (same md5,
both foreground and background/monochrome layers) to `carlink_native`'s — this app currently ships
the older app's icon, distinct from the OEM CarPlay-tile icon described under "OEM icon + the return
path" above, which is a separate asset pipeline.

**Call-audio plumbing wired for WP2 — Android Auto lane (see the scope correction below):**
`CT_UPLINK` byte 7 (codec) parsed into `onUplinkGate(on, rate, ch, codec)` / `uplinkCodec`;
`CarlinkManager.onMicGate` restarts capture on a format change (CVSD→mSBC keeps 16 kHz, so the old
early-return would have left PCM on a wideband socket) and drains through
`MicrophoneCaptureManager.drainUplink`. Not hardware-verified (no phone call).

## Phone-call audio — the Android Auto HFP telephony lane (WP2, 2026-09-18 — NOT yet hardware-verified)

> **Scope correction, 2026-09-18.** This work package was commissioned on the mistaken belief that
> `SEAM_PKT_PLAIN` carries CarPlay call audio. It does not. **CarPlay hands every audio stream —
> media, Siri, telephony — to the accessory inside the AirPlay/WiFi session; it never rides SCO/HFP**
> (`crates/vendor/wireless/src/sco_audio.rs:2-6`, `docs/wireless/01_BT_AND_RADIO.md:690-697`). The
> `:9112` mic-seam listener and the HFP link `SEAM_PKT_PLAIN` carries are gated to an **Android
> Auto** projection owner, wired or wireless — `owner == ProjectionOwner::WirelessAa || owner ==
> ProjectionOwner::WiredAa` (`crates/vendor/wireless/src/sco_audio.rs:1181-1183`) — because gearhead
> routes calls AND the Assistant through the connected Bluetooth headset, not this box's projection
> link (`crates/vendor/wireless/src/hfp_hf.rs:28-36`). So everything below is a real, tested `:app`
> capability, but it serves **Android Auto's** telephony lane, not this document's own CarPlay/OCBM
> architecture — a scope mismatch worth carrying forward rather than quietly correcting away, since
> the code and its tests are sound and simply were not the path this session was asked for. CarPlay
> phone calls on this box continue to ride the AirPlay session exactly as before, unaffected by
> anything in this section.

Before this, the Android Auto call-audio lane did not decode at all on `:app`'s side: `AudioSeam.handle`
matched only the three encrypted-seam markers, so the box's `SEAM_PKT_PLAIN` (`0x03`,
`crates/ocbm-proto/src/lib.rs` `SEAM_PKT_PLAIN` / `SEAM_CODEC_MSBC`) was silently dropped; and every
voice stream reaching `VoiceRouter` was fed to an AAC-ELD decoder regardless of codec. Landed, all in
`:app`:

- **`SEAM_PKT_PLAIN` in `ocbm/seam/AudioSeam.kt`.** `[scid 8 LE][payload]`, no key, no RTP. Under a
  PCM `SEAM_FORMAT` (`btd`'s narrowband: 8 kHz mono S16 **little-endian**, 320 B / 20 ms) the bytes
  pass through untouched. An unknown marker is skipped by its length prefix (bounded at 64 KiB so a
  false magic in ciphertext cannot swallow the lane) and is never a desync.
- **mSBC decoder, `telephony/Msbc.kt` + `MsbcFramer.kt`** — a line-for-line port of the macOS
  `MSBCCodec.swift` / `MSBCFramer.swift`, pinned by the same reference vectors (encoder reproduces the
  recorded bitstream byte-for-byte; decoder matches ffmpeg's fixed-point synthesis within 8 LSB).
  `AudioSeam` keeps one `MsbcTelephonyDecoder` per scid; it resynchronises on the H2 header
  (`0x01` + `08/38/C8/F8` + `0xAD`), never on message length, conceals sequence gaps, and writes
  NOTHING to the voice pipe unless a frame decoded — the bitstream is never rendered as PCM.
- **Codec carried through** (`ocbm/seam/VoiceTag.kt`, 12-byte tag). `VoiceRouter` now branches:
  `CODEC_PCM` → S16LE straight to the `AudioTrack` (no MediaCodec); `CODEC_AAC_ELD` → the existing
  ELD path; anything else is dropped with a warn-once naming the codec, BEFORE focus is requested.
  PCM in the tag is always little-endian: the seam byte-swaps the big-endian AirPlay PCM downlink
  (`SEAM_PKT`, codec 0) and passes HFP PLAIN / mSBC output through. Call audio of every flavour lands
  on the existing `Purpose.CALL` lane (`USAGE_VOICE_COMMUNICATION`); no new lane.
- **mSBC uplink** (`audio/MicProfile.kt`, `audio/MicrophoneCaptureManager.kt`). `start(decodeType,
  codec)` honours the `CT_UPLINK` codec byte: codec 4 forces 16 kHz mono capture and arms an
  `MsbcUplinkEncoder`; `drainUplink(maxPcmBytes, send)` cuts the ring buffer into whole 240-byte
  frames and sends each as its OWN 60-byte `CH_MIC` message (the box writes one payload per SCO
  write). The 7.5 ms cadence rides the 20 ms timer with the remainder carried in the ring buffer.
  An unknown codec REFUSES to capture rather than uplinking PCM the far end would hear as noise.

**Integration IS wired** *(corrected 2026-09-18 — this paragraph was written mid-session, while WP2
still owned only the seam/audio half and WP1 had not yet landed its side; it read "deliberately not
wired" and was already false by the end of the same session)*. All three hand-off points exist:
`CT_UPLINK` reads the codec byte when `len >= 8` (default 0) and `onUplinkGate` carries it
(`ocbm/OcbmClient.kt:92,443`); `onMicGate` restarts capture when
`microphoneManager?.matchesFormat(rate, channels, codec) == false` and sizes the drain from
`MicProfile.captureFormatFor`/`uplinkDrainBytes` (`CarlinkManager.kt:1815,1837,1850`); and
`sendMicrophoneData` drains through `drainUplink` (`CarlinkManager.kt:1907`).

The renegotiation guard is the load-bearing part: a CVSD→mSBC switch keeps the rate at 16 kHz, so an
early-return on "already capturing" would leave a raw-PCM uplink running on a wideband SCO socket —
silent corruption that reports no error anywhere.

**Verified by test (JVM):** 320-byte narrowband framing and byte order; PLAIN split across feeds;
unknown-marker skip (and the bounded-length resync); H2 resync across 37-byte and 30/90-byte splits,
false `0x01 0x08` pairs, a header split across pushes, loss from the sequence gap, junk and the 4 KiB
cap; drop-don't-render for an mSBC-format payload that never frames and for an undecodable codec;
big-endian AirPlay PCM swap; uplink encode → downlink decode round trip at > 20 dB SNR with the 7.5 ms
packets riding 100 ticks of the 20 ms timer without drift. Each of the endianness swap and the H2
syncword check was mutated and confirmed to fail its test.

**Unproven on hardware:** whether `btd` really emits the documented shapes at call time; the AAOS
`USAGE_VOICE_COMMUNICATION` track from a third-party app on the gminfo call bus; AudioTrack at 8 kHz
mono on that HAL; mSBC decode CPU on the Atom under a live call; the SCO write accepting one 60-byte
`CH_MIC` message per packet; echo/latency of the HFP round trip.

## Silent media after Siri — focus release, one volume authority, resume fade-in (2026-09-25, chevy12-measured)

**Root cause (confirmed).** `MediaFocus` (the `carlink_native` port in `MediaAudioPlayer`) maps
`AUDIOFOCUS_LOSS_TRANSIENT` to gain 0.0 and the effective volume was `min(duckGain, focus.gain)`; the
Siri sink (`USAGE_ASSISTANT`, `GAIN_TRANSIENT`) kept its focus until `VoiceRouter`'s 15 s idle sweep. So
"media restored to 1.0" on the duck path was masked by focus 0.0 until the sweep — 11–15 s of silent
media per turn, 32.7 s with two turns in the user's log (02:17:34.96 stream resumed → 02:18:07.65 gain
1.0). The old `AacPlayer` had no focus listener, so this was a regression of the 2026-09-25 media rewrite.

**Fix.**
- One volume authority: `MediaAudioPlayer.applyGain` sets `MediaGain.effective(focus, duck)` and logs
  both (`media gain 0.0 (focus 0.0, duck 1.0, ramp -)`). `min`, not a product: both are "duck to 0.2"
  for the same nav event (product = 0.04, inaudible); the resume ramp is applied to the PCM samples,
  so effective = focus × duck × ramp with the ramp factor in the samples.
- Transient focus is released on the real end-of-Siri signals (`VoiceRouter.releaseQuietSinks`):
  (1) audible media arriving while focus still suppresses it (`MediaFocus.noticeAudible` →
  `onAudibleWhileSuppressed`, sink quiet ≥ 250 ms) — decisive, iOS never resumes audible media inside a
  turn; (2) `UPLINK OFF` + `TransientFocusPolicy.UPLINK_GRACE_MS` = 1 s (a re-open inside the window
  keeps the held focus); (3) "assistant done" (4 s hold) but only when the uplink is closed; (4) the
  idle sweep as backstop — ASSISTANT 15 s → 4 s, NAV 8 s → 2 s (may-duck focus kept media at 0.2 for the
  whole window after every prompt), CALL/ALERT 3 s unchanged; uplink purposes use a 15 s backstop while
  the gate is open (a lost UPLINK OFF). Grace justification: measured gaps — UPLINK OFF → media stream
  resumed 0.15 s (this build) / 0.9 s (user log); UPLINK OFF → next user-initiated turn 7–9 s, i.e. a new
  session, where a fresh focus request (media audible in between) is the native behaviour.
- Nav: same end-of-stream release via the 2 s idle; the duck→restore stays a `setVolume` step (0.2 → 1.0,
  no ramp). Calls: `UPLINK OFF` + grace and media-audible release, 3 s idle backstop.

**Measured (three Siri cycles each, `input swipe 80 932 80 932 1500`, not speaking).** Before: 11–35 s.
After: media gain 1.0 within 59–64 ms of the first post-gap media frame (`stream resumed after a 6.6 s
gap` 02:31:27.777 → `siri: released (media audible again)` .835 → `media gain 1.0` .836), 202–215 ms
after `UPLINK OFF`; `dumpsys audio` 12 s after the trigger lists only `MediaFocus gain: GAIN USAGE_MEDIA`.
Back-to-back (second trigger 6 s later): each turn recovers the same way; media is audible between turns.

**Resume fade-in (`av/ResumeRamp.kt`).** After a HARD interruption (Siri, or focus 0 = exclusive
transient/permanent loss) with media active in the 2 s before it, the first media frame after the
track's ≥1 s gap (its re-prime point) starts an equal-power ramp `sin(π/2·p)` — 0 / 0.383 / 0.707 /
0.924 / 1.0 at 0/25/50/75/100 % — over `ResumeRamp.DURATION_MS` = 1000 ms, applied to the S16LE samples
in the write path (sample-accurate, independent of `AudioTrack.setVolume`, same code for wired PCM and
decoded AAC). Not armed by nav (soft duck), session start, plain underrun/network gaps or a user
pause/play — none of those call `interrupted()`. A new interruption mid-ramp cancels it and the restart
continues from the level reached (no dip); a chained interruption while media is already silenced keeps
the arm (the within-turn focus flap disarmed it in the first build). Measured envelope, cycle 1:
`UPLINK OFF` 02:36:13.883 → `resume ramp START from gain 0.00` 14.031 (+148 ms, same instant as the
first post-gap frame) → 25 % 0.47 @+303 ms → 50 % 0.79 @+574 ms → 75 % 0.96 @+815 ms → `END after 1062 ms`.
Cycles 2/3 identical (1049 ms; 0.51/0.79/0.96 and 0.40/0.75/0.96). Back-to-back: the second turn
cancelled the ramp at 1.00 and the next resume restarted "from 1.00, 2 ms" — no second fade, by the
restart-from-level rule. Nav prompt not exercised on the emulator (no route driven); excluded by
construction and by `ResumeRampTest`.

Tests: `AudioFocusPolicyTest` (gain composition, uplink grace once/cancel, measured-gap bound,
`uplinkOn`), `ResumeRampTest` (curve, positive trigger with sample envelope, excluded cases,
cancel/restart from level, chained arm, one-shot).

## Display cutout → CarPlay safe area (2026-09-25, chevy12-measured)

**What the app already did.** `DisplayProfile.detect` reads the framework's computed cutout insets
(`WindowInsetsCompat.getInsetsIgnoringVisibility(displayCutout())`, plus waterfall insets and
`RoundedCorner` radii — never the raw `DisplayCutout.boundingRects`), `SafeAreaMath.derive` turns them
into the largest EVEN-aligned rectangle inside the insets (origin rounded up, far edges down, so the
rect can only shrink), `ViewAreaRule` refuses anything iOS would tear the session down for, and
`VehicleConfigYaml` pushes `viewAreas[0] = {viewArea: full panel, safeArea: that rect}`. The stream and
touch space are the FULL content area (2914x1134 on chevy12); only the safe rect is inset. The receiver
(`vehicle_config.rs` `safe_area_inset` → `info.rs` `view_areas`) already forwarded it as
`displays[0].viewAreas[0].safeArea{originXPixels…}` and armed `viewAreas` in `enabledFeatures` — the
same path the macOS host's `SettingsWindow.swift` `va()` uses, same keys, same units (panel pixels,
absolute rect).

**What was wrong.** `VehicleConfigSpec.drawUIOutsideSafeArea` was never set (always `false`). On this
panel iOS then honoured the safe rect but painted the inset bands BLACK — measured on the chevy12 AVD
(2914x1134, GM CHEVROLET_12 cutout, framework insets top 167 / right 285 → pushed safe
`2628x966@0,168`): top band `y<168` and right band `x>=2628` were 100 % pure black (`/tmp/chevy12_before.png`).
`docs/carplay/06_AV_PIPELINE.md` had already recorded on hardware (2026-09-09) that this flag is exactly
what lets CarPlay render the wallpaper into the band.

**The change (app only, no box change).** `PanelGeometry.drawUiOutsideSafeArea` is `true` whenever the
declared safe area is a real inset and `false` for a full-panel safe area (that document stays
byte-identical); `CarlinkManager.vehicleConfigSpec()` copies it into the YAML. Tests:
`DisplayProfileTest` (`a cutout turns drawUIOutsideSafeArea on and a clean panel leaves it off`, the
chevy12 numbers), `VehicleConfigYamlTest` (pre-existing render of `drawUIOutsideSafeArea: true`), and
receiver `info.rs` `pushed_draw_ui_outside_safe_area_reaches_the_main_safe_area_both_ways` (YAML →
`DeviceConfig.main_draw_outside_safe` → plist `safeArea.drawUIOutsideSafeArea`, true/false/absent). The
receiver source is unchanged apart from that test, so the deployed box binary needs no rebuild.

**Measured after** (`/tmp/chevy12_after.png`, same session type, user 10, `PROJ_MODE WIRED_CP`,
STREAMING): pushed `[CONFIG] panel 2914x1134@60 safe 2628x966@0,168 drawUIOutsideSafeArea=true`;
right band `x>=2628` dark fraction 0.000 (was 1.000), top band `y<168` 0.219 (wallpaper's own dark
regions; sample pixels (16,15,27)/(14,14,24) vs (1,0,1) before); interactive UI (near-white pixels =
status clock, icon labels, page dots) bounding box `x 28..2372, y 218..1110` — entirely inside the safe
rect, zero near-white pixels in either band, in both screenshots. So: wallpaper edge-to-edge under the
cutout, UI in the safe area, touch unchanged (full-panel space).

**Curve vs rectangle.** CarPlay's safe area is one rectangle per view area; the GM cutout is curved.
The rect is the framework's computed safe insets (the cutout's bounding extent), so the region between
the curve and the rect is wallpaper only — nothing interactive can land there. `cornerMasks` (the
other CarPlay mechanism for shaped panels) is mutually exclusive with `safeArea` per display and
paints opaque corner bitmaps; it does not describe a top/right notch, so it is not used. A second view
area (narrower alternate layout) is a resize feature, not a shape hint, and is not pushed.

**Runtime changes.** Detection runs at launch, in `MainActivity.onConfigurationChanged` (a changed
`geometry` — which includes the safe rect — rebuilds the session with a fresh `CT_SUBSCRIBE`), on a
display-mode change and on Reset Connection (`reinitialize()` → `initializeCarlinkManager()` →
`DisplayProfile.detect`). An overlay toggle that changes the cutout therefore takes effect on the next
session; there is no live mid-session safe-area update (CarPlay's `updateViewArea` switches between
declared areas, it does not redefine one).

**Both display modes honour it (chevy12, 2026-09-25).** The content area already excludes the system
bars, so the cutout's share that the bars cover drops out of the safe inset on its own:
- Immersive / fullscreen: `panel 2914x1134@60 safe 2628x966@0,168 drawUIOutsideSafeArea=true`.
- System UI visible (bars top 95 / bottom 120): `panel 2914x918@60 safe 2628x846@0,72
  drawUIOutsideSafeArea=true`. The top inset is 167 − 95 = 72 and the right inset stays at 286. CarPlay
  rendered accordingly (user-observed).

**Reproducing the chevy12 test bench.**
- `chevy12` is an AVD cloned from `ultrawide` (android-35-ext15 automotive arm64) with
  `hw.lcd.width=2914`, `hw.lcd.height=1134` and `hw.lcd.density=200`. Launch it with `emu_cpc chevy12`,
  and only one box-passthrough emulator may run at a time.
- A config clone alone is not enough. The clone needs ultrawide's writable-system layer
  (`system.img.qcow2`, which carries `android.hardware.usb.host`). Without it `dumpsys usb` has no
  `host_manager`, and the box shows up in the guest's sysfs but never reaches the app.
- `adb install` installs for user 0 only. The driver user is 10, so after installing also run
  `pm install-existing --user 10 zeno.carlink.ocbm`.
- The cutout is an RRO carrying GM's CHEVROLET_12 `config_mainBuiltInDisplayCutout` string verbatim,
  taken from a GM AAOS 14 image. It must be signed with the platform test key, or idmap2 maps nothing.
  The `fill` variant is the one to use on the emulator.

**Provenance.** From `carlink_native`: nothing new to port — its `WindowMetricsCompat` was only the
API-29 compat shim for `currentWindowMetrics`/stable insets (this app's `DisplayProfile` is the evolved
form: content area, cutout/waterfall/corner, parity, legality rules), and its riddleBox "safearea" was
a fixed 100/50 px blob for the adapter's NAVISCREEN, unrelated to CarPlay `viewAreas`. From MacHost:
the YAML shape and semantics (`va()`: full-frame `viewArea`, absolute-rect `safeArea`,
`drawUIOutsideSafeArea` as a sibling key; `FieldInfo`: "wallpaper is displayed outside the safe area,
replacing the normal black background"). Disagreement: MacHost leaves `drawUIOutsideSafeArea` a
user toggle defaulting to `false` and does not even-align the safe rect (`max(1, …)` clamping only);
this app derives the flag from the inset and keeps the even rule — followed the hardware finding for
the flag and the parity teardown rule for the rect (1 px more conservative than the raw insets).

## Audio/video format matrix (2026-09-25 — wired CarPlay media PCM path landed; see verification status at the end)

The stream matrix below is built from what the box actually advertises and forwards
(`crates/vendor/receiver/src/info.rs` `preset_wired_pcm` / `preset_wireless_8`, `session.rs` SETUP
phase-2 audio → `SEAM_FORMAT` `[codec][rate][ch][bits][atype]`, `uplink.rs`) and from what `:app`
consumes. Rows marked *(outside knowledge)* come from the CarPlay/AA protocols, not from this repo.

**How the seam names a stream.** Every audio access unit reaches the app tagged with the SETUP
`audioType` (`atype`: 0 media, 1 telephony, 2 speechRecognition, 3 alert, 4 default/absent,
5 compatibility) and the negotiated codec (`SeamCrypto.CODEC_*`: 0 PCM, 1 AAC-LC, 2 AAC-ELD, 3 Opus,
4 mSBC — the last only from `btd`, decoded in `AudioSeam`). `AudioSeam` routes atype 0/5 → media lane,
1-4 → voice lane; `VoiceRouter` maps atype → purpose (1 CALL, 2 ASSISTANT, 3 ALERT, 4 → NAV when
≥44.1 k stereo else ASSISTANT). The stream type (100 main, 101 alt, 102 media-buffered) is NOT
forwarded — it is implied by the (atype, format) pair, which is why atype 4 needs the format to split
Siri (16 k mono) from alt-audio/nav (48 k stereo).

### CarPlay

| Stream | Transport | Wire format (box advertises) | atype / codec | App path | AudioAttributes usage | Focus | Status |
|---|---|---|---|---|---|---|---|
| Media (music, podcasts) | wired | PCM S16 **BE** 48 kHz stereo, type 100 catch-all (`pcm_48k_stereo`) | 0 / 0 | `AudioSeam` byte-swaps → `MediaAudioPlayer` → `PcmPassthrough` → `AudioTrack` | `USAGE_MEDIA` / `CONTENT_TYPE_MUSIC` | `AUDIOFOCUS_GAIN`, held for the session; software duck 0.2 from the voice lane; pause (not duck) while Siri speaks | **ADDED 2026-09-25** (was dropped: "AacPlayer consumes ADTS AAC-LC only") |
| Media | wireless | AAC-LC 48 kHz stereo, type 102 `media` (`aac_lc_48k_stereo`) | 0 / 1 | `MediaAudioPlayer` → `MediaCodecAacDecoder` (csd-0 `0x1190`) | same | same | supported (device-proven before this change as `AacPlayer`; now raw-AU tagged instead of ADTS — re-verify on wireless) |
| Media, PCM compatibility fallback | wireless | PCM 48 k stereo / 16 k mono, types 100/101 `compatibility` | 5 / 0 | media lane, same as wired PCM | same | same | added with the PCM path (never observed on device) |
| Alt audio / navigation prompts | wired | PCM 48 k stereo, type 101 (no audioType → `default`) | 4 / 0 | `VoiceRouter` NAV sink, direct PCM | `USAGE_ASSISTANCE_NAVIGATION_GUIDANCE` / `SPEECH` | `GAIN_TRANSIENT_MAY_DUCK`; ducks media to 0.2 on energy, restores 1.5 s after | supported (code path pre-existed; unexercised today, see verification) |
| Alt audio / navigation prompts | wireless | AAC-ELD 48 k stereo, type 101 `default` (`aac_eld_48k_stereo`) | 4 / 2 | NAV sink → `MediaCodecAacDecoder` (ELD csd synthesised for 48 k stereo) | same | same | supported (ELD decode path pre-existed) |
| Telephony (call downlink) | wired | PCM 16 k mono, type 100 catch-all (`pcm_16k_mono`) | 1 / 0 | `VoiceRouter` CALL sink, direct PCM | `USAGE_VOICE_COMMUNICATION` / `SPEECH` | `GAIN_TRANSIENT` (exclusive); released 3 s after the last audio | supported, unexercised |
| Telephony | wireless | AAC-ELD 16 k mono, type 100 `telephony` | 1 / 2 | CALL sink → ELD decoder (device-confirmed csd `f8f0312c00bc00`) | same | same | supported |
| Siri (speech recognition + `default` Siri downlink) | wired | PCM 16 k mono, type 100 | 2 or 4 / 0 | `VoiceRouter` ASSISTANT sink, direct PCM | `USAGE_ASSISTANT` / `SPEECH` | `GAIN_TRANSIENT`; keep-alive silence so the knob stays on the voice group; media paused | supported, unexercised |
| Siri | wireless | AAC-ELD 16 k mono, type 100 `speechRecognition` / `default` | 2 or 4 / 2 | ASSISTANT sink → ELD | same | same | supported (device-proven before this change) |
| Alerts | wireless | AAC-ELD 48 k stereo, type 100 `alert` | 3 / 2 | `VoiceRouter` ALERT sink | `USAGE_VOICE_COMMUNICATION_SIGNALLING` / `SONIFICATION` | `GAIN_TRANSIENT` | supported; bus mapping UNPROVEN on GM (docs/carplay/03) |
| Alerts | wired | not advertised (`preset_wired_pcm` has no `alert` entry) — iOS rides alerts on the 48 k stereo `default` stream | 4 / 0 → NAV | as alt audio | nav usage | may-duck | by design of the wired preset |
| Opus (`opus_16k/24k/48k_mono`) | either | expressible in a YAML `audio.formats` list only; **neither preset advertises it**, so no session negotiates it | any / 3 | `AudioDecoders.Path.UNSUPPORTED` → dropped with one diagnostic | — | — | not advertised, not decoded (deliberate: no way to exercise it) |
| Mic uplink (Siri, calls) | wired | box expects **PCM S16LE 16 k mono** on `CH_MIC` after a `CT_UPLINK` gate (rate/ch/codec carried); box converts to BE and encrypts (`uplink.rs`) | — / 0 | `MicrophoneCaptureManager` (`AudioRecord`, ring buffer, 20 ms ticks); `RECORD_AUDIO` checked at `hasPermission()`; `FOREGROUND_SERVICE_TYPE_MICROPHONE` | `AudioSource.VOICE_COMMUNICATION` (see `MicProfile`) | n/a | supported (hardware-verified 2026-08-15) |
| Mic uplink | wireless | box expects PCM 16 k mono and encodes **AAC-ELD itself** (`uplink.rs` `EldEncoder`, fdk) — the app never encodes | — / 0 | same | same | n/a | supported |
| Mic uplink (HFP, AA only) | BT | mSBC 60-byte eSCO packets via `CH_MIC` (`MicProfile.CODEC_MSBC`) | — / 4 | `MicrophoneCaptureManager` + `Msbc` encoder | same | n/a | JVM-tested only (WP2) |
| Screen video | either | H.264 (`avcC`) or HEVC (`hvcC`, only when the pushed `enablesHEVC: true` sets the box's `hevcInfo` lever) — iOS picks; max 2400x960@60 per the pushed geometry | — | `VideoSeam` → Annex-B → `VideoRenderer` (`video/hevc` csd-0 = VPS+SPS+PPS; `video/avc` csd-0 = SPS, csd-1 = PPS) | — | — | HEVC device-proven (emulator `c2.goldfish.hevc.decoder`, 2400x788); **H.264 ADDED 2026-09-25** (JVM-tested NAL walk; not yet exercised live — needs a session negotiated without HEVC) |
| Video codec advertisement | either | `accessoryConfig.enablesHEVC` in the pushed YAML | — | `VideoCodecs.probe(w,h,fps)` (`MediaCodecList.findDecoderForFormat`) → `VehicleConfigSpec.enablesHevc` | — | — | **ADDED 2026-09-25**; emulator probe: `h264=c2.goldfish.h264.decoder hevc=c2.goldfish.hevc.decoder -> enablesHEVC=true` |
| Instrument-cluster / alt-screen video (`CH_ALT_VIDEO`, `altVideoStreams`) | either | the box forwards `CH_ALT_VIDEO` and the config schema has `alt_video_streams`; the app pushes none | — | `VideoSeam` can parse it; **no consumer / surface** | — | — | missing — not advertised by this app, so never negotiated |

### Android Auto

| Stream | Wire format *(outside knowledge: AA protocol)* | App path | Status |
|---|---|---|---|
| Media | PCM 48 kHz stereo 16-bit (AudioStreamType MEDIA); AAC only if the HU offers it | none | **missing** |
| Guidance / navigation | PCM 16 kHz mono | none | missing |
| System / assistant audio | PCM 16 kHz mono | none | missing |
| Mic | PCM 16 kHz mono uplink; HFP mSBC/CVSD when a call rides the BT headset (this box's `SEAM_PKT_PLAIN` lane) | `MicrophoneCaptureManager` exists (mSBC/PCM) | partial — mic only |
| Video | H.264 baseline/main, 480p/720p/1080p @30/60; H.265 and VP9 only if the HU offers them | `VideoRenderer` could decode H.264 | missing |

**AA is not reachable with this app today, and not because of formats.** The box's `PM_WIRED_AA`
(`aa-bridge` AOAP pump) hands the AA *protocol* to the host over `CH_IP` (`IP_OPEN 127.0.0.1:5277`,
`ccpa/aa-bridge/src/appport.rs`): the head-unit side of Android Auto — the gearhead handshake, service
discovery, media/video channel setup — runs in the host app. The macOS host has that stack; `:app`
has none (`CH_IP` appears only as a constant in `OcbmProto.kt`). Until an AA client exists here, the AA
rows above are unreachable regardless of decoder coverage. The decode/playback layer built for CarPlay
(`AudioDecoders`, `MediaAudioPlayer`, `VoiceRouter`, `VideoRenderer`) is protocol-agnostic and would
be reused by it.

### What changed on 2026-09-25 (app side only; no box-side wire or negotiation change)

- **`av/AudioDecoder.kt`** — the one decode interface (`AudioDecoder`), `PcmPassthrough`,
  `MediaCodecAacDecoder`, `AacCsd` (LC + ELD AudioSpecificConfig builders, moved out of the players),
  `AudioDecoders` (codec → path dispatch, `canDecode`, `open`), `PcmLevel` (the shared -32 dBFS gate).
- **`av/MediaAudioPlayer.kt`** replaces `AacPlayer`: consumes `VoiceTag`-framed media, dispatches on
  the tag's codec, honours the tag's rate/channels (48 k stereo is only the prime). One `USAGE_MEDIA`
  track. `MediaFocus` holds `AUDIOFOCUS_GAIN`, maps focus changes to gain (CAN_DUCK 0.2, transient/
  permanent loss 0), and on a permanent loss asks the phone to pause (`MEDIA_BTN_PAUSE`) and re-requests
  once audible media resumes. `MediaTrack` carries the `carlink_native` discipline: 4x min buffer,
  `PERFORMANCE_MODE_NONE`, 80 ms pre-fill before `play()`, re-arm after a ≥1 s gap, `underrunCount`
  deltas logged, `ERROR_DEAD_OBJECT` rebuild, `THREAD_PRIORITY_URGENT_AUDIO` on the writer.
- **`AudioSeam`** — media lane now emits `VoiceTag` frames (PCM byte-swapped BE→LE; AAC-LC raw AU)
  instead of ADTS. The voice lane is unchanged.
- **`VoiceRouter`** — sinks decode through `AudioDecoders` (ELD csd/`openDecoder`/`peakExceeds` moved
  out; routing, focus, keep-alive, idle sweep untouched).
- **`av/VideoCodecs.kt` + `av/VideoRenderer.kt`** replace `HevcRenderer`: codec latched from the first
  parameter set (`sniff`), H.264 and HEVC csd handling, `probe()` for the advertisement.
- **`CarlinkManager`** — `vehicleConfigSpec()` sets `enablesHevc` from the probe; wires
  `MediaAudioPlayer(onFocusLost = pause the phone)`.
- Tests: `AudioDecodersTest`, `VideoCodecsTest`, `SeamTest` (media tag + wired PCM byte-swap),
  `VehicleConfigYamlTest` (`enablesHEVC` follows the probe).

### Ported from `carlink_native`, changed, and not carried over

Ported (measured on the GM head unit there): `AudioTrack` sizing (`bufferMultiplier` 4,
`PERFORMANCE_MODE_NONE` because `AUDIO_OUTPUT_FLAG_FAST` is denied to third-party apps), the per-purpose
usage → track mapping (`purposeToAttributes`: MEDIA/USAGE_MEDIA, PHONE_CALL/VOICE_COMMUNICATION,
SIRI/ASSISTANT, ALERT/VOICE_COMMUNICATION_SIGNALLING, NAVIGATION/ASSISTANCE_NAVIGATION_GUIDANCE — already
what `VoiceRouter.Purpose` uses), the focus gain types (GAIN for media, TRANSIENT for call/Siri/alert,
TRANSIENT_MAY_DUCK for nav — likewise already in place), the MEDIA focus listener's gain map, pre-fill
before `play()`, underrun accounting and recovery (re-arm pre-fill), dead-object recreation, URGENT_AUDIO
priority, and the mic capture manager (already ported earlier, 2026-08-15, with mSBC added).

Changed: no playback thread + `AudioRingBuffer` for output. `carlink_native`'s USB ingest thread must
never block, so a ring decoupled it; here `SeamPipe` (bounded, blocking) already decouples the OCBM
read thread from each consumer, and the consumer's blocking `AudioTrack.write` is the pacing — a second
ring would only add latency. Stream identity comes from the seam's per-stream `atype`/codec tag, not
from riddleBox `decode_type` heuristics (`AudioFormats.fromDecodeType`, zero-packet filter, nav
"end marker" / warm-up-noise skipping) — none of those apply to a format-tagged stream.

Not carried over (bugs in the reference): (1) `getOrCreateFocusListener` set `focusDuckLevel = 0` on
`AUDIOFOCUS_LOSS` and nothing ever re-requested media focus or told the phone, so media stayed muted
until the next `MEDIA_START`; (2) `AudioFormatConfig.bitDepth` was decorative — `ENCODING_PCM_16BIT`
regardless — whereas the seam's `bits` is honoured (a non-16-bit PCM stream is dropped with a diagnostic);
(3) nav packets were dropped for ~2 s after `NAVI_STOP` by counting consecutive zero packets, which
on a stream that carries digital silence by design would have eaten real prompts.

### Verification status (2026-09-25)

- Gate green: `bt ./gradlew :app:assembleSideloadDebug :app:detekt :app:ktlintCheck :app:testSideloadDebugUnitTest`;
  `detekt-baseline.xml` unchanged.
- Emulator-5554 after install: `[media] media audio focus: GRANTED`, `[media] AudioTrack 48000Hz 2ch
  buffer=36928B (min 9232) prefill=15360B`, `[video] stream codec: hevc`, `MediaCodec configured:
  video/hevc 2400x788 ... decoder=c2.goldfish.hevc.decoder`, and the probe line
  `decoders at 2400x788@60: h264=c2.goldfish.h264.decoder hevc=c2.goldfish.hevc.decoder -> enablesHEVC=true`.
- **Wired-CarPlay media PCM: PROVEN end to end (2026-09-25, emulator-5554 / chevy12, user 10).** The
  first relaunch after install dropped the iPhone off the box's USB bus (`PROJ_MODE WIRED_CP → NONE`,
  `SESSION_EVENT PHONE_ABSENT`; recovered only by the user re-plugging the phone). Once re-plugged:
  `[ocbm ] audio format scid=…: codec=PCM 48000Hz 2ch atype=0 -> media`,
  `[media] configured PCM 48000Hz 2ch -> AudioTrack(USAGE_MEDIA), decoder=direct PCM`,
  `[media] pre-fill complete: 15576B (~81 ms) buffered, playback started`, `[media] FIRST AUDIO FRAME PLAYED`,
  `[media] 32000 media frames played` with no `dropping` line. `dumpsys media.audio_flinger`, two
  samples 5 s apart: `Output thread … name AudioOut_D … Standby: no … Frames written: 11256192` →
  `11498112` (241 920 frames / 5 s ≈ 48.4 kHz). `dumpsys audio`: `pack: zeno.carlink.ocbm … client:
  …MediaFocus… gain: GAIN … attr: AudioAttributes: usage=USAGE_MEDIA content=CONTENT_TYPE_MUSIC`.
  Nav, Siri and telephony rows remain unexercised on this build (no route or Siri turn was driven).

## Mic path tests (2026-08-15)

22 tests over the two silent-failure surfaces: the `CT_UPLINK` gate coming down and `CH_MIC` PCM
going up. Neither reports an error anywhere when wrong — not on the box, not on the phone, not in
logcat — so a unit test is the only place they can be caught.

`audio/MicProfile.kt` was extracted from `CarlinkManager.captureProfileFor` to make the invariant
testable at all: the negotiated `(rate, channels)` has to survive a round trip through riddleBox's
`decodeType` table, which is what `MicrophoneCaptureManager` still keys capture off. When it does
not, capture opens at 16 kHz mono while the box believes otherwise and Siri hears a pitch-shifted
stream. `MicProfileTest` closes that loop through `MicFormats` rather than asserting the mapping in
isolation, so a table that is *self-consistently* wrong still fails. `decodeTypeFor` returns null
for an unmapped format rather than falling back silently — the fallback is now the caller's
decision, and it logs.

`OcbmMicUplinkTest` pins the gate's wire layout (`[CT_UPLINK][state u8][rate u32 LE][ch u8]`, built
by hand, with an explicit little-endian byte-order test), that an off edge clears the retained
format, and that a truncated gate is ignored rather than parsed from whatever bytes are present.
Uplink side: the subscribe gate, verbatim payload, offset slices, and the `MIC_CHUNK` splitter —
the inclusive boundary, lossless in-order reassembly, and 4-byte-aligned chunk boundaries so a
split cannot bisect a 16-bit stereo sample frame.

**Every test was mutation-checked** — the table entry, the byte order, the chunk boundary and the
`txLock` were each broken in turn to confirm the suite actually fails. That caught a bad test of
mine: the first interleaving test used a concurrent `sendTouch`, which **passes even with `txLock`
removed**, because `sendTouch` and `sendMicPcm` both enqueue onto `txQ` and are drained by the
single `ocbm-tx` thread — they serialise on the queue regardless of the lock. The lock's real job
is against the *blocking* senders (`setRadios`, `hello`, `mfi*`, `mgmt*`), which call `sendSync`
directly from the caller's thread. The test now uses `setRadios` against a `PacingTransport` that
stalls each write, making the contention deterministic; it fails when the lock is removed and
passed 6 consecutive clean runs.

**Still not unit-testable:** `MicrophoneCaptureManager`'s own capture loop, the `onCaptureError`
delivery and the `tryRecreateAudioRecord` identity guard all need a real `AudioRecord`. Robolectric
is on the classpath but its `AudioRecord` shadow does not model a failing `read()`, which is the
only state that matters here. These remain hardware-verified only.
- **ChaCha20-Poly1305 throughput at 60 fps on the Atom is unmeasured on the target**, but a JVM
  benchmark **refuted the assumption this file previously recorded**. `Cipher.getInstance` costs
  ~0.31 µs/call — noise at ~160 calls/s. The real cost is **allocation churn**: the payload is copied 9
  times end to end with 6 full-size allocations per frame, ≈9 MB/s of garbage at 12 Mbps, nearly all of
  it in ART's Large Object Space. So the escalation order in the plan is wrong: caching the cipher is
  not the win — **decrypting into a reused/pre-sized buffer is**, and JNI is not justified by any
  measurement. One copy has already been removed (`avccToAnnexB` now patches in place). The remaining
  restructure — `doFinal(in, off, len, out, 4)` straight into the emit buffer — would take copies 9→6
  and allocations 6→3; it is deliberately NOT done yet, because it is an optimization with no confirmed
  defect behind it and should be justified by a device trace.
- **USB sustained throughput is unmeasured.** `saturatedReads` was added to `UsbBulkTransport` as the
  evidence that would justify a larger read or `UsbRequest` pipelining; neither is earned until it fires.
- **`mgmtAction`/`mgmtGetInfo` still clear the response queue with no lock**, so two concurrent MGMT
  actions can steal each other's ACK. The plan called for an `mgmtLock` alongside the typed surface;
  both are still outstanding.
- **The stage 1-3 bench rig does not exist** (`--es bench usb|crypto|config`), nor `Ocbm.crc32`, nor
  `CH_FILE` `FILE_PULL`. Stage 3's pass criterion ("pushed CRC == readback CRC") is therefore not yet
  checkable, and the plan's safety argument — retire throughput/crypto/config unknowns *before* the
  first `CT_SUBSCRIBE` — has no code behind it yet.
- **No `[stat ]` emitter.** All the counters exist (`OcbmClient.statsLine`, `OcbmAvLanes.statsLine`,
  `Reassembler.resyncBytes`, the transport counters) but nothing schedules the 5-second line.
- **Several planned JVM tests are still missing**: `HeartbeatBackoffTest` (needs an injectable clock —
  it is the one test that protects the physical box from a reboot loop), `TranscriptReplayTest`,
  `FilePullTest`, `Crc32Test`.
- **The detekt baseline was regenerated** to absorb findings in the newly imported and newly written
  files. That waives complexity/style findings on new code, not just pre-existing code — a process
  smell worth revisiting. `ktlintFormat` also reformatted the imported `av/*.kt`; a token-level diff
  against the `gm_ccpa` originals confirmed all three remain semantically identical.
- ~~**This directory is untracked in the parent `ccpa_custom` git repo**, so there is no history to
  diff against. Worth fixing before the Part B cutover.~~ **SUPERSEDED 2026-08-15 — true when
  written, and fixed by the very commit that carried it.** `d842daf` imported `host/CarlinkAndroid/`
  into the repo (~100 tracked files, this document among them), so `git log` / `git blame` /
  `git diff` work here normally.

## Decisions taken (2026-08-14)

- Device management ships **degraded** — bare MACs with Forget, as the macOS host does. OCBM has no
  connect-by-MAC verb, no phone-disconnect verb, and `MGMT_INFO.devices` is a list of MAC strings with
  no name/type/last-seen/connected-flag. Matching today's `PhonesTab` needs box-side Rust work.
- ~~**Multi-touch is a v1 regression.**~~ **SUPERSEDED 2026-08-15 — built and hardware-verified**
  (see "Multi-touch" above; commit 694a34d). The single-touch mapping this line cited at
  `airplayd/src/main.rs:1079` is gone — `airplayd` now keeps two contact slots (`CONTACTS` /
  `contact_slot`, `main.rs:520-549`) and emits Apple's 12-byte two-finger report, and the host sends
  up to `MAX_CONTACTS = 2` pointers instead of suppressing secondaries. The 12-byte descriptor specced
  in `docs/carplay/06_AV_PIPELINE.md` is what shipped.
- **Steering-wheel media keys are kept** — they arrive via the AAOS MediaSession, not any box button,
  so they ride `INPUT_MEDIA_BTN` with `mediaButtonsSupport: true` in the pushed config.
- **No Siri affordance.** Touch-only leaves voice unreachable (CarPlay's screen has no Siri button).
  Re-addable later via `CMD_SIRI_DOWN`/`UP`.
- **App-driven SETUP deferred** (`appDrivenSetup: false`). The box's local response is the designed
  sticky fallback. This also defers the `cfg_crc` drift check that rides `RS_OPEN`.
