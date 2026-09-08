# NetProbe — non-privileged AAOS network capability prober

A single-screen Android app that answers one question empirically: **with only an ordinary app UID
(10xxx), what can this app see and reach on the Wi-Fi segment the vehicle hotspot puts it on?**

It does **no** CarPlay / iAP2 / MFi / CCPA work. It only characterizes:

| Probe | Answers |
|-------|---------|
| Identity / install / overlay | uid, installer attribution (`com.android.vending`?), `canDrawOverlays`, own `distractionOptimized` meta |
| Permissions | which network/location/multicast/overlay perms are actually granted |
| Driver distraction | live `CarUxRestrictions` (read-only, via reflection) — is the car in a driving-restricted state right now |
| Interfaces | every NIC, up/multicast/mtu, addresses & prefixes |
| Connectivity | every `Network`, its transports, internet/validated, which is **default**, per-net IPs/DNS |
| Wi-Fi / band | SSID/BSSID, IP, **frequency → 2.4 / 5 / 6 GHz**, link speed, DHCP gateway/netmask |
| mDNS / multicast | sends real mDNS queries on 224.0.0.251:5353 and lists responders — proves multicast **both directions** and enumerates the iPhone's advertised services |
| mDNS advertise | registers its own `_airplay._tcp` — proves an ordinary app can be **discoverable** |
| Reachability | ICMP + TCP connect to an iPhone IP (optionally after **binding to the Wi-Fi network**) — proves no AP client isolation |
| Subnet sweep | fast /24 sweep to locate the iPhone |
| Raw sockets | confirms ephemeral bind works and privileged (<1024) bind is denied |

Each probe is isolated — one failure never aborts the run. Output shows on screen, goes to `logcat -s
NETPROBE`, and **Save** writes a text report for `adb pull`.

> **Status:** feasibility phase complete (captures 2026-07-31); results live in
> [`../docs/01_FINDINGS.md`](../docs/01_FINDINGS.md). This is **not** archival — it is the foundation the
> next build phase grows on (see [`../docs/03_BUILD_PLAN.md`](../docs/03_BUILD_PLAN.md) §11).

## Build

Zero external dependencies, pure platform SDK.

**Use the Gradle-free path** — the Mac's JDK is 21, which fights AGP 7.4:

```bash
bash ../tools/build_apk.sh        # kotlinc -> d8 -> aapt2 -> zipalign -> apksigner
# -> ../apk/netprobe-debug-v3.1.apk
```

The Gradle files are kept for Android Studio editing/indexing. If you do want a Gradle build:

```bash
cd netprobe_app
gradle wrapper --gradle-version 7.6   # if no wrapper is checked in
./gradlew :app:assembleDebug
# -> app/build/outputs/apk/debug/app-debug.apk
```

## Install (Play-attributed, distraction-optimized)

Attribute the install to the Play Store so the distraction-optimized activity is eligible in motion,
and auto-grant the runtime perms:

```bash
adb install -i com.android.vending -g app/build/outputs/apk/debug/app-debug.apk
```

If `-g` didn't grant everything (older platform), grant explicitly via `shell` (uid 2000):

```bash
P=zeno.gmccpa
adb shell pm grant $P android.permission.ACCESS_FINE_LOCATION
adb shell pm grant $P android.permission.ACCESS_COARSE_LOCATION
adb shell pm grant $P android.permission.NEARBY_WIFI_DEVICES   # no-op pre-A13, harmless
adb shell appops set $P SYSTEM_ALERT_WINDOW allow              # overlay = in-motion display lever
# location services must also be ON, or SSID reads back as "<unknown ssid>":
adb shell settings put secure location_mode 3
```

Verify the install attribution stuck (the app also reports this on screen):

```bash
adb shell dumpsys package $P | grep -i installerPackageName
```

## Run

```bash
adb shell am start -n zeno.gmccpa/.MainActivity   # foregrounds even if hidden from the car launcher
adb logcat -c && adb logcat -s NETPROBE                    # live output
```

1. Connect the iPhone to the vehicle hotspot manually.
2. In the app, optionally type the iPhone's IP (iPhone: Settings ▸ Wi-Fi ▸ ⓘ on the network).
3. Optionally tick **Bind process to Wi-Fi network** (do this if the Connectivity probe shows the
   hotspot is *not* the default/validated network — otherwise sockets leak out cellular).
4. Tap **Run all**, then **Save**.

```bash
adb pull /sdcard/Android/data/zeno.gmccpa/files/netprobe_report_<ts>.txt
```

## How to read it (maps to the open questions)

- **2.4 vs 5 GHz** → `WI-FI INFO / BAND` line (`freq=...MHz => BAND=`).
- **Client isolation** → `REACHABILITY`: any completed TCP handshake or OPEN port to the iPhone = not
  isolated. Absence in mDNS alone is *not* proof of isolation (a plain iPhone may advertise nothing
  until an app runs) — always cross-check with a direct reachability hit.
- **Multicast / discovery survivable?** → `mDNS / MULTICAST`: responders listed = the AP passes
  multicast and Bonjour discovery is viable; empty = the most likely silent blocker.
- **Can we be the accessory?** → `mDNS ADVERTISE`: `REGISTERED` = an ordinary app can be discoverable.
- **In-motion display** → `IDENTITY` (`canDrawOverlays`, `distractionOptimized`) + `DRIVER
  DISTRACTION` (whether the car is restricted right now). With a Play-attributed install this is where
  you confirm the activity is allowed to run moving.
- **Wrong-network egress** → `CONNECTIVITY`: if the hotspot net isn't `[DEFAULT]`, bind before testing.

## Scope

Read-only characterization on your own hardware. It changes nothing on the vehicle, opens no CarPlay
session, and holds no privileged permission. Delete with `adb uninstall zeno.gmccpa`.
