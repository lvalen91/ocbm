# 13 — Audio routing (LIVING DOCUMENT — update in place)

**What this is.** Full CarPlay audio: media, phone calls, Siri, alerts, navigation and the microphone
uplink. Derived from four parallel studies — the working Android stack in `carlink_native_personal`,
the `ccpa_custom` receiver, its macOS host, and an audit of what this app does versus what it
advertises. See [`12_OBSERVED_FLOW.md`](12_OBSERVED_FLOW.md) for the flow this plugs into.

**Status: BUILT, compiles clean, Tier-0 green. Five of the six sinks are truck-confirmed.** All
five build-order steps below (A1–A5) landed in commits `525a845`/`ee1f270`/`5515bf4`/`a65a1f0`
(2026-08-12) plus the 2026-08-28/29 focus-recovery fixes. **Owner-confirmed on the truck 2026-09-04**
(no capture committed): media, Siri, call and navigation audio each route audibly to the correct GM
vehicle volume group — Media→`Audio`, Siri→`Voice`, Call→`Phone`/`Call`, Nav→`Navigation` — and the
**mic uplink works** (§4). Still unconfirmed on hardware: only the **alert** sink. The post-Siri
media-silence bug reported on 2026-09-04 is **fixed** in `b8e8736` (2026-09-08) and the fix is now
**measured on the truck at 1.95 s** — see §3a.

**Captured session 2026-09-09 (`gmccpa_probe_20260909_225421`, app build `7c92000`) — the first
capture that exercises ducking, and it found three defects.** Duck-and-return is no longer "unexercised":
it runs, and it PUMPS (§3 Ducking). `VoiceRouter` dropped **42 of 249 Siri AUs (17%)** (§3 rule 3).
Media took **6 underruns in 843 frames** (§3 rule 2). All four are filed as rows A6–A9 in
[`11_HARDENING_PLAN.md`](11_HARDENING_PLAN.md).

---

## 0a. What is truck-verified, and what is not

| Claim | State |
|---|---|
| Media (AAC-LC 48 k stereo, `AacPlayer`) | **Device-proven**, 2026-08-05 working session |
| Media pausing so the volume knob reaches `VOICE_COMMAND` during Siri | **Device-proven**, 2026-08-12 (117 knob steps measured; see §3) |
| `atype` byte on `:9003` (box-side `tag_voice`) | Landed in `ccpa_custom` `forward.rs`; parsed by `VoiceRouter`. **Routing device-confirmed 2026-09-04**: Siri, call and nav each land audibly on the correct bus, so the `atype`→purpose split is right on hardware |
| Per-purpose sinks: CALL and NAV routing, AAC-ELD decode, silence-fill watchdog (`VoiceRouter.kt`) | **Owner-confirmed on the truck 2026-09-04** (no capture committed): Siri→`Voice`, Call→`Phone`/`Call`, Nav→`Navigation` each audible on the correct volume group. The **ALERT** sink is still unexercised. The energy-gated **ducking** IS now exercised (2026-09-09 capture) and is defective — see §3 Ducking |
| AAC-ELD decode of the Siri downlink | **Device-proven 2026-09-09**: `[voice] siri: AAC-ELD 16000Hz 1ch -> AudioTrack(usage=16), decoder=c2.android.aac.decoder`. Note the decoder is SOFTWARE — this head unit has no hardware audio codec of any kind (§5) |
| `VoiceRouter` AU delivery | **DEFECTIVE, measured 2026-09-09**: `stopping — 249 AUs routed, 42 dropped` — 17 % of one Siri turn discarded by the `w == 0` branch in `Sink.feed`. Filed as A6 |
| Mic uplink (`MicUplink.kt`, `mic-uplink-eld` feature) | **Owner-confirmed on the truck 2026-09-04**: works. CarPlay raises the AAOS mic-in-use indicator when it requests the mic and it clears cleanly on release; no GM AAOS mic volume/gain control exists (expected) |
| `AacPlayer.reclaimFocus()` (recovers from `AUDIOFOCUS_LOSS` without a permanent hang) | Landed 2026-08-28, compiles — **not truck-verified** |
| Media recovery after a voice/call turn | **FIXED 2026-09-08 (`b8e8736`); truck re-measurement RECORDED 2026-09-09 at 1.95 s.** Owner-observed 2026-09-04 as 10–20 s of silence; device-measured at **12.0 s** before the fix, **1.95 s** after (assistant edge `22:55:50.348` → `media resumed after focus gain` `22:55:52.295`). Two clocks: `assistantTick` cleared `pausedForAssistant` after `ASSISTANT_HOLD_MS` (4 s), but focus was abandoned only by `Sink.release()` at `Purpose.idleMs` (15 s for ASSISTANT), so media resumed on the focus edge, not the assistant edge. `ASSISTANT.idleMs` is now `ASSISTANT_HOLD_MS + 1` sweep period. The design target was ~1 s; the measured 1.95 s is the sweep's 1 Hz throttle on top of it, and is accepted — see §3a |

