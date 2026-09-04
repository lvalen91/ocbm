# Wireless CarPlay — session, handoff and the AirPlay tunnel

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 20, 39, 30, 38, 29, 36, 37; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

**Contents:** the wireless research baseline → session verification → the metadata tunnel → the iAP2/AirPlay-tunnel handshake → Identify experiment results. The wireless *metadata* content lives in carplay/05.

## Wireless CarPlay — baseline research

<!-- absorbed: ../wireless/00_WIRELESS_CARPLAY.md -->

Research pass answering: *what does the CCPA box already have for Wireless CarPlay, and what must
be added?* Sources: live UART inventory of the box; the iAP2 spec archive + CarPlaySDK protocol reference notes
(mempalace `carplay_receiver`); WWDC transcripts (2016-722/723, 2017-717). Everything below is
ground-truth-verified, not assumed.

> **⚠️ SHIPPED — this began as research and is now largely BUILT**, device-verified through the A3
> BT→WiFi handoff. The §3 design is what was built and the §4 gap analysis is largely closed. **For the
> current state, deployed-vs-pending binaries, remaining work, gotchas and the exact test procedure, see
> the most recent `docs/SESSION_HANDOFF_*.md` (there is no root `HANDOFF.md`).** Full status text:
> [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-20W-1`.

---

### 0. TL;DR

The box carries a **complete but dormant Wireless-CarPlay hardware + firmware + script stack** (NXP
IW416 WiFi+BT combo). The radios, drivers, firmware, AP config, BT stack, and on-demand bring-up
scripts all exist and were deliberately preserved when the stock projection code was stripped. What
does **not** exist is the **orchestration + protocol glue**: the iAP2 wireless-CarPlay message
exchange (WiFi-credential handoff), discovery on `wlan0`, and pointing the (transport-transparent)
`airplayd`/`receiver` stack at the WiFi link instead of `ncm0`.

Because AirPlay is transport-transparent and the **first-time pairing can bootstrap over the USB link
we already run**, the cheapest path to a working wireless session reuses almost the entire existing
wired stack.

---

### 1. What exists on the CCPA (inventory results)

#### 1.1 Hardware
- **SoC:** Freescale i.MX6 UltraLite (armv7l), kernel **3.14.52** (`cfg80211` + `mac80211`
  built-in — the nl80211 wireless stack is present).
- **Combo radio: NXP IW416** (802.11n dual-band WiFi + Bluetooth 5.x).
  - **WiFi:** SDIO, enumerated at `mmc0:0001:1`, `SDIO_ID=02DF:9159` (0x02DF = NXP/Marvell). The
    silicon is powered and on the bus — **only the driver is unbound** (no `wlan0`).
  - **BT:** UART on `/dev/ttymxc2` @ 3 Mbaud (firmware loaded via `fw_loader_linux`, attached via
    `hciattach`).

#### 1.2 Firmware & driver (staged in `/lib/firmware/nxp`, not loaded)
| File | Role |
|---|---|
| `sdioiw416_wlan_v0.bin` | SDIO WiFi firmware |
| `uartiw416_bt_v0.bin` | UART BT firmware |
| `iw416_ko.tar.gz` | kernel modules: **`mlan.ko`, `moal.ko`** (NXP WiFi driver) + **`hci_uart.ko`** (BT line discipline) — staged, never extracted/inserted |
| `WlanCalData_ext.conf`, `txpower_iw416.bin` | calibration / TX power |
| `wifi_mod_para.conf` | `drv_mode=3` → **STA + uAP** (`wlan0` AP + `sta0`) |
| `wifi_mod_para_p2psta.conf` | `drv_mode=5` → **STA + WiFi-Direct/P2P** (the P2P/GO mode wireless CarPlay can use) |

#### 1.3 Stock binaries present (`/usr/sbin`, `/usr/bin`)
`bluetoothDaemon` (proprietary Carlinkit BT stack — BT pairing + **iAP2-over-BT** + HFP), `hfpd`,
`hcid`, `hostapd`, `fw_loader_linux`, `set_wifi_mac`, `hciattach`/`hciconfig`/`hcitool`.
> `bluetoothDaemon` is the closed "old firmware code" that historically drove the BT side + the
> BT→WiFi handoff. It is **not** part of our Rust binaries.

#### 1.4 Bring-up scripts (owned rewrites, functional, on-demand)

> **SUPERSEDED AS THE LIVE PATH (2026-08-15, docs/wireless/01_BT_AND_RADIO.md).** What follows describes the IW416
> baseline's own scripts and is still accurate *about those scripts*, but the supervisor no longer
> calls them: its four radio call sites now resolve through the chipset-neutral seam,
> `sh /script/radio_hal.sh {wifi_ap_on,bt_on,...}`. Read this section as the IW416 mapping the seam
> resolves TO, not as the sequence that runs.
>
> This distinction is not academic — it cost a full debugging session on 2026-08-28. Bluetooth was
> dead, this section was read as current, and `attach_bluetooth.sh` was patched. The patch was
> correct and changed nothing, because nothing calls it. See ../ops/06_CORRECTIONS_LEDGER.md R-20W-5.
- **`wlan_on.sh`** → extract `iw416_ko.tar.gz` → `insmod mlan.ko` + `insmod moal.ko
  mod_para=nxp/wifi_mod_para.conf` → `wlan0` → `start_bluetooth_wifi.sh`.
- **`start_bluetooth_wifi.sh`** → `ifconfig wlan0 192.168.43.1` → `hostapd /etc/hostapd.conf -B` →
  `udhcpd` (DHCP server). Box is the **WiFi AP**.
- **`bt_on.sh`** → **`attach_bluetooth.sh`** → `insmod hci_uart.ko` → `fw_loader_linux …
  uartiw416_bt_v0.bin` → `hciattach … 3000000 flow` → `hci0` (BT MAC 38:BA:B0…, HFP SCO, BLE) →
  `bluetoothDaemon -n`.
- `wlan_off.sh` / `bt_off.sh` release them.
> These are the *"scripts post old-code-removal that verified WLAN/BT still function"*: after the
> stock projection stack was removed, the radios were re-validated behind these clean bring-up paths.

#### 1.5 Config (`/etc`)
- **`hostapd.conf`** — `wpa=3` (WPA2/WPA3-PSK), `ssid=ccpa-b0df`, `wpa_passphrase=12345678`,
  `wpa_key_mgmt=WPA-PSK`, CCMP, `driver=nl80211`, `hw_mode=a channel=36` (5 GHz), `ieee80211n=1`.
- **`udhcpd.conf`** — DHCP **server** (box hands the iPhone an IP over WiFi → confirms AP topology,
  pool `192.168.43.100–.200`).
- **`wpa_supplicant.conf`** (STA mode, unused in AP topology), `bluetooth`, `bluetooth_name`,
  `wifi_name`.
- **`/etc/carplay_peers.bin`** (+ `.bak`) — paired-peer persistence (wireless auto-reconnect store).

#### 1.6 Current operating mode
- Boot modes are flag-gated in `/script`: `ncm_only` (wired, **no WiFi**), `ncm_wifi` (wired + WiFi
  AP backstop), or none = **default "OCBM accessory appliance."**
- **Neither flag is set** → box is in the default appliance mode: `start_main_service.sh` says *"all
  projection/vendor launches removed. Radios are on-demand (wlan_on/bt_on); NCM always-on … radios
  OFF at boot."*
- Runtime confirms it: only `ocbmd`, `iap2d` (`/dev/android_iap2`, **USB** iAP2), `airplayd` (over
  `ncm0`) run. **No `bluetoothDaemon`, `hostapd`, or `wpa_supplicant`.**
- `session_supervisor.sh` orchestrated only the wired path at the time of this survey
  (`host present → projection_up.sh (iAP2 → Identified) → airplayd + rx_connect`) with zero wireless
  references. **It is dual-transport today** — that survey line is the historical starting point, not
  the current shape.

**Bottom line:** the radios are a bring-up script away from alive; the *project* simply never wires
them into a CarPlay session.

---

### 2. How Wireless CarPlay actually works (authoritative)

#### 2.1 The session is transport-transparent
CarPlaySDK protocol reference notes (mempalace): `NetworkTransportType` is a bitmask — `Enet/WiFi/WFD/AWDL/USB/Direct/
BTLE/NAN/IPsecBT/IPSecWiFi`. *"CarPlay live = USB (NCM) + WiFi. TRANSPARENT to callers (same
machinery, only label differs)."* → The pairing, `/info`, SETUP, RTP/screen streams are **identical**
over WiFi; only the IP transport underneath changes. **`airplayd`/`receiver` are reusable as-is,
bound to `wlan0` instead of `ncm0`.**

#### 2.2 Bluetooth is the bootstrap channel
Wireless CarPlay starts over **Bluetooth**, carrying **iAP2**. Over that iAP2 link the phone and
accessory negotiate CarPlay and the WiFi hand-off. Authoritative iAP2 messages (from
`iap2messages-external.i2mspecarchive` — **path recorded 2026-08-16**, per docs/ops/03_REFERENCE_INDEX.md standing rule 1: the
`-external` archive ships ONLY in the standalone Simulator, at `~/Downloads/Carplay WWDC/Hardware/CarPlay Simulator.app/Contents/Frameworks/iAP2MessageKit.framework/Versions/A/Resources/`
(mirrored under `~/Documents/carlink/carplay_simulator/CarPlay Simulator.app/…`); Xcode's
CarPlaySimulator plugin carries only `-internal`. Decode with `tools/i2mspec_dump.py --archive <path>`,
never by inference — docs/ops/03_REFERENCE_INDEX.md §C):

| Message / param | Purpose |
|---|---|
| `CarPlayStartSession` / `CarPlayStartSessionWiFiSecurityType` | begin a CarPlay session, declare WiFi security |
| `WirelessCarPlayTransportComponent` | negotiate transport components (WiFi/…); error if unsupported |
| `RequestAccessoryWiFiConfigurationInformation` | phone asks the accessory for its WiFi config |
| `AccessoryWiFiConfigurationInformation` | accessory returns **SSID + `WiFiPassphrase` + `AccessoryWiFiConfigurationSecurityType`** |
| `BluetoothConnectionUpdate` | BT link state |
| `StartExternalAccessoryProtocolSession` / `ExternalAccessoryProtocolCarPlay` | EAP session for the CarPlay control protocol |
| `CarPlayAvailability`, `CarPlaySDKVersion`, `USBHostTransportCarPlayInterfaceNumber` | capability advertisement |

Flow: **BT connect → iAP2 identify → `CarPlayStartSession` + `WirelessCarPlayTransportComponent` →
`RequestAccessoryWiFiConfigurationInformation`/`AccessoryWiFiConfigurationInformation` (creds) →
phone joins the AP → AirPlay runs over WiFi.**

#### 2.3 Discovery after WiFi association
- **Older iOS:** accessory advertises Bonjour `_airplay._tcp` (+ `_carplay-ctrl._tcp`) on `wlan0`;
  phone discovers and connects.
- **2023+ (simplified):** RE corpus — *"noBonjour — IP+port over iAP2 (Enabling iAP Channel
  support), Bonjour skipped … WPA3 enabled by consequence (removing mDNS-over-WiFi)."* The accessory
  sends its `wlan0` IP:port over the iAP2 channel and the phone connects directly. (Our AP already
  runs WPA3-capable `wpa=3`.)

#### 2.4 The two ways to start wireless CarPlay (WWDC 2017-717, authoritative)

There are **two distinct pairing/initiation methods**. Both converge on the same steady state (iAP2
link → *"Enable wireless CarPlay"* prompt → creds/link-key stored → future sessions over WiFi), but
the bootstrap channel differs.

**Method A — USB out-of-band pairing (the recommended, easiest handover; reuses our USB link):**
1. iPhone plugged in over USB → USB **role switch** → CarPlay connects **over USB** (NCM), unit
   establishes the **iAP2 link declaring support for all necessary messages**.
2. *While the USB CarPlay session is already active and streaming*, iPhone prompts the user to
   **"Enable wireless CarPlay."** The USB session is **not interrupted** by this.
3. On user confirmation, **iOS generates a Bluetooth link key and sends it over iAP (i.e. over the
   USB link, not over BT)** to the unit.
4. The head unit **stores the link key + the device transport identifiers** as a BT paired device
   set up for CarPlay, and confirms → out-of-band pairing complete. *No actual BT exchange occurs
   during this.*
5. The device is saved as the **last-connected / preferred** device. **The wireless session then
   starts automatically on the next ignition cycle** (driver returns) — **not** when the driver
   unplugs USB (she may just be leaving). → For us, this bootstrap **reuses the existing `iap2d` USB
   link**; we just add the message set + a link-key/peer store.

**Method B — BT-first pairing (sole wireless, no cable ever):**
1. Initiate on the head unit: long-press the Voice-Recognition button *or* native UI (a dedicated
   "add CarPlay device" UI, or the generic "add Bluetooth device" UI). The head unit becomes
   **discoverable** (optionally also scans).
2. The head unit **advertises CarPlay support in its Bluetooth Extended Inquiry Response (EIR)** — the
   Apple CarPlay EIR is what lets a CarPlay-specific device list filter to only wireless-CarPlay cars.
3. Phone in BT/CarPlay settings discovers the car → select → **BT Secure Simple Pairing** → **IP over
   Bluetooth (iAP2 over BT)** → head unit identifies support for the required messages.
4. **When iAP2 connects, iOS prompts "Enable wireless CarPlay."** iOS provides the **device transport
   identifiers** so the unit stores the device and can recognize it over a *different* transport on
   reconnect.
5. On user confirmation, the device **requests the WiFi access-point credentials**; the head unit
   **waits for user consent, reconfigures for wireless, then responds with the creds** → iPhone
   **joins the AP**.
6. **Bonjour discovery** runs on WiFi → head unit **initiates the CarPlay session via the CarPlay
   Control API** → CarPlay session + **iAP2-over-CarPlay** (WiFi). iAP2 is briefly connected over
   *both* BT and WiFi; then iPhone sends a **disable-Bluetooth** command and the unit drops the BT
   links to that device.

*"The CarPlay experience is the same whether plugged in or wireless. Never indicate wired vs
wireless."* (WWDC 2016-723: *"an IP-based link to the head unit either over USB or over Wi-Fi … the
same CarPlay Communication Plug-in."*)

#### 2.5 Reconnection & transport arbitration (Apple's own rules == the §3 design)
WWDC 2017-717 prescribes **exactly** the HU-parity behavior in §3:
- *"Whether CarPlay is using USB or the wireless link is completely transparent to the user and
  **depends only on how a device is connected to the car and in which order the connections
  happened**."* → **first-to-connect wins.**
- *"Once a CarPlay session is running on a device, **do not interrupt it**."* (A friend plugging in
  mid-drive must **not** preempt the running session.) → **mutual exclusion, no preemption.**
- Example scenarios: wireless-only → runs wireless; plugging USB in *after* a wireless session started
  → **stays wireless** (no interruption).
- Reconnect considers all currently-connected devices + pre-ignition state; **revert to legacy
  (BT/iPod)** if CarPlay is unavailable; restore the user's last-used screen/source.
- Store the device as the **last-connected BT device** for automatic reconnection.

#### 2.6 Hardware requirements vs. what the box has
| Requirement (WWDC 2017-717) | Box status |
|---|---|
| Bluetooth: core spec + service discovery + iAP2 + **CarPlay EIR advertisement** | IW416 BT present; EIR advertisement is **to add** |
| WiFi AP: Wi-Fi-Alliance certified, **802.11ac / 5 GHz recommended**, **Apple Device Information Element + Interworking IE** | IW416 AP present, `hostapd` already **5 GHz ch36**; the Apple IEs are **to verify/add in `hostapd.conf`** |
| **Bonjour** + comm plugin for discovery (older iOS) | **to add** on `wlan0` (or use the 2023 IP-over-iAP path, §2.3) |
| Location: GNSS + speed + dead-reckoning | **not present** — acceptable for a dev/bench bring-up; a production HU needs it |
| **BT/WiFi coexistence:** extra BT profiles only if AP is 5 GHz; if AP is 2.4 GHz, **BT off during an active session** | Our AP is **5 GHz** → BT may coexist; keep the 2.4 GHz rule if we ever drop to `wifi_use_24G` (band is an app-pushed config value per docs/carplay/04_CAPABILITIES_AND_CONFIG.md) |

---

### 3. Target architecture — HU-parity dual transport

**Design intent (per project direction):** behave like a real vehicle head unit + the original
firmware — **Wired and Wireless CarPlay both live in real time.** First transport to connect owns the
session; the other is blocked *only while a session is live*; on teardown both return to idle waiting
for any iPhone; nothing ever stalls.

#### 3.1 Topology — the Mac is the "HU head," the box arbitrates the phone side
- The **macOS host app is the HU head/display**, permanently attached to the box over **USB/OCBM**
  (the host-facing gadget). This leg never changes and is not part of the arbitration.
- The box arbitrates the **phone-side transport**: **Wired** (iPhone on the box's phone-facing USB →
  `ncm0`) vs **Wireless** (iPhone over BT-bootstrapped WiFi → `wlan0`). The box executes the
  arbitration *mechanics*; the arbitration *policy* (preference, tiebreak) is app-pushed config
  (docs/carplay/04_CAPABILITIES_AND_CONFIG.md).
- Downstream of `airplayd` the pipeline is identical for both — `airplayd (receiver) → OCBM → Mac`.
  **Wireless adds a second phone-side ingress, not a new renderer.** (This resolves the rendering-
  topology question: the Mac stays USB/OCBM-connected; only the iPhone↔box leg differs.)

#### 3.2 Two always-listening ingress agents (idle = both armed, never stalling)
- **Wired agent** (today's path): waits for a 05ac iPhone to enumerate on the phone-facing USB bus
  (`SEV_PHONE_PRESENT`) → USB iAP2 (`iap2d`) → identify → `airplayd` on `ncm0`.
- **Wireless agent**: BT advertising + WiFi AP armed → waits for an iPhone to BT-connect and run the
  iAP2 wireless-CarPlay handshake → WiFi hand-off → `airplayd` on `wlan0`.
- Both agents advertise/listen **concurrently** whenever the session is IDLE; neither blocks the
  other while idle.

> **⚠️ CORRECTED — the §3.2 text above is pre-doctrine.** "Idle = both armed" begins only *after* the
> host app has connected and pushed config; until then the box holds IDLE with radios un-armed.
> Full reasoning: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-20W-3`.

#### 3.3 Single-active-session arbiter (first-come-wins, mutual exclusion)
```
        ┌──────────────── IDLE ─────────────────┐
        │  wired agent   : armed (USB enumerate) │
        │  wireless agent: armed (BT adv + AP)   │
        └───────┬────────────────────┬───────────┘
   iPhone wired │                    │ iPhone wireless
   identified   │  (atomic session-  │ handshake done
                │   lock: 1st wins)  │
        ┌───────▼────────┐   ┌───────▼────────┐
        │ ACTIVE(wired)  │   │ ACTIVE(wifi)   │
        │ other = BLOCKED│   │ other = BLOCKED│
        └───────┬────────┘   └───────┬────────┘
   unplug/gone  │                    │ WiFi/BT drop / gone
                └────► release lock ─────► IDLE (both re-arm)
```
- **Acquire** at the *confirmed session milestone* (iAP2 identified / wireless handshake complete) —
  **not** mere physical presence — so a phone that connects but never identifies can't hold the lock.
- **Exclusion:** while `ACTIVE(x)`, the other agent stays physically listening (BT still bonded, USB
  still enumerates) but the arbiter **refuses to start a competing CarPlay session**. Only a *live*
  session blocks the other.
- **Release** on teardown of the active transport → `IDLE`, both agents re-arm; no residual block.
- **Never stall:** the lock is held only by a confirmed-live session. A hung bring-up on one agent
  times out (watchdog) and resets *that* agent without touching the other or the lock — reusing the
  supervisor's existing non-stall lifecycle (tasks #26/#30).
- **Tiebreak (near-simultaneous):** first to the atomic lock wins; optionally prefer wired (more
  reliable) if both cross in the same tick — an app-pushed config value (docs/carplay/04_CAPABILITIES_AND_CONFIG.md), not a
  box-compiled preference.

#### 3.4 Rust-native vs. slim-script split (per the "in the binaries or a slim script" ask)
- **Slim scripts — unavoidable system plumbing only, kept minimal:** raw radio bring-up — `insmod
  mlan/moal/hci_uart`, `fw_loader_linux`, `hciattach`, `hostapd`, `udhcpd`. These already exist as the
  owned `wlan_on.sh` / `bt_on.sh` / `attach_bluetooth.sh` rewrites; they touch kernel modules +
  firmware, which is inherently shell/syscall territory and not worth reimplementing in Rust.
- **Rust-native — all CarPlay-specific logic, self-contained in the project:**
  - the **dual-transport arbiter + lifecycle** — a Rust session owner holding the atomic session-lock
    and driving both agents (either replace `session_supervisor.sh` with a Rust `sessiond`, or keep a
    thin shell wrapper that calls a Rust arbiter);
  - **iAP2-over-Bluetooth** — a Rust RFCOMM/L2CAP transport (`AF_BLUETOOTH` via `libc`) that **reuses
    `iap2d`'s existing iAP2 core**, so wired and wireless iAP2 share one message engine;
  - the **wireless-CarPlay message set** (`CarPlayStartSession`, `WirelessCarPlayTransportComponent`,
    `RequestAccessoryWiFiConfigurationInformation`/`AccessoryWiFiConfigurationInformation`,
    `BluetoothConnectionUpdate`);
  - **WiFi hand-off** (serve creds from `hostapd.conf` — interim mechanics: per docs/carplay/04_CAPABILITIES_AND_CONFIG.md the
    credentials are app-pushed at init, and `hostapd.conf` is written from the pushed config) +
    **discovery** (IP:port over the iAP2 channel, or a small mDNS responder on `wlan0`);
  - **`airplayd` gains a `wlan0` bind** alongside `ncm0` (the receiver is already transport-transparent).
- **BT stack decision:** prefer a **Rust `btd`** (raw HCI socket + RFCOMM) to stay self-contained,
  rather than depending on the closed stock `bluetoothDaemon` — which remains a Phase-1
  speed/reference fallback.

**Net:** HU parity — wired and wireless both alive in real time, first-come owns the session, the
loser is blocked only while a session is live, and teardown returns cleanly to dual-idle with no stall.

---

### 4. Gap analysis — what must be ADDED

> **⚠️ CLOSED — all seven items in the table below shipped**, and the section is retained only as the
> original scoping. Also superseded: this document's "no hostapd/wpa_supplicant, zero wireless
> references in session_supervisor.sh" observation (a 2026-07-13 snapshot) and its "parked mic issue"
> lead (the mic uplink ships, device-confirmed). Where each item landed, with paths and current
> figures: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-20W-4`.

| # | Component | State today | Needed for wireless |
|---|---|---|---|
| 1 | **Radio bring-up** (WiFi AP + BT `hci0`) | scripts exist, work, on-demand | orchestrate from the supervisor (call `wlan_on.sh`/`bt_on.sh`) |
| 2 | **iAP2 wireless-CarPlay messages** | `iap2d` speaks USB iAP2 (auth/identify only) | add `CarPlayStartSession`, `WirelessCarPlayTransportComponent`, `RequestAccessoryWiFiConfigurationInformation`→`AccessoryWiFiConfigurationInformation`, `BluetoothConnectionUpdate` |
| 3 | **WiFi credential hand-off** | AP creds live in `hostapd.conf` (interim) | serve them in `AccessoryWiFiConfigurationInformation` so the phone auto-joins — creds app-pushed at init per docs/carplay/04_CAPABILITIES_AND_CONFIG.md |
| 4 | **iAP2-over-Bluetooth transport** | only USB (`/dev/android_iap2`) | BT RFCOMM/L2CAP iAP2 for BT-first pairing + reconnect (reuse stock `bluetoothDaemon`, or new Rust `btd`) |
| 5 | **Discovery on `wlan0`** | none | mDNS `_airplay._tcp` on `wlan0` **or** IP:port over the iAP2 channel (2023+ path) |
| 6 | **`airplayd` on `wlan0`** | binds/receives over `ncm0` | bind the AirPlay receiver on `192.168.43.1`; transport-transparent, mostly a bind/discovery change |
| 7 | **Supervisor orchestration** | wired-only | new wireless lifecycle: BT connect → raise AP → hand off → `airplayd` on `wlan0`; persist `carplay_peers.bin` for auto-reconnect |

Nothing here requires new hardware or firmware; items 2–7 are software the project must author.

---

### 5. Recommended phasing

Maps the two initiation methods (§2.4) onto the dual-transport arbiter (§3) incrementally. The wired
path (`ncm0`) stays live and unchanged throughout — each phase only *adds* the wireless ingress.

- **Phase 0 — prove the radios (no protocol).** From a slim script / manually: `bt_on.sh` → confirm
  `hci0 UP RUNNING`; `wlan_on.sh` → confirm `wlan0` AP + `hostapd`/`udhcpd` up and an external device
  can associate + DHCP. Validates §1.2–1.5 end-to-end. Lowest risk; no iPhone needed.

- **Phase 1 — Method A (USB out-of-band handover).** Reuse the existing `iap2d` USB link: add the
  wireless-CarPlay message set (§2.2) + a **link-key/peer store** (`carplay_peers.bin`), so plugging
  in over USB yields the *"Enable wireless CarPlay"* prompt and stores the phone. Bring up the WiFi AP,
  add discovery on `wlan0`, and give `airplayd` a `wlan0` bind. Goal: after the USB pairing +
  ignition-cycle, the phone re-associates and the A/V session comes up **over WiFi** — feeding the
  same `airplayd → OCBM → Mac` pipeline. Highest value / lowest cost (no BT transport yet).

- **Phase 2 — Method B (BT-first) + the arbiter.** Add the Rust **iAP2-over-Bluetooth** transport
  (RFCOMM/L2CAP reusing `iap2d`'s core), **CarPlay EIR advertising**, BT SSP pairing, and the WiFi
  handoff on user consent. Stand up the **dual-transport arbiter** (§3.3) as the session owner:
  both agents armed when idle, first-to-identify wins, no preemption, clean release to dual-idle.
  Now a previously-paired phone starts CarPlay from BT alone (no cable).

- **Phase 3 — robustness.** Band/channel selection (band/channel are app-pushed config values per
  docs/carplay/04_CAPABILITIES_AND_CONFIG.md, 5 GHz the app-side default; the 2.4 GHz BT-off rule), Apple
  Device IE / Interworking IE in `hostapd.conf`, multi-phone favorites + reconnect-order rules
  (§2.5), `disable-Bluetooth`-command handling, power/idle, wired↔wireless coexistence hardening.

---

### 6. Decisions & remaining open questions

**Resolved by project direction (this session):**
- **Rust-native, minimal script** (§3.4): slim scripts do *only* raw radio bring-up (modules/fw/
  `hostapd`/`hciattach`); the on-box Rust binaries carry the protocol *mechanics*. **SUPERSEDED IN
  PART by docs/carplay/04_CAPABILITIES_AND_CONFIG.md:** "all CarPlay protocol + the arbiter live in Rust binaries [on the box]" no
  longer stands as written — everything configurable (arbitration policy, credentials, declaration
  content, band/channel) is app-authored and pushed at init; the box binaries execute mechanics, and
  any new box placement must be earned (docs/carplay/04_CAPABILITIES_AND_CONFIG.md directive 4).
- **BT stack: implement a Rust `btd`** (raw HCI + RFCOMM), reusing `iap2d`'s iAP2 core — not the
  closed stock `bluetoothDaemon` (kept only as a Phase-1 reference/fallback). Keeps the project
  self-contained.
- **Rendering topology:** the **Mac stays the HU head over USB/OCBM**; wireless only changes the
  *phone-side* ingress (`ncm0`→`wlan0`). `airplayd → OCBM → Mac` is unchanged (§3.1).
- **Arbitration:** first-to-connect wins, no preemption, dual-idle on teardown — **matches Apple's
  own reconnection rules** (§2.5), so this is spec-correct, not a project convention.

**Still open:**
1. **Discovery path:** ship a small **Bonjour responder on `wlan0`** (older-iOS, matches WWDC 2017
   flow) vs. the **2023 IP:port-over-iAP2** path (no mDNS, WPA3-clean). Likely both, feature-flagged.
2. **`airplayd` multi-transport shape:** one instance that binds both `ncm0` + `wlan0` and lets the
   arbiter gate which is live, vs. a second instance for the WiFi leg. (Leaning single-instance +
   arbiter gate, to keep one A/V pipeline. Whichever shape, the gate's *policy* is app-pushed
   config — docs/carplay/04_CAPABILITIES_AND_CONFIG.md.)
3. **Link-key storage format:** reuse/extend `carplay_peers.bin`, or a new Rust-owned peer store.
4. **Location requirement:** GNSS/speed/dead-reckoning is a production-HU requirement (§2.6); confirm
   it's out of scope for the bench bring-up.

---

### 7. Reuse base — the `carplayd` PoC already did this (iPhone-verified)

`~/Downloads/github/carplayd` (the Pi/Fedora PoC, and the origin of our vendored `iap2-core`)
implemented Wireless CarPlay **through Phase A2, live-verified against a real iPhone**, on the *same*
AirPlay receiver codebase we run. This turns the task from "build a wireless stack" into "**port a
proven one** to IW416 + OCBM." Reuse map:

| Piece | PoC status | In ccpa_custom |
|---|---|---|
| **AirPlay/RTSP receiver** (pairing, `/info`, SETUP, A/V, HID) | transport-agnostic, same code | **already vendored** (`crates/vendor/receiver`); just bind `wlan0` |
| **iAP2 core** + **wireless session-start + WiFi-cred codecs** (`CarPlayStartSession 0x4301`, `WirelessAttributes` SSID/passphrase/channel/security, `TransportComponent::Wireless`) | built + unit-tested | **already vendored** (`crates/vendor/iap2-core/src/session.rs`) |
| **BT bring-up + SDP + SSP + RFCOMM iAP2** (`rust/carplayd/crates/wireless`, ~1.8k LoC — path corrected 2026-08-16, there is no top-level `crates/` in the PoC; **libc-only Linux Bluetooth sockets (HCI/L2CAP/RFCOMM)**, no BlueZ D-Bus) | Phase A1+A2 DONE, iPhone-verified (discoverable, Just-Works pair, SDP, RFCOMM iAP2, **live NowPlaying over BT**) | **to port** |
| **Session arbiter** (`rust/carplayd/src/arbiter.rs` + `/run/carplay/arbiter.sock`: claim/deny/**preempt**) | DONE, unit + Pi-verified — *exactly* the §3 model | **to port** into the box supervisor |
| **mDNS discovery** (`rx_connect`, private `mdns-sd`, `_airplay._tcp`/`_carplay-ctrl._tcp`) | proven wired | retarget `ncm0`→`wlan0` |
| **WiFi AP** | hostapd + dnsmasq (Pi) | box has `hostapd` + `udhcpd` + `wlan_on.sh` |
| **MFi auth** | needed to forward sign requests to a real CCPA's MFi chip (the Pi had no chip) | **simpler here — box has the genuine local MFi chip** (`airplayd` `LocalMfiSigner`); no external signer |

**Box-specific adaptations (the "not tailored to IW416/OCBM" gap):**
- **BT bring-up:** the PoC shells `hciconfig` on Pi BlueZ; the IW416 attaches over UART
  (`attach_bluetooth.sh` → `hciattach` + NXP fw). Port `bt_bringup.rs` to operate on the box's `hci0`
  (or let the slim script attach and the Rust set CoD/EIR on it).
- **Kernel BT sockets:** **CONFIRMED (Phase 0, 2026-07-13).** `/proc/net/protocols` lists
  **RFCOMM + SCO + L2CAP + HCI** (all built-in; only `hci_uart` is a loadable module). `bt_on.sh`
  brought `hci0` UP RUNNING on the IW416; `wlan_on.sh` brought `wlan0` up as a 5 GHz ch36 AP
  (`192.168.43.1`, `hostapd` + `udhcpd`), with `ocbmd`/wired path unaffected. **The Linux BT-socket
  crate port is feasible** — every socket family it needs is present. Caveat: `hcid` runs an SDP
  server that must be stopped/masked for the raw approach (the PoC's documented `bluetoothd` conflict).
- **OCBM integration:** the PoC pointed the receiver at the Pi's own display; here the
  wireless-established session feeds the **same `airplayd → OCBM → Mac`** pipeline (§3.1). The wireless
  daemon's job ends at "phone joined WiFi + receiver discovering"; `airplayd` on `wlan0` takes over.
- **`bluetoothd`/`bluetoothDaemon` conflict:** the PoC masks `bluetoothd` (it would answer SDP with no
  iAP2 record and handle the CoD/EIR fields). The box has no `bluetoothd`, but the proprietary
  `bluetoothDaemon` may do the same — decide whether to run it at all in wireless mode.

**Bonus lead for the parked mic issue (task #7):** the PoC's **wired** mic uplink is DONE +
iPhone-verified using the *same* `receiver_core`/`uplink.rs` we vendored, via an `input=true` type-100
SETUP + 16 kHz PCM. Since it worked there and not here, our failure (iOS never sets `input=true`)
points at our **modified `/info` `audioFormats` advert** — we replaced receiver_core's original with a
minimal wired catch-all (§ that likely dropped the input-triggering entries). Fix path: diff our
`crates/vendor/receiver/src/info.rs::audio_formats()` (as of 2026-08-16, `:1051`) against the PoC /
`~/Documents/carlink/old/ncm_carplayd` (**sibling of this repo, NOT `ccpa_custom/old/` — path corrected
2026-08-16**) `receiver_core` original and restore what makes iOS request the mic.

---

*Probe + protocol sources: UART session 2026-07-13; `iap2messages-external.i2mspecarchive` (standalone
CarPlay Simulator only — `~/Downloads/Carplay WWDC/Hardware/CarPlay Simulator.app/Contents/Frameworks/iAP2MessageKit.framework/Versions/A/Resources/`;
path recorded 2026-08-16); mempalace `carplay_receiver` (audio/protocol rooms); WWDC 2016-722/723,
2017-717; the `carplayd` PoC (`~/Downloads/github/carplayd`, docs 16/19–23 +
`rust/carplayd/crates/wireless/`).*

---

## AirPlay tunnel — iAP2 handshake

<!-- absorbed: ../wireless/00_WIRELESS_CARPLAY.md -->

STATUS: IMPLEMENTED (2026-07-24), reviewed by 12 Fable agents, fixes applied, pending live-hardware
deploy + test.

### Root cause

docs/wireless/00_WIRELESS_CARPLAY.md/36/38 all converged on the same symptom: `send_wireless_metadata_subscriptions()` fired
`Start*Updates` messages into the AirPlay tunnel and iOS never replied. docs/wireless/00_WIRELESS_CARPLAY.md's Phase 5 experiments
(growing the BT-time `Wireless` Identify's params 6/7 to include metadata ids) were tried and
categorically rejected by iOS on real hardware (5.0 succeeded only for the RouteGuidanceDisplay
param; 5.1/5.2 both 0x1D03-rejected identically).

A combined 12-Fable + 6-Opus research pass (reading the Simulator/GM Cinemo reference material, the
iOS extracts, and — after being explicitly authorized to use it — the pristine vendor CarPlay stack
in `old/carplay_RE/`) found the actual answer in Apple's own **CarPlay Communication Plug-in R14G17
Integration Guide**:

> "To support continued iAP during Wireless CarPlay operation, additional AirPlayReceiverSession APIs
> have been added to tunnel iAP traffic over the CarPlay protocol. You must perform the full iAP
> handshaking over this protocol which includes the detect sequence and link synchronization. The
> Zero-Ack implementation is recommended for the link parameters... Only if the current CarPlay
> session is wireless, you must start a new iAP2 session over the CarPlay control channel. iAP2 over
> Bluetooth must not be disconnected until the disableBluetooth command is received."

The AirPlay tunnel is not a bare pipe iOS will accept subscribes on — it needs its **own, separate
iAP2 link + auth + Identify session**, distinct from the BT-time `Wireless` Identify (which only ever
existed to negotiate the WiFi handoff itself, per docs/wireless/00_WIRELESS_CARPLAY.md's findings, and correctly stays minimal).
Nothing in this codebase ever established that second session; every subscribe sent into the tunnel
was iAP2-meaningless traffic that the AirPlay layer 200-OK'd and silently dropped.

### The fix

- **New crate `crates/vendor/mfi-i2c-local/`** — a byte-for-byte port of `wireless/src/mfi_local.rs`'s
  flock-guarded (`/tmp/carplay_mfi.lock`) direct-I2C MFi cert/sign, needed because both `receiver` and
  `mfi` are `#![forbid(unsafe_code)]` and can't host the raw ioctl code themselves.
- **New module `crates/vendor/receiver/src/iap_tunnel.rs`** — runs the SAME `iap2_core::state::State`
  machine `bt_driver.rs`/`iap2d` use (Init → CertSent → SignSent → Authenticated → IdentSent →
  Identified), fed by inbound `iAPSendMessage` frames instead of a blocking socket loop. Modeled
  directly on `bt_driver.rs::process`/`process_one`/`execute`, including — after the review below
  caught their absence in the first draft — link-layer ACKs on every SYN-ACK/control message, a
  coalesced-packet walk (`link::packet_len`, matching bt_driver's #139 fix), an `mfi_retry` wrapper
  (matching bt_driver's #210 fix), and a guarded DETECT+SYN resend (only while `State::Init`, matching
  bt_driver's own guard — resending mid-auth would reset the phone's link state).
  `send_wireless_metadata_subscriptions()` now fires only once **this** handshake reaches
  `IdentifyAccept` — that's the actual fix.
- **`TransportComponent::AirPlayTunnel` variant** in `iap2-core/src/message.rs` — same
  transport-component shape as the BT-time `Wireless` arm (params 17/20/24), but params 6/7 declare
  the metadata `Start*/Update` message ids instead of the WiFi-handoff-only baseline. Byte-pinned by
  `ident_info_airplay_tunnel_declares_full_metadata_ids` (renamed 2026-07-31 to
  `ident_info_airplay_tunnel_declares_the_generated_metadata_ids` when the lists became
  `features.rs`-generated, docs/carplay/05_METADATA_AND_CONTROLS.md — grep that name in `message.rs`).
- **`events.rs` wiring** — `iap_tunnel::start()` replaces the direct `send_wireless_metadata_
  subscriptions()` call at RECORD and on the `modesChanged` one-shot nudge; inbound dispatch tries
  `iap_tunnel::handle_inbound` first, falling through to the existing `dispatch_iap_tunnel_message`
  once `Identified`; `clear()` resets the tunnel session on teardown. A `disableBluetooth` event type
  is now recognized/logged (log-only — `bt_driver.rs` already doesn't proactively disconnect the BT
  session outside a phone-initiated 0xAA04 abort or the pre-Identify handshake budget, so the "must
  not disconnect early" requirement holds without further wiring).

### Review findings (12 Fable agents) and fixes applied

- Missing link-layer ACKs on SYN-ACK/control messages — **fixed**, now ACKs every control frame like
  `bt_driver.rs` does.
- DETECT+SYN nudge resent in any pre-Identified state, risking a mid-auth link reset — **fixed**,
  gated to `State::Init` only.
- No retry on transient MFi I2C NAKs — **fixed**, added `mfi_retry` (3 attempts) mirroring bt_driver's
  #210.
- No coalesced-packet walk — **fixed**, `handle_inbound` now drains every `link::packet_len`-bounded
  packet in one inbound read, not just the first.
- `Action::Abort` left the session permanently stuck with no recovery path — **fixed**, abort now
  clears the session so the next `start()` rebuilds fresh.
- Missing byte-pin test for the new `AirPlayTunnel` params 6/7 — **fixed**, added.
- `events.rs`'s `disableBluetooth` comment overstated "never disconnects proactively" — **fixed**,
  corrected to name the two real (non-early, non-arbitrary) exceptions.
- Everything else (mfi-i2c-local lock-path fidelity, mutex lock ordering, plist binary-safety of the
  link-framed bytes, workspace scope, rollback plan) reviewed clean — see the 12 agent reports for
  detail; not duplicated here.

### Verification

- `cargo test` in `iap2-core`: 63/63 pass (62 pre-existing + the new byte-pin test).
- `cargo build`/`test` in `mfi-i2c-local`: clean.
- `cargo check` in `receiver`: clean.
- `cargo check -p carplay-wireless` + its 26 tests: clean, unaffected (untouched files).
- `cargo zigbuild --target armv7-unknown-linux-musleabihf --release -p airplayd`: clean release build.

### Next step

Deploy to hardware (close host app → reboot → build/UPX-pack/OCBM-push `airplayd` with checksum
verification → reboot → open host app) and test a live wireless connection, watching
`/tmp/airplayd_wl.log` for the handshake reaching `IdentifyAccept` and, for the first time, real
NowPlaying/RouteGuidance/CallState metadata replies flowing over the AirPlay tunnel.

---

## The metadata tunnel fix

<!-- absorbed: ../wireless/00_WIRELESS_CARPLAY.md -->

Continuation of the gap docs/wireless/00_WIRELESS_CARPLAY.md identified: wireless CarPlay is fully device-verified for A/V/touch/mic,
but NowPlaying / route guidance / call state never reach the app over wireless (wired is fine — it has
its own path via `iap2d`'s physical iAP2 link). This session implements and deploys a fix attempt;
**it is NOT yet device-verified** — that requires a live wireless session with a real iPhone, which is
the next step.

> **⚠️ CORRECTED (2026-07-24, docs/carplay/05_METADATA_AND_CONTROLS.md/36) — do not re-derive from this doc's original claims below.**
> Four items: (1) the `data` key is **lowercase** in current code — the code is right, this page's prose
> was never updated, don't "fix" the code back; (2) "off by default" is no longer true; (3) the FF5A
> framing hedge is very likely dead code — but read the docs/carplay/03_SDK_GROUND_TRUTH.md marker above before treating that as
> settled; (4) the "second `0x1D01 Identify`" experiment was replaced by `sessionManagementInfo`
> (docs/wireless/00_WIRELESS_CARPLAY.md #2.1). Full text: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-30-4`.

**The "Deployed state" section below is a historical deploy record.** The box is not currently armed
as it describes and the launch line it names no longer exists: `CARPLAY_WIRELESS_METADATA` is set only
at the wireless spawn site, and the wired ARM line never carries it.

### Root cause chain (grounded this session)

1. Apple's own CarPlay Communication Plug-in source (`AirPlayReceiverSession.c:5486`,
   `AirPlayReceiverSessionSendiAPMessage`, cross-checked against the Xcode-local CarPlay Simulator's
   `CarPlaySDK.framework` strings — `"iAPSendMessage"`, `"iAP Send Message"`) confirms: wireless CarPlay
   tunnels raw iAP2 messages inside an AirPlay `POST /command` — `{type:"iAPSendMessage",
   params:{Data: <raw iAP2 msg>}}` — instead of over the BT/USB iAP2 link. This channel is bidirectional
   (the receiver already sends other `/command` types like `hidSendReport`/`changeModes` over it).
2. `crates/vendor/wireless/src/bt_driver.rs` (`message.rs` `build_ident_info_excluding`'s
   `TransportComponent::Wireless` arm) deliberately declares almost nothing receivable/sendable over the
   BT identify — `sent={0x5703}`, `rcv={0x4E0A,0x4E0B,0x5702,0x4E0D,0x4E0E}` — specifically OMITTING
   NowPlaying(0x5000/0x5001)/RouteGuidance(0x5200/0x5201/0x5202)/CallState(0x4154/0x4155), because
   declaring them there diverts iOS into plain media-accessory mode and breaks the WiFi handoff
   (device-observed, see that file's comment at the `Action::Note` arm). This is why
   `scratchpad/decode_cmd_capture.py`'s prior capture saw **zero** unprompted iAP2 tunnel frames: the
   accessory never subscribed, wired or wirelessly, at the point that capture ran.
3. **The gap**: nothing in the codebase, before this session, ever sent the `Start*Updates` subscribes
   (that `iap2d` sends wired) over the AirPlay tunnel once the WiFi handoff completes — the one place
   those messages CAN safely be declared (the handoff risk is specific to the BT phase).

### What this session implemented (`crates/vendor/receiver/src/events.rs`)

- `send_iap_message(raw: &[u8])` — wraps raw iAP2 message bytes as `{type:"iAPSendMessage",
  params:{Data: raw}}` and sends over the existing encrypted event channel (`send_command`).
- `send_wireless_metadata_subscriptions()` — sends `iap2_core::link::msg_payload`-wrapped
  `start_now_playing()` / `start_route_guidance()` / `start_call_state()` (the SAME subscribe bodies
  `iap2d` sends wired) over the tunnel, spaced 50ms apart on a background thread. Fires once per
  session, only when `setup()` (event channel wiring, post-RECORD) sees a peer IP on the `192.168.43.0/24`
  AP subnet (same wireless-detection check `airplayd/src/main.rs::write_transport_flag` already uses)
  AND the env var `CARPLAY_WIRELESS_METADATA` is set. **Off by default — zero effect on the proven
  wired/wireless baseline unless explicitly enabled.**
- `dispatch_iap_tunnel_message(data)` — routes an inbound tunneled iAP2 message to the SAME
  `iap2_core::metadata::{now_playing,route_guidance,maneuver,call_state,communications}` parsers
  `iap2d` uses wired, forwarding decoded JSON to the existing `:9004` seam the host's Metadata window
  already renders. Accepts either candidate framing — bare `msg_payload` (`[0x40,0x40][len][msg-id][body]`)
  or that same shape wrapped in a 9-byte iAP2 LINK header (`[0xFF,0x5A]...`) — since which one iOS
  actually uses is unconfirmed.
- `handle_inbound_event()` now also unconditionally captures every inbound event-channel plist body to
  `/tmp/carplay_event_capture.bin` (`[u32 LE len][plist]`, size-capped 4 MiB, same framing
  `scratchpad/decode_cmd_capture.py` already decodes) — pure diagnostic, so the next live session's
  frames (if any arrive on this channel) can be pulled and inspected regardless of whether the dispatch
  above guesses the shape correctly.
- `crates/vendor/receiver/Cargo.toml` gained a path dependency on `iap2-core` (package
  `carplay-iap2-core`) to reuse its message-id constants, subscribe builders and metadata parsers rather
  than re-deriving them.

Reviewed by an independent Fable-model agent pass (adversarial, re-read every referenced API against
the actual crate source, re-ran `cargo test` — 30/30 pass — and an independent cross-build). Three
issues it found were fixed: the inbound `Data` key lookup now tries `Data/data/_data/_Data` (matching
the hedge `decode_cmd_capture.py` already had), the dispatcher now also accepts FF5A-link-wrapped
payloads, and an overclaiming comment about the exact wire shape was softened to cite its actual
evidence (SDK strings, not a byte-exact capture).

### Deployed state (this session)

- `airplayd` cross-built (`armv7-unknown-linux-musleabihf`, release, 1,471,804 B,
  md5 `3ae92319b92e2e6a207bf49fbe6fae7f`) and pushed to the box via OCBM (`ocbm-host push`, app closed) →
  `/usr/sbin/airplayd`. Prior binary backed up as `/usr/sbin/airplayd.bak.1784765459`.
- `tools/session_supervisor.sh` line 162's ARM launch now reads `OCBM_FWD_ENC=1
  CARPLAY_WIRELESS_METADATA=1 setsid airplayd …` (was `OCBM_FWD_ENC=1 setsid airplayd …`) — pushed to
  `/script/session_supervisor.sh`, and the running supervisor was killed so the inittab respawn wrapper
  (`run_supervisor.sh`) relaunched it fresh with the new script (confirmed via md5 + `ps`, new PID).
  **This means the box is CURRENTLY armed to run the experiment on the next wireless session.** Remove
  the `CARPLAY_WIRELESS_METADATA=1` clause (see the comment left in place at that line) to fall back to
  the exact prior launch once the experiment is evaluated.
- `/tmp/carplay_event_capture.bin`, `/tmp/carplay_cmd_capture.bin`, `/tmp/airplayd.log` cleared so the
  next session's capture starts from zero.
- `host/uart_cmd.sh` and `tools/uart_push.sh` PORT updated from the stale `/dev/cu.usbserial-B0010KMC`
  to the current adapter's `/dev/cu.usbserial-0001` (115200 baud) — confirmed live (`uname -a` round
  trip over the box's root console).

### What's needed to actually validate this (next step — requires a live iPhone)

1. Connect the phone wirelessly (BT pair if not already, WiFi handoff, AirPlay session up) and exercise
   Apple Music (NowPlaying), a call (CallState), and if possible a Maps route (RouteGuidance).
2. Pull `/tmp/carplay_event_capture.bin` (and `/tmp/airplayd.log` for the `[events]` TX/RX log lines)
   over OCBM (app closed) or UART, and run `scratchpad/decode_cmd_capture.py
   /tmp/carplay_event_capture.bin` — same decoder, same framing.
3. Three possible outcomes, and what each means:
   - **Inbound NowPlayingUpdate/CallStateUpdate/RouteGuidanceUpdate frames appear, host Metadata window
     populates** → hypothesis confirmed, fix works, promote `CARPLAY_WIRELESS_METADATA` to always-on
     (fold into the default launch line, drop the gate) after a soak.
   - **`[events] TX iAP2-tunnel …` lines show `sent`, but nothing comes back** → iOS silently ignores a
     subscribe for a message id the ORIGINAL (BT) identify never declared receivable — i.e. the tunnel is
     real but message-id receivability is gated by the BT-phase identify, not by transport. Next
     experiment: a SECOND `0x1D01 IdentifyInformation` tunneled the same way, now safely declaring the
     full set, sent once the AirPlay session is up (the BT-phase handoff risk no longer applies at that
     point). This needs a NEW, carefully isolated builder — do not reuse `build_ident_info_excluding`'s
     wired branch without device-testing the declaration ALONE first (see the SESSION-CRITICAL incident
     note at `message.rs:304-311` — a previous careless capability declaration broke an entire session).
   - **Nothing sent at all (`FAILED`), or the capture shows nothing usable** → the tunnel mechanism
     itself is wrong (Data key spelling, framing, or `iAPSendMessage` isn't actually how iOS→accessory
     traffic works this direction) — re-open the SDK/simulator disassembly for the RECEIVING side
     specifically (this session only confirmed the SENDING/outbound API from Apple's source).

---

## Identify experiments — results

<!-- absorbed: ../wireless/00_WIRELESS_CARPLAY.md -->

### Summary

docs/wireless/00_WIRELESS_CARPLAY.md Phase 5 set out to test, incrementally and in isolation, whether declaring wireless CarPlay
metadata capabilities (NowPlaying, RouteGuidance, CallState) in the BT-time `0x1D01
IdentificationInformation` message would let those subscriptions actually flow over the wireless
AirPlay tunnel — since Phase 4 had confirmed the tunnel-side subscribe/dispatch plumbing was already
complete and correct, but zero metadata replies came back with the proven, byte-faithful (Phase-5.0
baseline) Identify.

**Result: two independent id clusters (NowPlaying, RouteGuidance) both broke the wireless BT→WiFi
handoff identically on real hardware. This is now a confirmed closed dead end for the static
Identify-declaration approach as a class, not a per-cluster fluke.** Only the RouteGuidance *display
component* declaration (param 30, no message ids) survived; every params-6/7 message-id addition
tested did not.

### Methodology

Each increment followed the same discipline: implement in isolation → 12-agent Fable code review →
apply any real fixes found → deploy (close host app → reboot → build/pack/push via OCBM with
checksum verification → reboot → reopen host app) → from-scratch BT pairing test (not a warm
reconnect) → watch for the documented fast-fail signal (iOS silently starting BT-native NowPlaying
instead of sending `0x5702` within seconds of `IdentifyAccept`) and any `0x1D03
IdentificationRejected`. A failed increment was reverted immediately (source + test + redeploy) before
proceeding, per docs/wireless/00_WIRELESS_CARPLAY.md.

### Phase 5.0 — RouteGuidance display component (param 30): SUCCESS

Declared param 30 (`RouteGuidanceDisplayComponent`, sub 0 Identifier=42, sub 1 Name="RouteGuidance")
unconditionally on the wireless Identify, matching the wired arm byte-for-byte — previously wired-only.
No message ids were touched (params 6/7 stayed at the Phase-1-4 baseline).

Confirmed on a from-scratch bring-up: `IdentifyAccept` → `RX 0x1D02` → `RX 0x5702` → replied `0x5703`
→ session reached full `STREAMING`/`healthy=1`/`paired=1 record=1`, no reject, no retry. Grounded in
GM's shipped Cinemo NME reference (`reference/gm_cinemo/`) for the underlying `AV_LAYER_UP`-latch
pattern used in the same deploy cycle (docs/wireless/00_WIRELESS_CARPLAY.md), though that fix is unrelated to this specific
Identify change. **Shipped — this is part of the box's current configuration.**

### Phase 5.1 — NowPlaying ids (0x5000/0x5001): FAILED, reverted

Added `0x5000 StartNowPlayingUpdates` (sendable, param 6) / `0x5001 NowPlayingUpdate` (receivable,
param 7) to the wireless Identify, on top of 5.0. Full from-scratch bring-up test, two separate
connection attempts, identical outcome each time:

```
TX 0x1D01 IdentificationInformation (305 B, wireless transport)
RX 0x1D00 -> IdentSent
TX 0x1D01 retry, stripped [6] (305 B, wireless)   <- byte-identical: param 6 is required, can't strip
RX 0x1D03 -> IdentRetried                          <- rejected again
exit state=IdentRetried
[wireless] RFCOMM session ended
```

iOS explicitly `0x1D03`-rejects param 6 (`MessagesSentByAccessory`) once `0x5000` is in it. Since
params 6/7 are in `REQUIRED_IDENT_PARAMS`, `build_ident_info_excluding` cannot actually strip them, so
the retry is byte-identical and predictably rejected again — confirmed `Action::Abort`. **This is
software-recoverable**: the RFCOMM accept loop (`main.rs`) immediately returns to listening and a fresh
`State::Init` starts on the next connection — no physical replug of the box required. Worst case is a
livelock (iPhone reconnects, rejects again, repeat), not a bricked accessory.

Reverted: the two `extend_from_slice` calls removed from `message.rs`, the byte-pin test
(`ident_info_wireless_message_lists_are_byte_pinned`) reverted to the pre-5.1 lists, a stale
`bt_driver.rs` comment (which had briefly been made accurate for 5.1's declared state) reverted back.
62/62 iap2-core tests + 26/26 carplay-wireless tests pass on the reverted source. Redeployed via the
retained pre-5.1 UPX-packed binary (instant redeploy, no rebuild) — checksums verified byte-exact.
Confirmed via a fresh from-scratch reconnect: clean `STREAMING`, no reject.

### Phase 5.2 — RouteGuidance ids (0x5200/0x5201/0x5202): FAILED, reverted, identically

Layered on **Phase 5.0 alone** (deliberately skipping the reverted 5.1) — this was itself a
deliberate, reasoned deviation from docs/wireless/00_WIRELESS_CARPLAY.md's literal "on top of 5.0-5.1" wording, since 5.1 no longer
existed in the codebase to layer on top of, and testing RouteGuidance's own fate independently of
NowPlaying's was the whole diagnostic point (per docs/wireless/00_WIRELESS_CARPLAY.md's "partial results are diagnostic
information" framing).

Added `0x5200 StartRouteGuidanceUpdates` (sendable) / `0x5201 RouteGuidanceUpdate`, `0x5202
RouteGuidanceManeuverInformation` (receivable). Also added one-line diagnostic logging in
`bt_driver.rs` (log the raw `0x1D03` payload before it's parsed down to just top-level ids) —
specifically to get more signal out of an expected-possible failure.

From-scratch bring-up test hit the **identical reject-livelock shape**:

```
TX 0x1D01 IdentificationInformation (307 B, wireless transport)
RX 0x1D00 -> IdentSent
RX 0x1D03 IdentificationRejected raw payload (11 B): [40, 40, 00, 0a, 1d, 03, 00, 04, 00, 06, 4c]
TX 0x1D01 retry, stripped [6] (307 B, wireless)
RX 0x1D03 -> IdentRetried
RX 0x1D03 IdentificationRejected raw payload (11 B): [40, 40, 00, 0a, 1d, 03, 00, 04, 00, 06, 4c]
exit state=IdentRetried
[wireless] RFCOMM session ended
```

**The raw payload is the key new evidence.** Decoding the rejected-param TLV group inside that
payload: `[len=0x0004][pid=0x0006]` — a bare, zero-length "none" presence marker for param 6, carrying
**no embedded array of which specific message id was unsupported**. `parse_rejected_param_ids`'s own
doc comment already noted rejects are "most... zero-length `none` presence markers" as one possible
shape (versus a value-carrying uint16 array as the other) — this confirms the flat, non-granular shape
is what iOS actually sends for this case.

Combined with 5.1's result: **two unrelated message ids (0x5000, a completely different cluster from
0x5200) produced the identical flat, content-free rejection of param 6.** iOS gives no indication it
is reacting to *which* id was added — only that param 6 grew beyond what it will accept. That is real
evidence (2 independent data points, not conclusive proof, but a strong signal) that the rejection
mechanism is a **general** reaction to params 6/7 growing past the Phase-5.0-proven baseline
(`sent={0x5703}`, `received={0x4E0A,0x4E0B,0x5702,0x4E0D,0x4E0E}`), not specific to NowPlaying's
content as originally hypothesized.

Reverted the same way as 5.1: source, test, and comments all rolled back; redeployed the retained
Phase-5.0 binary directly (fastest path back to a working state, box was mid-troubleshooting with the
user actively retrying). 62/62 + 26/26 tests pass. Confirmed via a fresh from-scratch reconnect: clean
`STREAMING`, `IdentifyAccept` ×1, `0x5702` ×2 (retry correctly no-op'd via the docs/wireless/00_WIRELESS_CARPLAY.md `AV_LAYER_UP`
latch), zero rejects.

### Phase 5.3 — CallState ids: not attempted

Given the converging 5.1+5.2 evidence, and that CallState's ids were independently flagged (docs/wireless/00_WIRELESS_CARPLAY.md
V10, based on prior analysis) as the likeliest cluster to *additionally* risk a more severe
media-accessory/HFP-profile diversion on top of the now-confirmed general params-6/7 rejection, there
is no upside case for spending a deploy-test-revert cycle on a near-certain repeat failure. Skipped by
deliberate decision, not by oversight.

### Conclusion and remaining path

**SCOPE — read before applying this conclusion anywhere.** Every experiment in this document modified
the **Bluetooth-time `0x1D01 IdentificationInformation`**, and every rejection was observed on the
**Bluetooth** link. The conclusion below constrains the **BT Identify only**. It says nothing about the
Identify sent on the wireless iAP2 link itself, which is a separate identification on a separate
transport (the RCS DataStream, docs/carplay/05_METADATA_AND_CONTROLS.md) and must declare its own metadata message ids.

Static **Bluetooth**-Identify declaration changes to params 6/7 (the message-id capability lists) are now a
**confirmed closed dead end as a class** — not merely "NowPlaying didn't work" or "RouteGuidance didn't
work" individually, but "declaring new message ids in this specific way is rejected regardless of which
ids." Only the RouteGuidance *display component* (param 30, a capability/component declaration with no
message-id-list involvement) survived unscathed, consistent with GM Cinemo's reference architecture
using a similarly-shaped component declaration safely elsewhere.

> **⚠️ CORRECTED — the MECHANISM claim, NOT the outcome.** "Rejected regardless of which ids" was read
> off a payload that is a **generic** rejection marker, so what iOS objected to was never established.
> **The two reverts stand as device evidence and the BT-time Identify stays byte-pinned because of
> them** — as a safety constraint, not a proven rule about params 6/7. Do not carry the generalisation
> to another transport: the AirPlayTunnel Identify declares the full metadata id set and iOS accepted
> it. [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-38-4`.

The remaining path for wireless NowPlaying/RouteGuidance/CallState metadata, if pursued further, is the
deeper `accessoryd`-internal link-layer question flagged in docs/carplay/05_METADATA_AND_CONTROLS.md Part 2 — understanding *why* iOS's
Identify validator rejects params 6/7 growth here would need dynamic analysis (qemu emulation of the
relevant iOS binary paths), a fundamentally different kind of investigation than further static
Identify edits, and explicitly out of scope for docs/wireless/00_WIRELESS_CARPLAY.md's implementation-plan approach.

### Final shipped configuration

- Phase 1 (process/supervisor fixes) + Phase 2 (protocol declarations) — deployed, wired-regression
  clean.
- docs/wireless/00_WIRELESS_CARPLAY.md's `AV_LAYER_UP` / full-path-`pgrep` fix — deployed, confirmed holding across repeated live
  BT retries.
- Phase 5.0 (RouteGuidance display component, param 30) — deployed, confirmed safe.
- Phase 5.1 and 5.2's code changes — fully reverted from source; not part of the shipped binary.

---

## Verification and incident record

Condensed 2026-08-31 from three dated artifacts (a wireless session verification, a phased
implementation plan, and a BT retry incident). Git history holds the full text.

### Wireless session, verified end-to-end

A sustained wireless session on a freshly-rebooted box, driven through the full CarPlay feature
surface (BT pair → WiFi handoff → AirPlay/RTSP → OCBM → app), with a 1482 B VehicleConfig YAML pushed
at SUBSCRIBE and **SSP Just-Works** pairing (`pairing code: cleared` — the proven CCPA posture).

- **Video: HEVC 1920×720**, `hvcC` parsed on the wire (VPS 24 B, SPS 48 B, PPS 7 B, nalLen 4).
  16,479 frames decoded, **0 failures**. Static-screen handling correct: an 8 s A/V idle with a live
  link is reported as a static screen, not a fault.
- **Audio: every `audioType` negotiated and decoded**, AAC-LC (`codec=1`) and AAC-ELD (`codec=2`)
  through the app's `CompressedDecoder`.

### The phased wireless rollout

Phases 0–3 completed on hardware with **zero regression** to the wired session (full `STREAMING`, real
NowPlaying/RouteGuidance via `iap2d`). Phase 4 sent all five metadata subscriptions correctly over the
wireless tunnel and got **zero replies** despite confirmed media and nav activity on the phone. That
result was correct and its cause was found later: the inbound carrier is the RCS DataStream (SETUP
stream type 130), which was never answered — see `../carplay/05_METADATA_AND_CONTROLS.md`.

### The BT retry / transport-flag incident

`session_supervisor.sh` latched `pair-verify OK`, then immediately logged `wireless session ended
(transport flag cleared)` and fell back to the wired "no iPhone attached" loop — while the real
session was streaming and its TCP connections were `ESTABLISHED` throughout.

**Root cause:** `carplay-wireless` spawned airplayd, then polled `pgrep -x airplayd` for up to 1 s to
confirm it started. BusyBox `pgrep -x` matches the **full invoked path**, not the basename, so the
check never matched, `[av] airplayd failed to start — releasing the transport flag` fired, and each
`0x5702 RequestAccessoryWiFiConfig` retry re-entered the same loop.

**Fix:** stop probing for liveness. The spawn call site keeps the child handle it already has instead
of re-querying `pgrep` for a fresh snapshot, which removes the race class rather than widening the
1 s window — bring-up genuinely can need more than a second while BT and WiFi are also coming up.

