# gm_ccpa

**Wireless CarPlay on a 2024 Silverado, as an ordinary non-privileged Android app.**

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
| **Package** | `zeno.gmccpa` — unprivileged, `/data/app`, debug-signed |
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
Needs the Android SDK + NDK. Build output goes to `~/.cache`, never into this tree.

```bash
bash tools/test.sh                 # Tier-0 gate: host cargo tests + OCBM proto conformance
bash tools/build_apk.sh            # -> apk/netprobe-debug-<sha>.apk

# Play-attributed install keeps the in-motion path eligible; -g grants runtime perms; user 10 is foreground
adb install -i com.android.vending -r -g --user 10 apk/netprobe-debug-latest.apk
adb shell appops set zeno.gmccpa SYSTEM_ALERT_WINDOW allow

# Logging boots effectively dead on this unit, and silence looks like a dead app
adb shell "setprop persist.log.tag V" && adb logcat -G 16M

adb shell "am start -n zeno.gmccpa/.MainActivity --es run full \
           --es ssid '<ssid>' --es pass '<passphrase>'"
```

Expect the standard USB permission dialog once per device ("always open" retires it). The hotspot
passphrase cannot be read programmatically here — take it from Settings ▸ Hotspot and pass it in. If iOS
reports `No matching endpoint found`, forget the vehicle on the iPhone and re-pair; that, not GM's
`:7000`, is the usual cause.

Full procedure, expected log lines and the troubleshooting table:
[`docs/06_BRINGUP_RUNBOOK.md`](docs/06_BRINGUP_RUNBOOK.md) §5g.

## Layout

```
netprobe_app/     the Kotlin app — ocbm/ · pair/ · av/ · CarPlayRx · MainActivity (instrument)
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

**Rolling back:** `adb install -r ../../../gm_ccpa/apk/netprobe-debug-v4.0.apk`. `versionCode` is held
constant across releases, so this is a plain reinstall that preserves `carplay_peers.bin` — and therefore
the existing pairing.

## Scope

Interoperability and accessory development on the owner's own licensed hardware. The ADB recon scripts
are strictly read-only — no state changes, no i2c bus I/O.
