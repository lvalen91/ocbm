# 12 — Observed session flow (LIVING DOCUMENT — update in place, never fork)

**What this is.** The end-to-end sequence of a working wireless CarPlay session **as observed on our own
hardware**, with the failure points marked. [`05_SESSION_FLOW.md`](05_SESSION_FLOW.md) is traced from
`ocbm` source and remains the authority on *protocol* ordering; this is what our stack actually
does, in order, with the log lines you should expect to see.

> **UPDATE THIS FILE IN PLACE.** When the flow changes, edit the affected phase — do not write a new
> dated document. Raw per-session captures belong in `evidence/<date>/`, not in `docs/`. See
> [`00_HANDOFF.md`](00_HANDOFF.md) §6.

## Status

**Coexistence with GM's CarPlay service works; do not stop, disable or uninstall GM's service**
(Phase 5, Failure Point 3; device-verified 2026-08-12 and 2026-09-09). On-device acceptance of the AAAA+A form of
the self-hosted advert is still pending (Failure Point 3). Full A/V with GM's service running and untouched was first achieved 2026-08-12:
video (HEVC 2400x960, `OMX.Intel.hw_vd.h265`), media audio (48 kHz 2ch AAC-LC), Siri (16 kHz AAC-ELD
`speechRecognition`) and telephony streams all negotiated and flowing. Four things had to be true at once,
and each failed silently on its own:

1. box binaries packed with **UPX 3.96**, not 5.x (5.x's stub segfaults on this 3.14 kernel);
2. `libcarplayjni.so` linked with the **NDK cross-compiler** (`CC_x86_64_linux_android`) so the ELD shim is
   x86-64 ELF, not host arm64 Mach-O; otherwise `eld_enc_*` stays undefined, `dlopen` fails, and the app
   cannot pair: the native core is unavailable and the connection is closed (`native core unavailable —
   closing`; there is no Kotlin fallback);
3. a **self-hosted mDNS advert** on a hostname we own (Failure Point 3);
4. exactly **one** OCBM bring-up per process (Failure Point 4).

**Reference session (last verified end-to-end 2026-09-09)**, app build `7c92000`, capture bundle
`gmccpa_probe_20260909_225421` + its full `logcat.log` (not in this tree; `01_FINDINGS.md` §9). It is the first capture carrying the **whole** stack:
pair-verify, `/auth-setup`, SETUP 130/110/102, HEVC, AAC-LC, the iAP2 metadata tunnel authenticating and
Identifying, album art, the mic uplink, touch and a clean teardown, with GM's CarPlay service live throughout.

| Milestone | Elapsed from inbound |
|---|---|
| `INBOUND CONTROL CONNECTION` (22:55:20.349) | 0 |
| control channel ENCRYPTED (pair-verify done) | **57 ms** |
| `/auth-setup` OK, 1113 B M2 (2 chip ops over `CH_MFI`) | 1.691 s |
| `RECORD done` | 1.723 s |
| **`FIRST FRAME RENDERED`** | **2.598 s** |

`RECORD done` → first frame is 875 ms. First audio is not a latency figure: iOS did not SETUP the type-102
stream until 8.5 s after RECORD, the phone's choice. Session totals: 1358 frames rendered, 0 AUs dropped,
843 media audio frames, 361 mic chunks. The 0.82 s RECORD-to-first-frame figure of the 2026-08-12 standby session is a different span on a warm standby path.

Bench 2026-10-08, build `230eaa7` (first session after an `install -r`, ONE sample): pair-verify M1+M3 in
28 ms from accept, first frame ≈3.1 s after accept. Not a regression against 2.598 s (single post-reinstall
sample, different content) and not a pass: re-sample on a warm start.

**Behaviours newer than the reference session** (canonical description: `04_SYSTEM_MODEL.md` §4e). Bench
2026-10-08 (v5.8 + v5.9, cp12 capture, rc1 then rc2):
- **Claim with the app closed (rc2, 04:43:27):** `auto-claim (media-browser bind): CLAIM — claiming the
  adapter`, `ui: CLAIMING ADAPTER`, rediscover loop woken on `CT_HELLO_ACK`. No dialog, no UI.
- **Nav prompt (rc2, 04:48:16):** `duckAudio` (`volume=-13.98dB durationMs=500`); nav sink `AAC-ELD 48000Hz 2ch …
  buffer 48ms`, plays with `no focus request (stock: OD-8C)`; device starts 66 ms after `play()`, software duck
  lifts 313 ms after `duckAudio`; GM logs `updateMutexSink … ducking -18.70dB`. END line: `played 415 … excess 11
  … excess loud 0, silent catch-up 9, target 80ms W 200ms`.
- Not yet bench-observed: Stop-per-boot, the B-V1-W overlap matrix, OD-7F refusals, pairing-key migration
  (B-V5-1), UI layout/keyboard.
- The radio cold-restarted once during rc2 testing (04:48:50, per the session handoff, not in the tree); cause
  UNKNOWN (reason code 51, as for every earlier power cycle; no crash or shutdown lines). An adb dropout of
  ≈13 min with no reboot (same uptime and app pid) was seen the same evening (`6c95684`); the "restart" may have
  been the same thing, unconfirmed.

Earlier captures remain in the archive (`evidence/` lives in the archived standalone `gm_ccpa` checkout, not
under this tree): `evidence/session_2026-08-12-standby/`, `evidence/session_2026-08-12-freshpair/` and
`evidence/session_2026-08-12/` (16 min unbroken, 21,000 frames).

---

## The shape in one line

```
app claims box (USB/OCBM) → MFi relay proven → SUBSCRIBE wakes radios WITH creds → box does BT+iAP2
→ 0x5703 hands phone the VEHICLE hotspot → phone joins br0 → app advertises + dials → phone dials back
→ pair-verify → auth-setup (MFi over OCBM) → RECORD → streams → localhost seam → MediaCodec
```

Two MFi chip calls happen on the box over local i²c; **four more are relayed by the app over `CH_MFI`**
mid-session (two for `/auth-setup`, two for the iAP2 tunnel; `05_SESSION_FLOW.md` §10). OCBM carries only kilobytes throughout; all media crosses the vehicle's own 5 GHz SoftAP
directly to the app.

---

## Phase 0 — App claims the box

```
[usb  ] claimed interface 0 (class 0xff) IN=0x81 OUT=0x01 mps=512
[ocbm ] >> CT_HELLO  →  << CT_HELLO_ACK v1 caps=0x0000003f [CONSOLE|ECHO|MFI|IP|FILE|ETH]
[ocbm ] >> CT_SETTIME  →  ack status=0 (applied)
```

- Endpoints are **discovered by walking the interface**, not hardcoded. The hardware-verified pair is
  **IN `0x81` / OUT `0x01`**.
- **Who calls the claim:** the driver's Start, a launcher open, or (app closed, persistent USB grant in
  place) `AutoClaim.trigger` from the attach receiver, BOOT_COMPLETED or the media-browser bind
  (`auto-claim (<origin>): <DECISION> — <why>`); a bare shell `am start` takes the same path.