**Source, current as of this doc:** `netprobe_app/app/src/main/java/zeno/gmccpa/av/{AacPlayer,
VoiceRouter, MicUplink}.kt`, wired into `CarPlayActivity.kt`. `VoiceRouter` (656 lines as of
2026-09-09, was 597) replaces the
earlier `drainVoice`, which accepted `:9003` and discarded every byte — that silence is why this
section previously read "design only, nothing implemented"; it no longer applies.

---

## 0. The wire format this depends on

`:9003` carries every non-media stream, tagged:

```rust
// ccpa_custom/crates/vendor/receiver/src/forward.rs — pub fn tag_voice
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
| `bus4_call_out` | call | atype 3 `alert` | `USAGE_VOICE_COMMUNICATION_SIGNALLING` + `SONIFICATION` | `ALERT` — same bus as CALL, see below |
| `bus4_call_out` | call | atype 1 `telephony` | `USAGE_VOICE_COMMUNICATION` + `SPEECH` | `CALL` |

**You never address a bus directly.** GM's `CarAudioService` maps `usage → context → volume group →
bus`; picking the right usage is the whole mechanism. `carlink_native_personal` confirms this and
contains no `CarAudioManager`, no zone API, no bus address anywhere.

**`bus0_media_out` is a shared HAL mix bus, not a switch.** From the `<route type="mix"
sink="bus0_media_out">` entry in `audio_policy_configuration.xml`:
`<route type="mix" sink="bus0_media_out" sources="mixport_bus0_media_out,bus3_sxm_in,bus4_lvm_in,
bus8_tuner_in,bus10_tuner_am_in,bus13_dab_in,bus14_rsi_in"/>` — GM FM/AM/SXM/DAB (all
`isExternalBus=1` / `AUDIO_DEVICE_IN_BUS role="source"`) is injected at the DSP and never passes
through AudioFocus or an `AudioTrack`. `type="mix"` means the hardware **sums** these sources rather
than switching between them, so an app that keeps writing PCM into `bus0_media_out` during a
focus-loss window produces genuine double-talk over live radio, not a silent no-op — this is the
mechanism behind the device-observed `Use hal ducking signals true`. `AacPlayer.reclaimFocus()` has
not been tested against a live FM session; needs vehicle time.

### What the HAL will actually accept — read from the firmware, 2026-09-09

Extracted from the unit's own `/vendor/etc/audio_policy_configuration.xml` (cleanroom image of
`W231E-Y181.3.2`). These bound every buffer-sizing and format decision in this document, and two of
them were previously assumed rather than known:

- **Every output bus mixPort declares exactly one profile: `AUDIO_FORMAT_PCM_16_BIT / 48000 /
  AUDIO_CHANNEL_OUT_STEREO`.** That includes `mixport_bus2_voice_command_out`, which is where the
  16 kHz mono Siri track lands. AudioFlinger therefore resamples and up-mixes every voice AU; the
  HAL never sees 16 kHz. `getMinBufferSize(16000, MONO)` in `VoiceRouter.Sink.configure` is sizing
  against a rate this hardware does not serve. Tracked as A9.
