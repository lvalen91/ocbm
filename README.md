# ccpa_custom — open-source phone-projection firmware userspace + host app for the Carlinkit CPC200-CCPA

CarPlay and Android Auto, over one open transport (OCBM).

> **Documentation:** [docs/README.md](docs/README.md) is the map — four categories
> (`carplay/`, `wireless/`, `host/`, `ops/`), capped at 10 documents each by `tools/docs_check.py`.
> One topic, one file: correct the owning document, never add a dated sibling.

> **License:** [Unlicense](LICENSE) — public domain. The code is original; external projects
> were reference points only, and what was consulted is recorded in [NOTICE.md](NOTICE.md).

> **Scope & authorization:** licensed Apple/MFi developer, genuine hardware, ordinary accessory development — not security work. See [docs/ops/07_AUTHORIZATION.md](docs/ops/07_AUTHORIZATION.md).

**What it is.** The CPC200-CCPA is a small ARM Linux dongle that plugs into a car's USB port; a phone
connects to it and the car gets **phone projection** — CarPlay from an iPhone, Android Auto from an
Android phone. This project replaces the dongle's closed userspace with open code, and pairs it with a
host app — on a Mac, or on an Android Automotive head unit — that does the decode and rendering. Both
projection protocols ride the same open transport, **OCBM**: the dongle owns the stable,
hardware-bound half, the app owns the half that changes with every phone-OS release.

An open, Rust-first replacement for the CCPA's stock projection stack (Carlinkit "riddleBox":
`ARMadb-driver` / `AppleCarPlay` / `ARMiPhoneIAP2`), running as ordinary userspace on the same
hardware. It reuses the adapter's **hardware** — WLAN/BT radios and the onboard genuine Apple
**MFi 2.0C** authentication coprocessor (`/dev/i2c-1 @0x11`), used the way Apple's accessory
program intends — and its **factory vendor boot stack** (HAB-signed U-Boot + OTPMK-encrypted
kernel 3.14.52), which this project never modifies and treats as fixed platform firmware (a BIOS).
Everything above the kernel is rewritten open-source.

> **The problem this solves:** the stock CCPA ships an older, deliberately stripped CarPlay
> implementation — old SDK, H.264 only, features dropped. Replacing the userspace gets current,
> full-featured HEVC CarPlay, and keeps it current: new features land app-side. No phone
> modification, no custom PCB, no soldered MFi chip.

> **Android Auto:** the same split also carries **wired Android Auto** — the box does the AOAP USB
> switch and pumps raw bytes; the host runs the AA protocol engine and decodes, over the same OCBM
> link as CarPlay. Video, all three audio sinks, the mic uplink, touch, hardware buttons and the
> night/driving sensors are device-proven against a Pixel 10, driven from the app's own vehicle
> profile rather than hardcoded. Wired only; wireless AA is unbuilt. See Status and `docs/host/02_ANDROID_AUTO.md`.

---

## Requirements

- A genuine **Carlinkit CPC200-CCPA** adapter (i.MX6UL, 123 MB RAM, jffs2 rootfs, on-board MFi 2.0C
  coprocessor). A second adapter, the C2Air, is partially supported — see `c2air/`.
- An **iPhone**, and an active **Apple Developer Program membership** for the licensed CarPlay SDK
  material the protocol work is checked against.
- A **Mac with Xcode** to build the host app, or an AAOS head unit for the Android one.
- Rust with `rustup target add armv7-unknown-linux-musleabihf` and `cargo install cargo-zigbuild`
  (the box binaries cross-compile with the zig linker — no Linux VM needed).

## Getting started

| Step | Where |
|---|---|
| Build the box daemons and the host CLI | `./build.sh` — detail in [`docs/ops/00_BUILD_AND_DEPLOY.md`](docs/ops/00_BUILD_AND_DEPLOY.md) |
| Commission a fresh adapter | `tools/ncm_base_install.sh`, then `tools/ocbm_install.sh` — its `manifest()` is the authoritative file set |
| Run the macOS host app | Xcode project at `host/CarPlayHost/carlink_macOS` — [`docs/host/00_MACOS_HOST_APP.md`](docs/host/00_MACOS_HOST_APP.md) |
| Recover a wedged or bricked box | [`docs/ops/01_RECOVERY.md`](docs/ops/01_RECOVERY.md) |
| Verify a change before committing | `tools/docs_check.py`, `tools/proto_check.py`, and the test plans in [`docs/ops/02_TESTING.md`](docs/ops/02_TESTING.md) |

