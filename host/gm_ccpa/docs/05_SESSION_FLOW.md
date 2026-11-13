# 05 — CarPlay wireless session flow (traced from `ocbm`)

**What this is:** the code-traced sequence a wireless CarPlay session goes through, from Bluetooth
discovery to live streaming. [`04_SYSTEM_MODEL.md`](04_SYSTEM_MODEL.md) says what the system is; this
says what happens, in what order, and what breaks if the order is wrong. Treat it as the implementation
checklist for the head-unit app.

**Source:** traced across `ocbm/` (the `wireless`, `receiver`, `pairing`, `mfi`, `rtsp` and `metadata` crates),
its on-hardware captures (`docs/ops/captures/`) and unredacted iPhone-side logs (2026-08-02, iOS 27; not retained in this tree). Timings and failure modes are measured unless marked otherwise; iPhone-side log lines are
kept as reported and cannot be re-verified from this repo. Protocol detail owned by `ocbm/docs/carplay/` is
summarised here; this doc keeps the order, the failure modes and the gm_ccpa split.

---

## 0. The headline: there are TWO iAP2 handshakes

1. **iAP2 over Bluetooth/RFCOMM** — authenticates the phone and gets it onto the vehicle's Wi-Fi.
2. **A second, fresh iAP2 session over the AirPlay DataStream** (stream type 130) — full DETECT/SYN, MFi
   certificate + signature, Identify — carrying metadata and controls for the rest of the session.

The tunnel carries no state from the Bluetooth link. Consequence: the MFi coprocessor is called in three
separate places per session (§10).

---

## 1. Phase A — Bluetooth: becoming visible *(stays on the CCPA)*

- **A1 Radios wake.** Off at boot. The app's OCBM `CT_SUBSCRIBE` is the trigger: `ocbmd` mirrors host
  presence and the box brings BT up on that edge.
- **A2 HCI.** iOS classifies the box as a CarPlay accessory from CoD `0x200408`, the device name and an EIR with
  two 128-bit UUIDs: iAP2 `00000000-deca-fade-deca-deafdecacaff` (AD 0x06) and the CarPlay marker
  `d31fbf50-5d57-2797-a240-41cd484388ec` (AD 0x07). Then `piscan`.
- **A3 SDP.** Record "Wireless iAPv2" on L2CAP PSM 1 pointing at RFCOMM channel 1. Mandatory: without it the
  phone pairs, browses SDP, finds nothing and disconnects.
- **A4 SSP.** Just-Works (NoInputNoOutput), auto-accepted in-kernel; link key persisted. Numeric comparison is
  experimental.
- **A5 RFCOMM.** Inbound (phone dials ch 1), or outbound for a bonded phone.

> **Trap on the outbound path:** the phone's iAP2 service lives under a different UUID —
> `02030302-1d19-415f-86f2-22a2106a0a77` — not the accessory's `…decacaff` (empty result on iOS), and
> there is a one-byte-off decoy, "Wireless iAP5" `…decacafe`, next to it.

---

## 2. Phase B — iAP2 over RFCOMM: auth and handoff *(stays on the CCPA)*

| # | Dir | Message | Notes |
|---|---|---|---|
| B1 | A→P | DETECT prelude | `FF 55 02 00 EE 10` — six fixed bytes, not an iAP2 packet |
| B2 | A→P | **SYN** (ctl `0x80`) | LinkVersion 1, MaxRcvPacketLength 4096, two sessions declared: Control (id 1) and FileTransfer (id 2) |
| B3 | P→A | **SYN-ACK** (ctl `0xC0`) → bare ACK | Link up in ~6 ms best case; ~1.0 s if our SYN is missed and the phone retransmits on its 1 s timer |
| B4 | P→A / A→P | `0xAA00` → `0xAA01` AuthenticationCertificate | **Chip call #1** (i2c regs `0x30`/`0x31`) |
| B5 | P→A / A→P | `0xAA02` → `0xAA03` AuthenticationResponse | **Chip call #2** (regs `0x20`/`0x21`/`0x10`/`0x11`/`0x12`). 1.7–1.9 s |
| B6 | P→A | `0xAA05` AuthenticationSucceeded | Routinely coalesced into the same read as B7 — parse by declared length, not buffer length |
| B7 | P→A / A→P | `0x1D00` → `0x1D01` IdentificationInformation → `0x1D02` Accepted | 2.2 s (2026-07-25 trace) to 3.0 s (2026-08-02), attach → identified |
| B8 | P→A | `0x4E0E` DeviceTransportIdentifierNotification | The only place the phone's real Wi-Fi MAC is visible (ARP shows a private MAC) |
| B9 | P→A / A→P | `0x5702` → `0x5703` | SSID / passphrase / security type / channel, as TLV params 1–4 (no param 0) |