- `CAP_MFI` in the ACK gates everything downstream.
- `CT_SETTIME` matters because the box has no RTC battery.

## Phase 1 — Prove the MFi relay before anything depends on it

```
[mfi  ] cert: 945 bytes  (matches the expected 945)
[mfi  ] sign: 128 bytes  (matches RSA-1024)
```

Deliberately before any BT work: it needs no box-side change and proves the architecture's keystone in
isolation. A transient `status 1` here is known and benign if a retry succeeds
(`06_BRINGUP_RUNBOOK.md` §5 item 6).

**The caller must gate on the result.** `runAll()` reports failure by returning, not throwing — every `ABORT:`
path inside it is a bare `return` — so a bare `try/catch` never fires for "no adapter", "claim failed" or "no
`CT_HELLO_ACK`". The gate is the `if (!r.helloOk)` check after `ocbm().runAll(...)` in
`SessionCore.autoStart()`, under the `GATE ON THE RESULT` comment. Without it the
launcher reports "box claimed, MFi proven" with no adapter on the bus.

## Phase 2 — `CT_SUBSCRIBE`: the radio-wake edge — **credentials MUST be present**

> The app sends no `wifi_channel` on purpose (owner decision) and the box keeps its stock hostapd channel
> (`tools/session_supervisor.sh`, ocbm repo root, strips `channel=` only when a channel is supplied).

```
[ocbm ] >> CT_SUBSCRIBE (153B config)
         | wireless: true | wifi_ap: false | pairing: just_works
         | wifi_ssid: <vehicle SSID> | wifi_pass: …
```

> ### FAILURE POINT 1 — the most order-sensitive step in the whole flow
> `host_present` 0→1 is what makes the box run `wireless_up()` → `apply_host_wifi_creds()`, which
> rewrites `/etc/hostapd.conf`. **Credentials arriving in a later SUBSCRIBE are ignored** — `wireless_up`
> has already run. Subscribe once without them and `0x5703` is pinned to the box's stock `<box stock SSID>`
> for the entire session; the phone is told to join an AP that is never raised (`wifi_ap:false`), and
> per `06_BRINGUP_RUNBOOK.md` §5 item 4 that dead network then poisons the next attempt until it is
> forgotten on the phone.
> **Recovery is a full `host_present` cycle** (app stop/start, minding the ~20 s flap detector).
> `OcbmProbe` refuses to subscribe with no SSID rather than entering that state. There are no compiled-in
> credentials (OD-7A), so on first run, with none saved, Start opens the hotspot dialog
> (`SessionCore.blockStartForCreds`) instead of reaching this step.
> Verify with: `grep -aE '^ssid=' /etc/hostapd.conf` on the box — expect the vehicle SSID.

## Phase 3 — Box side: Bluetooth, iAP2, and the handoff

The box raises `hci0`, the SSP agent (Just-Works), SDP and RFCOMM, then drives reconnect of the bonded
phone. Over RFCOMM: `DETECT/SYN` → `0xAA01` cert (**chip call 1**, local i²c) → `0xAA03` sign
(**chip call 2**) → `0x1D01` Identify → `0x1D02` accepted → `0x5702` → **`0x5703` carrying the vehicle's
hotspot**. The phone leaves BT-only and joins `br0`.

**Outbound paging from the box works and is the normal path.** The box drives the whole reconnect itself,
unprompted: `[reconnect] 1 bonded phone(s) — driving reconnect when idle` → `[sdp-client] L2CAP SDP channel up
(phone paged)` → `iAP2 RFCOMM channel on the phone = 1` → `[reconnect] RFCOMM connected OUT to the phone
(ch 1) — starting iAP2 handshake` → cert → sign → Identify → `0x5703` (`04_SYSTEM_MODEL.md` §4c item 4).
What fails is a **one-sided bond**, see "The half-bond" below. Triggering from Settings ▸ General ▸ CarPlay
on the iPhone remains a valid manual kick but is not a precondition.

