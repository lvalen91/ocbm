# host/ — host-side tools

Tools and apps that run on the machine talking to the adapter — macOS, Linux, and (since 2026-08-14)
Android/AAOS.

| Path | What it is |
|---|---|
| `accbench.c` | libusb throughput **benchmark** for the `/dev/usb_accessory` pipe — prints the device's USB descriptors and measures IN/OUT throughput. Proven the transport: VID 0x1314, IF0 class 0xFF, EP IN 0x83 / OUT 0x02, 512 MPS; **339 Mbps read / 90 Mbps write**. |
| `uart_cmd.sh` | drive the box's serial root console from the host (single-fd, 115200) — send a command, capture output. Works when NCM is down / during partial boots. |
| `ocbm-host/` | the host end of the OCBM multiplexer (Rust) — claims the accessory interface, demuxes channels (IP tunnel, MFi bridge, A/V, control). Subcommands: `settime`, `echo`, `rtt`, `mfi`, `console`, `ip`, `srcbench`, `sinkbench`, `bridge`, `av`, **`avdec`**, `session`, `setup-relay`, `push`, `pull`. (`hello` is the DEFAULT mode string rather than a dispatch arm: every invocation sends HELLO and pushes the box clock before the `match`, and "hello" falls through it — so `ocbm-host hello`, like a bare `ocbm-host`, is a HELLO-plus-clock-push run and nothing more.) |
| `aa-headunit/` | Minimal **Android Auto head-unit client** for bench-testing the AA projection protocol against a phone's own developer head-unit server — no Carlinkit box in the loop. See [`aa-headunit/README.md`](aa-headunit/README.md). |
| `CallSim/` | **Fake phone calls** for Android Auto telephony testing — a self-managed Telecom `ConnectionService` whose calls Telecom, the dialer and gearhead treat as real. See [`CallSim/README.md`](CallSim/README.md). |
| `mfi-probe/` | Host-side exerciser for `mfid`: points at the box over USB-NCM and confirms the coprocessor answers with a real certificate and signature. The direct analogue of `CH_MFI`, minus OCBM. |
| `MacHost/` | the shipping **macOS host app** (Swift, Xcode project `carlink_macOS`) — drives **both CarPlay and Android Auto**: decrypts + decodes/renders the forward-encrypted A/V, input uplink, metadata, and app-driven SETUP (both transports since 2026-08-10). The OCBM reimplementation of the legacy riddlebox-firmware "Carlink macOS" app; renamed from `CarPlayHost/` 2026-09-08 since it is not CarPlay-only. See [`MacHost/HOSTAPP.md`](MacHost/HOSTAPP.md). |
| `gm_ccpa/` | **Wireless CarPlay as an unprivileged AAOS app** on a 2024 Silverado (GM Info 3.7, API 32). The phone streams over the **vehicle's own hotspot**; the adapter is reduced to the Bluetooth radio and the MFi coprocessor, and carries no media. Device-proven end to end — pairing, HEVC 2400x960 hardware decode, audio, touch, drive-restricted UI and day/night. Merged in 2026-09-08 by `git subtree add`. See [`gm_ccpa/README.md`](gm_ccpa/README.md). |
| `CarlinkAndroid/` | the **AAOS head-unit host app** (Kotlin) for GM gminfo3.7 — full OCBM, wireless CarPlay over the adapter's own radios. A graft of `carlink_native_personal` (UI/media), `gm_ccpa/` (head-unit-proven OCBM + HEVC/AAC renderers) and new seam/decrypt code. See [`CarlinkAndroid/OCBMANDROID.md`](CarlinkAndroid/OCBMANDROID.md). |

## Build

```
# accbench (macOS/Linux, needs libusb):
clang accbench.c -o accbench -I/opt/homebrew/include -L/opt/homebrew/lib -lusb-1.0
DYLD_LIBRARY_PATH=/opt/homebrew/lib ./accbench info 1314        # dump descriptors
./accbench read 1314 1520 6 262144                              # bulk-IN throughput
```

## Implemented

The two "planned" tools landed as `ocbm-host/`:
- host OCBM multiplexer client (claims the accessory interface, demuxes IP tunnel / MFi bridge /
  A/V / control).
- the `ocbm-rescue` role is `ocbm-host console` — `MODE_SELECT { CONSOLE }` bridging a root PTY
  over the bulk pipe (no NCM/WiFi needed). See [`../docs/ops/01_RECOVERY.md`](../docs/ops/01_RECOVERY.md).
