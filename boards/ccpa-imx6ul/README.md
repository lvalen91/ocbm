# boards/ccpa-imx6ul — CCPA (NXP i.MX6UL class, ARMv7) board material

> **Classification:** CCPA board material split out of `ccpa/` on 2026-11-02: `rootfs/` and `overlay/` live here, `tools/` holds the
> CCPA-only `iap_role_switch.c` (moved from `accessory_init/`). The board-independent daemons are in [`../../box/`](../../box/README.md). The stock
> `mtd0`/`mtd1` dumps (`ccpa/backup/`) stay where they were. See [../../README.md](../../README.md#roles-and-boards).

The CCPA runs on top of the fixed vendor floor
(HAB-signed U-Boot, never written: env-block erase only; stock 3.14.52 kernel, unmodified by this userspace and not part of the
frozen floor. A mainline
kernel can replace it through U-Boot's staging path, a separate track in `../../baseline/`; corrected
2026-10-20). See [`../../docs/carplay/00_ARCHITECTURE.md`](../../docs/carplay/00_ARCHITECTURE.md).

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
[`../../docs/wireless/01_BT_AND_RADIO.md`](../../docs/wireless/01_BT_AND_RADIO.md)
and `../../CLAUDE.md` before pushing any radio file by hand.

These are the **owned, vanilla-stripped, dead-code-free** scripts (2026-07-08 audit, extended
2026-08-15 with the radio seam): all `sh -n` clean, no dead references.

| File | Purpose |
|---|---|
| `etc/inittab` | boot table — **reference copy; the installers DERIVE the on-box file.** `::sysinit:/script/early_console.sh` (before rcS), `console::sysinit:/etc/init.d/rcS`, the two OCBM `::respawn` wrappers (`run_ocbmd.sh`, `run_supervisor.sh` — the task-#28 respawn protection, appended on-box by `ocbm_install.sh`), askfirst/restart/ctrlaltdel, `::shutdown:/script/after_shutdown.sh` |
| `etc/init.d/rcS` | boot: mount, dropbear/telnetd, mdev, sysctls, → `start_main_service` |
| `etc/mdev/usb_mount.sh` | the one mdev USB-storage hook (both sd rules, add + remove): vfat at `/mnt/UPAN`, idempotent, unmounts only its own device, `flock`-serialised. Replaced the stock `udisk_insert.sh`/`udisk_hotplug.sh` 2026-10-19; `ncm_base_install.sh bootpath` rewires `/etc/mdev.conf`, `strip` deletes the stock hooks |
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
| `script/copy_to_tmp.sh` | stage gadget kernel modules (`ko.tar.gz`) to /tmp — extracts EVERY member, which is why the Apple-gadget ones are filtered out of the archive itself (2026-10-19, see `script/ko.tar.gz`) |
| `script/mount_usb.sh` | manual USB-stick mount + staging area |
| `script/after_shutdown.sh` | shutdown: sync + unmount |
| `script/sync_box_time.sh` | NTP time sync (US IP servers) |
| `script/uart_console.sh` | (superseded by `early_console.sh`; retained) |
| `script/ko.tar.gz` | gadget kernel modules — binary artifact, members `g_android_accessory.ko` + `storage_common.ko`. **Reference only, never installed:** the bundle is built against each unit's own kernel (vermagic), so no installer ships it. *(2026-10-19, owner decision — this is an OCBM board now):* `tools/ncm_base_install.sh strip` FILTERS the unit's own `/script/ko.tar.gz` in place, dropping HeWei's Apple-gadget members `g_iphone.ko`, `f_ptp.ko`, `f_ptp_appledev.ko`, `f_ptp_appledev2.ko` (and their `/tmp` copies) and keeping every other member byte-identical (stock also carries `cdc_ncm.ko` and the `snd-soc-*` codec modules). It refuses, leaving the original untouched, unless the result lists cleanly and still carries both members above; the original stays as `ko.tar.gz.pre` until `tidy`. OCBM never used them: the phone-facing gadget is the built-in `g_android` (`android0`, `iap2,ncm`) — docs/carplay/07_PHONE_SIDE.md. `tools/audit_scripts.sh` flags any script still naming `g_iphone`/`f_ptp`. This overlay copy already had none of the four |

Runtime triggers (not committed): `/script/ncm_only` **or** `/script/ncm_wifi` selects an NCM boot;
with **neither** present the box boots the OCBM appliance. Always test
`[ -e /script/ncm_only ] || [ -e /script/ncm_wifi ]` — never `ncm_only` alone.
`ncm_base_install.sh --wifi-backstop` leaves only `ncm_wifi`, which is the trap both
`box/mfid/src/main.rs` (`NCM_FLAGS`) and `tools/run_mfid.sh` call out.

> **`script/run_ocbmd.sh` divergence (noted 2026-08-16):** `tools/ocbm_install.sh` installs
> `tools/run_ocbmd.sh`, **not** this overlay copy — the two have diverged (the overlay copy gained a
> deploy dead-man on 2026-08-15 that no installer ships). `ocbm_boot.sh` is the mirror image: the
> overlay copy is the one installed, and the stale `tools/ocbm_boot.sh` duplicate was deleted on 2026-11-02 (reorganisation phase 1).
