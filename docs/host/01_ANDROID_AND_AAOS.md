# Android / AAOS host

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 59; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

The Android projection app and the AAOS integration points.

**Scope.** Three Android-side consumers of the CCPA are described here or nearby:

- `host/CarlinkAndroid` `:app` (`zeno.carlink.ocbm`), the third-party OCBM host for adapter-bridged CarPlay.
- `host/gm_ccpa` (`zeno.gmccpa`), the app for a GM AAOS radio. There the CCPA serves as both the Bluetooth
  radio and the MFi provider; in the `wifi_ap:false` "bridge role" the box raises no SoftAP and the
  vehicle's own hotspot carries the session. Its own docs are `host/gm_ccpa/docs/` (start at
  `00_HANDOFF.md`). Most of the USB-permission history below was written for this app.
- `host/CarlinkAndroid` `:projection` (`com.carlink.projection`), the Raspberry Pi 4 AAOS system app
  (`pi/docs/01_PROJECTION_APP_DESIGN.md`). A Pi 4 AAOS image and a Linux workstation have reached full
  CarPlay A/V (light testing) with a CCPA in NCM mode used only as the MFi source; the host there uses its
  own Bluetooth and Wi-Fi radios, so none of the OCBM USB handling below applies to it.

**USB identities.** `1314:2d00` is the OCBM (open userspace) mode. `1314:1520` and `1314:1521` are the
stock Carlinkit firmware only, not an NCM identity. A distinct NCM PID, `1314:2d10`, is decided; the code
and tables have not been updated yet. The C2Air (a different box: Allwinner V821, AIC8800D80 radio, MFi 3.x)
uses `1f3a:ace2`, which is in the macOS table only; the Android `KnownDevices` list
(`protocol/MessageTypes.kt`) and `usb_device_filter.xml` do not contain it. OCBM on the C2Air's new baseline
is untested.

## The Android app is a THIRD-PARTY app, by decision (verified 2026-09-18)

`host/CarlinkAndroid` is one Gradle build with four modules (`:app`, `:projection`, `:car-system-stubs`,
`:usbhandler`), and the 2026-09-18 session settled which one is the AAOS integration route:

- **`:app`** — applicationId `zeno.carlink.ocbm`. Owns the USB/OCBM transport to the CCPA adapter
  (`ocbm/UsbBulkTransport.kt`, `ocbm/OcbmClient.kt`). **This is the product for adapter-bridged
  CarPlay on AAOS**, and integrates as an ORDINARY third-party app — it declares zero car
  permissions.
- **`:projection`** (`com.carlink.projection`) — consumes a loopback TCP seam
  (`SeamListener`, a `ServerSocket` on `SeamContract.LOOPBACK`), not the adapter, and its AAOS
  layer is built on `android.car.CarProjectionManager`. **That route is now rejected** — see below —
  so `:projection`'s AAOS layer does not have a path to ship as a third-party install on a locked unit.
  On the Pi image it is built in as a system app, where the privileged APIs are available.
- `minSdk = 32` for `:app` (`:projection` is 34). The real target is a GM gminfo head unit on Android 12L; the AAOS emulator (API 35)
  used for the verification below is a bench, not the target.

**Verified on a running AAOS 15 image with `pm list permissions -f`:** `android.car.permission.
CAR_PROJECTION`, `ACCESS_CAR_PROJECTION_STATUS`, `CAR_NAVIGATION_MANAGER`,
`CAR_UX_RESTRICTIONS_CONFIGURATION`, and `CAR_DRIVING_STATE` are all `signature|privileged` —
unobtainable by a third-party app on a locked unit (the same `/system` write / signature barrier
recorded below for `MANAGE_USB`). `CAR_INFO` is the one exception, `normal`. Separately,
`CarUxRestrictionsManager` listening and `CarAppFocusManager` focus claims need NO permission at
all. The public SDK jar (`~/Library/Android/sdk/platforms/android-35/optional/android.car.jar`)
exposes 55 classes and contains neither `CarProjectionManager` nor `ClusterHomeManager` — both are
`@SystemApi`, which is why `car-system-stubs/` exists as a compile-only shim for `:projection` and
was never a route to a runtime grant.