There is **no join confirmation** over iAP2; a successful `0x5703` write is the only signal, and iOS retries
`0x5702`, so the handler must be idempotent.

> **"The box raises `hci0`" has a precondition, and when it is unmet NOTHING says so** (device-diagnosed
> 2026-08-28). The supervisor brings radios up via `sh /script/radio_hal.sh bt_on` (output teed into
> `/tmp/box.log` and `/tmp/bt.log`) inside a detached `setsid` wrapper whose exit status it never reads.
> `hci_uart` is a **loadable module** on the CCPA, shipped in `/lib/firmware/nxp/iw416_ko.tar.gz`; the seam
> extracts and `insmod`s it. If `/script/radio_hal.sh` or `/script/radio_detect.sh` is missing (the state a
> targeted `ocbm_push.sh` leaves: it pushes `ocbmd` + `btd` only), the `n_hci` line discipline is never
> registered, `hciattach` fails `EINVAL` and `hci0` never appears. Every layer above still reports success
> (OCBM claim, `CT_HELLO_ACK`, a real 945-byte certificate, a real 128-byte signature, `CT_SUBSCRIBE`,
> `HOST_PRESENT`); the phone never sees a car.
>
> **Read `CT_BOX_HEALTH` first.** `BH_HCI_PRESENT` (bit 0) clear means *no controller registered at all* (the
> sysfs node survives `hciconfig hci0 down`); health `0x50` (`btd|rootfs-ok`) with bit 0 clear is the exact
> signature. The box's `/tmp/bt.log`, `/tmp/wl.log` etc. are streamed into logcat as `[box:<file>]` lines. See
> `ocbm` `docs/ops/06_CORRECTIONS_LEDGER.md` R-20W-5.

## Phase 4 — App becomes the accessory on Wi-Fi

```
[cprx ] CarPlay RX up on :7011 deviceid=<MAC-1> features=0x44440B80,0x61
[cprx ] advertise _airplay._tcp:7011 REGISTERED as 'gm-ccpa'
[cprx ] advert not yet REGISTERED — holding the connect-out until it is   (only if it lags)
[mdns ] live lookup: <phone name>._carplay-ctrl._tcp.local -> target=Owner-iPhone.local port=57347
                     addrs=<IPHONE_LL6>, <IPHONE_IP>
[cprx ] connect-out accepted on attempt 1 via <IPHONE_LL6>%br0:57347
```

Three things are load-bearing:
1. **The Car bit** (high word bit 32) in `features`, or iOS answers the connect-out and never opens RTSP.
2. **Advertise before dialling.** `GET /ctrl-int/1/connect` does not make iOS connect — it sets a
   *pending autoconnect* resolved immediately against an already-populated index. The gate is the
   **`onServiceRegistered` event** plus a 2.5 s margin, not a fixed wait: registration completing is
   observable, whereas "iOS has indexed us" is not.
3. **`AirPlay-Receiver-Device-ID` is a decimal uint64**, not a MAC string. TXT `deviceid` and `/info`
   `deviceID` keep the colon MAC — the asymmetry is deliberate.

> ### FAILURE POINT 2 — `NsdManager` serves a stale address AND a stale port
> The platform cache is unreliable in both fields, and **restarting discovery does not clear it** — the new
> resolve is answered from the same cache. iOS also rotates its control port constantly (observed
> `57236 → 57292 → 57347 → 57383` within minutes), so a cached port is dead within about a minute of being
> correct: right address + cached port ⇒ `ECONNREFUSED`; dead address ⇒ 3 s timeouts.
> **The wire is the authority.** `MdnsInspect.liveEndpoint()` queries SRV then A/AAAA directly and
> `CarPlayRx.dialCandidates()` dials those first, re-querying at attempts 3 and 6; the cached pair is kept only
> as a last resort because the wire query can occasionally miss a reply.
>
> Diagnose from the neighbour table — `INCOMPLETE` against the address in the `resolved '<peer>'` log line
> means you are dialling a ghost:
> ```bash
> adb shell ip neigh show dev br0
> adb shell "ping6 -c 2 -W 2 <resolved-addr>%br0"   # "Address unreachable" = confirmed
> ```

> ### FAILURE POINT 5 — the phone reads our TXT and never resolves SRV
> **Signature.** The phone is on `br0`, advertises `_carplay-ctrl._tcp`, answers `GET /ctrl-int/1/connect` with
> `200 OK`, and never opens the control connection (reproduced three times on 2026-08-27; 11 dials in one run).
> `airplayd` browses `_airplay._tcp`, queries **TXT** for every instance and stops: no SRV query, so no
> address to dial. Both SRV queries for our adverts in the capture were followed by a working session; all
> three failures had none:
> ```
> 13:25:53  QueryRecord START -- qname: gm-ccpa._airplay._tcp.local, qtype: SRV   → session came up
> 13:46:50  QueryRecord START -- qname: gm-ccpa._airplay._tcp.local, qtype: SRV   → session came up
> ```
> Capture with `idevicesyslog -p 'mDNSResponder|carkitd|wifid'`, grep `qtype: SRV`. iOS has **never**
> SRV-queried `gmccpa-rx` (the self-hosted advert); both working sessions resolved `gm-ccpa` (NsdManager), so
> the role of the self-hosted advert in the working sessions is not established (`11_HARDENING_PLAN.md`,
> discovery rows).
>
> **Does NOT fix it** (all tried): power-cycling the box, a fresh app process, re-binding/re-announcing the
> receiver, re-pairing Bluetooth, re-joining Wi-Fi through the CarPlay prompt. **Does:** reconnecting
> Bluetooth to the car **from the iPhone**, every time. "Forget This Car" alone is not enough while the box
> is connectable (iOS re-pairs instantly, `using cache with 1 vehicles`): take the box's Bluetooth down first,
> then forget, then re-pair. Not Failure Point 2: the failing runs resolved the phone perfectly; the gap is
> the phone's resolution of **us**.
>
> **App behaviour.** `CarPlayRx` counts accepted-but-unanswered dials (`STALL_DIALS` = 6, about 75 s at the
> rediscover cadence), logs once `!! STALLED: 6 consecutive connect-outs accepted with no inbound control
> connection on :<port>` with the instruction that works (reconnect Bluetooth from the iPhone), and calls the
> supervisor's `onStalled` (phase `STALLED`, `04_SYSTEM_MODEL.md` §4b).

