# Metadata, controls and the iAP2 data channel

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 47, 45, 20, 32, 34, 33, 35; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

**Contents:** the metadata surface we declare → the DataStream/RCS carrier → SDK research → the GM real-world references → the code audit that closed the loop.

## The metadata surface (declaration rules)

<!-- absorbed: ../carplay/05_METADATA_AND_CONTROLS.md -->

Status: authoritative for the iAP2 metadata plane.

§1–4 enumerate the surface and measure what was arriving before this change. §5 is the implementation.
§6 is the device record: four sessions, ending with the Identify accepted and every declared feed
arriving over the RCS tunnel.

Outcome: the declaration and subscribe problem is closed. One defect remains, and it is on the host
side — three panes read empty although the records reach the app (§6.6).

> **⚠️ "One defect remains" was true on 2026-07-25 and FALSE for the ten days after.** A second,
> unrelated defect broke the wireless TRANSPORT under this surface from 2026-07-31 (docs/carplay/05_METADATA_AND_CONTROLS.md §8), so on
> wireless the records did not reach the app at all. The declaration/subscribe conclusions here stand
> and the host-side pane defect is still open — but verify the transport is alive before debugging
> anything in §6.6 on a wireless session. Full reasoning: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-47-1`.


Sources: `spec::IAP2_MESSAGES` (144 ids, direction-typed, asserted by
`declared_message_lists_are_direction_correct`), CINEMO CT5 `libNmeIAP.so`, Apple's R14G17
`AirPlayCommon.h`, and the wired/wireless captures in `docs/ops/captures/`.

---

### 1. Two independent families

Metadata reaches the accessory over two unrelated channels. They are often conflated.

| | iAP2 messages | AirPlay `/command` plists |
|---|---|---|
| Carrier | iAP2 link — wired `/dev/android_iap2`, wireless the RCS DataStream (docs/carplay/05_METADATA_AND_CONTROLS.md) | AirPlay control connection |
| Content | Track, navigation, telephony, power, library, HID | Session lifecycle, UI mode, audio ducking, night mode |
| Gating | Must be declared in Identify params 6/7, then subscribed | None; the phone sends what it sends |
| Our handler | `iap2-core::metadata` → `emit_json` | `receiver::session::command` |

Only the iAP2 family is discussed below. The `/command` family is fully enumerated in R14G17
`AirPlayCommon.h` (28 commands, `kAirPlayCommand_*`); the host app already categorises all of them.

---

### 2. What the phone can send us

Device-sourced iAP2 messages, from `spec::IAP2_MESSAGES`. Grouped by what they are for; the subscribe
that turns each on is in brackets. Every id below is `Source::Device` — it belongs in Identify param 7,
and its bracketed subscribe (`Source::Accessory`) in param 6. **Feeds that LOOK like this but run the
other way are in §2.1 — check there before adding anything to param 7.** Apple's
`Start*`/`*Update`/`Stop*` naming does NOT imply direction; read the `Source` column in `spec.rs`.

#### Media

| Id | Message | Subscribe |
|---|---|---|
| 0x5001 | NowPlayingUpdate — title, artist, album, genre, composer, app name, duration, elapsed, track n/N, queue index/count, shuffle, repeat, playback status, artwork id | 0x5000 |
| — | Album artwork — iAP2 File Transfer on link session 2, not a message id | implicit in 0x5000 |
| 0x4C01 | MediaLibraryInformation — the library's identity and revision | 0x4C00 |
| 0x4C04 | MediaLibraryUpdate — playlists, artists, albums, tracks as a syncable database | 0x4C03 |

#### Navigation

| Id | Message | Subscribe |
|---|---|---|
| 0x5201 | RouteGuidanceUpdate — destination, current road, time/distance remaining, current maneuver index, guidance state | 0x5200 |
| 0x5202 | RouteGuidanceManeuverInformation — one maneuver: description, road after, distance, exit info | 0x5200 |
| 0x5204 | LaneGuidanceInformation | 0x5200 |
| 0x10DB | DestinationInformation | 0x10DA |

#### Telephony

| Id | Message | Subscribe |
|---|---|---|
| 0x4155 | CallStateUpdate — **per call**: remote id, display name, status, direction, call UUID, address-book id, label, service, conferenced, disconnect reason, start timestamp | 0x4154 |
| 0x4158 | CommunicationsUpdate — **radio and capability state**: signal strength, registration status, carrier name, cellular supported, telephony enabled, current call count, plus the availability flags that drive dialer button enablement | 0x4157 |
| 0x4171 | ListUpdate — **call history**: the Recents list and Favorites, per-entry | 0x4170 |

`CallStateUpdate` is per-call and only fires when a call exists. `CommunicationsUpdate` is the standing
telephony status. `ListUpdate` is the only source of call history — there is no separate history
message.

#### Device and session

0x4E09 DeviceInformationUpdate · 0x4E0A DeviceLanguageUpdate · 0x4E0B DeviceTimeUpdate ·
0x4E0C DeviceUUIDUpdate · 0x4E0D WirelessCarPlayUpdate · 0x4E0E DeviceTransportIdentifierNotification ·
0x4E04 BluetoothConnectionUpdate · 0x4300 CarPlayAvailability · 0x4303 MatchedDigitalCarKeys

#### Power, apps, accessibility, other

0xAE01 PowerUpdate [0xAE00] · 0xAD01 AppDiscoveryUpdate [0xAD00] · 0xAD04 AppDiscoveryAppIcon
[**0xAD03 RequestAppDiscoveryAppIcons** — a request/response, NOT the 0xAD00 subscribe; conflating the
two is what produced the `OptionalMsgNotValidWithoutRequiredMsgs` shape fixed 2026-07-30, §5.3] ·
0x5403 AssistiveTouchInformation [0x5402] · 0x560C VoiceOverUpdate and 0x5610 VoiceOverCursorUpdate
[0x560B / 0x560F] · 0x5701 WiFiInformation [0x5700 RequestWiFiInformation — a one-shot request, not a
Start/Stop pair] · 0x6801 DeviceHIDReport [0x6800] · 0x6807 HIDComponentUpdate [**no accessory
subscribe — §2.1**] · 0xB101 DeviceBluetoothLowEnergyUpdate [**no accessory subscribe — §2.1**] ·
0xDA01 USBDeviceModeAudioInformation [0xDA00]

CINEMO's CT5 head unit implements handlers for essentially all of these
(`On{NowPlaying,RouteGuidance,RouteGuidanceManeuver,CallState,Communications,List,MediaLibrary,
AppDiscovery,Power,BluetoothConnection,DeviceInformation,DeviceLanguage,DeviceTime,VoiceOver,
VoiceOverCursor,HIDComponent,WirelessCarPlay}Update` in `libNmeIAP.so`). It is a fair statement of the
achievable surface for a shipping accessory.

#### 2.1 Accessory-sourced — these would be ours to SEND, not receive

Four feeds read like device metadata and run the other way. Apple's `source` field puts the PAYLOAD on
the accessory and both halves of the Start/Stop on the device: **the phone subscribes to US.** So the
payload id belongs in param 6 and its Start/Stop in param 7 — the exact opposite of §2. Params 6/7 are
un-strippable, so getting this backwards costs an unrecoverable `0x1D03` (§5.6).

| Payload — param 6, ours to send | Its Start/Stop — param 7, phone sends | Declared today |
|---|---|---|
| 0x0D01 RoadObjectDetectionUpdate, Accessory (`spec.rs:580`) | 0x0D00 / 0x0D02, both Device (`spec.rs:579,581`) | no — would also need ident param 33 RoadObjectDetectionComponent |
| 0xFFFB LocationInformation, Accessory (`spec.rs:712`) | 0xFFFA / 0xFFFC, both Device (`spec.rs:711,713`) | no — would also need ident param 22 LocationInformationComponent. **NOT implemented as a sender** — `crates/vendor/metadata/src/location.rs` has parsers only (`parse_location_information`, `parse_start_location_information`, `merge`); no 0xFFFB builder exists anywhere, and Identify param 22's `build_location_information_component` has zero callers. *(Corrected 2026-08-16 — an earlier version of this row claimed a sender.)* |
| 0xA101 VehicleStatusUpdate, Accessory (`spec.rs:676`) | 0xA100 / 0xA102, both Device (`spec.rs:675,677`) | no — see the ⚠️ at `message.rs:908` before touching ident param 21 |
| 0xB102 AccessoryBluetoothLowEnergyUpdate, Accessory (`spec.rs:697`) | 0xB100 / 0xB107, both Device (`spec.rs:695,702`) | no |

Two ids listed in §2 are genuinely Device-sourced but have **no accessory-sendable subscribe at all**,
because both halves of their trigger pair are Device-sourced. There is nothing to put in param 6 for
them; the phone starts them or they do not start.

* **0xB101 DeviceBluetoothLowEnergyUpdate** (`spec.rs:696`) — 0xB100 Start and 0xB107 Stop are both
  `Source::Device`. Declaring 0xB100 in param 6 is a direction error. Our half of this feature is
  0xB102, above.
* **0x6807 HIDComponentUpdate** (`spec.rs:671`) — not gated by 0x6800 StartHID, which is Accessory
  (`spec.rs:666`) and gates 0x6801 DeviceHIDReport only. 0x6807's counterpart 0x6806 StartNativeHID is
  `Source::Device` (`spec.rs:670`).

⚠️ **`features::Feature` cannot express the inverted shape.** `sent_ids` is the trigger's start/stop and
`received_ids` is `updates` (`features.rs:649-660`), so a naive
`Feature { start: 0xFFFA, stop: 0xFFFC, updates: &[0xFFFB] }` declares BOTH backwards. The plan at
`docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §Mechanism B step (3) contains exactly that error for both 0xFFFA/0xFFFB and 0xA100/0xA101 —
fix it there before implementing.

Direction is machine-checked for what we actually declare: `declared_message_lists_are_direction_correct`
(`message.rs:1139`) asserts every param-6 id is `Source::Accessory` and every param-7 id is
`Source::Device`, and `declared_directions_match_apples_catalog` (`features.rs:677`) does the same for
the generated table. **THIS DOCUMENT IS NOT COVERED BY EITHER TEST**, which is why five entries in §2
were wrong until 2026-08-16. Re-derive from `spec.rs` — which is generated verbatim from Apple's
`i2mspecarchive` and marked do-not-hand-edit — never from this prose.

---

### 3. What we asked for before this change

Both transports — `iap2d` wired and the RCS tunnel — sent exactly three subscribes:

```
0x5000  StartNowPlayingUpdates
0x5200  StartRouteGuidanceUpdates
0x4154  StartCallStateUpdates
```

The Identify declaration bounds what those subscribes can return, and it did not match them:

| | Wired `RCV_MSG_IDS` | Wireless tunnel |
|---|---|---|
| 0x5001 NowPlayingUpdate | yes | yes |
| 0x5201 RouteGuidanceUpdate | yes | yes |
| 0x5202 ManeuverInformation | yes | yes |
| 0x4155 CallStateUpdate | yes | yes |
| 0x4E0A / 0x4E0B / 0x4E0E | yes | no |
| **0x4158 CommunicationsUpdate** | **no** | **no** |
| **0x4171 ListUpdate** | **no** | **no** |

---

### 4. What arrived under that declaration, measured

Box log `/tmp/airplayd_wl.log`, wireless session of 2026-07-25, before §5:

| Message | Count | Where it surfaces |
|---|---|---|
| 0x5001 NowPlayingUpdate | 692 | Media |
| 0x5201 / 0x5202 RouteGuidance | present | Navigation |
| session-2 file transfer | 2 | album artwork |
| **0x4155 CallStateUpdate** | **3** (110, 122, 127 B bodies) | Phone, and Call History on hang-up |
| 0x4158 CommunicationsUpdate | 0 | Telephony — empty |
| 0x4171 ListUpdate | 0 | — |

So four categories arrive, not two. Call state works over the RCS tunnel.

> **⚠️ RE-MEASURED 2026-08-10 — the table above is the OLD floor.** At tier `extended`, same hardware:
> Identify 342 B, `0x1D02` accepted, and `0x4158 CommunicationsUpdate` = 2 where this document
> measured 0. The compiled default stays `proven` as the recovery baseline — a doctrine question
> (docs/carplay/04_CAPABILITIES_AND_CONFIG.md), not a measurement one. Full reasoning: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-47-2`.


> **⚠️ TIER `all` IS REFUTED on the AirPlayTunnel arm (device evidence, 2026-08-10).** iOS named three
> `voice_over_cursor` ids in a decoded `0x1D03`; skipping them removed the ids but iOS rejected anyway
> with a GENERIC param-6 objection, so iterative skipping is a dead end. This says nothing against
> `extended`, which is accepted on this arm. Full reasoning: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-47-3`.


> **⚠️ "ARRIVE" / "WORKS" BELOW DESCRIBE THE 2026-07-25 SESSION ONLY.** The RCS tunnel was dead
> 2026-07-31 → 08-10 (docs/carplay/05_METADATA_AND_CONTROLS.md §8), so nothing arrived over it in that window. The declaration and
> subscribe findings are unaffected — the failure was one layer below. Wired was unaffected
> throughout. Full reasoning: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-47-4`.


**Call History is locally derived, not received.** `MetadataWindow.swift` moves `activeCall` into
`callHistory` when a 0x4155 reports the call disconnected. Those entries are calls this session
observed. The iPhone's own Recents and Favorites lists — 0x4171 ListUpdate, the `recentCall` and
`favorite` JSON kinds — have never arrived; across 131 host-app logs there is not one `recentCall`.
The Call History tab is real but shows only what happened while connected.

**Telephony is the genuinely empty tab.** It is fed exclusively by 0x4158 CommunicationsUpdate:
carrier, signal strength, registration status, active call count, voicemail count, airplane mode, mute,
telephony-enabled.

Two distinct reasons for the two gaps:

1. **0x4158 and 0x4171 are undeclared.** Neither appears in Identify param 7 on either transport, so no
   subscribe can produce them. A subscribe for an undeclared id is silently ignored — the 2026-07-24
   wired capture shows `TX 0x4157 StartCommunicationsUpdates` and `TX 0x4170 StartListUpdates` going out
   and nothing coming back. Both subscribes were removed on 2026-07-25 in lockstep with the Identify
   trim; their field lists now live in `features.rs` as `COMMS_FIELDS` and `LIST_GROUPS`.
2. **Everything else was never asked for.** Power, app discovery, media library, VoiceOver, Bluetooth
   connection and the device-info updates are neither declared nor subscribed.

The parsing and display sides are not the constraint. `iap2-core::metadata` has working parsers for
`communications` (0x4158) and `list_update` (0x4171); a single `metadata::dispatch` call dispatches them;
`MetadataWindow.swift` handles the `communications`, `recentCall` and `favorite` JSON kinds. The
pipeline is complete from the wire inwards. Nothing arrives to feed it.

Note on measuring this from the host app's log: it records only records #1–3 and every 50th
(`if jsonCount <= 3 || jsonCount % 50 == 0`). Kind counts taken from it are a sample, not a census. Use the box log.

---

### 5. Implementation

#### 5.1 One table

`iap2-core/src/features.rs` is the box-side table that Identify params 6 and 7 and the subscribe
sequence are all GENERATED from, on both transports — never hand-edit one of the three independently
(the generation invariant, §5.6). Each entry names the `Start*` id we send, the `*Update` ids we
expect, and the field selectors for the subscribe body. Per docs/carplay/04_CAPABILITIES_AND_CONFIG.md, the app's pushed config is the
single source of truth for WHICH tier/content is selected; the compiled table and the levers of §5.2
are the interim, box-side mechanism pending migration to app-pushed config.

They used to be three hand-maintained lists in three files, and they drifted: 0x4157 and 0x4170 were
being subscribed while 0x4158 and 0x4171 were absent from param 7, so iOS ignored both for the
project's entire history. `message.rs::ident_info_wired_message_lists_are_byte_pinned` asserts that every expected update is
declared receivable. It deliberately does NOT assert the sendable half, because the refuted `rx-only`
mode breaks that direction on purpose — so this is a test assertion covering one direction, not a
compile-time guarantee covering both.

Field ids come from Apple's `iap2messages-internal.i2mspecarchive` via `tools/i2mspec_dump.py`, which
decodes the NSKeyedArchiver graph the Simulator ships. `tools/i2mspec_dump.py --message 0x4158 --text`
prints a message's full parameter table, types, enum values and notes. It is the authority for TLV
parameter ids; do not hand-derive them.

#### 5.2 Tiers

| Tier | Contents | Default |
|---|---|---|
| `Proven` | NowPlaying, RouteGuidance, CallState — the pre-expansion baseline | recovery only |
| `Extended` | + Communications, ListUpdate, Power, AppDiscovery, LaneGuidance, Destination, device info | device-accepted |
| `Capability` | + VoiceOver, VoiceOver cursor, AssistiveTouch, MediaLibrary | off |

`Capability` is separated because those four declare that the accessory *does* something — renders
VoiceOver, hosts AssistiveTouch, browses the library — which changes how iOS drives the head unit,
not just what it reports. Everything in `Extended` only reports state.

`rx-only` also exists — param 6 at the baseline, param 7 grown. It is a **documented dead end**, kept
only so the experiment is not repeated: it constructs exactly the condition Apple's reason enum calls
`OptionalMsgNotValidWithoutRequiredMsgs` (§6.2).

The compiled default is `proven` — an interim safety floor per docs/carplay/04_CAPABILITIES_AND_CONFIG.md (compiled box-side defaults for
configurable values are to be retired as the app-pushed config covers tier selection). A build that
defaults to a declaration the device may reject cannot hold a session, and the failure is
unrecoverable within it.

Levers (interim, box-side — per docs/carplay/04_CAPABILITIES_AND_CONFIG.md tier selection migrates to app-pushed config; until then these
are how the current build is driven), resolved ONCE per process (not per Identify) and cached —
editing the file mid-run has no effect until the daemon restarts, which matters for the long-lived
`iap2d`:

```
CARPLAY_METADATA=proven|extended|all|rx-only     # environment
/tmp/carplay_metadata:  extended skip=call_history   # on-box file, same values plus a skip list
CARPLAY_METADATA_SKIP=power,destination          # environment equivalent of skip=
```

The file is read **unconditionally** — `file_setting()` is always called. Only the tier *word* loses to `CARPLAY_METADATA`; the `skip=` lists **concatenate**. *(Corrected 2026-08-16; CLAUDE.md states this correctly.)* `/tmp` is tmpfs on the box, so a reboot always
returns to the compiled default — an experiment cannot strand a working session. **Re-arm after any box
reboot** with `echo extended > /tmp/carplay_metadata`.

**MIGRATED 2026-08-10 (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B3) — these levers are now the APP-LESS BENCH PATH ONLY.** Tier
selection rides the app-pushed `metadata: {tier, skip}` section: the app emits it in every config
push (shipping `proven`, byte-equivalent to the compiled floor) and each daemon arms it once per
process before its Identify — iap2d at startup and again at SendIdentify, airplayd per control
connection, via `iap2_core::config::Iap2Config::arm_metadata_policy` →
`features::arm_pushed_policy` (first-arm-wins, so one link's declaration and its subscribes always
come from one snapshot). Precedence is **pushed > env > file > compiled `proven`**, so with an app
connected the re-arm instruction above has no effect — raise the tier in the app. The app-pushed
path REFUSES `rx-only` outright, and the app re-pushes on every SUBSCRIBE, which retires the re-arm
wart. **When a CHANGED tier takes effect differs per arm** (arming is first-arm-wins per process):
airplayd is spawned per session, so the AirPlayTunnel arm picks up a new tier on the next session;
iap2d is long-lived and survives app teardown, so the WIRED arm changes only when iap2d restarts —
a phone unplug/replug (the gadget goes un-CONFIGURED, iap2d exits, `projection_up.sh` respawns it),
NOT an app reconnect. A differing push against an already-armed process logs
`pushed metadata tier … IGNORED`. The BT-time Identify is unreachable from the pushed tier at all
(its params 6/7 are a hardcoded list in `message.rs`). The `skip` list from a pushed config REPLACES the bench lists rather than
concatenating: app intent must not be silently mixed with stale on-box experiment state.

#### 5.3 What the declaration became

Wireless tunnel, `extended` — the shape iOS accepted (~342 B Identify; corrected 2026-08-01: was
340 B — `0xAD03 RequestAppDiscoveryAppIcons` was added to `app_discovery`'s param 6 on 2026-07-30,
`features.rs`, adding 2 bytes):

```
param 6  0x5000 0x5002  0x5200 0x5203  0x4154
         0x4157 0x4159  0x4170 0x4172  0xAE00 0xAE02  0xAD00 0xAD02 0xAD03  0x10DA 0x10DC 0x10DD