**MFi comes before Identify**, not after.

**On `0x1D02` the accessory sends nothing.** The more dangerous rule is one step earlier: declaring
NowPlaying (`0x5000`) or RouteGuidance (`0x5200`) ids in params 6/7 of `0x1D01` gets the whole Identify
rejected with `0x1D03` IdentificationRejected — Identify never reaches `0x1D02`, so the handoff never
happens. Observed twice on hardware with unrelated id pairs. The BT-side Identify must stay minimal; the
tunnel Identify (Phase F) is where the metadata ids belong.

**There is no join confirmation.** A successful `0x5703` write is the only signal. iOS retries `0x5702`
two or three times as a matter of course, so the handler must be idempotent. After the handoff the
RFCOMM link stays up and idles — in the happy path only the phone ever closes it.

---

## 3. Phase C — Wi-Fi discovery *(moves to the APP)*

| # | Stage | Detail |
|---|---|---|
| C1 | Phone joins | `Sending iAP in-car wifi notification` → `setting WiFiManager to in-car` → `StartBonjourForWiFi … reason: initiated by CarKit` |
| C2 | Advertise | `_airplay._tcp`, TXT: `deviceid`, `features`, `flags=0x4`, `model`, `protovers=1.0`, `pi`, `srcvers`. No `pk` key — the Ed25519 LTPK is exchanged inside pairing, not advertised. Built by `CarPlayRx.txtRecordFor`. |
| C3 | **Dial out** | Discovery is bidirectional. The accessory also browses `_carplay-ctrl._tcp` and sends the phone `GET /ctrl-int/1/connect` with an `AirPlay-Receiver-Device-ID` header. Surfaces phone-side as `HandleControlServerEvent command 'connect'`. |
| C4 | Phone connects in | Resolves SRV → connects TCP. ~8 ms for SRV + DNS + connect. |

App-side: `CarPlayRx` advertises `_airplay._tcp` while browsing `_carplay-ctrl._tcp` (`browseForPhone`, called
from `start`) and dials the resolved peer (`rediscoverLoop` → `dialOut` → `connectOut`; see the `ORDER IS
LOAD-BEARING` comment in `start`). The phone's `_carplay-ctrl` port changes every session.

> **The `features` TXT value is load-bearing.** Its high word bit 32 (Car) is what makes iOS open RTSP
> back to the advertised port. Drop it and the phone answers the connect-out with 200 OK and never opens
> a control connection — no session, no video. TXT `features` must equal `/info` `features`, and TXT
> `deviceid` / `pi` must equal the receiver's or pair-verify fails.

---

## 4. Phase D — AirPlay pairing *(moves to the APP)*

| # | Request | Notes |
|---|---|---|
| D1 | `POST /pair-verify` M1, M3 | The phone always tries the fast path first, even on a virgin box, and gets `Error=Authentication` back |
| D2 | `POST /pair-setup` ×3 | First pairing only. SRP-6a, 3072-bit MODP, SHA-512, setup code `3939`. Persists exactly one record: controller ID → 32-byte Ed25519 LTPK |
| D3 | `POST /pair-verify` M1, M3 again | Now succeeds. Curve25519 ECDH + Ed25519 signatures. No chip. |
| D4 | Encryption flips | See below |
| D5 | `POST /auth-setup` | MFi-SAP. Chip calls #3 and #4. |

**The encryption flip:** the pair-verify M4 response goes out in plaintext; ChaCha20-Poly1305 starts with the next
byte in either direction. Keys: HKDF-SHA512 off the ECDH secret, salt `Control-Salt`, read/write info strings
crossed over (named from the controller's side: the server decrypts inbound with `Control-Write-…` and
encrypts outbound with `Control-Read-…`). Frame: `[len u16 LE][ciphertext][tag 16]`, AAD = the two length
bytes, nonce = 4 zero bytes ‖ per-direction u64 LE counter; outbound split at 1 KiB, inbound over 16 KiB
rejected before decrypt.

`/auth-setup` runs inside the encrypted channel, layered on pair-verify: X25519 ECDH → SHA-1-derived AES-128-CTR
key/IV → `create_signature(SHA1(ourPK‖peerPK))` **then** `copy_certificate()` (signature first;
`exchange_with_secret` in `ocbm/crates/vendor/mfi/src/sap.rs`) → 1113-byte M2, certificate in the clear, only
the signature AES-encrypted.

---