## Phase 5 — The phone dials back

```
HandleControlServerEvent command 'connect' received for deviceID: <MAC-1>
Setting pending autoconnect deviceID: <MAC-1>
>>> INBOUND CONTROL CONNECTION on :7011
```

> ### FAILURE POINT 3 — coexistence with GM's CarPlay service. GM stays running.
> GM's `CarplayService` runs, advertises on `:7000`, and is **irrelevant** once our advert is correct.
> "GM must be stopped" is wrong; do not re-derive it.
>
> **Mechanism.** iOS keys its Wi-Fi CarPlay endpoint index by **host**, one endpoint per host, and the
> *first* Bonjour record for that host wins. Captured from the iPhone:
> ```
> 21:38:04.548641  Bonjour TXT Add CarPlay._airplay._tcp.local.       <- GM's
> 21:38:04.548668  Bonjour TXT Add gm-ccpa._airplay._tcp.local.       <- ours, 27 MICROSECONDS later
> 21:38:04.549866  Created APEndpointCarPlay [0xE051] with name 'CarPlay' and id '<MAC-2>'
> 21:38:04.549876  Added new WiFi endpoint 0xE051 (CarPlayControlSupported: Y)
> ```
> Both records arrive; iOS creates **one** endpoint and stamps it with GM's identity. Ours is folded in, not
> rejected. Every later autoconnect then fails looking for an ID that was never indexed:
> ```
> carManager_handlePendingAutoconnect: index = WiFi, for deviceID: <MAC-2>,
>                                      pendingAutoconnectID: <MAC-1>
> carManager_handlePendingAutoconnect: No matching endpoint found for deviceID <MAC-1>
> HTTP connection closing: -6753/0xFFFFE59F kConnectionErr
> ```
> Cause: **both SRV records targeted the same host, `Android.local`** (the platform hostname). It is a race
> we lose whenever GM registers first, which it always does (GM starts at boot, we start ~51 s later). It was
> never about the port (`:7011` vs `:7000`), the instance name, the TXT fields or our deviceID.
>
> **Fix (implemented; coexistence device-verified 2026-08-12 and 2026-09-09; AAAA+A acceptance pending below).** Publish a **second advert on a hostname we own**. `NsdManager`
> cannot do this — AOSP drops `NsdServiceInfo.setHost()` on registration — so `MdnsResponder.kt` answers mDNS
> itself: instance `gmccpa-rx`, SRV target `gmccpa-rx.local`, an A record for the `br0` bridge (`192.168.5.1`)
> and an AAAA record for br0's IPv6 link-local (an AAAA query is answered with A + NSEC when there is none).
> It runs **alongside** the NsdManager advert, wired in `CarPlayRx.advertise()`. Result, same rig, GM running
> untouched (the `(…)` family suffix is newer than that capture):
> ```
> [cprx ] self-hosted advert up: gmccpa-rx.local -> br0:7011 (AAAA+A)
> [cprx ] >>> INBOUND CONTROL CONNECTION from fe80::...%br0:59419
> [cprx ] native receiver core started for ...:59419 gen=5 (/info 1969B)
> [rust ] pair-verify OK -> control channel encrypted
> [cprx ] *** MILESTONE: control channel is ENCRYPTED — pair-verify completed
> ```
> The suffix is `(AAAA+A)`, or `(A only — no IPv6 link-local on br0)`. An A-only set is undialable: on
> 2026-10-06, 9/9 `Connecting to 192.168.5.1:7011` ended in `kTimeoutErr` (the radio drops IPv4 `:7011`).
> AAAA + A is a code fix with on-device acceptance pending (the phone must resolve `gmccpa-rx.local` to
> `fe80::…%en0` and never dial `192.168.5.1`). `CarPlayRx.bridgeWatch` polls br0 every 3 s, re-announces on an
> address change (`self-hosted advert re-announced: br0 addresses changed`) and retries a failed start after
> 30 s; the receiver starts at boot, before GM's bridge may hold its link-local.
>
> Pinning one address also fixes a second bug: `Android.local` resolves to **three** addresses
> (`192.168.1.100`, `172.16.4.100`, `192.168.5.1`) and only the last is routable from the phone's subnet, so
> resolution was a coin flip across an 8-interface head unit.
>
> **Acceptance test** — on the phone, look for a second `Created APEndpointCarPlay ... id '<MAC-1>'`. Absence
> of errors proves nothing; that line is the proof.
>
> ### NEVER force-stop or disable GM's CarPlay service
> Not as a workaround, not as a diagnostic, not "just once to check" (owner-instructed, 2026-08-12). Stopping
> it **costs ADB access** — `com.gm.domain.server.delayed` also holds REBOOT / SECURE_SETTINGS / OTA
> (`01_FINDINGS.md` §3) — so you lose the log channel, the install path and scripted bring-up in one go. It cannot be disabled
> from shell (`SecurityException: Shell cannot change component state`) and returns on every reboot
> regardless. `pm uninstall --user 10` is not a fallback and should not be offered. (`am force-stop`
> only frees the single per-host endpoint slot, so the other advert wins by default: the symptom, not the cause.)

