# captures — reference logs from live on-hardware runs

Real device logs kept as ground-truth reference (what the wire/state actually did), captured over the
UART console from the box during live iPhone tests. Not code; evidence.

> **Verification method (important):** these runs were driven by `ccpa/airplayd`, which for the
> on-hardware validation **reused another project's CarPlay receiver — `ncm_carplayd/receiver_core`** —
> as the harness: its proven `ControlServer` (pair-setup/verify + auth-setup) and, for the full-session
> capture, its `AvSession` (SETUP/RECORD/stream handling). This **proves the session works on the
> adapter**; it is **NOT** the committed final design, which forwards the *encrypted* A/V + the session
> key over OCBM to the host app (no on-box decode). Our own code is the `LocalMfiSigner` (genuine local
> MFi chip) and the OCBM/box plumbing. Each `.log` carries this note in its header.
>
> **PATH UPDATE 2026-08-16:** that sibling tree is archived at
> `~/Documents/carlink/old/ncm_carplayd/receiver_core`, and the receiver has since been
> vendored into this repo as `crates/vendor/receiver` — so the harness these headers name is today's
> in-repo receiver, not an external dependency.

## 2026-07-09 — first end-to-end AirPlay pair-verify on the box (Phase 1 complete)

The first time the adapter's full **stable crypto foundation** ran to completion against a real iPhone:
iAP2 → Identified → AirPlay pair-setup → pair-verify → MFi-SAP auth-setup → derived ChaCha20 session
key — all on the genuine **local** MFi chip, via `ccpa/airplayd` (582 KB, reusing `receiver_core`).

| File | What it shows |
|---|---|
| `2026-07-09_iap2d_handshake.log` | `iap2d` iAP2 accessory handshake: SYN-ACK → cert/0xAA01 → sign/0xAA03 → **0xAA05 AuthSuccess** → 0x1D01 → **0x1D02 Identify**, then holds the link (post-identify 0x4E0A/0x4E0B). MFi on the local i2c chip. |
| `2026-07-09_rx_connect.log` | `rx_connect` mDNS: advertised `_airplay._tcp 'CarPlay' :5000`; resolved the iPhone's `_carplay-ctrl` to `fe80::1c76:d88:9cb3:9036` (scope `ncm0`); **connect-out `GET /ctrl-int/1/connect` → `HTTP/1.1 200 OK`** (the fix: dial the resolved fe80:: address, not the `.local` hostname). The trailing IPv4 `169.254…` connect-out fails harmlessly (IPv6 already worked). |
| `2026-07-09_rx_connect_session.log` | `rx_connect` from the full-session run — the mDNS advertise + connect-out that re-established the streaming session. |
| `2026-07-09_airplayd_pairverify.log` | `airplayd` pairing server: control conn from the iPhone on `ncm0:5000` → `/pair-setup` ×3 (peer saved) → `/pair-verify` ×2 → **pair-verify OK → channel encrypted** → **`/auth-setup` MFi-SAP OK (1113 B M2, local chip)** → iPhone advanced to `SETUP` → **`*** PAIR-VERIFY COMPLETE — session secret derived ***`**. |
| `2026-07-09_airplayd_full_session.log` | **Full live CarPlay session** with `airplayd` running receiver_core's `AvSession` (validation step — box decrypts; the committed model forwards encrypted). pair-verify → `SETUP phase1` (timing/event ports) → `RECORD` (event channel, session-focus handshake) → `SETUP phase2 screen(110)` → **`[screen] first frame decoded (Annex-B)` — VIDEO flowing** → `SETUP phase2 audio(100) 48000Hz 2ch Pcm "media"` → **audio (the Music) flowing** → `/command modesChanged/disableBluetooth` + continuous `/feedback`. The `:9001`/`:9002` "connection refused" are the IPC-seam forwards with no listener on the box (expected — that's the app/OCBM's job). Proves the whole session (pairing + video + audio + control) works on the adapter with a real streaming iPhone. |

## 2026-07-09 — COMMITTED forward-encrypted model validated (video, Phase 2)

The first proof of the **committed architecture** on hardware: the box forwards the *encrypted* video
+ hands the session key over OCBM, and the **host decrypts** — no on-box decode.

| File | What it shows |
|---|---|
| `2026-07-09_avdec_forward_encrypted.log` | `ocbm-host avdec` on the Mac. `airplayd` ran with `OCBM_FWD_ENC=1` so `spawn_screen` handed the per-stream key once then forwarded raw encrypted frames (`[hdr 128B][body]`) → `:9001` → `ocbmd` → `CH_VIDEO`. The host app received the per-stream session key over the OCBM seam and decrypted its own stream with ChaCha20-Poly1305 (nonce = `[0,0,0,0]‖counter_le64`, AAD = the frame header): **`468 frames decrypted on HOST, 0 failed`** → `✓ committed model validated`. The decrypted plaintext parsed as clean AVCC H.264 (2 IDR + 466 non-IDR slices, 2 823 037 B exact). |

