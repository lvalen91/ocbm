# Phone-side CarPlay behaviour

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 07; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

What the iPhone does that we cannot change, and what that implies for the accessory.

## Phone-side behaviour

<!-- absorbed: ../carplay/07_PHONE_SIDE.md -->

How the box becomes a wired-CarPlay accessory to the iPhone, and where CarPlay A/V goes.
This is the box's *phone-facing* half (ci_hdrc.0). Its *host-facing* half (OCBM) is docs 01–06.

### USB roles on the CCPA
- **ci_hdrc.1** = host-facing (the cable to the head-unit / dev Mac). Runs the **OCBM accessory**
  gadget (`/sys/class/android_usb_accessory/android0`, VID 0x1314). This is our transport.
- **ci_hdrc.0** = phone-facing (female USB-A). OTG dual-role. Presents the **`iap2,ncm` gadget**
  (`/sys/class/android_usb/android0`, VID 0x08e4) to the iPhone. This is the CarPlay accessory port.
- The two controllers are independent — OCBM (host-facing) and iAP2 (phone-facing) run at once.

### Host-app-driven projection lifecycle (the trigger for everything below)
The box does **not** switch the phone on its own. It boots to **IDLE**: `ocbm_boot.sh` launches
`ocbmd` and `session_supervisor.sh` (idle-waiting on `/tmp/host_present`); the iPhone stays in normal
mode (`05ac:12a8`), `iap2d` is not running, until a **host app** acts. The host app owns the authority
(see `docs/carplay/02_SESSION_LIFECYCLE.md`):

- Host app **SUBSCRIBEs** over the CH_CTRL session-control channel → ocbmd sets `/tmp/host_present=1`.
- On the 0→1 edge the supervisor runs **`projection_up.sh`** — the IDLE→projection bring-up (the cold
  start below, idempotent: it no-ops if already Identified) — **then** ARMs airplayd + rx_connect, so
  the iPhone opens its session.
- Host STOP / crash → ocbmd sets `/tmp/host_present=0` → supervisor tears down airplayd + rx_connect;
  **iap2d / the iAP2 link stay up** (holding pattern), so the next SUBSCRIBE re-arms instantly.

ocbmd tracks presence and mirrors it to `/tmp/host_present`; `projection_up.sh` and
`session_supervisor.sh` are the scripts. This is hardware-validated across a reboot. Auto-triggering
projection on phone *plug* without a host app is intentionally NOT done — the app is the authority.

### The cold-start sequence (proven with a real iPhone; run by `projection_up.sh`)
The iPhone enumerates on ci_hdrc.0 as a **device** in PTP mode (05ac:12a8, config 1 of 4). To start
wired CarPlay it must be role-switched to USB **host**, and the box must present the accessory gadget
the instant it does. Sequence (`projection_up.sh`, and `old/ncm_carplayd/ccpa/probes/iap2_carplay_test.sh` —
that tree is archived at `~/Documents/carlink/old/ncm_carplayd`, docs/carplay/00_ARCHITECTURE.md §Vendored assets):

1. iPhone present as device → find `/dev/bus/usb/BBB/DDD`.
2. Configure the phone-facing gadget (do NOT enable yet): `bDeviceClass=239/2/1`, VID `08e4` PID
   `01c0`, iManufacturer "Magic Communication Tec.", iProduct "Auto Box", `functions=iap2,ncm`.
3. Prime the peripheral role: `echo 1 > /sys/bus/platform/devices/ci_hdrc.0/inputs/a_suspend_req_inf`.
4. **Role-switch:** `iap_role_switch /dev/bus/usb/BBB/DDD` issues the standard Apple accessory role-switch request
   `bmRequestType=0x40, bRequest=0x51, wValue=1` (0x52 = usbmux, NOT CarPlay). The iPhone
   role-switches to USB host and disconnects as a device.
5. `sleep 2`, then `echo 1 > enable`. The iPhone-as-host **enumerates our `iap2,ncm` gadget**
   (state → CONFIGURED). This creates `/dev/android_iap2` (char 10,59) and the phone-facing NCM netdev,
   which is **`ncm0`** on an OCBM box (CORRECTED 2026-08-16: this step said `ncm1`. The name is
   enumeration-order dependent, not a constant: `ncm1` is what the phone-facing link is called when a
   HOST-facing NCM gadget already owns `ncm0` — the `ncm_only`/`ncm_wifi` bring-up mode of
   `custom_init.sh` (host-facing `ncm` at 192.168.50.2; `start_main_service.sh` even speaks of
   "release phone-side ncm0 owner"), and the old `ncm_carplayd` Pi topology this sequence came from,
   whose ACTIVITY_LOG and `tools/cold_start2.sh:62` both say `ncm1`. On the OCBM path the host-facing
   gadget presents `accessory` only, so the phone-facing link is `ncm0` — `tools/cold_start_airplay.sh`,
   `docs/ops/captures/2026-07-09_airplayd_pairverify.log`, and §"Where CarPlay A/V goes" below.)
6. Run **`iap2d /dev/android_iap2`** — the iAP2 accessory L3 daemon.

Each fresh cold-start needs a **physical replug** (steps 4–5 must fire while the iPhone is a device);
once Identified, `projection_up.sh` is idempotent and re-SUBSCRIBEs re-arm without a replug.

