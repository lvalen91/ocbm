# Android Auto — session state, A/V, input and open items

> **STATUS:** CURRENT · single owner for this topic. Split out of `docs/host/02_ANDROID_AUTO.md` on 2026-08-31 when Android Auto got its own category. Correct this file in place — do not add a sibling.

**Contents:** what works today → resolved defects and the causes worth keeping → open items.

Design and transport live in [`00_ARCHITECTURE.md`](00_ARCHITECTURE.md); arbitration in
[`02_ARBITRATION.md`](02_ARBITRATION.md); wireless in [`03_WIRELESS.md`](03_WIRELESS.md).

### 1. Current state

Android Auto runs end to end over OCBM/USB: the box switches the phone into AOAP and pumps the TLS
byte stream over `CH_IP` to `ccpa/aa-bridge`; the macOS app terminates the AA session, decodes H.264
through the same VideoToolbox path CarPlay uses, plays all three audio sinks through the same engine,
and sends input back on the AA input channel. The box selects AA on its own — no env var — and
CarPlay/AA arbitration is settled ([`02_ARBITRATION.md`](02_ARBITRATION.md)).

- **Video:** streams indefinitely. Startup FIFO deepens to 64 then shrinks to 2 after 30 decoded
  frames, which killed the warm-up drop (slotDrops=0 through 270 frames).
  **No media acks at protocol ≥ 6.0 (2026-09-04).** gearhead 17.5 builds its video and audio
  endpoints with ack handling disabled when the negotiated protocol is 6.0 or newer (`ivc.c` →
  `jdk.o`): every MEDIA_ACK we sent on channels 3/4/5 fell through its dispatch and was logged as
  `Received message with invalid type header: 32772` — 182,780 lines in one morning on the Pixel,
  with video and audio unaffected because the phone ignores them. Since we request 6.1 (the 2 s IDR
  interval), the session now sends no media acks when the phone answers ≥ 6.0; a phone answering
  < 6.0 still gets them. Verified: 0 drops, and the phone's count stopped growing.
  **All nine codec tiers device-verified 2026-09-04** (Pixel 10, gearhead 17.5, wireless, one at a
  time via `AA_FORCE_RES` / `AA_FORCE_FPS`; phone-side decision read from `CAR.VIDEO` "Checking
  video config … isAllowed" / "Accessory display sink available: DisplayParams(…)"):

  | Tier | Codec declared | fps asked → got | Result |
  |---|---|---|---|
  | 1 800×480, 2 1280×720, 3 1920×1080 | H.264 | 60 → 60 | streaming (2026-08-27 / 09-04) |
  | 4 2560×1440 | **H.265** | 30 → 30, 60 → 60 | streaming, 0 drops; as H.264 the phone answers "not allowed for the codec type" → "No working configuration" and closes the transport |
  | 5 3840×2160 | **H.265** | 30 → 30, 60 → 60 | streaming, 0 drops |
  | 6 720×1280 | H.264 | 60 → 60 | streaming |
  | 7 1080×1920 | H.264 | 30 → 30 | streaming; owner-confirmed scaling and touch |
  | 8 1440×2560, 9 2160×3840 | **H.265** | 30 → 30 | streaming, 0 drops |

  gearhead caps H.264 at 1080p in either orientation (`ivf.B`), so `AACapability.Resolution.needsHEVC`
  declares `MEDIA_CODEC_VIDEO_H265` (7) for tiers 4/5/8/9 and the session parses HEVC VPS/SPS/PPS
  from CODEC_CONFIG. The decompile's "60 fps only up to 1920×1080 pixels" branch did not fire on this
  phone (its SDK is past the threshold); other phones may downgrade to 30 above 1080p. On 2.4 GHz
  Wi-Fi gearhead refuses every tier above 1280×720 (`"not allowed due wireless frequency"`); the box
  AP is 5 GHz.

  **Non-tier panels (T4, 2026-09-04):** a profile that is not a tier declares the smallest tier whose
  aspect-fit sub-rect contains it plus `VideoConfiguration.width_margin` / `height_margin`; gearhead
  lays the UI out in `codec − margins` with the margins split evenly (`iux.c`), sends touch relative
  to that visible rect (`jjd` adds dispLeft/dispTop; the touchscreen width/height fields are ignored),
  and the app locks the window to the visible aspect, centre-crops the codec frame and maps touch
  bounds-relative. `AA_MARGINS=0` reverts to whole-tier declaration; `AA_PANEL=WxH` is the bench
  geometry lever. `ui_config.margins` (four-sided) exists for asymmetric placement and is not used.
  Device result, `AA_PANEL=2400x960`: declared tier 2560×1440 (H.265) with `height_margin 416`; the
  phone answered `DisplayParams(codecWidth=2560, codecHeight=1440, fps=60, dispWidth=2560,
  dispHeight=1024, …)` — the visible rect exactly as declared — and streamed with 0 drops. Owner-confirmed: the window locks to 2.5:1, the UI fills it edge to edge, and touch is accurate
  including at the cropped top and bottom edges. T4 is landed; remaining refinements (density from a
  physical panel size, `ui_config.margins` for asymmetric placement, a fallback configuration list)
  are in `../ops/08_FUTURE_TASKS.md`.
  **Density (2026-09-04):** `VideoConfiguration.density` is the DPI gearhead gives the virtual
  display it renders into (`createVirtualDisplay` with the declared value, unclamped), so UI
  elements scale by density/160 in pixels while tier, margins and the visible rect stay fixed, and
  the layout is chosen from the resulting point width. Measured on the 2400×960 case: at 160 the
  phone logs `layout rail height:80`, at 240 `rail height:120` (×1.5), same `dispWidth=2560,
  dispHeight=1024`, no "Calculated height too large" clamp. `real_density` (field 9) is only
  logged. Bench lever `AA_DENSITY=<80…640>`; the profile setting belongs to the settings redesign. Owner-confirmed at 240: UI ×1.5, rail thicker, touch still accurate (touch is mapped in the visible
  rect, which density does not change).
- **Audio:** parity with CarPlay, device-verified 2026-08-27. MEDIA 48 kHz/16/stereo, SPEECH and
  SYSTEM 16 kHz/16/mono. Each spoken prompt is its own media session with an incrementing
  `session_id`, so ACKs must follow the current id (`audioSessions[ch]`). The sink table lives once, in
  `AACapability.audioSinks`, read by both the service-discovery declaration and playback — they were
  independent literals, and a mismatch is not something the phone can detect.
  **Guidance/system at 48 kHz (2026-09-04):** the 16 kHz mono speech sinks came from Google's
  reference head unit (and its 2016 integration guide) and are why prompts sounded like a phone
  call. Declared at 48 kHz mono the phone accepted both (`audio config: 0 for channel: TTS`,
  `init, samplingRate: 48000 ... numberOfChannels: 1`) and streams 4096 B / 42.7 ms packets; the
  owner confirmed navigation and Assistant audio noticeably better. Now the default
  (`AACapability.voiceSinkRate`; `AA_VOICE_RATE=16000` restores the reference value). The voice
  lane's queue cap went 150 → 250 ms because each prompt opens with a three-packet burst that the
  old cap clipped by two packets. Calls stay on Bluetooth HFP regardless (see §telephony).
  **Phone-originated sounds (2026-09-04):** the incoming-call ringtone crosses the link over
  Bluetooth — the AG advertises in-band ringing (`+BSIR: 1`) and opens SCO while ringing, so it
  plays through the call lane. A Clock timer rang on the phone speaker only: zero packets on the
  media, guidance and system sinks (alarms are not projected). Notification chimes are designed for
  the SYSTEM sink (channel 6, opened every session, zero packets so far); a shell-posted notification
  has no sound channel and cannot exercise it. **Settled with CallSim's `NOTIFY` action (a
  NotificationCompat MessagingStyle notification with reply/mark-as-read semantic actions and a
  sounding channel):** gearhead accepts a messaging notification only from a package its
  validator allows — a sideloaded app is `CAR.VALIDATOR: Package DENIED; failed all other checks`
  until Android Auto's developer setting *Unknown sources* is on — and it requires the reply action
  to carry `showsUserInterface = false` (the NotificationCompat flag; a platform-built action is
  logged "semantic reply action, but getShowsUserInterface() is true" and dropped) and exactly one
  RemoteInput, plus the `com.google.android.gms.car.application` descriptor with `<uses
  name="notification"/>`. With those in place the chime IS projected — on the **GUIDANCE sink
  (channel 5, stream type 1)**, 1.3 s at 48 kHz, owner-heard on the Mac; the SYSTEM sink (type 2)
  stayed at zero, as it has in every session. There is no dedicated notification stream.