**Consequence: `android.car.CarProjectionManager` / `ProjectionStatus` is REJECTED as the
integration route for `:app`.** The 3P substitutes actually shipped, all hardware- or
box-verified (see `host/CarlinkAndroid/OCBMANDROID.md` "3P native shell" for the wire-level detail):
an active `MediaSession` (drives the AAOS media card, cluster now-playing, and steering-wheel media
keys) in place of `updateProjectionStatus`; `Notification.CallStyle` for calls; `CarAppFocusManager`
for nav focus; `CarUxRestrictionsManager` listening for drive state (already this doc's own
"Drive state" pattern in `OCBMANDROID.md`, of reading a UX-restrictions signal instead of a gated
one). **Cluster turn-by-turn video and `CH_ALT_VIDEO` on a cluster display are out of reach for a
third-party app** — do not re-plan either against `ClusterHomeManager`.

This finding is orthogonal to, and does not upgrade, the USB-handler squat below: squatting as
`android.car.usb.handler` buys silent USB device-permission arbitration only (confirmed narrow in
the "workaround" section), never a signature|privileged `android.car` grant. The two mechanisms
guard different resources and neither substitutes for the other.

## GM AAOS USB permission handler

<!-- absorbed: ../host/01_ANDROID_AND_AAOS.md -->

### The symptom

On a GM AAOS head unit (2024 Silverado ICE, `gminfo37`, build `W231E-Y181.3.2`, Android 12L / SDK 32)
the USB permission dialog for the OCBM adapter (`0x1314:0x2d00`) **reappears on every connect and after
every power cycle**; the "always allow / remember" checkbox never sticks, and the device is treated as
new each time. This is a **head-unit-side** problem, not an adapter-firmware one — but it gates the
whole seamless-connect experience for any third-party OCBM host app, so it lives here.

Evidence base: two independent static analyses of the Y181 `86331654` partition images — all six USB
framework classes decompiled and confirmed **byte-equivalent to stock AOSP 12** (GM changed no USB
Java). Full write-up and file/line citations are in the resources repo,
`gminfo_resources/analysis/platform_faq.md` §2 (+ the 2026-08-17 corrections). This doc is the
CCPA-project-facing summary and the action item.

### Root cause — a dangling resource, not a missing app

`framework-res.apk` `res/values/strings.xml:497`:

```xml
<string name="config_UsbDeviceConnectionHandling_component">android.car.usb.handler/android.car.usb.handler.UsbHostManagementActivity</string>
```

`UsbHostManager.usbDeviceAdded()` branches on that:

```java
if (getUsbDeviceConnectionHandler() == null) getCurrentUserSettings().deviceAttached(newDevice);          // stock path — consults the "always" map, issues implicit grants
else                                         getCurrentUserSettings().deviceAttachedForFixedHandler(...);  // ALWAYS taken here
```

The component is set but the package `android.car.usb.handler` is **stripped from the image** (verified
four ways incl. a raw dex-descriptor grep across all three partitions). So `deviceAttached()` — the path
that reads the default-app map the "always" checkbox writes and issues the implicit grant — is **never
reached for any USB host device.** `deviceAttachedForFixedHandler` instead throws `NameNotFoundException`
and returns. Separately, the genuinely-persistent store `usb_permissions.xml` (written only by
`setDevicePersistentPermission`) has **zero callers image-wide**, so it is never populated either. The
in-memory grant map is keyed on `/dev/bus/usb/BBB/DDD`, which changes on every enumeration — hence
"prompt every time," including on a hot replug within one boot.

**Precise scope — the dead-end is NARROW (this corrects an earlier overstatement).** It kills only the
*implicit-grant + resolver-launch for an untrusted third-party app, for a host-mode device*. It does NOT
break native projection or storage, because:

- The `UsbDevice` is still created (`mDevices.put`, line 292) and the `USB_DEVICE_ATTACHED` broadcast
  still fires (line 660) before the throw — `getDeviceList()` still returns the device.
- **System / privileged apps skip the resolver:** `hasPermission` line 122 short-circuits
  `uid == 1000 → true`, and any `MANAGE_USB` holder opens devices directly. GM's own
  `com.gm.lcm` / `com.gm.hmi.connection` / `com.gm.hmi.settings`, Google's apps, SystemUI and
  CarSettings all hold `MANAGE_USB`.
