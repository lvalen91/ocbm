# GM CCPA — Direct-WiFi CarPlay on a 2024 Silverado (GM Info 3.7 / Y181) — Findings

**Question:** Can a non-privileged, sideloaded Android app on the GM head unit act as a self-contained
wireless-CarPlay receiver — the iPhone streaming CarPlay to the app directly over the vehicle Wi-Fi
hotspot, with a Carlinkit CPC200-CCPA adapter (USB/OCBM) doing only Bluetooth + MFi?

**Answer: Yes, and it is device-proven end to end.** The feasibility case below (2026-07-31) proved
every access boundary individually. Full A/V — pairing, HEVC video, AAC-LC audio, alongside GM's own
unmodified CarPlay service — is a working, repeatable session as of 2026-08-12; the current session
mechanics, live failure points and log signatures are the *living* document,
[`12_OBSERVED_FLOW.md`](12_OBSERVED_FLOW.md). This document is the static findings record: what was
proven, the access boundaries, the codec inventory, and the two dated investigations (session-management
rejection, wireless video framing) that were fully resolved and are kept here as settled protocol facts.

Target: `gm/full_gminfo37_gb/gminfo37:12/W231E-Y181.3.2-SIHM22B-499.3/231:user/release-keys`,
Android 12 / API 32, SELinux Enforcing, verified-boot green, `ro.debuggable=0`, GAS build (Play Store +
GMS present). Installs as package `zeno.gmccpa`; source classes remain `zeno.gmccpa.*`
(`netprobe_app/app/build.gradle:16,20`).

---

## 1. Scorecard — feasibility probe (device-proven, captures 2026-07-31)

| Question | Verdict | Evidence |
|---|---|---|
| Install as ordinary app, Play-attributed | ✅ | uid 1010124, `installerPackageName=com.android.vending` |
| In-motion display eligible | ✅ | `distractionOptimized=true` + `canDrawOverlays=true`; CarUxRestrictions readable |
| Hotspot band | ✅ 5 GHz | `SoftAp mBand:2` (BAND_5GHZ), SSID `myChevrolet 32D4`, br0 |
| AP client isolation | ✅ none | app completed TCP to the iPhone; app also runs *on* the AP host |
| Multicast / mDNS both ways | ✅ | iPhone `Owner-iPhone` discovered on br0; app's own `_airplay._tcp` REGISTERED |
| Unicast reach to iPhone | ✅ | TCP 62078 + 49152 OPEN, ICMP true — both runs |
| App reads SoftAP SSID/pass | ❌ | `SecurityException` (getSoftApConfiguration) + `EACCES` (hostapd.conf) |
| App reaches MFi / i2c | ❌ | `/dev/i2c-{0,1}` → `EACCES` (untrusted_app SELinux domain) |
| No OS blocker on the AirPlay path | ✅ | iPhone: discover → connect → `GET /info` → `POST /pair-setup` M1, against an ordinary app UID |

The two ❌ rows are design inputs, not blockers: the passphrase is entered by the user from the OS
hotspot GUI (§4), and the MFi coprocessor stays behind the CCPA, reached over USB/OCBM (§5). Both are
current architecture, not workarounds — see `12_OBSERVED_FLOW.md` Phase 0–3.

### AirPlay receiver probe (evidence 04) — the decisive protocol test

A diagnostic `_airplay._tcp` receiver (`AirPlayRx.kt`, port 7010, no pairing implemented) was advertised;
the iPhone (`AirPlay/980.71.1`) discovered it, connected, ran `GET /info`, and sent a real 32-byte
`pair-setup` M1 body — against an ordinary app UID, with no SELinux/permission/network interference
anywhere on the path. That session was Screen Mirroring, not CarPlay (the probe advertised
`model=AppleTV3,2` without the Car feature bit), so the CarPlay-specific discovery path was not yet
exercised here — but transport, HTTP and RTSP framing are identical either way, and both have since been
confirmed live (`12_OBSERVED_FLOW.md` Phase 4–6).

What this probe did **not** establish, since it matters for read order: `pair-setup` and `pair-verify`
are chipless SRP-6a / Curve25519 — the MFi coprocessor is not touched until `auth-setup`, two steps
later. The probe stopped at "no pairing implemented," not at an MFi wall.

Full evidence index in §7.

---

## 2. Network topology

The head unit **hosts** the hotspot; it is not a Wi-Fi station. That fact explains several "blank"
readings and shapes the whole design.

