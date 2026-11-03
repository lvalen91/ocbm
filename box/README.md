# box/ — OCBM box daemons (armv7 / i.MX6UL today; riscv32 for C2Air)

> **Classification:** board-independent box userspace `ocbmd`, `iap2d`, `carplayd`, `mfid`, `aa-bridge`, `aa-wireless`
> (ocbmd/iap2d/carplayd verified building for riscv32; `mfid` is a CCPA-resident MFi oracle by design). Moved here from
> `ccpa/` on 2026-11-02; the CCPA board material (rootfs, overlay) is in
> [`../boards/ccpa-imx6ul/`](../boards/ccpa-imx6ul/README.md). See [../README.md](../README.md#roles-and-boards).

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
