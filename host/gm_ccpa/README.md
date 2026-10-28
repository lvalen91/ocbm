# gm_ccpa

**Wireless CarPlay on a 2024 Silverado, as an ordinary non-privileged Android app.**

> **Scope:** gm_ccpa is a personal, **CarPlay-only** app, published as a reference for the idea behind it.
> It has no Android Auto support and none is planned; anyone who wants it adds it themselves.

The iPhone streams CarPlay to this app directly over the **vehicle's own 5 GHz hotspot**. A Carlinkit
CPC200-CCPA adapter on USB — speaking [OCBM](../../docs/README.md) — is reduced to two jobs: **it is the
Bluetooth radio, and it is the MFi coprocessor.** It is not the AirPlay endpoint, it hosts no access
point, and no media crosses it. The iPhone is never wired to the CCPA.

```
iPhone ──5 GHz vehicle hotspot──▶ this app (AirPlay accessory on br0, :7011)
   └────Bluetooth / iAP2 / MFi──▶ CCPA adapter ──USB/OCBM──▶ this app
```

The app claims the box over OCBM, wakes its Bluetooth stack, reconnects the known iPhone, lets the box
do iAP2 + MFi over BT, then hands the phone the **vehicle's** hotspot credentials (`0x5703`). Once the
phone switches networks the app speaks the Apple protocols to it directly; any MFi the session still
needs is relayed back over OCBM.

| | |
|---|---|
| **Head unit** | GM Info 3.7, Y181, AAOS API 32, x86_64, 2400x960 |
| **Package** | `android.car.usb.handler` (GM's USB fixed-handler name; classes stay `zeno.gmccpa.*`) — unprivileged, `/data/app`, debug-signed. Was `zeno.gmccpa` 2026-09-08..10-15; changed back 2026-10-15 at the owner's request |
| **Video** | HEVC 2400x960, hardware `OMX.Intel.hw_vd.h265`, HWC `composition=DEVICE` |
| **Audio** | AAC-LC 48 kHz out, AAC-ELD voice, mic uplink |
| **Core** | Rust receiver/pairing/MFi via JNI, cross-built `x86_64-linux-android` |

## Status — working, device-proven

Full sessions run on the truck against a real iPhone: OCBM claim, MFi relay, BT/iAP2, the `0x5703`
handoff, Bonjour, pair-setup **and** pair-verify, `/auth-setup`, RECORD, and both A/V streams — video,
audio and touch. API 32's missing X25519/Ed25519 primitives are not a gate; the JNI'd Rust core supplies
them.

Recent work, all owner-verified on the vehicle:

| Date | |
|---|---|
| 2026-09-08 | Vehicle state → CarPlay: **drive-restricted UI** (Park/Drive) and **day/night**, from `GEAR_SELECTION` + `NIGHT_MODE` |
| 2026-09-08 | **CarPlay dynamic resize** — two view areas, cropped on the hardware composer, GM system UI revealed when shrunk |
| 2026-09-08 | **Now-playing metadata** published to the AAOS media card; app auto-selects as the media source |
| 2026-09-08 | **OCBM migration** to current box firmware, with box log passthrough over `CH_LOG` |
| 2026-10-08 | **v5.8/v5.9 working tree (uncommitted; bench partly done on rc1/rc2):** real-time per-stream voice, per-install pairing key, auto-claim with the app closed, rebuilt launcher UI, media-card/launcher-tile fixes. Status and bench list: [`docs/00_HANDOFF.md`](docs/00_HANDOFF.md) |

Known-open work is tracked in [`docs/11_HARDENING_PLAN.md`](docs/11_HARDENING_PLAN.md), which carries the
live landed-vs-deferred ledger. A 2026-08-09 review found the defects clustered in lifecycle, failure
surfacing and build reproducibility — **not** in protocol correctness.

## Documentation

Start at **[`docs/00_HANDOFF.md`](docs/00_HANDOFF.md)** — it is the resume point and names the reading
order.

| Doc | |
|---|---|
| [`04_SYSTEM_MODEL.md`](docs/04_SYSTEM_MODEL.md) | **Canonical architecture.** Outranks every other description |
| [`05_SESSION_FLOW.md`](docs/05_SESSION_FLOW.md) | Wire-level buildup + the ordering rules that kill sessions (read §8) |
| [`06_BRINGUP_RUNBOOK.md`](docs/06_BRINGUP_RUNBOOK.md) | Device-proven commands and the gotchas that cost time |
| [`12_OBSERVED_FLOW.md`](docs/12_OBSERVED_FLOW.md) | What the stack actually does, phase by phase |
| [`13_AUDIO_ROUTING.md`](docs/13_AUDIO_ROUTING.md) | Audio routing, volume groups, mic uplink |
| [`01_FINDINGS.md`](docs/01_FINDINGS.md) · [`03_BUILD_PLAN.md`](docs/03_BUILD_PLAN.md) · [`11_HARDENING_PLAN.md`](docs/11_HARDENING_PLAN.md) | Feasibility analysis · build map · work queue |

## Build and run

Gradle-free: `kotlinc → d8 → aapt2 → zipalign → apksigner`, plus a cargo cross-build for the JNI core.
Needs the Android SDK + NDK. Build output goes to `~/.cache`, never into this tree — `~/Documents` is
iCloud-synced and conflict copies poison build trees (`ccpa_custom/CLAUDE.md`). Every cargo call in
`tools/test.sh` and `tools/build_apk.sh` sets `CARGO_TARGET_DIR` under `~/.cache/cargo-targets/`, and the
gate's first step (`tools/tree_artifact_guard.sh`) FAILS on any real `target`/`build`/`__pycache__`-style
directory inside `host/gm_ccpa` or `crates/` (a symlink into `~/.cache` passes).

```bash
bash tools/test.sh                 # Tier-0 gate: tree-artifact guard, cargo tests, JNI cross-build, 46 JVM test classes, docs budget, OCBM proto conformance
bash tools/build_apk.sh            # -> apk/gmccpa-debug-<STAMP>.apk, and prints the install command
                                   #    (GMCCPA_BENCH=1 -> the bench flavor, ...-bench.apk)

# Install the stamped path the build just printed — there is deliberately no "latest" symlink.
# versionCode = commit count, versionName = 4.0+<STAMP> (<sha> or <sha>-dirty.<h8>).
# Check `adb shell am get-current-user` prints 10 first. Play-attributed install keeps the in-motion path
# eligible; -g grants runtime perms (incl. SYSTEM_ALERT_WINDOW). 30 s between an uninstall and a reinstall.
adb install --user 10 -g -r -i com.android.vending apk/gmccpa-debug-<STAMP>.apk

# Logging boots effectively dead on this unit, and silence looks like a dead app
adb shell "setprop persist.log.tag V" && adb logcat -G 16M

adb shell "am start -n android.car.usb.handler/zeno.gmccpa.MainActivity"   # then press Start; the first run asks for the hotspot
```

The hotspot SSID and passphrase are entered once in the app's dialog (no compiled-in defaults since 2026-10-07;
`--es ssid/--es pass` work for one scripted run and are not saved). The passphrase cannot be read
programmatically here — take it from Settings ▸ Hotspot. The USB permission dialog is removed by a persistent
grant written over adb (re-run it after any uninstall + reinstall: `docs/06_BRINGUP_RUNBOOK.md` §4); with it the
app also claims the adapter with the app closed. If iOS
reports `No matching endpoint found`, forget the vehicle on the iPhone and re-pair; that, not GM's
`:7000`, is the usual cause.