Why this is decisive: ChaCha20-**Poly1305** is an AEAD — a wrong key/nonce/AAD fails the auth tag. 468/468
successful decrypts is cryptographic proof the box→host key handoff + encrypted-frame framing are exact.
Audio still decodes on the box in this run (the video path is the proof; audio mirrors it next).

Key facts these pin down:
- The genuine coprocessor returns a 945-byte MFi certificate (standard PKCS#7 DER `30 82 03 ad 06 09 2a 86 48 86 f7 0d 01 07 02 …`) and 128-byte RSA-1024 signatures during normal authentication; the same chip performs both iAP2 auth and AirPlay MFi-SAP. (No key material leaves the chip.)
- The iPhone's CarPlay control service lives at its **IPv6 link-local** on the phone-facing NCM link;
  the box has no `.local` resolver, so dials must use the resolved address + interface scope.
- After pair-verify, `airplayd` (running `NoSession`) acks `SETUP` with placeholders — no A/V yet; that
  is **Phase 2** (forward encrypted A/V + hand the session key over OCBM to the host app).

## 2026-07-24 — wired regression test after docs/wireless/00_WIRELESS_CARPLAY.md Phase 1+2 (process/protocol fixes), live media + nav

Live wired CarPlay session (real iPhone, Music playing, Apple Maps navigation active) run immediately
after deploying the docs/wireless/00_WIRELESS_CARPLAY.md Phase 1 (process/supervisor) and Phase 2 (protocol) fixes to
`crates/vendor/wireless/src/av.rs`, `tools/session_supervisor.sh`, and `crates/vendor/receiver/src/{info,session,events}.rs`
— all 12-agent-reviewed, findings fixed, checksums verified on deploy. Purpose: prove the wireless-path
fixes introduced **zero regression** to the proven wired session, and capture a fresh reference sample
of wired metadata content. Pulled over **UART only** (not `ocbm-host`) because the host app held a live
OCBM session at capture time — using the OCBM CLI concurrently risks interleaving the live USB bulk
transfers.

| File | What it shows |
|---|---|
| `2026-07-24_iap2d_wired_metadata_session.log` | `iap2d` full session: SYN-ACK → cert/sign → AuthSuccess → Identify (all unchanged, local MFi chip) → `StartNowPlayingUpdates`/`StartRouteGuidanceUpdates`/`StartCallStateUpdates`/`StartCommunicationsUpdates` subscriptions → 263 live `NowPlaying` + 43 `RouteGuidance` + 31 `Maneuver` records. Confirms real content: a track (`"No More Tears"` / `"Distant Cowboy"` / `"No More Tears - Single"`, `duration_ms: 245558`) and live Apple Maps guidance (`route_state: Loading`, `nav_app: "Apple Maps"`, maneuvers with real distances e.g. `distance_text: "29"`/`Miles`). This path is the **separate physical `/dev/android_iap2` link** — untouched by this round's wireless-tunnel fixes — so its continued correct operation is the regression proof. |
| `2026-07-24_airplayd_phase12_session.log` | `airplayd` full session with the Phase 1+2 code: pairing, SETUP phase1/phase2, RECORD, screen+audio flowing — same shape as the 2026-07-09 baseline, confirming no regression from the `ensure_av_layer()`/`events.rs` changes. |
| `2026-07-24_rx_connect_phase12_session.log` | `rx-connect` mDNS advertise + connect-out, unchanged behavior. |
| `2026-07-24_session_supervisor_phase12.log` | `session_supervisor.sh` milestone scan reaching `armed=1 healthy=1 paired=1 record=1` (full STREAMING) — confirms the rewritten `wireless_owns_session()`-gated logic still drives the wired arm/health path correctly when no wireless session is active. |
| `2026-07-24_ocbmd_phase12_session.log` | `ocbmd` session log from the box side during the live test. |
| `2026-07-24_carplay_state_final.txt` | Final on-box state snapshot: `phase=STREAMING host_present=1 armed=1 healthy=1 stuck=0 paired=1 record=1 flaps=0 stuck_fails=0 uptime=282`. |
| `2026-07-24_lifecycle.ndjson` | Session lifecycle state-machine transitions for the run. |
| `2026-07-24_carplay_cmd_capture.bin` | Raw `[u32 LE len][plist]`-framed capture of the AirPlay `/command` channel (10548 B, decoded with `scratchpad/decode_cmd_capture.py`): 35 `modesChanged` + 1 each of `disableBluetooth`/`duckAudio`/`unduckAudio`, 38 frames total, **zero iAP2 tunnel frames** — expected for wired, since wired metadata flows over the separate `iap2d` physical link, not this AirPlay command channel. `duckAudio`/`unduckAudio` are new relative to the 2026-07-09 captures (not previously observed in this project's evidence). |
| `2026-07-24_carplay_event_capture.bin` | Raw capture of the AirPlay `/event` channel (224 B, 1 frame) — same decoder, confirms no iAP2 tunnel traffic on this channel either. |
| `2026-07-24_wireless_bootlogs_stale_from_boot.txt` | Stale `bt.log`/`wl.log`/`wlan.log`/`ocbm_boot.log` content from the box's most recent boot (pre-dates this test; wireless was not exercised in this run) — kept for completeness, not evidence of this test's behavior. |

Result: the wired session reached full `STREAMING` with real NowPlaying/RouteGuidance/Maneuver data
flowing end-to-end, confirming the docs/wireless/00_WIRELESS_CARPLAY.md Phase 1+2 wireless-path fixes are isolated and safe.
Phase 4 (wireless test with these fixes) and Phase 5 (wireless-Identify changes) remain untested.

## 2026-07-25 — wireless metadata

- `2026-07-25_SUCCESS_airplayd_wl_handshake.txt` — RCS DataStream link, MFi auth, identify (docs/carplay/05_METADATA_AND_CONTROLS.md).
- `2026-07-25_SUCCESS_artwork_session2.txt` — album artwork, byte-exact reassembly (docs/carplay/05_METADATA_AND_CONTROLS.md §1.8).
- `2026-07-25_iphone_iap2_trace_sess{2,3}.txt` — the phone's own iAP2 packet trace.
- `2026-07-25_iphone_iapreject_requiredinfomissing.txt` — `accessoryd` naming the rejected message ids
  and reasons. The datum that ended three sessions of bisection (docs/carplay/05_METADATA_AND_CONTROLS.md §6.3).
- `2026-07-25_SUCCESS_metadata_declaration_accepted.txt` — the full metadata declaration accepted, and
  every declared feed arriving (docs/carplay/05_METADATA_AND_CONTROLS.md §6.6). Console transcription, not a byte copy — see its header.

## 2026-07-10 — runtime snapshots + the USB-reset investigation

- `2026-07-10_p0_runtime_snapshot.md` — point-in-time CCPA + host-app runtime after deploying/validating
  P0 of the lifecycle-hardening plan (docs/carplay/02_SESSION_LIFECYCLE.md), captured over UART during a live streaming session.
  Box wall-clock is unreliable (no RTC), so uptime is the reference clock.
- `2026-07-10_usb_reset_investigation.md` — can a wedged iPhone be recovered SHORT of a box reboot?
  Every controller-level reset and programmatic VBUS switching were exercised. **Bottom line: no
  box-side reset re-enumerates a wedged iPhone; only a full box reboot does.**
- `2026-07-10_yaml_2400x960_snapshot.md` — the host-authoritative VehicleConfig YAML (docs/carplay/04_CAPABILITIES_AND_CONFIG.md / task #5)
  live at **2400×960** after a clean boot: full-frame video, decrypt `fail=0`, zero A/V drops.

## 2026-07-25 → 2026-08-10 — wireless tunnel evidence

- `2026-07-25_wireless_inbound_channel_evidence.md` — inbound tunnel traffic arrives on the **CONTROL**
  connection. Archived because docs/carplay/03_SDK_GROUND_TRUTH.md §1's central claim rests on it and it previously existed only in
  `/tmp/airplayd_wl.log` (tmpfs, lost on reboot).
- `2026-07-29_iphone_0x4171_listupdate.txt` — live `0x4171 ListUpdate` frames: wire evidence for the flat
  repeated-group encoding. Note the 48-byte hex-dump cap called out in its header.
- `2026-08-10_REGRESSION_datastream130_scid_rejected.txt` — the box REFUSING iOS's RCS iAP-channel SETUP
  (stream type 130) during a live wireless session on iOS 27; grep excerpt with source line numbers kept.
- `2026-08-10_TUNNEL_IDENT_REJECT_tier_all_voiceover_cursor.txt` — device evidence that metadata tier
  `all` is REJECTED on the AirPlayTunnel Identify, with the phone naming the exact message ids it
  refuses (the datum behind the docs/carplay/05_METADATA_AND_CONTROLS.md tier policy).