- **Input:** touch, media keys and HOME/BACK all work (device-confirmed 2026-08-27). Keycodes are
  aasdk's `ButtonCode` vocabulary, not plain Android keycodes, though most values coincide:
  Play/Pause `0x55`, Next `0x57`, Previous `0x58`, HOME `0x03`, BACK `0x04`, ENTER `0x17`, and
  `0x54` is **MICROPHONE_1** — the mic button that triggers the Assistant, not "search", which was
  our name for it before the enum was checked. `scrollWheel` (65536) is a button code, not a
  relative event. Three protocol facts established here: `button_event` is **field 4** of
  `InputReport` (layout `timestamp=1, disp_channel=2, touch_event=3, button_event=4`) — the first cut
  used field 2 and every key was silently discarded; `keycodes_supported` is **field 1** of
  `InputSourceService`, with gearhead echoing the declared set back in `KEY_BINDING_REQUEST`; and key
  timestamps are microseconds since epoch, `disp_channel` left unset (gearhead resolves the display
  itself, per its own log).
- **Mic:** serviced on channel 9. The SD response has always had to declare a mic source — a service
  set without one is rejected outright (`CAR.SERVICE Critical error 2/24 "No audio/mic"`) — but for a
  while nothing serviced it, so an Assistant tap opened the channel and waited for audio that never
  came. `onMicStart`/`onMicStop` now drive the app's real capture and `sendMicPCM` puts frames on 9;
  capture authorization is checked *before* answering `MicrophoneRequest`, so a denied mic is declined
  with a status the phone understands rather than answered with silence.