- **`mixport_bus0_media_out` carries `flags="AUDIO_OUTPUT_FLAG_PRIMARY"` and nothing else.** There is
  **no DEEP_BUFFER mixPort** on this unit, so media has no deep-buffer path to fall back on and the
  `AudioTrack` buffer is the entire cushion. Measured cost: 6 underruns in 843 frames. Tracked as A8.
- **There is no `AUDIO_OUTPUT_FLAG_COMPRESS_OFFLOAD` anywhere in the file.** The only output flags
  present in the whole configuration are `PRIMARY` and `DIRECT|VOIP_RX` (the latter on the single
  `"direct output"` mixPort, which does accept 8/16/24/44.1/48 kHz mono or stereo). A compressed
  bitstream can never reach the DSP; combined with §5's finding that every audio codec is software,
  every CarPlay sample on this unit is CPU-decoded and CPU-resampled with nothing to offload to.
- **Three buses exist that this document never listed, and none of them is an app lever.**
  `bus11_mix_unduck_out`, `bus12_audio_cue_out` and `bus13_high_priority_mutex_out` appear in
  `audio_routing_configuration.xml` wired to `pcmUnduck_p` / `pcmCue_p` / `pcmHPMutex_p`, but they
  have **no volume group in `car_audio_configuration.xml` and no usage in
  `audio_policy_engine_product_strategies.xml`** — they are DSP mix inputs GM drives internally and
  are unreachable through `AudioAttributes`. Recorded so the name `mix_unduck` is not mistaken for a
  way out of the ducking problem.

> **ALERT is not distinguishable from CALL on this build — confirmed, not a hypothesis (2026-09-09).**
> `audio_policy_engine_product_strategies.xml`'s `voice_call` product strategy puts
> `AUDIO_USAGE_VOICE_COMMUNICATION` and `AUDIO_USAGE_VOICE_COMMUNICATION_SIGNALLING` in the SAME
> `<AttributesGroup volumeGroup="phone">`, and the decompiled `CarAudioContext.CONTEXT_TO_USAGES` in
> `CarService.apk` settles it at the context/bus level: context 5 (`CALL` → `bus4_call_out`) carries
> usages `{VOICE_COMMUNICATION, VOICE_COMMUNICATION_SIGNALLING}`; context 4 (`CALL_RING` →
> `bus3_call_ring_out`) carries only `{NOTIFICATION_RINGTONE}`. So ALERT (`USAGE_VOICE_COMMUNICATION_
> SIGNALLING`) lands on `bus4_call_out`, the same bus, context and volume group as CALL — it does not
> reach `bus3_call_ring_out` at all. The earlier claim that GM remaps `USAGE_NOTIFICATION_RINGTONE` to
> `BUS_NOTIFICATION` is also wrong on this table: usage `NOTIFICATION_RINGTONE` maps straight to
> `CALL_RING`/`bus3_call_ring_out`, matching AOSP. Nothing in this build routes CarPlay's `alert`
> purpose to `bus3_call_ring_out`.

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
| ALERT | `:9003` atype 3 → `VoiceRouter` | AAC-ELD 48 k stereo | Built, untested on truck — and there is now a **mechanical reason to expect it is broken**, see below |
| NAV | `:9003` atype 4 @ 48 k stereo → `VoiceRouter` | AAC-ELD 48 k stereo | **Owner-confirmed on truck** (2026-09-04) — audible on `Navigation` |

> ### ⚠ The ALERT sink may be structurally inaudible on this unit — predicted 2026-09-10
> AudioFlinger will not mix a fresh `MODE_STREAM` track until its buffer has been filled once
> (`Track::isReady` compares `framesReady` against `bufferSizeInFrames`). Every voice track is
> floored at `TRACK_BUFFER_MIN_MS` = 400 ms, and `Purpose.ALERT` is by design *a short burst* with
> `keepAlive = false` — so **an alert shorter than 400 ms can never satisfy that gate and produces
> no sound at all**, rather than merely arriving late. That is a candidate explanation for why this
> row has read "built, untested" since it was written: it may have been tested and silently done
> nothing.
>
> Instrumented rather than assumed: `av/voice-first-out/alert` (N24) arms on `Sink.configure` and is
> met at the first `playbackHeadPosition > 0`. If it fires on the first truck run that plays an
> alert, that is a **true positive**, not a false alarm, and the fix is A10's — lower
> `KEEPALIVE_AFTER_MS` and the floor together, or give ALERT its own smaller floor since it is the
> one purpose that does not silence-fill.

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