> A *similar-looking* but different failure is the stale iOS endpoint cache, fixed by forgetting the vehicle
> on the iPhone and re-pairing. Distinguish them: cache staleness shows an index entry for **our** deviceID
> that has expired; coexistence shows an index entry for **`<MAC-2>`**.

## Phase 6 — Apple protocols, app-side

```
POST /pair-verify (plain, 37 B) → (plain, 125 B) → control channel ENCRYPTED
POST /auth-setup (enc, 33 B) → MFi sign+cert over CH_MFI → 1113 B M2      ← chip calls 3 & 4
SETUP phase1 → timingPort 33878 eventPort 35917 keepAlivePort 35396
RECORD → event channel accepted → session-focus handshake (requestUI=true, takeScreen=true)
SETUP phase2 DataStream(130) scid=… → streamID=1  [iAP channel]
[datastream] key schedule SOLVED: DataStream-Salt<seed>, read=output
SETUP phase2 screen(110) → dataPort 39697
SETUP phase2 audio(102) fmt=0x800000 48000Hz 2ch AacLc audioType="media" → dataPort 47312
POST /feedback every 2 s
```

- `/auth-setup` is the only place the OCBM link sits on the critical path mid-session (~1.7 s, nearly all of
  it the chip's signature poll).

> ### FAILURE POINT 4 — `/auth-setup` fails when the OCBM/USB link is dead → iOS `-72542`
> Because auth-setup is the one mid-session step that needs the adapter, a dead USB link kills the session
> *there* and nowhere earlier. Pairing completes first, which makes it look like a protocol problem when it
> is a transport problem. Signature on our side:
> ```
> [rust ] pair-verify OK → control channel encrypted
> [rust ] POST /auth-setup (enc, 33 B body)
> [ocbm ] >> CH_MFI create_signature (20B digest)
> [usb  ] write FAILED: bulkTransfer OUT failed at offset 0 (consecutive=12)
> [rust ] auth-setup FAILED: Signer(Custom { kind: Other, error: "createSignature threw" })
> ```
> Decoded from the iPhone (2026-08-12). **`-72542` has no `k…` symbol in Apple's logs**; it is the
> MFi-authentication failure raised by `mfiAuthentication_AuthenticateEndpoint`:
> ```
> Request written: CID …, Header 128 bytes, Body 33 bytes      <- the MFi challenge
> ### Error: -6753/0xFFFFE59F kConnectionErr                    <- we closed the socket
> mfiAuthentication_AuthenticateEndpoint:297: got error -72542/0xFFFEE4A2
> ### MFi failed: -72542/0xFFFEE4A2
> Deactivating endpoint 'gmccpa-rx' with reason 'Activation Failed'
> ```
> `isLikelyAccessoryIdleTimeout: no` with ~10 ms since the last response proves the accessory *actively
> closed* the connection rather than timing out: look at the transport, not at timing.
>
> **Root cause: two OCBM bring-ups in one app process** (`04_SYSTEM_MODEL.md` §4c item 2: the second
> `claimInterface` orphans the MFi signer bound to the first client). Tell in the log:
> `[cprx] acceptLoop: BindException: EADDRINUSE`, then a second `claimed interface 0`, then `read: rapid -1
> x50 — device gone`, and a `consecutive=` counter that only climbs (it resets to 0 on any successful write,
> so a monotonic climb means nothing has been written since). Verify with `grep -c "claimed interface 0"`;
> it must be exactly **1**. When re-launching by hand, `adb shell am force-stop android.car.usb.handler`
> before `am start`.

- **Streams are not batched.** `audio(102)` arrived ~4 minutes after `screen(110)` in the reference run.
  Expect SETUPs throughout the session.
- **Stream 130 accepted with a non-zero `streamID`** is accepted. The metadata plane exists and is consumed over `:9004` (Phase 7).

## Phase 7 — Localhost seam → MediaCodec

> **Bench 2026-10-08.** Build `230eaa7` (≈3 min in): 6000 → 6900 frames in 27.3 s = 33 fps; SurfaceFlinger
> present avg 34.9 ms (28.6 fps), 1/2/3/4+-vsync = 53/13/55/5; first 500 audio frames `1 AUs not queued, 3
> underruns` (start transient); PSS 42.7 MB, RSS 174 MB, 72 threads, 26.6 % of a core, 10 ART GCs in ≈3.5 min.
> rc2 (live owner session, read-only): 30.4-34.2 fps (~26 KB per AU) on a single 2400x960 mode at 60.001434 Hz;
> `/info` advertises `maxFPS 60`, so ~30 fps is the iPhone's adaptive encode target for static content (Apple
> Maps), not an app setting. Presentation is unpaced (`releaseOutputBuffer(idx, true)`), giving a bimodal
> 1/3-vsync cadence (F-C4; render pacing recorded, not adopted). 0 E lines, 0 underruns, 0 AU drops in 300 s,
> ~32 % of a core, PSS 39 MB; 32-37 % of log lines were the box `wireless` daemon's 1 Hz `EADDRINUSE` retry
> (F-C1, upstream) relayed over `CH_LOG`.

