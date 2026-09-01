# Android Auto — OCBM bridge and projection arbitration

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 60, 61; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

**Contents:** the AA bridge (protocol, channels, audio parity) → arbitration between CarPlay and Android Auto.

## Android Auto — the OCBM bridge

<!-- absorbed: ../host/02_ANDROID_AUTO.md -->

### 0. Scope and framing

This is standard interoperability and accessory development: enabling the project's own host application
to act as an Android Auto head unit for the device owner's own phone, over the owner's own CPC200-CCPA
hardware, reusing the same OCBM transport already built for CarPlay. It is the direct analogue of the
existing CarPlay path (`docs/carplay/00_ARCHITECTURE.md`, `docs/carplay/01_OCBM_PROTOCOL.md`): the box moves bytes, the host application implements the
projection protocol. Android Auto is Google's documented head-unit protocol; the reference
implementation used here is the widely-published open-source `aasdk`/`openauto` stack (GPLv3). All
testing is against first-party developer tooling (Google's in-app head-unit server / Desktop Head Unit)
and the owner's own devices.

The doctrine of `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` applies unchanged: **anything configurable about the projection session is
host-application-driven; the box presents a transport, not policy.**

### 1. Why AA does not map one-to-one onto the CarPlay OCBM model

Two structural differences shape the whole design. Both are properties of the AA protocol, established
from the sources in §2, not choices.

**1a. Session encryption is a single TLS record stream, not per-frame sealing.** OCBM's CarPlay media
model (`docs/carplay/01_OCBM_PROTOCOL.md` §"Media transport") works because AirPlay seals each access unit independently with
ChaCha20-Poly1305 and carries the nonce on the wire, so the box can forward ciphertext untouched and
hand the host one ephemeral key. AA instead runs the entire session — control, video, audio, input,
sensors — inside one TLS 1.2 connection. There is no per-frame key to hand off. Whoever terminates TLS
sees the whole session; nobody else sees any of it. Therefore the forward-encrypt-and-hand-the-key
technique does not transfer, and the box cannot be a media relay in the CarPlay sense.

**1b. The head unit presents a client credential during the TLS handshake.** AA authenticates the head
unit to the phone with an X.509 client certificate whose chain terminates at Google's "Google Automotive
Link" (GAL) root. There is no per-developer or per-device programmatic equivalent of the MFi
coprocessor. See §3 for how this is resolved.

The consequence of 1a is the central design decision: **the box forwards the raw TLS byte stream and the
host application is the head unit** — it terminates TLS, demultiplexes the AA channels, decodes A/V, and
sends input. This is the same "dumb byte pipe" role ocbmd already fills for `CH_RTSP` and `CH_IP`, and it
keeps every credential and codec decision in the host application where `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` says it belongs.

### 2. Established facts (from prior in-house analysis + public references)

The stock CPC200-CCPA already implements AA on-box, in the `ARMAndroidAuto` binary. Prior analysis of
that binary and of live wired sessions with the reference phone (Pixel 10, wired identity `18d1:2d01`)
established the following. Items marked CONFIRMED were verified against running hardware.

#### 2a. The stock AA implementation is the public open-source stack

`ARMAndroidAuto` is `openauto` + `aasdk` cross-compiled for ARM, custom-LZMA packed (container magic
`0x55225522`; not UPX). CONFIRMED three independent ways:

- C++ symbols in the reconstructed binary: `aasdk::transport::SSLWrapper`,
  `aasdk::messenger::Cryptor::cCertificate`, `openauto::service::AndroidAutoEntity::onHandshake`;
  dynamic-linked against `libssl.so.1.1`.
- Packed 489,800 B → unpacked 1,488,932 B (3.0×); true unpacked image reconstructed from memory
  segments (`ARMAndroidAuto_reconstructed`).
- The binary is AA-only — no references to any other protocol — and is independently start/stopped by
  `phone_link_deamon.sh`, so it can be studied and replaced without affecting other functions.

#### 2b. The head-unit certificate is the public `aasdk` credential

The certificate and RSA-2048 private key embedded in the stock `ARMAndroidAuto` runtime image are
**byte-for-byte identical** to the credential published in the `aasdk` source tree:

```
cert DER SHA-256   1c0e0ef9…85ea3c35   (stock CCPA == aasdk == in-house OE backup)
key  DER SHA-256   08e86e4d…f2e99a25   (stock CCPA == aasdk == in-house OE backup)
subject  C=JP, O=JVC Kenwood, OU=01
issuer   C=US, L=Mountain View, O=Google Automotive Link
serial   0x1B    RSA-2048    validity 2014-07-04 … 2045-04-29
```

Implication: the head-unit credential is not a per-unit provisioned secret and there is nothing unique
to extract from a given adapter. It is a single well-known credential shared across the entire
open-source AA head-unit ecosystem (`aasdk`, `openauto`, and downstream projects), and the stock
Carlinkit firmware simply redistributes it. The credential question that would otherwise dominate this
workstream is therefore already answered; see §3.

#### 2c. Protocol parameters (CONFIRMED against live hardware)

| Area | Finding |
|---|---|
| TLS | cipher `ECDHE-RSA-AES128-GCM-SHA256`; AA protocol version `1.7` |
| Video | H.264; SPS `1920×1088` (macroblock-aligned from 1080); 30 fps; IDR interval ~60 s; keyframe-request throttle 1 s |
| Audio | MEDIA 48 kHz/16-bit/stereo; SPEECH 16 kHz/16-bit/mono; SYSTEM 16 kHz/16-bit/mono; throughput ~184–192 KB/s on MEDIA |
| Audio-state bitmask | `BOX_TMP_DATA_AUDIO_TYPE`: `0x0000` silent, `0x0110` media, `0x0114` media+mic, `0x0404` speech/VR |
| Focus/control commands | video focus 500/501, audio focus 502/505, navi focus 506/507, keyframe 12, mic 1/2/7 (full table captured) |
| Navigation | turn/distance events with protobuf field names and enum values captured |
| Session geometry | `gLinkParam` delivers the negotiated `iWidth×iHeight` at connect (observed 2400×788 against a configured 1920×690) — this is the oversize offset the host app already compensates for (§5) |

#### 2d. Reference material on disk

Prior working directory (`/Volumes/stuff/misc/research/CPC200-CCPA/`):

- `aa_rebuild/aasdk-main`, `aa_rebuild/openauto-main` — extracted GPL source (protobuf definitions,
  framing, channel logic).
- `cpc200_ccpa_firmware_binaries/analysis/aa_full_session_adapter_20260315.txt` — the box side of a real
  wired AA session.
- `.../aa_full_session_emulator_20260315.txt` — the same session as seen by Google's Desktop Head Unit
  (DHU), i.e. a first-party reference to diff against.
- `.../ARMAndroidAuto_reconstructed`, `aa_dynsyms_demangled.txt`, `aa_relocations.txt` — unpacked binary
  and full symbol map.
- `A15W_viewarea_patch.img` — a prior box-side geometry patch (relevant to §5).

### 3. Credential handling

Given §2b, the host application uses the same published GAL-issued head-unit certificate, private key,
and GAL root certificate used by the reference open-source stack (`hu_cert.pem`, `hu_key.pem`,
`galroot_cert.pem`). No extraction from a specific adapter is required; three independent copies already
agree byte-for-byte.

Handling rules:

- These files are treated as vendored third-party assets, kept out of any public repository, consistent
  with the project's handling of other licensed/redistributed reference material.
- The licensing posture is the same as adopting `aasdk` itself (GPLv3 stack plus a redistributed
  well-known credential); running it on the CPC200-CCPA hardware does not change that either way.
- The credential is presented only to the device owner's own phone during the owner's own session.

### 4. Architecture

```
   Phone (owner's Pixel)
        │  Android Auto: one TLS 1.2 stream (control/video/audio/input/sensors), protobuf-framed
        │
   ┌────┴─────┐   AOAP (wired) or Wi-Fi+BT (wireless)
   │   Box    │   role-switch + raw byte pump. No TLS, no protobuf, no certificate on the box.
   │ (CCPA)   │
   └────┬─────┘
        │  OCBM: opaque byte stream on a dedicated channel (prototype on CH_IP; then CH_AA)
        │
   Host application  ── terminates TLS with the GAL head-unit credential
                     ── demuxes AA channels, decodes H.264 + audio (existing VideoToolbox/audio path)
                     ── sends touch/knob/nav input (existing CH_INPUT semantics map across)
```

**Box responsibilities (transport only):**
- Wired: perform the AOAP accessory handshake on the idle host-side USB port (`usb1`; controllers
  `ci_hdrc.0/.1` present). This mirrors the existing CarPlay role-switch (`iap_role_switch`), with AOAP
  control requests 51/52/53 in place of Apple's `0x51`.
- Wireless (later): bring up the SoftAP + Bluetooth using the existing `crates/vendor/wireless` seam and
  the RTL8822CS radio; exchange the `WifiInfo`/`WifiStart` protobufs (already present in `aasdk`) over
  RFCOMM; then carry the phone's TCP session bytes.
- Either transport: move the resulting byte stream onto OCBM unmodified, exactly as ocbmd already does
  for other seams.

**Host responsibilities (the head unit):**
- TLS termination and the AA handshake with the GAL credential.
- Channel setup (`ServiceDiscoveryRequest/Response`), video/audio/input/sensor channels.
- Decode and render (reuse of the CarPlay host's decode/audio/input stack), and session geometry
  handling per §5.

### 5. Video geometry

The stock path negotiates a session surface (`gLinkParam` `iWidth×iHeight`) that can differ from the
configured tier — observed 2400×788 delivered against a configured 1920×690 — and the host app already
corrects for this at draw time (`CarPlayView`: `resizeAspectFill` crop when the window is wider than the
video aspect, `resizeAspect` pillarbox when narrower, with touch normalization switched to Android's
crop formula `y = (eventY − cropTop) / surfaceHeight`). In the host-as-head-unit design the host authors
the AA video configuration directly (resolution enum, margins, DPI) in `ServiceDiscoveryResponse`, so it
requests the geometry it wants rather than inheriting a translated size through the box. The existing
crop/pillarbox and touch-normalization code carries over unchanged as defensive handling when the phone
returns a surface that differs from the request; the negotiation side stops being a workaround. The prior
`A15W_viewarea_patch.img` documents the box-side geometry adjustment for reference.

### 6. Current state

Android Auto runs end to end over OCBM/USB: the box switches the phone into AOAP and pumps the TLS
byte stream over `CH_IP` to `ccpa/aa-bridge`; the macOS app terminates the AA session, decodes H.264
through the same VideoToolbox path CarPlay uses, plays all four audio sinks through the same engine,
and sends input back on the AA input channel. The box selects AA on its own — no env var — and
CarPlay/AA arbitration is settled (see the arbitration half of this document).

- **Video:** streams indefinitely. Startup FIFO deepens to 64 then shrinks to 2 after 30 decoded
  frames, which killed the warm-up drop (slotDrops=0 through 270 frames).
- **Audio:** parity with CarPlay, device-verified 2026-08-27. MEDIA 48 kHz/16/stereo, SPEECH and
  SYSTEM 16 kHz/16/mono. Each spoken prompt is its own media session with an incrementing
  `session_id`, so ACKs must follow the current id (`audioSessions[ch]`). The sink table lives once, in
  `AACapability.audioSinks`, read by both the service-discovery declaration and playback — they were
  independent literals, and a mismatch is not something the phone can detect.
- **Input:** media keys work — Play/Pause (85), Next (87), Previous (88), Assistant via SEARCH (84).
  **HOME and BACK do not.** Two protocol facts, both established here: `button_event` is **field 4** of
  `InputReport` (layout `timestamp=1, disp_channel=2, touch_event=3, button_event=4`) — the first cut
  used field 2 and every key was silently discarded; and `keycodes_supported` is **field 1** of
  `InputSourceService`, with gearhead echoing the declared set back in `KEY_BINDING_REQUEST`.

### 7. Resolved defects, with causes worth keeping

**Session death at 57–94 s** (`errSSLDecryptionFail -9845`, then a busy loop on `errSSLClosedAbort`).
Ciphertext was fed to TLS out of order: there is ONE TLS stream shared by every channel, but `recvMsg`
reassembled a fragmented message's ciphertext **per channel** before decrypting — aasdk's shape, which
`host/aa-headunit` copied. That withholds a FIRST fragment's bytes while feeding the next frame's, and
the phone interleaves channels mid-message. It only bit when a >16 KB message fragmented and another
channel interleaved before the LAST, which is why it read as a random timer. **That the phone
interleaves at all proves encryption is per FRAME, not per message.** Fix: decrypt each frame as it
lands and accumulate PLAINTEXT per channel. Result: 309 s / 3510 frames / zero decrypt failures,
against a previous best of 94 s.

**Video stall at ~2 minutes over the box relay** (audio and ping/pong kept flowing). Cause:
`AAWire.mediaConfigReady()` advertised `max_unacked=1`, making video stop-and-wait — the phone will
not send frame N+1 until it sees the ACK for N. Over the fire-and-forget box relay one delayed ACK
wedged the channel; low-latency adb-TCP never tripped it. Fix: `max_unacked` 1 → 64, and send the
`mediaAck` on receipt before decode work. Window scaling confirmed the mechanism (1 → stall at 3690
frames, 8 → 4140, 64 → no stall).

**Mid-session pixelation.** The phone's keyframe cadence follows **the protocol version the head unit
REQUESTS**: `key_frame_interval_wireless = 60` s below 6.0, `key_frame_interval_ackless = 2` s at 6.0+.
At 1.7 the phone emits an IDR once a minute, and the protocol has no keyframe request at all, so one
shed P-frame visibly persists. With `AA_PROTO=6.1` IDRs land every ~60 frames instead of at frame
#1801. Not a codec problem — switching to H.265 would not have fixed it.

**Refuted:** the "we never ACKed audio" root cause for recurring teardowns. Disproved by experiment in
the same session — AA flow control is per-channel, and audio is the one channel we do not ACK, yet it
runs indefinitely. Audio ACK conformance remains worth doing, but it was not the cause.

### 8. Proposed OCBM additions (not implemented)

Additive per the `../carplay/01_OCBM_PROTOCOL.md` extensibility rules — frozen envelope, no version
bump. A `CH_AA` channel (proposed `0x0050`) for the opaque AA byte stream, and `CT_AA_*` lifecycle
opcodes on CH_CTRL. Until they exist, AA rides `CH_IP` unchanged, which is what ships today.

### 9. Test environment

Pixel 10 (`frankel_beta`, Android 17) with gearhead 17.5.663204, over adb; the head-unit credential is
presented only to the owner's own phone, using Google's in-app developer head-unit server. Reference
oracle: the captured DHU/emulator session and the captured stock-adapter session. Harness:
`host/aa-headunit` (`run_capture.sh`).

### 10. Open items

- **HOME and BACK keys.** Declared and echoed back by gearhead, but no effect.
- **Audio ACK conformance** — per-channel session ids for the audio sinks.
- **Mic and voice parity** — map AA's SPEECH/SYSTEM audio and mic capture onto the existing `CH_MIC` /
  `CH_ALT_AUDIO` host paths.
- **Wireless AA** is unbuilt; wired only.
- **Transport backpressure** for the AA path.
- **Footprint** — the box side adds an AOAP switch plus a byte pump; confirm against the rootfs budget
  (`../ops/00_BUILD_AND_DEPLOY.md`).

## Projection arbitration — CarPlay vs Android Auto

<!-- absorbed: ../host/02_ANDROID_AUTO.md -->

Design note for making the box correctly detect and arbitrate between CarPlay and Android Auto,
iPhone vs Android, wired vs wireless — so the right path/services start and an active session is
never interrupted by another connecting phone. Grounded in a 3-agent code audit + live headless
probing of the box (NCM/SSH, box in NCM mode). **No code changed by this doc — it is the plan.**

### 1. Current reality (what exists)

**Arbitration hub = `tools/session_supervisor.sh`** (installed to `/script/`), driven by `/tmp` flags
written by `ocbmd`:
- `host_present` — master app-session gate (ocbmd, on CT_SUBSCRIBE). Box IDLE until an app connects.
- `carplay_transport` = `wireless` when the wireless arm owns the session (`av.rs:274`); absent = wired-or-idle.
- `phone_present`, `radio_off` (app CT_RADIO), `carplay_cfg.yaml` (app-pushed YAML config).

**CarPlay wired↔wireless arbitration is robust** (all in session_supervisor):
- `wireless_owns_session()` → first-come-wins; wired `arm()` refuses while wireless owns.
- Hot-Handover **default OFF** → a fresh wired plug does NOT preempt a live wireless session
  (matches Apple WWDC 2017-717 "do not interrupt a running session"). Opt-in `hot_handover: true`.
- Radio-yield mirror parks the wireless radio (hostapd/hci0) for the duration of a wired session.

**Doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md): app-driven.** Box carries no opinions; the app pushes config (wireless on/off,
hot-handover, pairing mode, WiFi creds) via CT_SUBSCRIBE → `carplay_cfg.yaml`. There is **no** host
opcode to explicitly pick wired-CP / wireless-CP / AA (`CT_MODE_SELECT` is unrelated — projection vs
console debug). The Rust `/run/carplay/arbiter.sock` is a STUB (always grants standalone); all real
arbitration is the shell supervisor.

**Phone-type detection exists but as two ISOLATED probes:** CarPlay greps Apple `05ac`
(`projection_up.sh:24`, `session_supervisor.sh:94`); `aa-bridge` matches Google `0x18d1`
(`aa-bridge/src/main.rs:69,256`). Neither is aware of the other vendor. No unified resolver.

### 2. Configuration matrix — what happens TODAY

| # | Scenario | Behavior today | OK? | Gap |
|---|----------|----------------|-----|-----|
| 1 | Wired iPhone + app subscribed | CarPlay projects (projection_up→iap2d→airplayd) | ✅ | — |
| 2 | Wired Android + app subscribed | CarPlay no-ops (no 05ac); **AA never auto-starts**; phone just charges | ❌ | No auto-AA selection |
| 3 | aa-bridge run while iPhone present (normal) | `find_phone` 18d1-guard skips iPhone; **zero control transfers reach it** | ✅ | — |
| 4 | iPhone mid-CarPlay, aa-bridge started | iPhone role-switched to host → invisible to aa-bridge's host bus | ✅ | — |
| 5 | Android mid-AA, app subscribes (projection_up runs) | grep 05ac no-match → exits 1; `phone_waiting` latches; AA undisturbed | ⚠️ | Latent `phone_reset.sh` hazard (see §3) |
| 6 | Wireless CarPlay active + wired AA phone plugged | **No guard.** If aa-bridge runs it grabs ci_hdrc.0 unmediated → collision | ❌ | No CP↔AA arbitration |
| 7 | Wired CarPlay active + phone in BT range | Wireless defers (Hot-Handover off); radio parked | ✅ | CP-only (AA has no wireless) |
| 8 | Wireless Android Auto | Does not exist | — | Phase 3, unstarted |

Incidental safety today: strict per-VID self-filtering (CP=05ac, AA=18d1) + role-switched devices
becoming invisible to the other path prevent cross-wiring in 1,3,4,5. The real holes are **2** (AA
never selected) and **6** (no CP↔AA mutual exclusion), plus the latent **5** hazard.

### 3. The latent hazard (fix even before full AA integration)

`session_supervisor`'s `PROJ_AT=3` failure ladder → `escalate()` → `phone_reset.sh` does a **real USB
port reset of ci_hdrc.0**. Today it's capped at 1 failure by the `phone_waiting` debounce, so it isn't
reached — but it is *only* the debounce standing between a live AA session and a port reset. Make
`escalate()`/`kill_session()`/`phone_reset.sh` no-ops while AA owns the session (mirror the existing
`wireless_owns_session()` guard).

### 4. How arbitration works today

Single-owner flag in `box-common`, one definition of the token spellings shared by ocbmd, aa-bridge
and the shell (`box_common::flags::owner()`).

- **CarPlay claims it.** `arm()` calls `claim_carplay_owner()`; `kill_session()` releases. A `wired-cp`
  claim with no `airplayd`/`iap2d` alive is stale, cleared, and falls through, so a crashed CarPlay
  session cannot lock AA out permanently.
- **AA claims it before it can serve.** `aa-bridge` prepares the accessory and claims the flag
  **before `accept()`**, serves one session per prepared accessory, clears at session end, and loops.
  This was forced by a real bootstrap deadlock: the bridge used to claim inside `serve_session`, i.e.
  only after a host connected, while the app only connects after seeing `PM_WIRED_AA` — each side
  waited for the other and the box sat idle with a running bridge and a plugged-in phone. Claiming
  before `accept()` also keeps the flag honest: `wired-aa` means a live accessory link exists. Every
  wait on that path is bounded, since claiming early makes a hang a lock-out.
- **The box tells the app which mode is live.** `CT_PROJ_MODE = 0x19`, payload `[CT_PROJ_MODE][PM_*]`
  with `PM_NONE 0x00 / PM_WIRED_CP 0x01 / PM_WIRELESS_CP 0x02 / PM_WIRED_AA 0x03 / PM_WIRELESS_AA 0x04`
  (the last reserved for wireless AA, which is unbuilt). `ocbmd::proj_mode_tick` emits on change only,
  throttled to ~2/s once latched, re-armed on every fresh `CT_SUBSCRIBE` so a re-attaching app learns
  the current mode immediately. Same discipline as `CT_BT_PHASE`. Additive per the OCBM extensibility
  rules — frozen envelope, no version bump. Plugging an Android phone into a box with a subscribed app
  therefore projects AA with no env var and no user action.

### 5. Failure modes closed, and why each mattered

- **CarPlay hijack.** An already-resident `aa-bridge` could claim the box during a live wired CarPlay
  session; the app would park the CarPlay decoder and switch to AA mid-drive while the supervisor
  treated the still-running CarPlay stack as handed off. Neither existing stand-down covered it —
  `carplay_session_live()` gates only bridge LAUNCH, and `apple_on_bus()` goes blind the moment the
  iPhone role-switches out of `05ac`. The bridge now stands down on `wired-cp` at every point it could
  claim: loop top, immediately before `set_owner` (CarPlay can arm during the multi-second AOAP
  switch), on the unclaimed→client path, and inside the wait loop so a parked bridge notices within
  250 ms. `release_owner_if_ours()` replaced every blanket `clear_owner()` — the bridge had been
  deleting CarPlay's claim while cleaning up after itself, found by device test, not by review.
- **F6 — a bare hub is not a phone.** `phone::classify()` treated every non-Apple, non-root-hub VID as
  an Android candidate, so a hub, dashcam or card reader launched a permanently resident bridge that
  AOAP-probed it forever (the precondition for the hijack above) and suppressed wireless CarPlay.
  Hubs are now excluded by `bDeviceClass == 0x09`, mandatory in every hub device descriptor (USB 2.0
  §11.23.1) and never used by a phone — Apple, Android normal mode and AOAP accessories are all
  per-interface `0x00`. Measured on the box: Pixel `vid=18d1 class=00`, root hub `vid=1d6b class=09`.
  `apple_on_bus()` stays deliberately VID-only: over-inclusive there fails toward CarPlay.
- **F4 — a crashed app wedged AA.** ocbmd's `CT_HELLO` reattach cleared the output queues but not
  `self.conns`, so a host that died without `CT_STOP` and relaunched inside the heartbeat grace left
  its corpse's CH_IP socket being pumped while the new host's `IP_OPEN` sat unaccepted in the bridge's
  backlog.
- **F3 — the Android Auto toggle did not reach a running bridge.** `android_auto: false` was read in
  exactly one place, `session_supervisor.sh::aa_enabled()`, guarding `arm_aa` — the launch. Nothing
  consulted it afterwards, so turning AA off during a live session did nothing until the phone was
  unplugged. It is now checked inside the session loop as well.
- **F2 and F5** were settled on hardware 2026-08-27 and are closed.
- **Race: `wireless_up` vs AA ownership** — fixed 2026-08-24.