Every rule below is a device-proven GM behaviour, intended to be applied in both `VoiceRouter.Sink`
and `AacPlayer`. **CORRECTED 2026-09-09: rules 2, 3 and 7 were audited against the actual code.
Rules 2 and 3 diverge between the two sinks (rule 3 is still an open code defect). Rule 7 was
`VoiceRouter.Sink`-only when audited and was fixed in `AacPlayer` the same day. See each rule.**

1. **48 kHz everywhere, no `PERFORMANCE_MODE_LOW_LATENCY`.** `AUDIO_OUTPUT_FLAG_FAST` is denied to
   third-party apps on this unit (`createTrack_l(8): AUDIO_OUTPUT_FLAG_FAST denied by server`), so low
   latency mode buys nothing and can add jitter. Not AOSP-documented.
2. **Buffer sizing differs per caller.** `VoiceRouter.Sink.configure()` uses
   `maxOf(minBuf * 4, <bytes for TRACK_BUFFER_MIN_MS at the negotiated rate>)`, where
   `TRACK_BUFFER_MIN_MS = 2 * KEEPALIVE_PERIOD_MS` = 400 ms — so `getMinBufferSize() × 4` is only a
   FLOOR under a rate-derived 400 ms minimum (the code's own comment records that the old rate-blind
   `maxOf(minBuf, 4096) * 4` was too small at 16 kHz mono). `AacPlayer.buildTrack()` uses
   `maxOf(getMinBufferSize(), 8192) × 2` — half the multiple, floored at 8192 bytes, no time-derived
   minimum. Neither path "prefills" PCM before calling `play()`; both start the track playing as soon
   as it is built and let the codec feed it from empty.
   **MEASURED 2026-09-09: the media sizing is too small.** `[aac ] stopping — 843 frames played,
   0 dropped (no track), 6 underruns`. There is no DEEP_BUFFER mixPort on this unit (§1), so that
   buffer is the only cushion media has. `maxOf(minBuf, 8192) * 2` is 85–160 ms at 48 k stereo,
   **smaller than the phone's own ~200 ms delivery pause** — and because AudioFlinger will not mix a
   fresh `MODE_STREAM` track until its buffer has filled once, that depth IS the initial lead, which
   is why 5 of the 6 underruns fall in the first 9 s. Tracked as A8, **implemented at
   750 ms 2026-09-10** (`AacPlayer.TRACK_BUFFER_MS`). Two expected side effects a future capture
   reader must NOT chase as faults: ~750 ms to first sound per stream start (AudioFlinger's
   `Track::isReady` needs `framesReady >= bufferSizeInFrames`, and `addTrack_l` re-arms FILLING on
   every re-add, including a `play()` after a `pause()`), and one benign
   `AudioFlinger: BUFFER TIMEOUT` + `AudioTrack: restartIfDisabled` pair per stream start, because
   this unit's fill grace measures ~450 ms and the restart resumes the fill with contents intact.
   Also measured while sizing it: **the phone starts a media stream at roughly realtime with no
   pre-roll burst**, so the lead over the DAC at stream start equals the track buffer depth exactly
   and the only thing that grows it afterwards is an audible underrun — which is why
   `getMinBufferSize` multiples can never be the right rule here.
   **Read the "843 frames played" figure with care — it was inflated.** `AudioTrack.write` in
   blocking mode becomes NON-blocking on a paused track and returns a short count; `AacPlayer.feed`
   tested only `w < 0`, so a short write still incremented `framesDecoded`. The 500-frame checkpoint
   printed at 22:55:49.252 *while media was paused for the assistant*. **Sized precisely 2026-09-10
   from the `[rust]` stream lines: ~240 frames (~5 s), not the ~170 first estimated** — iOS ended
   stream 102 at 22:55:39.596, 17 ms after the focus pause, and started a new one at 47.179, 5.1 s
   before the track resumed at 52.295. Counter fixed 2026-09-10; a session now prints
   `N frames played, D discarded (paused), …` so the two can never be conflated again.

   **Correction to the threading model this rule and `AacPlayer.stop()`'s KDoc both assumed.** Both
   said the consume thread "parks in the blocking `write()`" during a hold. It does not: native
   `AudioTrack::obtainBuffer` forces non-blocking whenever `mState != STATE_ACTIVE`, so `feed` keeps
   being re-entered at the phone's delivery rate and the PCM is *discarded*, not queued. Only the
   single in-flight write at the moment of `pause()` is ever interrupted. Consequence worth keeping,
   because it inverts an earlier note: `AacPlayer.reclaimFocus()` **does** retry on its
   `FOCUS_RETRY_MS` cadence while the phone is still delivering. The case where it gets no attempts
   is when the phone *ends the stream* — which is exactly what it does for a Siri turn — and the
   thread parks in the seam `read()` instead.
