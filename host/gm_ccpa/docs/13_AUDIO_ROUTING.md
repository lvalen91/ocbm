# 13 — Audio routing

Full CarPlay audio: media, phone calls, Siri, alerts, navigation and the microphone uplink, on a GM AAOS head
unit where the app cannot address a bus and can only choose `AudioAttributes` usages. Flow context:
[`12_OBSERVED_FLOW.md`](12_OBSERVED_FLOW.md). History of the superseded 2026-09 voice design (keep-alive,
idle sweep, energy duck, per-usage `Sink`, rows A6-A11): [`11_HARDENING_PLAN.md`](11_HARDENING_PLAN.md).

Source: `app/src/main/java/zeno/gmccpa/av/` — `AacPlayer` (media), `VoiceRouter` (Android half of `:9003`),
`PurposeEngine` + `VoiceStreams` (one `AudioTrack` + `MediaCodec` per active purpose), `VoiceJitterBuffer`,
`VoiceLatency` (constants, `Purpose`, `mapping`), `VoiceFrame`, `FocusSlot`, `DuckRamp`, `MainAudioHandover`,
`FocusReclaimRule`, `MicUplink`; wired into `CarPlayActivity`.

---

## 0. Wire format: `:9003`

`:9002` carries media (bare ADTS AAC-LC). `:9003` carries every other stream, tagged by the box
(`crates/vendor/receiver/src/forward.rs`). All fields big endian. Base head, 11 bytes:

```
[rate u32][ch u16][atype u8][len u32][payload]        // tag_voice (v1): payload = AU
```

`atype`: 0 media, 1 telephony, 2 speechRecognition, 3 alert, 4 default/absent, 5 compatibility. The base
tag spec is `docs/ocbm/00_PROTOCOL.md` ("The `:9003` voice seam tag").

**The shipped app speaks voice-tag v2** (`voice-tag-v2` feature, enabled by `carplay-jni`; `tag_voice_v2`):

| Frame | `atype` | `len` covers | Payload |
|---|---|---|---|
| AU | `atype \| 0x80` | 23-byte extension + AU | `[scid u64][rx_us u64][seq u16][rtp_ts u32][stream_type u8][AU]` |
| control | `0xFF` | payload | `[kind u8][write_us u64]…`; kind 1 BEGIN, 2 END (`voice_begin_v2` payload is 35 bytes) |

`rx_us` is the box's `voice_clock_us()` at RTP receive; `seq` and `rtp_ts` are the packet's RTP fields. END
carries a reason (unknown, teardown, session reset, superseded, socket error).

Why `atype` exists: telephony, speechRecognition and Siri `default` are all AAC-ELD 16 kHz mono, so
`(rate, channels)` yields one bit where five purposes are needed. The macOS host collapses `audioType` to one
`isVoice` bit and `carlink_native_personal`'s `resolveTargetSlot` sends Siri to the call track, so neither
reference implementation can be copied for routing.

`VoiceLatency.mapping(streamType, audioType, codec)` picks the purpose; rows are evaluated top to bottom:

| Stream | Result |
|---|---|
| any + compatibility (5) | dropped, no PCM path (explicit decision) |
| non-ELD codec (non-media AAC-LC, PCM, Opus) | dropped, no voice decoder |
| media (0) | dropped; media never reaches `:9003` |
| 101, any remaining audioType | NAV |
| telephony / speechRecognition / alert | CALL / ASSISTANT / ALERT |
| default/absent | ASSISTANT (deviation from stock, which maps it to Media; not seen on this unit) |

---

## 0a. Verification state

