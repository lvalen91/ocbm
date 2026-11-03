# box/ — OCBM box daemons (armv7 / i.MX6UL today; riscv32 for C2Air)

> **Classification:** board-independent box userspace `ocbmd`, `iap2d`, `carplayd`, `mfid`, `aa-bridge`, `aa-wireless`
> (ocbmd/iap2d/carplayd verified building for riscv32; `mfid` is a CCPA-resident MFi oracle by design). The CCPA board
> material (rootfs, overlay) is in
> [`../boards/ccpa-imx6ul/`](../boards/ccpa-imx6ul/README.md). See [../docs/ocbm/01_ROLES_AND_MFI_SOURCE.md](../docs/ocbm/01_ROLES_AND_MFI_SOURCE.md#three-roles).

The OCBM box daemons that run on the adapter, on top of the fixed vendor floor
(HAB-signed U-Boot, never written: env-block erase only; stock 3.14.52 kernel, unmodified by this userspace and not part of the
frozen floor. A mainline
kernel can replace it through U-Boot's staging path, a separate track in `../baseline/`; corrected
2026-10-20). See [`../docs/carplay/00_ARCHITECTURE.md`](../docs/carplay/00_ARCHITECTURE.md).

The C2Air is a separate box with its own tree ([`../boards/c2air-v821b/README.md`](../boards/c2air-v821b/README.md)). The daemons
`ocbmd`, `iap2d` and `carplayd` build for it from these same sources (`../boards/c2air-v821b/README.md`, daemon table);
nothing under `box/` is specific to the CCPA board. (`aa-bridge`,
`aa-wireless` and `mfid` were not checked against the C2Air build.)


## Daemons — `ocbmd/`, `iap2d/`, `carplayd/` (shipped, hardware-validated) + `mfid/` (bring-up only)

Each is a sibling crate (`Cargo.toml` + `src/`), cross-built for `armv7-unknown-linux-musleabihf`:

| Crate | Purpose |
|---|---|
| `ocbmd/` | OCBM bulk-transport multiplexer — owns `/dev/usb_accessory`, MODE_SELECT/CONSOLE, channel demux + per-stream drain to the host over USB |
| `iap2d/` | iAP2 / CarPlay control daemon (Identify, metadata — content selection is app-driven per docs/carplay/04_CAPABILITIES_AND_CONFIG.md; today's box tier levers are interim — session management) |
| `carplayd/` | AirPlay receiver — pairing; SETUP/RECORD relayed to the app over `CH_RTSP` (app-driven SETUP is the default on both transports per docs/carplay/04_CAPABILITIES_AND_CONFIG.md, the local response is the fallback; wireless asserts from the app-pushed YAML); forward-encrypted A/V forwarding — the box never decrypts media by design (in-binary floor `fwd_enc()` in `../crates/vendor/receiver/src/levers.rs`; `OCBM_FWD_ENC` is the current-state mechanism) — input uplink |
| `mfid/` | **not shipped** — an ephemeral MFi-chip service for NCM bring-up: serves the cert/sign ops over TCP so a host (the Pi) can reach the coprocessor when OCBM is not the transport. Shares the same `/tmp/carplay_mfi.lock` `flock` as `ocbmd`'s `CH_MFI`, `carplayd` and `iap2d`. Staged to `/tmp` by `tools/run_mfid.sh`, refuses to start in OCBM mode, erased by a reboot |

`overlay/` (2026-10-30) is not a crate: the boot hook (`etc/rc.ocbm`), ocbmd's launcher and the AP templates of the
**OCBM overlay** for the mainline baseline — the one image that carries these daemons and the stock scripts onto a
baseline unit, outside its GPL rootfs (`../tools/ocbm_overlay.sh` builds it; `../baseline/README.md` §2.8, §7.0e).

The MFi authentication bridge and radio/link glue live in the vendor crates under `../crates/vendor/`.
See [`../docs/ocbm/00_PROTOCOL.md`](../docs/ocbm/00_PROTOCOL.md) and [`../docs/ops/04_OPEN_ITEMS.md`](../docs/ops/04_OPEN_ITEMS.md).
Build: **`./build.sh`** from the repo root. Do not invoke `cargo zigbuild` bare — Homebrew's `rustc`
shadows rustup's and has no `armv7-unknown-linux-musleabihf` std, and carplayd's eld-codec needs the
`CC`/`AR` zig pair that `build.sh` sets. `FDK_AAC_PREFIX` defaults to `$PWD/scratchpad/fdk/install`;
without an fdk-aac prefix `./build.sh` exits 1, so run `SKIP_CARPLAYD=1 ./build.sh` (everything except carplayd).

## Overview

**What it is.** The CPC200-CCPA is a small ARM Linux dongle that plugs into a car's USB port; a phone
connects to it and the car gets **phone projection** — CarPlay from an iPhone, Android Auto from an
Android phone. This project replaces the dongle's closed userspace with open code, and pairs it with a
host app — on a Mac, or on an Android Automotive head unit — that does the decode and rendering. Both
projection protocols ride the same open transport, **OCBM**: the dongle owns the stable,
hardware-bound half, the app owns the half that changes with every phone-OS release.

An open, Rust-first replacement for the CCPA's stock projection stack (Carlinkit "riddleBox":
`ARMadb-driver` / `AppleCarPlay` / `ARMiPhoneIAP2`), running as ordinary userspace on the same
hardware. It reuses the adapter's **hardware** — WLAN/BT radios and the onboard genuine Apple
**MFi** authentication coprocessor (a 2.0C on the CCPA bench unit; the stack supports MFi 2.0 through 3.0; `/dev/i2c-1 @0x11`), used the way Apple's accessory
program intends — and its **factory vendor boot stack** (HAB-signed U-Boot + kernel 3.14.52, whose
first 192 KB are DCP-encrypted per chip with an OTPMK-derived key; the rest is a plain zImage). The signed U-Boot is never modified and is treated as fixed platform firmware (a
BIOS); the vendor kernel stays the default on production units, and `baseline/` can replace it with a
mainline kernel through U-Boot's own staging mechanism.
Everything above the kernel is rewritten open-source.

> **The problem this solves:** the stock CCPA ships an older, deliberately stripped CarPlay
> implementation — old SDK, H.264 only, features dropped. Replacing the userspace gets current,
> full-featured HEVC CarPlay, and keeps it current: new features land app-side. No phone
> modification, no custom PCB, no soldered MFi chip.

> **Android Auto:** the same split also carries **Android Auto, wired and wireless** — wired: the box
> does the AOAP USB switch and pumps raw bytes; wireless: the box pairs as a Bluetooth headset (HFP),
> the phone dials the box's own AA record, then the phone joins the box's AP and streams over TCP. The
> host runs the AA protocol engine and decodes either way, over OCBM. See `docs/androidauto/00_ARCHITECTURE.md`.

## Architecture in brief

Full detail: [`docs/carplay/00_ARCHITECTURE.md`](../docs/carplay/00_ARCHITECTURE.md). The governing
doctrine is app-driven configuration ([`docs/carplay/04_CAPABILITIES_AND_CONFIG.md`](../docs/carplay/04_CAPABILITIES_AND_CONFIG.md)):
anything configurable is owned by the app, the box presents what the app pushes, and box placement is
earned rather than designed in.

**The split is by configurability, not by transport** (per the doctrine in `docs/carplay/04_CAPABILITIES_AND_CONFIG.md`:
anything capable of multiple values is app-side, even if the protocol part is stable). The adapter owns the hardware-bound parts Apple keeps
backward-compatible forever — the iAP2 accessory handshake, MFi authentication (2.0 through 3.0) on the on-board
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
[`docs/ocbm/00_PROTOCOL.md`](../docs/ocbm/00_PROTOCOL.md).

**Two secrets, not one.** The MFi private key never leaves the coprocessor; all MFi crypto happens on
the box and the app sees only status. The ChaCha20 session key is derived at pair-verify, is ephemeral
and per-connection, and is what lets the app decrypt A/V the adapter forwarded untouched.

**HEVC, not H.264** — the stock box negotiated H.264 High every session. Both sides of the assertion
are driven by the app's `enablesHEVC`: the adapter emits `hevcInfo` in `/info`, and the SETUP
`enabledFeatures` echo is app-authored on both transports.
[`docs/carplay/06_AV_PIPELINE.md`](../docs/carplay/06_AV_PIPELINE.md).

**Config is an Apple-shape `VehicleConfig` YAML** held app-side and pushed at SUBSCRIBE; the box never
persists it. [`docs/carplay/04_CAPABILITIES_AND_CONFIG.md`](../docs/carplay/04_CAPABILITIES_AND_CONFIG.md).

**SETUP is app-driven on both transports**, with the box's local response as the fallback the phone
never notices.
[`docs/carplay/02_SESSION_LIFECYCLE.md`](../docs/carplay/02_SESSION_LIFECYCLE.md).

Rejected alternatives and why, so they are not re-litigated:
[`docs/carplay/00_ARCHITECTURE.md`](../docs/carplay/00_ARCHITECTURE.md).

## Requirements

- Rust with `rustup target add armv7-unknown-linux-musleabihf` and `cargo install cargo-zigbuild`
  (the box binaries cross-compile with the zig linker — no Linux VM needed).

## Getting started

| Step | Where |
|---|---|
| Build the box daemons and the host CLI | `./build.sh` (without an fdk-aac prefix: `SKIP_CARPLAYD=1 ./build.sh`; otherwise set `FDK_AAC_PREFIX`) — detail in [`docs/ops/00_BUILD_AND_DEPLOY.md`](../docs/ops/00_BUILD_AND_DEPLOY.md) |