3. **`WRITE_NON_BLOCKING`, but the two callers diverge on retry, and neither retries a residual
   write.** `VoiceRouter.Sink.feed()` writes with `WRITE_NON_BLOCKING`; the
   `if (w == 0) { ausDropped.incrementAndGet(); break }` branch in its write loop counts the AU as
   dropped and moves on — it does **not** retry next pass. (The neighbouring `w < 0` branch is a
   different thing: the `ERROR_DEAD_OBJECT` track rebuild.) `AacPlayer.feed()`'s media write does not
   pass `WRITE_NON_BLOCKING` at all; it is a deliberate **blocking** write, per the code's own KDoc in
   `AacPlayer.kt` (search "parked in the blocking `write()`")
   that a blocking write is required there so `pause()` (not a socket close) is what unblocks a
   consume thread parked in it. **Open code defect (2026-09-09):** the documented intent — a short or
   zero-length write retried on the next pass rather than silently dropped — is not what either sink
   does today; `VoiceRouter.Sink` drops, and `AacPlayer` never uses non-blocking writes in the first
   place. Needs a follow-up code fix, not just a doc correction.
   **MEASURED 2026-09-09: `[voice] stopping — 249 AUs routed, 42 dropped` — 17 % of one Siri turn.
   But "retry on the next pass" was the WRONG remedy, and this rule's stated intent is hereby
   retired (2026-09-10).** The `w == 0` branch is the symptom; the cause is rule 5's keep-alive gate
   double-feeding the track (see rule 5). At 2× realtime **any** buffer fills, a retry queue grows
   for the whole gap and then drains late, and a blocking write parks the consume thread ~50 % of
   the time — so all three of the obvious fixes treat the symptom and two make it worse. Fix the
   gate; after that a **blocking** write is the correct residual, because with input rate == drain
   rate it parks at most one HAL period (~20 ms), and a `w == 0` then genuinely means "the track is
   paused by focus loss", where discarding is right. Tracked as A6.
4. **One `OnAudioFocusChangeListener` instance per usage** (`VoiceRouter.Sink.focusListener`) — AAOS
   `CarAudioFocus` keys on listener identity, so a shared listener cannot hold focus for two usages.