| Claim | State |
|---|---|
| Media AAC-LC 48 k stereo (`AacPlayer`) | Device-proven 2026-08-05 |
| Media paused during Siri so the knob reaches `VOICE_COMMAND` | Device-proven 2026-08-12 (117 knob steps measured, §3) |
| `atype` routing: Siri, call, nav each audible on the correct bus/volume group | Owner-confirmed on the truck 2026-09-04 (no capture committed). Pre-v5.8 build; the v5.8/v5.9 voice rebuild is bench-verified on cp12 only |
| AAC-ELD Siri decode | Device-proven 2026-09-09: `[voice] siri: AAC-ELD 16000Hz 1ch -> AudioTrack(usage=16), decoder=c2.android.aac.decoder` (software decoder; no hardware audio codec on this unit, §1) |
| Mic uplink | Owner-confirmed on the truck 2026-09-04: AAOS mic-in-use indicator raises on request and clears on release; no GM mic gain control exists |
| ALERT sink | Unexercised on the truck |
| `reclaimFocus` rework (§2), MainAudio take (OD-9C) | Not truck-verified; cp12 showed the screen restored twice |
| cp12 bench (nav live with no focus request, duck lift, platform duck, 0-loud-excess start) | Verified on the unit |
| Not yet measured | B-V1-W overlap matrix (10 nav + 3 Siri overlaps + 5 Siri), B-V3-3, media net-underrun rate vs cp12's ~1/min, FM -> CarPlay resume, whether iOS sends `audioLatencyMs` other than 80 beyond cp7/cp8, why the radio cold-restarted at 04:48:50 during rc2 (see `12`) |
| Media recovery after a voice/call turn | Measured on the truck: 12.0 s before the fix (2026-09-08), 1.95 s after (2026-09-09), on the pre-v5.8 two-clock design (assistant edge vs sink-idle focus release) that no longer exists; not re-measured on v5.8/v5.9 |

---

## 1. Target routing

Bus map, read from `/vendor/etc/car_audio_configuration.xml` on the unit:

| Bus | Context | CarPlay source | `AudioAttributes` usage | Purpose |
|---|---|---|---|---|
| `bus0_media_out` | music | type 102 `media` | `USAGE_MEDIA` + `CONTENT_TYPE_MUSIC` | `AacPlayer` |
| `bus1_navigation_out` | navigation | type 101, 48 k stereo | `USAGE_ASSISTANCE_NAVIGATION_GUIDANCE` + `SPEECH` | NAV |
| `bus2_voice_command_out` | voice_command | 100 speechRecognition/default, 16 k mono | `USAGE_ASSISTANT` + `SPEECH` | ASSISTANT |
| `bus3_call_ring_out` | call_ring | 100 alert | `USAGE_NOTIFICATION_RINGTONE` + `SONIFICATION` | ALERT (alert volume = ring volume; OD-7C) |
| `bus4_call_out` | call | 100 telephony | `USAGE_VOICE_COMMUNICATION` + `SPEECH` | CALL |

The app never addresses a bus: GM's `CarAudioService` maps usage -> context -> volume group -> bus.
`carlink_native_personal` uses no `CarAudioManager`, zone API or bus address either.

ALERT uses `NOTIFICATION_RINGTONE` because the alternative cannot reach the ring bus: the `voice_call` product
strategy puts `VOICE_COMMUNICATION` and `VOICE_COMMUNICATION_SIGNALLING` in the same volume group `phone`, and
`CarAudioContext.CONTEXT_TO_USAGES` (decompiled `CarService.apk`) maps context 5 (CALL, `bus4_call_out`) to
`{VOICE_COMMUNICATION, VOICE_COMMUNICATION_SIGNALLING}` and context 4 (CALL_RING, `bus3_call_ring_out`) to
`{NOTIFICATION_RINGTONE}` only. `dumpsys car_service` (2026-10-07) shows `CALL_RING -> bus3_call_ring_out`.
Not measured: how AAOS focus treats a `CALL_RING` request against CALL and MEDIA holders on this radio.

