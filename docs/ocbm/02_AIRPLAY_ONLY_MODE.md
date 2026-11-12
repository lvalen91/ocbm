# 02 — AirPlay-only mode (experimental)

> **STATUS:** CURRENT · single owner for AirPlay-only mode: scope, the protocol as measured against a real
> iPhone, the box/host split, the implementation's verified state and what is left. Implemented 2026-11-12
> (`box/airplayd`, `MODE_AIRPLAY`, the OCBM tunnel, `ocbm-host airplay`, the macOS AirPlay Test Host); the
> wire contract is [00_PROTOCOL.md](00_PROTOCOL.md) *AirPlay-only mode*. Correct this file in place — do not
> add a sibling.

**Contents:** §1 purpose and scope · §2 authorization and licensing · §3 verified vs open · §4 the measured
protocol (discovery, session type, pairing and MFi, SETUP, streams and keys, timing, other requests, session
end) · §4.10 display canvas, resolution and HEVC · §5 box/host split · §6 phased plan · §7 running the probe ·
§8 capturing the iPhone side · §9 implementation notes (box, test host, incidents) · §10 provenance.

Tags: **VERIFIED** = seen on the wire, in the iPhone's own log, or by decrypting/decoding captures in the
2026-11-12 probe runs (iPhone19,2, iOS 27.3 build 24C5037i, sender `AirPlay/1015.5.1`; the extended run was
variant B7 with the probe's timeouts removed: Music 187 s, then Screen Mirroring 233 s (music video), then
Screen Mirroring 269 s (first-party iOS UI, Music audio started at +61 s)); **INFERRED** = from the shipping macOS AirPlay sender/
receiver studied at interface level, or from open-source receivers (role list:
[../ops/03_REFERENCE_INDEX.md](../ops/03_REFERENCE_INDEX.md) §G), consistent with the evidence but not
isolated; **OPEN** = not yet measured. The raw probe log and the `airplayd` syslog captures are bench
evidence and are not kept in the repo (they carry session keys and device identifiers).

## 1. Purpose and scope

An alternative to CarPlay for the same box and host: the box advertises a **plain AirPlay receiver**
(no CarPlay bit, no iAP2, no Bluetooth bootstrap) on its own Wi-Fi AP, and the iPhone sends **audio** and
**screen mirroring** to it. Offline only: no URL playback (`/play`, AirPlay video, HLS); the phone renders
everything locally and streams it.

- **Mode, not coexistence.** AirPlay-only and projection (CarPlay/AA) are mutually exclusive. The host
  enters the mode with `CT_MODE_SELECT [MODE_AIRPLAY 0x02]` plus TLVs that **it** defines: SSID and
  passphrase (required), receiver name, channel, the display it renders for (default 1920 × 1080 at
  30) and an optional HEVC flag (feature bit 42, §4.10). The box answers `CT_MODE_ACK`, stops carplayd/iap2d/btd, raises the AP and starts `box/airplayd`;
  `ocbmd` and the supervisor's `airplay_tick` own the switch. No heartbeat for 10 s leaves the mode, as do
  `MODE_PROJECTION`, `CT_STOP` or a `CT_SUBSCRIBE`. The box always boots into projection; neither the mode
  nor the AP credentials are persisted. Wire detail: [00_PROTOCOL.md](00_PROTOCOL.md) *AirPlay-only mode*.
  Implemented and VERIFIED with a real iPhone (§3).
- **Box minimum, host decodes.** Same committed model as CarPlay
  ([00_PROTOCOL.md](00_PROTOCOL.md), Media transport): the box does discovery, pairing, MFi auth-setup,
  the RTSP control channel and timing, then forwards the still-encrypted media plus the per-stream key;
  the host decrypts, decodes and renders.

## 2. Authorization and licensing

The owner's scope decision is [../ops/07_AUTHORIZATION.md](../ops/07_AUTHORIZATION.md) (AirPlay-only
mode). In short: MFi `auth-setup` is answered by the genuine on-board coprocessor through the existing
`crates/vendor/mfi` code and chip lock (the key never leaves the chip); **FairPlay is not used, answered
or analyzed** (`fp-setup` is refused and was never requested); the macOS AirPlay receiver was studied at
interface level only. **Whether the owner's MFi license covers an AirPlay receiver is unconfirmed** with
Apple (open item). Some open-source references carry Apple key blobs or FairPlay code (§G of the
reference index): those parts are never read or copied.

## 3. Verified vs open