- **Sensors:** night mode is live from the app-pushed vehicle profile, Day/Night device-verified.
- **Driver position (2026-09-04):** `ServiceDiscoveryResponse.driver_position` (field 6) comes from
  the profile's `rightHandDrive` toggle. Device-verified against gearhead 17.5: wire value **2 puts
  the app rail on the LEFT edge, 1 on the RIGHT**, so 2 = LEFT and 1 = RIGHT (`AACapability.
  driverPositionLeft/Right`); the response used to hardcode 1, which is why the rail sat on the right
  for a left-hand-drive profile. aasdk's older reading of field 6 as a `left_hand_drive_vehicle` bool
  (1 = left) is refuted by that observation. Takes effect per session. There is no head-unit field for
  a "dock at the bottom" layout — rail side is the only placement a head unit controls; the phone's
  own Android Auto settings ("Change layout") decide the navigation-card side.
  Driving status is declared **unrestricted** and stays that way by design: it is a claim that the car
  IS moving, not a capability declaration, and mapping CarPlay's `limitedUI` onto it (which the first
  cut did) restricted the phone's UI on a stationary bench. It needs a real signal — vehicle speed or
  parking brake — and the box sources neither yet.
- **Observability (2026-09-03):** a whole AA session used to show as 4 lines in the app's combined
  log despite dozens of `log()` call sites in `AASession.swift` — every one of them went through
  `NSLog`, which carries the *process's* default subsystem rather than `com.carlink.app`, so
  `FileLogger`'s `OSLogStore` poll (which only keeps `com.carlink.*` entries) silently dropped them
  all. `AASession.osLog`/`AASession.defaultLog` (subsystem `com.carlink.app`, category `AA`) is now the
  sink for every AA log line in `AASession.swift`/`AAWire.swift` and the two `AppDelegate.swift` call
  sites that construct a session, so the same messages are captured. New lines cover CH_IP
  transport connect/close-with-reason, service-discovery (channel ids + kinds, one line), each
  channel-open and its negotiated config (video res/fps, audio rate/ch), the first video frame,
  nav-focus/video-focus grants, and the session-end reason (`remote BYEBYE` / `local shutdown` /
  `transport closed`). A throttled ~1 Hz `"AA stats"` line (video rx/decoded/dropped, per-sink audio
  packet counts, mic frames, wire bytes in/out, write backlog) rides the existing `eventLoop` iteration
  rather than a second timer — a live AA session is never quiet, so this lands at effectively 1 Hz
  without a new file logger. Separately, `StreamMetricsMonitor` (`App/StreamMetricsMonitor.swift`) is
  now rebound to the AA decoder in `startAAOverOCBM`/`stopAAOverOCBM` (`App/AppDelegate.swift`) instead
  of staying bound to the parked CarPlay decoder, and its `AVmon` line tags the main-video field
  `aa:v=…` while an AA decoder is bound — before this fix the monitor reported 0 fps for the whole AA
  session because it was still sampling a decoder receiving no frames.

**Wired baseline (2026-09-04, Pixel 10 / Android 17 / gearhead 17.5.663204, one 166 s session,
app 449-test build, box 949112b+):** transport open → VERSION 6.1 in 167 ms → TLS 1.2 (0xc02f) in
68 ms → service discovery → the phone opens ch 3/8/1/4/5/6/9 within 3 ms → CODEC_CONFIG at +850 ms.
Video 1280×720@60: rx 6068 / decoded 6067 / dropped 0 (≈37 fps average of a change-driven stream);
audio media 3192 pkt, guidance 590, system 0, mic 45 (one Assistant prompt); 47.8 MB in (≈2.3 Mbps),
490 KB out, CH_IP backlog ≤ 1, zero decode failures, one session. Only anomaly: the guidance sink
(16 kHz mono) logs `playback underrun — node ran dry between packets` in bursts of up to 8/s while a
prompt plays, each 6–9 ms dry — the AA guidance packets arrive at ~16/s with enough jitter to drain a
node primed with a single packet. Not audible in this run; see §3.

#### Telephony — carried by Bluetooth HFP, not by AA. This is protocol, not our gap.