- **Wired CarPlay** = `/system/app/GMCarPlaySrc/GMCarPlay.apk` (system app, Cinemo receiver).
  **Wired Android Auto** = AOA **accessory** mode → `accessoryAttached()`, a separate ungated path;
  `/system/app/GMGALSrc/GMGAL.apk` is a system app too.
- **USB storage** never uses `UsbManager` permissions — kernel `usb-storage` → vold →
  StorageManager/DocumentsProvider.

So the only thing that dead-ends is **exactly our case**: a sideloaded third-party app claiming a
host-mode vendor device (`0x1314:0x2d00`) with no kernel driver and no privileged system claimant. It is
also why GM never noticed — everything GM ships is privileged and routes around the broken handler.

**Log-confirmed unique (POTATO capture, 2026-08-17).** A sweep of ~4.9 GB of real `adb logcat` from this
unit (43 boots, `/Volumes/POTATO/logcat`) found `Default USB handling package (android.car.usb.handler)
not found` **1,176 times and it is the ONLY variant** of the "handling package (X) not found" grant
signature — no sibling handler is broken this way. 148 packages log as "allowlisted but not present," but
that set is inert for squatting (test apps, RRO theme overlays, stripped stock apps — nothing consumes
them via a grant-by-name path); the two config pointers whose targets are also absent
(`config_carrierAppInstallDialogComponent`→`com.android.simappdialog`,
`config_overrideComponentUiPackage`→`com.android.stk`) show **0 live launch/resolve refs** (dead
SIM/carrier flows that never fire on this vehicle). A separate, murkier surface exists — service-bind
failures for GM/Google components (`com.gm.permissionproxy/.PlatformPermissionService`,
`com.gm.vmsplugin/.VMSClusterService`, …) — but those are mostly present-but-failing, not absent, and are
not the same ungated grant primitive. A rigorous per-`config_*`-pointer exploitability sweep (present vs
absent × consumer signature-gate) is in progress to confirm the USB handler is the sole EXPLOITABLE
instance; update this line with its verdict.

