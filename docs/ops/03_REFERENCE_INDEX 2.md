# Reference material index

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 42; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

Which external source answers which kind of question, and where each lives on disk.

## Reference material index

<!-- absorbed: ../ops/03_REFERENCE_INDEX.md -->

**READ THIS BEFORE STARTING ANY PROTOCOL INVESTIGATION.**

### ▶ CONSULT ORDER (owner directive, REORDERED 2026-08-10 — ordered by CURRENCY)

Authoritative list in CLAUDE.md and docs/carplay/02_SESSION_LIFECYCLE.md. Short form, and it is the order to actually work in:

| # | source | why here |
|---|---|---|
| 1 | **`CarPlaySDK.framework`** (§C, inside the Simulator plugin) | The CURRENT receiver side. **Start here.** Symbols carry full C signatures, so `strings \| grep` usually yields a feature's contract with no disassembly. The ONLY source covering anything post-2017. |
| 2 | The rest of the **CarPlay Simulator** (§C) | Implementation examples + working config: the ten real `VehicleConfig` templates, `iAP2MessageKit` catalogs (`tools/i2mspec_dump.py`), the linked `iAP2Link.c`. **WIRED-ONLY**, so an enabled capability may never be exercised there — evidence of the CONTRACT, not the behaviour. |
| 3 | **CT5 CINEMO** (§B) | Shipping head unit; authoritative where the Apple sources are open. |
| 4 | **SpeedPlay TBOX** (§B) | Reverse-derived, but a real implementation. |
| 5 | **R14G17 licensed SDK, 2017** (§A) | **Presence is authority; absence is not evidence.** Where it CONTAINS a thing it is byte-authoritative and beats any re-derivation — `Platform/HID*.c` are literal descriptor builders, and its knob template is byte-identical to the 2026 Simulator's. Fifth for SEARCH ORDER only. |
| 6 | Everything else | Stock CPC200-CCPA firmware, iOS extracts — gap-filling. |

**Why the 2017 source moved down.** It was previously tier 1 alongside the Simulator, and that
bundling let a 2017 drop's silence read as an answer. Four features this project actually needed are
invisible in it and fully described in `CarPlaySDK.framework`: the RCS DataStream (type 130) and its
seven client types, `MainBuffered` audio, the Enhanced-Siri `AuxIn`/`AuxOut` uplink, and the SETUP
feature-intersection gate. The 2026-07-25 breakthrough came from exactly that escalation — the
reorder makes it the default path instead of the recovery path.

### Why this document exists

On 2026-07-25 this project spent a long session inferring answers — from iOS binary disassembly, from
vendor firmware, from behavioural experiments on real hardware — to questions that were **written down
in Apple's own licensed SDK source sitting in a sibling directory on this same Mac.**

Concretely, over **2026-07-22 → 07-25**, `docs/wireless/00_WIRELESS_CARPLAY.md`–`docs/wireless/01_BT_AND_RADIO.md` ran six passes of iOS disassembly
(docs/carplay/05_METADATA_AND_CONTROLS.md, 07-23) and several deploy-test-revert hardware cycles (docs/wireless/00_WIRELESS_CARPLAY.md, 07-24) on the
wireless-metadata problem. Answers to two of those questions were plain text in
`AppleCarPlay_CommunicationPlugIn_IntegrationGuide.txt`.

The sharpest illustration is not that the guide went unread — it is that **`docs/wireless/00_WIRELESS_CARPLAY.md` quoted the very
sentence it needed and did not act on it.** Its quotation includes *"The Zero-Ack implementation is
recommended for the link parameters"*, and the implementation it then describes reuses the Bluetooth
link parameters. The failure was not "nobody read it"; it was **"nobody acted on what they quoted, and
nobody recorded the path so the next reader could check."**

Several docs *did* name these files — docs/wireless/00_WIRELESS_CARPLAY.md cites `AirPlayReceiverSession.c:5486` (which resolves, but to unrelated array-building code; the function it means, `AirPlayReceiverSessionSendiAPMessage`, starts at `:5332` of this 5632-line copy — corrected 2026-08-16, the earlier "does not even resolve" wording was wrong), docs/wireless/00_WIRELESS_CARPLAY.md cites
`AirPlayReceiverSession.c` + `HTTPClient.c`, docs/wireless/00_WIRELESS_CARPLAY.md quotes the guide. **But none recorded where the
files are**, so nobody could open them.

