# Documentation map

> **Roles and boards:** the three roles (replacement-firmware box, CCPA as peripheral, host apps) and the
> board matrix are in [ocbm/01_ROLES_AND_MFI_SOURCE.md](ocbm/01_ROLES_AND_MFI_SOURCE.md#three-roles). CCPA is the Carlinkit CPC200-CCPA family only;
> C2Air is a separate box and the Pi ports are not CCPA variants.

**One topic, one file. Correct it in place.** Adding a new dated document instead of fixing the
owning one is what produced the 66-file corpus this replaced; the originals live in git history.

Rules that keep it that way:

1. **≤10 files per category directory.** `tools/docs_check.py` fails the build if a category exceeds
   it. If a topic needs a new file, something else must merge first.
2. **Corrections go inline**, at the claim they correct, with the date and the evidence. Do not open
   a `*_CORRECTIONS`/`*_V2`/`*_<DATE>` sibling.
3. **Historical material stays inside its owner** under a `HISTORICAL` heading, or is dropped —
   git remembers it either way.
4. Anything genuinely dead is deleted, not left as a file a skimming reader can land on.

## carplay/ — the CarPlay protocol and session

| File | Owns |
|---|---|
| [00_ARCHITECTURE.md](carplay/00_ARCHITECTURE.md) | vision, the box/host split, USB/NCM transport |
| [02_SESSION_LIFECYCLE.md](carplay/02_SESSION_LIFECYCLE.md) | session states, start ordering, the app-driven SETUP relay, teardown |
| [03_SDK_GROUND_TRUTH.md](carplay/03_SDK_GROUND_TRUTH.md) | what Apple's licensed SDK says; conformance and Simulator verification |
| [04_CAPABILITIES_AND_CONFIG.md](carplay/04_CAPABILITIES_AND_CONFIG.md) | app-driven doctrine, the YAML VehicleConfig framework, field glossary, capability dossiers |
| [05_METADATA_AND_CONTROLS.md](carplay/05_METADATA_AND_CONTROLS.md) | the metadata surface, iAP2 declaration rules, the DataStream/RCS carrier |
| [06_AV_PIPELINE.md](carplay/06_AV_PIPELINE.md) | audio (capability + pipeline), video, cornerMasks, touch/HID uplink |
| [07_PHONE_SIDE.md](carplay/07_PHONE_SIDE.md) | iPhone-side behaviour we cannot change |

## androidauto/ — the Android Auto protocol and session

| File | Owns |
|---|---|
| [00_ARCHITECTURE.md](androidauto/00_ARCHITECTURE.md) | scope, why AA differs from CarPlay, the stock stack, the GAL credential, the box/host split, video geometry |
| [01_SESSION_AND_AV.md](androidauto/01_SESSION_AND_AV.md) | what works today, resolved defects and their causes, open items |
| [02_ARBITRATION.md](androidauto/02_ARBITRATION.md) | CarPlay vs Android Auto — phone-type detection, the single owner flag, closed failure modes |
| [03_WIRELESS.md](androidauto/03_WIRELESS.md) | wireless Android Auto — BT bootstrap, headset gate, TCP pump; device-run end to end 2026-09-04 |

## wireless/ — the wireless transport

| File | Owns |
|---|---|
| [00_WIRELESS_CARPLAY.md](wireless/00_WIRELESS_CARPLAY.md) | wireless session bring-up, WiFi handoff, the AirPlay/iAP2 tunnel |
| [01_BT_AND_RADIO.md](wireless/01_BT_AND_RADIO.md) | the chipset-agnostic radio HAL, BT bring-up, pairing, reconnect |

## host/ — the host applications

| File | Owns |
|---|---|
| [00_MACOS_HOST_APP.md](host/00_MACOS_HOST_APP.md) | the shipping macOS app |
| [01_ANDROID_AND_AAOS.md](host/01_ANDROID_AND_AAOS.md) | the Android projection app and AAOS integration |

## ops/ — running, verifying and governing the work

| File | Owns |
|---|---|
| [00_BUILD_AND_DEPLOY.md](ops/00_BUILD_AND_DEPLOY.md) | build, flash/RAM footprint, deployment |
| [01_RECOVERY.md](ops/01_RECOVERY.md) | un-bricking, without touching the signed boot stack |
| [02_TESTING.md](ops/02_TESTING.md) | live-session test plans and gates |
| [03_REFERENCE_INDEX.md](ops/03_REFERENCE_INDEX.md) | which external source answers which question, and where it lives |
| [04_OPEN_ITEMS.md](ops/04_OPEN_ITEMS.md) | every open item + the roadmap — **the pick-up point for a new session** |
| [05_AUDITS.md](ops/05_AUDITS.md) | code audits and their remediation |
| [06_CORRECTIONS_LEDGER.md](ops/06_CORRECTIONS_LEDGER.md) | historical record; `R-<doc>-<n>` ids key to the pre-consolidation numbering |
| [07_AUTHORIZATION.md](ops/07_AUTHORIZATION.md) | scope/authorization statement and session framing |
| [08_FUTURE_TASKS.md](ops/08_FUTURE_TASKS.md) | owner-directed planned work that is not a defect (settings redesign, AA metadata services, mSBC) |
| ops/captures/ | raw session captures kept as evidence |

## ocbm/ — the OCBM protocol and roles

| File | Owns |
|---|---|
| [00_PROTOCOL.md](ocbm/00_PROTOCOL.md) | the OCBM envelope, channel ids, seam framing, backpressure — **projection-agnostic**: it carries Android Auto too |
| [01_ROLES_AND_MFI_SOURCE.md](ocbm/01_ROLES_AND_MFI_SOURCE.md) | the three roles, the boards, the MFi-source model (MFi 2.0 through 3.0) |

## boards/ — board ports outside the CCPA

| File | Owns |
|---|---|
| [00_PI_AAOS_PORT.md](boards/00_PI_AAOS_PORT.md) | Raspberry Pi 4 + AAOS port |
| [01_PROJECTION_APP_DESIGN.md](boards/01_PROJECTION_APP_DESIGN.md) | the AAOS projection app design |
| [03_BRINGUP_RUNBOOK.md](boards/03_BRINGUP_RUNBOOK.md) | cold bring-up of the Pi (historical record) |
| [04_IMAGE_REBUILD_GUIDE.md](boards/04_IMAGE_REBUILD_GUIDE.md) | what to change in the AOSP tree, by component |

## Where the old numbers went

`docs/56` → carplay/04 · `docs/45`, `docs/47`, `docs/20_METADATA` → carplay/05 · `docs/13`,
`docs/43`, `docs/49` → carplay/03 · `docs/27`, `docs/62`, `docs/23`, `docs/52` → carplay/06 ·
`docs/20_WIRELESS`, `docs/29–39` → wireless/00 · `docs/40`, `docs/41`, `docs/51`, `docs/57` →
wireless/01 · `docs/60` → androidauto/00+01, `docs/61` → androidauto/02 · `docs/25`, `docs/48`, `docs/50` → ops/05 ·
`REVISIONS.md` → ops/06. The canonical codec is `crates/ocbm-proto`. Full map: the `<!-- absorbed: … -->` marker at the head of every section.

## Repository layout

| Path | Contents |
|---|---|
| `docs/carplay/`, `docs/androidauto/`, `docs/wireless/`, `docs/host/`, `docs/ops/`, `docs/ocbm/`, `docs/boards/` | The whole corpus: seven categories, capped at 10 documents each by `tools/docs_check.py`. Start at [`docs/README.md`](README.md) |
| `crates/ocbm-proto/` | OCBM wire codec (envelope, channels, Reassembler, CRC-32) — shared by box + host |
| `crates/vendor/` | Nine crates vendored in to make the tree self-contained. Provenance is mixed: `receiver`, `pairing`, `rtsp`, `mfi` and `eld-codec` came from the archived `ncm_carplayd` tree; `iap2-core`, `metadata` and `wireless` came from the `carplayd` tree; `mfi-i2c-local` was written here (a port of `wireless/src/mfi_local.rs`, split out because `receiver`/`mfi` are `#![forbid(unsafe_code)]`). Eight are path-dependencies only (`Cargo.toml` `exclude`); `wireless` is the one workspace member, producing the shipped `btd` (BT/WiFi wireless-CarPlay stack) box binary; the mDNS `_airplay._tcp` advertiser is a thread inside `carplayd` (`box/carplayd/src/discovery.rs`) |
| `box/ocbmd/` | Box OCBM daemon (armv7-musl), over `/dev/usb_accessory`: CTRL `0x00` (incl. session-control SUBSCRIBE/heartbeat/presence → `/tmp/host_present`) / MFI `0x01` / CONSOLE `0x02` (root PTY) / IP `0x10` / FILE `0x11` / ETH `0x12` / VIDEO `0x20` / MEDIA_AUDIO `0x21` / ALT_AUDIO `0x22` (voice sink, seam `:9003`) / METADATA `0x23` (seam `:9004`) / ALT_VIDEO `0x24` (cluster screen, seam `:9005`) / INPUT `0x30` (HID uplink) / MIC `0x31` (mic uplink) / MGMT `0x40` (the app's adapter/management tab) / RTSP `0x41` (app-driven SETUP relay, seam `:9106`) / ECHO `0xFF` / DISCARD `0x0FFF`. Authoritative list: `crates/ocbm-proto/src/lib.rs` |
| `box/iap2d/` | Box iAP2 accessory daemon (armv7-musl): handshake + local-i2c MFi → Identify → the wired metadata plane (the `iap2-core` `features.rs` table generates Identify params 6/7 and the `Start*` subscribes; inbound updates go out as JSON on the `CH_METADATA` `:9004` seam; the app-pushed tier is armed at startup and again at SendIdentify — `box/iap2d/src/main.rs:373,597`) |
| `box/carplayd/` | Box AirPlay pairing daemon (armv7-musl): reuses receiver_core `ControlServer` + `LocalMfiSigner` (local i2c) → pair-setup/verify on `ncm0:5000` → derives the session key → forwards **encrypted** A/V + hands the per-stream key; disk-backed PeerStore (`/etc/carplay_peers.bin`) |
| `boards/ccpa-imx6ul/rootfs/` | Deployable box rootfs overlay (stripped scripts + boot config): boot chain `ocbm_boot.sh` (launches ocbmd + supervisor) + `early_console.sh` (UART recovery shell) |
| `host/ocbm-host/` | Host OCBM client (rusb/libusb): hello/echo/mfi/ip/console/settime/push/bridge/session/**avdec** (debug receiver: SUBSCRIBE + heartbeat + decrypt A/V) |
| `boards/ccpa-imx6ul/tools/` | `iap_role_switch.c`/`.armv7` (45 lines) — issues Apple's standard `0x51` host-role USB control request over raw usbfs (no libusb, so it runs on the stripped appliance). **First-party, implemented from documentation**, carried over from this owner's own `ncm_carplayd/ccpa/probes/iap_trigger.c`; the only C binary the box ships (built by `build.sh`, installed to `/usr/bin`) |
| `tools/` | Dev/ops. **Current provisioning path:** `ncm_base_install.sh` (Carlinkit stack out, owned boot path, USB-NCM root shell) then `ocbm_install.sh` (place → verify → reboot → reversible trial → finalize; `--full` also installs `iap2d`/`carplayd`/`btd`, `session_supervisor.sh`/`projection_up.sh` **and** the `radio_detect.sh`/`radio_hal.sh`/`radio_ap_up.sh` seam). `install_fhs.sh` is the older OCBM-era FHS install and still works, but places only `ocbmd`/`iap2d`/`carplayd` + `iap_role_switch` — no `btd`, no radio seam. Also: `session_supervisor.sh` + `projection_up.sh` (lifecycle actor + IDLE→projection bring-up), `uart_push.sh` (UART file deploy over the serial console), `boxsh.py`, script auditor, UART pad finder |
| `box/aa-bridge/` | Box Android Auto USB bridge (armv7-musl): AOAP switch + raw byte pump between the phone's bulk endpoints and the host, over OCBM `CH_IP`. No AA protocol knowledge — the engine is app-side |
| `crates/box-common/` | Protocol-agnostic box layer shared by the CarPlay and Android Auto sets: usbdevfs primitives, phone-type detection, the single projection-owner arbitration flag, and the app-pushed config levers |
| `boards/c2air-v821b/` | C2Air: a separate box (Allwinner V821B, rv32), MFi 3.x. OCBM not yet run on its new baseline; the earlier run on the stock-firmware-based unit is recorded in `boards/c2air-v821b/README.md`. `btattach` is deliberately the only board-specific Rust |
| `baseline/` | Vanilla CCPA image recipe (mainline kernel, recovery initramfs, busybox rootfs), derived from LIVI. GPL-3.0-or-later (DTS and patches GPL-2.0); see [NOTICE.md](../NOTICE.md) and [`baseline/README.md`](../baseline/README.md) |
| `boards/rpi4-aaos/` | Raspberry Pi 4 + AAOS port (light testing): full CarPlay A/V reached with a CCPA in NCM mode as MFi source only (`box/mfid`), the Pi's own BT/Wi-Fi used. See `docs/boards/00_PI_AAOS_PORT.md` |
| `crates/mfi-wire/` | `MFI1` framing and blocking client for the opt-in remote MFi path (`CARPLAY_MFI_ADDR`), linked into production crates |
| `box/mfid/` | Not shipped: ephemeral MFi service over USB-NCM for hosts without a coprocessor |
| `boards/rpi-zero2w/` | Raspberry Pi Zero 2 W bring-up. Measurement only — no OCBM port yet, and the board facts differ from the CCPA |
| `host/gm_ccpa/` | **Wireless CarPlay as an unprivileged AAOS app** on a 2024 Silverado (GM Info 3.7, API 32). The phone streams over the *vehicle's own* hotspot; the adapter carries no media. Its Rust core links this tree's crates. See [`host/gm_ccpa/README.md`](../host/gm_ccpa/README.md) |
| `host/CarlinkAndroid/` | AAOS head-unit host app (Kotlin) for GM Info 3.7 — full OCBM, wireless CarPlay over the *adapter's* radios. The opposite division of labour to `gm_ccpa/`. See [`host/CarlinkAndroid/OCBMANDROID.md`](../host/CarlinkAndroid/OCBMANDROID.md) |
| `host/mfi-probe/` | Host-side exerciser for `mfid`: proves the MFi coprocessor answers with a real certificate and signature over USB-NCM, before anything depends on it |
| `host/aa-headunit/` | Rust Android Auto head-unit reference client (TCP / `adb forward`) — the de-risking path the macOS engine was built against |
| `host/MacHost/` | The shipping macOS host app (Xcode project `carlink_macOS`) — VideoToolbox decode, audio, touch/media-key uplink, Settings/YAML, OCBM client |
| `host/CallSim/` | Android app: self-managed Telecom `ConnectionService` that fakes real phone calls (ringing, answer/hang-up, HFP/SCO audio routing) for testing AA telephony without a SIM |

## Reference material (locations + what each is authoritative for)

**Apple CarPlay Kit SDK — the single source of truth for the CarPlay/iAP2 protocol AND the config schema.**
`/Applications/Xcode.app/Contents/SharedFrameworks/DeviceKit.framework/Versions/A/PlugIns/CarPlaySimulator.devicekitplugin`
(Xcode 27 beta; bundled `Contents/Frameworks/CarPlaySDK.framework`). Accessed under an active Apple
Developer Program membership — licensed first-party material used under the developer program, obtained directly from the SDK install. Use it for:
- protocol truth (link/state/message framing, TLV layouts, message ids);
- the **YAML config schema** — `Contents/Resources/VehicleConfigs/Configs/*.yaml` (Standard, Widescreen,
  Portrait, Instrument Cluster, Navigation, Minimum…) and the feature toggles in the `MacOS/CarPlaySimulator`
  binary (`_enablesHEVC`, `_enablesMainBufferedAudio`, `supportsAltScreen`, cluster config…).
- See auto-memory `carplaysimulator-devicekitplugin-spec`.

**`carlink_macOS` — the reference model for the HOST APP (USB claim + typed-bulk protocol + decode).**
Outside the repo (see [ops/03_REFERENCE_INDEX.md](ops/03_REFERENCE_INDEX.md), "Locations outside the repo"). The original macOS host app for the *stock* CCPA. Shows
the isolated-USB-claim integration model we mirror: `USB/USBTransport.swift` (IOKit
`IOUSBInterfaceInterface300` bulk claim, no OS interface), `Protocol/MessageTypes.swift` (the
`0x55AA55AA` 16-byte header + typed message catalog OCBM replaces), `Video/H264Decoder.swift`
(VideoToolbox — we go HEVC), `Audio/`, `App/AppDelegate.swift`. The new host app is rebuilt from this.

**`CPC200-CCPA_resources` — community interoperability documentation for the original firmware + protocol.**
A separate checkout, outside the repo. Authoritative for how the stock box worked:
`documentation/02_Protocol_Reference/{usb_protocol,video_protocol,audio_protocol,carplay_handshake,
inbound_session_sequence,command_ids}.md`, `documentation/04_Implementation/host_app_guide.md`,
`documentation/01_Firmware_Architecture/hardware_platform.md`. Documents the `0x55AA55AA` framing, the
full message catalog, video (H.264 + 20-byte sub-header) / audio (PCM + 12-byte sub-header), and the
**host config flow** (`Open` 0x01, `BoxSettings` 0x19, `SendFile` 0x99 → AirPlay SETUP `viewAreas`) —
the field set our YAML absorbs. NOTE: the stock box *terminated + stripped*; we mirror its transport
model, not its scope.

**`ncm_carplayd` — proven Rust CarPlay receiver + CCPA helpers to reuse.**
Archived outside the repo. `receiver_core/` = independent, from-documentation Rust receiver (RTSP,
SRP/25519 pairing, MFi-SAP, ChaCha20 decrypt, typed A/V seams) — the code we cross-compile to the box.
`macos/{carplay-app,carplay-hud}/` = the SwiftUI/AppKit consumer apps (IPC-seam consumers) the host app
descends from. Its `ccpa/` bring-up helpers = the USB host-role switch helper, `iap2_auth.c` (the proven box-side iAP2 auth reference),
the MFi auth-service helper, and `ncm_bridge.c`. `ccpa/scripts/` = proven cold-start orchestration.

**`carplayd` — Apple-spec iAP2 core + reinforcing session reference.**
A separate checkout outside the repo (`rust/carplayd/`). `crates/iap2-core/` = the transport-agnostic
iAP2 link/state/message/spec crate `iap2d` depends on (`src/spec.rs` machine-generated from the Apple
plugin; `src/link.rs` holds the coalesced-read fix from this project).

**Chip backups — both in-repo and out.** The clean owned **baseline** is recorded in this repo at
`boards/ccpa-imx6ul/stock/`: per-region `mtd0`/`mtd1`, `SHA256SUMS`, `manifest.txt` and `rootfs.files.txt` (the
rootfs file list `tools/ncm_base_install.sh baselinediff` compares against) — see its own `README.md` for
the programmer restore procedure. Additional **per-device / per-session** factory and pre-change backups are
kept outside the repo, in the maintainer's local CCPA backup archive (and on the box's USB stick at
`/mnt/UPAN/ccpa_backups/`). All of it exists solely for recovery.
