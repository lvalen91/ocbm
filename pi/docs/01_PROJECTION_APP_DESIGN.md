# Projection app design — CarPlay as an AAOS-integrated experience

Companion to `00_PI_AAOS_PORT.md`, which covers the accessory stack underneath. That stack is
device-proven; **nothing consumes `127.0.0.1:9001` yet**, so this document is about the piece that
turns a working session into a usable product.

Scope: **wireless only.** Wired CarPlay needs OTG/UDC role toggling on the USB-C port that currently
carries adb, and is deliberately out of scope.

---

## 1. Decisions taken (owner, 2026-08-16)

| Question | Decision |
|---|---|
| Where the pairing / device UI lives | **Injected into AAOS Settings** — a Projection screen alongside Bluetooth/Wi-Fi |
| How a session starts once bonded | **Auto-start and take the screen** |
| Should projection act as HOME | **No** — ordinary app plus a launcher tile that returns to it |
| Resolution / FPS | **Detected at boot** from the active display, config generated before the radios come up |
| Video codec | **HEVC only** |
| Audio | **Stereo, AAC** |
| Instrument cluster / `altVideoStreams` | **Not implemented** — AAOS already owns the cluster display (§5.2) |
| Connection model | **Auto-connect or tap-to-connect (user setting), first-to-connect hierarchy** |
| Android Auto | **Separate track**, licensing-gated (§5.9) |

---

## 2. The constraint everything follows from

`carplay-wireless` drives `hci0` **directly** and Android's Bluetooth stack is disabled
(`svc bluetooth disable`) so it can. There is one controller and one owner.

**Therefore the stock Settings ▸ Bluetooth pane cannot pair the phone.** It has no stack behind it.
Any device list we show must be *our* list, backed by `carplay-wireless`, regardless of where it is
rendered.

This is not a workaround forced on us — it is what the OEM does too (§3). Returning to Android's BT
stack would mean the ROM-change path already costed and rejected: Class of Device, EIR layout and
SDP richness are not app-settable, and the raw-HCI route is the one that demonstrably works.

---

## 3. What the OEM references actually do

Decompiled from `reference/gm_cinemo/` (GM Info 3.7 / Silverado AAOS 12, and CT5 AAOS 14).

**GM splits projection across two components, by role — not one app.**

### `GMConnections` — the Settings/Connections app
`com.gm.hmi.connection.ui.activities.*`:
`PhonesActivity`, **`ProjectionPhonesActivity`**, `PhoneDetailsActivity`, `WifiHotspotActivity`,
`WifiHotspotNameActivity`, `WifiHotspotPasswordActivity`, `WifiNetworksActivity`,
plus `evG_ProjectionPopUpTermsOfUse` and `evG_BluetoothPairingPttParkMode`.

It also exposes a request-broker API — `IConnectionBluetoothReqHandler`,
`IConnectionWifiHotspotReqHandler`, `IConnectionWifiNetworkReqHandler` — so other apps ask
Connections to do connectivity work rather than doing it themselves.

> **The load-bearing observation: GM does NOT reuse the generic Bluetooth phone list for
> projection.** `ProjectionPhonesActivity` is a separate screen with its own semantics, next to
> `PhonesActivity`. Our constraint in §2 forces the same split, so we arrive where the OEM already is.

### `GMCarPlay` — a privileged service
A single component, `com.gm.server.carplay.service.CarPlayService`, with a system permission set
that reads as a specification for what a projection owner needs:

| Permission | What it buys |
|---|---|
| `MANAGE_ACTIVITY_TASKS`, `REAL_GET_TASKS` | Move the projection task to the front — **this is how "return to CarPlay" is implemented**, not by relaunching an activity |
| `MODIFY_AUDIO_ROUTING`, `RECEIVE_CAR_AUDIO_DUCKING_EVENTS` | Own the audio path; duck for nav/alerts |
| `ACCESS_VOICE_INTERACTION_SERVICE` | Siri |
| `CAR_UX_RESTRICTIONS_CONFIGURATION` | Driver distraction state |
| `CAR_DISPLAY_IN_CLUSTER`, `CAR_INSTRUMENT_CLUSTER_CONTROL` | Cluster surface |
| `WRITE_SECURE_SETTINGS`, `MANAGE_USB` | Platform state |