**`bus0_media_out` is a shared HAL mix bus.** `audio_policy_configuration.xml`:
`<route type="mix" sink="bus0_media_out" sources="mixport_bus0_media_out,bus3_sxm_in,bus4_lvm_in,bus8_tuner_in,
bus10_tuner_am_in,bus13_dab_in,bus14_rsi_in"/>`. GM FM/AM/SXM/DAB (`isExternalBus=1`) is injected at the DSP
and never passes through AudioFocus or an `AudioTrack`. `type="mix"` sums sources, so PCM written into
`bus0_media_out` during a focus-loss window is double-talk over live radio, not a no-op (cf. the observed
`Use hal ducking signals true`). `AacPlayer.reclaimFocus` has not been tested against a live FM session.

### HAL capabilities (from `/vendor/etc/audio_policy_configuration.xml`, image `W231E-Y181.3.2`, 2026-09-09)

- Every output bus mixPort declares exactly one profile, `PCM_16_BIT / 48000 / OUT_STEREO`, including
  `mixport_bus2_voice_command_out`. AudioFlinger resamples and up-mixes every 16 kHz voice AU; the HAL never
  sees 16 kHz. Building voice tracks at 48 k (plan row A9) was refuted and closed 2026-09-10 (see `11`):
  `getMinBufferSize` scales by the output-thread rate ratio, so it is not mis-sized.
- `mixport_bus0_media_out` has `flags="AUDIO_OUTPUT_FLAG_PRIMARY"` only. No DEEP_BUFFER mixPort exists, so the
  `AudioTrack` buffer is media's entire cushion (§3 rule 2).
- No `COMPRESS_OFFLOAD` anywhere; the only output flags are `PRIMARY` and `DIRECT|VOIP_RX` (the single
  `"direct output"` mixPort, 8/16/24/44.1/48 kHz mono or stereo). Every CarPlay sample is CPU-decoded and
  CPU-resampled.
- `bus11_mix_unduck_out`, `bus12_audio_cue_out`, `bus13_high_priority_mutex_out` (wired to `pcmUnduck_p` /
  `pcmCue_p` / `pcmHPMutex_p` in `audio_routing_configuration.xml`) have no volume group in
  `car_audio_configuration.xml` and no usage in `audio_policy_engine_product_strategies.xml`: DSP mix inputs GM
  drives internally, unreachable through `AudioAttributes`. `mix_unduck` is not a way around the platform duck.

---

## 2. Live mechanics (plan v5.8/v5.9, committed 02b12c0)

- **Real time, per stream.** Each voice stream (call, Siri, alert, nav) has its own RTP-timestamp jitter buffer
  (`VoiceJitterBuffer.kt`): target = SETUP `audioLatencyMs` (absent -> 80 ms, floor 32 ms; 80 on every type
  100/101 SETUP in cp8); watermark W = max(target + 20, 200) ms; Discard Excess down to W at the
  buffering->playing transition, on node-pool exhaustion and continuously while playing, re-anchored on the
  first surviving node, so a trim cuts delay and inserts no silence (`VoiceLatency` KDoc, `targetMs`,
  `watermarkMs`). Stale audio is dropped, never queued (owner 2026-10-07; OD-8A: no late nav). Inside a stream gaps are concealed with
  silence and, after a dry buffer, writes re-buffer silence at the period cadence (stock R14
  `AirPlayUtils.c:1052-1056`, `:1199-1206`). A stream lives from its BEGIN to its END (v2 control frames); a
  3 s no-AU stall infers a missing END.
- **Device buffer = stock's per-usage value** (`SD:CarPlayAudioManager.java:227-268`): CALL 32, ASSISTANT 128,
  ALERT 48, NAV 48 ms (`VoiceLatency.deviceBufferMs`), floored at `getMinBufferSize` (logged `FLOORED … (U1)`),
  `setStartThresholdInFrames` = that buffer, free space primed with silence before `play()`. One writer per
  track (its purpose worker, 8 ms period, `WRITE_NON_BLOCKING`), writing only what the track has room for
  (written - playback head vs device buffer) and pulling the jitter buffer for exactly that room, so a write
  never blocks and never retries. Unwritten audio is accounted at its playout time (played / suppressed /
  refused_lost, per stream in the END line). End to end = jitter target + device buffer (`PurposeEngine.prime`).