There are three audio sinks and not four because **Android Auto does not carry phone-call audio over
the projection link at all.** Google's Head Unit Integration Guide is explicit:

> "To provide a consistent hands-free telephony experience across both the native and projection user
> interfaces, AAP uses Bluetooth Hands-Free Profile (HFP) for voice telephony communication."
> … "the HU MUST support Bluetooth HFP"

and, on the stream that looks like it should do the job:

> "While future versions of AAP may use the voice stream for telephony, current implementations use
> Bluetooth for telephony"

The guide further requires the two to interlock: a head unit that supports only one HFP connection
MUST give it to the AA phone, and projection telephony MUST stay disabled on both ends until HFP is
established.

**This is the sharpest asymmetry with CarPlay in the whole project.** CarPlay carries call audio
in-band as an AirPlay audio stream type, which is why our CarPlay path negotiates `audioType`
variants and the AA path has nothing equivalent to negotiate. Apple put telephony inside the session;
Google delegated it to HFP. For AA 1.x that is permanent.

What the protocol nonetheless defines, and why it misleads:

| Artefact | Where | Reality |
|---|---|---|
| `AUDIO_STREAM_TELEPHONY = 4` | `AudioStreamType.proto` | defined; never one of the sinks gearhead negotiates |
| `MEDIA_SINK_TELEPHONY_AUDIO` | aasdk `ChannelId`, with a `TelephonyAudioChannel` | a channel exists in the reference stack |
| `PhoneStatusService` | its own service | call **display and acknowledgement only** — its own source note says "Hands-free profile (HFP) handles call audio routing separately" |
| `ButtonCode` `phone 0x05` / `callEnd 0x06` | declared by us | call *control*, not audio |

Reading the phone side agrees: gearhead 17.5.663204 knows the name — its stream-type-to-string helper
returns `"AUDIO_STREAM_TELEPHONY"` for 4 — but nothing in the decompiled tree shows a telephony sink
being opened. Knowing the enum is necessary, not sufficient.

The stock Carlinkit firmware did exactly what the guide requires and nothing cleverer. From the
2026-03-15 adapter capture:

```
[RiddleBluetoothService] start hfpd in AndroidAuto or CarLife mode
hfpd add device: <phone BD_ADDR>
>> +CIND: ("call",(0,1)),("callsetup",(0-3)),("service",(0-1)),…
268 root  5576 S  hfpd -y -E -f
```

`hfpd` is the stock nohands HFP daemon, started specifically for AA mode. The `callQuality` field in
its `CMD_BOX_INFO` is an HFP wideband/narrowband SCO codec knob, not an AA setting. So Carlinkit was
not restricting anything — a head unit that tried to carry call audio over the AA link would simply
get no call audio.

**Consequence for us:** call audio arrives on an SCO link the *box* terminates, not on OCBM. Routing
it to the host app is therefore a separate transport decision, closer in shape to `CH_MIC` than to
the AA sinks. `hfpd` (126 KB) and `bluetoothDaemon` (173 KB) are already on the box, unused by our
stack — we terminate SCO ourselves rather than adopt either.

#### Telephony and the Assistant — IMPLEMENTED 2026-09-03 (was "out of scope")

This section used to end "**explicitly out of scope for the wireless milestone**". It is no longer.
Two findings forced it, and the second is the one that made it urgent:

1. Call audio is on SCO, as above.
2. **The Assistant is NOT — REFUTED on hardware 2026-09-04.** This item used to claim that gearhead
   routes the Assistant over the headset link too (`BluetoothHeadset.startVoiceRecognition` →
   `setCommunicationDevice` → `startBluetoothSco()`, `kxr.java:118-150`) and that the `mic=0` seen
   on 2026-09-03 was the phone waiting for `+BVRA` and SCO. Measured with the SCO path live and the
   phone on the internet: an Assistant press over wireless AA opened the **AA microphone channel**
   (`<- MIC OPEN — capturing 16 kHz mono`, 174–692 frames per query) and answered on the **AA
   guidance channel** (257 frames); the box logged no `+BVRA` and no SCO connection at all. The
   `kxr.java` path exists in the binary but this phone does not take it for a projected Assistant
   session; the 2026-09-03 `mic=0` had another cause (the phone had no internet on that bench, so
   the Assistant never got as far as a microphone request). SCO is a telephony path, full stop.

**What the box does now** (`crates/vendor/wireless/src/sco_audio.rs`):

* A `BTPROTO_SCO` **listening** socket exists for the life of each headset link. The kernel only
  accepts an incoming (e)SCO Connection Request when something is listening (`sco_connect_ind`), so
  this socket is what makes the phone's `startBluetoothSco()` succeed at the HCI level.
