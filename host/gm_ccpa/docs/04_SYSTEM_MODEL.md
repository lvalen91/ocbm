# 04 — System model: what this project builds

**Canonical.** Where another doc in this repo disagrees on *what the system is*, this one is right.
Reading order: this file (what it is) → [`05_SESSION_FLOW.md`](05_SESSION_FLOW.md) (wire-level ordering,
authoritative on session sequencing) → [`01_FINDINGS.md`](01_FINDINGS.md) (proven on hardware) →
[`03_BUILD_PLAN.md`](03_BUILD_PLAN.md) (how to build) → [`00_HANDOFF.md`](00_HANDOFF.md) (current state).

---

## 1. What this is

A **wireless-only** CarPlay receiver running as an **unprivileged Android app** on a 2024 Silverado
head unit (GM Info 3.7 / `gminfo37`, Android 12 / API 32, x86_64). The iPhone streams CarPlay to the
app **over the vehicle's own 5 GHz hotspot** (`myChevrolet 32D4`, bridge `br0`, head unit at
`192.168.5.1`). No cable ever connects the iPhone to anything in this design.

A Carlinkit **CPC200-CCPA** adapter, on USB and speaking a custom protocol (**OCBM**), does exactly two
jobs: it is the phone's **Bluetooth radio**, and it is the **MFi coprocessor**. It is not the AirPlay
endpoint, it is not an access point, and no media ever crosses it.

The app installs under its own package name, `zeno.gmccpa` — the GM USB fixed-handler squat
(`android.car.usb.handler`) was reverted (2026-09-08) — TRUCK-VERIFIED. There is no silent USB-
permission grant: the app goes through the platform's normal USB-attach permission flow, one-time
dialog included (`netprobe_app/app/src/main/AndroidManifest.xml:19`).

---

## 2. Division of responsibility

