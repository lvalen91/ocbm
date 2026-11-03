# 01 — Roles, boards and MFi source

> **STATUS:** CURRENT

Vocabulary: **CCPA** means only the Carlinkit CPC200-CCPA box family (every unit an NXP i.MX SoC, ARMv7, differing
Wi-Fi/BT radios). **C2Air** is a different box (Allwinner V821, rv32; one unit). Neither is a variant of the other,
and the Pi / Orange Pi ports are not CCPA variants.

## Three roles

| Role | What runs where | Hardware today | Code that serves it |
|---|---|---|---|
| **R1. Replacement-firmware box** | Full OCBM stack as the box's own userspace (and, with `baseline/`, its own mainline kernel/rootfs) | CCPA family; C2Air V821 (rv32, one unit, personal reference) | box daemons + a board directory + `baseline/` (CCPA only) |
| **R2. CCPA as peripheral** | The CCPA supplies MFi (for `host/gm_ccpa` also the Bluetooth radio); the session runs on another computer | Pi 4 AAOS and a Linux workstation (MFi from a CCPA via `mfid`, own radios); GM radio AAOS (`host/gm_ccpa`) | `mfid`, `mfi-wire`, `ocbmd` `CH_MFI` / `btd` on the CCPA; the SBC or app on the other side |
| **R3. Host apps and clients** | Decode/render, UI, vehicle profile; speak OCBM to a box | macOS app, Android/AAOS app suite, gm_ccpa app, CLI tools | `host/**` |

A fourth, forward-looking target is R1 on a non-Carlinkit SBC (Pi Zero 2 W bring-up exists), with or without a local MFi
I2C chip, MFi version not assumed. Adding a board should be a directory plus a `board.env`, not a restructure.

## Boards

`ccpa-imx6ul`, `c2air-v821b`, `rpi4-aaos`, `rpi-zero2w`; later `opi-*`. `baseline/arm-imx6ul/board.env` is the first
real `board.env` (key `BOARD=arm-imx6ul` is not renamed). Board directories and their paths are listed in the top-level README.

## MFi source (a documentation model, not a config key that code reads)

| Value | Meaning |
|---|---|
| `local-i2c` | chip on `/dev/i2c-1@0x11`, serialised on `/tmp/carplay_mfi.lock` (`mfi_local.rs`, `LocalMfiSigner`, `mfi-i2c-local`, ocbmd `CH_MFI`) |
| `remote-mfid` | TCP `MFI1` to `mfid` on a chip-bearing CCPA via `CARPLAY_MFI_ADDR` |
| `host-provided` | the host app relays cert/sign over OCBM `CH_MFI` (gm_ccpa uses this arrangement from the app side) |
| `none` | not implemented. With `CARPLAY_MFI_ADDR` unset the daemons attempt local I2C and fail if no chip is present; there is no explicit refusal state (`crates/mfi-wire/src/lib.rs` `ADDR_ENV`, `crates/vendor/wireless/src/mfi_local.rs` `REMOTE_ADDR_ENV`) |

The only runtime selector in code is the `CARPLAY_MFI_ADDR` environment variable (set: remote `mfid`; unset: local
`/dev/i2c-1`). The keys `OCBM_ROLES`, `MFI_SOURCE`, `MFI_CHIP` and `TARGET_TRIPLE` in `baseline/arm-imx6ul/board.env`
(decided 2026-11-02) are **declarative metadata**: no daemon, script or installer reads them; the only consumer is
the value check in `baseline/common/tools/selftest.sh`, so a typo cannot go unnoticed.

The MFi chip version is not a setting at all: OCBM and the MFi chain support MFi 2.0 through 3.0 and are not specific to
one variant. Measured so far: CCPA bench unit 2.0C (945 B certificate, 128-byte RSA signature); C2Air 3.x (608 B
certificate, 64-byte ECDSA signature); future SBCs expected 3.0. Whether a 3.x chip accepts the 20-byte SHA-1
digest the MFi-SAP paths produce, and whether it uses the 2.0C register map, is **unverified**. This selection is
configuration, not new box code (owner, 2026-08-10 "dumb box" directive).