* **Downlink** (phone → app): read CVSD, aggregate to 20 ms / 320 B frames, and write them to
  ocbmd's existing voice-sink seam `127.0.0.1:9003` → `CH_ALT_AUDIO`, framed with the new
  `SEAM_PKT_PLAIN 0x03` marker after one `SEAM_FORMAT` of *PCM / 8000 Hz / 1 ch / 16-bit /
  audio_type 1 (telephony)*. scid is fixed at `0x4846_5053_434F_0001` (`HFPSCO\x00\x01`). No new
  channel and no new lane: this is the voice sink CarPlay already uses.
  See [`../carplay/01_OCBM_PROTOCOL.md`](../carplay/01_OCBM_PROTOCOL.md) for the marker.
* **Uplink** (app → phone): the existing `CH_MIC` relay, unchanged. ocbmd already connects to
  airplayd's mic seam `127.0.0.1:9112` whenever a host is subscribed; during an AA session airplayd
  is not running, so `carplay-wireless` LISTENS there itself, speaks the identical protocol
  (`mic <len>\n<pcm>` in, `uplink on 8000 1` / `uplink off` out), and feeds what arrives straight
  into the SCO socket. One write per read, so the controller's own SCO clock paces the uplink.
* **CVSD narrowband by default; mSBC wideband behind a lever — IMPLEMENTED 2026-09-04, UNTESTED ON
  HARDWARE.** Default is unchanged and byte-identical: `AT+BRSF=63` leaves HF bit 7 (codec
  negotiation — *not* bit 5, as this bullet used to say) clear, so the AG never sends `+BCS` and
  always opens CVSD / `Voice: 0x0060`. With `CARPLAY_HFP_WBS=1`, `/tmp/hfp_wbs` or `/script/hfp_wbs`
  present, the SLC sends `AT+BRSF=191` and — only if the AG's `+BRSF` has bit 9 (this Pixel answers
  879) — `AT+BAC=1,2` between `AT+BRSF` and `AT+CIND=?`, where HFP 1.6 §4.2 requires it. The AG then
  drives everything with `+BCS: <id>`: we set the SCO listener's `BT_VOICE` to transparent (`0x0003`)
  or CVSD (`0x0060`) BEFORE replying `AT+BCS=<id>`, because the AG opens (e)SCO within milliseconds
  of that reply and the accepted socket inherits the listener's air mode. An id we never offered is
  answered `AT+BAC=1,2`; a `setsockopt` failure is answered `AT+BAC=1` (CVSD only) — the AG is never
  left expecting transparent audio the box cannot pass. Under mSBC the downlink stops being PCM: each
  SCO read is forwarded VERBATIM as one `SEAM_PKT_PLAIN` under a `SEAM_FORMAT` of codec 4 / 16000 Hz,
  no 320 B aggregation, and the app decodes (the box has no mSBC codec and grows none). The uplink
  inverts it — `uplink on 16000 1 msbc`, and the app returns whole 60 B eSCO packets that go to the
  socket unmodified; an underrun SKIPS the write, because there is no silent mSBC frame to synthesise.
  **Where `BT_VOICE` may be set is a device finding, 2026-09-04 11:46Z.** The first implementation
  set it on the LISTENER whenever a `+BCS` arrived, and the box logged
  `BT_VOICE setsockopt(0x0060) failed: Invalid argument (os error 22)` — kernel 3.14's
  `sco_sock_setsockopt` accepts the option only in `BT_OPEN`, `BT_BOUND` or `BT_CONNECT2`, and a
  listening socket is `BT_LISTEN`, so it fails even for the CVSD value the socket already had. The
  BlueZ/oFono shape, which is what ships: set it once on the listener **between `bind` and `listen`**
  (`BT_BOUND`) as the default a child inherits, and set the real per-connection value on the
  **accepted child** while it sits in `BT_CONNECT2`. `set_codec` therefore touches no socket at all —
  it records the codec, and the accept path keeps that promise one connection later.
  **`BT_DEFER_SETUP` is what creates the `BT_CONNECT2` window**, and the listener carries it for both
  codecs: without it the kernel answers the incoming request from `hci_conn_request_evt` using the
  HCI global `hdev->voice_setting` and never reads the socket. With it the accept is deferred to
  `sco_conn_defer_accept(hcon, sco_pi(sk)->setting)`. The sequence after `accept()` is BlueZ's own:
  set the child's air mode, ONE read triggers the real `Accept_Synchronous_Connection_Request` and
  returns 0 (that zero is success, not an EOF), and `POLLOUT` is the kernel saying the link is up —
  nothing writes before that first read. A failed child `setsockopt` closes the child and latches
  wideband off for the link (`AT+BAC=1` on the next `+BCS`); a failed `BT_DEFER_SETUP` falls back to
  the non-deferred accept and refuses wideband the same way, so CVSD keeps working either way. The
  vendor's `scomtu 240:32` is 240 bytes over 32 packets and carries a 60 B uplink write with room to
  spare. **Proven on device 2026-09-04:** the Pixel picks mSBC (`+BCS: 2`) and the CVSD fallback
  (`AT+BAC=1` → `+BCS: 1` → call OK) works. **Still open:** a transparent channel carrying
  intelligible audio both ways.
  **App side landed the same day** (`host/CarPlayHost/carlink_macOS/Audio/MSBCCodec.swift` +
  `MSBCFramer.swift`): a from-the-spec mSBC encoder/decoder — macOS has no SBC codec — plus the H2
  resync, cross-read reassembly and sequence-driven concealment, wired into the telephony lane and the
  `CT_UPLINK` codec byte. Harness-tested (round trip, CRC, framer, a decode check against an
  independent fixed-point reference); see `docs/host/00_MACOS_HOST_APP.md`.
  **Open on hardware:** the whole path — `+BCS: 2`, a transparent channel, intelligible audio both
  ways — has not been run on a unit.
