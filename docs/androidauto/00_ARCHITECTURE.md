# Android Auto — architecture, credential and transport

> **STATUS:** CURRENT · single owner for this topic. Split out of `docs/host/02_ANDROID_AUTO.md` on 2026-08-31 when Android Auto got its own category. Correct this file in place — do not add a sibling.

**Contents:** scope → why AA does not map onto the CarPlay OCBM model → established facts about the
stock stack → the head-unit credential → the box/host split → video geometry → proposed OCBM
additions → the test bench.

Session behaviour, A/V and open items live in
[`01_SESSION_AND_AV.md`](01_SESSION_AND_AV.md); CarPlay/AA arbitration in
[`02_ARBITRATION.md`](02_ARBITRATION.md); the wireless transport in
[`03_WIRELESS.md`](03_WIRELESS.md).

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

Given §2b, the host application uses the same published GAL-issued head-unit certificate and private
key used by the reference open-source stack (`hu_cert.pem`, `hu_key.pem`; shipped here as the leaf-only
`host/aa-headunit/certs/headunit.{crt,key,p12}`). No extraction from a specific adapter is required;
three independent copies already agree byte-for-byte.

**Corrected 2026-09-02:** no `galroot_cert.pem` exists in this repo or on the host, and neither host
verifies the phone's chain — `host/aa-headunit/src/tls.rs` sets `SSL_VERIFY_NONE` and the macOS app's
`AATLS` breaks on server auth and continues; the reference `aasdk` stack does the same. Until a GAL
root is sourced, the only feasible check is log-only (compare the phone's presented issuer against
`O=Google Automotive Link`), which is required before wireless AA, where a rogue peer would receive
mic audio.

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

### 6. Proposed OCBM additions (not implemented)

Additive per the `../carplay/01_OCBM_PROTOCOL.md` extensibility rules — frozen envelope, no version
bump. A `CH_AA` channel (proposed `0x0050`) for the opaque AA byte stream, and `CT_AA_*` lifecycle
opcodes on CH_CTRL. Until they exist, AA rides `CH_IP` unchanged, which is what ships today.

### 7. Test environment

Pixel 10 (`frankel_beta`, Android 17) with gearhead 17.5.663204, over adb; the head-unit credential is
presented only to the owner's own phone, using Google's in-app developer head-unit server. Reference
oracle: the captured DHU/emulator session and the captured stock-adapter session. Harness:
`host/aa-headunit` (`run_capture.sh`).

