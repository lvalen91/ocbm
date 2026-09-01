# tools/ — dev / ops helpers

Reusable helpers for working on the adapter (not part of the shipped userspace).

| File | What it does |
|---|---|
| `ncm_base_install.sh` | take an NCM-capable CPC200-CCPA to the owned NCM base over the NCM link: preflight → backup → owned boot path → cold test → strip the Carlinkit stack → audit. Chipset-safe by construction — the install lists never carry another chip's bring-up (the IW416-only `wlan_on`/`bt_on`/`attach_bluetooth`/`start_bluetooth_wifi` are never pushed), and `is_radio()` re-checks every candidate immediately before deleting it, printing `SKIP (WLAN/BT guard)` — so no unit inherits the wrong radio support or loses its own (see `../docs/wireless/01_BT_AND_RADIO.md`). |
| `audit_scripts.sh` | run on the box: `sh -n` syntax-check every script + scan for dead references to removed components + dangling file refs. Used for the 2026-07-08 vanilla audit. |
| `uart_beacon.sh` | run on the box: emit a low-churn 1 Hz marker on `/dev/ttymxc0` to find the UART TX pad (watch the counters in `/proc/tty/driver/IMX-uart`). |
| `cold_start2.sh` | run on the box: phone-side wired-CarPlay cold start — polls for an iPhone replug, issues Apple's standard `0x51` host-role USB control request (`../accessory_init/iap_role_switch.armv7`) so the iPhone enters accessory host mode, presents the `iap2,ncm` gadget, and runs `iap2d` through iAP2 auth + identify. Needs `iap2d` + `iap_role_switch` pushed to `/tmp` first. **Do not run any host CH_MFI/`ocbm-host mfi` traffic during the sign window** (I2C contention drops the iPhone mid-auth). See [`../docs/carplay/07_PHONE_SIDE.md`](../docs/carplay/07_PHONE_SIDE.md) and HANDOFF "RESUME HERE". |
| `uart_push.sh` | deploy a file to the box over the UART root console when OCBM is unavailable (app closed → accessory de-enumerated): gzip + base64 stream with XON/XOFF flow control, decode + gunzip on the box, md5-verify end-to-end. Slow but reliable. `uart_push.sh LOCAL REMOTE [mode]`. |

Deploy pattern for the box binaries: cross-build with **`../build.sh`** (never a bare `cargo
zigbuild` — Homebrew's `rustc` shadows rustup's and has no armv7-musl std, and airplayd's eld-codec
needs the `CC`/`AR` zig pair `build.sh` sets) → UPX **3.96** pack in the Lima
`ccpa-build` VM (host UPX 5.x segfaults the box's 3.14 kernel), `upx -t` verify → push with
`uart_push.sh` (or OCBM `ocbm-host push` when the app is OPEN) → install to `/usr/sbin` by `mv` over
the running binary → `reboot`. Revert gadget/boot experiments via `reboot -f` (never
`echo 0 > enable` with transfers pending — see [`../docs/carplay/00_ARCHITECTURE.md`](../docs/carplay/00_ARCHITECTURE.md)).

## `i2mspec_dump.py` — Apple's iAP2 message spec, decoded

Decodes `iap2messages-internal.i2mspecarchive` from Xcode's CarPlaySimulator plug-in: 144 messages with
ids, names, source, and the full parameter tree (ids, types, cardinality, enum values, Apple's notes).
The authority for TLV parameter ids — `crates/vendor/iap2-core/src/spec.rs` was generated from it.

    tools/i2mspec_dump.py --message 0x4158 --text     # one message, readable table
    tools/i2mspec_dump.py --message 0x4170            # JSON
    tools/i2mspec_dump.py > spec.json                 # all 144

Does not hit the network or the device; reads the local Xcode install.