* **Call state** is read from the AG's own indicators, not from `RING`: `hfp_hf::CallTracker` maps
  `+CIEV: <n>,<v>` through the `AT+CIND=?` names (`call`, `callsetup`, `callheld`) and `+BVRA`, and
  logs named transitions. `RING` alone cannot distinguish a missed call from an answered one.
* **We never answer.** Answer/hang-up stay on the phone and the AA screen. The single exception is
  the bench lever `CARPLAY_HFP_AUTOANSWER=1` (or `/tmp/hfp_autoanswer`), which sends `ATA` on the
  ringing edge so one person with one phone can exercise the audio path.
* The controller's SCO setup, which `bt_bringup`'s DOWN→UP cycle resets away, is restored through
  the radio seam's new `sco_on` verb — see
  [`../wireless/01_BT_AND_RADIO.md`](../wireless/01_BT_AND_RADIO.md).

**DEVICE RESULT 2026-09-04 — both directions, both call directions, first deploy.** Pixel 10 /
gearhead 17.5, wireless AA session live, calls simulated by `host/CallSim` (a self-managed
ConnectionService with a tone pattern as far-end audio and a WAV recorder on the phone's uplink):

* Incoming: `+CIEV: 2,1` → *incoming call ringing* → `+CLIP` caller id → the phone opened SCO
  during ringing for its in-band ringtone (`+BSIR: 1`, 635 frames), closed it at answer, and
  reopened it for the call (`+CIEV: 1,1` → *call active*). Outgoing: `+CIEV: 2,2` → `2,3` →
  `1,1`, SCO up 0.6 s after dialing. Both hangups: `+CIEV: 1,0` → *call ended*, SCO closed.
* Downlink: 50 pkt/s of 320 B plain PCM reached the app's telephony lane and was audible on the Mac
  (owner-confirmed on both calls). The 20-minute first call ran 61,106 SCO frames with no gap.
* Uplink: the box asked for `uplink on 8000 1`, the app captured at 8 kHz and sent 50 frames/s;
  the phone-side recording contains the owner's speech exactly where they spoke (peak 16 % FS on
  the second call, digital silence between utterances) — the audio reached the phone over SCO.
* Two defects found and fixed the same day: the app's arrival-paced telephony playback ran dry
  once every ~3 s (now a 60 ms pre-roll, `AudioPlayer.telephonyPrerollSeconds`), and ocbmd dropped
  the mic seam on a write failure before draining the peer's `uplink off`, leaving the app's mic
  hot for 45 min after the hangup (ocbmd now synthesizes the OFF on any seam loss while ON).
* `+BVRA` handling is in place but has never fired — see item 2 above.


**DEVICE RESULT 2026-09-04 (wideband).** With `/script/hfp_wbs` set and the child-socket BT_VOICE
fix deployed: the phone answered our `AT+BAC=1,2` with `+BCS: 2` on the next headset link (the
first link after boot stayed CVSD — the AG only negotiates when it decides to, so the first call
after a fresh SLC can still be narrowband), the box replied `AT+BCS=2`, applied `transparent
(mSBC)` to the deferred-accept child, and logged `SCO connected — mSBC wideband, transparent eSCO
packets of 60 B`. The app decoded 134 frames/s (7.5 ms cadence) with `plc=1` every few seconds,
the mic uplink armed at `16000 Hz mSBC (60 B/7.5 ms eSCO packets)` and the box accepted the 60-byte
writes (no EMSGSIZE). A 77 s call ran 10,222 mSBC frames. Before the fix the same negotiation
ended in `BT_VOICE setsockopt failed: Invalid argument` on the listener and the box narrowed to
CVSD with `AT+BAC=1` — the fallback path is therefore device-proven too.

#### Metadata services — IMPLEMENTED 2026-09-04, device-proven the same day (Media and Navigation panes owner-confirmed against the phone)

