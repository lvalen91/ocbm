# 13 — Audio routing (LIVING DOCUMENT — update in place)

**What this is.** Full CarPlay audio: media, phone calls, Siri, alerts, navigation and the microphone
uplink. Derived from four parallel studies — the working Android stack in `carlink_native_personal`,
the `ccpa_custom` receiver, its macOS host, and an audit of what this app does versus what it
advertises. See [`12_OBSERVED_FLOW.md`](12_OBSERVED_FLOW.md) for the flow this plugs into.

**Status: BUILT, compiles clean, Tier-0 green. Five of the six sinks are now truck-confirmed.** All
five build-order steps below (A1–A5) landed in commits `525a845`/`ee1f270`/`5515bf4`/`a65a1f0`
(2026-08-12) plus the 2026-08-28/29 focus-recovery fixes. **Owner-confirmed on the truck 2026-09-04**
(no capture committed): media, Siri, call and navigation audio each route audibly to the correct GM
vehicle volume group — Media→`Audio`, Siri→`Voice`, Call→`Phone`/`Call`, Nav→`Navigation` — and the
**mic uplink works** (§4). Still unconfirmed on hardware: only the **alert** sink and the
**duck-and-return** interplay — see §0a. The post-Siri media-silence bug reported on 2026-09-04 is
**fixed** in `b8e8736` (2026-09-08) — see §3. Do not read "built" as "proven" for the alert/ducking
gaps.

---

## 0a. What is truck-verified, and what is not

| Claim | State |
|---|---|
| Media (AAC-LC 48 k stereo, `AacPlayer`) | **Device-proven**, 2026-08-05 working session |
| Media pausing so the volume knob reaches `VOICE_COMMAND` during Siri | **Device-proven**, 2026-08-12 (117 knob steps measured; see §3) |
| `atype` byte on `:9003` (box-side `tag_voice`) | Landed in `ccpa_custom` `forward.rs`; parsed by `VoiceRouter`. **Routing device-confirmed 2026-09-04**: Siri, call and nav each land audibly on the correct bus, so the `atype`→purpose split is right on hardware |
| Per-purpose sinks: CALL and NAV routing, AAC-ELD decode, silence-fill watchdog (`VoiceRouter.kt`) | **Owner-confirmed on the truck 2026-09-04** (no capture committed): Siri→`Voice`, Call→`Phone`/`Call`, Nav→`Navigation` each audible on the correct volume group. The **ALERT** sink and the energy-gated **ducking** are still unexercised |
| Mic uplink (`MicUplink.kt`, `mic-uplink-eld` feature) | **Owner-confirmed on the truck 2026-09-04**: works. CarPlay raises the AAOS mic-in-use indicator when it requests the mic and it clears cleanly on release; no GM AAOS mic volume/gain control exists (expected) |
| `AacPlayer.reclaimFocus()` (recovers from `AUDIOFOCUS_LOSS` without a permanent hang) | Landed 2026-08-28, compiles — **not truck-verified** |
| Media recovery after a voice/call turn | **FIXED 2026-09-08 (`b8e8736`), re-measurement on the truck not yet recorded.** Owner-observed 2026-09-04 as 10–20 s of silence; device-measured at **12.0 s** on gminfo37. Two clocks: `assistantTick` cleared `pausedForAssistant` after `ASSISTANT_HOLD_MS` (4 s), but focus was abandoned only by `Sink.release()` at `Purpose.idleMs` (15 s for ASSISTANT), so media resumed on the focus edge, not the assistant edge. `ASSISTANT.idleMs` is now `ASSISTANT_HOLD_MS + 1` sweep period, closing the gap to ~1 s by design — see §3 |

**Source, current as of this doc:** `netprobe_app/app/src/main/java/zeno/gmccpa/av/{AacPlayer,
VoiceRouter, MicUplink}.kt`, wired into `CarPlayActivity.kt`. `VoiceRouter` (597 lines) replaces the
earlier `drainVoice`, which accepted `:9003` and discarded every byte — that silence is why this
section previously read "design only, nothing implemented"; it no longer applies.

---

## 0. The wire format this depends on

`:9003` carries every non-media stream, tagged:

