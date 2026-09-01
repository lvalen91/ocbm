# Raspberry Pi 4 + AAOS 16 — wireless CarPlay port

**Status 2026-08-16 (end of session, Pi powered down): wireless CarPlay works on this Pi.**
Video, audio, Siri, metadata and touch are all device-proven. An iPhone (iPhone18,4, iOS 27.0)
pairs over Bluetooth, authenticates against the CCPA's MFi coprocessor **over USB-NCM**, takes the
`0x5702`/`0x5703` handoff onto the **Pi's own 5 GHz SoftAP**, and runs a full session.

| Plane | State | Evidence |
|---|---|---|
| **Video** | HEVC 1920x1080, **rpivid hardware** decode | 9,370 frames, 0 AUs dropped; 2.08 IRQ/frame from `feb00000.codec` |
| **Media audio** | AAC-LC 48 kHz stereo | 45,500+ frames played |
| **Voice / Siri** | AAC-ELD 16 kHz, both directions | **Siri understands speech** — owner-confirmed |
| **Metadata** | Now-playing on the AAOS card | `[media] session ACTIVE — "Breathe Again" / Clayton Nile Young` |
| **Touch** | HID reports to the phone | `x=741 y=447 (of 1920x1080)` |
| **Device management** | `:9115` control socket | `{"ok":true,"devices":[{...,"connected":true}]}` |
| **Night mode / drive state** | `setNightMode`, `setLimitedUI` | both `sent=true` |

**The two failures that cost this session the most time, both now fixed, both the same shape.**
A loud plane credited a silent one — see §5a, which is the reusable lesson.

1. **Video was black** while the seam was up, the key had arrived and decrypt counters were
   climbing: the box negotiated H.264 against an HEVC-only decoder, because it had never received
   the app-pushed config. The app now names a codec mismatch out loud.
2. **Siri heard nothing** while capture armed, chunks arrived and RTP left the box: the AAC-ELD
   encoder was emitting **LD-SBR** (`ASC f8f0312c00bc00`) which iOS silently discards. Fixed in
   `eld_shim.c` — the wire now reads `ASC f8f03000` and AUs grew from 87-97 B to 179 B against
   Apple's 180 B budget. **The crate's own test already asserted this and had been failing**; it
   never ran because it sits behind a feature flag. It runs in the build path now.

**A hardware caveat that is not a software defect.** The USB EarPods used as the bench mic and
speaker are a **full-speed** USB device, and the audio HAL drove them at S24_3LE stereo 48 kHz with
a 10 ms ring (`period_size 240 / buffer_size 480`). The link cannot carry that: roughly 10 drop
events per second in BOTH directions (`AHAL_StreamAlsa: incomplete data`), which made output
inaudible and the mic chopped. Unplugging them routed audio to HDMI0 — `period_size 1024 /
buffer_size 8192`, **zero drops**. The Pi 4 has no built-in microphone, so with the EarPods out
there is no capture device at all: audio and microphone currently cannot both work on this bench
without a separate USB mic.

**Preserved evidence:** `pi/evidence/` holds the state that lived only on the running box — the
generated `VehicleConfig`, the launch environment, the key `airplayd` log lines, the app session
log, and a full device snapshot (kernel config, SELinux labels, codec registration, audio mode,
occupant zones, process and socket state). `/tmp` is tmpfs, so none of it survives the shutdown.

> This doc lives in `pi/docs/` because the Pi port is a self-contained subtree (`pi/`), and
> because `docs/` was being reworked by a concurrent session when it was written.
>
> An earlier draft justified the placement by citing a "hard 15-file cap on `docs/`
> enforced by `tools/test.sh`". **That is wrong for this repo** — there is no
> `tools/test.sh` here and `docs/` holds 59 files. The cap is real but belongs to the
> *sibling* `gm_ccpa` repo, whose README states it. Correcting rather than deleting,
> because a plausible-sounding constraint imported from an adjacent repo is exactly the
> kind of thing that propagates unchallenged.

