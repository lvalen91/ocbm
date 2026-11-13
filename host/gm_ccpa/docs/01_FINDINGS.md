# GM CCPA — Direct-WiFi CarPlay on a 2024 Silverado (GM Info 3.7 / Y181) — Findings

**Question:** Can a non-privileged, sideloaded Android app on the GM head unit act as a self-contained
wireless-CarPlay receiver — the iPhone streaming CarPlay to the app directly over the vehicle Wi-Fi
hotspot, with a Carlinkit CPC200-CCPA adapter (USB/OCBM) doing only Bluetooth + MFi?

**Answer: Yes, device-proven end to end.** The feasibility probe (2026-07-31) proved every access boundary
individually. Full A/V (pairing, HEVC video, AAC-LC audio, alongside GM's own unmodified CarPlay service) is
a working, repeatable session as of 2026-08-12. Current session mechanics, failure points and log
signatures are in the living document [`12_OBSERVED_FLOW.md`](12_OBSERVED_FLOW.md). This document is the
static findings record: the access boundaries, the codec inventory, two resolved protocol investigations and
the debloat keep list.

Target: `gm/full_gminfo37_gb/gminfo37:12/W231E-Y181.3.2-SIHM22B-499.3/231:user/release-keys`,
Android 12 / API 32, SELinux Enforcing, verified-boot green, `ro.debuggable=0`, GAS build (Play Store +
GMS present at the factory; the debloat script uninstalls both per-user, see §8). Installs as package
`android.car.usb.handler` (owner request); source classes are `zeno.gmccpa.*`
(`app/build.gradle:20,26`, `tools/build_apk.sh` `INSTALL_PKG`).

---

## 1. Scorecard — feasibility probe (device-proven, captures 2026-07-31)

| Question | Verdict | Evidence |
|---|---|---|
| Install as ordinary app, Play-attributed | yes | uid 1010124, `installerPackageName=com.android.vending` |
| In-motion display eligible | yes | `distractionOptimized=true` + `canDrawOverlays=true`; CarUxRestrictions readable |
| Hotspot band | yes, 5 GHz | `SoftAp mBand:2` (BAND_5GHZ), SSID `<vehicle SSID>`, br0 |
| AP client isolation | yes, none | app completed TCP to the iPhone; app also runs *on* the AP host |
| Multicast / mDNS both ways | yes | iPhone `Owner-iPhone` discovered on br0; app's own `_airplay._tcp` REGISTERED |
| Unicast reach to iPhone | yes | TCP 62078 + 49152 OPEN, ICMP true — both runs |
| App reads SoftAP SSID/pass | no | `SecurityException` (getSoftApConfiguration) + `EACCES` (hostapd.conf) |
| App reaches MFi / i2c | no | `/dev/i2c-{0,1}` → `EACCES` (untrusted_app SELinux domain) |
| No OS blocker on the AirPlay path | yes | iPhone: discover → connect → `GET /info` → `POST /pair-setup` M1, against an ordinary app UID |

The two "no" rows are design inputs, not blockers: the passphrase is entered by the user from the OS hotspot GUI (§4), and the MFi
coprocessor stays behind the CCPA, reached over USB/OCBM (§4). Both are current architecture
(`12_OBSERVED_FLOW.md` Phases 0–3).

### AirPlay receiver probe (evidence 04)

A diagnostic `_airplay._tcp` receiver (`AirPlayRx.kt`, port 7010, no pairing) was advertised; the iPhone
(`AirPlay/980.71.1`) discovered it, connected, ran `GET /info`, and sent a real 32-byte `pair-setup` M1 —
against an ordinary app UID, with no SELinux, permission or network interference on the path. The session was
Screen Mirroring, not CarPlay (the probe advertised `model=AppleTV3,2` without the Car feature bit), so
CarPlay-specific discovery was not exercised, but transport, HTTP and RTSP framing are identical; both have
since been confirmed live (`12_OBSERVED_FLOW.md` Phases 4–6). `pair-setup` and `pair-verify` are chipless
SRP-6a / Curve25519; the MFi coprocessor is first touched at `auth-setup`, two steps later, so the probe
stopped at "no pairing implemented", not at an MFi wall.

`AirPlayRx.kt` was deleted (`11_HARDENING_PLAN.md` T6.1). Its successor `CarPlayRx.kt` listens on **7011**
(the `port` constructor default). `CarPlayRx` implements no pairing itself: pair-setup, pair-verify and
`/auth-setup` all run in the JNI'd Rust core via `CarPlayRx.handleNative` (API 32 exposes no X25519/Ed25519).
With the native core unavailable the connection is closed (`native core unavailable — closing; there is no
fallback`, `CarPlayRx.handle`).

Evidence index: §9.

---

## 2. Network topology

The head unit **hosts** the hotspot; it is not a Wi-Fi station. That explains several "blank" readings and
shapes the whole design.

```
              iPhone  ── Wi-Fi 5GHz ──►  br0 (SoftAP "<vehicle SSID>", 192.168.5.1/24, HEAD UNIT)
        <IPHONE_IP>                     │  the receiver APP runs HERE, on the AP host
   <IPHONE_LL6>%br0           │  → host↔client, so ap_isolate never applies
                                          │
   default route = CELLULAR (net 100/101, iface vlan5, validated=false)  ← OnStar/telematics
   internal VLANs: vlan5/eth0 192.168.1.0/24 (radio) · vlan4 172.16.4.0/24 (EOCM/telematics)
```

- `WifiManager.connectionInfo` is blank (SSID `<unknown>`, ip 0.0.0.0, freq -1) because the unit is the AP,
  not a client. Band comes from `dumpsys wifi` SoftAp (`mBand:2`), not the station API.
- The default network is cellular, so internet sockets egress cellular; the iPhone sits on br0's
  directly-connected route, reachable without binding (proven).
- Client isolation is moot: it blocks client↔client, never host↔client.

> **Methodology trap — `/system/etc/iptables.rules` alone (2026-09-10).** That file is IPv4 only. Its `INPUT`
> chain is default-`DROP` with no unicast allow for `:7011` or any other app port on `br0`, so read alone it
> suggests the control session can never reach the app. The phone's control connection arrives over its
> **IPv6 link-local** address (`fe80::…%br0`; `CarPlayRx.kt`: "The phone is ALWAYS IPv6 link-local on this
> rig"), governed by the separate `/system/etc/ip6tables.rules`. Its `INPUT` chain has one blanket rule,
> `-A INPUT -i br0 -s fe80::/64 -j ACCEPT`, which passes any link-local-sourced packet on `br0` regardless of
> port. Confirmed by a 2026-09-09 session capture (`>>> INBOUND CONTROL CONNECTION from fe80::…%br0:50341`,
> then pair-verify → `/auth-setup` → encrypted control channel). Check both rule files before concluding a
> port is unreachable. `netd` also logs `OemIptablesHook: OEM iptable hook installed` at boot, a GM-native
> hook beyond the two static files, so even both together are not guaranteed to be the full runtime picture.

---

## 3. Port :7000 and GM's own CarPlay receiver

- `:7000` (AirPlay/RTSP) is held by `com.gm.domain.server.delayed:CarplayService` — a Java system service
  (uid.system), not the `com.gm.hmi.applecarplay` HMI APK and not native Cinemo. Disabling the CarPlay HMI
  apps does not free it (proven: disabled, `:7000` still listened).
- **Do not disable `com.gm.domain.server.delayed`.** It also holds REBOOT / SECURE_SETTINGS / OTA and manages
  GM's Bluetooth; losing it costs ADB access and the install/log path. It is not in our data path: the CCPA
  uses its own BT radio and GM's stack is bypassed.
- A receiver does not need `:7000` freed: it advertises its own RTSP port in its own Bonjour SRV record and
  the iPhone dials whatever is advertised.
- GM's `CarplayService` advertises `CarPlay._airplay._tcp` on br0, a competing `_airplay` surface. Both
  records target the platform hostname `Android.local` and iOS folds them into one endpoint keyed by host.
  Coexistence is solved by a self-hosted mDNS advert on a hostname the app owns (`12_OBSERVED_FLOW.md`
  Phase 5). GM's service stays running and untouched.
- Re-confirmed end to end 2026-09-09: with every other GM CarPlay/projection component disabled or
  uninstalled (of all `com.gm.hmi.*` only `com.gm.hmi.settings` survives on user 0), OE GM CarPlay still
  latched to its port and `com.gm.domain.server.delayed` present and untouched, a full A/V session ran on our
  own radio software and ports (pair-verify → `/auth-setup` → SETUP 130/110/102 → HEVC first frame).

> **Methodology trap — `packages.txt` from the 2026-09-09 probe bundle.** That bundle's recon ran
> `adb shell pm list packages -f` with no `--user`, so it enumerated **user 0 only**. This app installs to
> user 10 and is absent from its own probe's list. Do not conclude "package X was removed" from that file: on
> the 2026-09-09 bundle `com.android.vending`, `com.gm.hmi.connection`, `com.gm.updater` and others look
> uninstalled when they are only absent from user 0. Use `pm list packages --user 10` and `--user 0`
> explicitly, or read the per-`User N: installed=` lines of `dumpsys package <pkg>` (as `pkg_detail.txt`
> does). `tools/adb_radio_probe.sh` and `tools/deepprobe.sh` now enumerate users 0 and 10 and label each
> line `[user <u>]` (`USERS="${USERS:-0 10}"`); the bundle was not produced by either script.

---

## 4. Access boundaries (settled with device proof)

- **SoftAP passphrase is unreadable by app and shell.** App: `getSoftApConfiguration` /
  `getWifiApConfiguration` → `SecurityException`; `hostapd.conf` → `EACCES`. Shell: `hostapd.conf` denied, no
  `cmd wifi` read. The app's own hosted AP does not appear in its own `getScanResults`. ⇒ the user types the
  passphrase in.
- **MFi coprocessor is unreachable by the app.** `/dev/i2c-0` (`saturnhd_device`) and `/dev/i2c-1`
  (`i2c_device`) are world `crw-rw-rw-` at DAC, but SELinux (Enforcing) denies the app's `untrusted_app`
  domain → `EACCES`; adb cannot relabel an app's domain without root. ⇒ MFi stays on the CCPA, reached over
  USB/OCBM (endpoints IN `0x81` / OUT `0x01`, hardware-confirmed and interface-discovered; `native/carplay-jni`,
  class KDoc of `UsbBulkTransport` in `app/.../ocbm/UsbBulkTransport.kt`).
- **In-motion display** requires a Play-attributed install (`adb install -i com.android.vending`) and
  `distractionOptimized` on the activity. The shell override (`cmd car_service`) is blocked on this `user`
  build (`SecurityException: requires non-user build`), so this is the only path. Both preconditions were
  confirmed; in-motion behaviour itself has not been re-tested (every capture was taken parked,
  `requiresDistractionOptimization=false`). The debloat script uninstalls `com.android.vending` on users
  0 and 10 (§8), so whether the attribution precondition still holds is UNTESTED.

---

## 5. Media codecs (Intel HD 505 Gen9 — raw dump in `evidence/05_codecs.txt`, archived; see §9)

**Video — all hardware-decoded, max 3840×2160 @ up to 40 Mbps, each with a `.secure` DRM variant:**

| Codec | HW decode | HW encode | Note |
|---|---|---|---|
| H.264 / AVC | yes, `OMX.Intel.hw_vd.h264` | yes, `hw_ve.h264` | CarPlay baseline video |
| HEVC / H.265 | yes, `OMX.Intel.hw_vd.h265` | yes, `hw_ve.h265` | halves Wi-Fi bitrate — the codec actually negotiated (§6) |
| VP8 / VP9 | yes | VP9 sw only | |
| VC-1 / WMV | yes | — | |
| AV1 | no, software only (`c2.android.av1`) | — | not used by CarPlay |

`blocks-per-second` 972000 ⇒ 1080p60 with headroom, 4K30. The render target is the panel's 2400×960@60, so
the decoders are never the bottleneck.

**Audio — there is NO hardware audio codec on this unit, for any format.** A full `dumpsys media.player`
enumeration (`gmccpa_probe_20260909_225421/codecs.txt`) lists every audio decoder and encoder as the AOSP
software set (`c2.android.{aac,mp3,flac,opus,vorbis,amrnb,amrwb,g711,raw}.decoder` plus `OMX.google.*` twins).
The only vendor codec XMLs are `media_codecs_google_audio.xml` and `media_codecs_google_video.xml`; the
hardware entries in `media_codecs.xml` are video only (`OMX.Intel.hw_vd.h264/h265/vp8/vp9`). In a live
session both AAC-LC media and AAC-ELD Siri decoded on `c2.android.aac.decoder`. Consequences:

- **No compressed-audio offload.** The only output flags in `/vendor/etc/audio_policy_configuration.xml` are
  `AUDIO_OUTPUT_FLAG_PRIMARY` and `AUDIO_OUTPUT_FLAG_DIRECT|AUDIO_OUTPUT_FLAG_VOIP_RX`; there is no
  `COMPRESS_OFFLOAD` mixPort. Every CarPlay sample is CPU-decoded, and the 16 kHz mono voice streams are
  CPU-resampled to the 48 kHz stereo the buses declare ([`13_AUDIO_ROUTING.md`](13_AUDIO_ROUTING.md) §1).
- The hardware that does exist is the **Harman DSP behind the audio HAL, strictly post-PCM**: bus summing, HAL
  ducking (`Use hal ducking signals true`), EQ. It never sees a codec frame.
- The mic uplink's AAC-ELD **encoder** cannot come from the platform (`c2.android.aac.encoder` has no ELD
  profile), so the JNI `.so` cross-builds and statically links libfdk-aac (`13_AUDIO_ROUTING.md` §4); this is
  a requirement, not an optimisation.

There is **no ALAC and no Dolby decoder of any kind**; neither matters. CarPlay's audio ceiling is stereo
AAC-LC 48 kHz, a wire-format limit set by Apple's `kAirPlayAudioFormat_*` bitmask (flat codec × rate ×
channels; no entry above 2ch, no ALAC/AC-3/E-AC-3/object-based). Across all five CarPlay WWDC sessions (2016
×2, 2017, 2019, 2023) there is no mention of Atmos, spatial, surround, multichannel, lossless or bit depth;
the codec story is three sentences in `wwdc2016-722.txt:80-82`.

