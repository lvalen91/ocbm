# gm_ccpa

Wireless CarPlay on a 2024 Silverado, as an ordinary non-privileged Android app.

## What it is

The iPhone streams CarPlay to this app directly over the vehicle's own 5 GHz hotspot. A Carlinkit CPC200-CCPA
adapter on USB, speaking [OCBM](../../docs/README.md), does two jobs only: it is the phone's Bluetooth radio and
the MFi coprocessor. It is not the AirPlay endpoint, hosts no access point, and no media crosses it. The iPhone is
never wired to the adapter.

```
iPhone ──5 GHz vehicle hotspot──▶ this app (AirPlay accessory on br0, :7011)
   └────Bluetooth / iAP2 / MFi──▶ CCPA adapter ──USB/OCBM──▶ this app
```

The app claims the adapter over OCBM, wakes its Bluetooth stack, reconnects the known iPhone, lets the adapter do
iAP2 and MFi over Bluetooth, then hands the phone the vehicle's hotspot credentials (`0x5703`). Once the phone
joins the hotspot the app speaks the Apple protocols to it directly; MFi operations the session still needs are
relayed to the adapter over OCBM. Architecture: [`docs/04_SYSTEM_MODEL.md`](docs/04_SYSTEM_MODEL.md).

## Scope

- Personal reference app for the owner's own hardware (interoperability and accessory development); published to
  document the approach.
- CarPlay only. No Android Auto support, none planned.
- Unprivileged: installed to `/data/app`, debug-signed, no privileged permission. Adb-granted extras: `READ_LOGS` (optional, widens log capture), the persistent USB
  grant, and the optional `ASSISTANT` role (steering-wheel Voice key); see `docs/06_BRINGUP_RUNBOOK.md` §4, §8.1.
- Does not carry media through the adapter and does not use a cable to the phone.
- In-motion display is proven eligible, not proven working (every capture was parked).

## Specification

| | |
|---|---|
| Head unit | GM Info 3.7, Y181, AAOS API 32, x86_64, 2400x960 |
| Package | `android.car.usb.handler` (GM's USB fixed-handler name); classes `zeno.gmccpa.*` |
| Video | HEVC 2400x960, hardware decoder `OMX.Intel.hw_vd.h265`, HWC `composition=DEVICE` |
| Audio | AAC-LC 48 kHz out, AAC-ELD voice, mic uplink |
| Core | Rust receiver/pairing/MFi via JNI, cross-built for `x86_64-linux-android` |

## Status

Full sessions run on the truck against a real iPhone: OCBM claim, MFi relay, Bluetooth/iAP2, hotspot handoff,
pair-setup and pair-verify, `/auth-setup`, video, audio, touch, vehicle state (drive-restricted UI, day/night),
CarPlay resize, now-playing metadata, real-time per-stream voice, per-install pairing key, auto-claim with the
app closed. The remaining bench checks are listed in
[`docs/00_HANDOFF.md`](docs/00_HANDOFF.md). Open and deferred work is the ledger in
[`docs/11_HARDENING_PLAN.md`](docs/11_HARDENING_PLAN.md).

## Build and run

Gradle-free: `kotlinc → d8 → aapt2 → zipalign → apksigner`, plus a cargo cross-build of the JNI core; needs the
Android SDK and NDK. The Gradle files are for Android Studio indexing only (a Gradle APK lacks the native
`.so`). Build output goes to `$OCBM_CACHE`, never into the tree (`ocbm/CLAUDE.md`; rationale in
[`docs/ops/00_BUILD_AND_DEPLOY.md`](../../docs/ops/00_BUILD_AND_DEPLOY.md)).

```bash
bash tools/test.sh          # Tier-0 gate
bash tools/build_apk.sh     # -> apk/gmccpa-debug-<STAMP>.apk; prints the install command
                            #    (GMCCPA_BENCH=1: bench flavor)

# Confirm `adb shell am get-current-user` prints 10 first; wait 30 s between an uninstall and a reinstall.
adb install --user 10 -g -r -i com.android.vending apk/gmccpa-debug-<STAMP>.apk
adb shell "am start -n android.car.usb.handler/zeno.gmccpa.MainActivity"   # then press Start
```

Hotspot credentials are entered once in the app (none are compiled in). Everything else (logging revival, USB
permission grant, scripted verbs, bring-up of a full A/V session, troubleshooting):
[`docs/06_BRINGUP_RUNBOOK.md`](docs/06_BRINGUP_RUNBOOK.md) §4 and §7.1.

## Layout

```
app/src/main/     Kotlin app, zeno.gmccpa: ocbm/ pair/ av/ logging/ assist/ plus session core and launcher UI
app/src/nobench/  release stubs of the bench-only hooks (compiled unless GMCCPA_BENCH=1)
app/src/bench/    bench-only hooks (mic_inject, finish_launcher)
app/src/test/     host JVM tests, listed in tools/jvm_tests.txt
native/carplay-jni/  Rust JNI core (path deps on crates/vendor/{receiver,pairing,mfi})
tools/            build/test gate, info.plist generators, ADB recon, capture scripts
docs/             design, runbook and status documents (below)
audit/            adversarial review, basis of the docs/11 R6 rows
reference/        debloat script and package lists
apk/              build output (symlink into $OCBM_CACHE made by hand, ignored; `build_apk.sh` only runs `mkdir -p apk`)
```

## Documents

| Doc | Content |
|---|---|
| [`00_HANDOFF.md`](docs/00_HANDOFF.md) | Resume point: code state, bench radio state, verified vs pending, rules, gotchas, file map |
| [`01_FINDINGS.md`](docs/01_FINDINGS.md) | Feasibility scorecard, network topology, access boundaries, codecs, debloat keep-list |
| [`04_SYSTEM_MODEL.md`](docs/04_SYSTEM_MODEL.md) | Canonical architecture: division of responsibility, session phases, lifecycle state machine, display modes, app surface, design decisions; outranks other docs |
| [`05_SESSION_FLOW.md`](docs/05_SESSION_FLOW.md) | Code-traced wire sequence and the ordering rules that kill sessions (§8) |
| [`06_BRINGUP_RUNBOOK.md`](docs/06_BRINGUP_RUNBOOK.md) | Build, install, USB grant and run commands; gotchas; Rust core build; full A/V bring-up; untethered capture |
| [`11_HARDENING_PLAN.md`](docs/11_HARDENING_PLAN.md) | Status ledger: landed, open, deferred, device-verified |
| [`12_OBSERVED_FLOW.md`](docs/12_OBSERVED_FLOW.md) | The session as observed on hardware, phase by phase, with failure points |
| [`13_AUDIO_ROUTING.md`](docs/13_AUDIO_ROUTING.md) | Audio routing, volume groups, voice design, mic uplink |

## Archive and rollback

The on-device capture corpus (`evidence/`) and the golden `netprobe-debug-v4.0.apk` (versionCode 7) are kept only
in the standalone `gm_ccpa` archive checkout, which is the archive of record. `evidence/...` citations in `docs/`
name files there; they are not paths in this tree.

`versionCode` is the commit count, so the golden build cannot be installed over a newer one with `-r`: run
`adb uninstall -k android.car.usb.handler` first (`-k` keeps the data). A build under another package name is a
separate app, not an upgrade: uninstall it fully. A pairing made by a current build does not match the golden
build's fixed key, nor the reverse: expect a re-pair. A plain `adb uninstall android.car.usb.handler` also removes
the pairing, the accessory key and the saved hotspot.