> **STATUS 2026-10-08 (read first).** The squat below is NOT the route in use. gm_ccpa was moved back to its own
> package (2026-09-08) and the owner found the squat never removed the prompt (2026-09-10). The mechanism in use
> is the persistent grant written over adb as the shell uid (section "2026-10-07 re-analysis" and "Bench
> verification of the persistent USB grant", below): bench-verified on a box reboot, a physical replug, a radio
> power cycle, and a cold boot with the box attached. The app then claims with the app closed
> (`AdapterAttachReceiver` -> `AutoClaim`). Uninstall + reinstall changes the uid (observed 1010120 -> 1010124 across
> this session's reinstalls), so the grant must be re-run; its loss after a reinstall was expected from the
> source, not separately bench-recorded. The squat sections that follow are history.

### The package-name squat — HISTORICAL (not the route in use)

The earlier workaround, kept short because the persistent grant below replaced it. Full text is in git
history.

- **Mechanism.** On this image the framework takes the fixed-handler branch
  (`UsbProfileGroupSettingsManager.deviceAttachedForFixedHandler`). If a third-party app is installed
  under the package name `android.car.usb.handler` in the foreground user (10, not 0), the framework
  resolves it and grants device permission to that package's UID silently on every attach. The grant is
  per-UID, so it helps only the app that IS that package; it is not a privilege escalation, a `/data`
  install stays `untrusted_app`, and it makes that app the fixed handler for every USB device on the vehicle.
- **Refuted shortcuts.** `adb pm grant ... MANAGE_USB` fails ("not a changeable permission type":
  `signature|privileged`, no `development` flag), so the squat cannot be upgraded into an arbiter for a
  separate app; pre-seeding `usb_permissions.xml` is blocked by SELinux for a third-party app; the package
  is not Play-shippable and would collide with a real `CarUsbHandler.apk` from a GM OTA.
- **Proof app.** `host/CarlinkAndroid/usbhandler` (`:usbhandler`, package `android.car.usb.handler`, a
  NoDisplay activity, catch-all `<usb-device/>` filter so an unknown adapter such as a C2Air gets its
  VID/PID logged, and `UsbGrantProbe` opening only the CCPA-OCBM). On-vehicle A/B 2026-08-17 on a 2024
  Silverado `gminfo37` (Y181): with the squat present the attach got `hasPermission=TRUE` and no dialog;
  with it uninstalled the ordinary `UsbPermissionActivity` prompt returned. Those were hot replugs.
- **Why it stopped being the answer.** The owner found on 2026-09-10 that the squat did not remove the
  prompt in the field. The 2026-10-07 re-analysis below explains it: a boot-time attach is processed in
  user 0, where a user-10 install is not found, and nothing re-processes it after the user switch. gm_ccpa
  returned to its own package (`zeno.gmccpa`) on 2026-09-08. The "(A) Unified" and "(B) handler plus FD
  hand-off" integration options from that period are not pursued.
- **Measured endpoints** on that unit: interface 0 class `0xFF`, bulk IN `0x81`, bulk OUT `0x01`, mps 512
  (both host apps enumerate endpoints; `../carplay/00_ARCHITECTURE.md` carries the correction).

**Adapter-side note.** None of this needs a firmware change. The adapter's `0x1314:0x2d00` identity and
its `iSerial` must stay stable per box: `DeviceFilter.hashCode()` omits the serial but `equals()` compares
manufacturer, product and serial, and the persistent-grant map is keyed on `DeviceFilter`, so a serial
change makes a persistent grant miss. See `../carplay/00_ARCHITECTURE.md`.

### 2026-10-07 re-analysis — why the squat "worked, then stopped", and the adb-once route

Sources: Y181 `services.jar` jadx at `/Volumes/stuff/virtual-machines/gm-x86_64/gm_emu/hy/re/services.jadx`
(md5 `b1e33fcdb798590eef43e485f8aedc58`, from the `y181gm` emulator tree, `com/android/server/usb/*`);
`DeviceFilter` from the same tree's `framework.jar`; GM SystemUI jadx in the same dir; live truck
data `/tmp/ccpa_sess/allpkg_B.txt` (`dumpsys package`, 2026-10-06) and `/tmp/ccpa_sess/logcat_cp10.txt`
(box reboot, 2026-10-07). Labels: **[SRC]** traced to code; **[LIVE]** truck data; **[UNVERIFIED]**
not yet independently confirmed or bench-tested.

**Live state today (current `zeno.gmccpa` build).** [LIVE] On each box attach the framework takes the
fixed-handler branch and dead-ends: `logcat_cp10.txt:3927436` `Default USB handling package
(android.car.usb.handler) not found  for user UserHandle{10}`, same thread (1652) and millisecond as
`UsbHostManager: Added device /dev/bus/usb/001/007` (`:3927428`). gm_ccpa then calls
`requestPermission` and SystemUI shows the dialog (`:3930726`). `zeno.gmccpa/.UsbAttachActivity` is
**never started** (0 matches in the capture), because the resolver (`deviceAttached()` →
`resolveActivity()`, `UsbProfileGroupSettingsManager.java:789-792`) is unreachable while the
component is set (`UsbHostManager.java:298-302`). `android.car.usb.handler` has 0 matches in the
live package dump. The device number changes on every re-enumeration (`001/007` then `001/008`).
- So docs that say "the ordinary attach resolver and its one-time 'always open' dialog" handle the box
  are wrong on this unit: there is no resolver launch, and the "always" checkbox persists nothing
  useful. SystemUI `UsbPermissionActivity.java:150-153` calls `grantDevicePermission` (in-memory,
  keyed on `getDeviceName()`, `UsbUserPermissionManager.java:247-253`) plus `setDevicePackage`. That
  writes `mDevicePreferenceMap`, which is read only by `resolveActivity` (`:604`), and that is the
  unreachable path. [SRC]

**Why the squat could work on hot replug and fail after a cold boot.** [SRC; truck UNVERIFIED]
1. `UsbService` constructor calls `onSwitchUser(0)` (`UsbService.java:149`). `systemReady()` starts the
   host-bus monitor thread (`UsbHostManager.java:409-414`). So every device that enumerates before
   CarService switches to user 10 is processed against the **user-0** profile-group settings.
2. `deviceAttachedForFixedHandler` resolves the handler package in `mParentUser`
   (`UsbProfileGroupSettingsManager.java:799`). The squat was installed in user 10 only, so a boot-time
   attach throws `NameNotFound` "for user UserHandle{0}" (`:809`) and **no grant is made**.
3. The later switch to user 10 only swaps the settings object (`UsbService.java:171-183`,
   `UsbHostManager.java:397-401`). Nothing re-processes devices that are already attached, so the box
   stays ungranted for that whole boot until it re-enumerates.
4. Hot replugs and box reboots after user 10 is foreground take the user-10 lookup and get the silent
   grant (`:799-800`). That matches the 2026-08-17 A/B, which was done on hot replugs, and the squat-era
   KDoc (`e92c0a7` history, commit `e17c0ec`, the `UsbHostManagementActivity` class KDoc): "works maybe half
   the time in the field … never reproducible with a Mac attached".

Corroboration on another GM image (not the truck): the CT5 real-kernel emulator log
`/Volumes/stuff/misc/research/GM_research/aaos/gm_aaos/realk/run/ccpa1.log:12504` shows `…not found
for user UserHandle{0}` before `switchUser | user: 10` at `:13846`. [UNVERIFIED on the truck]: no
truck cold-boot log is on this machine (the `/Volumes/POTATO` capture cited in
`gminfo_resources/analysis/platform_faq.md:72-75` is not mounted). Also unknown: whether the box
enumerates before or after the user switch on a real ignition cycle, and how GM suspend/resume affects
re-enumeration.

**Refuted squat-era hypotheses.** [SRC]
- "Process-age race: activity samples `hasPermission` before the grant commits." The grant (`:800`)
  precedes `startActivityAsUser` (`:804`) on the same thread. Only the broadcast (`:797`) precedes the
  grant, and gm_ccpa is not a receiver of it (live: delivered to `system` and
  `com.gm.domain.server.delayed` only, `logcat_cp10.txt:3927435-3927437`).
  **Correction 2026-10-08 (plan v5.9 D6):** "not a receiver" was true of the build in that capture
  only. The broadcast goes to the CURRENT user with `FLAG_RECEIVER_INCLUDE_BACKGROUND` before the
  failing handler lookup (AOSP32 `UsbProfileGroupSettingsManager.java:915-919`, GMJ `:795-797`), so a
  manifest receiver gets it despite the implicit-broadcast ban. gm_ccpa now declares one
  (`AdapterAttachReceiver`, `exported="false"`; the sender is system_server) and auto-claims the
  adapter from it with the app closed, relying on the persistent grant below — no dialog, no UI.
  Whether a THIRD-PARTY user-10 receiver is delivered on this build is still UNKNOWN until bench step
  B-D6-1a (U59-18). The hypothesis refuted here (process-age race) stays refuted.
- "Descriptor variant / serial change defeats the cache." The fixed-handler grant is per attach,
  keyed on device path, and unconditional on identity.
- Live also shows a user 11 on this unit (`allpkg_B.txt`, `zeno.gmccpa` `User 11: installed=false`).
  Any attach handled while user 11 is current also misses. Whether user 11 is ever foreground is
  unknown.

**Refuted shell levers.** [SRC/LIVE]
- `pm grant … MANAGE_USB`: the permission is `signature|privileged` (`allpkg_B.txt:7962-7964`).
- `dumpsys usb` only offers port subcommands (`UsbService.java:288-436`). `UsbService` has no shell
  command handler, so `cmd usb` has no permission operations.
- `setUsbDeviceConnectionHandler` needs MANAGE_USB and throws unless the calling user is the current
  user (`UsbService.java:887-897`). Shell runs as user 0 and the current user is 10. The setting is
  also in memory only.

**adb-once fix: shell-written persistent grant.** [SRC + LIVE premise; **bench-tested 2026-10-07, see the verification section below**]
- [LIVE] Shell uid 2000 holds `MANAGE_USB`: `allpkg_B.txt:24540` (`SharedUser [android.uid.shell]`,
  `granted=true`).
- [SRC] `IUsbManager.setDevicePersistentPermission(device, uid, user, true)` is gated only on
  MANAGE_USB (`UsbService.java:853-862`). It writes `/data/system/users/<user>/usb_permissions.xml`
  (`UsbUserPermissionManager.java:68`, `:512-531`), and that file is re-read when the per-user manager
  is constructed (`:69-71`, `:132`).
- [SRC] `hasPermission` checks that persistent map **before** the device-path map (`:290-293`).
  `requestPermission` returns "granted" with no dialog when `hasPermission` is true (`:440-447`). So
  the app needs no change.
- [SRC] The key is `DeviceFilter`: VID, PID, class, subclass, protocol, manufacturer, product and
  serial. It survives re-enumeration and reboot.
- [SRC] It is invalidated by: an uninstall+reinstall (new uid; no pruning code was found), a different
  box or different descriptor strings or serial, and a different Android user.
- The call requires the device to be attached once, from an `app_process` helper running as shell
  (same pattern as `~/carlink/btctl`), using uid = `zeno.gmccpa`'s user-10 uid and user 10.
- Writing the helper is a code task.

**Bench test (read-only first):**
```
adb shell am get-current-user; adb shell pm list users
adb shell cmd package list packages -U --user 10 zeno.gmccpa
adb shell dumpsys package com.android.shell | grep MANAGE_USB
adb shell dumpsys usb | sed -n '/host_manager/,/port_manager/p' | head -120   # handler, devices; persistent-perm dump exists (UsbUserPermissionManager.java:315-321), whether dumpsys reaches it is unverified
# cold-boot mechanism: right after an ignition cycle with the box plugged in
adb logcat -d -b all | grep -E 'Default USB handling|UsbHostManager: Added device|switchUser|onUserSwitching' | head -40
```
Pass criteria for the helper: `dumpsys usb` lists the persistent entry. Then, with no
`UsbPermissionActivity` START in logcat: replug, REBOOT BOX, a full head-unit reboot, and `adb install -r`
(the uid is kept). An uninstall+reinstall is expected to fail.

#### Bench verification of the persistent USB grant (2026-10-07, gminfo3.7 bench, user 10)
Helper `/tmp/ccpa_sess/usbgrant` (`app_process` as shell, `setDevicePersistentPermission(box, uid(zeno.gmccpa,u10), user 10, true)`); `dumpsys usb` then shows `device_persistent_permissions` for vid 0x1314 / pid 0x2d00 "Auto Box" with `uid=… is_granted=true`.
- REBOOT BOX (23:47:35): re-attached +2.72 s, NO UsbPermissionActivity, claimed + HELLO_ACK +4.02 s (before the grant, same test at 23:27: dialog shown, claim only after a tap).
- Force-stop + relaunch (23:48:31/41): claimed with no dialog (not a grant test: the device stayed attached).
- Physical unplug 38 s + replug (23:49:20 -> 23:49:58): NO dialog, claimed + HELLO_ACK 1.57 s after attach, armed +3.46 s.
- `UsbProfileGroupSettingsManager: Default USB handling package (android.car.usb.handler) not found for user UserHandle{10}` still logs on every attach (expected; the grant bypasses the dialog via hasPermission).
- Radio cold boot and box-attached-at-power-on: tested, see the next two bullets. Still not bench-recorded: uninstall+reinstall (a new uid is expected to lose the grant; re-run `grant`). `revoke` stores is_granted=false (does not delete) — re-run `grant` to undo a revoke.
- Radio power-cycle (2026-10-07 ~23:52): the `device_persistent_permissions` entry (Auto Box, uid of zeno.gmccpa, is_granted=true) survived the reboot. Box plugged in after boot (23:53:40, user 10 already current), app opened 23:53:50: claimed /dev/bus/usb/001/006 + HELLO_ACK at 23:53:55.9 with NO UsbPermissionActivity in the whole capture (logcat_cp11.txt). Still untested: box already attached at power-on (the user-0 enumeration case).
- **Cold boot with the box attached (2026-10-07 ~23:57, the user-0 case):** boot log (`/tmp/ccpa_sess/logcat_coldboot_box.txt`, dumped with `logcat -d -b all` right after adb came up) shows the box enumerated BEFORE the switch to user 10 — `Default USB handling package (android.car.usb.handler) not found for user UserHandle{0}` for vid/pid 1314:2d00 (line 3988), user switch to 10 later (line 9320, onSwitchUser 10 at line 9558). The persistent entry (uid of zeno.gmccpa, is_granted=true) was intact. App opened afterwards: claimed /dev/bus/usb/001/006 + HELLO_ACK in 77 ms (23:59:05.929 -> 06.006) with NO UsbPermissionActivity in either capture. So the persistent grant also covers the cold-boot case that defeated the 2026-08/09 handler-name workaround.
