# SESSION HANDOFF — gm_ccpa (resume here)

**Last updated: 2026-09-10.** This is the "where we left off" doc, kept short on purpose (see
[Document policy](#document-policy) below). For what the system **is**, read
[`04_SYSTEM_MODEL.md`](04_SYSTEM_MODEL.md) — canonical, outranks every architecture blurb elsewhere.
Then [`05_SESSION_FLOW.md`](05_SESSION_FLOW.md) (wire-level session buildup, read §8 before writing any
protocol code), then `evidence/session_2026-08-05/WORKING_SESSION.md`
(the working proof — in the standalone archive, not this tree; see §9), `01_FINDINGS.md` (what's proven), `06_BRINGUP_RUNBOOK.md` (how to run it),
`11_HARDENING_PLAN.md` (live landed-vs-deferred ledger — more current than this file for the hardening
queue) and `13_AUDIO_ROUTING.md` (audio design + build status).

Project root: `~/Documents/carlink/ccpa_custom/host/gm_ccpa` (this project was merged into `ccpa_custom`
on 2026-09-08 — see §9). The `evidence/` archive is NOT under this root: it lives in the standalone
checkout at `~/Documents/carlink/old/gm_ccpa/evidence/`. Every bare `evidence/...` path in this doc set
resolves there.


## START HERE (2026-09-10): a full session was captured, and the truck is running OLD code

**1. The 2026-09-09 capture is the new reference session.** Bundle
`gmccpa_probe_20260909_225421` (+ `logcat.log`) carries the whole stack end to end with GM's own
CarPlay service live throughout: pair-verify (57 ms), `/auth-setup` (1.69 s), SETUP 130/110/102,
HEVC **first frame at 2.598 s from inbound**, 1358 frames / 0 AUs dropped, AAC-LC media, the iAP2
metadata tunnel authenticating and Identifying, album art, mic uplink, touch, clean teardown.
Canonical write-up and all the numbers: [`12_OBSERVED_FLOW.md`](12_OBSERVED_FLOW.md). It retires
several "not yet exercised" claims — most importantly **T5.3's iAP2-tunnel MFi relay is now
device-confirmed**, and the `:9004` metadata seam is live rather than deferred.

**2. The truck is on `7c92000`; HEAD is `3e37616`.** Everything in `8cadba4` — six code fixes, the
debloat reconciliation, the comment and doc corrections — has never run on hardware. Three FABLE
verification passes on 2026-09-10 read the whole delta and found it **correct and
behaviour-preserving** (`AacPlayer` publish/duck/stop rework; `SessionSupervisor` ladder trim and the
`OcbmProbe`/`OcbmClient` self-test fix; the `LogCapture` package-derivation fix). So the next truck
visit is a **deploy-and-verify** visit. Do not mark any ledger row device-verified from the
2026-09-09 capture — it was taken from the older build.

**3. Two adapters, interchangeable, one chipset difference.** Both run identical `ccpa_custom` OCBM
firmware on identical hardware except the WLAN part — NXP IW416 and Realtek RTL8822CS — abstracted by
the firmware's `radio_caps`. Nothing in this app is chipset-aware. Consequences worth knowing before
debugging: the Realtek box must load its WLAN driver before Bluetooth will attach, so `wlan0` exists
even with `wifi_ap:false` (which invalidated an old safety argument — see
[`04_SYSTEM_MODEL.md`](04_SYSTEM_MODEL.md) §2), and the two boxes derive different identities
(`CarLink-626a` / `CarLink-f867`), so a phone bonded to one holds no key for the other.

**4. Nine new open items were filed from the capture**, all in
[`11_HARDENING_PLAN.md`](11_HARDENING_PLAN.md): **A6–A9** (audio — 17 % of Siri AUs dropped, the duck
pumping within one Siri turn, the rate-blind media buffer taking 6 underruns, and every voice sink
built at a rate the HAL does not serve) and **N17–N21** (`av_idle_ms` not exported; an `ocbmd`
restart between the MFi cert and sign leaving the new daemon nonce-less; MFi timeouts missing from
`SessionSummary`; **no `SESSION v=1` line emitted at all** for a non-USB-attach launch; the stale
`fixed-handler squat` banner). T6.4 is **closed, not open**: `CARPLAY_SCREEN_DUMP` is bounded to six lines
per session by `frames < 6` in the receiver, so there was nothing to gate — an earlier claim in
this doc set that it writes a line per video frame was wrong and is retracted.

**5. Logging now reports absence, and a plain `adb logcat` is the intended capture.** `6c6ec79`
added `logging/SessionTrace.kt`: `!! EXPECTED-MISSING` at ERROR when a step that should have
followed an observed precondition does not arrive, `## STATUS` blocks carrying the standing state of
~20 components, and severity that is judged rather than decorative — the 2026-09-09 session logged
634 `I` / 7 `W` / **0 `E`** while containing a 15 s MFi timeout, 42 dropped Siri AUs and 6
underruns. **Filter with two greps, not one**: `grep NETPROBE` for our narrative AND
`awk '$3==<pid>'` for the ~250 framework lines the app causes under `CCodec`/`MediaCodec`/
`BufferQueueProducer`, which are what separate an app fault from a platform one. The `IDENTITY`
anchor line exists to make that PID filter constructible. Recipe in
[`06_BRINGUP_RUNBOOK.md`](06_BRINGUP_RUNBOOK.md) §8.3.

**6. `crates/` and `ccpa/` Rust is STATIC — do not change it** (owner, 2026-09-10). It is proven on
the box and in the macOS host and this app is one of three consumers. Where the honest fix is
upstream, record it and solve it in Kotlin or not at all. N17 is now blocked by this rather than
merely open; A11 landed Kotlin-only because of it.

**7. One methodology fix before the next capture.** The recon script runs `pm list packages -f` with
no `--user`, so it enumerates user 0 only and this app — installed to user 10 — is absent from its
own probe. Do not read package removals from it; see the trap note in
[`01_FINDINGS.md`](01_FINDINGS.md) §3.


## CarPlay dynamic resize + AAOS display modes (2026-09-08) — DEVICE-PROVEN

Pressing CarPlay's Dock resize button now moves the picture between the full panel and GM's own app
bounds, revealing GM's system UI in the shrunk state. Owner-confirmed on the truck, both directions.
Canonical description: [`04_SYSTEM_MODEL.md`](04_SYSTEM_MODEL.md) §4d — read that rather than
re-deriving any of it.

End state, which is the intended design:

- the app is in system-UI mode by default;
- CarPlay starting goes fullscreen immersive and hides every GM element;
- CarPlay's resize returns the picture to `1416x842 @ (188,118)` with GM's system UI visible again.

Both rects are static constants for this panel — nothing measured or awaited at runtime.

### Three things that cost time here, recorded so they are not re-derived

1. **`viewArea`/`safeArea` are NOT the mechanism for showing head-unit chrome.** They are Apple's
   answer to non-rectangular or trim-obscured panels: the view area is where CarPlay draws (including
   into a curved void), the safe area is the inset that keeps interactive controls out of it. This is
   documented in `ccpa_custom/docs/carplay/06_AV_PIPELINE.md` with the WWDC 2019-252 quotes. Revealing
   GM's UI is purely an AAOS windowing question with no CarPlay protocol involvement.
2. **The static-plist path bypasses the generator — third instance.** `events::switch_view_area`
   refuses any index >= `info::declared_view_area_count()`, a static written only inside
   `info::build_info`, which this app never calls. iOS drew the button and sent `requestViewArea`, and
   our own core refused it. Same shape as the OEM icon. Anything `build_info` computes as a side
   effect must be primed explicitly here.
3. **Crop, do not scale; toggle visibility, not layout.** Both failure modes are written up in §4d
   with the exact symptoms (59%-and-mispositioned, and the 377,236 offset). The 377,236 case was
   misdiagnosed three times by inference before the device evidence already in this repo settled it —
   `evidence/drive_20260818-132220/headunit.log:5762` and the `mAppBounds` / system-bar-inset rows of the `06_BRINGUP_RUNBOOK.md` §6.4 geometry table had the
   answer the whole time.

### Still open on this feature

- The transition is asymmetric and snaps on shrink rather than tracking iOS's curve. Closing that
  needs the per-step rect from the `AirPlayScreenHeader`, which is consumed in-process before the
  `:9004` seam and would need a new hop.
- GM's bar show/hide animation cannot be driven in step by a non-privileged app.


## Migrated to current OCBM + box log passthrough, package squat reverted (2026-09-08) — TRUCK-VERIFIED

The box side (`ccpa_custom`) had moved on; this session brought the app up to it and proved the whole
chain on the head unit with a real iPhone. **The subscribe config did not need to change** — current
box code still reads `wifi_ap` / `wifi_ssid` / `wifi_pass` / `wifi_channel` in
`session_supervisor.sh:apply_host_wifi_creds` on the `host_present` 0->1 edge, and still sources the
`0x5703` answer from the `/etc/hostapd.conf` it rebuilds there. The migration was additive.

What landed:

- **`CH_LOG` box log passthrough** replaces the `CH_FILE` poller. `CT_LOG_CTL [enabled][cap_kb u16 LE]`
  arms it; entries arrive as `[source u8][flags u8][seq u16 LE][unix_ms u64 LE][len u16 LE][text]`
  packed into <=4096 B frames, surfacing in logcat as `[box:<source>]`. Eleven sources instead of six,
  push instead of poll, box-side offsets, per-line stamps, drop accounting, and a `BACKFILL` flag
  (rendered `[old]`) that separates history from live. `FILE_PULL` is KEPT for the few logs `CH_LOG`
  does not carry — `wlan.log`, `wlan_off.log`, `supervisor.log`, `ocbm_boot.log` — pulled once at
  session end.
- **`seq` on CH_LOG is channel-wide, NOT per source.** The spec's "per-channel entry counter" reads
  either way; the wire settles it — one ascending run spans `bt -> radio_bt_attach -> wl -> box`. A
  per-source counter reports a false gap at every source switch. Caught on the bench before it shipped.
- **`CT_SETTIME` after every SUBSCRIBE**, not only at HELLO, or every box log stamp is bogus.
- **Re-arm on the box's terms.** `CT_SUBSCRIBE` clears the box's `CT_RADIO` inhibit unconditionally and
  resets the log stream, so `OcbmClient.subscribe()` re-applies both. A MGMT verb that bounces the
  wireless stack does the same WITHOUT a re-subscribe, so `mgmtAction()` re-applies them too — see the
  defect note below.
- **The 2026-09-05 unsubscribed-heartbeat nudge.** The box now answers a heartbeat from an unsubscribed
  host with `SEV_HOST_GONE` every 30 s, meaning "re-SUBSCRIBE". The deliberate no-subscribe relink path
  (`runAll(subscribe = false)`, used when the box returns under a live session) must NOT act on it, or
  it re-latches presence and clears the radio inhibit behind the caller's back. Guarded on an empty
  `configBlob`.
- **`CT_PAIR_CONFIRM` responder.** Unused under `pairing: just_works` (the box still auto-accepts), but
  since 2026-09-03 the box no longer auto-accepts in numeric-comparison mode — it waits 55 s for this
  and gives up. Without it, switching pairing modes is impossible.
- **Package squat reverted**: `android.car.usb.handler` -> `zeno.gmccpa`, and
  `UsbHostManagementActivity` -> `zeno.gmccpa.UsbAttachActivity`. Owner's call. **Corrected
  2026-09-10: the squat never delivered what it was for.** This entry used to say "the silent
  per-UID USB grant is gone", implying the squat had been providing one. It was not — the app still
  needed the user to grant permission for the adapter, so nothing was lost by reverting. The
  ordinary attach resolver and its one-time "always open" dialog are now the only route in.

Proven on the head unit (gminfo37, `W231E-Y181.3.2`, API 32) 2026-09-08: fresh BT pair, iAP2 Identify,
`0x5703` handoff, phone onto the vehicle SoftAP, pair-verify, `/auth-setup`, 4 MFi ops relayed over
OCBM, HEVC 2400x960 on `OMX.Intel.hw_vd.h265`, AAC-LC 48 kHz, touch. 446 box lines over `CH_LOG` with
**0 gaps, 0 drops, 0 malformed entries** under live A/V load.

### The defect this session found, and the shape of it

After `MGMT_FORGET_ALL`, `CH_LOG` went permanently silent while the OCBM link stayed healthy and the
client still reported `subscribed=true`. Nothing re-sent `CT_LOG_CTL`, because only `subscribe()`
re-armed it and a MGMT-triggered wireless restart does not re-subscribe. **The failure is invisible by
construction** — "no box lines" is indistinguishable from "the box has nothing to say". Fixed in
`OcbmClient.reArmAfterMgmt()`, which re-applies the log stream and the radio inhibit after
`MGMT_RESTART_WIRELESS` / `MGMT_FORGET_ALL` / `MGMT_FORGET_DEVICE`.

The general rule worth carrying: **every piece of per-session box state the app asserts needs a
re-assertion path for each way the box can reset it.** There are three (teardown, heartbeat recovery,
MGMT restart), and covering only the first two is what produced a silent instrument.

### A second defect, found by the restored session

`SessionSupervisor.onBtPhase` had no live-session guard. A `CT_BT_PHASE` mirror arriving during
`SESSION_UP` moved the machine to `BT_PAIRING` / `HANDOFF_SENT` under a streaming session, which
resumes mDNS discovery (inviting a rediscover-loop hijack of the live control connection) and arms the
45 s handoff watchdog — whose expiry escalates the recovery ladder into `CT_RADIO` cycling and
`MGMT_RESTART_WIRELESS` beneath a session that was fine. Observed 2026-09-08 at 900 rendered frames.
Fixed by returning early in `SESSION_UP` / `BOX_LOST_SESSION_UP`, matching `onBoxHealth` and
`onDialAccepted`.

Pre-existing, not introduced by the migration — but it only became visible because `CH_LOG` and the
restored-session path put the box's mirrors and the supervisor's transitions in one timeline.

### Still open after this session

- `Ui.kt` hard-codes the vehicle SSID and passphrase (the `LauncherUi.ssid` / `LauncherUi.pass` defaults), and the unattended start now depends on
  it. Move to a stored preference.
- The box advertises `Hands-Free` and `Headset` SDP records on every wireless bring-up (2026-09-03).
  iOS enumerated both without hijacking telephony in this session, but a real call is untested. The
  vehicle owns telephony in this design and there is no config lever to suppress those records.
- `tools/test.sh` still never compiles Kotlin; Kotlin regressions surface only at packaging time.


## OcbmProto.kt is app-owned again (forked 2026-09-11; was a symlink 2026-08-31 .. 2026-09-11)

`netprobe_app/app/src/main/java/zeno/gmccpa/ocbm/OcbmProto.kt` is a regular file in `package
zeno.gmccpa.ocbm`. From 2026-08-31 it was a relative symlink into
`ccpa_custom/host/CarlinkAndroid/app/src/main/kotlin/com/carlink/ocbm/OcbmProto.kt` ("edit the protocol
once, in the main project"), which made sense when gm_ccpa was a separate checkout and did not once it
was in-tree: CarlinkAndroid is dormant and behind the current OCBM protocol, and this app and the macOS
host are the implementations that ship — the link pointed ownership the wrong way. The fork was born
byte-identical to the CarlinkAndroid copy except the package line, so any later divergence is a
deliberate edit here. Consequences for anyone working here:

- `crates/ocbm-proto` is still canonical for the wire; `ccpa_custom/tools/proto_check.py` now checks
  this file by default (no root argument — it rejects one) and `tools/test.sh` Tier-0 runs it.
- Same-package now, so `OcbmFraming`, `OcbmProbe` and `OcbmClient` no longer import it; the consumers
  outside the package (`logging/SessionSummary`, `MainActivity`, `SessionSupervisor`,
  `UsbAttachActivity`) import `zeno.gmccpa.ocbm.Ocbm`.
- `BH_REQUIRED_BRIDGE` moved OUT of the protocol file into `SessionSupervisor.kt`, where it belongs:
  it is this deployment's policy (no `BH_WLAN_AP` required in the bridge role), not protocol.
- The shared file is a superset of what this app previously had — verified with kotlinc + javap on the
  compiled constants, zero value mismatches. It carries constants this client does not send; that is
  the cost of sharing one file and does not change behaviour.
- The MFi correlation tag is now in the canonical Rust (`MFI_TAG_LEN`), in `ocbmd`, and in the shared
  Kotlin (`Mfi.certRequest(tag)`, `signRequest(digest, tag)`, `Response.tag`, tag-aware `parse`), with
  null defaults so untagged callers are unaffected.
- Pending content fixes to this file (the `Mfi.parse` length guard, `META_SEAM_MAGIC` mutability, the
  stale airplayd comments, the `bhString` label, RETIRED markers on `LOG_SRC_RX_CONNECT/_WL`) were
  blocked by the link and are unblocked by the fork; they are a separate decision and were NOT folded
  into it, so the fork commit is reviewable as a pure move.

## 0. What this is

A **wireless-only CarPlay receiver** running as an unprivileged Android app (`zeno.gmccpa`)
on a **2024 Silverado GM Info 3.7 head unit** (`gminfo37`, Y181, Android 12 / API 32, x86_64). The
iPhone streams CarPlay (H.264/HEVC + AAC audio, touch) **directly over the vehicle's own 5 GHz WiFi
hotspot** to the app — the iPhone is never wired to anything. A **Carlinkit CPC200-CCPA** adapter, on
USB and speaking the **OCBM** protocol, is **only the Bluetooth radio and the MFi coprocessor**: it
raises no access point, terminates no AirPlay traffic, and carries no media.

## 1. Current status

- **A full session — HEVC video, AAC-LC audio, touch — ran on the truck 2026-08-05**, from an
  unprivileged app, with 0 access units dropped. See the working-session evidence link above.
- A code review (`11` Appendix A: 4 CRITICAL, 16 HIGH, none in protocol correctness) drove a hardening
  pass. **R0–R3 and part of R4 are landed and compile green; none of it has been truck-verified since
  2026-08-05.** The next truck visit is a verification visit, not a build visit (§4).
- **Per-purpose audio routing (Siri/call/alert/nav + mic uplink) is BUILT**, not merely designed —
  `AacPlayer.kt`, `VoiceRouter.kt` and `MicUplink.kt` exist and are wired into `CarPlayActivity`
  (`netprobe_app/app/src/main/java/zeno/gmccpa/av/`). `13_AUDIO_ROUTING.md` said "DESIGN ONLY, nothing
  implemented" — that was stale by several commits and has been corrected. **Owner-confirmed on the
  truck 2026-09-04:** media, Siri, call, nav routing and the mic uplink all work; only the **alert**
  sink and the **duck-and-return** interplay remain unexercised (plus the active 10–20 s media-silence
  bug after a voice turn, §1 below).
- **Bluetooth outage 2026-08-28/29, fixed and device-verified**: `/script/radio_hal.sh` and
  `radio_detect.sh` were never installed on the box, so `hci_uart` never attached and `hci0` never
  existed, while every layer above still reported success. `CT_BOX_HEALTH` bit 0 (`BH_HCI_PRESENT`) is
  now the first thing to check when Bluetooth does nothing — see §5.

### Device-verified vs not

| Landed | Verified how |
|---|---|
| Core session (pairing, OCBM/MFi relay, bridge role, handoff, HEVC + AAC-LC + touch) | Truck, 2026-08-05 |
| Box-log streaming into logcat (`[box:<file>]` over `CH_FILE`) | Truck, 2026-08-28 |
| Bluetooth fix (radio_hal/radio_detect install) | Truck, 2026-08-28 |
| Audio routing — media→`Audio`, Siri→`Voice`, call→`Phone`/`Call`, nav→`Navigation` (each audible on the right volume group), plus Siri media-pause | Truck — media-pause 2026-08-12; media/Siri/call/nav routing **owner-confirmed 2026-09-04** (no capture committed) |
| `CarPlayActivity.onSessionEnded()` screen teardown | Compile/Tier-0 only — **not truck-verified** |
| `AacPlayer.reclaimFocus()` media-focus recovery | Compile/Tier-0 only — **not truck-verified** |
| `OcbmProbe.LinkResult` gating (launcher no longer claims a link that wasn't made) | Compile/Tier-0 only — **not truck-verified** |
| Alert sink + duck-and-return interplay (`VoiceRouter`) | Compile/Tier-0 only — **not truck-verified** |
| R0–R4 hardening (11) | Compile/Tier-0 only — **not truck-verified since 2026-08-05** |

**FIXED 2026-09-08 (`b8e8736`) — re-measurement on the truck not yet recorded.** Owner-observed
2026-09-04: after a call or Siri turn ended, the `Voice` track (`VoiceRouter`) lingered and media was
silent for **10–20 s**; device-measured at **12.0 s** on gminfo37. Cause was two clocks — the assistant
edge cleared `pausedForAssistant` at `ASSISTANT_HOLD_MS` (4 s) but focus was abandoned only by
`Sink.release()` at `Purpose.idleMs` (15 s), so media resumed on the focus edge. `ASSISTANT.idleMs` is
now `ASSISTANT_HOLD_MS + 1` sweep period. `reclaimFocus()` covers the separate *permanent*
`AUDIOFOCUS_LOSS` hang. Details in `13_AUDIO_ROUTING.md` §3a.

## 2. Immediate next actions

1. **Re-assert box-side rig invariants**: checksum patched `/script/session_supervisor.sh`, confirm
   `/etc/hostapd.conf`, **re-assert `/tmp/no_escalate`** (tmpfs, self-clears on reboot — the first flap
   re-arms the reboot ladder). Record GM `CarplayService`/`:7000` state — coexistence is solved (§6), so
   it being held is expected, not a blocker.
2. **T-REG the golden APK first** to prove the rig, *then* the sha-stamped build the compile printed. The golden
   build is not in this tree — install it from the archive:
   `adb install -r ../../../gm_ccpa/apk/netprobe-debug-v4.0.apk` (tag `baseline-2026-08-05-working`,
   which likewise exists only in that repo — see §9). Acceptance: `FIRST FRAME RENDERED`, `FIRST
   AUDIO FRAME PLAYED`, a touch `sent=true`, a renderer detach/reattach, 60 s soak, 4–5 ESTABLISHED
   sockets, 0 AUs dropped at teardown.
3. **Truck-test the remaining audio paths**: the **alert** sink, **mic uplink** end to end, the
   duck-and-return interplay, and the `onSessionEnded`/`reclaimFocus` fixes. (Media, Siri, call and nav
   routing are owner-confirmed on the truck 2026-09-04; the focus fixes still need a session.)
4. **Arm untethered capture** on the same visit (`06` §7): grant `READ_LOGS` then force-stop the app
   (the gid is assigned at fork — a running process reports the grant without having it), and run
   `export_log` before leaving. The app's own printed remediation is trustworthy again — see the
   fixed defect below.

   **FIXED 2026-09-09 — `LogCapture.kt` no longer names the reverted squat package.** *(Was: OPEN
   DEFECT 2026-09-09.)* The package-squat revert (§ above) landed in the manifest and
   `build.gradle` (`applicationId zeno.gmccpa`) on 2026-09-08 but not in the Kotlin. The pre-fix file
   is byte-exact at commit `b049810`
   (`git show b049810:host/gm_ccpa/netprobe_app/app/src/main/java/zeno/gmccpa/logging/LogCapture.kt`):
   `GRANT_CMD` (`@b049810 :163`) and `FORCE_STOP_CMD` (`:166`) still hard-coded
   `android.car.usb.handler`, and those two constants were what the app printed to the operator as
   remediation when scope degraded (`:572`, `:608-609`) and stamped into the `degraded :` line of
   every degraded capture-file header (`:794`); the header's `app :` line (`:790`) carried the same
   literal a third time, typed out directly as `applicationId android.car.usb.handler` rather than via
   either constant — the app's on-device guidance named a package that no longer existed on the
   device, so a copy-pasted `pm grant` silently failed. `MainActivity.kt`'s scriptable-entry-point
   KDoc example (`@b049810 :636-637`) was stale the same way.

   Fixed 2026-09-09 by **deriving the package from the running app instead of re-typing the
   literal**, so it cannot drift at the next rename: the two `const val`s are now
   `LogCapture.grantCmd(ctx)` / `LogCapture.forceStopCmd(ctx)`
   (`fun grantCmd` / `fun forceStopCmd` in
   `netprobe_app/app/src/main/java/zeno/gmccpa/logging/LogCapture.kt`, built from `ctx.packageName`),
   with all call sites updated (`verifyScope()`, `resolveScope()`, and the `degraded :` line of
   `header()`) and `header()`'s `app :` line now printing `applicationId ${ctx.packageName}`.
   `MainActivity.kt`'s KDoc `am start` example now says `zeno.gmccpa/zeno.gmccpa.MainActivity`. `BuildConfig.APPLICATION_ID` was deliberately NOT used:
   the canonical build (`tools/build_apk.sh`) is a raw `kotlinc` compile that generates no
   BuildConfig, and the module does not set `buildFeatures { buildConfig true }`. **The operator can
   copy-paste what the app prints again.** Remaining `android.car.usb.handler` hits in the tree are
   deliberate historical comments (manifest header, `UsbAttachActivity.kt`, the `SRCDIR` header comment in `build_apk.sh`).
   Verified by a green `tools/build_apk.sh` only — compilation, not on-device behaviour.
5. ~~Archive `--es run dump_setup` hex into `evidence/`~~ — dropped 2026-09-11 with the verb: the
   native core never sets `CARPLAY_SETUP_DUMP`, so there were no files to read, and the feature
   tokens the phone proposes are in the SETUP phase2 log lines (`carplay-jni/src/lib.rs`).

Universal fallback: reinstall the golden APK (`versionCode` unchanged across R0–R4, pairing survives).

## 3. Gotchas the next session must know

**The Wi-Fi passphrases in this tree are deliberate placeholders — do not raise them as a finding.**
Both the head-unit hotspot credential (prefilled in `Ui.kt` because the passphrase cannot be read
programmatically on this unit — `SecurityException` on `getSoftApConfiguration`) and
`ccpa_custom pi/evidence/hostapd_5g.conf` are generic by choice and protect nothing. The owner
assessed and accepted this on 2026-08-31, explicitly including the case where a repo gains a remote.
Recorded here because three independent audits flagged it in one session; no rotation, no scrub, no
further action.


- **`CT_BOX_HEALTH` bit 0 (`BH_HCI_PRESENT`) is the first read when Bluetooth does nothing.** A healthy
  OCBM link, a proven MFi relay and `CT_SUBSCRIBE` can all succeed while `hci0` never exists. Full
  write-up: `ccpa_custom` `docs/ops/06_CORRECTIONS_LEDGER.md` R-20W-5, `12_OBSERVED_FLOW.md` Phase 3.
  `tools/ocbm_push.sh` is **not** an install (its default set omits the radio scripts, and now warns);
  use `ocbm_install.sh --full`.
- **Box logs reach the app over OCBM, not UART.** `OcbmProbe` follows `/tmp/{bt,wlan,wl,supervisor,
  ocbmd,attach}.log` via `CH_FILE` and re-emits them as `[box:<file>]` logcat lines. UART is for
  recovery/flashing only — any instruction to read a box log "over UART" is stale.
- **GM's CarPlay service stays running, always — coexistence is solved, do not fight it.** Full A/V runs
  with GM's `:7000` receiver live and untouched. Never `am force-stop`/`pm disable`/`pm uninstall`
  `com.gm.domain.server.delayed` — it also holds REBOOT/SECURE_SETTINGS/OTA, so stopping it costs ADB
  and every tool that depends on it. `pm disable` is blocked by a `SecurityException` anyway and it
  returns on every reboot. The real conflict was never the port: iOS keys its WiFi CarPlay endpoint
  **per host**, both adverts targeted the platform hostname `Android.local`, and GM won by registering
  at boot (we start ~51 s later). Fixed by a second mDNS advert on a hostname we own
  (`gmccpa-rx.local`, `MdnsResponder.kt`) — detail in `12_OBSERVED_FLOW.md` Failure Point 3.
- **The app installs as `zeno.gmccpa`** (labelled "GM CCPA"). The package squat on the GM USB
  fixed-handler name (`android.car.usb.handler`) was reverted (2026-09-08) — TRUCK-VERIFIED; `zeno.gmccpa`
  is now the app's real installed package, not just the source-code package. The app goes through the
  ordinary attach resolver and its one-time permission dialog — as it effectively did under the squat
  too, since **the squat did not actually remove the permission requirement** (owner, 2026-09-10). `am`/`appops`/`pm
  grant`/`force-stop`/`uninstall`/`dumpsys package` all take `zeno.gmccpa`. Install to
  **user 10** (a user-0 install does not get the attach dialog / `ACTION_USB_DEVICE_ATTACHED` routing):
  `adb install -i com.android.vending -r -g --user 10 <apk>`.
  A re-packaged app is a **new** app to the platform — first install starts with an empty `filesDir`
  (no `carplay_peers.bin`, iPhone must re-pair) and the app uid changes on every uninstall/reinstall, so
  derive it in the socket-table check rather than hardcode it (`06` §5f).
- **USB endpoints are bulk IN `0x81` / OUT `0x01`** on the CCPA (`0x1314:0x2D00`, iface class
  255/240) — any reference to `0x83/0x02` is stale.
- **`/tmp/no_escalate` is tmpfs and self-clears on box reboot**, re-arming the reboot ladder. Check and
  re-assert every session.
- **In-motion display is proven ELIGIBLE, not proven working.** Preconditions confirmed on the truck
  (Play-attributed install, `distractionOptimized=true`, `canDrawOverlays=true`); every capture so far
  was parked. Re-test in motion.
- **`ccpa_custom` is a PATH dependency — it compiles that checkout's working tree, not a pinned rev.**
  Already bitten once: upstream flipped `OCBM_FWD_ENC` to forward-encrypted-ON by default; this app
  decrypts on-box and never set the var, so seams would have silently received encrypted frames. Fixed
  by setting `OCBM_FWD_ENC=0` in `JNI_OnLoad`. **Any `ccpa_custom` advance requires a rebuild plus
  T-REG**, and a re-scan of the `receiver`/`pairing`/`mfi`/`ocbm-proto` diffs.
- **Logcat boots effectively dead.** `setprop persist.log.tag V` + `logcat -G 16M` before concluding
  anything from silence.
- **Bluetooth is the single point of failure** — no cable fallback. A BT drop mid-session is total
  session loss while the phone is still on the hotspot. `ccpa_custom` docs 37/40/41/51 are a history of
  BT bring-up fixes; read before debugging blind.
- **The truck is the owner's daily driver** — changes must stay reversible; confirm before anything
  destructive.

## 4. Document policy

**Hard cap: 10 files in `docs/`, enforced by `tools/test.sh` (`DOC_MAX=10`). Currently 9.**

Cut from 15 to 10 on 2026-08-31. `ccpa_custom` has no cap and holds 65 documents; on 2026-08-28 a
superseded section there routed a fix to a script nothing calls. Models skim, and a stale paragraph in
an authoritative-looking file is indistinguishable from a live one. Fewer files makes correcting the
right one the path of least resistance.

Rules:

1. **Update in place, do not fork.** No `_v2`, `_CORRECTIONS`, or dated siblings.
2. **Raw session captures go in `evidence/<date>/`, never `docs/`.**
3. **One subject per file; the more specific file wins on conflict**, and the other must be corrected.
4. **Retire deliberately** — when a doc is fully executed or superseded, delete it and fold anything
   still true into its successor.
5. **When you correct a stale claim, say what was wrong** — a silent edit teaches nobody.

Authority order on conflict: [`04_SYSTEM_MODEL.md`](04_SYSTEM_MODEL.md) (what the system is) →
[`05_SESSION_FLOW.md`](05_SESSION_FLOW.md) (protocol ordering) →
[`12_OBSERVED_FLOW.md`](12_OBSERVED_FLOW.md) (what the stack actually does) →
[`11_HARDENING_PLAN.md`](11_HARDENING_PLAN.md) (landed vs deferred).

## 5. Architecture (three links)

```
      ┌─ BT (CCPA's OWN radio, not the vehicle's) ─┐
iPhone┤                                             ├─ CCPA ─ USB/OCBM (0x1314:0x2d00) ─ HEAD-UNIT APP
      └─ 5GHz WiFi (myChevrolet hotspot, br0) ──────────────────────────────────────► (AirPlay endpoint)
```

- **iPhone↔CCPA (BT):** iAP2 + MFi handshake, triggers the WiFi handoff, stays up as the session anchor.
  Uses the CCPA's own BT — GM's Bluetooth/CarPlay stack is bypassed entirely.
- **CCPA↔App (USB/OCBM):** `CTRL + MFI + MGMT` only. A/V channels stay silent because the box never
  spawns its A/V layer in this role — OCBM has no per-channel subscribe.
- **iPhone↔App (WiFi):** the app is the AirPlay endpoint on br0 (`192.168.5.1`); iPhone is a client.
  Because the app runs on the AP host, client isolation never applies.

Full phase-by-phase flow (claim box → BT bring-up → iAP2/MFi → hotspot handoff → app becomes the
accessory → in-session MFi over OCBM) is in `04_SYSTEM_MODEL.md` §3.

**Language:** Kotlin app shell + JNI'd Rust protocol core (`03_BUILD_PLAN.md` §10). JNI overhead is a
non-issue — coarse-grained, zero-copy `ByteBuffer`s, decode is HW `MediaCodec` either way.

## 6. Key decisions (don't relitigate)

1. **Reuse the ccpa Rust receiver via JNI, don't rewrite.** `receiver::server::ControlServer` is
   sans-IO and generic over `MfiSigner`; the app fills that slot with a `RemoteMfiSigner` relaying over
   OCBM `CH_MFI`. The socket is driven from outside by `receiver::net::serve_connection`, not injected
   into `ControlServer`. `mfi::auth_client::MfiAuthClient` is dead code — treat it as a shape to copy.
2. **`CH_MFI` relay is live every session**, and carries four chip ops (`auth-setup` sign+cert, tunnel
   iAP2 re-identify cert+sign again) — `pair-setup`/`pair-verify` are chipless (`05` §10).
3. **`receiver` is split inside `iap_tunnel.rs`, not gated out wholesale** — the app needs
   `iap_tunnel` (iAP2-over-AirPlay-DataStream, metadata/controls post-handoff). Its two chip call sites
   (`mfi_i2c_local::try_cert`/`try_sign`) must route through the `MfiSigner` trait instead.
4. **`wifi_handoff` is already wired**, not a scaffold — swaps one `read_hostapd_ap_config()` call for
   an OCBM-supplied `AccessoryWiFiConfig`.
5. **The head-unit app is the AirPlay endpoint, not the CCPA.** `wireless/av.rs` (spawns airplayd) is
   dead glue on this box role. The macOS OCBM host app is a transport/decode template only.
6. **OCBM accessory = VID:PID `0x1314:0x2d00`**, bDeviceClass=0, Android Open Accessory gadget.
7. **SoftAP passphrase is unreadable by the app** — the user types it in from `com.gm.hmi.connection`'s
   `WifiHotspotActivity` (KEEP that package); the app hands it to the CCPA over OCBM for the `0x5703`
   handoff.

## 7. Source projects

| Path | Role | Key reuse |
|---|---|---|
| `~/Documents/carlink/ccpa_custom` | Rust OCBM stack + AirPlay receiver core + macOS host | JNI the `receiver`/`pairing`/`rtsp`/`mfi`/`metadata`/`iap2-core` crates; `crates/ocbm-proto`; `host/MacHost` = Kotlin OCBM-client template |
| `~/Documents/carlink/carlink_native_personal` | stock-protocol Kotlin app (works today) | UI theme + touch model + `video/H264Renderer.java` (feedDirect) + Media3/PCM audio stack salvaged verbatim/near-verbatim |
| `~/Documents/carlink/carplay_simulator` | Apple protocol reference | iAP2 msg/TLV dict, real receiver-session oslog captures, `VDCSchema-External.json` |

Built from scratch, no salvage: the OCBM/wireless facade, AAC/AAC-ELD codec stage, the avcC→Annex-B
shim, a separate HEVC renderer (the salvage app is H.264-only PCM).

## 8. Hardware / environment state

- **Test truck (`gminfo37`):** ADB over USB when the Mac is plugged in (serial `CJUD4R4f1b5fd0`).
- **`zeno.carlink`** on the unit is a **separate, unrelated app** (stock-Carlinkit-firmware product,
  `carlink_native_personal`). Do not confuse it with this project's `zeno.gmccpa`.
- **`uart_cmd.sh` signature:** `uart_cmd.sh OUTFILE SECONDS 'command'`. Send one or two short commands
  per call — long compound commands over UART get truncated/garbled. **This means
  `ccpa_custom/scratchpad/uart_cmd.sh` specifically** — a different, incompatible `uart_cmd.sh` also
  exists at `ccpa_custom/host/uart_cmd.sh` (`Usage: uart_cmd.sh "shell command" [read_seconds]` —
  command first, seconds optional, no outfile argument); running that one against this doc's examples
  fails confusingly rather than loudly (found 2026-09-09).
- **Mac build toolchain:** Android SDK `~/Library/Android/sdk` (platforms 35–37), kotlinc 2.3.10.
  Gradle-free build via `tools/build_apk.sh` (kotlinc→d8→aapt2→zipalign→apksigner; Java 21 fights AGP
  7.4). **Compiles against `android-32`** (the unit's real API level, since `11` R0) — `android-35` let
  API 33+ symbols resolve at build time and `NoSuchMethodError` on the truck. Rust core: NDK
  `30.0.15729638`, target `x86_64-linux-android` (the head unit is x86_64, not arm64), **not**
  `cargo-ndk` (panics on this workspace) — recipe in `06` §5d. Use `~/.cargo/bin/cargo`; Homebrew's
  cargo shadows rustup and cannot add cross-targets.
- **APKs:** `tools/build_apk.sh` emits `apk/gmccpa-debug-<sha>.apk` and repoints
  no "latest" symlink — it prints the exact path and a ready-made `adb install` line instead
  (the symlink was removed 2026-09-10: pinned `versionCode` + a link surviving a failed build meant
  `-r` could silently install stale code on the truck). **Renamed from `netprobe-debug-*` on 2026-09-10** — the app stopped
  being "NetProbe" in 2026-08 and installs as `zeno.gmccpa` / "GM CCPA", so the artifact name was the
  last place the old identity survived. The frozen golden build keeps its historical name (below):
  it is a real file in the archive, not a generated one. `apk/` is gitignored here and starts empty on a fresh clone. The
  frozen golden build `netprobe-debug-v4.0.apk` (tag `baseline-2026-08-05-working`) lives only in the
  standalone `gm_ccpa` archive — see §9. `versionCode` stays 7 through R0–R4 so rollback is a plain
  `adb install -r` that keeps `carplay_peers.bin` (existing pairing).

### Box-side state (all reversible)

| What | Restore |
|---|---|
| `/script/session_supervisor.sh` patched (`wifi_ap` gate, credential injection) | `git checkout tools/session_supervisor.sh` in `ccpa_custom`, then `tools/uart_push.sh` |
| `/etc/hostapd.conf` rewritten with vehicle creds | `cp /etc/hostapd.conf.stock /etc/hostapd.conf` |
| `/tmp/no_escalate` inhibits the reboot ladder | tmpfs — clears itself on next boot, re-assert every session |

### Head-unit package changes (2026-07-31, reversible)

41 packages + Play Store were `pm uninstall --user`'d on both users during the feasibility
investigation, at owner request; **Play Store was later reinstalled**, attribution (`-i
com.android.vending`) works, and `com.android.vending` is present on user 0/10 today. Full list:
[`../reference/removed_packages.txt`](../reference/removed_packages.txt); kept list and rationale in
[`../reference/kept_packages.txt`](../reference/kept_packages.txt) — notably `com.gm.hmi.connection`
(SoftAP passphrase GUI) and `com.gm.domain.server.delayed` (hosts GM's `:7000` CarPlay receiver, holds
REBOOT/SECURE_SETTINGS/OTA — never touch it, see §3).

Reverse per package: `adb shell pm install-existing --user <0|10> <package>`, or restore the whole list:

```bash
while read -r p; do adb shell pm install-existing --user 10 "$p"; adb shell pm install-existing --user 0 "$p"; done < reference/removed_packages.txt
```

A factory reset restores everything.

## 9. Project file map

```
docs/                      reading order: 04 -> 05 -> evidence/session_2026-08-05 -> 01 -> 06 -> 11 -> 13 -> 00
  00_HANDOFF.md            <- you are here (resume entry point)
  01_FINDINGS.md           feasibility scorecard, topology, :7000, access boundaries, codecs
  03_BUILD_PLAN.md         reuse/move/create map, JNI keystone, language decision, build order
  04_SYSTEM_MODEL.md       CANONICAL architecture, division of responsibility
  05_SESSION_FLOW.md       wire-level session buildup + the ordering rules that kill sessions (READ §8)
  06_BRINGUP_RUNBOOK.md    device-proven commands (§5g) + the gotchas that cost time
  11_HARDENING_PLAN.md     the fix plan + LIVE implementation status (landed vs deferred) <- work queue
  12_OBSERVED_FLOW.md      what our stack actually does, phase by phase
  13_AUDIO_ROUTING.md      audio routing: what's built, what's truck-verified, what's still open
(evidence/)                NOT in this tree — 01 shell recon · 02 deep recon · 03 app runs ·
                           04 AirPlay/pair-setup · 05 codecs · ios/ · session_2026-08-05/ (the
                           working-session corpus). Lives in the standalone gm_ccpa archive.
netprobe_app/              the Kotlin app: ocbm/ · pair/ · av/ · CarPlayRx · MainActivity (instrument)
native/carplay-jni/        the Rust core — JNI over ccpa_custom's receiver/pairing/mfi crates
apk/                       gitignored build output, empty on a fresh clone:
                           gmccpa-debug-<sha>.apk builds; no 'latest' symlink.
                           v4.0 = GOLDEN / rollback target — in the archive, not here.
tools/                     build_apk.sh (canonical, android-32) · test.sh (Tier-0 gate) ·
                           adb_radio_probe.sh · deepprobe.sh · tri_capture.sh
reference/                 debloat script · removed_packages.txt · kept_packages.txt
```

Git: tag `baseline-2026-08-05-working` is the golden commit; each hardening release is its own commit
(`R0` `df0cedc` → `R4 partial` `43f41b0`), so a truck failure bisects to a task. **Those hashes and that
tag belong to the standalone `gm_ccpa` repo.** This project was merged into `ccpa_custom` at
`host/gm_ccpa` on 2026-09-08 via `git subtree add`, which preserved the commits but **rewrote every
hash** — the tag was not carried across. To bisect or roll back by tag, use the standalone checkout at
`~/Documents/carlink/old/gm_ccpa/`, which is the archive of record for history, `evidence/` and the
golden APKs. Do not delete it.

Cross-project memory: the Claude memory `direct-wifi-carplay-aaos` (in the `ccpa_custom` project's
memory dir) carries the full device-proven history.