```rust
// ccpa_custom/crates/vendor/receiver/src/forward.rs:101
pub fn tag_voice(au: &[u8], rate: u32, channels: u16, atype: u8) -> Vec<u8>
// -> [rate u32 BE][ch u16 BE][atype u8][len u32 BE][AU]
```

`atype` is the CarPlay purpose byte (0 media, 1 telephony, 2 speechRecognition, 3 alert, 4 default) —
**this is A1, landed**, not the historical gap. Before it, the three purposes that most need
separating were indistinguishable on the wire:

| Purpose | `/info` entry | Format |
|---|---|---|
| Siri downlink | type 100 `default` | AAC-ELD 16 kHz mono |
| Phone call | type 100 `telephony` | AAC-ELD 16 kHz mono |
| Speech recognition | type 100 `speechRecognition` | AAC-ELD 16 kHz mono |

`(rate, channels)` alone can only separate *16 kHz mono* from *48 kHz stereo* — one bit where five
purposes are needed. `VoiceRouter.purposeFor()` uses `atype` first and falls back to format only for
`atype 4` (`default`), where 16 kHz mono is genuinely Siri and 48 kHz stereo is genuinely nav/alt-audio
— that split is safe because the two entries actually differ in format.

Both reference implementations that guessed from format alone got it wrong: the macOS host collapses
`audioType` to a single `isVoice` bit, and `carlink_native_personal`'s `resolveTargetSlot` sends Siri
audio to the phone-call track whenever both flags are set (16 kHz mono matches the first branch). This
is why A1 was required before A2, not an optional refinement.

---

## 1. Target routing

The vehicle's bus map, read from `/vendor/etc/car_audio_configuration.xml` on the unit:

| Bus | Context | CarPlay source | Android `AudioAttributes` usage | `VoiceRouter.Purpose` |
|---|---|---|---|---|
| `bus0_media_out` | music | type 102 `media` | `USAGE_MEDIA` + `CONTENT_TYPE_MUSIC` | (handled by `AacPlayer`, not `VoiceRouter`) |
| `bus1_navigation_out` | navigation | type 101, 48 k stereo | `USAGE_ASSISTANCE_NAVIGATION_GUIDANCE` + `SPEECH` | `NAV` |
| `bus2_voice_command_out` | voice_command | atype 2/4, 16 k mono | `USAGE_ASSISTANT` + `SPEECH` | `ASSISTANT` |
| `bus3_call_ring_out` | call_ring | atype 3 `alert` | `USAGE_VOICE_COMMUNICATION_SIGNALLING` + `SONIFICATION` | `ALERT` |
| `bus4_call_out` | call | atype 1 `telephony` | `USAGE_VOICE_COMMUNICATION` + `SPEECH` | `CALL` |