Rust `session.rs` (`spawn_screen`/`spawn_audio`) dials **outbound** to `127.0.0.1:9001` and `:9002`, so the
consumer must be listening **before** stream SETUP or every access unit is dropped (`connect carlink :900x
failed`). `forward.rs` supplies only the byte-transform applied to each access unit (AVCC→Annex-B for video,
raw AAC-LC→ADTS for media audio); it has no socket. The dial and the log line `"[screen] iPhone connected
from {peer}; forwarding video → 127.0.0.1:{sink_port}"` are in `session.rs`.

> ### The consumers stand by; they are not started on demand
> `CarPlayRx` fires `onSessionUp` the moment the phone opens the control connection — before pair-verify,
> ~3 s before any stream SETUP — and `SessionCore.showCarPlayScreen` brings the CarPlay screen (and with it
> the A/V consumers) up from it. Only the supervisor's `SESSION_UP` and `SessionCore.sessionUp` wait for
> pair-verify (`04_SYSTEM_MODEL.md` §4b; `11_HARDENING_PLAN.md` A-F1). The seams, threads and Surface
> therefore exist before the phone asks, and `AacPlayer.prime()` has already built a playing `AudioTrack` (safe: stereo AAC-LC 48 kHz is a wire-format
> ceiling, and a format mismatch discards the primed track rather than imposing the wrong rate).
>
> Decoder warm-up measures 151 ms; a missing consumer (the phone streaming into a closed port) was the cause
> of the old multi-second video lag. Expect:
> ```
> [aac  ] primed AudioTrack 48000Hz 2ch — waiting for the stream
> [cpui ] renderer attached to Surface          <- during pair-setup
> [aac  ] adopting the primed AudioTrack
> ```
> **Video is deliberately NOT pre-configured.** `csd-0` arrives in-band and the IDR carries VPS+SPS+PPS
> itself, so a standing-by renderer configures from the keyframe message alone — no format guess, no
> reconfigure when a guess is wrong. **Audio leads video by ~184 ms and that is irreducible:** audio plays on
> its first ADTS frame, video must wait for an IDR.

| Seam | Framing |
|---|---|
| `:9001` video | `[u32 BE len][Annex-B payload]`, one message per screen message |
| `:9002` audio | **no** prefix — raw ADTS |
| `:9003` voice | `VoiceRouter` and the per-purpose engines in `av/VoiceStreams.kt`: Siri→`Voice`, call→`Phone`/`Call`, nav→`Navigation` (no focus), alert→`CALL_RING`; each owner-confirmed audible on the truck 2026-09-04 (alert unexercised). Voice-tag v2 framing (BEGIN/END, per-AU receive stamp + RTP timestamp, per-stream jitter buffer): `04_SYSTEM_MODEL.md` §4e, `docs/carplay/06_AV_PIPELINE.md` §10. `:9003 (voice, drained)` lines in the 2026-08-12 captures predate `VoiceRouter`. |
| `:9004` metadata | **LIVE since 2026-09-09** — `[cpui] meta seam connected` / `[rust] [meta] connected to ocbmd metadata seam 127.0.0.1:9004`. Carries now-playing deltas (543 `0x5001` frames in one session) and album art (a 76,869 B JPEG reassembled over the iAP2 session-2 file transfer). |

```
[hevc ] VPS (28 B)  SPS (66 B)  PPS (11 B)
[hevc ] MediaCodec configured: video/hevc 2400x960 csd-0=105 B (VPS+SPS+PPS), decoder=OMX.Intel.hw_vd.h265
[hevc ] keyframe — decoding starts   →   FIRST FRAME RENDERED
[aac  ] configured AAC-LC 48000Hz 2ch → AudioTrack (USAGE_MEDIA)   →   FIRST AUDIO FRAME PLAYED
[cpui ] touch down n=(0.367, 0.239) sent=true
```

- `csd-0` for HEVC is **VPS+SPS+PPS concatenated**, not the SPS/PPS pair H.264 uses.
- Parameter sets arrive **twice** — standalone, then again in-band inside the IDR access unit. Skip only a
  *pure* parameter-set message; discarding the second set throws away the keyframe and yields a black screen
  on a healthy session.
- Touch goes back as normalised 0–1 HID reports on the encrypted event channel.

---

## Phase 8 — Session end and recovery

> **Bench 2026-10-08 (evening), build `6c95684`.** Adapter unplugged under a live session: "box gone, session
> live", CarPlay kept running. When the phone's Wi-Fi/BT then went down (control silent 6 s → `LINK_LOST`) media
> focus and the voice router stopped cleanly, the grace window held 10 s, rung 0 re-announced, rungs 1-2 skipped
> with `adapter: ABSENT`, and the phase went STALLED with the no-adapter text. Recovery timings and the
> remaining observations: `11_HARDENING_PLAN.md` B-F1, BP-1..BP-4 and the bench-pass notes.

Two teardown gaps were found on a device session (2026-08-28), both fixed:

- **A dropped session left a frozen screen up.** `CarPlayRx.fireSessionDown` reached
  `SessionCore.onCarPlaySessionDown`, which cleared app state and nothing told `CarPlayActivity`. When the
  phone left Wi-Fi range the session was declared down but the last frame stayed on the Surface, still
  swallowing touches. Fixed by `CarPlayActivity.onSessionEnded()` (companion fun), which finishes the Activity.
  Finishing is also what makes a *resume* work: `startSession` returns early on "already started", so a stale
  live Activity would absorb the returning phone's `onSessionUp` and the screen would never rebuild.