param 7  0x5001  0x5201 0x5202  0x4155  0x4158  0x4171  0xAE01  0xAD01 0xAD04
         0x5204  0x10DB  0x4E09 0x4E0A 0x4E0B 0x4E0C 0x4E0D 0x4E0E
```

Every Start is paired with its Stop except `0x4154 StartCallStateUpdates`, whose unpaired form is
device-accepted and deliberately left alone (§6.3). See §5.6 rule 1. Wired is this unioned with its own floor, which
additionally carries the call-control cluster (0x415A–0x4161).

The Bluetooth-time Identify is untouched and must stay that way: docs/wireless/00_WIRELESS_CARPLAY.md recorded iOS
rejecting params-6/7 growth there twice, breaking the WiFi handoff. Note that those rejects carried
only the generic marker, so what they actually objected to was never established (§6.2).

#### 5.4 Bug fixed on the way

`start_list_updates` sent RecentsListProperties(1) and FavoritesListProperties(6) as **empty** groups.
iOS ignores an empty group, so call history could not have arrived even had 0x4171 been declared. The
table nests the per-entry field selectors, which is what Apple's 0x4170 definition requires.

#### 5.5 Parsers and display

New parsers in `metadata.rs`: `power`, `app_discovery`, `app_icon`, `lane_guidance`, `destination`
(Q10.22 fixed-point coordinates), `device_update`, `bluetooth_connection`, `voice_over`,
`voice_over_cursor`, `assistive_touch`, `media_library`. One `dispatch` function serves both
transports; `dispatcher_handles_every_declared_update_id` asserts it covers every id the table
declares receivable.

`MediaLibraryUpdate` is summarised (library id, revision, items in frame) rather than mirrored. A full
sync is a paged database of tens of thousands of items and CarPlay draws its own browse UI.

The host app gains Apps, Power, Device and Accessibility categories, and `KeyValuePane` replaces the
one-off telephony pane. Unmapped kinds still land in the category's event list, so nothing is lost.

#### 5.6 Declaration rules iOS enforces

Learned from the device, not inferred. Rules 1 and 2 are structural in `features.rs` (the `Trigger`
type makes an unpaired Start unwritable and names rider dependencies); rule 3 is a test assertion;
rule 4 (consent) is not encoded at all and is not representable as a protocol invariant.

1. **A `Start*` must be declared with its `Stop*`.** Omitting the Stop returns
   `RequiredInfoMissing` against the Stop id and rejects the whole feature (§6.3). Declaration is a
   capability statement, not a promise of traffic — we never send a Stop and declare them all anyway.
   Destination Sharing additionally requires `0x10DD DestinationInformationStatus`.
2. **A receive must not be declared without its send.** Apple's reason enum names this
   `OptionalMsgNotValidWithoutRequiredMsgs`; it is what refuted `rx-only` (§6.2).
3. **A subscribe for an id param 6 does not declare is silently ignored** — no error, no data. This is
   how 0x4157/0x4170 were sent for the project's history with nothing coming back (§4).
4. **Some feeds need user consent.** 0x4170 prompts for Contacts & Favorites on the iPhone. The prompt
   is driven by the iAP2 declaration, not by any Bluetooth profile (§6.4).

A `0x1D03` is unrecoverable within a session: params 6/7 are in `REQUIRED_IDENT_PARAMS`, so the retry is
byte-identical, the second reject aborts, and the session is cleared with no auth, no identify and no
subscribes. Recovery is `CARPLAY_METADATA=proven`, pinned by test to the pre-expansion bytes.

#### 5.7 Not included

Call control (0x415A–0x4161: initiate, accept, end, swap, merge, hold, mute, DTMF) stays wired-only.
It is command capability rather than metadata, and its ids have never been wire-captured on this
project's hardware. Adding it to the tunnel is a separate, deliberate change. Per docs/carplay/04_CAPABILITIES_AND_CONFIG.md this is a
per-transport sequencing decision (pushed config needs per-transport-arm applicability, docs/carplay/04_CAPABILITIES_AND_CONFIG.md),
not a permanent box-side scope policy.


---

### 6. Device results, 2026-07-25

Three sessions against a real iPhone. The third produced the answer; the first two are recorded because
each refuted a standing belief.

#### 6.1 `extended` — rejected, no detail

Link and MFi auth completed, then `0x1D03` twice with `[len=4][pid=6]` — a bare param-6 marker, no
message ids. Session cleared, no SETUP, no subscribes. Same shape docs/wireless/00_WIRELESS_CARPLAY.md saw on Bluetooth.

#### 6.2 `rx-only` — rejected, and the shape was wrong

Param 6 held at the accepted baseline, param 7 grown. First reject named a specific id:
`[param 6][], [param 7][0x4171]`. Dropping `call_history` removed that complaint and the message was
still rejected, now with the bare param-6 marker — while param 6 had not changed at all.

Two conclusions. The bare param-6 marker is a **generic rejection marker, not a statement about param 6's
contents**; docs/wireless/00_WIRELESS_CARPLAY.md's "params 6/7 growth is rejected as a class" was read off that marker in runs that
happened to grow param 6, so it was confounded and is not supported. And `rx-only` is structurally
invalid: Apple's reason enum contains `OptionalMsgNotValidWithoutRequiredMsgs`, which is exactly what
declaring a receive without its send constructs. The mode is retained only as a documented dead end.

#### 6.3 The reject reason, read from the phone

`accessoryd` logs the rejection in full. From
`captures/2026-07-25_iphone_iapreject_requiredinfomissing.txt`:

```
Identification info rejected for feature [Power], reject reason: 2
Identification info rejected for feature [Destination Sharing], reject reason: 2
Identification info rejected for feature [App Links], reject reason: 2
iapreject: Identification Rejected Details:
iapreject:  Param: MessagesSentByAccessory
    [msgID: 0xae02 Reason: RequiredInfoMissing]    StopPowerUpdates
    [msgID: 0x10dc Reason: RequiredInfoMissing]    StopDestinationInformation
    [msgID: 0x10dd Reason: RequiredInfoMissing]    DestinationInformationStatus
    [msgID: 0xad02 Reason: RequiredInfoMissing]    StopAppDiscoveryUpdates
