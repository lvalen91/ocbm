# 12 — Observed session flow (LIVING DOCUMENT — update in place, never fork)

**What this is.** The end-to-end sequence of a working wireless CarPlay session **as observed on our own
hardware**, with the failure points marked. [`05_SESSION_FLOW.md`](05_SESSION_FLOW.md) is traced from
`ccpa_custom` source and remains the authority on *protocol* ordering; this is what our stack actually
does, in order, with the log lines you should expect to see.

> **UPDATE THIS FILE IN PLACE.** When the flow changes, edit the affected phase — do not write a new
> dated document. Dated session write-ups are how a docs directory rots: five files describe the same
> flow at five moments and a later reader cannot tell which is current. Raw per-session captures belong
> in `evidence/<date>/`, not in `docs/`. See "Document policy" in [`00_HANDOFF.md`](00_HANDOFF.md).

> ### ✅ 2026-08-12 (late) — full A/V **with GM's CarPlay service running and untouched**
> Video (HEVC 2400x960, `OMX.Intel.hw_vd.h265`), media audio (48 kHz 2ch AAC-LC), Siri
> (16 kHz AAC-ELD `speechRecognition`) and telephony streams all negotiated and flowing. Coexistence
> is **solved** — see Failure Point 3 below, which has been rewritten from the old, wrong
> "force-stop GM" guidance. **Do not stop, disable or uninstall GM's service.**
>
> Four things had to be true at once, and each failed silently on its own:
> 1. box binaries packed with **UPX 3.96**, not 5.x (5.x's stub segfaults on this 3.14 kernel);
> 2. `libcarplayjni.so` linked with the **NDK cross-compiler** (`CC_x86_64_linux_android`) so the
>    ELD shim is x86-64 ELF, not host arm64 Mach-O — otherwise `eld_enc_*` stays undefined, `dlopen`
>    fails, and the app silently serves pair-verify as 501 from a Kotlin stub;
> 3. a **self-hosted mDNS advert** on a hostname we own (Failure Point 3);
> 4. exactly **one** OCBM bring-up per process (Failure Point 4).

**Last verified end-to-end: 2026-09-09**, app build `7c92000`, capture bundle
`gmccpa_probe_20260909_225421` + its full `logcat.log`. This is now the reference session: it is the
first capture that carries the **whole** stack — pair-verify, `/auth-setup`, SETUP 130/110/102, HEVC,
AAC-LC, the iAP2 metadata tunnel authenticating and Identifying, album art, the mic uplink, touch,
and a clean teardown — with GM's own CarPlay service live throughout.

Measured, from the control connection landing:

| Milestone | Elapsed from inbound |
|---|---|
| `INBOUND CONTROL CONNECTION` (22:55:20.349) | 0 |
| control channel ENCRYPTED (pair-verify done) | **57 ms** |
| `/auth-setup` OK, 1113 B M2 (2 chip ops over `CH_MFI`) | 1.691 s |
| `RECORD done` | 1.723 s |
| **`FIRST FRAME RENDERED`** | **2.598 s** |

`RECORD done` → first frame is **875 ms**. First audio is *not* a latency figure here: iOS did not
SETUP the type-102 stream until 8.5 s after RECORD, which is the phone's choice, not ours.
Session totals: **1358 frames rendered, 0 AUs dropped**, 843 media audio frames, 361 mic chunks.

> **This supersedes the previous banner's "2026-08-12, RECORD to first frame 0.82 s".** That number
> is not contradicted — it measured a different span on a warm standby path — but 2026-08-12 is no
> longer the newest end-to-end proof, and several rows below that said "not yet exercised" now are.
> `05_SESSION_FLOW.md` §9's "session-start → first-video-frame is not measured anywhere in the
> corpus — don't quote a number for it" is **retired**: it is 2.598 s from inbound, measured here.

Earlier captures remain the archive (`evidence/` lives in the standalone checkout at
`~/Documents/carlink/old/gm_ccpa/evidence/`, not under this tree):
`evidence/session_2026-08-12-standby/`, `evidence/session_2026-08-12-freshpair/` and
`evidence/session_2026-08-12/` (16 min unbroken, 21,000 frames).

---

## The shape in one line

```
app claims box (USB/OCBM) → MFi relay proven → SUBSCRIBE wakes radios WITH creds → box does BT+iAP2
→ 0x5703 hands phone the VEHICLE hotspot → phone joins br0 → app advertises + dials → phone dials back
→ pair-verify → auth-setup (MFi over OCBM) → RECORD → streams → localhost seam → MediaCodec
```

Two MFi chip calls happen on the box over local i²c; **two more are relayed by the app over `CH_MFI`
mid-session**. OCBM carries only kilobytes throughout — all media crosses the vehicle's own 5 GHz SoftAP
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
- `CAP_MFI` in the ACK gates everything downstream.
- `CT_SETTIME` matters because the box has no RTC battery.

## Phase 1 — Prove the MFi relay before anything depends on it

```
[mfi  ] cert: 945 bytes  (matches the expected 945)
[mfi  ] sign: 128 bytes  (matches RSA-1024)
```

Deliberately before any BT work: it needs no box-side change and proves the architecture's keystone in
isolation. A transient `status 1` here is known and benign if a retry succeeds (`06` §5.6).

**The caller must gate on the result.** `runAll()` reports failure by returning, not throwing — every
`ABORT:` path inside it is a bare `return` — so a bare `try/catch` around it never fires for "no
adapter," "claim failed," or "no `CT_HELLO_ACK`." Until 2026-08-28 this meant the launcher could report
"box claimed, MFi proven" with no adapter on the bus at all. Fixed by checking `LinkResult.helloOk`
explicitly (the `if (!r.helloOk)` gate after `ocbm().runAll()` in `MainActivity.autoStart()`, with the `GATE ON THE RESULT` comment above it).

## Phase 2 — `CT_SUBSCRIBE`: the radio-wake edge — **credentials MUST be present**

```
[ocbm ] >> CT_SUBSCRIBE (153B config)
         | wireless: true | wifi_ap: false | pairing: just_works
         | wifi_ssid: myChevrolet 32D4 | wifi_pass: … | wifi_channel: 36
```

> ### ⚠ FAILURE POINT 1 — the most order-sensitive step in the whole flow
> `host_present` 0→1 is what makes the box run `wireless_up()` → `apply_host_wifi_creds()`, which
> rewrites `/etc/hostapd.conf`. **Credentials arriving in a later SUBSCRIBE are ignored** — `wireless_up`
> has already run. Subscribe once without them and `0x5703` is pinned to the box's stock `ccpa-b0df`
> for the entire session; the phone is told to join an AP that is never raised (`wifi_ap:false`), and
> per `06` §5.4 that dead network then poisons the next attempt until it is forgotten on the phone.
> **Recovery is a full `host_present` cycle** (app stop/start, minding the ~20 s flap detector).
> `OcbmProbe` now refuses to subscribe with no SSID rather than entering that state.
> Verify with: `grep -aE '^ssid=' /etc/hostapd.conf` on the box — expect the vehicle SSID.

## Phase 3 — Box side: Bluetooth, iAP2, and the handoff

The box raises `hci0`, the SSP agent (Just-Works), SDP and RFCOMM, then drives reconnect of the bonded
phone. Over RFCOMM: `DETECT/SYN` → `0xAA01` cert (**chip call 1**, local i²c) → `0xAA03` sign
(**chip call 2**) → `0x1D01` Identify → `0x1D02` accepted → `0x5702` → **`0x5703` carrying the vehicle's
hotspot**. The phone leaves BT-only and joins `br0`.

> **CORRECTED 2026-09-09 — outbound paging from the box WORKS, and it is now the normal path.**
> This block used to read "The phone must initiate the BR/EDR connection. Outbound paging from the
> box fails (`CONNECT_FAILED status=0x07`, `Resource busy`, `Connection refused`) — observed every
> time." That was already contradicted by `04_SYSTEM_MODEL.md` §4c.4 (2026-09-08) and is now
> contradicted by a full capture: the box drove the entire reconnect itself, unprompted —
> `[reconnect] 1 bonded phone(s) — driving reconnect when idle` → `[sdp-client] L2CAP SDP channel up
> (phone paged)` → `iAP2 RFCOMM channel on the phone = 1` → `[reconnect] RFCOMM connected OUT to the
> phone (ch 1) — starting iAP2 handshake` → cert → sign → Identify → `0x5703`.
>
> What actually fails is a **one-sided bond**: the box holds a link key the phone has discarded, the
> authenticated RFCOMM connect times out after ~8 s, and iOS reports `keys available ? No`. The fix
> is to clear BOTH sides and re-pair, not to tap the phone. Full account in `04_SYSTEM_MODEL.md`
> §4c.4 and in "The half-bond" below. Triggering from Settings ▸ General ▸ CarPlay on the iPhone
> remains a valid manual kick, but it is no longer a precondition.

There is **no join confirmation** over iAP2; a successful `0x5703` write is the only signal, and iOS
retries `0x5702`, so the handler must be idempotent.

> **"The box raises `hci0`" has a precondition, and when it is unmet NOTHING says so.**
> Device-diagnosed 2026-08-28. The supervisor brings radios up via
> `sh /script/radio_hal.sh bt_on >/tmp/bt.log 2>&1`, inside a detached `setsid` wrapper whose exit
> status it never reads. `hci_uart` is a **loadable module** on the CCPA, shipped inside
> `/lib/firmware/nxp/iw416_ko.tar.gz`, and the seam is what extracts and `insmod`s it. If
> `/script/radio_hal.sh` or `/script/radio_detect.sh` is missing — the state a targeted
> `ocbm_push.sh` leaves, since it pushes `ocbmd` + `btd` and nothing else — the `n_hci`
> line discipline is never registered, `hciattach` fails `EINVAL`, and `hci0` never appears.
>
> Every layer above still reports success: OCBM claim, `CT_HELLO_ACK`, a real 945-byte certificate,
> a real 128-byte signature, `CT_SUBSCRIBE`, `HOST_PRESENT`. The app looks healthy and the phone
> simply never sees a car.
>
> **Read `CT_BOX_HEALTH` first.** `BH_HCI_PRESENT` (bit 0) clear means *no controller registered at
> all*, which is this fault — not "Bluetooth is down" (the sysfs node survives `hciconfig hci0
> down`). A health of `0x50` (`btd|rootfs-ok`) with bit 0 clear is the exact signature.
> The box's own `/tmp/bt.log`, `/tmp/wl.log` and friends are now streamed into logcat as
> `[box:<file>]` lines, so the box side of this lands in the same capture as the app side.
> See `ccpa_custom` `docs/ops/06_CORRECTIONS_LEDGER.md` R-20W-5.

## Phase 4 — App becomes the accessory on Wi-Fi

```
[cprx ] CarPlay RX up on :7011 deviceid=B2:D6:2A:9F:C9:30 features=0x44440B80,0x61
[cprx ] advertise _airplay._tcp:7011 REGISTERED as 'gm-ccpa'
[cprx ] advert not yet REGISTERED — holding the connect-out until it is   (only if it lags)
[mdns ] live lookup: Owner iPhone._carplay-ctrl._tcp.local -> target=Owner-iPhone.local port=57347
                     addrs=fe80::c0b:7143:d356:a90, 192.168.5.57
[cprx ] connect-out accepted on attempt 1 via fe80::c0b:7143:d356:a90%br0:57347
```

Three things are load-bearing:
1. **The Car bit** (high word bit 32) in `features`, or iOS answers the connect-out and never opens RTSP.
2. **Advertise before dialling.** `GET /ctrl-int/1/connect` does not make iOS connect — it sets a
   *pending autoconnect* resolved immediately against an already-populated index. The gate is the
   **`onServiceRegistered` event** plus a 2.5 s margin, not a fixed wait: registration completing is
   observable, whereas "iOS has indexed us" is not, and a flat 15 s guess was pure added latency.
3. **`AirPlay-Receiver-Device-ID` is a decimal uint64**, not a MAC string. TXT `deviceid` and `/info`
   `deviceID` keep the colon MAC — the asymmetry is deliberate.

> ### ⚠ FAILURE POINT 2 — `NsdManager` serves a stale address AND a stale port
> The platform cache is unreliable in both fields, and **restarting discovery does not clear it** — the
> new resolve is answered from the same cache. iOS also rotates its control port constantly
> (observed `57236 → 57292 → 57347 → 57383` within minutes), so a cached port is dead within about a
> minute of being correct: right address + cached port ⇒ `ECONNREFUSED`; dead address ⇒ 3 s timeouts.
> **The wire is the authority.** `MdnsInspect.liveEndpoint()` queries SRV then A/AAAA directly and
> `CarPlayRx.dialCandidates()` dials those first, re-querying at attempts 3 and 6; the cached pair is
> kept only as a last resort because the wire query can occasionally miss a reply.
>
> Diagnose it from the neighbour table — `INCOMPLETE` against the address in the `resolved '<peer>'`
> log line means you are dialling a ghost:
> ```bash
> adb shell ip neigh show dev br0
> adb shell "ping6 -c 2 -W 2 <resolved-addr>%br0"   # "Address unreachable" = confirmed
> ```

> ### ⚠ FAILURE POINT 5 — the phone reads our TXT and never resolves SRV (added 2026-08-27)
> **Signature.** The phone is on `br0`, advertises `_carplay-ctrl._tcp`, and answers
> `GET /ctrl-int/1/connect` with `200 OK` — and never opens the control connection. Reproduced three
> times on 2026-08-27; the app dialled 11 times in one run with a textbook-looking log.
>
> **What the phone is actually doing.** `airplayd` browses `_airplay._tcp`, queries **TXT** for every
> instance it finds, and stops. It issues no SRV query, so it holds no address to dial. Across a whole
> capture there were exactly two SRV queries for our adverts, and both were immediately followed by a
> working session; every failure had none. That correlation is the diagnostic — 2 for 2 on success,
> 0 for 3 on failure.
> ```
> 13:25:53  QueryRecord START -- qname: gm-ccpa._airplay._tcp.local, qtype: SRV   → session came up
> 13:46:50  QueryRecord START -- qname: gm-ccpa._airplay._tcp.local, qtype: SRV   → session came up
> ```
> Capture it with `idevicesyslog -p 'mDNSResponder|carkitd|wifid'` and grep `qtype: SRV`. Note the
> instance name: iOS has **never** SRV-queried `gmccpa-rx`, the self-hosted advert. Both working
> sessions resolved `gm-ccpa`, the NsdManager one — see Failure Point 3, whose fix may be load-bearing
> in a different way than it was written to be.
>
> **What does NOT fix it.** Verified by doing all of it and re-testing: power-cycling the box to
> `uptime_s: 5`, relaunching the app into a fresh process, re-binding and re-announcing the receiver,
> re-pairing Bluetooth, re-joining Wi-Fi through the proper CarPlay prompt. The phone still stopped at
> TXT. Nothing reachable from the head unit clears this.
>
> **What does.** Reconnecting Bluetooth to the car **from the iPhone**, every time. Note that
> "Forget This Car" alone is not enough while the box is connectable — iOS re-pairs instantly and
> re-creates the vehicle record (`using cache with 1 vehicles`, never 0). Take the box's Bluetooth
> down first, then forget, then re-pair.
>
> **Do not confuse with Failure Point 2.** A live mDNS answer for the *peer* does not help here; the
> failing runs resolved the phone perfectly. The gap is the phone's resolution of **us**.
>
> **What the app does now.** `CarPlayRx` counts accepted-but-unanswered dials and at six reports
> `STALLED` with the one instruction that works, instead of redialling silently forever
> (`04_SYSTEM_MODEL.md` §4b).

## Phase 5 — The phone dials back

```
HandleControlServerEvent command 'connect' received for deviceID: B2:D6:2A:9F:C9:30
Setting pending autoconnect deviceID: B2:D6:2A:9F:C9:30
>>> INBOUND CONTROL CONNECTION on :7011
```

> ### ✅ SOLVED 2026-08-12 — coexistence works. GM stays running. Do not fight it.
>
> **Read this before touching anything to do with GM's CarPlay service.** Every prior session has
> re-derived "GM must be stopped" from the older wording here and wasted hours on it. It is wrong.
> GM's `CarplayService` runs, advertises on `:7000`, and is **irrelevant** once our advert is correct.
>
> **The real mechanism.** iOS keys its Wi-Fi CarPlay endpoint index by **host**, one endpoint per host,
> and the *first* Bonjour record for that host wins. Captured from the iPhone, timestamps exact:
> ```
> 21:38:04.548641  Bonjour TXT Add CarPlay._airplay._tcp.local.       <- GM's
> 21:38:04.548668  Bonjour TXT Add gm-ccpa._airplay._tcp.local.       <- ours, 27 MICROSECONDS later
> 21:38:04.549866  Created APEndpointCarPlay [0xE051] with name 'CarPlay' and id 'F8:6D:CC:DC:32:D6'
> 21:38:04.549876  Added new WiFi endpoint 0xE051 (CarPlayControlSupported: Y)
> ```
> Both records arrive; iOS creates **one** endpoint and stamps it with GM's identity. Ours is
> discarded — not rejected, just folded in. Every later autoconnect then fails looking for an ID that
> was never indexed:
> ```
> carManager_handlePendingAutoconnect: index = WiFi, for deviceID: F8:6D:CC:DC:32:D6,
>                                      pendingAutoconnectID: B2:D6:2A:9F:C9:30
> carManager_handlePendingAutoconnect: No matching endpoint found for deviceID B2:D6:2A:9F:C9:30
> HTTP connection closing: -6753/0xFFFFE59F kConnectionErr
> ```
> The cause is that **both SRV records targeted the same host, `Android.local`** — the platform
> hostname. It is a race we lose whenever GM registers first, which it always does: GM starts at boot,
> we start ~51 s later. It was never about the port (`:7011` vs `:7000`), the instance name, the TXT
> fields, or our deviceID — all of those were already distinct and correct.
>
> **The fix, implemented and confirmed on hardware.** Publish a **second advert on a hostname we own**.
> `NsdManager` cannot do this — AOSP drops `NsdServiceInfo.setHost()` on registration — so
> `MdnsResponder.kt` answers mDNS itself: instance `gmccpa-rx`, SRV target `gmccpa-rx.local`, and a
> single A record pointing at the `br0` bridge (`192.168.5.1`). It runs **alongside** the NsdManager
> advert, wired in `CarPlayRx.advertise()`. Result, same rig, GM running untouched:
> ```
> [cprx ] self-hosted advert up: gmccpa-rx.local -> br0:7011
> [cprx ] >>> INBOUND CONTROL CONNECTION from fe80::...%br0:59419
> [cprx ] native receiver core started for ...:59419 gen=5 (/info 1969B)
> [rust ] pair-verify OK -> control channel encrypted
> [cprx ] *** MILESTONE: control channel is ENCRYPTED — pair-verify completed
> ```
> Pinning one address also fixes a second, independent bug: `Android.local` resolves to **three**
> addresses (`192.168.1.100`, `172.16.4.100`, `192.168.5.1`) and only the last is routable from the
> phone's subnet, so resolution was a coin flip across an 8-interface head unit.
>
> **Acceptance test** — on the phone, look for a second `Created APEndpointCarPlay ... id
> 'B2:D6:2A:9F:C9:30'`. Absence of errors proves nothing; that line is the proof.
>
> ### ⛔ NEVER force-stop or disable GM's CarPlay service
> Not as a workaround, not as a diagnostic, not "just once to check". Owner-instructed, 2026-08-12:
> **GM must stay active and ignored.** Stopping it **costs ADB access** — `com.gm.domain.server.delayed`
> also holds REBOOT / SECURE_SETTINGS / OTA (`01` §3, `02`) — so you lose the log channel, the install
> path and scripted bring-up in one go: everything needed to observe the session you were trying to
> enable. It cannot be disabled from shell (`SecurityException: Shell cannot change component state`)
> and returns on every reboot regardless.
>
> The historical `am force-stop` result is retained only as an explanation of *why the old theory
> looked right*: stopping GM freed the single per-host endpoint slot, so we won it by default. That was
> the symptom, not the cause. `pm uninstall --user 10` is **not** a fallback and should not be offered.

> A *similar-looking* but different failure is the stale iOS endpoint cache, whose fix is forgetting the
> vehicle on the iPhone and re-pairing. Distinguish them: cache staleness shows an index entry for **our**
> deviceID that has expired; coexistence shows an index entry for **`F8:6D:CC:DC:32:D6`**.

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

- `/auth-setup` is the only place the OCBM link sits on the critical path mid-session (~1.7 s, nearly all
  of it the chip's signature poll).

> ### ⚠ FAILURE POINT 4 — `/auth-setup` fails when the OCBM/USB link is dead → iOS `-72542`
> Because auth-setup is the one mid-session step that needs the adapter, a dead USB link kills the
> session *there* and nowhere earlier. Pairing completes perfectly first, which makes it look like a
> protocol problem when it is a transport problem. Signature on our side:
> ```
> [rust ] pair-verify OK → control channel encrypted
> [rust ] POST /auth-setup (enc, 33 B body)
> [ocbm ] >> CH_MFI create_signature (20B digest)
> [usb  ] write FAILED: bulkTransfer OUT failed at offset 0 (consecutive=12)
> [rust ] auth-setup FAILED: Signer(Custom { kind: Other, error: "createSignature threw" })
> ```
> and, decoded from the iPhone (2026-08-12) — **`-72542` has no `k…` symbol in Apple's logs**, so record
> it here: it is the MFi-authentication failure raised by `mfiAuthentication_AuthenticateEndpoint`:
> ```
> Request written: CID …, Header 128 bytes, Body 33 bytes      <- the MFi challenge
> ### Error: -6753/0xFFFFE59F kConnectionErr                    <- we closed the socket
> mfiAuthentication_AuthenticateEndpoint:297: got error -72542/0xFFFEE4A2
> ### MFi failed: -72542/0xFFFEE4A2
> Deactivating endpoint 'gmccpa-rx' with reason 'Activation Failed'
> ```
> `isLikelyAccessoryIdleTimeout: no` with ~10 ms since the last response proves the accessory *actively
> closed* the connection rather than timing out — i.e. look at the transport, not at timing.
>
> **Root cause seen in practice: two OCBM bring-ups in one app process.** The second `claimInterface`
> kills the first handle, but the receiver's MFi signer stays bound to the first, now-dead `OcbmClient`.
> The tell is `[cprx] acceptLoop: BindException: EADDRINUSE` followed by a second `claimed interface 0`,
> then `read: rapid -1 x50 — device gone`, and a `consecutive=` counter that only ever climbs (it resets
> to 0 on any successful write, so a monotonic climb means *nothing* has been written since).
>
> Fix: one bring-up per process. `adb shell am force-stop zeno.gmccpa` before `am start` if
> you are re-launching by hand — starting the activity while an instance is already live is enough to
> cause it. Verify with `grep -c "claimed interface 0"` — it must be exactly **1**.
- **Streams are not batched.** `audio(102)` arrived ~4 minutes after `screen(110)` in the reference run.
  Expect SETUPs throughout the session.
- **Stream 130 accepted with a non-zero `streamID` is current behaviour** and is newer than the
  2026-08-05 session, where it was rejected roughly once a second forever. The metadata plane therefore
  exists, but nothing consumes it yet — `[meta] seam 127.0.0.1:9004 unavailable` is expected today.

## Phase 7 — Localhost seam → MediaCodec

Rust `session.rs` (`spawn_screen`/`spawn_audio`) dials **outbound** to `127.0.0.1:9001` and `:9002`, so
the consumer must be listening **before** stream SETUP or every access unit is dropped (`connect carlink
:900x failed`). `forward.rs` supplies only the byte-transform applied to each access unit before that
write (AVCC→Annex-B for video, raw AAC-LC→ADTS for media audio) — it contains no socket, no `connect`;
the dial and the log line `"[screen] iPhone connected from {peer}; forwarding video →
127.0.0.1:{sink_port}"` are in `session.rs`. (Corrected 2026-09-09 — the doc previously attributed the
outbound dial to `forward.rs`.)

> ### The consumers stand by; they are not started on demand
> `CarPlayRx` fires `onSessionUp` the moment the phone opens the control connection — before
> pair-verify, ~3 s before any stream SETUP — and `MainActivity` brings the CarPlay screen up from it.
> The seams, threads and Surface therefore exist before the phone asks, and `AacPlayer.prime()` has
> already built a playing `AudioTrack` (safe: stereo AAC-LC 48 kHz is a wire-format ceiling, and a
> format mismatch discards the primed track rather than imposing the wrong rate).
>
> **This, not decoder warm-up, was the old multi-second video lag.** Warm-up measured 151 ms all
> along; the lag was that no consumer existed until an operator ran `carplay_ui`, so from RECORD
> onward the phone streamed into a closed port. Expect:
> ```
> [aac  ] primed AudioTrack 48000Hz 2ch — waiting for the stream
> [cpui ] renderer attached to Surface          <- during pair-setup
> [aac  ] adopting the primed AudioTrack
> ```
> **Video is deliberately NOT pre-configured.** `csd-0` arrives in-band and the IDR carries
> VPS+SPS+PPS itself (see below), so a standing-by renderer configures from the keyframe message
> alone — no format guess, no reconfigure when a guess is wrong.
>
> **Audio still leads video by ~184 ms and that is irreducible:** audio plays on its first ADTS frame,
> video must wait for an IDR.

| Seam | Framing |
|---|---|
| `:9001` video | `[u32 BE len][Annex-B payload]`, one message per screen message |
| `:9002` audio | **no** prefix — raw ADTS |
| `:9003` voice | consumed by `VoiceRouter` → Siri→`Voice`, call→`Phone`/`Call`, nav→`Navigation`, each owner-confirmed audible on the truck 2026-09-04. (The `:9003 (voice, drained)` lines in the 2026-08-12 captures predate `VoiceRouter` and are stale.) |
| `:9004` metadata | **LIVE since 2026-09-09** — `[cpui] meta seam connected` / `[rust] [meta] connected to ocbmd metadata seam 127.0.0.1:9004`. Carries now-playing deltas (543 `0x5001` frames in one session) and album art (a 76,869 B JPEG reassembled over the iAP2 session-2 file transfer). The old "unavailable — deferred by decision" row, and the `[meta] seam 127.0.0.1:9004 unavailable` line said to be "expected today", are both retired |

```
[hevc ] VPS (28 B)  SPS (66 B)  PPS (11 B)
[hevc ] MediaCodec configured: video/hevc 2400x960 csd-0=105 B (VPS+SPS+PPS), decoder=OMX.Intel.hw_vd.h265
[hevc ] keyframe — decoding starts   →   FIRST FRAME RENDERED
[aac  ] configured AAC-LC 48000Hz 2ch → AudioTrack (USAGE_MEDIA)   →   FIRST AUDIO FRAME PLAYED
[cpui ] touch down n=(0.367, 0.239) sent=true
```

- `csd-0` for HEVC is **VPS+SPS+PPS concatenated**, not the SPS/PPS pair H.264 uses.
- Parameter sets arrive **twice** — standalone, then again in-band inside the IDR access unit. Skip only
  a *pure* parameter-set message; discarding the second set throws away the keyframe and yields a black
  screen on a healthy session.
- Touch goes back as normalised 0–1 HID reports on the encrypted event channel.

---

## Phase 8 — Session end and recovery (device-reported 2026-08-28, fixed)

Two teardown gaps were found on the same device session and both are now fixed:

- **A dropped session left a frozen screen up.** Nothing used to tell `CarPlayActivity` a session had
  ended: `CarPlayRx.fireSessionDown` reached `MainActivity.onCarPlaySessionDown`, which cleared app
  state and stopped there. When the phone went out of Wi-Fi range, the pump timed out, the session was
  correctly declared down, and the last decoded video frame stayed on the Surface indefinitely — a
  frozen CarPlay screen over a dead session, still swallowing touches. Fixed by
  `CarPlayActivity.onSessionEnded()` (companion fun, `av/CarPlayActivity.kt`), which finishes the Activity rather
  than clearing the Surface and staying up — finishing is also what makes a *resume* work: `startSession`
  returns early on "already started," so a stale live Activity would otherwise absorb the returning
  phone's `onSessionUp` and the screen would never rebuild.
- **A permanent audio-focus loss was never recovered.** This head unit enforces focus
  (`dumpsys car_service`: `Use hal ducking signals true`); `AUDIOFOCUS_LOSS` (permanent, as opposed to
  transient) paused and abandoned focus correctly, but nothing re-requested it — the old assumption
  that "the next `start()` re-requests" was false, so media stayed silent even though Siri and phone
  calls (on a separate track, `VoiceRouter`) were still audible. Fixed by
  `AacPlayer.reclaimFocus()` (`av/AacPlayer.kt`), called from `feed()` — i.e. only when audio is
  actually arriving, which is the honest signal media is meant to be playing — rate-limited to one
  attempt per `FOCUS_RETRY_MS` since a focus request is a binder call and a refusing head unit keeps
  refusing.
- **Transient form — FIXED 2026-09-08 (`b8e8736`), truck re-measurement not yet recorded:** after a
  call or Siri turn ended the `Voice` track lingered and media was silent for **10–20 s**
  (device-measured 12.0 s). The assistant edge and the focus edge ran on different clocks;
  `ASSISTANT.idleMs` is now `ASSISTANT_HOLD_MS + 1` sweep period, closing the gap to ~1 s.
  `reclaimFocus()` covers the permanent hang, which is a separate fault. See `13_AUDIO_ROUTING.md` §3a.

---

## Bring-up in practice

Exact commands: [`06_BRINGUP_RUNBOOK.md`](06_BRINGUP_RUNBOOK.md) §5g. The short form, in order:

1. Revive logging — `setprop persist.log.tag V`, `logcat -G 16M`. It boots effectively dead and silence
   looks exactly like a dead app.
2. Re-assert `/tmp/no_escalate` on the box (tmpfs — it self-clears on every box reboot, re-arming the
   reboot ladder).
3. Launch with credentials: `--es run full --es ssid '…' --es pass '…'`.
4. Initiate CarPlay **from the iPhone**.

`--es run carplay_ui` is no longer part of bring-up — the screen starts itself from the session event.
It remains available to *restore* the screen after something backgrounds it.

> ### ✅ FIXED IN CODE 2026-08-28 (T5.3) — NOT YET DEVICE-CONFIRMED — the iAP2 metadata tunnel's MFi calls
> Symptom, present from at least 2026-07-22 through 2026-08-27 (`evidence/session_2026-08-12-freshpair/`,
> `evidence/session_2026-08-12-standby/`, `evidence/tri_20260818-052029/`): on an otherwise healthy,
> streaming session, `DataStream(130)` sets up, the tunnel gets its SYN-ACK, and then every 120 s until
> the attempt budget is gone:
> ```
> [iap-tunnel] TX detect+SYN over AirPlay tunnel — starting fresh iAP2 session
> [iap-tunnel] RX SYN-ACK — link up (zero-ack=true), ACKing
> [iap-tunnel] MFi cert: lock busy (another chip user holds /tmp/carplay_mfi.lock) — not retrying
> [iap-tunnel] RX 0xAA00: action failed, state held
> [iap-tunnel] handshake budget (120s) expired at Some(Init) — discarding and rebuilding (attempt N/3)
> ```
> Video and audio were unaffected; what was lost was everything the tunnel carries — NowPlaying
> metadata, route guidance, the controls channel.
>
> **The "lock busy" message was always wrong.** `MfiLock::acquire()`
> (`crates/vendor/mfi-i2c-local/src/lib.rs`) opens `/tmp/carplay_mfi.lock` with `O_CREAT`, and `/tmp`
> does not exist on Android (`ls: /tmp: No such file or directory`). The `open` fails `ENOENT`,
> `acquire()` returns `None`, and every `None` was mapped to `MfiError::LockBusy` — a missing directory
> reported as contention. Even a writable lock path would not have helped: the next step,
> `mfi_i2c_local::try_cert`/`try_sign` direct on `/dev/i2c-1`, is SELinux-blocked for an ordinary app UID
> (`evidence/03`) regardless of the `crw-rw-rw-` DAC bits.
>
> `iap_tunnel.rs` was calling the local-chip path directly instead of the `MfiSigner` the
> `ControlServer` already held — on the head unit that signer is `RemoteMfiSigner`
> (`struct RemoteMfiSigner`, `native/carplay-jni/src/lib.rs`), which relays over OCBM `CH_MFI` to the box and is the only
> chip path that works from Android (the same one `/auth-setup` uses successfully every session).
>
> **Fix.** `receiver::iap_tunnel::set_remote_signer()` (`pub fn set_remote_signer`, `crates/vendor/receiver/src/iap_tunnel.rs`)
> installs an `Arc<Mutex<dyn MfiSigner>>` the tunnel now calls instead of `mfi_i2c_local` when one is
> present; `mfi_i2c_local` is compiled in only under `local-mfi` (on-box builds), so an Android build
> without a remote signer now fails loudly instead of silently. The embedder wiring is
> the `receiver::iap_tunnel::set_remote_signer(...)` call in `Java_zeno_gmccpa_pair_NativeCore_nativeInit` (`native/carplay-jni/src/lib.rs`), landed in `ed3329f` (2026-08-28), the same commit as the
> box-log-to-logcat change.
>
> **DEVICE-CONFIRMED 2026-09-09.** *(Was: "has not yet been exercised on a live truck session — no
> capture post-dates the fix.")* The capture is exactly the verification this block asked for. The
> tunnel got real chip round trips through the relay, not the lock-busy line:
> ```
> [iap-tunnel] remote MFi signer installed — tunnel chip ops go through the embedder, not /dev/i2c-1
> [iap-tunnel] TX detect+SYN over AirPlay tunnel — starting fresh iAP2 session
> [iap-tunnel] RX SYN-ACK — link up (zero-ack=true), ACKing
> [iap-tunnel] TX 0xAA01 AuthenticationCertificate (tunnel)   ← [jni] MFi certificate via OCBM relay: 945 bytes
> [iap-tunnel] TX 0xAA03 AuthenticationResponse (tunnel)      ← [jni] MFi signature via OCBM relay: 128 bytes
> [iap-tunnel] AuthSuccess → RX 0x1D02 -> Identified
> [events] iAP2-tunnel metadata: 3 subscribes (now_playing, route_guidance, call_state)
> ```
> Zero `lock busy` lines, zero handshake-budget expiries, and the metadata plane then delivered
> NowPlaying and artwork for the rest of the session. **Six chip ops per session is now observed, not
> derived.** One new warning surfaced and is benign on this box role:
> `[iap-tunnel] WARN /sys/class/bluetooth/hci0/address unreadable -- param 17 carries a placeholder
> BD address` — the head unit has no local HCI, and the phone accepted the Identify anyway.

### Clearing a pairing — there are THREE independent stores

Clearing one clears nothing else, which is why forgetting the vehicle on the iPhone still leaves the
box paging the phone and the phone showing an unknown-accessory prompt (observed 2026-08-12):

| Store | Cleared by |
|---|---|
| iPhone's CarPlay/BT record | forget the vehicle on the iPhone — **but take the box's Bluetooth down first**, or it re-pairs and re-creates the record before the forget settles (observed 2026-08-27) |
| **Box's BT link key** | the **Forget Pairing** button, or `--es run ocbm_forget` |
| App's AirPlay LTPK (`carplay_peers.bin`) | the **Forget Pairing** button, or `adb uninstall` |

**Updated 2026-08-27:** the last two rows used to be independent, and the button that cleared the box's
bond left the app's peer store untouched. That split brain presents as `pair-verify` failing — which
reads as broken crypto and sends you debugging code that is fine. Forget Pairing now clears both
together; the iPhone's record remains yours to clear by hand.

A genuinely fresh pairing shows the full `pair-setup` M1/M3/M5 in the log. If it fast-paths straight
to `pair-verify`, a peer store survived and the test was not clean.

#### The half-bond, and how it presents (device-observed 2026-09-08)

Clearing exactly one of the BT stores leaves a **one-sided bond**, and it does not look like a pairing
problem from either end alone. The box holds a link key the phone has discarded, so its outbound
reconnect gets all the way through SDP and then dies at authentication:

```
[box:wl] [sdp-client] querying SDP on <phone> (PSM 0x0001)
[box:wl] [sdp-client] L2CAP SDP channel up (phone paged)
[box:wl] [sdp-client] iAP2 RFCOMM channel on the phone = 1
[box:wl] [reconnect] RFCOMM connect to channel 1 failed: connect timed out    <- ~8 s later
```

The phone's own account of the same moment names the cause exactly:

```
bluetoothd: Not delaying security enforcement for Address=<box> keys available ? No pairing state:(STATE_IDLE)
bluetoothd: Call to retrieve key chain magic key data of type 0 for device <box> failed with result 150
BluetoothSettings: Setting cell "CarLink-xxxx" paired 1 and connected 0
bluetoothd: Connection to device <box> failed - result was 431
bluetoothd: Received connection result for "Wireless IAP" profile on device <box> - result was 20431
```

`paired 1` with `keys available ? No` is the signature: the Settings record exists, the keychain entry
does not. Tapping the device gives "can't connect" forever.

**Two traps in reading this.** First, the box logs an SDP `ServiceSearchRequest -- 0 match(es)` just
before the phone hangs up, which looks like the phone rejecting our service records — it is not. iOS
enumerates all four records successfully first, and the 0-match search is incidental; the connection
dies at security enforcement, not at discovery. Second, the box's `reconnect` loop keeps retrying, so
the log fills with plausible-looking activity while nothing can ever complete.

**Remedy: clear BOTH sides, then re-pair.** `--es run ocbm_forget` (`MGMT_FORGET_ALL`) on the box, and
Forget This Device on the iPhone. Clearing one produces the same half-bond in the other direction.
After a genuine two-sided clear the box logs `LOAD_LINK_KEYS(count=0)` and the next pair writes
`NEW_LINK_KEY ... store_hint=1` — that pair of lines is the check that the slate is actually clean.

Health check (derive the uid — it changes on every reinstall):
```bash
APPID=$(adb shell dumpsys package zeno.gmccpa | sed -n 's/.*userId=\([0-9]*\).*/\1/p' | head -1)
adb shell "cat /proc/net/tcp6" | awk -v u="$((1000000 + APPID))" '$8==u && $4=="01"'
```
4–6 established sockets plus `POST /feedback` every 2 s is a healthy session.


---

## Vehicle state → CarPlay: drive-restricted UI and day/night

**Device-proven 2026-09-08 on gminfo37, owner-observed both directions.** `av/VehicleStateWatcher.kt`
drives two CarPlay runtime levers from AAOS vehicle state:

| Lever | Command | Source | Status |
|---|---|---|---|
| Drive-restricted UI | `setLimitedUI` | `GEAR_SELECTION` (`0x11400400`), UXR fallback | **Proven** — Apple Maps keyboard icon goes on shift out of Park, returns in Park |
| Day/night | `setNightMode` | `NIGHT_MODE` (`0x11200407`), `uiMode` fallback | **Proven** — carries to the CarPlay UI |

Both are pure runtime `/command`s on the live event channel: **no reconnect, no SETUP negotiation.**
`/info` still matters — see the element list below — but nothing about pairing or capabilities moves.

### The permission table is why this is possible at all

Measured on this head unit 2026-09-08. An unprivileged app in `/data/app` gets the two that matter:

| Permission | Protection | Gates |
|---|---|---|
| `CAR_POWERTRAIN` | **`normal`** | `GEAR_SELECTION` |
| `CAR_EXTERIOR_ENVIRONMENT` | **`normal`** | `NIGHT_MODE` |
| `CAR_SPEED` | `dangerous` | speed (unused) |
| `CAR_DRIVING_STATE` | `signature\|privileged` | **out of reach** |

`CarDrivingStateManager` gives the cleanest `PARKED/IDLING/MOVING`, and we cannot have it. Gear plus
`CarUxRestrictions` (which needs NO permission) covers the same ground.

### Two findings that cost time, recorded so they are not re-derived

**1. `uiMode` is NOT the night source on this head unit.** `carlink_native` reads Compose
`isSystemInDarkTheme()` — correct on the old Carlinkit firmware. Here, with vehicle night active,
`cmd uimode night` reported `no` and `Configuration.uiMode` read day: **GM does not propagate vehicle
night state into Android's uiMode.** The `NIGHT_MODE` vehicle property does carry it. `uiMode` is kept
only as a fallback for a head unit that wires it.

**2. `setLimitedUI` does nothing without `/info` `limitedUIElements`.** `limitedUI` is a boolean;
the element array is the SET it applies to. iOS builds a bitmask from that array, so with no array the
mask is 0 and the command restricts nothing — acked 2xx, no effect. We declare
`["softKeyboard","softPhoneKeypad","musicLists","longUserAlert"]` (`tools/info_plist_limitedui.py`),
mirroring what the macOS box serves in a session where the toggle demonstrably works. `softKeyboard`
is the Apple Maps keyboard icon. This corrects a "REFUTED" verdict upstream — see
`docs/carplay/03_SDK_GROUND_TRUTH.md` §10.

### Session-scoped, and the assert must retry

Both commands are refused with no event channel, so state is pushed fresh on session-up, never
persisted across sessions. **`CarPlay live` and "event channel usable" are not the same instant:**
measured 2026-09-08, the supervisor logged `awaiting dial-back -> CarPlay live` at 12:52:07.779 and
both commands were refused at 12:52:07.833 — 54 ms later. That run only recovered because the driver
happened to shift. `reassert()` therefore retries at 400 ms up to 20 times; verified sent at session-up
afterwards.
