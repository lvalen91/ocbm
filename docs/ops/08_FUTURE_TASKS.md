# Future tasks — planned work that is not a defect

> **STATUS:** CURRENT · single owner for planned, owner-directed work. Created 2026-09-04. Defects and
> verification gaps stay in `04_OPEN_ITEMS.md`; a task that lands is replaced here by a pointer.

Owner-directed work items that are neither an open defect nor a verification gap (those live in
[`04_OPEN_ITEMS.md`](04_OPEN_ITEMS.md)). One entry per task, dated when raised, with the current
state it starts from and what "done" means. When a task lands, replace its body with a one-line
pointer to the doc that now owns the result — do not leave a stale plan beside a shipped feature.

## T1. Settings redesign — projection-aware, vehicle-centric (raised 2026-09-04; IMPLEMENTED 2026-09-04)

LANDED. The Settings window is now three tabs, Vehicle / Adapter / Diagnostics, built under the design contract
`host/MacHost/carlink_macOS/App/Settings/DESIGN.md`. The result is described in `../host/00_MACOS_HOST_APP.md`
"Settings window" and the Android Auto side in `../androidauto/00_ARCHITECTURE.md` section 4. Protocol-exclusive
settings are badged sub-groups inside the feature they belong to, not protocol tabs, and the AA engine renders
the neutral `VehicleProfile` through `AACapability(profile:adapter:warn:)`. The `rightHandDrive` CarPlay consumer
was device-verified 2026-09-05 (`../carplay/04_CAPABILITIES_AND_CONFIG.md` rightHandDrive). The original plan is in git history.

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

