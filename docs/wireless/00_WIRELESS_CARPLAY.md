# Wireless CarPlay — session, handoff and the AirPlay tunnel

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 20, 39, 30, 38, 29, 36, 37; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

**Contents:** the wireless research baseline (§0-§2 current, §3-§7 condensed history) → the AirPlay tunnel and Identify experiments (history, pointer to carplay/05) → session verification and incident record. The wireless *metadata* content lives in carplay/05.

## Wireless CarPlay — baseline research

<!-- absorbed: ../wireless/00_WIRELESS_CARPLAY.md -->

Research pass answering: *what does the CCPA box already have for Wireless CarPlay, and what must
be added?* Sources: live UART inventory of the box; the iAP2 spec archive + CarPlaySDK protocol reference notes
(mempalace `carplay_receiver`); WWDC transcripts (2016-722/723, 2017-717). Everything below is
ground-truth-verified, not assumed.

> **⚠️ SHIPPED — this began as research and is now largely BUILT**, device-verified through the A3
> BT→WiFi handoff. The §3 design is what was built and the §4 gap analysis is closed. Full status
> text: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-20W-1`.
>
> **Scope of §0-§1: stock-kernel units.** The hardware inventory below describes a stock 3.14.52 IW416
> unit with the vendor rootfs. The mainline baseline (`baseline/README.md` §2.5) loads cfg80211, mac80211
> and Bluetooth as modules and names the IW416 interface `mlan0` (mwifiex), not `wlan0`. A CCPA unit
> probed 2026-10-28 is an NXP IW416 (SDIO 02df:9159, `mwifiex_sdio` + `btnxpuart`, Bluetooth 5.4).
> Other CCPA units carry other radios (see [01_BT_AND_RADIO.md](01_BT_AND_RADIO.md)). The C2Air is a
> different box (Allwinner V821 rv32, AIC8800D80 radio, MFi 3.x); see `c2air/README.md`.

---

### 0. TL;DR

The box carries a **complete but dormant Wireless-CarPlay hardware + firmware + script stack** (NXP
IW416 WiFi+BT combo). The radios, drivers, firmware, AP config, BT stack, and on-demand bring-up
scripts all exist and were deliberately preserved when the stock projection code was stripped. What
does **not** exist is the **orchestration + protocol glue**: the iAP2 wireless-CarPlay message
exchange (WiFi-credential handoff), discovery on `wlan0`, and pointing the (transport-transparent)
`carplayd`/`receiver` stack at the WiFi link instead of `ncm0`.

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
- **`hostapd.conf`** — `wpa=3` (WPA2/WPA3-PSK), `ssid=<box stock SSID>`, `wpa_passphrase=<box default passphrase>`,
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
- Runtime confirms it: only `ocbmd`, `iap2d` (`/dev/android_iap2`, **USB** iAP2), `carplayd` (over
  `ncm0`) run. **No `bluetoothDaemon`, `hostapd`, or `wpa_supplicant`.**
- `session_supervisor.sh` orchestrated only the wired path at the time of this survey
  (`host present → projection_up.sh (iAP2 → Identified) → carplayd + rx-connect`; `rx-connect` was merged into `carplayd` on 2026-09-08) with zero wireless
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
over WiFi; only the IP transport underneath changes. **`carplayd`/`receiver` are reusable as-is,
bound to `wlan0` instead of `ncm0`.**

#### 2.2 Bluetooth is the bootstrap channel
Wireless CarPlay starts over **Bluetooth**, carrying **iAP2**. Over that iAP2 link the phone and
accessory negotiate CarPlay and the WiFi hand-off. Authoritative iAP2 messages (from
`iap2messages-external.i2mspecarchive` — **path recorded 2026-08-16**, per docs/ops/03_REFERENCE_INDEX.md standing rule 1: the
`-external` archive ships ONLY in the standalone Simulator, at `~/Documents/carlink/carplay_simulator/CarPlay Simulator.app/Contents/Frameworks/iAP2MessageKit.framework/Versions/A/Resources/`
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
| **BT/WiFi coexistence:** extra BT profiles only if AP is 5 GHz; if AP is 2.4 GHz, **BT off during an active session** | Our AP is **5 GHz** → BT may coexist; keep the 2.4 GHz rule if we ever drop to `wifi_use_24G` (on-box flag file; the app pushes no channel or band, owner decision 2026-10-07 — see 01_BT_AND_RADIO.md "What the box does with app-pushed config") |

#### 2.7 How iOS classifies the vehicle SoftAP — known network, Internet, join/leave (added 2026-10-07)

Question: does our pairing tell iOS the SoftAP has Internet, or make it a normal known network? Why
does the bench iPhone stay joined (or join "early") when vehicles join only at CarPlay pairing and
leave at CarPlay end? Sources: iPhone `idevicesyslog` captures `/tmp/ccpa_sess/ios/ios_cp*.txt`
(gm_ccpa) and `ios_stock1.txt` (stock), all `osVersion=27.2`. SSID shown as `<SSID>`. Single-agent
read of raw logs + code; **not yet independently re-verified** (standing rule).

**What we send has no Internet / network-type field.** 0x5703 is SSID(1)/Passphrase(2)/SecurityType(3)/
Channel(4) only (Apple catalog via `tools/i2mspec_dump.py --message 0x5703`; builder
`wifi_handoff.rs` `build_accessory_wifi_configuration_information`; security mapping in `read_hostapd_ap_config`). 0x4301
WirelessAttributes is SSID/Passphrase/Channel/IPAddress/SecurityType (`crates/vendor/iap2-core/src/session.rs`, the `wireless` sub-parameter module).
Identify param 24 sub-params are id/name/iAP2/CarPlay/ThemeAssets (`iap2-core/src/spec.rs`, module `wireless_carplay`).
No `/info` key in `crates/vendor/receiver/src/info.rs` concerns Wi-Fi/network/Internet. R14G17 has no such
key either (only `kAsyncConnectionFlag_NonExpensive`, `AccessorySDK/Support/AsyncConnection.h:80`).

**iOS treats `<SSID>` as a CarPlay-only network, not an ordinary known network:**
- Every add/remove is by `carkitd`: `ios_cp3.txt` Oct 6 14:04:38 `WiFiManagerRemoveNetwork: reason 7` then
  14:04:51 `Add network <SSID>, requested by "carkitd"` / `WiFiManagerAddNetwork: reason 1`; `ios_cp6.txt`
  Oct 7 17:00:17.787 carkitd removes a *different* vehicle SoftAP (`<OTHER_SSID>`) and at 17:00:17.840 adds
  `<SSID>` (`__GetNetworkWithSameSsid: network not found` — fresh record). The record's
  `addedAt=[... (reason=WiFi Settings)]` is the label wifid prints for these carkitd "reason 1" adds —
  it does **not** mean the user joined it in Settings.
- `WiFiNetworkSyncHelperIsNetworkSyncable: carplay only network, not syncable`, `Network:<SSID> is CarPlay`,
  `carplay=yes` on both the known-network record and the BSS scan entry (cp6, and the same record in stock1).
- `__WiFiDeviceManagerSetInterfaceRank: internetAccess bit ZERO` + `kSCNetworkServicePrimaryRankNever`
  (13× in cp6): iOS never makes it the primary interface; cellular stays primary. **Where the
  `internetAccess` bit comes from is UNKNOWN** (no iAP2 or `/info` field carries it; the 802.11u
  Interworking IE "Internet" bit in the radio's beacon — §2.6 lists the Interworking IE as an AP
  requirement — is a candidate, unverified; the radio's `hostapd_bcm` config was not captured).

**Join is CarPlay-driven.** Each move from the home network to `<SSID>` is `asked to Disassociate ... reason
=<9:1003:1003>` followed by `AUTO-JOIN ... trigger=carplay` (cp6 17:00:17.924→17:00:25.488 link up →
17:00:31 `carplay session started`; again 20:50:24 and 20:51:28). Ordinary auto-join sees the record as
`enabled=true` but `deferrable (reason=DeferredNonHomeNetwork | TimeDeferredForPublicHotspot)`; with the
home network in range it did not pick `<SSID>` until `trigger=carplay` (cp6 20:50:58→20:51:28). The
"early" join is therefore after iAP2/0x5703 over BT but before the AirPlay session — the designed order.

**Leave: iOS 27.2 does not drop the SoftAP when the CarPlay session ends.** Across cp4/cp5/cp6, 22
`sessionDidDisconnect` events; the 5 that coincide with a Wi-Fi link-down all have the link-down
*first* (Wi-Fi loss ended the session, e.g. cp6 Oct 6 20:28:29.20 link down → 20:28:29.53 session ended;
two were `WiFiDeviceManagerSetUserAutoJoin: user auto join disabled` — a phone-side toggle). cp6 Oct 7
17:07:40 session ended with the link up until 20:48:38. When the SoftAP vanished the phone fell back to
the home network (cp6 20:50:57 → 20:50:58).

**Explanations for bench ≠ vehicle:**
| Hypothesis | Status |
|---|---|
| SoftAP saved as an ordinary user-joined network | **Refuted** for this phone — carkitd adds, "carplay only network" |
| Our pairing advertises Internet / non-CarPlay semantics | **Refuted** — no such field exists in what we send |
| Vehicle leave is iOS reacting to session end | **Not what iOS 27.2 does here**; vehicle AP teardown / deauth at session end is UNKNOWN (no vehicle log) |
| iOS 27 behaviour change | **UNKNOWN** — no pre-27 iPhone log exists to compare |
| Vehicle WAN (OnStar hotspot) changes `internetAccess` / auto-join | **UNKNOWN** — bench bit is ZERO; no vehicle capture |
| Bench has no home-network competition / phone left Wi-Fi joined after a session end | Consistent with logs, not isolated |

**Bench checks.** (1) iPhone Settings > Wi-Fi > *Edit/My Networks*: is `<SSID>` listed and what Auto-Join
state/labels it shows (iOS 27 UI labelling UNKNOWN — record it). (2) Settings > General > CarPlay >
car > *Forget This Car*, then confirm `Remove network <SSID>, requested by "carkitd"` and that the phone
no longer joins without BT pairing. (3) End a session phone-side with the AP still up, then stop the AP
with the session up; grep the order of `sessionDidDisconnect` vs `Sending link down`. (4) Repeat in a
vehicle and compare `internetAccess bit` and leave order. Grep:
`rg 'requested by "carkitd"|carplay only network|internetAccess bit|SetInterfaceRank|trigger=carplay|sessionDid(Connect|Disconnect)|Sending link down|link up to|SetUserAutoJoin|STOP_CAAJ'`.

---

### 3-7. Design, gap analysis, phasing and PoC reuse — HISTORICAL

> The original scoping (target architecture, gap analysis, phasing, decisions, reuse of the
> `carplayd` Pi/Fedora PoC) is condensed here. The full text is in git history. All seven gap items
> shipped (ledger `R-20W-4`); its "still open" questions are superseded by the code and must not be
> read as current.

What survives as current design:

- **Topology:** the host app is the head unit over USB/OCBM and never changes. Wireless adds a second
  phone-side ingress (BT-bootstrapped Wi-Fi to the AP interface instead of `ncm0`), not a new renderer;
  downstream of `carplayd` the pipeline is identical (`carplayd -> OCBM -> host`).
- **Arbitration:** one live session at a time, first to reach a confirmed milestone (iAP2 identified or
  wireless handshake complete) owns it, the other transport is blocked only while a session is live, and
  teardown returns to idle. This matches Apple's reconnection rules (section 2.5). The mechanics run on the
  box (`tools/session_supervisor.sh`, `crates/vendor/wireless/src/arbiter_client.rs`); the policy
  (preference, tiebreak, Wi-Fi SSID and passphrase) is app-pushed config per
  [../carplay/04_CAPABILITIES_AND_CONFIG.md](../carplay/04_CAPABILITIES_AND_CONFIG.md). Idle-with-both-armed
  begins only after the host app has connected and pushed config (ledger `R-20W-3`).
- **Rust `btd`** owns Bluetooth (raw HCI, SDP, SSP, RFCOMM iAP2) and reuses the iAP2 core that `iap2d` uses;
  the proprietary `bluetoothDaemon` is not used. See [01_BT_AND_RADIO.md](01_BT_AND_RADIO.md).
- **Stock-kernel fact (3.14.52 units only):** `/proc/net/protocols` listed RFCOMM, SCO, L2CAP and HCI as
  built in, with only `hci_uart` loadable; stock `hcid` runs an SDP server that must be stopped for the raw
  approach. The mainline baseline loads the Bluetooth stack as modules (`radio-modules.fragment`).
- **Mic lead:** the "modified `/info` audioFormats dropped the mic" hypothesis was resolved; the mic
  uplink ships and is device-confirmed (ledger `R-20W-4`).

---

*Probe + protocol sources: UART session 2026-07-13; `iap2messages-external.i2mspecarchive` (standalone
CarPlay Simulator only — `~/Documents/carlink/carplay_simulator/CarPlay Simulator.app/Contents/Frameworks/iAP2MessageKit.framework/Versions/A/Resources/`;
path recorded 2026-08-16); mempalace `carplay_receiver` (audio/protocol rooms); WWDC 2016-722/723,
2017-717; the `carplayd` PoC (`~/Downloads/github/carplayd`, docs 16/19–23 +
`rust/carplayd/crates/wireless/`).*

---

## AirPlay tunnel and Identify experiments — HISTORICAL

> **CARRIER REFUTED — read before using anything below.** The 2026-07 work in this area assumed
> wireless iAP2 rides `iAPSendMessage` inside AirPlay `POST /command` (the 2017 R14G17 mechanism).
> Current iOS carries it on a RemoteControlSession DataStream, SETUP stream type 130, with its own
> 32-byte header and ChaCha20-Poly1305 framing (`crates/vendor/receiver/src/datastream.rs`, module
> doc). `iAPSendMessage` survives only as a fallback. The authoritative text is
> [../carplay/05_METADATA_AND_CONTROLS.md](../carplay/05_METADATA_AND_CONTROLS.md): declaration rules
> (§5), device results (§6) and "Carrier — DataStream / RemoteControlSession / stream type 130".

The three long records that stood here (the AirPlay-tunnel handshake implementation of 2026-07-24,
the "metadata tunnel fix" deploy record, and the Phase 5 Identify experiments) are in git history
and are owned by carplay/05. What still stands from them:

- **The tunnel needs its own iAP2 link, MFi auth and Identify**, separate from the Bluetooth-time
  Identify (Apple's R14G17 Integration Guide says so). Implemented as the receiver's `iap_tunnel`
  module (`start`, `handle_inbound`, `reset`), the `TransportComponent::AirPlayTunnel` arm in
  iap2-core `message.rs`, and `crates/vendor/mfi-i2c-local` for the chip. Message shapes, link
  ACK/coalescing/retry behaviour and the Identify content survive the carrier change; only the
  carrier moved.
- **Bluetooth-time Identify stays byte-pinned.** Phase 5.0 (RouteGuidance display component, param 30)
  was accepted. Adding NowPlaying ids (5.1) or RouteGuidance ids (5.2) to params 6/7 drew a
  `0x1D03` reject with a generic param-6 marker both times, both were reverted, and the pin remains
  as a safety constraint. What iOS objected to was never established, and the result must not be
  generalised to another transport (carplay/05 §6.2).
- **Tunnel-arm Identify:** `extended` was accepted by the phone; `all` is refuted on that arm
  (carplay/05 §5.3, §6).
- **`CARPLAY_WIRELESS_METADATA`** is a spawn-scoped gate set only at the wireless carplayd spawn site in
  `tools/session_supervisor.sh`; the wired launch never carries it. The deploy record that described the
  box as armed for an experiment is obsolete.
- Metadata over wireless was verified live on the AirPlayTunnel arm on 2026-10-19 (carplay/05).

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

**Root cause:** `btd` spawned carplayd, then polled `pgrep -x carplayd` for up to 1 s to
confirm it started. BusyBox `pgrep -x` matches the **full invoked path**, not the basename, so the
check never matched, `[av] carplayd failed to start — releasing the transport flag` fired, and each
`0x5702 RequestAccessoryWiFiConfig` retry re-entered the same loop.

**Fix:** stop probing for liveness. The spawn call site keeps the child handle it already has instead
of re-querying `pgrep` for a fresh snapshot, which removes the race class rather than widening the
1 s window — bring-up genuinely can need more than a second while BT and WiFi are also coming up.