**You never address a bus directly.** GM's `CarAudioService` maps `usage → context → volume group →
bus`; picking the right usage is the whole mechanism. `carlink_native_personal` confirms this and
contains no `CarAudioManager`, no zone API, no bus address anywhere.

> **`USAGE_VOICE_COMMUNICATION_SIGNALLING` for alerts is UNPROVEN on this head unit** — no truck test
> has exercised the `ALERT` sink. GM remaps `USAGE_NOTIFICATION_RINGTONE` to `BUS_NOTIFICATION` rather
> than AOSP's `CALL_RING`, and `carlink_native_personal`'s attempt to steer ringtones was **reverted**
> (blocked voice-assistant volume adjust without improving ringtone control). Treat this row as a
> hypothesis to measure on the next truck visit, not a confirmed mapping.

---

## 2. Five sinks, one per usage — implemented in `VoiceRouter.kt`

One `AudioTrack` + `MediaCodec` per usage (`VoiceRouter.Sink`, one instance per active `Purpose`), all
allowed to play simultaneously. **Mixing is AudioFlinger's job** — no app-level mixer; nothing preempts
the media track because another purpose started.

| Sink | Fed by | Format | Status |
|---|---|---|---|
| MEDIA | `:9002` ADTS → `AacPlayer` | AAC-LC 48 k stereo | **Device-proven** (2026-08-05) |
| CALL | `:9003` atype 1 → `VoiceRouter` | AAC-ELD 16 k mono | **Owner-confirmed on truck** (2026-09-04) — audible on the `Phone`/`Call` group |
| ASSISTANT | `:9003` atype 2/4 @ 16 k mono → `VoiceRouter` | AAC-ELD 16 k mono | **Owner-confirmed on truck** (2026-09-04) — Siri audible on `Voice`; media-pause also proven (§3) |
| ALERT | `:9003` atype 3 → `VoiceRouter` | AAC-ELD 48 k stereo | Built, untested on truck |
| NAV | `:9003` atype 4 @ 48 k stereo → `VoiceRouter` | AAC-ELD 48 k stereo | **Owner-confirmed on truck** (2026-09-04) — audible on `Navigation` |

### Decoding the voice seam

`:9003` carries **raw AAC-ELD access units**, not ADTS — there is no self-describing header, so
`MediaCodec` needs a hand-built `csd-0`. `VoiceRouter.eldCsd()` builds it from the encoder's real
AudioSpecificConfig, recorded in `ccpa_custom/docs/ops/05_AUDITS.md (was docs/50:88-96`:)

```
csd-0 = f8 f0 31 2c 00 bc 00     // AAC-ELD, SBR enabled by fdk auto-mode, frameLength 480
```

**Not** the `f8f03000` the older docs claimed. This is the one thing the macOS host never had to
solve — it hardcodes `mFramesPerPacket` per codec instead, which `MediaCodec` will not accept.

---

## 3. Playback mechanics — implemented, per `carlink_native_personal`-derived rules

Every rule below is a device-proven GM behaviour, applied in `VoiceRouter.Sink` and `AacPlayer`. They
are not preferences.

1. **48 kHz everywhere, no `PERFORMANCE_MODE_LOW_LATENCY`.** `AUDIO_OUTPUT_FLAG_FAST` is denied to
   third-party apps on this unit (`createTrack_l(8): AUDIO_OUTPUT_FLAG_FAST denied by server`), so low
   latency mode buys nothing and can add jitter. Not AOSP-documented.
2. **Buffer = `getMinBufferSize() × 4`**, prefilled before `play()`.
3. **`WRITE_NON_BLOCKING` with residual retry** — a short write (including `written == 0`) is retried
   next pass, not dropped.
4. **One `OnAudioFocusChangeListener` instance per usage** (`VoiceRouter.Sink.focusListener`) — AAOS
   `CarAudioFocus` keys on listener identity, so a shared listener cannot hold focus for two usages.
5. **Silence-fill, not pause, for continuous streams** (`Sink.keepAlive()`). AAOS picks the volume
   group from *active players*; a track that stops playing surrenders the volume group and hardware
   volume keys jump elsewhere. Paced on an absolute clock (`nextSilenceAt`, floored) — relative pacing
   accumulates loop lateness into guaranteed underruns. `ALERT` is a short burst and does pause.
6. **Idle watchdog** (`sweepIdle()`, 1 Hz, per-purpose `idleMs`) — a sink with no audio past its window
   releases: track/codec/focus torn down, volume group freed. This exists because a dropped stop
   message previously wedged `USAGE_VOICE_COMMUNICATION` as the active volume group for a whole
   session.
7. **`pause()`/release before `abandonAudioFocusRequest()`** at sink teardown, so AAOS sees no active
   player of that usage at abandon time.

### Siri volume: PAUSE media, do not merely duck it — CONFIRMED ON HARDWARE 2026-08-12

Ducking is right for *audibility* and wrong for the *volume knob*; both are needed and are different
mechanisms.

This unit's `CarVolume` priority list (V1) is:

```
NAVIGATION > CALL > MUSIC > ANNOUNCEMENT > VOICE_COMMAND > CALL_RING > ...
```

`VOICE_COMMAND` sits **below** `MUSIC`, and a *ducked* MUSIC track still counts as active — so while
any media plays, the volume knob targets MUSIC even mid-Siri, with `USAGE_ASSISTANT` focus held.
Measured before the fix: **117 knob steps, every one to group 5 (MUSIC); group 2 never touched once.**

Audio-focus gain type is **not** an input — the strings do not appear in `CarVolume.java`, and calls
use the same `GAIN_TRANSIENT` as Siri with the opposite outcome.

**The fix, verified working:** pause the media `AudioTrack` for the duration of a Siri turn
(`AacPlayer.setAssistantSpeaking`, driven by `VoiceRouter.assistantTick`), which removes MUSIC from the
active set and lets `VOICE_COMMAND` win. GM's popup then renders it as "voice". Hold ~4 s past Siri's
last audio (`ASSISTANT_HOLD_MS`) — a shorter hold lets MUSIC re-enter while the driver is still reaching
for the dial. Uses `pause()` **without** `flush()` so buffered media resumes instead of dropping audio.

Two things this also settles:
- **Programmatic group volume is impossible here** — `setGroupVolume` is `@SystemApi` behind
  `CAR_CONTROL_AUDIO_VOLUME` (`signature|privileged`); this app is debug-signed in `/data/app`. No
  in-app trim substitutes for it.
- **Stock GM CarPlay can never reach `VOICE_COMMAND`** — it plays everything through a single
  `USAGE_UNKNOWN` source, so its Siri is indistinguishable from media.

### Ducking (`AacPlayer.setDucked`, `VoiceRouter` energy gate)

Duck **only** the media track: `effective = mediaVolume × min(commandedDuck, focusDuck)`.

- Focus map: `LOSS_TRANSIENT_CAN_DUCK` → **0.2**, `LOSS_TRANSIENT`/`LOSS` → 0.0, `GAIN` → 1.0.
  0.2, not 0.8 — "duck by 20%" (≈2 dB) was reported by users as "does not duck".
- Expect to duck yourself through the focus round-trip: this app's own NAV request
  (`GAIN_TRANSIENT_MAY_DUCK`) and CALL/Siri requests (`GAIN_TRANSIENT`) come back to the MEDIA listener
  as a loss. That is the mechanism, not a bug.
- **Duck trigger is energy-gated, not packet-gated** (`VoiceRouter.peakExceeds`, int16 peak ≥ ~800,
  ≈ −32 dBFS) — iOS streams continuous digital silence on idle voice streams, so a flow-based trigger
  would duck media permanently from session start.

### Latency classes

The iPhone delivers media in bursts ahead of realtime (~750 ms fast, then ~200 ms pause) and voice at
realtime. Media uses a deep buffer (~1 s) with staged pre-roll; voice uses ~150–300 ms rings with no
pre-roll — `carlink_native_personal`'s 750 ms media / 300 ms nav split reached the same conclusion
independently.

---

## 3a. FIXED — media stayed silent 12 s after a voice/call turn

**Owner-observed on the truck 2026-09-04; device-measured and fixed 2026-09-08 (`b8e8736`). Truck
re-measurement not yet recorded.** When a phone call or Siri prompt ended, the `Voice` track
(`VoiceRouter`) stayed active and the media `AudioTrack` (`AacPlayer`) stayed silent before recovering
on its own. Reported as 10–20 s; measured at **12.0 s** on gminfo37:

```
21:25:55.940 [voice] assistant done — media resumes
21:25:55.940 [aac  ] media paused for the assistant      <- same millisecond
21:26:07.918 [voice] siri: idle 15s — released (focus + volume group freed)
21:26:07.919 [aac  ] media focus LOSS_TRANSIENT -> GAIN
21:26:07.932 [aac  ] media resumed after focus gain
```

**Cause: two clocks, and only one of them freed the focus.** `assistantTick` declares Siri done after
`ASSISTANT_HOLD_MS` (4 s) of quiet and clears `pausedForAssistant`, but audio focus is abandoned ONLY by
`Sink.release()`, which `sweepIdle` calls at `Purpose.idleMs` — 15 s for `ASSISTANT`. In between, the
sink still held `AUDIOFOCUS_GAIN_TRANSIENT`, `AacPlayer` stayed in `LOSS_TRANSIENT` with
`pausedForFocus` set, and media could not resume. It came back on the focus edge, one millisecond after
the sweep — never on the assistant edge. This is NOT the `reclaimFocus()` fault (§6 A-track / `12`
Phase 8), which covers the *permanent* `AUDIOFOCUS_LOSS` hang.

**Fix:** `ASSISTANT.idleMs` is now `ASSISTANT_HOLD_MS` + one sweep period, so the focus hold ends just
after the edge that says Siri is finished — closing the gap to ~1 s. `keepAlive` is bounded at
`ASSISTANT_HOLD_MS` for the same reason: past that edge MUSIC outranks `VOICE_COMMAND` in `CarVolume`'s
priority list, so silence-filling cannot win the knob and only keeps a would-be-idle player active. Cost
is a codec rebuild if Siri thinks for longer than the window — ~130 ms of added latency that drops
nothing, since `configure()` runs synchronously ahead of the feed for the same AU.

**Rejected alternative, recorded so it is not re-proposed:** decoupling focus lifetime from sink lifetime
(abandon focus on the assistant edge, keep the track). An adversarial review showed it needs
pause-before-abandon to honour the device-derived rule at `Sink.release`, plus a `focusState` reset on
re-request or `keepAlive` dies permanently for that sink, and it locks in both `Sink` and `AacPlayer` —
for about two seconds more than two constants buy. Measure before spending that.

The same review found a race these constants ACTIVATE, closed in the same commit: `applyPauseState` is
check-then-act across two threads (assistant edge on `cp-voice-sweep`, focus callback on the main
looper), and the fix moves those edges from ~12 s apart to ~1 s. `AacPlayer` is now `@Synchronized` on
`setAssistantSpeaking` with the three `pausedForFocus` writes under the same monitor.

---

## 4. Microphone uplink — WORKS on the truck (owner-confirmed 2026-09-04)

**Truck result (2026-09-04):** the mic uplink works end to end. CarPlay raises the AAOS mic-in-use
indicator when it requests the mic and it clears cleanly on release; there is no GM AAOS mic volume/gain
control (none is expected).


`MicUplink.kt` (204 lines) connects eagerly to `127.0.0.1:9112` (the same socket carries the capture
gate and the outbound PCM — a data-triggered connect would deadlock). Gates on the control line, never
on downlink activity: the receiver writes `uplink on <rate> <ch>\n` when iOS SETUPs type 100 with
`input=true` and `uplink off\n` at teardown — Siri wants the mic before any downlink audio arrives, so
an activity-based gate would clip the onset.

- **Send**: `mic <len>\n` + `<len>` bytes S16LE PCM at the gated rate/channels. The Rust side does RTP
  framing, encryption with the stream input key, and the byte-order swap.
- **Capture**: `AudioRecord(VOICE_COMMUNICATION, 16000, MONO, PCM_16BIT, minBufferSize × 3)`, 20 ms
  chunks, dedicated thread at `URGENT_AUDIO`.
- **Manifest**: `RECORD_AUDIO` + `FOREGROUND_SERVICE_MICROPHONE`, granted at install (`-g`).
- **`ERROR_DEAD_OBJECT` is recoverable** — recreate in place, cap retries, guard against a recreate
  completing after `stop()` timed out (otherwise holds the Intel SST HAL input stream open forever).

### Build — DONE (2026-08-12)

`mic-uplink-eld` is enabled by default (`native/carplay-jni/Cargo.toml`, opt-out for the separate arm64
Pi build) and libfdk-aac 2.0.3 is cross-built for `x86_64-linux-android`. Verified by the same string
test that previously proved it absent: the shipped `.so` contains the enabled arm's `ELD encoder open
failed` string and no longer contains the `` `mic-uplink-eld` not built `` bail-out.

Recipe, for when it has to be rebuilt:

```bash
NDK=~/Library/Android/sdk/ndk/30.0.15729638/toolchains/llvm/prebuilt/darwin-x86_64/bin
cd ccpa_custom/scratchpad/fdk && tar xzf fdk-aac-2.0.3.tar.gz -C src-android --strip-components=1
cd src-android && ./configure --host=x86_64-linux-android \
  --prefix=../install-android-x86_64 --disable-shared --enable-static --with-pic \
  CC="$NDK/x86_64-linux-android32-clang" CXX="$NDK/x86_64-linux-android32-clang++" \
  AR="$NDK/llvm-ar" RANLIB="$NDK/llvm-ranlib" \
  CFLAGS="-O2 -fPIC -I../stub" CXXFLAGS="-O2 -fPIC -I../stub"
make -j8 && make install
```

**The one trap:** fdk-aac 2.0.3 includes AOSP's `<log/log.h>` under `__ANDROID__`, purely to call
`android_errorWriteLog()` from two CVE-hardening bounds checks in `libSBRdec/src/lpp_tran.cpp`. That
header ships with the platform tree, not the NDK, so a plain NDK build fails there.
`scratchpad/fdk/stub/log/log.h` is a no-op stand-in — the bounds checks still run and clamp; only the
platform telemetry call is neutered, which is correct for a third-party app.

`build_apk.sh` exports `FDK_AAC_PREFIX` and fails fast if the archive is missing. Note the separate `06`
§5d trap still applies: build `receiver` from its own directory, or workspace feature unification drags
`eld-codec` in elsewhere.

---

## 5. Advertise-vs-implement

`/info` advertises **8 audio formats and 4 microphone input formats**. Backing:

- **Output formats**: an unbacked output format produces silence, not teardown — `audioFormats` is a
  capability array iOS SETUPs against, and this app answers with a real bound `dataPort` regardless, so
  the negotiation stays well-formed (`05` §8 rule 9). Both observed `-16720` kills in this project
  (`cornerMasks`, `hevc`) were SETUP `enabledFeatures` tokens, not `audioFormats` entries.
- **Input formats**: of the four advertised `audioInputFormats`, all four can now arm — entry 1
  (`compatibility` PCM 16 kHz mono) and the three AAC-ELD entries, now that `mic-uplink-eld` is built
  (§4). Before that build, only entry 1 could arm; that historical asymmetry no longer applies. None of
  the four has been exercised end to end on the truck.
- **`mainBuffered` is OUT OF SCOPE for this project, deliberately** — still true, not built. It buys
  resilience to a WiFi hiccup on media (receiver-side buffer filled faster than realtime), genuinely
  valuable for 5 GHz to a moving vehicle, but implementing it means a buffered-stream handler plus a
  `FLUSHBUFFERED` verb. Until it exists it must not be advertised: if iOS moves media to a buffered
  stream and this app omits `mainBuffered`, **media goes silent**. Every stock Apple Simulator YAML
  template sets `enablesMainBufferedAudio: true`, so it can be armed by accident — check before copying
  a template `/info`.
- **Device-verified formats**: `pcm_16k_mono`, `pcm_48k_stereo`, `aac_lc_48k_stereo` (media),
  `aac_eld_16k_mono` (Siri media-pause path only, not Siri audio content itself). Everything else in
  `/info` is now backed by code but not yet proven on hardware.

### PCM entries (still advertised, unresolved recommendation)

**CORRECTED 2026-08-31 — the previous claim that `compatibility`/PCM is wired-only was wrong about
what the box ADVERTISES.** `preset_wireless_8()` carries two `compatibility` entries unconditionally
(`ccpa_custom crates/vendor/receiver/src/info.rs:1008-1035`): type 100 PCM 16k-mono/48k-stereo and
type 101 PCM 48k-stereo. There is no transport branch on that list, so they are offered on every
wireless session.

What remains true is that iOS has never been observed to SELECT one. Across 24 SETUP negotiations in
five captured sessions (2026-08-05, 08-12 ×2, 08-18 ×2): `audioType=media` ×18, `audioType=default`
×6, `compatibility` ×0. Media rides type 102 / `audioType=media` / AAC-LC 48k stereo, which the box
routes to `:9002` as atype 0 — it never reaches VoiceRouter.

**The conditional failure this creates.** If iOS ever declines the type-102 AAC-LC stream and falls
back to a `compatibility` PCM stream for media, that arrives on `:9003` as atype 5. This receiver has
no PCM media sink, so `VoiceRouter.purposeFor` returns null and the audio is dropped: media silent,
calls and Siri still audible. That presentation is identical to a stuck `AacPlayer` audio-focus hold,
and the two are distinguishable only by the log line — which is why the atype-5 drop now logs at
error level naming the consequence (`VoiceRouter.kt`, `ATYPE_COMPATIBILITY`).

Unresolved: no capture exists for the 2026-08-28 session where media was reported silent while Siri
worked, so neither this path nor the audio-focus path is confirmed for it.

**Two greps settle it on the next session, and both halves are unconditional — no verbose flag.**

| Half | Where | Source |
|---|---|---|
| Which sink each stream landed on, app side | logcat | the `atype 5 … DROPPED` error above, `VoiceRouter.kt` |
| The negotiated `audioType` per stream, box side | airplayd stderr → `[box:…]` in logcat | `[session] SETUP phase2 audio({ty}) … audioType={..} -> dataPort {..}` |
| The sink the box chose per stream | airplayd stderr | `[audio] stream {ty} {codec} -> {label} :{port}` |

Box-side lines are bare `eprintln!` in `ccpa_custom crates/vendor/receiver/src/session.rs` (the
phase-2 line ~:889) and `spawn_audio`; nothing gates them, and they reach this app's logcat through
the `CH_FILE` box-log stream. A fourth line dumps the full stream dict for every audio-range type
(100..=112) but is marked for removal once uplink negotiation is confirmed — do not build a
procedure on it.

Reading: a type-102 / `audioType=media` SETUP with audio still failing points at the audio-focus
path. No 102 stream but a `compatibility` SETUP points at the atype-5 drop, and the fix would then be
to route 5 to a PCM media sink rather than dropping it.
link makes iOS find no usable PCM and borrow no MainAudio at all). They remain advertised because they
are part of `preset_wireless_8`, the device-proven-as-a-whole preset, and `audioFormats` is a
reconnect-only key in `/info` — not a free edit.

> **Do not remove entry 1 without re-verifying the mic path first.** It was the *only* input format
> that could arm before `mic-uplink-eld` was built (§4); now that AAC-ELD mic encode is compiled, this
> constraint may have relaxed, but no truck test has confirmed the AAC-ELD mic path actually works
> end to end. Until that test happens, treat entry 1 as load-bearing.

**Still open**: entry 2 (type 101 `compatibility`, output-only PCM) carries no such mic obligation and
can be removed in its own change with its own truck test, independent of everything else in this doc —
a regression there would present as "no audio at all," the same signature as an audio-path regression
elsewhere, so bundle nothing else with it.

### `cornerMasks` — cited here only as a failure-mode example

An Apple design affordance for rounded/irregular display cutouts, irrelevant here (fullscreen video on
a rectangular panel). Not advertised (`levers::set_cornermasks(false)`). Mentioned only because it is
one of the two historical `-16720` teardowns and the evidence for which advert class can kill a session
(`enabledFeatures` tokens, not `audioFormats` entries).

---

## 6. Build order — status

| # | Step | Gate | Status |
|---|---|---|---|
| **A1** | `atype` in `tag_voice` (`ccpa_custom`, box-side) | Byte arrives on `:9003`, matches SETUP's `audioType` | **Landed** |
| **A2** | Voice decoder: parse `:9003` tag, `MediaCodec audio/mp4a-latm` with the ELD `csd-0` | Siri audio audible on `bus2_voice_command_out` | **Truck-confirmed 2026-09-04** — Siri audible on `Voice`; media-pause side effect also proven (§3) |
| **A3** | Per-usage sinks + one focus listener each + silence-fill + watchdog | Call audio on `bus4_call_out`; volume keys follow the active purpose | **Truck-confirmed 2026-09-04** for CALL and NAV routing; the ALERT sink is still unexercised |
| **A4** | Ducking (energy-gated, 0.2) | A nav prompt ducks music and music returns | **Built**; nav audio routes correctly (2026-09-04) but the duck-and-return interplay is not separately truck-verified |
| **A5** | Mic: `mic-uplink-eld` feature, then `AudioRecord` + `:9112` client | Siri hears speech; `/info` input formats become honest | **Truck-confirmed 2026-09-04** — works; AAOS mic-in-use indicator raises on request and clears cleanly; no GM mic gain control (expected) |

**Remaining verification** (construction is done): the **alert** sink, **mic-captured speech**, and the
duck-and-return interplay — each independently testable per §0a and not to be bundled into one changeset
if it fails. Media, Siri, call and nav routing are owner-confirmed on the truck (2026-09-04).

---

## Sources

`carlink_native_personal/app/src/main/kotlin/com/carlink/audio/` (DualStreamAudioManager,
AudioRingBuffer, MicrophoneCaptureManager) ·
`ccpa_custom/crates/vendor/receiver/src/{session,forward,uplink,info,stream}.rs` ·
`ccpa_custom/host/CarPlayHost/carlink_macOS/Audio/` · `ccpa_custom/docs/carplay/06_AV_PIPELINE.md` (audio formats), `docs/50`
(ELD ASC) · this app's `av/{AacPlayer,VoiceRouter,MicUplink,CarPlayActivity}.kt`, `assets/info.bplist`.
