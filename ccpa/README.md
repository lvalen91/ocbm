# ccpa/ — box-side (armv7 / i.MX6UL)

The open CCPA userspace that runs on the adapter, on top of the fixed vendor floor
(HAB-signed U-Boot + OTPMK-encrypted kernel, unmodified). See [`../docs/carplay/00_ARCHITECTURE.md`](../docs/carplay/00_ARCHITECTURE.md).

## `rootfs/` — the box-side script overlay (source of truth for the OWNED files)

Mirrors on-box paths, but **deploying is not a blanket copy of this tree.** The installers copy a
named subset (`tools/ncm_base_install.sh bootpath`, `tools/ocbm_install.sh place`), and six files
below are deliberately **never** installed on any box: `wlan_on.sh`/`wlan_off.sh`,
`bt_on.sh`/`bt_off.sh`, `attach_bluetooth.sh` and `start_bluetooth_wifi.sh` carry the **IW416
baseline's** chipset-specific bring-up, and a CCPA ships only its own chip's drivers (at least six
WLAN/BT parts exist, with no fallback set). Each unit keeps its own vendor copies instead — no
installer ever pushes those six, and `is_radio()` in `ncm_base_install.sh` covers the other half of
the rule by shielding every radio file from the STRIP phase (`SKIP (WLAN/BT guard)`): the install
lists govern what is placed, the guard governs what is deleted. `etc/inittab` is likewise a
reference copy: both installers *derive* the on-box inittab from the unit's existing one rather than
overwriting it. Read
[`../docs/wireless/01_BT_AND_RADIO.md`](../docs/wireless/01_BT_AND_RADIO.md)
and `../CLAUDE.md` before pushing any radio file by hand.

These are the **owned, vanilla-stripped, dead-code-free** scripts (2026-07-08 audit, extended
2026-08-15 with the radio seam): all `sh -n` clean, no dead references.

