> **SUPERSEDED as a description of the live box (note added 2026-08-16).** This file describes the
> 2026-07-08 archive in this directory and is accurate for it — do not rewrite it. The unit has since
> moved on twice: OCBM is now the boot default (`/script/ncm_only` **or** `/script/ncm_wifi` selects
> NCM instead), and the IW416-only `wlan_on`/`bt_on` path was replaced by the chipset-neutral radio
> seam `radio_detect.sh` / `radio_hal.sh` / `radio_ap_up.sh`. See `../../README.md`,
> `../../../docs/wireless/01_BT_AND_RADIO.md` and `../../../tools/ocbm_install.sh`.

# CPC200-CCPA Starting Baseline
State: **BASELINE** — the clean owned base. Stock projection stack (riddleBox) removed;
CarlinkBt/latent-trigger scripts deleted; all remaining scripts owned, **dead-code-free
and syntax-clean** (`sh -n` all pass, zero dead references, zero dangling refs); early
self-respawning UART console (`::sysinit` before rcS, 115200); radios on-demand (IW416
wlan_on/bt_on, slim scripts); NCM always-on. No backup files reside in rootfs.
Device: A15W "carlink", NXP i.MX6UL, kernel 3.14.52, sw 2025.10.15.1127, iAP2 `serialNumber` 2025.02.25.1521626a (a firmware build string, not a device serial;
CPC200-CCPA_resources `documentation/01_Firmware_Architecture/version_comparison.md`).

This was the starting baseline CCPA rootfs from which OCBM was built ontop of.

## Files
- CPC200-CCPA_full_nor_16MB.bin : complete 16MB SPI NOR image (flash @ 0x000000), mtd0+mtd1+mtd2 — **kept local, not in this repo** (see below)
- mtd0.bin (uboot,  256K)  @ 0x000000
- mtd1.bin (kernel, 3328K) @ 0x040000
- mtd2.bin (rootfs, 12800K)@ 0x380000 — **kept local, not in this repo**
- rootfs.tar.gz : live jffs2 rootfs contents (top-level dirs; excludes /proc /sys /dev /tmp) — **kept local, not in this repo**
- rootfs.files.txt : the file list of rootfs.tar.gz (`tar tzf | sed 's|^\./||' | grep -v '/$' | sort`), names only; added 2026-10-20 so `tools/ncm_base_install.sh` baselinediff keeps working without the tarball

**Removed from the repo 2026-10-20 (owner decision): the three files that hold the rootfs embed device
credentials** (dropbear private host keys, root password hash, Wi-Fi configs). They were `git rm`'d;
earlier commits still contain them, and the same bytes were already published through the mirror and
as CPC200-CCPA_resources `flash_dumps/CCPA_stripped/` (removed there the same day). Byte-identical local
copies, same sha256 (also listed in `SHA256SUMS`):

| File | sha256 | Local copy |
|---|---|---|
| `CPC200-CCPA_full_nor_16MB.bin` | `edd024536c6f71d8038aa1c9be7fe6379610ba9486636e655fe058300c9fc761` | `~/Documents/carlink/carlinkit_misc/ccpa_backups/_removed_from_repo_2026-10-20/ccpa_custom/ccpa/backup/base/` |
| `mtd2.bin` | `4250029cdf1edac27e47e101fd06aacec7ff2b5b2658ccb78a50cdd9ea1e32ea` | same folder |
| `rootfs.tar.gz` | `32b48dc174e0b1cbaa2bf5680a99271afc8a75f502cf738e7d5407d2b7ea7800` | same folder |

The original 07-08 backup is also at `~/Documents/carlink/carlinkit_misc/ccpa_backups/20260708_042628_vanilla_state/`.
A restore uses the local copies; only `mtd0.bin`/`mtd1.bin` are in this folder.
- manifest.txt  : device state at backup time (process list, /script, /usr/sbin, gadget/radios)
- SHA256SUMS

## Notes
- mtd0 (uboot) and mtd1 (kernel) are **bit-identical** to prior backups (this pass touched neither the HAB-signed
  U-Boot nor the stock kernel; corrected 2026-10-20: this said they are "immutable", but the kernel can be
  replaced through U-Boot's staging path, `../../../baseline/README.md`) — verified by sha256.
- mtd2 (rootfs) is the vanilla owned base (this pass's endpoint).
- What changed from stock: removed check_mfg_mode/check_log_size/init_bluetooth_wifi/
  close_bluetooth_wifi/start_iap2_ncm from `/script` (the inert `/etc/riddle.conf` is still present in
  `rootfs.tar.gz` — with the riddleBox stack gone nothing reads it); slimmed attach_bluetooth
  (IW416-only) and
  start_bluetooth_wifi (AP-only, wlan0-DHCP bug fixed); rewrote start_main_service (242 -> 72 lines,
  50 of them non-comment; all projection/vendor launches removed); added early_console.sh (Lever 1);
  ownership headers + dead-line removal across the retained scripts.

## SPI programmer restore (Macronix MX25L12835F, 16MB SOP8)
Write CPC200-CCPA_full_nor_16MB.bin to chip offset 0x000000 (full-chip erase+program+verify).
Or per-region: mtd0->0x0, mtd1->0x40000, mtd2->0x380000.
Both use the local copies above (`CPC200-CCPA_full_nor_16MB.bin`, `mtd2.bin`).
