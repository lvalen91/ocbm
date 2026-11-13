# 00 — Handoff: current state of gm_ccpa

Resume point: what is committed, what is on the bench radio, what is verified, what is open. Current state
only; history is in git. Architecture is owned by [`04_SYSTEM_MODEL.md`](04_SYSTEM_MODEL.md) and outranks every
other description of it. Project root: `ocbm/host/gm_ccpa`.

Reading order: `04` (what the system is) -> `05_SESSION_FLOW.md` (wire ordering; read §8 before touching
protocol code) -> `01_FINDINGS.md` (what is proven) -> `06_BRINGUP_RUNBOOK.md` (build, install, grant, run)
-> `11_HARDENING_PLAN.md` (status ledger; work queue) -> `12_OBSERVED_FLOW.md` (what the stack does, phase by
phase) -> `13_AUDIO_ROUTING.md` (audio design and status). Authority on conflict: 04, then 05, then 12, then 11.

## 1. What this is

A wireless-only CarPlay receiver running as an unprivileged app on a GM Info 3.7 head unit (`gminfo37`,
Android 12 / API 32, x86_64; installed package `android.car.usb.handler`, classes `zeno.gmccpa.*`). The iPhone
streams CarPlay to the app over the vehicle's own 5 GHz hotspot. A Carlinkit CPC200-CCPA on USB, speaking OCBM,
supplies only the Bluetooth radio and the MFi coprocessor. Three-link diagram and the per-phase flow: `04` §1-§3.
Rust core via JNI over the in-tree `crates/vendor/*` (`04` §6).

## 2. Code state

- Branch `main` only. v5.8/v5.9 (voice real-time, per-install pairing key, auto-claim, launcher UI) is `02b12c0`
  (2026-10-08). The 2026-10-08 remediation (Phases 1-7) is committed as `b392393`..`6c95684`, with the ledger
  evidence in `f5c7d87`.