```

**iOS requires each `Start*` to be declared together with its `Stop*`.** The table set `stop: None` on
every new feature, reasoning that we never send a Stop so there was nothing to declare. Declaration is a
capability statement, not a promise of traffic. Fixed: every extended and capability feature now
declares its Stop, and Destination Sharing additionally declares `0x10DD DestinationInformationStatus`.

`call_state` is deliberately left without `0x4156` — the baseline iOS accepts omits it, and iOS did not
ask for it.

#### 6.4 The contacts prompt

The 6.3 session prompted the user for **Contacts & Favorites** on the iPhone, from declaring
`0x4170 StartListUpdates` alone.

This closes the question of whether call history needs a Bluetooth phonebook profile. It does not. At
the time of this session the adapter advertised exactly one SDP service — "Wireless iAPv2",
`00000000-deca-fade-deca-deafdecacaff`, SerialPort descriptor, no HFP, no PBAP, no MAP
(`crates/bt-common/src/sdp_server.rs` `iap2_service`) — and the consent prompt appeared anyway. (Since
2026-09-03 the same responder additionally advertises an Android Auto SDP record — `sdp_server::run_services`
in `crates/vendor/wireless/src/main.rs` — but that record carries no phonebook profile either, so the
conclusion is unaffected.) The grant is driven by the iAP2 declaration and is held against the Bluetooth
device identity, which iOS joins to the CarPlay vehicle entry (forgetting one removes the other).

It also explains 6.2's `0x4171` complaint: consent had not yet been granted when that session ran.

No PBAP/HFP stub is required.

#### 6.5 How to read a reject

Do not bisect. `accessoryd` names the parameter, the message id and a reason from Apple's own enum:

```
NoError · Unsupported · RequiredInfoMissing · DuplicateID · DuplicateData · RepeatedParam
OutOfRange · InvalidLength · InvalidString · GroupParseError · ParamParseError
OptionalMsgNotValidWithoutRequiredMsgs · NotValidWithoutRequiredTransport · NotValidOnTransport
NotValidWithoutAssociatedData · InvalidData · FeatureNotSupportedByClass
```

Capture with `idevicesyslog -u <udid> -p accessoryd -o <file>` during the session, then
`grep -E "iapreject|Identification info rejected"`. Both transports also log a decoded line next to the
raw payload: `RX 0x1D03 decoded: param 7 unsupported: 0x4171 ListUpdate`.

Three sessions were spent guessing at a reject the phone was willing to explain in one.

#### 6.6 Fourth session — Identify accepted, data flowing on the box

`extended`, with the Stop pairs declared:

```
TX 0x1D01 IdentificationInformation (~342 B, AirPlay tunnel; corrected 2026-08-01: was 340 B —
                                      0xAD03 added to app_discovery's param 6, 2026-07-30)
RX 0x1D02 -> Identified
8 subscribes sent: now_playing route_guidance call_state communications
                   call_history power app_discovery destination
```

Inbound over the RCS tunnel, one session:

| id | message | count |
|---|---|---|
| 0x5001 | NowPlayingUpdate | 229 |
| 0xAE01 | PowerUpdate | 9 |
| 0x4171 | ListUpdate | 3 |
| 0x4E09–0x4E0E | device updates | 1 each |
| 0x4158 | CommunicationsUpdate | 1 |
| 0xAD01 | AppDiscoveryUpdate | 1 |
| 0x4155 | CallStateUpdate | 1 |

The declaration problem is solved. Call history arrives, after the Contacts & Favorites consent.

**Open:** the host app's Telephony, Power and Device panes read empty despite the records reaching it
(its log shows `device` records at #1–3). The remaining fault is between the box's JSON field names and
the app's key lookups, or in the box parsers emitting empty objects. Diagnosis was slow because a pane
whose fields do not map renders as "nothing arrived", which is indistinguishable from a protocol
failure. Fixed: every structured record is now also kept raw in its category's event list, and
`KeyValuePane` renders those raw records when no field mapped. The next session sees the actual JSON in
the pane.

Also hardened: `device_update` and `lane_guidance` indexed `v[0]` without a length check. An empty TLV
value would have panicked the metadata thread.

---

## Carrier — DataStream / RemoteControlSession / stream type 130

<!-- absorbed: ../carplay/05_METADATA_AND_CONTROLS.md -->

Status: authoritative for the wireless iAP2 transport. Supersedes the transport premise of docs/wireless/00_WIRELESS_CARPLAY.md.

Wireless iAP2 does not ride `iAPSendMessage` inside `POST /command` for inbound traffic. iOS opens a
RemoteControlSession (RCS) DataStream — SETUP stream type 130 — and carries the iAP2 link there. This
project never answered that SETUP, so the channel never existed, which is why no inbound iAP2 frame
arrived in its history.

Confirmed working on hardware 2026-07-25: link establishment, MFi authentication, identification,
NowPlaying, RouteGuidance, CallState and album artwork all operate over this channel.

> **⚠️ OPERATIONAL STATUS ONLY IS STALE — the protocol facts below are unaffected and remain
> correct.** Read every present-tense "works / operates / confirmed in service" claim as **"proven
> once on 2026-07-25, then regressed on 2026-07-31"**; the channel was dead 2026-07-31 → 08-10 (§8),
> and the code that produced the 07-25 result was never committed in that form. Read §8 before
> trusting any claim about what currently runs. [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-45-1`.

Evidence: `captures/2026-07-25_SUCCESS_airplayd_wl_handshake.txt`,
`captures/2026-07-25_SUCCESS_artwork_session2.txt`,
`captures/2026-07-25_iphone_iap2_trace_sess{2,3}.txt`.

Sources: Apple's licensed R14G17 SDK, Xcode's CarPlay Simulator (`CarPlaySDK.framework`, the current
receiver side), the iOS 27 `24A5390f` extract, and the iPhone's own logs over USB.

---

### 1. Transport

#### 1.1 One transport, two carriers

The iPhone's iAP2 packet trace labels this transport `AirPlay`, spanning both `iAPSendMessage` /
`POST /command` and the RCS DataStream. They are one iAP2 transport with two carriers. A SYN sent over
`POST /command` is answered on the DataStream.

`POST /command` remains a working outbound carrier on iOS 27. It delivered every DETECT and SYN in both
archived sessions, including one where the RCS was never created. Only the inbound direction requires
the RCS channel.

#### 1.2 Channel creation

From iOS 27 `AirPlaySender`:

```
carEndpoint_createiAPChannelIfNeeded(FigEndpointRef)
[%{ptr}] Creating RCS channel for iAP
carEndpoint_createCommChannelInternal(FigEndpointExtendedRef, CFDictionaryRef,
                                      FigEndpointRemoteControlSessionRef *, CFStringRef *)
carEndpoint_handleiAPChannelEvent(FigEndpointRemoteControlSessionRef, CFStringRef, CFDataRef, CFTypeRef)
carEndpoint_sendCommandOverRCSChannel(FigEndpointRef, FigEndpointRemoteControlSessionRef,
                                      CFStringRef, CFDataRef)
```

`E9459FD0-BCAD-4C45-820F-1E72447EF2F2` is the iAP `clientTypeUUID`.

**CONFIRMED AND EXPANDED 2026-07-30 — there are SEVEN client types, not four, and the pairing is no
longer inferred.** This paragraph previously listed four and hedged that "the UUID-to-name pairing is
positional and therefore inferred". Both limitations are now retired. The `CFStringCompare` chain in
`_DataStreamSessionSetup` (Simulator `CarPlaySDK.framework`, `0xdc04`–`0xdfe4`) sets an integer type
tag and loads the matching name CFString on the very next instruction, so the pairing is **literal**:

| tag | name | `clientTypeUUID` |
|-----|------|------------------|
| 1 | iAP | `E9459FD0-BCAD-4C45-820F-1E72447EF2F2` |
| 2 | LogTransfer | `75AD9926-4777-42B2-A7D8-823EBEECF7AA` |
| 3 | VehicleDataProtocol | `3E2F3C61-AAD0-42CB-A8AA-BF22186DA62E` |
| 4 | VehicleDataProtocolHigh | `FF4A6720-F2BE-4F56-A3E1-DB3B4E37D634` |
| 5 | UrlFling | `A6B27562-B43A-4F2D-B75F-82391E250194` |
| 6 | OverlayUI | `E3DC3EA6-E6C3-4B30-847C-B7ACFEBEA654` |
| 7 | SenderSettingsData | `BB493F61-A6B8-4769-8D74-80C23A9F71C4` |

Tags 3 and 4 are independently corroborated on the wire by Apple's own Simulator, which prints name
and UUID on one line (`DataStream VehicleDataProtocolHigh (id=1) for [..-RCS-274/FF4A6720-…]`). Tags
5–7 were unknown to this project; `UrlFling` is not hypothetical — `AirPlayUrlFling` appears as a live
log category in the same capture.

**Divergence from Apple worth knowing:** at `0xdc08`, an **absent** `clientTypeUUID` sends Apple's
receiver straight to the teardown path (`w8 = -6735`, `__DataStreamSessionTearDown`) — the same path as
an *unrecognised* UUID. Apple treats absent as a hard SETUP failure, as it does for absent `channelID`
and `clientUUID`. Our `session.rs` instead defaults an empty string to iAP. That branch should be
unreachable anyway (see §1.3: the SETUP request carries the real value; the `clientTypeUUID=(-)` in the
phone's log is a *log line*, not the request), so the deviation is permissive-but-harmless rather than
a bug — but it is a deviation.

#### 1.3 SETUP exchange

Stream type 130. Observed request:

```
reqKeys = [controlType, channelID, seed, clientUUID, type, wantsDedicatedSocket,
           sendMessageAsIs, clientTypeUUID]
channelID            = "5E:F7:F7:A9:CB:CD-RCS-1"
clientTypeUUID       = E9459FD0-BCAD-4C45-820F-1E72447EF2F2
controlType          = 2
wantsDedicatedSocket = true
seed                 = <u64, per session>
streamConnectionID   = absent
```

> **⚠️ `streamConnectionID` ABSENT IS NORMAL AND REQUIRED READING. Never treat it as invalid.**
>
> This stream does **not** carry a `streamConnectionID`, and it never will — it salts from `seed`
> (§1.4). Any code that parses it as `unwrap_or(0)` and then rejects zero will silently destroy the
> entire wireless metadata plane, because the stream is skipped before its own handler runs, no
> `streamID` transport token is returned, and the phone's outbound iAP2 path never exists. The tunnel
> pins at `Init`, no metadata arrives, and **A/V stays perfectly healthy**, so nothing looks wrong.
>
> This is not hypothetical. It happened for ten days, 2026-07-31 → 2026-08-10 (§8).
>
> The A/V streams (100–102, 110, 111) DO require a non-zero scid — it is their HKDF salt, and Apple
> enforces it at `AirPlayReceiverSession.c:4343` (the **screen** path). That requirement belongs to
> those types only. Any guard expressing it must be an **allowlist of the A/V types**, never a
> deny-list of exceptions; a deny-list silently applies an A/V rule to every stream type Apple adds
> next, which is exactly how the outage happened.
>
> **Do not "fix" this by citing R14G17's `_GetStreamSecurityKeys:4723-4747`**, which builds
> `"DataStream-Salt" + streamConnectionID`. That is the screen/audio use of the same constant. Type
> 130 reuses the constant with a different id (`seed`), and type 130 does not exist in R14G17 at all
> (`AirPlayCommon.h:251-255` stops at 110). Misreading those lines is the most likely route to
> reverting the fix.

Required response (`_DataStreamSessionSetup` in `CarPlaySDK.framework`):

```
type      = 130
streamID  = <int64 transport token, non-zero>
dataPort  = <TCP port>     // when wantsDedicatedSocket
```

- `streamID` is the transport token. Without it the phone logs `Failed to obtain transport token from
  SETUP response: -6727 kNotFoundErr`, then `apEndpointRemoteControlSession_sendMessageInternal` fails on
  every attempt. That function is the phone's entire outbound path to the accessory.
- `wantsDedicatedSocket` is mandatory for Apple's receiver, which fails setup with `-6714` otherwise.
  > **⚠️ UNSOURCED (flagged 2026-08-16).** `-6714` appears nowhere in this repo outside this line and the
  > notes citing it, and `receiver/src/session.rs`'s type-130 arm states the opposite — Apple's receiver
  > "does not branch on this at all", consuming the key only for a log line, and the absent-key arm has
  > never been observed because every capture sends it `true`. Do not cite this as established.
- `streamConnectionID` is never read back from our response; only `streamID` and `dataPort` are.
- The phone's RCS-creation log line reports `clientTypeUUID=(-)`. That is a creation-time view; the SETUP
  request carries the real value. Gate on the request, not the log line.

#### 1.4 Crypto

HKDF salt is `DataStream-Salt<seed>`, using the `seed` from this stream's SETUP — not
`streamConnectionID`, which the RCS SETUP does not carry.

```
info  DataStream-Output-Encryption-Key   decrypts iPhone -> receiver
info  DataStream-Input-Encryption-Key    encrypts receiver -> iPhone
```

`AirPlayReceiverSessionDataStreamCreate` passes the Output key as `MDC::EncryptionReadKey` and the Input
key as `MDC::EncryptionWriteKey`. Both directions are confirmed in service.

#### 1.5 Frame codec

Identical to the control and event channels; `rtsp::control::ControlChannel` drives it unchanged.

```
[u16 LE payload length][ChaCha20-Poly1305 ciphertext][tag:16]
AAD   = the 2-byte length prefix
nonce = 4 zero bytes || u64 LE frame counter, counters independent per direction
```

The DataStream uses `NetSocketChaCha20Poly1305Configure`, which frames at 16384 bytes. The RTSP/HTTP path
uses `NetTransportChaCha20Poly1305Configure` at 1024. The `NetSocket` variant does not exist in the 2017
SDK, which is how the 1024 figure entered this project.

#### 1.6 RCS message header

32 bytes, all fields big-endian, then the payload.

```
0x00  u32   totalLength    includes the 32-byte header
0x04  4CC   transport kind 'sync' | 'asyn' | 'rply'   (else -6717 kFormatErr)
0x08  u64   opaque; Apple's public send path always emits 0
0x10  u32   messageType    'comm' inbound / 'cmnd' outbound
0x14  u64   messageID / replyToken   (random for 'sync', 0 for 'asyn', echoed in 'rply')
0x1c  u32   OSStatus       (meaningful only in 'rply')
```

Verified against three independent Apple implementations: `CarPlaySDK`'s
`controlServer_sendRequestInternal`, `sendResponseInternal` and `receiveData`, plus the same
`APMediaDataControlServer` inside iOS 27's `AirPlayReceiver`. Geometry is byte-identical across both
operating systems. The `0x14` and `0x1c` semantics come from Apple's format string: `sending response
(id: %@, result: %@, error: %u)`.

#### 1.7 The message type is direction-asymmetric

Phone to accessory uses `'comm'` (`0x636F6D6D`). Accessory to phone must use `'cmnd'` (`0x636D6E64`).

`_apEndpointRemoteControlSession_startMessageHandling` (`AirPlaySender` @ `0x25175a3d4`) accepts exactly
two `OSType` values:

```asm
mov  w8, 0x6564 ; movk w8, 0x6469, lsl 16   ; 'died'  (internal, disconnect only)
cmp  w0, w8
mov  w8, 0x6e64 ; movk w8, 0x636d, lsl 16   ; 'cmnd'
ccmp w0, w8, #4, ne
b.ne 0x25175a4fc   ->  mov w23, 0 ; ... ; retab
```

The reject path contains no logging call and branches before the `CFRetain`: a silent drop returning
`noErr`. `apEndpointRemoteControlSession_sendMessageInternal` stamps `'comm'` at all four of its send
sites; `CarPlaySDK`'s `_AirPlayReceiverSessionDataStreamSendInternal` stamps `'cmnd'` at all three of
its, and `'comm'` does not appear anywhere in `CarPlaySDK`.

This was the final blocker. An earlier revision built the outbound header by reproducing the phone's own
frame verbatim, which is correct for receive and wrong for transmit. Every ACK was stamped `'comm'` and
discarded. The device sat in FSM state `Pending`, which has no timeout-to-RST edge, retransmitting
SYN-ACK indefinitely with no diagnostic on either side.

Apple's `_AirPlayReceiverSessionSendiAPMessage` sends iAP as `'sync'` rather than `'asyn'`, with a random
8-byte messageID and a 10 s wait. We send `'asyn'` and it is accepted: the filter never inspects the
kind, the phone sends us `'asyn'`, and `VehicleDataProtocol` uses it. If a future iOS tightens this,
`'sync'` is a two-line change.

#### 1.8 RCS messages span crypto frames

A 65,535-byte iAP2 link packet becomes a 65,567-byte RCS message. The crypto frame length field is `u16`
and the DataStream frames at 16,384 bytes, so any message above one frame arrives in several pieces and
must be reassembled on `totalLength` before the envelope is parsed.

`drain_rcs` in `session.rs` accumulates decrypted plaintext and emits complete messages, capped at
256 KB — `totalLength` is peer-supplied and a corrupt value must not drive allocation on a 123 MB box.

An earlier revision parsed each decrypted frame in isolation, so every multi-frame message failed
`declared == pt.len()` and was dropped. Album artwork was the case that exposed it: Setup (42 B) and the
final fragment (15,852 B) fit one frame and parsed; the two 65,557-byte data messages did not, and the
transfer never completed. Signature in the log: `envelope did NOT parse`.

---

### 2. iAP2 link layer

#### 2.1 Apple's implementation is available locally

`iAP2Link.c` and `iAP2LinkAccessory.c` are statically linked into the CarPlay Simulator binary, with
build paths intact: `…/CarPlaySimulator_Devices/Libraries/iAP2/iAP2/Public/iAP2Link/iAP2Link.c`. That is
the accessory-side FSM. The device side (`iAP2LinkDevice.c`) is in the iOS 27 extract's `accessoryd`.
Use these before any vendor implementation.

Accessory FSM actions: `SendDetect`, `SendSYNNewSeq`, `ResendSYN`, `RestartSYNWithRetransmit`,
`ConnectedACK`, `SendSYNACK`, `SendACKForOldSYN`.

`ConnectedACK` asserts the received packet has the SYN bit set and replies with a bare ACK carrying
`seq = sentSeq`, `ack = recvSeq`, `session = 0`, header only. `build_ack` in `iap2-core` is equivalent.

Constraints:

- A repeated SYN with the same seq is safe. `ResendSYN` reuses the latched seq, and `SendSYNACK` seeds its
  base once without storing back, so a repeat produces an identical SYN-ACK.
- The device counts received SYNs and fires `NotifyConnectionFail` at the 11th. Any retry loop needs a cap
  of 10 or fewer.
- Sending DETECT to an attached device re-runs its attach path and resets the link. Resend SYN only.
- Checksum selection is `link[0xd5] == 2`: version 2 uses a 10-byte header and 2-byte Fletcher checksum;
  versions 1 and 3 use the classic 9-byte header and 1-byte negated sum. We negotiate 1. Raising
  `LinkVersion` silently changes the wire format.

#### 2.2 SYN parameter field order

From Apple's validator format strings:

```
version | maxOutstanding | maxPacketSize | retransmitTimeout | cumAckTimeout
        | maxRetransmissions | maxCumAck | numSessionInfo | session[id,type,ver]
```

Bytes 2..4 are `MaxPacketSize`. An earlier reading, taken from SpeedPlay's re-derived stack, had them as
`MaxRetransmissions`.

The tunnel requires `MaxPacketSize = 0xFFFF` — Apple's transport-type-2 template value, matching the
stream's `controlType=2` and the phone's own SYN-ACK. `SYN_PARAMS_ZERO_ACK_TUNNEL` carries it; BT and
wired keep their proven constants.

The phone uses this in full: observed artwork fragments are 65,525-byte link payloads, four times the
16384-byte RCS framing limit. Such a message always spans several crypto frames and requires reassembly
(§1.8).

#### 2.3 Version downgrade

`iAP2LinkCheckNegotiation` logging `Older protocol Version detected on the accessory (3->1)` is benign.
It fires on Bluetooth as well, 81 ms before that link connects and then authenticates and identifies
fully. It rewrites the negotiated parameter block to the version-1 profile but does not change the wire
format.

---

### 3. Album artwork (link session 2)

NowPlaying attribute 26 carries a transfer id; the JPEG arrives as iAP2 File Transfer datagrams on link
session 2.

```
iPhone -> box  [id][04][size u64 BE]   Setup   (size 0 = probe, ignore)
box -> iPhone  [id][01]                Accept  (required; no data flows without it)
iPhone -> box  [id][flags][data]       Data    bit7 First, bit6 Last
box -> iPhone  [id][05]                Success (after the Last fragment)
```

`Artwork::on_session2` in `iap2-core` implements this and is used by both `iap2d` (wired) and the tunnel.
The trailing payload-checksum byte must be stripped before handing a fragment to the assembler: control
messages self-bound via their `[40 40][total]` header, raw fragments do not, and appending it injects one
stray byte per fragment.

`Artwork::on_session2` treats a Last-flagged fragment as complete only if the accumulated byte count
reaches the declared size. A short buffer is reported as `[art] INCOMPLETE id=… N B of M`, and neither
the image nor a Success reply is emitted: a truncated JPEG the phone believes was delivered is worse
than a missing one.

Verified 2026-07-25 with `airplayd c1db72bea1c8aa0756d9a44d7f33a612`: two transfers, 94,376 B and
102,411 B, both byte-exact against their declared sizes, and visually confirmed in the host app.

A superseded comment in `metadata.rs` claimed the phone would not offer artwork until the accessory
declared a supported-artwork-format list in `0x1D01`. No such parameter exists in Apple's iAP2 catalogs,
and no declaration was needed.

---

### 4. Implementation

- `receiver/src/datastream.rs` — RCS envelope, typed builders, generation-guarded outbound sink.
- `receiver/src/session.rs` — stream-130 SETUP arm, key probe, decrypt, RCS reassembly, routing; gated on the iAP
  `clientTypeUUID` and on `CARPLAY_WIRELESS_METADATA`.
- `receiver/src/events.rs` — `send_iap_message` prefers the DataStream sink, falls back to
  `POST /command`.
- `receiver/src/iap_tunnel.rs` — link state machine, `link_up` gating, session-2 artwork routing.
- `iap2-core/src/link.rs` — `SYN_PARAMS_ZERO_ACK_TUNNEL`, `SYN_PARAMS_TUNNEL_RETRANSMIT`.

Deployed and verified: `airplayd c1db72bea1c8aa0756d9a44d7f33a612`.

---

### 5. Open

1. Inbound `'sync'` obliges a `'rply'`; we log but do not send one. All observed frames are `'asyn'`.
2. Post-`Identified` frames bypass `state::on_message`, so a re-`0x1D00`, re-`0xAA00` or `0xAA04` falls
   through to the generic dispatcher.
3. The v2 checksum path is understood but untested.
4. We declare 2 link sessions where Apple's transport-type-2 template declares 3, and session version 1
   where it uses 2. Deferred so the first hardware test had one variable; that test has run.
5. `viewAreas` is emitted in `/info` but not echoed in the SETUP feature list, drawing a warning from the
   phone. `extendedFeatures` is nested inside the HEVC block and so is absent from non-HEVC sessions.
6. RESOLVED. The tunnel Identify now declares the full metadata set, generated from
   `iap2-core::features`; 0x4157/0x4170 and nine other ids were re-added and accepted. See docs/carplay/05_METADATA_AND_CONTROLS.md §6.

---

### 6. Working with iOS logs

- `idevicesyslog` over USB works on iOS 27 and supplied the entire phone-side picture.
- `accessoryd` emits a full iAP2 packet trace:
  `LOG; <t>; <endpoint>; <transport>; <Acc|iPod|Event>; len=; control=; seq=; ack=; session=; hdrChk=;
  payload(len= chk=)=<…>`. `Acc` is the accessory, `iPod` the phone. Transports: `Bluetooth Classic`,
  `AirPlay`.
- The trace is gated on `com.apple.iapd PrintIapPackets`, read at process launch. Installing a profile
  has no effect until `accessoryd` restarts. Reboot the phone and confirm `readLoggingPrefs` appears
  before trusting a null result.
- The trace persists on-device and can be pulled retroactively with `idevicesyslog archive` and
  `/usr/bin/log show --archive`. There is no need to start a capture before the session. Use
  `/usr/bin/log` explicitly; a shell function shadows `log` and swallows arguments.
- Do not judge trace availability from an idle sample: `accessoryd` is silent with no accessory attached.
  Plug in an MFi accessory for ten seconds and grep for `LOG;`.
- `grep " accessoryd"` without a word boundary also matches `audioaccessoryd` (AirPods proximity
  pairing). This produced one incorrect conclusion.
- `/tmp` on the box is tmpfs. Copy `/tmp/airplayd_wl.log` off before any reboot.
- Artwork transfers occur after NowPlaying settles. Sampling the box log immediately after session start
  will show no session-2 activity even when it later succeeds.

Tooling: `tools/capture_iphone_carplay.v2.sh`, `tools/extract_iap2_trace.sh`.

---

### 7. Corrections to earlier documents

- docs/wireless/00_WIRELESS_CARPLAY.md assume `iAPSendMessage` is the inbound carrier. Their message-shape, plist-key and
  link-layer content remains valid; the transport conclusion does not.
- docs/wireless/00_WIRELESS_CARPLAY.md (2026-07-17) identified the failure correctly — unhandled SETUP stream types are dropped — and
  went unactioned for eight days.
- docs/carplay/05_METADATA_AND_CONTROLS.md and docs/carplay/05_METADATA_AND_CONTROLS.md identified `carEndpoint_createiAPChannelIfNeeded` and
  `carEndpoint_sendCommandOverRCSChannel` as the mechanism on 2026-07-23, then retracted it as dead code
  because a static caller search found no callers. The call is made indirectly through endpoint
  activation. Absence of an observed caller is not evidence of absence of a caller.
- docs/wireless/00_WIRELESS_CARPLAY.md's params 6/7 conclusion is scoped to the Bluetooth Identify only.
- docs/ops/03_REFERENCE_INDEX.md stated the iAP2 link layer was not available locally; Apple's copy is in the Simulator binary.
  It also demoted the iOS extracts, which supplied the answer.
- docs/carplay/03_SDK_GROUND_TRUTH.md §1's stated root cause is not the root cause; §8.1's rejection of `0xFFFF` was wrong and was
  cited to block the required change.
- docs/carplay/02_SESSION_LIFECYCLE.md's Zero-Ack row instructed readers not to make that same change.

#### Claims made and later corrected during this investigation

Recorded because the pattern matters more than the individual errors. Each was a generalisation from
partial evidence, stated as an observation:

1. `accessoryd`'s `dataLength: 9` was attributed to the AirPlay transport; it was a Bluetooth endpoint.
2. A re-DETECT was attributed to sink registration; it was a startup race, and a second capture with no
   RCS channel at all showed the same behaviour.
3. The stream-130 SETUP arm was described as inert when the feature flag was unset; it was not gated.
4. DETECT #2 was said to precede the phone's SYN-ACK by 5.7 ms; the archived traces show it following by
   13 ms. The box-side log needed to settle this was never archived.
5. The phone was said not to offer the artwork transfer; the measurement was taken before the transfer
   occurs in a session.
6. A 65,525-byte artwork fragment was said to have been received intact despite exceeding the crypto
   frame size, and was carried into two documents as evidence that reassembly was unnecessary. It had
   been dropped: the log showed eight `envelope did NOT parse` and artwork never completed. The claim
   came from grepping for the fragment's arrival on the link rather than for the transfer's completion.

7. `rx-only` — declaring receives without their sends — was proposed as a way past the reject. Apple's
   reason enum names that exact condition `OptionalMsgNotValidWithoutRequiredMsgs`; the device refuted
   it in one session (docs/carplay/05_METADATA_AND_CONTROLS.md §6.2).
8. The bare `[len=4][pid=6]` reject marker was read as "iOS objects to param 6". It is a generic
   rejection marker: it appeared unchanged in a run where param 6 was the accepted baseline. This also
   invalidates docs/wireless/00_WIRELESS_CARPLAY.md's "params 6/7 growth is rejected as a class", which was read off the same signal
   and blocked the metadata work for two days.

Practice: cite the capture, the line and the transport for every on-wire claim. When the device can be
asked directly, ask it — `accessoryd` names the parameter, the message id and the reason (docs/carplay/05_METADATA_AND_CONTROLS.md §6.5).

---

### 8. The 2026-07-31 → 08-10 regression (added 2026-08-10)

**The channel documented above was dead for ten days.** Recorded here because this is the document a
future session reads before touching this code, and because the failure mode is invisible in the
place people look.

**What broke.** `5ce9d1c` (2026-07-31) added a guard rejecting any SETUP stream whose
`streamConnectionID` was 0 or absent, applied to **every** stream type. Per §1.3 the RCS iAP channel
carries no `streamConnectionID` at all, so `unwrap_or(0)` yielded 0 and every type-130 SETUP was
skipped *before reaching its own handler*. No `streamID` transport token went back, so — exactly as
§1.3 predicts — the phone's outbound path never existed.

**Why it survived ten days and ten commits to `session.rs`:**

1. **It is silent by construction.** A/V is a different stream set on a different code path. Video and
   audio stayed perfect (32–37 fps, zero decrypt failures) for the whole outage. Every health check
   that sampled A/V reported green.
2. **The guard was never written down.** It appears only in `5ce9d1c`'s commit message, credited to
   "the docs/carplay/03_SDK_GROUND_TRUTH.md Simulator-verification fixes" — and docs/carplay/03_SDK_GROUND_TRUTH.md §7 does not mention it. Nobody could
   review a scope that was never stated.
3. **The error counters all read zero, correctly.** Zero `iAPSendMessage` 400s, because there were
   zero sends. Absence of traffic looked like absence of errors.
4. **The oracle could not see it.** `relay.rs::setup_surface` compared `streams[].type` only, so a
   response that kept `{type:130}` and dropped `streamID` diffed clean. Widened to a per-stream key
   set on 2026-08-10.
5. **docs/ops/02_TESTING.md's triage table pointed away from it** — "no stream-130 SETUP at all → look at `/info` and
   the `enabledFeatures` echo". The SETUP *was* arriving; we were refusing it. Row added.

**How it was actually found** (the general lesson): the box log was read for what the *phone* was
asking, rather than for what we were failing to do. The rejection was printing 33 times per session
in plain text the whole time. Sampling A/V health and tunnel state showed only that metadata was
absent — never why.

**Fix.** The guard is now an allowlist of the types whose key derivation actually consumes scid
(`100..=102 | 110 | 111`); everything else falls through to its own arm, and an unimplemented type is
omitted at the `_` arm with a named diagnostic, as Apple's receiver does. Regression test:
`crates/vendor/receiver/tests/setup_stream_130.rs` (a type-130 SETUP with no `streamConnectionID`
must yield a response entry with a non-zero `streamID`; a type-110 with scid 0 must still be skipped).

**Evidence.** `docs/ops/captures/2026-08-10_REGRESSION_datastream130_scid_rejected.txt` — 33 rejections in
one session, the SYN → reject → resent-SYN causal sequence, and the before/after contrast against
`2026-07-25_SUCCESS_airplayd_wl_handshake.txt:25,36` (same `scid=0`, accepted, `seed` salt solved).

**⚠️ The fix is not hardware-validated, and the arm behind it has ZERO hardware hours.** At the last
07-25 commit (`c1c5901`) `session.rs` had no 130 arm, no scid guard and no key probe, and
`datastream.rs` did not exist. The code that produced the 07-25 success was never committed in that
form — it was rewritten into `5ce9d1c` alongside the guard that made it unreachable. So the 07-25
capture proves the **protocol shape**, not this implementation. Treat the next wireless session as a
first run of the accept path, the key probe, the RCS reassembly buffer and the supersede logic.

---

## SDK research — messages and controls

<!-- absorbed: ../carplay/05_METADATA_AND_CONTROLS.md -->

Reference for two host-app windows: a **Metadata** viewer (what CarPlay can push to a wired accessory
and over which channel) and a **Controls** window (clickable buttons that send HID reports / `/command`
messages to the phone). Grounded read-only in Apple's **CarPlaySimulator** SDK, the machine-generated iAP2 `spec.rs`,
the wire-verified `ncm_carplayd/research/WIRED_*` captures, and the live ccpa_custom / ncm_carplayd
sources. Companion to **docs/carplay/03_SDK_GROUND_TRUTH.md** (SDK ground truth) — this doc drills into §7/§8/§9 for these two windows.

Claims are **[E]** evidenced (string / exported symbol / extracted const bytes / wire capture cited) or
**[I]** inferred. New-vs-docs/carplay/03_SDK_GROUND_TRUTH.md findings are flagged **[NEW]**.

### 0. Sources used (beyond docs/carplay/03_SDK_GROUND_TRUTH.md)
- **CarPlaySDK binary** (arm64e): `…/CarPlaySimulator.devicekitplugin/Contents/Frameworks/CarPlaySDK.framework/Versions/A/CarPlaySDK` (6.6 MB). `strings`/`nm -gU`/`otool -tV`. Strings dumped to scratchpad `sdk_strings.txt`.
- **iAP2 spec crate** (machine-generated from the plugin): `~/Downloads/github/carplayd/rust/carplayd/crates/iap2-core/src/spec.rs` — message IDs + Apple's own param-ID names.
- **Wire captures** (stock CCPA, byte-decoded): `ncm_carplayd/research/WIRED_METADATA_PLANE.md`, `WIRED_ALBUM_ART.md`, `WIRED_NAV_METADATA.md`.
- **ios27 HID inventory:** `ncm_carplayd/research/ios27_sdk_inventory/11_hid_input.md`.
- **Live code:** `ccpa/airplayd/src/main.rs`, `ccpa/iap2d/src/main.rs`; `ncm_carplayd/receiver_core/crates/receiver/src/{session.rs,events.rs,hid.rs,info.rs}`; sibling `carplayd/vendor/ncm_carplayd/.../src/{hid.rs,info.rs}`.

---

## QUESTION 1 — METADATA CATALOG

### 1.0 The load-bearing split: TWO planes, TWO transports
Wired CarPlay multiplexes two logical control planes over the single USB link, and **metadata is split
across both** [E — `WIRED_METADATA_PLANE.md` §"The load-bearing fact"; docs/carplay/03_SDK_GROUND_TRUTH.md §7,§9]:

| Plane | Transport | Terminated today by | Carries |
|---|---|---|---|
| **AirPlay/IP session** | RTSP `/command` (binary-plist, encrypted "Events" channel) + A/V/HID streams | ccpa `airplayd` + ncm `receiver_core` (the "box"/Mac) | UI/session control: modes, ducking, night-mode, appearance, limitedUI, focus, view-area, vehicle-info, HID reports, Siri trigger. **NO now-playing/nav/call text.** |
| **iAP2 control session** | iAP2 TLV messages over the MFi link (msgId + nested param TLVs) | ccpa `iap2d` (MFi auth + Identify **+ the generated declare/subscribe metadata plane**, corrected 2026-08-16) | NowPlaying (artist/title/album/artwork), CallState, RouteGuidance/turn-by-turn, MediaLibrary, Location, Vehicle. |

**Consequence for the Metadata window:** artist/title/nav/call text is an **iAP2** feed that the box's
`iap2d` must subscribe to — it never rides the AirPlay `/command` channel. The AirPlay `/command` inbound
list is a *different* metadata family (UI state, not media content).

### 1.1 (a) AirPlay `/command` inbound metadata (iOS → accessory) [E]
Transport: `POST /command` binary-plist `{type, params}` on the encrypted event channel; each dispatched
to a `..._f` callback. From docs/carplay/03_SDK_GROUND_TRUTH.md §9 + binary string windows (`sdk_strings.txt` ~8690–9140). Payload
schemas below are **[E]** where the adjacent key strings sit next to the command name in the binary.

| type | direction | payload fields (evidenced keys) | meaning |
|---|---|---|---|
| `modesChanged` | iOS→acc | mode state: `screen`, `mainAudio`, `speech`, `phone`, `turns`, each with `entity`{controller/accessory/none} + `permanent*`; `speechMode`{none/speaking/recognizing} | who owns each resource now. Log fmt: `Modes changed: screen %s (permScreen %s), mainAudio %s …` [E] |
| `duckAudio` | iOS→acc | `durationMs` (f64), target gain | lower accessory audio. Log: `Delegating ducking of audio to %f within %f seconds` [E] |
| `unduckAudio` | iOS→acc | `durationMs` | restore. `Delegating unducking of audio within %f seconds` [E] |
| `setNightMode` | iOS→acc | night-mode bool/enum (`nightMode`) | day/night UI switch [E — `setNightMode` string] |
| `uiAppearanceUpdate` | iOS→acc | `appearanceMode` (`AirPlayAppearanceMode`), `appearanceSetting` (`AirPlayAppearanceSetting`) | UI light/dark/tint. Fn `AirPlayReceiverSessionUIAppearanceUpdate(…, AirPlayAppearanceMode, AirPlayAppearanceSetting, …)` [E] |
| `mapAppearanceUpdate` | iOS→acc | `appearanceMode`, `appearanceSetting` | map light/dark [E] |
| `setLimitedUI` | iOS→acc | `limitedUI` / `limitedUIElements` (element token array) | restrict UI element set [E] |
| `showUI` | iOS→acc | `uuid`, `url` | foreground a URL. `ShowUI from controller uuid=%@ url=%@` [E] |
| `changeUIContext` / `stopUI` | iOS→acc | `urls`, context ids | UI-context handoff [E] |
| `performHapticFeedback` | iOS→acc | `hapticFeedbackType`, `uuid` | pulse a haptic [E — adjacent keys] |
| `deviceOfferFocus` | iOS→acc | `uuid`, `originXPixels/originYPixels/widthPixels/heightPixels`, `focusHeading` | offer input focus to a region [E — adjacent keys] |
| `startSession` / `stopSession` | iOS→acc | `stopSession` carries `disconnectReason` (u32). `Received stopSession command with reason %u` | session lifecycle [E] |
| `tearDownStreams` | iOS→acc | `streams[]` (`streamConnectionID`, `streamID`, `type`) | drop specific streams [E] |
| `requestViewArea` | iOS→acc | view-area request | ask accessory to change view area [E] |
| `setEnhancedSiriParams` | iOS→acc | `enhancedSiriParameters`: `bufferSizeMs`, `bufferAudioFormat`, `burstPeriodMs`, `voiceModelLanguage` | configure Siri mic buffering [E — adjacent keys] |
| `setOEMLogConfiguration` / `handleLogArchiveRequest` | iOS→acc | log config | OEM log control [E] |

**ccpa_custom status (anchors + verdict corrected 2026-08-16):** `receiver::session::AvSession::command()`
**logs every inbound `/command` type** and forwards the raw plist to the host over the `:9004` metadata
seam (`META_CMD`) — the `[command] ← iPhone POST /command type='{ty}'` log, then
`iap2_core::metadata::emit_command_plist(request_plist)`. *(The old `session.rs:324/330/839` anchors are
dead, and so is the `META_SINK` static they named: `metadata::emit_command_plist` now owns the single
`:9004` connection this process makes, because ocbmd keeps one producer per channel and two sockets from
the same process evicted each other in a loop.)* So the Metadata window can *already* display the inbound
`type` + raw params for every command above. **One inbound command IS acted on** — this bullet previously
read "No inbound command is acted on", which stopped being true when `modesChanged` handling landed.
`modesChanged` drives two effects: `events::handle_inbound_event`'s `modesChanged` arm updates the
MainScreen-focus atomic read by `events::screen_focused`, and `events::modes_changed_tunnel_nudge()` fires
a one-shot iAP2-tunnel link nudge (docs/wireless/00_WIRELESS_CARPLAY.md #2.8, docs/wireless/00_WIRELESS_CARPLAY.md) — the latter wired on BOTH the event channel and
`session::command()`, because `events.rs` records that inbound `modesChanged` actually arrives on the
CONTROL channel. `disableBluetooth` is recognised and logged but deliberately not acted on; everything
else is display-only. [E — cited symbols]

### 1.2 (b) NowPlaying / media metadata → **iAP2**, not AirPlay [E, wire-verified]
**Answer: wired CarPlay delivers artist/title/album/artwork/playback-state to the accessory over an iAP2
NowPlaying session — NOT AirPlay SET_PARAMETER/DAAP, NOT MediaRemote.** The AirPlay plane carries no
now-playing text; the CarPlay *video* already renders the now-playing UI, so the iAP2 feed exists for the
head unit's own surfaces (cluster / dashboard / our host window). [E — `WIRED_METADATA_PLANE.md` §"metadata
is iAP2, NOT AirPlay"]

**iAP2 message IDs** [E — `spec.rs:149-156`, wire-confirmed `WIRED_METADATA_PLANE.md`]:

| msgId | name | source | role |
|---|---|---|---|
| `0x5000` | StartNowPlayingUpdates | Accessory | **subscribe** (3 params = the attribute groups to receive) |
| `0x5001` | NowPlayingUpdate | Device (iOS) | the delta stream (~2/s) |
| `0x5002` | StopNowPlayingUpdates | Accessory | unsubscribe |
| `0x5003` | SetNowPlayingInformation | Accessory | accessory→iOS push (rarely used) |

`0x5001` carries **two param groups** [E — `spec.rs:920` `now_playing`]:
- **MediaItemAttributes** (group id 0) — wire-verified param map [E — `WIRED_METADATA_PLANE.md` L120-127]:

| attr id | field | type |
|---|---|---|
| 0x01 | Title | utf8 |
| 0x04 | Duration | u32 ms |
| 0x06 | Album | utf8 |
| 0x0c | Artist | utf8 |
| 0x1a | **Artwork** | u8 = fileTransferIdentifier (→ iAP2 File Transfer, see below) |

  (Genre/other attrs exist in the catalog; only the subscribed ones stream.)
- **PlaybackAttributes** (group id 1) [E]: id 0x00 PlaybackStatus (u8 play/pause/stop), id 0x01
  ElapsedTime (u32 ms), id 0x07 AppName (utf8, e.g. "Music"). Catalog also: queue index/count, shuffle,
  repeat, `SetElapsedTimeAvailable` [E — `WIRED_METADATA_PLANE.md` §36-40].

**What the accessory must do to receive it** (all four are load-bearing, learned the hard way) [E —
`WIRED_METADATA_PLANE.md` L133-140]:
1. Complete iAP2 auth+identify (state ≥ 5).
2. **Declare `0x5001` receivable** in the `0x1D01 IdentificationInformation` `MessagesReceivedFromDevice`
   list — *iOS sends nothing until you declare you can receive it.* Byte-exact subscribes fired but got
   **0 RX** until the declaration was added. Stock declared recv set = `4e0a 4e0b 4155 5001 fffa fffc 5201
   5202`.
3. **Subscribe** `StartNowPlayingUpdates 0x5000` naming the attribute groups/ids wanted. Stock subscribe
   body (`WIRED_METADATA_PLANE.md` L100-104): group id0 MediaItem = {0x01,0x04,0x06,0x0c,0x1a}, group id1
   Playback = {0x00,0x01,0x07}.
4. Merge deltas (it's a DELTA stream: full title/artist on track change, then elapsed-only frames — merge
   non-empty fields per feed or elapsed frames blank the title). [E]

**Artwork** is NOT inline — attr 0x1a is a **fileTransferIdentifier**; the JPEG (600×600 JFIF, ~100 KB)
arrives over a separate **iAP2 File Transfer session** (link session 2, type 1 = FileTransfer) [E —
`WIRED_ALBUM_ART.md`, fully wire-decoded + SOLVED]. Prereqs: subscribe attr 0x1a, declare `0x5001`,
negotiate session-2 as type 1 in the link SYN, send the `[id][0x01]` Accept, reassemble Data fragments
(`[id][First|Last|Type]…`), complete on the `Last` bit (0x40) or byte count.

### 1.3 (c) Navigation metadata — distinguish THREE directions [E]
Three distinct nav mechanisms; do not conflate them:

**(i) Turn-by-turn / route guidance = iAP2, iOS → accessory** [E — `spec.rs:157-163`, wire-verified
`WIRED_NAV_METADATA.md`]. This is the one that feeds a nav card in the Metadata window.

| msgId | name | source |
|---|---|---|
| `0x5200` | StartRouteGuidanceUpdates | Accessory (subscribe, 6 params) |
| `0x5201` | RouteGuidanceUpdate | Device — trip summary, ~1/s (27 params) |
| `0x5202` | RouteGuidanceManeuverInformation | Device — the current turn (14 params) |
| `0x5203` | StopRouteGuidanceUpdates | Accessory |
| `0x5204` | LaneGuidanceInformation | Device |

`0x5201` param names (Apple's own) [E — `spec.rs:928` `route_guidance_update`]: RouteGuidanceState,
ManeuverState, CurrentRoadName(3, utf8), DestinationName(4), EstimatedTimeOfArrival(5, u64 epoch),
TimeRemainingToDestination(6, u64 s), DistanceRemaining(7, u32 m), DistanceRemainingDisplayString(8),
…Units(9), DistanceRemainingToNextManeuver(10, u32 m) + display string(11)/units(12),
RouteGuidanceManeuverCurrentList(13), …TotalCount(14), plus EV/charging/timezone fields (22-26).
`0x5202` [E — `spec.rs:986` `route_maneuver`]: ManeuverDescription(2, utf8), ManeuverType(3, enum),
AfterManeuverRoadName(4), DistanceBetweenManeuver(5, u32 m) + string(6)/units(7), DrivingSide(8),
JunctionType(9), ExitInfo(13). Wire-verified live values in `WIRED_NAV_METADATA.md`.

**Extra offer gate for nav** [E — `WIRED_NAV_METADATA.md` §"offer gate"]: beyond declaring 0x5201/0x5202
receivable + subscribing, iOS withholds route guidance unless the accessory declares a
**RouteGuidanceDisplayComponent (id 30)** in `0x1D01` — `{identifier:u16=42, name:utf8="RouteGuidance"}`
(`spec.rs:776` `RouteGuidanceDisplayComponent` group; `spec.rs:897` sub-params). Only then do updates flow
on an active Apple Maps route.

**(ii) VDC vehicle-data-channel navigation = accessory → iOS, the OPPOSITE direction** [E — docs/carplay/03_SDK_GROUND_TRUTH.md §6].
The VDC `Navigation` accessory (`0E000002`, `reference/carplay_sdk/apple_vdc/`) is how the *accessory feeds
GPS/vehicle telemetry to iOS* for dead-reckoning — `RouteStatus`/`SystemInformation`/`RouteSharing`
services plus **NMEA-0183** (`GPRMC/GPGGA/…` + Apple `OHPR`/`PAACD`/`PAGCD`/`PASCD`) sensor forwarding. This
is **input to** Apple Maps, not turn info **from** it. Do not put VDC in the Metadata (display) window — it
belongs to a GPS-uplink workstream. [E — direction is explicit in docs/carplay/03_SDK_GROUND_TRUTH.md §6]

**(iii) AirPlay `/command`** carries only `changeMapZoomLevel` (`{zoomDirection}`, accessory→iOS) and the
`turns` audio mode — no route text. [E — docs/carplay/03_SDK_GROUND_TRUTH.md §6/§9]

### 1.4 (d) Telephony / CallState metadata = iAP2, iOS → accessory [E]
| msgId | name | source |
|---|---|---|
| `0x4154` | StartCallStateUpdates | Accessory (subscribe, 12 params) |
| `0x4155` | CallStateUpdate | Device |
| `0x4156` | StopCallStateUpdates | Accessory |

`0x4155` params [E — `spec.rs:1018` `call_state`]: RemoteID(0), DisplayName(1, utf8), Status(2, enum),
Direction(3, enum), CallUUID(4), AddressBookID(6), Label(7), Service(8, enum), IsConferenced(9),
ConferenceGroup(10), DisconnectReason(11), StartTimestamp(12, secs64). Stock subscribe = flat attrs 0-4;
idle wire `41 55 00 05 00 02 00` → Status(id2,u8)=0. [E — `WIRED_METADATA_PLANE.md` L105-118,131]

Declare `0x4155` receivable + subscribe `0x4154`. Phonebook / call-history (List-Updates `0x4170`) are
**not** in the stock declared set and are likely BT-consent gated — deferred. [E — `WIRED_METADATA_PLANE.md`
L138-140]

### 1.5 (e) Everything else
| Category | Direction | Transport | Fields (evidenced) |
|---|---|---|---|
| **MediaLibrary** | iOS→acc | iAP2 `0x4C00-0x4C09` [E `spec.rs:109`] | library info/updates + `PlayMediaLibrary*` (accessory can request playback). Not captured wired; low value. |
| **Night mode** | iOS→acc | AirPlay `/command setNightMode` **and** `/info nightMode` [E] | day/night. `/info nightMode` is the static declaration; `setNightMode` is the live Class-B update. |
| **UI / map appearance** | iOS→acc | AirPlay `uiAppearanceUpdate` / `mapAppearanceUpdate` [E] | `appearanceMode` + `appearanceSetting`. |
| **limitedUI** | iOS→acc | AirPlay `/info limitedUI`/`limitedUIElements` + `/command setLimitedUI` [E] | element-token set the head unit restricts. |
| **Vehicle information** | acc→iOS | AirPlay `/command updateVehicleInformation` (outbound) + `/info vehicleInformation`; live telemetry over VDC | `vehicleInformation` blob [E]. |
| **oemIcon negotiation** | acc↔iOS | AirPlay `/info` keys `oemIcon`/`oemIcons`/`oemIconLabel`/`oemIconVisible`/`initialIconAppearance` [E] | OEM branding icon; static in `/info`, visibility toggled live. |
| **softwareVersion / identity** | acc→iOS | AirPlay `/info` (`sourceVersion`, `firmwareRevision`, `OSInfo`, `model`) + iAP2 identify | Class-A (reconnect to change) per docs/carplay/03_SDK_GROUND_TRUTH.md §3. |
| **Device language / time** | iOS→acc | iAP2 `DeviceLanguageUpdate 0x4E0A`, `DeviceTimeUpdate 0x4E0B` [E `spec.rs`] | locale/clock; in the stock declared recv set. |

---

### Q1 SUMMARY TABLE — category → transport → available on our stack today

| Metadata category | Direction | Transport (id / endpoint) | On our stack today? |
|---|---|---|---|
| Inbound `/command` UI state (modes/duck/nightMode/appearance/limitedUI/focus/haptic…) | iOS→acc | AirPlay `POST /command` | **YES (display)** — `session.rs` logs + forwards raw plist to host `:9004`; not acted on |
| NowPlaying text (title/artist/album/duration/playback/elapsed) | iOS→acc | iAP2 `0x5000`/`0x5001` | **YES** — declared + subscribed from `features::active()`; parsed by `metadata::dispatch` and forwarded to the host `:9004` seam |
| Album artwork | iOS→acc | iAP2 File Transfer (session 2, ref by NowPlaying attr 0x1a) | **YES** — `metadata::Artwork` reassembles the session-2 transfer inside `iap2d`'s link loop |
| Route guidance / turn-by-turn | iOS→acc | iAP2 `0x5200`/`0x5201`/`0x5202` (+ id30 component gate) | **YES** — same generated declare/subscribe plane |
| CallState | iOS→acc | iAP2 `0x4154`/`0x4155` | **YES** — declared + subscribed at the default *proven* tier (`features.rs` `call_state`) |
| MediaLibrary | iOS→acc | iAP2 `0x4C00-09` | **TIER-GATED** — in the generated table as `media_library` at the `capability` tier (`features.rs`: start 0x4C03, stop 0x4C05, also 0x4C00/0x4C02, updates 0x4C01/0x4C04); NOT declared or subscribed at the default *proven* tier, so nothing arrives unless the app pushes `metadata.tier: all` |
| GPS / vehicle telemetry (nav INPUT) | **acc→iOS** | VDC `Navigation` + NMEA | NO — separate uplink workstream, not a display feed |
| Night mode / appearance / limitedUI / oemIcon / vehicleInfo | iOS→acc / acc→iOS | AirPlay `/command` + `/info` | **PARTIAL** — inbound `/command` variants are logged/forwarded; the outbound `setNightMode` / `uiAppearanceUpdate` / `mapAppearanceUpdate` / `setLimitedUI` senders are host-driven |

*(An earlier version of this table answered "NO — iap2d is auth+identify only" for NowPlaying, nav
and call. That was true when written and is now false: `iap2d` arms the app-pushed metadata policy and,
once Identified, fires every `Start*Updates` in the active tier from the same generated table params
6/7 come from. The rows above are the rewritten, current answers.)*

---

## QUESTION 2 — HID CONTROL SEMANTICS

### 2.0 Transport recap [E]
Every HID input report is accessory→iOS: `AirPlayReceiverSessionSendHIDReport(session, uuid, report, len)`
→ AirPlay `/command {type:"hidSendReport", uuid:<hex>, hidReport:<bytes>}` on the encrypted event channel
(`events.rs:231`). Over iAP2 the same is `AccessoryHIDReport 0x6802` after `StartHID 0x6800`
(`spec.rs:215-226`). Each report is addressed by the **UID** the accessory declared in `/info hidDevices[]`;
there is **no report-ID prefix** — devices are disambiguated purely by `uuid`. Coordinates (touch/touchpad)
are **absolute 16-bit in [0, LogicalMaximum]**, LogicalMax = advertised display resolution, written into
the descriptor at build time. [E — docs/carplay/03_SDK_GROUND_TRUTH.md §8, `nm` symbols]

### 2.1 [NEW] The descriptor templates are STATIC const bytes — documented byte-exact
docs/carplay/03_SDK_GROUND_TRUTH.md §8 and the ios27 inventory (11_hid_input.md O1) said knob/touchpad/telephony descriptor bytes were
"runtime-built, not in const." **That is wrong for the button devices** — every non-touchscreen builder
`memcpy`s a fixed const blob out of `__TEXT __const` and returns a constant length. Only the touchscreen
builders set X/Y LogicalMax at runtime. Documented from `CarPlaySDK` (`otool -tV` gave each
`*CreateDescriptor`'s length + const address; bytes read from file offset). **[E — const bytes below]**

#### Report layouts (verified from the extracted descriptor bytes)

| Device | `nm` symbol | descriptor len | report len | layout |
|---|---|---|---|---|
| **TouchScreen single** | `HIDTouchScreenSingleCreateDescriptor` | runtime | **5 B** | `[tip:1b + 7b pad][X:u16 LE][Y:u16 LE]`, X/Y abs in [0,res] |
| TouchScreen single+cancel | `…SingleWithCancel…` | runtime | 5 B | tip bit + cancel bit in byte 0 |
| TouchScreen multi | `…MultiCreateDescriptor` | runtime | 12 B | 2× `[id][tip][X][Y]` per docs/carplay/03_SDK_GROUND_TRUTH.md §8 |
| **MediaButtons** | `HIDMediaButtonsCreateDescriptor` | 0x28 | **1 B** | Consumer array index (see below) |
| **TelephonyButtons** | `HIDTelephonyCreateDescriptor` | 0x39 | **1 B** | Telephony array index (see below) |
| **DPad** | `HIDDPadCreateDescriptor` | 0x27 | **2 B** | Consumer Var bitfields (see below) |
| **Knob minimal** | `HIDKnobMinimalCreateDescriptor` | 0x27 | **2 B** | `[Select:bit0 | 7b pad][Wheel:i8 rel]` |
| **Knob basic** | `HIDKnobBasicCreateDescriptor` | 0x33 | **2 B** | `[Select:b0, Home:b1, Back:b2 | 5b pad][Wheel:i8 rel]` |
| **Knob full** | `HIDKnobCreateDescriptor` | 0x46 | **4 B** | `[Select/Home/Back bits][NudgeX:i8][NudgeY:i8][Wheel:i8 rel]` |
| **TouchpadButtons** | `HIDTouchpadButtonsCreateDescriptor` | 0x25 | **1 B** | `[Select:b0, Back:b1, Home:b2 | 5b pad]` |
| Touchpad | `HIDTouchpadOnlyCreateDescriptor` | 0x50 | var | abs X/Y scaled to `touchpadWidth×Height` + scroll deltas |
| Touchpad multichar | `HIDTouchpadMultiCharacterCreateDescriptor` | 0xA0 | var | pointer + character entry |
| **SteeringWheel** | `HIDSteeringWheelCreateDescriptor` | 0x5c | **3 B** | `[Select/0x0232/Back bits][Menu 0x40-0x45: 6 bits][Wheel:i8 rel]` |
| Proximity | `HIDProximityCreateDescriptor` | 0x16 | 1 B | Sensor page presence bit |

**[NEW] This corrects docs/carplay/03_SDK_GROUND_TRUTH.md §8's knob guess** (`[buttons][X][Y][rot rel]`, 4 B): the real knob is
`[buttons][wheel:i8]` (minimal/basic, 2 B) — no X/Y — and the "full" variant adds nudge as X/Y i8 axes, so
the correct full report is `[buttons][nudgeX:i8][nudgeY:i8][wheel:i8]`. It also confirms the sibling
`hid.rs` "FLAGGED CORRECTED LAYOUT" comment.

#### Const descriptor bytes (from the SDK) [E]
```
MediaButtons (0x28): 05 0c 09 01 a1 01 15 00 25 06 05 0c 0a 00 00 0a b0 00 0a b1 00
                     0a cd 00 0a b5 00 0a b6 00 0a 9e 02 75 08 95 01 81 00 c0
  Consumer array, LogicalMax=6, Input(Array). Usage list (report byte = index into it):
    0=0x0000(none/release) 1=0x00B0 Play 2=0x00B1 Pause 3=0x00CD Play/Pause
    4=0x00B5 ScanNext 5=0x00B6 ScanPrev  6=0x029E [E present; semantic [I], Apple-specific]
  NOTE: sibling/ncm port declares LogicalMax=5 (6 usages) — the real SDK has a 7th usage (index 6). [NEW]

Telephony (0x39): 05 0b 09 07 a1 01 15 00 25 11 05 0b 09 00 09 20 09 21 09 26 09 2f
                  09 b0 09 b1 09 b2 09 b3 09 b4 09 b5 09 b6 09 b7 09 b8 09 b9 09 ba 09 bb
                  05 07 09 2a 75 08 95 01 81 00 c0
  Telephony-page(0x0B) array, LogicalMax=0x11(17), Input(Array). Usage list:
    0=none 1=0x20 2=0x21 3=0x26 4=0x2F 5..16=0xB0..0xBB 17=Keyboard 0x2A
    [I semantics] 0x20≈Hook Switch(answer/offhook), 0x2F≈Phone Mute, 0xB0-0xBB=phone keypad 0-9/*/#
    (DTMF), 0x2A(kbd)=Backspace/Delete. accept/end-call ride the Hook-Switch + call-control usages.

DPad (0x27): 05 0c 09 01 a1 01 15 00 25 01 75 01 0a 23 02 0a 24 02 95 02 81 02
             95 06 81 01 19 40 29 45 95 06 81 02 95 02 81 01 c0
  Consumer Var bitfields (NOT an array). Report 2 B:
    byte0: bit0=AC Home(0x0223) bit1=AC Back(0x0224) bits2-7=pad
    byte1: bits0-5=Consumer 0x40..0x45 (Menu/Menu Pick/Menu Up/Down/Left/Right) bits6-7=pad

KnobBasic (0x33): 05 01 09 08 a1 01 05 09 09 01 15 00 25 01 75 01 95 01 81 02
                  05 0c 0a 23 02 0a 24 02 95 02 81 02 95 05 81 01
                  05 01 09 38 15 81 25 7f 75 08 95 01 81 06 c0
  Report 2 B: byte0 = [Button1 Select:b0][AC Home:b1][AC Back:b2][5b pad];
              byte1 = Wheel(0x38) i8 relative (−127..127), Input(Data,Var,Rel).

SteeringWheel (0x5c): 05 01 09 08 a1 01 05 09 09 01 15 00 25 01 75 01 95 01 81 02
                      05 0c 0a 32 02 0a 24 02 75 01 95 02 81 02 95 05 81 01
                      05 0c 09 01 a1 01 0a 40 00 0a 41 00 0a 42 00 0a 43 00 0a 44 00 0a 45 00
                      15 00 25 01 75 01 95 06 81 02 95 02 81 03 c0
                      05 01 09 38 15 81 25 7f 75 08 95 01 81 06 c0
  Report 3 B: byte0=[Button1:b0][Consumer 0x0232:b1][AC Back:b2][5b pad];
              byte1=Menu 0x40-0x45 (6 bits); byte2=Wheel i8 rel.

TouchpadButtons (0x25): 05 0c 09 01 a1 01 05 09 09 01 15 00 25 01 75 01 95 01 81 02
                        05 0c 0a 24 02 0a 23 02 95 02 81 02 95 05 81 01 c0
  Report 1 B: bit0=Button1(Select) bit1=AC Back(0x0224) bit2=AC Home(0x0223) bits3-7=pad.
```

### 2.2 [NEW] Where is Home / Back? — they are **Consumer usages inside the knob/dpad/wheel report**, not a device
There is **no dedicated Home or Back HID device** and **no Consumer 0xCF (Voice Command)** anywhere in the
roster. Home = **Consumer AC Home 0x0223**, Back = **Consumer AC Back 0x0224**, present as *bits* in the
DPad, Knob-basic/full, SteeringWheel, and TouchpadButtons reports. The `HIDConfig` toggles
`knobSupportsHomeAndBackButton` / `backButtonSupport` / `notificationButton` select whether those bits are
present in the emitted descriptor variant (minimal knob omits them; basic/full include them). So "press
Home" = set bit1 of the DPad byte0 (or bit1 of the knob byte0), then clear it. [E — extracted descriptor
bytes; docs/carplay/03_SDK_GROUND_TRUTH.md §2 HIDConfig keys]

### 2.3 PRESS SEMANTICS — tap vs hold, per device
The wire model is uniform: **a report describes the current instantaneous state.** iOS reacts to the
transition. Two idioms [E — `HIDMediaButtonsFillReport` shape + sibling `hid.rs` + live ccpa `main.rs`]:

- **Single-press / "tap"** (array-index devices: MediaButtons, TelephonyButtons): send the press report
  `[index]` **immediately followed by** the release report `[0]` (index 0 = the descriptor's unassigned
  usage). This is exactly what ccpa does: the `INPUT_MEDIA_BTN` arm of airplayd's `handle_input_frame`
  (~`main.rs:907-918`) sends `hid::media_button_report(index)` then
  `hid::media_button_report(hid::media_button::NONE)`. `HIDMediaButtonsFillReport` implies release because
  the report is an Array whose value 0 = "no usage asserted" — you MUST send a following 0 or the button
  stays logically held. [E — `hid::media_button_report`, airplayd `handle_input_frame`'s `INPUT_MEDIA_BTN`
  arm; anchors corrected 2026-08-16]
- **Press-and-hold** (Var-bitfield devices: DPad, Knob buttons, SteeringWheel, TouchpadButtons, and touch):
  emit the **down** report (bit set / tip=1) on mouse-down and the **up** report (bit cleared / tip=0) on
  mouse-up. Touch already does this: airplayd `handle_input_frame` (`main.rs:882`, touch arm ~`:1160-1197`)
  DOWN/MOVE → tip=1, UP → tip=0, and `hid::touch_report(0, …)` keeps coords but clears the contact bit on
  release. For a Controls window button, a click = down-then-up back to back (a tap); a held button = down
  on press, up on release. [E — `hid::touch_report` / `hid::touch_report_multi`, airplayd
  `handle_input_frame`; the old `hid.rs:172-180` anchor and its `touch_report_len_and_release` test never
  existed in this file — corrected 2026-08-16]
- **Knob rotation** is a **relative** i8 delta (`+CW/−CCW`), sent per detent; there is no "release" — send
  the delta then optionally a 0. [E — Wheel `81 06` Input(Rel)]

### 2.4 Siri — `/command`, a HOLD-shaped action; no HID path [E]
**Siri is triggered over AirPlay `/command requestSiri`, NOT via HID.** There is no Consumer 0xCF Voice
Command usage in any descriptor. [E — full roster scanned]

`requestSiri` payload params [E — `sdk_strings.txt:9273-9278`]: `siriAction`, `siriTriggerTimestamp`,
`siriTriggerZone`. Internal fn: `_AirPlayReceiverSessionRequestSiriActionInternal(session,
AirPlaySiriAction, uint32_t*, uint32_t*, uint32_t*, completion, ctx)` — three u32 params. Log format:
`RequestSiri %s %llu ms latency %lu ms sample %lu zone %lx` → the u32s are (trigger timestamp / sample
index / zone). [E]

**`AirPlaySiriAction` enum values** (four adjacent strings right after the fn signature — these ARE the
enum's string forms) [E — `sdk_strings.txt:9279-9282`]:

| value | meaning [E name / I semantic] |
|---|---|
| `prewarm` | pre-warm the recognizer before the user commits (reduce latency) |
| `buttondown` | Siri button pressed **down** — begin hold |
| `buttonup` | Siri button **released** — end hold / commit |
| `voiceactivation` | "Hey Siri" voice trigger (no button) |

**⇒ Siri via the button IS a hold.** A press-and-hold Siri button maps to `requestSiri{siriAction:
buttondown}` on mouse-down and `requestSiri{siriAction: buttonup}` on mouse-up (optionally `prewarm` on
hover/first-touch). `voiceactivation` is the separate always-listening path. This **corrects the current
ccpa/ncm implementation**, which sends a single bare `{type:"requestSiri"}` with no `siriAction` and a
comment "a single command, not a press/hold pair" (`events.rs:216-228`) — that shape was never
wire-captured and, per the enum, is under-specified: the Controls window should send the
buttondown/buttonup pair with `siriAction`. [E — enum vs current code]

### 2.4b Siri hardware-button — full mechanism [E]
Full end-to-end trace from the CarPlaySDK binary (`_AirPlayReceiverSessionRequestSiriActionInternal`
0x271e44 + wrappers) and the CarPlaySimulator app's own Siri-button handler. This **supersedes §2.4's
open items** and **corrects our current impl**: the wire carries `siriAction` as an **integer enum**, not a
string. All addresses/bytes below are from the two local binaries (read-only).

#### (1) No HID path — Siri is `/command requestSiri` only [E]
Scanned every descriptor builder (`nm -gU` roster: `HID{MediaButtons,Telephony,DPad,Knob*,SteeringWheel,
TouchScreen*,Touchpad*,Proximity}CreateDescriptor`) — **none contains a Consumer 0x0CF "Voice Command",
Telephony voice, or GenericDesktop voice usage**. A search of the SDK's descriptor tables for `0a cf 00` (Consumer Usage 0x00CF,
2-byte) = **0 hits**; the extracted descriptor bytes (§2.1) confirm no voice usage in any emitted report. AC
Search 0x0221 and the `09 cf` byte pairs that appear are incidental const bytes, not in any HID descriptor
builder's blob. **⇒ Siri has no HID trigger. The only path is AirPlay `POST /command {type:"requestSiri"}`.**
[E — descriptor roster + byte scan]

#### (2) Exact wire params — `siriAction` is an INTEGER, timestamp/zone optional [E]
`_AirPlayReceiverSessionRequestSiriActionInternal(session, AirPlaySiriAction action, uint32_t *arg2,
uint32_t *arg3, uint32_t *arg4, completion, ctx)` builds the command dict (disasm 0x271e44‑0x2720b0,
cfstring targets resolved through the chained-fixup low-36-bits):
- outer `{ "type": "requestSiri" }` — key = `kAirPlayKey_Type`, value CFString `"requestSiri"` (@0x3ecfd0). [E]
- params dict built with **`CFDictionarySetInt64`** (NOT SetValue/string):
  - `"siriAction"` (@0x3ecff0) ← `sxtw action` — **always set, as an integer**. [E — `CFDictionarySetInt64` @0x271ecc]
  - `"siriTriggerTimestamp"` (@0x3ed010) ← set **only if `arg2 != NULL`** (`orr x8,x26,x24; cbz` gate @0x271ed0; SetInt64 @0x27207c). u32/latency-derived. **Optional.** [E]
  - `"siriTriggerZone"` (@0x3ed030) ← set **only if `action == 4`** (`cmp w23,#0x4; b.ne` @0x272080; SetInt64 @0x2720a0) — i.e. **voiceactivation-only**. **Optional / irrelevant to the button.** [E]
- `outer["params"] = params`; then `dispatch_sync_f` onto the session queue → sends the `/command`. [E]

Log fmt confirms the three trailing u32s are advisory telemetry: `RequestSiri %s %llu ms latency %lu ms
sample %lu zone %lx` (the `%s` is a **log-only** name; the wire value is the integer). [E]

**`AirPlaySiriAction` numeric enum values [E]** — from the log-name pointer table at file-off 0x3a83a8
(indices 0/1/2) plus the `voiceactivation` special-case (`cmp #0x4; csel eq` @0x272108) and the
VoiceActivation wrappers (`mov w1,#0x4`):

| value | name | how sent |
|---|---|---|
| **0** | `NotApplicable` (n/a) | not a real trigger action (corrected 2026-08-01: was `prewarm`) |
| **1** | `prewarm` | pre-warm recognizer (optional, on hover/first-touch) (corrected 2026-08-01: was `buttondown`) |
| **2** | `buttondown` | Siri button pressed **down** (corrected 2026-08-01: was `buttonup`) |
| **3** | `buttonup` | Siri button **released** (corrected 2026-08-01: previously absent from this table) |
| **4** | `voiceactivation` | "Hey Siri" (uses `siriTriggerZone`/sample; not the button) |

(corrected 2026-08-01: this note previously claimed "3 is unused — indices 0‑2 only"; that was the same
off-by-one misreading. Index **3 = `buttonup`** per R14G17 `AirPlayCommon.h:1366-1369`.) [E]

#### (3) Public wrappers prove timestamp/zone are NULL for the button [E]
- `RequestSiriAction(session, action, completion, ctx)` @0xb6e0 → `Internal(session, action, **NULL, NULL,
  NULL**, completion, ctx)` — the plain button call passes all three u32* as NULL, so it emits
  `{type:"requestSiri", params:{siriAction:<int>}}` and **nothing else**. [E]
- `RequestSiriActionWithLatency(session, action, latencyMs, …)` @0x2721ec → `arg2 = &latencyMs`, arg3/arg4
  NULL ⇒ adds `siriTriggerTimestamp`. [E]
- `RequestSiriVoiceActivationWith{Latency,Sample}` @0x272224/0x272258 → `w1 = action = 4`. [E]

#### (4) AcquireFocus is INDEPENDENT — not a prerequisite [E]
`_AirPlayReceiverSessionAccessoryAcquireFocus` @0x272448 is a **separate command builder**: it makes its own
dict `{type:"accessoryAcquireFocus"}` (CFString @0x3ec810) and `dispatch_sync_f`s it. Nothing in the Siri
path references it, and the simulator's Siri handler (below) never calls it before RequestSiri.
`HasFeatureFocusTransfer` (@0x80a0, reads bool at ctx+0x159) gates focus-transfer, not Siri. **⇒ You do NOT
need AcquireFocus for the Siri button to register.** [E]

#### (5) Mic uplink is NOT required for the button — Classic vs Enhanced Siri [E]
CarPlaySimulator's dispatcher (disasm 0x11940‑0x11cc4) reads `AirPlaySiriAction.rawValue` (Int32) and calls,
on **both** branches, `RequestSiriAction(session, rawValue, NULL, NULL)` (@0x11c5c) or
`RequestSiriActionWithLatency(session, rawValue, 0, NULL, NULL)` (@0x11cc0) — **with NO AuxIn/mic setup and
NO AcquireFocus** on the button path. The mic path is a **separate feature**:
- **Classic Siri** = just `RequestSiriAction` with the enum int. iOS brings up Siri using the **phone's own
  mic**. This is what the hardware button does. [E — "Requesting Classic Siri Action for %d" @0x11bac]
- **Enhanced Siri** = accessory streams car-mic audio to iOS over an **AuxIn** uplink
  (`_AirPlayReceiverSessionAuxInStart` @stub, logs "Starting AuxIn for Enhanced Siri", "Started AuxIn …
  setting zllBuffered to true"), gated on `_AirPlayReceiverSessionHasFeatureEnhancedSiri` (@0x7fbc, bool at
  ctx+0x136) which is negotiated via `/info enhancedSiri`/`enhancedSiriInfo` +
  `setEnhancedSiriParams`(`bufferSizeMs`/`bufferAudioFormat`/`burstPeriodMs`/`voiceModelLanguage`). "Handling
  audio setup, but Enhanced Siri not supported for this config" is the no-feature branch. [E]

  > **EXPANDED 2026-08-02 — "streams car-mic audio" undersells the obligation by a wide margin.**
  > `wwdc2019-252.txt:86-134` is the architecture session (WWDC 2023-10150 explicitly redirects to it:
  > *"See 'Advances in CarPlay Systems' for a detailed look"*). Enhanced Siri is a **two-stage detector
  > with the first stage in the car**, and the accessory owns a real DSP pipeline:
  >
  > 1. **Always-on mic + continuous voice processing** (`:97`). Not on-demand capture.
  > 2. **ECNR** — echo canceller + noise reduction to clean the input (`:130`). The echo reference is the
  >    car's own speaker output, *including the CarPlay audio we are playing*.
  > 3. **An audio ring buffer inside the Communication Plug-in holding "a couple of seconds of historical
  >    audio," stored in the car** until a trigger sends it (`:99-100`).
  > 4. **TWO detectors, both mandatory** (`:101-107`): a **keyword detector** (driver says "Siri") and a
  >    **voice activity detector** (driver starts talking). *"Both detectors must be available in the car
  >    as iPhone determines which one is used for a particular scenario."* We do not choose.
  > 5. On trigger the car notifies iOS and ships the buffer; **iOS re-analyses it with its own second-pass
  >    voice-trigger detector and only then activates Siri** (`:108-112`). This is what
  >    `kAFErrorSpeechAbortedFalseVoiceTrigger` is — the phone rejecting our first-pass hit — and why
  >    `siriTriggerTimestamp` exists: the phone needs to know *when* to re-analyse from within the
  >    shipped buffer. (Consistent with §6 below: the plain button call passes NULL for it.)
  >
  > **The same ring buffer serves the button path** (`:113-117`): a press makes iOS *"request audio data
  > from the time when the user pressed the button,"* and *"the buffered audio is sent faster than
  > real-time"* — the uplink twin of `mainBuffered`. That is what makes button-Siri feel instant, and it
  > is an Enhanced-Siri benefit, not a Classic one.
  >
  > **AuxOut is the other half of the pair** (`:124-126`): when Siri launches, *"an additional audio
  > stream dedicated to Siri output, **Aux Out**, will be opened"*; the car mixes it with music, ducks the
  > music, and must handle **three parallel streams** — media, Siri prompts, and route guidance. So
  > **AuxOut 106 = Siri downlink, AuxIn 107 = car-mic uplink**, which is why
  > `docs/carplay/03_SDK_GROUND_TRUTH.md` §7 groups them as `GeneralAudio`.
  >
  > **Cost assessment:** this is a DSP project, not a protocol one — always-on capture, ECNR with a live
  > echo reference, two detectors, a historical ring buffer, and a three-way mixer. Advertising
  > `enhancedSiri` obligates all of it. The Classic button path needs **none** of it (§5 above).

**⇒ Pressing the Siri button WILL bring up Siri with no mic uplink configured** (Classic path) — provided the
`siriAction` value is the correct **integer**. Enhanced Siri (accessory mic) is an add-on, not a
precondition. [E — simulator dispatcher structure; iOS honoring is [I]]

#### (6) Button-press → action mapping (pressed bool) [E]
`VideoStreamView.handleKeyEvent(...)` (@0xd8600) maps the Siri key: `tst w27,#0x1; mov w8,#1; cinc w23,w8,eq`
(@0xd95d4) ⇒ **pressed=true → siriAction = 2 (buttondown)**, **pressed=false → 3 (buttonup)** (corrected
2026-08-01: this section's raw reading was 1/2 and was the source of the off-by-one; the wire values are
2/3 per `AirPlayCommon.h:1366-1369` and device logs `cp.log:1802-1805`). Logged
`handleKeyEvent siriAction: %d keyCode: %hu pressed: %{bool}d`. So the button is a **press-to-talk HOLD**:
down=2 on key-down, up=3 on key-up. [E]

#### (6b) `/info` capability — enhancedSiri gates the MIC, not the button [E]
`enhancedSiri` / `enhancedSiriInfo` in `/info` (+ `HasFeatureEnhancedSiri` flag) only unlock the **AuxIn
accessory-mic uplink** (§5). The plain `requestSiri` command builder has **no feature gate** in its
disassembly. So the accessory does **not** need to declare a Siri capability for the Classic button to send;
it only needs the encrypted event channel up. (Whether a given iOS build silently ignores requestSiri from an
undeclared accessory is [I] — untested — but the SDK send-path imposes no such requirement.) [E send-path / I iOS-side]

#### VERDICT — is our buttondown/buttonup `requestSiri` correct + complete?
**NO — one concrete bug: `siriAction` must be an INTEGER, not a string.** Our
`events.rs:send_request_siri_action(action:&str)` sends `params:{siriAction:"buttondown"}` (a CFString);
airplayd `main.rs:507‑508` wires `CMD_SIRI_DOWN→"buttondown"`, `CMD_SIRI_UP→"buttonup"`. The SDK ground truth
(`CFDictionarySetInt64` + simulator passing `rawValue:Int32`) is that the wire value is an **integer enum**:
- press → `params:{siriAction: 2}` (buttondown) (corrected 2026-08-01: was `siriAction: 1`)
- release → `params:{siriAction: 3}` (buttonup) (corrected 2026-08-01: was `siriAction: 2`)
- (optional pre-warm on first touch → `1`; `voiceactivation` = `4` is a different, Hey-Siri path) (corrected 2026-08-01: prewarm was `0`)

Everything else in our approach is **correct/complete**:
- Envelope `{type:"requestSiri", params:{…}}` ✓ (matches the resolved cfstrings). [E]
- Omitting `siriTriggerTimestamp`/`siriTriggerZone` ✓ — the plain button call passes NULL for both; zone is
  voiceactivation-only. [E]
- Down/up **HOLD** semantics ✓ — matches `cinc` pressed→1/2. [E]
- **No** `accessoryAcquireFocus` needed ✓. [E]
- **No** mic/AuxIn/`enhancedSiri` needed for the button to bring up Siri ✓ (that's Enhanced Siri only). [E]

**Fix = change the value type from string to int** — **2 on press / 3 on release, NOT 1/2** (corrected
2026-07-31: 1 is prewarm, so 1/2 was off by one; the values eight lines above are the right ones).
**LANDED 2026-07-30** — `events.rs:1023` now takes `action: i64` and `airplayd/src/main.rs:985-986`
passes 2 and 3. The bare `{type:"requestSiri"}` shape
(`send_request_siri`, `CMD_REQUEST_SIRI`) is under-specified (no `siriAction`) and already validated-negative
— keep only the integer down/up pair. The prior string-valued attempt was never SDK-conformant; the string
never matches the integer enum iOS expects, which is the most likely reason it "dispatches but iOS doesn't
react."

### 2.5 Telephony device [E]
`HIDTelephonyCreateDescriptor` (bytes §2.1): a 1-byte Telephony-page (0x0B) **array** report, LogicalMax 17,
same trivial idiom as MediaButtons. Usages include Hook Switch (0x20, answer/offhook), Phone Mute (0x2F),
and the phone keypad 0xB0-0xBB (DTMF 0-9/*/#). **Accept call** = emit the Hook-Switch/answer index then
release [0]; **end call** = the on-hook/reject index (tap). Note the **cross-plane split** (ios27 inventory
O3): the telephony *keypad/hook* is this AirPlay HID device, but call *state* (who's calling, status) rides
**iAP2 CallStateUpdate 0x4155** (§1.4). A Controls window "answer/hangup" button can drive the HID device;
the "who is calling" label comes from the iAP2 feed. [E]

### 2.6 The reconnect INCIDENT — safe descriptor set today [E]
**INCIDENT 2026-07-06** (`sibling info.rs` module doc): adding a **third** `hidDevices[]` entry
(`knob_descriptor`, UID 3) to the every-session `/info` capability response caused **both** the AirPlay and
the separate iAP2 session to stop reconnecting, unrecoverable even by USB replug. A 6-agent investigation
cleared the descriptor bytes themselves (HID 1.11 valid) and every other file; `hidDevices` growing to a
never-before-seen third entry in the unconditional `/info` was the only remaining cause. Reverted to **two**
devices. The knob code (`knob_descriptor`/`HID_UID_KNOB`/`hid::knob`) is kept but **not advertised**. [E —
`info.rs:8-18` module doc]

**⇒ SUPERSEDED 2026-08-16 — the descriptor set today is FIVE uids, two unconditional and three app-gated**
(`info.rs` `build_info`'s `hids` vec, ~`:766-811`): touchscreen **uid 1** and media buttons **uid 2** always;
**uid 3 D-Pad**, **uid 4 Knob** and **uid 5 Telephony** each emitted only when the app-pushed `hidConfig`
arms the matching lever (`airplayd/src/main.rs:682/684/686` → `events::set_dpad_advertised` /
`set_knob_advertised` / `set_telephony_advertised`). Host defaults: `dPadSupport` **true**, `knobSupport`
and `telephonyButtonsSupport` **false**. The 2026-07-06 incident no longer gates advertising — a
five-entry `hidDevices[]` has run on hardware. The original two-device conclusion follows.

**⇒ (ORIGINAL, superseded) SAFE descriptor set to advertise today = exactly two devices:** touchscreen **UID 1** + media buttons
**UID 2** (`info.rs:196-213`, both bound to `DISPLAY_UUID`). The Controls window can safely drive **touch
(uid 1)** and **media buttons (uid 2)** plus the AirPlay `/command` actions (`requestUI`, `requestSiri`,
`changeMapZoomLevel`, focus, etc.) which need no new `hidDevices[]` entry. Adding knob/telephony/dpad HID
devices is **gated on resolving the incident** (isolate whether it was the third-entry count, a specific
descriptor, or an unrelated race) before re-advertising. [E] Per docs/carplay/04_CAPABILITIES_AND_CONFIG.md this two-device bound is an
evidence-backed *constraint on the app-pushed `hidConfig`* — the app doesn't push a third device
until the incident is resolved — not a box-owned constant.

---

### Q2 SUMMARY TABLE — control → transport → available on our stack today

| Control | Transport | Report / payload | On our stack today? |
|---|---|---|---|
| Touch (tap / drag) | HID `hidSendReport` uid 1 | 5 B `[tip][X u16 LE][Y u16 LE]` abs; tip=1 down / 0 up | **YES** — `hid.rs` + ccpa `main.rs` ingest `:9110` |
| Media buttons (play/pause/next/prev) | HID `hidSendReport` uid 2 | 1 B Consumer array index; tap = `[i]` then `[0]` | **YES** — ccpa `main.rs:459-467` |
| Home / Back | HID uid 3 D-Pad, byte0 bit0 AC Home / bit1 AC Back | 2 B Var bitfield, press then all-zero release | **WIRED** — `AppDelegate.swift:746-749` → `airplayd:914-915`; D-Pad advertised by default. `requestUI` is NOT Home and is no longer sent by the host |
| Siri | AirPlay `/command requestSiri` | `{siriAction:<int>}` — **2** on press, **3** on release (HOLD); timestamp/zone omitted | **WIRED** — `ControlsWindow.swift:102-127` → `airplayd:985-986` → `events.rs:1023`. Bare `{type:requestSiri}` retained deprecated, A/B only |
| Map zoom | AirPlay `/command changeMapZoomLevel` | `{uuid:<ALT_DISPLAY_UUID>, zoomDirection: 0 in / 1 out}` | **WIRED (cluster only)** — `VideoChromeOverlay.swift:334-337` → `events.rs:704`; dropped unless the alt screen is advertised (`airplayd:1012-1029`) |
| Knob (rotate / select / nudge) | HID **uid 4** | 4 B `[flags][nudge_x i8][nudge_y i8][rotation i8]`, press then all-zero release | **WIRED, arming-gated** — `ControlsWindow.swift:635-681` → `airplayd:936-948`; needs app-pushed `hidConfig.knobSupport` (default off) |
| D-Pad | HID **uid 3** (Apple's `HIDDPadCreateDescriptor`) | 2 B Var bitfield (Home/Back/Select/Up/Down/Left/Right) | **WIRED** — `AppDelegate.swift:750-754` → `airplayd:913-927`; advertised by default (`dPadSupport` defaults true) |
| Steering wheel / touchpad | HID (2-3 B Var bitfields) | — | **ABSENT** — no descriptor entry, no OCBM opcode; `steeringWheelSupport` is **parse-only in our code** — nothing consumes it (`vehicle_config.rs`, under the "Nothing consumes these yet, deliberately" block). *(Corrected 2026-08-16 — this said it "only sets the display features bit 0x20", which is true of Apple's `HIDConfig.displayFeatures` but false of ours: the word we emit is the constant `if levers::dpad() { 0x1A } else { 0x0A }` in `info.rs`, so `dPadSupport` is its only input.)* |
| Telephony (answer/end/flash/mute/DTMF) | HID **uid 5** + iAP2 `0x4155` for state | 1 B Telephony array index, then `[0]` | **WIRED, arming-gated** — `ControlsWindow.swift:733-771` → `airplayd:952-961`; needs pushed `telephonyButtonsSupport` (default off). **Call state IS declared + subscribed** at the default *proven* tier (`features.rs:440-452`, `iap2d/src/main.rs:604-613`) |
| Appearance / limitedUI outbound | AirPlay `/command` | `uiAppearanceUpdate` / `mapAppearanceUpdate` / `setNightMode` / `setLimitedUI` | **WIRED** — `ControlsWindow.swift:181-259` → `events.rs:899/937/945/978`; alt-display appearance is alt-screen gated |
| Focus / haptic outbound | AirPlay `/command` | `changeModes` / `performHapticFeedback` | **NO host-driven** — take-screen `changeModes` fires automatically at RECORD (`events.rs:732`, `session.rs:1802`); no outbound haptic exists (inbound decode label only) |

---

### Implementation notes for the Metadata & Controls windows

**Metadata window (Q1).** Two data sources, wire them separately:
1. **AirPlay `/command` inbound (available NOW):** `session.rs` already forwards every inbound command
   plist to the host over `:9004` as `[u32 BE len][0x01 META_CMD][plist]`. The window can decode `type` +
   params for modesChanged / duckAudio / setNightMode / appearance / limitedUI / focus and show a "session
   state" panel with **zero new box work**. Start here — it's free.
2. **iAP2 feeds (NowPlaying / RouteGuidance / CallState / artwork): LANDED — corrected 2026-08-16.**
   *(This item originally read as a to-do: "require porting the sibling's solved `iap2d` metadata path —
   flip `declare_wired`, add the `MessagesReceivedFromDevice` declaration (`4e0a 4e0b 4155 5001 5201
   5202`), the `RouteGuidanceDisplayComponent` id-30 identify component, the `Start*Updates` subscribes
   (0x5000/0x4154/0x5200), the TLV param decoder, delta-merge per feed, and the session-2 File-Transfer
   handler for artwork … a known-quantity port, not new RE." Every piece of that list now exists.)*
   The declaration and the subscribe list are GENERATED from `crates/vendor/iap2-core/src/features.rs`
   (docs/carplay/05_METADATA_AND_CONTROLS.md) — never hand-edit one of the two. `iap2d` fires the tier's `Start*Updates` once
   `State::Identified` is reached, decodes the Device→Accessory updates in
   `iap2_core::metadata::dispatch`, reassembles artwork with `metadata::Artwork` off the session-2 File
   Transfer, and emits newline-JSON over the existing `:9004` seam (`metadata.rs` `SEAM_ADDR`) — no new
   TCP seam was added on the fragile management link. `declare_wired` is still `false` and always was
   irrelevant here: it only adds the wired-CarPlay ids `0x4301`/`0x4300`.
   Per docs/carplay/04_CAPABILITIES_AND_CONFIG.md, WHICH tier is declared/subscribed is app-pushed config (`metadata: {tier, skip}`); the
   box implements the declare/subscribe mechanics.
3. **Do not** put VDC/NMEA nav in this window — that's accessory→iOS GPS uplink, opposite direction.

**Controls window (Q2).** 
- **Shipping today (corrected 2026-08-16):** touch (uid 1), media buttons (uid 2) and the D-Pad (uid 3,
  advertised by default) unconditionally; knob (uid 4) and telephony (uid 5) once the app pushes
  `hidConfig.knobSupport` / `telephonyButtonsSupport` and the session reconnects. Plus the `/command`
  surfaces needing no HID entry: `requestSiri`, `changeMapZoomLevel`, `setNightMode`,
  `uiAppearanceUpdate`/`mapAppearanceUpdate`, `setLimitedUI`, `showUI`/`stopUI`. **Home is NOT `requestUI`**
  — it is the uid-3 D-Pad AC-Home bit; the host stopped sending `requestUI` in 2026-07-12.
- **Press semantics:** array-index buttons (media/telephony) = send `[index]` then `[0]` for a tap.
  Var-bitfield buttons (dpad/knob/wheel/touch) = down report on mouse-down, up report on mouse-up (hold if
  held). Knob rotation = relative i8 per detent.
- **Siri — LANDED 2026-07-30, not a to-do.** *(This bullet originally prescribed the STRING form
  `siriAction:"buttondown"`/`"buttonup"` plus timestamp and zone. It was written from §2.4-era research and
  is superseded by §2.4b in this same document — corrected 2026-08-16.)* The box sends
  `{type:"requestSiri", params:{siriAction: 2}}` on press and `{siriAction: 3}` on release.
  `siriAction` is the **integer** `AirPlaySiriAction` enum, never a string: R14G17
  `AirPlayCommon.h:1126` types the key `[Number:AirPlaySiriAction]`, `:1366-1369` gives the values, and
  Apple's own accessory-side sender uses `CFDictionarySetInt64` (`AirPlayReceiverSession.c:5212`).
  (`AirPlaySiriActionFromString()` exists in the header but has **zero call sites** anywhere in the drop —
  it is a logging helper, not a wire parser, so the string form was never accepted on any iOS.)
  `siriTriggerTimestamp`/`siriTriggerZone` are deliberately **omitted** — the plain-button path passes NULL
  for both and the zone is written only when `siriAction == 4`. Live path: `events.rs:1023`,
  `airplayd/src/main.rs:985-986`, host `ControlsWindow.swift:102-127` / `AppDelegate.swift:755-767`.
  **Still not device-proven as a trigger:** the 2026-08-10 hardware confirmation covers Siri *audio* (mic
  uplink), not that our `/command` initiated the session.
- **~~Do NOT advertise a third `hidDevices[]` entry~~ — RETIRED 2026-08-16.** uid 3/4/5 all ship under
  app-pushed `hidConfig`; the 2026-07-06 incident no longer gates them. (Historical text follows.) The
  2026-07-06 reconnect incident broke both sessions unrecoverably. The report layouts and byte-exact descriptor
  bytes for all of them are now in §2.1 ready to use once advertising a 3rd device is proven safe (e.g.
  behind a session-scoped negotiation rather than the unconditional `/info`).

### 2.7 Simulator D-Pad send-path trace [E]

Full end-to-end trace from Apple's `CarPlaySimulator` (SIM) + `CarPlaySDK` (SDK) arm64e binaries.
Every claim cites a symbol/address. **Diagnosis — SUPERSEDED 2026-07-30, see `info.rs:530-563`.** The
`DisplayFeatures` labels below are permuted. The real map is `0x02 Knobs · 0x04 LowFidelityTouch ·
0x08 HighFidelityTouch · 0x10 Touchpad · 0x20 DirectionButtons` — so **`0x10` is Touchpad, not Direction
Buttons**, and `dPadSupport` contributes **nothing** to this word (`0x10` is ORed from `touchpadSupport`,
`0x20` from `steeringWheelSupport`). §2.7.4 misread `[HIDConfig+0x24]` as the D-pad bool; +0x24 is
`touchpadSupport`. The uid-3 entry was always correct; there was no missing bitmask gate. (§2.7.4's own
note at the end of this section already caught half of this.)

#### 2.7.1 Action → fill → transmit chain (SIM)
- `HIDDPadView` button press builds a `DPadInputData` struct (8 bools: up,down,left,right,select,home,
  menu,back — init `DPadInputData.init(upPressed:downPressed:leftPressed:rightPressed:selectPressed:
  homePressed:menuPressed:backPressed:)`) and calls `HIDController.handleDPadInput(_:forVideoStreamID:)`
  (SIM `_$s16CarPlaySimulator13HIDControllerC15handleDPadInput...` @0xafdb8).
- `handleDPadInput` (@0xafdb8):
  - Looks up the videoStreamID in a per-stream dict → bucket has `uid@+0x38` (the dPadID) and a
    `dPadSupported flag@+0x3c` (0xafee8-0xafeec). Sends only when supported + session running
    (`AirBaseController.sessionRunning`, 0xaff04).
  - Allocates a 2-byte buffer (0xaff18) and extracts the 8 bools from the packed struct (bits 0/8/16/24/
    32/40/48/56, 0xaff34-0xaff8c) into args, then calls `_HIDDPadFillReport` (0xaff98).
  - **Transmit (0xaffc4): `AirPlayReceiverSessionSendHIDReport(session=[x23+0x28], uid=x26(=dPadID from
    +0x38), buf=filled 2 bytes, len=2)`.** This is answer #3 = **path (a)**. It is the SAME API we already
    use for touch/media; the session pointer is the same session; the only per-device difference is `uid`
    (each device sends under its own uid, exactly like our uid 1/2/3). No iAP2, no `_sendReport`, no
    `enqueueHIDReport`, no separate HID transport, no separate event channel (answer #5: same channel).
  - Everything after 0xaffc8 is os_log only.

#### 2.7.2 `_HIDDPadFillReport` and exact report bytes (SDK @0x3864) — answer #6
Arg order from `handleDPadInput`: `(x0=buf, w1=up, w2=down, w3=left, w4=right, w5=select, w6=home,
w7=menu, stack0=back)`. Fill logic (SDK 0x3864-0x388c):
- `byte0 = home | (back<<1)`   → bit0=Home(AC 0x0223), bit1=Back(AC 0x0224), bits2-7 = const 0.
- `byte1 = menu | (select<<1) | (up<<2) | (down<<3) | (left<<4) | (right<<5)` → bit0=Menu(0x40),
  bit1=Select/MenuPick(0x41), bit2=Up(0x42), bit3=Down(0x43), bit4=Left(0x44), bit5=Right(0x45), bits6-7=0.

**"Down" press = `[0x00, 0x08]`.** This matches `HIDDPadCreateDescriptor` (SDK @0x26aa2c, 0x27=39 bytes,
memcpy'd from template `__const` @0x2dd6ec) verbatim:
```
05 0c 09 01 a1 01 15 00 25 01 75 01 0a 23 02 0a 24 02 95 02 81 02 95 06 81 01 19 40 29 45 95 06 81 02 95 02 81 01 c0
```
(Consumer page; Home+Back = 2 bits in byte0 + 6 pad; UsageMin 0x40 / UsageMax 0x45 = 6 bits in byte1 + 2
pad. No Report ID.) So our descriptor + 2-byte report packing is correct IF byte-identical to the above.

#### 2.7.3 Device binding — how the hidDevices entry is built — answer #4
`HIDController.airPlayHID.getter` (@0xa6530) builds the whole `hidDevices[]` array in ONE pass, calling
`addTouchScreenDevice`(@0xa6530 body), `addMediaButtonsDevice`, `addDPadDevice`(@0xb5104), etc. sequentially.
Every one of the 7 adders ends in `AirPlayInfoArrayAddHIDDevice` (SDK @0x272dac). That SDK function builds
this dict (keys resolved from `__cfstring`):
`{ uuid: hex(uid,"%X"), name: <cstr>, displayUUID: <CFString>, hidProductID: arg4, hidVendorID: arg3,
hidCountryCode: arg5, hidDescriptor: CFData(ptr,len) }`.

**The D-pad dict is built byte-for-byte the same way as touch and media** (compared 0xb579c vs touch
0xa9f10 vs media 0xb482c):
- `hidVendorID=0, hidProductID=0, hidCountryCode=0` for ALL three (`mov w3/w4/w5,#0` at each call site).
- `displayUUID` for ALL three = the literal string **`"VideoStream.Main"` / `"VideoStream.Alt1"` /
  `"VideoStream.Alt2"`** (0xb5728-0xb575c for D-pad; identical selection at 0xa9ea0 touch, 0xb47b8 media).
  It is NOT a raw UUID — it is the video-stream identifier string, and it is the same value the SIM uses as
  the display's `uuid` (see 2.7.4), so displayUUID↔display uuid must match. Our port already matches these
  (we use our display's UUID for both display.uuid and hidDevices.displayUUID), which is why touch/media work.
- Only `name` ("D-Pad" vs "Touch Screen" vs "Media Buttons"), `uid`, and `hidDescriptor` differ.

**Conclusion for #4: there is nothing special in the D-pad hidDevices entry.** No `primaryInputDevice`,
`features`, `hidLanguages`, or `buttonInfo` field lives on the HID-device dict at all — those keys live on
the DISPLAY dict / server-info (2.7.4). Our uid-3 entry is structurally correct.

#### 2.7.4 DISPLAY `features` bits — what they actually mean

Bit `0x10` is **Touchpad**; **Direction Buttons is `0x20`**, and `dPadSupport` contributes nothing to
`displays[].features` — there is no "D-pad routing gate" in this field. (An earlier disassembly-based
derivation here concluded the opposite and drove a fix for a failing uid-3 D-pad; it was refuted, and
the derivation is dropped rather than kept as provenance — ../ops/06_CORRECTIONS_LEDGER.md `R-20M-2`
has the reasoning.) The value we emit, `0x1A` = Knobs | HighFidelityTouch | Touchpad, is unchanged and
hardware-validated.

#### 2.7.5 Timing (answer #5)
No press+release/hold/repeat trickery: `handleDPadInput` sends exactly ONE 2-byte report per input event
(down report on mouse-down, up report `[0,0]` on mouse-up, per §2.6). Same event channel/session/uid model
as touch. No auto-repeat in the transmit path. Timing is not the cause.

#### 2.7.6 FIX for our failing uid-3 D-pad
1. In `receiver_core .../info.rs build_info` DISPLAY dict (line ~232) our display `features` is currently
   `0x0A` (HighFidelityTouch 0x02 | 0x08) and **lacks the D-pad bit**. Change it to **`0x1A`** (`0x0A |
   0x10`), i.e. OR in `0x10` = "Direction Buttons". This is the missing routing gate (the display
   `features` word derives from the app-authored HID/display config, pushed at init per docs/carplay/04_CAPABILITIES_AND_CONFIG.md —
   not a box constant). `primaryInputDevice`
   can stay `0` (matches SIM behaviour — it is not the D-pad binding).
2. Keep `hidDevices[uid=3].displayUUID` == that same display's `uuid` (already done).
3. Verify our 39 descriptor bytes are byte-identical to 2.7.2 and that our "Down" report is `[0x00,0x08]`
   (byte1 bit3), sent on mouse-down with `[0x00,0x00]` on release. Send API stays
   `AirPlayReceiverSessionSendHIDReport(uid=3, …)` — that was never the problem.

### Caveats
- The display `features` bit map (0x2/0x4/0x8 touch, 0x10 dpad, 0x20 …) is [E] from the SIM airPlayValues
  table @0x3ec78c. **REFUTED 2026-08-16 — see §2.7.4's own correction:** `0x10` is **Touchpad**
  (`AirPlayCommon.h:213`) and `0x20` is DirectionButtons; `dPadSupport` contributes nothing to Apple's
  word, so there is no "D-pad routing gate". ~~only "Direction Buttons"=0x10 and its role as the D-pad routing gate are load-bearing~~
  here. Whether iOS *additionally* requires a matching touch bit alongside 0x10 is [I] — set 0x10 in
  addition to the touch bits we already advertise.
- iAP2 param-ID maps in §1.2-1.4 are **wire-verified** on a stock CCPA capture, but a few enum *values*
  (PlaybackStatus, ManeuverType, CallState Status/Direction) are [I] pending per-value capture.
- The MediaButtons 7th usage (index 6 = Consumer 0x029E) is **[E] present** in the real SDK descriptor but
  its semantic is **[I]** (Apple-specific; our port omits it). **CORRECTED 2026-08-16 — this bullet
  previously said the numeric `AirPlaySiriAction` / `AirPlayZoomDirection` values "remain [I] (only string
  forms emitted)". Both are wrong now:** `AirPlaySiriAction` is **[E]** (R14G17 `AirPlayCommon.h:1366-1369`
  — 0 n/a · 1 prewarm · 2 buttondown · 3 buttonup; 4 voiceactivation is from `CarPlaySDK.framework`, not
  the 2017 header) and we emit it as an integer (`events.rs:1023`); `AirPlayZoomDirection` is likewise
  emitted as an integer (`events.rs:707`). `AirPlayAppearanceMode/Setting` numeric values **do** remain
  [I]. Telephony usage semantics (§2.5) are [I] label-mapped from the HID Telephony page.
- Descriptor bytes in §2.1 are [E] extracted from `CarPlaySDK` `__TEXT __const`; touchscreen bytes are
  runtime-built (X/Y max injected) so only their 5-/12-byte report *layout* is [E], per docs/carplay/03_SDK_GROUND_TRUTH.md §8.

---

## GM real-world reference

<!-- absorbed: ../carplay/05_METADATA_AND_CONTROLS.md -->

**Status: REFERENCE.** Answers the standing question from docs/wireless/00_WIRELESS_CARPLAY.md: is ccpa_custom's wireless metadata
gap (NowPlaying/RouteGuidance/CallState never arriving, even after fixing the plist-casing bug and the
missing `iAPChannel` SETUP gate) caused by needing **additional advertisement**, or **something else**?
Researched by mining GM's real, shipping CINEMO-based CarPlay implementation (CT5 AAOS14, unobfuscated
Java + native libs; Silverado AAOS12, existing corpus) — reference material only, not ccpa_custom code.

**Short answer: not advertisement.** GM's real implementation needs no wireless-specific advertised
capability or feature bit at all. GM's OWN design uses a genuinely separate, real iAP2-over-socket
link, distinct from the AirPlay/RTSP video-audio session (§2) — but a **follow-up pass against Apple's
own iOS27 extraction (§4, resolved 2026-07-23, corrected on a second deeper pass) found Apple's real
mechanism does NOT work that way**: no second socket exists in Apple's stack either. `iAPSendMessage`'s
payload flows, with no added framing, straight into a cross-framework call
(`APAccTransportClientEndpointForwardData` → `CoreAccessoriesLibrary`'s `acc_transportClient_
processIncomingData`) on the SAME already-connected AirPlay endpoint — architecturally much closer to
what ccpa_custom already attempts than to GM's separate-socket design. **The real gap is therefore
narrower than a missing transport**: it's the exact wire format/gating inside that one still-
undecompiled function. See §4–5 for the precise, now-scoped open question.

---

### 1. What GM does NOT do (ruling things out)

- **No Bonjour/TXT-record capability bit unlocks metadata.** GM's app layer never builds a Bonjour
  service type or TXT record itself — that's entirely inside the closed native Cinemo SDK
  (`CarPlay/Bonjour/ZeroconfType` config key, no app-visible value). No `_airplay._tcp`/TXT-record
  strings exist in any extracted native library. GM's real wireless bring-up uses the **iAP2 BT/USB-
  bootstrapped WiFi handoff** (`CinemoIAPCarPlayStartSessionWirelessAttributes` /
  `CinemoIAPAccessoryWifiConfigInfo` — SSID/passphrase/channel/security/IP list exchanged over an
  already-connected transport), matching Apple's own **legacy** pre-iOS14 Bonjour flow (WWDC 2016/2017,
  already documented in docs/carplay/02_SESSION_LIFECYCLE.md) — not a novel GM-specific advertisement mechanism.
- **No wireless-specific SETUP feature bit exists.** `WIFIAccessory.initSupportFeatures()` adds
  `CommunicationManager`/`RouteGuidance`/`LocationInfo`/`VehicleStatus`/`EAPChannel`
  **unconditionally** — no `isCarPlayEnabled()`/wireless-capability check at all (contrast
  `USBAccessory`, which DOES gate on `isCarPlayEnabled()`/`isWirelessCarPlayEnabled()`, because USB
  can carry non-CarPlay iAP2 too). `IdentityConfig.java`'s message-id table (`mSendMsgs`/`mRecvMsgs`)
  is the same static array for every transport type — **no wireless-only message-id set exists to
  advertise or negotiate.**
- **The only "wireless" gate found anywhere** is `Cinemo SKU feature CARPLAY_WIRELESS` in
  `libNmeIAP.so` — a closed-SDK **license/SKU flag**, checked internally when Cinemo builds the iAP2
  Identify message per transport component. Not app-settable, not exposed via `/info` or any
  SETUP-response field, and not applicable to an independent Rust implementation — this is Cinemo's
  own build-time licensing, not a protocol mechanism ccpa_custom is missing.
- **The metadata subscription code itself is 100% transport-agnostic.** `RouteGuidance.onStart()`,
  `CommunicationManager.startUpdating()`, and `MediaManager.startMediaAccess()` each call their
  `Start*Updates()` iAP2 messages byte-for-byte identically regardless of whether the underlying
  `Accessory` is `USBAccessory` or `WIFIAccessory`. The only difference found anywhere is a longer
  cumulative-ACK timeout for WiFi (300 vs 60, expected for a lossier transport) — nothing
  metadata-specific.

**Conclusion of this section: if ccpa_custom is looking for an extra feature to advertise or declare,
GM's real implementation proves there isn't one to find.** The gap is architectural, not a missing flag.

---

### 2. What GM DOES do — a real, separate iAP2 socket, not an AirPlay tunnel

This is the key structural finding. GM's wireless CarPlay does **not** carry iAP2 messages tunneled
inside AirPlay/RTSP commands (which is what ccpa_custom's current `iAPSendMessage`-based experiment
does). Instead:

1. Cinemo's config registers a **dedicated WiFi iAP2 transport**:
   `OPTION_IAP_TRANSPORT_LIBRARIES = ...,NmeTransport:CreateNmeIAPWifiTransport,...` alongside the USB
   one (`GMCinemoManager.java`).
2. That transport is backed by **`NmeWifiTransportClient`/`NmeWifiTransportServer`** in
   `libNmeBaseClasses.so`/`libNmeTransport.so` — confirmed **real BSD sockets**
   (`NmeSocket::Connect/Bind/Listen/Accept`, `nme_connect`/`nme_send`/`nme_recv`, `SetTCPNoDelay`), not
   a message wrapped inside another protocol's command channel.
3. The socket's endpoint is an `iap://wifi://...`-scheme URL, obtained via
   `CarPlayManager.getIAPWifiUrl()` → native `ICinemoCarPlay.GetIAPOverWiFiURL()` — **but only once an
   AirPlay/CarPlay session is already running** (native guard string: `"GetIAPWifiUrl: No session
   running"`). The endpoint is not independently discovered by the Android app — it's handed over by
   the closed native AirPlay-receiver code once the AirPlay session exists.
4. Once that socket is open and identifies with `WirelessCarPlayTransportComponent`/`SUPPORTS_CARPLAY`
   (the same iAP2 Identify concept wired uses), GM's transport-agnostic subscription code (§1) just
   works — metadata flows because a **real iAP2 protocol session** exists on that link, with its own
   Identify/message-id declaration, not because of anything advertised over AirPlay.

**This is the structural piece ccpa_custom is missing.** The current wireless-metadata experiment
(docs/wireless/00_WIRELESS_CARPLAY.md) tunnels bare `msg_payload` bytes through AirPlay's `iAPSendMessage` command — essentially
asking the AirPlay control channel to carry iAP2-shaped cargo without ever standing up a real iAP2
link (identify, message-id declaration, SYN/ACK-equivalent) on the wireless side. GM's evidence
suggests that's structurally insufficient: iOS's real NowPlaying/RouteGuidance producers may simply
never route data to a channel that never completed a genuine iAP2 Identify on its own transport
component — regardless of what's declared in the AirPlay SETUP `enabledFeatures`/`iAPChannelInfo`.


> **⚠️ THE "NO SECOND SOCKET" CONCLUSION ABOVE IS REFUTED.** Apple's type-130 SETUP request carries
> `wantsDedicatedSocket = true` and its response returns a `dataPort` we bind — so a dedicated socket
> **does** exist, negotiated inside the AirPlay session rather than as a separate GM-style iAP2 link.
> What survives is exactly that distinction. The "mandatory / `-6714`" strength of the claim is
> explicitly UNPROVEN — read the sourcing caveat before repeating it.
> [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-32-4`, `R-32-5`.

---

### 3. What's still open (not resolved by GM research alone)

- **Exactly how iOS and the accessory agree on the secondary socket's IP:port is not determined.**
  GM's Android app receives the endpoint pre-computed from closed native Cinemo/AirPlay code — the
  Java/native-string evidence available doesn't show which AirPlay/RTSP exchange field carries it
  (e.g. a SETUP response key, a follow-up RTSP request, or something else). This is the concrete next
  research question, and it points at Apple's OWN side (already-extracted iOS27 material — accessoryd/
  AirPlaySender strings, `CARPLAY_FEATURE_REFERENCE.md`, docs/carplay/02_SESSION_LIFECYCLE.md's Apple findings) rather than GM's,
  since GM's app just consumes whatever the closed SDK decided.
- **CORRECTED (2026-07-23, resolved via direct firmware extraction — see docs/carplay/05_METADATA_AND_CONTROLS.md):** this doc
  originally reported, based only on the pre-existing `gminfo_resources` documentation summary, that
  Silverado's r14 build "appears to lack this WiFi iAP2 transport entirely." A follow-up pass extracted
  Silverado's REAL native libraries directly from its firmware (never done before — the prior corpus
  only listed filenames, not symbol contents) and found **r14 has the exact same WiFi iAP2 socket
  transport as CT5's r17** (`CreateNmeIAPWifiTransport` in `libNmeTransport.so`,
  `NmeWifiTransportClient`/`Server` in `libNmeBaseClasses.so`, `GetIAPUrlWifi()` + `iap://wifi://` +
  a live log string `"This is a CarPlay session over wireless, %s"` in `libNmeCarPlay.so`) — just
  distributed across generically-named libraries rather than isolated in one obviously-named file, which
  is why the earlier filename-only inventory missed it. **There is no meaningful r14→r17 architecture
  difference here — both generations have had this capability all along.** See docs/carplay/05_METADATA_AND_CONTROLS.md for the full
  writeup, including why the pre-existing documentation corpus's summary was misleading rather than
  wrong (it correctly listed which `.so` *files* exist, but the WiFi-transport symbols live inside
  generically-named ones like `libNmeTransport.so`/`libNmeBaseClasses.so`, not a dedicated file, so a
  filename-only survey couldn't have found them).

---

### 4. What the iOS-27 disassembly pass established — and what it got wrong

Four sections of "Resolved (2026-07-23)" analysis stood here. They were premised on `iAPSendMessage`
inside `POST /command` being the inbound carrier, which the 2026-07-25 hardware work refuted (the
inbound path is the RCS DataStream, SETUP stream type 130 — see §Carrier in this file). The dead
reasoning is dropped; this is everything from it that survives, plus why the rest died.

**Stands:**

- **Apple uses no second iAP2-over-WiFi socket.** No `socket`/`connect`/`bind` call exists anywhere on
  the path that carries this traffic — architecturally closer to ccpa_custom's approach than to GM's.
- **`carEndpoint_createiAPChannelIfNeeded` / `carEndpoint_sendCommandOverRCSChannel` ARE the
  mechanism** (RCS DataStream, stream type 130), confirmed on hardware from both ends 2026-07-25. A
  second decompilation pass had called them "dead code at the call-site level"; that retraction was
  itself wrong. `iAPSendMessage` is real, but as the **outbound** carrier only.
- **The forwarding chain is pure, unmodified pass-through.**
  `acc_transportClient_processIncomingData` resolves and forwards, nothing more; the real function
  lives in `CoreAccessories.framework`, not in a separate "CoreAccessoriesLibrary" (that name is a
  local dlopen-caching wrapper symbol inside `AirPlaySender`).
- **`accessoryd`'s endpoint-UUID contract:** `_acc_manager2_copyConnectionUUIDFromEndpointUUID` splits
  on `"_"` and takes the first token, so an endpoint UUID must be `<connectionUUID>_<suffix>`;
  ownership is then checked against `clientInfo.connectionUUIDs`. Pure string parsing, not a session
  or Identify lookup.
- **Client-side registration is NOT transport-conditional.** `acc_transportClient_createConnection(type=5,…)`
  and `acc_transportClient_createEndpoint(…, transportType=4, …)` are hard-coded literals — no
  `transportType`/`isWireless`/`P2PWiFi`/`wiredLink` branch exists on that path.

**Died with the carrier premise:**

- *"The bug must be inside `accessoryd`."* It does not follow: the real inbound channel (stream-130
  SETUP) was never answered at all.
- *The silent-drop-on-missing-registration bug candidate.* Dead on capture — a registered
  `<connectionUUID>_<endpointUUID>` pair with our SYN reaching `iAP2PacketParseBuffer`.

The decompilation artifacts these sections cited were per-session scratchpad space and are gone; the
observations above are the record.
### Artifacts

> **⚠️ THE DUMPS BELOW ARE GONE (verified 2026-08-16)** — per-session scratchpad space, not preserved.
> **They are the entire primary evidence base for §5-§7**, which cannot now be re-read: treat those
> sections as a secondary record and re-derive before relying on a detail. Note `accessoryd` itself is
> NOT in the split cache. [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-32-8`.

Decompiled evidence *was* saved at (paths retained for the record only — do not follow):
`/private/tmp/claude-501/-Users-zeno/ca0b6a3c-4963-4ecc-a1b4-6ec2c380d527/scratchpad/{processIncomingData_remote,copyConnUUID2,manager2_processIncomingDataForConnectionEndpoint,manager2_block_invoke,outlined_18_33,getConnectionStruct,connection2_processIncomingDataForEndpoint_internal,endpoint2_processIncomingData,isTransportRestricted}.txt`

---

## GM Silverado R14 vs CT5 R17

<!-- absorbed: ../carplay/05_METADATA_AND_CONTROLS.md -->

**Status: REFERENCE.** Answers a direct follow-up question: given docs/carplay/05_METADATA_AND_CONTROLS.md found CT5 (AAOS14, CINEMO
r17) uses a dedicated WiFi iAP2 socket transport (`NmeIAPWifiTransport`) for wireless metadata, how does
Silverado (AAOS12, CINEMO r14) — which the pre-existing `gminfo_resources` documentation corpus
implied lacked any such transport — support wireless metadata at all? **Answer: there is no real
architecture difference. Silverado's r14 has the exact same WiFi iAP2 socket transport as CT5's r17.**
The earlier impression of a difference was an artifact of a filename-only documentation survey, not a
real generational change in GM/CINEMO's design.

**Corrects a claim in docs/carplay/05_METADATA_AND_CONTROLS.md §3**, which (based only on the pre-existing corpus) said Silverado
"appears to lack this WiFi iAP2 transport entirely" — that claim is retracted here.

---

### What was done

Unlike the original Silverado research (which relied entirely on the pre-existing `gminfo_resources`
documentation corpus, never opening a native binary), this pass extracted Silverado's **real firmware**
directly: `/Volumes/stuff/misc/research/GM_research/gm_aaos/2024_Silverado_ICE/firmware/update_packages/Y181/`.
The delivery manifest identified `86331654` as the real Android system partition (a zip containing a
plain ext2 image, already separately pre-extracted at `.../Y181/extractions/86331654`, 3.2GB) —
confirmed via `debugfs -R "ls -l /"` to be the true `/system` (full `apex`/`app`/`priv-app`/`lib64`
content; `/vendor`/`/product` inside this same image are empty stubs, real content lives in separate
partition images not needed for this question). Extracted all 26 `libNme*.so` from `/system/lib64` and
`GMConnections.apk` from `/system/priv-app/GMConnectionsSrc`, read-only via `debugfs -R rdump`.

### Findings

**r14 build confirmed directly**: `strings libNme*.so | grep NmeCarPlay/r` returns only
`.../NmeCarPlay/r14/src/*.cpp` — matches the existing corpus.

**No multi-revision JNI surface**: unlike CT5's `CARPLAY_R{11-18}_COUNT_get` getters, Silverado's r14
`libNmeSDK.so` has zero `CARPLAY_R*` symbols anywhere — r14 is a single fixed protocol generation with
no revision-negotiation API, consistent with it predating whatever multi-revision requirement r17 was
built to handle.

**The WiFi iAP2 transport is fully present — just distributed across generically-named libraries
instead of one dedicated file:**

| Library | What it has |
|---|---|
| `libNmeTransport.so` | `CreateNmeIAPWifiTransport` (source: `NmeTransport/src/platform_code/common/NmeIAPWifi/NmeIAPWifi.cpp`) |
| `libNmeBaseClasses.so` | The actual C++ socket classes: `NmeWifiTransportServer::Create/on_iap_message/ThreadProc/handle_accept/handle_data`, `NmeWifiTransportClient::Create/send/recv/handle_connect` (source: `NmeBaseClasses/src/wifi/*.cpp`) |
| `libNmeCarPlay.so` | `GetIAPUrlWifi()`, the `iap://wifi://` URL scheme (exact analogue of CT5's approach), and a **live runtime log line**: `"This is a CarPlay session over wireless, %s"` + feature-flag GUID `#F_PROJECTION_CARPLAYWIRELESS` |
| `libNmeIAP.so` | The iAP2 protocol-layer WiFi info-sharing: `RequestAccessoryWiFiConfigurationInformation`/`AccessoryWiFiConfigurationInformation` (SSID/passphrase/channel/security handoff), gated by a Cinemo SKU feature bit `CARPLAY_WIRELESS` (`Identify_Transports: Cinemo SKU feature CARPLAY_WIRELESS is missing for WiFi transport component`) |
| `libNmeSDK.so` | Full JNI bridge: `CinemoIAPWiFiInfo`, `CinemoIAPAccessoryWifiConfigInfo`, `ICinemoCarPlay_GetIAPUrlWifi`, etc. |

`libNmeAndroidTransport.so` (Android Auto's transport, genuinely separate) has **zero** wifi strings —
confirming the capability found is CarPlay-specific, not an Android Auto artifact bleeding into the
search.

**Why the documentation corpus missed this**: its library inventory correctly listed which `.so`
*files* exist, but the WiFi-transport code isn't in a separately, obviously-named file (there's no
`libNmeIAPWifiTransport.so`) — it's built into the generic-sounding `libNmeTransport.so` and
`libNmeBaseClasses.so`. A filename survey without opening the binaries could never have found this; the
corpus was misleading, not fabricated.

**`GMConnections.apk` — a correction to the extraction task's own assumption**: this app (package
`com.gm.hmi.connection`) is NOT the CarPlay/iAP2 protocol implementation — it's the connections-
settings HMI layer (Bluetooth pairing, WiFi hotspot toggle, projection device-conflict UI), a *client*
of `gm.carplay.CarPlayServiceManager`/`gm.connection.DeviceConnectionManager`, not their implementer
(unlike CT5's `GMCarPlay.apk`, which bundled the real `com.gm.server.carplay.service.internals.*`
implementation directly). Still useful: its `ProjectionRepository.java` confirms the real activation
flow —
```java
return this.mCalibrationRequest.getBooleanValue(CalItem.Apple_CarPlay_enableWireless);
```
— Bluetooth/RFCOMM detects a wireless-capable phone → GM's settings layer calls
`turnOnWifiHotspotRequired()` → the native Cinemo stack establishes both the AirPlay video/audio session
AND, via `CreateNmeIAPWifiTransport`/`NmeWifiTransportServer`, the dedicated iAP2-over-WiFi socket whose
address is handed to the phone via the `iap://wifi://` URL from `GetIAPUrlWifi()`. This confirms the
calibration flag gates a real, reachable code path, not dead/vestigial code.

### Bottom line

**There is no meaningful r14→r17 architectural evolution for wireless metadata.** Both generations have
had the same dedicated-socket design (`NmeWifiTransportServer`/`Client`, `CreateNmeIAPWifiTransport`,
`GetIAPUrlWifi`/`GetIAPOverWiFiURL`) all along — GM's own design choice (per docs/carplay/05_METADATA_AND_CONTROLS.md) is consistent
across at least two hardware/SoC generations and three model years. What differs between the two units
is policy/configuration (whether the `CARPLAY_WIRELESS` Cinemo SKU bit and `Apple_CarPlay_
enableWireless` GM calibration flag are actually set true on a given build), not SDK capability — and
this firmware read alone can't determine Y181's actual calibration state, only that the mechanism it
gates is fully implemented and reachable in the binary.

This is orthogonal to, and does not change, docs/carplay/05_METADATA_AND_CONTROLS.md's separate finding that **Apple's own real protocol
does NOT require a dedicated socket** (confirmed via `AirPlaySender` disassembly) — GM's consistent
choice to use one across generations is GM/CINEMO's own architectural preference, not something Apple's
protocol demands.

> **⚠️ THE PARAGRAPH ABOVE IS THE ONE CORRECTED CLAIM IN THIS DOCUMENT.** Apple's type-130 SETUP does
> negotiate a dedicated DataStream socket — inside the AirPlay session, not as a separate GM-style
> iAP2 link, so the *architectural* contrast survives even though "requires no socket" does not. The
> "mandatory / `-6714`" strength is unproven. [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-34-2`, `R-34-3`.

### Artifacts

Extracted (read-only) to
`/private/tmp/claude-501/-Users-zeno/ca0b6a3c-4963-4ecc-a1b4-6ec2c380d527/scratchpad/gm_silverado_extract/`
— all 26 Silverado `libNme*.so`, `GMConnections.apk`, and its jadx decompile. Copied into
`~/Downloads/Carplay WWDC/GM CarPlay Reference/` (persistent, alongside the CT5 material) —
see that folder's `README.md`.

---

## Investigation record — the 2026-07 research passes

Two dated artifacts closed here: a combined session/metadata findings report and a 12-reviewer code
audit. Both were written before the 2026-07-25 hardware work identified the real inbound carrier, so
their central framing is obsolete. What they established, and what died with them:

**Stands.**

- Two independent real-world references were mined — Apple's own stack and GM's production CINEMO
  implementation across two vehicle generations. `ccpa_custom`'s core session philosophy
  (pause-in-place, persistent pairing + ephemeral config, drain-health detection) already matched both
  independently.
- The wireless-metadata failure was **not** an advertisement problem, and Apple does **not** use a
  GM-style separate iAP2 socket.
- `iAPChannel` really is a SETUP-negotiated capability gate.
- The code audit's Part 1 bugs — found independently by multiple reviewers — are **all fixed and
  shipped**. Its line anchors are long stale; do not chase them.

**Died.**

- *"The gap is the exact wire format and identify-gating inside that existing channel, not a missing
  transport."* Backwards: the channel never existed, because the phone's stream-130 SETUP went
  unanswered. That is the whole bug, and it is fixed.
- *"No `iAPChannel` echo → iOS 400s every subsequent request"* — an unsupported causal chain.
- The audit's "core protocol-level question, sharpened but unresolved" was answered by the stream-130
  finding, not by any of the framing hypotheses it proposed.
- The proposed empirical experiment (compare wireless vs wired `enabledFeatures` to find a capability
  gate on ACC registration) was aimed at the wrong target; the tunnel's own fresh iAP2 Identify —
  filed as the lower-ranked guess — was the correct one and is implemented.

**Still open from that planning list** (tracked in `../ops/04_OPEN_ITEMS.md`, not here):
`sessionManagementInfo` / teardown-reason declaration using GM's five-value reason vocabulary; a
resource-arbitration model beyond one-shot `takeScreen`; the reconnect fast-path and wireless
bring-up model. Session-lifecycle detail lives in `02_SESSION_LIFECYCLE.md`.

The decompilation artifacts both reports cited were per-session scratchpad space and are gone.