---

## 1. The architecture

The Pi provides **both radios**. The CCPA is reduced to **MFi coprocessor only**,
reached over the USB-NCM link rather than OCBM's `CH_MFI`.

```
   iPhone ──── Bluetooth (Pi's CYW43455) ────────► carplay-wireless   ─┐
      │                                                                │ MFI1/TCP
      └─────── 5 GHz Wi-Fi (Pi's own SoftAP) ─────► airplayd           │ 192.168.50.2:7789
                                                       │               ▼
                                              127.0.0.1:9001/:9002   mfid ── i2c ── MFi 2.0C
                                              (encrypted A/V seam)   (on the CCPA)
```

vs. the GM Silverado design (`gm_ccpa`), where the **vehicle** owned the AP and the
**CCPA** owned Bluetooth: both of those moved onto the Pi. The load-bearing property is
unchanged and now holds by construction — the receiver runs **on the AP host**, so AP
client isolation never applies.

Not implemented, deliberately: **wired CarPlay**. It needs OTG/UDC role toggling on the
USB-C port, which currently carries adb.

---

## 2. What runs on the Pi

| Process | Role |
|---|---|
| `carplay-wireless` | BT bring-up, SSP, SDP, RFCOMM, iAP2, the `0x5702`/`0x5703` handoff |
| `airplayd` | AirPlay/RTSP receiver: pair-setup/verify, auth-setup, SETUP, RECORD, A/V |
| `rx-connect` | Bonjour advertise + the outbound `GET /ctrl-int/1/connect` nudge |
| `hostapd` | 5 GHz AP on channel 36 (standalone, outside Android's Wi-Fi framework) |
| `apdhcpd` | DHCP for the AP (see §4) |
| `mfid` | On the **CCPA**, serves the two MFi chip operations over `MFI1`/TCP |

Launch environment — every one of these is an opt-in gate; unset, the CCPA behaves
exactly as before:

```sh
CARPLAY_HCI_BACKEND=native            # ioctl + raw HCI instead of hciconfig
CARPLAY_RFCOMM_BACKEND=userspace      # userspace RFCOMM instead of the kernel module
CARPLAY_MFI_ADDR=192.168.50.2:7789    # remote coprocessor
CARPLAY_HOSTAPD_CONF=/data/local/tmp/hostapd_5g.conf
CARPLAY_STATE_DIR=/data/local/tmp/carplay
AIRPLAYD_BIN=/data/local/tmp/airplayd
RX_CONNECT_BIN=/data/local/tmp/rx-connect
PEERSTORE_PATH=/data/local/tmp/carplay/carplay_peers.bin   # /etc is read-only
```

---

## 3. What had to be written

### `crates/vendor/wireless/src/rfcomm_uspace.rs` — RFCOMM in userspace
The AAOS kernel ships **without `CONFIG_BT_RFCOMM`**, so
`socket(AF_BLUETOOTH, SOCK_STREAM, BTPROTO_RFCOMM)` returns `EPROTONOSUPPORT`. That is not
an oversight: Android's own Bluetooth stack implements RFCOMM in userspace over L2CAP, so
AOSP has no reason to enable the module. Implemented TS 07.10 over L2CAP PSM 3, both
directions. Device-proven: inbound for first pairing, outbound (`dlci=2`, `mfs=666`) for
the bonded reconnect.

Two things that are easy to get silently wrong, both now covered by tests:
* The **credit octet is excluded from the length field** (RFCOMM 1.2 §6.5.2). Counting it
  makes every credit grant malformed and deadlocks the link at startup.
* **FCS spans differ**: 2 bytes for UIH, 3 for SABM/UA/DM/DISC.

### `crates/vendor/wireless/src/hci.rs` — native `hciconfig`
`hciconfig` is BlueZ userspace and does not exist on Android. Uses **raw HCI, not mgmt**,
because mgmt synthesises the EIR itself and cannot express the CarPlay marker UUID.

`piscan`/`noscan` go through the **`HCISETSCAN` ioctl**, not a raw `Write_Scan_Enable` —
the ioctl also syncs the mgmt-level `CONNECTABLE`/`DISCOVERABLE` flags, and without them
Linux 6.12 calls `hci_update_scan()` on every ACL connect/disconnect, recomputes
`SCAN_DISABLED`, and turns the accessory invisible after the phone's first connection.

### `pi/apdhcpd` — DHCP for the SoftAP
See §4.

### `crates/mfi-wire` — `MFI1` framing + client
One implementation shared by all consumers. There are **three independent MFi chip users**
and all three needed redirecting: `wireless/src/mfi_local.rs`, `airplayd`'s
`LocalMfiSigner`, and `crates/vendor/mfi-i2c-local` (which backs the AirPlay-tunnel iAP2
handshake).

---

## 4. Android-specific traps (the expensive ones)

Each of these cost real debugging time and none of them announce themselves.

**`target_os` is `"android"`, not `"linux"`.** `cloexec.rs` gated on
`cfg(target_os = "linux")`, so on `aarch64-linux-android` it silently selected the macOS
fallback: `SOCK_CLOEXEC` became `0` and `accept_cloexec` degraded to plain `accept`. Every
Bluetooth socket leaked into the detached daemons; a surviving `rx-connect` then held
L2CAP PSM 3 and the next start failed with `Address already in use`.

**`pgrep -x` matches opposite things on BusyBox and toybox.** BusyBox matches `argv[0]`
(the full path); Android's toybox matches `comm` (the basename). `av.rs` passed the full
path — correct on the CCPA, never matching on the Pi — so `airplayd` was declared dead
while running, the transport flag was released, and iOS restarted the session in a loop.

**Android deletes the `from all lookup main` ip rule.** The connected route for the AP
subnet sits in `main` the whole time but *nothing consults it*; the rule list only has
entries for the framework's own networks. Any process outside the framework gets
`ENETUNREACH` to its own AP subnet. Fixed with rules scoped to the CarPlay subnet only:
```sh
ip rule add to 192.168.43.0/24 lookup main pref 15000
ip rule add from 192.168.43.1  lookup main pref 15001
```

**Android's bundled `dnsmasq` is 2.51 (2009)** and is a vestige — tethering moved to
NetworkStack's own `DhcpServer`. It answered `DISCOVER` with an `OFFER` the phone never
received, and `--dhcp-broadcast` — the obvious lever — is unrecognised and **silently
disables DHCP entirely**: no error, no `DHCP, IP range` line, no bind on `:67`. Replaced
with `pi/apdhcpd`, which **broadcasts every reply** (unicasting to a client with no address
needs an injected ARP entry — the step most likely to have been failing), always sends
server-id/lease/netmask/router/DNS, pads to the 300-byte BOOTP minimum, and assigns
stably per MAC. Worked first try.

**`/etc` is a symlink into the read-mostly `/system` partition**, so the peer store and BT
link keys belong under `/data` (`PEERSTORE_PATH`, `CARPLAY_STATE_DIR`).

---

## 5. Verified session trace

```
[wireless]  RFCOMM client connected -- starting iAP2 handshake
[rfcomm-u]  outbound DLC open on dlci=2 (channel 1, mfs=666)
[mfi]       REMOTE coprocessor 192.168.50.2:7789 — local /dev/i2c-1 will NOT be used
[bt-driver] AuthSuccess  /  IdentifyAccept
[bt-driver] RX 0x5702 -> replying 0x5703 (ssid="pi-carplay" ch=36 WPA2/WPA3)
[apdhcpd]   DISCOVER -> OFFER -> REQUEST -> ACK 192.168.43.100
[rx]        GET /ctrl-int/1/connect -> HTTP/1.1 200 OK
[receiver]  pair-verify OK → control channel encrypted
[receiver]  auth-setup (MFi-SAP) OK → 1113 B M2
[session]   phone identity: iPhone18,4 / iPhone OS 27.0
[session]   RECORD done ; SETUP phase2 DataStream(130) streamID=1 [iAP channel]
[command]   ← iPhone POST /command type='disableBluetooth'
[session]   SETUP phase2 screen(110) → dataPort 34833
[screen]    forwarding video → 127.0.0.1:9001   (enc_seq climbing)
```

MFi latency over NCM, measured 12/12 with zero failures: **cert 155–165 ms, sign
1469–1473 ms**. A session needs six chip operations (~4.9 s total), each far inside the
phone's 10 s per-operation timeout.

---

## 5a. Verify the plane that carries the meaning, not the one that is loud

Two failures on this port were "confirmed working" for days while being broken, and both have the
same shape. Recording it because the AAOS bring-up has **five** independent planes — video, media
audio, voice audio, metadata, input — and only some of them are noisy.

**Mic uplink.** `docs/carplay/03_SDK_GROUND_TRUTH.md` §10 marked it ✅, "owner-confirmed on hardware". What had actually been
observed was that the uplink *carried data*: capture armed, chunks arrived, RTP packets left the box,
counters climbed. What was never observed was Siri *responding*. The encoder was emitting AAC-ELD
with LD-SBR, which iOS discards silently, so every one of those loud indicators was true and the
feature did nothing.

**Video.** The seam was up, the key arrived, decrypt counters climbed and the session read ACTIVE —
with nothing on screen, because the box was negotiating H.264 against an HEVC-only decoder.

The pattern: **a loud plane credits a silent one.** Data-moved is not work-done, and the indicator
that is easy to build is rarely the one that matters. The same trap is recorded independently in
`docs/ops/06_CORRECTIONS_LEDGER.md` `R-49-7`, where A/V health was read as whole-session health and a dead metadata
plane went unnoticed for ten days.

Two habits that actually catch it, both now in the code:

* **Make the silent plane assert its own meaning.** `ProjectionSeamServer` names a codec mismatch;
  `MicUplink` measures signal level *and* detects fabricated silence, because a healthy level is
  compatible with a stream a recogniser cannot use.
* **Run the test that cannot run.** `eld_16k_mono_asc_matches_iphone` asserted the correct ASC and
  had been RED for the life of the feature — it is behind `mic-uplink-eld` and needs fdk-aac, so
  nothing ever executed it. `pi/tools/build_pi_binaries.sh` now runs it on the one path where the
  library is guaranteed present, and fails the build. A test that cannot run is not a test.

---

## 6. What is NOT done

1. **Nothing consumes `127.0.0.1:9001`.** `airplayd` forwards *encrypted* frames to a
   localhost seam by design; the consumer that decrypts, decodes HEVC and renders is the
   Android app. Session established and streaming, **nothing on screen**. This is the
   next body of work — see `pi/docs/01_PROJECTION_APP_DESIGN.md`.
2. **HEVC decode on VideoCore VI is unvalidated.** Every proven decode in this project is
   `OMX.Intel.hw_vd.h265` on Intel; `docs/carplay/02_SESSION_LIFECYCLE.md` (gm_ccpa) calls HEVC MediaCodec
   "first-of-kind" in this ecosystem. Expect surprises here.
3. **Nothing is persistent.** Binaries live in `/data/local/tmp`, `hostapd`/`apdhcpd` are
   started by hand, and `carplay-wireless` is held by an adb session. Needs init services.
4. **The AirPlay-tunnel iAP2 MFi fix is built but not yet exercised** — it needs an
   `airplayd` restart. Affects metadata/controls, not video.
5. **Mic uplink (AAC-ELD) is absent on arm64** — `mic-uplink-eld` needs a cross-built
   libfdk-aac. Already a known gap upstream.
6. **Wired CarPlay** — out of scope for now (OTG/UDC role toggling).

---

## 7. Required AAOS image changes

**`pi/docs/04_IMAGE_REBUILD_GUIDE.md` is the actionable version**, organised by where each change
goes in the AOSP tree, with per-item confidence and post-rebuild verification commands.
`pi/docs/02` holds the device observations behind it, and the earlier round is in
`/Volumes/stuff/rpi/aaos/docs/os-corrections-2026-08-16.md` (external volume).

The two with the largest payoff both **delete code this port had to write**: `CONFIG_BT_RFCOMM=y`
removes `rfcomm_uspace.rs` entirely, and fixing the Wi-Fi vendor HAL's `setCountryCode` would let
the framework SoftAP work and remove both standalone `hostapd` and `pi/apdhcpd`.

The critical ones:

* **`CONFIG_BT_RFCOMM=y`** would delete the entire `rfcomm_uspace.rs` requirement.
* Remove `dtoverlay=miniuart-bt` — it breaks Bluetooth *and* the serial console.
* `ro.boot.wificountrycode` must be a real ISO code, not `00`.
* The Wi-Fi vendor HAL's `IWifiApIface.setCountryCode` must not return `NOT_SUPPORTED`,
  or framework 5 GHz SoftAP can never start.

---

## 8. Shared-code changes that reach the CCPA — read before trusting them there

The Pi port deliberately **modifies shared code** rather than forking it, because the seam, the
crypto and the iAP2 layer are proven and a fork would drift. The cost is that five changes made and
tested on the Pi now also run on a CCPA, where **none of them has been exercised on hardware**.

Every one carries a `⚠️ PI-VERIFIED ONLY (2026-08-16)` comment at the site. To find them all:

```sh
grep -rn "PI-VERIFIED ONLY" --include='*.rs' --include='*.c' crates/ ccpa/
```

| Change | What it does on a CCPA | Assessment |
|---|---|---|
| `eld-codec/csrc/eld_shim.c` | AAC-ELD encoder no longer emits LD-SBR; bitrate 24k→48k | The bug was platform-independent (fdk-aac's SBR auto-table does not know the board), so the CCPA had it too. The fix is almost certainly wanted; it is simply unverified there. |
| `receiver/src/session.rs` | Mic uplink disarms on per-stream teardown, not only session teardown | On WIRED, type 100 is the MEDIA stream, so a media teardown now also drops the mic. Same semantics — the uplink is that stream's input leg — but a real behaviour change on a path never run. |
| `wireless/src/control.rs` | **New**: binds `:9115`; writes `projection_policy.json` to `/etc/carplay` (flash) on a policy push | A failed bind is non-fatal by design. The flash write is new for this crate on that platform, though only on a user toggle. |
| `wireless/src/main.rs` | Session claim hoisted to process lifetime + RAII guard | Touches the path deciding whether an inbound connect is ACCEPTED — the CCPA's primary path. 64 unit tests pass; that is not a paired phone. |
| `wireless/src/ssp_agent.rs` | Link-key store read-modify-write serialized | Closes a genuine lost-update race, in the conservative direction. Guards how a CCPA remembers a paired phone across reboots. |

**Additive and safe by construction** (unset environment keeps prior behaviour byte-for-byte):
`CARPLAY_CFG_FILE` and the `receiver::uplink::set_display` call in `ccpa/airplayd/src/main.rs`, the
`onCodec` hook on the shared `VideoSeam.kt` (defaults null), and the `aarch64-linux-android` section
in `.cargo/config.toml`.

**Suggested first CCPA session after this work:** pair a phone (exercises the claim path and the
link-key store), start a wired session and tear down a stream (exercises the uplink change), and
trigger Siri (exercises the encoder). If `:9115` fails to bind, that is expected to be a log line
and nothing more — confirm that it is.