```
              iPhone  ── Wi-Fi 5GHz ──►  br0 (SoftAP "myChevrolet 32D4", 192.168.5.1/24, HEAD UNIT)
        192.168.5.206                     │  the receiver APP runs HERE, on the AP host
   fe80::c5f:9f98:aee2:3a4f%br0           │  → host↔client, so ap_isolate never applies
                                          │
   default route = CELLULAR (net 100/101, iface vlan5, validated=false)  ← OnStar/telematics
   internal VLANs: vlan5/eth0 192.168.1.0/24 (radio) · vlan4 172.16.4.0/24 (EOCM/telematics)
```

- `WifiManager.connectionInfo` is blank (SSID `<unknown>`, ip 0.0.0.0, freq -1) because the unit is the
  AP, not a client. Band comes from `dumpsys wifi` SoftAp (`mBand:2`), not the station API.
- The default network is cellular, so *internet* sockets egress cellular — but the iPhone sits on br0's
  directly-connected route, reachable without binding (proven).
- The app runs on the AP host, so client isolation is moot: isolation blocks client↔client, never
  host↔client.

---

## 3. Port :7000 and GM's own CarPlay receiver

- `:7000` (AirPlay/RTSP) is held by `com.gm.domain.server.delayed:CarplayService` — a Java system
  service (uid.system), not the `com.gm.hmi.applecarplay` HMI APK and not native Cinemo. Disabling the
  CarPlay HMI apps does not free it (proven: disabled, `:7000` still listened).
- **Do not disable `com.gm.domain.server.delayed`.** It also holds REBOOT / SECURE_SETTINGS / OTA and
  manages GM's Bluetooth — losing it costs ADB access and the install/log path. It is not a reason to
  keep GM's stack in the data path; the CCPA uses its own BT radio and GM's stack is bypassed entirely.
- A receiver does not need `:7000` freed — it advertises its own RTSP port in its own Bonjour SRV
  record and the iPhone dials whatever is advertised.
- GM's `CarplayService` **does** advertise `CarPlay._airplay._tcp` on br0, so it is a competing
  `_airplay` surface on the hotspot. This used to look like a blocker (both records target the platform
  hostname `Android.local`, and iOS folds them into one endpoint keyed by host); it is now a **solved**
  coexistence problem — see `12_OBSERVED_FLOW.md` Phase 5 for the mechanism and the fix (a self-hosted
  mDNS advert on a hostname the app owns). GM's service stays running and untouched.

---

## 4. Access boundaries (settled with device proof)

- **SoftAP passphrase is unreadable by app *and* shell.** App: `getSoftApConfiguration` /
  `getWifiApConfiguration` → `SecurityException`; `hostapd.conf` → `EACCES`. Shell: `hostapd.conf`
  denied, no `cmd wifi` read. The app's own hosted AP does not even appear in its own
  `getScanResults`. ⇒ user types the passphrase in.
- **MFi coprocessor is unreachable by the app.** `/dev/i2c-0` (`saturnhd_device`) and `/dev/i2c-1`
  (`i2c_device`) are world `crw-rw-rw-` at DAC, but SELinux (Enforcing) denies the app's `untrusted_app`
  domain → `EACCES`. adb cannot relabel an app's SELinux domain without root. ⇒ MFi stays on the CCPA,
  reached over USB/OCBM (endpoints IN `0x81` / OUT `0x01`, hardware-confirmed and interface-discovered
  — `native/carplay-jni`, `netprobe_app/.../ocbm/UsbBulkTransport.kt:19`).
- **In-motion display** requires Play-attributed install (`adb install -i com.android.vending`) *and*
  `distractionOptimized` on the activity. The shell override (`cmd car_service`) is blocked on this
  `user` build (`SecurityException: requires non-user build`), so this is the only path. Both
  preconditions are confirmed; the in-motion behaviour itself (as opposed to eligibility) has not been
  re-tested since — every capture to date was taken parked (`requiresDistractionOptimization=false`).

---

## 5. Media codecs (Intel HD 505 Gen9 — raw dump in `evidence/05_codecs.txt`)

**Video — all hardware-decoded, max 3840×2160 @ up to 40 Mbps, each with a `.secure` DRM variant:**

| Codec | HW decode | HW encode | Note |
|---|---|---|---|
| H.264 / AVC | ✅ `OMX.Intel.hw_vd.h264` | ✅ `hw_ve.h264` | CarPlay baseline video |
| HEVC / H.265 | ✅ `OMX.Intel.hw_vd.h265` | ✅ `hw_ve.h265` | halves Wi-Fi bitrate — this is the codec actually negotiated (see §6) |
| VP8 / VP9 | ✅ | VP9 sw only | |
| VC-1 / WMV | ✅ | — | |
| AV1 | ❌ software only (`c2.android.av1`) | — | not used by CarPlay |

`blocks-per-second` 972000 ⇒ 1080p60 with headroom, 4K30. Render target is the panel's 2400×960@60, so
the decoders are never the bottleneck.

