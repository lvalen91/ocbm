# The macOS host app

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 17; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

The shipping host application: decode, UI, settings/YAML authoring, and the SDK audit that shaped it.

**Other app-visible UI/session features (2026-09-03) with no dedicated section here** — pointers only:
`OCBMClient` now re-sends `CT_SETTIME` after every SUBSCRIBE, not just at HELLO; the Box Log window
gained dynamic per-source colours, backfill-entry dimming and a Hide-history filter
(`App/BoxLogWindow.swift`, `App/BoxLogStore.swift`); the pairing-code panel gained **Pair**/**Cancel**
buttons, shown only when interactive numeric-comparison pairing is on (`docs/wireless/01_BT_AND_RADIO.md`);
and the CCPA tab gained a confirmed **Enter NCM** action plus a `carlink://box/enter-ncm` URL handler
(`open -a <app> carlink://box/enter-ncm`, `App/AppDelegate.swift`). Wire-level detail for all of these is
in `docs/carplay/01_OCBM_PROTOCOL.md` and `docs/wireless/01_BT_AND_RADIO.md`, not here.

## Host app — SDK audit and plan

<!-- absorbed: ../host/00_MACOS_HOST_APP.md -->

Consolidated findings from a 12-facet audit of the macOS host app against Apple's CarPlaySDK ground
truth (docs/carplay/03_SDK_GROUND_TRUTH.md, the SDK binary, Apple's YAML/VDC templates). Each facet classified every behavior
ADHERES / DEVIATES, and every deviation was verified against the SDK and either JUSTIFIED or flagged
BUG/GAP. Governing rule: adhere to the SDK; deviations must be verified + justified.

### Headline
The crypto, framing, audio, video-decode, single-touch, window geometry, and session-lifecycle cores all
**adhere** and are byte-for-byte faithful to the box/SDK. The real issues are (a) **one correctness bug
that explains the current 4K poisoning**, and (b) an **incomplete OCBM migration** — touch was ported to
the OCBM/CarPlay path but commands, multi-touch, and diagnostics were left on the now-dead legacy `adapter`
path. The deep SDK deviations (per-packet RTP seq, NACK retransmit, flow control) are all correctly
delegated to the box or deferred with justification — roadmap, not bugs.

### Correction to one agent's framing
The nav/AltVideo audit concluded the app "can't be Apple-faithful because the dongle is the AirPlay
endpoint." That is true for the *legacy stock-firmware* model but **wrong for ccpa_custom**: our own
`airplayd` on the box IS the AirPlay receiver, so real AltVideo / VDC navigation / cluster ARE
implementable (in the box, forwarded over OCBM). The agent's useful finding stands: the 0x2C/508/509 nav
path is legacy proprietary code to retire — not a fidelity target.

### Tier 0 — correctness bugs — **ALL THREE RESOLVED (2026-08; see the 4K-track section below)**
1. **Video-frame size cap rejects 4K IDRs → constant poisoning [A/V-recovery].** `OCBMAVDecrypt.nextVideoMessage`
   caps a reassembled video message at `2 × OCBM.maxPayload` = 131072 B. A 4K IDR is far larger, so **every
   4K keyframe is rejected** → resync → the decoder never gets a clean IDR → permanent P-frame poisoning.
   This is a pre-existing bug (old parser had the same cap): benign at 1920×720, fatal at 2400×960+/4K, and
   it is the direct cause of "corrects on a keyframe, but constant." **Fix:** the reassembled *video-message*
   limit is a different concern from the OCBM *per-frame* transport limit (the seam already reassembles
   across OCBM frames) — size it to the max coded frame for the negotiated resolution/codec (several MB).
   **RESOLVED:** `maxVideoMessage = 16 MB` in `OCBMAVDecrypt.swift`; 4K IDRs pass, decoder gets clean keyframes.
2. **Renderer keyframe-recovery is a no-op [video-decode].** `VideoDecoder.onNeedsKeyFrame` (fired on a
   flush-required-to-resume) is log-only in the OCBM path; only the seq-gap detector requests keyframes.
   **RESOLVED:** `onNeedsKeyFrame` is a live wired callback → `requestKeyframe()`. (The class was renamed
   from `H264Decoder` and now decodes H.264 **and** HEVC.)
   **Fix:** wire `decoder.onNeedsKeyFrame → client.requestKeyframe()` (reuse the ≤1/500 ms throttle) so a
   renderer-driven poison also forces an IDR. Cheap, complements Tier 0 #1.
3. **Main-thread block on transport-lost teardown [USB].** `handleOCBMTransportLost → endSession →
   disconnect()` did a `queue.sync` blocking `WritePipeTO` (~7 s) on an already-dead pipe → UI beachball on
   the exact failure it's recovering from. **Fix:** send STOP fire-and-forget; never `queue.sync` a bulk
   write from the main actor.
   **RESOLVED (verified 2026-08-16):** `OCBMClient.disconnect()` is async end to end — the timer cancel +
   generation bump + `helloAcked` reset ride `queue.async`, and STOP is best-effort behind a bounded
   `DispatchSemaphore` grace while `transport.stop()` AbortPipes the endpoint. No `queue.sync` remains.

*(Deploy Tier 0 alongside the already-built ocbmd backpressure change — backpressure reduces the drops,
the frame-cap fix lets the recovery keyframes actually land. Together they are the real 4K fix.)*

### Tier 1 — the incomplete-OCBM-migration cluster (functional holes)
4. **Keyboard/media command surface is entirely dead in OCBM mode [command-map, touch, legacy].** Home,
   Siri, play/pause/next/prev, D-pad, knob all route to the nil legacy `adapter` — only touch was migrated.
   **Fix:** add OCBM `CH_INPUT` opcodes for HID media-buttons (uid 2) + D-pad, and `/command`
   (`requestUI` for Home, `requestSiri` for Siri); rewire `didPressCommand` to `ocbmClient`. Box already
   advertises the media-buttons HID device. (Ties to tasks #19 + #20.) Also: `disableBluetooth` is a
   fabricated non-Apple command (drop it); `NowPlayingManager` is never instantiated (dead — wire or remove).
   **RESOLVED (verified 2026-08-16) — the "entirely dead" statement is no longer true.**
   `AppDelegate.carPlayView(_:didPressCommand:)` guards on `ocbmClient` and routes the whole surface:
   media keys → `sendMediaButton` (uid 2), Home/Back/D-pad/knob → `sendNav` (uid-3 HID D-Pad — NOT
   `requestUI`, which did nothing as a Home button, 2026-07-12), Siri → `sendCommand(cmdSiriDown/Up)` with
   the paired UP on a 0.3 s deadline. `NowPlayingManager` was retired by deletion (no such file).
5. **Multi-touch / gestures dead in OCBM mode [touch].** Pinch + two-finger scroll are captured but routed
   to the nil adapter, and the box only advertises a single-touch HID descriptor. **Fix:** 2-point
   `CH_INPUT` sub-frame + box `HIDTouchScreenMulti` descriptor (12-B report). Interim: degrade `scrollWheel`
   to a single-finger drag when `ocbmClient != nil` so map scroll works. (Ties to #20.)
6. **HELLO not gated on HELLO_ACK → boot-race silent death [session, USB].** Confirmed by 3 audits and
   task #34. On a boot race the HELLO is lost, the app SUBSCRIBEs into the void, the 5-error path can't fire
   (timeouts, not errors), and the UI shows a misleading "Waiting for phone…". **Fix:** retransmit HELLO
   until HELLO_ACK with a bounded deadline; gate SUBSCRIBE/heartbeat on it; surface a real "box not ready".

### Tier 2 — config completeness + robustness hardening
7. **VehicleConfig completeness [config].** Add `enablesUIAppearance`/`enablesMapAppearance` (all templates
   set them; zero box change), then `viewAreas`/`safeArea`, then `hidConfig`/`primaryInput` (with #20).
   De-dup the duplicate 4K preset and fix the docs/carplay/04_CAPABILITIES_AND_CONFIG.md↔code default note. **4K@60 is the TARGET, retained
   (user directive):** iOS accepting the 4K `/info` + SETUP is the ground truth; Apple's simulator templates
   are examples, not the protocol ceiling, so the audit's "lower resolution / experimental" recommendation
   is OVERRULED. The project is optimized *for* clean, stable 4K@60 (see "4K@60 optimization track" below).
8. **Derive touch aspect from the decoded frame, not the advertised resolution [window].** Today safe only
   because iOS encodes at the advertised res; decoupling removes a latent touch-misregistration risk.
9. **Make the 8 s A/V-stall path actionable or honest [session].** It shows "Reconnecting…" but does
   nothing; the `OCBMAVDecrypt.reset()` + `resetWatchdog()` primitives exist but are never called. Wire a
   bounded STOP→reset→SUBSCRIBE, or relabel the status.
10. **USB robustness [USB].** Only `ClearPipeStall` on a real `kIOReturnPipeStall` (not on every idle
    timeout); fix the mislabeled "5 consecutive errors" (timeout should reset it) + add retry backoff.

### Tier 3 — legacy retirement + diagnostics
11. **Retire the dead legacy stack [legacy].** `AdapterProtocol`, `MessageSerializer`, `MessageParser`,
    `SessionTokenDecryptor`, `IAP2CallStateDecoder` are wholly unreachable in OCBM mode. Delete them + the
    adapter-only `MessageTypes` members + the dead `adapter?.…` call sites. **Preserve** the shared pieces:
    `USBDeviceID`/`kSupportedDevices`, `DisplayResolution`, the `Data` LE helpers. Keep `IAP2CallStateDecoder`'s
    TLV field maps as reference docs only.
12. **Rewire diagnostics to OCBM [legacy].** `SessionRecorder` + `ProtocolLogger` record NOTHING in OCBM
    mode (they hang off the skipped legacy framing) — "record a session" captures an empty file. Move their
    hooks to the OCBM raw-read/write path; keep the PIN-masking + throttling.
13. **Finish disabling the legacy reinit path [session].** The nav-resolution and fullscreen-screen-native
    paths still call `reinitializeAdapterSession`, which can needlessly tear down a live OCBM session; make
    them window-only like the menu paths. (The transport-lost caller is the one legitimate use — keep it.)
    **CLOSED BY REMOVAL (verified 2026-08-16) — this was REAL when written:** the 2026-07-25 tree still had
    a second call site, `reinitializeAdapterSession(reason: "screen-native resolution")`. Fullscreen support
    was removed 2026-08-02 and took that caller with it, so exactly ONE call site remains —
    `AppDelegate.handleOCBMTransportLost` → `reinitializeAdapterSession(reason: "OCBM transport lost")`, the
    legitimate use this item already exempted. A stale mention survives in a `USBDeviceManager.swift`
    comment — code comment only, no call.

### Tier 4 — deferred / roadmap (verified-justified deviations, not bugs)
- **HEVC decode — DONE (verified 2026-08-16; this bullet also contradicted Tier 0 #2's own note above).**
  `VideoDecoder` carries a full `.hevc` path — VPS/SPS/PPS parameter sets, `createHEVCFormatDescription`,
  HEVC NAL typing `(byte0 >> 1) & 0x3F` — and `OCBMAVBridge` parses the `hvcC` box out of the box's video
  config, falling back to a logged drop when the config is neither `avcC` nor `hvcC`, which is the FourCC
  guard this bullet asked for. Whether a session NEGOTIATES HEVC is app-pushed, not compiled: `enablesHEVC`
  arms the box's two gates per connection (`hevcInfo` in `/info` + `enabledFeatures:["hevc"]`) via
  `levers::set_hevc(vc.accessory_config.enables_hevc)` in airplayd, and the app's stored default is **ON**
  (`SettingsWindow`: `b("enablesHEVC", true)`). Only the app-less / parse-failure path clears the lever to
  off — that, not the app default, is the H.264 fallback this bullet assumed.
- **Real nav / AltVideo / VDC / NMEA GPS** — implementable in *our* box (see correction above); large feature.
- **NACK retransmit** — fuller SDK parity beyond forceKeyFrame-on-gap; needs a bidirectional NACK channel.
- **Audio**: the 24 kHz mic rate shipped (`MicCapture.startCapture(sampleRate:channels:)` takes the
  box-negotiated rate, no hardcode). The audio seam now resets on a box-signalled `F_NEW_SOURCE` frame
  flag and resyncs on `SEAM_MAGIC` so a producer swap can't desync the decrypt counter (2026-09-03; see
  `docs/carplay/01_OCBM_PROTOCOL.md`). Still open: watch long-session A/V clock drift (add a rate-matched
  ring buffer if it appears — not a full jitter buffer). Set decoder color attachments (709/601-4/sRGB)
  as cheap insurance.
- **OCBM cleanups**: the header `seq` is written-but-never-read (dead/footgun) and the "fragmentable"/SOM-EOM
  doc claim overstates v1 — either assert the single-frame contract on the host or implement coalescing.

### Telephony PCM lane + the 8 kHz mic uplink (2026-09-03)
Android Auto call audio never rides the projection link — gearhead leaves it on Bluetooth HFP/SCO — so
the box terminates the SCO link and forwards the call on the EXISTING voice sink (`CH_ALT_AUDIO`) using
a new audio-seam marker, `SEAM_PKT_PLAIN 0x03`: a `SEAM_FORMAT` of *PCM / 8000 Hz / 1 ch / 16-bit /
audio_type 1 (telephony)*, then one `[0x03][scid u64 LE][320 B]` message per 20 ms carrying raw S16LE
verbatim — no key, no RTP, no RFC 2198 (`OCBM/OCBMAVDecrypt.swift` `drainAudio`; the payload is
LITTLE-endian, unlike CarPlay's big-endian PCM, and `OCBMAudioStreamFormat.plainLE` is what stops
`OCBMAVBridge` byte-swapping it into white noise). Playback needs nothing new: 8 kHz mono voice is
already one of the 14 pre-warmed `AudioPlayer` nodes, `audio_type != 0` routes it to `navMixer`, and it
ducks media through the same energy-gated path as a Siri prompt. It logs `telephony PCM 8000 Hz/1ch
scid=… — player armed` once, the first 8 frames as `audio pkt trace … plain len=…`, and one
`telephony rx=<n> frames (<ms> audio)` per second. A plain frame arriving before its `SEAM_FORMAT` is
dropped and counted (`audioPlainNoFormatDrops`) rather than guessed at. **Uplink** is the existing
mic path with no new machinery: the box's gate asks for `uplink on 8000 1`, `MicCapture` keeps
capturing at the hardware rate and lets its `AVAudioConverter` resample (the input node is never asked
for 8 kHz), and the converted PCM is cut into exact **20 ms / 320 B** frames — a carried remainder, not
a padded or truncated one — before `sendMicPCM` puts them on `CH_MIC`. That chunking applies to every
rate, so a CarPlay Siri turn now ships 640 B frames instead of one 100 ms lump. It logs `mic uplink
armed 8000 Hz …` at the gate edge and `mic tx=<n> frames rms=<x>` each second, the RMS being the only
thing that distinguishes a live-but-silent uplink from a muted input device. Both lanes appear on the
`AVmon` line as `tel=<pps>` and `mictx=<pps>`.

#### Wideband (mSBC) on the same lane — 2026-09-04, NOT YET EXERCISED ON HARDWARE
The lane is codec-tagged now: `SEAM_FORMAT` codec `4` (`OCBM.seamCodecMsbc`, `ocbm-proto::SEAM_CODEC_MSBC`)
means the box negotiated HFP **wideband**, and each `SEAM_PKT_PLAIN` then carries one raw transparent-eSCO
read — 2-byte H2 header + 57-byte mSBC frame + pad, 60 B per 7.5 ms — instead of 20 ms of PCM. macOS ships
no SBC codec of any kind, so **the app decodes it itself**: `Audio/MSBCCodec.swift` is a from-the-spec mSBC
encoder + decoder (16 kHz, mono, 15 blocks, 8 subbands, LOUDNESS, bitpool 26, syncword 0xAD) and
`Audio/MSBCFramer.swift` is the eSCO transport around it — H2 resync, reassembly across split reads, and
packet-loss concealment driven by the H2 sequence number (the last good frame faded to zero, then silence).
Neither file imports anything but Foundation, so both run in the hardware-free harness, which is where the
filterbank's prototype table, a 20 dB-plus round trip, CRC rejection, framer resync and a decode check
against an independent fixed-point reference implementation are pinned. Playback is unchanged: 16 kHz mono
voice is already pre-warmed, and the decoded PCM goes through the same `feedPCM` + telephony pre-roll as
the narrowband lane. It logs `telephony mSBC 16000 Hz/1ch scid=… — player armed` and adds `plc=<n>` to the
per-second `telephony rx=` line whenever concealment ran.
**Uplink** follows the same gate, which grew an optional trailing codec byte
(`[state][rate u32 LE][ch][codec]`; the 7-byte form still means PCM and is what an OFF still sends).
On codec 4 `MicCapture` cuts the converted capture into **7.5 ms / 240 B** frames instead of 20 ms ones,
encodes each to a 57-byte mSBC frame, wraps it in an H2 header with the cycling sequence
(0x08/0x38/0xC8/0xF8) plus a pad byte, and sends the whole 60-byte packet as one `CH_MIC` chunk — the box
writes each chunk to the SCO socket verbatim, so a chunk boundary is a packet boundary. The PCM path is
byte-identical to before: the codec byte only ever selects the other branch. `tel=`/`mictx=` keep counting
chunks, which under mSBC is ~133/s per direction rather than 50/s.

### Android Auto in the Metadata window (2026-09-04)

The Metadata window's Media, Navigation and Phone panes are fed by a second source during an AA
session: the phone's MediaPlaybackStatus / NavigationStatus / PhoneStatus services, decoded in
`AA/AAMetadata.swift` and applied through `MetadataStore.applyAndroidAuto` with the same delta
semantics as iAP2. Navigation shows the phone's maneuver type as a glyph (the AA scheme in use sends
no image), the exit cue, step distance and the phone's own ETA string. State is cleared when the AA
session ends. Wire details: `docs/androidauto/01_SESSION_AND_AV.md` §"Metadata services".

### Android Auto in Settings ▸ stream performance (2026-09-03)
`StreamPerfSection` reads the OCBM decrypt layer's accumulators, which AA traffic never reaches (it
rides `CH_IP` → `AASession`), so an entire AA drive rendered four all-zero CarPlay rows. `AASession`
now publishes its own 1 Hz counters as an `AAStatsSnapshot` value (`OCBM/StreamMetrics.swift`, beside
the CarPlay rate math so the harness can test it) through a `Mutex` box, and the monitor's existing
1 Hz sampler converts it with `AARates.between` — per-second rates, never raw totals, with the
transport labelled from the box's projection mode. The rows hide when no AA session is publishing and
age out after 5 s if its loop stalls; `/tmp/carlink_metrics.json` gains a matching `aa` object (null
when no AA session), including the box-side `telephonyRxPerSec` / `micUplinkPerSec` from the lane
above.

### 4K@60 optimization track (primary goal — user directive: retain 4K@60, deliver clean stable video)
iOS negotiated 3840×2160 @ maxFPS 60 and the box is NOT CPU-bound (load ≈0.9). So clean 4K is a
transport-efficiency + recovery-correctness problem, not a resolution problem. Ordered path:
1. **Frame-cap fix — DONE (host).** Raise the reassembled video-message limit to 16 MB so 4K IDRs are no
   longer rejected. This alone should let keyframes land and clear the constant poisoning.
2. **Backpressure, not drop — BUILT (ocbmd), staged.** Gate the video read on the out-queue draining so a
   slow pipe throttles the iPhone's encoder (Apple flow control) instead of dropping P-frames. Fewer/zero
   drops → continuous seq → no poisoning.
3. **Renderer keyframe recovery — DONE (host).** `decoder.onNeedsKeyFrame → requestKeyframe()`.
4. **Forward-path efficiency (next).** Cut the `airplayd → :9001 → ocbmd` local-TCP-loopback copies (unix
   socket / splice) so the box sustains 4K@60 bitrate headroom and backpressure rarely engages. The stock
   firmware forwarded 4K@60 with no drops on this hardware — the target is parity.
5. **HEVC (later, halves bitrate).** Once H.264 4K@60 is clean, HEVC 4K@60 is easier (lower bandwidth); the
   3 gates (publish hevcInfo, accept hevc at SETUP, decode hvc1) become the follow-on.

### Host decode pipeline — bounded FIFOs, off the main thread (2026-09-03)
`Video/VideoDecoder.swift` moves each frame across two hand-offs: USB read queue → `decodeQueue`
(parse + sample-buffer build), then `decodeQueue` → `renderQueue` (`Receiver.enqueueImmediately`). Both
hand-offs are `AVCCFastPath.FrameFIFO`, a bounded FIFO whose depth defaults to **3** (the incoming frame
plus a two-frame consumer cushion); before this pass both were depth-1 latest-wins slots, and the
render hand-off ran on the main thread. Its overflow policy is the point:
- A frame is **protected** if it is an IDR *or* the frame immediately following an IDR, decided in
  stream order at push time and carried with the frame (an eviction never re-labels its neighbours).
- Overflow sheds the **oldest unprotected P**. If a keyframe already sits behind the hole — or the
  incoming frame is one — the chain repairs itself within `depth` frames and **no** keyframe is
  requested, which is why the fix also cuts the keyframe-request churn.
- If every queued frame is protected and the newcomer is a P, the **newcomer** is dropped and a keyframe
  requested — exactly the old "P over IDR" rule.
- If every queued frame is protected and the newcomer is an **IDR**, the oldest frame is evicted anyway.
  Refusing an incoming IDR would orphan every P that references it (~2 s of poison); evicting one queued
  frame costs at most the frames behind it, which that IDR repairs almost immediately.

The **producer is never blocked** on either hop — a full FIFO always resolves to a drop, so a stalled
consumer still cannot back-pressure the USB read path. The live-UI rule ("drop on backpressure, never
buffer") is intact; only the *choice of victim* changed. The depth knob survives: `maxDecodeDepth` /
`maxEnqueueDepth`, with AA still raising the decode hop to 64 to absorb VideoToolbox's one-time warm-up
burst, and `1` reproducing the old table exactly (the harness asserts that against the retained
`AVCCFastPath.resolveSlot` oracle rather than a copied table).

**The render hand-off (`drainEnqueue`) runs on `renderQueue`, a dedicated serial `.userInteractive`
queue — nothing on the frame path touches main any more** (the one remaining
`DispatchQueue.main.async` in the file is the `onDimensions` window-sizing callback, which is genuinely
UI). This is API-legal, not a liberty: in the macOS 27 AVFoundation swiftinterface
`AVSampleBufferVideoRenderer.Receiver` is neither `Sendable` nor `@MainActor` — `sampleBufferReceiver(adding:)`
returns it `sending`, i.e. a single-owner object transferred into one isolation domain, and Apple's own
usage example enqueues from a client-chosen serial queue, never main. `receiver` is built on the main
actor in `init` (the transfer point) and afterwards is touched only from `renderQueue`
(`enqueueImmediately` plus both `flush()` call sites); `synchronizer` and `displayLayer` stay
main-confined and are never read after init. `flush()` also gets stronger ordering from this: it is now
decodeQueue → renderQueue, a plain serial-queue guarantee, rather than racing the main thread's own UI
work.

**Latency is measured, not asserted**, because a queue trades frame loss for delay:
`VideoDecoder.wrapLatencyMs` (arrival → `CMSampleBuffer` built) and `.handoffLatencyMs` (arrival →
handed to the renderer), EWMA α=1/8, printed as `wraplat=<main>/<alt>ms` (`wrap>handoff`) in the `AVmon`
line and as per-stream `wrapLatencyMs`/`handoffLatencyMs` in `/tmp/carlink_metrics.json`. These names
replaced a first-session `decodeLatencyMs`/`declat` that measured the same zero-copy buffer wrap
(~0.1 ms) but implied VideoToolbox decode time — `Receiver` exposes no `VTDecompressionSession` and no
per-frame decode completion (its only feedback is `didFailToDecode`/`requiresFlushToResumeDecoding`/
`failed`), so the top-level JSON `decodeLatencyMs` key stays `null`, reserved for a build that routes
frames through an explicit decompression session. Read `handoffLatencyMs` beside `dropFps`: the fix is
working when `dropFps`≈0 and `handoffLatencyMs` stays inside one frame interval — if it climbs, the
depth-3 cushion is buying latency instead of buying frames.

**Status: the bounded-FIFO change (both hops) was measured on device** — drops collapsed from a
sustained trickle to a start-of-session-only burst (30 `evict-oldest-P`, all on the ENQUEUE hand-off, in
the first 25 s, then none), which is what motivated moving that hand-off off the main thread. **The
off-main-thread `renderQueue` change is built and unit-tested (53 harness cases) but UNMEASURED ON
DEVICE.** The next relaunch must confirm the start-of-session enqueue-queue burst is gone, `dropFps`≈0
in steady state, and `handoffLatencyMs` stays inside one frame interval.

### Cross-cutting theme
The unifying story is **finish the OCBM migration**: the CarPlay path is correct where it was ported
(crypto, single-touch, config, lifecycle) and simply absent where it wasn't (commands, multi-touch,
diagnostics). Tiers 1 + 3 are that completion; Tier 0 is the one real bug plus two robustness fixes; the
rest is config growth and roadmap.