Two levers concern delivery, not fidelity: stream type 102 for high-latency media (keeps Apple Music off the
low-latency voice path), and `mainBuffered`, a head-unit-side 2-minute buffer fed faster than real time that
survives a Wi-Fi glitch (`wwdc2023-10150.txt:136-142`). For media over 5 GHz to a moving vehicle
`mainBuffered` is the more valuable, and it is negotiated live (`12_OBSERVED_FLOW.md` Phase 6).

Full enum, WWDC quotes and Simulator symbol evidence: `ocbm/docs/carplay/06_AV_PIPELINE.md`; stream types in
`05_SESSION_FLOW.md` §6, §8 rule 10.

**Build implication (realized):** `MediaCodec` + the Intel HW decoder feeding a `Surface`. HEVC is negotiated
in every proven session (`12_OBSERVED_FLOW.md` Phase 7: `OMX.Intel.hw_vd.h265`, `csd-0` = VPS+SPS+PPS);
framing detail in §6b.

---

## 6. Two resolved protocol investigations (settled facts)

Both were multi-hour investigations against a session that otherwise looked healthy; both are fixed and
confirmed live since 2026-08-12. Kept because the root causes are non-obvious protocol facts.

### 6a. `sessionManagement` — SETUP rejected with `-16720 kFigEndpointError_InvalidParameter`

