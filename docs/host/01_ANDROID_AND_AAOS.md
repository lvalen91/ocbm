# Android / AAOS host

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 59; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

The Android projection app and the AAOS integration points.

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

### The workaround — "package-name squat" (CONFIRMED by decompile, NOT device-tested)

`deviceAttachedForFixedHandler` (`UsbProfileGroupSettingsManager.java:658`):

```java
Intent intent = createDeviceAttachedIntent(device);
mContext.sendBroadcastAsUser(intent, UserHandle.of(ActivityManager.getCurrentUser()));
try {
    ApplicationInfo appInfo = mPackageManager.getApplicationInfoAsUser(component.getPackageName(), 0, mParentUser /* =10 */);
    mUsbService.getPermissionsForUser(UserHandle.getUserId(appInfo.uid)).grantDevicePermission(device, appInfo.uid);  // ← SILENT, per attach
    Intent activityIntent = new Intent(intent).setComponent(component);
    try { mContext.startActivityAsUser(activityIntent, mParentUser); }
    catch (ActivityNotFoundException e) { /* logged; grant already happened */ }
} catch (PackageManager.NameNotFoundException e2) { /* the current dead-end */ }
```

If a third-party app is installed **under the package name `android.car.usb.handler`, in the foreground
user (user 10)**, the framework resolves *that app* and grants it device permission **silently on every
USB attach** — no dialog, no root, no persistence needed (re-granted each attach, which is behaviourally
"always allow").

**What the name buys, and what it does NOT.** It grants exactly one power: **USB device-permission
arbitration for host-mode devices.** It is not a privilege escalation. A `/data` install is
`untrusted_app` regardless of name — no platform signature, no system UID, no `MANAGE_USB`, no access to
`/data/system/users/*` (that write is a SELinux `neverallow`, `plat_sepolicy.cil:6707`, so the
"pre-seed `usb_permissions.xml`" idea is **refuted** for any third-party app). The tradeoff: the
squatting app becomes the fixed handler for **every** USB device on the vehicle — filter hard on
`0x1314:0x2d00`, ignore everything else, and provide a no-display `UsbHostManagementActivity` that
`finish()`es immediately.