- `e584415` is the 2026-10-20 WIP snapshot of in-flight work from other sessions (committed unreviewed at the
  owner's request). Later gm_ccpa commits include `dc1ccdf` (steering-wheel Voice button -> Siri).
- Working tree (2026-11-12): UNCOMMITTED. `netprobe_app/` is flattened into the gm_ccpa root (`app/`, `build.gradle`,
  `settings.gradle`, `gradle.properties`) and docs paths are updated to match.
- `crates/` and `box/` Rust is static (owner rule, `11` header): the app adapts to it, never the reverse.
- Tier-0 gate: `bash tools/test.sh` (tree-artifact guard, cargo tests, JNI cross-build, 48 JVM test classes
  from `tools/jvm_tests.txt`, docs budget, proto check). Build: `tools/build_apk.sh`, compiled against
  `android-32`. Both procedures: `06` §4.

## 3. On the bench radio

Owner-observed 2026-11-12: build `4.0+6529691-dirty.06e21ad8` was installed for user 10 with
`adb install --user 10 -g -r -i com.android.vending <apk>`. The persistent USB grant was NOT re-run after that
fresh install. The grant is keyed to the app uid, so treat it as absent until `UsbGrant status` says otherwise
(`06` §4); the same applies to `READ_LOGS` (`06` §8.1; a plain `-r` keeps the USB grant but drops `READ_LOGS`).
The `-dirty` stamp means the APK does not correspond to a committed tree; the stamp scheme is in `06` §4.

## 4. Verified vs pending

The status ledger in [`11_HARDENING_PLAN.md`](11_HARDENING_PLAN.md) is the only record of what is LANDED and what
is device-verified (separate columns). Reference session (2026-09-09, HEVC first frame 2.598 s, 0 AUs dropped,
iAP2 tunnel authenticated): `12`. Do not mark a ledger row device-verified from a capture taken on another build.

Not bench-verified at all: the steering-wheel Voice button -> Siri path (`06` §4).

## 5. Open items and next actions

Next actions, in order:

1. Confirm what is on the radio: `dumpsys package android.car.usb.handler` (versionName), `UsbGrant status`,
   `READ_LOGS`; re-run the USB grant (`06` §4) and `READ_LOGS` grant (`06` §8.1) as needed.
2. Run the pending bench checks listed in `11` ("Still pending bench checks").
3. Take owner decisions on committing the working tree (flatten + docs); no commit unless asked.

Open ledger rows (detail in `11`): T2.2 (session ownership into the service), T5.1 (env config -> `nativeInit`),
T0.1 (unpinned `crates/vendor`), N6 (CORE lock across the JNI upcall), HEVC reader/decoder split, M7 (deferred),
B-F2 (partial), N14/N15 (Kotlin-path gaps against the reference), R6.6.27-29 (box-side credential
rotation/scrub), BP-1..BP-4 (found 2026-10-08 evening), N25 (READ_LOGS cause).

Unexplained: the radio cold-restart at 04:48:50 during rc2 testing and a ~13 min adb transport dropout with no
reboot on `6c95684`; the first may have been the same kind of event (unconfirmed). Cause unknown, no kernel/USB
lines found.

## 6. Rules in force

- Install only with `adb install --user 10 -g -r -i com.android.vending <apk>`, after `adb shell am get-current-user`
  prints `10`. A plain uninstall wipes `carplay_peers.bin`, the accessory seed and the saved hotspot details
  (iPhone re-pairs; the uid changes, so the USB grant is re-run); `uninstall -k` keeps data.
- 30 s between any uninstall and reinstall.
- Rolling back to an older artifact needs `adb uninstall -k android.car.usb.handler` first (`versionCode` is the
  commit count). The frozen golden build `netprobe-debug-v4.0.apk` (tag `baseline-2026-08-05-working`) is NOT in
  this tree; it exists only in the archived standalone `gm_ccpa` checkout, which is also the archive of record for
  `evidence/` and for the pre-merge commit hashes (they do not resolve in `ocbm`). Do not delete that checkout.
- No commit unless asked; one branch (`main`); docs corrected in place; at most 10 docs in `docs/`
  (`tools/test.sh`, `DOC_MAX=10`; currently 8). Raw session captures go in `evidence/`, never `docs/`.
- The debloat script uninstalls `com.android.vending` per-user; the `-i` install behaviour once it is gone is
  UNTESTED (`01_FINDINGS.md` §8).
- The truck is the owner's daily driver: keep changes reversible; confirm before anything destructive.

## 7. Gotchas

- Hotspot credentials: no compiled-in values. The launcher dialog opens until one is saved
  (`HotspotPrefs.kt`); `--es ssid/pass` apply to one scripted run and are not saved. Docs never print them.
- `CT_BOX_HEALTH` bit 0 (`BH_HCI_PRESENT`) is the first read when Bluetooth does nothing: a healthy OCBM link,
  a proven MFi relay and `CT_SUBSCRIBE` can all succeed while `hci0` never exists (`12` Phase 3,
  `docs/ops/06_CORRECTIONS_LEDGER.md` R-20W-5). `tools/ocbm_push.sh` is not an install; use `ocbm_install.sh --full`.
- Box logs reach the app over OCBM `CH_LOG` (logcat `[box:<source>]`); `wlan.log`, `wlan_off.log`,
  `supervisor.log` and `ocbm_boot.log` are pulled once over `CH_FILE` at session end. UART is for recovery and
  flashing only.
- GM's CarPlay service (`com.gm.domain.server.delayed`) stays running and untouched: it holds
  REBOOT/SECURE_SETTINGS/OTA, `pm disable` is blocked, and A/V runs with its `:7000` receiver live. The
  conflict was the platform mDNS hostname, solved by our own advert (`gmccpa-rx.local`, `MdnsResponder.kt`);
  `12` Failure Point 3, `01` §3.
- Per-session box state the app asserts needs a re-assertion path for every way the box resets it: teardown,
  heartbeat recovery and MGMT restart (`OcbmClient.reArmAfterMgmt`). "No box lines" is indistinguishable from "box
  silent", so a missed re-arm is invisible.
- `/tmp/no_escalate` is tmpfs and clears on box reboot, which re-arms the reboot ladder: re-assert every session.
  Other box-side rig state (patched `/script/session_supervisor.sh`, `/etc/hostapd.conf` rewritten from vehicle
  credentials): restore with `git checkout tools/session_supervisor.sh` then `tools/uart_push.sh` (both under
  the repo-root `tools/`), and `cp /etc/hostapd.conf.stock /etc/hostapd.conf`.
- USB endpoints on the CCPA (`0x1314:0x2D00`, interface class 0xFF, subclass 0xF0) are bulk IN `0x81` / OUT `0x01`.
- In-motion display is proven eligible (Play-attributed install, `distractionOptimized=true`,
  `canDrawOverlays=true`), not proven working; every capture was parked.
- Logcat boots effectively dead: `setprop persist.log.tag V` and `logcat -G 16M` before concluding anything from
  silence (`06` §7).
- Bluetooth is the single point of failure (no cable fallback): a BT drop mid-session is total loss while the
  phone is still on the hotspot. BT bring-up history: `docs/wireless/01_BT_AND_RADIO.md`.
- `crates/vendor/*` are in-tree path deps: the build compiles the working tree (a dirty tree stamps `-dirty`).
  A change under `crates/vendor/` needs a rebuild and the bench regression. `OCBM_FWD_ENC=0` is set in
  `JNI_OnLoad` because the receiver defaults to forward-encrypted-ON (drift procedure: `11`).
- `OcbmProto.kt` is app-owned (a regular file in `zeno.gmccpa.ocbm`, forked from the CarlinkAndroid copy
  2026-09-11). `crates/ocbm-proto` stays canonical for the wire; the repo-root `tools/proto_check.py` checks the Kotlin file
  and the gm_ccpa `tools/test.sh` runs it. `BH_REQUIRED_BRIDGE` is deployment policy and lives in `SessionSupervisor.kt`.
- The box serves Hands-Free (RFCOMM 5) and Headset (RFCOMM 6) SDP records on every wireless bring-up
  (`crates/vendor/wireless/src/main.rs`). iOS enumerated both without taking over telephony (2026-09-08); a real
  call with them present is untested, and no config lever suppresses them. The vehicle owns telephony.
- `zeno.carlink` on the unit is a separate, unrelated app (`carlink_native_personal`).
- `uart_cmd.sh`: `scratchpad/uart_cmd.sh OUTFILE SECONDS 'command'` (git-ignored, workstation-local; a fresh clone
  has only `host/uart_cmd.sh`, which has the incompatible signature `"shell command" [read_seconds]` and fails
  confusingly against these examples). One or two short commands per call; long compounds get garbled.
- Package/activity names: `am`/`appops`/`pm grant`/`force-stop`/`uninstall`/`dumpsys package` take
  `android.car.usb.handler`; activity components are `android.car.usb.handler/zeno.gmccpa.<Class>` (e.g.
  `MainActivity.kt` KDoc `am start` example). `tools/build_apk.sh` sets the package at link time (`INSTALL_PKG`);
  the manifest declares `android.car.usb.handler.UsbHostManagementActivity` as an `<activity-alias>` onto
  `UsbAttachActivity`, the component GM's fixed handler starts after its grant. Switching to a different package
  is an uninstall + reinstall.
- Mac toolchain: Android SDK at `~/Library/Android/sdk` (hard-coded in the scripts), kotlinc 2.3.10, Gradle-free `tools/build_apk.sh` (Java 21 fights AGP
  7.4). Compile against `android-32`: newer SDKs let API 33+ symbols resolve and `NoSuchMethodError` on the unit.
  Rust core: NDK `30.0.15729638`, target `x86_64-linux-android`, not `cargo-ndk` (panics on this workspace),
  rustup's cargo (Homebrew's shadows it); recipe `06` §6.3.