The head unit declares three more `gal.*` services in `ServiceDiscoveryResponse` (lever
`AA_METADATA`, default on; `=0` withholds them): **MediaPlaybackStatusService** on channel 10
(`ChannelDescriptor` field 9, empty config), **NavigationStatusService** on channel 11 (field 8,
`{minimum_interval_ms 1000, type IMAGE, image_options 128×128×32}`) and **PhoneStatusService** on
channel 12 (field 10, empty). The Pixel 10 / gearhead 17.5 accepted the set and opened all three
channels 140 ms after discovery. What it then sent, from the wire:

| Channel | id | Message | Cadence | Notes |
|---|---|---|---|---|
| 10 | 32769 | `MediaPlaybackStatus` {1 state (1 stopped/2 playing/3 paused), 2 source app, 3 position s, 4/5/6 shuffle/repeat/repeat-one} | 1 Hz while playing | layout confirmed from gearhead's own sender (`jav`/`xkm`) |
| 10 | 32771 | `MediaPlaybackMetadata` {1 song, 2 artist, 3 album, 4 album art PNG, 5 playlist, 6 duration s, 7 rating} | per track | 92–139 KB art observed (`jav`/`xkl`) |
| 11 | 32771 | `NavigationStatus` {1 enum: 1 active} | on change | |
| 11 | 32774 | `NavigationState` {1 steps[ {1 maneuver {1 type, 2 roundabout exit, 3 angle}, 2 road {1 name}, 3 lanes, 4 cue {1 text}} ], 2 destinations} | ~0.5 Hz | the maneuver is a **type enum** (gal `NavigationType` 0–42, e.g. 22 = exit slight right); **no image is sent** even with IMAGE options declared |
| 11 | 32775 | `NavigationCurrentPosition` {1 step {1 distance {1 m, 2 display, 3 unit}, 2 seconds}, 2 destination {1 distance, 2 ETA text, 3 seconds}, 3 current road} | ~0.5 Hz | "20 mi in 1141 s; dest 173 mi ETA 8:25 AM" |
| 12 | 32769 | `PhoneStatus` {1 calls[ {1 state, 2 duration, 3 number, 4 caller id, 5 type, 6 thumbnail} ], 2 signal} | on change | device-proven with a Phony fake call: `4:Guy man` (INCOMING) ~1 s after the HFP `RING`/`+CLIP`, `1:Guy man` (IN_CALL) on answer, `calls=0` on hangup; the app builds Call History locally from these transitions (AA has no recents list). Owner-confirmed: Phone pane correct, audio "8 kHz good" (Phony playing the Harvard sentences as the far end — the reference for the wideband comparison in T3) |

The deprecated `NavigationNextTurnEvent` (32772, the one that carries a rendered turn image) and
`NavigationNextTurnDistanceEvent` (32773) were **never sent** by this phone at protocol 1.7; the
decoders exist for older phones. So the answer to "does AA ship the maneuver icon as an image":
only in the deprecated scheme; current gearhead sends the type and the head unit draws the glyph —
same division of labour as CarPlay. The app maps the type to an SF Symbol in the Metadata
window's Next Maneuver card and shows the phone's ETA string.

Sourcing, per the owner's rule: gearhead's decompile gave the message ids and the media layouts
(protobuf-lite `newMessageInfo` strings decoded by hand: `xkl` = 1 str, 2 str, 3 str, 4 bytes,
5 str, 6 u32, 7 i32; `xkm` = 1 enum, 2 str, 3 u32, 4–6 bool); aasdk's protos gave the navigation and
phone layouts and openauto the subscribe recipe; the DHU binary confirmed the service names; the
wire (this session) settled which navigation scheme is live. Code: `AA/AAMetadata.swift`,
`AAWire.serviceDiscoveryResponseFull(metadataServices:)`, `MetadataStore.applyAndroidAuto`.

#### Wired AA black screen — audio but no video (FIXED 2026-09-04)

Wired Android Auto played audio with a black screen while the decoder reported success
(`decoded` climbing, IDRs arriving). Cause: the box's presence check (`phone_on_bus`,
`tools/session_supervisor.sh`) watches for the iPhone USB id (05ac) and the phone-facing gadget
`android0`; a wired Android phone is a USB *device* toward the box, so neither matches and the box
emits `SEV_PHONE_ABSENT` ~1.3 s after it announces `pmWiredAa` (the phone re-enumerates through the
AOAP switch). The macOS `OCBMSessionCoordinator` turned that into `onStreaming(false)` →
`CarPlayView.setStreaming(false)` → `videoLayer.isHidden = true` — and because the AA decoder's
`AVSampleBufferDisplayLayer` had been swapped into `videoLayer`, the *AA* layer was hidden. Frames
kept decoding into a hidden layer; audio (not routed through the view) was unaffected. Wireless AA
never emits ABSENT (the box stays "present" via `wireless_owns_session`), which is why the same
2400×960 / tier-4-HEVC / margins / 240-dpi geometry rendered fine wirelessly.

