# The macOS host app

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 17; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

The shipping host application: decode, UI, settings/YAML authoring, and the SDK audit that shaped it.

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
- **Audio**: add the 24 kHz mic rate; watch long-session A/V clock drift (add a rate-matched ring buffer if
  it appears — not a full jitter buffer). Set decoder color attachments (709/601-4/sRGB) as cheap insurance.
- **OCBM cleanups**: the header `seq` is written-but-never-read (dead/footgun) and the "fragmentable"/SOM-EOM
  doc claim overstates v1 — either assert the single-frame contract on the host or implement coalescing.

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

### Cross-cutting theme
The unifying story is **finish the OCBM migration**: the CarPlay path is correct where it was ported
(crypto, single-touch, config, lifecycle) and simply absent where it wasn't (commands, multi-touch,
diagnostics). Tiers 1 + 3 are that completion; Tier 0 is the one real bug plus two robustness fixes; the
rest is config growth and roadmap.
