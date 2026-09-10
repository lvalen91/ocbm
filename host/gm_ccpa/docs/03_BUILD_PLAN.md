# Build plan — direct-WiFi CarPlay receiver app + modified CCPA

Maps every component of the design to reuse/move/create, with file paths. Read `01_FINDINGS.md` first
for the underlying feasibility work. Wire-level session ordering lives in
[`05_SESSION_FLOW.md`](05_SESSION_FLOW.md) — read its §8 ("ordering rules that kill sessions") before
writing protocol code.

Sources analyzed:
- `~/Documents/carlink/ccpa_custom` — the OCBM stack + AirPlay receiver core (Rust) + macOS OCBM host app (Swift).
- `~/Documents/carlink/carlink_native_personal` — the current working stock-protocol Android app (renderer/touch/UI to salvage).
- `~/Documents/carlink/carplay_simulator` — Apple's CarPlay Simulator (protocol reference).

---

## 1. Architecture — three links, one new endpoint

Canonical architecture + session flow: [`04_SYSTEM_MODEL.md`](04_SYSTEM_MODEL.md). This section is the
build-facing summary. The design is wireless-only — the iPhone is never wired to the CCPA, so `iap2d`,
`hot_handover`, and the wired/wireless arbitration machinery are out of scope.

```
      ┌─ BT (CCPA's own radio) ──────────────┐
iPhone┤                                       ├─ CCPA adapter ─ USB/OCBM (0x1314:0x2d00) ─ HEAD-UNIT APP
      └─ 5GHz WiFi (vehicle hotspot, br0) ─────────────────────────────────────────────► (AirPlay endpoint)
```