Sessions completed pairing, `/auth-setup`, SETUP and RECORD, then iOS tore the connection down ~10 ms later.
Root cause: `sessionManagementInfo` is a key Apple's per-feature `/info` validator checks for presence
(`carEndpoint_validateInfoResponseKeyPresentForFeature`; the key is listed with `hevcInfo`, `logTransferInfo`
and `mainBufferedInfo` in `ocbm/docs/carplay/03_SDK_GROUND_TRUTH.md`, and the validator cluster is described in
`ocbm/docs/carplay/02_SESSION_LIFECYCLE.md`). It was absent from this project's static
`/info` while the SETUP response still echoed `"sessionManagement"` in `features`: a desync between the two.
HEVC was ruled out separately: it gates on three independent conditions (non-null `hevcInfo`, `hevc` in SETUP `features`, iOS
streaming `hvc1`), and the first two were already satisfied.

**Fix, confirmed on hardware:** `CARPLAY_SESSION_MGMT=1` set unconditionally in `JNI_OnLoad` (the
`std::env::set_var("CARPLAY_SESSION_MGMT", "1")` call, `native/carplay-jni/src/lib.rs`), with `/info`
regenerated under that same env. A static asset built under a different env than the running server is
exactly the hazard this bug was.

**Structural gaps on the Kotlin/Android path** (reference `carplayd` implements all of them): HID input
listener on `127.0.0.1:9110` and per-connection `build_info(&load_device_config())` are not implemented here
(the app advertises `hidDevices` without implementing it and ships a static `/info` asset). Tracked as
N14 and N15 in [`11_HARDENING_PLAN.md`](11_HARDENING_PLAN.md), which owns their status.