### SpeedPlay
`reference/tbox_speedplay/` is a shipped Carlinkit implementation — useful as evidence of *what can
be made to work*, never as a correctness reference. Per `CLAUDE.md`, its iAP2 link layer is a
re-derivation, not a licensed drop.

---

## 4. The AAOS projection framework — Google's own integration surface

**Google documents the AAOS-side hooks, but not the protocols.** There is a first-class projection
framework in AAOS and it is already enabled on this Pi, with nothing bound to it — that slot is
meant for an app like ours:

```
**CarProjectionService**
  Registered key event handlers:
  Local-only hotspot reservation: null
  Stable local-only hotspot configuration: true
  Wireless clients: 0
```

`projection` is an enabled car feature, `PROJECTION` is a **power-policy component** alongside
AUDIO / WIFI / BLUETOOTH, and the platform declares `android.car.permission.CAR_PROJECTION`,
`BIND_PROJECTION_SERVICE`, `ACCESS_CAR_PROJECTION_STATUS` and `TOGGLE_AUTOMOTIVE_PROJECTION`.

This reframes §3: GM's permission set is not an OEM invention — `GMCarPlay` is a **consumer of this
framework**. We should be one too.

### 4.1 What `CarProjectionManager` gives us

From AOSP source (`car-lib/src/android/car/CarProjectionManager.java`), not summaries:

| API | Relevance |
|---|---|
| `startProjectionAccessPoint(cb)` / `stopProjectionAccessPoint()` | *"Request to start Wi-Fi access point … for wireless projection receiver app."* Returns SSID / BSSID / preSharedKey |
| `getAvailableWifiChannels(band)` | Channel list in MHz — the sanctioned form of what we currently do with `iw` |
| `requestBluetoothProfileInhibit(device, profile)` | *"Disconnect the given profile … and prevent it from reconnecting"* — the supported way to stop Android's BT stack contending with projection |
| `addKeyEventHandler(events, handler)` | Steering-wheel **voice** and **call** keys; *"the system will suppress its default behavior … and call the event handler instead"* |
| `updateProjectionStatus(status)` | Publishes projection state system-wide |
| `registerProjectionRunner(intent)` | Reverse binding on projection start |
| `getProjectionOptions()` | OEM customisation bundle |

Key events available: `KEY_EVENT_VOICE_SEARCH_*` and `KEY_EVENT_CALL_*` (down / short-press-up /
long-press-down / long-press-up).

### 4.2 What Google does NOT document

There is no CarPlay implementation guide, and there will not be. The framework is deliberately
protocol-agnostic — the javadoc says *"wireless projection receiver app"*, never "CarPlay". The
protocol side is Apple/MFi licensed material, which is what `docs/ops/03_REFERENCE_INDEX.md` indexes. Google supplies the
socket; Apple supplies the receiver spec. Android Auto's receiver is likewise Google's own
proprietary implementation exposed through the same hooks.

**So: use the AAOS framework for platform integration, and `docs/ops/03_REFERENCE_INDEX.md`'s sources for the protocol.
Neither substitutes for the other.**

### 4.3 The tension, and what we adopt regardless

The framework assumes **the platform owns Wi-Fi and Bluetooth**. We bypass both — raw `hci0`
because Android's BT stack cannot express the CarPlay accessory identity (§2), and standalone
`hostapd` because framework SoftAP would not start on 5 GHz. That does not disqualify the framework;
it splits it into two halves.

**Adopt immediately — no dependency on the platform owning a radio:**

* **`updateProjectionStatus`** — today the system has no idea CarPlay is running. This is what makes
  projection legible to the HMI, the power policy and any other app.
* **`addKeyEventHandler`** — the correct answer for the Siri / voice button and the call button.
  Better than anything we would have invented, and it suppresses the default handler for us.
* **`registerProjectionRunner`** / **`getProjectionOptions`** — cheap, and they place us where the
  platform expects a projection app to be.

**Conditional on testing:**

* **`startProjectionAccessPoint`** is backed by **LocalOnlyHotspot**, which is a *different*
  `WifiManager` path from the tethered SoftAP that failed for us — and the device already reports
  `Stable local-only hotspot configuration: true`. It may or may not hit the same
  `IWifiApIface.setCountryCode` → `NOT_SUPPORTED` blocker. **Untested, and worth an hour**: if it
  works we can drop standalone `hostapd` *and* `pi/apdhcpd` for platform-supported equivalents, and
  inherit credential handling for free. If it fails, we keep what already works.