## 5. Phase E — session establishment *(moves to the APP)*

| # | Request | Establishes |
|---|---|---|
| E1 | SETUP phase 1 (no `streams` key) | `{timingPort, eventPort, keepAlivePort, enabledFeatures[]}`. Idempotent — a repeat without a TEARDOWN returns the same ports. |
| E2 | *(feature intersection)* | `/info` advertises a superset; the SETUP `features` array intersects it. Advertising is not enough — a capability must survive this intersection. |
| E3 | `GET /info` | Encrypted re-read, after phase 1, not before. ~36 KB response. Carries a bplist `qualifier` requesting a subset, ignored with no ill effect. |
| E4 | RECORD | Accept the event-channel TCP (separate connection to `eventPort`) → send `requestUI` + `takeScreen` → open the iAP2 tunnel → then set `sessionStarted` |

The E4 ordering is mandatory. Every accessory→phone command is hard-gated on `sessionStarted`; setting
it before the tunnel opens lets an inbound `modesChanged` race in and trigger a second DETECT+SYN. The
event channel gets its own keys — salt `Events-Salt`, no read/write crossover, because the receiver is
the HTTP client on that socket.

Measured 2026-08-02: the phone proposed 14 feature keys, six survived (`hevc`, `altScreen`, `viewAreas`,
`cornerMasks`, `iAPChannel`, `sessionManagement`); not negotiated: `uiContext`, `focusTransfer`,
`h.264Level5.1`, `mainBuffered`, `enhancedSiri`, `vehicleStateProtocol`, `videoPlayback`, `logTransfer`.
**HEVC needs three gates:** a `hevcInfo` key in `/info` (an empty dict; presence is the signal), `hevc` in the
SETUP feature array, then iOS streams `hvc1`.

---

## 6. Phase F — streams, on demand and interleaved *(moves to the APP)*

**Streams are not set up in a batch.** One 30-minute session logged 15 audio SETUPs alone; the phone
re-SETUPs MainAudio on every Siri turn.

| Type | Purpose | Transport | In R14G17? |
|---|---|---|---|
| **100** | MainAudio — bidirectional; voice / telephony / speechRecognition / alert, and the mic uplink | UDP RTP | yes |
| **101** | AltAudio — second low-latency sink | UDP RTP | yes |
| **102** | MainHighAudio — high-latency media (AAC-LC). Only type given a `controlPort` (RTCP) | UDP RTP + RTCP | yes |
| **110** | MainScreen | TCP (accessory binds, phone connects out) | yes |
| **111** | AltScreen / instrument cluster | TCP | no (post-2017) |
| **130** | DataStream / RemoteControlSession — the iAP2 carrier | TCP | no (post-2017) |
| **106** | AuxOutAudio — dedicated Siri downlink, opened at Siri launch | — | no (post-2017) |
| **107** | AuxInAudio — the Enhanced Siri car-mic uplink (paired with 106 as `GeneralAudio`) | — | no (post-2017) |
| — | MainBuffered — music/media, distinct from 102 | TCP | no (post-2017) |

> R14G17 `AirPlayCommon.h:251-255` defines only types 100, 101, 102, 110; the rest are post-2017, evidenced by
> `CarPlaySDK` symbols (`ocbm/docs/carplay/03_SDK_GROUND_TRUTH.md` §7 "Audio + Siri", **Stream types**). Silence
> in a 2017 source is not evidence of absence; that assumption hid the missing type-130 channel upstream.

`mainBuffered` (receiver-side buffer of up to 2 minutes, RTSP `FLUSHBUFFERED`) is not built and must not be
advertised: [`13_AUDIO_ROUTING.md`](13_AUDIO_ROUTING.md) §5; evidence in `ocbm/docs/carplay/06_AV_PIPELINE.md`
(`mainBuffered`).