5. **Silence-fill, not pause, for continuous streams** (`Sink.keepAlive()`). AAOS picks the volume
   group from *active players*; a track that stops playing surrenders the volume group and hardware
   volume keys jump elsewhere. Paced on an absolute clock (`nextSilenceAt`, floored) — relative pacing
   accumulates loop lateness into guaranteed underruns. `ALERT` is a short burst and does pause.
   **DEFECT, root cause of A6, found 2026-09-10: the gate is on the wrong clock.** `keepAlive` tests
   `now - lastAudioAt`, and `feed` advances `lastAudioAt` **only inside the `peakExceeds` branch** —
   so it means "last LOUD frame", not "last AU". iOS does not stop sending during a Siri pause; it
   streams **silent AUs at realtime** right through it (proved by arithmetic: 249 AUs × 30 ms =
   7.47 s ≈ the sink's whole 7.22 s life). So for every gap between `KEEPALIVE_AFTER_MS` and
   `ASSISTANT_HOLD_MS` the sweeper adds 200 ms of silence per 200 ms on top of the stream's own
   realtime feed — 2× into a 400–640 ms buffer — and the track is full within half a second.
   **Two clocks are needed:** a new `lastAuAt` (last *delivered* AU) to gate `keepAlive`, and
   `lastAudioAt` (last *loud* frame) kept for duck, idle and the assistant edge, all of which must
   go on ignoring iOS's streamed silence. Note `Purpose.idleMs` is on the loud clock too, which is
   fine for ASSISTANT but suspect for **CALL**: 3 s of far-end silence mid-call would release the
   sink from `cp-voice-sweep` while `feed` decodes on the consume thread. Not yet adjudicated.
   **Also: `keepAlive` and `feed` write to the same `AudioTrack` from two threads with no lock**
   (`cp-voice-sweep` and the consume thread), and they overlap in exactly this window.
   `AudioTrack.write` is not safe for concurrent callers. Needs a `writeLock`.
6. **Idle watchdog** (`sweepIdle()`, 1 Hz, per-purpose `idleMs`) — a sink with no audio past its window
   releases: track/codec/focus torn down, volume group freed. This exists because a dropped stop
   message previously wedged `USAGE_VOICE_COMMUNICATION` as the active volume group for a whole
   session.
7. **`pause()`/release before `abandonAudioFocusRequest()`** at sink teardown, so AAOS sees no active
   player of that usage at abandon time. `VoiceRouter.Sink.release()` follows this order (pause → flush
   → stop → release, then abandon). **`AacPlayer.stop()` now follows it too: `running=false` → `pause()` → `flush()` → (release the primed track if the
   seam never connected) → `abandonFocus()` last. CORRECTED 2026-09-09, fixed in source the same day:
   the audit that morning found `abandonFocus()` called BEFORE `pause()`/`flush()`, so for the window
   before `pause()` landed the app was still writing PCM at full gain while AAOS had already handed
   focus to the next owner.** With a live consumer the track's `stop()`/`release()` still happen later
   on the consume thread (that is deliberate — see the KDoc on `stop()`); by abandon time the track is
   paused and flushed, which is what AAOS's active-player check sees. **Not truck-verified** — a
   green build proves compilation only.

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

### Ducking (`AacPlayer.setVoiceDucked` / `setFocusDucked`, `VoiceRouter` energy gate)

**CORRECTED 2026-09-09 (twice). First correction, that morning: the `effective = mediaVolume ×
min(commandedDuck, focusDuck)` formula this section used to state did not exist in code — `setDucked`
was one shared `duckGain` boolean written by two callers, last writer wins. Second correction, same
day, after the fix landed: the min() arbitration is now implemented, with a different shape than the
old formula described.** Duck **only** the media track. What actually runs:

- Two INDEPENDENT duck sources, each with its own entry point so a caller cannot touch the other's
  (`AacPlayer.setVoiceDucked` / `AacPlayer.setFocusDucked`; stale line cite, re-anchored 2026-09-10): `setVoiceDucked(Boolean)` is `VoiceRouter`'s energy gate, wired via
  the `onDuck =` lambda in `CarPlayActivity`; `setFocusDucked(Boolean)` is `AacPlayer`'s own focus listener
  (`LOSS_TRANSIENT_CAN_DUCK` → true in `AacPlayer.focusListener`, `AUDIOFOCUS_GAIN` → false, and `LOSS` and a
  fresh focus grant also clear it, since no focus held means no focus-derived duck). Both funnel into
  one private `setDucked(source, ducked)` (`AacPlayer.setDucked`; stale line cite, re-anchored 2026-09-10) that keeps a flag per source and
  derives the effective gain as **0.2 if EITHER flag is set, 1.0 only when NEITHER is** — the min() of
  the two — under the single `duckLock`. `applyGain()` (`:330-332`) re-asserts that effective gain onto
  any track built mid-duck, under the same lock.
- **What was wrong before the fix:** with one shared boolean, an `AUDIOFOCUS_GAIN` landing while a nav
  prompt was still loud restored media to unity over the prompt, and the gate's un-duck at the end of a
  prompt cancelled a focus duck AAOS had not lifted. The `synchronized(duckLock)` block that already
  existed fixed a *different*, older race (a non-atomic read-compare-write that could leave
  `duckGain=1.0` with the hardware at 0.2 and the duck unrecoverable) — it never arbitrated between
  sources. Both the old race and the new arbitration now live behind the same lock, so the two flags
  and the gain derived from them change as one atomic state.
- Idempotence is per source (`VoiceRouter` re-asserts `onDuck(false)` at 1 Hz from its idle sweep);
  a source edge that does not change the effective gain because the other source still holds is logged
  as `media duck: <SOURCE> off, gain stays 0.2 (voice=… focus=…)` and not applied, so a capture shows
  which source is holding the duck.
- **The two-source arbitration is still not truck-verified** — it is in the working tree but was NOT
  in the `7c92000` build that produced the 2026-09-09 capture, so that session ran the old single
  shared boolean. Whether AAOS's `CAN_DUCK`/`GAIN` edges and the energy gate interleave the way the
  fix assumes remains unobserved.
- **What the 2026-09-09 capture DID prove: the energy gate pumps.** Inside one Siri turn the media
  gain went `0.2` (22:55:39.827) → `1.0` (42.208) → `0.2` (43.725) → `1.0` (48.237). `DUCK_RELEASE_MS`
  is 1500 ms and `sweepIdle` self-throttles to 1 Hz, so ~1.5–2.5 s of quiet un-ducks — and Siri's own
  pause between hearing the query and speaking the answer routinely exceeds that. This is independent
  of the arbitration fix and will still be there after it lands. Tracked as A7.
- **…and the pumping is INAUDIBLE, so do not spend a truck slot on it as such.** Media was paused
  from 22:55:39.579 (`LOSS_TRANSIENT`, before the Siri sink had even configured) to 52.295, and
  `setVolume` on a paused track produces no sound. The same holds for CALL, which also takes
  `GAIN_TRANSIENT` and pauses media. Treat A7's pumping as **log hygiene**.
- **The bug worth fixing lives in the same method, and it is an ordering one.** `sweepIdle` calls
  `release()` on dead sinks — which abandons focus, so AAOS posts `AUDIOFOCUS_GAIN` and `AacPlayer`
  calls `play()` — **before** it evaluates `if (!anyLoud) onDuck(false)`. Today the un-duck happens
  seconds earlier so the order never shows. With ASSISTANT's hold at 4 s against an `idleMs` of 5 s
  the two edges are one sweep apart, and sweeps run 1000–1100 ms apart, so a late sweep puts both in
  the same pass: focus abandoned, GAIN delivered, **media resumes at 0.2** until the sweeper reaches
  the un-duck microseconds later. Move `onDuck(false)` above the `dead.forEach { … release() }`.
- **The case that actually matters is NAV, not Siri.** NAV requests `GAIN_TRANSIENT_MAY_DUCK`, so if
  AAOS answers with `LOSS_TRANSIENT_CAN_DUCK` media is *ducked and still playing* — and then the
  focus duck masks the energy gate entirely (`gain stays 0.2 (voice=false focus=true)`) and holds
  media at 0.2 until the NAV sink releases at `idleMs` = **8 s**. If instead GM's `CarAudioFocus`
  treats NAV and MUSIC as concurrent and ducks in the HAL (`Use hal ducking signals true`), we get no
  callback at all and the software 0.2 stacks on top of the hardware duck. **Measure this**: time
  from a nav prompt's last loud frame to `media restored to 1.0`, and whether any
  `gain stays 0.2 (voice=false focus=true)` line appears.
- 0.2, not 0.8 — "duck by 20%" (≈2 dB) was reported by users as "does not duck".
- `LOSS_TRANSIENT`/`LOSS` do **not** duck to 0.0 through `setFocusDucked`. They go through a different
  mechanism entirely: the focus listener sets `pausedForFocus = true` and fully pauses the media track
  (the `LOSS_TRANSIENT` / `LOSS` arms of `AacPlayer.focusListener`; stale line cite, re-anchored 2026-09-10), the same pause path Siri uses, not a duck-to-zero gain.
- Expect to duck yourself through the focus round-trip: this app's own NAV request
  (`GAIN_TRANSIENT_MAY_DUCK`) and CALL/Siri requests (`GAIN_TRANSIENT`) come back to the MEDIA listener
  as a loss. That is the mechanism, not a bug.
- **Duck trigger is energy-gated, not packet-gated** (`VoiceRouter.peakExceeds`, int16 peak ≥ ~800,
  ≈ −32 dBFS) — iOS streams continuous digital silence on idle voice streams, so a flow-based trigger
  would duck media permanently from session start.

> ### ⚠ The energy gate should probably not exist at all — A11, filed 2026-09-10
> Everything in this section reconstructs, from audio samples and timers, state that **iOS is
> explicitly sending us and we are discarding**. In the 2026-09-09 capture the `modesChanged` plist
> on the encrypted control channel brackets the Siri turn perfectly: 6 of 6 messages at one payload
> size fall inside it, 14 of 14 at the other fall outside, it leads the mic gate by 317 ms, and it
> announces the end **3.27 s before** `assistantTick`'s inference and **5.2 s before** media actually
> resumed. The payload already reaches Kotlin — `:9004` → `MetadataSeam` → `CarPlayActivity.onCommandPlist`
> — which drops it because it only implements `requestViewArea`. Its own KDoc names `modesChanged`
> and `duckAudio` as ignored. Two further explicit signals are also arriving unused: `0x4155`
> CallStateUpdate on the iAP2 tunnel, and NowPlaying `playbackStatus`, which `NowPlayingState`
> already parses for the media card while `AacPlayer` infers play/pause from seam traffic.
>
> If A11 lands, the assistant edge, the duck trigger and most of the per-purpose idle windows become
> control-plane driven and the energy gate degrades to a fallback for a phone that does not send the
> mode. **A6's buffer/lock/write fixes and A8's sizing fixes are orthogonal and still needed** — they
> are about how PCM reaches the HAL, not about when a turn starts. A7 and A10's *triggers* are
> superseded. Decode one plist of each size before building on this; the bracket is a size
> correlation, not yet a parse.

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
  `aac_eld_16k_mono` — the last one upgraded 2026-09-09 from "Siri media-pause path only" to real
  decoded Siri audio content (`[voice] siri: AAC-ELD 16000Hz 1ch -> AudioTrack(usage=16)`).
  Everything else in `/info` is now backed by code but not yet proven on hardware.
- **Every one of them decodes in SOFTWARE.** This head unit ships no hardware audio codec at all —
  see [`01_FINDINGS.md`](01_FINDINGS.md) §5, corrected 2026-09-09. Both media and Siri were observed
  on `c2.android.aac.decoder`. Budget CPU accordingly: there is no offload path (§1) and no
  hardware fallback to switch to.

### PCM entries (still advertised, unresolved recommendation)

**CORRECTED 2026-08-31 — the previous claim that `compatibility`/PCM is wired-only was wrong about
what the box ADVERTISES.** `preset_wireless_8()` carries two `compatibility` entries unconditionally
(`pub fn preset_wireless_8`, `ccpa_custom crates/vendor/receiver/src/info.rs`; was at :1008-1035, then
:1404-1430 — re-anchored to the symbol 2026-09-10): type 100 PCM 16k-mono/48k-stereo and
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
phase-2 line :935, not ~:889 — checked 2026-09-09; :886 is the unrelated ALT-screen(111) line) and
`spawn_audio`; nothing gates them, and they reach this app's logcat through
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
`ccpa_custom/host/MacHost/carlink_macOS/Audio/` · `ccpa_custom/docs/carplay/06_AV_PIPELINE.md` (audio formats), `docs/50`
(ELD ASC) · this app's `av/{AacPlayer,VoiceRouter,MicUplink,CarPlayActivity}.kt`, `assets/info.bplist`.