**Audio:** AAC-LC and AAC-ELD (profile 39) decode *and* encode — CarPlay's audio codec and the
mic-uplink codec, both native. No ALAC, no Dolby decoder of any kind. Neither matters: CarPlay's audio
ceiling is stereo AAC-LC 48 kHz, a wire-format limit set by Apple's `kAirPlayAudioFormat_*` bitmask
(flat codec × rate × channels, no entry above 2ch, no ALAC/AC-3/E-AC-3/object-based). Across all five
CarPlay WWDC sessions (2016 ×2, 2017, 2019, 2023) there are zero mentions of Atmos, spatial, surround,
multichannel, lossless or bit depth; the codec story is three sentences in `wwdc2016-722.txt:80-82`.

Two levers exist that are about delivery, not fidelity: stream type 102 for high-latency media (keeps
Apple Music off the low-latency voice path), and `mainBuffered` — a head-unit-side 2-minute buffer fed
faster than real time, which survives a Wi-Fi glitch (`wwdc2023-10150.txt:136-142`). For media crossing
5 GHz to a moving vehicle, `mainBuffered` is the more valuable of the two, and is what
`12_OBSERVED_FLOW.md` Phase 6 shows negotiated live.

Full enum, WWDC quotes and Simulator symbol evidence: `ccpa_custom/docs/carplay/06_AV_PIPELINE.md`;
stream types in `05_SESSION_FLOW.md` §6, §8 rule 10.

**Build implication (realized):** `MediaCodec` + the Intel HW decoder feeding a `Surface`. HEVC is what
is actually negotiated with the iPhone in every proven session (`12_OBSERVED_FLOW.md` Phase 7:
`OMX.Intel.hw_vd.h265`, `csd-0` = VPS+SPS+PPS). See §6 for the framing detail that made this work.

---

## 6. Two resolved protocol investigations (kept as settled facts)

Both of these were multi-hour investigations against a session that otherwise looked healthy. Both are
fixed and confirmed working live since 2026-08-12 (`12_OBSERVED_FLOW.md` banner at top). Kept here,
compressed, because the root causes are non-obvious protocol facts a future session could rediscover the
hard way.

### 6a. `sessionManagement` — SETUP rejected with `-16720 kFigEndpointError_InvalidParameter`

Sessions completed pairing, `/auth-setup`, SETUP and RECORD, then iOS tore the connection down ~10 ms
later. Root cause: `sessionManagementInfo` is one of six keys Apple's per-feature `/info` validator
checks for presence (`carEndpoint_validateInfoResponseKeyPresentForFeature` —
`sessionManagementInfo`, `mainBufferedInfo`, `fileTransferInfo`, `vehicleStateProtocolInfo`,
`logTransferInfo`, `uiSyncInfo`; `ccpa_custom/docs/carplay/05_METADATA_AND_CONTROLS.md (was docs/35:157-162`)) and it was absent from this project's
static `/info` while the SETUP response still echoed `"sessionManagement"` in `features` — a
desync between the two. HEVC was ruled out separately: it gates on three independent conditions
(non-null `hevcInfo`, `hevc` in SETUP `features`, iOS streaming `hvc1`), and the first two were already
satisfied.

**Fix, confirmed on hardware:** `CARPLAY_SESSION_MGMT=1` set unconditionally in `JNI_OnLoad`
(`native/carplay-jni/src/lib.rs:366`), with `/info` regenerated under that same env — a static asset
built under a different env than the running server is exactly the hazard this bug was.