- **A permanent audio-focus loss was never recovered.** This head unit enforces focus (`dumpsys car_service`:
  `Use hal ducking signals true`); `AUDIOFOCUS_LOSS` paused and abandoned focus, but nothing re-requested it,
  so media stayed silent while Siri and calls (separate track, `VoiceRouter`) stayed audible. Now, after a
  permanent loss focus returns only on a resume intent (including the CarPlay screen returning to the front,
  `CarPlayActivity.onResume` → `AacPlayer.onScreenReturned`) or a phone-side resume (a phone already paused at
  the loss counts), and the loss also sends the phone a MainAudio Take as stock does (OD-9C). Bench-pending:
  whether iOS pauses on it (plan B-MED-9).
  Transient form of the second gap: after a call or Siri turn ended the `Voice` track lingered and media was silent for
  **10–20 s** (device-measured 12.0 s). The assistant edge and the focus edge ran on different clocks;
  shortening the assistant idle window to just past the hold closed the gap to ~1 s (1.95 s measured on the
  truck 2026-09-09, on the pre-v5.8 design; not re-measured on v5.8/v5.9). See `13_AUDIO_ROUTING.md` §0a.

---

## Bring-up in practice

Exact commands: [`06_BRINGUP_RUNBOOK.md`](06_BRINGUP_RUNBOOK.md) §7.1 (logging revival: §7). The short form:

1. Revive logging — `setprop persist.log.tag V`, `logcat -G 16M`. It boots effectively dead and silence looks
   exactly like a dead app.
2. Re-assert `/tmp/no_escalate` on the box (tmpfs — it self-clears on every box reboot, re-arming the reboot
   ladder).
3. Launch with credentials: `--es run full --es ssid '…' --es pass '…'`.
4. Initiate CarPlay **from the iPhone**.

`--es run carplay_ui` is not part of bring-up — the screen starts itself from the session event. It remains
available to *restore* the screen after something backgrounds it.

### iAP2 metadata tunnel: MFi calls go through the OCBM relay (device-confirmed 2026-09-09)

Without a remote signer the tunnel (`DataStream(130)`) gets its SYN-ACK and then, every 120 s until the attempt
budget is gone, logs `MFi cert: lock busy (another chip user holds /tmp/carplay_mfi.lock)`, `RX 0xAA00: action failed, state
held`, `handshake budget (120s) expired at Some(Init)`. Video and audio were unaffected; NowPlaying metadata,
route guidance and the controls channel were lost.

"Lock busy" was a misreport. `MfiLock::acquire()` (`crates/vendor/mfi-i2c-local/src/lib.rs`) opens
`/tmp/carplay_mfi.lock`; `/tmp` does not exist on Android, so the open fails `ENOENT`. `acquire` now returns
`MfiError::LockUnavailable` for that (no local chip access; a remote signer is required) and reserves
`MfiError::LockBusy` for a real 10 s lock timeout. Direct `/dev/i2c-1` is SELinux-blocked for an app UID regardless. `iap_tunnel.rs` called
the local-chip path instead of the `MfiSigner` the `ControlServer` held (`RemoteMfiSigner`,
`native/carplay-jni/src/lib.rs`, relaying over OCBM `CH_MFI`).

Fix: `receiver::iap_tunnel::set_remote_signer()` (`crates/vendor/receiver/src/iap_tunnel.rs`) installs the
signer the tunnel calls instead of `mfi_i2c_local`, which is compiled in only under `local-mfi` (on-box
builds), so an Android build without a remote signer fails loudly. Called in
`Java_zeno_gmccpa_pair_NativeCore_nativeInit` (`native/carplay-jni/src/lib.rs`).

Device result: `remote MFi signer installed`, `TX 0xAA01` ← `[jni] MFi certificate via OCBM relay: 945 bytes`,
`TX 0xAA03` ← `MFi signature via OCBM relay: 128 bytes`, `AuthSuccess → RX 0x1D02 -> Identified`, `3 subscribes
(now_playing, route_guidance, call_state)`; zero `lock busy`, zero budget expiries. **Six chip ops per session
is observed, not derived.** One benign warning: `/sys/class/bluetooth/hci0/address unreadable -- param 17
carries a placeholder BD address` (no local HCI on the head unit; the phone accepted the Identify).

### Clearing a pairing — there are THREE independent stores

Clearing one clears nothing else, which is why forgetting the vehicle on the iPhone still leaves the box
paging the phone and the phone showing an unknown-accessory prompt (observed 2026-08-12):

| Store | Cleared by |
|---|---|
| iPhone's CarPlay/BT record | forget the vehicle on the iPhone — **but take the box's Bluetooth down first**, or it re-pairs and re-creates the record before the forget settles (observed 2026-08-27) |
| **Box's BT link key** | **Adapter & logs ▸ Forget pairing**, or `--es run ocbm_forget` |
| App's AirPlay LTPK (`carplay_peers.bin`) | **Adapter & logs ▸ Forget pairing**, `--es run ocbm_forget`, or `adb uninstall` |

Forget Pairing and `ocbm_forget` clear the box's bond and the app's peer store together (a split brain here
presents as `pair-verify` failing, which reads as broken crypto). The iPhone's record remains yours to clear
by hand. With `--es mac` the box clears that one bond plus all of its AirPlay peers and the app clears its
whole peer store (no per-device mapping), so every other iPhone bonded to the adapter must be forgotten and
re-paired too. With no adapter link it answers "No adapter connected — press Start first" and leaves the
state word.

