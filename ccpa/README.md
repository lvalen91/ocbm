# ccpa/

Only `backup/` is left here. The rest of the former `ccpa/` moved in the 2026-11 reorganisation:

- `backup/base/` stock-firmware evidence for the CCPA bench unit (partition dumps, checksums, manifests).
  Evidence, not product code. It stayed because `tools/sync_mirror.sh` pins its paths (`PROTECTED`) and
  `tools/ncm_base_install.sh` reads it. The stock images embed device credentials and are already public;
  see `PROTECTED` in `tools/sync_mirror.sh`.
- Box daemons (`ocbmd`, `iap2d`, `carplayd`, `mfid`, `aa-bridge`, `aa-wireless`) are in `box/`.
- CCPA board material (rootfs, overlay, tools) is in `boards/ccpa-imx6ul/`.