Concurrently with the A/V SETUPs, the second iAP2 handshake runs over stream 130 — DETECT/SYN, then
`0xAA00`→`0xAA01` (**chip call #5**), `0xAA02`→`0xAA03` (**chip call #6**), `0x1D00`→`0x1D01`→`0x1D02`,
then the metadata subscribes (`0x5000` NowPlaying, `0x5200` RouteGuidance, `0x4154` CallState, …). The
two pipelines interleave; neither waits for the other.

Streams are keyed individually: HKDF salt `DataStream-Salt<streamConnectionID>`. Except stream 130,
which carries no `streamConnectionID` — its salt uses the SETUP `seed` instead.

> **Audio ceiling: stereo AAC-LC 48 kHz** (wire-format limit: no channel count above 2, no ALAC, AC-3/E-AC-3
> or object-based format; spatial content is rendered down to stereo). Best media path: type 102. Enum:
> `ocbm/docs/carplay/06_AV_PIPELINE.md`.

---

## 7. Phase G — steady state

**Video:** repeating `[128-byte header][body]`. Opcode 0 = encrypted AVCC frame (ChaCha20-Poly1305, the whole
header as AAD, per-VideoFrame u64 LE counter nonce); opcode 1 = plaintext config blob (auto-detect avcC vs
hvcC), which does not advance the counter. **Audio:** RTP over UDP, `[12-byte RTP header][ciphertext][tag 16]
[nonce 8]`, AAD = RTP bytes 4..12 (`ts‖ssrc`), minimum packet 36 bytes. Detail:
`ocbm/docs/carplay/06_AV_PIPELINE.md`.

**Also flowing:** mic RTP uplink (type-100 SETUP with `input=true`), NTP time-sync (request 210, 32-byte
response 211), keepAlive beacons, HID reports on the event channel, RCS metadata on stream 130. Soak
(2026-07-17, before the type-130 tunnel; A/V durability only): 30.5 min, 58,068 video + 93,314 audio frames
(~32 video, ~51 audio fps), zero failures.

**Teardown** is partial or full, distinguished by presence *and non-emptiness* of `streams[]`: a non-empty
array stops just those streams and keeps the session; absent, non-array or **empty** means full teardown
(`fn teardown`, `ocbm/crates/vendor/receiver/src/session.rs`). Treating `streams: []` as partial leaks every
stream thread; in Rust `as_array()` returns `Some(&[])` for it, so test the length, not the `Option`.

---

## 8. Ordering rules that kill sessions

Each of these is an observed, reproducible hard failure. Highest-value section here.

1. **A negotiated feature must be backed by real state, or the phone kills the session ~21 ms after
   RECORD.** Measured 2026-08-02: `carEndpoint_checkCarPlayFeatureAcceptance failed: cornerMasks flag
   not set for any view` → `-16720 kFigEndpointError_InvalidParameter` → `Endpoint_Failed`. Negotiating
   `cornerMasks` obliges you to set the flag on a view. Only negotiate what you actually implement.
2. **The SETUP-130 response must carry a non-zero int64 `streamID`.** Without it the phone logs
   `Failed to obtain transport token from SETUP response: -6727 kNotFoundErr` and its entire outbound
   path to the accessory ceases to exist. Observed 20+ times in one failing session.
3. **Accessory→phone RCS frames must be stamped `'cmnd'`, not `'comm'`** (`'comm'` is the phone→accessory
   direction; `MSGTYPE_COMM` / `MSGTYPE_CMND`, `datastream.rs`). The wrong 4CC is dropped silently, with no
   logging on either side, returning `noErr`. Symptom: the phone parks in FSM state `Pending` retransmitting SYN-ACK forever.
4. **Declaring a `Start*` iAP2 message id without its `Stop*` partner rejects the whole Identify** —
   Apple's condition name is `OptionalMsgNotValidWithoutRequiredMsgs`.
5. **Tunnel `MaxPacketSize` must be `0xFFFF`**, not 4096 — the phone's SYN-ACK carries `FF FF`.
6. **Never re-send DETECT to an already-attached device** — it re-runs the attach path and resets the
   link. Re-send SYN only, and cap retries at ≤10: the phone fires `NotifyConnectionFail` on the 11th
   SYN.
7. **RCS messages span multiple crypto frames.** Reassemble on the envelope's `totalLength` before
   parsing — link packets reach 65 KB while DataStream frames cap at 16 KB. Parsing per-frame silently
   truncates album artwork.
8. **`wantsDedicatedSocket` and `clientTypeUUID` are mandatory** on SETUP 130 for Apple's receiver
   (`-6714` / `-6735` otherwise).
9. **Unhandled SETUP stream types must not be answered with silence.** An unhandled-type branch that
   returns nothing is a silent-failure generator — it hid the missing type-130 channel for the upstream
   project's entire history. Omit the entry from the response array (what Apple's receiver does) and log
   loudly.
10. **Never advertise a stream you haven't implemented.** Rules 1 and 9 combined: advertise
    `enhancedSiri` and iOS will SETUP AuxIn 107 and AuxOut 106; advertise `mainBuffered` and it will
    SETUP the buffered stream. Same shape as the type-130 bug. Implement, then advertise.

    `enhancedSiri` is a DSP project, not a protocol one: always-on mic under continuous processing, an echo
    canceller referenced to the car's own speaker output, a historical ring buffer, keyword and voice-activity
    detectors, and a three-way mixer (`ocbm/docs/carplay/03_SDK_GROUND_TRUTH.md` §7 "Audio + Siri");
    implausible from an unprivileged app. Classic Siri via the steering-wheel button needs none of it (`ocbm/docs/carplay/05_METADATA_AND_CONTROLS.md`,
    "Mic uplink is NOT required for the button"). Ship Classic; skip Enhanced.