- **Start-transient exemption.** After `play()` the HAL bus takes ~140-400 ms to start pulling (cp12). Until the
  head first moves no trim happens (bounded at W + 500 ms, `START_STALL_CAP_MS`); what built up becomes an
  allowance, paid back only by dropping silent AUs (`VoiceJitterBuffer.holdTrims` / `deviceStarted` /
  `silenceCatchUp`), so first words are delayed, not cut. iOS streams silent AUs at realtime through idle
  and paused voice streams, so loudness (`LOUD_PEAK` 800, ~-32 dBFS) is only a classifier for that
  catch-up and for the stats; it gates nothing else. Observed (rc2 nav, 04:48:20): excess 11, excess loud 0,
  silent catch-up 9.
- **URGENT_AUDIO.** `VoiceLatency.VOICE_THREADS_URGENT_AUDIO` (default true) raises the voice workers and the
  `:9003` reader to `THREAD_PRIORITY_URGENT_AUDIO` (`VoiceRouter.raisePriority`), as `MicUplink`'s capture already
  is. Origin: a perf review saw a nav worker runnable-not-running for 97.5 ms at nice 0 against a 48 ms
  device buffer; no before/after CPU measurement exists. The review notes are not in the tree.
- **Focus.** CALL, ASSISTANT and ALERT request focus through a fresh epoch-stamped listener per request
  (`FocusPort` in `VoiceRouter`, `FocusSlot.kt`); AAOS `CarAudioFocus` keys on listener identity, so one
  listener per request is required. CALL/ALERT focus is kept until phone 1->0 while a call is up; ASSISTANT
  is handed over to a Siri-placed call.
  The phone entity of iOS's `modesChanged` (`VoiceStreams.ctrlCall`, set by `VoiceRouter`) is the in-call signal; in the 2026-10-06 call
  capture (n=1) `phone=1` arrived 1.0 s before the telephony SETUP and `phone=0` 0.25 s after its TEARDOWN. **NAV requests none** (OD-8C = YES, as stock,
  `SD:CarPlayAudioManager.java:748-750`; bench T1/T3 from a scratch capture, `/tmp/ccpa_sess/stock_runtime`,
  not in the tree, cited in the `NavTest` KDoc) and plays live from BEGIN to END. NAV during a call or Siri is
  PLAY_LIVE (OD-8D; the phone's maneuver tone over a call); `NAV_DURING_VOICE=DROP` remains only as a
  non-default safety net (`VoiceLatency.NAV_DURING_VOICE`).
- **Ducking.** Media follows `duckAudio` / `unduckAudio` (volume dB, durationMs; defaults 0.2 / 500 ms) through
  one VolumeShaper per track: gain = min(voice ramp, focus ramp) (`DuckRamp.kt`). 0.2, not 0.8: a 20 % duck
  (~2 dB) was reported as "does not duck". Focus `LOSS_TRANSIENT`/`LOSS` pause the media track
  (`pausedForFocus`), they are not a duck to zero. On unduck media returns to 1.0 at once; the GM platform duck
  carries the tail (stock T1). `NAV_UNDUCK_HOLD` (software hold at -13.6 dB until the nav stream ends) is OFF
  by default: the platform duck already runs for gm_ccpa and a hold would stack to ~-30 dB
  (`VoiceLatency.NAV_UNDUCK_HOLD`). This app's own NAV/CALL/Siri focus requests also reach the MEDIA listener as a
  loss or duck; that is the mechanism, not a bug.
- **Duck lift (owner 2026-10-08).** When the nav track's head first advances (bus1 active), our software duck
  lifts over 100 ms (`NAV_DUCK_LIFT_RAMP_MS`) so media sits at the platform duck alone (~-14..-19 dB) instead
  of ~-30 dB: `PurposeEngine.liftForBus`, `NAV_DUCK_LIFT_ON_BUS`. rc2: lift 286 ms and 313 ms
  after `duckAudio`, platform `ducking -18.70dB` (cp12 04:48:16, 04:48:46).
- **Siri and the knob.** Media is PAUSED, not ducked, while Siri is live (§3): `VoiceRouter.assistantTick` ->
  `AacPlayer.setAssistantSpeaking`; edges come from the ASSISTANT stream's BEGIN..END plus the speech-mode START
  edge, not a quiet timer.
- **Alert** -> `USAGE_NOTIFICATION_RINGTONE` (bus3, OD-7C). Unexercised on the truck.
- **Permanent media-focus loss.** `AacPlayer.reclaimFocus` retries only on a resume intent (MediaSession play,
  new screen, or the surviving screen back at the front: `CarPlayActivity.onResume` ->
  `AacPlayer.onScreenReturned`) or a phone-side resume (`FocusReclaimRule.kt`; a phone already paused when the
  loss landed counts), not just because media keeps flowing. The loss also takes MainAudio from the phone like
  stock (OD-9C, `MainAudioHandover.kt`; `events.rs` Take {resource 2, type 1, priority 500, take 500,
  borrow 100} / Untake {2, 2}; JNI `nativeTakeMainAudio` / `nativeReleaseMainAudio`) and gives it back on focus
  GAIN. Unknown until bench (plan B-MED-9): whether iOS pauses on the take, and whether HID PLAY alone resumes.

### Decoding the voice seam

`:9003` carries raw AAC-ELD access units, not ADTS, so `MediaCodec` needs a hand-built `csd-0`
(`VoiceRouter.eldCsd`) from the encoder's real AudioSpecificConfig
(`docs/ops/05_AUDITS.md`, "eld-codec ELD-SBR ASC"):

```
csd-0 = f8 f0 31 2c 00 bc 00     // 16 kHz mono: AAC-ELD, SBR enabled by fdk auto-mode, frameLength 480
```

This 7-byte value is `ELD_CSD_16K_MONO`; `eldCsd` returns it for 16 kHz mono only and synthesises the ASC for
every other rate/channel pair (for example 48 kHz mono, self-checked in `VoiceRouter` against `F8 E6 30 00`). The
macOS host hardcodes `mFramesPerPacket` per codec instead, which `MediaCodec` will not accept.

---

## 3. Playback mechanics

Device-proven GM behaviours shared by `AacPlayer` and the voice tracks.

1. **48 kHz everywhere, no `PERFORMANCE_MODE_LOW_LATENCY`.** `AUDIO_OUTPUT_FLAG_FAST` is denied to third-party
   apps on this unit (`createTrack_l(8): AUDIO_OUTPUT_FLAG_FAST denied by server`), so low-latency mode buys
   nothing and can add jitter. Not AOSP-documented.
2. **Buffer sizing.** Voice: §2 (stock device buffer). Media: `AacPlayer.TRACK_BUFFER_MS` = 750, floored at
   2 x `getMinBufferSize`. Rationale: a 2026-09-09 session with an 85-160 ms buffer logged `[aac ] stopping — 843 frames
   played, 0 dropped (no track), 6 underruns`, 5 of 6 in the first 9 s; that is smaller than the phone's ~200 ms
   delivery pause. AudioFlinger will not mix a fresh `MODE_STREAM` track until its
   buffer has filled once (`Track::isReady`: `framesReady >= bufferSizeInFrames`), so that depth is the initial
   lead. The phone starts a media stream at roughly realtime with no pre-roll burst, so the lead equals the
   track buffer exactly and only an audible underrun grows it. Expected side effects, not faults: ~750 ms to
   first sound per stream start (`addTrack_l` re-arms FILLING on every re-add, including `play()` after
   `pause()`), and one benign `AudioFlinger: BUFFER TIMEOUT` + `AudioTrack: restartIfDisabled` pair per stream
   start (this unit's fill grace measures ~450 ms; the restart resumes the fill with contents intact).
   Frame accounting: `AudioTrack.write` in blocking mode becomes non-blocking on a paused track, so a session
   prints `N frames played, D discarded (paused), …`; the 2026-09-09 "843 played" figure included ~240 frames
   (~5 s) discarded while paused (iOS ended stream 102 at 22:55:39.596 and started a new one at 47.179, 5.1 s
   before the track resumed at 52.295).
3. **Writes.** Voice: `WRITE_NON_BLOCKING`, room-bounded (§2). Media: a deliberate blocking write
   (`AacPlayer.feed`; KDoc "parked in the blocking `write()`") so `pause()`, not a socket close, unblocks the
   consume thread. Native `AudioTrack::obtainBuffer` forces non-blocking whenever the track is not
   `STATE_ACTIVE`, so during a hold `feed` keeps being re-entered at the phone's delivery rate and the PCM is
   discarded, not queued; only the single in-flight write at `pause()` is interrupted. Hence
   `reclaimFocus` retries on its `FOCUS_RETRY_MS` cadence while the phone still delivers; it gets no attempts
   when the phone ends the stream (as it does for a Siri turn) and the thread parks in the seam `read()`.
4. **Silence-fill keeps the volume group.** AAOS picks the volume group from active players; a track that
   stops surrenders it and the volume keys jump elsewhere. A voice track therefore never stops between its
   stream's BEGIN and END (§2 gap concealment).
5. **Pause/release before `abandonAudioFocusRequest()`** at teardown, so AAOS sees no active player of that
   usage at abandon time. `AacPlayer.stop()`: `running=false` -> `pause()` -> `flush()` -> (release the primed
   track if the seam never connected) -> `abandonFocus()` last; with a live consumer the track's
   `stop()`/`release()` still happen later on the consume thread (deliberate, see the KDoc on `stop()`). Not
   truck-verified.

### Siri volume: pause media, do not merely duck it — hardware-confirmed 2026-08-12

Ducking serves audibility; pausing serves the volume knob. This unit's `CarVolume` priority list is
`NAVIGATION > CALL > MUSIC > ANNOUNCEMENT > VOICE_COMMAND > CALL_RING > ...`. `VOICE_COMMAND` sits below
`MUSIC` and a ducked MUSIC track still counts as active, so while media plays the knob targets MUSIC even
mid-Siri: 117 knob steps, all to group 5 (MUSIC), group 2 never. Focus gain type is not an input (the strings do
not appear in `CarVolume.java`; calls use the same `GAIN_TRANSIENT` as Siri with the opposite outcome).

The fix: `AacPlayer.setAssistantSpeaking` pauses the media `AudioTrack` for the Siri turn (without `flush()`, so
buffered media resumes), removing MUSIC from the active set; GM's popup then renders "voice".

- Programmatic group volume is impossible: `setGroupVolume` is `@SystemApi` behind `CAR_CONTROL_AUDIO_VOLUME`
  (`signature|privileged`) and this app is debug-signed in `/data/app`.
- Stock GM CarPlay can never reach `VOICE_COMMAND`: it plays everything through one `USAGE_UNKNOWN` source.

---

## 4. Microphone uplink

`MicUplink` connects eagerly to `127.0.0.1:9112` (the same socket carries the capture gate and
the outbound PCM; a data-triggered connect would deadlock). It gates on the control line, never on downlink
activity: the receiver writes `uplink on <rate> <ch>\n` when iOS SETUPs type 100 with `input=true` and
`uplink off\n` at teardown. Siri wants the mic before any downlink audio arrives, so an activity gate would clip
the onset.

- **Send:** `mic <len>\n` + `<len>` bytes S16LE PCM at the gated rate/channels. The Rust side does RTP framing,
  encryption with the stream input key and the byte-order swap.
- **Capture:** `AudioRecord(VOICE_COMMUNICATION, 16000, MONO, PCM_16BIT, minBufferSize x 3)`, 20 ms chunks,
  dedicated thread at `URGENT_AUDIO`.
- **Manifest:** `RECORD_AUDIO` + `FOREGROUND_SERVICE_MICROPHONE`, granted at install (`-g`).
- **Errors:** a non-positive `AudioRecord.read` (including `ERROR_DEAD_OBJECT`) ends that capture; nothing is
  retried in place; the next `uplink on` re-arms (`MicUplink.captureLoop`).

### Build

`mic-uplink-eld` is enabled by default (`native/carplay-jni/Cargo.toml`; opt-out for the separate arm64 Pi
build) with libfdk-aac 2.0.3 cross-built for `x86_64-linux-android`. `tools/build_apk.sh` exports `FDK_AAC_PREFIX` and
fails fast if the archive is missing. Rebuild recipe:

```bash
# from the repo root; scratchpad/ is gitignored
NDK=$ANDROID_HOME/ndk/30.0.15729638/toolchains/llvm/prebuilt/darwin-x86_64/bin
cd scratchpad/fdk && tar xzf fdk-aac-2.0.3.tar.gz -C src-android --strip-components=1
cd src-android && ./configure --host=x86_64-linux-android \
  --prefix=../install-android-x86_64 --disable-shared --enable-static --with-pic \
  CC="$NDK/x86_64-linux-android32-clang" CXX="$NDK/x86_64-linux-android32-clang++" \
  AR="$NDK/llvm-ar" RANLIB="$NDK/llvm-ranlib" \
  CFLAGS="-O2 -fPIC -I../stub" CXXFLAGS="-O2 -fPIC -I../stub"
make -j8 && make install
```

The trap: fdk-aac 2.0.3 includes AOSP's `<log/log.h>` under `__ANDROID__` for `android_errorWriteLog()` in two
CVE-hardening bounds checks in `libSBRdec/src/lpp_tran.cpp`. That header ships with the platform tree, not the
NDK. `scratchpad/fdk/stub/log/log.h` is a no-op stand-in; the bounds checks still run and clamp. Also build
`receiver` from its own directory, or workspace feature unification drags `eld-codec` in elsewhere
(`06_BRINGUP_RUNBOOK.md` §6.3, "The `eld-codec` trap").

---

## 5. Advertise vs implement

`/info` advertises 8 audio formats and 4 microphone input formats.

- **Output formats.** An unbacked output format produces silence, not teardown: `audioFormats` is a capability
  array iOS SETUPs against and the app answers with a real bound `dataPort`, so negotiation stays well-formed
  (`05_SESSION_FLOW.md` §8 rule 9). Both observed `-16720` kills (`cornerMasks`, `hevc`) were SETUP `enabledFeatures` tokens,
  not `audioFormats` entries.
- **Input formats.** All four can arm: entry 1 (`compatibility` PCM 16 kHz mono) and the three AAC-ELD entries
  (`mic-uplink-eld`, §4). None has been exercised end to end on the truck.
- **`mainBuffered` is out of scope, deliberately.** It buys resilience to a Wi-Fi hiccup (receiver-side buffer
  filled faster than realtime) but needs a buffered-stream handler plus a `FLUSHBUFFERED` verb. Until it exists
  it must not be advertised: if iOS moves media to a buffered stream and the app omits `mainBuffered`, media
  goes silent. Every stock Apple Simulator YAML template sets `enablesMainBufferedAudio: true`; check before
  copying a template `/info`.
- **Device-verified formats:** `pcm_16k_mono`, `pcm_48k_stereo`, `aac_lc_48k_stereo` (media),
  `aac_eld_16k_mono` (decoded Siri audio, 2026-09-09). Everything else in `/info` is backed by code but not
  proven on hardware.
- **All decode in software** (`c2.android.aac.decoder` for media and Siri; [`01_FINDINGS.md`](01_FINDINGS.md)
  §5). No offload path exists (§1).

### PCM `compatibility` entries

`preset_wireless_8()` (`crates/vendor/receiver/src/info.rs`) carries two `compatibility` entries on every
wireless session: type 100 PCM 16k-mono / 48k-stereo and type 101 PCM 48k-stereo. iOS has never been observed
to select one: 24 SETUP negotiations in five sessions (2026-08-05, 08-12 x2, 08-18 x2) gave `audioType=media`
x18, `default` x6, `compatibility` x0. Media rides type 102 -> `:9002` as atype 0.

If iOS ever falls back to a `compatibility` PCM stream for media, it arrives on `:9003` as atype 5; this
receiver has no PCM media sink, so `VoiceLatency.mapping` returns an unsupported mapping and the audio is
dropped (`VoiceLatency.AT_COMPATIBILITY`). Media would be silent while calls and Siri still work,
which looks identical to a stuck `AacPlayer` focus hold. The drop is logged at error level
(`VoiceStreams`, logged on BEGIN): `voice: <label>: <type> + compatibility: no PCM path (explicit decision) — its AUs are
DROPPED`. No capture exists for the 2026-08-28 session where media was silent while Siri worked, so neither
this path nor the focus path is confirmed for it.

Two logs settle it on the next session; neither needs a verbose flag:

| Half | Where | Line |
|---|---|---|
| App side: which sink each stream landed on | logcat | the `DROPPED` error above |
| Box side: negotiated `audioType` per stream | airplayd stderr -> `[box:…]` in logcat | `[session] SETUP phase2 audio({ty}) … audioType={..} -> dataPort {..}` |
| Box side: sink chosen per stream | airplayd stderr | `[audio] stream {ty} {codec} → {label} :{port} ({sr}Hz {ch}ch)` |

Both box lines are bare `eprintln!`s in `crates/vendor/receiver/src/session.rs` (the `SETUP phase2 audio(`
line and `spawn_audio`), ungated, reaching this app's logcat via the `CH_FILE` box-log stream. A fourth line
dumps the full stream dict for types 100..=112 and is marked for removal; do not build a procedure on it.

Reading: a type-102 / `audioType=media` SETUP with audio still failing points at the focus path; no 102 stream
but a `compatibility` SETUP points at the atype-5 drop, and the fix would be a PCM media sink. The entries stay
advertised because they are part of `preset_wireless_8`, the device-proven-as-a-whole preset, and `audioFormats`
is a reconnect-only key in `/info`.

Do not remove entry 1 until the AAC-ELD mic path is truck-tested end to end; treat it as load-bearing. Entry 2
(type 101 `compatibility`, output-only) carries no mic obligation and can be removed in its own change with its
own truck test; a regression there presents as "no audio at all", so bundle nothing with it.

### `cornerMasks`

An Apple affordance for rounded display cutouts, irrelevant to a rectangular panel and not advertised
(`levers::set_cornermasks(false)`). Cited only as one of the two historical `-16720` teardowns, evidence that
`enabledFeatures` tokens, not `audioFormats` entries, are what kills a session.

---

## Sources

`carlink_native_personal/app/src/main/kotlin/com/carlink/audio/` (DualStreamAudioManager, AudioRingBuffer,
MicrophoneCaptureManager) · `ocbm/crates/vendor/receiver/src/{session,forward,uplink,info,stream}.rs` ·
`ocbm/host/MacHost/carlink_macOS/Audio/` · `ocbm/docs/carplay/06_AV_PIPELINE.md` (audio formats) ·
`ocbm/docs/ops/05_AUDITS.md` (ELD ASC) · this app's `av/{AacPlayer,VoiceRouter,PurposeEngine,VoiceStreams,MicUplink,CarPlayActivity}.kt`,
`assets/info.bplist`.