### 6b. Wireless VideoConfig is a QuickTime sample-description box, not a bare `avcC`/`hvcC` record

On the wired path the opcode-1 VideoConfig frame is a bare configuration record (`configurationVersion == 1`
as the first byte). On WIRELESS it is wrapped in a sample-description box (`size, 'hvc1'/'avc1', reserved...,
<nested hvcC/avcC atom>`). Parsing it as a bare record extracts no parameter sets, decodes to 0 bytes and
produces a healthy-looking session with a permanently black screen. Fix: `unwrap_sample_description()` scans
for the nested atom; bare records (`body[0] == 1`) pass through, so the wired path is unaffected.

The code is not under `host/gm_ccpa`: it is in the shared `receiver` crate
(`crates/vendor/receiver/src/session.rs`, `fn unwrap_sample_description`), reached via the
`receiver = { path = "../../../../crates/vendor/receiver", … }` dependency in `native/carplay-jni/Cargo.toml`.

The bug is latent in `ocbm`'s own reference implementation too: its proven wireless capture runs
`OCBM_FWD_ENC` and forwards encrypted frames without parsing the config, so that branch is never exercised
there. This project is the first to decrypt on-box over wireless and so the first to hit it.

**Codec confirmed by decoded NAL stream, not by negotiation:** HEVC (`hvc1`) — VPS (type 32), SPS (33), PPS
(34), then IDR (`IDR_N_LP`, type 20). Per `ocbm/docs/carplay/03_SDK_GROUND_TRUTH.md` §5 the codec is never
declared in the SETUP dict; it rides in-band as the FourCC. For `MediaCodec`: MIME `video/hevc`, `csd-0` =
VPS+SPS+PPS concatenated (not the SPS/PPS pair H.264 uses); live-confirmed in `12_OBSERVED_FLOW.md` Phase 7.