Full procedure, expected log lines and the troubleshooting table:
[`docs/06_BRINGUP_RUNBOOK.md`](docs/06_BRINGUP_RUNBOOK.md) §7.1 (there is no §5g).

## Layout

```
netprobe_app/     the Kotlin app — ocbm/ · pair/ · av/ · logging/ · CarPlayRx · SessionCore · MainActivity (launcher)
native/           the Rust core: JNI over ccpa_custom's receiver/pairing/mfi crates
tools/            build_apk.sh · test.sh · info_plist_*.py · ADB recon · capture
docs/             see above
audit/            adversarial review, cited as the authority for doc 11's R6 section
```

## Archive: evidence and golden APKs

This project was merged into `ccpa_custom` on 2026-09-08 (`git subtree add`, history preserved). Two
things were deliberately left behind, taking the import from 368 MB to 876 KB:

- **`evidence/`** — the on-device capture corpus backing every "device-proven" claim.
- **`apk/`** — sha-stamped builds and the golden `netprobe-debug-v4.0.apk` rollback target.

Both stay in the **standalone `gm_ccpa` checkout**, along with the `baseline-2026-08-05-working` tag,
which the subtree hash rewrite did not carry across. That checkout is the archive of record — do not
delete it. `evidence/...` citations in `docs/` are **provenance, not live paths**: they name real files
there so every claim stays traceable to the capture that proved it. `apk/` here is gitignored build
output.

**Rolling back:** corrected 2026-10-08 — `versionCode` has been the commit count since 2026-09-11, so the golden
`../../../gm_ccpa/apk/netprobe-debug-v4.0.apk` (code 7) cannot be installed over a newer build with `-r`.
Run `adb uninstall -k <installed package>` first (`-k` keeps the data; `android.car.usb.handler` since 2026-10-15). A build
from another package name is a separate app, not an upgrade: uninstall it fully, or two copies install side by side. A pairing made by a v5.8+ build will not match
the golden build's fixed key, and the reverse: expect a re-pair.

## Scope

Interoperability and accessory development on the owner's own licensed hardware. The ADB recon scripts
are strictly read-only — no state changes, no i2c bus I/O.