| Component | Owns | Explicitly does NOT |
|---|---|---|
| **Head-unit app** (Kotlin shell + JNI'd Rust core) | OCBM link and session lifecycle; AirPlay/CarPlay receiver identity and protocol; RTSP/pair-verify/auth-setup; video + audio decode; touch/HID; UI; supplying the vehicle hotspot credentials | Touch the MFi chip (SELinux-blocked); read the SoftAP passphrase (privilege-blocked — the driver types it in) |
| **CCPA adapter** (over USB/OCBM) | Its own Bluetooth radio: HCI bring-up, SSP pairing, SDP, RFCOMM, the iAP2 accessory handshake, accessory-initiated reconnect. The MFi coprocessor, served locally (BT-side iAP2 auth) and remotely to the app over `CH_MFI` | Raise its own SoftAP (`wifi_ap:false`, enforced by the box supervisor — see §3 Phase 0); carry video, audio, metadata, or input |
| **Vehicle** | The 5 GHz SoftAP on `br0` the app already lives on; the GM Bluetooth stack, which is bypassed entirely because the CCPA uses its own radio | — |

The app runs **on the AP host**, so client isolation never applies to it — see
[`01_FINDINGS.md`](01_FINDINGS.md) §2.

**Box-side `airplayd`/`rx-connect` are not gated off, and that is deliberate for now.** With
`wifi_ap:false` there is no `wlan0`, so nothing on the vehicle subnet can reach them; they start, latch
"A/V layer up," and are otherwise inert. An explicit `AV_DISABLED` gate that stops them from spawning at
all does not exist yet (`ccpa_custom/tools/session_supervisor.sh:769`); pointing their launch env at a
no-op binary was tried and rejected because the health-check's `pid_alive` requires `argv[0]` to equal
the full binary path, which no shell script or immediately-exiting binary satisfies
(`session_supervisor.sh:760-764`).

---

## 3. Session flow

```
                    ┌─ BT (CCPA's OWN radio, not the vehicle's) ─┐
   iPhone ──────────┤                                             ├─ CCPA ─USB/OCBM─ HEAD-UNIT APP
          └─ 5 GHz WiFi (vehicle hotspot, br0) ─────────────────────────────────────► (AirPlay endpoint)
```

### Phase 0 — the app claims the box

The app claims USB `0x1314:0x2d00` (interface 0, bulk **IN `0x81` / OUT `0x01`**, 512-byte max packet;
hardware-verified 2026-08-12 — `netprobe_app/.../ocbm/UsbBulkTransport.kt:19`). No AOA control handshake;
it is a raw byte pipe. Then `CT_HELLO` → `CT_HELLO_ACK`, `CT_SETTIME` (the box has no RTC battery),
`CT_SUBSCRIBE` with the session config — including `wifi_ap: false` (`OcbmProbe.kt:84`) — and a 1 Hz
`CT_HEARTBEAT`.

**The app is the ignition.** The box's radios are off at boot; `ocbmd` mirrors host presence to
`/tmp/host_present` on the `CT_SUBSCRIBE` edge, and the box's `wireless_up()` brings the radios up from
there, reading `wifi_ap:false` out of that same config to suppress its own SoftAP
(`session_supervisor.sh:789-790`: `BOX_WIFI_AP=0` → `"box SoftAP SUPPRESSED"`) while still bringing
up Bluetooth (`radio_hal.sh bt_on`, unconditional). If the app is not running and subscribed, no session
can start. (Practical consequence: give the app a `device_filter.xml` +
`ACTION_USB_DEVICE_ATTACHED` filter on `0x1314:0x2d00`, so plugging in the adapter *is* the trigger.)

The box's `CT_BOX_HEALTH` (`0x1A`) report carries per-subsystem readiness bits, including
`BH_HCI_PRESENT` (bit 0), which the box sets from `HCIGETDEVINFO` on a raw HCI socket — the same ioctl
`hciconfig` uses to print `UP RUNNING` (ioctl def `ccpa_custom/ccpa/ocbmd/src/main.rs:1147-1204`, setter
at `:2156`; corrected 2026-09-09, previously cited `:789-839`, which is `LogTail`/`CH_LOG` rotation code).
This is the
signal the app's lifecycle layer (§4b) uses to know the box's radios actually came up, not just that
`ocbmd` answered.

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
with the **vehicle's** hotspot credentials — SSID, passphrase, security type, channel — supplied by the
app over OCBM, not read from the box's own `hostapd.conf`. The box's AP never comes up. The iPhone
leaves Bluetooth-only and joins `myChevrolet 32D4` on `br0`. There is no join confirmation over iAP2 — a
successful `0x5703` write is the only signal, and iOS retries `0x5702` two or three times as a matter of
course, so the handler must be idempotent.

The passphrase cannot be read programmatically by any app on this head unit; the driver enters it once
from the OS hotspot GUI (`com.gm.hmi.connection`'s `WifiHotspotActivity` — keep that package).

### Phase 4 — the app becomes the accessory on Wi-Fi

The app advertises `_airplay._tcp` on `br0` via **two adverts run concurrently, deliberately, not one**
*(corrected 2026-09-09; this entry previously named `NsdManager` as the sole mechanism)*:
`NsdManager.registerService` (primary — the one iOS has demonstrably indexed) **and** a hand-rolled,
self-hosted `MdnsResponder` run alongside it on purpose (`CarPlayRx.kt:372-445`, "ALONGSIDE NsdManager"
rationale at `CarPlayRx.kt:429`). The reason the second one exists: AOSP drops `NsdServiceInfo.setHost()`
on registration, so `NsdManager` cannot set the SRV target, and it shares GM's `Android.local` host,
whose Bonjour record wins the endpoint-index race whenever GM registers first (`MdnsResponder.kt:16-26`).
`MdnsResponder` answers mDNS itself, owns `gmccpa-rx.local`, and publishes exactly one address (`br0`),
pinning a second, separately-keyed endpoint that carries the app's own deviceID — using a deviceID
consistent with what the box presented over Bluetooth, on its own port (GM's own CarPlay service holds
`:7000` and must not be disturbed).

**Discovery is bidirectional.** The accessory also *browses* `_carplay-ctrl._tcp` and dials the phone
with `GET /ctrl-int/1/connect`; the phone then opens the **control** connection inbound
(`CarPlayRx.kt:32,148,289,325`). The app never dials the phone for the control channel itself, but it
does have to make that outbound nudge. Two TXT values are load-bearing: `features` must carry the
**Car** bit (high word bit 32) or iOS answers the connect-out and never opens RTSP; `features` /
`deviceid` / `pi` must all equal what `/info` returns or pair-verify fails. See
[`05_SESSION_FLOW.md`](05_SESSION_FLOW.md) §3.

From here the app and the iPhone speak the Apple protocols directly over Wi-Fi: RTSP/HTTP control,
pair-verify (pure Curve25519/Ed25519, no chip), MFi-SAP `auth-setup`, per-stream setup, then H.264/HEVC
video and AAC/AAC-ELD audio, HID input uplink, mic uplink, and iAP2 over the AirPlay DataStream for
metadata and controls.

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
| **USB / OCBM** (`0x1314:0x2d00`) | `CH_CTRL` (handshake, heartbeat, session events, pairing code), `CH_MFI` (four chip ops/session), `CH_MGMT` (health, identity, forget-bond, reboot), `CH_LOG` (the box's own logs, PUSHED continuously into app logcat as `[box:<source>]` lines, armed by `CT_LOG_CTL` — `OcbmClient.handleLog`), `CH_FILE` (one-shot `FILE_PULL` of the few logs `CH_LOG` does not carry). Kilobytes. | Video, audio, metadata, input, mic — ever |
| **5 GHz Wi-Fi** (`br0`) | Everything heavy: video, audio, RTSP control, pair-verify, auth-setup, HID, mic, iAP2 DataStream | — |

**Why media doesn't ride USB:** not primarily bandwidth — `ccpa_custom/docs/carplay/00_ARCHITECTURE.md` measures
339 Mbps adapter→host and 90 Mbps host→adapter, both well above CarPlay's 8–30 Mbps video need in
either direction. The real constraint is §4a below: the box has no network route to `br0`, and the
owner's requirement is that the phone join the *vehicle's* SoftAP, not the adapter's.

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

> **No per-channel SUBSCRIBE in OCBM** — a host cannot ask the box to stop sending a channel.
> `CH_VIDEO`/`CH_MEDIA_AUDIO`/`CH_ALT_VIDEO`/`CH_METADATA`/`CH_INPUT`/`CH_MIC` stay silent here because
> the box never spawns the A/V layer in this role, not because the app opted out — a box-side role
> change (`wifi_ap:false` + BT-only bridge config), not a protocol negotiation.

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
  `onBoxHealth` and `onDialAccepted` already did. Device-observed 2026-09-08 (`CarPlay live -> pairing`
  at 900 rendered frames); fixed the same day.
- **Losing the box mid-session must not end the session.** The A/V path is Wi-Fi and does not depend on
  Bluetooth or on the box once streaming. On the box's return the app re-establishes the link *without*
  `CT_SUBSCRIBE` (`OcbmProbe.runAll(subscribe = false)`) and asserts `CT_RADIO` off, so the MFi relay
  comes back — `/auth-setup` is per control connection, not per pairing — while the box's radios stay
  down under the live session.
- **`OcbmProbe.runAll()` returns a `LinkResult`, and callers must gate on it.** It used to return `Unit`;
  the launcher then announced "box claimed, MFi proven" with no adapter on the bus at all
  (`OcbmProbe.kt:225-247`, device-observed 2026-08-28). `LinkResult` carries `claimed`, `helloOk`,
  `mfiProven`, `subscribed` as explicit fields.
- **`CarPlayActivity.onSessionEnded()` tears the screen down on session end** — the LIVE screen does not
  linger after the phone disconnects (`CarPlayActivity.kt:77`).
- **A readiness probe must not perturb what it measures.** `CarPlayRx.selfTest()` deliberately does not
  dial `:7011` on loopback — that would enter `acceptLoop`, take a liveness slot, spin up a native core
  and fire a spurious session-down on close. Holding the bound, open `ServerSocket` is the capability
  that matters and is directly observable.

### The recovery ladder

| Rung | Action | Cooldown | Touches `host_present`? |
|---|---|---|---|
| 0 | re-announce mDNS | 30 s | no |
| 1 | `CT_RADIO` off → on | 90 s | **no** — this is the point of it |
| 2 | `MGMT_RESTART_WIRELESS` | 180 s | no |
| 3 | terminal: tell the driver to reconnect Bluetooth from the iPhone | — | — |

(`SessionSupervisor.kt`.) The ladder re-enters itself on a timer; only a real session resets it — but
since 2026-09-08 it stands that timer DOWN while a handshake is in flight (see the refinement below).
Cooldowns are correctness, not politeness — see §4c. The "Recover" button runs the same ladder from
rung 0 ignoring cooldowns, because those exist to stop the machine hammering the box unattended and a
human request is not that.

**Rung 3 is not an action, and that is deliberate.** A full manual reset — box power-cycled, app
relaunched, receiver re-bound, Bluetooth re-paired, Wi-Fi re-joined through the proper CarPlay prompt —
does not always recover a session where iOS reads `_airplay._tcp` TXT and issues no SRV query. Nothing
reachable from the head unit clears that; an app that keeps silently retrying what cannot work is worse
than one that says so.

> **Refinement (2026-09-08) — the ladder must not fire into live progress.** `escalate()` arms a blind
> `LADDER_RECHECK_MS` (20 s) retry so a silent phone still escalates; nothing external calls back when
> nothing is happening. But that retry carried the ORIGINAL `why` and fired on schedule regardless of
> what the box and phone had achieved since. Device-observed twice on 2026-09-08, the second time
> shooting a handshake in the back mid-authentication:
>
> ```
> 12:07:31.778  rung 1 (CT_RADIO off/on)
> 12:07:50.160  BT_PHASE LINK_UP        -> pairing
> 12:07:51.150  BT_PHASE AUTHENTICATING
> 12:07:51.779  rung 2 (MGMT_RESTART_WIRELESS)   <- 20.001 s after rung 1
> 12:07:52.296  BT_PHASE WIFI_HANDOFF / PROJ_MODE WIRELESS_CP
> 12:07:53.284  BT_PHASE IDLE / PROJ_MODE NONE   <- collapsed
> 12:07:55.659  BOX_HEALTH 0x43                  <- airplayd gone
> ```
>
> Rung 2 is the one rung that tears down the box's whole wireless stack, `airplayd` included. After it
> fired, the phone 200-OK'd every `/ctrl-int/1/connect` nudge forever and never dialled back — the
> receiver it was being pointed at no longer existed. iOS records this as `EAAccessoryLeft`. Diagnosing
> it from the head-unit side alone reads as failure point FP5 ("phone reads TXT, never SRV-queries us")
> and sends you hunting a phone-side fault that is not there.
>
> `deferLadder()` now cancels the blind retry on forward progress — `BTP_LINK_UP`/`AUTHENTICATING`,
> `BTP_IDENTIFIED`, `BTP_WIFI_HANDOFF`, and `onDialAccepted`. It deliberately does NOT reset `rung`:
> ladder position and cooldowns are earned state, and a phone that stalls again must resume where it
> left off. It only yields the CLOCK to the narrower deadline the new phase just armed (45 s handoff
> watchdog, or 15 s dial-back), each of which re-enters `escalate()` on expiry — so a genuinely stuck
> phone still escalates, just never on top of a live handshake. Same rule `onDialAccepted` already
> applied to `handoffTimer`: once a narrower clock owns the deadline, the broader one lets go.
>
> Corollary worth keeping: the session that finally succeeded that day did so **because the ladder had
> exhausted into STALLED**, which stops arming the retry. Recovery machinery giving up was the
> precondition for recovery. If a session only connects after the ladder is spent, suspect the ladder.

> **Refinement (2026-09-08).** Rung 3's wording assumed the driver must tap because the box could not
> page. §4c.4 now records that it can. That does not promote rung 3 to an action: the failure it
> exists for is a bond mismatch, and no rung the app can reach repairs a key the *phone* has thrown
> away. If rung 3 ever grows a body, the right one is to name the specific remedy — forget on both
> sides and re-pair — rather than to retry the reconnect the box is already retrying on its own.

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

Both rects are STATIC constants for this panel. Nothing is measured, awaited or renegotiated at
runtime — this app targets one radio and one display. (`carlink_native` does detect insets and wait
for them to settle, because it is a universal app that must discover its display at launch. Do not
copy that here; it is solving a problem this project does not have.)

### How the resize works

`/info` declares two `displays[0].viewAreas[]` entries with `adjacentViewAreas` DERIVED from
`initialViewArea`, and the `viewAreas` feature is negotiated in the SETUP `enabledFeatures` (the lever
at `lib.rs`; without it iOS ignores the whole structure). iOS then draws the Dock resize button and
sends `/command requestViewArea`, which reaches the app as a `META_CMD` plist on the `:9004` seam; the
receiver answers `updateViewArea` with `animationDurationMillis`.

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

4. **The box CAN page the phone, but only against a two-sided bond.** *(Corrected 2026-09-08; this
   entry previously said outbound paging fails every time with `CONNECT_FAILED status=0x07`.)* Current
   box code runs a `reconnect` loop that walks `/etc/carplay/bt_link_keys`, SDP-queries the phone and
   dials iAP2 RFCOMM outbound. Device-observed twice on 2026-09-08: the box paged the phone, brought
   up L2CAP, read the phone's `Wireless iAP v2` record and resolved RFCOMM channel 1 — so the paging
   itself works.

   What it cannot survive is a **one-sided bond**. When the box holds a link key the phone has
   discarded, the authenticated RFCOMM connect times out after ~8 s
   (`[reconnect] RFCOMM connect to channel 1 failed: connect timed out`), and iOS reports the mirror
   image — `keys available ? No`, `retrieve key chain magic key data ... failed with result 150`, then
   `Connection to device ... failed - result was 431` on the *Wireless IAP* profile. The phone shows
   the accessory as paired while holding no key for it.

   The practical rule is unchanged in effect but different in cause: a reconnect that will not
   complete is a **bond mismatch**, not an inability to page. The fix is to clear BOTH sides —
   `MGMT_FORGET_ALL` on the box (`--es run ocbm_forget`) and Forget This Device on the iPhone — and
   re-pair. Clearing only one leaves the same half-bond in the other direction. See the three
   independent pairing stores in `evidence/session_2026-08-12-freshpair/README.md`.

---

## 5. Non-goals and consequences of wireless-only

Stated explicitly because the upstream `ccpa_custom` project supports a wired path, and a good deal of
its machinery exists only to serve it:

- **No wired CarPlay.** `iap2d` never runs.
- **No wired/wireless arbitration.** `hot_handover`, `wireless_owns_session()`, the
  `/tmp/carplay_transport` ownership token, and the arbiter are all machinery for a question this design
  does not have. (Caveat: that flag file is written as a side effect of spawning the A/V layer, so
  skipping the spawn silently changes what the box supervisor believes.)
- **MFi chip contention largely evaporates.** Upstream has five chip-user code paths sharing one lock.
  Here there are two users — the box's BT-side iAP2 auth (local i²c) and the app's `CH_MFI` relay — and
  the protocol flow sequences their three touch points (Phase 2, then Phase 5, then Phase 6). Overlap is
  possible only if Bluetooth re-establishes during a live session.
- **The wired/wireless identity split is moot.** There is one identity path. The requirement that the
  BT-presented deviceID equal the app's Bonjour deviceID (Phase 4) still stands.
- **Bluetooth is the single point of failure.** With no cable fallback, a BT drop is total session
  loss — and by then the phone is on the hotspot and streaming. Treat BT bring-up reliability as the top
  risk.