---

## 7. Target architecture (validated design)

> Canonical version: [`04_SYSTEM_MODEL.md`](04_SYSTEM_MODEL.md); phase-by-phase flow:
> [`12_OBSERVED_FLOW.md`](12_OBSERVED_FLOW.md).

The CCPA is reduced to the Bluetooth radio + the MFi coprocessor, reached over USB/OCBM. It raises no access
point and carries no media; the iPhone is never wired to it.

```
 iPhone ──BT──► CCPA (own BT, NOT vehicle BT) ──USB/OCBM──► head-unit APP
   │            └ MFi coprocessor (cert + signature, relayed over OCBM every session)
   │
   └─ told to join "<vehicle SSID>" via 0x5703 handoff (app-supplied SSID+pass) ─┐
                                                                              ▼
 iPhone (<IPHONE_IP>) ──── 5GHz Wi-Fi, br0 ────► APP is the AirPlay endpoint (192.168.5.1)
        advertises _airplay._tcp ◄─ iPhone connects in ─► RTSP + pair-verify + HEVC stream
```

Implemented and confirmed live: app-held AirPlay identity + software `pair-verify`; MFi relay over OCBM
`CH_MFI` (the chip key never leaves the CCPA; re-run every session, not first-pairing only); parameterized
`0x5703` handoff carrying the vehicle's own hotspot credentials; BT↔Wi-Fi identity consistency; and two
independent planes after the handoff (BT link as session anchor + OCBM for the MFi relay, media on br0
directly, bypassing the ~90 Mbps USB write cap).