## The one hard constraint (the vendor floor)

The i.MX6UL boot ROM enforces HAB signature verification and the kernel is per-chip OTPMK-encrypted,
so **U-Boot + kernel are fixed platform firmware** — untouched here. All CCPA behavior is userspace,
and userspace is 100% open in this project. Practical implication of the fixed kernel: no
configfs/gadgetfs/functionfs, so development uses the kernel's existing USB-gadget functions
(`f_accessory` bulk + `f_ncm`). See [`docs/carplay/00_ARCHITECTURE.md`](docs/carplay/00_ARCHITECTURE.md),
which covers the architecture and the transport.

---

## Status

Open work and the pick-up point: **[docs/ops/04_OPEN_ITEMS.md](docs/ops/04_OPEN_ITEMS.md)**.

**CarPlay — wired and wireless, end to end.** HEVC 1920x720, every `audioType` negotiated and
decoded, mic uplink, touch and HID, metadata (NowPlaying, route and lane guidance, call state).
Wireless rides BT pair -> WiFi handoff -> AirPlay/RTSP -> OCBM; the inbound iAP2 carrier is the
RemoteControlSession DataStream, stream type 130. App-driven SETUP is the default on **both**
transports, with box-driven SETUP as the selectable sticky fallback.

**Android Auto — wired, device-proven against a Pixel 10.** The box does the AOAP switch and pumps
raw bytes (`ccpa/aa-bridge`); the host runs the AA protocol engine over OCBM `CH_IP`. Video, all
three audio sinks, mic, touch, hardware buttons and the night/driving sensors work, driven from the
same app-pushed vehicle profile CarPlay uses. The box selects AA on its own, and CarPlay/AA
arbitration is first-come-wins with neither able to interrupt the other. **Wireless AA is unbuilt**,
and HOME/BACK keys are declared but have no effect.

**Two hosts.** `host/CarPlayHost/` is the shipping macOS app. `host/CarlinkAndroid/` is an AAOS
head-unit app (GM `gminfo3.7`, AAOS 12L / API 32) that claims the adapter over OCBM and runs full
wireless CarPlay — same client contract, its own decode/render/mic stack. It is tracked in-tree on a
feature branch and not yet merged to `main`; see `host/CarlinkAndroid/OCBMANDROID.md`.

**One protocol definition.** `crates/ocbm-proto` is canonical and `tools/proto_check.py` verifies
every client against it — the Swift client, the Kotlin client, and a sibling checkout when one is
passed. A value that disagrees is an error; a constant a client has not defined is a gap, and an
error only for channels, `CT_*` opcodes and frame flags.

**Sibling project.** `gm_ccpa` is a different architecture (a GM head-unit bridge), but it is not
independent: its `carplay-jni` takes `receiver`, `pairing` and `mfi` as cargo path deps into this
checkout, and its `OcbmProto.kt` is a symlink to this repo's copy, so the protocol is edited once
here. Its app-level code is deliberately its own.

## Architecture in brief

Full detail: [`docs/carplay/00_ARCHITECTURE.md`](docs/carplay/00_ARCHITECTURE.md). The governing
doctrine is app-driven configuration ([`docs/carplay/04_CAPABILITIES_AND_CONFIG.md`](docs/carplay/04_CAPABILITIES_AND_CONFIG.md)):
anything configurable is owned by the app, the box presents what the app pushes, and box placement is
earned rather than designed in.

