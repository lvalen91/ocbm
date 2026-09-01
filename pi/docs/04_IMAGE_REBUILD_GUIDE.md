# What to change in the AAOS tree to improve CarPlay on this Pi

Companion to `pi/docs/02`, which records what was **observed on the device**. This one is organised
the other way round — by **where the change goes in the AOSP tree** — because that is the order you
need when you have a build environment in front of you.

Platform: `RaspberryVanillaAOSP16-20260413-rpi4_car.img`, kernel `6.12.73-g8af794a959ec-v8`,
Raspberry Pi 4 Model B Rev 1.5. Upstream is the
[raspberry-vanilla](https://github.com/raspberry-vanilla) project.

**Confidence is marked per item.** *Verified* means measured on the running device (the raw capture
is in `pi/evidence/`). *Researched* means read from upstream source with a citation. *Inferred* means
reasoned from those two and not tested.

---

## Tier 1 — these delete code this port had to write

Both are worth more than everything below combined, because the maintenance burden they remove is
permanent.

### 1.1 Kernel: enable RFCOMM — *verified*

```
arch/arm64/configs/android_rpi4_defconfig
-# CONFIG_BT_RFCOMM is not set
+CONFIG_BT_RFCOMM=y
```

Verified on device: `zcat /proc/config.gz | grep CONFIG_BT_RFCOMM` → `# CONFIG_BT_RFCOMM is not set`.

`socket(AF_BLUETOOTH, SOCK_STREAM, BTPROTO_RFCOMM)` therefore returns `EPROTONOSUPPORT`, which is
why `crates/vendor/wireless/src/rfcomm_uspace.rs` exists: a hand-written TS 07.10 implementation over
L2CAP PSM 3, both directions, ~900 lines. Two of its bugs took hardware sessions to find — the credit
octet must be **excluded** from the length field (RFCOMM 1.2 §6.5.2), and the FCS span differs by
frame type (2 bytes for UIH, 3 for SABM/UA/DM/DISC).

The omission is not an oversight upstream: Android's own Bluetooth stack implements RFCOMM in
userspace over L2CAP, so AOSP has no reason to enable the module. It costs us because we bypass that
stack (§1.2 below explains why).

**After the change:** delete the `CARPLAY_RFCOMM_BACKEND=userspace` gate and the whole file. The
kernel path is already implemented in `rfcomm.rs` and was the original code.

### 1.2 Wi-Fi vendor HAL: `IWifiApIface.setCountryCode` must not return `NOT_SUPPORTED` — *verified earlier in this project*

Recorded in the earlier round on `/Volumes/stuff/rpi/aaos/docs/os-corrections-2026-08-16.md`. This is
what blocks the framework's 5 GHz SoftAP, and it forced two workarounds:

* standalone `hostapd` outside the Wi-Fi framework (`pi/tools/hostapd_5g.conf`), and
* **`pi/apdhcpd`**, a DHCP server written from scratch — because Android's bundled `dnsmasq` is
  version 2.51 (2009), a vestige left behind when tethering moved to NetworkStack's own
  `DhcpServer`. It answered `DISCOVER` with an `OFFER` the phone never received, and
  `--dhcp-broadcast` is unrecognised and **silently disables DHCP entirely** — no error, no bind.

With the HAL fixed, `CarProjectionManager.startProjectionAccessPoint()` (LocalOnlyHotspot) becomes
usable. The device already reports `Stable local-only hotspot configuration: true`. That deletes
`hostapd`, `apdhcpd`, and the `CARPLAY_HOSTAPD_CONF` gate, and inherits SSID/PSK handling from the
platform.

**Caveat, *inferred*:** LocalOnlyHotspot is a different `WifiManager` path from the tethered SoftAP
that failed for us. It may or may not hit the same blocker. Worth an hour's test before deleting
anything — `pi/docs/01` §4.3 tracks it.

---

## Tier 2 — fixes for defects measured on this hardware

### 2.1 Car audio is disabled — *verified*, and the highest product impact

```
dumpsys car_service --services CarAudioService
  Run in legacy mode? true
  Configured using audio control? false
  Rely on core audio for routing? false
  Car audio configuration path: /vendor/etc/car_audio_configuration.xml
```

`car_audio_configuration.xml` **is not parsed at all**. There are no audio zones, no volume groups,
and no dynamic routing — AAOS falls back to plain Android stream volumes.

This is the likely cause of the reported "media volume jerks and resets to a lower value": with no
volume groups, `VoiceRouter`'s deliberate group juggling (it pauses media "so the knob can reach the
voice group") has nothing coherent to act on.

**Needed:** the `android.hardware.automotive.audiocontrol` HAL packaged in `device.mk`, dynamic
routing enabled, and a `car_audio_configuration.xml` that maps CarPlay's usages —
`USAGE_MEDIA`, `USAGE_VOICE_COMMUNICATION`, `USAGE_ASSISTANT`, `USAGE_ASSISTANCE_NAVIGATION_GUIDANCE`
— onto real zones and volume groups.

**One trap, learned the hard way** (`os-corrections-2026-08-16.md`): in `car_audio_configuration.xml`
v3, `<device address>` matches the devicePort's **`address` attribute, never its `tagName`**. Getting
that wrong produced a `SIGABRT` boot loop where `sys.boot_completed` was never set.

### 2.2 USB audio cannot carry a CarPlay session — *verified*

The bench mic/speaker (Apple EarPods) enumerated at **full-speed** USB, and the HAL drove it at:

```
format: S24_3LE   channels: 2   rate: 48000
period_size: 240  buffer_size: 480        <- a 10 ms ring, two periods
```

Result: `AHAL_StreamAlsa: transfer: incomplete data ... inserting/dropping 240 frames` roughly **ten
times a second, in both directions**. Playback was inaudible; capture was ~75% fabricated silence,
which a level meter reads as healthy. HDMI0 runs the same audio at `period_size 1024 /
buffer_size 8192` with **zero** drops.

**Needed**, in the `android.hardware.audio.service.rpi` input/output profiles:

* prefer the device's **Altset 1 (S16_LE)** over Altset 2 (S24_3LE) — the EarPods advertise both, and
  S16 is a third less bandwidth on a link that is already failing;
* raise `period_count` well above 2 so there is real slack.

**Also missing** (*verified*): `/vendor/etc/audio_policy_configuration.xml` declares only
`AUDIO_DEVICE_IN_BUILTIN_MIC` — there is no USB input `devicePort` — and `/vendor/lib64/hw/` has only
`audio.primary.default.so`, no `audio.usb.*.so`. Capture worked anyway, so the routing found a path,
but a declared USB input port is the correct configuration.

**Note for testers:** the Pi 4 has **no built-in microphone**. Card 3 was the only capture-capable
PCM, so removing the USB device removes the microphone entirely.

### 2.3 The HEVC decoder does a redundant full-frame CPU copy — *researched*

Hardware decode itself works — `c2.ffmpeg.hevc.decoder` drives the rpivid block on `/dev/video19`
through ffmpeg's V4L2 stateless hwaccel, confirmed by 2.08 interrupts per frame from
`feb00000.codec`. The cost is in the **output path**: two full-frame CPU passes where one would do.

| Pass | What | Necessary |
|---|---|---|
| 1 | SAND (`NV12_COL128`) → planar detile | Yes, today |
| 2 | `sws_scale` yuv420p → **yuv420p** | **No — a pure memcpy** |

Pass 2 has identical source and destination dimensions *and* pixel format, so swscale takes its
unscaled copy path. Measured cost: **~33% of one Cortex-A72 core** for a near-static 1080p stream,
split 18.2% user / 18.7% system — a genuine zero-copy passthrough would be almost entirely system.

**Needed**, in
[`android_external_ffmpeg_codec2`](https://github.com/raspberry-vanilla/android_external_ffmpeg_codec2)
`C2FFMPEGVideoDecodeComponent::outputFrame()`, roughly 40 lines: fetch and map the graphic block
**first**, point the hwaccel transfer's `data[]`/`linesize[]` at the `C2GraphicView` planes
(`av_hwframe_transfer_data` only allocates when `dst->buf[0]` is NULL), and delete the swscale path.
Expect roughly half the CPU back. Switching the output surface from `HAL_PIXEL_FORMAT_YV12` to NV12
stacks on top — upstream already does that for HEVC Main 10.

**No property controls this** (*verified*): the HAL reads only
`persist.vendor.ffmpeg_codec2.rank[.audio|.video]`, `.v4l2.h264`, `.v4l2.h265` and
`debug.ffmpeg.loglevel`.

### 2.4 `/dev/media0` has the wrong SELinux label — *verified*

```
/dev/video19  u:object_r:video_device:s0     correct
/dev/media0   u:object_r:device:s0           generic fallback — WRONG
getenforce                                   Permissive
```

Inert today because the board is permissive. **Under enforcing, hardware HEVC decode stops
entirely** — rpivid is a stateless Request-API device and needs `/dev/media0` for control
submission. Needed: a `media_device` type in the device's `file_contexts` plus allow rules for the
codec2 HAL domain. Fix it *before* anyone tries enforcing, because the failure will present as a
codec regression rather than a policy one.

---

## Tier 3 — make the port stop being a bench setup

None of this is a defect; it is the difference between a demo and a head unit.

* **Ship the projection app as a `priv-app`** with `privapp-permissions-com.carlink.projection.xml`
  (in `pi/tools/`) baked into `/system/etc/permissions`. Without it `addKeyEventHandler` is denied
  and **steering-wheel voice and call keys do not reach CarPlay** — the one user-visible feature
  still gated on this. `pi/tools/install_projection_app.sh --system` does it by hand today.
* **Init `.rc` services** for `carplay-wireless`, `hostapd`, `apdhcpd` and the app, with the binaries
  somewhere other than `/data/local/tmp`. Today a reboot means a full manual restart, and nothing
  respawns `carplay-wireless` if it dies (the CCPA has `session_supervisor.sh`; the Pi has nothing).
* **SELinux policy** for those domains, needed before enforcing regardless of §2.4.
* **The `from all lookup main` ip rule.** Android deletes it, so the connected route for the AP
  subnet sits in `main` the whole time and nothing consults it — any process outside the framework
  gets `ENETUNREACH` to its own AP subnet. We add scoped rules by hand in `start_stack.sh`.

---

## Tier 4 — decisions, not fixes

* **The instrument cluster on HDMI port 1.** `CarOccupantZoneService` maps `port=1` to
  `displayType=2`, `cluster_service` is enabled by build config, and a renderer is bound. If you
  never want an AAOS cluster there, disabling it frees the port. If you do, that is precisely why we
  do **not** advertise CarPlay `altVideoStreams` — the two would contend.
* **`dtoverlay=miniuart-bt` must stay removed.** It breaks Bluetooth *and* the serial console.
  Recorded in the earlier round; repeated because it is the kind of change that gets re-added.

---

## Explicitly NOT worth doing

* **Adding a hardware HEVC entry to `media_codecs_v4l2_c2_video.xml`.** Upstream removed it
  deliberately ([commit `846ea6f78`](https://github.com/raspberry-vanilla/android_external_v4l2_codec2/commit/846ea6f78)):
  `V4L2Decoder.cpp` maps `VideoCodec::HEVC` to `V4L2_PIX_FMT_HEVC` — the **stateful** fourcc — and the
  ChromeOS-derived codebase has no Media Request API code at all. It cannot drive rpivid, which is
  stateless and takes `V4L2_PIX_FMT_HEVC_SLICE`. An XML entry would fail at component creation.
* **Chasing `CCodecBufferChannel: Query output surface allocator returned 0 params => BAD_INDEX`.**
  Benign — it is the designed trigger for the fallback **to** `BUFFERQUEUE` (allocatorID 18), a
  surface-backed gralloc pool. Every component that does not override the allocator logs it.
* **`ro.boot.audio.tinyalsa.simulate_input=true`.** It looks like it should suppress capture and it
  does not — measured speech at −4 dBFS peak through it. See `pi/docs/02` §1 for the retraction and,
  more usefully, for how the wrong conclusion was reached.
* **True zero-copy SAND scanout.** The silicon and the DRM/Mesa layers both understand SAND (vc4 KMS
  handles the modifiers, Mesa v3d can import them), but gralloc cannot describe or allocate one and
  Codec2 has no path to wrap an externally-allocated V4L2 DMABuf. It needs upstream work in minigbm,
  codec2 and drm_hwcomposer. §2.3's in-component fix gets most of the benefit for ~40 lines.

---

## Uncertain, and worth someone checking

**Could Android's Bluetooth stack stay enabled?** We disable it (`settings put global bluetooth_on 0`)
so `carplay-wireless` can own `hci0` through raw HCI, because **mgmt synthesises the EIR itself and
cannot express the CarPlay marker UUID**. That is a stack limitation rather than a config one as far
as I know — but I have not checked whether Fluoride's configuration can inject a custom EIR, and if
it can, the raw-HCI layer (`crates/vendor/wireless/src/hci.rs`) could go too. Note this is
independent of §1.1: even with `CONFIG_BT_RFCOMM=y` we would still need the EIR control.

---

## Verifying each change after a rebuild

```sh
# 1.1  kernel RFCOMM
adb shell 'zcat /proc/config.gz | grep CONFIG_BT_RFCOMM'          # want: =y

# 1.2  framework SoftAP — should start without standalone hostapd
adb logcat -s WifiApIface | grep -i countrycode                    # want: no NOT_SUPPORTED

# 2.1  car audio out of legacy mode
adb shell 'dumpsys car_service --services CarAudioService | grep -E "legacy|audio control"'

# 2.2  USB audio profile
adb shell 'cat /proc/asound/card*/pcm0p/sub0/hw_params'            # want: S16_LE, buffer >> 480
adb shell 'logcat -d -s AHAL_StreamAlsa | grep -c incomplete'      # want: 0

# 2.3  decoder CPU
adb shell 'top -H -b -n 1 -p $(pidof android.hardware.media.c2-service-ffmpeg)'   # was ~33%

# 2.4  SELinux label
adb shell 'ls -Z /dev/media0'                                      # want: a media_device type

# Tier 3  privileged permissions
adb shell 'dumpsys package com.carlink.projection | grep -A3 "requested permissions"'
adb logcat -s NETPROBE | grep addKeyEventHandler                   # want: no SecurityException
```

`pi/evidence/device_state_2026-08-16.txt` is the pre-change baseline for all of these.