| Area | State |
|---|---|
| Discovery → transient pair-setup → MFi auth-setup → encrypted control channel | **VERIFIED** (B7 variant) |
| Type 110 screen frames authenticate under the receiver's existing key/nonce rule | **VERIFIED** 5937/5937 and 1584/1584 (two mirroring sessions; 89/89 earlier) |
| Type 103 buffered audio authenticates with `shk` | **VERIFIED** 14200/14200 (393/393 earlier) |
| Type 96 realtime audio authenticates with the **same per-stream HKDF rule as type 110** (no `shk`) | **VERIFIED** 20504/20504 and 14111/14111 |
| Offline decode, end to end (box capture → decrypt → decode) | **VERIFIED**: H.264 5937 and 1584 frames decode with no error; AAC-LC from 103 and 96 decodes to non-silent audio (§4.6) |
| Sessions of minutes, ended by the user | **VERIFIED** 187 s, 233 s and 269 s, no timeout, the receiver silent on NTP/PTP (§4.9) |
| A type 96 stream comes and goes with audio inside a mirroring session, each time with a new key | **VERIFIED** (§4.6) |
| Tunnel over OCBM, real iPhone → box → `ocbm-host airplay` | **VERIFIED**: video 4421/4421 frames decrypted, 0 failed, H.264 High 498 × 1080; audio 3355 + 115 packets, 0 failed; ended by `TEARDOWN`, the box returned to projection |
| macOS AirPlay Test Host, real iPhone and box, **mirroring** | **VERIFIED live**: link open, `box_caps` 0x7f (`CAP_AIRPLAY` set), bundle `ok`, AP up, session streaming; video 3382 decoded / 0 errors at 498 × 1080; type 96 audio 3701 decoded / 0 errors / 0 underruns; the phone's volume drives the app's output gain (−21 dB shown as 30 %); real picture and sound observed by the owner |
| macOS AirPlay Test Host, **Music only** (no mirroring) | **VERIFIED live**: a single type 103 stream, 9820 packets, 5044 decoded, 0 decode / 0 undecryptable errors, playing; title and artwork arrived (they did not during mirroring); −15 dB shown as 50 %; 2 underruns in one snapshot (cause not isolated; 103 arrives ahead of real time, so the backlog is large) |
| Video smoothness in the test host | owner-observed (earlier build): some sluggish video. Phone side (VERIFIED, `fvdp_statsReport`, 161 s of one mirroring session): submits 20.7/s average (0–31), encodes 22.4/s, 9 encoder drops, 0 queue drops, 41 of 161 s under 20 encodes — iOS encodes on screen change, so a still screen gives a low rate. The earlier readout `video.fps: 0` could not be reproduced; best explanation (INFERRED): it covered only the 0.5 s between status polls, and iOS sends no frames on a still screen. The current build reads a 2 s moving rate, the average, the rate by header timestamps, the age of the last picture and inter-frame jitter against the header timestamps, and has optional timestamp pacing ("Smooth playback", off by default, 100 ms buffer, not remembered between launches) (§9). **Offline-verified only; no live run with this build; the new readout is a different measurement and is not confirmed live. Pacing evaluation OPEN** |
| A/V sync in the test host | **absent** (no playout clock tied to the phone's) |
| Whether the receiver must follow PTP / answer NTP for longer sessions or for sync | **OPEN** (not needed for 4.5 min) |
| HEVC, phone side | **VERIFIED on the wire** (probe variants, 2026-11-12): the phone sends HEVC Main only when bit 42 **and** a larger-than-1080p canvas (3840 × 2160) are advertised; bit 42 at 1920 × 1080, or a 4K canvas without bit 42, stays H.264 (§4.10). HEVC size threshold, HDR keys and sustained load OPEN |
| HEVC, box and protocol (commit 78867b3) | **Implemented**: mode request TLV `0x06` (bit 0 = feature bit 42), validated display, `ocbm-host airplay --hevc`. **VERIFIED on the box with a synthetic HEVC sender** (RAM overlay): hevc Main 994 × 2160, 150 frames, frame checksums identical to the source; with `--hevc` the box advertised bit 42 and 3840 × 2160 `maxFPS 60` in `/info` (box-side `wget`), without it neither. Offline `capcheck` on the real 4K captures: hevc Main 994 × 2160, 485 frames decode; B7 still h264 High 498 × 1080, 1584 frames. **NOT verified:** a real iPhone with `--hevc`, with the test host, and the mDNS record as seen from the Mac (§4.10 risk, §9) |
| HEVC, macOS test host | decodes H.264 and HEVC (VideoToolbox, hardware decoder reported; codec detected per stream from the opcode-1 body). **Offline only**: 303 tests in AirPlayTestHost and 1530 in MacHost pass, libx265 994 × 2160 60/60 frames decoded in hardware, synthetic-box screenshot. **No live iPhone/box run with this build** |
| Larger canvas / frame rate | **VERIFIED** (§4.10): the phone fits its screen into the advertised canvas height (994 × 2160 at 3840 × 2160, 332 × 720 at 1280 × 720) and follows `maxFPS` (60 and 120 both ~56 fps, 30 → ~28–30) |
| Protected/DRM video (§4.6 blanking), apps other than Music | **OPEN** |
| Remote control from the head unit, HID/touch, keyframe requests, host → phone volume | **OPEN** (not implemented) |
| Several sessions in a row / switching audio ↔ mirroring, Wi-Fi coexistence, MFi 3.0 chips | **OPEN** |
| MFi license coverage for an AirPlay receiver | **OPEN** (owner, §2) |

## 4. The measured protocol

### 4.1 Discovery (`_airplay._tcp`) — what is necessary, what is sufficient

**Sufficient (VERIFIED):** variant B7 — `features=0x445F8A80,0x1C340` (bits 7, 9, 11, 15–20, 22, 26, 30, 38,
40, 41, 46, 47, 48), `srcvers=366.0`, `protovers=1.1`, `flags=0x4`, and the AirPlay-2 TXT keys `deviceid`,
`model`, `pi`, `pk` (Ed25519 public key), `vv=2`, `acl=0`, `rsf=0x0`, `gid`, `psi`, `gcgl=0`, `igl=0`, `fex`.
It carried both Music audio and screen mirroring.

| Bit | Name (macOS sender's own feature check; community name in brackets) | Finding |
|---|---|---|
| 40 | `SupportsBufferedAudio` | **NECESSARY (VERIFIED).** Every variant without it that the iPhone log covers (M, Mm, A48, A48n, C0; the earlier variant A showed the same `OPTIONS`-only wire pattern) got an `APSenderSessionLegacyAudio` on the iPhone: one `OPTIONS`, a local "activation succeeded" in 20–90 ms with no network round trip, then `sdpsession_setupStreamCreatingResponseInternal: false condition` → `kAPSenderSessionError_UnsupportedStreamType` for the screen stream. Every variant with it got `APSenderSessionAirPlay`. The macOS sender's `isLegacyAirPlaySpeaker` is `srcvers < threshold OR (not AppleTV AND not bit 40)` (INFERRED to match iOS) |
| 26 | [MFi authentication] | **NECESSARY without another auth bit (VERIFIED).** Bn7 (= B7 − 26): transient pair-setup completes, then `No supported authentication types` → `kAPEndpointDescriptionError_Unsupported`. A 403 to `auth-setup` ends activation (`mfiAuthentication_AuthenticateEndpoint … Forbidden`) |
| 48 | `SupportsTransientPairing` | used: pair-setup M1 carries `Flags=0x10` (transient). Necessity not isolated (INFERRED) |
| 7 | `SupportsAirPlayScreen` | screen mirroring offered. Necessity not isolated (B without 7 was never chosen) |
| 15–17 | [metadata features] | artwork, DMAP and progress were sent (§4.8). Link INFERRED |
| 11 | [audio redundant] | type 96 SETUP carried `redundantAudio: 2`. Link INFERRED |
| 41 | `SupportsExtendedWHA` per the macOS sender [community: `SupportsPTP`] | iPhone logged `Receiver supports PTP: yes`; which input drives it is not isolated |
| 38 | conflicting labels ([`SupportsUnifiedMediaControl`] vs the probe's "CoreUtilsPairingAndEncryption") | unresolved; keep it set |
| 14 | FairPlay | never advertised, never asked for |

Bit 42 (`SupportsScreenMultiCodec`) is **not** in B7 or in `airplayd`'s base `FEATURES`; `airplayd` adds it only when the mode request carries the HEVC flag (§4.10), and probe variants that set it are what produced HEVC. Not set and so not exercised: 51 (`SupportsUnifiedPairingAndMFi`),
52 (`SupportsSetPeersExtendedMessage`: the phone sent the legacy `SETPEERS`). `srcvers=366.0` passes the
legacy threshold; `320.17` was only tried without bit 40, so it is untested. A variant without `pk` (C0) is
logged `Ignoring device found without pairing ID` yet was still offered for mirroring (legacy session).
Community bit tables mislabel bit 48 as CoreUtils pairing; the macOS sender maps 48 to transient pairing.

### 4.2 Control sequence (VERIFIED, B7)

Mirroring: `GET /info` (plain, body `{qualifier:[txtAirPlay]}`; reply carries `txtAirPlay`) → `POST
/pair-setup` M1 (`X-Apple-HKP: 4`, Method 0, Flags 0x10) → M3 (SRP proof; PIN `3939`, 3072-bit group) →
**the control channel is encrypted from here on with keys from the SRP session key** → `POST /auth-setup`
→ `SETUP` (session, NTP) → event TCP connect → `GET /info` (encrypted) → `GET_PARAMETER volume` →
`RECORD` → `SETUP` type 110 → `SET_PARAMETER volume` → `SETUP` type 96 → `POST /feedback` every ~2 s
(iPhone: keep-alive "Standard", interval 2017 ms) → `POST /audioMode`.

Audio (Music): the same up to `auth-setup`, then `SETUP` (session, PTP) → event connect → PTP to UDP
319/320 → `GET /info` → `GET_PARAMETER volume` → `RECORD` → `SETPEERS` (plist array of the phone's own
addresses) → `POST /command updateMRSupportedCommands` (twice, again later) → `SETUP` type 103 → `POST /audioMode` →
`SETRATEANCHORTIME` → metadata `SET_PARAMETER`s → `/feedback` → `TEARDOWN`s.

No `pair-verify` (transient pairing only), no `fp-setup`, no `FLUSH`/`PAUSE`/`ANNOUNCE` in any session.
`RECORD`'s reply carried `Audio-Latency: 0`.

Extended run, whole-session request counts (VERIFIED): **Music 187 s** — `GET /info` ×2, `pair-setup` ×2,
`auth-setup` ×1, `SETUP` ×2, `GET_PARAMETER` ×1, `RECORD` ×1, `SETPEERS` ×1, `POST /command` ×4,
`POST /audioMode` ×2, `SETRATEANCHORTIME` ×3 (rate 1, then 0, then 1 around a track change),
`FLUSHBUFFERED` ×1 (`flushUntilSeq`, `flushUntilTS`), `SET_PARAMETER` ×13, `/feedback` ×91 (~2 s),
`TEARDOWN` ×2. **Mirroring 233 s** — `GET /info` ×2, `pair-setup` ×2, `auth-setup` ×1, `SETUP` ×4
(session, 110, 96, a second 96), `GET_PARAMETER` ×1, `RECORD` ×1, `SET_PARAMETER` ×3 (volume),
`POST /audioMode` ×1, `/feedback` ×114, `TEARDOWN` ×3. **Mirroring 269 s** (first-party iOS UI) — 151
requests: `GET /info` ×2, `pair-setup` ×2, `auth-setup` ×1, `SETUP` ×3 (session at +2 s, type 110 at +2 s,
type 96 at +61 s), `GET_PARAMETER` ×1, `RECORD` ×1, `SET_PARAMETER` ×3, `POST /audioMode` ×4, `/feedback`
×132, `TEARDOWN` ×2. A port census on the box's Wi-Fi interface saw
the phone touch only the advertised ports (control, event, data, UDP control) plus UDP 5353 (mDNS) and
319/320 (PTP): zero attempts on any port nothing listened on in the Music and first mirroring sessions. In the
269-s session the census saw three UDP packets from the phone's port 16403 to box ports 16403, 3492 and 3485
(nothing listened; ~+118 s). The `airplayd` log shows nothing at that moment, so another iOS service is the
likely sender (INFERRED); unexplained, OPEN.

### 4.3 Pairing and MFi (VERIFIED)

- Transient pair-setup is SRP-6a only (no Ed25519 exchange, no M5/M6); the 64-byte SRP session key is the
  pairing secret for the control channel and for the stream keys below. `pk` is advertised but unused in
  this flow.
- `auth-setup`: request 33 B = `0x01` + 32-byte Curve25519 public key; reply 1113 B = 32-byte public key ‖
  `[u32 BE 945][certificate]` ‖ `[u32 BE 128][signature]`, produced by `mfi::sap::MfiSapServer` with the
  chip. The iPhone validated the CCPA's MFi 2.0 certificate (`authVersion: [V2]`, `certStatus: [Valid]`).
  A `MessageSecurity` "unable to decode content info (896 bytes)" error in the same second is benign: it
  precedes a valid status in successful sessions too.
- Chip access is flaky at start: one probe start logged `MFi self-test FAILED` and recovered on restart, and
  the extended run's self-test failed its first of three attempts (`chip NAK/timeout`). Cause OPEN (lock
  contention suspected). The mode switch must own the chip lock cleanly and retry.

### 4.4 SETUP, phase 1 (session)

| | Mirroring | Audio (Music) |
|---|---|---|
| `timingProtocol` | `NTP` + `timingPort` (the iPhone's own NTP server port) | `PTP` + `timingPeerInfo` / `timingPeerList` (`ID`, `ClockID`, `DeviceType 0`, `SupportsClockPortMatchingOverride`) + `asyncPTPClockConfig` |
| other keys | `isScreenMirroringSession: true`, `isMultiSelectAirPlay: false`, `combinedGetInfoWithControlSetup: true`, `statsCollectionEnabled`, `updateSessionRequest`, `diagnosticsAndUsage`, `sessionUUID`, `sessionCorrelationUUID`, `deviceID`, `macAddress`, `model`, `name`, `osName`, `osVersion`, `osBuildVersion`, `sourceVersion`, `internalBuild` | the same identity keys, plus `isMultiSelectAirPlay: true`, `groupUUID`, `groupContainsGroupLeader`, `supportsGroupCohesion`, `senderSupportsRelay` |
| absent | `ekey`, `eiv`, `et`, `shk` (no FairPlay key material anywhere) | same |
| receiver reply that worked | `eventPort`, `timingPort` (receiver's UDP port) | `eventPort`, `timingPort: 0`, `timingPeerInfo {Addresses:[box IP], ID}` |

`macAddress` changes between sessions (private address); identify the phone by `deviceID`/`pi`, not by it.

### 4.5 SETUP, phase 2 (streams)

| Key | type 110 (screen) | type 96 (realtime audio, with mirroring) | type 103 (buffered audio) |
|---|---|---|---|
| `streamConnectionID` | yes (may arrive as a **negative** plist integer: the salt uses its unsigned 64-bit decimal form — VERIFIED on a type 96 stream whose SETUP carried a negative value) | yes | yes |
| codec | — | `ct 4`, `audioFormat 0x400000` (bit 22), `sr 44100`, `spf 1024` | `ct 4`, `audioFormat 0x400000`, `audioFormatIndex 22`, `spf 1024` |
| meaning | H.264 (§4.6) | **AAC-LC 44.1 kHz stereo** (bit 22 per [../carplay/06_AV_PIPELINE.md](../carplay/06_AV_PIPELINE.md) audio-format enum; the iPhone logged `AAC-LC/44100/2`, 1024 samples/packet) | **AAC-LC 44.1 kHz stereo** (iPhone: `supportedAudioFormats=[AAC-LC/44100/2]`; RTP SSRC `0x16000000` = index 22) |
| latency | `latencyMs 100`, `timestampInfo` names `SubSu BePxT AfPxT BefEn EmEnc` | `latencyMin 0`, `latencyMax 7497` samples (= 170 ms; iPhone: `audio latency 170 ms`), `redundantAudio 2` (iPhone: `redundancy count 2`) | — |
| other | — | `controlPort` (the phone's), `usingScreen`, `isMedia`, `audioMode`, `supportsDynamicStreamID` | `clientID` (`com.apple.Music`), `shk` (32 B), `isMedia`, `audioMode`, `supportsDynamicStreamID` |
| receiver reply | `type`, `dataPort` (TCP) | `type`, `dataPort`, `controlPort` (UDP) | `type`, `dataPort` (TCP), `controlPort` (UDP), `audioBufferSize` |

`ct` values per the references: 1 PCM, 2 ALAC, 4 AAC-LC, 8 AAC-ELD (INFERRED; 4 = AAC-LC VERIFIED here).
**Type 103 reply is strict (VERIFIED):** a reply with only `type` + `dataPort` made the iPhone fail
`audioStream_setupAudioStream … kNotFoundErr` and `TEARDOWN` the stream within 10 ms; adding
`audioBufferSize` (8 MiB) and `controlPort` fixed it. Which of the two is required is not isolated.

### 4.6 Packet layouts and keys

**Screen, type 110 (TCP).** Each frame = 128-byte header ‖ body. Header (little-endian): `bodySize u32 @0`,
`opcode u8 @4`, flags `@5` (`0x10` on an IDR frame), `@6..8` `0x0116` on the codec frame, timestamp
`@8..16` = 32.32 fixed-point seconds on the phone's network clock (≈ phone uptime, the same timeline as
PTP `networkTimeSecs`). Codec frame (opcode 1) floats: source `498.0 × 1080.0` @16/@20 and @40/@44, x
offset `711.0` @48 (= (1920 − 498)/2), output `498 × 1080` @56/@60. VERIFIED.
- Opcode 0 = video frame: ChaCha20-Poly1305, key = HKDF-SHA512(pairing secret, salt
  `"DataStream-Salt" + scid`, info `"DataStream-Output-Encryption-Key"`), AAD = the 128-byte header, nonce
  = 4 zero bytes ‖ u64 LE count of opcode-0 frames. **VERIFIED** — the same rule as CarPlay
  (`receiver::session::spawn_screen`).
- Opcode 1 = codec configuration: one 37-byte **plaintext avcC** at stream start (profile 100 = High,
  level 31, NAL length size 4, one 18-byte SPS + one 4-byte PPS), not counted in the nonce. VERIFIED.
- Opcode 5 = plaintext **binary-plist statistics**, about one per second (230 in 233 s), not counted in the
  nonce. Key names readable in the capture include `rttAvg`, `txUsageAvg`, `encoderCurrentFPS`,
  `sinkOverflowDropFPS`; the rest were not decoded. Safe to ignore for display.
- Histograms: 6168 frames = opcode 0 ×5937, 1 ×1, 5 ×230 (233-s session); 1640 = 1584, 1, 55 (first 55 s of
  the 269-s session). Each session had only **one IDR**, at its start (230 s and 55 s captured): a decoder
  that misses the start or loses a frame has no keyframe to recover from unless the receiver can ask for
  one (no AirPlay keyframe request is known; OPEN).
- Video parameters (VERIFIED): H.264 High L3.1, 498 × 1080 (portrait phone fitted into the advertised 1920 ×
  1080 display, x offset 711), 25.74 fps average by header timestamps over 230.6 s (the phone's encoder is
  set to 30 fps from the advertised `maxFPS 30`; it skips frames when the screen is static), target 8 Mbit/s,
  `supports rotation: no`. All 5937 frames decode with ffmpeg without error; in the 269-s session (first-party
  iOS UI) the captured 1584 frames decode cleanly at 28.6 fps average. Without `/info` (legacy) the
  phone defaults to 1280 × 720 at 60 fps. HEVC was not offered (bit 42 absent; with it and a 4K canvas the phone sends HEVC, §4.10).
- Picture content (VERIFIED visually on a decoded frame near the start: the phone's real screen, Apple Music,
  at 498 × 1080). For most of the session after ~35 s the decoded frames are near-black (luma max 5–10),
  with a few bright frames at the start and end; the owner was playing an Apple Music **music video**.
  Contrast (VERIFIED): in the 269-s session, mirroring first-party iOS UI, every 5-s sample of the captured
  first 55 s had luma max 255 and no frame was black. So the blanking follows the content, consistent with
  the phone withholding protected (FairPlay/DRM) video from a receiver that cannot attest HDCP-like
  protection. Still INFERRED: not isolated (no protected and unprotected video compared in one session, and
  the unprotected run's capture stopped at 55 s).

**Buffered audio, type 103 (TCP).** `[u16 BE length, counting itself][RTP 12][ciphertext][tag 16][nonce 8]`.
RTP byte 0 `0x80`, bytes 1–3 a 24-bit sequence number, timestamp +1024 per packet, SSRC `0x16000000`.
Key = `shk` from the stream SETUP, AAD = RTP bytes 4..12. **VERIFIED** (14200/14200). Payload = **raw AAC-LC
access units, no ADTS**; 1 copy per packet (no redundancy); two SSRC values across a track change. The
187-s session delivered **329.7 s** of audio (buffered audio runs ahead of real time), non-silent, peak
−1.8 dBFS; one AU failed to decode (a 0-byte AU). Silence frames are 6-byte AUs.

**Realtime audio, type 96 (UDP).** `[RTP 12][ciphertext][tag 16][nonce 8]`, PT 96, 16-bit sequence, SSRC 0;
the first packets were 42 B (6-byte AU). **Every packet is sent three times** (`redundantAudio 2`): 20504
captured = 6835 unique by sequence number, exactly 3.00 copies each — deduplicate by sequence. VERIFIED.
**Key (VERIFIED 20504/20504):** no `shk`; HKDF-SHA512(pairing secret, `"DataStream-Salt" + scid`,
`"DataStream-Output-Encryption-Key"`) — the same per-stream rule as type 110 and as CarPlay realtime audio
(`receiver::stream`), AAD = RTP 4..12. Payload = raw AAC-LC AUs (median 579 B), 158.7 s decoded,
non-silent.
**A type 96 stream exists only while the phone plays audio (VERIFIED, both logs).** A mirroring session
starts with **no** audio stream: in the 269-s session (first-party iOS UI) type 110 was set up at +2 s and the
only type 96 `SETUP` came at +61 s, when the owner started Music (iPhone: `Registering streamType MainScreen`
02:22:02, `Registering streamType GeneralAudio` 02:23:00, `Unregistering … GeneralAudio` 02:26:28, then
`Deactivating endpoint … 'UserRoutedAway'`). That stream (14111/14111 packets authenticate, 3.00 copies
each, 109.3 s of non-silent audio decoded) had its own `streamConnectionID` and therefore its own key. The
host must accept a type 96 stream appearing and disappearing at any point of a mirroring session, with a
fresh key each time.

**Mid-session re-setup (VERIFIED, both logs):** in the 233-s mirroring session the phone suspended its
screen-audio engine, sent `TEARDOWN {streams:[{type 96, streamID 0, ccCount…, ioDiscontinuityCount,
primaryPacketDropCount}]}` at +34 s (iPhone: `Unregistering streamType GeneralAudio`), and ~10 s later
(+44 s) sent a **new type 96 `SETUP` with a new `streamConnectionID`** (so a new key and new ports;
iPhone: `Registering streamType GeneralAudio … connectionID <new>`). The host must accept a second
type-96 key mid-session and switch at that point. The audio-start/stop behaviour above explains this too
(INFERRED). **Likely cause (INFERRED, not isolated):** the owner was running Music (audio) and then started a music video during the mirror; the iPhone log shows a buffered-AirPlay client unregistering (`FigBufferedAirPlayClientRoutingRegistry`, client count 0) about 0.5 s before the screen-audio suspend, and the new type 96 stream appears when the video's own audio starts. The same moment (~+35 s) is when the decoded frames turn near-black; a run with unprotected content and a steady audio source would separate these causes. The session's final `TEARDOWN` of type 96 came before
`SET_PARAMETER volume -144` (mute), `volume -20`, and the session `TEARDOWN {}`.
Control port: 20-byte sync packets about once per second, `80 d4 00 04` (`90 d4` on the first): `[rtpTime −
latencyMax u32][network time 32.32][rtpTime u32]` — the arithmetic checks against `latencyMax 7497` on all
three logged packets. VERIFIED. iPhone: `Total retransmits: 0`.

**Event channel (TCP).** No byte flowed in either direction in any session, including the 187-s, 233-s
and 269-s ones. Keys exist for it
(`Events-Salt`, `Events-{Read,Write}-Encryption-Key`, as in CarPlay:
[../carplay/02_SESSION_LIFECYCLE.md](../carplay/02_SESSION_LIFECYCLE.md)). **It must stay open:** closing it
ends the session (§4.9).

### 4.7 Timing

- **Mirroring uses NTP, not PTP (VERIFIED)**, although the iPhone logged `Receiver supports PTP: yes,
  NTP forced: no`. The phone starts an NTP *server* and puts its port in `timingPort`; the receiver is the
  client. The box never queried it, and the 233-s and 269-s mirroring sessions ran to a user stop. In the
  269-s session the filtered `airplayd` log has no session-failure error at all (its 33 `<Error>` lines are
  the routine `endpoint_copyPropertyInternal` / `audioStream_CopyProperty` "false condition" kind seen in
  every run).
- **Audio uses PTP (VERIFIED).** The iPhone is the gPTP grandmaster (its `ClockID` from SETUP; `TimeSync`
  log: UDPv4 end-to-end port to the box, sync interval 2^-3 s): Sync (two-step) to UDP 319 at ~8 Hz,
  Follow_Up (802.1AS TLV), Announce and Signaling (Apple OUI `00:0D:93` TLV) to 320 — in the 187-s session
  Sync ×1476, Follow_Up ×1475, Announce ×736, Signaling ×736. The box sent nothing back. The iPhone's only
  complaint was `kAPSNetworkClockError_TimelineIDMismatch` twice in buffered-audio start-up diagnostics
  (`bufferedAudioEngine_logStartUpTimeForHose`); the session played on to a user stop. `SETRATEANCHORTIME` gave
  `rate 1`, `networkTimeTimelineID` (= ClockID), `networkTimeSecs/Frac`, `rtpTime`. nqptp, the reference
  PTP companion, sends Announce "pings" to the grandmaster; whether iOS needs anything back for long
  sessions or for sync on a real playout is OPEN.

### 4.8 Volume, metadata, commands (VERIFIED unless marked)

- `GET_PARAMETER` `volume` (text/parameters) → the reply `volume: -20.000000` became the phone's initial
  volume (`apsession_ensureInitialVolumeFetched: initialVolume = -20 dB`); the phone then `SET_PARAMETER`s
  the same. Scale is dB, −30 = slider 0 (iPhone log). No user volume change was captured (OPEN).
- Audio sessions with bits 15–17: `SET_PARAMETER` `image/jpeg` artwork (~180 KB, re-sent on a track
  change), `application/x-dmap-tagged` track info (`mlit` with title `minm`, artist `asar`, album `asal`,
  duration), `text/parameters` `progress: start/current/end` (RTP times), each with `RTP-Info`. 18 metadata
  bodies in the 187-s session. Volume `-144` = mute (sent at mirroring teardown).
- `POST /command` `updateMRSupportedCommands`: the MediaRemote command list (38 entries). No
  `DACP-ID`/`Active-Remote` headers in AirPlay-2 sessions (only on legacy `OPTIONS`), so head-unit
  remote control does not use DACP here; its path is OPEN.
- `POST /audioMode {audioMode: default}`, `SETPEERS` with `Content-Type: /peer-list-changed`.
- `TEARDOWN {streams:[{streamID 0, type 103}]}` then `TEARDOWN {}` for the session.
- No HID/touch: AirPlay mirroring here is view-only; touch would need a HID path (OPEN, not designed).

### 4.9 How each session ended (VERIFIED from both logs)

| Session | End | Cause |
|---|---|---|
| Extended run: Music 187 s | `TEARDOWN` 103, `TEARDOWN {}`, control closed | user: `RemovedFromAggregate` |
| Extended run: mirroring 233 s | `TEARDOWN` 96, mute/volume, `TEARDOWN {}`, control closed | user: `UserRoutedAway` |
| Mirroring 269 s (first-party iOS UI) | `TEARDOWN` ×2, control closed | user: `UserRoutedAway` (type 96 unregistered first) |
| Mirroring ×2 | iPhone: `Connection … closed to <box>:<eventPort>` → `-72320` → endpoint failure `Disconnected` | **the probe's own 30-s idle read timeout on the event socket** closed it. Not a protocol finding: a receiver must keep the event connection open indefinitely |
| Music, first two tries | stream `TEARDOWN` 10 ms after SETUP; session idle until the event socket closed | incomplete type 103 reply (§4.5) |
| Music, working | `Deactivating endpoint … reason 'RemovedFromAggregate'`, then both `TEARDOWN`s | user removed the route on the phone |

With the probe's own timeouts removed, nothing ended a session except the user, for up to ~4.5 minutes with
the receiver never answering NTP or following PTP.

### 4.10 Display canvas, resolution and HEVC

- **Canvas (VERIFIED from both logs).** The receiver advertises a display canvas in `/info` `displays`
  (`airplayd` default 1920 × 1080 at 30; the host can set it with `AIRPLAY_REQ_DISPLAY`: exactly 5 bytes, 320..7680 per side,
  1..120 fps, else `MODE_ACK` status 1; the range was 16..8192 before 78867b3). The iPhone logs `endpoint display info … 1920.0 x 1080.0`, fits its own screen into that
  canvas, and encodes only the content: 498 × 1080 for a portrait phone. It tells the receiver the content
  size and placement in the opcode-1 codec frame floats (source 498 × 1080, x offset 711 = (1920 − 498)/2).
  The receiver places or scales that picture in its own window.
- **`displays` keys the sender reads (VERIFIED, `endpoint display info` log line):** name, `widthPixels`,
  `heightPixels`, `widthPixelsMax`, `heightPixelsMax`, `widthPhysical`, `heightPhysical`, `uuid`, `edid`, HDR,
  **`maxFPS` (not `refreshRate`)**, `overscanned`, `rotation`, receiver-supports-444, third-party-TV and
  `features`. The macOS sender itself emits width/height/widthPixels/heightPixels/widthPixelsMax/
  heightPixelsMax/widthPhysical/heightPhysical/uuid/overscanned/maxFPS/rotation/receiverSupports444/HDRInfo
  (INFERRED, interface level). The value formats of `HDRInfo` and `receiverSupports444` /
  `receiverSupportsHDRMirroring` are unknown and were not sent (the log shows `HDR: no`, `444: no`).
  `airplayd` emits (VERIFIED in `build_info`) `uuid`, width/height/widthPixels/heightPixels, physical 0,
  `features 14`, `maxFPS`, `refreshRate` (1/fps; the default 1920 × 1080 at 30 keeps the older 1/60),
  `overscanned` and `rotation`; no `*Max` keys, no HDR/444 keys.
- **Larger-display / HEVC test (VERIFIED, 2026-11-12).** Probe variants with a spec-driven `displays`
  dictionary; every run authenticated under the existing key rule (0 failures). Runs were 7–45 s (the owner
  stopped early), so bitrate and fps are rough.

  | Variant | Bit 42 | Canvas (`maxFPS`) | Codec | Picture, x offset | Frames, fps, bitrate |
  |---|---|---|---|---|---|
  | H7, H7r (earlier) | yes | 1920 × 1080 (30) | H.264 High | 498 × 1080 | 579 / 870 frames, 25.6 / 29.4 fps |
  | D4k60 | yes | 3840 × 2160 (60) | **HEVC Main** | 994 × 2160, 1423 | 485 in 8.5 s, 56.8 fps, 5897 kbit/s |
  | D4k120 | yes | 3840 × 2160 (120) | HEVC (`hvc1`; not decoded) | 994 × 2160, 1423 | 395 in 7.0 s, 56.5 fps, 7183 kbit/s |
  | D4k60m | yes | 3840 × 2160 (60) + `widthPixelsMax`/`heightPixelsMax` | HEVC (`hvc1`; not decoded) | 994 × 2160, 1423 | 677 in 13.6 s, 49.8 fps, 3672 kbit/s |
  | D4kn | no | 3840 × 2160 (60) | H.264 High L4.2 (avcC) | 994 × 2160, 1423 | 524 in 17.4 s, 30.1 fps, 2829 kbit/s; decodes cleanly |
  | D720 | no | 1280 × 720 (30) | H.264 High | 332 × 720, 474 = (1280 − 332)/2 | 1295 in 45.4 s, 28.5 fps, 1005 kbit/s; decodes cleanly |

  The iPhone log shows `VTCompressionSession … codec hvc1` for D4k60, D4k120 and D4k60m and `avc1` for D4kn
  and D720 (H7, H7r and B7: `avc1` each time, earlier run).
- **HEVC wire format (VERIFIED on D4k60).** The opcode-1 body is **not** an avcC: it is an `hvc1`
  sample-entry box (229 B, starting `00 00 00 e5 'hvc1'`) wrapping an `hvcC` (VPS 24 B, SPS 44 B, PPS 7 B,
  NAL length size 4). Prepending those three parameter sets (Annex-B) to the decrypted frames decodes with
  ffmpeg as HEVC Main 994 × 2160, 485 frames. Header, key rule, nonce and plaintext opcode-5 statistics are
  unchanged. The `hvcC` is found by searching the body for `hvcC`.
- **Conclusion.** This iPhone chose HEVC only when bit 42 **and** a larger-than-1080p canvas were advertised.
  Bit 42 at 1920 × 1080 (H7, H7r) and a 4K canvas without bit 42 (D4kn) both stayed H.264. The encoded width
  follows the phone's aspect at the canvas height. The frame rate follows `maxFPS`: 60 and 120 both gave ~56
  fps (a ~60 fps cap), 30 gave ~28–30.
- **Not isolated:** why D4k60m ran lower (49.8 fps; content-driven, since iOS encodes on screen change, is
  possible but unchecked); why D4kn ran 30 fps while advertising 60 (the phone log shows its encoder frame
  rate set to 60 at connect, then 30 about 5 s later, then 60 again: a phone-side adjustment, cause
  unknown); the HEVC size threshold (only 1280 × 720, 1920 × 1080 and 3840 × 2160 were tried); whether a
  1080p canvas with bit 42 plus other keys ever yields HEVC; HDR; sustained load.
- **Box load, 4K HEVC run (D4k60; the probe's HEALTH lines, short run).** CPU busy ~23–24 % with the probe
  ~7–8 % and softirq 4–5 % while streaming (idle-ish samples of ~7 % later), load average ~0.1–0.16, memory
  available ~98–99 MB, Wi-Fi `txfailed` 0. Sustained load was not measured; Wi-Fi retry counters are
  unavailable (this driver's `iw` station dump has no tx retries).
- **Implemented (commit 78867b3; code VERIFIED, bench status in §3).** The mode request's TLV `0x06`
  `[flags u8]`: bit 0 = HEVC = feature bit 42. The box reads the first byte only, ignores unknown bits, treats
  an empty value as 0; a request without the TLV is byte-identical to the old layout. `airplayd` then
  advertises bit 42 in the TXT `features`, `/info` `features` and `fex` together. The display is validated
  as above. `MGMT_INFO` `airplay` gains `hevc` and `display` ("WxH@F"; a missing key means an older `ocbmd`).
  The box forwards the opcode-1 body verbatim (any codec; cap `MAX_FRAME` 8 MiB), so the tunnel is unchanged.
  `ocbm-host airplay --hevc --display WxH@F` writes `video.h264` or `video.h265` by the stream's own
  configuration frame and prints the codec; `box/airplayd/examples/fake_sender.rs` sends `.h265` hvc1-wrapped.
- **Open decision / risk (INFERRED).** `airplayd` keeps one receiver identity across HEVC on and off. If iOS
  caches the feature set per identity, a phone that saw the receiver without bit 42 might stay on H.264 after
  the host turns HEVC on (the probe used a fresh identity per feature set, which is why it did not meet this).
  Not tested with a real iPhone.
- **Policy mismatch (decision to align later).** The box ignores reserved flag bits (forward compatibility);
  the test host refuses a request whose flags TLV has a reserved bit or is not exactly 1 byte
  (`AirPlayModeWire.swift`).
- A 4K stream means more bytes over the box Wi-Fi and USB (~6–7 Mbit/s in the short runs) and a heavier Mac
  decode (unmeasured sustained).
- **Tooling (fixed in 78867b3).** `capcheck` / `pull_and_check.sh` now read the `hvc1`-wrapped `hvcC` through
  `ocbm_proto::seam::video_config`, label the codec from the configuration frame, and say "NOT decoded" when
  none was captured. Offline check on the real 4K captures: hevc Main 994 × 2160, 485 frames decode
  (reported by the implementer; not re-run for this doc).

## 5. Box/host split

As implemented (2026-11-12). "Not yet" marks the parts of the design that are not built.

| Box (`box/airplayd`, started by `ocbmd` / `airplay_tick` instead of carplayd/iap2d/btd) | Host (`ocbm-host airplay`; macOS AirPlay Test Host) |
|---|---|
| mDNS `_airplay._tcp` with the §4.1 set; the AP through the radio seam with the host's SSID/passphrase | Mode request (TLVs), heartbeat, state display |
| RTSP server, `/info` with the host's display, transient pair-setup only, `auth-setup` through `mfi::sap` and the chip lock; `fp-setup` refused | Decrypt with the forwarded per-stream keys; H.264 and raw AAC-LC decode; render and play |
| SETUP replies, event socket held open, `/feedback`, `GET/SET_PARAMETER`, `/audioMode`, `SETPEERS`, `FLUSHBUFFERED`, `TEARDOWN` | Phone volume → app output gain (VERIFIED). Host → phone volume: not yet |
| Binds the timing ports and answers nothing. NTP client / PTP follower and clock samples to the host: not yet | A/V sync and a playout clock tied to the phone's: not yet |
| Advertises B7, plus bit 42 only when the request's HEVC flag is set; the display from the request (§4.10) | Decodes or writes H.264 and HEVC: the test host through VideoToolbox, `ocbm-host airplay` as `video.h264` / `video.h265` (§9) |
| Forwards type 110 on `CH_VIDEO`, types 96 (first copy of each sequence) and 103 on `CH_MEDIA_AUDIO`, each with its key and format; events, metadata and artwork on `CH_METADATA` | Track, artwork, progress and volume display |

The pairing secret never leaves the box; the box never decrypts media.

## 6. Phased plan

| Phase | Deliverable | Exit test |
|---|---|---|
| **State 2026-11-12** | P1, P2 and P4 built and VERIFIED live with a real iPhone through the test host (§3); P5 partly (phone → host volume, track, artwork, progress); P3 not started | — |
| P1 protocol + mode switch | `MODE_AIRPLAY 0x02` in `ocbm-proto` and `ocbmd` (stop CarPlay/BT advertising, own the chip lock and the AP, start the AirPlay daemon; an acknowledgement: `CT_MODE_ACK`, built); daemon with §4.1–§4.6 control path; event socket held open | phone selects the box, sessions survive ≥ 10 min idle |
| P2 audio | type 103 forward + `shk` handoff, type 96 forward + HKDF key handoff (dedup ×3, re-key on a second SETUP); host raw-AAC-LC decode (offline decode already VERIFIED) | Music and mirroring audio play live in a host app |
| P3 timing | NTP client for mirroring, PTP follower for audio, clock samples to the host | A/V offset measured, no drift over 30 min |
| P4 mirroring | type 110 forward over `CH_VIDEO`; host decode at the advertised size (offline decode VERIFIED); display size from the host; a keyframe strategy (one IDR per session today) | measured frame rate and latency on the host, not a build; unprotected content shown |
| P5 metadata + volume | artwork/DMAP/progress lane, volume both ways, remote commands (path OPEN) | values shown in the host UI |
| Test app | a minimal host-side view (decode + numbers) before touching the shipping apps | **done**: `host/MacHost/AirPlayTestHost` ([../host/00_MACOS_HOST_APP.md](../host/00_MACOS_HOST_APP.md)) |

## 7. Running the probe (bench only)

`box/airplay-probe/` is standalone (empty `[workspace]`, outside `./build.sh` and the overlay). Build it for
the box with `CARGO_TARGET_DIR` outside the tree, copy the binary and `probe_ctl.sh` to the box's `/tmp`
(RAM; the script refuses to run unless `/tmp` and `/etc` are tmpfs), then:

1. `probe_ctl.sh ap-up <ssid> <passphrase>` (throwaway values, never recorded) — brings up the AP through
   the owned radio seam.
2. `probe_ctl.sh start B7/B+7 Bn7/B-26+7 …` — each SPEC is advertised as its own receiver on its own port.
3. Choose the receiver on the iPhone (Control Center audio or Screen Mirroring); collect the phone's side
   with `idevicesyslog -p airplayd`.
4. `probe_ctl.sh log` / `status`, then `probe_ctl.sh stop` and `ap-down`.
5. Off-box: build `capcheck` (`--features capcheck`) and run `pull_and_check.sh <TAG>` over USB-NCM (or
   `--analyze <dir>` on files already pulled): capcheck per stream, a decode test of the decrypted H.264 and
   AAC through ffmpeg (frames, errors, stills, duration, peak level), and the probe's per-session summaries
   (requests, streams, ports touched, metadata). Needs ffmpeg/ffprobe and python3. Captures and `.meta`
   files hold session keys: delete them when done.

The early probe builds had a 30-s event-socket timeout (§4.9), logged only three PTP messages per type and
did not capture type 96; the extended run used a build without those limits. The capture cap is still 12 MB
per stream: busy video fills it in about a minute (the 269-s session's type 110 capture stopped at +58 s), so
a long capture needs a rolling or larger buffer. The probe's counters (e.g. the PTP ones) are cumulative per
probe process, not per session. Check the pull's integrity line: in that pull one capture file's sha256 did
not match the box's (the packets in it still authenticated, but treat that file as possibly truncated).

## 8. Capturing the iPhone side

The phone's `airplayd` log (`idevicesyslog -p airplayd`) is the highest-yield evidence (§G of the reference
index) and also very noisy. Recipe used on 2026-11-12 (the helper scripts are bench tooling outside the repo):

1. Capture `idevicesyslog -p airplayd` raw for the whole run.
2. Filtered copy: keep `<Notice>`, `<Error>` and `<Fault>` lines only, and drop the noisy classes —
   CFPrefs lookups, XPC `get_property_data`, Network agent / `nw_path` chatter, `SCDynamicStore`,
   connection-statistics dumps, the aggregate-audio-format spam, `ztsController`, and the BAE
   `nowRemoteMediaTime` lines.
3. A second copy of every line mentioning the receiver's advertised name.

Then search the filtered copy for `Created APSenderSession`, `Registering streamType`, `Deactivating
endpoint … reason`, `false condition`, `got error`, and `Notifying about endpoint failure`. The raw log
contains the phone's UDID and addresses: keep it out of the repo.

## 9. Implementation notes (box, test host, incidents)

Box (`box/airplayd`, `ocbmd`, supervisor), from the code and the implementers' bench logs:

- **MFi chip wake-up.** The first chip transaction after idle NAKs and a retry succeeds, seen repeatedly
  (fits a chip that sleeps; INFERRED cause). `airplayd` retries a chip NAK up to 5 times with a growing pause
  (40, 80, 160, 320 ms) and does not retry a lock failure (`chip_retry`, VERIFIED in code). A signature takes
  about 1.7 s on this chip (implementer measurement).
- **Pairing.** Only transient pair-setup is accepted; a non-transient M1 is refused and `/pair-verify`
  answers 470 (VERIFIED in code). The receiver identity (Ed25519 seed, device id, `pi`) is
  `/etc/airplay_ident`, saved once per box to the persist area by `rc.ocbm` so the phone's cache of the
  receiver survives a reboot.
- **Supervisor ordering.** `airplay_tick` gates CarPlay bring-up, so the supervisor cannot raise CarPlay's
  AP over the AirPlay one. `radio_ap_up.sh` now rebuilds the `ssid=` line instead of `sed`-substituting it
  (a host SSID may contain `/` or `&`); this also runs for CarPlay (box-verified).
- **Flow control.** `CH_VIDEO` is lossless; `CH_MEDIA_AUDIO` is not gated, so a host more than 1 MiB behind
  loses frames, and type 103 arrives ahead of real time.
- **Feature set.** `FEATURES` in `main.rs` is the B7 constant (TXT `0x445F8A80,0x1C340`; a unit test pins it);
  bit 42 is OR-ed in per run from the request's HEVC flag (`ocbmd` writes `hevc=0|1` into the mode config,
  `airplayd` reads it or `--hevc`). The identity is the same either way (§4.10 risk).
- **Not built:** NTP/PTP answers, host → box volume, keyframe request, remote control.

macOS AirPlay Test Host (`host/MacHost/AirPlayTestHost`, its own Xcode project sharing `carlink_macOS`
sources; detail in [../host/00_MACOS_HOST_APP.md](../host/00_MACOS_HOST_APP.md)): deliberate replacements
`AirPlayModeClient` (the shipping `OCBMClient` sends `CT_SUBSCRIBE`, which would end the mode),
`MirrorVideoDecoder` (`VTDecompressionSession`, no frame shedding, no keyframe requests) and
`AirPlayAudioPlayer` (pull playout; type 103 arrives minutes ahead). It claims the USB interface only between
Start and Stop. **No A/V sync.** Working tree, not yet committed (VERIFIED by reading the code; behaviour
offline-verified only, no live run with this build):

- **Codec.** `MirrorVideoConfig.parse` reads the opcode-1 body: a bare avcC (H.264), or an `hvc1`/`hev1`
  sample entry whose `hvcC` child carries VPS/SPS/PPS (HEVC). A stream whose codec differs from the last one
  rebuilds the decoder; the status shows codec, profile/level and whether VideoToolbox gave a hardware decoder;
  a Mac with no HEVC decoder says so.
- **Controls.** Display (presets 1920 × 1080 at 30, 3840 × 2160 at 60 and 120) and "Allow HEVC" (remembered).
  Example request tail for 3840 × 2160 at 60 plus HEVC: `05 05 00 0F 70 08 3C 06 01 01` (no `06` TLV when HEVC is
  off). The host refuses a reserved flag bit or a flags TLV that is not 1 byte (§4.10 policy mismatch).
- **Readout.** Frame rate over a 2 s moving window, the run average, the rate by header timestamps, the age of
  the last picture, and RFC 3550 interarrival jitter of frame arrival against the header timestamps.
- **Smooth playback** (optional, default off, not remembered): each picture is held to its header timestamp
  re-based on the fastest arrival of the last 4 s, plus a 100 ms buffer (the phone's `latencyMs`); a late
  picture is shown at once and counted. Whether it helps is OPEN.
- **Offline results (reported by the implementer, tests not re-run for this doc):** 303 tests pass in
  AirPlayTestHost, 1530 in MacHost; libx265 994 × 2160, 60/60 frames decoded in hardware; a synthetic-box
  screenshot. At 4K the Mac decode load is unmeasured.

Incident (2026-11-12): every USB device dropped from the Mac mid-session; the box lost power and rebooted into
the previously **flashed** overlay, which lacked AirPlay mode, so the app reported no `CT_MODE_ACK` (the box
did not know `MODE_AIRPLAY`). Files replaced in `/opt/ocbm` at run time live in RAM and are lost at a reboot;
a build must be flashed to survive one. It was then flashed with `ocbm-overlay write` (33 erase blocks, a
2,132,664-byte image in a 4,587,520-byte region) and the box reports a clean bundle stamp.

## 10. Provenance

Where each kind of fact or code in this mode came from. Stated plainly, including what is unchecked.

- **(a) OCBM's own receiver crates.** Pairing, `rtsp`, `receiver` and `mfi` under `crates/vendor/` come from the
  project's archived `ncm_carplayd` tree (see [../README.md](../README.md)) and are reused by `box/airplayd`
  and the probe. First-party; the MFi path is the on-board coprocessor (§2).
- **(b) Measurements from a real iPhone.** Probe logs, the iPhone's own `airplayd` log, and offline decrypt and
  decode of captures. This is what the VERIFIED tags in this file rest on.
- **(c) Static analysis of Apple's macOS AirPlay receiver and sender frameworks** (symbols, strings,
  disassembly at the interface level) for names, flags, handlers and labels. FairPlay was mapped only at its
  call boundary; no FairPlay internals, tables or keys were used (§2).
- **(d) Open-source AirPlay projects** (UxPlay, RPiPlay, shairport-sync, goplay2, airplay2-receiver,
  shairplay) read only as protocol references, never built or run. No code from them is copied into the
  repo; the only mentions are two comments in `box/airplay-probe/src/main.rs` (line 226 names
  airplay2-receiver, line 1229 names nqptp, shairport-sync's PTP companion; VERIFIED by search). Verbatim similarity was
  checked once (see Credits below).
- **(e) The licensed Apple CarPlay SDK material** already used by the CarPlay layers (the shared key and nonce
  rules are the CarPlay ones, §4.6).

**Credits and similarity check.** These open-source projects informed our understanding of the AirPlay protocol
(read-only references; no code copied into this repository). Each project's own license governs its code; consult it
there, none is restated here: UxPlay (github.com/FDH2/UxPlay), RPiPlay (github.com/FD-/RPiPlay), shairport-sync and
nqptp (github.com/mikebrady/shairport-sync, github.com/mikebrady/nqptp), goplay2 (github.com/openairplay/goplay2),
airplay2-receiver (github.com/openairplay/airplay2-receiver), shairplay (github.com/juhovh/shairplay), and the
ShairPlay/PlayFair lineage those projects cite. On 2026-11-12 every non-comment source line of at least 45 characters
in `box/airplayd`, `box/airplay-probe`, `host/ocbm-host/src/airplay.rs`, `crates/ocbm-proto/src/seam.rs` and
`host/MacHost/AirPlayTestHost/Sources` (3,482 lines) was compared, after whitespace normalisation, against about
73,000 such lines from local read-only clones of UxPlay, RPiPlay, shairport-sync, nqptp, goplay2, goplay2b,
airplay2-receiver, airplayreceiver, airplay-server, openairplay2 and shairplay: 0 exact matches. Limit: this detects
verbatim copying only, not paraphrase or translation between languages, and the protocol knowledge itself (field
names, layouts, key labels) is shared with those projects because the iPhone's wire format fixes it.
