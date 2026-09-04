# Future tasks — planned work that is not a defect

> **STATUS:** CURRENT · single owner for planned, owner-directed work. Created 2026-09-04. Defects and
> verification gaps stay in `04_OPEN_ITEMS.md`; a task that lands is replaced here by a pointer.

Owner-directed work items that are neither an open defect nor a verification gap (those live in
[`04_OPEN_ITEMS.md`](04_OPEN_ITEMS.md)). One entry per task, dated when raised, with the current
state it starts from and what "done" means. When a task lands, replace its body with a one-line
pointer to the doc that now owns the result — do not leave a stale plan beside a shipped feature.

## T1. Settings redesign — projection-aware, vehicle-centric (raised 2026-09-04)

**Why.** The app supports two projection protocols, Apple CarPlay and Android Auto, each wired and
wireless. The Settings window does not reflect that: its Configuration tab is one long list built
around the CarPlay `VehicleConfig` YAML (`host/CarPlayHost/carlink_macOS/App/SettingsWindow.swift`,
three tabs: Configuration, CCPA, Diagnostics), and everything Android Auto specific is an
environment lever read at launch (`AA_FORCE_RES`, `AA_NO_TOUCH`, `AA_DRIVER_POSITION`,
`AA_TELEPHONY_SINK`, `AA_LEGACY_VIDEO`, `AA_SKIP_AUDIO_ACK`, `AA_TRACE_UNHANDLED`, `AA_P12`,
`AA_P12_PASS`, …) with no UI at all. Some vehicle facts are shared but only half-wired: the
`rightHandDrive` toggle drives Android Auto's `driver_position` since 2026-09-04 but is inert for
CarPlay (`docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §rightHandDrive); `nightMode` is a CarPlay
sensor and an AA sensor but its config-field form is stored and not pushed.

**Target shape.**

1. **Vehicle** — facts about the car that apply to both protocols, each a real control (toggle,
   segmented picker, stepper), never a free-text field where a bounded choice exists:
   name; drive side (Left / Right, and Center for AA); display geometry (resolution, fps, density)
   with the per-protocol consequence shown inline (CarPlay takes any size, AA snaps to 800×480 /
   1280×720 / 1920×1080 — `AACapability.Resolution.nearest`); night mode source; driving-restriction
   policy; microphone and telephony (HFP for AA, in-band for CarPlay); instrument-cluster / second
   display when it exists.
2. **CarPlay** — what only iOS consumes: the iAP2 feature tier (`proven`/`extended`/`all` and skips),
   audio format matrix, enhancedSiri, ETC, EV fields, wireless (BT/Wi-Fi) options, the SSP pairing
   answer lever.
3. **Android Auto** — what only gearhead consumes: touchscreen vs controller (`AA_NO_TOUCH`),
   declared keycodes, video/legacy-video experiments, the metadata services once T2 lands, the
   head-unit certificate source. Every current `AA_*` lever that is a real setting becomes a
   control here; every one that is a bench-only experiment moves to a clearly labelled Experiments
   group, off by default, with the guard-rail text from `03_WIRELESS.md` §6.
4. **Transport** — wired / wireless per protocol, shown as state, with the box-side actions the
   CCPA tab already has (restart wireless stack, forget bonds, enter NCM).

**Rules that bind the redesign.**

- App-driven doctrine stays: the app is the single source of truth and pushes; the box presents
  (`docs/carplay/04_CAPABILITIES_AND_CONFIG.md`). Nothing here adds a box-side default.
- A setting the box or the phone does not consume must say so where it is shown (the existing ⚠️
  inline convention), or not be shown. Settings that take effect only on the next session say so.
- Apple HIG for macOS: switches for booleans, segmented controls for small enumerations, steppers
  or pickers for numbers, sentence-case labels, help text in the inspector style already used.
- The generated YAML preview and the "unsaved changes" flow are kept; the YAML schema may grow but
  must not break `tools/proto_check.py` or the existing config push.
- Model split follows the UI: a shared vehicle model, plus a per-protocol model, so the AA engine
  keeps taking a `Sendable` snapshot (`AACapability.init(config:)`) and never reads the observable
  model from the session thread.

**Done means.** No `AA_*` environment variable is required for a normal session; the drive-side
toggle changes both protocols (CarPlay half needs the box consumer wired — see the open item); the
Configuration tab is three groups the owner can scan in one screen each; `docs/host/00_MACOS_HOST_APP.md`
and `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` describe the new layout in place.

## T2. Android Auto metadata services — LANDED 2026-09-04

Shipped the same day it was raised; the result and the wire table live in
[`../androidauto/01_SESSION_AND_AV.md`](../androidauto/01_SESSION_AND_AV.md) §"Metadata services".
Left open there: the Media Browser and Generic Notification services (not declared), and the
head-unit → phone `MediaPlaybackInput` control path.

## T3. Wideband HFP audio (mSBC) — LANDED 2026-09-04

Negotiated and streamed on device the same day (`+BCS: 2`, transparent eSCO, 60 B packets, 134
frames/s decoded, mic at 16 kHz mSBC); the pure-Swift codec and the kernel-3.14 socket detail are
in `../androidauto/01_SESSION_AND_AV.md` §telephony and `../host/00_MACOS_HOST_APP.md`. Still
behind the box lever (`/script/hfp_wbs`) until a few real calls have been heard. Follow-ons:
super-wideband (LC3-SWB, 32 kHz — the Pixel advertises it; needs an LC3 codec in the app and
`AT+BAC=1,2,3`), and an app-side dump of the decoded telephony lane for an objective bandwidth
measurement against the Harvard reference.

## T4. Non-standard Android Auto display sizes — LANDED 2026-09-04

Shipped and owner-confirmed the day it was raised (2400×960 panel → tier 2560×1440 H.265 with
`height_margin 416`; window 2.5:1, UI edge to edge, touch accurate). Mechanism, tier table and the
phone-side citations live in [`../androidauto/01_SESSION_AND_AV.md`](../androidauto/01_SESSION_AND_AV.md)
§1 "Video". Left open, none blocking:

- **Density as a real setting.** `density` defaults to 160 with an `AA_DENSITY` bench lever
  (2026-09-04: 240 on the 2400×960 panel scaled the UI ×1.5 with the rail 80→120 px, same visible
  rect). gearhead sizes everything from it, so the profile needs either a direct DPI field or a
  physical panel size (`density = px diagonal / inch diagonal` of the VISIBLE rect). T1's home.
- **`ui_config.margins`** (four-sided) for asymmetric placement — codec margins are always split
  evenly by the phone, which is what the app's centre-crop assumes. Only needed if a panel wants
  the visible rect off-centre.
- **Priority-ordered configuration list.** `MediaSinkService.video_configs` is repeated and gearhead
  takes the first it allows; declaring e.g. [tier+margins, 1920×1080+margins] would make the
  session survive a phone whose encoder refuses the first choice. The Config reply's
  `configuration_indices` would need to list them.
- Fullscreen: the crop assumes the view keeps the visible aspect; a fullscreen display of a different
  aspect will letterbox around the (correctly cropped) frame.

## T5. Android Auto cluster / auxiliary display — a second projected video stream (raised 2026-09-04)

**What it is (from gearhead 17.5, not from any public doc).** `MediaSinkService` carries
`display_id` (field 6) and `display_type` (field 7, gal `DisplayType`: 0 MAIN, 1 CLUSTER, 2
AUXILIARY). A head unit that declares a SECOND video sink with `display_type = CLUSTER` (or
AUXILIARY) gets a second H.264/H.265 stream, exactly like CarPlay's alternate video: gearhead
creates a `CarDisplayId` per video sink (`ivc.java:155-170`) and renders a dedicated component on
it — for the cluster that is Google Maps' `GmmCarAuxiliaryProjectionService` ("auxiliary map"),
the only component in the `MultiDisplay__cluster_display_supported_components` flag (Maps, its
dev/dogfood/fishfood builds). Other flags: `cluster_display_default_configuration = 2`,
`cluster_launcher_enabled = false` (no app launcher on the cluster), `reject_clusters_for_
unsupported_nav_apps` = {hyundai, kia, genesis} (an OEM reject list keyed on our declared
manufacturer — not us), `cluster_rotary_window_navigation = true`. The DHU has the matching keys
`instrumentcluster`, `navcluster`, `phonecluster`, `displaytype = main|cluster|auxiliary`.
This is distinct from the **data** path already landed (T2: `NavigationStatusService`, where the
head unit draws its own cluster from maneuver/distance/ETA messages); the video path shows the
phone's own map on the cluster.

**Plan.** Declare a second `MediaSinkService` (new channel id, `display_id 1`, `display_type 1`,
its own `VideoConfiguration` — tier/margins/density computed for the cluster panel) behind a lever;
run the channel-open / SETUP / CONFIG / START / CODEC_CONFIG / DATA / VideoFocus dance on that
channel a second time, feeding the app's existing alternate decoder (`altDecoder`, the CarPlay
alt-video path) into a second window. Read gearhead's `CAR.WM "Configuring display: %s, %s"` and
`GH.DisplayLayout` lines for what it chose; watch for the cluster component appearing when a
route is active. Unknowns: whether a session without a route shows anything on the cluster,
whether input is expected on that display (`InstrumentClusterInput`), and focus behaviour between
the two displays. **Done means** the phone's map renders in a second window while the main display
keeps projecting, recorded with the phone-side lines.

