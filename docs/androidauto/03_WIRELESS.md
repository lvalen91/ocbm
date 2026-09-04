# Wireless Android Auto

> **STATUS:** CURRENT · single owner for this topic. Opened 2026-08-31 when the wireless AA
> workstream started. Correct this file in place — do not add a sibling.

**Contents:** what exists today → what the transport has to do → what is reusable from wireless
CarPlay → the bench → open questions, and what has actually been verified.

Wired AA — which is complete — is in [`00_ARCHITECTURE.md`](00_ARCHITECTURE.md) and
[`01_SESSION_AND_AV.md`](01_SESSION_AND_AV.md). Arbitration is in
[`02_ARBITRATION.md`](02_ARBITRATION.md). The wireless CarPlay path this work borrows from is
[`../wireless/00_WIRELESS_CARPLAY.md`](../wireless/00_WIRELESS_CARPLAY.md) and
[`../wireless/01_BT_AND_RADIO.md`](../wireless/01_BT_AND_RADIO.md).

## 1. State: the bootstrap layer and the pump are built; the radio bring-up is reused

**End-to-end device result and WIRELESS BASELINE (2026-09-04, Pixel 10 / Android 17 / gearhead
17.5.663204, box 71f4b27, app 449-test build).** Timeline from the box paging the bonded phone:
+0.00 s ACL up → HFP SLC done in 144 ms → the phone opens our AA record and the bootstrap
(4→5→1→2→3→7→6) completes inside the same 0.5 s tick → +0.5 s `PM_WIRELESS_AA` to the app →
+1.5 s DHCP ACK on the box AP (5 GHz ch 36, WPA2) → +2.0 s phone TCP to `aa-bridge --wireless`, app
attached, VERSION 6.1 → +2.2 s TLS 1.2 → +2.3 s service discovery → +2.4 s video config
1280×720@60 → **+2.7 s first IDR**. Steady state at 339 s: 75.5 MB phone→host (≈1.8 Mbps), 500 KB
host→phone, CH_IP backlog ≤ 1; video rx 4429 / decoded 4429 / dropped 0, keyframe requests 0, slot
drops 0; audio media 7864 pkt, guidance 36 (one prompt), underruns 0; one bootstrap, no re-dials; two
log errors, both nits: the app does not parse the `CT_SETTIME` ack (`unhandled frame ch=0x0 op=0x5`)
and one `playback backlog over 150ms — dropped 1 packet` at the guidance prompt start. Phone side (gearhead logcat, same session): 0 `WPP_SOCKET_CLOSED_BY_PEER` restarts, 1 RFCOMM
create, state machine CONNECTING_RFCOMM → CONNECTED_RFCOMM → WIFI_PROJECTION_START_REQUESTED →
CONNECTING_WIFI → FOUND_COMPATIBLE_WIFI_NETWORK → VERSION_CHECK_COMPLETE → CONNECTED_WIFI (+1.8 s) →
PROJECTION_INITIATED; media and navigation confirmed by the owner. Parity with the wired baseline
in 01_SESSION_AND_AV.md. Two defects found on the way and fixed the same night:
the app ignored projection mode 4, and the accept path dropped the phone's AA RFCOMM channel after
the bootstrap — gearhead logs `WPP_SOCKET_CLOSED_BY_PEER`, restarts the Wi-Fi Projection Protocol
and re-dials at ~1 Hz (the "Connecting to Android Auto" overlay flapping); the channel is now held
for the session, as stock does. Known nits, harmless so far: our field-less `WifiVersionRequest`
("HU requested version: 0.0") sent back-to-back with `WifiStartRequest` ("Got a version request
after unexpected start request").


Decision, 2026-09-01 (corrected in place 2026-09-01 after step 1 landed): **`ccpa/aa-wireless` is a
LIBRARY, not a daemon** — it has no `main.rs` and no `[[bin]]`, and is served by the existing
`carplay-wireless` daemon. It could not be its own process: `sdp_server` binds L2CAP PSM 1 with no
`SO_REUSEADDR`, so a second SDP-serving daemon gets `EADDRINUSE` and is never advertised (§6b).
`aa-bridge` stays a wired AOAP pump and is not refactored. The wired path ships and works; this one
does not yet, and the two only need to agree about who owns the box. They meet at
`box_common::flags` and nowhere else.

| Piece | Where | State |
|---|---|---|
| Bootstrap framing + the seven messages | `ccpa/aa-wireless/src/proto.rs`, `wpp.rs` | **built, 30 unit tests** |
| Bootstrap state machine | `wpp::Bootstrap` | **built**, transport-free and host-testable |
| `ProjectionOwner::WirelessAa` (`"wireless-aa"` → `PM_WIRELESS_AA`) | `crates/box-common/src/flags.rs` | **built** |
| Owner claim / stand-down | `aa-wireless/src/lib.rs` (`claim_owner`/`release_owner_if_ours`), called from `crates/vendor/wireless/src/main.rs` | **built** |
| Shared BT primitives | `crates/bt-common` | **extracted 2026-09-01** from carplay-wireless; 29 tests |
| AA SDP record + RFCOMM accept (channel 4) | `crates/bt-common/src/sdp_server.rs`, `crates/vendor/wireless/src/main.rs` | **built** (steps 3-4 below) — and §2f confirms this IS the direction: the phone dials us |
| Headset gate: HFP-HF + HSP-HS records, AG search, AT SLC client | `crates/bt-common/src/sdp_record.rs` (`HandsFreeRecord`/`HeadsetRecord`), `sdp_server.rs`, `crates/vendor/wireless/src/hfp_hf.rs`, `sdp_client.rs`, `reconnect.rs` (`attempt_headset`) | **built 2026-09-04**, 30 host tests; the precondition the phone waits on (§6b, §6d) |
| AP bring-up | — | not started (mechanism exists for CarPlay) |
| OCBM `CH_IP` pump | `ccpa/aa-bridge/src/wireless.rs` + `pump.rs` + `appport.rs`, armed by `aa-bridge --wireless` | **built 2026-09-04**, 13 host tests; see §6c |

There are no `--selftest` / `--listen` flags: those belonged to the standalone binary that step 1
removed. `run_bootstrap` takes any `Read + Write`, so the codec, the sequence and the whole bootstrap
run under `cargo test -p aa-wireless` with no hardware and no socket — RFCOMM is a stream too, so the
framing path under test is the one that ships. `run_bootstrap` returns an `Outcome`
(`Established` / `Failed` / `PeerClosed` / `FramingLost`); the caller keeps the projection-owner
claim only on `Established`.

### Footprint — measured, not estimated

`aa-wireless` armv7-musl release: **366,880 B** static, unpacked (rustc 1.98.0). At the measured UPX ratio
(`tools/upx_pack.sh`, ~50%) that is **~180 KB**, against **3.4 MB free** on the box's jffs2 rootfs
(`df` on the live unit, 2026-09-01: 12800K total / 9280K used / 3520K available).

**Space is not a constraint on this workstream**, and the footprint open item in
[`01_SESSION_AND_AV.md`](01_SESSION_AND_AV.md) §3 is answered. For reference if it ever tightens:
`airplayd` (1,793,056 B) and `carplay-wireless` (495,976 B) are both shipped **unpacked**, and
`ocbmd.orig` (450,888 B) is a backup git already holds — roughly 1.6 MB reclaimable without losing
any function.

### Build-host trap — resolved 2026-09-01

`cargo zigbuild --target armv7-unknown-linux-musleabihf` failed with ``can't find crate for `core` ``
because `rustc`/`cargo` resolved to the **Homebrew** Rust, which ships std for the host only.
`rustup target add` reported "up to date" throughout, because rustup's own toolchain did have the
target — it was simply never on `PATH`. The error names the target, not the toolchain, which is what
makes it expensive.

Fixed by removing the Homebrew formula (nothing depended on it) and sourcing `~/.cargo/env` from
**both** `~/.zshrc` and `~/.zshenv`. Both are needed and the second is the one that is easy to miss:
`~/.zshrc` runs for interactive shells only, so without the `~/.zshenv` line every scripted or
non-interactive build — which is where the box cross-builds actually run — still finds no `cargo`.

## 2. The bootstrap protocol — recovered, not guessed

Structurally this is **not** the wired problem with a different cable. Wired AA is one AOAP bulk
pipe. Wireless AA is a Bluetooth bootstrap that hands the phone an AP and a TCP endpoint, after
which the *same* TLS AA session runs over that socket. The host app's AA engine should not be able
to tell the difference — that is the design constraint, and it is what makes this cheap if it holds.

### 2a. The decisive fact: this box already does it

`strings` on the stock `ARMAndroidAuto` (reconstructed image,
`cpc200_ccpa_firmware_binaries/analysis/`) shows the full bootstrap message set compiled in:

```
aasdk::proto::data::WifiStartRequest      WifiStartResponse
aasdk::proto::data::WifiInfoRequest       WifiInfoResponse
aasdk::proto::data::WifiVersionRequest    WifiVersionResponse
   from WifiStartData.proto / WifiInfoData.proto / WifiVersionData.proto
```

The stock firmware implements wireless AA on **this silicon, these radios**. So there is no hardware
question to answer and no capability to prove — only our own code to write. It also gives an on-box
oracle: the stock stack can be run and captured for any behaviour ours gets wrong. Note the stock
binary uses the older `aasdk::proto::data` namespace, i.e. an earlier aasdk layout than the
`aap_protobuf::aaw` one below; the messages are the same, the packaging is not.

### 2b. Bluetooth: what the head unit advertises

Four SDP records, all browsable, all from one Bluetooth identity on one SDP server
(`crates/bt-common/src/sdp_server.rs`; PSM 0x0001 has a single holder, so a second daemon
advertising separately would only get `EADDRINUSE`):

| Record | Service class | RFCOMM ch | Builder |
|---|---|---|---|
| Wireless iAPv2 | `00000000-deca-fade-deca-deafdecacaff` | 1 | `ServiceRecord` |
| Wireless Android Auto Protocol | `4de17a00-52cb-11e6-bdf4-0800200c9a66` | 4 | `ServiceRecord` |
| Hands-Free | `0x111E` Handsfree + `0x1203` GenericAudio, profile `0x111E` v1.7, `0x0311` SupportedFeatures = `0x003F` | 5 | `HandsFreeRecord` |
| Headset | `0x1108` Headset + `0x1203` GenericAudio, profile `0x1108` v1.2, `0x0302` RemoteAudioVolumeControl = false | 6 | `HeadsetRecord` |

The first two are the projection records; the phone reads the AA channel out of its record and
dials it. **The last two are the gate** (§6b): gearhead will not begin wireless setup until the
phone's own `BluetoothProfile.HEADSET` reports the head unit connected, and a phone whose
`PhonePolicy` auto-connects to a bonded headset-class device needs a record to find first. Both
profiles are advertised because AOSP reaches that state by different routes for each — see §6d.

`0x003F` is not a guess: it mirrors the `AT+BRSF=63` the stock box sends. Note the two bitmaps are
not the same field (SDP bit 5 is Wide-Band Speech; BRSF bit 5 is Enhanced Call Status), and it is a
struct field rather than a constant so it can be narrowed to `0x001F` without touching the encoder.

*(The justification here used to be "harmless because we never open SCO". We open SCO now — see
[`01_SESSION_AND_AV.md`](01_SESSION_AND_AV.md) §telephony — and the mismatch is still harmless, but
for a different and stronger reason: BRSF 63 does not claim codec negotiation, so the AG never
offers a codec and always opens plain CVSD narrowband regardless of what the SDP record advertises
about wide-band speech. **Updated 2026-09-04:** the wideband lever (`CARPLAY_HFP_WBS`) sends
`AT+BRSF=191` instead, at which point the SDP record's WBS bit and the wire finally AGREE — the
record needs no change either way, and with the lever off nothing here moves.)*

**What we do NOT advertise: `0x111F` or `0x1112`.** Those are the audio-GATEWAY sides of the two
profiles and belong to the phone. Claiming one would present the phone with a second gateway and
never satisfy a gate that reads `BluetoothProfile.HEADSET`.

The Rust reference implementation registers a wider set of dummy phone-like profiles (PBAP `0x112f`,
MAP `0x1132`, A2DP source, AVRCP target, PANU, NAP) purely to be recognised. **None of those has
been shown necessary here** and the four above are what the gate actually reads. Add more only
against a measured failure.

### 2c. The RFCOMM wire format

Fixed 4-byte header, both fields big-endian, then the protobuf payload:

```
[ length : u16 BE ][ message_id : u16 BE ][ payload : length bytes ]
```

### 2d. Message ids

```
WIFI_START_REQUEST    = 1     WIFI_VERSION_RESPONSE = 5     WIFI_PING_REQUEST  = 8
WIFI_INFO_REQUEST     = 2     WIFI_CONNECT_STATUS   = 6     WIFI_PING_RESPONSE = 9
WIFI_INFO_RESPONSE    = 3     WIFI_START_RESPONSE   = 7     WIFI_SETUP_INFO    = 11
WIFI_VERSION_REQUEST  = 4
```

Ids 1–7 are in both references. **8, 9 and 11 appear only in the Rust one** — treat the ping pair as
likely-required keepalive and `WIFI_SETUP_INFO` as unknown until observed.

### 2e. Messages (proto2)

```proto
message WifiStartRequest  { required string ip_address = 1; required uint32 port = 2; }
message WifiStartResponse { optional string ip_address = 1; optional uint32 port = 2;
                            required Status status = 3; }
message WifiInfoRequest   { }
message WifiInfoResponse  { required string ssid = 1; required string password = 2;
                            required string bssid = 3;
                            required WifiSecurityMode security_mode = 4;
                            optional AccessPointType access_point_type = 5; }
message WifiVersionRequest  { }
message WifiVersionResponse { required uint32 unknown_value_a = 1;
                              required uint32 unknown_value_b = 2;
                              optional string unknown_value_c = 3;
                              required uint32 unknown_value_d = 4; }
message WifiConnectionStatus { required Status status = 1; optional string error_message = 2; }
```

`Status`: `SUCCESS 0`, `UNSOLICITED_MESSAGE 1`, and negatives that are the actual diagnostic surface —
`NO_COMPATIBLE_VERSION -1`, `WIFI_INACCESSIBLE_CHANNEL -2`, `WIFI_INCORRECT_CREDENTIALS -3`,
`PROJECTION_ALREADY_STARTED -4`, `WIFI_DISABLED -5`, `WIFI_NOT_YET_STARTED -6`, `INVALID_HOST -7`,
`NO_SUPPORTED_WIFI_CHANNELS -8`, `INSTRUCT_USER_TO_CHECK_THE_PHONE -9`, `PHONE_WIFI_DISABLED -10`,
`WIFI_NETWORK_UNAVAILABLE -11`. Log the name, never the bare number.

`WifiSecurityMode`: `OPEN 1`, `WEP_64 2`, `WEP_128 3`, `WPA_PERSONAL 4`, `WPA2_PERSONAL 8`,
`WPA_WPA2_PERSONAL 12`, `WPA_ENTERPRISE 20`, `WPA2_ENTERPRISE 24`, `WPA_WPA2_ENTERPRISE 28`.
`AccessPointType`: `STATIC 0`, `DYNAMIC 1`.

> **Quirk worth knowing.** The C++ reference sends `security_mode = WPA2_ENTERPRISE (24)` for what is
> an ordinary WPA2-PSK AP, with a source comment that AAP uses different values here than the
> in-session WifiProjection channel does. Do not assume the obvious value is the accepted one; if the
> phone rejects the credentials, this field is the first thing to vary.

### 2f. Sequence

0. **The head unit becomes HEADSET-connected on the phone.** Either the head unit dials the phone's
   audio gateway (HFP `0x111F` or HSP `0x1112`) and completes a service-level connection, or the
   phone's `PhonePolicy` dials one of the head unit's own headset records. Until the phone's
   `BluetoothProfile.HEADSET` reports the head unit, nothing below happens at all — see §6b for the
   gate and §6d for both routes.
1. **The PHONE opens RFCOMM to the head unit's wireless-projection record** — UUID
   `4de17a00-52cb-11e6-bdf4-0800200c9a66`, channel 4, the record §2b advertises. gearhead is the
   CLIENT of that UUID (`createRfcommSocketToServiceRecord`, `ojk.java:31-35`) and hosts no server
   for it. On stock this lands **26 ms** after the SLC's last `OK`.
2. **Head unit speaks first** on that socket, unprompted: `WifiVersionRequest`, then
   `WifiStartRequest{ ip_address = <the AP-side IP>, port = <our TCP port> }`.
3. Phone replies `WifiVersionResponse`, and sends `WifiInfoRequest`.
4. Head unit replies `WifiInfoResponse{ ssid, password, bssid, security_mode, access_point_type }`.
5. Phone associates to the AP, then opens TCP to the advertised `ip_address:port`.
6. `WifiStartResponse` / `WifiConnectionStatus` carry the outcome; ping/pong may run throughout.
7. From the first byte on that TCP socket it is the **ordinary AA TLS session** — the wired engine,
   unchanged.

> **CORRECTION HISTORY, because this cost a milestone and the wrong version is still in git.**
> Step 1 originally read "phone opens RFCOMM to the advertised channel" — correct. On 2026-09-04 it
> was inverted to "head unit opens RFCOMM to the PHONE's wireless-projection service" on the
> inference that gearhead's `waitForHeadUnitConnected` timeout meant the phone was waiting to be
> dialled. It is not: what it waits for is the HEADSET state, and the phone hosts no
> `4de17a00-…` record at all — our own targeted search of the bench Pixel returns an empty
> attribute list (`AA-wireless-UUID search -> 2 bytes: 3500`). The original reading is restored,
> with step 0 as the precondition that was missing from it. The `attempt_aa` code written for the
> inverted version is deleted; the search is kept as a diagnostic only.

Stock's own ordering, from the capture (`aa_full_session_adapter_20260315.txt:442-607`), is exactly
steps 0→7: `hfpd` pages the phone, reads its AG record (`SDP: Supported features: 12f`), runs the AT
SLC, gets `AG …: Connected` — and 26 ms later `$$$ accept! AAP`, the phone opening the box's own
Android Auto channel, followed by `sendRFCOMMData type: 4` (WifiVersionRequest) from the box.

**The port is ours to choose.** It is carried in `WifiStartRequest`, not fixed by the protocol: the
C++ reference advertises 5000, the Rust one and field captures use 5288. Pick one, put it in config,
and do not hardcode it in two places.

## 3. What is reusable — now actually shared

The box already does every radio operation this needs, for wireless CarPlay. As of 2026-09-01 those
primitives are no longer private modules of that binary: they live in **`crates/bt-common`**, which
both daemons depend on. The alternative was a second copy, and two copies of a pairing agent drift.

| Module (now in `bt-common`) | What it already does | For wireless AA |
|---|---|---|
| `hci.rs` | controller bring-up, HCI command/event | unchanged |
| `rfcomm_uspace.rs`, `rfcomm.rs` | userspace RFCOMM | unchanged — AA's bootstrap rides RFCOMM too |
| `sdp_server.rs` | `bluetoothd`-less SDP server | **needs the AA record**; see below |
| `ssp_agent.rs` | pairing / Secure Simple Pairing agent | unchanged |
| `cloexec.rs` | fd hygiene | unchanged |

Deliberately left in `carplay-wireless` as genuinely CarPlay-coupled: `sdp_client.rs` (browses for
the iAP2 service — AA never needs it, since the phone browses *us*), `bt_bringup.rs` (sequences the
CarPlay bring-up), and all the session logic (`av`, `bt_driver`, `control`, `wifi_handoff`,
`mfi_local`, `box_identity`, `reconnect`, `arbiter_client`).

**The extraction was a move, not a rewrite.** The files moved verbatim; the only source edit was
widening `ssp_agent::state_dir` from `pub(crate)` to `pub`, because `control.rs` now reaches it
across a crate boundary. `carplay-wireless` keeps every `crate::hci::…` / `crate::ssp_agent::…` path
it had, via a re-export at its crate root. Verified against a pre-extraction build of the same
commit: **66 tests before, 66 after** (37 in `carplay-wireless` + 29 in `bt-common`), and every
behaviour-carrying string identical in the armv7 binary — `CARPLAY_HCI_BACKEND`, `CARPLAY_STATE_DIR`,
`CARPLAY_PAIRING_MODE`, `bt_link_keys`, `projection_policy.json`. The binary is **not** byte-identical
(573,632 → 575,432 B, +0.3%); that is codegen and metadata from the crate split, not a behaviour
change.

Two traps this extraction walked into, both worth remembering:

- **Do not cfg-gate the new crate to Linux.** The first cut did, on the reasoning that these are all
  `AF_BLUETOOTH` sockets. But they compiled on macOS as part of `carplay-wireless`, and
  `tools/run_tests.sh` runs `cargo test -p carplay-wireless` on the build host — gating took 29
  tests out of the host run and broke the build outright. Per-syscall gating lives inside the
  modules that need it (`cloexec.rs` already has a macOS branch).
- **Register the new crate in `tools/run_tests.sh`.** Its 29 tests used to run under the `wireless`
  line. An extraction that leaves them unregistered is exactly how a suite quietly shrinks.

The `CARPLAY_*` environment variable names inside these modules are retained on purpose: they are
the deployed contract on a shipping box, and renaming them would be a behaviour change dressed as a
refactor. New AA-side knobs get their own names (`AAW_*`).

The honest expectation stands and has now partly been paid: the radio half is close to free, and the
bootstrap half was the new work.

## 4. Bench

- **Phone:** the bench Pixel 10 (`frankel_beta`, Android 17 / API 37), gearhead 17.5.663204, on USB
  with adb active — the same phone the wired path was proven against.
- **Box:** the CPC200-CCPA on USB, normally in OCBM mode. `tools/ocbmcmd.py 'cmd'` gets a root shell
  over the OCBM console channel with no mode switch at all; switch to NCM only when file transfer is
  needed (`../ops/00_BUILD_AND_DEPLOY.md`).
- **Build host:** see the toolchain trap in §1 — box binaries need rustup's toolchain, not Homebrew's.

### Bluetooth control without touching the screen

`~/carlink/btctl` (built by a parallel effort, 2026-09-01) drives the phone's Bluetooth over adb with
no root and no screen taps, which is what makes an automated bootstrap loop possible at all:

```
btctl list | state <MAC> | scan [--timeout S]
btctl pair <MAC> [--pin P]     # createBond + poll-and-auto-confirm
btctl accept <MAC>             # grant a HEAD-UNIT-initiated pairing  <-- the one this workstream needs
btctl unpair <MAC> | unpair-all
btctl ui dump | tap <regex> | wait <regex> [s]
```

`btctl accept` is the relevant one: our box initiates, so the phone raises the consent prompt, and
this grants it without a human. Run `~/carlink/btctl/build.sh` once, and again after any phone wipe.

It works because `com.android.shell` (uid 2000) holds `BLUETOOTH_PRIVILEGED` and `BLUETOOTH_STACK`,
and a dex in `app_process` runs at that privilege with the full `BluetoothAdapter` API —
`cmd bluetooth_manager` exposes none of it. Verified here 2026-09-01: `list` answers correctly
against the live phone. **Not yet verified: a real head-unit-initiated pairing**, for the obvious
reason that the head-unit side is what we are building. Treat `accept` as untested against real
hardware until this workstream tests it.

### The debugging property that changes how this gets built

**USB adb stays up through a wireless AA session.** Wired AA switches the phone into AOA and USB adb
drops, which is why the wired bring-up was so much guesswork off captures. Wireless does not touch
USB, so gearhead's own logcat can be watched *live, during a session* — the single biggest tooling
difference between the two paths, and worth exploiting rather than reproducing the wired workflow.

**adb cannot start a projection session.** The head unit initiates over BT RFCOMM; there is no
`am start` for it. For a headless full-session loop the substitute is Google's DHU: enable "Start
head unit server" in the phone's Android Auto developer settings, `adb forward tcp:5277 tcp:5277`,
and run `desktop-head-unit`. That exercises the AA session but **not** the bootstrap — DHU comes in
over TCP and skips the whole Bluetooth handover this document is about. It is an oracle for the
session, not for §2.

### Arbitration

Wireless AA is a *fourth* owner. §2 row 8 of [`02_ARBITRATION.md`](02_ARBITRATION.md) called it
non-existent; that row is the one this workstream closes. Wireless CarPlay and wireless AA contend
for the same radios — and now, since the 2026-09-01 extraction, for the same code in
`crates/bt-common`. The owner flag has to mediate them the way it already mediates the wired pair.
Design it before the first packet flows: this is the part most likely to produce a subtle bug, and
`ProjectionOwner::WirelessAa` existing is not the same as the two arms actually standing down for
each other.

## 5. Open questions

Answered above and no longer open: whether the hardware can do it (§2a — stock firmware does),
the message set, the framing, the UUID, and who speaks first.

**Who CONNECTS — settled 2026-09-04 (second pass), and it is not who speaks first.** These are two
different questions and conflating them cost this workstream a milestone. **The PHONE connects**: it
opens the head unit's own `4de17a00-…` RFCOMM record on channel 4, and then the HEAD UNIT speaks
first on that socket (§2f steps 1–2 — that second half was always right). What the phone waits for
before doing so is not a timer and not a dial from us: it is its own `BluetoothProfile.HEADSET`
reporting the head unit connected (§6b). So the head unit's job is to be a headset, and the
projection socket arrives on its own.

A middle version of this section claimed the head unit dialled an AA-wireless record ON THE PHONE.
That is refuted three ways: gearhead calls `createRfcommSocketToServiceRecord` for that UUID
(`ojk.java:31-35`, i.e. it is the client), it registers no server for it, and the bench Pixel's SDP
has no such record (`AA-wireless-UUID search -> 2 bytes: 3500`). The `attempt_aa` code is gone.

Still genuinely unknown, in the order they will bite:

- ~~**Which dummy BT profiles this phone actually requires**~~ — largely answered: it is not "dummy
  profiles" in general, it is a HEADSET-class profile specifically, because that is the only thing
  `BluetoothProfile.HEADSET` reads (§6b). We advertise HFP HF and HSP HS and nothing else new.
  Whether either alone suffices, and whether the record alone is enough without a completed SLC, is
  still open — measure before adding PBAP/MAP/A2DP.
- **Whether a prior wired/BT pairing is required** before a cold wireless association. This decides
  whether wireless AA is reachable standalone or only as a handover. Partial answer 2026-09-04
  (Pixel 10, gearhead log): with the phone already BONDED, the box paging it is enough to wake the
  setup service — on `ACL_CONNECTED` gearhead logs `Wireless projection is available`, `previously
  known to have Android Auto UUID`, starts `WirelessSetupSharedService`, and reaches
  `WIRELESS_SETUP_CDM_AND_HU_TRACKER_READY` in ~100 ms. It then logs
  `WIRELESS_SETUP_HU_NOT_CONNECTED_TIMEOUT` 5 s later, and **that timeout is the HEADSET gate, not a
  dial we owe the phone** (§6b). The box therefore raises a headset link on the same SDP
  conversation (`reconnect::attempt_headset`), and the 30 s ACL hold
  (`CARPLAY_ACL_HOLD_SECS` / `/tmp/acl_hold_secs`, `0` disables) is narrowed to a bond exposing
  NEITHER iAP2 nor any audio gateway. The cold, unbonded case is untested.
- **Which route to the headset gate a given phone takes** — HFP with the AT SLC (what stock does) or
  HSP with none (what both public dongles do). Both are implemented and tried in that order;
  `CARPLAY_AA_HEADSET_PATH=hfp|hsp` (or `/tmp/aa_headset_path`) forces one for a bench run. Which
  one this Pixel actually accepts is unmeasured — only stock's HFP success is evidence, and that was
  stock's stack, not ours.
- ~~The right `security_mode` value~~ — answered: **8 = WPA2_PERSONAL**, field-proven (stock box capture; the 24/WPA2_ENTERPRISE attempt never associated). `crates/vendor/wireless/src/main.rs` sent 24 until 2026-09-04; fixed.
- **AP or Wi-Fi Direct?** §2e's `WifiInfoResponse` carries `ssid`/`password`/`bssid`, i.e. an
  ordinary infrastructure AP, and that is what the reference dongles run. But the phone is reported
  to hold a Wi-Fi Direct group up concurrently with STA during a session, and Wi-Fi Direct support
  is an open request against `WirelessAndroidAutoDongle`. Unverified either way here. It matters:
  if the phone expects P2P, the AP half of this design is wrong.
- **Whether the box WLAN sustains AA video concurrently with the BT link** on this silicon. Wireless
  CarPlay's measurements are the closest prior, not a substitute for measuring.
- **`WIFI_SETUP_INFO` (11) and the ping pair (8/9)** — required, optional, or vestigial.
- ~~**One bridge or two.**~~ — **DECIDED 2026-09-04: one bridge, one process, two transports.**
  `aa-bridge` grew a wireless arm rather than a second daemon. The AOAP-specific setup separated
  cleanly because it is entirely *setup* — control transfers, re-enumeration, interface claim,
  endpoint discovery — and none of it appears downstream of "I have two byte streams". What is
  shared is the arbitration, the app-side socket and the copy loop; what is NOT shared is the pump
  itself, which stays two implementations on purpose (§6c). One process rather than two was forced
  by the supervisor: `arm_aa`'s guard is `pgrep aa-bridge` by NAME, and `pgrep` is an unanchored
  match, so a second binary called `aa-bridge-wl` would satisfy that guard and silently suppress the
  wired launch.

## 6. Future test point — telephony as its own stream (NOT in this milestone)

**Scope guard: telephony is explicitly out of the wireless AA milestone.** Wireless is done when the
existing wired session runs over the BT-bootstrapped TCP socket. Do not let this section grow into it.

Recorded here because wireless brings up the BT stack anyway, so the experiment becomes cheap for the
first time — the radio, the pairing agent and the SDP server will all be running.

**The premise.** AA carries call audio over Bluetooth HFP, not over the projection link — Google's
Head Unit Integration Guide requires it, and the stock firmware complied by running `hfpd`. Full
evidence and citations in [`01_SESSION_AND_AV.md`](01_SESSION_AND_AV.md) §1, "Telephony". But the
protocol does define `AUDIO_STREAM_TELEPHONY = 4` and the reference stack has a
`MEDIA_SINK_TELEPHONY_AUDIO` channel, and the guide's own wording is *"current implementations use
Bluetooth"* — which is a statement about today, not about what the wire refuses.

**The question.** Will gearhead open a telephony sink if a head unit declares one — giving CarPlay-like
in-band call audio over Wi-Fi — or is the head-unit side of that path simply absent?

**The experiment.** Cheap, self-contained, and safe to run against the working wired path:

1. Add a fourth entry to `AACapability.audioSinks` with stream type 4, mirroring the MEDIA sink's
   shape. The sink table is already single-sourced, so declaration and playback stay consistent.
   Landed 2026-09-03 as the `AA_TELEPHONY_SINK` lever (off by default; channel 2, 16 kHz mono like
   GUIDANCE, logs `telephony sink DECLARED (experiment AA_TELEPHONY_SINK)` at service discovery and
   shouts if the phone ever opens the channel) — **untested**, step 2 onwards still owes the answer.
2. Bring up a call with the phone paired for HFP and watch whether a channel-open request ever
   arrives for it. `AA_TRACE_UNHANDLED` already surfaces messages we do not route.
3. If it opens, check what actually flows: sample rate, direction, whether the mic channel doubles as
   the uplink, and whether HFP goes silent or duplicates.
4. Either way, record the result here and close the question.

**RESULT 2026-09-04 — CLOSED, and worse than predicted: the phone rejects the whole service set.**
Run wireless (the wired arm was not available that day) with `AA_TELEPHONY_SINK=1`: the phone
closed the transport **0.36 s after our SERVICE_DISCOVERY_RESPONSE** carrying the type-4 sink, on
two consecutive attempts (`stream ended` before any CHANNEL_OPEN), and its GAL layer then stopped
answering our VERSION_REQUEST until gearhead was force-stopped. Without the lever the same phone
opened channels and streamed video 1 s after discovery. So the sink never opens because the phone
refuses to talk to a head unit that declares it — exactly the guard-rail failure mode below. The
lever stays in the code as documentation of the experiment and must never ship on. The phone-side
reason was not captured (the logcat transport died during the run); the app-side evidence is
sufficient to close the question.

**Predicted outcome was: it does not open.** Recorded before the run so a null result read as a
settled question rather than a failure. Static analysis supports the prediction — gearhead 17.5.663204 knows the
*name* `AUDIO_STREAM_TELEPHONY`, but nothing in the decompiled tree opens such a sink.

**Guard rails.**
- Do not declare it in the shipping SD response until it is proven. A service set the phone dislikes
  is rejected *whole* — that is exactly how the missing mic source produced
  `CAR.SERVICE Critical error 2/24`. Put it behind an env lever, off by default, alongside
  `AA_NO_TOUCH` / `AA_SKIP_AUDIO_ACK` / `AA_LEGACY_VIDEO`.
- Run it wired first. Wireless adds a transport variable to an experiment that already has enough.
- A second phone-side check worth doing before touching our code at all: re-examine gearhead for a
  channel-open path keyed on stream type 4, not merely the enum name.

## 6b. Implementation plan (agreed 2026-09-01, after two 3-agent review rounds)

**Shape: wireless AA is served by the EXISTING `carplay-wireless` daemon, additively.** Not a second
binary. Wireless CarPlay is device-proven and must not regress; every step below is ordered so it
stays working at each commit. Governed by the first-come-first-served rule in
[`02_ARBITRATION.md`](02_ARBITRATION.md) §0.

Why one process, stated correctly: an earlier rationale said "one process avoids contention for both
well-known PSMs". Only half true. On the kernel RFCOMM backend the kernel owns PSM 3 and hands out
server channels, so PSM 3 was never contended. **PSM 1 (SDP) is the exclusive one** — `sdp_server.rs`
binds it with a hand-rolled L2CAP socket and no `SO_REUSEADDR`, so a second daemon gets `EADDRINUSE`
and is silently never advertised.

| Step | Work | Risk | Gate |
|---|---|---|---|
| 1 | ✅ **DONE** — `aa-wireless` binary → library (`proto`, `wpp`, `run_bootstrap`) | none | full suite green, no behaviour change |
| 2 | ✅ **DONE** — `wireless-aa` arm + self-heal in `session_supervisor.sh`; `aa-bridge` stand-down widened to "anyone else owns it" | low | full suite green; `preempt_wireless_for_wired` verified already safe (gated on the CarPlay-only `carplay_transport` flag AND Hot-Handover, default off) |
| 3 | ✅ **DONE** — SDP multi-record server: service table, real UUID matching in all three handlers, size-safe outer sequence, concurrent clients | **highest** | 42 tests (was 29); the pre-existing search test kept **unchanged** and still passing |
| 4 | ✅ **DONE** — AA service registered; second RFCOMM accept thread on channel 4 | medium | CarPlay's channel-1 loop has **zero deleted lines** in the diff |
| 5 | ✅ **DONE** (folded into 4) — AP credentials from the live AP | low | refuses to serve a `wpa_psk`-only AP |

### Step 1 — library

Add `src/lib.rs` exposing `proto`, `wpp` and `run_bootstrap`. Keeping only `proto`+`wpp` would force
`carplay-wireless` to reimplement the framing loop that is already written and tested. Drop the
`[[bin]]` and the `build.sh` standalone-binary lines. `run_tests.sh` needs no change and
`ocbm_install.sh` never had an entry.

### Step 2 — flags, before anything can run safely

`session_supervisor.sh`'s `projection_owner()` has **no `wireless-aa` arm and no default**, so it
returns `""` and the supervisor believes the box is idle during a live AA session — then `pkill`s
the daemon on the next wired plug or host edge. The Rust and shell readers of the same file disagree
today, and the shell holds the kill switch. Add the arm plus the liveness self-heal that `wired-cp`
and `wired-aa` already have.

`aa-bridge` stands down only on `owner().is_carplay()`, which is false for `WirelessAa` — so a wired
Android phone stomps a live wireless AA claim. Under first-come-wins the predicate is "owner is
neither `None` nor mine", regardless of protocol.

### Step 3 — SDP multi-record (the hard one)

`sdp_server.rs` becomes a handle table (`0x00010000` iAP2, `0x00010001` AAP) with real UUID matching
in **all three** handlers. Matching only `handle_search_attr` is not enough: `handle_service_search`
answers PDU 0x02 and `handle_service_attr` rejects any handle but the one hardcoded constant, so an
AA search would still resolve to iAP2's handle and the phone would read channel 1.

Rule: AA UUID present and iAP2 absent → AA. `0x1002` PublicBrowseGroup → **both**. Anything else,
including a parse failure → iAP2, exactly as today.

Three corrections review forced:

- **"Byte-identical CarPlay SDP" is DROPPED as a goal.** A correct PublicBrowseGroup browse must
  return both records, taking the response from 91 to ~196 bytes. Serving a correct browse and
  keeping CarPlay's bytes identical are mutually exclusive; every production head unit serves
  several records, so serve both and say so rather than discovering it in test.
- **`DE_SEQ16` for the combined blob, now.** Two records leave 59 bytes under `put_seq8`'s 255-byte
  ceiling. Tripping it asserts, and the release profile is `panic = "abort"` — so an Android-side
  cosmetic edit (a longer service name, a third record) would abort `carplay-wireless` and take
  CarPlay down with it.
- **Concurrent SDP clients.** `listen(fd, 1)` plus run-to-completion `serve_client` is safe only
  while iPhones are the only browsers. A phone may legitimately idle on an open SDP channel, so once
  an AA record is advertised an idling Android phone can hold PSM 1 while an arriving iPhone's
  browse goes unserved — which is exactly the documented CarPlay death ("browses SDP, finds no iAP2
  service, disconnects"). **This is a regression of a proven path introduced by adding AA**, and it
  must be fixed in the same change, not after.

Hedge on stock's larger service set: add SerialPort `0x1101` to the AA record's `ServiceClassIDList`
(openauto does; our encoder emits the UUID128 alone).

### Step 3 as built (2026-09-01)

`Service { handle, uuid128, record, name }` table; iAP2 keeps handle `0x00010000`, AA gets
`0x00010001`. `run()` is unchanged for existing callers and registers iAP2 alone; `run_services()`
takes an arbitrary set. **AA is not registered yet — that is step 4**, so this change ships the
machinery without altering what the box currently advertises.

- `parse_search_pattern` decodes UUID16/32/128 from a `DE_SEQ`, expanding short forms through the
  Bluetooth Base UUID (a raw `0x1101` compared against a 128-bit class would otherwise never match).
- `select_services`: PublicBrowseGroup → all; a matching service UUID → those; **anything else,
  including an unparseable pattern → the first service**. That fallback is what keeps every request
  an iPhone has ever made answered exactly as before.
- `wrap_attr_lists` emits an 8-bit outer length while the body fits one and a 16-bit length
  otherwise — so the single-record response stays byte-identical, and a third service can never
  overflow into the assert that `panic = "abort"` would turn into a CarPlay outage.
- **Concurrent clients.** `serve_client` now runs on a scoped thread, capped at `MAX_SDP_CLIENTS`.
  The old accept-and-serve-to-completion loop with `listen(fd, 1)` was safe only while iPhones were
  the only browsers; an idling Android phone would have held the single slot while an arriving
  iPhone's connect went unserved, which is precisely the "browses SDP, finds no iAP2 service,
  disconnects" failure. Adding AA would have introduced it, so it is fixed in the same change.
- `ServiceRecord::extra_class_uuid16` is the §6b hedge, **off by default** because off matches
  stock — whose record builders emit the 128-bit class alone and which does wireless AA on this
  hardware. openauto disagrees; only stock is proven here.

Footprint: `carplay-wireless` 575,448 → 582,584 B (+7 KB), against ~3.4 MB free.

### Step 4 — second accept thread

A separate thread running `accept_one(4, …)` mirroring `rfcomm_handle`'s shape, sharing
`session_active`/`ctrl` only through `SessionClaim`, **touching zero lines of the channel-1 loop**.
Kernel RFCOMM multiplexes DLCs over one L2CAP session itself. Channel 4 must drop cleanly on claim
failure exactly as channel 1 does.

**Kernel backend only.** `rfcomm_uspace` (opt-in via `CARPLAY_RFCOMM_BACKEND=userspace`, used on the
Pi) cannot serve two channels: `open_dlc` returns on the first matching SABM and sends `DM` to any
other channel's, so an AA thread would actively reject CarPlay's DLC on a shared session, roughly
half the time. AA stays excluded from that backend until it becomes a real multi-DLC multiplexer.

### Steps 4 + 5 as built (2026-09-01)

The SDP thread now registers **both** services; a second thread runs `accept_one(4, ..)` alongside
the untouched channel-1 loop. Both contend for the same `session_active` slot through
`SessionClaim`, which is the first-come-first-served rule expressed as one `compare_exchange`.

- **Two independent blocking accepts, not a shared poll.** The kernel multiplexes DLCs over one
  L2CAP session itself, so neither loop can starve the other, and the proven CarPlay path is not
  edited at all.
- **Kernel backend only.** The AA thread returns immediately under `CARPLAY_RFCOMM_BACKEND=userspace`
  with a log line, because `rfcomm_uspace::open_dlc` answers any SABM for a channel it is not
  serving with `DM` — a second accept loop there would reject CarPlay's own DLC about half the time.
- **AP credentials come from the running AP**: `wifi_handoff::read_hostapd_ap_config()` — the same
  source CarPlay's own 0x5703 handoff uses — plus `RADIO_WLAN_MAC` from `/tmp/radio_caps` for the
  BSSID. Read from the caps file rather than `/sys/class/net/wlan0/address` because the interface
  name is an insmod parameter on this hardware, not a constant. `AP_IP` is now `pub(crate)` in
  `av.rs` so the address has one definition.
- **`security_mode` is 8**, field-proven from the stock box's own working session.
- **The owner flag is claimed after the phone opens the channel**, via `aa_wireless::claim_owner`,
  which also stands down if another transport already owns the box and releases ours-only.

Footprint: `carplay-wireless` 582,584 → 599,264 B. Still ~5.5× under the free-space budget.

**This is the first change that alters what the box advertises**, so it is the first that needs a
device test: an iPhone must still find iAP2 and project, before anything Android is attempted.

### DEVICE RESULT — CarPlay regression gate PASSED (2026-09-01)

The change that mattered was steps 3+4: the box began advertising a SECOND SDP record and serving
SDP clients on threads. The gate was whether wireless CarPlay still works, tested BEFORE any Android
Auto attempt so that a failure could not be confused with an AA bring-up problem.

- Bench: iPhone 18,4 / iOS 27.0, wired to the Mac for logging only; the CarPlay session itself is
  wireless. Box running the new `carplay-wireless` (599,312 B) and the updated supervisor.
- Result: **wireless CarPlay connected and ran.** `carkitd` reports `CarPlay session is active`.
- **Zero** identification rejects across the whole capture — no `iapreject`, no
  `Identification info rejected`, no `RequiredInfoMissing`, no `OptionalMsgNotValidWithoutRequiredMsgs`.

So two SDP records and a threaded SDP server do not disturb iOS. The concern that a
PublicBrowseGroup browse returning two records might upset the iAP2 flow is answered for iOS.

**Not** answered by this test: whether the phone ever *saw* the Android Auto record. `bluetoothd`
does not log SDP browse RESULTS — the "Adding SDP Legacy record" lines in its log are the phone
registering its OWN records. Confirming what the box actually served needs a box-side or `btmon`
capture, not the iOS syslog.

#### Log filtering, for the next run

`idevicesyslog -p` matches the process name PREFIX, so `-p accessoryd` also captures
`accessoryd(RunningBoardServices)` — 162k lines in two minutes, almost all of it app-lifecycle noise.
Add `-M RunningBoardServices` (and `-M HMFoundation`) to cut it. The signal lines are few and
specific: `carkitd` session state, and anything matching `iapreject|Identification info rejected`.

### DEVICE RESULT — the phone now accepts the box as wireless-AA capable (2026-09-01)

**`CAR.BTCapsStore: AAW status (SUPPORTED)`**, up from `UNKNOWN_AND_DONT_TRY_RFCOMM`.

#### What was actually wrong

Android's discovery makes exactly two SDP searches — DID `0x1200`, then **L2CAP `0x0100`** — and never
uses PublicBrowseGroup. `select_services` compared pattern UUIDs only against each service's
`ServiceClassIDList` UUID, so the L2CAP search matched nothing, fell through to the
single-service fallback, and returned the iAP2 record alone. Measured on the phone, before the fix:

    [BR/EDR UUIDs]: 00000000-deca-fade-deca-deafdecacaff

and after:

    [BR/EDR UUIDs]: 00000000-deca-fade-deca-deafdecacaff 4de17a00-52cb-11e6-bdf4-0800200c9a66

(`adb shell dumpsys bluetooth_manager`, bonded box entry.) Gearhead gates on that cached set, so with
one UUID it refused to even request SDP.

#### The fix

The "return every service" arm now covers `{0x1002 browse, 0x0100 L2CAP, 0x0003 RFCOMM}` — the UUIDs
every record necessarily carries and which therefore cannot discriminate between services — plus an
explicit empty-pattern guard, because `all()` over zero UUIDs is vacuously true and a zero-length DES
would otherwise have returned everything.

This is what the only known-good oracle does. The stock CCPA does not implement SDP at all: it
registers records with BlueZ (`sdp_record_register`, `sdp_set_access_protos`) and `sdpd` answers,
matching against the record's ACCUMULATED UUID set — which `sdp_set_access_protos` fills with exactly
L2CAP and RFCOMM.

The catch-all fallback was KEPT. Under the new rule Android's search matches, so it never reaches the
fallback; removing it would have changed behaviour only for iOS-side patterns nothing matches — pure
downside against a device-verified path.

#### Three things that were believed and were wrong

- **`sdpu_find_most_specific_service_uuid: Bad Service Class ID list attribute` is NOT the bug.**
  That AOSP metrics path only recognises a 16-BIT UUID as the class list's first element, so ANY
  record whose class list holds a UUID128 emits it — including a correct AA record, and including
  stock's. **It will still appear after the fix. Do not use its absence as a success criterion.**
- **The record was never malformed.** All 89 bytes were walked element by element against the
  data-element rules: correct, attribute IDs strictly ascending, body length exact.
- **logcat could not have told us which.** Gearhead prints the identical
  `"valid UUIDs but doesn't contain AA UUID"` string for `UUID_ARRAY_IS_EMPTY` and
  `UUID_ARRAY_HAS_OTHER_ITEM`. Only `dumpsys bluetooth_manager` distinguishes them, and that is the
  measurement that settled it.

#### Retest hygiene (learned the hard way)

- Gearhead takes a `DONT_REQUEST` branch and **never calls `fetchUuidsWithSdp()`** — `getUuids()` is
  whatever bond-time SDP cached. **Unpair and re-bond after every push**, or the phone serves the
  stale list forever.
- A per-MAC `NOT_SUPPORTED` result is cached stickily. `adb shell pm clear
  com.google.android.projection.gearhead` clears it — but it also resets the app's preferences.
- `dumpsys bluetooth_manager` REDACTS MAC addresses; identify the entry by name (`CarLink-0a6c`).

#### Also hardened in the same change

`serve_attr_blob` now caps each chunk at 600 bytes regardless of the client's requested maximum. The
server never reads the negotiated L2CAP MTU, and the two-record response grew from 97 to ~205 bytes;
an over-large PDU would fail `EMSGSIZE` and close the channel — which an iPhone reads as "no iAP2
service", and disconnects.

### WHERE THIS STOPPED — a HEADSET profile is required, proven three ways (2026-09-04)

**Read the correction history before the evidence, because this section has been rewritten twice.**
It originally concluded "HFP is required" from an experiment. On 2026-09-04 it was retitled
`CORRECTED` and that conclusion withdrawn, on the inference that the phone hosted a
wireless-projection record we simply had not dialled. **That inference was wrong and the original
conclusion is restored.** The measurements never changed; only what they were read to mean did.

Three independent sources now say the same thing, which is why this is no longer an inference:

1. **gearhead 17.5, decompiled.** The wireless-setup gate is
   `BluetoothProfile.HEADSET.getConnectedDevices().contains(headUnit)` — `pcl.java:80`, with
   `kzt.java:56-64` and `pco.java:24-29` as the state mapping, and `ozb.java:139` widening it to
   `getDevicesMatchingConnectionStates({CONNECTED, CONNECTING})`, so it passes as soon as the
   phone's `HeadsetService` STARTS the connection. Failing it emits
   `WIRELESS_SETUP_FAILED_TO_START_NO_HFP_FROM_HU_PRESENCE`. On success the PHONE opens
   `createRfcommSocketToServiceRecord(4de17a00-52cb-11e6-bdf4-0800200c9a66)` toward the head unit
   (`ojk.java:31-35`) — it is that UUID's client and never its server.
2. **The stock CCPA, with this same Pixel** (`aa_full_session_adapter_20260315.txt:442-607`). `hfpd`
   (nohands, HF role) pages the phone, SDP-reads its AG record (`SDP: Supported features: 12f`),
   opens RFCOMM to the AG channel and runs, in order: `AT+BRSF=63`→`+BRSF: 879`,`OK`; `AT+CIND=?`;
   `AT+CMER=3,0,0,1`; `AT+CLIP=1`; `AT+CCWA=1`; `AT+CHLD=?`→`+CHLD: (0,1,2,3)`; `AT+CIND?`→
   `+CIND: 0,0,0,0,0,5,0`,`OK`. No SCO *in that capture*, no codec negotiation, no `AT+BIND`. (The
   absence of SCO there is an absence of a CALL during the capture, not a property of the stock
   design — `hfpd` is a full HFP unit and stock carried call audio on this hardware. Do not read
   this line as evidence that SCO is unnecessary.) **26 ms after that last
   `OK` the phone connects to the box's own AAP RFCOMM record** and the bootstrap runs
   4→5→1→2→3→7→6.
3. **The public dongles** (aa-proxy-rs, WirelessAndroidAutoDongle) register an HS/HF-class record and
   `connect_profile(HSP_AG)` toward the phone as the nudge, with no AT dialogue at all — a second,
   cheaper route to the same gate (§6d).

And our own bench negative: the bench Pixel's SDP has **no** `4de17a00-…` record
(`AA-wireless-UUID search -> 2 bytes: 3500`), and zero RFCOMM frames were ever exchanged in the
2026-09-04 session, because the phone's cache held only our two custom UUIDs and nothing
headset-class to auto-connect to.

#### The gate, read out of gearhead itself

`pco.hT` (decompiled, `jadx_out2/sources/defpackage/pco.java:22-29`) is what gates it:

    pcq h = kzt.h(state, aczv.be());
    if (!requireProfile ? (h == pcq.c || h == pcq.d) : (h == pcq.d)) -> proceed

and `kzt.h` (`kzt.java:56-63`) over `HuBluetoothState(aclConnected, hfpConnected, a2dpConnected, isBonded)`:

    if (hfp || a2dp)      return pcq.d;   // CONNECTED_WITH_PROFILE
    if (bonded || !flag)  return acl ? pcq.c : pcq.b;

We are bonded, so with no HFP and no A2DP this can only ever return `pcq.c` or `pcq.b` — **never
`pcq.d`**. Whether `pcq.c` is accepted depends on `requireProfile`, a server-side Phenotype flag not
readable from the APK. The experiment below settles it empirically.

#### The experiment that settled it

Two worlds were consistent with everything observed: either `requireProfile` was false and merely
holding the link was enough, or it was true and a profile is mandatory.

`reconnect::attempt` was pausing on the "peer has no iAP2 service" arm to hold the ACL open
(`sdp_client::hold_acl`, lever `/tmp/acl_hold_secs`, OFF by default). Result:

| | before | with the hold |
|---|---|---|
| ACL lifetime | 2.26 s | **17.3 s** |
| `waitForHeadUnitConnected timeout` | every cycle | **still every cycle, at 5.06 s, with the link UP** |
| `Creating rfcomm socket ... 4de17a00-…` | 0 | **0** |

**`requireProfile` is TRUE**, and holding the ACL is nowhere near sufficient. Time is not what the
phone is waiting for; a headset profile is. That is the original conclusion, and it stands.

#### A real defect found on the way — CLOSED 2026-09-04

Our CarPlay `reconnect` loop pages every bonded phone every ~66 s, SDP-queries it for the iAP2
service, and — finding none, because an Android phone has none — returned immediately, dropping the
ACL it just raised. That page is what wakes gearhead's setup service, and we then hung up on it
2.3 s later. Box side logged `DEVICE_DISCONNECTED reason=0x02` (terminated by local host).

`sdp_client::query` now asks the same SDP channel for the two audio-gateway UUIDs, and
`reconnect::attempt_headset` connects out to whichever comes back — so the link is no longer
dropped, it becomes the headset link the gate needs. (An intermediate fix searched for
`4de17a00-…` on the phone and dialled that; it never matched, because no such record exists there.)

#### A second real defect, found in the Pixel's own HCI snoop — CLOSED 2026-09-04

`sdp_server::select_services` answered a search that matched NOTHING with the iAP2 record. In the
snoop the phone asked, after bonding, for PnP Information (`0x1200`) and Phonebook Access Client
(`0x112E`) and got "Wireless iAPv2" both times — a protocol lie that makes the phone cache a service
class we do not implement, and which with four records in the table would have handed a phone the
wrong record for `0x111E`. A well-formed pattern that matches nothing now returns nothing (a
zero-length handle list, or an empty `35 00` attribute list). The fallback is kept for an
UNPARSEABLE or empty pattern only — that is our inability to read the request, not a genuine miss,
and iOS depends on the browse-group `0x1002` / L2CAP `0x0100` arms which are untouched.

#### Ruled OUT with evidence — do not re-investigate

- **Class of Device** — byte-identical to stock (`0x200408`), and the phone sees it.
- **EIR** — functionally identical to stock; **neither** advertises the AA UUID. Not a discovery path.
- **Device ID / PnP `0x1200` record** — stock registers none and works.
- **The projection records themselves** — all 89 bytes walked against the data-element rules; correct.
  The phone parses them and caches both UUIDs.
- **Time / ACL lifetime** — the experiment above. The phone is not waiting for a timer.
- **A head-unit-initiated dial of `4de17a00-…` on the phone** — the phone hosts no such record, and
  gearhead is that UUID's client. Refuted with a targeted SDP search and the decompiled call site.
- **`sdpu_find_most_specific_service_uuid: Bad Service Class ID list attribute`** — metrics noise.
  That AOSP path only recognises a 16-bit UUID as the class list's first element, so ANY UUID128-only
  record emits it, including stock's. **It will not disappear when things work.**
- **CDM association** — already present (`CDM confirmed wireless device appeared. ID = 2`).

The verification string to watch is gearhead's own
`Creating rfcomm socket for device: <phone-bdaddr> and uuid: 4de17a00-52cb-11e6-bdf4-0800200c9a66`
(tag `GH.WIRELESS.BT`). It was at zero for every run so far and is the single line that says the gate
opened. On the box side the line before it is
`AA: HFP hands-free link up with the phone (SLC in <n> ms) — waiting for it to open our Android Auto channel`.
(The bench phone's Bluetooth address was written out in full in this file until 2026-09-04 and is
redacted above; it is still in this file's git history.)

## 6c. The transport pump, as built (2026-09-04)

The half §2f leaves after the phone associates: something has to answer the endpoint the bootstrap
advertised. It is `aa-bridge`, given a second arm.

**Where.** `ccpa/aa-bridge/src/wireless.rs` (listener, owner policy, TCP<->TCP session),
`pump.rs` (the copy loop and the claim decision — pure, host-tested), `appport.rs` (the single
acceptor for the app side). `main.rs` gained a `--wireless` flag and the resident loop; the AOAP
code in it is unchanged. `tools/session_supervisor.sh` gained `arm_aa_wireless` (called from
`wireless_up`) and `aa_bridge_wireless_down` (called from `wireless_down`).

**One definition of the endpoint.** The address is `box_common::net::AP_IP`, hoisted there from
`av.rs` on 2026-09-04; `aa-wireless`'s `AAW_IP` default now reads the same constant, and `av.rs`
re-exports it so `carplay-wireless` keeps its `crate::av::AP_IP` path. The port is
`aa_wireless::DEFAULT_PORT`, which `aa-bridge` now depends on the crate for. What the bootstrap
puts in `WifiStartRequest` and what the pump binds are therefore literally the same two symbols —
§2f's "do not hardcode it in two places", enforced by the compiler rather than by review.

**It binds the AP address, not `0.0.0.0`,** with a retry loop for the seconds before
`radio_hal.sh wifi_ap_on` has put the address on the interface. A wildcard bind would also expose
the raw, unauthenticated AA stream on `ncm0`, which is the USB link the macOS app and every bench
tool sit on.

**Two pumps, not one.** The wired pump is bound to usbdevfs bulk ioctls on a raw fd and to a
device-node watchdog; the wireless one is two TCP sockets. They share `pump::copy_stream` — the
loop, the 16 KiB granularity, the once-a-second per-direction totals — and nothing else. Forcing
the wired one through a `Read + Write` shim would have been a refactor of the one path that ships,
for no benefit; §5's "only if the AOAP-specific setup separates cleanly" is satisfied by sharing the
arbitration and the loop, not the I/O.

**The app-side socket is brokered, and that is the subtle part.** Both transports need the SAME
`:5277` client: the app opens exactly one CH_IP relay, after it sees `CT_PROJ_MODE`. Two `accept()`
callers on one listener would race for it, and there is an interleaving where the WRONG one wins —
the wired arm sits in its unclaimed wait polling `accept()` every 250 ms, a phone finishes the
bootstrap, `ocbmd` emits `PM_WIRELESS_AA` within 500 ms, the app connects, and the wired arm takes
it up to 250 ms before its own owner poll would have parked it. It then spends ~6 s failing to find
a phone on USB and drops the socket. So there is one acceptor thread and one queue, and the take is
gated on an in-process intent flag the wireless arm sets BEFORE it writes the owner flag. That
ordering is what a shared file cannot give you: read-then-write on `/tmp/projection_owner` is a
TOCTOU no matter how carefully it is read.

**Owner policy** (`pump::decide_wireless_claim`, unit-tested): idle → claim `wireless-aa`; ALREADY
`wireless-aa` → adopt, because `carplay-wireless` claims it at the end of the bootstrap and
deliberately holds it across the association; anything else → refuse and close, including
`wired-aa`. Released at session end whenever the flag still reads `wireless-aa`.

**Known limit, stated rather than hidden.** `release_owner_if_ours` is ours-only by TOKEN, and this
process and `carplay-wireless` write the same `wireless-aa` token — neither can tell its own claim
from the other's. `carplay-wireless`'s session teardown can therefore clear the flag under a live
TCP session here. It is bounded: that teardown means the radio is going away, so the session is over
anyway. Fixing it properly means putting a pid or a lock in the flag file, which changes a format
three daemons and the shell supervisor parse.

**`--wireless` makes the process resident**, and it has to. The listener must be up whenever the box
is discoverable, but `arm_aa`'s `pgrep aa-bridge` guard will not relaunch a process that is still
alive — so the resident bridge runs the wired arm's precondition gate itself
(`main::wait_for_wired_work`, the same conditions `arm_aa` applies in the shell) and parks instead
of exiting. Without `--wireless` the lifecycle is byte-for-byte what it was: exit, and let `arm_aa`
relaunch. The wired arm's "exiting" log lines are kept verbatim in both modes so existing greps
still match; in resident mode a following line says what actually happened.

**A hazard the wired path does not have.** A phone that leaves Wi-Fi sends no FIN and no RST, and AA
is phone-driven, so both directions of the pump would park in a blocking read forever — holding
`wireless-aa`, which stands `arm()`, `kill_session()` and `escalate()` down, i.e. locking CarPlay out
until someone killed the process by hand. `SO_KEEPALIVE` with `TCP_KEEPIDLE=10 / KEEPINTVL=5 /
KEEPCNT=3` bounds it at ~25 s. The wired arm's device-node watchdog has no analogue over Wi-Fi;
the kernel default (2 h idle) is not one.

**Not yet run against a phone** — the pump specifically. The BOOTSTRAP in front of it is now
device-proven (§2f, §6d), so the untested part is no longer "will a phone ever start"; it is whether
the phone that has the credentials associates, dials `AP_IP:5288`, and streams.

## 6d. The headset gate, as built (2026-09-04)

§2f was missing a step 0, and supplying it is the whole milestone: **the box has to be a headset
before the phone will project.** Everything downstream of the channel-4 socket — the seven messages,
the state machine, the AP credentials — was already right and is unchanged, as is the direction
(the phone dials us).

**Where.**
`crates/bt-common/src/sdp_record.rs`: `HandsFreeRecord`, `HeadsetRecord` and the shared
`encode_audio_record`, plus `HFP_HF_RFCOMM_CHANNEL` (5) and `HSP_HS_RFCOMM_CHANNEL` (6).
`crates/bt-common/src/sdp_server.rs`: `hfp_hf_service`, `hsp_hs_service`, and the `select_services`
no-match fix. `crates/vendor/wireless/src/hfp_hf.rs` (new): the AT client, the HSP no-op, the line
framing and the path lever. `crates/vendor/wireless/src/sdp_client.rs`: `search_pattern_uuid16`,
`scan_hfp_supported_features`, and `Services{ hfp_ag, hfp_ag_features, hsp_ag }`.
`crates/vendor/wireless/src/reconnect.rs`: `attempt_headset`, `headset_candidates`,
`hold_headset_link` / `drain_headset_link`. `main.rs`: two more registered records and two inbound
accept threads.

**Two routes, both implemented, tried in that order.** They differ in AOSP, not just in taste:

* **HFP (primary).** Connect to the phone's Handsfree AG (`0x111F`; channel **4** on this Pixel) and
  run the stock AT dialogue verbatim — `AT+BRSF=63`, `AT+CIND=?`, `AT+CMER=3,0,0,1`, `AT+CLIP=1`,
  `AT+CCWA=1`, `AT+CHLD=?` (only when `+BRSF` bit 0 says three-way), `AT+CIND?`. This is the route
  the stock firmware proved against this exact phone. (The wideband lever adds exactly one step:
  `AT+BRSF=191` and `AT+BAC=1,2` immediately after it, when the AG's `+BRSF` has bit 9.)
* **HSP (fallback).** Connect to the phone's Headset AG (`0x1112`; channel **3** on this Pixel) and
  say nothing. AOSP arms the SLC timer only for an inbound HFP connection —
  `bta_ag_act.cc:533-540` is `if conn_service == BTA_AG_HFP { start SLC timer } else {
  bta_ag_svc_conn_open }`, and that second branch raises `BTA_AG_CONN_EVT` →
  `BTHF_CONNECTION_STATE_SLC_CONNECTED` → HeadsetStateMachine `mConnected` immediately. Both public
  dongles use exactly this and exchange no AT traffic.

`CARPLAY_AA_HEADSET_PATH=hfp|hsp`, or `/tmp/aa_headset_path`, forces one; default is auto. A forced
path never silently falls back to the other — the lever exists to isolate a failure.

**Channels are read, never assumed.** `sdp_client::query` searches `0x111F` and `0x1112` on the same
L2CAP channel the iAP2 search just used and takes the RFCOMM channel out of each answer, along with
the AG's `0x0311 SupportedFeatures` when present (logged; the SLC acts on the `+BRSF` from the wire
instead). The 3/4 above are this phone's numbers, not constants.

**Inbound too.** A phone whose `PhonePolicy` auto-connects to a bonded headset will dial our records
rather than wait. Two more accept threads serve them, kernel backend only (the userspace RFCOMM
backend cannot serve a second channel without rejecting CarPlay's DLC). The HFP arm runs the **same**
dialogue over the accepted socket: in HFP the hands-free unit sends `AT+BRSF` first regardless of who
opened the channel, and the gateway waits for it.

**No claim, no bootstrap.** `attempt_headset` never takes the `SessionClaim` and never runs
`run_aa_bootstrap` — the claim and the owner flag are still taken by `run_aa_bootstrap` in the
channel-4 accept path when the phone connects, exactly as before.

**CORRECTED 2026-09-03 — this paragraph used to end "and no audio: this layer never opens SCO".**
It does now, and it had to: gearhead routes calls over the headset link. (The 2026-09-03 wording
also claimed the Assistant takes that path via `kxr.java:118-150`; **refuted on hardware
2026-09-04** — the Assistant uses the AA mic and guidance channels over Wi-Fi and never sends
`+BVRA`; details in [`01_SESSION_AND_AV.md`](01_SESSION_AND_AV.md) §telephony.) `sco_audio` serves
SCO for the life of each headset link, and both call directions were proven end to end on
2026-09-04 (same section). Two clauses of the old sentence survive
unchanged and are still policy: this layer **negotiates a codec only under the wideband lever**
(default `AT+BRSF=63` claims no codec negotiation, so the AG always opens plain CVSD; with
`CARPLAY_HFP_WBS` / `/tmp/hfp_wbs` it sends `AT+BRSF=191` + `AT+BAC=1,2` and answers `+BCS` —
[`01_SESSION_AND_AV.md`](01_SESSION_AND_AV.md) §telephony) and **never answers an unsolicited
result** —
`+CIEV`/`+BSIR`/`RING`/`+BVRA` are drained, classified and logged (`+CIEV: 6,4` renders as
`battchg = 4` against the `AT+CIND=?` names), and answering a call stays with the driver. The one
exception is the bench lever `CARPLAY_HFP_AUTOANSWER=1`, which sends `ATA` on the ringing edge.

**How long the link is held, and why it is bounded.** Outbound: hold until the owner flag reads
`wireless-aa` (the phone dialled us and the bootstrap claimed the box), then hold for the whole
session, then release. If the phone never dials, release after **20 s** — this wait blocks the
reconnect loop, so every second of it is a second a bonded iPhone later in the list is not being
driven back into CarPlay; 20 s is ~800x the 26 ms stock's phone needed and well past gearhead's own
5 s window. If some other projection claims the box while we wait, stand down immediately
(first-come-wins). Inbound: no grace and no exit on the session ending — the phone opened that link
and it is the phone's to close; dropping it would flip its HEADSET state back to disconnected.

**The `hold_acl` lever is narrowed, not removed.** Its premise — that holding an idle ACL past 5 s
would open the gate — is refuted. It now applies only to a bonded peer exposing NEITHER iAP2 NOR any
audio gateway, i.e. one with nothing to connect to.

**And the loop stops paging while a session is live.** `attempt()` returns early on a `wireless-aa`
owner. Only that token, never "any owner": `flags::owner()` falls back to the legacy
`/tmp/carplay_transport`, so a stale `wireless` there would silently disable the CarPlay reconnect
loop the function exists for.

**Proven on the bench 2026-09-04:** the outbound HFP dial flips this Pixel's
`BluetoothProfile.HEADSET` (SLC in 296 ms), gearhead then dials our AA record, the bootstrap reaches
`Established` over that channel-4 socket, and the session runs (§1 baseline). One operational fact
learned the same day: gearhead refuses wireless AA outright while **any VPN** is active on the
phone (`SOCKET_VPN_CONNECTION_ERROR`, logged as a deliberate user disconnect, no retry) — adb
reverse-tethering tools are therefore unusable on this bench, and after the VPN is gone the
session only returns once the HEADSET state changes again (a Bluetooth off/on on the phone does it).

### Explicitly NOT doing

- **Loosening `wireless_up()`'s gate.** `wireless: false` means wired-only for BOTH protocols
  (`02_ARBITRATION.md` §0), so the existing gate is already correct, and this avoids editing a
  choke point carrying an owner directive and four converging paths.
- A separate AA enable switch — AA rides CarPlay's `wireless:` flag by design.
- Any change to `bt_bringup`: stock's EIR carries **no** AA UUID and still does wireless AA, and its
  CoD is byte-identical to ours (`0x200408`). Verified, so leave it alone.

### Settleable only on hardware

1. **Does the phone need more than the four records?** Answered in principle (§6b: a headset-class
   profile is the gate) but not measured on ours. Remaining hedges in order: `0x1101` in the AA
   record's class list (`ServiceRecord::extra_class_uuid16`) → a plain Serial Port record → A2DP
   sink, which also yields `pcq.d` and is heavier.
2. **Android's browse pattern.** The PublicBrowseGroup claim is general BlueZ knowledge, not a
   capture from this bench. Step 3's matching rule depends on it; a `btmon` capture settles it.
3. **Message ids 8/9/11** — single-sourced; the ping reply is gated off behind `AAW_ANSWER_PING=1`.

## 7. Sources

- `aasdk` (CubeOne fork) — `protobuf/aap_protobuf/aaw/*.proto`, `src/Channel/WifiProjection/`,
  extracted locally under `aa_rebuild/aasdk-main-extracted/`. GPLv3: **read for protocol facts,
  do not copy code.** Note `WifiSecurityRequestMessage.proto` in that extraction is corrupt (it
  contains a boost test file); the `aaw/` copies are intact.
- `openauto` (same fork) — `src/btservice/AndroidBluetoothService.cpp` (the SDP record) and
  `AndroidBluetoothServer.cpp` (the RFCOMM state machine). Same licence caveat.
- [`aa-proxy-rs`](https://github.com/aa-proxy/aa-proxy-rs) — a Rust implementation of exactly this
  bootstrap; source of the dummy-profile requirement and message ids 8/9/11.
- [`WirelessAndroidAutoDongle`](https://github.com/nisargjhaveri/WirelessAndroidAutoDongle) — field
  reports and the 5288 port convention.
- The stock `ARMAndroidAuto` binary (§2a) — the on-box oracle, and the only source here that is
  known to work against *this* hardware.