**The split is by stability, not by transport.** The adapter owns the parts Apple keeps
backward-compatible forever — the iAP2 accessory handshake, MFi 2.0C authentication on the on-board
coprocessor, HomeKit pair-setup/pair-verify, and the ChaCha20 session-key derivation. The host app
owns the part that changes with every SDK: the AirPlay/RTSP SETUP negotiation (codec, resolution,
screens, audio formats, feature enablement), A/V decode, UI and input. New CarPlay features therefore
land app-side, usually with no adapter change.

| | Adapter (rarely changes) | Host app (where features live) |
|---|---|---|
| Owns | iAP2 handshake, MFi auth, pairing, key derivation, the OCBM mux | SETUP negotiation, decode, render, input, the YAML config |
| Media | forwards encrypted A/V untouched — never decodes or re-encodes it | decrypts with the adapter-provided ephemeral key, decodes HEVC (VideoToolbox) |
| Config | presents what the app pushed | single source of truth |

The adapter does parse CarPlay above pairing — it answers SETUP including the `enabledFeatures` echo,
reads the decrypted `/command` channel, decrypts the type-130 DataStream and relays iAP2 TLVs. What it
never touches is the media itself.

**No OS network interface.** The host app claims the adapter by VID:PID (`0x1314:0x2d00`) and speaks a
bulk protocol entirely inside the app — OCBM, the open replacement for the stock `0x55AA55AA` typed-bulk
protocol. The OS only ever sees "an app claimed a USB device".
[`docs/carplay/01_OCBM_PROTOCOL.md`](docs/carplay/01_OCBM_PROTOCOL.md).

**Two secrets, not one.** The MFi private key never leaves the coprocessor; all MFi crypto happens on
the box and the app sees only status. The ChaCha20 session key is derived at pair-verify, is ephemeral
and per-connection, and is what lets the app decrypt A/V the adapter forwarded untouched.

**HEVC, not H.264** — the stock box negotiated H.264 High every session. Both sides of the assertion
are driven by the app's `enablesHEVC`: the adapter emits `hevcInfo` in `/info`, and the SETUP
`enabledFeatures` echo is app-authored on both transports.
[`docs/carplay/06_AV_PIPELINE.md`](docs/carplay/06_AV_PIPELINE.md).

**Config is an Apple-shape `VehicleConfig` YAML** held app-side and pushed at SUBSCRIBE; the box never
persists it. [`docs/carplay/04_CAPABILITIES_AND_CONFIG.md`](docs/carplay/04_CAPABILITIES_AND_CONFIG.md).

**SETUP is app-driven on both transports**, with the box's local response as the fallback the phone
never notices. Measured relay cost is p99 2.36 ms under A/V load against a 50 ms gate.
[`docs/carplay/02_SESSION_LIFECYCLE.md`](docs/carplay/02_SESSION_LIFECYCLE.md).

Rejected alternatives and why, so they are not re-litigated:
[`docs/carplay/00_ARCHITECTURE.md`](docs/carplay/00_ARCHITECTURE.md).

---

## Reproducibility bar

Install needs a root shell over USB-NCM — which the stock firmware leaves open (a telnet/SSH login with no password set, as shipped; no keys or certificates are involved), so what bounds it is the local USB link rather than a credential — or an **SPI flash programmer** for the offline recovery path
(also the brick-proof recovery path). Lower and less invasive than any field alternative (no
phone modification, no soldered chip). See [`docs/ops/01_RECOVERY.md`](docs/ops/01_RECOVERY.md).

---

## Layout