| File | Purpose |
|---|---|
| `etc/inittab` | boot table — **reference copy; the installers DERIVE the on-box file.** `::sysinit:/script/early_console.sh` (before rcS), `console::sysinit:/etc/init.d/rcS`, the two OCBM `::respawn` wrappers (`run_ocbmd.sh`, `run_supervisor.sh` — the task-#28 respawn protection, appended on-box by `ocbm_install.sh`), askfirst/restart/ctrlaltdel, `::shutdown:/script/after_shutdown.sh` |
| `etc/init.d/rcS` | boot: mount, dropbear/telnetd, mdev, sysctls, → `start_main_service` |
| `etc/mdev/udisk_insert.sh` | USB-storage hotplug auto-mount → `/mnt/UPAN` |
| `script/early_console.sh` | **Lever 1** — earliest self-respawning 115200 UART root console |
| `script/custom_init.sh` | early CDC-NCM arm (primary NCM bring-up) |
| `script/start_main_service.sh` | minimal boot orchestrator (79 lines; projection cruft removed) — dispatches to `ocbm_boot.sh` when neither NCM flag is set |
| `script/start_ncm.sh` | ncm0 addr/mtu |
| `script/radio_detect.sh` | **chipset-neutral seam** — read-only detection of THIS unit's WLAN/BT platform → `/tmp/radio_caps` |
| `script/radio_hal.sh` | **chipset-neutral seam** — the radio verbs everything above calls: `probe`, `status`, `wifi_ap_on`, `wifi_ap_off`, `bt_on`, `bt_off`. `session_supervisor.sh` uses these, not the scripts below |
| `script/radio_ap_up.sh` | **chipset-neutral seam** — the owned SoftAP layer, deliberately NOT named `start_bluetooth_wifi.sh` (every unit already ships a *vendor* file at that path) |
| `script/wlan_on.sh` / `wlan_off.sh` | IW416-only WiFi AP up/down — **reference only, never installed**; reached only as `radio_hal.sh`'s `backend=owned` path on a unit that already has them |
| `script/bt_on.sh` / `bt_off.sh` | IW416-only BT up/down — **reference only, never installed** (same `backend=owned` path) |
| `script/attach_bluetooth.sh` | IW416-only BT HCI attach (slim) — **reference only, never installed** |
| `script/start_bluetooth_wifi.sh` | IW416-only AP bring-up (slim; wlan0-DHCP bug fixed) — **reference only, never installed**; `radio_hal.sh` explicitly refuses to fall back to this path. Still invoked by `custom_init.sh`'s `ncm_wifi` backstop |
| `script/ocbm_boot.sh` | OCBM appliance boot: pure-accessory gadget + `ocbmd` + session supervisor, plus the first-boot dead-man (`/script/ocbm_trial`) and the opt-in NCM failover watchdog (`/script/ocbm_failover`). Installed by `tools/ocbm_install.sh` |
| `script/run_ocbmd.sh` | inittab `::respawn` wrapper for `ocbmd` (+ a temporary deploy dead-man). **See the note under the table** |
| `script/init_gpio.sh` | BT-reset + quick-charge GPIO |
| `script/copy_to_tmp.sh` | stage gadget kernel modules (`ko.tar.gz`) to /tmp |
| `script/mount_usb.sh` | manual USB-stick mount + staging area |
| `script/after_shutdown.sh` | shutdown: sync + unmount |
| `script/sync_box_time.sh` | NTP time sync (US IP servers) |
| `script/uart_console.sh` | (superseded by `early_console.sh`; retained) |
| `script/ko.tar.gz` | gadget kernel modules (`g_android_accessory.ko`, …) — binary artifact |

Runtime triggers (not committed): `/script/ncm_only` **or** `/script/ncm_wifi` selects an NCM boot;
with **neither** present the box boots the OCBM appliance. Always test
`[ -e /script/ncm_only ] || [ -e /script/ncm_wifi ]` — never `ncm_only` alone.
`ncm_base_install.sh --wifi-backstop` leaves only `ncm_wifi`, which is the trap both
`ccpa/mfid/src/main.rs` (`NCM_FLAGS`) and `tools/run_mfid.sh` call out.

> **`script/run_ocbmd.sh` divergence (noted 2026-08-16):** `tools/ocbm_install.sh` installs
> `tools/run_ocbmd.sh`, **not** this overlay copy — the two have diverged (the overlay copy gained a
> deploy dead-man on 2026-08-15 that no installer ships). `ocbm_boot.sh` is the mirror image: the
> overlay copy is the one installed, and `tools/ocbm_boot.sh` is the stale duplicate.

## Daemons — `ocbmd/`, `iap2d/`, `airplayd/` (shipped, hardware-validated) + `mfid/` (bring-up only)

Each is a sibling crate (`Cargo.toml` + `src/`), cross-built for `armv7-unknown-linux-musleabihf`:

| Crate | Purpose |
|---|---|
| `ocbmd/` | OCBM bulk-transport multiplexer — owns `/dev/usb_accessory`, MODE_SELECT/CONSOLE, channel demux + per-stream drain to the host over USB |
| `iap2d/` | iAP2 / CarPlay control daemon (Identify, metadata — content selection is app-driven per docs/carplay/04_CAPABILITIES_AND_CONFIG.md; today's box tier levers are interim — session management) |
| `airplayd/` | AirPlay receiver — pairing; SETUP/RECORD relayed to the app over `CH_RTSP` (app-driven SETUP is the default on both transports per docs/carplay/04_CAPABILITIES_AND_CONFIG.md, the local response is the fallback; wireless asserts from the app-pushed YAML); forward-encrypted A/V forwarding — the box never decrypts media by design (in-binary floor `fwd_enc()` in `../crates/vendor/receiver/src/levers.rs`; `OCBM_FWD_ENC` is the current-state mechanism) — input uplink |
| `mfid/` | **not shipped** — an ephemeral MFi-chip service for NCM bring-up: serves the cert/sign ops over TCP so a host (the Pi) can reach the coprocessor when OCBM is not the transport. Shares the same `/tmp/carplay_mfi.lock` `flock` as `ocbmd`'s `CH_MFI`, `airplayd` and `iap2d`. Staged to `/tmp` by `tools/run_mfid.sh`, refuses to start in OCBM mode, erased by a reboot |

The MFi authentication bridge and radio/link glue live in the vendor crates under `../crates/vendor/`.
See [`../docs/carplay/01_OCBM_PROTOCOL.md`](../docs/carplay/01_OCBM_PROTOCOL.md) and [`../docs/ops/04_OPEN_ITEMS.md`](../docs/ops/04_OPEN_ITEMS.md).
Build: **`./build.sh`** from the repo root. Do not invoke `cargo zigbuild` bare — Homebrew's `rustc`
shadows rustup's and has no `armv7-unknown-linux-musleabihf` std, and airplayd's eld-codec needs the
`CC`/`AR` zig pair that `build.sh` sets. `FDK_AAC_PREFIX` defaults to `$PWD/scratchpad/fdk/install`.
