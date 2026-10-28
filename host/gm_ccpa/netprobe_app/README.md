# netprobe_app — the gm_ccpa Android app (applicationId `android.car.usb.handler`, class package `zeno.gmccpa`)

*Rewritten in place 2026-10-08. This file used to describe the 2026-07-31 NetProbe network prober ("does no
CarPlay / iAP2 / MFi / CCPA work", a Run all / Save screen, `netprobe-debug-v3.1.apk`). That app is gone:
`MainActivity.kt`'s header records the feasibility probes as retired 2026-08-12, and the project name
"NetProbe" survives only in the directory name, the `NETPROBE` logcat tag and the golden `netprobe-debug-v4.0.apk`.*

The directory holds the wireless-CarPlay receiver: a Kotlin shell around a JNI'd Rust core. What the app is and
does: [`../docs/04_SYSTEM_MODEL.md`](../docs/04_SYSTEM_MODEL.md) (§4e is the current app surface).

## Screen

One launcher (`MainActivity`): status on the left (state, driver line, pairing code, adapter Bluetooth name, rekey
hint, hotspot chip), actions on the right — **Set hotspot / Start / Restart session / Stop / Adapter & logs**.
Hotspot credentials are entered once in the dialog (no compiled-in defaults). The CarPlay screen
(`av/CarPlayActivity`) appears when a session is live. No Recover, Box Info, Log Scope or Log Status button
exists any more.

## Build

Not Gradle. The Gradle files exist for Android Studio indexing only (`app/build.gradle` header: a Gradle APK is
pairing-incapable because it does not package the native `.so`). Use, from `host/gm_ccpa`:

```bash
bash tools/test.sh                      # Tier-0 gate (cargo, JNI cross-build, JVM test classes listed in tools/jvm_tests.txt, docs budget, proto check)
bash tools/build_apk.sh                 # release flavor -> apk/gmccpa-debug-<STAMP>.apk
GMCCPA_BENCH=1 bash tools/build_apk.sh  # bench flavor (adds mic_inject / finish_launcher) -> ...-bench.apk
```

`STAMP` is `<sha>` or `<sha>-dirty.<h8>`; `versionCode` is the git commit count; `versionName` is `4.0+<STAMP>`
(`-bench` suffix for the bench flavor). Exactly one of `src/nobench/java` and `src/bench/java` is compiled.

## Install

```bash
adb shell am get-current-user     # must print 10
adb install --user 10 -g -r -i com.android.vending apk/gmccpa-debug-<STAMP>.apk
```

`-i com.android.vending` keeps the in-motion display path eligible; `-g` grants the runtime permissions;
wait 30 s between any uninstall and the following reinstall. After an uninstall + reinstall re-run the persistent
USB grant (`../docs/06_BRINGUP_RUNBOOK.md` §4, "USB permission grant"). `SYSTEM_ALERT_WINDOW` is `development`
and granted by `-g`; never set its appop to `ignore`/`deny`.

## Run and observe

```bash
adb shell am start --user 10 -n android.car.usb.handler/zeno.gmccpa.MainActivity
adb logcat -d -b all | grep -a NETPROBE       # the app's narrative; `grep -a` is needed on binary-laden captures
```

Scripted verbs (`--es run <verb>`) and their arguments are listed in
[`../docs/06_BRINGUP_RUNBOOK.md`](../docs/06_BRINGUP_RUNBOOK.md) §4. `--es ssid/--es pass` apply to that run only.

## Layout

`ocbm/` OCBM client and USB transport · `pair/NativeCore.kt` JNI bindings · `av/` video, audio, voice, mic, media
service, status notification · `logging/` capture, export, redaction, session summary · top level: session core,
supervisor, receiver, launcher UI, auto-claim. Tests: `app/src/test/java` (host JVM, listed in
`../tools/jvm_tests.txt`).

## Scope

Interoperability and accessory development on the owner's own hardware. The app holds no privileged permission.
Remove with `adb uninstall android.car.usb.handler` (a plain uninstall also removes the pairing, the accessory key and the
saved hotspot details).
