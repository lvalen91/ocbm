# CarPlay-specific AOSP patches

Patches to the AAOS source tree that belong to **this** project rather than to
the board.

The platform corrections — `miniuart-bt`, the Wi-Fi country code and the
`setCountryCode` HAL fallback, USB audio, `/dev/media0` sepolicy, the HEVC
decoder fast path, `CONFIG_BT_RFCOMM=y` — are **not** here. They are board facts
with no CarPlay content and live in the platform repo:

> **github.com/lvalen91/RPi_AAOS16** → `patches/`

Apply that first, then this.

```sh
# platform
git clone https://github.com/lvalen91/RPi_AAOS16
RPi_AAOS16/patches/apply.sh ~/aosp
RPi_AAOS16/patches/build_kernel_rpi4.sh ~/aosp

# then this project's bits
git -C ~/aosp/device/brcm/rpi4 apply pi/patches/device_brcm_rpi4/*.patch
```

## What's in here

**`device_brcm_rpi4/`**

- `carlink/privapp-permissions-com.carlink.projection.xml` — the privileged
  permission allowlist for the projection app. It has to be in the image
  **before** the APK reaches `/system/priv-app`: an unallowlisted privileged
  permission is boot-blocking, not a silent denial. Without it
  `addKeyEventHandler` is denied, and the steering-wheel voice and call keys
  never reach CarPlay.
- `carlink/carlink_boot.sh` + `carlink_eth0.rc` — static address on `eth0`.
  Nothing runs DHCP on the direct link to the workstation and Android's
  `EthernetManager` never claims `eth0`, so adb-over-TCP on the wire is
  otherwise unreachable. The init service needs `seclabel u:r:su:s0`: init
  refuses to start a shell service because `shell_exec` has no domain transition
  from init, and **that check fires even under permissive SELinux** — the
  service simply never runs, with nothing obvious in the log.

Both are bench/product specific, which is why they are split out.

## Image provenance

The image these were built against:

| | |
|---|---|
| Built | 2026-08-22 |
| Base | `android-16.0.0_r4` + raspberry-vanilla `android-16.0` |
| Kernel | `6.12.73-v8-gb9c076007b21` (`8af794a959ec` + `CONFIG_BT_RFCOMM=y`) |
| Target | `aosp_rpi4_car-bp4a-userdebug` |

Record the platform repo commit here when it is tagged, so this image is
reproducible from the two repos alone.