---

## 8. What to protect from the debloat (`reference/debloat/gm_debloat_full.sh`)

`reference/kept_packages.txt` is the authoritative keep list (12 entries):

| Group | Packages | Why kept |
|---|---|---|
| Hotspot UI | `com.gm.hmi.connection` | hosts `WifiHotspotActivity`, the only GUI that can read the hotspot passphrase (§4). `brand.chevrolet.app.connection` is NOT kept: it was removed and the hotspot GUI still worked. Recoverable with `pm install-existing --user <u> com.gm.hmi.connection`. |
| Tethering | `com.android.networkstack.tethering.inprocess`, `….inprocess.csm`, `….tethering.csm` | disabling them stops `hostapd` respawning at boot |
| GM services | `com.gm.vmsplugin`, `com.gm.updater`, `com.gm.domain.server.delayed` | `delayed` also holds REBOOT / SECURE_SETTINGS / OTA (§3) |
| HVAC | `com.gm.hmi.hvac`, `brand.chevrolet.app.hvac` | removing them breaks volume control |
| Projection | `com.google.android.embedded.projection` | Automotive Template Host; not present on this unit, dependency UNTESTED |
| Trailer | `com.gm.hmi.trailer`, `brand.chevrolet.app.trailer` | kept at owner request (2026-10-15); marked in `gm_debloat_full.sh` |

Five of the original thirteen entries (`com.android.vending`, `com.gm.hmi.connection`, the three tethering
packages) collided with the script as it stood on 2026-07-01 (byte-exact at
`git show b049810:host/gm_ccpa/reference/debloat/gm_debloat_full.sh`). That script removed the hotspot
UI package, disabled `com.android.vending`, disabled the three tethering packages, and wrote six persistent
`settings put global` values (`tether_supported 0`, `wifi_ap_enabled 0`, `fid_hotspot_disable_status 1`,
`share_hotspot_data_status 0`, `soft_ap_timeout_enabled 1`, `tethering_allow 0`), then killed `hostapd`: the very
`<vehicle SSID>` hotspot the design needs. The other eight
original entries (`com.google.android.gms`, `com.google.android.gsf`, `com.gm.vmsplugin`, `com.gm.updater`,
`com.gm.hmi.hvac`, `brand.chevrolet.app.hvac`, `com.google.android.embedded.projection`,
`com.gm.domain.server.delayed`) were already commented out of, or never in, the script; GMS and GSF have since
left the keep list.