| Path | Contents |
|---|---|
| `docs/carplay/`, `docs/wireless/`, `docs/host/`, `docs/ops/` | The whole corpus: 21 documents in four categories, capped at 10 each by `tools/docs_check.py`. Start at [`docs/README.md`](docs/README.md) |
| `crates/ocbm-proto/` | OCBM wire codec (envelope, channels, Reassembler, CRC-32) — shared by box + host |
| `crates/vendor/` | Ten crates vendored in on 2026-07-13 to make the tree self-contained. Provenance is mixed: `receiver`, `pairing`, `rtsp`, `mfi`, `eld-codec` and `rx-connect` came from the archived `ncm_carplayd` tree; `iap2-core`, `metadata` and `wireless` came from the `carplayd` tree; `mfi-i2c-local` was written here (a port of `wireless/src/mfi_local.rs`, split out because `receiver`/`mfi` are `#![forbid(unsafe_code)]`). Eight are path-dependencies only (`Cargo.toml` `exclude`); `wireless` and `rx-connect` are workspace members producing the shipped `carplay-wireless` (BT/WiFi wireless-CarPlay stack) and `rx-connect` (mDNS `_airplay._tcp` advertiser) box binaries |
| `ccpa/ocbmd/` | Box OCBM daemon (armv7-musl), over `/dev/usb_accessory`: CTRL `0x00` (incl. session-control SUBSCRIBE/heartbeat/presence → `/tmp/host_present`) / MFI `0x01` / CONSOLE `0x02` (root PTY) / IP `0x10` / FILE `0x11` / ETH `0x12` / VIDEO `0x20` / MEDIA_AUDIO `0x21` / ALT_AUDIO `0x22` (voice sink, seam `:9003`) / METADATA `0x23` (seam `:9004`) / ALT_VIDEO `0x24` (cluster screen, seam `:9005`) / INPUT `0x30` (HID uplink) / MIC `0x31` (mic uplink) / MGMT `0x40` (the app's "CCPA" tab) / RTSP `0x41` (app-driven SETUP relay, seam `:9106`) / ECHO `0xFF` / DISCARD `0x0FFF`. Authoritative list: `crates/ocbm-proto/src/lib.rs` |
| `ccpa/iap2d/` | Box iAP2 accessory daemon (armv7-musl): handshake + local-i2c MFi → Identify |
| `ccpa/airplayd/` | Box AirPlay pairing daemon (armv7-musl): reuses receiver_core `ControlServer` + `LocalMfiSigner` (local i2c) → pair-setup/verify on `ncm0:5000` → derives the session key → forwards **encrypted** A/V + hands the per-stream key; disk-backed PeerStore (`/etc/carplay_peers.bin`) |
| `ccpa/rootfs/` | Deployable box rootfs overlay (stripped scripts + boot config): boot chain `ocbm_boot.sh` (launches ocbmd + supervisor) + `early_console.sh` (UART recovery shell) |
| `host/ocbm-host/` | Host OCBM client (rusb/libusb): hello/echo/mfi/ip/console/settime/push/bridge/session/**avdec** (debug receiver: SUBSCRIBE + heartbeat + decrypt A/V) |
| `accessory_init/` | `iap_role_switch.c`/`.armv7` (45 lines) — issues Apple's standard `0x51` host-role USB control request over raw usbfs (no libusb, so it runs on the stripped appliance). **First-party, implemented from documentation**, carried over from this owner's own `ncm_carplayd/ccpa/probes/iap_trigger.c`; the only C binary the box ships (built by `build.sh`, installed to `/usr/bin`) |
| `tools/` | Dev/ops. **Current provisioning path:** `ncm_base_install.sh` (Carlinkit stack out, owned boot path, USB-NCM root shell) then `ocbm_install.sh` (place → verify → reboot → reversible trial → finalize; `--full` also installs `iap2d`/`airplayd`/`rx-connect`/`carplay-wireless`, `session_supervisor.sh`/`projection_up.sh` **and** the `radio_detect.sh`/`radio_hal.sh`/`radio_ap_up.sh` seam). `install_fhs.sh` is the older OCBM-era FHS install and still works, but places only `ocbmd`/`iap2d`/`airplayd`/`rx-connect` + `iap_role_switch` — no `carplay-wireless`, no radio seam. Also: `session_supervisor.sh` + `projection_up.sh` (lifecycle actor + IDLE→projection bring-up), `uart_push.sh` (UART file deploy over the serial console), `boxsh.py`, script auditor, UART pad finder |
| `ccpa/aa-bridge/` | Box Android Auto USB bridge (armv7-musl): AOAP switch + raw byte pump between the phone's bulk endpoints and the host, over OCBM `CH_IP`. No AA protocol knowledge — the engine is app-side |
| `crates/box-common/` | Protocol-agnostic box layer shared by the CarPlay and Android Auto sets: usbdevfs primitives, phone-type detection, the single projection-owner arbitration flag, and the app-pushed config levers |
| `c2air/` | C2Air (Allwinner V821, riscv32) — a second adapter. OCBM proven; `btattach` is deliberately the only board-specific Rust |
| `pizero/` | Raspberry Pi Zero 2 W bring-up. Measurement only — no OCBM port yet, and the board facts differ from the CCPA |
| `host/aa-headunit/` | Rust Android Auto head-unit reference client (TCP / `adb forward`) — the de-risking path the macOS engine was built against |
| `host/CarPlayHost/` | The shipping macOS host app (Xcode project `carlink_macOS`) — VideoToolbox decode, audio, touch/media-key uplink, Settings/YAML, OCBM client |

---

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
`~/Documents/carlink/carlink_macOS`. The original macOS host app for the *stock* CCPA. Shows
the isolated-USB-claim integration model we mirror: `USB/USBTransport.swift` (IOKit
`IOUSBInterfaceInterface300` bulk claim, no OS interface), `Protocol/MessageTypes.swift` (the
`0x55AA55AA` 16-byte header + typed message catalog OCBM replaces), `Video/H264Decoder.swift`
(VideoToolbox — we go HEVC), `Audio/`, `App/AppDelegate.swift`. The new host app is rebuilt from this.

**`CPC200-CCPA_resources` — community interoperability documentation for the original firmware + protocol.**
`~/Downloads/github/CPC200-CCPA_resources`. Authoritative for how the stock box worked:
`documentation/02_Protocol_Reference/{usb_protocol,video_protocol,audio_protocol,carplay_handshake,
inbound_session_sequence,command_ids}.md`, `documentation/04_Implementation/host_app_guide.md`,
`documentation/01_Firmware_Architecture/hardware_platform.md`. Documents the `0x55AA55AA` framing, the
full message catalog, video (H.264 + 20-byte sub-header) / audio (PCM + 12-byte sub-header), and the
**host config flow** (`Open` 0x01, `BoxSettings` 0x19, `SendFile` 0x99 → AirPlay SETUP `viewAreas`) —
the field set our YAML absorbs. NOTE: the stock box *terminated + stripped*; we mirror its transport
model, not its scope.

**`ncm_carplayd` — proven Rust CarPlay receiver + CCPA helpers to reuse.**
`~/Documents/carlink/old/ncm_carplayd`. `receiver_core/` = independent, from-documentation Rust receiver (RTSP,
SRP/25519 pairing, MFi-SAP, ChaCha20 decrypt, typed A/V seams) — the code we cross-compile to the box.
`macos/{carplay-app,carplay-hud}/` = the SwiftUI/AppKit consumer apps (IPC-seam consumers) the host app
descends from. Its `ccpa/` bring-up helpers = the USB host-role switch helper, `iap2_auth.c` (the proven box-side iAP2 auth reference),
the MFi auth-service helper, and `ncm_bridge.c`. `ccpa/scripts/` = proven cold-start orchestration.

**`carplayd` — Apple-spec iAP2 core + reinforcing session reference.**
`~/Downloads/github/carplayd/rust/carplayd`. `crates/iap2-core/` = the transport-agnostic
iAP2 link/state/message/spec crate `iap2d` depends on (`src/spec.rs` machine-generated from the Apple
plugin; `src/link.rs` holds the coalesced-read fix from this project). `pi/iap2_pi.c` = the C the crate
was ported from.

**Chip backups — both in-repo and out.** The clean owned **baseline** IS tracked in this repo at
`ccpa/backup/base/`: the full 16 MB SPI NOR image (`CPC200-CCPA_full_nor_16MB.bin`) plus per-region
`mtd0`/`mtd1`/`mtd2`, `rootfs.tar.gz`, `SHA256SUMS` and `manifest.txt` — see its own `README.md` for the
programmer restore procedure. Additional **per-device / per-session** factory and pre-change backups are
kept outside the repo at `~/Downloads/ccpa_backups` (and on the box's USB stick at
`/mnt/UPAN/ccpa_backups/`). All of it exists solely for recovery.