A genuinely fresh pairing shows the full `pair-setup` M1/M3/M5 in the log. If it fast-paths straight to
`pair-verify`, a peer store survived and the test was not clean.

#### The half-bond, and how it presents (device-observed 2026-09-08)

Clearing exactly one of the BT stores leaves a **one-sided bond** (`04_SYSTEM_MODEL.md` §4c item 4). It does
not look like a pairing problem from either end alone. The box holds a link key the phone has discarded, so
its outbound reconnect gets through SDP and dies at authentication:

```
[box:wl] [sdp-client] querying SDP on <phone> (PSM 0x0001)
[box:wl] [sdp-client] L2CAP SDP channel up (phone paged)
[box:wl] [sdp-client] iAP2 RFCOMM channel on the phone = 1
[box:wl] [reconnect] RFCOMM connect to channel 1 failed: connect timed out    <- ~8 s later
```

The phone's account of the same moment:

```
bluetoothd: Not delaying security enforcement for Address=<box> keys available ? No pairing state:(STATE_IDLE)
bluetoothd: Call to retrieve key chain magic key data of type 0 for device <box> failed with result 150
BluetoothSettings: Setting cell "CarLink-xxxx" paired 1 and connected 0
bluetoothd: Received connection result for "Wireless IAP" profile on device <box> - result was 20431
```

`paired 1` with `keys available ? No` is the signature: the Settings record exists, the keychain entry does
not. Tapping the device gives "can't connect" forever.

**Two traps in reading this.** The box logs an SDP `ServiceSearchRequest -- 0 match(es)` just before the
phone hangs up, which looks like the phone rejecting our service records; it is not (iOS enumerates all four
records first, the connection dies at security enforcement, not discovery). And the box's `reconnect` loop
keeps retrying, so the log fills with plausible activity while nothing can complete.

**Remedy: clear BOTH sides, then re-pair.** `--es run ocbm_forget` (`MGMT_FORGET_ALL`) on the box, and
Forget This Device on the iPhone. After a genuine two-sided clear the box logs `LOAD_LINK_KEYS(count=0)` and
the next pair writes `NEW_LINK_KEY ... store_hint=1`; that pair of lines is the check that the slate is clean.

Health check (derive the uid — it changes on every reinstall):
```bash
APPID=$(adb shell dumpsys package android.car.usb.handler | sed -n 's/.*userId=\([0-9]*\).*/\1/p' | head -1)
adb shell "cat /proc/net/tcp6" | awk -v u="$((1000000 + APPID))" '$8==u && $4=="01"'
```
4–6 established sockets plus `POST /feedback` every 2 s is a healthy session.

---

## Vehicle state → CarPlay: drive-restricted UI and day/night

**Device-proven 2026-09-08 on gminfo37, owner-observed both directions.** `av/VehicleStateWatcher.kt` drives
two CarPlay runtime levers from AAOS vehicle state. Both are pure runtime `/command`s on the live event
channel: no reconnect, no SETUP negotiation.

| Lever | Command | Source | Status |
|---|---|---|---|
| Drive-restricted UI | `setLimitedUI` | `GEAR_SELECTION` (`0x11400400`), UXR fallback | **Proven** — Apple Maps keyboard icon goes on shift out of Park, returns in Park |
| Day/night | `setNightMode` | `NIGHT_MODE` (`0x11200407`), `uiMode` fallback | **Proven** — carries to the CarPlay UI |

**Permissions (measured 2026-09-08).** An unprivileged app gets `CAR_POWERTRAIN` (`normal`, gates
`GEAR_SELECTION`) and `CAR_EXTERIOR_ENVIRONMENT` (`normal`, gates `NIGHT_MODE`). `CAR_SPEED` is `dangerous`
(unused); `CAR_DRIVING_STATE` is `signature|privileged` and out of reach, so gear plus `CarUxRestrictions`
(no permission needed) replaces `CarDrivingStateManager`'s `PARKED/IDLING/MOVING`.

**Two findings that cost time:**
1. **`uiMode` is NOT the night source on this head unit.** With vehicle night active, `cmd uimode night`
   reported `no` and `Configuration.uiMode` read day: GM does not propagate vehicle night into uiMode. The
   `NIGHT_MODE` property carries it; `uiMode` is only a fallback.
2. **`setLimitedUI` does nothing without `/info` `limitedUIElements`.** `limitedUI` is a boolean; the array is
   the SET it applies to (iOS builds a bitmask from it; none means mask 0, acked 2xx, no effect). We declare
   `["softKeyboard","softPhoneKeypad","musicLists","longUserAlert"]` (`tools/info_plist_limitedui.py`),
   mirroring the macOS box where the toggle demonstrably works; `softKeyboard` is the Apple Maps keyboard
   icon. See `docs/carplay/03_SDK_GROUND_TRUTH.md` §10.

**Session-scoped; the assert must retry.** Both commands are refused with no event channel, so state is
pushed fresh on session-up and never persisted. `CarPlay live` and "event channel usable" are not the same
instant: on 2026-09-08 `awaiting dial-back -> CarPlay live` logged at 12:52:07.779 and both commands were
refused at 12:52:07.833. `VehicleStateWatcher.reassert` retries every 400 ms up to 20 times (~8 s), then logs
`gave up asserting vehicle state` at E and keeps re-asserting every 2 s while the session is up (`LeverRetry`).