**Structural gaps still open against `airplayd`** (tracked in `11_HARDENING_PLAN.md`, not blocking):
`arm_keepalive` TCP 3/3/3 dead-link detect (Java sockets can't express it), `start_input_listener()`
HID on `127.0.0.1:9110` (not started — the app advertises `hidDevices` but does not implement it), and
per-connection `build_info(&load_device_config())` (this project ships a static `/info` asset instead).

### 6b. Wireless VideoConfig is a QuickTime sample-description box, not a bare `avcC`/`hvcC` record

On the wired path the opcode-1 VideoConfig frame is a bare configuration record
(`configurationVersion == 1` as the first byte). On WIRELESS it is wrapped in a sample-description box
(`size, 'hvc1'/'avc1', reserved..., <nested hvcC/avcC atom>`). Parsing it as a bare record extracts no
parameter sets, decodes to 0 bytes, and produces a healthy-looking session with a permanently black
screen. Fix: `unwrap_sample_description()` in `session.rs` scans for the nested atom; bare records
(`body[0] == 1`) pass through untouched, so the wired path is unaffected.

This bug is latent in `ccpa_custom`'s own reference implementation too — its proven wireless capture
runs `OCBM_FWD_ENC` and forwards encrypted frames to the host without ever parsing the config, so that
branch is never exercised there. This project is the first thing to decrypt on-box over wireless and
therefore the first to hit it.

**Codec confirmed by decoded NAL stream, not by negotiation:** HEVC (`hvc1`) — VPS (type 32), SPS
(33), PPS (34), then IDR (`IDR_N_LP`, type 20). Per `ccpa_custom/docs/carplay/03_SDK_GROUND_TRUTH.md` §5 the codec is never declared
in the SETUP dict; it rides in-band as the FourCC. For `MediaCodec`: MIME `video/hevc`, `csd-0` =
VPS+SPS+PPS concatenated (not the separate SPS/PPS pair H.264 uses) — confirmed live in
`12_OBSERVED_FLOW.md` Phase 7.

---

## 7. Target architecture (validated design)

> Canonical version: [`04_SYSTEM_MODEL.md`](04_SYSTEM_MODEL.md). This section is the evidence-side
> summary; phase-by-phase session flow lives in [`12_OBSERVED_FLOW.md`](12_OBSERVED_FLOW.md).

The CCPA is reduced to the Bluetooth radio + the MFi coprocessor, reached over USB/OCBM. It raises no
access point and carries no media; the iPhone is never wired to it.

```
 iPhone ──BT──► CCPA (own BT, NOT vehicle BT) ──USB/OCBM──► head-unit APP
   │            └ MFi coprocessor (cert + signature, relayed over OCBM every session)
   │
   └─ told to join "myChevrolet" via 0x5703 handoff (app-supplied SSID+pass) ─┐
                                                                              ▼
 iPhone (192.168.5.206) ──── 5GHz Wi-Fi, br0 ────► APP is the AirPlay endpoint (192.168.5.1)
        advertises _airplay._tcp ◄─ iPhone connects in ─► RTSP + pair-verify + H.264/HEVC stream
```

All five build tasks this section used to enumerate as future work are implemented and confirmed live:
app-held AirPlay identity + software `pair-verify`, MFi relay over OCBM `CH_MFI` (chip key never leaves
the CCPA, re-run every session — not first-pairing only), parameterized `0x5703` handoff carrying the
vehicle's own hotspot creds, BT↔Wi-Fi identity consistency, and the two independent planes post-handoff
(BT link as session anchor + OCBM for MFi relay, media on br0 directly — bypassing the ~90 Mbps USB
write cap). See `12_OBSERVED_FLOW.md` for the current phase-by-phase mechanics and open failure points.

---

## 8. What to protect from the debloat (`reference/debloat/gm_debloat_full.sh`)

Two of its actions conflict with this plan (verified against the script):

- **Disables the SoftAP** (`gm_debloat_full.sh:352-354,482-484`: `wifi_ap_enabled 0`,
  `fid_hotspot_disable_status 1`) — the very `myChevrolet` hotspot the plan needs. Skip it / re-enable.
- **Disables `com.android.vending`** (`:292,542`) — the in-motion path relies on the Play-attributed
  install; the attribution is a stored string so it likely survives, but re-test in-motion after a
  debloat.
- **Keep `com.gm.hmi.connection`** (`:247`, in the disable list) — it hosts `WifiHotspotActivity`, the
  only GUI to read the hotspot passphrase, which the app cannot read programmatically (§4).

---

## 9. Evidence index (`evidence/`)

| File | What it is |
|---|---|
| `01_radio_probe_shell.txt` | First shell-side recon (build, packages, i2c perms, VLAN map, UXR config) |
| `02_radio_deepprobe_shell.txt` | Deep shell recon (:7000 owner, SoftAP band+clients, ARP/iPhone, BT, audio buses, USB, SELinux labels) |
| `03_netprobe_app_bothruns.txt` | The app's two capability runs (scorecard §1 — reachability, SoftAP-creds block, MFi EACCES) |
| `04_airplay_rx_pairsetup.txt` | AirPlay receiver probe — reached `/pair-setup` (§1) |
| `05_codecs.txt` | Video/audio codec inventory + Intel HW limits |
| `session_2026-08-12-standby/` | Current regression oracle — full A/V, event-driven standby, RECORD→first-frame 0.82 s |

Hardware IDs seen (2026-07-31 capture): iPhone `Owner-iPhone` = `192.168.5.206` /
`fe80::c5f:9f98:aee2:3a4f%br0` / BT-ish MAC `4a:b1:2c:f2:7c:39`. Head unit br0 = `192.168.5.1` /
`fe80::f86d:ccff:fe1c:32d4%br0`. SoftAP `myChevrolet 32D4`, 5 GHz, US, max 8 clients.