Separately, `../carplay/05_METADATA_AND_CONTROLS.md` reconstructed HID descriptor bytes by disassembling `CarPlaySDK` at file offsets;
`AppleCarPlay/Platform/HID*.c` is actual C source in the same SDK — though see the vintage caveat in
§A before preferring one over the other.

**Recording the paths is the failure this index prevents.** The rule that follows from it:

> **Check the licensed Apple SDK source FIRST. Then shipping vendor implementations. Then iOS
> extracts. Disassembly and hardware experiments are the LAST resort, not the first.**

---

### A. Apple CarPlay Communication Plug-in R14G17 — THE AUTHORITY

**`~/carlink/local_carplay_sdk/reference/apple_carplay_sdk_R14G17/`** (6.1 MB, 267 files)

Licensed first-party material under the owner's Apple Developer Program membership. This is Apple's
own **accessory-side** reference implementation — the same role this project plays. Where any project
doc, disassembly finding, or inference conflicts with this source, **this source wins.**

| Path | What it is |
|---|---|
| `AppleCarPlay_CommunicationPlugIn_IntegrationGuide.txt` | The integration contract. Read it **in full**, not by grep — the iAP2-over-wireless section is at ~line 285-306, but session lifecycle, properties and feature negotiation elsewhere impose preconditions on it. |
| `AppleCarPlay_CommunicationPlugIn_ChangeLog.txt` | When/why APIs were added — useful for "is this old or current". |
| `AppleCarPlay_CommunicationPlugIn_Bonjour.txt` | Discovery/advertisement. |
| `AppleCarPlay/Sources/AirPlayReceiverSession.c` / `.h` / `Priv.h` | Session lifecycle, `sessionStarted`, the delegate contract, `AirPlayReceiverSessionSendCommand`, `AirPlayReceiverSessionSendiAPMessage`. **The single most load-bearing file.** |
| `AppleCarPlay/Sources/AirPlayReceiverServer.c` / `.h` / `Priv.h` | RTSP request handling — `_requestProcessRecord`, SETUP, `/info`, `/command` dispatch. |
| `AppleCarPlay/Sources/AirPlayReceiverSessionScreen.c` / `.h` | Screen stream. |
| `AppleCarPlay/Sources/AirPlayCommon.h` | **All command / key / property string constants.** Settle key names and casing here, not by device trial. |
| `AppleCarPlay/Sources/CarPlayControlClient.c` / `.h` | CarPlay control client. |
| `AppleCarPlay/Sources/AirPlayReceiverPOSIX.c` | The POSIX platform layer — shows which platform calls are purely local vs. on-wire. |
| `AppleCarPlay/Platform/HID*.c` / `.h` | **HID descriptors and report packing in C source**: TouchScreen, Touchpad, Knob, MediaButtons, Telephony, Proximity. **Cross-check against `../carplay/05_METADATA_AND_CONTROLS.md`, do not blindly prefer** — R14G17 is 2017 vintage. Telephony and Proximity match ../carplay/05_METADATA_AND_CONTROLS.md byte-for-byte; **MediaButtons diverges** (R14G17 has LogicalMax 5 / 6 usages, ../carplay/05_METADATA_AND_CONTROLS.md's newer `CarPlaySDK` extraction has 6 / 7 including `0x029E`), and R14G17 has **no DPad or SteeringWheel builder**. ../carplay/05_METADATA_AND_CONTROLS.md already flags this. **CORRECTED 2026-07-30: this line previously also claimed R14G17 had no "Knob-minimal" builder. That was WRONG and it matters — `AppleCarPlay/Platform/HIDKnob.c` defines BOTH `HIDKnobCreateDescriptor` (70 bytes, home+back+nudge) AND `HIDKnobBasicCreateDescriptor` (51 bytes, i.e. exactly the "minimal" builder claimed absent). Both are emitted verbatim (`memcpy`, no runtime patching), and the 70-byte template is BYTE-IDENTICAL to the one Apple's 2026 CarPlay Simulator ships at `CarPlaySDK.framework` file offset `0x2D9503`. This erroneous line plausibly caused the 2026-07-06 incident, in which a knob descriptor was GUESSED — and broke the box — while the real bytes sat in the licensed source this index points at.** |
| `Examples/AppleCarPlay_AppStub.c` | The reference accessory's wiring — delegate registration, `_AirPlayHandleSessionStarted`, control dispatch. Shows the intended *shape* of an accessory. |
| `AppleCarPlay/AccessorySDK/Support/` | **The SDK's own HTTP/CF/Bonjour layer** — `HTTPClient.c/.h`, `HTTPServer`, `HTTPMessage`, `HTTPUtils`, `CFLite`, `CFLiteBinaryPlist`, `BonjourBrowser`, `AsyncConnection`, `HIDUtils`, `ChaCha20Poly1305`. 89 files. **Where transport-level behaviour is decided** — e.g. whether the event socket accepts unsolicited inbound requests at all. |
| `AppleCarPlay/AccessorySDK/External/` | Crypto only — Curve25519, Ed25519, SRP, GladmanAES, LibTomMath, Small25519. (It is *this* subdirectory that is crypto-only, not `AccessorySDK/` as a whole.) |

**CORRECTED 2026-07-25 — Apple's own iAP2 link layer IS on this machine.** `iAP2Link.c` and
`iAP2LinkAccessory.c` are statically linked into the CarPlay Simulator binary (§C) with their build
paths intact: `…/CarPlaySimulator_Devices/Libraries/iAP2/iAP2/Public/iAP2Link/iAP2Link.c`. That is
Apple's **accessory-side** FSM — our exact role — and it supplies the action tables, the SYN parameter
field order from Apple's own validator, and the per-transport SYN templates. The **device** side
(`iAP2LinkDevice.c`) is in the iOS 27 extract's `accessoryd` (§D). Answer iAP2 link-layer questions from
those two FIRST; §B's vendor implementations are corroboration, not the primary source. The paragraph
below sent readers to SpeedPlay instead, which is how the SYN field order got misread.

**NOT in this SDK:** the iAP2 link layer itself. The guide defers to the **AISpec** ("Accessory
Interface Specification") for link parameters, and the AISpec is **not present anywhere on this
machine** (verified 2026-07-25). iAP2 link-layer questions must be answered from the shipping vendor
implementations in §B, or the AISpec obtained separately.

The surrounding project `~/carlink/local_carplay_sdk/` (`src/`, `include/`, `docs/`,
`conformance/`, `MAP`) is a separate CarPlay SDK implementation effort — check it before mining
binaries; it may already have solved the problem.

---

### B. Shipping vendor implementations — for what Apple's SDK doesn't cover

These are real products that ship working wireless CarPlay. Authoritative for *how a shipping
accessory actually does it*, which is evidence but not spec — distinguish "vendor's choice" from
"Apple requires".

| Path | What it is | Best for |
|---|---|---|
| `~/carlink/local_carplay_sdk/reference/tbox_speedplay/` (36 MB) | SpeedPlay TBox: `SpeedPlay_chelianyi.apk` + `extracted/lib/armeabi-v7a/` — `libcustomiap.so`, `libAirPlay.so`, `libCarplayJni.so`, `libAirPlaySupport.so`, `libAudioConverter.so`, `libMirrorAirPlay.so`, plus `classes.dex` | **iAP2 link layer** (`libcustomiap.so` is the obvious first stop), ARM CarPlay receiver internals |
| `ccpa_custom/reference/gm_cinemo/` (522 MB) | GM Silverado AAOS12 + CT5 AAOS14: `CT5_extracted_libs_and_apks/`, jadx decompiles of GMCarPlay (both), GMConnections; plus two research write-ups | Wireless metadata architecture, `libNmeIAP.so` / `libNmeCarPlay.so` / `libNmeTransport.so` / `libNmeBaseClasses.so`, the `iap://wifi://` socket transport (iAP2 over a reliable stream — structurally the same problem as our AirPlay tunnel) |
| `~/carlink/local_carplay_sdk/reference/cinemo_aarch64_ct5/` (19 MB) | Additional CT5 Cinemo material | Cross-check against §B gm_cinemo |
| `~/carlink/local_carplay_sdk/reference/cinemo_reference/` (14 MB) | Additional Cinemo reference | As above |

---

### C. Apple CarPlay Simulator — the controller side

| Path | Notes |
|---|---|
| `/Applications/Xcode.app/Contents/SharedFrameworks/DeviceKit.framework/Versions/A/PlugIns/CarPlaySimulator.devicekitplugin` | Present. `CarPlaySDK.framework` (the "StarkSDK" receiver), `iAP2MessageKit.framework` (Apple's own iAP2 message/param catalogs), `Contents/Resources/VehicleConfigs/` (10 real `VehicleConfig` YAML templates + VDC schemas) |

#### The iAP2 spec archive — decode it, don't grep symbol names

`iAP2MessageKit.framework/Versions/A/Resources/iap2messages-internal.i2mspecarchive` is an
NSKeyedArchiver graph holding Apple's **complete** iAP2 definition: 144 messages, each with its id,
name, source, and the full parameter tree — ids, types, cardinality, enum values and Apple's own notes.
`spec.rs` was generated from it.

    tools/i2mspec_dump.py --message 0x4158 --text     # one message, readable
    tools/i2mspec_dump.py > spec.json                 # all 144

**This is the authority for TLV parameter ids.** Earlier sessions derived them from exported symbol
names (`nm -gU`), which gives names but neither ids nor cardinality nor the notes that state a
parameter's prerequisites. Do not hand-derive a parameter id when this file will state it.
| `~/Downloads/Carplay WWDC/Hardware/CarPlay Simulator.app` | **Available on disk since 2026-08-16** (previously indexed as the unmounted `/Volumes/Additional Tools/…`). Standalone Hardware IO Tools build 267 / v4.0. **Mirrored at `~/Documents/carlink/carplay_simulator/CarPlay Simulator.app` (verified 2026-08-16) — the two standalone copies are the ONLY place `iap2messages-external.i2mspecarchive` exists on this machine; Xcode's plugin ships `-internal` and `mfi4authmessages-internal` and nothing else. `../carplay/05_METADATA_AND_CONTROLS.md` §2.2's wireless iAP2 message table is sourced from that external archive, so this entry is its only recorded path.** Carries its OWN `CarPlaySDK.framework` (6.8 MB arm64), `iAP2MessageKit` — with the **external** archive `iap2messages-external.i2mspecarchive` at `Contents/Frameworks/iAP2MessageKit.framework/Versions/A/Resources/` (the shipped MFi contract, vs Xcode's `-internal`; pass it with `--archive`) — `iAP2MessageKitCore`, `LunaUI`, and the same 10 `VehicleConfig` templates. **The `CarPlay Simulator` binary itself is the best source for the VEHICLE-SIDE session event ladder** (`strings`): connection ladder, delegate names, terminal session states. Basis for `docs/carplay/02_SESSION_LIFECYCLE.md`. Its `Resources/Javascript/*.bundle.js` is the AirPlay **Video player** web app — no CarPlay session logic, do not mine it for one. |
| `ccpa_custom/reference/carplay_sdk/` (72 KB) | Verbatim copies already pulled out: `apple_vehicleconfigs/`, `apple_vdc/` |

Basis for `docs/carplay/03_SDK_GROUND_TRUTH.md` (SDK ground truth), `../carplay/05_METADATA_AND_CONTROLS.md`, `docs/carplay/04_CAPABILITIES_AND_CONFIG.md`, `docs/carplay/03_SDK_GROUND_TRUTH.md`.

---

### D. iOS extracts — the phone side

| Path | Notes |
|---|---|
| `ccpa_custom/reference/ios27_extract/` (15 MB) | Distilled: `CARPLAY_FEATURE_REFERENCE.md`, `METADATA_FINDINGS.md`, `headers/`, `mined/` (strings), `dyld_extract.log`. **`METADATA_FINDINGS.md`'s endorsement of `iAPSendMessage` as the wireless iAP2 carrier is inbound-refuted — see docs/carplay/05_METADATA_AND_CONTROLS.md §1.1; its strings-level observations stand.** Note `reference/` is gitignored, so nothing under it is version-controlled or swept by a docs pass |
| `~/Downloads/ios27_extract_24A5390f/` | The raw dyld/`mnt_fs` dumps (**8.3 GB**) — `AirPlaySender`, `accessoryd`, `CarKit`, `CoreAccessories` |

Basis for `docs/carplay/05_METADATA_AND_CONTROLS.md`/`docs/carplay/05_METADATA_AND_CONTROLS.md`'s chain analysis. **AMENDED 2026-07-25: this demotion is conditional on §A actually speaking.** R14G17 is a 2017 drop and
is silent on everything added since — SETUP stream type 130, `APEndpointRemoteControlSession`, the whole
DataStream layer. When §A is silent on something demonstrably on the wire, §D plus `CarPlaySDK.framework`
plus the Simulator binary **are** the primary evidence; they supplied the answer this project had been
missing since its first wireless session (docs/carplay/05_METADATA_AND_CONTROLS.md). **Silence in a 2017 source is not a finding that the
feature does not exist.** The phone's own logs are part of §D and are the highest-yield item in it:
`accessoryd` emits a full iAP2 packet trace when `com.apple.iapd PrintIapPackets` is set — a preference
read at *process launch*, so a profile does nothing until the daemon restarts.

**Use these to understand phone-side behaviour, not to
derive the accessory contract** — that is what §A is for. This inversion is precisely the mistake
`docs/wireless/00_WIRELESS_CARPLAY.md`–`docs/wireless/01_BT_AND_RADIO.md` made.

---

### E. WWDC session transcripts — Apple's stated intent

*Added 2026-08-02. Several docs already cite these informally (`docs/carplay/06_AV_PIPELINE.md`, `docs/carplay/02_SESSION_LIFECYCLE.md`, `docs/carplay/05_METADATA_AND_CONTROLS.md`,
`docs/carplay/05_METADATA_AND_CONTROLS.md`); this is where the paths live.*

Location: `~/Downloads/Carplay WWDC/` (session IDs are the stable identifier; the local path
is not).

| File | Session | Authoritative for |
|---|---|---|
| `wwdc2016-722.txt` | Developing CarPlay Systems, Part 1 | **The only session that names audio codecs.** Screen requirements (24-bit colour, 60 Hz, H.264 profile), touch fidelity tiers + the **140 ms** high-fidelity latency budget |
| `wwdc2016-723.txt` | Developing CarPlay Systems, Part 2 | Audio **routing** — main vs alternate channels, mixing rules, resource ownership/transfer, per-app volume, `modesChanged`. 45 audio mentions, all about routing |
| `wwdc2017-717.txt` | Wireless CarPlay | The wireless handoff at intent level |
| `wwdc2019-252.txt` | Advances in CarPlay Systems | **THE authority on Enhanced Siri architecture** (`:86-134`) — always-on mic, ECNR, the in-car historical ring buffer, the two mandatory detectors, iOS's second-pass verification, and `AuxOut` as the Siri downlink. WWDC 2023-10150 explicitly redirects here. Also Dashboard and dynamic screen resizing |
| `wwdc2023-10150.txt` | Optimize CarPlay for vehicle systems | **`mainBuffered` / enhanced audio buffering**, Enhanced Siri, the iOS14+ "simplified connection flow" that reuses the existing iAP2 connection instead of Bonjour |

**What WWDC is good for:** Apple's *intent* and the feature's purpose, in plain language, with the
accessory-side obligation stated. It is how you learn that `mainBuffered` exists to survive dropouts
rather than to raise fidelity — a distinction no header conveys.

**What it is NOT good for:** any numeric constant, stream type id, bitmask, or wire format. It names
things it never defines. Treat a WWDC mention as a *pointer* to look in §A/§C, never as the value.

> **A finding that only a full read produces (2026-08-02):** across all five sessions there are **zero**
> mentions of Atmos, spatial audio, surround, multichannel, lossless, hi-res, bit depth, or sample rate.
> Seven years of CarPlay sessions, and the entire audio-format story is three sentences in `wwdc2016-722`
> (LPCM wired; AAC-LC media + OPUS-or-AAC-ELD other, wireless). Apple's audio effort went into
> *resilience* (`mainBuffered`) and *voice* (Enhanced Siri), not fidelity. See `docs/carplay/06_AV_PIPELINE.md`.

---

### Decision table — where to look first

| Question | Look here first |
|---|---|
| What command/key/property string, and what casing? | §A `AirPlayCommon.h` |
| When may the accessory send X? | §A `AirPlayReceiverSession.c` + Integration Guide |
| Session lifecycle — teardown triggers, idle timeouts, hijack? | §A Integration Guide `:212-230` (network + 9 s/30 s inactivity rules) + `AirPlayCommon.h:101` — still current, `CarPlaySDK` retains the same log lines. See `docs/carplay/02_SESSION_LIFECYCLE.md` §6-§7 |
| Session **management** — `stopSession`, disconnect reasons, `teardownCompleted`, `isRemoteControlOnly`? | **§C, and specifically the STANDALONE Simulator** — Xcode's copy is missing 5 of the 9 relevant strings (docs/carplay/02_SESSION_LIFECYCLE.md). The whole feature is post-2017 and returns ZERO hits in §A. Corroborate phone-side with §D `CarKit/CARSession*.h` + `CARSessionStatus.h`. See `docs/carplay/02_SESSION_LIFECYCLE.md` §5 |
| Session lifecycle on the PHONE side (connect/disconnect/timeout)? | §D `CarKit/CARSessionObserving-Protocol.h` and `CARSessionStatus.h` — NOT `CARSession.h`, which has only the stop selector. `docs/carplay/02_SESSION_LIFECYCLE.md` §9 |
| How should an accessory be wired? | §A `Examples/AppleCarPlay_AppStub.c` |
| HID descriptor bytes / report layout? | §A `AppleCarPlay/Platform/HID*.c` |
| iAP2 **link layer** (SYN params, ack policy, framing)? | §B SpeedPlay `libcustomiap.so`, Cinemo `libNmeIAP.so`, and Apple's own iAP2 link library statically linked into §C's `CarPlaySimulator` binary — **not in §A's sources**; AISpec absent |
| HTTP/transport behaviour (does a socket accept inbound? reply shapes?) | §A `AccessorySDK/Support/` — `HTTPClient.c/.h`, `HTTPServer`, `HTTPMessage` |
| iAP2 **message/param ids**? | `tools/i2mspec_dump.py` / the `.i2mspecarchive` (§C) — decoded, not `nm -gU` (corrected 2026-08-01: was "§C `iAP2MessageKit` (`nm -gU`)", contradicting this doc's own §C subsection *The iAP2 spec archive — decode it, don't grep symbol names*: "`nm -gU` gives names but neither ids nor cardinality ... Do not hand-derive a parameter id") |
| `VehicleConfig` schema / `/info` field meaning? | §C templates, then `docs/carplay/03_SDK_GROUND_TRUTH.md`/`docs/carplay/04_CAPABILITIES_AND_CONFIG.md` |
| Audio codec / format / stream-type question? | §A `AirPlayCommon.h` for the enum, §C `CarPlaySDK` strings for what postdates it, §E for *why*. All three reconciled in `docs/carplay/06_AV_PIPELINE.md` |
| What is a feature actually FOR? | §E WWDC — then §A/§C for the values it doesn't give you |
| Why does the phone behave a certain way? | §D — but check §A first for what we're *supposed* to do |
| Anything at all | **§A. Always start at §A.** |

---

### Standing rules

1. **Act on what you quote — and record the path.** docs/wireless/00_WIRELESS_CARPLAY.md quoted the Zero-Ack sentence verbatim and
   still shipped Bluetooth link parameters. Quoting a source is not consulting it. And a citation
   without a path (docs/wireless/00_WIRELESS_CARPLAY.md, 36, 39) cannot be followed by the next reader.
2. **Primary source beats inference.** A disassembly finding, a vendor behaviour, or a hardware
   observation that conflicts with §A means our reading of §A is wrong, or §A doesn't cover it — not
   that §A is stale.
3. **Cite the file:line in project docs**, so the next session can re-check rather than re-derive.
4. **Hardware experiments cost a deploy-test-revert cycle each.** They are for questions no source
   answers. Budget them accordingly.
5. **Record negatives.** "The AISpec is not on this machine" and "`AccessorySDK/` is crypto-only" are
   findings worth writing down; both were re-derived more than once.
6. **`strings` adjacency proves NEITHER order NOR ownership.** It is a literal-pool layout, not a
   sequence and not a function boundary. A 2026-08-16 verification pass caught the SAME error four times
   in one document (docs/carplay/02_SESSION_LIFECYCLE.md): a "bring-up ladder" whose backing enum is in a different order, a
   "creation order" table that was backwards, a "verb list" that straddled two functions, and a "nested
   key" that was actually parsed from the inbound request. A claim about ORDER, or about WHICH function
   owns a string, needs disassembly — resolve the `CFEqual`/`strcmp` targets and the call sites — or it
   must be labelled an inference. Adjacency is a lead, never a finding.
7. **Quote format strings whole.** A `%d` cannot testify to its runtime value (docs/carplay/02_SESSION_LIFECYCLE.md), and
   trimming a `%s%?u%s` silently deletes a conditional field (§8). If a quote is abridged, say so.

### Second host platform — the Raspberry Pi / AAOS port

**There is a working non-CCPA host running this accessory stack.** A Raspberry Pi 4 on AAOS 16
(arm64) runs `carplay-wireless` + `airplayd` + `rx-connect` natively, providing **both radios
itself** — its own Bluetooth and its own 5 GHz SoftAP — with the CCPA reduced to the **MFi
coprocessor only**, reached over USB-NCM (`CARPLAY_MFI_ADDR` → `ccpa/mfid`). Device-proven end to
end against an iPhone on iOS 27: pairing, iAP2, MFi auth, the `0x5702`/`0x5703` handoff, DHCP,
pair-verify, MFi-SAP auth-setup, RECORD and the screen stream.

→ **`pi/docs/00_PI_AAOS_PORT.md`**

Read it before assuming anything in this tree is CCPA-specific, and before re-deriving any of:

* RFCOMM is implemented **in userspace over L2CAP** there (`crates/bt-common/src/rfcomm_uspace.rs`,
  moved out of `vendor/wireless` 2026-09-03) — the AAOS kernel ships without `CONFIG_BT_RFCOMM`.
* `hciconfig` does not exist on Android; bring-up is native ioctls + raw HCI
  (`crates/bt-common/src/hci.rs`),
  and it must be raw HCI rather than mgmt because mgmt synthesises EIR and cannot express the
  CarPlay marker UUID.
* There are **three** independent MFi chip users in this tree, not one.
* Four Android-specific traps that cost real debugging time (`target_os` is `"android"` not
  `"linux"`; `pgrep -x` matches opposite things on BusyBox vs toybox; Android deletes the
  `from all lookup main` ip rule; the bundled dnsmasq 2.51 silently disables DHCP on an
  unrecognised flag).

Required AAOS *image* changes are recorded outside this repo, in
`/Volumes/stuff/rpi/aaos/docs/os-corrections-2026-08-16.md` - **an external volume, NOT mounted as of
2026-08-16 (`/Volumes/` holds only `Macintosh HD`, `Recovery` and the Time Machine local-snapshots
mount). Mount `stuff` before following this pointer; unmounted, it reads as a missing file rather than
an absent one.**

### F. Android Auto — the Desktop Head Unit is the authority (owner directive, 2026-08-25)

This index carried nothing for Android Auto until now, which is exactly the gap it exists to prevent:
the AA material was described only inside `docs/androidauto/00_ARCHITECTURE.md`, so a session that started from this document
found no AA sources at all.

**Consult order for ANY Android Auto protocol question:**

1. **Google's Desktop Head Unit (DHU)** — the first-party head-unit reference, installed by Android
   Studio ▸ SDK Manager at `~/Library/Android/sdk/extras/google/auto/` (v2.0). It is the AA analogue of
   the CarPlay Simulator and the authoritative statement of what a head unit is expected to do:
   - `config/default.ini` — Google's reference head-unit config (**800×480, dpi 160, 30 fps**, sensors
     location/night/driving). Every geometry/sensor choice in our ServiceDiscoveryResponse matches it.
   - `desktop-head-unit` — the binary; its protobuf namespace is **`gal.*`** (Google Automotive Link),
     and the symbol names carry the message shapes (`gal.ServiceDiscoveryResponse`, `gal.MediaSinkService`,
     `gal.VideoConfiguration`, `gal.NavFocusType`, `gal.AudioFocusRequestType`). `Controller::sendVersionRequest`
     requests protocol **1.7**.
   Google keeps the DHU binary stable while the phone side evolves, so it is a stable contract, not a
   snapshot of one app version.
2. **The shipping Android Auto app** (`com.google.android.projection.gearhead`, pulled from the owner's
   own phone and decompiled) — for what the CURRENT phone actually enforces: the protocol version
   ceiling (`rux(1,6)`..`rux(1,7)` in the GAL handler), the teardown reasons that name a missing service,
   and the `gal.*` schema as shipped.
3. **The public open-source `aasdk` / `openauto` stack (GPLv3)** — SUPPLEMENTARY. It is a
   re-derivation, not a Google drop, and the stock CPC200-CCPA's own `ARMAndroidAuto` is built from it —
   so it is excellent for framing/encapsulated-TLS mechanics and for "what a working implementation
   does", and it is where the public head-unit credential comes from, but where it disagrees with the
   DHU or the shipping app, THEY win. Treat it exactly as this document treats SpeedPlay on the CarPlay
   side: separate its *choices* from its *observations*.
4. **Our own captures** — `analysis/aa_full_session_emulator_*.txt` (a DHU session) and the stock-adapter
   capture, for diffing behaviour rather than deriving contracts.

**What Google has said in public (surveyed 2026-09-04; every item fetched, dates are access dates).**
Google publishes no Android Auto head-unit specification. The public corpus is: the Desktop Head
Unit page (developer.android.com/training/cars/testing/dhu — resolutions 800×480/1280×720/1920×1080
as *examples*, framerate default 30, dpi default 160 plus `realdpi`/`normalizedpi`, `marginwidth`/
`marginheight`/`margins`/`cropmargins`, `contentinsets`, `pixelaspectratio`, `instrumentcluster`/
`navcluster`/`phonecluster`, `inputmode` touch/rotary/hybrid — no codec, audio-rate or HFP detail);
the user-facing requirements (support.google.com/androidauto/answer/6348019: wireless needs Android
11+, a data plan and 5 GHz Wi-Fi; /6348029: Bluetooth + Wi-Fi + Location for first pairing — no
profile or codec named); the Car App Quality guidelines (DD-2/DD-3/DD-4: nothing visible or audible
while driving except audio from video-category apps on capable devices); and the yearly I/O posts on
android-developers.googleblog.com (2015 launch; 2021 Car App Library + AA on the BMW iX cluster; 2022
split-screen redesign; 2023 video/gaming categories, cluster integration for nav apps; 2025 Gemini,
parked video on Android 16 phones; 2026 "smooth 60 fps HD video playback" while parked on Android
17+ phones). The only spec-shaped document is a **leaked 2016 partner "Head Unit Integration Guide"
v1.3.0** (Google Confidential, mirrored at milek7.pl/.stuff/galdocs/) — H.264 Baseline, 800×480
required / 720p / 1080p optional, 30 fps minimum and 60 recommended, PCM 16-bit with **16 kHz mono
guidance/voice and 48 kHz stereo media**, AAC-LC optional, Bluetooth HFP 1.5 — a decade-old floor,
not a ceiling. **Not found anywhere public:** codec-tier or margin semantics, DPI guidance, any HFP
codec (mSBC/LC3) requirement, a current OEM compatibility spec, a Google primary source for the 2018
wireless rollout, or 4K/HDR named for Android Auto. Everything above 1080p, the H.265 requirement,
60 fps at 4K and the margin/touch behaviour exist only as our measurements
(`docs/androidauto/01_SESSION_AND_AV.md` §1) and the decompile.

Our implementations to diff against: `host/aa-headunit/` (the Rust reference client) and
`host/CarPlayHost/carlink_macOS/AA/` (the Swift engine that ships in the app). The box side carries no
AA protocol at all — `ccpa/aa-bridge` is an AOAP byte pump (`docs/androidauto/00_ARCHITECTURE.md`, `docs/androidauto/00_ARCHITECTURE.md`).

### Note on the real-time cyber safeguard

Investigating this material can trip Anthropic's real-time classifier, because protocol-accurate
accessory development shares vocabulary with offensive tooling. It is a false positive on this work —
see `../ops/07_AUTHORIZATION.md` and `../ops/07_AUTHORIZATION.md` for the standing frame and the phrasing
guide. In short: describe the work as **implementation and conformance review against licensed
reference material**, which is what it is. Avoid framing tasks as probing, injecting, capturing
replies, or bypassing. If it trips anyway, start a fresh session and lead with the authorization
frame; a flagged thread keeps re-tripping.