* **`getAvailableWifiChannels(band)`** — only meaningful if we move to the platform AP.
* **`requestBluetoothProfileInhibit`** — irrelevant while Android's BT stack is down, but the right
  tool if it is ever re-enabled for A2DP/HFP alongside our raw-`hci0` CarPlay link.

## 5. Capability specification

The target is **integration as deep as a native OEM's**, not a video window. This section fixes what
that means feature by feature, and — importantly — where each piece already exists.

**Most of this is wiring, not new protocol.** `crates/vendor/receiver/src/vehicle_config.rs` mirrors
**Apple's own `VehicleConfig` format**; the tree even carries Apple's ten templates
(`reference/carplay_sdk/apple_vehicleconfigs/`: *Standard Navigation*, *Portrait*, *Widescreen
Instrument Cluster*, *Minimum*, …). The knobs below are almost all present already. What is missing
is the **source of their values** — nothing currently reads them from AAOS.

| Capability | Where it already lives | Work needed |
|---|---|---|
| Dynamic resolution / FPS | `DisplayPanelsConfig` · `PixelDimensions` · `VideoStreamsConfig` | Generate from `DisplayManager` at boot |
| HEVC only | `levers` codec selection | Already forced on in the JNI path |
| Stereo AAC | `AudioConfig` · `AudioSubConfig` · `AudioFormatEntry` | Bind to AAOS car audio zones |
| Touch / multi-touch | `hidConfig.touchScreenMode` · `multi_touch` in `receiver/src/{events,info,levers}.rs` | Config only — **but see §5.3** |
| Steering wheel | `hidConfig.telephonyButtonsSupport` / `mediaButtonsSupport` / `knobSupport` / `dPadSupport` **+** `CarProjectionManager.addKeyEventHandler` | Join the two halves |
| oemIcon | `OemIconConfig` · `OemIconImage` | Supply the asset |
| Drive mode / limitedUI | `LimitedUiConfig` · `limited_ui` in `airplayd` **+** `CarUxRestrictions` | Bridge them |
| Focus transfer | `borrow` in `iap2-core/src/message.rs`, `receiver/src/events.rs` | Already the protocol's own model (§5.6) |
| Metadata → media source | iAP2 DataStream(130) — **observed open** in the proven session | Build the AAOS `MediaBrowserService` side |
| Auto-connect / first-to-connect | `reconnect.rs` · `KnownDeviceStore` | Policy + ordering (§5.8) |

### 5.1 Boot ordering

The owner's sequence is correct and is adopted:

```
boot → OS settles → detect active display → generate VehicleConfig
     → bring up BT + WLAN → auto-connect (if enabled)
```

This fits the existing design rather than fighting it: **`docs/carplay/04_CAPABILITIES_AND_CONFIG.md` already makes config app-driven**,
pushed to the box rather than compiled into it. The generator is a new *producer* for a channel that
exists.

One ordering constraint must be respected. Arming is **first-arm-wins per process**: `airplayd` is
spawned per session so it picks up a regenerated config on the next session, but a long-lived process
will not. Generating before anything arms — as specified — is what keeps that from biting.

Measured today: **`1920x1080 @ 60`**, density 240, single mode, no HDR
(`deviceProductInfo name=HDMI TO USB` — the display is a capture dongle, not a panel).

### 5.2 Why there is no cluster stream

**`altVideoStreams` will not be implemented**, and the platform is the reason, not just scope.

This AAOS build already reserves the second HDMI port for **its own** cluster UI:

```
**mDisplayConfigs**
 port=0 config={displayType=1 occupantZoneId=0 inputTypes=[210, 100, 101, 10]}
 port=1 config={displayType=2 occupantZoneId=0 inputTypes=[100]}
```

`displayType=2` is `INSTRUMENT_CLUSTER`. Supporting evidence, all from the running device:

* `cluster_service` appears in `mDefaultEnabledFeaturesFromConfig` — enabled by build config, not by us.
* `InstrumentClusterService` is running, with a renderer already bound:
  `mRenderingServiceConfig: android.car.cluster/.ClusterRenderingService`.
