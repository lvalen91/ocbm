# CarPlayHost — the macOS host app (A/V verification milestone)

Refactored from `carlink_macOS` (the original CCPA host app) into `ccpa_custom/host/CarPlayHost/`. Goal
of this first milestone: **real pixels on screen + audio playing**, proving the committed OCBM
forward-encrypted A/V wire works with an actual app (not just the Rust `ocbm-host avdec` frame-counter).

## Architecture: what's reused vs replaced

The original app spoke the riddlebox protocol (magic `0x55AA55AA`, `AdapterProtocol`/`MessageTypes`) and
unwrapped the vendor session token using the AES parameters the accessory protocol defines for the host. The committed model is different: the **box forwards
ENCRYPTED A/V + hands the per-stream ChaCha20 key over OCBM**; the host decrypts. So:

**REUSE (unchanged / lightly adapted):**
- `USB/USBDeviceManager.swift` + `USB/USBTransport.swift` — legacy-IOKit claim of the CCPA device + bulk
  pipe I/O with stall recovery. Repoint the PID to the OCBM accessory (`0x1314:0x2d00`) and expose raw
  bulk read/write (see wiring #2).
- `Video/VideoDecoder.swift` (renamed from `H264Decoder.swift`, rewritten for the macOS-26 render
  model) — decode (H.264 + HEVC) rendered via `AVSampleBufferRenderSynchronizer` +
  `AVSampleBufferVideoRenderer.Receiver` (no `VTDecompressionSession`); bounded decode + enqueue FIFOs
  (`AVCCFastPath.FrameFIFO`, depth 3, non-blocking, IDR-protecting — V5, 2026-09-03; superseded the
  depth-1 latest-wins slots), drained on a dedicated serial `renderQueue` that owns the `Receiver` — the
  frame path never touches the main thread (V6). One instance per video lane (main + alt/cluster).
- `App/CarPlayView.swift` — the `AVSampleBufferDisplayLayer` host NSView (render surface).
- `Audio/AudioPlayer.swift` — media audio playback.
- App shell: `main.swift`, `App/AppDelegate.swift`, `App/MainWindowController.swift`.

**REPLACE (new, in `OCBM/`):**
- `OCBM/OCBMFraming.swift` — OCBM v1 16-byte framing + cursor-based `OCBMReassembler` (port of the
  hardened Rust `ocbm-proto`, resyncs on magic+hcheck).
- `OCBM/OCBMAVDecrypt.swift` — the A/V seam reassembler + ChaCha20-Poly1305
  decrypt via CryptoKit `ChaChaPoly` (exact port of the validated `avdec`/`receiver_core` crypto: video
  nonce `[0,0,0,0]‖counter_le64` + 128-B-header AAD; audio packet-trailing-8 nonce + `pkt[4..12]` AAD).
  **Framing (CORRECTED 2026-08-16 — this line gave `[u32 BE len][marker][payload]` for both lanes):** that
  shape is the AUDIO lane. The VIDEO lanes (main + alt) are `[u32 BE len][SEAM_MAGIC 4B][marker][payload]`,
  and a frame record is `[0x01][seq 8B LE][hdr 128B][body]` — the magic is the resync anchor
  (`resyncVideoToMagic`) and `seq` is both the ChaCha counter and the gap detector that fires
  `requestKeyframe()`.
- `OCBM/OCBMClient.swift` — the host session controller: HELLO → **SUBSCRIBE** (commands box
  IDLE→projection→ARM) → ~1 Hz HEARTBEAT → STOP; routes CH_VIDEO/CH_MEDIA_AUDIO to the decrypt layer.

**DROP for this milestone (dormant / delete later):** `Protocol/*` (riddlebox), `Protocol/SessionTokenDecryptor.swift`
(AES token), `Audio/MicCapture.swift`, `App/CallManager.swift`, `Audio/NowPlayingManager.swift`,
`Protocol/IAP2CallStateDecoder.swift`. Touch input is a later milestone (uplink).

> **UPDATE 2026-08-16 — the "delete later" happened, with one exception.** `Protocol/` (and with it
> `SessionTokenDecryptor` and `IAP2CallStateDecoder`), `App/CallManager.swift` and
> `Audio/NowPlayingManager.swift` no longer exist. `Audio/MicCapture.swift` was NOT dropped — it came back
> as the Siri/telephony mic uplink and is instantiated in the OCBM path (see the 2026-08-16 correction below).

## Data flow

```
iPhone → box (airplayd fwd-enc) → OCBM CH_VIDEO/CH_ALT_VIDEO/CH_MEDIA_AUDIO/CH_ALT_AUDIO (encrypted + key)
      → USB bulk → USBTransport(raw, demux-only) → OCBMClient → OCBMReassembler → by channel
      → OCBMAVDecrypt: per-lane serial decrypt queue (video / altVideo / audio), ChaCha20-Poly1305
      → { avcC/hvcC config, AVCC video frames, audio AUs }
      → main VideoDecoder + alt/cluster VideoDecoder → AVSampleBufferVideoRenderer.Receiver   +   AudioPlayer
```

## Wiring status

**DONE in code (this pass):**
- `USBTransport` — added a raw-bulk mode (`rawReadHandler` skips the `0x55AA55AA` reframing) +
  `writeBulkRaw`, and conformed it to **`RawBulkTransport`**.
- `USB/USBDeviceManager.swift` — added `0x1314:0x2d00` (OCBM accessory) to `kSupportedDevices`. Endpoint
  discovery (bulk IN/OUT by direction) already works for it — no change needed.
- `OCBM/OCBMAVBridge.swift` — the decoder feed: parses the plaintext **avcC** for SPS/PPS, converts each
  decrypted **AVCC** access unit to **Annex-B** (prepending SPS/PPS on keyframes), and feeds the reused
  `VideoDecoder`; audio → `AudioPlayer.feedMediaPCM`.
- `Audio/AudioPlayer.swift` — added `feedMediaPCM(_:)` (48 kHz/16/stereo LPCM, skips the riddlebox header).
- `App/AppDelegate.swift` — `setupDevice()` now builds the OCBM chain (`OCBMAVBridge` + `OCBMClient`) instead
  of the riddlebox `AdapterProtocol`, and `client.connect()` sends HELLO → SUBSCRIBE → heartbeat. `endSession()`
  tears the OCBM client down.

**REMAINING (Xcode, against the live box) — the only step left before running:**
1. **Add the `OCBM/` group to the Xcode target** (drag the `OCBM/` folder into the project navigator → check
   the target, or Build Phases → Compile Sources). This is why SourceKit currently shows "cannot find OCBM…" —
   the new files aren't in the target yet. CryptoKit is a system framework (no linking needed).
2. **Build + run** with the box powered (boots to IDLE, supervisor waiting) + iPhone connected. On launch the
   app claims `0x1314:0x2d00`, and `client.connect()` SUBSCRIBEs → the box projects → frames render.

> **UPDATE 2026-08-16 — step 1 is long since DONE** (the list stands as the milestone record). The `OCBM/`
> group and every file in it are in `carlink_macOS.xcodeproj/project.pbxproj` — PBXGroup `OCBM`
> (`OCBMFraming`, `OCBMAVDecrypt`, `OCBMClient`, `OCBMSessionCoordinator`, `OCBMControlRelay`, `OCBMAVBridge`,
> plus `AirPlaySetupSession` and `StreamMetrics` added since), each with its own Sources build-phase entry.

> The `AdapterProtocolDelegate` / touch handlers were inert here (no `adapter` created) at this milestone.
> **UPDATE 2026-08-01:** that legacy `AdapterProtocolDelegate` path has since been REMOVED (it no longer
> exists in any `.swift` file); touch now rides `CarPlayView` → `OCBMClient.sendTouch` → `CH_INPUT`.
> `MicCapture`/`CallManager`/`NowPlayingManager` are no longer instantiated in the OCBM path.

> **UPDATE 2026-08-16 — the `MicCapture` half of the line above is SUPERSEDED.** It was true at this
> milestone (2026-07-09), where `Audio/MicCapture.swift` sits on the DROP list higher up, and it was
> carried forward unchanged by the 2026-08-01 update — but the mic came back. `AppDelegate.setupDevice()`
> builds a `MicCapture`, feeds `client.sendMicPCM`, and starts/stops it from `client.onUplinkGate` — the
> box's type-100 `input` SETUP gate (Siri/telephony), at the box-negotiated rate/channels — and the file is
> in the Xcode target. `CallManager` and `NowPlayingManager` were retired by DELETION — neither file exists.

## Verify — DONE (2026-07-09, live box + iPhone)

Confirmed end-to-end: **CarPlay video renders + media audio plays correctly**, box and app agreeing.
- Box: SUBSCRIBE → projection → `[sup] ARMED` → pair-verify OK → `fwd-enc: handed video/media key to seam`.
- App (FileLogger, subsystem `com.carlink.ocbm`): `received video key` → `H264 Format updated from SPS/PPS`
  → decrypt tally `video ok=… fail=0, audio ok=… fail=0` (both climb, **0 failures**).

**Audio endianness fix (the "static" bug):** wired CarPlay media PCM is 16-bit **big-endian** (network
byte order); `feedMediaPCM` was copying it into a host-endian (LE) `int16ChannelData` buffer, byte-swapping
every sample → white-noise static. Fixed by swapping BE→host on copy (see `AudioPlayer.feedMediaPCM` +
`WIRED_AUDIO_ROOT_CAUSE.md`, which is NOT in this repo — it lives in the archived predecessor tree at
`../old/ncm_carplayd/research/` relative to the repo root: BE Δ/amp≈0.03 clean vs LE≈1.30 static). Decrypt was
never the problem — audio always ran 0-fail; only playback interpretation was wrong.

**Logging:** OCBM loggers use subsystem `com.carlink.ocbm` so the `FileLogger` (`OSLogStore`, captures
`com.carlink.*`) persists them to `~/Library/Logs/Carlink/`. The 1 Hz heartbeat only emits a tally line
when the counts change (no idle flood).

**Resolution:** the box advertises the negotiated dimensions — default **1920×720**, up to **4K@60
(3840×2160)** hardware-validated (docs/carplay/06_AV_PIPELINE.md). The YAML config framework has since landed: the box reads
`/tmp/carplay_cfg.yaml` (airplayd), so 1920×720 is now just the fallback default rather than a hardcode
(per docs/carplay/04_CAPABILITIES_AND_CONFIG.md even that box-side default is interim — target state is the box holding IDLE until the
app pushes config, with no defaults of its own).
The app's own display/window menu (e.g. 1280×720) is a separate scaler.

## Notes
- **HEVC — DONE (corrected 2026-08-16; this bullet described it as still to do):** the fwd-enc forwarding
  path is codec-agnostic, as written, and the hvcC path landed — `OCBMAVBridge` pulls the `hvcC` box out of
  the box's video config (VPS/SPS/PPS + `lenSizeMinusOne`) and `VideoDecoder`'s `.hevc` case builds the
  format description from it. What the box does carry is the ADVERTISEMENT gates (`hevcInfo` in `/info` +
  `enabledFeatures:["hevc"]`), armed per connection from the app-pushed `enablesHEVC`
  (`levers::set_hevc(vc.accessory_config.enables_hevc)`); the app's stored default for that toggle is **ON**
  (`SettingsWindow`: `b("enablesHEVC", true)`), and only the app-less / parse-failure path clears it to the
  H.264 behaviour.
- **Input uplink** (touch → iPhone) is the next milestone after A/V renders — not in this scope.
  **✅ UPDATE 2026-08-01: since implemented** — this HOSTAPP.md milestone doc predates it. Touch now
  rides `CarPlayView` touch handlers → `OCBMClient.sendTouch` → `CH_INPUT`; the "later milestone / inert
  touch handlers" notes above describe the earlier legacy `AdapterProtocolDelegate` path, not the live one.
- The ChaCha crypto is confirmed byte-for-byte against Apple's CarPlaySDK (see the hardening pass), so a
  decrypt failure here means a framing/key-plumbing bug in the Swift port, not a crypto mismatch.