- **iPhone ↔ CCPA over BT**: pairing/iAP2 handshake + MFi auth (CCPA's coprocessor). Triggers the WiFi handoff.
- **CCPA ↔ App over USB/OCBM**: control + the MFi challenge relay only. No media.
- **iPhone ↔ App over WiFi**: the app is the AirPlay endpoint on br0 (`192.168.5.1`). H.264/HEVC video +
  AAC/AAC-ELD audio flow here, bypassing USB entirely.

The CCPA stops being the WiFi/AirPlay termination point; that role moves into the app.

---

## 2. The OCBM link shrinks dramatically

VID:PID `0x1314:0x2d00`, `bDeviceClass=0`, Android Open Accessory gadget (`functions=accessory`,
`/dev/usb_accessory` on the box). Channel map is `crates/ocbm-proto/src/lib.rs`.

| OCBM channel | Stock OCBM use | New design |
|---|---|---|
| `CH_CTRL 0x0000` | HELLO/SUBSCRIBE/HEARTBEAT/SESSION_EVENT/PAIRING_CODE | KEEP — handshake, BT bring-up trigger, session anchor, PIN |
| `CH_MFI 0x0001` | host↔box coprocessor bridge | KEEP — the MFi challenge relay (§4) |
| `CH_MGMT 0x0040` | box control/health/bond mgmt | KEEP — reboot, forget-bond, health, identity snapshot |
| `CH_CONSOLE/FILE/IP/ETH` | rescue console, deploy, NCM IP tunnel | keep console/file for deploy; IP/ETH unused (media isn't tunneled) |
| `CH_VIDEO 0x0020` | box→host H.264 | DROP — video comes over WiFi to the app |
| `CH_MEDIA_AUDIO/ALT_AUDIO 0x0021/22` | box→host audio | DROP — audio over WiFi |
| `CH_ALT_VIDEO 0x0024` | box→host cluster screen | DROP — app terminates it over WiFi |
| `CH_METADATA 0x0023` | box→host NowPlaying | DROP — metadata over the WiFi iAP tunnel |
| `CH_INPUT 0x0030` | host→box HID | DROP — app sends HID over WiFi |
| `CH_MIC 0x0031` | host→box mic uplink | DROP — app sends mic over WiFi |

Net: after handoff, OCBM carries only CTRL + MFI + MGMT — kilobytes, not megabits.

**How "DROP" is actually achieved.** OCBM has no per-channel SUBSCRIBE: `CT_SUBSCRIBE` is a presence
latch plus an opaque YAML blob, and nothing in the wire format lets a host decline a channel. Those rows
go silent because the box never spawns its A/V layer in this role — the `av::ensure_av_layer()` call at
the tail of the `0x5702` handler is skipped. This is a box-side role change, not a protocol negotiation,
which is why the role flag (§11 B3) is load-bearing rather than cosmetic.

---

## 3. Component disposition (Rust crates + daemons)

`crates/vendor/` inventory with the keep/move decision:

| Crate / daemon | loc | Disposition |
|---|---|---|
| `wireless` (bt_bringup, bt_driver, ssp_agent, sdp_server, sdp_client, rfcomm, reconnect, mfi_local, **wifi_handoff**, box_identity, av, arbiter_client) | ~4000 | STAYS on CCPA — BT bring-up, RFCOMM-iAP2, WiFi handoff |
| `mfi` + `mfi-i2c-local` | 365+295 | STAYS on CCPA — the coprocessor. App reaches it via `RemoteMfiSigner` over `CH_MFI` (§4) |
| `ocbmd` (daemon) | — | STAYS, unchanged — OCBM multiplexer. Its `CH_MFI` service handler (`handle_mfi`) already exists and works |
| `iap2-core` | 6915 | SHARED — BT-side identify stays on CCPA; the message/spec/TLV half is also needed app-side for the WiFi iAP tunnel |
| `receiver` (server, session, stream, datastream, info, hid, iap_tunnel, uplink, events, net, forward, vehicle_config, levers) | ~8300 | MOVES to app (JNI, §4) — the AirPlay receiver core |
| `pairing` (setup/srp, verify, crypto, tlv) | 958 | MOVES to app — pair-setup/pair-verify crypto |
| `rtsp` | 615 | MOVES to app — RTSP control-plane codec |
| `metadata` | 2406 | MOVES to app — NowPlaying/RouteGuidance TLV |
| `eld-codec` | 95 | DROP/optional — head unit has native AAC-ELD (MediaCodec) |
| `rx-connect` | 238 | REPLACE with Android `NsdManager` (advertise `_airplay._tcp`, browse) |
| `carplayd` (daemon; was `ccpa/airplayd` before commit `9c7191a`) | — | NOT SPAWNED in this role. Its receiver orchestration moves into the app; its `LocalMfiSigner` role is already covered by `ocbmd`'s `handle_mfi` |

---

## 4. The keystone: sans-IO `ControlServer` + `MfiSigner` trait → JNI reuse

- **`receiver::server::ControlServer` is sans-IO** (`crates/vendor/receiver/src/server.rs`): a
  transport-free RTSP state machine. The real signature is `ControlServer<'a, P: Pairings, S: MfiSigner>`,
  `feed()` returns `Result<Vec<u8>, ServerError>`. The `'a` borrow is a genuine JNI design constraint —
  it cannot be boxed into a long-lived native handle as-is; handled in practice by holding `&'static
  Identity` via `OnceLock` (proven §5d).
- **The socket is not injected into `ControlServer`.** It is driven from outside by
  `receiver::net::serve_connection<T: Read + Write>` — the app supplies the transport, one level up from
  where the server sits.
- **`ControlServer` is generic over MFi.** `mfi::auth_client::MfiSigner`
  (`crates/vendor/mfi/src/auth_client.rs`) is two methods: `copy_certificate()` and
  `create_signature(digest)`. `struct LocalMfiSigner` (on-box chip, `ccpa/carplayd/src/main.rs`) implements it
  against `/dev/i2c-1`.
- **`MfiAuthClient` no longer exists** (removed as audit Fix #19 — corrected 2026-09-09, was
  previously described here as dead code worth copying). `crates/vendor/mfi/src/auth_client.rs` now
  contains only the `MfiSigner` trait (`auth_client.rs:9-15`); the removal is recorded in the file's
  own header comment (`auth_client.rs:3-4`). There is no surviving struct whose wire-format shape can
  be copied — `RemoteMfiSigner` is new work against the `MfiSigner` trait shape, full stop.

**The app runs `ControlServer<_, RemoteMfiSigner>`**, where `RemoteMfiSigner` relays `copy_certificate`
/ `create_signature` over OCBM `CH_MFI` to the CCPA's coprocessor. **The CCPA side needs nothing new**:
`ocbmd`'s `handle_mfi` already exists and works — real I²C, the shared `/tmp/carplay_mfi.lock`,
`CAP_MFI` advertised in `HELLO_ACK`, sits on the reliable high-priority queue.

**JNI feasibility: HIGH** (verified §5d — real cross-build, not estimate). No async/tokio in the core,
`#![forbid(unsafe_code)]`, pure-Rust deps (`plist`/`chacha20poly1305`/`hkdf`/`sha2`/`serde`) cross-compile
cleanly. `pairing`, `rtsp`, `metadata`, `iap2-core` relocate with zero hardware/C deps.

### `receiver` doesn't move wholesale — the cut line is inside `iap_tunnel.rs`

`receiver` has an unconditional path dep on the box-only `mfi-i2c-local` via `iap_tunnel.rs` (direct
`/dev/i2c` chip access at two call sites: `Action::SendCert` and `Action::SignChallenge`). It compiles
for Android — it's just `libc` — but faults at runtime.

**Do not gate out `iap_tunnel`.** It is the iAP2-over-AirPlay-DataStream path that carries NowPlaying,
RouteGuidance and call state after the handoff — a wireless-only design needs it *more*, not less.
Feature-gating it off would silently delete the metadata and controls plane.

**The correct cut is inside the module:** route its two chip call sites through the `S: MfiSigner`
trait, the way `ControlServer` already does, instead of calling `mfi_i2c_local` directly. Then
`mfi-i2c-local` alone goes behind `local-mfi` (off on Android) and the tunnel keeps working.

Consequence: the app has two `CH_MFI` consumers, not one — `auth-setup` and the tunnel's own Identify.
Four relayed chip ops per session; see §9.

`eld-codec` (native libfdk-aac) is already optional/off-by-default — leave it off (native AAC-ELD on the
head unit).

Alternative to JNI: reimplement in Kotlin. Not recommended — it discards ~19k loc of tested
crypto/protocol across the six crates §10 relocates.

---

## 5. The `wifi_handoff` change (swap ONE source call)

`wifi_handoff.rs` is not an unfinished scaffold — its module header says so but the code contradicts
it. The `0x5703` path is already wired: `read_hostapd_ap_config()` (parses `/etc/hostapd.conf`) is
dispatched from `crates/vendor/wireless/src/bt_driver.rs:417` on the `0x5702` request (corrected
2026-09-09; was misquoted as `bt_driver.rs:245`), inside the same `0x5702` handler that calls
`build_accessory_wifi_configuration_information` at `bt_driver.rs:426` to build `0x5703`, and writes
it on control session 1.

The `0x5703` builder is generic and reusable as-is:

```rust
build_accessory_wifi_configuration_information(&AccessoryWiFiConfig { ssid, passphrase, security_type, channel })
```

**Change:** replace the `read_hostapd_ap_config()` call at `crates/vendor/wireless/src/bt_driver.rs:417`
with a source fn that
builds `AccessoryWiFiConfig` from app-supplied vehicle creds handed up over OCBM (`myChevrolet 32D4` +
user passphrase + WPA2 + 5 GHz channel), reusing the same `has_wpa && !pass.is_empty() →
Wpa2OrWpa3Personal` mapping. The builder, TLV layout, enums, and session-1 dispatch are untouched. Needs
one new OCBM control message carrying `{ssid, passphrase, channel}` + a holder the `0x5702` handler
reads. Note the two distinct `SecurityType` enums (`0x5701` device-side vs `0x5703` accessory-side), and
that `0x5703` has no param 0 — the builder already handles this. The `0x5700→0x5701` sub-flow is the
only genuinely un-wired part, and this design doesn't need it.

**Device-proven end-to-end 2026-08-04** — see `06_BRINGUP_RUNBOOK.md` §2.

---

## 6. The head-unit app (Kotlin shell + JNI'd Rust core)

| Layer | Source / plan |
|---|---|
| USB claim + OCBM client | Port the macOS `OCBM/OCBMClient.swift` + `OCBMFraming.swift` (`host/MacHost`) to Kotlin; claim `0x1314:0x2d00` via `UsbManager` host mode. Reuse `carlink_native_personal`'s existing USB-claim/permission plumbing. |
| WiFi sockets | `ServerSocket`/`DatagramSocket` bound on br0, fed into the JNI'd `ControlServer`. |
| AirPlay/CarPlay core | JNI'd Rust `receiver`/`pairing`/`rtsp` (§4), with `RemoteMfiSigner` over OCBM. |
| Video | Android `MediaCodec` (HW `OMX.Intel.hw_vd.h264` and `.h265`) → `Surface`. Negotiate HEVC (`CARPLAY_HEVC`/`extendedFeatures`) to cut 5 GHz bitrate. Renderer salvage from carlink_native — §7. |
| Audio | `AudioTrack` (native AAC-ELD via MediaCodec); mic uplink AAC-ELD encode native. Car audio zone `bus0_media_out`. |
| Touch / HID | Capture touches → the Rust `hid` module's report format → over the WiFi HID channel. Touch salvage from carlink_native — §7. |
| Discovery | `NsdManager` advertise `_airplay._tcp` (replaces `rx-connect`). Distinct deviceID from GM's `:7000` so the iPhone routes to us. |
| Identity | One identity sourced once, used by the CCPA's BT phase AND the app's br0 Bonjour advert. Wireless-only retires the wired/wireless split; `box_identity` reads the `wlan0` MAC, which is absent once the box stops raising its AP. |
| UI | Salvage `carlink_native_personal`'s projection view + design (§7). |

### macOS host app = the closest template
`host/MacHost` already claims `0x1314:0x2d00`, speaks OCBM (`OCBMClient`/`OCBMFraming`), decrypts
A/V seams (`OCBMAVDecrypt`), decodes H.264 (`H264Decoder` via VideoToolbox), plays audio, sends input.
The Android app is its OCBM-client + render + input halves, plus the receiver core the macOS app
delegated to the CCPA (because now the app — not the CCPA — is the WiFi endpoint), minus the OCBM A/V
seam path (media arrives over WiFi instead).

---

## 7. Salvage from `carlink_native_personal` (Compose app, package `com.carlink`, AAOS `gminfo37` 2400×960)

**Key structural fact:** the app offloads all codec + protocol work to the CPC200 adapter and only ever
handles linear PCM audio + Annex-B H.264. Protocol coupling funnels through one god-object facade —
`CarlinkManager.kt` (3131 loc) — behind clean observer interfaces (`CarlinkManager.Callback`,
`DeviceListener`, `MediaControlCallback`, `updateMetadata(...)`). The UI/media/video/audio layers never
touch USB; they call the facade. So the central task is to build a wireless/OCBM facade that
re-implements the same upward-facing contracts — the salvaged layers then drop on structurally intact.
There is no transport interface today (callers bind concrete classes), so that facade is new code.

**Crown-jewel reuse (as-is or near):**

| Component | File(s) | Rating | Notes |
|---|---|---|---|
| **H.264 renderer** | `video/H264Renderer.java` (1052 loc) + `ui/components/VideoSurface(View).kt` | REUSE 7/10 | Hardened HW `MediaCodec` (sync mode) → `Surface` via `SurfaceView` (HWC overlay). Only input is `feedDirect(bytes)` — transport-agnostic; decoder core is production-hardened (sync-gate, watchdog, reactive keyframes, surface-swap, CSD cache). Gap is entirely at the bitstream boundary, needing four changes for WiFi: (a) avcC→Annex-B shim (WiFi CarPlay H.264 is length-prefixed; the NAL parser assumes start codes — verify wire format first); (b) feed out-of-band SPS/PPS as a synthesized `[SPS][PPS][IDR]` Annex-B bundle for the first frame; (c) repoint `KeyframeRequestCallback` from the USB `FRAME` cmd to the wireless force-IDR request; (d) raise `STAGED_FRAME_CAPACITY` (2 MB drops keyframes at ~2400×960 — bump to ~6–8 MB). H.264 ONLY — HEVC (which the real session negotiates: `supportsHEVC=true`) needs a separate renderer (2-byte NAL header, `(byte>>1)&0x3F` type). |
| **Touch model** | `ui/MainScreen.kt` `handleTouchEvent()` (~731–839) | ADAPT | Normalized 0.0–1.0 multi-touch with per-pointer state, ~0.3% deadband, `ACTION_CANCEL` handling, DOWN→MOVE demotion. Protocol-neutral — reuse the logic + `TouchPoint(x,y,action,id)` verbatim; swap only the wire serialization (CarPlay wireless touch is also normalized 0..1). |
| **Frosted-glass design** | `ui/theme/FrostedGlass.kt`, `ui/theme/Theme.kt`, `ui/components/LoadingSpinner.kt` | REUSE verbatim | Zero protocol imports; day/night derived from `surface.luminance()`. |
| **Media3 AAOS session stack** | `media/MediaSessionManager.kt` (1525 loc), `media/UsbAdapterPlayer.kt`, `media/CarlinkMediaBrowserService.kt`, `media/AlbumArtCache.kt` | ADAPT (keep arch) | Field-proven GM-AAOS knowledge: dual-carrier album art via FileProvider with per-package read-grants, stale-card avoidance, FGS reconciliation, seek/duration dedup. Entry points `updateMetadata(...)` + `MediaControlCallback` are already protocol-neutral seams — re-point source/sink only. `AlbumArtCache` reuses verbatim. |
| **PCM output engine** | `audio/DualStreamAudioManager.kt` (output half), `audio/AudioRingBuffer.kt`, `platform/AudioConfig.kt` | ADAPT | Five USAGE-tagged `AudioTrack`s (MEDIA/SIRI/PHONE/ALERT/NAV) → GM CarAudioService bus routing, per-purpose AudioFocus SM, urgent-priority playback thread with prefill/underrun/silence-fill. Reuse the output choreography; feed it from a new AAC decoder instead of USB PCM. `AudioRingBuffer` + `AudioConfig` reuse as-is. |
| **Platform/infra** | `platform/PlatformDetector.kt`, `logging/Logger.kt`, `util/*`, `MainActivity` immersive/window-metrics | REUSE/ADAPT | Decoder selection (Intel/GM detect), DPI/native-rate, immersive + even-dimension logic. Drop the USB ViewArea/SafeArea blob building. |

**Replace entirely:** all of `usb/` + `protocol/` (`UsbDeviceWrapper.kt`, `AdapterDriver.kt`,
`MessageParser/Serializer.kt`, `MessageTypes.kt`) — stock-Carlinkit USB (VID/PID `0x1314:0x1520/1521`,
magic `0x55aa55aa`). Salvage one thing from it: `MessageTypes.CommandMapping` is a useful semantic map of
the control events CarPlay expects (Siri 5/6, media 200–205, knob 111–114, D-pad 100–106, phone
300–314) — keep it as a reference when mapping controls to iAP2/HID.

**Build-from-scratch (no salvage in this app):**
1. The OCBM/wireless facade re-implementing the `CarlinkManager` upward contracts (so salvaged layers
   attach unchanged). This is where the JNI'd Rust receiver core (§4) + OCBM client (§6) land.
2. An AAC / AAC-ELD decode (downlink) + encode (mic uplink) stage — this app is pure PCM (the adapter
   did all codec work; grep confirms zero `MediaCodec`/AAC in audio). The head unit has native AAC-ELD
   (MediaCodec), so it's buildable, just new.
3. The avcC→Annex-B shim + the HEVC decode path (§ table above).

---

## 8. Apple reference to build against (`carplay_simulator`)

- `…/iAP2MessageKit.framework/…/iap2messages-external.i2mspecarchive` — authoritative iAP2 message +
  TLV parameter dictionary (message ids, param ids, enums). Same archive `ccpa_custom/tools/i2mspec_dump.py` reads.
- `logs/20260731-062508/oslog-stream.log` (445 MB) — a real receiver-side CarPlay session vs an
  iOS 27 iPhone: the exact handshake ordering (Bonjour `_airplay._tcp`:7000 → pair-verify → AuthSetup →
  SessionSetup → per-stream StreamSetup → DataStream/VehicleData/MediaControl) and decoded feature-flag
  names (HEVC, H.264 L5.1, Alt Screen, Enhanced Siri, iAP channel, …). The best "what happens on the
  wire, in what order" reference.
- `…/CarPlaySDK.framework/…/VDCSchema-External.json` — vehicle-data characteristic UUIDs/formats.
- `…/Resources/VehicleConfigs/Configs/*.yaml` — 10 capability/geometry/HID/audio profiles the accessory
  declares at setup. Maps directly onto `receiver::info` / `receiver::vehicle_config`.
- `doc/input-to-wire.md` + per-control HID docs — HID report byte layouts (tier-2, unverified;
  cross-check `doc/VERIFICATION-BACKLOG.md`).

---

## 9. The `CH_MFI` relay is live every session — and carries four ops

`pair-setup` and `pair-verify` never touch the chip (pure SRP-6a / Curve25519 / Ed25519). Two things do:

| # | Where | Ops | Path |
|---|---|---|---|
| 1, 2 | Box, BT-side iAP2 `0xAA01`/`0xAA03` | cert + sign | local i²c, stays on the CCPA |
| 3, 4 | App, `Route::AuthSetup` → MFi-SAP (`mfi/sap.rs`), after pair-verify on every connect | sign then cert (signature first) | OCBM `CH_MFI` |
| 5, 6 | App, the tunnel's own iAP2 Identify over DataStream 130 | cert + sign | OCBM `CH_MFI` |

Six chip operations per session, four relayed by the app. See [`05_SESSION_FLOW.md`](05_SESSION_FLOW.md)
for phase letters; rows 5–6 are why §4's `local-mfi` cut line had to move inside `iap_tunnel.rs`.

**Design impact:** the OCBM link is never idle during a session; `RemoteMfiSigner` must stay wired for
the whole session lifetime, not torn down after pairing.

**Latency is not a problem, but budget the tail.** `/auth-setup` measures 1.91 s end-to-end on real
hardware — essentially all of it the chip's signature poll — against a 10 s phone-side request timeout,
so a USB round trip of a few hundred bytes is noise. But the poll is bounded at 2.5 s by a wall-clock
deadline in the driver (not firmware); the older loop was observed running ~7.1 s under chip NAK, and
with up to 3 MFi retries that approaches the ceiling.

Settling test if ever doubted: disable the relay after first pair; AirPlay `auth-setup` M1→M2 is expected
to fail.

**Device-proven:** the certificate (945 B) and signature (128 B, RSA-1024) both came back over `CH_MFI`
from an ordinary app UID — see `06_BRINGUP_RUNBOOK.md` §1.

---

## 10. Language & module boundary (DECIDED: Rust core + Kotlin shell)

**ABI: three architectures, all different.** `adb shell getprop ro.product.cpu.abilist` on the head unit
returns `x86_64` — consistent with the Intel HD 505 / `OMX.Intel.hw_vd.*` decoders.

| Target | ABI | Notes |
|---|---|---|
| GM head unit (the real target) | `x86_64` | build `x86_64-linux-android` |
| Android Automotive emulator | `arm64-v8a` | Apple-Silicon host; build `aarch64-linux-android` too if you want emulator runs |
| CCPA adapter (box side, unchanged) | armv7 musl | `armv7-unknown-linux-musleabihf`, as upstream |

The APK is pure dex with no `lib/` entries until the JNI core lands, so this costs nothing to fix now.

Not "Rust vs native" — both, split at the protocol/platform line.

| Layer | Language | Why |
|---|---|---|
| USB/OCBM client, WiFi sockets, `MediaCodec`→`Surface`, `AudioTrack`/car-audio, `NsdManager`, touch, Compose UI, `MediaSession` | Kotlin/Java | mandatory Android framework APIs; this is exactly the salvaged `carlink_native_personal` code |
| RTSP state machine, pair-setup/verify, MFi-SAP auth-setup, ChaCha20 control/stream framing, iAP2 TLV, identity crypto | Rust (JNI) | ~19k loc tested/clean-room; correctness-critical; the relocation seam (`ControlServer` generic over `MfiSigner`, remote signer) is already built |
| A/V decrypt | Rust or Kotlin | the one real toss-up — Rust `receiver::stream` for reuse, or Kotlin native `ChaCha20-Poly1305` for less JNI data traffic. Lean Rust to keep nonce/AAD logic in one place |

**JNI performance is a non-issue here** (do NOT let it push you to an all-Kotlin rewrite):
- Crossings are coarse-grained — per network buffer / per frame, not per byte/sample. Video ≈ 30–60
  crossings/sec, audio ≈ 1000/sec, each ~10–40 ns, single-digit µs/sec against ms frame budgets.
- Bulk A/V passes zero-copy via direct `ByteBuffer` (`NewDirectByteBuffer` / `GetDirectBufferAddress`) —
  the 30 Mbps stream is never copied across the boundary; `MediaCodec` reads it in place.
- The throughput-critical work is identical either way: H.264/HEVC decode is fixed-function silicon via
  `MediaCodec` (Kotlin-side regardless), render is the HWC `Surface`, audio is `AudioTrack`. Rust only
  decrypts (ChaCha20 ≈ GB/s). An all-Kotlin build runs the same pipeline for zero decode speedup.
- Mild plus for Rust: no GC ⇒ no GC-pause jitter in the data-plane crypto.

The real costs of the Rust path are engineering, not runtime: `cargo-ndk` build/packaging (see §5d for
the working recipe without it), cross-boundary debugging, native-memory/JNI-ref discipline. The one
gotcha to get right: Rust owns its worker threads and calls `AttachCurrentThread` once at startup (never
per-call), caching method IDs and reusing the direct buffers — per-call attach/detach is the only thing
that would introduce real overhead.

**Cross-build proven end-to-end** (`06_BRINGUP_RUNBOOK.md` §5d): all six crates cross-compile clean to
`x86_64-linux-android`, and a `cdylib` linking `receiver` + `pairing` runs on the head unit.

---

## 11. Build order

### Method: grow the prober into the instrument, then graduate a product out of it

Each capability lands first as another NetProbe section — isolated, logged to `NETPROBE`, exportable via
SAF — before it becomes product code. A failing brick then reports rather than crashes, which is the
right posture for protocol bring-up where most failures are silent (§8 of `05_SESSION_FLOW.md` lists
nine that produce no diagnostic at all).

**Keep the prober permanently.** This mirrors how `ccpa_custom` itself worked: `host/ocbm-host` (a CLI
instrument with `hello`/`mfi`/`console`/`avdec`/`session`/`pull` subcommands) and `host/MacHost` (the
product) both still exist, and the CLI is what gets reached for when the product misbehaves. Graduate a
clean app out of the foundation once it holds; don't throw the instrument away.

**Two things built in from brick one, because retrofitting them is expensive:**

- **The transport seam.** Port the macOS `RawBulkTransport` shape — four methods (`writeBulk`,
  `setReadHandler`, `start`, `stop`), no platform types. That project proves the value by compiling its
  entire OCBM client against a `FakeTransport` with no hardware.
- **`device_filter.xml` + an `ACTION_USB_DEVICE_ATTACHED` intent filter** on `0x1314:0x2d00`. In a
  wireless-only design the app is the ignition — the box's radios are off at boot and wake on our
  `CT_SUBSCRIBE` — and there is no other physical trigger. Plugging in the adapter becomes it.

### Track A — no hardware required (COMPLETE — see §5c of the runbook)

**A1. Pairing crypto.** `pair-setup` (TLV8 SRP-6a) and `pair-verify` (Curve25519 + Ed25519) are pure
software — no chip, no adapter, no OCBM (§9). **Device-proven** against a real iPhone; closes a gap the
`pairing` crate's own source flagged as unclosable offline ("we have no captured pair-setup").

**A2. CarPlay identity in the TXT record.** Swap the probe's AppleTV identity for the Car-bit-bearing
value. **Device-proven** — the phone leads with `pair-verify` instead of `pair-setup`.

### Track B — gated on hardware (COMPLETE — see §1–§2 of the runbook)

**B0. Flash a spare CCPA adapter** to the OCBM state (PID 2d00, `ocbmd` + `wireless`;
`ccpa_custom/tools/ocbm_boot.sh` + `install_fhs.sh`).

**B1. USB claim + OCBM client.** `UsbManager` claim of `0x1314:0x2d00`, interface 0, bulk endpoints
discovered by walking the interface (IN `0x81` / OUT `0x01` — device-measured; do not hardcode), no AOA
control handshake. 16-byte LE framing with resync. `CT_HELLO` → `CT_HELLO_ACK` → `CT_SETTIME` → 1 Hz
`CT_HEARTBEAT`. Reference `host/ocbm-host`'s `Link` over the Swift client — it shares the canonical
`ocbm-proto` codec so it cannot drift from the box. **Device-proven.**

**B2. `CH_MFI` round trip.** Relay `copy_certificate` + `create_signature` to the real chip. Needs zero
box-side changes. **Device-proven** — 945-byte certificate, 128-byte RSA-1024 signature, from an
ordinary app UID.

**B3. CCPA bridge role** (first box-side code). Skip `av::ensure_av_layer()`; drop `wlan_on.sh` from the
wireless launch wrapper so the box raises no AP; swap the `read_hostapd_ap_config()` call in
`bt_driver.rs` for app-supplied vehicle creds arriving over OCBM. Gated behind a role flag in the
`CT_SUBSCRIBE` YAML. **Device-proven** — BT comes up, no `hostapd` runs on the box, no `carplayd` is
spawned.

**B4. BT bring-up + handoff, driven from the app.** **Device-proven** — the iPhone pairs, completes iAP2
identify, sends `0x5702`, receives the truck's credentials, and joins `myChevrolet 32D4`. iOS retries
`0x5702` (observed twice per session); the handler is idempotent.

### Converged

**C1. JNI the receiver core.** Cross-compile `receiver`+`pairing`+`rtsp`+`mfi`+`metadata`+`iap2-core` to
`x86_64-linux-android` — not `aarch64` (§10). Gate `mfi-i2c-local` only behind `local-mfi` (off on
Android) and route `iap_tunnel`'s two chip call sites through the `MfiSigner` trait — do not gate out the
tunnel itself (§4). **Device-proven end to end** (§10, §5d of the runbook): all six crates cross-compile
and the linked `.so` runs on the head unit.

**C2. WiFi endpoint on br0.** `NsdManager` advertise with a distinct deviceID from GM's `:7000`, plus the
`_carplay-ctrl._tcp` browse and connect-out — discovery is bidirectional. Wire the sockets into
`ControlServer`. **Device-proven** — `GET /ctrl-int/1/connect` accepted, the phone opens the control
connection inbound (runbook §5b).

**C3. RECORD — the real gate.** Done when RECORD is answered, the event channel is accepted,
`sessionStarted` is set, and the session survives 60 seconds without `Endpoint_Failed`. This is where
sessions die: negotiate a feature you don't back and the phone deactivates in ~21 ms. Only negotiate what
is actually implemented. **Partially proven** — pair-setup is device-verified (runbook §5c); RECORD
itself is the open item, see runbook §6.

**C4. Streams, one at a time.** 130 DataStream (non-zero `streamID` is mandatory), 110 screen, then
100/101/102 audio as they arrive. Expect SETUPs throughout the session, not batched. **Device-proven** —
a full A/V session with both stream SETUPs, HEVC decode, and AAC playback (runbook §5g).

**C5. Media/UI salvage.** `H264Renderer` (+ avcC→Annex-B shim, out-of-band SPS/PPS, capacity bump,
keyframe retarget) + a new HEVC renderer; new AAC/AAC-ELD decode+encode; `DualStreamAudioManager`
output; touch model; FrostedGlass UI; Media3 stack.

**C6. Identity consistency.** One identity for both the CCPA BT phase and the app's br0 Bonjour advert,
sourced once and shared over OCBM (`MGMT_GET_INFO` already returns a snapshot). `box_identity` derives
from the `wlan0` MAC, which does not exist once the box stops raising its AP.