**REFUTED — `adb pm grant MANAGE_USB` cannot upgrade the squat into a third-party arbiter.** The
tempting shortcut ("grant the handler `MANAGE_USB` over adb, then it can `grantPermission(device,
zeno.carlink.ocbm)` for a SEPARATE app, exactly like the real car-usb-handler") does not work on this
image. `MANAGE_USB` is `signature|privileged` with **no `development` flag** (Y181,
`gminfo_resources/analysis/platform_faq.md:157`), and `pm grant` only mutates runtime/`dangerous` perms
or signature perms carrying `development` (that is why `WRITE_SECURE_SETTINGS` is adb-grantable and this
is not). `pm grant android.car.usb.handler android.permission.MANAGE_USB` returns **"not a changeable
permission type"** — the SAME result GM's own research got testing signature-level
`gm.permission.ACCESS_ONSTAR` / `ACCESS_IPC_HUD` (`gminfo_resources/research/security/
SHELL_ACCESS_ESCALATION_Jun2026.md:38`). Even if it were changeable, `privileged` means it is only
grantable on the `/system/priv-app` path, which needs `/system` write (root / verified-boot off) the
locked GM does not give. There is no `appops` for USB management either. So on a `/data` sideload the
squat's grant lands on **its own UID only** — the "system app blesses our separate app" mechanism
requires a PRIV-APP install (fine on the Pi, unavailable on the locked truck). This is what forces
integration option (A) below: the claiming app must itself carry the `android.car.usb.handler` name.

**Verified installability (this build):** `ParsingPackageUtils.validateName` has no `android.*` prefix
reservation; PMS reserves only the literal `"android"` and `android.uid.*`; the `privapp-permissions` /
`install-in-user-type` config entries for the name do not reject a `/data` install. **Must be user 10,
not user 0** — a user-0 install resolves in the wrong `UsbUserPermissionManager` and does nothing; the
`UserHandle{0}`/`UserHandle{10}` log pair is the "app absent in that user" signature.

**Install route caveats (unverified on this build):** `adb install` accepts the name, but getting adb on
the locked unit is its own problem (ADB is OTG-only on the center-console Type-C, CAN-gated). A 3P
installer / AltStore-style route calls the same `PackageInstaller` API — the name is not the blocker,
the unknown-sources / `REQUEST_INSTALL_PACKAGES` policy is, and that is untested. Not Play-shippable
(reserved `android.*` namespace). A future GM OTA shipping a real `CarUsbHandler.apk` would collide with
the `/data` package.

### PENDING TASK — build + on-vehicle test of a handler-squat proof app

**Goal:** confirm on the actual head unit that the silent grant fires, before committing a real app to
the name.

> **STEP 1 DONE (2026-08-17): the proof app is built.** Module `host/CarlinkAndroid/usbhandler`
> (`:usbhandler` in the Gradle build), package `android.car.usb.handler`, `assembleDebug` green. It is a
> SEPARATE module so the daily-driver `:app` (`zeno.carlink.ocbm`) is untouched. The merged manifest was
> verified to emit package `android.car.usb.handler` + activity
> `android.car.usb.handler.UsbHostManagementActivity` — byte-identical to GM's
> `config_UsbDeviceConnectionHandling_component` string. Structure mirrors AOSP
> `packages/services/Car/car-usb-handler` (same package + `.UsbHostManagementActivity`, the
> `USB_DEVICE_ATTACHED` intent-filter + `usb_device_filter` meta-data); two deliberate divergences:
> **NoDisplay** instead of the Dialog picker, and **no `MANAGE_USB`** (signature|privileged; a `/data`
> install can never hold it — SELinux — and does not need it: the framework grants *to* us). The device
> filter is a catch-all `<usb-device/>` on purpose — the fixed handler sees every device, so this is also
> how an unknown adapter (a C2Air) gets its VID/PID logged the first time it is plugged in;
> `UsbGrantProbe` does the CCPA-vs-other classification in code and only OPENS the CCPA-OCBM
> (`0x1314:0x2d00`), releasing and closing immediately so it never blocks the real host app's claim.
> **STEPS 2–4 DONE (2026-08-17, on-vehicle) — PASS.** Log `~/Downloads/log.txt` (2024 Silverado
> `gminfo37` Y181), one boot, A/B in ~2 min:
> - **Squat present, user 10:** two CCPA-OCBM attaches (`/dev/bus/usb/001/009`, then `…/010` on replug),
>   framework launched `android.car.usb.handler/.UsbHostManagementActivity` **from uid 1000** (the
>   fixed-handler path), and both times `hasPermission=TRUE … no dialog`, `open OK: claimIf0=true`. The
>   `Default USB handling package (android.car.usb.handler) not found` line stops once we are present.
> - **Control (squat uninstalled, OCBM app alone):** on the next attach the OCBM app
>   (`zeno.carlink.ocbm`) triggered `com.android.systemui/.usb.UsbPermissionActivity` — the ordinary
>   prompt reappears. Same unit, same adapter, ~1 min later. This is the causal proof: the squat, and
>   only the squat, removes the prompt.
> - **Endpoints, measured live:** interface 0 class `0xFF`, **bulk IN `0x81` / OUT `0x01`**, mps 512 —
>   read identically by the proof app AND the OCBM app's own transport. This differs from docs/carplay/00_ARCHITECTURE.md's
>   `0x83`/`0x02`; both host apps enumerate endpoints dynamically so it is cosmetic, but docs/carplay/00_ARCHITECTURE.md is
>   stale for this unit (corrected there).
> Build/install/watch harness: `usbhandler/test_squat.sh` (installs to **user 10**, tails the probe
> lines + the framework's dead-end line).

1. **Minimal proof APK** — nothing but:
   - `applicationId = "android.car.usb.handler"`, installed into **user 10**.
   - `<activity android:name="android.car.usb.handler.UsbHostManagementActivity">` with a no-display
     theme (`@android:style/Theme.NoDisplay`) that `finish()`es in `onCreate`.
   - a `USB_DEVICE_ATTACHED` receiver (+ `res/xml/device_filter.xml` on `0x1314:0x2d00`) that logs the
     device and calls `UsbManager.hasPermission()` / `openDevice()` **without** calling
     `requestPermission()` — the whole point is that permission is already held.
2. **Instrument the grant.** On attach, log `hasPermission(device)` immediately; expect `true` with **no
   dialog**. Cross-check `adb shell dumpsys usb` (if adb is reachable) — `default_usb_host_connection_handler`
   and the in-memory permission map. Watch for the `NameNotFoundException` line to **disappear** once the
   app is present in user 10.
3. **Install-route reality check.** Determine what actually installs on this unit: `adb install` (if adb
   is obtainable), a 3P installer, or AltStore-equivalent — and whether user-10 targeting is possible via
   `pm install --user 10`. This is the real risk, not the framework behaviour.
4. **Coexistence + collision checks.** Confirm the squat app does not break native CarPlay/AA/storage
   (it shouldn't — those route around the handler), and note the OTA-collision hazard in the app's own
   docs.
5. **Decide integration — and mind the per-UID grant.** The grant is
   `grantDevicePermission(device, appInfo.uid)` where `appInfo` is **the `android.car.usb.handler`
   package** (§ "The workaround", `:88`). USB device-permission is **per-UID**, so the grant helps
   *only the app that IS `android.car.usb.handler`* — a separate claiming app (different package →
   different UID) still gets the dialog. There is no `/data`-legal way to re-grant to another package
   (`UsbManager.grantPermission` is signature-gated; `sharedUserId` is deprecated and cannot pair a
   fresh `/data` app with an arbitrary sibling). So "our apps claim without a prompt" resolves ONE of
   two ways, and this is the real integration fork:
     - **(A) Unified — recommended.** The app that opens the CCPA-OCBM over USB IS
       `android.car.usb.handler`. For the softAP/wireless CarPlay path that is the OCBM host currently
       named `zeno.carlink.ocbm` (`:app`). Adopting the handler identity there is the clean single-UID
       answer; cost is losing the side-by-side-with-`zeno.carlink` A/B property (CLAUDE.md build note)
       and an OTA-collision hazard if GM ever ships a real `CarUsbHandler.apk`. Because it is one app
       serving both adapters, "our apps" (plural) collapses to one — the CCPA-OCBM today, the C2Air-OCBM
       (another session's in-flight conversion) once its VID/PID is known.
     - **(B) Handler + FD hand-off.** Keep a thin `android.car.usb.handler` that holds the grant, opens
       the device, and passes the raw usbfs fd (`UsbDeviceConnection.getFileDescriptor()`, dup'd into a
       `ParcelFileDescriptor`) to the real app over a bound service. Heavier: the receiver must drive the
       pipe via libusb/usbfs on the fd because there is **no public API to rebuild a `UsbDeviceConnection`
       from a bare fd**. Only worth it if the claiming app genuinely cannot adopt the handler name.
   The adapter side needs no change either way — entirely head-unit-app work. Cross-ref
   `../carplay/00_ARCHITECTURE.md` (the `0x2d00` PID is what the `device_filter.xml` must match).

**Adapter-side note:** none of this requires a firmware change. The `0x1314:0x2d00` identity and its
stable placeholder `iSerial` are already correct; iSerial is **not** a lever here (the build's
`DeviceFilter.hashCode()` omits the serial). See `../carplay/00_ARCHITECTURE.md`.