### iap2d — the accessory handshake (this box's job)
Reuses carplayd's transport-agnostic `carplay-iap2-core` (link framing + auth/identify state machine
+ TLV builders + spec, all from Apple's CarPlaySimulator.devicekitplugin). Only the transport differs
from carplayd: a single `/dev/android_iap2` fd instead of FunctionFS ep1/ep2, and `host_configured()`
reads `android_usb` state. **MFi auth is done on the LOCAL i2c chip** (`/dev/i2c-1 @0x11`) — carplayd's
`mfi.rs` (a remote NCM-based MFi bridge at 192.168.50.2:5290) is deliberately NOT used by `iap2d`, which
drives the chip directly (`ccpa/iap2d/src/main.rs`, the `G_I2C`/`i2c_rd` path).

> **⚠️ UPDATED 2026-08-16 — "this setup has no NCM" no longer holds repo-wide.** It still holds for
> THIS wired CCPA path: `iap2d` drives the local chip and speaks no NCM. But the repo now also carries
> an opt-in MFi-over-NCM bridge (`ccpa/mfid` + `crates/mfi-wire`, gated on `CARPLAY_MFI_ADDR`) for the
> Pi / NCM bring-up boxes; with that variable unset — the CCPA's own case — the local i2c path is
> byte-for-byte unchanged. Full reasoning: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-07-1`.

Handshake flow (iAP2, all message ids from spec.rs):
1. **L2 link:** TX detect prelude + SYN → RX SYN-ACK → TX ACK. (Established with the real iPhone.)
2. **Auth (0xAA0x):** RX `0xAA00 RequestAuthenticationCertificate` → local `mfi_cert()` (reg 0x30/0x31)
   → TX `0xAA01`. RX `0xAA02 RequestAuthenticationChallengeResponse` → local `mfi_sign(challenge)`
   (0x20/0x21 write, 0x10 go, 0x11/0x12 read) → TX `0xAA03`. RX `0xAA05 AuthenticationSucceeded`.
3. **Identify (0x1D0x):** RX `0x1D00 StartIdentification` → TX `0x1D01 IdentificationInformation`
   (`declare_wired=false`; declares the USBHostTransportComponent with `cp_iface=1` = the NCM data
   interface that carries CarPlay A/V) → RX `0x1D02 IdentificationAccepted`. Done.

### Where CarPlay A/V goes (KEY)
CarPlay video/audio is **NOT** carried over wired iAP2 (`0x4301 CarPlayStartSession` / `0xEA00` is a
documented dead-end on this hardware). After Identify, the iPhone opens a separate **AirPlay session
over the phone-side NCM (`ncm0` on this box — the phone-facing NCM; IPv6 link-local `fe80::…:5000`)**:
advertise `_airplay._tcp`, iPhone browses `_carplay-ctrl._tcp`, dials `GET /ctrl-int/1/connect`, then
RTSP/AirPlay. HEVC is confirmed in `carplayd`/`receiver_core` on a Pi 4B.

### Committed boundary (supersedes the earlier "host runs the whole session" note)
See repo `README.md` §Architecture. Split by stability:
- **Box (stable):** runs the AirPlay **pairing path of `receiver_core`** on its real `ncm0`
  (pair-setup/verify + **MFi on the local chip**), derives the **ChaCha20 session key**, and forwards
  the **encrypted A/V untouched** over OCBM plus the **ephemeral session key**. All MFi crypto stays on
  the box; the app never relays an MFi op — the private key never leaves the chip.
- **App (evolving):** claims the accessory over USB, decrypts with the handed-over key, **drives the
  post-pairing SETUP** (codecs/HEVC, resolution, screens — target), decodes HEVC (VideoToolbox), renders
  (the shipping macOS app now lives in THIS repo at `host/CarPlayHost/carlink_macOS`; it began as
  `old/ncm_carplayd/macos` carplay-app), and sends input back.

So: **box = iAP2/pairing *mechanics* + MFi + encrypted-A/V forward + key handoff; app = every
configurable content decision (incl. the iAP2 identification/declaration content, pushed as config
at init — docs/carplay/04_CAPABILITIES_AND_CONFIG.md) + decrypt + SETUP + decode + UI.** The old "bridge the raw IP session over `CH_IP` and run the whole receiver on the host" plan is
dropped — no OS interface, and the box terminates the transport on its own `ncm0`.

### Status
**DONE (2026-07-10): the whole phone-side path validated on hardware, end-to-end.** iAP2 handshake to
Identified (SYN-ACK → 0xAA00/cert(945B)/0xAA01 → 0xAA02/sign(128B)/0xAA03 → 0xAA05 AuthSuccess →
0x1D01(275B) → 0x1D02 IdentifyAccept → Identified, holds the link), MFi auth on the local i2c chip.
Two fixes got here: (1) local-i2c MFi (not the NCM bridge), and (2) `carplay-iap2-core`
`link.rs::parse()` tolerates coalesced reads (the iPhone piggybacks 0xAA05 with the next 0x1D00 frame).
Beyond Identify, pair-setup/verify on `ncm0` derives the ChaCha20 session key, and the box **forwards
the encrypted A/V + hands the per-stream key over OCBM** (`CH_VIDEO`/`CH_MEDIA_AUDIO`); a Rust debug
receiver (`ocbm-host avdec`) decrypted hundreds of video frames + thousands of audio packets host-side,
**0 failures**, driven by the host-app-driven projection lifecycle above. Pairing persists (disk-backed
PeerStore) so a known device reconnects with pair-verify only. The host app that was "remaining" here is **done and
hardware-validated** (`host/CarPlayHost/carlink_macOS` — VideoToolbox decode/render, audio, touch
uplink). Open work: `../ops/04_OPEN_ITEMS.md`. See also `README.md` §Architecture and
`../carplay/02_SESSION_LIFECYCLE.md`.