* The `android.car.cluster` package is installed.
* Cluster HAL properties are present: `CLUSTER_SWITCH_UI`, `CLUSTER_REQUEST_DISPLAY`,
  `CLUSTER_DISPLAY_STATE`, `CLUSTER_NAVIGATION_STATE`, `CLUSTER_REPORT_STATE`, `CLUSTER_HEARTBEAT`.
* Port 1 is given a **reduced input set** (one entry vs port 0's four) — consistent with a cluster,
  not a second interactive display.

So attaching a second display brings up **AAOS's own cluster**, and a CarPlay cluster stream would be
contending for a surface the platform already claims. Note this is also why GM's `CarPlayService`
holds `CAR_DISPLAY_IN_CLUSTER` / `CAR_INSTRUMENT_CLUSTER_CONTROL` (§3): an OEM doing cluster
projection has to *take* that surface from the platform. We are not doing that.

`mActiveOccupantConfigs` currently lists only `displayId=0`, because nothing is plugged into port 1.

### 5.3 Input — configurable now, untestable now

Configure touch and multi-touch: AAOS declares `touchscreen.multitouch.jazzhand` and Apple's
`hidConfig` has `touchScreenMode: "High Fidelty"` *(sic — Apple's own spelling)* for exactly this.

**But this bench has no touchscreen.** `/proc/bus/input/devices` lists only `vc4-hdmi-0` /
`vc4-hdmi-1 HDMI Jack`, a *Logitech USB Optical Mouse*, and *Apple, Inc. EarPods*. The Android feature
declaration is derived from build config, not from hardware. The mouse exercises the **single-pointer**
path only.

**Multi-touch can therefore be declared and plumbed, but not validated** until real touch hardware is
attached. Do not read a working session as evidence that multi-touch works.

Steering wheel is a **two-sided** job and both sides exist: Apple's `hidConfig` advertises the
capability to the phone, and `CarProjectionManager.addKeyEventHandler` receives the keys from AAOS —
which also *suppresses the platform's default handling* for us (§4.1). Available events are
`KEY_EVENT_VOICE_SEARCH_*` (→ Siri) and `KEY_EVENT_CALL_*`. Anything beyond voice and call is not
offered by that API and would need another route.

### 5.4 Metadata — the part most likely to break a session

Media metadata (so AAOS's media/radio HMI sees CarPlay as a source), telephony metadata, phonebook
and call history are all wanted. **The declaration rules constrain how, and getting this wrong costs
the whole session, not the feature.**

From `CLAUDE.md` and `docs/carplay/05_METADATA_AND_CONTROLS.md` §5.6, device-proven:

1. A `Start*` must be declared together with its `Stop*`, or iOS returns `RequiredInfoMissing`
   against the Stop id and **rejects the entire Identify**.
2. A receive must not be declared without its send (`OptionalMsgNotValidWithoutRequiredMsgs`).
3. A subscribe for an id param 6 does not declare is **silently ignored** — no error, no data.

And a `0x1D03` reject is **unrecoverable within a session**: params 6/7 are un-strippable, so the
retry is byte-identical and the second reject aborts.

**The hard constraint: telephony, phonebook and call history go on the WIRED / AirPlay-tunnel arm
only — never the Bluetooth-time arm.** The BT-time Identify is byte-pinned; `docs/wireless/00_WIRELESS_CARPLAY.md` §5.1/5.2
recorded iOS rejecting params-6/7 growth there twice, and both times it broke the Wi-Fi handoff — the
exact step this port depends on. That pin is *structural*, not conventional: the BT arm's param 6/7
list is hardcoded in `message.rs` and never consults the pushed policy.

The compiled default tier is `proven` deliberately. **Raising it is an explicit decision, not a
default** — it is where sessions have historically been lost. Per `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` the tier is app-pushed, so
the projection app is the right place to own that choice, and it should surface as a setting rather
than a constant.

Also load-bearing for the wired-arm work: **the DataStream(130) iAP channel was observed open** in
the proven session (`00_PI_AAOS_PORT.md` §5), and per `docs/carplay/05_METADATA_AND_CONTROLS.md` that — not `POST /command` — is the
wireless iAP2 carrier. The transport for metadata already works; only the AAOS-side consumer is
missing.

### 5.5 oemIcon — leave, don't quit

`OemIconConfig` / `OemIconImage` already exist. The icon returns the user to the AAOS home screen
**without ending projection**, which is the same requirement the launcher tile (§7.4) satisfies from
the other direction. Both depend on §7.1's split: the *service* owns the session, the *activity* owns
only the surface.

### 5.6 Focus transfer

CarPlay's own model is `borrow` vs take, already present in `iap2-core/src/message.rs` and
`receiver/src/events.rs`. A borrow is the right primitive for a transient nav prompt, call or message
alert: temporary foreground, automatic return. This needs no invention — it needs the AAOS side to
honour it rather than treating every transfer as a permanent takeover.

### 5.7 Drive mode

`LimitedUiConfig` (Apple side) ↔ `CarUxRestrictions` (AAOS side). `DriveStateMonitor` in
CarlinkAndroid already watches the AAOS half. The work is propagating it into `requestUI` /
`limited_ui`, which `airplayd` and `ocbm-proto` already carry.

### 5.8 Connection policy

Following GM: **auto-connect** or **tap-to-connect** as a user setting, with a **first-to-connect
hierarchy** over known devices — try the first, fall through to the second if unavailable.

Consequences for the app:

* `KnownDeviceStore` needs an **ordering**, not just a set.
* The attempt loop needs a per-device timeout, or one absent phone stalls the fallback.
* Auto-connect runs *after* config generation (§5.1), which is what makes the boot ordering matter.
* The Settings screen (§7.3) owns the setting and the ordering; `CarPlayService` executes it.

### 5.9 Android Auto — a licensing question, not a technical one

Correct that it needs no MFi. But **there is no public head-unit SDK**: the Android Auto *receiver*
protocol is proprietary and licensed, which is why NXP and AllGo sell "Android Auto Projection" as a
licensed product rather than it being something one builds against published docs. Google's
documented AAOS surface (§4) is protocol-agnostic and gives us the *hooks*, not the receiver.

**Treat as a separate track gated on a Google agreement, and confirm current terms before planning
around it.** Nothing in this document depends on it.

---

## 6. Our starting point

**`host/CarlinkAndroid` — not `gm_ccpa`.** `gm_ccpa/netprobe_app` is an instrument that grew into a
receiver; its UI is scaffolding and explicitly not a design reference.

CarlinkAndroid already has the right shape:

* Manifest declares `LAUNCHER` + `CAR_MODE` + `HOME` + `APP_MUSIC` categories, and foreground
  service types `mediaPlayback` / `connectedDevice` / `microphone`.
* `HevcRenderer`, `AacPlayer`, `VoiceRouter`, `MicrophoneCaptureManager`, `DriveStateMonitor`,
  `KnownDeviceStore`, `MainScreen` — plus a real unit-test suite.
* It is the app that **rendered HEVC in the emulator's decoder**, which is the single most valuable
  thing anyone has proven about the display path.

The `HOME` category is present but, per §1, **will not be used**. Leave it declared or remove it —
either way it must not be the default handler.

---

## 7. Proposed architecture

```
  ┌─────────────────────────── AAOS ────────────────────────────┐
  │                                                              │
  │  Settings ▸ Projection            Launcher tile              │
  │  (injected screen)                "CarPlay"                  │
  │        │                                │                    │
  │        │ bind                           │ moveTaskToFront    │
  │        ▼                                ▼                    │
  │  ┌──────────────────────────────────────────────────┐        │
  │  │  CarPlayService   (privileged, persistent)       │◄──────┐│
  │  │  session lifecycle · decode · audio · focus      │       ││
  │  └──────────────────────────────────────────────────┘       ││
  │                                                     CarProjectionManager
  │                                          updateProjectionStatus · keys ││
  │                                          (§4 — AAOS framework)         ││
  │        │ surface                                             │
  │        ▼                                                     │
  │  ProjectionActivity   (video surface only)                   │
  └──────────────────────────────────────────────────────────────┘
           │ :9001 (video, encrypted)   :9002 (audio, ADTS)
           ▼
      airplayd  ──►  the accessory stack (00_PI_AAOS_PORT.md)
```

### 7.1 `CarPlayService` — the owner
Privileged and long-lived. Owns the session so it **survives the user navigating away**, which is
the whole point of the launcher tile. Responsibilities:

* Consume `:9001` (encrypted video) and `:9002` (ADTS audio); hold the per-stream key.
* Drive `MediaCodec` (HEVC) and `AudioTrack`.
* Own audio focus and ducking.
* Track `CarUxRestrictions` via `DriveStateMonitor`.
* Expose session state to the Settings screen and the tile.

It is also **the app that binds the AAOS projection framework** (§4):

* `updateProjectionStatus()` on every session transition, so the platform and other apps can see
  that projection is active — today nothing does.
* `addKeyEventHandler()` for `KEY_EVENT_VOICE_SEARCH_*` (→ Siri) and `KEY_EVENT_CALL_*`.
* `registerProjectionRunner()` on start, `unregisterProjectionRunner()` on stop.
* Read `getProjectionOptions()` for OEM customisation rather than hardcoding.

It must **not** own a UI surface. Separating this from the activity is what lets projection be left
and returned to without tearing down the CarPlay session — the failure mode we already watched cost
us a session when the AV layer was wrongly declared dead.

### 7.2 `ProjectionActivity` — the surface
A `SurfaceView`/`TextureView` and input forwarding, nothing else. Cheap to create and destroy.
Attaches to the running service; never starts a session itself.

### 7.3 Settings injection — the device list
A Projection screen in AAOS Settings, backed by `carplay-wireless` (§2), showing paired phones,
which is active, and pair/forget/allow-projection controls.

Two candidate mechanisms, to be decided when we build it:
* An **injected Settings entry** (`com.android.settings.category` metadata) pointing at our activity
  — small, and keeps our UI in our own app.
* A **Settings overlay/extension** — deeper native integration, more ROM coupling.

Given no ROM rebuild is available, the injected-entry route is the realistic first cut.

### 7.4 Launcher tile — "return to projection"
Its only job is `moveTaskToFront` on the existing projection task (`MANAGE_ACTIVITY_TASKS` /
`REAL_GET_TASKS`, exactly as GM does). It must never restart the session.

---

## 8. Auto-start and take-the-screen

Per §1 the session starts on its own and takes the foreground. The protocol already leans this way —
`RECORD` sends `requestUI=true, takeScreen=true`, and the phone sends `disableBluetooth` once the
handoff completes, so iOS believes it owns the experience from that moment.

**The risk, stated plainly:** an unconditional grab will fight the user if it fires while they are
mid-task, and a reconnect storm (which we have already observed when a lower layer failed) would
yank the screen repeatedly.

Design it to take the screen **on session establishment, not on every `0x5702` retry**, and gate it:

* Once per *session*, latched — retries and re-`SETUP`s do not re-grab.
* Not while `CarUxRestrictions` indicates a state where a takeover is inappropriate.
* A user who explicitly navigates away is not pulled back until the next fresh session.

This keeps the chosen behaviour while removing the pathology. Make it a single flag so it can be
reversed cheaply if it proves annoying in the vehicle.

---

## 9. Known risks

1. **HEVC decode on VideoCore VI is unvalidated on this hardware.** Every proven decode in this
   project is `OMX.Intel.hw_vd.h265` on Intel; `gm_ccpa/docs/09` calls HEVC MediaCodec
   "first-of-kind" in this ecosystem. CarlinkAndroid decoding in the *emulator* is encouraging but
   is not the Pi's decoder. **Validate this before building UI on top of it.**
2. **Privileged permissions need a system app.** `MANAGE_ACTIVITY_TASKS`, `MODIFY_AUDIO_ROUTING`,
   `CAR_UX_RESTRICTIONS_CONFIGURATION` are not grantable to an ordinary APK. With root and a
   writable `/system` this is reachable by installing to `priv-app` with the right
   `privapp-permissions` allowlist — but it is a real integration step, not a manifest line.
3. **Audio currently has no route.** The Pi's audio HAL targets `vc4hdmi0`/the jack; nothing yet
   bridges CarPlay audio into AAOS's car audio zones.
4. **We are not yet visible to the platform as a projection app.** `CarProjectionService` reports
   `Bound to projection app: false`, so the power policy's `PROJECTION` component, the HMI and any
   other app have no idea a session exists. Cheap to fix (§4.3) and worth doing early — it is the
   difference between a process that happens to be running and something AAOS understands.
5. **The accessory stack is not persistent** — binaries in `/data/local/tmp`, started by hand. The
   app cannot depend on it until it is an init service.
6. **Multi-touch cannot be validated on this bench** — there is no touch hardware (§5.3). It can be
   declared and plumbed; it cannot be proven until a real panel is attached.
7. **Widening metadata declarations can cost the whole session, not the feature** (§5.4). A `0x1D03`
   is unrecoverable within a session, and on the BT arm it breaks the Wi-Fi handoff this port
   depends on. Treat any tier change as a deliberate, reversible experiment.
8. **Android Auto is licensing-blocked, not merely unbuilt** (§5.9). Do not schedule it as
   engineering work until the licensing position is confirmed.

---

## 10. Suggested order

1. **Prove HEVC on the Pi's decoder** with a throwaway consumer on `:9001`. Highest-risk unknown,
   cheapest to test, and it invalidates everything downstream if it fails.
2. **Test `startProjectionAccessPoint`** (§4.3). Cheap, and the outcome changes what we build: a
   working LocalOnlyHotspot removes standalone `hostapd` *and* `pi/apdhcpd` in favour of platform
   paths. Do it before investing further in the hand-rolled AP.
3. Stand whatever survives step 2 up as init services, so the stack outlives a reboot.
4. **Boot-time config generator** (§5.1) — read the active display from `DisplayManager`, emit the
   `VehicleConfig`, push it before the radios come up. Small, self-contained, and it removes the last
   hardcoded resolution. Independent of steps 1–3, so it can run in parallel.
5. `CarPlayService` + `ProjectionActivity` in CarlinkAndroid, consuming the seams — and binding
   `CarProjectionManager` (status + key events) from the start rather than retrofitting it.
   Key events are half of the steering-wheel story; the `hidConfig` side (§5.3) is the other.
6. Launcher tile with `moveTaskToFront`.
7. Settings injection for the device list, plus the connection policy it owns (§5.8) — ordering,
   auto vs tap, per-device timeout.
8. Audio routing into car audio zones.
9. **Metadata → AAOS media source**, on the wired/tunnel arm only (§5.4). Last because it is the
   step most able to break a working session, and it should land against a stack that is otherwise
   stable enough to attribute a regression to it.

---

## Sources

* [`CarProjectionManager.java`](https://android.googlesource.com/platform/packages/services/Car/+/master/car-lib/src/android/car/CarProjectionManager.java) — the API surface quoted in §4.1
* [`CarProjectionService.java`](https://android.googlesource.com/platform/packages/services/Car/+/refs/heads/main/service/src/com/android/car/CarProjectionService.java)
* [`ProjectionOptions.java`](https://android.googlesource.com/platform/packages/services/Car/+/master/car-lib/src/android/car/projection/ProjectionOptions.java)
* [AAOS — Integrate the AOSP host](https://source.android.com/docs/automotive/hmi/aosp_host)
* [AAOS — Integrate unbundled apps](https://source.android.com/docs/automotive/unbundled_apps/integration)
* Config format: `crates/vendor/receiver/src/vehicle_config.rs` and Apple's own templates in
  `reference/carplay_sdk/apple_vehicleconfigs/` — the authorities for §5's knobs.
* Declaration rules: `docs/carplay/05_METADATA_AND_CONTROLS.md` §5.6 (device-proven), `docs/wireless/00_WIRELESS_CARPLAY.md` §5.1/5.2 (the BT-arm pin),
  `docs/carplay/05_METADATA_AND_CONTROLS.md` (DataStream 130 is the wireless iAP2 carrier), `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` (config is app-pushed).
* Device evidence for §5.1–§5.3, captured 2026-08-16 from the running Pi:
  `dumpsys display`, `dumpsys car_service --services CarOccupantZoneService`,
  `dumpsys car_service | grep -i cluster`, `/proc/bus/input/devices`.
* Local: `reference/gm_cinemo/` (decompiled GM Connections + GM CarPlay), `reference/tbox_speedplay/`,
  and `docs/ops/03_REFERENCE_INDEX.md` for the protocol-side authorities.