- Head-unit package removals are reversible: `adb shell pm install-existing --user <0|10> <package>` per package, or
  loop over `reference/removed_packages.txt`; a factory reset restores everything. Kept list and rationale:
  `reference/kept_packages.txt` (notably `com.gm.hmi.connection`, the SoftAP passphrase GUI, and
  `com.gm.domain.server.delayed`). `01` §8 owns the debloat detail.

## 8. File map

```
docs/                  00 handoff · 01 findings · 04 system model (canonical) · 05 session flow
                       · 06 bring-up runbook · 11 hardening ledger · 12 observed flow · 13 audio routing
app/                   Kotlin app: ocbm/ · pair/ · av/ · assist/ · logging/ · CarPlayRx · MainActivity · SessionCore
native/carplay-jni/    Rust core, JNI over receiver/pairing/mfi crates
apk/                   gitignored build output (symlink into $OCBM_CACHE made by hand; `build_apk.sh` only runs `mkdir -p apk`)
tools/                 build_apk.sh (canonical) · test.sh (Tier-0) · adb_radio_probe.sh · deepprobe.sh · tri_capture.sh
audit/                 2026-08-27 adversarial review (basis of the `11` R6 rows)
reference/             debloat script · removed_packages.txt · kept_packages.txt
(evidence/)            NOT in this tree: standalone gm_ccpa archive (recon, app runs, codecs, session_2026-08-05/ with
                       WORKING_SESSION.md, the working-session proof)
```

Every bare `evidence/...` path in this doc set resolves in the archived standalone `gm_ccpa` checkout.