Fix (`App/AppDelegate.swift`, `App/CarPlayView.swift`): the coordinator's `onStreaming`/`onStatus`
callbacks are no-ops while an AA session owns the view (`aaSession != nil || parkedCarPlayDecoder
!= nil`), `applyAppearance` no longer hides the layer during AA, and `startAAOverOCBM` asserts the
layer visible. Owner-confirmed A/V after the fix. **Box-side follow-up (open item):**
`phone_on_bus` should also return present for a wired Android (or an AA owner) so the ABSENT event
never fires during wired AA in the first place.

### 2. Resolved defects, with causes worth keeping

**Session death at 57–94 s** (`errSSLDecryptionFail -9845`, then a busy loop on `errSSLClosedAbort`).
Ciphertext was fed to TLS out of order: there is ONE TLS stream shared by every channel, but `recvMsg`
reassembled a fragmented message's ciphertext **per channel** before decrypting — aasdk's shape, which
`host/aa-headunit` copied. That withholds a FIRST fragment's bytes while feeding the next frame's, and
the phone interleaves channels mid-message. It only bit when a >16 KB message fragmented and another
channel interleaved before the LAST, which is why it read as a random timer. **That the phone
interleaves at all proves encryption is per FRAME, not per message.** Fix: decrypt each frame as it
lands and accumulate PLAINTEXT per channel. Result: 309 s / 3510 frames / zero decrypt failures,
against a previous best of 94 s.

**Video stall at ~2 minutes over the box relay** (audio and ping/pong kept flowing). Cause:
`AAWire.mediaConfigReady()` advertised `max_unacked=1`, making video stop-and-wait — the phone will
not send frame N+1 until it sees the ACK for N. Over the fire-and-forget box relay one delayed ACK
wedged the channel; low-latency adb-TCP never tripped it. Fix: `max_unacked` 1 → 64, and send the
`mediaAck` on receipt before decode work. Window scaling confirmed the mechanism (1 → stall at 3690
frames, 8 → 4140, 64 → no stall).

**Mid-session pixelation.** The phone's keyframe cadence follows **the protocol version the head unit
REQUESTS**: `key_frame_interval_wireless = 60` s below 6.0, `key_frame_interval_ackless = 2` s at 6.0+.
At 1.7 the phone emits an IDR once a minute, and the protocol has no keyframe request at all, so one
shed P-frame visibly persists. With `AA_PROTO=6.1` IDRs land every ~60 frames instead of at frame
#1801. Not a codec problem — switching to H.265 would not have fixed it.

**Refuted:** the "we never ACKed audio" root cause for recurring teardowns. Disproved by experiment in
the same session — AA flow control is per-channel, and audio is the one channel we do not ACK, yet it
runs indefinitely. Audio ACK conformance remains worth doing, but it was not the cause.

### 3. Open items

- **`CT_SETTIME` ack unhandled in the app** — `unhandled frame ch=0x0 op=0x5 len=10` once per SUBSCRIBE;
  parse it in `OCBMClient` (status byte) and log clock-set success/failure. Cosmetic.
- **Guidance-sink micro-underruns** — prime the 16 kHz sinks with ~40–60 ms before starting playback
  (baseline above: 6–9 ms dry gaps at up to 8/s during prompts). App-side, `Audio/AudioPlayer.swift`.
- **Audio ACK conformance** — per-channel session ids for the audio sinks.
- **Driving status needs a real signal** — vehicle speed or parking brake; the box sources neither, so
  the sensor channel declares unrestricted (see §6).
- **D-Pad focus** — all four directions are delivered exactly (sent 18/16/16/14, gearhead received
  keycode 19/21/20/22 with the same counts), but gearhead's own focus ring moves inconsistently,
  jumps to the sidebar and sometimes disappears. Not a defect on our side. `AA_NO_TOUCH=1` (declare
  keycodes, no `touch_screen_config`, DHU's `rotary.ini` shape) was tested on the theory that a head
  unit with a touchscreen gets focus navigation second-class: no improvement, so the lever stays
  env-gated and OFF alongside `AA_SKIP_AUDIO_ACK` / `AA_EDGE_CLAMP_BUG` / `AA_LEGACY_VIDEO`.
- **`AA/AATLS.swift` still logs via `NSLog`** (cipher / peer-issuer lines), which `FileLogger` filters out
  by subsystem; route it through `AASession.osLog` like the rest (2026-09-04).
- **Wireless AA** — end-to-end on device 2026-09-04 (HFP hands-free link → phone-initiated bootstrap → AP →
  pump); see [`03_WIRELESS.md`](03_WIRELESS.md) §1 for the measured run and the two open nits.
- **Transport backpressure** for the AA path.
- **Footprint** — the box side adds an AOAP switch plus a byte pump; confirm against the rootfs budget
  (`../ops/00_BUILD_AND_DEPLOY.md`).