---

## 9. Measured timings

Wireless reconnect of a known device, iPhone-side log, 2026-08-02:

BT service connect → fast-reconnect 51 ms; fast-reconnect → `connect` command (Wi-Fi bring-up + Bonjour) 3.40 s;
Bonjour SRV + DNS + TCP connect 8 ms; pair-verify (both round trips) 120 ms; `POST /auth-setup` (MFi-SAP)
**1.91 s**, the slowest step; SETUP phase 1 8.8 ms; `GET /info` (36,635 B) 92 ms; RECORD 14.6 ms; TCP connect →
RECORD answered 2.19 s.

That session died at RECORD on the cornerMasks error and never streamed. Inbound to first rendered frame on a
later session: **2.598 s** (`RECORD done` to first frame 875 ms), [`12_OBSERVED_FLOW.md`](12_OBSERVED_FLOW.md).

Phone-enforced timeouts: 10 s on every normal request, 1 s on teardown, 30 s temporary assertion on the
`connect` command.

---

## 10. What this means for the gm_ccpa app

**Split.** Phases A and B stay on the CCPA, unchanged. The app implements C through G in full.

**The MFi chip is called six times per session, four of them from the app over OCBM `CH_MFI`:**

| # | Where | Caller | Transport |
|---|---|---|---|
| 1, 2 | Phase B — BT iAP2 cert + sign | box `mfi_local` | local i²c |
| 3, 4 | Phase D5 — `/auth-setup` MFi-SAP sign + cert | **app** | **OCBM `CH_MFI`** |
| 5, 6 | Phase F — tunnel iAP2 cert + sign | **app** | **OCBM `CH_MFI`** |

The `local-mfi` feature gate must cut inside `iap_tunnel.rs` at its two chip call sites (`fn mfi_cert`,
`fn mfi_sign`: the `#[cfg(feature = "local-mfi")]` arms), not at the module boundary; gating the module out
would remove the metadata/controls plane.

**OCBM relay latency is a non-issue.** `/auth-setup` costs 1.91 s end to end, essentially all of it the
coprocessor's signature poll; a USB round trip of a few hundred bytes is noise against the phone's 10 s request
timeout.

**Budgets, outermost to innermost.**
- Box: `ocbmd`'s own I2C driver polls the chip under a 2.5 s wall-clock deadline (`SIGN_POLL_DEADLINE` in
  `Mfi::sign()`, `box/ocbmd/src/main.rs`), reached from `handle_mfi`. It replaced an iteration-count loop
  observed running ~7.1 s under chip NAK. `crates/vendor/mfi-i2c-local` carries an identical constant but
  serves `mfid` and the reference `carplayd`, not the `CH_MFI` path.
- No in-box retry on this path. `mfi_retry` (3 attempts, `iap_tunnel.rs`) is compiled only under `local-mfi`, so
  it does not apply to the app; on a `local-mfi` host the poll worst case is 3 x 2.5 s = 7.5 s.
- App, `OcbmClient` budgets cover the USB round trip, the box poll and the lock wait out of one clock. Two tiers:
  - `/auth-setup` (the `ControlServer` signer, `RemoteMfiSigner { fast: false }` in `native/carplay-jni`) and the
    bring-up probe (`OcbmProbe.runAllLocked`) use the defaults, 12 s cert / 15 s sign (`MFI_CERT_TIMEOUT_MS`,
    `MFI_SIGN_TIMEOUT_MS`). The relay serves the certificate from the link's `cachedCertificate()` once any read
    succeeded, so the 12 s applies only to the first read; the signature is never cached.
  - The iAP2 tunnel (`RemoteMfiSigner { fast: true }`, registered through `set_remote_signer`) uses the 4 s
    `copyCertificateFast` / `createSignatureFast` (`OcbmProbe.mfiRelay`; cert ~1.1 s, sign ~1.7 s per
    `iap_tunnel.rs`). Its chip ops run under the module-wide `SESSION` mutex, so the budget must stay short.
  The phone's request timeout is 10 s.

**Chip contention stays benign.** The six calls are sequenced by the protocol flow — B before D before
F — and with no wired path there is no competing `iap2d`. Overlap is possible only if Bluetooth
re-establishes during a live session.