**Current script behaviour.** The hotspot section and the Play Store force-stop are deleted outright, not
flag-gated: the script re-runs every boot, the settings persist, and one accidental run poisons the unit until
the settings are reverted by hand (six were written, five differ from stock; recipe below). A pre-flight guard (`KEEP_FILE` / `is_kept`, placed after the arrays so
`--status` is guarded too) reads `kept_packages.txt` and exits 1 if any keep-list package appears in
`BLOAT_PACKAGES` or `DISABLE_PACKAGES`; `remove_package`/`disable_package` refuse kept packages a second
time. To disable one deliberately, delete its line from `kept_packages.txt` first.

**Play Store / GMS / GSF are removed (owner request).** `com.android.vending`, `com.google.android.gms` and
`com.google.android.gsf` are in `BLOAT_PACKAGES` (last), so the script `pm uninstall --user`s them on users 0
and 10 (disable-user fallback if the uninstall does not stick) and `--status` counts them in the
removed-check. The in-motion installer-attribution dependency (§4) and the Automotive Template Host
dependency are UNTESTED after removal; apps installed with the installer flag no longer have a vending
installer on-device. Reverse per package: `pm install-existing --user <0|10> <package>`. The copy of the
script in the archived standalone `gm_ccpa` checkout is a different variant ("hotspot kept enabled; Play
Store/Services/Maps removed"), not this file's ancestor.

**Revert recipe for a unit that already had the old hotspot section applied.** Restore the five settings that
gate the hotspot and re-enable the three tethering packages for both users, then reboot (the tethering
packages are what start `hostapd` at boot):

```
adb shell settings put global wifi_ap_enabled 1
adb shell settings put global fid_hotspot_disable_status 0
adb shell settings put global tether_supported 1
adb shell settings put global tethering_allow 1
adb shell settings put global soft_ap_timeout_enabled 0
for u in 0 10; do
  for p in com.android.networkstack.tethering.inprocess \
           com.android.networkstack.tethering.inprocess.csm \
           com.android.networkstack.tethering.csm; do
    adb shell pm enable --user $u $p
  done
done
```

Stock values on a unit with a working hotspot (2026-09-09 capture bundle `gmccpa_probe_20260909_225421`, not in this tree; §9):
`tether_supported=1`, `tethering_allow=1`, `fid_hotspot_disable_status=0`, `wifi_ap_enabled` unset,
`soft_ap_timeout_enabled=0` (the script wrote 1), `share_hotspot_data_status=0` (same as the script wrote).

---

## 9. Evidence index (`evidence/`)

`evidence/` is not in this tree; it lives in the archived standalone `gm_ccpa` checkout. Citations to
`evidence/...` in `docs/` are provenance, not live paths (README, "Archive and rollback"). The same holds for the
2026-09-09 capture bundle `gmccpa_probe_20260909_225421` (`codecs.txt`, `settings.txt`, `logcat.log`) cited in
`01` and `12`: it is not in this tree.

| File | What it is |
|---|---|
| `01_radio_probe_shell.txt` | First shell-side recon (build, packages, i2c perms, VLAN map, UXR config) |
| `02_radio_deepprobe_shell.txt` | Deep shell recon (:7000 owner, SoftAP band+clients, ARP/iPhone, BT, audio buses, USB, SELinux labels) |
| `03_netprobe_app_bothruns.txt` | The app's two capability runs (scorecard §1: reachability, SoftAP-creds block, MFi EACCES) |
| `04_airplay_rx_pairsetup.txt` | AirPlay receiver probe, reached `/pair-setup` (§1) |
| `05_codecs.txt` | Video/audio codec inventory + Intel HW limits |
| `session_2026-08-12-standby/` | Regression oracle: full A/V, event-driven standby, RECORD→first-frame 0.82 s |

Hardware IDs seen (2026-07-31 capture): iPhone `Owner-iPhone` = `<IPHONE_IP>` / `<IPHONE_LL6>%br0` / BT-ish
MAC `<MAC-1>`. Head unit br0 = `192.168.5.1` / `fe80::<vehicle-derived>%br0`. SoftAP `<vehicle SSID>`,
5 GHz, US, max 8 clients.
