# 04 — System model: what this project builds

**Canonical.** Where another doc in this repo disagrees on *what the system is*, this one is right.
Reading order: this file (what it is, and why it is built this way) → [`05_SESSION_FLOW.md`](05_SESSION_FLOW.md)
(wire-level ordering, authoritative on session sequencing) → [`01_FINDINGS.md`](01_FINDINGS.md) (proven on
hardware) → [`00_HANDOFF.md`](00_HANDOFF.md) (current state).

---

## 1. What this is

A **wireless-only** CarPlay receiver running as an **unprivileged Android app** on a 2024 Silverado
head unit (GM Info 3.7 / `gminfo37`, Android 12 / API 32, x86_64). The iPhone streams CarPlay to the
app **over the vehicle's own 5 GHz hotspot** (its SSID is deliberately not recorded in these docs; bridge `br0`,
head unit at `192.168.5.1`). No cable ever connects the iPhone to anything in this design.

A Carlinkit **CPC200-CCPA** adapter, on USB and speaking a custom protocol (**OCBM**), does exactly two
jobs: it is the phone's **Bluetooth radio**, and it is the **MFi coprocessor**. It is not the AirPlay
endpoint, it is not an access point, and no media ever crosses it.

There are two such adapters, and they are interchangeable. Both run identical OCBM firmware built
from `ocbm`, on identical hardware **except the WLAN chipset** — one NXP IW416, one Realtek
RTL8822CS. The firmware's `radio_caps` layer (`RADIO_CHIP`, `RADIO_BT_ATTACH`, `RADIO_WLAN_MODULES`,
`RADIO_BT_AFTER_WLAN`) abstracts that difference, so nothing in this app is chipset-aware and neither
box is "the test box". Two places where the variant is nonetheless observable: the Realtek part must load
its WLAN driver before Bluetooth will attach, which makes `wlan0` exist (§2), and the two boxes derive
different identities (each box's Bluetooth name is derived from its own identity), so a phone bonded to
one holds no record for the other.

**Install identity and USB attach.** The app installs as package `android.car.usb.handler` (classes stay
`zeno.gmccpa.*`; `tools/build_apk.sh` `INSTALL_PKG`). GM's fixed USB handler is that literal package, so on
every USB host attach the framework grants device permission to this uid and then starts
`android.car.usb.handler.UsbHostManagementActivity`, which the manifest declares as an alias onto
`UsbAttachActivity` (post-grant entry). `AdapterAttachReceiver` (manifest receiver for
`USB_DEVICE_ATTACHED`) is the route that runs before the grant; its broadcast can precede the grant, so
its background pre-check may read "no permission". `UsbAttachActivity` also carries the `device_filter.xml`
filter for `0x1314:0x2d00`; the ordinary resolver path to it does not run on this unit.

The USB permission prompt is removed by a *persistent* grant written once over adb by an `app_process`
helper running as the shell uid (bench-verified 2026-10-07; `docs/host/01_ANDROID_AND_AAOS.md`, "Bench
verification of the persistent USB grant"). It is keyed to the app's uid, so it must be re-run after any
uninstall + reinstall and after a box swap; a plain `install -r` keeps it. Without the grant the auto-claim
never prompts: it logs `SKIP_NO_GRANT` and the driver's Start asks as before (`AutoClaimPolicy`).

---

## 2. Division of responsibility

| Component | Owns | Explicitly does NOT |
|---|---|---|
| **Head-unit app** (Kotlin shell + JNI'd Rust core) | OCBM link and session lifecycle; AirPlay/CarPlay receiver identity and protocol; RTSP/pair-verify/auth-setup; video + audio decode; touch/HID; UI; supplying the vehicle hotspot credentials | Touch the MFi chip (SELinux-blocked); read the SoftAP passphrase (privilege-blocked — the driver types it in) |
| **CCPA adapter** (over USB/OCBM) | Its own Bluetooth radio: HCI bring-up, SSP pairing, SDP, RFCOMM, the iAP2 accessory handshake, accessory-initiated reconnect. The MFi coprocessor, served locally (BT-side iAP2 auth) and remotely to the app over `CH_MFI` | Raise its own SoftAP (`wifi_ap:false`, enforced by the box supervisor — see §3 Phase 0); carry video, audio, metadata, or input |
| **Vehicle** | The 5 GHz SoftAP on `br0` the app already lives on; the GM Bluetooth stack, which is bypassed entirely because the CCPA uses its own radio | — |

The app runs **on the AP host**, so client isolation never applies to it — see
[`01_FINDINGS.md`](01_FINDINGS.md) §2.

**The box-side A/V layer is not gated off, and that is deliberate for now.** `av::ensure_av_layer`
is called unconditionally after the `0x5703` write (`crates/vendor/wireless/src/bt_driver.rs:446`), so the
box's `carplayd` (which carries the in-process mDNS discovery) starts, latches "A/V layer up" and carries no media for us. An `AV_DISABLED` gate
that stops it from spawning does not exist; it would be a Rust change in
`crates/vendor/wireless/src/av.rs::ensure_av_layer` (rationale: `tools/session_supervisor.sh`, comment
block above the bridge-role launch). Pointing the launch env at a no-op binary does not work: the
health-check's `pid_alive` requires `argv[0]` to equal the full binary path, which no shell script or
immediately-exiting binary satisfies.

*Why it is not harmless on every box.* With `wifi_ap:false` the box raises no SoftAP, but on the Realtek
part `RADIO_BT_AFTER_WLAN=1` forces the WLAN driver to load before Bluetooth can attach (`[radio_hal]
chipset requires the WLAN driver before BT attach - loading it (driver only, no AP)` → `insmod
/tmp/88x2cs.ko if2name=sta0` → `WLAN interface wlan0 up - BT attach may proceed`), so `wlan0` exists.
Bench observation 2026-09-09 on that box: `carplayd` (mDNS discovery) advertised a **second**
`CarPlay._airplay._tcp` on `:5000` at `192.168.43.1`, under a different identity (`device id <MAC-1>`,
`pi 1544acf6-…`) from the app's and from the one the box presented over Bluetooth. No harm resulted
(`192.168.43.1` is the box's own unbridged interface; the phone is on `br0`), but "unreachable" is an accident
of routing, and a competing AirPlay endpoint with a mismatched deviceID is the fault class of Phase 5 in
[`12_OBSERVED_FLOW.md`](12_OBSERVED_FLOW.md). `AV_DISABLED` is worth doing; re-check on any box whose WLAN
chipset needs the driver up for BT.

---

## 3. Session flow

```
                    ┌─ BT (CCPA's OWN radio, not the vehicle's) ─┐
   iPhone ──────────┤                                             ├─ CCPA ─USB/OCBM─ HEAD-UNIT APP
          └─ 5 GHz WiFi (vehicle hotspot, br0) ─────────────────────────────────────► (AirPlay endpoint)
```

### Phase 0 — the app claims the box

The app claims USB `0x1314:0x2d00` (interface 0, bulk **IN `0x81` / OUT `0x01`**, 512-byte max packet;
hardware-verified 2026-08-12 — class KDoc of `UsbBulkTransport`). Endpoints are discovered by walking the
interface, not hardcoded. No AOA control handshake; it is a raw byte pipe with 16-byte LE framing and
resync. The box's accessory gadget uses `bDeviceClass=0` (class defined at the interface); PID `0x2d00`
is the only descriptor-level marker that the pipe no longer speaks the Carlinkit protocol, which keeps a
native-protocol app (`carlink_native` matches `0x1520/0x1521` only) from claiming a converted box
(`boards/ccpa-imx6ul/rootfs/script/ocbm_boot.sh`). Then `CT_HELLO` → `CT_HELLO_ACK`, `CT_SETTIME` (the box has
no RTC battery), `CT_SUBSCRIBE` with the session config — including `wifi_ap: false`
(`OcbmProbe.btOnlyConfig()`) — and a 1 Hz `CT_HEARTBEAT`.

**The app is the ignition.** The box's radios are off at boot; `ocbmd` mirrors host presence to
`/tmp/host_present` on the `CT_SUBSCRIBE` edge, and the box's `wireless_up()` brings the radios up from
there, reading `wifi_ap:false` out of that same config to suppress its own SoftAP (the `BOX_WIFI_AP = 0`
branch inside `wireless_up()`, `tools/session_supervisor.sh:994-995`, logging `box SoftAP SUPPRESSED`) while
still bringing up Bluetooth (`radio_hal.sh bt_on`, unconditional). If the app is not running and
subscribed, no session can start. The app learns the adapter is present without being open via the attach
routes in §1 and the other triggers in §4e.

The box's `CT_BOX_HEALTH` (`0x1A`) report carries per-subsystem readiness bits, including `BH_HCI_PRESENT`
(bit 0), which the box sets from `HCIGETDEVINFO` on a raw HCI socket — the same ioctl `hciconfig` uses to
print `UP RUNNING` (ioctl in `fn hci0_up`, set in `box_health_tick`, both in `box/ocbmd/src/main.rs`).
This is the signal the app's lifecycle layer (§4b) uses to know the box's radios actually came up, not just
that `ocbmd` answered.

### Phase 1 — Bluetooth bring-up and phone attach

The box raises HCI, the SSP agent, the SDP server and the RFCOMM listener, and advertises as a CarPlay
accessory. Two entry paths:

- **Known / last-connected iPhone** — the box initiates the reconnect (SDP query out, then RFCOMM
  connect out). It must search the *phone's* iAP2 UUID `02030302-1d19-415f-86f2-22a2106a0a77`, not the
  accessory's.
- **New iPhone** — the phone initiates. Default pairing is Just-Works (NoInputNoOutput), auto-accepted
  in-kernel with no user interaction. A numeric-comparison mode exists — the box reaches the app over
  `CT_PAIRING_CODE` to display it — but it is opt-in and experimental upstream; don't build a
  PIN-confirmation UI as the primary flow.

### Phase 2 — iAP2 identify + MFi, on the box

Over RFCOMM the box runs the iAP2 accessory handshake: link SYN/ACK, MFi **certificate + challenge
signature** from the coprocessor over local i²c, then Identify → IdentificationAccepted. The app is not
in this loop; it observes progress through `CT_SESSION_EVENT`.

### Phase 3 — handoff to the *vehicle's* hotspot

The iPhone sends iAP2 `0x5702` *RequestAccessoryWiFiConfigurationInformation*. The box answers `0x5703`
with the **vehicle's** hotspot credentials — SSID, passphrase, security type. The credentials are the
app's: they ride the `CT_SUBSCRIBE` YAML (`wifi_ssid`/`wifi_pass`, `SubscribeConfig.kt`), and the box
supervisor (`apply_host_wifi_creds`) rewrites `/etc/hostapd.conf` from `/etc/hostapd.conf.stock` before
`btd` starts. The box's `read_hostapd_ap_config()` (unchanged) then reads that file to build `0x5703`. No
channel is sent: the app does not supply one, so the supervisor strips any `channel=` only when a channel is
supplied and the box keeps its stock hostapd channel line (owner, 2026-10-07). There are no compiled-in
credentials, OD-7A: with none saved, a start opens the hotspot dialog instead of subscribing. The box's AP
never comes up. The iPhone leaves Bluetooth-only and joins the vehicle hotspot on `br0`. There is no join
confirmation over iAP2 — a successful `0x5703` write is the only signal, and iOS retries `0x5702` two or
three times as a matter of course, so the handler must be idempotent.

The passphrase cannot be read programmatically by any app on this head unit; the driver enters it once
from the OS hotspot GUI (`com.gm.hmi.connection`'s `WifiHotspotActivity` — keep that package).

### Phase 4 — the app becomes the accessory on Wi-Fi

The app advertises `_airplay._tcp` on `br0` via **two adverts run concurrently, deliberately**
(`CarPlayRx.advertise()`, "ALONGSIDE NsdManager" comment block): `NsdManager.registerService` (primary — the
one iOS has demonstrably indexed) **and** a hand-rolled, self-hosted `MdnsResponder`. The second exists
because AOSP drops `NsdServiceInfo.setHost()` on registration, so `NsdManager` cannot set the SRV target,
and it shares GM's `Android.local` host, whose Bonjour record wins the endpoint-index race whenever GM
registers first (`MdnsResponder` class KDoc, "Why this exists"). `MdnsResponder` answers mDNS itself, owns
`gmccpa-rx.local`, and publishes exactly one address (`br0`), pinning a second, separately-keyed endpoint
that carries the app's own deviceID — consistent with what the box presented over Bluetooth, on its own port
(GM's own CarPlay service holds `:7000` and must not be disturbed).

**Discovery is bidirectional.** The accessory also *browses* `_carplay-ctrl._tcp` and dials the phone
with `GET /ctrl-int/1/connect`; the phone then opens the **control** connection inbound
(`CarPlayRx` class KDoc rule 1, the `dialsSinceInbound` KDoc, and the `ORDER IS LOAD-BEARING` comment in
`CarPlayRx.start()`). The app never dials the phone for the control channel itself, but it does have to make
that outbound nudge. Two TXT values are load-bearing: `features` must carry the **Car** bit (high word
bit 32) or iOS answers the connect-out and never opens RTSP; `features` / `deviceid` / `pi` must all equal
what `/info` returns or pair-verify fails. See [`05_SESSION_FLOW.md`](05_SESSION_FLOW.md) §3.

From here the app and the iPhone speak the Apple protocols directly over Wi-Fi: RTSP/HTTP control,
pair-setup (SRP-6a) and pair-verify (Curve25519/Ed25519), neither touching the chip, MFi-SAP `auth-setup`,
per-stream setup, then HEVC video and AAC/AAC-ELD audio, HID input uplink, mic uplink, and iAP2 over the
AirPlay DataStream for metadata and controls.

### Phase 5 — MFi during the session, relayed over OCBM

`auth-setup` runs after pair-verify on **every** connect, not just first pairing, and calls the
coprocessor twice — `create_signature` **then** `copy_certificate` (signature first). The app relays
both over OCBM `CH_MFI`; the chip's private key never leaves the CCPA.

### Phase 6 — a second, independent iAP2 session over the DataStream

Once the session is recording, the app opens a fresh iAP2 session over the AirPlay DataStream (SETUP
stream type 130) carrying metadata and controls. It continues nothing from the Bluetooth link — full
DETECT/SYN, MFi certificate + signature, and Identify all run again. That is two more chip calls.

So: **six chip operations per session, four of them relayed by the app over `CH_MFI`** (table at §10 of
[`05_SESSION_FLOW.md`](05_SESSION_FLOW.md)). The OCBM link stays live for the whole session but still
carries only kilobytes.

---

## 4. What crosses each link

| Link | Carries | Does not carry |
|---|---|---|
| **Bluetooth** (CCPA's own radio) | iAP2 handshake, MFi auth, the `0x5702`/`0x5703` handoff. Stays up as the session anchor. | Media of any kind |
| **USB / OCBM** (`0x1314:0x2d00`) | `CH_CTRL` (handshake, heartbeat, session events, pairing code), `CH_MFI` (four chip ops/session), `CH_MGMT` (health, identity, forget-bond, reboot), `CH_LOG` (the box's own logs, PUSHED continuously into app logcat as `[box:<source>]` lines, armed by `CT_LOG_CTL` — `OcbmClient.handleLog`), `CH_FILE` (one-shot `FILE_PULL` of the few logs `CH_LOG` does not carry), `CH_CONSOLE` (rescue console). Kilobytes. | Video, audio, metadata, input, mic — ever |
| **5 GHz Wi-Fi** (`br0`) | Everything heavy: video, audio, RTSP control, pair-verify, auth-setup, HID, mic, iAP2 DataStream | — |

The channel map is `crates/ocbm-proto/src/lib.rs`. `CH_IP`/`CH_ETH` are unused (media is not tunneled).

**Why media doesn't ride USB:** not bandwidth — `docs/carplay/00_ARCHITECTURE.md` measures 339 Mbps
adapter→host and 90 Mbps host→adapter, above CarPlay's 8–30 Mbps video need. The constraint is §4a: the box
has no network route to `br0`, and the phone must join the *vehicle's* SoftAP, not the adapter's.

### 4a. Why the receiver must run on the head unit

**Governing requirement: the Wi-Fi the iPhone joins must be the vehicle's SoftAP, not the adapter's.**
With the phone on the vehicle SoftAP, the receiver must be reachable on `br0` / `192.168.5.0/24` — and
the box has no route to that subnet.

The box cannot join the vehicle SoftAP as a station on this firmware:

- No `wpa_supplicant` binary on the rootfs (only orphaned `.conf` files), and no `iw`, `iwconfig`,
  `wpa_cli`, `iwpriv`, `mlanutl`.
- Only `hostapd` is present — AP mode only.

Bridging the box onto `br0` over OCBM was evaluated and rejected too: `CH_ETH` needs `AF_PACKET` on
`br0` (`CAP_NET_RAW`/root — the app has neither), and `CH_IP` is host-initiated only, while the receiver
makes outbound flows on ephemeral ports advertised inside encrypted SETUP responses.

> **No per-channel SUBSCRIBE in OCBM.** `CT_SUBSCRIBE` is a presence latch plus an opaque YAML blob; a host
> cannot ask the box to stop sending a channel. `CH_VIDEO`/`CH_MEDIA_AUDIO`/`CH_ALT_VIDEO`/`CH_METADATA`/
> `CH_INPUT`/`CH_MIC` stay silent here because the box-side A/V layer (§2: spawned, not gated) has no
> phone media to carry in this role — the phone never joins the box's network, since the role is
> `wifi_ap:false` (`SubscribeConfig.kt`). It is a box-side role change, not a protocol negotiation, which
> is why the role flag is load-bearing rather than cosmetic.

---

## 4b. Lifecycle and health: the app/box state machine

*Wire ordering is in [`05_SESSION_FLOW.md`](05_SESSION_FLOW.md) §8; landed-vs-deferred status in
[`11_HARDENING_PLAN.md`](11_HARDENING_PLAN.md). Implemented in `SessionSupervisor.kt`.*

State is an explicit machine, not inferred from which objects happen to be non-null. Three situations
that need opposite responses have to stay distinguishable inside the reconnect loop:

1. the phone has not dialled back **yet** — wait;
2. the phone dialled back and hung up — wait ~10 s, it usually returns;
3. the phone can see us, answers our nudge, and **will not** connect — waiting is useless.

### Phases

```
IDLE ──onReceiverReady──→ RX_READY ──onBoxLinked──→ BOX_LINKED ──onBoxSubscribed──→ ARMED
  │                          ↑                                                        │
  ├── RX_UNHEALTHY ──────────┤                        BT_PAIRING → BT_PAIRED → HANDOFF_SENT
  └── BOX_UNHEALTHY ─────────┘                                                        │
                                              SESSION_UP ←── INBOUND_EXPECTED ←───────┘
                                              │      ↑             │ 15 s
                                       10 s   ↓      │             ↓
                                            GRACE ───┘        escalate → ARMED
                                              │                     ↓ (ladder exhausted)
                                BOX_LOST_SESSION_UP               STALLED
```

Every state has an exit. Discovery is suspended in exactly `SESSION_UP`, `BOX_LOST_SESSION_UP` and
`GRACE`, and running everywhere else including `STALLED` — the phone may return unprompted at any
moment.

**`SESSION_UP` is entered at pair-verify, not at accept.** The accept of a control connection on `:7011`
stands the consumers and the CarPlay screen up at once (`CarPlayRx.onSessionUp` →
`SessionCore.showCarPlayScreen`; streams are SETUP within ~300 ms of RECORD), but `SessionCore.sessionUp`
and `supervisor.onSessionUp` move in the pair-verified callback (`SessionCore.onPairVerified`, before the
rekey early return). Otherwise any hotspot client that opened the port would pause discovery and cancel
every recovery clock. A connection still plaintext 10 s after accept is closed by the pump as
`EndReason.UNVERIFIED` (`UnverifiedConnRule`, `CarPlayRx.handleNative`), sending or silent; SessionCore
takes the screen down but does NOT hand that end to the supervisor — no GRACE, no ladder reset, the phase
stands. A real phone that closes early on its own still arrives as `CLOSED` and keeps the grace window
(case 2 above).

### Rules that are not obvious

- **`BTP_WIFI_HANDOFF` is the only trustworthy "a CarPlay connect is coming" signal.** The phone
  appearing on the SoftAP is not one: the vehicle hotspot is an ordinary saved network on the phone, so
  it joins whenever in range whether or not CarPlay is involved.
- **A live session outranks every BT phase.** Bluetooth builds a session; it does not sustain one. Once
  the phone is streaming over Wi-Fi, a late or repeated `CT_BT_PHASE` says nothing about session
  health, and acting on it walks the machine backwards out of `SESSION_UP` — which resumes discovery
  (letting the 12 s rediscover loop hijack the live control connection) and arms the 45 s handoff
  watchdog, whose expiry escalates the recovery ladder into cycling the box's radios under a working
  session. `onBtPhase` therefore returns early in `SESSION_UP` / `BOX_LOST_SESSION_UP`, as
  `onBoxHealth` and `onDialAccepted` do. Device-observed 2026-09-08 (`CarPlay live -> pairing` at 900
  rendered frames).
- **No new control connection displaces a live encrypted session (OD-7F, owner 2026-10-07).** The accept
  loop asks `Admission.decide` (`CarPlayRxSlots.kt`, `internal object Admission`) first: a LOCAL newcomer
  (`Admission.isLocal`: loopback, the socket's own address, an address the head unit owns, or an
  unnameable peer) is always refused; with no incumbent, a closed incumbent, or an incumbent whose control
  channel is not yet ENCRYPTED (pair-verify not done), the newcomer is admitted; against an ENCRYPTED
  incumbent every newcomer is refused. There is no quiet-time or third-try escape. Without this rule the
  newest connection wins and the rediscover loop above could hijack a live session. Test status: host-JUnit
  only (`AdmissionPolicyTest`); the cp12 bench capture (rc1/rc2) contains no refusal line, so the refusal
  path has not been exercised on the unit.
- **Losing the box mid-session must not end the session.** The A/V path is Wi-Fi and does not depend on
  Bluetooth or on the box once streaming. On the box's return the app re-establishes the link *without*
  `CT_SUBSCRIBE` (`OcbmProbe.runAll(subscribe = false)`) and asserts `CT_RADIO` off, so the MFi relay
  comes back — `/auth-setup` is per control connection, not per pairing — while the box's radios stay
  down under the live session.
- **`OcbmProbe.runAll()` returns a `LinkResult`, and callers must gate on it** (`SessionCore.autoStart`,
  the `if (!r.helloOk)` branch under the `GATE ON THE RESULT` comment). `LinkResult` carries `claimed`,
  `helloOk`, `mfiProven`, `subscribed` as explicit fields; ignoring it lets the launcher announce "box
  claimed, MFi proven" with no adapter on the bus (device-observed 2026-08-28).
- **`CarPlayActivity.onSessionEnded()` tears the screen down on session end** — the LIVE screen does not
  linger after the phone disconnects (companion `onSessionEnded` in `av/CarPlayActivity.kt`).
- **A readiness probe must not perturb what it measures.** `CarPlayRx.selfTest()` deliberately does not
  dial `:7011` on loopback — since OD-7F `acceptLoop` refuses a local newcomer, so such a dial would only
  log a refusal and prove nothing. Holding the bound, open `ServerSocket` is the capability that matters
  and is directly observable.

### The recovery ladder

| Rung | Action | Cooldown | Recheck after | Touches `host_present`? |
|---|---|---|---|---|
| 0 | re-announce mDNS | 30 s | `LADDER_RECHECK_MS` 20 s | no |
| 1 | `CT_RADIO` off → on | 90 s | `RUNG1_RECHECK_MS` 35 s | **no** — this is the point of it |
| 2 | `MGMT_RESTART_WIRELESS` | 180 s | `RUNG2_RECHECK_MS` 45 s | no |
| 3 | terminal: tell the driver to reconnect Bluetooth from the iPhone | — | — | — |

The ladder re-enters itself on a timer (`SessionSupervisor.escalate` arms the retry, because a silent phone
produces no events). Its POSITION resets to rung 0 on a real session (`onSessionUp`, which also cancels the
retry), on a session ending (`onSessionDown`) and on the box link going away outside GRACE (`onBoxLost`, so a
retry armed before the transport died cannot fire into the relink); the per-rung cooldown stamps survive all
three. Cooldowns are correctness, not politeness — see §4c. The ladder runs on its own; Restart Session
clears its cooldowns, because those exist to stop the machine hammering the box unattended and a human
request is not that (`SessionSupervisor.onOperatorRestart`; there is no separate "Recover" button).

**The ladder must not fire into live progress.** The blind retry carries the ORIGINAL `why` and would fire
regardless of what the box and phone achieved since. `deferLadder()` cancels it on forward progress —
`BTP_LINK_UP`/`AUTHENTICATING`, `BTP_IDENTIFIED`, `BTP_WIFI_HANDOFF`, and `onDialAccepted`. It deliberately
does NOT reset `rung`: ladder position and cooldowns are earned state, and a phone that stalls again must
resume where it left off. It only yields the CLOCK to the narrower deadline the new phase just armed (45 s
handoff watchdog, or 15 s dial-back), each of which re-enters `escalate()` on expiry — so a genuinely stuck
phone still escalates, just never on top of a live handshake. The failure this prevents, device-observed
twice on 2026-09-08 (rung 2 shot a handshake in the back mid-authentication):

```
12:07:31.778  rung 1 (CT_RADIO off/on)
12:07:50.160  BT_PHASE LINK_UP        -> pairing
12:07:51.150  BT_PHASE AUTHENTICATING
12:07:51.779  rung 2 (MGMT_RESTART_WIRELESS)   <- 20.001 s after rung 1
12:07:52.296  BT_PHASE WIFI_HANDOFF / PROJ_MODE WIRELESS_CP
12:07:53.284  BT_PHASE IDLE / PROJ_MODE NONE   <- collapsed
12:07:55.659  BOX_HEALTH 0x43                  <- airplayd gone
```

Rung 2 tears down the box's whole wireless stack, `airplayd` included, after which the phone 200-OK'd every
`/ctrl-int/1/connect` nudge forever and never dialled back; iOS records this as `EAAccessoryLeft`.
From the head-unit side alone it reads as failure point FP5 ("phone reads TXT, never SRV-queries us") and
sends you hunting a phone-side fault that is not there. The session that finally succeeded that day did so
**because the ladder had exhausted into STALLED**, which stops arming the retry: if a session only connects
after the ladder is spent, suspect the ladder.

**Rung 3 is not an action, and that is deliberate.** A full manual reset — box power-cycled, app
relaunched, receiver re-bound, Bluetooth re-paired, Wi-Fi re-joined through the proper CarPlay prompt —
does not always recover a session where iOS reads `_airplay._tcp` TXT and issues no SRV query. Nothing
reachable from the head unit clears that; an app that keeps silently retrying what cannot work is worse
than one that says so. The failure rung 3 exists for is a bond mismatch (§4c.4), and no rung the app can
reach repairs a key the *phone* has thrown away. If rung 3 ever grows a body, it should name the specific
remedy — forget on both sides and re-pair — rather than retry the reconnect the box is already retrying.

---

## 4c. Constraints that bound any recovery design

These interlock, and a design that ignores one will break the session it is trying to rescue.

1. **`CT_SUBSCRIBE`'s Wi-Fi credentials are one-shot.** The box applies them only inside
   `wireless_up()`, on the `host_present` 0→1 edge. A later subscribe carrying credentials does not
   re-apply them. A genuine reset therefore needs a full `host_present` cycle.
2. **One OCBM bring-up per app process.** A second `claimInterface` orphans the MFi signer bound to the
   dead client and surfaces later as iOS `-72542` at `/auth-setup`, not at claim time. An app-side stack
   reset means a *process* restart.
3. **A `host_present` cycle inside ~20 s trips the box's flap detector**, escalating to an `ocbmd`
   restart and then a full box reboot.

(1) + (2) + (3) mean "just restart both stacks" is the most expensive rung available and must be
rate-limited. `CT_RADIO` resets the box's radios **without** touching `host_present`, so it is the
strongest lever carrying no flap exposure at all.

4. **The box CAN page the phone, but only against a two-sided bond.** The box runs a `reconnect` loop that
   walks `/etc/carplay/bt_link_keys`, SDP-queries the phone and dials iAP2 RFCOMM outbound. Device-observed
   twice on 2026-09-08: the box paged the phone, brought up L2CAP, read the phone's `Wireless iAP v2`
   record and resolved RFCOMM channel 1 — so the paging itself works.

   What it cannot survive is a **one-sided bond**. When the box holds a link key the phone has
   discarded, the authenticated RFCOMM connect times out after ~8 s
   (`[reconnect] RFCOMM connect to channel 1 failed: connect timed out`), and iOS reports the mirror
   image — `keys available ? No`, `retrieve key chain magic key data ... failed with result 150`, then
   `Connection to device ... failed - result was 431` on the *Wireless IAP* profile. The phone shows
   the accessory as paired while holding no key for it.

   A reconnect that will not complete is therefore a **bond mismatch**, not an inability to page. The fix
   is to clear BOTH sides — `MGMT_FORGET_ALL` on the box (`--es run ocbm_forget`) and Forget This Device
   on the iPhone — and re-pair. Clearing only one leaves the same half-bond in the other direction. See
   the three independent pairing stores in `evidence/session_2026-08-12-freshpair/README.md` (archived in the
   standalone `gm_ccpa` checkout; see `host/gm_ccpa/README.md`, "Archive and rollback").

---

## 4d. Display modes: CarPlay view areas and GM's system UI

*Device-proven 2026-09-08 on gminfo37. Implemented in `av/CarPlayActivity.kt`; geometry declared in
`assets/info.bplist` via `tools/info_plist_viewareas.py` and primed in `native/carplay-jni/src/lib.rs`.*

The app has exactly two display states, and CarPlay's own resize button switches between them.

| | View area 0 | View area 1 |
|---|---|---|
| Rect | `2400x960 @ (0,0)` — the whole panel | `1416x842 @ (188,118)` — GM's app bounds |
| GM system UI | hidden (immersive) | **visible** — LeftBar, TopCarSystemBar, widget pane |
| Surface | full-bleed | cropped to the rect on the hardware composer |

Both rects are STATIC constants for this panel; nothing is measured, awaited or renegotiated at runtime
(this app targets one radio and one display, unlike `carlink_native`, which must discover its display).

### How the resize works

`/info` declares two `displays[0].viewAreas[]` entries with `adjacentViewAreas` DERIVED from
`initialViewArea`, and the `viewAreas` feature is negotiated in the SETUP `enabledFeatures` (the lever
in `lib.rs`; without it iOS ignores the whole structure). iOS then draws the Dock resize button and
sends `/command requestViewArea`, which reaches the app as a `META_CMD` plist on the `:9004` seam; the
receiver answers `updateViewArea` with `animationDurationMillis`.

The two keys are distinct in Apple's model (`docs/carplay/06_AV_PIPELINE.md`, WWDC 2019-252): `viewAreas` carry
resizing, `safeArea` is Apple's mechanism for non-rectangular panels. The app serves a static `/info` plist,
so `build_info` never runs; `native/carplay-jni/src/lib.rs` calls it once to prime
`info::declared_view_area_count()`, without which the receiver refuses `requestViewArea index=1`.

**iOS keeps the coded size CONSTANT and moves the crop.** Measured here: 130 parameter-set bursts with
identical CSD and zero decoder reconfigurations. So there is one decoder, never two, and no
re-negotiation — the buffer is always the full panel with iOS's UI drawn into the sub-rect and black
around it.

### The two rules that are easy to get wrong

1. **Crop, never scale.** Shrinking the `SurfaceView`'s LAYOUT makes SurfaceFlinger scale the whole
   2400x960 frame into the smaller window — the UI lands at 59% and mispositioned. The correct
   operation is a crop, and a `SurfaceView` cannot crop its buffer through the View API. It is done
   with `SurfaceControl.Transaction.setGeometry`, which keeps the layer on the hardware overlay
   (`composition=DEVICE`, `forceClientComposition=false`, `usesClientComposition=false`) — a
   `TextureView` would achieve the same picture by moving 2400x960 60 fps HEVC onto GPU composition.
   The crop is also what stops us painting over GM's right-hand widget pane.
2. **Toggle bar VISIBILITY, never window LAYOUT.** AAOS insets the CONTENT, not the window: `mBounds`
   is the full panel and `mWindowingMode=fullscreen` in both states, and GM's bars are higher-Z
   system-UI windows that overlay ours by default. So `LAYOUT_STABLE | LAYOUT_FULLSCREEN |
   LAYOUT_HIDE_NAVIGATION` stay set permanently and only `IMMERSIVE_STICKY | FULLSCREEN |
   HIDE_NAVIGATION` — plus the theme's `FLAG_FULLSCREEN`, which suppresses the status bar
   independently — move. Clearing the LAYOUT flags too moves the root to (189,118) and displaces the
   surface to 377,236.

Touch is unaffected in both states: it normalises against the PANEL, which is the space
`/info displays[]` advertises and iOS maps taps from. A view area is a sub-rect inside that space,
never a new coordinate system.

### Timing

The transition is asymmetric and deliberately not interpolated — the app cannot see iOS's per-step
rect (the `AirPlayScreenHeader` carrying it is consumed in-process before the seam), so any ramp runs
on a different clock. Growing applies the target at once; shrinking waits the animation out and then
snaps, because a crop that leads iOS clips its animation. Safe to schedule blind because the duration
is ours — the receiver sends `animationDurationMillis` itself.

A non-privileged app cannot drive GM's bar show/hide animation in step with iOS's; that is system
UI's own transition.

---

## 4e. The app surface, as built

*What the Kotlin shell does around the protocol. Code is under `app/src/main/java/zeno/gmccpa/`, cited by
symbol; commit history is in `00_HANDOFF.md` §2. Bench status is stated per item.*

**Launcher (`Ui.kt`, `LauncherModel.kt`).** Two panes: status on the left (state word, driver line, pairing
code, the adapter's Bluetooth name, the V5 rekey hint, and the always-visible hotspot chip "Hotspot: <ssid> ·
Edit" or "Hotspot not set · Set"), actions on the right chosen by state (`LauncherModel.actionsFor`). The
verbs are **Set hotspot** (while credentials are missing), **Start**, **Restart session**, **Stop** and
**Adapter & logs**. Restart session also clears the recovery ladder's cooldowns
(`SessionSupervisor.onOperatorRestart`). The **Adapter & logs** sheet (`SheetItem`, `ADAPTER_SHEET_TITLE`)
holds Restart adapter Bluetooth, Reboot adapter, Forget pairing (each confirmed, `confirmBodyFor`), Export
logs, and the full "Last message". Saving changed hotspot details while the box is subscribed offers **Apply
now (restarts the session)** / Later (OD-9A: the offer in `Ui.kt`, `credentialsApplyOffer` →
`SessionCore.credentialsNeedResubscribe` and `MainActivity.onApplyCredentialsNow`). Driver wording is the
OD-9B table, `LauncherModel.driverLine`. The launcher's metrics are applied from a
`ViewTreeObserver.OnPreDrawListener`, not from a layout-change listener (no `requestLayout()` inside the
layout pass). Bench-pending: layout and keyboard handling; measuring the
launcher (gfxinfo janky %, "requestLayout() improperly called" count).

**Hotspot credentials (OD-7A).** No compiled-in SSID or passphrase: `LauncherUi` starts blank and loads only
what `HotspotPrefs` saved; the dialog's Save is the only writer. With none saved, Start or an attach opens
the dialog instead of subscribing (`SessionCore.credsMissing` → `blockStartForCreds`, called from
`autoStart` and the USB-attach path). `--es ssid/--es pass` apply to that run only and are not persisted
(`HotspotPrefs` KDoc). The app sends no channel (`SubscribeConfig.kt`). A `chan` an older build saved is
purged by the next Save (D015).

**Claiming the adapter with the app closed (D6, OD-9D, OD-9E).** Three triggers call `AutoClaim.trigger`:
`usb-attach` (`AdapterAttachReceiver`), `boot` (`logging/BootReceiver`, BOOT_COMPLETED, about 48 s after the
media-browser bind has usually claimed already, so it is mostly a NOOP) and `media-browser bind`
(`CarPlayMediaBrowserService.onCreate`, about +7 s at boot when this app is the media source). The decision
table is `AutoClaimPolicy.decide`; every trigger logs `auto-claim (<origin>): <DECISION> — <why>`. No dialog
is ever raised from the background. An on-screen **Stop** suppresses every trigger until the next head-unit
boot — a Stop does NOT survive a reboot (OD-9D): the boot identity is `Settings.Global.BOOT_COUNT`, else
`/proc/sys/kernel/random/boot_id`, else not suppressed (`AutoClaim`, `AutoClaimPolicy.sameBoot`); Start and
Restart session clear it. While the adapter is linked or a session runs, ONE ongoing notification (channel
`gmccpa_status`, "GM CCPA status"; the old `carplay_session` channel is deleted at start) shows the state and
carries no names or addresses (OD-9E, `av/StatusNotice`, `av/CarPlaySessionService`). "Adapter reconnecting"
with no adapter is by design, not a leak: a detach arms a relink bounded at 10 minutes (`OcbmProbe`). When
that relink ends without a link — expired, capped or failed, adapter still enumerated or not — the hold is
released by the same rule every other trigger end uses (`AutoClaimPolicy.shouldRelease`: no link and no
relink owning the claim; `SessionCore.onRelinkDone` → `releaseLinkIfUnlinked`). Bench: headless auto-claim
through the media-browser bind and the status notification were observed on rc2; the other triggers,
Stop-per-boot and the notification text are bench-pending.

**Pairing key (V5, Q001).** The accessory's Ed25519 seed is 32 `SecureRandom` bytes generated once per
install in `noBackupFilesDir` (`AccessoryKey`); `CarPlayRx` has no default seed. An install that finds an
existing peer store but no seed (an upgrade), or a corrupt/legacy-pattern seed file, persists a rekey flag;
`RekeyFlow` then runs Forget Pairing automatically once on the next OCBM link (OD-8B) and the launcher shows
"Accessory key changed — press … then forget this car on the iPhone, then pair again" until a pair-verify
succeeds under the new key. Every existing iPhone pairing must be redone once (owner, 2026-10-07).
Host-JVM tested; the on-unit run (B-V5-1) is pending. There is no Kotlin pairing fallback: when the native
core is unavailable the connection is closed with `native core unavailable — closing; there is no fallback`
(`CarPlayRx`; D016).

**Voice (calls, Siri, nav, alerts).** Design and routing: [`13_AUDIO_ROUTING.md`](13_AUDIO_ROUTING.md).
`:9003` carries voice-tag v2 (BEGIN/END, receive stamp, RTP timestamp). Each voice stream gets a jitter buffer
keyed on the RTP timestamp (`av/VoiceJitterBuffer`): target = the SETUP's `audioLatencyMs` (absent -> 80 ms,
floor 32 ms), watermark W = max(target + 20, 200) ms, trimmed DOWN TO W continuously ("real time, never a
growing buffer", owner 2026-10-07), the node pool as in stock (`av/VoiceLatency`). The AudioTrack is sized to
stock's per-usage device buffer (call 32, Siri 128, alert 48, nav 48 ms) and primed with silence.
**Start-transient exemption:** the HAL bus takes about 140-400 ms to wake after `play()`; the buffer is not
trimmed during that stall, what built up is carried as an allowance and paid back by dropping only silent AUs
(`VoiceLatency.START_STALL_CAP_MS` 500). On the rc2 nav stream logged at 04:48:20 that cost "excess 11 …
excess loud 0, silent catch-up 9". The voice workers and the `:9003` reader run at
`THREAD_PRIORITY_URGENT_AUDIO` (`VoiceLatency.VOICE_THREADS_URGENT_AUDIO`).
**NAV takes no audio focus** and plays live from its BEGIN to its END (OD-8C/8D, stock-aligned); CALL,
ASSISTANT and ALERT still request focus. Media ducking follows the iPhone's `duckAudio` / `unduckAudio`
(volume, duration) through one shaper; an unduck ramps back to 1.0 at once and the GM platform duck carries
the tail (`NAV_UNDUCK_HOLD` = false, the non-default software hold). **Stock-style duck lift:** once the nav
track's head first advances (bus1 active) our own software duck lifts in 100 ms so media sits at the platform
duck alone instead of stacking (`VoiceLatency.NAV_DUCK_LIFT_ON_BUS`, owner 2026-10-08). Rc2 capture: lifts
logged 286 ms and 313 ms after `duckAudio`, with the platform duck at -18.70 dB (cp12, 04:48:16 and 04:48:46).
**After a permanent media-focus loss** (the driver picks FM) focus is re-requested only on a resume intent — a
MediaSession play, a new screen, the surviving screen returning to the front (`CarPlayActivity.onResume` →
`AacPlayer.onScreenReturned`) — or when the phone itself resumes (`av/FocusReclaimRule`; a phone already
paused at the loss counts). The loss also takes MainAudio from the phone as stock does (OD-9C,
`av/MainAudioHandover`). **A focus request REFUSED at start** (AOSP rejects MUSIC while a CALL holds focus)
holds media silent until a later request is granted, as stock does (GM Cinemo configures its media streams
only after a granted request): the refusal sets the same `pausedForFocus` hold and `feed`'s gate re-requests
once per 3 s while media flows, the first refusal an E, repeats at I (C-F1 variant B, owner-approved
2026-10-08). Bench-pending: that refusal path, whether iOS pauses on the take, and whether a HID PLAY alone
resumes it (plan item B-MED-9).

**Voice-interaction assistant (wheel Voice key -> Siri; NOT bench-verified).** The app declares a
`VoiceInteractionService` + session service + a stub `RecognitionService` with no intent-filter, so it never
joins the speech-recognizer list (manifest voice block, `res/xml/voice_interaction_service.xml`,
`assist/Assistant.kt`). Inert until the app holds `android.app.role.ASSISTANT`, which is not requestable
in-app and is granted over adb (`cmd role add-role-holder --user 10`; status logged at startup). Once held,
GM's `GMCarInputService` routes a wheel Voice SHORT press to `AssistUtils.showSessionForActiveService`, i.e.
`AssistantSession.onShow`. That queues `NativeCore.siriTap` on the `cp-event-tx` executor, which sends
`requestSiri` siriAction 2 then 3 only once the CarPlay session has started (`NativeCore.siriTap`, the
`requestSiri` path in `carplay-jni/src/lib.rs`; -1 = not started, nothing sent), then `hide()`/`finish()`.
No call-state branching: iOS decides (`AssistOutcome`). Not delivered by this path: the hangup key (164
reaches only AudioService, though it closes an open voice session) and the call key (5 may reach a focused
activity, unmeasured). Procedure and bench checks: `06_BRINGUP_RUNBOOK.md` §4,
"Steering-wheel Voice button → Siri".

**Media card and launcher tile (v5.9 MED).** `NowPlayingMerge` treats now-playing as a delta stream: a
a title change does not blank artist/album/art. `CarPlayMediaBrowserService` is selectable while idle
(STATE_STOPPED, actions 0, placeholder card) and carries `com.gm.homescreen.visibility = HIDDEN` so the grid
shows one launcher icon, not two. Bench (rc1): one icon, source and metadata kept across same-artist skips.
Whether GM's own media picker honours HIDDEN is recorded as unknown in the manifest comment.

**Bench-only hooks.** `mic_inject` and `finish_launcher` exist only when `GMCCPA_BENCH=1` selects
`src/bench`; a release build compiles the `src/nobench` stub that only logs "not in this build (release)".
`mic_inject` is additionally gated by `debug.gmccpa.mic_inject` (`bench/BenchHooks.kt`). Runbook:
`06_BRINGUP_RUNBOOK.md` §4.

---

## 5. Non-goals and consequences of wireless-only

Stated explicitly because the upstream `ocbm` project supports a wired path, and a good deal of
its machinery exists only to serve it:

- **No wired CarPlay.** `iap2d` never runs.
- **No wired/wireless arbitration.** `hot_handover`, `wireless_owns_session()`, the
  `/tmp/carplay_transport` ownership token, and the arbiter are all machinery for a question this design
  does not have. (Caveat: that flag file is written as a side effect of spawning the A/V layer, so a future
  `AV_DISABLED` gate would silently change what the box supervisor believes.)
- **MFi chip contention largely evaporates.** Upstream has five chip-user code paths sharing one lock.
  Here there are two users — the box's BT-side iAP2 auth (local i²c) and the app's `CH_MFI` relay — and
  the protocol flow sequences their three touch points (Phase 2, then Phase 5, then Phase 6). Overlap is
  possible only if Bluetooth re-establishes during a live session.
- **The wired/wireless identity split is moot.** There is one identity path. The requirement that the
  BT-presented deviceID equal the app's Bonjour deviceID (Phase 4) still stands; the app advertises a pinned
  constant (`CarPlayRx.BOX_DEVICE_ID`), and the box's `box_identity` enumerates the WLAN interface rather
  than assuming `wlan0`.
- **Bluetooth is the single point of failure.** With no cable fallback, a BT drop is total session
  loss — and by then the phone is on the hotspot and streaming. Treat BT bring-up reliability as the top
  risk.

---

## 6. Design decisions

Rationale for how the app is built, as implemented.

**Rust protocol core + Kotlin shell, split at the protocol/platform line.** Kotlin owns what needs Android
framework APIs: USB/OCBM client, Wi-Fi sockets, `MediaCodec` → `Surface` (`HevcRenderer`, HEVC only),
`AudioTrack`/car audio, `NsdManager` + `MdnsResponder`, touch, platform `MediaSession`, plain views (no
Compose, no Media3). Rust (JNI, `native/carplay-jni`) owns the tested protocol code: RTSP state machine,
pair-setup/pair-verify, MFi-SAP `auth-setup`, stream framing, iAP2 TLV, and the AVCC → Annex-B conversion
(`receiver/src/forward.rs`). The core is six vendored crates (`receiver`, `pairing`, `rtsp`, `mfi`,
`metadata`, `iap2-core`, about 29k lines of Rust); reimplementing them in Kotlin would discard that tested
code. Feasibility is device-proven: they cross-compile cleanly (no async runtime, `#![forbid(unsafe_code)]`
in `receiver`, pure-Rust crypto dependencies) and the linked `.so` runs on the head unit.

**ABI.** The head unit reports `x86_64`; `tools/build_apk.sh` builds `x86_64-linux-android` and packages
`lib/x86_64/libcarplayjni.so`. The adapter side is armv7 musl.

**Sans-IO core.** `receiver::server::ControlServer<'a, P: Pairings, S: MfiSigner>` is a transport-free RTSP
state machine; the socket is driven from outside by `receiver::net::serve_connection`, so the app supplies
the transport. The `'a` borrow means the server cannot be held as a long-lived native handle directly; the
JNI layer holds the identity as `&'static` through a `OnceLock`.

**MFi through a trait, relayed over OCBM.** `MfiSigner` (`crates/vendor/mfi/src/auth_client.rs`) has two
methods, `copy_certificate()` and `create_signature(digest)`. The app runs the core with `RemoteMfiSigner`
(`native/carplay-jni/src/lib.rs`), which calls back into Kotlin and relays both over `CH_MFI`; the box side
needs nothing new (`ocbmd`'s `handle_mfi` does the I²C, takes the shared `/tmp/carplay_mfi.lock`, and
advertises `CAP_MFI` in `HELLO_ACK`). The trait is the only shape.

**`iap_tunnel` is not gated out; its chip calls are routed through the signer.** `receiver` must not depend
unconditionally on the box-only `mfi-i2c-local` (direct `/dev/i2c` access at the tunnel's cert and sign call
sites), which compiles for Android but faults at runtime. `iap_tunnel` carries NowPlaying, RouteGuidance and
call state after the handoff, so gating it off would delete the metadata and controls plane. Instead the
tunnel takes a signer (`iap_tunnel::set_remote_signer`, installed by
`Java_zeno_gmccpa_pair_NativeCore_nativeInit`) and `mfi-i2c-local` sits behind the `local-mfi` feature, off
in the JNI build. Consequence: the app has two `CH_MFI` consumers, `auth-setup` and the tunnel's own
Identify, and `RemoteMfiSigner` must stay wired for the whole session, not torn down after pairing.

**`eld-codec`.** libfdk-aac is ON in the JNI build as the mic-uplink AAC-ELD encoder
(`mic-uplink-eld`); `build_apk.sh` requires it. Downlink audio decode uses `MediaCodec`.

**MFi latency.** `/auth-setup` measures 1.7-1.9 s end to end on hardware (reference session in
`12_OBSERVED_FLOW.md`), essentially all of it the chip's signature poll, against a 10 s phone-side request
timeout; a USB round trip of a few hundred bytes is noise. The poll is bounded at 2.5 s by a wall-clock
deadline in `ocbmd`'s own I2C driver (`SIGN_POLL_DEADLINE`, `Mfi::sign()` in `box/ocbmd/src/main.rs`; no
in-box retry on this path). Budgets and retry rules: `05_SESSION_FLOW.md` §10. The device-proven
relay returns a 945-byte certificate and a 128-byte RSA-1024 signature to an ordinary app uid.

**JNI cost.** Crossings are coarse (per network buffer or frame, not per byte or sample), and the
throughput-critical work is the same either way: decode is fixed-function silicon via `MediaCodec`, render is
the HWC `Surface`, audio is `AudioTrack`. Rust threads attach to the JVM with `attach_current_thread` and
byte arrays are copied across the boundary (`RemoteMfiSigner::call`, `byte_array_from_slice`); the core does
not use direct `ByteBuffer`s. The real costs of the Rust path are build packaging, cross-boundary debugging
and JNI reference discipline, not runtime.

**Provenance.** The Rust core and `crates/ocbm-proto` come from this repo. `host/MacHost` is the OCBM-client
template for the Kotlin client. `carlink_native_personal` (stock-protocol Kotlin app, outside the repo) is a
reference only: `HevcRenderer` is structured after its `H264Renderer` (MediaCodec sync mode), `LogCapture`
is ported from its `FileLogManager`, and `AacPlayer`'s buffer value is its setting; its stock-Carlinkit
USB/protocol layer does not apply and its Compose/Media3 UI is not used (plain views, platform
`MediaSession`). Written new here: the OCBM client, the AAC/AAC-ELD stages, the HEVC-only renderer and the
AVCC → Annex-B conversion (in Rust). Apple protocol references (CarPlay Simulator, SDK) are indexed in `docs/ops/03_REFERENCE_INDEX.md`.

**Transport seam.** The OCBM client (`OcbmClient`) is written against `RawBulkTransport`
(`ocbm/Transport.kt`: `writeBulk`, `setReadHandler`, `start`, `stop`; no platform types), implemented by
`UsbBulkTransport`. It mirrors the macOS client (`host/MacHost/carlink_macOS/OCBM`: `OCBMClient.swift`,
`OCBMFraming.swift`) and shares the canonical `ocbm-proto` codec with the box and `host/ocbm-host` (the CLI
instrument), so it cannot drift from the box's framing.

**RECORD is the real gate for any receiver change.** A session is up when RECORD is answered, the event
channel is accepted, `sessionStarted` is set, and the session survives 60 seconds without
`Endpoint_Failed`. Negotiating a feature the app does not back makes the phone deactivate in ~21 ms, so only
negotiate what is implemented. Streams are SETUP throughout the session, not batched (130 DataStream with a
non-zero `streamID`, 110 screen, then 100/101/102 audio). Ordering rules that kill sessions without a
diagnostic: `05_SESSION_FLOW.md` §8 (ten observed hard-failure rules).
