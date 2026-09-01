# Bringing the Pi back up from cold

Written 2026-08-16, immediately before the Pi was powered down. Everything here was true of the
running box; `pi/evidence/` holds the captured state to compare against.

**Nothing in this stack is persistent yet.** Binaries live in `/data/local/tmp`, the stack is
started by hand, and `/tmp` is tmpfs — so the box log and the generated `VehicleConfig` are gone
after a power cycle. Making it survive a reboot is the top item in §5.

---

## 1. What must be true before anything works

| | Where | Why |
|---|---|---|
| The **CCPA is attached over USB-NCM** and reachable at `192.168.50.2:7789` | `mfid` on the CCPA | The Pi has no MFi coprocessor. Every cert/sign goes here. No MFi, no session. |
| `hostapd` running on 5 GHz | `/data/local/tmp/hostapd_5g.conf` | The framework SoftAP path is blocked by the Wi-Fi HAL (`pi/docs/02`). |
| `apdhcpd` on `wlan0` | `pi/apdhcpd` | Android's bundled dnsmasq is a 2009 vestige that silently disables itself. |
| Android's Bluetooth stack **off** | `settings put global bluetooth_on 0` | `carplay-wireless` owns `hci0` directly; two owners fail confusingly. |
| The **projection app installed** | `pi/tools/install_projection_app.sh` | It generates the config and is the only consumer of the A/V seams. |

Order matters in one place only: **the app must have written its config before `airplayd` starts**,
because arming is first-arm-wins per process. `start_stack.sh` checks for the file and warns.

## 2. Cold start, in order

```sh
# 1. Binaries. Also runs the ELD encoder tests and FAILS the build on them —
#    that suite is the only thing that catches an encoder misconfiguration.
pi/tools/build_pi_binaries.sh --serial <serial>

# 2. The app. Writes /data/user/10/com.carlink.projection/files/carplay_cfg.yaml on service start.
pi/tools/install_projection_app.sh --debug --serial <serial>
adb -s <serial> shell am start -n com.carlink.projection/.LaunchTileActivity

# 3. The accessory stack, with the launch environment.
pi/tools/start_stack.sh --serial <serial> --restart
```

`start_stack.sh` carries the environment; the copy captured from the running process is in
`pi/evidence/stack_launch_env.txt`. The one that is easy to miss is **`CARPLAY_CFG_FILE`** — without
it `airplayd` reads its compiled default, negotiates H.264, and the HEVC-only decoder renders
nothing while every counter reads healthy.

## 3. Confirming it actually works — per plane, not overall

The lesson of §5a in `00_PI_AAOS_PORT.md` is that a loud plane credits a silent one. Check each.

```sh
# Video — must say hvcC, not avcC. avcC means CARPLAY_CFG_FILE did not take effect.
adb logcat -s NETPROBE | grep "video config"
adb logcat -s NETPROBE | grep "frames rendered"

# Audio
adb logcat -s NETPROBE | grep "FIRST AUDIO FRAME PLAYED"

# Siri uplink — the ASC MUST be the 4-byte f8f03000. f8f0312c00bc00 is LD-SBR, which iOS discards.
adb shell 'grep "uplink] mic" /tmp/airplayd_wl.log'
adb logcat -s NETPROBE | grep -E "uplink ARMED|CHOPPED|INAUDIBLE"

# Metadata — the AAOS now-playing card
adb logcat -s NETPROBE | grep "\[media\]"

# Device management
adb shell 'echo "{\"cmd\":\"list\"}" | nc 127.0.0.1 9115'

# Seams bound (must be BEFORE a session starts, or frames are dropped silently)
adb shell 'ss -ltn | grep -E "9001|9002|9003|9004"'
```

## 4. Traps that have already cost time — do not rediscover these

* **`pgrep -x carplay-wireless` matches nothing.** `TASK_COMM_LEN` truncates `comm` to 15 chars, so
  it reads `carplay-wireles`. Match `-f` on the command line. This let two instances run at once.
* **`adb shell "... &" &` does not detach** — adb holds the remote stdout, so the script hangs while
  the stack runs perfectly. Use `nohup` plus closing all three fds.
* **`~/Documents` is iCloud-synced** and generates `* 2.*` conflict copies that break the Gradle
  build with duplicate-class and invalid-resource-name errors. 416 of them appeared in one session.
  Delete with `find . -name "* [0-9].*" -not -path "./.git/*"` — verify none are tracked first.
* **USB audio on this board cannot carry 24-bit stereo 48 kHz.** A full-speed device drops ~10
  events/second in both directions. Prefer HDMI, or fix the altset selection (`pi/docs/02`).
* **The Pi 4 has no built-in microphone.** Removing the only USB audio device removes the only
  capture PCM.
* **`android.app.Service` has a hidden final `setForeground(boolean)`** — overriding it compiles and
  throws `LinkageError` on class load.

## 5. What is NOT done, in priority order

1. **Persistence.** Init `.rc` services for `carplay-wireless`, `hostapd`, `apdhcpd`, and the app,
   plus binaries somewhere other than `/data/local/tmp`. Today a reboot means a full manual restart.
2. **`--system` install.** `pi/tools/install_projection_app.sh --system` installs to `priv-app` with
   the permission allowlist. Without it `addKeyEventHandler` is denied and **steering-wheel voice
   and call keys do not reach CarPlay** — the one user-visible feature still gated on this.
3. **Focus transfer** on the AAOS side. The config declares `enablesFocusTransfer: true` and the
   protocol's borrow-vs-take model is in `iap2-core`, but nothing honours it, so a transient nav
   prompt or call alert does not return the screen on its own.
4. **Telephony metadata, phonebook, call history.** Available on wireless via the **AirPlay-tunnel**
   Identify — not the BT-time one, which is byte-pinned. See `pi/docs/01` §5.4 for the declaration
   rules; getting them wrong costs the whole session, not the feature.
5. **The volume-group question.** AAOS car audio runs in **legacy mode** on this image
   (`Run in legacy mode? true`), so there are no volume groups at all. That is the likely cause of
   the "volume jerks and resets" behaviour and it is an image fix — see `pi/docs/02`.
6. **Wired CarPlay** — out of scope by decision (OTG/UDC role toggling on the port carrying adb).
7. **Android Auto** — licensing-gated, not engineering-gated. There is no public head-unit SDK.

## 6. Image-level changes

All in `pi/docs/02_AAOS_IMAGE_CORRECTIONS_2026-08-16.md`, with the earlier round on the (currently
unmounted) `/Volumes/stuff/rpi/aaos`. The two with the largest payoff both **delete code this port
had to write**: `CONFIG_BT_RFCOMM=y` removes `rfcomm_uspace.rs` entirely, and fixing the Wi-Fi
vendor HAL's `setCountryCode` would let the framework SoftAP work and remove both standalone
`hostapd` and `pi/apdhcpd`.
